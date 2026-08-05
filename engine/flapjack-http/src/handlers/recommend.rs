//! HTTP handler for the batched recommendations endpoint, dispatching to trending-items, trending-facets, related-products, bought-together, and looking-similar models with validation, rule application, and replica resolution.
use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

use flapjack::error::FlapjackError;
use flapjack::recommend::cooccurrence::{self, EventFilter};
use flapjack::recommend::looking_similar;
use flapjack::recommend::rules;
use flapjack::recommend::trending;
use flapjack::recommend::{
    MAX_RECOMMENDATIONS_MAX, MAX_RECOMMENDATIONS_MIN, MODELS_REQUIRING_OBJECT_ID, THRESHOLD_MAX,
    THRESHOLD_MIN, VALID_MODELS,
};
use flapjack::validate_index_name;

use super::AppState;

// ── Request DTOs ────────────────────────────────────────────────────────────

/// Batched recommendations request body.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RecommendBatchRequest {
    pub requests: Vec<RecommendRequest>,
}

/// A single recommendation request within a batch.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecommendRequest {
    pub index_name: String,
    pub model: String,
    #[serde(default, rename = "objectID")]
    pub object_id: Option<String>,
    pub threshold: Option<u32>,
    #[serde(default)]
    pub max_recommendations: Option<u32>,
    #[serde(default)]
    pub facet_name: Option<String>,
    #[serde(default)]
    pub facet_value: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub query_parameters: Option<serde_json::Value>,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub fallback_parameters: Option<serde_json::Value>,
}

// ── Response DTOs ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RecommendBatchResponse {
    pub results: Vec<RecommendResult>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RecommendResult {
    #[schema(value_type = Vec<Object>)]
    pub hits: Vec<serde_json::Value>,
    #[serde(rename = "processingTimeMS")]
    pub processing_time_ms: u64,
}

// ── Validation ──────────────────────────────────────────────────────────────

/// Validate a single recommendation request, checking model name, threshold bounds, maxRecommendations range, required objectID for co-occurrence models, and required facetName for trending-facets.
///
/// # Returns
///
/// `Ok(())` when all constraints pass, or `Err(FlapjackError::InvalidQuery)` describing the first violation.
fn validate_request(req: &RecommendRequest) -> Result<(), FlapjackError> {
    // Validate index name to prevent path traversal
    validate_index_name(&req.index_name)?;

    // model must be one of the valid values
    if !VALID_MODELS.contains(&req.model.as_str()) {
        return Err(FlapjackError::InvalidQuery(format!(
            "Unsupported model: {}. Must be one of: {}",
            req.model,
            VALID_MODELS.join(", ")
        )));
    }

    // threshold is required
    let threshold = req
        .threshold
        .ok_or_else(|| FlapjackError::InvalidQuery("threshold is required".to_string()))?;

    if threshold > THRESHOLD_MAX {
        return Err(FlapjackError::InvalidQuery(format!(
            "threshold must be between {} and {}",
            THRESHOLD_MIN, THRESHOLD_MAX
        )));
    }

    // maxRecommendations validation (if provided)
    if let Some(max) = req.max_recommendations {
        if !(MAX_RECOMMENDATIONS_MIN..=MAX_RECOMMENDATIONS_MAX).contains(&max) {
            return Err(FlapjackError::InvalidQuery(format!(
                "maxRecommendations must be between {} and {}",
                MAX_RECOMMENDATIONS_MIN, MAX_RECOMMENDATIONS_MAX
            )));
        }
    }

    // objectID required for certain models
    if MODELS_REQUIRING_OBJECT_ID.contains(&req.model.as_str())
        && req
            .object_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err(FlapjackError::InvalidQuery(format!(
            "objectID is required for model '{}'",
            req.model
        )));
    }

    // facetName required for trending-facets
    if req.model == "trending-facets"
        && req
            .facet_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err(FlapjackError::InvalidQuery(
            "facetName is required for model 'trending-facets'".to_string(),
        ));
    }

    // queryParameters/fallbackParameters not supported for trending-facets
    if req.model == "trending-facets"
        && (req.query_parameters.is_some() || req.fallback_parameters.is_some())
    {
        return Err(FlapjackError::InvalidQuery(
            "queryParameters and fallbackParameters are not supported for model 'trending-facets'"
                .to_string(),
        ));
    }

    Ok(())
}

// ── Handler ─────────────────────────────────────────────────────────────────

