use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig};
use flapjack::index::manager::publication::{
    ContentDigest, PublicationGenerationEvidence, PublicationJournal, PublicationPaths,
    PublicationTarget, PublicationTransactionId,
};
use flapjack::{Document, FieldValue, IndexManager};
use std::collections::HashMap;
use tempfile::TempDir;
use tower::ServiceExt;

use crate::auth::{ApiKey, KeyStore};
use crate::handlers::dashboard::{dashboard_test_asset_bytes, dashboard_test_index_bytes};
use crate::middleware::REQUEST_ID_HEADER_NAME;
use crate::openapi::{DOCUMENTED_INTERNAL_MEMBERSHIP_PATHS, DOCUMENTED_MEMBERSHIP_SCHEMA_NAMES};
use crate::openapi_test_helpers::{
    assert_add_peer_openapi_contract, assert_remove_peer_openapi_contract,
};
use crate::test_helpers::{
    body_json, build_test_router, send_empty_request, send_json_request, TestStateBuilder,
};

fn build_auth_test_app() -> (TempDir, axum::Router) {
    build_auth_test_app_with_dashboard_policy(false)
}

fn build_auth_test_app_with_dashboard_policy(disable_dashboard: bool) -> (TempDir, axum::Router) {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router_with_dashboard_policy(&tmp, Some(key_store), disable_dashboard);
    (tmp, app)
}

fn build_no_auth_test_app() -> (TempDir, axum::Router) {
    let tmp = TempDir::new().unwrap();
    let app = build_test_router(&tmp, None);
    (tmp, app)
}

fn build_test_router_with_dashboard_policy(
    tmp: &TempDir,
    key_store: Option<Arc<KeyStore>>,
    disable_dashboard: bool,
) -> axum::Router {
    let state = TestStateBuilder::new(tmp).with_analytics().build_shared();
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
        key_store,
        analytics_collector,
        trusted_proxy_matcher,
        tmp.path(),
        crate::router::RouterConfig {
            cors_mode: crate::startup::CorsMode::LoopbackOnly,
            disable_dashboard,
        },
    )
}

#[tokio::test]
async fn openapi_membership_contract_is_served_when_auth_is_enabled() {
    let (_tmp, app) = build_auth_test_app();
    let response = send_empty_request(&app, Method::GET, "/api-docs/openapi.json").await;

    assert_eq!(response.status(), StatusCode::OK);
    let document = body_json(response).await;
    assert_add_peer_openapi_contract(&document);
    assert_remove_peer_openapi_contract(&document);
}

