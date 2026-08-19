//! Stub summary for analytics_tests.rs.
use super::*;
use crate::auth::{ApiKey, SecuredKeyRestrictions, INVALID_API_CREDENTIALS_MESSAGE};
use crate::test_helpers::body_json;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    routing::get,
    Router,
};
use flapjack::analytics::{
    schema::{InsightEvent, SearchEvent},
    AnalyticsCollector, AnalyticsConfig,
};
use tempfile::TempDir;
use tower::ServiceExt;

fn test_analytics_config(tmp: &TempDir) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 3600,
        flush_size: 10_000,
        retention_days: 90,
    }
}

fn analytics_seed_request(index: &str, search_count: u32) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/2/analytics/seed")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "index": index,
                "days": 1,
                "searchCount": search_count
            })
            .to_string(),
        ))
        .unwrap()
}

fn analytics_only_key_restricted_to(indexes: Vec<String>) -> ApiKey {
    ApiKey {
        hash: "analytics-test-hash".to_string(),
        salt: "analytics-test-salt".to_string(),
        hmac_key: None,
        created_at: 0,
        acl: vec!["analytics".to_string()],
        description: "analytics test key".to_string(),
        indexes,
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: Vec::new(),
        restrict_sources: None,
        validity: 0,
    }
}

#[tokio::test]
async fn analytics_seed_endpoint_applies_configured_volume_and_distributions() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let app = Router::new()
        .route("/2/analytics/seed", axum::routing::post(seed_analytics))
        .route("/2/searches/count", get(get_search_count))
        .route("/2/searches/noResultRate", get(get_no_result_rate))
        .route("/2/devices", get(get_device_breakdown))
        .route("/2/geo", get(get_geo_breakdown))
        .with_state(Arc::new(AnalyticsQueryEngine::new(config)));

    let seed_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/2/analytics/seed")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "index": "configured-seed",
                        "days": 1,
                        "searchCount": 20,
                        "noResultRate": 0.25,
                        "deviceDistribution": {
                            "desktop": 0.0,
                            "mobile": 1.0,
                            "tablet": 0.0
                        },
                        "countryDistribution": { "DE": 1.0 }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(seed_response.status(), StatusCode::OK);
    let seed_body = body_json(seed_response).await;
    assert_eq!(seed_body["totalSearches"], 20);
    let seeded_dates = seed_body["seededDates"].as_array().unwrap();
    assert_eq!(seeded_dates.len(), 1);
    let seeded_date = seeded_dates[0].as_str().unwrap();
    let query = format!("index=configured-seed&startDate={seeded_date}&endDate={seeded_date}");

    let count = get_json(&app, &format!("/2/searches/count?{query}")).await;
    assert_eq!(count["count"], 20);

    let no_results = get_json(&app, &format!("/2/searches/noResultRate?{query}")).await;
    assert_eq!(no_results["count"], 20);
    assert_eq!(no_results["noResults"], 5);
    assert_eq!(no_results["rate"], 0.25);

    let devices = get_json(&app, &format!("/2/devices?{query}")).await;
    assert_eq!(
        devices["platforms"],
        serde_json::json!([{ "platform": "mobile", "count": 20 }])
    );

    let geography = get_json(&app, &format!("/2/geo?{query}")).await;
    assert_eq!(geography["total"], 20);
    assert_eq!(
        geography["countries"],
        serde_json::json!([{ "country": "DE", "count": 20 }])
    );
}

#[tokio::test]
async fn overview_endpoint_filters_to_requested_index() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let app = Router::new()
        .route("/2/analytics/seed", axum::routing::post(seed_analytics))
        .route("/2/overview", get(get_overview))
        .with_state(Arc::new(AnalyticsQueryEngine::new(config)));

    seed_index(&app, "overview-primary", 20).await;
    seed_index(&app, "overview-other", 35).await;

    let overview = get_json(&app, "/2/overview?index=overview-primary").await;

    assert_eq!(overview["totalSearches"], 20);
    assert_eq!(
        overview["indices"],
        serde_json::json!([{ "index": "overview-primary", "noResults": 1, "searches": 20 }])
    );
}

#[tokio::test]
async fn analytics_index_routes_reject_api_keys_restricted_to_other_indexes() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let app = Router::new()
        .route("/2/analytics/seed", axum::routing::post(seed_analytics))
        .route("/2/searches/count", get(get_search_count))
        .with_state(Arc::new(AnalyticsQueryEngine::new(config)));

    seed_index(&app, "allowed-analytics-index", 5).await;
    seed_index(&app, "blocked-analytics-index", 9).await;

    let mut request = Request::builder()
        .method(Method::GET)
        .uri("/2/searches/count?index=blocked-analytics-index")
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(analytics_only_key_restricted_to(vec![
            "allowed-analytics-index".to_string(),
        ]));

    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = body_json(response).await;
    assert_eq!(body["message"], INVALID_API_CREDENTIALS_MESSAGE);
}

#[tokio::test]
async fn analytics_index_routes_reject_secured_key_restrict_indices_mismatches() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let app = Router::new()
        .route("/2/analytics/seed", axum::routing::post(seed_analytics))
        .route("/2/searches/count", get(get_search_count))
        .with_state(Arc::new(AnalyticsQueryEngine::new(config)));

    seed_index(&app, "secured-allowed-index", 4).await;
    seed_index(&app, "secured-blocked-index", 6).await;

    let mut request = Request::builder()
        .method(Method::GET)
        .uri("/2/searches/count?index=secured-blocked-index")
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(analytics_only_key_restricted_to(Vec::new()));
    request.extensions_mut().insert(SecuredKeyRestrictions {
        restrict_indices: Some(vec!["secured-allowed-index".to_string()]),
        ..SecuredKeyRestrictions::default()
    });

    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = body_json(response).await;
    assert_eq!(body["message"], INVALID_API_CREDENTIALS_MESSAGE);
}

#[tokio::test]
async fn overview_route_blocks_server_wide_access_for_restricted_keys() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let app = Router::new()
        .route("/2/analytics/seed", axum::routing::post(seed_analytics))
        .route("/2/overview", get(get_overview))
        .with_state(Arc::new(AnalyticsQueryEngine::new(config)));

    seed_index(&app, "overview-allowed-index", 7).await;
    seed_index(&app, "overview-blocked-index", 11).await;

    let mut request = Request::builder()
        .method(Method::GET)
        .uri("/2/overview")
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(analytics_only_key_restricted_to(vec![
            "overview-allowed-index".to_string(),
        ]));

    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = body_json(response).await;
    assert_eq!(body["message"], INVALID_API_CREDENTIALS_MESSAGE);
}

#[tokio::test]
async fn analytics_clear_filesystem_failures_return_server_error() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_analytics_config(&tmp);
    config.data_dir = tmp.path().join("analytics-file");
    std::fs::write(&config.data_dir, b"not a directory").unwrap();

    let app = Router::new()
        .route("/2/analytics/clear", axum::routing::delete(clear_analytics))
        .with_state(Arc::new(AnalyticsQueryEngine::new(config)));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/2/analytics/clear")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "index": "clear-failure" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_json(response).await;
    assert_eq!(body["status"], 500);
}

