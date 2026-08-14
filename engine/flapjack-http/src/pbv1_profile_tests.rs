use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::api_profile::{
    prepare_paid_beta_v1_batch, ApiProfile, ApiProfileConfigError, FLAPJACK_API_PROFILE_ENV,
    PAID_BETA_V1_DIRECT_SEARCH_PATH, PAID_BETA_V1_SEARCH_PARAMS,
};
use crate::auth::{ApiKey, KeyStore};
use crate::middleware::TrustedProxyMatcher;
use crate::router::{build_router, RouterConfig};
use crate::startup::CorsMode;
use crate::test_helpers::{body_json, TestStateBuilder};

const ADMIN_KEY: &str = "pbv1-admin-key";
const INDEX_NAME: &str = "tenant_123_products";
const DIRECT_SEARCH_PATH: &str = "/1/indexes/*/queries";
const PEER_KEY: &str = "pbv1-replication-peer-key";

struct PbV1Fixture {
    _tmp: TempDir,
    app: axum::Router,
    key_store: Arc<KeyStore>,
    search_key: String,
}

fn api_key(acls: &[&str], indexes: &[&str], validity: i64) -> ApiKey {
    ApiKey {
        hash: String::new(),
        salt: String::new(),
        hmac_key: None,
        created_at: 0,
        acl: acls.iter().map(|acl| (*acl).to_string()).collect(),
        description: "PBV1 contract fixture".to_string(),
        indexes: indexes
            .iter()
            .map(|index_name| (*index_name).to_string())
            .collect(),
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: vec![],
        restrict_sources: None,
        validity,
    }
}

fn build_profile_router(
    tmp: &TempDir,
    key_store: Arc<KeyStore>,
    profile: ApiProfile,
) -> axum::Router {
    build_profile_router_with_replication(tmp, key_store, profile, None)
}

fn build_profile_router_with_replication(
    tmp: &TempDir,
    key_store: Arc<KeyStore>,
    profile: ApiProfile,
    replication_api_key: Option<String>,
) -> axum::Router {
    let state = TestStateBuilder::new(tmp).with_analytics().build_shared();
    state.manager.create_tenant(INDEX_NAME).unwrap();
    let analytics_config = AnalyticsConfig {
        enabled: false,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 60,
        flush_size: 1000,
        retention_days: 30,
    };
    build_router(
        state,
        Some(key_store),
        AnalyticsCollector::new(analytics_config),
        Arc::new(TrustedProxyMatcher::from_optional_csv(None).unwrap()),
        tmp.path(),
        RouterConfig {
            cors_mode: CorsMode::LoopbackOnly,
            disable_dashboard: true,
            replication_api_key,
            api_profile: profile,
        },
    )
}

fn pbv1_fixture() -> PbV1Fixture {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), ADMIN_KEY));
    let (_, search_key) = key_store.create_key(api_key(&["search", "browse"], &[INDEX_NAME], 0));
    let app = build_profile_router(&tmp, Arc::clone(&key_store), ApiProfile::PaidBetaV1);
    PbV1Fixture {
        _tmp: tmp,
        app,
        key_store,
        search_key,
    }
}

