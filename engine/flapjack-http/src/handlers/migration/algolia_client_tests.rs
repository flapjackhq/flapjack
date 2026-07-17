use super::*;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

#[derive(Debug, Clone)]
struct ScriptedTransport {
    responses: VecDeque<Result<RawResponse, AlgoliaClientError>>,
    requests: Vec<PlannedRequest>,
}

impl ScriptedTransport {
    fn new(responses: Vec<Result<RawResponse, AlgoliaClientError>>) -> Self {
        Self {
            responses: responses.into(),
            requests: Vec::new(),
        }
    }
}

impl AlgoliaTransport for ScriptedTransport {
    fn send<'a>(
        &'a mut self,
        request: PlannedRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RawResponse, AlgoliaClientError>> + Send + 'a>> {
        self.requests.push(request);
        let response = self.responses.pop_front().unwrap_or_else(|| {
            Err(AlgoliaClientError::new(
                AlgoliaErrorKind::Transport,
                "scripted response missing",
            ))
        });
        Box::pin(async move { response })
    }
}

fn ok(body: Value) -> Result<RawResponse, AlgoliaClientError> {
    Ok(RawResponse {
        status: 200,
        body: serde_json::to_vec(&body).unwrap(),
    })
}

fn status(status: u16) -> Result<RawResponse, AlgoliaClientError> {
    Ok(RawResponse {
        status,
        body: br#"{"message":"hidden"}"#.to_vec(),
    })
}

fn request_urls(transport: &ScriptedTransport) -> Vec<&str> {
    transport
        .requests
        .iter()
        .map(|request| request.url.as_str())
        .collect()
}

fn request_for_test(
    app_id: &str,
    index_name: &str,
    method: AlgoliaMethod,
    suffix: &str,
) -> Result<PlannedRequest, AlgoliaClientError> {
    plan_request(app_id, "key", method, index_path(index_name, suffix), None)
}

fn scripted_json_for_test(
    transport: &mut ScriptedTransport,
    app_id: &str,
    index_name: &str,
    method: AlgoliaMethod,
    suffix: &str,
) -> Result<Value, AlgoliaClientError> {
    let request = request_for_test(app_id, index_name, method, suffix)?;
    tokio_test::block_on(execute_json_with_retry(transport, request))
}

fn list_indexes_for_test(
    transport: &mut ScriptedTransport,
) -> Result<Vec<AlgoliaIndexRecord>, AlgoliaClientError> {
    tokio_test::block_on(list_indexes_with_transport(transport, "APP123", "key"))
}

fn list_indexes_with_limits_for_test(
    transport: &mut ScriptedTransport,
    limits: TraversalLimits,
) -> Result<Vec<AlgoliaIndexRecord>, AlgoliaClientError> {
    tokio_test::block_on(list_indexes_with_transport_and_limits(
        transport, "APP123", "key", limits,
    ))
}

fn key_allows_unretrievable_for_test(
    transport: &mut ScriptedTransport,
) -> Result<bool, AlgoliaClientError> {
    tokio_test::block_on(key_allows_unretrievable_with_transport(
        transport, "APP123", "key",
    ))
}

fn require_unretrievable_access_for_test(
    transport: &mut ScriptedTransport,
    settings: &Value,
) -> Result<(), AlgoliaClientError> {
    tokio_test::block_on(require_unretrievable_access_with_transport(
        transport, "APP123", "key", settings,
    ))
}

fn wait_for_quiescent_source_for_test(
    transport: &mut ScriptedTransport,
) -> Result<AlgoliaIndexRecord, AlgoliaClientError> {
    tokio_test::block_on(wait_for_quiescent_source_with_transport(
        transport,
        "APP123",
        "key",
        "products",
        QuiescencePolicy {
            max_polls: 3,
            poll_interval: Duration::from_millis(1),
        },
        |_| async {},
    ))
}

fn index_page(items: Value, page: usize, nb_pages: usize) -> Value {
    json!({
        "items": items,
        "page": page,
        "nbPages": nb_pages
    })
}

fn paginated_hits_for_test<T: DeserializeOwned>(
    transport: &mut ScriptedTransport,
    endpoint: &str,
) -> Result<Vec<T>, AlgoliaClientError> {
    let raw = paginated_raw_hits_for_test(transport, endpoint)?;
    raw.into_iter()
        .map(|hit| {
            serde_json::from_value(hit).map_err(|_| {
                AlgoliaClientError::new(
                    AlgoliaErrorKind::Schema,
                    "Algolia hit did not match the expected schema",
                )
            })
        })
        .collect()
}

fn paginated_raw_hits_for_test(
    transport: &mut ScriptedTransport,
    endpoint: &str,
) -> Result<Vec<Value>, AlgoliaClientError> {
    let mut delivered = Vec::new();
    let result = tokio_test::block_on(paginated_hits_with_transport(
        transport,
        "APP123",
        "key",
        "products",
        endpoint,
        |page| {
            delivered.extend(page);
            Ok::<_, Infallible>(())
        },
    ));
    match result {
        Ok(()) => Ok(delivered),
        Err(BrowseError::Client(error)) => Err(error),
        Err(BrowseError::Consumer(never)) => match never {},
    }
}

