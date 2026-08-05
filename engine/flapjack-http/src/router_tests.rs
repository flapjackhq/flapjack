use std::{collections::HashMap, fs, path::Path, sync::Arc};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig};
use flapjack::index::manager::publication::{
    ContentDigest, PublicationEvent, PublicationGenerationEvidence, PublicationJournal,
    PublicationPaths, PublicationTarget, PublicationTransactionId,
};
use flapjack::{Document, FieldValue, IndexManager};
use flapjack_replication::{
    config::{NodeConfig, PeerConfig},
    manager::ReplicationManager,
};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower::ServiceExt;
use tracing_subscriber::prelude::*;
use wiremock::{
    matchers::{header as header_matcher, method as method_matcher, path as path_matcher},
    Mock, MockServer, ResponseTemplate,
};

use crate::auth::{ApiKey, KeyStore};
use crate::handlers::dashboard::{dashboard_test_asset_bytes, dashboard_test_index_bytes};
use crate::handlers::migration::{
    spool::{SpoolLimits, SpoolStore},
    with_test_algolia_base_url_override, AsyncMigrationSourceProvider,
};
use crate::middleware::REQUEST_ID_HEADER_NAME;
use crate::openapi::{DOCUMENTED_INTERNAL_MEMBERSHIP_PATHS, DOCUMENTED_MEMBERSHIP_SCHEMA_NAMES};
use crate::openapi_test_helpers::{
    assert_add_peer_openapi_contract, assert_cluster_status_openapi_contract,
    assert_remove_peer_openapi_contract,
};
use crate::test_helpers::{
    body_json, build_test_router, send_empty_request, send_json_request, EnvVarRestoreGuard,
    SharedLogBuffer, TestStateBuilder, ENV_MUTEX,
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
            replication_api_key: None,
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
    assert_cluster_status_openapi_contract(&document);
}

#[tokio::test]
async fn openapi_migration_status_schema_includes_resume_fields() {
    let (_tmp, app) = build_auth_test_app();
    let response = send_empty_request(&app, Method::GET, "/api-docs/openapi.json").await;

    assert_eq!(response.status(), StatusCode::OK);
    let document = body_json(response).await;
    let properties = document
        .pointer("/components/schemas/AsyncMigrationStatusResponse/properties")
        .and_then(serde_json::Value::as_object)
        .expect("AsyncMigrationStatusResponse schema must expose its properties");

    assert_eq!(
        properties.get("resumable"),
        Some(&serde_json::json!({"type": ["boolean", "null"]}))
    );
    assert_eq!(
        properties.get("operation"),
        Some(&serde_json::json!({"type": ["string", "null"]}))
    );
    assert_eq!(
        properties.get("resumeHandle"),
        Some(&serde_json::json!({"type": ["string", "null"]}))
    );
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
            replication_api_key: None,
        },
    )
}

fn build_auth_router_for_state(
    tmp: &TempDir,
    state: Arc<crate::handlers::AppState>,
    key_store: Arc<KeyStore>,
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
        Some(key_store),
        analytics_collector,
        trusted_proxy_matcher,
        tmp.path(),
        crate::router::RouterConfig {
            cors_mode: crate::startup::CorsMode::LoopbackOnly,
            disable_dashboard: false,
            replication_api_key: None,
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

fn write_committed_generation_evidence(
    base: &std::path::Path,
    target_name: &str,
    generation: &str,
) {
    let target = PublicationTarget::new(target_name).unwrap();
    let transaction = PublicationTransactionId::new(format!("{target_name}_current_gen")).unwrap();
    let paths = PublicationPaths::new(base, &target, &transaction);
    let journal = PublicationJournal::prepare(
        transaction,
        target,
        PublicationGenerationEvidence::new(generation).unwrap(),
        publication_digest(),
        paths.clone(),
    )
    .apply(PublicationEvent::Commit)
    .unwrap();
    std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
    std::fs::write(paths.journal, journal.to_json_value().to_string()).unwrap();
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

async fn assert_migration_job_not_found_response(resp: axum::response::Response) {
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(resp).await,
        serde_json::json!({
            "message": "Migration job not found",
            "status": 404,
            "code": "migration_job_not_found"
        })
    );
}

async fn assert_migration_resume_not_available_response(resp: axum::response::Response) {
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(resp).await,
        serde_json::json!({
            "message": "Migration resume is not available",
            "status": 409,
            "code": "migration_resume_not_available"
        })
    );
}

async fn assert_source_migration_alias_admin_contract(
    app: &axum::Router,
    source_provider: AsyncMigrationSourceProvider,
    non_admin_keys: [&str; 2],
) {
    let provider = source_provider.as_str().unwrap();
    let submit_path = format!("/1/migrations/{provider}");
    let submit_payload = serde_json::json!({
        "appId": "APPID",
        "apiKey": "source-key",
        "sourceIndex": "products",
        "targetIndex": "products_copy"
    });
    assert_invalid_credentials_response(
        post_json(app, &submit_path, None, submit_payload.clone()).await,
    )
    .await;
    for api_key in non_admin_keys {
        assert_method_not_allowed_response(
            post_json(app, &submit_path, Some(api_key), submit_payload.clone()).await,
        )
        .await;
    }

    let job_path = format!("/1/migrations/{provider}/01890f8e-8b28-78e8-b542-8cfdcb2d4f24");
    assert_invalid_credentials_response(get_request(app, &job_path, None).await).await;
    for api_key in non_admin_keys {
        assert_method_not_allowed_response(get_request(app, &job_path, Some(api_key)).await).await;
    }
    assert_migration_job_not_found_response(get_request(app, &job_path, Some("admin-key")).await)
        .await;

    assert_migration_job_action_contract(app, &job_path, non_admin_keys, source_provider).await;

    if source_provider == AsyncMigrationSourceProvider::Typesense {
        let recognized_provider =
            post_json(app, &submit_path, Some("admin-key"), submit_payload).await;
        assert_eq!(recognized_provider.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(recognized_provider).await,
            serde_json::json!({
                "message": "Source provider is not supported",
                "status": 400,
                "code": "source_provider_unsupported"
            })
        );
    }
}

async fn assert_migration_job_action_contract(
    app: &axum::Router,
    job_path: &str,
    non_admin_keys: [&str; 2],
    source_provider: AsyncMigrationSourceProvider,
) {
    let mut actions = vec![
        ("cancel", serde_json::json!({}), false),
        ("acknowledge", serde_json::json!({}), false),
    ];
    if source_provider == AsyncMigrationSourceProvider::Algolia {
        actions.push((
            "resume",
            serde_json::json!({
                "appId": "APPID",
                "apiKey": "source-key",
                "sourceIndex": "products"
            }),
            true,
        ));
    }
    for (action, payload, resume_not_available_for_missing_job) in actions {
        let action_path = format!("{job_path}/{action}");
        assert_invalid_credentials_response(
            post_json(app, &action_path, None, payload.clone()).await,
        )
        .await;
        for api_key in non_admin_keys {
            assert_method_not_allowed_response(
                post_json(app, &action_path, Some(api_key), payload.clone()).await,
            )
            .await;
        }
        let missing_job_response = post_json(app, &action_path, Some("admin-key"), payload).await;
        if resume_not_available_for_missing_job {
            assert_migration_resume_not_available_response(missing_job_response).await;
        } else {
            assert_migration_job_not_found_response(missing_job_response).await;
        }
    }
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

async fn post_ndjson(
    app: &axum::Router,
    uri: &str,
    api_key: Option<&str>,
    body: &str,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-ndjson");
    if let Some(api_key) = api_key {
        builder = builder
            .header("x-algolia-api-key", api_key)
            .header("x-algolia-application-id", "route-contract-app");
    }
    app.clone()
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap()
}

fn peer_configured_replication_manager(data_dir: &std::path::Path) -> Arc<ReplicationManager> {
    ReplicationManager::new(
        NodeConfig {
            node_id: "bulk-replace-local".to_string(),
            bind_addr: "127.0.0.1:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "bulk-replace-peer".to_string(),
                addr: "http://127.0.0.1:7701".to_string(),
            }],
        },
        None,
        data_dir.to_path_buf(),
    )
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

// The process-global environment lock must span each asynchronous router
// request so another test cannot change S3 configuration mid-fixture.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn internal_snapshot_capability_requires_admin_authentication() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let _bucket = EnvVarRestoreGuard::remove("FLAPJACK_S3_BUCKET");
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let search_key = search_only_key_value(&key_store);
    let app = build_test_router(&tmp, Some(key_store));

    let missing_credentials = get_request(&app, "/internal/snapshots/capability", None).await;
    assert_invalid_credentials_response(missing_credentials).await;

    let search_only = get_request(&app, "/internal/snapshots/capability", Some(&search_key)).await;
    assert_method_not_allowed_response(search_only).await;

    let admin = get_request(&app, "/internal/snapshots/capability", Some("admin-key")).await;
    assert_eq!(admin.status(), StatusCode::OK);
    assert_eq!(
        body_json(admin).await,
        serde_json::json!({
            "backend": "s3",
            "state": "not_configured",
            "bucket": null
        })
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn internal_snapshot_capability_remains_available_without_authentication() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let _bucket = EnvVarRestoreGuard::remove("FLAPJACK_S3_BUCKET");
    let (_tmp, app) = build_no_auth_test_app();

    let response = get_request(&app, "/internal/snapshots/capability", None).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({
            "backend": "s3",
            "state": "not_configured",
            "bucket": null
        })
    );
}

