//! Safety integration tests
//!
//! Combines:
//! - test_cold_start_cache_bounds.rs: Facet cache warming, eviction, correctness
//! - test_memory_safety.rs: Document size limits, batch limits, memory pressure

mod common;

mod cold_start_cache_bounds {
    use super::common::faceted_search_options;
    use flapjack::index::settings::IndexSettings;
    use flapjack::types::{Document, FacetRequest, FieldValue};
    use flapjack::IndexManager;
    use std::collections::HashMap;
    use std::sync::atomic::Ordering;
    use tempfile::TempDir;

    fn doc(id: &str, fields: Vec<(&str, FieldValue)>) -> Document {
        let f: HashMap<String, FieldValue> = fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        Document {
            id: id.to_string(),
            fields: f,
        }
    }

    fn text(s: &str) -> FieldValue {
        FieldValue::Text(s.to_string())
    }

    async fn setup_with_docs(count: usize) -> (TempDir, std::sync::Arc<IndexManager>) {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("t1").unwrap();

        let settings_path = temp_dir.path().join("t1").join("settings.json");
        let settings = IndexSettings {
            attributes_for_faceting: vec!["category".to_string(), "brand".to_string()],
            ..IndexSettings::default()
        };
        settings.save(&settings_path).unwrap();
        manager.invalidate_settings_cache("t1");

        let docs: Vec<Document> = (0..count)
            .map(|i| {
                doc(
                    &format!("doc{}", i),
                    vec![
                        ("name", text(&format!("product {}", i))),
                        ("category", text(&format!("cat{}", i % 10))),
                        ("brand", text(&format!("brand{}", i % 5))),
                    ],
                )
            })
            .collect();

        manager.add_documents_sync("t1", docs).await.unwrap();
        (temp_dir, manager)
    }

    #[tokio::test]
    async fn test_searchable_paths_warm_on_load() {
        let (_temp_dir, manager) = setup_with_docs(50).await;

        manager.unload(&"t1".to_string()).unwrap();

        let _index = manager.get_or_load("t1").unwrap();

        let index2 = manager.get_or_load("t1").unwrap();
        let t0 = std::time::Instant::now();
        let paths = index2.searchable_paths();
        let paths_time = t0.elapsed();

        assert!(!paths.is_empty(), "should have searchable paths");
        assert!(
            paths_time.as_micros() < 1000,
            "searchable_paths after get_or_load should be cached (<1ms), got {:?}",
            paths_time
        );
    }

    #[tokio::test]
    async fn test_searchable_paths_warm_on_create_existing() {
        let (temp_dir, manager) = setup_with_docs(50).await;

        manager.unload(&"t1".to_string()).unwrap();

        let manager2 = IndexManager::new(temp_dir.path());
        manager2.create_tenant("t1").unwrap();

        let index = manager2.get_or_load("t1").unwrap();
        let t0 = std::time::Instant::now();
        let paths = index.searchable_paths();
        let paths_time = t0.elapsed();

        assert!(!paths.is_empty());
        assert!(
            paths_time.as_micros() < 1000,
            "searchable_paths after create_tenant(existing) should be cached (<1ms), got {:?}",
            paths_time
        );
    }

    async fn setup_with_cap(
        doc_count: usize,
        cap: usize,
    ) -> (TempDir, std::sync::Arc<IndexManager>) {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager
            .facet_cache_cap
            .store(cap, std::sync::atomic::Ordering::Relaxed);
        manager.create_tenant("t1").unwrap();

        let settings_path = temp_dir.path().join("t1").join("settings.json");
        let settings = IndexSettings {
            attributes_for_faceting: vec!["category".to_string(), "brand".to_string()],
            ..IndexSettings::default()
        };
        settings.save(&settings_path).unwrap();
        manager.invalidate_settings_cache("t1");

        let docs: Vec<Document> = (0..doc_count)
            .map(|i| {
                doc(
                    &format!("doc{}", i),
                    vec![
                        ("name", text(&format!("product {}", i))),
                        ("category", text(&format!("cat{}", i % 5))),
                        ("brand", text(&format!("brand{}", i % 3))),
                    ],
                )
            })
            .collect();

        manager.add_documents_sync("t1", docs).await.unwrap();
        (temp_dir, manager)
    }

