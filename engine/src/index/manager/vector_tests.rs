use super::*;

// ── Vector index store tests (6.11) ──

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_index_store_and_retrieve() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    manager.set_vector_index("tenant1", vi);

    let retrieved = manager.get_vector_index("tenant1");
    assert!(retrieved.is_some());
    let lock = retrieved.unwrap();
    let guard = lock.read().unwrap();
    assert_eq!(guard.dimensions(), 3);
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_index_missing_returns_none() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    assert!(manager.get_vector_index("nonexistent").is_none());
}

/// TODO: Document test_vector_index_search_through_manager.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_index_search_through_manager() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.add("doc2", &[0.0, 1.0, 0.0]).unwrap();
    vi.add("doc3", &[0.0, 0.0, 1.0]).unwrap();
    manager.set_vector_index("t1", vi);

    let lock = manager.get_vector_index("t1").unwrap();
    let guard = lock.read().unwrap();
    let results = guard.search(&[1.0, 0.0, 0.0], 2).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].doc_id, "doc1");
}

// ── Multi-tenant vector isolation test ──

/// TODO: Document test_vector_tenant_isolation.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_tenant_isolation() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Tenant A: 3-dim vectors about "cats"
    let mut vi_a = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi_a.add("cat1", &[1.0, 0.0, 0.0]).unwrap();
    vi_a.add("cat2", &[0.9, 0.1, 0.0]).unwrap();
    vi_a.add("cat3", &[0.8, 0.2, 0.0]).unwrap();
    manager.set_vector_index("tenant_a", vi_a);

    // Tenant B: 3-dim vectors about "dogs" (orthogonal direction)
    let mut vi_b = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi_b.add("dog1", &[0.0, 0.0, 1.0]).unwrap();
    vi_b.add("dog2", &[0.0, 0.1, 0.9]).unwrap();
    manager.set_vector_index("tenant_b", vi_b);

    // Search tenant A — must only return tenant A's docs
    {
        let lock = manager.get_vector_index("tenant_a").unwrap();
        let guard = lock.read().unwrap();
        let results = guard.search(&[1.0, 0.0, 0.0], 10).unwrap();
        assert_eq!(results.len(), 3, "tenant_a should have exactly 3 vectors");
        for r in &results {
            assert!(
                r.doc_id.starts_with("cat"),
                "tenant_a search returned '{}' which belongs to tenant_b",
                r.doc_id
            );
        }
    }

    // Search tenant B — must only return tenant B's docs
    {
        let lock = manager.get_vector_index("tenant_b").unwrap();
        let guard = lock.read().unwrap();
        let results = guard.search(&[0.0, 0.0, 1.0], 10).unwrap();
        assert_eq!(results.len(), 2, "tenant_b should have exactly 2 vectors");
        for r in &results {
            assert!(
                r.doc_id.starts_with("dog"),
                "tenant_b search returned '{}' which belongs to tenant_a",
                r.doc_id
            );
        }
    }

    // Verify tenant C (nonexistent) returns None
    assert!(
        manager.get_vector_index("tenant_c").is_none(),
        "nonexistent tenant should return None"
    );

    // Delete tenant A's index, verify tenant B is unaffected
    manager.vector_indices.remove("tenant_a");
    assert!(manager.get_vector_index("tenant_a").is_none());
    {
        let lock = manager.get_vector_index("tenant_b").unwrap();
        let guard = lock.read().unwrap();
        assert_eq!(
            guard.len(),
            2,
            "tenant_b should be unaffected by tenant_a removal"
        );
    }
}

#[tokio::test]
async fn all_tenant_oplog_seqs_empty_when_no_oplogs() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    // Create tenant but don't write anything (no oplog created)
    manager.create_tenant("t1").unwrap();
    let seqs = manager.all_tenant_oplog_seqs();
    assert!(seqs.is_empty(), "no oplog loaded means empty result");
}

// ── Vector index load-on-open tests (8.4) ──

