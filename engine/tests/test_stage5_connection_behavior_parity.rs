use axum::{
    http::{header, Method, StatusCode, Version},
    Router,
};
use serde_json::{json, Value};
use tokio::task::JoinSet;

mod common;

const ADMIN_KEY: &str = "test-admin-key-stage5e";
const APP_ID: &str = "stage5e-app";

async fn send_json(
    app: &Router,
    method: Method,
    uri: &str,
    extra_headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, Value) {
    common::send_authed(
        app,
        method,
        uri,
        ADMIN_KEY,
        APP_ID,
        extra_headers,
        Some(body),
    )
    .await
}

async fn send_empty(
    app: &Router,
    method: Method,
    uri: &str,
    extra_headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    common::send_authed(app, method, uri, ADMIN_KEY, APP_ID, extra_headers, None).await
}

async fn spawn_tcp_server(app: Router) -> String {
    use tokio::net::TcpListener as TokioTcpListener;

    let listener = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

async fn send_search_http(
    client: &reqwest::Client,
    addr: &str,
    index_name: &str,
) -> reqwest::Response {
    client
        .post(format!("http://{}/1/indexes/{}/query", addr, index_name))
        .header("x-algolia-api-key", ADMIN_KEY)
        .header("x-algolia-application-id", APP_ID)
        .header("content-type", "application/json")
        .json(&json!({ "query": "jacket", "hitsPerPage": 1 }))
        .send()
        .await
        .unwrap()
}

async fn seed_documents(app: &Router, index_name: &str) {
    let (status, body) = send_json(
        app,
        Method::POST,
        &format!("/1/indexes/{index_name}/batch"),
        &[("content-type", "application/json")],
        json!({
            "requests": [
                {
                    "action": "addObject",
                    "body": { "objectID": "doc-1", "name": "alpha jacket", "brand": "Acme" }
                },
                {
                    "action": "addObject",
                    "body": { "objectID": "doc-2", "name": "beta jacket", "brand": "Acme" }
                }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "batch write must succeed: {body}");
    common::wait_for_task_local_with_key(app, common::extract_task_id(&body), ADMIN_KEY).await;
}

#[tokio::test]
async fn sdk_flow_batch_write_poll_task_then_search_sees_published_records() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    seed_documents(&app, "stage5e-products-a").await;

    let (status, search_body) = send_json(
        &app,
        Method::POST,
        "/1/indexes/stage5e-products-a/query",
        &[("content-type", "application/json")],
        json!({ "query": "alpha" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(search_body["nbHits"], json!(1));
    assert_eq!(search_body["hits"][0]["objectID"], json!("doc-1"));
}

#[tokio::test]
async fn sdk_flow_search_browse_then_get_object_is_state_consistent() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    seed_documents(&app, "stage5e-products-b").await;

    let (search_status, search_body) = send_json(
        &app,
        Method::POST,
        "/1/indexes/stage5e-products-b/query",
        &[("content-type", "application/json")],
        json!({ "query": "jacket", "hitsPerPage": 10 }),
    )
    .await;
    assert_eq!(search_status, StatusCode::OK);
    assert_eq!(search_body["nbHits"], json!(2));

    let (browse_status, browse_body) = send_json(
        &app,
        Method::POST,
        "/1/indexes/stage5e-products-b/browse",
        &[("content-type", "application/json")],
        json!({ "hitsPerPage": 10 }),
    )
    .await;
    assert_eq!(browse_status, StatusCode::OK);

    let browse_hits = browse_body["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("browse hits must be array: {browse_body}"));
    assert!(
        browse_hits.len() >= 2,
        "browse should include seeded docs: {browse_body}"
    );

    let object_id = browse_hits
        .iter()
        .find_map(|hit| hit["objectID"].as_str())
        .unwrap_or_else(|| panic!("browse hit missing objectID: {browse_body}"));

    let (get_status, object_body) = send_empty(
        &app,
        Method::GET,
        &format!("/1/indexes/stage5e-products-b/{object_id}"),
        &[],
    )
    .await;
    assert_eq!(get_status, StatusCode::OK);

    assert_eq!(object_body["objectID"], json!(object_id));
    assert_eq!(object_body["brand"], json!("Acme"));
    let search_hits = search_body["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("search hits must be array: {search_body}"));
    assert!(
        search_hits
            .iter()
            .any(|hit| hit["objectID"] == object_body["objectID"]),
        "object from browse/get must also exist in search result set"
    );
}

#[tokio::test]
async fn rapid_sequential_requests_do_not_return_spurious_429_or_connection_errors() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    seed_documents(&app, "stage5e-products-c").await;

    for i in 0..50 {
        let resp = common::send_authed_response(
            &app,
            Method::POST,
            "/1/indexes/stage5e-products-c/query",
            ADMIN_KEY,
            APP_ID,
            &[],
            Some(json!({ "query": "jacket", "hitsPerPage": 1 })),
        )
        .await;
        assert_eq!(resp.version(), Version::HTTP_11);
        let connection = resp
            .headers()
            .get(header::CONNECTION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        assert!(
            connection.is_empty() || connection.eq_ignore_ascii_case("keep-alive"),
            "request {i} should keep connection alive, got Connection header: {connection}"
        );
        let status = resp.status();
        let body = common::parse_response_json(resp).await;

        assert_eq!(
            status,
            StatusCode::OK,
            "request {i} should not fail/spuriously throttle: {body}"
        );
        assert_eq!(body["nbHits"], json!(2), "request {i} body: {body}");
        assert_eq!(body["hits"].as_array().map(|h| h.len()), Some(1));
    }
}

fn make_view_events(n: usize) -> Value {
    let events: Vec<Value> = (0..n)
        .map(|i| {
            json!({
                "eventType": "view",
                "eventName": format!("test_view_{}", i),
                "index": "stage5e-events-index",
                "userToken": format!("user_token_{}", i),
                "objectIDs": [format!("obj_{}", i)]
            })
        })
        .collect();
    json!({ "events": events })
}

#[tokio::test]
async fn post_events_accepts_1000_events() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let payload = make_view_events(1000);
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/1/events",
        &[("content-type", "application/json")],
        payload,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "should accept 1000 events: {body}");
    assert_eq!(body["status"], 200);
    assert_eq!(body["message"], "OK");
}

#[tokio::test]
async fn post_events_rejects_1001_events() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let payload = make_view_events(1001);
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/1/events",
        &[("content-type", "application/json")],
        payload,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "should reject 1001 events: {body}"
    );
    assert_eq!(body["status"], 400);
}

#[tokio::test]
async fn concurrent_20_requests_all_succeed() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    seed_documents(&app, "stage5e-concurrent-test").await;
    let addr = spawn_tcp_server(app).await;

    let client = reqwest::Client::new();
    let mut join_set = JoinSet::new();

    for i in 0..20 {
        let client = client.clone();
        let addr = addr.clone();
        join_set.spawn(async move {
            let resp = send_search_http(&client, &addr, "stage5e-concurrent-test").await;
            (i, resp.status(), resp.json::<Value>().await.unwrap())
        });
    }

    while let Some(result) = join_set.join_next().await {
        let (i, status, body) = result.unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "request {i} should succeed, got {status}: {body}"
        );
        assert!(
            body.get("nbHits").is_some(),
            "request {i} should have nbHits: {body}"
        );
        assert!(
            body.get("hits").is_some(),
            "request {i} should have hits: {body}"
        );
    }
}

#[tokio::test]
#[ignore = "slow test (30s tcp-keepalive idle wait) - run manually with cargo test -p flapjack --test test_stage5_connection_behavior_parity -- --ignored"]
async fn connection_reuse_after_30s_idle_slow() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    seed_documents(&app, "stage5e-idle-test").await;
    let addr = spawn_tcp_server(app).await;

    let client = reqwest::Client::builder()
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .build()
        .unwrap();

    let first_resp = send_search_http(&client, &addr, "stage5e-idle-test").await;

    assert_eq!(first_resp.status(), StatusCode::OK);
    let first_connection = first_resp
        .headers()
        .get(header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let first_body: Value = first_resp.json().await.unwrap();
    assert!(first_body.get("nbHits").is_some());
    assert!(
        first_connection.is_empty() || !first_connection.eq_ignore_ascii_case("close"),
        "connection should stay open for first response, got: {first_connection}"
    );

    tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

    let second_resp = send_search_http(&client, &addr, "stage5e-idle-test").await;

    assert_eq!(second_resp.status(), StatusCode::OK);

    let connection = second_resp
        .headers()
        .get(header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let second_body: Value = second_resp.json().await.unwrap();
    assert!(second_body.get("nbHits").is_some());

    assert!(
        connection.is_empty() || !connection.eq_ignore_ascii_case("close"),
        "connection should not be closed after 30s idle, got: {connection}"
    );
}