    #[tokio::test]
    async fn test_facet_cache_bounded_by_cap() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.facet_cache_cap.store(15, Ordering::Relaxed);

        for i in 0..20 {
            manager.facet_cache.insert(
                format!("t1:q{}:category", i),
                std::sync::Arc::new((
                    std::time::Instant::now(),
                    0,
                    HashMap::new(),
                    HashMap::new(),
                    true,
                )),
            );
        }
        assert_eq!(
            manager.facet_cache.len(),
            20,
            "no eviction yet, just raw inserts"
        );

        let cap = manager.facet_cache_cap.load(Ordering::Relaxed);
        while manager.facet_cache.len() >= cap {
            let key = {
                let entry = manager.facet_cache.iter().next().unwrap();
                entry.key().clone()
            };
            manager.facet_cache.remove(&key);
        }

        assert_eq!(manager.facet_cache.len(), 14, "evicted down to cap-1");
    }

    #[tokio::test]
    async fn test_facet_cache_no_eviction_under_cap() {
        let (_temp_dir, manager) = setup_with_cap(10, 50).await;

        let facets = vec![FacetRequest {
            field: "category".to_string(),
            path: "/category".to_string(),
            value_query: None,
        }];

        for i in 0..20 {
            let query = format!("q{}", i);
            let options = faceted_search_options(None, None, 1, 0, Some(&facets));
            let _ = manager.search_with_options("t1", &query, &options);
        }

        let cache_len = manager.facet_cache.len();
        assert_eq!(
            cache_len, 1,
            "all queries with same facets/filter should share one cache entry, got {}",
            cache_len
        );
    }

    #[tokio::test]
    async fn test_facet_cache_still_returns_correct_results() {
        let (_temp_dir, manager) = setup_with_docs(100).await;

        let facets = vec![FacetRequest {
            field: "category".to_string(),
            path: "/category".to_string(),
            value_query: None,
        }];

        let options = faceted_search_options(None, None, 10, 0, Some(&facets));
        let r1 = manager
            .search_with_options("t1", "product", &options)
            .unwrap();
        assert!(!r1.facets.is_empty(), "should have facet results");
        assert!(
            r1.facets.contains_key("category"),
            "should have category facet"
        );

        let r2 = manager
            .search_with_options("t1", "product", &options)
            .unwrap();
        assert_eq!(
            r1.facets["category"].len(),
            r2.facets["category"].len(),
            "cached result should match"
        );
    }

    #[tokio::test]
    async fn test_facet_cache_invalidated_on_write() {
        let (_temp_dir, manager) = setup_with_docs(50).await;

        let facets = vec![FacetRequest {
            field: "category".to_string(),
            path: "/category".to_string(),
            value_query: None,
        }];

        let warm_options = faceted_search_options(None, None, 10, 0, Some(&facets));
        let _ = manager
            .search_with_options("t1", "product", &warm_options)
            .unwrap();
        assert!(!manager.facet_cache.is_empty(), "cache should have entries");

        manager
            .add_documents_sync(
                "t1",
                vec![doc(
                    "newdoc",
                    vec![("name", text("product new")), ("category", text("catnew"))],
                )],
            )
            .await
            .unwrap();

        let refreshed_options = faceted_search_options(None, None, 100, 0, Some(&facets));
        let r = manager
            .search_with_options("t1", "product", &refreshed_options)
            .unwrap();
        let cat_paths: Vec<&str> = r.facets["category"]
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert!(
            cat_paths.iter().any(|p| p.contains("catnew")),
            "new category should appear after cache invalidation"
        );
    }
}

mod memory_safety {
    use super::common;
    use axum::{
        middleware,
        routing::{get, post},
        Router,
    };
    use flapjack::types::{Document, FieldValue, TaskStatus};
    use flapjack::IndexManager;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::net::TcpListener;

    fn is_port_binding_restricted_error(err: &std::io::Error) -> bool {
        err.kind() == std::io::ErrorKind::PermissionDenied
    }

