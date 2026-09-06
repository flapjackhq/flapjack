//! Stub summary for finalization.rs.
use std::sync::Arc;

use crate::types::{TaskInfo, TaskStatus};

use super::{PreparedWriteDocument, PreparedWriteOperation, WriteFinalizationContext};

pub(crate) const PERSISTED_VECTORS_DIR: &str = "vectors";

#[cfg(any(test, feature = "fault-injection"))]
static FINALIZATION_FAULTS: once_cell::sync::Lazy<
    dashmap::DashMap<String, FinalizationFaultPoint>,
> = once_cell::sync::Lazy::new(dashmap::DashMap::new);
#[cfg(test)]
static COMMITS_IN_PROGRESS: once_cell::sync::Lazy<dashmap::DashSet<String>> =
    once_cell::sync::Lazy::new(dashmap::DashSet::new);

#[cfg(test)]
struct CommitInProgressGuard<'a> {
    tenant_id: &'a str,
}

#[cfg(test)]
impl Drop for CommitInProgressGuard<'_> {
    fn drop(&mut self) {
        COMMITS_IN_PROGRESS.remove(self.tenant_id);
    }
}

#[cfg(any(test, feature = "fault-injection"))]
pub(crate) struct FinalizationFaultGuard {
    tenant_id: String,
}

#[cfg(test)]
impl FinalizationFaultGuard {
    pub(crate) fn was_triggered(&self) -> bool {
        !FINALIZATION_FAULTS.contains_key(&self.tenant_id)
    }
}

#[cfg(any(test, feature = "fault-injection"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizationFaultPoint {
    BeforeTantivyCommit,
    DuringOplogAppendAfterPartialDurableWrite,
    AfterOplogAppendBeforeTantivyCommit,
    AfterTantivyCommitBeforeVersionReceipts,
    AfterFirstVersionReceiptStatement,
    AfterVersionTransactionBeforeCommittedSeq,
    AfterCommittedSeqBeforeOplogTruncation,
    AfterOplogTruncationBeforeAdmissionAck,
}

#[cfg(any(test, feature = "fault-injection"))]
impl Drop for FinalizationFaultGuard {
    fn drop(&mut self) {
        FINALIZATION_FAULTS.remove(&self.tenant_id);
    }
}

#[cfg(any(test, feature = "fault-injection"))]
pub(crate) fn fail_next_commit_for_test(tenant_id: &str) -> FinalizationFaultGuard {
    fail_next_finalization_for_test(tenant_id, FinalizationFaultPoint::BeforeTantivyCommit)
}

#[cfg(any(test, feature = "fault-injection"))]
pub(crate) fn fail_next_finalization_for_test(
    tenant_id: &str,
    fault_point: FinalizationFaultPoint,
) -> FinalizationFaultGuard {
    let tenant_id = tenant_id.to_string();
    assert!(
        FINALIZATION_FAULTS
            .insert(tenant_id.clone(), fault_point)
            .is_none(),
        "a finalization failure is already armed for tenant {tenant_id}"
    );
    FinalizationFaultGuard { tenant_id }
}

#[cfg(test)]
pub(crate) fn commit_is_in_progress_for_test(tenant_id: &str) -> bool {
    COMMITS_IN_PROGRESS.contains(tenant_id)
}

#[cfg(any(test, feature = "fault-injection"))]
pub(crate) fn inject_finalization_fault(
    tenant_id: &str,
    fault_point: FinalizationFaultPoint,
) -> crate::error::Result<()> {
    let should_inject = FINALIZATION_FAULTS
        .get(tenant_id)
        .is_some_and(|armed| *armed.value() == fault_point);
    if !should_inject {
        return Ok(());
    }
    FINALIZATION_FAULTS.remove(tenant_id);
    match fault_point {
        FinalizationFaultPoint::BeforeTantivyCommit => {
            return Err(crate::error::FlapjackError::Tantivy(
                "injected write-queue commit failure".to_string(),
            ));
        }
        FinalizationFaultPoint::DuringOplogAppendAfterPartialDurableWrite => {
            return Err(crate::error::FlapjackError::Io(
                "injected oplog append I/O failure after partial durable write".to_string(),
            ));
        }
        _ => {}
    }
    Err(crate::error::FlapjackError::Tantivy(format!(
        "injected write-queue finalization failure at {fault_point:?}"
    )))
}

