//! Stage 1 RED enforcement contract for the replication peer-vs-admin boundary.
//!
//! `auth/middleware.rs` is the single authorization enforcer, so the served
//! boundary is asserted here through `authenticate_and_authorize` and its
//! existing `KeyStore` / request-extension seams rather than through a parallel
//! auth harness.
//!
//! Status codes: this codebase answers every rejected credential with
//! `403 Forbidden` and the Algolia-compatible body
//! `{"message":"Invalid Application-ID or API key","status":403}`
//! (`auth::invalid_api_credentials_error`); an ACL mismatch on a route the key
//! *is* known for answers `403` with `"Method not allowed with this API key"`.
//! There is no `401` path in this crate. The two refusals are distinguished by
//! body below so "refused" cannot be satisfied by the wrong refusal.
//!
//! `peer_credential_is_accepted_on_peer_allowed_internal_routes` is the Stage 2
//! green contract. Everything else is the regression lock Stage 2 must not break.

use super::authenticate_and_authorize;
use crate::auth::{ApiKey, KeyStore, RateLimiter, ReplicationPeerCredential};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::{delete, get, post},
    Extension, Router,
};
use flapjack_replication::peer::REPLICATION_PEER_APPLICATION_ID;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

use crate::auth::tests::peer_boundary_route_contract::{
    admin_only_routes, high_risk_admin_mutations, peer_allowed_routes, INTERNAL_ROUTE_CONTRACT,
};

const ADMIN_KEY: &str = "stage1-boundary-admin-key";
/// The application ID `PeerClient` already sends (`peer.rs:53-60`). The lane
/// deliberately introduces no new header, so this is the peer's only identity.
const PEER_APP_ID: &str = REPLICATION_PEER_APPLICATION_ID;
const PEER_SECRET: &str = "stage1-boundary-peer-credential";
const RANDOM_KEY: &str = "stage1-boundary-not-a-real-key";

const INVALID_CREDENTIALS_MESSAGE: &str = "Invalid Application-ID or API key";
const ACL_MISMATCH_MESSAGE: &str = "Method not allowed with this API key";

#[test]
fn blank_peer_credentials_never_authenticate() {
    for raw_secret in ["", " ", "\n\t"] {
        let credential =
            ReplicationPeerCredential::from_optional_secret(Some(raw_secret.to_string()));

        assert!(
            !credential.matches_secret(""),
            "blank candidate unexpectedly authenticated for {raw_secret:?}"
        );
        assert!(
            !credential.matches_secret(raw_secret),
            "configured blank credential unexpectedly authenticated for {raw_secret:?}"
        );
    }
}

fn admin_key_store() -> (TempDir, Arc<KeyStore>) {
    let temp_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(temp_dir.path(), ADMIN_KEY));
    (temp_dir, key_store)
}

fn search_only_api_key(description: &str) -> ApiKey {
    ApiKey {
        hash: String::new(),
        salt: String::new(),
        hmac_key: None,
        created_at: 0,
        acl: vec!["search".to_string(), "listIndexes".to_string()],
        description: description.to_string(),
        indexes: vec![],
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: vec![],
        restrict_sources: None,
        validity: 0,
    }
}

/// Mounts the routes under test behind the real auth middleware.
fn peer_boundary_app(key_store: Arc<KeyStore>, downstream_marker: Arc<AtomicBool>) -> Router {
    let mut app = Router::new();
    for row in INTERNAL_ROUTE_CONTRACT {
        let marker = Arc::clone(&downstream_marker);
        let ok = move || {
            let marker = Arc::clone(&marker);
            async move {
                marker.store(true, Ordering::SeqCst);
                StatusCode::OK
            }
        };
        let method_router = match row.method {
            axum::http::Method::GET => get(ok),
            axum::http::Method::POST => post(ok),
            axum::http::Method::DELETE => delete(ok),
            ref method => panic!("unsupported shared contract method {method}"),
        };
        app = app.route(row.mounted_pattern, method_router);
    }

    let marker = Arc::clone(&downstream_marker);
    app.route(
        "/1/indexes",
        get(move || async move {
            marker.store(true, Ordering::SeqCst);
            StatusCode::OK
        }),
    )
    .layer(axum::middleware::from_fn(|request, next| async move {
        authenticate_and_authorize(request, next, false).await
    }))
    .layer(Extension(key_store))
    .layer(Extension(ReplicationPeerCredential::from_optional_secret(
        Some(PEER_SECRET.to_string()),
    )))
    .layer(Extension(RateLimiter::new()))
}

