use super::*;
use crate::auth::{ApiKey, KeyStore};
use crate::test_helpers::{body_json, send_empty_request, send_json_request, TestStateBuilder};
use axum::{
    body::Body,
    http::{Method, StatusCode},
    routing::{delete, get, post},
    Router,
};
use flapjack::analytics::schema::SearchEvent;
use flapjack::analytics::{AnalyticsConfig, AnalyticsQueryEngine};
use serde_json::json;
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

fn app_router(collector: Arc<AnalyticsCollector>) -> Router {
    let profile_store_base_path = collector
        .config()
        .data_dir
        .parent()
        .expect("test analytics dir should have a parent")
        .to_path_buf();
    app_router_with_base(collector, profile_store_base_path)
}

/// Build the test router with a custom profile store base path for GDPR delete integration tests.
fn app_router_with_base(
    collector: Arc<AnalyticsCollector>,
    profile_store_base_path: std::path::PathBuf,
) -> Router {
    app_router_with_base_and_notifier(collector, profile_store_base_path, None)
}

fn app_router_with_base_and_notifier(
    collector: Arc<AnalyticsCollector>,
    profile_store_base_path: std::path::PathBuf,
    gdpr_notifier: Option<Arc<crate::notifications::NotificationService>>,
) -> Router {
    let gdpr_state = GdprDeleteState {
        analytics_collector: Arc::clone(&collector),
        profile_store_base_path,
        gdpr_notifier,
    };
    Router::new()
        .route("/1/events", post(post_events))
        .route("/1/events/debug", get(get_debug_events))
        .with_state(collector)
        .merge(
            Router::new()
                .route(
                    "/1/indexes/:indexName/usertokens/:userToken",
                    delete(delete_index_usertoken),
                )
                .route("/1/usertokens/:userToken", delete(delete_usertoken))
                .with_state(gdpr_state),
        )
}

/// Assert the standard malformed-event HTTP statuses used by the JSON and handler layers.
fn assert_rejected_event_status(status: StatusCode) {
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "expected 400 or 422, got {status}"
    );
}

fn pbv3_search_event(index: &str, query_id: &str, user_token: &str) -> SearchEvent {
    SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: "shoes".to_string(),
        query_id: Some(query_id.to_string()),
        index_name: index.to_string(),
        nb_hits: 1,
        processing_time_ms: 1,
        user_token: Some(user_token.to_string()),
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
    }
}

fn pbv3_key(key_store: &KeyStore, indexes: Vec<String>, max_per_hour: i64) -> String {
    key_store
        .create_key(ApiKey {
            hash: String::new(),
            salt: String::new(),
            hmac_key: None,
            created_at: 0,
            acl: vec!["search".to_string()],
            description: "PBV3 Insights test key".to_string(),
            indexes,
            max_hits_per_query: 0,
            max_queries_per_ip_per_hour: max_per_hour,
            query_parameters: String::new(),
            referers: vec![],
            restrict_sources: None,
            validity: 0,
        })
        .1
}

fn pbv3_production_app(
    tmp: &TempDir,
    collector: Arc<AnalyticsCollector>,
    key_store: Arc<KeyStore>,
) -> Router {
    crate::router::build_router(
        TestStateBuilder::new(tmp).with_analytics().build_shared(),
        Some(key_store),
        collector,
        Arc::new(crate::middleware::TrustedProxyMatcher::from_optional_csv(None).unwrap()),
        tmp.path(),
        crate::router::RouterConfig {
            cors_mode: crate::startup::CorsMode::LoopbackOnly,
            disable_dashboard: true,
            replication_api_key: None,
            api_profile: crate::api_profile::ApiProfile::Full,
        },
    )
}

