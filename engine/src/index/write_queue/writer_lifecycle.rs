//! Single-writer lifetime, release, and telemetry for a tenant write worker.
//!
//! A worker opens its writer lazily and retains it across uncontended commits. Channel close,
//! commit failure, idle timeout, startup replay completion, or a real memory-budget waiter all
//! close through the same merge-quiescent path. That path waits for merge threads, records
//! lifetime and merge-wait metrics, refreshes settled segment health, emits the close reason,
//! and finally releases the writer's memory-budget permit.

use super::{acquire_writer_for_queue, configure_merge_policy, WriteQueueContext};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_QUEUE_METRICS_ID: AtomicU64 = AtomicU64::new(1);
static LIVE_QUEUE_METRICS: Lazy<DashMap<String, BTreeSet<u64>>> = Lazy::new(DashMap::new);
#[derive(Clone, Copy)]
struct WriterOpenState {
    queue_metrics_id: u64,
    opened_at: Instant,
}

static WRITER_OPENED_AT_BY_TENANT: Lazy<DashMap<String, WriterOpenState>> = Lazy::new(DashMap::new);
const DEFAULT_WRITER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITER_IDLE_TIMEOUT_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_WRITER_IDLE_TIMEOUT_MS";
const WRITER_CLOSE_REASON_CHANNEL_CLOSED: &str = "channel_closed";
const WRITER_CLOSE_REASON_COMMIT_FAILURE: &str = "commit_failure";
const WRITER_CLOSE_REASON_IDLE_TIMEOUT: &str = "idle_timeout";
const WRITER_CLOSE_REASON_STARTUP_REPLAY: &str = "startup_replay";
const WRITER_CLOSE_REASON_WAITER_YIELD: &str = "waiter_yield";
const WRITER_CLOSE_REASONS: [&str; 5] = [
    WRITER_CLOSE_REASON_CHANNEL_CLOSED,
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
        let mut opened = acquire_writer_for_queue(&ctx.index, &ctx.tenant_id).await?;
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
) -> crate::error::Result<()> {
    if !ctx.index.memory_budget().has_writer_waiters() {
        return Ok(());
    }

    close_writer_after_merge_quiescence(ctx, writer, WRITER_CLOSE_REASON_WAITER_YIELD)
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
) -> crate::error::Result<()> {
    let flush_result = if pending.is_empty() {
        Ok(())
    } else {
        tracing::info!(
            "[WQ {}] channel closed, flushing {} pending",
            ctx.tenant_id,
            pending.len()
        );
        super::flush_pending_batch(ctx, writer, pending).await
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
        let merge_wait_started_at = Instant::now();
        let close_result = writer.wait_merging_threads();
        let merge_wait = merge_wait_started_at.elapsed();
        if let Some(opened_at) =
            remove_writer_open_state_for_queue(&ctx.tenant_id, ctx.queue_metrics_id)
        {
            super::observe_write_queue_writer_lifetime(&ctx.tenant_id, opened_at.elapsed());
        }
        super::observe_write_queue_writer_merge_wait(&ctx.tenant_id, reason, merge_wait);
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
