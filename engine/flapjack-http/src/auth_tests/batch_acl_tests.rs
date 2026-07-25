use super::*;
use axum::http::Method;
use serde_json::{json, Value};

const APP_ID: &str = "app-id";
const ADMIN_KEY: &str = "admin-key";
const INDEX_NAME: &str = "scopetest";

#[tokio::test]
async fn batch_rejects_search_only_and_missing_keys_without_mutation() {
    let temp_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(temp_dir.path(), ADMIN_KEY));
    let (_, search_key) = key_store.create_key(test_search_api_key("batch acl search key"));
    let app = crate::test_helpers::build_test_router(&temp_dir, Some(Arc::clone(&key_store)));

    create_empty_index(&app).await;
    assert_forbidden_json(
        post_batch(&app, Some(&search_key), batch_body()).await,
        json!({
            "message": "Method not allowed with this API key",
            "status": 403
        }),
    )
    .await;
    assert_forbidden_json(
        post_batch(&app, None, batch_body()).await,
        json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        }),
    )
    .await;

    assert_empty_index(&app).await;

    let authorized_batch = post_batch(&app, Some(ADMIN_KEY), batch_body()).await;
    assert_eq!(authorized_batch.status(), StatusCode::OK);
    let task_id = numeric_task_id(body_json(authorized_batch).await);
    wait_for_published_task(&app, task_id).await;
    assert_single_doc(&app).await;
}

async fn create_empty_index(app: &Router) {
    let response = send_json(
        app,
        Method::POST,
        "/1/indexes",
        Some(ADMIN_KEY),
        json!({
            "uid": INDEX_NAME
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

async fn post_batch(app: &Router, api_key: Option<&str>, body: Value) -> axum::response::Response {
    send_json(
        app,
        Method::POST,
        &format!("/1/indexes/{INDEX_NAME}/batch"),
        api_key,
        body,
    )
    .await
}

async fn query_index(app: &Router) -> Value {
    let response = send_json(
        app,
        Method::POST,
        &format!("/1/indexes/{INDEX_NAME}/query"),
        Some(ADMIN_KEY),
        json!({
            "query": ""
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

async fn assert_empty_index(app: &Router) {
    let body = query_index(app).await;
    assert_eq!(body["nbHits"], json!(0));
    assert_eq!(body["hits"], json!([]));
}

async fn assert_single_doc(app: &Router) {
    let body = query_index(app).await;
    assert_eq!(body["nbHits"], json!(1));
    let hits = body["hits"].as_array().expect("hits must be an array");
    assert_eq!(hits.len(), 1, "authorized batch must not create duplicates");
    assert_eq!(hits[0]["objectID"], json!("doc1"));
}

async fn assert_forbidden_json(response: axum::response::Response, expected: Value) {
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(response).await, expected);
}

async fn send_json(
    app: &Router,
    method: Method,
    uri: &str,
    api_key: Option<&str>,
    body: Value,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-algolia-application-id", APP_ID);
    if let Some(api_key) = api_key {
        request = request.header("x-algolia-api-key", api_key);
    }

    app.clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

fn batch_body() -> Value {
    json!({
        "requests": [{
            "action": "addObject",
            "body": {
                "objectID": "doc1",
                "title": "blocked"
            }
        }]
    })
}

fn numeric_task_id(body: Value) -> i64 {
    body["taskID"]
        .as_i64()
        .unwrap_or_else(|| panic!("batch response missing numeric taskID: {body}"))
}

async fn wait_for_published_task(app: &Router, task_id: i64) {
    let mut last_body = None;
    for _ in 0..200 {
        let response = send_json(
            app,
            Method::GET,
            &format!("/1/indexes/{INDEX_NAME}/task/{task_id}"),
            Some(ADMIN_KEY),
            json!({}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        if body["pendingTask"] == json!(false) && body["status"] == json!("published") {
            return;
        }
        last_body = Some(body);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    panic!("task {task_id} did not publish before timeout: {last_body:?}");
}