fn paginated_raw_hits_with_limits_for_test(
    transport: &mut ScriptedTransport,
    endpoint: &str,
    limits: TraversalLimits,
) -> Result<Vec<Value>, AlgoliaClientError> {
    let mut delivered = Vec::new();
    let result = tokio_test::block_on(paginated_hits_with_transport_and_limits(
        transport,
        "APP123",
        "key",
        "products",
        endpoint,
        limits,
        |page| {
            delivered.extend(page);
            Ok::<_, Infallible>(())
        },
    ));
    match result {
        Ok(()) => Ok(delivered),
        Err(BrowseError::Client(error)) => Err(error),
        Err(BrowseError::Consumer(never)) => match never {},
    }
}

fn browse_documents_for_test(
    transport: &mut ScriptedTransport,
) -> Result<Vec<Value>, AlgoliaClientError> {
    let mut delivered = Vec::new();
    let result = tokio_test::block_on(browse_documents_with_transport(
        transport,
        "APP123",
        "key",
        "products",
        |documents| {
            delivered.extend(documents);
            Ok::<_, Infallible>(())
        },
    ));
    match result {
        Ok(()) => Ok(delivered),
        Err(BrowseError::Client(error)) => Err(error),
        Err(BrowseError::Consumer(never)) => match never {},
    }
}

fn browse_documents_with_limits_for_test(
    transport: &mut ScriptedTransport,
    limits: TraversalLimits,
) -> Result<Vec<Value>, AlgoliaClientError> {
    let mut delivered = Vec::new();
    let result = tokio_test::block_on(browse_documents_with_transport_and_limits(
        transport,
        "APP123",
        "key",
        "products",
        limits,
        |documents| {
            delivered.extend(documents);
            Ok::<_, Infallible>(())
        },
    ));
    match result {
        Ok(()) => Ok(delivered),
        Err(BrowseError::Client(error)) => Err(error),
        Err(BrowseError::Consumer(never)) => match never {},
    }
}

#[test]
fn client_policy_validates_app_id_before_host_construction() {
    for app_id in ["", "bad/id", "bad.example", "bad:443", "bad app"] {
        assert_eq!(
            request_for_test(app_id, "products", AlgoliaMethod::Get, "settings")
                .expect_err("invalid app ID must fail before URL construction")
                .kind(),
            AlgoliaErrorKind::Validation
        );
    }

    let request = request_for_test("APP123", "products", AlgoliaMethod::Get, "settings")
        .expect("valid app ID should produce a request");
    assert_eq!(
        request.url,
        "https://APP123-dsn.algolia.net/1/indexes/products/settings"
    );
}

#[test]
fn client_policy_percent_encodes_index_names() {
    let request = request_for_test("APP123", "summer/sale 2026", AlgoliaMethod::Post, "browse")
        .expect("valid request should be planned");

    assert_eq!(
        request.url,
        "https://APP123-dsn.algolia.net/1/indexes/summer%2Fsale%202026/browse"
    );
    assert_eq!(request.method, AlgoliaMethod::Post);
}

#[test]
fn client_policy_uses_exact_https_host_and_fixed_timeouts() {
    let request = request_for_test("APP123", "products", AlgoliaMethod::Get, "settings")
        .expect("valid request should be planned");

    assert!(request.url.starts_with("https://APP123-dsn.algolia.net/"));
    assert!(!request.url.contains("http://"));
    assert_eq!(request.policy.connect_timeout, Duration::from_secs(5));
    assert_eq!(request.policy.request_timeout, Duration::from_secs(30));
    assert!(request.policy.redirects_disabled);
    assert!(request.policy.proxy_disabled);
}

#[test]
fn client_policy_has_no_production_base_url_override() {
    let request = request_for_test("APP123", "products", AlgoliaMethod::Get, "settings")
        .expect("valid request should be planned");

    assert_eq!(
        request.url,
        "https://APP123-dsn.algolia.net/1/indexes/products/settings"
    );
}

#[test]
fn retry_policy_retries_transient_failures_and_stops_on_success() {
    let mut transport = ScriptedTransport::new(vec![
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Timeout,
            "Algolia request timed out",
        )),
        status(429),
        ok(json!({"done": true})),
    ]);

    let response = scripted_json_for_test(
        &mut transport,
        "APP123",
        "products",
        AlgoliaMethod::Get,
        "settings",
    )
    .expect("third attempt should succeed");

    assert_eq!(response, json!({"done": true}));
    assert_eq!(transport.requests.len(), 3);
}

#[test]
fn retry_policy_uses_algolia_fallback_hosts_after_transient_data_failure() {
    let mut transport = ScriptedTransport::new(vec![
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Timeout,
            "Algolia request timed out",
        )),
        ok(json!({"done": true})),
    ]);

    let response = scripted_json_for_test(
        &mut transport,
        "APP123",
        "products",
        AlgoliaMethod::Get,
        "settings",
    )
    .expect("first fallback host should succeed");

    assert_eq!(response, json!({"done": true}));
    assert_eq!(
        request_urls(&transport),
        vec![
            "https://APP123-dsn.algolia.net/1/indexes/products/settings",
            "https://APP123-1.algolianet.com/1/indexes/products/settings",
        ]
    );
}

