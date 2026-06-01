use super::http::{extract_task_id, parse_response_json, send_json, wait_for_task_local_with_key};
use std::collections::HashSet;

/// Assert canonical Algolia-style error payload contract:
/// - HTTP status matches expected
/// - Content-Type is JSON
/// - Body object has exactly {"message", "status"}
pub fn assert_error_contract(
    body: &serde_json::Value,
    content_type: &str,
    status: axum::http::StatusCode,
    expected_status: u16,
) {
    let expected_status = axum::http::StatusCode::from_u16(expected_status)
        .unwrap_or_else(|_| panic!("invalid status code: {expected_status}"));
    assert_eq!(status, expected_status);

    assert!(
        content_type.contains("application/json"),
        "expected application/json Content-Type, got: {content_type}"
    );

    let object = body
        .as_object()
        .unwrap_or_else(|| panic!("error response should be a JSON object, got {body}"));
    assert_eq!(
        object.len(),
        2,
        "error response should have exactly 2 keys, got: {body}"
    );
    assert!(
        object.contains_key("message"),
        "error response should include message: {body}"
    );
    assert!(
        object["message"].is_string(),
        "message should be a string: {body}"
    );
    assert!(
        object.contains_key("status"),
        "error response should include status: {body}"
    );
    let status_value = object
        .get("status")
        .and_then(|status| status.as_u64())
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or_else(|| panic!("status should be integer status code: {body}"));
    assert_eq!(
        status_value,
        expected_status.as_u16(),
        "status field should match HTTP status"
    );
}

/// Parse an in-process axum response and assert canonical error contract.
pub async fn assert_error_contract_from_oneshot(
    resp: axum::http::Response<axum::body::Body>,
    expected_status: u16,
) -> serde_json::Value {
    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = parse_response_json(resp).await;
    assert_error_contract(&body, &content_type, status, expected_status);
    body
}

/// Parse a reqwest response and assert canonical error contract.
pub async fn assert_error_contract_from_reqwest(
    resp: reqwest::Response,
    expected_status: u16,
) -> serde_json::Value {
    let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or_else(|_| panic!("invalid reqwest status: {}", resp.status().as_u16()));
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = resp
        .json::<serde_json::Value>()
        .await
        .unwrap_or_else(|error| panic!("response body is not JSON: {error}"));
    assert_error_contract(&body, &content_type, status, expected_status);
    body
}

/// Browse all cursor pages and return visited objectIDs + last response body.
/// Validates per-page response shape during traversal.
pub async fn browse_all_cursor_pages(
    app: &axum::Router,
    index_name: &str,
    admin_key: &str,
    hits_per_page: usize,
) -> (HashSet<String>, serde_json::Value, usize) {
    let mut visited = HashSet::new();
    let mut page_count = 0;
    let mut cursor: Option<String> = None;
    let mut last_body = serde_json::json!({});

    loop {
        let payload = match &cursor {
            Some(c) => serde_json::json!({"cursor": c}),
            None => serde_json::json!({"hitsPerPage": hits_per_page}),
        };

        let (status, body) = send_json(
            app,
            axum::http::Method::POST,
            &format!("/1/indexes/{index_name}/browse"),
            admin_key,
            Some(payload),
        )
        .await;

        assert_eq!(
            status,
            axum::http::StatusCode::OK,
            "browse failed on page {page_count}"
        );
        validate_browse_response_shape(&body, hits_per_page);

        let hits = body["hits"].as_array().expect("hits should be an array");
        assert!(
            hits.len() <= hits_per_page,
            "page {page_count} has {} hits, expected at most {}",
            hits.len(),
            hits_per_page
        );

        for hit in hits {
            let oid = hit["objectID"]
                .as_str()
                .expect("every hit must include objectID as string")
                .to_string();
            assert!(
                !visited.contains(&oid),
                "duplicate objectID {oid} on page {page_count}"
            );
            visited.insert(oid);
        }

        page_count += 1;
        last_body = body.clone();
        cursor = body["cursor"].as_str().map(|s| s.to_string());
        if cursor.is_none() {
            break;
        }
    }

    (visited, last_body, page_count)
}

