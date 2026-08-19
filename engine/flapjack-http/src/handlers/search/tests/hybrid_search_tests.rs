//! Stub summary for hybrid_search_tests.rs.
use super::*;
use crate::dto::HybridSearchParams;
use flapjack::index::settings::{IndexMode, IndexSettings};
use flapjack::types::{Document, FieldValue};
use flapjack::vector::MetricKind;
use std::collections::HashMap;
use tempfile::TempDir;

fn make_doc(id: &str, text: &str) -> Document {
    let mut fields = HashMap::new();
    fields.insert("title".to_string(), FieldValue::Text(text.to_string()));
    Document {
        id: id.to_string(),
        fields,
    }
}

/// Save settings JSON to the expected path for a tenant.
fn save_settings(state: &Arc<AppState>, index_name: &str, settings: &IndexSettings) {
    let dir = state.manager.base_path.join(index_name);
    std::fs::create_dir_all(&dir).unwrap();
    settings.save(dir.join("settings.json")).unwrap();
}

/// Set up a VectorIndex with 3D cosine vectors for testing.
/// Returns the query vector to use for search.
fn setup_vector_index(state: &Arc<AppState>, index_name: &str) -> Vec<f32> {
    let mut vi = flapjack::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    // doc1: very close to query [1.0, 0.0, 0.0]
    vi.add("doc1", &[0.99, 0.1, 0.0]).unwrap();
    // doc2: moderate distance
    vi.add("doc2", &[0.5, 0.5, 0.0]).unwrap();
    // doc3: very far from query
    vi.add("doc3", &[0.0, 0.1, 0.99]).unwrap();
    // doc4: close to query but different keyword content
    vi.add("doc4", &[0.95, 0.15, 0.0]).unwrap();
    state.manager.set_vector_index(index_name, vi);
    vec![1.0, 0.0, 0.0]
}

/// Settings with a UserProvided embedder (no HTTP calls needed).
fn settings_with_embedder() -> IndexSettings {
    let mut embedders = HashMap::new();
    embedders.insert(
        "default".to_string(),
        serde_json::json!({
            "source": "userProvided",
            "dimensions": 3
        }),
    );
    IndexSettings {
        embedders: Some(embedders),
        ..Default::default()
    }
}

