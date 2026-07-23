use crate::error::{FlapjackError, Result};
use crate::index::write_queue::{WriteAction, WriteOp};
use crate::types::{TaskInfo, TaskStatus};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::Condvar;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
type LifecycleHook = Box<dyn FnOnce() + Send>;

pub(crate) const WRITE_ADMISSION_DIR: &str = "write_admission";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WriteAdmissionRecord {
    pub sequence: u64,
    pub task_id: String,
    pub numeric_id: i64,
    pub received_documents: usize,
    pub created_at_ms: u64,
    pub actions: Vec<WriteAction>,
}

impl WriteAdmissionRecord {
    pub(crate) fn new(
        task_id: String,
        numeric_id: i64,
        received_documents: usize,
        actions: Vec<WriteAction>,
    ) -> Self {
        Self {
            sequence: 0,
            task_id,
            numeric_id,
            received_documents,
            created_at_ms: system_time_ms(SystemTime::now()),
            actions,
        }
    }

    pub(crate) fn task_info(&self) -> TaskInfo {
        let mut task = TaskInfo::new(
            self.task_id.clone(),
            self.numeric_id,
            self.received_documents,
        );
        task.status = TaskStatus::Enqueued;
        task.created_at = UNIX_EPOCH + Duration::from_millis(self.created_at_ms);
        task
    }

