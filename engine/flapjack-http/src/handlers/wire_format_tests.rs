/// Stage 2: SDK Wire Format Verification Tests
///
/// These are confirmation/regression-lock tests for protocol-level behavior that is
/// already implemented. Any RED test indicates a real bug to fix.
#[cfg(test)]
mod tests {
    use crate::auth::{ApiKey, KeyStore, RateLimiter};
    use crate::handlers::search::{batch_search, search, search_get};
    use crate::handlers::settings::get_settings;
    use crate::handlers::AppState;
    use crate::middleware::{ensure_json_errors, normalize_content_type, TrustedProxyMatcher};
    use crate::router::build_cors_layer;
    use crate::startup::CorsMode;
    use crate::test_helpers::body_json;
    use axum::{
        body::Body,
        extract::ConnectInfo,
        http::{Method, Request, StatusCode},
        middleware,
        routing::{get, post},
        Router,
    };
    use flapjack::types::{Document, FieldValue};
    use serde_json::json;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tower::ServiceExt;

    const TEST_ADMIN_KEY: &str = "test-admin-key-wire-format";

    // ──────────────────────────────────────────────────────────────────────────
    // Test infrastructure
    // ──────────────────────────────────────────────────────────────────────────

    fn full_stack_router(
        state: Arc<AppState>,
        key_store: Arc<KeyStore>,
        rate_limiter: RateLimiter,
    ) -> Router {
        let ks = key_store.clone();
        let rl = rate_limiter.clone();
        let auth_middleware = middleware::from_fn(
            move |mut request: axum::extract::Request, next: middleware::Next| {
                let ks_clone = ks.clone();
                let rl_clone = rl.clone();
                async move {
                    request.extensions_mut().insert(ks_clone);
                    request.extensions_mut().insert(rl_clone);
                    crate::auth::authenticate_and_authorize(request, next, false).await
                }
            },
        );

        // In tower oneshot tests there is no real socket, so ConnectInfo is absent.
        // Inject a loopback peer address and the default TrustedProxyMatcher (which
        // trusts loopback CIDRs) so that X-Forwarded-For is honoured by
        // extract_client_ip — the same semantics as a production server sitting
        // behind a trusted local proxy.
        let trusted_matcher = Arc::new(
            TrustedProxyMatcher::from_optional_csv(None).expect("default trusted proxy matcher"),
        );
        let peer_info_middleware = middleware::from_fn(
            move |mut request: axum::extract::Request, next: middleware::Next| {
                let matcher = trusted_matcher.clone();
                async move {
                    let loopback: SocketAddr = ([127, 0, 0, 1], 0u16).into();
                    request.extensions_mut().insert(ConnectInfo(loopback));
                    request.extensions_mut().insert(matcher);
                    next.run(request).await
                }
            },
        );

        Router::new()
            .route("/1/indexes/:indexName/query", post(search))
            .route("/1/indexes/:indexName", get(search_get))
            .route("/1/indexes/:indexName/queries", post(batch_search))
            .route("/1/indexes/:indexName/settings", get(get_settings))
            .with_state(state)
            .layer(auth_middleware)
            .layer(middleware::from_fn(normalize_content_type))
            .layer(middleware::from_fn(ensure_json_errors))
            .layer(peer_info_middleware)
            .layer(build_cors_layer(&CorsMode::LoopbackOnly))
    }

    /// Create a search-only API key with a specified per-IP rate limit and return its plaintext value.
    ///
    /// # Arguments
    ///
    /// * `key_store` — the `KeyStore` in which to persist the new key.
    /// * `max_queries_per_ip_per_hour` — hourly per-IP query cap; set to `0` for unlimited.
    ///
    /// # Returns
    ///
    /// The plaintext API key string suitable for use in `x-algolia-api-key` headers.
    fn make_search_key(key_store: &KeyStore, max_queries_per_ip_per_hour: i64) -> String {
        let key = ApiKey {
            hash: String::new(),
            salt: String::new(),
            hmac_key: None,
            created_at: 0,
            acl: vec!["search".into()],
            description: "test search key".into(),
            indexes: vec![],
            max_hits_per_query: 0,
            max_queries_per_ip_per_hour,
            query_parameters: String::new(),
            referers: vec![],
            restrict_sources: None,
            validity: 0,
        };
        let (_stored_key, plaintext) = key_store.create_key(key);
        plaintext
    }

