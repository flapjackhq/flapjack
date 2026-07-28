//! Durable per-tenant write admission, batching, commit, and lifecycle ownership.
//!
//! Each tenant worker owns at most one live [`ManagedIndexWriter`](crate::index::ManagedIndexWriter)
//! in its processing loop. The writer is opened lazily, reused across commits, and closed only
//! through `writer_lifecycle`, which also owns memory-budget release and close telemetry. Segment
//! and file-state reporting comes from `segment_observation`; commit/finalization code consumes
//! that observer instead of maintaining a second view of Tantivy state.
pub(crate) mod admission;
#[cfg(test)]
mod admission_tests;
pub(crate) mod backpressure;
mod finalization;
pub(crate) mod segment_observation;
mod vectors;
mod writer_lifecycle;

#[cfg(test)]
pub(crate) use finalization::fail_next_commit_for_test;
pub(crate) use finalization::PERSISTED_VECTORS_DIR;

use crate::types::{DocFailure, Document, TaskInfo, TaskStatus};
use admission::{reconcile_records, WriteAdmissionRecord, WriteAdmissionStore};
use once_cell::sync::Lazy;
use prometheus::{
    core::Collector, proto::MetricFamily, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec,
    Opts,
};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::cell::RefCell;
use std::collections::BTreeSet;
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
const WRITE_QUEUE_MIN_MERGE_SEGMENTS_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_MIN_MERGE_SEGMENTS";
const WRITE_QUEUE_MAX_DOCS_BEFORE_MERGE_ENV_VAR: &str =
    "FLAPJACK_WRITE_QUEUE_MAX_DOCS_BEFORE_MERGE";
const WRITE_QUEUE_FLUSH_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_WRITE_QUEUE_CHANNEL_CAPACITY: usize = 512;
const WRITE_QUEUE_CHANNEL_CAPACITY_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_CHANNEL_CAPACITY";
const WRITE_QUEUE_START_DELAY_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_START_DELAY_MS";
pub(crate) const SELECTED_MERGE_POLICY_MIN_NUM_SEGMENTS: usize = 8;
pub(crate) const SELECTED_MERGE_POLICY_MAX_DOCS_BEFORE_MERGE: usize = 10_000_000;
pub(crate) const SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND: (usize, usize) = (2, 4);
const WRITE_QUEUE_PHASE_METRIC_NAME: &str = "flapjack_write_queue_phase_seconds";
const WRITE_QUEUE_PHASE_METRIC_HELP: &str = "Write queue phase execution time in seconds";
const WRITE_QUEUE_WRITER_OPENS_METRIC_NAME: &str = "flapjack_write_queue_writer_opens_total";
const WRITE_QUEUE_COMMITS_METRIC_NAME: &str = "flapjack_write_queue_commits_total";
const WRITE_QUEUE_WRITER_CLOSES_METRIC_NAME: &str = "flapjack_write_queue_writer_closes_total";
const WRITE_QUEUE_LIVE_SEGMENTS_METRIC_NAME: &str = "flapjack_write_queue_live_segments";
const WRITE_QUEUE_LIVE_DOCS_METRIC_NAME: &str = "flapjack_write_queue_live_docs";
const WRITE_QUEUE_DOCUMENTS_PER_SEGMENT_METRIC_NAME: &str =
    "flapjack_write_queue_documents_per_segment";
const WRITE_QUEUE_INDEX_FILES_METRIC_NAME: &str = "flapjack_write_queue_index_files";
const WRITE_QUEUE_INDEX_BYTES_METRIC_NAME: &str = "flapjack_write_queue_index_bytes";
const WRITE_QUEUE_ORPHAN_FILE_SETS_METRIC_NAME: &str = "flapjack_write_queue_orphan_file_sets";
const WRITE_QUEUE_WRITER_LIFETIME_METRIC_NAME: &str =
    "flapjack_write_queue_writer_lifetime_seconds";
