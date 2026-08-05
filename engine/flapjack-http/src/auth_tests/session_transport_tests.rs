//! External transport contract for dashboard cookie sessions.
//!
//! Stage 1 intentionally leaves these tests red. They use the full router so a
//! missing route, ACL mapping, cookie authenticator, or revocation call cannot be
//! hidden by a test-only handler.

use super::*;
use crate::auth::middleware::DASHBOARD_SESSION_EXCHANGES_PER_IP_PER_HOUR;
use crate::auth::route_acl::RouteAcl;
use crate::startup::TlsPaths;
use crate::test_helpers::{
    build_test_router, build_test_router_for_data_dir, send_empty_request, send_json_request,
    send_request_with_headers,
};
use axum::http::{header::SET_COOKIE, Method};
use rcgen::{generate_simple_self_signed, CertifiedKey};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::RootCertStore;
use serde_json::json;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio_rustls::TlsConnector;

const ADMIN_KEY: &str = "dashboard-session-admin-key";
const APPLICATION_ID: &str = "flapjack";
const SESSION_PATH: &str = "/1/dashboard/session";
const SESSION_COOKIE_NAME: &str = "flapjack_dashboard_session";
const INVALID_CREDENTIALS: &str = "Invalid Application-ID or API key";

fn session_router() -> (TempDir, Arc<KeyStore>, Router) {
    let temp_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(temp_dir.path(), ADMIN_KEY));
    let app = build_test_router(&temp_dir, Some(Arc::clone(&key_store)));
    (temp_dir, key_store, app)
}

fn session_request_body(api_key: &str) -> serde_json::Value {
    json!({ "apiKey": api_key })
}

struct NativeTlsCertFiles {
    cert_path: PathBuf,
    key_path: PathBuf,
    cert_der: CertificateDer<'static>,
}

fn write_native_tls_test_cert_files(temp_dir: &TempDir) -> NativeTlsCertFiles {
    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("native TLS session test certificate should generate");
    let cert_path = temp_dir.path().join("native_tls_cert.pem");
    let key_path = temp_dir.path().join("native_tls_key.pem");
    std::fs::write(&cert_path, cert.pem()).expect("native TLS session cert should write");
    std::fs::write(&key_path, key_pair.serialize_pem())
        .expect("native TLS session key should write");

    NativeTlsCertFiles {
        cert_path,
        key_path,
        cert_der: cert.der().clone(),
    }
}

fn native_tls_paths(cert_path: impl AsRef<Path>, key_path: impl AsRef<Path>) -> TlsPaths {
    TlsPaths {
        cert_path: cert_path.as_ref().to_path_buf(),
        key_path: key_path.as_ref().to_path_buf(),
    }
}

async fn assert_invalid_credentials(response: axum::response::Response) {
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(response).await,
        json!({
            "message": INVALID_CREDENTIALS,
            "status": 403,
        })
    );
}

fn single_set_cookie(response: &axum::response::Response) -> String {
    let values: Vec<_> = response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .map(|value| {
            value
                .to_str()
                .expect("session cookie must be an ASCII header")
                .to_string()
        })
        .collect();
    assert_eq!(
        values.len(),
        1,
        "a successful exchange must emit exactly one Set-Cookie header"
    );
    values.into_iter().next().unwrap()
}

fn cookie_token(set_cookie: &str) -> &str {
    let pair = set_cookie
        .split(';')
        .next()
        .expect("Set-Cookie must contain a name/value pair");
    let (name, token) = pair
        .split_once('=')
        .expect("session Set-Cookie must contain an equals sign");
    assert_eq!(name, SESSION_COOKIE_NAME);
    assert!(!token.is_empty(), "session cookie token must not be empty");
    token
}

fn has_cookie_attribute(set_cookie: &str, expected: &str) -> bool {
    set_cookie
        .split(';')
        .skip(1)
        .map(str::trim)
        .any(|attribute| attribute.eq_ignore_ascii_case(expected))
}

async fn assert_success_body_has_no_token(response: axum::response::Response, session_token: &str) {
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let serialized = String::from_utf8(body.to_vec()).expect("success body must be UTF-8 JSON");
    assert!(
        !serialized.contains(session_token),
        "the opaque session token must exist only in Set-Cookie"
    );
    assert!(
        !serialized.contains(ADMIN_KEY),
        "the admin key must never be reflected in the exchange body"
    );
}

async fn exchange_session(app: &Router) -> axum::response::Response {
    send_json_request(
        app,
        Method::POST,
        SESSION_PATH,
        session_request_body(ADMIN_KEY),
    )
    .await
}

