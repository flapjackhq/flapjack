//! Thread-safe registry tracking which indexes are paused, used to reject writes with 503 during migration.
use dashmap::DashSet;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

const GLOBAL_FENCE_FILE: &str = "release-write-fence.json";
const GLOBAL_FENCE_KIND: &str = "flapjack_global_mutation_fence";

/// Builds the hidden sibling name `.<data_root_name>.<suffix>` that release
/// state files use next to a data root, preserving the raw (possibly
/// non-UTF-8) bytes of the data-root name.
pub(crate) fn data_root_sibling_name(data_root_name: &OsStr, suffix: &str) -> OsString {
    let mut sibling_name = OsString::with_capacity(data_root_name.len() + suffix.len() + 2);
    sibling_name.push(".");
    sibling_name.push(data_root_name);
    sibling_name.push(".");
    sibling_name.push(suffix);
    sibling_name
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedGlobalFence {
    schema_version: u8,
    kind: String,
    transaction_id: String,
    active: bool,
}

impl<'de> Deserialize<'de> for PersistedGlobalFence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FenceVisitor;

        impl<'de> Visitor<'de> for FenceVisitor {
            type Value = PersistedGlobalFence;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a strict global mutation fence object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut schema_version = None;
                let mut kind = None;
                let mut transaction_id = None;
                let mut active = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "schemaVersion" if schema_version.is_none() => {
                            schema_version = Some(map.next_value()?)
                        }
                        "kind" if kind.is_none() => kind = Some(map.next_value()?),
                        "transactionId" if transaction_id.is_none() => {
                            transaction_id = Some(map.next_value()?)
                        }
                        "active" if active.is_none() => active = Some(map.next_value()?),
                        "schemaVersion" | "kind" | "transactionId" | "active" => {
                            return Err(de::Error::custom(format!(
                                "duplicate global mutation fence field: {key}"
                            )))
                        }
                        _ => return Err(de::Error::unknown_field(&key, &[])),
                    }
                }
                Ok(PersistedGlobalFence {
                    schema_version: schema_version
                        .ok_or_else(|| de::Error::missing_field("schemaVersion"))?,
                    kind: kind.ok_or_else(|| de::Error::missing_field("kind"))?,
                    transaction_id: transaction_id
                        .ok_or_else(|| de::Error::missing_field("transactionId"))?,
                    active: active.ok_or_else(|| de::Error::missing_field("active"))?,
                })
            }
        }

        deserializer.deserialize_map(FenceVisitor)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GlobalMutationFenceStatus {
    pub(crate) transaction_id: String,
}

#[derive(Clone)]
#[doc(hidden)]
pub struct MutationPermit {
    _guard: Arc<tokio::sync::OwnedRwLockReadGuard<Option<PersistedGlobalFence>>>,
}

tokio::task_local! {
    static REQUEST_MUTATION_PERMIT: MutationPermit;
}

pub(crate) async fn scope_request_mutation<F>(permit: MutationPermit, future: F) -> F::Output
where
    F: Future,
{
    REQUEST_MUTATION_PERMIT.scope(permit, future).await
}

pub(crate) fn request_mutation_permit() -> Option<MutationPermit> {
    REQUEST_MUTATION_PERMIT.try_with(Clone::clone).ok()
}

