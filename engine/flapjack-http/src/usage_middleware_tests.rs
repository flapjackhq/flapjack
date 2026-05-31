use super::*;
use axum::http::Method;

// ── extract_index_name ──

#[test]
fn extract_from_query_path() {
    assert_eq!(
        extract_index_name("/1/indexes/products/query"),
        Some("products".to_string())
    );
}

#[test]
fn extract_from_batch_path() {
    assert_eq!(
        extract_index_name("/1/indexes/my-idx/batch"),
        Some("my-idx".to_string())
    );
}

#[test]
fn extract_from_object_id_path() {
    assert_eq!(
        extract_index_name("/1/indexes/products/abc123"),
        Some("products".to_string())
    );
}

#[test]
fn extract_from_index_root() {
    assert_eq!(
        extract_index_name("/1/indexes/products"),
        Some("products".to_string())
    );
}

#[test]
fn extract_none_for_health() {
    assert_eq!(extract_index_name("/health"), None);
}

#[test]
fn extract_none_for_metrics() {
    assert_eq!(extract_index_name("/metrics"), None);
}

#[test]
fn extract_none_for_keys() {
    assert_eq!(extract_index_name("/1/keys"), None);
}

#[test]
fn extract_none_for_internal() {
    assert_eq!(extract_index_name("/internal/status"), None);
}

#[test]
fn extract_none_for_analytics() {
    assert_eq!(extract_index_name("/2/searches"), None);
}

// ── classify_request ──

#[test]
fn classify_search_query() {
    assert_eq!(
        classify_request(&Method::POST, "/1/indexes/products/query"),
        Some(RequestKind::Search)
    );
}

#[test]
fn classify_search_queries() {
    assert_eq!(
        classify_request(&Method::POST, "/1/indexes/products/queries"),
        Some(RequestKind::Search)
    );
}

#[test]
fn classify_write_batch() {
    assert_eq!(
        classify_request(&Method::POST, "/1/indexes/products/batch"),
        Some(RequestKind::Write)
    );
}

#[test]
fn classify_write_put_object() {
    assert_eq!(
        classify_request(&Method::PUT, "/1/indexes/products/abc123"),
        Some(RequestKind::Write)
    );
}

#[test]
fn classify_write_delete_object() {
    assert_eq!(
        classify_request(&Method::DELETE, "/1/indexes/products/abc123"),
        Some(RequestKind::Write)
    );
}

#[test]
fn classify_write_delete_by_query() {
    assert_eq!(
        classify_request(&Method::POST, "/1/indexes/products/deleteByQuery"),
        Some(RequestKind::Write)
    );
}

#[test]
fn classify_write_post_to_index_root() {
    assert_eq!(
        classify_request(&Method::POST, "/1/indexes/products"),
        Some(RequestKind::Write)
    );
}

#[test]
fn classify_read_get_object() {
    assert_eq!(
        classify_request(&Method::GET, "/1/indexes/products/abc123"),
        Some(RequestKind::Read)
    );
}

#[test]
fn classify_read_post_objects() {
    assert_eq!(
        classify_request(&Method::POST, "/1/indexes/products/objects"),
        Some(RequestKind::Read)
    );
}

#[test]
fn classify_read_post_browse() {
    assert_eq!(
        classify_request(&Method::POST, "/1/indexes/products/browse"),
        Some(RequestKind::Read)
    );
}

#[test]
fn classify_none_for_non_index() {
    assert_eq!(classify_request(&Method::GET, "/health"), None);
}

#[test]
fn classify_none_for_keys() {
    assert_eq!(classify_request(&Method::GET, "/1/keys"), None);
}

// ── middleware unit tests ──

/// Verify that a POST to `/query` increments `search_count` for the target index and leaves other counters at zero.
#[tokio::test]
async fn middleware_increments_search_count() {
    let counters = Arc::new(DashMap::new());
    let c = counters.clone();

    let handler = || async { "ok" };
    let app = axum::Router::new()
        .route("/1/indexes/:idx/query", axum::routing::post(handler))
        .layer(axum::middleware::from_fn(move |req, next| {
            let c = c.clone();
            async move { usage_counting_layer(req, next, &c).await }
        }));

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/1/indexes/products/query")
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{}"))
        .unwrap();

    tower::ServiceExt::oneshot(app, req).await.unwrap();

    let entry = counters
        .get("products")
        .expect("counter entry should exist");
    assert_eq!(entry.search_count.load(Ordering::Relaxed), 1);
    assert_eq!(entry.write_count.load(Ordering::Relaxed), 0);
}

