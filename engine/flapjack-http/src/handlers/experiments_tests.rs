use super::*;
use crate::test_helpers::{body_json, send_empty_request, send_json_request};
use axum::{
    http::Method,
    routing::{get, post},
    Router,
};
use flapjack::analytics::schema::SearchEvent;
use flapjack::analytics::AnalyticsQueryEngine;
use flapjack::index::settings::IndexSettings;
use std::path::PathBuf;
use tempfile::TempDir;

/// Construct a test `AppState` with an experiment store and an optional analytics query engine.
///
/// # Arguments
///
/// * `tmp` - Temporary directory used as the data root for `IndexManager` and `ExperimentStore`.
/// * `analytics_engine` - Optional analytics engine; pass `None` to disable analytics in tests.
fn make_experiments_state_with_analytics_engine(
    tmp: &TempDir,
    analytics_engine: Option<Arc<AnalyticsQueryEngine>>,
) -> Arc<AppState> {
    let builder = crate::test_helpers::TestStateBuilder::new(tmp).with_experiments();
    match analytics_engine {
        Some(engine) => builder.with_analytics_engine(engine).build_shared(),
        None => builder.build_shared(),
    }
}

fn make_experiments_state(tmp: &TempDir) -> Arc<AppState> {
    make_experiments_state_with_analytics_engine(tmp, None)
}

/// Write synthetic `SearchEvent` records to the analytics directory for a given index, creating the required directory structure on disk.
///
/// # Arguments
///
/// * `tmp` - Temporary directory root.
/// * `index_name` - Target index name.
/// * `count` - Number of search events to generate.
fn seed_search_analytics_events(tmp: &TempDir, index_name: &str, count: usize) {
    let searches_dir = tmp
        .path()
        .join("analytics")
        .join(index_name)
        .join("searches");
    let now_ms = chrono::Utc::now().timestamp_millis();
    let events: Vec<SearchEvent> = (0..count)
        .map(|i| SearchEvent {
            timestamp_ms: now_ms - (i as i64 % 86_400_000),
            query: "iphone".to_string(),
            query_id: None,
            index_name: index_name.to_string(),
            nb_hits: 42,
            processing_time_ms: 12,
            user_token: None,
            user_ip: None,
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
        })
        .collect();
    flapjack::analytics::writer::flush_search_events(&events, &searches_dir)
        .expect("failed to seed analytics searches");
}

/// Build a test `Router` with all experiment endpoints wired to the given shared state.
///
/// # Arguments
///
/// * `state` - Shared `AppState` backing the handler extractors.
fn app_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/2/abtests", post(create_experiment).get(list_experiments))
        .route("/2/abtests/estimate", post(estimate_ab_test))
        .route(
            "/2/abtests/:id",
            get(get_experiment)
                .put(update_experiment)
                .delete(delete_experiment),
        )
        .route("/2/abtests/:id/start", post(start_experiment))
        .route("/2/abtests/:id/stop", post(stop_experiment))
        .route("/2/abtests/:id/conclude", post(conclude_experiment))
        .route("/2/abtests/:id/results", get(get_experiment_results))
        .with_state(state)
}

fn conclude_experiment_body() -> serde_json::Value {
    serde_json::json!({
        "winner": "variant",
        "reason": "Statistically significant result",
        "controlMetric": 0.12,
        "variantMetric": 0.14,
        "confidence": 0.97,
        "significant": true,
        "promoted": false
    })
}

fn estimate_body_with_variants(variants: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "configuration": {
            "minimumDetectableEffect": {
                "size": 0.05,
                "metric": "clickThroughRate"
            }
        },
        "variants": variants
    })
}

fn assert_error_message_and_status(json: &serde_json::Value, status: StatusCode) {
    assert!(json["message"].as_str().is_some_and(|m| !m.is_empty()));
    assert_eq!(json["status"].as_u64(), Some(status.as_u16() as u64));
    assert_eq!(
        sorted_object_keys(json),
        vec!["message".to_string(), "status".to_string()]
    );
}

/// Build a JSON request body in Algolia AB test format for creating an experiment on the given index.
///
/// # Arguments
///
/// * `index_name` - The primary index name; the variant index is derived by appending `_v2`.
fn create_algolia_experiment_body(index_name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": "Ranking test",
        "variants": [
            {
                "index": index_name,
                "trafficPercentage": 50,
                "description": "control"
            },
            {
                "index": format!("{index_name}_v2"),
                "trafficPercentage": 50,
                "description": "variant"
            }
        ],
        "endAt": "2099-01-01T00:00:00Z",
        "metrics": [{ "name": "clickThroughRate" }]
    })
}

async fn seed_exact_index_collision_fixture(app: &Router) {
    // `products`, `products_staging`, and `products_v2` all share the `products`
    // prefix (and `Products` collides only under case-insensitive matching), so an
    // exact `indexName=products` filter must return exactly one experiment while a
    // `indexPrefix=products` filter returns all three prefix collisions.
    for index_name in [
        "products",
        "Products",
        "products_staging",
        "orders",
        "products_v2",
    ] {
        let response = send_json_request(
            app,
            Method::POST,
            "/2/abtests",
            create_algolia_experiment_body(index_name),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}

fn create_experiment_body_for_index(index_name: &str) -> serde_json::Value {
    create_algolia_experiment_body(index_name)
}

fn create_experiment_body() -> serde_json::Value {
    create_algolia_experiment_body("products")
}

/// Build a JSON request body for updating an experiment targeting the given index name.
///
/// # Arguments
///
/// * `index_name` - The index to reference in the update payload.
fn update_experiment_body_for_index(index_name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": "Ranking test",
        "indexName": index_name,
        "trafficSplit": 0.5,
        "control": {
            "name": "control"
        },
        "variant": {
            "name": "variant",
            "queryOverrides": {
                "enableSynonyms": false
            }
        },
        "primaryMetric": "ctr"
    })
}

fn update_experiment_body() -> serde_json::Value {
    update_experiment_body_for_index("products")
}

/// Send an HTTP request with a JSON body to the test router and return the raw response.
///
/// # Arguments
///
/// * `app` - The test `Router` instance.
/// * `method` - HTTP method.
/// * `uri` - Request URI path.
/// * `body` - JSON payload serialized into the request body.
async fn create_experiment_and_get_id(app: &Router) -> i64 {
    let resp = send_json_request(app, Method::POST, "/2/abtests", create_experiment_body()).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    json["abTestID"].as_i64().unwrap()
}

