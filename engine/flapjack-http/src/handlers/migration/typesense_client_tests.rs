use super::typesense_client::{
    capture_source_with_transport, decode_document_page, encoded_collection_name,
    fetch_document_pages_with_expected_count_for_test, fetch_document_pages_with_transport,
    observe_source_with_transport, page_exceeds_traversal_budget, read_settings_with_transport,
    require_read_access_with_transport, TraversalLimits, TypesenseClient, TypesenseClientError,
    TypesenseErrorKind, TypesenseMethod, TypesenseRequest, TypesenseResponse, TypesenseTransport,
    CONNECT_TIMEOUT, DOCUMENT_PAGE_LIMIT, MAX_DOCUMENT_ITEMS, MAX_DOCUMENT_PAGES,
    MAX_RESPONSE_BYTES, REQUEST_TIMEOUT,
};
use flapjack::security::test_helpers::install_test_outbound_host_resolver;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::future::Future;
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

const RAW_ENDPOINT_CANARY: &str = "https://tenant-secret.typesense.example";
const API_KEY_CANARY: &str = "typesense-secret-api-key";
const SOURCE_CANARY: &str = "private-typesense-collection";

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
fn default_document_traversal_budget_reaches_advertised_item_ceiling() {
    let reachable_items = DOCUMENT_PAGE_LIMIT
        .checked_mul(MAX_DOCUMENT_PAGES)
        .expect("document traversal budget multiplication should not overflow");
    assert!(
        reachable_items >= MAX_DOCUMENT_ITEMS,
        "default traversal can only reach {reachable_items} items before page limit"
    );
    assert!(
        !page_exceeds_traversal_budget(
            MAX_DOCUMENT_PAGES + 1,
            TraversalLimits::default(),
            Some(MAX_DOCUMENT_ITEMS),
            MAX_DOCUMENT_ITEMS,
        ),
        "counted traversal must reserve an empty completion probe at the advertised ceiling"
    );
    assert!(
        page_exceeds_traversal_budget(
            MAX_DOCUMENT_PAGES + 2,
            TraversalLimits::default(),
            Some(MAX_DOCUMENT_ITEMS),
            MAX_DOCUMENT_ITEMS,
        ),
        "counted traversal must reserve only one completion probe"
    );
}