/// Verify that a POST to `/batch` increments `write_count` for the target index.
#[tokio::test]
async fn middleware_increments_write_count() {
    let counters = Arc::new(DashMap::new());
    let c = counters.clone();

    let handler = || async { "ok" };
    let app = axum::Router::new()
        .route("/1/indexes/:idx/batch", axum::routing::post(handler))
        .layer(axum::middleware::from_fn(move |req, next| {
            let c = c.clone();
            async move { usage_counting_layer(req, next, &c).await }
        }));

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/1/indexes/products/batch")
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{}"))
        .unwrap();

    tower::ServiceExt::oneshot(app, req).await.unwrap();

    let entry = counters
        .get("products")
        .expect("counter entry should exist");
    assert_eq!(entry.write_count.load(Ordering::Relaxed), 1);
}

/// Verify that the middleware reads the `Content-Length` header and adds its value to `bytes_in` for the target index.
#[tokio::test]
async fn middleware_tracks_bytes_in() {
    let counters = Arc::new(DashMap::new());
    let c = counters.clone();

    let handler = || async { "ok" };
    let app = axum::Router::new()
        .route("/1/indexes/:idx/batch", axum::routing::post(handler))
        .layer(axum::middleware::from_fn(move |req, next| {
            let c = c.clone();
            async move { usage_counting_layer(req, next, &c).await }
        }));

    let body = r#"{"requests":[]}"#;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/1/indexes/products/batch")
        .header("content-type", "application/json")
        .header("content-length", body.len().to_string())
        .body(axum::body::Body::from(body))
        .unwrap();

    tower::ServiceExt::oneshot(app, req).await.unwrap();

    let entry = counters
        .get("products")
        .expect("counter entry should exist");
    assert_eq!(entry.bytes_in.load(Ordering::Relaxed), body.len() as u64);
}

/// Verify that a GET object request increments `read_count` while `search_count` and `write_count` remain zero.
#[tokio::test]
async fn middleware_increments_read_count() {
    let counters = Arc::new(DashMap::new());
    let c = counters.clone();

    let handler = || async { "ok" };
    let app = axum::Router::new()
        .route("/1/indexes/:idx/:objectID", axum::routing::get(handler))
        .layer(axum::middleware::from_fn(move |req, next| {
            let c = c.clone();
            async move { usage_counting_layer(req, next, &c).await }
        }));

    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/1/indexes/products/abc123")
        .body(axum::body::Body::empty())
        .unwrap();

    tower::ServiceExt::oneshot(app, req).await.unwrap();

    let entry = counters
        .get("products")
        .expect("counter entry should exist");
    assert_eq!(entry.read_count.load(Ordering::Relaxed), 1);
    assert_eq!(entry.search_count.load(Ordering::Relaxed), 0);
    assert_eq!(entry.write_count.load(Ordering::Relaxed), 0);
}

/// Verify that requests to non-index routes (e.g. `/health`) produce no counter entries.
#[tokio::test]
async fn middleware_ignores_non_index_routes() {
    let counters = Arc::new(DashMap::new());
    let c = counters.clone();

    let handler = || async { "ok" };
    let app = axum::Router::new()
        .route("/health", axum::routing::get(handler))
        .layer(axum::middleware::from_fn(move |req, next| {
            let c = c.clone();
            async move { usage_counting_layer(req, next, &c).await }
        }));

    let req = axum::http::Request::builder()
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();

    tower::ServiceExt::oneshot(app, req).await.unwrap();

    assert!(
        counters.is_empty(),
        "no counter entries for non-index routes"
    );
}

// ── handler-level counter integration tests ──

/// Build a minimal `AppState` backed by the given temp directory for use in handler-level integration tests.
///
/// # Arguments
///
/// * `tmp` — Temporary directory used as the base path for `IndexManager` and `DictionaryManager`.
fn make_app_state(tmp: &tempfile::TempDir) -> std::sync::Arc<crate::handlers::AppState> {
    std::sync::Arc::new(crate::handlers::AppState {
        manager: flapjack::IndexManager::new(tmp.path()),
        key_store: None,
        replication_manager: None,
        ssl_manager: None,
        analytics_engine: None,
        recommend_config: flapjack::recommend::RecommendConfig::default(),
        experiment_store: None,
        dictionary_manager: std::sync::Arc::new(
            flapjack::dictionaries::manager::DictionaryManager::new(tmp.path()),
        ),
        metrics_state: None,
        usage_counters: Arc::new(DashMap::new()),
        usage_persistence: None,
        paused_indexes: crate::pause_registry::PausedIndexes::new(),
        geoip_reader: None,
        notification_service: None,
        start_time: std::time::Instant::now(),
        conversation_store: crate::conversation_store::ConversationStore::default_shared(),
        embedder_store: std::sync::Arc::new(crate::embedder_store::EmbedderStore::new()),
        idempotency_cache: std::sync::Arc::new(
            crate::idempotency::IdempotencyCache::from_env_with_data_dir(tmp.path()),
        ),
    })
}

