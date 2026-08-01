use super::meilisearch_client::{
    capture_source_with_transport, decode_document_page, encoded_index_uid,
    fetch_document_pages_with_transport, require_read_access_with_transport, MeilisearchClient,
    MeilisearchClientError, MeilisearchErrorKind, MeilisearchMethod, MeilisearchRequest,
    MeilisearchResponse, MeilisearchTransport, TraversalLimits, CONNECT_TIMEOUT,
    MAX_RESPONSE_BYTES, REQUEST_TIMEOUT,
};
use flapjack::security::test_helpers::install_test_outbound_host_resolver;
use serde_json::json;
use std::collections::VecDeque;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

const RAW_ENDPOINT_CANARY: &str = "https://tenant-secret.meilisearch.example";
const API_KEY_CANARY: &str = "meili-secret-api-key";

fn assert_error_is_sanitized(error: &MeilisearchClientError) {
    let debug = format!("{error:?}");
    let serialized = serde_json::to_string(error).unwrap();
    for canary in [RAW_ENDPOINT_CANARY, API_KEY_CANARY] {
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
fn production_constructor_accepts_documented_meilisearch_cloud_hostnames() {
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, port| {
        assert_eq!(port, Some(443));
        assert!(
            host == "meilisearch.io" || host.ends_with(".meilisearch.io"),
            "unexpected host admitted to resolver: {host}"
        );
        Some(vec!["8.8.8.8".parse::<IpAddr>().unwrap()])
    }));

    for endpoint in [
        "https://meilisearch.io",
        "https://your-instance.meilisearch.io",
        "https://tenant.region.meilisearch.io",
    ] {
        MeilisearchClient::new(endpoint, API_KEY_CANARY, "products")
            .expect("documented Meilisearch Cloud hostname should be admitted");
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
        "https://evilmeilisearch.io",
        "https://meilisearch.io.evil.test",
        "https://meilisearch.com",
        "https://tenant.meilisearch.com",
        "https://127.0.0.1",
        "http://localhost:7700",
    ] {
        let error = MeilisearchClient::new(endpoint, API_KEY_CANARY, "products").unwrap_err();
        assert_eq!(error.kind(), MeilisearchErrorKind::Validation);
        assert_eq!(
            error.safe_message(),
            "Meilisearch Cloud endpoint is not allowed"
        );
        assert_error_is_sanitized(&error);
    }
}

#[test]
fn client_debug_scrubs_endpoint_key_and_index_identity() {
    let client = MeilisearchClient::for_test(
        "tenant-secret.meilisearch.example",
        vec!["8.8.8.8:443".parse().unwrap()],
        API_KEY_CANARY,
        "private-index-name",
    )
    .unwrap();
    let debug = format!("{client:?}");
    for canary in [
        "tenant-secret.meilisearch.example",
        API_KEY_CANARY,
        "private-index-name",
        "8.8.8.8",
    ] {
        assert!(!debug.contains(canary), "client Debug leaked {canary}");
    }
}

#[test]
fn request_builder_keeps_bearer_credential_out_of_url_and_diagnostics() {
    let client = MeilisearchClient::for_test(
        "tenant-secret.meilisearch.example",
        vec!["8.8.8.8:443".parse().unwrap()],
        API_KEY_CANARY,
        "catalog/2026",
    )
    .unwrap();
    let request = client
        .build_http_request(MeilisearchRequest {
            method: MeilisearchMethod::Get,
            path: "/indexes/catalog%2F2026/settings".to_string(),
            body: None,
        })
        .unwrap();

    assert_eq!(
        request.url().as_str(),
        "https://tenant-secret.meilisearch.example/indexes/catalog%2F2026/settings"
    );
    assert!(!request.url().as_str().contains(API_KEY_CANARY));
    assert_eq!(
        request.headers()["authorization"].to_str().unwrap(),
        format!("Bearer {API_KEY_CANARY}")
    );
}

