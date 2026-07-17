use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig};
use flapjack::index::manager::publication::{
    ContentDigest, PublicationGenerationEvidence, PublicationJournal, PublicationPaths,
    PublicationTarget, PublicationTransactionId,
};
use flapjack::{Document, FieldValue, IndexManager};
use std::collections::HashMap;
use tempfile::TempDir;
use tower::ServiceExt;

use crate::auth::KeyStore;
use crate::middleware::REQUEST_ID_HEADER_NAME;
use crate::test_helpers::{
    body_json, build_test_router, send_empty_request, send_json_request, TestStateBuilder,
};

fn build_auth_test_app() -> (TempDir, axum::Router) {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));
    (tmp, app)
}

fn build_no_auth_test_app() -> (TempDir, axum::Router) {
    let tmp = TempDir::new().unwrap();
    let app = build_test_router(&tmp, None);
    (tmp, app)
}

fn build_no_auth_router_for_state(
    tmp: &TempDir,
    state: Arc<crate::handlers::AppState>,
) -> axum::Router {
    let analytics_config = AnalyticsConfig {
        enabled: false,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 60,
        flush_size: 1000,
        retention_days: 30,
    };
    let analytics_collector = AnalyticsCollector::new(analytics_config);
    let trusted_proxy_matcher =
        Arc::new(crate::middleware::TrustedProxyMatcher::from_optional_csv(None).unwrap());

    crate::router::build_router(
        state,
        None,
        analytics_collector,
        trusted_proxy_matcher,
        crate::startup::CorsMode::LoopbackOnly,
        tmp.path(),
    )
}

fn publication_digest() -> ContentDigest {
    ContentDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap()
}

async fn seed_document(manager: &IndexManager, tenant: &str, object_id: &str, version: &str) {
    manager.create_tenant(tenant).unwrap();
    manager
        .add_documents_sync(
            tenant,
            vec![Document {
                id: object_id.to_string(),
                fields: HashMap::from([
                    (
                        "title".to_string(),
                        FieldValue::Text(format!("{version} product")),
                    ),
                    ("version".to_string(), FieldValue::Text(version.to_string())),
                ]),
            }],
        )
        .await
        .unwrap();
}

async fn create_journaled_publication_evidence(
    base: &std::path::Path,
    target_name: &str,
    transaction_name: &str,
    staged_version: &str,
) -> PublicationPaths {
    let target = PublicationTarget::new(target_name).unwrap();
    let transaction = PublicationTransactionId::new(transaction_name).unwrap();
    let paths = PublicationPaths::new(base, &target, &transaction);

    let staging_base = paths.staging.parent().unwrap();
    let staging_manager = IndexManager::new(staging_base);
    seed_document(&staging_manager, "staging", "new_product", staged_version).await;
    std::fs::create_dir_all(&paths.backup).unwrap();
    std::fs::create_dir_all(&paths.quarantine).unwrap();

    let journal = PublicationJournal::prepare(
        transaction,
        target,
        PublicationGenerationEvidence::new(format!("generation_{transaction_name}")).unwrap(),
        publication_digest(),
        paths.clone(),
    );
    std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
    std::fs::write(&paths.journal, journal.to_json_value().to_string()).unwrap();
    std::fs::write(
        paths.quarantine.join("journal.json"),
        journal.to_json_value().to_string(),
    )
    .unwrap();

    paths
}

fn item_names(body: &serde_json::Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap().to_string())
        .collect()
}

async fn assert_reserved_search_rejected(app: &axum::Router, index_name: &str) {
    let response = send_json_request(
        app,
        Method::POST,
        &format!("/1/indexes/{index_name}/query"),
        serde_json::json!({ "query": "" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({
            "message": "Index name is reserved publication namespace",
            "status": 400
        })
    );
}

fn search_only_key_value(key_store: &KeyStore) -> String {
    key_store
        .list_all_as_dto()
        .into_iter()
        .find(|key| key.acl == ["search"])
        .expect("default key store should include a search-only key")
        .value
}

async fn assert_invalid_credentials_response(resp: axum::response::Response) {
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(resp).await,
        serde_json::json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );
}

async fn assert_method_not_allowed_response(resp: axum::response::Response) {
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(resp).await,
        serde_json::json!({
            "message": "Method not allowed with this API key",
            "status": 403
        })
    );
}

