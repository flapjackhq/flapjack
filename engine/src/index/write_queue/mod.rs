pub(crate) mod admission;
mod finalization;
mod vectors;

pub(crate) use finalization::PERSISTED_VECTORS_DIR;

use crate::types::{DocFailure, Document, TaskInfo, TaskStatus};
use admission::{reconcile_records, WriteAdmissionRecord, WriteAdmissionStore};
use once_cell::sync::Lazy;
use prometheus::{core::Collector, proto::MetricFamily, HistogramOpts, HistogramVec};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::timeout_at;

// Raised from 10 to amortize the dominant Tantivy commit fixed-cost over more
// queued ops per flush. Stage-3 multi_phase evidence (see
// docs/reference/research/pl10_write_bottleneck_20260528T033040Z_classification.md)
// shows commit_writer_with_panic_guard nested inside commit_batch consuming
// ~90% of in-batch wall time; WRITE_QUEUE_FLUSH_INTERVAL still caps tail
// latency and the resolved write queue channel capacity still gates QueueFull admission.
const DEFAULT_WRITE_QUEUE_BATCH_SIZE: usize = 32;
// Canonical runtime config key for write queue batching behavior.
const WRITE_QUEUE_BATCH_SIZE_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_BATCH_SIZE";
const DEFAULT_WRITER_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITER_ACQUIRE_TIMEOUT_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_WRITER_ACQUIRE_TIMEOUT_MS";
const WRITE_QUEUE_FLUSH_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_WRITE_QUEUE_CHANNEL_CAPACITY: usize = 2_000;
const WRITE_QUEUE_CHANNEL_CAPACITY_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_CHANNEL_CAPACITY";
const WRITE_QUEUE_START_DELAY_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_START_DELAY_MS";
const WRITE_QUEUE_PHASE_METRIC_NAME: &str = "flapjack_write_queue_phase_seconds";
const WRITE_QUEUE_PHASE_METRIC_HELP: &str = "Write queue phase execution time in seconds";

const PHASE_PROCESS_WRITES: &str = "process_writes";
const PHASE_FLUSH_PENDING_BATCH: &str = "flush_pending_batch";
const PHASE_COMMIT_BATCH: &str = "commit_batch";
pub(super) const PHASE_COMMIT_WRITER_WITH_PANIC_GUARD: &str = "commit_writer_with_panic_guard";
pub(super) const PHASE_FINALIZE_COMMITTED_BATCH: &str = "finalize_committed_batch";

static WRITE_QUEUE_PHASE_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    let histogram = HistogramVec::new(
        HistogramOpts::new(WRITE_QUEUE_PHASE_METRIC_NAME, WRITE_QUEUE_PHASE_METRIC_HELP),
        &["phase"],
    )
    .expect("write queue phase histogram should be constructible");
    for phase in [
        PHASE_PROCESS_WRITES,
        PHASE_FLUSH_PENDING_BATCH,
        PHASE_COMMIT_BATCH,
        PHASE_COMMIT_WRITER_WITH_PANIC_GUARD,
        PHASE_FINALIZE_COMMITTED_BATCH,
    ] {
        histogram.with_label_values(&[phase]);
    }
    histogram
});

pub(super) fn observe_write_queue_phase(phase: &str, started_at: Instant) {
    WRITE_QUEUE_PHASE_SECONDS
        .with_label_values(&[phase])
        .observe(started_at.elapsed().as_secs_f64());
}

fn write_queue_batch_size() -> usize {
    match std::env::var(WRITE_QUEUE_BATCH_SIZE_ENV_VAR) {
        Ok(raw_value) => match raw_value.parse::<usize>() {
            Ok(parsed) if parsed > 0 => parsed,
            Ok(_) => {
                tracing::warn!(
                    "{} must be greater than 0; falling back to default {}",
                    WRITE_QUEUE_BATCH_SIZE_ENV_VAR,
                    DEFAULT_WRITE_QUEUE_BATCH_SIZE
                );
                DEFAULT_WRITE_QUEUE_BATCH_SIZE
            }
            Err(error) => {
                tracing::warn!(
                    "failed to parse {}='{}' as usize: {}; falling back to default {}",
                    WRITE_QUEUE_BATCH_SIZE_ENV_VAR,
                    raw_value,
                    error,
                    DEFAULT_WRITE_QUEUE_BATCH_SIZE
                );
                DEFAULT_WRITE_QUEUE_BATCH_SIZE
            }
        },
        Err(_) => DEFAULT_WRITE_QUEUE_BATCH_SIZE,
    }
}

fn writer_acquire_timeout() -> Duration {
    match std::env::var(WRITER_ACQUIRE_TIMEOUT_ENV_VAR) {
        Ok(raw_value) => match raw_value.parse::<u64>() {
            Ok(parsed) if parsed > 0 => Duration::from_millis(parsed),
            Ok(_) => {
                tracing::warn!(
                    "{} must be greater than 0; falling back to {:?}",
                    WRITER_ACQUIRE_TIMEOUT_ENV_VAR,
                    DEFAULT_WRITER_ACQUIRE_TIMEOUT
                );
                DEFAULT_WRITER_ACQUIRE_TIMEOUT
            }
            Err(error) => {
                tracing::warn!(
                    "failed to parse {}='{}' as milliseconds: {}; falling back to {:?}",
                    WRITER_ACQUIRE_TIMEOUT_ENV_VAR,
                    raw_value,
                    error,
                    DEFAULT_WRITER_ACQUIRE_TIMEOUT
                );
                DEFAULT_WRITER_ACQUIRE_TIMEOUT
            }
        },
        Err(_) => DEFAULT_WRITER_ACQUIRE_TIMEOUT,
    }
}

