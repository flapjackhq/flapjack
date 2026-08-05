use super::*;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axum::extract::ConnectInfo;
use axum::routing::get;
use axum::Router;
use rcgen::{generate_simple_self_signed, CertifiedKey as RcgenCertifiedKey};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::RootCertStore;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{oneshot, Notify};
use tokio_rustls::TlsConnector;

pub(super) struct TestCertFiles {
    pub(super) cert_path: std::path::PathBuf,
    pub(super) key_path: std::path::PathBuf,
    pub(super) cert_der: CertificateDer<'static>,
}

struct OneShotAcceptError {
    listener: TcpListener,
    error_pending: AtomicBool,
}

impl TcpAccept for OneShotAcceptError {
    async fn accept(&self) -> std::io::Result<(tokio::net::TcpStream, SocketAddr)> {
        if self.error_pending.swap(false, Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "injected transient accept failure",
            ));
        }
        self.listener.accept().await
    }
}

struct TimedAcceptState {
    calls: AtomicUsize,
    error_injected: Notify,
    accept_times: std::sync::Mutex<Vec<tokio::time::Instant>>,
}

/// Accepts one real connection, injects one resource-pressure error, then
/// delegates to the real listener, recording every accept attempt time.
struct TimedAcceptError {
    listener: TcpListener,
    state: Arc<TimedAcceptState>,
}

impl TcpAccept for TimedAcceptError {
    async fn accept(&self) -> std::io::Result<(tokio::net::TcpStream, SocketAddr)> {
        let call = self.state.calls.fetch_add(1, Ordering::SeqCst);
        self.state
            .accept_times
            .lock()
            .expect("accept times lock should not be poisoned")
            .push(tokio::time::Instant::now());
        if call == 1 {
            self.state.error_injected.notify_one();
            return Err(std::io::Error::other("injected resource exhaustion"));
        }
        self.listener.accept().await
    }
}

pub(super) fn write_test_cert_files(temp_dir: &TempDir) -> TestCertFiles {
    write_named_test_cert_files(temp_dir, "cert")
}

pub(super) fn write_named_test_cert_files(temp_dir: &TempDir, name: &str) -> TestCertFiles {
    let RcgenCertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("test certificate should generate");
    let cert_path = temp_dir.path().join(format!("{name}.pem"));
    let key_path = temp_dir.path().join(format!("{name}_key.pem"));
    std::fs::write(&cert_path, cert.pem()).expect("test cert should write");
    std::fs::write(&key_path, key_pair.serialize_pem()).expect("test key should write");

    TestCertFiles {
        cert_path,
        key_path,
        cert_der: cert.der().clone(),
    }
}

fn leaf_der(certified_key: &CertifiedKey) -> &[u8] {
    certified_key
        .cert
        .first()
        .expect("test certified key should contain a leaf certificate")
        .as_ref()
}

pub(super) fn tls_paths(cert_path: impl AsRef<Path>, key_path: impl AsRef<Path>) -> TlsPaths {
    TlsPaths {
        cert_path: cert_path.as_ref().to_path_buf(),
        key_path: key_path.as_ref().to_path_buf(),
    }
}

fn assert_tls_config_error(paths: &TlsPaths, expected: String) {
    assert_eq!(load_tls_config(paths).unwrap_err(), expected);
}

pub(super) fn assert_current_leaf_der(resolver: &ReloadableTlsResolver, expected_der: &[u8]) {
    assert_eq!(
        leaf_der(&resolver.current_key()),
        expected_der,
        "resolver should keep serving the last validated leaf certificate DER"
    );
}

pub(super) async fn tls_http_request(
    bind_addr: SocketAddr,
    cert_der: CertificateDer<'static>,
    path: &str,
) -> (SocketAddr, String) {
    let mut roots = RootCertStore::empty();
    roots
        .add(cert_der)
        .expect("test root certificate should be trusted");
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(client_config));
    let tcp = tokio::net::TcpStream::connect(bind_addr)
        .await
        .expect("TLS test server should accept TCP connections");
    let local_addr = tcp.local_addr().expect("client socket should have address");
    let server_name = ServerName::try_from("localhost").expect("localhost is a DNS name");
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .expect("TLS handshake should complete");
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    tls.write_all(request.as_bytes())
        .await
        .expect("TLS request should write");
    let mut response = String::new();
    tls.read_to_string(&mut response)
        .await
        .expect("TLS response should read");
    (local_addr, response)
}

pub(super) fn response_body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .expect("HTTP response should contain headers and body")
        .1
}