    pub(crate) fn write_op(&self) -> WriteOp {
        WriteOp {
            task_id: self.task_id.clone(),
            actions: self.actions.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct WriteAdmissionEnvelope {
    checksum: String,
    record: serde_json::Value,
}

/// A record that has been staged on disk (temp-written, data-synced, and renamed
/// into its final path) but whose directory entry is not yet guaranteed durable.
/// A staged record only becomes an admitted, crash-safe record once a directory
/// flush covers its rename; `append_record` does not return it until then. This
/// lets concurrent admissions share one directory flush (group commit) instead of
/// each paying an independent, serialized directory fsync.
#[cfg(test)]
struct PendingPublish {
    /// Monotonic publish ticket assigned when the rename completed. A directory
    /// flush that begins after this ticket was staged makes the record durable.
    ticket: u64,
    final_path: PathBuf,
    /// Whether this stage created the admission directory, so a publish-time
    /// rollback knows whether it must also remove that directory.
    created_admission_directory: bool,
    durable: bool,
}

struct AdmissionState {
    #[cfg(test)]
    next_sequence: u64,
    live_record_count: usize,
    /// Monotonic counter of staged records. Assigned under the lifecycle lock so a
    /// directory-flush leader can capture an exact "all renames up to here" bound.
    #[cfg(test)]
    next_ticket: u64,
    /// Records staged but not yet confirmed durable by a directory flush.
    #[cfg(test)]
    pending: Vec<PendingPublish>,
    /// Whether a directory flush is currently in progress. Set under the lifecycle
    /// lock before the leader releases it to fsync, so followers wait instead of
    /// issuing a redundant flush.
    #[cfg(test)]
    flush_in_progress: bool,
}

impl AdmissionState {
    fn empty() -> Self {
        Self {
            #[cfg(test)]
            next_sequence: 1,
            live_record_count: 0,
            #[cfg(test)]
            next_ticket: 0,
            #[cfg(test)]
            pending: Vec::new(),
            #[cfg(test)]
            flush_in_progress: false,
        }
    }

    fn recovered(records: &[WriteAdmissionRecord]) -> Result<Self> {
        #[cfg(not(test))]
        {
            Ok(Self {
                live_record_count: records.len(),
            })
        }

        #[cfg(test)]
        {
            let Some(recovered_max_sequence) = records.iter().map(|record| record.sequence).max()
            else {
                return Ok(Self::empty());
            };
            let next_sequence = recovered_max_sequence.checked_add(1).ok_or_else(|| {
                FlapjackError::Io("write admission sequence space is exhausted".to_string())
            })?;
            Ok(Self {
                next_sequence,
                live_record_count: records.len(),
                next_ticket: 0,
                pending: Vec::new(),
                flush_in_progress: false,
            })
        }
    }

    /// Assign the next publish ticket for a freshly renamed record.
    #[cfg(test)]
    fn take_next_ticket(&mut self) -> u64 {
        self.next_ticket += 1;
        self.next_ticket
    }

    #[cfg(test)]
    fn take_next_sequence(&mut self) -> Result<u64> {
        let sequence = self.next_sequence;
        self.next_sequence = sequence.checked_add(1).ok_or_else(|| {
            FlapjackError::Io("write admission sequence space is exhausted".to_string())
        })?;
        Ok(sequence)
    }
}

pub(crate) struct WriteAdmissionStore {
    path: PathBuf,
    lifecycle_lock: Mutex<AdmissionState>,
    /// Signalled when a directory flush completes so waiting appenders can observe
    /// their record becoming durable (or take over as the next flush leader).
    #[cfg(test)]
    flush_condvar: Condvar,
    #[cfg(test)]
    load_records_unlocked_calls: AtomicUsize,
    #[cfg(test)]
    directory_flush_calls: AtomicUsize,
    #[cfg(test)]
    after_stage_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    before_empty_directory_remove_hook: Mutex<Option<LifecycleHook>>,
    #[cfg(test)]
    lifecycle_contention_hook: Mutex<Option<LifecycleHook>>,
}

impl WriteAdmissionStore {
    pub(crate) fn open(base_path: &Path, tenant_id: &str) -> Result<Self> {
        let path = base_path.join(tenant_id).join(WRITE_ADMISSION_DIR);
        if path.exists() && !path.is_dir() {
            return Err(FlapjackError::Io(format!(
                "{} exists but is not a directory",
                path.display()
            )));
        }
        let store = Self {
            path,
            lifecycle_lock: Mutex::new(AdmissionState::empty()),
            #[cfg(test)]
            flush_condvar: Condvar::new(),
            #[cfg(test)]
            load_records_unlocked_calls: AtomicUsize::new(0),
            #[cfg(test)]
            directory_flush_calls: AtomicUsize::new(0),
            #[cfg(test)]
            after_stage_hook: Mutex::new(None),
            #[cfg(test)]
            before_empty_directory_remove_hook: Mutex::new(None),
            #[cfg(test)]
            lifecycle_contention_hook: Mutex::new(None),
        };
        store.recover_state()?;
        Ok(store)
    }

    /// Durably admit a write record. The record is staged (temp-write, data-sync,
    /// rename) under the lifecycle lock, then a directory flush makes its rename
    /// crash-safe before this returns. Concurrent admissions share one flush.
    #[cfg(test)]
    pub(crate) fn append_record(
        &self,
        mut record: WriteAdmissionRecord,
    ) -> Result<WriteAdmissionRecord> {
        let ticket = self.stage_record(&mut record)?;
        #[cfg(test)]
        self.run_after_stage_hook();
        self.publish_record(ticket)?;
        Ok(record)
    }

    /// Phase A: assign a sequence, serialize, then temp-write + data-sync + rename
    /// the record into its final path under the lifecycle lock. Returns the publish
    /// ticket for the staged record. The directory entry is not yet durable — that
    /// is the job of `publish_record`. On any staging failure the partial state is
    /// rolled back exactly as the pre-group-commit path did and the error is
    /// returned before any pending publish is recorded.
    #[cfg(test)]
    fn stage_record(&self, record: &mut WriteAdmissionRecord) -> Result<u64> {
        let mut state = self.lock_lifecycle()?;
        record.sequence = state.take_next_sequence()?;
        let contents = serialize_record_envelope(record)?;
        let final_path = self.record_path(record.sequence);
        let tmp_path = self.path.join(format!("{:020}.tmp", record.sequence));
        let admission_directory_existed = self.path.exists();
        // A newly created or previously empty admission directory still requires
        // its entry in the tenant directory to be synced before publishing a record.
        let admission_directory_needs_parent_sync =
            !admission_directory_existed || state.live_record_count == 0;

        let write_result = (|| -> Result<()> {
            fs::create_dir_all(&self.path)?;
            if admission_directory_needs_parent_sync {
                sync_parent_directory(&self.path)?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            file.write_all(&contents)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&tmp_path, &final_path)?;
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&tmp_path);
            let _ = fs::remove_file(&final_path);
            if !admission_directory_existed {
                let _ = fs::remove_dir(&self.path);
            }
        }
        write_result?;
        let ticket = state.take_next_ticket();
        state.pending.push(PendingPublish {
            ticket,
            final_path,
            created_admission_directory: !admission_directory_existed,
            durable: false,
        });
        Ok(ticket)
    }

    /// Phase B: make the staged record's rename crash-safe with a directory flush,
    /// then account it as a live record. Does not return until the record is durable
    /// (or rolls back and returns the flush error).
    ///
    /// This is a group commit: concurrent appenders whose renames are already staged
    /// share one directory flush. Exactly one caller becomes the flush leader while
    /// the rest wait on `flush_condvar`; when the leader's flush succeeds it marks
    /// every rename staged before it (ticket `<= target`) durable at once, so the
    /// per-record directory fsync cost is amortized across the batch. A caller only
    /// returns once its own rename is covered by a completed flush.
    #[cfg(test)]
    fn publish_record(&self, ticket: u64) -> Result<()> {
        let mut state = self.lock_lifecycle()?;
        loop {
            if pending_is_durable(&state, ticket) {
                state.pending.retain(|pending| pending.ticket != ticket);
                return Ok(());
            }
            if state.flush_in_progress {
                state = self
                    .flush_condvar
                    .wait(state)
                    .map_err(|_| self.lifecycle_lock_poisoned_error())?;
                continue;
            }

            // Become the flush leader for every rename staged so far. The bound is
            // read under the lock, where no rename is in flight, so all tickets
            // `<= target` are fully renamed and will be made durable by this flush.
            state.flush_in_progress = true;
            let target = state.next_ticket;
            drop(state);

            #[cfg(test)]
            self.directory_flush_calls.fetch_add(1, Ordering::Relaxed);
            let flush_result = sync_directory(&self.path);

            state = self.lock_lifecycle()?;
            state.flush_in_progress = false;
            let outcome = self.resolve_flush_outcome(&mut state, ticket, target, flush_result);
            self.flush_condvar.notify_all();
            return outcome;
        }
    }

    /// Apply a completed directory flush covering all pending tickets `<= target`.
    /// On success, every not-yet-durable pending record in range is counted live;
    /// this caller's own record (`ticket`) is then removed and `Ok` returned. On
    /// failure only this caller's record is rolled back and the error returned;
    /// other pending records stay staged for a later flush attempt.
    #[cfg(test)]
    fn resolve_flush_outcome(
        &self,
        state: &mut AdmissionState,
        ticket: u64,
        target: u64,
        flush_result: Result<()>,
    ) -> Result<()> {
        match flush_result {
            Ok(()) => {
                let mut newly_durable = 0;
                for pending in state.pending.iter_mut() {
                    if pending.ticket <= target && !pending.durable {
                        pending.durable = true;
                        newly_durable += 1;
                    }
                }
                state.live_record_count += newly_durable;
                state.pending.retain(|pending| pending.ticket != ticket);
                Ok(())
            }
            Err(error) => {
                self.roll_back_pending(state, ticket);
                Err(error)
            }
        }
    }

    /// Remove a staged-but-undurable record after a failed directory flush,
    /// deleting its file and — if this stage created the admission directory and no
    /// other live or pending records remain — the now-empty directory.
    #[cfg(test)]
    fn roll_back_pending(&self, state: &mut AdmissionState, ticket: u64) {
        let Some(position) = state
            .pending
            .iter()
            .position(|pending| pending.ticket == ticket)
        else {
            return;
        };
        let pending = state.pending.remove(position);
        let _ = fs::remove_file(&pending.final_path);
        if pending.created_admission_directory
            && state.pending.is_empty()
            && state.live_record_count == 0
        {
            let _ = fs::remove_dir(&self.path);
        }
    }

    fn recover_state(&self) -> Result<()> {
        let mut state = self.lock_lifecycle()?;
        let records = self.load_records_unlocked()?;
        *state = AdmissionState::recovered(&records)?;
        Ok(())
    }

    pub(crate) fn load_records(&self) -> Result<Vec<WriteAdmissionRecord>> {
        let _guard = self.lock_lifecycle()?;
        self.load_records_unlocked()
    }

    fn load_records_unlocked(&self) -> Result<Vec<WriteAdmissionRecord>> {
        #[cfg(test)]
        self.load_records_unlocked_calls
            .fetch_add(1, Ordering::Relaxed);
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        if !self.path.is_dir() {
            return Err(FlapjackError::Io(format!(
                "{} exists but is not a directory",
                self.path.display()
            )));
        }

        let mut records = Vec::new();
        for path in self.sorted_record_paths()? {
            if path.extension().is_some_and(|ext| ext == "tmp") {
                let _ = fs::remove_file(path);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            records.push(read_record(&path)?);
        }
        records.sort_by_key(|record| record.sequence);
        Ok(records)
    }

    pub(crate) fn remove_task(&self, task_id: &str) -> Result<()> {
        self.remove_tasks([task_id])
    }

    pub(crate) fn remove_tasks<'a>(
        &self,
        task_ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<()> {
        let task_ids: BTreeSet<&str> = task_ids.into_iter().collect();
        if task_ids.is_empty() {
            return Ok(());
        }
        let mut state = self.lock_lifecycle()?;

        let mut records_to_remove = Vec::new();
        for path in self.sorted_record_paths()? {
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let record = read_record(&path)?;
            if task_ids.contains(record.task_id.as_str()) {
                let record_bytes = fs::read(&path)?;
                records_to_remove.push((path, record_bytes));
            }
        }
        if records_to_remove.is_empty() {
            return Ok(());
        }

        let remaining_live_record_count = state
            .live_record_count
            .checked_sub(records_to_remove.len())
            .ok_or_else(|| {
                FlapjackError::Io(format!(
                    "write admission state for {} counted fewer live records than removal found",
                    self.path.display()
                ))
            })?;
        // Records staged by a concurrent `append_record` whose directory flush has
        // not yet completed still have files on disk and are not counted live. The
        // admission directory is therefore only empty — and only safe to remove —
        // when no such pending publishes remain.
        #[cfg(test)]
        let has_pending_publishes = state.pending.iter().any(|pending| !pending.durable);
        #[cfg(not(test))]
        let has_pending_publishes = false;
        let cleanup_result = (|| -> Result<()> {
            for (path, _) in &records_to_remove {
                fs::remove_file(path)?;
            }
            if remaining_live_record_count == 0 && !has_pending_publishes {
                #[cfg(test)]
                self.run_before_empty_directory_remove_hook();
                fs::remove_dir(&self.path)?;
                sync_parent_directory(&self.path)?;
            } else {
                sync_directory(&self.path)?;
            }
            Ok(())
        })();
        if let Err(error) = cleanup_result {
            self.restore_removed_records(&records_to_remove)?;
            return Err(error);
        }

        state.live_record_count = remaining_live_record_count;
        Ok(())
    }

    #[cfg(test)]
    fn record_path(&self, sequence: u64) -> PathBuf {
        self.path.join(format!("{sequence:020}.json"))
    }

    fn sorted_record_paths(&self) -> Result<Vec<PathBuf>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let mut paths: Vec<PathBuf> = fs::read_dir(&self.path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        Ok(paths)
    }

    fn restore_removed_records(&self, removed_records: &[(PathBuf, Vec<u8>)]) -> Result<()> {
        let admission_directory_existed = self.path.exists();
        fs::create_dir_all(&self.path)?;
        for (path, record_bytes) in removed_records {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?;
            file.write_all(record_bytes)?;
            file.sync_all()?;
        }
        sync_directory(&self.path)?;
        if !admission_directory_existed {
            sync_parent_directory(&self.path)?;
        }
        Ok(())
    }

    fn lock_lifecycle(&self) -> Result<MutexGuard<'_, AdmissionState>> {
        #[cfg(test)]
        match self.lifecycle_lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::WouldBlock) => self.run_lifecycle_contention_hook(),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(self.lifecycle_lock_poisoned_error())
            }
        }
        self.lifecycle_lock
            .lock()
            .map_err(|_| self.lifecycle_lock_poisoned_error())
    }

