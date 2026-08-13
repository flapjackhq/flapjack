/// The Typesense loopback seam is reachable in every profile behind the
/// explicit `FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1` opt-in, so the submit-path
/// tests below run un-gated. Only `preview_typesense_client` stays
/// `#[cfg(debug_assertions)]`, because release preview delegates to
/// `typesense_source_reader` instead —
/// `typesense_preview_release_source_reader_delegates_to_submit_owner` pins
/// that split.
#[cfg(debug_assertions)]
use super::super::preview_typesense_client;
use super::super::source_reader::TypesenseSourceReader;
use super::super::source_test_support::{typesense_observation, ScriptedTypesenseSource};
use super::super::typesense_client::TYPESENSE_PREVIEW_LOOPBACK_ENV;
use super::super::MigrateFromTypesenseRequest;
use super::*;
use flapjack::security::test_helpers::install_test_outbound_host_resolver;
use std::net::IpAddr;

const TYPESENSE_PREVIEW_ENDPOINT: &str = "http://127.0.0.1:17748";
const TYPESENSE_SOURCE_INDEX: &str = "fj_ts_migration_products";

use crate::router_tests::typesense_fixture_test_support::{
    product_count, products_collection, products_documents,
};
use std::sync::atomic::{AtomicUsize, Ordering};

fn typesense_m0b_preview_source_reader() -> TypesenseSourceReader<ScriptedTypesenseSource> {
    let collection = products_collection();
    let documents = products_documents().to_vec();
    let source_record_count = documents.len() as u64;
    let mut settings = collection
        .as_object()
        .expect("M0B products collection must be an object")
        .clone();
    settings.remove("name");
    settings.remove("documents");
    let source = ScriptedTypesenseSource::with_passes(
        typesense_observation(TYPESENSE_SOURCE_INDEX, source_record_count),
        Value::Object(settings),
        vec![vec![documents]],
    );
    TypesenseSourceReader::from_source(TYPESENSE_SOURCE_INDEX, source, true)
}

fn typesense_preview_body(api_key: &str) -> Value {
    typesense_preview_body_with_attestation(api_key, true)
}

fn typesense_preview_body_with_attestation(api_key: &str, source_write_frozen: bool) -> Value {
    let mut body = json!({
        "node": TYPESENSE_PREVIEW_ENDPOINT,
        "apiKey": api_key,
        "sourceIndex": TYPESENSE_SOURCE_INDEX,
        "targetIndex": "shop"
    });
    if source_write_frozen {
        body["sourceWriteFrozen"] = json!(true);
    }
    body
}

fn typesense_unattested_preview_body(api_key: &str) -> Value {
    json!({
        "node": TYPESENSE_PREVIEW_ENDPOINT,
        "apiKey": api_key,
        "sourceIndex": TYPESENSE_SOURCE_INDEX,
        "targetIndex": "shop"
    })
}

fn typesense_preview_router_with_factory_calls(
    tmp: &TempDir,
    factory_calls: Arc<AtomicUsize>,
) -> axum::Router {
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let source_factory = TestMigrationSourceReaderFactory::new(move |source_provider| {
        factory_calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(source_provider, AsyncMigrationSourceProvider::Typesense);
        Ok(Box::new(typesense_m0b_preview_source_reader()))
    });
    build_test_router(tmp, Some(key_store)).layer(Extension(source_factory))
}

fn typesense_preview_router(tmp: &TempDir) -> axum::Router {
    typesense_preview_router_with_factory_calls(tmp, Arc::new(AtomicUsize::new(0)))
}

async fn assert_typesense_preview_rejection(
    app: &axum::Router,
    request_body: Value,
    expected_body: Value,
) {
    let response = post_provider_preview(app, "typesense", request_body).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(response).await, expected_body);
}