#[derive(Debug, Error)]
pub(crate) enum MutationFenceError {
    #[error("invalid release transaction identifier")]
    InvalidTransaction,
    #[error("release mutation fence is active for another transaction")]
    Conflict,
    #[error("release mutation fence storage failed: {0}")]
    Storage(#[from] std::io::Error),
}

#[derive(Clone)]
#[doc(hidden)]
pub struct GlobalMutationFence {
    state_path: Arc<PathBuf>,
    state: Arc<tokio::sync::RwLock<Option<PersistedGlobalFence>>>,
}

impl GlobalMutationFence {
    pub fn open(data_root: &Path) -> std::io::Result<Self> {
        let canonical_data_root = data_root.canonicalize()?;
        let parent = canonical_data_root.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Flapjack data root has no parent for global mutation fence state",
            )
        })?;
        let name = canonical_data_root.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Flapjack data root has no file name",
            )
        })?;
        let state_file = if name == OsStr::new("data") {
            OsString::from(GLOBAL_FENCE_FILE)
        } else {
            data_root_sibling_name(name, GLOBAL_FENCE_FILE)
        };
        let state_path = parent.join(state_file);
        let persisted = match std::fs::symlink_metadata(&state_path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "global mutation fence state must be a regular non-symlink file",
                    ));
                }
                let bytes = std::fs::read(&state_path)?;
                let record: PersistedGlobalFence = serde_json::from_slice(&bytes)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
                validate_persisted_fence(&record)?;
                Some(record)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok(Self {
            state_path: Arc::new(state_path),
            state: Arc::new(tokio::sync::RwLock::new(persisted)),
        })
    }

    pub(crate) async fn admit_mutation(&self) -> Result<MutationPermit, MutationFenceError> {
        let guard = Arc::new(Arc::clone(&self.state).read_owned().await);
        if guard.as_ref().as_ref().is_some_and(|record| record.active) {
            return Err(MutationFenceError::Conflict);
        }
        Ok(MutationPermit { _guard: guard })
    }

    pub(crate) fn try_admit_mutation(&self) -> Result<MutationPermit, MutationFenceError> {
        let guard = Arc::new(
            Arc::clone(&self.state)
                .try_read_owned()
                .map_err(|_| MutationFenceError::Conflict)?,
        );
        if guard.as_ref().as_ref().is_some_and(|record| record.active) {
            return Err(MutationFenceError::Conflict);
        }
        Ok(MutationPermit { _guard: guard })
    }

    pub(crate) async fn acquire(
        &self,
        transaction_id: &str,
    ) -> Result<GlobalMutationFenceStatus, MutationFenceError> {
        validate_transaction_id(transaction_id)?;
        let mut state = self.state.write().await;
        if let Some(record) = state.as_ref() {
            if record.active {
                if record.transaction_id == transaction_id {
                    return Ok(GlobalMutationFenceStatus {
                        transaction_id: transaction_id.to_string(),
                    });
                }
                return Err(MutationFenceError::Conflict);
            }
        }
        let record = PersistedGlobalFence {
            schema_version: 1,
            kind: GLOBAL_FENCE_KIND.to_string(),
            transaction_id: transaction_id.to_string(),
            active: true,
        };
        persist_fence(&self.state_path, &record)?;
        *state = Some(record);
        Ok(GlobalMutationFenceStatus {
            transaction_id: transaction_id.to_string(),
        })
    }

    pub(crate) async fn release(&self, transaction_id: &str) -> Result<(), MutationFenceError> {
        validate_transaction_id(transaction_id)?;
        let mut state = self.state.write().await;
        let record = state.as_ref().ok_or(MutationFenceError::Conflict)?;
        if record.transaction_id != transaction_id {
            return Err(MutationFenceError::Conflict);
        }
        if !record.active {
            return Ok(());
        }
        let released = PersistedGlobalFence {
            active: false,
            ..record.clone()
        };
        persist_fence(&self.state_path, &released)?;
        *state = Some(released);
        Ok(())
    }

    pub(crate) async fn status(&self) -> Option<GlobalMutationFenceStatus> {
        self.state.read().await.as_ref().and_then(|record| {
            record.active.then(|| GlobalMutationFenceStatus {
                transaction_id: record.transaction_id.clone(),
            })
        })
    }
}

fn validate_transaction_id(transaction_id: &str) -> Result<(), MutationFenceError> {
    let valid = !transaction_id.is_empty()
        && transaction_id.len() <= 128
        && !transaction_id.contains("..")
        && transaction_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(MutationFenceError::InvalidTransaction)
    }
}

fn validate_persisted_fence(record: &PersistedGlobalFence) -> std::io::Result<()> {
    if record.schema_version != 1 || record.kind != GLOBAL_FENCE_KIND {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "global mutation fence schema or kind is invalid",
        ));
    }
    validate_transaction_id(&record.transaction_id)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn persist_fence(path: &Path, record: &PersistedGlobalFence) -> std::io::Result<()> {
    let mut payload = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    payload.push(b'\n');
    flapjack::index::atomic_write_private_file(path, &payload)
}

/// Tracks which indexes are currently paused (writes rejected with 503).
/// Thread-safe and lock-free via DashSet.
#[derive(Clone)]
pub struct PausedIndexes {
    inner: Arc<DashSet<String>>,
}

impl PausedIndexes {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashSet::new()),
        }
    }

    /// Mark an index as paused. Idempotent — pausing an already-paused index is a no-op.
    pub fn pause(&self, index_name: &str) {
        self.inner.insert(index_name.to_string());
    }

    /// Clear the paused flag for an index. Idempotent — resuming a non-paused index is a no-op.
    pub fn resume(&self, index_name: &str) {
        self.inner.remove(index_name);
    }

    /// Returns true if the given index is currently paused.
    pub fn is_paused(&self, index_name: &str) -> bool {
        self.inner.contains(index_name)
    }
}