    fn lifecycle_lock_poisoned_error(&self) -> FlapjackError {
        FlapjackError::Tantivy(format!(
            "write admission lifecycle lock poisoned for {}",
            self.path.display()
        ))
    }

    #[cfg(test)]
    pub(crate) fn load_records_unlocked_call_count_for_test(&self) -> usize {
        self.load_records_unlocked_calls.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn directory_flush_call_count_for_test(&self) -> usize {
        self.directory_flush_calls.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn set_after_stage_hook(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.after_stage_hook.lock().unwrap() = Some(Arc::new(hook));
    }

    #[cfg(test)]
    fn run_after_stage_hook(&self) {
        // Clone the hook out before invoking it: the hook may block (e.g. on a test
        // barrier) until sibling appenders reach the same point, so holding the hook
        // mutex across the call would deadlock those siblings.
        let hook = self.after_stage_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_before_empty_directory_remove_hook(
        &self,
        hook: impl FnOnce() + Send + 'static,
    ) {
        *self.before_empty_directory_remove_hook.lock().unwrap() = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_before_empty_directory_remove_hook(&self) {
        if let Some(hook) = self
            .before_empty_directory_remove_hook
            .lock()
            .unwrap()
            .take()
        {
            hook();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_lifecycle_contention_hook(&self, hook: impl FnOnce() + Send + 'static) {
        *self.lifecycle_contention_hook.lock().unwrap() = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_lifecycle_contention_hook(&self) {
        if let Some(hook) = self.lifecycle_contention_hook.lock().unwrap().take() {
            hook();
        }
    }
}

pub(crate) fn reconcile_records(
    store: &WriteAdmissionStore,
    applied_task_ids: &BTreeSet<String>,
) -> Result<Vec<WriteAdmissionRecord>> {
    let mut pending = Vec::new();
    for record in store.load_records()? {
        if applied_task_ids.contains(&record.task_id) {
            store.remove_task(&record.task_id)?;
        } else {
            pending.push(record);
        }
    }
    Ok(pending)
}

/// Whether the pending publish for `ticket` has been marked durable by a completed
/// directory flush (its own or a batch leader's).
#[cfg(test)]
fn pending_is_durable(state: &AdmissionState, ticket: u64) -> bool {
    state
        .pending
        .iter()
        .any(|pending| pending.ticket == ticket && pending.durable)
}

/// Serialize a record into its on-disk envelope bytes (checksum + record JSON).
#[cfg(test)]
fn serialize_record_envelope(record: &WriteAdmissionRecord) -> Result<Vec<u8>> {
    let record_value = serde_json::to_value(record).map_err(|error| {
        FlapjackError::Json(format!(
            "failed to serialize write admission record: {error}"
        ))
    })?;
    let envelope = WriteAdmissionEnvelope {
        checksum: record_value_checksum(&record_value)?,
        record: record_value,
    };
    serde_json::to_vec(&envelope).map_err(|error| {
        FlapjackError::Json(format!(
            "failed to serialize write admission record: {error}"
        ))
    })
}

fn read_record(path: &Path) -> Result<WriteAdmissionRecord> {
    let bytes = fs::read(path)?;
    let envelope: WriteAdmissionEnvelope = serde_json::from_slice(&bytes).map_err(|error| {
        FlapjackError::Json(format!(
            "corrupt complete write admission record {}: {error}",
            path.display()
        ))
    })?;
    let expected = record_value_checksum(&envelope.record)?;
    if envelope.checksum != expected {
        return Err(FlapjackError::Json(format!(
            "checksum mismatch in complete write admission record {}",
            path.display()
        )));
    }
    serde_json::from_value(envelope.record).map_err(|error| {
        FlapjackError::Json(format!(
            "corrupt complete write admission record {}: {error}",
            path.display()
        ))
    })
}

fn record_value_checksum(record: &serde_json::Value) -> Result<String> {
    // Normalize through the persisted JSON representation before hashing. In-memory
    // `Number` values created from large nested float payloads can serialize to a
    // representation whose parsed form differs internally, even though the JSON is
    // semantically identical. The on-disk reader always sees the parsed form.
    let serialized_record = serde_json::to_vec(record).map_err(|error| {
        FlapjackError::Json(format!(
            "failed to normalize write admission record checksum: {error}"
        ))
    })?;
    let normalized_record: serde_json::Value =
        serde_json::from_slice(&serialized_record).map_err(|error| {
            FlapjackError::Json(format!(
                "failed to normalize write admission record checksum: {error}"
            ))
        })?;
    let canonical_record = canonicalize_json_value(&normalized_record);
    let bytes = serde_json::to_vec(&canonical_record).map_err(|error| {
        FlapjackError::Json(format!(
            "failed to checksum write admission record: {error}"
        ))
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn canonicalize_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_unstable_by_key(|(key, _)| *key);
            let mut canonical = serde_json::Map::new();
            for (key, value) in entries {
                canonical.insert(key.clone(), canonicalize_json_value(value));
            }
            serde_json::Value::Object(canonical)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonicalize_json_value).collect())
        }
        _ => value.clone(),
    }
}

fn system_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        FlapjackError::Io(format!(
            "{} has no parent directory to sync",
            path.display()
        ))
    })?;
    sync_directory(parent)
}
