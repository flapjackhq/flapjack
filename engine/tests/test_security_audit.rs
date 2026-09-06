use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use base64::Engine as _;
use flapjack::analytics::schema::SearchEvent;
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig, AnalyticsQueryEngine};
#[cfg(feature = "vector-search")]
use flapjack::index::settings::IndexSettings;
use flapjack::types::{Document, FieldValue};
use flapjack_http::analytics_cluster::AnalyticsClusterClient;
use flapjack_http::auth::{
    generate_secured_api_key, index_pattern_matches, referer_matches,
    validate_restrict_sources_csv, validate_restrict_sources_entries, validate_secured_key,
    KeyStore,
};
use flapjack_http::startup::{
    cors_origins_from_value, validate_startup_auth_policy, CorsMode, StartupAuthValidationError,
    StartupAuthValidationOutcome,
};
use flapjack_replication::config::NodeConfig;
use flapjack_replication::manager::ReplicationManager;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use tar::{Builder, EntryType, Header};
use tempfile::TempDir;
use tower::ServiceExt;
use tracing_subscriber::prelude::*;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

mod common;

const ADMIN_KEY: &str = "test-admin-key-security-audit";

use common::authed_request;

fn a10_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct A10EnvRestore {
    keys: Vec<(&'static str, Option<String>)>,
}

impl A10EnvRestore {
    fn capture(keys: &[&'static str]) -> Self {
        Self {
            keys: keys
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect(),
        }
    }
}

impl Drop for A10EnvRestore {
    fn drop(&mut self) {
        for (key, value) in &self.keys {
            match value {
                Some(restored) => std::env::set_var(key, restored),
                None => std::env::remove_var(key),
            }
        }
    }
}

async fn a10_set_neural_mode(app: &axum::Router, index_name: &str, user_data: serde_json::Value) {
    let settings_response = app
        .clone()
        .oneshot(authed_request(
            Method::PUT,
            &format!("/1/indexes/{index_name}/settings"),
            ADMIN_KEY,
            Some(json!({
                "mode": "neuralSearch",
                "userData": user_data,
            })),
        ))
        .await
        .unwrap();
    assert_eq!(
        settings_response.status(),
        StatusCode::OK,
        "settings update must succeed for a10 chat setup"
    );
    let settings_body = common::body_json(settings_response).await;
    common::wait_for_task_local_with_key(app, common::extract_task_id(&settings_body), ADMIN_KEY)
        .await;
}

#[tokio::test]
// The std-Mutex guard intentionally spans the awaits below: it serializes
// process-global env-var access for the whole test, which mutates and then
// awaits HTTP calls that read those vars. `a10_env_lock` is also used by a
// synchronous `#[test]`, so it cannot be an async-aware mutex.
#[allow(clippy::await_holding_lock)]
async fn a10_chat_ai_provider_rejects_unsafe_base_urls_from_settings() {
    let _env_guard = a10_env_lock().lock().unwrap();
    let _env_restore = A10EnvRestore::capture(&[
        "FLAPJACK_AI_BASE_URL",
        "FLAPJACK_AI_API_KEY",
        "FLAPJACK_AI_MODEL",
        "FLAPJACK_AI_ALLOW_LOCAL_URLS",
    ]);
    std::env::remove_var("FLAPJACK_AI_BASE_URL");
    std::env::remove_var("FLAPJACK_AI_API_KEY");
    std::env::remove_var("FLAPJACK_AI_MODEL");
    // Default fail-closed posture: the loopback/private opt-in must be off here.
    std::env::remove_var("FLAPJACK_AI_ALLOW_LOCAL_URLS");

    let index_name = "a10_chat_settings_idx";
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    common::seed_docs(
        &app,
        index_name,
        ADMIN_KEY,
        vec![json!({"objectID": "seed-1", "title": "seed"})],
    )
    .await;

    // non-http scheme, loopback, RFC1918 private, and a malformed URL. Each fails
    // closed at the config seam (400) before any outbound call is attempted.
    for payload in [
        "file:///etc/passwd",
        "http://127.0.0.1:9",
        "http://10.0.0.1",
        "ht!tp://oops",
    ] {
        a10_set_neural_mode(
            &app,
            index_name,
            json!({
                "aiProvider": {
                    "baseUrl": payload,
                    "apiKey": "test-key"
                }
            }),
        )
        .await;

        let response = app
            .clone()
            .oneshot(authed_request(
                Method::POST,
                &format!("/1/indexes/{index_name}/chat"),
                ADMIN_KEY,
                Some(json!({ "query": "hello" })),
            ))
            .await
            .unwrap();
        let status = response.status();
        let body = common::body_json(response).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "chat config with payload {payload} must fail closed at config seam"
        );
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|msg| msg.contains("base URL")),
            "chat error must identify invalid base URL payload {payload}: {body}"
        );
    }
}

#[tokio::test]
// See the sibling test above: the guard intentionally spans awaits to serialize
// process-global env-var access, and the lock is shared with a sync `#[test]`.
#[allow(clippy::await_holding_lock)]
async fn a10_chat_ai_provider_rejects_unsafe_base_urls_from_env() {
    let _env_guard = a10_env_lock().lock().unwrap();
    let _env_restore = A10EnvRestore::capture(&[
        "FLAPJACK_AI_BASE_URL",
        "FLAPJACK_AI_API_KEY",
        "FLAPJACK_AI_MODEL",
        "FLAPJACK_AI_ALLOW_LOCAL_URLS",
    ]);
    // Default fail-closed posture: the loopback/private opt-in must be off here.
    std::env::remove_var("FLAPJACK_AI_ALLOW_LOCAL_URLS");

    let index_name = "a10_chat_env_idx";
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    common::seed_docs(
        &app,
        index_name,
        ADMIN_KEY,
        vec![json!({"objectID": "seed-2", "title": "seed"})],
    )
    .await;
    a10_set_neural_mode(&app, index_name, json!({})).await;

    // link-local metadata, loopback, RFC1918 private, and a malformed URL. Each
    // fails closed at the config seam (400) before any outbound call is attempted.
    for payload in [
        "http://169.254.169.254",
        "http://127.0.0.1:9",
        "http://10.0.0.1",
        "http://[::1",
    ] {
        std::env::set_var("FLAPJACK_AI_BASE_URL", payload);
        std::env::set_var("FLAPJACK_AI_API_KEY", "test-key");
        std::env::remove_var("FLAPJACK_AI_MODEL");

        let response = app
            .clone()
            .oneshot(authed_request(
                Method::POST,
                &format!("/1/indexes/{index_name}/chat"),
                ADMIN_KEY,
                Some(json!({ "query": "hello" })),
            ))
            .await
            .unwrap();
        let status = response.status();
        let body = common::body_json(response).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "env base URL payload {payload} must fail closed before outbound call"
        );
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|msg| msg.contains("base URL")),
            "chat env error must identify invalid base URL payload {payload}: {body}"
        );
    }
}

// Asserts behavior of `IndexSettings::validate_embedders_inner`, which is a
// no-op under `#[cfg(not(feature = "vector-search"))]` — gating the test on
// the same feature keeps the SSRF coverage live under the heavy CI lane
// (`cargo nextest run ... --features vector-search`) while letting the lean
// default-build test suite skip it instead of asserting a no-op.
#[cfg(feature = "vector-search")]
#[test]
fn a10_vector_embedders_reject_ssrf_payload_urls() {
    // Each payload is validated in isolation so every class is asserted-rejected
    // individually (a single combined map would only prove one entry failed).
    // Classes:
    //   - link-local metadata endpoint, loopback, RFC1918 private (literal IPs)
    //   - numeric-form host `2130706433`: the url crate canonicalizes this to
    //     127.0.0.1 during parse, so the literal-IP check already rejects it
    //     (asserts that canonicalization defense holds; not a resolution bypass)
    //   - `localhost.` (trailing-dot FQDN): the url crate keeps this as the
    //     registered name "localhost.", which slips past both the literal-IP
    //     check AND the exact "localhost" string check; it is only caught by
    //     resolving the host and checking the resolved addresses. This is the
    //     hostname-resolution SSRF bypass that the seam fix closes.
    //   - non-http scheme and a malformed URL
    let payloads = [
        "http://169.254.169.254",
        "http://127.0.0.1:9",
        "http://10.0.0.1",
        "http://2130706433",
        "http://localhost.",
        "file:///etc/passwd",
        "http://[::1",
    ];

    for payload in payloads {
        // openAi source.
        let mut openai = std::collections::HashMap::new();
        openai.insert(
            "openai_probe".to_string(),
            json!({"source": "openAi", "apiKey": "sk-test", "url": payload}),
        );
        let openai_settings = IndexSettings {
            embedders: Some(openai),
            ..Default::default()
        };
        let openai_err = match openai_settings.validate_embedders() {
            Ok(()) => panic!("openAi embedder must reject payload {payload}"),
            Err(e) => e,
        };
        assert!(
            openai_err.contains("openai_probe"),
            "openAi validation error must name the embedder for {payload}: {openai_err}"
        );

        // rest source.
        let mut rest = std::collections::HashMap::new();
        rest.insert(
            "rest_probe".to_string(),
            json!({
                "source": "rest",
                "url": payload,
                "request": {"input": "{{text}}"},
                "response": {"embedding": "{{embedding}}"},
            }),
        );
        let rest_settings = IndexSettings {
            embedders: Some(rest),
            ..Default::default()
        };
        let rest_err = match rest_settings.validate_embedders() {
            Ok(()) => panic!("rest embedder must reject payload {payload}"),
            Err(e) => e,
        };
        assert!(
            rest_err.contains("rest_probe"),
            "rest validation error must name the embedder for {payload}: {rest_err}"
        );
    }
}

#[test]
fn a10_peer_address_intake_filters_only_localhost_and_metadata_destinations() {
    let _env_guard = a10_env_lock().lock().unwrap();
    let _env_restore = A10EnvRestore::capture(&["FLAPJACK_NODE_ID", "FLAPJACK_PEERS"]);
    std::env::set_var("FLAPJACK_NODE_ID", "node-a");
    // Classes that must be dropped at config intake:
    //   - link-local metadata, loopback
    //   - non-http scheme
    //   - numeric-form `2130706433` (url crate canonicalizes to 127.0.0.1, caught
    //     by the literal-IP check; asserts canonicalization defense holds)
    //   - `localhost.` trailing-dot FQDN (kept as a registered name by the url
    //     crate, slips past literal + "localhost" string checks; only the
    //     resolve-and-check seam catches it — the hostname-resolution SSRF bypass)
    //   - RFC1918 private peers remain allowed because replication/analytics
    //     fan-out is operator-configured for internal clusters.
    std::env::set_var(
        "FLAPJACK_PEERS",
        "meta=http://169.254.169.254,loop=http://127.0.0.1:7700,priv=http://10.0.0.1:7700,scheme=file:///tmp/a10,numeric=http://2130706433:7700,lhdot=http://localhost.:7700",
    );

    let temp_dir = tempfile::tempdir().unwrap();
    let config = NodeConfig::load_or_default(temp_dir.path());
    assert_eq!(config.peers.len(), 1, "only the RFC1918 peer should remain");
    assert_eq!(config.peers[0].node_id, "priv");
    assert_eq!(config.peers[0].addr, "http://10.0.0.1:7700");

    let repl = ReplicationManager::new(config.clone(), None, temp_dir.path().to_path_buf());
    assert_eq!(
        repl.peer_count(),
        1,
        "replication manager must keep the operator-configured private peer"
    );
    assert!(
        AnalyticsClusterClient::new(&config, None).is_some(),
        "analytics fan-out client should still be constructed for private-cluster peers"
    );
}

