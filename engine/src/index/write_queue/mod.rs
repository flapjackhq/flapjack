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
mod compensation;
mod finalization;
pub(crate) mod segment_observation;
mod vectors;
mod writer_lifecycle;

#[cfg(any(debug_assertions, test, feature = "test-support"))]
pub use backpressure::force_backpressure_pause_for_test;
pub(crate) use compensation::{compensate_uncommitted_tasks, DurableReplayState};
#[cfg(test)]
pub(crate) use compensation::{
    compensation_fault_attempts_remaining_for_test, fail_compensation_attempts_for_test,
    fail_next_compensation_for_test, set_compensation_before_oplog_retraction_hook_for_test,
};
pub(crate) use finalization::PERSISTED_VECTORS_DIR;
#[cfg(any(test, feature = "fault-injection"))]
pub(crate) use finalization::{
    fail_next_commit_for_test, fail_next_finalization_for_test, inject_finalization_fault,
    FinalizationFaultPoint,
};
#[cfg(test)]
pub(crate) use writer_lifecycle::set_writer_close_hook_for_test;
pub(crate) use writer_lifecycle::WriteTaskHandle;
#[cfg(any(debug_assertions, test, feature = "test-support"))]
pub use writer_lifecycle::{
    clear_writer_lifecycle_test_events, record_writer_lifecycle_publication_checkpoint,
    writer_lifecycle_test_events, WriterLifecycleTestEvent,
};

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
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
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
#[cfg(any(debug_assertions, test, feature = "test-support"))]
const WRITE_QUEUE_TEST_COMMIT_DELAY_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_TEST_COMMIT_DELAY_MS";
#[cfg(test)]
static WRITE_QUEUE_TEST_COMMIT_DELAY_BY_TENANT: Lazy<dashmap::DashMap<String, Duration>> =
    Lazy::new(dashmap::DashMap::new);
pub(crate) const SELECTED_MERGE_POLICY_MIN_NUM_SEGMENTS: usize = 8;
pub(crate) const SELECTED_MERGE_POLICY_MAX_DOCS_BEFORE_MERGE: usize = 10_000_000;
// The canonical band spans both measured settled shapes: Stage 5's 128/256-document
// online specimens (2/4 segments) and Stage 6's 50k/100k staged bulk builds (8/9).
//
// This is NOT a test-only expectation: `backpressure::sample_is_at_or_below_selected_ceiling`
// and `all_samples_above_selected_ceiling` read `.1` as the live-segment ceiling that
// admission pauses above, so widening the band to cover the staged-bulk regime also
// raised that runtime ceiling. That direction is required — a bulk build that legitimately
// settles at 8-9 segments must not be paused as unhealthy — but it does mean the online
// path pauses later than it did at (2, 4). The online settled shape is pinned separately
// by `ONLINE_SPECIMEN_SETTLED_MAX` in write_queue_tests.rs so a regression there still fails.
pub(crate) const SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND: (usize, usize) = (2, 9);
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
pub(super) const PHASE_VERSION_STORE_UPDATE: &str = "version_store_update";
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
        PHASE_VERSION_STORE_UPDATE,
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

/// TODO: Document write_queue_batch_size.
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

/// TODO: Document writer_acquire_timeout.
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

/// TODO: Document write_queue_channel_capacity.
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

/// TODO: Document write_queue_start_delay.
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

/// Removes the tenant's injected commit stall when the registering test finishes.
#[cfg(test)]
pub(crate) struct WriteQueueTestCommitDelayGuard {
    tenant_id: String,
    previous_delay: Option<Duration>,
}

#[cfg(test)]
impl Drop for WriteQueueTestCommitDelayGuard {
    fn drop(&mut self) {
        match self.previous_delay {
            Some(delay) => {
                WRITE_QUEUE_TEST_COMMIT_DELAY_BY_TENANT.insert(self.tenant_id.clone(), delay);
            }
            None => {
                WRITE_QUEUE_TEST_COMMIT_DELAY_BY_TENANT.remove(&self.tenant_id);
            }
        }
    }
}