fn direct_request(
    method: Method,
    path: &str,
    api_key: &str,
    body: serde_json::Value,
) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("x-algolia-application-id", "flapjack")
        .header("x-algolia-api-key", api_key)
        .header("authorization", format!("Bearer {api_key}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn admin_request(method: Method, path: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("x-algolia-application-id", "flapjack")
        .header("x-algolia-api-key", ADMIN_KEY)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn valid_batch() -> serde_json::Value {
    serde_json::json!({
        "requests": [
            {
                "indexName": INDEX_NAME,
                "params": {"query": "ridge", "page": 0, "hitsPerPage": 20}
            },
            {
                "indexName": INDEX_NAME,
                "params": {"query": "ridge", "page": 1, "hitsPerPage": 2}
            }
        ]
    })
}

fn all_allowed_params_batch() -> serde_json::Value {
    serde_json::json!({
        "requests": [
            {
                "indexName": INDEX_NAME,
                "params": {
                    "query": "ridge",
                    "page": 0,
                    "hitsPerPage": 20,
                    "facets": ["color"],
                    "facetFilters": [["color:blue"]],
                    "filters": "published = true"
                }
            },
            {
                "indexName": INDEX_NAME,
                "params": {"query": "ridge", "page": 1, "hitsPerPage": 2}
            }
        ]
    })
}

async fn assert_error(
    response: axum::response::Response,
    status: StatusCode,
    message: &str,
    boundary: &str,
) {
    assert_eq!(response.status(), status, "wrong status for {boundary}");
    assert_eq!(
        body_json(response).await,
        serde_json::json!({"message": message, "status": status.as_u16()}),
        "wrong error envelope for {boundary}"
    );
}

#[test]
fn pbv1_profile_parser_is_explicit_and_fail_closed() {
    assert_eq!(PAID_BETA_V1_DIRECT_SEARCH_PATH, DIRECT_SEARCH_PATH);
    assert_eq!(
        PAID_BETA_V1_SEARCH_PARAMS,
        [
            "query",
            "page",
            "hitsPerPage",
            "facets",
            "facetFilters",
            "filters"
        ]
    );
    assert_eq!(
        ApiProfile::from_optional_value(None).unwrap(),
        ApiProfile::Full
    );
    assert_eq!(
        ApiProfile::from_optional_value(Some("full")).unwrap(),
        ApiProfile::Full
    );
    assert_eq!(
        ApiProfile::from_optional_value(Some("paid_beta_v1")).unwrap(),
        ApiProfile::PaidBetaV1
    );

    for invalid in ["", " ", "pbv1", "paid-beta-v1", "unknown"] {
        assert_eq!(
            ApiProfile::from_optional_value(Some(invalid)),
            Err(ApiProfileConfigError::UnknownValue(invalid.to_string())),
            "{FLAPJACK_API_PROFILE_ENV}={invalid:?} must fail startup"
        );
    }
    assert_eq!(
        ApiProfile::PaidBetaV1.validate_auth_enabled(false),
        Err(ApiProfileConfigError::AuthenticationRequired)
    );
    assert_eq!(ApiProfile::Full.validate_auth_enabled(false), Ok(()));
}

#[tokio::test]
async fn pbv1_health_reports_the_active_runtime_profile() {
    let fixture = pbv1_fixture();
    let response = fixture
        .app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["build"]["apiProfile"], "paid_beta_v1");
    assert_eq!(
        json["build"]["supportedApiProfiles"],
        serde_json::json!(["full", "paid_beta_v1"])
    );
}

#[tokio::test]
async fn pbv1_search_key_route_inventory_is_exact() {
    let fixture = pbv1_fixture();
    let allowed = fixture
        .app
        .clone()
        .oneshot(direct_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            &fixture.search_key,
            valid_batch(),
        ))
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);

    let wrong_method = fixture
        .app
        .clone()
        .oneshot(direct_request(
            Method::GET,
            DIRECT_SEARCH_PATH,
            &fixture.search_key,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);

    // This denominator spans every currently mounted customer-facing family and
    // the explicitly excluded PBV1 families. A route accidentally made visible
    // to a search key changes one of these exact 404s.
    let denied = [
        (Method::GET, "/1/indexes"),
        (Method::POST, "/1/indexes"),
        (Method::POST, "/1/indexes/tenant_123_products/query"),
        (Method::POST, "/1/indexes/tenant_123_products/browse"),
        (Method::POST, "/1/indexes/tenant_123_products/batch"),
        (Method::GET, "/1/indexes/tenant_123_products/settings"),
        (Method::GET, "/1/keys"),
        (Method::POST, "/1/insights"),
        (Method::POST, "/1/indexes/*/recommendations"),
        (Method::POST, "/1/personalization"),
        (Method::GET, "/2/abtests"),
        (Method::GET, "/dashboard/"),
        (Method::GET, "/swagger-ui/"),
        (Method::GET, "/api-docs/openapi.json"),
        (Method::GET, "/internal/status"),
        (Method::POST, "/1/events"),
        (Method::POST, "/1/migrate-from-algolia"),
        (Method::POST, "/1/indexes/tenant_123_products/restore"),
        (Method::GET, "/metrics"),
    ];
    for (method, path) in denied {
        let response = fixture
            .app
            .clone()
            .oneshot(direct_request(
                method,
                path,
                &fixture.search_key,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "leaked {path}");
    }

    let invalid_key_on_unpublished_route = fixture
        .app
        .clone()
        .oneshot(direct_request(
            Method::POST,
            &format!("/1/indexes/{INDEX_NAME}/query"),
            "not-a-real-key",
            serde_json::json!({"query": "ridge"}),
        ))
        .await
        .unwrap();
    assert_eq!(
        invalid_key_on_unpublished_route.status(),
        StatusCode::NOT_FOUND,
        "unpublished routes must not reveal whether a customer key is valid"
    );
}

#[tokio::test]
async fn pbv1_replication_peer_route_remains_operational() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), ADMIN_KEY));
    let app = build_profile_router_with_replication(
        &tmp,
        key_store,
        ApiProfile::PaidBetaV1,
        Some(PEER_KEY.to_string()),
    );
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/internal/status")
                .header(
                    "x-algolia-application-id",
                    flapjack_replication::peer::REPLICATION_PEER_APPLICATION_ID,
                )
                .header("x-algolia-api-key", PEER_KEY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn pbv1_allowed_parameter_inventory_normalizes_for_the_legacy_search_core() {
    let key = api_key(&["search", "browse"], &[INDEX_NAME], 0);
    let batch = prepare_paid_beta_v1_batch(all_allowed_params_batch(), Some(&key)).unwrap();
    assert_eq!(batch.requests.len(), 2);
    let first = &batch.requests[0];
    assert_eq!(first.index_name.as_deref(), Some(INDEX_NAME));
    assert_eq!(first.query, "ridge");
    assert_eq!(first.page, 0);
    assert_eq!(first.hits_per_page, Some(20));
    assert_eq!(
        first.facets.as_deref(),
        Some(["color".to_string()].as_slice())
    );
    assert_eq!(
        first.facet_filters,
        Some(serde_json::json!([["color:blue"]]))
    );
    assert_eq!(first.filters.as_deref(), Some("published = true"));
}

#[tokio::test]
async fn pbv1_admin_control_plane_routes_remain_operational() {
    let fixture = pbv1_fixture();
    let create = fixture
        .app
        .clone()
        .oneshot(admin_request(
            Method::POST,
            "/1/indexes",
            serde_json::json!({"uid": "admin_managed_index"}),
        ))
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::OK);

    let routes = [
        (
            Method::POST,
            "/1/indexes/admin_managed_index/batch",
            serde_json::json!({"requests": [{"action": "addObject", "body": {"objectID": "1", "title": "ridge"}}]}),
        ),
        (
            Method::POST,
            "/1/indexes/admin_managed_index/settings",
            serde_json::json!({"searchableAttributes": ["title"]}),
        ),
        (
            Method::PUT,
            "/1/indexes/admin_managed_index/synonyms/ridge",
            serde_json::json!({"objectID": "ridge", "type": "synonym", "synonyms": ["ridge", "crest"]}),
        ),
        (
            Method::PUT,
            "/1/indexes/admin_managed_index/rules/ridge-rule",
            serde_json::json!({"objectID": "ridge-rule", "conditions": [], "consequence": {}}),
        ),
        (
            Method::POST,
            "/1/dictionaries/stopwords/batch",
            serde_json::json!({"requests": []}),
        ),
        (
            Method::POST,
            "/1/keys",
            serde_json::json!({"acl": ["search", "browse"], "indexes": ["admin_managed_index"]}),
        ),
    ];
    for (method, path, body) in routes {
        let response = fixture
            .app
            .clone()
            .oneshot(admin_request(method, path, body))
            .await
            .unwrap();
        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "PBV1 profile hid required admin route {path}"
        );
    }
}