#[test]
fn retry_policy_uses_algolia_fallback_hosts_after_transient_control_failure() {
    let mut transport = ScriptedTransport::new(vec![
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Transport,
            "Algolia request failed",
        )),
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 2,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": false
            }]),
            0,
            1,
        )),
    ]);

    let indexes = list_indexes_for_test(&mut transport).expect("control fallback should succeed");

    assert_eq!(indexes.len(), 1);
    assert_eq!(
        request_urls(&transport),
        vec![
            "https://APP123.algolia.net/1/indexes?page=0&hitsPerPage=100",
            "https://APP123-1.algolianet.com/1/indexes?page=0&hitsPerPage=100",
        ]
    );
}

#[test]
fn retry_policy_stops_immediately_for_non_retryable_failures() {
    for kind in [
        AlgoliaErrorKind::Validation,
        AlgoliaErrorKind::Schema,
        AlgoliaErrorKind::Decode,
        AlgoliaErrorKind::Redirect,
        AlgoliaErrorKind::Progress,
        AlgoliaErrorKind::Limit,
    ] {
        let mut transport =
            ScriptedTransport::new(vec![Err(AlgoliaClientError::new(kind, "non retryable"))]);
        let result = scripted_json_for_test(
            &mut transport,
            "APP123",
            "products",
            AlgoliaMethod::Get,
            "settings",
        );
        assert_eq!(
            result.expect_err("non-retryable error should fail").kind(),
            kind
        );
        assert_eq!(transport.requests.len(), 1);
    }

    let mut transport = ScriptedTransport::new(vec![status(400)]);
    assert_eq!(
        scripted_json_for_test(
            &mut transport,
            "APP123",
            "products",
            AlgoliaMethod::Get,
            "settings",
        )
        .expect_err("4xx should fail")
        .kind(),
        AlgoliaErrorKind::Upstream
    );
    assert_eq!(transport.requests.len(), 1);
}

#[test]
fn retry_policy_returns_stable_variant_after_retry_budget() {
    for (responses, expected_kind) in [
        (
            vec![
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Timeout,
                    "Algolia request timed out",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Timeout,
                    "Algolia request timed out",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Timeout,
                    "Algolia request timed out",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Timeout,
                    "Algolia request timed out",
                )),
            ],
            AlgoliaErrorKind::Timeout,
        ),
        (
            vec![
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "Algolia request failed",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "Algolia request failed",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "Algolia request failed",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "Algolia request failed",
                )),
            ],
            AlgoliaErrorKind::Transport,
        ),
        (
            vec![status(429), status(429), status(429), status(429)],
            AlgoliaErrorKind::RateLimit,
        ),
        (
            vec![status(503), status(500), status(502), status(504)],
            AlgoliaErrorKind::Server,
        ),
    ] {
        let mut transport = ScriptedTransport::new(responses);

        let result = scripted_json_for_test(
            &mut transport,
            "APP123",
            "products",
            AlgoliaMethod::Get,
            "settings",
        );

        assert_eq!(
            result.expect_err("retry budget should fail").kind(),
            expected_kind
        );
        assert_eq!(
            request_urls(&transport),
            vec![
                "https://APP123-dsn.algolia.net/1/indexes/products/settings",
                "https://APP123-1.algolianet.com/1/indexes/products/settings",
                "https://APP123-2.algolianet.com/1/indexes/products/settings",
                "https://APP123-3.algolianet.com/1/indexes/products/settings",
            ]
        );
    }
}

#[test]
fn list_indexes_pagination_starts_at_page_zero_and_follows_nb_pages_changes() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "items": [{"name": "products", "entries": 2, "updatedAt": "2026-01-01T00:00:00Z", "pendingTask": false}],
            "nbPages": 2
        })),
        ok(json!({
            "items": [{"name": "articles", "entries": 1, "updatedAt": "2026-01-02T00:00:00Z", "pendingTask": false}],
            "nbPages": 3
        })),
        ok(json!({
            "items": [{"name": "archive", "entries": 0, "updatedAt": "2026-01-03T00:00:00Z", "pendingTask": false}],
            "nbPages": 3
        })),
    ]);

    let indexes = list_indexes_for_test(&mut transport).expect("pagination should complete");

    assert_eq!(indexes.len(), 3);
    assert_eq!(indexes[0].name, "products");
    assert_eq!(
        transport.requests[0].url,
        "https://APP123.algolia.net/1/indexes?page=0&hitsPerPage=100"
    );
    assert_eq!(
        transport.requests[1].url,
        "https://APP123.algolia.net/1/indexes?page=1&hitsPerPage=100"
    );
    assert_eq!(
        transport.requests[2].url,
        "https://APP123.algolia.net/1/indexes?page=2&hitsPerPage=100"
    );
}

#[test]
fn list_indexes_pagination_rejects_missing_metadata_and_bad_items() {
    for (body, expected_kind) in [
        (json!({"items": []}), AlgoliaErrorKind::Schema),
        (
            json!({"items": [], "page": 1, "nbPages": 1}),
            AlgoliaErrorKind::Progress,
        ),
        (
            json!({"items": [{"name": 7}], "page": 0, "nbPages": 1}),
            AlgoliaErrorKind::Schema,
        ),
        (
            json!({"items": [{"name": "x", "entries": "many", "updatedAt": "", "pendingTask": false}], "page": 0, "nbPages": 1}),
            AlgoliaErrorKind::Schema,
        ),
    ] {
        let mut transport = ScriptedTransport::new(vec![ok(body)]);
        assert_eq!(
            list_indexes_for_test(&mut transport)
                .expect_err("invalid listing should fail")
                .kind(),
            expected_kind
        );
    }
}