async fn get_internal_experiment_id(app: &Router, ab_test_id: i64) -> String {
    let resp = send_empty_request(
        app,
        Method::GET,
        &format!("/2/abtests/{ab_test_id}/results"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    json["experimentID"].as_str().unwrap().to_string()
}

async fn create_algolia_experiment_and_get_numeric_id(app: &Router) -> i64 {
    let resp = send_json_request(
        app,
        Method::POST,
        "/2/abtests",
        create_algolia_experiment_body("products"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    json["abTestID"]
        .as_i64()
        .expect("create response must contain integer abTestID")
}

fn sorted_object_keys(json: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = json
        .as_object()
        .expect("expected JSON object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn assert_algolia_action_shape(
    json: &serde_json::Value,
    expected_ab_test_id: i64,
    expected_index: &str,
) {
    assert_eq!(json["abTestID"], expected_ab_test_id);
    assert_eq!(json["taskID"], expected_ab_test_id);
    assert_eq!(json["index"], expected_index);
    assert_eq!(
        sorted_object_keys(json),
        vec!["abTestID", "index", "taskID"]
    );
}

fn assert_algolia_estimate_shape(json: &serde_json::Value) {
    assert_eq!(
        sorted_object_keys(json),
        vec!["durationDays", "sampleSizes"]
    );
    assert!(json["durationDays"].as_i64().is_some());
    let sizes = json["sampleSizes"]
        .as_array()
        .expect("sampleSizes must be an array");
    assert_eq!(sizes.len(), 2);
    for size in sizes {
        assert!(size.as_i64().is_some_and(|n| n > 0));
    }
}

/// Assert that a JSON value conforms to the full Algolia AB test wire schema, validating top-level keys, variant structure, significance fields, configuration block, and optional `stoppedAt` presence.
///
/// # Arguments
///
/// * `json` - The response body to validate.
/// * `expected_status` - The expected value of the `status` field (e.g. `"active"`, `"stopped"`).
/// * `expect_stopped_at` - Whether `stoppedAt` must be present and non-null.
fn assert_algolia_abtest_schema(
    json: &serde_json::Value,
    expected_status: &str,
    expect_stopped_at: bool,
) {
    let mut expected_top_level_keys = vec![
        "abTestID".to_string(),
        "addToCartSignificance".to_string(),
        "clickSignificance".to_string(),
        "configuration".to_string(),
        "conversionSignificance".to_string(),
        "createdAt".to_string(),
        "endAt".to_string(),
        "name".to_string(),
        "purchaseSignificance".to_string(),
        "revenueSignificance".to_string(),
        "status".to_string(),
        "updatedAt".to_string(),
        "variants".to_string(),
    ];
    if expect_stopped_at {
        expected_top_level_keys.push("stoppedAt".to_string());
    }
    expected_top_level_keys.sort();
    assert_eq!(sorted_object_keys(json), expected_top_level_keys);

    assert!(json["abTestID"].as_i64().is_some_and(|id| id > 0));
    assert!(json["name"].as_str().is_some());
    assert_eq!(json["status"], expected_status);
    assert!(json["endAt"].as_str().is_some());
    assert!(json["createdAt"].as_str().is_some());
    assert!(json["updatedAt"].as_str().is_some());
    if expect_stopped_at {
        assert!(json["stoppedAt"].as_str().is_some());
    } else {
        assert!(json.get("stoppedAt").is_none());
    }
    assert!(json["clickSignificance"].is_null());
    assert!(json["conversionSignificance"].is_null());
    assert!(json["addToCartSignificance"].is_null());
    assert!(json["purchaseSignificance"].is_null());
    assert!(json["revenueSignificance"].is_null());

    assert_eq!(
        sorted_object_keys(&json["configuration"]),
        vec!["outliers".to_string()]
    );
    assert!(json["configuration"]["outliers"]["exclude"].is_boolean());

    let variants = json["variants"]
        .as_array()
        .expect("variants must be an array");
    assert_eq!(variants.len(), 2);
    for variant in variants {
        let mut expected_variant_keys = vec![
            "addToCartCount".to_string(),
            "addToCartRate".to_string(),
            "averageClickPosition".to_string(),
            "clickCount".to_string(),
            "clickThroughRate".to_string(),
            "conversionCount".to_string(),
            "conversionRate".to_string(),
            "currencies".to_string(),
            "description".to_string(),
            "estimatedSampleSize".to_string(),
            "filterEffects".to_string(),
            "index".to_string(),
            "noResultCount".to_string(),
            "purchaseCount".to_string(),
            "purchaseRate".to_string(),
            "searchCount".to_string(),
            "trackedSearchCount".to_string(),
            "trackedUserCount".to_string(),
            "trafficPercentage".to_string(),
            "userCount".to_string(),
        ];
        if variant.get("customSearchParameters").is_some() {
            expected_variant_keys.push("customSearchParameters".to_string());
        }
        expected_variant_keys.sort();
        assert_eq!(sorted_object_keys(variant), expected_variant_keys);
    }
}

fn normalize_algolia_abtest_snapshot(mut json: serde_json::Value) -> serde_json::Value {
    json["abTestID"] = serde_json::json!(1);
    json["createdAt"] = serde_json::json!("<RFC3339>");
    json["updatedAt"] = serde_json::json!("<RFC3339>");
    if json.get("stoppedAt").is_some() {
        json["stoppedAt"] = serde_json::json!("<RFC3339>");
    }
    json
}

#[tokio::test]
async fn create_experiment_returns_200() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state.clone());

    let resp = send_json_request(&app, Method::POST, "/2/abtests", create_experiment_body()).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let id = json["abTestID"]
        .as_i64()
        .expect("create returns numeric abTestID");
    assert_algolia_action_shape(&json, id, "products");
}

#[tokio::test]
async fn create_experiment_invalid_traffic_split_returns_400() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state.clone());

    let mut body = create_experiment_body();
    body["variants"] = serde_json::json!([
        { "index": "products", "trafficPercentage": 0, "description": "control" },
        { "index": "products_v2", "trafficPercentage": 100, "description": "variant" }
    ]);

    let resp = send_json_request(&app, Method::POST, "/2/abtests", body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_experiment_missing_variant_config_returns_400() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let mut body = create_experiment_body();
    body["variants"] = serde_json::json!([
        { "index": "products", "trafficPercentage": 100, "description": "control" }
    ]);

    let resp = send_json_request(&app, Method::POST, "/2/abtests", body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_experiment_returns_200() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;

    let resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["abTestID"], id);
    assert_algolia_abtest_schema(&json, "active", false);
}

#[tokio::test]
async fn get_nonexistent_experiment_returns_404() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let resp = send_empty_request(&app, Method::GET, "/2/abtests/nope").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_experiments_empty_returns_null_abtests() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let resp = send_empty_request(&app, Method::GET, "/2/abtests").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(sorted_object_keys(&json), vec!["abtests", "count", "total"]);
    assert!(json["abtests"].is_null(), "abtests must be null when empty");
    assert_eq!(json["count"], 0);
    assert_eq!(json["total"], 0);
}

/// Verify that the estimate endpoint rejects requests with more or fewer than exactly two variants, returning 400 Bad Request.
#[tokio::test]
async fn estimate_requires_exactly_two_variants() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let body = estimate_body_with_variants(serde_json::json!([
        { "index": "products", "trafficPercentage": 40 },
        { "index": "products_v2", "trafficPercentage": 40 },
        { "index": "products_v3", "trafficPercentage": 20 }
    ]));

    let resp = send_json_request(&app, Method::POST, "/2/abtests/estimate", body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert_error_message_and_status(&json, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn estimate_rejects_traffic_percentages_outside_1_to_99() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let body = estimate_body_with_variants(serde_json::json!([
        { "index": "products", "trafficPercentage": 0 },
        { "index": "products_v2", "trafficPercentage": 100 }
    ]));

    let resp = send_json_request(&app, Method::POST, "/2/abtests/estimate", body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert_error_message_and_status(&json, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn estimate_requires_traffic_percentages_to_sum_to_100() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let body = estimate_body_with_variants(serde_json::json!([
        { "index": "products", "trafficPercentage": 70 },
        { "index": "products_v2", "trafficPercentage": 20 }
    ]));

    let resp = send_json_request(&app, Method::POST, "/2/abtests/estimate", body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert_error_message_and_status(&json, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn estimate_valid_request_returns_duration_and_sample_sizes() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let body = estimate_body_with_variants(serde_json::json!([
        { "index": "products", "trafficPercentage": 50 },
        { "index": "products_v2", "trafficPercentage": 50 }
    ]));

    let resp = send_json_request(&app, Method::POST, "/2/abtests/estimate", body).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_algolia_estimate_shape(&json);
}

/// Verify that the estimate endpoint uses historical analytics search volume to compute realistic duration and sample-size projections.
#[tokio::test]
async fn estimate_uses_historical_analytics_traffic_for_duration() {
    let tmp = TempDir::new().unwrap();
    seed_search_analytics_events(&tmp, "products", 6_000);
    let analytics_dir = tmp.path().join("analytics");
    let state = make_experiments_state_with_analytics(&tmp, &analytics_dir);
    let app = app_router(state);

    let body = estimate_body_with_variants(serde_json::json!([
        { "index": "products", "trafficPercentage": 50 },
        { "index": "products_v2", "trafficPercentage": 50 }
    ]));

    let resp = send_json_request(&app, Method::POST, "/2/abtests/estimate", body).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_algolia_estimate_shape(&json);
    assert_eq!(json["sampleSizes"], serde_json::json!([3136, 3136]));
    assert_eq!(json["durationDays"], 16);
}

#[tokio::test]
async fn list_experiments_filters_by_index_prefix_and_suffix() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let first = send_json_request(
        &app,
        Method::POST,
        "/2/abtests",
        create_algolia_experiment_body("prod_alpha_v1"),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = send_json_request(
        &app,
        Method::POST,
        "/2/abtests",
        create_algolia_experiment_body("prod_beta_v2"),
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);

    let resp = send_empty_request(
        &app,
        Method::GET,
        "/2/abtests?indexPrefix=prod_&indexSuffix=_v2",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["count"], 1);
    assert_eq!(json["total"], 1);
    assert_eq!(json["abtests"][0]["variants"][0]["index"], "prod_beta_v2");
}

#[tokio::test]
async fn algolia_lifecycle_create_get_stop_get_matches_wire_schema() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let create_resp = send_json_request(
        &app,
        Method::POST,
        "/2/abtests",
        create_algolia_experiment_body("products"),
    )
    .await;
    assert_eq!(create_resp.status(), StatusCode::OK);
    let create_json = body_json(create_resp).await;
    let ab_test_id = create_json["abTestID"]
        .as_i64()
        .expect("create response must include abTestID");
    assert_algolia_action_shape(&create_json, ab_test_id, "products");

    let get_active_resp =
        send_empty_request(&app, Method::GET, &format!("/2/abtests/{ab_test_id}")).await;
    assert_eq!(get_active_resp.status(), StatusCode::OK);
    let get_active_json = body_json(get_active_resp).await;
    assert_algolia_abtest_schema(&get_active_json, "active", false);
    assert_eq!(get_active_json["abTestID"], ab_test_id);

    let stop_resp =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{ab_test_id}/stop")).await;
    assert_eq!(stop_resp.status(), StatusCode::OK);
    let stop_json = body_json(stop_resp).await;
    assert_algolia_action_shape(&stop_json, ab_test_id, "products");

    let get_stopped_resp =
        send_empty_request(&app, Method::GET, &format!("/2/abtests/{ab_test_id}")).await;
    assert_eq!(get_stopped_resp.status(), StatusCode::OK);
    let get_stopped_json = body_json(get_stopped_resp).await;
    assert_algolia_abtest_schema(&get_stopped_json, "stopped", true);
    assert_eq!(get_stopped_json["abTestID"], ab_test_id);
}

#[tokio::test]
async fn stop_preserves_scheduled_end_at_and_sets_distinct_stopped_at() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let create_resp = send_json_request(
        &app,
        Method::POST,
        "/2/abtests",
        create_algolia_experiment_body("products"),
    )
    .await;
    assert_eq!(create_resp.status(), StatusCode::OK);
    let create_json = body_json(create_resp).await;
    let ab_test_id = create_json["abTestID"]
        .as_i64()
        .expect("create response must include abTestID");

    let start_resp = send_empty_request(
        &app,
        Method::POST,
        &format!("/2/abtests/{ab_test_id}/start"),
    )
    .await;
    assert_eq!(start_resp.status(), StatusCode::OK);

    let stop_resp =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{ab_test_id}/stop")).await;
    assert_eq!(stop_resp.status(), StatusCode::OK);

    let get_resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{ab_test_id}")).await;
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_json = body_json(get_resp).await;
    assert_eq!(get_json["status"], "stopped");
    assert_eq!(get_json["endAt"], "2099-01-01T00:00:00Z");
    assert!(get_json["stoppedAt"].is_string());
    assert_ne!(get_json["stoppedAt"], get_json["endAt"]);
}

#[tokio::test]
async fn stopped_experiment_releases_active_slot_for_same_index() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    // Create and start experiment A on "products"
    let create_a = send_json_request(
        &app,
        Method::POST,
        "/2/abtests",
        create_algolia_experiment_body("products"),
    )
    .await;
    assert_eq!(create_a.status(), StatusCode::OK);
    let id_a = body_json(create_a).await["abTestID"].as_i64().unwrap();

    let start_a = send_empty_request(&app, Method::POST, &format!("/2/abtests/{id_a}/start")).await;
    assert_eq!(start_a.status(), StatusCode::OK);

    // Stop experiment A — should release the active slot
    let stop_a = send_empty_request(&app, Method::POST, &format!("/2/abtests/{id_a}/stop")).await;
    assert_eq!(stop_a.status(), StatusCode::OK);

    // Create and start experiment B on the same index — must succeed, not 409
    let create_b = send_json_request(
        &app,
        Method::POST,
        "/2/abtests",
        create_algolia_experiment_body("products"),
    )
    .await;
    assert_eq!(create_b.status(), StatusCode::OK);
    let id_b = body_json(create_b).await["abTestID"].as_i64().unwrap();

    let start_b = send_empty_request(&app, Method::POST, &format!("/2/abtests/{id_b}/start")).await;
    assert_eq!(start_b.status(), StatusCode::OK);

    // Verify B is active
    let get_b = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id_b}")).await;
    assert_eq!(get_b.status(), StatusCode::OK);
    let get_b_json = body_json(get_b).await;
    assert_eq!(get_b_json["status"], "active");

    // Verify A is still stopped
    let get_a = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id_a}")).await;
    assert_eq!(get_a.status(), StatusCode::OK);
    let get_a_json = body_json(get_a).await;
    assert_eq!(get_a_json["status"], "stopped");
}

/// Verify that the GET abtest response, after timestamp normalization, matches the expected Algolia wire-format payload exactly.
#[tokio::test]
async fn algolia_get_abtest_snapshot_matches_expected_payload() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let ab_test_id = create_algolia_experiment_and_get_numeric_id(&app).await;
    let get_resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{ab_test_id}")).await;
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_json = body_json(get_resp).await;
    let normalized = normalize_algolia_abtest_snapshot(get_json);

    let expected = serde_json::json!({
        "abTestID": 1,
        "name": "Ranking test",
        "status": "active",
        "endAt": "2099-01-01T00:00:00Z",
        "createdAt": "<RFC3339>",
        "updatedAt": "<RFC3339>",
        "variants": [
            {
                "index": "products",
                "trafficPercentage": 50,
                "description": "control",
                "addToCartCount": null,
                "addToCartRate": null,
                "averageClickPosition": null,
                "clickCount": null,
                "clickThroughRate": null,
                "conversionCount": null,
                "conversionRate": null,
                "currencies": {},
                "estimatedSampleSize": 0,
                "filterEffects": null,
                "noResultCount": null,
                "purchaseCount": null,
                "purchaseRate": null,
                "searchCount": null,
                "trackedSearchCount": null,
                "userCount": null,
                "trackedUserCount": null
            },
            {
                "index": "products_v2",
                "trafficPercentage": 50,
                "description": "variant",
                "addToCartCount": null,
                "addToCartRate": null,
                "averageClickPosition": null,
                "clickCount": null,
                "clickThroughRate": null,
                "conversionCount": null,
                "conversionRate": null,
                "currencies": {},
                "estimatedSampleSize": 0,
                "filterEffects": null,
                "noResultCount": null,
                "purchaseCount": null,
                "purchaseRate": null,
                "searchCount": null,
                "trackedSearchCount": null,
                "userCount": null,
                "trackedUserCount": null
            }
        ],
        "configuration": {
            "outliers": {
                "exclude": false
            }
        },
        "clickSignificance": null,
        "conversionSignificance": null,
        "addToCartSignificance": null,
        "purchaseSignificance": null,
        "revenueSignificance": null
    });
    assert_eq!(normalized, expected);
}

/// Verify that PUT on a draft experiment returns 200 with the updated fields reflected in the Algolia abtest schema response.
#[tokio::test]
async fn update_draft_experiment_returns_200() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let mut body = update_experiment_body();
    body["name"] = serde_json::json!("Updated name");

    let resp = send_json_request(&app, Method::PUT, &format!("/2/abtests/{id}"), body).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["name"], "Updated name");
    assert_eq!(json["abTestID"], id);
    assert_algolia_abtest_schema(&json, "active", false);
}

/// Verify that updating a draft experiment advances the `updatedAt` timestamp beyond its previous value.
#[tokio::test]
async fn update_draft_experiment_refreshes_updated_at() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;

    let before_resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}")).await;
    assert_eq!(before_resp.status(), StatusCode::OK);
    let before_json = body_json(before_resp).await;
    let before_updated_at = chrono::DateTime::parse_from_rfc3339(
        before_json["updatedAt"]
            .as_str()
            .expect("updatedAt must be present before update"),
    )
    .expect("updatedAt before update must be RFC3339")
    .timestamp_millis();

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let mut body = update_experiment_body();
    body["name"] = serde_json::json!("Updated name");
    let update_resp = send_json_request(&app, Method::PUT, &format!("/2/abtests/{id}"), body).await;
    assert_eq!(update_resp.status(), StatusCode::OK);

    let after_resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}")).await;
    assert_eq!(after_resp.status(), StatusCode::OK);
    let after_json = body_json(after_resp).await;
    let after_updated_at = chrono::DateTime::parse_from_rfc3339(
        after_json["updatedAt"]
            .as_str()
            .expect("updatedAt must be present after update"),
    )
    .expect("updatedAt after update must be RFC3339")
    .timestamp_millis();

    assert!(
        after_updated_at > before_updated_at,
        "updatedAt should advance after update; before={before_updated_at}, after={after_updated_at}"
    );
}

/// Verify that attempting to update a running experiment returns 409 Conflict.
#[tokio::test]
async fn update_running_experiment_returns_409() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let start_resp =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(start_resp.status(), StatusCode::OK);

    let mut body = update_experiment_body();
    body["name"] = serde_json::json!("Updated name");

    let resp = send_json_request(&app, Method::PUT, &format!("/2/abtests/{id}"), body).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

/// Verify that deleting a draft experiment returns 200 with the standard Algolia action shape (`abTestID`, `taskID`, `index`) and that a subsequent GET returns 404.
#[tokio::test]
async fn delete_draft_experiment_returns_200_action_shape() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;

    let delete_resp = send_empty_request(&app, Method::DELETE, &format!("/2/abtests/{id}")).await;
    assert_eq!(delete_resp.status(), StatusCode::OK);
    let delete_json = body_json(delete_resp).await;
    assert_algolia_action_shape(&delete_json, id, "products");

    let get_resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}")).await;
    assert_eq!(get_resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_running_experiment_returns_409() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;

    let start_resp =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(start_resp.status(), StatusCode::OK);

    let delete_resp = send_empty_request(&app, Method::DELETE, &format!("/2/abtests/{id}")).await;
    assert_eq!(delete_resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn start_experiment_returns_200() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let resp = send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_algolia_action_shape(&json, id, "products");
    let get_resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}")).await;
    let get_json = body_json(get_resp).await;
    assert_eq!(get_json["status"], "active");
}

/// Verify that concluding a running experiment returns 200, sets status to `concluded`, persists all conclusion fields, and round-trips them through the results endpoint.
#[tokio::test]
async fn conclude_experiment_returns_200_and_sets_conclusion() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let start_resp =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(start_resp.status(), StatusCode::OK);

    let conclude_resp = send_json_request(
        &app,
        Method::POST,
        &format!("/2/abtests/{id}/conclude"),
        conclude_experiment_body(),
    )
    .await;
    assert_eq!(conclude_resp.status(), StatusCode::OK);
    let cj = body_json(conclude_resp).await;
    assert_eq!(cj["status"], "concluded");
    assert!(cj["stoppedAt"].as_i64().is_some(), "stoppedAt must be set");
    // Verify all conclusion fields round-trip through HTTP
    let c = &cj["conclusion"];
    assert_eq!(c["winner"], "variant");
    assert_eq!(c["reason"], "Statistically significant result");
    assert_eq!(c["controlMetric"], 0.12);
    assert_eq!(c["variantMetric"], 0.14);
    assert_eq!(c["confidence"], 0.97);
    assert_eq!(c["significant"], true);
    assert_eq!(c["promoted"], false);

    // Verify persistence via /results (internal status fields).
    let get_resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}/results")).await;
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_json = body_json(get_resp).await;
    assert_eq!(get_json["status"], "concluded");
    assert_eq!(get_json["conclusion"]["winner"], "variant");
    assert_eq!(get_json["conclusion"]["controlMetric"], 0.12);
}

/// Verify that concluding without specifying a winner (inconclusive result) returns 200 with a null winner and `significant: false`.
#[tokio::test]
async fn conclude_experiment_without_winner_returns_200() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;

    let body = serde_json::json!({
        "reason": "Inconclusive — not enough data",
        "controlMetric": 0.10,
        "variantMetric": 0.11,
        "confidence": 0.60,
        "significant": false,
        "promoted": false
    });

    let resp = send_json_request(
        &app,
        Method::POST,
        &format!("/2/abtests/{id}/conclude"),
        body,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "concluded");
    assert!(
        json["conclusion"]["winner"].is_null(),
        "winner should be null for inconclusive conclusion"
    );
    assert_eq!(json["conclusion"]["significant"], false);
}