#[tokio::test]
async fn tls_request_preserves_connect_info_socket_addr() {
    let temp_dir = TempDir::new().unwrap();
    let cert_files = write_test_cert_files(&temp_dir);
    let config = load_tls_config(&tls_paths(&cert_files.cert_path, &cert_files.key_path))
        .expect("TLS config should load")
        .config;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind_addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let app = Router::new().route(
        "/peer",
        get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.to_string() }),
    );
    let server = tokio::spawn(serve_tls(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
        config,
        async {
            let _ = shutdown_rx.await;
        },
    ));

    let (client_addr, response) = tls_http_request(bind_addr, cert_files.cert_der, "/peer").await;
    assert_eq!(response_body(&response), client_addr.to_string());

    shutdown_tx.send(()).expect("shutdown signal should send");
    server
        .await
        .expect("TLS server task should join")
        .expect("TLS server should shut down cleanly");
}

#[tokio::test]
async fn tls_shutdown_waits_for_in_flight_response() {
    let temp_dir = TempDir::new().unwrap();
    let cert_files = write_test_cert_files(&temp_dir);
    let config = load_tls_config(&tls_paths(&cert_files.cert_path, &cert_files.key_path))
        .expect("TLS config should load")
        .config;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind_addr = listener.local_addr().unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let app = Router::new().route(
        "/slow",
        get({
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            move || {
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                async move {
                    entered.notify_one();
                    release.notified().await;
                    "finished"
                }
            }
        }),
    );
    let server = tokio::spawn(serve_tls(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
        config,
        async {
            let _ = shutdown_rx.await;
        },
    ));
    let client = tokio::spawn(tls_http_request(bind_addr, cert_files.cert_der, "/slow"));

    entered.notified().await;
    shutdown_tx.send(()).expect("shutdown signal should send");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !server.is_finished(),
        "TLS server must wait for the in-flight response before returning"
    );

    release.notify_one();
    let (_client_addr, response) = client.await.expect("client task should join");
    assert_eq!(response_body(&response), "finished");
    server
        .await
        .expect("TLS server task should join")
        .expect("TLS server should shut down cleanly");
}

#[tokio::test]
async fn idle_tls_handshake_does_not_block_shutdown() {
    let temp_dir = TempDir::new().unwrap();
    let cert_files = write_test_cert_files(&temp_dir);
    let config = load_tls_config(&tls_paths(&cert_files.cert_path, &cert_files.key_path))
        .expect("TLS config should load")
        .config;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind_addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let app = Router::new().route("/ready", get(|| async { "ready" }));
    let server = tokio::spawn(serve_tls(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
        config,
        async {
            let _ = shutdown_rx.await;
        },
    ));
    let idle_client = tokio::net::TcpStream::connect(bind_addr)
        .await
        .expect("TLS test server should accept TCP connections");
    assert!(
        idle_client.local_addr().is_ok(),
        "idle client setup must hold an open TCP connection before shutdown"
    );

    shutdown_tx.send(()).expect("shutdown signal should send");
    tokio::time::timeout(std::time::Duration::from_secs(1), server)
        .await
        .expect("idle TLS handshake must not block server shutdown")
        .expect("TLS server task should join")
        .expect("TLS server should shut down cleanly");
    drop(idle_client);
}

#[tokio::test]
async fn accept_error_does_not_stop_tls_serving_or_skip_caller_drain() {
    let temp_dir = TempDir::new().unwrap();
    let cert_files = write_test_cert_files(&temp_dir);
    let config = load_tls_config(&tls_paths(&cert_files.cert_path, &cert_files.key_path))
        .expect("TLS config should load")
        .config;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind_addr = listener.local_addr().unwrap();
    let listener = OneShotAcceptError {
        listener,
        error_pending: AtomicBool::new(true),
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (drain_tx, drain_rx) = oneshot::channel();

    let app = Router::new().route("/ready", get(|| async { "ready" }));
    let server = tokio::spawn(async move {
        serve_tls(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
            config,
            async {
                let _ = shutdown_rx.await;
            },
        )
        .await?;
        drain_tx
            .send(())
            .expect("caller-side write-queue drain should run");
        Ok::<(), BoxError>(())
    });

    tokio::task::yield_now().await;
    assert!(
        !server.is_finished(),
        "an injected accept error must not terminate TLS serving"
    );
    let (_client_addr, response) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tls_http_request(bind_addr, cert_files.cert_der, "/ready"),
    )
    .await
    .expect("TLS request after an accept error should not hang");
    assert_eq!(response_body(&response), "ready");

    shutdown_tx.send(()).expect("shutdown signal should send");
    tokio::time::timeout(std::time::Duration::from_secs(1), drain_rx)
        .await
        .expect("caller-side write-queue drain should be reached")
        .expect("caller-side write-queue drain signal should send");
    server
        .await
        .expect("TLS server task should join")
        .expect("TLS server should shut down cleanly");
}