#[tokio::test]
async fn analytics_seed_distinguishes_invalid_options_from_filesystem_failures() {
    let tmp = TempDir::new().unwrap();
    let healthy_config = test_analytics_config(&tmp);
    let healthy_app = Router::new()
        .route("/2/analytics/seed", axum::routing::post(seed_analytics))
        .with_state(Arc::new(AnalyticsQueryEngine::new(healthy_config.clone())));

    let healthy_response = healthy_app
        .clone()
        .oneshot(analytics_seed_request("healthy-seed", 3))
        .await
        .unwrap();
    assert_eq!(healthy_response.status(), StatusCode::OK);
    let healthy_body = body_json(healthy_response).await;
    assert_eq!(healthy_body["status"], "ok");
    assert_eq!(healthy_body["index"], "healthy-seed");
    assert_eq!(healthy_body["days"], 1);
    assert_eq!(healthy_body["totalSearches"], 3);
    assert_eq!(healthy_body["seededDates"].as_array().unwrap().len(), 1);

    let invalid_response = healthy_app
        .oneshot(analytics_seed_request("invalid-seed", 0))
        .await
        .unwrap();
    assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
    let invalid_body = body_json(invalid_response).await;
    assert_eq!(
        invalid_body,
        serde_json::json!({
            "message": "Seed error: searchCount must be between 1 and 100000",
            "status": 400
        })
    );

    let obstructed_index = "partition-setup-failure";
    std::fs::create_dir_all(
        healthy_config
            .target_artifact_paths(obstructed_index)
            .index_root,
    )
    .unwrap();
    std::fs::write(
        healthy_config.searches_dir(obstructed_index),
        b"not a directory",
    )
    .unwrap();
    let obstructed_app = Router::new()
        .route("/2/analytics/seed", axum::routing::post(seed_analytics))
        .with_state(Arc::new(AnalyticsQueryEngine::new(healthy_config)));

    let filesystem_response = obstructed_app
        .oneshot(analytics_seed_request(obstructed_index, 1))
        .await
        .unwrap();
    assert_eq!(
        filesystem_response.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let filesystem_body = body_json(filesystem_response).await;
    assert_eq!(
        filesystem_body,
        serde_json::json!({
            "message": "Internal server error",
            "status": 500
        })
    );
}

async fn seed_index(app: &Router, index: &str, search_count: u32) {
    let response = app
        .clone()
        .oneshot(analytics_seed_request(index, search_count))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn get_json(app: &Router, uri: &str) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    body_json(response).await
}

/// TODO: Document make_search.
fn make_search(query: &str, index: &str, query_id: &str) -> SearchEvent {
    SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: query.to_string(),
        query_id: Some(query_id.to_string()),
        index_name: index.to_string(),
        nb_hits: 10,
        processing_time_ms: 5,
        user_token: Some("user_1".to_string()),
        user_ip: Some("127.0.0.1".to_string()),
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: None,
        region: None,
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    }
}

/// TODO: Document make_conversion.
fn make_conversion(query_id: &str, index: &str, subtype: &str) -> InsightEvent {
    InsightEvent {
        event_type: "conversion".to_string(),
        event_subtype: Some(subtype.to_string()),
        event_name: "Conversion".to_string(),
        index: index.to_string(),
        user_token: "user_1".to_string(),
        authenticated_user_token: None,
        query_id: Some(query_id.to_string()),
        object_ids: vec!["obj1".to_string()],
        object_ids_alt: vec![],
        positions: None,
        timestamp: Some(chrono::Utc::now().timestamp_millis()),
        value: Some(10.0),
        currency: Some("USD".to_string()),
        interleaving_team: None,
    }
}

/// Verify the `/2/conversions/addToCartRate` endpoint uses `addToCartCount` (not `conversionCount`) in both top-level and per-date entries, and computes rate as addToCartCount / trackedSearchCount.
#[tokio::test]
async fn add_to_cart_rate_response_uses_add_to_cart_count_field() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let qid_a = "a".repeat(32);
    let qid_b = "b".repeat(32);

    collector.record_search(make_search("iphone", "products", &qid_a));
    collector.record_search(make_search("iphone", "products", &qid_b));
    collector.record_insight(make_conversion(&qid_a, "products", "addToCart"));
    collector.record_insight(make_conversion(&qid_b, "products", "purchase"));
    collector.flush_all();

    let app = Router::new()
        .route("/2/conversions/addToCartRate", get(get_add_to_cart_rate))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/conversions/addToCartRate?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["addToCartCount"], 1);
    assert_eq!(body["trackedSearchCount"], 2);
    assert!((body["rate"].as_f64().unwrap() - 0.5).abs() < 0.001);
    assert!(body.get("conversionCount").is_none());
    let dates = body["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 1);
    assert_eq!(dates[0]["addToCartCount"], 1);
    assert_eq!(dates[0]["trackedSearchCount"], 2);
    assert!(dates[0].get("conversionCount").is_none());
}

/// Verify the `/2/conversions/purchaseRate` endpoint uses `purchaseCount` (not `conversionCount`) in both top-level and per-date entries, matching the Algolia contract.
#[tokio::test]
async fn purchase_rate_response_uses_purchase_count_field() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let qid_a = "a".repeat(32);
    let qid_b = "b".repeat(32);

    collector.record_search(make_search("iphone", "products", &qid_a));
    collector.record_search(make_search("iphone", "products", &qid_b));
    collector.record_insight(make_conversion(&qid_a, "products", "addToCart"));
    collector.record_insight(make_conversion(&qid_b, "products", "purchase"));
    collector.flush_all();

    let app = Router::new()
        .route("/2/conversions/purchaseRate", get(get_purchase_rate))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/conversions/purchaseRate?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["purchaseCount"], 1);
    assert_eq!(body["trackedSearchCount"], 2);
    assert!((body["rate"].as_f64().unwrap() - 0.5).abs() < 0.001);
    assert!(body.get("conversionCount").is_none());
    let dates = body["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 1);
    assert_eq!(dates[0]["purchaseCount"], 1);
    assert_eq!(dates[0]["trackedSearchCount"], 2);
    assert!(dates[0].get("conversionCount").is_none());
}

/// Verify the add-to-cart rate endpoint returns `null` for `rate`, zero for `trackedSearchCount`, and zero for `addToCartCount` when no search events exist.
#[tokio::test]
async fn add_to_cart_rate_returns_null_rate_when_no_tracked_searches() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let app = Router::new()
        .route("/2/conversions/addToCartRate", get(get_add_to_cart_rate))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/conversions/addToCartRate?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert!(body["rate"].is_null());
    assert_eq!(body["trackedSearchCount"], 0);
    assert_eq!(body["addToCartCount"], 0);
}

/// Verify the `/2/conversions/revenue` endpoint returns a `currencies` object keyed by currency code, each containing `currency` and `revenue` fields, with a per-day `dates` array mirroring the same structure.
#[tokio::test]
async fn revenue_handler_returns_currency_map_structure() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let qid = "a".repeat(32);

    collector.record_search(make_search("iphone", "products", &qid));
    collector.record_insight(make_conversion(&qid, "products", "purchase"));
    collector.flush_all();

    let app = Router::new()
        .route("/2/conversions/revenue", get(get_revenue))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/conversions/revenue?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let currencies = body["currencies"].as_object().unwrap();
    assert!(currencies.contains_key("USD"), "make_conversion uses USD");
    assert_eq!(currencies["USD"]["currency"], "USD");
    assert!(currencies["USD"]["revenue"].as_f64().unwrap() > 0.0);

    let dates = body["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 1);
    let day_currencies = dates[0]["currencies"].as_object().unwrap();
    assert!(day_currencies.contains_key("USD"));
}

/// Verify the revenue endpoint returns an empty `currencies` object and empty `dates` array when no purchase events exist.
#[tokio::test]
async fn revenue_handler_empty_returns_empty_structure() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let app = Router::new()
        .route("/2/conversions/revenue", get(get_revenue))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/conversions/revenue?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let currencies = body["currencies"].as_object().unwrap();
    assert!(currencies.is_empty());
    let dates = body["dates"].as_array().unwrap();
    assert!(dates.is_empty());
}

/// Verify the `/2/countries` endpoint does not include a `total` field in the response, matching the Algolia contract.
#[tokio::test]
async fn countries_handler_omits_total_field() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let qid = "a".repeat(32);

    let mut search = make_search("iphone", "products", &qid);
    search.country = Some("US".to_string());
    collector.record_search(search);
    collector.flush_all();

    let app = Router::new()
        .route("/2/countries", get(get_countries))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/countries?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let countries = body["countries"].as_array().unwrap();
    assert_eq!(countries.len(), 1);
    assert_eq!(countries[0]["country"], "US");
    assert_eq!(countries[0]["count"], 1);
    // Must NOT have "total" field — Algolia doesn't include it
    assert!(
        body.get("total").is_none(),
        "countries endpoint should not include 'total' field"
    );
}

