//! Stub summary for engine/src/vector/embedder_tests.rs.
use super::*;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug)]
struct CountingRebindingResolverState {
    calls: AtomicUsize,
    first: SocketAddr,
    second: SocketAddr,
}

#[derive(Clone, Debug)]
struct CountingRebindingResolver {
    state: Arc<CountingRebindingResolverState>,
}

impl CountingRebindingResolver {
    fn new(first: SocketAddr, second: SocketAddr) -> Self {
        Self {
            state: Arc::new(CountingRebindingResolverState {
                calls: AtomicUsize::new(0),
                first,
                second,
            }),
        }
    }

    fn call_count(&self) -> usize {
        self.state.calls.load(Ordering::SeqCst)
    }

    fn resolve_for_validation(&self) -> std::net::IpAddr {
        let call_number = self.state.calls.fetch_add(1, Ordering::SeqCst);
        if call_number == 0 {
            self.state.first.ip()
        } else {
            self.state.second.ip()
        }
    }
}

impl reqwest::dns::Resolve for CountingRebindingResolver {
    fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let call_number = self.state.calls.fetch_add(1, Ordering::SeqCst);
        let selected = if call_number == 0 {
            self.state.first
        } else {
            self.state.second
        };
        Box::pin(async move {
            let addrs: reqwest::dns::Addrs = Box::new(std::iter::once(selected));
            Ok(addrs)
        })
    }
}

// ── UserProvidedEmbedder tests (3.9) ──

#[test]
fn test_user_provided_dimensions_getter() {
    let e = UserProvidedEmbedder::new(384);
    assert_eq!(e.dimensions(), 384);
    assert_eq!(e.source(), EmbedderSource::UserProvided);
}

#[test]
fn test_user_provided_validate_correct_dimensions() {
    let e = UserProvidedEmbedder::new(3);
    assert!(e.validate_vector(&[1.0, 2.0, 3.0]).is_ok());
}

#[test]
fn test_user_provided_validate_wrong_dimensions() {
    let e = UserProvidedEmbedder::new(3);
    let err = e.validate_vector(&[1.0, 2.0]).unwrap_err();
    match err {
        VectorError::DimensionMismatch { expected, got } => {
            assert_eq!(expected, 3);
            assert_eq!(got, 2);
        }
        other => panic!("expected DimensionMismatch, got: {other}"),
    }
}

#[tokio::test]
async fn test_user_provided_embed_query_returns_error() {
    let e = UserProvidedEmbedder::new(3);
    let result = e.embed_query("hello").await;
    assert!(result.is_err());
    match result.unwrap_err() {
        VectorError::EmbeddingError(msg) => {
            assert!(msg.contains("cannot generate embeddings"));
        }
        other => panic!("expected EmbeddingError, got: {other}"),
    }
}

#[tokio::test]
async fn test_user_provided_embed_documents_returns_error() {
    let e = UserProvidedEmbedder::new(3);
    let result = e.embed_documents(&["hello", "world"]).await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        VectorError::EmbeddingError(_)
    ));
}

// ── Factory tests (3.30) ──

// These tests share process-global outbound URL test hooks in `security.rs`
// and `embedder.rs`, so constructor/request cases stay on one serial lane.

#[test]
fn test_factory_creates_user_provided() {
    let config = EmbedderConfig {
        source: EmbedderSource::UserProvided,
        dimensions: Some(768),
        ..Default::default()
    };
    let embedder = create_embedder(&config).unwrap();
    assert_eq!(embedder.dimensions(), 768);
    assert_eq!(embedder.source(), EmbedderSource::UserProvided);
}

