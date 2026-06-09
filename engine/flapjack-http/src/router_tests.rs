//! Stub summary for /Users/stuart/parallel_development/flapjack_dev/jun05_pm_1_dashboard_trailing_slash_routing_fix/flapjack_dev/engine/flapjack-http/src/router_tests.rs.
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::auth::KeyStore;
use crate::middleware::REQUEST_ID_HEADER_NAME;
use crate::test_helpers::{body_json, build_test_router, send_empty_request};

fn build_auth_test_app() -> (TempDir, axum::Router) {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));
    (tmp, app)
}

fn build_no_auth_test_app() -> (TempDir, axum::Router) {
    let tmp = TempDir::new().unwrap();
    let app = build_test_router(&tmp, None);
    (tmp, app)
}

async fn assert_invalid_credentials_response(resp: axum::response::Response) {
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(resp).await,
        serde_json::json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );
}

#[tokio::test]
async fn readiness_route_is_public() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/health/ready").await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    assert_eq!(
        body_json(resp).await,
        serde_json::json!({
            "ready": true
        })
    );
}

#[tokio::test]
async fn health_route_is_public() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/health").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}
#[tokio::test]
async fn dashboard_route_is_public_and_serves_html() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("text/html"),
        "expected dashboard route to return HTML, got: {content_type}"
    );

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("<html"),
        "dashboard body should contain HTML markup"
    );
}

#[tokio::test]
async fn dashboard_trailing_slash_route_is_public_and_serves_html() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard/").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("text/html"),
        "expected dashboard trailing slash route to return HTML, got: {content_type}"
    );

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("<html"),
        "dashboard trailing slash body should contain HTML markup"
    );
}

#[tokio::test]
async fn dashboard_spa_fallback_route_is_public() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard/settings/profile").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("<html"),
        "SPA fallback should return index HTML"
    );
}

#[tokio::test]
async fn dashboard_prefix_without_separator_is_not_public() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard-admin").await;
    assert_invalid_credentials_response(resp).await;
}

#[tokio::test]
async fn metrics_returns_403_without_auth_headers() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/metrics").await;
    assert_invalid_credentials_response(resp).await;
}
#[tokio::test]
async fn request_id_present_on_auth_403() {
    let (_tmp, app) = build_auth_test_app();

    let response = send_empty_request(&app, Method::GET, "/metrics").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let request_id = response
        .headers()
        .get(REQUEST_ID_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .expect("403 response should include x-request-id");
    let parsed = uuid::Uuid::parse_str(request_id).expect("request ID should be a UUID");
    assert_eq!(
        parsed.get_version(),
        Some(uuid::Version::Random),
        "request ID should be UUID v4"
    );
}
#[tokio::test]
async fn metrics_returns_200_with_admin_key_only() {
    let (_tmp, app) = build_auth_test_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/metrics")
                .header("x-algolia-api-key", "admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("text/plain"),
        "expected Prometheus text/plain, got: {content_type}"
    );
}

#[tokio::test]
async fn metrics_rejects_query_param_admin_key() {
    let (_tmp, app) = build_auth_test_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/metrics?x-algolia-api-key=admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_invalid_credentials_response(resp).await;
}

#[tokio::test]
async fn internal_replication_routes_remain_available_when_auth_disabled() {
    let (_tmp, app) = build_no_auth_test_app();

    let internal_status = send_empty_request(&app, Method::GET, "/internal/status").await;
    assert_eq!(
        internal_status.status(),
        StatusCode::OK,
        "no-auth mode must still expose /internal/status for peer health probing"
    );

    let cluster_status = send_empty_request(&app, Method::GET, "/internal/cluster/status").await;
    assert_eq!(
        cluster_status.status(),
        StatusCode::OK,
        "no-auth mode must still expose /internal/cluster/status for HA checks"
    );

    // Route-availability probe: malformed tenant IDs must reach handler validation
    // (400) instead of falling through the router (404).
    let malformed_ops = send_empty_request(
        &app,
        Method::GET,
        "/internal/ops?tenant_id=../evil&since_seq=0",
    )
    .await;
    assert_eq!(
        malformed_ops.status(),
        StatusCode::BAD_REQUEST,
        "no-auth mode must expose /internal/ops for peer catch-up"
    );

    let malformed_replicate = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/replicate")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"tenant_id":"../evil","ops":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        malformed_replicate.status(),
        StatusCode::BAD_REQUEST,
        "no-auth mode must expose /internal/replicate for peer replication writes"
    );
}
#[tokio::test]
async fn internal_storage_returns_403_with_admin_key_only_no_app_id() {
    let (_tmp, app) = build_auth_test_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/internal/storage")
                .header("x-algolia-api-key", "admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_invalid_credentials_response(resp).await;
}

/// Verify that the request latency histogram middleware records both successful and authentication-rejected requests with proper status class labels. Sends a successful POST request (200) and an auth-rejected POST request (403) to the same endpoint, then confirms both metrics appear in the Prometheus output with correct method, route, and status_class labels. Also verifies that the metrics endpoint itself remains admin-only protected.
#[tokio::test]
async fn latency_histogram_captures_success_and_auth_rejection_while_metrics_stays_admin_only() {
    let (_tmp, app) = build_auth_test_app();

    let create_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes")
                .header("x-algolia-api-key", "admin-key")
                .header("x-algolia-application-id", "latency-app")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"uid":"latency_success_index"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);

    let rejected_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"uid":"latency_forbidden_index"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected_resp.status(), StatusCode::FORBIDDEN);

    let metrics_without_auth = send_empty_request(&app, Method::GET, "/metrics").await;
    assert_eq!(metrics_without_auth.status(), StatusCode::FORBIDDEN);

    let metrics_with_admin = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/metrics")
                .header("x-algolia-api-key", "admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(metrics_with_admin.status(), StatusCode::OK);

    let body = axum::body::to_bytes(metrics_with_admin.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("request_duration_seconds"),
        "expected shared latency histogram family in /metrics output"
    );
    assert!(
        text.lines().any(|line| {
            line.starts_with("request_duration_seconds_count")
                && line.contains("method=\"POST\"")
                && line.contains("route=\"/1/indexes\"")
                && line.contains("status_class=\"2xx\"")
        }),
        "expected POST 2xx request_duration_seconds_count for /1/indexes in:\n{text}"
    );
    assert!(
        text.lines().any(|line| {
            line.starts_with("request_duration_seconds_count")
                && line.contains("method=\"POST\"")
                && line.contains("route=\"/1/indexes\"")
                && line.contains("status_class=\"4xx\"")
        }),
        "expected POST 4xx request_duration_seconds_count for /1/indexes in:\n{text}"
    );
}
