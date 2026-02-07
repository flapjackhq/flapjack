use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use std::sync::Arc;

use super::AppState;
use crate::dto::{
    AddDocumentsRequest, AddDocumentsResponse, BatchOperation, DeleteByQueryRequest,
    GetObjectsRequest, GetObjectsResponse,
};
use crate::filter_parser::parse_filter;
use flapjack::error::FlapjackError;
use flapjack::types::{Document, FieldValue};

fn field_value_to_json(value: &FieldValue) -> serde_json::Value {
    match value {
        FieldValue::Object(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                obj.insert(k.clone(), field_value_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
        FieldValue::Array(items) => {
            serde_json::Value::Array(items.iter().map(field_value_to_json).collect())
        }
        FieldValue::Text(s) => serde_json::Value::String(s.clone()),
        FieldValue::Integer(i) => serde_json::Value::Number((*i).into()),
        FieldValue::Float(f) => serde_json::json!(f),
        FieldValue::Date(d) => serde_json::Value::Number((*d).into()),
        FieldValue::Facet(s) => serde_json::Value::String(s.clone()),
    }
}

async fn add_documents_batch_impl(
    State(state): State<Arc<AppState>>,
    index_name: String,
    req: AddDocumentsRequest,
) -> Result<Json<AddDocumentsResponse>, FlapjackError> {
    state.manager.create_tenant(&index_name)?;

    let mut object_ids = Vec::new();
    let mut documents = Vec::new();
    let mut deletes = Vec::new();

    let operations = match req {
        AddDocumentsRequest::Batch { requests } => requests,
        AddDocumentsRequest::Legacy { documents: docs } => docs
            .into_iter()
            .map(|body| BatchOperation {
                action: "addObject".to_string(),
                body,
                create_if_not_exists: None,
            })
            .collect(),
    };

    let max_batch_size: usize = std::env::var("FLAPJACK_MAX_BATCH_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);
    if operations.len() > max_batch_size {
        return Err(FlapjackError::BatchTooLarge {
            size: operations.len(),
            max: max_batch_size,
        });
    }

    for op in operations {
        tracing::info!("Batch operation: action={}", op.action);
        match op.action.as_str() {
            "deleteObject" => {
                let object_id = op
                    .body
                    .get("objectID")
                    .or_else(|| op.body.get("id"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        FlapjackError::InvalidQuery("Missing objectID in deleteObject".to_string())
                    })?
                    .to_string();

                object_ids.push(object_id.clone());
                deletes.push(object_id);
            }
            "partialUpdateObject" | "partialUpdateObjectNoCreate" => {
                let object_id = op
                    .body
                    .get("objectID")
                    .or_else(|| op.body.get("id"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        FlapjackError::InvalidQuery(
                            "Missing objectID in partialUpdateObject".to_string(),
                        )
                    })?
                    .to_string();

                tracing::info!(
                    "partialUpdateObject: objectID={}, action={}, createIfNotExists={:?}",
                    object_id,
                    op.action,
                    op.create_if_not_exists
                );

                object_ids.push(object_id.clone());

                let create_if_not_exists = if op.action == "partialUpdateObjectNoCreate" {
                    false
                } else {
                    op.create_if_not_exists.unwrap_or(true)
                };

                let existing = state.manager.get_document(&index_name, &object_id)?;

                let merged_doc = match existing {
                    Some(doc) => {
                        let mut fields = doc.fields.clone();
                        for (k, v) in op.body {
                            if k != "objectID" && k != "id" {
                                if let Some(field_val) =
                                    flapjack::types::json_value_to_field_value(&v)
                                {
                                    fields.insert(k, field_val);
                                }
                            }
                        }
                        deletes.push(object_id.clone());
                        Some(Document {
                            id: object_id.clone(),
                            fields,
                        })
                    }
                    None => {
                        tracing::info!("Doc not found, createIfNotExists={}", create_if_not_exists);
                        if !create_if_not_exists {
                            None
                        } else {
                            let mut doc_map = op.body;
                            doc_map.remove("objectID");
                            doc_map.remove("id");

                            let mut json_obj = serde_json::Map::new();
                            json_obj.insert(
                                "_id".to_string(),
                                serde_json::Value::String(object_id.clone()),
                            );
                            for (k, v) in doc_map {
                                json_obj.insert(k, v);
                            }

                            Some(Document::from_json(&serde_json::Value::Object(json_obj))?)
                        }
                    }
                };

                if let Some(doc) = merged_doc {
                    documents.push(doc);
                }
            }
            "updateObject" => {
                let object_id = op
                    .body
                    .get("objectID")
                    .or_else(|| op.body.get("id"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        FlapjackError::InvalidQuery("Missing objectID in updateObject".to_string())
                    })?
                    .to_string();

                object_ids.push(object_id.clone());

                let mut doc_map = op.body;
                doc_map.remove("objectID");
                doc_map.remove("id");

                let mut json_obj = serde_json::Map::new();
                json_obj.insert(
                    "_id".to_string(),
                    serde_json::Value::String(object_id.clone()),
                );
                for (k, v) in doc_map {
                    json_obj.insert(k, v);
                }

                let document = Document::from_json(&serde_json::Value::Object(json_obj))?;
                documents.push(document);
            }
            "addObject" => {
                let mut doc_map = op.body;
                let id = doc_map
                    .remove("objectID")
                    .or_else(|| doc_map.remove("id"))
                    .and_then(|v| v.as_str().map(String::from))
                    .ok_or_else(|| {
                        FlapjackError::InvalidQuery(
                            "Missing objectID in batch operation".to_string(),
                        )
                    })?;

                object_ids.push(id.clone());

                let mut json_obj = serde_json::Map::new();
                json_obj.insert("_id".to_string(), serde_json::Value::String(id.clone()));
                for (k, v) in doc_map {
                    json_obj.insert(k, v);
                }

                let document = Document::from_json(&serde_json::Value::Object(json_obj))?;
                documents.push(document);
            }
            _ => {
                return Err(FlapjackError::InvalidQuery(format!(
                    "Unsupported batch action: {}",
                    op.action
                )));
            }
        }
    }

    let task = if documents.is_empty() && !deletes.is_empty() {
        state
            .manager
            .delete_documents_sync(&index_name, deletes)
            .await?;
        return Ok(Json(AddDocumentsResponse::Algolia {
            task_id: 0,
            object_ids,
        }));
    } else if !deletes.is_empty() {
        // Batch has explicit deletes (e.g. partialUpdateObject) — delete first, then add
        state
            .manager
            .delete_documents_sync(&index_name, deletes)
            .await?;
        state.manager.add_documents(&index_name, documents)?
    } else {
        // addObject/updateObject — always upsert (Algolia replaces if objectID exists)
        state.manager.add_documents(&index_name, documents)?
    };

    Ok(Json(AddDocumentsResponse::Algolia {
        task_id: task.numeric_id,
        object_ids,
    }))
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
        (status = 200, description = "Documents added successfully", body = AddDocumentsResponse),
        (status = 400, description = "Invalid request")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn add_documents(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<AddDocumentsResponse>, FlapjackError> {
    if let Ok(batch_req) = serde_json::from_value::<AddDocumentsRequest>(req.clone()) {
        return add_documents_batch_impl(State(state), index_name, batch_req).await;
    }

    let mut doc_map = req
        .as_object()
        .ok_or_else(|| FlapjackError::InvalidQuery("Expected object".to_string()))?
        .clone();

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
    let task = state.manager.add_documents(&index_name, vec![document])?;

    Ok(Json(AddDocumentsResponse::Algolia {
        task_id: task.id.parse().unwrap_or(0),
        object_ids: vec![id],
    }))
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
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let doc = state
        .manager
        .get_document(&index_name, &object_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match doc {
        None => Err((
            StatusCode::NOT_FOUND,
            format!("Object {} not found", object_id),
        )),
        Some(document) => {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "objectID".to_string(),
                serde_json::Value::String(document.id),
            );

            for (key, value) in document.fields {
                obj.insert(key, field_value_to_json(&value));
            }

            Ok(Json(serde_json::Value::Object(obj)))
        }
    }
}

/// Delete a single object by ID
#[utoipa::path(
    delete,
    path = "/1/indexes/{indexName}/{objectID}",
    tag = "documents",
    params(
        ("indexName" = String, Path, description = "Index name"),
        ("objectID" = String, Path, description = "Object ID to delete")
    ),
    responses(
        (status = 200, description = "Object deleted successfully", body = serde_json::Value),
        (status = 404, description = "Object not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn delete_object(
    State(state): State<Arc<AppState>>,
    Path((index_name, object_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    state
        .manager
        .delete_documents_sync(&index_name, vec![object_id])
        .await?;

    Ok(Json(serde_json::json!({
        "taskID": 0,
        "deletedAt": chrono::Utc::now().to_rfc3339()
    })))
}

/// Update or create an object
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
        (status = 200, description = "Object updated successfully", body = serde_json::Value),
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
) -> Result<Json<serde_json::Value>, FlapjackError> {
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

    state
        .manager
        .delete_documents_sync(&index_name, vec![object_id.clone()])
        .await?;
    state
        .manager
        .add_documents_sync(&index_name, vec![document])
        .await?;

    Ok(Json(serde_json::json!({
        "taskID": 0,
        "objectID": object_id,
        "updatedAt": chrono::Utc::now().to_rfc3339()
    })))
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
                    obj.insert(key, field_value_to_json(&value));
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
) -> Result<Json<serde_json::Value>, FlapjackError> {
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
        let result = state.manager.search_with_facets(
            &index_name,
            "",
            filter.as_ref(),
            None,
            BATCH_SIZE,
            offset,
            None,
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
        return Ok(Json(serde_json::json!({
            "taskID": 0,
            "deletedAt": chrono::Utc::now().to_rfc3339()
        })));
    }

    state
        .manager
        .delete_documents_sync(&index_name, all_ids)
        .await?;

    Ok(Json(serde_json::json!({
        "taskID": 0,
        "deletedAt": chrono::Utc::now().to_rfc3339()
    })))
}
