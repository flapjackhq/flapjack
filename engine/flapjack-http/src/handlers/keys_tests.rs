use super::*;
use crate::test_helpers::body_json;
use axum::{
    body::Body,
    http::{Method, Request},
    routing::{delete, get, post, put},
    Router,
};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

const TEST_ADMIN_KEY: &str = "test-admin-key-12345";

fn keys_router(key_store: Arc<KeyStore>) -> Router {
    Router::new()
        .route("/1/keys", post(create_key))
        .route("/1/keys", get(list_keys))
        .route("/1/keys/:key", get(get_key))
        .route("/1/keys/:key", put(update_key))
        .route("/1/keys/:key", delete(delete_key))
        .route("/1/keys/:key/restore", post(restore_key))
        .with_state(key_store)
}

fn make_test_key_store(tmp: &TempDir) -> Arc<KeyStore> {
    Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY))
}

fn block_key_material_writes(tmp: &TempDir) {
    let material_path = tmp.path().join("key_material.json");
    std::fs::remove_file(&material_path).unwrap();
    std::fs::create_dir(&material_path).unwrap();
}

/// Assert that a JSON value is a valid RFC 3339 timestamp string.
fn assert_rfc3339(val: &serde_json::Value, label: &str) {
    assert!(
        val.is_string(),
        "{} should be an RFC 3339 string, got: {}",
        label,
        val
    );
    let s = val.as_str().unwrap();
    chrono::DateTime::parse_from_rfc3339(s)
        .unwrap_or_else(|e| panic!("{} '{}' is not valid RFC 3339: {}", label, s, e));
}

/// Assert that a JSON value is an integer epoch timestamp.
fn assert_epoch_millis_integer(val: &serde_json::Value, label: &str) {
    assert!(
        val.is_i64() || val.is_u64(),
        "{} should be an integer epoch timestamp, got: {}",
        label,
        val
    );
}

/// TODO: Document create_test_key.
async fn create_test_key(app: &Router) -> Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/keys")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "acl": ["search", "browse"],
                        "description": "test key",
                        "indexes": ["idx1"],
                        "maxHitsPerQuery": 100,
                        "maxQueriesPerIPPerHour": 1000,
                        "queryParameters": "tags=public",
                        "referers": ["*.example.com"],
                        "validity": 3600
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await
}

// T1.1: GET /1/keys/{key} — createdAt is integer epoch timestamp
/// Verify GET /1/keys/{key} returns `createdAt` as an integer timestamp for SDK compatibility.
#[tokio::test]
async fn get_key_created_at_is_epoch_integer() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);

    let created = create_test_key(&app).await;
    let key_value = created["key"].as_str().unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/1/keys/{key_value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_epoch_millis_integer(&json["createdAt"], "GET key createdAt");
}

// T1.2: GET /1/keys — every key's createdAt is integer epoch timestamp
/// Verify GET /1/keys returns every key's `createdAt` as an integer timestamp.
#[tokio::test]
async fn list_keys_created_at_is_epoch_integer() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);

    // Create an extra key besides the admin key
    create_test_key(&app).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/1/keys")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let keys = json["keys"].as_array().expect("keys should be an array");
    assert!(!keys.is_empty(), "should have at least one key");
    for key in keys {
        assert_epoch_millis_integer(&key["createdAt"], "list keys createdAt");
    }
}

// T1.3: POST /1/keys — createdAt in response is RFC 3339 string
#[tokio::test]
async fn create_key_created_at_is_iso_string() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);

    let created = create_test_key(&app).await;
    assert_rfc3339(&created["createdAt"], "POST key createdAt");
}

// T1.4: POST /1/keys — response field is `key`, not `value`
#[tokio::test]
async fn create_key_response_field_is_key_not_value() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);

    let created = create_test_key(&app).await;
    assert!(
        created.get("key").is_some(),
        "POST response should have 'key' field"
    );
    assert!(
        created.get("value").is_none(),
        "POST response should NOT have 'value' field"
    );
}

/// A credential mutation is not successful until its durable state is
/// published. This fixture turns the material-file target into a directory so
/// the write fails deterministically on every supported filesystem.
#[tokio::test]
async fn create_key_persistence_failure_returns_500_and_keeps_memory_unchanged() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let original_keys = store.list_all();
    block_key_material_writes(&tmp);
    let app = keys_router(Arc::clone(&store));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/keys")
                .header("content-type", "application/json")
                .body(Body::from(create_key_json_body()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        store.list_all().len(),
        original_keys.len(),
        "an unpersisted key must never become active in memory"
    );
}