// ── Helpers for multi-day and multi-type tests ──

fn make_search_at(query: &str, index: &str, query_id: &str, timestamp_ms: i64) -> SearchEvent {
    SearchEvent {
        timestamp_ms,
        ..make_search(query, index, query_id)
    }
}

fn make_conversion_at(
    query_id: &str,
    index: &str,
    subtype: &str,
    timestamp_ms: i64,
) -> InsightEvent {
    InsightEvent {
        timestamp: Some(timestamp_ms),
        ..make_conversion(query_id, index, subtype)
    }
}

fn make_conversion_with_currency(
    query_id: &str,
    index: &str,
    value: f64,
    currency: &str,
    timestamp_ms: i64,
) -> InsightEvent {
    InsightEvent {
        value: Some(value),
        currency: Some(currency.to_string()),
        timestamp: Some(timestamp_ms),
        ..make_conversion(query_id, index, "purchase")
    }
}

/// Create a test `InsightEvent` of type "click" with position 3, linked to a query by ID.
///
/// # Arguments
///
/// * `query_id` - The query ID to associate the click with.
/// * `index` - The index name.
///
/// # Returns
///
/// An `InsightEvent` with one object ID and a single click position at index 3.
fn make_click(query_id: &str, index: &str) -> InsightEvent {
    InsightEvent {
        event_type: "click".to_string(),
        event_subtype: None,
        event_name: "Click".to_string(),
        index: index.to_string(),
        user_token: "user_1".to_string(),
        authenticated_user_token: None,
        query_id: Some(query_id.to_string()),
        object_ids: vec!["obj1".to_string()],
        object_ids_alt: vec![],
        positions: Some(vec![3]),
        timestamp: Some(chrono::Utc::now().timestamp_millis()),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

fn yesterday_date() -> String {
    (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

fn yesterday_ms() -> i64 {
    (chrono::Utc::now() - chrono::Duration::days(1)).timestamp_millis()
}

fn today_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ── B1: addToCartRate multi-day + date format ──

/// Verify the add-to-cart rate endpoint returns per-day breakdowns across a two-day span with correct `addToCartCount` and `trackedSearchCount` fields in YYYY-MM-DD date format.
#[tokio::test]
async fn add_to_cart_rate_multi_day_dates_array() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let yesterday = yesterday_date();

    // Day 1 (yesterday): 1 search, 1 addToCart
    let qid_y = "y".repeat(32);
    collector.record_search(make_search_at("iphone", "products", &qid_y, yesterday_ms()));
    collector.record_insight(make_conversion_at(
        &qid_y,
        "products",
        "addToCart",
        yesterday_ms(),
    ));

    // Day 2 (today): 2 searches, 1 addToCart
    let qid_t1 = "t".repeat(32);
    let qid_t2 = "u".repeat(32);
    collector.record_search(make_search_at("iphone", "products", &qid_t1, today_ms()));
    collector.record_search(make_search_at("iphone", "products", &qid_t2, today_ms()));
    collector.record_insight(make_conversion_at(
        &qid_t1,
        "products",
        "addToCart",
        today_ms(),
    ));

    collector.flush_all();

    let app = Router::new()
        .route("/2/conversions/addToCartRate", get(get_add_to_cart_rate))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/conversions/addToCartRate?index=products&startDate={yesterday}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    // Top-level totals: 3 tracked searches, 2 addToCart
    assert_eq!(body["trackedSearchCount"], 3);
    assert_eq!(body["addToCartCount"], 2);

    let dates = body["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 2, "should have entries for both days");

    // Each date entry has correct fields
    for entry in dates {
        assert!(entry.get("date").is_some());
        assert!(entry.get("rate").is_some());
        assert!(entry.get("addToCartCount").is_some());
        assert!(entry.get("trackedSearchCount").is_some());

        // Date format must be YYYY-MM-DD (not ISO timestamp)
        let date_str = entry["date"].as_str().unwrap();
        assert_eq!(
            date_str.len(),
            10,
            "date should be YYYY-MM-DD format: {date_str}"
        );
        assert!(
            chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d").is_ok(),
            "date should parse as YYYY-MM-DD: {date_str}"
        );
    }
}

// ── B2: purchaseRate multi-day ──

/// Verify the purchase rate endpoint returns per-day breakdowns across a two-day span, using `purchaseCount` (never `conversionCount`) in both top-level and per-date entries.
#[tokio::test]
async fn purchase_rate_multi_day_dates_array() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let yesterday = yesterday_date();

    // Day 1: 1 search, 1 purchase
    let qid_y = "y".repeat(32);
    collector.record_search(make_search_at("laptop", "products", &qid_y, yesterday_ms()));
    collector.record_insight(make_conversion_at(
        &qid_y,
        "products",
        "purchase",
        yesterday_ms(),
    ));

    // Day 2: 2 searches, 1 purchase
    let qid_t1 = "t".repeat(32);
    let qid_t2 = "u".repeat(32);
    collector.record_search(make_search_at("laptop", "products", &qid_t1, today_ms()));
    collector.record_search(make_search_at("laptop", "products", &qid_t2, today_ms()));
    collector.record_insight(make_conversion_at(
        &qid_t1,
        "products",
        "purchase",
        today_ms(),
    ));

    collector.flush_all();

    let app = Router::new()
        .route("/2/conversions/purchaseRate", get(get_purchase_rate))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/conversions/purchaseRate?index=products&startDate={yesterday}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["trackedSearchCount"], 3);
    assert_eq!(body["purchaseCount"], 2);
    // Must be purchaseCount, NOT conversionCount
    assert!(
        body.get("conversionCount").is_none(),
        "purchaseRate must not have conversionCount"
    );

    let dates = body["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 2);
    for entry in dates {
        assert!(entry.get("purchaseCount").is_some());
        assert!(entry.get("trackedSearchCount").is_some());
        assert!(
            entry.get("conversionCount").is_none(),
            "dates entries must use purchaseCount"
        );
    }
}

// ── B3: revenue multi-currency ──

/// Verify the revenue endpoint returns independent currency entries for USD and EUR with correct numeric revenue values when purchases use different currencies.
#[tokio::test]
async fn revenue_multi_currency_usd_and_eur() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let qid_a = "a".repeat(32);
    let qid_b = "b".repeat(32);

    collector.record_search(make_search("iphone", "products", &qid_a));
    collector.record_search(make_search("iphone", "products", &qid_b));
    collector.record_insight(make_conversion_with_currency(
        &qid_a,
        "products",
        29.99,
        "USD",
        today_ms(),
    ));
    collector.record_insight(make_conversion_with_currency(
        &qid_b,
        "products",
        25.50,
        "EUR",
        today_ms(),
    ));

    collector.flush_all();

    let app = Router::new()
        .route("/2/conversions/revenue", get(get_revenue))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/conversions/revenue?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let currencies = body["currencies"].as_object().unwrap();

    // Both currencies present with independent totals
    assert!(currencies.contains_key("USD"), "should have USD key");
    assert!(currencies.contains_key("EUR"), "should have EUR key");
    assert_eq!(currencies["USD"]["currency"].as_str().unwrap(), "USD");
    assert_eq!(currencies["EUR"]["currency"].as_str().unwrap(), "EUR");

    // Revenue is numeric float (not string)
    let usd_rev = currencies["USD"]["revenue"].as_f64();
    assert!(usd_rev.is_some(), "revenue should be numeric float");
    assert!((usd_rev.unwrap() - 29.99).abs() < 0.01);

    let eur_rev = currencies["EUR"]["revenue"].as_f64();
    assert!(eur_rev.is_some(), "revenue should be numeric float");
    assert!((eur_rev.unwrap() - 25.50).abs() < 0.01);
}

/// Verify the revenue endpoint returns separate date entries for a two-day span, each with per-currency breakdown using YYYY-MM-DD date format.
#[tokio::test]
async fn revenue_multi_day_per_currency_breakdown() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let yesterday = yesterday_date();

    let qid_a = "a".repeat(32);
    let qid_b = "b".repeat(32);

    collector.record_search(make_search_at("iphone", "products", &qid_a, yesterday_ms()));
    collector.record_search(make_search_at("iphone", "products", &qid_b, today_ms()));
    collector.record_insight(make_conversion_with_currency(
        &qid_a,
        "products",
        10.0,
        "USD",
        yesterday_ms(),
    ));
    collector.record_insight(make_conversion_with_currency(
        &qid_b,
        "products",
        20.0,
        "USD",
        today_ms(),
    ));

    collector.flush_all();

    let app = Router::new()
        .route("/2/conversions/revenue", get(get_revenue))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/conversions/revenue?index=products&startDate={yesterday}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let dates = body["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 2, "should have entries for both days");

    for entry in dates {
        let date_str = entry["date"].as_str().unwrap();
        assert_eq!(date_str.len(), 10, "date should be YYYY-MM-DD");
        let day_currencies = entry["currencies"].as_object().unwrap();
        assert!(
            day_currencies.contains_key("USD"),
            "each day should have USD"
        );
        let day_usd = &day_currencies["USD"];
        assert!(
            day_usd["revenue"].as_f64().is_some(),
            "revenue should be numeric"
        );
        assert_eq!(day_usd["currency"].as_str().unwrap(), "USD");
    }
}

// ── B4: countries multi-country ──

/// Verify the `/2/countries` endpoint returns multiple countries ordered by search count descending, with each entry containing exactly two fields (`country`, `count`) and no top-level `total` field.
#[tokio::test]
async fn countries_multi_country_ordered_by_count_desc() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    // 3 searches from US, 1 from DE
    for i in 0..3 {
        let qid = "a".repeat(31) + &i.to_string();
        let mut search = make_search("iphone", "products", &qid);
        search.country = Some("US".to_string());
        collector.record_search(search);
    }
    let qid_de = "d".repeat(32);
    let mut search_de = make_search("laptop", "products", &qid_de);
    search_de.country = Some("DE".to_string());
    collector.record_search(search_de);

    collector.flush_all();

    let app = Router::new()
        .route("/2/countries", get(get_countries))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/countries?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let countries = body["countries"].as_array().unwrap();
    assert_eq!(countries.len(), 2);

    // Ordered by count descending: US (3) first, DE (1) second
    assert_eq!(countries[0]["country"].as_str().unwrap(), "US");
    assert_eq!(countries[0]["count"].as_i64().unwrap(), 3);
    assert_eq!(countries[1]["country"].as_str().unwrap(), "DE");
    assert_eq!(countries[1]["count"].as_i64().unwrap(), 1);

    // No extra fields per entry — only country (string) and count (integer)
    for entry in countries {
        let obj = entry.as_object().unwrap();
        assert_eq!(
            obj.len(),
            2,
            "each entry should have exactly 2 fields: country, count"
        );
        assert!(entry["country"].is_string());
        assert!(entry["count"].is_i64());
    }

    // No total at top level
    assert!(body.get("total").is_none());
}

// ── Geo regions endpoint ──

/// Verify the `/2/geo/:country/regions` endpoint returns region-level counts filtered to a single country, ordered by count descending, and excludes regions from other countries.
#[tokio::test]
async fn geo_regions_returns_region_breakdown_for_country() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    // 2 searches from US/CA, 1 from US/NY, 1 from DE (should not appear)
    for i in 0..2 {
        let qid = "c".repeat(31) + &i.to_string();
        let mut search = make_search("iphone", "products", &qid);
        search.country = Some("US".to_string());
        search.region = Some("CA".to_string());
        collector.record_search(search);
    }
    let qid_ny = "n".repeat(32);
    let mut search_ny = make_search("laptop", "products", &qid_ny);
    search_ny.country = Some("US".to_string());
    search_ny.region = Some("NY".to_string());
    collector.record_search(search_ny);

    let qid_de = "e".repeat(32);
    let mut search_de = make_search("tablet", "products", &qid_de);
    search_de.country = Some("DE".to_string());
    search_de.region = Some("BY".to_string());
    collector.record_search(search_de);

    collector.flush_all();

    let app = Router::new()
        .route("/2/geo/:country/regions", get(get_geo_regions))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/geo/US/regions?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["country"].as_str().unwrap(), "US");

    let regions = body["regions"].as_array().unwrap();
    assert_eq!(regions.len(), 2);

    // Ordered by count descending: CA (2) first, NY (1) second
    assert_eq!(regions[0]["region"].as_str().unwrap(), "CA");
    assert_eq!(regions[0]["count"].as_i64().unwrap(), 2);
    assert_eq!(regions[1]["region"].as_str().unwrap(), "NY");
    assert_eq!(regions[1]["count"].as_i64().unwrap(), 1);
}

