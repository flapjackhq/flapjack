//! Stub summary for engine/src/index/oplog.rs.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[cfg(test)]
type CommittedSnapshotHook = std::sync::Arc<dyn Fn() + Send + Sync>;
#[cfg(test)]
static COMMITTED_SNAPSHOT_AFTER_BOUND_HOOKS: once_cell::sync::Lazy<
    dashmap::DashMap<String, CommittedSnapshotHook>,
> = once_cell::sync::Lazy::new(dashmap::DashMap::new);

const SEGMENT_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub(crate) const OPLOG_DIR: &str = "oplog";
pub(crate) const COMMITTED_SEQ_FILE: &str = "committed_seq";
const OPLOG_TASK_ID_FIELD: &str = "_flapjack_task_id";
const OPLOG_ORIGIN_SEQ_FIELD: &str = "_flapjack_origin_seq";
const TASKLESS_APPEND_INTENT_FILE: &str = ".taskless_append_intent.json";
const TASKLESS_APPEND_COMPLETION_FILE: &str = ".taskless_append_completion.json";
const RETENTION_FLOOR_FILE: &str = ".retention_floor.json";
const TASKLESS_APPEND_INTENT_VERSION: u8 = 1;
const TASKLESS_APPEND_COMPLETION_VERSION: u8 = 2;
const RETENTION_FLOOR_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppendDurability {
    Buffered,
    Synced,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TasklessAppendIntent {
    version: u8,
    generation: u64,
    prior_seq: u64,
    segment_id: u32,
    segment_size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TasklessAppendCompletion {
    version: u8,
    generation: u64,
    #[serde(default)]
    resulting_seq: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RetentionFloor {
    version: u8,
    retained_through: u64,
}

enum TasklessRecoveryState {
    Healthy {
        last_generation: u64,
        sequence_floor: u64,
    },
    Unresolved(TasklessAppendIntent),
}

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
    recovery_failure: Option<String>,
}

pub struct OpLog {
    dir: PathBuf,
    tenant_id: String,
    node_id: String,
    current_seq: AtomicU64,
    segment_max_bytes: AtomicU64,
    segment: Mutex<ActiveSegment>,
    finalization_gate: std::sync::Arc<Semaphore>,
    recovery_required: std::sync::atomic::AtomicBool,
}

#[derive(Debug)]
pub struct CommittedOpLogSnapshot {
    pub ops: Vec<OpLogEntry>,
    pub current_seq: u64,
    pub oldest_retained_seq: Option<u64>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum MalformedRowPolicy {
    Skip,
    Reject,
}

#[cfg(test)]
pub(crate) struct CommittedSnapshotHookGuard {
    tenant_id: String,
}

#[cfg(test)]
impl Drop for CommittedSnapshotHookGuard {
    fn drop(&mut self) {
        COMMITTED_SNAPSHOT_AFTER_BOUND_HOOKS.remove(&self.tenant_id);
    }
}

/// Exclusive authority for allocating and publishing one oplog suffix.
///
/// Lock order is this permit before `segment` and all retraction/retention work.
/// Dropping an uncertified permit poisons ordinary acquisition until recovery
/// proves the retained tail safe.
pub(crate) struct OpLogFinalizationPermit {
    oplog: std::sync::Arc<OpLog>,
    _permit: OwnedSemaphorePermit,
    safe: bool,
    recovery: bool,
}

impl OpLogFinalizationPermit {
    pub(crate) fn advance_committed_seq(&self, requested: u64) -> crate::error::Result<u64> {
        if self.recovery {
            return Err(OpLog::recovery_required_error());
        }
        let tenant_path = self.oplog.dir.parent().ok_or_else(|| {
            crate::error::FlapjackError::Io("oplog has no tenant directory".to_string())
        })?;
        let existing = self.oplog.committed_seq()?.unwrap_or(0);
        let current = self.oplog.current_seq();
        if existing > current || requested > current {
            return Err(crate::error::FlapjackError::Io(format!(
                "committed watermark cannot advance beyond oplog tail {current} (existing={existing}, requested={requested})"
            )));
        }
        let committed = existing.max(requested);
        #[cfg(any(test, feature = "fault-injection"))]
        crate::index::write_queue::inject_finalization_fault(
            &self.oplog.tenant_id,
            crate::index::write_queue::FinalizationFaultPoint::BeforeCommittedSeqPublication,
        )?;
        write_committed_seq(tenant_path, committed)?;
        Ok(committed)
    }

    pub(crate) fn mark_safe(&mut self) {
        if self.recovery {
            return;
        }
        self.safe = true;
    }

    /// Withdraw an already granted certification once verification proved the
    /// retained suffix is not durably readable. Ordinary acquisition is poisoned
    /// immediately, and a taskless append is durably returned to the existing
    /// unmatched-intent recovery path so restart retracts the uncertified suffix.
    pub(crate) fn require_recovery(
        &mut self,
        taskless_sequence: Option<u64>,
    ) -> crate::error::Result<()> {
        self.safe = false;
        self.oplog.recovery_required.store(true, Ordering::Release);
        if let Some(sequence) = taskless_sequence {
            self.oplog.preserve_taskless_readback_failure(sequence)?;
        }
        Ok(())
    }

    /// Restore ordinary finalization only after recovery has made the durable
    /// watermark and readable oplog tail agree. Merely holding the recovery
    /// permit is not proof that a failed suffix was replayed or retracted.
    pub(crate) fn mark_recovered(&mut self) -> crate::error::Result<()> {
        if !self.recovery {
            return Err(crate::error::FlapjackError::Io(
                "ordinary finalization permit cannot certify recovery".to_string(),
            ));
        }
        let committed = self.oplog.committed_seq()?.unwrap_or(0);
        let current = self.oplog.current_seq();
        if current != committed {
            return Err(crate::error::FlapjackError::Io(format!(
                "oplog recovery is incomplete: tail={current}, committed_seq={committed}"
            )));
        }
        self.safe = true;
        self.oplog.recovery_required.store(false, Ordering::Release);
        Ok(())
    }
}

impl Drop for OpLogFinalizationPermit {
    fn drop(&mut self) {
        if !self.safe {
            self.oplog.recovery_required.store(true, Ordering::Release);
        }
    }
}

fn committed_seq_path(tenant_path: &Path) -> PathBuf {
    tenant_path.join(COMMITTED_SEQ_FILE)
}

fn read_regular_json<T: serde::de::DeserializeOwned>(path: &Path) -> std::io::Result<Option<T>> {
    let metadata = match fs::symlink_metadata(path) {
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
    serde_json::from_slice(&fs::read(path)?)
        .map(Some)
        .map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid recovery evidence at {}: {error}", path.display()),
            )
        })
}

fn taskless_recovery_state(oplog_dir: &Path) -> std::io::Result<TasklessRecoveryState> {
    let intent: Option<TasklessAppendIntent> =
        read_regular_json(&oplog_dir.join(TASKLESS_APPEND_INTENT_FILE))?;
    let completion: Option<TasklessAppendCompletion> =
        read_regular_json(&oplog_dir.join(TASKLESS_APPEND_COMPLETION_FILE))?;
    match (intent, completion) {
        (None, None) => Ok(TasklessRecoveryState::Healthy {
            last_generation: 0,
            sequence_floor: 0,
        }),
        (Some(intent), Some(completion))
            if intent.version == TASKLESS_APPEND_INTENT_VERSION
                && intent.generation == completion.generation =>
        {
            let sequence_floor =
                OpLog::validate_completed_recovery_evidence(oplog_dir, &intent, &completion)?;
            Ok(TasklessRecoveryState::Healthy {
                last_generation: intent.generation,
                sequence_floor,
            })
        }
        (Some(intent), None) if intent.version == TASKLESS_APPEND_INTENT_VERSION => {
            Ok(TasklessRecoveryState::Unresolved(intent))
        }
        (Some(intent), Some(completion))
            if intent.version == TASKLESS_APPEND_INTENT_VERSION
                && matches!(completion.version, 1 | TASKLESS_APPEND_COMPLETION_VERSION) =>
        {
            Ok(TasklessRecoveryState::Unresolved(intent))
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "inconsistent taskless append recovery evidence",
        )),
    }
}