/// Stalls one tenant's commits so an in-crate test can hold a commit window open.
///
/// The `FLAPJACK_WRITE_QUEUE_TEST_COMMIT_DELAY_MS` fallback below stalls every write queue in
/// the process, which is only safe for the out-of-crate integration binaries that own their
/// whole process. Lib tests share one process with hundreds of others, so they register the
/// stall against their own tenant instead of exporting it to unrelated queues.
#[cfg(test)]
pub(crate) fn delay_commits_for_test(
    tenant_id: &str,
    delay: Duration,
) -> WriteQueueTestCommitDelayGuard {
    let previous_delay =
        WRITE_QUEUE_TEST_COMMIT_DELAY_BY_TENANT.insert(tenant_id.to_string(), delay);
    WriteQueueTestCommitDelayGuard {
        tenant_id: tenant_id.to_string(),
        previous_delay,
    }
}

#[cfg(test)]
fn tenant_scoped_test_commit_delay(tenant_id: &str) -> Option<Duration> {
    WRITE_QUEUE_TEST_COMMIT_DELAY_BY_TENANT
        .get(tenant_id)
        .map(|delay| *delay)
}

/// The tenant-scoped delay map is owned by this crate's own unit tests. Every
/// other build that compiles `write_queue_test_commit_delay` — debug, or a
/// dependent's test build enabling `test-support` — gets the no-op arm.
#[cfg(all(not(test), any(debug_assertions, feature = "test-support")))]
fn tenant_scoped_test_commit_delay(_tenant_id: &str) -> Option<Duration> {
    None
}