#[test]
fn list_indexes_pagination_rejects_repeated_content() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "items": [{"name": "products", "entries": 2, "updatedAt": "2026-01-01T00:00:00Z", "pendingTask": false}],
            "page": 0,
            "nbPages": 2
        })),
        ok(json!({
            "items": [{"name": "products", "entries": 2, "updatedAt": "2026-01-01T00:00:00Z", "pendingTask": false}],
            "page": 1,
            "nbPages": 2
        })),
    ]);

    assert_eq!(
        list_indexes_for_test(&mut transport)
            .expect_err("repeated content should fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
}

#[test]
fn list_indexes_pagination_rejects_page_equal_to_shrunk_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "items": [{"name": "products", "entries": 2, "updatedAt": "2026-01-01T00:00:00Z", "pendingTask": false}],
            "page": 0,
            "nbPages": 2
        })),
        ok(json!({
            "items": [{"name": "articles", "entries": 1, "updatedAt": "2026-01-02T00:00:00Z", "pendingTask": false}],
            "page": 1,
            "nbPages": 1
        })),
    ]);

    assert_eq!(
        list_indexes_for_test(&mut transport)
            .expect_err("page equal to the shrunk page count must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
    assert_eq!(transport.requests.len(), 2);
}

#[test]
fn list_indexes_pagination_rejects_nonempty_zero_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "items": [{"name": "products", "entries": 2, "updatedAt": "2026-01-01T00:00:00Z", "pendingTask": false}],
        "page": 0,
        "nbPages": 0
    }))]);

    assert_eq!(
        list_indexes_for_test(&mut transport)
            .expect_err("non-empty zero-page listing must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
}

#[test]
fn list_indexes_pagination_accepts_empty_zero_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "items": [],
        "page": 0,
        "nbPages": 0
    }))]);

    let indexes = list_indexes_for_test(&mut transport).expect("empty zero-page listing is valid");

    assert!(indexes.is_empty());
    assert_eq!(transport.requests.len(), 1);
}

#[test]
fn list_indexes_pagination_accepts_public_rows_without_pending_task() {
    let mut transport = ScriptedTransport::new(vec![ok(index_page(
        json!([{
            "name": "products",
            "entries": 2,
            "updatedAt": "2026-01-01T00:00:00Z"
        }]),
        0,
        1,
    ))]);

    let indexes =
        list_indexes_for_test(&mut transport).expect("public listing does not need pendingTask");

    assert_eq!(indexes.len(), 1);
    assert_eq!(indexes[0].name, "products");
    assert_eq!(indexes[0].entries, 2);
    assert_eq!(indexes[0].updated_at, "2026-01-01T00:00:00Z");
}

#[test]
fn source_export_acl_and_quiescence_reads_key_acl_through_strict_planner() {
    for (acl, expected) in [
        (json!(["search", "seeUnretrievableAttributes"]), true),
        (json!(["admin"]), true),
        (json!(["search", "browse"]), false),
    ] {
        let mut transport = ScriptedTransport::new(vec![ok(json!({ "acl": acl }))]);

        assert_eq!(
            key_allows_unretrievable_for_test(&mut transport).expect("ACL lookup should parse"),
            expected
        );
        assert_eq!(transport.requests.len(), 1);
        assert_eq!(transport.requests[0].method, AlgoliaMethod::Get);
        assert_eq!(
            transport.requests[0].url,
            "https://APP123.algolia.net/1/keys/key"
        );
        assert_eq!(transport.requests[0].body, None);
    }
}

#[test]
fn source_export_acl_and_quiescence_rejects_unretrievable_without_capability() {
    let settings = json!({ "unretrievableAttributes": ["secret"] });
    let mut denied = ScriptedTransport::new(vec![ok(json!({ "acl": ["search"] }))]);

    let error = require_unretrievable_access_for_test(&mut denied, &settings)
        .expect_err("settings with hidden fields need capability proof");

    assert_eq!(error.kind(), AlgoliaErrorKind::Validation);
    assert!(!error.safe_message().contains("source-secret"));

    let mut secret_key_transport = ScriptedTransport::new(vec![ok(json!({ "acl": ["search"] }))]);
    let secret_key_error = tokio_test::block_on(require_unretrievable_access_with_transport(
        &mut secret_key_transport,
        "APP123",
        "source-secret",
        &settings,
    ))
    .expect_err("scrubbed errors must not echo the API key");
    assert!(!secret_key_error.safe_message().contains("source-secret"));

    let mut allowed = ScriptedTransport::new(vec![ok(json!({
        "acl": ["seeUnretrievableAttributes"]
    }))]);
    require_unretrievable_access_for_test(&mut allowed, &settings)
        .expect("seeUnretrievableAttributes should allow export");

    let mut no_hidden_fields = ScriptedTransport::new(Vec::new());
    require_unretrievable_access_for_test(&mut no_hidden_fields, &json!({}))
        .expect("ACL lookup is unnecessary without unretrievableAttributes");
    assert!(no_hidden_fields.requests.is_empty());
}

