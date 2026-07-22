use super::*;
use crate::handlers::AppState;
use crate::test_helpers::body_json;
use axum::{http::StatusCode, Router};
use flapjack::types::Document;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use tempfile::TempDir;

/// Canonical 25-document input set shared by this KAT and the live HTTP
/// verifier (`engine/tests/search_pagination_live_http.sh`). Both consumers read
/// this one file so the served boundary is checked against the same documents
/// the in-process KAT uses.
const PAGINATION_FIXTURE_JSON: &str =
    include_str!("../../../../../tests/fixtures/search_pagination_known_answer.json");

fn save_settings(
    state: &Arc<AppState>,
    index_name: &str,
    settings: &flapjack::index::settings::IndexSettings,
) {
    let dir = state.manager.base_path.join(index_name);
    std::fs::create_dir_all(&dir).unwrap();
    settings.save(dir.join("settings.json")).unwrap();
}

/// Deserialize the shared fixture into the documents seeded by every KAT below.
fn pagination_docs() -> Vec<Document> {
    let raw: Vec<Value> = serde_json::from_str(PAGINATION_FIXTURE_JSON)
        .expect("pagination fixture must be a JSON array of documents");
    assert_eq!(
        raw.len(),
        25,
        "pagination known-answer expectations assume a 25-document fixture"
    );
    raw.iter()
        .map(|value| Document::from_json(value).expect("fixture document must parse"))
        .collect()
}

async fn create_pagination_index(
    state: &Arc<AppState>,
    index_name: &str,
    settings: flapjack::index::settings::IndexSettings,
) {
    state.manager.create_tenant(index_name).unwrap();
    save_settings(state, index_name, &settings);
    state
        .manager
        .add_documents_sync(index_name, pagination_docs())
        .await
        .unwrap();
}

async fn search_json(app: &Router, index_name: &str, request: Value) -> Value {
    let resp = post_search_simple(app, index_name, request).await;
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await
}

fn json_u64(body: &Value, key: &str) -> u64 {
    body[key]
        .as_u64()
        .unwrap_or_else(|| panic!("expected numeric response field {key}, got {}", body[key]))
}

fn hit_len(body: &Value) -> usize {
    body["hits"]
        .as_array()
        .expect("response must include hits array")
        .len()
}

fn hit_ids(body: &Value) -> Vec<String> {
    body["hits"]
        .as_array()
        .expect("response must include hits array")
        .iter()
        .map(|hit| {
            hit["objectID"]
                .as_str()
                .expect("hit must include string objectID")
                .to_string()
        })
        .collect()
}

fn hit_id_set(body: &Value) -> HashSet<String> {
    hit_ids(body).into_iter().collect()
}

fn assert_no_hit_id_overlap(left: &Value, right: &Value) {
    let left_ids = hit_id_set(left);
    let right_ids = hit_id_set(right);
    assert!(
        left_ids.is_disjoint(&right_ids),
        "page hit objectID sets must be non-overlapping: left={left_ids:?} right={right_ids:?}"
    );
}

#[tokio::test]
async fn search_pagination_known_answer_simple_search_uses_total_hits() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp, None);
    let index_name = "pagination_simple";
    create_pagination_index(
        &state,
        index_name,
        flapjack::index::settings::IndexSettings {
            searchable_attributes: Some(vec!["title".to_string()]),
            ..Default::default()
        },
    )
    .await;
    let app = search_app(state);

    let page_0 = search_json(
        &app,
        index_name,
        json!({"query": "pagination", "hitsPerPage": 10, "page": 0}),
    )
    .await;
    let page_2 = search_json(
        &app,
        index_name,
        json!({"query": "pagination", "hitsPerPage": 10, "page": 2}),
    )
    .await;

    assert_eq!(json_u64(&page_0, "nbHits"), 25);
    assert_eq!(json_u64(&page_0, "nbPages"), 3);
    assert_eq!(hit_len(&page_0), 10);
    assert_eq!(json_u64(&page_2, "nbHits"), 25);
    assert_eq!(json_u64(&page_2, "nbPages"), 3);
    assert_eq!(hit_len(&page_2), 5);
    assert_no_hit_id_overlap(&page_0, &page_2);
}

