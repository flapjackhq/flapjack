use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsDateCount {
    pub date: String,
    pub count: i64,
}

/// Analytics response entry for a top search query with its count.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsTopSearchEntry {
    pub search: String,
    pub count: i64,
    #[serde(rename = "nbHits", skip_serializing_if = "Option::is_none")]
    pub nb_hits: Option<i64>,
    #[serde(rename = "trackedSearchCount", skip_serializing_if = "Option::is_none")]
    pub tracked_search_count: Option<i64>,
    #[serde(rename = "clickCount", skip_serializing_if = "Option::is_none")]
    pub click_count: Option<i64>,
    #[serde(rename = "clickThroughRate", skip_serializing_if = "Option::is_none")]
    pub click_through_rate: Option<f64>,
    #[serde(rename = "conversionRate", skip_serializing_if = "Option::is_none")]
    pub conversion_rate: Option<f64>,
    #[serde(rename = "conversionCount", skip_serializing_if = "Option::is_none")]
    pub conversion_count: Option<i64>,
    #[serde(
        rename = "averageClickPosition",
        skip_serializing_if = "Option::is_none"
    )]
    pub average_click_position: Option<f64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsTopSearchesResponse {
    pub searches: Vec<AnalyticsTopSearchEntry>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsTopHitsResponse {
    pub hits: Vec<AnalyticsHitEntry>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsHitEntry {
    pub hit: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsFiltersResponse {
    pub filters: Vec<AnalyticsFilterEntry>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsFilterEntry {
    pub attribute: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsFilterValuesResponse {
    pub attribute: String,
    pub values: Vec<AnalyticsValueCount>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsValueCount {
    pub value: String,
    pub count: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsCountWithDatesResponse {
    pub count: i64,
    pub dates: Vec<AnalyticsDateCount>,
}

/// Analytics time-series entry pairing a date with a rate value.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsRateDateEntry {
    pub date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    #[serde(rename = "noResults", skip_serializing_if = "Option::is_none")]
    pub no_results: Option<i64>,
    #[serde(rename = "noClicks", skip_serializing_if = "Option::is_none")]
    pub no_clicks: Option<i64>,
    #[serde(rename = "clickCount", skip_serializing_if = "Option::is_none")]
    pub click_count: Option<i64>,
    #[serde(rename = "trackedSearchCount", skip_serializing_if = "Option::is_none")]
    pub tracked_search_count: Option<i64>,
    #[serde(rename = "conversionCount", skip_serializing_if = "Option::is_none")]
    pub conversion_count: Option<i64>,
    #[serde(rename = "addToCartCount", skip_serializing_if = "Option::is_none")]
    pub add_to_cart_count: Option<i64>,
    #[serde(rename = "purchaseCount", skip_serializing_if = "Option::is_none")]
    pub purchase_count: Option<i64>,
}

/// Analytics response containing an aggregate rate and per-date breakdown.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsRateWithDatesResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    #[serde(rename = "noResults", skip_serializing_if = "Option::is_none")]
    pub no_results: Option<i64>,
    #[serde(rename = "noClicks", skip_serializing_if = "Option::is_none")]
    pub no_clicks: Option<i64>,
    #[serde(rename = "clickCount", skip_serializing_if = "Option::is_none")]
    pub click_count: Option<i64>,
    #[serde(rename = "trackedSearchCount", skip_serializing_if = "Option::is_none")]
    pub tracked_search_count: Option<i64>,
    #[serde(rename = "conversionCount", skip_serializing_if = "Option::is_none")]
    pub conversion_count: Option<i64>,
    #[serde(rename = "addToCartCount", skip_serializing_if = "Option::is_none")]
    pub add_to_cart_count: Option<i64>,
    #[serde(rename = "purchaseCount", skip_serializing_if = "Option::is_none")]
    pub purchase_count: Option<i64>,
    pub dates: Vec<AnalyticsRateDateEntry>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsAverageClickPositionDate {
    pub date: String,
    pub average: f64,
    #[serde(rename = "clickCount")]
    pub click_count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsAverageClickPositionResponse {
    pub average: f64,
    #[serde(rename = "clickCount")]
    pub click_count: i64,
    pub dates: Vec<AnalyticsAverageClickPositionDate>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsClickPositionBucket {
    pub position: (i32, i32),
    #[serde(rename = "clickCount")]
    pub click_count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsClickPositionsResponse {
    pub positions: Vec<AnalyticsClickPositionBucket>,
    #[serde(rename = "clickCount")]
    pub click_count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsCountryCount {
    pub country: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsCountriesResponse {
    pub countries: Vec<AnalyticsCountryCount>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsGeoBreakdownResponse {
    pub countries: Vec<AnalyticsCountryCount>,
    pub total: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsPlatformCount {
    pub platform: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsPlatformDateCount {
    pub date: String,
    pub platform: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsDeviceBreakdownResponse {
    pub platforms: Vec<AnalyticsPlatformCount>,
    pub dates: Vec<AnalyticsPlatformDateCount>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsGeoTopSearchesResponse {
    pub country: String,
    pub searches: Vec<AnalyticsTopSearchEntry>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsRegionCount {
    pub region: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsGeoRegionsResponse {
    pub country: String,
    pub regions: Vec<AnalyticsRegionCount>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsCurrencyRevenue {
    pub currency: String,
    pub revenue: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsRevenueDateEntry {
    pub date: String,
    pub currencies: std::collections::HashMap<String, AnalyticsCurrencyRevenue>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsRevenueResponse {
    pub currencies: std::collections::HashMap<String, AnalyticsCurrencyRevenue>,
    pub dates: Vec<AnalyticsRevenueDateEntry>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsOverviewIndexSummary {
    pub index: String,
    pub searches: i64,
    pub no_results: i64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsOverviewResponse {
    #[serde(rename = "totalSearches")]
    pub total_searches: i64,
    #[serde(rename = "uniqueUsers")]
    pub unique_users: usize,
    #[serde(rename = "noResultRate", skip_serializing_if = "Option::is_none")]
    pub no_result_rate: Option<f64>,
    #[serde(rename = "clickThroughRate", skip_serializing_if = "Option::is_none")]
    pub click_through_rate: Option<f64>,
    pub indices: Vec<AnalyticsOverviewIndexSummary>,
    pub dates: Vec<AnalyticsDateCount>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsSeedResponse {
    pub status: String,
    pub index: String,
    pub days: u32,
    #[serde(rename = "seededDates")]
    pub seeded_dates: Vec<String>,
    #[serde(rename = "totalSearches")]
    pub total_searches: u64,
    #[serde(rename = "totalClicks")]
    pub total_clicks: u64,
    #[serde(rename = "totalConversions")]
    pub total_conversions: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsFlushResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsClearResponse {
    pub status: String,
    pub index: String,
    pub partitions_removed: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsStatusResponse {
    pub enabled: bool,
    pub has_data: bool,
    pub retention_days: u32,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsCleanupResponse {
    pub status: String,
    pub removed_indices: Vec<String>,
    pub removed_count: usize,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeedRequest {
    pub index: Option<String>,
    pub days: Option<u32>,
    pub search_count: Option<u32>,
    pub no_result_rate: Option<f64>,
    pub device_distribution: Option<std::collections::BTreeMap<String, f64>>,
    pub country_distribution: Option<std::collections::BTreeMap<String, f64>>,
}

/// Request body for `DELETE /2/analytics/clear`. Clear only targets an index; it must
/// not inherit the seed-only volume/distribution controls from `SeedRequest`, or the
/// published contract would advertise controls the clear handler silently ignores.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClearRequest {
    pub index: Option<String>,
}
