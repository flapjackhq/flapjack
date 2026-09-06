use std::io::Write as _;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use flate2::write::GzEncoder;
use flate2::Compression;
use rcgen::{generate_simple_self_signed, CertifiedKey as RcgenCertifiedKey};
use serial_test::serial;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

use super::*;
use crate::index::manager::publication::CrawlerTerminalOutcome;
use crate::security::test_helpers::install_test_outbound_host_resolver;
use crate::security::{
    CRAWLER_FIXTURE_CA_PATH_ENV, CRAWLER_FIXTURE_ENDPOINT_ENV, CRAWLER_FIXTURE_HOST_ENV,
    CRAWLER_FIXTURE_PUBLIC_IP_ENV,
};

#[derive(Clone, Default)]
struct FixtureFetcher {
    pages: Arc<Mutex<HashMap<String, CrawlerFetchResponse>>>,
    calls: Arc<Mutex<Vec<String>>>,
    after_fetch: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

impl FixtureFetcher {
    fn with_pages(pages: impl IntoIterator<Item = (&'static str, CrawlerFetchResponse)>) -> Self {
        Self {
            pages: Arc::new(Mutex::new(
                pages
                    .into_iter()
                    .map(|(url, response)| (url.to_owned(), response))
                    .collect(),
            )),
            calls: Arc::default(),
            after_fetch: None,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl CrawlerFetcher for FixtureFetcher {
    fn fetch<'a>(
        &'a self,
        target: &'a VettedCrawlerUrlTarget,
        _max_decoded_body_bytes: u64,
        _guard: &'a CrawlerExecutionGuard,
    ) -> CrawlerBoxFuture<'a, Result<CrawlerFetchResponse, CrawlerRuntimeErrorCode>> {
        Box::pin(async move {
            let url = target.canonical_url.as_str().to_owned();
            self.calls.lock().unwrap().push(url.clone());
            let response = self
                .pages
                .lock()
                .unwrap()
                .get(&url)
                .cloned()
                .ok_or(CrawlerRuntimeErrorCode::TargetRejected)?;
            if let Some(after_fetch) = &self.after_fetch {
                after_fetch(&url);
            }
            Ok(response)
        })
    }
}

#[derive(Default)]
struct RecordingHandoff {
    batches: Mutex<Vec<CrawlerPublicationBatch>>,
}

impl CrawlerPublicationHandoff for RecordingHandoff {
    type Receipt = usize;

    fn handoff<'a>(
        &'a self,
        batch: CrawlerPublicationBatch,
        _counters: CrawlerRuntimeCounters,
        guard: &'a CrawlerExecutionGuard,
    ) -> CrawlerBoxFuture<'a, Result<Self::Receipt, ()>> {
        Box::pin(async move {
            // This mirrors Goal 1's required in-boundary recheck.
            guard.check().map_err(|_| ())?;
            let count = batch.records.len();
            self.batches.lock().unwrap().push(batch);
            Ok(count)
        })
    }
}

fn html(body: &str) -> CrawlerFetchResponse {
    CrawlerFetchResponse {
        status: 200,
        content_type: Some("text/html; charset=utf-8".to_owned()),
        decoded_body: body.as_bytes().to_vec(),
    }
}

fn limits(max_pages: u32) -> CrawlerRuntimeLimits {
    CrawlerRuntimeLimits {
        max_depth: 3,
        max_pages,
        max_decoded_body_bytes: 64 * 1024,
        max_record_bytes: 32 * 1024,
        max_records: max_pages,
        max_concurrency: 2,
    }
}

fn transform() -> CrawlerTransformSpec {
    CrawlerTransformSpec {
        fields: vec![
            CrawlerSelectedField {
                source: CrawlerCanonicalField::Url,
                output: "url".to_owned(),
            },
            CrawlerSelectedField {
                source: CrawlerCanonicalField::Title,
                output: "name".to_owned(),
            },
            CrawlerSelectedField {
                source: CrawlerCanonicalField::Metadata,
                output: "metadata".to_owned(),
            },
            CrawlerSelectedField {
                source: CrawlerCanonicalField::Text,
                output: "text".to_owned(),
            },
        ],
        object_id_source: CrawlerCanonicalField::Url,
    }
}

fn request(max_pages: u32) -> CrawlerRuntimeRequest {
    CrawlerRuntimeRequest {
        start_url: "https://example.com/".to_owned(),
        limits: limits(max_pages),
        transform: transform(),
        max_run_duration: Duration::from_secs(60),
    }
}

#[test]
fn crawler_runtime_replay_budget_is_bounded_by_durable_start_time() {
    let mut replay = request(1);
    replay.max_run_duration = Duration::from_millis(1_000);
    replay.apply_elapsed_budget(10_000, 10_750);
    assert_eq!(replay.max_run_duration, Duration::from_millis(250));
    replay.apply_elapsed_budget(10_000, 11_000);
    assert_eq!(replay.max_run_duration, Duration::ZERO);
}

#[test]
fn crawler_runtime_guard_observes_durable_cancellation_without_registry() {
    let canceled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let durable = Arc::clone(&canceled);
    let cancellation = CrawlerCancellation::with_durable_probe(move || {
        durable.load(std::sync::atomic::Ordering::Acquire)
    });
    let guard = CrawlerExecutionGuard::new(cancellation, Duration::from_secs(1));
    assert_eq!(guard.check(), Ok(()));
    canceled.store(true, std::sync::atomic::Ordering::Release);
    assert_eq!(guard.check(), Err(CrawlerGuardFailure::Canceled));
}

fn public_resolver(counter: Arc<AtomicUsize>) -> impl Drop {
    install_test_outbound_host_resolver(Arc::new(move |_host, port| {
        assert_eq!(port, Some(443));
        counter.fetch_add(1, Ordering::SeqCst);
        Some(vec!["1.1.1.1".parse().unwrap()])
    }))
}

struct HermeticTlsIdentity {
    acceptor: TlsAcceptor,
    client_root: reqwest::Certificate,
    client_root_pem: String,
}

fn hermetic_tls_identity() -> HermeticTlsIdentity {
    hermetic_tls_identity_for("crawler.invalid")
}

fn hermetic_tls_identity_for(host: &str) -> HermeticTlsIdentity {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let RcgenCertifiedKey { cert, key_pair } = generate_simple_self_signed(vec![host.to_owned()])
        .expect("hermetic crawler certificate must generate");
    let cert_der: CertificateDer<'static> = cert.der().clone();
    let client_root_pem = cert.pem();
    let key_der = PrivatePkcs8KeyDer::from(key_pair.serialize_der());
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], PrivateKeyDer::Pkcs8(key_der))
        .expect("hermetic crawler TLS identity must be valid");
    HermeticTlsIdentity {
        acceptor: TlsAcceptor::from(Arc::new(server_config)),
        client_root: reqwest::Certificate::from_der(cert_der.as_ref())
            .expect("hermetic crawler root certificate must parse"),
        client_root_pem,
    }
}

