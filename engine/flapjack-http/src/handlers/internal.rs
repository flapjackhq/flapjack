use crate::extractors::ValidatedIndexName;
use crate::handlers::internal_ops::{
    apply_clear_index_op, apply_clear_rules_op, apply_clear_synonyms_op, apply_copy_index_op,
    apply_delete_op, apply_delete_rule_op, apply_delete_synonym_op, apply_move_index_op,
    apply_save_rule_op, apply_save_rules_op, apply_save_synonym_op, apply_save_synonyms_op,
    apply_upsert_op, flush_document_batch,
};
use crate::handlers::AppState;
use axum::{
    extract::{Path, Query, State},
    http::header,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use flapjack::index::oplog::{OpLog, OpLogEntry};
use flapjack::index::snapshot::export_to_bytes;
use flapjack::{validate_index_name, IndexManager};
use flapjack_replication::config::{NodeConfig, PeerConfig};
use flapjack_replication::manager::AddPeerError;
use flapjack_replication::types::{
    GetOpsQuery, GetOpsResponse, ListTenantsResponse, ReplicateOpsRequest, ReplicateOpsResponse,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use utoipa::ToSchema;

/// Core apply logic: parse ops and write to IndexManager.
/// Returns the highest sequence number applied, or an error string.
///
/// Implements LWW (last-writer-wins) conflict resolution:
/// - For upserts: (timestamp_ms, node_id) tuples are compared; higher wins.
/// - For deletes: only applied if no newer upsert has been recorded for the doc.
/// - LWW state is tracked in-memory in IndexManager::lww_map.
pub async fn apply_ops_to_manager(
    manager: &IndexManager,
    tenant_id: &str,
    ops: &[OpLogEntry],
) -> Result<u64, String> {
    validate_index_name(tenant_id).map_err(|e| e.to_string())?;

    bootstrap_document_lww_state(manager, tenant_id, ops);

    let mut max_seq = 0u64;
    let mut upserts = Vec::new();
    let mut deletes = Vec::new();
    let mut final_op_type: HashMap<String, &str> = HashMap::new();

    for op_entry in ops {
        max_seq = max_seq.max(op_entry.seq);
        let incoming = (op_entry.timestamp_ms, op_entry.node_id.clone());
        apply_replication_op(
            manager,
            tenant_id,
            op_entry,
            incoming,
            &mut upserts,
            &mut deletes,
            &mut final_op_type,
        )
        .await;
    }

    flush_document_batch(manager, tenant_id, upserts, deletes, final_op_type).await?;
    Ok(max_seq)
}

fn bootstrap_document_lww_state(manager: &IndexManager, tenant_id: &str, ops: &[OpLogEntry]) {
    if !contains_document_replication_ops(ops) {
        return;
    }
    if manager.get_or_load(tenant_id).is_ok() {
        return;
    }
    let _ = manager.create_tenant(tenant_id);
}

fn contains_document_replication_ops(ops: &[OpLogEntry]) -> bool {
    ops.iter()
        .any(|op| matches!(op.op_type.as_str(), "upsert" | "delete"))
}

/// Applies a single replicated oplog entry (upsert, delete, settings change, copy/move, etc.) to the local index, accumulating batch upserts and deletes.
async fn apply_replication_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
    incoming: (u64, String),
    upserts: &mut Vec<flapjack::types::Document>,
    deletes: &mut Vec<String>,
    final_op_type: &mut HashMap<String, &str>,
) {
    match op_entry.op_type.as_str() {
        "upsert" => {
            apply_upsert_op(
                manager,
                tenant_id,
                op_entry,
                incoming,
                upserts,
                final_op_type,
            );
        }
        "delete" => {
            apply_delete_op(
                manager,
                tenant_id,
                op_entry,
                incoming,
                deletes,
                final_op_type,
            );
        }
        "move_index" => {
            log_op_error(apply_move_index_op(manager, tenant_id, op_entry).await);
        }
        "copy_index" => {
            log_op_error(apply_copy_index_op(manager, tenant_id, op_entry).await);
        }
        "clear_index" => {
            log_op_error(apply_clear_index_op(manager, tenant_id, op_entry).await);
        }
        "save_synonym" => apply_save_synonym_op(manager, tenant_id, op_entry),
        "save_synonyms" => apply_save_synonyms_op(manager, tenant_id, op_entry),
        "delete_synonym" => apply_delete_synonym_op(manager, tenant_id, op_entry),
        "clear_synonyms" => apply_clear_synonyms_op(manager, tenant_id, op_entry),
        "save_rule" => apply_save_rule_op(manager, tenant_id, op_entry),
        "save_rules" => apply_save_rules_op(manager, tenant_id, op_entry),
        "delete_rule" => apply_delete_rule_op(manager, tenant_id, op_entry),
        "clear_rules" => apply_clear_rules_op(manager, tenant_id, op_entry),
        _ => tracing::warn!(
            "[REPL {}] unknown op_type {} at seq {}",
            tenant_id,
            op_entry.op_type,
            op_entry.seq
        ),
    }
}

fn log_op_error(result: Result<(), String>) {
    if let Err(error) = result {
        tracing::warn!("{}", error);
    }
}

fn local_node_current_seq_map(state: &AppState, current_seq: u64) -> BTreeMap<String, u64> {
    let mut node_current_seqs = BTreeMap::new();
    if let Some(repl_mgr) = state.replication_manager.as_ref() {
        node_current_seqs.insert(repl_mgr.node_id().to_string(), current_seq);
    }
    node_current_seqs
}

/// POST /internal/replicate
/// Receive operations from a peer and apply them to local index.
pub async fn replicate_ops(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReplicateOpsRequest>,
) -> Result<Json<ReplicateOpsResponse>, crate::error_response::HandlerError> {
    let tenant_id = req.tenant_id.clone();

    // Preserve 400 semantics for malformed peer input before apply_ops_to_manager
    // erases validation failures into a plain String for shared non-HTTP callers.
    validate_index_name(&tenant_id).map_err(crate::error_response::HandlerError::from)?;

    let max_seq = apply_ops_to_manager(&state.manager, &tenant_id, &req.ops).await?;

    tracing::info!(
        "[REPL {}] applied {} ops (max_seq={})",
        tenant_id,
        req.ops.len(),
        max_seq
    );

    Ok(Json(ReplicateOpsResponse {
        tenant_id,
        acked_seq: max_seq,
    }))
}

/// GET /internal/ops?tenant_id=X&since_seq=N
/// Fetch operations since a given sequence number for catch-up
pub async fn get_ops(
    State(state): State<Arc<AppState>>,
    Query(query): Query<GetOpsQuery>,
) -> Result<Json<GetOpsResponse>, crate::error_response::HandlerError> {
    use crate::error_response::HandlerError;

    let tenant_id = query.tenant_id.clone();
    validate_index_name(&tenant_id).map_err(HandlerError::from)?;

    // Get oplog for tenant
    let oplog = match state.manager.get_oplog(&tenant_id) {
        Some(ol) => ol,
        None => {
            // move_index writes are logged under the destination stream after the move,
            // which means the source tenant oplog path no longer exists. For anti-entropy
            // catch-up, when source oplog is missing, search existing oplogs for a matching
            // move_index(source=tenant_id) and return only ops up to that move boundary.
            if let Some((ops, current_seq, moved_to)) =
                find_moved_source_ops(&state, &tenant_id, query.since_seq)
            {
                tracing::info!(
                    "[REPL {}] source oplog missing; serving {} moved-source ops from destination stream {} (since_seq={}, current_seq={})",
                    tenant_id,
                    ops.len(),
                    moved_to,
                    query.since_seq,
                    current_seq
                );
                return Ok(Json(GetOpsResponse {
                    tenant_id,
                    ops,
                    current_seq,
                    oldest_retained_seq: None,
                    node_current_seqs: local_node_current_seq_map(&state, current_seq),
                }));
            }

            tracing::warn!("[REPL {}] oplog not found", tenant_id);
            return Err(HandlerError::not_found("Tenant not found"));
        }
    };

    // Read ops since requested sequence
    let ops = oplog.read_since(query.since_seq).map_err(|e| {
        tracing::error!("[REPL {}] failed to read oplog: {}", tenant_id, e);
        HandlerError::from(e)
    })?;

    let current_seq = oplog.current_seq();
    let oldest_retained_seq = oplog.oldest_seq();

    tracing::info!(
        "[REPL {}] serving {} ops (since_seq={}, current_seq={})",
        tenant_id,
        ops.len(),
        query.since_seq,
        current_seq
    );

    Ok(Json(GetOpsResponse {
        tenant_id,
        ops,
        current_seq,
        oldest_retained_seq,
        node_current_seqs: local_node_current_seq_map(&state, current_seq),
    }))
}

/// GET /internal/tenants
/// Return visible tenant directory names for startup catch-up discovery.
pub async fn list_tenants(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListTenantsResponse>, crate::error_response::HandlerError> {
    let mut tenants = crate::tenant_dirs::visible_tenant_dir_names(&state.manager.base_path)?;
    tenants.sort();
    Ok(Json(ListTenantsResponse { tenants }))
}

/// GET /internal/snapshot/:tenantId
/// Export a tenant directory as gzipped snapshot bytes for startup gap recovery.
pub async fn internal_snapshot(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(tenant_id): ValidatedIndexName,
) -> Result<impl IntoResponse, crate::error_response::HandlerError> {
    use crate::error_response::HandlerError;

    let tenant_path = state.manager.base_path.join(&tenant_id);
    if !tenant_path.exists() {
        return Err(HandlerError::not_found("Tenant not found"));
    }

    let bytes = export_to_bytes(&tenant_path).map_err(|error| {
        tracing::error!(
            "[REPL {}] failed to export internal snapshot: {}",
            tenant_id,
            error
        );
        HandlerError::from(error)
    })?;

    Ok(([(header::CONTENT_TYPE, "application/gzip")], bytes))
}

/// Search all tenant oplogs for a `move_index` entry whose source matches `source_tenant`.
///
/// Used as a fallback when a source tenant's oplog no longer exists because the
/// index was renamed. Returns ops from the destination stream up to (and including)
/// the move boundary, so the replica can catch up without missing the move event.
///
/// # Arguments
///
/// * `state` - Application state providing access to the index manager.
/// * `source_tenant` - Original tenant name before the move.
/// * `since_seq` - Sequence number to read ops from.
///
/// # Returns
///
/// `Some((ops, current_seq, destination_tenant))` if a matching move was found,
/// or `None` if no destination stream contains a relevant `move_index` entry.
fn find_moved_source_ops(
    state: &AppState,
    source_tenant: &str,
    since_seq: u64,
) -> Option<(Vec<OpLogEntry>, u64, String)> {
    let tenant_names =
        crate::tenant_dirs::valid_index_tenant_dir_names(&state.manager.base_path).ok()?;
    let node_id = std::env::var("FLAPJACK_NODE_ID").unwrap_or_else(|_| "unknown".to_string());

    for candidate_tenant in tenant_names {
        if candidate_tenant == source_tenant {
            continue;
        }

        let oplog_dir = state
            .manager
            .base_path
            .join(&candidate_tenant)
            .join("oplog");
        if !oplog_dir.exists() {
            continue;
        }

        let oplog = match OpLog::open(&oplog_dir, &candidate_tenant, &node_id) {
            Ok(oplog) => oplog,
            Err(e) => {
                tracing::debug!(
                    "[REPL {}] moved-source fallback skipping {}: failed to open oplog: {}",
                    source_tenant,
                    candidate_tenant,
                    e
                );
                continue;
            }
        };

        let mut ops = match oplog.read_since(since_seq) {
            Ok(ops) => ops,
            Err(e) => {
                tracing::debug!(
                    "[REPL {}] moved-source fallback skipping {}: failed to read oplog: {}",
                    source_tenant,
                    candidate_tenant,
                    e
                );
                continue;
            }
        };

        let Some(move_pos) = ops.iter().position(|op| {
            op.op_type == "move_index"
                && op
                    .payload
                    .get("source")
                    .and_then(|v| v.as_str())
                    .map(|src| src == source_tenant)
                    .unwrap_or(false)
        }) else {
            continue;
        };

        // Never return destination writes after the move boundary when serving
        // source stream catch-up.
        ops.truncate(move_pos + 1);
        let current_seq = ops.last().map(|op| op.seq).unwrap_or(since_seq);
        return Some((ops, current_seq, candidate_tenant));
    }

    None
}

/// GET /internal/status
/// Return basic replication status for monitoring
pub async fn replication_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let (node_id, replication_enabled, peer_count) = match &state.replication_manager {
        Some(repl_mgr) => (repl_mgr.node_id().to_string(), true, repl_mgr.peer_count()),
        None => (
            std::env::var("FLAPJACK_NODE_ID").unwrap_or_else(|_| "unknown".to_string()),
            false,
            0,
        ),
    };

    // Get SSL renewal status if available
    let ssl_renewal = if let Some(ref ssl_mgr) = state.ssl_manager {
        Some(ssl_mgr.get_status().await)
    } else {
        None
    };

    let storage_total_bytes: u64 = state
        .manager
        .all_tenant_storage()
        .iter()
        .map(|(_, b)| b)
        .sum();
    let tenant_count = state.manager.loaded_count();

    #[cfg(feature = "vector-search")]
    let vector_memory_bytes = state.manager.vector_memory_usage();
    #[cfg(not(feature = "vector-search"))]
    let vector_memory_bytes = 0usize;

    let response = serde_json::json!({
        "node_id": node_id,
        "replication_enabled": replication_enabled,
        "peer_count": peer_count,
        "ssl_renewal": ssl_renewal,
        "storage_total_bytes": storage_total_bytes,
        "tenant_count": tenant_count,
        "vector_memory_bytes": vector_memory_bytes,
    });

    (StatusCode::OK, Json(response)).into_response()
}

/// GET /internal/cluster/status
/// Return health status of all peers based on last_success timestamps.
/// Provides quick cluster health overview without active probing.
pub async fn cluster_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let repl_mgr = match &state.replication_manager {
        Some(r) => r,
        None => {
            return (
                StatusCode::OK,
                Json(ClusterStatusResponse {
                    node_id: std::env::var("FLAPJACK_NODE_ID")
                        .unwrap_or_else(|_| "unknown".to_string()),
                    replication_enabled: false,
                    peers_total: None,
                    peers_healthy: None,
                    peers: Vec::new(),
                }),
            )
                .into_response();
        }
    };

    let peers = repl_mgr
        .peer_statuses()
        .into_iter()
        .map(|peer| ClusterPeerStatus {
            peer_id: peer.peer_id,
            addr: peer.addr,
            status: peer.status,
            last_success_secs_ago: peer.last_success_secs_ago,
        })
        .collect::<Vec<_>>();

    let healthy_count = peers.iter().filter(|peer| peer.status == "healthy").count();
    let peers_total = peers.len();

    (
        StatusCode::OK,
        Json(ClusterStatusResponse {
            node_id: repl_mgr.node_id().to_string(),
            replication_enabled: true,
            peers_total: Some(peers_total),
            peers_healthy: Some(healthy_count),
            peers,
        }),
    )
        .into_response()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClusterPeerStatus {
    pub peer_id: String,
    pub addr: String,
    pub status: String,
    pub last_success_secs_ago: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClusterStatusResponse {
    pub node_id: String,
    pub replication_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peers_total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peers_healthy: Option<usize>,
    pub peers: Vec<ClusterPeerStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct AddPeerRequest {
    pub node_id: String,
    pub addr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct AddPeerResponse {
    pub node_id: String,
    pub addr: String,
    pub peers_total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct RemovePeerResponse {
    pub node_id: String,
    pub peers_total: usize,
}

/// Add one peer to the live replication membership without changing bootstrap config.
#[utoipa::path(
    post,
    path = "/internal/cluster/peers",
    tag = "cluster",
    request_body = AddPeerRequest,
    responses(
        (status = 200, description = "Peer added to runtime membership", body = AddPeerResponse),
        (status = 400, description = "Invalid peer or replication is not configured"),
        (status = 403, description = "Admin API key is required"),
        (status = 409, description = "Peer conflicts with current runtime membership")
    ),
    security(("api_key" = []))
)]
pub async fn add_cluster_peer(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AddPeerRequest>,
) -> Response {
    let node_id = request.node_id.trim().to_string();
    if node_id.is_empty() {
        return crate::error_response::json_error(
            StatusCode::BAD_REQUEST,
            "node_id must not be blank",
        );
    }

    let Some(addr) = NodeConfig::normalize_peer_addr(&request.addr) else {
        return crate::error_response::json_error(
            StatusCode::BAD_REQUEST,
            "addr must be a safe HTTP or HTTPS peer URL",
        );
    };
    let Some(replication_manager) = state.replication_manager.as_ref() else {
        return crate::error_response::json_error(
            StatusCode::BAD_REQUEST,
            "replication manager is not configured",
        );
    };

    let receipt = match replication_manager.add_peer(PeerConfig { node_id, addr }) {
        Ok(receipt) => receipt,
        Err(error) => return add_peer_error_response(error),
    };

    (
        StatusCode::OK,
        Json(AddPeerResponse {
            node_id: receipt.node_id,
            addr: receipt.addr,
            peers_total: receipt.peers_total,
        }),
    )
        .into_response()
}

fn add_peer_error_response(error: AddPeerError) -> Response {
    match error {
        AddPeerError::Conflict(message) => {
            crate::error_response::json_error(StatusCode::CONFLICT, message)
        }
        AddPeerError::Persistence(error) => {
            tracing::error!(error = %error, "failed to persist replication peer membership");
            crate::error_response::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist replication peer membership",
            )
        }
    }
}

/// Remove one peer from the live replication membership without changing bootstrap config.
#[utoipa::path(
    delete,
    path = "/internal/cluster/peers/{node_id}",
    tag = "cluster",
    params(
        ("node_id" = String, Path, description = "Runtime peer node identifier")
    ),
    responses(
        (status = 200, description = "Peer removed from runtime membership", body = RemovePeerResponse),
        (status = 400, description = "Replication is not configured"),
        (status = 403, description = "Admin API key is required"),
        (status = 404, description = "Peer is not in runtime membership")
    ),
    security(("api_key" = []))
)]
pub async fn remove_cluster_peer(
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<String>,
) -> Response {
    let Some(replication_manager) = state.replication_manager.as_ref() else {
        return crate::error_response::json_error(
            StatusCode::BAD_REQUEST,
            "replication manager is not configured",
        );
    };

    match replication_manager.remove_peer(&node_id) {
        Ok(Some(receipt)) => (
            StatusCode::OK,
            Json(RemovePeerResponse {
                node_id: receipt.node_id,
                peers_total: receipt.peers_total,
            }),
        )
            .into_response(),
        Ok(None) => crate::error_response::json_error(
            StatusCode::NOT_FOUND,
            format!("Peer '{node_id}' not found"),
        ),
        Err(error) => {
            tracing::error!(error = %error, "failed to remove replication peer membership");
            crate::error_response::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist replication peer membership",
            )
        }
    }
}

