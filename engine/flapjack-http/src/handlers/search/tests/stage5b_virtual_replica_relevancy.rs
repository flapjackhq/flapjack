//! Stub summary for stage5b_virtual_replica_relevancy.rs.
use super::*;

fn make_priority_doc(id: &str, title: String, priority: i64) -> Document {
    let mut fields = HashMap::new();
    fields.insert("title".to_string(), FieldValue::Text(title));
    fields.insert("priority".to_string(), FieldValue::Integer(priority));
    Document {
        id: id.to_string(),
        fields,
    }
}

fn save_primary_with_replica(
    state: &Arc<AppState>,
    primary_index_name: &str,
    replica_entry: String,
) {
    state.manager.create_tenant(primary_index_name).unwrap();
    let primary_settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        replicas: Some(vec![replica_entry]),
        ..Default::default()
    };
    save_index_settings(state, primary_index_name, &primary_settings);
}

/// TODO: Document setup_virtual_replica_index.
async fn setup_virtual_replica_index(
    state: &Arc<AppState>,
    primary_index_name: &str,
    virtual_replica_name: &str,
    stored_strictness: Option<u32>,
    docs: Vec<Document>,
) {
    save_primary_with_replica(
        state,
        primary_index_name,
        format!("virtual({virtual_replica_name})"),
    );

    let virtual_settings = flapjack::index::settings::IndexSettings {
        primary: Some(primary_index_name.to_string()),
        searchable_attributes: Some(vec!["title".to_string()]),
        custom_ranking: Some(vec!["desc(priority)".to_string()]),
        relevancy_strictness: stored_strictness,
        ..Default::default()
    };
    save_index_settings(state, virtual_replica_name, &virtual_settings);

    state
        .manager
        .add_documents_sync(primary_index_name, docs)
        .await
        .unwrap();
}
/// TODO: Document virtual_replica_search_uses_replica_dictionary_entries.
#[tokio::test]
async fn virtual_replica_search_uses_replica_dictionary_entries() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let primary_index_name = "stage5b_primary_dictionary_routing";
    let virtual_replica_name = "stage5b_virtual_dictionary_routing";
    let docs = vec![
        make_priority_doc("doc-delta", "delta waves".to_string(), 1),
        make_priority_doc("doc-alpha", "alpha particles".to_string(), 1),
    ];
    setup_virtual_replica_index(&state, primary_index_name, virtual_replica_name, None, docs).await;

    let dictionary_manager = state.manager.dictionary_manager().unwrap();
    dictionary_manager
        .batch(
            virtual_replica_name,
            flapjack::dictionaries::DictionaryName::Stopwords,
            &flapjack::dictionaries::BatchDictionaryRequest {
                clear_existing_dictionary_entries: false,
                requests: vec![flapjack::dictionaries::BatchRequest {
                    action: flapjack::dictionaries::BatchAction::AddEntry,
                    body: serde_json::json!({
                        "objectID": "sw-alpha-virtual",
                        "language": "en",
                        "word": "alpha",
                        "state": "enabled",
                        "type": "custom"
                    }),
                }],
            },
        )
        .unwrap();
    dictionary_manager
        .batch(
            primary_index_name,
            flapjack::dictionaries::DictionaryName::Stopwords,
            &flapjack::dictionaries::BatchDictionaryRequest {
                clear_existing_dictionary_entries: false,
                requests: vec![flapjack::dictionaries::BatchRequest {
                    action: flapjack::dictionaries::BatchAction::AddEntry,
                    body: serde_json::json!({
                        "objectID": "sw-delta-primary",
                        "language": "en",
                        "word": "delta",
                        "state": "enabled",
                        "type": "custom"
                    }),
                }],
            },
        )
        .unwrap();

    let app = search_router(state);
    let resp = post_search(
        &app,
        virtual_replica_name,
        json!({
            "query": "alpha delta",
            "removeStopWords": true,
            "queryLanguages": ["en"],
            "queryType": "prefixNone",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let ids = hit_ids(&body);
    assert_eq!(
        ids,
        vec!["doc-delta".to_string()],
        "virtual replica searches must use replica-local dictionary entries rather than the primary index dictionary"
    );
}

/// TODO: Document setup_standard_replica_index.
async fn setup_standard_replica_index(
    state: &Arc<AppState>,
    primary_index_name: &str,
    standard_replica_name: &str,
    docs: Vec<Document>,
) {
    save_primary_with_replica(state, primary_index_name, standard_replica_name.to_string());

    state.manager.create_tenant(standard_replica_name).unwrap();
    let standard_settings = flapjack::index::settings::IndexSettings {
        primary: Some(primary_index_name.to_string()),
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(state, standard_replica_name, &standard_settings);

    state
        .manager
        .add_documents_sync(standard_replica_name, docs)
        .await
        .unwrap();
}
/// TODO: Document virtual_replica_search_uses_stored_strictness_when_query_omits_param.
#[tokio::test]
async fn virtual_replica_search_uses_stored_strictness_when_query_omits_param() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let primary_index_name = "stage5b_primary_stored_strictness";
    let virtual_replica_name = "stage5b_virtual_stored_strictness";
    let docs = vec![
        make_priority_doc("doc_best_text", "foo foo foo foo foo".to_string(), 1),
        make_priority_doc("doc_high_priority", "foo foo foo foo".to_string(), 100),
        make_priority_doc("doc_low_text", "foo".to_string(), 50),
    ];
    setup_virtual_replica_index(
        &state,
        primary_index_name,
        virtual_replica_name,
        Some(90),
        docs,
    )
    .await;

    let app = search_router(state);
    let resp = post_search(
        &app,
        virtual_replica_name,
        json!({
            "query": "foo",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let ids = hit_ids(&body);
    assert!(!ids.is_empty());
    assert_eq!(
        ids[0], "doc_high_priority",
        "stored strictness=90 should use custom-ranking-first blend for virtual replicas"
    );
}
/// TODO: Document query_relevancy_strictness_overrides_virtual_replica_stored_value.
#[tokio::test]
async fn query_relevancy_strictness_overrides_virtual_replica_stored_value() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let primary_index_name = "stage5b_primary_query_override";
    let virtual_replica_name = "stage5b_virtual_query_override";
    let docs = vec![
        make_priority_doc("doc_best_text", "foo ".repeat(10), 1),
        make_priority_doc("doc_medium_text_high_priority", "foo foo".to_string(), 100),
        make_priority_doc("doc_low_text", "foo".to_string(), 50),
    ];
    setup_virtual_replica_index(
        &state,
        primary_index_name,
        virtual_replica_name,
        Some(90),
        docs,
    )
    .await;

    let app = search_router(state);
    let resp_no_override = post_search(
        &app,
        virtual_replica_name,
        json!({
            "query": "foo",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_no_override.status(), StatusCode::OK);
    let body_no_override = body_json(resp_no_override).await;
    let ids_no_override = hit_ids(&body_no_override);
    assert!(!ids_no_override.is_empty());
    assert_eq!(
        ids_no_override[0], "doc_best_text",
        "stored strictness=90 should keep best-text document first in this fixture"
    );

    let resp_override = post_search(
        &app,
        virtual_replica_name,
        json!({
            "query": "foo",
            "relevancyStrictness": 50,
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_override.status(), StatusCode::OK);
    let body_override = body_json(resp_override).await;
    let ids_override = hit_ids(&body_override);
    assert!(!ids_override.is_empty());
    assert_eq!(
        ids_override[0], "doc_medium_text_high_priority",
        "query strictness=50 must override stored strictness and promote higher-priority document"
    );
    assert_ne!(
        ids_no_override, ids_override,
        "query strictness should change ordering relative to stored strictness"
    );
}
/// TODO: Document virtual_replica_without_stored_strictness_defaults_to_hundred.
#[tokio::test]
async fn virtual_replica_without_stored_strictness_defaults_to_hundred() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let primary_index_name = "stage5b_primary_default_strictness";
    let virtual_replica_name = "stage5b_virtual_default_strictness";
    let long_tail = "lorem ipsum dolor sit amet ".repeat(4000);
    let docs = vec![
        make_priority_doc("doc_best_text", "foo".to_string(), 1),
        make_priority_doc("doc_mid", "foo bar".to_string(), 50),
        make_priority_doc("doc_best_prio", format!("foo {long_tail}"), 100),
    ];
    setup_virtual_replica_index(&state, primary_index_name, virtual_replica_name, None, docs).await;

    let app = search_router(state);
    let resp = post_search(
        &app,
        virtual_replica_name,
        json!({
            "query": "foo",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let ids = hit_ids(&body);
    assert!(!ids.is_empty());
    assert_eq!(
        ids[0], "doc_best_text",
        "without stored strictness, virtual replicas should default to strictness=100"
    );
}
/// TODO: Document virtual_replica_ctr_reranking_uses_stored_strictness_when_query_omits_param.
#[tokio::test]
async fn virtual_replica_ctr_reranking_uses_stored_strictness_when_query_omits_param() {
    let tmp = TempDir::new().unwrap();
    let analytics_cfg = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(analytics_cfg.clone());
    let analytics_engine = Arc::new(AnalyticsQueryEngine::new(analytics_cfg));
    let state = make_basic_search_state_with_analytics(&tmp, Some(analytics_engine));
    let primary_index_name = "stage5b_primary_ctr_stored_strictness";
    let virtual_replica_name = "stage5b_virtual_ctr_stored_strictness";
    let docs = vec![
        make_priority_doc("doc_best_text", "foo foo foo foo foo".to_string(), 1),
        make_priority_doc("doc_clicked", "foo foo foo foo".to_string(), 100),
    ];
    setup_virtual_replica_index(
        &state,
        primary_index_name,
        virtual_replica_name,
        Some(100),
        docs,
    )
    .await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    record_click_events(
        &collector,
        "user-rerank",
        virtual_replica_name,
        "doc_clicked",
        6,
        now_ms,
    );
    collector.flush_all();

    let app = search_router(state);
    let resp = post_search(
        &app,
        virtual_replica_name,
        json!({
            "query": "foo",
            "enableReRanking": true,
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let ids = hit_ids(&body);
    assert!(!ids.is_empty());
    assert_eq!(
        ids[0], "doc_best_text",
        "stored strictness=100 should keep text-first ranking during CTR reranking for virtual replicas"
    );
}
/// TODO: Document virtual_replica_ctr_reranking_without_stored_strictness_defaults_to_hundred.
#[tokio::test]
async fn virtual_replica_ctr_reranking_without_stored_strictness_defaults_to_hundred() {
    let tmp = TempDir::new().unwrap();
    let analytics_cfg = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(analytics_cfg.clone());
    let analytics_engine = Arc::new(AnalyticsQueryEngine::new(analytics_cfg));
    let state = make_basic_search_state_with_analytics(&tmp, Some(analytics_engine));
    let primary_index_name = "stage5b_primary_ctr_default_strictness";
    let virtual_replica_name = "stage5b_virtual_ctr_default_strictness";
    let docs = vec![
        make_priority_doc("doc_best_text", "foo foo foo foo foo".to_string(), 1),
        make_priority_doc("doc_clicked", "foo foo foo foo".to_string(), 100),
    ];
    setup_virtual_replica_index(&state, primary_index_name, virtual_replica_name, None, docs).await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    record_click_events(
        &collector,
        "user-rerank-default",
        virtual_replica_name,
        "doc_clicked",
        6,
        now_ms,
    );
    collector.flush_all();

    let app = search_router(state);
    let resp = post_search(
        &app,
        virtual_replica_name,
        json!({
            "query": "foo",
            "enableReRanking": true,
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let ids = hit_ids(&body);
    assert!(!ids.is_empty());
    assert_eq!(
        ids[0], "doc_best_text",
        "without stored strictness, virtual replicas should still default to strictness=100 during CTR reranking"
    );
}
/// TODO: Document standard_replica_ctr_reranking_without_query_strictness_keeps_default_fifty.
#[tokio::test]
async fn standard_replica_ctr_reranking_without_query_strictness_keeps_default_fifty() {
    let tmp = TempDir::new().unwrap();
    let analytics_cfg = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(analytics_cfg.clone());
    let analytics_engine = Arc::new(AnalyticsQueryEngine::new(analytics_cfg));
    let state = make_basic_search_state_with_analytics(&tmp, Some(analytics_engine));
    let primary_index_name = "stage5b_primary_standard_ctr_default";
    let standard_replica_name = "stage5b_standard_ctr_default";
    let docs = vec![
        make_priority_doc("doc_best_text", "foo foo foo foo foo".to_string(), 1),
        make_priority_doc("doc_clicked", "foo foo foo foo".to_string(), 100),
    ];
    setup_standard_replica_index(&state, primary_index_name, standard_replica_name, docs).await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    record_click_events(
        &collector,
        "user-standard-rerank-default",
        standard_replica_name,
        "doc_clicked",
        6,
        now_ms,
    );
    collector.flush_all();

    let app = search_router(state);
    let resp = post_search(
        &app,
        standard_replica_name,
        json!({
            "query": "foo",
            "enableReRanking": true,
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let ids = hit_ids(&body);
    assert!(!ids.is_empty());
    assert_eq!(
        ids[0], "doc_clicked",
        "standard replicas should keep CTR reranking's default 50/50 blend when no query strictness is provided"
    );
}
