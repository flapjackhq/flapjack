//! Read (GET) analytics endpoint handlers — top searches, rates, clicks, conversions, users, overview.
use axum::{
    extract::{Query, RawQuery, State},
    http::HeaderMap,
    Extension, Json,
};
use std::sync::Arc;

use flapjack::analytics::{AnalyticsQueryEngine, AnalyticsQueryParams};
use flapjack::error::FlapjackError;

use super::{
    analytics_request_has_index_restrictions, clamp_limit, enforce_analytics_index_access,
    maybe_fan_out, validate_analytics_index, AnalyticsParams, OverviewParams,
};
use crate::auth::{ApiKey, SecuredKeyRestrictions};
use crate::handlers::analytics_dto::*;

/// GET /2/searches - Top searches ranked by frequency
#[utoipa::path(
    get,
    path = "/2/searches",
    tag = "analytics",
    responses((status = 200, description = "Top searches", body = AnalyticsTopSearchesResponse)),
    security(("api_key" = []))
)]
pub async fn get_top_searches(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let limit = clamp_limit(params.limit.unwrap_or(10));
    let click_analytics = params.click_analytics.unwrap_or(false);
    let result = engine
        .top_searches(
            &AnalyticsQueryParams {
                index_name: &params.index,
                start_date: &params.start_date,
                end_date: &params.end_date,
                limit,
                tags: params.tags.as_deref(),
            },
            click_analytics,
            params.country.as_deref(),
        )
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "searches",
        "/2/searches",
        &raw_query.unwrap_or_default(),
        result,
        limit,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/searches/count - Total search count with daily breakdown
#[utoipa::path(
    get,
    path = "/2/searches/count",
    tag = "analytics",
    responses((status = 200, description = "Search count and daily breakdown", body = AnalyticsCountWithDatesResponse)),
    security(("api_key" = []))
)]
pub async fn get_search_count(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let result = engine
        .search_count(&params.index, &params.start_date, &params.end_date)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "searches/count",
        "/2/searches/count",
        &raw_query.unwrap_or_default(),
        result,
        1000,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/searches/noResults - Top queries with 0 results
#[utoipa::path(
    get,
    path = "/2/searches/noResults",
    tag = "analytics",
    responses((status = 200, description = "Top searches with no results", body = AnalyticsTopSearchesResponse)),
    security(("api_key" = []))
)]
pub async fn get_no_results(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let limit = clamp_limit(params.limit.unwrap_or(1000));
    let result = engine
        .no_results_searches(&params.index, &params.start_date, &params.end_date, limit)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "searches/noResults",
        "/2/searches/noResults",
        &raw_query.unwrap_or_default(),
        result,
        limit,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/searches/noResultRate - No-results rate with daily breakdown
#[utoipa::path(
    get,
    path = "/2/searches/noResultRate",
    tag = "analytics",
    responses((status = 200, description = "No-results rate and daily breakdown", body = AnalyticsRateWithDatesResponse)),
    security(("api_key" = []))
)]
pub async fn get_no_result_rate(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let result = engine
        .no_results_rate(&params.index, &params.start_date, &params.end_date)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "searches/noResultRate",
        "/2/searches/noResultRate",
        &raw_query.unwrap_or_default(),
        result,
        1000,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/searches/noClicks - Top searches with 0 clicks
#[utoipa::path(
    get,
    path = "/2/searches/noClicks",
    tag = "analytics",
    responses((status = 200, description = "Top searches with no clicks", body = AnalyticsTopSearchesResponse)),
    security(("api_key" = []))
)]
pub async fn get_no_clicks(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let limit = clamp_limit(params.limit.unwrap_or(1000));
    let result = engine
        .no_click_searches(&params.index, &params.start_date, &params.end_date, limit)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "searches/noClicks",
        "/2/searches/noClicks",
        &raw_query.unwrap_or_default(),
        result,
        limit,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/searches/noClickRate - No-click rate with daily breakdown
#[utoipa::path(
    get,
    path = "/2/searches/noClickRate",
    tag = "analytics",
    responses((status = 200, description = "No-click rate and daily breakdown", body = AnalyticsRateWithDatesResponse)),
    security(("api_key" = []))
)]
pub async fn get_no_click_rate(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let result = engine
        .no_click_rate(&params.index, &params.start_date, &params.end_date)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "searches/noClickRate",
        "/2/searches/noClickRate",
        &raw_query.unwrap_or_default(),
        result,
        1000,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/clicks/clickThroughRate - CTR with daily breakdown
#[utoipa::path(
    get,
    path = "/2/clicks/clickThroughRate",
    tag = "analytics",
    responses((status = 200, description = "Click-through rate and daily breakdown", body = AnalyticsRateWithDatesResponse)),
    security(("api_key" = []))
)]
pub async fn get_click_through_rate(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let result = engine
        .click_through_rate(&params.index, &params.start_date, &params.end_date)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "clicks/clickThroughRate",
        "/2/clicks/clickThroughRate",
        &raw_query.unwrap_or_default(),
        result,
        1000,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/clicks/averageClickPosition - Average click position
#[utoipa::path(
    get,
    path = "/2/clicks/averageClickPosition",
    tag = "analytics",
    responses((status = 200, description = "Average click position and daily breakdown", body = AnalyticsAverageClickPositionResponse)),
    security(("api_key" = []))
)]
pub async fn get_average_click_position(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let result = engine
        .average_click_position(&params.index, &params.start_date, &params.end_date)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "clicks/averageClickPosition",
        "/2/clicks/averageClickPosition",
        &raw_query.unwrap_or_default(),
        result,
        1000,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/clicks/positions - Click position distribution (Algolia-style buckets)
#[utoipa::path(
    get,
    path = "/2/clicks/positions",
    tag = "analytics",
    responses((status = 200, description = "Click position distribution", body = AnalyticsClickPositionsResponse)),
    security(("api_key" = []))
)]
pub async fn get_click_positions(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let result = engine
        .click_positions(&params.index, &params.start_date, &params.end_date)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "clicks/positions",
        "/2/clicks/positions",
        &raw_query.unwrap_or_default(),
        result,
        1000,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/conversions/addToCartRate - Add-to-cart conversion rate
#[utoipa::path(
    get,
    path = "/2/conversions/addToCartRate",
    tag = "analytics",
    responses((status = 200, description = "Add-to-cart conversion rate", body = AnalyticsRateWithDatesResponse)),
    security(("api_key" = []))
)]
pub async fn get_add_to_cart_rate(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let result = engine
        .conversion_rate_for_subtype(
            &params.index,
            &params.start_date,
            &params.end_date,
            "addToCart",
        )
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "conversions/addToCartRate",
        "/2/conversions/addToCartRate",
        &raw_query.unwrap_or_default(),
        result,
        1000,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/conversions/purchaseRate - Purchase conversion rate
#[utoipa::path(
    get,
    path = "/2/conversions/purchaseRate",
    tag = "analytics",
    responses((status = 200, description = "Purchase conversion rate", body = AnalyticsRateWithDatesResponse)),
    security(("api_key" = []))
)]
pub async fn get_purchase_rate(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let result = engine
        .conversion_rate_for_subtype(
            &params.index,
            &params.start_date,
            &params.end_date,
            "purchase",
        )
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "conversions/purchaseRate",
        "/2/conversions/purchaseRate",
        &raw_query.unwrap_or_default(),
        result,
        1000,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/conversions/conversionRate - Conversion rate
#[utoipa::path(
    get,
    path = "/2/conversions/conversionRate",
    tag = "analytics",
    responses((status = 200, description = "Conversion rate", body = AnalyticsRateWithDatesResponse)),
    security(("api_key" = []))
)]
pub async fn get_conversion_rate(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let result = engine
        .conversion_rate(&params.index, &params.start_date, &params.end_date)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "conversions/conversionRate",
        "/2/conversions/conversionRate",
        &raw_query.unwrap_or_default(),
        result,
        1000,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/hits - Top clicked objectIDs
#[utoipa::path(
    get,
    path = "/2/hits",
    tag = "analytics",
    responses((status = 200, description = "Top clicked objectIDs", body = AnalyticsTopHitsResponse)),
    security(("api_key" = []))
)]
pub async fn get_top_hits(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let limit = clamp_limit(params.limit.unwrap_or(1000));
    let result = engine
        .top_hits(&params.index, &params.start_date, &params.end_date, limit)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "hits",
        "/2/hits",
        &raw_query.unwrap_or_default(),
        result,
        limit,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/filters - Top filter attributes
#[utoipa::path(
    get,
    path = "/2/filters",
    tag = "analytics",
    responses((status = 200, description = "Top filters", body = AnalyticsFiltersResponse)),
    security(("api_key" = []))
)]
pub async fn get_top_filters(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let limit = clamp_limit(params.limit.unwrap_or(1000));
    let result = engine
        .top_filters(&params.index, &params.start_date, &params.end_date, limit)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "filters",
        "/2/filters",
        &raw_query.unwrap_or_default(),
        result,
        limit,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/filters/:attribute - Top values for a filter attribute
#[utoipa::path(get, path = "/2/filters/{attribute}", tag = "analytics",
    params(("attribute" = String, Path, description = "Filter attribute name")),
    responses((status = 200, description = "Top values for a filter attribute", body = AnalyticsFilterValuesResponse)),
    security(("api_key" = [])))]