/// Verify that concluding with an unrecognized winner value (not `"control"` or `"variant"`) returns 400 Bad Request.
#[tokio::test]
async fn conclude_experiment_invalid_winner_returns_400() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let start_resp =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(start_resp.status(), StatusCode::OK);

    let mut body = conclude_experiment_body();
    body["winner"] = serde_json::json!("bogus");

    let conclude_resp = send_json_request(
        &app,
        Method::POST,
        &format!("/2/abtests/{id}/conclude"),
        body,
    )
    .await;
    assert_eq!(conclude_resp.status(), StatusCode::BAD_REQUEST);
}

/// Verify that attempting to conclude an already-concluded experiment returns 409 Conflict.
#[tokio::test]
async fn conclude_already_concluded_returns_409() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;

    // First conclude succeeds
    let resp1 = send_json_request(
        &app,
        Method::POST,
        &format!("/2/abtests/{id}/conclude"),
        conclude_experiment_body(),
    )
    .await;
    assert_eq!(resp1.status(), StatusCode::OK);

    // Second conclude must fail
    let resp2 = send_json_request(
        &app,
        Method::POST,
        &format!("/2/abtests/{id}/conclude"),
        conclude_experiment_body(),
    )
    .await;
    assert_eq!(resp2.status(), StatusCode::CONFLICT);
}

/// Verify that a stopped (but not yet concluded) experiment can be concluded successfully, returning 200 with `status: "concluded"`.
#[tokio::test]
async fn conclude_stopped_experiment_returns_200() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/stop")).await;

    let body = serde_json::json!({
        "reason": "Inconclusive after stopping",
        "controlMetric": 0.10,
        "variantMetric": 0.11,
        "confidence": 0.60,
        "significant": false,
        "promoted": false
    });
    let resp = send_json_request(
        &app,
        Method::POST,
        &format!("/2/abtests/{id}/conclude"),
        body,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "concluded");
    assert!(json["conclusion"]["winner"].is_null());
    assert!(json["stoppedAt"].as_i64().is_some());
}

#[tokio::test]
async fn conclude_draft_experiment_returns_409() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let resp = send_json_request(
        &app,
        Method::POST,
        &format!("/2/abtests/{id}/conclude"),
        conclude_experiment_body(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn create_experiment_default_minimum_days() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state.clone());

    let mut body = create_experiment_body();
    body.as_object_mut().unwrap().remove("metrics");

    let resp = send_json_request(&app, Method::POST, "/2/abtests", body).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let create_json = body_json(resp).await;
    let id = create_json["abTestID"].as_i64().unwrap();
    let store = state.experiment_store.as_ref().unwrap();
    let uuid = store.get_uuid_for_numeric(id).unwrap();
    let experiment = store.get(&uuid).unwrap();
    assert_eq!(
        experiment.minimum_days, DEFAULT_MINIMUM_DAYS,
        "omitted minimumDays should default to {DEFAULT_MINIMUM_DAYS}"
    );
}

/// Verify that the results endpoint for a draft experiment returns the complete response structure with zero-valued metrics, a closed gate, null significance, and correct metadata fields.
#[tokio::test]
async fn results_draft_experiment_returns_full_response_structure() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}/results")).await;

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;

    // Core experiment fields
    assert!(json["experimentID"].as_str().is_some());
    assert_eq!(json["name"], "Ranking test");
    assert_eq!(json["status"], "draft");
    assert_eq!(json["indexName"], "products");
    assert_eq!(json["trafficSplit"], 0.5);
    assert_eq!(json["primaryMetric"], "ctr");

    // Gate should exist with readyToRead=false (draft, no data)
    assert!(json["gate"].is_object());
    assert_eq!(json["gate"]["readyToRead"], false);
    assert_eq!(json["gate"]["minimumNReached"], false);

    // Arms should be empty
    assert_eq!(json["control"]["searches"], 0);
    assert_eq!(json["variant"]["searches"], 0);

    // Significance should be null when gate not ready
    assert!(json["significance"].is_null());

    // SRM defaults to false
    assert_eq!(json["sampleRatioMismatch"], false);
}

/// Verify that the results endpoint serializes all field names in camelCase, including nested gate and arm-level fields.
#[tokio::test]
async fn results_response_has_camel_case_fields() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}/results")).await;
    let json = body_json(resp).await;

    // Verify camelCase serialization for key fields
    assert!(json.get("experimentID").is_some());
    assert!(json.get("indexName").is_some());
    assert!(json.get("endedAt").is_some());
    assert!(json.get("conclusion").is_some());
    assert!(json.get("trafficSplit").is_some());
    assert!(json.get("primaryMetric").is_some());
    assert!(json.get("sampleRatioMismatch").is_some());
    assert!(json.get("outlierUsersExcluded").is_some());
    assert!(json.get("noStableIdQueries").is_some());
    assert!(json["gate"].get("readyToRead").is_some());
    assert!(json["gate"].get("minimumNReached").is_some());
    assert!(json["gate"].get("minimumDaysReached").is_some());
    assert!(json["gate"].get("requiredSearchesPerArm").is_some());
    assert!(json["gate"].get("currentSearchesPerArm").is_some());
    assert!(json["gate"].get("progressPct").is_some());
    assert!(json["gate"].get("estimatedDaysRemaining").is_some());
    assert!(json["control"].get("zeroResultRate").is_some());
    assert!(json["control"].get("conversionRate").is_some());
    assert!(json["control"].get("revenuePerSearch").is_some());
    assert!(json["control"].get("abandonmentRate").is_some());
}

/// Verify that results for a running experiment include a non-null RFC 3339 `startDate` string.
#[tokio::test]
async fn results_running_experiment_shows_start_date() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;

    let resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}/results")).await;
    let json = body_json(resp).await;

    assert_eq!(json["status"], "running");
    assert!(
        json["startDate"].is_string(),
        "startDate should be an RFC3339 string"
    );
}

/// Verify that results for a concluded experiment include a populated `conclusion` object with all round-tripped fields and a non-null RFC 3339 `endedAt` timestamp.
#[tokio::test]
async fn results_concluded_experiment_includes_conclusion_and_ended_date() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let start_resp =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(start_resp.status(), StatusCode::OK);
    let conclude_resp = send_json_request(
        &app,
        Method::POST,
        &format!("/2/abtests/{id}/conclude"),
        conclude_experiment_body(),
    )
    .await;
    assert_eq!(conclude_resp.status(), StatusCode::OK);

    let resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}/results")).await;
    let json = body_json(resp).await;

    assert_eq!(json["status"], "concluded");
    assert!(
        json["endedAt"].is_string(),
        "endedAt should be an RFC3339 string when concluded"
    );
    assert_eq!(json["conclusion"]["winner"], "variant");
    assert_eq!(
        json["conclusion"]["reason"],
        "Statistically significant result"
    );
    assert_eq!(json["conclusion"]["controlMetric"], 0.12);
    assert_eq!(json["conclusion"]["variantMetric"], 0.14);
    assert_eq!(json["conclusion"]["confidence"], 0.97);
    assert_eq!(json["conclusion"]["significant"], true);
    assert_eq!(json["conclusion"]["promoted"], false);
}

/// Verify that results return zero-valued metrics for both arms and zero outlier/no-stable-id counters when no analytics engine is configured.
#[tokio::test]
async fn results_zero_metrics_when_no_analytics_engine() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}/results")).await;
    let json = body_json(resp).await;

    assert_eq!(json["control"]["searches"], 0);
    assert_eq!(json["control"]["users"], 0);
    assert_eq!(json["control"]["ctr"], 0.0);
    assert_eq!(json["variant"]["searches"], 0);
    assert_eq!(json["variant"]["users"], 0);
    assert_eq!(json["variant"]["ctr"], 0.0);
    assert_eq!(json["outlierUsersExcluded"], 0);
    assert_eq!(json["noStableIdQueries"], 0);
}

#[tokio::test]
async fn results_gate_progress_fields_present() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}/results")).await;
    let json = body_json(resp).await;

    let gate = &json["gate"];
    assert!(gate["requiredSearchesPerArm"].as_u64().is_some());
    assert_eq!(gate["currentSearchesPerArm"], 0);
    assert_eq!(gate["progressPct"], 0.0);
}

/// Build an AppState with a real analytics engine pointing at the given data dir.
fn make_experiments_state_with_analytics(
    tmp: &TempDir,
    analytics_dir: &std::path::Path,
) -> Arc<AppState> {
    let config = flapjack::analytics::config::AnalyticsConfig {
        enabled: true,
        data_dir: analytics_dir.to_path_buf(),
        flush_interval_secs: 3600,
        flush_size: 100_000,
        retention_days: 90,
    };
    crate::test_helpers::TestStateBuilder::new(tmp)
        .with_experiments()
        .with_analytics_engine(Arc::new(flapjack::analytics::AnalyticsQueryEngine::new(
            config,
        )))
        .build_shared()
}

/// Verify that results reflect real search and click metrics from seeded analytics events, with correct per-arm counts and non-zero CTR values.
#[tokio::test]
async fn results_with_seeded_analytics_returns_real_metrics() {
    use flapjack::analytics::schema::{InsightEvent, SearchEvent};
    use flapjack::analytics::writer;

    let tmp = TempDir::new().unwrap();
    let analytics_dir = tmp.path().join("analytics");

    let state = make_experiments_state_with_analytics(&tmp, &analytics_dir);
    let app = app_router(state.clone());

    // Create and start an experiment
    let id = create_experiment_and_get_id(&app).await;
    let start_resp =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(start_resp.status(), StatusCode::OK);
    let internal_id = get_internal_experiment_id(&app, id).await;

    // Seed search events for the experiment
    let mut search_events = Vec::new();
    let mut click_events = Vec::new();

    for i in 0..20u32 {
        let variant = if i < 10 { "control" } else { "variant" };
        let user = format!("user_{}", i % 4);
        let qid = format!("qid_{}", i);
        search_events.push(SearchEvent {
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            query: "test".to_string(),
            query_id: Some(qid.clone()),
            index_name: "products".to_string(),
            nb_hits: 5,
            processing_time_ms: 3,
            user_token: Some(user.clone()),
            user_ip: None,
            filters: None,
            facets: None,
            analytics_tags: None,
            page: 0,
            hits_per_page: 20,
            has_results: true,
            country: None,
            region: None,
            experiment_id: Some(internal_id.clone()),
            variant_id: Some(variant.to_string()),
            assignment_method: Some("user_token".to_string()),
        });

        // Give some clicks (6 for control arm qids 0-5, 8 for variant arm qids 10-17)
        if i < 6 || (10..18).contains(&i) {
            click_events.push(InsightEvent {
                event_type: "click".to_string(),
                event_subtype: None,
                event_name: "Click".to_string(),
                index: "products".to_string(),
                user_token: user.clone(),
                authenticated_user_token: None,
                query_id: Some(qid),
                object_ids: vec!["obj1".to_string()],
                object_ids_alt: vec![],
                positions: Some(vec![1]),
                timestamp: Some(chrono::Utc::now().timestamp_millis()),
                value: None,
                currency: None,
                interleaving_team: None,
            });
        }
    }

    let searches_dir = analytics_dir.join("products").join("searches");
    let events_dir = analytics_dir.join("products").join("events");
    writer::flush_search_events(&search_events, &searches_dir).unwrap();
    writer::flush_insight_events(&click_events, &events_dir).unwrap();

    // Fetch results
    let resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}/results")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;

    // Verify real metrics came through
    assert_eq!(json["control"]["searches"], 10);
    assert_eq!(json["variant"]["searches"], 10);
    assert_eq!(json["control"]["clicks"].as_u64().unwrap(), 6);
    assert_eq!(json["variant"]["clicks"].as_u64().unwrap(), 8);
    assert!(json["control"]["ctr"].as_f64().unwrap() > 0.0);
    assert!(json["variant"]["ctr"].as_f64().unwrap() > 0.0);

    // Gate should not be ready (not enough data)
    assert_eq!(json["gate"]["readyToRead"], false);
    // Significance should be null since gate not ready
    assert!(json["significance"].is_null());
}

/// Verify that all experiment endpoints return 503 Service Unavailable with an error message when `experiment_store` is `None`.
#[tokio::test]
async fn experiment_store_unavailable_returns_503() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let app = app_router(state);

    let resp = send_empty_request(&app, Method::GET, "/2/abtests").await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let json = body_json(resp).await;
    assert_error_message_and_status(&json, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json["message"], "experiment store unavailable");
}

#[tokio::test]
async fn results_store_unavailable_returns_503() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let app = app_router(state);

    let resp = send_empty_request(&app, Method::GET, "/2/abtests/any-id/results").await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let json = body_json(resp).await;
    assert_error_message_and_status(&json, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json["message"], "experiment store unavailable");
}

#[test]
fn results_handler_uses_shared_store_and_id_resolver_seam() {
    let source = include_str!("experiments/results.rs");
    let handler_start = source
        .find("pub async fn get_experiment_results")
        .expect("results handler function must exist");
    let handler_end = source[handler_start..]
        .find("fn resolve_analytics_data_dir")
        .map(|offset| handler_start + offset)
        .expect("results handler helper boundary must exist");
    let handler_source = &source[handler_start..handler_end];

    assert!(
        handler_source.contains("resolve_store_and_experiment_id(&state, &id)"),
        "results handler must use the shared store/id resolver seam"
    );
    assert!(
        !handler_source.contains("require_experiment_store(&state)"),
        "results handler should not call require_experiment_store directly"
    );
    assert!(
        !handler_source.contains("resolve_experiment_id(store, &id)"),
        "results handler should not call resolve_experiment_id directly"
    );
}

#[tokio::test]
async fn results_numeric_id_resolves_to_experiment() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    // Hold a store reference to resolve UUID independently of the endpoint under test.
    let store = state.experiment_store.as_ref().unwrap().clone();
    let app = app_router(state);

    let ab_test_id = create_experiment_and_get_id(&app).await;
    let experiment_uuid = store
        .get_uuid_for_numeric(ab_test_id)
        .expect("store must map numeric ID to UUID after creation");

    let resp = send_empty_request(
        &app,
        Method::GET,
        &format!("/2/abtests/{ab_test_id}/results"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["experimentID"], experiment_uuid);
}