pub(super) fn write_valid_documents(
    writer: &mut crate::index::ManagedIndexWriter,
    valid_docs: &[PreparedWriteDocument],
) -> crate::error::Result<Vec<(String, serde_json::Value)>> {
    let mut valid_docs_json = Vec::new();
    for (doc_id, doc_json, tantivy_doc) in valid_docs {
        let phase_start = std::time::Instant::now();
        writer.add_document(tantivy_doc.clone())?;
        super::observe_write_queue_phase(super::PHASE_ADD_STAGING, phase_start);
        valid_docs_json.push((doc_id.clone(), doc_json.clone()));
    }
    Ok(valid_docs_json)
}

/// Append upsert and delete operations to the oplog as a single batch. No-ops
/// if the oplog is `None` or if the batch is empty.
pub(super) fn append_batch_to_oplog(
    oplog: Option<&Arc<crate::index::oplog::OpLog>>,
    task_id: &str,
    batch_ops: &[crate::index::oplog::OpLogOperation],
    tenant_id: &str,
) -> crate::error::Result<Vec<crate::index::oplog::OpLogReceipt>> {
    let Some(oplog) = oplog else {
        return Ok(Vec::new());
    };

    if batch_ops.is_empty() {
        return Ok(Vec::new());
    }

    let phase_start = std::time::Instant::now();
    let result = oplog
        .append_operations_for_task(task_id, batch_ops.to_vec())
        .map_err(|error| {
            tracing::error!("[WQ {}] oplog append failed: {}", tenant_id, error);
            error
        });
    super::observe_write_queue_phase(super::PHASE_OPLOG_APPEND, phase_start);
    result
}

/// Commit the Tantivy writer, catching panics via `catch_unwind` to prevent
/// process abort. Returns commit wall-time in seconds on success; wraps
/// panics and errors into `FlapjackError`.
pub(super) fn commit_writer_with_panic_guard(
    writer: &mut crate::index::ManagedIndexWriter,
    tenant_id: &str,
    added_count: usize,
    deleted_count: usize,
    rejected_count: usize,
) -> crate::error::Result<u64> {
    let phase_start = std::time::Instant::now();
    tracing::info!(
        "[WQ {}] committing {} adds, {} deletes, {} rejected",
        tenant_id,
        added_count,
        deleted_count,
        rejected_count
    );
    #[cfg(any(test, feature = "fault-injection"))]
    inject_finalization_fault(tenant_id, FinalizationFaultPoint::BeforeTantivyCommit)?;
    #[cfg(test)]
    let _commit_in_progress = {
        COMMITS_IN_PROGRESS.insert(tenant_id.to_string());
        CommitInProgressGuard { tenant_id }
    };
    let commit_start = std::time::Instant::now();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.commit())) {
        Ok(Ok(_opstamp)) => {
            super::observe_write_queue_phase(super::PHASE_WRITER_COMMIT, commit_start);
            super::observe_write_queue_commit_succeeded(tenant_id);
            super::observe_write_queue_phase(
                super::PHASE_COMMIT_WRITER_WITH_PANIC_GUARD,
                phase_start,
            );
            Ok(commit_start.elapsed().as_secs())
        }
        Ok(Err(error)) => {
            tracing::error!("[WQ {}] commit error: {}", tenant_id, error);
            Err(error.into())
        }
        Err(panic_info) => {
            let msg = if let Some(message) = panic_info.downcast_ref::<String>() {
                message.clone()
            } else if let Some(message) = panic_info.downcast_ref::<&str>() {
                message.to_string()
            } else {
                "unknown panic in tantivy commit".to_string()
            };
            tracing::error!("[WQ {}] PANIC during commit: {}", tenant_id, msg);
            Err(crate::error::FlapjackError::Tantivy(msg))
        }
    }
}

