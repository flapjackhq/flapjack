use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const ADMIN_KEY: &str = "test-admin-key-stage5-auth";
const INVALID_AUTH_BODY: &str = r#"{"message":"Invalid Application-ID or API key","status":403}"#;

fn request_with_headers(method: Method, uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::empty()).unwrap()
}

async fn send_request(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(request_with_headers(method, uri, headers))
        .await
        .unwrap()
}

async fn read_json_body(resp: axum::http::Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn assert_forbidden_invalid_auth(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
) {
    let resp = send_request(app, method, uri, headers).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("application/json"),
        "auth failures must return application/json, got: {content_type}"
    );
    let body = read_json_body(resp).await;
    assert_eq!(
        body,
        json!({"message": "Invalid Application-ID or API key", "status": 403})
    );
    assert_eq!(body.to_string(), INVALID_AUTH_BODY);
}

#[tokio::test]
async fn application_id_query_transport_is_rejected_on_privileged_routes() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let response = send_request(
        &app,
        Method::GET,
        "/1/indexes?x-algolia-application-id=test",
        &[("x-algolia-api-key", ADMIN_KEY)],
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn privileged_routes_require_header_credentials_without_query_fallback() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let header_only = send_request(
        &app,
        Method::GET,
        "/1/indexes",
        &[
            ("x-algolia-api-key", ADMIN_KEY),
            ("x-algolia-application-id", "test"),
        ],
    )
    .await;
    assert_eq!(header_only.status(), StatusCode::OK);

    assert_forbidden_invalid_auth(
        &app,
        Method::GET,
        &format!("/1/indexes?x-algolia-api-key={ADMIN_KEY}"),
        &[("x-algolia-application-id", "test")],
    )
    .await;

    assert_forbidden_invalid_auth(
        &app,
        Method::GET,
        &format!("/1/indexes?x-algolia-api-key={ADMIN_KEY}"),
        &[
            ("x-algolia-api-key", ADMIN_KEY),
            ("x-algolia-application-id", "test"),
        ],
    )
    .await;

    assert_forbidden_invalid_auth(
        &app,
        Method::GET,
        &format!("/1/indexes?x-algolia-api-key={ADMIN_KEY}"),
        &[
            ("x-algolia-api-key", "wrong-key"),
            ("x-algolia-application-id", "test"),
        ],
    )
    .await;
}

#[tokio::test]
async fn auth_failures_return_consistent_403_json_shape_for_invalid_combinations() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    assert_forbidden_invalid_auth(&app, Method::GET, "/1/indexes", &[]).await;

    assert_forbidden_invalid_auth(
        &app,
        Method::GET,
        "/1/indexes",
        &[
            ("x-algolia-api-key", "wrong-key"),
            ("x-algolia-application-id", "test"),
        ],
    )
    .await;

    assert_forbidden_invalid_auth(
        &app,
        Method::GET,
        "/1/indexes",
        &[("x-algolia-api-key", ADMIN_KEY)],
    )
    .await;
}

#[tokio::test]
async fn mixed_case_sdk_headers_authenticate_successfully() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let resp = send_request(
        &app,
        Method::GET,
        "/1/indexes",
        &[
            ("X-Algolia-Application-Id", "test"),
            ("X-Algolia-API-Key", ADMIN_KEY),
        ],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}