/// Verify that `put_object` increments `documents_indexed_total` by one after successfully indexing a single document.
#[tokio::test]
async fn handler_documents_indexed_total_increments_on_put() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = make_app_state(&tmp);
    state.manager.create_tenant("test_idx").unwrap();

    let app = axum::Router::new()
        .route(
            "/1/indexes/:indexName/:objectID",
            axum::routing::put(crate::handlers::put_object),
        )
        .with_state(state.clone());

    let req = axum::http::Request::builder()
        .method("PUT")
        .uri("/1/indexes/test_idx/doc1")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(r#"{"name":"Alice"}"#))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let entry = state
        .usage_counters
        .get("test_idx")
        .expect("counter entry should exist");
    assert_eq!(
        entry.documents_indexed_total.load(Ordering::Relaxed),
        1,
        "put_object should increment documents_indexed_total by 1"
    );
}

/// Verify that a batch request containing multiple `addObject` actions increments `documents_indexed_total` by the number of documents in the batch.
#[tokio::test]
async fn handler_documents_indexed_total_increments_on_batch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = make_app_state(&tmp);

    let app = crate::router::app_id_layer(
        axum::Router::new()
            .route(
                "/1/indexes/:indexName/batch",
                axum::routing::post(crate::handlers::add_documents),
            )
            .with_state(state.clone()),
    );

    let body = serde_json::json!({
        "requests": [
            {"action": "addObject", "body": {"objectID": "a", "name": "Alice"}},
            {"action": "addObject", "body": {"objectID": "b", "name": "Bob"}},
            {"action": "addObject", "body": {"objectID": "c", "name": "Carol"}}
        ]
    });

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/1/indexes/batch_idx/batch")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            serde_json::to_string(&body).unwrap(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let entry = state
        .usage_counters
        .get("batch_idx")
        .expect("counter entry should exist");
    assert_eq!(
        entry.documents_indexed_total.load(Ordering::Relaxed),
        3,
        "batch of 3 addObject should increment documents_indexed_total by 3"
    );
}

/// Verify that the search handler increments `search_results_total` by the number of hits returned in the response.
#[tokio::test]
async fn handler_search_results_total_increments_on_search() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = make_app_state(&tmp);

    // Add documents first so search has something to find
    state.manager.create_tenant("search_idx").unwrap();
    let docs = vec![
        flapjack::types::Document {
            id: "1".to_string(),
            fields: {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "name".to_string(),
                    flapjack::types::FieldValue::Text("hello world".to_string()),
                );
                m
            },
        },
        flapjack::types::Document {
            id: "2".to_string(),
            fields: {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "name".to_string(),
                    flapjack::types::FieldValue::Text("hello universe".to_string()),
                );
                m
            },
        },
    ];
    state
        .manager
        .add_documents_sync("search_idx", docs)
        .await
        .unwrap();

    let app = axum::Router::new()
        .route(
            "/1/indexes/:indexName/query",
            axum::routing::post(crate::handlers::search),
        )
        .with_state(state.clone());

    let body = serde_json::json!({"query": "hello"});
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/1/indexes/search_idx/query")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            serde_json::to_string(&body).unwrap(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let resp_body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
    let nb_hits = json["nbHits"].as_u64().unwrap();

    let entry = state
        .usage_counters
        .get("search_idx")
        .expect("counter entry should exist");
    assert_eq!(
        entry.search_results_total.load(Ordering::Relaxed),
        nb_hits,
        "search_results_total should match nbHits from response"
    );
    assert!(
        nb_hits > 0,
        "search should return at least 1 hit for 'hello'"
    );
}

// ── concurrent correctness ──

/// Verify that 100 concurrent search requests produce exactly 100 `search_count` increments and the correct cumulative `bytes_in`, ensuring no updates are lost under contention.
#[tokio::test]
async fn concurrent_requests_no_lost_increments() {
    let counters = Arc::new(DashMap::new());
    let total_requests = 100;

    let mut handles = Vec::new();
    for _ in 0..total_requests {
        let c = counters.clone();
        let handle = tokio::spawn(async move {
            let handler = || async { "ok" };
            let c2 = c.clone();
            let app = axum::Router::new()
                .route("/1/indexes/:idx/query", axum::routing::post(handler))
                .layer(axum::middleware::from_fn(move |req, next| {
                    let c3 = c2.clone();
                    async move { usage_counting_layer(req, next, &c3).await }
                }));

            let req = axum::http::Request::builder()
                .method("POST")
                .uri("/1/indexes/concurrent_test/query")
                .header("content-type", "application/json")
                .header("content-length", "2")
                .body(axum::body::Body::from("{}"))
                .unwrap();

            tower::ServiceExt::oneshot(app, req).await.unwrap();
        });
        handles.push(handle);
    }

    for h in handles {
        h.await.unwrap();
    }

    let entry = counters
        .get("concurrent_test")
        .expect("counter entry should exist");
    assert_eq!(
        entry.search_count.load(Ordering::Relaxed),
        total_requests,
        "all {} search increments should be counted",
        total_requests,
    );
    assert_eq!(
        entry.bytes_in.load(Ordering::Relaxed),
        total_requests * 2,
        "all bytes_in should be counted (2 bytes per request)",
    );
}