/// Validate per-page browse response shape.
pub fn validate_browse_response_shape(body: &serde_json::Value, expected_hits_per_page: usize) {
    assert!(body["hits"].is_array(), "hits must be an array");
    assert!(
        body["nbHits"].is_number(),
        "nbHits must be numeric, got {:?}",
        body["nbHits"]
    );
    assert!(
        body["hitsPerPage"].is_number(),
        "hitsPerPage must be numeric, got {:?}",
        body["hitsPerPage"]
    );
    assert!(
        body["page"].is_number(),
        "page must be numeric, got {:?}",
        body["page"]
    );
    assert!(
        body["nbPages"].is_number(),
        "nbPages must be numeric, got {:?}",
        body["nbPages"]
    );

    let actual_hits_per_page = body["hitsPerPage"].as_u64().unwrap_or(0) as usize;
    assert_eq!(
        actual_hits_per_page, expected_hits_per_page,
        "hitsPerPage mismatch: expected {}, got {}",
        expected_hits_per_page, actual_hits_per_page
    );

    if let Some(hits) = body["hits"].as_array() {
        for (i, hit) in hits.iter().enumerate() {
            assert!(hit.get("objectID").is_some(), "hit {} missing objectID", i);
        }
    }
}

/// Assert exactly-once browse invariants.
pub fn assert_browse_exactly_once_invariants(
    visited: &HashSet<String>,
    last_body: &serde_json::Value,
    page_count: usize,
    expected_doc_count: usize,
    hits_per_page: usize,
) {
    assert_eq!(
        visited.len(),
        expected_doc_count,
        "expected exactly {} unique docs visited, got {}",
        expected_doc_count,
        visited.len()
    );

    let cursor_value = last_body
        .get("cursor")
        .expect("terminal response must include cursor key");
    assert!(
        cursor_value.is_null(),
        "terminal response must set cursor to explicit null, got {:?}",
        cursor_value
    );

    let expected_pages = expected_doc_count.div_ceil(hits_per_page);
    assert_eq!(
        page_count, expected_pages,
        "expected {} pages, got {}",
        expected_pages, page_count
    );
}

// ── Contract-testing assertion helpers ─────────────────────────────────────

/// Assert that `s` is a valid ISO-8601 / RFC-3339 timestamp string.
/// Panics with a descriptive message if validation fails.
pub fn assert_iso8601(s: &str, field: &str) {
    // RFC 3339 requires at minimum: YYYY-MM-DDTHH:MM:SS followed by timezone (Z or ±HH:MM).
    // We validate by parsing with chrono which implements the superset of ISO-8601 we use.
    let ok = chrono::DateTime::parse_from_rfc3339(s).is_ok()
        || chrono::DateTime::parse_from_str(s, "%+").is_ok();
    assert!(
        ok,
        "Expected ISO-8601 timestamp for field '{}', got: {:?}",
        field, s
    );
}

/// Assert that a JSON field value is a valid ISO-8601 / RFC-3339 timestamp string.
pub fn assert_iso8601_value(value: &serde_json::Value, field: &str) {
    let s = value.as_str().unwrap_or_else(|| {
        panic!(
            "Expected ISO-8601 timestamp string for field '{}', got: {}",
            field, value
        )
    });
    assert_iso8601(s, field);
}

/// Assert that a JSON field value is an integer (signed or unsigned).
pub fn assert_integer_value(value: &serde_json::Value, field: &str) {
    assert!(
        value.is_i64() || value.is_u64(),
        "Expected integer for field '{}', got: {}",
        field,
        value
    );
}

/// Assert that a JSON response body has the write-task envelope fields required
/// by the Algolia API contract.  `timestamp_field` is the key that should hold
/// a RFC-3339 timestamp (`createdAt`, `updatedAt`, or `deletedAt`).
///
/// Required shape: `{ "taskID": <integer>, "<timestamp_field>": "<iso8601>" }`
/// Also rejects snake_case `task_id` leakage.
pub fn assert_write_task_envelope(body: &serde_json::Value, timestamp_field: &str) {
    let task_id = body
        .get("taskID")
        .unwrap_or_else(|| panic!("write-task envelope missing 'taskID' field: {}", body));
    assert!(
        task_id.is_i64() || task_id.is_u64(),
        "write-task 'taskID' must be an integer, got: {}",
        task_id
    );

    // Must NOT expose snake_case variant
    assert!(
        body.get("task_id").is_none(),
        "write-task envelope must not have snake_case 'task_id': {}",
        body
    );

    let ts = body
        .get(timestamp_field)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            panic!(
                "write-task envelope missing '{}' field: {}",
                timestamp_field, body
            )
        });
    assert_iso8601(ts, timestamp_field);
}

