//! Stub summary for /Users/stuart/parallel_development/flapjack_dev/may31_12pm_4_idempotency_cache_durability/flapjack_dev/engine/flapjack-http/src/handlers/objects_tests.rs.
use super::*;
use axum::http::StatusCode;

// ── is_operation ──

#[test]
fn is_operation_true() {
    let v = serde_json::json!({"_operation": "Increment", "value": 1});
    assert!(is_operation(&v));
}

#[test]
fn is_operation_false_no_key() {
    let v = serde_json::json!({"name": "Alice"});
    assert!(!is_operation(&v));
}

#[test]
fn is_operation_false_not_object() {
    assert!(!is_operation(&serde_json::json!("hello")));
    assert!(!is_operation(&serde_json::json!(42)));
    assert!(!is_operation(&serde_json::json!(null)));
}

// ── apply_operation: Increment ──

#[test]
fn increment_integer() {
    let existing = Some(FieldValue::Integer(10));
    let result = apply_operation(existing.as_ref(), "Increment", &serde_json::json!(5));
    assert_eq!(result, Some(FieldValue::Integer(15)));
}

#[test]
fn increment_float() {
    let existing = Some(FieldValue::Float(1.5));
    let result = apply_operation(existing.as_ref(), "Increment", &serde_json::json!(0.5));
    assert_eq!(result, Some(FieldValue::Float(2.0)));
}

#[test]
fn increment_missing_field_integer() {
    let result = apply_operation(None, "Increment", &serde_json::json!(7));
    assert_eq!(result, Some(FieldValue::Integer(7)));
}

#[test]
fn increment_missing_field_float() {
    let result = apply_operation(None, "Increment", &serde_json::json!(2.5));
    assert_eq!(result, Some(FieldValue::Float(2.5)));
}

#[test]
fn increment_from_alias() {
    let existing = Some(FieldValue::Integer(3));
    let result = apply_operation(existing.as_ref(), "IncrementFrom", &serde_json::json!(10));
    assert_eq!(result, Some(FieldValue::Integer(13)));
}

// ── apply_operation: Decrement ──

#[test]
fn decrement_integer() {
    let existing = Some(FieldValue::Integer(10));
    let result = apply_operation(existing.as_ref(), "Decrement", &serde_json::json!(3));
    assert_eq!(result, Some(FieldValue::Integer(7)));
}

#[test]
fn decrement_float() {
    let existing = Some(FieldValue::Float(5.0));
    let result = apply_operation(existing.as_ref(), "Decrement", &serde_json::json!(1.5));
    assert_eq!(result, Some(FieldValue::Float(3.5)));
}

#[test]
fn decrement_missing_field() {
    let result = apply_operation(None, "Decrement", &serde_json::json!(4));
    assert_eq!(result, Some(FieldValue::Integer(-4)));
}

// ── apply_operation: Add ──

#[test]
fn add_to_existing_array() {
    let existing = Some(FieldValue::Array(vec![FieldValue::Text("a".into())]));
    let result = apply_operation(existing.as_ref(), "Add", &serde_json::json!("b"));
    let arr = match result {
        Some(FieldValue::Array(arr)) => arr,
        _ => panic!("expected array"),
    };
    assert_eq!(
        arr,
        vec![FieldValue::Text("a".into()), FieldValue::Text("b".into())]
    );
}

#[test]
fn add_to_none_creates_array() {
    let result = apply_operation(None, "Add", &serde_json::json!("x"));
    match result {
        Some(FieldValue::Array(arr)) => {
            assert_eq!(arr, vec![FieldValue::Text("x".into())]);
        }
        _ => panic!("expected array"),
    }
}

#[test]
fn add_to_non_array_wraps() {
    let existing = Some(FieldValue::Text("old".into()));
    let result = apply_operation(existing.as_ref(), "Add", &serde_json::json!("new"));
    let arr = match result {
        Some(FieldValue::Array(arr)) => arr,
        _ => panic!("expected array"),
    };
    assert_eq!(
        arr,
        vec![
            FieldValue::Text("old".into()),
            FieldValue::Text("new".into())
        ]
    );
}

// ── apply_operation: Remove ──

/// Verify that the `Remove` operation filters the matching element from an existing array, leaving the remaining elements intact.
#[test]
fn remove_from_array() {
    let existing = Some(FieldValue::Array(vec![
        FieldValue::Text("a".into()),
        FieldValue::Text("b".into()),
        FieldValue::Text("c".into()),
    ]));
    let result = apply_operation(existing.as_ref(), "Remove", &serde_json::json!("b"));
    match result {
        Some(FieldValue::Array(arr)) => {
            assert_eq!(arr.len(), 2);
            assert_eq!(arr[0], FieldValue::Text("a".into()));
            assert_eq!(arr[1], FieldValue::Text("c".into()));
        }
        _ => panic!("expected array"),
    }
}

#[test]
fn remove_nonexistent_item() {
    let existing = Some(FieldValue::Array(vec![FieldValue::Text("a".into())]));
    let result = apply_operation(existing.as_ref(), "Remove", &serde_json::json!("z"));
    match result {
        Some(FieldValue::Array(arr)) => {
            assert_eq!(arr, vec![FieldValue::Text("a".into())]);
        }
        _ => panic!("expected array"),
    }
}

#[test]
fn remove_from_non_array_returns_existing() {
    let existing = Some(FieldValue::Text("hello".into()));
    let result = apply_operation(existing.as_ref(), "Remove", &serde_json::json!("x"));
    assert_eq!(result, Some(FieldValue::Text("hello".into())));
}

// ── apply_operation: AddUnique ──