/// POST /internal/rotate-admin-key
/// Generate a new admin key, update the in-memory KeyStore and persist to disk.
/// Returns the new plaintext key. Requires admin auth.
pub async fn rotate_admin_key(
    key_store: axum::Extension<Arc<crate::auth::KeyStore>>,
) -> impl IntoResponse {
    match key_store.rotate_admin_key() {
        Ok(new_key) => {
            tracing::info!(
                event = "security_audit_admin_action",
                admin_action = "rotate_admin_key",
                "security event: admin action"
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({ "key": new_key, "message": "Admin key rotated" })),
            )
                .into_response()
        }
        Err(e) => rotate_admin_key_error_response(&e),
    }
}

fn rotate_admin_key_error_response(error: &dyn std::fmt::Display) -> Response {
    tracing::error!(error = %error, "admin key rotation failed");
    crate::error_response::json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Failed to rotate admin key",
    )
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
#[path = "internal_tests.rs"]
mod tests;

/// POST /internal/analytics-rollup
///
/// Receive a pre-computed analytics rollup from a peer and store it in the
/// global rollup cache. Part of Phase 4 (HA Analytics Tier 2).
///
/// Protected by auth middleware in normal operation: `/internal/*` routes
/// require the admin key (see `required_acl_for_route`). In `--no-auth` local
/// dev mode, these routes are intentionally open.
pub async fn receive_analytics_rollup(
    Json(rollup): Json<crate::analytics_cluster::AnalyticsRollup>,
) -> impl IntoResponse {
    let cache = crate::analytics_cluster::get_global_rollup_cache();
    tracing::debug!(
        "[ROLLUP] received rollup from peer={} index={} generated_at={}",
        rollup.node_id,
        rollup.index,
        rollup.generated_at_secs
    );
    cache.store(rollup);
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

/// GET /internal/rollup-cache
///
/// Diagnostic endpoint: returns all entries currently stored in the global
/// rollup cache. Used by tests and operators to inspect the Tier 2 cache state.
///
/// Response: `{"count": N, "entries": [AnalyticsRollup, ...]}`
pub async fn rollup_cache_status() -> impl IntoResponse {
    let cache = crate::analytics_cluster::get_global_rollup_cache();
    let entries = cache.all_entries();
    let count = entries.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "count": count,
            "entries": entries
        })),
    )
        .into_response()
}

