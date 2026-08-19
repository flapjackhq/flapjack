//! Integration tests validating search request parameter parsing, feature interaction, and ranking behavior for advanced search options including syntax features, filtering, faceting, highlighting, synonyms, and exact matching modes.
use super::*;

/// Create an index with searchable "title" attribute and add documents using the provided titles as content.
///
/// # Arguments
/// * `state` - AppState for accessing the search manager
/// * `index_name` - Name of the index to create
/// * `titles` - Array of title strings to use as document content
async fn create_search_index_with_titles(state: &Arc<AppState>, index_name: &str, titles: &[&str]) {
    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(state, index_name, &settings);

    let docs: Vec<Document> = titles
        .iter()
        .enumerate()
        .map(|(i, title)| {
            let mut fields = HashMap::new();
            fields.insert("title".to_string(), FieldValue::Text((*title).to_string()));
            Document {
                id: format!("doc_{i}"),
                fields,
            }
        })
        .collect();

    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();
}

/// Return HTTP 400 when advancedSyntaxFeatures contains an unknown feature name, with an error message referencing advancedSyntaxFeatures.
#[tokio::test]
async fn search_rejects_unknown_advanced_syntax_features_value() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_invalid_adv_features_idx";
    create_search_index_with_titles(&state, index_name, &["blue wireless earbuds"]).await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "blue",
            "advancedSyntax": true,
            "advancedSyntaxFeatures": ["notARealFeature"]
        }),
        None,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    let msg = body["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("advancedSyntaxFeatures"),
        "expected advancedSyntaxFeatures validation error, got: {msg}"
    );
}

/// Return HTTP 400 when minProximity is 0 or 100, with an error message referencing minProximity.
#[tokio::test]
async fn search_rejects_min_proximity_out_of_range_values() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_invalid_min_proximity_idx";
    create_search_index_with_titles(&state, index_name, &["blue wireless earbuds"]).await;
    let app = search_router(state);

    for invalid in [0_u64, 100_u64] {
        let resp = post_search(
            &app,
            index_name,
            json!({
                "query": "blue",
                "minProximity": invalid
            }),
            None,
        )
        .await;

        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "minProximity={invalid} must return 400"
        );
        let body = body_json(resp).await;
        let msg = body["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("minProximity"),
            "expected minProximity validation error, got: {msg}"
        );
    }
}

/// Return HTTP 400 when exactOnSingleWordQuery receives an invalid value, with an error message referencing the invalid parameter.
#[tokio::test]
async fn search_rejects_invalid_exact_on_single_word_query() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_invalid_exact_single_word_idx";
    create_search_index_with_titles(&state, index_name, &["blue wireless earbuds"]).await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "blue",
            "exactOnSingleWordQuery": "invalid"
        }),
        None,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    let msg = body["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("exactOnSingleWordQuery"),
        "expected exactOnSingleWordQuery validation error, got: {msg}"
    );
}

/// Return HTTP 400 when minProximity receives a string instead of an integer.
#[tokio::test]
async fn search_rejects_wrong_type_for_min_proximity() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_wrong_type_min_proximity_idx";
    create_search_index_with_titles(&state, index_name, &["blue wireless earbuds"]).await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "blue",
            "minProximity": "2"
        }),
        None,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// When advancedSyntaxFeatures contains only "exactPhrase", exclude the excludeWords feature and return docs that would be excluded by "-word" syntax.
