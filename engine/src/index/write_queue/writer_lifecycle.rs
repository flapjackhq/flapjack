//! Single-writer lifetime, release, and telemetry for a tenant write worker.
//!
//! A worker opens its writer lazily and retains it across uncontended commits. Channel close,
//! commit failure, idle timeout, startup replay completion, or a real memory-budget waiter all
//! close through the same merge-quiescent path. That path waits for merge threads, records
//! lifetime and merge-wait metrics, refreshes settled segment health, emits the close reason,
//! and finally releases the writer's memory-budget permit.

use super::{
    acquire_writer_for_queue, configure_merge_policy, WriteQueueCancellation, WriteQueueContext,
    WriteQueueWorkerCompletion,
};
use crate::error::{FlapjackError, Result};
use crate::types::TenantId;
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(any(debug_assertions, test, feature = "test-support"))]
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

static NEXT_QUEUE_METRICS_ID: AtomicU64 = AtomicU64::new(1);
static LIVE_QUEUE_METRICS: Lazy<DashMap<String, BTreeSet<u64>>> = Lazy::new(DashMap::new);
const DROP_WRITE_QUEUE_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(any(debug_assertions, test, feature = "test-support"))]
static WRITER_LIFECYCLE_TEST_LOG: Lazy<Mutex<WriterLifecycleTestLog>> =
    Lazy::new(|| Mutex::new(WriterLifecycleTestLog::default()));
#[cfg(test)]
static WRITER_CLOSE_HOOK: Lazy<Mutex<Option<WriterCloseHook>>> = Lazy::new(|| Mutex::new(None));

#[derive(Clone)]
pub(crate) struct WriteTaskHandle {
    inner: Arc<WriteTaskHandleInner>,
}

struct WriteTaskHandleInner {
    state: std::sync::Mutex<WriteTaskHandleState>,
    completion: tokio::sync::Notify,
    cancellation: Option<WriteQueueCancellation>,
    worker_completion: Option<WriteQueueWorkerCompletion>,
}

enum WriteTaskHandleState {
    Running(JoinHandle<Result<()>>),
    Draining,
    Finished(Result<()>),
}

impl WriteTaskHandle {
    #[cfg(test)]
    pub(crate) fn new(handle: JoinHandle<Result<()>>) -> Self {
        Self {
            inner: Arc::new(WriteTaskHandleInner {
                state: std::sync::Mutex::new(WriteTaskHandleState::Running(handle)),
                completion: tokio::sync::Notify::new(),
                cancellation: None,
                worker_completion: None,
            }),
        }
    }

    pub(crate) fn new_with_cancellation(
        handle: JoinHandle<Result<()>>,
        cancellation: WriteQueueCancellation,
        worker_completion: WriteQueueWorkerCompletion,
    ) -> Self {
        Self {
            inner: Arc::new(WriteTaskHandleInner {
                state: std::sync::Mutex::new(WriteTaskHandleState::Running(handle)),
                completion: tokio::sync::Notify::new(),
                cancellation: Some(cancellation),
                worker_completion: Some(worker_completion),
            }),
        }
    }

    pub(crate) fn abort(&self) {
        if let Some(cancellation) = &self.inner.cancellation {
            // Dedicated write workers stop at the next write-loop event
            // boundary after committing any work already inside commit_batch.
            cancellation.cancel();
        }
        if let WriteTaskHandleState::Running(handle) = &*self.inner.state.lock().unwrap() {
            if self.inner.cancellation.is_none() {
                handle.abort();
            }
        }
    }

    pub(crate) fn wait_for_shutdown_after_cancellation(&self, tenant_id: TenantId) {
        let Some(worker_completion) = self.inner.worker_completion.clone() else {
            return;
        };

        match worker_completion.wait_timeout(DROP_WRITE_QUEUE_DRAIN_TIMEOUT) {
            Some(Ok(())) => {}
            Some(Err(error)) => {
                tracing::error!(
                    tenant_id = %tenant_id,
                    %error,
                    "write queue drain failed during IndexManager drop"
                );
            }
            None => {
                tracing::error!(
                    tenant_id = %tenant_id,
                    "write queue drain did not report completion during IndexManager drop"
                );
            }
        }
    }