fn write_queue_channel_capacity() -> usize {
    match std::env::var(WRITE_QUEUE_CHANNEL_CAPACITY_ENV_VAR) {
        Ok(raw_value) => match raw_value.parse::<usize>() {
            Ok(parsed) if parsed > 0 => parsed,
            Ok(_) => {
                tracing::warn!(
                    "{} must be greater than 0; falling back to default {}",
                    WRITE_QUEUE_CHANNEL_CAPACITY_ENV_VAR,
                    DEFAULT_WRITE_QUEUE_CHANNEL_CAPACITY
                );
                DEFAULT_WRITE_QUEUE_CHANNEL_CAPACITY
            }
            Err(error) => {
                tracing::warn!(
                    "failed to parse {}='{}' as usize: {}; falling back to default {}",
                    WRITE_QUEUE_CHANNEL_CAPACITY_ENV_VAR,
                    raw_value,
                    error,
                    DEFAULT_WRITE_QUEUE_CHANNEL_CAPACITY
                );
                DEFAULT_WRITE_QUEUE_CHANNEL_CAPACITY
            }
        },
        Err(_) => DEFAULT_WRITE_QUEUE_CHANNEL_CAPACITY,
    }
}

fn write_queue_start_delay() -> Option<Duration> {
    let raw_value = std::env::var(WRITE_QUEUE_START_DELAY_ENV_VAR).ok()?;
    match raw_value.parse::<u64>() {
        Ok(0) => None,
        Ok(parsed) => Some(Duration::from_millis(parsed)),
        Err(error) => {
            tracing::warn!(
                "failed to parse {}='{}' as milliseconds: {}; ignoring start delay",
                WRITE_QUEUE_START_DELAY_ENV_VAR,
                raw_value,
                error
            );
            None
        }
    }
}

pub fn gather_write_queue_phase_metric_families() -> Vec<MetricFamily> {
    WRITE_QUEUE_PHASE_SECONDS
        .collect()
        .into_iter()
        .filter(|family| !family.get_metric().is_empty())
        .collect()
}

/// Vector search context for the write queue.
/// When `vector-search` feature is disabled, this is a zero-sized type.
#[derive(Clone)]
pub(crate) struct VectorWriteContext {
    #[cfg(feature = "vector-search")]
    pub vector_indices:
        Arc<dashmap::DashMap<String, Arc<std::sync::RwLock<crate::vector::index::VectorIndex>>>>,
}

impl VectorWriteContext {
    #[cfg(feature = "vector-search")]
    pub fn new(
        vector_indices: Arc<
            dashmap::DashMap<String, Arc<std::sync::RwLock<crate::vector::index::VectorIndex>>>,
        >,
    ) -> Self {
        Self { vector_indices }
    }

    #[cfg(not(feature = "vector-search"))]
    pub fn new() -> Self {
        Self {}
    }
}

/// Shared context for write-queue lifecycle functions.
#[derive(Clone)]
pub(crate) struct WriteQueueContext {
    pub tenant_id: String,
    pub index: Arc<crate::index::Index>,
    pub _writers:
        Arc<dashmap::DashMap<String, Arc<tokio::sync::Mutex<crate::index::ManagedIndexWriter>>>>,
    pub tasks: Arc<dashmap::DashMap<String, TaskInfo>>,
    pub base_path: std::path::PathBuf,
    pub oplog: Option<Arc<crate::index::oplog::OpLog>>,
    pub admission_store: Arc<WriteAdmissionStore>,
    pub facet_cache: super::FacetCacheMap,
    pub lww_map: super::LwwMap,
    pub vector_ctx: VectorWriteContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WriteAction {
    Add(Document),
    Upsert(Document),
    /// Like Upsert but skips lww_map update — used by apply_ops_to_manager which
    /// has already recorded the correct op timestamp in lww_map before queuing.
    UpsertNoLwwUpdate(Document),
    Delete(String),
    /// Like Delete but skips lww_map update — same rationale as UpsertNoLwwUpdate.
    DeleteNoLwwUpdate(String),
    Compact,
}

#[derive(Debug, Clone)]
pub struct WriteOp {
    pub task_id: String,
    pub actions: Vec<WriteAction>,
}

pub type WriteQueue = mpsc::Sender<WriteOp>;

type PreparedWriteDocument = (String, serde_json::Value, tantivy::TantivyDocument);

struct PreparedWriteOperation {
    task_id: String,
    numeric_id: String,
    valid_docs: Vec<PreparedWriteDocument>,
    rejected: Vec<DocFailure>,
    deleted_ids: Vec<String>,
    primary_upsert_ids: Vec<String>,
    primary_delete_ids: Vec<String>,
    #[cfg(feature = "vector-search")]
    doc_vectors: Vec<Option<std::collections::HashMap<String, Vec<f32>>>>,
    #[cfg(feature = "vector-search")]
    vectors_modified: bool,
}

impl PreparedWriteOperation {
    fn new(task_id: String, numeric_id: String) -> Self {
        Self {
            task_id,
            numeric_id,
            valid_docs: Vec::new(),
            rejected: Vec::new(),
            deleted_ids: Vec::new(),
            primary_upsert_ids: Vec::new(),
            primary_delete_ids: Vec::new(),
            #[cfg(feature = "vector-search")]
            doc_vectors: Vec::new(),
            #[cfg(feature = "vector-search")]
            vectors_modified: false,
        }
    }