#[tokio::test]
async fn results_uuid_resolves_to_experiment() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    // Hold a store reference to resolve UUID independently of the endpoint under test.
    let store = state.experiment_store.as_ref().unwrap().clone();
    let app = app_router(state);

    let ab_test_id = create_experiment_and_get_id(&app).await;
    let experiment_uuid = store
        .get_uuid_for_numeric(ab_test_id)
        .expect("store must map numeric ID to UUID after creation");

    let resp = send_empty_request(
        &app,
        Method::GET,
        &format!("/2/abtests/{experiment_uuid}/results"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["experimentID"], experiment_uuid);
}

#[tokio::test]
async fn start_nonexistent_experiment_returns_404() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let resp = send_empty_request(&app, Method::POST, "/2/abtests/nonexistent/start").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stop_nonexistent_experiment_returns_404() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let resp = send_empty_request(&app, Method::POST, "/2/abtests/nonexistent/stop").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stop_draft_experiment_returns_200() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let resp = send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/stop")).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn update_nonexistent_experiment_returns_404() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let body = update_experiment_body();
    let resp = send_json_request(&app, Method::PUT, "/2/abtests/nonexistent", body).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_nonexistent_experiment_returns_404() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let resp = send_empty_request(&app, Method::DELETE, "/2/abtests/nonexistent").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_experiments_default_limit_is_ten() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    for i in 0..12 {
        let mut body = create_experiment_body();
        body["name"] = serde_json::json!(format!("Experiment {i}"));
        let resp = send_json_request(&app, Method::POST, "/2/abtests", body).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    let resp = send_empty_request(&app, Method::GET, "/2/abtests").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["count"], 10);
    assert_eq!(json["total"], 12);
    assert_eq!(
        json["abtests"].as_array().map_or(0, Vec::len),
        10,
        "default list limit must be 10"
    );
}

/// C2: Ordering — list returns experiments sorted by created_at ascending,
/// with numeric ID ascending as a stable tiebreaker for equal timestamps.
#[tokio::test]
async fn list_experiments_ordered_by_creation_then_id() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state.clone());
    let store = state.experiment_store.as_ref().unwrap().clone();

    // Seed experiments where created_at order intentionally differs from numeric ID order.
    // This prevents false positives from implementations that sort only by numeric ID.
    let fixtures = [
        ("alpha", 3000_i64),
        ("beta", 1000_i64),
        ("charlie", 2000_i64),
    ];
    let mut expected_order: Vec<(i64, i64, &str)> = Vec::new();
    for (idx, created_at) in fixtures {
        let req: AlgoliaCreateAbTestRequest =
            serde_json::from_value(create_algolia_experiment_body(idx)).unwrap();
        let mut experiment = dto_algolia::algolia_create_to_experiment(&req).unwrap();
        experiment.created_at = created_at;
        let created = store.create(experiment).unwrap();
        let numeric_id = store
            .get_numeric_id(&created.id)
            .expect("numeric ID must exist for created experiment");
        expected_order.push((created_at, numeric_id, idx));
    }
    expected_order
        .sort_by(|(a_ts, a_id, _), (b_ts, b_id, _)| a_ts.cmp(b_ts).then_with(|| a_id.cmp(b_id)));

    let expected_ids: Vec<i64> = expected_order.iter().map(|(_, id, _)| *id).collect();
    let expected_indices: Vec<&str> = expected_order.iter().map(|(_, _, idx)| *idx).collect();

    let resp = send_empty_request(&app, Method::GET, "/2/abtests").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let abtests = json["abtests"].as_array().expect("abtests must be array");
    assert_eq!(abtests.len(), fixtures.len());

    // Must follow created_at ordering first, with numeric ID tie-breaks.
    let returned_ids: Vec<i64> = abtests
        .iter()
        .map(|t| t["abTestID"].as_i64().unwrap())
        .collect();
    assert_eq!(
        returned_ids, expected_ids,
        "list must return experiments in stable ascending order by creation"
    );

    // Verify index names match the creation order
    let returned_indices: Vec<&str> = abtests
        .iter()
        .map(|t| t["variants"][0]["index"].as_str().unwrap())
        .collect();
    assert_eq!(
        returned_indices, expected_indices,
        "experiments must be in creation order"
    );
}

#[tokio::test]
async fn list_experiments_pagination_slices_ordered_list() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    // Create 5 experiments with distinct names
    let names = ["Exp_A", "Exp_B", "Exp_C", "Exp_D", "Exp_E"];
    let mut ids = Vec::new();
    for name in &names {
        let mut body = create_algolia_experiment_body("products");
        body["name"] = serde_json::json!(name);
        let resp = send_json_request(&app, Method::POST, "/2/abtests", body).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        ids.push(json["abTestID"].as_i64().unwrap());
    }

    // Page 1: offset=0, limit=2 → first 2 in order
    let resp = send_empty_request(&app, Method::GET, "/2/abtests?offset=0&limit=2").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(sorted_object_keys(&json), vec!["abtests", "count", "total"]);
    assert_eq!(json["count"], 2);
    assert_eq!(json["total"], 5);
    let page1_ids: Vec<i64> = json["abtests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["abTestID"].as_i64().unwrap())
        .collect();
    for abtest in json["abtests"].as_array().unwrap() {
        assert_algolia_abtest_schema(abtest, "active", false);
    }
    assert_eq!(
        page1_ids,
        &ids[0..2],
        "page 1 must contain first 2 experiments"
    );

    // Page 2: offset=2, limit=2 → next 2 in order
    let resp = send_empty_request(&app, Method::GET, "/2/abtests?offset=2&limit=2").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["count"], 2);
    assert_eq!(json["total"], 5);
    let page2_ids: Vec<i64> = json["abtests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["abTestID"].as_i64().unwrap())
        .collect();
    for abtest in json["abtests"].as_array().unwrap() {
        assert_algolia_abtest_schema(abtest, "active", false);
    }
    assert_eq!(
        page2_ids,
        &ids[2..4],
        "page 2 must contain next 2 experiments"
    );

    // Page 3: offset=4, limit=2 → remaining 1
    let resp = send_empty_request(&app, Method::GET, "/2/abtests?offset=4&limit=2").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["count"], 1);
    assert_eq!(json["total"], 5);
    let page3_ids: Vec<i64> = json["abtests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["abTestID"].as_i64().unwrap())
        .collect();
    for abtest in json["abtests"].as_array().unwrap() {
        assert_algolia_abtest_schema(abtest, "active", false);
    }
    assert_eq!(page3_ids, &ids[4..5], "page 3 must contain last experiment");

    // Beyond range: offset=5, limit=2 → empty (but total still 5)
    let resp = send_empty_request(&app, Method::GET, "/2/abtests?offset=5&limit=2").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["count"], 0);
    assert_eq!(json["total"], 5);
    let beyond_page_abtests = json["abtests"]
        .as_array()
        .expect("abtests must be an array when total > 0");
    assert!(
        beyond_page_abtests.is_empty(),
        "beyond-range page must be empty"
    );
}

#[tokio::test]
async fn list_experiments_filter_applied_before_pagination() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    // Create 5 experiments: 3 matching prefix "prod_", 2 not matching
    let test_indices = [
        "prod_alpha_v1",
        "dev_beta",
        "prod_charlie_v1",
        "staging_delta",
        "prod_echo_v1",
    ];
    let mut prod_ids = Vec::new();
    for idx in &test_indices {
        let body = create_algolia_experiment_body(idx);
        let resp = send_json_request(&app, Method::POST, "/2/abtests", body).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let id = json["abTestID"].as_i64().unwrap();
        if idx.starts_with("prod_") {
            prod_ids.push(id);
        }
    }
    assert_eq!(prod_ids.len(), 3, "should have 3 prod experiments");

    // Filter by indexPrefix=prod_, limit=2 → total=3, count=2
    let resp = send_empty_request(&app, Method::GET, "/2/abtests?indexPrefix=prod_&limit=2").await;
    let json = body_json(resp).await;
    assert_eq!(json["total"], 3, "total must reflect filtered set size");
    assert_eq!(json["count"], 2, "count must be min(limit, remaining)");
    let page1_ids: Vec<i64> = json["abtests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["abTestID"].as_i64().unwrap())
        .collect();
    assert_eq!(page1_ids, &prod_ids[0..2], "page 1 of filtered list");

    // Second page of filtered results: offset=2, limit=2 → count=1, total=3
    let resp = send_empty_request(
        &app,
        Method::GET,
        "/2/abtests?indexPrefix=prod_&offset=2&limit=2",
    )
    .await;
    let json = body_json(resp).await;
    assert_eq!(json["total"], 3, "total must stay the same across pages");
    assert_eq!(json["count"], 1, "count must be min(limit, remaining)");
    let page2_ids: Vec<i64> = json["abtests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["abTestID"].as_i64().unwrap())
        .collect();
    assert_eq!(page2_ids, &prod_ids[2..3], "page 2 of filtered list");

    // Combine prefix + suffix filtering: prod_*_v1
    let resp = send_empty_request(
        &app,
        Method::GET,
        "/2/abtests?indexPrefix=prod_&indexSuffix=_v1",
    )
    .await;
    let json = body_json(resp).await;
    assert_eq!(json["total"], 3, "all prod_ indices end in _v1");
    assert_eq!(json["count"], 3);
}

/// Regression-pin that exact `indexName` filtering is exact, never prefix.
///
/// The collision fixture seeds three `products`-prefixed control indexes
/// (`products`, `products_staging`, `products_v2`). Against the same fixture:
/// - `indexName=products` must return only the exactly-named index (count 1).
/// - `indexPrefix=products` must return all three prefix collisions (count 3).
///
/// If exact matching silently degraded into prefix matching, the exact query
/// would return 3 instead of 1 and this test would fail — that divergence is the
/// mutation-sensitive proof that the two filters cannot behave identically.
#[tokio::test]
async fn list_experiments_index_name_is_exact_not_prefix() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);
    seed_exact_index_collision_fixture(&app).await;

    // Exact match: only the experiment whose control index is exactly `products`.
    let response = send_empty_request(&app, Method::GET, "/2/abtests?indexName=products").await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["count"], 1);
    assert_eq!(json["total"], 1);
    let abtests = json["abtests"]
        .as_array()
        .expect("abtests must be an array");
    assert_eq!(abtests.len(), 1);
    assert_eq!(abtests[0]["variants"][0]["index"], "products");

    // Prefix match against the same fixture: every control index starting with
    // `products`, i.e. `products`, `products_staging`, and `products_v2`.
    let response = send_empty_request(&app, Method::GET, "/2/abtests?indexPrefix=products").await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["count"], 3);
    assert_eq!(json["total"], 3);
    let abtests = json["abtests"]
        .as_array()
        .expect("abtests must be an array");
    assert_eq!(abtests.len(), 3);
    let mut control_indexes: Vec<String> = abtests
        .iter()
        .map(|t| {
            t["variants"][0]["index"]
                .as_str()
                .expect("control variant index must be a string")
                .to_string()
        })
        .collect();
    control_indexes.sort();
    assert_eq!(
        control_indexes,
        vec![
            "products".to_string(),
            "products_staging".to_string(),
            "products_v2".to_string(),
        ]
    );
}

#[tokio::test]
async fn list_experiments_exact_index_name_no_match_is_empty() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);
    seed_exact_index_collision_fixture(&app).await;

    let response =
        send_empty_request(&app, Method::GET, "/2/abtests?indexName=does_not_exist").await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(json["abtests"].is_null());
    assert_eq!(json["count"], 0);
    assert_eq!(json["total"], 0);
}

#[tokio::test]
async fn list_experiments_exact_index_name_composes_with_prefix_and_suffix() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);
    seed_exact_index_collision_fixture(&app).await;

    let matching_response = send_empty_request(
        &app,
        Method::GET,
        "/2/abtests?indexName=products&indexPrefix=prod&indexSuffix=cts",
    )
    .await;
    assert_eq!(matching_response.status(), StatusCode::OK);
    let matching_json = body_json(matching_response).await;
    assert_eq!(matching_json["count"], 1);
    assert_eq!(matching_json["total"], 1);
    let abtests = matching_json["abtests"]
        .as_array()
        .expect("abtests must be an array");
    assert_eq!(abtests.len(), 1);
    assert_eq!(abtests[0]["variants"][0]["index"], "products");

    let conflicting_response = send_empty_request(
        &app,
        Method::GET,
        "/2/abtests?indexName=products&indexPrefix=prod&indexSuffix=_staging",
    )
    .await;
    assert_eq!(conflicting_response.status(), StatusCode::OK);
    let conflicting_json = body_json(conflicting_response).await;
    assert!(conflicting_json["abtests"].is_null());
    assert_eq!(conflicting_json["count"], 0);
    assert_eq!(conflicting_json["total"], 0);
}

#[tokio::test]
async fn list_experiments_exact_index_name_empty_string_matches_nothing() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);
    seed_exact_index_collision_fixture(&app).await;

    let response = send_empty_request(&app, Method::GET, "/2/abtests?indexName=").await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(json["abtests"].is_null());
    assert_eq!(json["count"], 0);
    assert_eq!(json["total"], 0);
}

#[tokio::test]
async fn list_experiments_count_equals_page_size_total_equals_filtered() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    // Create 6 experiments: 4 with suffix "_v2", 2 without
    let test_indices = [
        "alpha_v2",
        "beta_v2",
        "charlie_v2",
        "delta_v2",
        "echo_v1",
        "foxtrot_v3",
    ];
    for idx in &test_indices {
        let body = create_algolia_experiment_body(idx);
        let resp = send_json_request(&app, Method::POST, "/2/abtests", body).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // No filter: total=6, limit=3 → count=3
    let resp = send_empty_request(&app, Method::GET, "/2/abtests?limit=3").await;
    let json = body_json(resp).await;
    assert_eq!(json["total"], 6, "total = all experiments");
    assert_eq!(json["count"], 3, "count = page size");

    // Filter by suffix _v2: total=4, limit=3 → count=3
    let resp = send_empty_request(&app, Method::GET, "/2/abtests?indexSuffix=_v2&limit=3").await;
    let json = body_json(resp).await;
    assert_eq!(json["total"], 4, "total = filtered count");
    assert_eq!(json["count"], 3, "count = min(limit, filtered)");

    // Filter by suffix _v2, offset=3: total=4, count=1
    let resp = send_empty_request(
        &app,
        Method::GET,
        "/2/abtests?indexSuffix=_v2&limit=3&offset=3",
    )
    .await;
    let json = body_json(resp).await;
    assert_eq!(json["total"], 4, "total stays same across pages");
    assert_eq!(json["count"], 1, "count = remaining items on last page");

    // Filter with no matches: total=0, count=0, abtests=null
    let resp = send_empty_request(&app, Method::GET, "/2/abtests?indexPrefix=nonexistent_").await;
    let json = body_json(resp).await;
    assert_eq!(json["total"], 0);
    assert_eq!(json["count"], 0);
    assert!(
        json["abtests"].is_null(),
        "no matches → abtests must be null"
    );
}

