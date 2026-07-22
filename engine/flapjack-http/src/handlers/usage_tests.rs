use super::*;
use crate::test_helpers::body_json;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware,
    routing::get,
    Router,
};
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use tower::ServiceExt;

/// Create a minimal `AppState` for usage tests with no persistence layer and empty counters.
fn make_state(tmp: &TempDir) -> Arc<AppState> {
    let mut state = crate::test_helpers::TestStateBuilder::new(tmp).build();
    state.metrics_state = None;
    Arc::new(state)
}

fn usage_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/1/usage/:statistic", get(usage_global))
        .route("/1/usage/:statistic/:indexName", get(usage_per_index))
        .with_state(state)
}

async fn get_usage(app: Router, path: &str) -> axum::response::Response {
    let req = Request::builder().uri(path).body(Body::empty()).unwrap();
    app.oneshot(req).await.unwrap()
}

fn usage_test_document(id: &str) -> flapjack::types::Document {
    flapjack::types::Document {
        id: id.to_string(),
        fields: std::collections::HashMap::from([(
            "title".to_string(),
            flapjack::types::FieldValue::Text(format!("Document {id}")),
        )]),
    }
}

/// Create and index test documents so live segment counts are observable.
async fn seed_loaded_documents(state: &Arc<AppState>, index_name: &str, count: u64) {
    state.manager.create_tenant(index_name).unwrap();
    let docs = (0..count)
        .map(|idx| usage_test_document(&format!("{index_name}_{idx}")))
        .collect();
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();
    assert_eq!(
        state.manager.tenant_doc_count(index_name),
        Some(count),
        "test setup must create the expected live segment document count"
    );
}

// ── Red tests ──

/// Verify the `search_operations` statistic returns a single data point with the correct count and a numeric timestamp.
#[tokio::test]
async fn usage_search_operations_returns_daily_counts() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    state
        .usage_counters
        .entry("my_index".to_string())
        .or_default()
        .search_count
        .fetch_add(1, Ordering::Relaxed);

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/search_operations").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["search_operations"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["v"].as_u64().unwrap(), 1);
    assert!(arr[0]["t"].is_number());
}

/// Verify the `total_write_operations` statistic returns the accumulated write count from live counters.
#[tokio::test]
async fn usage_total_write_operations_returns_counts() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    state
        .usage_counters
        .entry("products".to_string())
        .or_default()
        .write_count
        .fetch_add(3, Ordering::Relaxed);

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/total_write_operations").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["total_write_operations"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["v"].as_u64().unwrap(), 3);
}

/// Verify the `records` statistic reflects the `documents_indexed_total` counter value.
#[tokio::test]
async fn usage_records_returns_document_count() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    state
        .usage_counters
        .entry("catalog".to_string())
        .or_default()
        .documents_indexed_total
        .fetch_add(42, Ordering::Relaxed);

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/records").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["records"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["v"].as_u64().unwrap(), 42);
}

/// Verify `documents_count` returns the loaded segment document count for one index.
#[tokio::test]
async fn usage_documents_count_per_index_returns_live_segment_count() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    seed_loaded_documents(&state, "products", 3).await;

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/documents_count/products").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["documents_count"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    // Hand calculation: products has exactly 3 live documents.
    assert_eq!(arr[0]["v"].as_u64().unwrap(), 3);
}

/// Verify global `documents_count` sums all loaded indexes.
#[tokio::test]
async fn usage_documents_count_global_sums_loaded_indexes() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    seed_loaded_documents(&state, "products", 2).await;
    seed_loaded_documents(&state, "articles", 3).await;

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/documents_count").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["documents_count"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    // Hand calculation: products 2 + articles 3 == 5.
    assert_eq!(arr[0]["v"].as_u64().unwrap(), 5);
}

#[tokio::test]
async fn usage_returns_empty_for_no_activity() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    // usage_counters is empty — no requests have been tracked

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/search_operations").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["search_operations"].as_array().unwrap();
    assert!(arr.is_empty(), "expected empty array when no activity");
}

