use super::*;

// --- is_cjk ---

#[test]
fn cjk_unified_ideograph() {
    assert!(is_cjk('\u{4E2D}')); // 中
    assert!(is_cjk('\u{6587}')); // 文
}

#[test]
fn cjk_extension_a() {
    assert!(is_cjk('\u{3400}')); // first in extension A
    assert!(is_cjk('\u{4DBF}')); // last in extension A
}

#[test]
fn cjk_hangul() {
    assert!(is_cjk('\u{AC00}')); // first Hangul syllable
    assert!(is_cjk('\u{D7AF}')); // last Hangul syllable
}

#[test]
fn cjk_hiragana_katakana() {
    assert!(is_cjk('\u{3041}')); // hiragana small A
    assert!(is_cjk('\u{30A2}')); // katakana A
}

#[test]
fn cjk_false_for_ascii() {
    assert!(!is_cjk('a'));
    assert!(!is_cjk('Z'));
    assert!(!is_cjk('0'));
    assert!(!is_cjk(' '));
    assert!(!is_cjk('-'));
}

#[test]
fn cjk_false_for_latin_extended() {
    assert!(!is_cjk('é'));
    assert!(!is_cjk('ñ'));
}

// --- split_cjk_aware ---

#[test]
fn split_pure_ascii() {
    assert_eq!(split_cjk_aware("laptop"), vec!["laptop"]);
}

#[test]
fn split_two_ascii_words() {
    assert_eq!(split_cjk_aware("hot dog"), vec!["hot", "dog"]);
}

#[test]
fn split_cjk_sequence() {
    // Each CJK character becomes its own token
    let result = split_cjk_aware("中文");
    assert_eq!(result, vec!["中", "文"]);
}

#[test]
fn split_mixed_ascii_cjk() {
    let result = split_cjk_aware("hello世界");
    assert_eq!(result, vec!["hello", "世", "界"]);
}

#[test]
fn split_cjk_then_ascii() {
    let result = split_cjk_aware("世界hello");
    assert_eq!(result, vec!["世", "界", "hello"]);
}

#[test]
fn split_cjk_interspersed() {
    // CJK, ascii, CJK
    let result = split_cjk_aware("中hello文");
    assert_eq!(result, vec!["中", "hello", "文"]);
}

#[test]
fn split_empty_string() {
    let result = split_cjk_aware("");
    assert!(result.is_empty());
}

#[test]
fn split_whitespace_only() {
    let result = split_cjk_aware("   ");
    assert!(result.is_empty());
}

#[test]
fn split_punctuation_stripped() {
    // hyphens and punctuation are not alphanumeric, so they split
    let result = split_cjk_aware("well-known");
    assert_eq!(result, vec!["well", "known"]);
}

#[test]
fn split_with_indexed_separators() {
    let result = split_cjk_aware_with_indexed_separators("c++ is #1", &['+', '#']);
    assert_eq!(result, vec!["c", "+", "+", "is", "#", "1"]);
}

#[test]
fn split_numbers_kept() {
    let result = split_cjk_aware("iphone15");
    assert_eq!(result, vec!["iphone15"]);
}

#[test]
fn split_trailing_space_no_empty_token() {
    let result = split_cjk_aware("laptop ");
    assert_eq!(result, vec!["laptop"]);
}

// --- preprocess_advanced_syntax ---

#[test]
fn advanced_no_special_tokens() {
    let (phrases, exclusions, remaining) =
        QueryParser::preprocess_advanced_syntax("laptop samsung");
    assert!(phrases.is_empty());
    assert!(exclusions.is_empty());
    assert_eq!(remaining, "laptop samsung");
}