#[tokio::test]
async fn pbv1_batch_body_and_parameter_contract_is_closed() {
    let fixture = pbv1_fixture();
    let invalid_bodies = [
        serde_json::json!({}),
        serde_json::json!({"requests": [], "extra": true}),
        serde_json::json!({"requests": []}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME}]}),
        serde_json::json!({"requests": [{"params": {}}]}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME, "params": {}, "extra": true}]}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME, "params": {"query": 1}}]}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME, "params": {"page": -1}}]}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME, "params": {"page": 1.5}}]}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME, "params": {"hitsPerPage": 0}}]}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME, "params": {"facets": ["color", 1]}}]}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME, "params": {"facetFilters": "color:blue"}}]}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME, "params": {"filters": ["published"]}}]}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME, "params": {"attributesToHighlight": ["title"]}}]}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME, "params": {"analytics": false}}]}),
        serde_json::json!({"requests": [{"indexName": INDEX_NAME, "params": {"clickAnalytics": false}}]}),
        serde_json::json!({"requests": [
            {"indexName": INDEX_NAME, "params": {}},
            {"indexName": "tenant_123_other", "params": {}}
        ]}),
    ];
    for body in invalid_bodies {
        let response = fixture
            .app
            .clone()
            .oneshot(direct_request(
                Method::POST,
                DIRECT_SEARCH_PATH,
                &fixture.search_key,
                body.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "body unexpectedly dispatched: {body}"
        );
    }
}

