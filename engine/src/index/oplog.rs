use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const SEGMENT_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub(crate) const OPLOG_DIR: &str = "oplog";
pub(crate) const COMMITTED_SEQ_FILE: &str = "committed_seq";
const OPLOG_TASK_ID_FIELD: &str = "_flapjack_task_id";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpLogEntry {
    pub seq: u64,
    pub timestamp_ms: u64,
    pub node_id: String,
    pub tenant_id: String,
    pub op_type: String,
    pub payload: serde_json::Value,
}

struct ActiveSegment {
    writer: BufWriter<File>,
    path: PathBuf,
    size: u64,
    id: u32,
}

pub struct OpLog {
    dir: PathBuf,
    tenant_id: String,
    node_id: String,
    current_seq: AtomicU64,
    segment: Mutex<ActiveSegment>,
}

fn committed_seq_path(tenant_path: &Path) -> PathBuf {
    tenant_path.join(COMMITTED_SEQ_FILE)
}

/// Read the durable committed sequence number for a tenant.
/// Returns 0 when the sidecar is missing, unreadable, or malformed.
pub fn read_committed_seq(tenant_path: &Path) -> u64 {
    let path = committed_seq_path(tenant_path);
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or(0)
}

/// Persist the durable committed sequence number for a tenant.
pub fn write_committed_seq(tenant_path: &Path, seq: u64) -> std::io::Result<()> {
    let path = committed_seq_path(tenant_path);
    fs::create_dir_all(tenant_path)?;
    let tmp_path = tenant_path.join(format!(
        ".{COMMITTED_SEQ_FILE}.{seq}.{}.tmp",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(seq.to_string().as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp_path, &path)?;
        File::open(tenant_path)?.sync_all()?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_result
}

impl OpLog {
    /// Open or create an operation log rooted at `dir`.
    ///
    /// Creates the directory if it does not exist, scans for existing segments to recover the latest sequence number, and opens the most recent segment file for appending.
    ///
    /// # Arguments
    ///
    /// * `dir` - Directory where segment files are stored.
    /// * `tenant_id` - Tenant identifier stamped on every entry.
    /// * `node_id` - Node identifier stamped on every entry.
    pub fn open(dir: &Path, tenant_id: &str, node_id: &str) -> crate::error::Result<Self> {
        fs::create_dir_all(dir)?;

        let (max_seq, max_seg_id) = Self::scan_existing(dir)?;
        let next_seg_id = if max_seg_id > 0 { max_seg_id } else { 1 };
        let seg_path = dir.join(format!("segment_{:04}.jsonl", next_seg_id));
        let seg_size = seg_path.metadata().map(|m| m.len()).unwrap_or(0);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&seg_path)?;

        Ok(OpLog {
            dir: dir.to_path_buf(),
            tenant_id: tenant_id.to_string(),
            node_id: node_id.to_string(),
            current_seq: AtomicU64::new(max_seq),
            segment: Mutex::new(ActiveSegment {
                writer: BufWriter::new(file),
                path: seg_path,
                size: seg_size,
                id: next_seg_id,
            }),
        })
    }

    /// Scan the oplog directory for existing segment files and return the highest sequence number and segment ID found.
    ///
    /// # Returns
    ///
    /// A tuple of `(max_seq, max_seg_id)`. Returns `(0, 0)` when no segments exist.
    fn scan_existing(dir: &Path) -> crate::error::Result<(u64, u32)> {
        let mut max_seq: u64 = 0;
        let mut max_seg_id: u32 = 0;

        let entries = sorted_segment_entries(dir)?;

        for entry in &entries {
            let name = entry.file_name();
            let name_str = name.to_str().unwrap_or("");
            if let Some(id_str) = name_str
                .strip_prefix("segment_")
                .and_then(|s| s.strip_suffix(".jsonl"))
            {
                if let Ok(id) = id_str.parse::<u32>() {
                    if id > max_seg_id {
                        max_seg_id = id;
                    }
                }
            }
        }

        if let Some(last) = entries.last() {
            let f = File::open(last.path())?;
            let reader = BufReader::new(f);
            for line in reader.lines() {
                let line = line?;
                if let Ok(entry) = serde_json::from_str::<OpLogEntry>(&line) {
                    if entry.seq > max_seq {
                        max_seq = entry.seq;
                    }
                }
            }
        }

        Ok((max_seq, max_seg_id))
    }

    pub fn current_seq(&self) -> u64 {
        self.current_seq.load(Ordering::SeqCst)
    }

    /// Return the sequence number of the oldest retained operation, if any.
    pub fn oldest_seq(&self) -> Option<u64> {
        let mut segment = self.segment.lock().ok()?;
        segment.writer.flush().ok()?;
        drop(segment);

        let entries = sorted_segment_entries(&self.dir).ok()?;
        for entry in entries {
            let file = File::open(entry.path()).ok()?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line.ok()?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(op) = serde_json::from_str::<OpLogEntry>(&line) {
                    return Some(op.seq);
                }
            }
        }

        None
    }

    /// Append a single operation to the log and return its assigned sequence number.
    ///
    /// Atomically increments the sequence counter, serializes the entry as a JSON line, flushes to disk, and rotates the segment file when it exceeds `SEGMENT_MAX_BYTES`.
    ///
    /// # Arguments
    ///
    /// * `op_type` - Operation kind (e.g. `"upsert"`, `"delete"`).
    /// * `payload` - Arbitrary JSON payload for the operation.
    pub fn append(&self, op_type: &str, payload: serde_json::Value) -> crate::error::Result<u64> {
        let seq = self.current_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let entry = OpLogEntry {
            seq,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            node_id: self.node_id.clone(),
            tenant_id: self.tenant_id.clone(),
            op_type: op_type.to_string(),
            payload,
        };

        let line = serde_json::to_string(&entry)
            .map_err(|e| crate::error::FlapjackError::Io(e.to_string()))?;

        let mut seg = self.segment.lock().unwrap();
        seg.writer.write_all(line.as_bytes())?;
        seg.writer.write_all(b"\n")?;
        seg.writer.flush()?;
        seg.size += line.len() as u64 + 1;

        if seg.size >= SEGMENT_MAX_BYTES {
            self.rotate_segment_locked(&mut seg)?;
        }

        Ok(seq)
    }

    /// Append multiple operations in a single lock acquisition and return the last assigned sequence number.
    ///
    /// All entries share the same timestamp. The segment is rotated after the batch if the size threshold is exceeded.
    ///
    /// # Arguments
    ///
    /// * `ops` - Slice of `(op_type, payload)` pairs to append.
    pub fn append_batch(&self, ops: &[(String, serde_json::Value)]) -> crate::error::Result<u64> {
        self.append_batch_with_task_id(None, ops)
    }

    pub fn append_batch_for_task(
        &self,
        task_id: &str,
        ops: &[(String, serde_json::Value)],
    ) -> crate::error::Result<u64> {
        self.append_batch_with_task_id(Some(task_id), ops)
    }

    fn append_batch_with_task_id(
        &self,
        task_id: Option<&str>,
        ops: &[(String, serde_json::Value)],
    ) -> crate::error::Result<u64> {
        let mut last_seq = self.current_seq.load(Ordering::SeqCst);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let mut seg = self.segment.lock().unwrap();
        for (op_type, payload) in ops {
            last_seq += 1;
            let mut payload = payload.clone();
            if let (Some(task_id), Some(object)) = (task_id, payload.as_object_mut()) {
                object.insert(
                    OPLOG_TASK_ID_FIELD.to_string(),
                    serde_json::Value::String(task_id.to_string()),
                );
            }
            let entry = OpLogEntry {
                seq: last_seq,
                timestamp_ms: now,
                node_id: self.node_id.clone(),
                tenant_id: self.tenant_id.clone(),
                op_type: op_type.clone(),
                payload,
            };
            let line = serde_json::to_string(&entry)
                .map_err(|e| crate::error::FlapjackError::Io(e.to_string()))?;
            seg.writer.write_all(line.as_bytes())?;
            seg.writer.write_all(b"\n")?;
            seg.size += line.len() as u64 + 1;
        }
        seg.writer.flush()?;
        if task_id.is_some() {
            seg.writer.get_ref().sync_all()?;
        }
        self.current_seq.store(last_seq, Ordering::SeqCst);

        if seg.size >= SEGMENT_MAX_BYTES {
            self.rotate_segment_locked(&mut seg)?;
        }

        Ok(last_seq)
    }

    pub(crate) fn committed_task_ids(
        &self,
        committed_seq: u64,
    ) -> crate::error::Result<BTreeSet<String>> {
        Ok(self
            .read_since(0)?
            .into_iter()
            .filter(|entry| entry.seq <= committed_seq)
            .filter_map(|entry| {
                entry
                    .payload
                    .get(OPLOG_TASK_ID_FIELD)
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect())
    }

    fn rotate_segment_locked(&self, seg: &mut ActiveSegment) -> crate::error::Result<()> {
        seg.writer.flush()?;
        seg.id += 1;
        let new_path = self.dir.join(format!("segment_{:04}.jsonl", seg.id));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&new_path)?;
        seg.writer = BufWriter::new(file);
        seg.path = new_path;
        seg.size = 0;
        Ok(())
    }

    /// Read all entries with a sequence number strictly greater than `since_seq`.
    ///
    /// Flushes the active writer before reading, scans every segment file in order, and returns results sorted by sequence number.
    pub fn read_since(&self, since_seq: u64) -> crate::error::Result<Vec<OpLogEntry>> {
        let mut results = Vec::new();
        let entries = sorted_segment_entries(&self.dir)?;

        {
            let mut seg = self.segment.lock().unwrap();
            seg.writer.flush()?;
        }

        for entry in entries {
            let f = File::open(entry.path())?;
            let reader = BufReader::new(f);
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<OpLogEntry>(&line) {
                    Ok(op) => {
                        if op.seq > since_seq {
                            results.push(op);
                        }
                    }
                    Err(_) => continue,
                }
            }
        }
        results.sort_by_key(|e| e.seq);
        Ok(results)
    }

    /// Remove old segment files whose entries all have sequence numbers below `before_seq`.
    ///
    /// Skips the currently active segment. Only deletes a file when every entry in it has a sequence number less than `before_seq`.
    ///
    /// # Returns
    ///
    /// The number of segment files removed.
    pub fn truncate_before(&self, before_seq: u64) -> crate::error::Result<u64> {
        let mut removed = 0u64;
        let seg = self.segment.lock().unwrap();
        let current_seg_name = seg.path.file_name().unwrap().to_str().unwrap().to_string();
        drop(seg);

        let entries = sorted_segment_entries(&self.dir)?;

        for entry in entries {
            let name = entry.file_name().to_str().unwrap().to_string();
            if name == current_seg_name {
                continue;
            }
            let f = File::open(entry.path())?;
            let reader = BufReader::new(f);
            let mut max_seq_in_file = 0u64;
            for line in reader.lines() {
                let line = line?;
                if let Ok(op) = serde_json::from_str::<OpLogEntry>(&line) {
                    if op.seq > max_seq_in_file {
                        max_seq_in_file = op.seq;
                    }
                }
            }
            if max_seq_in_file > 0 && max_seq_in_file < before_seq {
                fs::remove_file(entry.path())?;
                removed += 1;
            }
        }

        Ok(removed)
    }
}