#[tokio::test]
async fn malformed_secured_keys_return_canonical_403_without_decode_leaks() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    // Both a non-Base64 token and a short decoded payload must follow the same
    // canonical rejection shape from invalid_api_credentials_error().
    for malformed_key in ["not_base64!!!", "c2hvcnQ="] {
        let req = authed_request(
            Method::POST,
            "/1/indexes/products/query",
            malformed_key,
            Some(json!({"query": "test"})),
        );
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let body = common::assert_error_contract_from_oneshot(resp, 403).await;

        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "malformed credential '{malformed_key}' must be rejected with 403",
        );
        assert_eq!(
            body,
            json!({
                "message": "Invalid Application-ID or API key",
                "status": 403
            }),
            "malformed secured key must not leak decode/parse internals in error payload",
        );
    }
}

/// SEC-G1 regression: a byte length >= 64 does not imply byte offset 64 is a
/// UTF-8 char boundary. The parser must split the fixed-width ASCII HMAC prefix
/// as bytes before validating the parameter suffix as UTF-8.
///
/// The pre-auth contract is that any malformed key is rejected (returns `None`)
/// without crashing the parser. This test prevents regressions to fixed-offset
/// `str` slicing.
#[test]
fn a07_secured_key_validation_survives_non_char_boundary_payload() {
    // Layout: 63 ASCII bytes, then `é` (2 UTF-8 bytes at offsets 63 and 64),
    // then trailing ASCII. Byte offset 64 lands on the `é` continuation byte,
    // so it is NOT a char boundary even though the string is well past 64 bytes.
    let payload = format!("{}{}{}", "a".repeat(63), 'é', "trailing_ascii_params");
    assert!(
        !payload.is_char_boundary(64),
        "reproducer invariant: byte offset 64 must fall off a UTF-8 char boundary",
    );

    let tmp = TempDir::new().expect("keystore tempdir");
    let key_store = KeyStore::load_or_create(tmp.path(), ADMIN_KEY);

    let encoded = base64::engine::general_purpose::STANDARD.encode(payload.as_bytes());

    // catch_unwind so the panic at the byte-index slice becomes an assertable
    // Err(..) rather than aborting the test binary. Ok(None) is the only
    // acceptable outcome: parsed, rejected, no crash.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        validate_secured_key(&encoded, &key_store)
    }));

    assert!(
        matches!(result, Ok(None)),
        "validate_secured_key must reject a non-char-boundary secured key by \
         returning None, not panic at the decoded_str[..64] byte slice (SEC-G1)",
    );
}

#[test]
fn a07_secured_key_validation_rejects_invalid_utf8_parameters() {
    let tmp = TempDir::new().expect("keystore tempdir");
    let key_store = KeyStore::load_or_create(tmp.path(), ADMIN_KEY);
    let search_key = key_store
        .list_all()
        .into_iter()
        .find_map(|key| key.hmac_key)
        .expect("default search key must retain HMAC material");

    // Make the HMAC valid for the lossy interpretation of 0xFF. A parser that
    // silently replaces invalid UTF-8 would therefore accept this credential.
    let mut payload = base64::engine::general_purpose::STANDARD
        .decode(generate_secured_api_key(&search_key, "\u{FFFD}"))
        .expect("generated secured key must be valid base64");
    payload.truncate(64);
    payload.push(0xFF);
    assert!(
        payload[..64].iter().all(u8::is_ascii_hexdigit),
        "fixture HMAC prefix must reach parameter UTF-8 validation",
    );

    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        validate_secured_key(&encoded, &key_store)
    }));

    assert!(
        matches!(result, Ok(None)),
        "invalid UTF-8 parameters must be rejected, not replaced or accepted",
    );
}

// Stage 3 parser sweep: auth public seams. These keep private secured-key and
// referer helpers private while exercising hostile inputs through stable API
// boundaries.
#[test]
fn stage3_secured_key_restrictions_round_trip_multibyte_and_empty_values() {
    let tmp = TempDir::new().expect("keystore tempdir");
    let key_store = KeyStore::load_or_create(tmp.path(), ADMIN_KEY);
    let search_key = key_store
        .list_all()
        .into_iter()
        .find_map(|key| key.hmac_key)
        .expect("default search key must retain HMAC material");

    let params = concat!(
        "filters=category%3Acr%C3%A8me+br%C3%BBl%C3%A9e",
        "&restrictIndices=%5B%22prod_cafe_%C3%A9%22%2C%22tenant_%E6%9D%B1%E4%BA%AC%22%5D",
        "&userToken=%E7%94%A8%E6%88%B7-123",
        "&hitsPerPage=184467440737095516160",
        "&restrictSources="
    );
    let secured_key = generate_secured_api_key(&search_key, params);

    let (_api_key, restrictions) =
        validate_secured_key(&secured_key, &key_store).expect("valid HMAC must parse");
    assert_eq!(
        restrictions.filters.as_deref(),
        Some("category:crème brûlée")
    );
    assert_eq!(restrictions.user_token.as_deref(), Some("用户-123"));
    assert_eq!(
        restrictions.restrict_indices,
        Some(vec!["prod_cafe_é".to_string(), "tenant_東京".to_string()])
    );
    assert_eq!(
        restrictions.hits_per_page, None,
        "overflowing unsigned hitsPerPage must be ignored rather than wrapped"
    );
    assert_eq!(restrictions.restrict_sources.as_deref(), Some(""));
}

#[test]
fn stage3_secured_key_rejects_empty_and_one_byte_short_decoded_values() {
    let tmp = TempDir::new().expect("keystore tempdir");
    let key_store = KeyStore::load_or_create(tmp.path(), ADMIN_KEY);

    for decoded in [Vec::new(), vec![b'a'; 63]] {
        let encoded = base64::engine::general_purpose::STANDARD.encode(decoded);
        assert!(
            validate_secured_key(&encoded, &key_store).is_none(),
            "short decoded secured-key payloads must reject without panic"
        );
    }
}

#[test]
fn stage3_referer_and_index_globs_handle_multibyte_and_empty_inputs() {
    let referer_patterns = vec![
        "https://shop.example.com/*".to_string(),
        "*пример.рф".to_string(),
    ];
    assert!(referer_matches(
        "https://shop.example.com/café?item=東京",
        &referer_patterns
    ));
    assert!(referer_matches("https://пример.рф/путь", &referer_patterns));
    assert!(!referer_matches("", &referer_patterns));
    assert!(referer_matches("", &[]));

    let index_patterns = vec!["prod_*".to_string(), "*東京".to_string()];
    assert!(index_pattern_matches(&index_patterns, "prod_café"));
    assert!(index_pattern_matches(&index_patterns, "tenant_東京"));
    assert!(!index_pattern_matches(&index_patterns, ""));
    assert!(index_pattern_matches(&[], ""));
}

#[test]
fn stage3_restrict_sources_public_validators_reject_hostile_entries() {
    assert!(validate_restrict_sources_csv(" ,127.0.0.1, 10.0.0.0/8 ").is_ok());
    assert_eq!(
        validate_restrict_sources_csv("../keys").unwrap_err(),
        "../keys"
    );

    let entries = vec![
        "192.168.1.0/24".to_string(),
        "https://shop.example.com/*".to_string(),
        "bad source".to_string(),
    ];
    assert_eq!(
        validate_restrict_sources_entries(&entries).unwrap_err(),
        "bad source"
    );
}

#[tokio::test]
async fn a01_restricted_key_denies_cross_tenant_index_query() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    common::seed_docs(
        &app,
        "tenant_allowed",
        ADMIN_KEY,
        vec![json!({"objectID": "allowed-1", "name": "Allowed Document"})],
    )
    .await;

    let create_req = authed_request(
        Method::POST,
        "/1/keys",
        ADMIN_KEY,
        Some(json!({
            "acl": ["search"],
            "indexes": ["tenant_allowed"],
            "description": "a01 restricted key tenant scope"
        })),
    );
    let create_resp = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);
    let restricted_key = common::body_json(create_resp).await["key"]
        .as_str()
        .expect("create key response must include plaintext key")
        .to_string();

    let forbidden_req = authed_request(
        Method::POST,
        "/1/indexes/tenant_forbidden/query",
        &restricted_key,
        Some(json!({"query": "forbidden"})),
    );
    let forbidden_resp = app.clone().oneshot(forbidden_req).await.unwrap();
    let status = forbidden_resp.status();
    let body = common::assert_error_contract_from_oneshot(forbidden_resp, 403).await;

    assert!(
        status == StatusCode::FORBIDDEN,
        "restricted key must fail closed for cross-tenant index access"
    );
    assert_eq!(
        body,
        json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );
}

#[tokio::test]
async fn a01_non_admin_key_cannot_access_internal_status() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let create_req = authed_request(
        Method::POST,
        "/1/keys",
        ADMIN_KEY,
        Some(json!({
            "acl": ["search"],
            "description": "a01 non-admin key"
        })),
    );
    let create_resp = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);
    let non_admin_key = common::body_json(create_resp).await["key"]
        .as_str()
        .expect("create key response must include plaintext key")
        .to_string();

    let internal_req = authed_request(Method::GET, "/internal/status", &non_admin_key, None);
    let internal_resp = app.clone().oneshot(internal_req).await.unwrap();
    let status = internal_resp.status();
    let body = common::assert_error_contract_from_oneshot(internal_resp, 403).await;

    assert!(
        status == StatusCode::FORBIDDEN,
        "non-admin key must fail closed on /internal/status"
    );
    assert_eq!(
        body,
        json!({
            "message": "Method not allowed with this API key",
            "status": 403
        })
    );
}

#[tokio::test]
async fn a01_internal_route_rejects_empty_application_id_header() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method(Method::GET)
                .uri("/internal/status")
                .header("x-algolia-api-key", ADMIN_KEY)
                .header("x-algolia-application-id", "")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = common::assert_error_contract_from_oneshot(response, 403).await;

    assert!(
        status == StatusCode::FORBIDDEN,
        "empty application-id header must fail closed on internal routes"
    );
    assert_eq!(
        body,
        json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );
}

#[tokio::test]
async fn a02_generated_search_keys_have_128_bits_after_prefix_strip() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let mut seen = HashSet::new();

    for i in 0..64 {
        let create_req = authed_request(
            Method::POST,
            "/1/keys",
            ADMIN_KEY,
            Some(json!({
                "acl": ["search"],
                "description": format!("a02 entropy key {i}")
            })),
        );
        let create_resp = app.clone().oneshot(create_req).await.unwrap();
        assert_eq!(create_resp.status(), StatusCode::OK);
        let key = common::body_json(create_resp).await["key"]
            .as_str()
            .expect("create key response must include plaintext key")
            .to_string();

        assert!(
            key.starts_with("fj_search_"),
            "search key must keep fj_search_ prefix"
        );
        let stripped = &key["fj_search_".len()..];
        assert_eq!(
            stripped.len(),
            32,
            "search-key suffix must be 32 hex chars (=128 bits)"
        );
        assert!(
            stripped.chars().all(|c| c.is_ascii_hexdigit()),
            "search-key suffix must be hex"
        );
        assert!(
            seen.insert(key),
            "generated keys must not repeat in this sample set"
        );
    }
}