async fn pbv3_authed_event_request(
    app: &Router,
    api_key: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            axum::http::Request::builder()
                .method(Method::POST)
                .uri("/1/events")
                .header("content-type", "application/json")
                .header("x-algolia-application-id", "pbv3-app")
                .header("x-algolia-api-key", api_key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn pbv3_official_after_search_click_and_purchase_persist_correlation_and_rates() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let app = app_router(Arc::clone(&collector));
    let index = "tenant__products";
    let user_token = "018f6b5e-4d3c-7a21-8b9c-0123456789ab";
    let click_query_id = "0123456789abcdef0123456789abcdef";
    let purchase_query_id = "abcdef0123456789abcdef0123456789";

    collector.record_search(pbv3_search_event(index, click_query_id, user_token));
    collector.record_search(pbv3_search_event(index, purchase_query_id, user_token));

    let click = send_json_request(
        &app,
        Method::POST,
        "/1/events",
        json!({
            "events": [{
                "eventType": "click",
                "eventName": "PBV3 Click",
                "index": index,
                "queryID": click_query_id,
                "objectIDs": ["sku-1"],
                "positions": [7],
                "userToken": user_token
            }]
        }),
    )
    .await;
    assert_eq!(click.status(), StatusCode::OK);

    let purchase = send_json_request(
        &app,
        Method::POST,
        "/1/events",
        json!({
            "events": [{
                "eventType": "conversion",
                "eventSubtype": "purchase",
                "eventName": "PBV3 Purchase",
                "index": index,
                "objectIDs": ["sku-1"],
                "objectData": [{
                    "queryID": purchase_query_id,
                    "price": 19.95,
                    "quantity": 2
                }],
                "value": 39.90,
                "currency": "USD",
                "userToken": user_token
            }]
        }),
    )
    .await;
    assert_eq!(purchase.status(), StatusCode::OK);

    collector.flush_all();
    let query = AnalyticsQueryEngine::new(config);
    let rows = query
        .query_events(
            index,
            "SELECT event_type, event_subtype, query_id, user_token, positions, value, currency FROM events ORDER BY event_type",
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "both selected official methods must persist");
    assert_eq!(rows[0]["event_type"], "click");
    assert_eq!(rows[0]["query_id"], click_query_id);
    assert_eq!(rows[0]["positions"], "[7]");
    assert_eq!(rows[0]["user_token"], user_token);
    assert_eq!(rows[1]["event_type"], "conversion");
    assert_eq!(rows[1]["event_subtype"], "purchase");
    assert_eq!(rows[1]["query_id"], purchase_query_id);
    assert_eq!(rows[1]["value"], 39.90);
    assert_eq!(rows[1]["currency"], "USD");
    assert_eq!(rows[1]["user_token"], user_token);

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let ctr = query
        .click_through_rate(index, &today, &today)
        .await
        .unwrap();
    assert_eq!(ctr["trackedSearchCount"], 2);
    assert_eq!(ctr["clickCount"], 1);
    assert_eq!(ctr["rate"], 0.5);
    let purchase_rate = query
        .conversion_rate_for_subtype(index, &today, &today, "purchase")
        .await
        .unwrap();
    assert_eq!(purchase_rate["trackedSearchCount"], 2);
    assert_eq!(purchase_rate["purchaseCount"], 1);
    assert_eq!(purchase_rate["rate"], 0.5);
}

#[tokio::test]
async fn pbv3_mixed_valid_invalid_batch_is_atomic_with_no_debug_or_persisted_effect() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let app = app_router(Arc::clone(&collector));
    let user_token = "018f6b5e-4d3c-7a21-8b9c-0123456789ab";

    let response = send_json_request(
        &app,
        Method::POST,
        "/1/events",
        json!({
            "events": [
                {
                    "eventType": "click",
                    "eventName": "Would otherwise persist",
                    "index": "tenant__products",
                    "queryID": "0123456789abcdef0123456789abcdef",
                    "objectIDs": ["sku-1"],
                    "positions": [1],
                    "userToken": user_token
                },
                {
                    "eventType": "click",
                    "eventName": "Invalid missing positions",
                    "index": "tenant__products",
                    "queryID": "abcdef0123456789abcdef0123456789",
                    "objectIDs": ["sku-2"],
                    "userToken": user_token
                }
            ]
        }),
    )
    .await;
    assert_rejected_event_status(response.status());
    assert!(
        collector
            .get_debug_events(10, None, None, None, None, None)
            .is_empty(),
        "an atomically rejected request must not enter the debugger"
    );
    collector.flush_all();
    let rows = AnalyticsQueryEngine::new(config)
        .query_events("tenant__products", "SELECT event_name FROM events")
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "an atomically rejected request persisted rows"
    );
    assert_eq!(
        collector.analytics_metrics_snapshot().accepted_events_total,
        0,
        "an atomically rejected request must not increment analytics metrics"
    );
}

