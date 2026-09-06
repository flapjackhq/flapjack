//! Stub summary for engine/src/index/oplog.rs.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const SEGMENT_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub(crate) const OPLOG_DIR: &str = "oplog";
pub(crate) const COMMITTED_SEQ_FILE: &str = "committed_seq";
const OPLOG_TASK_ID_FIELD: &str = "_flapjack_task_id";
const OPLOG_ORIGIN_SEQ_FIELD: &str = "_flapjack_origin_seq";

#[cfg(test)]
type AfterBatchSequenceSnapshotHook = std::sync::Arc<dyn Fn() + Send + Sync>;

#[cfg(test)]
static AFTER_BATCH_SEQUENCE_SNAPSHOT_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<AfterBatchSequenceSnapshotHook>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
struct AfterBatchSequenceSnapshotHookGuard {
    previous: Option<AfterBatchSequenceSnapshotHook>,
}

#[cfg(test)]
impl Drop for AfterBatchSequenceSnapshotHookGuard {
    fn drop(&mut self) {
        *after_batch_sequence_snapshot_hook().lock().unwrap() = self.previous.take();
    }
}

#[cfg(test)]
fn after_batch_sequence_snapshot_hook(
) -> &'static std::sync::Mutex<Option<AfterBatchSequenceSnapshotHook>> {
    AFTER_BATCH_SEQUENCE_SNAPSHOT_HOOK.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
fn set_after_batch_sequence_snapshot_hook_for_test(
    hook: impl Fn() + Send + Sync + 'static,
) -> AfterBatchSequenceSnapshotHookGuard {
    let mut slot = after_batch_sequence_snapshot_hook().lock().unwrap();
    AfterBatchSequenceSnapshotHookGuard {
        previous: slot.replace(std::sync::Arc::new(hook)),
    }
}

