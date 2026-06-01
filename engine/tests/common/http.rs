//! Stub summary for /Users/stuart/parallel_development/flapjack_dev/jun01_am_2_ha_contracts_ci_stabilization/flapjack_dev/engine/tests/common/http.rs.
use axum::extract::ConnectInfo;
use std::net::SocketAddr;

fn body_preview(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn parse_json_text_or_panic(
    context: &str,
    status: reqwest::StatusCode,
    body_text: &str,
) -> serde_json::Value {
    serde_json::from_str(body_text).unwrap_or_else(|err| {
        panic!(
            "{context}; status {status}; body preview: {}; decode error: {err}",
            body_preview(body_text, 500)
        )
    })
}

async fn parse_reqwest_response_json(
    context: &str,
    resp: reqwest::Response,
) -> (reqwest::StatusCode, serde_json::Value) {
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_else(|err| {
        panic!("{context}; failed to read response body for status {status}: {err}")
    });
    let body = parse_json_text_or_panic(context, status, &body_text);
    (status, body)
}

/// Poll the task endpoint until the task reaches "published" status.
/// Use this instead of blind sleeps after batch/write operations.
pub async fn wait_for_task(client: &reqwest::Client, addr: &str, task_id: i64) {
    wait_for_task_authed(client, addr, task_id, None).await;
}

/// Like `wait_for_task` but sends authentication headers (for servers with auth enabled).
pub async fn wait_for_task_authed(
    client: &reqwest::Client,
    addr: &str,
    task_id: i64,
    api_key: Option<&str>,
) {
    for _ in 0..5000 {
        let mut req = client.get(format!("http://{}/1/tasks/{}", addr, task_id));
        if let Some(key) = api_key {
            req = req
                .header("x-algolia-api-key", key)
                .header("x-algolia-application-id", "test");
        }
        let (_, body) = parse_reqwest_response_json(
            "wait_for_task_authed expected JSON task payload",
            req.send().await.unwrap(),
        )
        .await;
        match body["status"].as_str().unwrap_or("pending") {
            "published" => return,
            "error" => panic!(
                "Task {} failed with error: {}",
                task_id,
                body.get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown")
            ),
            _ => tokio::time::sleep(tokio::time::Duration::from_millis(10)).await,
        }
    }
    panic!("Task {} did not complete within 50s timeout", task_id);
}

/// Extract taskID from a batch/write response body and wait for it to complete.
pub async fn wait_for_response_task(client: &reqwest::Client, addr: &str, resp: reqwest::Response) {
    wait_for_response_task_authed(client, addr, resp, None).await;
}

/// Like `wait_for_response_task` but sends authentication headers.
pub async fn wait_for_response_task_authed(
    client: &reqwest::Client,
    addr: &str,
    resp: reqwest::Response,
    api_key: Option<&str>,
) {
    let (status, body) = parse_reqwest_response_json(
        "wait_for_response_task_authed expected JSON response body",
        resp,
    )
    .await;
    assert!(
        status.is_success(),
        "Expected 2xx response but got {}: {}",
        status,
        body
    );
    let task_id = body
        .get("taskID")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| panic!("Response missing taskID field: {}", body));
    wait_for_task_authed(client, addr, task_id, api_key).await;
}

/// Poll the task endpoint using an in-process router (no TCP binding needed).
/// Avoids socket-binding flakes in parallel test suites.
/// Uses provided admin_key or falls back to "test-admin-key-parity".
pub async fn wait_for_task_local(app: &axum::Router, task_id: i64) {
    wait_for_task_local_with_key(app, task_id, "test-admin-key-parity").await;
}