async fn native_tls_session_exchange(app: Router) -> (u16, Vec<String>) {
    let cert_dir = TempDir::new().unwrap();
    let cert_files = write_native_tls_test_cert_files(&cert_dir);
    let tls_config = crate::tls_serve::load_tls_config(&native_tls_paths(
        &cert_files.cert_path,
        &cert_files.key_path,
    ))
    .expect("native TLS session test config should load")
    .config;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("native TLS session test listener should bind");
    let bind_addr = listener
        .local_addr()
        .expect("native TLS session test listener should expose its address");
    assert!(
        bind_addr.port() > 0 && bind_addr.ip().is_loopback(),
        "native TLS exchange precondition: listener must be bound on loopback before the request"
    );
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(crate::tls_serve::serve_tls(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
        tls_config,
        async {
            let _ = shutdown_rx.await;
        },
    ));

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tls_http_session_exchange(bind_addr, cert_files.cert_der),
    )
    .await
    .expect("native TLS session exchange should not hang");
    shutdown_tx
        .send(())
        .expect("native TLS session server shutdown signal should send");
    server
        .await
        .expect("native TLS session server task should join")
        .expect("native TLS session server should shut down cleanly");
    response
}

async fn tls_http_session_exchange(
    bind_addr: SocketAddr,
    cert_der: CertificateDer<'static>,
) -> (u16, Vec<String>) {
    let mut roots = RootCertStore::empty();
    roots
        .add(cert_der)
        .expect("native TLS session root certificate should be trusted");
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(client_config));
    let tcp = tokio::net::TcpStream::connect(bind_addr)
        .await
        .expect("native TLS session server should accept TCP connections");
    let server_name = ServerName::try_from("localhost").expect("localhost is a DNS name");
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .expect("native TLS session handshake should complete");
    let body = session_request_body(ADMIN_KEY).to_string();
    let request = format!(
        "POST {SESSION_PATH} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    tls.write_all(request.as_bytes())
        .await
        .expect("native TLS session request should write");
    let mut response = String::new();
    tls.read_to_string(&mut response)
        .await
        .expect("native TLS session response should read");

    (
        http_status_code(&response),
        set_cookie_headers_from_http_response(&response),
    )
}

fn http_status_code(response: &str) -> u16 {
    let status = response
        .lines()
        .next()
        .expect("native TLS response should include a status line")
        .split_whitespace()
        .nth(1)
        .expect("native TLS response status line should include a status code");
    status
        .parse()
        .expect("native TLS response status code should be numeric")
}

fn set_cookie_headers_from_http_response(response: &str) -> Vec<String> {
    response
        .lines()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("set-cookie")
                .then(|| value.trim().to_string())
        })
        .collect()
}

#[tokio::test]
async fn session_routes_have_explicit_acl_and_exposure_contracts() {
    assert_eq!(
        required_acl_for_route(&Method::POST, SESSION_PATH),
        RouteAcl::Public,
        "the exchange handler owns one-time admin-key validation"
    );
    assert_eq!(
        required_acl_for_route(&Method::DELETE, SESSION_PATH),
        RouteAcl::Required("admin"),
        "logout must require an authenticated admin session"
    );

    let temp_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(temp_dir.path(), ADMIN_KEY));
    let downstream_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&downstream_calls);
    let app = Router::new()
        .route(
            SESSION_PATH,
            axum::routing::any(move || {
                let calls_for_handler = Arc::clone(&calls_for_handler);
                async move {
                    calls_for_handler.fetch_add(1, Ordering::SeqCst);
                    StatusCode::IM_A_TEAPOT
                }
            }),
        )
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }))
        .layer(Extension(RateLimiter::new()))
        .layer(Extension(key_store));

    let exchange = send_empty_request(&app, Method::POST, SESSION_PATH).await;
    assert_eq!(
        exchange.status(),
        StatusCode::IM_A_TEAPOT,
        "unauthenticated POST must pass shared middleware and reach the exchange handler"
    );

    let logout = send_empty_request(&app, Method::DELETE, SESSION_PATH).await;
    assert_invalid_credentials(logout).await;
    assert_eq!(
        downstream_calls.load(Ordering::SeqCst),
        1,
        "unauthenticated DELETE must be rejected by shared middleware before the logout handler"
    );
}