const WRITE_QUEUE_WRITER_MERGE_WAIT_METRIC_NAME: &str =
    "flapjack_write_queue_writer_merge_wait_seconds";
const WRITE_QUEUE_GC_REMOVED_FILES_METRIC_NAME: &str =
    "flapjack_write_queue_gc_removed_files_total";
const WRITE_QUEUE_SETTLED_INDEX_BYTES_METRIC_NAME: &str =
    "flapjack_write_queue_settled_index_bytes";

const PHASE_PROCESS_WRITES: &str = "process_writes";
const PHASE_FLUSH_PENDING_BATCH: &str = "flush_pending_batch";
const PHASE_COMMIT_BATCH: &str = "commit_batch";
pub(super) const PHASE_COMMIT_WRITER_WITH_PANIC_GUARD: &str = "commit_writer_with_panic_guard";
pub(super) const PHASE_FINALIZE_COMMITTED_BATCH: &str = "finalize_committed_batch";
const PHASE_DOCUMENT_CONVERSION: &str = "document_conversion";
const PHASE_DELETE_STAGING: &str = "delete_staging";
const PHASE_ADD_STAGING: &str = "add_staging";
pub(super) const PHASE_WRITER_COMMIT: &str = "writer_commit";
pub(super) const PHASE_READER_RELOAD: &str = "reader_reload";
pub(super) const PHASE_METADATA_PERSISTENCE: &str = "metadata_persistence";
pub(super) const PHASE_LWW_UPDATE: &str = "lww_update";
pub(super) const PHASE_OPLOG_APPEND: &str = "oplog_append";
pub(super) const PHASE_OPLOG_COMMIT_STATE_PERSISTENCE: &str = "oplog_commit_state_persistence";
#[cfg(feature = "vector-search")]
pub(super) const PHASE_VECTOR_SAVE: &str = "vector_save";

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
        PHASE_DOCUMENT_CONVERSION,
        PHASE_DELETE_STAGING,
        PHASE_ADD_STAGING,
        PHASE_WRITER_COMMIT,
        PHASE_READER_RELOAD,
        PHASE_METADATA_PERSISTENCE,
        PHASE_LWW_UPDATE,
        PHASE_OPLOG_APPEND,
        PHASE_OPLOG_COMMIT_STATE_PERSISTENCE,
        #[cfg(feature = "vector-search")]
        PHASE_VECTOR_SAVE,
    ] {
        histogram.with_label_values(&[phase]);
    }
    histogram
});

static WRITE_QUEUE_WRITER_OPENS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    IntCounterVec::new(
        Opts::new(
            WRITE_QUEUE_WRITER_OPENS_METRIC_NAME,
            "Total write queue writer opens by tenant",
        ),
        &["tenant"],
    )
    .expect("write queue writer-open counter should be constructible")
});

static WRITE_QUEUE_COMMITS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    IntCounterVec::new(
        Opts::new(
            WRITE_QUEUE_COMMITS_METRIC_NAME,
            "Total successful write queue commits by tenant",
        ),
        &["tenant"],
    )
    .expect("write queue commit counter should be constructible")
});

static WRITE_QUEUE_WRITER_CLOSES_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    IntCounterVec::new(
        Opts::new(
            WRITE_QUEUE_WRITER_CLOSES_METRIC_NAME,
            "Total write queue writer closes by tenant and reason",
        ),
        &["tenant", "reason"],
    )
    .expect("write queue writer-close counter should be constructible")
});

static WRITE_QUEUE_LIVE_SEGMENTS: Lazy<IntGaugeVec> = Lazy::new(|| {
    IntGaugeVec::new(
        Opts::new(
            WRITE_QUEUE_LIVE_SEGMENTS_METRIC_NAME,
            "Current live searchable segments by tenant",
        ),
        &["tenant"],
    )
    .expect("write queue live-segment gauge should be constructible")
});