#[tokio::test]
async fn a02_keys_json_does_not_persist_plaintext_api_keys() {
    let (app, tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let list_req = authed_request(Method::GET, "/1/keys", ADMIN_KEY, None);
    let list_resp = app.clone().oneshot(list_req).await.unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_body = common::body_json(list_resp).await;
    let values = list_body
        .get("keys")
        .and_then(|keys| keys.as_array())
        .expect("list keys response must include keys array")
        .iter()
        .filter_map(|entry| entry.get("value").and_then(|value| value.as_str()))
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert!(
        !values.is_empty(),
        "list keys must expose at least one key value"
    );

    let keys_json = std::fs::read_to_string(tmp.path().join("keys.json"))
        .expect("keys.json must exist for auth-enabled app");
    let key_material_json = std::fs::read_to_string(tmp.path().join("key_material.json"))
        .expect("key_material.json must exist for auth-enabled app");
    assert!(
        keys_json.contains("\"hash\"") && keys_json.contains("\"salt\""),
        "keys.json must persist salted-hash fields"
    );

    for value in values {
        assert!(
            !keys_json.contains(&value),
            "keys.json must not persist plaintext API key value: {value}"
        );
        assert!(
            !key_material_json.contains(&value),
            "key_material.json must not persist plaintext API key value: {value}"
        );
    }
    assert!(
        !keys_json.contains(ADMIN_KEY),
        "keys.json must never persist plaintext admin key value"
    );
    assert!(
        !key_material_json.contains(ADMIN_KEY),
        "key_material.json must never persist plaintext admin key value"
    );
}

#[tokio::test]
async fn a02_rotate_admin_key_invalidates_old_and_accepts_new() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let rotate_req = authed_request(Method::POST, "/internal/rotate-admin-key", ADMIN_KEY, None);
    let rotate_resp = app.clone().oneshot(rotate_req).await.unwrap();
    assert_eq!(rotate_resp.status(), StatusCode::OK);
    let new_key = common::body_json(rotate_resp).await["key"]
        .as_str()
        .expect("rotation response must include new key")
        .to_string();
    assert_ne!(new_key, ADMIN_KEY, "rotation must produce a new key");

    let old_req = authed_request(Method::GET, "/internal/status", ADMIN_KEY, None);
    let old_resp = app.clone().oneshot(old_req).await.unwrap();
    assert_eq!(old_resp.status(), StatusCode::FORBIDDEN);
    let old_body = common::assert_error_contract_from_oneshot(old_resp, 403).await;
    assert_eq!(
        old_body,
        json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );

    let new_req = authed_request(Method::GET, "/internal/status", &new_key, None);
    let new_resp = app.clone().oneshot(new_req).await.unwrap();
    assert_eq!(
        new_resp.status(),
        StatusCode::OK,
        "newly rotated key must authenticate internal route access"
    );
}

#[test]
fn a02_reset_admin_key_refuses_offline_rotation_with_encrypted_key_material() {
    let tmp = TempDir::new().unwrap();
    let _store = flapjack_http::auth::KeyStore::load_or_create(tmp.path(), ADMIN_KEY);
    let keys_before = std::fs::read_to_string(tmp.path().join("keys.json"))
        .expect("keys.json must exist after key store initialization");
    let key_material_before = std::fs::read_to_string(tmp.path().join("key_material.json"))
        .expect("key_material.json must exist after key store initialization");

    let error = flapjack_http::auth::reset_admin_key(tmp.path())
        .expect_err("offline admin reset must fail closed once key material is encrypted");

    assert!(
        error.contains("rotate-admin-key") && error.contains("key_material.json"),
        "error should explain the safe recovery path: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("keys.json")).unwrap(),
        keys_before,
        "failed offline reset must leave keys.json unchanged"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("key_material.json")).unwrap(),
        key_material_before,
        "failed offline reset must leave key_material.json unchanged"
    );
}

#[tokio::test]
async fn a02_internal_auth_depends_only_on_algolia_auth_headers() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let success_with_no_tls_headers = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method(Method::GET)
                .uri("/internal/status")
                .header("x-algolia-api-key", ADMIN_KEY)
                .header("x-algolia-application-id", "test")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(success_with_no_tls_headers.status(), StatusCode::OK);

    let fail_with_tls_metadata_headers = app
        .oneshot(
            axum::http::Request::builder()
                .method(Method::GET)
                .uri("/internal/status")
                .header("x-algolia-api-key", "wrong-key")
                .header("x-algolia-application-id", "test")
                .header("x-forwarded-proto", "https")
                .header("x-forwarded-host", "search.example.test")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        fail_with_tls_metadata_headers.status(),
        StatusCode::FORBIDDEN
    );
    let fail_body =
        common::assert_error_contract_from_oneshot(fail_with_tls_metadata_headers, 403).await;
    assert_eq!(
        fail_body,
        json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );
}

#[tokio::test]
async fn a07_repeated_invalid_credentials_keep_canonical_403_and_do_not_consume_valid_key_budget() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let index_name = "a07_invalid_boundary";

    common::seed_docs(
        &app,
        index_name,
        ADMIN_KEY,
        vec![json!({"objectID": "doc-1", "name": "A07 boundary fixture"})],
    )
    .await;

    let create_req = authed_request(
        Method::POST,
        "/1/keys",
        ADMIN_KEY,
        Some(json!({
            "acl": ["search"],
            "maxQueriesPerIPPerHour": 2,
            "description": "a07 rate-limit boundary key"
        })),
    );
    let create_resp = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);
    let valid_key = common::body_json(create_resp).await["key"]
        .as_str()
        .expect("create key response must include plaintext key")
        .to_string();

    for i in 0..5 {
        let invalid_req = authed_request(
            Method::POST,
            &format!("/1/indexes/{index_name}/query"),
            "definitely-invalid-key",
            Some(json!({"query": "test"})),
        );
        let invalid_resp = app.clone().oneshot(invalid_req).await.unwrap();
        assert_eq!(
            invalid_resp.status(),
            StatusCode::FORBIDDEN,
            "invalid request {i} must keep canonical 403"
        );
        let invalid_body = common::assert_error_contract_from_oneshot(invalid_resp, 403).await;
        assert_eq!(
            invalid_body,
            json!({
                "message": "Invalid Application-ID or API key",
                "status": 403
            }),
            "invalid request {i} must keep canonical auth-failure shape"
        );
    }

    for i in 0..2 {
        let valid_req = authed_request(
            Method::POST,
            &format!("/1/indexes/{index_name}/query"),
            &valid_key,
            Some(json!({"query": "test"})),
        );
        let valid_resp = app.clone().oneshot(valid_req).await.unwrap();
        assert_eq!(
            valid_resp.status(),
            StatusCode::OK,
            "valid request {i} should still be accepted after repeated invalid attempts"
        );
    }

    let throttled_req = authed_request(
        Method::POST,
        &format!("/1/indexes/{index_name}/query"),
        &valid_key,
        Some(json!({"query": "test"})),
    );
    let throttled_resp = app.clone().oneshot(throttled_req).await.unwrap();
    assert_eq!(
        throttled_resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "third valid request should hit the key-specific budget of 2/hour"
    );
    let throttled_body = common::assert_error_contract_from_oneshot(throttled_resp, 429).await;
    assert_eq!(
        throttled_body,
        json!({
            "message": "Too many requests per IP per hour",
            "status": 429
        })
    );
}

#[tokio::test]
// Deliberate N/A marker: the constant assertion documents that the current
// auth surface has no session/JWT seam to attack, so this audit row stays a
// recorded no-op rather than a live exploit test.
#[allow(clippy::assertions_on_constants)]
async fn a07_session_fixation_and_jwt_downgrade_are_not_applicable_to_current_auth_surface() {
    // Flapjack's HTTP auth model at HEAD is API-key only (direct keys,
    // PBV1 bearer-carried raw keys, and HMAC secured keys). There is no session
    // cookie state or JWT verifier seam in the auth owners, so session
    // fixation/JWT downgrade are N/A for this stage.
    const HAS_SESSIONS: bool = false;
    assert!(
        !HAS_SESSIONS,
        "A07 variant-c should stay N/A while auth remains key/HMAC based"
    );

    let middleware_src = std::fs::read_to_string("flapjack-http/src/auth/middleware.rs")
        .expect("must be able to read auth middleware owner seam");
    let mod_src = std::fs::read_to_string("flapjack-http/src/auth/mod.rs")
        .expect("must be able to read secured-key owner seam");

    for src in [
        middleware_src.to_ascii_lowercase(),
        mod_src.to_ascii_lowercase(),
    ] {
        assert!(
            !src.contains("jwt"),
            "auth owner files must not expose a JWT verifier surface at HEAD",
        );
        assert!(
            !src.contains("set-cookie") && !src.contains("sessionid"),
            "auth owner files must not expose a session-cookie lifecycle surface at HEAD",
        );
    }
}

#[tokio::test]
async fn a07_admin_key_rotation_never_allows_old_and_new_admin_keys_simultaneously() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let rotate_req = authed_request(Method::POST, "/internal/rotate-admin-key", ADMIN_KEY, None);
    let rotate_resp = app.clone().oneshot(rotate_req).await.unwrap();
    assert_eq!(rotate_resp.status(), StatusCode::OK);
    let rotated_key = common::body_json(rotate_resp).await["key"]
        .as_str()
        .expect("rotation response must include new key")
        .to_string();
    assert_ne!(
        rotated_key, ADMIN_KEY,
        "rotation must produce a distinct admin key"
    );

    let saw_split_brain = Arc::new(AtomicBool::new(false));
    let mut probes = Vec::new();
    for _ in 0..40 {
        let app_clone = app.clone();
        let new_key = rotated_key.clone();
        let saw_split_brain_clone = saw_split_brain.clone();
        probes.push(tokio::spawn(async move {
            let old_resp = app_clone
                .clone()
                .oneshot(authed_request(
                    Method::GET,
                    "/internal/status",
                    ADMIN_KEY,
                    None,
                ))
                .await
                .unwrap();
            let new_resp = app_clone
                .oneshot(authed_request(
                    Method::GET,
                    "/internal/status",
                    &new_key,
                    None,
                ))
                .await
                .unwrap();

            if old_resp.status() == StatusCode::OK && new_resp.status() == StatusCode::OK {
                saw_split_brain_clone.store(true, Ordering::SeqCst);
            }
        }));
    }
    for probe in probes {
        probe.await.unwrap();
    }

    assert!(
        !saw_split_brain.load(Ordering::SeqCst),
        "rotation must never leave a split-brain window where both admin keys authenticate",
    );
}

fn a03_analytics_config(temp_dir: &Path) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: temp_dir.to_path_buf(),
        flush_interval_secs: 60,
        flush_size: 10_000,
        retention_days: 90,
    }
}