#[test]
fn test_factory_rejects_invalid_config() {
    let config = EmbedderConfig {
        source: EmbedderSource::OpenAi,
        // Missing api_key
        ..Default::default()
    };
    assert!(create_embedder(&config).is_err());
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn test_factory_creates_rest() {
    let config = EmbedderConfig {
        source: EmbedderSource::Rest,
        url: Some("http://localhost:1234/embed".into()),
        request: Some(serde_json::json!({"input": "{{text}}"})),
        response: Some(serde_json::json!({"embedding": "{{embedding}}"})),
        dimensions: Some(384),
        ..Default::default()
    };
    let embedder = create_embedder(&config).unwrap();
    assert_eq!(embedder.source(), EmbedderSource::Rest);
    assert_eq!(embedder.dimensions(), 384);
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn test_factory_creates_openai() {
    let config = EmbedderConfig {
        source: EmbedderSource::OpenAi,
        api_key: Some("sk-test".into()),
        ..Default::default()
    };
    let embedder = create_embedder(&config).unwrap();
    assert_eq!(embedder.source(), EmbedderSource::OpenAi);
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn test_factory_creates_openai_with_localhost_url() {
    let config = EmbedderConfig {
        source: EmbedderSource::OpenAi,
        api_key: Some("sk-test".into()),
        url: Some("http://localhost:1234".into()),
        ..Default::default()
    };
    let embedder = create_embedder(&config).unwrap();
    assert_eq!(embedder.source(), EmbedderSource::OpenAi);
}

// ── RestEmbedder tests (3.13) ──

mod rest_tests {
    use std::collections::HashMap;

    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Verify that RestEmbedder renders request templates by replacing `{{text}}` placeholder with input text.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_rest_embedder_request_format() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embed"))
            .and(body_json(serde_json::json!({"input": "hello world"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [0.1, 0.2, 0.3]
            })))
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::Rest,
            url: Some(format!("{}/embed", server.uri())),
            request: Some(serde_json::json!({"input": "{{text}}"})),
            response: Some(serde_json::json!({"embedding": "{{embedding}}"})),
            dimensions: Some(3),
            ..Default::default()
        };
        let e = RestEmbedder::new(&config).unwrap();
        let result = e.embed_query("hello world").await.unwrap();
        assert_eq!(result.len(), 3);
        assert!((result[0] - 0.1).abs() < 0.001);
    }

    /// Verify that RestEmbedder navigates nested JSON response templates to extract embedding vectors.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_rest_embedder_response_parsing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {"embedding": [1.0, 2.0, 3.0, 4.0]}
            })))
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::Rest,
            url: Some(format!("{}/embed", server.uri())),
            request: Some(serde_json::json!({"text": "{{text}}"})),
            response: Some(serde_json::json!({"data": {"embedding": "{{embedding}}"}})),
            dimensions: Some(4),
            ..Default::default()
        };
        let e = RestEmbedder::new(&config).unwrap();
        let result = e.embed_query("test").await.unwrap();
        assert_eq!(result, vec![1.0, 2.0, 3.0, 4.0]);
    }

    /// Verify that RestEmbedder detects batch templates containing `{{..}}` and expands them with all texts.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_rest_embedder_batch_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embeddings": [
                    [0.1, 0.2],
                    [0.3, 0.4]
                ]
            })))
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::Rest,
            url: Some(format!("{}/embed", server.uri())),
            request: Some(serde_json::json!({"inputs": ["{{text}}", "{{..}}"]})),
            response: Some(serde_json::json!({"embeddings": ["{{embedding}}", "{{..}}"]})),
            dimensions: Some(2),
            ..Default::default()
        };
        let e = RestEmbedder::new(&config).unwrap();
        let results = e.embed_documents(&["hello", "world"]).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], vec![0.1, 0.2]);
        assert_eq!(results[1], vec![0.3, 0.4]);
    }

    /// Verify that RestEmbedder returns error for connection failures to unreachable endpoints.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_rest_embedder_network_error() {
        let config = EmbedderConfig {
            source: EmbedderSource::Rest,
            url: Some("http://127.0.0.1:1/embed".into()),
            request: Some(serde_json::json!({"input": "{{text}}"})),
            response: Some(serde_json::json!({"embedding": "{{embedding}}"})),
            dimensions: Some(3),
            ..Default::default()
        };
        let e = RestEmbedder::new(&config).unwrap();
        let result = e.embed_query("test").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            VectorError::EmbeddingError(_)
        ));
    }

    /// Verify that RestEmbedder returns error when the server responds with non-success HTTP status.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_rest_embedder_bad_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal server error"))
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::Rest,
            url: Some(format!("{}/embed", server.uri())),
            request: Some(serde_json::json!({"input": "{{text}}"})),
            response: Some(serde_json::json!({"embedding": "{{embedding}}"})),
            dimensions: Some(3),
            ..Default::default()
        };
        let e = RestEmbedder::new(&config).unwrap();
        let result = e.embed_query("test").await;
        assert!(result.is_err());
    }

    /// Verify that RestEmbedder includes custom headers from configuration in HTTP requests.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_rest_embedder_custom_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("X-Custom", "my-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [0.1, 0.2, 0.3]
            })))
            .mount(&server)
            .await;

        let mut headers = HashMap::new();
        headers.insert("X-Custom".into(), "my-value".into());

        let config = EmbedderConfig {
            source: EmbedderSource::Rest,
            url: Some(format!("{}/embed", server.uri())),
            request: Some(serde_json::json!({"input": "{{text}}"})),
            response: Some(serde_json::json!({"embedding": "{{embedding}}"})),
            headers: Some(headers),
            dimensions: Some(3),
            ..Default::default()
        };
        let e = RestEmbedder::new(&config).unwrap();
        let result = e.embed_query("test").await.unwrap();
        assert_eq!(result.len(), 3);
    }
}