/// Poll the task endpoint using a custom admin key.
pub async fn wait_for_task_local_with_key(app: &axum::Router, task_id: i64, admin_key: &str) {
    for _ in 0..5000 {
        let resp = send_oneshot(
            app,
            axum::http::Method::GET,
            &format!("/1/tasks/{task_id}"),
            &[
                ("x-algolia-api-key", admin_key),
                ("x-algolia-application-id", "test"),
            ],
            axum::body::Body::empty(),
        )
        .await;
        let body = parse_response_json(resp).await;
        match body["status"].as_str().unwrap_or("pending") {
            "published" => return,
            "error" => panic!("Task {} failed: {}", task_id, body),
            _ => tokio::time::sleep(tokio::time::Duration::from_millis(10)).await,
        }
    }
    panic!("Task {} did not complete within 50s timeout", task_id);
}

/// Build an HTTP request with arbitrary method, URI, headers, and body.
/// Useful for in-process (oneshot) integration tests.
pub fn build_oneshot_request(
    method: axum::http::Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: axum::body::Body,
) -> axum::http::Request<axum::body::Body> {
    let mut builder = axum::http::Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(body).unwrap()
}

/// Send a oneshot request to an in-process router and return the response.
pub async fn send_oneshot(
    app: &axum::Router,
    method: axum::http::Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: axum::body::Body,
) -> axum::http::Response<axum::body::Body> {
    use tower::ServiceExt;
    app.clone()
        .oneshot(build_oneshot_request(method, uri, headers, body))
        .await
        .unwrap()
}

/// Send an unauthenticated in-process request with an optional JSON body.
pub async fn send_json_response(
    app: &axum::Router,
    method: axum::http::Method,
    uri: &str,
    body: Option<serde_json::Value>,
) -> axum::http::Response<axum::body::Body> {
    send_json_response_with_headers(app, method, uri, body, &[]).await
}

/// Send an unauthenticated in-process request with an optional JSON body and extra headers.
pub async fn send_json_response_with_headers(
    app: &axum::Router,
    method: axum::http::Method,
    uri: &str,
    body: Option<serde_json::Value>,
    extra_headers: &[(&str, &str)],
) -> axum::http::Response<axum::body::Body> {
    let mut headers: Vec<(&str, &str)> = extra_headers.to_vec();
    let has_content_type_header = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"));
    let body = if let Some(value) = body {
        if !has_content_type_header {
            headers.push(("content-type", "application/json"));
        }
        axum::body::Body::from(value.to_string())
    } else {
        axum::body::Body::empty()
    };
    send_oneshot(app, method, uri, &headers, body).await
}

/// Send an unauthenticated in-process request with no request body.
pub async fn send_empty_response(
    app: &axum::Router,
    method: axum::http::Method,
    uri: &str,
) -> axum::http::Response<axum::body::Body> {
    send_json_response(app, method, uri, None).await
}

/// Parse an axum response body as JSON.
pub async fn parse_response_json(
    resp: axum::http::Response<axum::body::Body>,
) -> serde_json::Value {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 10_000_000)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        let body_text = String::from_utf8_lossy(&bytes);
        panic!(
            "parse_response_json failed for HTTP status {status}; body preview: {}; decode error: {err}",
            body_preview(&body_text, 500)
        )
    })
}

/// Extract taskID from a JSON response body.
pub fn extract_task_id(body: &serde_json::Value) -> i64 {
    body["taskID"]
        .as_i64()
        .or_else(|| body["taskID"].as_u64().map(|v| v as i64))
        .unwrap_or_else(|| panic!("missing taskID in response: {body}"))
}

fn build_authed_request(
    method: axum::http::Method,
    uri: &str,
    key: &str,
    app_id: &str,
    extra_headers: &[(&str, &str)],
    body: Option<serde_json::Value>,
) -> axum::http::Request<axum::body::Body> {
    let mut builder = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("x-algolia-api-key", key)
        .header("x-algolia-application-id", app_id);

    let has_content_type_header = extra_headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"));
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }

    let req_body = if let Some(value) = body {
        if !has_content_type_header {
            builder = builder.header("content-type", "application/json");
        }
        axum::body::Body::from(value.to_string())
    } else {
        axum::body::Body::empty()
    };

    let mut req = builder.body(req_body).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
    req
}

