use super::*;

#[path = "settings_tests/serde_optional_roundtrip.rs"]
mod serde_optional_roundtrip;

fn roundtrip_settings(settings: &IndexSettings) -> IndexSettings {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("settings.json");
    settings.save(&path).unwrap();
    IndexSettings::load(&path).unwrap()
}

#[test]
fn test_parse_modifiers() {
    assert_eq!(parse_facet_modifier("category"), "category");
    assert_eq!(parse_facet_modifier("filterOnly(price)"), "price");
    assert_eq!(parse_facet_modifier("searchable(brand)"), "brand");
}

#[test]
fn ranking_criterion_token_classifier_accepts_only_runtime_textual_criteria() {
    let recognized = [
        ("typo", RankingCriterionToken::Typo),
        ("geo", RankingCriterionToken::Geo),
        ("words", RankingCriterionToken::Words),
        ("filters", RankingCriterionToken::Filters),
        ("proximity", RankingCriterionToken::Proximity),
        ("attribute", RankingCriterionToken::Attribute),
        ("exact", RankingCriterionToken::Exact),
    ];

    for (source, expected) in recognized {
        assert_eq!(classify_ranking_criterion_token(source), Some(expected));
        assert!(is_ranking_criterion_token(source));
    }

    for source in ["custom", "desc(price)", "typo ", "bogus", ""] {
        assert_eq!(classify_ranking_criterion_token(source), None, "{source}");
        assert!(!is_ranking_criterion_token(source), "{source}");
    }
}

#[test]
fn custom_ranking_token_parser_preserves_runtime_grammar() {
    assert_eq!(
        parse_custom_ranking_token("desc(price)"),
        Some(ParsedCustomRankingToken {
            attribute: "price".to_string(),
            direction: CustomRankingDirection::Desc,
        })
    );
    assert_eq!(
        parse_custom_ranking_token("asc(meta.score)"),
        Some(ParsedCustomRankingToken {
            attribute: "meta.score".to_string(),
            direction: CustomRankingDirection::Asc,
        })
    );
    assert_eq!(
        parse_custom_ranking_token("desc(price"),
        Some(ParsedCustomRankingToken {
            attribute: "price".to_string(),
            direction: CustomRankingDirection::Desc,
        }),
        "existing runtime parser accepts a missing closing parenthesis"
    );
    assert_eq!(parse_custom_ranking_token("custom"), None);
    assert_eq!(parse_custom_ranking_token("ascending(price)"), None);
}

#[test]
fn test_facet_set() {
    let settings = IndexSettings {
        attributes_for_faceting: vec![
            "category".to_string(),
            "filterOnly(price)".to_string(),
            "searchable(brand)".to_string(),
        ],
        ..Default::default()
    };

    let facets = settings.facet_set();
    assert!(facets.contains("category"));
    assert!(facets.contains("price"));
    assert!(facets.contains("brand"));
}

#[test]
fn test_distinct_value() {
    let bool_false = DistinctValue::Bool(false);
    assert_eq!(bool_false.as_count(), 0);

    let bool_true = DistinctValue::Bool(true);
    assert_eq!(bool_true.as_count(), 1);

    let int_val = DistinctValue::Integer(3);
    assert_eq!(int_val.as_count(), 3);
}