/// TODO: Document test_load_vector_index_on_get_or_load.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_load_vector_index_on_get_or_load() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "load_vec_t";
    let tenant_path = tmp.path().join(tenant_id);

    // Create a Tantivy index on disk
    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save settings with an embedder so load_vector_index proceeds past the
    // "no embedders configured" guard (added in 8.19).
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Manually save a VectorIndex with 3 docs (no fingerprint file → backward compat load)
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.add("doc2", &[0.0, 1.0, 0.0]).unwrap();
    vi.add("doc3", &[0.0, 0.0, 1.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    // Create IndexManager and get_or_load
    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    // Verify VectorIndex was loaded from disk
    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(vi_arc.is_some(), "VectorIndex should be loaded from disk");
    let vi_arc = vi_arc.unwrap();
    let guard = vi_arc.read().unwrap();
    assert_eq!(guard.len(), 3);
    assert_eq!(guard.dimensions(), 3);

    // Verify it's searchable
    let results = guard.search(&[1.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "doc1");
}

/// TODO: Document test_load_no_vectors_dir_ok.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_load_no_vectors_dir_ok() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "novecdir_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    // No VectorIndex should be loaded
    assert!(
        manager.get_vector_index(tenant_id).is_none(),
        "get_vector_index should return None when no vectors/ dir exists"
    );
}

/// TODO: Document test_load_corrupted_vector_index_logs_warning.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_load_corrupted_vector_index_logs_warning() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "corrupt_vec_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save settings with an embedder so load_vector_index actually attempts
    // VectorIndex::load (without this it returns early at the "no embedders
    // configured" guard, making the test a false positive).
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Write garbage to id_map.json (no fingerprint → backward compat, proceeds to load)
    let vectors_dir = tenant_path.join("vectors");
    std::fs::create_dir_all(&vectors_dir).unwrap();
    std::fs::write(vectors_dir.join("id_map.json"), "not valid json!!!").unwrap();

    let manager = IndexManager::new(tmp.path());
    // Should not error — gracefully skip corrupted vectors
    manager.get_or_load(tenant_id).unwrap();

    // VectorIndex should not be loaded
    assert!(
        manager.get_vector_index(tenant_id).is_none(),
        "corrupted vector index should not be loaded"
    );
}

/// TODO: Document test_create_tenant_loads_existing_vectors.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_create_tenant_loads_existing_vectors() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "create_load_t";
    let tenant_path = tmp.path().join(tenant_id);

    // Create tenant dir with Tantivy index
    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save settings with an embedder so load_vector_index proceeds past the
    // "no embedders configured" guard (added in 8.19).
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Save VectorIndex (no fingerprint file → backward compat load)
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.add("doc2", &[0.0, 1.0, 0.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();

    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(
        vi_arc.is_some(),
        "VectorIndex should be loaded on create_tenant"
    );
    let vi_arc = vi_arc.unwrap();
    let guard = vi_arc.read().unwrap();
    assert_eq!(guard.len(), 2);
}

// ── Vector recovery from oplog tests (8.10) ──

/// Helper: create a tenant dir with a Tantivy index and an oplog, then write oplog entries
/// with `_vectors` in the body. Returns the tenant path.
#[cfg(feature = "vector-search")]
fn setup_tenant_with_oplog_vectors(
    base_path: &Path,
    tenant_id: &str,
    ops: &[(String, serde_json::Value)],
) -> PathBuf {
    let tenant_path = base_path.join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();

    // Create a Tantivy index
    let schema = crate::index::schema::Schema::builder().build();
    let _ = crate::index::Index::create(&tenant_path, schema).unwrap();

    // Write default settings
    let settings = crate::index::settings::IndexSettings::default();
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Create oplog and write entries
    let oplog_dir = tenant_path.join("oplog");
    let oplog = OpLog::open(&oplog_dir, tenant_id, "test_node").unwrap();
    oplog.append_batch(ops).unwrap();

    // Write committed_seq=0 to force full replay
    std::fs::write(tenant_path.join("committed_seq"), "0").unwrap();

    tenant_path
}

/// TODO: Document test_recover_vectors_from_oplog.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_vectors_from_oplog() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_vec_t";

    let ops = vec![
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc1",
                "body": {
                    "objectID": "doc1",
                    "title": "first",
                    "_vectors": {"default": [1.0, 0.0, 0.0]}
                }
            }),
        ),
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc2",
                "body": {
                    "objectID": "doc2",
                    "title": "second",
                    "_vectors": {"default": [0.0, 1.0, 0.0]}
                }
            }),
        ),
    ];

    setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    // Verify VectorIndex was rebuilt from oplog
    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(vi_arc.is_some(), "VectorIndex should be rebuilt from oplog");
    let vi_arc = vi_arc.unwrap();
    let guard = vi_arc.read().unwrap();
    assert_eq!(guard.len(), 2);

    let results = guard.search(&[1.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "doc1");
}

