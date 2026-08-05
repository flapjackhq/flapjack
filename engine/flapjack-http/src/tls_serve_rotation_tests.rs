use super::tls_serve_tests::{
    assert_current_leaf_der, response_body, tls_http_request, tls_paths,
    write_named_test_cert_files, write_test_cert_files, TestCertFiles,
};
use super::*;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::routing::{get, head, post};
use axum::{extract::State, Json, Router};
use base64::Engine as _;
use rcgen::{CertificateParams, KeyPair, PublicKeyData, SignatureAlgorithm};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

#[cfg(unix)]
pub(super) fn publish_symlink_generation(
    temp_dir: &TempDir,
    material_dir: &Path,
    generation_name: &str,
    cert_files: &TestCertFiles,
) -> std::path::PathBuf {
    let generation = temp_dir.path().join(generation_name);
    std::fs::create_dir_all(&generation).unwrap();
    std::fs::copy(&cert_files.cert_path, generation.join(FULLCHAIN_FILE_NAME)).unwrap();
    std::fs::copy(&cert_files.key_path, generation.join(PRIVATE_KEY_FILE_NAME)).unwrap();
    if material_dir.exists() {
        std::fs::remove_file(material_dir).unwrap();
    }
    std::os::unix::fs::symlink(generation.file_name().unwrap(), material_dir).unwrap();
    generation
}

#[cfg(unix)]
pub(super) fn publish_real_dir_generation(
    material_dir: &Path,
    generation_name: &str,
    cert_files: &TestCertFiles,
) -> std::path::PathBuf {
    let target = Path::new("..").join(generation_name);
    let generation = material_dir
        .parent()
        .expect("material directory fixture should have a parent")
        .join(generation_name);
    std::fs::create_dir_all(&generation).unwrap();
    std::fs::copy(&cert_files.cert_path, generation.join(FULLCHAIN_FILE_NAME)).unwrap();
    std::fs::copy(&cert_files.key_path, generation.join(PRIVATE_KEY_FILE_NAME)).unwrap();
    let current = material_dir.join(CURRENT_LINK_NAME);
    let next = material_dir.join("next");
    if next.exists() {
        std::fs::remove_file(&next).unwrap();
    }
    std::os::unix::fs::symlink(&target, &next).unwrap();
    std::fs::rename(next, current).unwrap();
    for file_name in [FULLCHAIN_FILE_NAME, PRIVATE_KEY_FILE_NAME] {
        let visible_path = material_dir.join(file_name);
        if visible_path.exists() {
            std::fs::remove_file(&visible_path).unwrap();
        }
        std::os::unix::fs::symlink(Path::new(CURRENT_LINK_NAME).join(file_name), visible_path)
            .unwrap();
    }
    material_dir.join(target)
}

pub(super) fn test_resolver(cert_files: &TestCertFiles) -> ReloadableTlsResolver {
    ReloadableTlsResolver::new(
        load_certified_key(&tls_paths(&cert_files.cert_path, &cert_files.key_path))
            .expect("test TLS pair should load"),
        tls_paths(&cert_files.cert_path, &cert_files.key_path),
    )
}

pub(super) fn startup_resolver_under_material(
    material_dir: &Path,
    cert_files: &TestCertFiles,
) -> ReloadableTlsResolver {
    let startup_dir = material_dir.join("startup");
    std::fs::create_dir_all(&startup_dir).unwrap();
    let startup_cert_path = startup_dir.join("certificate.pem");
    let startup_key_path = startup_dir.join("private_key.pem");
    std::fs::copy(&cert_files.cert_path, &startup_cert_path).unwrap();
    std::fs::copy(&cert_files.key_path, &startup_key_path).unwrap();
    test_resolver(&TestCertFiles {
        cert_path: startup_cert_path,
        key_path: startup_key_path,
        cert_der: cert_files.cert_der.clone(),
    })
}