/// Verify that `GET /1/usage/:stat/:indexName` returns only the named index's counter, excluding other indexes.
#[tokio::test]
async fn usage_per_index_endpoint() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    state
        .usage_counters
        .entry("index_a".to_string())
        .or_default()
        .search_count
        .fetch_add(5, Ordering::Relaxed);

    // index_b's count must not bleed into index_a's result
    state
        .usage_counters
        .entry("index_b".to_string())
        .or_default()
        .search_count
        .fetch_add(99, Ordering::Relaxed);

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/search_operations/index_a").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["search_operations"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(
        arr[0]["v"].as_u64().unwrap(),
        5,
        "per-index result must only include that index's count"
    );
}

/// Verify that a comma-separated statistic path like `search_operations,total_write_operations` returns independent arrays for each requested metric.
#[tokio::test]
async fn usage_comma_separated_statistics() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    {
        let entry = state.usage_counters.entry("idx".to_string()).or_default();
        entry.search_count.fetch_add(2, Ordering::Relaxed);
        entry.write_count.fetch_add(7, Ordering::Relaxed);
    }

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/search_operations,total_write_operations").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert!(json["search_operations"].is_array());
    assert!(json["total_write_operations"].is_array());
    assert_eq!(json["search_operations"][0]["v"].as_u64().unwrap(), 2);
    assert_eq!(json["total_write_operations"][0]["v"].as_u64().unwrap(), 7);
}

/// Verify comma-separated counter and gauge stats are returned independently.
#[tokio::test]
async fn usage_comma_separated_search_operations_and_documents_count() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    state
        .usage_counters
        .entry("idx".to_string())
        .or_default()
        .search_count
        .fetch_add(2, Ordering::Relaxed);
    seed_loaded_documents(&state, "idx", 4).await;

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/search_operations,documents_count").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let search = json["search_operations"].as_array().unwrap();
    let documents = json["documents_count"].as_array().unwrap();
    assert_eq!(search.len(), 1);
    assert_eq!(documents.len(), 1);
    assert_eq!(search[0]["v"].as_u64().unwrap(), 2);
    // Hand calculation: idx has exactly 4 live documents.
    assert_eq!(documents[0]["v"].as_u64().unwrap(), 4);
}

#[tokio::test]
async fn usage_comma_separated_live_points_share_timestamp() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    state
        .usage_counters
        .entry("idx".to_string())
        .or_default()
        .search_count
        .fetch_add(2, Ordering::Relaxed);
    seed_loaded_documents(&state, "idx", 4).await;

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/search_operations,documents_count").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let search = json["search_operations"].as_array().unwrap();
    let documents = json["documents_count"].as_array().unwrap();
    assert_eq!(search.len(), 1);
    assert_eq!(documents.len(), 1);
    assert_eq!(
        search[0]["t"].as_i64().unwrap(),
        documents[0]["t"].as_i64().unwrap(),
        "one response must publish one live timestamp across all requested statistics"
    );
}

#[tokio::test]
async fn usage_unknown_statistic_returns_error() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/bogus_stat").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let json = body_json(resp).await;
    assert_eq!(json["status"].as_u64().unwrap(), 400);
    assert!(
        json["message"].as_str().unwrap().contains("bogus_stat"),
        "error message should name the unknown stat"
    );
}

