use super::super::MigrateFromMeilisearchRequest;
use super::*;
use flapjack::security::test_helpers::install_test_outbound_host_resolver;
use std::net::IpAddr;

/// Loopback endpoint the Meilisearch route-admission tests exercise. Nothing
/// listens on it: every case here is refused or admitted at reader
/// construction, before any outbound request.
const MEILISEARCH_LOOPBACK_ENDPOINT: &str = "http://127.0.0.1:17747";

fn meilisearch_loopback_body(endpoint: &str, api_key: &str) -> Value {
    json!({
        "endpoint": endpoint,
        "apiKey": api_key,
        "sourceIndex": "products",
        "targetIndex": "shop"
    })
}

#[tokio::test]
async fn meilisearch_preview_requires_explicit_loopback_opt_in() {
    const API_KEY_CANARY: &str = "preview-route-api-key-canary";

    let _env = with_env_var(MEILISEARCH_PREVIEW_LOOPBACK_ENV, "");
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let response = post_provider_preview(
        &app,
        "meilisearch",
        meilisearch_loopback_body(MEILISEARCH_LOOPBACK_ENDPOINT, API_KEY_CANARY),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(
        body,
        json!({
            "message": "Meilisearch preview loopback endpoint is disabled",
            "status": 400
        })
    );
    let diagnostics = body.to_string();
    for canary in [MEILISEARCH_LOOPBACK_ENDPOINT, API_KEY_CANARY] {
        assert!(
            !diagnostics.contains(canary),
            "disabled loopback diagnostics leaked request canary: {canary}"
        );
    }
}

/// Submit must admit the same explicitly opted-in loopback endpoint the live
/// contract fixture serves. Asserted at the reader constructor rather than the
/// route so the test never spawns a background import against a dead port.
#[test]
fn meilisearch_submit_admits_opted_in_loopback_source_reader() {
    let _env = with_env_var(MEILISEARCH_PREVIEW_LOOPBACK_ENV, "1");
    let payload: MigrateFromMeilisearchRequest = serde_json::from_value(meilisearch_loopback_body(
        MEILISEARCH_LOOPBACK_ENDPOINT,
        "submit-route-api-key-canary",
    ))
    .expect("Meilisearch submit fixture must deserialize");

    super::super::meilisearch_source_reader(&payload).expect(
        "submit must admit an opted-in loopback endpoint through the same seam as discovery",
    );
}

/// Production admission must remain the first branch in every profile. This
/// constructor-only check resolves a vetted vendor host without issuing a
/// request and proves the absent loopback opt-in cannot shadow Cloud submit.
#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn meilisearch_submit_accepts_vetted_cloud_endpoint_without_loopback_opt_in() {
    const CLOUD_HOST: &str = "submit-debug-contract.meilisearch.io";

    let _env = with_env_var(MEILISEARCH_PREVIEW_LOOPBACK_ENV, "");
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, port| {
        assert_eq!(host, CLOUD_HOST);
        assert_eq!(port, Some(443));
        Some(vec!["8.8.8.8".parse::<IpAddr>().unwrap()])
    }));
    let payload = MigrateFromMeilisearchRequest {
        endpoint: format!("https://{CLOUD_HOST}"),
        api_key: "submit-cloud-api-key-canary".to_string(),
        source_index: "products".to_string(),
        target_index: Some("shop".to_string()),
        overwrite: false,
    };

    super::super::meilisearch_source_reader(&payload)
        .expect("submit must retain vetted Meilisearch Cloud admission in every profile");
}

