//! Stub summary for engine/flapjack-http/src/router_inline_tests.rs.
use super::*;
#[cfg(feature = "fault-injection")]
use crate::middleware::REQUEST_ID_HEADER_NAME;
use crate::startup::CorsMode;
use crate::test_helpers::{
    body_json, send_empty_request, send_json_request, with_env_var, EnvVarRestoreGuard,
    SharedLogBuffer, TestStateBuilder, ENV_MUTEX,
};
use axum::body::Body;
#[cfg(feature = "fault-injection")]
use axum::http::header;
use axum::http::{header::HeaderMap, Method, Request, StatusCode};
use axum::routing::{get, post};
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig};
#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower::ServiceExt;

fn build_test_router_for_data_dir(
    tmp: &TempDir,
    key_store: Option<Arc<KeyStore>>,
    data_dir: &Path,
) -> Router {
    build_test_router_with_state_for_data_dir(tmp, key_store, data_dir).0
}

/// TODO: Document build_test_router_with_state_for_data_dir.
fn build_test_router_with_state_for_data_dir(
    tmp: &TempDir,
    key_store: Option<Arc<KeyStore>>,
    data_dir: &Path,
) -> (Router, Arc<AppState>) {
    let state = TestStateBuilder::new(tmp).with_analytics().build_shared();
    let analytics_config = AnalyticsConfig {
        enabled: false,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 60,
        flush_size: 1000,
        retention_days: 30,
    };
    let analytics_collector = AnalyticsCollector::new(analytics_config);
    let trusted_proxy_matcher = Arc::new(TrustedProxyMatcher::from_optional_csv(None).unwrap());

    let app = build_router(
        Arc::clone(&state),
        key_store,
        analytics_collector,
        trusted_proxy_matcher,
        data_dir,
        RouterConfig {
            cors_mode: CorsMode::LoopbackOnly,
            disable_dashboard: false,
            replication_api_key: None,
            api_profile: crate::api_profile::ApiProfile::Full,
        },
    );

    (app, state)
}

fn build_test_router(tmp: &TempDir, key_store: Option<Arc<KeyStore>>) -> Router {
    build_test_router_for_data_dir(tmp, key_store, tmp.path())
}

fn build_test_router_with_state(
    tmp: &TempDir,
    key_store: Option<Arc<KeyStore>>,
) -> (Router, Arc<AppState>) {
    build_test_router_with_state_for_data_dir(tmp, key_store, tmp.path())
}

async fn body_text(resp: axum::http::Response<axum::body::Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[cfg(feature = "fault-injection")]
fn fault_http_request(path: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("x-algolia-application-id", "resource-bounds-test")
        .header("x-algolia-api-key", "admin-key")
        .body(Body::empty())
        .unwrap()
}

#[cfg(feature = "fault-injection")]
async fn fault_request(app: Router, path: &str) -> axum::http::Response<Body> {
    app.oneshot(fault_http_request(path)).await.unwrap()
}

#[cfg(feature = "fault-injection")]
async fn wait_for_fault_sleep_marker(logs: &SharedLogBuffer) {
    for _ in 0..40 {
        if logs.contents().contains(FAULT_SLEEP_LOG_MARKER) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("fault sleep log marker was not emitted before health request");
}

#[cfg(feature = "fault-injection")]
fn fault_test_router() -> (TempDir, Router, Arc<KeyStore>) {
    test_router_with_resource_bounds(ResourceBounds {
        request_timeout: Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
        max_concurrent_requests: DEFAULT_MAX_CONCURRENT_REQUESTS,
    })
}

fn test_router_with_resource_bounds(
    resource_bounds: ResourceBounds,
) -> (TempDir, Router, Arc<KeyStore>) {
    let tmp = TempDir::new().unwrap();
    let key_dir = tmp.path().join("keys");
    std::fs::create_dir_all(&key_dir).unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(&key_dir, "admin-key"));
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    let analytics_config = AnalyticsConfig {
        enabled: false,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 60,
        flush_size: 1000,
        retention_days: 30,
    };
    let app = build_router_with_resource_bounds(
        state,
        Some(Arc::clone(&key_store)),
        AnalyticsCollector::new(analytics_config),
        Arc::new(TrustedProxyMatcher::from_optional_csv(None).unwrap()),
        tmp.path(),
        RouterConfig {
            cors_mode: CorsMode::LoopbackOnly,
            disable_dashboard: false,
            replication_api_key: None,
            api_profile: crate::api_profile::ApiProfile::Full,
        },
        resource_bounds,
    );
    (tmp, app, key_store)
}

#[cfg(feature = "fault-injection")]
fn assert_json_content_type(response: &axum::http::Response<Body>) {
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("application/json"),
        "expected JSON content-type, got: {content_type:?}"
    );
}

