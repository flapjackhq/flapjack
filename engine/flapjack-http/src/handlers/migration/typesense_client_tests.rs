use super::source_identity_partitions::SourceIdentityConfig;
use super::source_reader::source_record_identity_page;
use super::source_snapshot::SourceSnapshotBuilder;
/// The loopback seam is reachable in every profile behind the explicit
/// `FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1` opt-in, so the tests below and this
/// import run un-gated.
use super::typesense_client::TYPESENSE_PREVIEW_LOOPBACK_ENV;
use super::typesense_client::{
    capture_source_with_transport, decode_document_page, encoded_collection_name,
    fetch_document_pages_with_expected_count_for_test, fetch_document_pages_with_transport,
    list_collections_with_transport, observe_source_with_transport, read_settings_with_transport,
    require_read_access_with_transport, TraversalLimits, TypesenseClient, TypesenseClientError,
    TypesenseErrorKind, TypesenseMethod, TypesenseRequest, TypesenseResponse, TypesenseTransport,
    CONNECT_TIMEOUT, MAX_DOCUMENT_ITEMS, MAX_RESPONSE_BYTES, REQUEST_TIMEOUT,
};
use super::typesense_source_reader::{map_typesense_client_error, typesense_document_records};
use super::AlgoliaErrorKind;
use flapjack::security::test_helpers::install_test_outbound_host_resolver;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::future::Future;
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const RAW_ENDPOINT_CANARY: &str = "https://tenant-secret.typesense.example";
const API_KEY_CANARY: &str = "typesense-secret-api-key";
const SOURCE_CANARY: &str = "private-typesense-collection";
const LIVE_NETWORK_AWAIT_TIMEOUT: Duration = Duration::from_secs(10);

use crate::router_tests::typesense_fixture_test_support::product_ids as expected_product_ids;

fn assert_error_is_sanitized(error: &TypesenseClientError) {
    let debug = format!("{error:?}");
    let serialized = serde_json::to_string(error).unwrap();
    for canary in [RAW_ENDPOINT_CANARY, API_KEY_CANARY, SOURCE_CANARY] {
        assert!(!debug.contains(canary), "Debug leaked credential canary");
        assert!(!error.safe_message().contains(canary));
        assert!(
            !serialized.contains(canary),
            "serialized error leaked credential canary"
        );
    }
}

#[test]
fn typesense_client_errors_use_the_canonical_source_boundary_mapping() {
    for (source_kind, expected_kind) in [
        (TypesenseErrorKind::Validation, AlgoliaErrorKind::Validation),
        (TypesenseErrorKind::Transport, AlgoliaErrorKind::Transport),
        (TypesenseErrorKind::Timeout, AlgoliaErrorKind::Timeout),
        (TypesenseErrorKind::Redirect, AlgoliaErrorKind::Redirect),
        (TypesenseErrorKind::Upstream, AlgoliaErrorKind::Upstream),
        (TypesenseErrorKind::Schema, AlgoliaErrorKind::Schema),
        (TypesenseErrorKind::Progress, AlgoliaErrorKind::Progress),
        (TypesenseErrorKind::Limit, AlgoliaErrorKind::Limit),
    ] {
        let mapped = map_typesense_client_error(TypesenseClientError::new(
            source_kind,
            "Typesense mapping canary",
        ));

        assert_eq!(mapped.kind(), expected_kind);
        assert_eq!(mapped.safe_message(), "Typesense mapping canary");
    }
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn production_constructor_accepts_documented_typesense_cloud_hostnames() {
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, port| {
        assert_eq!(port, Some(443));
        assert!(!host.is_empty(), "vetted host must be passed to resolver");
        Some(vec!["8.8.8.8".parse::<IpAddr>().unwrap()])
    }));

    for endpoint in [
        "https://typesense.net",
        "https://your-instance.typesense.net",
        "https://tenant.region.typesense.net",
    ] {
        TypesenseClient::new(endpoint, API_KEY_CANARY, "products")
            .expect("documented Typesense Cloud hostname should be admitted");
    }
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn production_constructor_rejects_sibling_suffix_authority_confusion() {
    let _resolver = install_test_outbound_host_resolver(Arc::new(|_, _| {
        Some(vec!["8.8.8.8".parse::<IpAddr>().unwrap()])
    }));

    for endpoint in [
        RAW_ENDPOINT_CANARY,
        "https://example.com",
        "https://eviltypesense.net",
        "https://typesense.net.evil.test",
        "https://typesense.com",
        "https://tenant.typesense.com",
        "https://127.0.0.1",
        "http://localhost:8108",
    ] {
        let error = TypesenseClient::new(endpoint, API_KEY_CANARY, SOURCE_CANARY).unwrap_err();
        assert_eq!(error.kind(), TypesenseErrorKind::Validation);
        assert_eq!(
            error.safe_message(),
            "Typesense Cloud endpoint is not allowed"
        );
        assert_error_is_sanitized(&error);
    }
}

#[test]
fn client_debug_scrubs_endpoint_key_and_source_identity() {
    let client = TypesenseClient::for_test(
        "tenant-secret.typesense.example",
        vec!["8.8.8.8:443".parse().unwrap()],
        API_KEY_CANARY,
        SOURCE_CANARY,
    )
    .unwrap();
    let debug = format!("{client:?}");
    for canary in [
        "tenant-secret.typesense.example",
        API_KEY_CANARY,
        SOURCE_CANARY,
        "8.8.8.8",
    ] {
        assert!(!debug.contains(canary), "client Debug leaked {canary}");
    }
}