#[tokio::test]
async fn update_key_persistence_failure_returns_500_and_preserves_old_value() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let (_, plaintext) = store.create_key(ApiKey {
        description: "before failed update".to_string(),
        ..CreateKeyRequest {
            acl: vec!["search".to_string()],
            description: None,
            indexes: None,
            max_hits_per_query: None,
            max_queries_per_ip_per_hour: None,
            query_parameters: None,
            referers: None,
            restrict_sources: None,
            validity: None,
        }
        .into_api_key()
    });
    block_key_material_writes(&tmp);
    let app = keys_router(Arc::clone(&store));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/1/keys/{plaintext}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"acl": ["search"], "description": "after"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        store.lookup(&plaintext).unwrap().description,
        "before failed update"
    );
}

#[tokio::test]
async fn delete_key_persistence_failure_returns_500_and_keeps_key_active() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let (_, plaintext) = store.create_key(
        CreateKeyRequest {
            acl: vec!["search".to_string()],
            description: Some("failed delete".to_string()),
            indexes: None,
            max_hits_per_query: None,
            max_queries_per_ip_per_hour: None,
            query_parameters: None,
            referers: None,
            restrict_sources: None,
            validity: None,
        }
        .into_api_key(),
    );
    block_key_material_writes(&tmp);
    let app = keys_router(Arc::clone(&store));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/1/keys/{plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(store.lookup(&plaintext).is_some());
}

#[tokio::test]
async fn restore_key_persistence_failure_returns_500_and_keeps_key_deleted() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let (_, plaintext) = store.create_key(
        CreateKeyRequest {
            acl: vec!["search".to_string()],
            description: Some("failed restore".to_string()),
            indexes: None,
            max_hits_per_query: None,
            max_queries_per_ip_per_hour: None,
            query_parameters: None,
            referers: None,
            restrict_sources: None,
            validity: None,
        }
        .into_api_key(),
    );
    assert!(store.delete_key(&plaintext));
    block_key_material_writes(&tmp);
    let app = keys_router(Arc::clone(&store));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/1/keys/{plaintext}/restore"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(store.lookup(&plaintext).is_none());
}

// T1.5: GET key — hash, salt, hmac_key never exposed
/// Verify GET /1/keys/{key} never leaks internal credential fields (`hash`, `salt`, `hmac_key`, `hmacKey`).
#[tokio::test]
async fn key_response_never_exposes_hash_salt_hmac() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);

    let created = create_test_key(&app).await;
    let key_value = created["key"].as_str().unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/1/keys/{key_value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;

    assert!(json.get("hash").is_none(), "hash should not be in response");
    assert!(json.get("salt").is_none(), "salt should not be in response");
    assert!(
        json.get("hmac_key").is_none(),
        "hmac_key should not be in response"
    );
    assert!(
        json.get("hmacKey").is_none(),
        "hmacKey should not be in response"
    );
}

// T1.6: GET key — all Algolia fields present
/// Verify GET /1/keys/{key} includes all Algolia-compatible fields: `value`, `createdAt`, `acl`, `description`, `indexes`, `maxHitsPerQuery`, `maxQueriesPerIPPerHour`, `queryParameters`, `referers`, and `validity`.
#[tokio::test]
async fn key_response_includes_all_algolia_fields() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);

    let created = create_test_key(&app).await;
    let key_value = created["key"].as_str().unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/1/keys/{key_value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(resp).await;

    let required_fields = [
        "value",
        "createdAt",
        "acl",
        "description",
        "indexes",
        "maxHitsPerQuery",
        "maxQueriesPerIPPerHour",
        "queryParameters",
        "referers",
        "validity",
    ];
    for field in &required_fields {
        assert!(
            json.get(field).is_some(),
            "Missing required field '{}' in GET key response. Got: {}",
            field,
            json
        );
    }
}

// ── Content-Type middleware verification tests ──
// These prove normalize_content_type middleware is essential for Json<> extractor endpoints.

fn keys_router_with_middleware(key_store: Arc<KeyStore>) -> Router {
    Router::new()
        .route("/1/keys", post(create_key))
        .route("/1/keys", get(list_keys))
        .route("/1/keys/:key", get(get_key))
        .route("/1/keys/:key", put(update_key))
        .route("/1/keys/:key", delete(delete_key))
        .layer(axum::middleware::from_fn(
            crate::middleware::normalize_content_type,
        ))
        .with_state(key_store)
}