const EXPECTED_SECURITY_HEADERS: &[(&str, &str)] = &[
    (
        "content-security-policy",
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
    ),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    (
        "permissions-policy",
        "camera=(), microphone=(), geolocation=()",
    ),
];

const CUSTOM_CONTENT_SECURITY_POLICY: &str =
    "default-src 'none'; frame-ancestors 'none'; base-uri 'none'";

const EXPECTED_SWAGGER_SCRIPT_TAGS: &[&str] = &[
    r#"<script src="./swagger-ui-bundle.js" charset="UTF-8">"#,
    r#"<script src="./swagger-ui-standalone-preset.js" charset="UTF-8">"#,
    r#"<script src="./swagger-initializer.js" charset="UTF-8">"#,
];

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).map(|value| {
        value
            .to_str()
            .map(str::to_owned)
            .unwrap_or_else(|_| "<non-utf8>".to_owned())
    })
}

fn assert_security_headers(surface: &str, headers: &HeaderMap) {
    let mut mismatches = Vec::new();

    for (name, expected) in EXPECTED_SECURITY_HEADERS {
        match header_value(headers, name) {
            None => mismatches.push(format!("{name} missing; expected={expected:?}")),
            Some(actual) if actual.is_empty() => {
                mismatches.push(format!("{name} empty; expected={expected:?}"))
            }
            Some(actual) if actual != *expected => mismatches.push(format!(
                "{name} wrong; expected={expected:?} actual={actual:?}"
            )),
            Some(_) => {}
        }
    }

    let csp = header_value(headers, "content-security-policy");
    let x_frame_options = header_value(headers, "x-frame-options");
    if !csp
        .as_deref()
        .is_some_and(|value| value.contains("frame-ancestors 'none'"))
        && !x_frame_options
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("deny"))
    {
        mismatches.push(format!(
            "frame protection missing; expected=\"content-security-policy with frame-ancestors 'none' or x-frame-options DENY\" actual_csp={:?} actual_x_frame_options={:?}",
            csp, x_frame_options
        ));
    }

    if let Some(actual) = header_value(headers, "strict-transport-security") {
        mismatches.push(format!(
            "strict-transport-security present; expected absent until HTTPS listener support exists actual={actual:?}"
        ));
    }

    assert!(
        mismatches.is_empty(),
        "{surface} security header mismatches:\n{}",
        mismatches.join("\n")
    );
}

fn script_opening_tags(html: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut remaining = html;

    while let Some(start) = remaining.find("<script") {
        remaining = &remaining[start..];
        if let Some(end) = remaining.find('>') {
            tags.push(remaining[..=end].to_owned());
            remaining = &remaining[end + 1..];
        } else {
            break;
        }
    }

    tags
}

fn script_tag_has_src(tag: &str) -> bool {
    tag.to_ascii_lowercase().contains(" src=")
}

async fn get_response(app: Router, path: &str) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

#[allow(clippy::await_holding_lock)]
async fn assert_surface_security_headers(surface: &str, path: &str) {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let (_tmp, app, _restore) = build_test_router_with_csp_env(None);
    let response = get_response(app, path).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{surface} must return 200 before security headers are evaluated"
    );

    assert_security_headers(surface, response.headers());
}

fn build_test_router_with_csp_env(
    raw_policy: Option<&str>,
) -> (TempDir, Router, EnvVarRestoreGuard) {
    let restore = match raw_policy {
        Some(policy) => EnvVarRestoreGuard::set("FLAPJACK_CONTENT_SECURITY_POLICY", policy),
        None => EnvVarRestoreGuard::remove("FLAPJACK_CONTENT_SECURITY_POLICY"),
    };
    let tmp = TempDir::new().unwrap();
    let app = build_test_router(&tmp, None);
    (tmp, app, restore)
}

#[allow(clippy::await_holding_lock)]
async fn assert_csp_from_env(raw_policy: Option<&str>, expected_policy: &str) {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let (_tmp, app, _restore) = build_test_router_with_csp_env(raw_policy);
    let response = get_response(app, "/health").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_value(response.headers(), "content-security-policy").as_deref(),
        Some(expected_policy)
    );
    assert_security_headers("GET /health with CSP env", response.headers());
}