#[test]
fn request_builder_keeps_api_key_out_of_url_and_diagnostics() {
    let client = TypesenseClient::for_test(
        "tenant-secret.typesense.example",
        vec!["8.8.8.8:443".parse().unwrap()],
        API_KEY_CANARY,
        "catalog/2026",
    )
    .unwrap();
    let request = client
        .build_http_request(TypesenseRequest {
            method: TypesenseMethod::Get,
            path: "/collections/catalog%2F2026".to_string(),
            body: None,
        })
        .unwrap();

    assert_eq!(
        request.url().as_str(),
        "https://tenant-secret.typesense.example/collections/catalog%2F2026"
    );
    assert!(!request.url().as_str().contains(API_KEY_CANARY));
    assert_eq!(
        request.headers()["x-typesense-api-key"].to_str().unwrap(),
        API_KEY_CANARY
    );
    assert!(
        !format!("{request:?}").contains(API_KEY_CANARY),
        "request Debug must redact the Typesense API key header"
    );
}

#[test]
fn client_transport_time_budgets_are_bounded() {
    assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(5));
    assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(30));
}

#[test]
fn default_document_traversal_budget_is_the_advertised_item_ceiling() {
    assert_eq!(
        TraversalLimits::default(),
        TraversalLimits {
            max_items: MAX_DOCUMENT_ITEMS
        },
        "the export stream carries no budget other than the advertised item ceiling"
    );
}

/// The item ceiling is an exact boundary on the single stream: a stream of
/// exactly `max_items` values is accepted and one more value is refused.
#[tokio::test]
async fn export_stream_item_ceiling_accepts_the_limit_and_refuses_one_more() {
    for (documents, expected_error) in [(3usize, None), (4, Some(TypesenseErrorKind::Limit))] {
        let mut transport = ScriptedTransport::with_export_responses([numbered_page(0, documents)]);
        let result = fetch_document_pages_with_transport(
            &mut transport,
            "catalog",
            TraversalLimits { max_items: 3 },
            |_| Ok::<_, TypesenseClientError>(()),
        )
        .await;

        match expected_error {
            None => result.expect("a stream at the item ceiling must be accepted"),
            Some(kind) => {
                let error = result.expect_err("a stream past the item ceiling must be refused");
                assert_eq!(error.kind(), kind);
                assert_error_is_sanitized(&error);
            }
        }
        assert_eq!(transport.requests.len(), 1);
    }
}

#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn client_bypasses_ambient_https_proxy_and_uses_pinned_address() {
    let direct_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("direct-listener precondition must bind");
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy-listener precondition must bind");
    let direct_address = direct_listener.local_addr().unwrap();
    let proxy_address = proxy_listener.local_addr().unwrap();
    let _proxy =
        crate::test_helpers::with_env_var("HTTPS_PROXY", &format!("http://{proxy_address}"));
    let client = TypesenseClient::for_test(
        "pinned-target.example",
        vec![direct_address],
        API_KEY_CANARY,
        "catalog",
    )
    .unwrap();

    let request = tokio::spawn(async move { client.observe_source().await });
    let (direct_stream, _) = tokio::time::timeout(Duration::from_secs(2), direct_listener.accept())
        .await
        .expect("client did not use the pinned direct address")
        .expect("direct listener accept failed");
    drop(direct_stream);

    let error = tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .expect("request did not fail after the direct listener closed")
        .expect("request task panicked")
        .expect_err("the listener intentionally closes before TLS");
    assert_eq!(error.kind(), TypesenseErrorKind::Transport);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), proxy_listener.accept())
            .await
            .is_err(),
        "ambient HTTPS proxy unexpectedly received the request"
    );
    assert_error_is_sanitized(&error);
}

#[test]
fn collection_name_is_percent_encoded_as_one_path_segment() {
    assert_eq!(
        encoded_collection_name("catalog/../../private?x=1"),
        "catalog%2F..%2F..%2Fprivate%3Fx%3D1"
    );
    assert_eq!(
        encoded_collection_name("books and tools"),
        "books%20and%20tools"
    );
}