#[test]
fn source_export_acl_and_quiescence_polls_until_selected_index_is_not_pending() {
    let mut transport = ScriptedTransport::new(vec![
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 7,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": true
            }]),
            0,
            1,
        )),
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 7,
                "updatedAt": "2026-01-01T00:00:01Z",
                "pendingTask": false
            }]),
            0,
            1,
        )),
    ]);

    let record = wait_for_quiescent_source_for_test(&mut transport)
        .expect("pending selected index should eventually settle");

    assert_eq!(record.name, "products");
    assert_eq!(record.entries, 7);
    assert_eq!(record.updated_at, "2026-01-01T00:00:01Z");
    assert!(!record.pending_task);
    assert_eq!(transport.requests.len(), 2);
}

#[test]
fn source_export_acl_and_quiescence_rejects_ambiguous_selected_index_metadata() {
    for body in [
        index_page(
            json!([{
                "name": "other",
                "entries": 1,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": false
            }]),
            0,
            1,
        ),
        index_page(
            json!([
                {
                    "name": "products",
                    "entries": 1,
                    "updatedAt": "2026-01-01T00:00:00Z",
                    "pendingTask": false
                },
                {
                    "name": "products",
                    "entries": 2,
                    "updatedAt": "2026-01-01T00:00:00Z",
                    "pendingTask": false
                }
            ]),
            0,
            1,
        ),
    ] {
        let mut transport = ScriptedTransport::new(vec![ok(body)]);

        assert_eq!(
            wait_for_quiescent_source_for_test(&mut transport)
                .expect_err("missing or duplicate selected index should fail")
                .kind(),
            AlgoliaErrorKind::Progress
        );
    }
}

#[test]
fn source_export_acl_and_quiescence_requires_selected_pending_task_metadata() {
    for item in [
        json!({
            "name": "products",
            "entries": 1,
            "updatedAt": "2026-01-01T00:00:00Z"
        }),
        json!({
            "name": "products",
            "entries": 1,
            "updatedAt": "2026-01-01T00:00:00Z",
            "pendingTask": "false"
        }),
    ] {
        let mut transport = ScriptedTransport::new(vec![ok(index_page(json!([item]), 0, 1))]);

        let error = wait_for_quiescent_source_for_test(&mut transport)
            .expect_err("selected source quiescence requires pendingTask");

        assert_eq!(error.kind(), AlgoliaErrorKind::Schema);
    }
}

#[test]
fn source_export_acl_and_quiescence_deadline_expiry_is_scrubbed() {
    let mut transport = ScriptedTransport::new(vec![
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 1,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": true
            }]),
            0,
            1,
        )),
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 1,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": true
            }]),
            0,
            1,
        )),
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 1,
                "updatedAt": "secret-body-value",
                "pendingTask": true
            }]),
            0,
            1,
        )),
    ]);

    let error = wait_for_quiescent_source_for_test(&mut transport)
        .expect_err("poll budget should bound pending tasks");

    assert_eq!(error.kind(), AlgoliaErrorKind::Progress);
    assert!(!error.safe_message().contains("source-secret"));
    assert!(!error.safe_message().contains("secret-body-value"));
    assert_eq!(transport.requests.len(), 3);
}

#[test]
fn strict_source_progress_rejects_malformed_hits() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [{"objectID": "ok"}, "bad"],
        "page": 0,
        "nbPages": 1
    }))]);

    let result: Result<Vec<Value>, AlgoliaClientError> =
        paginated_hits_for_test(&mut transport, "rules/search");

    assert_eq!(
        result.expect_err("malformed hit should fail").kind(),
        AlgoliaErrorKind::Schema
    );
}

#[test]
fn strict_source_progress_uses_explicit_nb_pages_not_short_page_heuristic() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "hits": [{"objectID": "one"}],
            "page": 0,
            "nbPages": 2
        })),
        ok(json!({
            "hits": [{"objectID": "two"}],
            "page": 1,
            "nbPages": 2
        })),
    ]);

    let result: Vec<Value> =
        paginated_hits_for_test(&mut transport, "rules/search").expect("two pages should load");
    assert_eq!(result.len(), 2);
    assert_eq!(transport.requests.len(), 2);
}

#[test]
fn strict_source_progress_rejects_page_equal_to_shrunk_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "hits": [{"objectID": "one"}],
            "page": 0,
            "nbPages": 2
        })),
        ok(json!({
            "hits": [{"objectID": "two"}],
            "page": 1,
            "nbPages": 1
        })),
    ]);

    let result: Result<Vec<Value>, AlgoliaClientError> =
        paginated_hits_for_test(&mut transport, "rules/search");

    assert_eq!(
        result
            .expect_err("page equal to the shrunk page count must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
    assert_eq!(transport.requests.len(), 2);
}

#[test]
fn strict_source_progress_rejects_nonempty_zero_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [{"objectID": "one"}],
        "page": 0,
        "nbPages": 0
    }))]);

    let result: Result<Vec<Value>, AlgoliaClientError> =
        paginated_hits_for_test(&mut transport, "rules/search");

    assert_eq!(
        result
            .expect_err("non-empty zero-page search result must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
}

#[test]
fn strict_source_progress_accepts_empty_zero_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [],
        "page": 0,
        "nbPages": 0
    }))]);

    let result: Vec<Value> = paginated_hits_for_test(&mut transport, "rules/search")
        .expect("empty zero-page result is valid");

    assert!(result.is_empty());
    assert_eq!(transport.requests.len(), 1);
}