fn a03_search_event(index_name: &str, query: &str, analytics_tags: Option<&str>) -> SearchEvent {
    SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: query.to_string(),
        query_id: None,
        index_name: index_name.to_string(),
        nb_hits: 1,
        processing_time_ms: 5,
        user_token: Some("user-1".to_string()),
        user_ip: None,
        filters: None,
        facets: None,
        analytics_tags: analytics_tags.map(str::to_string),
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: Some("US".to_string()),
        region: None,
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    }
}

#[tokio::test]
async fn a03_search_rejects_malformed_filters_instead_of_ignoring_them() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let index_name = "a03_injection_search_idx";

    let mut fields = std::collections::HashMap::new();
    fields.insert(
        "title".to_string(),
        FieldValue::Text("Injection Audit Document".to_string()),
    );
    let add_resp = app
        .clone()
        .oneshot(common::authed_request(
            Method::POST,
            &format!("/1/indexes/{index_name}/batch"),
            ADMIN_KEY,
            Some(json!({
                "requests": [{
                    "action": "addObject",
                    "body": Document {
                        id: "doc-1".to_string(),
                        fields,
                    }
                }]
            })),
        ))
        .await
        .unwrap();
    assert_eq!(add_resp.status(), StatusCode::OK);
    let add_body = common::body_json(add_resp).await;
    common::wait_for_task_local_with_key(&app, common::extract_task_id(&add_body), ADMIN_KEY).await;

    let response = app
        .clone()
        .oneshot(common::authed_request(
            Method::POST,
            &format!("/1/indexes/{index_name}/query"),
            ADMIN_KEY,
            Some(json!({
                "query": "Injection",
                "filters": "category:books OR )"
            })),
        ))
        .await
        .unwrap();

    let status = response.status();
    let body = common::body_json(response).await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "malformed filters must be rejected instead of broadening to an unfiltered search"
    );
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("Filter parse error")),
        "expected filter parse error contract, got: {body}"
    );
}

#[tokio::test]
async fn a03_top_searches_tags_sql_injection_payload_does_not_broaden_results() {
    let temp_dir = TempDir::new().unwrap();
    let config = a03_analytics_config(temp_dir.path());
    let collector = AnalyticsCollector::new(config.clone());

    collector.record_search(a03_search_event("products", "boots", Some("promo")));
    collector.record_search(a03_search_event("products", "sandals", Some("clearance")));
    collector.flush_searches();

    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .top_searches(
            &flapjack::analytics::query::AnalyticsQueryParams {
                index_name: "products",
                start_date: &today,
                end_date: &today,
                limit: 10,
                tags: Some("promo%' OR 1=1 --"),
            },
            false,
            None,
        )
        .await
        .unwrap();

    let searches = result["searches"].as_array().expect("searches array");
    assert!(
        searches.is_empty(),
        "quoted analytics tag payload must not widen results: {searches:?}"
    );
}

// A03 — analytics SQL equality interpolation (`country = '{}'`). Distinct seam from the
// tags/LIKE test above: the country param routes through `sanitize_sql_eq`, the
// single-quote-escaping sanitizer used for equality comparisons. A classic
// `' OR '1'='1` equality-injection payload, if it reached the SQL unescaped, would
// neutralize the country predicate and return every recorded row. With the sanitizer
// the payload is treated as a literal country value that matches nothing, so the
// result set must be empty. Recorded events all use country "US".
#[tokio::test]
async fn a03_top_searches_country_sql_injection_payload_does_not_broaden_results() {
    let temp_dir = TempDir::new().unwrap();
    let config = a03_analytics_config(temp_dir.path());
    let collector = AnalyticsCollector::new(config.clone());

    collector.record_search(a03_search_event("products", "boots", Some("promo")));
    collector.record_search(a03_search_event("products", "sandals", Some("clearance")));
    collector.flush_searches();

    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .top_searches(
            &flapjack::analytics::query::AnalyticsQueryParams {
                index_name: "products",
                start_date: &today,
                end_date: &today,
                limit: 10,
                tags: None,
            },
            false,
            // Equality-injection payload: a real injection would OR-true the predicate
            // and return both "US" rows; sanitized, it is a literal non-matching country.
            Some("ZZ' OR '1'='1"),
        )
        .await
        .unwrap();

    let searches = result["searches"].as_array().expect("searches array");
    assert!(
        searches.is_empty(),
        "quoted analytics country payload must not widen results: {searches:?}"
    );
}

#[tokio::test]
async fn a04_auth_enabled_routes_fail_closed_without_credentials() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method(Method::POST)
                .uri("/1/indexes/a04_protected/query")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(json!({"query": "test"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = common::assert_error_contract_from_oneshot(response, 403).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        }),
        "auth-enabled bootstrap must fail closed when credentials are absent"
    );
}

#[test]
fn a04_production_bootstrap_rejects_missing_blank_or_short_admin_key() {
    assert_eq!(
        validate_startup_auth_policy("production", false, None, "127.0.0.1:7700", false),
        Err(StartupAuthValidationError::MissingAdminKeyInProduction)
    );
    assert_eq!(
        validate_startup_auth_policy("production", false, Some("   "), "127.0.0.1:7700", false),
        Err(StartupAuthValidationError::MissingAdminKeyInProduction)
    );
    assert_eq!(
        validate_startup_auth_policy("production", false, Some("short"), "127.0.0.1:7700", false),
        Err(StartupAuthValidationError::AdminKeyTooShortInProduction)
    );
    assert_eq!(
        validate_startup_auth_policy(
            "production",
            true,
            Some("1234567890abcdef"),
            "0.0.0.0:7700",
            true
        ),
        Err(StartupAuthValidationError::NoAuthInProduction)
    );
    for variant in ["Production", " production ", "PRODUCTION"] {
        assert_eq!(
            validate_startup_auth_policy(variant, false, None, "127.0.0.1:7700", false),
            Err(StartupAuthValidationError::MissingAdminKeyInProduction),
            "variant {variant:?} must not bypass production bootstrap policy"
        );
        assert_eq!(
            validate_startup_auth_policy(
                variant,
                true,
                Some("1234567890abcdef"),
                "0.0.0.0:7700",
                true
            ),
            Err(StartupAuthValidationError::NoAuthInProduction),
            "variant {variant:?} must keep no-auth forbidden in production"
        );
    }
    assert_eq!(
        validate_startup_auth_policy(
            "production",
            false,
            Some("1234567890abcdef"),
            "0.0.0.0:7700",
            false
        ),
        Ok(StartupAuthValidationOutcome::Accepted)
    );
}

#[test]
fn a04_unset_allowed_origins_defaults_to_loopback_only_contract() {
    assert_eq!(cors_origins_from_value(None), CorsMode::LoopbackOnly);
    assert_eq!(cors_origins_from_value(Some("")), CorsMode::LoopbackOnly);
}

#[tokio::test]
async fn a04_default_cors_blocks_non_loopback_browser_origins() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method(Method::OPTIONS)
                .uri("/1/indexes")
                .header("origin", "https://app.example.com")
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "content-type")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none(),
        "default CORS mode must not allow non-loopback browser origins without FLAPJACK_ALLOWED_ORIGINS"
    );
}

#[tokio::test]
async fn a04_shipped_default_search_key_has_bounded_rate_limit() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let list_resp = app
        .clone()
        .oneshot(common::authed_request(
            Method::GET,
            "/1/keys",
            ADMIN_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_body = common::body_json(list_resp).await;
    let keys = list_body["keys"]
        .as_array()
        .expect("list keys response must include keys array");
    let search_key = keys
        .iter()
        .find(|entry| {
            entry["description"]
                .as_str()
                .is_some_and(|description| description == "Default Search API Key")
        })
        .expect("default search key must exist");

    let max_queries = search_key["maxQueriesPerIPPerHour"]
        .as_i64()
        .expect("default search key maxQueriesPerIPPerHour must be an integer");
    assert!(
        max_queries > 0,
        "default search key must not ship with an unbounded per-IP hourly query limit"
    );
}

#[tokio::test]
async fn a05_invalid_credentials_error_contract_has_no_metadata_leaks() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let response = app
        .clone()
        .oneshot(common::authed_request(
            Method::POST,
            "/1/indexes/a05_missing/query",
            "definitely-not-a-valid-key",
            Some(json!({"query": "test"})),
        ))
        .await
        .unwrap();

    let status = response.status();
    let body = common::assert_error_contract_from_oneshot(response, 403).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["message"], json!("Invalid Application-ID or API key"));
    assert_eq!(body["status"], json!(403));
    assert_eq!(body.as_object().map(|obj| obj.len()), Some(2));
}

#[tokio::test]
async fn a05_public_health_uses_explicit_metadata_denylist() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method(Method::GET)
                .uri("/health")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = common::body_json(response).await;
    let actual_keys: HashSet<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let expected_keys: HashSet<&str> = [
        "status",
        "version",
        "build",
        "uptime_secs",
        "capabilities",
        "active_writers",
        "max_concurrent_writers",
        "facet_cache_entries",
        "facet_cache_cap",
        "heap_allocated_mb",
        "system_limit_mb",
        "pressure_level",
        "allocator",
        "tenants_loaded",
    ]
    .into_iter()
    .collect();

    assert_eq!(body["status"], json!("ok"));
    assert_eq!(
        body["build"],
        json!({
            "schemaVersion": flapjack::build_info().schema_version,
            "version": flapjack::build_info().version,
            "profile": flapjack::build_info().profile,
            "apiProfile": "full",
            "supportedApiProfiles": [
                "full",
                "paid_beta_v1",
                "paid_beta_v3",
                "paid_beta_v4",
                "paid_beta_v5"
            ],
            "capabilities": flapjack::build_info().capabilities,
        })
    );
    assert_eq!(body["version"], body["build"]["version"]);
    assert!(body["active_writers"].is_number());
    assert!(body["max_concurrent_writers"].is_number());
    assert!(body["facet_cache_entries"].is_number());
    assert!(body["facet_cache_cap"].is_number());
    assert!(body["heap_allocated_mb"].is_number());
    assert!(body["system_limit_mb"].is_number());
    assert!(body["pressure_level"].is_string());
    assert!(body["allocator"].is_string());
    assert!(body["uptime_secs"].is_number());
    assert_eq!(body["capabilities"], body["build"]["capabilities"]);
    assert!(body["tenants_loaded"].is_number());
    assert_eq!(
        actual_keys, expected_keys,
        "public /health must keep an exact allowlist contract"
    );
    assert_no_migration_capability_spellings(&body["build"]);
    assert_no_migration_capability_spellings(&body["capabilities"]);
    for field in [
        "build_profile",
        "profile",
        "target",
        "features",
        "workspaceDigest",
    ] {
        assert!(
            body.get(field).is_none(),
            "public /health must not expose top-level build metadata: {field}"
        );
    }
}

fn assert_no_migration_capability_spellings(value: &serde_json::Value) {
    let serialized = value.to_string();
    for spelling in ["algolia_migration_v1", "algoliaMigrationV1"] {
        assert!(
            !serialized.contains(spelling),
            "serialized build capability payload must not include {spelling}: {serialized}"
        );
    }
}

#[tokio::test]
async fn a05_internal_route_unauthorized_response_has_canonical_shape_only() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method(Method::GET)
                .uri("/internal/storage")
                .header("x-algolia-api-key", "invalid-admin-key")
                .header("x-algolia-application-id", "tenant-a05")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = common::assert_error_contract_from_oneshot(response, 403).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["message"], json!("Invalid Application-ID or API key"));
    assert_eq!(body["status"], json!(403));
    assert_eq!(body.as_object().map(|obj| obj.len()), Some(2));
}

