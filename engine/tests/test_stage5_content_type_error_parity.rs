use axum::{
    body::Body,
    extract::DefaultBodyLimit,
    http::{Method, StatusCode},
    middleware,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

mod common;
use common::body_json;

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Body,
) -> axum::http::Response<Body> {
    common::send_oneshot(app, method, uri, headers, body).await
}

async fn response_bytes(resp: axum::http::Response<Body>) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), 10_000_000)
        .await
        .unwrap()
        .to_vec()
}

fn task_id(body: &Value) -> i64 {
    common::extract_task_id(body)
}

async fn seed_single_doc(app: &Router, index_name: &str) {
    let batch = send(
        app,
        Method::POST,
        &format!("/1/indexes/{index_name}/batch"),
        &[("content-type", "application/json")],
        Body::from(
            json!({
                "requests": [
                    {
                        "action": "addObject",
                        "body": {
                            "objectID": "doc-1",
                            "name": "alpha"
                        }
                    }
                ]
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(batch.status(), StatusCode::OK);
    let batch_body = body_json(batch).await;
    common::wait_for_task_local(app, task_id(&batch_body)).await;
}

async fn assert_json_error_shape(resp: axum::http::Response<Body>, status: StatusCode) {
    assert_eq!(resp.status(), status);
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("application/json"),
        "expected JSON error content-type, got: {content_type}"
    );

    let body = body_json(resp).await;
    assert_eq!(body["status"], json!(status.as_u16()));
    assert!(
        body["message"].as_str().is_some(),
        "expected message string in error body: {body}"
    );
}

async fn exercise_json_endpoints_with_content_type(
    app: &Router,
    index_name: &str,
    content_type: Option<&str>,
) {
    let mut settings_headers = vec![];
    if let Some(ct) = content_type {
        settings_headers.push(("content-type", ct));
    }

    let settings = send(
        app,
        Method::PUT,
        &format!("/1/indexes/{index_name}/settings"),
        &settings_headers,
        Body::from(json!({ "searchableAttributes": ["name"] }).to_string()),
    )
    .await;
    assert_eq!(settings.status(), StatusCode::OK);
    let settings_body = body_json(settings).await;
    common::wait_for_task_local(app, task_id(&settings_body)).await;

    let mut batch_headers = vec![];
    if let Some(ct) = content_type {
        batch_headers.push(("content-type", ct));
    }

    let batch = send(
        app,
        Method::POST,
        &format!("/1/indexes/{index_name}/batch"),
        &batch_headers,
        Body::from(
            json!({
                "requests": [
                    {
                        "action": "addObject",
                        "body": {
                            "objectID": "doc-1",
                            "name": "alpha"
                        }
                    }
                ]
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(batch.status(), StatusCode::OK);
    let batch_body = body_json(batch).await;
    common::wait_for_task_local(app, task_id(&batch_body)).await;

    let mut search_headers = vec![];
    if let Some(ct) = content_type {
        search_headers.push(("content-type", ct));
    }

    let search = send(
        app,
        Method::POST,
        &format!("/1/indexes/{index_name}/query"),
        &search_headers,
        Body::from(json!({ "query": "alpha" }).to_string()),
    )
    .await;
    assert_eq!(search.status(), StatusCode::OK);
    let search_body = body_json(search).await;
    assert_eq!(search_body["nbHits"], json!(1));
    assert_eq!(search_body["hits"][0]["objectID"], json!("doc-1"));
}

#[tokio::test]
async fn json_post_put_endpoints_accept_text_plain_content_type() {
    let (app, _tmp) = common::build_test_app_for_local_requests(None);
    exercise_json_endpoints_with_content_type(&app, "ct-text-plain", Some("text/plain")).await;
}

#[tokio::test]
async fn json_post_put_endpoints_accept_form_urlencoded_content_type() {
    let (app, _tmp) = common::build_test_app_for_local_requests(None);
    exercise_json_endpoints_with_content_type(
        &app,
        "ct-form-urlencoded",
        Some("application/x-www-form-urlencoded"),
    )
    .await;
}

#[tokio::test]
async fn json_post_put_endpoints_accept_missing_charset_and_bare_json_content_types() {
    let (app, _tmp) = common::build_test_app_for_local_requests(None);

    exercise_json_endpoints_with_content_type(&app, "ct-missing", None).await;
    exercise_json_endpoints_with_content_type(
        &app,
        "ct-json-charset",
        Some("application/json; charset=utf-8"),
    )
    .await;
    exercise_json_endpoints_with_content_type(&app, "ct-json-bare", Some("application/json")).await;
}

#[tokio::test]
async fn import_snapshot_with_application_gzip_still_works_under_content_type_normalization() {
    let (app, _tmp) = common::build_test_app_for_local_requests(None);
    let index_name = "snapshot-source";

    seed_single_doc(&app, index_name).await;

    let export_resp = send(
        &app,
        Method::GET,
        &format!("/1/indexes/{index_name}/export"),
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(export_resp.status(), StatusCode::OK);
    let export_ct = export_resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        export_ct.contains("application/gzip"),
        "expected snapshot export to return gzip content type, got: {export_ct}"
    );
    let snapshot_bytes = response_bytes(export_resp).await;
    assert!(
        !snapshot_bytes.is_empty(),
        "snapshot export should not be empty"
    );

    let import_resp = send(
        &app,
        Method::POST,
        &format!("/1/indexes/{index_name}/import"),
        &[("content-type", "application/gzip")],
        Body::from(snapshot_bytes),
    )
    .await;
    assert_eq!(import_resp.status(), StatusCode::OK);
    let import_body_bytes = response_bytes(import_resp).await;
    let import_body: Value = serde_json::from_slice(&import_body_bytes).unwrap();
    assert_eq!(import_body, json!({"status": "imported"}));

    let search = send(
        &app,
        Method::POST,
        &format!("/1/indexes/{index_name}/query"),
        &[("content-type", "application/json")],
        Body::from(json!({ "query": "alpha" }).to_string()),
    )
    .await;
    assert_eq!(search.status(), StatusCode::OK);
    let search_body = body_json(search).await;
    assert_eq!(search_body["nbHits"], json!(1));
    assert_eq!(search_body["hits"][0]["objectID"], json!("doc-1"));
}

#[tokio::test]
async fn framework_rejections_and_missing_routes_are_wrapped_in_json_error_shape() {
    let (app, _tmp) = common::build_test_app_for_local_requests(None);

    let missing_route = send(
        &app,
        Method::GET,
        "/does-not-exist-stage5c",
        &[],
        Body::empty(),
    )
    .await;
    assert_json_error_shape(missing_route, StatusCode::NOT_FOUND).await;

    let limited_app = Router::new()
        .route(
            "/limited",
            post(|Json(_payload): Json<Value>| async { Json(json!({ "ok": true })) }),
        )
        .layer(DefaultBodyLimit::max(8))
        .layer(middleware::from_fn(
            flapjack_http::middleware::normalize_content_type,
        ))
        .layer(middleware::from_fn(
            flapjack_http::middleware::ensure_json_errors,
        ));

    let too_large = send(
        &limited_app,
        Method::POST,
        "/limited",
        &[("content-type", "application/json")],
        Body::from(json!({"a": "0123456789"}).to_string()),
    )
    .await;
    assert_json_error_shape(too_large, StatusCode::PAYLOAD_TOO_LARGE).await;
}

#[tokio::test]
async fn malformed_json_on_search_batch_and_settings_returns_400_json_error_shape() {
    let (app, _tmp) = common::build_test_app_for_local_requests(None);

    for (method, path) in [
        (Method::POST, "/1/indexes/malformed/query"),
        (Method::POST, "/1/indexes/malformed/batch"),
        (Method::PUT, "/1/indexes/malformed/settings"),
    ] {
        let resp = send(
            &app,
            method,
            path,
            &[("content-type", "application/json")],
            Body::from("{\"bad\": "),
        )
        .await;
        assert_json_error_shape(resp, StatusCode::BAD_REQUEST).await;
    }
}