    pub(crate) async fn drain(&self, tenant_id: TenantId) -> Result<()> {
        self.start_drain_monitor(tenant_id);
        loop {
            let notified = self.inner.completion.notified();
            if let WriteTaskHandleState::Finished(result) = &*self.inner.state.lock().unwrap() {
                return result.clone();
            }
            notified.await;
        }
    }

    pub(crate) fn same_handle(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    fn start_drain_monitor(&self, tenant_id: TenantId) {
        let handle = {
            let mut state_guard = self.inner.state.lock().unwrap();
            match std::mem::replace(&mut *state_guard, WriteTaskHandleState::Draining) {
                WriteTaskHandleState::Running(handle) => Some(handle),
                previous @ (WriteTaskHandleState::Draining | WriteTaskHandleState::Finished(_)) => {
                    *state_guard = previous;
                    None
                }
            }
        };

        if let Some(handle) = handle {
            let inner = Arc::clone(&self.inner);
            tokio::spawn(async move {
                let result = match handle.await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => Err(write_queue_drain_error(&tenant_id, error)),
                    Err(error) => Err(write_queue_drain_error(&tenant_id, error)),
                };
                *inner.state.lock().unwrap() = WriteTaskHandleState::Finished(result);
                inner.completion.notify_waiters();
            });
        }
    }
}

fn write_queue_drain_error(tenant_id: &str, error: impl std::fmt::Display) -> FlapjackError {
    FlapjackError::Tantivy(format!(
        "destination write queue drain failed for {tenant_id}: {error}"
    ))
}

#[cfg(test)]
type WriterCloseHook = Arc<dyn Fn(&str) + Send + Sync + 'static>;

#[cfg(test)]
pub(crate) struct WriterCloseHookGuard {
    previous: Option<WriterCloseHook>,
}

#[cfg(test)]
impl Drop for WriterCloseHookGuard {
    fn drop(&mut self) {
        *WRITER_CLOSE_HOOK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.previous.take();
    }
}

#[cfg(test)]
pub(crate) fn set_writer_close_hook_for_test(
    hook: impl Fn(&str) + Send + Sync + 'static,
) -> WriterCloseHookGuard {
    let mut slot = WRITER_CLOSE_HOOK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    WriterCloseHookGuard {
        previous: slot.replace(Arc::new(hook)),
    }
}

#[cfg(test)]
fn run_writer_close_hook_for_test(tenant_id: &str) {
    let hook = WRITER_CLOSE_HOOK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Some(hook) = hook {
        hook(tenant_id);
    }
}

#[cfg(not(test))]
fn run_writer_close_hook_for_test(_tenant_id: &str) {}

#[cfg(any(debug_assertions, test, feature = "test-support"))]
const WRITER_LIFECYCLE_TEST_EVENT_LIMIT: usize = 4096;
#[cfg(any(debug_assertions, test, feature = "test-support"))]
const WRITER_LIFECYCLE_TEST_TENANT_LIMIT: usize = 4096;

#[cfg(any(debug_assertions, test, feature = "test-support"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriterLifecycleTestEvent {
    pub tenant_id: String,
    pub reason: String,
    pub phase: &'static str,
    pub sequence: u64,
}

#[cfg(any(debug_assertions, test, feature = "test-support"))]
#[derive(Default)]
struct WriterLifecycleTestLog {
    events_by_tenant: BTreeMap<String, Vec<WriterLifecycleTestEvent>>,
    next_sequence_by_tenant: BTreeMap<String, u64>,
    tenant_order: VecDeque<String>,
}

#[cfg(any(debug_assertions, test, feature = "test-support"))]
impl WriterLifecycleTestLog {
    fn clear(&mut self) {
        self.events_by_tenant.clear();
        self.next_sequence_by_tenant.clear();
        self.tenant_order.clear();
    }