struct CrawlerFixtureTransportEnvironment {
    prior: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl CrawlerFixtureTransportEnvironment {
    fn set(host: &str, public_ip: &str, endpoint: SocketAddr, ca_path: &std::path::Path) -> Self {
        Self::set_raw(host, public_ip, &endpoint.to_string(), ca_path)
    }

    fn set_raw(host: &str, public_ip: &str, endpoint: &str, ca_path: &std::path::Path) -> Self {
        let values = [
            (CRAWLER_FIXTURE_HOST_ENV, host.to_owned()),
            (CRAWLER_FIXTURE_PUBLIC_IP_ENV, public_ip.to_owned()),
            (CRAWLER_FIXTURE_ENDPOINT_ENV, endpoint.to_owned()),
            (CRAWLER_FIXTURE_CA_PATH_ENV, ca_path.display().to_string()),
        ];
        let mut prior = Vec::new();
        for (key, value) in values {
            prior.push((key, std::env::var_os(key)));
            std::env::set_var(key, value);
        }
        Self { prior }
    }

    fn clear() -> Self {
        let mut prior = Vec::new();
        for key in [
            CRAWLER_FIXTURE_HOST_ENV,
            CRAWLER_FIXTURE_PUBLIC_IP_ENV,
            CRAWLER_FIXTURE_ENDPOINT_ENV,
            CRAWLER_FIXTURE_CA_PATH_ENV,
        ] {
            prior.push((key, std::env::var_os(key)));
            std::env::remove_var(key);
        }
        Self { prior }
    }
}

impl Drop for CrawlerFixtureTransportEnvironment {
    fn drop(&mut self) {
        for (key, value) in &self.prior {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn pinned_test_target(addr: SocketAddr, path: &str) -> VettedCrawlerUrlTarget {
    VettedCrawlerUrlTarget {
        canonical_url: reqwest::Url::parse(&format!(
            "https://crawler.invalid:{}{path}",
            addr.port()
        ))
        .unwrap(),
        host: "crawler.invalid".to_owned(),
        port: addr.port(),
        resolved_ips: vec![addr.ip()],
    }
}

fn hermetic_fetcher(identity: &HermeticTlsIdentity) -> ReqwestCrawlerFetcher {
    ReqwestCrawlerFetcher::for_hermetic_test(
        identity.client_root.clone(),
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
}

async fn read_http_request<S>(stream: &mut S) -> String
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "client closed before sending request headers");
        request.extend_from_slice(&chunk[..read]);
        assert!(
            request.len() <= 16 * 1024,
            "request headers must stay bounded"
        );
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8(request).unwrap();
        }
    }
}

fn request_path(request: &str) -> &str {
    request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("hermetic request must contain a path")
}

fn fixed_response(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    )
    .into_bytes();
    for (name, value) in headers {
        response.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(body);
    response
}

async fn bind_hermetic_listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

fn spawn_fixed_tls_server(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    paths: Arc<Mutex<Vec<String>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            // Negative CA cases intentionally abort the handshake. Keep the
            // fixture alive so each later assertion gets an independent
            // transport attempt instead of inheriting a dead server task.
            let Ok(mut tls) = acceptor.accept(tcp).await else {
                continue;
            };
            let request = read_http_request(&mut tls).await;
            let path = request_path(&request).to_owned();
            paths.lock().unwrap().push(path.clone());
            let response = match path.as_str() {
                "/ok" => fixed_response(
                    "200 OK",
                    &[("Content-Type", "text/html".to_owned())],
                    b"<html><body>ok</body></html>",
                ),
                "/redirect" => fixed_response(
                    "302 Found",
                    &[
                        ("Content-Type", "text/html".to_owned()),
                        ("Location", "/ok".to_owned()),
                    ],
                    b"redirect",
                ),
                "/gzip" => {
                    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                    encoder.write_all(&vec![b'x'; 4 * 1024]).unwrap();
                    let compressed = encoder.finish().unwrap();
                    fixed_response(
                        "200 OK",
                        &[
                            ("Content-Type", "text/html".to_owned()),
                            ("Content-Encoding", "gzip".to_owned()),
                        ],
                        &compressed,
                    )
                }
                other => panic!("unexpected hermetic crawler path {other}"),
            };
            tls.write_all(&response).await.unwrap();
            tls.shutdown().await.unwrap();
        }
    })
}