// ── B5a: top searches response shape ──

/// Verify the `/2/searches` endpoint returns entries with `search`, `count`, and `nbHits` fields when `clickAnalytics` is not enabled.
#[tokio::test]
async fn top_searches_response_shape_without_click_analytics() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let qid = "a".repeat(32);
    collector.record_search(make_search("iphone", "products", &qid));
    collector.flush_all();

    let app = Router::new()
        .route("/2/searches", get(get_top_searches))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/searches?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let searches = body["searches"].as_array().unwrap();
    assert_eq!(searches.len(), 1);

    let entry = &searches[0];
    // Algolia shape: { search (string), count (int), nbHits (int) }
    assert_eq!(entry["search"].as_str().unwrap(), "iphone");
    assert!(entry["count"].as_i64().is_some(), "count should be integer");
    assert!(entry.get("nbHits").is_some(), "should have nbHits field");
    assert!(
        entry["nbHits"].as_i64().is_some(),
        "nbHits should be integer"
    );
    assert!(
        entry.get("_nbHitsSum").is_none() && entry.get("_nbHitsCount").is_none(),
        "cluster-only weighted-average components must not leak into the public response"
    );

    let local_only_req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/searches?index=products&startDate={today}&endDate={today}"
        ))
        .header("X-Flapjack-Local-Only", "true")
        .body(Body::empty())
        .unwrap();
    let local_only_body = body_json(app.oneshot(local_only_req).await.unwrap()).await;
    let local_only_entry = &local_only_body["searches"][0];
    assert!(local_only_entry["_nbHitsSum"].as_i64().is_some());
    assert!(local_only_entry["_nbHitsCount"].as_i64().is_some());
}

/// Verify the `/2/searches` endpoint includes `trackedSearchCount`, `clickCount`, `clickThroughRate`, `conversionRate`, `conversionCount`, and `averageClickPosition` when `clickAnalytics=true`.
#[tokio::test]
async fn top_searches_with_click_analytics_has_enriched_fields() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let qid = "a".repeat(32);
    collector.record_search(make_search("iphone", "products", &qid));
    collector.record_insight(make_click(&qid, "products"));
    collector.record_insight(make_conversion(&qid, "products", "purchase"));
    collector.flush_all();

    let app = Router::new()
        .route("/2/searches", get(get_top_searches))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/searches?index=products&startDate={today}&endDate={today}&clickAnalytics=true"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let searches = body["searches"].as_array().unwrap();
    assert!(!searches.is_empty());

    let entry = &searches[0];
    // Algolia enriched fields with clickAnalytics=true
    assert!(
        entry.get("trackedSearchCount").is_some(),
        "missing trackedSearchCount"
    );
    assert!(entry.get("clickCount").is_some(), "missing clickCount");
    assert!(
        entry.get("clickThroughRate").is_some(),
        "missing clickThroughRate"
    );
    assert!(
        entry.get("conversionRate").is_some(),
        "missing conversionRate"
    );
    assert!(
        entry.get("conversionCount").is_some(),
        "missing conversionCount"
    );
    assert!(
        entry.get("averageClickPosition").is_some(),
        "missing averageClickPosition"
    );
}

