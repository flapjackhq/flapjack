use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;

use super::super::indices::clear_index_impl;
use super::super::replicas::sync_add_documents_to_standard_replicas;
use super::super::AppState;
use super::{
    check_record_size, delete_documents_durable_task_aware, merge_partial_update,
    sync_delete_documents_to_standard_replicas_task_aware, AddDocumentsError,
};
use crate::dto::{AddDocumentsRequest, AddDocumentsResponse, BatchOperation};
use crate::pause_registry::check_not_paused;
use flapjack::error::FlapjackError;
use flapjack::types::Document;

#[derive(Default)]
struct PendingBatchOperations {
    documents: Vec<Document>,
    deletes: Vec<String>,
    explicit_delete_count: u64,
    operation_count: u64,
}

struct DrainedBatchOperations {
    documents: Vec<Document>,
    deletes: Vec<String>,
    explicit_delete_count: u64,
    operation_count: u64,
}

impl PendingBatchOperations {
    fn is_empty(&self) -> bool {
        self.documents.is_empty() && self.deletes.is_empty()
    }

    fn push_document(&mut self, document: Document) {
        self.documents.push(document);
    }

    fn push_explicit_delete(&mut self, object_id: String) {
        self.deletes.push(object_id);
        self.explicit_delete_count += 1;
    }

    fn push_replacement_delete(&mut self, object_id: String) {
        self.deletes.push(object_id);
    }

    fn record_operation(&mut self) {
        self.operation_count += 1;
    }

    fn drain(&mut self) -> DrainedBatchOperations {
        let drained = DrainedBatchOperations {
            documents: std::mem::take(&mut self.documents),
            deletes: std::mem::take(&mut self.deletes),
            explicit_delete_count: self.explicit_delete_count,
            operation_count: self.operation_count,
        };
        self.explicit_delete_count = 0;
        self.operation_count = 0;
        drained
    }
}

#[derive(Default)]
struct BatchExecutionState {
    object_ids: Vec<String>,
    pending: PendingBatchOperations,
    last_task_id: Option<i64>,
}

impl BatchExecutionState {
    fn push_object_id(&mut self, object_id: String) {
        self.object_ids.push(object_id);
    }

    fn queue_delete(
        &mut self,
        action: &str,
        body: HashMap<String, serde_json::Value>,
    ) -> Result<(), FlapjackError> {
        let object_id = required_object_id(&body, action)?.to_string();
        self.push_object_id(object_id.clone());
        self.pending.push_explicit_delete(object_id);
        self.pending.record_operation();
        Ok(())
    }

    /// Queues a partial update by merging new fields into the existing document.
    fn queue_partial_update(
        &mut self,
        state: &Arc<AppState>,
        index_name: &str,
        body: HashMap<String, serde_json::Value>,
        create_if_not_exists: bool,
    ) -> Result<(), FlapjackError> {
        let object_id = required_object_id(&body, "partialUpdateObject")?.to_string();
        self.push_object_id(object_id.clone());
        self.pending.record_operation();

        let existing = state.manager.get_document(index_name, &object_id)?;
        if existing.is_some() {
            self.pending.push_replacement_delete(object_id.clone());
        }

        let body_map: serde_json::Map<String, serde_json::Value> = body.into_iter().collect();
        if let Some(doc) =
            merge_partial_update(existing, &object_id, &body_map, create_if_not_exists)?
        {
            self.pending.push_document(doc);
        }

        Ok(())
    }

    fn queue_update(
        &mut self,
        mut body: HashMap<String, serde_json::Value>,
    ) -> Result<(), FlapjackError> {
        let object_id = required_object_id(&body, "updateObject")?.to_string();
        self.push_object_id(object_id.clone());
        self.pending.record_operation();
        body.remove("objectID");
        body.remove("id");
        self.pending
            .push_document(document_from_body(body, object_id)?);
        Ok(())
    }

    fn queue_add(
        &mut self,
        mut body: HashMap<String, serde_json::Value>,
    ) -> Result<(), FlapjackError> {
        let explicit_id = body
            .remove("objectID")
            .or_else(|| body.remove("id"))
            .and_then(|value| value.as_str().map(String::from));
        // Auto-ID path mirrors `add_record_auto_id`: derive a stable content
        // hash so client retries upsert rather than duplicate (ADR 0005).
        let object_id = explicit_id.unwrap_or_else(|| super::auto_id_from_body(body.iter()));
        self.push_object_id(object_id.clone());
        self.pending.record_operation();
        self.pending
            .push_document(document_from_body(body, object_id)?);
        Ok(())
    }

