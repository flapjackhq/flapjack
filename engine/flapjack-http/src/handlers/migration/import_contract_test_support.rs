use super::super::spool::{SpoolLimits, SpoolStore};
use crate::dto::SearchRequest;
use crate::extractors::ValidatedIndexName;
use crate::handlers::index_resource_store::save_resource_batch;
use crate::handlers::indices::list_indices;
use crate::handlers::objects::get_object;
use crate::handlers::rules::get_rule;
use crate::handlers::search::search_single;
use crate::handlers::settings::{get_settings, persist_index_settings};
use crate::handlers::synonyms::get_synonym;
use crate::test_helpers::body_json;
use axum::{
    extract::{Path as AxumPath, Query, State},
    response::IntoResponse,
    Json,
};
use flapjack::index::rules::{Consequence, Rule, RuleStore};
use flapjack::index::settings::IndexSettings;
use flapjack::index::synonyms::{Synonym, SynonymStore};
use flapjack::types::{Document, FieldValue};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

const PREEXISTING_DOCUMENT_ID: &str = "kept-1";
const PREEXISTING_DOCUMENT_TITLE: &str = "Cedar Caliper";
const PREEXISTING_DOCUMENT_CATEGORY: &str = "tools";
const PREEXISTING_SYNONYM_ID: &str = "kept-synonym";
const PREEXISTING_RULE_ID: &str = "kept-rule";

pub(super) async fn seed_preexisting_target_resources(
    state: &Arc<crate::handlers::AppState>,
    target_index: &str,
) {
    state.manager.create_tenant(target_index).unwrap();
    state
        .manager
        .add_documents_durable(
            target_index,
            vec![Document {
                id: PREEXISTING_DOCUMENT_ID.to_string(),
                fields: HashMap::from([
                    (
                        "title".to_string(),
                        FieldValue::Text(PREEXISTING_DOCUMENT_TITLE.to_string()),
                    ),
                    (
                        "category".to_string(),
                        FieldValue::Text(PREEXISTING_DOCUMENT_CATEGORY.to_string()),
                    ),
                ]),
            }],
        )
        .await
        .unwrap();

    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        attributes_for_faceting: vec!["category".to_string()],
        hits_per_page: 17,
        ..Default::default()
    };
    persist_index_settings(&state.manager, target_index, &settings)
        .unwrap_or_else(|_| panic!("preexisting settings should persist"));
    save_resource_batch::<SynonymStore, _>(
        &state.manager,
        target_index,
        [preexisting_synonym()],
        true,
    )
    .unwrap();
    save_resource_batch::<RuleStore, _>(&state.manager, target_index, [preexisting_rule()], true)
        .unwrap();
}

pub(super) async fn assert_preexisting_target_resources(
    state: &Arc<crate::handlers::AppState>,
    target_index: &str,
) {
    assert_query_returns_document(
        state,
        target_index,
        PREEXISTING_DOCUMENT_TITLE,
        PREEXISTING_DOCUMENT_ID,
        PREEXISTING_DOCUMENT_TITLE,
        PREEXISTING_DOCUMENT_CATEGORY,
    )
    .await;
    let settings = body_json(
        get_settings(
            State(Arc::clone(state)),
            ValidatedIndexName(target_index.to_string()),
        )
        .await
        .unwrap_or_else(|_| panic!("preexisting settings should remain served"))
        .into_response(),
    )
    .await;
    assert_eq!(settings["searchableAttributes"], json!(["title"]));
    assert_eq!(settings["attributesForFaceting"], json!(["category"]));
    assert_eq!(settings["hitsPerPage"], 17);

    let Json(synonym) = get_synonym(
        State(Arc::clone(state)),
        AxumPath((target_index.to_string(), PREEXISTING_SYNONYM_ID.to_string())),
    )
    .await
    .unwrap_or_else(|_| panic!("preexisting synonym should remain served"));
    assert_eq!(synonym, preexisting_synonym());
    let Json(rule) = get_rule(
        State(Arc::clone(state)),
        AxumPath((target_index.to_string(), PREEXISTING_RULE_ID.to_string())),
    )
    .await
    .unwrap_or_else(|_| panic!("preexisting rule should remain served"));
    assert_eq!(
        serde_json::to_value(rule).unwrap(),
        serde_json::to_value(preexisting_rule()).unwrap()
    );
}

fn preexisting_synonym() -> Synonym {
    Synonym::OneWay {
        object_id: PREEXISTING_SYNONYM_ID.to_string(),
        input: "caliper".to_string(),
        synonyms: vec!["gauge".to_string(), "measuring tool".to_string()],
    }
}