#[tokio::test]
async fn pbv3_index_restrictions_and_low_cap_reject_before_event_effects() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let key_store = Arc::new(KeyStore::load_or_create(
        &tmp.path().join("keys"),
        "admin-key",
    ));
    let scoped_key = pbv3_key(&key_store, vec!["tenant-a".to_string()], 0);
    let limited_key = pbv3_key(&key_store, vec!["tenant-a".to_string()], 1);
    let app = pbv3_production_app(&tmp, Arc::clone(&collector), key_store);
    let user_token = "018f6b5e-4d3c-7a21-8b9c-0123456789ab";
    let click = |event_name: &str, index: &str, object_id: &str| {
        json!({
            "eventType": "click",
            "eventName": event_name,
            "index": index,
            "queryID": "0123456789abcdef0123456789abcdef",
            "objectIDs": [object_id],
            "positions": [1],
            "userToken": user_token
        })
    };
    let purchase = |event_name: &str, index: &str, object_id: &str| {
        json!({
            "eventType": "conversion",
            "eventSubtype": "purchase",
            "eventName": event_name,
            "index": index,
            "objectIDs": [object_id],
            "objectData": [{
                "queryID": "abcdef0123456789abcdef0123456789",
                "price": 12.5,
                "quantity": 1
            }],
            "value": 12.5,
            "currency": "USD",
            "userToken": user_token
        })
    };

    let same_index = pbv3_authed_event_request(
        &app,
        &scoped_key,
        json!({
            "events": [
                click("Same-index accepted", "tenant-a", "a-1"),
                purchase("Same-index purchase", "tenant-a", "a-1")
            ]
        }),
    )
    .await;
    assert_eq!(same_index.status(), StatusCode::OK);

    let mixed_index = pbv3_authed_event_request(
        &app,
        &scoped_key,
        json!({
            "events": [
                click("Mixed allowed", "tenant-a", "a-2"),
                click("Mixed forbidden", "tenant-b", "b-1")
            ]
        }),
    )
    .await;
    assert_eq!(mixed_index.status(), StatusCode::FORBIDDEN);

    let within_cap = pbv3_authed_event_request(
        &app,
        &limited_key,
        json!({"events": [click("Rate accepted", "tenant-a", "a-3")]}),
    )
    .await;
    assert_eq!(within_cap.status(), StatusCode::OK);
    let over_cap = pbv3_authed_event_request(
        &app,
        &limited_key,
        json!({"events": [click("Rate rejected", "tenant-a", "a-4")]}),
    )
    .await;
    assert_eq!(over_cap.status(), StatusCode::TOO_MANY_REQUESTS);

    let debug_names: Vec<String> = collector
        .get_debug_events(10, None, None, None, None, None)
        .into_iter()
        .map(|event| event.event_name)
        .collect();
    assert_eq!(
        debug_names,
        vec![
            "Rate accepted".to_string(),
            "Same-index purchase".to_string(),
            "Same-index accepted".to_string()
        ]
    );
    collector.flush_all();
    let query = AnalyticsQueryEngine::new(config);
    let rows = query
        .query_events(
            "tenant-a",
            "SELECT event_name FROM events ORDER BY event_name",
        )
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![
            json!({"event_name": "Rate accepted"}),
            json!({"event_name": "Same-index accepted"}),
            json!({"event_name": "Same-index purchase"})
        ]
    );
    let forbidden_rows = query
        .query_events("tenant-b", "SELECT event_name FROM events")
        .await
        .unwrap();
    assert!(
        forbidden_rows.is_empty(),
        "the forbidden second index must have no persisted effect"
    );
    assert_eq!(
        collector.analytics_metrics_snapshot().accepted_events_total,
        3,
        "only the two same-index events and one within-cap event are accepted"
    );
}