#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn crawler_fault_fixture_transport_is_exact_explicit_and_default_closed() {
    const FIXTURE_HOST: &str = "crawler-fixture.example.com";
    let identity = hermetic_tls_identity_for(FIXTURE_HOST);
    let temp = TempDir::new().unwrap();
    let ca_path = temp.path().join("crawler-fixture-ca.pem");
    std::fs::write(&ca_path, &identity.client_root_pem).unwrap();
    let malformed_ca_path = temp.path().join("malformed-crawler-fixture-ca.pem");
    std::fs::write(&malformed_ca_path, "not a PEM certificate").unwrap();
    let unrelated_identity = hermetic_tls_identity_for(FIXTURE_HOST);
    let unrelated_ca_path = temp.path().join("unrelated-crawler-fixture-ca.pem");
    std::fs::write(&unrelated_ca_path, &unrelated_identity.client_root_pem).unwrap();
    let (listener, endpoint) = bind_hermetic_listener().await;
    let paths = Arc::new(Mutex::new(Vec::new()));
    let server = spawn_fixed_tls_server(listener, identity.acceptor, Arc::clone(&paths));
    let (proxy_listener, proxy_endpoint) = bind_hermetic_listener().await;
    let proxy_accepts = Arc::new(AtomicUsize::new(0));
    let proxy = spawn_proxy_trap(proxy_listener, Arc::clone(&proxy_accepts));

    {
        let _closed = CrawlerFixtureTransportEnvironment::clear();
        let _private_dns = install_test_outbound_host_resolver(Arc::new(|_, _| {
            Some(vec!["127.0.0.1".parse().unwrap()])
        }));
        assert_eq!(
            vet_crawler_url_target(&format!("https://{FIXTURE_HOST}/ok")),
            Err(CrawlerTargetAdmissionError::DnsResolutionFailed),
            "default configuration must retain strict private-IP rejection"
        );
        assert!(paths.lock().unwrap().is_empty());
    }

    {
        let _partial = CrawlerFixtureTransportEnvironment::clear();
        std::env::set_var(CRAWLER_FIXTURE_HOST_ENV, FIXTURE_HOST);
        assert_eq!(
            vet_crawler_url_target(&format!("https://{FIXTURE_HOST}/ok")),
            Err(CrawlerTargetAdmissionError::TargetRejected),
            "a partially configured fixture seam must fail closed"
        );
        assert!(paths.lock().unwrap().is_empty());
    }

    {
        let _malformed_endpoint = CrawlerFixtureTransportEnvironment::set_raw(
            FIXTURE_HOST,
            "1.1.1.1",
            "not-a-socket-address",
            &ca_path,
        );
        assert_eq!(
            vet_crawler_url_target(&format!("https://{FIXTURE_HOST}/ok")),
            Err(CrawlerTargetAdmissionError::TargetRejected),
            "a malformed fixture endpoint must fail closed during admission"
        );
        assert!(paths.lock().unwrap().is_empty());
    }

    {
        let _malformed_ca = CrawlerFixtureTransportEnvironment::set(
            FIXTURE_HOST,
            "1.1.1.1",
            endpoint,
            &malformed_ca_path,
        );
        let target = vet_crawler_url_target(&format!("https://{FIXTURE_HOST}/ok")).unwrap();
        let guard =
            CrawlerExecutionGuard::new(CrawlerCancellation::default(), Duration::from_secs(5));
        assert_eq!(
            ReqwestCrawlerFetcher::default()
                .fetch(&target, 1024, &guard)
                .await,
            Err(CrawlerRuntimeErrorCode::FetchTimeout),
            "malformed fixture CA material must fail closed during TLS"
        );
        assert!(paths.lock().unwrap().is_empty());
    }

    let _fixture =
        CrawlerFixtureTransportEnvironment::set(FIXTURE_HOST, "1.1.1.1", endpoint, &ca_path);
    let _private_dns = install_test_outbound_host_resolver(Arc::new(|_, _| {
        Some(vec!["127.0.0.1".parse().unwrap()])
    }));
    let _proxy_environment = ProxyEnvironmentGuard::point_at(proxy_endpoint);
    assert_eq!(
        vet_crawler_url_target("https://other-fixture.example.com/ok"),
        Err(CrawlerTargetAdmissionError::DnsResolutionFailed),
        "the transport seam must not widen any other hostname"
    );
    let target = vet_crawler_url_target(&format!("https://{FIXTURE_HOST}/ok")).unwrap();
    assert_eq!(
        target.resolved_ips,
        vec!["1.1.1.1".parse::<std::net::IpAddr>().unwrap()]
    );
    let guard = CrawlerExecutionGuard::new(CrawlerCancellation::default(), Duration::from_secs(5));
    let response = ReqwestCrawlerFetcher::default()
        .fetch(&target, 1024, &guard)
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(proxy_accepts.load(Ordering::SeqCst), 0);

    let redirect_target =
        vet_crawler_url_target(&format!("https://{FIXTURE_HOST}/redirect")).unwrap();
    assert_eq!(
        ReqwestCrawlerFetcher::default()
            .fetch(&redirect_target, 1024, &guard)
            .await,
        Err(CrawlerRuntimeErrorCode::RedirectRejected),
        "the fixture transport must preserve the production redirect refusal"
    );
    assert_eq!(paths.lock().unwrap().as_slice(), ["/ok", "/redirect"]);
    assert_eq!(proxy_accepts.load(Ordering::SeqCst), 0);

    drop(_fixture);
    let _wrong_fixture_root = CrawlerFixtureTransportEnvironment::set(
        FIXTURE_HOST,
        "1.1.1.1",
        endpoint,
        &unrelated_ca_path,
    );
    let target = vet_crawler_url_target(&format!("https://{FIXTURE_HOST}/ok")).unwrap();
    let fetcher_with_ambient_correct_root = ReqwestCrawlerFetcher::for_hermetic_test(
        identity.client_root,
        Duration::from_secs(5),
        Duration::from_secs(5),
        Duration::from_secs(5),
    );
    assert_eq!(
        fetcher_with_ambient_correct_root
            .fetch(&target, 1024, &guard)
            .await,
        Err(CrawlerRuntimeErrorCode::FetchTimeout),
        "fixture transport must trust only its explicit fixture CA"
    );
    assert_eq!(paths.lock().unwrap().as_slice(), ["/ok", "/redirect"]);
    assert_eq!(proxy_accepts.load(Ordering::SeqCst), 0);
    server.abort();
    proxy.abort();
}

struct ProxyEnvironmentGuard {
    prior: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl ProxyEnvironmentGuard {
    fn point_at(addr: SocketAddr) -> Self {
        let proxy = format!("http://{addr}");
        let mut prior = Vec::new();
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
        ] {
            prior.push((key, std::env::var_os(key)));
            if matches!(key, "NO_PROXY" | "no_proxy") {
                std::env::remove_var(key);
            } else {
                std::env::set_var(key, &proxy);
            }
        }
        Self { prior }
    }
}