// ── B5b: clickThroughRate response shape ──

/// Verify the `/2/clicks/clickThroughRate` endpoint returns `rate`, `clickCount`, `trackedSearchCount`, and a `dates` array with per-day breakdowns including the same fields.
#[tokio::test]
async fn click_through_rate_response_shape_with_data() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let qid = "a".repeat(32);
    collector.record_search(make_search("iphone", "products", &qid));
    collector.record_insight(make_click(&qid, "products"));
    collector.flush_all();

    let app = Router::new()
        .route("/2/clicks/clickThroughRate", get(get_click_through_rate))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/clicks/clickThroughRate?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    // Algolia shape: { rate, clickCount, trackedSearchCount, dates }
    assert!(body.get("rate").is_some(), "missing rate");
    assert_eq!(body["clickCount"].as_i64().unwrap(), 1);
    assert_eq!(body["trackedSearchCount"].as_i64().unwrap(), 1);
    assert!((body["rate"].as_f64().unwrap() - 1.0).abs() < 0.001);

    let dates = body["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 1);
    assert!(dates[0].get("date").is_some());
    assert!(dates[0].get("rate").is_some());
    assert!(dates[0].get("clickCount").is_some());
    assert!(dates[0].get("trackedSearchCount").is_some());
}

/// Verify the click-through rate endpoint returns `null` for `rate` (not zero) with zero tracked searches, matching Algolia's null-rate convention.
#[tokio::test]
async fn click_through_rate_empty_returns_null_rate() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let app = Router::new()
        .route("/2/clicks/clickThroughRate", get(get_click_through_rate))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/clicks/clickThroughRate?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    // Algolia: rate is null (not 0) when no data
    assert!(
        body["rate"].is_null(),
        "rate should be null with no data, got: {}",
        body["rate"]
    );
    assert_eq!(body["clickCount"].as_i64().unwrap(), 0);
    assert_eq!(body["trackedSearchCount"].as_i64().unwrap(), 0);
}

// ── B5c: conversionRate response shape ──

/// Verify the `/2/conversions/conversionRate` endpoint uses `conversionCount` (not subtype-specific names) in both top-level and per-date entries, matching the Algolia contract.
#[tokio::test]
async fn conversion_rate_response_uses_conversion_count_field() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let qid = "a".repeat(32);

    collector.record_search(make_search("iphone", "products", &qid));
    collector.record_insight(make_conversion(&qid, "products", "purchase"));
    collector.flush_all();

    let app = Router::new()
        .route("/2/conversions/conversionRate", get(get_conversion_rate))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/conversions/conversionRate?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    // Algolia shape: { rate, trackedSearchCount, conversionCount, dates }
    assert!(
        body.get("conversionCount").is_some(),
        "must have conversionCount field"
    );
    assert_eq!(body["conversionCount"].as_i64().unwrap(), 1);
    assert_eq!(body["trackedSearchCount"].as_i64().unwrap(), 1);
    assert!((body["rate"].as_f64().unwrap() - 1.0).abs() < 0.001);

    let dates = body["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 1);
    assert!(
        dates[0].get("conversionCount").is_some(),
        "dates must use conversionCount"
    );
    assert!(dates[0].get("trackedSearchCount").is_some());
}

/// Verify the conversion rate endpoint returns `null` for `rate` (not zero) with zero tracked searches, matching Algolia's null-rate convention.
#[tokio::test]
async fn conversion_rate_empty_returns_null_rate() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let engine = Arc::new(AnalyticsQueryEngine::new(config));
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let app = Router::new()
        .route("/2/conversions/conversionRate", get(get_conversion_rate))
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/conversions/conversionRate?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert!(body["rate"].is_null(), "rate should be null with no data");
    assert_eq!(body["conversionCount"].as_i64().unwrap(), 0);
    assert_eq!(body["trackedSearchCount"].as_i64().unwrap(), 0);
}

mod stage5_analytics_integration_tests {
    use super::*;

    /// Create a minimal test `SearchEvent` with an optional country field for GeoIP integration tests.
    ///
    /// # Arguments
    ///
    /// * `index` - The index name.
    /// * `country` - Optional two-letter country code to simulate GeoIP enrichment.
    fn make_search_event(index: &str, country: Option<&str>) -> SearchEvent {
        SearchEvent {
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            query: "test".to_string(),
            query_id: Some("a".repeat(32)),
            index_name: index.to_string(),
            nb_hits: 1,
            processing_time_ms: 5,
            user_token: Some("user_1".to_string()),
            user_ip: Some("8.8.8.8".to_string()),
            filters: None,
            facets: None,
            analytics_tags: None,
            page: 0,
            hits_per_page: 20,
            has_results: true,
            country: country.map(|s| s.to_string()),
            region: None,
            experiment_id: None,
            variant_id: None,
            assignment_method: None,
        }
    }

    async fn fetch_countries_json(app: &Router, index: &str, date: &str) -> serde_json::Value {
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/2/countries?index={index}&startDate={date}&endDate={date}"
            ))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    /// TODO: Document countries_endpoint_reflects_enriched_country_field.
    #[tokio::test]
    async fn countries_endpoint_reflects_enriched_country_field() {
        let tmp = TempDir::new().unwrap();
        let config = test_analytics_config(&tmp);
        let collector = AnalyticsCollector::new(config.clone());
        let engine = Arc::new(AnalyticsQueryEngine::new(config));
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        // Simulate what the search handler does when GeoIP enrichment resolves "DE"
        collector.record_search(make_search_event("enrichment_idx", Some("DE")));
        collector.flush_all();

        let app = Router::new()
            .route("/2/countries", get(get_countries))
            .with_state(engine);

        let body = fetch_countries_json(&app, "enrichment_idx", &today).await;
        let countries = body["countries"].as_array().unwrap();
        assert_eq!(countries.len(), 1, "should have one country entry");
        assert_eq!(
            countries[0]["country"], "DE",
            "country should be DE from enrichment"
        );
        assert_eq!(countries[0]["count"], 1);
    }
    /// TODO: Document analytics_event_persisted_without_country_when_geoip_unavailable.
    #[tokio::test]
    async fn analytics_event_persisted_without_country_when_geoip_unavailable() {
        let tmp = TempDir::new().unwrap();
        let config = test_analytics_config(&tmp);
        let collector = AnalyticsCollector::new(config.clone());
        let engine = Arc::new(AnalyticsQueryEngine::new(config));
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        // country=None simulates missing GeoIP reader
        collector.record_search(make_search_event("no_geoip_idx", None));
        collector.flush_all();

        let app = Router::new()
            .route("/2/countries", get(get_countries))
            .route("/2/searches", get(get_top_searches))
            .with_state(engine);

        let body = fetch_countries_json(&app, "no_geoip_idx", &today).await;

        // No country entry — but also no error. Empty array is correct.
        let countries = body["countries"].as_array().unwrap();
        assert!(
            countries.is_empty(),
            "countries should be empty when no GeoIP enrichment, got: {countries:?}"
        );

        let req = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/2/searches?index=no_geoip_idx&startDate={today}&endDate={today}"
            ))
            .body(Body::empty())
            .unwrap();
        let searches_body = body_json(app.clone().oneshot(req).await.unwrap()).await;
        let searches = searches_body["searches"].as_array().unwrap();
        assert_eq!(searches.len(), 1, "search event should still be persisted");
        assert_eq!(searches[0]["search"], "test");
        assert_eq!(searches[0]["count"], 1);
    }
}

mod stage_b_contract_parity_tests {
    use super::*;

    fn make_search_with_country_and_tags(
        query: &str,
        index: &str,
        query_id: &str,
        country: &str,
        tags: Option<&str>,
    ) -> SearchEvent {
        SearchEvent {
            country: Some(country.to_string()),
            analytics_tags: tags.map(|t| t.to_string()),
            ..make_search(query, index, query_id)
        }
    }