#[tokio::test(start_paused = true)]
async fn handshake_completion_does_not_shorten_accept_backoff() {
    let temp_dir = TempDir::new().unwrap();
    let cert_files = write_test_cert_files(&temp_dir);
    let config = load_tls_config(&tls_paths(&cert_files.cert_path, &cert_files.key_path))
        .expect("TLS config should load")
        .config;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind_addr = listener.local_addr().unwrap();
    let state = Arc::new(TimedAcceptState {
        calls: AtomicUsize::new(0),
        error_injected: Notify::new(),
        accept_times: std::sync::Mutex::new(Vec::new()),
    });
    let listener = TimedAcceptError {
        listener,
        state: Arc::clone(&state),
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let app = Router::new().route("/ready", get(|| async { "ready" }));
    let server = tokio::spawn(serve_tls(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
        config,
        async {
            let _ = shutdown_rx.await;
        },
    ));

    // Accept 1: a real TCP connection whose TLS handshake is held back.
    let tcp = tokio::net::TcpStream::connect(bind_addr)
        .await
        .expect("TLS test server should accept TCP connections");
    // Accept 2: injected resource-pressure error starts the one-second backoff.
    state.error_injected.notified().await;

    // Pin the paused clock: while the spinner keeps a task runnable the
    // runtime never goes idle, so paused time cannot auto-advance to the
    // backoff deadline before the handshake below has completed.
    let clock_pin = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });

    // Completing a TLS handshake during the backoff must not let the next
    // accept attempt fire before the full retry delay has elapsed.
    let mut roots = RootCertStore::empty();
    roots
        .add(cert_files.cert_der)
        .expect("test root certificate should be trusted");
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(client_config));
    let server_name = ServerName::try_from("localhost").expect("localhost is a DNS name");
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .expect("TLS handshake should complete during the accept backoff");
    // A full request/response proves the accept loop has observed the
    // completed handshake before we inspect the accept attempt times.
    tls.write_all(b"GET /ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("TLS request should write");
    let mut response = String::new();
    tls.read_to_string(&mut response)
        .await
        .expect("TLS response should read");
    assert_eq!(response_body(&response), "ready");
    clock_pin.abort();

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if state.accept_times.lock().unwrap().len() >= 3 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("accept loop should retry after the backoff deadline");

    {
        let accept_times = state.accept_times.lock().unwrap();
        assert!(
            accept_times[2] - accept_times[1] >= ACCEPT_ERROR_RETRY_DELAY,
            "handshake completion must not shorten the accept backoff: {accept_times:?}"
        );
    }

    drop(tls);
    shutdown_tx.send(()).expect("shutdown signal should send");
    tokio::time::timeout(std::time::Duration::from_secs(10), server)
        .await
        .expect("TLS server should shut down")
        .expect("TLS server task should join")
        .expect("TLS server should shut down cleanly");
}

#[test]
fn accept_error_retry_matches_axum_backoff_policy() {
    assert_eq!(
        accept_error_retry_delay(&std::io::Error::from(std::io::ErrorKind::ConnectionAborted)),
        None
    );
    assert_eq!(
        accept_error_retry_delay(&std::io::Error::from(std::io::ErrorKind::Other)),
        Some(std::time::Duration::from_secs(1))
    );
}

#[test]
fn resolver_current_material_updates_after_valid_publication() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let cert_b = write_named_test_cert_files(&temp_dir, "cert_b");
    assert_ne!(
        cert_a.cert_der, cert_b.cert_der,
        "test pairs must have distinct leaf certificate DER"
    );
    let resolver = Arc::new(ReloadableTlsResolver::new(
        load_certified_key(&tls_paths(&cert_a.cert_path, &cert_a.key_path))
            .expect("pair A should load"),
        tls_paths(&cert_a.cert_path, &cert_a.key_path),
    ));
    let _config = tls_config_with_resolver(resolver.clone());

    assert_current_leaf_der(&resolver, cert_a.cert_der.as_ref());

    resolver
        .publish_from_paths(&tls_paths(&cert_b.cert_path, &cert_b.key_path))
        .expect("pair B should validate before publication");

    assert_eq!(
        leaf_der(&resolver.current_key()),
        cert_b.cert_der.as_ref(),
        "post-publication DER equality: resolver must return pair B's exact leaf DER"
    );
}