/// Verify that all major settings fields—faceting, searchable attributes, ranking, retrieval, distinct—survive a JSON save/load cycle.
#[test]
fn test_settings_roundtrip_preserves_all_fields() {
    let original = IndexSettings {
        attributes_for_faceting: vec!["category".to_string(), "filterOnly(price)".to_string()],
        searchable_attributes: Some(vec!["title".to_string(), "brand".to_string()]),
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        attributes_to_retrieve: Some(vec!["title".to_string(), "price".to_string()]),
        unretrievable_attributes: Some(vec!["internal_id".to_string()]),
        attribute_for_distinct: Some("product_id".to_string()),
        distinct: Some(DistinctValue::Integer(2)),
        ..Default::default()
    };

    let loaded = roundtrip_settings(&original);

    assert_eq!(
        loaded.attributes_for_faceting, original.attributes_for_faceting,
        "attributes_for_faceting mismatch"
    );
    assert_eq!(
        loaded.searchable_attributes, original.searchable_attributes,
        "searchable_attributes mismatch"
    );
    assert_eq!(loaded.ranking, original.ranking, "ranking mismatch");
    assert_eq!(
        loaded.custom_ranking, original.custom_ranking,
        "custom_ranking mismatch"
    );
    assert_eq!(
        loaded.attributes_to_retrieve, original.attributes_to_retrieve,
        "attributes_to_retrieve mismatch"
    );
    assert_eq!(
        loaded.unretrievable_attributes, original.unretrievable_attributes,
        "unretrievable_attributes mismatch"
    );
    assert_eq!(
        loaded.attribute_for_distinct, original.attribute_for_distinct,
        "attribute_for_distinct mismatch"
    );
    assert_eq!(loaded.distinct, original.distinct, "distinct mismatch");
}

#[test]
fn test_partial_json_uses_defaults() {
    let json = r#"{"queryType":"prefixAll"}"#;
    let settings: IndexSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.query_type, "prefixAll");
    assert_eq!(settings.min_word_size_for_1_typo, 4); // default value
}

// ── Embedders field tests (4.1) ──

#[test]
fn test_settings_embedders_default_none() {
    let settings = IndexSettings::default();
    assert!(settings.embedders.is_none());
}

/// Verify that an embedder configuration with source and dimensions survives a JSON save/load cycle.
#[test]
fn test_settings_embedders_roundtrip() {
    let mut embedders = HashMap::new();
    embedders.insert(
        "default".to_string(),
        serde_json::json!({"source": "userProvided", "dimensions": 384}),
    );

    let original = IndexSettings {
        embedders: Some(embedders),
        ..Default::default()
    };

    let loaded = roundtrip_settings(&original);

    let loaded_embedders = loaded
        .embedders
        .as_ref()
        .expect("embedders should survive roundtrip");
    let default_config = loaded_embedders
        .get("default")
        .expect("should have 'default' key");
    assert_eq!(default_config["source"], "userProvided");
    assert_eq!(default_config["dimensions"], 384);
}

#[test]
fn test_settings_embedders_skip_serializing_when_none() {
    let settings = IndexSettings::default();
    let json_str = serde_json::to_string(&settings).unwrap();
    assert!(
        !json_str.contains("embedders"),
        "JSON should not contain 'embedders' when None"
    );
}

#[test]
fn test_settings_embedders_partial_update() {
    let json = r#"{"embedders": {"myEmb": {"source": "userProvided", "dimensions": 128}}}"#;
    let settings: IndexSettings = serde_json::from_str(json).unwrap();

    // Embedders populated
    let emb = settings.embedders.as_ref().unwrap();
    assert_eq!(emb["myEmb"]["dimensions"], 128);

    // Other fields got defaults
    assert_eq!(settings.hits_per_page, 20);
    assert_eq!(settings.query_type, "prefixLast");
}

#[test]
fn test_settings_backward_compat_no_embedders() {
    let json = r#"{"queryType":"prefixAll","hitsPerPage":10}"#;
    let settings: IndexSettings = serde_json::from_str(json).unwrap();
    assert!(settings.embedders.is_none());
    assert_eq!(settings.query_type, "prefixAll");
    assert_eq!(settings.hits_per_page, 10);
}

// ── Embedder validation tests (4.5) ──

#[cfg(feature = "vector-search")]
#[test]
fn test_validate_embedders_valid() {
    let mut embedders = HashMap::new();
    embedders.insert(
        "default".to_string(),
        serde_json::json!({"source": "userProvided", "dimensions": 384}),
    );
    let settings = IndexSettings {
        embedders: Some(embedders),
        ..Default::default()
    };
    assert!(settings.validate_embedders().is_ok());
}