/// POST /1/indexes/*/recommendations
#[utoipa::path(
    post,
    path = "/1/indexes/*/recommendations",
    tag = "recommend",
    request_body(content = RecommendBatchRequest, description = "Batched recommendation requests"),
    responses(
        (status = 200, description = "Recommendation results", body = RecommendBatchResponse)
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn recommend(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RecommendBatchRequest>,
) -> Result<Json<RecommendBatchResponse>, FlapjackError> {
    let mut results = Vec::with_capacity(body.requests.len());

    for req in &body.requests {
        validate_request(req)?;
        let target_index = resolve_recommend_data_index(&state, &req.index_name);

        let start = Instant::now();
        let max_recs = req
            .max_recommendations
            .unwrap_or(state.recommend_config.max_recommendations_default);
        let threshold = req.threshold.unwrap_or(0);

        let hits = match req.model.as_str() {
            "trending-items" => {
                dispatch_trending_items(&state, &target_index, req, threshold, max_recs).await?
            }
            "trending-facets" => {
                dispatch_trending_facets(&state, &target_index, req, threshold, max_recs).await?
            }
            "related-products" => {
                dispatch_cooccurrence(
                    &state,
                    &target_index,
                    req,
                    EventFilter::ClickAndConversion,
                    threshold,
                    max_recs,
                )
                .await?
            }
            "bought-together" => {
                dispatch_cooccurrence(
                    &state,
                    &target_index,
                    req,
                    EventFilter::PurchaseOnly,
                    threshold,
                    max_recs,
                )
                .await?
            }
            "looking-similar" => {
                dispatch_looking_similar(&state, &target_index, req, threshold, max_recs).await?
            }
            _ => unreachable!("validated above"),
        };

        // Apply recommend rules (promote/hide)
        let hits = apply_recommend_rules(&state.manager, &target_index, req, hits);

        let elapsed = start.elapsed().as_millis() as u64;
        results.push(RecommendResult {
            hits,
            processing_time_ms: elapsed,
        });
    }

    Ok(Json(RecommendBatchResponse { results }))
}

fn resolve_recommend_data_index(state: &Arc<AppState>, requested_index: &str) -> String {
    state
        .manager
        .get_settings(requested_index)
        .and_then(|settings| settings.primary.clone())
        .unwrap_or_else(|| requested_index.to_string())
}

// ── Model dispatch helpers ──────────────────────────────────────────────────

/// Compute trending-items recommendations by querying the analytics engine for conversion frequency weighted by recency.
///
/// # Arguments
///
/// * `state` - Shared application state containing the analytics engine and index manager.
/// * `index_name` - Target index (resolved to primary if replica).
/// * `req` - The originating recommend request, used for optional facet filtering.
/// * `threshold` - Minimum score (0–100) a hit must meet to be included.
/// * `max_recs` - Maximum number of hits to return.
///
/// # Returns
///
/// JSON hits sorted by descending trending score, each annotated with `_score`.
async fn dispatch_trending_items(
    state: &Arc<AppState>,
    index_name: &str,
    req: &RecommendRequest,
    threshold: u32,
    max_recs: u32,
) -> Result<Vec<serde_json::Value>, FlapjackError> {
    let analytics = state
        .analytics_engine
        .as_ref()
        .ok_or_else(|| FlapjackError::InvalidQuery("Analytics not enabled".to_string()))?;

    let hits = trending::compute_trending_items(
        analytics,
        &state.manager,
        index_name,
        state.recommend_config.trending_window_days,
        req.facet_name.as_deref().map(|name| trending::FacetFilter {
            name,
            value: req.facet_value.as_deref(),
        }),
        threshold,
        max_recs,
    )
    .await
    .map_err(FlapjackError::InvalidQuery)?;

    Ok(hits
        .into_iter()
        .map(|h| {
            let mut hit = doc_to_hit_json(h.document.as_ref(), &h.object_id);
            hit["_score"] = serde_json::json!(h.score);
            hit
        })
        .collect())
}

/// Compute trending-facets recommendations by aggregating conversion events per facet value for the given facet name.
///
/// # Arguments
///
/// * `state` - Shared application state containing the analytics engine and index manager.
/// * `index_name` - Target index (resolved to primary if replica).
/// * `req` - The originating recommend request; `facet_name` must be set.
/// * `threshold` - Minimum score (0–100) a facet hit must meet.
/// * `max_recs` - Maximum number of facet hits to return.
///
/// # Returns
///
/// JSON objects with `facetName`, `facetValue`, and `_score` fields, sorted by descending score.
async fn dispatch_trending_facets(
    state: &Arc<AppState>,
    index_name: &str,
    req: &RecommendRequest,
    threshold: u32,
    max_recs: u32,
) -> Result<Vec<serde_json::Value>, FlapjackError> {
    let analytics = state
        .analytics_engine
        .as_ref()
        .ok_or_else(|| FlapjackError::InvalidQuery("Analytics not enabled".to_string()))?;

    let facet_name = req.facet_name.as_deref().unwrap_or_default();

    let hits = trending::compute_trending_facets(
        analytics,
        &state.manager,
        index_name,
        state.recommend_config.trending_window_days,
        facet_name,
        threshold,
        max_recs,
    )
    .await
    .map_err(FlapjackError::InvalidQuery)?;

    Ok(hits
        .into_iter()
        .map(|h| {
            serde_json::json!({
                "facetName": h.facet_name,
                "facetValue": h.facet_value,
                "_score": h.score,
            })
        })
        .collect())
}

/// Compute co-occurrence recommendations (related-products or bought-together) by analyzing which items appear together in user sessions.
///
/// # Arguments
///
/// * `state` - Shared application state containing the analytics engine and index manager.
/// * `index_name` - Target index (resolved to primary if replica).
/// * `req` - The originating recommend request; `object_id` must be set.
/// * `event_filter` - Whether to consider all click/conversion events or only purchase events.
/// * `threshold` - Minimum co-occurrence score (0–100) for inclusion.
/// * `max_recs` - Maximum number of hits to return.
///
/// # Returns
///
/// JSON hits sorted by descending co-occurrence score, excluding the seed objectID.
async fn dispatch_cooccurrence(
    state: &Arc<AppState>,
    index_name: &str,
    req: &RecommendRequest,
    event_filter: EventFilter,
    threshold: u32,
    max_recs: u32,
) -> Result<Vec<serde_json::Value>, FlapjackError> {
    let analytics = state
        .analytics_engine
        .as_ref()
        .ok_or_else(|| FlapjackError::InvalidQuery("Analytics not enabled".to_string()))?;

    let seed_id = req.object_id.as_deref().unwrap_or_default();

    let hits = cooccurrence::compute_cooccurrence(
        analytics,
        &state.manager,
        index_name,
        seed_id,
        event_filter,
        threshold,
        max_recs,
    )
    .await
    .map_err(FlapjackError::InvalidQuery)?;

    Ok(hits
        .into_iter()
        .map(|h| {
            let mut hit = doc_to_hit_json(h.document.as_ref(), &h.object_id);
            hit["_score"] = serde_json::json!(h.score);
            hit
        })
        .collect())
}

/// Dispatch looking-similar recommendations through the async HTTP boundary.
///
/// The compute work is offloaded to a blocking task. The response contains JSON
/// hits ranked by descending similarity, excludes the seed, and uses term
/// similarity only when vector similarity is unavailable.
///
/// # Arguments
///
/// * `state` - Shared application state containing the index manager.
/// * `index_name` - Target index (resolved to primary if replica).
/// * `req` - The originating recommend request; `object_id` must be set.
/// * `threshold` - Minimum similarity score (0-100) for inclusion.
/// * `max_recs` - Maximum number of hits to return.
///
/// # Returns
///
/// JSON hits ranked by descending similarity.
async fn dispatch_looking_similar(
    state: &Arc<AppState>,
    index_name: &str,
    req: &RecommendRequest,
    threshold: u32,
    max_recs: u32,
) -> Result<Vec<serde_json::Value>, FlapjackError> {
    let state = Arc::clone(state);
    let index_name = index_name.to_string();
    let seed_id = req.object_id.clone().unwrap_or_default();
    let hits = tokio::task::spawn_blocking(move || {
        looking_similar::compute_looking_similar(
            &state.manager,
            &index_name,
            &seed_id,
            threshold,
            max_recs,
        )
    })
    .await
    .map_err(|error| FlapjackError::InvalidQuery(format!("spawn_blocking join error: {error}")))?
    .map_err(FlapjackError::InvalidQuery)?;

    Ok(hits
        .into_iter()
        .map(|h| {
            let mut hit = doc_to_hit_json(h.document.as_ref(), &h.object_id);
            hit["_score"] = serde_json::json!(h.score);
            hit
        })
        .collect())
}

/// Convert an optional Document to JSON hit format, including objectID.
fn doc_to_hit_json(doc: Option<&flapjack::types::Document>, object_id: &str) -> serde_json::Value {
    match doc {
        Some(d) => {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "objectID".to_string(),
                serde_json::Value::String(object_id.to_string()),
            );
            for (key, value) in &d.fields {
                obj.insert(
                    key.clone(),
                    flapjack::types::field_value_to_json_value(value),
                );
            }
            serde_json::Value::Object(obj)
        }
        None => {
            serde_json::json!({ "objectID": object_id })
        }
    }
}