/// Publish all post-Tantivy state for a committed write batch.
///
/// Per-object versions are committed in one SQLite transaction before the
/// oplog watermark advances.
///
/// This is the committed-post-Tantivy path, so an error here cannot retract the
/// batch — its documents are already durable in the index. The caller
/// ([`super::publish_committed_batch`]) therefore marks the batch's tasks
/// terminal `Failed` and exits the tenant writer, while leaving the durable
/// admission records in place. Only the pre-commit path compensates, via
/// [`super::compensation::compensate_failed_commit_batch`].
///
/// Recovery is then idempotent re-application, not a blanket replay of oplog
/// receipts: [`super::admission::reconcile_records`] re-drives a surviving
/// admission record only when neither the advanced `committed_seq` nor the
/// version store already reports its task as published.
pub(super) fn finalize_committed_batch(
    context: &WriteFinalizationContext<'_>,
    prepared_ops: &[PreparedWriteOperation],
    build_secs: u64,
) -> crate::error::Result<()> {
    let phase_start = std::time::Instant::now();
    let metadata_start = std::time::Instant::now();
    persist_index_metadata(context.base_path, context.tenant_id, build_secs);
    super::observe_write_queue_phase(super::PHASE_METADATA_PERSISTENCE, metadata_start);
    #[cfg(feature = "vector-search")]
    {
        save_vector_index(context, prepared_ops);
    }
    let version_start = std::time::Instant::now();
    let committed_watermark =
        apply_version_receipts(context.base_path, context.tenant_id, prepared_ops)?;
    super::observe_write_queue_phase(super::PHASE_VERSION_STORE_UPDATE, version_start);
    let oplog_state_start = std::time::Instant::now();
    persist_oplog_commit_state(
        context.oplog,
        context.base_path,
        context.tenant_id,
        committed_watermark,
    )?;
    super::observe_write_queue_phase(
        super::PHASE_OPLOG_COMMIT_STATE_PERSISTENCE,
        oplog_state_start,
    );
    let reload_start = std::time::Instant::now();
    refresh_search_state(context.index, context.facet_cache, context.tenant_id)?;
    super::observe_write_queue_phase(super::PHASE_READER_RELOAD, reload_start);
    record_segment_health(context.tenant_id, context.index);
    super::observe_write_queue_phase(super::PHASE_FINALIZE_COMMITTED_BATCH, phase_start);
    Ok(())
}

fn persist_index_metadata(base_path: &std::path::Path, tenant_id: &str, build_secs: u64) {
    let tenant_dir = base_path.join(tenant_id);
    if let Ok(mut meta) = crate::index::index_metadata::IndexMetadata::load_or_create(&tenant_dir) {
        meta.last_build_time_s = build_secs;
        if let Err(error) = meta.save(&tenant_dir) {
            tracing::warn!(
                "[WQ {}] failed to save index metadata: {}",
                tenant_id,
                error
            );
        }
    }
}

fn refresh_search_state(
    index: &Arc<crate::index::Index>,
    facet_cache: &super::super::FacetCacheMap,
    tenant_id: &str,
) -> crate::error::Result<()> {
    index.reader().reload()?;
    index.invalidate_searchable_paths_cache();
    facet_cache.retain(|cache_key, _| !cache_key.belongs_to_tenant(tenant_id));
    Ok(())
}