    fn indexed_document_count(&self) -> usize {
        self.valid_docs.len() + self.deleted_ids.len()
    }

    fn finalized_rejections(&self) -> (usize, Vec<DocFailure>) {
        let total_rejected = self.rejected.len();
        let mut rejected = self.rejected.clone();
        rejected.truncate(100);
        (total_rejected, rejected)
    }
}

#[derive(Clone, Copy)]
enum DocumentWriteMode {
    Add,
    PrimaryUpsert,
    ReplicatedUpsert,
}

impl DocumentWriteMode {
    fn deletes_existing(self) -> bool {
        matches!(self, Self::PrimaryUpsert | Self::ReplicatedUpsert)
    }

    fn tracks_primary(self) -> bool {
        matches!(self, Self::Add | Self::PrimaryUpsert)
    }
}

struct WritePreparationContext<'a> {
    index: &'a Arc<crate::index::Index>,
    settings: Option<&'a crate::index::settings::IndexSettings>,
    writer: &'a mut crate::index::ManagedIndexWriter,
    id_field: tantivy::schema::Field,
    #[cfg(feature = "vector-search")]
    embedder_configs: &'a [(String, crate::vector::config::EmbedderConfig)],
}

struct WriteFinalizationContext<'a> {
    tenant_id: &'a str,
    index: &'a Arc<crate::index::Index>,
    tasks: &'a Arc<dashmap::DashMap<String, TaskInfo>>,
    base_path: &'a std::path::Path,
    oplog: Option<&'a Arc<crate::index::oplog::OpLog>>,
    admission_store: &'a Arc<WriteAdmissionStore>,
    facet_cache: &'a super::FacetCacheMap,
    lww_map: &'a super::LwwMap,
    #[cfg(feature = "vector-search")]
    vector_ctx: &'a VectorWriteContext,
    #[cfg(feature = "vector-search")]
    embedder_configs: &'a [(String, crate::vector::config::EmbedderConfig)],
}

/// Spawn the background write-processing task for a tenant and return the channel sender and join handle.
///
/// # Arguments
///
/// * `tenant_id` - Tenant identifier used for logging, path resolution, and LWW map keying.
/// * `index` - Shared Tantivy index to write documents into.
/// * `writers` - Global writer registry (currently unused; writer is acquired per batch).
/// * `tasks` - Shared task-status map updated as operations are processed.
/// * `base_path` - Root data directory; tenant subdirectories contain settings, oplog, and vector files.
/// * `oplog` - Optional operation log for durable write-ahead recording.
/// * `facet_cache` - Shared facet cache invalidated after each commit.
/// * `lww_map` - Last-writer-wins map for primary write conflict resolution.
/// * `vector_ctx` - Vector index context for embedding and storing document vectors.
///
/// # Returns
///
/// A `(WriteQueue, JoinHandle)` tuple: the channel sender for submitting `WriteOp`s and the spawned task handle.
pub(crate) fn create_write_queue(
    ctx: WriteQueueContext,
) -> crate::error::Result<(
    WriteQueue,
    tokio::task::JoinHandle<crate::error::Result<()>>,
)> {
    let (tx, rx) = mpsc::channel(write_queue_channel_capacity());

    if let Some(ref ol) = ctx.oplog {
        tracing::info!(
            "[WQ {}] using shared oplog, seq={}",
            ctx.tenant_id,
            ol.current_seq()
        );
    }
    let committed_seq =
        crate::index::oplog::read_committed_seq(ctx.base_path.join(&ctx.tenant_id).as_path());
    let applied_task_ids = ctx
        .oplog
        .as_ref()
        .map(|oplog| oplog.committed_task_ids(committed_seq))
        .transpose()?
        .unwrap_or_default();
    let replay_records = reconcile_records(ctx.admission_store.as_ref(), &applied_task_ids)?;
    for record in &replay_records {
        let task = record.task_info();
        ctx.tasks.insert(task.id.clone(), task.clone());
        ctx.tasks.insert(task.numeric_id.to_string(), task);
    }
    run_replay_startup(&ctx, replay_records)?;

    let handle = tokio::spawn(async move { process_writes(ctx, rx, Vec::new()).await });

    Ok((tx, handle))
}