#[tokio::test]
async fn start_already_running_experiment_returns_409() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let resp = send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Try starting again — should conflict
    let resp = send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn start_second_experiment_on_same_index_returns_409() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let first_id = create_experiment_and_get_id(&app).await;
    let mut second_body = create_experiment_body_for_index("products");
    second_body["name"] = serde_json::json!("Ranking test 2");
    let second_resp = send_json_request(&app, Method::POST, "/2/abtests", second_body).await;
    assert_eq!(second_resp.status(), StatusCode::OK);
    let second_json = body_json(second_resp).await;
    let second_id = second_json["abTestID"].as_i64().unwrap();

    let first_start =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{first_id}/start")).await;
    assert_eq!(first_start.status(), StatusCode::OK);

    let second_start =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{second_id}/start")).await;
    assert_eq!(second_start.status(), StatusCode::CONFLICT);
}

/// Verify that starting an experiment sets a non-null RFC 3339 `startDate` in the results endpoint, while `endedAt` remains null.
#[tokio::test]
async fn start_experiment_sets_started_at_timestamp() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let resp = send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let _json = body_json(resp).await;
    let results_resp =
        send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}/results")).await;
    let json = body_json(results_resp).await;
    assert!(
        json["startDate"].as_str().is_some(),
        "startDate should be set after start"
    );
    assert_eq!(json["endedAt"], serde_json::Value::Null);
}

#[tokio::test]
async fn list_experiments_exposes_started_at_only_after_start() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;

    let draft_list = send_empty_request(&app, Method::GET, "/2/abtests").await;
    assert_eq!(draft_list.status(), StatusCode::OK);
    let draft_json = body_json(draft_list).await;
    let draft_experiment = &draft_json["abtests"][0];
    assert!(
        draft_experiment.get("startedAt").is_none(),
        "draft experiments should not expose startedAt"
    );

    let start_resp =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(start_resp.status(), StatusCode::OK);

    let running_list = send_empty_request(&app, Method::GET, "/2/abtests").await;
    assert_eq!(running_list.status(), StatusCode::OK);
    let running_json = body_json(running_list).await;
    let running_experiment = &running_json["abtests"][0];
    assert!(
        running_experiment["startedAt"].as_str().is_some(),
        "running experiments should expose startedAt so draft and running are distinguishable"
    );
}

/// Verify that stopping an experiment sets the `stoppedAt` timestamp to a non-null RFC 3339 string.
#[tokio::test]
async fn stop_experiment_sets_ended_at_timestamp() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let start_resp =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(start_resp.status(), StatusCode::OK);

    let stop_resp = send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/stop")).await;
    assert_eq!(stop_resp.status(), StatusCode::OK);
    let _json = body_json(stop_resp).await;
    let get_resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}")).await;
    let json = body_json(get_resp).await;
    assert!(
        json["stoppedAt"].as_str().is_some(),
        "stoppedAt should be set after stop"
    );
}

/// Verify that deleting a stopped experiment returns 200 with the standard Algolia action shape.
#[tokio::test]
async fn delete_stopped_experiment_returns_200_action_shape() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;
    let start_resp =
        send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(start_resp.status(), StatusCode::OK);
    let stop_resp = send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/stop")).await;
    assert_eq!(stop_resp.status(), StatusCode::OK);

    let delete_resp = send_empty_request(&app, Method::DELETE, &format!("/2/abtests/{id}")).await;
    assert_eq!(delete_resp.status(), StatusCode::OK);
    let delete_json = body_json(delete_resp).await;
    assert_algolia_action_shape(&delete_json, id, "products");
}

#[tokio::test]
async fn results_nonexistent_experiment_returns_404() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let resp = send_empty_request(&app, Method::GET, "/2/abtests/nonexistent/results").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Verify that Bayesian probability estimates are returned even when the significance gate is closed, while frequentist significance remains gated (null).
#[test]
fn build_results_response_includes_bayesian_when_gate_not_ready() {
    let now = chrono::Utc::now().timestamp_millis();
    let experiment = Experiment {
        id: "exp-bayes-1".to_string(),
        name: "Bayes visibility".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: now - 1_000,
        started_at: Some(now - 60_000),
        ended_at: None,
        stopped_at: None,
        minimum_days: 14,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let metrics = metrics::ExperimentMetrics {
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: 10,
            users: 3,
            clicks: 6,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.6,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(2.0, 3.0), (3.0, 5.0), (1.0, 2.0)],
            per_user_conversion_rates: vec![(0.0, 3.0), (0.0, 5.0), (0.0, 2.0)],
            per_user_zero_result_rates: vec![(0.0, 3.0), (0.0, 5.0), (0.0, 2.0)],
            per_user_abandonment_rates: vec![(0.0, 3.0), (0.0, 5.0), (0.0, 2.0)],
            per_user_revenues: vec![0.0, 0.0, 0.0],
            per_user_ids: (0..3).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: 10,
            users: 3,
            clicks: 8,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.8,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(2.0, 2.0), (3.0, 4.0), (3.0, 4.0)],
            per_user_conversion_rates: vec![(0.0, 2.0), (0.0, 4.0), (0.0, 4.0)],
            per_user_zero_result_rates: vec![(0.0, 2.0), (0.0, 4.0), (0.0, 4.0)],
            per_user_abandonment_rates: vec![(0.0, 2.0), (0.0, 4.0), (0.0, 4.0)],
            per_user_revenues: vec![0.0, 0.0, 0.0],
            per_user_ids: (0..3).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);
    assert!(
        !response.gate.ready_to_read,
        "gate should be closed for low-count, short-runtime experiment"
    );
    assert!(
        response.bayesian.is_some(),
        "bayesian must be returned even while significance is gated"
    );
    assert!(response.significance.is_none());
}

/// Verify that sample-ratio mismatch detection runs and surfaces warnings in the recommendation even when the significance gate is closed.
#[test]
fn build_results_response_includes_srm_when_gate_not_ready() {
    let now = chrono::Utc::now().timestamp_millis();
    let experiment = Experiment {
        id: "exp-srm-1".to_string(),
        name: "SRM visibility".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: now - 1_000,
        started_at: Some(now - 60_000), // very recent → gate not ready
        ended_at: None,
        stopped_at: None,
        minimum_days: 14,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    // Heavily skewed split: 4500 vs 5500 at 50/50 → SRM should fire
    let metrics = metrics::ExperimentMetrics {
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: 4500,
            users: 1000,
            clicks: 900,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.2,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(1.0, 5.0); 1000],
            per_user_conversion_rates: vec![(0.0, 5.0); 1000],
            per_user_zero_result_rates: vec![(0.0, 5.0); 1000],
            per_user_abandonment_rates: vec![(0.0, 5.0); 1000],
            per_user_revenues: vec![0.0; 1000],
            per_user_ids: (0..1000).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: 5500,
            users: 1000,
            clicks: 1100,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.2,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(1.0, 5.0); 1000],
            per_user_conversion_rates: vec![(0.0, 5.0); 1000],
            per_user_zero_result_rates: vec![(0.0, 5.0); 1000],
            per_user_abandonment_rates: vec![(0.0, 5.0); 1000],
            per_user_revenues: vec![0.0; 1000],
            per_user_ids: (0..1000).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);
    assert!(
        !response.gate.ready_to_read,
        "gate should be closed for short-runtime experiment"
    );
    assert!(
        response.sample_ratio_mismatch,
        "SRM must be computed even when gate is not ready"
    );
    // Significance should still be gated
    assert!(response.significance.is_none());
    // Recommendation should warn about SRM even pre-gate
    assert!(
        response
            .recommendation
            .as_ref()
            .is_some_and(|r| r.contains("Sample ratio mismatch")),
        "recommendation should warn about SRM even when gate is closed"
    );
}

/// Verify that when both minimum-N and minimum-days thresholds are met, the gate opens and significance, Bayesian analysis, and a recommendation are all populated.
#[test]
fn build_results_response_gate_ready_returns_significance() {
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000); // 3 days ago
    let experiment = Experiment {
        id: "exp-sig-1".to_string(),
        name: "Significance gate".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    // High baseline CTR (0.5) keeps required_sample_size low (~13k per arm).
    // Use 15000 per arm to exceed the threshold comfortably.
    // Clear CTR difference: control 50%, variant 65%.
    let n = 15_000;
    let users = 3000;
    let metrics = metrics::ExperimentMetrics {
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: n,
            users,
            clicks: 7500,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.5,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: (0..users).map(|_| (2.5, 5.0)).collect(),
            per_user_conversion_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_zero_result_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_abandonment_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_revenues: vec![0.0; users as usize],
            per_user_ids: (0..users as usize).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: n,
            users,
            clicks: 9750,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.65,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: (0..users).map(|_| (3.25, 5.0)).collect(),
            per_user_conversion_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_zero_result_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_abandonment_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_revenues: vec![0.0; users as usize],
            per_user_ids: (0..users as usize).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 5,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);
    assert!(
        response.gate.ready_to_read,
        "gate should be ready: per_arm={}, required={}",
        response.gate.current_searches_per_arm, response.gate.required_searches_per_arm
    );
    let sig = response
        .significance
        .as_ref()
        .expect("significance must be present when gate ready");
    assert!(
        sig.p_value < 0.05,
        "p_value={} should be < 0.05",
        sig.p_value
    );
    assert!(sig.significant);
    assert_eq!(sig.winner.as_deref(), Some("variant"));
    assert!(response.bayesian.is_some());
    assert!(response.bayesian.as_ref().unwrap().prob_variant_better > 0.9);
    assert!(!response.sample_ratio_mismatch);
    assert!(
        response
            .recommendation
            .as_ref()
            .is_some_and(|r| r.contains("Statistically significant")),
        "recommendation should declare significant result"
    );
}

/// Verify that when `primaryMetric` is `ConversionRate`, significance and winner are determined from conversion data rather than CTR data.
#[test]
fn build_results_response_conversion_rate_uses_conversion_metric() {
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-conv-metric-1".to_string(),
        name: "Conversion metric selection".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::ConversionRate,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let users = 3000;
    let searches_per_user = 10_u64;
    let n = users as u64 * searches_per_user;
    let metrics = metrics::ExperimentMetrics {
        // CTR strongly favors variant...
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: n,
            users: users as u64,
            clicks: 4_500,
            conversions: 22_500,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.15,
            conversion_rate: 0.75,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: (0..users)
                .map(|i| if i % 2 == 0 { (1.0, 10.0) } else { (2.0, 10.0) })
                .collect(),
            per_user_conversion_rates: (0..users)
                .map(|i| if i % 2 == 0 { (8.0, 10.0) } else { (7.0, 10.0) })
                .collect(),
            per_user_zero_result_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_abandonment_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_revenues: vec![0.0; users],
            per_user_ids: (0..users).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: n,
            users: users as u64,
            clicks: 25_500,
            conversions: 13_500,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.85,
            conversion_rate: 0.45,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: (0..users)
                .map(|i| if i % 2 == 0 { (8.0, 10.0) } else { (9.0, 10.0) })
                .collect(),
            per_user_conversion_rates: (0..users)
                .map(|i| if i % 2 == 0 { (5.0, 10.0) } else { (4.0, 10.0) })
                .collect(),
            per_user_zero_result_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_abandonment_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_revenues: vec![0.0; users],
            per_user_ids: (0..users).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);
    assert!(response.gate.ready_to_read);
    let sig = response
        .significance
        .as_ref()
        .expect("significance must be present when gate ready");
    assert_eq!(
        sig.winner.as_deref(),
        Some("control"),
        "conversion-rate winner must follow conversion data, not CTR data"
    );
}

/// Verify that Bayesian analysis uses the experiment's primary metric data rather than defaulting to CTR, by constructing a `ConversionRate` experiment where CTR and conversion rate disagree on direction.
#[test]
fn build_results_response_bayesian_uses_primary_metric_data() {
    // ConversionRate experiment where CTR favors variant but conversion rate favors control.
    // If bayesian uses CTR data (bug), probVariantBetter > 0.5.
    // If bayesian correctly uses conversion data, probVariantBetter < 0.5.
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-bayes-metric-1".to_string(),
        name: "Bayesian metric selection".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::ConversionRate,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let n = 10_000_u64;
    let metrics = metrics::ExperimentMetrics {
        // CTR: variant much higher (8000 vs 2000 clicks) - if bayesian used CTR, probVariantBetter would be near 1.0
        // ConversionRate: control much higher (8000 vs 2000 conversions)
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: n,
            users: 1000,
            clicks: 2000,      // low CTR
            conversions: 8000, // high conversion rate
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.20,
            conversion_rate: 0.80,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(2.0, 10.0); 1000],
            per_user_conversion_rates: vec![(8.0, 10.0); 1000],
            per_user_zero_result_rates: vec![(0.0, 10.0); 1000],
            per_user_abandonment_rates: vec![(0.0, 10.0); 1000],
            per_user_revenues: vec![0.0; 1000],
            per_user_ids: (0..1000).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: n,
            users: 1000,
            clicks: 8000,      // high CTR
            conversions: 2000, // low conversion rate
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.80,
            conversion_rate: 0.20,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(8.0, 10.0); 1000],
            per_user_conversion_rates: vec![(2.0, 10.0); 1000],
            per_user_zero_result_rates: vec![(0.0, 10.0); 1000],
            per_user_abandonment_rates: vec![(0.0, 10.0); 1000],
            per_user_revenues: vec![0.0; 1000],
            per_user_ids: (0..1000).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);
    let bayesian = response.bayesian.expect("bayesian must be present");
    assert!(
        bayesian.prob_variant_better < 0.01,
        "for ConversionRate metric, bayesian should use conversion data (control wins), got probVariantBetter={}",
        bayesian.prob_variant_better
    );
}