#[tokio::test]
async fn health_surface_has_expected_security_headers() {
    assert_surface_security_headers("GET /health", "/health").await;
}

#[tokio::test]
async fn dashboard_surface_has_expected_security_headers() {
    assert_surface_security_headers("GET /dashboard/", "/dashboard/").await;
}

#[tokio::test]
async fn swagger_surface_has_expected_security_headers() {
    assert_surface_security_headers("GET /swagger-ui/", "/swagger-ui/").await;
}

#[tokio::test]
async fn unset_csp_env_uses_strict_default_security_header_policy() {
    assert_csp_from_env(None, EXPECTED_SECURITY_HEADERS[0].1).await;
}

#[tokio::test]
async fn empty_csp_env_uses_strict_default_security_header_policy() {
    assert_csp_from_env(Some(""), EXPECTED_SECURITY_HEADERS[0].1).await;
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn valid_csp_env_overrides_default_security_header_policy() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let (_tmp, app, _restore) =
        build_test_router_with_csp_env(Some(CUSTOM_CONTENT_SECURITY_POLICY));
    let response = get_response(app, "/health").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_value(response.headers(), "content-security-policy").as_deref(),
        Some(CUSTOM_CONTENT_SECURITY_POLICY)
    );
    assert!(
        header_value(response.headers(), "strict-transport-security").is_none(),
        "HSTS must remain absent until the server has an HTTPS listener"
    );
}

#[tokio::test]
async fn invalid_csp_env_fails_closed_to_strict_default_security_header_policy() {
    assert_csp_from_env(
        Some("default-src 'self'\nscript-src 'none'"),
        EXPECTED_SECURITY_HEADERS[0].1,
    )
    .await;
}

#[tokio::test]
async fn swagger_ui_script_shape_is_recorded_for_csp_policy() {
    let tmp = TempDir::new().unwrap();
    let app = build_test_router(&tmp, None);
    let response = get_response(app, "/swagger-ui/").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_text(response).await;
    println!("SWAGGER_UI_HTML_START\n{body}\nSWAGGER_UI_HTML_END");

    let script_tags = script_opening_tags(&body);
    for tag in &script_tags {
        println!("SWAGGER_SCRIPT_TAG={tag}");
    }

    let inline_script_present = script_tags.iter().any(|tag| !script_tag_has_src(tag));
    println!("SWAGGER_INLINE_SCRIPT_PRESENT={inline_script_present}");

    assert_eq!(
        script_tags, EXPECTED_SWAGGER_SCRIPT_TAGS,
        "Swagger script opening tags changed"
    );
    assert!(
        !inline_script_present,
        "Swagger UI should continue to serve same-origin external scripts so the strict script-src 'self' contract remains sufficient"
    );
}

#[test]
fn max_body_mb_from_value_defaults_to_100_when_unset() {
    assert_eq!(max_body_mb_from_value(None), 100);
}

#[test]
fn max_body_mb_from_value_parses_valid_integer() {
    assert_eq!(max_body_mb_from_value(Some("50")), 50);
    assert_eq!(max_body_mb_from_value(Some("0")), 0);
}

#[test]
fn max_body_mb_from_value_defaults_to_100_for_invalid_values() {
    assert_eq!(max_body_mb_from_value(Some("abc")), 100);
    assert_eq!(max_body_mb_from_value(Some("")), 100);
    assert_eq!(max_body_mb_from_value(Some("-1")), 100);
}

#[test]
fn request_timeout_secs_from_value_uses_default_when_unset_or_invalid() {
    assert_eq!(
        request_timeout_secs_from_value(None),
        DEFAULT_REQUEST_TIMEOUT_SECS
    );
    assert_eq!(
        request_timeout_secs_from_value(Some("")),
        DEFAULT_REQUEST_TIMEOUT_SECS
    );
    assert_eq!(
        request_timeout_secs_from_value(Some("abc")),
        DEFAULT_REQUEST_TIMEOUT_SECS
    );
    assert_eq!(
        request_timeout_secs_from_value(Some("0")),
        DEFAULT_REQUEST_TIMEOUT_SECS
    );
}

#[test]
fn request_timeout_secs_from_value_parses_positive_integer() {
    assert_eq!(request_timeout_secs_from_value(Some("7")), 7);
}