/// Verify that an API key lacking the `usage` ACL receives a 403 Forbidden response from the usage endpoint.
#[tokio::test]
async fn usage_requires_usage_acl() {
    use crate::auth::{authenticate_and_authorize, ApiKey, KeyStore, RateLimiter};

    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    let ks = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let (_, search_key) = ks.create_key(ApiKey {
        hash: String::new(),
        salt: String::new(),
        hmac_key: None,
        created_at: 0,
        acl: vec!["search".to_string()],
        description: "search-only".to_string(),
        indexes: vec![],
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: vec![],
        restrict_sources: None,
        validity: 0,
    });

    let ks_clone = ks.clone();
    let rl = RateLimiter::new();
    let app = Router::new()
        .route("/1/usage/:statistic", get(usage_global))
        .with_state(state)
        .layer(middleware::from_fn(
            move |mut req: axum::extract::Request, next: middleware::Next| {
                let ks = ks_clone.clone();
                let rl = rl.clone();
                async move {
                    req.extensions_mut().insert(ks);
                    req.extensions_mut().insert(rl);
                    authenticate_and_authorize(req, next, false).await
                }
            },
        ));

    let req = Request::builder()
        .uri("/1/usage/search_operations")
        .header("x-algolia-application-id", "test")
        .header("x-algolia-api-key", search_key.as_str())
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "key without usage ACL must get 403"
    );

    let json = body_json(resp).await;
    assert!(
        json["message"].as_str().is_some(),
        "error must have message"
    );
}

/// Verify that `startDate` and `endDate` query parameters are accepted without causing a parse error.
#[tokio::test]
async fn usage_date_range_params_accepted() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    let app = usage_router(state);
    let resp = get_usage(
        app,
        "/1/usage/search_operations?startDate=2026-02-25&endDate=2026-02-26",
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "date range params must not cause a parse error"
    );
}

/// Verify the response shape matches the Algolia usage API: top-level stat name mapping to an array of `{"t": <epoch_ms>, "v": <integer>}` objects.
#[tokio::test]
async fn usage_response_format_matches_algolia() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    state
        .usage_counters
        .entry("idx".to_string())
        .or_default()
        .search_count
        .fetch_add(1, Ordering::Relaxed);

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/search_operations").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;

    // Top-level key is the statistic name mapping to an array
    let arr = json["search_operations"]
        .as_array()
        .expect("search_operations must be an array");
    assert_eq!(arr.len(), 1);

    let point = &arr[0];
    // "t" must be a positive integer (epoch milliseconds)
    let t = point["t"].as_i64().expect("t must be an integer");
    assert!(t > 0, "t must be a positive epoch-millisecond timestamp");

    // "v" must be an integer
    assert!(
        point["v"].is_u64() || point["v"].is_i64(),
        "v must be an integer"
    );
}

/// Verify absent storage cache data is represented as unknown instead of zero.
#[tokio::test]
async fn usage_storage_bytes_absent_cache_is_explicit_not_zero() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    state
        .usage_counters
        .entry("products".to_string())
        .or_default()
        .search_count
        .fetch_add(1, Ordering::Relaxed);

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/storage_bytes/products").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["storage_bytes"].as_array().unwrap();
    assert!(
        arr.is_empty(),
        "missing metrics cache must be represented as unknown, not v=0"
    );
}

/// Verify per-index `storage_bytes` reads the background metrics cache.
#[tokio::test]
async fn usage_storage_bytes_per_index_reads_metrics_cache() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let metrics_state = state.metrics_state.as_ref().unwrap();
    metrics_state.storage_gauges.clear();
    metrics_state
        .storage_gauges
        .insert("products".to_string(), 12_345);
    metrics_state
        .storage_gauges
        .insert("other".to_string(), 6_789);

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/storage_bytes/products").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["storage_bytes"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["v"].as_u64().unwrap(), 12_345);
}

/// Verify global `storage_bytes` sums the background metrics cache.
#[tokio::test]
async fn usage_storage_bytes_global_sums_metrics_cache() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let metrics_state = state.metrics_state.as_ref().unwrap();
    metrics_state.storage_gauges.clear();
    metrics_state
        .storage_gauges
        .insert("products".to_string(), 12_345);
    metrics_state
        .storage_gauges
        .insert("articles".to_string(), 6_789);

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/storage_bytes").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["storage_bytes"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    // Hand calculation: products 12_345 + articles 6_789 == 19_134.
    assert_eq!(arr[0]["v"].as_u64().unwrap(), 19_134);
}