fn a06_engine_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn a06_run_cargo_command(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("cargo")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "failed to run `cargo {}` from {}: {error}",
                args.join(" "),
                cwd.display()
            )
        })
}

#[test]
fn a06_vulnerable_fixture_fails_cargo_audit() {
    let fixture_dir = a06_engine_root().join("tests/fixtures/a06_vulnerable");
    let output = a06_run_cargo_command(&fixture_dir, &["audit", "--deny", "warnings"]);

    assert!(
        !output.status.success(),
        "vulnerable fixture must fail `cargo audit --deny warnings`; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("RUSTSEC-")
            || String::from_utf8_lossy(&output.stderr).contains("RUSTSEC-"),
        "fixture audit failure must be advisory-driven, not tooling noise; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a06_vulnerable_fixture_fails_cargo_deny_advisories() {
    let fixture_dir = a06_engine_root().join("tests/fixtures/a06_vulnerable");
    let output = a06_run_cargo_command(&fixture_dir, &["deny", "check", "advisories"]);

    assert!(
        !output.status.success(),
        "vulnerable fixture must fail `cargo deny check advisories`; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("advis")
            || String::from_utf8_lossy(&output.stderr).contains("advis"),
        "fixture deny failure must include advisory output; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a06_workspace_passes_cargo_audit_and_cargo_deny() {
    let engine_dir = a06_engine_root();

    let audit_output = a06_run_cargo_command(&engine_dir, &["audit", "--deny", "warnings"]);
    assert!(
        audit_output.status.success(),
        "workspace must pass `cargo audit --deny warnings`; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&audit_output.stdout),
        String::from_utf8_lossy(&audit_output.stderr)
    );

    let deny_output = a06_run_cargo_command(&engine_dir, &["deny", "check", "advisories"]);
    assert!(
        deny_output.status.success(),
        "workspace must pass `cargo deny check advisories`; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&deny_output.stdout),
        String::from_utf8_lossy(&deny_output.stderr)
    );

    // The engine ships under Elastic-2.0, so a copyleft dependency reaching
    // beyond file scope would conflict with the license and with offering
    // commercial terms. Goes red when a dependency arrives under a license
    // absent from the `[licenses] allow` list in engine/deny.toml, or when a
    // workspace crate ships with no `license` field at all.
    let license_output = a06_run_cargo_command(&engine_dir, &["deny", "check", "licenses"]);
    assert!(
        license_output.status.success(),
        "workspace must pass `cargo deny check licenses`; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&license_output.stdout),
        String::from_utf8_lossy(&license_output.stderr)
    );
}

#[derive(Clone, Default)]
struct A09LogBuffer {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl A09LogBuffer {
    fn contents(&self) -> String {
        String::from_utf8(self.buffer.lock().unwrap().clone()).unwrap()
    }
}

struct A09LogWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl io::Write for A09LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for A09LogBuffer {
    type Writer = A09LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        A09LogWriter {
            buffer: Arc::clone(&self.buffer),
        }
    }
}

fn a09_capture_dispatch(log_buffer: A09LogBuffer) -> tracing::Dispatch {
    tracing::Dispatch::new(
        tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .without_time()
                .with_ansi(false)
                .with_target(false)
                .with_writer(log_buffer),
        ),
    )
}

fn a09_matching_security_event_line<'a>(
    logs: &'a str,
    event: &str,
    action: &str,
    target: &str,
    outcome: &str,
    additional_fields: &[&str],
) -> Option<&'a str> {
    let canonical_fields = [
        format!("event=\"{event}\""),
        "actor=\"admin_api_key\"".to_string(),
        format!("action=\"{action}\""),
        format!("target=\"{target}\""),
        format!("outcome=\"{outcome}\""),
    ];

    logs.lines().find(|line| {
        canonical_fields.iter().all(|field| line.contains(field))
            && additional_fields.iter().all(|field| line.contains(field))
    })
}

fn a09_assert_security_event<'a>(
    logs: &'a str,
    event: &str,
    action: &str,
    target: &str,
    outcome: &str,
    additional_fields: &[&str],
) -> &'a str {
    a09_matching_security_event_line(
        logs,
        event,
        action,
        target,
        outcome,
        additional_fields,
    )
    .unwrap_or_else(|| {
        panic!(
            "no single audit record contains the canonical fields for event `{event}`, action `{action}`, target `{target}`, and outcome `{outcome}`; logs={logs}"
        )
    })
}

fn a09_api_key_target(key_value: &str) -> String {
    let digest = Sha256::digest(key_value.as_bytes());
    format!("api_key:{}", hex::encode(&digest[..8]))
}

fn a09_assert_no_forbidden(logs: &str, forbidden: &[&str]) {
    for value in forbidden {
        assert!(
            !value.is_empty(),
            "forbidden leak specimen must be nonempty before testing absence"
        );
        assert!(
            !logs.contains(value),
            "security audit logs must not leak sensitive material `{value}`; logs={logs}"
        );
    }
}

fn a09_assert_nonempty_key(body: &serde_json::Value, context: &str) -> String {
    let key = body["key"]
        .as_str()
        .unwrap_or_else(|| panic!("{context} response must include plaintext key"))
        .to_string();
    assert!(
        !key.is_empty(),
        "{context} response key must be nonempty before leak assertions"
    );
    key
}

fn a09_nonempty_secret_specimen(value: &str, context: &str) -> String {
    let specimen = value.to_string();
    assert!(
        !specimen.is_empty(),
        "{context} must be nonempty before leak assertions"
    );
    specimen
}

fn a09_authed_body_request(
    method: Method,
    uri: &str,
    key: &str,
    content_type: &str,
    body: Body,
) -> axum::http::Request<Body> {
    let mut req = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("x-algolia-api-key", key)
        .header("x-algolia-application-id", "test")
        .header("content-type", content_type)
        .body(body)
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
    req
}

async fn a09_seed_single_doc(app: &axum::Router, index_name: &str) {
    common::seed_docs(
        app,
        index_name,
        ADMIN_KEY,
        vec![json!({"objectID": "doc-1", "name": "alpha"})],
    )
    .await;
}