fn write_recovery_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    let payload = serde_json::to_vec(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    crate::index::utils::atomic_write(path, &payload)
}

fn read_retention_floor(oplog_dir: &Path) -> std::io::Result<u64> {
    let Some(floor): Option<RetentionFloor> =
        read_regular_json(&oplog_dir.join(RETENTION_FLOOR_FILE))?
    else {
        return Ok(0);
    };
    if floor.version != RETENTION_FLOOR_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "unsupported oplog retention floor version {}",
                floor.version
            ),
        ));
    }
    Ok(floor.retained_through)
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
    if matches!(
        taskless_recovery_state(&tenant_path.join(OPLOG_DIR))?,
        TasklessRecoveryState::Unresolved(_)
    ) {
        return Err(std::io::Error::other(
            "unresolved taskless oplog append blocks committed watermark publication",
        ));
    }
    let path = committed_seq_path(tenant_path);
    fs::create_dir_all(tenant_path)?;
    crate::index::utils::atomic_write(&path, seq.to_string().as_bytes())
}

impl OpLog {
    fn preserve_taskless_readback_failure(&self, sequence: u64) -> crate::error::Result<()> {
        let intent_path = self.dir.join(TASKLESS_APPEND_INTENT_FILE);
        let completion_path = self.dir.join(TASKLESS_APPEND_COMPLETION_FILE);
        let intent: TasklessAppendIntent = read_regular_json(&intent_path)?.ok_or_else(|| {
            crate::error::FlapjackError::Io(
                "taskless readback failure has no durable append intent".into(),
            )
        })?;
        let completion: TasklessAppendCompletion = read_regular_json(&completion_path)?
            .ok_or_else(|| {
                crate::error::FlapjackError::Io(
                    "taskless readback failure has no durable append completion".into(),
                )
            })?;
        let appended_sequence = intent.prior_seq.checked_add(1);
        if intent.version != TASKLESS_APPEND_INTENT_VERSION
            || completion.version != TASKLESS_APPEND_COMPLETION_VERSION
            || completion.resulting_seq != Some(sequence)
            || appended_sequence != Some(sequence)
        {
            return Err(crate::error::FlapjackError::Io(
                "taskless readback failure does not match durable append evidence".into(),
            ));
        }
        if intent.generation == completion.generation {
            self.renew_taskless_intent_before_rollback(&intent)?;
        } else if intent.generation != completion.generation.saturating_add(1) {
            return Err(crate::error::FlapjackError::Io(
                "taskless readback recovery evidence has inconsistent generations".into(),
            ));
        }
        Ok(())
    }