#[tokio::test(start_paused = true)]
async fn bulk_replace_upload_outlives_global_request_timeout_until_body_eof() {
    let (_tmp, app, _key_store) = test_router_with_resource_bounds(ResourceBounds {
        request_timeout: Duration::from_millis(500),
        max_concurrent_requests: DEFAULT_MAX_CONCURRENT_REQUESTS,
    });
    let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(1);
    tx.send(Ok(axum::body::Bytes::from_static(
        b"{\"objectID\":\"slow-one\"}\n",
    )))
    .await
    .unwrap();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/1/migrations/bulk-replace?indexName=slow_upload")
        .header("content-type", "application/x-ndjson")
        .header("x-algolia-api-key", "admin-key")
        .header("x-algolia-application-id", "timeout-contract-app")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap();
    let submission = tokio::spawn(app.oneshot(request));

    let body_poll_permit = tokio::time::timeout(Duration::from_secs(10), tx.reserve())
        .await
        .expect("bulk replacement should start consuming the request body")
        .unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        !submission.is_finished(),
        "the generic request timeout must not abort a bounded bulk-replace upload"
    );
    drop(body_poll_permit);
    drop(tx);

    let response = tokio::time::timeout(Duration::from_secs(10), submission)
        .await
        .expect("bulk replacement should admit after request-body EOF")
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}

#[tokio::test(start_paused = true)]
async fn bulk_replace_upload_is_still_bounded_against_slow_clients() {
    let (_tmp, app, _key_store) = test_router_with_resource_bounds(ResourceBounds {
        request_timeout: Duration::from_millis(20),
        max_concurrent_requests: DEFAULT_MAX_CONCURRENT_REQUESTS,
    });
    let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(1);
    tx.send(Ok(axum::body::Bytes::from_static(
        b"{\"objectID\":\"slow-one\"}\n",
    )))
    .await
    .unwrap();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/1/migrations/bulk-replace?indexName=bounded_slow_upload")
        .header("content-type", "application/x-ndjson")
        .header("x-algolia-api-key", "admin-key")
        .header("x-algolia-application-id", "timeout-contract-app")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap();
    let submission = tokio::spawn(app.oneshot(request));

    let body_poll_permit = tokio::time::timeout(Duration::from_secs(10), tx.reserve())
        .await
        .expect("bulk replacement should start consuming the request body")
        .unwrap();
    drop(body_poll_permit);
    tokio::time::advance(Duration::from_millis(121)).await;

    let response = tokio::time::timeout(Duration::from_secs(1), submission)
        .await
        .expect("a stalled bulk-replace upload must have a finite deadline")
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
}

#[tokio::test]
async fn global_request_timeout_still_bounds_other_routes() {
    let app = Router::new()
        .route(
            "/slow",
            post(|| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                StatusCode::OK
            }),
        )
        .route(
            BULK_REPLACE_UPLOAD_PATH,
            get(|| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                StatusCode::OK
            }),
        )
        .layer(middleware::from_fn(|request, next| {
            enforce_request_timeout(request, next, Duration::from_millis(10))
        }));

    for (method, path) in [
        (Method::POST, "/slow"),
        (Method::GET, BULK_REPLACE_UPLOAD_PATH),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::REQUEST_TIMEOUT,
            "{method} {path} must remain subject to the global request timeout"
        );
    }
}

#[test]
fn max_concurrent_requests_from_value_uses_default_when_unset_or_invalid() {
    assert_eq!(
        max_concurrent_requests_from_value(None),
        DEFAULT_MAX_CONCURRENT_REQUESTS
    );
    assert_eq!(
        max_concurrent_requests_from_value(Some("")),
        DEFAULT_MAX_CONCURRENT_REQUESTS
    );
    assert_eq!(
        max_concurrent_requests_from_value(Some("abc")),
        DEFAULT_MAX_CONCURRENT_REQUESTS
    );
    assert_eq!(
        max_concurrent_requests_from_value(Some("0")),
        DEFAULT_MAX_CONCURRENT_REQUESTS
    );
}

#[test]
fn max_concurrent_requests_from_value_parses_positive_integer() {
    assert_eq!(max_concurrent_requests_from_value(Some("17")), 17);
}

