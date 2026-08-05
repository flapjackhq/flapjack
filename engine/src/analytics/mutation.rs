use super::config::AnalyticsConfig;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};

static INDEX_OPERATION_STATES: Lazy<Mutex<HashMap<PathBuf, Weak<IndexOperationState>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Default)]
struct IndexOperationCounts {
    active_readers: usize,
    writer_active: bool,
    /// Mutations that have queued but not yet acquired. New readers stop being
    /// admitted while this is non-zero so a steady read stream cannot starve a
    /// waiting seed or clear (writer preference).
    waiting_writers: usize,
}

struct IndexOperationState {
    counts: Mutex<IndexOperationCounts>,
    changed: Condvar,
}

impl IndexOperationState {
    fn new() -> Self {
        Self {
            counts: Mutex::new(IndexOperationCounts::default()),
            changed: Condvar::new(),
        }
    }
}

pub(super) struct IndexReadGuard {
    state: Arc<IndexOperationState>,
}

struct IndexMutationGuard {
    state: Arc<IndexOperationState>,
}

/// Serialize filesystem mutations per analytics index. Different indices remain
/// independent, while seed and clear cannot remove or replace each other's files.
pub(super) fn with_index_mutation<T>(
    config: &AnalyticsConfig,
    index_name: &str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let index_root = config.target_artifact_paths(index_name).index_root;
    let state = operation_state(index_root)?;
    let _guard = begin_index_mutation(state)?;
    operation()
}

pub(super) async fn begin_index_read(
    config: &AnalyticsConfig,
    index_name: &str,
) -> Result<IndexReadGuard, String> {
    let config = config.clone();
    let index_name = index_name.to_string();
    tokio::task::spawn_blocking(move || begin_index_read_blocking(&config, &index_name))
        .await
        .map_err(|error| format!("analytics read admission task failed: {error}"))?
}

fn begin_index_read_blocking(
    config: &AnalyticsConfig,
    index_name: &str,
) -> Result<IndexReadGuard, String> {
    let index_root = config.target_artifact_paths(index_name).index_root;
    let state = operation_state(index_root)?;
    let mut counts = state
        .counts
        .lock()
        .map_err(|error| format!("analytics operation state poisoned: {error}"))?;
    while counts.writer_active || counts.waiting_writers > 0 {
        counts = state
            .changed
            .wait(counts)
            .map_err(|error| format!("analytics operation state poisoned: {error}"))?;
    }
    counts.active_readers += 1;
    drop(counts);
    Ok(IndexReadGuard { state })
}

pub(super) fn clear_index(config: &AnalyticsConfig, index_name: &str) -> Result<u64, String> {
    with_index_mutation(config, index_name, || {
        let searches_removed = remove_directory_children(&config.searches_dir(index_name))?;
        let events_removed = remove_directory_children(&config.events_dir(index_name))?;
        Ok(searches_removed + events_removed)
    })
}

fn operation_state(index_root: PathBuf) -> Result<Arc<IndexOperationState>, String> {
    let mut states = INDEX_OPERATION_STATES
        .lock()
        .map_err(|error| format!("analytics operation registry poisoned: {error}"))?;
    states.retain(|_, state| state.strong_count() > 0);
    if let Some(state) = states.get(&index_root).and_then(Weak::upgrade) {
        return Ok(state);
    }

    let state = Arc::new(IndexOperationState::new());
    states.insert(index_root, Arc::downgrade(&state));
    Ok(state)
}

fn begin_index_mutation(state: Arc<IndexOperationState>) -> Result<IndexMutationGuard, String> {
    let mut counts = state
        .counts
        .lock()
        .map_err(|error| format!("analytics operation state poisoned: {error}"))?;
    // Register as waiting before blocking so newly arriving readers queue behind
    // this mutation instead of continually refreshing `active_readers`.
    counts.waiting_writers += 1;
    while counts.writer_active || counts.active_readers > 0 {
        counts = match state.changed.wait(counts) {
            Ok(counts) => counts,
            Err(error) => {
                let message = format!("analytics operation state poisoned: {error}");
                let mut counts = error.into_inner();
                counts.waiting_writers = counts.waiting_writers.saturating_sub(1);
                state.changed.notify_all();
                return Err(message);
            }
        };
    }
    counts.waiting_writers -= 1;
    counts.writer_active = true;
    drop(counts);
    Ok(IndexMutationGuard { state })
}