/// Verify that DELETE /1/usertokens/:token returns 200 with an RFC 3339 `deletedAt` timestamp.
#[tokio::test]
async fn delete_usertoken_returns_ok_with_rfc3339_deleted_at() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let response = send_empty_request(&app, Method::DELETE, "/1/usertokens/user_123").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["status"], json!(200));
    assert_eq!(body["message"], json!("OK"));
    let deleted_at = body["deletedAt"]
        .as_str()
        .expect("deletedAt should be an RFC3339 timestamp string");
    chrono::DateTime::parse_from_rfc3339(deleted_at)
        .expect("deletedAt should be parseable RFC3339");
}

/// Verify that GDPR delete removes the target user's events from Parquet query results while leaving other users' data intact.
#[tokio::test]
async fn delete_usertoken_purges_events_from_analytics_queries() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let app = app_router(collector.clone());

    let ingest_body = json!({
        "events": [
            {
                "eventType": "view",
                "eventName": "Viewed",
                "index": "products",
                "userToken": "delete-me",
                "objectIDs": ["obj1"]
            },
            {
                "eventType": "view",
                "eventName": "Viewed",
                "index": "products",
                "userToken": "other_user",
                "objectIDs": ["obj2"]
            }
        ]
    });
    let ingest_response = send_json_request(&app, Method::POST, "/1/events", ingest_body).await;
    assert_eq!(ingest_response.status(), StatusCode::OK);
    collector.flush_all();

    let delete_response = send_empty_request(&app, Method::DELETE, "/1/usertokens/delete-me").await;
    assert_eq!(delete_response.status(), StatusCode::OK);
    let delete_body = body_json(delete_response).await;
    let deleted_at = delete_body["deletedAt"]
        .as_str()
        .expect("deletedAt should be present");
    chrono::DateTime::parse_from_rfc3339(deleted_at)
        .expect("deletedAt should be parseable RFC3339");

    let engine = AnalyticsQueryEngine::new(config);
    let rows = engine
            .query_events(
                "products",
                "SELECT user_token, COUNT(*) as count FROM events GROUP BY user_token ORDER BY user_token",
            )
            .await
            .unwrap();

    assert!(
        !rows
            .iter()
            .any(|row| row.get("user_token") == Some(&json!("delete-me"))),
        "delete-me should be fully removed from events: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.get("user_token") == Some(&json!("other_user"))),
        "non-target users should remain present: {rows:?}"
    );
}

#[tokio::test]
async fn delete_index_usertoken_purges_only_the_exact_index_and_preserves_profile() {
    use flapjack::personalization::{PersonalizationProfile, PersonalizationProfileStore};

    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let app = app_router(Arc::clone(&collector));
    let token = "shared-user";

    let ingest_response = send_json_request(
        &app,
        Method::POST,
        "/1/events",
        json!({
            "events": [
                {"eventType": "view", "eventName": "A", "index": "tenant-a", "userToken": token, "objectIDs": ["a"]},
                {"eventType": "view", "eventName": "B", "index": "tenant-b", "userToken": token, "objectIDs": ["b"]}
            ]
        }),
    )
    .await;
    assert_eq!(ingest_response.status(), StatusCode::OK);
    collector.flush_all();

    let profiles = PersonalizationProfileStore::new(tmp.path());
    profiles
        .save_profile(&PersonalizationProfile {
            user_token: token.to_string(),
            last_event_at: None,
            scores: Default::default(),
        })
        .unwrap();

    let response = send_empty_request(
        &app,
        Method::DELETE,
        "/1/indexes/tenant-a/usertokens/shared-user",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["deletionScope"], "exactIndex");

    let query = AnalyticsQueryEngine::new(config);
    assert!(query
        .query_events("tenant-a", "SELECT user_token FROM events")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        query
            .query_events("tenant-b", "SELECT user_token FROM events")
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(profiles.load_profile(token).unwrap().is_some());
}

/// Verify that click events without a `positions` array are rejected with 400 or 422.
#[tokio::test]
async fn post_events_rejects_click_without_positions() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let body = json!({
        "events": [{
            "eventType": "click",
            "eventName": "Product Clicked",
            "index": "products",
            "userToken": "018f6b5e-4d3c-7a21-8b9c-0123456789ab",
            "objectIDs": ["obj1"]
        }]
    });

    let response = send_json_request(&app, Method::POST, "/1/events", body).await;
    assert_rejected_event_status(response.status());
}