/// Verify that validation rejects an embedder with an unrecognized source and names the offending embedder in the error.
#[cfg(feature = "vector-search")]
#[test]
fn test_validate_embedders_invalid_source() {
    let mut embedders = HashMap::new();
    embedders.insert(
        "broken".to_string(),
        serde_json::json!({"source": "nonExistent"}),
    );
    let settings = IndexSettings {
        embedders: Some(embedders),
        ..Default::default()
    };
    let err = settings.validate_embedders().unwrap_err();
    assert!(
        err.contains("broken"),
        "error should mention embedder name: {}",
        err
    );
}

/// Verify that validation rejects an embedder missing a required field (e.g., `apiKey` for `openAi`) and names both the embedder and the missing field.
#[cfg(feature = "vector-search")]
#[test]
fn test_validate_embedders_missing_required_field() {
    let mut embedders = HashMap::new();
    embedders.insert("myEmb".to_string(), serde_json::json!({"source": "openAi"}));
    let settings = IndexSettings {
        embedders: Some(embedders),
        ..Default::default()
    };
    let err = settings.validate_embedders().unwrap_err();
    assert!(
        err.contains("myEmb"),
        "error should mention embedder name: {}",
        err
    );
    assert!(
        err.contains("apiKey"),
        "error should mention missing field: {}",
        err
    );
}

#[cfg(feature = "vector-search")]
#[test]
fn test_validate_embedders_null_value_skipped() {
    let mut embedders = HashMap::new();
    embedders.insert("default".to_string(), serde_json::Value::Null);
    let settings = IndexSettings {
        embedders: Some(embedders),
        ..Default::default()
    };
    assert!(settings.validate_embedders().is_ok());
}

// ── indexLanguages tests (Stage 2 B) ──

#[test]
fn test_index_languages_default_empty() {
    let settings = IndexSettings::default();
    assert!(settings.index_languages.is_empty());
}

/// Verify that `indexLanguages` survives a JSON save/load cycle and preserves order.
#[test]
fn test_index_languages_roundtrip() {
    let original = IndexSettings {
        index_languages: vec!["ja".to_string(), "en".to_string()],
        ..Default::default()
    };

    let loaded = roundtrip_settings(&original);

    assert_eq!(
        loaded.index_languages,
        vec!["ja".to_string(), "en".to_string()]
    );
}

#[test]
fn test_index_languages_skip_serializing_when_empty() {
    let settings = IndexSettings::default();
    let json_str = serde_json::to_string(&settings).unwrap();
    assert!(
        !json_str.contains("indexLanguages"),
        "JSON should not contain 'indexLanguages' when empty"
    );
}

#[test]
fn test_index_languages_backward_compat() {
    let json = r#"{"queryType":"prefixAll"}"#;
    let settings: IndexSettings = serde_json::from_str(json).unwrap();
    assert!(settings.index_languages.is_empty());
}

#[test]
fn test_validate_embedders_none_is_ok() {
    let settings = IndexSettings::default();
    assert!(settings.validate_embedders().is_ok());
}

#[test]
fn test_validate_embedders_empty_map_is_ok() {
    let settings = IndexSettings {
        embedders: Some(HashMap::new()),
        ..Default::default()
    };
    assert!(settings.validate_embedders().is_ok());
}

// ── Stale vector detection tests (4.8) ──

#[test]
fn test_embedder_changes_detects_model_change() {
    let old = Some(HashMap::from([(
        "emb1".to_string(),
        serde_json::json!({"source": "openAi", "model": "A"}),
    )]));
    let new = Some(HashMap::from([(
        "emb1".to_string(),
        serde_json::json!({"source": "openAi", "model": "B"}),
    )]));
    let changes = detect_embedder_changes(&old, &new);
    assert_eq!(changes, vec![EmbedderChange::Modified("emb1".to_string())]);
}