fn run_replay_startup(
    ctx: &WriteQueueContext,
    replay_records: Vec<WriteAdmissionRecord>,
) -> crate::error::Result<()> {
    if replay_records.is_empty() {
        return Ok(());
    }

    let ctx = ctx.clone();
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                crate::error::FlapjackError::Tantivy(format!(
                    "failed to create write replay runtime: {error}"
                ))
            })
            .and_then(|runtime| {
                runtime.block_on(async move {
                    let mut pending = Vec::new();
                    let resolved_batch_size = write_queue_batch_size();
                    replay_admitted_writes(&ctx, &mut pending, replay_records, resolved_batch_size)
                        .await?;
                    if !pending.is_empty() {
                        flush_pending_batch(&ctx, &mut pending).await?;
                    }
                    Ok(())
                })
            });
        let _ = result_tx.send(result);
    });

    let result = result_rx.recv().map_err(|error| {
        crate::error::FlapjackError::Tantivy(format!(
            "write replay startup result channel closed: {error}"
        ))
    })?;
    thread.join().map_err(|_| {
        crate::error::FlapjackError::Tantivy("write replay startup panicked".to_string())
    })?;
    result
}

fn configure_merge_policy(writer: &mut crate::index::ManagedIndexWriter) {
    // Reclaim disk space steadily when many tombstones accumulate.
    let mut merge_policy = tantivy::merge_policy::LogMergePolicy::default();
    merge_policy.set_del_docs_ratio_before_merge(0.3);
    writer.set_merge_policy(Box::new(merge_policy));
}

/// Try to acquire a writer slot, retrying on contention for up to 30 seconds.
///
/// Returns an error if the slot cannot be acquired within the deadline so the
/// queue can surface the failure instead of hanging indefinitely.
async fn acquire_writer_for_queue(
    index: &Arc<crate::index::Index>,
    tenant_id: &str,
) -> crate::error::Result<crate::index::ManagedIndexWriter> {
    const RETRY_INTERVAL: Duration = Duration::from_millis(5);
    let acquire_timeout = writer_acquire_timeout();
    let deadline = Instant::now() + acquire_timeout;
    let mut retries = 0usize;
    loop {
        match index.writer() {
            Ok(mut writer) => {
                configure_merge_policy(&mut writer);
                return Ok(writer);
            }
            Err(crate::error::FlapjackError::TooManyConcurrentWrites { current, max }) => {
                retries += 1;
                if Instant::now() >= deadline {
                    tracing::error!(
                        "[WQ {}] giving up after {} retries ({:?}) waiting for writer slot \
                         (active={}, max={})",
                        tenant_id,
                        retries,
                        acquire_timeout,
                        current,
                        max
                    );
                    return Err(crate::error::FlapjackError::TooManyConcurrentWrites {
                        current,
                        max,
                    });
                }
                if retries.is_multiple_of(200) {
                    tracing::warn!(
                        "[WQ {}] writer slot contention persists (active={}, max={}, retries={})",
                        tenant_id,
                        current,
                        max,
                        retries
                    );
                }
                tokio::time::sleep(RETRY_INTERVAL).await;
            }
            Err(e) => {
                tracing::error!("[WQ {}] failed to create writer: {}", tenant_id, e);
                return Err(e);
            }
        }
    }
}

/// Acquire a writer slot and commit all pending write operations in a single batch.
///
/// Drains `pending` and delegates to `commit_batch`. Returns early with `Ok(())` when `pending` is empty.
///
/// # Errors
///
/// Returns an error if the writer slot cannot be acquired within the retry deadline or if the batch commit fails.
async fn flush_pending_batch(
    ctx: &WriteQueueContext,
    pending: &mut Vec<WriteOp>,
) -> crate::error::Result<()> {
    let phase_start = Instant::now();
    if pending.is_empty() {
        observe_write_queue_phase(PHASE_FLUSH_PENDING_BATCH, phase_start);
        return Ok(());
    }
    let index = &ctx.index;
    let tenant_id = &ctx.tenant_id;
    let mut writer = match acquire_writer_for_queue(index, tenant_id).await {
        Ok(writer) => writer,
        Err(error) => {
            let pending_task_ids = pending
                .iter()
                .map(|op| op.task_id.clone())
                .collect::<Vec<_>>();
            finalization::mark_tasks_failed(&ctx.tasks, &pending_task_ids, &error);
            return Err(error);
        }
    };
    let result = commit_batch(ctx, pending, &mut writer).await;
    observe_write_queue_phase(PHASE_FLUSH_PENDING_BATCH, phase_start);
    result
}