#[tokio::test]
async fn openapi_membership_contract_is_hidden_when_auth_is_disabled() {
    let (_tmp, app) = build_no_auth_test_app();
    let response = send_empty_request(&app, Method::GET, "/api-docs/openapi.json").await;

    assert_eq!(response.status(), StatusCode::OK);
    let document = body_json(response).await;
    let paths = document
        .get("paths")
        .and_then(|value| value.as_object())
        .expect("served OpenAPI must have paths");

    for path in DOCUMENTED_INTERNAL_MEMBERSHIP_PATHS {
        assert!(
            !paths.contains_key(path),
            "no-auth router should not serve OpenAPI for unavailable path {path}"
        );
    }

    for schema in DOCUMENTED_MEMBERSHIP_SCHEMA_NAMES {
        assert!(
            document
                .pointer(&format!("/components/schemas/{schema}"))
                .is_none(),
            "no-auth router should not serve unused membership schema {schema}"
        );
    }
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
        tmp.path(),
        crate::router::RouterConfig {
            cors_mode: crate::startup::CorsMode::LoopbackOnly,
            disable_dashboard: false,
        },
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

fn create_test_key_with_acl(key_store: &KeyStore, acl: &str) -> String {
    let key = ApiKey {
        hash: String::new(),
        salt: String::new(),
        hmac_key: None,
        created_at: 0,
        acl: vec![acl.to_string()],
        description: format!("{acl} test key"),
        indexes: vec![],
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: vec![],
        restrict_sources: None,
        validity: 0,
    };
    key_store.create_key(key).1
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

async fn get_request(
    app: &axum::Router,
    uri: &str,
    api_key: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(api_key) = api_key {
        builder = builder
            .header("x-algolia-api-key", api_key)
            .header("x-algolia-application-id", "route-contract-app");
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn migration_routes_preserve_admin_contract() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let search_key = search_only_key_value(&key_store);
    let write_key = create_test_key_with_acl(&key_store, "addObject");
    let app = build_test_router(&tmp, Some(key_store));

    for path in [
        "/1/migrate-from-algolia",
        "/1/algolia-list-indexes",
        "/1/migrations/algolia",
    ] {
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

    let status_path = "/1/migrations/algolia/01890f8e-8b28-78e8-b542-8cfdcb2d4f24";
    let missing_auth = get_request(&app, status_path, None).await;
    assert_invalid_credentials_response(missing_auth).await;
    for api_key in [&search_key, &write_key] {
        let non_admin = get_request(&app, status_path, Some(api_key)).await;
        assert_method_not_allowed_response(non_admin).await;
    }
    let cancel_path = "/1/migrations/algolia/01890f8e-8b28-78e8-b542-8cfdcb2d4f24/cancel";
    let cancel_missing_auth = post_json(&app, cancel_path, None, serde_json::json!({})).await;
    assert_invalid_credentials_response(cancel_missing_auth).await;
    for api_key in [&search_key, &write_key] {
        let non_admin = post_json(&app, cancel_path, Some(api_key), serde_json::json!({})).await;
        assert_method_not_allowed_response(non_admin).await;
    }
    let cancel_missing_job =
        post_json(&app, cancel_path, Some("admin-key"), serde_json::json!({})).await;
    assert_eq!(cancel_missing_job.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(cancel_missing_job).await,
        serde_json::json!({
            "message": "Migration job not found",
            "status": 404,
            "code": "migration_job_not_found"
        })
    );
    let post_missing_auth = post_json(
        &app,
        "/1/migrations/algolia",
        None,
        serde_json::json!({
            "appId": "APPID",
            "apiKey": "source-key",
            "sourceIndex": "products",
            "targetIndex": "products_copy"
        }),
    )
    .await;
    assert_invalid_credentials_response(post_missing_auth).await;
    let post_write_only = post_json(
        &app,
        "/1/migrations/algolia",
        Some(&write_key),
        serde_json::json!({
            "appId": "APPID",
            "apiKey": "source-key",
            "sourceIndex": "products",
            "targetIndex": "products_copy"
        }),
    )
    .await;
    assert_method_not_allowed_response(post_write_only).await;

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

    assert_eq!(body_bytes(resp).await, dashboard_test_index_bytes());
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

    assert_eq!(body_bytes(resp).await, dashboard_test_index_bytes());
}

#[tokio::test]
async fn dashboard_spa_fallback_route_is_public() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard/settings/profile").await;
    assert_eq!(resp.status(), StatusCode::OK);

    assert_eq!(body_bytes(resp).await, dashboard_test_index_bytes());
}

#[tokio::test]
async fn dashboard_spa_fallback_serves_index_for_dotted_route_with_trailing_path() {
    let (_tmp, app) = build_auth_test_app();

    // Index names may legally contain dots and must remain SPA client routes.
    let resp = send_empty_request(&app, Method::GET, "/dashboard/indexes/my.index/settings").await;
    assert_eq!(resp.status(), StatusCode::OK);

    assert_eq!(body_bytes(resp).await, dashboard_test_index_bytes());
}

#[tokio::test]
async fn dashboard_spa_fallback_serves_index_for_dot_in_final_segment() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard/indexes/my.index").await;
    assert_eq!(resp.status(), StatusCode::OK);

    assert_eq!(body_bytes(resp).await, dashboard_test_index_bytes());
}

#[tokio::test]
async fn dashboard_non_public_embedded_artifact_falls_back_to_index() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard/stats.html").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_bytes(resp).await, dashboard_test_index_bytes());
}

#[tokio::test]
async fn dashboard_assets_prefix_cannot_traverse_to_non_public_artifact() {
    let (_tmp, app) = build_auth_test_app();

    for path in [
        "/dashboard/assets/../stats.html",
        "/dashboard/assets/..\\stats.html",
    ] {
        let resp = send_empty_request(&app, Method::GET, path).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "path: {path}");
    }
}

