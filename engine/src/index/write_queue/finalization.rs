use std::sync::Arc;

use crate::types::{TaskInfo, TaskStatus};

use super::{PreparedWriteDocument, PreparedWriteOperation, WriteFinalizationContext};

pub(crate) const PERSISTED_VECTORS_DIR: &str = "vectors";

pub(super) fn write_valid_documents(
    writer: &mut crate::index::ManagedIndexWriter,
    valid_docs: &[PreparedWriteDocument],
) -> crate::error::Result<Vec<(String, serde_json::Value)>> {
    let mut valid_docs_json = Vec::new();
    for (doc_id, doc_json, tantivy_doc) in valid_docs {
        writer.add_document(tantivy_doc.clone())?;
        valid_docs_json.push((doc_id.clone(), doc_json.clone()));
    }
    Ok(valid_docs_json)
}

/// Append upsert and delete operations to the oplog as a single batch. No-ops
/// if the oplog is `None` or if the batch is empty.
pub(super) fn append_batch_to_oplog(
    oplog: Option<&Arc<crate::index::oplog::OpLog>>,
    valid_docs_json: &[(String, serde_json::Value)],
    deleted_ids: &[String],
    tenant_id: &str,
) {
    let Some(oplog) = oplog else {
        return;
    };

    let mut batch_ops: Vec<(String, serde_json::Value)> = Vec::new();
    for (doc_id, doc_json) in valid_docs_json {
        batch_ops.push((
            "upsert".into(),
            serde_json::json!({"objectID": doc_id, "body": doc_json}),
        ));
    }
    for deleted_id in deleted_ids {
        batch_ops.push(("delete".into(), serde_json::json!({"objectID": deleted_id})));
    }
    if batch_ops.is_empty() {
        return;
    }

    if let Err(error) = oplog.append_batch(&batch_ops) {
        tracing::error!("[WQ {}] oplog append failed: {}", tenant_id, error);
    }
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
    let commit_start = std::time::Instant::now();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.commit())) {
        Ok(Ok(_opstamp)) => {
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

pub(super) fn finalize_committed_batch(
    context: &WriteFinalizationContext<'_>,
    prepared_ops: &[PreparedWriteOperation],
    build_secs: u64,
) -> crate::error::Result<()> {
    let phase_start = std::time::Instant::now();
    persist_index_metadata(context.base_path, context.tenant_id, build_secs);
    refresh_search_state(context.index, context.facet_cache, context.tenant_id)?;
    #[cfg(feature = "vector-search")]
    save_vector_index(context, prepared_ops);
    for prepared in prepared_ops {
        update_lww_state(context.lww_map, context.tenant_id, prepared);
    }
    persist_oplog_commit_state(context.oplog, context.base_path, context.tenant_id);
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
    facet_cache.retain(|cache_key, _| !cache_key.starts_with(&format!("{}:", tenant_id)));
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

    if let Err(error) = guard.save(&vectors_dir) {
        tracing::error!(
            "[WQ {}] failed to save vector index: {}",
            context.tenant_id,
            error
        );
        return;
    }

    if context.embedder_configs.is_empty() {
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
}

/// Record the current timestamp and node ID in the LWW map for every primary
/// upsert and delete, enabling last-writer-wins conflict resolution during
/// replication.
fn update_lww_state(
    lww_map: &super::super::LwwMap,
    tenant_id: &str,
    prepared: &PreparedWriteOperation,
) {
    if prepared.primary_upsert_ids.is_empty() && prepared.primary_delete_ids.is_empty() {
        return;
    }

    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let node_id = crate::index::configured_node_id();
    let tenant_map = lww_map.entry(tenant_id.to_string()).or_default();
    for doc_id in &prepared.primary_upsert_ids {
        tenant_map.insert(doc_id.clone(), (now_ts, node_id.clone()));
    }
    for doc_id in &prepared.primary_delete_ids {
        tenant_map.insert(doc_id.clone(), (now_ts, node_id.clone()));
    }
}

/// Write the committed sequence number to disk and truncate oplog entries older
/// than the retention window (`FLAPJACK_OPLOG_RETENTION`, default 1000 entries).
fn persist_oplog_commit_state(
    oplog: Option<&Arc<crate::index::oplog::OpLog>>,
    base_path: &std::path::Path,
    tenant_id: &str,
) {
    let Some(oplog) = oplog else {
        return;
    };

    let seq = oplog.current_seq();
    let tenant_path = base_path.join(tenant_id);
    if let Err(error) = crate::index::oplog::write_committed_seq(&tenant_path, seq) {
        tracing::error!(
            "[WQ {}] failed to write committed_seq: {}",
            tenant_id,
            error
        );
    }

    let retention = std::env::var("FLAPJACK_OPLOG_RETENTION")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1000);
    if seq > retention {
        let _ = oplog.truncate_before(seq - retention);
    }
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

fn numeric_task_id(tasks: &Arc<dashmap::DashMap<String, TaskInfo>>, task_id: &str) -> String {
    tasks
        .get(task_id)
        .map(|task| task.numeric_id.to_string())
        .unwrap_or_else(|| task_id.to_string())
}

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

        index.reader().reload()?;
        index.invalidate_searchable_paths_cache();
        Ok(())
    })();

    let numeric_id = numeric_task_id(tasks, task_id);

    let status = match &result {
        Ok(()) => TaskStatus::Succeeded,
        Err(e) => TaskStatus::Failed(e.to_string()),
    };
    tasks.alter(task_id, |_, mut t| {
        t.status = status.clone();
        t
    });
    tasks.alter(&numeric_id, |_, mut t| {
        t.status = status;
        t
    });

    result
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
    fn update_lww_state_no_primary_changes_keeps_map_unchanged() {
        let lww_map: super::super::super::LwwMap = Arc::new(dashmap::DashMap::new());
        let prepared = PreparedWriteOperation::new("task-1".to_string(), "1".to_string());

        update_lww_state(&lww_map, "tenant_a", &prepared);

        assert!(
            lww_map.get("tenant_a").is_none(),
            "tenants with no primary upsert/delete ids should not create LWW state entries"
        );
    }
    #[test]
    fn update_lww_state_tracks_primary_upserts_and_deletes() {
        let lww_map: super::super::super::LwwMap = Arc::new(dashmap::DashMap::new());
        let mut prepared = PreparedWriteOperation::new("task-2".to_string(), "2".to_string());
        prepared.primary_upsert_ids = vec!["doc_a".to_string(), "doc_b".to_string()];
        prepared.primary_delete_ids = vec!["doc_c".to_string()];

        update_lww_state(&lww_map, "tenant_b", &prepared);

        let tenant_map = lww_map
            .get("tenant_b")
            .expect("expected tenant LWW map after primary changes");
        assert_eq!(
            tenant_map.len(),
            3,
            "all upserts/deletes should be recorded"
        );

        let a = tenant_map.get("doc_a").expect("missing doc_a");
        let b = tenant_map.get("doc_b").expect("missing doc_b");
        let c = tenant_map.get("doc_c").expect("missing doc_c");

        assert!(a.value().0 > 0, "timestamp should be populated");
        assert_eq!(
            a.value().0,
            b.value().0,
            "all entries from one batch should share a consistent timestamp"
        );
        assert_eq!(
            b.value().0,
            c.value().0,
            "delete entries should use the same batch timestamp as upserts"
        );
        assert!(
            !a.value().1.is_empty(),
            "node id should always be recorded in LWW entries"
        );
        assert_eq!(a.value().1, b.value().1);
        assert_eq!(b.value().1, c.value().1);
    }
}
