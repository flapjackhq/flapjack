//! Stub summary for auto_embed_integration_tests.rs.
use super::*;
use crate::dto::HybridSearchParams;
use flapjack::index::settings::IndexSettings;
use flapjack::security::test_helpers::AllowLocalUrlsGuard;
use flapjack::types::{Document, FieldValue};
use serial_test::serial;
use std::collections::HashMap;
use tempfile::TempDir;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_doc(id: &str, text: &str) -> Document {
    let mut fields = HashMap::new();
    fields.insert("title".to_string(), FieldValue::Text(text.to_string()));
    Document {
        id: id.to_string(),
        fields,
    }
}

fn save_settings(state: &Arc<AppState>, index_name: &str, settings: &IndexSettings) {
    let dir = state.manager.base_path.join(index_name);
    std::fs::create_dir_all(&dir).unwrap();
    settings.save(dir.join("settings.json")).unwrap();
}

/// TODO: Document rest_embedder_settings.
fn rest_embedder_settings(server_uri: &str) -> IndexSettings {
    let mut embedders = HashMap::new();
    embedders.insert(
        "default".to_string(),
        serde_json::json!({
            "source": "rest",
            "url": format!("{}/embed", server_uri),
            "request": {"input": "{{text}}"},
            "response": {"embedding": "{{embedding}}"},
            "dimensions": 3
        }),
    );
    IndexSettings {
        embedders: Some(embedders),
        ..Default::default()
    }
}

/// Full pipeline: add documents → auto-embed via REST → hybrid search finds them.
#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn test_add_documents_and_hybrid_search() {
    // Hybrid-search integration goes through IndexSettings::load, which
    // now runs the SSOT outbound-URL safety check at the disk-load trust
    // boundary (Plan B SoC split). wiremock binds loopback, so this test
    // simulates an operator opt-in via the same env var an operator would
    // set to run a local model server. See flapjack::security::test_helpers.
    let _allow_local = AllowLocalUrlsGuard::enable();
    let server = MockServer::start().await;

    // Return different vectors based on document content
    Mock::given(method("POST"))
        .and(body_string_contains("machine"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"embedding": [0.9, 0.1, 0.0]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("neural"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"embedding": [0.7, 0.3, 0.0]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("cooking"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"embedding": [0.0, 0.1, 0.9]})),
        )
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "int_add_hybrid";

    state.manager.create_tenant(idx).unwrap();
    save_settings(&state, idx, &rest_embedder_settings(&server.uri()));

    let docs = vec![
        make_doc("doc1", "machine learning algorithms"),
        make_doc("doc2", "neural networks deep learning"),
        make_doc("doc3", "cooking recipes for beginners"),
    ];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    // Vector index should be auto-created by write queue
    assert!(
        state.manager.get_vector_index(idx).is_some(),
        "VectorIndex should be auto-created after add_documents_sync"
    );

    // Cache query vector for search (close to doc1's [0.9, 0.1, 0.0])
    state
        .embedder_store
        .query_cache
        .insert("default", "machine", vec![1.0, 0.0, 0.0]);

    let req = SearchRequest {
        query: "machine".to_string(),
        hybrid: Some(HybridSearchParams {
            semantic_ratio: 0.5,
            embedder: "default".to_string(),
        }),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), idx.to_string(), req)
        .await
        .unwrap();
    let hits = result.0["hits"].as_array().unwrap();

    assert!(!hits.is_empty(), "hybrid search should return results");
    // doc1 should rank high (BM25 match + closest vector)
    let ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"doc1"), "doc1 should be in results");
}

/// User-provided vectors: no embedding API calls, vectors stored directly.
#[tokio::test]
async fn test_add_documents_with_vectors_field_and_search() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "int_vectors_field";

    state.manager.create_tenant(idx).unwrap();

    let mut embedders = HashMap::new();
    embedders.insert(
        "default".to_string(),
        serde_json::json!({
            "source": "userProvided",
            "dimensions": 3
        }),
    );
    let settings = IndexSettings {
        embedders: Some(embedders),
        ..Default::default()
    };
    save_settings(&state, idx, &settings);

    // Documents with _vectors field
    let doc1 = Document {
        id: "doc1".to_string(),
        fields: {
            let mut f = HashMap::new();
            f.insert(
                "title".to_string(),
                FieldValue::Text("machine learning".to_string()),
            );
            let mut vecs = HashMap::new();
            vecs.insert(
                "default".to_string(),
                FieldValue::Array(vec![
                    FieldValue::Float(0.9),
                    FieldValue::Float(0.1),
                    FieldValue::Float(0.0),
                ]),
            );
            f.insert("_vectors".to_string(), FieldValue::Object(vecs));
            f
        },
    };
    let doc2 = Document {
        id: "doc2".to_string(),
        fields: {
            let mut f = HashMap::new();
            f.insert(
                "title".to_string(),
                FieldValue::Text("cooking recipes".to_string()),
            );
            let mut vecs = HashMap::new();
            vecs.insert(
                "default".to_string(),
                FieldValue::Array(vec![
                    FieldValue::Float(0.0),
                    FieldValue::Float(0.1),
                    FieldValue::Float(0.9),
                ]),
            );
            f.insert("_vectors".to_string(), FieldValue::Object(vecs));
            f
        },
    };

    state
        .manager
        .add_documents_sync(idx, vec![doc1, doc2])
        .await
        .unwrap();

    // Vector index auto-created from _vectors
    assert!(state.manager.get_vector_index(idx).is_some());

    // Cache query vector close to doc1
    state
        .embedder_store
        .query_cache
        .insert("default", "machine", vec![1.0, 0.0, 0.0]);

    let req = SearchRequest {
        query: "machine".to_string(),
        hybrid: Some(HybridSearchParams {
            semantic_ratio: 1.0, // Pure vector search
            embedder: "default".to_string(),
        }),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), idx.to_string(), req)
        .await
        .unwrap();
    let hits = result.0["hits"].as_array().unwrap();

    assert!(hits.len() >= 2, "should return both docs");
    // doc1 should be first (closest to query vector)
    assert_eq!(
        hits[0]["objectID"].as_str().unwrap(),
        "doc1",
        "doc1 should be closest to query vector [1,0,0]"
    );
}