/// Build an authenticated request with standard Algolia headers.
/// For tests that need non-admin keys, pass the desired key.
pub fn authed_request(
    method: axum::http::Method,
    uri: &str,
    key: &str,
    body: Option<serde_json::Value>,
) -> axum::http::Request<axum::body::Body> {
    build_authed_request(method, uri, key, "test", &[], body)
}

/// Send an authenticated request and return the raw response body.
pub async fn send_authed_response(
    app: &axum::Router,
    method: axum::http::Method,
    uri: &str,
    key: &str,
    app_id: &str,
    extra_headers: &[(&str, &str)],
    body: Option<serde_json::Value>,
) -> axum::http::Response<axum::body::Body> {
    use tower::ServiceExt;
    app.clone()
        .oneshot(build_authed_request(
            method,
            uri,
            key,
            app_id,
            extra_headers,
            body,
        ))
        .await
        .unwrap()
}

/// Parse an axum response body as JSON. Alias matching the pattern used across test files.
pub async fn body_json(resp: axum::http::Response<axum::body::Body>) -> serde_json::Value {
    parse_response_json(resp).await
}

/// Send an authenticated JSON request and return (StatusCode, parsed JSON body).
pub async fn send_json(
    app: &axum::Router,
    method: axum::http::Method,
    uri: &str,
    key: &str,
    body: Option<serde_json::Value>,
) -> (axum::http::StatusCode, serde_json::Value) {
    let resp = send_authed_response(app, method, uri, key, "test", &[], body).await;
    let status = resp.status();
    let body = parse_response_json(resp).await;
    (status, body)
}

/// Send an authenticated JSON request with extra headers and return (StatusCode, parsed JSON body).
pub async fn send_json_with_headers(
    app: &axum::Router,
    method: axum::http::Method,
    uri: &str,
    key: &str,
    body: Option<serde_json::Value>,
    extra_headers: &[(&str, &str)],
) -> (axum::http::StatusCode, serde_json::Value) {
    let resp = send_authed_response(app, method, uri, key, "test", extra_headers, body).await;
    let status = resp.status();
    let body = parse_response_json(resp).await;
    (status, body)
}

/// Send an authenticated request with a caller-supplied application ID and return (StatusCode, parsed JSON body).
///
/// Unlike [`send_json`] / [`send_json_with_headers`] which hardcode `app_id = "test"`,
/// this helper lets each test file supply its own application ID constant.
pub async fn send_authed(
    app: &axum::Router,
    method: axum::http::Method,
    uri: &str,
    key: &str,
    app_id: &str,
    extra_headers: &[(&str, &str)],
    body: Option<serde_json::Value>,
) -> (axum::http::StatusCode, serde_json::Value) {
    let resp = send_authed_response(app, method, uri, key, app_id, extra_headers, body).await;
    let status = resp.status();
    let body = parse_response_json(resp).await;
    (status, body)
}

#[cfg(test)]
mod local_request_helper_tests {
    use super::{
        parse_response_json, send_empty_response, send_json_response_with_headers,
        wait_for_response_task_authed, wait_for_task_authed,
    };
    use axum::{
        http::{HeaderMap, Method, StatusCode},
        response::Json,
        routing::{get, post},
        Router,
    };
    use serde_json::{json, Value};
    use std::any::Any;