#[test]
fn document_stream_decoding_preserves_every_value_in_stream_order() {
    let documents = decode_document_page(
        br#"{"id":"prod_001"}
{"id":"prod_002"}"#,
    )
    .unwrap();

    assert_eq!(documents.len(), 2);
    assert_eq!(
        documents
            .iter()
            .map(|document| document["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["prod_001", "prod_002"]
    );
}

#[test]
fn document_export_decoding_reads_json_values_without_search_envelope() {
    let documents = decode_document_page(
        br#"{"id":"prod_001","title":"Alpha"}
{"id":"prod_002","title":"Beta"}"#,
    )
    .unwrap();

    assert_eq!(documents.len(), 2);
    assert_eq!(
        documents
            .iter()
            .map(|document| document["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["prod_001", "prod_002"]
    );
}

#[test]
fn document_page_decoding_rejects_malformed_and_over_budget_responses() {
    for malformed in [
        br#"{"id":"prod_001""#.as_slice(),
        br#"[{"id":"prod_001"}]"#.as_slice(),
        br#""not-an-object""#.as_slice(),
    ] {
        let error = decode_document_page(malformed).unwrap_err();
        assert_eq!(error.kind(), TypesenseErrorKind::Progress);
        assert_error_is_sanitized(&error);
    }

    let oversized = vec![b' '; MAX_RESPONSE_BYTES + 1];
    let error = decode_document_page(&oversized).unwrap_err();
    assert_eq!(error.kind(), TypesenseErrorKind::Limit);
    assert_error_is_sanitized(&error);
}

#[derive(Default)]
struct ScriptedTransport {
    responses: VecDeque<TypesenseResponse>,
    requests: Vec<TypesenseRequest>,
}

impl ScriptedTransport {
    fn with_json_responses(responses: impl IntoIterator<Item = Value>) -> Self {
        Self {
            responses: responses
                .into_iter()
                .map(|body| TypesenseResponse {
                    status: 200,
                    body: serde_json::to_vec(&body).unwrap(),
                })
                .collect(),
            requests: Vec::new(),
        }
    }

    fn with_export_responses(responses: impl IntoIterator<Item = Vec<Value>>) -> Self {
        Self {
            responses: responses
                .into_iter()
                .map(|documents| TypesenseResponse {
                    status: 200,
                    body: export_body(documents),
                })
                .collect(),
            requests: Vec::new(),
        }
    }
}

impl TypesenseTransport for ScriptedTransport {
    fn send<'a>(
        &'a mut self,
        request: TypesenseRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TypesenseResponse, TypesenseClientError>> + Send + 'a>>
    {
        self.requests.push(request);
        Box::pin(async move {
            self.responses.pop_front().ok_or_else(|| {
                TypesenseClientError::new(
                    TypesenseErrorKind::Transport,
                    "Typesense transport failed",
                )
            })
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct RecordedExchange {
    request: TypesenseRequest,
    no_terminal_newline: Option<bool>,
}

struct RecordingTransport<T> {
    delegate: T,
    exchanges: Vec<RecordedExchange>,
}

impl<T> RecordingTransport<T> {
    fn new(delegate: T) -> Self {
        Self {
            delegate,
            exchanges: Vec::new(),
        }
    }
}

impl<T: TypesenseTransport + Send> TypesenseTransport for RecordingTransport<T> {
    fn send<'a>(
        &'a mut self,
        request: TypesenseRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TypesenseResponse, TypesenseClientError>> + Send + 'a>>
    {
        Box::pin(async move {
            let recorded_request = request.clone();
            let result = self.delegate.send(request).await;
            let no_terminal_newline = result
                .as_ref()
                .ok()
                .map(|response| response.body.last().is_some_and(|byte| *byte != b'\n'));
            self.exchanges.push(RecordedExchange {
                request: recorded_request,
                no_terminal_newline,
            });
            result
        })
    }
}

#[derive(Default)]
struct UnderreportedExportTransport {
    requests: Vec<TypesenseRequest>,
}

const UNDERREPORTED_DECLARED_DOCUMENTS: usize = 100;
const UNDERREPORTED_STREAMED_DOCUMENTS: usize = 200;

impl TypesenseTransport for UnderreportedExportTransport {
    fn send<'a>(
        &'a mut self,
        request: TypesenseRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TypesenseResponse, TypesenseClientError>> + Send + 'a>>
    {
        let path = request.path.clone();
        self.requests.push(request);
        Box::pin(async move {
            match path.as_str() {
                "/collections/catalog" => {
                    Ok(json_response(collection(UNDERREPORTED_DECLARED_DOCUMENTS)))
                }
                "/collections/catalog/documents/export" => Ok(export_response(numbered_page(
                    0,
                    UNDERREPORTED_STREAMED_DOCUMENTS,
                ))),
                _ => Err(TypesenseClientError::new(
                    TypesenseErrorKind::Transport,
                    "Typesense transport failed",
                )),
            }
        })
    }
}

fn page(ids: &[&str]) -> Vec<Value> {
    ids.iter().map(|id| json!({"id": id})).collect()
}

fn numbered_page(start: usize, count: usize) -> Vec<Value> {
    (start..start + count)
        .map(|id| json!({"id": format!("prod_{id:03}")}))
        .collect()
}

/// A Typesense 30.2 export body: newline-separated JSON values with no terminal
/// newline, which is the exact shape the live contract pins.
fn export_body(documents: Vec<Value>) -> Vec<u8> {
    documents
        .iter()
        .map(|document| serde_json::to_string(document).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

#[tokio::test]
async fn recording_transport_delegates_the_original_request_unchanged() {
    let request = TypesenseRequest {
        method: TypesenseMethod::Get,
        path: "/collections/catalog/documents/export?sentinel=original".to_string(),
        body: None,
    };
    let response = TypesenseResponse {
        status: 200,
        body: br#"{"id":"prod_001"}"#.to_vec(),
    };
    let mut transport = RecordingTransport::new(ScriptedTransport {
        responses: VecDeque::from([response.clone()]),
        requests: Vec::new(),
    });

    assert_eq!(transport.send(request.clone()).await.unwrap(), response);
    assert_eq!(transport.delegate.requests, vec![request.clone()]);
    assert_eq!(
        transport.exchanges,
        vec![RecordedExchange {
            request,
            no_terminal_newline: Some(true),
        }]
    );
}

#[tokio::test]
async fn recording_transport_records_delegate_errors_before_propagating() {
    let request = TypesenseRequest {
        method: TypesenseMethod::Get,
        path: "/collections/catalog/documents/export".to_string(),
        body: None,
    };
    let mut transport = RecordingTransport::new(ScriptedTransport::default());

    let error = transport.send(request.clone()).await.unwrap_err();

    assert_eq!(error.kind(), TypesenseErrorKind::Transport);
    assert_eq!(
        transport.exchanges,
        vec![RecordedExchange {
            request,
            no_terminal_newline: None,
        }],
        "failed delegate attempts must remain visible to request-count oracles"
    );
}

/// The 137 canonical fixture products as export documents, in fixture order.
fn fixture_export_documents() -> Vec<Value> {
    expected_product_ids()
        .iter()
        .map(|id| json!({ "id": id }))
        .collect()
}

fn export_exchanges<T>(transport: &RecordingTransport<T>) -> Vec<&RecordedExchange> {
    transport
        .exchanges
        .iter()
        .filter(|exchange| exchange.request.path.contains("/documents/export"))
        .collect()
}

#[tokio::test]
async fn source_capture_reads_the_whole_collection_from_one_query_free_export_stream() {
    let documents = fixture_export_documents();
    assert_eq!(documents.len(), 137, "the live denominator must be exact");
    let mut transport = RecordingTransport::new(ScriptedTransport {
        responses: VecDeque::from([
            json_response(collection(documents.len())),
            export_response(documents.clone()),
            json_response(collection(documents.len())),
        ]),
        requests: Vec::new(),
    });
    let mut captured_ids = Vec::new();

    capture_source_with_transport(&mut transport, "catalog", |page| {
        captured_ids.extend(
            page.iter()
                .map(|document| document["id"].as_str().unwrap().to_string()),
        );
        Ok::<_, TypesenseClientError>(())
    })
    .await
    .expect("one bounded Typesense export stream must capture the whole collection");

    assert_eq!(
        captured_ids,
        expected_product_ids(),
        "every fixture value must be consumed exactly once, in stream order"
    );
    let exports = export_exchanges(&transport);
    assert_eq!(exports.len(), 1, "export is one unpaginated stream request");
    assert_eq!(
        exports[0].request.path, "/collections/catalog/documents/export",
        "Typesense 30.2 export takes no page/per_page query"
    );
    assert_eq!(
        exports[0].no_terminal_newline,
        Some(true),
        "the fixture stream must carry no terminal newline"
    );
}

#[tokio::test]
async fn counted_export_consumes_the_whole_stream_in_one_query_free_request() {
    let documents = fixture_export_documents();
    let mut transport = RecordingTransport::new(ScriptedTransport {
        responses: VecDeque::from([export_response(documents.clone())]),
        requests: Vec::new(),
    });
    let mut consumed_pages = Vec::new();

    fetch_document_pages_with_expected_count_for_test(
        &mut transport,
        "catalog/2026",
        TraversalLimits::default(),
        documents.len(),
        |page| {
            consumed_pages.push(page);
            Ok::<_, TypesenseClientError>(())
        },
    )
    .await
    .expect("the counted export must accept the whole stream in one request");

    assert_eq!(
        consumed_pages,
        vec![documents],
        "the export stream is handed to the consumer once, unsplit"
    );
    let exports = export_exchanges(&transport);
    assert_eq!(exports.len(), 1);
    assert_eq!(
        exports[0].request.path,
        format!(
            "/collections/{}/documents/export",
            encoded_collection_name("catalog/2026")
        )
    );
}

#[tokio::test]
async fn read_access_probe_uses_the_same_query_free_export_path() {
    let documents = fixture_export_documents();
    let mut transport = RecordingTransport::new(ScriptedTransport {
        responses: VecDeque::from([
            json_response(collection(documents.len())),
            export_response(documents),
        ]),
        requests: Vec::new(),
    });

    require_read_access_with_transport(&mut transport, "catalog")
        .await
        .expect("a readable collection must admit the query-free export probe");

    let exports = export_exchanges(&transport);
    assert_eq!(exports.len(), 1);
    assert_eq!(
        exports[0].request.path,
        "/collections/catalog/documents/export"
    );
}

#[tokio::test]
#[ignore = "requires the pinned Typesense 30.2 live contract driver"]
async fn typesense_export_stream_live_contract() {
    let endpoint = std::env::var("TYPESENSE_ENDPOINT").expect("driver must pass loopback endpoint");
    let api_key = std::env::var("TYPESENSE_API_KEY").expect("driver must pass scoped key");
    let collection =
        std::env::var("TYPESENSE_COLLECTION").expect("driver must pass source collection");
    let expected_path = PathBuf::from(
        std::env::var("TYPESENSE_EXPECTED_IDS_FILE")
            .expect("driver must pass expected-ID artifact"),
    );
    let expected_ids = std::fs::read_to_string(&expected_path)
        .expect("expected-ID artifact must be readable")
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(expected_ids.len(), 137, "live denominator must be exact");

    let upstream = url::Url::parse(&endpoint).expect("driver endpoint must be a URL");
    let upstream_ip: IpAddr = upstream
        .host_str()
        .expect("driver endpoint must have a host")
        .parse()
        .expect("driver endpoint must use a literal loopback address");
    assert!(upstream_ip.is_loopback());
    assert!(upstream.port_or_known_default().is_some());
    let _loopback = crate::test_helpers::with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "1");
    let discovery_client = TypesenseClient::new_discovery_preview_loopback(&endpoint, &api_key)
        .expect("driver loopback endpoint must be admitted for discovery");
    // `RecordingTransport` decorates the production transport, so byte-limit
    // enforcement, error-kind mapping, and `resolve_to_addrs` pinning keep one
    // owner. The finite verdict comes from the client's own `CONNECT_TIMEOUT`
    // and `REQUEST_TIMEOUT` plus the `LIVE_NETWORK_AWAIT_TIMEOUT` wrappers
    // below, not from a second bounded-await transport.
    let mut discovery_transport = RecordingTransport::new(discovery_client.transport());
    let discovered = tokio::time::timeout(
        LIVE_NETWORK_AWAIT_TIMEOUT,
        list_collections_with_transport(&mut discovery_transport, None, None),
    )
    .await
    .expect("collection discovery exceeded the live-contract deadline")
    .expect("collection discovery must succeed before export");
    assert!(
        discovered.iter().any(|item| item.name == collection),
        "driver collection must be visible to scoped discovery"
    );
    let discovery_export_requests = discovery_transport
        .exchanges
        .iter()
        .filter(|exchange| exchange.request.path.contains("/documents/export"))
        .count();
    assert_eq!(discovery_export_requests, 0);

    let client = TypesenseClient::new_preview_loopback(&endpoint, &api_key, &collection)
        .expect("driver loopback endpoint must be admitted for capture");
    let mut transport = RecordingTransport::new(client.transport());
    let mut captured_ids = Vec::new();
    let capture = tokio::time::timeout(
        LIVE_NETWORK_AWAIT_TIMEOUT,
        capture_source_with_transport(&mut transport, &collection, |documents| {
            captured_ids.extend(documents.into_iter().map(|document| {
                document["id"]
                    .as_str()
                    .expect("Typesense fixture IDs must be strings")
                    .to_string()
            }));
            Ok::<_, TypesenseClientError>(())
        }),
    )
    .await
    .expect("source capture exceeded the live-contract deadline");
    let export_requests = transport
        .exchanges
        .iter()
        .filter(|exchange| exchange.request.path.contains("/documents/export"))
        .collect::<Vec<_>>();
    assert_eq!(
        export_requests.len(),
        1,
        "capture must issue one export request"
    );
    assert_eq!(export_requests[0].request.method, TypesenseMethod::Get);
    assert_eq!(
        export_requests[0].request.path,
        format!(
            "/collections/{}/documents/export",
            encoded_collection_name(&collection)
        ),
        "Typesense 30.2 export is one stream and rejects fabricated page/per_page traversal"
    );
    capture.expect("one bounded Typesense export stream must capture successfully");

    captured_ids.sort();
    assert_eq!(
        captured_ids, expected_ids,
        "all exact fixture IDs are required"
    );
    assert!(
        export_requests[0].no_terminal_newline == Some(true),
        "Typesense 30.2 export fixture must preserve no terminal newline"
    );
    let captured_path = expected_path.with_file_name("captured_product_ids.txt");
    std::fs::write(&captured_path, format!("{}\n", captured_ids.join("\n")))
        .expect("captured-ID artifact must be writable beside the driver artifact");
    println!(
        "TYPESENSE_EXPORT_STREAM_CONTRACT documents=137 exact_ids=PASS export_requests=1 query_pagination=absent no_terminal_newline=PASS discovery_export_requests=0"
    );
}

/// The uncounted traversal — the variant with no advertised document count —
/// still reads the whole stream from one percent-encoded query-free request.
#[tokio::test]
async fn uncounted_document_traversal_streams_fixture_ids_from_one_request() {
    let expected_ids = expected_product_ids()
        .into_iter()
        .take(3)
        .collect::<Vec<_>>();
    assert_eq!(expected_ids.len(), 3, "the stream specimen uses three IDs");
    let mut transport = ScriptedTransport::with_export_responses([page(&[
        &expected_ids[0],
        &expected_ids[1],
        &expected_ids[2],
    ])]);
    let mut page_counts = Vec::new();
    let mut ids = Vec::new();

    fetch_document_pages_with_transport(
        &mut transport,
        "catalog/2026",
        TraversalLimits::default(),
        |documents| {
            page_counts.push(documents.len());
            ids.extend(
                documents
                    .iter()
                    .map(|document| document["id"].as_str().unwrap().to_string()),
            );
            Ok::<_, TypesenseClientError>(())
        },
    )
    .await
    .unwrap();

    assert_eq!(page_counts, vec![3], "the stream is consumed once, unsplit");
    assert_eq!(ids, expected_ids);
    assert_eq!(transport.requests.len(), 1);
    assert_eq!(transport.requests[0].method, TypesenseMethod::Get);
    assert_eq!(
        transport.requests[0].path,
        format!(
            "/collections/{}/documents/export",
            encoded_collection_name("catalog/2026")
        )
    );
    assert_eq!(transport.requests[0].body, None);
}

/// The counted traversal admits exactly the advertised number of values. A
/// short stream, a long stream, and a stream carrying an extra in-stream error
/// object all disagree with the advertised count and are refused before any
/// value reaches the consumer.
#[tokio::test]
async fn counted_export_rejects_streams_that_do_not_match_the_advertised_count() {
    let specimens = [
        page(&["prod_001", "prod_002"]),
        page(&["prod_001", "prod_002", "prod_003", "prod_004"]),
        vec![
            json!({"id": "prod_001"}),
            json!({"id": "prod_002"}),
            json!({"code": 500, "error": "Not found."}),
            json!({"id": "prod_003"}),
        ],
    ];

    for documents in specimens {
        let mut transport = ScriptedTransport::with_export_responses([documents]);
        let mut consumed = 0usize;
        let error = fetch_document_pages_with_expected_count_for_test(
            &mut transport,
            "catalog",
            TraversalLimits::default(),
            3,
            |page| {
                consumed += page.len();
                Ok::<_, TypesenseClientError>(())
            },
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind(), TypesenseErrorKind::Progress);
        assert_eq!(
            consumed, 0,
            "a rejected stream must never reach the consumer"
        );
        assert_eq!(transport.requests.len(), 1);
        assert_eq!(
            transport.requests[0].path,
            "/collections/catalog/documents/export"
        );
        assert_error_is_sanitized(&error);
    }
}

#[tokio::test]
async fn counted_export_rejects_advertised_count_above_item_ceiling_as_limit() {
    let mut transport =
        ScriptedTransport::with_export_responses([page(&["prod_001", "prod_002", "prod_003"])]);
    let mut consumed = 0usize;

    let error = fetch_document_pages_with_expected_count_for_test(
        &mut transport,
        "catalog",
        TraversalLimits { max_items: 3 },
        4,
        |documents| {
            consumed += documents.len();
            Ok::<_, TypesenseClientError>(())
        },
    )
    .await
    .expect_err("advertised counts above the traversal ceiling must be refused");

    assert_eq!(error.kind(), TypesenseErrorKind::Limit);
    assert_eq!(consumed, 0, "a rejected stream must not reach the consumer");
    assert!(
        transport.requests.is_empty(),
        "an impossible advertised count must fail before requesting the stream"
    );
    assert_error_is_sanitized(&error);
}

#[tokio::test]
async fn document_traversal_refuses_redirects_without_following_them() {
    let mut transport = ScriptedTransport {
        responses: VecDeque::from([TypesenseResponse {
            status: 302,
            body: Vec::new(),
        }]),
        requests: Vec::new(),
    };
    let error = fetch_document_pages_with_transport(
        &mut transport,
        "catalog",
        TraversalLimits::default(),
        |_| Ok::<_, TypesenseClientError>(()),
    )
    .await
    .unwrap_err();

    assert_eq!(error.kind(), TypesenseErrorKind::Redirect);
    assert_eq!(transport.requests.len(), 1);
}

#[tokio::test]
async fn source_capture_accepts_empty_collection_export() {
    let mut transport = ScriptedTransport {
        responses: VecDeque::from([
            json_response(collection(0)),
            export_response(Vec::new()),
            json_response(collection(0)),
        ]),
        requests: Vec::new(),
    };
    let mut consumed_pages = 0usize;

    let capture = capture_source_with_transport(&mut transport, "catalog", |documents| {
        consumed_pages += 1;
        assert!(documents.is_empty());
        Ok::<_, TypesenseClientError>(())
    })
    .await
    .unwrap();

    assert_eq!(capture.observation().document_count, 0);
    assert_eq!(consumed_pages, 0);
    assert_eq!(
        transport
            .requests
            .iter()
            .map(|request| (request.method, request.path.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (TypesenseMethod::Get, "/collections/catalog"),
            (
                TypesenseMethod::Get,
                "/collections/catalog/documents/export"
            ),
            (TypesenseMethod::Get, "/collections/catalog"),
        ]
    );
}

#[tokio::test]
async fn source_capture_rejects_underreported_metadata_with_trailing_export_data() {
    let mut transport = UnderreportedExportTransport::default();
    let mut consumed_ids = Vec::new();

    let error = capture_source_with_transport(&mut transport, "catalog", |documents| {
        consumed_ids.extend(
            documents
                .iter()
                .map(|document| document["id"].as_str().unwrap().to_string()),
        );
        Ok::<_, TypesenseClientError>(())
    })
    .await
    .unwrap_err();

    assert_eq!(error.kind(), TypesenseErrorKind::Progress);
    assert!(
        consumed_ids.is_empty(),
        "a stream longer than the advertised count must never reach the consumer"
    );
    assert_eq!(
        transport
            .requests
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/collections/catalog",
            "/collections/catalog/documents/export",
        ]
    );
    assert_error_is_sanitized(&error);
}

#[tokio::test]
async fn restricted_credentials_fail_before_document_access_is_accepted() {
    let mut transport = ScriptedTransport::with_json_responses([collection(3)]);
    transport.responses.push_back(TypesenseResponse {
        status: 403,
        body: Vec::new(),
    });

    let error = require_read_access_with_transport(&mut transport, "catalog")
        .await
        .unwrap_err();

    assert_eq!(error.kind(), TypesenseErrorKind::Upstream);
    assert_eq!(
        error.safe_message(),
        "Typesense source credentials lack required read access"
    );
    assert_eq!(transport.requests.len(), 2);
    assert_error_is_sanitized(&error);
}

fn collection(document_count: usize) -> Value {
    json!({
        "name": "catalog",
        "num_documents": document_count,
        "created_at": 1785020400_u64,
        "fields": [{"name": "id", "type": "string"}],
        "default_sorting_field": "price",
        "enable_nested_fields": true,
        "token_separators": ["-"],
        "symbols_to_index": ["#"]
    })
}

fn json_response(body: Value) -> TypesenseResponse {
    TypesenseResponse {
        status: 200,
        body: serde_json::to_vec(&body).unwrap(),
    }
}

fn export_response(documents: Vec<Value>) -> TypesenseResponse {
    TypesenseResponse {
        status: 200,
        body: export_body(documents),
    }
}

fn capture_responses() -> Vec<TypesenseResponse> {
    vec![
        json_response(collection(3)),
        export_response(page(&["prod_001", "prod_002", "prod_003"])),
        json_response(collection(3)),
    ]
}

#[path = "typesense_client_capture_metadata_tests.rs"]
mod capture_metadata_tests;

// ── Source-discovery seam (Stage 2) ─────────────────────────────────────
//
// The broader vendor-host and DNS admission matrix is owned by
// `engine/src/security_tests.rs`; these only pin the discovery-specific
// behavior that the shared matrix cannot see.

const KAT_LOOPBACK_ENDPOINT: &str = "http://127.0.0.1:17748";

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn discovery_constructor_rejects_non_vendor_host_before_resolution() {
    let resolution_attempts = Arc::new(AtomicUsize::new(0));
    let observed_attempts = Arc::clone(&resolution_attempts);
    let _resolver = install_test_outbound_host_resolver(Arc::new(move |_, _| {
        observed_attempts.fetch_add(1, Ordering::SeqCst);
        Some(vec!["8.8.8.8".parse::<IpAddr>().unwrap()])
    }));

    for endpoint in [
        "https://evil.example.com",
        "https://typesense.net.evil.example",
        KAT_LOOPBACK_ENDPOINT,
    ] {
        let error = TypesenseClient::new_discovery(endpoint, API_KEY_CANARY).unwrap_err();

        assert_eq!(error.kind(), TypesenseErrorKind::Validation);
        assert_eq!(
            error.safe_message(),
            "Typesense Cloud endpoint is not allowed"
        );
        assert_error_is_sanitized(&error);
    }

    assert_eq!(
        resolution_attempts.load(Ordering::SeqCst),
        0,
        "discovery admission must refuse a non-vendor host before any resolution"
    );
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn discovery_loopback_constructor_requires_explicit_opt_in_before_resolution() {
    let _env = crate::test_helpers::with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "");
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, _| {
        panic!("disabled discovery loopback unexpectedly resolved {host}")
    }));

    let error =
        TypesenseClient::new_discovery_preview_loopback(KAT_LOOPBACK_ENDPOINT, API_KEY_CANARY)
            .unwrap_err();

    assert_eq!(error.kind(), TypesenseErrorKind::Validation);
    assert_eq!(
        error.safe_message(),
        "Typesense preview loopback endpoint is disabled"
    );
    assert_error_is_sanitized(&error);
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn discovery_loopback_constructor_admits_only_literal_loopback_addresses() {
    let _env = crate::test_helpers::with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "1");
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, _| {
        panic!("loopback-only discovery unexpectedly resolved {host}")
    }));

    for refused in [
        "http://localhost:17748",
        "https://tenant.typesense.net",
        "http://127.0.0.1:17748/collections",
    ] {
        let error =
            TypesenseClient::new_discovery_preview_loopback(refused, API_KEY_CANARY).unwrap_err();
        assert_eq!(error.kind(), TypesenseErrorKind::Validation);
        assert_eq!(
            error.safe_message(),
            "Typesense Cloud endpoint is not allowed"
        );
    }

    let client =
        TypesenseClient::new_discovery_preview_loopback(KAT_LOOPBACK_ENDPOINT, API_KEY_CANARY)
            .expect("a literal loopback IP must be admitted under the opt-in");
    let request = client
        .build_http_request(TypesenseRequest {
            method: TypesenseMethod::Get,
            path: "/collections?exclude_fields=fields".to_string(),
            body: None,
        })
        .unwrap();
    assert_eq!(
        request.url().as_str(),
        "http://127.0.0.1:17748/collections?exclude_fields=fields"
    );
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn typesense_preview_loopback_constructor_rejects_empty_source_collection_before_endpoint_parsing()
{
    let _env = crate::test_helpers::with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "1");
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, _| {
        panic!("empty-collection preview admission unexpectedly resolved {host}")
    }));

    let error = TypesenseClient::new_preview_loopback("not a URL", API_KEY_CANARY, "")
        .expect_err("preview must require a nonempty source collection before parsing the node");

    assert_eq!(error.kind(), TypesenseErrorKind::Validation);
    assert_eq!(
        error.safe_message(),
        "Typesense credentials and source collection are required"
    );
    assert_error_is_sanitized(&error);
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn typesense_preview_loopback_constructor_retains_requested_source_collection_without_io() {
    let _env = crate::test_helpers::with_env_var(TYPESENSE_PREVIEW_LOOPBACK_ENV, "1");
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, _| {
        panic!("literal-loopback preview admission unexpectedly resolved {host}")
    }));

    let client =
        TypesenseClient::new_preview_loopback(KAT_LOOPBACK_ENDPOINT, API_KEY_CANARY, SOURCE_CANARY)
            .expect("opted-in literal loopback must be admitted for one source collection");

    assert_eq!(client.source_collection_for_test(), Some(SOURCE_CANARY));
}

#[test]
fn discovery_constructor_requires_credentials_without_requiring_a_collection() {
    let error = TypesenseClient::new_discovery("https://typesense.net", "").unwrap_err();
    assert_eq!(error.kind(), TypesenseErrorKind::Validation);
    assert_eq!(error.safe_message(), "Typesense credentials are required");

    // The export constructor keeps rejecting an empty source collection, so the
    // split cannot silently admit a collection-less export client.
    let export_error =
        TypesenseClient::new("https://typesense.net", API_KEY_CANARY, "").unwrap_err();
    assert_eq!(export_error.kind(), TypesenseErrorKind::Validation);
    assert_eq!(
        export_error.safe_message(),
        "Typesense credentials and source collection are required"
    );
}

#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn discovery_client_bypasses_proxy_pins_address_and_refuses_redirect() {
    // Discovery must inherit every `from_vetted_target` transport rule:
    // `resolve_to_addrs(...)` (reach the pinned address), `.no_proxy()` (never an
    // ambient proxy), and `.redirect(Policy::none())` (refuse, never follow).
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("discovery upstream precondition must bind");
    let redirect_target = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("redirect-target precondition must bind");
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy-listener precondition must bind");
    let upstream_address = upstream.local_addr().unwrap();
    let redirect_location = format!(
        "http://{}/collections",
        redirect_target.local_addr().unwrap()
    );
    let proxy_address = proxy.local_addr().unwrap();
    // The discovery origin is plain HTTP so the upstream can answer with a real
    // status line, which makes `HTTP_PROXY` the proxy variable under test.
    let _ambient_proxy =
        crate::test_helpers::with_env_var("HTTP_PROXY", &format!("http://{proxy_address}"));

    let responder = crate::test_helpers::serve_single_redirect(upstream, redirect_location);
    let client = TypesenseClient::for_discovery_test(
        "pinned-target.example",
        "http://pinned-target.example".to_string(),
        vec![upstream_address],
        API_KEY_CANARY,
    )
    .unwrap();

    let error = tokio::time::timeout(
        Duration::from_secs(5),
        client.list_collections(None, Some(10)),
    )
    .await
    .expect("discovery request did not complete against the pinned address")
    .expect_err("a 302 must be refused rather than followed");

    assert_eq!(error.kind(), TypesenseErrorKind::Redirect);
    assert_eq!(error.safe_message(), "Typesense redirect was refused");
    assert_eq!(
        responder.await.expect("redirect responder panicked"),
        "GET /collections?exclude_fields=fields&limit=10 HTTP/1.1",
        "discovery must reach the pinned address with the enumeration request line"
    );
    // Redirect refusal is proven deterministically by the `Redirect` error kind
    // and safe message above; the pinned upstream request line proves the request
    // reached the vetted address rather than following the 302. A port-based
    // "no connection ever reached the redirect target" probe added nothing but
    // nondeterminism (it spuriously failed the broad suite while passing in
    // isolation), so it is intentionally omitted.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), proxy.accept())
            .await
            .is_err(),
        "ambient proxy unexpectedly received the discovery request"
    );
    assert_error_is_sanitized(&error);
}