#[cfg(test)]
fn run_after_batch_sequence_snapshot_hook_for_test() {
    let hook = after_batch_sequence_snapshot_hook().lock().unwrap().clone();
    if let Some(hook) = hook {
        hook();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpLogEntry {
    pub seq: u64,
    pub timestamp_ms: u64,
    pub node_id: String,
    pub tenant_id: String,
    pub op_type: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpLogOrigin {
    pub timestamp_ms: u64,
    pub node_id: String,
    pub origin_seq: Option<u64>,
}

impl OpLogOrigin {
    pub fn new(timestamp_ms: u64, node_id: impl Into<String>) -> Self {
        Self {
            timestamp_ms,
            node_id: node_id.into(),
            origin_seq: None,
        }
    }

    pub fn with_origin_seq(mut self, origin_seq: u64) -> Self {
        self.origin_seq = Some(origin_seq);
        self
    }
}

#[derive(Debug, Clone)]
pub struct OpLogOperation {
    pub op_type: String,
    pub payload: serde_json::Value,
    pub origin: Option<OpLogOrigin>,
}

impl OpLogOperation {
    pub fn local(op_type: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            op_type: op_type.into(),
            payload,
            origin: None,
        }
    }

    pub fn replicated(
        op_type: impl Into<String>,
        payload: serde_json::Value,
        origin: OpLogOrigin,
    ) -> Self {
        Self {
            op_type: op_type.into(),
            payload,
            origin: Some(origin),
        }
    }
}

impl From<(String, serde_json::Value)> for OpLogOperation {
    fn from((op_type, payload): (String, serde_json::Value)) -> Self {
        Self::local(op_type, payload)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpLogReceipt {
    pub seq: u64,
    pub object_id: Option<String>,
    pub timestamp_ms: u64,
    pub node_id: String,
    pub is_tombstone: bool,
    pub origin_seq: Option<u64>,
    pub effect_digest: Option<[u8; 32]>,
}

const REPLICATION_EFFECT_DIGEST_DOMAIN: &[u8] = b"flapjack-replication-effect\0v1\0";

/// Digest the full accepted upsert body, including its canonical object ID and
/// any vectors, before internal task metadata is injected into the oplog row.
pub fn upsert_effect_digest(document: &crate::types::Document) -> [u8; 32] {
    digest_logical_effect(b"upsert", &document.to_json())
}

/// Digest a delete in a domain distinct from an upsert of the same object ID.
pub fn delete_effect_digest(object_id: &str) -> [u8; 32] {
    digest_logical_effect(b"delete", &serde_json::Value::String(object_id.to_string()))
}

fn digest_logical_effect(domain: &[u8], effect: &serde_json::Value) -> [u8; 32] {
    let canonical = crate::index::utils::canonicalize_json_value(effect);
    let bytes = serde_json::to_vec(&canonical)
        .expect("accepted logical document effects always serialize to JSON");
    let mut digest = Sha256::new();
    digest.update(REPLICATION_EFFECT_DIGEST_DOMAIN);
    digest.update(domain);
    digest.update([0]);
    digest.update(bytes);
    digest.finalize().into()
}

pub(crate) fn operation_effect_digest(
    op_type: &str,
    payload: &serde_json::Value,
) -> Option<[u8; 32]> {
    match op_type {
        "upsert" => payload
            .get("body")
            .and_then(|body| crate::types::Document::from_json(body).ok())
            .map(|document| upsert_effect_digest(&document)),
        "delete" => payload
            .get("objectID")
            .and_then(serde_json::Value::as_str)
            .map(delete_effect_digest),
        _ => None,
    }
}

/// Read the stable source sequence carried inside one document oplog payload.
///
/// This reserved metadata survives destination-local oplog renumbering without
/// expanding the public `OpLogEntry` wire structure. Presence with any JSON
/// type other than an unsigned integer is corrupt replication evidence.
pub fn replication_origin_seq(payload: &serde_json::Value) -> crate::error::Result<Option<u64>> {
    let Some(value) = payload.get(OPLOG_ORIGIN_SEQ_FIELD) else {
        return Ok(None);
    };
    value.as_u64().map(Some).ok_or_else(|| {
        crate::error::FlapjackError::Json(format!(
            "reserved {OPLOG_ORIGIN_SEQ_FIELD} must be an unsigned integer"
        ))
    })
}

fn insert_replication_origin_seq(
    payload: &mut serde_json::Value,
    origin_seq: u64,
) -> crate::error::Result<()> {
    let object = payload.as_object_mut().ok_or_else(|| {
        crate::error::FlapjackError::Json(
            "document oplog payload must be an object before origin metadata insertion".to_string(),
        )
    })?;
    object.insert(
        OPLOG_ORIGIN_SEQ_FIELD.to_string(),
        serde_json::Value::from(origin_seq),
    );
    Ok(())
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

/// Read and validate the durable committed sequence sidecar.
///
/// A missing sidecar is represented as `None` because a crash after the first
/// oplog append and before the first watermark write is a valid recovery state.
/// Existing but unreadable, non-regular, or malformed evidence fails closed.
pub(crate) fn read_checked_committed_seq(tenant_path: &Path) -> std::io::Result<Option<u64>> {
    let path = committed_seq_path(tenant_path);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let contents = std::fs::read_to_string(&path)?;
    let sequence = contents.trim().parse::<u64>().map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is not a u64 (got {:?}): {error}",
                path.display(),
                contents.trim()
            ),
        )
    })?;
    Ok(Some(sequence))
}

/// Read the durable committed sequence number for a tenant.
///
/// This compatibility reader intentionally maps missing or invalid evidence to
/// zero. Durability-sensitive owners must use [`read_checked_committed_seq`].
pub fn read_committed_seq(tenant_path: &Path) -> u64 {
    read_checked_committed_seq(tenant_path)
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// Strictly read every committed retained oplog entry once for legacy proof recovery.
pub(crate) fn retained_effect_entries(
    tenant_path: &Path,
) -> crate::error::Result<BTreeMap<u64, OpLogEntry>> {
    let oplog_dir = tenant_path.join(OPLOG_DIR);
    if !oplog_dir.is_dir() {
        return Ok(BTreeMap::new());
    }
    let Some(committed_seq) = read_checked_committed_seq(tenant_path)? else {
        return Ok(BTreeMap::new());
    };
    let mut retained = BTreeMap::new();
    for segment in sorted_segment_entries(&oplog_dir)? {
        for line in BufReader::new(File::open(segment.path())?).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: OpLogEntry = serde_json::from_str(&line)
                .map_err(|error| crate::error::FlapjackError::Json(error.to_string()))?;
            if entry.seq > committed_seq {
                continue;
            }
            let sequence = entry.seq;
            if retained.insert(sequence, entry).is_some() {
                return Err(crate::error::FlapjackError::Json(format!(
                    "duplicate retained oplog sequence {sequence}"
                )));
            }
        }
    }
    Ok(retained)
}

/// Persist the durable committed sequence number for a tenant.
pub fn write_committed_seq(tenant_path: &Path, seq: u64) -> std::io::Result<()> {
    let path = committed_seq_path(tenant_path);
    fs::create_dir_all(tenant_path)?;
    crate::index::utils::atomic_write(&path, seq.to_string().as_bytes())
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

    pub fn advance_current_seq_floor(&self, floor: u64) {
        let mut current = self.current_seq.load(Ordering::SeqCst);
        while current < floor {
            match self.current_seq.compare_exchange(
                current,
                floor,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(updated) => current = updated,
            }
        }
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
        self.append_operations_with_task_id(
            None,
            std::iter::once(OpLogOperation::local(op_type, payload)),
        )?
        .first()
        .map(|receipt| receipt.seq)
        .ok_or_else(|| crate::error::FlapjackError::Io("oplog append produced no receipt".into()))
    }

    /// Append multiple operations in a single lock acquisition and return the last assigned sequence number.
    ///
    /// All entries share the same timestamp. The segment is rotated after the batch if the size threshold is exceeded.
    ///
    /// # Arguments
    ///
    /// * `ops` - Slice of `(op_type, payload)` pairs to append.
    pub fn append_batch(&self, ops: &[(String, serde_json::Value)]) -> crate::error::Result<u64> {
        Ok(self
            .append_operations_with_task_id(None, ops.iter().cloned().map(Into::into))?
            .last()
            .map(|receipt| receipt.seq)
            .unwrap_or_else(|| self.current_seq.load(Ordering::SeqCst)))
    }

    pub fn append_batch_for_task(
        &self,
        task_id: &str,
        ops: &[(String, serde_json::Value)],
    ) -> crate::error::Result<Vec<OpLogReceipt>> {
        self.append_operations_with_task_id(Some(task_id), ops.iter().cloned().map(Into::into))
    }

    pub fn append_operations_for_task(
        &self,
        task_id: &str,
        ops: Vec<OpLogOperation>,
    ) -> crate::error::Result<Vec<OpLogReceipt>> {
        self.append_operations_with_task_id(Some(task_id), ops)
    }

    /// TODO: Document OpLog.append_batch_with_task_id.
    fn append_operations_with_task_id<I>(
        &self,
        task_id: Option<&str>,
        ops: I,
    ) -> crate::error::Result<Vec<OpLogReceipt>>
    where
        I: IntoIterator<Item = OpLogOperation>,
    {
        #[cfg(test)]
        run_after_batch_sequence_snapshot_hook_for_test();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let mut receipts = Vec::new();

        let mut seg = self.segment.lock().unwrap();
        let mut last_seq = self.current_seq.load(Ordering::SeqCst);
        for op in ops {
            last_seq += 1;
            let effect_digest = operation_effect_digest(&op.op_type, &op.payload);
            let mut payload = op.payload;
            let embedded_origin_seq = if effect_digest.is_some() {
                replication_origin_seq(&payload)?
            } else {
                None
            };
            if let (Some(task_id), Some(object)) = (task_id, payload.as_object_mut()) {
                object.insert(
                    OPLOG_TASK_ID_FIELD.to_string(),
                    serde_json::Value::String(task_id.to_string()),
                );
            }
            let (origin, origin_seq) = match op.origin {
                Some(origin) => {
                    let origin_seq = match (origin.origin_seq, embedded_origin_seq) {
                        (Some(explicit), Some(embedded)) if explicit != embedded => {
                            return Err(crate::error::FlapjackError::Json(format!(
                                "replicated oplog origin sequence {explicit} conflicts with embedded sequence {embedded}"
                            )));
                        }
                        (Some(explicit), _) => Some(explicit),
                        (None, embedded) => embedded,
                    };
                    (origin, origin_seq)
                }
                None => (
                    OpLogOrigin {
                        timestamp_ms: now,
                        node_id: self.node_id.clone(),
                        origin_seq: Some(last_seq),
                    },
                    Some(last_seq),
                ),
            };
            if let (Some(_), Some(origin_seq)) = (effect_digest, origin_seq) {
                insert_replication_origin_seq(&mut payload, origin_seq)?;
            }
            let object_id = payload_object_id(&payload).map(str::to_string);
            let is_tombstone = op.op_type == "delete";
            let entry = OpLogEntry {
                seq: last_seq,
                timestamp_ms: origin.timestamp_ms,
                node_id: origin.node_id.clone(),
                tenant_id: self.tenant_id.clone(),
                op_type: op.op_type,
                payload,
            };
            let line = serde_json::to_string(&entry)
                .map_err(|e| crate::error::FlapjackError::Io(e.to_string()))?;
            seg.writer.write_all(line.as_bytes())?;
            seg.writer.write_all(b"\n")?;
            seg.size += line.len() as u64 + 1;

            #[cfg(any(test, feature = "fault-injection"))]
            if let Err(injected_error) = crate::index::write_queue::inject_finalization_fault(
                &self.tenant_id,
                crate::index::write_queue::FinalizationFaultPoint::DuringOplogAppendAfterPartialDurableWrite,
            ) {
                // This point models EIO/ENOSPC after a task-tagged row reaches
                // durable storage but before current_seq learns about it.
                // Returning only after flush + sync preserves that exact replay
                // hazard without slowing ordinary fault-injection builds.
                seg.writer.flush()?;
                if task_id.is_some() {
                    seg.writer.get_ref().sync_all()?;
                }
                return Err(injected_error);
            }
            receipts.push(OpLogReceipt {
                seq: last_seq,
                object_id,
                timestamp_ms: origin.timestamp_ms,
                node_id: origin.node_id,
                is_tombstone,
                origin_seq,
                effect_digest,
            });
        }
        seg.writer.flush()?;
        if task_id.is_some() {
            seg.writer.get_ref().sync_all()?;
        }
        self.current_seq.fetch_max(last_seq, Ordering::SeqCst);

        if seg.size >= SEGMENT_MAX_BYTES {
            self.rotate_segment_locked(&mut seg)?;
        }

        Ok(receipts)
    }

    /// TODO: Document OpLog.committed_task_ids.
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

    /// Physically retract every durable entry with `seq >= from_seq`, removing
    /// the suffix from the segment files so a subsequent restart never replays
    /// it. This is the retraction primitive that closes DUR-1: when a batch's
    /// Tantivy commit fails, the oplog rows appended for that batch must be
    /// erased, not merely left un-acknowledged, or recovery would resurrect
    /// writes the client was told failed.
    ///
    /// Retraction keys on the sequence floor rather than on returned receipts,
    /// so a partially written batch whose receipts never reached the caller is
    /// also erased. Returns the number of entries removed and fails closed on
    /// any I/O error, leaving the caller to treat the batch as non-terminal.
    pub fn retract_from(&self, from_seq: u64) -> crate::error::Result<u64> {
        let mut segment = self.segment.lock().unwrap();
        segment.writer.flush().map_err(|error| {
            oplog_io_error(
                "flush active oplog segment before retraction",
                &segment.path,
                error,
            )
        })?;

        let outcome = retract_suffix_segments(&self.dir, from_seq)?;
        if outcome.changed {
            self.reopen_active_segment_after_retraction(&mut segment)?;
        }
        Ok(outcome.removed)
    }

    /// Retract only task-tagged entries for a failed write-queue batch, leaving
    /// unrelated synchronous metadata rows in the same sequence suffix intact.
    pub fn retract_tasks_from<'a, I>(&self, from_seq: u64, task_ids: I) -> crate::error::Result<u64>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let task_ids: BTreeSet<String> = task_ids.into_iter().map(str::to_string).collect();
        if task_ids.is_empty() {
            return Ok(0);
        }

        let mut segment = self.segment.lock().unwrap();
        segment.writer.flush().map_err(|error| {
            oplog_io_error(
                "flush active oplog segment before task retraction",
                &segment.path,
                error,
            )
        })?;

        let outcome = retract_task_segments(&self.dir, from_seq, &task_ids)?;
        if outcome.changed {
            self.reopen_active_segment_after_retraction(&mut segment)?;
        }
        Ok(outcome.removed)
    }

    fn reopen_active_segment_after_retraction(
        &self,
        segment: &mut ActiveSegment,
    ) -> crate::error::Result<()> {
        let (max_seq, max_segment_id) = Self::scan_existing(&self.dir)?;
        let active_segment_id = max_segment_id.max(1);
        let active_segment_path = self
            .dir
            .join(format!("segment_{active_segment_id:04}.jsonl"));
        let active_segment_size = active_segment_path
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&active_segment_path)?;
        *segment = ActiveSegment {
            writer: BufWriter::new(file),
            path: active_segment_path,
            size: active_segment_size,
            id: active_segment_id,
        };
        self.current_seq.store(max_seq, Ordering::SeqCst);
        Ok(())
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

    #[cfg(any(test, feature = "fault-injection"))]
    pub(crate) fn rotate_segment_for_test(&self) -> crate::error::Result<()> {
        let mut seg = self.segment.lock().unwrap();
        self.rotate_segment_locked(&mut seg)
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

pub(crate) fn payload_object_id(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get("objectID")
        .and_then(|value| value.as_str())
        .or_else(|| {
            payload
                .get("body")
                .and_then(|body| body.get("_id"))
                .and_then(|value| value.as_str())
        })
        .filter(|object_id| !object_id.is_empty())
}

pub(crate) fn payload_task_id(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get(OPLOG_TASK_ID_FIELD)
        .and_then(|value| value.as_str())
}

#[path = "oplog_retraction.rs"]
mod oplog_retraction;
use oplog_retraction::*;

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
    include!("oplog_tests.rs");
}

#[cfg(test)]
#[path = "oplog_receipt_tests.rs"]
mod receipt_tests;