    fn make_conversion_with_currency(
        query_id: &str,
        index: &str,
        value: f64,
        currency: &str,
    ) -> InsightEvent {
        InsightEvent {
            value: Some(value),
            currency: Some(currency.to_string()),
            ..make_conversion(query_id, index, "purchase")
        }
    }

    // ── B.1: countries limit/offset/orderBy ──
    /// TODO: Document countries_supports_limit_offset_order_by_direction_consistently.
    #[tokio::test]
    async fn countries_supports_limit_offset_order_by_direction_consistently() {
        let tmp = TempDir::new().unwrap();
        let config = test_analytics_config(&tmp);
        let collector = AnalyticsCollector::new(config.clone());
        let engine = Arc::new(AnalyticsQueryEngine::new(config));
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        // Seed 4 countries with distinct counts: US=4, DE=3, FR=2, JP=1
        for i in 0..4 {
            let qid = format!("us{:030}", i);
            let mut s = make_search("q", "idx", &qid);
            s.country = Some("US".to_string());
            collector.record_search(s);
        }
        for i in 0..3 {
            let qid = format!("de{:030}", i);
            let mut s = make_search("q", "idx", &qid);
            s.country = Some("DE".to_string());
            collector.record_search(s);
        }
        for i in 0..2 {
            let qid = format!("fr{:030}", i);
            let mut s = make_search("q", "idx", &qid);
            s.country = Some("FR".to_string());
            collector.record_search(s);
        }
        {
            let qid = format!("jp{:030}", 0);
            let mut s = make_search("q", "idx", &qid);
            s.country = Some("JP".to_string());
            collector.record_search(s);
        }
        collector.flush_all();

        let app = Router::new()
            .route("/2/countries", get(get_countries))
            .with_state(engine);

        // (a) limit=2 → only top 2
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/2/countries?index=idx&startDate={today}&endDate={today}&limit=2"
            ))
            .body(Body::empty())
            .unwrap();
        let body = body_json(app.clone().oneshot(req).await.unwrap()).await;
        let countries = body["countries"].as_array().unwrap();
        assert_eq!(countries.len(), 2, "limit=2 should return 2 entries");
        assert_eq!(countries[0]["country"], "US");
        assert_eq!(countries[1]["country"], "DE");

        // (b) offset=1&limit=2 → skip first, take next 2
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/2/countries?index=idx&startDate={today}&endDate={today}&limit=2&offset=1"
            ))
            .body(Body::empty())
            .unwrap();
        let body = body_json(app.clone().oneshot(req).await.unwrap()).await;
        let countries = body["countries"].as_array().unwrap();
        assert_eq!(
            countries.len(),
            2,
            "offset=1&limit=2 should return 2 entries"
        );
        assert_eq!(
            countries[0]["country"], "DE",
            "offset=1 should skip US and start at DE"
        );
        assert_eq!(countries[1]["country"], "FR");

        // (c) orderBy=count:asc → ascending order
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/2/countries?index=idx&startDate={today}&endDate={today}&orderBy=count:asc"
            ))
            .body(Body::empty())
            .unwrap();
        let body = body_json(app.clone().oneshot(req).await.unwrap()).await;
        let countries = body["countries"].as_array().unwrap();
        assert!(countries.len() >= 4, "should return all 4 countries");
        // Ascending: JP (1) should be first
        assert_eq!(
            countries[0]["country"], "JP",
            "orderBy=count:asc should put lowest count first"
        );
        assert_eq!(countries[0]["count"], 1);
    }

    // ── B.2: countries invalid date range → 400 ──
    /// TODO: Document countries_invalid_date_range_returns_algolia_shaped_400.
    #[tokio::test]
    async fn countries_invalid_date_range_returns_algolia_shaped_400() {
        let tmp = TempDir::new().unwrap();
        let config = test_analytics_config(&tmp);
        let engine = Arc::new(AnalyticsQueryEngine::new(config));

        let app = Router::new()
            .route("/2/countries", get(get_countries))
            .with_state(engine);

        // (a) endDate before startDate
        let req = Request::builder()
            .method(Method::GET)
            .uri("/2/countries?index=idx&startDate=2026-02-20&endDate=2026-02-10")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "inverted date range must return 400"
        );
        let body = body_json(resp).await;
        assert_eq!(body["status"], 400);
        assert!(
            body["message"].as_str().is_some(),
            "error body must have a message field"
        );

        // (b) malformed date
        let req = Request::builder()
            .method(Method::GET)
            .uri("/2/countries?index=idx&startDate=not-a-date&endDate=2026-02-20")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "malformed date must return 400"
        );
        let body = body_json(resp).await;
        assert_eq!(body["status"], 400);
    }

    // ── B.3: countries tags filter ──
    /// TODO: Document countries_tags_filter_applies_when_tags_present.
    #[tokio::test]
    async fn countries_tags_filter_applies_when_tags_present() {
        let tmp = TempDir::new().unwrap();
        let config = test_analytics_config(&tmp);
        let collector = AnalyticsCollector::new(config.clone());
        let engine = Arc::new(AnalyticsQueryEngine::new(config));
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        // 2 US searches with tag "platform:mobile", 1 US search with tag "platform:desktop"
        for i in 0..2 {
            let qid = format!("mob{:029}", i);
            collector.record_search(make_search_with_country_and_tags(
                "q",
                "idx",
                &qid,
                "US",
                Some("platform:mobile"),
            ));
        }
        {
            let qid = format!("dsk{:029}", 0);
            collector.record_search(make_search_with_country_and_tags(
                "q",
                "idx",
                &qid,
                "US",
                Some("platform:desktop"),
            ));
        }
        // 1 DE search with tag "platform:mobile"
        {
            let qid = format!("dem{:029}", 0);
            collector.record_search(make_search_with_country_and_tags(
                "q",
                "idx",
                &qid,
                "DE",
                Some("platform:mobile"),
            ));
        }
        collector.flush_all();

        let app = Router::new()
            .route("/2/countries", get(get_countries))
            .with_state(engine);

        // Filter by tags=platform:mobile → US:2 + DE:1, no desktop searches
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/2/countries?index=idx&startDate={today}&endDate={today}&tags=platform:mobile"
            ))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        let countries = body["countries"].as_array().unwrap();

        // Total mobile searches: US=2, DE=1 → 2 country entries
        assert_eq!(countries.len(), 2, "should have US and DE with mobile tag");

        let us = countries.iter().find(|c| c["country"] == "US").unwrap();
        let de = countries.iter().find(|c| c["country"] == "DE").unwrap();
        assert_eq!(us["count"], 2, "US should have 2 mobile searches");
        assert_eq!(de["count"], 1, "DE should have 1 mobile search");

        // The desktop-only search should NOT be counted
        let total: i64 = countries.iter().map(|c| c["count"].as_i64().unwrap()).sum();
        assert_eq!(total, 3, "total should be 3 (only mobile-tagged), not 4");
    }

    // ── B.4: conversion date bucket format consistency ──
    /// TODO: Document conversion_endpoints_share_identical_date_bucket_format.
    #[tokio::test]
    async fn conversion_endpoints_share_identical_date_bucket_format() {
        let tmp = TempDir::new().unwrap();
        let config = test_analytics_config(&tmp);
        let collector = AnalyticsCollector::new(config.clone());
        let engine = Arc::new(AnalyticsQueryEngine::new(config));
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let qid = "a".repeat(32);

        collector.record_search(make_search("q", "products", &qid));
        collector.record_insight(make_conversion(&qid, "products", "addToCart"));
        collector.record_insight(make_conversion(&qid, "products", "purchase"));
        collector.flush_all();

        let app = Router::new()
            .route("/2/conversions/addToCartRate", get(get_add_to_cart_rate))
            .route("/2/conversions/purchaseRate", get(get_purchase_rate))
            .route("/2/conversions/conversionRate", get(get_conversion_rate))
            .route("/2/conversions/revenue", get(get_revenue))
            .with_state(engine);

        let endpoints = [
            "/2/conversions/addToCartRate",
            "/2/conversions/purchaseRate",
            "/2/conversions/conversionRate",
            "/2/conversions/revenue",
        ];

        let mut all_dates: Vec<(String, String)> = Vec::new();

        for endpoint in &endpoints {
            let req = Request::builder()
                .method(Method::GET)
                .uri(format!(
                    "{endpoint}?index=products&startDate={today}&endDate={today}"
                ))
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "endpoint {endpoint} should return 200"
            );
            let body = body_json(resp).await;
            let dates = body["dates"].as_array().unwrap();

            for entry in dates {
                let date_str = entry["date"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{endpoint}: date field should be string"));
                // Verify YYYY-MM-DD format (10 chars, parseable)
                assert_eq!(
                    date_str.len(),
                    10,
                    "{endpoint}: date '{date_str}' should be YYYY-MM-DD (10 chars)"
                );
                assert!(
                    chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d").is_ok(),
                    "{endpoint}: date '{date_str}' should parse as YYYY-MM-DD"
                );
                all_dates.push((endpoint.to_string(), date_str.to_string()));
            }
        }

        // All endpoints should use the same date string for the same day
        let unique_dates: std::collections::HashSet<&str> =
            all_dates.iter().map(|(_, d)| d.as_str()).collect();
        assert!(
            unique_dates.len() <= 1,
            "all conversion endpoints should use identical date format, got: {:?}",
            all_dates
        );
    }

    // ── B.5: revenue contract strictness ──
    /// TODO: Document revenue_endpoint_currency_map_and_totals_match_contract.
    #[tokio::test]
    async fn revenue_endpoint_currency_map_and_totals_match_contract() {
        let tmp = TempDir::new().unwrap();
        let config = test_analytics_config(&tmp);
        let collector = AnalyticsCollector::new(config.clone());
        let engine = Arc::new(AnalyticsQueryEngine::new(config));
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        let qid_a = "a".repeat(32);
        let qid_b = "b".repeat(32);

        collector.record_search(make_search("q", "products", &qid_a));
        collector.record_search(make_search("q", "products", &qid_b));
        collector.record_insight(make_conversion_with_currency(
            &qid_a, "products", 49.99, "USD",
        ));
        collector.record_insight(make_conversion_with_currency(
            &qid_b, "products", 35.00, "EUR",
        ));
        collector.flush_all();

        let app = Router::new()
            .route("/2/conversions/revenue", get(get_revenue))
            .with_state(engine);

        let req = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/2/conversions/revenue?index=products&startDate={today}&endDate={today}"
            ))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = body_json(resp).await;
        let top = body.as_object().unwrap();

        // Strict top-level fields: only "currencies" and "dates"
        let allowed_top: std::collections::HashSet<&str> =
            ["currencies", "dates"].iter().copied().collect();
        for key in top.keys() {
            assert!(
                allowed_top.contains(key.as_str()),
                "unexpected top-level field in revenue response: '{key}'"
            );
        }

        // currencies is an object (not array)
        let currencies = body["currencies"].as_object().unwrap();
        assert!(currencies.len() >= 2);

        // Each currency entry: exactly { currency, revenue }
        for (code, entry) in currencies {
            let obj = entry.as_object().unwrap();
            assert_eq!(
                obj.len(),
                2,
                "currency entry '{code}' should have exactly 2 fields, got: {obj:?}"
            );
            assert_eq!(
                obj["currency"].as_str().unwrap(),
                code,
                "currency field should match key"
            );
            assert!(
                obj["revenue"].as_f64().is_some(),
                "revenue should be numeric"
            );
        }

        // dates array entries: exactly { date, currencies }
        let dates = body["dates"].as_array().unwrap();
        assert!(!dates.is_empty());
        for entry in dates {
            let obj = entry.as_object().unwrap();
            assert_eq!(
                obj.len(),
                2,
                "dates entry should have exactly 2 fields (date, currencies), got: {obj:?}"
            );
            assert!(obj.contains_key("date"), "dates entry must have 'date'");
            assert!(
                obj.contains_key("currencies"),
                "dates entry must have 'currencies'"
            );
        }
    }

    // ── B.6: no extra top-level fields ──
    /// TODO: Document analytics_endpoints_never_emit_extra_top_level_fields.
    #[tokio::test]
    async fn analytics_endpoints_never_emit_extra_top_level_fields() {
        let tmp = TempDir::new().unwrap();
        let config = test_analytics_config(&tmp);
        let collector = AnalyticsCollector::new(config.clone());
        let engine = Arc::new(AnalyticsQueryEngine::new(config));
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let qid = "a".repeat(32);

        let mut s = make_search("q", "products", &qid);
        s.country = Some("US".to_string());
        collector.record_search(s);
        collector.record_insight(make_conversion(&qid, "products", "addToCart"));
        collector.record_insight(make_conversion(&qid, "products", "purchase"));
        collector.flush_all();

        let app = Router::new()
            .route("/2/countries", get(get_countries))
            .route("/2/conversions/addToCartRate", get(get_add_to_cart_rate))
            .route("/2/conversions/purchaseRate", get(get_purchase_rate))
            .route("/2/conversions/conversionRate", get(get_conversion_rate))
            .with_state(engine);

        // countries: only { countries }
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/2/countries?index=products&startDate={today}&endDate={today}"
            ))
            .body(Body::empty())
            .unwrap();
        let body = body_json(app.clone().oneshot(req).await.unwrap()).await;
        let keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            keys,
            vec!["countries"],
            "countries response must have exactly one top-level field 'countries', got: {keys:?}"
        );

        // addToCartRate: { rate, trackedSearchCount, addToCartCount, dates }
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/2/conversions/addToCartRate?index=products&startDate={today}&endDate={today}"
            ))
            .body(Body::empty())
            .unwrap();
        let body = body_json(app.clone().oneshot(req).await.unwrap()).await;
        let mut keys: Vec<String> = body.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        let mut expected = vec!["addToCartCount", "dates", "rate", "trackedSearchCount"];
        expected.sort();
        assert_eq!(keys, expected, "addToCartRate unexpected fields");

        // purchaseRate: { rate, trackedSearchCount, purchaseCount, dates }
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/2/conversions/purchaseRate?index=products&startDate={today}&endDate={today}"
            ))
            .body(Body::empty())
            .unwrap();
        let body = body_json(app.clone().oneshot(req).await.unwrap()).await;
        let mut keys: Vec<String> = body.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        let mut expected = vec!["dates", "purchaseCount", "rate", "trackedSearchCount"];
        expected.sort();
        assert_eq!(keys, expected, "purchaseRate unexpected fields");

        // conversionRate: { rate, trackedSearchCount, conversionCount, dates }
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!(
                "/2/conversions/conversionRate?index=products&startDate={today}&endDate={today}"
            ))
            .body(Body::empty())
            .unwrap();
        let body = body_json(app.clone().oneshot(req).await.unwrap()).await;
        let mut keys: Vec<String> = body.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        let mut expected = vec!["conversionCount", "dates", "rate", "trackedSearchCount"];
        expected.sort();
        assert_eq!(keys, expected, "conversionRate unexpected fields");
    }
}