/// Persist the in-memory VectorIndex to disk and save the embedder fingerprint
/// for change detection. Skips entirely when no vectors were modified in the batch.
#[cfg(feature = "vector-search")]
fn save_vector_index(
    context: &WriteFinalizationContext<'_>,
    prepared_ops: &[PreparedWriteOperation],
) {
    if !prepared_ops
        .iter()
        .any(|prepared| prepared.vectors_modified)
    {
        return;
    }

    let vectors_dir = context
        .base_path
        .join(context.tenant_id)
        .join(PERSISTED_VECTORS_DIR);
    let Some(vector_index) = context.vector_ctx.vector_indices.get(context.tenant_id) else {
        return;
    };
    let read_result = vector_index.read();
    let Ok(guard) = read_result else {
        return;
    };

    let vector_start = std::time::Instant::now();
    if let Err(error) = guard.save(&vectors_dir) {
        tracing::error!(
            "[WQ {}] failed to save vector index: {}",
            context.tenant_id,
            error
        );
        super::observe_write_queue_phase(super::PHASE_VECTOR_SAVE, vector_start);
        return;
    }

    if context.embedder_configs.is_empty() {
        super::observe_write_queue_phase(super::PHASE_VECTOR_SAVE, vector_start);
        return;
    }

    let fingerprint = crate::vector::config::EmbedderFingerprint::from_configs(
        context.embedder_configs,
        guard.dimensions(),
    );
    if let Err(error) = fingerprint.save(&vectors_dir) {
        tracing::error!(
            "[WQ {}] failed to save embedder fingerprint: {}",
            context.tenant_id,
            error
        );
    }
    super::observe_write_queue_phase(super::PHASE_VECTOR_SAVE, vector_start);
}

fn apply_version_receipts(
    base_path: &std::path::Path,
    tenant_id: &str,
    prepared_ops: &[PreparedWriteOperation],
) -> crate::error::Result<Option<u64>> {
    let receipts: Vec<_> = prepared_ops
        .iter()
        .flat_map(|prepared| prepared.oplog_receipts.iter().cloned())
        .collect();
    let finalized_task_ids: Vec<_> = prepared_ops
        .iter()
        .map(|prepared| prepared.task_id.as_str())
        .collect();
    let Some(committed_watermark) = receipts.last().map(|receipt| receipt.seq) else {
        return Ok(None);
    };
    let tenant_path = base_path.join(tenant_id);
    let version_store = crate::index::version_store::VersionStore::open(&tenant_path)?;
    version_store.apply_receipts_and_tasks_with_hook(
        &receipts,
        &finalized_task_ids,
        |_applied_statement_count| {
            #[cfg(any(test, feature = "fault-injection"))]
            if _applied_statement_count == 1 {
                inject_finalization_fault(
                    tenant_id,
                    FinalizationFaultPoint::AfterFirstVersionReceiptStatement,
                )
                .map_err(|error| {
                    crate::index::version_store::VersionStoreError::Injected(error.to_string())
                })?;
            }
            Ok(())
        },
    )?;
    #[cfg(any(test, feature = "fault-injection"))]
    inject_finalization_fault(
        tenant_id,
        FinalizationFaultPoint::AfterVersionTransactionBeforeCommittedSeq,
    )?;
    Ok(Some(committed_watermark))
}

pub(super) fn forget_finalized_tasks(
    base_path: &std::path::Path,
    tenant_id: &str,
    prepared_ops: &[PreparedWriteOperation],
) {
    let task_ids: Vec<_> = prepared_ops
        .iter()
        .map(|prepared| prepared.task_id.as_str())
        .collect();
    let tenant_path = base_path.join(tenant_id);
    let result = crate::index::version_store::VersionStore::open(&tenant_path)
        .and_then(|store| store.remove_finalized_tasks(&task_ids));
    if let Err(error) = result {
        tracing::warn!("[WQ {tenant_id}] failed to prune finalized admission evidence: {error}");
    }
}

fn oplog_retention() -> u64 {
    std::env::var("FLAPJACK_OPLOG_RETENTION")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1000)
}

fn truncate_committed_oplog(
    oplog: &crate::index::oplog::OpLog,
    committed_seq: u64,
) -> crate::error::Result<()> {
    let retention = oplog_retention();
    if committed_seq >= retention {
        let before_seq = committed_seq.saturating_sub(retention).saturating_add(1);
        oplog.truncate_before(before_seq)?;
    }
    Ok(())
}