/// Verify that for lower-is-better metrics (e.g. `ZeroResultRate`), `probVariantBetter` is high when the variant has a lower (better) rate.
#[test]
fn build_results_response_bayesian_flipped_for_lower_is_better_metric() {
    // ZeroResultRate experiment where variant has LOWER zero-result rate (better).
    // probVariantBetter should be high (variant is better because lower is better).
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-bayes-lower-1".to_string(),
        name: "Bayesian lower-is-better".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::ZeroResultRate,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let n = 10_000_u64;
    let metrics = metrics::ExperimentMetrics {
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: n,
            users: 1000,
            clicks: 5000,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 4000, // 40% zero-result rate (bad)
            abandoned_searches: 0,
            ctr: 0.50,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.40,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(5.0, 10.0); 1000],
            per_user_conversion_rates: vec![(0.0, 10.0); 1000],
            per_user_zero_result_rates: vec![(4.0, 10.0); 1000],
            per_user_abandonment_rates: vec![(0.0, 10.0); 1000],
            per_user_revenues: vec![0.0; 1000],
            per_user_ids: (0..1000).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: n,
            users: 1000,
            clicks: 5000,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 1000, // 10% zero-result rate (good)
            abandoned_searches: 0,
            ctr: 0.50,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.10,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(5.0, 10.0); 1000],
            per_user_conversion_rates: vec![(0.0, 10.0); 1000],
            per_user_zero_result_rates: vec![(1.0, 10.0); 1000],
            per_user_abandonment_rates: vec![(0.0, 10.0); 1000],
            per_user_revenues: vec![0.0; 1000],
            per_user_ids: (0..1000).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);
    let bayesian = response.bayesian.expect("bayesian must be present");
    // Variant has LOWER zero-result rate = BETTER. probVariantBetter should be high.
    assert!(
        bayesian.prob_variant_better > 0.99,
        "for lower-is-better metric with variant clearly better, probVariantBetter should be high, got {}",
        bayesian.prob_variant_better
    );
}

/// Verify that when `primaryMetric` is `ZeroResultRate`, the variant with the lower rate wins and relative improvement is reported as positive, even when CTR favors the other arm.
#[test]
fn build_results_response_zero_result_rate_treats_lower_as_better() {
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-zrr-direction-1".to_string(),
        name: "Zero-result direction".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::ZeroResultRate,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let users = 3000;
    let searches_per_user = 10_u64;
    let n = users as u64 * searches_per_user;
    let metrics = metrics::ExperimentMetrics {
        // CTR favors control, but ZeroResultRate favors variant (lower is better).
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: n,
            users: users as u64,
            clicks: 25_500,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 13_500,
            abandoned_searches: 0,
            ctr: 0.85,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.45,
            abandonment_rate: 0.0,
            per_user_ctrs: (0..users)
                .map(|i| if i % 2 == 0 { (8.0, 10.0) } else { (9.0, 10.0) })
                .collect(),
            per_user_conversion_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_zero_result_rates: (0..users)
                .map(|i| if i % 2 == 0 { (4.0, 10.0) } else { (5.0, 10.0) })
                .collect(),
            per_user_abandonment_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_revenues: vec![0.0; users],
            per_user_ids: (0..users).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: n,
            users: users as u64,
            clicks: 4_500,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 4_500,
            abandoned_searches: 0,
            ctr: 0.15,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.15,
            abandonment_rate: 0.0,
            per_user_ctrs: (0..users)
                .map(|i| if i % 2 == 0 { (1.0, 10.0) } else { (2.0, 10.0) })
                .collect(),
            per_user_conversion_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_zero_result_rates: (0..users)
                .map(|i| if i % 2 == 0 { (1.0, 10.0) } else { (2.0, 10.0) })
                .collect(),
            per_user_abandonment_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_revenues: vec![0.0; users],
            per_user_ids: (0..users).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);
    assert!(response.gate.ready_to_read);
    let sig = response
        .significance
        .as_ref()
        .expect("significance must be present when gate ready");
    assert_eq!(
        sig.winner.as_deref(),
        Some("variant"),
        "lower zero-result rate should win even when CTR goes the other way"
    );
    assert!(
        sig.relative_improvement > 0.0,
        "relative improvement should be positive when variant improves a lower-is-better metric"
    );
}

/// Verify that when `primaryMetric` is `AbandonmentRate`, the variant with the lower rate wins and relative improvement is reported as positive.
#[test]
fn build_results_response_abandonment_rate_treats_lower_as_better() {
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-ar-direction-1".to_string(),
        name: "Abandonment direction".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::AbandonmentRate,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let users = 3000;
    let searches_per_user = 10_u64;
    let n = users as u64 * searches_per_user;
    let metrics = metrics::ExperimentMetrics {
        // CTR favors control, but AbandonmentRate favors variant (lower is better).
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: n,
            users: users as u64,
            clicks: 25_500,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 13_500,
            ctr: 0.85,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.45,
            per_user_ctrs: (0..users)
                .map(|i| if i % 2 == 0 { (8.0, 10.0) } else { (9.0, 10.0) })
                .collect(),
            per_user_conversion_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_zero_result_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_abandonment_rates: (0..users)
                .map(|i| if i % 2 == 0 { (4.0, 10.0) } else { (5.0, 10.0) })
                .collect(),
            per_user_revenues: vec![0.0; users],
            per_user_ids: (0..users).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: n,
            users: users as u64,
            clicks: 4_500,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 4_500,
            ctr: 0.15,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.15,
            per_user_ctrs: (0..users)
                .map(|i| if i % 2 == 0 { (1.0, 10.0) } else { (2.0, 10.0) })
                .collect(),
            per_user_conversion_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_zero_result_rates: (0..users).map(|_| (0.0, 10.0)).collect(),
            per_user_abandonment_rates: (0..users)
                .map(|i| if i % 2 == 0 { (1.0, 10.0) } else { (2.0, 10.0) })
                .collect(),
            per_user_revenues: vec![0.0; users],
            per_user_ids: (0..users).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);
    assert!(response.gate.ready_to_read);
    let sig = response
        .significance
        .as_ref()
        .expect("significance must be present when gate ready");
    assert_eq!(
        sig.winner.as_deref(),
        Some("variant"),
        "lower abandonment rate should win even when CTR goes the other way"
    );
    assert!(
        sig.relative_improvement > 0.0,
        "relative improvement should be positive when variant improves a lower-is-better metric"
    );
}

/// Verify that the gate includes a positive `estimatedDaysRemaining` when the experiment is running with incoming data but has not yet met the minimum-days or sample-size thresholds.
#[test]
fn build_results_response_gate_has_estimated_days_remaining() {
    let now = chrono::Utc::now().timestamp_millis();
    // started 1 day ago, needs 14 minimum_days, low N
    let started_at = now - (24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-eta-1".to_string(),
        name: "ETA test".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 14,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let metrics = metrics::ExperimentMetrics {
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: 100,
            users: 50,
            clicks: 20,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.2,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(1.0, 5.0); 50],
            per_user_conversion_rates: vec![(0.0, 5.0); 50],
            per_user_zero_result_rates: vec![(0.0, 5.0); 50],
            per_user_abandonment_rates: vec![(0.0, 5.0); 50],
            per_user_revenues: vec![0.0; 50],
            per_user_ids: (0..50).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: 100,
            users: 50,
            clicks: 25,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.25,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(1.0, 4.0); 50],
            per_user_conversion_rates: vec![(0.0, 4.0); 50],
            per_user_zero_result_rates: vec![(0.0, 4.0); 50],
            per_user_abandonment_rates: vec![(0.0, 4.0); 50],
            per_user_revenues: vec![0.0; 50],
            per_user_ids: (0..50).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);
    assert!(!response.gate.ready_to_read);
    // Should have an estimate since we have data flowing
    assert!(
        response.gate.estimated_days_remaining.is_some(),
        "estimatedDaysRemaining should be present when experiment is running with data"
    );
    let eta = response.gate.estimated_days_remaining.unwrap();
    assert!(eta > 0.0, "ETA should be positive");
    // Should be at least minimum_days minus elapsed (≈13 days)
    assert!(
        eta >= 12.0,
        "ETA should account for minimum_days requirement"
    );
}

/// Verify soft-gate behavior: when minimum-N is reached but minimum-days is not, significance is still computed and the gate reports `readyToRead: false` with `minimumNReached: true`.
#[test]
fn build_results_response_n_reached_days_not_reached_still_returns_significance() {
    let now = chrono::Utc::now().timestamp_millis();
    // Started 3 days ago but minimum_days is 14 — days NOT reached
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-soft-gate-1".to_string(),
        name: "Soft gate test".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 14,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    // High baseline CTR (0.5) keeps required_sample_size low (~13k per arm).
    // Use 15000 per arm to exceed the threshold — N IS reached.
    let n = 15_000;
    let users = 3000;
    let metrics = metrics::ExperimentMetrics {
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: n,
            users,
            clicks: 7500,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.5,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: (0..users).map(|_| (2.5, 5.0)).collect(),
            per_user_conversion_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_zero_result_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_abandonment_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_revenues: vec![0.0; users as usize],
            per_user_ids: (0..users as usize).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: n,
            users,
            clicks: 9750,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.65,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: (0..users).map(|_| (3.25, 5.0)).collect(),
            per_user_conversion_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_zero_result_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_abandonment_rates: (0..users).map(|_| (0.0, 5.0)).collect(),
            per_user_revenues: vec![0.0; users as usize],
            per_user_ids: (0..users as usize).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);

    // Gate: N reached but days not reached → readyToRead should be false
    assert!(
        response.gate.minimum_n_reached,
        "minimumNReached should be true"
    );
    assert!(
        !response.gate.minimum_days_reached,
        "minimumDaysReached should be false (3 days < 14)"
    );
    assert!(
        !response.gate.ready_to_read,
        "readyToRead should be false (days not reached)"
    );

    // Soft gate: significance SHOULD still be computed when N is reached
    let sig = response
        .significance
        .as_ref()
        .expect("significance must be present when N reached (soft gate)");
    assert!(sig.significant, "should be significant with clear CTR diff");
    assert_eq!(sig.winner.as_deref(), Some("variant"));

    // Recommendation should also be present
    assert!(
        response.recommendation.is_some(),
        "recommendation should be present when significance is computed"
    );
}

/// Construct an `ArmMetrics` value from per-user CTR tuples, deriving aggregate searches, clicks, users, and CTR automatically.
///
/// # Arguments
///
/// * `arm_name` - Name of the experiment arm.
/// * `per_user_ctrs` - Vec of `(clicks, searches)` tuples per user.
/// * `per_user_ids` - Corresponding user identifiers.
fn build_ctr_arm_metrics(
    arm_name: &str,
    per_user_ctrs: Vec<(f64, f64)>,
    per_user_ids: Vec<String>,
) -> metrics::ArmMetrics {
    let searches = per_user_ctrs.iter().map(|(_, d)| *d).sum::<f64>() as u64;
    let clicks = per_user_ctrs.iter().map(|(n, _)| *n).sum::<f64>() as u64;
    let users = per_user_ctrs.len() as u64;

    metrics::ArmMetrics {
        arm_name: arm_name.to_string(),
        searches,
        users,
        clicks,
        conversions: 0,
        revenue: 0.0,
        zero_result_searches: 0,
        abandoned_searches: 0,
        ctr: if searches > 0 {
            clicks as f64 / searches as f64
        } else {
            0.0
        },
        conversion_rate: 0.0,
        revenue_per_search: 0.0,
        zero_result_rate: 0.0,
        abandonment_rate: 0.0,
        per_user_ctrs: per_user_ctrs.clone(),
        per_user_conversion_rates: per_user_ctrs.iter().map(|(_, d)| (0.0, *d)).collect(),
        per_user_zero_result_rates: per_user_ctrs.iter().map(|(_, d)| (0.0, *d)).collect(),
        per_user_abandonment_rates: per_user_ctrs.iter().map(|(_, d)| (0.0, *d)).collect(),
        per_user_revenues: vec![0.0; users as usize],
        per_user_ids,
        mean_click_rank: 0.0,
    }
}

/// Verify that results for an interleaving experiment include the `interleaving` response block with delta, win/tie counts, p-value, significance flag, and data-quality check.
#[test]
fn build_results_response_includes_interleaving_preference_for_interleaving_experiment() {
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-interleaving-results-1".to_string(),
        name: "Interleaving results".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: Some("products_v2".to_string()),
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: Some(true),
    };

    let interleaving_metrics = metrics::InterleavingMetrics {
        preference: stats::PreferenceResult {
            delta_ab: -0.2,
            wins_a: 8,
            wins_b: 12,
            ties: 5,
            p_value: 0.03,
        },
        total_queries: 25,
        first_team_a_ratio: 0.48,
    };

    let response = build_results_response(&experiment, None, None, Some(&interleaving_metrics));
    let interleaving = response
        .interleaving
        .expect("interleaving response must be present for interleaving experiments");

    assert!((interleaving.delta_ab + 0.2).abs() < f64::EPSILON);
    assert_eq!(interleaving.wins_control, 8);
    assert_eq!(interleaving.wins_variant, 12);
    assert_eq!(interleaving.ties, 5);
    assert!((interleaving.p_value - 0.03).abs() < f64::EPSILON);
    assert!(interleaving.significant);
    assert_eq!(interleaving.total_queries, 25);
    assert!(interleaving.data_quality_ok); // 0.48 is within 0.45..0.55
}

/// Verify that standard (non-interleaving) experiments omit the `interleaving` response block even when interleaving metrics are provided.
#[test]
fn build_results_response_omits_interleaving_preference_for_standard_experiment() {
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-standard-results-1".to_string(),
        name: "Standard results".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let interleaving_metrics = metrics::InterleavingMetrics {
        preference: stats::PreferenceResult {
            delta_ab: 0.15,
            wins_a: 12,
            wins_b: 8,
            ties: 2,
            p_value: 0.04,
        },
        total_queries: 22,
        first_team_a_ratio: 0.5,
    };

    let response = build_results_response(&experiment, None, None, Some(&interleaving_metrics));
    assert!(
        response.interleaving.is_none(),
        "standard experiments must not include interleaving preference data"
    );
}