impl Drop for ProxyEnvironmentGuard {
    fn drop(&mut self) {
        for (key, value) in &self.prior {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn spawn_proxy_trap(
    listener: TcpListener,
    accepted: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            accepted.fetch_add(1, Ordering::SeqCst);
            let _ = stream
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    })
}

#[test]
fn crawler_runtime_closed_transform_known_answer_is_shared_by_preview_and_execution() {
    let compiled = CompiledCrawlerTransform::compile(transform()).unwrap();
    let record = CanonicalCrawlerRecord {
        url: "https://example.com/catalog?q=1".to_owned(),
        title: "Example".to_owned(),
        metadata: BTreeMap::from([("description".to_owned(), "Known answer".to_owned())]),
        text: "One two three".to_owned(),
    };
    let expected = serde_json::json!({
        "objectID": "f35cce177f2820b38a9bc7dffbb0bee06d3be6ceecaa0dcf72bf3b7ee63381fc",
        "url": "https://example.com/catalog?q=1",
        "name": "Example",
        "metadata": {"description": "Known answer"},
        "text": "One two three"
    });
    assert_eq!(compiled.preview(&record).unwrap(), expected);
    assert_eq!(compiled.apply(&record).unwrap(), expected);
}

#[test]
fn crawler_runtime_transform_rejects_open_or_ambiguous_shapes() {
    for fields in [
        vec![CrawlerSelectedField {
            source: CrawlerCanonicalField::Url,
            output: "objectID".to_owned(),
        }],
        vec![
            CrawlerSelectedField {
                source: CrawlerCanonicalField::Url,
                output: "one".to_owned(),
            },
            CrawlerSelectedField {
                source: CrawlerCanonicalField::Url,
                output: "two".to_owned(),
            },
        ],
        vec![CrawlerSelectedField {
            source: CrawlerCanonicalField::Text,
            output: "has whitespace".to_owned(),
        }],
    ] {
        assert_eq!(
            CompiledCrawlerTransform::compile(CrawlerTransformSpec {
                fields,
                object_id_source: CrawlerCanonicalField::Url,
            })
            .unwrap_err(),
            CrawlerRuntimeErrorCode::TransformInvalid
        );
    }
}

#[test]
fn crawler_runtime_transform_field_and_output_boundaries_are_exact() {
    let selected = |source, output: String| CrawlerSelectedField { source, output };
    let spec = |fields| CrawlerTransformSpec {
        fields,
        object_id_source: CrawlerCanonicalField::Url,
    };
    // Five selected fields necessarily duplicate one of the four canonical
    // sources, so pin the independent count rule directly as well as proving
    // the compiled and HTTP-facing rejection below.
    assert!(!valid_transform_field_count(0));
    assert!(valid_transform_field_count(1));
    assert!(valid_transform_field_count(4));
    assert!(!valid_transform_field_count(5));
    let one = vec![selected(CrawlerCanonicalField::Url, "a".repeat(64))];
    let four = vec![
        selected(CrawlerCanonicalField::Url, "url".to_owned()),
        selected(CrawlerCanonicalField::Title, "title_1".to_owned()),
        selected(CrawlerCanonicalField::Metadata, "meta.path".to_owned()),
        selected(CrawlerCanonicalField::Text, "text-value".to_owned()),
    ];
    assert!(CompiledCrawlerTransform::compile(spec(one)).is_ok());
    assert!(CompiledCrawlerTransform::compile(spec(four.clone())).is_ok());

    let mut five = four;
    five.push(selected(CrawlerCanonicalField::Url, "fifth".to_owned()));
    for fields in [
        Vec::new(),
        five,
        vec![selected(CrawlerCanonicalField::Url, "a".repeat(65))],
        vec![selected(CrawlerCanonicalField::Url, "9leading".to_owned())],
        vec![selected(CrawlerCanonicalField::Url, "bad/slash".to_owned())],
        vec![selected(CrawlerCanonicalField::Url, "objectID".to_owned())],
    ] {
        assert_eq!(
            CompiledCrawlerTransform::compile(spec(fields)).unwrap_err(),
            CrawlerRuntimeErrorCode::TransformInvalid
        );
    }
}

#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn crawler_runtime_crawls_same_origin_html_and_hands_off_one_bounded_batch() {
    let dns_calls = Arc::new(AtomicUsize::new(0));
    let _resolver = public_resolver(Arc::clone(&dns_calls));
    let fetcher = FixtureFetcher::with_pages([
        (
            "https://example.com/",
            html(
                r#"<html><head><title> Root   page </title>
                <meta name="description" content="Catalog root"></head><body>
                Visible <script>secret()</script><style>.hidden{}</style> text
                <a href="/a#one">A1</a><a href="/a#two">A2</a>
                <a href="/b?q=kept#gone">B</a>
                <a href="https://other.example/no">cross</a>
                <a href="http://example.com/no">http</a></body></html>"#,
            ),
        ),
        (
            "https://example.com/a",
            html("<html><head><title>A</title></head><body>Alpha</body></html>"),
        ),
        (
            "https://example.com/b?q=kept",
            html("<html><head><title>B</title></head><body>Beta</body></html>"),
        ),
    ]);
    let fetch_probe = fetcher.clone();
    let handoff = RecordingHandoff::default();

    let outcome = CrawlerRuntime::new(fetcher)
        .execute(request(4), CrawlerCancellation::default(), &handoff)
        .await;

    let (receipt, counters) = match outcome {
        CrawlerRuntimeOutcome::Handoff {
            receipt, counters, ..
        } => (receipt, counters),
        other => panic!("expected successful handoff, got {other:?}"),
    };
    assert_eq!(receipt, 3);
    assert_eq!(
        counters,
        CrawlerRuntimeCounters {
            fetched: 3,
            discovered: 2,
            transformed: 3,
        }
    );
    assert_eq!(dns_calls.load(Ordering::SeqCst), 3);
    let mut calls = fetch_probe.calls();
    calls.sort();
    assert_eq!(
        calls,
        [
            "https://example.com/",
            "https://example.com/a",
            "https://example.com/b?q=kept",
        ]
    );
    let batches = handoff.batches.lock().unwrap();
    let root = batches[0]
        .records
        .iter()
        .find(|record| record["url"] == "https://example.com/")
        .unwrap();
    assert_eq!(root["name"], "Root page");
    assert_eq!(root["metadata"]["description"], "Catalog root");
    assert_eq!(root["text"], "Visible text A1 A2 B cross http");
    assert!(!root["text"].as_str().unwrap().contains("secret"));
}