async fn post_json(
    app: &axum::Router,
    uri: &str,
    api_key: Option<&str>,
    body: serde_json::Value,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(api_key) = api_key {
        builder = builder
            .header("x-algolia-api-key", api_key)
            .header("x-algolia-application-id", "route-contract-app");
    }
    app.clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn migration_routes_preserve_admin_contract() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let search_key = search_only_key_value(&key_store);
    let app = build_test_router(&tmp, Some(key_store));

    for path in ["/1/migrate-from-algolia", "/1/algolia-list-indexes"] {
        let valid_payload = if path == "/1/migrate-from-algolia" {
            serde_json::json!({
                "appId": "APPID",
                "apiKey": "source-key",
                "sourceIndex": "products",
                "targetIndex": "products_copy"
            })
        } else {
            serde_json::json!({
                "appId": "APPID",
                "apiKey": "source-key"
            })
        };

        let missing_auth = post_json(&app, path, None, valid_payload.clone()).await;
        assert_invalid_credentials_response(missing_auth).await;

        let non_admin = post_json(&app, path, Some(&search_key), valid_payload).await;
        assert_method_not_allowed_response(non_admin).await;
    }

    let migrate_validation = post_json(
        &app,
        "/1/migrate-from-algolia",
        Some("admin-key"),
        serde_json::json!({
            "appId": "",
            "apiKey": "",
            "sourceIndex": "",
            "targetIndex": ""
        }),
    )
    .await;
    assert_eq!(migrate_validation.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(migrate_validation).await,
        serde_json::json!({
            "message": "appId, apiKey, and sourceIndex are required",
            "status": 400
        })
    );

    let list_validation = post_json(
        &app,
        "/1/algolia-list-indexes",
        Some("admin-key"),
        serde_json::json!({
            "appId": "",
            "apiKey": ""
        }),
    )
    .await;
    assert_eq!(list_validation.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(list_validation).await,
        serde_json::json!({
            "message": "appId and apiKey are required",
            "status": 400
        })
    );
}

#[tokio::test]
async fn readiness_route_is_public() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/health/ready").await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    assert_eq!(
        body_json(resp).await,
        serde_json::json!({
            "ready": true
        })
    );
}

#[tokio::test]
async fn health_route_is_public() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/health").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}
#[tokio::test]
async fn dashboard_route_is_public_and_serves_html() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("text/html"),
        "expected dashboard route to return HTML, got: {content_type}"
    );

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("<html"),
        "dashboard body should contain HTML markup"
    );
}

#[tokio::test]
async fn dashboard_trailing_slash_route_is_public_and_serves_html() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard/").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("text/html"),
        "expected dashboard trailing slash route to return HTML, got: {content_type}"
    );

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("<html"),
        "dashboard trailing slash body should contain HTML markup"
    );
}

#[tokio::test]
async fn dashboard_spa_fallback_route_is_public() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard/settings/profile").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        html.contains("<html"),
        "SPA fallback should return index HTML"
    );
}

#[tokio::test]
async fn dashboard_prefix_without_separator_is_not_public() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard-admin").await;
    assert_invalid_credentials_response(resp).await;
}