/// Run the write-queue event loop: receive `WriteOp`s from the channel, batch them by count or timeout, and flush via `commit_batch`.
///
/// The loop flushes when the batch reaches the runtime `FLAPJACK_WRITE_QUEUE_BATCH_SIZE`
/// threshold, the 100 ms deadline expires, or the channel closes. Compact operations
/// are handled immediately after flushing any pending batch.
///
/// # Errors
///
/// Returns an error if writer acquisition or batch commit fails.
async fn process_writes(
    ctx: WriteQueueContext,
    mut rx: mpsc::Receiver<WriteOp>,
    replay_records: Vec<WriteAdmissionRecord>,
) -> crate::error::Result<()> {
    let phase_start = Instant::now();
    let tenant_id = &ctx.tenant_id;
    let resolved_batch_size = write_queue_batch_size();
    tracing::info!("Write queue started for tenant {}", tenant_id);
    tracing::info!(
        "[WQ {}] using resolved batch size {} from {}",
        tenant_id,
        resolved_batch_size,
        WRITE_QUEUE_BATCH_SIZE_ENV_VAR
    );
    if let Some(delay) = write_queue_start_delay() {
        tracing::warn!(
            "[WQ {}] delaying write queue start by {:?}",
            tenant_id,
            delay
        );
        tokio::time::sleep(delay).await;
    }
    let mut pending = Vec::new();
    let mut deadline = reset_write_queue_deadline();
    replay_admitted_writes(&ctx, &mut pending, replay_records, resolved_batch_size).await?;
    if !pending.is_empty() {
        flush_pending_batch(&ctx, &mut pending).await?;
    }

    loop {
        log_write_queue_state(tenant_id, pending.len(), deadline);
        match next_write_queue_event(deadline, &mut rx).await {
            WriteQueueEvent::Received(op) => {
                if handle_received_write_op(&ctx, &mut pending, op, resolved_batch_size).await? {
                    deadline = reset_write_queue_deadline();
                }
            }
            WriteQueueEvent::ChannelClosed => {
                flush_pending_on_channel_close(&ctx, &mut pending).await?;
                break;
            }
            WriteQueueEvent::DeadlineElapsed => {
                deadline = handle_write_queue_timeout(&ctx, &mut pending).await?;
            }
        }
    }
    observe_write_queue_phase(PHASE_PROCESS_WRITES, phase_start);
    Ok(())
}

async fn replay_admitted_writes(
    ctx: &WriteQueueContext,
    pending: &mut Vec<WriteOp>,
    replay_records: Vec<WriteAdmissionRecord>,
    resolved_batch_size: usize,
) -> crate::error::Result<()> {
    for record in replay_records {
        handle_received_write_op(ctx, pending, record.write_op(), resolved_batch_size).await?;
    }
    Ok(())
}

enum WriteQueueEvent {
    Received(WriteOp),
    ChannelClosed,
    DeadlineElapsed,
}

fn reset_write_queue_deadline() -> Instant {
    Instant::now() + WRITE_QUEUE_FLUSH_INTERVAL
}

fn log_write_queue_state(tenant_id: &str, pending_len: usize, deadline: Instant) {
    let deadline_in_ms = deadline
        .saturating_duration_since(Instant::now())
        .as_millis();
    if pending_len == 0 {
        tracing::trace!("[WQ {}] idle, deadline_in={}ms", tenant_id, deadline_in_ms);
    } else {
        tracing::debug!(
            "[WQ {}] waiting, pending={}, deadline_in={}ms",
            tenant_id,
            pending_len,
            deadline_in_ms
        );
    }
}

async fn next_write_queue_event(
    deadline: Instant,
    rx: &mut mpsc::Receiver<WriteOp>,
) -> WriteQueueEvent {
    match timeout_at(deadline.into(), rx.recv()).await {
        Ok(Some(op)) => WriteQueueEvent::Received(op),
        Ok(None) => WriteQueueEvent::ChannelClosed,
        Err(_timeout) => WriteQueueEvent::DeadlineElapsed,
    }
}

/// Route an incoming `WriteOp`: flush the pending batch and run compaction
/// immediately for `Compact` ops; otherwise buffer the op and flush when the
/// batch threshold (resolved from `FLAPJACK_WRITE_QUEUE_BATCH_SIZE`) is reached.
/// Returns `true` when a flush occurred and the deadline should be reset.
async fn handle_received_write_op(
    ctx: &WriteQueueContext,
    pending: &mut Vec<WriteOp>,
    op: WriteOp,
    resolved_batch_size: usize,
) -> crate::error::Result<bool> {
    let tenant_id = &ctx.tenant_id;
    let action_count = op.actions.len();
    let is_compact = matches!(op.actions.first(), Some(WriteAction::Compact));
    tracing::debug!(
        "[WQ {}] received op task={} actions={}{}",
        tenant_id,
        op.task_id,
        action_count,
        if is_compact { " (compact)" } else { "" }
    );

    if is_compact {
        flush_pending_batch(ctx, pending).await?;
        let mut writer = acquire_writer_for_queue(&ctx.index, tenant_id).await?;
        finalization::compact_segments(
            &ctx.index,
            &ctx.tasks,
            &op.task_id,
            &mut writer,
            tenant_id,
        )?;
        if let Err(error) = ctx.admission_store.remove_task(&op.task_id) {
            finalization::mark_tasks_failed(&ctx.tasks, std::slice::from_ref(&op.task_id), &error);
            return Err(error);
        }
        finalization::mark_compact_task_succeeded(&ctx.tasks, &op.task_id);
        return Ok(true);
    }

    pending.push(op);
    if !should_flush_pending_batch(pending.len(), resolved_batch_size) {
        return Ok(false);
    }

    tracing::debug!(
        "[WQ {}] batch threshold, committing {} ops",
        tenant_id,
        pending.len()
    );
    flush_pending_batch(ctx, pending).await?;
    Ok(true)
}

