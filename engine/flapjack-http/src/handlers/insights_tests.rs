use super::*;
use crate::test_helpers::{body_json, send_empty_request, send_json_request};
use axum::{
    http::{Method, StatusCode},
    routing::{delete, get, post},
    Router,
};
use flapjack::analytics::{AnalyticsConfig, AnalyticsQueryEngine};
use serde_json::json;
use tempfile::TempDir;

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
                .route("/1/usertokens/:userToken", delete(delete_usertoken))
                .with_state(gdpr_state),
        )
}

/// Send a one-shot HTTP request with a JSON body to the given router and return the raw response.
fn assert_rejected_event_status(status: StatusCode) {
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "expected 400 or 422, got {status}"
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
            "userToken": "user_123",
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
            "userToken": "user_123",
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
            "userToken": "user_123",
            "objectIDs": ["obj1"],
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

/// Verify that the debug ring buffer records both accepted (200) and rejected (422) events with correct metadata.
#[tokio::test]
async fn debug_endpoint_records_valid_and_invalid_events() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    // Send a mix of valid and invalid events
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
    assert_eq!(resp.status(), StatusCode::OK);

    // Query debug endpoint
    let debug_resp = send_empty_request(&app, Method::GET, "/1/events/debug").await;
    assert_eq!(debug_resp.status(), StatusCode::OK);
    let body = body_json(debug_resp).await;
    assert_eq!(body["count"], 2);

    let events = body["events"].as_array().unwrap();
    // Newest first (reverse chronological)
    let invalid = &events[0];
    assert_eq!(invalid["eventType"], "bogus");
    assert_eq!(invalid["httpCode"], 422);
    assert!(!invalid["validationErrors"].as_array().unwrap().is_empty());

    let valid = &events[1];
    assert_eq!(valid["eventType"], "view");
    assert_eq!(valid["httpCode"], 200);
    assert!(valid["validationErrors"].as_array().unwrap().is_empty());
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