/// TODO: Document test_recover_vectors_with_deletes.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_vectors_with_deletes() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_del_t";

    let ops = vec![
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc1",
                "body": {
                    "objectID": "doc1",
                    "title": "first",
                    "_vectors": {"default": [1.0, 0.0, 0.0]}
                }
            }),
        ),
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc2",
                "body": {
                    "objectID": "doc2",
                    "title": "second",
                    "_vectors": {"default": [0.0, 1.0, 0.0]}
                }
            }),
        ),
        (
            "delete".to_string(),
            serde_json::json!({"objectID": "doc1"}),
        ),
    ];

    setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(vi_arc.is_some(), "VectorIndex should exist after recovery");
    let vi_lock = vi_arc.unwrap();
    let guard = vi_lock.read().unwrap();
    assert_eq!(guard.len(), 1, "only doc2 should remain after delete");

    let results = guard.search(&[0.0, 1.0, 0.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "doc2");
}

/// TODO: Document test_recover_no_vectors_in_old_oplog.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_no_vectors_in_old_oplog() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_novec_t";

    // Oplog entries without _vectors (pre-stage-8 format)
    let ops = vec![(
        "upsert".to_string(),
        serde_json::json!({
            "objectID": "doc1",
            "body": {"objectID": "doc1", "title": "old format doc"}
        }),
    )];

    setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    // No VectorIndex should be created
    assert!(
        manager.get_vector_index(tenant_id).is_none(),
        "no VectorIndex when oplog has no _vectors"
    );
}

/// TODO: Document test_recover_vectors_after_clear_op.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_vectors_after_clear_op() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_clear_t";

    let ops = vec![
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc1",
                "body": {
                    "objectID": "doc1",
                    "title": "first",
                    "_vectors": {"default": [1.0, 0.0, 0.0]}
                }
            }),
        ),
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc2",
                "body": {
                    "objectID": "doc2",
                    "title": "second",
                    "_vectors": {"default": [0.0, 1.0, 0.0]}
                }
            }),
        ),
        ("clear".to_string(), serde_json::json!({})),
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc3",
                "body": {
                    "objectID": "doc3",
                    "title": "third",
                    "_vectors": {"default": [0.0, 0.0, 1.0]}
                }
            }),
        ),
    ];

    setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(vi_arc.is_some(), "VectorIndex should exist after recovery");
    let vi_lock = vi_arc.unwrap();
    let guard = vi_lock.read().unwrap();
    assert_eq!(guard.len(), 1, "only doc3 should exist after clear + add");

    let results = guard.search(&[0.0, 0.0, 1.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "doc3");
}

/// TODO: Document test_recover_vectors_saved_to_disk.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_vectors_saved_to_disk() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_disk_t";

    let ops = vec![(
        "upsert".to_string(),
        serde_json::json!({
            "objectID": "doc1",
            "body": {
                "objectID": "doc1",
                "title": "first",
                "_vectors": {"default": [1.0, 0.0, 0.0]}
            }
        }),
    )];

    let tenant_path = setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    // Verify vector files were saved to disk after recovery
    let vectors_dir = tenant_path.join("vectors");
    assert!(
        vectors_dir.join("index.usearch").exists(),
        "index.usearch should be saved after recovery"
    );
    assert!(
        vectors_dir.join("id_map.json").exists(),
        "id_map.json should be saved after recovery"
    );
}

/// TODO: Document test_recover_vectors_upsert_same_doc_twice.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_vectors_upsert_same_doc_twice() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_dup_t";

    // Upsert doc1 with vector A, then upsert doc1 again with vector B
    let ops = vec![
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc1",
                "body": {
                    "objectID": "doc1",
                    "title": "first version",
                    "_vectors": {"default": [1.0, 0.0, 0.0]}
                }
            }),
        ),
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc1",
                "body": {
                    "objectID": "doc1",
                    "title": "second version",
                    "_vectors": {"default": [0.0, 1.0, 0.0]}
                }
            }),
        ),
    ];

    setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(vi_arc.is_some(), "VectorIndex should exist after recovery");
    let vi_lock = vi_arc.unwrap();
    let guard = vi_lock.read().unwrap();
    assert_eq!(guard.len(), 1, "re-upsert should not duplicate doc1");

    // The vector should be the SECOND one (latest wins)
    let results = guard.search(&[0.0, 1.0, 0.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "doc1");
}

