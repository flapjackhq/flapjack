//! Algolia Insights API-compatible event ingestion, debug event inspection, and GDPR user token deletion handlers.
use axum::{
    extract::{Path, Query, State},
    Extension, Json,
};
use std::sync::Arc;

use flapjack::analytics::schema::{validate_user_token, InsightEvent};
use flapjack::analytics::{AnalyticsCollector, DebugEvent};
use flapjack::error::FlapjackError;

use super::analytics::enforce_analytics_index_access;
use crate::auth::{key_allows_index, ApiKey, SecuredKeyRestrictions};

const DEBUG_EVENTS_DEFAULT_LIMIT: usize = 100;
const DEBUG_EVENTS_MAX_LIMIT: usize = 1000;
const DEBUG_EVENTS_LIMIT_ERROR: &str = "limit must be a positive integer between 1 and 1000";
const DEBUG_EVENTS_TIME_ERROR: &str =
    "from and until must be non-negative unix timestamps in milliseconds";
const DEBUG_EVENTS_TIME_RANGE_ERROR: &str = "from must be less than or equal to until";

/// POST /1/events - Algolia Insights API compatible event ingestion
#[utoipa::path(post, path = "/1/events", tag = "insights", security(("api_key" = [])))]
pub async fn post_events(
    State(collector): State<Arc<AnalyticsCollector>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    Json(body): Json<InsightsRequest>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    if body.events.len() > 1000 {
        return Err(FlapjackError::InvalidQuery(
            "Maximum 1000 events per request".to_string(),
        ));
    }

    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);

    // Authorize every target before recording anything. A mixed-index request is
    // one atomic authorization decision; otherwise an allowed prefix could be
    // persisted before a later forbidden event rejects the batch.
    for event in &body.events {
        enforce_analytics_index_access(api_key, secured_restrictions, &event.index)?;
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut accepted = 0;
    let mut errors: Vec<String> = Vec::new();

    for event in body.events {
        let debug_entry = |http_code: u16, validation_errors: Vec<String>| DebugEvent {
            timestamp_ms: event.timestamp.unwrap_or(now_ms),
            index: event.index.clone(),
            event_type: event.event_type.clone(),
            event_subtype: event.event_subtype.clone(),
            event_name: event.event_name.clone(),
            user_token: event.user_token.clone(),
            object_ids: event.effective_object_ids().to_vec(),
            http_code,
            validation_errors,
        };

        match event.validate() {
            Ok(()) => {
                collector.record_debug_event(debug_entry(200, vec![]));
                collector.record_insight(event);
                accepted += 1;
            }
            Err(e) => {
                collector.record_debug_event(debug_entry(422, vec![e.clone()]));
                errors.push(e);
            }
        }
    }

    if !errors.is_empty() && accepted == 0 {
        return Err(FlapjackError::InvalidQuery(format!(
            "All events rejected: {}",
            errors.join("; ")
        )));
    }

    Ok(Json(serde_json::json!({
        "status": 200,
        "message": "OK"
    })))
}