/// Verify the `dataQualityOk` flag boundary conditions: `firstTeamARatio` within [0.45, 0.55] passes, values outside that range fail.
#[test]
fn results_interleaving_data_quality_check() {
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-quality-check-1".to_string(),
        name: "Quality check".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: Some("products_control".to_string()),
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: None,
            index_name: Some("products_variant".to_string()),
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: Some(true),
    };

    // Balanced first-team distribution (0.50) → data_quality_ok = true
    let balanced_metrics = metrics::InterleavingMetrics {
        preference: stats::PreferenceResult {
            delta_ab: 0.0,
            wins_a: 50,
            wins_b: 50,
            ties: 10,
            p_value: 1.0,
        },
        total_queries: 110,
        first_team_a_ratio: 0.50,
    };
    let resp = build_results_response(&experiment, None, None, Some(&balanced_metrics));
    assert!(resp.interleaving.as_ref().unwrap().data_quality_ok);

    // Skewed first-team distribution (0.60) → data_quality_ok = false
    let skewed_metrics = metrics::InterleavingMetrics {
        preference: stats::PreferenceResult {
            delta_ab: 0.1,
            wins_a: 55,
            wins_b: 45,
            ties: 10,
            p_value: 0.3,
        },
        total_queries: 110,
        first_team_a_ratio: 0.60,
    };
    let resp = build_results_response(&experiment, None, None, Some(&skewed_metrics));
    assert!(!resp.interleaving.as_ref().unwrap().data_quality_ok);

    // Edge: 0.45 → data_quality_ok = true (boundary)
    let edge_metrics = metrics::InterleavingMetrics {
        preference: stats::PreferenceResult {
            delta_ab: 0.0,
            wins_a: 50,
            wins_b: 50,
            ties: 0,
            p_value: 1.0,
        },
        total_queries: 100,
        first_team_a_ratio: 0.45,
    };
    let resp = build_results_response(&experiment, None, None, Some(&edge_metrics));
    assert!(resp.interleaving.as_ref().unwrap().data_quality_ok);

    // Edge: 0.44 → data_quality_ok = false (just below lower bound)
    let just_below_lower = metrics::InterleavingMetrics {
        preference: stats::PreferenceResult {
            delta_ab: 0.0,
            wins_a: 50,
            wins_b: 50,
            ties: 0,
            p_value: 1.0,
        },
        total_queries: 100,
        first_team_a_ratio: 0.44,
    };
    let resp = build_results_response(&experiment, None, None, Some(&just_below_lower));
    assert!(!resp.interleaving.as_ref().unwrap().data_quality_ok);

    // Edge: 0.55 → data_quality_ok = true (upper boundary inclusive)
    let upper_boundary = metrics::InterleavingMetrics {
        preference: stats::PreferenceResult {
            delta_ab: 0.0,
            wins_a: 50,
            wins_b: 50,
            ties: 0,
            p_value: 1.0,
        },
        total_queries: 100,
        first_team_a_ratio: 0.55,
    };
    let resp = build_results_response(&experiment, None, None, Some(&upper_boundary));
    assert!(resp.interleaving.as_ref().unwrap().data_quality_ok);

    // Edge: 0.56 → data_quality_ok = false (just above upper bound)
    let just_above_upper = metrics::InterleavingMetrics {
        preference: stats::PreferenceResult {
            delta_ab: 0.0,
            wins_a: 50,
            wins_b: 50,
            ties: 0,
            p_value: 1.0,
        },
        total_queries: 100,
        first_team_a_ratio: 0.56,
    };
    let resp = build_results_response(&experiment, None, None, Some(&just_above_upper));
    assert!(!resp.interleaving.as_ref().unwrap().data_quality_ok);
}

/// Verify that CUPED variance reduction is applied when ≥100 users per arm have matched covariates with strong correlation, resulting in improved z-score signal-to-noise ratio.
#[test]
fn build_results_response_applies_cuped_when_covariates_available() {
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-cuped-apply-1".to_string(),
        name: "CUPED apply".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let users = 200;
    let searches_per_user = 100.0;

    let mut control_ids = Vec::with_capacity(users);
    let mut variant_ids = Vec::with_capacity(users);
    let mut control_samples = Vec::with_capacity(users);
    let mut variant_samples = Vec::with_capacity(users);
    let mut covariates = std::collections::HashMap::new();

    for i in 0..users {
        let x = i as f64;
        let noise = (i % 5) as f64 - 2.0;
        let control_clicks = 40.0 + (0.1 * x) + noise;
        let variant_clicks = 44.0 + (0.1 * x) + noise;

        let control_id = format!("c{i}");
        let variant_id = format!("v{i}");
        covariates.insert(control_id.clone(), x);
        covariates.insert(variant_id.clone(), x);
        control_ids.push(control_id);
        variant_ids.push(variant_id);
        control_samples.push((control_clicks, searches_per_user));
        variant_samples.push((variant_clicks, searches_per_user));
    }

    let metrics = metrics::ExperimentMetrics {
        control: build_ctr_arm_metrics("control", control_samples, control_ids),
        variant: build_ctr_arm_metrics("variant", variant_samples, variant_ids),
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let raw_response = build_results_response(&experiment, Some(&metrics), None, None);
    let cuped_response =
        build_results_response(&experiment, Some(&metrics), Some(&covariates), None);

    let raw_sig = raw_response
        .significance
        .expect("significance should be present when gate is ready");
    let cuped_sig = cuped_response
        .significance
        .expect("significance should be present when gate is ready");

    assert!(
        cuped_response.cuped_applied,
        "CUPED should be applied with >=100 matched users and a correlated covariate"
    );
    assert!(
        cuped_sig.z_score.abs() > raw_sig.z_score.abs(),
        "CUPED should improve signal-to-noise when covariate is strongly correlated"
    );
    assert!(
        (cuped_sig.z_score - raw_sig.z_score).abs() > f64::EPSILON,
        "z-score should change when CUPED adjustment is applied"
    );
}

/// Verify that CUPED is skipped when fewer than 100 users per arm have matched covariates, leaving z-score and p-value identical to raw values.
#[test]
fn build_results_response_skips_cuped_when_insufficient_coverage() {
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-cuped-skip-1".to_string(),
        name: "CUPED skip".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let users = 200;
    let searches_per_user = 100.0;

    let mut control_ids = Vec::with_capacity(users);
    let mut variant_ids = Vec::with_capacity(users);
    let mut control_samples = Vec::with_capacity(users);
    let mut variant_samples = Vec::with_capacity(users);
    let mut sparse_covariates = std::collections::HashMap::new();

    for i in 0..users {
        let x = i as f64;
        let noise = (i % 5) as f64 - 2.0;
        let control_clicks = 40.0 + (0.1 * x) + noise;
        let variant_clicks = 44.0 + (0.1 * x) + noise;

        let control_id = format!("c{i}");
        let variant_id = format!("v{i}");
        if i < 99 {
            sparse_covariates.insert(control_id.clone(), x);
            sparse_covariates.insert(variant_id.clone(), x);
        }
        control_ids.push(control_id);
        variant_ids.push(variant_id);
        control_samples.push((control_clicks, searches_per_user));
        variant_samples.push((variant_clicks, searches_per_user));
    }

    let metrics = metrics::ExperimentMetrics {
        control: build_ctr_arm_metrics("control", control_samples, control_ids),
        variant: build_ctr_arm_metrics("variant", variant_samples, variant_ids),
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let raw_response = build_results_response(&experiment, Some(&metrics), None, None);
    let sparse_cov_response =
        build_results_response(&experiment, Some(&metrics), Some(&sparse_covariates), None);

    let raw_sig = raw_response
        .significance
        .expect("significance should be present when gate is ready");
    let sparse_sig = sparse_cov_response
        .significance
        .expect("significance should be present when gate is ready");

    assert!(
        !sparse_cov_response.cuped_applied,
        "CUPED should not apply with fewer than 100 matched users per arm"
    );
    assert!(
        (sparse_sig.z_score - raw_sig.z_score).abs() < f64::EPSILON,
        "without CUPED coverage, z-score should be unchanged"
    );
    assert!(
        (sparse_sig.p_value - raw_sig.p_value).abs() < f64::EPSILON,
        "without CUPED coverage, p-value should be unchanged"
    );
}

/// Verify that CUPED is skipped when only one arm has sufficient covariate coverage (≥100 matched users) while the other does not, leaving z-score and p-value unchanged from raw values.
#[test]
fn build_results_response_skips_cuped_when_one_arm_has_insufficient_coverage() {
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-cuped-one-arm-skip-1".to_string(),
        name: "CUPED one-arm skip".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let users = 200;
    let searches_per_user = 100.0;

    let mut control_ids = Vec::with_capacity(users);
    let mut variant_ids = Vec::with_capacity(users);
    let mut control_samples = Vec::with_capacity(users);
    let mut variant_samples = Vec::with_capacity(users);
    let mut partial_covariates = std::collections::HashMap::new();

    for i in 0..users {
        let x = i as f64;
        let noise = (i % 7) as f64 - 3.0;
        let control_clicks = 40.0 + (0.1 * x) + noise;
        let variant_clicks = 44.0 + (0.1 * x) + noise;

        let control_id = format!("c{i}");
        let variant_id = format!("v{i}");

        // Control has full coverage, variant has only 50 matched users.
        partial_covariates.insert(control_id.clone(), x);
        if i < 50 {
            partial_covariates.insert(variant_id.clone(), x);
        }

        control_ids.push(control_id);
        variant_ids.push(variant_id);
        control_samples.push((control_clicks, searches_per_user));
        variant_samples.push((variant_clicks, searches_per_user));
    }

    let metrics = metrics::ExperimentMetrics {
        control: build_ctr_arm_metrics("control", control_samples, control_ids),
        variant: build_ctr_arm_metrics("variant", variant_samples, variant_ids),
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let raw_response = build_results_response(&experiment, Some(&metrics), None, None);
    let partial_cov_response =
        build_results_response(&experiment, Some(&metrics), Some(&partial_covariates), None);

    let raw_sig = raw_response
        .significance
        .expect("significance should be present when gate is ready");
    let partial_sig = partial_cov_response
        .significance
        .expect("significance should be present when gate is ready");

    assert!(
        !partial_cov_response.cuped_applied,
        "CUPED should not apply when either arm has fewer than 100 matched users"
    );
    assert!(
        (partial_sig.z_score - raw_sig.z_score).abs() < f64::EPSILON,
        "z-score should remain unchanged when one arm lacks CUPED coverage"
    );
    assert!(
        (partial_sig.p_value - raw_sig.p_value).abs() < f64::EPSILON,
        "p-value should remain unchanged when one arm lacks CUPED coverage"
    );
}

/// Verify that creating a Mode B experiment (control and variant on different indexes, no `customSearchParameters`) returns 200 with correct variant index names.
#[tokio::test]
async fn create_mode_b_experiment_returns_200() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let body = serde_json::json!({
        "name": "Index redirect test",
        "variants": [
            { "index": "products", "trafficPercentage": 50, "description": "control" },
            { "index": "products_v2", "trafficPercentage": 50, "description": "variant" }
        ],
        "endAt": "2099-01-01T00:00:00Z",
        "metrics": [{ "name": "clickThroughRate" }]
    });

    let resp = send_json_request(&app, Method::POST, "/2/abtests", body).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let create_json = body_json(resp).await;
    let id = create_json["abTestID"].as_i64().unwrap();
    let get_resp = send_empty_request(&app, Method::GET, &format!("/2/abtests/{id}")).await;
    let json = body_json(get_resp).await;
    assert_eq!(json["variants"][0]["index"], "products");
    assert_eq!(json["variants"][1]["index"], "products_v2");
    assert!(json["variants"][1]["customSearchParameters"].is_null());
}

/// Verify that creating a Mode A experiment with an invalid `customSearchParameters` type (e.g. string instead of bool for `enableSynonyms`) returns 400 Bad Request.
#[tokio::test]
async fn create_experiment_control_with_overrides_returns_400() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let body = serde_json::json!({
        "name": "Bad custom params",
        "variants": [
            { "index": "products", "trafficPercentage": 50, "description": "control" },
            {
                "index": "products",
                "trafficPercentage": 50,
                "description": "variant",
                "customSearchParameters": { "enableSynonyms": "not-a-bool" }
            }
        ],
        "endAt": "2099-01-01T00:00:00Z",
        "metrics": [{ "name": "clickThroughRate" }]
    });

    let resp = send_json_request(&app, Method::POST, "/2/abtests", body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Verify that creating a variant with both a different index name and `customSearchParameters` returns 400 (mixed Mode A + Mode B is not allowed).
#[tokio::test]
async fn create_experiment_mixed_mode_variant_returns_400() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let body = serde_json::json!({
        "name": "Mixed mode bad",
        "variants": [
            { "index": "products", "trafficPercentage": 50, "description": "control" },
            {
                "index": "products_v2",
                "trafficPercentage": 50,
                "description": "variant",
                "customSearchParameters": { "enableSynonyms": false }
            }
        ],
        "endAt": "2099-01-01T00:00:00Z",
        "metrics": [{ "name": "clickThroughRate" }]
    });

    let resp = send_json_request(&app, Method::POST, "/2/abtests", body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Verify that updating a variant with both `queryOverrides` and a different `indexName` returns 400 (mixed Mode A + Mode B is not allowed).
#[tokio::test]
async fn update_experiment_mixed_mode_variant_returns_400() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state);

    let id = create_experiment_and_get_id(&app).await;

    let body = serde_json::json!({
        "name": "Mixed mode update",
        "indexName": "products",
        "trafficSplit": 0.5,
        "control": { "name": "control" },
        "variant": {
            "name": "variant",
            "queryOverrides": { "enableSynonyms": false },
            "indexName": "products_v2"
        },
        "primaryMetric": "ctr"
    });

    let resp = send_json_request(&app, Method::PUT, &format!("/2/abtests/{id}"), body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// --- Promote flow tests ---

fn index_settings_path(tmp: &TempDir, index_name: &str) -> PathBuf {
    tmp.path().join(index_name).join("settings.json")
}

fn load_index_settings(tmp: &TempDir, index_name: &str) -> IndexSettings {
    IndexSettings::load(index_settings_path(tmp, index_name)).unwrap()
}

fn create_index_with_custom_ranking(
    tmp: &TempDir,
    state: &Arc<AppState>,
    index_name: &str,
    custom_ranking: &[&str],
) {
    state.manager.create_tenant(index_name).unwrap();

    let settings_path = index_settings_path(tmp, index_name);
    let mut settings = IndexSettings::load(&settings_path).unwrap();
    settings.custom_ranking = Some(
        custom_ranking
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
    );
    settings.save(&settings_path).unwrap();
}

async fn create_start_conclude_mode_b(
    app: &Router,
    state: &Arc<AppState>,
    promoted: bool,
    winner: &str,
) -> i64 {
    // Ensure both indexes exist on disk with settings
    state.manager.create_tenant("products").unwrap();
    state.manager.create_tenant("products_v2").unwrap();

    let body = serde_json::json!({
        "name": "Mode B promote test",
        "variants": [
            { "index": "products", "trafficPercentage": 50, "description": "control" },
            { "index": "products_v2", "trafficPercentage": 50, "description": "variant" }
        ],
        "endAt": "2099-01-01T00:00:00Z",
        "metrics": [{ "name": "clickThroughRate" }]
    });
    let resp = send_json_request(app, Method::POST, "/2/abtests", body).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let id = body_json(resp).await["abTestID"].as_i64().unwrap();

    // Start
    let resp = send_empty_request(app, Method::POST, &format!("/2/abtests/{id}/start")).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Conclude
    let conclude = serde_json::json!({
        "winner": winner,
        "reason": "Promote test",
        "controlMetric": 0.12,
        "variantMetric": 0.14,
        "confidence": 0.97,
        "significant": true,
        "promoted": promoted
    });
    let resp = send_json_request(
        app,
        Method::POST,
        &format!("/2/abtests/{id}/conclude"),
        conclude,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    id
}

#[tokio::test]
async fn promote_mode_b_copies_variant_settings_to_main_index() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state.clone());

    // Set up variant index with distinct custom_ranking
    state.manager.create_tenant("products").unwrap();
    create_index_with_custom_ranking(&tmp, &state, "products_v2", &["desc(popularity)"]);

    // Verify main index does NOT have custom_ranking yet
    let main_before = load_index_settings(&tmp, "products");
    assert!(main_before.custom_ranking.is_none());

    create_start_conclude_mode_b(&app, &state, true, "variant").await;

    // After promote, main index should have variant's custom_ranking
    state.manager.invalidate_settings_cache("products");
    let main_after = load_index_settings(&tmp, "products");
    assert_eq!(
        main_after.custom_ranking,
        Some(vec!["desc(popularity)".to_string()]),
        "promote should copy variant settings to main index"
    );
}

#[tokio::test]
async fn promote_mode_b_control_winner_does_not_change_settings() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state.clone());

    state.manager.create_tenant("products").unwrap();
    create_index_with_custom_ranking(&tmp, &state, "products_v2", &["desc(popularity)"]);

    create_start_conclude_mode_b(&app, &state, true, "control").await;

    // Main index should remain unchanged (control winner = keep original)
    let main_after = load_index_settings(&tmp, "products");
    assert!(
        main_after.custom_ranking.is_none(),
        "control winner should not copy variant settings"
    );
}

#[tokio::test]
async fn conclude_without_promote_does_not_change_settings() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state.clone());

    state.manager.create_tenant("products").unwrap();
    create_index_with_custom_ranking(&tmp, &state, "products_v2", &["desc(popularity)"]);

    create_start_conclude_mode_b(&app, &state, false, "variant").await;

    // promoted=false → main index untouched
    let main_after = load_index_settings(&tmp, "products");
    assert!(
        main_after.custom_ranking.is_none(),
        "promoted=false should not change main index settings"
    );
}