#[tokio::test]
async fn migration_routes_preserve_admin_contract() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let search_key = search_only_key_value(&key_store);
    let write_key = create_test_key_with_acl(&key_store, "addObject");
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

    for source_provider in AsyncMigrationSourceProvider::PUBLIC {
        assert_source_migration_alias_admin_contract(
            &app,
            source_provider,
            [&search_key, &write_key],
        )
        .await;
    }

    let unknown_provider = post_json(
        &app,
        "/1/migrations/not-a-provider",
        Some("admin-key"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(unknown_provider.status(), StatusCode::NOT_FOUND);

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
async fn migration_preview_route_preserves_admin_contract() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let search_key = search_only_key_value(&key_store);
    let write_key = create_test_key_with_acl(&key_store, "addObject");
    let app = build_test_router(&tmp, Some(key_store));
    let payload = serde_json::json!({
        "appId": "APPID",
        "apiKey": "source-key",
        "sourceIndex": "products",
        "targetIndex": "products_copy"
    });

    for source_provider in AsyncMigrationSourceProvider::PUBLIC {
        let provider = source_provider.as_str().unwrap();
        let path = format!("/1/migrations/{provider}/preview");
        assert_invalid_credentials_response(post_json(&app, &path, None, payload.clone()).await)
            .await;
        for api_key in [&search_key, &write_key] {
            assert_method_not_allowed_response(
                post_json(&app, &path, Some(api_key), payload.clone()).await,
            )
            .await;
        }
    }

    for source_provider in AsyncMigrationSourceProvider::PUBLIC {
        let provider = source_provider.as_str().unwrap();
        let routed_admin_request = post_json(
            &app,
            &format!("/1/migrations/{provider}/preview"),
            Some("admin-key"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(
            routed_admin_request.status(),
            StatusCode::BAD_REQUEST,
            "{provider} preview must route admin-key requests to body validation"
        );
        assert_eq!(
            body_json(routed_admin_request).await,
            serde_json::json!({
                "message": "Invalid migration request body",
                "status": 400
            }),
            "{provider} preview must share the migration body validation contract"
        );
    }
}

#[tokio::test]
async fn migration_preview_refuses_unknown_source_providers() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));
    let payload = serde_json::json!({
        "appId": "APPID",
        "apiKey": "source-key",
        "sourceIndex": "products",
        "targetIndex": "products_copy"
    });

    let unknown = post_json(
        &app,
        "/1/migrations/not-a-provider/preview",
        Some("admin-key"),
        payload.clone(),
    )
    .await;
    assert_eq!(
        unknown.status(),
        StatusCode::NOT_FOUND,
        "unknown provider segments must remain outside the closed migration router"
    );
}

// ── Neutral source-discovery contract ─────────────────────────────────────
//
// These tests drive the served `/1/migrations/{provider}/list-indexes` routes
// through string paths and bodies, keeping the HTTP contract independent of the
// provider-client implementation. The MIG-12 provider-neutral source seam is
// `engine/flapjack-http/src/handlers/migration/source_reader.rs`; its seam types
// are `MigrationSourceReader` (trait), `SourceExportRecord`, `SourceExportError`,
// `SourceExportSink`, and `AcceptedSourceExport`.

/// A syntactically valid provider-specific discovery body (credentials + host).
fn source_discovery_request_body(provider: AsyncMigrationSourceProvider) -> serde_json::Value {
    match provider {
        AsyncMigrationSourceProvider::Algolia => {
            serde_json::json!({ "appId": "APPID", "apiKey": "source-key" })
        }
        AsyncMigrationSourceProvider::Meilisearch => {
            serde_json::json!({ "endpoint": "http://127.0.0.1:1", "apiKey": "source-key" })
        }
        AsyncMigrationSourceProvider::Typesense => {
            serde_json::json!({ "node": "http://127.0.0.1:1", "apiKey": "source-key" })
        }
    }
}

/// Build a discovery body with a variable host field name (`endpoint`/`node`).
fn discovery_host_body(host_field: &str, host: &str, api_key: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        host_field.to_string(),
        serde_json::Value::String(host.to_string()),
    );
    map.insert(
        "apiKey".to_string(),
        serde_json::Value::String(api_key.to_string()),
    );
    serde_json::Value::Object(map)
}

/// Start a test-owned JSON upstream and pin the complete request contract.
async fn start_discovery_upstream(
    request_path: &str,
    query: &[(&str, &str)],
    required_headers: &[(&str, &str)],
    status: u16,
    response: serde_json::Value,
) -> MockServer {
    let server = MockServer::start().await;
    assert!(
        server.address().ip().is_loopback() && server.address().port() != 0,
        "discovery test upstream must bind an ephemeral loopback port before the request"
    );

    let mut mock = Mock::given(method_matcher("GET")).and(path_matcher(request_path));
    for (name, value) in required_headers {
        mock = mock.and(header_matcher(*name, *value));
    }
    let mut expected_query = query
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect::<Vec<_>>();
    expected_query.sort();
    // Match the complete query multiset. Presence-only matchers would accept
    // forbidden Typesense search pagination (`page`/`per_page`) as extras.
    mock = mock.and(move |request: &wiremock::Request| {
        let mut actual_query = request
            .url
            .query_pairs()
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        actual_query.sort();
        actual_query == expected_query
    });
    mock.respond_with(ResponseTemplate::new(status).set_body_json(response))
        .expect(1)
        .mount(&server)
        .await;
    server
}

async fn mount_meilisearch_stats_upstream(
    server: &MockServer,
    authorization: &str,
    counts: &[(&str, u64)],
) {
    for (index_uid, document_count) in counts {
        Mock::given(method_matcher("GET"))
            .and(path_matcher(format!("/indexes/{index_uid}/stats")))
            .and(header_matcher("authorization", authorization))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "numberOfDocuments": document_count,
                "fieldDistribution": {},
                "isIndexing": false
            })))
            .expect(1)
            .mount(server)
            .await;
    }
}

fn read_file_tree(root: &Path) -> Vec<u8> {
    let mut paths = vec![root.to_path_buf()];
    let mut bytes = Vec::new();
    while let Some(path) = paths.pop() {
        let metadata = fs::metadata(&path)
            .unwrap_or_else(|error| panic!("required leak-sweep path {}: {error}", path.display()));
        if metadata.is_dir() {
            for entry in fs::read_dir(&path).unwrap_or_else(|error| {
                panic!("read leak-sweep directory {}: {error}", path.display())
            }) {
                paths.push(entry.expect("read leak-sweep directory entry").path());
            }
        } else if metadata.is_file() {
            bytes.extend(fs::read(&path).unwrap_or_else(|error| {
                panic!("read leak-sweep file {}: {error}", path.display())
            }));
        }
    }
    bytes
}

fn assert_secret_absent(surface: &str, bytes: &[u8], secret: &str) {
    assert!(
        !String::from_utf8_lossy(bytes).contains(secret),
        "{surface} leaked the source credential sentinel"
    );
}

fn discovery_credential_sentinel() -> String {
    ["sk-sentinel-", "DO-NOT-LEAK-", "9f3c"].concat()
}

fn typesense_collection_summaries(summaries: &[(&str, u64, u64, &str)]) -> serde_json::Value {
    serde_json::Value::Array(
        summaries
            .iter()
            .map(
                |(name, document_count, created_at, default_sorting_field)| {
                    serde_json::json!({
                        "name": name,
                        "num_documents": document_count,
                        "created_at": created_at,
                        "default_sorting_field": default_sorting_field
                    })
                },
            )
            .collect(),
    )
}