#[test]
fn advanced_quoted_phrase() {
    let (phrases, exclusions, remaining) =
        QueryParser::preprocess_advanced_syntax(r#""gaming laptop" review"#);
    assert_eq!(phrases, vec!["gaming laptop"]);
    assert!(exclusions.is_empty());
    assert_eq!(remaining.trim(), "review");
}

#[test]
fn advanced_multiple_phrases() {
    let (phrases, _, remaining) =
        QueryParser::preprocess_advanced_syntax(r#""red shoes" "size 10""#);
    assert_eq!(phrases, vec!["red shoes", "size 10"]);
    assert_eq!(remaining.trim(), "");
}

#[test]
fn advanced_exclusion() {
    let (phrases, exclusions, remaining) =
        QueryParser::preprocess_advanced_syntax("laptop -refurbished");
    assert!(phrases.is_empty());
    assert_eq!(exclusions, vec!["refurbished"]);
    assert_eq!(remaining.trim(), "laptop");
}

#[test]
fn advanced_multiple_exclusions() {
    let (_, exclusions, remaining) =
        QueryParser::preprocess_advanced_syntax("laptop -used -refurbished");
    assert_eq!(exclusions, vec!["used", "refurbished"]);
    assert_eq!(remaining.trim(), "laptop");
}

#[test]
fn advanced_phrase_and_exclusion() {
    let (phrases, exclusions, remaining) =
        QueryParser::preprocess_advanced_syntax(r#""gaming laptop" -cheap"#);
    assert_eq!(phrases, vec!["gaming laptop"]);
    assert_eq!(exclusions, vec!["cheap"]);
    assert_eq!(remaining.trim(), "");
}

#[test]
fn advanced_empty_quotes_ignored() {
    let (phrases, _, _) = QueryParser::preprocess_advanced_syntax(r#""""#);
    assert!(phrases.is_empty());
}

#[test]
fn advanced_unclosed_quote_captures_rest() {
    let (phrases, _, _) = QueryParser::preprocess_advanced_syntax(r#""open phrase"#);
    assert_eq!(phrases, vec!["open phrase"]);
}

#[test]
fn advanced_hyphen_mid_word_not_exclusion() {
    // "-" only triggers exclusion at word boundary (after space or start)
    let (_, exclusions, remaining) = QueryParser::preprocess_advanced_syntax("well-known laptop");
    // The hyphen in "well-known" is mid-word, not an exclusion
    assert!(exclusions.is_empty());
    assert!(remaining.contains("well"));
}

// --- extract_terms ---

#[test]
fn extract_terms_basic() {
    let schema = tantivy::schema::Schema::builder().build();
    let parser = QueryParser::new(&schema, vec![]);
    let q = Query {
        text: "Laptop Gaming".to_string(),
    };
    assert_eq!(parser.extract_terms(&q), vec!["laptop", "gaming"]);
}

#[test]
fn extract_terms_empty() {
    let schema = tantivy::schema::Schema::builder().build();
    let parser = QueryParser::new(&schema, vec![]);
    let q = Query {
        text: "".to_string(),
    };
    assert!(parser.extract_terms(&q).is_empty());
}

#[test]
fn extract_terms_trims_punctuation() {
    let schema = tantivy::schema::Schema::builder().build();
    let parser = QueryParser::new(&schema, vec![]);
    let q = Query {
        text: "hello, world!".to_string(),
    };
    // split_cjk_aware splits on non-alnum, then extract_terms trims
    let terms = parser.extract_terms(&q);
    assert_eq!(terms, vec!["hello", "world"]);
}

#[test]
fn extract_terms_cjk() {
    let schema = tantivy::schema::Schema::builder().build();
    let parser = QueryParser::new(&schema, vec![]);
    let q = Query {
        text: "hello世界".to_string(),
    };
    let terms = parser.extract_terms(&q);
    assert_eq!(terms, vec!["hello", "世", "界"]);
}

#[test]
fn extract_terms_strips_trailing_star() {
    let schema = tantivy::schema::Schema::builder().build();
    let parser = QueryParser::new(&schema, vec![]);
    // extract_terms lowercases but doesn't strip stars (that's in parse())
    let q = Query {
        text: "laptop*".to_string(),
    };
    let terms = parser.extract_terms(&q);
    // The * is non-alphanumeric, so split_cjk_aware excludes it
    assert_eq!(terms, vec!["laptop"]);
}

#[test]
fn extract_terms_normalizes_diacritics_by_default() {
    let schema = tantivy::schema::Schema::builder().build();
    let parser = QueryParser::new(&schema, vec![]);
    let q = Query {
        text: "København".to_string(),
    };
    assert_eq!(parser.extract_terms(&q), vec!["kobenhavn"]);
}

#[test]
fn extract_terms_keeps_selected_diacritics() {
    let schema = tantivy::schema::Schema::builder().build();
    let parser = QueryParser::new(&schema, vec![]).with_keep_diacritics_on_characters("ø");
    let q = Query {
        text: "København".to_string(),
    };
    assert_eq!(parser.extract_terms(&q), vec!["københavn"]);
}

// --- advancedSyntaxFeatures ---

/// Verify that when advancedSyntaxFeatures contains only "exactPhrase", quoted phrases are parsed as advanced syntax while exclusions (hyphens) are treated as regular text.
#[test]
fn advanced_syntax_features_exact_phrase_only() {
    // When advancedSyntaxFeatures = ["exactPhrase"], phrases should work
    // but exclusions should NOT work (dash treated as regular text)
    let mut builder = tantivy::schema::Schema::builder();
    let opts = tantivy::schema::JsonObjectOptions::default().set_indexing_options(
        tantivy::schema::TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(tantivy::schema::IndexRecordOption::WithFreqsAndPositions),
    );
    let json_field = builder.add_json_field("_json_search", opts.clone());
    let json_exact = builder.add_json_field("_json_exact", opts);
    let schema = builder.build();

    let parser = QueryParser::new_with_weights(
        &schema,
        vec![json_field],
        vec![1.0],
        vec!["name".to_string()],
    )
    .with_exact_field(json_exact)
    .with_advanced_syntax(true)
    .with_advanced_syntax_features(vec!["exactPhrase".to_string()]);

    // Phrase should be parsed as advanced syntax
    let q = Query {
        text: r#""blue wireless""#.to_string(),
    };
    let result = parser.parse(&q);
    assert!(result.is_ok(), "phrase query should parse successfully");

    // Exclusion should NOT be treated as advanced syntax (feature not enabled)
    let q2 = Query {
        text: "laptop -desktop".to_string(),
    };
    let result2 = parser.parse(&q2);
    assert!(result2.is_ok(), "exclusion query should parse successfully");
    // The query should be a regular query, not a BooleanQuery with MustNot
    let debug = format!("{:?}", result2.unwrap());
    assert!(
        !debug.contains("MustNot"),
        "exclusion should not be treated as MustNot when excludeWords feature is disabled, got: {}",
        debug
    );
}

/// Verify that when advancedSyntaxFeatures contains only "excludeWords", hyphens trigger word exclusion while quoted phrases are treated as regular text.
#[test]
fn advanced_syntax_features_exclude_words_only() {
    // When advancedSyntaxFeatures = ["excludeWords"], exclusions should work
    // but phrases should NOT work (quotes treated as regular text)
    let mut builder = tantivy::schema::Schema::builder();
    let opts = tantivy::schema::JsonObjectOptions::default().set_indexing_options(
        tantivy::schema::TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(tantivy::schema::IndexRecordOption::WithFreqsAndPositions),
    );
    let json_field = builder.add_json_field("_json_search", opts.clone());
    let json_exact = builder.add_json_field("_json_exact", opts);
    let schema = builder.build();

    let parser = QueryParser::new_with_weights(
        &schema,
        vec![json_field],
        vec![1.0],
        vec!["name".to_string()],
    )
    .with_exact_field(json_exact)
    .with_advanced_syntax(true)
    .with_advanced_syntax_features(vec!["excludeWords".to_string()]);

    // Exclusion should work
    let q = Query {
        text: "laptop -desktop".to_string(),
    };
    let result = parser.parse(&q);
    assert!(result.is_ok(), "exclusion query should parse successfully");
    let debug = format!("{:?}", result.unwrap());
    assert!(
        debug.contains("MustNot"),
        "exclusion should be treated as MustNot when excludeWords is enabled, got: {}",
        debug
    );
}

/// Verify that when advancedSyntaxFeatures includes both "exactPhrase" and "excludeWords", both features parse correctly in combined queries.
#[test]
fn advanced_syntax_features_both_enabled() {
    // When advancedSyntaxFeatures = ["exactPhrase", "excludeWords"], both should work
    let mut builder = tantivy::schema::Schema::builder();
    let opts = tantivy::schema::JsonObjectOptions::default().set_indexing_options(
        tantivy::schema::TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(tantivy::schema::IndexRecordOption::WithFreqsAndPositions),
    );
    let json_field = builder.add_json_field("_json_search", opts.clone());
    let json_exact = builder.add_json_field("_json_exact", opts);
    let schema = builder.build();

    let parser = QueryParser::new_with_weights(
        &schema,
        vec![json_field],
        vec![1.0],
        vec!["name".to_string()],
    )
    .with_exact_field(json_exact)
    .with_advanced_syntax(true)
    .with_advanced_syntax_features(vec!["exactPhrase".to_string(), "excludeWords".to_string()]);

    // Both should work when both features are enabled
    let q = Query {
        text: r#""blue wireless" -desktop"#.to_string(),
    };
    let result = parser.parse(&q);
    assert!(result.is_ok(), "combined query should parse successfully");
    let debug = format!("{:?}", result.unwrap());
    assert!(
        debug.contains("MustNot"),
        "exclusion should work when excludeWords is enabled"
    );
}

/// Verify backward compatibility: when advancedSyntaxFeatures is None (default), both quoted phrases and word exclusions are enabled.
#[test]
fn advanced_syntax_features_default_both_enabled() {
    // When advancedSyntaxFeatures is None (default), both features should be enabled
    // (backward compatibility)
    let mut builder = tantivy::schema::Schema::builder();
    let opts = tantivy::schema::JsonObjectOptions::default().set_indexing_options(
        tantivy::schema::TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(tantivy::schema::IndexRecordOption::WithFreqsAndPositions),
    );
    let json_field = builder.add_json_field("_json_search", opts.clone());
    let json_exact = builder.add_json_field("_json_exact", opts);
    let schema = builder.build();

    let parser = QueryParser::new_with_weights(
        &schema,
        vec![json_field],
        vec![1.0],
        vec!["name".to_string()],
    )
    .with_exact_field(json_exact)
    .with_advanced_syntax(true);
    // No .with_advanced_syntax_features() call — defaults to both enabled

    let q = Query {
        text: r#""blue wireless" -desktop"#.to_string(),
    };
    let result = parser.parse(&q);
    assert!(
        result.is_ok(),
        "combined query should parse when features default to both"
    );
    let debug = format!("{:?}", result.unwrap());
    assert!(
        debug.contains("MustNot"),
        "exclusion should work by default"
    );
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// Verify that words in the disabled list avoid fuzzy matching clauses while other words still apply typo tolerance.
#[test]
fn disables_typo_tolerance_for_words() {
    let mut builder = tantivy::schema::Schema::builder();
    let opts = tantivy::schema::JsonObjectOptions::default().set_indexing_options(
        tantivy::schema::TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(tantivy::schema::IndexRecordOption::WithFreqsAndPositions),
    );
    let json_field = builder.add_json_field("_json_search", opts.clone());
    let json_exact = builder.add_json_field("_json_exact", opts);
    let schema = builder.build();

    let parser = QueryParser::new_with_weights(
        &schema,
        vec![json_field],
        vec![1.0],
        vec!["title".to_string()],
    )
    .with_exact_field(json_exact);
    let query = Query {
        text: "iphonne".to_string(),
    };

    let typo_enabled = parser.parse(&query).unwrap();
    let typo_enabled_debug = format!("{:?}", typo_enabled);
    assert!(
        typo_enabled_debug.contains("FuzzyTermQuery"),
        "when typo tolerance is not disabled, parser should build fuzzy clause"
    );

    let typo_disabled = parser
        .with_disabled_typo_words(vec!["iphonne".to_string()])
        .parse(&query)
        .unwrap();
    let typo_disabled_debug = format!("{:?}", typo_disabled);
    assert!(
        !typo_disabled_debug.contains("FuzzyTermQuery"),
        "disabled typo words should avoid fuzzy clauses"
    );
}

/// Verify that attributes in the disabled list suppress fuzzy matching only for those attributes; other attributes remain unaffected.
#[test]
fn disables_typo_tolerance_for_attributes() {
    let mut builder = tantivy::schema::Schema::builder();
    let opts = tantivy::schema::JsonObjectOptions::default().set_indexing_options(
        tantivy::schema::TextFieldIndexing::default()
            .set_tokenizer("default")
            .set_index_option(tantivy::schema::IndexRecordOption::WithFreqsAndPositions),
    );
    let json_field = builder.add_json_field("_json_search", opts.clone());
    let json_exact = builder.add_json_field("_json_exact", opts);
    let schema = builder.build();

    let parser = QueryParser::new_with_weights(
        &schema,
        vec![json_field],
        vec![1.0, 1.0],
        vec!["sku".to_string(), "title".to_string()],
    )
    .with_exact_field(json_exact);
    let query = Query {
        text: "abc12".to_string(),
    };

    let typo_enabled = parser.parse(&query).unwrap();
    let typo_enabled_count = count_occurrences(&format!("{:?}", typo_enabled), "FuzzyTermQuery");
    assert!(
        typo_enabled_count >= 2,
        "baseline should include fuzzy clauses across enabled paths"
    );

    let typo_disabled_attr = parser
        .with_disabled_typo_attrs(vec!["sku".to_string()])
        .parse(&query)
        .unwrap();
    let typo_disabled_attr_count =
        count_occurrences(&format!("{:?}", typo_disabled_attr), "FuzzyTermQuery");
    assert_eq!(
        typo_disabled_attr_count, 1,
        "disabled typo attributes should suppress fuzzy clauses for that attribute only"
    );
}

// Search through the production IndexManager writer, then pin parser path order
// to make the historical fuzzy cap defect reproducible rather than probabilistic.
async fn recall_fixture() -> (tempfile::TempDir, std::sync::Arc<crate::IndexManager>) {
    let tmp = tempfile::tempdir().unwrap();
    let manager = crate::IndexManager::new(tmp.path());
    manager.create_tenant("recall").unwrap();
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../sdk_test/fixtures/algolia_migrant_recall.json"
    ))
    .unwrap();
    let docs = fixture["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|object| {
            let fields = object
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, _)| key.as_str() != "objectID")
                .map(|(key, value)| {
                    (
                        key.clone(),
                        match value {
                            serde_json::Value::String(text) => {
                                crate::FieldValue::Text(text.clone())
                            }
                            serde_json::Value::Number(number) => {
                                crate::FieldValue::Integer(number.as_i64().unwrap())
                            }
                            _ => panic!("unsupported fixture value"),
                        },
                    )
                })
                .collect();
            crate::Document {
                id: object["objectID"].as_str().unwrap().to_owned(),
                fields,
            }
        })
        .collect();
    manager.add_documents_sync("recall", docs).await.unwrap();
    (tmp, manager)
}

fn pinned_recall_ids(
    manager: &crate::IndexManager,
    paths: &[&str],
    query: &str,
    typos: bool,
) -> Vec<String> {
    let index = manager.get_or_load("recall").unwrap();
    let schema = index.inner().schema();
    let parser = QueryParser::new_with_weights(
        &schema,
        vec![schema.get_field("_json_search").unwrap()],
        vec![1.0; paths.len()],
        paths.iter().map(|s| (*s).to_string()).collect(),
    )
    .with_exact_field(schema.get_field("_json_exact").unwrap())
    .with_typo_tolerance(typos);
    let query = parser
        .parse(&Query {
            text: query.to_string(),
        })
        .unwrap();
    let result = crate::QueryExecutor::new(index.converter(), schema)
        .execute(&index.reader().searcher(), query, None, 100)
        .unwrap();
    let mut ids: Vec<_> = result
        .documents
        .into_iter()
        .map(|d| d.document.id)
        .collect();
    ids.sort();
    ids
}

macro_rules! recall_case {
    ($name:ident, $query:expr, $expected:expr, $typos:expr) => {
        #[tokio::test]
        async fn $name() {
            let (_tmp, manager) = recall_fixture().await;
            let paths = if $expected == "p5" {
                ["name", "brand", "category", "sku", "description"]
            } else {
                ["name", "brand", "category", "description", "sku"]
            };
            assert_eq!(
                pinned_recall_ids(&manager, &paths, $query, $typos),
                [$expected],
                "{}",
                $query
            );
        }
    };
}
recall_case!(
    recall_assertion_1_late_typo,
    "trail waterprof membrane",
    "p1",
    true
);
recall_case!(
    recall_assertion_2_late_typo,
    "northpeak footwear lightwieght",
    "p1",
    true
);
recall_case!(
    recall_assertion_3_long_exact,
    "rechtsschutzversicherungsgesellschaften",
    "p5",
    true
);
recall_case!(
    recall_assertion_4_long_prefix,
    "rechtsschutzversicherungs",
    "p5",
    false
);
recall_case!(
    recall_assertion_5_long_final,
    "folder rechtsschutzversicherungsgesellschaften",
    "p5",
    true
);
recall_case!(
    recall_assertion_6_long_id,
    "3f2b8c1e9d4a4b7f8a6c5e1d2f3a4b5c",
    "p6",
    true
);
recall_case!(
    recall_assertion_7_uuid,
    "7c9e6679-7425-40de-944b-e07fc1f90ae7",
    "p6",
    false
);

#[tokio::test]
async fn recall_path_permutation_assertion_8() {
    let (_tmp, manager) = recall_fixture().await;
    let early = ["description", "sku", "name", "brand", "category"];
    let late = ["name", "brand", "category", "description", "sku"];
    for query in ["trail waterprof membrane", "northpeak footwear lightwieght"] {
        let early_ids = pinned_recall_ids(&manager, &early, query, true);
        assert_eq!(early_ids, ["p1"]);
        assert_eq!(
            pinned_recall_ids(&manager, &late, query, true),
            early_ids,
            "{query}"
        );
    }
}

#[tokio::test]
async fn recall_long_exact_fallback_stems_like_non_prefix_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = crate::IndexManager::new(tmp.path());
    manager.create_tenant("stemmed").unwrap();
    let settings = crate::index::settings::IndexSettings {
        index_languages: vec!["en".to_string()],
        ..Default::default()
    };
    settings
        .save(tmp.path().join("stemmed/settings.json"))
        .unwrap();
    manager.unload_tenant("stemmed");
    manager.invalidate_settings_cache("stemmed");
    manager
        .add_documents_sync(
            "stemmed",
            vec![crate::Document {
                id: "long".into(),
                fields: [(
                    "name".into(),
                    crate::FieldValue::Text("internationalizations".into()),
                )]
                .into_iter()
                .collect(),
            }],
        )
        .await
        .unwrap();
    let options = crate::index::SearchOptions {
        typo_tolerance: Some(false),
        ..Default::default()
    };
    for query in ["internationalizations", "INTERNATIONALIZATIONS"] {
        let result = manager
            .search_with_options("stemmed", query, &options)
            .unwrap();
        assert_eq!(result.total, 1, "{query}");
    }
}

#[tokio::test]
async fn recall_default_paths_stay_sorted_after_writes_and_invalidation() {
    let (_tmp, manager) = recall_fixture().await;
    let index = manager.get_or_load("recall").unwrap();
    let mut expected = index.searchable_paths();
    expected.sort();
    for step in 0..3 {
        assert_eq!(index.searchable_paths(), expected);
        manager
            .add_documents_sync(
                "recall",
                vec![crate::Document {
                    id: format!("new{step}"),
                    fields: [(
                        "description".into(),
                        crate::FieldValue::Text("unrelated".into()),
                    )]
                    .into_iter()
                    .collect(),
                }],
            )
            .await
            .unwrap();
        index.invalidate_searchable_paths_cache();
    }
}