/// Verify explicit zero document counts are emitted instead of treated as missing.
#[tokio::test]
async fn usage_documents_count_explicit_zero_is_emitted() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    state.manager.create_tenant("empty").unwrap();
    assert_eq!(state.manager.tenant_doc_count("empty"), Some(0));

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/documents_count/empty").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["documents_count"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["v"].as_u64().unwrap(), 0);
}

/// Verify explicit zero storage cache entries are emitted instead of treated as missing.
#[tokio::test]
async fn usage_storage_bytes_explicit_zero_is_emitted() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let metrics_state = state.metrics_state.as_ref().unwrap();
    metrics_state.storage_gauges.clear();
    metrics_state.storage_gauges.insert("empty".to_string(), 0);

    let app = usage_router(state);
    let resp = get_usage(app, "/1/usage/storage_bytes/empty").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["storage_bytes"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["v"].as_u64().unwrap(), 0);
}

// ── Date range + persistence tests ──

/// Create an `AppState` wired to the given `UsagePersistence` instance so tests can exercise historical snapshot loading.
fn make_state_with_persistence(
    tmp: &TempDir,
    persistence: Arc<crate::usage_persistence::UsagePersistence>,
) -> Arc<AppState> {
    let mut state = crate::test_helpers::TestStateBuilder::new(tmp).build();
    state.metrics_state = None;
    state.usage_persistence = Some(persistence);
    Arc::new(state)
}