#[tokio::test]
async fn native_tls_listener_sets_secure_cookie_attribute() {
    let (_temp_dir, _key_store, app) = session_router();

    let (status, set_cookies) = native_tls_session_exchange(app).await;

    assert_eq!(
        status,
        StatusCode::OK.as_u16(),
        "a valid admin key must be exchangeable through Flapjack's native TLS listener"
    );
    assert_eq!(
        set_cookies.len(),
        1,
        "native TLS exchange must emit exactly one Set-Cookie header"
    );
    let set_cookie = set_cookies.into_iter().next().unwrap();
    cookie_token(&set_cookie);
    assert!(has_cookie_attribute(&set_cookie, "HttpOnly"));
    assert!(has_cookie_attribute(&set_cookie, "SameSite=Strict"));
    assert!(has_cookie_attribute(&set_cookie, "Path=/"));
    assert!(
        has_cookie_attribute(&set_cookie, "Secure"),
        "Flapjack's native TLS serving seam must attach the shared transport-security marker used for Secure session cookies"
    );
}

#[tokio::test]
async fn admin_exchange_sets_one_plaintext_safe_cookie_without_returning_token_material() {
    let (_temp_dir, key_store, app) = session_router();
    let (_, search_key) = key_store.create_key(test_search_api_key("session exchange rejection"));

    let rejected = send_json_request(
        &app,
        Method::POST,
        SESSION_PATH,
        session_request_body(&search_key),
    )
    .await;
    assert_invalid_credentials(rejected).await;

    let response = exchange_session(&app).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a valid admin key must be exchangeable at the mounted session route"
    );
    let set_cookie = single_set_cookie(&response);
    let token = cookie_token(&set_cookie).to_string();
    assert!(has_cookie_attribute(&set_cookie, "HttpOnly"));
    assert!(has_cookie_attribute(&set_cookie, "SameSite=Strict"));
    assert!(has_cookie_attribute(&set_cookie, "Path=/"));
    assert!(
        !has_cookie_attribute(&set_cookie, "Secure"),
        "the plaintext router must issue a cookie usable by local HTTP operators"
    );
    assert_success_body_has_no_token(response, &token).await;
}

#[tokio::test]
async fn session_exchange_is_rate_limited_per_trusted_client_ip() {
    let (_temp_dir, _key_store, app) = session_router();
    let first_peer: SocketAddr = "192.0.2.10:43123".parse().unwrap();
    for attempt in 0..DASHBOARD_SESSION_EXCHANGES_PER_IP_PER_HOUR {
        let response = send_request_with_headers(
            &app,
            Method::POST,
            SESSION_PATH,
            Some(session_request_body("wrong-admin-key")),
            &[],
            Some(first_peer),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "attempt {attempt} must reach credential validation before the budget is exhausted"
        );
    }

    let limited = send_request_with_headers(
        &app,
        Method::POST,
        SESSION_PATH,
        Some(session_request_body(ADMIN_KEY)),
        &[],
        Some(first_peer),
    )
    .await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        body_json(limited).await,
        json!({
            "message": "Too many dashboard session exchange attempts per IP per hour",
            "status": 429,
        })
    );

    let second_peer: SocketAddr = "192.0.2.11:43123".parse().unwrap();
    let independent_peer = send_request_with_headers(
        &app,
        Method::POST,
        SESSION_PATH,
        Some(session_request_body(ADMIN_KEY)),
        &[],
        Some(second_peer),
    )
    .await;
    assert_eq!(
        independent_peer.status(),
        StatusCode::OK,
        "each trusted client IP must have an independent exchange budget"
    );
}

