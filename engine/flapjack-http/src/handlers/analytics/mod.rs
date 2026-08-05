//! Analytics HTTP handlers implementing Algolia-compatible analytics endpoints with cluster fan-out support, input validation, and comprehensive contract-parity tests.
use axum::{extract::State, http::HeaderMap, Json};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;

pub use super::analytics_dto::*;

use flapjack::analytics::AnalyticsQueryEngine;
use flapjack::error::FlapjackError;
use flapjack::index::manager::validate_index_name;

use super::AppState;
use crate::auth::{
    invalid_api_credentials_flapjack_error, key_allows_index, ApiKey, SecuredKeyRestrictions,
};

mod read_endpoints;
pub use read_endpoints::*;

mod geo_endpoints;
pub use geo_endpoints::*;

/// Maximum limit for analytics query results.
const MAX_ANALYTICS_LIMIT: usize = 10_000;

/// Clamp a user-supplied limit to prevent DoS via unbounded result sets.
pub(crate) fn clamp_limit(limit: usize) -> usize {
    limit.min(MAX_ANALYTICS_LIMIT)
}

/// Validate an analytics index name, converting the `String` error from
/// [`validate_index_name`] into [`FlapjackError::InvalidQuery`].
///
/// Kept as a wrapper because all 25+ analytics call sites need the same
/// error-type conversion; inlining the `.map_err` everywhere would be less DRY.
pub(crate) fn validate_analytics_index(index: &str) -> Result<(), FlapjackError> {
    validate_index_name(index).map_err(|e| FlapjackError::InvalidQuery(e.to_string()))
}

pub(crate) fn enforce_analytics_index_access(
    api_key: Option<&ApiKey>,
    secured_restrictions: Option<&SecuredKeyRestrictions>,
    index: &str,
) -> Result<(), FlapjackError> {
    if let Some(api_key) = api_key {
        if !key_allows_index(api_key, secured_restrictions, index) {
            return Err(invalid_api_credentials_flapjack_error());
        }
    }
    Ok(())
}

pub(crate) fn analytics_request_has_index_restrictions(
    api_key: Option<&ApiKey>,
    secured_restrictions: Option<&SecuredKeyRestrictions>,
) -> bool {
    api_key.is_some_and(|key| !key.indexes.is_empty())
        || secured_restrictions
            .and_then(|restrictions| restrictions.restrict_indices.as_ref())
            .is_some_and(|indices| !indices.is_empty())
}

/// Validate that `end_date` is not before `start_date`. Returns 400 for inverted ranges.
pub(crate) fn validate_date_range(start_date: &str, end_date: &str) -> Result<(), FlapjackError> {
    let start = chrono::NaiveDate::parse_from_str(start_date, "%Y-%m-%d").map_err(|e| {
        FlapjackError::InvalidQuery(format!("Invalid startDate '{}': {}", start_date, e))
    })?;
    let end = chrono::NaiveDate::parse_from_str(end_date, "%Y-%m-%d").map_err(|e| {
        FlapjackError::InvalidQuery(format!("Invalid endDate '{}': {}", end_date, e))
    })?;
    if end < start {
        return Err(FlapjackError::InvalidQuery(format!(
            "endDate '{}' must not be before startDate '{}'",
            end_date, start_date
        )));
    }
    Ok(())
}

/// Shared query parameters for all analytics endpoints.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsParams {
    pub index: String,
    #[serde(default = "default_start_date")]
    pub start_date: String,
    #[serde(default = "default_end_date")]
    pub end_date: String,
    #[serde(default)]
    pub tags: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub click_analytics: Option<bool>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub order_by: Option<String>,
}

/// Query parameters for the overview endpoint (no index required).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewParams {
    #[serde(default)]
    pub index: Option<String>,
    #[serde(default = "default_start_date")]
    pub start_date: String,
    #[serde(default = "default_end_date")]
    pub end_date: String,
}

