use axum::{
    extract::{Path, Query, State},
    Json,
};
use std::collections::HashMap;
use std::sync::Arc;

use super::replicas::{
    has_physical_index_data, reject_writes_to_virtual_replica, standard_replicas_for_primary,
};
use super::AppState;
use crate::dto::{CreateIndexRequest, CreateIndexResponse, DeleteIndexResponse};
use flapjack::error::FlapjackError;

/// Recursively compute total size of all files in a directory.
fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if ft.is_dir() {
                total += dir_size(&entry.path());
            }
        }
    }
    total
}

/// Append an operation to the index oplog and asynchronously replicate new entries to followers.
///
/// Captures the current oplog sequence number before appending, then reads back any new
/// operations and spawns a background task to push them through the replication manager.
/// No-ops silently when replication is not configured.
fn replicate_oplog_entry(
    state: &Arc<AppState>,
    index_name: &str,
    op_type: &str,
    payload: serde_json::Value,
) {
    let pre_seq = state
        .manager
        .get_oplog(index_name)
        .map(|ol| ol.current_seq())
        .unwrap_or(0);

    state.manager.append_oplog(index_name, op_type, payload);

    let Some(repl_mgr) = state.replication_manager.as_ref().map(Arc::clone) else {
        return;
    };

    if let Some(oplog) = state.manager.get_oplog(index_name) {
        match oplog.read_since(pre_seq) {
            Ok(ops) if !ops.is_empty() => {
                let tenant = index_name.to_string();
                tokio::spawn(async move {
                    repl_mgr.replicate_ops(&tenant, ops).await;
                });
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(
                "[REPL] failed to read oplog for index {} while replicating {}: {}",
                index_name,
                op_type,
                e
            ),
        }
    }
}

fn read_optional_json_file(path: &std::path::Path) -> serde_json::Value {
    match std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    {
        Some(value) => value,
        None => serde_json::Value::Null,
    }
}

/// Single index entry returned by the list-indices endpoint.
///
/// Field names are serialized in camelCase to match the Algolia REST API wire format.
/// `replicas`, `primary`, and `virtual_replica` are omitted from the response when `None`.
#[derive(Clone, serde::Serialize, utoipa::ToSchema)]
pub struct ListIndexItem {
    pub name: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub entries: u64,
    #[serde(rename = "dataSize")]
    pub data_size: u64,
    #[serde(rename = "fileSize")]
    pub file_size: u64,
    #[serde(rename = "lastBuildTimeS")]
    pub last_build_time_s: u64,
    #[serde(rename = "numberOfPendingTasks")]
    pub number_of_pending_tasks: usize,
    #[serde(rename = "pendingTask")]
    pub pending_task: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replicas: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    #[serde(rename = "virtual", skip_serializing_if = "Option::is_none")]
    pub virtual_replica: Option<bool>,
}

#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct ListIndicesResponse {
    pub items: Vec<ListIndexItem>,
    #[serde(rename = "nbPages")]
    pub nb_pages: u64,
}