#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn crawler_runtime_same_host_duplicate_probe_is_linear_by_unique_fetch() {
    let dns_calls = Arc::new(AtomicUsize::new(0));
    let _resolver = public_resolver(Arc::clone(&dns_calls));
    let duplicate_links = (0..1_000)
        .map(|ordinal| format!(r#"<a href="/a#{ordinal}">same</a>"#))
        .collect::<String>();
    let fetcher = FixtureFetcher::with_pages([
        ("https://example.com/", html(&duplicate_links)),
        ("https://example.com/a", html("<p>only child</p>")),
    ]);
    let fetch_probe = fetcher.clone();
    let handoff = RecordingHandoff::default();

    let outcome = CrawlerRuntime::new(fetcher)
        .execute(request(2), CrawlerCancellation::default(), &handoff)
        .await;

    assert!(matches!(outcome, CrawlerRuntimeOutcome::Handoff { .. }));
    assert_eq!(fetch_probe.calls().len(), 2);
    assert_eq!(dns_calls.load(Ordering::SeqCst), 2);
    drop(_resolver);

    // A canceled libc lookup cannot be forcefully stopped, so admission owns
    // a fixed global permit that remains with every detached blocking worker.
    // Drive four times the cap into a resolver that cannot finish and prove
    // only the cap reaches blocking work while every queued caller cancels.
    let entered = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let entered_changed = Arc::new(tokio::sync::Semaphore::new(0));
    let drained = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let blocking_resolver = {
        let entered = Arc::clone(&entered);
        let active = Arc::clone(&active);
        let max_active = Arc::clone(&max_active);
        let entered_changed = Arc::clone(&entered_changed);
        let drained = Arc::clone(&drained);
        let release = Arc::clone(&release);
        install_test_outbound_host_resolver(Arc::new(move |_host, _port| {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(current, Ordering::SeqCst);
            entered.fetch_add(1, Ordering::SeqCst);
            entered_changed.add_permits(1);
            let (released, changed) = &*release;
            let mut released = released.lock().unwrap();
            while !*released {
                released = changed.wait(released).unwrap();
            }
            active.fetch_sub(1, Ordering::SeqCst);
            drained.add_permits(1);
            Some(vec!["1.1.1.1".parse().unwrap()])
        }))
    };
    let cancellation = CrawlerCancellation::default();
    let admissions = (0..MAX_CRAWLER_DNS_ADMISSIONS * 4)
        .map(|ordinal| {
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                let guard = CrawlerExecutionGuard::new(cancellation, Duration::from_secs(60));
                admit_target(format!("https://host-{ordinal}.invalid/"), &guard).await
            })
        })
        .collect::<Vec<_>>();
    entered_changed
        .acquire_many(MAX_CRAWLER_DNS_ADMISSIONS as u32)
        .await
        .unwrap()
        .forget();
    assert_eq!(active.load(Ordering::SeqCst), MAX_CRAWLER_DNS_ADMISSIONS);
    assert_eq!(
        max_active.load(Ordering::SeqCst),
        MAX_CRAWLER_DNS_ADMISSIONS
    );
    cancellation.cancel();
    for admission in admissions {
        assert_eq!(
            admission.await.unwrap(),
            Err(CrawlerRuntimeErrorCode::InternalFailure)
        );
    }
    {
        let (released, changed) = &*release;
        *released.lock().unwrap() = true;
        changed.notify_all();
    }
    drained
        .acquire_many(MAX_CRAWLER_DNS_ADMISSIONS as u32)
        .await
        .unwrap()
        .forget();
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(entered.load(Ordering::SeqCst), MAX_CRAWLER_DNS_ADMISSIONS);
    drop(blocking_resolver);
}

#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn crawler_runtime_response_policy_closes_redirect_content_and_body_failures() {
    let cases = [
        (
            CrawlerFetchResponse {
                status: 302,
                content_type: Some("text/html".to_owned()),
                decoded_body: Vec::new(),
            },
            CrawlerRuntimeErrorCode::RedirectRejected,
        ),
        (
            CrawlerFetchResponse {
                status: 200,
                content_type: Some("application/json".to_owned()),
                decoded_body: b"{}".to_vec(),
            },
            CrawlerRuntimeErrorCode::ContentTypeRejected,
        ),
        (
            CrawlerFetchResponse {
                status: 200,
                content_type: Some("text/html".to_owned()),
                decoded_body: vec![0; 9],
            },
            CrawlerRuntimeErrorCode::BodyLimitExceeded,
        ),
    ];
    for (response, expected) in cases {
        assert_eq!(validate_fetch_response(response, 8).unwrap_err(), expected);
    }

    let identity = hermetic_tls_identity();
    let (listener, addr) = bind_hermetic_listener().await;
    let paths = Arc::new(Mutex::new(Vec::new()));
    let server = spawn_fixed_tls_server(listener, identity.acceptor.clone(), Arc::clone(&paths));
    let (proxy_listener, proxy_addr) = bind_hermetic_listener().await;
    let proxy_accepts = Arc::new(AtomicUsize::new(0));
    let proxy = spawn_proxy_trap(proxy_listener, Arc::clone(&proxy_accepts));
    let _proxy_environment = ProxyEnvironmentGuard::point_at(proxy_addr);
    let fetcher = hermetic_fetcher(&identity);
    let guard = CrawlerExecutionGuard::new(CrawlerCancellation::default(), Duration::from_secs(30));

    // The deliberately non-resolving hostname succeeds only because the
    // already-admitted loopback address is pinned. The proxy trap must remain
    // untouched even though every standard proxy variable points at it.
    let ok = fetcher
        .fetch(&pinned_test_target(addr, "/ok"), 1024, &guard)
        .await
        .unwrap();
    assert_eq!(ok.status, 200);
    assert_eq!(proxy_accepts.load(Ordering::SeqCst), 0);

    assert_eq!(
        fetcher
            .fetch(&pinned_test_target(addr, "/redirect"), 1024, &guard)
            .await,
        Err(CrawlerRuntimeErrorCode::RedirectRejected)
    );

    // The wire body is tiny, but automatic gzip decoding expands it beyond
    // the decoded-byte cap. Disabling decompression would make this pass.
    assert_eq!(
        fetcher
            .fetch(&pinned_test_target(addr, "/gzip"), 1024, &guard)
            .await,
        Err(CrawlerRuntimeErrorCode::BodyLimitExceeded)
    );
    assert_eq!(
        paths.lock().unwrap().as_slice(),
        ["/ok", "/redirect", "/gzip"]
    );
    assert_eq!(proxy_accepts.load(Ordering::SeqCst), 0);
    server.abort();
    proxy.abort();
}

#[test]
fn crawler_runtime_decoded_stream_cap_rejects_before_over_limit_buffering() {
    let mut decoded = Vec::new();
    append_decoded_chunk(&mut decoded, b"1234", 8).unwrap();
    assert_eq!(
        append_decoded_chunk(&mut decoded, b"56789", 8),
        Err(CrawlerRuntimeErrorCode::BodyLimitExceeded)
    );
    assert_eq!(decoded, b"1234");
}

#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn crawler_runtime_cancel_after_fetch_prevents_transform_and_handoff() {
    let _resolver = public_resolver(Arc::new(AtomicUsize::new(0)));
    let cancellation = CrawlerCancellation::default();
    let cancel_during_fetch = cancellation.clone();
    let mut fetcher = FixtureFetcher::with_pages([(
        "https://example.com/",
        html("<title>must not publish</title>"),
    )]);
    fetcher.after_fetch = Some(Arc::new(move |_| cancel_during_fetch.cancel()));
    let handoff = RecordingHandoff::default();

    let outcome = CrawlerRuntime::new(fetcher)
        .execute(request(1), cancellation, &handoff)
        .await;

    assert!(matches!(outcome, CrawlerRuntimeOutcome::Canceled { .. }));
    assert!(handoff.batches.lock().unwrap().is_empty());
}

struct CancelInsideHandoff {
    cancellation: CrawlerCancellation,
    attempted: AtomicUsize,
}

