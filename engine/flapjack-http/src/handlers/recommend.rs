//! HTTP handler for the batched recommendations endpoint, dispatching to trending-items, trending-facets, related-products, bought-together, and looking-similar models with validation, rule application, and replica resolution.
use axum::{
    extract::{FromRequestParts, Query, State},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::sync::{Arc, LazyLock};
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
use crate::auth::{
    invalid_api_credentials_flapjack_error, key_allows_index, ApiKey, AuthenticatedAdminKey,
    AuthenticatedAppId, SecuredKeyRestrictions,
};
use crate::idempotency::{IdempotencyRecord, IDEMPOTENCY_HEADER};

const RECOMMEND_IDEMPOTENCY_LOCK_STRIPES: usize = 64;
pub(crate) const TRUSTED_RECOMMEND_CLIENT_IP_HEADER: &str =
    "x-flapjack-trusted-recommend-client-ip";
static RECOMMEND_IDEMPOTENCY_LOCKS: LazyLock<Vec<tokio::sync::Mutex<()>>> = LazyLock::new(|| {
    (0..RECOMMEND_IDEMPOTENCY_LOCK_STRIPES)
        .map(|_| tokio::sync::Mutex::new(()))
        .collect()
});

pub struct RecommendRequestContext {
    api_key: Option<ApiKey>,
    secured_restrictions: Option<SecuredKeyRestrictions>,
    application_id: Option<String>,
    idempotency_key: Option<String>,
    user_ip: Option<String>,
    #[cfg(test)]
    test_hooks: Option<RecommendTestHooks>,
}

#[cfg(test)]
#[derive(Clone, Default)]
struct RecommendTestHooks {
    analytics_collector: Option<Arc<flapjack::analytics::AnalyticsCollector>>,
    yield_after_idempotency_miss: bool,
    before_idempotency_store: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[axum::async_trait]
impl<S> FromRequestParts<S> for RecommendRequestContext
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Reuse the canonical proxy-aware resolver without consuming the body.
        // The final Request extractor retains the existing handler-local body
        // cap and content-type-independent JSON parsing contract.
        let mut request = axum::extract::Request::new(axum::body::Body::empty());
        *request.headers_mut() = parts.headers.clone();
        *request.extensions_mut() = parts.extensions.clone();
        let authenticated_admin = parts.extensions.get::<AuthenticatedAdminKey>().is_some();
        let pbv5_profile = matches!(
            parts.extensions.get::<crate::api_profile::ApiProfile>(),
            Some(crate::api_profile::ApiProfile::PaidBetaV5)
        );
        let user_ip = resolve_recommend_user_ip(&request, authenticated_admin, pbv5_profile)
            .map(|ip| ip.to_string());
        let api_key = parts.extensions.get::<ApiKey>().cloned();
        let secured_restrictions = parts.extensions.get::<SecuredKeyRestrictions>().cloned();
        let authenticated_app_id = parts
            .extensions
            .get::<AuthenticatedAppId>()
            .map(|id| id.0.clone());
        let idempotency_key = parts
            .headers
            .get(IDEMPOTENCY_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let application_id = authenticated_app_id.or_else(|| {
            parts
                .headers
                .get("x-algolia-application-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        });

        Ok(Self {
            api_key,
            secured_restrictions,
            application_id,
            idempotency_key,
            user_ip,
            #[cfg(test)]
            test_hooks: parts.extensions.get::<RecommendTestHooks>().cloned(),
        })
    }
}

fn resolve_recommend_user_ip(
    request: &axum::extract::Request,
    authenticated_admin: bool,
    pbv5_profile: bool,
) -> Option<IpAddr> {
    let internal_values = request
        .headers()
        .get_all(TRUSTED_RECOMMEND_CLIENT_IP_HEADER)
        .iter()
        .collect::<Vec<_>>();
    if authenticated_admin {
        let trusted = match internal_values.as_slice() {
            [value] => value
                .to_str()
                .ok()
                .and_then(|value| value.trim().parse::<IpAddr>().ok()),
            [] | [_, _, ..] => None,
        };
        if trusted.is_some() || pbv5_profile {
            return trusted;
        }
    }

    crate::middleware::extract_client_ip_opt(request)
}

fn recommendation_idempotency_lock(
    application_id: &str,
    scope: &str,
    idempotency_key: &str,
) -> &'static tokio::sync::Mutex<()> {
    let mut hasher = DefaultHasher::new();
    (application_id, scope, idempotency_key).hash(&mut hasher);
    &RECOMMEND_IDEMPOTENCY_LOCKS[(hasher.finish() as usize) % RECOMMEND_IDEMPOTENCY_LOCK_STRIPES]
}

// ── Request DTOs ────────────────────────────────────────────────────────────

/// Batched recommendations request body.
#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct RecommendBatchRequest {
    pub requests: Vec<RecommendRequest>,
}

/// A single recommendation request within a batch.
#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
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
    /// Count this request in Recommend Analytics; defaults to true.
    #[serde(default, deserialize_with = "deserialize_strict_optional_bool")]
    #[schema(default = true, nullable = false)]
    pub analytics: Option<bool>,
    /// Return a queryID for Events attribution; requires analytics and a UUID userToken.
    #[serde(default, deserialize_with = "deserialize_strict_optional_bool")]
    #[schema(default = false, nullable = false)]
    pub click_analytics: Option<bool>,
    /// Optional 1-129 ASCII token; click attribution requires a hyphenated UUID.
    #[serde(default, deserialize_with = "deserialize_strict_optional_string")]
    #[schema(
        min_length = 1,
        max_length = 129,
        pattern = r"^[A-Za-z0-9_-]+$",
        nullable = false
    )]
    pub user_token: Option<String>,
}