// ── Export throughput probe (Stage 2) ───────────────────────────────────
//
// A timed unit-throughput measurement in this lane's real execution locality.
// The item-ceiling projection measures parser and identity CPU only: the
// independent response-byte ceiling can bind first for a representative export
// body, and the probe reports that boundary separately. It drives the
// production export parser and the canonical provider-neutral identity path,
// so it cannot pass on a no-op.

const THROUGHPUT_PROBE_DOCUMENTS: usize = 10_000;
const THROUGHPUT_BUDGET_SECONDS: usize = 1_200;

#[derive(Debug, PartialEq, Eq)]
struct ThroughputMeasurement {
    elapsed_ms: usize,
    documents_per_second: usize,
    projected_worst_case_seconds: usize,
}

fn throughput_measurement(
    documents: usize,
    measured_elapsed_ms: usize,
    traversal_limit: usize,
) -> ThroughputMeasurement {
    let documents_per_second = documents * 1000 / measured_elapsed_ms;
    ThroughputMeasurement {
        elapsed_ms: measured_elapsed_ms,
        documents_per_second,
        projected_worst_case_seconds: traversal_limit.div_ceil(documents_per_second),
    }
}

fn representative_byte_ceiling_items(
    documents: usize,
    body_bytes: usize,
    response_byte_limit: usize,
) -> usize {
    assert!(
        documents > 0,
        "the byte projection needs a document specimen"
    );
    assert!(body_bytes > 0, "the byte projection needs a body specimen");
    response_byte_limit / body_bytes.div_ceil(documents)
}