#[cfg(feature = "fault-injection")]
#[serial_test::serial(resource_bounds_env)]
#[tokio::test]
async fn fault_sleep_times_out_with_canonical_json_error() {
    let (_tmp, app, _key_store) = test_router_with_resource_bounds(ResourceBounds {
        request_timeout: Duration::from_secs(1),
        max_concurrent_requests: DEFAULT_MAX_CONCURRENT_REQUESTS,
    });

    let response = fault_request(app, "/internal/fault/sleep").await;

    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_json_content_type(&response);
    assert_eq!(
        body_text(response).await,
        r#"{"message":"Request timed out","status":408}"#
    );
}

#[cfg(feature = "fault-injection")]
#[serial_test::serial(resource_bounds_env)]
#[tokio::test]
async fn fault_panic_returns_canonical_json_error_with_request_id() {
    let (_tmp, app, _key_store) = fault_test_router();

    let response = fault_request(app, "/internal/fault/panic").await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        response.headers().get(REQUEST_ID_HEADER_NAME).is_some(),
        "panic response should include x-request-id"
    );
    assert_json_content_type(&response);
    assert_eq!(
        body_text(response).await,
        r#"{"message":"Internal server error","status":500}"#
    );
}

#[cfg(feature = "fault-injection")]
#[serial_test::serial(resource_bounds_env)]
#[tokio::test]
async fn global_concurrency_limit_queues_health_while_fault_sleep_owns_slot() {
    let (_tmp, app, _key_store) = test_router_with_resource_bounds(ResourceBounds {
        request_timeout: Duration::from_secs(5),
        max_concurrent_requests: 1,
    });
    let logs = SharedLogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .with_writer(logs.clone())
        .finish();
    let _log_guard = tracing::subscriber::set_default(subscriber);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    let client = reqwest::Client::new();

    let sleep_client = client.clone();
    let sleep_url = format!("{base_url}/internal/fault/sleep");
    let mut sleep_task = tokio::spawn(async move {
        sleep_client
            .get(sleep_url)
            .header("x-algolia-application-id", "resource-bounds-test")
            .header("x-algolia-api-key", "admin-key")
            .send()
            .await
            .unwrap()
    });
    wait_for_fault_sleep_marker(&logs).await;

    let health_url = format!("{base_url}/health");
    let mut health_task = tokio::spawn(async move { client.get(health_url).send().await.unwrap() });
    tokio::select! {
        _ = &mut health_task => panic!("health completed while the sleep request held the only global slot"),
        _ = tokio::time::sleep(Duration::from_millis(250)) => {}
    }

    let sleep_response = tokio::select! {
        health_response = &mut health_task => {
            let response = health_response.unwrap();
            panic!(
                "health completed before sleep released the only global slot with status {}",
                response.status()
            );
        }
        sleep_response = &mut sleep_task => sleep_response.unwrap(),
    };
    assert_eq!(sleep_response.status(), reqwest::StatusCode::OK);

    let health_response = health_task.await.unwrap();
    assert_eq!(health_response.status(), reqwest::StatusCode::OK);
    let _ = shutdown_tx.send(());
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

/// TODO: Document body_limit_from_env_rejects_payload_over_limit.
#[tokio::test]
async fn body_limit_from_env_rejects_payload_over_limit() {
    // `with_env_var` holds ENV_MUTEX for the guard's whole lifetime and
    // restores under that lock. Setting the variable with an unlocked
    // `EnvVarRestoreGuard` instead leaves the restore racing every sibling that
    // reads `FLAPJACK_MAX_BODY_MB` while building its own router.
    let _env_guard = with_env_var("FLAPJACK_MAX_BODY_MB", "1");
    let tmp = TempDir::new().unwrap();
    let app = build_test_router(&tmp, None);
    // The router captured its limit at build time, so release the lock before
    // awaiting rather than holding ENV_MUTEX across `.await`.
    drop(_env_guard);
    let oversized_payload = serde_json::json!({
        "uid": "body-limit-over",
        "padding": "a".repeat(1_048_577)
    });

    let response = send_json_request(&app, Method::POST, "/1/indexes", oversized_payload).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn body_limit_from_env_allows_payload_under_limit() {
    // Same locking contract as `body_limit_from_env_rejects_payload_over_limit`.
    let _env_guard = with_env_var("FLAPJACK_MAX_BODY_MB", "1");
    let tmp = TempDir::new().unwrap();
    let app = build_test_router(&tmp, None);
    drop(_env_guard);
    let small_payload = serde_json::json!({ "uid": "body-limit-under" });

    let response = send_json_request(&app, Method::POST, "/1/indexes", small_payload).await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// TODO: Document build_router_open_mode_allows_protected_routes_without_auth_layer.
#[tokio::test]
async fn build_router_open_mode_allows_protected_routes_without_auth_layer() {
    let tmp = TempDir::new().unwrap();
    let app = build_test_router(&tmp, None);

    let create_resp = send_json_request(
        &app,
        Method::POST,
        "/1/indexes",
        serde_json::json!({ "uid": "test" }),
    )
    .await;
    assert_eq!(create_resp.status(), axum::http::StatusCode::OK);

    let search_resp = send_json_request(
        &app,
        Method::POST,
        "/1/indexes/test/query",
        serde_json::json!({ "query": "test" }),
    )
    .await;
    assert_eq!(search_resp.status(), axum::http::StatusCode::OK);

    let body = body_json(search_resp).await;
    assert!(
        body.get("hits").is_some(),
        "search response should include hits"
    );
}
/// TODO: Document build_router_open_mode_allows_dictionary_routes_without_auth_layer.
#[tokio::test]
async fn build_router_open_mode_allows_dictionary_routes_without_auth_layer() {
    let tmp = TempDir::new().unwrap();
    let app = build_test_router(&tmp, None);

    let batch_resp = send_json_request(
        &app,
        Method::POST,
        "/1/dictionaries/stopwords/batch",
        serde_json::json!({
            "clearExistingDictionaryEntries": false,
            "requests": [
                {
                    "action": "addEntry",
                    "body": {
                        "objectID": "open-mode-1",
                        "word": "alpha",
                        "language": "en"
                    }
                }
            ]
        }),
    )
    .await;
    assert_eq!(batch_resp.status(), axum::http::StatusCode::OK);

    let search_resp = send_json_request(
        &app,
        Method::POST,
        "/1/dictionaries/stopwords/search",
        serde_json::json!({ "query": "alpha" }),
    )
    .await;
    assert_eq!(search_resp.status(), axum::http::StatusCode::OK);

    let body = body_json(search_resp).await;
    assert_eq!(body["nbHits"].as_u64(), Some(1));
}

#[tokio::test]
async fn build_router_open_mode_does_not_expose_internal_routes() {
    let tmp = TempDir::new().unwrap();
    let app = build_test_router(&tmp, None);

    let response = send_empty_request(&app, Method::GET, "/internal/storage").await;
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
}
/// TODO: Document build_router_does_not_log_trusted_proxy_initialization.
#[tokio::test]
async fn build_router_does_not_log_trusted_proxy_initialization() {
    let tmp = TempDir::new().unwrap();
    let logs = SharedLogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .with_writer(logs.clone())
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        let _ = build_test_router(&tmp, None);
    });

    assert!(
        !logs.contents().contains("Trusted proxy header forwarding"),
        "router construction should not re-log trusted proxy initialization"
    );
}
/// TODO: Document cors_preflight_returns_expected_allow_origin_for_restricted_and_loopback_modes.
#[tokio::test]
async fn cors_preflight_returns_expected_allow_origin_for_restricted_and_loopback_modes() {
    let restricted_router = Router::new()
        .route("/cors", post(|| async { axum::http::StatusCode::OK }))
        .layer(build_cors_layer(&CorsMode::Restricted(vec![
            "https://allowed.example".parse().unwrap(),
        ])));

    let restricted_response = restricted_router
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/cors")
                .header("origin", "https://allowed.example")
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "content-type")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let restricted_origin = restricted_response
        .headers()
        .get("access-control-allow-origin")
        .and_then(|value| value.to_str().ok());
    assert_eq!(restricted_origin, Some("https://allowed.example"));

    let loopback_router = Router::new()
        .route("/cors", post(|| async { axum::http::StatusCode::OK }))
        .layer(build_cors_layer(&CorsMode::LoopbackOnly));
    let loopback_response = loopback_router
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/cors")
                .header("origin", "http://127.0.0.1:5173")
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "content-type")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let loopback_origin = loopback_response
        .headers()
        .get("access-control-allow-origin")
        .and_then(|value| value.to_str().ok());
    assert_eq!(loopback_origin, Some("http://127.0.0.1:5173"));
}