#[test]
fn test_embedder_changes_detects_source_change() {
    let old = Some(HashMap::from([(
        "emb1".to_string(),
        serde_json::json!({"source": "openAi"}),
    )]));
    let new = Some(HashMap::from([(
        "emb1".to_string(),
        serde_json::json!({"source": "rest"}),
    )]));
    let changes = detect_embedder_changes(&old, &new);
    assert_eq!(changes, vec![EmbedderChange::Modified("emb1".to_string())]);
}

#[test]
fn test_embedder_changes_detects_dimensions_change() {
    let old = Some(HashMap::from([(
        "emb1".to_string(),
        serde_json::json!({"source": "userProvided", "dimensions": 128}),
    )]));
    let new = Some(HashMap::from([(
        "emb1".to_string(),
        serde_json::json!({"source": "userProvided", "dimensions": 384}),
    )]));
    let changes = detect_embedder_changes(&old, &new);
    assert_eq!(changes, vec![EmbedderChange::Modified("emb1".to_string())]);
}

#[test]
fn test_embedder_changes_unchanged() {
    let config = serde_json::json!({"source": "userProvided", "dimensions": 384});
    let old = Some(HashMap::from([("emb1".to_string(), config.clone())]));
    let new = Some(HashMap::from([("emb1".to_string(), config)]));
    let changes = detect_embedder_changes(&old, &new);
    assert!(changes.is_empty());
}

#[test]
fn test_embedder_changes_new_embedder() {
    let old: Option<HashMap<String, serde_json::Value>> = Some(HashMap::new());
    let new = Some(HashMap::from([(
        "new_emb".to_string(),
        serde_json::json!({"source": "userProvided", "dimensions": 128}),
    )]));
    let changes = detect_embedder_changes(&old, &new);
    assert_eq!(changes, vec![EmbedderChange::Added("new_emb".to_string())]);
}

#[test]
fn test_embedder_changes_removed_embedder() {
    let old = Some(HashMap::from([(
        "old_emb".to_string(),
        serde_json::json!({"source": "userProvided", "dimensions": 128}),
    )]));
    let new: Option<HashMap<String, serde_json::Value>> = Some(HashMap::new());
    let changes = detect_embedder_changes(&old, &new);
    assert_eq!(
        changes,
        vec![EmbedderChange::Removed("old_emb".to_string())]
    );
}

#[test]
fn test_embedder_changes_both_none() {
    let changes = detect_embedder_changes(&None, &None);
    assert!(changes.is_empty());
}

// ── Mode and SemanticSearch tests (5.2) ──

#[test]
fn test_settings_mode_default_none() {
    let settings = IndexSettings::default();
    assert!(settings.mode.is_none());
}

#[test]
fn test_settings_enable_personalization_default_none() {
    let settings = IndexSettings::default();
    assert!(settings.enable_personalization.is_none());
}

#[test]
fn test_settings_enable_personalization_roundtrip() {
    let original = IndexSettings {
        enable_personalization: Some(false),
        ..Default::default()
    };

    let loaded = roundtrip_settings(&original);
    assert_eq!(loaded.enable_personalization, Some(false));
}

#[test]
fn test_settings_enable_re_ranking_roundtrip() {
    let original = IndexSettings {
        enable_re_ranking: Some(true),
        ..Default::default()
    };

    let loaded = roundtrip_settings(&original);
    assert_eq!(loaded.enable_re_ranking, Some(true));
}

#[test]
fn test_settings_mode_roundtrip() {
    let original = IndexSettings {
        mode: Some(IndexMode::NeuralSearch),
        ..Default::default()
    };

    let loaded = roundtrip_settings(&original);

    assert_eq!(loaded.mode, Some(IndexMode::NeuralSearch));
}