async fn start_test_acme_account_server(
    cert_files: &TestCertFiles,
) -> (
    String,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), BoxError>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test ACME listener must bind before the client starts");
    let bind_addr = listener.local_addr().unwrap();
    let base_url = format!("https://localhost:{}", bind_addr.port());
    let directory = serde_json::json!({
        "newNonce": format!("{base_url}/nonce"),
        "newAccount": format!("{base_url}/account"),
        "newOrder": format!("{base_url}/order"),
        "revokeCert": format!("{base_url}/revoke")
    });
    let account_location = format!("{base_url}/account/1");
    let acme_state = TestAcmeState {
        base_url: base_url.clone(),
        issued_certificate: Arc::new(std::sync::Mutex::new(None)),
    };
    let app = Router::new()
        .route(
            "/directory",
            get(move || {
                let directory = directory.clone();
                async move { Json(directory) }
            }),
        )
        .route(
            "/nonce",
            head(|| async { ([("replay-nonce", "test-nonce")], "") }),
        )
        .route(
            "/account",
            post(move || {
                let account_location = account_location.clone();
                async move {
                    Response::builder()
                        .status(StatusCode::CREATED)
                        .header("replay-nonce", "account-nonce")
                        .header("location", account_location)
                        .body(axum::body::Body::from("{}"))
                        .unwrap()
                }
            }),
        )
        .route("/order", post(test_acme_new_order))
        .route("/authz/1", post(test_acme_authorization))
        .route("/order/1", post(test_acme_order))
        .route("/finalize/1", post(test_acme_finalize))
        .route("/certificate/1", post(test_acme_certificate))
        .with_state(acme_state);
    let config = load_tls_config(&tls_paths(&cert_files.cert_path, &cert_files.key_path))
        .expect("test ACME TLS config should load")
        .config;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(serve_tls(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
        config,
        async {
            let _ = shutdown_rx.await;
        },
    ));
    (format!("{base_url}/directory"), shutdown_tx, server)
}

#[derive(Clone)]
struct TestAcmeState {
    base_url: String,
    issued_certificate: Arc<std::sync::Mutex<Option<String>>>,
}

fn test_acme_response(
    status: StatusCode,
    body: serde_json::Value,
    location: Option<&str>,
) -> AxumResponse {
    let mut response = (status, [("replay-nonce", "test-nonce")], Json(body)).into_response();
    if let Some(location) = location {
        response.headers_mut().insert(
            axum::http::header::LOCATION,
            location.parse().expect("test ACME location must be valid"),
        );
    }
    response
}

fn test_acme_order_body(state: &TestAcmeState, status: &str) -> serde_json::Value {
    serde_json::json!({
        "status": status,
        "authorizations": [format!("{}/authz/1", state.base_url)],
        "finalize": format!("{}/finalize/1", state.base_url),
        "certificate": (status == "valid").then(|| format!("{}/certificate/1", state.base_url))
    })
}

async fn test_acme_new_order(State(state): State<TestAcmeState>) -> AxumResponse {
    test_acme_response(
        StatusCode::CREATED,
        test_acme_order_body(&state, "pending"),
        Some(&format!("{}/order/1", state.base_url)),
    )
}

async fn test_acme_authorization() -> AxumResponse {
    test_acme_response(
        StatusCode::OK,
        serde_json::json!({
            "identifier": { "type": "dns", "value": "localhost" },
            "status": "valid",
            "challenges": []
        }),
        None,
    )
}

async fn test_acme_order(State(state): State<TestAcmeState>) -> AxumResponse {
    let status = if state.issued_certificate.lock().unwrap().is_some() {
        "valid"
    } else {
        "ready"
    };
    test_acme_response(StatusCode::OK, test_acme_order_body(&state, status), None)
}

async fn test_acme_finalize(State(state): State<TestAcmeState>, body: Bytes) -> AxumResponse {
    let certificate = sign_test_acme_csr(&body);
    *state.issued_certificate.lock().unwrap() = Some(certificate);
    test_acme_response(StatusCode::OK, test_acme_order_body(&state, "valid"), None)
}

async fn test_acme_certificate(State(state): State<TestAcmeState>) -> AxumResponse {
    let certificate = state
        .issued_certificate
        .lock()
        .unwrap()
        .clone()
        .expect("finalization must issue a certificate before retrieval");
    (
        StatusCode::OK,
        [
            ("replay-nonce", "test-nonce"),
            ("content-type", "application/pem-certificate-chain"),
        ],
        certificate,
    )
        .into_response()
}

struct TestCsrPublicKey(Vec<u8>);

impl PublicKeyData for TestCsrPublicKey {
    fn der_bytes(&self) -> &[u8] {
        &self.0
    }