#[cfg(any(debug_assertions, test, feature = "test-support"))]
fn write_queue_test_commit_delay(tenant_id: &str) -> Option<Duration> {
    if let Some(delay) = tenant_scoped_test_commit_delay(tenant_id) {
        return Some(delay);
    }
    let raw_value = std::env::var(WRITE_QUEUE_TEST_COMMIT_DELAY_ENV_VAR).ok()?;
    match raw_value.parse::<u64>() {
        Ok(0) => None,
        Ok(parsed) => Some(Duration::from_millis(parsed)),
        Err(error) => {
            tracing::warn!(
                "failed to parse {}='{}' as milliseconds: {}; ignoring test commit delay",
                WRITE_QUEUE_TEST_COMMIT_DELAY_ENV_VAR,
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

/// Read-only counts of `_id` delete terms staged while preparing one write task.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteTermObservation {
    pub explicit_delete_actions: usize,
    pub document_write_delete_terms: usize,
}

/// Returns the delete-term work observed for one task without affecting write behavior.
pub fn delete_term_observation(task: &TaskInfo) -> DeleteTermObservation {
    DeleteTermObservation {
        explicit_delete_actions: task.explicit_delete_term_count,
        document_write_delete_terms: task.document_write_delete_term_count,
    }
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
    pub vector_ctx: VectorWriteContext,
    pub queue_metrics_id: u64,
    pub writer_buffer_size: usize,
    #[cfg(test)]
    pub test_overrides: WriteQueueTestOverrides,
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct WriteQueueTestOverrides {
    pub batch_size: Option<usize>,
    pub min_merge_segments: Option<usize>,
    pub max_docs_before_merge: Option<usize>,
    pub writer_idle_timeout: Option<Duration>,
    pub worker_start_gate: Option<Arc<WriteQueueWorkerGate>>,
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct WriteQueueWorkerGate {
    released: std::sync::Mutex<bool>,
    release_notification: std::sync::Condvar,
}

#[cfg(test)]
impl WriteQueueWorkerGate {
    pub(crate) fn closed() -> Self {
        Self {
            released: std::sync::Mutex::new(false),
            release_notification: std::sync::Condvar::new(),
        }
    }

    pub(crate) fn release(&self) {
        *self
            .released
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        self.release_notification.notify_all();
    }

    fn wait_until_released(&self) {
        let mut released = self
            .released
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*released {
            released = self
                .release_notification
                .wait(released)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicatedWriteOrigin {
    pub timestamp_ms: u64,
    pub node_id: String,
}

impl ReplicatedWriteOrigin {
    pub fn new(timestamp_ms: u64, node_id: String) -> Self {
        Self {
            timestamp_ms,
            node_id,
        }
    }

    fn into_oplog_origin(self) -> crate::index::oplog::OpLogOrigin {
        crate::index::oplog::OpLogOrigin::new(self.timestamp_ms, self.node_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WriteAction {
    Add(Document),
    Upsert(Document),
    /// Legacy replicated upsert with no recoverable origin tuple.
    UpsertNoLwwUpdate(Document),
    UpsertWithOrigin {
        doc: Document,
        origin: ReplicatedWriteOrigin,
    },
    Delete(String),
    /// Legacy replicated delete with no recoverable origin tuple.
    DeleteNoLwwUpdate(String),
    DeleteWithOrigin {
        object_id: String,
        origin: ReplicatedWriteOrigin,
    },
    Compact,
}

#[derive(Debug, Clone)]
pub struct WriteOp {
    pub task_id: String,
    pub actions: Vec<WriteAction>,
}

pub type WriteQueue = mpsc::Sender<WriteOp>;

#[derive(Clone)]
pub(crate) struct WriteQueueCancellation {
    sender: watch::Sender<bool>,
}

impl WriteQueueCancellation {
    pub(crate) fn cancel(&self) {
        let _ = self.sender.send(true);
    }

    fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }
}

#[derive(Clone)]
pub(crate) struct WriteQueueWorkerCompletion {
    inner: Arc<WriteQueueWorkerCompletionInner>,
}

struct WriteQueueWorkerCompletionInner {
    result: Mutex<Option<crate::error::Result<()>>>,
    ready: Condvar,
}

struct WriteQueueWorkerCompletionReporter {
    completion: WriteQueueWorkerCompletion,
    tenant_id: String,
    reported: bool,
}

impl WriteQueueWorkerCompletion {
    fn new() -> Self {
        Self {
            inner: Arc::new(WriteQueueWorkerCompletionInner {
                result: Mutex::new(None),
                ready: Condvar::new(),
            }),
        }
    }

    fn complete(&self, result: crate::error::Result<()>) {
        *self
            .inner
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
        self.inner.ready.notify_all();
    }

    fn reporter(&self, tenant_id: String) -> WriteQueueWorkerCompletionReporter {
        WriteQueueWorkerCompletionReporter {
            completion: self.clone(),
            tenant_id,
            reported: false,
        }
    }

    pub(crate) fn wait_timeout(&self, timeout: Duration) -> Option<crate::error::Result<()>> {
        let deadline = Instant::now() + timeout;
        let mut guard = self
            .inner
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(result) = guard.clone() {
                return Some(result);
            }
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let (next_guard, wait) = self
                .inner
                .ready
                .wait_timeout(guard, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard = next_guard;
            if wait.timed_out() && guard.is_none() {
                return None;
            }
        }
    }
}

impl WriteQueueWorkerCompletionReporter {
    fn report(mut self, result: crate::error::Result<()>) {
        self.completion.complete(result);
        self.reported = true;
    }
}

impl Drop for WriteQueueWorkerCompletionReporter {
    fn drop(&mut self) {
        if !self.reported {
            self.completion.complete(write_queue_worker_stopped_result(
                &self.tenant_id,
                "worker thread unwound",
            ));
        }
    }
}

fn write_queue_worker_stopped_result(
    tenant_id: &str,
    detail: impl std::fmt::Display,
) -> crate::error::Result<()> {
    Err(crate::error::FlapjackError::Tantivy(format!(
        "write queue worker for {tenant_id} stopped before reporting completion: {detail}"
    )))
}

type PreparedWriteDocument = (String, serde_json::Value, tantivy::TantivyDocument);

struct PreparedWriteOperation {
    task_id: String,
    numeric_id: String,
    valid_docs: Vec<PreparedWriteDocument>,
    rejected: Vec<DocFailure>,
    deleted_ids: Vec<String>,
    oplog_ops: Vec<crate::index::oplog::OpLogOperation>,
    oplog_receipts: Vec<crate::index::oplog::OpLogReceipt>,
    explicit_delete_term_count: usize,
    document_write_delete_term_count: usize,
    #[cfg(feature = "vector-search")]
    doc_vectors: Vec<Option<std::collections::HashMap<String, Vec<f32>>>>,
    #[cfg(feature = "vector-search")]
    vectors_modified: bool,
}

struct PreparedWriteBatch {
    operations: Vec<PreparedWriteOperation>,
    added_count: usize,
    deleted_count: usize,
    rejected_count: usize,
}

impl PreparedWriteOperation {
    fn new(task_id: String, numeric_id: String) -> Self {
        Self {
            task_id,
            numeric_id,
            valid_docs: Vec::new(),
            rejected: Vec::new(),
            deleted_ids: Vec::new(),
            oplog_ops: Vec::new(),
            oplog_receipts: Vec::new(),
            explicit_delete_term_count: 0,
            document_write_delete_term_count: 0,
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

#[derive(Clone)]
enum DocumentWriteMode {
    Add,
    PrimaryUpsert,
    ReplicatedUpsert(Option<ReplicatedWriteOrigin>),
}

#[derive(Clone)]
enum OplogWriteOrigin {
    Local,
    Replicated(ReplicatedWriteOrigin),
    LegacyUnproven,
}

impl DocumentWriteMode {
    fn deletes_existing(&self) -> bool {
        matches!(self, Self::PrimaryUpsert | Self::ReplicatedUpsert(_))
    }

    fn oplog_origin(&self) -> OplogWriteOrigin {
        match self {
            Self::ReplicatedUpsert(Some(origin)) => OplogWriteOrigin::Replicated(origin.clone()),
            Self::ReplicatedUpsert(None) => OplogWriteOrigin::LegacyUnproven,
            Self::Add | Self::PrimaryUpsert => OplogWriteOrigin::Local,
        }
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
    #[cfg(feature = "vector-search")]
    vector_ctx: &'a VectorWriteContext,
    #[cfg(feature = "vector-search")]
    embedder_configs: &'a [(String, crate::vector::config::EmbedderConfig)],
}

/// Spawn the background write-processing task for a tenant and return the channel sender and join handle.
///
/// # Arguments
///
/// * `tenant_id` - Tenant identifier used for logging and path resolution.
/// * `index` - Shared Tantivy index to write documents into.
/// * `tasks` - Shared task-status map updated as operations are processed.
/// * `base_path` - Root data directory; tenant subdirectories contain settings, oplog, and vector files.
/// * `oplog` - Optional operation log for durable write-ahead recording.
/// * `facet_cache` - Shared facet cache invalidated after each commit.
/// * `vector_ctx` - Vector index context for embedding and storing document vectors.
///
/// # Returns
///
/// A `(WriteQueue, JoinHandle, WriteQueueCancellation, WriteQueueWorkerCompletion)` tuple: the
/// channel sender for submitting `WriteOp`s, the async worker completion handle, the cancellation
/// signal, and a blocking worker completion signal.
pub(crate) fn create_write_queue(
    mut ctx: WriteQueueContext,
) -> crate::error::Result<(
    WriteQueue,
    tokio::task::JoinHandle<crate::error::Result<()>>,
    WriteQueueCancellation,
    WriteQueueWorkerCompletion,
)> {
    let (tx, rx) = mpsc::channel(write_queue_channel_capacity());
    let (cancellation, cancellation_rx) = write_queue_cancellation_channel();
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
    run_replay_startup(&ctx, replay_records, &cancellation)?;

    let (handle, completion) = spawn_dedicated_write_worker(
        ctx,
        rx,
        cancellation.clone(),
        cancellation_rx,
        tenant_metrics,
    )?;

    Ok((tx, handle, cancellation, completion))
}

fn write_queue_cancellation_channel() -> (WriteQueueCancellation, watch::Receiver<bool>) {
    let (sender, receiver) = watch::channel(false);
    (WriteQueueCancellation { sender }, receiver)
}

fn spawn_dedicated_write_worker(
    ctx: WriteQueueContext,
    rx: mpsc::Receiver<WriteOp>,
    cancellation: WriteQueueCancellation,
    cancellation_rx: watch::Receiver<bool>,
    tenant_metrics: writer_lifecycle::WriteQueueTenantMetrics,
) -> crate::error::Result<(
    tokio::task::JoinHandle<crate::error::Result<()>>,
    WriteQueueWorkerCompletion,
)> {
    let tenant_id = ctx.tenant_id.clone();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let completion = WriteQueueWorkerCompletion::new();
    let thread_completion = completion.clone();
    let completion_tenant_id = tenant_id.clone();
    std::thread::Builder::new()
        .name(format!("flapjack-write-{tenant_id}"))
        .spawn(move || {
            let completion_reporter = thread_completion.reporter(completion_tenant_id);
            let result = run_dedicated_write_worker_runtime(
                ctx,
                rx,
                cancellation,
                cancellation_rx,
                tenant_metrics,
            );
            completion_reporter.report(result.clone());
            let _ = result_tx.send(result);
        })
        .map_err(|error| {
            crate::error::FlapjackError::Tantivy(format!(
                "failed to spawn dedicated write queue worker for {tenant_id}: {error}"
            ))
        })?;

    let handle = tokio::spawn(async move {
        match result_rx.await {
            Ok(result) => result,
            Err(error) => write_queue_worker_stopped_result(&tenant_id, error),
        }
    });
    Ok((handle, completion))
}

fn run_dedicated_write_worker_runtime(
    ctx: WriteQueueContext,
    rx: mpsc::Receiver<WriteOp>,
    cancellation: WriteQueueCancellation,
    cancellation_rx: watch::Receiver<bool>,
    tenant_metrics: writer_lifecycle::WriteQueueTenantMetrics,
) -> crate::error::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            crate::error::FlapjackError::Tantivy(format!(
                "failed to create dedicated write queue runtime: {error}"
            ))
        })?
        .block_on(async move {
            // Tokio is local to this dedicated writer thread. Blocking Tantivy
            // commit work no longer runs on the shared server runtime.
            let _tenant_metrics = tenant_metrics;
            process_writes(ctx, rx, cancellation, cancellation_rx, Vec::new()).await
        })
}

/// TODO: Document run_replay_startup.
fn run_replay_startup(
    ctx: &WriteQueueContext,
    replay_records: Vec<WriteAdmissionRecord>,
    cancellation: &WriteQueueCancellation,
) -> crate::error::Result<()> {
    if replay_records.is_empty() {
        return Ok(());
    }

    let ctx = ctx.clone();
    let cancellation = cancellation.clone();
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
                            &cancellation,
                        )
                        .await?;
                        if !pending.is_empty() {
                            flush_pending_batch(&ctx, &mut writer, &mut pending, &cancellation)
                                .await?;
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
    writer_buffer_size: usize,
) -> crate::error::Result<crate::index::ManagedIndexWriter> {
    const RETRY_INTERVAL: Duration = Duration::from_millis(5);
    let acquire_timeout = writer_acquire_timeout();
    let deadline = Instant::now() + acquire_timeout;
    let mut retries = 0usize;
    let mut writer_waiter = None;
    loop {
        match index.writer_with_size(writer_buffer_size) {
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
                std::thread::sleep(RETRY_INTERVAL);
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
    cancellation: &WriteQueueCancellation,
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
            writer_lifecycle::yield_writer_to_waiter_after_merge_quiescence(
                ctx,
                writer,
                cancellation,
            )
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
    cancellation: WriteQueueCancellation,
    mut cancellation_rx: watch::Receiver<bool>,
    replay_records: Vec<WriteAdmissionRecord>,
) -> crate::error::Result<()> {
    let phase_start = Instant::now();
    let tenant_id = &ctx.tenant_id;
    let resolved_batch_size = resolved_write_queue_batch_size(&ctx);
    log_write_queue_start(tenant_id, resolved_batch_size);
    apply_write_queue_start_delay(tenant_id).await;
    #[cfg(test)]
    wait_for_test_worker_start_gate(&ctx);
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
        &cancellation,
    )
    .await?;
    let mut writer_idle_since = writer.as_ref().map(|_| Instant::now());

    loop {
        log_write_queue_state(tenant_id, pending.len(), deadline);
        match next_write_queue_event(deadline, &mut rx, &mut cancellation_rx).await {
            WriteQueueEvent::Received(op) => {
                if handle_received_write_op(
                    &ctx,
                    &mut writer,
                    &mut pending,
                    op,
                    resolved_batch_size,
                    &cancellation,
                )
                .await?
                {
                    deadline = reset_write_queue_deadline();
                    writer_idle_since = writer.as_ref().map(|_| Instant::now());
                }
            }
            WriteQueueEvent::ChannelClosed => {
                writer_lifecycle::drain_writer_on_channel_close(
                    &ctx,
                    &mut writer,
                    &mut pending,
                    &cancellation,
                )
                .await?;
                break;
            }
            WriteQueueEvent::Cancelled => {
                writer_lifecycle::close_writer_after_cancellation(&ctx, &mut writer)?;
                break;
            }
            WriteQueueEvent::DeadlineElapsed => {
                deadline = handle_write_queue_timeout(
                    &ctx,
                    &mut writer,
                    &mut pending,
                    &mut writer_idle_since,
                    &cancellation,
                )
                .await?;
            }
        }
    }
    observe_write_queue_phase(PHASE_PROCESS_WRITES, phase_start);
    Ok(())
}

#[cfg(test)]
fn wait_for_test_worker_start_gate(ctx: &WriteQueueContext) {
    if let Some(gate) = &ctx.test_overrides.worker_start_gate {
        gate.wait_until_released();
    }
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
        std::thread::sleep(delay);
    }
}

async fn replay_and_flush_admitted_writes(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
    pending: &mut Vec<WriteOp>,
    replay_records: Vec<WriteAdmissionRecord>,
    resolved_batch_size: usize,
    cancellation: &WriteQueueCancellation,
) -> crate::error::Result<()> {
    replay_admitted_writes(
        ctx,
        writer,
        pending,
        replay_records,
        resolved_batch_size,
        cancellation,
    )
    .await?;
    if !pending.is_empty() {
        flush_pending_batch(ctx, writer, pending, cancellation).await?;
    }
    Ok(())
}

async fn replay_admitted_writes(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
    pending: &mut Vec<WriteOp>,
    replay_records: Vec<WriteAdmissionRecord>,
    resolved_batch_size: usize,
    cancellation: &WriteQueueCancellation,
) -> crate::error::Result<()> {
    for record in replay_records {
        handle_received_write_op(
            ctx,
            writer,
            pending,
            record.write_op(),
            resolved_batch_size,
            cancellation,
        )
        .await?;
    }
    Ok(())
}

enum WriteQueueEvent {
    Received(WriteOp),
    ChannelClosed,
    Cancelled,
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
    cancellation_rx: &mut watch::Receiver<bool>,
) -> WriteQueueEvent {
    if *cancellation_rx.borrow() {
        return WriteQueueEvent::Cancelled;
    }

    tokio::select! {
        maybe_op = rx.recv() => match maybe_op {
            Some(op) => WriteQueueEvent::Received(op),
            None => WriteQueueEvent::ChannelClosed,
        },
        changed = cancellation_rx.changed() => match changed {
            Ok(()) => WriteQueueEvent::Cancelled,
            Err(_) => WriteQueueEvent::Cancelled,
        },
        _ = timeout_at(deadline.into(), std::future::pending::<()>()) => {
            WriteQueueEvent::DeadlineElapsed
        }
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
    cancellation: &WriteQueueCancellation,
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
        flush_pending_batch(ctx, writer, pending, cancellation).await?;
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
    flush_pending_batch(ctx, writer, pending, cancellation).await?;
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
    cancellation: &WriteQueueCancellation,
) -> crate::error::Result<Instant> {
    if pending.is_empty() {
        // No write arrived for a full interval. Yield only to a real waiter so
        // an uncontended writer remains alive to own background merges. A
        // contended yield waits for Tantivy merges before releasing the writer.
        writer_lifecycle::yield_writer_to_waiter_after_merge_quiescence(ctx, writer, cancellation)?;
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
        flush_pending_batch(ctx, writer, pending, cancellation).await?;
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
/// Certify a batch as failed only after retracting its durable side effects.
///
/// Runs [`compensation::compensate_failed_commit_batch`] first. On success the
/// tasks are marked terminal `Failed` and `error` is returned. On compensation
/// failure the tasks are left non-terminal and the compensation error is
/// returned instead, so the worker exits and recovery replays the still-durable
/// state rather than the client seeing a `Failed` verdict a restart could
/// contradict. Only pre-commit failures use this path; a post-commit failure
/// leaves durable Tantivy state that must survive, so it marks failed directly.
fn fail_batch_with_compensation(
    context: &WriteFinalizationContext<'_>,
    pre_batch_oplog_seq: Option<u64>,
    batch_task_ids: &[String],
    error: crate::error::FlapjackError,
) -> crate::error::FlapjackError {
    if let Err(compensation_error) =
        compensation::compensate_failed_commit_batch(context, pre_batch_oplog_seq, batch_task_ids)
    {
        return compensation_error;
    }
    finalization::mark_tasks_failed(context.tasks, batch_task_ids, &error);
    error
}

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
        #[cfg(feature = "vector-search")]
        vector_ctx: &ctx.vector_ctx,
        #[cfg(feature = "vector-search")]
        embedder_configs: &embedder_configs,
    };

    // The oplog sequence floor bounds this batch's task-tagged rows. Synchronous
    // metadata may interleave above the same floor, so compensation selects by
    // task id and preserves unrelated rows (see fail_batch_with_compensation).
    let pre_batch_oplog_seq = finalization_context.oplog.map(|oplog| oplog.current_seq());

    let prepared_batch =
        match stage_batch_for_commit(&finalization_context, settings.as_ref(), writer, ops).await {
            Ok(prepared_batch) => prepared_batch,
            Err(error) => {
                return Err(fail_batch_with_compensation(
                    &finalization_context,
                    pre_batch_oplog_seq,
                    &batch_task_ids,
                    error,
                ));
            }
        };

    #[cfg(any(debug_assertions, test, feature = "test-support"))]
    if let Some(delay) = write_queue_test_commit_delay(ctx.tenant_id.as_str()) {
        // A yielding sleep would hide the shared-runtime blocking reproduced only by
        // single_worker_runtime_serves_count_during_injected_two_second_commit in
        // engine/flapjack-http/tests/write_runtime_isolation.rs.
        std::thread::sleep(delay);
    }

    let build_secs = match finalization::commit_writer_with_panic_guard(
        writer,
        ctx.tenant_id.as_str(),
        prepared_batch.added_count,
        prepared_batch.deleted_count,
        prepared_batch.rejected_count,
    ) {
        Ok(build_secs) => build_secs,
        Err(error) => {
            // The Tantivy commit failed after the batch appended to the oplog.
            // Retract the orphaned oplog/admission state so recovery cannot
            // resurrect writes the client is about to be told failed (DUR-1).
            return Err(fail_batch_with_compensation(
                &finalization_context,
                pre_batch_oplog_seq,
                &batch_task_ids,
                error,
            ));
        }
    };
    publish_committed_batch(
        &finalization_context,
        &prepared_batch.operations,
        build_secs,
        &batch_task_ids,
    )?;

    observe_write_queue_phase(PHASE_COMMIT_BATCH, phase_start);
    Ok(())
}

async fn stage_batch_for_commit(
    context: &WriteFinalizationContext<'_>,
    settings: Option<&crate::index::settings::IndexSettings>,
    writer: &mut crate::index::ManagedIndexWriter,
    ops: &mut Vec<WriteOp>,
) -> crate::error::Result<PreparedWriteBatch> {
    let mut operations = Vec::with_capacity(ops.len());
    let mut added_count = 0;
    let mut deleted_count = 0;
    let mut rejected_count = 0;

    for op in ops.drain(..) {
        // Stage every queued operation into one writer so a queue flush pays
        // Tantivy's fixed commit cost only once.
        let prepared = stage_write_op_for_commit(context, settings, writer, op).await?;
        added_count += prepared.valid_docs.len();
        deleted_count += prepared.deleted_ids.len();
        rejected_count += prepared.rejected.len();
        operations.push(prepared);
    }

    Ok(PreparedWriteBatch {
        operations,
        added_count,
        deleted_count,
        rejected_count,
    })
}

fn publish_committed_batch(
    context: &WriteFinalizationContext<'_>,
    prepared_ops: &[PreparedWriteOperation],
    build_secs: u64,
    batch_task_ids: &[String],
) -> crate::error::Result<()> {
    #[cfg(any(test, feature = "fault-injection"))]
    if let Err(error) = finalization::inject_finalization_fault(
        context.tenant_id,
        finalization::FinalizationFaultPoint::AfterTantivyCommitBeforeVersionReceipts,
    ) {
        finalization::mark_tasks_failed(context.tasks, batch_task_ids, &error);
        return Err(error);
    }
    if let Err(error) = finalization::finalize_committed_batch(context, prepared_ops, build_secs) {
        finalization::mark_tasks_failed(context.tasks, batch_task_ids, &error);
        return Err(error);
    }
    if let Err(error) = context.admission_store.remove_tasks(
        prepared_ops
            .iter()
            .map(|prepared| prepared.task_id.as_str()),
    ) {
        finalization::mark_tasks_failed(context.tasks, batch_task_ids, &error);
        return Err(error);
    }
    finalization::forget_finalized_tasks(context.base_path, context.tenant_id, prepared_ops);
    for prepared in prepared_ops {
        record_delete_term_observation(context.tasks, prepared);
        finalization::mark_task_succeeded(context.tasks, prepared);
    }
    Ok(())
}

fn record_delete_term_observation(
    tasks: &Arc<dashmap::DashMap<String, TaskInfo>>,
    prepared: &PreparedWriteOperation,
) {
    for task_id in [&prepared.task_id, &prepared.numeric_id] {
        if let Some(mut task) = tasks.get_mut(task_id) {
            task.explicit_delete_term_count = prepared.explicit_delete_term_count;
            task.document_write_delete_term_count = prepared.document_write_delete_term_count;
        }
    }
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
/// post-commit finalization (oplog, caches, durable versions, vectors), and mark
/// succeeded.
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

    let _valid_docs_json = finalization::write_valid_documents(writer, &prepared.valid_docs)?;
    prepared.oplog_receipts = finalization::append_batch_to_oplog(
        context.oplog,
        &prepared.task_id,
        &prepared.oplog_ops,
        context.tenant_id,
    )?;
    #[cfg(any(test, feature = "fault-injection"))]
    finalization::inject_finalization_fault(
        context.tenant_id,
        finalization::FinalizationFaultPoint::AfterOplogAppendBeforeTantivyCommit,
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

/// Dispatch each `WriteAction` to delete, add, upsert, or replicated document preparation.
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
                    OplogWriteOrigin::Local,
                );
            }
            WriteAction::DeleteNoLwwUpdate(object_id) => {
                prepare_delete_action(
                    prepared,
                    preparation_context.writer,
                    preparation_context.id_field,
                    object_id,
                    OplogWriteOrigin::LegacyUnproven,
                );
            }
            WriteAction::DeleteWithOrigin { object_id, origin } => {
                prepare_delete_action(
                    prepared,
                    preparation_context.writer,
                    preparation_context.id_field,
                    object_id,
                    OplogWriteOrigin::Replicated(origin),
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
                    DocumentWriteMode::ReplicatedUpsert(None),
                );
            }
            WriteAction::UpsertWithOrigin { doc, origin } => {
                prepare_document_write(
                    preparation_context,
                    prepared,
                    doc,
                    DocumentWriteMode::ReplicatedUpsert(Some(origin)),
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
    origin: OplogWriteOrigin,
) {
    let phase_start = Instant::now();
    writer.delete_term(tantivy::Term::from_field_text(id_field, &object_id));
    if let Some(operation) = oplog_operation(
        "delete",
        serde_json::json!({"objectID": object_id.clone()}),
        origin,
    ) {
        prepared.oplog_ops.push(operation);
    }
    prepared.explicit_delete_term_count += 1;
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
        prepared.document_write_delete_term_count += 1;
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
            if let Some(operation) = oplog_operation(
                "upsert",
                serde_json::json!({"objectID": doc.id.clone(), "body": doc_json.clone()}),
                document_write_mode.oplog_origin(),
            ) {
                prepared.oplog_ops.push(operation);
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

fn oplog_operation(
    op_type: &'static str,
    payload: serde_json::Value,
    origin: OplogWriteOrigin,
) -> Option<crate::index::oplog::OpLogOperation> {
    match origin {
        OplogWriteOrigin::Local => {
            Some(crate::index::oplog::OpLogOperation::local(op_type, payload))
        }
        OplogWriteOrigin::Replicated(origin) => {
            Some(crate::index::oplog::OpLogOperation::replicated(
                op_type,
                payload,
                origin.into_oplog_origin(),
            ))
        }
        // Old admission records intentionally carried no origin tuple. Replaying
        // their Tantivy mutation is idempotent, but publishing invented conflict
        // evidence would poison the durable owner and downstream peers.
        OplogWriteOrigin::LegacyUnproven => None,
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