pub async fn get_filter_values(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    axum::extract::Path(attribute): axum::extract::Path<String>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let limit = clamp_limit(params.limit.unwrap_or(1000));
    let result = engine
        .filter_values(
            &params.index,
            &attribute,
            &params.start_date,
            &params.end_date,
            limit,
        )
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let endpoint = format!("filters/{}", attribute);
    let path = format!("/2/filters/{}", attribute);
    let result = maybe_fan_out(
        &headers,
        &endpoint,
        &path,
        &raw_query.unwrap_or_default(),
        result,
        limit,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/filters/noResults - Filters causing no results
#[utoipa::path(
    get,
    path = "/2/filters/noResults",
    tag = "analytics",
    responses((status = 200, description = "Filters causing no results", body = AnalyticsFiltersResponse)),
    security(("api_key" = []))
)]
pub async fn get_filters_no_results(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let limit = clamp_limit(params.limit.unwrap_or(1000));
    let result = engine
        .filters_no_results(&params.index, &params.start_date, &params.end_date, limit)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "filters/noResults",
        "/2/filters/noResults",
        &raw_query.unwrap_or_default(),
        result,
        limit,
    )
    .await;
    Ok(Json(result))
}

/// GET /2/users/count - Unique user count
#[utoipa::path(
    get,
    path = "/2/users/count",
    tag = "analytics",
    responses((status = 200, description = "Unique user count and daily breakdown", body = AnalyticsCountWithDatesResponse)),
    security(("api_key" = []))
)]
pub async fn get_users_count(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<AnalyticsParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    validate_analytics_index(&params.index)?;
    enforce_analytics_index_access(api_key, secured_restrictions, &params.index)?;
    let result = engine
        .users_count_hll(&params.index, &params.start_date, &params.end_date)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let mut result = maybe_fan_out(
        &headers,
        "users/count",
        "/2/users/count",
        &raw_query.unwrap_or_default(),
        result,
        1000,
    )
    .await;
    // Strip internal HLL fields from PUBLIC API responses only.
    //
    // When X-Flapjack-Local-Only is set, this node is responding to a cluster
    // coordinator query. The coordinator needs hll_sketch and daily_sketches to
    // merge sketches across nodes via merge_user_counts(). Do NOT strip them here.
    //
    // When serving a public client (no local-only header), strip them:
    //   - single-node: users_count_hll() returns them but clients must not see them
    //   - cluster mode: merge_user_counts() already stripped them (this is a no-op)
    if headers.get("X-Flapjack-Local-Only").is_none() {
        if let Some(obj) = result.as_object_mut() {
            obj.remove("hll_sketch");
            obj.remove("daily_sketches");
        }
    }
    Ok(Json(result))
}