/// Delete removes document from vector index — no longer found by hybrid search.
#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn test_delete_document_removes_from_hybrid_search() {
    // Hybrid-search integration goes through IndexSettings::load, which
    // now runs the SSOT outbound-URL safety check at the disk-load trust
    // boundary (Plan B SoC split). wiremock binds loopback, so this test
    // simulates an operator opt-in via the same env var an operator would
    // set to run a local model server. See flapjack::security::test_helpers.
    let _allow_local = AllowLocalUrlsGuard::enable();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"embedding": [0.9, 0.1, 0.0]})),
        )
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "int_delete";

    state.manager.create_tenant(idx).unwrap();
    save_settings(&state, idx, &rest_embedder_settings(&server.uri()));

    state
        .manager
        .add_documents_sync(idx, vec![make_doc("doc1", "machine learning")])
        .await
        .unwrap();

    // Verify vector index has the document
    {
        let vi_arc = state.manager.get_vector_index(idx).unwrap();
        let vi = vi_arc.read().unwrap();
        assert_eq!(vi.len(), 1);
    }

    // Delete the document
    state
        .manager
        .delete_documents_sync(idx, vec!["doc1".to_string()])
        .await
        .unwrap();

    // Vector index should be empty after delete
    {
        let vi_arc = state.manager.get_vector_index(idx).unwrap();
        let vi = vi_arc.read().unwrap();
        assert_eq!(vi.len(), 0, "vector index should be empty after delete");
    }

    // Hybrid search should return nothing
    state
        .embedder_store
        .query_cache
        .insert("default", "machine", vec![1.0, 0.0, 0.0]);

    let req = SearchRequest {
        query: "machine".to_string(),
        hybrid: Some(HybridSearchParams {
            semantic_ratio: 1.0,
            embedder: "default".to_string(),
        }),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), idx.to_string(), req)
        .await
        .unwrap();
    let hits = result.0["hits"].as_array().unwrap();
    assert!(
        hits.is_empty(),
        "deleted doc should not appear in hybrid search"
    );
}

/// Upsert replaces the vector — hybrid search results change accordingly.
#[tokio::test]
#[serial(flapjack_outbound_url_policy)]
async fn test_upsert_document_updates_vector() {
    // Hybrid-search integration goes through IndexSettings::load, which
    // now runs the SSOT outbound-URL safety check at the disk-load trust
    // boundary (Plan B SoC split). wiremock binds loopback, so this test
    // simulates an operator opt-in via the same env var an operator would
    // set to run a local model server. See flapjack::security::test_helpers.
    let _allow_local = AllowLocalUrlsGuard::enable();
    let server = MockServer::start().await;

    // Different vectors based on content
    Mock::given(method("POST"))
        .and(body_string_contains("original"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"embedding": [1.0, 0.0, 0.0]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("updated"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"embedding": [0.0, 0.0, 1.0]})),
        )
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "int_upsert";

    state.manager.create_tenant(idx).unwrap();
    save_settings(&state, idx, &rest_embedder_settings(&server.uri()));

    // Add original document
    state
        .manager
        .add_documents_sync(idx, vec![make_doc("doc1", "original content")])
        .await
        .unwrap();

    // Verify initial vector is close to [1,0,0]
    {
        let vi_arc = state.manager.get_vector_index(idx).unwrap();
        let vi = vi_arc.read().unwrap();
        let results = vi.search(&[1.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(results[0].doc_id, "doc1");
        assert!(
            results[0].distance < 0.01,
            "initial vector should be close to [1,0,0]"
        );
    }

    // Upsert with different content (add_documents uses upsert semantics)
    state
        .manager
        .add_documents_sync(idx, vec![make_doc("doc1", "updated content")])
        .await
        .unwrap();

    // Vector should now be close to [0,0,1]
    {
        let vi_arc = state.manager.get_vector_index(idx).unwrap();
        let vi = vi_arc.read().unwrap();
        let results = vi.search(&[0.0, 0.0, 1.0], 1).unwrap();
        assert_eq!(results[0].doc_id, "doc1");
        assert!(
            results[0].distance < 0.01,
            "upserted vector should be close to [0,0,1], distance={}",
            results[0].distance
        );
    }

    // Hybrid search with query vector close to new embedding should find it
    state
        .embedder_store
        .query_cache
        .insert("default", "updated", vec![0.0, 0.0, 1.0]);

    let req = SearchRequest {
        query: "updated".to_string(),
        hybrid: Some(HybridSearchParams {
            semantic_ratio: 1.0,
            embedder: "default".to_string(),
        }),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), idx.to_string(), req)
        .await
        .unwrap();
    let hits = result.0["hits"].as_array().unwrap();

    assert!(!hits.is_empty(), "upserted doc should be found");
    assert_eq!(hits[0]["objectID"].as_str().unwrap(), "doc1");
}