#[tokio::test]
async fn trusted_tls_forwarding_signal_sets_secure_cookie_attribute() {
    let (_temp_dir, _key_store, app) = session_router();
    let loopback_proxy: SocketAddr = "127.0.0.1:43123".parse().unwrap();
    let response = send_request_with_headers(
        &app,
        Method::POST,
        SESSION_PATH,
        Some(session_request_body(ADMIN_KEY)),
        &[("x-forwarded-proto", "https")],
        Some(loopback_proxy),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = single_set_cookie(&response);
    assert!(has_cookie_attribute(&set_cookie, "HttpOnly"));
    assert!(has_cookie_attribute(&set_cookie, "SameSite=Strict"));
    assert!(has_cookie_attribute(&set_cookie, "Path=/"));
    assert!(
        has_cookie_attribute(&set_cookie, "Secure"),
        "trusted HTTPS forwarding must produce a Secure cookie"
    );
}

#[tokio::test]
async fn untrusted_forwarded_https_signal_does_not_set_secure_cookie_attribute() {
    let (_temp_dir, _key_store, app) = session_router();
    let untrusted_peer: SocketAddr = "203.0.113.10:43123".parse().unwrap();
    let response = send_request_with_headers(
        &app,
        Method::POST,
        SESSION_PATH,
        Some(session_request_body(ADMIN_KEY)),
        &[("x-forwarded-proto", "https")],
        Some(untrusted_peer),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = single_set_cookie(&response);
    assert!(has_cookie_attribute(&set_cookie, "HttpOnly"));
    assert!(has_cookie_attribute(&set_cookie, "SameSite=Strict"));
    assert!(has_cookie_attribute(&set_cookie, "Path=/"));
    assert!(
        !has_cookie_attribute(&set_cookie, "Secure"),
        "untrusted peers must not be able to spoof HTTPS forwarding into a Secure cookie"
    );
}

#[tokio::test]
async fn cookie_authenticates_dashboard_route_without_breaking_header_auth() {
    let (_temp_dir, _key_store, app) = session_router();

    let header_response = send_request_with_headers(
        &app,
        Method::GET,
        "/1/indexes",
        None,
        &[
            ("x-algolia-application-id", APPLICATION_ID),
            ("x-algolia-api-key", ADMIN_KEY),
        ],
        None,
    )
    .await;
    assert_eq!(
        header_response.status(),
        StatusCode::OK,
        "Algolia-compatible API-key header authentication must remain supported"
    );

    let exchange = exchange_session(&app).await;
    assert_eq!(exchange.status(), StatusCode::OK);
    let cookie = single_set_cookie(&exchange)
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let cookie_response = send_request_with_headers(
        &app,
        Method::GET,
        "/1/indexes",
        None,
        &[
            ("x-algolia-application-id", APPLICATION_ID),
            ("cookie", &cookie),
        ],
        None,
    )
    .await;
    assert_eq!(
        cookie_response.status(),
        StatusCode::OK,
        "the session cookie must authenticate a route used by the dashboard"
    );
}

#[tokio::test]
async fn session_cookie_survives_full_router_rebuild_from_same_data_dir() {
    let data_dir = TempDir::new().unwrap();
    let first_key_store = Arc::new(KeyStore::load_or_create(data_dir.path(), ADMIN_KEY));
    let first_app = build_test_router_for_data_dir(
        &data_dir,
        Some(Arc::clone(&first_key_store)),
        data_dir.path(),
    );

    let exchange = exchange_session(&first_app).await;
    assert_eq!(exchange.status(), StatusCode::OK);
    let cookie = single_set_cookie(&exchange)
        .split(';')
        .next()
        .unwrap()
        .to_string();
    drop(first_app);
    drop(first_key_store);

    let reopened_key_store = Arc::new(KeyStore::load_or_create(data_dir.path(), ADMIN_KEY));
    let reopened_app = build_test_router_for_data_dir(
        &data_dir,
        Some(Arc::clone(&reopened_key_store)),
        data_dir.path(),
    );
    let reopened_response = send_request_with_headers(
        &reopened_app,
        Method::GET,
        "/1/indexes",
        None,
        &[
            ("x-algolia-application-id", APPLICATION_ID),
            ("cookie", &cookie),
        ],
        None,
    )
    .await;

    assert_eq!(
        reopened_response.status(),
        StatusCode::OK,
        "a session minted through the transport must reopen through DashboardSessionStore"
    );
}

#[tokio::test]
async fn logout_revokes_session_cookie_while_health_stays_public() {
    let (_temp_dir, _key_store, app) = session_router();

    let health = send_empty_request(&app, Method::GET, "/health").await;
    assert_eq!(
        health.status(),
        StatusCode::OK,
        "dashboard session auth must not protect the existing health route"
    );

    let exchange = exchange_session(&app).await;
    assert_eq!(exchange.status(), StatusCode::OK);
    let cookie = single_set_cookie(&exchange)
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let logout = send_request_with_headers(
        &app,
        Method::DELETE,
        SESSION_PATH,
        None,
        &[
            ("x-algolia-application-id", APPLICATION_ID),
            ("cookie", &cookie),
        ],
        None,
    )
    .await;
    assert_eq!(
        logout.status(),
        StatusCode::NO_CONTENT,
        "logout must revoke the presented session through DashboardSessionStore"
    );

    let replay = send_request_with_headers(
        &app,
        Method::GET,
        "/1/indexes",
        None,
        &[
            ("x-algolia-application-id", APPLICATION_ID),
            ("cookie", &cookie),
        ],
        None,
    )
    .await;
    assert_invalid_credentials(replay).await;
}