// ── Rules application ────────────────────────────────────────────────────────

/// Load recommend rules for the given index+model and apply hide/promote consequences.
fn apply_recommend_rules(
    manager: &flapjack::IndexManager,
    index_name: &str,
    req: &RecommendRequest,
    mut hits: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let loaded = match rules::load_rules(&manager.base_path, index_name, &req.model) {
        Ok(r) => r,
        Err(_) => return hits, // If rules can't be loaded, return hits unchanged
    };

    let active_rules: Vec<_> = loaded
        .into_iter()
        .filter(|r| r.enabled && rule_matches_request(r, req))
        .collect();
    if active_rules.is_empty() {
        return hits;
    }

    // Collect all hidden objectIDs
    let hidden_ids: std::collections::HashSet<String> = active_rules
        .iter()
        .filter_map(|r| r.consequence.as_ref())
        .filter_map(|c| c.hide.as_ref())
        .flat_map(|hides| hides.iter().map(|h| h.object_id.clone()))
        .collect();

    // Remove hidden hits
    if !hidden_ids.is_empty() {
        hits.retain(|h| {
            h.get("objectID")
                .and_then(|v| v.as_str())
                .map(|id| !hidden_ids.contains(id))
                .unwrap_or(true)
        });
    }

    // Collect all promoted items (sorted by position for correct insertion)
    let mut promotions: Vec<(usize, String)> = active_rules
        .iter()
        .filter_map(|r| r.consequence.as_ref())
        .filter_map(|c| c.promote.as_ref())
        .flat_map(|promos| promos.iter().map(|p| (p.position, p.object_id.clone())))
        .collect();
    promotions.sort_by_key(|(pos, _)| *pos);

    // Insert promoted items at their specified positions
    for (position, object_id) in promotions {
        // Reuse existing hit when present to preserve payload fields.
        let mut promoted_hit = if let Some(existing_pos) = hits.iter().position(|h| {
            h.get("objectID")
                .and_then(|v| v.as_str())
                .map(|id| id == object_id)
                .unwrap_or(false)
        }) {
            hits.remove(existing_pos)
        } else {
            // Otherwise, hydrate from stored document if available.
            let doc = manager.get_document(index_name, &object_id).ok().flatten();
            doc_to_hit_json(doc.as_ref(), &object_id)
        };
        promoted_hit["_score"] = serde_json::json!(100);
        let insert_pos = position.min(hits.len());
        hits.insert(insert_pos, promoted_hit);
    }

    hits
}