    fn authed_post(uri: &str, api_key: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("x-algolia-application-id", "test-app")
            .header("x-algolia-api-key", api_key)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn authed_post_with_xff(uri: &str, api_key: &str, body: &str, xff: &str) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("x-algolia-application-id", "test-app")
            .header("x-algolia-api-key", api_key)
            .header("x-forwarded-for", xff)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// Populate an index with two minimal documents ("red shoes" and "blue shoes") for search tests.
    ///
    /// Creates the tenant if it does not already exist and synchronously indexes
    /// two `Document` values with `title` fields so that query-based assertions
    /// have deterministic data to match against.
    ///
    /// # Arguments
    ///
    /// * `state` — shared `AppState` whose `IndexManager` receives the documents.
    /// * `index_name` — logical index name to create and populate.
    async fn seed_docs(state: &AppState, index_name: &str) {
        let _ = state.manager.create_tenant(index_name);
        let mut fields = std::collections::HashMap::new();
        fields.insert(
            "title".to_string(),
            FieldValue::Text("red shoes".to_string()),
        );
        let doc1 = Document {
            id: "1".to_string(),
            fields: fields.clone(),
        };
        let mut fields2 = std::collections::HashMap::new();
        fields2.insert(
            "title".to_string(),
            FieldValue::Text("blue shoes".to_string()),
        );
        let doc2 = Document {
            id: "2".to_string(),
            fields: fields2,
        };
        state
            .manager
            .add_documents_sync(index_name, vec![doc1, doc2])
            .await
            .unwrap();
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Pre-flight
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn preflight_build_check() {
        // Sanity test: full_stack_router can be constructed without panicking.
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let _app = full_stack_router(state, key_store, rate_limiter);
    }

    #[test]
    fn batch_handler_does_not_inline_invalid_credentials_literal() {
        let batch_handler_source = include_str!("search/batch.rs");
        assert!(
            !batch_handler_source.contains("\"Invalid Application-ID or API key\""),
            "batch handler must use auth invalid-credential helper instead of local literals"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.1 Header: X-Algolia-Application-Id is required
    // ──────────────────────────────────────────────────────────────────────────

    /// Verify that the `X-Algolia-Application-Id` header is mandatory for authenticated endpoints.
    ///
    /// Asserts that omitting the header yields 403 and that including it
    /// (alongside a valid admin key) yields 200.
    #[tokio::test]
    async fn x_algolia_application_id_required() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state.clone(), key_store.clone(), rate_limiter);

        seed_docs(&state, "products").await;

        // Without X-Algolia-Application-Id → 403
        let resp_no_app_id = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/1/indexes/products/query")
                    .header("x-algolia-api-key", TEST_ADMIN_KEY)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"query": ""}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp_no_app_id.status(),
            StatusCode::FORBIDDEN,
            "missing x-algolia-application-id should return 403"
        );

        // With X-Algolia-Application-Id + valid admin key → 200
        let resp_ok = app
            .clone()
            .oneshot(authed_post(
                "/1/indexes/products/query",
                TEST_ADMIN_KEY,
                &json!({"query": ""}).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp_ok.status(),
            StatusCode::OK,
            "valid headers should return 200"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.2 Header: X-Algolia-UserToken captured without error
    // ──────────────────────────────────────────────────────────────────────────

    /// Confirm that the `X-Algolia-UserToken` header is accepted without error and does not interfere with normal search results.
    ///
    /// Sends a search request with `x-algolia-usertoken: user_abc` and asserts
    /// that the response is 200 with a well-formed `hits` array.
    #[tokio::test]
    async fn x_algolia_user_token_header_captured_in_search() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state.clone(), key_store, rate_limiter);

        seed_docs(&state, "products").await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/1/indexes/products/query")
                    .header("x-algolia-application-id", "test-app")
                    .header("x-algolia-api-key", TEST_ADMIN_KEY)
                    .header("x-algolia-usertoken", "user_abc")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"query": ""}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "X-Algolia-UserToken should not cause errors"
        );
        let json = body_json(resp).await;
        assert!(
            json["hits"].is_array(),
            "response should have hits array, got: {}",
            json
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.3 Header: X-Forwarded-For used for rate limiting
    // ──────────────────────────────────────────────────────────────────────────

    /// Verify that `X-Forwarded-For` determines the client IP for per-IP rate limiting.
    ///
    /// Creates a key limited to 2 queries per IP per hour, exhausts the limit
    /// from IP `1.2.3.4`, then confirms that the third request from the same IP
    /// receives 429 while a request from a different IP (`5.6.7.8`) succeeds.
    #[tokio::test]
    async fn x_forwarded_for_used_for_rate_limiting() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state.clone(), key_store.clone(), rate_limiter);