#[tokio::test]
async fn dashboard_missing_asset_under_assets_prefix_returns_404() {
    let (_tmp, app) = build_auth_test_app();

    // Missing content-hashed Vite assets are real 404s, not SPA fallbacks.
    let resp =
        send_empty_request(&app, Method::GET, "/dashboard/assets/index-DOESNOTEXIST.js").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dashboard_root_static_file_returns_expected_static_result() {
    let (_tmp, app) = build_auth_test_app();

    // Real dashboard builds embed public files; fallback builds may not.
    let expected_status = match dashboard_test_asset_bytes("favicon.ico") {
        Some(_) => StatusCode::OK,
        None => StatusCode::NOT_FOUND,
    };
    let resp = send_empty_request(&app, Method::GET, "/dashboard/favicon.ico").await;
    assert_eq!(resp.status(), expected_status);
}

#[tokio::test]
async fn dashboard_prefix_without_separator_is_not_public() {
    let (_tmp, app) = build_auth_test_app();

    let resp = send_empty_request(&app, Method::GET, "/dashboard-admin").await;
    assert_invalid_credentials_response(resp).await;
}

#[tokio::test]
async fn dashboard_routes_follow_lockdown_policy() {
    let (_tmp, locked_app) = build_auth_test_app_with_dashboard_policy(true);

    for path in [
        "/dashboard",
        "/dashboard/",
        "/dashboard/settings",
        "/swagger-ui",
        "/swagger-ui/",
        "/api-docs/openapi.json",
    ] {
        let resp = send_empty_request(&locked_app, Method::GET, path).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "path: {path}");
    }

    for path in ["/health", "/health/ready"] {
        let resp = send_empty_request(&locked_app, Method::GET, path).await;
        assert_eq!(resp.status(), StatusCode::OK, "path: {path}");
    }

    let acme = send_empty_request(
        &locked_app,
        Method::GET,
        "/.well-known/acme-challenge/token-123",
    )
    .await;
    assert_eq!(acme.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(acme).await,
        serde_json::json!({
            "message": "Challenge not found",
            "status": 404
        })
    );

    let near_prefix = send_empty_request(&locked_app, Method::GET, "/dashboard-admin").await;
    assert_invalid_credentials_response(near_prefix).await;

    let (_tmp, default_app) = build_auth_test_app_with_dashboard_policy(false);
    for path in ["/dashboard", "/swagger-ui/", "/api-docs/openapi.json"] {
        let resp = send_empty_request(&default_app, Method::GET, path).await;
        assert_eq!(resp.status(), StatusCode::OK, "path: {path}");
    }

    let swagger_redirect = send_empty_request(&default_app, Method::GET, "/swagger-ui").await;
    assert_eq!(swagger_redirect.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        swagger_redirect.headers().get(header::LOCATION),
        Some(&header::HeaderValue::from_static("/swagger-ui/"))
    );
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

    let add_peer = send_json_request(
        &app,
        Method::POST,
        "/internal/cluster/peers",
        serde_json::json!({"node_id": "", "addr": "not-an-origin"}),
    )
    .await;
    assert_eq!(
        add_peer.status(),
        StatusCode::NOT_FOUND,
        "no-auth mode must not expose runtime membership mutation"
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