#[test]
fn test_settings_mode_skip_serializing_when_none() {
    let settings = IndexSettings::default();
    let json_str = serde_json::to_string(&settings).unwrap();
    assert!(
        !json_str.contains("\"mode\""),
        "JSON should not contain 'mode' when None"
    );
}

/// Verify that `IndexMode` variants serialize to and deserialize from their camelCase JSON string representations.
#[test]
fn test_settings_mode_serde_values() {
    // Deserialize
    let neural: IndexMode = serde_json::from_str("\"neuralSearch\"").unwrap();
    assert_eq!(neural, IndexMode::NeuralSearch);

    let keyword: IndexMode = serde_json::from_str("\"keywordSearch\"").unwrap();
    assert_eq!(keyword, IndexMode::KeywordSearch);

    // Serialize
    assert_eq!(
        serde_json::to_string(&IndexMode::NeuralSearch).unwrap(),
        "\"neuralSearch\""
    );
    assert_eq!(
        serde_json::to_string(&IndexMode::KeywordSearch).unwrap(),
        "\"keywordSearch\""
    );
}

#[test]
fn test_settings_mode_backward_compat() {
    let json = r#"{"queryType":"prefixAll","hitsPerPage":10}"#;
    let settings: IndexSettings = serde_json::from_str(json).unwrap();
    assert!(settings.mode.is_none());
    assert_eq!(settings.query_type, "prefixAll");
}

#[test]
fn test_settings_semantic_search_default_none() {
    let settings = IndexSettings::default();
    assert!(settings.semantic_search.is_none());
}

/// Verify that `semanticSearch` with `eventSources` survives a JSON save/load cycle.
#[test]
fn test_settings_semantic_search_roundtrip() {
    let original = IndexSettings {
        semantic_search: Some(SemanticSearchSettings {
            event_sources: Some(vec!["idx1".to_string(), "idx2".to_string()]),
        }),
        ..Default::default()
    };

    let loaded = roundtrip_settings(&original);

    let ss = loaded
        .semantic_search
        .expect("semantic_search should survive roundtrip");
    assert_eq!(
        ss.event_sources,
        Some(vec!["idx1".to_string(), "idx2".to_string()])
    );
}

/// Verify that `SemanticSearchSettings` serializes `event_sources` as camelCase `eventSources` and roundtrips correctly.
#[test]
fn test_settings_semantic_search_event_sources() {
    let ss = SemanticSearchSettings {
        event_sources: Some(vec!["idx1".to_string()]),
    };
    let json_str = serde_json::to_string(&ss).unwrap();
    assert!(
        json_str.contains("eventSources"),
        "should use camelCase: {}",
        json_str
    );
    assert!(
        !json_str.contains("event_sources"),
        "should not use snake_case: {}",
        json_str
    );

    // Roundtrip
    let deserialized: SemanticSearchSettings = serde_json::from_str(&json_str).unwrap();
    assert_eq!(deserialized.event_sources, Some(vec!["idx1".to_string()]));
}

/// Verify that `camelCaseAttributes` survives a JSON save/load cycle.
#[test]
fn test_settings_camel_case_attributes_roundtrip() {
    let settings = IndexSettings {
        camel_case_attributes: vec!["productName".to_string()],
        ..Default::default()
    };
    assert_eq!(
        settings.camel_case_attributes,
        vec!["productName".to_string()]
    );

    let loaded = roundtrip_settings(&settings);
    assert_eq!(
        loaded.camel_case_attributes,
        vec!["productName".to_string()]
    );
}

#[test]
fn test_settings_camel_case_attributes_serde_key() {
    let json = r#"{"camelCaseAttributes":["productName"]}"#;
    let settings: IndexSettings = serde_json::from_str(json).unwrap();
    assert_eq!(
        settings.camel_case_attributes,
        vec!["productName".to_string()]
    );
}

