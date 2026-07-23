use crate::error::{FlapjackError, Result};
use crate::index::write_queue::{WriteAction, WriteOp};
use crate::types::{TaskInfo, TaskStatus};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
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

pub(crate) struct WriteAdmissionStore {
    path: PathBuf,
    lifecycle_lock: Mutex<()>,
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
            lifecycle_lock: Mutex::new(()),
            #[cfg(test)]
            before_empty_directory_remove_hook: Mutex::new(None),
            #[cfg(test)]
            lifecycle_contention_hook: Mutex::new(None),
        };
        store.load_records()?;
        Ok(store)
    }

    pub(crate) fn append_record(
        &self,
        mut record: WriteAdmissionRecord,
    ) -> Result<WriteAdmissionRecord> {
        let _guard = self.lock_lifecycle()?;
        record.sequence = self.next_sequence()?;
        let record_value = serde_json::to_value(&record).map_err(|error| {
            FlapjackError::Json(format!(
                "failed to serialize write admission record: {error}"
            ))
        })?;
        let envelope = WriteAdmissionEnvelope {
            checksum: record_value_checksum(&record_value)?,
            record: record_value,
        };
        let contents = serde_json::to_vec(&envelope).map_err(|error| {
            FlapjackError::Json(format!(
                "failed to serialize write admission record: {error}"
            ))
        })?;
        let final_path = self.record_path(record.sequence);
        let tmp_path = self.path.join(format!("{:020}.tmp", record.sequence));
        let admission_directory_existed = self.path.exists();
        let admission_directory_needs_parent_sync =
            !admission_directory_existed || self.is_empty()?;

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
            sync_directory(&self.path)?;
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
        Ok(record)
    }

    pub(crate) fn load_records(&self) -> Result<Vec<WriteAdmissionRecord>> {
        let _guard = self.lock_lifecycle()?;
        self.load_records_unlocked()
    }

    fn load_records_unlocked(&self) -> Result<Vec<WriteAdmissionRecord>> {
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
        let _guard = self.lock_lifecycle()?;

        let mut removed_records = Vec::new();
        for path in self.sorted_record_paths()? {
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let record = read_record(&path)?;
            if task_ids.contains(record.task_id.as_str()) {
                let record_bytes = fs::read(&path)?;
                fs::remove_file(&path)?;
                removed_records.push((path, record_bytes));
            }
        }
        if removed_records.is_empty() {
            return Ok(());
        }

        let cleanup_result = if self.is_empty()? {
            #[cfg(test)]
            self.run_before_empty_directory_remove_hook();
            fs::remove_dir(&self.path)?;
            sync_parent_directory(&self.path)
        } else {
            sync_directory(&self.path)
        };
        if let Err(error) = cleanup_result {
            self.restore_removed_records(&removed_records)?;
            return Err(error);
        }

        Ok(())
    }

    fn next_sequence(&self) -> Result<u64> {
        Ok(self
            .load_records_unlocked()?
            .into_iter()
            .map(|record| record.sequence)
            .max()
            .unwrap_or(0)
            + 1)
    }

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

    fn is_empty(&self) -> Result<bool> {
        Ok(self.sorted_record_paths()?.into_iter().all(|path| {
            !path
                .extension()
                .is_some_and(|ext| ext == "json" || ext == "tmp")
        }))
    }

    fn restore_removed_records(&self, removed_records: &[(PathBuf, Vec<u8>)]) -> Result<()> {
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
        Ok(())
    }

    fn lock_lifecycle(&self) -> Result<MutexGuard<'_, ()>> {
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