/// Assert that a JSON response body matches the Algolia error-envelope shape:
/// `{ "message": <string>, "status": <integer matching expected_status> }`.
pub fn assert_error_envelope(body: &serde_json::Value, expected_status: u16) {
    let message = body
        .get("message")
        .unwrap_or_else(|| panic!("error envelope missing 'message' field: {}", body));
    assert!(
        message.is_string(),
        "error 'message' must be a string, got: {}",
        message
    );
    assert!(
        !message.as_str().unwrap_or("").is_empty(),
        "error 'message' must not be empty: {}",
        body
    );

    let status = body
        .get("status")
        .unwrap_or_else(|| panic!("error envelope missing 'status' field: {}", body));
    assert_eq!(
        status.as_u64(),
        Some(expected_status as u64),
        "error 'status' expected {}, got: {}",
        expected_status,
        status
    );
}

/// Assert that a task-status response body has the Algolia-exact published shape:
/// `{ "status": "published", "pendingTask": false }`.
pub(crate) fn published_task_shape_error(body: &serde_json::Value) -> Option<String> {
    let object = body.as_object();
    let Some(object) = object else {
        return Some(format!(
            "task status response must be a JSON object: {}",
            body
        ));
    };
    if object.len() != 2 {
        return Some(format!(
            "task status response must contain exactly 'status' and 'pendingTask': {}",
            body
        ));
    }
    if body["status"].as_str() != Some("published") {
        return Some(format!("task status must be 'published': {}", body));
    }
    if body["pendingTask"].as_bool() != Some(false) {
        return Some(format!("task pendingTask must be false: {}", body));
    }
    if body.get("pending_task").is_some() {
        return Some(format!("must not have snake_case 'pending_task': {}", body));
    }
    None
}

pub fn assert_published_task_shape(body: &serde_json::Value) {
    if let Some(message) = published_task_shape_error(body) {
        panic!("{message}");
    }
}

/// Poll the index-level task-status route (local app variant) until the task is
/// complete, then assert the response body matches the published shape.
///
/// Uses `GET /1/indexes/{index}/task/{task_id}` (index-scoped route).
pub async fn wait_for_task_published(app: &axum::Router, index: &str, task_id: i64) {
    wait_for_task_published_with_key(app, index, task_id, "test-admin-key-parity").await;
}

/// Same as [`wait_for_task_published`], but allows specifying the API key used
/// to poll the global and index-scoped task endpoints.
pub async fn wait_for_task_published_with_key(
    app: &axum::Router,
    index: &str,
    task_id: i64,
    api_key: &str,
) {
    // First poll until completion via the global task route.
    wait_for_task_local_with_key(app, task_id, api_key).await;

    // Then assert exact shape on the index-scoped route.
    let (status, body) = send_json(
        app,
        axum::http::Method::GET,
        &format!("/1/indexes/{}/task/{}", index, task_id),
        api_key,
        None,
    )
    .await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "index-scoped task route returned non-200: {}",
        body
    );
    assert_published_task_shape(&body);
}

/// PUT /1/indexes/{index}/settings with optional forwardToReplicas query param.
/// Asserts OK status, validates write-task envelope, waits for task completion.
pub async fn put_settings_and_wait(
    app: &axum::Router,
    index: &str,
    key: &str,
    settings: serde_json::Value,
    forward_to_replicas: bool,
) -> serde_json::Value {
    let uri = if forward_to_replicas {
        format!("/1/indexes/{index}/settings?forwardToReplicas=true")
    } else {
        format!("/1/indexes/{index}/settings")
    };

    let (status, body) = send_json(app, axum::http::Method::PUT, &uri, key, Some(settings)).await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "settings update failed: {body}"
    );
    assert_write_task_envelope(&body, "updatedAt");
    wait_for_task_local_with_key(app, extract_task_id(&body), key).await;
    body
}