static WRITE_QUEUE_LIVE_DOCS: Lazy<IntGaugeVec> = Lazy::new(|| {
    IntGaugeVec::new(
        Opts::new(
            WRITE_QUEUE_LIVE_DOCS_METRIC_NAME,
            "Current live searchable documents by tenant",
        ),
        &["tenant"],
    )
    .expect("write queue live-doc gauge should be constructible")
});

static WRITE_QUEUE_DOCUMENTS_PER_SEGMENT: Lazy<IntGaugeVec> = Lazy::new(|| {
    IntGaugeVec::new(
        Opts::new(
            WRITE_QUEUE_DOCUMENTS_PER_SEGMENT_METRIC_NAME,
            "Current live searchable documents by tenant and segment",
        ),
        &["tenant", "segment"],
    )
    .expect("write queue documents-per-segment gauge should be constructible")
});

static WRITE_QUEUE_INDEX_FILES: Lazy<IntGaugeVec> = Lazy::new(|| {
    IntGaugeVec::new(
        Opts::new(
            WRITE_QUEUE_INDEX_FILES_METRIC_NAME,
            "Current managed index file count by tenant",
        ),
        &["tenant"],
    )
    .expect("write queue index-file gauge should be constructible")
});

static WRITE_QUEUE_INDEX_BYTES: Lazy<IntGaugeVec> = Lazy::new(|| {
    IntGaugeVec::new(
        Opts::new(
            WRITE_QUEUE_INDEX_BYTES_METRIC_NAME,
            "Current index directory bytes by tenant",
        ),
        &["tenant"],
    )
    .expect("write queue index-byte gauge should be constructible")
});

static WRITE_QUEUE_ORPHAN_FILE_SETS: Lazy<IntGaugeVec> = Lazy::new(|| {
    IntGaugeVec::new(
        Opts::new(
            WRITE_QUEUE_ORPHAN_FILE_SETS_METRIC_NAME,
            "Current stale or orphan segment file-set count by tenant",
        ),
        &["tenant"],
    )
    .expect("write queue orphan-file-set gauge should be constructible")
});

static WRITE_QUEUE_WRITER_LIFETIME_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    HistogramVec::new(
        HistogramOpts::new(
            WRITE_QUEUE_WRITER_LIFETIME_METRIC_NAME,
            "Write queue writer lifetime in seconds by tenant",
        ),
        &["tenant"],
    )
    .expect("write queue writer-lifetime histogram should be constructible")
});

static WRITE_QUEUE_WRITER_MERGE_WAIT_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    HistogramVec::new(
        HistogramOpts::new(
            WRITE_QUEUE_WRITER_MERGE_WAIT_METRIC_NAME,
            "Write queue writer close merge wait in seconds by tenant and close reason",
        ),
        &["tenant", "reason"],
    )
    .expect("write queue writer-merge-wait histogram should be constructible")
});

static WRITE_QUEUE_GC_REMOVED_FILES_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    IntCounterVec::new(
        Opts::new(
            WRITE_QUEUE_GC_REMOVED_FILES_METRIC_NAME,
            "Total index files removed by write-queue garbage collection",
        ),
        &["tenant"],
    )
    .expect("write queue gc-removed-files counter should be constructible")
});

static WRITE_QUEUE_SETTLED_INDEX_BYTES: Lazy<IntGaugeVec> = Lazy::new(|| {
    IntGaugeVec::new(
        Opts::new(
            WRITE_QUEUE_SETTLED_INDEX_BYTES_METRIC_NAME,
            "Index bytes after write-queue compaction and cleanup by tenant",
        ),
        &["tenant"],
    )
    .expect("write queue settled-index-byte gauge should be constructible")
});

static WRITE_QUEUE_SEGMENT_LABELS_BY_TENANT: Lazy<dashmap::DashMap<String, BTreeSet<String>>> =
    Lazy::new(dashmap::DashMap::new);

#[cfg(test)]
struct WriteQueuePhaseCapture {
    phase: String,
    count: u64,
}