    fn push(&mut self, tenant_id: &str, reason: &str, phase: &'static str) {
        if !self.events_by_tenant.contains_key(tenant_id) {
            self.tenant_order.push_back(tenant_id.to_string());
            if self.tenant_order.len() > WRITER_LIFECYCLE_TEST_TENANT_LIMIT {
                if let Some(expired_tenant) = self.tenant_order.pop_front() {
                    self.events_by_tenant.remove(&expired_tenant);
                    self.next_sequence_by_tenant.remove(&expired_tenant);
                }
            }
        }
        let sequence = self
            .next_sequence_by_tenant
            .entry(tenant_id.to_string())
            .or_default();
        *sequence += 1;
        let events = self
            .events_by_tenant
            .entry(tenant_id.to_string())
            .or_default();
        events.push(WriterLifecycleTestEvent {
            tenant_id: tenant_id.to_string(),
            reason: reason.to_string(),
            phase,
            sequence: *sequence,
        });
        let excess = events
            .len()
            .saturating_sub(WRITER_LIFECYCLE_TEST_EVENT_LIMIT);
        if excess > 0 {
            events.drain(..excess);
        }
    }

    fn events_for_tenant(&self, tenant_id: &str) -> Vec<WriterLifecycleTestEvent> {
        self.events_by_tenant
            .get(tenant_id)
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }
}

#[derive(Clone, Copy)]
struct WriterOpenState {
    queue_metrics_id: u64,
    opened_at: Instant,
}

static WRITER_OPENED_AT_BY_TENANT: Lazy<DashMap<String, WriterOpenState>> = Lazy::new(DashMap::new);
// Stage 6 selected 30s with:
// `timeout 600 cargo test -p flapjack --lib -- index::write_queue::tests::writer_idle_timeout_candidate_matrix_selects_default --ignored --nocapture`.
// n=1 per candidate/gap: 30s retained 10s/25s burst gaps, 15s reopened at
// 25s, and 60s retained 35s but delayed one-slot admission by about 60s.
pub(super) const DEFAULT_WRITER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITER_IDLE_TIMEOUT_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_WRITER_IDLE_TIMEOUT_MS";
const WRITER_CLOSE_REASON_CHANNEL_CLOSED: &str = "channel_closed";
const WRITER_CLOSE_REASON_CANCELLED: &str = "cancelled";
const WRITER_CLOSE_REASON_COMMIT_FAILURE: &str = "commit_failure";
const WRITER_CLOSE_REASON_IDLE_TIMEOUT: &str = "idle_timeout";
const WRITER_CLOSE_REASON_STARTUP_REPLAY: &str = "startup_replay";
const WRITER_CLOSE_REASON_WAITER_YIELD: &str = "waiter_yield";
const WRITER_WAITER_HANDOFF_POLL_INTERVAL: Duration = Duration::from_millis(1);
const WRITER_CLOSE_REASONS: [&str; 6] = [
    WRITER_CLOSE_REASON_CHANNEL_CLOSED,
    WRITER_CLOSE_REASON_CANCELLED,
    WRITER_CLOSE_REASON_COMMIT_FAILURE,
    WRITER_CLOSE_REASON_IDLE_TIMEOUT,
    WRITER_CLOSE_REASON_STARTUP_REPLAY,
    WRITER_CLOSE_REASON_WAITER_YIELD,
];

pub(super) struct WriteQueueTenantMetrics {
    tenant_id: String,
    queue_metrics_id: u64,
}

impl WriteQueueTenantMetrics {
    pub(super) fn for_queue(tenant_id: &str) -> Self {
        let queue_metrics_id = NEXT_QUEUE_METRICS_ID.fetch_add(1, Ordering::Relaxed);
        register_live_queue_metrics_id(tenant_id, queue_metrics_id);
        Self {
            tenant_id: tenant_id.to_string(),
            queue_metrics_id,
        }
    }

    pub(super) fn queue_metrics_id(&self) -> u64 {
        self.queue_metrics_id
    }
}