// ── OpenAiEmbedder tests (3.20) ──

mod openai_tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build a mock OpenAI embeddings API response with the given embedding vectors.
    ///
    /// Constructs the standard OpenAI response format with `data` array containing indexed embedding objects.
    ///
    /// # Arguments
    ///
    /// - `embeddings`: Vec of embedding vectors (as f64).
    ///
    /// # Returns
    ///
    /// `serde_json::Value` in OpenAI response format with object/data/model/usage fields.
    fn openai_response(embeddings: Vec<Vec<f64>>) -> serde_json::Value {
        let data: Vec<serde_json::Value> = embeddings
            .into_iter()
            .enumerate()
            .map(|(i, emb)| {
                serde_json::json!({
                    "object": "embedding",
                    "embedding": emb,
                    "index": i
                })
            })
            .collect();
        serde_json::json!({
            "object": "list",
            "data": data,
            "model": "text-embedding-3-small",
            "usage": {"prompt_tokens": 5, "total_tokens": 5}
        })
    }

    /// Verify that OpenAiEmbedder constructs correct HTTP request with Bearer token and embedding dimensions.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_openai_sends_correct_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(header("Authorization", "Bearer sk-test123"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(openai_response(vec![vec![0.1, 0.2, 0.3]])),
            )
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::OpenAi,
            api_key: Some("sk-test123".into()),
            url: Some(server.uri()),
            ..Default::default()
        };
        let e = OpenAiEmbedder::new(&config).unwrap();
        let result = e.embed_query("hello").await.unwrap();
        assert_eq!(result.len(), 3);
    }

    /// Verify that OpenAiEmbedder correctly extracts embedding vectors from OpenAI API response format.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_openai_parses_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(openai_response(vec![vec![1.0, 2.0, 3.0]])),
            )
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::OpenAi,
            api_key: Some("sk-test".into()),
            url: Some(server.uri()),
            ..Default::default()
        };
        let e = OpenAiEmbedder::new(&config).unwrap();
        let result = e.embed_query("test").await.unwrap();
        assert_eq!(result, vec![1.0, 2.0, 3.0]);
    }

    /// Verify that OpenAiEmbedder batches multiple texts in a single request and returns embeddings in order.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_openai_batch_multiple_texts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(openai_response(vec![vec![0.1, 0.2], vec![0.3, 0.4]])),
            )
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::OpenAi,
            api_key: Some("sk-test".into()),
            url: Some(server.uri()),
            ..Default::default()
        };
        let e = OpenAiEmbedder::new(&config).unwrap();
        let results = e.embed_documents(&["hello", "world"]).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], vec![0.1, 0.2]);
        assert_eq!(results[1], vec![0.3, 0.4]);
    }

    /// Verify that OpenAiEmbedder respects custom model name in configuration.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_openai_custom_model() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_response(vec![vec![0.5, 0.5]])),
            )
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::OpenAi,
            api_key: Some("sk-test".into()),
            model: Some("text-embedding-ada-002".into()),
            url: Some(server.uri()),
            ..Default::default()
        };
        let e = OpenAiEmbedder::new(&config).unwrap();
        assert_eq!(e.model, "text-embedding-ada-002");
        let result = e.embed_query("test").await.unwrap();
        assert_eq!(result.len(), 2);
    }

    /// Verify that OpenAiEmbedder accepts custom base URL and strips trailing slashes.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_openai_custom_url() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(openai_response(vec![vec![0.1]])),
            )
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::OpenAi,
            api_key: Some("sk-test".into()),
            url: Some(server.uri()),
            ..Default::default()
        };
        let e = OpenAiEmbedder::new(&config).unwrap();
        assert!(e.base_url.starts_with("http://127.0.0.1"));
        let result = e.embed_query("test").await.unwrap();
        assert_eq!(result.len(), 1);
    }

    /// Verify that OpenAiEmbedder extracts and surfaces error messages from API error responses.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_openai_error_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {
                    "message": "Invalid API key",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })))
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::OpenAi,
            api_key: Some("sk-bad".into()),
            url: Some(server.uri()),
            ..Default::default()
        };
        let e = OpenAiEmbedder::new(&config).unwrap();
        let result = e.embed_query("test").await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("Invalid API key"));
    }

    /// Verify that configured dimensions are included in the OpenAI embeddings request when set.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_openai_dimensions_in_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(openai_response(vec![vec![0.1, 0.2, 0.3]])),
            )
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::OpenAi,
            api_key: Some("sk-test".into()),
            url: Some(server.uri()),
            dimensions: Some(256),
            ..Default::default()
        };
        let e = OpenAiEmbedder::new(&config).unwrap();
        assert_eq!(e.dimensions(), 256);
        let _ = e.embed_query("test").await.unwrap();
    }

    /// Verify that OpenAiEmbedder auto-detects and caches embedding dimensions from the first response.
    #[tokio::test]
    #[serial_test::serial(flapjack_outbound_url_policy)]
    async fn test_openai_dimensions_auto_detection() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(openai_response(vec![vec![0.1, 0.2, 0.3, 0.4, 0.5]])),
            )
            .mount(&server)
            .await;

        let config = EmbedderConfig {
            source: EmbedderSource::OpenAi,
            api_key: Some("sk-test".into()),
            url: Some(server.uri()),
            // No dimensions configured — should auto-detect
            ..Default::default()
        };
        let e = OpenAiEmbedder::new(&config).unwrap();
        assert_eq!(e.dimensions(), 0); // Before first call
        let _ = e.embed_query("test").await.unwrap();
        assert_eq!(e.dimensions(), 5); // Auto-detected from response
    }
}