    fn run_local_request_test(test: impl std::future::Future<Output = ()>) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("local helper tests should create a runtime")
            .block_on(test);
    }

    fn panic_message(panic: Box<dyn Any + Send>) -> String {
        if let Some(message) = panic.downcast_ref::<String>() {
            return message.clone();
        }
        if let Some(message) = panic.downcast_ref::<&str>() {
            return (*message).to_string();
        }
        "non-string panic payload".to_string()
    }

    fn assert_panic_contains(
        expected_substrings: &[&str],
        test: impl FnOnce() + std::panic::UnwindSafe,
    ) {
        let panic = std::panic::catch_unwind(test).expect_err("expected panic");
        let message = panic_message(panic);
        for expected in expected_substrings {
            assert!(
                message.contains(expected),
                "expected panic message to contain {expected:?}, got {message:?}"
            );
        }
    }

    async fn inspect_request(headers: HeaderMap, body: String) -> Json<Value> {
        Json(json!({
            "content_type": headers.get("content-type").and_then(|value| value.to_str().ok()),
            "content_type_count": headers.get_all("content-type").iter().count(),
            "x_test": headers.get("x-test").and_then(|value| value.to_str().ok()),
            "body": body,
        }))
    }

    async fn inspect_empty_request(headers: HeaderMap) -> Json<Value> {
        Json(json!({
            "content_type": headers.get("content-type").and_then(|value| value.to_str().ok()),
            "content_type_count": headers.get_all("content-type").iter().count(),
        }))
    }

    #[test]
    fn send_json_response_with_headers_preserves_single_content_type_header() {
        run_local_request_test(async {
            let app = Router::new().route("/inspect", post(inspect_request));

            let response = send_json_response_with_headers(
                &app,
                Method::POST,
                "/inspect",
                Some(json!({"hello": "world"})),
                &[("x-test", "1"), ("content-type", "application/custom+json")],
            )
            .await;

            let body = parse_response_json(response).await;
            assert_eq!(body["content_type"], json!("application/custom+json"));
            assert_eq!(body["content_type_count"], json!(1));
            assert_eq!(body["x_test"], json!("1"));
            assert_eq!(body["body"], json!("{\"hello\":\"world\"}"));
        });
    }

    #[test]
    fn send_empty_response_omits_content_type_header() {
        run_local_request_test(async {
            let app = Router::new().route("/inspect", get(inspect_empty_request));

            let response = send_empty_response(&app, Method::GET, "/inspect").await;

            let body = parse_response_json(response).await;
            assert_eq!(body["content_type"], Value::Null);
            assert_eq!(body["content_type_count"], json!(0));
        });
    }

    async fn spawn_plaintext_server(status: StatusCode, body: &'static str) -> String {
        let app = Router::new().fallback(move || async move { (status, body) });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let addr = listener
            .local_addr()
            .expect("test server should expose local addr")
            .to_string();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test server should run");
        });
        addr
    }

    #[test]
    fn parse_response_json_reports_status_and_body_on_non_json() {
        assert_panic_contains(
            &[
                "parse_response_json failed for HTTP status 401 Unauthorized",
                "body preview: auth failed as plain text",
            ],
            || {
                run_local_request_test(async {
                    let app = Router::new().route(
                        "/plain",
                        get(|| async { (StatusCode::UNAUTHORIZED, "auth failed as plain text") }),
                    );

                    let response = send_empty_response(&app, Method::GET, "/plain").await;
                    let _ = parse_response_json(response).await;
                });
            },
        );
    }

    #[test]
    fn wait_for_task_authed_reports_status_and_body_on_non_json() {
        assert_panic_contains(
            &[
                "wait_for_task_authed expected JSON task payload; status 401 Unauthorized",
                "body preview: auth failed as plain text",
            ],
            || {
                run_local_request_test(async {
                    let addr =
                        spawn_plaintext_server(StatusCode::UNAUTHORIZED, "auth failed as plain text")
                            .await;
                    let client = reqwest::Client::new();
                    wait_for_task_authed(&client, &addr, 1, None).await;
                });
            },
        );
    }

    #[test]
    fn wait_for_response_task_authed_reports_status_and_body_on_non_json() {
        assert_panic_contains(
            &[
                "wait_for_response_task_authed expected JSON response body; status 401 Unauthorized",
                "body preview: auth failed as plain text",
            ],
            || {
                run_local_request_test(async {
                    let addr =
                        spawn_plaintext_server(StatusCode::UNAUTHORIZED, "auth failed as plain text")
                            .await;
                    let client = reqwest::Client::new();
                    let resp = client
                        .post(format!("http://{addr}/1/indexes/test/batch"))
                        .send()
                        .await
                        .expect("seed response should be returned");

                    wait_for_response_task_authed(&client, &addr, resp, None).await;
                });
            },
        );
    }
}