impl Drop for WriteQueueTenantMetrics {
    fn drop(&mut self) {
        if !retire_live_queue_metrics_id(&self.tenant_id, self.queue_metrics_id) {
            return;
        }

        let label_values = [self.tenant_id.as_str()];
        let _ = super::WRITE_QUEUE_WRITER_OPENS_TOTAL.remove_label_values(&label_values);
        let _ = super::WRITE_QUEUE_COMMITS_TOTAL.remove_label_values(&label_values);
        let _ = super::WRITE_QUEUE_LIVE_SEGMENTS.remove_label_values(&label_values);
        let _ = super::WRITE_QUEUE_LIVE_DOCS.remove_label_values(&label_values);
        let _ = super::WRITE_QUEUE_INDEX_FILES.remove_label_values(&label_values);
        let _ = super::WRITE_QUEUE_INDEX_BYTES.remove_label_values(&label_values);
        let _ = super::WRITE_QUEUE_ORPHAN_FILE_SETS.remove_label_values(&label_values);
        let _ = super::WRITE_QUEUE_WRITER_LIFETIME_SECONDS.remove_label_values(&label_values);
        let _ = super::WRITE_QUEUE_GC_REMOVED_FILES_TOTAL.remove_label_values(&label_values);
        let _ = super::WRITE_QUEUE_SETTLED_INDEX_BYTES.remove_label_values(&label_values);
        remove_writer_open_state_for_queue(&self.tenant_id, self.queue_metrics_id);
        if let Some((_, segment_ids)) =
            super::WRITE_QUEUE_SEGMENT_LABELS_BY_TENANT.remove(&self.tenant_id)
        {
            for segment_id in segment_ids {
                let label_values = [self.tenant_id.as_str(), segment_id.as_str()];
                let _ = super::WRITE_QUEUE_DOCUMENTS_PER_SEGMENT.remove_label_values(&label_values);
            }
        }
        for reason in WRITER_CLOSE_REASONS {
            let label_values = [self.tenant_id.as_str(), reason];
            let _ = super::WRITE_QUEUE_WRITER_CLOSES_TOTAL.remove_label_values(&label_values);
            let _ = super::WRITE_QUEUE_WRITER_MERGE_WAIT_SECONDS.remove_label_values(&label_values);
        }
    }
}

fn register_live_queue_metrics_id(tenant_id: &str, queue_metrics_id: u64) {
    match LIVE_QUEUE_METRICS.entry(tenant_id.to_string()) {
        Entry::Occupied(mut entry) => {
            entry.get_mut().insert(queue_metrics_id);
        }
        Entry::Vacant(entry) => {
            entry.insert(BTreeSet::from([queue_metrics_id]));
        }
    }
}

fn retire_live_queue_metrics_id(tenant_id: &str, queue_metrics_id: u64) -> bool {
    match LIVE_QUEUE_METRICS.entry(tenant_id.to_string()) {
        Entry::Occupied(mut entry) => {
            entry.get_mut().remove(&queue_metrics_id);
            if entry.get().is_empty() {
                entry.remove();
                true
            } else {
                false
            }
        }
        Entry::Vacant(_) => false,
    }
}

pub(super) async fn writer_for_queue<'a>(
    ctx: &WriteQueueContext,
    writer: &'a mut Option<crate::index::ManagedIndexWriter>,
) -> crate::error::Result<&'a mut crate::index::ManagedIndexWriter> {
    if writer.is_none() {
        let mut opened =
            acquire_writer_for_queue(&ctx.index, &ctx.tenant_id, ctx.writer_buffer_size).await?;
        // Merge policy is installed once when the tenant worker opens its
        // writer; keeping the writer alive keeps that merge owner alive too.
        configure_merge_policy(ctx, &mut opened);
        record_writer_open_state(&ctx.tenant_id, ctx.queue_metrics_id, Instant::now());
        *writer = Some(opened);
    }
    Ok(writer
        .as_mut()
        .expect("writer slot should be populated after successful acquisition"))
}