/// Stage-1 red test: once validation has approved a hostname, the outbound
/// request path must not re-resolve into loopback for REST embedders.
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn security_dns_rebind_rest_embedder_loopback_sink_not_contacted() {
    let blocked_sink = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "embedding": [0.9, 0.8, 0.7]
        })))
        .mount(&blocked_sink)
        .await;
    let blocked_port = blocked_sink.address().port();

    let config = EmbedderConfig {
        source: EmbedderSource::Rest,
        url: Some(format!(
            "http://vector-dns-rebind.test:{blocked_port}/embed"
        )),
        request: Some(serde_json::json!({"input": "{{text}}"})),
        response: Some(serde_json::json!({"embedding": "{{embedding}}"})),
        dimensions: Some(3),
        ..Default::default()
    };
    config
        .validate()
        .expect("config validation should pass before outbound send");

    let resolver = CountingRebindingResolver::new(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), blocked_port),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), blocked_port),
    );
    let _validation_resolver_guard =
        crate::security::test_helpers::install_test_outbound_host_resolver(Arc::new({
            let resolver = resolver.clone();
            move |_host, _port| Some(vec![resolver.resolve_for_validation()])
        }));
    let _resolver_guard = install_test_dns_resolver(Arc::new(resolver.clone()));

    let embedder = RestEmbedder::new(&config).expect("rest embedder should build");
    let result = embedder.embed_query("hello").await;
    assert!(
        result.is_err(),
        "safe behavior should reject rebound destination instead of contacting loopback sink"
    );
    assert_eq!(
        resolver.call_count(),
        1,
        "safe behavior should avoid a post-validation resolver lookup"
    );
    let sink_requests = blocked_sink.received_requests().await.unwrap_or_default();
    assert!(
        sink_requests.is_empty(),
        "loopback sink must not receive outbound embedding requests"
    );
}