/// Write the committed sequence number to disk and truncate oplog entries older
/// than the retention window (`FLAPJACK_OPLOG_RETENTION`, default 1000 entries).
fn persist_oplog_commit_state(
    oplog: Option<&Arc<crate::index::oplog::OpLog>>,
    base_path: &std::path::Path,
    tenant_id: &str,
    committed_watermark: Option<u64>,
) -> crate::error::Result<()> {
    let Some(committed_seq) = committed_watermark else {
        return Ok(());
    };
    let oplog = oplog.ok_or_else(|| {
        crate::error::FlapjackError::Io(format!(
            "committed receipts for tenant {tenant_id} have no oplog owner"
        ))
    })?;
    let tenant_path = base_path.join(tenant_id);
    crate::index::oplog::write_committed_seq(&tenant_path, committed_seq)?;
    #[cfg(any(test, feature = "fault-injection"))]
    inject_finalization_fault(
        tenant_id,
        FinalizationFaultPoint::AfterCommittedSeqBeforeOplogTruncation,
    )?;
    #[cfg(any(test, feature = "fault-injection"))]
    if FINALIZATION_FAULTS.get(tenant_id).is_some_and(|armed| {
        *armed.value() == FinalizationFaultPoint::AfterOplogTruncationBeforeAdmissionAck
    }) {
        oplog.rotate_segment_for_test()?;
    }
    truncate_committed_oplog(oplog, committed_seq)?;
    #[cfg(any(test, feature = "fault-injection"))]
    inject_finalization_fault(
        tenant_id,
        FinalizationFaultPoint::AfterOplogTruncationBeforeAdmissionAck,
    )?;
    Ok(())
}

/// Update the task status to `Succeeded` with indexed and rejected document
/// counts. Writes to both the string task ID and numeric ID entries.
pub(super) fn mark_task_succeeded(
    tasks: &Arc<dashmap::DashMap<String, TaskInfo>>,
    prepared: &PreparedWriteOperation,
) {
    let (total_rejected, rejected_documents) = prepared.finalized_rejections();
    tasks.alter(&prepared.task_id, |_, mut task| {
        task.status = TaskStatus::Succeeded;
        task.indexed_documents = prepared.indexed_document_count();
        task.rejected_documents = rejected_documents.clone();
        task.rejected_count = total_rejected;
        task
    });
    tasks.alter(&prepared.numeric_id, |_, mut task| {
        task.status = TaskStatus::Succeeded;
        task.indexed_documents = prepared.indexed_document_count();
        task.rejected_documents = rejected_documents.clone();
        task.rejected_count = total_rejected;
        task
    });
}

pub(super) fn mark_compact_task_succeeded(
    tasks: &Arc<dashmap::DashMap<String, TaskInfo>>,
    task_id: &str,
) {
    let numeric_id = numeric_task_id(tasks, task_id);
    tasks.alter(task_id, |_, mut task| {
        task.status = TaskStatus::Succeeded;
        task
    });
    tasks.alter(&numeric_id, |_, mut task| {
        task.status = TaskStatus::Succeeded;
        task
    });
}

fn numeric_task_id(tasks: &Arc<dashmap::DashMap<String, TaskInfo>>, task_id: &str) -> String {
    tasks
        .get(task_id)
        .map(|task| task.numeric_id.to_string())
        .unwrap_or_else(|| task_id.to_string())
}

/// TODO: Document apply_failed_status.
fn apply_failed_status(
    tasks: &Arc<dashmap::DashMap<String, TaskInfo>>,
    task_id: &str,
    numeric_id: &str,
    message: &str,
) {
    tasks.alter(task_id, |_, mut task| {
        task.status = TaskStatus::Failed(message.to_string());
        task.indexed_documents = 0;
        task.rejected_documents.clear();
        task.rejected_count = 0;
        task
    });
    tasks.alter(numeric_id, |_, mut task| {
        task.status = TaskStatus::Failed(message.to_string());
        task.indexed_documents = 0;
        task.rejected_documents.clear();
        task.rejected_count = 0;
        task
    });
}

/// Mark every task in a failed batch as failed before the write worker exits.
///
/// Batched write-queue errors are terminal for the worker task. Updating each
/// queued task here prevents them from getting stranded in queued/processing
/// states when a shared batch dependency (settings load, commit, reload, etc.)
/// aborts the whole flush.
pub(super) fn mark_tasks_failed(
    tasks: &Arc<dashmap::DashMap<String, TaskInfo>>,
    task_ids: &[String],
    error: &crate::error::FlapjackError,
) {
    let message = error.to_string();
    for task_id in task_ids {
        let numeric_id = numeric_task_id(tasks, task_id);
        apply_failed_status(tasks, task_id, &numeric_id, &message);
    }
}

