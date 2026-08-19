//! Stub summary for fastembed_rejected_tests.rs.
use super::*;
use flapjack::index::settings::IndexSettings;
use flapjack::types::{Document, FieldValue};
use std::collections::HashMap;
use tempfile::TempDir;

fn save_settings(state: &Arc<AppState>, index_name: &str, settings: &IndexSettings) {
    let dir = state.manager.base_path.join(index_name);
    std::fs::create_dir_all(&dir).unwrap();
    settings.save(dir.join("settings.json")).unwrap();
}
/// TODO: Document test_fastembed_config_rejected_without_feature.
#[tokio::test]
async fn test_fastembed_config_rejected_without_feature() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let idx = "fe_rejected";

    state.manager.create_tenant(idx).unwrap();

    let mut embedders = HashMap::new();
    embedders.insert(
        "default".to_string(),
        serde_json::json!({ "source": "fastEmbed" }),
    );
    let settings = IndexSettings {
        embedders: Some(embedders),
        ..Default::default()
    };
    save_settings(&state, idx, &settings);

    // Add a document — embedding should fail, but BM25 indexing should succeed
    let mut fields = HashMap::new();
    fields.insert(
        "title".to_string(),
        FieldValue::Text("test document for rejected embed".to_string()),
    );
    let docs = vec![Document {
        id: "doc1".to_string(),
        fields,
    }];
    state.manager.add_documents_sync(idx, docs).await.unwrap();

    // BM25 search should still work — document was indexed in Tantivy
    let req = SearchRequest {
        query: "test document".to_string(),
        ..Default::default()
    };
    let result = search_single(State(state.clone()), idx.to_string(), req)
        .await
        .unwrap();
    let hits = result.0["hits"].as_array().unwrap();
    assert!(
        !hits.is_empty(),
        "BM25 search should work despite embedding failure"
    );
    assert_eq!(hits[0]["objectID"].as_str().unwrap(), "doc1");

    // Vector index should NOT exist (embedding failed)
    assert!(
        state.manager.get_vector_index(idx).is_none()
            || state
                .manager
                .get_vector_index(idx)
                .map(|vi| vi.read().unwrap().is_empty())
                .unwrap_or(true),
        "vector index should be empty — fastembed not available without feature"
    );
}