/// TODO: Document cors_preflight_blocks_non_loopback_origins_in_loopback_mode.
#[tokio::test]
async fn cors_preflight_blocks_non_loopback_origins_in_loopback_mode() {
    let app = Router::new()
        .route("/cors", post(|| async { axum::http::StatusCode::OK }))
        .layer(build_cors_layer(&CorsMode::LoopbackOnly));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/cors")
                .header("origin", "https://app.example.com")
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "content-type")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none(),
        "non-loopback origin should not receive access-control-allow-origin in default mode"
    );
}
/// TODO: Document cors_preflight_rejects_blocked_origins_in_restricted_mode.
#[tokio::test]
async fn cors_preflight_rejects_blocked_origins_in_restricted_mode() {
    let app = Router::new()
        .route("/cors", post(|| async { axum::http::StatusCode::OK }))
        .layer(build_cors_layer(&CorsMode::Restricted(vec![
            "https://allowed.example".parse().unwrap(),
        ])));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/cors")
                .header("origin", "https://blocked.example")
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "content-type")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none(),
        "blocked origin should not receive access-control-allow-origin"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn build_router_accepts_non_utf8_data_dir_paths() {
    let tmp = TempDir::new().unwrap();
    let non_utf8_path = Path::new(OsStr::from_bytes(b"test-\xFF-data"));
    let app = build_test_router_for_data_dir(&tmp, None, non_utf8_path);

    let health_resp = send_empty_request(&app, Method::GET, "/health").await;
    assert_eq!(health_resp.status(), axum::http::StatusCode::OK);
}