#[test]
fn throughput_measurement_reports_observed_elapsed_time() {
    assert_eq!(
        throughput_measurement(10_000, 125, 1_000_000),
        ThroughputMeasurement {
            elapsed_ms: 125,
            documents_per_second: 80_000,
            projected_worst_case_seconds: 13,
        }
    );
    assert_eq!(
        representative_byte_ceiling_items(10_000, 1_000_000, MAX_RESPONSE_BYTES),
        83_886,
        "the byte projection uses the conservative ceiling of specimen bytes per document"
    );
}

fn assert_and_report_throughput_evidence(
    measurement: &ThroughputMeasurement,
    traversal_limit: usize,
    body_bytes: usize,
) {
    let representative_items = representative_byte_ceiling_items(
        THROUGHPUT_PROBE_DOCUMENTS,
        body_bytes,
        MAX_RESPONSE_BYTES,
    );
    assert!(measurement.documents_per_second > 0);
    assert_eq!(
        measurement.documents_per_second,
        THROUGHPUT_PROBE_DOCUMENTS * 1000 / measurement.elapsed_ms,
        "the receipt figures must satisfy the validator's integer arithmetic"
    );
    assert!(
        measurement.projected_worst_case_seconds <= THROUGHPUT_BUDGET_SECONDS,
        "projected parser and identity work for {traversal_limit} documents takes \
         {}s, over the {THROUGHPUT_BUDGET_SECONDS}s stage budget; \
         re-plan the traversal instead of widening the budget",
        measurement.projected_worst_case_seconds
    );
    assert!(
        representative_items < traversal_limit,
        "the receipt must not misstate the item-ceiling CPU projection as an end-to-end export ceiling"
    );
    println!(
        "TYPESENSE_EXPORT_THROUGHPUT documents={THROUGHPUT_PROBE_DOCUMENTS} \
elapsed_ms={} docs_per_second={} \
traversal_limit={traversal_limit} \
projected_worst_case_seconds={} \
budget_seconds={THROUGHPUT_BUDGET_SECONDS} locality=lane_worktree",
        measurement.elapsed_ms,
        measurement.documents_per_second,
        measurement.projected_worst_case_seconds,
    );
    println!(
        "TYPESENSE_EXPORT_BYTE_CEILING documents={THROUGHPUT_PROBE_DOCUMENTS} \
body_bytes={body_bytes} response_byte_limit={MAX_RESPONSE_BYTES} \
representative_byte_ceiling_items={representative_items} \
item_ceiling={traversal_limit}"
    );
}