/// Stage-1 red test: metadata rebinding must be blocked before REST outbound send.
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn security_dns_rebind_rest_embedder_metadata_rebind_blocked_before_send() {
    let config = EmbedderConfig {
        source: EmbedderSource::Rest,
        url: Some("http://vector-dns-rebind.test:8081/embed".into()),
        request: Some(serde_json::json!({"input": "{{text}}"})),
        response: Some(serde_json::json!({"embedding": "{{embedding}}"})),
        dimensions: Some(3),
        ..Default::default()
    };
    config
        .validate()
        .expect("config validation should pass before outbound send");

    let resolver = CountingRebindingResolver::new(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 8081),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)), 8081),
    );
    let _validation_resolver_guard =
        crate::security::test_helpers::install_test_outbound_host_resolver(Arc::new({
            let resolver = resolver.clone();
            move |_host, _port| Some(vec![resolver.resolve_for_validation()])
        }));
    let _resolver_guard = install_test_dns_resolver(Arc::new(resolver.clone()));

    let embedder = RestEmbedder::new(&config).expect("rest embedder should build");
    let result = embedder.embed_query("hello").await;
    assert!(
        result.is_err(),
        "safe behavior should block metadata rebound before outbound send"
    );
    assert_eq!(
        resolver.call_count(),
        1,
        "safe behavior should avoid a post-validation resolver lookup"
    );
}

/// Stage-1 red test: once validation has approved a hostname, the outbound
/// OpenAI embedder request path must not re-resolve into loopback.
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn security_dns_rebind_openai_embedder_loopback_sink_not_contacted() {
    let blocked_sink = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [{"object": "embedding", "embedding": [0.1, 0.2, 0.3], "index": 0}],
            "model": "text-embedding-3-small",
            "usage": {"prompt_tokens": 1, "total_tokens": 1}
        })))
        .mount(&blocked_sink)
        .await;
    let blocked_port = blocked_sink.address().port();

    let config = EmbedderConfig {
        source: EmbedderSource::OpenAi,
        api_key: Some("sk-test".into()),
        url: Some(format!("http://vector-dns-rebind.test:{blocked_port}")),
        ..Default::default()
    };
    config
        .validate()
        .expect("config validation should pass before outbound send");

    let resolver = CountingRebindingResolver::new(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), blocked_port),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), blocked_port),
    );
    let _validation_resolver_guard =
        crate::security::test_helpers::install_test_outbound_host_resolver(Arc::new({
            let resolver = resolver.clone();
            move |_host, _port| Some(vec![resolver.resolve_for_validation()])
        }));
    let _resolver_guard = install_test_dns_resolver(Arc::new(resolver.clone()));

    let embedder = OpenAiEmbedder::new(&config).expect("openai embedder should build");
    let result = embedder.embed_query("hello").await;
    assert!(
        result.is_err(),
        "safe behavior should reject rebound destination instead of contacting loopback sink"
    );
    assert_eq!(
        resolver.call_count(),
        1,
        "safe behavior should avoid a post-validation resolver lookup"
    );
    let sink_requests = blocked_sink.received_requests().await.unwrap_or_default();
    assert!(
        sink_requests.is_empty(),
        "loopback sink must not receive outbound OpenAI embedding requests"
    );
}