#[test]
fn client_transport_time_budgets_are_bounded() {
    assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(5));
    assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(30));
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
    let client = MeilisearchClient::for_test(
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
    assert_eq!(error.kind(), MeilisearchErrorKind::Transport);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), proxy_listener.accept())
            .await
            .is_err(),
        "ambient HTTPS proxy unexpectedly received the request"
    );
    assert_error_is_sanitized(&error);
}

#[test]
fn index_uid_is_percent_encoded_as_one_path_segment() {
    assert_eq!(
        encoded_index_uid("catalog/../../private?x=1"),
        "catalog%2F..%2F..%2Fprivate%3Fx%3D1"
    );
    assert_eq!(encoded_index_uid("books and tools"), "books%20and%20tools");
}

#[test]
fn document_page_decoding_preserves_declared_pagination() {
    let page = decode_document_page(
        serde_json::to_vec(&json!({
            "results": [{"sku": "SKU-001"}, {"sku": "SKU-002"}],
            "offset": 0,
            "limit": 2,
            "total": 3
        }))
        .unwrap()
        .as_slice(),
    )
    .unwrap();

    assert_eq!(page.offset, 0);
    assert_eq!(page.limit, 2);
    assert_eq!(page.total, 3);
    assert_eq!(page.results.len(), 2);
}

#[test]
fn document_page_decoding_rejects_malformed_and_over_budget_responses() {
    for malformed in [
        json!({"results": [], "offset": 0, "limit": 0, "total": 1}),
        json!({"results": [{}], "offset": 2, "limit": 2, "total": 1}),
        json!({"results": "not-an-array", "offset": 0, "limit": 2, "total": 1}),
        json!({"results": [], "offset": 0, "limit": 2}),
    ] {
        let error = decode_document_page(&serde_json::to_vec(&malformed).unwrap()).unwrap_err();
        assert_eq!(error.kind(), MeilisearchErrorKind::Progress);
        assert_error_is_sanitized(&error);
    }

    let oversized = vec![b' '; MAX_RESPONSE_BYTES + 1];
    let error = decode_document_page(&oversized).unwrap_err();
    assert_eq!(error.kind(), MeilisearchErrorKind::Limit);
    assert_error_is_sanitized(&error);
}

#[derive(Default)]
struct ScriptedTransport {
    responses: VecDeque<MeilisearchResponse>,
    requests: Vec<MeilisearchRequest>,
}

impl ScriptedTransport {
    fn with_json_responses(responses: impl IntoIterator<Item = serde_json::Value>) -> Self {
        Self {
            responses: responses
                .into_iter()
                .map(|body| MeilisearchResponse {
                    status: 200,
                    body: serde_json::to_vec(&body).unwrap(),
                })
                .collect(),
            requests: Vec::new(),
        }
    }
}

impl MeilisearchTransport for ScriptedTransport {
    fn send<'a>(
        &'a mut self,
        request: MeilisearchRequest,
    ) -> Pin<
        Box<dyn Future<Output = Result<MeilisearchResponse, MeilisearchClientError>> + Send + 'a>,
    > {
        self.requests.push(request);
        Box::pin(async move {
            self.responses.pop_front().ok_or_else(|| {
                MeilisearchClientError::new(
                    MeilisearchErrorKind::Transport,
                    "Meilisearch transport failed",
                )
            })
        })
    }
}

fn page(offset: usize, total: usize, ids: &[&str]) -> serde_json::Value {
    json!({
        "results": ids.iter().map(|id| json!({"sku": id})).collect::<Vec<_>>(),
        "offset": offset,
        "limit": 2,
        "total": total
    })
}

#[tokio::test]
async fn document_traversal_follows_declared_offset_limit_and_total() {
    let mut transport = ScriptedTransport::with_json_responses([
        page(0, 3, &["SKU-001", "SKU-002"]),
        page(2, 3, &["SKU-003"]),
    ]);
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
                    .map(|document| document["sku"].as_str().unwrap().to_string()),
            );
            Ok::<_, MeilisearchClientError>(())
        },
    )
    .await
    .unwrap();

    assert_eq!(page_counts, vec![2, 1]);
    assert_eq!(ids, vec!["SKU-001", "SKU-002", "SKU-003"]);
    assert_eq!(transport.requests.len(), 2);
    for (request, expected_offset) in transport.requests.iter().zip([0, 2]) {
        assert_eq!(request.method, MeilisearchMethod::Post);
        assert_eq!(request.path, "/indexes/catalog%2F2026/documents/fetch");
        assert_eq!(
            request.body,
            Some(json!({"offset": expected_offset, "limit": 2}))
        );
    }
}