#[cfg(test)]
thread_local! {
    static WRITE_QUEUE_PHASE_CAPTURE: RefCell<Option<WriteQueuePhaseCapture>> =
        const { RefCell::new(None) };
}

pub(super) fn observe_write_queue_phase(phase: &str, started_at: Instant) {
    WRITE_QUEUE_PHASE_SECONDS
        .with_label_values(&[phase])
        .observe(started_at.elapsed().as_secs_f64());
    #[cfg(test)]
    record_write_queue_phase_for_test(phase);
}

#[cfg(test)]
fn record_write_queue_phase_for_test(phase: &str) {
    WRITE_QUEUE_PHASE_CAPTURE.with(|capture| {
        let mut capture = capture.borrow_mut();
        let Some(capture) = capture.as_mut() else {
            return;
        };
        if capture.phase == phase {
            capture.count += 1;
        }
    });
}

#[cfg(test)]
fn count_write_queue_phase_observations_for_test<T>(
    phase: &str,
    test_body: impl FnOnce() -> T,
) -> (T, u64) {
    WRITE_QUEUE_PHASE_CAPTURE.with(|capture| {
        let previous = capture.replace(Some(WriteQueuePhaseCapture {
            phase: phase.to_string(),
            count: 0,
        }));
        let result = test_body();
        let active_capture = capture
            .replace(previous)
            .expect("phase capture should be installed for test body");
        (result, active_capture.count)
    })
}

fn observe_write_queue_writer_opened(tenant_id: &str) {
    WRITE_QUEUE_WRITER_OPENS_TOTAL
        .with_label_values(&[tenant_id])
        .inc();
}

pub(super) fn observe_write_queue_commit_succeeded(tenant_id: &str) {
    WRITE_QUEUE_COMMITS_TOTAL
        .with_label_values(&[tenant_id])
        .inc();
}

pub(super) fn observe_write_queue_writer_lifetime(tenant_id: &str, lifetime: Duration) {
    WRITE_QUEUE_WRITER_LIFETIME_SECONDS
        .with_label_values(&[tenant_id])
        .observe(lifetime.as_secs_f64());
}

pub(super) fn observe_write_queue_writer_merge_wait(
    tenant_id: &str,
    reason: &str,
    merge_wait: Duration,
) {
    WRITE_QUEUE_WRITER_MERGE_WAIT_SECONDS
        .with_label_values(&[tenant_id, reason])
        .observe(merge_wait.as_secs_f64());
}

pub(super) fn observe_write_queue_segment_health(
    tenant_id: &str,
    observation: &segment_observation::SegmentObservation,
) {
    WRITE_QUEUE_LIVE_SEGMENTS
        .with_label_values(&[tenant_id])
        .set(observation.live_segment_count as i64);
    WRITE_QUEUE_LIVE_DOCS
        .with_label_values(&[tenant_id])
        .set(observation.live_docs as i64);
    WRITE_QUEUE_INDEX_FILES
        .with_label_values(&[tenant_id])
        .set(observation.managed_index_file_count as i64);
    WRITE_QUEUE_INDEX_BYTES
        .with_label_values(&[tenant_id])
        .set(observation.index_bytes as i64);
    WRITE_QUEUE_ORPHAN_FILE_SETS
        .with_label_values(&[tenant_id])
        .set(observation.orphan_file_set_ids.len() as i64);
    replace_documents_per_segment_labels(tenant_id, &observation.per_segment_doc_counts);
}

pub(super) fn observe_write_queue_gc_removed_files(tenant_id: &str, removed_file_count: u64) {
    WRITE_QUEUE_GC_REMOVED_FILES_TOTAL
        .with_label_values(&[tenant_id])
        .inc_by(removed_file_count);
}

pub(super) fn observe_write_queue_settled_index_bytes(
    tenant_id: &str,
    observation: &segment_observation::SegmentObservation,
) {
    WRITE_QUEUE_SETTLED_INDEX_BYTES
        .with_label_values(&[tenant_id])
        .set(observation.index_bytes as i64);
}