#[tokio::test]
async fn typesense_preview_report_matches_translation_owner_and_exact_source_counts() {
    let tmp = TempDir::new().unwrap();
    let app = typesense_preview_router(&tmp);

    let response =
        post_provider_preview(&app, "typesense", typesense_preview_body("source-key")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(
        body["report"]["entries"],
        json!([
            {"severity":"ScopeGap","code":"ProductNotMigrated","resource":"Analytics","pageIndex":null,"itemIndex":null,"jsonPath":"$"},
            {"severity":"ScopeGap","code":"ProductNotMigrated","resource":"ApiKeys","pageIndex":null,"itemIndex":null,"jsonPath":"$"},
            {"severity":"ScopeGap","code":"ProductNotMigrated","resource":"Events","pageIndex":null,"itemIndex":null,"jsonPath":"$"},
            {"severity":"ScopeGap","code":"ProductNotMigrated","resource":"Experiments","pageIndex":null,"itemIndex":null,"jsonPath":"$"},
            {"severity":"ScopeGap","code":"ProductNotMigrated","resource":"Recommend","pageIndex":null,"itemIndex":null,"jsonPath":"$"},
            {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.curation_sets"},
            {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.default_sorting_field"},
            {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.fields[10]"},
            {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.fields[11]"},
            {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.symbols_to_index"},
            {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.synonym_sets"},
            {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.token_separators"}
        ])
    );
    assert_eq!(
        body["report"]["summary"],
        json!({"totalEntries": 12, "hardRejections": 0, "warnings": 7, "scopeGaps": 5})
    );
    assert_eq!(
        body["sourceCounts"],
        json!({"indexes": 1, "records": product_count()})
    );
}

#[tokio::test]
async fn typesense_write_freeze_missing_and_false_are_refused_before_io() {
    let tmp = TempDir::new().unwrap();
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let app = typesense_preview_router_with_factory_calls(&tmp, Arc::clone(&factory_calls));
    let rejected_bodies = [
        typesense_unattested_preview_body("missing-freeze-key"),
        {
            let mut body = typesense_unattested_preview_body("false-freeze-key");
            body["sourceWriteFrozen"] = json!(false);
            body
        },
        {
            let mut body = typesense_unattested_preview_body("wrong-type-freeze-key");
            body["sourceWriteFrozen"] = json!("true");
            body
        },
    ];

    for body in rejected_bodies {
        let before = factory_calls.load(Ordering::SeqCst);
        let preview_response = post_provider_preview(&app, "typesense", body.clone()).await;
        if preview_response.status() != StatusCode::BAD_REQUEST
            || factory_calls.load(Ordering::SeqCst) != before
        {
            println!("WRITE_FREEZE_RED_ROUTE=missing_or_false_reached_transport");
            panic!("preview request without literal sourceWriteFrozen=true reached source I/O");
        }

        let before = factory_calls.load(Ordering::SeqCst);
        let submit_response = post_provider_submit(&app, "typesense", body).await;
        if submit_response.status() != StatusCode::BAD_REQUEST
            || factory_calls.load(Ordering::SeqCst) != before
        {
            println!("WRITE_FREEZE_RED_ROUTE=missing_or_false_reached_transport");
            panic!("submit request without literal sourceWriteFrozen=true reached source I/O");
        }
    }

    let before = factory_calls.load(Ordering::SeqCst);
    let preview_response = post_provider_preview(
        &app,
        "typesense",
        typesense_preview_body_with_attestation("attested-preview-key", true),
    )
    .await;
    assert_ne!(
        factory_calls.load(Ordering::SeqCst),
        before,
        "literal sourceWriteFrozen=true must reach the existing preview source-reader factory"
    );
    assert!(
        preview_response.status().is_success(),
        "attested Typesense preview should enter the existing capture path"
    );

    let before = factory_calls.load(Ordering::SeqCst);
    let submit_response = post_provider_submit(
        &app,
        "typesense",
        typesense_preview_body_with_attestation("attested-submit-key", true),
    )
    .await;
    assert_ne!(
        factory_calls.load(Ordering::SeqCst),
        before,
        "literal sourceWriteFrozen=true must reach the existing submit source-reader factory"
    );
    assert!(
        submit_response.status().is_success(),
        "attested Typesense submit should enter the existing capture path"
    );
}

#[tokio::test]
async fn typesense_preview_discriminates_route_body_before_translation() {
    let tmp = TempDir::new().unwrap();
    let app = typesense_preview_router(&tmp);

    let accepted =
        post_provider_preview(&app, "typesense", typesense_preview_body("source-key")).await;
    assert_eq!(accepted.status(), StatusCode::OK);
    assert_eq!(
        body_json(accepted).await["sourceCounts"],
        json!({"indexes": 1, "records": product_count()})
    );

    assert_typesense_preview_rejection(
        &app,
        json!({
            "appId": "APPID",
            "apiKey": "source-key",
            "sourceIndex": "products",
            "targetIndex": "shop"
        }),
        json!({
            "message": "Source provider is not supported",
            "status": 400,
            "code": "source_provider_unsupported"
        }),
    )
    .await;
    assert_typesense_preview_rejection(
        &app,
        json!({
            "endpoint": "http://127.0.0.1:17747",
            "apiKey": "source-key",
            "sourceIndex": "products",
            "targetIndex": "shop"
        }),
        json!({
            "message": "Typesense payload does not match source_provider",
            "status": 400,
            "code": "source_provider_payload_mismatch"
        }),
    )
    .await;
}

/// Returns one cfg-gated `preview_typesense_source_reader` definition. The
/// release half is unreachable from debug-profile runtime tests, so this pins
/// its delegation contract while the release build gate proves compilation.
fn typesense_preview_source_reader_body(cfg_attribute: &str) -> String {
    let module_source = include_str!("../mod.rs");
    let definition = format!("{cfg_attribute}\nfn preview_typesense_source_reader(");
    let start = module_source
        .find(&definition)
        .unwrap_or_else(|| panic!("mod.rs must define {definition}"));
    let remainder = &module_source[start..];
    let end = remainder
        .find("\n}\n")
        .expect("the definition must be terminated by a top-level closing brace");
    remainder[..end].to_string()
}

#[test]
fn typesense_preview_release_source_reader_delegates_to_submit_owner() {
    let release_body = typesense_preview_source_reader_body("#[cfg(not(debug_assertions))]");
    assert!(
        release_body.contains("typesense_source_reader(payload)"),
        "the release preview reader must delegate to the submit-path owner, got:\n{release_body}"
    );
    assert!(
        !release_body.contains("preview_typesense_client"),
        "the loopback preview client must not be reachable outside debug builds, got:\n{release_body}"
    );

    let debug_body = typesense_preview_source_reader_body("#[cfg(debug_assertions)]");
    assert!(
        debug_body.contains("preview_typesense_client(payload)"),
        "the debug preview reader must build through the loopback preview client, got:\n{debug_body}"
    );
    assert_ne!(
        debug_body, release_body,
        "two byte-identical cfg halves mean the cfg split is dead duplication"
    );
}

#[tokio::test]
async fn typesense_preview_does_not_write_durable_state_byte_identity() {
    let tmp = TempDir::new().unwrap();
    let app = typesense_preview_router(&tmp);
    let before = seed_durable_state_specimens(tmp.path()).await;
    let response =
        post_provider_preview(&app, "typesense", typesense_preview_body("source-key")).await;

    assert_preview_preserves_durable_state(tmp.path(), before, response).await;
}

#[tokio::test]
#[cfg(debug_assertions)]
async fn typesense_preview_requires_explicit_loopback_opt_in() {
    const API_KEY_CANARY: &str = "preview-route-api-key-canary";

    let _env = with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "");
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let response =
        post_provider_preview(&app, "typesense", typesense_preview_body(API_KEY_CANARY)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(
        body,
        json!({
            "message": "Typesense preview loopback endpoint is disabled",
            "status": 400
        })
    );
    let diagnostics = body.to_string();
    for canary in [TYPESENSE_PREVIEW_ENDPOINT, API_KEY_CANARY] {
        assert!(
            !diagnostics.contains(canary),
            "disabled loopback diagnostics leaked request canary: {canary}"
        );
    }
}

/// Typesense counterpart to
/// `meilisearch_submit_admits_opted_in_loopback_source_reader`: submit admits
/// the explicitly opted-in loopback node the live contract fixture serves.
#[test]
fn typesense_submit_admits_opted_in_loopback_source_reader() {
    let _env = with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "1");
    let payload: MigrateFromTypesenseRequest =
        serde_json::from_value(typesense_preview_body("submit-route-api-key-canary"))
            .expect("Typesense submit fixture must deserialize");

    super::super::typesense_source_reader(&payload)
        .expect("submit must admit an opted-in loopback node through the same seam as discovery");
}

/// Production admission must remain the first branch in every profile. This
/// constructor-only check resolves a vetted vendor host without issuing a
/// request and proves the absent loopback opt-in cannot shadow Cloud submit.
#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn typesense_submit_accepts_vetted_cloud_endpoint_without_loopback_opt_in() {
    const CLOUD_HOST: &str = "submit-debug-contract.typesense.net";

    let _env = with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "");
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, port| {
        assert_eq!(host, CLOUD_HOST);
        assert_eq!(port, Some(443));
        Some(vec!["8.8.8.8".parse::<IpAddr>().unwrap()])
    }));
    let payload = MigrateFromTypesenseRequest {
        node: format!("https://{CLOUD_HOST}"),
        api_key: "submit-cloud-api-key-canary".to_string(),
        source_index: TYPESENSE_SOURCE_INDEX.to_string(),
        target_index: Some("shop".to_string()),
        overwrite: false,
        source_write_frozen: true,
    };

    super::super::typesense_source_reader(&payload)
        .expect("submit must retain vetted Typesense Cloud admission in every profile");
}

/// Without the opt-in, submit stays refused and reports the production vendor
/// refusal, never the loopback seam's own message.
#[tokio::test]
async fn typesense_submit_requires_explicit_loopback_opt_in() {
    const API_KEY_CANARY: &str = "submit-route-api-key-canary";

    let _env = with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "");
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let response =
        post_provider_submit(&app, "typesense", typesense_preview_body(API_KEY_CANARY)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(
        body,
        json!({
            "message": "Typesense Cloud endpoint is not allowed",
            "status": 400
        })
    );
    let diagnostics = body.to_string();
    for canary in [
        TYPESENSE_PREVIEW_ENDPOINT,
        API_KEY_CANARY,
        "Typesense preview loopback endpoint is disabled",
    ] {
        assert!(
            !diagnostics.contains(canary),
            "disabled submit loopback diagnostics leaked request canary: {canary}"
        );
    }
}

/// The opt-in widens submit admission to literal loopback only. With the
/// switch on, a non-vendor host is still refused by production admission.
#[tokio::test]
async fn typesense_submit_opt_in_does_not_admit_non_loopback_hosts() {
    const API_KEY_CANARY: &str = "submit-route-non-loopback-api-key-canary";
    const NON_LOOPBACK_NODE: &str = "https://evil.example.com";

    let _env = with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "1");
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let response = post_provider_submit(
        &app,
        "typesense",
        json!({
            "node": NON_LOOPBACK_NODE,
            "apiKey": API_KEY_CANARY,
            "sourceIndex": TYPESENSE_SOURCE_INDEX,
            "targetIndex": "shop",
            "sourceWriteFrozen": true
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(
        body,
        json!({
            "message": "Typesense Cloud endpoint is not allowed",
            "status": 400
        })
    );
    let diagnostics = body.to_string();
    for canary in [NON_LOOPBACK_NODE, API_KEY_CANARY] {
        assert!(
            !diagnostics.contains(canary),
            "opted-in submit refusal leaked request canary: {canary}"
        );
    }
}

#[tokio::test]
#[cfg(debug_assertions)]
async fn typesense_preview_non_loopback_refusal_hides_loopback_opt_in() {
    const API_KEY_CANARY: &str = "preview-route-non-loopback-api-key-canary";
    const NON_LOOPBACK_NODE: &str = "https://evil.example.com";

    let _env = with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "");
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let response = post_provider_preview(
        &app,
        "typesense",
        json!({
            "node": NON_LOOPBACK_NODE,
            "apiKey": API_KEY_CANARY,
            "sourceIndex": TYPESENSE_SOURCE_INDEX,
            "targetIndex": "shop",
            "sourceWriteFrozen": true
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(
        body,
        json!({
            "message": "Typesense Cloud endpoint is not allowed",
            "status": 400
        })
    );
    let diagnostics = body.to_string();
    for canary in [
        NON_LOOPBACK_NODE,
        API_KEY_CANARY,
        "Typesense preview loopback endpoint is disabled",
    ] {
        assert!(
            !diagnostics.contains(canary),
            "non-loopback refusal leaked hidden loopback seam or request canary: {canary}"
        );
    }
}

#[test]
#[cfg(debug_assertions)]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn typesense_preview_accepts_cloud_endpoint_without_loopback_opt_in() {
    let _env = with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "");
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, port| {
        assert_eq!(host, "tenant.typesense.net");
        assert_eq!(port, Some(443));
        Some(vec!["8.8.8.8".parse::<IpAddr>().unwrap()])
    }));
    let payload = MigrateFromTypesenseRequest {
        node: "https://tenant.typesense.net".to_string(),
        api_key: "source-key".to_string(),
        source_index: TYPESENSE_SOURCE_INDEX.to_string(),
        target_index: Some("shop".to_string()),
        overwrite: false,
        source_write_frozen: true,
    };

    let client = preview_typesense_client(&payload)
        .expect("preview must retain production Typesense Cloud admission in debug builds");

    assert_eq!(
        client.source_collection_for_test(),
        Some(TYPESENSE_SOURCE_INDEX)
    );
}