        seed_docs(&state, "products").await;

        // Create a key with maxQueriesPerIPPerHour = 2
        let rate_limited_key = make_search_key(&key_store, 2);

        let search_body = json!({"query": ""}).to_string();

        // Request 1 → 200
        let resp1 = app
            .clone()
            .oneshot(authed_post_with_xff(
                "/1/indexes/products/query",
                &rate_limited_key,
                &search_body,
                "1.2.3.4",
            ))
            .await
            .unwrap();
        assert_eq!(resp1.status(), StatusCode::OK, "request 1 should succeed");

        // Request 2 → 200
        let resp2 = app
            .clone()
            .oneshot(authed_post_with_xff(
                "/1/indexes/products/query",
                &rate_limited_key,
                &search_body,
                "1.2.3.4",
            ))
            .await
            .unwrap();
        assert_eq!(resp2.status(), StatusCode::OK, "request 2 should succeed");

        // Request 3 → 429 (limit exceeded for 1.2.3.4)
        let resp3 = app
            .clone()
            .oneshot(authed_post_with_xff(
                "/1/indexes/products/query",
                &rate_limited_key,
                &search_body,
                "1.2.3.4",
            ))
            .await
            .unwrap();
        assert_eq!(
            resp3.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "request 3 from same IP should be rate limited"
        );

        // Request from different IP → 200 (different rate limit bucket)
        let resp_other_ip = app
            .clone()
            .oneshot(authed_post_with_xff(
                "/1/indexes/products/query",
                &rate_limited_key,
                &search_body,
                "5.6.7.8",
            ))
            .await
            .unwrap();
        assert_eq!(
            resp_other_ip.status(),
            StatusCode::OK,
            "different IP should not be rate limited"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.4 CORS: OPTIONS preflight enforces loopback-default contract
    // ──────────────────────────────────────────────────────────────────────────

    /// Assert that default CORS mode allows loopback origins and blocks non-loopback origins.
    #[tokio::test]
    async fn cors_preflight_respects_loopback_default_contract() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state, key_store, rate_limiter);