fn create_key_json_body() -> String {
    serde_json::json!({
        "acl": ["search"],
        "description": "content-type test key"
    })
    .to_string()
}

// ── Notification wiring tests ──

/// Verify POST /1/keys increments the `key_lifecycle_call_count` on the global notifier.
#[tokio::test]
async fn create_key_sends_lifecycle_notification() {
    let service = std::sync::Arc::new(crate::notifications::NotificationService::disabled());
    crate::notifications::init_global_notifier(std::sync::Arc::clone(&service));

    let notifier = crate::notifications::global_notifier().expect("notifier should be set");
    let before = notifier
        .key_lifecycle_call_count
        .load(std::sync::atomic::Ordering::Relaxed);

    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);

    create_test_key(&app).await;

    let after = notifier
        .key_lifecycle_call_count
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        after > before,
        "send_key_lifecycle should have been called on create: before={before}, after={after}"
    );
}

/// Verify DELETE /1/keys/{key} increments the `key_lifecycle_call_count` on the global notifier.
#[tokio::test]
async fn delete_key_sends_lifecycle_notification() {
    let service = std::sync::Arc::new(crate::notifications::NotificationService::disabled());
    crate::notifications::init_global_notifier(std::sync::Arc::clone(&service));

    let notifier = crate::notifications::global_notifier().expect("notifier should be set");

    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);

    // Create a key first
    let created = create_test_key(&app).await;
    let key_value = created["key"].as_str().unwrap();

    let before = notifier
        .key_lifecycle_call_count
        .load(std::sync::atomic::Ordering::Relaxed);

    // Delete it
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/1/keys/{key_value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let after = notifier
        .key_lifecycle_call_count
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        after > before,
        "send_key_lifecycle should have been called on delete: before={before}, after={after}"
    );
}

// T3.1: Without middleware, text/plain content-type is rejected by Json<> extractor
/// Create a new API key with specified ACL permissions and restrictions.text_plain_rejected_without_middleware.
#[tokio::test]
async fn create_key_text_plain_rejected_without_middleware() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store); // NO middleware

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/keys")
                .header("content-type", "text/plain")
                .body(Body::from(create_key_json_body()))
                .unwrap(),
        )
        .await
        .unwrap();

    // Axum Json<> extractor rejects non-application/json content types
    assert_ne!(
        resp.status(),
        StatusCode::OK,
        "Without middleware, text/plain should be rejected by Json<> extractor"
    );
}

// T3.2: With middleware, text/plain is normalized to application/json and accepted
/// Create a new API key with specified ACL permissions and restrictions.text_plain_accepted_with_middleware.
#[tokio::test]
async fn create_key_text_plain_accepted_with_middleware() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router_with_middleware(store); // WITH middleware

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/keys")
                .header("content-type", "text/plain")
                .body(Body::from(create_key_json_body()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "With middleware, text/plain should be normalized to application/json"
    );
}

// T3.3: With middleware, charset variant is normalized and accepted
/// Create a new API key with specified ACL permissions and restrictions.charset_content_type_accepted_with_middleware.
#[tokio::test]
async fn create_key_charset_content_type_accepted_with_middleware() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router_with_middleware(store);

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/keys")
                .header("content-type", "application/json; charset=utf-8")
                .body(Body::from(create_key_json_body()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "application/json; charset=utf-8 must be accepted with middleware"
    );
}

// T3.4: With middleware, missing content-type is normalized and accepted
/// Create a new API key with specified ACL permissions and restrictions.no_content_type_accepted_with_middleware.
#[tokio::test]
async fn create_key_no_content_type_accepted_with_middleware() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router_with_middleware(store);

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/keys")
                .body(Body::from(create_key_json_body()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "Missing Content-Type must be accepted with middleware"
    );
}

// ── restrictSources CRUD round-trip tests ──