    async fn clear(
        &mut self,
        state: &Arc<AppState>,
        index_name: &str,
    ) -> Result<(), AddDocumentsError> {
        self.flush_pending(state, index_name).await?;
        let index_name = index_name.to_string();
        self.last_task_id = Some(clear_index_impl(state, &index_name).await?);
        Ok(())
    }

    /// Applies a single batch write operation (add, update, partial update, or delete).
    async fn apply_operation(
        &mut self,
        state: &Arc<AppState>,
        index_name: &str,
        operation: BatchOperation,
    ) -> Result<(), AddDocumentsError> {
        let BatchOperation {
            action,
            index_name: _,
            body,
            create_if_not_exists,
        } = operation;
        let body = operation_body(&action, body)?;

        validate_operation_body_size(&action, &body)?;

        match action.as_str() {
            "deleteObject" | "delete" => self.queue_delete(&action, body).map_err(Into::into),
            "partialUpdateObject" => self
                .queue_partial_update(
                    state,
                    index_name,
                    body,
                    create_if_not_exists.unwrap_or(true),
                )
                .map_err(Into::into),
            "partialUpdateObjectNoCreate" => self
                .queue_partial_update(state, index_name, body, false)
                .map_err(Into::into),
            "updateObject" => self.queue_update(body).map_err(Into::into),
            "addObject" => self.queue_add(body).map_err(Into::into),
            "clear" => self.clear(state, index_name).await,
            _ => Err(Into::into(FlapjackError::InvalidQuery(format!(
                "Unsupported batch action: {}",
                action
            )))),
        }
    }

    async fn flush_pending(
        &mut self,
        state: &Arc<AppState>,
        index_name: &str,
    ) -> Result<(), AddDocumentsError> {
        if let Some(task_id) =
            flush_pending_batch_operations(state, index_name, &mut self.pending).await?
        {
            self.last_task_id = Some(task_id);
        }
        Ok(())
    }

    fn into_response(
        self,
        state: &Arc<AppState>,
        index_name: &str,
    ) -> Result<AddDocumentsResponse, FlapjackError> {
        let task_id = match self.last_task_id {
            Some(task_id) => task_id,
            None => state.manager.make_noop_task(index_name)?.numeric_id,
        };

        Ok(AddDocumentsResponse::Algolia {
            task_id,
            object_ids: self.object_ids,
        })
    }
}

fn batch_operations_from_request(req: AddDocumentsRequest) -> Vec<BatchOperation> {
    match req {
        AddDocumentsRequest::Batch { requests } => requests,
        AddDocumentsRequest::Legacy { documents: docs } => docs
            .into_iter()
            .map(|body| BatchOperation {
                action: "addObject".to_string(),
                index_name: None,
                body: Some(body),
                create_if_not_exists: None,
            })
            .collect(),
    }
}

fn validate_batch_size(operation_count: usize) -> Result<(), FlapjackError> {
    let max_batch_size: usize = std::env::var("FLAPJACK_MAX_BATCH_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);
    if operation_count > max_batch_size {
        return Err(FlapjackError::BatchTooLarge {
            size: operation_count,
            max: max_batch_size,
        });
    }
    Ok(())
}

fn operation_body(
    action: &str,
    body: Option<HashMap<String, serde_json::Value>>,
) -> Result<HashMap<String, serde_json::Value>, FlapjackError> {
    match (action, body) {
        ("clear", maybe_body) => Ok(maybe_body.unwrap_or_default()),
        (_, Some(body)) => Ok(body),
        _ => Err(FlapjackError::InvalidQuery(format!(
            "Missing body in {}",
            action
        ))),
    }
}

fn validate_operation_body_size(
    action: &str,
    body: &HashMap<String, serde_json::Value>,
) -> Result<(), FlapjackError> {
    if matches!(action, "deleteObject" | "delete" | "clear") {
        return Ok(());
    }
    check_record_size(body)
}

fn required_object_id<'a>(
    body: &'a HashMap<String, serde_json::Value>,
    action: &str,
) -> Result<&'a str, FlapjackError> {
    body.get("objectID")
        .or_else(|| body.get("id"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| FlapjackError::InvalidQuery(format!("Missing objectID in {}", action)))
}

fn document_from_body(
    body: HashMap<String, serde_json::Value>,
    object_id: String,
) -> Result<Document, FlapjackError> {
    let mut json_obj = serde_json::Map::new();
    json_obj.insert("_id".to_string(), serde_json::Value::String(object_id));
    for (key, value) in body {
        json_obj.insert(key, value);
    }
    Document::from_json(&serde_json::Value::Object(json_obj))
}