        let loopback_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/1/indexes/products/query")
                    .header("origin", "http://127.0.0.1:3000")
                    .header("access-control-request-method", "POST")
                    .header("access-control-request-headers", "content-type")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let allow_origin = loopback_resp
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(
            allow_origin, "http://127.0.0.1:3000",
            "default CORS mode should allow loopback browser origins"
        );
        let allow_methods = loopback_resp
            .headers()
            .get("access-control-allow-methods")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            allow_methods == "*" || allow_methods.contains("POST"),
            "preflight allow methods should include POST or wildcard, got: {allow_methods}"
        );

        let blocked_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/1/indexes/products/query")
                    .header("origin", "https://example.com")
                    .header("access-control-request-method", "POST")
                    .header("access-control-request-headers", "content-type")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            blocked_resp
                .headers()
                .get("access-control-allow-origin")
                .is_none(),
            "default CORS mode must not allow non-loopback origins"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.5 Params string: single search decoded correctly
    // ──────────────────────────────────────────────────────────────────────────

    /// Confirm that a URL-encoded `params` string in a single-index search request body is decoded and applied.
    ///
    /// Sends `params: "query=shoes&hitsPerPage=1"` and asserts the response
    /// contains exactly one hit and reports `hitsPerPage: 1`.
    #[tokio::test]
    async fn params_string_in_single_search_decoded_correctly() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state.clone(), key_store, rate_limiter);

        seed_docs(&state, "products").await;

        // params string "hitsPerPage=1" should limit hits to 1
        let resp = app
            .clone()
            .oneshot(authed_post(
                "/1/indexes/products/query",
                TEST_ADMIN_KEY,
                &json!({"params": "query=shoes&hitsPerPage=1"}).to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let hits = json["hits"].as_array().expect("hits should be array");
        assert_eq!(
            hits.len(),
            1,
            "hitsPerPage=1 from params string should limit to 1 hit, got: {}",
            json
        );
        assert_eq!(
            json["hitsPerPage"].as_i64(),
            Some(1),
            "hitsPerPage in response should be 1"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.6 Params string: batch search uses per-query params
    // ──────────────────────────────────────────────────────────────────────────

    /// Confirm that per-query `params` strings in a multi-index batch search request are decoded and applied independently.
    ///
    /// Sends two queries with `hitsPerPage=1` and `hitsPerPage=2` respectively,
    /// then asserts each result set respects its own page-size limit.
    #[tokio::test]
    async fn params_string_in_batch_search_decoded_correctly() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state.clone(), key_store, rate_limiter);

        seed_docs(&state, "products").await;

        let resp = app
            .clone()
            .oneshot(authed_post(
                "/1/indexes/*/queries",
                TEST_ADMIN_KEY,
                &json!({
                    "requests": [
                        {"indexName": "products", "params": "query=shoes&hitsPerPage=1"},
                        {"indexName": "products", "params": "query=shoes&hitsPerPage=2"}
                    ]
                })
                .to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let results = json["results"].as_array().expect("results should be array");
        assert_eq!(results.len(), 2, "should have 2 results");

        let hits0 = results[0]["hits"]
            .as_array()
            .expect("hits[0] should be array");
        let hits1 = results[1]["hits"]
            .as_array()
            .expect("hits[1] should be array");
        assert_eq!(
            hits0.len(),
            1,
            "first query hitsPerPage=1 should give 1 hit"
        );
        assert!(
            hits1.len() <= 2,
            "second query hitsPerPage=2 should give at most 2 hits"
        );
        assert!(
            hits1.len() >= hits0.len(),
            "second query should have at least as many hits as first"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.7 Params string: URL-encoded brackets for facets decoded correctly
    // ──────────────────────────────────────────────────────────────────────────

    /// Verify that URL-encoded bracket characters in a `params` string (e.g. `%5B`, `%5D` for `[`, `]`) are correctly decoded.
    ///
    /// Sends `facets=%5B%22brand%22%5D` (decodes to `facets=["brand"]`) and
    /// asserts the response contains canonical facet params and activates facet
    /// processing metadata such as `exhaustiveFacetsCount`.
    #[tokio::test]
    async fn params_string_with_url_encoded_brackets() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state.clone(), key_store, rate_limiter);

        // Seed docs with brand facet
        let _ = state.manager.create_tenant("products");
        let mut fields = std::collections::HashMap::new();
        fields.insert("brand".to_string(), FieldValue::Facet("Nike".to_string()));
        fields.insert("title".to_string(), FieldValue::Text("shoe".to_string()));
        state
            .manager
            .add_documents_sync(
                "products",
                vec![Document {
                    id: "1".to_string(),
                    fields,
                }],
            )
            .await
            .unwrap();

        // facets=%5B%22brand%22%5D decodes to facets=["brand"]
        let resp = app
            .clone()
            .oneshot(authed_post(
                "/1/indexes/products/query",
                TEST_ADMIN_KEY,
                &json!({"params": "query=shoe&facets=%5B%22brand%22%5D"}).to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "URL-encoded facets param should be accepted"
        );
        // Ensure decoded facets value is actually applied by checking it survives into
        // canonical params output and enables facet-exhaustiveness metadata.
        let json = body_json(resp).await;
        let params = json["params"].as_str().unwrap_or_default();
        assert!(
            params.contains("facets=%5B%22brand%22%5D"),
            "decoded facets should be present in canonical params, got: {json}"
        );
        assert_eq!(
            json["exhaustiveFacetsCount"].as_bool(),
            Some(true),
            "decoded facets should activate facet processing flags, got: {json}"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.8 Params string: overrides body fields (params takes precedence)
    // ──────────────────────────────────────────────────────────────────────────

    /// Assert that values in the `params` query string take precedence over top-level body fields.
    ///
    /// Sends a request with `query: "oldquery"` in the JSON body and
    /// `query=newquery` in the `params` string, then verifies only the document
    /// matching "newquery" appears in the results.
    #[tokio::test]
    async fn params_string_overrides_body_fields() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state.clone(), key_store, rate_limiter);

        // Seed docs: only "new_query_match" matches "newquery"
        let _ = state.manager.create_tenant("products");
        let mut fields1 = std::collections::HashMap::new();
        fields1.insert(
            "title".to_string(),
            FieldValue::Text("oldquery_match".to_string()),
        );
        let mut fields2 = std::collections::HashMap::new();
        fields2.insert(
            "title".to_string(),
            FieldValue::Text("newquery_match".to_string()),
        );
        state
            .manager
            .add_documents_sync(
                "products",
                vec![
                    Document {
                        id: "1".to_string(),
                        fields: fields1,
                    },
                    Document {
                        id: "2".to_string(),
                        fields: fields2,
                    },
                ],
            )
            .await
            .unwrap();

        // body has query="oldquery", params has query="newquery" — params wins
        let resp = app
            .clone()
            .oneshot(authed_post(
                "/1/indexes/products/query",
                TEST_ADMIN_KEY,
                &json!({"query": "oldquery", "params": "query=newquery&hitsPerPage=10"})
                    .to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let hits = json["hits"].as_array().expect("hits should be array");

        // params string should override body query → only doc 2 matches "newquery"
        assert!(
            hits.iter().any(|h| h["objectID"].as_str() == Some("2")),
            "params query='newquery' should match doc 2, got hits: {}",
            json["hits"]
        );
        assert!(
            !hits.iter().any(|h| h["objectID"].as_str() == Some("1")),
            "body query='oldquery' should be overridden, doc 1 should not appear"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.9 Error format: malformed JSON → { message, status: 400 }
    // ──────────────────────────────────────────────────────────────────────────

    /// Verify that a malformed JSON request body produces a 400 response with the standard Algolia-compatible error envelope (`{message, status}`) and `application/json` content type.
    #[tokio::test]
    async fn error_response_has_message_and_status_fields() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state, key_store, rate_limiter);

        // Send malformed JSON body
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/1/indexes/products/query")
                    .header("x-algolia-application-id", "test-app")
                    .header("x-algolia-api-key", TEST_ADMIN_KEY)
                    .header("content-type", "application/json")
                    .body(Body::from("{not valid json"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("application/json"),
            "error response should be application/json, got: {}",
            ct
        );

        let json = body_json(resp).await;
        assert!(
            json["message"].is_string(),
            "error should have 'message' field, got: {}",
            json
        );
        assert!(
            json["status"].is_number(),
            "error should have 'status' field, got: {}",
            json
        );
        assert_eq!(
            json["status"].as_i64(),
            Some(400),
            "malformed JSON should return status=400 in body"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.10 Error format: 404 for unknown route is JSON not plain text
    // ──────────────────────────────────────────────────────────────────────────

    /// Ensure that requests to unknown routes return a 404 response with `application/json` content type and the standard `{message, status}` error envelope.
    ///
    /// Guards against the default plain-text "Not Found" body that frameworks
    /// typically emit for unmatched routes.
    #[tokio::test]
    async fn not_found_error_is_json_not_plain_text() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state, key_store, rate_limiter);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/1/nonexistent/route/that/does/not/exist")
                    .header("x-algolia-application-id", "test-app")
                    .header("x-algolia-api-key", TEST_ADMIN_KEY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "unknown route should return 404"
        );
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("application/json"),
            "404 should be application/json, got: {}",
            ct
        );

        let json = body_json(resp).await;
        assert!(
            json["message"].is_string(),
            "404 should have 'message' field, got: {}",
            json
        );
        assert_eq!(
            json["status"].as_i64(),
            Some(404),
            "404 JSON status field should be 404"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.11 Error format: invalid API key → 403 JSON
    // ──────────────────────────────────────────────────────────────────────────

    /// Verify that an unrecognised API key produces a 403 JSON error with `message` and `status` fields.
    ///
    /// Uses the key `"totally-invalid-key-xyz"` to confirm the auth middleware
    /// rejects unknown credentials with a well-formed error envelope.
    #[tokio::test]
    async fn invalid_api_key_returns_403_json() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state, key_store, rate_limiter);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/1/indexes/products/query")
                    .header("x-algolia-application-id", "test-app")
                    .header("x-algolia-api-key", "totally-invalid-key-xyz")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"query": ""}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "invalid API key should return 403"
        );
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("application/json"),
            "403 response should be application/json, got: {}",
            ct
        );

        let json = body_json(resp).await;
        assert!(
            json["message"].is_string(),
            "403 should have 'message' field"
        );
        assert_eq!(json["status"].as_i64(), Some(403));
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.12 Error format: valid key, insufficient ACL → 403 JSON
    // ──────────────────────────────────────────────────────────────────────────

    /// Verify that a valid API key lacking the required ACL for an endpoint produces a 403 JSON error.
    ///
    /// Creates a search-only key and uses it to request the settings endpoint
    /// (which requires the `settings` ACL), asserting that the response is 403
    /// with a JSON body containing `message` and `status` fields.
    #[tokio::test]
    async fn insufficient_acl_returns_403_json() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state, key_store.clone(), rate_limiter);

        // Create a search-only key
        let search_only_key = make_search_key(&key_store, 0);

        // Try to GET settings (requires "settings" ACL) with search-only key → 403
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/1/indexes/products/settings")
                    .header("x-algolia-application-id", "test-app")
                    .header("x-algolia-api-key", &search_only_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "search-only key hitting settings endpoint should return 403"
        );
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("application/json"),
            "403 response should be application/json, got: {}",
            ct
        );

        let json = body_json(resp).await;
        assert!(
            json["message"].is_string(),
            "403 should have 'message' field"
        );
        assert_eq!(json["status"].as_i64(), Some(403));
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.13 Error format: rate limit exceeded → 429 JSON
    // ──────────────────────────────────────────────────────────────────────────

    /// Confirm that a 429 rate-limit response uses `application/json` content type and includes the standard `{message, status}` error envelope.
    ///
    /// Creates a key limited to 1 query per IP per hour, exhausts it, and
    /// inspects the subsequent 429 response format.
    #[tokio::test]
    async fn rate_limit_error_format_is_json() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state.clone(), key_store.clone(), rate_limiter);

        seed_docs(&state, "products").await;

        let limited_key = make_search_key(&key_store, 1);
        let body = json!({"query": ""}).to_string();

        // First request → 200
        let _ = app
            .clone()
            .oneshot(authed_post_with_xff(
                "/1/indexes/products/query",
                &limited_key,
                &body,
                "1.2.3.4",
            ))
            .await
            .unwrap();

        // Second request → 429
        let resp = app
            .clone()
            .oneshot(authed_post_with_xff(
                "/1/indexes/products/query",
                &limited_key,
                &body,
                "1.2.3.4",
            ))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "should be rate limited after 1 request"
        );
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("application/json"),
            "429 response should be application/json, got: {}",
            ct
        );

        let json = body_json(resp).await;
        assert!(
            json["message"].is_string(),
            "429 should have 'message' field"
        );
        assert_eq!(json["status"].as_i64(), Some(429));
    }

    // ──────────────────────────────────────────────────────────────────────────
    // T4.14 Connection: 20 rapid sequential requests all return 200
    // ──────────────────────────────────────────────────────────────────────────

    /// Verify that 20 back-to-back search requests against the same router all return 200 with a valid `hits` array.
    ///
    /// Guards against connection-handling regressions such as socket exhaustion,
    /// shared-state corruption under reuse, or accidental single-use router
    /// configurations that would cause later requests to fail.
    #[tokio::test]
    async fn rapid_sequential_requests_handled_gracefully() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), TEST_ADMIN_KEY));
        let rate_limiter = RateLimiter::new();
        let app = full_stack_router(state.clone(), key_store, rate_limiter);

        seed_docs(&state, "products").await;

        let body = json!({"query": ""}).to_string();

        for i in 0..20 {
            let resp = app
                .clone()
                .oneshot(authed_post(
                    "/1/indexes/products/query",
                    TEST_ADMIN_KEY,
                    &body,
                ))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "request {} should return 200",
                i
            );
            let json = body_json(resp).await;
            assert!(
                json["hits"].is_array(),
                "request {} response should have hits array",
                i
            );
        }
    }
}