#[test]
fn strict_source_progress_rejects_empty_intermediate_search_page() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "hits": [{"objectID": "one"}],
            "page": 0,
            "nbPages": 3
        })),
        ok(json!({
            "hits": [],
            "page": 1,
            "nbPages": 3
        })),
    ]);

    let result: Result<Vec<Value>, AlgoliaClientError> =
        paginated_hits_for_test(&mut transport, "rules/search");

    assert_eq!(
        result
            .expect_err("empty intermediate search page must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
    assert_eq!(transport.requests.len(), 2);
}

#[test]
fn strict_source_progress_rejects_repeated_browse_cursor() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({"hits": [{"objectID": "one"}], "cursor": "same"})),
        ok(json!({"hits": [{"objectID": "two"}], "cursor": "same"})),
    ]);

    assert_eq!(
        browse_documents_for_test(&mut transport)
            .expect_err("repeated cursor must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
}

#[test]
fn strict_source_progress_rejects_malformed_browse_hits() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [{"objectID": "one"}, "bad"]
    }))]);

    assert_eq!(
        browse_documents_for_test(&mut transport)
            .expect_err("malformed browse hit should fail")
            .kind(),
        AlgoliaErrorKind::Schema
    );
}

#[test]
fn strict_source_progress_rejects_non_string_browse_cursor() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [{"objectID": "one"}],
        "cursor": 123
    }))]);

    assert_eq!(
        browse_documents_for_test(&mut transport)
            .expect_err("malformed browse cursor must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
}

#[test]
fn strict_source_progress_streams_browse_page_before_following_request_failure() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({"hits": [{"objectID": "one"}], "cursor": "next"})),
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Transport,
            "Algolia request failed",
        )),
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Transport,
            "Algolia request failed",
        )),
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Transport,
            "Algolia request failed",
        )),
    ]);
    let mut delivered_page_sizes = Vec::new();

    let result = tokio_test::block_on(browse_documents_with_transport(
        &mut transport,
        "APP123",
        "key",
        "products",
        |documents| {
            delivered_page_sizes.push(documents.len());
            Ok::<_, Infallible>(())
        },
    ));

    assert_eq!(
        result
            .expect_err("the second request must surface its transport failure")
            .client_error()
            .expect("the traversal should report a client error")
            .kind(),
        AlgoliaErrorKind::Transport
    );
    assert_eq!(delivered_page_sizes, vec![1]);
}

#[test]
fn response_byte_limit_is_enforced_before_json_decoding() {
    let mut transport = ScriptedTransport::new(vec![Ok(RawResponse {
        status: 200,
        body: vec![b' '; MAX_RESPONSE_BYTES + 1],
    })]);

    assert_eq!(
        scripted_json_for_test(
            &mut transport,
            "APP123",
            "products",
            AlgoliaMethod::Get,
            "settings",
        )
        .expect_err("oversized response should fail before JSON decode")
        .kind(),
        AlgoliaErrorKind::Limit
    );
}

#[test]
fn response_byte_limit_rejects_production_content_length_before_buffering() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let address = listener.local_addr().expect("test server address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("test request should arrive");
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n",
            MAX_RESPONSE_BYTES + 1
        )
        .expect("headers should write");
    });

    let client = reqwest::Client::builder()
        .connect_timeout(ALGOLIA_CONNECT_TIMEOUT)
        .timeout(ALGOLIA_REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("test client should build");
    let mut transport = ReqwestTransport { client: &client };
    let request = PlannedRequest {
        method: AlgoliaMethod::Get,
        url: format!("http://{address}/oversized"),
        fallback_urls: Vec::new(),
        headers: Vec::new(),
        body: None,
        policy: RequestPolicy {
            connect_timeout: ALGOLIA_CONNECT_TIMEOUT,
            request_timeout: ALGOLIA_REQUEST_TIMEOUT,
            redirects_disabled: true,
            proxy_disabled: true,
        },
        max_response_bytes: MAX_RESPONSE_BYTES,
    };

    let result = tokio_test::block_on(transport.send(request));

    server.join().expect("test server should finish");
    assert_eq!(
        result
            .expect_err("oversized content-length should fail before body buffering")
            .kind(),
        AlgoliaErrorKind::Limit
    );
}

#[test]
fn source_export_raw_traversal_preserves_json_and_uses_strict_browse_bodies() {
    let raw = json!({
        "objectID": "doc-1",
        "enabled": true,
        "deletedAt": null,
        "nested": {"z": 1, "items": [false, null, {"x": "y"}]},
        "_highlightResult": {"must": "remain"}
    });
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({"hits": [raw.clone()], "cursor": "next"})),
        ok(json!({"hits": []})),
    ]);
    let mut delivered = Vec::new();

    tokio_test::block_on(browse_documents_with_transport(
        &mut transport,
        "APP123",
        "key",
        "summer/sale",
        |page| {
            delivered.extend(page);
            Ok::<_, Infallible>(())
        },
    ))
    .expect("raw traversal should succeed");

    assert_eq!(delivered, vec![raw]);
    assert!(transport
        .requests
        .iter()
        .all(|request| request.url.contains("/1/indexes/summer%2Fsale/browse")));
    assert_eq!(
        transport.requests[0].body,
        Some(json!({"hitsPerPage": 1000, "attributesToRetrieve": ["*"]}))
    );
    assert_eq!(transport.requests[1].body, Some(json!({"cursor": "next"})));
}