fn throughput_probe_documents() -> Vec<Value> {
    (0..THROUGHPUT_PROBE_DOCUMENTS)
        .map(|index| {
            json!({
                "id": format!("prod_{index:06}"),
                "title": format!("Probe product {index}"),
                "sku": format!("SKU-{index:06}"),
                "price": index,
            })
        })
        .collect()
}

#[tokio::test]
#[ignore = "timed throughput probe recorded in the Stage 2 evidence receipt"]
async fn typesense_export_identity_throughput_probe() {
    let documents = throughput_probe_documents();
    let body = export_body(documents.clone());
    let body_bytes = body.len();
    assert!(
        body_bytes < MAX_RESPONSE_BYTES,
        "the probe stream must fit under the unchanged byte ceiling, not widen it"
    );
    let traversal_limit = TraversalLimits::default().max_items;
    let certified_max_items =
        u64::try_from(traversal_limit).expect("traversal ceiling must fit identity arithmetic");
    let spool_root = TempDir::new().expect("identity spool root should be created");
    let identity_config =
        SourceIdentityConfig::for_test(spool_root.path(), 1 << 20, certified_max_items);
    assert_eq!(
        identity_config.certified_max_items, certified_max_items,
        "the measured identity partition layout must certify the printed traversal ceiling"
    );
    let mut builder =
        SourceSnapshotBuilder::new(identity_config).expect("snapshot builder should be created");
    builder.record_settings(&json!({ "fields": [] }));
    let mut transport = ScriptedTransport {
        responses: VecDeque::from([
            json_response(collection(THROUGHPUT_PROBE_DOCUMENTS)),
            TypesenseResponse { status: 200, body },
            json_response(collection(THROUGHPUT_PROBE_DOCUMENTS)),
        ]),
        requests: Vec::new(),
    };
    let mut page_index = 0usize;

    let started = Instant::now();
    capture_source_with_transport(&mut transport, "catalog", |page| {
        let records = typesense_document_records(&page, "$.documents")
            .expect("probe documents must carry the canonical Typesense stable id");
        builder
            .record_documents_page(page_index, &source_record_identity_page(&records))
            .expect("probe documents must record into the canonical source identity");
        page_index += 1;
        Ok::<_, TypesenseClientError>(())
    })
    .await
    .expect("the probe stream must capture through the production export parser");
    let snapshot = builder
        .finish()
        .expect("the canonical source identity must close");
    let measured_elapsed_ms = usize::try_from(started.elapsed().as_millis())
        .expect("probe elapsed milliseconds must fit a usize");

    assert_eq!(
        snapshot.documents.count, THROUGHPUT_PROBE_DOCUMENTS,
        "every streamed document must reach the canonical identity path"
    );
    assert_eq!(transport.requests.len(), 3);
    assert!(
        measured_elapsed_ms > 0,
        "the probe must take measurable wall-clock time"
    );

    // Read the ceiling from its single owner, and project it exactly as the
    // receipt validator does: `(traversal_limit + rate - 1) / rate`.
    let measurement = throughput_measurement(
        THROUGHPUT_PROBE_DOCUMENTS,
        measured_elapsed_ms,
        traversal_limit,
    );

    assert_and_report_throughput_evidence(&measurement, traversal_limit, body_bytes);
}