fn should_flush_pending_batch(pending_len: usize, resolved_batch_size: usize) -> bool {
    pending_len >= resolved_batch_size
}

async fn flush_pending_on_channel_close(
    ctx: &WriteQueueContext,
    pending: &mut Vec<WriteOp>,
) -> crate::error::Result<()> {
    if pending.is_empty() {
        return Ok(());
    }

    tracing::info!(
        "[WQ {}] channel closed, flushing {} pending",
        ctx.tenant_id,
        pending.len()
    );
    flush_pending_batch(ctx, pending).await
}

async fn handle_write_queue_timeout(
    ctx: &WriteQueueContext,
    pending: &mut Vec<WriteOp>,
) -> crate::error::Result<Instant> {
    if !pending.is_empty() {
        tracing::debug!(
            "[WQ {}] timeout, flushing {} pending",
            ctx.tenant_id,
            pending.len()
        );
        flush_pending_batch(ctx, pending).await?;
    }
    Ok(reset_write_queue_deadline())
}

/// Extract, validate, and strip `_vectors` from a document before Tantivy conversion.
/// Returns Ok(cleaned vectors) or Err(rejection failure).
/// Strips `_vectors` from `doc.fields` so Tantivy doesn't index large float arrays.
#[cfg(feature = "vector-search")]
fn process_doc_vectors(
    doc: &mut Document,
    doc_json: &serde_json::Value,
    embedder_configs: &[(String, crate::vector::config::EmbedderConfig)],
) -> Result<Option<std::collections::HashMap<String, Vec<f32>>>, DocFailure> {
    use crate::vector::vectors_field::{extract_vectors, strip_vectors_from_document};

    let extracted = match extract_vectors(doc_json) {
        Ok(vecs) => vecs,
        Err(e) => {
            return Err(DocFailure {
                doc_id: doc.id.clone(),
                error: "invalid_vectors".to_string(),
                message: e.to_string(),
            });
        }
    };

    let clean_vectors = if let Some(map) = extracted {
        let mut clean = std::collections::HashMap::new();
        for (emb_name, result) in map {
            // Only validate vectors for configured embedders
            if let Some((_, cfg)) = embedder_configs.iter().find(|(n, _)| n == &emb_name) {
                match result {
                    Err(e) => {
                        return Err(DocFailure {
                            doc_id: doc.id.clone(),
                            error: "invalid_vectors".to_string(),
                            message: format!("embedder '{}': {}", emb_name, e),
                        });
                    }
                    Ok(vec) => {
                        if let Some(expected) = cfg.dimensions {
                            if vec.len() != expected {
                                return Err(DocFailure {
                                    doc_id: doc.id.clone(),
                                    error: "dimension_mismatch".to_string(),
                                    message: format!(
                                        "embedder '{}': expected {} dimensions, got {}",
                                        emb_name,
                                        expected,
                                        vec.len()
                                    ),
                                });
                            }
                        }
                        clean.insert(emb_name, vec);
                    }
                }
            }
            // Vectors for unconfigured embedders are silently ignored
        }
        if clean.is_empty() {
            None
        } else {
            Some(clean)
        }
    } else {
        None
    };

    // Strip _vectors from doc.fields BEFORE to_tantivy
    strip_vectors_from_document(doc);

    Ok(clean_vectors)
}