/// Commit accumulated document adds and deletes for a single index, replicate the resulting oplog entries, update usage counters, and reset the pending buffers.
///
/// # Returns
///
/// The task ID of the committed work, or `None` if both buffers were empty.
async fn flush_pending_batch_operations(
    state: &Arc<AppState>,
    index_name: &str,
    pending: &mut PendingBatchOperations,
) -> Result<Option<i64>, AddDocumentsError> {
    if pending.is_empty() {
        return Ok(None);
    }

    let DrainedBatchOperations {
        documents,
        deletes,
        explicit_delete_count,
        operation_count,
    } = pending.drain();

    // Capture oplog seq before write so we can replicate only the new ops.
    let pre_seq = state
        .manager
        .get_oplog(index_name)
        .map(|ol| ol.current_seq())
        .unwrap_or(0);

    let task_id = if documents.is_empty() && !deletes.is_empty() {
        let delete_task =
            delete_documents_durable_task_aware(state, index_name, deletes.clone()).await?;
        sync_delete_documents_to_standard_replicas_task_aware(
            state,
            index_name,
            &deletes,
            delete_task.numeric_id,
        )
        .await?;
        // Deletes committed synchronously — replicate immediately.
        trigger_replication(state, index_name, pre_seq, false);
        state
            .manager
            .make_noop_task(index_name)
            .map_err(AddDocumentsError::from)?
            .numeric_id
    } else if !deletes.is_empty() {
        // Batch has explicit deletes (e.g. partialUpdateObject) — delete first, then add.
        let delete_task =
            delete_documents_durable_task_aware(state, index_name, deletes.clone()).await?;
        sync_delete_documents_to_standard_replicas_task_aware(
            state,
            index_name,
            &deletes,
            delete_task.numeric_id,
        )
        .await?;
        let task = state
            .manager
            .add_documents_durable(index_name, documents.clone())
            .await
            .map_err(AddDocumentsError::from)?;
        sync_add_documents_to_standard_replicas(state, index_name, &documents)
            .await
            .map_err(AddDocumentsError::from)?;
        // Adds are durable — the write queue has committed before we read the oplog.
        trigger_replication(state, index_name, pre_seq, true);
        task.numeric_id
    } else {
        // addObject/updateObject — always upsert (Algolia replaces if objectID exists).
        let task = state
            .manager
            .add_documents_durable(index_name, documents.clone())
            .await
            .map_err(AddDocumentsError::from)?;
        sync_add_documents_to_standard_replicas(state, index_name, &documents)
            .await
            .map_err(AddDocumentsError::from)?;
        trigger_replication(state, index_name, pre_seq, true);
        task.numeric_id
    };

    // Keep usage accounting behavior aligned with the prior single-flush implementation.
    let entry = state
        .usage_counters
        .entry(index_name.to_string())
        .or_default();
    let indexed_count = operation_count.saturating_sub(explicit_delete_count);
    if indexed_count > 0 {
        entry
            .documents_indexed_total
            .fetch_add(indexed_count, std::sync::atomic::Ordering::Relaxed);
    }
    if explicit_delete_count > 0 {
        entry
            .documents_deleted_total
            .fetch_add(explicit_delete_count, std::sync::atomic::Ordering::Relaxed);
    }

    Ok(Some(task_id))
}

/// Execute a single-index batch of document operations (add, update, partial update, delete, clear), enforcing per-record size limits, flushing pending writes, and replicating changes to standard replicas.
///
/// # Arguments
///
/// * `index_name` - Target index for all operations in this batch.
/// * `req` - Batch or legacy add-documents request body.
pub(super) async fn add_documents_batch_impl(
    State(state): State<Arc<AppState>>,
    index_name: String,
    req: AddDocumentsRequest,
) -> Result<Json<AddDocumentsResponse>, AddDocumentsError> {
    use super::super::replicas::reject_writes_to_virtual_replica;

    reject_writes_to_virtual_replica(&state, &index_name)?;
    state.manager.create_tenant(&index_name)?;

    let operations = batch_operations_from_request(req);
    validate_batch_size(operations.len())?;
    // Keep production logs proportional to HTTP requests, not record count.
    // The previous event lived inside `apply_operation` and produced one line
    // per object (2,148,002 lines in the July 25 reference specimen).
    tracing::info!(
        index_name = %index_name,
        operation_count = operations.len(),
        "Batch request"
    );

    let mut execution = BatchExecutionState::default();
    for operation in operations {
        execution
            .apply_operation(&state, &index_name, operation)
            .await?;
    }
    execution.flush_pending(&state, &index_name).await?;

    Ok(Json(execution.into_response(&state, &index_name)?))
}

