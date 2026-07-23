mod batch;

use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    Extension, Json,
};
use std::sync::Arc;

use super::replicas::{
    is_virtual_settings_only_index, reject_writes_to_virtual_replica,
    sync_add_documents_to_standard_replicas, sync_delete_documents_to_standard_replicas,
};
use super::AppState;
use crate::auth::AuthenticatedAppId;
use crate::dto::{
    AddDocumentsRequest, AddDocumentsResponse, BatchWriteResponse, DeleteByQueryRequest,
    DeleteObjectResponse, GetObjectsRequest, GetObjectsResponse, PartialUpdateObjectResponse,
    PutObjectResponse, SaveObjectResponse,
};
use crate::filter_parser::parse_filter;
use crate::pause_registry::check_not_paused;
use flapjack::error::FlapjackError;
use flapjack::index::SearchOptions;
use flapjack::types::{Document, FieldValue, TaskInfo};

use flapjack::types::field_value_to_json_value;

/// Maximum serialized JSON size for a single record, in bytes.
///
/// Defaults to 100 KB. Configurable at startup via `FLAPJACK_MAX_RECORD_BYTES`.
/// Algolia's documented limit is 10 KB; 100 KB is more permissive while still
/// preventing individual documents from consuming unreasonable memory.
pub(super) fn max_record_bytes() -> usize {
    std::env::var("FLAPJACK_MAX_RECORD_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(102_400)
}

pub(super) fn check_record_size<T: serde::Serialize>(doc: &T) -> Result<(), FlapjackError> {
    let size = serde_json::to_vec(doc).map(|v| v.len()).unwrap_or(0);
    let limit = max_record_bytes();
    if size > limit {
        return Err(FlapjackError::DocumentTooLarge { size, max: limit });
    }
    Ok(())
}

/// Derive a deterministic 32-character hex object ID from a document body.
///
/// PL-8 / ADR 0005: nginx does not retry POST writes across upstream failover,
/// so client retries of the same body must upsert (not duplicate). Hashing the
/// canonical body keeps that property without persisting any per-request state.
///
/// Callers must remove the client-supplied `objectID` / `id` fields before
/// invoking this helper — both the single-record and batch auto-ID paths
/// already do so, and including them would tie the hash to fields that this
/// helper is explicitly replacing.
///
/// Canonical form: copy entries into a `BTreeMap` so iteration is sorted by
/// key, serialize via `serde_json::to_vec`, then take the SHA-256 digest and
/// keep the first 32 hex characters (matching UUID length).
pub(crate) fn auto_id_from_body<'a, I, V>(entries: I) -> String
where
    I: IntoIterator<Item = (&'a String, V)>,
    V: serde::Serialize + 'a,
{
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    let canonical: BTreeMap<&str, serde_json::Value> = entries
        .into_iter()
        .map(|(k, v)| {
            (
                k.as_str(),
                serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
            )
        })
        .collect();
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{:02x}", byte);
    }
    hex.truncate(32);
    hex
}

/// Apply a built-in partial update operation (Increment, Decrement, Add, Remove, AddUnique).
/// Returns the new FieldValue for the field, or None if the operation is invalid.
fn apply_operation(
    existing: Option<&FieldValue>,
    operation: &str,
    value: &serde_json::Value,
) -> Option<FieldValue> {
    match operation {
        "Increment" | "IncrementFrom" | "IncrementSet" => {
            let delta = value.as_f64().unwrap_or(0.0);
            match existing {
                Some(FieldValue::Integer(n)) => Some(FieldValue::Integer(*n + delta as i64)),
                Some(FieldValue::Float(n)) => Some(FieldValue::Float(*n + delta)),
                _ => {
                    // Field missing or non-numeric: create with delta value
                    if delta.fract() == 0.0 {
                        Some(FieldValue::Integer(delta as i64))
                    } else {
                        Some(FieldValue::Float(delta))
                    }
                }
            }
        }
        "Decrement" | "DecrementFrom" | "DecrementSet" => {
            let delta = value.as_f64().unwrap_or(0.0);
            match existing {
                Some(FieldValue::Integer(n)) => Some(FieldValue::Integer(*n - delta as i64)),
                Some(FieldValue::Float(n)) => Some(FieldValue::Float(*n - delta)),
                _ => {
                    if delta.fract() == 0.0 {
                        Some(FieldValue::Integer(-(delta as i64)))
                    } else {
                        Some(FieldValue::Float(-delta))
                    }
                }
            }
        }
        "Add" => {
            let new_item = flapjack::types::json_value_to_field_value(value)?;
            match existing {
                Some(FieldValue::Array(arr)) => {
                    let mut new_arr = arr.clone();
                    new_arr.push(new_item);
                    Some(FieldValue::Array(new_arr))
                }
                None => Some(FieldValue::Array(vec![new_item])),
                _ => {
                    // Non-array: wrap existing + new into array
                    Some(FieldValue::Array(vec![existing.unwrap().clone(), new_item]))
                }
            }
        }
        "Remove" => {
            let remove_json = serde_json::to_string(value).unwrap_or_default();
            match existing {
                Some(FieldValue::Array(arr)) => {
                    let new_arr: Vec<FieldValue> = arr
                        .iter()
                        .filter(|item| {
                            let item_json = serde_json::to_string(
                                &flapjack::types::field_value_to_json_value(item),
                            )
                            .unwrap_or_default();
                            item_json != remove_json
                        })
                        .cloned()
                        .collect();
                    Some(FieldValue::Array(new_arr))
                }
                _ => existing.cloned(),
            }
        }
        "AddUnique" => {
            let new_item = flapjack::types::json_value_to_field_value(value)?;
            let new_json = serde_json::to_string(value).unwrap_or_default();
            match existing {
                Some(FieldValue::Array(arr)) => {
                    let already_exists = arr.iter().any(|item| {
                        let item_json = serde_json::to_string(
                            &flapjack::types::field_value_to_json_value(item),
                        )
                        .unwrap_or_default();
                        item_json == new_json
                    });
                    if already_exists {
                        Some(FieldValue::Array(arr.clone()))
                    } else {
                        let mut new_arr = arr.clone();
                        new_arr.push(new_item);
                        Some(FieldValue::Array(new_arr))
                    }
                }
                None => Some(FieldValue::Array(vec![new_item])),
                _ => Some(FieldValue::Array(vec![existing.unwrap().clone(), new_item])),
            }
        }
        _ => None,
    }
}

/// Check if a JSON value is a built-in operation object (has `_operation` key).
fn is_operation(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .map(|obj| obj.contains_key("_operation"))
        .unwrap_or(false)
}

/// Merge partial update fields into an existing document, or create a new one.
/// Returns `None` only when the document doesn't exist and `create_if_not_exists` is false.
pub(super) fn merge_partial_update(
    existing: Option<Document>,
    object_id: &str,
    body: &serde_json::Map<String, serde_json::Value>,
    create_if_not_exists: bool,
) -> Result<Option<Document>, FlapjackError> {
    match existing {
        Some(doc) => {
            let mut fields = doc.fields.clone();
            for (k, v) in body {
                if k == "objectID" || k == "id" {
                    continue;
                }
                if is_operation(v) {
                    let obj = v.as_object().unwrap();
                    let op = obj.get("_operation").and_then(|o| o.as_str()).unwrap_or("");
                    let op_value = obj.get("value").unwrap_or(&serde_json::Value::Null);
                    if let Some(new_val) = apply_operation(fields.get(k), op, op_value) {
                        fields.insert(k.clone(), new_val);
                    }
                } else if let Some(field_val) = flapjack::types::json_value_to_field_value(v) {
                    fields.insert(k.clone(), field_val);
                }
            }
            Ok(Some(Document {
                id: object_id.to_string(),
                fields,
            }))
        }
        None => {
            if !create_if_not_exists {
                return Ok(None);
            }
            let mut json_obj = serde_json::Map::new();
            json_obj.insert(
                "_id".to_string(),
                serde_json::Value::String(object_id.to_string()),
            );
            // For new documents, apply operations to empty fields
            let mut fields_from_ops = std::collections::HashMap::new();
            for (k, v) in body {
                if k == "objectID" || k == "id" {
                    continue;
                }
                if is_operation(v) {
                    let obj = v.as_object().unwrap();
                    let op = obj.get("_operation").and_then(|o| o.as_str()).unwrap_or("");
                    let op_value = obj.get("value").unwrap_or(&serde_json::Value::Null);
                    if let Some(new_val) = apply_operation(None, op, op_value) {
                        fields_from_ops.insert(k.clone(), new_val);
                    }
                } else {
                    json_obj.insert(k.clone(), v.clone());
                }
            }
            let mut doc = Document::from_json(&serde_json::Value::Object(json_obj))?;
            for (k, v) in fields_from_ops {
                doc.fields.insert(k, v);
            }
            Ok(Some(doc))
        }
    }
}

/// Add or update documents in batch
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/batch",
    tag = "documents",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    request_body(content = serde_json::Value, description = "Batch operations or single document"),
    responses(
        (status = 200, description = "Documents added successfully", body = BatchWriteResponse),
        (status = 400, description = "Invalid request")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn add_documents(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(app_id)): Extension<AuthenticatedAppId>,
    Path(index_name): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<axum::response::Response, FlapjackError> {
    use crate::idempotency::{IdempotencyRecord, BATCH_INDEX_WILDCARD, IDEMPOTENCY_HEADER};

    let index_segment = if index_name == BATCH_INDEX_WILDCARD {
        BATCH_INDEX_WILDCARD
    } else {
        index_name.as_str()
    };

    let idem_key = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if let Some(ref key) = idem_key {
        match state
            .idempotency_cache
            .lookup_scoped(&app_id, index_segment, key)
        {
            Ok(Some(record)) => return Ok(record.into_response()),
            Ok(None) => {}
            Err(err) => {
                tracing::error!(error = %err, "idempotency cache lookup failed");
                return Err(FlapjackError::Io(
                    "idempotency persistence lookup failed".to_string(),
                ));
            }
        }
    }

    // Render write errors into the response rather than propagating them as `Err`.
    // With durable adds, run_add_documents now awaits the Tantivy commit, so a commit
    // failure (5xx) or ack timeout (503) must surface as a real error status — never a
    // false 200. The status is identical whether axum converts an `Err` or we return
    // the rendered response here; returning it directly also lets callers that invoke
    // the handler outside the router observe the mapped status (PL-13 contract).
    let response = match run_add_documents(&state, &index_name, req).await {
        Ok(response) => response,
        Err(err) => return Ok(err.into_response()),
    };

    if let Some(key) = idem_key {
        if let Ok(body_bytes) = serde_json::to_vec(&response) {
            if let Err(err) = state.idempotency_cache.store_scoped(
                &app_id,
                index_segment,
                &key,
                IdempotencyRecord::json(axum::http::StatusCode::OK, body_bytes.into()),
            ) {
                tracing::error!(
                    error = %err,
                    app_id = %app_id,
                    index_segment,
                    idempotency_key = %key,
                    "idempotency cache store failed after successful write; returning write response"
                );
            }
        }
    }

    Ok(Json(response).into_response())
}

/// Failure outcome of the add-documents pipeline.
///
/// Distinguishes a request rejected before it was ever enqueued (no task exists)
/// from a write that was accepted into the queue but failed to commit durably. The
/// latter carries the enqueued `taskID` so the error response still reports it —
/// preserving the Algolia write-response contract (every accepted write has a
/// `taskID`) even when the durable commit fails or times out (PL-13 ack-on-durable).
pub(super) enum AddDocumentsError {
    /// Rejected before enqueue (validation, pause, parse, backpressure). No task id.
    Rejected(FlapjackError),
    /// Enqueued but the durable commit failed or timed out; reports the `taskID`.
    DurableCommitFailed { task_id: i64, source: FlapjackError },
}

impl From<FlapjackError> for AddDocumentsError {
    fn from(err: FlapjackError) -> Self {
        AddDocumentsError::Rejected(err)
    }
}

impl AddDocumentsError {
    fn durable_commit_failed(task_id: i64, source: FlapjackError) -> Self {
        AddDocumentsError::DurableCommitFailed { task_id, source }
    }
}

impl axum::response::IntoResponse for AddDocumentsError {
    fn into_response(self) -> axum::response::Response {
        match self {
            AddDocumentsError::Rejected(err) => err.into_response(),
            AddDocumentsError::DurableCommitFailed { task_id, source } => {
                let status = source.status_code();
                let mut response = (
                    status,
                    Json(serde_json::json!({
                        "taskID": task_id,
                        "message": source.api_message(),
                        "status": status.as_u16(),
                    })),
                )
                    .into_response();
                // Mirror the retriable signal the default FlapjackError path
                // attaches so task-aware responses preserve client retry behavior.
                // WriteAckTimeout, TooManyConcurrentWrites, and QueueFull are retryable.
                if matches!(
                    source,
                    FlapjackError::WriteAckTimeout
                        | FlapjackError::TooManyConcurrentWrites { .. }
                        | FlapjackError::QueueFull
                ) {
                    response
                        .headers_mut()
                        .insert("Retry-After", "1".parse().unwrap());
                }
                response
            }
        }
    }
}

async fn run_add_documents(
    state: &Arc<AppState>,
    index_name: &str,
    req: serde_json::Value,
) -> Result<AddDocumentsResponse, AddDocumentsError> {
    check_not_paused(&state.paused_indexes, index_name)?;
    if index_name != "*" {
        reject_writes_to_virtual_replica(state, index_name)?;
    }
    if let Ok(batch_req) = serde_json::from_value::<AddDocumentsRequest>(req.clone()) {
        if index_name == "*" {
            return batch::add_documents_multi_index_impl(State(state.clone()), batch_req)
                .await
                .map(|json| json.0);
        }

        if let AddDocumentsRequest::Batch { requests } = &batch_req {
            if requests.iter().any(|op| op.index_name.is_some()) {
                return Err(FlapjackError::InvalidQuery(
                    "The indexName attribute is only allowed on multiple indexes".to_string(),
                )
                .into());
            }
        }

        return batch::add_documents_batch_impl(
            State(state.clone()),
            index_name.to_string(),
            batch_req,
        )
        .await
        .map(|json| json.0);
    }

    let mut doc_map = req
        .as_object()
        .ok_or_else(|| FlapjackError::InvalidQuery("Expected object".to_string()))?
        .clone();

    check_record_size(&doc_map)?;

    let id = doc_map
        .remove("objectID")
        .or_else(|| doc_map.remove("id"))
        .and_then(|v| v.as_str().map(String::from))
        .ok_or_else(|| FlapjackError::InvalidQuery("Missing objectID or id field".to_string()))?;

    let fields = doc_map
        .into_iter()
        .filter_map(|(key, value)| {
            let field_value = match value {
                serde_json::Value::String(s) => Some(FieldValue::Text(s)),
                serde_json::Value::Number(n) => n
                    .as_i64()
                    .map(FieldValue::Integer)
                    .or_else(|| n.as_f64().map(FieldValue::Float)),
                serde_json::Value::Array(arr) => {
                    if arr.len() == 1 {
                        arr[0].as_str().map(|s| FieldValue::Facet(s.to_string()))
                    } else {
                        None
                    }
                }
                _ => None,
            };
            field_value.map(|v| (key, v))
        })
        .collect();

    let document = Document {
        id: id.clone(),
        fields,
    };
    // Enqueue, then wait for the durable commit while still holding the task so a
    // commit failure can report the taskID (PL-13). QueueFull surfaces as a 429 with
    // no task id, matching the pre-enqueue backpressure contract.
    let task = state
        .manager
        .add_documents(index_name, vec![document.clone()])?;
    if let Err(source) = state.manager.wait_for_write_durable(&task.id).await {
        return Err(AddDocumentsError::durable_commit_failed(
            task.numeric_id,
            source,
        ));
    }
    sync_add_documents_to_standard_replicas(state, index_name, &[document]).await?;

    Ok(AddDocumentsResponse::Algolia {
        task_id: task.numeric_id,
        object_ids: vec![id],
    })
}

/// Queue deletes and preserve the accepted-write task metadata on durable commit
/// failures so handlers can return Algolia-compatible error payloads.
pub(super) async fn delete_documents_durable_task_aware(
    state: &Arc<AppState>,
    index_name: &str,
    object_ids: Vec<String>,
) -> Result<TaskInfo, AddDocumentsError> {
    let task = state.manager.delete_documents(index_name, object_ids)?;
    if let Err(source) = state.manager.wait_for_write_durable(&task.id).await {
        return Err(AddDocumentsError::durable_commit_failed(
            task.numeric_id,
            source,
        ));
    }
    Ok(task)
}

/// Mirror accepted deletes to standard replicas while preserving the already
/// accepted primary taskID in any durable-failure response.
pub(super) async fn sync_delete_documents_to_standard_replicas_task_aware(
    state: &Arc<AppState>,
    primary_index_name: &str,
    object_ids: &[String],
    accepted_task_id: i64,
) -> Result<(), AddDocumentsError> {
    sync_delete_documents_to_standard_replicas(state, primary_index_name, object_ids)
        .await
        .map_err(|source| AddDocumentsError::durable_commit_failed(accepted_task_id, source))
}

/// Get a single object by ID
#[utoipa::path(
    get,
    path = "/1/indexes/{indexName}/{objectID}",
    tag = "documents",
    params(
        ("indexName" = String, Path, description = "Index name"),
        ("objectID" = String, Path, description = "Object ID to retrieve")
    ),
    responses(
        (status = 200, description = "Object retrieved successfully", body = serde_json::Value),
        (status = 404, description = "Object not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn get_object(
    State(state): State<Arc<AppState>>,
    Path((index_name, object_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    if is_virtual_settings_only_index(&state, &index_name) {
        return Err(FlapjackError::ObjectNotFound);
    }
    let doc = state.manager.get_document(&index_name, &object_id)?;

    match doc {
        None => Err(FlapjackError::ObjectNotFound),
        Some(document) => {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "objectID".to_string(),
                serde_json::Value::String(document.id),
            );

            for (key, value) in document.fields {
                obj.insert(key, field_value_to_json_value(&value));
            }

            Ok(Json(serde_json::Value::Object(obj)))
        }
    }
}

/// Delete a record from the specified index by its object ID.
#[utoipa::path(
    delete,
    path = "/1/indexes/{indexName}/{objectID}",
    tag = "documents",
    params(
        ("indexName" = String, Path, description = "Index name"),
        ("objectID" = String, Path, description = "Object ID to delete")
    ),
    responses(
        (status = 200, description = "Object deleted successfully", body = DeleteObjectResponse),
        (status = 404, description = "Object not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn delete_object(
    State(state): State<Arc<AppState>>,
    Path((index_name, object_id)): Path<(String, String)>,
) -> Result<axum::response::Response, FlapjackError> {
    check_not_paused(&state.paused_indexes, &index_name)?;
    reject_writes_to_virtual_replica(&state, &index_name)?;
    let delete_ids = vec![object_id];
    let pre_seq = state
        .manager
        .get_oplog(&index_name)
        .map(|ol| ol.current_seq())
        .unwrap_or(0);
    let delete_task =
        match delete_documents_durable_task_aware(&state, &index_name, delete_ids.clone()).await {
            Ok(task) => task,
            Err(err) => return Ok(err.into_response()),
        };
    if let Err(err) = sync_delete_documents_to_standard_replicas_task_aware(
        &state,
        &index_name,
        &delete_ids,
        delete_task.numeric_id,
    )
    .await
    {
        return Ok(err.into_response());
    }
    batch::trigger_replication(&state, &index_name, pre_seq, false);

    // Increment usage counter: 1 document deleted
    state
        .usage_counters
        .entry(index_name.clone())
        .or_insert_with(crate::usage_middleware::TenantUsageCounters::new)
        .documents_deleted_total
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let task = state.manager.make_noop_task(&index_name)?;
    Ok(Json(DeleteObjectResponse {
        task_id: task.numeric_id,
        deleted_at: chrono::Utc::now().to_rfc3339(),
    })
    .into_response())
}

/// Add or replace a record in the specified index.
#[utoipa::path(
    put,
    path = "/1/indexes/{indexName}/{objectID}",
    tag = "documents",
    params(
        ("indexName" = String, Path, description = "Index name"),
        ("objectID" = String, Path, description = "Object ID to update or create")
    ),
    request_body(content = serde_json::Value, description = "Object data"),
    responses(
        (status = 200, description = "Object updated successfully", body = PutObjectResponse),
        (status = 400, description = "Invalid request")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn put_object(
    State(state): State<Arc<AppState>>,
    Path((index_name, object_id)): Path<(String, String)>,
    Json(mut body): Json<serde_json::Map<String, serde_json::Value>>,
) -> Result<axum::response::Response, FlapjackError> {
    check_not_paused(&state.paused_indexes, &index_name)?;
    reject_writes_to_virtual_replica(&state, &index_name)?;
    state.manager.create_tenant(&index_name)?;

    body.remove("objectID");
    body.remove("id");

    let mut json_obj = serde_json::Map::new();
    json_obj.insert(
        "_id".to_string(),
        serde_json::Value::String(object_id.clone()),
    );
    for (k, v) in body {
        json_obj.insert(k, v);
    }

    let document = Document::from_json(&serde_json::Value::Object(json_obj))?;

    let pre_seq = state
        .manager
        .get_oplog(&index_name)
        .map(|ol| ol.current_seq())
        .unwrap_or(0);
    let delete_ids = vec![object_id.clone()];
    let delete_task =
        match delete_documents_durable_task_aware(&state, &index_name, delete_ids.clone()).await {
            Ok(task) => task,
            Err(err) => return Ok(err.into_response()),
        };
    if let Err(err) = sync_delete_documents_to_standard_replicas_task_aware(
        &state,
        &index_name,
        &delete_ids,
        delete_task.numeric_id,
    )
    .await
    {
        return Ok(err.into_response());
    }
    state
        .manager
        .add_documents_durable(&index_name, vec![document.clone()])
        .await?;
    sync_add_documents_to_standard_replicas(&state, &index_name, &[document]).await?;
    batch::trigger_replication(&state, &index_name, pre_seq, false);

    // Increment usage counter: 1 document indexed (put = upsert)
    state
        .usage_counters
        .entry(index_name.clone())
        .or_insert_with(crate::usage_middleware::TenantUsageCounters::new)
        .documents_indexed_total
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let task = state.manager.make_noop_task(&index_name)?;
    Ok(Json(PutObjectResponse {
        task_id: task.numeric_id,
        object_id,
        updated_at: chrono::Utc::now().to_rfc3339(),
    })
    .into_response())
}

/// Get multiple objects by ID in batch
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/objects",
    tag = "documents",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    request_body = GetObjectsRequest,
    responses(
        (status = 200, description = "Objects retrieved successfully", body = GetObjectsResponse)
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn get_objects(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GetObjectsRequest>,
) -> Result<Json<GetObjectsResponse>, FlapjackError> {
    let mut results = Vec::new();

    for request in req.requests {
        match state
            .manager
            .get_document(&request.index_name, &request.object_id)
        {
            Ok(Some(document)) => {
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "objectID".to_string(),
                    serde_json::Value::String(document.id),
                );

                for (key, value) in document.fields {
                    if let Some(attrs) = &request.attributes_to_retrieve {
                        if !attrs.contains(&key) {
                            continue;
                        }
                    }
                    obj.insert(key, field_value_to_json_value(&value));
                }

                results.push(serde_json::Value::Object(obj));
            }
            Ok(None) => {
                results.push(serde_json::Value::Null);
            }
            Err(_) => {
                results.push(serde_json::Value::Null);
            }
        }
    }

    Ok(Json(GetObjectsResponse { results }))
}

/// Delete objects matching a filter query
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/deleteByQuery",
    tag = "documents",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    request_body = DeleteByQueryRequest,
    responses(
        (status = 200, description = "Objects deleted successfully", body = serde_json::Value),
        (status = 400, description = "Invalid filter query")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn delete_by_query(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
    Json(req): Json<DeleteByQueryRequest>,
) -> Result<axum::response::Response, FlapjackError> {
    check_not_paused(&state.paused_indexes, &index_name)?;
    reject_writes_to_virtual_replica(&state, &index_name)?;
    let filter = if let Some(filter_str) = &req.filters {
        Some(
            parse_filter(filter_str)
                .map_err(|e| FlapjackError::InvalidQuery(format!("Filter parse error: {}", e)))?,
        )
    } else {
        return Err(FlapjackError::InvalidQuery(
            "filters parameter required".to_string(),
        ));
    };

    const BATCH_SIZE: usize = 1000;
    let mut all_ids = Vec::new();
    let mut offset = 0;

    loop {
        let result = state.manager.search_with_options(
            &index_name,
            "",
            &SearchOptions {
                filter: filter.as_ref(),
                limit: BATCH_SIZE,
                offset,
                ..Default::default()
            },
        )?;

        if result.documents.is_empty() {
            break;
        }

        for doc in &result.documents {
            all_ids.push(doc.document.id.clone());
        }

        offset += result.documents.len();

        if result.documents.len() < BATCH_SIZE {
            break;
        }

        if offset >= result.total {
            break;
        }
    }

    if all_ids.is_empty() {
        let task = state.manager.make_noop_task(&index_name)?;
        return Ok(Json(serde_json::json!({
            "taskID": task.numeric_id,
            "deletedAt": chrono::Utc::now().to_rfc3339()
        }))
        .into_response());
    }

    let deleted_count = all_ids.len() as u64;
    let pre_seq = state
        .manager
        .get_oplog(&index_name)
        .map(|ol| ol.current_seq())
        .unwrap_or(0);
    let delete_task =
        match delete_documents_durable_task_aware(&state, &index_name, all_ids.clone()).await {
            Ok(task) => task,
            Err(err) => return Ok(err.into_response()),
        };
    if let Err(err) = sync_delete_documents_to_standard_replicas_task_aware(
        &state,
        &index_name,
        &all_ids,
        delete_task.numeric_id,
    )
    .await
    {
        return Ok(err.into_response());
    }
    batch::trigger_replication(&state, &index_name, pre_seq, false);

    // Increment usage counter: N documents deleted by query
    state
        .usage_counters
        .entry(index_name.clone())
        .or_insert_with(crate::usage_middleware::TenantUsageCounters::new)
        .documents_deleted_total
        .fetch_add(deleted_count, std::sync::atomic::Ordering::Relaxed);

    let task = state.manager.make_noop_task(&index_name)?;
    Ok(Json(serde_json::json!({
        "taskID": task.numeric_id,
        "deletedAt": chrono::Utc::now().to_rfc3339()
    }))
    .into_response())
}

/// Add a record to the specified index with an auto-generated object ID.
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}",
    tag = "documents",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    request_body(content = serde_json::Value, description = "Object data (objectID is auto-generated)"),
    responses(
        (status = 201, description = "Object created successfully", body = SaveObjectResponse)
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn add_record_auto_id(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(app_id)): Extension<AuthenticatedAppId>,
    Path(index_name): Path<String>,
    headers: axum::http::HeaderMap,
    Json(mut body): Json<serde_json::Map<String, serde_json::Value>>,
) -> Result<axum::response::Response, FlapjackError> {
    use crate::idempotency::{IdempotencyRecord, IDEMPOTENCY_HEADER};

    let idem_key = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    if let Some(ref key) = idem_key {
        match state
            .idempotency_cache
            .lookup_scoped(&app_id, &index_name, key)
        {
            Ok(Some(record)) => return Ok(record.into_response()),
            Ok(None) => {}
            Err(err) => {
                tracing::error!(error = %err, "idempotency cache lookup failed");
                return Err(FlapjackError::Io(
                    "idempotency persistence lookup failed".to_string(),
                ));
            }
        }
    }

    check_not_paused(&state.paused_indexes, &index_name)?;
    reject_writes_to_virtual_replica(&state, &index_name)?;
    state.manager.create_tenant(&index_name)?;
    check_record_size(&body)?;

    body.remove("objectID");
    body.remove("id");

    // Content-hash auto-ID: same body → same ID → client retry is a safe upsert.
    let generated_id = auto_id_from_body(body.iter());

    let mut json_obj = serde_json::Map::new();
    json_obj.insert(
        "_id".to_string(),
        serde_json::Value::String(generated_id.clone()),
    );
    for (k, v) in body {
        json_obj.insert(k, v);
    }

    let document = Document::from_json(&serde_json::Value::Object(json_obj))?;
    let pre_seq = state
        .manager
        .get_oplog(&index_name)
        .map(|ol| ol.current_seq())
        .unwrap_or(0);
    let task = state
        .manager
        .add_documents_durable(&index_name, vec![document.clone()])
        .await?;
    sync_add_documents_to_standard_replicas(&state, &index_name, &[document]).await?;
    batch::trigger_replication(&state, &index_name, pre_seq, true);

    // Increment usage counter: 1 document indexed (auto-id create)
    state
        .usage_counters
        .entry(index_name.clone())
        .or_insert_with(crate::usage_middleware::TenantUsageCounters::new)
        .documents_indexed_total
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let payload = SaveObjectResponse {
        task_id: task.numeric_id,
        object_id: generated_id,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let status = axum::http::StatusCode::CREATED;

    if let Some(key) = idem_key {
        let body_bytes = serde_json::to_vec(&payload).unwrap_or_default();
        if let Err(err) = state.idempotency_cache.store_scoped(
            &app_id,
            &index_name,
            &key,
            IdempotencyRecord::json(status, body_bytes.into()),
        ) {
            tracing::error!(
                error = %err,
                app_id = %app_id,
                index_name = %index_name,
                idempotency_key = %key,
                "idempotency cache store failed after successful write; returning write response"
            );
        }
    }

    Ok((status, Json(payload)).into_response())
}

/// Partially update a record in the specified index by merging fields.
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/{objectID}/partial",
    tag = "documents",
    params(
        ("indexName" = String, Path, description = "Index name"),
        ("objectID" = String, Path, description = "Object ID to partially update"),
        ("createIfNotExists" = Option<bool>, Query, description = "Create the record if it doesn't exist (default: true)")
    ),
    request_body(content = serde_json::Value, description = "Fields to update"),
    responses(
        (status = 200, description = "Object partially updated", body = PartialUpdateObjectResponse)
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn partial_update_object(
    State(state): State<Arc<AppState>>,
    Path((index_name, object_id)): Path<(String, String)>,
    Query(params): Query<PartialUpdateParams>,
    Json(body): Json<serde_json::Map<String, serde_json::Value>>,
) -> Result<axum::response::Response, FlapjackError> {
    check_not_paused(&state.paused_indexes, &index_name)?;
    reject_writes_to_virtual_replica(&state, &index_name)?;
    state.manager.create_tenant(&index_name)?;

    let create_if_not_exists = params.create_if_not_exists.unwrap_or(true);
    let existing = state.manager.get_document(&index_name, &object_id)?;
    if existing.is_none() && !create_if_not_exists {
        return Err(FlapjackError::ObjectNotFound);
    }
    let had_existing = existing.is_some();

    let pre_seq = state
        .manager
        .get_oplog(&index_name)
        .map(|ol| ol.current_seq())
        .unwrap_or(0);
    let delete_ids = vec![object_id.clone()];

    if had_existing {
        let delete_task = match delete_documents_durable_task_aware(
            &state,
            &index_name,
            delete_ids.clone(),
        )
        .await
        {
            Ok(task) => task,
            Err(err) => return Ok(err.into_response()),
        };
        if let Err(err) = sync_delete_documents_to_standard_replicas_task_aware(
            &state,
            &index_name,
            &delete_ids,
            delete_task.numeric_id,
        )
        .await
        {
            return Ok(err.into_response());
        }
    }

    if let Some(doc) = merge_partial_update(existing, &object_id, &body, create_if_not_exists)? {
        state
            .manager
            .add_documents_durable(&index_name, vec![doc.clone()])
            .await?;
        sync_add_documents_to_standard_replicas(&state, &index_name, &[doc]).await?;
    }

    batch::trigger_replication(&state, &index_name, pre_seq, false);

    let task = state.manager.make_noop_task(&index_name)?;
    Ok(Json(PartialUpdateObjectResponse {
        task_id: task.numeric_id,
        object_id,
        updated_at: chrono::Utc::now().to_rfc3339(),
    })
    .into_response())
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialUpdateParams {
    pub create_if_not_exists: Option<bool>,
}

#[cfg(test)]
#[path = "../objects_tests.rs"]
mod tests;