struct ExpectedNeutralDiscoveryMetadata<'a> {
    name: &'a str,
    primary_key: Option<&'a str>,
    entries: Option<u64>,
    document_count: Option<u64>,
    created_at: Option<serde_json::Value>,
    updated_at: Option<&'a str>,
    default_sorting_field: Option<&'a str>,
}

impl<'a> ExpectedNeutralDiscoveryMetadata<'a> {
    fn empty(name: &'a str) -> Self {
        Self {
            name,
            primary_key: None,
            entries: None,
            document_count: None,
            created_at: None,
            updated_at: None,
            default_sorting_field: None,
        }
    }

    fn algolia(name: &'a str, entries: u64, updated_at: &'a str) -> Self {
        Self {
            entries: Some(entries),
            updated_at: Some(updated_at),
            ..Self::empty(name)
        }
    }

    fn meilisearch(
        name: &'a str,
        primary_key: Option<&'a str>,
        document_count: u64,
        created_at: &'a str,
        updated_at: &'a str,
    ) -> Self {
        Self {
            primary_key,
            document_count: Some(document_count),
            created_at: Some(serde_json::json!(created_at)),
            updated_at: Some(updated_at),
            ..Self::empty(name)
        }
    }

    fn typesense(
        name: &'a str,
        document_count: u64,
        created_at: u64,
        default_sorting_field: &'a str,
    ) -> Self {
        Self {
            document_count: Some(document_count),
            created_at: Some(serde_json::json!(created_at)),
            default_sorting_field: Some(default_sorting_field),
            ..Self::empty(name)
        }
    }
}

fn assert_neutral_discovery_metadata(
    document: &serde_json::Value,
    expected: &[ExpectedNeutralDiscoveryMetadata<'_>],
) {
    let indexes = document
        .get("indexes")
        .and_then(serde_json::Value::as_array)
        .expect("neutral discovery response must contain an indexes array");
    assert_eq!(indexes.len(), expected.len());
    let indexes_by_name: HashMap<_, _> = indexes
        .iter()
        .map(|index| {
            let name = index
                .get("name")
                .and_then(serde_json::Value::as_str)
                .expect("each neutral discovery entry must contain a name");
            (name, index)
        })
        .collect();
    assert_eq!(
        indexes_by_name.len(),
        expected.len(),
        "index names must be unique"
    );

    for expected_entry in expected {
        let name = expected_entry.name;
        let index = indexes_by_name
            .get(name)
            .unwrap_or_else(|| panic!("missing neutral discovery entry {name}"));
        let expected_primary_key = expected_entry
            .primary_key
            .map(|value| serde_json::Value::String(value.to_string()))
            .unwrap_or(serde_json::Value::Null);
        let expected_entries = expected_entry
            .entries
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null);
        let expected_document_count = expected_entry
            .document_count
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null);
        let expected_created_at = expected_entry
            .created_at
            .clone()
            .unwrap_or(serde_json::Value::Null);
        let expected_updated_at = expected_entry
            .updated_at
            .map(|value| serde_json::Value::String(value.to_string()))
            .unwrap_or(serde_json::Value::Null);
        let expected_default_sorting_field = expected_entry
            .default_sorting_field
            .map(|value| serde_json::Value::String(value.to_string()))
            .unwrap_or(serde_json::Value::Null);
        assert_eq!(
            index.get("primaryKey"),
            Some(&expected_primary_key),
            "{name} primaryKey"
        );
        assert_eq!(
            index.get("entries"),
            Some(&expected_entries),
            "{name} entries"
        );
        assert_eq!(
            index.get("documentCount"),
            Some(&expected_document_count),
            "{name} documentCount"
        );
        assert_eq!(
            index.get("createdAt"),
            Some(&expected_created_at),
            "{name} createdAt"
        );
        assert_eq!(
            index.get("updatedAt"),
            Some(&expected_updated_at),
            "{name} updatedAt"
        );
        assert_eq!(
            index.get("defaultSortingField"),
            Some(&expected_default_sorting_field),
            "{name} defaultSortingField"
        );
    }
}

#[tokio::test]
async fn source_discovery_route_is_mounted_for_every_public_provider() {
    // `/1/migrations/{provider}/list-indexes` must remain a mounted POST route for
    // every public provider rather than falling through to the job-status route.
    let (_tmp, app) = build_auth_test_app();

    for source_provider in AsyncMigrationSourceProvider::PUBLIC {
        let provider = source_provider.as_str().unwrap();
        let path = format!("/1/migrations/{provider}/list-indexes");
        let body = source_discovery_request_body(source_provider);
        let response = post_json(&app, &path, Some("admin-key"), body).await;
        let status = response.status();
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{provider} list-indexes must be a mounted route, not a routing 404"
        );
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{provider} list-indexes must accept POST, not fall through to the job-status GET route"
        );
    }
}

#[tokio::test]
async fn source_discovery_route_preserves_job_status_param_route() {
    // Coexistence guard: the literal `list-indexes` segment must not shadow the
    // `:job_id` param route. A UUID job id still reaches the job-status handler.
    // This remains a regression guard alongside the literal discovery route.
    let (_tmp, app) = build_auth_test_app();
    for source_provider in AsyncMigrationSourceProvider::PUBLIC {
        let provider = source_provider.as_str().unwrap();
        let job_path = format!("/1/migrations/{provider}/01890f8e-8b28-78e8-b542-8cfdcb2d4f24");
        assert_migration_job_not_found_response(
            get_request(&app, &job_path, Some("admin-key")).await,
        )
        .await;
    }
}