fn default_start_date() -> String {
    (chrono::Utc::now() - chrono::Duration::days(8))
        .format("%Y-%m-%d")
        .to_string()
}

fn default_end_date() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Extract the `index` query parameter from a raw query string (e.g. "index=foo&startDate=...").
fn extract_index_from_query(raw_query: &str) -> Option<String> {
    raw_query.split('&').find_map(|pair| {
        let mut parts = pair.splitn(2, '=');
        match (parts.next(), parts.next()) {
            (Some("index"), Some(v)) => {
                Some(urlencoding::decode(v).unwrap_or_default().into_owned())
            }
            _ => None,
        }
    })
}

/// If cluster mode and not local-only, fan out query to peers and merge results.
///
/// Tier 2 (Phase 4): when all peers have fresh rollups in the rollup cache, merges
/// locally without any live HTTP fan-out (lower latency, tolerates peer restarts).
///
/// Tier 1 fallback: when rollups are stale or absent, queries all peers via HTTP
/// and merges the responses (existing behaviour, always correct).
///
/// Returns local result unchanged in standalone mode or when X-Flapjack-Local-Only is set.
pub(crate) async fn maybe_fan_out(
    headers: &HeaderMap,
    endpoint: &str,
    path: &str,
    raw_query: &str,
    local_result: serde_json::Value,
    limit: usize,
) -> serde_json::Value {
    // Skip fan-out if local-only header present (peer-to-peer query)
    if headers.get("X-Flapjack-Local-Only").is_some() {
        return local_result;
    }
    // No cluster client configured → standalone mode
    let cluster = match crate::analytics_cluster::get_global_cluster() {
        Some(c) => c,
        None => return local_result,
    };

    // Phase 4 Tier 2: serve from rollup cache when all peers have fresh snapshots.
    // This avoids cross-region HTTP fan-out for globally distributed clusters.
    let peer_ids = cluster.peer_ids();
    if let Some(index) = extract_index_from_query(raw_query) {
        let cache = crate::analytics_cluster::get_global_rollup_cache();
        if cache.all_fresh(
            &peer_ids,
            &index,
            crate::analytics_cluster::ROLLUP_MAX_AGE_SECS,
        ) {
            tracing::debug!(
                "[ANALYTICS] using rollup cache for endpoint={} index={}",
                endpoint,
                index
            );
            let peer_rollups = cache.all_for_index(&index);
            let mut all_results = vec![local_result];
            for rollup in &peer_rollups {
                if let Some(result) = rollup.results.get(endpoint) {
                    all_results.push(result.clone());
                }
            }
            return flapjack::analytics::merge::merge_results(endpoint, &all_results, limit);
        }
    }

    // Tier 1 fallback: live fan-out to all available peers
    cluster
        .fan_out_and_merge(endpoint, path, raw_query, local_result, limit, headers)
        .await
}

/// POST /2/analytics/seed - Seed demo analytics data for an index (local only, no fan-out)
#[utoipa::path(
    post,
    path = "/2/analytics/seed",
    tag = "analytics-operations",
    request_body = SeedRequest,
    responses((status = 200, description = "Seeded analytics data summary", body = AnalyticsSeedResponse)),
    security(("api_key" = []))
)]
pub async fn seed_analytics(
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    Json(body): Json<SeedRequest>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let index = body
        .index
        .ok_or_else(|| FlapjackError::InvalidQuery("Missing 'index' field".to_string()))?;
    validate_analytics_index(&index)?;
    let options = flapjack::analytics::seed::AnalyticsSeedOptions {
        days: body.days.unwrap_or(30).min(90),
        search_count: body.search_count,
        no_result_rate: body.no_result_rate,
        device_distribution: body.device_distribution,
        country_distribution: body.country_distribution,
    };
    flapjack::analytics::seed::validate_seed_options(&options)
        .map_err(|error| FlapjackError::InvalidQuery(format!("Seed error: {error}")))?;

    let config = engine.config().clone();
    let seed_index = index.clone();
    let result = tokio::task::spawn_blocking(move || {
        flapjack::analytics::seed::seed_analytics_with_options(&config, &seed_index, &options)
    })
    .await
    .map_err(|error| FlapjackError::Io(format!("Seed task error: {error}")))?
    .map_err(|error| FlapjackError::Io(format!("Seed error: {error}")))?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "index": index,
        "days": result.days,
        "seededDates": result.seeded_dates,
        "totalSearches": result.total_searches,
        "totalClicks": result.total_clicks,
        "totalConversions": result.total_conversions,
    })))
}