#[tokio::test]
async fn document_traversal_rejects_repeated_truncated_inconsistent_and_over_budget_pages() {
    let specimens = [
        vec![
            page(0, 3, &["SKU-001", "SKU-002"]),
            page(0, 3, &["SKU-001"]),
        ],
        vec![page(0, 3, &["SKU-001", "SKU-002"]), page(2, 3, &[])],
        vec![
            page(0, 3, &["SKU-001", "SKU-002"]),
            page(2, 4, &["SKU-003"]),
        ],
    ];

    for responses in specimens {
        let mut transport = ScriptedTransport::with_json_responses(responses);
        let error = fetch_document_pages_with_transport(
            &mut transport,
            "catalog",
            TraversalLimits::default(),
            |_| Ok::<_, MeilisearchClientError>(()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), MeilisearchErrorKind::Progress);
        assert_error_is_sanitized(&error);
    }

    let mut transport =
        ScriptedTransport::with_json_responses([page(0, 3, &["SKU-001", "SKU-002"])]);
    let error = fetch_document_pages_with_transport(
        &mut transport,
        "catalog",
        TraversalLimits {
            max_pages: 1,
            max_items: 1,
        },
        |_| Ok::<_, MeilisearchClientError>(()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), MeilisearchErrorKind::Limit);
}

#[tokio::test]
async fn document_traversal_refuses_redirects_without_following_them() {
    let mut transport = ScriptedTransport {
        responses: VecDeque::from([MeilisearchResponse {
            status: 302,
            body: Vec::new(),
        }]),
        requests: Vec::new(),
    };
    let error = fetch_document_pages_with_transport(
        &mut transport,
        "catalog",
        TraversalLimits::default(),
        |_| Ok::<_, MeilisearchClientError>(()),
    )
    .await
    .unwrap_err();

    assert_eq!(error.kind(), MeilisearchErrorKind::Redirect);
    assert_eq!(transport.requests.len(), 1);
}

#[tokio::test]
async fn restricted_credentials_fail_before_document_access_is_accepted() {
    let mut transport = ScriptedTransport::with_json_responses([
        metadata(Some("sku"), "2026-07-26T00:00:01Z"),
        stats(3, false),
        tasks("succeeded", 1),
        json!({
            "commitSha": "2ecfd54",
            "commitDate": "unknown",
            "pkgVersion": "1.50.0"
        }),
        json!({"searchableAttributes": ["title"]}),
    ]);
    transport.responses.push_back(MeilisearchResponse {
        status: 403,
        body: Vec::new(),
    });

    let error = require_read_access_with_transport(&mut transport, "catalog")
        .await
        .unwrap_err();

    assert_eq!(error.kind(), MeilisearchErrorKind::Upstream);
    assert_eq!(
        error.safe_message(),
        "Meilisearch source credentials lack required read access"
    );
    assert_eq!(transport.requests.len(), 6);
    assert_error_is_sanitized(&error);
}

fn metadata(primary_key: Option<&str>, updated_at: &str) -> serde_json::Value {
    json!({
        "uid": "catalog",
        "primaryKey": primary_key,
        "createdAt": "2026-07-26T00:00:00Z",
        "updatedAt": updated_at
    })
}

fn stats(count: usize, is_indexing: bool) -> serde_json::Value {
    json!({
        "numberOfDocuments": count,
        "isIndexing": is_indexing,
        "fieldDistribution": {"sku": count, "title": count}
    })
}

fn tasks(status: &str, total: usize) -> serde_json::Value {
    json!({
        "results": [{"uid": 7, "indexUid": "catalog", "status": status}],
        "total": total,
        "limit": 1000,
        "from": null,
        "next": null
    })
}