/// Verify historical snapshots without gauge fields do not fabricate zeroes.
#[tokio::test]
async fn usage_documents_count_no_fabricated_historical_zero() {
    use crate::usage_persistence::UsagePersistence;

    let tmp = TempDir::new().unwrap();
    let persistence = Arc::new(UsagePersistence::new(tmp.path()).unwrap());

    let counters: dashmap::DashMap<String, crate::usage_middleware::TenantUsageCounters> =
        dashmap::DashMap::new();
    counters
        .entry("idx".to_string())
        .or_default()
        .search_count
        .fetch_add(7, Ordering::Relaxed);
    persistence.save_snapshot("2026-02-24", &counters).unwrap();

    let state = make_state_with_persistence(&tmp, persistence);
    let app = usage_router(state);
    let resp = get_usage(
        app,
        "/1/usage/documents_count?startDate=2026-02-24&endDate=2026-02-24",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["documents_count"].as_array().unwrap();
    assert!(
        arr.is_empty(),
        "historical snapshots without gauge data must stay unknown, not v=0"
    );
}

/// Verify that querying a past date range loads only the matching persisted snapshots and returns the correct values.
#[tokio::test]
async fn usage_date_range_returns_historical_data() {
    use crate::usage_persistence::UsagePersistence;

    let tmp = TempDir::new().unwrap();
    let persistence = Arc::new(UsagePersistence::new(tmp.path()).unwrap());

    // Persist snapshots for 3 separate days
    for (date, count) in [
        ("2026-02-22", 10u64),
        ("2026-02-23", 20),
        ("2026-02-24", 30),
    ] {
        let c: dashmap::DashMap<String, crate::usage_middleware::TenantUsageCounters> =
            dashmap::DashMap::new();
        c.entry("idx".to_string())
            .or_default()
            .search_count
            .fetch_add(count, Ordering::Relaxed);
        persistence.save_snapshot(date, &c).unwrap();
    }

    let state = make_state_with_persistence(&tmp, persistence);
    let app = usage_router(state);

    // Query spanning only 2 of the 3 persisted dates (both before today)
    let resp = get_usage(
        app,
        "/1/usage/search_operations?startDate=2026-02-22&endDate=2026-02-23",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["search_operations"].as_array().unwrap();
    assert_eq!(
        arr.len(),
        2,
        "should return exactly 2 historical data points"
    );

    let values: Vec<u64> = arr.iter().map(|p| p["v"].as_u64().unwrap()).collect();
    assert!(values.contains(&10), "day 1 value must be present");
    assert!(values.contains(&20), "day 2 value must be present");
}

/// Verify corrupt persisted usage history returns a server error instead of an empty success payload.
#[tokio::test]
async fn usage_date_range_returns_internal_error_for_corrupt_history() {
    use crate::usage_persistence::UsagePersistence;

    let tmp = TempDir::new().unwrap();
    let persistence = Arc::new(UsagePersistence::new(tmp.path()).unwrap());
    std::fs::write(
        tmp.path().join("_usage/2026-02-22.json"),
        "{ not valid json }",
    )
    .unwrap();

    let state = make_state_with_persistence(&tmp, persistence);
    let app = usage_router(state);
    let resp = get_usage(
        app,
        "/1/usage/search_operations?startDate=2026-02-22&endDate=2026-02-22",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let json = body_json(resp).await;
    assert_eq!(json["status"].as_u64().unwrap(), 500);
    assert_eq!(
        json["message"].as_str().unwrap(),
        "Failed to load usage history"
    );
}

/// Verify that the per-index endpoint only returns the filtered index's value from a persisted snapshot containing multiple indexes.
#[tokio::test]
async fn usage_date_range_per_index_filters_correctly() {
    use crate::usage_persistence::UsagePersistence;

    let tmp = TempDir::new().unwrap();
    let persistence = Arc::new(UsagePersistence::new(tmp.path()).unwrap());

    // Snapshot has both idx_a and idx_b
    let c: dashmap::DashMap<String, crate::usage_middleware::TenantUsageCounters> =
        dashmap::DashMap::new();
    c.entry("idx_a".to_string())
        .or_default()
        .search_count
        .fetch_add(10, Ordering::Relaxed);
    c.entry("idx_b".to_string())
        .or_default()
        .search_count
        .fetch_add(20, Ordering::Relaxed);
    persistence.save_snapshot("2026-02-24", &c).unwrap();

    let state = make_state_with_persistence(&tmp, persistence);
    let app = usage_router(state);

    let resp = get_usage(
        app,
        "/1/usage/search_operations/idx_a?startDate=2026-02-24&endDate=2026-02-24",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["search_operations"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(
        arr[0]["v"].as_u64().unwrap(),
        10,
        "per-index filter must only return idx_a's value"
    );
}

/// Verify daily granularity produces exactly one data point per calendar day, each stamped at midnight UTC and ordered chronologically.
#[tokio::test]
async fn usage_granularity_daily_returns_one_point_per_day() {
    use crate::usage_persistence::UsagePersistence;

    let tmp = TempDir::new().unwrap();
    let persistence = Arc::new(UsagePersistence::new(tmp.path()).unwrap());

    // Save two days of data (both before today)
    for (date, count) in [("2026-02-23", 1u64), ("2026-02-24", 2u64)] {
        let c: dashmap::DashMap<String, crate::usage_middleware::TenantUsageCounters> =
            dashmap::DashMap::new();
        c.entry("idx".to_string())
            .or_default()
            .search_count
            .fetch_add(count, Ordering::Relaxed);
        persistence.save_snapshot(date, &c).unwrap();
    }

    let state = make_state_with_persistence(&tmp, persistence);
    let app = usage_router(state);

    let resp = get_usage(
        app,
        "/1/usage/search_operations?startDate=2026-02-23&endDate=2026-02-24&granularity=daily",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["search_operations"].as_array().unwrap();
    assert_eq!(
        arr.len(),
        2,
        "daily granularity returns one point per calendar day with data"
    );

    // Timestamps must be midnight UTC (divisible by 86_400_000 ms)
    for point in arr {
        let t = point["t"].as_i64().unwrap();
        assert_eq!(t % 86_400_000, 0, "each t must be at midnight UTC");
    }

    // Points must be in chronological order
    let t0 = arr[0]["t"].as_i64().unwrap();
    let t1 = arr[1]["t"].as_i64().unwrap();
    assert!(t1 > t0, "data points must be in chronological order");
}

#[path = "usage_historical_gauge_tests.rs"]
mod historical_gauge_tests;