fn sorted_segment_entries(dir: &Path) -> std::io::Result<Vec<std::fs::DirEntry>> {
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .map(|name| name.starts_with("segment_") && name.ends_with(".jsonl"))
                .unwrap_or(false)
        })
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Verify that appending entries increments the sequence counter and that `read_since` correctly filters by sequence number.
    #[test]
    fn test_append_and_read() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

        assert_eq!(oplog.current_seq(), 0);
        let s1 = oplog
            .append("upsert", serde_json::json!({"objectID": "1"}))
            .unwrap();
        assert_eq!(s1, 1);
        let s2 = oplog
            .append("delete", serde_json::json!({"objectID": "2"}))
            .unwrap();
        assert_eq!(s2, 2);

        let all = oplog.read_since(0).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].seq, 1);
        assert_eq!(all[1].seq, 2);

        let since1 = oplog.read_since(1).unwrap();
        assert_eq!(since1.len(), 1);
        assert_eq!(since1[0].seq, 2);
    }

    /// Verify that `append_batch` assigns contiguous sequence numbers and all entries are retrievable.
    #[test]
    fn test_batch_append() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

        let ops: Vec<(String, serde_json::Value)> = vec![
            ("upsert".into(), serde_json::json!({"objectID": "a"})),
            ("upsert".into(), serde_json::json!({"objectID": "b"})),
            ("delete".into(), serde_json::json!({"objectID": "c"})),
        ];
        let last = oplog.append_batch(&ops).unwrap();
        assert_eq!(last, 3);
        assert_eq!(oplog.current_seq(), 3);

        let all = oplog.read_since(0).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn committed_task_ids_exclude_logged_but_uncommitted_entries() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();
        oplog
            .append_batch_for_task(
                "committed_task",
                &[(
                    "upsert".into(),
                    serde_json::json!({"objectID": "a", "body": {"objectID": "a"}}),
                )],
            )
            .unwrap();
        oplog
            .append_batch_for_task(
                "logged_uncommitted_task",
                &[(
                    "upsert".into(),
                    serde_json::json!({"objectID": "b", "body": {"objectID": "b"}}),
                )],
            )
            .unwrap();

        assert_eq!(
            oplog.committed_task_ids(1).unwrap(),
            BTreeSet::from(["committed_task".to_string()]),
            "admission reconciliation must not treat pre-commit oplog append as durable completion"
        );
    }

    #[cfg(unix)]
    #[test]
    fn task_tagged_append_rejects_unsyncable_segment_before_advancing_seq() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let segment_path = tmp.path().join("segment_0001.jsonl");
        symlink("/dev/null", &segment_path).unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

        let result = oplog.append_batch_for_task(
            "crash_boundary_task",
            &[(
                "upsert".into(),
                serde_json::json!({"objectID": "a", "body": {"objectID": "a"}}),
            )],
        );

        assert!(
            result.is_err(),
            "task-tagged append must fail when the segment cannot be synced"
        );
        assert_eq!(
            oplog.current_seq(),
            0,
            "task-tagged append must not publish a sequence before durable sync succeeds"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_committed_seq_replaces_existing_path_instead_of_following_it() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        std::fs::create_dir_all(&tenant_path).unwrap();
        let committed_path = tenant_path.join("committed_seq");
        symlink("/dev/null", &committed_path).unwrap();

        write_committed_seq(&tenant_path, 42).unwrap();

        let metadata = std::fs::symlink_metadata(&committed_path).unwrap();
        assert!(
            !metadata.file_type().is_symlink() && metadata.file_type().is_file(),
            "committed_seq must be atomically installed as a regular durable sidecar"
        );
        assert_eq!(read_committed_seq(&tenant_path), 42);
    }

    /// Verify that reopening an oplog on the same directory resumes from the previously written sequence number without gaps or duplicates.
    #[test]
    fn test_reopen_continues_seq() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        {
            let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
            oplog.append("upsert", serde_json::json!({"x": 1})).unwrap();
            oplog.append("upsert", serde_json::json!({"x": 2})).unwrap();
        }

        let oplog2 = OpLog::open(&dir, "t1", "node1").unwrap();
        assert_eq!(oplog2.current_seq(), 2);
        let s3 = oplog2
            .append("delete", serde_json::json!({"x": 3}))
            .unwrap();
        assert_eq!(s3, 3);

        let all = oplog2.read_since(0).unwrap();
        assert_eq!(all.len(), 3);
    }

    /// Verify that `truncate_before` removes only segments whose entries are entirely below the threshold, leaving newer entries intact.
    #[test]
    fn test_truncate() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        {
            let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
            for i in 0..5 {
                oplog.append("upsert", serde_json::json!({"i": i})).unwrap();
            }
            oplog
                .rotate_segment_locked(&mut oplog.segment.lock().unwrap())
                .unwrap();
            for i in 5..10 {
                oplog.append("upsert", serde_json::json!({"i": i})).unwrap();
            }
        }

        let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
        let removed = oplog.truncate_before(6).unwrap();
        assert_eq!(removed, 1);

        let remaining = oplog.read_since(0).unwrap();
        assert_eq!(remaining.len(), 5);
        assert_eq!(remaining[0].seq, 6);
    }

    #[test]
    fn test_oldest_seq_none_when_no_entries() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

        assert_eq!(oplog.oldest_seq(), None);
    }
    #[test]
    fn test_oldest_seq_after_truncate_before() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        {
            let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
            for i in 0..5 {
                oplog.append("upsert", serde_json::json!({"i": i})).unwrap();
            }
            oplog
                .rotate_segment_locked(&mut oplog.segment.lock().unwrap())
                .unwrap();
            for i in 5..10 {
                oplog.append("upsert", serde_json::json!({"i": i})).unwrap();
            }
        }

        let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
        assert_eq!(oplog.oldest_seq(), Some(1));

        let removed = oplog.truncate_before(6).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(oplog.oldest_seq(), Some(6));
    }

    #[test]
    fn test_read_write_committed_seq_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        std::fs::create_dir_all(&tenant_path).unwrap();

        assert_eq!(read_committed_seq(&tenant_path), 0);
        write_committed_seq(&tenant_path, 42).unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 42);
    }

    #[test]
    fn test_oldest_seq_active_segment_only() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

        oplog.append("upsert", serde_json::json!({"a": 1})).unwrap();
        oplog.append("upsert", serde_json::json!({"a": 2})).unwrap();
        oplog.append("upsert", serde_json::json!({"a": 3})).unwrap();

        // Without any segment rotation, oldest_seq should still read
        // the first entry from the flushed active segment.
        assert_eq!(oplog.oldest_seq(), Some(1));
    }

    #[test]
    fn test_read_committed_seq_malformed_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        std::fs::create_dir_all(&tenant_path).unwrap();

        // Write non-numeric content to the sidecar file.
        std::fs::write(tenant_path.join("committed_seq"), "not-a-number").unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 0);

        // Write empty content.
        std::fs::write(tenant_path.join("committed_seq"), "").unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 0);
    }

    #[test]
    fn test_read_committed_seq_missing_file_returns_zero() {
        let tmp = TempDir::new().unwrap();
        // Tenant path exists as a directory but has no committed_seq file.
        let tenant_path = tmp.path().join("tenant_no_file");
        std::fs::create_dir_all(&tenant_path).unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 0);

        // Tenant path does not exist at all.
        let missing_path = tmp.path().join("nonexistent_tenant");
        assert_eq!(read_committed_seq(&missing_path), 0);
    }

    #[test]
    fn test_write_committed_seq_overwrites_previous() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        std::fs::create_dir_all(&tenant_path).unwrap();

        write_committed_seq(&tenant_path, 42).unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 42);

        write_committed_seq(&tenant_path, 100).unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 100);
    }
}