/// Create a new index
#[utoipa::path(
    post,
    path = "/1/indexes",
    tag = "indices",
    request_body = CreateIndexRequest,
    responses(
        (status = 200, description = "Index created successfully", body = CreateIndexResponse),
        (status = 400, description = "Invalid request"),
        (status = 409, description = "Index already exists")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn create_index(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateIndexRequest>,
) -> Result<Json<CreateIndexResponse>, FlapjackError> {
    state.manager.create_tenant(&req.uid)?;

    Ok(Json(CreateIndexResponse {
        uid: req.uid,
        created_at: chrono::Utc::now().to_rfc3339(),
    }))
}

/// Delete an index
#[utoipa::path(
    delete,
    path = "/1/indexes/{indexName}",
    tag = "indices",
    params(
        ("indexName" = String, Path, description = "Index name to delete")
    ),
    responses(
        (status = 200, description = "Index deleted successfully", body = DeleteIndexResponse),
        (status = 404, description = "Index not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn delete_index(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
) -> Result<Json<DeleteIndexResponse>, FlapjackError> {
    state.manager.delete_tenant(&index_name).await?;
    let task = state.manager.make_noop_task(&index_name)?;
    Ok(Json(DeleteIndexResponse {
        task_id: task.numeric_id,
        deleted_at: chrono::Utc::now().to_rfc3339(),
    }))
}

/// List all indices
#[utoipa::path(
    get,
    path = "/1/indexes",
    tag = "indices",
    params(
        ("page" = Option<i64>, Query, description = "Page number (null = return all unpaginated)"),
        ("hitsPerPage" = Option<i64>, Query, description = "Number of indices per page (default 100, max 1000)")
    ),
    responses(
        (status = 200, description = "List of all indices", body = ListIndicesResponse),
        (status = 400, description = "Invalid pagination parameters")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn list_indices(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ListIndicesResponse>, FlapjackError> {
    let (page, hits_per_page) = parse_pagination_params(&params)?;

    let mut items: Vec<ListIndexItem> = Vec::new();

    for name in crate::tenant_dirs::valid_index_tenant_dir_names(&state.manager.base_path)? {
        let index_path = state.manager.base_path.join(&name);
        let file_size = dir_size(&index_path);

        let settings = state.manager.get_settings(&name);
        let replicas = settings.as_ref().and_then(|s| s.replicas.clone());
        let primary = settings.as_ref().and_then(|s| s.primary.clone());
        let is_virtual = primary.is_some() && !has_physical_index_data(&state, &name);

        // Compute entries and dataSize together from the same searcher.
        // dataSize: Algolia defines this as "bytes in minified format" (logical data).
        // We approximate using Tantivy's store data_usage (compressed stored-document bytes).
        let (entries, data_size) = if is_virtual {
            (0, 0)
        } else {
            match state.manager.get_or_load(&name) {
                Ok(index) => {
                    let reader = index.reader();
                    let searcher = reader.searcher();
                    let entries = searcher.num_docs();
                    let data_size = searcher
                        .space_usage()
                        .map(|su| {
                            su.segments()
                                .iter()
                                .map(|seg| seg.store().data_usage().get_bytes())
                                .sum::<u64>()
                        })
                        .unwrap_or(file_size);
                    (entries, data_size)
                }
                Err(e) => {
                    tracing::warn!("Failed to load index {}: {}", name, e);
                    continue;
                }
            }
        };

        // Read durable metadata for createdAt and lastBuildTimeS
        let meta = state.manager.tenant_metadata(&name);

        // Algolia: createdAt is "" when index has no records
        let created_at = if entries == 0 {
            "".to_string()
        } else {
            meta.as_ref()
                .map(|m| m.created_at.clone())
                .unwrap_or_default()
        };

        let last_build_time_s = meta.as_ref().map(|m| m.last_build_time_s).unwrap_or(0);

        let pending = state.manager.pending_task_count(&name);

        items.push(ListIndexItem {
            name,
            created_at,
            updated_at: chrono::Utc::now().to_rfc3339(),
            entries,
            data_size,
            file_size,
            last_build_time_s,
            number_of_pending_tasks: pending,
            pending_task: pending > 0,
            replicas,
            primary,
            virtual_replica: if is_virtual { Some(true) } else { None },
        });
    }

    // Sort alphabetically by name for stable ordering
    items.sort_by(|a, b| a.name.cmp(&b.name));

    // Apply pagination
    let (paginated_items, nb_pages) = if let Some(p) = page {
        let hpp = hits_per_page.unwrap_or(100) as usize;
        let total = items.len();
        let nb_pages = if total == 0 { 1 } else { total.div_ceil(hpp) };
        let start = (p as usize) * hpp;
        let page_items = if start >= total {
            vec![]
        } else {
            items[start..(start + hpp).min(total)].to_vec()
        };
        (page_items, nb_pages as u64)
    } else {
        // No page param: return all items unpaginated
        (items, 1)
    };

    Ok(Json(ListIndicesResponse {
        items: paginated_items,
        nb_pages,
    }))
}

/// Extract and validate `page` and `hitsPerPage` query parameters from a string map.
///
/// # Returns
///
/// A tuple of `(page, hits_per_page)` where either value is `None` when the corresponding
/// key is absent from the map.
///
/// # Errors
///
/// Returns `InvalidQuery` if `page` is negative, `hitsPerPage` is less than 1 or greater
/// than 1000, or either value cannot be parsed as an `i64`.
fn parse_pagination_params(
    params: &HashMap<String, String>,
) -> Result<(Option<i64>, Option<i64>), FlapjackError> {
    let page: Option<i64> = params
        .get("page")
        .map(|v| {
            v.parse::<i64>()
                .map_err(|_| FlapjackError::InvalidQuery(format!("Invalid page value: {}", v)))
        })
        .transpose()?;

    let hits_per_page: Option<i64> = params
        .get("hitsPerPage")
        .map(|v| {
            v.parse::<i64>().map_err(|_| {
                FlapjackError::InvalidQuery(format!("Invalid hitsPerPage value: {}", v))
            })
        })
        .transpose()?;

    if let Some(p) = page {
        if p < 0 {
            return Err(FlapjackError::InvalidQuery("page must be >= 0".to_string()));
        }
    }

    if let Some(hpp) = hits_per_page {
        if hpp < 1 {
            return Err(FlapjackError::InvalidQuery(
                "hitsPerPage must be >= 1".to_string(),
            ));
        }
        if hpp > 1000 {
            return Err(FlapjackError::InvalidQuery(
                "hitsPerPage must be <= 1000".to_string(),
            ));
        }
    }

    Ok((page, hits_per_page))
}

/// Clear all documents from an index
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/clear",
    tag = "indices",
    params(
        ("indexName" = String, Path, description = "Index name to clear")
    ),
    responses(
        (status = 200, description = "Index cleared successfully", body = serde_json::Value),
        (status = 404, description = "Index not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn clear_index(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let task_id = clear_index_impl(&state, &index_name).await?;
    Ok(Json(serde_json::json!({
        "taskID": task_id,
        "updatedAt": chrono::Utc::now().to_rfc3339()
    })))
}

/// Clear all documents from an index and its standard replicas, recording the operation in the oplog.
///
/// Rejects writes to virtual replicas, preserves settings and relevance configuration
/// for each cleared index, and returns the task ID of the resulting no-op task.
///
/// # Errors
///
/// Returns an error if the index is a virtual replica, if replica resolution fails,
/// or if any underlying clear operation fails.
pub(crate) async fn clear_index_impl(
    state: &Arc<AppState>,
    index_name: &String,
) -> Result<i64, FlapjackError> {
    reject_writes_to_virtual_replica(state, index_name)?;
    let replica_names = standard_replicas_for_primary(state, index_name)?;

    replicate_oplog_entry(
        state,
        index_name,
        "clear_index",
        serde_json::json!({ "index_name": index_name }),
    );

    clear_single_index_preserving_settings(state, index_name).await?;
    for replica_name in replica_names {
        clear_single_index_preserving_settings(state, &replica_name).await?;
    }

    let task = state.manager.make_noop_task(index_name)?;
    Ok(task.numeric_id)
}

/// Delete and recreate a single index while preserving its `settings.json` and `relevance.json` files.
///
/// Waits for the write queue to drain via `delete_tenant` to avoid race conditions,
/// then restores the saved configuration into the fresh index directory.
///
/// # Errors
///
/// Returns an error if the index name is invalid, the tenant cannot be deleted or
/// recreated, or filesystem I/O fails.
async fn clear_single_index_preserving_settings(
    state: &Arc<AppState>,
    index_name: &String,
) -> Result<(), FlapjackError> {
    flapjack::index::manager::validate_index_name(index_name)?;
    let index_path = state.manager.base_path.join(index_name);
    let settings_path = index_path.join("settings.json");
    let relevance_path = index_path.join("relevance.json");

    // Preserve settings and relevance config before clearing
    let settings = if settings_path.exists() {
        Some(std::fs::read(&settings_path)?)
    } else {
        None
    };
    let relevance = if relevance_path.exists() {
        Some(std::fs::read(&relevance_path)?)
    } else {
        None
    };

    // Use delete_tenant (which awaits the write queue) instead of
    // unload + remove_dir_all to avoid race conditions.
    state.manager.delete_tenant(index_name).await?;
    state.manager.create_tenant(index_name)?;

    // Restore settings and relevance config
    if let Some(data) = settings {
        std::fs::write(&settings_path, data)?;
    }
    if let Some(data) = relevance {
        std::fs::write(&relevance_path, data)?;
    }

    Ok(())
}

/// Compact an index (merge segments and reclaim disk space)
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/compact",
    tag = "indices",
    params(
        ("indexName" = String, Path, description = "Index name to compact")
    ),
    responses(
        (status = 200, description = "Compaction started", body = serde_json::Value),
        (status = 404, description = "Index not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn compact_index(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let task = state.manager.compact_index(&index_name)?;
    Ok(Json(serde_json::json!({
        "taskID": task.numeric_id,
        "updatedAt": chrono::Utc::now().to_rfc3339()
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{body_json, TestStateBuilder};
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use std::collections::HashMap;
    use tower::ServiceExt;

    #[test]
    fn dir_size_nonexistent() {
        assert_eq!(dir_size(std::path::Path::new("/nonexistent/path")), 0);
    }

    #[test]
    fn dir_size_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dir_size(dir.path()), 0);
    }

    #[test]
    fn dir_size_with_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap(); // 5 bytes
        std::fs::write(dir.path().join("b.txt"), "world!").unwrap(); // 6 bytes
        assert_eq!(dir_size(dir.path()), 11);
    }

    #[test]
    fn dir_size_recursive() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("top.txt"), "ab").unwrap(); // 2 bytes
        std::fs::write(sub.join("nested.txt"), "cde").unwrap(); // 3 bytes
        assert_eq!(dir_size(dir.path()), 5);
    }

    #[test]
    fn parse_pagination_rejects_negative_page() {
        let mut params = HashMap::new();
        params.insert("page".to_string(), "-1".to_string());

        let err = parse_pagination_params(&params).unwrap_err();
        assert!(matches!(err, FlapjackError::InvalidQuery(_)));
        assert!(err.to_string().contains("page must be >= 0"));
    }

    #[test]
    fn parse_pagination_rejects_zero_hits_per_page() {
        let mut params = HashMap::new();
        params.insert("hitsPerPage".to_string(), "0".to_string());

        let err = parse_pagination_params(&params).unwrap_err();
        assert!(matches!(err, FlapjackError::InvalidQuery(_)));
        assert!(err.to_string().contains("hitsPerPage must be >= 1"));
    }

    #[test]
    fn parse_pagination_rejects_hits_per_page_above_max() {
        let mut params = HashMap::new();
        params.insert("hitsPerPage".to_string(), "1001".to_string());

        let err = parse_pagination_params(&params).unwrap_err();
        assert!(matches!(err, FlapjackError::InvalidQuery(_)));
        assert!(err.to_string().contains("hitsPerPage must be <= 1000"));
    }

    #[test]
    fn parse_pagination_accepts_valid_values() {
        let mut params = HashMap::new();
        params.insert("page".to_string(), "2".to_string());
        params.insert("hitsPerPage".to_string(), "50".to_string());

        let (page, hits_per_page) = parse_pagination_params(&params).unwrap();
        assert_eq!(page, Some(2));
        assert_eq!(hits_per_page, Some(50));
    }

    #[test]
    fn parse_pagination_accepts_absent_values() {
        let params = HashMap::new();
        let (page, hits_per_page) = parse_pagination_params(&params).unwrap();
        assert_eq!(page, None);
        assert_eq!(hits_per_page, None);
    }

    // ── Stage 5: Wire-Format Parity ────────────────────────────────────────────

    /// list_indices returns dataSize and fileSize as integer byte values,
    /// with dataSize <= fileSize (dataSize is stored-document bytes,
    /// fileSize is total directory size including overhead).
    #[tokio::test]
    async fn list_indices_reports_datasize_filesize_byte_semantics() {
        let tmp = tempfile::tempdir().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();

        // Create a test index with documents
        state.manager.create_tenant("test_size_idx").unwrap();
        let settings = flapjack::index::settings::IndexSettings {
            searchable_attributes: Some(vec!["title".to_string()]),
            ..Default::default()
        };
        let settings_path = tmp.path().join("test_size_idx").join("settings.json");
        std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        std::fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        // Add some documents to create actual data
        state
            .manager
            .add_documents_sync(
                "test_size_idx",
                vec![
                    flapjack::Document {
                        id: "doc1".into(),
                        fields: {
                            let mut m = std::collections::HashMap::new();
                            m.insert(
                                "title".into(),
                                flapjack::FieldValue::Text(
                                    "hello world this is a test document".into(),
                                ),
                            );
                            m
                        },
                    },
                    flapjack::Document {
                        id: "doc2".into(),
                        fields: {
                            let mut m = std::collections::HashMap::new();
                            m.insert(
                                "title".into(),
                                flapjack::FieldValue::Text(
                                    "another test document for sizing".into(),
                                ),
                            );
                            m
                        },
                    },
                ],
            )
            .await
            .unwrap();

        let app = Router::new()
            .route("/1/indexes", get(list_indices))
            .with_state(Arc::clone(&state));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/1/indexes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;

        let items = body["items"]
            .as_array()
            .expect("list_indices response must contain items array");
        let index_item = items
            .iter()
            .find(|item| item["name"] == "test_size_idx")
            .expect("list_indices must include test_size_idx");

        let entries = index_item["entries"]
            .as_u64()
            .expect("entries must be an integer");
        let data_size = index_item["dataSize"]
            .as_u64()
            .expect("dataSize must be an integer");
        let file_size = index_item["fileSize"]
            .as_u64()
            .expect("fileSize must be an integer");

        // Verify byte semantics:
        // 1. Both values are integers (JSON numbers parsed as u64 above)
        // 2. dataSize <= fileSize (data is subset of total file size)
        // 3. fileSize > 0 for non-empty index
        assert!(
            data_size <= file_size,
            "dataSize ({}) should be <= fileSize ({})",
            data_size,
            file_size
        );
        assert!(file_size > 0, "fileSize should be > 0 for non-empty index");

        // Verify entries count matches document count
        assert_eq!(entries, 2, "entries should match document count");
    }

    #[tokio::test]
    async fn list_indices_excludes_publication_roots_and_paginates_visible_tenants() {
        let tmp = tempfile::tempdir().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        state.manager.create_tenant("products").unwrap();
        std::fs::create_dir_all(tmp.path().join(".publication")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".publication_quarantine")).unwrap();

        let app = Router::new()
            .route("/1/indexes", get(list_indices))
            .with_state(Arc::clone(&state));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/1/indexes?page=0&hitsPerPage=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let names: Vec<_> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["products"]);
        assert_eq!(body["nbPages"], 1);
    }

    #[tokio::test]
    async fn list_indices_preserves_valid_nonpublication_tenant_names() {
        let tmp = tempfile::tempdir().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        state.manager.create_tenant("_shadow").unwrap();
        state.manager.create_tenant("analytics").unwrap();
        state.manager.create_tenant("products").unwrap();
        std::fs::create_dir_all(tmp.path().join(".publication")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".publication_quarantine")).unwrap();

        let app = Router::new()
            .route("/1/indexes", get(list_indices))
            .with_state(Arc::clone(&state));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/1/indexes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let names: Vec<_> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["_shadow", "analytics", "products"]);
        assert_eq!(body["nbPages"], 1);
    }
}

#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct OperationIndexRequest {
    pub operation: String,
    pub destination: String,
    pub scope: Option<Vec<String>>,
}