/// Returns `true` when a rule's `condition` is satisfied by the request.
/// - No condition: always matches.
/// - `filters`: exact, trimmed string match against `queryParameters.filters`.
/// - `context`: request must contain a matching value in `queryParameters.ruleContexts`
///   (string or array), or `queryParameters.context`.
fn rule_matches_request(rule: &rules::RecommendRule, req: &RecommendRequest) -> bool {
    let Some(condition) = rule.condition.as_ref() else {
        return true;
    };

    if let Some(condition_filters) = condition.filters.as_ref() {
        let requested_filters = get_query_parameter_value(req, "filters");
        match requested_filters.as_deref() {
            Some(requested) if requested == condition_filters.trim() => {}
            _ => return false,
        }
    }

    if let Some(condition_context) = condition.context.as_ref() {
        let condition_context = condition_context.trim();
        let requested_contexts = get_rule_context_values(req);
        if !requested_contexts.iter().any(|c| c == condition_context) {
            return false;
        }
    }

    true
}

fn get_query_parameter_value(req: &RecommendRequest, key: &str) -> Option<String> {
    req.query_parameters
        .as_ref()
        .and_then(|params| params.get(key))
        .and_then(|value| value.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Extract rule context values from the request's `queryParameters.ruleContexts` (string or array) and `queryParameters.context` fields.
///
/// # Returns
///
/// A vec of trimmed, non-empty context strings found in the request. Returns an empty vec if no query parameters or context values are present.
fn get_rule_context_values(req: &RecommendRequest) -> Vec<String> {
    let Some(params) = req.query_parameters.as_ref() else {
        return Vec::new();
    };

    let mut contexts = Vec::new();

    if let Some(context_value) = params.get("ruleContexts") {
        match context_value {
            serde_json::Value::String(v) => {
                let context = v.trim();
                if !context.is_empty() {
                    contexts.push(context.to_string());
                }
            }
            serde_json::Value::Array(values) => {
                for item in values {
                    if let Some(context) = item.as_str() {
                        let context = context.trim();
                        if !context.is_empty() {
                            contexts.push(context.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(context_value) = params.get("context") {
        if let Some(context) = context_value.as_str() {
            let context = context.trim();
            if !context.is_empty() {
                contexts.push(context.to_string());
            }
        }
    }

    contexts
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "recommend_tests.rs"]
mod tests;