/// Route a multi-index batch request by grouping operations by `indexName`, delegating each group to `add_documents_batch_impl`, and returning a combined response with per-index task IDs.
pub(super) async fn add_documents_multi_index_impl(
    State(state): State<Arc<AppState>>,
    req: AddDocumentsRequest,
) -> Result<Json<AddDocumentsResponse>, AddDocumentsError> {
    let operations = match req {
        AddDocumentsRequest::Batch { requests } => requests,
        AddDocumentsRequest::Legacy { documents: docs } => docs
            .into_iter()
            .map(|body| BatchOperation {
                action: "addObject".to_string(),
                index_name: None,
                body: Some(body),
                create_if_not_exists: None,
            })
            .collect(),
    };

    let mut grouped_operations: HashMap<String, Vec<BatchOperation>> = HashMap::new();
    let mut index_order: Vec<String> = Vec::new();

    for mut operation in operations {
        let target_index = operation
            .index_name
            .clone()
            .ok_or_else(|| FlapjackError::InvalidQuery("Missing indexName".to_string()))?;
        check_not_paused(&state.paused_indexes, &target_index)?;

        if !grouped_operations.contains_key(&target_index) {
            index_order.push(target_index.clone());
            grouped_operations.insert(target_index.clone(), Vec::new());
        }

        operation.index_name = None;
        grouped_operations
            .get_mut(&target_index)
            .expect("group must exist")
            .push(operation);
    }

    let mut all_object_ids: Vec<String> = Vec::new();
    let mut task_ids: HashMap<String, i64> = HashMap::new();

    for target_index in index_order {
        let requests = grouped_operations.remove(&target_index).unwrap_or_default();
        let response = add_documents_batch_impl(
            State(state.clone()),
            target_index.clone(),
            AddDocumentsRequest::Batch { requests },
        )
        .await?;

        match response.0 {
            AddDocumentsResponse::Algolia {
                task_id,
                object_ids,
            } => {
                all_object_ids.extend(object_ids);
                task_ids.insert(target_index, task_id);
            }
            AddDocumentsResponse::MultiIndexAlgolia { .. } => {
                return Err(AddDocumentsError::from(FlapjackError::InvalidQuery(
                    "Unexpected multi-index response while processing grouped batch".to_string(),
                )));
            }
            AddDocumentsResponse::Legacy { .. } => {
                return Err(AddDocumentsError::from(FlapjackError::InvalidQuery(
                    "Unexpected legacy response while processing grouped batch".to_string(),
                )));
            }
        }
    }

    Ok(Json(AddDocumentsResponse::MultiIndexAlgolia {
        task_id: task_ids,
        object_ids: all_object_ids,
    }))
}