/// Verify that click events are rejected when the length of `positions` does not match `objectIDs`.
#[tokio::test]
async fn post_events_rejects_click_when_positions_count_mismatches_object_ids() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let body = json!({
        "events": [{
            "eventType": "click",
            "eventName": "Product Clicked",
            "index": "products",
            "userToken": "user_123",
            "objectIDs": ["obj1", "obj2"],
            "positions": [1]
        }]
    });

    let response = send_json_request(&app, Method::POST, "/1/events", body).await;
    assert_rejected_event_status(response.status());
}

/// Verify that a click event with a valid `queryID` and matching `positions`/`objectIDs` counts is accepted.
#[tokio::test]
async fn post_events_accepts_click_with_query_id_and_matching_positions() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let query_id = "a".repeat(32);
    let body = json!({
        "events": [{
            "eventType": "click",
            "eventName": "Product Clicked",
            "index": "products",
            "userToken": "018f6b5e-4d3c-7a21-8b9c-0123456789ab",
            "objectIDs": ["obj1"],
            "positions": [1],
            "queryID": query_id
        }]
    });

    let response = send_json_request(&app, Method::POST, "/1/events", body).await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// Verify that user tokens containing characters outside the allowed set (e.g. `@`) are rejected.
#[tokio::test]
async fn post_events_rejects_user_token_with_invalid_characters() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let body = json!({
        "events": [{
            "eventType": "view",
            "eventName": "Viewed Product",
            "index": "products",
            "userToken": "user@email.com",
            "objectIDs": ["obj1"]
        }]
    });

    let response = send_json_request(&app, Method::POST, "/1/events", body).await;
    assert_rejected_event_status(response.status());
}

/// Verify that user tokens composed of alphanumerics, hyphens, and underscores are accepted.
#[tokio::test]
async fn post_events_accepts_user_token_with_allowed_characters() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let body = json!({
        "events": [{
            "eventType": "view",
            "eventName": "Viewed Product",
            "index": "products",
            "userToken": "valid-user_123",
            "objectIDs": ["obj1"]
        }]
    });

    let response = send_json_request(&app, Method::POST, "/1/events", body).await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// Verify that user tokens exceeding 129 characters are rejected.
#[tokio::test]
async fn post_events_rejects_user_token_longer_than_129_chars() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let body = json!({
        "events": [{
            "eventType": "view",
            "eventName": "Viewed Product",
            "index": "products",
            "userToken": "x".repeat(130),
            "objectIDs": ["obj1"]
        }]
    });

    let response = send_json_request(&app, Method::POST, "/1/events", body).await;
    assert_rejected_event_status(response.status());
}

/// Verify that `eventSubtype` is rejected on non-conversion event types such as click.
#[tokio::test]
async fn post_events_rejects_event_subtype_on_non_conversion_events() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let body = json!({
        "events": [{
            "eventType": "click",
            "eventName": "Product Clicked",
            "index": "products",
            "userToken": "user_123",
            "objectIDs": ["obj1"],
            "positions": [1],
            "eventSubtype": "addToCart"
        }]
    });

    let response = send_json_request(&app, Method::POST, "/1/events", body).await;
    assert_rejected_event_status(response.status());
}