#[tokio::test]
async fn published_migration_paths_are_all_mounted() {
    // Served-route vs published-OpenAPI invariant: every `/1/migrations/` path the
    // served OpenAPI advertises with a `post` operation must resolve on the router.
    // A routing miss is an empty-bodied 404 from the axum fallback; a mounted
    // handler reporting "job not found" returns 404 WITH a JSON body — only the
    // former means an advertised path is unserved. This prevents OpenAPI and
    // router registration from drifting apart.
    let (_tmp, app) = build_auth_test_app();
    let response = send_empty_request(&app, Method::GET, "/api-docs/openapi.json").await;
    assert_eq!(response.status(), StatusCode::OK);
    let document = body_json(response).await;
    let paths = document
        .get("paths")
        .and_then(serde_json::Value::as_object)
        .expect("served OpenAPI must expose paths");

    let mut checked = 0usize;
    for (path, operations) in paths {
        if !path.starts_with("/1/migrations/") {
            continue;
        }
        let Some(ops) = operations.as_object() else {
            continue;
        };
        if !ops.contains_key("post") {
            continue;
        }
        // Migration paths embed concrete provider segments; only `{job_id}` is
        // templated. Substitute a concrete UUID so the request reaches the router.
        let concrete = path.replace("{job_id}", "01890f8e-8b28-78e8-b542-8cfdcb2d4f24");
        let response = post_json(&app, &concrete, Some("admin-key"), serde_json::json!({})).await;
        let status = response.status();
        let body = body_bytes(response).await;
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "published migration POST path {path} (requested as {concrete}) fell through to a route that does not accept POST"
        );
        assert!(
            !(status == StatusCode::NOT_FOUND && body.is_empty()),
            "published migration path {path} (requested as {concrete}) is not mounted on the router"
        );
        checked += 1;
    }
    assert!(
        checked >= 1,
        "expected at least one published /1/migrations POST path to check"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn algolia_list_indexes_compat_contract_is_preserved() {
    // Algolia wire-compat guard for the legacy `/1/algolia-list-indexes` handler.
    // The neutral discovery route must not disturb it, so this exercises the
    // legacy path end to end against a deterministic test-owned upstream rather
    // than serializing `ListAlgoliaIndexesResponse` in the test — a constructed
    // value proves the struct's serde attributes and nothing about what the
    // served route actually returns.
    //
    // (1) The route still reaches the legacy handler — proven by its own 400
    // validation message rather than a routing 404/405 or a neutral-route capture.
    let (_tmp, app) = build_auth_test_app();
    let routed = post_json(
        &app,
        "/1/algolia-list-indexes",
        Some("admin-key"),
        serde_json::json!({ "appId": "", "apiKey": "" }),
    )
    .await;
    assert_eq!(routed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(routed).await,
        serde_json::json!({
            "message": "appId and apiKey are required",
            "status": 400
        })
    );

    // (2) A successful served request returns exactly `{ indexes: [{ name,
    // entries, updatedAt }] }` carrying the upstream's live values, in upstream
    // order. The upstream is pinned to `GET /1/indexes?page=0&hitsPerPage=100`
    // with the credential headers, so a changed request contract fails the mock
    // rather than silently passing. Upstream-only fields (`pendingTask`, and any
    // field the vendor adds) must not reach the client, which whole-body
    // equality — not field-presence checks — is what catches.
    let upstream = start_discovery_upstream(
        "/1/indexes",
        &[("page", "0"), ("hitsPerPage", "100")],
        &[
            ("x-algolia-application-id", "COMPATAPP1"),
            ("x-algolia-api-key", "compat-source-key"),
        ],
        200,
        serde_json::json!({
            "items": [
                {
                    "name": "compat_products",
                    "entries": 42,
                    "updatedAt": "2026-07-26T00:00:00Z",
                    "createdAt": "2026-07-01T00:00:00Z",
                    "pendingTask": false
                },
                {
                    "name": "compat_categories",
                    "entries": 7,
                    "updatedAt": "2026-07-25T12:34:56Z",
                    "createdAt": "2026-07-02T00:00:00Z",
                    "pendingTask": false
                }
            ],
            "page": 0,
            "nbPages": 1
        }),
    )
    .await;

    let upstream_uri = upstream.uri();
    let listed =
        with_test_algolia_base_url_override(Some("COMPATAPP1"), Some(&upstream_uri), async {
            post_json(
                &app,
                "/1/algolia-list-indexes",
                Some("admin-key"),
                serde_json::json!({ "appId": "COMPATAPP1", "apiKey": "compat-source-key" }),
            )
            .await
        })
        .await;
    assert_eq!(
        listed.status(),
        StatusCode::OK,
        "the legacy Algolia discovery route must complete against the test upstream"
    );
    assert_eq!(
        body_json(listed).await,
        serde_json::json!({
            "indexes": [
                { "name": "compat_products", "entries": 42, "updatedAt": "2026-07-26T00:00:00Z" },
                { "name": "compat_categories", "entries": 7, "updatedAt": "2026-07-25T12:34:56Z" }
            ]
        })
    );
    upstream.verify().await;
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn list_source_indexes_returns_meilisearch_known_answer() {
    // This reaches a test-owned Meilisearch upstream through the explicit
    // loopback opt-in. M0A known answer
    // (docs2/4_EVIDENCE/2026_07_26_jul26_am_12_meilisearch_source_contract.md,
    // "Index discovery" row): `GET /indexes?limit=10` returns uid set
    // {ambiguous_pk, configured_pk, inferred_pk} with total=3, offset=0, limit=10.
    // The neutral response preserves the upstream pagination triple, so this
    // checks the values rather than treating array length as a pagination proxy.
    let upstream = start_discovery_upstream(
        "/indexes",
        &[("limit", "10")],
        &[("authorization", "Bearer m0a-source-key")],
        200,
        serde_json::json!({
            "results": [
                {
                    "uid": "ambiguous_pk",
                    "primaryKey": null,
                    "createdAt": "2026-07-26T00:00:00Z",
                    "updatedAt": "2026-07-26T00:00:00Z"
                },
                {
                    "uid": "configured_pk",
                    "primaryKey": "sku",
                    "createdAt": "2026-07-26T00:00:01Z",
                    "updatedAt": "2026-07-26T00:00:01Z"
                },
                {
                    "uid": "inferred_pk",
                    "primaryKey": "book_id",
                    "createdAt": "2026-07-26T00:00:02Z",
                    "updatedAt": "2026-07-26T00:00:02Z"
                }
            ],
            "total": 3,
            "offset": 0,
            "limit": 10
        }),
    )
    .await;
    mount_meilisearch_stats_upstream(
        &upstream,
        "Bearer m0a-source-key",
        &[
            ("ambiguous_pk", 0),
            ("configured_pk", 3),
            ("inferred_pk", 2),
        ],
    )
    .await;
    let _env_lock = ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _loopback = EnvVarRestoreGuard::set("FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK", "1");
    let (_tmp, app) = build_auth_test_app();
    let response = post_json(
        &app,
        "/1/migrations/meilisearch/list-indexes?limit=10",
        Some("admin-key"),
        serde_json::json!({ "endpoint": upstream.uri(), "apiKey": "m0a-source-key" }),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "meilisearch discovery must return 200 once the route is mounted"
    );
    let document = body_json(response).await;
    let mut names: Vec<String> = document
        .get("indexes")
        .and_then(serde_json::Value::as_array)
        .expect("discovery response must carry an `indexes` array")
        .iter()
        .map(|entry| {
            entry
                .get("name")
                .and_then(serde_json::Value::as_str)
                .expect("each discovery entry must carry a `name`")
                .to_string()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "ambiguous_pk".to_string(),
            "configured_pk".to_string(),
            "inferred_pk".to_string()
        ]
    );
    assert_eq!(names.len(), 3);
    assert_neutral_discovery_metadata(
        &document,
        &[
            ExpectedNeutralDiscoveryMetadata::meilisearch(
                "ambiguous_pk",
                None,
                0,
                "2026-07-26T00:00:00Z",
                "2026-07-26T00:00:00Z",
            ),
            ExpectedNeutralDiscoveryMetadata::meilisearch(
                "configured_pk",
                Some("sku"),
                3,
                "2026-07-26T00:00:01Z",
                "2026-07-26T00:00:01Z",
            ),
            ExpectedNeutralDiscoveryMetadata::meilisearch(
                "inferred_pk",
                Some("book_id"),
                2,
                "2026-07-26T00:00:02Z",
                "2026-07-26T00:00:02Z",
            ),
        ],
    );
    assert_eq!(
        document.get("total").and_then(serde_json::Value::as_u64),
        Some(3)
    );
    assert_eq!(
        document.get("offset").and_then(serde_json::Value::as_u64),
        Some(0)
    );
    assert_eq!(
        document.get("limit").and_then(serde_json::Value::as_u64),
        Some(10)
    );
    upstream.verify().await;
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn list_source_indexes_maps_meilisearch_invalid_api_key() {
    // M0A "Least-privilege actions" row: an upstream key missing
    // `indexes.get` yields HTTP 403 `invalid_api_key` (type `auth`). The discovery
    // endpoint must surface that as a labelled 403 refusal, not a 200 or an opaque
    // 5xx.
    let upstream = start_discovery_upstream(
        "/indexes",
        &[("limit", "10")],
        &[("authorization", "Bearer missing-indexes-get")],
        403,
        serde_json::json!({
            "message": "The provided API key is invalid.",
            "code": "invalid_api_key",
            "type": "auth",
            "link": "https://docs.meilisearch.com/errors#invalid_api_key"
        }),
    )
    .await;
    let _env_lock = ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _loopback = EnvVarRestoreGuard::set("FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK", "1");
    let (_tmp, app) = build_auth_test_app();
    let response = post_json(
        &app,
        "/1/migrations/meilisearch/list-indexes?limit=10",
        Some("admin-key"),
        serde_json::json!({ "endpoint": upstream.uri(), "apiKey": "missing-indexes-get" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = body_json(response).await;
    assert_eq!(
        body.get("code").and_then(serde_json::Value::as_str),
        Some("invalid_api_key")
    );
    upstream.verify().await;
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn list_source_indexes_returns_typesense_known_answer() {
    // This reaches a test-owned Typesense upstream through the explicit loopback
    // opt-in. The collection name set comes from
    // tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json:
    // {fj_ts_migration_products, fj_ts_migration_categories}. Creation order in
    // tests/typesense_migration_contract.sh (lines 196-197): categories are created
    // BEFORE products, so Typesense `GET /collections` returns newest-first:
    // ["fj_ts_migration_products", "fj_ts_migration_categories"]. Hand-calculated
    // pagination slices over that ordered list:
    //   limit=1            -> ["fj_ts_migration_products"]
    //   offset=1&limit=1   -> ["fj_ts_migration_categories"]
    //   offset=1           -> ["fj_ts_migration_categories"]
    //   offset=2&limit=1   -> HTTP 400 {"message":"Invalid offset param."}
    // Counts and sorting fields come from the canonical M0B bundle. `created_at`
    // uses the source-contract probe's recorded stable marker; response position,
    // not a fabricated timestamp, owns the newest-first ordering assertion.
    let expected = [
        ("fj_ts_migration_products", 3, 1_785_020_400, "price"),
        ("fj_ts_migration_categories", 2, 1_785_020_400, "priority"),
    ];
    let full_upstream = start_discovery_upstream(
        "/collections",
        &[("exclude_fields", "fields")],
        &[("x-typesense-api-key", "m0b-source-key")],
        200,
        typesense_collection_summaries(&expected),
    )
    .await;
    let limit_upstream = start_discovery_upstream(
        "/collections",
        &[("exclude_fields", "fields"), ("limit", "1")],
        &[("x-typesense-api-key", "m0b-source-key")],
        200,
        typesense_collection_summaries(&expected[..1]),
    )
    .await;
    let page_upstream = start_discovery_upstream(
        "/collections",
        &[
            ("exclude_fields", "fields"),
            ("offset", "1"),
            ("limit", "1"),
        ],
        &[("x-typesense-api-key", "m0b-source-key")],
        200,
        typesense_collection_summaries(&expected[1..]),
    )
    .await;
    let offset_without_limit_upstream = start_discovery_upstream(
        "/collections",
        &[("exclude_fields", "fields"), ("offset", "1")],
        &[("x-typesense-api-key", "m0b-source-key")],
        200,
        typesense_collection_summaries(&expected[1..]),
    )
    .await;
    let exhausted_upstream = start_discovery_upstream(
        "/collections",
        &[
            ("exclude_fields", "fields"),
            ("offset", "2"),
            ("limit", "1"),
        ],
        &[("x-typesense-api-key", "m0b-source-key")],
        400,
        serde_json::json!({ "message": "Invalid offset param." }),
    )
    .await;

    let _env_lock = ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _loopback = EnvVarRestoreGuard::set("FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK", "1");
    let (_tmp, app) = build_auth_test_app();

    let full = post_json(
        &app,
        "/1/migrations/typesense/list-indexes",
        Some("admin-key"),
        serde_json::json!({ "node": full_upstream.uri(), "apiKey": "m0b-source-key" }),
    )
    .await;
    assert_eq!(full.status(), StatusCode::OK);
    let full_document = body_json(full).await;
    assert_eq!(
        discovery_index_names(full_document.clone()),
        vec![
            "fj_ts_migration_products".to_string(),
            "fj_ts_migration_categories".to_string()
        ],
        "typesense discovery must preserve newest-first collection order"
    );
    assert_neutral_discovery_metadata(
        &full_document,
        &[
            ExpectedNeutralDiscoveryMetadata::typesense(
                "fj_ts_migration_products",
                3,
                1_785_020_400,
                "price",
            ),
            ExpectedNeutralDiscoveryMetadata::typesense(
                "fj_ts_migration_categories",
                2,
                1_785_020_400,
                "priority",
            ),
        ],
    );

    let limit1 = post_json(
        &app,
        "/1/migrations/typesense/list-indexes?limit=1",
        Some("admin-key"),
        serde_json::json!({ "node": limit_upstream.uri(), "apiKey": "m0b-source-key" }),
    )
    .await;
    assert_eq!(limit1.status(), StatusCode::OK);
    let limit1_document = body_json(limit1).await;
    assert_eq!(
        discovery_index_names(limit1_document.clone()),
        vec!["fj_ts_migration_products".to_string()]
    );
    assert_neutral_discovery_metadata(
        &limit1_document,
        &[ExpectedNeutralDiscoveryMetadata::typesense(
            "fj_ts_migration_products",
            3,
            1_785_020_400,
            "price",
        )],
    );

    let page2 = post_json(
        &app,
        "/1/migrations/typesense/list-indexes?offset=1&limit=1",
        Some("admin-key"),
        serde_json::json!({ "node": page_upstream.uri(), "apiKey": "m0b-source-key" }),
    )
    .await;
    assert_eq!(page2.status(), StatusCode::OK);
    let page2_document = body_json(page2).await;
    assert_eq!(
        discovery_index_names(page2_document.clone()),
        vec!["fj_ts_migration_categories".to_string()]
    );
    assert_neutral_discovery_metadata(
        &page2_document,
        &[ExpectedNeutralDiscoveryMetadata::typesense(
            "fj_ts_migration_categories",
            2,
            1_785_020_400,
            "priority",
        )],
    );

    let offset_without_limit = post_json(
        &app,
        "/1/migrations/typesense/list-indexes?offset=1",
        Some("admin-key"),
        serde_json::json!({
            "node": offset_without_limit_upstream.uri(),
            "apiKey": "m0b-source-key"
        }),
    )
    .await;
    assert_eq!(offset_without_limit.status(), StatusCode::OK);
    let offset_without_limit_document = body_json(offset_without_limit).await;
    assert_eq!(
        discovery_index_names(offset_without_limit_document.clone()),
        vec!["fj_ts_migration_categories".to_string()]
    );
    assert_neutral_discovery_metadata(
        &offset_without_limit_document,
        &[ExpectedNeutralDiscoveryMetadata::typesense(
            "fj_ts_migration_categories",
            2,
            1_785_020_400,
            "priority",
        )],
    );

    let exhausted = post_json(
        &app,
        "/1/migrations/typesense/list-indexes?offset=2&limit=1",
        Some("admin-key"),
        serde_json::json!({
            "node": exhausted_upstream.uri(),
            "apiKey": "m0b-source-key"
        }),
    )
    .await;
    assert_eq!(exhausted.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        body_json(exhausted).await,
        serde_json::json!({
            "message": "Typesense request failed",
            "status": 502
        })
    );
    full_upstream.verify().await;
    limit_upstream.verify().await;
    page_upstream.verify().await;
    offset_without_limit_upstream.verify().await;
    exhausted_upstream.verify().await;
}

/// Extract the ordered `name` values from a neutral discovery response.
fn discovery_index_names(document: serde_json::Value) -> Vec<String> {
    document
        .get("indexes")
        .and_then(serde_json::Value::as_array)
        .expect("discovery response must carry an `indexes` array")
        .iter()
        .map(|entry| {
            entry
                .get("name")
                .and_then(serde_json::Value::as_str)
                .expect("each discovery entry must carry a `name`")
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn list_source_indexes_refuses_non_vendor_host() {
    // The vetting matrix is owned by `engine/src/security_tests.rs` and is not
    // re-derived here. This only asserts that a non-vendor host is refused with a
    // labelled 400 vendor-policy error rather than reaching an upstream.
    let (_tmp, app) = build_auth_test_app();
    for (provider, host_field, expected_message) in [
        (
            "meilisearch",
            "endpoint",
            "Meilisearch Cloud endpoint is not allowed",
        ),
        (
            "typesense",
            "node",
            "Typesense Cloud endpoint is not allowed",
        ),
    ] {
        let path = format!("/1/migrations/{provider}/list-indexes");
        let body = discovery_host_body(host_field, "https://evil.example.com", "source-key");
        let response = post_json(&app, &path, Some("admin-key"), body).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{provider} must refuse a non-vendor host before any outbound call"
        );
        assert_eq!(
            body_json(response).await,
            serde_json::json!({
                "message": expected_message,
                "status": 400
            }),
            "{provider} must return its canonical safe vendor-policy refusal"
        );
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn list_source_indexes_gates_loopback_behind_opt_in() {
    // With the opt-in env unset a loopback host is refused; with it set to exactly
    // "1" a literal loopback IP is accepted while hostname `localhost` stays
    // refused.
    //
    // The expected upstream query is per-provider: an unpaginated Meilisearch
    // discovery request carries no query at all, while every Typesense
    // discovery request carries `exclude_fields=fields` (the same contract
    // `list_source_indexes_returns_typesense_known_answer` pins). Sharing one
    // empty expectation across both providers would demand two mutually
    // exclusive Typesense request contracts.
    for (
        provider,
        host_field,
        env_var,
        upstream_path,
        upstream_query,
        header_name,
        response_body,
    ) in [
        (
            "meilisearch",
            "endpoint",
            "FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK",
            "/indexes",
            &[][..],
            "authorization",
            serde_json::json!({ "results": [], "total": 0, "offset": 0, "limit": 20 }),
        ),
        (
            "typesense",
            "node",
            "FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK",
            "/collections",
            &[("exclude_fields", "fields")][..],
            "x-typesense-api-key",
            serde_json::json!([]),
        ),
    ] {
        let header_value = if provider == "meilisearch" {
            "Bearer loopback-source-key"
        } else {
            "loopback-source-key"
        };
        let upstream = start_discovery_upstream(
            upstream_path,
            upstream_query,
            &[(header_name, header_value)],
            200,
            response_body,
        )
        .await;
        let localhost_uri = upstream.uri().replacen("127.0.0.1", "localhost", 1);
        let _env_lock = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = format!("/1/migrations/{provider}/list-indexes");

        {
            let _unset = EnvVarRestoreGuard::remove(env_var);
            let (_tmp, app) = build_auth_test_app();
            let refused = post_json(
                &app,
                &path,
                Some("admin-key"),
                discovery_host_body(host_field, &upstream.uri(), "loopback-source-key"),
            )
            .await;
            assert_eq!(
                refused.status(),
                StatusCode::BAD_REQUEST,
                "{provider} loopback host must be refused when {env_var} is unset"
            );
        }

        {
            let _set = EnvVarRestoreGuard::set(env_var, "1");
            let (_tmp, app) = build_auth_test_app();
            let accepted = post_json(
                &app,
                &path,
                Some("admin-key"),
                discovery_host_body(host_field, &upstream.uri(), "loopback-source-key"),
            )
            .await;
            assert_eq!(
                accepted.status(),
                StatusCode::OK,
                "{provider} literal loopback IP must complete discovery when {env_var}=1"
            );

            let localhost_refused = post_json(
                &app,
                &path,
                Some("admin-key"),
                discovery_host_body(host_field, &localhost_uri, "loopback-source-key"),
            )
            .await;
            assert_eq!(
                localhost_refused.status(),
                StatusCode::BAD_REQUEST,
                "{provider} hostname localhost must stay refused even under {env_var}=1"
            );
        }
        upstream.verify().await;
    }
}

#[tokio::test]
async fn list_source_indexes_refuses_payload_mismatch() {
    // Body-mismatch: an Algolia-shaped `appId` sent to the hosted-source routes,
    // and hosted-source host fields sent to the Algolia or
    // opposite hosted route, must produce a labelled
    // `source_provider_payload_mismatch` refusal rather than serde coercion or
    // silent misrouting. Run those failure paths under the same log and
    // temporary-state sweep as the successful leak guard so parser/admission-only
    // credential leaks are observable.
    let sentinel = discovery_credential_sentinel();
    let logs = SharedLogBuffer::default();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .without_time()
            .with_ansi(false)
            .with_writer(logs.clone()),
    );
    let _log_guard = tracing::subscriber::set_default(subscriber);
    let (tmp, app) = build_auth_test_app();

    let mismatch_cases = [
        (
            AsyncMigrationSourceProvider::Algolia,
            "/1/migrations/algolia/list-indexes",
            serde_json::json!({ "endpoint": "https://x.example.com", "apiKey": sentinel.as_str() }),
        ),
        (
            AsyncMigrationSourceProvider::Meilisearch,
            "/1/migrations/meilisearch/list-indexes",
            serde_json::json!({ "appId": "APPID", "apiKey": sentinel.as_str() }),
        ),
        (
            AsyncMigrationSourceProvider::Typesense,
            "/1/migrations/typesense/list-indexes",
            serde_json::json!({ "appId": "APPID", "apiKey": sentinel.as_str() }),
        ),
    ];
    let covered_providers: Vec<_> = mismatch_cases.iter().map(|case| case.0).collect();
    assert_eq!(
        covered_providers,
        AsyncMigrationSourceProvider::PUBLIC.to_vec(),
        "failure-path leak sweep must cover every public source provider"
    );

    for (provider, path, body) in mismatch_cases {
        let response = post_json(&app, path, Some("admin-key"), body).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{} payload mismatch must be refused before provider dispatch",
            provider.as_str().unwrap()
        );
        let response_bytes = body_bytes(response).await;
        assert_secret_absent("payload-mismatch error body", &response_bytes, &sentinel);
        let body: serde_json::Value =
            serde_json::from_slice(&response_bytes).expect("mismatch body must be JSON");
        assert_eq!(
            body.get("code").and_then(serde_json::Value::as_str),
            Some("source_provider_payload_mismatch")
        );
    }

    for (provider, path, body, expected_message) in [
        (
            AsyncMigrationSourceProvider::Meilisearch,
            "/1/migrations/meilisearch/list-indexes",
            discovery_host_body("endpoint", "https://evil.example.com", sentinel.as_str()),
            "Meilisearch Cloud endpoint is not allowed",
        ),
        (
            AsyncMigrationSourceProvider::Typesense,
            "/1/migrations/typesense/list-indexes",
            discovery_host_body("node", "https://evil.example.com", sentinel.as_str()),
            "Typesense Cloud endpoint is not allowed",
        ),
    ] {
        let response = post_json(&app, path, Some("admin-key"), body).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{} non-vendor host must be refused at admission",
            provider.as_str().unwrap()
        );
        let response_bytes = body_bytes(response).await;
        assert_secret_absent("vendor-policy error body", &response_bytes, &sentinel);
        let body: serde_json::Value =
            serde_json::from_slice(&response_bytes).expect("vendor-policy body must be JSON");
        assert_eq!(
            body,
            serde_json::json!({
                "message": expected_message,
                "status": 400
            })
        );
    }

    assert_secret_absent(
        "captured failure-path logs",
        logs.contents().as_bytes(),
        &sentinel,
    );
    assert_secret_absent(
        "temporary failure-path spool and data files",
        &read_file_tree(tmp.path()),
        &sentinel,
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn list_source_indexes_never_leaks_credentials() {
    // Exercise a successful upstream request for every public provider, then
    // sweep all externally observable and durable sinks. This is
    // deliberately separate from the mismatch test so an early parser refusal
    // cannot make the runtime leak guard vacuous.
    let sentinel = discovery_credential_sentinel();
    let meilisearch_authorization = format!("Bearer {sentinel}");

    let algolia = start_discovery_upstream(
        "/1/indexes",
        &[("page", "0"), ("hitsPerPage", "100")],
        &[
            ("x-algolia-application-id", "APP123"),
            ("x-algolia-api-key", sentinel.as_str()),
        ],
        200,
        serde_json::json!({
            "items": [{
                "name": "algolia_products",
                "entries": 2,
                "updatedAt": "2026-07-26T00:00:00Z",
                "pendingTask": false
            }],
            "page": 0,
            "nbPages": 1
        }),
    )
    .await;
    let meilisearch = start_discovery_upstream(
        "/indexes",
        &[("limit", "1")],
        &[("authorization", meilisearch_authorization.as_str())],
        200,
        serde_json::json!({
            "results": [{
                "uid": "meili_products",
                "primaryKey": "id",
                "createdAt": "2026-07-26T00:00:00Z",
                "updatedAt": "2026-07-26T00:00:00Z"
            }],
            "total": 1,
            "offset": 0,
            "limit": 1
        }),
    )
    .await;
    mount_meilisearch_stats_upstream(
        &meilisearch,
        meilisearch_authorization.as_str(),
        &[("meili_products", 1)],
    )
    .await;
    let typesense = start_discovery_upstream(
        "/collections",
        &[("exclude_fields", "fields"), ("limit", "1")],
        &[("x-typesense-api-key", sentinel.as_str())],
        200,
        typesense_collection_summaries(&[("typesense_products", 1, 1_785_020_400, "price")]),
    )
    .await;

    let logs = SharedLogBuffer::default();
    let (tmp, app) = build_auth_test_app();

    let requests = [
        (
            AsyncMigrationSourceProvider::Algolia,
            "/1/migrations/algolia/list-indexes",
            serde_json::json!({ "appId": "APP123", "apiKey": sentinel.as_str() }),
            vec![ExpectedNeutralDiscoveryMetadata::algolia(
                "algolia_products",
                2,
                "2026-07-26T00:00:00Z",
            )],
        ),
        (
            AsyncMigrationSourceProvider::Meilisearch,
            "/1/migrations/meilisearch/list-indexes?limit=1",
            serde_json::json!({ "endpoint": meilisearch.uri(), "apiKey": sentinel.as_str() }),
            vec![ExpectedNeutralDiscoveryMetadata::meilisearch(
                "meili_products",
                Some("id"),
                1,
                "2026-07-26T00:00:00Z",
                "2026-07-26T00:00:00Z",
            )],
        ),
        (
            AsyncMigrationSourceProvider::Typesense,
            "/1/migrations/typesense/list-indexes?limit=1",
            serde_json::json!({ "node": typesense.uri(), "apiKey": sentinel.as_str() }),
            vec![ExpectedNeutralDiscoveryMetadata::typesense(
                "typesense_products",
                1,
                1_785_020_400,
                "price",
            )],
        ),
    ];
    let covered_providers: Vec<_> = requests.iter().map(|request| request.0).collect();
    assert_eq!(
        covered_providers,
        AsyncMigrationSourceProvider::PUBLIC.to_vec(),
        "leak sweep must cover every public source provider"
    );

    let algolia_uri = algolia.uri();
    let responses =
        with_test_algolia_base_url_override(Some("APP123"), Some(&algolia_uri), async {
            let _env_lock = ENV_MUTEX
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _meili_loopback =
                EnvVarRestoreGuard::set("FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK", "1");
            let _typesense_loopback =
                EnvVarRestoreGuard::set("FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK", "1");
            let subscriber = tracing_subscriber::registry().with(
                tracing_subscriber::fmt::layer()
                    .without_time()
                    .with_ansi(false)
                    .with_writer(logs.clone()),
            );
            let _log_guard = tracing::subscriber::set_default(subscriber);

            let mut responses = Vec::with_capacity(requests.len());
            for (provider, path, request_body, expected_metadata) in requests {
                let response = post_json(&app, path, Some("admin-key"), request_body).await;
                responses.push((provider, response, expected_metadata));
            }
            responses
        })
        .await;

    // All process-global environment overrides are restored and their locks are
    // released before response or mock assertions can panic.
    for (provider, response, expected_metadata) in responses {
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{} valid discovery request must complete",
            provider.as_str().unwrap()
        );
        let response_bytes = body_bytes(response).await;
        assert_secret_absent("valid discovery response body", &response_bytes, &sentinel);
        let response_json: serde_json::Value =
            serde_json::from_slice(&response_bytes).expect("discovery response must be JSON");
        let expected_names = expected_metadata
            .iter()
            .map(|expected| expected.name.to_string())
            .collect::<Vec<_>>();
        assert_eq!(discovery_index_names(response_json.clone()), expected_names);
        assert_neutral_discovery_metadata(&response_json, &expected_metadata);
    }

    algolia.verify().await;
    meilisearch.verify().await;
    typesense.verify().await;
    assert_secret_absent(
        "captured request logs",
        logs.contents().as_bytes(),
        &sentinel,
    );
    assert_secret_absent(
        "temporary migration spool and data files",
        &read_file_tree(tmp.path()),
        &sentinel,
    );

    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (surface, path) in [
        ("migration fixtures", crate_dir.join("../tests/fixtures")),
        (
            "synced Stage 1 work-note contract",
            crate_dir.join("src/router_tests.rs"),
        ),
        ("canonical OpenAPI", crate_dir.join("../docs2/openapi.json")),
        (
            "demo OpenAPI",
            crate_dir.join("../demo-dualclient/public/openapi.json"),
        ),
    ] {
        assert_secret_absent(surface, &read_file_tree(&path), &sentinel);
    }

    // The dev chat is useful additional coverage when present, but Debbie mirrors
    // intentionally do not publish `chats/`. The synced test source above carries
    // the Stage 1 work-note contract in every supported test locality.
    let dev_work_notes =
        crate_dir.join("../../chats/icg/aug02_5am_4_neutral_source_discovery_contract.md");
    if dev_work_notes.is_file() {
        assert_secret_absent(
            "development Stage 1 work notes",
            &read_file_tree(&dev_work_notes),
            &sentinel,
        );
    }

    let served_openapi =
        body_bytes(send_empty_request(&app, Method::GET, "/api-docs/openapi.json").await).await;
    assert_secret_absent("served OpenAPI", &served_openapi, &sentinel);
}

#[tokio::test]
async fn bulk_replace_rejects_unauthenticated_use() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let search_key = search_only_key_value(&key_store);
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    let app = build_auth_router_for_state(&tmp, Arc::clone(&state), key_store);
    let uri = "/1/migrations/bulk-replace?indexName=bulk_replace_auth_target";
    let payload = "{\"objectID\":\"authorized-replacement\"}\n";

    let missing_auth = post_ndjson(&app, uri, None, payload).await;
    assert_invalid_credentials_response(missing_auth).await;
    assert!(!state
        .manager
        .base_path
        .join("bulk_replace_auth_target")
        .exists());

    let non_admin = post_ndjson(&app, uri, Some(&search_key), payload).await;
    assert_method_not_allowed_response(non_admin).await;
    assert!(!state
        .manager
        .base_path
        .join("bulk_replace_auth_target")
        .exists());

    let admin = post_ndjson(&app, uri, Some("admin-key"), payload).await;
    assert_eq!(
        admin.status(),
        StatusCode::ACCEPTED,
        "the admin control request must prove the protected route exists"
    );
}

#[tokio::test]
async fn bulk_replace_rejects_peer_routed_use() {
    let tmp = TempDir::new().unwrap();
    let replication_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let state = TestStateBuilder::new(&tmp)
        .with_analytics()
        .with_replication_manager(peer_configured_replication_manager(replication_dir.path()))
        .build_shared();
    seed_document(
        &state.manager,
        "bulk_replace_peer_target",
        "sentinel",
        "original",
    )
    .await;
    let app = build_auth_router_for_state(&tmp, Arc::clone(&state), key_store);

    let response = post_ndjson(
        &app,
        "/1/migrations/bulk-replace?indexName=bulk_replace_peer_target",
        Some("admin-key"),
        "{\"objectID\":\"replacement\"}\n",
    )
    .await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({
            "message": "Migration is only supported when no replication peers are configured",
            "status": 503,
            "code": "migration_ha_unsupported"
        })
    );
    let sentinel = state
        .manager
        .get_document("bulk_replace_peer_target", "sentinel")
        .unwrap();
    assert!(
        sentinel.is_some(),
        "peer refusal must preserve the original target generation"
    );
    assert!(
        state
            .manager
            .get_document("bulk_replace_peer_target", "replacement")
            .unwrap()
            .is_none(),
        "peer refusal must not publish the request body"
    );
}

#[tokio::test]
async fn bulk_replace_receipt_states_single_node_topology() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let response = post_ndjson(
        &app,
        "/1/migrations/bulk-replace?indexName=bulk_replace_receipt_target",
        Some("admin-key"),
        "{\"objectID\":\"one\",\"title\":\"First\"}\n{\"objectID\":\"two\",\"title\":\"Second\"}\n",
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let receipt = body_json(response).await;
    assert_eq!(receipt["topology"], "single_node_only");
    assert_eq!(receipt["targetIndex"], "bulk_replace_receipt_target");
    assert_eq!(receipt["disposition"], "running");
    assert!(
        receipt["jobID"].as_str().is_some(),
        "receipt must expose the durable job identifier"
    );
    let job_id = receipt["jobID"].as_str().unwrap();
    let terminal = poll_bulk_replace_terminal(&app, job_id).await;
    assert_eq!(terminal["topology"], "single_node_only");
    assert_eq!(terminal["targetIndex"], "bulk_replace_receipt_target");
    assert_eq!(terminal["disposition"], "succeeded");
    assert_eq!(
        terminal["objectsImported"],
        serde_json::json!({"imported": 2})
    );
}

#[tokio::test]
async fn bulk_replace_status_count_matches_three_submitted_documents() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let response = post_ndjson(
        &app,
        "/1/migrations/bulk-replace?indexName=bulk_replace_three_document_count",
        Some("admin-key"),
        concat!(
            "{\"objectID\":\"ordinary\",\"title\":\"Ordinary\"}\n",
            "{\"objectID\":\"zzsentinel_3_0\",\"donorType\":\"sentinel\"}\n",
            "{\"objectID\":\"zzsentinel_3_1\",\"donorType\":\"sentinel\"}\n",
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let receipt = body_json(response).await;
    let terminal = poll_bulk_replace_terminal(&app, receipt["jobID"].as_str().unwrap()).await;
    // 1 ordinary document + 2 sentinel documents = 3 submitted documents.
    assert_eq!(terminal["disposition"], "succeeded");
    assert_eq!(
        terminal["exportProgress"],
        serde_json::json!({"completed": 3, "total": 3})
    );
    assert_eq!(
        terminal["objectsImported"],
        serde_json::json!({"imported": 3})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bulk_replace_streaming_submission_waits_for_body_before_202() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));
    let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(2);
    tx.send(Ok(axum::body::Bytes::from_static(
        b"{\"objectID\":\"streaming-one\",\"title\":\"First\"}\n",
    )))
    .await
    .unwrap();

    let request = Request::builder()
        .method("POST")
        .uri("/1/migrations/bulk-replace?indexName=streaming_bulk_replace")
        .header("content-type", "application/x-ndjson")
        .header("x-algolia-api-key", "admin-key")
        .header("x-algolia-application-id", "route-contract-app")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap();
    let submission = tokio::spawn(app.clone().oneshot(request));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !submission.is_finished(),
        "202 must not close the transport while the request body is still uploading"
    );

    tx.send(Ok(axum::body::Bytes::from_static(
        b"{\"objectID\":\"streaming-two\",\"title\":\"Second\"}\n",
    )))
    .await
    .unwrap();
    drop(tx);

    let response = tokio::time::timeout(std::time::Duration::from_secs(10), submission)
        .await
        .expect("bulk replace must return its durable receipt after spooling the request body")
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let receipt = body_json(response).await;
    assert_eq!(receipt["phase"], "submitted");
    assert_eq!(receipt["disposition"], "running");
    let job_id = receipt["jobID"].as_str().unwrap().to_string();

    let terminal = poll_bulk_replace_terminal(&app, &job_id).await;
    assert_eq!(terminal["disposition"], "succeeded");
    assert_eq!(
        terminal["objectsImported"],
        serde_json::json!({"imported": 2})
    );
}

#[tokio::test]
async fn bulk_replace_rejects_payload_over_configured_cap_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let state = TestStateBuilder::new(&tmp)
        .with_analytics()
        .with_bulk_replace_max_bytes(32)
        .build_shared();
    let app = build_auth_router_for_state(&tmp, Arc::clone(&state), key_store);
    assert_eq!(count_bulk_replace_jobs(&state), 0);

    let response = post_ndjson(
        &app,
        "/1/migrations/bulk-replace?indexName=over_cap_target",
        Some("admin-key"),
        "{\"objectID\":\"too-large\",\"payload\":\"exceeds-cap\"}\n",
    )
    .await;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(!state.manager.base_path.join("over_cap_target").exists());
    assert_eq!(
        count_bulk_replace_jobs(&state),
        0,
        "admission-time rejection must not leave an unreachable durable job behind"
    );
}

async fn poll_bulk_replace_terminal(app: &axum::Router, job_id: &str) -> serde_json::Value {
    let mut last_status = serde_json::Value::Null;
    for _ in 0..200 {
        let response = get_request(
            app,
            &format!("/1/migrations/bulk-replace/{job_id}"),
            Some("admin-key"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let status = body_json(response).await;
        if status["disposition"] != "running" {
            return status;
        }
        last_status = status;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("bulk replacement did not reach a terminal state: {last_status}");
}

fn pause_next_bulk_replace_at_prepublication(
    state: &crate::handlers::AppState,
) -> (Arc<std::sync::Barrier>, Arc<std::sync::Barrier>) {
    let reached = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    state
        .migration_runner
        .set_bulk_replace_prepublication_hook_for_test({
            let reached = Arc::clone(&reached);
            let release = Arc::clone(&release);
            move || {
                reached.wait();
                release.wait();
            }
        });
    (reached, release)
}

fn bulk_replace_staging_path(
    state: &crate::handlers::AppState,
    target_index: &str,
    job_id: &str,
) -> std::path::PathBuf {
    let job_uuid = uuid::Uuid::parse_str(job_id).unwrap();
    let spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    let transaction_id = spool
        .read_async_migration_metadata(job_uuid)
        .unwrap()
        .publication_transaction_id
        .expect("paused build must have saved its publication receipt");
    PublicationPaths::new(
        &state.manager.base_path,
        &PublicationTarget::new(target_index).unwrap(),
        &transaction_id,
    )
    .staging
}

fn count_bulk_replace_jobs(state: &crate::handlers::AppState) -> usize {
    let spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    let jobs_root = spool
        .job_dir(uuid::Uuid::nil())
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::read_dir(jobs_root).unwrap().count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_build_removes_only_its_own_staging_generation_after_saving_its_receipt() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let state = TestStateBuilder::new(&tmp)
        .with_analytics()
        .with_migration_capacity(2)
        .build_shared();
    let app = build_auth_router_for_state(&tmp, Arc::clone(&state), key_store);
    let payload = "{\"objectID\":\"doc-1\",\"ordinal\":1}\n";

    let (cancelled_reached, cancelled_release) = pause_next_bulk_replace_at_prepublication(&state);
    let cancelled_submit = post_ndjson(
        &app,
        "/1/migrations/bulk-replace?indexName=cancelled_bulk_replace",
        Some("admin-key"),
        payload,
    )
    .await;
    assert_eq!(cancelled_submit.status(), StatusCode::ACCEPTED);
    let cancelled_receipt = body_json(cancelled_submit).await;
    let cancelled_job = cancelled_receipt["jobID"].as_str().unwrap();
    cancelled_reached.wait();
    let cancelled_staging =
        bulk_replace_staging_path(&state, "cancelled_bulk_replace", cancelled_job);

    let (surviving_reached, surviving_release) = pause_next_bulk_replace_at_prepublication(&state);
    let surviving_submit = post_ndjson(
        &app,
        "/1/migrations/bulk-replace?indexName=surviving_bulk_replace",
        Some("admin-key"),
        payload,
    )
    .await;
    assert_eq!(surviving_submit.status(), StatusCode::ACCEPTED);
    let surviving_receipt = body_json(surviving_submit).await;
    let surviving_job = surviving_receipt["jobID"].as_str().unwrap();
    surviving_reached.wait();
    let surviving_staging =
        bulk_replace_staging_path(&state, "surviving_bulk_replace", surviving_job);
    assert!(cancelled_staging.exists());
    assert!(surviving_staging.exists());

    let cancel = post_json(
        &app,
        &format!("/1/migrations/bulk-replace/{cancelled_job}/cancel"),
        Some("admin-key"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::ACCEPTED);
    cancelled_release.wait();

    let cancelled_status = poll_bulk_replace_terminal(&app, cancelled_job).await;
    assert_eq!(cancelled_status["disposition"], "cancelled");
    let cancelled_staging_was_removed = !cancelled_staging.exists();
    let surviving_staging_was_preserved = surviving_staging.exists();

    surviving_release.wait();
    let surviving_status = poll_bulk_replace_terminal(&app, surviving_job).await;
    assert_eq!(surviving_status["disposition"], "succeeded");

    assert!(
        cancelled_staging_was_removed,
        "cancelled build must remove its job-owned staging generation"
    );
    assert!(
        surviving_staging_was_preserved,
        "cancelling one build must preserve another build's staging generation"
    );
}

#[tokio::test]
async fn privacy_scrub_router_is_private_and_not_publicly_reachable() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let private_migration_key = create_test_key_with_acl(&key_store, "privateMigration");
    let search_key = search_only_key_value(&key_store);
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    state
        .manager
        .create_tenant("route_contract_tenant")
        .unwrap();
    write_committed_generation_evidence(
        &state.manager.base_path,
        "route_contract_tenant",
        "generation-route-contract",
    );
    let app = build_auth_router_for_state(&tmp, Arc::clone(&state), Arc::clone(&key_store));
    let no_auth_app = build_no_auth_router_for_state(&tmp, state);
    let scrub_body = serde_json::json!({
        "scrubId": "privacy-scrub-router-stable-id",
        "tenant": "route_contract_tenant",
        "objectIDs": ["doc-private"],
        "expectedGeneration": "generation-route-contract"
    });

    let health_probe = send_empty_request(&app, Method::GET, "/health").await;
    assert_eq!(
        health_probe.status(),
        StatusCode::OK,
        "public health remains the only no-credential surface in this probe"
    );

    let no_auth_probe = post_json(
        &no_auth_app,
        "/1/migrations/privacy-scrub",
        None,
        scrub_body.clone(),
    )
    .await;
    assert_eq!(
        no_auth_probe.status(),
        StatusCode::NOT_FOUND,
        "privacy scrub must not be exposed by the no-auth/public router"
    );

    let search_key_probe = post_json(
        &app,
        "/1/migrations/privacy-scrub",
        Some(&search_key),
        scrub_body.clone(),
    )
    .await;
    assert_method_not_allowed_response(search_key_probe).await;

    let ordinary_admin_probe = post_json(
        &app,
        "/1/migrations/privacy-scrub",
        Some("admin-key"),
        scrub_body.clone(),
    )
    .await;
    assert_method_not_allowed_response(ordinary_admin_probe).await;

    let private_probe = post_json(
        &app,
        "/1/migrations/privacy-scrub",
        Some(&private_migration_key),
        scrub_body,
    )
    .await;
    assert_eq!(
        private_probe.status(),
        StatusCode::ACCEPTED,
        "authenticated private migration command should be routed to the scrub handler"
    );
    let ack = body_json(private_probe).await;
    assert_eq!(ack["scrubId"], "privacy-scrub-router-stable-id");
    assert_eq!(ack["disposition"], "acknowledged");
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