/// Force-merge all segments into one and garbage-collect stale files.
pub(super) fn compact_segments(
    index: &Arc<crate::index::Index>,
    tasks: &Arc<dashmap::DashMap<String, TaskInfo>>,
    task_id: &str,
    writer: &mut crate::index::ManagedIndexWriter,
    tenant_id: &str,
) -> crate::error::Result<()> {
    tasks.alter(task_id, |_, mut t| {
        t.status = TaskStatus::Processing;
        t
    });

    let segment_ids = index.inner().searchable_segment_ids()?;
    tracing::info!(
        "[WQ {}] compacting {} segments",
        tenant_id,
        segment_ids.len()
    );

    let result: crate::error::Result<()> = (|| {
        if segment_ids.len() > 1 {
            let merge_future = writer.merge(&segment_ids);
            // Block on the merge (runs in Tantivy's merge thread pool).
            // wait() returns Option<SegmentMeta>; None means all docs were deleted.
            if let Err(e) = merge_future.wait() {
                tracing::error!("[WQ {}] merge failed: {}", tenant_id, e);
                return Err(crate::error::FlapjackError::Tantivy(e.to_string()));
            }
        }

        // Clean up orphaned segment files left by completed merges
        let gc_result = writer
            .garbage_collect_files()
            .wait()
            .map_err(|e| crate::error::FlapjackError::Tantivy(e.to_string()))?;
        tracing::info!(
            "[WQ {}] compact done, gc removed {} files",
            tenant_id,
            gc_result.deleted_files.len()
        );
        super::observe_write_queue_gc_removed_files(
            tenant_id,
            gc_result.deleted_files.len() as u64,
        );

        index.reader().reload()?;
        index.invalidate_searchable_paths_cache();
        if let Some(observation) = record_segment_health(tenant_id, index) {
            super::observe_write_queue_settled_index_bytes(tenant_id, &observation);
        }
        Ok(())
    })();

    if let Err(error) = &result {
        mark_tasks_failed(tasks, &[task_id.to_string()], error);
    }

    result
}

pub(super) fn record_segment_health(
    tenant_id: &str,
    index: &Arc<crate::index::Index>,
) -> Option<super::segment_observation::SegmentObservation> {
    match super::segment_observation::observe_segments(index) {
        Ok(observation) => {
            super::observe_write_queue_segment_health(tenant_id, &observation);
            Some(observation)
        }
        Err(error) => {
            tracing::warn!(
                "[WQ {}] failed to observe segment health: {}",
                tenant_id,
                error
            );
            None
        }
    }
}