fn quiescent_capture_responses() -> Vec<serde_json::Value> {
    vec![
        metadata(Some("sku"), "2026-07-26T00:00:01Z"),
        stats(3, false),
        tasks("succeeded", 1),
        json!({
            "commitSha": "2ecfd54",
            "commitDate": "unknown",
            "pkgVersion": "1.50.0"
        }),
        json!({
            "searchableAttributes": ["title"],
            "synonyms": {"wrench": ["spanner"]}
        }),
        page(0, 3, &["SKU-001", "SKU-002"]),
        page(2, 3, &["SKU-003"]),
        tasks("succeeded", 1),
        stats(3, false),
        metadata(Some("sku"), "2026-07-26T00:00:01Z"),
    ]
}

#[tokio::test]
async fn source_capture_reads_proved_endpoints_and_requires_stable_quiescence() {
    let mut transport = ScriptedTransport::with_json_responses(quiescent_capture_responses());
    let mut ids = Vec::new();
    let capture = capture_source_with_transport(&mut transport, "catalog", |documents| {
        ids.extend(
            documents
                .iter()
                .map(|document| document["sku"].as_str().unwrap().to_string()),
        );
        Ok::<_, MeilisearchClientError>(())
    })
    .await
    .unwrap();

    assert_eq!(capture.metadata.uid, "catalog");
    assert_eq!(capture.metadata.primary_key, "sku");
    assert_eq!(capture.stats.number_of_documents, 3);
    assert_eq!(capture.version.package_version, "1.50.0");
    assert_eq!(capture.settings["synonyms"]["wrench"][0], "spanner");
    assert_eq!(ids, vec!["SKU-001", "SKU-002", "SKU-003"]);
    assert_eq!(
        transport
            .requests
            .iter()
            .map(|request| (request.method, request.path.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (MeilisearchMethod::Get, "/indexes/catalog"),
            (MeilisearchMethod::Get, "/indexes/catalog/stats"),
            (
                MeilisearchMethod::Get,
                "/tasks?indexUids=catalog&limit=1000"
            ),
            (MeilisearchMethod::Get, "/version"),
            (MeilisearchMethod::Get, "/indexes/catalog/settings"),
            (MeilisearchMethod::Post, "/indexes/catalog/documents/fetch"),
            (MeilisearchMethod::Post, "/indexes/catalog/documents/fetch"),
            (
                MeilisearchMethod::Get,
                "/tasks?indexUids=catalog&limit=1000"
            ),
            (MeilisearchMethod::Get, "/indexes/catalog/stats"),
            (MeilisearchMethod::Get, "/indexes/catalog"),
        ]
    );
}

#[tokio::test]
async fn source_capture_rejects_missing_primary_key_nonterminal_tasks_and_source_drift() {
    let mut missing_primary_key = quiescent_capture_responses();
    missing_primary_key[0] = metadata(None, "2026-07-26T00:00:01Z");
    let mut nonterminal = quiescent_capture_responses();
    nonterminal[2] = tasks("processing", 1);
    let mut metadata_drift = quiescent_capture_responses();
    metadata_drift[9] = metadata(Some("sku"), "2026-07-26T00:00:02Z");
    let mut stats_drift = quiescent_capture_responses();
    stats_drift[8] = stats(4, false);
    let mut truncated_tasks = quiescent_capture_responses();
    truncated_tasks[2] = tasks("succeeded", 2);

    for responses in [
        missing_primary_key,
        nonterminal,
        metadata_drift,
        stats_drift,
        truncated_tasks,
    ] {
        let mut transport = ScriptedTransport::with_json_responses(responses);
        let error = capture_source_with_transport(&mut transport, "catalog", |_| {
            Ok::<_, MeilisearchClientError>(())
        })
        .await
        .unwrap_err();
        assert!(
            matches!(
                error.kind(),
                MeilisearchErrorKind::Schema | MeilisearchErrorKind::Progress
            ),
            "unexpected quiescence error: {error:?}"
        );
        assert_error_is_sanitized(&error);
    }
}