/// Pre-populate the query cache so the embedder is never actually called.
fn cache_query_vector(state: &Arc<AppState>, embedder_name: &str, query: &str, vector: Vec<f32>) {
    state
        .embedder_store
        .query_cache
        .insert(embedder_name, query, vector);
}
/// TODO: Document test_hybrid_search_pure_bm25.
#[tokio::test]
async fn test_hybrid_search_pure_bm25() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "test_pure_bm25";
    state.manager.create_tenant(idx).unwrap();

    let docs = vec![
        make_doc("doc1", "machine learning algorithms"),
        make_doc("doc2", "deep learning neural networks"),
        make_doc("doc3", "cooking recipes for beginners"),
    ];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    let settings = settings_with_embedder();
    save_settings(&state, idx, &settings);

    let query_vec = setup_vector_index(&state, idx);
    cache_query_vector(&state, "default", "learning", query_vec);

    // Search with hybrid semanticRatio=0.0 → pure BM25
    let req = SearchRequest {
        query: "learning".to_string(),
        hybrid: Some(HybridSearchParams {
            semantic_ratio: 0.0,
            embedder: "default".to_string(),
        }),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), idx.to_string(), req)
        .await
        .unwrap();
    let hits = result.0["hits"].as_array().unwrap();

    // With ratio=0.0, only BM25 matters. doc1 and doc2 contain "learning".
    assert!(hits.len() >= 2, "Expected at least 2 BM25 hits");
    let ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"doc1"));
    assert!(ids.contains(&"doc2"));
    // doc3 ("cooking recipes") should NOT appear for query "learning"
    assert!(!ids.contains(&"doc3"));
}
/// TODO: Document test_hybrid_search_pure_vector.
#[tokio::test]
async fn test_hybrid_search_pure_vector() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "test_pure_vector";
    state.manager.create_tenant(idx).unwrap();

    let docs = vec![
        make_doc("doc1", "machine learning algorithms"),
        make_doc("doc2", "deep learning neural networks"),
        make_doc("doc3", "cooking recipes for beginners"),
        make_doc("doc4", "artificial intelligence research"),
    ];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    let settings = settings_with_embedder();
    save_settings(&state, idx, &settings);

    let query_vec = setup_vector_index(&state, idx);
    cache_query_vector(&state, "default", "learning", query_vec);

    // Search with hybrid semanticRatio=1.0 → pure vector
    let req = SearchRequest {
        query: "learning".to_string(),
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

    // With ratio=1.0, order should follow vector similarity.
    // Vector distances from [1,0,0]: doc1 closest, doc4 next, doc2, doc3 farthest.
    assert!(
        hits.len() >= 3,
        "Expected at least 3 hits from vector search"
    );
    let first_id = hits[0]["objectID"].as_str().unwrap();
    assert_eq!(first_id, "doc1", "doc1 should be first (closest vector)");

    // doc4 should appear even though "learning" has no keyword match in "artificial intelligence research"
    let ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"doc4"),
        "doc4 should appear via vector search even without keyword match"
    );
}
/// TODO: Document test_hybrid_search_blended.
#[tokio::test]
async fn test_hybrid_search_blended() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "test_blended";
    state.manager.create_tenant(idx).unwrap();

    let docs = vec![
        make_doc("doc1", "machine learning algorithms"),
        make_doc("doc2", "deep learning neural networks"),
        make_doc("doc3", "cooking recipes for beginners"),
        make_doc("doc4", "artificial intelligence research"),
    ];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    let settings = settings_with_embedder();
    save_settings(&state, idx, &settings);

    let query_vec = setup_vector_index(&state, idx);
    cache_query_vector(&state, "default", "learning", query_vec);

    // Pure BM25 search (no hybrid)
    let bm25_req = SearchRequest {
        query: "learning".to_string(),
        ..Default::default()
    };
    let bm25_result = search_single(State(state.clone()), idx.to_string(), bm25_req)
        .await
        .unwrap();
    let bm25_ids: Vec<&str> = bm25_result.0["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();

    // Blended search (ratio=0.5)
    let hybrid_req = SearchRequest {
        query: "learning".to_string(),
        hybrid: Some(HybridSearchParams {
            semantic_ratio: 0.5,
            embedder: "default".to_string(),
        }),
        ..Default::default()
    };
    let hybrid_result = search_single(State(state.clone()), idx.to_string(), hybrid_req)
        .await
        .unwrap();
    let hybrid_ids: Vec<&str> = hybrid_result.0["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();

    // Blended results should include docs from both BM25 and vector.
    // doc4 only appears via vector (no "learning" keyword).
    assert!(
        hybrid_ids.contains(&"doc4"),
        "Blended search should include vector-only doc4, got {:?}",
        hybrid_ids
    );
    // BM25-only search should NOT include doc4
    assert!(
        !bm25_ids.contains(&"doc4"),
        "BM25 search should not include doc4"
    );
}
/// TODO: Document test_hybrid_search_no_embedder_fallback.
#[tokio::test]
async fn test_hybrid_search_no_embedder_fallback() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "test_no_embedder";
    state.manager.create_tenant(idx).unwrap();

    let docs = vec![
        make_doc("doc1", "machine learning algorithms"),
        make_doc("doc2", "deep learning neural networks"),
    ];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    // Settings with neuralSearch mode but NO embedders configured
    let settings = IndexSettings {
        mode: Some(IndexMode::NeuralSearch),
        ..Default::default()
    };
    save_settings(&state, idx, &settings);

    let req = SearchRequest {
        query: "learning".to_string(),
        mode: Some(IndexMode::NeuralSearch),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), idx.to_string(), req)
        .await
        .unwrap();
    let response = &result.0;

    // Should still return BM25 results (graceful fallback)
    let hits = response["hits"].as_array().unwrap();
    assert!(!hits.is_empty(), "Should fall back to BM25 results");

    // Should include a warning message about fallback
    assert!(
        response.get("message").is_some(),
        "Response should include 'message' field with fallback warning"
    );
    let msg = response["message"].as_str().unwrap();
    assert!(
        msg.contains("Hybrid search unavailable"),
        "Message should indicate hybrid search unavailable, got: {}",
        msg
    );
}
/// TODO: Document test_hybrid_search_neural_mode_default_params.
#[tokio::test]
async fn test_hybrid_search_neural_mode_default_params() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "test_neural_mode";
    state.manager.create_tenant(idx).unwrap();

    let docs = vec![
        make_doc("doc1", "machine learning algorithms"),
        make_doc("doc2", "deep learning neural networks"),
        make_doc("doc3", "cooking recipes for beginners"),
        make_doc("doc4", "artificial intelligence research"),
    ];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    let mut settings = settings_with_embedder();
    settings.mode = Some(IndexMode::NeuralSearch);
    save_settings(&state, idx, &settings);

    let query_vec = setup_vector_index(&state, idx);
    // Cache with embedder "default" and query "learning"
    cache_query_vector(&state, "default", "learning", query_vec);

    // mode=neuralSearch with NO explicit hybrid param →
    // should synthesize hybrid with ratio=0.5, embedder="default"
    let req = SearchRequest {
        query: "learning".to_string(),
        mode: Some(IndexMode::NeuralSearch),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), idx.to_string(), req)
        .await
        .unwrap();
    let hits = result.0["hits"].as_array().unwrap();
    let ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();

    // neuralSearch mode should trigger hybrid → doc4 should appear via vector
    assert!(
        ids.contains(&"doc4"),
        "neuralSearch mode should trigger hybrid and include vector-only doc4, got {:?}",
        ids
    );
}
/// TODO: Document test_hybrid_search_empty_vector_index_fallback.
#[tokio::test]
async fn test_hybrid_search_empty_vector_index_fallback() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "test_empty_vi";
    state.manager.create_tenant(idx).unwrap();

    let docs = vec![
        make_doc("doc1", "machine learning algorithms"),
        make_doc("doc2", "deep learning neural networks"),
    ];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    let settings = settings_with_embedder();
    save_settings(&state, idx, &settings);

    // Create an empty VectorIndex (no vectors added)
    let vi = flapjack::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    state.manager.set_vector_index(idx, vi);

    cache_query_vector(&state, "default", "learning", vec![1.0, 0.0, 0.0]);

    let req = SearchRequest {
        query: "learning".to_string(),
        hybrid: Some(HybridSearchParams {
            semantic_ratio: 0.5,
            embedder: "default".to_string(),
        }),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), idx.to_string(), req)
        .await
        .unwrap();
    let response = &result.0;

    // Should still return BM25 results
    let hits = response["hits"].as_array().unwrap();
    assert!(!hits.is_empty(), "Should return BM25 results");

    // Should include fallback message about empty vector index
    assert!(
        response.get("message").is_some(),
        "Response should include fallback message for empty vector index"
    );
}
/// TODO: Document test_hybrid_search_vector_only_docs_fetched.
#[tokio::test]
async fn test_hybrid_search_vector_only_docs_fetched() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "test_vector_only";
    state.manager.create_tenant(idx).unwrap();

    let docs = vec![
        make_doc("doc1", "machine learning algorithms"),
        make_doc("doc2", "deep learning neural networks"),
        make_doc("doc3", "cooking recipes for beginners"),
        make_doc("doc4", "artificial intelligence research"),
    ];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    let settings = settings_with_embedder();
    save_settings(&state, idx, &settings);

    let query_vec = setup_vector_index(&state, idx);
    cache_query_vector(&state, "default", "learning", query_vec);

    // ratio=0.7 weights vector heavily → doc4 should surface
    let req = SearchRequest {
        query: "learning".to_string(),
        hybrid: Some(HybridSearchParams {
            semantic_ratio: 0.7,
            embedder: "default".to_string(),
        }),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), idx.to_string(), req)
        .await
        .unwrap();
    let hits = result.0["hits"].as_array().unwrap();
    let ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();

    // doc4 is NOT in BM25 results for "learning" but IS close in vector space.
    // It should be fetched via get_document and included in the fused results.
    assert!(
        ids.contains(&"doc4"),
        "Vector-only doc4 should be fetched and included, got {:?}",
        ids
    );
    // Verify doc4 has its full document data (title field)
    let doc4_hit = hits.iter().find(|h| h["objectID"] == "doc4").unwrap();
    assert_eq!(
        doc4_hit["title"].as_str().unwrap(),
        "artificial intelligence research",
        "Vector-only doc should have its full document fields"
    );
}