impl Default for PausedIndexes {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard function: returns `Err(FlapjackError::IndexPaused)` if the index is paused.
/// Call at the top of each write handler to reject writes during migration.
pub fn check_not_paused(
    paused: &PausedIndexes,
    index_name: &str,
) -> Result<(), flapjack::error::FlapjackError> {
    if paused.is_paused(index_name) {
        Err(flapjack::error::FlapjackError::IndexPaused(
            index_name.to_string(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn global_fence_waits_for_inflight_mutation_and_survives_restart() {
        let temp = tempfile::TempDir::new().unwrap();
        let data_root = temp.path().join("flapjack/data");
        std::fs::create_dir_all(&data_root).unwrap();
        let fence = GlobalMutationFence::open(&data_root).unwrap();
        let permit = fence.admit_mutation().await.unwrap();

        let acquiring = {
            let fence = fence.clone();
            tokio::spawn(async move { fence.acquire("release-transaction-1").await })
        };
        tokio::task::yield_now().await;
        assert!(!acquiring.is_finished());

        drop(permit);
        acquiring.await.unwrap().unwrap();
        assert_eq!(
            fence.status().await.unwrap().transaction_id,
            "release-transaction-1"
        );
        assert!(fence.admit_mutation().await.is_err());

        let restarted = GlobalMutationFence::open(&data_root).unwrap();
        assert_eq!(
            restarted.status().await.unwrap().transaction_id,
            "release-transaction-1"
        );
        assert!(restarted.release("other-transaction").await.is_err());
        restarted.release("release-transaction-1").await.unwrap();
        assert!(restarted.status().await.is_none());
        restarted.admit_mutation().await.unwrap();
    }

    #[tokio::test]
    async fn cloned_permit_keeps_detached_mutation_counted_until_task_terminal() {
        let temp = tempfile::TempDir::new().unwrap();
        let data_root = temp.path().join("flapjack/data");
        std::fs::create_dir_all(&data_root).unwrap();
        let fence = GlobalMutationFence::open(&data_root).unwrap();
        let permit = fence.admit_mutation().await.unwrap();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let detached = tokio::spawn(async move {
            let _permit = permit.clone();
            let _ = release_rx.await;
        });

        let acquiring = {
            let fence = fence.clone();
            tokio::spawn(async move { fence.acquire("release-detached-1").await })
        };
        tokio::task::yield_now().await;
        assert!(!acquiring.is_finished());

        release_tx.send(()).unwrap();
        detached.await.unwrap();
        acquiring.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn request_permit_clone_survives_a_queued_release_writer() {
        let temp = tempfile::TempDir::new().unwrap();
        let data_root = temp.path().join("flapjack/data");
        std::fs::create_dir_all(&data_root).unwrap();
        let fence = GlobalMutationFence::open(&data_root).unwrap();
        let request_permit = fence.admit_mutation().await.unwrap();

        let acquiring = {
            let fence = fence.clone();
            tokio::spawn(async move { fence.acquire("release-request-child-1").await })
        };
        tokio::task::yield_now().await;
        assert!(!acquiring.is_finished());

        let child_permit = scope_request_mutation(request_permit, async {
            request_mutation_permit().expect("request task must clone its admitted permit")
        })
        .await;
        assert!(
            !acquiring.is_finished(),
            "the child clone must keep the queued release writer behind the request"
        );

        drop(child_permit);
        acquiring.await.unwrap().unwrap();
    }

    #[test]
    fn global_fence_rejects_duplicate_or_symlinked_persisted_state() {
        let temp = tempfile::TempDir::new().unwrap();
        let data_root = temp.path().join("flapjack/data");
        std::fs::create_dir_all(&data_root).unwrap();
        let state_path = temp.path().join("flapjack/release-write-fence.json");
        std::fs::write(
            &state_path,
            br#"{"schemaVersion":1,"transactionId":"one","transactionId":"two"}"#,
        )
        .unwrap();
        assert!(GlobalMutationFence::open(&data_root).is_err());

        std::fs::remove_file(&state_path).unwrap();
        let foreign = temp.path().join("foreign.json");
        std::fs::write(&foreign, b"{}").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&foreign, &state_path).unwrap();
        #[cfg(unix)]
        assert!(GlobalMutationFence::open(&data_root).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn global_fence_accepts_non_utf8_data_root_name() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::TempDir::new().unwrap();
        let data_root = temp.path().join(OsStr::from_bytes(b"utf8-ok-\xff"));
        std::fs::create_dir_all(&data_root).unwrap();

        let fence = GlobalMutationFence::open(&data_root).unwrap();

        let expected_state_path = temp
            .path()
            .canonicalize()
            .unwrap()
            .join(OsStr::from_bytes(b".utf8-ok-\xff.release-write-fence.json"));
        assert_eq!(*fence.state_path, expected_state_path);
    }

    #[test]
    fn test_pause_registry_starts_empty() {
        let registry = PausedIndexes::new();
        assert!(
            !registry.is_paused("foo"),
            "new registry should have no paused indexes"
        );
        assert!(
            !registry.is_paused("bar"),
            "new registry should have no paused indexes"
        );
    }

    #[test]
    fn test_pause_marks_index_as_paused() {
        let registry = PausedIndexes::new();
        registry.pause("foo");
        assert!(
            registry.is_paused("foo"),
            "after pause('foo'), is_paused('foo') should be true"
        );
    }

    #[test]
    fn test_resume_clears_paused_flag() {
        let registry = PausedIndexes::new();
        registry.pause("foo");
        registry.resume("foo");
        assert!(
            !registry.is_paused("foo"),
            "after pause then resume, is_paused should be false"
        );
    }

    #[test]
    fn test_pause_is_per_index() {
        let registry = PausedIndexes::new();
        registry.pause("foo");
        assert!(registry.is_paused("foo"), "foo should be paused");
        assert!(!registry.is_paused("bar"), "bar should NOT be paused");
    }

    #[test]
    fn test_double_pause_is_idempotent() {
        let registry = PausedIndexes::new();
        registry.pause("foo");
        registry.pause("foo"); // second call should not panic or error
        assert!(
            registry.is_paused("foo"),
            "foo should still be paused after double pause"
        );
    }

    #[test]
    fn test_double_resume_is_idempotent() {
        let registry = PausedIndexes::new();
        // resume without ever pausing — should be a no-op
        registry.resume("foo");
        assert!(!registry.is_paused("foo"), "foo should not be paused");

        // pause, resume, resume — second resume should be a no-op
        registry.pause("bar");
        registry.resume("bar");
        registry.resume("bar");
        assert!(
            !registry.is_paused("bar"),
            "bar should not be paused after double resume"
        );
    }

    #[test]
    fn test_check_not_paused_ok_when_not_paused() {
        let registry = PausedIndexes::new();
        assert!(check_not_paused(&registry, "foo").is_ok());
    }

    #[test]
    fn test_check_not_paused_err_when_paused() {
        let registry = PausedIndexes::new();
        registry.pause("foo");
        let result = check_not_paused(&registry, "foo");
        assert!(result.is_err());
        match result.unwrap_err() {
            flapjack::error::FlapjackError::IndexPaused(name) => {
                assert_eq!(name, "foo");
            }
            other => panic!("expected IndexPaused, got {:?}", other),
        }
    }

    #[test]
    fn test_check_not_paused_per_index() {
        let registry = PausedIndexes::new();
        registry.pause("foo");
        assert!(
            check_not_paused(&registry, "foo").is_err(),
            "foo is paused, should be Err"
        );
        assert!(
            check_not_paused(&registry, "bar").is_ok(),
            "bar is not paused, should be Ok"
        );
    }

    /// Verify that concurrent pause and resume calls on the same index do not panic or deadlock.
    ///
    /// Spawns 100 threads that alternate between pausing and resuming a shared index.
    /// The final paused state is intentionally unasserted since it depends on thread
    /// scheduling; the test validates absence of panics, data races, and deadlocks.
    #[test]
    fn test_pause_resume_concurrent_safe() {
        let registry = PausedIndexes::new();
        let mut handles = Vec::new();

        for i in 0..100 {
            let r = registry.clone();
            let handle = std::thread::spawn(move || {
                if i % 2 == 0 {
                    r.pause("shared");
                } else {
                    r.resume("shared");
                }
            });
            handles.push(handle);
        }

        for h in handles {
            h.join().expect("thread should not panic");
        }

        // No assertion on final state (it's racy), just verifying no panic/deadlock
        let _ = registry.is_paused("shared");
    }
}