#[test]
fn source_export_raw_traversal_requires_unique_string_object_ids() {
    for hits in [
        json!([{"value": 1}]),
        json!([{"objectID": 7}]),
        json!([{"objectID": "same"}, {"objectID": "same"}]),
    ] {
        let mut transport = ScriptedTransport::new(vec![ok(json!({"hits": hits}))]);
        let result = browse_documents_for_test(&mut transport);
        assert_eq!(
            result.expect_err("invalid objectID must fail").kind(),
            AlgoliaErrorKind::Schema
        );
    }
}

/// Search-only response decorations are not part of saved rule or synonym definitions.
#[test]
fn source_export_raw_traversal_strips_search_decorations_without_normalizing_definitions() {
    let expected_rule = json!({
        "objectID": "rule-1",
        "enabled": true,
        "condition": {"pattern": "sale"},
        "consequence": {"params": {"filters": ["brand:acme", null]}},
        "customDefinitionField": {"nested": {"keep": true}}
    });
    let expected_synonym = json!({
        "objectID": "syn-1",
        "type": "synonym",
        "synonyms": ["tv", "television"],
        "metadata": {"nested": {"keep": false}}
    });
    let mut decorated_rule = expected_rule.clone();
    decorated_rule["_highlightResult"] = json!({"condition": {"pattern": {"value": "sale"}}});
    decorated_rule["_metadata"] = json!({"lastUpdate": 123});
    let mut decorated_synonym = expected_synonym.clone();
    decorated_synonym["_highlightResult"] = json!({"synonyms": [{"value": "tv"}]});
    let mut rules_transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [decorated_rule],
        "page": 0,
        "nbPages": 1
    }))]);
    let mut synonyms_transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [decorated_synonym],
        "page": 0,
        "nbPages": 1
    }))]);

    let rules = paginated_raw_hits_for_test(&mut rules_transport, "rules/search")
        .expect("rules should stream as raw JSON");
    let synonyms = paginated_raw_hits_for_test(&mut synonyms_transport, "synonyms/search")
        .expect("synonyms should stream as raw JSON");

    assert_eq!(rules, vec![expected_rule]);
    assert_eq!(synonyms, vec![expected_synonym]);
    assert_eq!(
        rules_transport.requests[0].body,
        Some(json!({"query": "", "hitsPerPage": 1000, "page": 0}))
    );
    assert!(rules_transport.requests[0]
        .url
        .ends_with("/1/indexes/products/rules/search"));
    assert!(synonyms_transport.requests[0]
        .url
        .ends_with("/1/indexes/products/synonyms/search"));
}

#[test]
fn source_export_synonyms_search_accepts_algolia_nbhits_only_pagination() {
    let first_page_hits: Vec<Value> = (0..1000)
        .map(|index| {
            json!({
                "objectID": format!("syn-{index:04}"),
                "type": "synonym",
                "synonyms": [format!("term-{index:04}"), format!("alias-{index:04}")]
            })
        })
        .collect();
    let last_hit = json!({
        "objectID": "syn-1000",
        "type": "synonym",
        "synonyms": ["term-1000", "alias-1000"]
    });
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "hits": first_page_hits,
            "nbHits": 1001
        })),
        ok(json!({
            "hits": [last_hit.clone()],
            "nbHits": 1001
        })),
    ]);

    let synonyms = paginated_raw_hits_for_test(&mut transport, "synonyms/search")
        .expect("nbHits-only synonym pagination should stream all pages");

    assert_eq!(synonyms.len(), 1001);
    assert_eq!(synonyms[0]["objectID"], "syn-0000");
    assert_eq!(synonyms[999]["objectID"], "syn-0999");
    assert_eq!(synonyms[1000], last_hit);
    assert_eq!(transport.requests.len(), 2);
    assert_eq!(
        transport.requests[0].body,
        Some(json!({"query": "", "hitsPerPage": 1000, "page": 0}))
    );
    assert_eq!(
        transport.requests[1].body,
        Some(json!({"query": "", "hitsPerPage": 1000, "page": 1}))
    );
}

#[test]
fn source_export_raw_traversal_requires_unique_string_object_ids_for_rules_and_synonyms() {
    for endpoint in ["rules/search", "synonyms/search"] {
        for hits in [
            json!([{"condition": {"pattern": "sale"}}]),
            json!([{"objectID": null}]),
            json!([{"objectID": "dup"}, {"objectID": "dup"}]),
        ] {
            let mut transport = ScriptedTransport::new(vec![ok(json!({
                "hits": hits,
                "page": 0,
                "nbPages": 1
            }))]);
            let result = paginated_raw_hits_for_test(&mut transport, endpoint);
            assert_eq!(
                result.expect_err("invalid objectID must fail").kind(),
                AlgoliaErrorKind::Schema
            );
        }
    }
}