#[tokio::test]
async fn search_pagination_known_answer_distinct_uses_current_group_total_contract() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp, None);
    let index_name = "pagination_distinct";
    create_pagination_index(
        &state,
        index_name,
        flapjack::index::settings::IndexSettings {
            searchable_attributes: Some(vec!["title".to_string()]),
            attribute_for_distinct: Some("group".to_string()),
            ..Default::default()
        },
    )
    .await;
    let app = search_app(state);

    let page_0 = search_json(
        &app,
        index_name,
        json!({"query": "pagination", "hitsPerPage": 10, "page": 0, "distinct": true}),
    )
    .await;
    let page_2 = search_json(
        &app,
        index_name,
        json!({"query": "pagination", "hitsPerPage": 10, "page": 2, "distinct": true}),
    )
    .await;

    assert_eq!(json_u64(&page_0, "nbHits"), 5);
    assert_eq!(json_u64(&page_0, "nbPages"), 1);
    assert_eq!(hit_len(&page_0), 5);
    assert_eq!(json_u64(&page_2, "nbHits"), 5);
    assert_eq!(json_u64(&page_2, "nbPages"), 1);
    assert_eq!(hit_len(&page_2), 0);
}

#[tokio::test]
async fn search_pagination_known_answer_faceted_query_keeps_total_hits() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp, None);
    let index_name = "pagination_faceted";
    create_pagination_index(
        &state,
        index_name,
        flapjack::index::settings::IndexSettings {
            searchable_attributes: Some(vec!["title".to_string()]),
            attributes_for_faceting: vec!["category".to_string()],
            ..Default::default()
        },
    )
    .await;
    let app = search_app(state);

    let page_0 = search_json(
        &app,
        index_name,
        json!({"query": "pagination", "hitsPerPage": 10, "page": 0, "facets": ["category"]}),
    )
    .await;
    let page_2 = search_json(
        &app,
        index_name,
        json!({"query": "pagination", "hitsPerPage": 10, "page": 2, "facets": ["category"]}),
    )
    .await;

    assert_eq!(json_u64(&page_0, "nbHits"), 25);
    assert_eq!(json_u64(&page_0, "nbPages"), 3);
    assert_eq!(hit_len(&page_0), 10);
    assert_eq!(page_0["facets"]["category"]["even"].as_u64(), Some(13));
    assert_eq!(json_u64(&page_2, "nbHits"), 25);
    assert_eq!(json_u64(&page_2, "nbPages"), 3);
    assert_eq!(hit_len(&page_2), 5);
    assert_eq!(page_2["facets"]["category"]["odd"].as_u64(), Some(12));
}

#[tokio::test]
async fn search_pagination_known_answer_empty_query_browse_uses_total_hits() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp, None);
    let index_name = "pagination_browse";
    create_pagination_index(
        &state,
        index_name,
        flapjack::index::settings::IndexSettings {
            searchable_attributes: Some(vec!["title".to_string()]),
            ..Default::default()
        },
    )
    .await;
    let app = search_app(state);

    let page_0 = search_json(
        &app,
        index_name,
        json!({"query": "", "hitsPerPage": 10, "page": 0}),
    )
    .await;
    let page_2 = search_json(
        &app,
        index_name,
        json!({"query": "", "hitsPerPage": 10, "page": 2}),
    )
    .await;

    assert_eq!(json_u64(&page_0, "nbHits"), 25);
    assert_eq!(json_u64(&page_0, "nbPages"), 3);
    assert_eq!(hit_len(&page_0), 10);
    assert_eq!(json_u64(&page_2, "nbHits"), 25);
    assert_eq!(json_u64(&page_2, "nbPages"), 3);
    assert_eq!(hit_len(&page_2), 5);
}

#[tokio::test]
async fn search_pagination_known_answer_sorted_query_keeps_sorted_total_hits() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp, None);
    let index_name = "pagination_sorted";
    create_pagination_index(
        &state,
        index_name,
        flapjack::index::settings::IndexSettings {
            searchable_attributes: Some(vec!["title".to_string()]),
            ..Default::default()
        },
    )
    .await;
    let app = search_app(state);

    let page_0 = search_json(
        &app,
        index_name,
        json!({"query": "pagination", "hitsPerPage": 10, "page": 0, "sort": ["rank:asc"]}),
    )
    .await;
    let page_2 = search_json(
        &app,
        index_name,
        json!({"query": "pagination", "hitsPerPage": 10, "page": 2, "sort": ["rank:asc"]}),
    )
    .await;
    let page_0_ids = hit_ids(&page_0);
    let page_2_ids = hit_ids(&page_2);

    assert_eq!(json_u64(&page_0, "nbHits"), 25);
    assert_eq!(json_u64(&page_0, "nbPages"), 3);
    assert_eq!(hit_len(&page_0), 10);
    assert_eq!(page_0_ids[0], "id_zebra_900");
    assert_eq!(page_0_ids[9], "id_echo_050");
    assert_eq!(json_u64(&page_2, "nbHits"), 25);
    assert_eq!(json_u64(&page_2, "nbPages"), 3);
    assert_eq!(hit_len(&page_2), 5);
    assert_eq!(page_2_ids[0], "id_romeo_130");
    assert_eq!(page_2_ids[4], "id_november_170");
}