fn remove_directory_children(directory: &Path) -> Result<u64, String> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("failed to read {}: {error}", directory.display())),
    };

    let mut removed = 0;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("failed to read entry in {}: {error}", directory.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
        let result = if file_type.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        result.map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

impl Drop for IndexReadGuard {
    fn drop(&mut self) {
        let Ok(mut counts) = self.state.counts.lock() else {
            return;
        };
        counts.active_readers = counts.active_readers.saturating_sub(1);
        if counts.active_readers == 0 {
            self.state.changed.notify_all();
        }
    }
}

impl Drop for IndexMutationGuard {
    fn drop(&mut self) {
        let Ok(mut counts) = self.state.counts.lock() else {
            return;
        };
        counts.writer_active = false;
        self.state.changed.notify_all();
    }
}

#[cfg(test)]
pub(super) fn waiting_writers(config: &AnalyticsConfig, index_name: &str) -> usize {
    let index_root = config.target_artifact_paths(index_name).index_root;
    let state = operation_state(index_root).unwrap();
    let counts = state.counts.lock().unwrap();
    counts.waiting_writers
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    fn test_config(temp_dir: &TempDir) -> AnalyticsConfig {
        AnalyticsConfig {
            enabled: true,
            data_dir: temp_dir.path().to_path_buf(),
            flush_interval_secs: 60,
            flush_size: 10_000,
            retention_days: 90,
        }
    }

    fn wait_until_waiting_writers(config: &AnalyticsConfig, index_name: &str, target: usize) {
        for _ in 0..2000 {
            if waiting_writers(config, index_name) == target {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("timed out waiting for {target} queued writer(s)");
    }

    /// Coordinator admission must park outside Tokio workers. On a saturated
    /// single-worker runtime, an active reader followed by a queued mutation and
    /// a later reader must all make progress once the active reader is released.
    #[tokio::test(flavor = "current_thread")]
    async fn queued_mutation_does_not_block_the_only_tokio_worker() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config(&temp_dir);
        let index_name = "async_admission";
        let first_reader = begin_index_read(&config, index_name).await.unwrap();

        let writer_config = config.clone();
        let writer = tokio::task::spawn_blocking(move || {
            with_index_mutation(&writer_config, index_name, || Ok("writer"))
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while waiting_writers(&config, index_name) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("mutation must queue without blocking the runtime worker");

        let reader_config = config.clone();
        let later_reader = tokio::spawn(async move {
            let _guard = begin_index_read(&reader_config, index_name).await?;
            Ok::<_, String>("reader")
        });

        drop(first_reader);
        let (writer_result, reader_result) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(writer, later_reader)
        })
        .await
        .expect("queued mutation and later reader must not deadlock");
        assert_eq!(writer_result.unwrap().unwrap(), "writer");
        assert_eq!(reader_result.unwrap().unwrap(), "reader");
    }

    #[test]
    fn failed_writer_wait_does_not_leave_readers_permanently_blocked() {
        let state = Arc::new(IndexOperationState::new());
        state.counts.lock().unwrap().active_readers = 1;

        let writer_state = Arc::clone(&state);
        let writer = std::thread::spawn(move || begin_index_mutation(writer_state));
        for _ in 0..2_000 {
            if state.counts.lock().unwrap().waiting_writers == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        let poison_state = Arc::clone(&state);
        let _ = std::thread::spawn(move || {
            let mut counts = poison_state.counts.lock().unwrap();
            counts.active_readers = 0;
            poison_state.changed.notify_all();
            panic!("poison coordinator while writer admission is waiting");
        })
        .join();

        assert!(writer.join().unwrap().is_err());
        let counts = match state.counts.lock() {
            Ok(_) => panic!("coordinator mutex should be poisoned"),
            Err(error) => error.into_inner(),
        };
        assert_eq!(
            counts.waiting_writers, 0,
            "failed admission must unregister the queued writer"
        );
    }

    /// A steady stream of readers must not be able to postpone a queued mutation
    /// indefinitely. Once a seed or clear has queued, later readers wait behind
    /// it, and the mutation runs before any of them acquire.
    #[test]
    fn queued_mutation_blocks_later_readers_and_runs_first() {
        let temp_dir = TempDir::new().unwrap();
        let config = test_config(&temp_dir);
        let index_name = "writer_fairness";

        // A reader is already in flight, holding the index open for reads.
        let first_reader = begin_index_read_blocking(&config, index_name).unwrap();

        let (order_tx, order_rx) = mpsc::channel();

        // Queue a mutation; it must wait for the in-flight reader to drain.
        let writer_config = config.clone();
        let writer_order = order_tx.clone();
        let (writer_release_tx, writer_release_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            with_index_mutation(&writer_config, index_name, || {
                writer_order.send("writer").unwrap();
                writer_release_rx.recv().unwrap();
                Ok(())
            })
        });

        // Wait until the mutation has registered itself as waiting.
        wait_until_waiting_writers(&config, index_name, 1);

        // A later reader must not overtake the queued mutation.
        let reader_config = config.clone();
        let reader_order = order_tx.clone();
        let late_reader = std::thread::spawn(move || {
            let guard = begin_index_read_blocking(&reader_config, index_name).unwrap();
            reader_order.send("reader").unwrap();
            drop(guard);
        });

        // Nothing acquires while the in-flight reader still holds and the writer
        // is queued: the later reader is parked behind the mutation.
        assert_eq!(
            order_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "a later reader must not overtake a queued mutation"
        );

        // Draining the in-flight reader lets the queued writer proceed first.
        drop(first_reader);
        assert_eq!(order_rx.recv().unwrap(), "writer");

        // Only after the mutation finishes may the later reader acquire.
        writer_release_tx.send(()).unwrap();
        writer.join().unwrap().unwrap();
        assert_eq!(order_rx.recv().unwrap(), "reader");
        late_reader.join().unwrap();
    }
}