    fn validate_completed_recovery_evidence(
        dir: &Path,
        intent: &TasklessAppendIntent,
        completion: &TasklessAppendCompletion,
    ) -> std::io::Result<u64> {
        let appended_seq = intent.prior_seq.checked_add(1);
        let resulting_seq = match (completion.version, completion.resulting_seq) {
            (1, None) => intent.prior_seq,
            (TASKLESS_APPEND_COMPLETION_VERSION, Some(resulting_seq))
                if resulting_seq == intent.prior_seq || Some(resulting_seq) == appended_seq =>
            {
                resulting_seq
            }
            _ => {
                return Err(Self::invalid_completed_recovery_evidence(
                    "invalid completion",
                ))
            }
        };
        let segment_path = Self::recovery_segment_path(dir, intent)?;
        match fs::symlink_metadata(&segment_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::validate_retired_recovery_boundary(dir, intent, resulting_seq)
            }
            _ => {
                Self::validate_recovery_boundary(dir, intent).map_err(|error| {
                    Self::invalid_completed_recovery_evidence(error.to_string())
                })?;
                if resulting_seq > intent.prior_seq
                    && !Self::contains_retained_sequence(dir, resulting_seq)?
                {
                    return Err(Self::invalid_completed_recovery_evidence(
                        "completed row is not retained",
                    ));
                }
                Ok(resulting_seq)
            }
        }
    }

    fn validate_retired_recovery_boundary(
        dir: &Path,
        intent: &TasklessAppendIntent,
        resulting_seq: u64,
    ) -> std::io::Result<u64> {
        let has_newer_segment = sorted_segment_entries(dir)?.iter().any(|entry| {
            segment_id_from_path(&entry.path()).is_some_and(|id| id > intent.segment_id)
        });
        if !has_newer_segment {
            return Err(Self::invalid_completed_recovery_evidence(
                "captured segment is missing without a newer retained segment",
            ));
        }
        let tenant_path = dir.parent().ok_or_else(|| {
            Self::invalid_completed_recovery_evidence("oplog has no tenant directory")
        })?;
        let committed_seq = read_checked_committed_seq(tenant_path)?.ok_or_else(|| {
            Self::invalid_completed_recovery_evidence(
                "captured segment is missing without a committed watermark",
            )
        })?;
        if committed_seq < resulting_seq {
            return Err(Self::invalid_completed_recovery_evidence(
                "captured segment was removed before its resulting sequence was committed",
            ));
        }
        Ok(committed_seq)
    }

    fn contains_retained_sequence(dir: &Path, sequence: u64) -> std::io::Result<bool> {
        for entry in sorted_segment_entries(dir)? {
            for line in BufReader::new(File::open(entry.path())?).lines() {
                let line = line?;
                if serde_json::from_str::<OpLogEntry>(&line)
                    .is_ok_and(|entry| entry.seq == sequence)
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn invalid_completed_recovery_evidence(message: impl std::fmt::Display) -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid completed taskless append recovery evidence: {message}"),
        )
    }

    fn recovery_segment_path(
        dir: &Path,
        intent: &TasklessAppendIntent,
    ) -> std::io::Result<PathBuf> {
        if intent.segment_id == 0 {
            return Err(Self::invalid_completed_recovery_evidence(
                "taskless append recovery intent has invalid segment id",
            ));
        }
        Ok(dir.join(format!("segment_{:04}.jsonl", intent.segment_id)))
    }

    fn validate_recovery_boundary(
        dir: &Path,
        intent: &TasklessAppendIntent,
    ) -> crate::error::Result<PathBuf> {
        if intent.segment_id == 0 {
            return Err(crate::error::FlapjackError::Io(
                "taskless append recovery intent has invalid segment id".into(),
            ));
        }
        let segment_path = dir.join(format!("segment_{:04}.jsonl", intent.segment_id));
        match fs::symlink_metadata(&segment_path) {
            Ok(metadata)
                if metadata.file_type().is_file() && metadata.len() >= intent.segment_size =>
            {
                Self::validate_recovery_prefix(&segment_path, intent)?;
                Ok(segment_path)
            }
            Ok(_) => Err(crate::error::FlapjackError::Io(
                "taskless append recovery boundary is not a valid segment position".into(),
            )),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && intent.segment_size == 0 =>
            {
                Ok(segment_path)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn validate_recovery_prefix(
        segment_path: &Path,
        intent: &TasklessAppendIntent,
    ) -> crate::error::Result<()> {
        let file = File::open(segment_path)?;
        let mut prefix = Vec::with_capacity(intent.segment_size as usize);
        file.take(intent.segment_size).read_to_end(&mut prefix)?;
        if prefix.len() as u64 != intent.segment_size
            || (!prefix.is_empty() && !prefix.ends_with(b"\n"))
        {
            return Err(crate::error::FlapjackError::Io(
                "taskless append recovery boundary splits an oplog row".into(),
            ));
        }
        for line in prefix
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let entry: OpLogEntry = serde_json::from_slice(line)
                .map_err(|error| crate::error::FlapjackError::Io(error.to_string()))?;
            if entry.seq > intent.prior_seq {
                return Err(crate::error::FlapjackError::Io(
                    "taskless append recovery boundary contains a newer sequence".into(),
                ));
            }
        }
        Ok(())
    }

    fn recover_unmatched_taskless_append(dir: &Path) -> crate::error::Result<(u64, u64)> {
        let intent = match taskless_recovery_state(dir)? {
            TasklessRecoveryState::Healthy {
                last_generation,
                sequence_floor,
            } => return Ok((last_generation, sequence_floor)),
            TasklessRecoveryState::Unresolved(intent) => intent,
        };
        Self::restore_taskless_append_boundary(dir, &intent)?;
        Ok((intent.generation, intent.prior_seq))
    }

    fn restore_taskless_append_boundary(
        dir: &Path,
        intent: &TasklessAppendIntent,
    ) -> crate::error::Result<()> {
        let segment_path = Self::validate_recovery_boundary(dir, intent)?;
        retract_suffix_segments(dir, intent.prior_seq.saturating_add(1))?;
        for entry in sorted_segment_entries(dir)? {
            let Some(segment_id) = segment_id_from_path(&entry.path()) else {
                continue;
            };
            if segment_id > intent.segment_id {
                remove_segment_file(&entry.path())?;
            }
        }
        match segment_path.metadata() {
            Ok(metadata) if metadata.len() >= intent.segment_size => {
                truncate_segment_file(&segment_path, intent.segment_size)?;
            }
            Ok(_) => {
                return Err(crate::error::FlapjackError::Io(
                    "taskless append recovery boundary exceeds segment length".into(),
                ));
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && intent.segment_size == 0 =>
            {
                File::create(&segment_path)?.sync_all()?;
                crate::index::utils::sync_parent_directory(&segment_path)?;
            }
            Err(error) => return Err(error.into()),
        }
        let (max_seq, max_segment_id) = Self::scan_existing(dir)?;
        if max_seq > intent.prior_seq || max_segment_id > intent.segment_id {
            return Err(crate::error::FlapjackError::Io(
                "taskless append recovery left an inconsistent oplog suffix".into(),
            ));
        }
        write_recovery_json(
            &dir.join(TASKLESS_APPEND_COMPLETION_FILE),
            &TasklessAppendCompletion {
                version: TASKLESS_APPEND_COMPLETION_VERSION,
                generation: intent.generation,
                resulting_seq: Some(intent.prior_seq),
            },
        )?;
        Ok(())
    }

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
        Self::open_with_missing_directory(dir, tenant_id, node_id, true)
    }

    pub(crate) fn open_existing(
        dir: &Path,
        tenant_id: &str,
        node_id: &str,
    ) -> crate::error::Result<Self> {
        Self::open_with_missing_directory(dir, tenant_id, node_id, false)
    }

    fn open_with_missing_directory(
        dir: &Path,
        tenant_id: &str,
        node_id: &str,
        create_missing: bool,
    ) -> crate::error::Result<Self> {
        if create_missing {
            fs::create_dir_all(dir)?;
            crate::index::utils::sync_parent_directory(dir)?;
        } else if !fs::metadata(dir)?.is_dir() {
            return Err(crate::error::FlapjackError::Io(format!(
                "oplog path is not a directory: {}",
                dir.display()
            )));
        }

        let (_, recovery_sequence_floor) = Self::recover_unmatched_taskless_append(dir)?;
        let retention_sequence_floor = read_retention_floor(dir)?;

        let (scanned_max_seq, max_seg_id) = Self::scan_existing(dir)?;
        // Retention may durably remove every committed segment. Its sidecar is
        // the affirmative proof that distinguishes that state from a watermark
        // advanced beyond bytes that were never written.
        let max_seq = scanned_max_seq
            .max(recovery_sequence_floor)
            .max(retention_sequence_floor);
        let tenant_path = dir.parent().ok_or_else(|| {
            crate::error::FlapjackError::Io("oplog has no tenant directory".to_string())
        })?;
        let committed_seq = read_checked_committed_seq(tenant_path)?;
        let recovery_required = match committed_seq {
            Some(committed_seq) => max_seq != committed_seq,
            None => max_seq > 0,
        };
        let next_seg_id = if max_seg_id > 0 { max_seg_id } else { 1 };
        let seg_path = dir.join(format!("segment_{:04}.jsonl", next_seg_id));
        let seg_size = seg_path.metadata().map(|m| m.len()).unwrap_or(0);

        let segment_existed = seg_path.exists();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&seg_path)?;
        if !segment_existed {
            file.sync_all()?;
            crate::index::utils::sync_parent_directory(&seg_path)?;
        }

        Ok(OpLog {
            dir: dir.to_path_buf(),
            tenant_id: tenant_id.to_string(),
            node_id: node_id.to_string(),
            current_seq: AtomicU64::new(max_seq),
            segment_max_bytes: AtomicU64::new(SEGMENT_MAX_BYTES),
            segment: Mutex::new(ActiveSegment {
                writer: BufWriter::new(file),
                path: seg_path,
                size: seg_size,
                id: next_seg_id,
                recovery_failure: None,
            }),
            finalization_gate: std::sync::Arc::new(Semaphore::new(1)),
            recovery_required: std::sync::atomic::AtomicBool::new(recovery_required),
        })
    }

    pub(crate) async fn acquire_finalization(
        self: &std::sync::Arc<Self>,
    ) -> crate::error::Result<OpLogFinalizationPermit> {
        if self.recovery_required.load(Ordering::Acquire) {
            return Err(Self::recovery_required_error());
        }
        let permit = self
            .finalization_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Self::recovery_required_error())?;
        if self.recovery_required.load(Ordering::Acquire) {
            return Err(Self::recovery_required_error());
        }
        Ok(OpLogFinalizationPermit {
            oplog: std::sync::Arc::clone(self),
            _permit: permit,
            safe: false,
            recovery: false,
        })
    }

    pub(crate) fn acquire_recovery(
        self: &std::sync::Arc<Self>,
    ) -> crate::error::Result<OpLogFinalizationPermit> {
        let permit = self
            .finalization_gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                crate::error::FlapjackError::Io("oplog finalization is still active".to_string())
            })?;
        Ok(OpLogFinalizationPermit {
            oplog: std::sync::Arc::clone(self),
            _permit: permit,
            safe: false,
            recovery: true,
        })
    }

    pub fn committed_seq(&self) -> crate::error::Result<Option<u64>> {
        let tenant_path = self.dir.parent().ok_or_else(|| {
            crate::error::FlapjackError::Io("oplog has no tenant directory".to_string())
        })?;
        Ok(read_checked_committed_seq(tenant_path)?)
    }

    fn recovery_required_error() -> crate::error::FlapjackError {
        crate::error::FlapjackError::Io(
            "oplog finalization requires durable recovery before new appends".to_string(),
        )
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

        for entry in entries {
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
            let f = File::open(entry.path())?;
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
        self.scan_entries_locked(0, MalformedRowPolicy::Skip)
            .ok()?
            .first()
            .map(|entry| entry.seq)
    }

    /// Append a single operation to the log and return its assigned sequence number.
    ///
    /// Atomically increments the sequence counter, serializes the entry as a JSON line, flushes to disk, and rotates the segment file when it exceeds `SEGMENT_MAX_BYTES`.
    ///
    /// # Arguments
    ///
    /// * `op_type` - Operation kind (e.g. `"upsert"`, `"delete"`).
    /// * `payload` - Arbitrary JSON payload for the operation.
    pub fn append(
        &self,
        op_type: &str,
        payload: serde_json::Value,
        durability: AppendDurability,
    ) -> crate::error::Result<u64> {
        self.append_operations_with_task_id(
            None,
            std::iter::once(OpLogOperation::local(op_type, payload)),
            durability,
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
            .append_operations_with_task_id(
                None,
                ops.iter().cloned().map(Into::into),
                AppendDurability::Buffered,
            )?
            .last()
            .map(|receipt| receipt.seq)
            .unwrap_or_else(|| self.current_seq.load(Ordering::SeqCst)))
    }

    pub fn append_batch_for_task(
        &self,
        task_id: &str,
        ops: &[(String, serde_json::Value)],
    ) -> crate::error::Result<Vec<OpLogReceipt>> {
        self.append_operations_with_task_id(
            Some(task_id),
            ops.iter().cloned().map(Into::into),
            AppendDurability::Synced,
        )
    }

    pub fn append_operations_for_task(
        &self,
        task_id: &str,
        ops: Vec<OpLogOperation>,
    ) -> crate::error::Result<Vec<OpLogReceipt>> {
        self.append_operations_with_task_id(Some(task_id), ops, AppendDurability::Synced)
    }

    /// TODO: Document OpLog.append_batch_with_task_id.
    fn append_operations_with_task_id<I>(
        &self,
        task_id: Option<&str>,
        ops: I,
        durability: AppendDurability,
    ) -> crate::error::Result<Vec<OpLogReceipt>>
    where
        I: IntoIterator<Item = OpLogOperation>,
    {
        #[cfg(test)]
        run_after_batch_sequence_snapshot_hook_for_test();
        let ops: Vec<_> = ops.into_iter().collect();
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let mut seg = self.segment.lock().unwrap();
        if let Some(error) = &seg.recovery_failure {
            return Err(crate::error::FlapjackError::Io(error.clone()));
        }
        let recovery = if task_id.is_none() && durability == AppendDurability::Synced {
            Some(self.begin_taskless_append(&mut seg)?)
        } else {
            None
        };
        let append_result = self.append_entries_locked(&mut seg, task_id, ops, now, durability);
        let (receipts, last_seq) = match append_result {
            Ok(result) => result,
            Err(error) => return self.finish_failed_append(&mut seg, recovery, error),
        };
        self.current_seq.store(last_seq, Ordering::SeqCst);

        if seg.size >= self.segment_max_bytes.load(Ordering::Relaxed) {
            if let Err(error) = self.rotate_segment_locked(&mut seg) {
                return self.finish_failed_append(&mut seg, recovery, error);
            }
        }
        if let Some(intent) = recovery {
            #[cfg(any(test, feature = "fault-injection"))]
            if let Err(error) = crate::index::write_queue::inject_finalization_fault(
                &self.tenant_id,
                crate::index::write_queue::FinalizationFaultPoint::AfterTasklessOplogRowSyncBeforeCompletion,
            ) {
                return self.finish_failed_append(&mut seg, Some(intent), error);
            }
            if let Err(error) = write_recovery_json(
                &self.dir.join(TASKLESS_APPEND_COMPLETION_FILE),
                &TasklessAppendCompletion {
                    version: TASKLESS_APPEND_COMPLETION_VERSION,
                    generation: intent.generation,
                    resulting_seq: Some(last_seq),
                },
            ) {
                return self.finish_failed_append(&mut seg, Some(intent), error.into());
            }
        }
        Ok(receipts)
    }

    fn append_entries_locked(
        &self,
        seg: &mut ActiveSegment,
        task_id: Option<&str>,
        ops: Vec<OpLogOperation>,
        now: u64,
        durability: AppendDurability,
    ) -> crate::error::Result<(Vec<OpLogReceipt>, u64)> {
        let mut receipts = Vec::with_capacity(ops.len());
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
            if task_id.is_none() && durability == AppendDurability::Synced {
                #[cfg(any(test, feature = "fault-injection"))]
                if crate::index::write_queue::finalization_fault_is_armed(
                    &self.tenant_id,
                    crate::index::write_queue::FinalizationFaultPoint::DuringTasklessOplogAppendAfterPartialWrite,
                ) {
                    seg.writer
                        .get_mut()
                        .write_all(&line.as_bytes()[..line.len() / 2])?;
                    seg.writer.get_ref().sync_all()?;
                    crate::index::write_queue::inject_finalization_fault(
                        &self.tenant_id,
                        crate::index::write_queue::FinalizationFaultPoint::DuringTasklessOplogAppendAfterPartialWrite,
                    )?;
                }
                seg.writer.get_mut().write_all(line.as_bytes())?;
                seg.writer.get_mut().write_all(b"\n")?;
            } else {
                seg.writer.write_all(line.as_bytes())?;
                seg.writer.write_all(b"\n")?;
            }
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
        #[cfg(any(test, feature = "fault-injection"))]
        if task_id.is_none() && durability == AppendDurability::Synced {
            crate::index::write_queue::inject_finalization_fault(
                &self.tenant_id,
                crate::index::write_queue::FinalizationFaultPoint::BeforeTasklessOplogRowSync,
            )?;
        }
        if durability == AppendDurability::Synced {
            seg.writer.get_ref().sync_all()?;
        }
        Ok((receipts, last_seq))
    }

    fn begin_taskless_append(
        &self,
        segment: &mut ActiveSegment,
    ) -> crate::error::Result<TasklessAppendIntent> {
        let last_generation = match taskless_recovery_state(&self.dir)? {
            TasklessRecoveryState::Healthy {
                last_generation, ..
            } => last_generation,
            TasklessRecoveryState::Unresolved(_) => {
                return Err(crate::error::FlapjackError::Io(
                    "unresolved taskless oplog append blocks another append".into(),
                ));
            }
        };
        segment.writer.flush()?;
        segment.writer.get_ref().sync_all()?;
        crate::index::utils::sync_parent_directory(&segment.path)?;
        let intent = TasklessAppendIntent {
            version: 1,
            generation: last_generation.saturating_add(1),
            prior_seq: self.current_seq.load(Ordering::SeqCst),
            segment_id: segment.id,
            segment_size: segment.writer.get_ref().metadata()?.len(),
        };
        #[cfg(any(test, feature = "fault-injection"))]
        crate::index::write_queue::inject_finalization_fault(
            &self.tenant_id,
            crate::index::write_queue::FinalizationFaultPoint::BeforeTasklessOplogIntentPersistence,
        )?;
        if let Err(error) =
            write_recovery_json(&self.dir.join(TASKLESS_APPEND_INTENT_FILE), &intent)
        {
            let recovery_failure = match taskless_recovery_state(&self.dir) {
                Ok(TasklessRecoveryState::Healthy { .. }) => None,
                Ok(TasklessRecoveryState::Unresolved(_)) => {
                    Some("unresolved taskless oplog append blocks further appends".to_string())
                }
                Err(evidence_error) => Some(format!(
                    "invalid taskless oplog recovery evidence: {evidence_error}"
                )),
            };
            segment.recovery_failure = recovery_failure;
            return Err(error.into());
        }
        Ok(intent)
    }

    fn finish_failed_append<T>(
        &self,
        segment: &mut ActiveSegment,
        recovery: Option<TasklessAppendIntent>,
        append_error: crate::error::FlapjackError,
    ) -> crate::error::Result<T> {
        let Some(intent) = recovery else {
            return Err(append_error);
        };
        let intent = match self.renew_taskless_intent_before_rollback(&intent) {
            Ok(intent) => intent,
            Err(evidence_error) => {
                let failure = format!(
                    "taskless oplog append failed ({append_error}); failed to preserve unresolved recovery evidence ({evidence_error})"
                );
                segment.recovery_failure = Some(failure.clone());
                return Err(crate::error::FlapjackError::Io(failure));
            }
        };
        #[cfg(any(test, feature = "fault-injection"))]
        let rollback_result = crate::index::write_queue::inject_finalization_fault(
            &self.tenant_id,
            crate::index::write_queue::FinalizationFaultPoint::DuringTasklessOplogRollback,
        )
        .and_then(|()| self.rollback_taskless_append(segment, &intent));
        #[cfg(not(any(test, feature = "fault-injection")))]
        let rollback_result = self.rollback_taskless_append(segment, &intent);
        if let Err(rollback_error) = rollback_result {
            let failure = format!(
                "taskless oplog append failed ({append_error}); durable rollback failed ({rollback_error})"
            );
            segment.recovery_failure = Some(failure.clone());
            return Err(crate::error::FlapjackError::Io(failure));
        }
        Err(append_error)
    }

    fn renew_taskless_intent_before_rollback(
        &self,
        intent: &TasklessAppendIntent,
    ) -> crate::error::Result<TasklessAppendIntent> {
        let mut renewed_intent = intent.clone();
        renewed_intent.generation = intent.generation.checked_add(1).ok_or_else(|| {
            crate::error::FlapjackError::Io(
                "taskless append recovery generation is exhausted".into(),
            )
        })?;
        write_recovery_json(&self.dir.join(TASKLESS_APPEND_INTENT_FILE), &renewed_intent)?;
        Ok(renewed_intent)
    }

    fn rollback_taskless_append(
        &self,
        segment: &mut ActiveSegment,
        intent: &TasklessAppendIntent,
    ) -> crate::error::Result<()> {
        Self::restore_taskless_append_boundary(&self.dir, intent)?;
        self.reopen_active_segment_after_retraction(segment)?;
        self.current_seq.store(intent.prior_seq, Ordering::SeqCst);
        Ok(())
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
        let segment_existed = active_segment_path.exists();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&active_segment_path)?;
        if !segment_existed {
            file.sync_all()?;
            crate::index::utils::sync_parent_directory(&active_segment_path)?;
        }
        *segment = ActiveSegment {
            writer: BufWriter::new(file),
            path: active_segment_path,
            size: active_segment_size,
            id: active_segment_id,
            recovery_failure: None,
        };
        self.current_seq.store(max_seq, Ordering::SeqCst);
        Ok(())
    }

    fn rotate_segment_locked(&self, seg: &mut ActiveSegment) -> crate::error::Result<()> {
        seg.writer.flush()?;
        let new_id = seg.id + 1;
        let new_path = self.dir.join(format!("segment_{new_id:04}.jsonl"));
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&new_path)?;
        file.sync_all()?;
        crate::index::utils::sync_parent_directory(&new_path)?;
        seg.writer = BufWriter::new(file);
        seg.path = new_path;
        seg.size = 0;
        seg.id = new_id;
        Ok(())
    }

    #[cfg(any(test, feature = "fault-injection"))]
    pub(crate) fn rotate_segment_for_test(&self) -> crate::error::Result<()> {
        let mut seg = self.segment.lock().unwrap();
        self.rotate_segment_locked(&mut seg)
    }

    #[cfg(test)]
    pub(crate) fn set_segment_max_bytes_for_test(&self, segment_max_bytes: u64) {
        self.segment_max_bytes
            .store(segment_max_bytes, Ordering::Relaxed);
    }

    /// Read all entries with a sequence number strictly greater than `since_seq`.
    ///
    /// Flushes the active writer before reading, scans every segment file in order, and returns results sorted by sequence number.
    pub fn read_since(&self, since_seq: u64) -> crate::error::Result<Vec<OpLogEntry>> {
        let mut segment = self.segment.lock().unwrap();
        segment.writer.flush()?;
        self.scan_entries_locked(since_seq, MalformedRowPolicy::Skip)
    }

    /// Capture and materialize one immutable committed prefix while segment
    /// replacement and retention are excluded by the segment lock.
    pub fn read_committed_since(
        &self,
        since_seq: u64,
    ) -> crate::error::Result<CommittedOpLogSnapshot> {
        let mut segment = self.segment.lock().unwrap();
        segment.writer.flush()?;
        let current_seq = self.committed_seq()?.unwrap_or(0).min(self.current_seq());
        #[cfg(test)]
        if let Some(hook) = COMMITTED_SNAPSHOT_AFTER_BOUND_HOOKS.get(&self.tenant_id) {
            let hook = std::sync::Arc::clone(hook.value());
            hook();
        }
        let retained = self.scan_entries_locked(0, MalformedRowPolicy::Reject)?;
        let oldest_retained_seq = match retained.first() {
            Some(entry) => Some(entry.seq),
            None if current_seq == 0 => None,
            // A fully retired committed prefix has no physical first row. Expose
            // the next available coordinate so catch-up consumers still detect
            // every request below the retention floor as a gap.
            None => Some(current_seq.checked_add(1).ok_or_else(|| {
                crate::error::FlapjackError::Io(
                    "oplog sequence space is exhausted after full retention".to_string(),
                )
            })?),
        };
        let ops = retained
            .into_iter()
            .filter(|entry| entry.seq > since_seq && entry.seq <= current_seq)
            .collect();
        Ok(CommittedOpLogSnapshot {
            ops,
            current_seq,
            oldest_retained_seq,
        })
    }

    #[cfg(test)]
    pub(crate) fn set_committed_snapshot_after_bound_hook_for_test(
        &self,
        hook: CommittedSnapshotHook,
    ) -> CommittedSnapshotHookGuard {
        COMMITTED_SNAPSHOT_AFTER_BOUND_HOOKS.insert(self.tenant_id.clone(), hook);
        CommittedSnapshotHookGuard {
            tenant_id: self.tenant_id.clone(),
        }
    }

    fn scan_entries_locked(
        &self,
        since_seq: u64,
        malformed_rows: MalformedRowPolicy,
    ) -> crate::error::Result<Vec<OpLogEntry>> {
        let mut results = Vec::new();
        for entry in sorted_segment_entries(&self.dir)? {
            let reader = BufReader::new(File::open(entry.path())?);
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let op = match serde_json::from_str::<OpLogEntry>(&line) {
                    Ok(op) => op,
                    Err(_) if malformed_rows == MalformedRowPolicy::Skip => continue,
                    Err(error) => return Err(error.into()),
                };
                if op.seq > since_seq {
                    results.push(op);
                }
            }
        }
        results.sort_by_key(|entry| entry.seq);
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
        let mut seg = self.segment.lock().unwrap();
        seg.writer.flush()?;
        let current_seg_name = seg.path.file_name().unwrap().to_str().unwrap().to_string();

        let entries = sorted_segment_entries(&self.dir)?;
        let mut removable = Vec::new();

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
                removable.push((entry.path(), max_seq_in_file));
            }
        }

        let Some(retained_through) = removable.iter().map(|(_, max_seq)| *max_seq).max() else {
            return Ok(0);
        };
        let retained_through = retained_through.max(read_retention_floor(&self.dir)?);
        write_recovery_json(
            &self.dir.join(RETENTION_FLOOR_FILE),
            &RetentionFloor {
                version: RETENTION_FLOOR_VERSION,
                retained_through,
            },
        )?;
        for (path, _) in &removable {
            remove_segment_file(path)?;
        }

        Ok(removable.len() as u64)
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

fn segment_id_from_path(path: &Path) -> Option<u32> {
    path.file_name()?
        .to_str()?
        .strip_prefix("segment_")?
        .strip_suffix(".jsonl")?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    include!("oplog_tests.rs");
}

#[cfg(test)]
#[path = "oplog_receipt_tests.rs"]
mod receipt_tests;