#[tokio::test]
async fn pbv1_key_transport_identity_acl_and_index_scope_fail_closed() {
    let fixture = pbv1_fixture();

    let missing = direct_request(Method::POST, DIRECT_SEARCH_PATH, "", valid_batch());
    let response = fixture.app.clone().oneshot(missing).await.unwrap();
    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "Invalid Application-ID or API key",
        "missing Bearer key",
    )
    .await;

    let invalid = fixture
        .app
        .clone()
        .oneshot(direct_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            "malformed-key",
            valid_batch(),
        ))
        .await
        .unwrap();
    assert_error(
        invalid,
        StatusCode::FORBIDDEN,
        "Invalid Application-ID or API key",
        "malformed Bearer key",
    )
    .await;

    let (_, browse_only) = fixture
        .key_store
        .create_key(api_key(&["browse"], &[INDEX_NAME], 0));
    let under_scoped = fixture
        .app
        .clone()
        .oneshot(direct_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            &browse_only,
            valid_batch(),
        ))
        .await
        .unwrap();
    assert_error(
        under_scoped,
        StatusCode::FORBIDDEN,
        "Method not allowed with this API key",
        "authenticated key missing search ACL",
    )
    .await;

    let (_, search_only) = fixture
        .key_store
        .create_key(api_key(&["search"], &[INDEX_NAME], 0));
    let wrong_scope = fixture
        .app
        .clone()
        .oneshot(direct_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            &search_only,
            valid_batch(),
        ))
        .await
        .unwrap();
    assert_error(
        wrong_scope,
        StatusCode::FORBIDDEN,
        "Invalid Application-ID or API key",
        "authenticated key missing exact search+browse ACL scope",
    )
    .await;

    let (_, expired_key) =
        fixture
            .key_store
            .create_key(api_key(&["search", "browse"], &[INDEX_NAME], 1));
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let expired = fixture
        .app
        .clone()
        .oneshot(direct_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            &expired_key,
            valid_batch(),
        ))
        .await
        .unwrap();
    assert_error(
        expired,
        StatusCode::FORBIDDEN,
        "Invalid Application-ID or API key",
        "expired key",
    )
    .await;

    let (_, revoked_key) =
        fixture
            .key_store
            .create_key(api_key(&["search", "browse"], &[INDEX_NAME], 0));
    assert!(fixture.key_store.delete_key(&revoked_key));
    let revoked = fixture
        .app
        .clone()
        .oneshot(direct_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            &revoked_key,
            valid_batch(),
        ))
        .await
        .unwrap();
    assert_error(
        revoked,
        StatusCode::FORBIDDEN,
        "Invalid Application-ID or API key",
        "revoked key",
    )
    .await;

    let wrong_index_body = serde_json::json!({
        "requests": [{"indexName": "tenant_999_products", "params": {"query": "ridge"}}]
    });
    let wrong_index = fixture
        .app
        .clone()
        .oneshot(direct_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            &fixture.search_key,
            wrong_index_body,
        ))
        .await
        .unwrap();
    assert_error(
        wrong_index,
        StatusCode::FORBIDDEN,
        "Invalid Application-ID or API key",
        "wrong physical index",
    )
    .await;

    let wrong_app = Request::builder()
        .method(Method::POST)
        .uri(DIRECT_SEARCH_PATH)
        .header("content-type", "application/json")
        .header("x-algolia-application-id", "not-flapjack")
        .header("x-algolia-api-key", &fixture.search_key)
        .header("authorization", format!("Bearer {}", fixture.search_key))
        .body(Body::from(valid_batch().to_string()))
        .unwrap();
    let response = fixture.app.clone().oneshot(wrong_app).await.unwrap();
    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "Invalid Application-ID or API key",
        "wrong application ID",
    )
    .await;

    let header_only = Request::builder()
        .method(Method::POST)
        .uri(DIRECT_SEARCH_PATH)
        .header("content-type", "application/json")
        .header("x-algolia-application-id", "flapjack")
        .header("x-algolia-api-key", &fixture.search_key)
        .body(Body::from(valid_batch().to_string()))
        .unwrap();
    let response = fixture.app.clone().oneshot(header_only).await.unwrap();
    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "Invalid Application-ID or API key",
        "legacy x-algolia-api-key transport",
    )
    .await;
}

#[tokio::test]
async fn full_profile_retains_existing_search_key_routes_and_transport() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), ADMIN_KEY));
    let (_, search_key) = key_store.create_key(api_key(&["search"], &[INDEX_NAME], 0));
    let app = build_profile_router(&tmp, key_store, ApiProfile::Full);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/1/indexes/{INDEX_NAME}/query"))
                .header("content-type", "application/json")
                .header("x-algolia-application-id", "existing-client")
                .header("x-algolia-api-key", search_key)
                .body(Body::from(r#"{"query":"ridge"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