    fn algorithm(&self) -> &SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

fn sign_test_acme_csr(jws_body: &[u8]) -> String {
    let envelope: serde_json::Value = serde_json::from_slice(jws_body).unwrap();
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(envelope["payload"].as_str().unwrap())
        .unwrap();
    let finalize: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    let csr = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(finalize["csr"].as_str().unwrap())
        .unwrap();
    let public_key = TestCsrPublicKey(csr_public_key_bytes(&csr));
    let issuer_key = KeyPair::generate().unwrap();
    let issuer = CertificateParams::new(vec!["test-acme-issuer".to_string()])
        .unwrap()
        .self_signed(&issuer_key)
        .unwrap();
    CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .signed_by(&public_key, &issuer, &issuer_key)
        .unwrap()
        .pem()
}

fn csr_public_key_bytes(csr: &[u8]) -> Vec<u8> {
    let mut csr = csr;
    let mut outer = der_value(&mut csr, 0x30);
    let mut request_info = der_value(&mut outer, 0x30);
    der_value(&mut request_info, 0x02);
    der_value(&mut request_info, 0x30);
    let mut public_key_info = der_value(&mut request_info, 0x30);
    der_value(&mut public_key_info, 0x30);
    let public_key = der_value(&mut public_key_info, 0x03);
    assert_eq!(public_key.first(), Some(&0), "CSR key must be byte-aligned");
    public_key[1..].to_vec()
}

fn der_value<'a>(input: &mut &'a [u8], expected_tag: u8) -> &'a [u8] {
    assert_eq!(input.first(), Some(&expected_tag), "unexpected DER tag");
    let (header_len, value_len) = if input[1] < 0x80 {
        (2, input[1] as usize)
    } else {
        let length_bytes = (input[1] & 0x7f) as usize;
        let value_len = input[2..2 + length_bytes]
            .iter()
            .fold(0usize, |length, byte| (length << 8) | *byte as usize);
        (2 + length_bytes, value_len)
    };
    let value = &input[header_len..header_len + value_len];
    *input = &input[header_len + value_len..];
    value
}

#[cfg(unix)]
#[test]
fn material_observer_publishes_changed_symlink_generation_as_pair() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let cert_b = write_named_test_cert_files(&temp_dir, "cert_b");
    let resolver = test_resolver(&cert_a);
    let material_dir = temp_dir.path().join("material");
    let generation = publish_symlink_generation(&temp_dir, &material_dir, "generation_b", &cert_b);
    let mut last_successful_generation = None;

    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Published(generation.clone())
    );

    assert_eq!(last_successful_generation, Some(generation));
    assert_current_leaf_der(&resolver, cert_b.cert_der.as_ref());
}

#[cfg(unix)]
#[test]
fn material_observer_publishes_real_directory_current_generation_as_pair() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let cert_b = write_named_test_cert_files(&temp_dir, "cert_b");
    let resolver = test_resolver(&cert_a);
    let material_dir = temp_dir.path().join("material");
    std::fs::create_dir_all(&material_dir).unwrap();
    let generation = publish_real_dir_generation(&material_dir, "generation_b", &cert_b);
    let mut last_successful_generation = None;

    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Published(generation.clone())
    );

    assert_eq!(last_successful_generation, Some(generation));
    assert_current_leaf_der(&resolver, cert_b.cert_der.as_ref());
}

#[test]
fn material_observer_publishes_direct_material_directory_pair() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let cert_b = write_named_test_cert_files(&temp_dir, "cert_b");
    let resolver = test_resolver(&cert_a);
    let material_dir = temp_dir.path().join("material");
    std::fs::create_dir_all(&material_dir).unwrap();
    std::fs::copy(&cert_b.cert_path, material_dir.join(FULLCHAIN_FILE_NAME)).unwrap();
    std::fs::copy(&cert_b.key_path, material_dir.join(PRIVATE_KEY_FILE_NAME)).unwrap();
    let mut last_successful_generation = None;

    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Published(material_dir.clone())
    );

    assert_eq!(last_successful_generation, Some(material_dir));
    assert_current_leaf_der(&resolver, cert_b.cert_der.as_ref());
}