#[test]
fn add_unique_new_item() {
    let existing = Some(FieldValue::Array(vec![FieldValue::Text("a".into())]));
    let result = apply_operation(existing.as_ref(), "AddUnique", &serde_json::json!("b"));
    match result {
        Some(FieldValue::Array(arr)) => {
            assert_eq!(
                arr,
                vec![FieldValue::Text("a".into()), FieldValue::Text("b".into())]
            );
        }
        _ => panic!("expected array"),
    }
}

#[test]
fn add_unique_duplicate_item() {
    let existing = Some(FieldValue::Array(vec![FieldValue::Text("a".into())]));
    let result = apply_operation(existing.as_ref(), "AddUnique", &serde_json::json!("a"));
    match result {
        Some(FieldValue::Array(arr)) => {
            assert_eq!(arr, vec![FieldValue::Text("a".into())]);
        }
        _ => panic!("expected array"),
    }
}

#[test]
fn add_unique_to_none() {
    let result = apply_operation(None, "AddUnique", &serde_json::json!("x"));
    match result {
        Some(FieldValue::Array(arr)) => {
            assert_eq!(arr, vec![FieldValue::Text("x".into())]);
        }
        _ => panic!("expected array"),
    }
}

// ── apply_operation: edge cases ──

#[test]
fn unknown_operation_returns_none() {
    let result = apply_operation(None, "FooBar", &serde_json::json!(1));
    assert_eq!(result, None);
}

#[test]
fn increment_negative_delta() {
    let existing = Some(FieldValue::Integer(10));
    let result = apply_operation(existing.as_ref(), "Increment", &serde_json::json!(-3));
    assert_eq!(result, Some(FieldValue::Integer(7)));
}

#[test]
fn decrement_from_alias() {
    let existing = Some(FieldValue::Integer(10));
    let result = apply_operation(existing.as_ref(), "DecrementFrom", &serde_json::json!(4));
    assert_eq!(result, Some(FieldValue::Integer(6)));
}

#[test]
fn increment_set_alias() {
    let existing = Some(FieldValue::Integer(5));
    let result = apply_operation(existing.as_ref(), "IncrementSet", &serde_json::json!(2));
    assert_eq!(result, Some(FieldValue::Integer(7)));
}

#[test]
fn remove_from_empty_array() {
    let existing = Some(FieldValue::Array(vec![]));
    let result = apply_operation(existing.as_ref(), "Remove", &serde_json::json!("a"));
    match result {
        Some(FieldValue::Array(arr)) => assert!(arr.is_empty()),
        _ => panic!("expected empty array"),
    }
}

#[test]
fn add_unique_integer_dedup() {
    let existing = Some(FieldValue::Array(vec![FieldValue::Integer(42)]));
    let result = apply_operation(existing.as_ref(), "AddUnique", &serde_json::json!(42));
    match result {
        Some(FieldValue::Array(arr)) => {
            assert_eq!(arr, vec![FieldValue::Integer(42)]);
        }
        _ => panic!("expected array"),
    }
}

// ── Write handler guard (pause) tests ──────────────────────────────

use axum::body::Body;
use axum::http::Request;
use axum::routing::{delete, get, post, put};
use axum::Router;
use tempfile::TempDir;
use tower::ServiceExt;

/// Build a minimal `AppState` backed by a temporary directory for use in pause-guard and size-limit tests.
fn make_write_guard_state(tmp: &TempDir) -> Arc<AppState> {
    let mut state = crate::test_helpers::TestStateBuilder::new(tmp).build();
    state.metrics_state = None;
    Arc::new(state)
}

/// Build an Axum `Router` wired to the document write handlers for use in pause-guard integration tests.
fn make_write_guard_app(state: Arc<AppState>) -> Router {
    crate::router::app_id_layer(
        Router::new()
            .route("/1/indexes/:indexName/batch", post(super::add_documents))
            .route("/1/indexes/:indexName/:objectID", put(super::put_object))
            .route(
                "/1/indexes/:indexName/:objectID",
                delete(super::delete_object),
            )
            .route(
                "/1/indexes/:indexName/:objectID/partial",
                post(super::partial_update_object).put(super::partial_update_object),
            )
            .route(
                "/1/indexes/:indexName/deleteByQuery",
                post(super::delete_by_query),
            )
            .route("/1/indexes/:indexName", post(super::add_record_auto_id))
            .with_state(state),
    )
}

/// Verify that POST `/1/indexes/{indexName}/batch` returns 503 when the target index is paused.
#[tokio::test]
async fn test_add_documents_blocked_when_paused() {
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    state.paused_indexes.pause("test_index");
    let app = make_write_guard_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/test_index/batch")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("temporarily unavailable"));
}

/// Verify that PUT `/1/indexes/{indexName}/{objectID}` returns 503 when the target index is paused.
#[tokio::test]
async fn test_put_object_blocked_when_paused() {
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    state.paused_indexes.pause("test_index");
    let app = make_write_guard_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/1/indexes/test_index/obj1")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("temporarily unavailable"));
}

/// Verify that DELETE `/1/indexes/{indexName}/{objectID}` returns 503 when the target index is paused.
#[tokio::test]
async fn test_delete_object_blocked_when_paused() {
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    state.paused_indexes.pause("test_index");
    let app = make_write_guard_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/1/indexes/test_index/obj1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("temporarily unavailable"));
}

/// Verify that POST `/1/indexes/{indexName}/{objectID}/partial` returns 503 when the target index is paused.
#[tokio::test]
async fn test_partial_update_blocked_when_paused() {
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    state.paused_indexes.pause("test_index");
    let app = make_write_guard_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/test_index/obj1/partial")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("temporarily unavailable"));
}