/// Verify that unrecognized `eventSubtype` values on conversion events are rejected.
#[tokio::test]
async fn post_events_rejects_invalid_event_subtype_on_conversion_events() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let body = json!({
        "events": [{
            "eventType": "conversion",
            "eventName": "Product Purchased",
            "index": "products",
            "userToken": "user_123",
            "objectIDs": ["obj1"],
            "eventSubtype": "invalid"
        }]
    });

    let response = send_json_request(&app, Method::POST, "/1/events", body).await;
    assert_rejected_event_status(response.status());
}

/// Verify that `eventSubtype: "purchase"` is accepted on conversion events.
#[tokio::test]
async fn post_events_accepts_purchase_event_subtype_on_conversion_events() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let body = json!({
        "events": [{
            "eventType": "conversion",
            "eventName": "Product Purchased",
            "index": "products",
            "userToken": "018f6b5e-4d3c-7a21-8b9c-0123456789ab",
            "objectIDs": ["obj1"],
            "objectData": [{
                "queryID": "0123456789abcdef0123456789abcdef",
                "price": 12.5,
                "quantity": 2
            }],
            "value": 25.0,
            "currency": "USD",
            "eventSubtype": "purchase"
        }]
    });

    let response = send_json_request(&app, Method::POST, "/1/events", body).await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// Verify that `eventSubtype: "addToCart"` is accepted on conversion events.
#[tokio::test]
async fn post_events_accepts_add_to_cart_event_subtype_on_conversion_events() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let body = json!({
        "events": [{
            "eventType": "conversion",
            "eventName": "Product Added To Cart",
            "index": "products",
            "userToken": "user_123",
            "objectIDs": ["obj1"],
            "eventSubtype": "addToCart"
        }]
    });

    let response = send_json_request(&app, Method::POST, "/1/events", body).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn debug_endpoint_returns_empty_when_no_events() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let resp = send_empty_request(&app, Method::GET, "/1/events/debug").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["count"], 0);
    assert_eq!(body["events"].as_array().unwrap().len(), 0);
}

/// Verify that an atomically rejected batch publishes no Event Debugger entries.
#[tokio::test]
async fn debug_endpoint_omits_atomically_rejected_batch() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let ingest_body = json!({
        "events": [
            {
                "eventType": "view",
                "eventName": "Viewed Product",
                "index": "products",
                "userToken": "user_abc",
                "objectIDs": ["obj1"]
            },
            {
                "eventType": "bogus",
                "eventName": "Bad Event",
                "index": "products",
                "userToken": "user_xyz",
                "objectIDs": ["obj2"]
            }
        ]
    });
    let resp = send_json_request(&app, Method::POST, "/1/events", ingest_body).await;
    assert_rejected_event_status(resp.status());

    let debug_resp = send_empty_request(&app, Method::GET, "/1/events/debug").await;
    assert_eq!(debug_resp.status(), StatusCode::OK);
    let body = body_json(debug_resp).await;
    assert_eq!(body["count"], 0);
    assert!(body["events"].as_array().unwrap().is_empty());
}

/// Verify that the debug endpoint correctly filters events by `index` and `status` query parameters.
#[tokio::test]
async fn debug_endpoint_filters_by_index_and_status() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let ingest_body = json!({
        "events": [
            {
                "eventType": "view",
                "eventName": "V1",
                "index": "products",
                "userToken": "user_a",
                "objectIDs": ["o1"]
            },
            {
                "eventType": "view",
                "eventName": "V2",
                "index": "orders",
                "userToken": "user_b",
                "objectIDs": ["o2"]
            }
        ]
    });
    send_json_request(&app, Method::POST, "/1/events", ingest_body).await;

    // Filter by index
    let resp = send_empty_request(&app, Method::GET, "/1/events/debug?index=products").await;
    let body = body_json(resp).await;
    assert_eq!(body["count"], 1);
    assert_eq!(body["events"][0]["index"], "products");

    // Filter by status=ok (both are valid)
    let resp = send_empty_request(&app, Method::GET, "/1/events/debug?status=ok").await;
    let body = body_json(resp).await;
    assert_eq!(body["count"], 2);

    // Filter by status=error (none are errors)
    let resp = send_empty_request(&app, Method::GET, "/1/events/debug?status=error").await;
    let body = body_json(resp).await;
    assert_eq!(body["count"], 0);
}