async fn a09_snapshot_bytes(app: &axum::Router, index_name: &str) -> Vec<u8> {
    a09_seed_single_doc(app, index_name).await;
    let response = app
        .clone()
        .oneshot(authed_request(
            Method::GET,
            &format!("/1/indexes/{index_name}/export"),
            ADMIN_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "snapshot export fixture must succeed"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 100_000_000)
        .await
        .expect("snapshot export body must be readable")
        .to_vec();
    assert!(
        !bytes.is_empty(),
        "snapshot export fixture must be nonempty before import audit assertions"
    );
    bytes
}

async fn a09_captured_json_response(
    app: &axum::Router,
    dispatch: &tracing::Dispatch,
    request: Request<Body>,
) -> (StatusCode, serde_json::Value) {
    let response = {
        let _guard = tracing::dispatcher::set_default(dispatch);
        app.clone().oneshot(request).await.unwrap()
    };
    let status = response.status();
    (status, common::body_json(response).await)
}

fn a09_capture_s3_env() -> A10EnvRestore {
    A10EnvRestore::capture(&[
        "FLAPJACK_S3_BUCKET",
        "FLAPJACK_S3_REGION",
        "FLAPJACK_S3_ENDPOINT",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
    ])
}

fn a09_configure_s3_env(s3: &MockServer) {
    std::env::set_var("FLAPJACK_S3_BUCKET", "snapshot-bucket");
    std::env::set_var("FLAPJACK_S3_REGION", "us-east-1");
    std::env::set_var("FLAPJACK_S3_ENDPOINT", s3.uri());
    std::env::set_var("AWS_ACCESS_KEY_ID", "test-access-key");
    std::env::set_var("AWS_SECRET_ACCESS_KEY", "test-secret-key");
}

async fn a09_mount_s3_snapshot(s3: &MockServer, key: &str, body: Vec<u8>) {
    Mock::given(method("GET"))
        .and(path(format!("/snapshot-bucket/{key}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(s3)
        .await;
}

async fn a09_restore_s3_response(
    app: &axum::Router,
    dispatch: &tracing::Dispatch,
    index_name: &str,
    key: &str,
) -> (StatusCode, serde_json::Value) {
    a09_captured_json_response(
        app,
        dispatch,
        authed_request(
            Method::POST,
            &format!("/1/indexes/{index_name}/restore"),
            ADMIN_KEY,
            Some(json!({ "key": key })),
        ),
    )
    .await
}

fn a09_assert_control_index_events(logs: &str, escaped_index_name: &str) {
    let index_target = format!("index:{escaped_index_name}");
    let settings_target = format!("{index_target}:settings");
    let snapshot_target = format!("{index_target}:snapshot");
    for (action, target, outcome) in [
        ("set_settings", settings_target.as_str(), "success"),
        ("import_snapshot", snapshot_target.as_str(), "failure"),
        (
            "restore_snapshot_from_s3",
            snapshot_target.as_str(),
            "failure",
        ),
        ("delete_index", index_target.as_str(), "success"),
    ] {
        let event_line = a09_assert_security_event(
            logs,
            "security_audit_admin_action",
            action,
            target,
            outcome,
            &[],
        );
        assert!(
            !event_line.contains('\n') && !event_line.contains('\r') && !event_line.contains('\t'),
            "audit target for {action} must stay single-line; line={event_line:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a09_failed_direct_key_auth_emits_audit_event_without_secret_or_query_leaks() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let direct_key_probe = "fj_search_a09_direct_invalid_key_probe";
    let query_probe = "a09_query_probe_direct";
    let filter_probe = "a09_filter_probe_direct";
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::POST,
                "/1/indexes/a09_products/query",
                direct_key_probe,
                Some(json!({
                    "query": query_probe,
                    "filters": format!("category:{filter_probe}")
                })),
            ))
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _body = common::assert_error_contract_from_oneshot(response, 403).await;
    let logs = log_buffer.contents();

    assert!(
        logs.contains("security event: auth failure")
            && logs.contains("auth_attempt_type=\"direct\""),
        "direct-key auth failures must emit a detectable A09 security event; logs={logs}"
    );
    for forbidden in [
        direct_key_probe,
        "x-algolia-api-key",
        query_probe,
        filter_probe,
    ] {
        assert!(
            !logs.contains(forbidden),
            "logs must not leak sensitive direct-key auth material `{forbidden}`; logs={logs}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a09_non_prefixed_admin_key_failed_attempt_logs_direct_auth_type() {
    let non_prefixed_admin_key = "stage10customadminkey";
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(non_prefixed_admin_key));
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::GET)
                    .uri("/internal/status")
                    .header("x-algolia-api-key", non_prefixed_admin_key)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _body = common::assert_error_contract_from_oneshot(response, 403).await;
    let logs = log_buffer.contents();

    assert!(
        logs.contains("security event: auth failure")
            && logs.contains("auth_attempt_type=\"direct\"")
            && logs.contains("reason=\"application_id_missing\""),
        "non-prefixed direct admin key failures must log as direct auth attempts; logs={logs}"
    );
    for forbidden in [non_prefixed_admin_key, "x-algolia-api-key"] {
        assert!(
            !logs.contains(forbidden),
            "logs must not leak direct-key material `{forbidden}`; logs={logs}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a09_failed_secured_key_auth_emits_audit_event_without_token_payload_or_query_leaks() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let token_payload_probe = "a09_secured_payload_probe";

    let create_resp = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            "/1/keys",
            ADMIN_KEY,
            Some(json!({
                "acl": ["search"],
                "indexes": ["a09_products"],
                "description": "a09 secured-key parent"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);
    let parent_key = common::body_json(create_resp).await["key"]
        .as_str()
        .expect("create key response must include plaintext key")
        .to_string();

    let secured_params =
        format!("filters=category%3A{token_payload_probe}&restrictIndices=a09_products");
    let secured_key = flapjack_http::auth::generate_secured_api_key(&parent_key, &secured_params);

    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());
    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::POST)
                    .uri("/1/indexes/a09_products/query")
                    .header("x-algolia-api-key", &secured_key)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"query": "a09_query_probe_secured"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _body = common::assert_error_contract_from_oneshot(response, 403).await;
    let logs = log_buffer.contents();

    assert!(
        logs.contains("security event: auth failure")
            && logs.contains("auth_attempt_type=\"secured\""),
        "secured-key auth failures must emit a detectable A09 security event; logs={logs}"
    );
    for forbidden in [
        "x-algolia-api-key",
        token_payload_probe,
        "a09_query_probe_secured",
    ] {
        assert!(
            !logs.contains(forbidden),
            "logs must not leak secured-key payload/query material `{forbidden}`; logs={logs}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a09_structurally_secured_shaped_invalid_key_does_not_log_secured_attempt_type() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let shaped_invalid_token = base64::engine::general_purpose::STANDARD.encode(format!(
        "{}{}",
        "a".repeat(64),
        "attacker_controlled_payload"
    ));
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::POST,
                "/1/indexes/a09_products/query",
                &shaped_invalid_token,
                Some(json!({"query": "shape_probe"})),
            ))
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _body = common::assert_error_contract_from_oneshot(response, 403).await;
    let logs = log_buffer.contents();

    assert!(
        logs.contains("security event: auth failure")
            && logs.contains("auth_attempt_type=\"direct\""),
        "invalid keys that only mimic secured-token structure must not log as secured; logs={logs}"
    );
    assert!(
        !logs.contains("auth_attempt_type=\"secured\""),
        "structure-only invalid keys must never be classified as secured; logs={logs}"
    );
    for forbidden in [shaped_invalid_token.as_str(), "x-algolia-api-key"] {
        assert!(
            !logs.contains(forbidden),
            "logs must not leak shaped invalid token material `{forbidden}`; logs={logs}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a09_rotate_admin_key_success_emits_audit_event_without_key_leaks() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let rotate_resp = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::POST,
                "/internal/rotate-admin-key",
                ADMIN_KEY,
                None,
            ))
            .await
            .unwrap()
    };

    assert_eq!(rotate_resp.status(), StatusCode::OK);
    let rotated_key = common::body_json(rotate_resp).await["key"]
        .as_str()
        .expect("rotation response must include new admin key")
        .to_string();
    let logs = log_buffer.contents();

    assert!(
        logs.contains("security event: admin action")
            && logs.contains("admin_action=\"rotate_admin_key\""),
        "successful rotate-admin-key calls must emit a detectable A09 admin-action event; logs={logs}"
    );
    for forbidden in [ADMIN_KEY, rotated_key.as_str(), "x-algolia-api-key"] {
        assert!(
            !logs.contains(forbidden),
            "admin action logs must not leak secret key material `{forbidden}`; logs={logs}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a09_create_key_emits_audit_event_without_plaintext_key() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::POST,
                "/1/keys",
                ADMIN_KEY,
                Some(json!({
                    "acl": ["search"],
                    "indexes": ["a09_create_key_index"],
                    "description": "a09 create key audit"
                })),
            ))
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::OK);
    let body = common::body_json(response).await;
    let created_key = a09_assert_nonempty_key(&body, "create key");
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "create_key",
        &a09_api_key_target(&created_key),
        "success",
        &[],
    );
    a09_assert_no_forbidden(&logs, &[created_key.as_str(), "x-algolia-api-key"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a09_update_key_emits_audit_event_without_key_in_target() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let create_resp = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            "/1/keys",
            ADMIN_KEY,
            Some(json!({
                "acl": ["search"],
                "indexes": ["a09_update_key_index"],
                "description": "a09 update key audit before"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);
    let key_value = a09_assert_nonempty_key(&common::body_json(create_resp).await, "create key");

    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());
    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::PUT,
                &format!("/1/keys/{key_value}"),
                ADMIN_KEY,
                Some(json!({
                    "acl": ["search"],
                    "indexes": ["a09_update_key_index"],
                    "description": "a09 update key audit after"
                })),
            ))
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::OK);
    let body = common::body_json(response).await;
    assert_eq!(
        body["key"].as_str(),
        Some(key_value.as_str()),
        "update response must identify the path key before log non-leak assertion"
    );
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "update_key",
        &a09_api_key_target(&key_value),
        "success",
        &[],
    );

    let missing_key = a09_nonempty_secret_specimen("a09_update_missing_key_probe", "missing key");
    let missing_response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::PUT,
                &format!("/1/keys/{missing_key}"),
                ADMIN_KEY,
                Some(json!({
                    "acl": ["search"],
                    "indexes": ["a09_update_key_index"],
                    "description": "a09 update missing key audit"
                })),
            ))
            .await
            .unwrap()
    };

    assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
    let _body = common::assert_error_contract_from_oneshot(missing_response, 404).await;
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "update_key",
        &a09_api_key_target(&missing_key),
        "failure",
        &["reason=\"key_not_found\""],
    );
    a09_assert_no_forbidden(
        &logs,
        &[
            key_value.as_str(),
            missing_key.as_str(),
            "x-algolia-api-key",
        ],
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a09_generate_secured_key_emits_audit_event_without_parent_or_generated_key_or_restrictions(
) {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let create_resp = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            "/1/keys",
            ADMIN_KEY,
            Some(json!({
                "acl": ["search"],
                "indexes": ["a09_secured_key_index"],
                "description": "a09 secured-key parent"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);
    let parent_key =
        a09_assert_nonempty_key(&common::body_json(create_resp).await, "secured-key parent");
    let filter_probe = "a09_generate_secured_filter_probe";
    let user_token_probe = "a09_generate_secured_user_token_probe";
    let source_probe = "192.0.2.42";
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::POST,
                "/1/keys/generateSecuredApiKey",
                ADMIN_KEY,
                Some(json!({
                    "parentApiKey": parent_key,
                    "restrictions": {
                        "filters": format!("tenant:{filter_probe}"),
                        "userToken": user_token_probe,
                        "restrictSources": source_probe
                    }
                })),
            ))
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::OK);
    let body = common::body_json(response).await;
    let generated_key = body["securedApiKey"]
        .as_str()
        .expect("generate secured key response must include securedApiKey")
        .to_string();
    assert!(
        !generated_key.is_empty(),
        "securedApiKey must be nonempty before leak assertions"
    );
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "generate_secured_key",
        &a09_api_key_target(&parent_key),
        "success",
        &[],
    );
    a09_assert_no_forbidden(
        &logs,
        &[
            parent_key.as_str(),
            generated_key.as_str(),
            filter_probe,
            user_token_probe,
            source_probe,
            "x-algolia-api-key",
        ],
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a09_delete_key_failure_outcomes_are_distinguishable() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let unknown_key =
        a09_nonempty_secret_specimen("a09_unknown_delete_key_probe", "unknown delete key");
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let admin_response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::DELETE,
                &format!("/1/keys/{ADMIN_KEY}"),
                ADMIN_KEY,
                None,
            ))
            .await
            .unwrap()
    };
    assert_eq!(admin_response.status(), StatusCode::FORBIDDEN);
    let _body = common::assert_error_contract_from_oneshot(admin_response, 403).await;

    let unknown_response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::DELETE,
                &format!("/1/keys/{unknown_key}"),
                ADMIN_KEY,
                None,
            ))
            .await
            .unwrap()
    };
    assert_eq!(unknown_response.status(), StatusCode::NOT_FOUND);
    let _body = common::assert_error_contract_from_oneshot(unknown_response, 404).await;
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "delete_key",
        &a09_api_key_target(ADMIN_KEY),
        "failure",
        &["reason=\"cannot_delete_admin_key\""],
    );
    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "delete_key",
        &a09_api_key_target(&unknown_key),
        "failure",
        &["reason=\"key_not_found\""],
    );
    a09_assert_no_forbidden(
        &logs,
        &[ADMIN_KEY, unknown_key.as_str(), "x-algolia-api-key"],
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a09_restore_key_emits_audit_event() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let create_resp = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            "/1/keys",
            ADMIN_KEY,
            Some(json!({
                "acl": ["search"],
                "indexes": ["a09_restore_key_index"],
                "description": "a09 restore key audit"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);
    let key_value = a09_assert_nonempty_key(&common::body_json(create_resp).await, "create key");

    let delete_resp = app
        .clone()
        .oneshot(authed_request(
            Method::DELETE,
            &format!("/1/keys/{key_value}"),
            ADMIN_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(delete_resp.status(), StatusCode::OK);

    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());
    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::POST,
                &format!("/1/keys/{key_value}/restore"),
                ADMIN_KEY,
                None,
            ))
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::OK);
    let body = common::body_json(response).await;
    assert_eq!(
        body["key"].as_str(),
        Some(key_value.as_str()),
        "restore response must identify the raw path key before log non-leak assertion"
    );
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "restore_key",
        &a09_api_key_target(&key_value),
        "success",
        &[],
    );
    a09_assert_no_forbidden(&logs, &[key_value.as_str(), "x-algolia-api-key"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a09_restore_missing_key_emits_failure_audit_event() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let key_value =
        a09_nonempty_secret_specimen("a09_restore_missing_key_probe", "restore missing key");
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::POST,
                &format!("/1/keys/{key_value}/restore"),
                ADMIN_KEY,
                None,
            ))
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let _body = common::assert_error_contract_from_oneshot(response, 404).await;
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "restore_key",
        &a09_api_key_target(&key_value),
        "failure",
        &["reason=\"key_not_found\""],
    );
    a09_assert_no_forbidden(&logs, &[key_value.as_str(), "x-algolia-api-key"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a09_delete_index_emits_audit_event_on_success_and_failure() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let index_name = "a09_delete_index";
    a09_seed_single_doc(&app, index_name).await;
    let missing_index = "a09_delete_index_missing";
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let success = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::DELETE,
                &format!("/1/indexes/{index_name}"),
                ADMIN_KEY,
                None,
            ))
            .await
            .unwrap()
    };
    assert_eq!(success.status(), StatusCode::OK);
    let success_body = common::body_json(success).await;
    common::wait_for_task_local_with_key(&app, common::extract_task_id(&success_body), ADMIN_KEY)
        .await;

    let failure = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::DELETE,
                &format!("/1/indexes/{missing_index}"),
                ADMIN_KEY,
                None,
            ))
            .await
            .unwrap()
    };
    assert_eq!(failure.status(), StatusCode::NOT_FOUND);
    let _body = common::assert_error_contract_from_oneshot(failure, 404).await;
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "delete_index",
        &format!("index:{index_name}"),
        "success",
        &[],
    );
    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "delete_index",
        &format!("index:{missing_index}"),
        "failure",
        &[],
    );
    a09_assert_no_forbidden(&logs, &["x-algolia-api-key"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a09_set_settings_emits_audit_event_naming_fields_not_values() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let index_name = "a09_settings_index";
    a09_seed_single_doc(&app, index_name).await;
    let probe_token = "a09_settings_nested_probe_token";
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::PUT,
                &format!("/1/indexes/{index_name}/settings"),
                ADMIN_KEY,
                Some(json!({
                    "userData": {
                        "nested": {
                            "probe": probe_token
                        }
                    }
                })),
            ))
            .await
            .unwrap()
    };

    assert!(
        response.status().is_success(),
        "settings update must return 2xx before audit log assertions"
    );
    let body = common::body_json(response).await;
    common::wait_for_task_local_with_key(&app, common::extract_task_id(&body), ADMIN_KEY).await;
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "set_settings",
        &format!("index:{index_name}:settings"),
        "success",
        &["changed_fields=\"userData\""],
    );
    a09_assert_no_forbidden(&logs, &[probe_token, "x-algolia-api-key"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a09_set_settings_post_save_failure_does_not_emit_success_audit() {
    let (app, tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let index_name = "a09_settings_post_save_failure";
    let replica_name = "a09_settings_blocked_replica";
    a09_seed_single_doc(&app, index_name).await;
    std::fs::write(tmp.path().join(replica_name), b"not a directory")
        .expect("replica collision fixture must be written before settings request");
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::PUT,
                &format!("/1/indexes/{index_name}/settings"),
                ADMIN_KEY,
                Some(json!({
                    "replicas": [format!("virtual({replica_name})")],
                    "userData": {"probe": "a09-post-save-failure-value"}
                })),
            ))
            .await
            .unwrap()
    };

    let status = response.status();
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let _body = common::body_json(response).await;
    assert!(
        tmp.path().join(index_name).join("settings.json").exists(),
        "fixture must reach the post-save failure seam before returning {status}"
    );
    let logs = log_buffer.contents();
    assert!(
        a09_matching_security_event_line(
            &logs,
            "security_audit_admin_action",
            "set_settings",
            &format!("index:{index_name}:settings"),
            "success",
            &[],
        )
        .is_none(),
        "failed set_settings responses must not emit success audit events; logs={logs}"
    );
    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "set_settings",
        &format!("index:{index_name}:settings"),
        "failure",
        &["changed_fields=\"userData,replicas\""],
    );
    a09_assert_no_forbidden(&logs, &["a09-post-save-failure-value", "x-algolia-api-key"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a09_import_snapshot_emits_audit_event_on_success_and_failure() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let source_index = "a09_snapshot_source";
    let invalid_index = "a09_snapshot_invalid";
    let snapshot_bytes = a09_snapshot_bytes(&app, source_index).await;
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let success = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(a09_authed_body_request(
                Method::POST,
                &format!("/1/indexes/{source_index}/import"),
                ADMIN_KEY,
                "application/gzip",
                Body::from(snapshot_bytes),
            ))
            .await
            .unwrap()
    };
    assert_eq!(success.status(), StatusCode::OK);
    let success_body = common::body_json(success).await;
    assert_eq!(success_body["status"], json!("imported"));

    let failure = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(a09_authed_body_request(
                Method::POST,
                &format!("/1/indexes/{invalid_index}/import"),
                ADMIN_KEY,
                "application/gzip",
                Body::from("a09 invalid snapshot bytes".as_bytes().to_vec()),
            ))
            .await
            .unwrap()
    };
    assert_eq!(failure.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let _body = common::body_json(failure).await;
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "import_snapshot",
        &format!("index:{source_index}:snapshot"),
        "success",
        &[],
    );
    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "import_snapshot",
        &format!("index:{invalid_index}:snapshot"),
        "failure",
        &[],
    );
    a09_assert_no_forbidden(&logs, &["x-algolia-api-key"]);
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::await_holding_lock)]
async fn a09_restore_snapshot_from_s3_emits_audit_event_without_s3_key() {
    let _env_guard = a10_env_lock().lock().unwrap();
    let _env_restore = a09_capture_s3_env();
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let success_index = "a09_s3_restore_success";
    let failure_index = "a09_s3_restore_failure";
    let missing_index = "a09_s3_restore_missing";
    let rejected_index = "a09_s3_restore_rejected";
    let success_key = format!("snapshots/{success_index}/a09-success.tar.gz");
    let failure_key = format!("snapshots/{failure_index}/a09-failure.tar.gz");
    let missing_key = format!("snapshots/{missing_index}/a09-missing-private-marker.tar.gz");
    let rejected_key = "snapshots/another-index/a09-rejected-private-marker.tar.gz";
    let snapshot_bytes = a09_snapshot_bytes(&app, success_index).await;
    let s3 = MockServer::start().await;
    a09_mount_s3_snapshot(&s3, &success_key, snapshot_bytes).await;
    a09_mount_s3_snapshot(&s3, &failure_key, b"a09 invalid s3 snapshot bytes".to_vec()).await;
    a09_configure_s3_env(&s3);
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let (success_status, success_body) =
        a09_restore_s3_response(&app, &dispatch, success_index, &success_key).await;
    assert_eq!(success_status, StatusCode::OK);
    assert_eq!(success_body["status"], json!("restored"));

    let (failure_status, _) =
        a09_restore_s3_response(&app, &dispatch, failure_index, &failure_key).await;
    assert_eq!(failure_status, StatusCode::INTERNAL_SERVER_ERROR);

    let (missing_status, missing_body) =
        a09_restore_s3_response(&app, &dispatch, missing_index, &missing_key).await;
    assert_eq!(missing_status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(missing_body["message"], json!("Internal server error"));

    let (rejected_status, rejected_body) =
        a09_restore_s3_response(&app, &dispatch, rejected_index, rejected_key).await;
    assert_eq!(rejected_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        rejected_body["message"],
        json!("key must reference a snapshot for the requested index")
    );
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "restore_snapshot_from_s3",
        &format!("index:{success_index}:snapshot"),
        "success",
        &[],
    );
    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "restore_snapshot_from_s3",
        &format!("index:{failure_index}:snapshot"),
        "failure",
        &[],
    );
    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "restore_snapshot_from_s3",
        &format!("index:{missing_index}:snapshot"),
        "failure",
        &[],
    );
    a09_assert_security_event(
        &logs,
        "security_audit_admin_action",
        "restore_snapshot_from_s3",
        &format!("index:{rejected_index}:snapshot"),
        "failure",
        &[],
    );
    a09_assert_no_forbidden(
        &logs,
        &[
            success_key.as_str(),
            failure_key.as_str(),
            missing_key.as_str(),
            rejected_key,
        ],
    );
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::await_holding_lock)]
async fn a09_control_character_index_name_keeps_mutation_audit_events() {
    let _env_guard = a10_env_lock().lock().unwrap();
    let _env_restore = a09_capture_s3_env();
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let raw_index_name = "a09_control\tindex";
    let encoded_index_name = "a09_control%09index";
    let audit_log_index_name = "a09_control\\\\tindex";
    let snapshot_source = "a09_control_snapshot_source";
    let snapshot_bytes = a09_snapshot_bytes(&app, snapshot_source).await;
    let s3_key = "snapshots/a09_control\tindex/success.tar.gz";
    let s3 = MockServer::start().await;
    a09_mount_s3_snapshot(
        &s3,
        &format!("snapshots/{encoded_index_name}/success.tar.gz"),
        snapshot_bytes.clone(),
    )
    .await;
    a09_configure_s3_env(&s3);
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let (settings_status, settings_body) = a09_captured_json_response(
        &app,
        &dispatch,
        authed_request(
            Method::PUT,
            &format!("/1/indexes/{encoded_index_name}/settings"),
            ADMIN_KEY,
            Some(json!({"userData": {"probe": "a09-control-settings-value"}})),
        ),
    )
    .await;
    assert_eq!(
        settings_status,
        StatusCode::OK,
        "settings body={}",
        settings_body
    );
    common::wait_for_task_local_with_key(&app, common::extract_task_id(&settings_body), ADMIN_KEY)
        .await;

    let (import_status, _) = a09_captured_json_response(
        &app,
        &dispatch,
        a09_authed_body_request(
            Method::POST,
            &format!("/1/indexes/{encoded_index_name}/import"),
            ADMIN_KEY,
            "application/gzip",
            Body::from(snapshot_bytes),
        ),
    )
    .await;
    assert_eq!(import_status, StatusCode::INTERNAL_SERVER_ERROR);

    let (restore_status, _) =
        a09_restore_s3_response(&app, &dispatch, encoded_index_name, s3_key).await;
    assert_eq!(restore_status, StatusCode::INTERNAL_SERVER_ERROR);

    let (delete_status, delete_body) = a09_captured_json_response(
        &app,
        &dispatch,
        authed_request(
            Method::DELETE,
            &format!("/1/indexes/{encoded_index_name}"),
            ADMIN_KEY,
            None,
        ),
    )
    .await;
    assert_eq!(delete_status, StatusCode::OK);
    common::wait_for_task_local_with_key(&app, common::extract_task_id(&delete_body), ADMIN_KEY)
        .await;
    assert!(
        !_tmp.path().join(raw_index_name).exists(),
        "delete_index must preserve storage deletion semantics for manager-valid decoded names"
    );
    let logs = log_buffer.contents();

    a09_assert_control_index_events(&logs, audit_log_index_name);
    a09_assert_no_forbidden(
        &logs,
        &[s3_key, "a09-control-settings-value", "x-algolia-api-key"],
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a09_successful_admin_auth_emits_event() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(Method::GET, "/1/keys", ADMIN_KEY, None))
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::OK);
    let _body = common::body_json(response).await;
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_auth_success",
        "authenticate",
        "route:/1/keys",
        "success",
        &[],
    );
    a09_assert_no_forbidden(&logs, &["x-algolia-api-key"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a09_auth_audit_base_routes_use_live_route_targets() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let index_name = "a09_base_route_target";
    let bad_key = "a09_base_route_bad_key";
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let delete_index_response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::DELETE,
                &format!("/1/indexes/{index_name}"),
                bad_key,
                None,
            ))
            .await
            .unwrap()
    };
    assert_eq!(delete_index_response.status(), StatusCode::FORBIDDEN);
    let _body = common::assert_error_contract_from_oneshot(delete_index_response, 403).await;

    let security_sources_response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::GET,
                "/1/security/sources",
                ADMIN_KEY,
                None,
            ))
            .await
            .unwrap()
    };
    assert_eq!(security_sources_response.status(), StatusCode::OK);
    let _body = common::body_json(security_sources_response).await;
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_auth_failure",
        "authenticate",
        "route:/1/indexes/{index}",
        "failure",
        &[],
    );
    a09_assert_security_event(
        &logs,
        "security_audit_auth_success",
        "authenticate",
        "route:/1/security/sources",
        "success",
        &[],
    );
    a09_assert_no_forbidden(&logs, &[index_name, bad_key, "x-algolia-api-key"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a09_successful_search_auth_emits_no_auth_success_event() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let index_name = "a09_search_auth_index";
    a09_seed_single_doc(&app, index_name).await;

    let create_resp = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            "/1/keys",
            ADMIN_KEY,
            Some(json!({
                "acl": ["search"],
                "indexes": [index_name],
                "description": "a09 search auth key"
            })),
        ))
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);
    let search_key = a09_assert_nonempty_key(&common::body_json(create_resp).await, "search key");

    let precheck = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            &format!("/1/indexes/{index_name}/query"),
            &search_key,
            Some(json!({"query": "alpha"})),
        ))
        .await
        .unwrap();
    assert_eq!(precheck.status(), StatusCode::OK);
    let precheck_body = common::body_json(precheck).await;
    assert_eq!(precheck_body["nbHits"], json!(1));

    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());
    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::POST,
                &format!("/1/indexes/{index_name}/query"),
                &search_key,
                Some(json!({"query": "alpha"})),
            ))
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::OK);
    let body = common::body_json(response).await;
    assert_eq!(body["nbHits"], json!(1));
    let logs = log_buffer.contents();
    let auth_success_count = logs
        .matches("event=\"security_audit_auth_success\"")
        .count();
    assert_eq!(
        auth_success_count, 0,
        "successful search-only auth must not emit admin auth-success events; logs={logs}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a09_failed_key_path_auth_failure_does_not_log_raw_key() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let raw_url_key = a09_nonempty_secret_specimen("a09_raw_url_key_probe", "raw URL key specimen");
    let bad_header_key =
        a09_nonempty_secret_specimen("a09_bad_header_key_probe", "bad header key specimen");
    let log_buffer = A09LogBuffer::default();
    let dispatch = a09_capture_dispatch(log_buffer.clone());

    let response = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        app.clone()
            .oneshot(authed_request(
                Method::DELETE,
                &format!("/1/keys/{raw_url_key}"),
                &bad_header_key,
                None,
            ))
            .await
            .unwrap()
    };

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let _body = common::assert_error_contract_from_oneshot(response, 403).await;
    let logs = log_buffer.contents();

    a09_assert_security_event(
        &logs,
        "security_audit_auth_failure",
        "authenticate",
        "route:/1/keys/{key}",
        "failure",
        &[],
    );
    a09_assert_no_forbidden(
        &logs,
        &[
            raw_url_key.as_str(),
            bad_header_key.as_str(),
            "x-algolia-api-key",
        ],
    );
}