struct ProbeOutcome {
    status: StatusCode,
    body: serde_json::Value,
    downstream_ran: bool,
}

async fn probe(
    key_store: Arc<KeyStore>,
    method: &str,
    uri: &str,
    app_id: Option<&str>,
    api_key_header: Option<&str>,
) -> ProbeOutcome {
    let downstream_ran = Arc::new(AtomicBool::new(false));
    let app = peer_boundary_app(key_store, Arc::clone(&downstream_ran));

    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(app_id) = app_id {
        builder = builder.header("x-algolia-application-id", app_id);
    }
    if let Some(api_key) = api_key_header {
        builder = builder.header("x-algolia-api-key", api_key);
    }

    let response = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    // Refusals carry an Algolia JSON error body; the success handlers return an
    // empty body, so an empty payload becomes `null` rather than a parse panic.
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if body_bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body_bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&body_bytes).into_owned())
        })
    };

    ProbeOutcome {
        status,
        body,
        downstream_ran: downstream_ran.load(Ordering::SeqCst),
    }
}

fn assert_refused(outcome: &ProbeOutcome, expected_message: &str, context: &str) {
    assert_eq!(
        outcome.status,
        StatusCode::FORBIDDEN,
        "{context}: expected the 403 refusal this crate emits, got {} body {}",
        outcome.status,
        outcome.body
    );
    assert_eq!(
        outcome.body,
        serde_json::json!({ "message": expected_message, "status": 403 }),
        "{context}: refused with the wrong reason"
    );
    assert!(
        !outcome.downstream_ran,
        "{context}: handler ran despite the refusal"
    );
}

/// RED until Stage 2 adds the inbound peer tier.
///
/// Fails today because every `/internal/*` route requires the admin key, so the
/// configured peer credential is an unknown key and is refused with
/// `Invalid Application-ID or API key`. The failure is the missing tier, not the
/// fixture: the identical request with the admin key succeeds on the same routes
/// (`admin_credential_is_accepted_on_peer_allowed_internal_routes`).
#[tokio::test]
async fn peer_credential_is_accepted_on_peer_allowed_internal_routes() {
    let (_temp_dir, key_store) = admin_key_store();

    assert!(
        key_store.lookup(PEER_SECRET).is_none(),
        "the peer credential must never be inserted into KeyStore"
    );
    assert!(
        !key_store.is_admin(PEER_SECRET),
        "a peer credential that is secretly an admin key would make this whole lane a no-op"
    );

    let mut refused = Vec::new();
    for row in peer_allowed_routes() {
        let method = row.method.as_str();
        let path = row.specimen_path;
        let outcome = probe(
            Arc::clone(&key_store),
            method,
            path,
            Some(PEER_APP_ID),
            Some(PEER_SECRET),
        )
        .await;
        if outcome.status != StatusCode::OK || !outcome.downstream_ran {
            refused.push(format!("{method} {path} -> {}", outcome.status));
        }
    }

    assert!(
        refused.is_empty(),
        "peer tier missing: configured peer credential refused on replication routes: {refused:?}"
    );
}