#[tokio::test]
async fn metrics_returns_403_without_auth_headers() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/metrics").await;
    assert_invalid_credentials_response(resp).await;
}
#[tokio::test]
async fn request_id_present_on_auth_403() {
    let (_tmp, app) = build_auth_test_app();

    let response = send_empty_request(&app, Method::GET, "/metrics").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let request_id = response
        .headers()
        .get(REQUEST_ID_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .expect("403 response should include x-request-id");
    let parsed = uuid::Uuid::parse_str(request_id).expect("request ID should be a UUID");
    assert_eq!(
        parsed.get_version(),
        Some(uuid::Version::Random),
        "request ID should be UUID v4"
    );
}
#[tokio::test]
async fn metrics_returns_200_with_admin_key_only() {
    let (_tmp, app) = build_auth_test_app();

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
}

#[tokio::test]
async fn metrics_rejects_query_param_admin_key() {
    let (_tmp, app) = build_auth_test_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/metrics?x-algolia-api-key=admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_invalid_credentials_response(resp).await;
}

#[tokio::test]
async fn internal_replication_routes_remain_available_when_auth_disabled() {
    let (_tmp, app) = build_no_auth_test_app();

    let internal_status = send_empty_request(&app, Method::GET, "/internal/status").await;
    assert_eq!(
        internal_status.status(),
        StatusCode::OK,
        "no-auth mode must still expose /internal/status for peer health probing"
    );

    let cluster_status = send_empty_request(&app, Method::GET, "/internal/cluster/status").await;
    assert_eq!(
        cluster_status.status(),
        StatusCode::OK,
        "no-auth mode must still expose /internal/cluster/status for HA checks"
    );

    // Route-availability probe: malformed tenant IDs must reach handler validation
    // (400) instead of falling through the router (404).
    let malformed_ops = send_empty_request(
        &app,
        Method::GET,
        "/internal/ops?tenant_id=../evil&since_seq=0",
    )
    .await;
    assert_eq!(
        malformed_ops.status(),
        StatusCode::BAD_REQUEST,
        "no-auth mode must expose /internal/ops for peer catch-up"
    );

    let malformed_replicate = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/replicate")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"tenant_id":"../evil","ops":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        malformed_replicate.status(),
        StatusCode::BAD_REQUEST,
        "no-auth mode must expose /internal/replicate for peer replication writes"
    );
}

#[tokio::test]
async fn publication_namespace_interrupted_replacement_serves_only_live_target() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    seed_document(&state.manager, "products", "old_product", "old").await;
    let _paths =
        create_journaled_publication_evidence(tmp.path(), "products", "txn_replacement", "new")
            .await;
    let app = build_no_auth_router_for_state(&tmp, state);

    let indices = send_empty_request(&app, Method::GET, "/1/indexes").await;
    assert_eq!(indices.status(), StatusCode::OK);
    let indices_body = body_json(indices).await;
    assert_eq!(item_names(&indices_body), vec!["products"]);
    assert_eq!(indices_body["nbPages"], 1);

    let tenants = send_empty_request(&app, Method::GET, "/internal/tenants").await;
    assert_eq!(tenants.status(), StatusCode::OK);
    assert_eq!(
        body_json(tenants).await["tenants"],
        serde_json::json!(["products"])
    );

    let search = send_json_request(
        &app,
        Method::POST,
        "/1/indexes/products/query",
        serde_json::json!({ "query": "", "hitsPerPage": 10 }),
    )
    .await;
    assert_eq!(search.status(), StatusCode::OK);
    let search_body = body_json(search).await;
    assert_eq!(search_body["nbHits"], 1);
    assert_eq!(search_body["hits"][0]["objectID"], "old_product");
    assert_eq!(search_body["hits"][0]["version"], "old");
    assert!(search_body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .all(|hit| hit["version"] != "new"));

    let ready = send_empty_request(&app, Method::GET, "/health/ready").await;
    assert_eq!(ready.status(), StatusCode::OK);
    assert_eq!(body_json(ready).await, serde_json::json!({ "ready": true }));

    assert_reserved_search_rejected(&app, ".publication").await;
    assert_reserved_search_rejected(&app, ".publication_quarantine").await;
}