/// TODO: Document post_key_with_restrict_sources_round_trips_through_get.
#[tokio::test]
async fn post_key_with_restrict_sources_round_trips_through_get() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/keys")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "acl": ["search"],
                        "restrictSources": ["192.168.1.0/24", "10.0.0.1"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = body_json(resp).await;
    let key_value = created["key"].as_str().unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/1/keys/{key_value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(
        json["restrictSources"],
        serde_json::json!(["192.168.1.0/24", "10.0.0.1"])
    );
}

/// PUT /1/keys/{key} updates restrictSources; GET /1/keys list includes the updated value.
#[tokio::test]
async fn put_key_updates_restrict_sources_visible_in_list() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);

    // Create a key without restrictSources
    let created = create_test_key(&app).await;
    let key_value = created["key"].as_str().unwrap();

    // PUT update with restrictSources
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/1/keys/{key_value}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "acl": ["search"],
                        "restrictSources": ["10.0.0.0/8"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // GET /1/keys list — find the key and verify restrictSources
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/1/keys")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let keys = json["keys"].as_array().unwrap();
    let matching = keys
        .iter()
        .find(|k| k["value"].as_str() == Some(key_value))
        .expect("key should appear in list");
    assert_eq!(
        matching["restrictSources"],
        serde_json::json!(["10.0.0.0/8"])
    );
}

/// Create a new API key with specified ACL permissions and restrictions.rejects_malformed_restrict_sources.
#[tokio::test]
async fn create_key_rejects_malformed_restrict_sources() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/keys")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "acl": ["search"],
                        "restrictSources": ["not-a-network"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert!(
        json["message"]
            .as_str()
            .is_some_and(|msg| msg.contains("Invalid restrictSources entry")),
        "error message should call out restrictSources validation: {json}"
    );
}

/// TODO: Document update_key_rejects_malformed_restrict_sources.
#[tokio::test]
async fn update_key_rejects_malformed_restrict_sources() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = keys_router(store);
    let created = create_test_key(&app).await;
    let key_value = created["key"]
        .as_str()
        .expect("created key should be present");

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri(format!("/1/keys/{key_value}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "acl": ["search"],
                        "restrictSources": ["bad-source"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert!(
        json["message"]
            .as_str()
            .is_some_and(|msg| msg.contains("Invalid restrictSources entry")),
        "error message should call out restrictSources validation: {json}"
    );
}

/// TODO: Document generate_secured_key_rejects_malformed_restrict_sources.
#[tokio::test]
async fn generate_secured_key_rejects_malformed_restrict_sources() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let app = Router::new()
        .route("/1/keys/generateSecuredApiKey", post(generate_secured_key))
        .with_state(store.clone());

    let (_, parent_api_key) = store.create_key(ApiKey {
        hash: String::new(),
        salt: String::new(),
        hmac_key: None,
        created_at: 0,
        acl: vec!["search".to_string()],
        description: "parent".to_string(),
        indexes: vec![],
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: vec![],
        validity: 0,
        restrict_sources: None,
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/keys/generateSecuredApiKey")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "parentApiKey": parent_api_key,
                        "restrictions": {
                            "restrictSources": "not-a-network"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert!(
        json["message"]
            .as_str()
            .is_some_and(|msg| msg.contains("Invalid restrictSources entry")),
        "error message should call out restrictSources validation: {json}"
    );
}

/// `GenerateSecuredKeyRequest` must preserve `restrictSources` in signed params so auth middleware can enforce source restrictions.
#[test]
fn generate_secured_key_request_carries_restrict_sources_into_signed_payload() {
    let tmp = TempDir::new().unwrap();
    let store = make_test_key_store(&tmp);
    let (_, parent_api_key) = store.create_key(ApiKey {
        hash: String::new(),
        salt: String::new(),
        hmac_key: None,
        created_at: 0,
        acl: vec!["search".to_string()],
        description: "parent".to_string(),
        indexes: vec![],
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: vec![],
        validity: 0,
        restrict_sources: None,
    });

    let request: GenerateSecuredKeyRequest = serde_json::from_value(serde_json::json!({
        "parentApiKey": parent_api_key,
        "restrictions": {
            "restrictSources": "127.0.0.0/8,10.0.0.0/8"
        }
    }))
    .expect("request JSON should deserialize");

    let params_str = request.restrictions.to_query_params();
    let secured = crate::auth::generate_secured_api_key(&request.parent_api_key, &params_str);
    let (_, parsed_restrictions) = crate::auth::validate_secured_key(&secured, &store)
        .expect("generated secured key should validate");

    assert_eq!(
        parsed_restrictions.restrict_sources,
        Some("127.0.0.0/8,10.0.0.0/8".to_string()),
        "signed restrictions must include restrictSources for middleware enforcement"
    );
}