#[test]
fn material_observer_reloads_direct_material_directory_after_in_place_rotation() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let cert_b = write_named_test_cert_files(&temp_dir, "cert_b");
    let cert_c = write_named_test_cert_files(&temp_dir, "cert_c");
    let resolver = test_resolver(&cert_a);
    let material_dir = temp_dir.path().join("material");
    std::fs::create_dir_all(&material_dir).unwrap();
    std::fs::copy(&cert_b.cert_path, material_dir.join(FULLCHAIN_FILE_NAME)).unwrap();
    std::fs::copy(&cert_b.key_path, material_dir.join(PRIVATE_KEY_FILE_NAME)).unwrap();
    let mut last_successful_generation = None;

    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Published(material_dir.clone())
    );
    std::fs::copy(&cert_c.cert_path, material_dir.join(FULLCHAIN_FILE_NAME)).unwrap();
    std::fs::copy(&cert_c.key_path, material_dir.join(PRIVATE_KEY_FILE_NAME)).unwrap();

    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Published(material_dir.clone()),
        "changed certificate content at a stable direct-material path must reload"
    );
    assert_current_leaf_der(&resolver, cert_c.cert_der.as_ref());
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn authorized_startup_pair_observes_first_later_generation() {
    let temp_dir = TempDir::new().unwrap();
    let material_dir = temp_dir.path().join("material");
    let startup_cert = write_named_test_cert_files(&temp_dir, "startup_cert");
    let published_cert = write_named_test_cert_files(&temp_dir, "published_cert");
    let resolver = Arc::new(startup_resolver_under_material(
        &material_dir,
        &startup_cert,
    ));
    let (observation_tx, mut observation_rx) = mpsc::unbounded_channel();
    let observer = tokio::spawn(run_tls_material_observer(
        Arc::clone(&resolver),
        material_dir.clone(),
        || async { Some(29) },
        move |observation| observation_tx.send(observation.clone()).unwrap(),
    ));

    assert_eq!(resolve_material_generation(&material_dir), Ok(None));
    assert_eq!(
        observation_rx.recv().await,
        Some(TlsMaterialObservation::Absent),
        "an authorized source must observe while managed material is absent"
    );

    let generation = publish_real_dir_generation(&material_dir, "generation_b", &published_cert);
    tokio::time::advance(TLS_MATERIAL_OBSERVER_INTERVAL).await;
    assert_eq!(
        observation_rx.recv().await,
        Some(TlsMaterialObservation::Published(generation)),
        "the same observer must publish the first later managed generation"
    );
    assert_current_leaf_der(&resolver, published_cert.cert_der.as_ref());

    observer.abort();
    let _ = observer.await;
}

#[test]
fn unrelated_static_pair_is_rejected_while_managed_material_is_absent() {
    let temp_dir = TempDir::new().unwrap();
    let static_cert = write_named_test_cert_files(&temp_dir, "static_cert");
    let resolver = test_resolver(&static_cert);
    let material_dir = temp_dir.path().join("material");
    std::fs::create_dir_all(&material_dir).unwrap();

    let error = resolver
        .validate_material_observer_source(&material_dir)
        .expect_err("an unrelated static pair must not start an observer");

    assert!(
        error.contains("is outside managed material directory"),
        "divergence must be explicit to the startup caller: {error}"
    );
    assert_current_leaf_der(&resolver, static_cert.cert_der.as_ref());
}