/// GET /internal/storage
/// Returns disk usage and doc count for all loaded tenants.
pub async fn storage_all(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let tenants: Vec<serde_json::Value> = state
        .manager
        .all_tenant_storage()
        .into_iter()
        .map(|(id, bytes)| {
            let doc_count = state.manager.tenant_doc_count(&id).unwrap_or(0);
            serde_json::json!({"id": id, "bytes": bytes, "doc_count": doc_count})
        })
        .collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({ "tenants": tenants })),
    )
        .into_response()
}

/// GET /internal/storage/:indexName
/// Returns disk usage and doc count for a specific tenant.
pub async fn storage_index(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
) -> impl IntoResponse {
    let bytes = state.manager.tenant_storage_bytes(&index_name);
    let doc_count = state.manager.tenant_doc_count(&index_name).unwrap_or(0);
    (
        StatusCode::OK,
        Json(serde_json::json!({ "index": index_name, "bytes": bytes, "doc_count": doc_count })),
    )
        .into_response()
}

/// GET /.well-known/acme-challenge/:token
/// ACME http-01 challenge handler for Let's Encrypt validation
pub async fn acme_challenge(
    Path(token): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    tracing::debug!("[SSL] ACME challenge request for token: {}", token);

    if let Some(ref ssl_mgr) = state.ssl_manager {
        if let Some(acme_client) = ssl_mgr.get_acme_client() {
            if let Some(response) = acme_client.get_challenge_response(&token) {
                tracing::info!("[SSL] Serving ACME challenge response for token: {}", token);
                return (StatusCode::OK, response).into_response();
            }
        }
    }

    tracing::warn!("[SSL] ACME challenge token not found: {}", token);
    (StatusCode::NOT_FOUND, "Challenge not found").into_response()
}

/// POST /internal/pause/:indexName
/// Mark an index as paused — writes will be rejected with 503.
pub async fn pause_index(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
) -> impl IntoResponse {
    state.paused_indexes.pause(&index_name);
    tracing::info!("[PAUSE] index '{}' paused", index_name);
    (
        StatusCode::OK,
        Json(serde_json::json!({"index": index_name, "paused": true})),
    )
        .into_response()
}

/// POST /internal/resume/:indexName
/// Clear the paused flag — writes resume normally.
pub async fn resume_index(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
) -> impl IntoResponse {
    state.paused_indexes.resume(&index_name);
    tracing::info!("[PAUSE] index '{}' resumed", index_name);
    (
        StatusCode::OK,
        Json(serde_json::json!({"index": index_name, "paused": false})),
    )
        .into_response()
}