/// Move or copy an index
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/operation",
    tag = "indices",
    params(
        ("indexName" = String, Path, description = "Source index name")
    ),
    request_body = OperationIndexRequest,
    responses(
        (status = 200, description = "Operation completed successfully", body = serde_json::Value),
        (status = 400, description = "Invalid operation"),
        (status = 404, description = "Index not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn operation_index(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
    Json(req): Json<OperationIndexRequest>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let task = match req.operation.as_str() {
        "move" => {
            let task = state
                .manager
                .move_index(&index_name, &req.destination)
                .await?;
            replicate_oplog_entry(
                &state,
                &req.destination,
                "move_index",
                serde_json::json!({
                    "source": index_name.clone(),
                    "destination": req.destination.clone()
                }),
            );
            task
        }
        "copy" => {
            let task = state
                .manager
                .copy_index(&index_name, &req.destination, req.scope.as_deref())
                .await?;

            let source_index_dir = state.manager.base_path.join(&index_name);
            let source_settings = state
                .manager
                .get_settings(&index_name)
                .and_then(|s| serde_json::to_value(s.as_ref()).ok())
                .unwrap_or(serde_json::Value::Null);
            let source_synonyms = read_optional_json_file(&source_index_dir.join("synonyms.json"));
            let source_rules = read_optional_json_file(&source_index_dir.join("rules.json"));
            replicate_oplog_entry(
                &state,
                &index_name,
                "copy_index",
                serde_json::json!({
                    "source": index_name.clone(),
                    "destination": req.destination.clone(),
                    "scope": req.scope.clone(),
                    "source_settings": source_settings,
                    "source_synonyms": source_synonyms,
                    "source_rules": source_rules
                }),
            );
            task
        }
        _ => {
            return Err(FlapjackError::InvalidQuery(format!(
                "Unknown operation: {}",
                req.operation
            )))
        }
    };

    Ok(Json(serde_json::json!({
        "taskID": task.numeric_id,
        "updatedAt": chrono::Utc::now().to_rfc3339()
    })))
}