#[cfg(unix)]
#[tokio::test]
async fn ssl_manager_and_observer_share_material_layout_contract() {
    let temp_dir = TempDir::new().unwrap();
    let acme_server_cert = write_named_test_cert_files(&temp_dir, "acme_server");
    let served_cert = write_named_test_cert_files(&temp_dir, "served");
    let material_dir = temp_dir.path().join("material");
    std::fs::create_dir_all(&material_dir).unwrap();
    let (acme_directory, shutdown_tx, acme_server) =
        start_test_acme_account_server(&acme_server_cert).await;
    let manager = flapjack_ssl::SslManager::new(flapjack_ssl::SslConfig {
        public_ip: None,
        acme_identifier: "localhost".to_string(),
        email: "test@example.com".to_string(),
        acme_directory,
        material_dir: material_dir.clone(),
        root_ca_pem: Some(acme_server_cert.cert_path.clone()),
        check_interval_secs: 3_600,
        renew_days_threshold: 0,
    })
    .await
    .expect("public SSL manager construction should reach the local ACME account server");
    let renewal_task = tokio::spawn(Arc::clone(&manager).start_renewal_loop());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let status = manager.get_status().await;
            assert_ne!(
                status.status, "failed",
                "the SSL manager must recognize the HTTP observer's material names: {:?}",
                status.error
            );
            if status.status == "ok" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the SSL manager's immediate issuance should complete");

    let resolver = test_resolver(&served_cert);
    let mut successful_generation = None;
    let generation = resolve_material_generation(&material_dir)
        .expect("the HTTP observer must resolve the manager's publication layout")
        .expect("the manager must atomically publish a generation");
    assert_ne!(generation, material_dir);
    let published_leaf = load_certificates(&generation.join(FULLCHAIN_FILE_NAME))
        .unwrap()
        .remove(0);
    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut successful_generation),
        TlsMaterialObservation::Published(generation)
    );
    assert_current_leaf_der(&resolver, published_leaf.as_ref());

    renewal_task.abort();
    let _ = renewal_task.await;
    shutdown_tx.send(()).unwrap();
    acme_server.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn timed_material_observer_reads_expiry_only_after_publication() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let cert_b = write_named_test_cert_files(&temp_dir, "cert_b");
    let material_dir = temp_dir.path().join("material");
    let resolver = Arc::new(startup_resolver_under_material(&material_dir, &cert_a));
    let expiry_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (observation_tx, mut observation_rx) = mpsc::unbounded_channel();
    let observer = tokio::spawn(run_tls_material_observer(
        Arc::clone(&resolver),
        material_dir.clone(),
        {
            let expiry_calls = Arc::clone(&expiry_calls);
            move || {
                let expiry_calls = Arc::clone(&expiry_calls);
                async move {
                    expiry_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Some(17)
                }
            }
        },
        move |observation| observation_tx.send(observation.clone()).unwrap(),
    ));

    assert_eq!(
        observation_rx.recv().await,
        Some(TlsMaterialObservation::Absent)
    );
    assert_eq!(
        expiry_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "absent startup material must not read certificate expiry"
    );

    let generation = publish_real_dir_generation(&material_dir, "generation_b", &cert_b);
    std::fs::write(
        generation.join(FULLCHAIN_FILE_NAME),
        b"-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    tokio::time::advance(TLS_MATERIAL_OBSERVER_INTERVAL).await;
    assert!(matches!(
        observation_rx.recv().await,
        Some(TlsMaterialObservation::Rejected { generation: rejected, .. }) if rejected == generation
    ));
    assert_eq!(
        expiry_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "rejected material must not read certificate expiry"
    );

    std::fs::copy(&cert_b.cert_path, generation.join(FULLCHAIN_FILE_NAME)).unwrap();
    tokio::time::advance(TLS_MATERIAL_OBSERVER_INTERVAL).await;
    assert_eq!(
        observation_rx.recv().await,
        Some(TlsMaterialObservation::Published(generation.clone()))
    );
    assert_eq!(
        expiry_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "published material must read certificate expiry exactly once"
    );

    tokio::time::advance(TLS_MATERIAL_OBSERVER_INTERVAL).await;
    assert_eq!(
        observation_rx.recv().await,
        Some(TlsMaterialObservation::Unchanged(generation))
    );
    assert_eq!(
        expiry_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "unchanged material must not read certificate expiry again"
    );
    assert_current_leaf_der(&resolver, cert_b.cert_der.as_ref());
    observer.abort();
    let _ = observer.await;
}

#[cfg(unix)]
#[test]
fn material_observer_handles_initial_absence_then_first_publication() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let cert_b = write_named_test_cert_files(&temp_dir, "cert_b");
    let resolver = test_resolver(&cert_a);
    let material_dir = temp_dir.path().join("material");
    let mut last_successful_generation = None;

    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Absent
    );
    assert_eq!(last_successful_generation, None);

    let generation = publish_symlink_generation(&temp_dir, &material_dir, "generation_b", &cert_b);
    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Published(generation.clone())
    );
    assert_eq!(last_successful_generation, Some(generation));
    assert_current_leaf_der(&resolver, cert_b.cert_der.as_ref());
}

#[cfg(unix)]
#[test]
fn material_observer_does_not_reload_unchanged_generation() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let cert_b = write_named_test_cert_files(&temp_dir, "cert_b");
    let resolver = test_resolver(&cert_a);
    let material_dir = temp_dir.path().join("material");
    let generation = publish_symlink_generation(&temp_dir, &material_dir, "generation_b", &cert_b);
    let mut last_successful_generation = None;
    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Published(generation.clone())
    );
    std::fs::write(
        generation.join(FULLCHAIN_FILE_NAME),
        b"-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
    )
    .unwrap();

    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Unchanged(generation.clone())
    );

    assert_eq!(last_successful_generation, Some(generation));
    assert_current_leaf_der(&resolver, cert_b.cert_der.as_ref());
}

#[cfg(unix)]
#[test]
fn observer_initial_marker_uses_already_served_generation() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let resolver = test_resolver(&cert_a);
    let material_dir = temp_dir.path().join("material");
    let generation = publish_symlink_generation(&temp_dir, &material_dir, "generation_a", &cert_a);

    assert_eq!(
        initial_successful_tls_material_generation(&material_dir, &resolver),
        Some(generation.clone())
    );

    let mut last_successful_generation =
        initial_successful_tls_material_generation(&material_dir, &resolver);
    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Unchanged(generation.clone()),
        "startup material already served by the resolver must not publish as a renewal"
    );
    assert_eq!(last_successful_generation, Some(generation));
    assert_current_leaf_der(&resolver, cert_a.cert_der.as_ref());
}