impl CrawlerPublicationHandoff for CancelInsideHandoff {
    type Receipt = ();

    fn handoff<'a>(
        &'a self,
        _batch: CrawlerPublicationBatch,
        _counters: CrawlerRuntimeCounters,
        guard: &'a CrawlerExecutionGuard,
    ) -> CrawlerBoxFuture<'a, Result<Self::Receipt, ()>> {
        Box::pin(async move {
            self.attempted.fetch_add(1, Ordering::SeqCst);
            self.cancellation.cancel();
            guard.check().map_err(|_| ())?;
            Ok(())
        })
    }
}

#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn crawler_runtime_goal1_handoff_recheck_closes_last_cancel_race() {
    let _resolver = public_resolver(Arc::new(AtomicUsize::new(0)));
    let cancellation = CrawlerCancellation::default();
    let handoff = CancelInsideHandoff {
        cancellation: cancellation.clone(),
        attempted: AtomicUsize::new(0),
    };
    let fetcher =
        FixtureFetcher::with_pages([("https://example.com/", html("<title>ready</title>"))]);

    let outcome = CrawlerRuntime::new(fetcher)
        .execute(request(1), cancellation, &handoff)
        .await;

    assert!(matches!(outcome, CrawlerRuntimeOutcome::Canceled { .. }));
    assert_eq!(handoff.attempted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn crawler_index_handoff_atomically_replaces_once_and_persists_exact_success() {
    let temp = TempDir::new().unwrap();
    let manager = Arc::new(IndexManager::new(temp.path()));
    manager.create_tenant("crawler_destination").unwrap();
    manager
        .add_documents_durable(
            "crawler_destination",
            vec![Document::from_json(&serde_json::json!({
                "objectID": "last-good",
                "title": "last good"
            }))
            .unwrap()],
        )
        .await
        .unwrap();
    manager.drain_all_write_queues().await.unwrap();

    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let digest = ContentDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
    let store = CrawlerRunStore::new(temp.path());
    store.start(run_id, digest.clone(), 1).unwrap();
    let handoff = IndexCrawlerPublicationHandoff::new(
        Arc::clone(&manager),
        store.clone(),
        "crawler_destination".to_string(),
        run_id.to_string(),
        digest,
        now_unix_ms(),
    )
    .unwrap();
    let batch = CrawlerPublicationBatch {
        records: vec![serde_json::json!({
            "objectID": "replacement",
            "title": "replacement"
        })],
    };
    let runtime_counters = CrawlerRuntimeCounters {
        fetched: 1,
        discovered: 2,
        transformed: 1,
    };
    let guard = CrawlerExecutionGuard::new(CrawlerCancellation::default(), Duration::from_secs(60));

    handoff
        .handoff(batch.clone(), runtime_counters, &guard)
        .await
        .unwrap();
    let success = store.load(run_id).unwrap().unwrap();
    let terminal = success.crawler_run.unwrap().terminal.unwrap();
    assert_eq!(terminal.outcome, CrawlerTerminalOutcome::Succeeded);
    assert_eq!(terminal.counters.fetched, 1);
    assert_eq!(terminal.counters.discovered, 2);
    assert_eq!(terminal.counters.transformed, 1);
    assert_eq!(terminal.counters.published, 1);
    let publication = terminal.publication.unwrap();
    assert_eq!(publication.destination_index, "crawler_destination");
    assert!(publication.task_id > 0);
    assert_eq!(
        manager
            .get_task(&publication.task_id.to_string())
            .unwrap()
            .numeric_id,
        publication.task_id
    );
    assert!(publication.generation.as_str().starts_with("snapshot_"));
    assert!(publication.digest.as_str().starts_with("sha256:"));
    assert!(manager
        .get_document("crawler_destination", "last-good")
        .unwrap()
        .is_none());
    assert_eq!(
        manager
            .get_document("crawler_destination", "replacement")
            .unwrap()
            .unwrap()
            .id,
        "replacement"
    );

    assert!(handoff
        .handoff(batch, runtime_counters, &guard)
        .await
        .is_err());
    let replayed = store.load(run_id).unwrap().unwrap();
    assert_eq!(
        replayed
            .crawler_run
            .unwrap()
            .terminal
            .unwrap()
            .publication
            .unwrap(),
        publication
    );
}

#[tokio::test]
async fn crawler_index_handoff_publishes_first_generation_into_empty_destination() {
    let temp = TempDir::new().unwrap();
    let manager = Arc::new(IndexManager::new(temp.path()));
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ac";
    let digest = ContentDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    let store = CrawlerRunStore::new(temp.path());
    store.start(run_id, digest.clone(), 1).unwrap();

    let handoff = IndexCrawlerPublicationHandoff::new(
        Arc::clone(&manager),
        store.clone(),
        "empty_crawler_destination".to_string(),
        run_id.to_string(),
        digest,
        now_unix_ms(),
    )
    .unwrap();
    assert!(
        !temp.path().join("empty_crawler_destination").exists(),
        "constructing a handoff must not create the destination before publication"
    );

    handoff
        .handoff(
            CrawlerPublicationBatch {
                records: vec![serde_json::json!({
                    "objectID": "first-generation",
                    "title": "first generation"
                })],
            },
            CrawlerRuntimeCounters {
                fetched: 1,
                discovered: 0,
                transformed: 1,
            },
            &CrawlerExecutionGuard::new(CrawlerCancellation::default(), Duration::from_secs(60)),
        )
        .await
        .unwrap();

    let terminal = store
        .load(run_id)
        .unwrap()
        .unwrap()
        .crawler_run
        .unwrap()
        .terminal
        .unwrap();
    assert_eq!(terminal.outcome, CrawlerTerminalOutcome::Succeeded);
    assert_eq!(terminal.counters.published, 1);
    assert_eq!(
        manager
            .get_document("empty_crawler_destination", "first-generation")
            .unwrap()
            .unwrap()
            .id,
        "first-generation"
    );
}

#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn crawler_index_handoff_fetch_failure_leaves_no_physical_destination() {
    let _resolver = public_resolver(Arc::new(AtomicUsize::new(0)));
    let temp = TempDir::new().unwrap();
    let manager = Arc::new(IndexManager::new(temp.path()));
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ad";
    let digest = ContentDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap();
    let store = CrawlerRunStore::new(temp.path());
    store.start(run_id, digest.clone(), 1).unwrap();
    let handoff = IndexCrawlerPublicationHandoff::new(
        Arc::clone(&manager),
        store,
        "failed_crawler_destination".to_string(),
        run_id.to_string(),
        digest,
        now_unix_ms(),
    )
    .unwrap();

    let outcome = CrawlerRuntime::new(FixtureFetcher::default())
        .execute(request(1), CrawlerCancellation::default(), &handoff)
        .await;

    assert!(matches!(
        outcome,
        CrawlerRuntimeOutcome::Failed {
            code: CrawlerRuntimeErrorCode::TargetRejected,
            ..
        }
    ));
    assert!(!temp.path().join("failed_crawler_destination").exists());
}

