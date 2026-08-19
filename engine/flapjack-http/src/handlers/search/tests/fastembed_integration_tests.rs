//! Stub summary for fastembed_integration_tests.rs.
use super::*;
use crate::dto::HybridSearchParams;
use flapjack::index::settings::IndexSettings;
use flapjack::types::{Document, FieldValue};
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

fn save_settings(state: &Arc<AppState>, index_name: &str, settings: &IndexSettings) {
    let dir = state.manager.base_path.join(index_name);
    std::fs::create_dir_all(&dir).unwrap();
    settings.save(dir.join("settings.json")).unwrap();
}

fn fastembed_settings() -> IndexSettings {
    let mut embedders = HashMap::new();
    embedders.insert(
        "default".to_string(),
        serde_json::json!({ "source": "fastEmbed" }),
    );
    IndexSettings {
        embedders: Some(embedders),
        ..Default::default()
    }
}

/// TODO: Document test_fastembed_hybrid_search_end_to_end.
#[tokio::test]
async fn test_fastembed_hybrid_search_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "fe_hybrid_e2e";

    state.manager.create_tenant(idx).unwrap();
    save_settings(&state, idx, &fastembed_settings());

    let docs = vec![
        make_doc("doc1", "machine learning algorithms for data science"),
        make_doc("doc2", "neural networks and deep learning models"),
        make_doc("doc3", "cooking recipes for Italian pasta dishes"),
    ];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    // Vector index should be auto-created with 384 dimensions (BGESmallENV15)
    let vi = state
        .manager
        .get_vector_index(idx)
        .expect("VectorIndex should be auto-created after fastembed add");
    {
        let vi_read = vi.read().unwrap();
        assert_eq!(vi_read.len(), 3, "all 3 docs should be embedded");
        assert_eq!(
            vi_read.dimensions(),
            384,
            "BGESmallENV15 produces 384-dim vectors"
        );
    }

    // Hybrid search — fastembed will embed the query at search time
    let req = SearchRequest {
        query: "machine learning".to_string(),
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
    // doc1 should rank high (BM25 match + semantic similarity)
    let ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"doc1"), "doc1 should be in results");
}