/// GET /2/overview - Server-wide analytics overview across all indices
#[utoipa::path(
    get,
    path = "/2/overview",
    tag = "analytics",
    responses((status = 200, description = "Global analytics overview", body = AnalyticsOverviewResponse)),
    security(("api_key" = []))
)]
pub async fn get_overview(
    headers: HeaderMap,
    State(engine): State<Arc<AnalyticsQueryEngine>>,
    api_key: Option<Extension<ApiKey>>,
    secured_restrictions: Option<Extension<SecuredKeyRestrictions>>,
    RawQuery(raw_query): RawQuery,
    Query(params): Query<OverviewParams>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let api_key = api_key.as_ref().map(|Extension(api_key)| api_key);
    let secured_restrictions = secured_restrictions
        .as_ref()
        .map(|Extension(restrictions)| restrictions);
    let result = match params.index.as_deref() {
        Some(index) => {
            validate_analytics_index(index)?;
            enforce_analytics_index_access(api_key, secured_restrictions, index)?;
            engine
                .overview_for_index(index, &params.start_date, &params.end_date)
                .await
        }
        None => {
            if analytics_request_has_index_restrictions(api_key, secured_restrictions) {
                return Err(crate::auth::invalid_api_credentials_flapjack_error());
            }
            engine.overview(&params.start_date, &params.end_date).await
        }
    }
    .map_err(|e| FlapjackError::InvalidQuery(format!("Analytics error: {}", e)))?;
    let result = maybe_fan_out(
        &headers,
        "overview",
        "/2/overview",
        &raw_query.unwrap_or_default(),
        result,
        1000,
    )
    .await;
    Ok(Json(result))
}