pub(super) fn yield_writer_to_waiter_after_merge_quiescence(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
    cancellation: &WriteQueueCancellation,
) -> crate::error::Result<()> {
    let Some(handoff) = ctx.index.memory_budget().writer_waiter_handoff() else {
        return Ok(());
    };

    let close_result =
        close_writer_after_merge_quiescence(ctx, writer, WRITER_CLOSE_REASON_WAITER_YIELD);
    if close_result.is_ok() {
        wait_for_registered_writer_waiter_handoff(&handoff, cancellation);
    }
    close_result
}

fn wait_for_registered_writer_waiter_handoff(
    handoff: &crate::index::memory::WriterWaiterHandoff,
    cancellation: &WriteQueueCancellation,
) {
    // Every captured waiter owns a bounded writer-acquire attempt, so normal
    // completion remains bounded by that contract. Explicit queue cancellation
    // lets abort wake this worker immediately instead of weakening fairness.
    while !handoff.is_complete() && !cancellation.is_cancelled() {
        std::thread::sleep(WRITER_WAITER_HANDOFF_POLL_INTERVAL);
    }
}

#[cfg(test)]
// The lifecycle tests sit beside the private handoff seam they exercise; the
// production close helpers remain below so their operational sequence stays
// contiguous.
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::index::memory::{MemoryBudget, MemoryBudgetConfig};

    #[test]
    fn prior_writer_waiter_handoff_does_not_expire_after_fixed_timeout() {
        let budget = MemoryBudget::new(MemoryBudgetConfig {
            max_concurrent_writers: 1,
            ..Default::default()
        });
        let first_waiter = budget.register_writer_waiter();
        let second_waiter = budget.register_writer_waiter();
        let handoff = budget
            .writer_waiter_handoff()
            .expect("handoff should capture both prior waiters");
        let (cancellation, _cancellation_rx) = super::super::write_queue_cancellation_channel();
        let (handoff_complete_tx, handoff_complete_rx) = std::sync::mpsc::channel();

        let wait_thread = std::thread::spawn(move || {
            wait_for_registered_writer_waiter_handoff(&handoff, &cancellation);
            handoff_complete_tx.send(()).unwrap();
        });

        assert!(
            matches!(
                handoff_complete_rx.recv_timeout(Duration::from_millis(125)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "the yielding tenant must remain outside writer acquisition while captured prior waiters remain registered"
        );

        drop(first_waiter);
        drop(second_waiter);
        handoff_complete_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handoff should complete after all captured prior waiters retire");
        wait_thread.join().unwrap();
    }

    #[test]
    fn cancellation_ends_a_pending_writer_waiter_handoff() {
        let budget = MemoryBudget::new(MemoryBudgetConfig::default());
        let _prior_waiter = budget.register_writer_waiter();
        let handoff = budget
            .writer_waiter_handoff()
            .expect("handoff should capture the prior waiter");
        let (cancellation, _cancellation_rx) = super::super::write_queue_cancellation_channel();
        let cancellation_for_wait = cancellation.clone();
        let (handoff_complete_tx, handoff_complete_rx) = std::sync::mpsc::channel();

        let wait_thread = std::thread::spawn(move || {
            wait_for_registered_writer_waiter_handoff(&handoff, &cancellation_for_wait);
            handoff_complete_tx.send(()).unwrap();
        });
        std::thread::sleep(Duration::from_millis(10));

        cancellation.cancel();

        handoff_complete_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("cancellation should wake a yielding worker without waiting for prior waiters");
        wait_thread.join().unwrap();
    }

    #[cfg(debug_assertions)]
    #[test]
    fn writer_lifecycle_sequence_is_scoped_per_tenant() {
        record_writer_lifecycle_test_event(
            "tenant_with_prior_events",
            "channel_closed",
            "merge_quiesced",
        );
        record_writer_lifecycle_test_event(
            "tenant_with_prior_events",
            "publication",
            "publication_checkpoint",
        );
        record_writer_lifecycle_test_event(
            "tenant_with_independent_sequence",
            "channel_closed",
            "merge_quiesced",
        );

        let events = writer_lifecycle_test_events("tenant_with_independent_sequence");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].sequence, 1,
            "retained lifecycle sequence must be assigned inside the tenant scope"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn writer_lifecycle_test_events_are_bounded_in_debug_builds() {
        record_writer_lifecycle_test_event(
            "unrelated_retained_tenant",
            "channel_closed",
            "merge_quiesced",
        );

        for index in 0..5000 {
            record_writer_lifecycle_test_event(
                "bounded_retention",
                "channel_closed",
                if index % 2 == 0 {
                    "merge_quiesced"
                } else {
                    "publication_checkpoint"
                },
            );
        }

        let events = writer_lifecycle_test_events("bounded_retention");
        assert!(
            events.len() <= 4096,
            "debug writer-lifecycle retention must be bounded, got {} events",
            events.len()
        );
        assert_eq!(
            events.last().map(|event| event.sequence),
            Some(5000),
            "bounded retention should keep the newest lifecycle evidence"
        );
        assert_eq!(
            writer_lifecycle_test_events("unrelated_retained_tenant").len(),
            1,
            "one tenant's high event volume must not evict another parallel test's evidence"
        );
    }
}

pub(super) fn close_idle_writer_after_timeout(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
    idle_since: Option<Instant>,
) -> crate::error::Result<()> {
    let Some(idle_since) = idle_since else {
        return Ok(());
    };
    if idle_since.elapsed() < writer_idle_timeout(ctx) {
        return Ok(());
    }

    close_writer_after_merge_quiescence(ctx, writer, WRITER_CLOSE_REASON_IDLE_TIMEOUT)
}

pub(super) async fn drain_writer_on_channel_close(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
    pending: &mut Vec<super::WriteOp>,
    cancellation: &WriteQueueCancellation,
) -> crate::error::Result<()> {
    let flush_result = if pending.is_empty() {
        Ok(())
    } else {
        tracing::info!(
            "[WQ {}] channel closed, flushing {} pending",
            ctx.tenant_id,
            pending.len()
        );
        super::flush_pending_batch(ctx, writer, pending, cancellation).await
    };
    let close_result =
        close_writer_after_merge_quiescence(ctx, writer, WRITER_CLOSE_REASON_CHANNEL_CLOSED);

    flush_result.and(close_result)
}

pub(super) fn close_startup_replay_writer(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
) -> crate::error::Result<()> {
    close_writer_after_merge_quiescence(ctx, writer, WRITER_CLOSE_REASON_STARTUP_REPLAY)
}

pub(super) fn close_writer_after_cancellation(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
) -> crate::error::Result<()> {
    close_writer_after_merge_quiescence(ctx, writer, WRITER_CLOSE_REASON_CANCELLED)
}

pub(super) fn close_writer_after_commit_failure(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
) -> crate::error::Result<()> {
    // Tantivy does not promise that a writer remains reusable after every
    // commit error, so discard its staged state and reopen lazily.
    close_writer_after_merge_quiescence(ctx, writer, WRITER_CLOSE_REASON_COMMIT_FAILURE)
}

fn close_writer_after_merge_quiescence(
    ctx: &WriteQueueContext,
    writer: &mut Option<crate::index::ManagedIndexWriter>,
    reason: &str,
) -> crate::error::Result<()> {
    if let Some(writer) = writer.take() {
        run_writer_close_hook_for_test(&ctx.tenant_id);
        let merge_wait_started_at = Instant::now();
        let close_result = writer.wait_merging_threads();
        let merge_wait = merge_wait_started_at.elapsed();
        if let Some(opened_at) =
            remove_writer_open_state_for_queue(&ctx.tenant_id, ctx.queue_metrics_id)
        {
            super::observe_write_queue_writer_lifetime(&ctx.tenant_id, opened_at.elapsed());
        }
        super::observe_write_queue_writer_merge_wait(&ctx.tenant_id, reason, merge_wait);
        if close_result.is_ok() {
            record_writer_lifecycle_test_event(&ctx.tenant_id, reason, "merge_quiesced");
        }
        super::finalization::record_segment_health(&ctx.tenant_id, &ctx.index);
        super::observe_write_queue_writer_closed(&ctx.tenant_id, reason);
        tracing::debug!(
            "[WQ {}] closed writer after merge quiescence ({})",
            ctx.tenant_id,
            reason
        );
        close_result?;
    }
    Ok(())
}

#[cfg(any(debug_assertions, test, feature = "test-support"))]
fn record_writer_lifecycle_test_event(tenant_id: &str, reason: &str, phase: &'static str) {
    WRITER_LIFECYCLE_TEST_LOG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(tenant_id, reason, phase);
}

#[cfg(not(any(debug_assertions, test, feature = "test-support")))]
fn record_writer_lifecycle_test_event(_tenant_id: &str, _reason: &str, _phase: &'static str) {}

#[cfg(any(debug_assertions, test, feature = "test-support"))]
pub fn record_writer_lifecycle_publication_checkpoint(tenant_id: &str, phase: &'static str) {
    record_writer_lifecycle_test_event(tenant_id, "publication", phase);
}

#[cfg(any(debug_assertions, test, feature = "test-support"))]
pub fn clear_writer_lifecycle_test_events() {
    WRITER_LIFECYCLE_TEST_LOG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

#[cfg(any(debug_assertions, test, feature = "test-support"))]
pub fn writer_lifecycle_test_events(tenant_id: &str) -> Vec<WriterLifecycleTestEvent> {
    WRITER_LIFECYCLE_TEST_LOG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .events_for_tenant(tenant_id)
}

fn record_writer_open_state(tenant_id: &str, queue_metrics_id: u64, opened_at: Instant) {
    WRITER_OPENED_AT_BY_TENANT.insert(
        tenant_id.to_string(),
        WriterOpenState {
            queue_metrics_id,
            opened_at,
        },
    );
}

fn remove_writer_open_state_for_queue(tenant_id: &str, queue_metrics_id: u64) -> Option<Instant> {
    WRITER_OPENED_AT_BY_TENANT
        .remove_if(tenant_id, |_, state| {
            state.queue_metrics_id == queue_metrics_id
        })
        .map(|(_, state)| state.opened_at)
}

#[cfg(test)]
pub(super) fn record_writer_open_state_for_test(tenant_id: &str, queue_metrics_id: u64) {
    record_writer_open_state(tenant_id, queue_metrics_id, Instant::now());
}

#[cfg(test)]
pub(super) fn writer_open_queue_metrics_id_for_test(tenant_id: &str) -> Option<u64> {
    WRITER_OPENED_AT_BY_TENANT
        .get(tenant_id)
        .map(|state| state.queue_metrics_id)
}

#[cfg(test)]
pub(super) fn remove_writer_open_state_for_test(tenant_id: &str, queue_metrics_id: u64) -> bool {
    remove_writer_open_state_for_queue(tenant_id, queue_metrics_id).is_some()
}

pub(super) fn writer_idle_timeout(_ctx: &WriteQueueContext) -> Duration {
    #[cfg(test)]
    if let Some(timeout) = _ctx.test_overrides.writer_idle_timeout {
        return timeout;
    }
    configured_writer_idle_timeout()
}

pub(super) fn configured_writer_idle_timeout() -> Duration {
    match std::env::var(WRITER_IDLE_TIMEOUT_ENV_VAR) {
        Ok(raw_value) => match raw_value.parse::<u64>() {
            Ok(parsed) if parsed > 0 => Duration::from_millis(parsed),
            Ok(_) => {
                tracing::warn!(
                    "{} must be greater than 0; falling back to {:?}",
                    WRITER_IDLE_TIMEOUT_ENV_VAR,
                    DEFAULT_WRITER_IDLE_TIMEOUT
                );
                DEFAULT_WRITER_IDLE_TIMEOUT
            }
            Err(error) => {
                tracing::warn!(
                    "failed to parse {}='{}' as milliseconds: {}; falling back to {:?}",
                    WRITER_IDLE_TIMEOUT_ENV_VAR,
                    raw_value,
                    error,
                    DEFAULT_WRITER_IDLE_TIMEOUT
                );
                DEFAULT_WRITER_IDLE_TIMEOUT
            }
        },
        Err(_) => DEFAULT_WRITER_IDLE_TIMEOUT,
    }
}