/// Without the opt-in, submit stays refused, and it reports the production
/// vendor refusal rather than the loopback seam's own message — an
/// unrecognised host must never learn that the debug seam exists.
#[tokio::test]
async fn meilisearch_submit_requires_explicit_loopback_opt_in() {
    const API_KEY_CANARY: &str = "submit-route-api-key-canary";

    let _env = with_env_var(MEILISEARCH_PREVIEW_LOOPBACK_ENV, "");
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let response = post_provider_submit(
        &app,
        "meilisearch",
        meilisearch_loopback_body(MEILISEARCH_LOOPBACK_ENDPOINT, API_KEY_CANARY),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(
        body,
        json!({
            "message": "Meilisearch Cloud endpoint is not allowed",
            "status": 400
        })
    );
    let diagnostics = body.to_string();
    for canary in [
        MEILISEARCH_LOOPBACK_ENDPOINT,
        API_KEY_CANARY,
        "Meilisearch preview loopback endpoint is disabled",
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
async fn meilisearch_submit_opt_in_does_not_admit_non_loopback_hosts() {
    const API_KEY_CANARY: &str = "submit-route-non-loopback-api-key-canary";
    const NON_LOOPBACK_ENDPOINT: &str = "https://evil.example.com";

    let _env = with_env_var(MEILISEARCH_PREVIEW_LOOPBACK_ENV, "1");
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let response = post_provider_submit(
        &app,
        "meilisearch",
        meilisearch_loopback_body(NON_LOOPBACK_ENDPOINT, API_KEY_CANARY),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(
        body,
        json!({
            "message": "Meilisearch Cloud endpoint is not allowed",
            "status": 400
        })
    );
    let diagnostics = body.to_string();
    for canary in [NON_LOOPBACK_ENDPOINT, API_KEY_CANARY] {
        assert!(
            !diagnostics.contains(canary),
            "opted-in submit refusal leaked request canary: {canary}"
        );
    }
}

#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn meilisearch_preview_rejects_non_loopback_literals_and_authority_confusion() {
    const API_KEY_CANARY: &str = "preview-route-api-key-canary";
    const REBIND_HOST: &str = "preview-rebind.meilisearch.io";

    let _env = with_env_var(MEILISEARCH_PREVIEW_LOOPBACK_ENV, "1");
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, port| {
        assert_eq!(host, REBIND_HOST, "unexpected host reached the resolver");
        assert_eq!(port, Some(443));
        Some(vec!["127.0.0.1".parse::<IpAddr>().unwrap()])
    }));
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    for (case, endpoint) in [
        ("localhost", "http://localhost:17747"),
        ("public literal IP", "https://8.8.8.8"),
        (
            "URL userinfo",
            "https://user:password-canary@tenant.meilisearch.io",
        ),
        (
            "Meilisearch Cloud-shaped hostname resolving to loopback",
            "https://preview-rebind.meilisearch.io",
        ),
    ] {
        let response = post_provider_preview(
            &app,
            "meilisearch",
            json!({
                "endpoint": endpoint,
                "apiKey": API_KEY_CANARY,
                "sourceIndex": "products",
                "targetIndex": "shop"
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "case: {case}");
        let body = body_json(response).await;
        assert_eq!(
            body,
            json!({
                "message": "Meilisearch Cloud endpoint is not allowed",
                "status": 400
            }),
            "case: {case}"
        );
        let diagnostics = body.to_string();
        for canary in [endpoint, API_KEY_CANARY, "password-canary"] {
            assert!(
                !diagnostics.contains(canary),
                "case {case} leaked request canary: {canary}"
            );
        }
    }
}

#[cfg(not(debug_assertions))]
#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn release_preview_reader_accepts_vetted_meilisearch_cloud_endpoint() {
    const CLOUD_HOST: &str = "preview-release-contract.meilisearch.io";

    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, port| {
        assert_eq!(host, CLOUD_HOST);
        assert_eq!(port, Some(443));
        Some(vec!["8.8.8.8".parse::<IpAddr>().unwrap()])
    }));
    let payload = MigrateFromMeilisearchRequest {
        endpoint: format!("https://{CLOUD_HOST}"),
        api_key: "release-preview-api-key-canary".to_string(),
        source_index: "products".to_string(),
        target_index: Some("shop".to_string()),
        overwrite: false,
    };

    super::super::preview_meilisearch_source_reader(&payload)
        .expect("release preview must retain vetted Meilisearch Cloud support");
}
