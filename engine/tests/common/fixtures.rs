use super::http::{
    extract_task_id, parse_response_json, send_json, send_oneshot, wait_for_task_local,
    wait_for_task_local_with_key,
};

/// Seed multiple documents into an index and wait for task completion.
pub async fn seed_docs(
    app: &axum::Router,
    index_name: &str,
    key: &str,
    docs: Vec<serde_json::Value>,
) {
    let requests: Vec<serde_json::Value> = docs
        .into_iter()
        .map(|doc| serde_json::json!({"action": "addObject", "body": doc}))
        .collect();

    let (status, body) = send_json(
        app,
        axum::http::Method::POST,
        &format!("/1/indexes/{index_name}/batch"),
        key,
        Some(serde_json::json!({"requests": requests})),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "seed_docs batch failed");
    wait_for_task_local_with_key(app, extract_task_id(&body), key).await;
}

/// Seed `count` lightweight documents in batches of 5,000.
///
/// Each document has shape `{"objectID": "doc_{i}", "v": i}`.
/// Batches are sent sequentially, each waiting for its task to complete.
pub async fn seed_docs_bulk(app: &axum::Router, index_name: &str, key: &str, count: usize) {
    const BATCH_SIZE: usize = 5_000;
    for start in (0..count).step_by(BATCH_SIZE) {
        let end = (start + BATCH_SIZE).min(count);
        let docs: Vec<serde_json::Value> = (start..end)
            .map(|i| serde_json::json!({"objectID": format!("doc_{i}"), "v": i}))
            .collect();
        seed_docs(app, index_name, key, docs).await;
    }
}

/// Seed a single document into an index using the in-process router.
pub async fn seed_doc_local(app: &axum::Router, index_name: &str) {
    let resp = send_oneshot(
        app,
        axum::http::Method::POST,
        &format!("/1/indexes/{index_name}/batch"),
        &[("content-type", "application/json")],
        axum::body::Body::from(
            serde_json::json!({
                "requests": [
                    {
                        "action": "addObject",
                        "body": { "objectID": "doc-1", "name": "alpha" }
                    }
                ]
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = parse_response_json(resp).await;
    wait_for_task_local(app, extract_task_id(&body)).await;
}

/// Sample current process RSS in KB via `ps`. Returns 0 on failure.
pub fn sample_rss_kb() -> u64 {
    let pid = std::process::id().to_string();
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Parse a usize from optional text, falling back to the provided default.
pub fn parse_usize_or_default(raw: Option<&str>, default: usize) -> usize {
    raw.and_then(|s| s.parse::<usize>().ok()).unwrap_or(default)
}

/// Read a usize override from env, falling back to the provided default.
#[allow(dead_code)] // Used by test_browse_scalability.rs and test_browse_scalability_slow.rs
pub fn env_usize_or_default(var: &str, default: usize) -> usize {
    parse_usize_or_default(std::env::var(var).ok().as_deref(), default)
}