#[tokio::test]
async fn crawler_index_handoff_canceled_before_publication_leaves_no_physical_destination() {
    let temp = TempDir::new().unwrap();
    let manager = Arc::new(IndexManager::new(temp.path()));
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ae";
    let digest = ContentDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap();
    let store = CrawlerRunStore::new(temp.path());
    store.start(run_id, digest.clone(), 1).unwrap();
    let handoff = IndexCrawlerPublicationHandoff::new(
        Arc::clone(&manager),
        store.clone(),
        "canceled_crawler_destination".to_string(),
        run_id.to_string(),
        digest,
        now_unix_ms(),
    )
    .unwrap();
    store.request_cancel(run_id, now_unix_ms()).unwrap();

    assert!(handoff
        .handoff(
            CrawlerPublicationBatch {
                records: vec![serde_json::json!({
                    "objectID": "must-not-publish",
                    "title": "must not publish"
                })],
            },
            CrawlerRuntimeCounters {
                fetched: 1,
                discovered: 0,
                transformed: 1,
            },
            &CrawlerExecutionGuard::new(CrawlerCancellation::default(), Duration::from_secs(60)),
        )
        .await
        .is_err());
    assert!(!temp.path().join("canceled_crawler_destination").exists());
}

struct AdvancingFetcher;

impl CrawlerFetcher for AdvancingFetcher {
    fn fetch<'a>(
        &'a self,
        _target: &'a VettedCrawlerUrlTarget,
        _max_decoded_body_bytes: u64,
        _guard: &'a CrawlerExecutionGuard,
    ) -> CrawlerBoxFuture<'a, Result<CrawlerFetchResponse, CrawlerRuntimeErrorCode>> {
        Box::pin(async {
            tokio::time::advance(Duration::from_secs(2)).await;
            Ok(html("<title>late</title>"))
        })
    }
}

#[derive(Clone, Copy)]
enum TransportStall {
    ConnectTls,
    ResponseHeaders,
    ResponseBody,
}

fn spawn_stalled_transport(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    stage: TransportStall,
    ready: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        if matches!(stage, TransportStall::ConnectTls) {
            ready.notify_one();
            std::future::pending::<()>().await;
        }
        let mut tls = acceptor.accept(tcp).await.unwrap();
        let _request = read_http_request(&mut tls).await;
        match stage {
            TransportStall::ConnectTls => unreachable!(),
            TransportStall::ResponseHeaders => {
                ready.notify_one();
                std::future::pending::<()>().await;
            }
            TransportStall::ResponseBody => {
                tls.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 100\r\n\r\nx",
                )
                .await
                .unwrap();
                tls.flush().await.unwrap();
                ready.notify_one();
                std::future::pending::<()>().await;
            }
        }
    })
}

async fn assert_transport_stall_maps_to_fetch_timeout(stage: TransportStall) {
    let identity = hermetic_tls_identity();
    let (listener, addr) = bind_hermetic_listener().await;
    let ready = Arc::new(Notify::new());
    let server = spawn_stalled_transport(
        listener,
        identity.acceptor.clone(),
        stage,
        Arc::clone(&ready),
    );
    let (connect_timeout, response_header_timeout, response_body_timeout) = match stage {
        TransportStall::ConnectTls => (
            Duration::from_millis(20),
            Duration::from_secs(5),
            Duration::from_secs(5),
        ),
        TransportStall::ResponseHeaders => (
            Duration::from_secs(5),
            Duration::from_millis(20),
            Duration::from_secs(5),
        ),
        TransportStall::ResponseBody => (
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_millis(20),
        ),
    };
    let mut fetcher = ReqwestCrawlerFetcher::for_hermetic_test(
        identity.client_root.clone(),
        connect_timeout,
        response_header_timeout,
        response_body_timeout,
    );
    let body_started = Arc::new(Notify::new());
    if matches!(stage, TransportStall::ResponseBody) {
        fetcher.policy.body_started = Some(Arc::clone(&body_started));
    }
    let target = pinned_test_target(addr, "/stall");
    let guard = CrawlerExecutionGuard::new(CrawlerCancellation::default(), Duration::from_secs(30));
    let fetch = tokio::spawn(async move { fetcher.fetch(&target, 1024, &guard).await });
    ready.notified().await;
    if matches!(stage, TransportStall::ResponseBody) {
        body_started.notified().await;
    }
    let result = tokio::time::timeout(Duration::from_millis(250), fetch)
        .await
        .expect("the targeted crawler transport deadline must beat the narrow watchdog")
        .unwrap();
    assert_eq!(result, Err(CrawlerRuntimeErrorCode::FetchTimeout));
    server.abort();
}

#[tokio::test(start_paused = true)]
#[serial(flapjack_outbound_url_policy)]
async fn crawler_runtime_monotonic_deadline_prevents_handoff_without_sleep() {
    let _resolver = public_resolver(Arc::new(AtomicUsize::new(0)));
    let target = vet_crawler_url_target("https://example.com/").unwrap();
    let start_url = target.canonical_url.as_str().to_owned();
    let guard = CrawlerExecutionGuard::new(CrawlerCancellation::default(), Duration::from_secs(1));
    let handoff = RecordingHandoff::default();
    let runtime = CrawlerRuntime::new(AdvancingFetcher);
    // Feed the already-admitted target through the private runtime seam so
    // paused Tokio time cannot race a blocking DNS worker.
    let outcome = runtime
        .execute_admitted(
            start_url,
            limits(1),
            CompiledCrawlerTransform::compile(transform()).unwrap(),
            guard,
            Some(target),
            &handoff,
        )
        .await;

    assert!(matches!(
        outcome,
        CrawlerRuntimeOutcome::Failed {
            code: CrawlerRuntimeErrorCode::DeadlineExceeded,
            ..
        }
    ));
    assert!(handoff.batches.lock().unwrap().is_empty());

    // Resume real monotonic time for the hermetic socket/TLS probes. Each has
    // a 250-ms outer watchdog only to turn a removed inner transport
    // deadline into a deterministic failure rather than a hung test process.
    tokio::time::resume();
    assert_transport_stall_maps_to_fetch_timeout(TransportStall::ConnectTls).await;
    assert_transport_stall_maps_to_fetch_timeout(TransportStall::ResponseHeaders).await;
    assert_transport_stall_maps_to_fetch_timeout(TransportStall::ResponseBody).await;
}