#[tokio::test]
async fn publication_namespace_interrupted_create_is_invisible() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    let _paths =
        create_journaled_publication_evidence(tmp.path(), "products", "txn_create", "new").await;
    let app = build_no_auth_router_for_state(&tmp, state.clone());

    let indices = send_empty_request(&app, Method::GET, "/1/indexes").await;
    assert_eq!(indices.status(), StatusCode::OK);
    let indices_body = body_json(indices).await;
    assert_eq!(item_names(&indices_body), Vec::<String>::new());
    assert_eq!(indices_body["nbPages"], 1);

    let tenants = send_empty_request(&app, Method::GET, "/internal/tenants").await;
    assert_eq!(tenants.status(), StatusCode::OK);
    assert_eq!(body_json(tenants).await["tenants"], serde_json::json!([]));

    let search = send_json_request(
        &app,
        Method::POST,
        "/1/indexes/products/query",
        serde_json::json!({ "query": "" }),
    )
    .await;
    assert_eq!(search.status(), StatusCode::NOT_FOUND);
    let search_body = body_json(search).await;
    assert_eq!(
        search_body,
        serde_json::json!({
            "message": "Index 'products' does not exist",
            "status": 404
        })
    );
    assert!(search_body.get("hits").is_none());

    let ready = send_empty_request(&app, Method::GET, "/health/ready").await;
    assert_eq!(ready.status(), StatusCode::OK);
    assert_eq!(body_json(ready).await, serde_json::json!({ "ready": true }));
    assert!(
        !state
            .manager
            .loaded_tenant_ids()
            .iter()
            .any(|tenant| tenant == "products"),
        "readiness and failed search must not load a staged-only target"
    );
}
#[tokio::test]
async fn internal_storage_returns_403_with_admin_key_only_no_app_id() {
    let (_tmp, app) = build_auth_test_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/internal/storage")
                .header("x-algolia-api-key", "admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_invalid_credentials_response(resp).await;
}

/// Verify that the request latency histogram middleware records both successful and authentication-rejected requests with proper status class labels. Sends a successful POST request (200) and an auth-rejected POST request (403) to the same endpoint, then confirms both metrics appear in the Prometheus output with correct method, route, and status_class labels. Also verifies that the metrics endpoint itself remains admin-only protected.
#[tokio::test]
async fn latency_histogram_captures_success_and_auth_rejection_while_metrics_stays_admin_only() {
    let (_tmp, app) = build_auth_test_app();

    let create_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes")
                .header("x-algolia-api-key", "admin-key")
                .header("x-algolia-application-id", "latency-app")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"uid":"latency_success_index"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);

    let rejected_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"uid":"latency_forbidden_index"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected_resp.status(), StatusCode::FORBIDDEN);

    let metrics_without_auth = send_empty_request(&app, Method::GET, "/metrics").await;
    assert_eq!(metrics_without_auth.status(), StatusCode::FORBIDDEN);

    let metrics_with_admin = app
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
    assert_eq!(metrics_with_admin.status(), StatusCode::OK);

    let body = axum::body::to_bytes(metrics_with_admin.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("request_duration_seconds"),
        "expected shared latency histogram family in /metrics output"
    );
    assert!(
        text.lines().any(|line| {
            line.starts_with("request_duration_seconds_count")
                && line.contains("method=\"POST\"")
                && line.contains("route=\"/1/indexes\"")
                && line.contains("status_class=\"2xx\"")
        }),
        "expected POST 2xx request_duration_seconds_count for /1/indexes in:\n{text}"
    );
    assert!(
        text.lines().any(|line| {
            line.starts_with("request_duration_seconds_count")
                && line.contains("method=\"POST\"")
                && line.contains("route=\"/1/indexes\"")
                && line.contains("status_class=\"4xx\"")
        }),
        "expected POST 4xx request_duration_seconds_count for /1/indexes in:\n{text}"
    );
}