fn observe_write_queue_writer_closed(tenant_id: &str, reason: &str) {
    WRITE_QUEUE_WRITER_CLOSES_TOTAL
        .with_label_values(&[tenant_id, reason])
        .inc();
}

fn replace_documents_per_segment_labels(
    tenant_id: &str,
    per_segment_doc_counts: &std::collections::BTreeMap<String, u64>,
) {
    if let Some((_, old_segment_ids)) =
        WRITE_QUEUE_SEGMENT_LABELS_BY_TENANT.remove(&tenant_id.to_string())
    {
        for segment_id in old_segment_ids {
            let label_values = [tenant_id, segment_id.as_str()];
            let _ = WRITE_QUEUE_DOCUMENTS_PER_SEGMENT.remove_label_values(&label_values);
        }
    }

    let mut segment_ids = BTreeSet::new();
    for (segment_id, doc_count) in per_segment_doc_counts {
        WRITE_QUEUE_DOCUMENTS_PER_SEGMENT
            .with_label_values(&[tenant_id, segment_id])
            .set(*doc_count as i64);
        segment_ids.insert(segment_id.clone());
    }
    WRITE_QUEUE_SEGMENT_LABELS_BY_TENANT.insert(tenant_id.to_string(), segment_ids);
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
    let collectors: [&dyn Collector; 14] = [
        &*WRITE_QUEUE_PHASE_SECONDS,
        &*WRITE_QUEUE_WRITER_OPENS_TOTAL,
        &*WRITE_QUEUE_COMMITS_TOTAL,
        &*WRITE_QUEUE_WRITER_CLOSES_TOTAL,
        &*WRITE_QUEUE_LIVE_SEGMENTS,
        &*WRITE_QUEUE_LIVE_DOCS,
        &*WRITE_QUEUE_DOCUMENTS_PER_SEGMENT,
        &*WRITE_QUEUE_INDEX_FILES,
        &*WRITE_QUEUE_INDEX_BYTES,
        &*WRITE_QUEUE_ORPHAN_FILE_SETS,
        &*WRITE_QUEUE_WRITER_LIFETIME_SECONDS,
        &*WRITE_QUEUE_WRITER_MERGE_WAIT_SECONDS,
        &*WRITE_QUEUE_GC_REMOVED_FILES_TOTAL,
        &*WRITE_QUEUE_SETTLED_INDEX_BYTES,
    ];
    collectors
        .into_iter()
        .flat_map(Collector::collect)
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
    pub tasks: Arc<dashmap::DashMap<String, TaskInfo>>,
    pub base_path: std::path::PathBuf,
    pub oplog: Option<Arc<crate::index::oplog::OpLog>>,
    pub admission_store: Arc<WriteAdmissionStore>,
    pub facet_cache: super::FacetCacheMap,
    pub lww_map: super::LwwMap,
    pub vector_ctx: VectorWriteContext,
    pub queue_metrics_id: u64,
    #[cfg(test)]
    pub test_overrides: WriteQueueTestOverrides,
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
pub(crate) struct WriteQueueTestOverrides {
    pub batch_size: Option<usize>,
    pub min_merge_segments: Option<usize>,
    pub max_docs_before_merge: Option<usize>,
    pub writer_idle_timeout: Option<Duration>,
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
    mut ctx: WriteQueueContext,
) -> crate::error::Result<(
    WriteQueue,
    tokio::task::JoinHandle<crate::error::Result<()>>,
)> {
    let (tx, rx) = mpsc::channel(write_queue_channel_capacity());
    // This guard spans startup replay and the steady-state worker so every
    // exit path retires any per-tenant metric series it created.
    let tenant_metrics = writer_lifecycle::WriteQueueTenantMetrics::for_queue(&ctx.tenant_id);
    ctx.queue_metrics_id = tenant_metrics.queue_metrics_id();

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

    let handle = tokio::spawn(async move {
        let _tenant_metrics = tenant_metrics;
        process_writes(ctx, rx, Vec::new()).await
    });

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
                    // Replay runs to completion before the steady-state worker
                    // is spawned, so this short-lived writer cannot coexist
                    // with the tenant worker's persistent writer.
                    let mut writer = None;
                    let resolved_batch_size = resolved_write_queue_batch_size(&ctx);
                    let replay_result: crate::error::Result<()> = async {
                        replay_admitted_writes(
                            &ctx,
                            &mut writer,
                            &mut pending,
                            replay_records,
                            resolved_batch_size,
                        )
                        .await?;
                        if !pending.is_empty() {
                            flush_pending_batch(&ctx, &mut writer, &mut pending).await?;
                        }
                        Ok(())
                    }
                    .await;
                    let close_result =
                        writer_lifecycle::close_startup_replay_writer(&ctx, &mut writer);
                    replay_result.and(close_result)
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

fn configure_merge_policy(_ctx: &WriteQueueContext, writer: &mut crate::index::ManagedIndexWriter) {
    // Stage 5 selected Tantivy 0.26.1 LogMergePolicy defaults on 2026-07-27:
    // min_num_segments=8, max_docs_before_merge=10_000_000, del_docs_ratio=0.3.
    // The ordered matrix's first passing candidate was the default row: 128
    // one-doc writes settled from 599497 bytes/128 segments to 140723 bytes/2
    // segments, and the larger 256-doc rerun settled from 1198051 bytes/256
    // segments to 263020 bytes/4 segments, both with exact query parity and no
    // settled orphan file sets. Later override rows also passed but were not
    // first in the fixed order.
    let mut merge_policy = tantivy::merge_policy::LogMergePolicy::default();
    merge_policy.set_min_num_segments(SELECTED_MERGE_POLICY_MIN_NUM_SEGMENTS);
    merge_policy.set_max_docs_before_merge(SELECTED_MERGE_POLICY_MAX_DOCS_BEFORE_MERGE);
    merge_policy.set_del_docs_ratio_before_merge(0.3);
    apply_usize_merge_policy_override(
        &mut merge_policy,
        #[cfg(test)]
        _ctx.test_overrides.min_merge_segments,
        WRITE_QUEUE_MIN_MERGE_SEGMENTS_ENV_VAR,
        "minimum merge segment count",
        tantivy::merge_policy::LogMergePolicy::set_min_num_segments,
    );
    apply_usize_merge_policy_override(
        &mut merge_policy,
        #[cfg(test)]
        _ctx.test_overrides.max_docs_before_merge,
        WRITE_QUEUE_MAX_DOCS_BEFORE_MERGE_ENV_VAR,
        "maximum docs before merge",
        tantivy::merge_policy::LogMergePolicy::set_max_docs_before_merge,
    );
    // Stage 5 will select production policy values from measured evidence; the
    // Stage 2 env overrides exist only to make lifecycle behavior observable.
    writer.set_merge_policy(Box::new(merge_policy));
}

fn apply_usize_merge_policy_override(
    merge_policy: &mut tantivy::merge_policy::LogMergePolicy,
    #[cfg(test)] test_override: Option<usize>,
    env_var: &str,
    description: &str,
    apply: fn(&mut tantivy::merge_policy::LogMergePolicy, usize),
) {
    #[cfg(test)]
    if let Some(parsed) = test_override {
        apply(merge_policy, parsed);
        return;
    }

    if let Ok(raw_value) = std::env::var(env_var) {
        match raw_value.parse::<usize>() {
            Ok(parsed) if parsed > 0 => apply(merge_policy, parsed),
            Ok(_) => {
                tracing::warn!("{env_var} must be greater than 0; ignoring {description} override");
            }
            Err(error) => {
                tracing::warn!(
                    "failed to parse {env_var}='{raw_value}' as usize: {error}; ignoring {description} override"
                );
            }
        }
    }
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
    let mut writer_waiter = None;
    loop {
        match index.writer() {
            Ok(writer) => {
                observe_write_queue_writer_opened(tenant_id);
                return Ok(writer);
            }
            Err(crate::error::FlapjackError::TooManyConcurrentWrites { current, max }) => {
                writer_waiter.get_or_insert_with(|| index.memory_budget().register_writer_waiter());
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
    writer: &mut Option<crate::index::ManagedIndexWriter>,
    pending: &mut Vec<WriteOp>,
) -> crate::error::Result<()> {
    let phase_start = Instant::now();
    if pending.is_empty() {
        observe_write_queue_phase(PHASE_FLUSH_PENDING_BATCH, phase_start);
        return Ok(());
    }
    let result = {
        let active_writer = match writer_lifecycle::writer_for_queue(ctx, writer).await {
            Ok(active_writer) => active_writer,
            Err(error) => {
                let pending_task_ids = pending
                    .iter()
                    .map(|op| op.task_id.clone())
                    .collect::<Vec<_>>();
                finalization::mark_tasks_failed(&ctx.tasks, &pending_task_ids, &error);
                return Err(error);
            }
        };
        commit_batch(ctx, pending, active_writer).await
    };
    let result = match result {
        Ok(()) => {
            backpressure::sample_after_worker_event(ctx);
            // A successful commit is a safe yield boundary: Tantivy has accepted
            // the batch, and merge quiescence preserves its background merge owner
            // before a real waiter receives the global writer permit.
            writer_lifecycle::yield_writer_to_waiter_after_merge_quiescence(ctx, writer)
        }
        Err(error) => {
            if let Err(close_error) =
                writer_lifecycle::close_writer_after_commit_failure(ctx, writer)
            {
                tracing::error!(
                    "[WQ {}] failed to quiesce writer after commit error: {}",
                    ctx.tenant_id,
                    close_error
                );
            }
            Err(error)
        }
    };
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
    let resolved_batch_size = resolved_write_queue_batch_size(&ctx);
    log_write_queue_start(tenant_id, resolved_batch_size);
    apply_write_queue_start_delay(tenant_id).await;
    let mut pending = Vec::new();
    // The writer slot starts empty so queues that never receive writes do not
    // consume memory budget. Active commits reuse one writer while uncontended;
    // safe post-commit boundaries yield its global slot to a real waiter.
    let mut writer = None;
    let mut deadline = reset_write_queue_deadline();
    replay_and_flush_admitted_writes(
        &ctx,
        &mut writer,
        &mut pending,
        replay_records,
        resolved_batch_size,
    )
    .await?;
    let mut writer_idle_since = writer.as_ref().map(|_| Instant::now());

    loop {
        log_write_queue_state(tenant_id, pending.len(), deadline);
        match next_write_queue_event(deadline, &mut rx).await {
            WriteQueueEvent::Received(op) => {
                if handle_received_write_op(
                    &ctx,
                    &mut writer,
                    &mut pending,
                    op,
                    resolved_batch_size,
                )
                .await?
                {
                    deadline = reset_write_queue_deadline();
                    writer_idle_since = writer.as_ref().map(|_| Instant::now());
                }
            }
            WriteQueueEvent::ChannelClosed => {
                writer_lifecycle::drain_writer_on_channel_close(&ctx, &mut writer, &mut pending)
                    .await?;
                break;
            }
            WriteQueueEvent::DeadlineElapsed => {
                deadline = handle_write_queue_timeout(
                    &ctx,
                    &mut writer,
                    &mut pending,
                    &mut writer_idle_since,
                )
                .await?;
            }
        }
    }
    observe_write_queue_phase(PHASE_PROCESS_WRITES, phase_start);
    Ok(())
}

fn resolved_write_queue_batch_size(_ctx: &WriteQueueContext) -> usize {
    #[cfg(test)]
    if let Some(batch_size) = _ctx.test_overrides.batch_size {
        return batch_size;
    }
    write_queue_batch_size()
}

fn log_write_queue_start(tenant_id: &str, resolved_batch_size: usize) {
    tracing::info!("Write queue started for tenant {}", tenant_id);
    tracing::info!(
        "[WQ {}] using resolved batch size {} from {}",
        tenant_id,
        resolved_batch_size,
        WRITE_QUEUE_BATCH_SIZE_ENV_VAR
    );
}

async fn apply_write_queue_start_delay(tenant_id: &str) {
    if let Some(delay) = write_queue_start_delay() {
        tracing::warn!(
            "[WQ {}] delaying write queue start by {:?}",
            tenant_id,
            delay
        );
        tokio::time::sleep(delay).await;
    }
}

async fn replay_and_flush_admitted_writes(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
    pending: &mut Vec<WriteOp>,
    replay_records: Vec<WriteAdmissionRecord>,
    resolved_batch_size: usize,
) -> crate::error::Result<()> {
    replay_admitted_writes(ctx, writer, pending, replay_records, resolved_batch_size).await?;
    if !pending.is_empty() {
        flush_pending_batch(ctx, writer, pending).await?;
    }
    Ok(())
}

async fn replay_admitted_writes(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
    pending: &mut Vec<WriteOp>,
    replay_records: Vec<WriteAdmissionRecord>,
    resolved_batch_size: usize,
) -> crate::error::Result<()> {
    for record in replay_records {
        handle_received_write_op(ctx, writer, pending, record.write_op(), resolved_batch_size)
            .await?;
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
    writer: &mut Option<crate::index::ManagedIndexWriter>,
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
        flush_pending_batch(ctx, writer, pending).await?;
        let writer = writer_lifecycle::writer_for_queue(ctx, writer).await?;
        // Compact reuses the tenant worker writer so background merge
        // ownership stays single-source instead of racing a second writer.
        finalization::compact_segments(&ctx.index, &ctx.tasks, &op.task_id, writer, tenant_id)?;
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
    flush_pending_batch(ctx, writer, pending).await?;
    Ok(true)
}

fn should_flush_pending_batch(pending_len: usize, resolved_batch_size: usize) -> bool {
    pending_len >= resolved_batch_size
}

async fn handle_write_queue_timeout(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
    pending: &mut Vec<WriteOp>,
    writer_idle_since: &mut Option<Instant>,
) -> crate::error::Result<Instant> {
    if pending.is_empty() {
        // No write arrived for a full interval. Yield only to a real waiter so
        // an uncontended writer remains alive to own background merges. A
        // contended yield waits for Tantivy merges before releasing the writer.
        writer_lifecycle::yield_writer_to_waiter_after_merge_quiescence(ctx, writer)?;
        writer_lifecycle::close_idle_writer_after_timeout(ctx, writer, *writer_idle_since)?;
        backpressure::sample_after_worker_event(ctx);
        if writer.is_none() {
            *writer_idle_since = None;
        }
    } else {
        tracing::debug!(
            "[WQ {}] timeout, flushing {} pending",
            ctx.tenant_id,
            pending.len()
        );
        flush_pending_batch(ctx, writer, pending).await?;
        *writer_idle_since = writer.as_ref().map(|_| Instant::now());
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
    let phase_start = Instant::now();
    writer.delete_term(tantivy::Term::from_field_text(id_field, &object_id));
    if track_primary_delete {
        prepared.primary_delete_ids.push(object_id.clone());
    }
    prepared.deleted_ids.push(object_id);
    observe_write_queue_phase(PHASE_DELETE_STAGING, phase_start);
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
        let phase_start = Instant::now();
        preparation_context
            .writer
            .delete_term(tantivy::Term::from_field_text(
                preparation_context.id_field,
                &doc.id,
            ));
        observe_write_queue_phase(PHASE_DELETE_STAGING, phase_start);
    }

    let conversion_start = Instant::now();
    let conversion_result = preparation_context
        .index
        .converter()
        .to_tantivy(&doc, preparation_context.settings);
    observe_write_queue_phase(PHASE_DOCUMENT_CONVERSION, conversion_start);

    match conversion_result {
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
