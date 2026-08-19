//! Stub summary for rules.rs.
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

use super::{
    index_resource_store::{
        clear_resource_store, delete_resource_item, forward_store_to_replicas, load_existing_store,
        load_store_or_empty, save_resource_batch, save_resource_item,
    },
    safe_nb_pages,
    settings::parse_bool_query_param,
    AppState,
};
use crate::error_response::HandlerError;
use crate::extractors::{validate_index_http, ValidatedIndexName};
use flapjack::index::rules::{Rule, RuleStore};

/// Retrieve a query rule by its object ID from the specified index.
#[utoipa::path(
    get,
    path = "/1/indexes/{indexName}/rules/{objectID}",
    tag = "rules",
    params(
        ("indexName" = String, Path, description = "Index name"),
        ("objectID" = String, Path, description = "Rule ID")
    ),
    responses(
        (status = 200, description = "Rule retrieved", body = serde_json::Value),
        (status = 404, description = "Rule not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn get_rule(
    State(state): State<Arc<AppState>>,
    Path((index_name, object_id)): Path<(String, String)>,
) -> Result<Json<Rule>, HandlerError> {
    validate_index_http(&index_name)?;
    let Some(store) = load_existing_store::<RuleStore>(state.manager.as_ref(), &index_name)
        .map_err(HandlerError::internal)?
    else {
        return Err(HandlerError::not_found(format!(
            "Rule {} not found",
            object_id
        )));
    };

    store
        .get(&object_id)
        .cloned()
        .ok_or_else(|| HandlerError::not_found(format!("Rule {} not found", object_id)))
        .map(Json)
}

/// Save or update a single query rule in the specified index.
#[utoipa::path(
    put,
    path = "/1/indexes/{indexName}/rules/{objectID}",
    tag = "rules",
    params(
        ("indexName" = String, Path, description = "Index name"),
        ("objectID" = String, Path, description = "Rule ID")
    ),
    request_body(content = serde_json::Value, description = "Rule data"),
    responses(
        (status = 200, description = "Rule saved", body = serde_json::Value)
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn save_rule(
    State(state): State<Arc<AppState>>,
    Path((index_name, _object_id)): Path<(String, String)>,
    Json(rule): Json<Rule>,
) -> Result<Json<serde_json::Value>, HandlerError> {
    validate_index_http(&index_name)?;
    save_resource_item::<RuleStore>(state.manager.as_ref(), &index_name, rule.clone())
        .map_err(HandlerError::internal)?;

    state.manager.append_oplog(
        &index_name,
        "save_rule",
        serde_json::to_value(&rule).unwrap_or_default(),
    );

    let task = state
        .manager
        .make_noop_task(&index_name)
        .map_err(HandlerError::internal)?;
    Ok(Json(serde_json::json!({
        "taskID": task.numeric_id,
        "updatedAt": chrono::Utc::now().to_rfc3339(),
        "id": rule.object_id
    })))
}

/// Delete a query rule from the specified index by its object ID.
#[utoipa::path(
    delete,
    path = "/1/indexes/{indexName}/rules/{objectID}",
    tag = "rules",
    params(
        ("indexName" = String, Path, description = "Index name"),
        ("objectID" = String, Path, description = "Rule ID")
    ),
    responses(
        (status = 200, description = "Rule deleted", body = serde_json::Value),
        (status = 404, description = "Rule not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn delete_rule(
    State(state): State<Arc<AppState>>,
    Path((index_name, object_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, HandlerError> {
    validate_index_http(&index_name)?;
    if !delete_resource_item::<RuleStore>(state.manager.as_ref(), &index_name, &object_id)
        .map_err(HandlerError::internal)?
    {
        return Err(HandlerError::not_found(format!(
            "Rule {} not found",
            object_id
        )));
    }

    state.manager.append_oplog(
        &index_name,
        "delete_rule",
        serde_json::json!({"objectID": object_id}),
    );

    let task = state
        .manager
        .make_noop_task(&index_name)
        .map_err(HandlerError::internal)?;
    Ok(Json(serde_json::json!({
        "taskID": task.numeric_id,
        "deletedAt": chrono::Utc::now().to_rfc3339()
    })))
}

/// Batch create or update multiple query rules in the specified index.
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/rules/batch",
    tag = "rules",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    request_body(content = serde_json::Value, description = "Array of rules"),
    responses(
        (status = 200, description = "Rules saved", body = serde_json::Value),
        (status = 400, description = "Invalid query parameter value")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn save_rules(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
    Query(params): Query<HashMap<String, String>>,
    Json(rules): Json<Vec<Rule>>,
) -> Result<Json<serde_json::Value>, HandlerError> {
    let clear_existing = parse_bool_query_param(&params, "clearExistingRules")?;
    let rules_json: Vec<serde_json::Value> = rules
        .iter()
        .map(|r| serde_json::to_value(r).unwrap_or_default())
        .collect();
    let store = save_resource_batch::<RuleStore, _>(
        state.manager.as_ref(),
        &index_name,
        rules,
        clear_existing,
    )
    .map_err(HandlerError::internal)?;

    // Forward rules to replicas if requested
    let forward_to_replicas = parse_bool_query_param(&params, "forwardToReplicas")?;
    if forward_to_replicas {
        forward_store_to_replicas::<RuleStore>(state.manager.as_ref(), &index_name, &store)
            .map_err(HandlerError::internal)?;
    }

    state.manager.append_oplog(
        &index_name,
        "save_rules",
        serde_json::json!({"rules": rules_json, "clearExisting": clear_existing}),
    );

    let task = state
        .manager
        .make_noop_task(&index_name)
        .map_err(HandlerError::internal)?;
    Ok(Json(serde_json::json!({
        "taskID": task.numeric_id,
        "updatedAt": chrono::Utc::now().to_rfc3339()
    })))
}

/// Remove all query rules from the specified index.
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/rules/clear",
    tag = "rules",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    responses(
        (status = 200, description = "Rules cleared", body = serde_json::Value)
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn clear_rules(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
) -> Result<Json<serde_json::Value>, HandlerError> {
    clear_resource_store::<RuleStore>(state.manager.as_ref(), &index_name)
        .map_err(HandlerError::internal)?;
    state
        .manager
        .append_oplog(&index_name, "clear_rules", serde_json::json!({}));

    let task = state
        .manager
        .make_noop_task(&index_name)
        .map_err(HandlerError::internal)?;
    Ok(Json(serde_json::json!({
        "taskID": task.numeric_id,
        "updatedAt": chrono::Utc::now().to_rfc3339()
    })))
}

#[derive(Deserialize)]
pub struct SearchRulesRequest {
    #[serde(default)]
    pub query: String,

    #[serde(default)]
    pub page: usize,

    #[serde(rename = "hitsPerPage", default = "default_hits_per_page")]
    pub hits_per_page: usize,
}

fn default_hits_per_page() -> usize {
    20
}

/// Search for query rules in the specified index with optional filtering.
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/rules/search",
    tag = "rules",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    request_body(content = serde_json::Value, description = "Search parameters"),
    responses(
        (status = 200, description = "Matching rules", body = serde_json::Value)
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn search_rules(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
    Json(req): Json<SearchRulesRequest>,
) -> Result<Json<serde_json::Value>, HandlerError> {
    let store = load_store_or_empty::<RuleStore>(state.manager.as_ref(), &index_name)
        .map_err(HandlerError::internal)?;

    let (hits, total) = store.search(&req.query, req.page, req.hits_per_page);
    let nb_pages = safe_nb_pages(total, req.hits_per_page);

    Ok(Json(serde_json::json!({
        "hits": hits,
        "nbHits": total,
        "page": req.page,
        "nbPages": nb_pages
    })))
}