#[test]
fn reload_rejections_keep_serving_last_valid_certificate() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let cert_b = write_named_test_cert_files(&temp_dir, "cert_b");
    let missing_cert = temp_dir.path().join("missing_reload_cert.pem");
    let malformed_cert = temp_dir.path().join("malformed_reload_cert.pem");
    std::fs::write(
        &malformed_cert,
        "-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    assert!(
        !missing_cert.exists(),
        "missing reload fixture must stay absent for the read-error assertion to be real"
    );

    let resolver = ReloadableTlsResolver::new(
        load_certified_key(&tls_paths(&cert_a.cert_path, &cert_a.key_path))
            .expect("pair A should load"),
        tls_paths(&cert_a.cert_path, &cert_a.key_path),
    );

    assert_eq!(
        resolver
            .publish_from_paths(&tls_paths(&missing_cert, &cert_b.key_path))
            .unwrap_err(),
        format!(
            "failed to read TLS certificate file {}: file not found",
            missing_cert.display()
        )
    );
    assert_current_leaf_der(&resolver, cert_a.cert_der.as_ref());

    assert_eq!(
        resolver
            .publish_from_paths(&tls_paths(&malformed_cert, &cert_b.key_path))
            .unwrap_err(),
        format!(
            "failed to parse TLS certificate file {}: invalid PEM data",
            malformed_cert.display()
        )
    );
    assert_current_leaf_der(&resolver, cert_a.cert_der.as_ref());

    assert_eq!(
        resolver
            .publish_from_paths(&tls_paths(&cert_b.cert_path, &cert_a.key_path))
            .unwrap_err(),
        "failed to build TLS server config: keys may not be consistent: KeyMismatch"
    );
    assert_current_leaf_der(&resolver, cert_a.cert_der.as_ref());
}

#[test]
fn tls_config_loader_rejects_certificate_key_mismatch() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let cert_b = write_named_test_cert_files(&temp_dir, "cert_b");

    assert_tls_config_error(
        &tls_paths(&cert_b.cert_path, &cert_a.key_path),
        "failed to build TLS server config: keys may not be consistent: KeyMismatch".to_string(),
    );
}

#[test]
fn tls_config_loader_reports_exact_pem_errors() {
    let temp_dir = TempDir::new().unwrap();
    let cert_files = write_test_cert_files(&temp_dir);
    let missing_cert = temp_dir.path().join("missing_cert.pem");
    let empty_cert = temp_dir.path().join("empty_cert.pem");
    let malformed_cert = temp_dir.path().join("malformed_cert.pem");
    let key_without_private_key = temp_dir.path().join("key_without_private_key.pem");
    let invalid_key = temp_dir.path().join("invalid_key.pem");
    let nonexistent_key = temp_dir.path().join("nonexistent_key.pem");

    std::fs::write(&empty_cert, "").unwrap();
    std::fs::write(
        &malformed_cert,
        "-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    std::fs::write(&key_without_private_key, "not a private key").unwrap();
    std::fs::write(
        &invalid_key,
        "-----BEGIN PRIVATE KEY-----\nnot-base64\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();

    assert_tls_config_error(
        &tls_paths(&missing_cert, &cert_files.key_path),
        format!(
            "failed to read TLS certificate file {}: file not found",
            missing_cert.display()
        ),
    );
    assert_tls_config_error(
        &tls_paths(&empty_cert, &cert_files.key_path),
        format!(
            "TLS certificate file {} did not contain any certificates",
            empty_cert.display()
        ),
    );
    assert_tls_config_error(
        &tls_paths(&malformed_cert, &cert_files.key_path),
        format!(
            "failed to parse TLS certificate file {}: invalid PEM data",
            malformed_cert.display()
        ),
    );
    assert!(
        !nonexistent_key.exists(),
        "nonexistent key fixture must stay absent for the read-error assertion to be real: {}",
        nonexistent_key.display()
    );
    assert_tls_config_error(
        &tls_paths(&cert_files.cert_path, &nonexistent_key),
        format!(
            "failed to read TLS private key file {}: file not found",
            nonexistent_key.display()
        ),
    );
    assert_tls_config_error(
        &tls_paths(&cert_files.cert_path, &key_without_private_key),
        format!(
            "TLS private key file {} did not contain a private key",
            key_without_private_key.display()
        ),
    );
    assert_tls_config_error(
        &tls_paths(&cert_files.cert_path, &invalid_key),
        format!(
            "failed to parse TLS private key file {}: invalid PEM data",
            invalid_key.display()
        ),
    );
}

#[cfg(unix)]
#[test]
fn rustls_pki_types_classifies_missing_private_key_as_no_items() {
    let mut reader = std::io::Cursor::new(b"not a private key");
    assert!(matches!(
        PrivateKeyDer::from_pem_reader(&mut reader),
        Err(rustls_pki_types::pem::Error::NoItemsFound)
    ));
}