#[test]
fn security_audit_event_matcher_rejects_fields_split_across_records() {
    let split_logs = concat!(
        "event=\"security_audit_admin_action\" actor=\"admin_api_key\" action=\"delete_index\"\n",
        "target=\"index:split_fields\" outcome=\"success\"\n",
    );

    assert!(
        a09_matching_security_event_line(
            split_logs,
            "security_audit_admin_action",
            "delete_index",
            "index:split_fields",
            "success",
            &[],
        )
        .is_none(),
        "canonical audit fields split across records must not satisfy the event contract"
    );
}

#[test]
fn security_audit_key_target_uses_known_sha256_prefix() {
    assert_eq!(
        a09_api_key_target("key-target-known-answer"),
        "api_key:4e38e2aa63786ebc"
    );
}

fn a08_assert_in_order(haystack: &str, ordered_needles: &[&str], context: &str) {
    let mut cursor = 0usize;
    for needle in ordered_needles {
        let remaining = &haystack[cursor..];
        let relative_pos = remaining.find(needle).unwrap_or_else(|| {
            panic!("missing expected snippet `{needle}` in {context}");
        });
        cursor += relative_pos + needle.len();
    }
}

#[test]
fn a08_installer_fails_closed_when_verification_material_is_missing() {
    let install_path = a06_engine_root().join("install.sh");
    let install_script = std::fs::read_to_string(&install_path).unwrap_or_else(|error| {
        panic!(
            "failed to read installer at {}: {error}",
            install_path.display()
        )
    });

    a08_assert_in_order(
        &install_script,
        &["No checksum file available", "exit 1"],
        "install.sh checksum-file gate",
    );
    a08_assert_in_order(
        &install_script,
        &["No checksum tool found", "exit 1"],
        "install.sh checksum-tool gate",
    );
    assert!(
        !install_script.contains("skipping verification"),
        "installer must fail closed; warning-and-continue integrity paths are not allowed",
    );
}