/// The peer secret is not a globally valid credential: Stage 2 must require
/// the application ID that `PeerClient` sends as well as the matching secret.
/// Cover the full peer subset so no individual route can omit that check.
#[tokio::test]
async fn peer_credential_requires_exact_replication_application_id_on_every_peer_route() {
    let (_temp_dir, key_store) = admin_key_store();

    for (application_id, label) in [
        (None, "missing application ID"),
        (
            Some("not-the-replication-application-id"),
            "incorrect application ID",
        ),
    ] {
        for row in peer_allowed_routes() {
            let method = row.method.as_str();
            let path = row.specimen_path;
            let outcome = probe(
                Arc::clone(&key_store),
                method,
                path,
                application_id,
                Some(PEER_SECRET),
            )
            .await;
            assert_refused(
                &outcome,
                INVALID_CREDENTIALS_MESSAGE,
                &format!("peer credential with {label} on {method} {path}"),
            );
        }
    }
}

/// GREEN and load-bearing for rolling upgrades: a cluster still configured with
/// the admin key keeps replicating after the primary upgrades, because admin
/// acceptance on peer-allowed routes stays unconditional.
#[tokio::test]
async fn admin_credential_is_accepted_on_peer_allowed_internal_routes() {
    let (_temp_dir, key_store) = admin_key_store();

    for row in peer_allowed_routes() {
        let method = row.method.as_str();
        let path = row.specimen_path;
        let outcome = probe(
            Arc::clone(&key_store),
            method,
            path,
            Some(PEER_APP_ID),
            Some(ADMIN_KEY),
        )
        .await;
        assert_eq!(
            outcome.status,
            StatusCode::OK,
            "admin key must keep working on {method} {path}, got body {}",
            outcome.body
        );
        assert!(
            outcome.downstream_ran,
            "admin key must reach the handler on {method} {path}"
        );
    }
}

#[tokio::test]
async fn admin_credential_keeps_normal_auth_context_when_peer_secret_matches() {
    let (_temp_dir, key_store) = admin_key_store();
    let admin_context_present = Arc::new(AtomicBool::new(false));
    let context_marker = Arc::clone(&admin_context_present);

    let app = Router::new()
        .route(
            "/internal/ops",
            get(move |request: axum::extract::Request| async move {
                context_marker.store(
                    request.extensions().get::<ApiKey>().is_some(),
                    Ordering::SeqCst,
                );
                StatusCode::OK
            }),
        )
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }))
        .layer(Extension(Arc::clone(&key_store)))
        .layer(Extension(ReplicationPeerCredential::from_optional_secret(
            Some(ADMIN_KEY.to_string()),
        )))
        .layer(Extension(RateLimiter::new()));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/internal/ops")
                .header("x-algolia-application-id", REPLICATION_PEER_APPLICATION_ID)
                .header("x-algolia-api-key", ADMIN_KEY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        admin_context_present.load(Ordering::SeqCst),
        "an admin key matching the peer secret must retain its normal ApiKey context"
    );
}

#[tokio::test]
async fn restricted_api_key_keeps_its_acl_when_peer_secret_matches() {
    let (_temp_dir, key_store) = admin_key_store();
    let (_, search_key_value) =
        key_store.create_key(search_only_api_key("peer-secret collision key"));
    let downstream_ran = Arc::new(AtomicBool::new(false));
    let downstream_marker = Arc::clone(&downstream_ran);

    let app = Router::new()
        .route(
            "/internal/ops",
            get(move || async move {
                downstream_marker.store(true, Ordering::SeqCst);
                StatusCode::OK
            }),
        )
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }))
        .layer(Extension(Arc::clone(&key_store)))
        .layer(Extension(ReplicationPeerCredential::from_optional_secret(
            Some(search_key_value.clone()),
        )))
        .layer(Extension(RateLimiter::new()));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/internal/ops")
                .header("x-algolia-application-id", REPLICATION_PEER_APPLICATION_ID)
                .header("x-algolia-api-key", search_key_value)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        !downstream_ran.load(Ordering::SeqCst),
        "a restricted API key matching the peer secret must retain its ACL"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({
            "message": ACL_MISMATCH_MESSAGE,
            "status": 403
        })
    );
}