/// Stage-1 red test: metadata rebinding must be blocked before OpenAI outbound send.
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn security_dns_rebind_openai_embedder_metadata_rebind_blocked_before_send() {
    let config = EmbedderConfig {
        source: EmbedderSource::OpenAi,
        api_key: Some("sk-test".into()),
        url: Some("http://vector-dns-rebind.test:8082".into()),
        ..Default::default()
    };
    config
        .validate()
        .expect("config validation should pass before outbound send");

    let resolver = CountingRebindingResolver::new(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 8082),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)), 8082),
    );
    let _validation_resolver_guard =
        crate::security::test_helpers::install_test_outbound_host_resolver(Arc::new({
            let resolver = resolver.clone();
            move |_host, _port| Some(vec![resolver.resolve_for_validation()])
        }));
    let _resolver_guard = install_test_dns_resolver(Arc::new(resolver.clone()));

    let embedder = OpenAiEmbedder::new(&config).expect("openai embedder should build");
    let result = embedder.embed_query("hello").await;
    assert!(
        result.is_err(),
        "safe behavior should block metadata rebound before outbound send"
    );
    assert_eq!(
        resolver.call_count(),
        1,
        "safe behavior should avoid a post-validation resolver lookup"
    );
}

/// TODO: Document security_rest_embedder_pins_to_vetted_resolution_addresses.
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn security_rest_embedder_pins_to_vetted_resolution_addresses() {
    let sink = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "embedding": [0.1, 0.2, 0.3]
        })))
        .mount(&sink)
        .await;
    let sink_port = sink.address().port();
    let hostname = "vector-pin-rest.test";
    let url = format!("http://{hostname}:{sink_port}/embed");
    let _allow_local_guard = crate::security::test_helpers::AllowLocalUrlsGuard::set("1");
    let _validation_resolver_guard =
        crate::security::test_helpers::install_test_outbound_host_resolver(Arc::new({
            let host = hostname.to_string();
            move |lookup_host, lookup_port| {
                if lookup_host == host && lookup_port == Some(sink_port) {
                    return Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
                }
                None
            }
        }));

    let config = EmbedderConfig {
        source: EmbedderSource::Rest,
        url: Some(url),
        request: Some(serde_json::json!({"input": "{{text}}"})),
        response: Some(serde_json::json!({"embedding": "{{embedding}}"})),
        dimensions: Some(3),
        ..Default::default()
    };
    config.validate().expect("validation should pass");

    let embedder = RestEmbedder::new(&config).expect("embedder should build with pinned addresses");
    let embedding = embedder
        .embed_query("hello")
        .await
        .expect("request should use pinned localhost address");
    assert_eq!(embedding, vec![0.1, 0.2, 0.3]);
}