/// TODO: Document test_load_vector_index_skips_when_already_loaded.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_load_vector_index_skips_when_already_loaded() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "skip_load_t";
    let tenant_path = tmp.path().join(tenant_id);

    // Create tenant on disk
    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save a VectorIndex with 2 docs to disk
    let mut vi_disk = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi_disk.add("disk_doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi_disk.add("disk_doc2", &[0.0, 1.0, 0.0]).unwrap();
    vi_disk.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());

    // Pre-populate vector_indices with a DIFFERENT VectorIndex (1 doc)
    let mut vi_mem = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi_mem.add("mem_doc1", &[0.0, 0.0, 1.0]).unwrap();
    manager.set_vector_index(tenant_id, vi_mem);

    // Now call get_or_load — load_vector_index should skip because already populated
    manager.get_or_load(tenant_id).unwrap();

    // Verify we still have the in-memory version (1 doc), NOT the disk version (2 docs)
    let vi_arc = manager.get_vector_index(tenant_id).unwrap();
    let guard = vi_arc.read().unwrap();
    assert_eq!(
        guard.len(),
        1,
        "should keep in-memory index, not overwrite from disk"
    );
    let results = guard.search(&[0.0, 0.0, 1.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "mem_doc1");
}

/// TODO: Document test_full_crash_recovery_vectors_available.
#[cfg(feature = "vector-search")]
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn test_full_crash_recovery_vectors_available() {
    use crate::security::test_helpers::AllowLocalUrlsGuard;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Full hydration path through IndexSettings::load with a wiremock
    // loopback URL — opt in to the SSRF policy like an operator running a
    // local model server would. See crate::security::test_helpers for the
    // discipline behind this guard.
    let _allow_local = AllowLocalUrlsGuard::enable();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "embedding": [0.7, 0.8, 0.9]
        })))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let tenant_id = "crash_rec_t";

    // Phase 1: Create manager, add docs with embedder, let commit happen
    {
        let manager = IndexManager::new(tmp.path());
        manager.create_tenant(tenant_id).unwrap();

        // Configure embedder in settings
        let tenant_path = tmp.path().join(tenant_id);
        let settings = crate::index::settings::IndexSettings {
            embedders: Some(HashMap::from([(
                "default".to_string(),
                serde_json::json!({
                    "source": "rest",
                    "url": format!("{}/embed", server.uri()),
                    "request": {"input": "{{text}}"},
                    "response": {"embedding": "{{embedding}}"},
                    "dimensions": 3
                }),
            )])),
            ..Default::default()
        };
        settings.save(tenant_path.join("settings.json")).unwrap();

        // Add docs through write queue (which creates oplog entries)
        let docs = vec![Document {
            id: "doc1".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                crate::types::FieldValue::Text("recovery test".to_string()),
            )]),
        }];
        manager.add_documents_sync(tenant_id, docs).await.unwrap();

        // Verify vectors exist in memory
        let vi_arc = manager.get_vector_index(tenant_id);
        assert!(vi_arc.is_some(), "vectors should be in memory after add");
    }

    // Phase 2: Simulate crash — create new IndexManager
    {
        let manager2 = IndexManager::new(tmp.path());
        manager2.get_or_load(tenant_id).unwrap();

        // Vectors should be loaded from disk (saved after commit)
        let vi_arc = manager2.get_vector_index(tenant_id);
        assert!(
            vi_arc.is_some(),
            "vectors should survive manager restart (loaded from disk)"
        );
        let vi_lock = vi_arc.unwrap();
        let guard = vi_lock.read().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard.dimensions(), 3);
    }
}

// ── Fingerprint integration tests (8.18) ──

/// TODO: Document test_fingerprint_match_loads_vectors.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_fingerprint_match_loads_vectors() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "fp_match_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save settings with a rest embedder
    // url/request/response are required by IndexSettings::load (which now
    // runs full intake-style validation at the disk-load trust boundary
    // post-Plan-B SoC split). We use an RFC 5737 TEST-NET-3 address
    // (203.0.113.0/24) so the URL is a) syntactically valid, b) never
    // routes to a real host, and c) passes the SSRF policy with the env
    // var unset — this test cares about fingerprint matching, not about
    // outbound URL policy.
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "rest",
                "model": "text-embedding-3-small",
                "url": "http://203.0.113.42/embed",
                "request": {"input": "{{text}}"},
                "response": {"embedding": "{{embedding}}"},
                "dimensions": 3
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Save VectorIndex
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    // Save matching fingerprint
    let configs = vec![(
        "default".to_string(),
        crate::vector::config::EmbedderConfig {
            source: crate::vector::config::EmbedderSource::Rest,
            model: Some("text-embedding-3-small".into()),
            dimensions: Some(3),
            ..Default::default()
        },
    )];
    let fp = crate::vector::config::EmbedderFingerprint::from_configs(&configs, 3);
    fp.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    assert!(
        manager.get_vector_index(tenant_id).is_some(),
        "vectors should load when fingerprint matches"
    );
}