/// Get or create a VectorIndex for a tenant. Uses actual vector length for dimensions.
/// If the entry already exists in the DashMap, returns it. Otherwise creates a new one.
#[cfg(feature = "vector-search")]
pub(super) fn get_or_create_vector_index(
    vector_indices: &dashmap::DashMap<
        String,
        Arc<std::sync::RwLock<crate::vector::index::VectorIndex>>,
    >,
    tenant_id: &str,
    dimensions: usize,
) -> Arc<std::sync::RwLock<crate::vector::index::VectorIndex>> {
    if let Some(existing) = vector_indices.get(tenant_id) {
        return Arc::clone(&existing);
    }
    let vi = crate::vector::index::VectorIndex::new(dimensions, usearch::ffi::MetricKind::Cos)
        .expect("failed to create VectorIndex");
    let arc = Arc::new(std::sync::RwLock::new(vi));
    vector_indices.insert(tenant_id.to_string(), Arc::clone(&arc));
    arc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial(write_queue_commit_failure_hook)]
    fn commit_failure_hooks_are_isolated_by_tenant() {
        let _tenant_a_hook = fail_next_commit_for_test("fault_hook_tenant_a");
        let _tenant_b_hook = fail_next_commit_for_test("fault_hook_tenant_b");

        assert!(
            inject_finalization_fault(
                "fault_hook_tenant_a",
                FinalizationFaultPoint::BeforeTantivyCommit,
            )
            .is_err(),
            "arming tenant B must not overwrite tenant A's pending failure"
        );
        assert!(
            inject_finalization_fault(
                "fault_hook_tenant_b",
                FinalizationFaultPoint::BeforeTantivyCommit,
            )
            .is_err(),
            "each tenant must retain its own one-shot failure"
        );
        assert!(
            inject_finalization_fault(
                "fault_hook_tenant_a",
                FinalizationFaultPoint::BeforeTantivyCommit,
            )
            .is_ok(),
            "tenant-scoped failures must be consumed exactly once"
        );
    }

    #[test]
    fn replication_origin_proof_finalization_precedes_committed_seq() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let tenant_id = "durable_finalization";
        let tenant_path = temp_dir.path().join(tenant_id);
        std::fs::create_dir_all(&tenant_path).unwrap();
        let schema = crate::index::schema::Schema::builder().build();
        let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
        let oplog = Arc::new(
            crate::index::oplog::OpLog::open(&tenant_path.join("oplog"), tenant_id, "local-node")
                .unwrap(),
        );
        let receipts = oplog
            .append_operations_for_task(
                "task-1",
                vec![
                    crate::index::oplog::OpLogOperation::replicated(
                        "upsert",
                        serde_json::json!({"objectID": "doc-a", "body": {"objectID": "doc-a"}}),
                        crate::index::oplog::OpLogOrigin::new(5000, "node-a").with_origin_seq(50),
                    ),
                    crate::index::oplog::OpLogOperation::replicated(
                        "delete",
                        serde_json::json!({"objectID": "doc-b"}),
                        crate::index::oplog::OpLogOrigin::new(6000, "node-b").with_origin_seq(60),
                    ),
                ],
            )
            .unwrap();
        let mut prepared = PreparedWriteOperation::new("task-1".to_string(), "1".to_string());
        prepared.oplog_receipts = receipts;
        let tasks = Arc::new(dashmap::DashMap::new());
        let admission_store = Arc::new(
            super::super::admission::WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap(),
        );
        let facet_cache = Arc::new(dashmap::DashMap::new());
        #[cfg(feature = "vector-search")]
        let vector_ctx = super::super::VectorWriteContext::new(Arc::new(dashmap::DashMap::new()));
        let context = WriteFinalizationContext {
            tenant_id,
            index: &index,
            tasks: &tasks,
            base_path: temp_dir.path(),
            oplog: Some(&oplog),
            admission_store: &admission_store,
            facet_cache: &facet_cache,
            #[cfg(feature = "vector-search")]
            vector_ctx: &vector_ctx,
            #[cfg(feature = "vector-search")]
            embedder_configs: &[],
        };

        finalize_committed_batch(&context, &[prepared], 0).unwrap();

        let version_store = crate::index::version_store::VersionStore::open(&tenant_path).unwrap();
        assert_eq!(
            version_store.get("doc-a").unwrap(),
            Some(
                crate::index::version_store::VersionRecord::new(5000, "node-a", false, 1,)
                    .with_origin_proof(
                        50,
                        crate::index::oplog::upsert_effect_digest(
                            &crate::types::Document::from_json(
                                &serde_json::json!({"objectID": "doc-a"}),
                            )
                            .unwrap(),
                        ),
                    )
            )
        );
        assert_eq!(
            version_store.get("doc-b").unwrap(),
            Some(
                crate::index::version_store::VersionRecord::new(6000, "node-b", true, 2,)
                    .with_origin_proof(60, crate::index::oplog::delete_effect_digest("doc-b"))
            )
        );
        assert_eq!(
            crate::index::oplog::read_committed_seq(&tenant_path),
            2,
            "the durable version transaction must complete before the watermark advances"
        );
        assert_eq!(
            oplog
                .read_since(0)
                .unwrap()
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![1, 2],
            "freshly committed oplog rows must remain retained"
        );
    }
}
