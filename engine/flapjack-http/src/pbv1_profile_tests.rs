use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig};
use tempfile::TempDir;
use tower::ServiceExt;
use tracing_subscriber::prelude::*;

use crate::api_profile::{
    prepare_paid_beta_v1_batch, prepare_paid_beta_v3_batch, ApiProfile, ApiProfileConfigError,
    FLAPJACK_API_PROFILE_ENV, PAID_BETA_V1_DIRECT_SEARCH_PATH, PAID_BETA_V1_SEARCH_PARAMS,
    PAID_BETA_V3_EVENTS_PATH, PAID_BETA_V3_SEARCH_PARAMS,
};
use crate::auth::{ApiKey, KeyStore};
use crate::middleware::TrustedProxyMatcher;
use crate::router::{build_router, RouterConfig};
use crate::startup::CorsMode;
use crate::test_helpers::{body_json, SharedLogBuffer, TestStateBuilder};

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

fn pbv3_fixture() -> PbV1Fixture {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), ADMIN_KEY));
    let (_, search_key) = key_store.create_key(api_key(&["search", "browse"], &[INDEX_NAME], 0));
    let app = build_profile_router(&tmp, Arc::clone(&key_store), ApiProfile::PaidBetaV3);
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

fn managed_search_request(
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
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn managed_search_query_request(
    method: Method,
    path: &str,
    api_key: &str,
    body: serde_json::Value,
) -> Request<Body> {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("x-algolia-application-id", "flapjack")
        .append_pair("x-algolia-api-key", api_key)
        .finish();
    Request::builder()
        .method(method)
        .uri(format!("{path}?{query}"))
        .header("content-type", "application/json")
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

const USER_TOKEN: &str = "3f25cf54-46f6-4f67-9ac8-87c4a34c86f1";

fn pbv3_allowed_params_batch() -> serde_json::Value {
    serde_json::json!({
        "requests": [
            {
                "indexName": INDEX_NAME,
                "query": "ridge",
                "page": 0,
                "hitsPerPage": 20,
                "facets": ["color"],
                "facetFilters": [["color:blue"]],
                "filters": "published = true",
                "analytics": false,
                "clickAnalytics": true,
                "userToken": USER_TOKEN,
                "highlightPreTag": "__ais-highlight__",
                "highlightPostTag": "__/ais-highlight__",
                "maxValuesPerFacet": 20
            }
        ]
    })
}

fn pbv3_query_batch() -> serde_json::Value {
    serde_json::json!({
        "requests": [{
            "indexName": INDEX_NAME,
            "query": "ridge",
            "page": 0,
            "hitsPerPage": 20,
            "clickAnalytics": true,
            "userToken": USER_TOKEN
        }]
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

async fn response_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
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

#[test]
fn pbv3_profile_parser_and_parameter_inventory_are_explicit() {
    assert_eq!(PAID_BETA_V3_EVENTS_PATH, "/1/events");
    assert_eq!(
        PAID_BETA_V3_SEARCH_PARAMS,
        [
            "query",
            "page",
            "hitsPerPage",
            "facets",
            "facetFilters",
            "filters",
            "analytics",
            "clickAnalytics",
            "userToken",
            "highlightPreTag",
            "highlightPostTag",
            "maxValuesPerFacet"
        ]
    );
    assert_eq!(
        ApiProfile::from_optional_value(Some("paid_beta_v3")).unwrap(),
        ApiProfile::PaidBetaV3
    );
    assert_eq!(ApiProfile::PaidBetaV3.as_str(), "paid_beta_v3");
    assert_eq!(
        ApiProfile::PaidBetaV3.validate_auth_enabled(false),
        Err(ApiProfileConfigError::AuthenticationRequired)
    );
}

#[test]
fn pbv3_allowed_parameters_preserve_insights_identity() {
    let key = api_key(&["search", "browse"], &[INDEX_NAME], 0);
    let batch = prepare_paid_beta_v3_batch(pbv3_allowed_params_batch(), Some(&key)).unwrap();
    assert_eq!(batch.requests.len(), 1);
    let request = &batch.requests[0];
    assert_eq!(request.index_name.as_deref(), Some(INDEX_NAME));
    assert_eq!(request.analytics, Some(false));
    assert_eq!(request.click_analytics, Some(true));
    assert_eq!(request.user_token.as_deref(), Some(USER_TOKEN));
    assert_eq!(
        request.highlight_pre_tag.as_deref(),
        Some("__ais-highlight__")
    );
    assert_eq!(
        request.highlight_post_tag.as_deref(),
        Some("__/ais-highlight__")
    );
    assert_eq!(request.max_values_per_facet, Some(20));

    let derived_facet = prepare_paid_beta_v3_batch(
        serde_json::json!({
            "requests": [{
                "indexName": INDEX_NAME,
                "hitsPerPage": 0,
                "page": 0,
                "facets": "brand",
                "analytics": false,
                "clickAnalytics": false,
                "userToken": USER_TOKEN,
                "highlightPreTag": "__ais-highlight__",
                "highlightPostTag": "__/ais-highlight__",
                "maxValuesPerFacet": 10
            }]
        }),
        Some(&key),
    )
    .unwrap();
    assert_eq!(derived_facet.requests[0].hits_per_page, Some(0));
    assert_eq!(
        derived_facet.requests[0].facets.as_deref(),
        Some(["brand".to_string()].as_slice())
    );
}

#[tokio::test]
async fn pbv3_accepts_standard_header_and_native_query_credentials() {
    let fixture = pbv3_fixture();
    let response = fixture
        .app
        .clone()
        .oneshot(managed_search_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            &fixture.search_key,
            pbv3_query_batch(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let native_query_response = fixture
        .app
        .clone()
        .oneshot(managed_search_query_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            &fixture.search_key,
            pbv3_query_batch(),
        ))
        .await
        .unwrap();
    assert_eq!(native_query_response.status(), StatusCode::OK);

    let bearer_cannot_override_bad_standard_header = Request::builder()
        .method(Method::POST)
        .uri(DIRECT_SEARCH_PATH)
        .header("content-type", "application/json")
        .header("x-algolia-application-id", "flapjack")
        .header("x-algolia-api-key", "wrong-standard-key")
        .header("authorization", format!("Bearer {}", fixture.search_key))
        .body(Body::from(pbv3_query_batch().to_string()))
        .unwrap();
    let response = fixture
        .app
        .oneshot(bearer_cannot_override_bad_standard_header)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn pbv3_duplicate_and_mixed_credentials_require_exact_agreement() {
    let fixture = pbv3_fixture();
    let encoded_key: String =
        url::form_urlencoded::byte_serialize(fixture.search_key.as_bytes()).collect();

    let matching_uri = format!(
        "{DIRECT_SEARCH_PATH}?x-algolia-application-id=flapjack&x-algolia-application-id=flapjack&x-algolia-api-key={encoded_key}&x-algolia-api-key={encoded_key}"
    );
    let mut matching = Request::builder()
        .method(Method::POST)
        .uri(matching_uri)
        .header("content-type", "application/json")
        .body(Body::from(pbv3_query_batch().to_string()))
        .unwrap();
    matching
        .headers_mut()
        .append("x-algolia-application-id", "flapjack".parse().unwrap());
    matching
        .headers_mut()
        .append("x-algolia-application-id", "flapjack".parse().unwrap());
    matching
        .headers_mut()
        .append("x-algolia-api-key", fixture.search_key.parse().unwrap());
    matching
        .headers_mut()
        .append("x-algolia-api-key", fixture.search_key.parse().unwrap());
    assert_eq!(
        fixture
            .app
            .clone()
            .oneshot(matching)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let mismatches = [
        format!("{DIRECT_SEARCH_PATH}?x-algolia-application-id=other&x-algolia-api-key={encoded_key}"),
        format!("{DIRECT_SEARCH_PATH}?x-algolia-application-id=flapjack&x-algolia-api-key=other"),
        format!("{DIRECT_SEARCH_PATH}?x-algolia-application-id=flapjack&x-algolia-api-key={encoded_key}&x-algolia-api-key=other"),
        format!("{DIRECT_SEARCH_PATH}?x-algolia-application-id=flapjack&x-algolia-application-id=other&x-algolia-api-key={encoded_key}"),
    ];
    for uri in mismatches {
        let request = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("content-type", "application/json")
            .header("x-algolia-application-id", "flapjack")
            .header("x-algolia-api-key", &fixture.search_key)
            .body(Body::from(pbv3_query_batch().to_string()))
            .unwrap();
        let response = fixture.app.clone().oneshot(request).await.unwrap();
        assert_error(
            response,
            StatusCode::FORBIDDEN,
            "Invalid Application-ID or API key",
            "conflicting credential sources",
        )
        .await;
    }

    let mut duplicate_header_disagreement = managed_search_request(
        Method::POST,
        DIRECT_SEARCH_PATH,
        &fixture.search_key,
        pbv3_query_batch(),
    );
    duplicate_header_disagreement
        .headers_mut()
        .append("x-algolia-api-key", "different".parse().unwrap());
    let response = fixture
        .app
        .oneshot(duplicate_header_disagreement)
        .await
        .unwrap();
    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "Invalid Application-ID or API key",
        "duplicate header credential disagreement",
    )
    .await;
}

#[tokio::test]
async fn query_credentials_do_not_unlock_privileged_routes() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), ADMIN_KEY));
    let app = build_profile_router(&tmp, key_store, ApiProfile::Full);
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("x-algolia-application-id", "flapjack")
        .append_pair("x-algolia-api-key", ADMIN_KEY)
        .finish();
    let query_only = Request::builder()
        .method(Method::POST)
        .uri(format!("/1/indexes?{query}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"uid":"query_must_not_admin"}"#))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(query_only).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );

    let mixed = Request::builder()
        .method(Method::POST)
        .uri(format!("/1/indexes?{query}"))
        .header("content-type", "application/json")
        .header("x-algolia-application-id", "flapjack")
        .header("x-algolia-api-key", ADMIN_KEY)
        .body(Body::from(r#"{"uid":"mixed_must_not_admin"}"#))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(mixed).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );

    let header_only = admin_request(
        Method::POST,
        "/1/indexes",
        serde_json::json!({"uid": "header_admin_still_works"}),
    );
    assert_eq!(
        app.oneshot(header_only).await.unwrap().status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn pbv3_query_key_material_is_absent_from_errors_logs_traces_and_metrics() {
    const SENTINEL: &str = "PBV3_CREDENTIAL_MUST_NEVER_APPEAR";
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), SENTINEL));
    let app = build_profile_router(&tmp, key_store, ApiProfile::PaidBetaV3);
    let logs = SharedLogBuffer::default();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .without_time()
            .with_writer(logs.clone()),
    );
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    let conflicting_query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("x-algolia-application-id", "flapjack")
        .append_pair("x-algolia-api-key", SENTINEL)
        .append_pair("x-algolia-api-key", "different")
        .finish();
    let rejected = Request::builder()
        .method(Method::POST)
        .uri(format!("{DIRECT_SEARCH_PATH}?{conflicting_query}"))
        .header("content-type", "application/json")
        .body(Body::from(pbv3_query_batch().to_string()))
        .unwrap();
    let rejected = app.clone().oneshot(rejected).await.unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    let error_body = response_text(rejected).await;
    assert!(
        !error_body.contains(SENTINEL),
        "error response exposed credential material"
    );

    let metrics = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/metrics")
                .header("x-algolia-application-id", "flapjack")
                .header("x-algolia-api-key", SENTINEL)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(metrics.status(), StatusCode::OK);
    let metrics_body = response_text(metrics).await;
    assert!(
        !metrics_body.contains(SENTINEL),
        "metrics output exposed credential material"
    );
    assert!(
        !logs.contents().contains(SENTINEL),
        "application-owned logs or tracing spans exposed credential material"
    );
}

#[tokio::test]
async fn pbv3_customer_route_inventory_is_exact() {
    let fixture = pbv3_fixture();
    let search = fixture
        .app
        .clone()
        .oneshot(managed_search_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            &fixture.search_key,
            pbv3_query_batch(),
        ))
        .await
        .unwrap();
    assert_eq!(search.status(), StatusCode::OK);

    let events = fixture
        .app
        .clone()
        .oneshot(managed_search_query_request(
            Method::POST,
            PAID_BETA_V3_EVENTS_PATH,
            &fixture.search_key,
            serde_json::json!({"events": []}),
        ))
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);

    let cross_tenant_event = fixture
        .app
        .clone()
        .oneshot(managed_search_query_request(
            Method::POST,
            PAID_BETA_V3_EVENTS_PATH,
            &fixture.search_key,
            serde_json::json!({"events": [{
                "eventType": "click",
                "eventName": "cross tenant",
                "index": "tenant_999_products",
                "userToken": USER_TOKEN,
                "objectIDs": ["object-1"]
            }]}),
        ))
        .await
        .unwrap();
    assert_eq!(cross_tenant_event.status(), StatusCode::FORBIDDEN);

    for (method, path) in [
        (Method::GET, DIRECT_SEARCH_PATH),
        (Method::GET, PAID_BETA_V3_EVENTS_PATH),
        (Method::GET, "/1/indexes"),
        (Method::POST, "/1/indexes"),
        (Method::POST, "/1/indexes/tenant_123_products/query"),
        (Method::POST, "/1/indexes/tenant_123_products/browse"),
        (Method::POST, "/1/indexes/tenant_123_products/batch"),
        (Method::GET, "/1/indexes/tenant_123_products/settings"),
        (Method::GET, "/1/keys"),
        (Method::GET, "/1/events/debug"),
        (Method::DELETE, "/1/usertokens/token"),
        (
            Method::DELETE,
            "/1/indexes/tenant_123_products/usertokens/token",
        ),
        (Method::POST, "/1/indexes/*/recommendations"),
        (Method::GET, "/2/abtests"),
        (Method::GET, "/dashboard/"),
        (Method::GET, "/swagger-ui/"),
        (Method::GET, "/api-docs/openapi.json"),
        (Method::GET, "/internal/status"),
        (Method::GET, "/metrics"),
    ] {
        let response = fixture
            .app
            .clone()
            .oneshot(managed_search_request(
                method,
                path,
                &fixture.search_key,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "leaked {path}");
    }
}

#[tokio::test]
async fn pbv3_parameter_and_scope_failures_are_closed() {
    let fixture = pbv3_fixture();
    for request in [
        serde_json::json!({"indexName": INDEX_NAME, "clickAnalytics": "true"}),
        serde_json::json!({"indexName": INDEX_NAME, "userToken": "not-a-uuid"}),
        serde_json::json!({"indexName": INDEX_NAME, "highlightPreTag": true}),
        serde_json::json!({"indexName": INDEX_NAME, "highlightPostTag": []}),
        serde_json::json!({"indexName": INDEX_NAME, "maxValuesPerFacet": "20"}),
        serde_json::json!({"indexName": INDEX_NAME, "analytics": "false"}),
        serde_json::json!({"indexName": INDEX_NAME, "attributesToHighlight": ["title"]}),
        serde_json::json!({"indexName": INDEX_NAME, "notARealSearchParameter": true}),
        serde_json::json!({"indexName": INDEX_NAME, "params": {"query": "legacy-nested"}}),
    ] {
        let response = fixture
            .app
            .clone()
            .oneshot(managed_search_request(
                Method::POST,
                DIRECT_SEARCH_PATH,
                &fixture.search_key,
                serde_json::json!({"requests": [request.clone()]}),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "accepted {request}"
        );
    }

    let (_, under_scoped) = fixture
        .key_store
        .create_key(api_key(&["search"], &[INDEX_NAME], 0));
    let under_scoped_response = fixture
        .app
        .clone()
        .oneshot(managed_search_request(
            Method::POST,
            PAID_BETA_V3_EVENTS_PATH,
            &under_scoped,
            serde_json::json!({"events": []}),
        ))
        .await
        .unwrap();
    assert_eq!(under_scoped_response.status(), StatusCode::FORBIDDEN);

    let wrong_index = fixture
        .app
        .clone()
        .oneshot(managed_search_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            &fixture.search_key,
            serde_json::json!({"requests": [{"indexName": "tenant_999_products", "query": ""}]}),
        ))
        .await
        .unwrap();
    assert_eq!(wrong_index.status(), StatusCode::FORBIDDEN);

    let wrong_app = Request::builder()
        .method(Method::POST)
        .uri(DIRECT_SEARCH_PATH)
        .header("content-type", "application/json")
        .header("x-algolia-application-id", "another-application")
        .header("x-algolia-api-key", &fixture.search_key)
        .body(Body::from(pbv3_query_batch().to_string()))
        .unwrap();
    assert_eq!(
        fixture.app.oneshot(wrong_app).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn pbv3_click_analytics_returns_a_correlated_query_id() {
    let fixture = pbv3_fixture();
    let response = fixture
        .app
        .oneshot(managed_search_request(
            Method::POST,
            DIRECT_SEARCH_PATH,
            &fixture.search_key,
            pbv3_query_batch(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let query_id = json["results"][0]["queryID"]
        .as_str()
        .expect("clickAnalytics=true must return queryID");
    assert_eq!(query_id.len(), 32);
    assert!(query_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

fn pbv3_preflight(path: &str, requested_method: &str) -> Request<Body> {
    Request::builder()
        .method(Method::OPTIONS)
        .uri(path)
        .header("origin", "http://127.0.0.1:5173")
        .header("access-control-request-method", requested_method)
        .header(
            "access-control-request-headers",
            "content-type,x-algolia-application-id,x-algolia-api-key,x-algolia-agent",
        )
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn pbv3_cors_admits_only_the_two_post_routes() {
    let fixture = pbv3_fixture();
    for path in [DIRECT_SEARCH_PATH, PAID_BETA_V3_EVENTS_PATH] {
        let response = fixture
            .app
            .clone()
            .oneshot(pbv3_preflight(path, "POST"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "blocked {path}");
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some("http://127.0.0.1:5173")
        );
    }

    let unsupported_route = fixture
        .app
        .clone()
        .oneshot(pbv3_preflight("/1/indexes", "POST"))
        .await
        .unwrap();
    assert_eq!(unsupported_route.status(), StatusCode::NOT_FOUND);

    let unsupported_method = fixture
        .app
        .oneshot(pbv3_preflight(DIRECT_SEARCH_PATH, "GET"))
        .await
        .unwrap();
    assert_eq!(unsupported_method.status(), StatusCode::NOT_FOUND);
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
        serde_json::json!(["full", "paid_beta_v1", "paid_beta_v3"])
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