#[tokio::test]
async fn metrics_returns_403_without_auth_headers() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let resp = send_empty_request(&app, Method::GET, "/metrics").await;
    assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(resp).await,
        serde_json::json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );
}
/// TODO: Document metrics_returns_200_with_admin_key_only.
#[tokio::test]
async fn metrics_returns_200_with_admin_key_only() {
    use axum::body::Body;
    use axum::http::Request;
    use flapjack::types::{Document, FieldValue};
    use std::collections::HashMap;
    use tower::ServiceExt;

    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let (app, state) = build_test_router_with_state(&tmp, Some(key_store));

    state.manager.create_tenant("metrics_auth_tenant").unwrap();
    state
        .manager
        .add_documents_sync(
            "metrics_auth_tenant",
            vec![Document {
                id: "d1".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("router-metrics-seed".to_string()),
                )]),
            }],
        )
        .await
        .unwrap();

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

    let body = body_text(resp).await;
    let oplog_line = body
        .lines()
        .find(|line| {
            line.contains("flapjack_oplog_current_seq")
                && line.contains("metrics_auth_tenant")
                && !line.starts_with('#')
        })
        .unwrap_or_else(|| {
            panic!(
                "expected flapjack_oplog_current_seq for metrics_auth_tenant in:\n{}",
                body
            )
        });
    let value: f64 = oplog_line
        .split_whitespace()
        .last()
        .expect("missing metric value")
        .parse()
        .expect("metric value should parse as f64");
    assert!(value > 0.0, "expected positive oplog seq, got: {value}");
}

/// Proves that the `DefaultBodyLimit` layer returns HTTP 413 with an
/// Algolia-compatible JSON error when the request body exceeds the configured
/// `FLAPJACK_MAX_BODY_MB`. The `ensure_json_errors` middleware wraps the
/// plain-text rejection from Axum's body-limit extractor into
/// `{"message": "...", "status": 413}`, ensuring clients always receive JSON.
/// This prevents denial-of-service via unbounded request bodies.
#[tokio::test]
async fn oversized_body_returns_413_json_error() {
    let tmp = TempDir::new().unwrap();

    // Set a 1 MB body limit for this test, then build the router while the
    // env var is active. The guard restores the previous value on drop.
    let _env_guard = with_env_var("FLAPJACK_MAX_BODY_MB", "1");
    let app = build_test_router(&tmp, None);
    drop(_env_guard);

    // Build a body slightly over 1 MB.
    let oversized = vec![b'x'; 1_048_576 + 1024];

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/products/batch")
                .header("content-type", "application/json")
                .body(Body::from(oversized))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        axum::http::StatusCode::PAYLOAD_TOO_LARGE,
        "oversized body must be rejected with 413"
    );

    let body = body_json(resp).await;
    assert_eq!(
        body["status"], 413,
        "JSON error wrapper must include status 413"
    );
    assert!(
        body["message"].as_str().is_some(),
        "JSON error wrapper must include a message string"
    );
}