struct ConcurrencyProbeFetcher {
    barrier: Arc<tokio::sync::Barrier>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

impl CrawlerFetcher for ConcurrencyProbeFetcher {
    fn fetch<'a>(
        &'a self,
        target: &'a VettedCrawlerUrlTarget,
        _max_decoded_body_bytes: u64,
        _guard: &'a CrawlerExecutionGuard,
    ) -> CrawlerBoxFuture<'a, Result<CrawlerFetchResponse, CrawlerRuntimeErrorCode>> {
        Box::pin(async move {
            if target.canonical_url.path() == "/" {
                return Ok(html(
                    r#"<a href="/a">a</a><a href="/b">b</a><a href="/c">c</a>"#,
                ));
            }
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            if matches!(target.canonical_url.path(), "/a" | "/b") {
                self.barrier.wait().await;
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(html("<p>leaf</p>"))
        })
    }
}

#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn crawler_runtime_enforces_concurrency_cap_without_timing_assertions() {
    let _resolver = public_resolver(Arc::new(AtomicUsize::new(0)));
    let max_active = Arc::new(AtomicUsize::new(0));
    let fetcher = ConcurrencyProbeFetcher {
        barrier: Arc::new(tokio::sync::Barrier::new(2)),
        active: Arc::new(AtomicUsize::new(0)),
        max_active: Arc::clone(&max_active),
    };
    let handoff = RecordingHandoff::default();

    let outcome = CrawlerRuntime::new(fetcher)
        .execute(request(4), CrawlerCancellation::default(), &handoff)
        .await;

    assert!(matches!(outcome, CrawlerRuntimeOutcome::Handoff { .. }));
    assert_eq!(max_active.load(Ordering::SeqCst), 2);
}

#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn crawler_runtime_rejects_zero_or_client_widened_resource_limits() {
    macro_rules! rejects_limit {
        ($field:ident, $value:expr) => {{
            let mut invalid = request(1);
            invalid.limits.$field = $value;
            assert_eq!(
                validate_request(&invalid),
                Err(CrawlerRuntimeErrorCode::CrawlLimitExceeded),
                "{}={:?} must reject",
                stringify!($field),
                $value
            );
        }};
    }
    rejects_limit!(max_depth, 0);
    rejects_limit!(max_depth, MAX_CRAWLER_DEPTH + 1);
    rejects_limit!(max_pages, 0);
    rejects_limit!(max_pages, MAX_CRAWLER_PAGES + 1);
    rejects_limit!(max_decoded_body_bytes, 0);
    rejects_limit!(max_decoded_body_bytes, MAX_CRAWLER_DECODED_BODY_BYTES + 1);
    rejects_limit!(max_record_bytes, 0);
    rejects_limit!(max_record_bytes, MAX_CRAWLER_RECORD_BYTES + 1);
    rejects_limit!(max_records, 0);
    rejects_limit!(max_records, MAX_CRAWLER_RECORDS + 1);
    rejects_limit!(max_concurrency, 0);
    rejects_limit!(max_concurrency, MAX_CRAWLER_CONCURRENCY + 1);

    for duration in [
        Duration::ZERO,
        MAX_CRAWLER_RUN_DURATION + Duration::from_nanos(1),
    ] {
        let mut invalid = request(1);
        invalid.max_run_duration = duration;
        assert_eq!(
            validate_request(&invalid),
            Err(CrawlerRuntimeErrorCode::CrawlLimitExceeded)
        );
    }

    let _resolver = public_resolver(Arc::new(AtomicUsize::new(0)));
    let cases = [
        (
            "page queue",
            FixtureFetcher::with_pages([(
                "https://example.com/",
                html(r#"<a href="/overflow">overflow</a>"#),
            )]),
            request(1),
            CrawlerRuntimeErrorCode::CrawlLimitExceeded,
        ),
        (
            "decoded body",
            FixtureFetcher::with_pages([(
                "https://example.com/",
                html("<body>decoded expansion exceeds eight bytes</body>"),
            )]),
            {
                let mut request = request(1);
                request.limits.max_decoded_body_bytes = 8;
                request
            },
            CrawlerRuntimeErrorCode::BodyLimitExceeded,
        ),
        (
            "record bytes",
            FixtureFetcher::with_pages([(
                "https://example.com/",
                html("<body>record exceeds the configured output cap</body>"),
            )]),
            {
                let mut request = request(1);
                request.limits.max_record_bytes = 8;
                request
            },
            CrawlerRuntimeErrorCode::CrawlLimitExceeded,
        ),
        (
            "record count",
            FixtureFetcher::with_pages([
                (
                    "https://example.com/",
                    html(r#"<a href="/second">second</a>"#),
                ),
                ("https://example.com/second", html("second")),
            ]),
            {
                let mut request = request(2);
                request.limits.max_records = 1;
                request
            },
            CrawlerRuntimeErrorCode::CrawlLimitExceeded,
        ),
    ];
    for (name, fetcher, request, expected) in cases {
        let handoff = RecordingHandoff::default();
        let outcome = CrawlerRuntime::new(fetcher)
            .execute(request, CrawlerCancellation::default(), &handoff)
            .await;
        assert!(
            matches!(outcome, CrawlerRuntimeOutcome::Failed { code, .. } if code == expected),
            "{name} exhaustion must fail safely, got {outcome:?}"
        );
        assert!(
            handoff.batches.lock().unwrap().is_empty(),
            "{name} exhaustion must never hand off a partial batch"
        );
    }
}

#[test]
fn crawler_runtime_same_origin_canonicalization_preserves_query_and_drops_fragment() {
    let origin = "https://example.com";
    assert_eq!(
        canonical_same_origin_link(
            "https://example.com/catalog/start",
            "../next?q=kept#gone",
            origin,
        ),
        Some("https://example.com/next?q=kept".to_owned())
    );
    for href in [
        "http://example.com/no",
        "https://other.example/no",
        "mailto:private@example.com",
        "https://user@example.com/no",
    ] {
        assert_eq!(
            canonical_same_origin_link("https://example.com/", href, origin),
            None
        );
    }
}

// Compile-time assertion: runtime tests have no network-capable fallback and
// the post-admission fixture fetcher is safe to share across bounded batches.
#[test]
fn crawler_runtime_fixture_fetcher_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FixtureFetcher>();
}
