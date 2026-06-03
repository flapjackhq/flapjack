use axum::{
    http::{header, Method, StatusCode, Version},
    Router,
};
use serde_json::{json, Value};
use tokio::task::JoinSet;

mod common;

const ADMIN_KEY: &str = "test-admin-key-conn-hard";
const APP_ID: &str = "conn-hard-app";
const CONNECT_RETRY_ATTEMPTS: usize = 4;
const CONNECT_RETRY_BASE_DELAY_MS: u64 = 25;

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
) -> Result<reqwest::Response, reqwest::Error> {
    client
        .post(format!("http://{}/1/indexes/{}/query", addr, index_name))
        .header("x-algolia-api-key", ADMIN_KEY)
        .header("x-algolia-application-id", APP_ID)
        .header("content-type", "application/json")
        .json(&json!({ "query": "test", "hitsPerPage": 10 }))
        .send()
        .await
}

fn is_addr_not_available_message(message: &str) -> bool {
    message.contains("AddrNotAvailable") || message.contains("Can't assign requested address")
}

async fn send_search_http_with_retry(
    client: &reqwest::Client,
    addr: &str,
    index_name: &str,
) -> Result<(StatusCode, Value), String> {
    for attempt in 1..=CONNECT_RETRY_ATTEMPTS {
        match send_search_http(client, addr, index_name).await {
            Ok(resp) => {
                let status = resp.status();
                let body = resp.json::<Value>().await.map_err(|err| {
                    format!("response JSON decode failed on attempt {attempt}: {err}")
                })?;
                return Ok((status, body));
            }
            Err(err) => {
                let message = err.to_string();
                if attempt < CONNECT_RETRY_ATTEMPTS && is_addr_not_available_message(&message) {
                    tokio::time::sleep(tokio::time::Duration::from_millis(
                        CONNECT_RETRY_BASE_DELAY_MS * attempt as u64,
                    ))
                    .await;
                    continue;
                }
                return Err(format!(
                    "HTTP request failed on attempt {attempt}: {message}"
                ));
            }
        }
    }

    Err("HTTP request did not complete after retry loop".to_string())
}

async fn seed_documents(app: &Router, index_name: &str, count: usize) {
    let requests: Vec<Value> = (0..count)
        .map(|i| {
            json!({
                "action": "addObject",
                "body": {
                    "objectID": format!("doc-{}", i),
                    "name": format!("test item {}", i),
                    "price": (i * 10) as f64,
                    "category": if i % 2 == 0 { "even" } else { "odd" }
                }
            })
        })
        .collect();

    let (status, body) = send_json(
        app,
        Method::POST,
        &format!("/1/indexes/{index_name}/batch"),
        &[("content-type", "application/json")],
        json!({ "requests": requests }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "batch write must succeed: {body}");
    common::wait_for_task_local_with_key(app, common::extract_task_id(&body), ADMIN_KEY).await;
}

fn make_view_events(n: usize) -> Value {
    let events: Vec<Value> = (0..n)
        .map(|i| {
            json!({
                "eventType": "view",
                "eventName": format!("test_view_{}", i),
                "index": "conn-hard-events-index",
                "userToken": format!("user_token_{}", i),
                "objectIDs": [format!("obj_{}", i)]
            })
        })
        .collect();
    json!({ "events": events })
}

#[tokio::test]
async fn test_100_rapid_sequential_requests_no_429_or_connection_errors() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    seed_documents(&app, "conn-hard-seq", 10).await;

    for i in 0..100 {
        let resp = common::send_authed_response(
            &app,
            Method::POST,
            "/1/indexes/conn-hard-seq/query",
            ADMIN_KEY,
            APP_ID,
            &[],
            Some(json!({ "query": "test", "hitsPerPage": 1 })),
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
        assert!(
            body.get("nbHits").is_some(),
            "request {i} should have nbHits: {body}"
        );
    }
}

#[tokio::test]
async fn test_50_concurrent_client_connections_all_succeed() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    seed_documents(&app, "conn-hard-concurrent", 10).await;
    let addr = spawn_tcp_server(app).await;

    let client = reqwest::Client::new();
    let mut join_set = JoinSet::new();

    for i in 0..50 {
        let client = client.clone();
        let addr = addr.clone();
        join_set.spawn(async move {
            (
                i,
                send_search_http_with_retry(&client, &addr, "conn-hard-concurrent").await,
            )
        });
    }

    let mut success_count = 0;
    while let Some(result) = join_set.join_next().await {
        let (i, request_result) = result.unwrap();
        let (status, body) = request_result.unwrap_or_else(|message| {
            panic!("request {i} failed after retries: {message}");
        });
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
        success_count += 1;
    }
    assert_eq!(
        success_count, 50,
        "all 50 concurrent requests should succeed"
    );
}

#[test]
fn addr_not_available_detection_matches_known_os_signatures() {
    assert!(is_addr_not_available_message(
        "ConnectError(\"tcp connect error\", 127.0.0.1:64707, Os { code: 49, kind: AddrNotAvailable, message: \"Can't assign requested address\" })"
    ));
    assert!(!is_addr_not_available_message("connection refused"));
}

#[tokio::test]
#[ignore = "slow test (30s tcp-keepalive idle wait) - run manually with cargo test -p flapjack --test test_connection_hardening -- --ignored"]
async fn test_keepalive_reuse_after_30s_idle() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    seed_documents(&app, "conn-hard-idle", 10).await;
    let addr = spawn_tcp_server(app).await;

    let client = reqwest::Client::builder()
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .build()
        .unwrap();

    let first_resp = send_search_http(&client, &addr, "conn-hard-idle")
        .await
        .expect("first request should succeed");

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

    let second_resp = send_search_http(&client, &addr, "conn-hard-idle")
        .await
        .expect("second request should succeed");

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

#[tokio::test]
async fn test_500_document_batch_write_with_task_polling() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let doc_count = 500;
    seed_documents(&app, "conn-hard-batch-500", doc_count).await;

    let (status, search_body) = send_json(
        &app,
        Method::POST,
        "/1/indexes/conn-hard-batch-500/query",
        &[("content-type", "application/json")],
        json!({ "query": "", "hitsPerPage": doc_count }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "search should succeed: {search_body}"
    );
    assert_eq!(
        search_body["nbHits"].as_i64().unwrap_or(0),
        doc_count as i64,
        "search should return all {} documents, got: {}",
        doc_count,
        search_body["nbHits"]
    );

    let hits = search_body["hits"]
        .as_array()
        .expect("hits should be array");
    assert_eq!(hits.len(), doc_count, "should have {} hits", doc_count);
}

#[tokio::test]
async fn test_1000_event_insights_batch_accepted() {
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
async fn test_1001_event_insights_batch_rejected() {
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
