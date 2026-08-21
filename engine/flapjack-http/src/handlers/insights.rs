//! Algolia Insights API-compatible event ingestion, debug event inspection, and GDPR user token deletion handlers.
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    Extension, Json,
};
use std::sync::Arc;

use flapjack::analytics::schema::{validate_user_token, InsightEvent};
use flapjack::analytics::{AnalyticsCollector, DebugEvent};
use flapjack::error::FlapjackError;

use super::analytics::{analytics_request_has_index_restrictions, enforce_analytics_index_access};
use crate::auth::{key_allows_index, ApiKey, AuthenticatedAppId, SecuredKeyRestrictions};
use crate::idempotency::{
    event_index_set_segment, IdempotencyCache, IdempotencyRecord, IDEMPOTENCY_HEADER,
};

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
    authenticated_app_id: Option<Extension<AuthenticatedAppId>>,
    idempotency_cache: Option<Extension<Arc<IdempotencyCache>>>,
    headers: HeaderMap,
    Json(body): Json<InsightsRequest>,
) -> Result<Response, FlapjackError> {
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

    // Validate the complete request before publishing debugger, persistence, or
    // analytics effects. Official Insights sends batches as one request; a
    // single malformed member therefore rejects the complete batch atomically.
    let validation_errors: Vec<String> = body
        .events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            event
                .validate()
                .err()
                .map(|error| format!("event {index}: {error}"))
        })
        .collect();
    if !validation_errors.is_empty() {
        return Err(FlapjackError::InvalidQuery(format!(
            "Event batch rejected: {}",
            validation_errors.join("; ")
        )));
    }

    let idempotency_key = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|value| value.to_str().ok());
    let app_id = authenticated_app_id
        .as_ref()
        .map(|Extension(app_id)| app_id.0.as_str())
        .or_else(|| {
            headers
                .get("x-algolia-application-id")
                .and_then(|value| value.to_str().ok())
        });
    let idempotency_scope = event_idempotency_scope(&body.events);
    if let Some(key) = idempotency_key {
        let app_id = app_id.ok_or_else(|| {
            FlapjackError::InvalidQuery(
                "X-Algolia-Application-Id is required with an idempotency key".to_string(),
            )
        })?;
        let cache = idempotency_cache
            .as_ref()
            .ok_or_else(|| FlapjackError::Io("idempotency cache is unavailable".to_string()))?;
        match cache.lookup_scoped(app_id, &idempotency_scope, key) {
            Ok(Some(record)) => return Ok(record.into_response()),
            Ok(None) => {}
            Err(err) => {
                tracing::error!(error = %err, "event idempotency cache lookup failed");
                return Err(FlapjackError::Io(
                    "idempotency persistence lookup failed".to_string(),
                ));
            }
        }
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
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

        collector.record_debug_event(debug_entry(200, vec![]));
        collector.record_insight(event);
    }

    let response_body = serde_json::json!({
        "status": 200,
        "message": "OK"
    });

    if let (Some(key), Some(app_id), Some(Extension(cache))) =
        (idempotency_key, app_id, idempotency_cache)
    {
        let response_bytes = serde_json::to_vec(&response_body).unwrap_or_default();
        if let Err(err) = cache.store_scoped(
            app_id,
            &idempotency_scope,
            key,
            IdempotencyRecord::json(axum::http::StatusCode::OK, response_bytes.into()),
        ) {
            tracing::error!(
                error = %err,
                app_id,
                idempotency_scope,
                "event idempotency cache store failed after accepted events; returning success"
            );
        }
    }

    Ok(Json(response_body).into_response())
}

fn event_idempotency_scope(events: &[InsightEvent]) -> String {
    event_index_set_segment(events.iter().map(|event| event.index.as_str()))
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

/// DELETE /1/usertokens/{userToken} - Delete insight events tied to a user token.
///
/// Admin and unrestricted keys retain the full GDPR cleanup across analytics
/// and the global personalization profile. Index-restricted keys purge only
/// authorized analytics partitions because the profile has no index scope.
#[utoipa::path(delete, path = "/1/usertokens/{userToken}", tag = "insights",
    params(("userToken" = String, Path, description = "User token to delete")),
    security(("api_key" = [])))]