/// Verify that POST `/1/indexes/{indexName}/deleteByQuery` returns 503 when the target index is paused.
#[tokio::test]
async fn test_delete_by_query_blocked_when_paused() {
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    state.paused_indexes.pause("test_index");
    let app = make_write_guard_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/test_index/deleteByQuery")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("temporarily unavailable"));
}

/// Verify that POST `/1/indexes/{indexName}` (auto-ID creation) returns 503 when the target index is paused.
#[tokio::test]
async fn test_add_record_auto_id_blocked_when_paused() {
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    state.paused_indexes.pause("test_index");
    let app = make_write_guard_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/test_index")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("temporarily unavailable"));
}

// ── Reads-unaffected tests (2G) ─────────────────────────────────────

fn make_read_write_app(state: Arc<AppState>) -> Router {
    crate::router::app_id_layer(
        Router::new()
            .route(
                "/1/indexes/:indexName/query",
                post(crate::handlers::search::search),
            )
            .route(
                "/1/indexes/:indexName/:objectID",
                axum::routing::get(super::get_object),
            )
            .route("/1/indexes/:indexName/batch", post(super::add_documents))
            .with_state(state),
    )
}

/// Verify that search requests are not blocked (no 503) when the target index is paused.
#[tokio::test]
async fn test_search_allowed_when_paused() {
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    state.paused_indexes.pause("test_index");
    let app = make_read_write_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/test_index/query")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"query":""}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should be anything but 503 — likely 404 (TenantNotFound) since no index exists
    assert_ne!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "search should NOT be blocked when index is paused; got 503"
    );
}

/// Verify that GET `/1/indexes/{indexName}/{objectID}` is not blocked (no 503) when the target index is paused.
#[tokio::test]
async fn test_get_object_allowed_when_paused() {
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    state.paused_indexes.pause("test_index");
    let app = make_read_write_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/1/indexes/test_index/obj1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Should be anything but 503 — likely 500 or 404
    assert_ne!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "get_object should NOT be blocked when index is paused; got 503"
    );
}

// ── Cross-index isolation test (2H) ─────────────────────────────────

/// Verify that pausing index "foo" does not block writes to a different index "bar".
#[tokio::test]
async fn test_pause_does_not_affect_other_indexes() {
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    // Pause "foo" but write to "bar"
    state.paused_indexes.pause("foo");
    let app = make_write_guard_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/bar/batch")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    // "bar" is not paused — should NOT be 503
    assert_ne!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "writes to 'bar' should NOT be blocked when only 'foo' is paused; got 503"
    );
}

// ── check_record_size unit tests ───────────────────────────────────

#[test]
fn record_size_empty_doc_ok() {
    let doc: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    assert!(check_record_size(&doc).is_ok());
}

#[test]
fn record_size_small_doc_ok() {
    let doc = serde_json::json!({"name": "Alice", "age": 30});
    assert!(check_record_size(&doc).is_ok());
}

/// Verify that a record whose serialized size equals exactly `max_record_bytes()` passes the size check.
#[test]
fn record_size_at_limit_ok() {
    // Determine overhead empirically: serialize {"k":""} and measure
    let mut empty_map = serde_json::Map::new();
    empty_map.insert("k".to_string(), serde_json::Value::String(String::new()));
    let overhead = serde_json::to_vec(&empty_map).unwrap().len(); // e.g. {"k":""}

    let limit = max_record_bytes();
    let value_len = limit - overhead;
    let value: String = "x".repeat(value_len);
    let mut map = serde_json::Map::new();
    map.insert("k".to_string(), serde_json::Value::String(value));
    let serialized_len = serde_json::to_vec(&map).unwrap().len();
    assert_eq!(
        serialized_len, limit,
        "test setup: doc must be exactly at limit"
    );
    assert!(check_record_size(&map).is_ok());
}

/// Verify that a record one byte over `max_record_bytes()` is rejected with `DocumentTooLarge`.
#[test]
fn record_size_one_byte_over_limit_rejected() {
    let mut empty_map = serde_json::Map::new();
    empty_map.insert("k".to_string(), serde_json::Value::String(String::new()));
    let overhead = serde_json::to_vec(&empty_map).unwrap().len();

    let limit = max_record_bytes();
    let value_len = limit - overhead + 1; // one byte over
    let value: String = "x".repeat(value_len);
    let mut map = serde_json::Map::new();
    map.insert("k".to_string(), serde_json::Value::String(value));
    let err = check_record_size(&map).unwrap_err();
    match err {
        FlapjackError::DocumentTooLarge { size, max } => {
            assert!(size > max);
        }
        other => panic!("expected DocumentTooLarge, got {:?}", other),
    }
}

// ── Per-record size limit: HTTP integration tests ──────────────────

fn make_ingest_app(state: Arc<AppState>) -> Router {
    crate::router::app_id_layer(
        Router::new()
            .route("/1/indexes/:indexName/batch", post(super::add_documents))
            .route("/1/indexes/:indexName", post(super::add_record_auto_id))
            .with_state(state),
    )
}