fn deserialize_strict_optional_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Bool(value) => Ok(Some(value)),
        serde_json::Value::Null => Err(D::Error::custom("expected boolean, found null")),
        _ => Err(D::Error::custom("expected boolean")),
    }
}

fn deserialize_strict_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(value) => Ok(Some(value)),
        serde_json::Value::Null => Err(D::Error::custom("expected string, found null")),
        _ => Err(D::Error::custom("expected string")),
    }
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
    #[serde(rename = "queryID", skip_serializing_if = "Option::is_none")]
    pub query_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecommendAnalyticsParams {
    pub index: String,
    pub model: String,
    pub start_date: String,
    pub end_date: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecommendAnalyticsResponse {
    pub index: String,
    pub model: String,
    pub start_date: String,
    pub end_date: String,
    pub total_users: u64,
    pub total_recommendations: u64,
    pub tracked_recommendations: u64,
    pub clicked_recommendations: u64,
    pub converted_recommendations: u64,
    pub click_through_rate: f64,
    pub conversion_rate: f64,
    pub click_position_distribution: Vec<ClickPositionCount>,
    pub average_click_position: Option<f64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClickPositionCount {
    pub position: u32,
    pub count: u64,
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

    if let Some(user_token) = req.user_token.as_deref() {
        flapjack::analytics::schema::validate_user_token(user_token)
            .map_err(FlapjackError::InvalidQuery)?;
    }

    if req.click_analytics == Some(true) && req.analytics == Some(false) {
        return Err(FlapjackError::InvalidQuery(
            "analytics=false cannot be combined with clickAnalytics=true".to_string(),
        ));
    }
    if req.click_analytics == Some(true) && req.user_token.is_none() {
        return Err(FlapjackError::InvalidQuery(
            "clickAnalytics=true requires a present valid userToken".to_string(),
        ));
    }
    if req.click_analytics == Some(true) {
        flapjack::analytics::schema::validate_attributed_user_token(
            req.user_token
                .as_deref()
                .expect("presence checked immediately above"),
        )
        .map_err(FlapjackError::InvalidQuery)?;
    }

    Ok(())
}

fn recommendation_idempotency_scope(body: &RecommendBatchRequest) -> String {
    let canonical = serde_json::to_vec(body).expect("recommend request must serialize");
    format!(
        "/recommendations/{}",
        hex::encode(Sha256::digest(canonical))
    )
}

fn recommendation_analytics_bounds(
    start_date: &str,
    end_date: &str,
) -> Result<(i64, i64), FlapjackError> {
    let start = NaiveDate::parse_from_str(start_date, "%Y-%m-%d")
        .map_err(|_| FlapjackError::InvalidQuery("startDate must be YYYY-MM-DD".to_string()))?;
    let end = NaiveDate::parse_from_str(end_date, "%Y-%m-%d")
        .map_err(|_| FlapjackError::InvalidQuery("endDate must be YYYY-MM-DD".to_string()))?;
    if end < start {
        return Err(FlapjackError::InvalidQuery(
            "endDate must not be before startDate".to_string(),
        ));
    }
    let inclusive_days = end.signed_duration_since(start).num_days() + 1;
    if inclusive_days > 30 {
        return Err(FlapjackError::InvalidQuery(
            "recommendation analytics date range must not exceed 30 days".to_string(),
        ));
    }
    let start_ms = Utc
        .from_utc_datetime(&start.and_hms_opt(0, 0, 0).expect("midnight is valid"))
        .timestamp_millis();
    let end_exclusive = end.checked_add_signed(Duration::days(1)).ok_or_else(|| {
        FlapjackError::InvalidQuery("endDate is outside the supported range".to_string())
    })?;
    let end_exclusive_ms = Utc
        .from_utc_datetime(
            &end_exclusive
                .and_hms_opt(0, 0, 0)
                .expect("midnight is valid"),
        )
        .timestamp_millis();
    Ok((start_ms, end_exclusive_ms))
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
    context: RecommendRequestContext,
    request: axum::extract::Request,
) -> Result<Response, FlapjackError> {
    let body_bytes = axum::body::to_bytes(request.into_body(), 10_000_000)
        .await
        .map_err(|error| {
            FlapjackError::InvalidQuery(format!("failed to read recommendations request: {error}"))
        })?;
    let body: RecommendBatchRequest = serde_json::from_slice(&body_bytes).map_err(|error| {
        FlapjackError::InvalidQuery(format!("invalid recommendations request: {error}"))
    })?;
    let RecommendRequestContext {
        api_key,
        secured_restrictions,
        application_id,
        idempotency_key,
        user_ip,
        #[cfg(test)]
        test_hooks,
    } = context;

    let api_key_ref = api_key.as_ref();
    let secured_restrictions_ref = secured_restrictions.as_ref();
    // The wildcard route has no concrete path index for middleware to authorize.
    // Validate and authorize the complete batch before resolving any targets so a
    // later forbidden request cannot cause earlier index or analytics work.
    for req in &body.requests {
        validate_request(req)?;
        if api_key_ref
            .is_some_and(|key| !key_allows_index(key, secured_restrictions_ref, &req.index_name))
        {
            return Err(invalid_api_credentials_flapjack_error());
        }
    }

    let mut prepared = Vec::with_capacity(body.requests.len());
    for req in &body.requests {
        let target_index = resolve_recommend_data_index(&state, &req.index_name);
        // Event storage can outlive an index. Resolve through the live index owner
        // before querying analytics so deleted targets cannot return stale hits.
        state.manager.get_or_load(&target_index)?;
        prepared.push((req, target_index));
    }

    let idempotency_scope = recommendation_idempotency_scope(&body);
    let idempotency_admission = if let Some(key) = idempotency_key.as_deref() {
        let app_id = application_id.as_deref().ok_or_else(|| {
            FlapjackError::InvalidQuery(
                "X-Algolia-Application-Id is required with an idempotency key".to_string(),
            )
        })?;
        // Serialize the complete lookup -> execution -> durable response store ->
        // telemetry publication sequence. A bounded stripe set avoids an
        // attacker-controlled per-key lock registry while preserving exact replay.
        let guard = recommendation_idempotency_lock(app_id, &idempotency_scope, key)
            .lock()
            .await;
        match state
            .idempotency_cache
            .lookup_scoped(app_id, &idempotency_scope, key)
        {
            Ok(Some(record)) => return Ok(record.into_response()),
            Ok(None) => {}
            Err(error) => {
                tracing::error!(error = %error, "recommend idempotency cache lookup failed");
                return Err(FlapjackError::Io(
                    "idempotency persistence lookup failed".to_string(),
                ));
            }
        }
        #[cfg(test)]
        if test_hooks
            .as_ref()
            .is_some_and(|hooks| hooks.yield_after_idempotency_miss)
        {
            tokio::task::yield_now().await;
        }
        Some((guard, app_id.to_string(), key.to_string()))
    } else {
        None
    };

    let mut results = Vec::with_capacity(body.requests.len());
    let mut telemetry = Vec::with_capacity(body.requests.len());

    for (req, target_index) in prepared {
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
        let query_id = req
            .click_analytics
            .is_some_and(|enabled| enabled)
            .then(|| hex::encode(uuid::Uuid::new_v4().as_bytes()));
        results.push(RecommendResult {
            hits,
            processing_time_ms: elapsed,
            query_id: query_id.clone(),
        });
        if req.analytics != Some(false) {
            telemetry.push((
                req.index_name.clone(),
                req.model.as_str(),
                req.user_token.as_deref(),
                query_id,
            ));
        }
    }

    let response_body = RecommendBatchResponse { results };
    if let Ok(_mutation_permit) = state.global_mutation_fence.admit_mutation().await {
        if let Some((_guard, app_id, key)) = &idempotency_admission {
            #[cfg(test)]
            if let Some(hook) = test_hooks
                .as_ref()
                .and_then(|hooks| hooks.before_idempotency_store.as_ref())
            {
                hook();
            }
            let response_bytes = serde_json::to_vec(&response_body).map_err(|error| {
                FlapjackError::Io(format!(
                    "failed to serialize idempotent recommendation response: {error}"
                ))
            })?;
            state
                .idempotency_cache
                .store_scoped(
                    app_id,
                    &idempotency_scope,
                    key,
                    IdempotencyRecord::json(StatusCode::OK, response_bytes.into()),
                )
                .map_err(|error| {
                    tracing::error!(error = %error, "recommend idempotency cache store failed");
                    FlapjackError::Io("idempotency persistence store failed".to_string())
                })?;
        }

        // Every request has completed and any idempotent response is durable before
        // telemetry becomes visible. Store failure therefore returns no queryID and
        // creates no count that a retry could duplicate.
        #[cfg(test)]
        let collector = test_hooks
            .as_ref()
            .and_then(|hooks| hooks.analytics_collector.as_ref())
            .or_else(|| flapjack::analytics::get_global_collector());
        #[cfg(not(test))]
        let collector = flapjack::analytics::get_global_collector();
        if let Some(collector) = collector {
            let timestamp_ms = Utc::now().timestamp_millis();
            for (requested_index, model, user_token, query_id) in &telemetry {
                collector.record_recommendation_request(
                    requested_index,
                    model,
                    *user_token,
                    user_ip.as_deref(),
                    query_id.clone(),
                    timestamp_ms,
                );
            }
        }
    } else {
        tracing::debug!("release mutation fence active; optional Recommend persistence suppressed");
    }

    Ok(Json(response_body).into_response())
}

/// GET /internal/recommendations/analytics
#[utoipa::path(
    get,
    path = "/internal/recommendations/analytics",
    tag = "recommendations",
    params(
        ("index" = String, Query, description = "Exact physical index name"),
        ("model" = String, Query, description = "Recommend model"),
        ("startDate" = String, Query, description = "Inclusive UTC start date"),
        ("endDate" = String, Query, description = "Inclusive UTC end date")
    ),
    responses(
        (status = 200, description = "Bounded Recommend analytics", body = RecommendAnalyticsResponse),
        (status = 400, description = "Invalid model, index, or date range"),
        (status = 503, description = "Analytics unavailable")
    ),
    security(("api_key" = []))
)]
pub async fn recommendation_analytics(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RecommendAnalyticsParams>,
) -> Result<Response, FlapjackError> {
    validate_index_name(&params.index)?;
    if !VALID_MODELS.contains(&params.model.as_str()) {
        return Err(FlapjackError::InvalidQuery(format!(
            "unsupported recommendation model: {}",
            params.model
        )));
    }
    let (start_ms, end_exclusive_ms) =
        recommendation_analytics_bounds(&params.start_date, &params.end_date)?;
    let Some(engine) = state.analytics_engine.as_ref() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "message": "recommendation analytics unavailable",
                "status": 503
            })),
        )
            .into_response());
    };
    let summary = match engine
        .recommendation_analytics(&params.index, &params.model, start_ms, end_exclusive_ms)
        .await
    {
        Ok(summary) => summary,
        Err(error) => {
            tracing::warn!(
                index = %params.index,
                model = %params.model,
                error = %error,
                "recommendation analytics query unavailable"
            );
            return Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "message": "recommendation analytics unavailable",
                    "status": 503
                })),
            )
                .into_response());
        }
    };

    Ok(Json(RecommendAnalyticsResponse {
        index: params.index,
        model: params.model,
        start_date: params.start_date,
        end_date: params.end_date,
        total_users: summary.total_users,
        total_recommendations: summary.total_recommendations,
        tracked_recommendations: summary.tracked_recommendations,
        clicked_recommendations: summary.clicked_recommendations,
        converted_recommendations: summary.converted_recommendations,
        click_through_rate: summary.click_through_rate,
        conversion_rate: summary.conversion_rate,
        click_position_distribution: summary
            .click_position_distribution
            .into_iter()
            .map(|(position, count)| ClickPositionCount { position, count })
            .collect(),
        average_click_position: summary.average_click_position,
    })
    .into_response())
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