/// GET /1/events/debug - Return recent events from the debug ring buffer
#[utoipa::path(get, path = "/1/events/debug", tag = "insights", security(("api_key" = [])))]
pub async fn get_debug_events(
    State(collector): State<Arc<AnalyticsCollector>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    Query(params): Query<DebugEventsQuery>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    if let Some(status) = params.status.as_deref() {
        if !matches!(status, "ok" | "error") {
            return Err(FlapjackError::InvalidQuery(
                "status must be one of: ok, error".to_string(),
            ));
        }
    }

    let limit = parse_debug_limit(params.limit.as_deref())?;
    let from_timestamp_ms = parse_debug_timestamp(params.from.as_deref())?;
    let until_timestamp_ms = parse_debug_timestamp(params.until.as_deref())?;
    if let (Some(from_ms), Some(until_ms)) = (from_timestamp_ms, until_timestamp_ms) {
        if from_ms > until_ms {
            return Err(FlapjackError::InvalidQuery(
                DEBUG_EVENTS_TIME_RANGE_ERROR.to_string(),
            ));
        }
    }

    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    if let Some(index) = params.index.as_deref() {
        enforce_analytics_index_access(api_key, secured_restrictions, index)?;
    }

    // Fetch all matching ring-buffer entries so the caller's limit is applied
    // after tenant filtering. Limiting first would let recent forbidden events
    // starve older allowed events from an otherwise valid response.
    let mut events = collector.get_debug_events(
        usize::MAX,
        params.index.as_deref(),
        params.event_type.as_deref(),
        params.status.as_deref(),
        from_timestamp_ms,
        until_timestamp_ms,
    );
    if let Some(api_key) = api_key {
        events.retain(|event| key_allows_index(api_key, secured_restrictions, &event.index));
    }
    events.truncate(limit);

    Ok(Json(serde_json::json!({
        "events": events,
        "count": events.len(),
    })))
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugEventsQuery {
    pub limit: Option<String>,
    pub index: Option<String>,
    pub event_type: Option<String>,
    pub status: Option<String>,
    pub from: Option<String>,
    pub until: Option<String>,
}

/// Parse and validate the `limit` query parameter for the debug events endpoint.
///
/// Returns `DEBUG_EVENTS_DEFAULT_LIMIT` when `limit` is `None`. Clamps valid values
/// to `DEBUG_EVENTS_MAX_LIMIT`.
///
/// # Returns
///
/// The parsed limit clamped to `[1, 1000]`, or a validation error for zero,
/// negative, or non-numeric input.
fn parse_debug_limit(limit: Option<&str>) -> Result<usize, FlapjackError> {
    let Some(raw_limit) = limit else {
        return Ok(DEBUG_EVENTS_DEFAULT_LIMIT);
    };

    let parsed_limit = raw_limit
        .parse::<usize>()
        .map_err(|_| FlapjackError::InvalidQuery(DEBUG_EVENTS_LIMIT_ERROR.to_string()))?;

    if parsed_limit == 0 {
        return Err(FlapjackError::InvalidQuery(
            DEBUG_EVENTS_LIMIT_ERROR.to_string(),
        ));
    }

    Ok(parsed_limit.min(DEBUG_EVENTS_MAX_LIMIT))
}

fn parse_debug_timestamp(value: Option<&str>) -> Result<Option<i64>, FlapjackError> {
    let Some(raw_value) = value else {
        return Ok(None);
    };

    let parsed = raw_value
        .parse::<i64>()
        .map_err(|_| FlapjackError::InvalidQuery(DEBUG_EVENTS_TIME_ERROR.to_string()))?;
    if parsed < 0 {
        return Err(FlapjackError::InvalidQuery(
            DEBUG_EVENTS_TIME_ERROR.to_string(),
        ));
    }
    Ok(Some(parsed))
}

/// DELETE /1/usertokens/{userToken} - GDPR deletion for all insight events tied to a user token
///
/// Multi-store cleanup: purges insight events from analytics collector AND
/// deletes the personalization profile cache for the user token. Ordering is
/// deterministic (analytics first, then profile cache) with best-effort
/// semantics — each store is cleaned independently so a failure in one does
/// not block cleanup of the other.
#[utoipa::path(delete, path = "/1/usertokens/{userToken}", tag = "insights",
    params(("userToken" = String, Path, description = "User token to delete")),
    security(("api_key" = [])))]
pub async fn delete_usertoken(
    State(state): State<GdprDeleteState>,
    Path(user_token): Path<String>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    validate_user_token(&user_token).map_err(FlapjackError::InvalidQuery)?;

    // 1. Purge analytics events (in-memory buffer + on-disk Parquet)
    if let Err(e) = state.analytics_collector.purge_user_token(&user_token) {
        tracing::warn!(
            user_token_len = user_token.len(),
            "GDPR delete: failed to purge analytics events: {e}"
        );
    }

    // 2. Delete personalization profile cache
    let profile_store =
        flapjack::personalization::PersonalizationProfileStore::new(&state.profile_store_base_path);
    if let Err(e) = profile_store.delete_profile(&user_token) {
        tracing::warn!(
            user_token_len = user_token.len(),
            "GDPR delete: failed to remove personalization profile: {e}"
        );
    }

    let deleted_at = chrono::Utc::now().to_rfc3339();

    if let Some(notifier) = crate::notifications::global_notifier() {
        notifier.send_gdpr_confirmation(&user_token);
    }

    Ok(Json(serde_json::json!({
        "status": 200,
        "message": "OK",
        "deletedAt": deleted_at
    })))
}

/// State for the GDPR delete endpoint, bundling the analytics collector and
/// the base path needed to construct a PersonalizationProfileStore.
#[derive(Clone)]
pub struct GdprDeleteState {
    pub analytics_collector: Arc<AnalyticsCollector>,
    pub profile_store_base_path: std::path::PathBuf,
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct InsightsRequest {
    pub events: Vec<InsightEvent>,
}

#[cfg(test)]
#[path = "insights_tests.rs"]
mod tests;