#[tokio::test]
async fn search_advanced_syntax_features_exact_phrase_only_disables_exclude_words() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_adv_features_exact_phrase_only_idx";
    create_search_index_with_titles(
        &state,
        index_name,
        &[
            "blue wireless earbuds",
            "blue wireless speaker",
            "wireless blue earbuds",
        ],
    )
    .await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "\"blue wireless\" -earbuds",
            "advancedSyntax": true,
            "advancedSyntaxFeatures": ["exactPhrase"],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let ids = hit_ids(&body);
    assert!(
        ids.iter().any(|id| id == "doc_0"),
        "doc_0 must be present because excludeWords is disabled"
    );

    let resp_with_exclusion = post_search(
        &app,
        index_name,
        json!({
            "query": "\"blue wireless\" -earbuds",
            "advancedSyntax": true,
            "advancedSyntaxFeatures": ["exactPhrase", "excludeWords"],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;

    assert_eq!(resp_with_exclusion.status(), StatusCode::OK);
    let body_with_exclusion = body_json(resp_with_exclusion).await;
    let ids_with_exclusion = hit_ids(&body_with_exclusion);
    assert!(
        !ids_with_exclusion.iter().any(|id| id == "doc_0"),
        "doc_0 must be excluded once excludeWords is enabled"
    );
}

/// Verify that when advancedSyntaxFeatures contains only "excludeWords", the exactPhrase feature remains disabled and quoted strings are treated as literal words.
#[tokio::test]
async fn search_advanced_syntax_features_exclude_words_only_disables_exact_phrase() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_adv_features_exclude_only_idx";
    create_search_index_with_titles(
        &state,
        index_name,
        &[
            "blue wireless earbuds",
            "blue wireless speaker",
            "wireless blue earbuds",
            "wireless blue speaker",
        ],
    )
    .await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "\"blue wireless\" -earbuds",
            "advancedSyntax": true,
            "advancedSyntaxFeatures": ["excludeWords"],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let ids = hit_ids(&body);

    assert!(
        ids.iter().any(|id| id == "doc_1"),
        "doc_1 must match (blue wireless speaker)"
    );
    assert!(
        ids.iter().any(|id| id == "doc_3"),
        "doc_3 must match because exactPhrase is disabled and quotes are treated as regular words"
    );
    assert!(
        !ids.iter().any(|id| id == "doc_0"),
        "doc_0 must be excluded by -earbuds"
    );
    assert!(
        !ids.iter().any(|id| id == "doc_2"),
        "doc_2 must be excluded by -earbuds"
    );
}

// ── facetingAfterDistinct ──

/// When similarQuery is provided, replace query semantics with OR/Should logic, overriding any AND/MUST semantics from the query parameter.
#[tokio::test]
async fn search_similar_query_overrides_query_param() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_similar_query_overrides_query_param_idx";
    create_search_index_with_titles(&state, index_name, &["blue", "red", "running"]).await;
    let app = search_router(state);

    let baseline_resp = post_search(
        &app,
        index_name,
        json!({
            "query": "blue red",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(baseline_resp.status(), StatusCode::OK);
    let baseline_body = body_json(baseline_resp).await;
    assert_eq!(
        baseline_body["nbHits"].as_u64().unwrap_or(0),
        0,
        "baseline query (blue red) should return no hits with AND/MUST logic"
    );

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "blue red",
            "similarQuery": "blue red",
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
        vec!["doc_0".to_string(), "doc_1".to_string()],
        "similarQuery should override query and use OR/Should logic"
    );
}

/// Apply OR matching logic to similarQuery, returning docs that match any word in the query.
#[tokio::test]
async fn search_similar_query_uses_or_logic() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_similar_query_uses_or_logic_idx";
    create_search_index_with_titles(
        &state,
        index_name,
        &[
            "red",
            "running",
            "shoes",
            "lightweight",
            "red running shoes lightweight",
        ],
    )
    .await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "blue running",
            "similarQuery": "red running shoes lightweight",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let mut ids = hit_ids(&body);
    let expected: Vec<String> = vec![
        "doc_0".into(),
        "doc_1".into(),
        "doc_2".into(),
        "doc_3".into(),
        "doc_4".into(),
    ];
    let mut expected = expected;
    ids.sort();
    expected.sort();
    assert_eq!(
        ids, expected,
        "OR logic should include docs matching any query word"
    );
}

/// Filter out stop words ("the", "and", "in") from similarQuery before applying OR matching logic.
#[tokio::test]
async fn search_similar_query_removes_stop_words() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_similar_query_removes_stop_words_idx";
    create_search_index_with_titles(
        &state,
        index_name,
        &["running", "the", "the in", "red running"],
    )
    .await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "blue",
            "similarQuery": "the and running",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let mut ids = hit_ids(&body);
    let mut expected = vec!["doc_0".to_string(), "doc_3".to_string()];
    ids.sort();
    expected.sort();
    assert_eq!(ids, expected);
}

/// Confirm that similarQuery disables prefix matching and only matches exact words, returning only docs with exact matches.
#[tokio::test]
async fn search_similar_query_disables_prefix_matching() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_similar_query_disables_prefix_matching_idx";
    create_search_index_with_titles(&state, index_name, &["run", "running", "runner"]).await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "blue",
            "similarQuery": "run",
            "typoTolerance": false,
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let ids = hit_ids(&body);
    assert_eq!(ids, vec!["doc_0".to_string()]);
}

/// Rank docs with more matching words from similarQuery higher, with exact-match count as the primary ranking signal.
#[tokio::test]
async fn search_similar_query_words_ranking_first() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_similar_query_words_ranking_first_idx";
    create_search_index_with_titles(
        &state,
        index_name,
        &[
            "red",
            "red running",
            "red running shoes",
            "red running shoes lightweight",
        ],
    )
    .await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "anything",
            "similarQuery": "red running shoes lightweight",
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
        vec![
            "doc_3".to_string(),
            "doc_2".to_string(),
            "doc_1".to_string(),
            "doc_0".to_string()
        ]
    );
}