#[test]
fn a08_installer_fails_closed_when_checksum_validation_detects_tampering() {
    let install_path = a06_engine_root().join("install.sh");
    let install_script = std::fs::read_to_string(&install_path).unwrap_or_else(|error| {
        panic!(
            "failed to read installer at {}: {error}",
            install_path.display()
        )
    });

    a08_assert_in_order(
        &install_script,
        &[
            "Checksum verification FAILED! The download may be corrupted.",
            "Expected checksum from: ${checksum_name}",
            "exit 1",
        ],
        "install.sh tampered-checksum gate",
    );
}

#[test]
fn a08_release_workflow_emits_verifiable_provenance_metadata() {
    let release_workflow_path = a06_engine_root().join("../.github/workflows/release.yml");
    let workflow = std::fs::read_to_string(&release_workflow_path).unwrap_or_else(|error| {
        panic!(
            "failed to read release workflow at {}: {error}",
            release_workflow_path.display()
        )
    });

    assert!(
        workflow.contains("id-token: write"),
        "release workflow must request OIDC token permissions for provenance attestation"
    );
    assert!(
        workflow.contains("provenance: mode=max"),
        "release workflow must emit build provenance for published GHCR images"
    );
    assert!(
        workflow.contains("sbom: true"),
        "release workflow must emit SBOM metadata for published GHCR images"
    );
}

fn a08_recompute_tar_header_checksum(header_block: &mut [u8]) {
    header_block[148..156].fill(b' ');
    let checksum = header_block.iter().map(|byte| *byte as u32).sum::<u32>();
    let checksum_octal = format!("{checksum:06o}\0 ");
    header_block[148..156].copy_from_slice(checksum_octal.as_bytes());
}

fn a08_archive_with_patched_path(path: &str, contents: &[u8]) -> Vec<u8> {
    assert!(
        path.len() < 100,
        "path must fit into the ustar name field for this test helper"
    );

    let mut archive = Builder::new(Vec::new());
    let mut header = Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    archive
        .append_data(&mut header, "safe.txt", contents)
        .expect("must build baseline test archive entry");
    let mut tar_bytes = archive.into_inner().expect("must finalize tar stream");

    let header_block = &mut tar_bytes[0..512];
    header_block[0..100].fill(0);
    header_block[0..path.len()].copy_from_slice(path.as_bytes());
    a08_recompute_tar_header_checksum(header_block);

    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&tar_bytes)
        .expect("must write tar payload into gzip encoder");
    encoder.finish().expect("must finalize gzip stream")
}

// Build a gzipped tar carrying a single link entry (symlink or hard link,
// selected by `entry_type`) followed by a regular payload entry. Both symlink
// and hard-link entries are written through `append_link`, so parameterizing by
// `EntryType` keeps one source of truth for the crafted link-escape fixture.
fn a08_archive_with_link(
    entry_type: EntryType,
    link_path: &str,
    target: &str,
    payload_path: &str,
) -> Vec<u8> {
    let mut archive = Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));

    let mut link_header = Header::new_gnu();
    link_header.set_entry_type(entry_type);
    link_header.set_size(0);
    link_header.set_mode(0o777);
    link_header.set_cksum();
    archive
        .append_link(&mut link_header, link_path, target)
        .expect("must build link entry");

    let payload = b"link-escape-probe";
    let mut payload_header = Header::new_gnu();
    payload_header.set_entry_type(EntryType::Regular);
    payload_header.set_size(payload.len() as u64);
    payload_header.set_mode(0o644);
    payload_header.set_cksum();
    archive
        .append_data(&mut payload_header, payload_path, payload.as_slice())
        .expect("must build payload entry");

    archive
        .into_inner()
        .expect("must finalize tar stream")
        .finish()
        .expect("must finalize gzip stream")
}

// Assert a crafted snapshot archive is rejected by the Flapjack guard itself.
// Checking `expected_guard_error` proves the rejection came from
// `validate_archive_entries` rather than incidental `tar::Archive::unpack`
// behavior, and `!outside_path.exists()` proves nothing escaped the destination.
fn a08_assert_snapshot_import_rejected(
    archive: &[u8],
    dest: &Path,
    outside_path: &Path,
    expected_guard_error: &str,
) {
    let result = flapjack::index::snapshot::import_from_bytes(archive, dest);
    assert!(
        result.is_err(),
        "snapshot import must reject the crafted archive entry"
    );
    let message = result
        .expect_err("snapshot import rejection must surface an error")
        .to_string();
    assert!(
        message.contains(expected_guard_error),
        "rejection must come from the Flapjack snapshot guard, not incidental tar unpack \
         behavior; expected error containing {expected_guard_error:?}, got: {message}"
    );
    assert!(
        !outside_path.exists(),
        "rejected archive must not write outside destination: {}",
        outside_path.display()
    );
}

#[test]
fn a08_snapshot_import_rejects_parent_dir_traversal_entries() {
    let sandbox = TempDir::new().expect("sandbox tempdir");
    let dest = sandbox.path().join("dest");
    let outside_path = sandbox.path().join("outside.txt");
    let archive = a08_archive_with_patched_path("../outside.txt", b"escaped");

    a08_assert_snapshot_import_rejected(
        &archive,
        &dest,
        &outside_path,
        "snapshot entry path escapes destination",
    );
}

#[test]
fn a08_snapshot_import_rejects_absolute_path_entries() {
    let sandbox = TempDir::new().expect("sandbox tempdir");
    let dest = sandbox.path().join("dest");
    let outside_path = sandbox.path().join("outside_abs.txt");
    let absolute_entry = outside_path.to_string_lossy().to_string();
    let archive = a08_archive_with_patched_path(&absolute_entry, b"escaped");

    a08_assert_snapshot_import_rejected(
        &archive,
        &dest,
        &outside_path,
        "snapshot entry path must be relative",
    );
}

#[test]
fn a08_snapshot_import_rejects_symlink_escape_pivots() {
    let sandbox = TempDir::new().expect("sandbox tempdir");
    let dest = sandbox.path().join("dest");
    let outside_path = sandbox.path().join("escaped_via_symlink.txt");
    let archive = a08_archive_with_link(
        EntryType::Symlink,
        "pivot",
        "..",
        "pivot/escaped_via_symlink.txt",
    );

    a08_assert_snapshot_import_rejected(
        &archive,
        &dest,
        &outside_path,
        "snapshot archive contains unsupported link entry",
    );
}

#[test]
fn a08_snapshot_import_rejects_hardlink_escape_pivots() {
    let sandbox = TempDir::new().expect("sandbox tempdir");
    let dest = sandbox.path().join("dest");
    let outside_path = sandbox.path().join("escaped_via_hardlink.txt");
    // A hard-link entry whose target resolves outside `dest` would, at unpack
    // time, pivot writes into the parent tree. The guard must reject it before
    // any unpacking happens.
    let archive = a08_archive_with_link(
        EntryType::Link,
        "pivot",
        "../escaped_via_hardlink.txt",
        "pivot/escaped_via_hardlink.txt",
    );

    a08_assert_snapshot_import_rejected(
        &archive,
        &dest,
        &outside_path,
        "snapshot archive contains unsupported link entry",
    );
}
