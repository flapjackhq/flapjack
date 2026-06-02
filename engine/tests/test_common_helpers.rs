//! Shared-helper tests that should run once in their own binary.
//!
//! Keeping these out of owner binaries avoids duplicating helper-only coverage
//! across unrelated integration suites like replication and snapshot restore.

mod common;

use axum::{
    http::{HeaderMap, Method},
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::tempdir;

async fn assert_eventually_unreachable(client: &reqwest::Client, addr: &str) {
    for _ in 0..20 {
        if client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .is_err()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    panic!("server still accepted requests after expected shutdown");
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
fn published_task_shape_rejects_extra_fields() {
    assert!(
        common::assertions::published_task_shape_error(&json!({
            "status": "published",
            "pendingTask": false,
            "taskID": 123
        }))
        .is_some(),
        "published-task assertion must reject extra fields"
    );
}

#[test]
fn uses_default_when_missing() {
    assert_eq!(common::fixtures::parse_usize_or_default(None, 123), 123);
}

#[test]
fn uses_default_when_invalid() {
    assert_eq!(
        common::fixtures::parse_usize_or_default(Some("bad"), 77),
        77
    );
}

#[test]
fn parses_valid_usize() {
    assert_eq!(
        common::fixtures::parse_usize_or_default(Some("10000"), 1),
        10_000
    );
}

#[tokio::test(flavor = "current_thread")]
async fn send_json_response_with_headers_preserves_single_content_type_header() {
    let app = Router::new().route("/inspect", post(inspect_request));

    let response = common::http::send_json_response_with_headers(
        &app,
        Method::POST,
        "/inspect",
        Some(json!({"hello": "world"})),
        &[("x-test", "1"), ("content-type", "application/custom+json")],
    )
    .await;

    let body = common::http::parse_response_json(response).await;
    assert_eq!(body["content_type"], json!("application/custom+json"));
    assert_eq!(body["content_type_count"], json!(1));
    assert_eq!(body["x_test"], json!("1"));
    assert_eq!(body["body"], json!("{\"hello\":\"world\"}"));
}

#[tokio::test(flavor = "current_thread")]
async fn send_empty_response_omits_content_type_header() {
    let app = Router::new().route("/inspect", get(inspect_empty_request));

    let response = common::http::send_empty_response(&app, Method::GET, "/inspect").await;

    let body = common::http::parse_response_json(response).await;
    assert_eq!(body["content_type"], Value::Null);
    assert_eq!(body["content_type_count"], json!(0));
}

#[tokio::test]
async fn make_test_app_state_wires_manager_dictionary_and_defaults() {
    let tmp = tempdir().expect("tempdir");
    let state = common::state::make_test_app_state(tmp.path(), None, None, None, None, None, None);

    let manager_dm = state
        .manager
        .dictionary_manager()
        .expect("manager dictionary should be wired");
    assert!(Arc::ptr_eq(manager_dm, &state.dictionary_manager));
    assert!(state.key_store.is_none());
    assert!(state.replication_manager.is_none());
    assert!(state.analytics_engine.is_none());
    assert!(state.experiment_store.is_none());
}

#[tokio::test]
async fn make_test_app_state_preserves_manager_override_and_rewires_dictionary() {
    let tmp = tempdir().expect("tempdir");
    let manager_override = Arc::new(flapjack::IndexManager::new(tmp.path()));

    let state = common::state::make_test_app_state(
        tmp.path(),
        Some(Arc::clone(&manager_override)),
        None,
        None,
        None,
        None,
        None,
    );

    assert!(Arc::ptr_eq(&state.manager, &manager_override));
    let manager_dm = state
        .manager
        .dictionary_manager()
        .expect("manager dictionary should be wired");
    assert!(Arc::ptr_eq(manager_dm, &state.dictionary_manager));
}

#[tokio::test]
async fn dropping_temp_dir_stops_attached_server() {
    let (app, mut temp_dir) = common::state::build_test_app_for_local_requests(None);
    let addr = common::state::spawn_router(app, &mut temp_dir).await;
    let client = reqwest::Client::new();

    let ready = client
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .expect("server should answer before drop");
    assert!(ready.status().is_success());

    drop(temp_dir);

    assert_eventually_unreachable(&client, &addr).await;
}

#[tokio::test]
async fn reattaching_temp_dir_server_shuts_down_previous_node() {
    let (first_app, mut temp_dir) = common::state::build_test_app_for_local_requests(None);
    let first_addr = common::state::spawn_router(first_app, &mut temp_dir).await;
    let client = reqwest::Client::new();

    let ready_first = client
        .get(format!("http://{first_addr}/health"))
        .send()
        .await
        .expect("first server should answer before replacement");
    assert!(ready_first.status().is_success());

    let second_app = common::state::build_test_app_for_existing_data_dir(temp_dir.path(), None);
    let second_addr = common::state::spawn_router(second_app, &mut temp_dir).await;
    assert_ne!(first_addr, second_addr);

    let ready_second = client
        .get(format!("http://{second_addr}/health"))
        .send()
        .await
        .expect("second server should answer after replacement");
    assert!(ready_second.status().is_success());

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert!(
        client
            .get(format!("http://{first_addr}/health"))
            .send()
            .await
            .is_err(),
        "previously attached server should stop promptly after replacement"
    );
}

#[test]
fn local_request_helper_assertions_have_single_owner_boundary() {
    let http_source = include_str!("common/http.rs");
    assert!(
        !http_source.contains("mod local_request_helper_tests"),
        "engine/tests/common/http.rs must not own local-request helper assertions once test_common_helpers.rs is the canonical owner"
    );
}

fn run_local_request_test(test: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("local helper tests should create a runtime")
        .block_on(test);
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
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

async fn spawn_plaintext_server(status: axum::http::StatusCode, body: &'static str) -> String {
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
                    get(|| async {
                        (
                            axum::http::StatusCode::UNAUTHORIZED,
                            "auth failed as plain text",
                        )
                    }),
                );

                let response = common::http::send_empty_response(&app, Method::GET, "/plain").await;
                let _ = common::http::parse_response_json(response).await;
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
                let addr = spawn_plaintext_server(
                    axum::http::StatusCode::UNAUTHORIZED,
                    "auth failed as plain text",
                )
                .await;
                let client = reqwest::Client::new();
                common::http::wait_for_task_authed(&client, &addr, 1, None).await;
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
                let addr = spawn_plaintext_server(
                    axum::http::StatusCode::UNAUTHORIZED,
                    "auth failed as plain text",
                )
                .await;
                let client = reqwest::Client::new();
                let resp = client
                    .post(format!("http://{addr}/1/indexes/test/batch"))
                    .send()
                    .await
                    .expect("seed response should be returned");

                common::http::wait_for_response_task_authed(&client, &addr, resp, None).await;
            });
        },
    );
}