/// Execute a batch of write operations against Tantivy: validate documents, strip and process `_vectors`, embed via configured embedders, update the VectorIndex, commit the Tantivy writer, persist vectors and fingerprint to disk, append to the oplog, invalidate caches, and update task status.
///
/// # Errors
///
/// Returns an error if the Tantivy commit fails or panics. Embedding failures are logged but do not block the Tantivy commit.
#[allow(unused_mut, unused_variables)]
async fn commit_batch(
    ctx: &WriteQueueContext,
    ops: &mut Vec<WriteOp>,
    writer: &mut crate::index::ManagedIndexWriter,
) -> crate::error::Result<()> {
    let phase_start = Instant::now();
    tracing::warn!(
        "[WQ {}] commit_batch: {} operations",
        ctx.tenant_id,
        ops.len()
    );
    #[cfg(not(feature = "vector-search"))]
    let _ = &ctx.vector_ctx;
    let batch_task_ids: Vec<String> = ops.iter().map(|op| op.task_id.clone()).collect();
    let settings = match load_write_settings(&ctx.base_path, &ctx.tenant_id) {
        Ok(settings) => settings,
        Err(error) => {
            finalization::mark_tasks_failed(&ctx.tasks, &batch_task_ids, &error);
            return Err(error);
        }
    };
    #[cfg(feature = "vector-search")]
    let embedder_configs = parse_embedder_configs(settings.as_ref(), &ctx.tenant_id);
    let finalization_context = WriteFinalizationContext {
        tenant_id: &ctx.tenant_id,
        index: &ctx.index,
        tasks: &ctx.tasks,
        base_path: ctx.base_path.as_path(),
        oplog: ctx.oplog.as_ref(),
        admission_store: &ctx.admission_store,
        facet_cache: &ctx.facet_cache,
        lww_map: &ctx.lww_map,
        #[cfg(feature = "vector-search")]
        vector_ctx: &ctx.vector_ctx,
        #[cfg(feature = "vector-search")]
        embedder_configs: &embedder_configs,
    };

    let mut prepared_ops = Vec::with_capacity(ops.len());
    let mut added_count = 0usize;
    let mut deleted_count = 0usize;
    let mut rejected_count = 0usize;

    for op in ops.drain(..) {
        // PL-10 saturation fix: stage every queued op into the same Tantivy
        // writer and commit once per queue flush. The previous loop committed
        // once per op, which turned a queue batch into many tiny disk commits.
        let prepared =
            match stage_write_op_for_commit(&finalization_context, settings.as_ref(), writer, op)
                .await
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    finalization::mark_tasks_failed(
                        finalization_context.tasks,
                        &batch_task_ids,
                        &error,
                    );
                    return Err(error);
                }
            };
        added_count += prepared.valid_docs.len();
        deleted_count += prepared.deleted_ids.len();
        rejected_count += prepared.rejected.len();
        prepared_ops.push(prepared);
    }

    let build_secs = match finalization::commit_writer_with_panic_guard(
        writer,
        ctx.tenant_id.as_str(),
        added_count,
        deleted_count,
        rejected_count,
    ) {
        Ok(build_secs) => build_secs,
        Err(error) => {
            finalization::mark_tasks_failed(finalization_context.tasks, &batch_task_ids, &error);
            return Err(error);
        }
    };
    if let Err(error) =
        finalization::finalize_committed_batch(&finalization_context, &prepared_ops, build_secs)
    {
        finalization::mark_tasks_failed(finalization_context.tasks, &batch_task_ids, &error);
        return Err(error);
    }
    if let Err(error) = finalization_context.admission_store.remove_tasks(
        prepared_ops
            .iter()
            .map(|prepared| prepared.task_id.as_str()),
    ) {
        finalization::mark_tasks_failed(finalization_context.tasks, &batch_task_ids, &error);
        return Err(error);
    }
    for prepared in &prepared_ops {
        finalization::mark_task_succeeded(finalization_context.tasks, prepared);
    }

    observe_write_queue_phase(PHASE_COMMIT_BATCH, phase_start);
    Ok(())
}

fn load_write_settings(
    base_path: &std::path::Path,
    tenant_id: &str,
) -> crate::error::Result<Option<crate::index::settings::IndexSettings>> {
    let settings_path = base_path.join(tenant_id).join("settings.json");
    if settings_path.exists() {
        Ok(Some(crate::index::settings::IndexSettings::load(
            &settings_path,
        )?))
    } else {
        Ok(None)
    }
}