/// TODO: Document security_openai_embedder_pins_to_vetted_resolution_addresses.
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn security_openai_embedder_pins_to_vetted_resolution_addresses() {
    let sink = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [{"object": "embedding", "embedding": [0.11, 0.22, 0.33], "index": 0}],
            "model": "text-embedding-3-small",
            "usage": {"prompt_tokens": 1, "total_tokens": 1}
        })))
        .mount(&sink)
        .await;
    let sink_port = sink.address().port();
    let hostname = "vector-pin-openai.test";
    let url = format!("http://{hostname}:{sink_port}");
    let _allow_local_guard = crate::security::test_helpers::AllowLocalUrlsGuard::set("1");
    let _validation_resolver_guard =
        crate::security::test_helpers::install_test_outbound_host_resolver(Arc::new({
            let host = hostname.to_string();
            move |lookup_host, lookup_port| {
                if lookup_host == host && lookup_port == Some(sink_port) {
                    return Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
                }
                None
            }
        }));

    let config = EmbedderConfig {
        source: EmbedderSource::OpenAi,
        api_key: Some("sk-test".into()),
        url: Some(url),
        ..Default::default()
    };
    config.validate().expect("validation should pass");

    let embedder =
        OpenAiEmbedder::new(&config).expect("embedder should build with pinned addresses");
    let embedding = embedder
        .embed_query("hello")
        .await
        .expect("request should use pinned localhost address");
    assert_eq!(embedding, vec![0.11, 0.22, 0.33]);
}

// ── FastEmbedEmbedder tests (9.7) ──

#[cfg(feature = "vector-search-local")]
mod fastembed_tests {
    use super::*;
    use serial_test::serial;

    // ── Model lookup tests ──

    #[test]
    fn test_parse_embedding_model_default() {
        let model = parse_embedding_model(None).unwrap();
        assert!(matches!(model, fastembed::EmbeddingModel::BGESmallENV15));
    }

    #[test]
    fn test_parse_embedding_model_known() {
        let model = parse_embedding_model(Some("all-MiniLM-L6-v2")).unwrap();
        assert!(matches!(model, fastembed::EmbeddingModel::AllMiniLML6V2));
    }

    #[test]
    fn test_parse_embedding_model_case_insensitive() {
        let model = parse_embedding_model(Some("BGE-Small-EN-V1.5")).unwrap();
        assert!(matches!(model, fastembed::EmbeddingModel::BGESmallENV15));
    }

    /// Verify that all supported fastembed models parse correctly and match expected enum variants.
    #[test]
    fn test_parse_embedding_model_all_supported() {
        let cases = vec![
            (
                "bge-small-en-v1.5",
                fastembed::EmbeddingModel::BGESmallENV15,
            ),
            ("bge-base-en-v1.5", fastembed::EmbeddingModel::BGEBaseENV15),
            (
                "bge-large-en-v1.5",
                fastembed::EmbeddingModel::BGELargeENV15,
            ),
            ("all-MiniLM-L6-v2", fastembed::EmbeddingModel::AllMiniLML6V2),
            (
                "all-MiniLM-L12-v2",
                fastembed::EmbeddingModel::AllMiniLML12V2,
            ),
            (
                "nomic-embed-text-v1.5",
                fastembed::EmbeddingModel::NomicEmbedTextV15,
            ),
            (
                "multilingual-e5-small",
                fastembed::EmbeddingModel::MultilingualE5Small,
            ),
        ];
        for (input, expected) in cases {
            let result = parse_embedding_model(Some(input)).unwrap();
            assert_eq!(
                std::mem::discriminant(&result),
                std::mem::discriminant(&expected),
                "failed for input: {input}"
            );
        }
    }