mod stage3_rollup_http_parity_tests {
    use super::*;
    use flapjack::analytics::writer::flush_rollup_window;

    const FIXTURE_INDEX: &str = "products";
    const FIXTURE_DATE: &str = "2025-06-15";
    const HOUR_MS: i64 = 3_600_000;
    const DAY_MS: i64 = 86_400_000;

    struct TestAppFixture {
        _tmp: TempDir,
        app: Router,
    }

    fn fixture_day_start_ms() -> i64 {
        chrono::NaiveDate::from_ymd_opt(2025, 6, 15)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    /// TODO: Document fixture_search_event.
    fn fixture_search_event(
        query: &str,
        nb_hits: u32,
        has_results: bool,
        timestamp_ms: i64,
        user_token: &str,
    ) -> SearchEvent {
        SearchEvent {
            timestamp_ms,
            query: query.to_string(),
            query_id: None,
            index_name: FIXTURE_INDEX.to_string(),
            nb_hits,
            processing_time_ms: 5,
            user_token: Some(user_token.to_string()),
            user_ip: None,
            filters: None,
            facets: None,
            analytics_tags: None,
            page: 0,
            hits_per_page: 20,
            has_results,
            country: None,
            region: None,
            experiment_id: None,
            variant_id: None,
            assignment_method: None,
        }
    }

    /// TODO: Document seed_known_answer_daily_dataset.
    fn seed_known_answer_daily_dataset(config: &AnalyticsConfig) {
        let collector = AnalyticsCollector::new(config.clone());
        let base_ts = fixture_day_start_ms() + 12 * HOUR_MS;

        let events = vec![
            fixture_search_event("laptop", 10, true, base_ts, "user-a"),
            fixture_search_event("laptop", 50, true, base_ts + 1_000, "user-b"),
            fixture_search_event("laptop", 60, true, base_ts + 2_000, "user-c"),
            fixture_search_event("phone", 100, true, base_ts + 3_000, "user-a"),
            fixture_search_event("phone", 200, true, base_ts + 4_000, "user-b"),
            fixture_search_event("xyzzy", 0, false, base_ts + 5_000, "user-a"),
            fixture_search_event("zzznothing", 0, false, base_ts + 6_000, "user-d"),
        ];

        for event in events {
            collector.record_search(event);
        }
        collector.flush_all();
    }

    fn flush_daily_rollup(config: &AnalyticsConfig) {
        let day_start = fixture_day_start_ms();
        for offset in 0..24 {
            let window_start = day_start + offset * HOUR_MS;
            let window_end = window_start + HOUR_MS;
            flush_rollup_window(config, FIXTURE_INDEX, "1hour", window_start, window_end)
                .expect("hourly rollup flush");
        }
        flush_rollup_window(config, FIXTURE_INDEX, "1day", day_start, day_start + DAY_MS)
            .expect("daily rollup flush");
    }

    /// TODO: Document build_test_app.
    fn build_test_app(remove_raw_searches: bool) -> TestAppFixture {
        let tmp = TempDir::new().unwrap();
        let config = test_analytics_config(&tmp);
        seed_known_answer_daily_dataset(&config);
        flush_daily_rollup(&config);

        if remove_raw_searches {
            let searches_dir = config.searches_dir(FIXTURE_INDEX);
            if searches_dir.exists() {
                std::fs::remove_dir_all(&searches_dir).unwrap();
            }
        }

        let app = Router::new()
            .route("/2/searches", get(get_top_searches))
            .route("/2/searches/count", get(get_search_count))
            .route("/2/users/count", get(get_users_count))
            .with_state(Arc::new(AnalyticsQueryEngine::new(config)));

        TestAppFixture { _tmp: tmp, app }
    }

    /// TODO: Document request_bytes.
    async fn request_bytes(app: &Router, uri: &str, local_only: bool) -> (StatusCode, Vec<u8>) {
        let mut request = Request::builder().method(Method::GET).uri(uri);
        if local_only {
            request = request.header("X-Flapjack-Local-Only", "1");
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, body)
    }

    fn assert_count_within_two_percent(raw_count: i64, rollup_count: i64) {
        let tolerance = ((raw_count as f64) * 0.02).ceil() as i64;
        let delta = (raw_count - rollup_count).abs();
        assert!(
            delta <= tolerance,
            "rollup count drift {} exceeds allowed tolerance {} (raw={}, rollup={})",
            delta,
            tolerance,
            raw_count,
            rollup_count
        );
    }

    /// TODO: Document rollup_top_searches_and_search_count_payload_parity.
    #[tokio::test]
    async fn rollup_top_searches_and_search_count_payload_parity() {
        let raw_fixture = build_test_app(false);
        let rollup_fixture = build_test_app(true);
        let query =
            format!("?index={FIXTURE_INDEX}&startDate={FIXTURE_DATE}&endDate={FIXTURE_DATE}");

        let (raw_status, raw_searches) =
            request_bytes(&raw_fixture.app, &format!("/2/searches{query}"), false).await;
        let (rollup_status, rollup_searches) =
            request_bytes(&rollup_fixture.app, &format!("/2/searches{query}"), false).await;
        assert_eq!(raw_status, StatusCode::OK);
        assert_eq!(rollup_status, StatusCode::OK);
        assert_eq!(
            rollup_searches, raw_searches,
            "rollup-backed /2/searches response must be byte-equal to raw-backed response"
        );

        let (raw_status, raw_count) = request_bytes(
            &raw_fixture.app,
            &format!("/2/searches/count{query}"),
            false,
        )
        .await;
        let (rollup_status, rollup_count) = request_bytes(
            &rollup_fixture.app,
            &format!("/2/searches/count{query}"),
            false,
        )
        .await;
        assert_eq!(raw_status, StatusCode::OK);
        assert_eq!(rollup_status, StatusCode::OK);
        assert_eq!(
            rollup_count, raw_count,
            "rollup-backed /2/searches/count response must be byte-equal to raw-backed response"
        );
    }

    /// TODO: Document rollup_users_count_contract_parity_and_header_modes.
    #[tokio::test]
    async fn rollup_users_count_contract_parity_and_header_modes() {
        let raw_fixture = build_test_app(false);
        let rollup_fixture = build_test_app(true);
        let uri = format!(
            "/2/users/count?index={FIXTURE_INDEX}&startDate={FIXTURE_DATE}&endDate={FIXTURE_DATE}"
        );

        // Public contract (no local-only header): allow <=2% drift on count, but
        // all other public fields must be identical and internal sketch fields must
        // stay stripped.
        let (raw_status, raw_public_bytes) = request_bytes(&raw_fixture.app, &uri, false).await;
        let (rollup_status, rollup_public_bytes) =
            request_bytes(&rollup_fixture.app, &uri, false).await;
        assert_eq!(raw_status, StatusCode::OK);
        assert_eq!(rollup_status, StatusCode::OK);

        let raw_public: serde_json::Value = serde_json::from_slice(&raw_public_bytes).unwrap();
        let rollup_public: serde_json::Value =
            serde_json::from_slice(&rollup_public_bytes).unwrap();

        let raw_count = raw_public["count"].as_i64().unwrap();
        let rollup_count = rollup_public["count"].as_i64().unwrap();
        assert_count_within_two_percent(raw_count, rollup_count);

        assert!(raw_public.get("hll_sketch").is_none());
        assert!(raw_public.get("daily_sketches").is_none());
        assert!(rollup_public.get("hll_sketch").is_none());
        assert!(rollup_public.get("daily_sketches").is_none());

        let mut raw_without_count = raw_public.clone();
        let mut rollup_without_count = rollup_public.clone();
        raw_without_count.as_object_mut().unwrap().remove("count");
        rollup_without_count
            .as_object_mut()
            .unwrap()
            .remove("count");
        assert_eq!(
            rollup_without_count, raw_without_count,
            "public /2/users/count payload must match aside from allowed count drift"
        );

        // Local-only mode (cluster merge path): sketch fields must remain present
        // and match across raw-backed and rollup-only backends.
        let (raw_status, raw_local_bytes) = request_bytes(&raw_fixture.app, &uri, true).await;
        let (rollup_status, rollup_local_bytes) =
            request_bytes(&rollup_fixture.app, &uri, true).await;
        assert_eq!(raw_status, StatusCode::OK);
        assert_eq!(rollup_status, StatusCode::OK);

        let raw_local: serde_json::Value = serde_json::from_slice(&raw_local_bytes).unwrap();
        let rollup_local: serde_json::Value = serde_json::from_slice(&rollup_local_bytes).unwrap();
        assert!(raw_local.get("hll_sketch").is_some());
        assert!(raw_local.get("daily_sketches").is_some());
        assert!(rollup_local.get("hll_sketch").is_some());
        assert!(rollup_local.get("daily_sketches").is_some());
        assert_eq!(
            rollup_local, raw_local,
            "local-only /2/users/count payload must be identical across raw and rollup backends"
        );
    }
}