/// Verify that the batch endpoint returns 400 when any individual record exceeds `max_record_bytes()`.
#[tokio::test]
async fn batch_rejects_oversized_record() {
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    let app = make_ingest_app(state);

    let limit = max_record_bytes();
    // Build a value that is definitely over the limit
    let big_value: String = "x".repeat(limit + 1000);
    let body = serde_json::json!({
        "requests": [{
            "action": "addObject",
            "body": { "objectID": "big1", "data": big_value }
        }]
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/test/batch")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Verify that `deleteObject` batch operations skip the record size check and succeed even though delete bodies are trivially small.
#[tokio::test]
async fn batch_delete_bypasses_size_check() {
    // deleteObject carries only an ID — size check must NOT fire
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    let app = make_ingest_app(state);

    let body = serde_json::json!({
        "requests": [{
            "action": "deleteObject",
            "body": { "objectID": "nonexistent" }
        }]
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/test/batch")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should be 200 (or at worst 404), never 400 from size check
    assert_ne!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Verify that the auto-ID endpoint returns 400 when the record exceeds `max_record_bytes()`.
#[tokio::test]
async fn auto_id_rejects_oversized_record() {
    let tmp = TempDir::new().unwrap();
    let state = make_write_guard_state(&tmp);
    let app = make_ingest_app(state);

    let limit = max_record_bytes();
    let big_value: String = "x".repeat(limit + 1000);
    let body = serde_json::json!({ "data": big_value });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/test")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

mod stage4_multi_index {
    use super::*;
    use crate::test_helpers::body_json;
    use flapjack::types::{Document, TaskStatus};
    use serde_json::{json, Value};

    fn make_stage4_app(state: Arc<AppState>) -> Router {
        crate::router::app_id_layer(
            Router::new()
                .route("/1/indexes/:indexName/batch", post(super::add_documents))
                .route("/1/indexes/:indexName/objects", post(super::get_objects))
                .route(
                    "/1/indexes/:indexName",
                    get(crate::handlers::search::search_get),
                )
                .with_state(state),
        )
    }

    async fn post_json(app: &Router, uri: &str, body: Value) -> axum::http::Response<Body> {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn get_request(app: &Router, uri: &str) -> axum::http::Response<Body> {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn wait_for_task_succeeded(state: &Arc<AppState>, task_id: i64) {
        for _ in 0..200 {
            if let Ok(task) = state.manager.get_task(&task_id.to_string()) {
                if task.status == TaskStatus::Succeeded {
                    return;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
        panic!("task {task_id} did not reach succeeded status");
    }

    fn save_index_settings(
        state: &Arc<AppState>,
        index_name: &str,
        settings: &flapjack::index::settings::IndexSettings,
    ) {
        let dir = state.manager.base_path.join(index_name);
        std::fs::create_dir_all(&dir).unwrap();
        settings.save(dir.join("settings.json")).unwrap();
        state.manager.invalidate_settings_cache(index_name);
    }

    async fn seed_docs(state: &Arc<AppState>, index_name: &str, docs: Vec<Value>) {
        state.manager.create_tenant(index_name).unwrap();
        let mut documents: Vec<Document> = Vec::new();
        for doc in docs {
            documents.push(Document::from_json(&doc).unwrap());
        }
        state
            .manager
            .add_documents_sync(index_name, documents)
            .await
            .unwrap();
    }

    /// Write documents to two different indexes via `*/batch` and verify each document lands in the correct index.
    #[tokio::test]
    async fn multi_index_batch_write_different_indexes() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_stage4_app(state.clone());

        let resp = post_json(
            &app,
            "/1/indexes/*/batch",
            json!({
                "requests": [
                    { "action": "addObject", "indexName": "idx_a", "body": { "objectID": "a1", "title": "alpha one" } },
                    { "action": "addObject", "indexName": "idx_b", "body": { "objectID": "b1", "title": "beta one" } },
                    { "action": "addObject", "indexName": "idx_a", "body": { "objectID": "a2", "title": "alpha two" } }
                ]
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;

        assert_eq!(body["objectIDs"], json!(["a1", "a2", "b1"]));
        let task_a = body["taskID"]["idx_a"].as_i64().unwrap();
        let task_b = body["taskID"]["idx_b"].as_i64().unwrap();
        wait_for_task_succeeded(&state, task_a).await;
        wait_for_task_succeeded(&state, task_b).await;

        assert!(state.manager.get_document("idx_a", "a1").unwrap().is_some());
        assert!(state.manager.get_document("idx_a", "a2").unwrap().is_some());
        assert!(state.manager.get_document("idx_b", "b1").unwrap().is_some());
    }

    /// Verify the multi-index batch response contains `objectIDs` as an array and `taskID` as a per-index map.
    #[tokio::test]
    async fn multi_index_batch_response_format() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_stage4_app(state.clone());

        let resp = post_json(
            &app,
            "/1/indexes/*/batch",
            json!({
                "requests": [
                    { "action": "addObject", "indexName": "fmt_idx", "body": { "objectID": "fmt1", "title": "format" } }
                ]
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;

        assert_eq!(body["objectIDs"], json!(["fmt1"]));
        assert!(
            body["taskID"].is_object(),
            "taskID must be a map for * path"
        );
        let task_id = body["taskID"]["fmt_idx"].as_i64().unwrap();
        wait_for_task_succeeded(&state, task_id).await;
    }

    /// Exercise addObject, updateObject, partialUpdateObject, and deleteObject in a single multi-index batch and verify each action's effect on stored documents.
    #[tokio::test]
    async fn multi_index_batch_all_actions() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        seed_docs(
            &state,
            "all_actions_idx",
            vec![
                json!({"objectID": "upd1", "title": "before", "category": "old"}),
                json!({"objectID": "part1", "title": "partial", "count": 1}),
                json!({"objectID": "del1", "title": "delete me"}),
            ],
        )
        .await;
        let app = make_stage4_app(state.clone());

        let resp = post_json(
            &app,
            "/1/indexes/*/batch",
            json!({
                "requests": [
                    { "action": "addObject", "indexName": "all_actions_idx", "body": { "objectID": "add1", "title": "added" } },
                    { "action": "updateObject", "indexName": "all_actions_idx", "body": { "objectID": "upd1", "title": "after" } },
                    { "action": "partialUpdateObject", "indexName": "all_actions_idx", "body": { "objectID": "part1", "count": { "_operation": "Increment", "value": 2 } } },
                    { "action": "deleteObject", "indexName": "all_actions_idx", "body": { "objectID": "del1" } }
                ]
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let task_id = body["taskID"]["all_actions_idx"].as_i64().unwrap();
        wait_for_task_succeeded(&state, task_id).await;

        assert!(state
            .manager
            .get_document("all_actions_idx", "add1")
            .unwrap()
            .is_some());
        let updated = state
            .manager
            .get_document("all_actions_idx", "upd1")
            .unwrap()
            .unwrap();
        assert_eq!(
            updated.fields.get("title"),
            Some(&FieldValue::Text("after".to_string()))
        );
        assert!(
            !updated.fields.contains_key("category"),
            "updateObject must replace the object"
        );

        let partial = state
            .manager
            .get_document("all_actions_idx", "part1")
            .unwrap()
            .unwrap();
        assert_eq!(partial.fields.get("count"), Some(&FieldValue::Integer(3)));
        assert!(state
            .manager
            .get_document("all_actions_idx", "del1")
            .unwrap()
            .is_none());
    }

    /// Verify that a multi-index batch returns 400 with a descriptive message when an operation omits `indexName`.
    #[tokio::test]
    async fn multi_index_batch_missing_index_name_error() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_stage4_app(state);

        let resp = post_json(
            &app,
            "/1/indexes/*/batch",
            json!({
                "requests": [
                    { "action": "addObject", "body": { "objectID": "x1", "title": "missing index name" } }
                ]
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        let message = body["message"].as_str().unwrap_or_default();
        assert!(message.contains("Missing indexName"));
    }

    /// Verify that a single-index batch endpoint returns 400 when any operation includes an `indexName` field.
    #[tokio::test]
    async fn single_index_batch_rejects_index_name_in_body() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_stage4_app(state);

        let resp = post_json(
            &app,
            "/1/indexes/single_idx/batch",
            json!({
                "requests": [
                    { "action": "addObject", "indexName": "other_idx", "body": { "objectID": "x1", "title": "must fail" } }
                ]
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        let message = body["message"].as_str().unwrap_or_default();
        assert!(message.contains("The indexName attribute is only allowed on multiple indexes"));
    }

    /// Verify that `getObjects` retrieves documents from multiple indexes in a single request.
    #[tokio::test]
    async fn multi_index_get_objects_different_indexes() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        seed_docs(
            &state,
            "obj_idx_a",
            vec![json!({"objectID": "a1", "title": "alpha"})],
        )
        .await;
        seed_docs(
            &state,
            "obj_idx_b",
            vec![json!({"objectID": "b1", "title": "beta"})],
        )
        .await;
        let app = make_stage4_app(state);

        let resp = post_json(
            &app,
            "/1/indexes/*/objects",
            json!({
                "requests": [
                    { "indexName": "obj_idx_a", "objectID": "a1" },
                    { "indexName": "obj_idx_b", "objectID": "b1" }
                ]
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["results"][0]["objectID"], "a1");
        assert_eq!(body["results"][1]["objectID"], "b1");
    }

    /// Verify that `getObjects` returns `null` entries for object IDs that do not exist in the index.
    #[tokio::test]
    async fn multi_index_get_objects_null_for_missing() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        seed_docs(
            &state,
            "obj_missing_idx",
            vec![json!({"objectID": "exists", "title": "present"})],
        )
        .await;
        let app = make_stage4_app(state);

        let resp = post_json(
            &app,
            "/1/indexes/*/objects",
            json!({
                "requests": [
                    { "indexName": "obj_missing_idx", "objectID": "exists" },
                    { "indexName": "obj_missing_idx", "objectID": "does_not_exist" }
                ]
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body["results"][0].is_object());
        assert!(body["results"][1].is_null());
    }

    /// Verify that `attributesToRetrieve` filters the returned fields, keeping only the requested attributes plus `objectID`.
    #[tokio::test]
    async fn multi_index_get_objects_attributes_to_retrieve() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        seed_docs(
            &state,
            "obj_attrs_idx",
            vec![json!({"objectID": "a1", "title": "alpha", "category": "electronics", "brand": "acme"})],
        )
        .await;
        let app = make_stage4_app(state);

        let resp = post_json(
            &app,
            "/1/indexes/*/objects",
            json!({
                "requests": [
                    { "indexName": "obj_attrs_idx", "objectID": "a1", "attributesToRetrieve": ["title"] }
                ]
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let obj = &body["results"][0];
        assert_eq!(obj["objectID"], "a1");
        assert_eq!(obj["title"], "alpha");
        assert!(obj.get("category").is_none());
        assert!(obj.get("brand").is_none());
    }

    /// End-to-end test: write documents to multiple indexes via batch, then retrieve them with `getObjects` and verify contents.
    #[tokio::test]
    async fn multi_index_batch_write_then_get_objects_e2e() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_stage4_app(state.clone());

        let write_resp = post_json(
            &app,
            "/1/indexes/*/batch",
            json!({
                "requests": [
                    { "action": "addObject", "indexName": "e2e_get_a", "body": { "objectID": "a1", "title": "alpha doc" } },
                    { "action": "addObject", "indexName": "e2e_get_b", "body": { "objectID": "b1", "title": "beta doc" } }
                ]
            }),
        )
        .await;
        assert_eq!(write_resp.status(), StatusCode::OK);
        let write_body = body_json(write_resp).await;
        wait_for_task_succeeded(&state, write_body["taskID"]["e2e_get_a"].as_i64().unwrap()).await;
        wait_for_task_succeeded(&state, write_body["taskID"]["e2e_get_b"].as_i64().unwrap()).await;

        let read_resp = post_json(
            &app,
            "/1/indexes/*/objects",
            json!({
                "requests": [
                    { "indexName": "e2e_get_a", "objectID": "a1" },
                    { "indexName": "e2e_get_b", "objectID": "b1" }
                ]
            }),
        )
        .await;
        assert_eq!(read_resp.status(), StatusCode::OK);
        let read_body = body_json(read_resp).await;
        assert_eq!(read_body["results"][0]["title"], "alpha doc");
        assert_eq!(read_body["results"][1]["title"], "beta doc");
    }

    /// End-to-end test: write documents via multi-index batch, wait for indexing, then search by query and verify hits.
    #[tokio::test]
    async fn get_search_after_batch_write_e2e() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        state.manager.create_tenant("e2e_search_idx").unwrap();
        save_index_settings(
            &state,
            "e2e_search_idx",
            &flapjack::index::settings::IndexSettings {
                searchable_attributes: Some(vec!["title".to_string()]),
                ..Default::default()
            },
        );
        let app = make_stage4_app(state.clone());

        let write_resp = post_json(
            &app,
            "/1/indexes/*/batch",
            json!({
                "requests": [
                    { "action": "addObject", "indexName": "e2e_search_idx", "body": { "objectID": "l1", "title": "laptop pro" } },
                    { "action": "addObject", "indexName": "e2e_search_idx", "body": { "objectID": "p1", "title": "phone max" } }
                ]
            }),
        )
        .await;
        assert_eq!(write_resp.status(), StatusCode::OK);
        let write_body = body_json(write_resp).await;
        wait_for_task_succeeded(
            &state,
            write_body["taskID"]["e2e_search_idx"].as_i64().unwrap(),
        )
        .await;

        let search_resp = get_request(
            &app,
            "/1/indexes/e2e_search_idx?query=laptop&hitsPerPage=10",
        )
        .await;
        assert_eq!(search_resp.status(), StatusCode::OK);
        let search_body = body_json(search_resp).await;
        assert!(search_body["nbHits"].as_u64().unwrap() >= 1);
        let hits = search_body["hits"].as_array().unwrap();
        assert!(hits.iter().any(|h| h["title"]
            .as_str()
            .map(|t| t.contains("laptop"))
            .unwrap_or(false)));
    }
}

// ── Content-hash auto-IDs and idempotency-key dedup (PL-8 Stage 2) ──
//
// These tests pin the write-loss-recovery contract: identical request bodies
// must produce identical auto-IDs (so client retries upsert instead of
// duplicating) and an explicit `X-Flapjack-Idempotency-Key` header must dedup
// the entire response on retry.
mod content_hash_and_idempotency {
    use super::*;
    use crate::auth::AuthenticatedAppId;
    use crate::router::app_id_layer;
    use crate::test_helpers::body_json;
    use rusqlite::Connection;
    use serde_json::{json, Value};
    use tempfile::tempdir;

    fn make_app(state: Arc<AppState>) -> Router {
        app_id_layer(
            Router::new()
                .route("/1/indexes/:indexName", post(super::add_record_auto_id))
                .route("/1/indexes/:indexName/batch", post(super::add_documents))
                .with_state(state),
        )
    }

    async fn post_auto_id(
        app: &Router,
        index: &str,
        body: Value,
        idem_key: Option<&str>,
        app_id_override: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/1/indexes/{}", index))
            .header("content-type", "application/json");
        if let Some(k) = idem_key {
            builder = builder.header("x-flapjack-idempotency-key", k);
        }
        let mut request = builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        if let Some(app_id) = app_id_override {
            request
                .extensions_mut()
                .insert(AuthenticatedAppId(app_id.to_string()));
        }
        let resp = app.clone().oneshot(request).await.unwrap();
        let status = resp.status();
        let json = body_json(resp).await;
        (status, json)
    }

    async fn post_batch(
        app: &Router,
        index: &str,
        body: Value,
        idem_key: Option<&str>,
        app_id_override: Option<&str>,
    ) -> (StatusCode, Value) {
        let (status, raw_bytes) = post_batch_raw(app, index, body, idem_key, app_id_override).await;
        let json: Value =
            serde_json::from_slice(&raw_bytes).expect("batch response should be json");
        (status, json)
    }

    async fn post_batch_raw(
        app: &Router,
        index: &str,
        body: Value,
        idem_key: Option<&str>,
        app_id_override: Option<&str>,
    ) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/1/indexes/{}/batch", index))
            .header("content-type", "application/json");
        if let Some(key) = idem_key {
            builder = builder.header("x-flapjack-idempotency-key", key);
        }
        let mut request = builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        if let Some(app_id) = app_id_override {
            request
                .extensions_mut()
                .insert(AuthenticatedAppId(app_id.to_string()));
        }
        let resp = app.clone().oneshot(request).await.unwrap();
        let status = resp.status();
        let raw = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("batch response body should be readable")
            .to_vec();
        (status, raw)
    }

    fn install_idempotency_store_failure_trigger(state: &Arc<AppState>) {
        let cache_path = state
            .idempotency_cache
            .persistence_path()
            .expect("test requires persistent idempotency cache")
            .to_path_buf();
        let conn = Connection::open(cache_path).expect("open idempotency sqlite db");
        conn.execute(
            "CREATE TRIGGER idempotency_store_fail_before_insert
             BEFORE INSERT ON idempotency_cache
             BEGIN
                 SELECT RAISE(FAIL, 'forced idempotency store failure');
             END;",
            [],
        )
        .expect("install sqlite trigger to force post-commit idempotency store failure");
    }

    fn index_document_count(state: &Arc<AppState>, index_name: &str) -> usize {
        let index = state
            .manager
            .get_or_load(index_name)
            .expect("index should load for document-count assertion");
        index
            .reader()
            .searcher()
            .segment_readers()
            .iter()
            .map(|segment| segment.num_docs() as usize)
            .sum()
    }

    /// Same body in two separate auto-ID calls must produce the same objectID
    /// so a retried POST upserts rather than creates a duplicate document.
    #[tokio::test]
    async fn test_auto_id_same_body_same_objectid() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_app(state.clone());

        let body = json!({"title": "hello", "count": 1, "tags": ["x", "y"]});
        let (s1, r1) = post_auto_id(&app, "idem_idx", body.clone(), None, None).await;
        let (s2, r2) = post_auto_id(&app, "idem_idx", body, None, None).await;

        assert_eq!(s1, StatusCode::CREATED);
        assert_eq!(s2, StatusCode::CREATED);
        let id1 = r1["objectID"].as_str().expect("objectID present");
        let id2 = r2["objectID"].as_str().expect("objectID present");
        assert_eq!(
            id1, id2,
            "identical bodies must yield identical content-hash objectIDs",
        );
        // The two taskIDs may differ (each request enqueues a write), but the
        // shared object ID is what makes the retry safe — same key into the
        // index ⇒ upsert at storage layer, not a duplicate row.
    }

    /// Distinct bodies must produce distinct objectIDs.
    #[tokio::test]
    async fn test_auto_id_different_body_different_objectid() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_app(state);

        let (_, r1) = post_auto_id(&app, "diff_idx", json!({"x": 1}), None, None).await;
        let (_, r2) = post_auto_id(&app, "diff_idx", json!({"x": 2}), None, None).await;

        assert_ne!(r1["objectID"], r2["objectID"]);
    }

    /// Explicit objectID supplied via the batch addObject path must be preserved
    /// (no content-hash override). Auto-ID is only the fallback for missing IDs.
    #[tokio::test]
    async fn test_explicit_objectid_not_overridden() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_app(state);

        let body = json!({
            "requests": [
                { "action": "addObject", "body": { "objectID": "explicit-keepme", "title": "kept" } }
            ]
        });
        let (status, response) = post_batch(&app, "explicit_idx", body, None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["objectIDs"][0], "explicit-keepme");
    }

    /// Batch addObject without an explicit objectID must use the same content-hash
    /// helper as the single-record auto-ID path so the two paths converge.
    #[tokio::test]
    async fn test_batch_auto_id_same_body_same_objectid() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_app(state);

        let doc = json!({"name": "alpha", "score": 7});
        let batch_body = json!({"requests": [{"action": "addObject", "body": doc.clone()}]});

        let (_, b1) = post_batch(&app, "batch_idx", batch_body.clone(), None, None).await;
        let (_, b2) = post_batch(&app, "batch_idx", batch_body, None, None).await;

        let id_b1 = b1["objectIDs"][0].as_str().unwrap();
        let id_b2 = b2["objectIDs"][0].as_str().unwrap();
        assert_eq!(id_b1, id_b2, "batch addObject must produce stable IDs");

        // Same body via the single-record auto-ID path must converge on the same ID.
        let (_, r_auto) = post_auto_id(&app, "batch_idx", doc, None, None).await;
        assert_eq!(
            id_b1,
            r_auto["objectID"].as_str().unwrap(),
            "batch and single-record auto-ID paths must share the same hash helper",
        );
    }

    /// Two requests with the same X-Flapjack-Idempotency-Key must return the
    /// cached response from the first call, even when the body differs.
    #[tokio::test]
    async fn test_idempotency_key_dedup() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_app(state);

        let key = "client-retry-abc-123";
        let (s1, r1) = post_auto_id(&app, "key_idx", json!({"v": 1}), Some(key), None).await;
        let (s2, r2) = post_auto_id(&app, "key_idx", json!({"v": 2}), Some(key), None).await;

        assert_eq!(s1, StatusCode::CREATED);
        assert_eq!(s2, StatusCode::CREATED);
        assert_eq!(r1["objectID"], r2["objectID"]);
        assert_eq!(r1["taskID"], r2["taskID"]);
        assert_eq!(r1["createdAt"], r2["createdAt"]);
    }

    /// Without an idempotency key, distinct bodies must remain independent —
    /// the dedup cache must not conflate unrelated requests.
    #[tokio::test]
    async fn test_no_idempotency_key_independent() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_app(state);

        let (_, r1) = post_auto_id(&app, "noidem_idx", json!({"v": "a"}), None, None).await;
        let (_, r2) = post_auto_id(&app, "noidem_idx", json!({"v": "b"}), None, None).await;
        assert_ne!(r1["objectID"], r2["objectID"]);
    }

    /// Same idempotency key in the same index but different applications must not replay across apps.
    #[tokio::test]
    async fn test_idempotency_key_isolated_across_applications() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_app(state.clone());

        let key = "same-key-different-apps";
        let (s1, r1) = post_auto_id(
            &app,
            "app_scope_idx",
            json!({"title": "doc from app a"}),
            Some(key),
            Some("app-a"),
        )
        .await;
        let (s2, r2) = post_auto_id(
            &app,
            "app_scope_idx",
            json!({"title": "doc from app b"}),
            Some(key),
            Some("app-b"),
        )
        .await;

        assert_eq!(s1, StatusCode::CREATED);
        assert_eq!(s2, StatusCode::CREATED);
        assert_ne!(r1["taskID"], r2["taskID"]);

        let id1 = r1["objectID"].as_str().expect("first object id");
        let id2 = r2["objectID"].as_str().expect("second object id");
        assert!(state
            .manager
            .get_document("app_scope_idx", id1)
            .unwrap()
            .is_some());
        assert!(state
            .manager
            .get_document("app_scope_idx", id2)
            .unwrap()
            .is_some());
        assert_ne!(id1, id2);
        assert_eq!(
            index_document_count(&state, "app_scope_idx"),
            2,
            "same idempotency key across different applications must produce exactly two persisted documents in the shared index"
        );
    }

    /// Same app and key but different indexes must not cross-replay.
    #[tokio::test]
    async fn test_idempotency_key_isolated_across_indexes() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_app(state.clone());

        let key = "same-key-different-indexes";
        let (s1, r1) = post_auto_id(
            &app,
            "scope_idx_a",
            json!({"title": "doc for index a"}),
            Some(key),
            Some("shared-app"),
        )
        .await;
        let (s2, r2) = post_auto_id(
            &app,
            "scope_idx_b",
            json!({"title": "doc for index b"}),
            Some(key),
            Some("shared-app"),
        )
        .await;

        assert_eq!(s1, StatusCode::CREATED);
        assert_eq!(s2, StatusCode::CREATED);
        assert_ne!(r1["taskID"], r2["taskID"]);
        assert!(state
            .manager
            .get_document("scope_idx_a", r1["objectID"].as_str().unwrap())
            .unwrap()
            .is_some());
        assert!(state
            .manager
            .get_document("scope_idx_b", r2["objectID"].as_str().unwrap())
            .unwrap()
            .is_some());
    }

    /// Multi-index envelope dedup must replay the exact first envelope response.
    #[tokio::test]
    async fn test_multi_index_batch_replay_returns_first_envelope_response() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_app(state);

        let request_body = json!({
            "requests": [
                { "action": "addObject", "indexName": "multi_a", "body": { "objectID": "a1", "title": "alpha" } },
                { "action": "addObject", "indexName": "multi_b", "body": { "objectID": "b1", "title": "beta" } }
            ]
        });
        let key = "multi-index-envelope-key";
        let (s1, raw1) =
            post_batch_raw(&app, "*", request_body.clone(), Some(key), Some("app-a")).await;
        let (s2, raw2) = post_batch_raw(&app, "*", request_body, Some(key), Some("app-a")).await;

        assert_eq!(s1, StatusCode::OK);
        assert_eq!(s2, StatusCode::OK);
        assert_eq!(
            raw1, raw2,
            "multi-index envelope replay must return byte-for-byte identical cached response body"
        );
    }

    /// Open-mode requests without auth headers still flow through the shared app_id_layer fallback.
    #[tokio::test]
    async fn test_open_mode_default_application_id_replays_consistently() {
        let tmp = TempDir::new().unwrap();
        let state = make_write_guard_state(&tmp);
        let app = make_app(state);

        let key = "open-mode-default-app";
        let (s1, r1) = post_auto_id(&app, "open_mode_idx", json!({"v": 1}), Some(key), None).await;
        let (s2, r2) = post_auto_id(&app, "open_mode_idx", json!({"v": 2}), Some(key), None).await;

        assert_eq!(s1, StatusCode::CREATED);
        assert_eq!(s2, StatusCode::CREATED);
        assert_eq!(r1, r2);
    }

    #[tokio::test]
    async fn test_auto_id_post_commit_idempotency_store_failure_preserves_created_response() {
        let env_guard = crate::test_helpers::with_env_var("FLAPJACK_IDEMPOTENCY_PERSISTENT", "1");
        let temp = tempdir().unwrap();
        let state = make_write_guard_state(&temp);
        install_idempotency_store_failure_trigger(&state);
        let app = make_app(state);

        let (status, body) = post_auto_id(
            &app,
            "fault_idx",
            json!({"title": "still committed"}),
            Some("store-fails-after-commit"),
            Some("app-fault"),
        )
        .await;

        drop(env_guard);
        assert_eq!(status, StatusCode::CREATED);
        assert!(body.get("taskID").and_then(|v| v.as_i64()).is_some());
        assert!(body.get("objectID").and_then(|v| v.as_str()).is_some());
    }

    #[tokio::test]
    async fn test_batch_post_commit_idempotency_store_failure_preserves_success_response() {
        let env_guard = crate::test_helpers::with_env_var("FLAPJACK_IDEMPOTENCY_PERSISTENT", "1");
        let temp = tempdir().unwrap();
        let state = make_write_guard_state(&temp);
        install_idempotency_store_failure_trigger(&state);
        let app = make_app(state);

        let (status, body) = post_batch(
            &app,
            "fault_batch_idx",
            json!({
                "requests": [
                    { "action": "addObject", "body": { "objectID": "k1", "title": "kept" } }
                ]
            }),
            Some("batch-store-fails-after-commit"),
            Some("app-fault"),
        )
        .await;

        drop(env_guard);
        assert_eq!(status, StatusCode::OK);
        assert!(body.get("taskID").and_then(|v| v.as_i64()).is_some());
        assert_eq!(body["objectIDs"], json!(["k1"]));
    }
}