/// POST /2/analytics/flush - Flush buffered analytics events to disk immediately (local only)
#[utoipa::path(
    post,
    path = "/2/analytics/flush",
    tag = "analytics-operations",
    responses((status = 200, description = "Flush status", body = AnalyticsFlushResponse)),
    security(("api_key" = []))
)]
pub async fn flush_analytics() -> Result<Json<serde_json::Value>, FlapjackError> {
    if let Some(collector) = flapjack::analytics::get_global_collector() {
        collector.flush_all();
        Ok(Json(serde_json::json!({ "status": "ok" })))
    } else {
        Ok(Json(
            serde_json::json!({ "status": "ok", "note": "analytics not initialized" }),
        ))
    }
}

/// DELETE /2/analytics/clear - Clear all analytics data for an index (local only)
#[utoipa::path(
    delete,
    path = "/2/analytics/clear",
    tag = "analytics-operations",
    request_body = ClearRequest,
    responses((status = 200, description = "Analytics clear status", body = AnalyticsClearResponse)),
    security(("api_key" = []))
)]
pub async fn clear_analytics(
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    Json(body): Json<ClearRequest>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let index = body
        .index
        .ok_or_else(|| FlapjackError::InvalidQuery("Missing 'index' field".to_string()))?;
    validate_analytics_index(&index)?;

    let config = engine.config().clone();
    let clear_index = index.clone();
    let removed = tokio::task::spawn_blocking(move || {
        flapjack::analytics::seed::clear_analytics(&config, &clear_index)
    })
    .await
    .map_err(|error| FlapjackError::Io(format!("Clear task error: {error}")))?
    .map_err(|error| FlapjackError::Io(format!("Clear error: {error}")))?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "index": index,
        "partitionsRemoved": removed,
    })))
}

/// POST /2/analytics/cleanup - Remove analytics data for indexes that no longer exist (local only)
#[utoipa::path(
    post,
    path = "/2/analytics/cleanup",
    tag = "analytics-operations",
    responses((status = 200, description = "Cleanup summary", body = AnalyticsCleanupResponse)),
    security(("api_key" = []))
)]
pub async fn cleanup_analytics(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let engine = state
        .analytics_engine
        .as_ref()
        .ok_or_else(|| FlapjackError::InvalidQuery("Analytics not available".to_string()))?;

    // Get analytics index names
    let analytics_indices = engine
        .list_analytics_indices()
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;

    // Get active index names from the IndexManager's base_path
    let mut active_indices: HashSet<String> = HashSet::new();
    if state.manager.base_path.exists() {
        if let Ok(entries) = std::fs::read_dir(&state.manager.base_path) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        active_indices.insert(name.to_string());
                    }
                }
            }
        }
    }

    // Diff: orphaned = analytics_indices - active_indices
    let orphaned: Vec<String> = analytics_indices
        .into_iter()
        .filter(|name| !active_indices.contains(name))
        .collect();

    // Delete analytics directories for orphaned indexes
    let config = engine.config();
    for index_name in &orphaned {
        let index_dir = config.data_dir.join(index_name);
        if index_dir.exists() {
            let _ = std::fs::remove_dir_all(&index_dir);
        }
    }

    let count = orphaned.len();
    Ok(Json(serde_json::json!({
        "status": "ok",
        "removedIndices": orphaned,
        "removedCount": count,
    })))
}

#[cfg(test)]
#[path = "../analytics_tests.rs"]
mod tests;