/// Ranking quality test: hybrid search surfaces semantically relevant docs
/// that pure BM25 misses entirely, and ranks them above keyword-only partial matches.
///
/// Scenario: user queries "comfortable office chair"
/// - doc1 ("office chair with lumbar support"): keyword match + vector close → #1
/// - doc2 ("ergonomic seating for better posture"): NO keyword match, vector closest → surfaced by hybrid
/// - doc3 ("office desk organizer"): partial keyword ("office") + vector far
/// - doc4 ("wooden dining chair set"): partial keyword ("chair") + vector far
///
/// BM25 alone cannot find doc2 at all. Hybrid search must:
/// 1. Surface doc2 (proves semantic recall)
/// 2. Rank doc2 above doc3 and doc4 (proves ranking quality)
#[tokio::test]
async fn test_hybrid_ranking_quality_semantic_beats_keyword() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "test_ranking_quality";
    state.manager.create_tenant(idx).unwrap();

    let docs = vec![
        make_doc("doc1", "office chair with lumbar support"),
        make_doc("doc2", "ergonomic seating for better posture"),
        make_doc("doc3", "office desk organizer"),
        make_doc("doc4", "wooden dining chair set"),
    ];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    let settings = settings_with_embedder();
    save_settings(&state, idx, &settings);

    // Vector space: query [1,0,0] represents "comfortable office seating" concept
    // doc2 is closest (same semantic concept, different words)
    // doc1 is close (semantic + keyword overlap)
    // doc3 and doc4 are far (different concepts)
    let mut vi = flapjack::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[0.92, 0.1, 0.0]).unwrap(); // close: office seating concept
    vi.add("doc2", &[0.98, 0.02, 0.0]).unwrap(); // closest: ergonomic seating
    vi.add("doc3", &[0.1, 0.95, 0.0]).unwrap(); // far: office supplies concept
    vi.add("doc4", &[0.15, 0.1, 0.9]).unwrap(); // far: dining furniture concept
    state.manager.set_vector_index(idx, vi);

    let query_vec = vec![1.0, 0.0, 0.0];
    cache_query_vector(&state, "default", "office chair", query_vec);

    // --- BM25 only (no hybrid) ---
    let bm25_req = SearchRequest {
        query: "office chair".to_string(),
        ..Default::default()
    };
    let bm25_result = search_single(State(state.clone()), idx.to_string(), bm25_req)
        .await
        .unwrap();
    let bm25_hits = bm25_result.0["hits"].as_array().unwrap();
    let bm25_ids: Vec<&str> = bm25_hits
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();

    // BM25 should NOT find doc2 (no keyword overlap with "office chair")
    assert!(
        !bm25_ids.contains(&"doc2"),
        "BM25 should not find 'ergonomic seating' for query 'office chair', got {:?}",
        bm25_ids
    );
    // BM25 should find doc1 (contains both "office" and "chair")
    assert!(
        bm25_ids.contains(&"doc1"),
        "BM25 should find doc1 which contains 'office chair', got {:?}",
        bm25_ids
    );

    // --- Hybrid search (ratio=0.5) ---
    let hybrid_req = SearchRequest {
        query: "office chair".to_string(),
        hybrid: Some(HybridSearchParams {
            semantic_ratio: 0.5,
            embedder: "default".to_string(),
        }),
        ..Default::default()
    };
    let hybrid_result = search_single(State(state.clone()), idx.to_string(), hybrid_req)
        .await
        .unwrap();
    let hybrid_hits = hybrid_result.0["hits"].as_array().unwrap();
    let hybrid_ids: Vec<&str> = hybrid_hits
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();

    // 1. Hybrid MUST surface doc2 (semantic recall)
    assert!(
        hybrid_ids.contains(&"doc2"),
        "Hybrid search must surface semantically relevant doc2 ('ergonomic seating'), got {:?}",
        hybrid_ids
    );

    // 2. doc1 should be #1 (strong in both BM25 and vector)
    assert_eq!(
        hybrid_ids[0], "doc1",
        "doc1 should be ranked #1 (keyword + semantic match), got {:?}",
        hybrid_ids
    );

    // 3. doc2 should rank above doc3 and doc4 (semantic relevance > partial keyword)
    let doc2_pos = hybrid_ids.iter().position(|&id| id == "doc2").unwrap();
    if let Some(doc3_pos) = hybrid_ids.iter().position(|&id| id == "doc3") {
        assert!(
            doc2_pos < doc3_pos,
            "doc2 (semantic match) should rank above doc3 (partial keyword only): doc2={}, doc3={}",
            doc2_pos,
            doc3_pos
        );
    }
    if let Some(doc4_pos) = hybrid_ids.iter().position(|&id| id == "doc4") {
        assert!(
            doc2_pos < doc4_pos,
            "doc2 (semantic match) should rank above doc4 (partial keyword only): doc2={}, doc4={}",
            doc2_pos,
            doc4_pos
        );
    }
}
/// TODO: Document test_hybrid_search_algolia_compat_no_hybrid.
#[tokio::test]
async fn test_hybrid_search_algolia_compat_no_hybrid() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "test_compat";
    state.manager.create_tenant(idx).unwrap();

    let docs = vec![
        make_doc("doc1", "machine learning algorithms"),
        make_doc("doc2", "deep learning neural networks"),
    ];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    // Standard Algolia search — no mode, no hybrid
    let req = SearchRequest {
        query: "learning".to_string(),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), idx.to_string(), req)
        .await
        .unwrap();
    let response = &result.0;

    // Verify standard response shape
    assert!(response.get("hits").is_some());
    assert!(response.get("nbHits").is_some());
    assert!(response.get("page").is_some());
    assert!(response.get("hitsPerPage").is_some());
    assert!(response.get("query").is_some());

    // No message field for standard search
    assert!(
        response.get("message").is_none(),
        "Standard search should not have 'message' field"
    );

    let hits = response["hits"].as_array().unwrap();
    assert!(hits.len() >= 2);
}
/// TODO: Document test_mode_b_hybrid_uses_variant_embedder_settings.
#[tokio::test]
async fn test_mode_b_hybrid_uses_variant_embedder_settings() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp)
        .with_experiments()
        .build_shared();
    let experiment_store = state
        .experiment_store
        .as_ref()
        .expect("experiment store should be configured")
        .clone();

    let original_index = "mode_b_hybrid_original";
    let variant_index = "mode_b_hybrid_variant";
    state.manager.create_tenant(original_index).unwrap();
    state.manager.create_tenant(variant_index).unwrap();
    state
        .manager
        .add_documents_sync(
            original_index,
            vec![make_doc("o1", "original keyword-only content")],
        )
        .await
        .unwrap();
    state
        .manager
        .add_documents_sync(
            variant_index,
            vec![make_doc("v1", "semantic-only document text")],
        )
        .await
        .unwrap();

    // Original index has no hybrid mode; variant index enables neural search.
    save_settings(&state, original_index, &IndexSettings::default());
    let mut variant_settings = settings_with_embedder();
    variant_settings.mode = Some(IndexMode::NeuralSearch);
    save_settings(&state, variant_index, &variant_settings);

    let mut vi = flapjack::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("v1", &[0.99, 0.1, 0.0]).unwrap();
    state.manager.set_vector_index(variant_index, vi);

    let query = "needle-query";
    cache_query_vector(&state, "default", query, vec![1.0, 0.0, 0.0]);

    let experiment = mode_b_experiment("exp-mode-b-hybrid", original_index, variant_index);
    experiment_store.create(experiment).unwrap();
    experiment_store.start("exp-mode-b-hybrid").unwrap();
    let running = experiment_store.get("exp-mode-b-hybrid").unwrap();
    let variant_token = find_user_token_for_arm(&running, "variant");

    let req = SearchRequest {
        query: query.to_string(),
        user_token: Some(variant_token),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), original_index.to_string(), req)
        .await
        .unwrap();
    let response = result.0;
    let hits = response["hits"]
        .as_array()
        .expect("response must include hits array");

    assert_eq!(
        response["index"], original_index,
        "response index must remain the requested index"
    );
    assert_eq!(
        response["indexUsed"], variant_index,
        "Mode B variant must expose effective index"
    );
    assert!(
        !hits.is_empty(),
        "hybrid Mode B variant search should return vector hits from variant index"
    );
    assert_eq!(
        hits[0]["objectID"], "v1",
        "vector-ranked result must come from variant index"
    );
}
/// TODO: Document test_mode_b_hybrid_control_stays_keyword_only.
#[tokio::test]
async fn test_mode_b_hybrid_control_stays_keyword_only() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp)
        .with_experiments()
        .build_shared();
    let experiment_store = state
        .experiment_store
        .as_ref()
        .expect("experiment store should be configured")
        .clone();

    let original_index = "mode_b_hybrid_control_original";
    let variant_index = "mode_b_hybrid_control_variant";
    state.manager.create_tenant(original_index).unwrap();
    state.manager.create_tenant(variant_index).unwrap();
    state
        .manager
        .add_documents_sync(
            original_index,
            vec![make_doc("o1", "needle-query control keyword hit")],
        )
        .await
        .unwrap();
    state
        .manager
        .add_documents_sync(
            variant_index,
            vec![make_doc("v1", "semantic-only document text")],
        )
        .await
        .unwrap();

    // Only the variant index is neural-enabled.
    save_settings(&state, original_index, &IndexSettings::default());
    let mut variant_settings = settings_with_embedder();
    variant_settings.mode = Some(IndexMode::NeuralSearch);
    save_settings(&state, variant_index, &variant_settings);

    let mut vi = flapjack::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("v1", &[0.99, 0.1, 0.0]).unwrap();
    state.manager.set_vector_index(variant_index, vi);

    let query = "needle-query";
    cache_query_vector(&state, "default", query, vec![1.0, 0.0, 0.0]);

    let experiment = mode_b_experiment("exp-mode-b-hybrid-control", original_index, variant_index);
    experiment_store.create(experiment).unwrap();
    experiment_store.start("exp-mode-b-hybrid-control").unwrap();
    let running = experiment_store.get("exp-mode-b-hybrid-control").unwrap();
    let control_token = find_user_token_for_arm(&running, "control");

    let req = SearchRequest {
        query: query.to_string(),
        user_token: Some(control_token),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), original_index.to_string(), req)
        .await
        .unwrap();
    let response = result.0;
    let hits = response["hits"]
        .as_array()
        .expect("response must include hits array");

    assert_eq!(
        response["index"], original_index,
        "control arm must keep requested/original index"
    );
    assert!(
        response.get("indexUsed").is_none(),
        "control arm must not expose indexUsed when no reroute happens"
    );
    assert!(
        response.get("message").is_none(),
        "control arm should remain keyword-only and not emit hybrid fallback warnings"
    );
    assert!(
        !hits.is_empty(),
        "control arm should still return keyword hits"
    );
    assert_eq!(
        hits[0]["objectID"], "o1",
        "control arm results must come from the original index"
    );
    assert_eq!(
        response["abTestVariantID"], "control",
        "control token must be annotated as control arm"
    );
}