#[tokio::test]
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
fn document_page_decoding_preserves_declared_pagination() {
    let page = decode_document_page(
        br#"{"id":"prod_001"}
{"id":"prod_002"}
"#,
    )
    .unwrap();

    assert_eq!(page.page, 1);
    assert_eq!(page.found, 2);
    assert_eq!(
        page.documents
            .iter()
            .map(|document| document["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["prod_001", "prod_002"]
    );
}

#[test]
fn document_export_decoding_reads_json_values_without_search_envelope() {
    let page = decode_document_page(
        br#"{"id":"prod_001","title":"Alpha"}
{"id":"prod_002","title":"Beta"}
"#,
    )
    .unwrap();

    assert_eq!(page.page, 1);
    assert_eq!(page.found, 2);
    assert_eq!(
        page.documents
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

#[derive(Default)]
struct UnderreportedExportTransport {
    requests: Vec<TypesenseRequest>,
}

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
                "/collections/catalog" => Ok(json_response(collection(DOCUMENT_PAGE_LIMIT))),
                "/collections/catalog/documents/export?page=1&per_page=100" => {
                    Ok(export_response(numbered_page(0, DOCUMENT_PAGE_LIMIT)))
                }
                "/collections/catalog/documents/export?page=2&per_page=100" => Ok(export_response(
                    numbered_page(DOCUMENT_PAGE_LIMIT, DOCUMENT_PAGE_LIMIT),
                )),
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

fn export_body(documents: Vec<Value>) -> Vec<u8> {
    let mut body = Vec::new();
    for document in documents {
        body.extend_from_slice(&serde_json::to_vec(&document).unwrap());
        body.push(b'\n');
    }
    body
}

fn expected_product_ids() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json");
    let bundle: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    bundle["source"]["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|collection| collection["name"] == "fj_ts_migration_products")
        .unwrap()["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|document| document["id"].as_str().unwrap().to_string())
        .collect()
}

fn fixture_traversal_limits() -> TraversalLimits {
    TraversalLimits {
        page_size: 2,
        ..TraversalLimits::default()
    }
}

#[tokio::test]
async fn document_traversal_follows_declared_pages_and_fixture_ids() {
    let expected_ids = expected_product_ids();
    let mut transport = ScriptedTransport::with_export_responses([
        page(&[&expected_ids[0], &expected_ids[1]]),
        page(&[&expected_ids[2]]),
    ]);
    let mut page_counts = Vec::new();
    let mut ids = Vec::new();

    fetch_document_pages_with_transport(
        &mut transport,
        "catalog/2026",
        fixture_traversal_limits(),
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

    assert_eq!(page_counts, vec![2, 1]);
    assert_eq!(ids, expected_ids);
    assert_eq!(transport.requests.len(), 2);
    for (request, expected_page) in transport.requests.iter().zip([1, 2]) {
        assert_eq!(request.method, TypesenseMethod::Get);
        assert_eq!(
            request.path,
            format!(
                "/collections/{}/documents/export?page={expected_page}&per_page=2",
                encoded_collection_name("catalog/2026")
            )
        );
        assert_eq!(request.body, None);
    }
}

#[tokio::test]
async fn document_traversal_rejects_repeated_truncated_inconsistent_and_over_budget_pages() {
    let specimens = [
        vec![page(&["prod_001", "prod_002"]), page(&[])],
        vec![page(&["prod_001", "prod_002", "prod_003"])],
    ];

    for responses in specimens {
        let mut transport = ScriptedTransport::with_export_responses(responses);
        let error = fetch_document_pages_with_transport(
            &mut transport,
            "catalog",
            fixture_traversal_limits(),
            |_| Ok::<_, TypesenseClientError>(()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), TypesenseErrorKind::Progress);
        assert_error_is_sanitized(&error);
    }

    let mut transport = ScriptedTransport::with_export_responses([page(&["prod_001", "prod_002"])]);
    let error = fetch_document_pages_with_transport(
        &mut transport,
        "catalog",
        TraversalLimits {
            max_pages: 1,
            max_items: 1,
            page_size: 2,
        },
        |_| Ok::<_, TypesenseClientError>(()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), TypesenseErrorKind::Limit);
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
                "/collections/catalog/documents/export?page=1&per_page=100"
            ),
            (TypesenseMethod::Get, "/collections/catalog"),
        ]
    );
}

#[tokio::test]
async fn source_capture_accepts_exact_multiple_after_empty_completion_probe() {
    let mut transport = ScriptedTransport {
        responses: VecDeque::from([
            json_response(collection(2 * DOCUMENT_PAGE_LIMIT)),
            export_response(numbered_page(0, DOCUMENT_PAGE_LIMIT)),
            export_response(numbered_page(DOCUMENT_PAGE_LIMIT, DOCUMENT_PAGE_LIMIT)),
            export_response(Vec::new()),
            json_response(collection(2 * DOCUMENT_PAGE_LIMIT)),
        ]),
        requests: Vec::new(),
    };
    let mut consumed_ids = Vec::new();

    let capture = capture_source_with_transport(&mut transport, "catalog", |documents| {
        consumed_ids.extend(
            documents
                .iter()
                .map(|document| document["id"].as_str().unwrap().to_string()),
        );
        Ok::<_, TypesenseClientError>(())
    })
    .await
    .expect("an exact multiple of the export page size should accept an empty completion probe");

    assert_eq!(
        capture.observation().document_count,
        (2 * DOCUMENT_PAGE_LIMIT) as u64
    );
    assert_eq!(consumed_ids.len(), 2 * DOCUMENT_PAGE_LIMIT);
    assert_eq!(consumed_ids.first().unwrap(), "prod_000");
    assert_eq!(consumed_ids.last().unwrap(), "prod_199");
    assert_eq!(
        transport
            .requests
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/collections/catalog",
            "/collections/catalog/documents/export?page=1&per_page=100",
            "/collections/catalog/documents/export?page=2&per_page=100",
            "/collections/catalog/documents/export?page=3&per_page=100",
            "/collections/catalog",
        ]
    );
}

#[tokio::test]
async fn counted_export_reserves_page_budget_for_empty_completion_probe() {
    let mut transport = ScriptedTransport::with_export_responses([
        numbered_page(0, DOCUMENT_PAGE_LIMIT),
        numbered_page(DOCUMENT_PAGE_LIMIT, DOCUMENT_PAGE_LIMIT),
        Vec::new(),
    ]);
    let limits = TraversalLimits {
        max_pages: 2,
        max_items: 2 * DOCUMENT_PAGE_LIMIT,
        page_size: DOCUMENT_PAGE_LIMIT,
    };
    let mut consumed_ids = Vec::new();

    fetch_document_pages_with_expected_count_for_test(
        &mut transport,
        "catalog",
        limits,
        2 * DOCUMENT_PAGE_LIMIT,
        |documents| {
            consumed_ids.extend(
                documents
                    .iter()
                    .map(|document| document["id"].as_str().unwrap().to_string()),
            );
            Ok::<_, TypesenseClientError>(())
        },
    )
    .await
    .expect("counted traversal should reserve one request for the empty completion probe");

    assert_eq!(consumed_ids.len(), 2 * DOCUMENT_PAGE_LIMIT);
    assert_eq!(
        transport
            .requests
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/collections/catalog/documents/export?page=1&per_page=100",
            "/collections/catalog/documents/export?page=2&per_page=100",
            "/collections/catalog/documents/export?page=3&per_page=100",
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
    assert_eq!(consumed_ids.len(), DOCUMENT_PAGE_LIMIT);
    assert_eq!(
        transport
            .requests
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/collections/catalog",
            "/collections/catalog/documents/export?page=1&per_page=100",
            "/collections/catalog/documents/export?page=2&per_page=100",
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