#[test]
fn test_settings_decompounded_attributes_roundtrip() {
    let mut settings = IndexSettings::default();
    let mut decompounded = HashMap::new();
    decompounded.insert("de".to_string(), vec!["title".to_string()]);
    settings.decompounded_attributes = Some(decompounded.clone());

    let loaded = roundtrip_settings(&settings);

    assert_eq!(loaded.decompounded_attributes, Some(decompounded));
}

#[test]
fn test_settings_decompounded_attributes_serde_key() {
    let json = r#"{"decompoundedAttributes":{"de":["title"]}}"#;
    let settings: IndexSettings = serde_json::from_str(json).unwrap();
    let mut expected = HashMap::new();
    expected.insert("de".to_string(), vec!["title".to_string()]);
    assert_eq!(settings.decompounded_attributes, Some(expected));
}

/// Verify that `is_neural_search_active` returns `true` only when mode is `NeuralSearch`.
#[test]
fn test_settings_is_neural_search_active() {
    let mut settings = IndexSettings::default();
    assert!(
        !settings.is_neural_search_active(),
        "default should not be neural"
    );

    settings.mode = Some(IndexMode::KeywordSearch);
    assert!(
        !settings.is_neural_search_active(),
        "keywordSearch should not be neural"
    );

    settings.mode = Some(IndexMode::NeuralSearch);
    assert!(
        settings.is_neural_search_active(),
        "neuralSearch should be neural"
    );
}

// ── searchableAttributes unordered() round-trip tests ──

/// Verify that `searchableAttributes` containing `unordered()` wrappers survive a file save/load cycle intact.
#[test]
fn test_searchable_attributes_unordered_file_roundtrip() {
    let original = IndexSettings {
        searchable_attributes: Some(vec![
            "unordered(title)".to_string(),
            "description".to_string(),
        ]),
        ..Default::default()
    };

    let loaded = roundtrip_settings(&original);
    assert_eq!(loaded.searchable_attributes, original.searchable_attributes);
}

#[test]
fn test_searchable_attributes_unordered_json_deserialize() {
    let json = r#"{"searchableAttributes": ["unordered(title)", "body"]}"#;
    let settings: IndexSettings = serde_json::from_str(json).unwrap();
    assert_eq!(
        settings.searchable_attributes,
        Some(vec!["unordered(title)".to_string(), "body".to_string()])
    );
}

#[test]
fn test_strip_unordered_prefix_valid_wrapper() {
    assert_eq!(strip_unordered_prefix("unordered(title)"), "title");
    assert_eq!(
        strip_unordered_prefix("unordered(description.text)"),
        "description.text"
    );
}

#[test]
fn test_strip_unordered_prefix_non_wrapper_passthrough() {
    assert_eq!(strip_unordered_prefix("title"), "title");
    // Missing closing paren should not be treated as wrapped.
    assert_eq!(strip_unordered_prefix("unordered(title"), "unordered(title");
}

// ── allowCompressionOfIntegerArray: no-op compatibility field ──
// This setting is accepted and persisted but has no behavioral effect
// on indexing or querying. It exists solely for SDK client compatibility.

#[test]
fn test_allow_compression_of_integer_array_deserialize() {
    let json = r#"{"allowCompressionOfIntegerArray": true}"#;
    let settings: IndexSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.allow_compression_of_integer_array, Some(true));
}

#[test]
fn test_allow_compression_of_integer_array_serialize() {
    let settings = IndexSettings {
        allow_compression_of_integer_array: Some(false),
        ..Default::default()
    };
    let json = serde_json::to_string(&settings).unwrap();
    assert!(json.contains(r#""allowCompressionOfIntegerArray":false"#));
}

#[test]
fn test_allow_compression_of_integer_array_default_skipped() {
    let settings = IndexSettings::default();
    assert!(settings.allow_compression_of_integer_array.is_none());
    let json = serde_json::to_string(&settings).unwrap();
    assert!(!json.contains("allowCompressionOfIntegerArray"));
}