/// The lane's actual deliverable. A peer credential that works on the
/// replication subset but was never proven refused here has closed nothing.
#[tokio::test]
async fn peer_credential_is_refused_on_administrative_mutations() {
    let (_temp_dir, key_store) = admin_key_store();

    for row in high_risk_admin_mutations() {
        let method = row.method.as_str();
        let path = row.specimen_path;
        let outcome = probe(
            Arc::clone(&key_store),
            method,
            path,
            Some(PEER_APP_ID),
            Some(PEER_SECRET),
        )
        .await;
        assert_eq!(
            outcome.status,
            StatusCode::FORBIDDEN,
            "{method} {path} must refuse the peer credential with 403, not 404/500; body {}",
            outcome.body
        );
        assert!(
            !outcome.downstream_ran,
            "{method} {path} handler ran with a peer credential"
        );
        assert_eq!(
            outcome.body["status"], 403,
            "{method} {path} must answer in the Algolia error shape"
        );
        let message = outcome.body["message"].as_str().unwrap_or_default();
        assert!(
            message == INVALID_CREDENTIALS_MESSAGE || message == ACL_MISMATCH_MESSAGE,
            "{method} {path} refused with an unexpected message: {message}"
        );
    }
}

/// The three high-risk mutations are necessary but not sufficient: a peer
/// credential also must not reach admin-only read diagnostics or fault routes.
#[tokio::test]
async fn peer_credential_is_refused_on_every_admin_only_internal_route() {
    let (_temp_dir, key_store) = admin_key_store();

    for row in admin_only_routes() {
        let method = row.method.as_str();
        let path = row.specimen_path;
        let outcome = probe(
            Arc::clone(&key_store),
            method,
            path,
            Some(PEER_APP_ID),
            Some(PEER_SECRET),
        )
        .await;
        assert_refused(
            &outcome,
            INVALID_CREDENTIALS_MESSAGE,
            &format!("peer credential on admin-only route {method} {path}"),
        );
    }
}

/// Admin keys must still reach every administrative route; otherwise the
/// refusal above could be satisfied by breaking the routes for everyone.
#[tokio::test]
async fn admin_credential_still_reaches_every_admin_only_internal_route() {
    let (_temp_dir, key_store) = admin_key_store();

    for row in admin_only_routes() {
        let method = row.method.as_str();
        let path = row.specimen_path;
        let outcome = probe(
            Arc::clone(&key_store),
            method,
            path,
            Some(PEER_APP_ID),
            Some(ADMIN_KEY),
        )
        .await;
        assert_eq!(
            outcome.status,
            StatusCode::OK,
            "admin key must still reach {method} {path}; body {}",
            outcome.body
        );
        assert!(outcome.downstream_ran, "{method} {path} handler must run");
    }
}

/// URL-borne credentials stay refused on internal routes after the peer tier
/// lands. `extract_api_key` currently gates this on
/// `RouteAcl::Required("admin")`; a new `PeerOrAdmin` tier that does not extend
/// that check would start accepting replication secrets from query strings,
/// where they leak into access logs and shell history.
#[tokio::test]
async fn query_string_only_credential_is_refused_on_peer_allowed_internal_routes() {
    let (_temp_dir, key_store) = admin_key_store();

    for (credential, label) in [(PEER_SECRET, "peer"), (ADMIN_KEY, "admin")] {
        let uri = format!("/internal/ops?x-algolia-api-key={credential}");
        let outcome = probe(Arc::clone(&key_store), "GET", &uri, Some(PEER_APP_ID), None).await;
        assert_refused(
            &outcome,
            INVALID_CREDENTIALS_MESSAGE,
            &format!("{label} credential in query string on GET /internal/ops"),
        );
    }
}