/// Spawn a background task to replicate newly committed ops to peers.
///
/// `needs_delay`: if true, waits 300ms for the write queue to flush before
/// reading the oplog. Set false when writes are already committed (sync path).
pub(super) fn trigger_replication(
    state: &Arc<AppState>,
    index_name: &str,
    pre_seq: u64,
    needs_delay: bool,
) {
    let repl_mgr = match &state.replication_manager {
        Some(r) => Arc::clone(r),
        None => return,
    };
    let mgr = Arc::clone(&state.manager);
    let tenant = index_name.to_string();

    tokio::spawn(async move {
        if needs_delay {
            // Write queue flushes every ~100ms; 300ms gives a comfortable margin.
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        }
        if let Some(oplog) = mgr.get_oplog(&tenant) {
            match oplog.read_since(pre_seq) {
                Ok(ops) if !ops.is_empty() => {
                    repl_mgr.replicate_ops(&tenant, ops).await;
                }
                Ok(_) => {} // Nothing new (empty write or timing miss — catch-up handles it)
                Err(e) => tracing::warn!("[REPL] failed to read oplog for {}: {}", tenant, e),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use tracing::instrument::WithSubscriber;
    use tracing::subscriber::Interest;
    use tracing::{Event, Metadata, Subscriber};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{registry::LookupSpan, Layer};

    #[derive(Clone, Default)]
    struct CapturedBatchEvents(Arc<Mutex<Vec<CapturedBatchEvent>>>);

    #[derive(Clone, Debug, Default)]
    struct CapturedBatchEvent {
        message: Option<String>,
        index_name: Option<String>,
        operation_count: Option<u64>,
    }

    impl CapturedBatchEvents {
        fn events(&self) -> Vec<CapturedBatchEvent> {
            self.0.lock().unwrap().clone()
        }
    }

    impl<S> Layer<S> for CapturedBatchEvents
    where
        S: Subscriber + for<'span> LookupSpan<'span>,
    {
        fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
            // Other tests install thread-local dispatchers in the same process. Re-evaluate
            // this capture layer for each event so a callsite first seen elsewhere cannot
            // remain disabled, without rebuilding tracing's process-wide interest cache.
            Interest::sometimes()
        }

        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            let mut captured = CapturedBatchEvent::default();
            event.record(&mut captured);
            self.0.lock().unwrap().push(captured);
        }
    }

    impl tracing::field::Visit for CapturedBatchEvent {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            match field.name() {
                "message" => self.message = Some(value.to_string()),
                "index_name" => self.index_name = Some(value.to_string()),
                _ => {}
            }
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            if field.name() == "operation_count" {
                self.operation_count = Some(value);
            }
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            let value = format!("{value:?}").trim_matches('"').to_string();
            match field.name() {
                "message" if self.message.is_none() => self.message = Some(value),
                "index_name" if self.index_name.is_none() => self.index_name = Some(value),
                _ => {}
            }
        }
    }

    fn legacy_doc(id: &str) -> HashMap<String, serde_json::Value> {
        HashMap::from([(
            "objectID".to_string(),
            serde_json::Value::String(id.to_string()),
        )])
    }

    fn document(id: &str) -> Document {
        Document::from_json(&serde_json::json!({ "_id": id })).expect("valid test document")
    }

    fn three_adds_request() -> AddDocumentsRequest {
        AddDocumentsRequest::Batch {
            requests: vec![
                BatchOperation {
                    action: "addObject".to_string(),
                    index_name: None,
                    body: Some(legacy_doc("one")),
                    create_if_not_exists: None,
                },
                BatchOperation {
                    action: "addObject".to_string(),
                    index_name: None,
                    body: Some(legacy_doc("two")),
                    create_if_not_exists: None,
                },
                BatchOperation {
                    action: "addObject".to_string(),
                    index_name: None,
                    body: Some(legacy_doc("three")),
                    create_if_not_exists: None,
                },
            ],
        }
    }

    #[test]
    fn batch_operations_from_legacy_request_wraps_docs_as_add_object_actions() {
        let operations = batch_operations_from_request(AddDocumentsRequest::Legacy {
            documents: vec![legacy_doc("one"), legacy_doc("two")],
        });

        assert_eq!(operations.len(), 2);
        assert_eq!(operations[0].action, "addObject");
        assert_eq!(
            operations[0]
                .body
                .as_ref()
                .and_then(|body| body.get("objectID"))
                .and_then(|value| value.as_str()),
            Some("one")
        );
        assert_eq!(operations[1].action, "addObject");
    }

    #[test]
    fn pending_batch_operations_drain_resets_counts() {
        let mut pending = PendingBatchOperations::default();
        pending.push_explicit_delete("delete-me".to_string());
        pending.push_replacement_delete("replace-me".to_string());
        pending.push_document(document("keep-me"));
        pending.record_operation();

        let drained = pending.drain();

        assert_eq!(drained.deletes, vec!["delete-me", "replace-me"]);
        assert_eq!(drained.documents.len(), 1);
        assert_eq!(drained.explicit_delete_count, 1);
        assert_eq!(drained.operation_count, 1);
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn multi_object_request_emits_one_batch_info_summary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let captured = CapturedBatchEvents::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let request = three_adds_request();

        let result = add_documents_batch_impl(State(state), "log_contract".to_string(), request)
            .with_subscriber(subscriber)
            .await;
        assert!(result.is_ok(), "batch fixture must commit successfully");

        let events = captured.events();
        let summaries: Vec<_> = events
            .iter()
            .filter(|event| event.message.as_deref() == Some("Batch request"))
            .collect();
        assert_eq!(
            summaries.len(),
            1,
            "one HTTP batch must emit exactly one INFO summary: {events:?}",
        );
        let summary = summaries[0];
        assert_eq!(
            summary.operation_count,
            Some(3),
            "summary must retain useful batch cardinality: {events:?}",
        );
        assert_eq!(
            summary.index_name.as_deref(),
            Some("log_contract"),
            "summary must retain index context: {events:?}",
        );
        assert!(
            !events
                .iter()
                .any(|event| event.message.as_deref() == Some("Batch operation:")),
            "the prior per-object INFO event must not survive: {events:?}",
        );
    }
}