#[test]
fn source_export_raw_traversal_enforces_index_list_item_limits() {
    let limits = TraversalLimits {
        max_pages: 1,
        max_items: 2,
        max_response_bytes: 512,
    };
    let exact_cap_page = index_page(
        json!([
            {
                "name": "products",
                "entries": 2,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": false
            },
            {
                "name": "archive",
                "entries": 0,
                "updatedAt": "2026-01-01T00:00:01Z",
                "pendingTask": false
            }
        ]),
        0,
        1,
    );
    let over_cap_page = index_page(
        json!([
            {
                "name": "products",
                "entries": 2,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": false
            },
            {
                "name": "archive",
                "entries": 0,
                "updatedAt": "2026-01-01T00:00:01Z",
                "pendingTask": false
            },
            {
                "name": "logs",
                "entries": 1,
                "updatedAt": "2026-01-01T00:00:02Z",
                "pendingTask": false
            }
        ]),
        0,
        1,
    );

    let mut exact_transport = ScriptedTransport::new(vec![ok(exact_cap_page)]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut exact_transport, limits)
            .expect("exact index item cap should pass")
            .len(),
        2
    );

    let mut over_transport = ScriptedTransport::new(vec![ok(over_cap_page)]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut over_transport, limits)
            .expect_err("index item cap+1 should fail")
            .kind(),
        AlgoliaErrorKind::Limit
    );
}

#[test]
fn source_export_raw_traversal_enforces_index_list_page_limits() {
    let limits = TraversalLimits {
        max_pages: 2,
        max_items: 10,
        max_response_bytes: 512,
    };
    let first_page = index_page(
        json!([{
            "name": "products",
            "entries": 2,
            "updatedAt": "2026-01-01T00:00:00Z",
            "pendingTask": false
        }]),
        0,
        2,
    );
    let second_page = index_page(
        json!([{
            "name": "archive",
            "entries": 0,
            "updatedAt": "2026-01-01T00:00:01Z",
            "pendingTask": false
        }]),
        1,
        2,
    );
    let page_cap_plus_one = index_page(
        json!([{
            "name": "logs",
            "entries": 1,
            "updatedAt": "2026-01-01T00:00:02Z",
            "pendingTask": false
        }]),
        1,
        3,
    );

    let mut exact_transport = ScriptedTransport::new(vec![ok(first_page.clone()), ok(second_page)]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut exact_transport, limits)
            .expect("exact index page cap should pass")
            .len(),
        2
    );

    let mut over_transport = ScriptedTransport::new(vec![ok(first_page), ok(page_cap_plus_one)]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut over_transport, limits)
            .expect_err("index page cap+1 should fail before requesting page 2")
            .kind(),
        AlgoliaErrorKind::Limit
    );
    assert_eq!(over_transport.requests.len(), 2);
}

#[test]
fn source_export_raw_traversal_enforces_index_list_response_byte_limits() {
    let page = index_page(
        json!([{
            "name": "products",
            "entries": 2,
            "updatedAt": "2026-01-01T00:00:00Z",
            "pendingTask": false
        }]),
        0,
        1,
    );
    let body = serde_json::to_vec(&page).expect("test fixture should serialize");
    let exact_limits = TraversalLimits {
        max_pages: 1,
        max_items: 1,
        max_response_bytes: body.len(),
    };
    let over_limits = TraversalLimits {
        max_pages: 1,
        max_items: 1,
        max_response_bytes: body.len() - 1,
    };

    let mut exact_transport = ScriptedTransport::new(vec![Ok(RawResponse {
        status: 200,
        body: body.clone(),
    })]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut exact_transport, exact_limits)
            .expect("exact index response byte cap should pass")
            .len(),
        1
    );

    let mut over_transport = ScriptedTransport::new(vec![Ok(RawResponse { status: 200, body })]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut over_transport, over_limits)
            .expect_err("index response byte cap+1 should fail")
            .kind(),
        AlgoliaErrorKind::Limit
    );
}

#[test]
fn source_export_raw_traversal_uses_independent_resource_limits() {
    let exact_two_items = vec![ok(json!({
        "hits": [{"objectID": "one"}, {"objectID": "two"}],
        "page": 0,
        "nbPages": 1
    }))];
    let cap_plus_one = vec![ok(json!({
        "hits": [{"objectID": "one"}, {"objectID": "two"}, {"objectID": "three"}],
        "page": 0,
        "nbPages": 1
    }))];
    let limits = TraversalLimits {
        max_pages: 1,
        max_items: 2,
        max_response_bytes: 512,
    };

    let mut rules_transport = ScriptedTransport::new(exact_two_items.clone());
    let mut synonyms_transport = ScriptedTransport::new(exact_two_items);
    assert_eq!(
        paginated_raw_hits_with_limits_for_test(&mut rules_transport, "rules/search", limits)
            .expect("exact rules item cap should pass")
            .len(),
        2
    );
    assert_eq!(
        paginated_raw_hits_with_limits_for_test(&mut synonyms_transport, "synonyms/search", limits)
            .expect("exact synonyms item cap should pass")
            .len(),
        2
    );

    let mut rules_over_cap = ScriptedTransport::new(cap_plus_one.clone());
    let mut synonyms_over_cap = ScriptedTransport::new(cap_plus_one);
    assert_eq!(
        paginated_raw_hits_with_limits_for_test(&mut rules_over_cap, "rules/search", limits)
            .expect_err("rules cap+1 should fail")
            .kind(),
        AlgoliaErrorKind::Limit
    );
    assert_eq!(
        paginated_raw_hits_with_limits_for_test(&mut synonyms_over_cap, "synonyms/search", limits)
            .expect_err("synonyms cap+1 should fail")
            .kind(),
        AlgoliaErrorKind::Limit
    );

    let mut documents_transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [{"objectID": "one"}, {"objectID": "two"}]
    }))]);
    assert_eq!(
        browse_documents_with_limits_for_test(&mut documents_transport, limits)
            .expect("exact document item cap should pass")
            .len(),
        2
    );
}