    struct PressureOverrideGuard<'a> {
        observer: &'a flapjack::MemoryObserver,
        restore_to: Option<flapjack::PressureLevel>,
    }

    impl<'a> PressureOverrideGuard<'a> {
        fn new(observer: &'a flapjack::MemoryObserver) -> Self {
            Self {
                observer,
                restore_to: observer.pressure_override(),
            }
        }
    }

    impl Drop for PressureOverrideGuard<'_> {
        fn drop(&mut self) {
            self.observer.set_pressure_override(self.restore_to);
        }
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(original) = &self.original {
                std::env::set_var(self.key, original);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[tokio::test]
    async fn test_oversized_document_rejected() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test_idx").unwrap();

        let big_value = "x".repeat(4 * 1024 * 1024);
        let doc = Document {
            id: "big-doc-1".to_string(),
            fields: HashMap::from([("payload".to_string(), FieldValue::Text(big_value))]),
        };

        let task = manager.add_documents("test_idx", vec![doc]).unwrap();

        loop {
            let status = manager.get_task(&task.id).unwrap();
            match status.status {
                TaskStatus::Enqueued | TaskStatus::Processing => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
                TaskStatus::Succeeded => {
                    assert_eq!(status.rejected_count, 1, "Expected 1 rejected document");
                    assert_eq!(status.rejected_documents.len(), 1);
                    assert_eq!(status.rejected_documents[0].doc_id, "big-doc-1");
                    assert_eq!(status.rejected_documents[0].error, "document_too_large");
                    assert_eq!(
                        status.indexed_documents, 0,
                        "No docs should have been indexed"
                    );
                    break;
                }
                TaskStatus::Failed(e) => {
                    panic!("Task failed unexpectedly: {}", e);
                }
            }
        }
    }

    #[tokio::test]
    async fn test_normal_document_accepted() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test_idx").unwrap();

        let doc = Document {
            id: "small-doc-1".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                FieldValue::Text("A perfectly normal document".to_string()),
            )]),
        };

        let task = manager.add_documents("test_idx", vec![doc]).unwrap();

        loop {
            let status = manager.get_task(&task.id).unwrap();
            match status.status {
                TaskStatus::Enqueued | TaskStatus::Processing => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
                TaskStatus::Succeeded => {
                    assert_eq!(status.rejected_count, 0, "No docs should be rejected");
                    assert!(status.indexed_documents > 0, "Document should be indexed");
                    break;
                }
                TaskStatus::Failed(e) => {
                    panic!("Task failed unexpectedly: {}", e);
                }
            }
        }
    }

    #[tokio::test]
    async fn test_mixed_batch_partial_rejection() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test_idx").unwrap();

        let big_value = "x".repeat(4 * 1024 * 1024);
        let docs = vec![
            Document {
                id: "small-1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("Good doc".to_string()),
                )]),
            },
            Document {
                id: "big-1".to_string(),
                fields: HashMap::from([("payload".to_string(), FieldValue::Text(big_value))]),
            },
            Document {
                id: "small-2".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("Another good doc".to_string()),
                )]),
            },
        ];

        let task = manager.add_documents("test_idx", docs).unwrap();

        loop {
            let status = manager.get_task(&task.id).unwrap();
            match status.status {
                TaskStatus::Enqueued | TaskStatus::Processing => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
                TaskStatus::Succeeded => {
                    assert_eq!(
                        status.rejected_count, 1,
                        "Only the oversized doc should be rejected"
                    );
                    assert_eq!(status.rejected_documents[0].doc_id, "big-1");
                    assert_eq!(status.rejected_documents[0].error, "document_too_large");
                    assert_eq!(
                        status.indexed_documents, 2,
                        "Two normal docs should be indexed"
                    );
                    break;
                }
                TaskStatus::Failed(e) => {
                    panic!("Task failed unexpectedly: {}", e);
                }
            }
        }
    }

    #[tokio::test]
    async fn test_oversized_upsert_rejected() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test_idx").unwrap();

        let small_doc = Document {
            id: "doc-1".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                FieldValue::Text("OriginalUpsertTest".to_string()),
            )]),
        };
        let task = manager
            .add_documents_insert("test_idx", vec![small_doc])
            .unwrap();
        loop {
            let status = manager.get_task(&task.id).unwrap();
            match status.status {
                TaskStatus::Enqueued | TaskStatus::Processing => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
                TaskStatus::Succeeded => {
                    break;
                }
                TaskStatus::Failed(e) => {
                    panic!("Initial add failed: {}", e);
                }
            }
        }

        let big_value = "x".repeat(4 * 1024 * 1024);
        let big_doc = Document {
            id: "doc-1".to_string(),
            fields: HashMap::from([("payload".to_string(), FieldValue::Text(big_value))]),
        };
        let task = manager.add_documents("test_idx", vec![big_doc]).unwrap();
        loop {
            let status = manager.get_task(&task.id).unwrap();
            match status.status {
                TaskStatus::Enqueued | TaskStatus::Processing => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
                TaskStatus::Succeeded => {
                    assert_eq!(
                        status.rejected_count, 1,
                        "Oversized upsert should be rejected"
                    );
                    assert_eq!(status.rejected_documents[0].doc_id, "doc-1");
                    assert_eq!(status.rejected_documents[0].error, "document_too_large");
                    assert_eq!(status.indexed_documents, 0, "No docs should be re-indexed");
                    break;
                }
                TaskStatus::Failed(e) => {
                    panic!("Upsert task failed: {}", e);
                }
            }
        }

        let results = manager
            .search("test_idx", "OriginalUpsertTest", None, None, 10)
            .unwrap();
        assert_eq!(
            results.documents.len(),
            1,
            "Original document should still exist after failed upsert"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_batch_size_limit() {
        let _batch_size_guard = EnvVarGuard::set("FLAPJACK_MAX_BATCH_SIZE", "5");
        let observer = flapjack::MemoryObserver::global();
        let _pressure_override_guard = PressureOverrideGuard::new(observer);
        observer.set_pressure_override(Some(flapjack::PressureLevel::Normal));

        let (addr, _tmp) = common::spawn_server().await;
        let client = reqwest::Client::new();

        let small_ops: Vec<serde_json::Value> = (0..5)
            .map(|i| {
                serde_json::json!({
                    "action": "addObject",
                    "body": {"objectID": format!("doc-{}", i), "title": format!("Doc {}", i)}
                })
            })
            .collect();

        let resp = client
            .post(format!("http://{}/1/indexes/test_idx/batch", addr))
            .json(&serde_json::json!({"requests": small_ops}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "Expected 200 for batch within limit");

        let big_ops: Vec<serde_json::Value> = (0..6)
            .map(|i| {
                serde_json::json!({
                    "action": "addObject",
                    "body": {"objectID": format!("big-{}", i), "title": format!("Doc {}", i)}
                })
            })
            .collect();

        let resp = client
            .post(format!("http://{}/1/indexes/test_idx/batch", addr))
            .json(&serde_json::json!({"requests": big_ops}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "Expected 400 for batch too large");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["message"].as_str().unwrap().contains("Batch size"));
        assert_eq!(body["status"], 400);
    }

    #[tokio::test]
    async fn test_health_returns_json_with_memory_fields() {
        let (addr, _tmp) = common::spawn_server().await;
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{}/health", addr))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();

        assert_eq!(body["status"], "ok");
        assert!(
            body["active_writers"].is_number(),
            "active_writers should be a number"
        );
        assert!(
            body["max_concurrent_writers"].is_number(),
            "max_concurrent_writers should be a number"
        );
        assert!(
            body["facet_cache_entries"].is_number(),
            "facet_cache_entries should be a number"
        );
        assert!(
            body["facet_cache_cap"].is_number(),
            "facet_cache_cap should be a number"
        );
        assert!(
            body["heap_allocated_mb"].is_number(),
            "heap_allocated_mb should be a number"
        );
        assert!(
            body["system_limit_mb"].is_number(),
            "system_limit_mb should be a number"
        );
        assert!(
            body["pressure_level"].is_string(),
            "pressure_level should be a string"
        );
        assert!(
            body["allocator"].is_string(),
            "allocator should be a string"
        );
    }

    async fn spawn_server_with_pressure_middleware() -> Option<(String, TempDir)> {
        let temp_dir = TempDir::new().unwrap();
        let manager = flapjack::IndexManager::new(temp_dir.path());

        let state = Arc::new(flapjack_http::handlers::AppState {
            manager,
            key_store: None,
            replication_manager: None,
            ssl_manager: None,
            analytics_engine: None,
            recommend_config: Default::default(),
            dictionary_manager: Arc::new(flapjack::dictionaries::manager::DictionaryManager::new(
                temp_dir.path(),
            )),
            metrics_state: None,
            usage_counters: std::sync::Arc::new(dashmap::DashMap::new()),
            paused_indexes: flapjack_http::pause_registry::PausedIndexes::new(),
            usage_persistence: None,
            geoip_reader: None,
            notification_service: None,
            start_time: std::time::Instant::now(),
            conversation_store:
                flapjack_http::conversation_store::ConversationStore::default_shared(),
            experiment_store: None,
            embedder_store: std::sync::Arc::new(flapjack_http::embedder_store::EmbedderStore::new()),
            idempotency_cache: std::sync::Arc::new(
                flapjack_http::idempotency::IdempotencyCache::new(std::time::Duration::from_secs(
                    300,
                )),
            ),
        });

        let health_route =
            flapjack_http::router::build_public_health_routes().with_state(state.clone());

        let protected = Router::new()
            .route("/1/indexes", post(flapjack_http::handlers::create_index))
            .route(
                "/1/indexes/:indexName/batch",
                post(flapjack_http::handlers::add_documents),
            )
            .route(
                "/1/indexes/:indexName/query",
                post(flapjack_http::handlers::search),
            )
            .route("/1/task/:task_id", get(flapjack_http::handlers::get_task))
            .route("/1/tasks/:task_id", get(flapjack_http::handlers::get_task))
            .with_state(state.clone());

        let mgr_for_pressure = Arc::clone(&state.manager);
        let default_cap = state
            .manager
            .facet_cache_cap
            .load(std::sync::atomic::Ordering::Relaxed);
        let memory_middleware = middleware::from_fn(
            move |request: axum::extract::Request, next: middleware::Next| {
                let mgr = mgr_for_pressure.clone();
                async move {
                    flapjack_http::memory_middleware::memory_pressure_guard(
                        request,
                        next,
                        &mgr,
                        default_cap,
                    )
                    .await
                }
            },
        );

        // Auth middleware is intentionally omitted: key_store is None (open mode).
        // This matches production behavior in router.rs where auth_layer is only
        // applied when key_store is Some.
        let app = Router::new()
            .merge(health_route)
            .merge(protected)
            .layer(memory_middleware);

        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) if is_port_binding_restricted_error(&err) => return None,
            Err(err) => panic!("failed to bind test listener: {err}"),
        };
        let addr = listener.local_addr().unwrap().to_string();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Poll health endpoint instead of blind sleep
        let client = reqwest::Client::new();
        for _ in 0..100 {
            if client
                .get(format!("http://{}/health", addr))
                .send()
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
        Some((addr, temp_dir))
    }

    #[tokio::test]
    #[serial]
    async fn test_memory_pressure_levels() {
        let Some((addr, _tmp)) = spawn_server_with_pressure_middleware().await else {
            eprintln!(
                "Skipping memory_safety::test_memory_pressure_levels due to sandbox-restricted port binding"
            );
            return;
        };
        let client = reqwest::Client::new();
        let observer = flapjack::MemoryObserver::global();
        let _pressure_override_guard = PressureOverrideGuard::new(observer);
        let assert_memory_pressure_body = |body: &serde_json::Value, expected_level: &str| {
            assert_eq!(
                body["error"], "memory_pressure",
                "error payload should expose memory_pressure error code"
            );
            assert!(
                body["allocated_mb"].is_number(),
                "error payload should expose allocated_mb as number"
            );
            assert!(
                body["limit_mb"].is_number(),
                "error payload should expose limit_mb as number"
            );
            assert_eq!(
                body["level"], expected_level,
                "error payload should expose expected pressure level"
            );
        };

        // --- Normal: everything should pass through the middleware ---
        observer.set_pressure_override(Some(flapjack::PressureLevel::Normal));

        let resp = client
            .post(format!("http://{}/1/indexes/test_idx/batch", addr))
            .json(&serde_json::json!({"requests": [{
                "action": "addObject",
                "body": {"objectID": "doc-normal", "title": "Test"}
            }]}))
            .send()
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            503,
            "Writes should NOT be rejected under normal pressure"
        );

        // --- Critical: reject everything except /health ---
        observer.set_pressure_override(Some(flapjack::PressureLevel::Critical));

        let resp = client
            .post(format!("http://{}/1/indexes/test_idx/batch", addr))
            .json(&serde_json::json!({"requests": [{
                "action": "addObject",
                "body": {"objectID": "doc-1", "title": "Test"}
            }]}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            503,
            "Expected 503 for writes under critical pressure"
        );
        assert_eq!(
            resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("5"),
            "503 should include Retry-After: 5 header"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_memory_pressure_body(&body, "critical");

        let resp = client
            .post(format!("http://{}/1/indexes/test_idx/query", addr))
            .json(&serde_json::json!({"query": "test"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            503,
            "Search should be rejected under critical pressure"
        );
        assert_eq!(
            resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("5"),
            "Critical-pressure query rejection should include Retry-After: 5 header"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_memory_pressure_body(&body, "critical");

        let resp = client
            .get(format!("http://{}/health", addr))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            200,
            "Health should return 200 even under critical pressure"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok");

        // --- Elevated: reject all POSTs, allow GET + health ---
        observer.set_pressure_override(Some(flapjack::PressureLevel::Elevated));

        let resp = client
            .post(format!("http://{}/1/indexes/test_idx/batch", addr))
            .json(&serde_json::json!({"requests": [{
                "action": "addObject",
                "body": {"objectID": "doc-1", "title": "Test"}
            }]}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            503,
            "Writes should be rejected under elevated pressure"
        );
        assert_eq!(
            resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("5"),
            "Elevated-pressure write rejection should include Retry-After: 5 header"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_memory_pressure_body(&body, "elevated");

        let resp = client
            .post(format!("http://{}/1/indexes/test_idx/query", addr))
            .json(&serde_json::json!({"query": "test"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            503,
            "POST search should be rejected under elevated pressure"
        );
        assert_eq!(
            resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("5"),
            "Elevated-pressure query rejection should include Retry-After: 5 header"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_memory_pressure_body(&body, "elevated");

        let resp = client
            .get(format!("http://{}/health", addr))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            200,
            "Health should return 200 under elevated pressure"
        );
    }

    #[test]
    #[serial]
    fn pressure_override_guard_restores_previous_override_on_unwind() {
        let observer = flapjack::MemoryObserver::global();
        observer.set_pressure_override(Some(flapjack::PressureLevel::Elevated));

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = PressureOverrideGuard::new(observer);
            observer.set_pressure_override(Some(flapjack::PressureLevel::Critical));
            assert_eq!(observer.pressure_level(), flapjack::PressureLevel::Critical);
            panic!("intentional panic to verify override restoration");
        }));

        assert_eq!(observer.pressure_level(), flapjack::PressureLevel::Elevated);
        observer.set_pressure_override(None);
    }

    #[test]
    #[serial]
    fn pressure_override_guard_restores_existing_override_without_manual_restore_value() {
        let observer = flapjack::MemoryObserver::global();
        observer.set_pressure_override(Some(flapjack::PressureLevel::Elevated));
        {
            let _guard = PressureOverrideGuard::new(observer);
            observer.set_pressure_override(Some(flapjack::PressureLevel::Critical));
            assert_eq!(observer.pressure_level(), flapjack::PressureLevel::Critical);
        }

        assert_eq!(
            observer.pressure_level(),
            flapjack::PressureLevel::Elevated,
            "guard should restore pre-existing override even when caller does not pass it explicitly"
        );
        observer.set_pressure_override(None);
    }

    #[test]
    fn bind_permission_denied_is_detected_as_restricted() {
        let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "sandbox blocked");
        assert!(is_port_binding_restricted_error(&err));
    }

    #[test]
    fn non_permission_bind_errors_are_not_marked_restricted() {
        let err = std::io::Error::new(std::io::ErrorKind::AddrInUse, "already in use");
        assert!(!is_port_binding_restricted_error(&err));
    }
}