/// TODO: Document test_fingerprint_mismatch_skips_vectors.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_fingerprint_mismatch_skips_vectors() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "fp_mismatch_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Settings with model B
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "openAi",
                "model": "text-embedding-3-large",
                "dimensions": 3,
                "apiKey": "sk-test"
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Save VectorIndex
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    // Save fingerprint with model A (MISMATCH)
    let configs = vec![(
        "default".to_string(),
        crate::vector::config::EmbedderConfig {
            source: crate::vector::config::EmbedderSource::OpenAi,
            model: Some("text-embedding-3-small".into()),
            dimensions: Some(3),
            ..Default::default()
        },
    )];
    let fp = crate::vector::config::EmbedderFingerprint::from_configs(&configs, 3);
    fp.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    assert!(
        manager.get_vector_index(tenant_id).is_none(),
        "vectors should NOT load when fingerprint mismatches (model changed)"
    );
}

/// TODO: Document test_no_fingerprint_file_loads_vectors_anyway.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_no_fingerprint_file_loads_vectors_anyway() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "nofp_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save settings with embedder. See test_fingerprint_match_loads_vectors
    // for the rationale on the TEST-NET-3 URL and missing-fields fix —
    // this test exercises the same load path through IndexSettings::load.
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "rest",
                "model": "text-embedding-3-small",
                "url": "http://203.0.113.42/embed",
                "request": {"input": "{{text}}"},
                "response": {"embedding": "{{embedding}}"},
                "dimensions": 3
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Save VectorIndex but NO fingerprint.json (backward compat)
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    assert!(
        manager.get_vector_index(tenant_id).is_some(),
        "vectors should load when no fingerprint file exists (backward compat)"
    );
}

/// TODO: Document test_fingerprint_mismatch_template_change_skips.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_fingerprint_mismatch_template_change_skips() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "fp_tmpl_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Settings with NEW template
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "rest",
                "model": "model-a",
                "dimensions": 3,
                "documentTemplate": "{{doc.title}}"
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Save VectorIndex
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    // Save fingerprint with OLD template (MISMATCH)
    let configs = vec![(
        "default".to_string(),
        crate::vector::config::EmbedderConfig {
            source: crate::vector::config::EmbedderSource::Rest,
            model: Some("model-a".into()),
            dimensions: Some(3),
            document_template: Some("{{doc.title}} {{doc.body}}".into()),
            ..Default::default()
        },
    )];
    let fp = crate::vector::config::EmbedderFingerprint::from_configs(&configs, 3);
    fp.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    assert!(
        manager.get_vector_index(tenant_id).is_none(),
        "vectors should NOT load when document_template changed"
    );
}

// ── Memory accounting tests (8.21) ──

/// TODO: Document test_vector_memory_usage_with_indices.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_memory_usage_with_indices() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("mem_t").unwrap();

    // Create a VectorIndex with some vectors
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.add("doc2", &[0.0, 1.0, 0.0]).unwrap();
    vi.add("doc3", &[0.0, 0.0, 1.0]).unwrap();
    manager.set_vector_index("mem_t", vi);

    let usage = manager.vector_memory_usage();
    assert!(
        usage > 0,
        "vector_memory_usage should be > 0 when vectors exist, got {}",
        usage
    );
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_memory_usage_no_indices() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let usage = manager.vector_memory_usage();
    assert_eq!(usage, 0, "vector_memory_usage should be 0 with no indices");
}

// ── HTTP integration tests (8.25) ──

