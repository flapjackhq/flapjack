//! Exercise usage attribution through the real authenticated router and search handlers.
use std::collections::{BTreeMap, HashMap};
use std::sync::{atomic::Ordering, Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::auth::{ApiKey, KeyStore};
use crate::handlers::AppState;
use crate::test_helpers::{body_json, TestStateBuilder};

// The same logical index belongs to two customers, with distinct physical UIDs.
const A: &str = "11111111111111111111111111111111_products";
const B: &str = "22222222222222222222222222222222_products";
const ADMIN: &str = "batch-usage-test-admin";

async fn fixture(tmp: &TempDir) -> (Router, Arc<AppState>, Arc<KeyStore>) {
    let state = TestStateBuilder::new(tmp).with_analytics().build_shared();
    for index in [A, B] {
        state.manager.create_tenant(index).unwrap();
        let settings = flapjack::index::settings::IndexSettings {
            attributes_for_faceting: vec!["searchable(category)".into()],
            ..Default::default()
        };
        settings
            .save(tmp.path().join(index).join("settings.json"))
            .unwrap();
        state.manager.invalidate_settings_cache(index);
        state
            .manager
            .add_documents_sync(
                index,
                vec![flapjack::Document {
                    id: "one".into(),
                    fields: HashMap::from([
                        ("title".into(), flapjack::FieldValue::Text("shoe".into())),
                        (
                            "category".into(),
                            flapjack::FieldValue::Text("footwear".into()),
                        ),
                    ]),
                }],
            )
            .await
            .unwrap();
    }
    let keys = Arc::new(KeyStore::load_or_create(tmp.path(), ADMIN));
    let analytics =
        flapjack::analytics::AnalyticsCollector::new(flapjack::analytics::AnalyticsConfig {
            enabled: false,
            data_dir: tmp.path().join("analytics"),
            flush_interval_secs: 60,
            flush_size: 1000,
            retention_days: 30,
        });
    let app = crate::router::build_router(
        state.clone(),
        Some(keys.clone()),
        analytics,
        Arc::new(crate::middleware::TrustedProxyMatcher::from_optional_csv(None).unwrap()),
        tmp.path(),
        crate::router::RouterConfig {
            cors_mode: crate::startup::CorsMode::LoopbackOnly,
            disable_dashboard: true,
            replication_api_key: None,
            api_profile: crate::api_profile::ApiProfile::Full,
        },
    );
    (app, state, keys)
}

async fn post(app: &Router, path: &str, key: &str, body: Value) -> axum::response::Response {
    let body = body.to_string();
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .header("content-length", body.len())
                .header("x-algolia-api-key", key)
                .header("x-algolia-application-id", "batch-usage-app")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

fn counts(state: &AppState) -> BTreeMap<String, u64> {
    state
        .usage_counters
        .iter()
        .map(|entry| {
            (
                entry.key().clone(),
                entry.search_count.load(Ordering::Relaxed),
            )
        })
        .collect()
}

#[tokio::test]
async fn batch_usage_counts_operands_by_physical_uid_and_preserves_rollup() {
    let tmp = TempDir::new().unwrap();
    let (app, state, _) = fixture(&tmp).await;
    let response = post(
        &app,
        "/1/indexes/*/queries",
        ADMIN,
        json!({"requests": [
            {"indexName": A, "query": "shoe"},
            {"indexName": A, "query": "shoe"},
            {"indexName": B, "query": "shoe"}
        ]}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    for result in body["results"].as_array().unwrap() {
        assert_eq!(result["nbHits"], 1);
    }
    assert_eq!(
        counts(&state),
        BTreeMap::from([(A.into(), 2), (B.into(), 1)])
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("x-algolia-api-key", ADMIN)
                .header("x-algolia-application-id", "batch-usage-app")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    for (index, count) in [(A, 2), (B, 1)] {
        assert!(
            text.lines().any(|line| line
                == format!("flapjack_search_requests_total{{index=\"{index}\"}} {count}")),
            "{text}"
        );
    }
    assert!(!text.contains("index=\"*\""));
    crate::usage_persistence::UsagePersistence::new(tmp.path())
        .unwrap()
        .rollup("2026-10-17", &state.usage_counters)
        .unwrap();
    assert_eq!(
        counts(&state),
        BTreeMap::from([(A.into(), 0), (B.into(), 0)])
    );
    // The mounted batch route also accepts a named envelope. Only its operands
    // own usage, even when the path itself looks like a physical index name.
    let response = post(
        &app,
        "/1/indexes/batch-envelope/queries",
        ADMIN,
        json!({"requests": [{"indexName": B, "query": "shoe"}]}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        counts(&state),
        BTreeMap::from([(A.into(), 0), (B.into(), 1)])
    );
}

#[tokio::test]
async fn batch_usage_rejects_whole_malformed_batch_before_dispatch() {
    let tmp = TempDir::new().unwrap();
    let (app, state, _) = fixture(&tmp).await;
    for invalid in [
        json!({"indexName": B, "type": "invalid"}),
        json!({"query": "missing index"}),
        json!({"indexName": "../invalid"}),
        json!({"indexName": B, "hitsPerPage": -1}),
    ] {
        let response = post(
            &app,
            "/1/indexes/*/queries",
            ADMIN,
            json!({"requests": [{"indexName": A, "query": "shoe"}, invalid]}),
        )
        .await;
        assert!(response.status().is_client_error());
        assert!(
            state.usage_counters.is_empty(),
            "no operand or envelope may be counted"
        );
    }
}

#[tokio::test]
async fn batch_usage_rejects_unauthorized_later_operand_before_dispatch() {
    let tmp = TempDir::new().unwrap();
    let (app, state, keys) = fixture(&tmp).await;
    let (_, key) = keys.create_key(ApiKey {
        hash: String::new(),
        salt: String::new(),
        hmac_key: None,
        created_at: 0,
        acl: vec!["search".into()],
        description: "batch usage restricted fixture".into(),
        indexes: vec![A.into()],
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: vec![],
        validity: 0,
        restrict_sources: None,
    });
    let response = post(
        &app,
        "/1/indexes/*/queries",
        &key,
        json!({"requests": [
            {"indexName": A, "query": "shoe"}, {"indexName": B, "query": "shoe"}
        ]}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(state.usage_counters.is_empty());
    let response = post(
        &app,
        "/1/indexes/*/queries",
        &key,
        json!({"requests": [{"indexName": A, "query": "shoe"}]}),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the key really permits A"
    );
    assert_eq!(counts(&state), BTreeMap::from([(A.into(), 1)]));
}

#[tokio::test]
async fn batch_usage_early_stop_does_not_count_skipped_operands() {
    let tmp = TempDir::new().unwrap();
    let (app, state, _) = fixture(&tmp).await;
    let response = post(
        &app,
        "/1/indexes/*/queries",
        ADMIN,
        json!({
            "strategy": "stopIfEnoughMatches", "requests": [
                {"indexName": A, "query": "shoe", "hitsPerPage": 1},
                {"indexName": A, "query": "shoe"}, {"indexName": B, "query": "shoe"}
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["results"][0]["nbHits"], 1);
    assert_eq!(body["results"][1]["processed"], false);
    assert_eq!(body["results"][2]["processed"], false);
    assert_eq!(counts(&state), BTreeMap::from([(A.into(), 1)]));
}

#[tokio::test]
async fn batch_usage_facets_and_direct_searches_count_each_attempt_once() {
    let tmp = TempDir::new().unwrap();
    let (app, state, _) = fixture(&tmp).await;
    let response = post(
        &app,
        "/1/indexes/*/queries",
        ADMIN,
        json!({"requests": [
            {"indexName": A, "query": "shoe"},
            {"indexName": A, "type": "facet", "facet": "category", "facetQuery": "foot"}
        ]}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["results"][1]["facetHits"][0]["value"], "footwear");
    assert_eq!(counts(&state), BTreeMap::from([(A.into(), 2)]));
    let response = post(
        &app,
        &format!("/1/indexes/{A}/facets/category/query"),
        ADMIN,
        json!({"facetQuery": "foot"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(counts(&state), BTreeMap::from([(A.into(), 3)]));
    let response = post(
        &app,
        &format!("/1/indexes/{A}/query"),
        ADMIN,
        json!({"query": "shoe"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(counts(&state), BTreeMap::from([(A.into(), 4)]));
}

#[tokio::test]
async fn batch_usage_failed_executed_attempt_is_counted_like_direct_search() {
    let tmp = TempDir::new().unwrap();
    let (app, state, _) = fixture(&tmp).await;
    let response = post(
        &app,
        "/1/indexes/*/queries",
        ADMIN,
        json!({"requests": [{"indexName": "missing"}]}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(counts(&state), BTreeMap::from([("missing".into(), 1)]));
    let response = post(&app, "/1/indexes/missing/query", ADMIN, json!({})).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(counts(&state), BTreeMap::from([("missing".into(), 2)]));
}