/// Deserialize embedder configurations from index settings JSON, skipping null
/// or malformed entries with a warning log.
#[cfg(feature = "vector-search")]
fn parse_embedder_configs(
    settings: Option<&crate::index::settings::IndexSettings>,
    tenant_id: &str,
) -> Vec<(String, crate::vector::config::EmbedderConfig)> {
    settings
        .and_then(|settings| settings.embedders.as_ref())
        .map(|embedder_map| {
            embedder_map
                .iter()
                .filter_map(|(name, json)| {
                    if json.is_null() {
                        return None;
                    }
                    match serde_json::from_value::<crate::vector::config::EmbedderConfig>(
                        json.clone(),
                    ) {
                        Ok(config) => Some((name.clone(), config)),
                        Err(error) => {
                            tracing::warn!(
                                "[WQ {}] failed to parse embedder '{}': {}",
                                tenant_id,
                                name,
                                error
                            );
                            None
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Process one `WriteOp` end-to-end: mark the task as processing, prepare
/// documents and deletes, embed vectors, write to Tantivy, commit, run
/// post-commit finalization (oplog, caches, LWW, vectors), and mark succeeded.
async fn stage_write_op_for_commit(
    context: &WriteFinalizationContext<'_>,
    settings: Option<&crate::index::settings::IndexSettings>,
    writer: &mut crate::index::ManagedIndexWriter,
    op: WriteOp,
) -> crate::error::Result<PreparedWriteOperation> {
    let numeric_id = mark_task_processing(context.tasks, &op.task_id);
    let id_field = context.index.inner().schema().get_field("_id").unwrap();
    let mut prepared = PreparedWriteOperation::new(op.task_id, numeric_id);
    {
        let mut preparation_context = WritePreparationContext {
            index: context.index,
            settings,
            writer,
            id_field,
            #[cfg(feature = "vector-search")]
            embedder_configs: context.embedder_configs,
        };
        prepare_write_actions(&mut preparation_context, &mut prepared, op.actions)?;
    }
    #[cfg(feature = "vector-search")]
    vectors::process_vectors_for_write_op(context, &mut prepared).await;

    let valid_docs_json = finalization::write_valid_documents(writer, &prepared.valid_docs)?;
    finalization::append_batch_to_oplog(
        context.oplog,
        &prepared.task_id,
        &valid_docs_json,
        &prepared.deleted_ids,
        context.tenant_id,
    )?;
    Ok(prepared)
}

fn mark_task_processing(tasks: &Arc<dashmap::DashMap<String, TaskInfo>>, task_id: &str) -> String {
    let numeric_id = tasks
        .get(task_id)
        .map(|task| task.numeric_id.to_string())
        .unwrap_or_else(|| task_id.to_string());
    tasks.alter(task_id, |_, mut task| {
        task.status = TaskStatus::Processing;
        task
    });
    numeric_id
}

/// Dispatch each `WriteAction` to the appropriate handler: delete (with or
/// without LWW tracking), add, upsert, or replicated-upsert document
/// preparation.
fn prepare_write_actions(
    preparation_context: &mut WritePreparationContext<'_>,
    prepared: &mut PreparedWriteOperation,
    actions: Vec<WriteAction>,
) -> crate::error::Result<()> {
    for action in actions {
        match action {
            WriteAction::Delete(object_id) => {
                prepare_delete_action(
                    prepared,
                    preparation_context.writer,
                    preparation_context.id_field,
                    object_id,
                    true,
                );
            }
            WriteAction::DeleteNoLwwUpdate(object_id) => {
                prepare_delete_action(
                    prepared,
                    preparation_context.writer,
                    preparation_context.id_field,
                    object_id,
                    false,
                );
            }
            WriteAction::Add(doc) => {
                prepare_document_write(preparation_context, prepared, doc, DocumentWriteMode::Add);
            }
            WriteAction::Upsert(doc) => {
                prepare_document_write(
                    preparation_context,
                    prepared,
                    doc,
                    DocumentWriteMode::PrimaryUpsert,
                );
            }
            WriteAction::UpsertNoLwwUpdate(doc) => {
                prepare_document_write(
                    preparation_context,
                    prepared,
                    doc,
                    DocumentWriteMode::ReplicatedUpsert,
                );
            }
            WriteAction::Compact => {}
        }
    }
    Ok(())
}

fn prepare_delete_action(
    prepared: &mut PreparedWriteOperation,
    writer: &mut crate::index::ManagedIndexWriter,
    id_field: tantivy::schema::Field,
    object_id: String,
    track_primary_delete: bool,
) {
    writer.delete_term(tantivy::Term::from_field_text(id_field, &object_id));
    if track_primary_delete {
        prepared.primary_delete_ids.push(object_id.clone());
    }
    prepared.deleted_ids.push(object_id);
}

/// Validate a document (size limit, vector schema, Tantivy conversion), strip
/// `_vectors`, delete the existing term on upsert, and push to the prepared
/// batch or reject list.
fn prepare_document_write(
    preparation_context: &mut WritePreparationContext<'_>,
    prepared: &mut PreparedWriteOperation,
    doc: Document,
    document_write_mode: DocumentWriteMode,
) {
    #[allow(unused_mut)]
    let mut doc = doc;
    let doc_json = doc.to_json();
    #[cfg(feature = "vector-search")]
    let vectors =
        match process_doc_vectors(&mut doc, &doc_json, preparation_context.embedder_configs) {
            Ok(vectors) => vectors,
            Err(failure) => {
                prepared.rejected.push(failure);
                return;
            }
        };

    let doc_id = doc.id.clone();
    let estimated_size = serde_json::to_string(&doc_json)
        .map(|json| json.len())
        .unwrap_or(0);
    if let Err(error) = preparation_context
        .index
        .memory_budget()
        .validate_document_size(estimated_size)
    {
        prepared.rejected.push(DocFailure {
            doc_id,
            error: classify_error(&error),
            message: error.to_string(),
        });
        return;
    }

    if document_write_mode.deletes_existing() {
        preparation_context
            .writer
            .delete_term(tantivy::Term::from_field_text(
                preparation_context.id_field,
                &doc.id,
            ));
    }

    match preparation_context
        .index
        .converter()
        .to_tantivy(&doc, preparation_context.settings)
    {
        Ok(tantivy_doc) => {
            if document_write_mode.tracks_primary() {
                prepared.primary_upsert_ids.push(doc.id.clone());
            }
            prepared
                .valid_docs
                .push((doc.id.clone(), doc_json, tantivy_doc));
            #[cfg(feature = "vector-search")]
            prepared.doc_vectors.push(vectors);
        }
        Err(error) => {
            prepared.rejected.push(DocFailure {
                doc_id: doc.id,
                error: classify_error(&error),
                message: error.to_string(),
            });
        }
    }
}

fn classify_error(e: &crate::error::FlapjackError) -> String {
    match e {
        crate::error::FlapjackError::FieldNotFound(_) => "field_not_found".to_string(),
        crate::error::FlapjackError::TypeMismatch { .. } => "type_mismatch".to_string(),
        crate::error::FlapjackError::MissingField(_) => "missing_field".to_string(),
        crate::error::FlapjackError::DocumentTooLarge { .. } => "document_too_large".to_string(),
        _ => "validation_error".to_string(),
    }
}

#[cfg(test)]
#[path = "../write_queue_tests.rs"]
mod tests;