/// TODO: Document test_vectors_survive_manager_restart.
#[cfg(feature = "vector-search")]
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn test_vectors_survive_manager_restart() {
    use crate::security::test_helpers::AllowLocalUrlsGuard;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _allow_local = AllowLocalUrlsGuard::enable();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "embedding": [0.5, 0.6, 0.7]
        })))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let tenant_id = "restart_surv_t";

    // Phase 1: Create manager, add docs with embedder, verify vectors exist
    {
        let manager = IndexManager::new(tmp.path());
        manager.create_tenant(tenant_id).unwrap();

        let tenant_path = tmp.path().join(tenant_id);
        let settings = crate::index::settings::IndexSettings {
            embedders: Some(HashMap::from([(
                "default".to_string(),
                serde_json::json!({
                    "source": "rest",
                    "url": format!("{}/embed", server.uri()),
                    "request": {"input": "{{text}}"},
                    "response": {"embedding": "{{embedding}}"},
                    "dimensions": 3
                }),
            )])),
            ..Default::default()
        };
        settings.save(tenant_path.join("settings.json")).unwrap();

        let docs = vec![
            Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    crate::types::FieldValue::Text("alpha bravo".to_string()),
                )]),
            },
            Document {
                id: "doc2".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    crate::types::FieldValue::Text("charlie delta".to_string()),
                )]),
            },
        ];
        manager.add_documents_sync(tenant_id, docs).await.unwrap();

        // Verify vectors exist in memory
        let vi_arc = manager
            .get_vector_index(tenant_id)
            .expect("vectors should exist");
        let guard = vi_arc.read().unwrap();
        assert_eq!(guard.len(), 2, "should have 2 vectors");
        // Verify search works
        let results = guard.search(&[0.5, 0.6, 0.7], 2).unwrap();
        assert_eq!(results.len(), 2, "search should return 2 results");
    }

    // Phase 2: Restart — create new IndexManager with same base_path
    {
        let manager2 = IndexManager::new(tmp.path());
        manager2.get_or_load(tenant_id).unwrap();

        // Vectors should be loaded from disk
        let vi_arc = manager2.get_vector_index(tenant_id);
        assert!(vi_arc.is_some(), "vectors should survive manager restart");

        let vi_lock = vi_arc.unwrap();
        let guard = vi_lock.read().unwrap();
        assert_eq!(guard.len(), 2, "should still have 2 vectors after restart");
        assert_eq!(guard.dimensions(), 3);

        // Verify search still works after restart
        let results = guard.search(&[0.5, 0.6, 0.7], 2).unwrap();
        assert_eq!(
            results.len(),
            2,
            "search should return 2 results after restart"
        );
    }
}

/// TODO: Document test_vectors_lost_when_embedder_model_changes.
#[cfg(feature = "vector-search")]
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn test_vectors_lost_when_embedder_model_changes() {
    use crate::security::test_helpers::AllowLocalUrlsGuard;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _allow_local = AllowLocalUrlsGuard::enable();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "embedding": [0.1, 0.2, 0.3]
        })))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let tenant_id = "model_chg_t";
    let tenant_path = tmp.path().join(tenant_id);

    // Phase 1: Add docs with model A (REST embedder)
    {
        let manager = IndexManager::new(tmp.path());
        manager.create_tenant(tenant_id).unwrap();

        let settings = crate::index::settings::IndexSettings {
            embedders: Some(HashMap::from([(
                "default".to_string(),
                serde_json::json!({
                    "source": "rest",
                    "model": "model-a",
                    "url": format!("{}/embed", server.uri()),
                    "request": {"input": "{{text}}"},
                    "response": {"embedding": "{{embedding}}"},
                    "dimensions": 3
                }),
            )])),
            ..Default::default()
        };
        settings.save(tenant_path.join("settings.json")).unwrap();

        let docs = vec![Document {
            id: "doc1".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                crate::types::FieldValue::Text("test doc".to_string()),
            )]),
        }];
        manager.add_documents_sync(tenant_id, docs).await.unwrap();

        assert!(
            manager.get_vector_index(tenant_id).is_some(),
            "vectors should exist after Phase 1"
        );
    }

    // Phase 2: Change settings to model B, restart
    {
        let settings = crate::index::settings::IndexSettings {
            embedders: Some(HashMap::from([(
                "default".to_string(),
                serde_json::json!({
                    "source": "rest",
                    "model": "model-b",
                    "url": format!("{}/embed", server.uri()),
                    "request": {"input": "{{text}}"},
                    "response": {"embedding": "{{embedding}}"},
                    "dimensions": 3
                }),
            )])),
            ..Default::default()
        };
        settings.save(tenant_path.join("settings.json")).unwrap();

        let manager2 = IndexManager::new(tmp.path());
        manager2.get_or_load(tenant_id).unwrap();

        // Vectors should NOT be loaded — fingerprint mismatch
        assert!(
            manager2.get_vector_index(tenant_id).is_none(),
            "vectors should NOT load when embedder model changes (fingerprint mismatch)"
        );
    }
}