    /// Verify that unknown model names produce an error mentioning the invalid model and listing valid options.
    #[test]
    fn test_parse_embedding_model_unknown() {
        let result = parse_embedding_model(Some("nonexistent-model"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            VectorError::EmbeddingError(msg) => {
                assert!(
                    msg.contains("nonexistent-model"),
                    "error should mention the invalid model"
                );
                assert!(
                    msg.contains("bge-small-en-v1.5"),
                    "error should list valid models"
                );
            }
            other => panic!("expected EmbeddingError, got: {other}"),
        }
    }

    // ── Embedder behavior tests ──

    fn fastembed_test_config() -> EmbedderConfig {
        EmbedderConfig {
            source: EmbedderSource::FastEmbed,
            // Default model (bge-small-en-v1.5, 384d)
            ..Default::default()
        }
    }

    #[test]
    // Concurrent ONNX model cache initialization can race and flake with
    // "Failed to retrieve onnx/model.onnx" when these tests run in parallel.
    #[serial]
    fn test_fastembed_dimensions_from_model() {
        let e = FastEmbedEmbedder::new(&fastembed_test_config()).unwrap();
        assert_eq!(e.dimensions(), 384);
    }

    #[test]
    #[serial]
    fn test_fastembed_source_returns_fastembed() {
        let e = FastEmbedEmbedder::new(&fastembed_test_config()).unwrap();
        assert_eq!(e.source(), EmbedderSource::FastEmbed);
    }

    #[tokio::test]
    #[serial]
    async fn test_fastembed_embed_documents() {
        let e = FastEmbedEmbedder::new(&fastembed_test_config()).unwrap();
        let texts = &["hello world", "rust programming", "vector search"];
        let results = e.embed_documents(texts).await.unwrap();
        assert_eq!(results.len(), 3);
        for vec in &results {
            assert_eq!(vec.len(), 384, "each vector should be 384-dim");
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_fastembed_embed_query() {
        let e = FastEmbedEmbedder::new(&fastembed_test_config()).unwrap();
        let result = e.embed_query("hello world").await.unwrap();
        assert_eq!(result.len(), 384);
    }

    #[tokio::test]
    #[serial]
    async fn test_fastembed_embed_deterministic() {
        let e = FastEmbedEmbedder::new(&fastembed_test_config()).unwrap();
        let v1 = e.embed_query("deterministic test").await.unwrap();
        let v2 = e.embed_query("deterministic test").await.unwrap();
        assert_eq!(v1, v2, "same input should produce identical vectors");
    }

    #[tokio::test]
    #[serial]
    async fn test_fastembed_embed_empty_batch() {
        let e = FastEmbedEmbedder::new(&fastembed_test_config()).unwrap();
        let results = e.embed_documents(&[]).await.unwrap();
        assert!(results.is_empty());
    }

    /// Verify that FastEmbedEmbedder rejects configuration when dimensions don't match the model's actual dimensions.
    #[test]
    fn test_fastembed_dimension_mismatch_in_new() {
        let config = EmbedderConfig {
            source: EmbedderSource::FastEmbed,
            // bge-small-en-v1.5 is 384d, but we claim 768
            dimensions: Some(768),
            ..Default::default()
        };
        let result = FastEmbedEmbedder::new(&config);
        assert!(result.is_err());
        match result.unwrap_err() {
            VectorError::EmbeddingError(msg) => {
                assert!(
                    msg.contains("384"),
                    "error should mention actual dimensions"
                );
                assert!(
                    msg.contains("768"),
                    "error should mention configured dimensions"
                );
            }
            other => panic!("expected EmbeddingError, got: {other}"),
        }
    }

    #[test]
    #[serial]
    fn test_factory_creates_fastembed() {
        let config = EmbedderConfig {
            source: EmbedderSource::FastEmbed,
            ..Default::default()
        };
        let embedder = create_embedder(&config).unwrap();
        assert_eq!(embedder.source(), EmbedderSource::FastEmbed);
        assert_eq!(embedder.dimensions(), 384);
    }
}

// Test the error path when vector-search-local is NOT enabled
/// Verify that the factory function rejects FastEmbed source when compiled without the `vector-search-local` feature.
#[cfg(not(feature = "vector-search-local"))]
#[test]
fn test_factory_fastembed_rejected_without_feature() {
    let config = EmbedderConfig {
        source: EmbedderSource::FastEmbed,
        ..Default::default()
    };
    let result = create_embedder(&config);
    assert!(result.is_err());
    match result.unwrap_err() {
        VectorError::EmbeddingError(msg) => {
            assert!(
                msg.contains("vector-search-local"),
                "error should mention the required feature flag, got: {msg}"
            );
        }
        other => panic!("expected EmbeddingError, got: {other}"),
    }
}