fn preexisting_rule() -> Rule {
    Rule {
        object_id: PREEXISTING_RULE_ID.to_string(),
        conditions: vec![],
        consequence: Consequence {
            promote: None,
            hide: None,
            filter_promotes: None,
            user_data: Some(json!({"preserved": "original-rule-canary"})),
            params: None,
        },
        description: Some("preexisting rule".to_string()),
        enabled: Some(true),
        validity: None,
    }
}

pub(super) async fn query_hit_count(
    state: &Arc<crate::handlers::AppState>,
    target_index: &str,
    query: &str,
) -> usize {
    let Json(search_response) = search_single(
        State(Arc::clone(state)),
        target_index.to_string(),
        SearchRequest {
            query: query.to_string(),
            hits_per_page: Some(10),
            ..Default::default()
        },
    )
    .await
    .expect("target should be queryable");
    search_response["hits"].as_array().unwrap().len()
}

pub(super) async fn assert_object_fields(
    state: &Arc<crate::handlers::AppState>,
    target_index: &str,
    object_id: &str,
    title: &str,
    page_marker: i64,
    score: i64,
) {
    let Json(object) = get_object(
        State(Arc::clone(state)),
        AxumPath((target_index.to_string(), object_id.to_string())),
    )
    .await
    .expect("activated document should be readable by objectID");
    assert_eq!(object["objectID"], object_id);
    assert_eq!(object["title"], title);
    assert_eq!(object["page_marker"], page_marker);
    assert_eq!(object["score"], score);
}

pub(super) async fn assert_query_returns_document(
    state: &Arc<crate::handlers::AppState>,
    target_index: &str,
    query: &str,
    object_id: &str,
    title: &str,
    category: &str,
) {
    let Json(search_response) = search_single(
        State(Arc::clone(state)),
        target_index.to_string(),
        SearchRequest {
            query: query.to_string(),
            hits_per_page: Some(10),
            ..Default::default()
        },
    )
    .await
    .expect("imported target should be queryable through search_single");
    let hits = search_response["hits"]
        .as_array()
        .expect("search response should include hits");
    assert_eq!(hits.len(), 1, "query {query:?} should match one document");
    assert_eq!(hits[0]["objectID"], object_id);
    assert_eq!(hits[0]["title"], title);
    assert_eq!(hits[0]["category"], category);
}

pub(super) fn directory_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut snapshot = BTreeMap::new();
    collect_directory_snapshot(root, root, &mut snapshot);
    snapshot
}

fn collect_directory_snapshot(
    root: &Path,
    current: &Path,
    snapshot: &mut BTreeMap<PathBuf, Vec<u8>>,
) {
    let mut entries = fs::read_dir(current)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_directory_snapshot(root, &path, snapshot);
        } else {
            snapshot.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                fs::read(path).unwrap(),
            );
        }
    }
}

pub(super) async fn assert_target_absent_from_disk_and_list(
    state: &Arc<crate::handlers::AppState>,
    target_index: &str,
) {
    assert!(
        !state.manager.base_path.join(target_index).exists(),
        "aborted migration must not create the target directory"
    );
    let axum::Json(indices) = list_indices(State(Arc::clone(state)), Query(HashMap::new()))
        .await
        .expect("index list should remain readable after aborted import");
    assert!(
        indices.items.iter().all(|item| item.name != target_index),
        "aborted migration target must not be listable"
    );
}

pub(super) fn assert_spool_lifecycle_with_artifacts(
    state: &Arc<crate::handlers::AppState>,
    lifecycle: &str,
) {
    let spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    let job_uuid = only_spool_job(&spool);
    let manifest: serde_json::Value =
        serde_json::from_str(&spool.manifest_json(job_uuid).unwrap()).unwrap();
    assert_eq!(manifest["lifecycle"], lifecycle);
    assert!(
        !spool.visible_artifacts(job_uuid).unwrap().is_empty(),
        "failed pre-activation import should retain spool evidence"
    );
}

pub(super) fn assert_no_retained_accepted_spool_document_artifacts(
    state: &Arc<crate::handlers::AppState>,
) {
    let spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    let job_uuid = only_spool_job(&spool);
    let manifest: serde_json::Value =
        serde_json::from_str(&spool.manifest_json(job_uuid).unwrap()).unwrap();
    assert_eq!(manifest["lifecycle"], "Deleted");
    assert!(
        spool.visible_artifacts(job_uuid).unwrap().is_empty(),
        "successful import should delete accepted spool document batches"
    );
}

fn only_spool_job(spool: &SpoolStore) -> uuid::Uuid {
    let jobs = spool.job_uuids().unwrap();
    assert_eq!(jobs.len(), 1, "expected one migration spool job");
    jobs[0]
}