#[cfg(unix)]
#[test]
fn material_observer_retries_rejected_generation_without_advancing_marker() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let cert_b = write_named_test_cert_files(&temp_dir, "cert_b");
    let resolver = test_resolver(&cert_a);
    let material_dir = temp_dir.path().join("material");
    let generation = publish_symlink_generation(&temp_dir, &material_dir, "generation_b", &cert_b);
    std::fs::write(
        generation.join(FULLCHAIN_FILE_NAME),
        b"-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let mut last_successful_generation = None;

    let malformed =
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation);
    assert!(
        matches!(&malformed, TlsMaterialObservation::Rejected { error, .. } if error.contains("invalid PEM data")),
        "malformed generation must be rejected by the shared PEM loader: {malformed:?}"
    );
    assert_eq!(last_successful_generation, None);
    assert_current_leaf_der(&resolver, cert_a.cert_der.as_ref());

    std::fs::copy(&cert_b.cert_path, generation.join(FULLCHAIN_FILE_NAME)).unwrap();
    std::fs::copy(&cert_a.key_path, generation.join(PRIVATE_KEY_FILE_NAME)).unwrap();

    let rejected =
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation);
    assert!(
        matches!(&rejected, TlsMaterialObservation::Rejected { error, .. } if error.contains("KeyMismatch")),
        "mismatched generation must be rejected with the loader's key mismatch: {rejected:?}"
    );
    assert_eq!(last_successful_generation, None);
    assert_current_leaf_der(&resolver, cert_a.cert_der.as_ref());

    std::fs::copy(&cert_b.key_path, generation.join(PRIVATE_KEY_FILE_NAME)).unwrap();
    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Published(generation.clone())
    );
    assert_eq!(last_successful_generation, Some(generation));
    assert_current_leaf_der(&resolver, cert_b.cert_der.as_ref());
}

async fn plaintext_http_request(bind_addr: SocketAddr, method: &str, path: &str) -> String {
    let mut tcp = tokio::net::TcpStream::connect(bind_addr)
        .await
        .expect("plaintext test server should accept TCP connections");
    let request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    tcp.write_all(request.as_bytes())
        .await
        .expect("plaintext request should write");
    let mut response = String::new();
    tcp.read_to_string(&mut response)
        .await
        .expect("plaintext response should read");
    response
}

#[tokio::test]
async fn plaintext_challenge_get_reaches_handler_without_exposing_other_paths() {
    let temp_dir = TempDir::new().unwrap();
    let cert_files = write_test_cert_files(&temp_dir);
    let config = load_tls_config(&tls_paths(&cert_files.cert_path, &cert_files.key_path))
        .expect("TLS config should load")
        .config;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind_addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let app = Router::new()
        .route(
            "/.well-known/acme-challenge/:token",
            get(|axum::extract::Path(token): axum::extract::Path<String>| async move { token }),
        )
        .route("/ready", get(|| async { "ready" }));
    let server = tokio::spawn(serve_tls(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
        config,
        async {
            let _ = shutdown_rx.await;
        },
    ));

    let challenge =
        plaintext_http_request(bind_addr, "GET", "/.well-known/acme-challenge/token-a").await;
    assert!(challenge.starts_with("HTTP/1.1 200 OK"), "{challenge}");
    assert_eq!(response_body(&challenge), "token-a");

    let rejected = plaintext_http_request(bind_addr, "GET", "/ready").await;
    assert!(rejected.starts_with("HTTP/1.1 404 Not Found"), "{rejected}");
    assert_eq!(response_body(&rejected), "Not Found");
    let rejected_method =
        plaintext_http_request(bind_addr, "POST", "/.well-known/acme-challenge/token-a").await;
    assert!(
        rejected_method.starts_with("HTTP/1.1 404 Not Found"),
        "{rejected_method}"
    );

    let (_client_addr, tls_response) =
        tls_http_request(bind_addr, cert_files.cert_der, "/ready").await;
    assert_eq!(response_body(&tls_response), "ready");
    shutdown_tx.send(()).expect("shutdown signal should send");
    server
        .await
        .expect("TLS server task should join")
        .expect("TLS server should shut down cleanly");
}