/// Verify that an unrecognized `status` filter value returns 400 with a descriptive error.
#[tokio::test]
async fn debug_endpoint_rejects_invalid_status_filter() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let resp = send_empty_request(&app, Method::GET, "/1/events/debug?status=invalid").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    let message = body["message"]
        .as_str()
        .expect("error body should include message");
    assert!(
        message.contains("status"),
        "expected status validation message, got: {message}"
    );
}

/// Verify that `from` and `until` query parameters filter debug events to the specified millisecond time window.
#[tokio::test]
async fn debug_endpoint_filters_by_time_range() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let base = chrono::Utc::now().timestamp_millis() - 10_000;
    let ingest_body = json!({
        "events": [
            {
                "eventType": "view",
                "eventName": "older",
                "index": "products",
                "userToken": "user_a",
                "objectIDs": ["o1"],
                "timestamp": base
            },
            {
                "eventType": "view",
                "eventName": "middle",
                "index": "products",
                "userToken": "user_a",
                "objectIDs": ["o2"],
                "timestamp": base + 1_000
            },
            {
                "eventType": "view",
                "eventName": "newer",
                "index": "products",
                "userToken": "user_a",
                "objectIDs": ["o3"],
                "timestamp": base + 2_000
            }
        ]
    });
    let post_resp = send_json_request(&app, Method::POST, "/1/events", ingest_body).await;
    assert_eq!(post_resp.status(), StatusCode::OK);

    let resp = send_empty_request(
        &app,
        Method::GET,
        &format!(
            "/1/events/debug?from={}&until={}",
            base + 1_000,
            base + 2_000
        ),
    )
    .await;
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "unexpected response body: {body}");
    assert_eq!(body["count"], 2);
    let names: Vec<String> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["eventName"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"middle".to_string()));
    assert!(names.contains(&"newer".to_string()));
    assert!(!names.contains(&"older".to_string()));
}

/// Verify that `from` greater than `until` returns 400 with a time-range validation error.
#[tokio::test]
async fn debug_endpoint_rejects_invalid_time_range() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let resp = send_empty_request(&app, Method::GET, "/1/events/debug?from=2000&until=1000").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    let message = body["message"]
        .as_str()
        .expect("error body should include message");
    assert!(
        message.contains("from"),
        "expected time-range validation message, got: {message}"
    );
}

/// Verify that `limit=0` returns 400 since the minimum allowed limit is 1.
#[tokio::test]
async fn debug_endpoint_rejects_zero_limit() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let resp = send_empty_request(&app, Method::GET, "/1/events/debug?limit=0").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    let message = body["message"]
        .as_str()
        .expect("error body should include message");
    assert!(
        message.contains("limit"),
        "expected limit validation message, got: {message}"
    );
}

/// Verify that a non-numeric `limit` value returns 400 with a limit validation error.
#[tokio::test]
async fn debug_endpoint_rejects_non_numeric_limit() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let resp = send_empty_request(&app, Method::GET, "/1/events/debug?limit=abc").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    let message = body["message"]
        .as_str()
        .expect("error body should include message");
    assert!(
        message.contains("limit"),
        "expected limit validation message, got: {message}"
    );
}

/// Verify that a negative `limit` value returns 400 with a limit validation error.
#[tokio::test]
async fn debug_endpoint_rejects_negative_limit() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let resp = send_empty_request(&app, Method::GET, "/1/events/debug?limit=-1").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    let message = body["message"]
        .as_str()
        .expect("error body should include message");
    assert!(
        message.contains("limit"),
        "expected limit validation message, got: {message}"
    );
}

#[path = "insights_tests_gdpr.rs"]
mod gdpr;