/// `extract_api_key` is the seam that enforces the header-only rule. Asserted
/// directly so the reason a query-string request fails is pinned, not inferred
/// from a status code that several other checks can also produce.
#[test]
fn extract_api_key_ignores_query_string_credentials_on_internal_routes() {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/internal/ops?x-algolia-api-key={PEER_SECRET}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        super::extract_api_key(&request),
        None,
        "internal routes must not accept URL-borne credentials"
    );

    let header_request = Request::builder()
        .method("GET")
        .uri("/internal/ops")
        .header("x-algolia-api-key", PEER_SECRET)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        super::extract_api_key(&header_request),
        Some(PEER_SECRET.to_string()),
        "the header path must still deliver the credential"
    );

    let public_request = Request::builder()
        .method("GET")
        .uri("/1/indexes/baseline_index/query?x-algolia-api-key=search-key")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        super::extract_api_key(&public_request),
        Some("search-key".to_string()),
        "Algolia-compatible query-string keys must keep working on client-facing routes"
    );
}

/// An unrelated key gets no internal access at all.
#[tokio::test]
async fn random_key_is_refused_on_peer_allowed_internal_routes() {
    let (_temp_dir, key_store) = admin_key_store();

    for row in peer_allowed_routes() {
        let method = row.method.as_str();
        let path = row.specimen_path;
        let outcome = probe(
            Arc::clone(&key_store),
            method,
            path,
            Some(PEER_APP_ID),
            Some(RANDOM_KEY),
        )
        .await;
        assert_refused(
            &outcome,
            INVALID_CREDENTIALS_MESSAGE,
            &format!("random key on {method} {path}"),
        );
    }
}

/// The peer principal must not become a globally authenticated identity. It is
/// unknown on the protected client-facing API, so `GET /1/indexes` refuses it
/// even while the same credential is accepted on replication routes.
#[tokio::test]
async fn peer_credential_is_refused_on_protected_client_facing_indexes_route() {
    let (_temp_dir, key_store) = admin_key_store();

    let outcome = probe(
        Arc::clone(&key_store),
        "GET",
        "/1/indexes",
        Some(PEER_APP_ID),
        Some(PEER_SECRET),
    )
    .await;
    assert_refused(
        &outcome,
        INVALID_CREDENTIALS_MESSAGE,
        "peer credential on GET /1/indexes",
    );

    let search_key_store = Arc::clone(&key_store);
    let (_, search_key) = search_key_store.create_key(search_only_api_key("Stage 1 boundary key"));
    let allowed = probe(
        Arc::clone(&key_store),
        "GET",
        "/1/indexes",
        Some("baseline-app"),
        Some(&search_key),
    )
    .await;
    assert_eq!(
        allowed.status,
        StatusCode::OK,
        "a listIndexes key must still reach GET /1/indexes; body {}",
        allowed.body
    );
}

/// `ensure_route_acl_allows_request` is the ACL gate itself. An unknown random
/// key exercises credential validation above this seam; this valid search key
/// proves Stage 2 does not accidentally interpret "peer-allowed" as "any
/// authenticated key" on any route in the peer subset.
#[tokio::test]
async fn ensure_route_acl_allows_request_rejects_non_peer_keys_on_every_peer_route() {
    let (_temp_dir, key_store) = admin_key_store();
    let (search_key, search_key_value) =
        key_store.create_key(search_only_api_key("Stage 1 ACL gate key"));

    for row in peer_allowed_routes() {
        let method = &row.method;
        let path = row.specimen_path;
        let denied = super::ensure_route_acl_allows_request(
            &key_store,
            &search_key,
            &search_key_value,
            method,
            path,
        )
        .unwrap_or_else(|| panic!("a non-peer API key reached {method} {path}"));
        assert_eq!(
            denied.status(),
            StatusCode::FORBIDDEN,
            "a non-peer API key got the wrong status on {method} {path}"
        );
        let body = axum::body::to_bytes(denied.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({
                "message": ACL_MISMATCH_MESSAGE,
                "status": 403
            }),
            "a valid non-peer API key must be refused for an ACL mismatch on {method} {path}"
        );
    }

    let admin_key = key_store
        .lookup(ADMIN_KEY)
        .expect("admin key must be present in the store");
    assert!(
        super::ensure_route_acl_allows_request(
            &key_store,
            &admin_key,
            ADMIN_KEY,
            &axum::http::Method::GET,
            "/internal/ops",
        )
        .is_none(),
        "the admin key must stay authorized for /internal/ops"
    );
}