pub async fn delete_usertoken(
    State(state): State<GdprDeleteState>,
    Path(user_token): Path<String>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    validate_user_token(&user_token).map_err(FlapjackError::InvalidQuery)?;

    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    let index_restricted = analytics_request_has_index_restrictions(api_key, secured_restrictions);

    // A full deletion must prove the profile target is safe before analytics
    // mutation starts. Restricted deletion has no global profile scope.
    let (profile_store, _profile_operation) = if index_restricted {
        (None, None)
    } else {
        let store = flapjack::personalization::PersonalizationProfileStore::new(
            &state.profile_store_base_path,
        );
        let operation = store.begin_user_operation(&user_token).await.map_err(|e| {
            tracing::warn!(
                user_token_len = user_token.len(),
                "GDPR delete: failed to order personalization profile deletion: {e}"
            );
            FlapjackError::Io("failed to order user profile deletion".to_string())
        })?;
        store.preflight_delete_profile(&user_token).map_err(|e| {
            tracing::warn!(
                user_token_len = user_token.len(),
                "GDPR delete: unsafe personalization profile target: {e}"
            );
            FlapjackError::Io("failed to validate user profile deletion".to_string())
        })?;
        (Some(store), Some(operation))
    };

    // Restricted keys can purge only their authorized event partitions. The
    // unscoped profile and notification remain reserved for a full deletion.
    let purge_result = if index_restricted {
        state
            .analytics_collector
            .purge_user_token_where_index(&user_token, &|index| {
                api_key.is_some_and(|key| key_allows_index(key, secured_restrictions, index))
            })
    } else {
        state.analytics_collector.purge_user_token(&user_token)
    };
    purge_result.map_err(|e| {
        tracing::warn!(
            user_token_len = user_token.len(),
            "user-token deletion: failed to purge analytics events: {e}"
        );
        FlapjackError::Io("failed to purge user analytics".to_string())
    })?;

    if let Some(profile_store) = profile_store {
        // Delete the global personalization profile only for a full deletion.
        profile_store.delete_profile(&user_token).map_err(|e| {
            tracing::warn!(
                user_token_len = user_token.len(),
                "GDPR delete: failed to remove personalization profile: {e}"
            );
            FlapjackError::Io("failed to delete user profile".to_string())
        })?;

        if let Some(notifier) = &state.gdpr_notifier {
            notifier.send_gdpr_confirmation(&user_token);
        }
    }

    let deleted_at = chrono::Utc::now().to_rfc3339();

    let mut response = serde_json::json!({
        "status": 200,
        "message": "OK",
        "deletedAt": deleted_at
    });
    if index_restricted {
        response["deletionScope"] = serde_json::json!("authorizedIndexes");
    }

    Ok(Json(response))
}

/// DELETE /1/indexes/{indexName}/usertokens/{userToken} - Delete insight
/// events tied to a user token from exactly one index.
///
/// This operational endpoint deliberately leaves the VM-global
/// personalization profile and GDPR notification untouched. It is used by a
/// tenant-aware control plane after it has resolved the tenant's physical
/// index name.
#[utoipa::path(delete, path = "/1/indexes/{indexName}/usertokens/{userToken}", tag = "insights",
    params(
        ("indexName" = String, Path, description = "Exact physical index name"),
        ("userToken" = String, Path, description = "User token to delete")
    ),
    security(("api_key" = [])))]
pub async fn delete_index_usertoken(
    State(state): State<GdprDeleteState>,
    Path((index_name, user_token)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    flapjack::validate_index_name(&index_name)?;
    validate_user_token(&user_token).map_err(FlapjackError::InvalidQuery)?;

    state
        .analytics_collector
        .purge_user_token_where_index(&user_token, &|index| index == index_name)
        .map_err(|error| {
            tracing::warn!(
                index_name,
                user_token_len = user_token.len(),
                "index user-token deletion: failed to purge analytics events: {error}"
            );
            FlapjackError::Io("failed to purge user analytics".to_string())
        })?;

    Ok(Json(serde_json::json!({
        "status": 200,
        "message": "OK",
        "deletedAt": chrono::Utc::now().to_rfc3339(),
        "deletionScope": "exactIndex"
    })))
}

/// State for the GDPR delete endpoint, bundling the analytics collector and
/// the base path needed to construct a PersonalizationProfileStore.
#[derive(Clone)]
pub struct GdprDeleteState {
    pub analytics_collector: Arc<AnalyticsCollector>,
    pub profile_store_base_path: std::path::PathBuf,
    /// Canonical notifier supplied by `AppState` through the production router.
    pub gdpr_notifier: Option<Arc<crate::notifications::NotificationService>>,
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct InsightsRequest {
    pub events: Vec<InsightEvent>,
}

#[cfg(test)]
#[path = "insights_tests.rs"]
mod tests;