/// When facetingAfterDistinct is true, recompute facet counts based on the deduplicated result set; when false, count facets from the pre-distinct result set.
#[tokio::test]
async fn search_faceting_after_distinct_recomputes_facet_counts() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_faceting_after_distinct_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        attributes_for_faceting: vec!["category".to_string()],
        attribute_for_distinct: Some("group".to_string()),
        distinct: Some(flapjack::index::settings::DistinctValue::Integer(1)),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    // 3 docs in group "A" with category "electronics", 1 doc in group "B" with "books"
    let docs: Vec<Document> = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("laptop one".into()));
            f.insert(
                "category".to_string(),
                FieldValue::Text("electronics".into()),
            );
            f.insert("group".to_string(), FieldValue::Text("A".into()));
            Document {
                id: "d1".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("laptop two".into()));
            f.insert(
                "category".to_string(),
                FieldValue::Text("electronics".into()),
            );
            f.insert("group".to_string(), FieldValue::Text("A".into()));
            Document {
                id: "d2".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("laptop three".into()));
            f.insert(
                "category".to_string(),
                FieldValue::Text("electronics".into()),
            );
            f.insert("group".to_string(), FieldValue::Text("A".into()));
            Document {
                id: "d3".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("laptop book".into()));
            f.insert("category".to_string(), FieldValue::Text("books".into()));
            f.insert("group".to_string(), FieldValue::Text("B".into()));
            Document {
                id: "d4".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    // Without facetingAfterDistinct: electronics=3, books=1 (pre-distinct counts)
    let resp_without = post_search(
        &app,
        index_name,
        json!({
            "query": "laptop",
            "facets": ["category"],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_without.status(), StatusCode::OK);
    let body_without = body_json(resp_without).await;
    let electronics_count_without = body_without["facets"]["category"]["electronics"]
        .as_u64()
        .unwrap_or(0);
    // Pre-distinct should have 3 electronics (all 3 docs counted)
    assert_eq!(
        electronics_count_without, 3,
        "Without facetingAfterDistinct, electronics should count all docs"
    );

    // With facetingAfterDistinct=true: electronics=1, books=1 (post-distinct counts)
    let resp_with = post_search(
        &app,
        index_name,
        json!({
            "query": "laptop",
            "facets": ["category"],
            "facetingAfterDistinct": true,
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_with.status(), StatusCode::OK);
    let body_with = body_json(resp_with).await;
    let electronics_count_with = body_with["facets"]["category"]["electronics"]
        .as_u64()
        .unwrap_or(0);
    // Post-distinct: only 1 doc from group A survives, so electronics=1
    assert_eq!(
        electronics_count_with, 1,
        "With facetingAfterDistinct, electronics should count only deduplicated docs"
    );
}

// ── restrictHighlightAndSnippetArrays ──

/// When restrictHighlightAndSnippetArrays is true, include only matched array elements in highlight results; when false, include all array elements.
#[tokio::test]
async fn search_restrict_highlight_arrays_limits_to_matched_elements() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_restrict_hl_arrays_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["tags".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    // Doc with array field: only "laptop" tag should match query "laptop"
    let mut fields = HashMap::new();
    fields.insert(
        "tags".to_string(),
        FieldValue::Array(vec![
            FieldValue::Text("laptop".into()),
            FieldValue::Text("desktop".into()),
            FieldValue::Text("tablet".into()),
        ]),
    );
    let doc = Document {
        id: "arr_doc".into(),
        fields,
    };
    state
        .manager
        .add_documents_sync(index_name, vec![doc])
        .await
        .unwrap();

    let app = search_router(state);

    // Without restriction: all array elements in highlight
    let resp_unrestricted = post_search(
        &app,
        index_name,
        json!({
            "query": "laptop",
            "restrictHighlightAndSnippetArrays": false,
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_unrestricted.status(), StatusCode::OK);
    let body_unrestricted = body_json(resp_unrestricted).await;
    let hl_tags_unrestricted = &body_unrestricted["hits"][0]["_highlightResult"]["tags"];
    let unrestricted_count = hl_tags_unrestricted
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(
        unrestricted_count, 3,
        "Without restriction, all 3 array elements should be in highlight"
    );

    // With restriction: only matched array elements
    let resp_restricted = post_search(
        &app,
        index_name,
        json!({
            "query": "laptop",
            "restrictHighlightAndSnippetArrays": true,
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_restricted.status(), StatusCode::OK);
    let body_restricted = body_json(resp_restricted).await;
    let hl_tags_restricted = &body_restricted["hits"][0]["_highlightResult"]["tags"];
    let restricted_count = hl_tags_restricted.as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(
        restricted_count, 1,
        "With restriction, only matched array elements should appear in highlight"
    );
}

// ── replaceSynonymsInHighlight ──

/// When replaceSynonymsInHighlight is true, include the expanded synonym word in matchedWords; when false, map back to the original query term.
#[tokio::test]
async fn search_replace_synonyms_in_highlight_shows_synonym_text() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_replace_syn_highlight_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    // Add a synonym: notebook <-> laptop
    let mut synonym_store = flapjack::index::synonyms::SynonymStore::new();
    synonym_store.insert(flapjack::index::synonyms::Synonym::Regular {
        object_id: "syn1".to_string(),
        synonyms: vec!["notebook".to_string(), "laptop".to_string()],
    });
    let syn_dir = state.manager.base_path.join(index_name);
    std::fs::create_dir_all(&syn_dir).ok();
    synonym_store.save(syn_dir.join("synonyms.json")).unwrap();
    state.manager.invalidate_synonyms_cache(index_name);

    let docs = vec![Document {
        id: "syn_doc".into(),
        fields: {
            let mut f = HashMap::new();
            f.insert(
                "title".to_string(),
                FieldValue::Text("best laptop for work".into()),
            );
            f
        },
    }];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    // replaceSynonymsInHighlight=true: matchedWords should include "laptop"
    // (the actual document word that was matched via synonym expansion, NOT mapped back)
    let resp_replace = post_search(
        &app,
        index_name,
        json!({
            "query": "notebook",
            "replaceSynonymsInHighlight": true,
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_replace.status(), StatusCode::OK);
    let body_replace = body_json(resp_replace).await;
    let hl_title = &body_replace["hits"][0]["_highlightResult"]["title"];
    let matched_words_replace: Vec<String> = hl_title["matchedWords"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    // When replaceSynonymsInHighlight=true, the expanded synonym word "laptop" should appear
    // because synonym mapping back to originals is skipped
    assert!(
        matched_words_replace.iter().any(|w| w == "laptop"),
        "replaceSynonymsInHighlight=true should keep expanded synonym 'laptop' in matchedWords, got: {:?}",
        matched_words_replace
    );

    // replaceSynonymsInHighlight=false (default): matchedWords mapped back to original query terms
    let resp_no_replace = post_search(
        &app,
        index_name,
        json!({
            "query": "notebook",
            "replaceSynonymsInHighlight": false,
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_no_replace.status(), StatusCode::OK);
    let body_no_replace = body_json(resp_no_replace).await;
    let hl_title_no = &body_no_replace["hits"][0]["_highlightResult"]["title"];
    let matched_words_no: Vec<String> = hl_title_no["matchedWords"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    assert!(
        matched_words_no.iter().any(|w| w == "notebook"),
        "replaceSynonymsInHighlight=false should map synonyms back to original query term 'notebook', got: {:?}",
        matched_words_no
    );
    // And "laptop" should NOT be in matchedWords when mapping back to originals
    assert!(
        !matched_words_no.iter().any(|w| w == "laptop"),
        "replaceSynonymsInHighlight=false should NOT contain expanded synonym 'laptop', got: {:?}",
        matched_words_no
    );
}

// ── snippetEllipsisText integration test ──

/// Replace the default ellipsis character with snippetEllipsisText; when empty, omit ellipsis markers entirely.
#[tokio::test]
async fn search_snippet_ellipsis_text_customizes_ellipsis() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_snippet_ellipsis_idx";

    create_search_index_with_titles(
        &state,
        index_name,
        &["the quick brown fox jumps over the lazy dog with many more words following here in this document"],
    )
    .await;
    let app = search_router(state);

    // With empty snippetEllipsisText: should have no ellipsis markers
    let resp_empty = post_search(
        &app,
        index_name,
        json!({
            "query": "fox",
            "attributesToSnippet": ["title:3"],
            "snippetEllipsisText": "",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_empty.status(), StatusCode::OK);
    let body_empty = body_json(resp_empty).await;
    let snippet_value = body_empty["hits"][0]["_snippetResult"]["title"]["value"]
        .as_str()
        .unwrap_or("");
    assert!(
        !snippet_value.contains('\u{2026}'),
        "Empty snippetEllipsisText should remove default ellipsis, got: {snippet_value}"
    );

    // With custom snippetEllipsisText
    let resp_custom = post_search(
        &app,
        index_name,
        json!({
            "query": "fox",
            "attributesToSnippet": ["title:3"],
            "snippetEllipsisText": " [...]",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_custom.status(), StatusCode::OK);
    let body_custom = body_json(resp_custom).await;
    let snippet_custom = body_custom["hits"][0]["_snippetResult"]["title"]["value"]
        .as_str()
        .unwrap_or("");
    assert!(
        snippet_custom.contains("[...]"),
        "Custom snippetEllipsisText should appear in snippet, got: {snippet_custom}"
    );
}

/// Omitted request fields inherit the selected saved highlighting and snippet settings.
#[tokio::test]
async fn search_uses_saved_highlight_and_snippet_defaults() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "saved_highlight_defaults_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "description".to_string()]),
        attributes_to_highlight: Some(vec!["description".to_string()]),
        attributes_to_snippet: Some(vec!["description:3".to_string()]),
        highlight_pre_tag: Some("<mark>".to_string()),
        highlight_post_tag: Some("</mark>".to_string()),
        snippet_ellipsis_text: Some(" [...]".to_string()),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);
    state
        .manager
        .add_documents_sync(
            index_name,
            vec![Document {
                id: "pbv2-trail-001".to_string(),
                fields: HashMap::from([
                    (
                        "title".to_string(),
                        FieldValue::Text("Bluebird Ridge Jacket".to_string()),
                    ),
                    (
                        "description".to_string(),
                        FieldValue::Text(
                            "Weatherproof alpine shell for bluebird ridge hikes".to_string(),
                        ),
                    ),
                ]),
            }],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let response = post_search(&app, index_name, json!({"query": "bluebird"}), None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let hit = &body["hits"][0];

    assert!(hit["_highlightResult"].get("title").is_none());
    assert_eq!(
        hit["_highlightResult"]["description"]["value"],
        "Weatherproof alpine shell for <mark>bluebird</mark> ridge hikes"
    );
    assert!(
        hit["_snippetResult"]["description"]["value"]
            .as_str()
            .is_some_and(|value| value.contains("<mark>bluebird</mark>") && value.contains("[...]")),
        "saved snippet attributes, tags, and ellipsis must affect the response"
    );
}

/// An explicit request remains authoritative over saved highlighting defaults.
#[tokio::test]
async fn search_request_overrides_saved_highlight_defaults() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "saved_highlight_override_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "description".to_string()]),
        attributes_to_highlight: Some(vec!["description".to_string()]),
        highlight_pre_tag: Some("<saved>".to_string()),
        highlight_post_tag: Some("</saved>".to_string()),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);
    state
        .manager
        .add_documents_sync(
            index_name,
            vec![Document {
                id: "1".to_string(),
                fields: HashMap::from([
                    (
                        "title".to_string(),
                        FieldValue::Text("Bluebird Ridge Jacket".to_string()),
                    ),
                    (
                        "description".to_string(),
                        FieldValue::Text("Bluebird shell".to_string()),
                    ),
                ]),
            }],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let response = post_search(
        &app,
        index_name,
        json!({
            "query": "bluebird",
            "attributesToHighlight": ["title"],
            "highlightPreTag": "<request>",
            "highlightPostTag": "</request>"
        }),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let highlight = &body["hits"][0]["_highlightResult"];

    assert_eq!(
        highlight["title"]["value"],
        "<request>Bluebird</request> Ridge Jacket"
    );
    assert!(highlight.get("description").is_none());
}

// ── sortFacetValuesBy integration test ──

/// Order facet values alphabetically when sortFacetValuesBy is "alpha", and by descending frequency when "count".
#[tokio::test]
async fn search_sort_facet_values_by_alpha_vs_count() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_sort_facet_values_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        attributes_for_faceting: vec!["color".to_string()],
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    // Create docs with varied color frequencies: red=3, blue=2, green=1
    let docs: Vec<Document> = vec![
        ("d1", "shirt red", "red"),
        ("d2", "shirt red", "red"),
        ("d3", "shirt red", "red"),
        ("d4", "shirt blue", "blue"),
        ("d5", "shirt blue", "blue"),
        ("d6", "shirt green", "green"),
    ]
    .into_iter()
    .map(|(id, title, color)| {
        let mut f = HashMap::new();
        f.insert("title".to_string(), FieldValue::Text(title.into()));
        f.insert("color".to_string(), FieldValue::Text(color.into()));
        Document {
            id: id.into(),
            fields: f,
        }
    })
    .collect();
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    // sortFacetValuesBy: "count" — red(3), blue(2), green(1)
    let resp_count = post_search(
        &app,
        index_name,
        json!({
            "query": "shirt",
            "facets": ["color"],
            "sortFacetValuesBy": "count",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_count.status(), StatusCode::OK);
    let body_count = body_json(resp_count).await;
    let facet_keys_count: Vec<String> = body_count["facets"]["color"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    // In count mode, first key should be the most frequent
    assert!(
        !facet_keys_count.is_empty(),
        "Expected facet values for color"
    );
    assert_eq!(
        facet_keys_count[0], "red",
        "Count sort: first value should be 'red' (most frequent), got: {:?}",
        facet_keys_count
    );

    // sortFacetValuesBy: "alpha" — blue, green, red (alphabetical)
    let resp_alpha = post_search(
        &app,
        index_name,
        json!({
            "query": "shirt",
            "facets": ["color"],
            "sortFacetValuesBy": "alpha",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_alpha.status(), StatusCode::OK);
    let body_alpha = body_json(resp_alpha).await;
    let facet_keys_alpha: Vec<String> = body_alpha["facets"]["color"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    assert_eq!(
        facet_keys_alpha,
        vec!["blue", "green", "red"],
        "Alpha sort: facet values should be alphabetically ordered"
    );
}

// ── percentileComputation acceptance test ──

/// Accept both true and false values for percentileComputation parameter without raising validation errors.
#[tokio::test]
async fn search_percentile_computation_accepted_without_error() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_percentile_comp_idx";
    create_search_index_with_titles(&state, index_name, &["hello world"]).await;
    let app = search_router(state);

    for val in [true, false] {
        let resp = post_search(
            &app,
            index_name,
            json!({
                "query": "hello",
                "percentileComputation": val,
                "hitsPerPage": 10
            }),
            None,
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "percentileComputation={val} should be accepted without error"
        );
    }
}

// ── sumOrFiltersScores ──

/// When sumOrFiltersScores is true, sum scores from all matching filters in an OR group; when false, use only the maximum score.
#[tokio::test]
async fn search_sum_or_filters_scores_changes_or_group_scoring() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_sum_or_filters_scores_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        attributes_for_faceting: vec!["brand".to_string(), "color".to_string()],
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    // d1: matches brand:Apple(score=2) AND color:Red(score=2) in an OR group
    //   max = 2, sum = 4
    // d2: matches color:Green(score=3) in a separate AND-level filter
    //   score = 3
    // With default (max): d2(3) > d1(2) → d2 ranks first
    // With sumOrFiltersScores=true: d1(4) > d2(3) → d1 ranks first
    let docs = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("widget".into()));
            f.insert("brand".to_string(), FieldValue::Text("Apple".into()));
            f.insert("color".to_string(), FieldValue::Text("Red".into()));
            Document {
                id: "d1".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("widget".into()));
            f.insert("brand".to_string(), FieldValue::Text("Samsung".into()));
            f.insert("color".to_string(), FieldValue::Text("Green".into()));
            Document {
                id: "d2".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    // Default (sumOrFiltersScores=false): OR group uses max
    // d1 matches both in OR group → max(2,2) = 2
    // d2 matches color:Green → score 3
    // d2 should rank first
    let resp_default = post_search(
        &app,
        index_name,
        json!({
            "query": "widget",
            "optionalFilters": [
                ["brand:Apple<score=2>", "color:Red<score=2>"],
                "color:Green<score=3>"
            ],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_default.status(), StatusCode::OK);
    let body_default = body_json(resp_default).await;
    let hits_default = body_default["hits"].as_array().unwrap();
    assert_eq!(hits_default.len(), 2);
    assert_eq!(
        hits_default[0]["objectID"].as_str().unwrap(),
        "d2",
        "Default (max): d2 with single score=3 should beat d1 with max(2,2)=2"
    );

    // With sumOrFiltersScores=true: d1 gets 2+2=4, d2 gets 3 → d1 first
    let resp_sum = post_search(
        &app,
        index_name,
        json!({
            "query": "widget",
            "optionalFilters": [
                ["brand:Apple<score=2>", "color:Red<score=2>"],
                "color:Green<score=3>"
            ],
            "sumOrFiltersScores": true,
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_sum.status(), StatusCode::OK);
    let body_sum = body_json(resp_sum).await;
    let hits_sum = body_sum["hits"].as_array().unwrap();
    assert_eq!(hits_sum.len(), 2);
    assert_eq!(
        hits_sum[0]["objectID"].as_str().unwrap(),
        "d1",
        "Sum: d1 with 2+2=4 should beat d2 with 3"
    );
}

/// Apply negative scores from optional filters with the "-" prefix, demoting docs that match the filter while keeping them in results.
#[tokio::test]
async fn search_negative_optional_filter_demotes_matching_docs() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_negative_optional_filter_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let docs = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("widget".into()));
            f.insert("brand".to_string(), FieldValue::Text("Nokia".into()));
            Document {
                id: "doc_nokia".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("widget".into()));
            f.insert("brand".to_string(), FieldValue::Text("Apple".into()));
            Document {
                id: "doc_apple".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "widget",
            "optionalFilters": ["-brand:Nokia<score=3>"],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0]["objectID"].as_str().unwrap(),
        "doc_apple",
        "negative optional filter must demote matching Nokia doc"
    );
}

/// Strip leading and trailing whitespace from field names, values, and score expressions in optional filters before parsing.
#[tokio::test]
async fn search_optional_filter_whitespace_is_trimmed() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_optional_filter_whitespace_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "priority".to_string()]),
        custom_ranking: Some(vec!["desc(priority)".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let docs = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("widget".into()));
            f.insert("brand".to_string(), FieldValue::Text("Nokia".into()));
            f.insert("priority".to_string(), FieldValue::Integer(100));
            Document {
                id: "doc_nokia".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("widget".into()));
            f.insert("brand".to_string(), FieldValue::Text("Apple".into()));
            f.insert("priority".to_string(), FieldValue::Integer(0));
            Document {
                id: "doc_apple".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "widget",
            "optionalFilters": [" - brand : Nokia <score=3> "],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0]["objectID"].as_str().unwrap(),
        "doc_apple",
        "optional filter parser must trim whitespace around field/value so demotion is applied"
    );
}

/// Trim whitespace inside score delimiters when parsing optional filter scores, enabling demotion scores to apply correctly.
#[tokio::test]
async fn search_optional_filter_whitespace_padded_score_is_parsed() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_optional_filter_whitespace_score_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let docs = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("widget".into()));
            f.insert("brand".to_string(), FieldValue::Text("Nokia".into()));
            Document {
                id: "aaa_nokia".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("widget".into()));
            f.insert("brand".to_string(), FieldValue::Text("Apple".into()));
            Document {
                id: "zzz_apple".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "widget",
            "optionalFilters": ["brand:Nokia<score=2>", "-brand:Nokia<score= 3 >"],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0]["objectID"].as_str().unwrap(),
        "zzz_apple",
        "score parser must trim whitespace inside <score=...>; without score parsing, tie-break/doc_id keeps aaa_nokia first"
    );
}
/// Default to exactOnSingleWordQuery="attribute" behavior: single-token exact matches rank higher than multi-token prefix matches.
#[tokio::test]
async fn search_exact_on_single_word_attribute_mode_default_is_attribute_semantics() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_exact_single_word_attribute_mode_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "priority".to_string()]),
        custom_ranking: Some(vec!["desc(priority)".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let docs = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Red".into()));
            f.insert("priority".to_string(), FieldValue::Integer(0));
            Document {
                id: "exact_attr_first".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Red Shoes".into()));
            f.insert("priority".to_string(), FieldValue::Integer(100));
            Document {
                id: "prefix_boost_high".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    let resp_default = post_search(
        &app,
        index_name,
        json!({
            "query": "red",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_default.status(), StatusCode::OK);
    let body_default = body_json(resp_default).await;
    let hits_default = body_default["hits"].as_array().unwrap();
    assert_eq!(hits_default.len(), 2);
    assert_eq!(
        hits_default[0]["objectID"].as_str().unwrap(),
        "exact_attr_first",
        "Default exactOnSingleWordQuery should be \"attribute\": single-token exact match should outrank prefix-only title match"
    );
}

/// When exactOnSingleWordQuery is "word", treat any single-word match (even within a multi-word value) as an exact match.
#[tokio::test]
async fn search_exact_on_single_word_word_mode_counts_word_matches_as_exact() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_exact_single_word_word_mode_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "priority".to_string()]),
        custom_ranking: Some(vec!["desc(priority)".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let docs = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Red".into()));
            f.insert("priority".to_string(), FieldValue::Integer(0));
            Document {
                id: "exact_attr_first".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Red Shoes".into()));
            f.insert("priority".to_string(), FieldValue::Integer(100));
            Document {
                id: "prefix_boost_high".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    let resp_word = post_search(
        &app,
        index_name,
        json!({
            "query": "red",
            "exactOnSingleWordQuery": "word",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_word.status(), StatusCode::OK);
    let body_word = body_json(resp_word).await;
    let hits_word = body_word["hits"].as_array().unwrap();
    assert_eq!(hits_word.len(), 2);
    assert_eq!(
        hits_word[0]["objectID"].as_str().unwrap(),
        "prefix_boost_high",
        "word mode should treat single-word match in multi-token value as exact"
    );
}

/// When exactOnSingleWordQuery is "none", discard the exact-vs-prefix ranking distinction for single-word queries.
#[tokio::test]
async fn search_exact_on_single_word_none_mode_disables_single_word_exact_distinction() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_exact_single_word_none_mode_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "priority".to_string()]),
        custom_ranking: Some(vec!["desc(priority)".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let docs = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Red".into()));
            f.insert("priority".to_string(), FieldValue::Integer(0));
            Document {
                id: "exact_attr_first".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Red Shoes".into()));
            f.insert("priority".to_string(), FieldValue::Integer(100));
            Document {
                id: "prefix_boost_high".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    let resp_none = post_search(
        &app,
        index_name,
        json!({
            "query": "red",
            "exactOnSingleWordQuery": "none",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_none.status(), StatusCode::OK);
    let body_none = body_json(resp_none).await;
    let hits_none = body_none["hits"].as_array().unwrap();
    assert_eq!(hits_none.len(), 2);
    assert_eq!(
        hits_none[0]["objectID"].as_str().unwrap(),
        "prefix_boost_high",
        "none mode should ignore exact-vs-prefix distinction for single-word queries"
    );
}

/// Allow request-level disableExactOnAttributes to override index settings, re-enabling exact-tier ranking when cleared.
#[tokio::test]
async fn search_disable_exact_on_attributes_request_overrides_settings() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_disable_exact_on_attributes_override_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "priority".to_string()]),
        disable_exact_on_attributes: Some(vec!["title".to_string()]),
        custom_ranking: Some(vec!["desc(priority)".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let docs = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Red".into()));
            f.insert("priority".to_string(), FieldValue::Integer(0));
            Document {
                id: "override_sensitive_first".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Red Shoes".into()));
            f.insert("priority".to_string(), FieldValue::Integer(100));
            Document {
                id: "settings_only_first".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    let resp_settings = post_search(
        &app,
        index_name,
        json!({
            "query": "red",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_settings.status(), StatusCode::OK);
    let body_settings = body_json(resp_settings).await;
    let hits_settings = body_settings["hits"].as_array().unwrap();
    assert_eq!(hits_settings.len(), 2);
    assert_eq!(
        hits_settings[0]["objectID"].as_str().unwrap(),
        "settings_only_first",
        "Index setting disableExactOnAttributes should disable title exactness when request does not override"
    );

    let resp_override = post_search(
        &app,
        index_name,
        json!({
            "query": "red",
            "disableExactOnAttributes": [],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_override.status(), StatusCode::OK);
    let body_override = body_json(resp_override).await;
    let hits_override = body_override["hits"].as_array().unwrap();
    assert_eq!(hits_override.len(), 2);
    assert_eq!(
        hits_override[0]["objectID"].as_str().unwrap(),
        "override_sensitive_first",
        "Request disableExactOnAttributes should override settings and restore title exactness"
    );
}

/// Ensure disableExactOnAttributes only removes the exactness bonus and does not disable the attribute criterion that ranks earlier-position matches higher.
#[tokio::test]
async fn search_disable_exact_on_attributes_does_not_disable_attribute_criterion() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_disable_exact_disabled_only_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec![
            "title".to_string(),
            "description".to_string(),
            "priority".to_string(),
        ]),
        custom_ranking: Some(vec!["desc(priority)".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let docs = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Red".into()));
            f.insert(
                "description".to_string(),
                FieldValue::Text("red shoes".into()),
            );
            f.insert("priority".to_string(), FieldValue::Integer(0));
            Document {
                id: "attr0_match".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Shoes".into()));
            f.insert("description".to_string(), FieldValue::Text("red".into()));
            f.insert("priority".to_string(), FieldValue::Integer(100));
            Document {
                id: "attr1_match".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    let resp_baseline = post_search(
        &app,
        index_name,
        json!({
            "query": "red",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_baseline.status(), StatusCode::OK);
    let body_baseline = body_json(resp_baseline).await;
    let hits_baseline = body_baseline["hits"].as_array().unwrap();
    assert_eq!(hits_baseline.len(), 2);
    assert_eq!(
        hits_baseline[0]["objectID"].as_str().unwrap(),
        "attr0_match",
        "baseline must rank earlier searchable attribute match first even when custom ranking favors attr1_match"
    );

    let resp_disabled = post_search(
        &app,
        index_name,
        json!({
            "query": "red",
            "disableExactOnAttributes": ["title"],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_disabled.status(), StatusCode::OK);
    let body_disabled = body_json(resp_disabled).await;
    let hits = body_disabled["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0]["objectID"].as_str().unwrap(),
        "attr0_match",
        "disableExactOnAttributes must not disable attribute criterion at HTTP boundary even when exact/custom favor attr1_match"
    );
}

/// Prevent docs from receiving exact-tier ranking if their exact match exists only on a disabled attribute.
#[tokio::test]
async fn search_disable_exact_on_attributes_disabled_only_exact_match_is_not_exact() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_disable_exact_disabled_only_exact_http_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec![
            "title".to_string(),
            "description".to_string(),
            "priority".to_string(),
        ]),
        custom_ranking: Some(vec!["desc(priority)".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let docs = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Red".into()));
            f.insert(
                "description".to_string(),
                FieldValue::Text("red shoes".into()),
            );
            f.insert("priority".to_string(), FieldValue::Integer(0));
            Document {
                id: "disabled_exact_only".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("Red".into()));
            f.insert("description".to_string(), FieldValue::Text("Red".into()));
            f.insert("priority".to_string(), FieldValue::Integer(0));
            Document {
                id: "eligible_exact".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    let resp_disabled = post_search(
        &app,
        index_name,
        json!({
            "query": "red",
            "disableExactOnAttributes": ["title"],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_disabled.status(), StatusCode::OK);
    let body_disabled = body_json(resp_disabled).await;
    let hits_disabled = body_disabled["hits"].as_array().unwrap();
    assert_eq!(hits_disabled.len(), 2);
    assert_eq!(
        hits_disabled[0]["objectID"].as_str().unwrap(),
        "eligible_exact",
        "doc with exact match only on disabled attribute must not receive exact-tier credit"
    );
}

// A5 regression: alternativesAsExact is parsed at DTO but deferred at engine level.
// This test ensures setting it does not accidentally alter ranking behavior.
/// Verify that setting alternativesAsExact to any value produces identical ranking behavior, confirming the feature is deferred at the engine level.
#[tokio::test]
async fn search_alternatives_as_exact_has_no_behavioral_effect() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "stage4_alternatives_as_exact_noop_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "priority".to_string()]),
        custom_ranking: Some(vec!["desc(priority)".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let docs = vec![
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("shoe".into()));
            f.insert("priority".to_string(), FieldValue::Integer(10));
            Document {
                id: "doc_exact".into(),
                fields: f,
            }
        },
        {
            let mut f = HashMap::new();
            f.insert("title".to_string(), FieldValue::Text("shoes".into()));
            f.insert("priority".to_string(), FieldValue::Integer(100));
            Document {
                id: "doc_plural".into(),
                fields: f,
            }
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    // Baseline: no alternativesAsExact
    let resp_baseline = post_search(
        &app,
        index_name,
        json!({
            "query": "shoe",
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_baseline.status(), StatusCode::OK);
    let body_baseline = body_json(resp_baseline).await;
    let hits_baseline: Vec<&str> = body_baseline["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();
    assert!(
        body_baseline["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["objectID"].as_str() == Some("doc_plural")),
        "fixture must include a plural-only alternative hit so alternativesAsExact coverage is behavior-relevant"
    );

    // With alternativesAsExact set to empty array
    let resp_empty = post_search(
        &app,
        index_name,
        json!({
            "query": "shoe",
            "alternativesAsExact": [],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_empty.status(), StatusCode::OK);
    let body_empty = body_json(resp_empty).await;
    let hits_empty: Vec<&str> = body_empty["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();

    // With alternativesAsExact set to all values
    let resp_all = post_search(
        &app,
        index_name,
        json!({
            "query": "shoe",
            "alternativesAsExact": ["ignorePlurals", "singleWordSynonym", "multiWordsSynonym"],
            "hitsPerPage": 10
        }),
        None,
    )
    .await;
    assert_eq!(resp_all.status(), StatusCode::OK);
    let body_all = body_json(resp_all).await;
    let hits_all: Vec<&str> = body_all["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["objectID"].as_str().unwrap())
        .collect();

    // All three should produce identical ranking — A5 is a no-op
    assert_eq!(
        hits_baseline, hits_empty,
        "alternativesAsExact=[] should not change ranking (A5 deferred)"
    );
    assert_eq!(
        hits_baseline, hits_all,
        "alternativesAsExact with all flags should not change ranking (A5 deferred)"
    );
}