#[tokio::test]
async fn promote_mode_a_applies_custom_ranking_to_main_index() {
    let tmp = TempDir::new().unwrap();
    let state = make_experiments_state(&tmp);
    let app = app_router(state.clone());

    state.manager.create_tenant("products").unwrap();

    // Create Mode A experiment with custom_ranking override
    let body = serde_json::json!({
        "name": "Mode A promote test",
        "variants": [
            { "index": "products", "trafficPercentage": 50, "description": "control" },
            {
                "index": "products",
                "trafficPercentage": 50,
                "description": "variant",
                "customSearchParameters": {
                    "customRanking": ["desc(sales)", "asc(price)"],
                    "removeWordsIfNoResults": "lastWords"
                }
            }
        ],
        "endAt": "2099-01-01T00:00:00Z",
        "metrics": [{ "name": "clickThroughRate" }]
    });
    let resp = send_json_request(&app, Method::POST, "/2/abtests", body).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let id = body_json(resp).await["abTestID"].as_i64().unwrap();

    // Start
    send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;

    // Conclude with promote
    let conclude = serde_json::json!({
        "winner": "variant",
        "reason": "Mode A promote",
        "controlMetric": 0.12,
        "variantMetric": 0.15,
        "confidence": 0.98,
        "significant": true,
        "promoted": true
    });
    let resp = send_json_request(
        &app,
        Method::POST,
        &format!("/2/abtests/{id}/conclude"),
        conclude,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Main index should now have custom_ranking and remove_words_if_no_results from overrides
    state.manager.invalidate_settings_cache("products");
    let main_after = load_index_settings(&tmp, "products");
    assert_eq!(
        main_after.custom_ranking,
        Some(vec!["desc(sales)".to_string(), "asc(price)".to_string()]),
        "promote should apply custom_ranking from query overrides"
    );
    assert_eq!(
        main_after.remove_words_if_no_results, "lastWords",
        "promote should apply remove_words_if_no_results from query overrides"
    );
}

// ── Guard Rail Tests ────────────────────────────────────────────

/// Verify that a guard-rail alert is emitted when the variant's primary metric drops significantly (≥ ~20%) relative to control.
#[test]
fn build_results_response_includes_guard_rail_alert_when_triggered() {
    // Control CTR = 0.20, variant CTR = 0.10 → 50% drop → should trigger
    let now = chrono::Utc::now().timestamp_millis();
    let experiment = Experiment {
        id: "exp-guard-1".to_string(),
        name: "Guard rail test".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: now - 1_000,
        started_at: Some(now - 60_000),
        ended_at: None,
        stopped_at: None,
        minimum_days: 14,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let metrics = metrics::ExperimentMetrics {
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: 100,
            users: 10,
            clicks: 20,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.20,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(2.0, 10.0); 10],
            per_user_conversion_rates: vec![(0.0, 10.0); 10],
            per_user_zero_result_rates: vec![(0.0, 10.0); 10],
            per_user_abandonment_rates: vec![(0.0, 10.0); 10],
            per_user_revenues: vec![0.0; 10],
            per_user_ids: (0..10).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: 100,
            users: 10,
            clicks: 10,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.10,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(1.0, 10.0); 10],
            per_user_conversion_rates: vec![(0.0, 10.0); 10],
            per_user_zero_result_rates: vec![(0.0, 10.0); 10],
            per_user_abandonment_rates: vec![(0.0, 10.0); 10],
            per_user_revenues: vec![0.0; 10],
            per_user_ids: (0..10).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);
    assert!(
        !response.guard_rail_alerts.is_empty(),
        "guard rail should trigger when variant drops 50%"
    );
    let alert = &response.guard_rail_alerts[0];
    assert_eq!(alert.metric_name, "ctr");
    assert!(
        alert.drop_pct > 40.0,
        "expected ~50% drop, got {}",
        alert.drop_pct
    );
}

/// Verify that no guard-rail alert is emitted when the variant's primary metric is equal to or better than control.
#[test]
fn build_results_response_no_guard_rail_alert_when_healthy() {
    // Control CTR = 0.10, variant CTR = 0.12 → variant better → no alert
    let now = chrono::Utc::now().timestamp_millis();
    let experiment = Experiment {
        id: "exp-guard-2".to_string(),
        name: "Healthy test".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: now - 1_000,
        started_at: Some(now - 60_000),
        ended_at: None,
        stopped_at: None,
        minimum_days: 14,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let metrics = metrics::ExperimentMetrics {
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: 100,
            users: 10,
            clicks: 10,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.10,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(1.0, 10.0); 10],
            per_user_conversion_rates: vec![(0.0, 10.0); 10],
            per_user_zero_result_rates: vec![(0.0, 10.0); 10],
            per_user_abandonment_rates: vec![(0.0, 10.0); 10],
            per_user_revenues: vec![0.0; 10],
            per_user_ids: (0..10).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: 100,
            users: 10,
            clicks: 12,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.12,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(1.2, 10.0); 10],
            per_user_conversion_rates: vec![(0.0, 10.0); 10],
            per_user_zero_result_rates: vec![(0.0, 10.0); 10],
            per_user_abandonment_rates: vec![(0.0, 10.0); 10],
            per_user_revenues: vec![0.0; 10],
            per_user_ids: (0..10).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 0.0,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);
    assert!(
        response.guard_rail_alerts.is_empty(),
        "no guard rail alert expected when variant is healthy"
    );
}

// ── MeanClickRank handler wiring ────────────────────────────────

/// Verify that the results response includes `meanClickRank` for each arm, reflecting the values provided by the metrics layer.
#[test]
fn results_includes_mean_click_rank_per_arm() {
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (15 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-mcr-1".to_string(),
        name: "Click rank test".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: now - 20 * 24 * 60 * 60 * 1000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 14,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let metrics = metrics::ExperimentMetrics {
        control: metrics::ArmMetrics {
            arm_name: "control".to_string(),
            searches: 200,
            users: 100,
            clicks: 80,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.40,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(0.8, 2.0); 100],
            per_user_conversion_rates: vec![(0.0, 2.0); 100],
            per_user_zero_result_rates: vec![(0.0, 2.0); 100],
            per_user_abandonment_rates: vec![(0.0, 2.0); 100],
            per_user_revenues: vec![0.0; 100],
            per_user_ids: (0..100).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 3.5,
        },
        variant: metrics::ArmMetrics {
            arm_name: "variant".to_string(),
            searches: 200,
            users: 100,
            clicks: 80,
            conversions: 0,
            revenue: 0.0,
            zero_result_searches: 0,
            abandoned_searches: 0,
            ctr: 0.40,
            conversion_rate: 0.0,
            revenue_per_search: 0.0,
            zero_result_rate: 0.0,
            abandonment_rate: 0.0,
            per_user_ctrs: vec![(0.8, 2.0); 100],
            per_user_conversion_rates: vec![(0.0, 2.0); 100],
            per_user_zero_result_rates: vec![(0.0, 2.0); 100],
            per_user_abandonment_rates: vec![(0.0, 2.0); 100],
            per_user_revenues: vec![0.0; 100],
            per_user_ids: (0..100).map(|i| format!("u{i}")).collect(),
            mean_click_rank: 2.1,
        },
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let response = build_results_response(&experiment, Some(&metrics), None, None);

    assert!(
        (response.control.mean_click_rank - 3.5).abs() < 0.001,
        "control mean_click_rank expected 3.5, got {}",
        response.control.mean_click_rank
    );
    assert!(
        (response.variant.mean_click_rank - 2.1).abs() < 0.001,
        "variant mean_click_rank expected 2.1, got {}",
        response.variant.mean_click_rank
    );
    // Variant has lower (better) click rank
    assert!(response.variant.mean_click_rank < response.control.mean_click_rank);
}

/// Verify that when CUPED adjustment cannot reduce variance (e.g. uncorrelated covariates), the safety check falls back to raw z-score and p-value values and reports `cupedApplied: false`.
#[test]
fn build_results_response_cuped_safety_fallback_when_adjusted_variance_not_lower() {
    // Construct data where CUPED cannot lower variance (adj_var >= raw_var).
    // The safety check should fall back to raw values.
    let now = chrono::Utc::now().timestamp_millis();
    let started_at = now - (3 * 24 * 60 * 60 * 1000);
    let experiment = Experiment {
        id: "exp-cuped-safety-1".to_string(),
        name: "CUPED safety fallback".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(Default::default()),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: started_at - 1_000,
        started_at: Some(started_at),
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let users = 200;
    let searches_per_user = 100.0;

    let mut control_ids = Vec::with_capacity(users);
    let mut variant_ids = Vec::with_capacity(users);
    let mut control_samples = Vec::with_capacity(users);
    let mut variant_samples = Vec::with_capacity(users);
    let mut covariates = std::collections::HashMap::new();

    // Very tight, low-variance outcome data with uncorrelated covariates.
    // Theta is approximately zero, so adjusted variance is not lower than raw.
    for i in 0..users {
        // Uniform outcome with near-zero variance
        let clicks = 50.0;
        let control_id = format!("c{i}");
        let variant_id = format!("v{i}");

        // Uncorrelated large-magnitude covariate: alternating extreme values
        let covariate = if i % 2 == 0 { 1000.0 } else { -1000.0 };
        covariates.insert(control_id.clone(), covariate);
        covariates.insert(variant_id.clone(), covariate);

        control_ids.push(control_id);
        variant_ids.push(variant_id);
        control_samples.push((clicks, searches_per_user));
        variant_samples.push((clicks, searches_per_user));
    }

    let metrics = metrics::ExperimentMetrics {
        control: build_ctr_arm_metrics("control", control_samples, control_ids),
        variant: build_ctr_arm_metrics("variant", variant_samples, variant_ids),
        outlier_users_excluded: 0,
        no_stable_id_queries: 0,
        winsorization_cap_applied: None,
    };

    let raw_response = build_results_response(&experiment, Some(&metrics), None, None);
    let cuped_response =
        build_results_response(&experiment, Some(&metrics), Some(&covariates), None);

    // Safety check should have detected that CUPED doesn't help and fallen back
    assert!(
        !cuped_response.cuped_applied,
        "CUPED should NOT be applied when adjusted variance >= raw variance"
    );

    // z-scores should be identical to raw since we fell back
    let raw_sig = raw_response
        .significance
        .expect("significance should be present");
    let cuped_sig = cuped_response
        .significance
        .expect("significance should be present");

    assert!(
        (cuped_sig.z_score - raw_sig.z_score).abs() < f64::EPSILON,
        "z-score should be unchanged when CUPED safety fallback triggers (raw={}, cuped={})",
        raw_sig.z_score,
        cuped_sig.z_score
    );
}

#[test]
fn resolve_experiment_index_names_deduplicates_variant_index() {
    let experiment = Experiment {
        id: "exp-index-name-helpers".to_string(),
        name: "index name helpers".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Draft,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: None,
            index_name: Some("products".to_string()),
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: chrono::Utc::now().timestamp_millis(),
        started_at: None,
        ended_at: None,
        stopped_at: None,
        minimum_days: 1,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    };

    let index_names = resolve_experiment_index_names(&experiment);
    assert_eq!(index_names, vec!["products".to_string()]);
}
