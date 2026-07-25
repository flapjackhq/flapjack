use super::*;
use serde_json::json;
use tempfile::TempDir;

/// Create a minimal rule with no conditions, no consequences, and default settings for testing.
fn bare_rule(id: &str) -> Rule {
    Rule {
        object_id: id.to_string(),
        conditions: vec![],
        consequence: Consequence {
            promote: None,
            hide: None,
            filter_promotes: None,
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    }
}

/// Create a rule with a single pattern condition and no consequences for testing.
fn rule_with_pattern(id: &str, pattern: &str, anchoring: Anchoring) -> Rule {
    Rule {
        object_id: id.to_string(),
        conditions: vec![Condition {
            pattern: Some(pattern.to_string()),
            anchoring: Some(anchoring),
            alternatives: None,
            context: None,
            filters: None,
        }],
        consequence: Consequence {
            promote: None,
            hide: None,
            filter_promotes: None,
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    }
}

// --- Rule::is_enabled ---

#[test]
fn enabled_defaults_to_true() {
    let r = bare_rule("x");
    assert!(r.is_enabled());
}

#[test]
fn enabled_explicit_true() {
    let mut r = bare_rule("x");
    r.enabled = Some(true);
    assert!(r.is_enabled());
}

#[test]
fn enabled_explicit_false() {
    let mut r = bare_rule("x");
    r.enabled = Some(false);
    assert!(!r.is_enabled());
}

// --- Rule::is_valid_at ---

#[test]
fn validity_none_always_valid() {
    let r = bare_rule("x");
    assert!(r.is_valid_at(0));
    assert!(r.is_valid_at(i64::MAX));
}

#[test]
fn validity_within_range() {
    let mut r = bare_rule("x");
    r.validity = Some(vec![TimeRange {
        from: 1000,
        until: 2000,
    }]);
    assert!(r.is_valid_at(1000));
    assert!(r.is_valid_at(1500));
    assert!(r.is_valid_at(2000));
}

#[test]
fn validity_outside_range() {
    let mut r = bare_rule("x");
    r.validity = Some(vec![TimeRange {
        from: 1000,
        until: 2000,
    }]);
    assert!(!r.is_valid_at(999));
    assert!(!r.is_valid_at(2001));
}

/// Verify that a rule with multiple validity time ranges matches if the timestamp falls within any range.
#[test]
fn validity_multiple_ranges_matches_any() {
    let mut r = bare_rule("x");
    r.validity = Some(vec![
        TimeRange {
            from: 100,
            until: 200,
        },
        TimeRange {
            from: 500,
            until: 600,
        },
    ]);
    assert!(r.is_valid_at(150));
    assert!(r.is_valid_at(550));
    assert!(!r.is_valid_at(350));
}

// --- Rule::matches (no conditions → always matches) ---

#[test]
fn no_conditions_always_matches() {
    let r = bare_rule("x");
    assert!(r.matches("anything", None, None, None));
    assert!(r.matches("", None, None, None));
}

#[test]
fn disabled_rule_never_matches() {
    let mut r = rule_with_pattern("x", "laptop", Anchoring::Is);
    r.enabled = Some(false);
    assert!(!r.matches("laptop", None, None, None));
}

// --- RuleStore::apply_rules ---

#[test]
fn apply_rules_promotes_single() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "laptop", Anchoring::Is);
    rule.consequence.promote = Some(vec![Promote::Single {
        object_id: "doc-1".to_string(),
        position: 0,
    }]);
    store.insert(rule);

    let effects = store.apply_rules("laptop", None, None, None);
    assert_eq!(effects.applied_rules, vec!["r1"]);
    assert_eq!(effects.pins, vec![("doc-1".to_string(), 0)]);
    assert!(effects.hidden.is_empty());
}

#[test]
fn apply_rules_carries_filter_promotes_flag() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "laptop", Anchoring::Is);
    rule.consequence.filter_promotes = Some(true);
    rule.consequence.promote = Some(vec![Promote::Single {
        object_id: "doc-1".to_string(),
        position: 0,
    }]);
    store.insert(rule);

    let effects = store.apply_rules("laptop", None, None, None);
    assert_eq!(effects.filter_promotes, Some(true));
}

/// Verify that a `Promote::Multiple` consequence expands into individual pins with sequential positions.
#[test]
fn apply_rules_promotes_multiple() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "sale", Anchoring::Contains);
    rule.consequence.promote = Some(vec![Promote::Multiple {
        object_ids: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        position: 2,
    }]);
    store.insert(rule);

    let effects = store.apply_rules("big sale today", None, None, None);
    assert_eq!(
        effects.pins,
        vec![
            ("a".to_string(), 2),
            ("b".to_string(), 3),
            ("c".to_string(), 4),
        ]
    );
}

#[test]
fn apply_rules_hides() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "laptop", Anchoring::Is);
    rule.consequence.hide = Some(vec![Hide {
        object_id: "bad-doc".to_string(),
    }]);
    store.insert(rule);

    let effects = store.apply_rules("laptop", None, None, None);
    assert_eq!(effects.hidden, vec!["bad-doc"]);
}

/// Verify that a rule with more than 50 hidden object IDs only applies the first 50.
#[test]
fn apply_rules_caps_hidden_items_per_rule_to_50() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "laptop", Anchoring::Contains);
    let hidden: Vec<Hide> = (0..60)
        .map(|i| Hide {
            object_id: format!("hidden-{i}"),
        })
        .collect();
    rule.consequence.hide = Some(hidden);
    store.insert(rule);

    let effects = store.apply_rules("gaming laptop", None, None, None);
    assert_eq!(effects.hidden.len(), 50);
    assert_eq!(effects.hidden[0], "hidden-0");
    assert_eq!(effects.hidden[49], "hidden-49");
}

#[test]
fn apply_rules_user_data() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "promo", Anchoring::Contains);
    rule.consequence.user_data = Some(json!({"banner": "sale"}));
    store.insert(rule);

    let effects = store.apply_rules("promo items", None, None, None);
    assert_eq!(effects.user_data, vec![json!({"banner": "sale"})]);
}

/// Verify that rendering content from multiple rules is deep-merged with later rules overriding conflicting keys.
#[test]
fn apply_rules_merges_rendering_content_with_later_rule_override() {
    let mut store = RuleStore::new();

    let mut first = rule_with_pattern("r1", "laptop", Anchoring::Contains);
    first.consequence.params = Some(ConsequenceParams {
        query: None,
        rendering_content: Some(json!({
            "facetOrdering": {
                "facets": { "order": ["brand"] }
            },
            "redirect": { "url": "https://example.com/old" }
        })),
        ..Default::default()
    });

    let mut second = rule_with_pattern("r2", "laptop", Anchoring::Contains);
    second.consequence.params = Some(ConsequenceParams {
        query: None,
        rendering_content: Some(json!({
            "redirect": { "url": "https://example.com/new" },
            "widgets": {
                "banners": [{
                    "image": {
                        "urls": [{ "url": "https://example.com/banner.jpg" }]
                    }
                }]
            }
        })),
        ..Default::default()
    });

    store.insert(first);
    store.insert(second);

    let effects = store.apply_rules("gaming laptop", None, None, None);
    assert_eq!(
        effects.rendering_content,
        Some(json!({
            "facetOrdering": {
                "facets": { "order": ["brand"] }
            },
            "redirect": { "url": "https://example.com/new" },
            "widgets": {
                "banners": [{
                    "image": {
                        "urls": [{ "url": "https://example.com/banner.jpg" }]
                    }
                }]
            }
        }))
    );
}

#[test]
fn apply_rules_no_match_returns_empty() {
    let mut store = RuleStore::new();
    store.insert(rule_with_pattern("r1", "laptop", Anchoring::Is));

    let effects = store.apply_rules("phone", None, None, None);
    assert!(effects.applied_rules.is_empty());
    assert!(effects.pins.is_empty());
}

/// Verify that accumulated pins from multiple rules are sorted by position in the final effects.
#[test]
fn apply_rules_pins_sorted_by_position() {
    // Two rules both match; their pins should come out sorted
    let mut store = RuleStore::new();
    let mut r1 = rule_with_pattern("r1", "sale", Anchoring::Contains);
    r1.consequence.promote = Some(vec![Promote::Single {
        object_id: "b".to_string(),
        position: 5,
    }]);
    let mut r2 = rule_with_pattern("r2", "sale", Anchoring::Contains);
    r2.consequence.promote = Some(vec![Promote::Single {
        object_id: "a".to_string(),
        position: 1,
    }]);
    store.insert(r1);
    store.insert(r2);

    let effects = store.apply_rules("sale", None, None, None);
    // pins sorted by position: 1, then 5
    assert_eq!(effects.pins[0], ("a".to_string(), 1));
    assert_eq!(effects.pins[1], ("b".to_string(), 5));
}

// --- RuleStore::apply_query_rewrite ---

#[test]
fn query_rewrite_matches() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "tv", Anchoring::Is);
    rule.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Literal("television".to_string())),
        ..Default::default()
    });
    store.insert(rule);

    assert_eq!(
        store.apply_query_rewrite("tv", None, None, None),
        Some("television".to_string())
    );
}

#[test]
fn query_rewrite_no_match() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "tv", Anchoring::Is);
    rule.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Literal("television".to_string())),
        ..Default::default()
    });
    store.insert(rule);

    assert_eq!(store.apply_query_rewrite("phone", None, None, None), None);
}

/// Verify that an `EditType::Remove` edit removes the specified word from the query text.
#[test]
fn query_rewrite_remove_word_edit() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "cheap laptop", Anchoring::Is);
    rule.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Edits {
            remove: None,
            edits: Some(vec![Edit {
                edit_type: EditType::Remove,
                delete: "cheap".to_string(),
                insert: None,
            }]),
        }),
        ..Default::default()
    });
    store.insert(rule);

    assert_eq!(
        store.apply_query_rewrite("cheap laptop", None, None, None),
        Some("laptop".to_string())
    );
}

/// Verify that an `EditType::Replace` edit substitutes one word for another in the query.
#[test]
fn query_rewrite_replace_word_edit() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "phone", Anchoring::Is);
    rule.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Edits {
            remove: None,
            edits: Some(vec![Edit {
                edit_type: EditType::Replace,
                delete: "phone".to_string(),
                insert: Some("smartphone".to_string()),
            }]),
        }),
        ..Default::default()
    });
    store.insert(rule);

    assert_eq!(
        store.apply_query_rewrite("phone", None, None, None),
        Some("smartphone".to_string())
    );
}

/// Verify that the `remove` shorthand array in `ConsequenceQuery::Edits` removes the specified word from the query.
#[test]
fn query_rewrite_remove_shorthand() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "cheap laptop", Anchoring::Is);
    rule.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Edits {
            remove: Some(vec!["cheap".to_string()]),
            edits: None,
        }),
        ..Default::default()
    });
    store.insert(rule);

    assert_eq!(
        store.apply_query_rewrite("cheap laptop", None, None, None),
        Some("laptop".to_string())
    );
}

/// Verify that when both a remove and replace target the same word, the remove takes precedence and the replace is skipped.
#[test]
fn query_rewrite_remove_takes_precedence_over_replace_for_same_word() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "cheap phone", Anchoring::Is);
    rule.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Edits {
            remove: Some(vec!["cheap".to_string()]),
            edits: Some(vec![Edit {
                edit_type: EditType::Replace,
                delete: "cheap".to_string(),
                insert: Some("budget".to_string()),
            }]),
        }),
        ..Default::default()
    });
    store.insert(rule);

    assert_eq!(
        store.apply_query_rewrite("cheap phone", None, None, None),
        Some("phone".to_string())
    );
}

/// Verify that query edits on a conditionless rule are ignored and the query passes through unchanged.
#[test]
fn query_rewrite_conditionless_rule_edits_ignored() {
    let mut store = RuleStore::new();
    let mut rule = bare_rule("r1");
    rule.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Edits {
            remove: Some(vec!["cheap".to_string()]),
            edits: None,
        }),
        ..Default::default()
    });
    store.insert(rule);

    assert_eq!(
        store.apply_query_rewrite("cheap laptop", None, None, None),
        None
    );
}

// --- RuleStore::search ---

#[test]
fn search_empty_query_returns_all() {
    let mut store = RuleStore::new();
    store.insert(bare_rule("alpha"));
    store.insert(bare_rule("beta"));
    store.insert(bare_rule("gamma"));

    let (hits, total) = store.search("", 0, 10);
    assert_eq!(total, 3);
    assert_eq!(hits.len(), 3);
}

#[test]
fn search_filters_by_id() {
    let mut store = RuleStore::new();
    store.insert(bare_rule("laptop-rule"));
    store.insert(bare_rule("phone-rule"));

    let (hits, total) = store.search("laptop", 0, 10);
    assert_eq!(total, 1);
    assert_eq!(hits[0].object_id, "laptop-rule");
}

/// Verify that `RuleStore::search` correctly paginates results across multiple pages.
#[test]
fn search_pagination() {
    let mut store = RuleStore::new();
    for i in 0..5 {
        store.insert(bare_rule(&format!("rule-{}", i)));
    }

    let (page0, total) = store.search("", 0, 2);
    assert_eq!(total, 5);
    assert_eq!(page0.len(), 2);

    let (page1, _) = store.search("", 1, 2);
    assert_eq!(page1.len(), 2);

    let (page2, _) = store.search("", 2, 2);
    assert_eq!(page2.len(), 1);
}

#[test]
fn search_pagination_extreme_page_does_not_panic() {
    let mut store = RuleStore::new();
    for i in 0..5 {
        store.insert(bare_rule(&format!("rule-{i}")));
    }

    let (hits, total) = store.search("", usize::MAX, 2);
    assert_eq!(total, 5);
    assert!(hits.is_empty());
}

#[test]
fn search_past_end_returns_empty() {
    let mut store = RuleStore::new();
    store.insert(bare_rule("only-one"));

    let (hits, total) = store.search("", 5, 10);
    assert_eq!(total, 1);
    assert!(hits.is_empty());
}

/// Verify that `RuleStore::search` matches rules by their condition pattern text.
#[test]
fn search_filters_by_pattern() {
    let mut store = RuleStore::new();
    let mut r = bare_rule("boost-electronics");
    r.conditions.push(Condition {
        pattern: Some("gaming".to_string()),
        anchoring: Some(Anchoring::Contains),
        alternatives: None,
        context: None,
        filters: None,
    });
    store.insert(r);
    store.insert(bare_rule("other-rule"));

    let (hits, total) = store.search("gaming", 0, 10);
    assert_eq!(total, 1);
    assert_eq!(hits[0].object_id, "boost-electronics");
}

// --- Anchoring variants (matches()) ---

#[test]
fn anchoring_is() {
    let r = rule_with_pattern("x", "laptop", Anchoring::Is);
    assert!(r.matches("laptop", None, None, None));
    assert!(r.matches("LAPTOP", None, None, None));
    assert!(!r.matches("gaming laptop", None, None, None));
    assert!(!r.matches("lapto", None, None, None));
}

#[test]
fn anchoring_starts_with() {
    let r = rule_with_pattern("x", "gaming", Anchoring::StartsWith);
    assert!(r.matches("gaming", None, None, None));
    assert!(r.matches("GAMING laptop", None, None, None));
    assert!(!r.matches("laptop gaming", None, None, None));
    assert!(!r.matches("gaminglaptop", None, None, None));
}

#[test]
fn anchoring_ends_with() {
    let r = rule_with_pattern("x", "laptop", Anchoring::EndsWith);
    assert!(r.matches("laptop", None, None, None));
    assert!(r.matches("gaming LAPTOP", None, None, None));
    assert!(!r.matches("laptop gaming", None, None, None));
    assert!(!r.matches("gaminglaptop", None, None, None));
}

#[test]
fn anchoring_contains() {
    let r = rule_with_pattern("x", "laptop", Anchoring::Contains);
    assert!(r.matches("laptop", None, None, None));
    assert!(r.matches("gaming LAPTOP", None, None, None));
    assert!(r.matches("gaming laptop stand", None, None, None));
    assert!(!r.matches("overlaptop", None, None, None));
    assert!(!r.matches("computer", None, None, None));
}

#[test]
fn anchoring_contains_matches_contiguous_phrase_only() {
    let r = rule_with_pattern("x", "gaming laptop", Anchoring::Contains);
    assert!(r.matches("best gaming laptop deals", None, None, None));
    assert!(!r.matches("gaming ultra laptop", None, None, None));
}

#[test]
fn anchoring_is_empty_pattern() {
    let r = rule_with_pattern("x", "", Anchoring::Is);
    assert!(r.matches("", None, None, None));
    assert!(!r.matches("anything", None, None, None));
}

#[test]
fn context_required_mismatched_skips_condition() {
    let mut r = rule_with_pattern("x", "laptop", Anchoring::Contains);
    r.conditions[0].context = Some("mobile".to_string());
    let mobile_contexts = vec!["mobile".to_string()];
    let desktop_contexts = vec!["desktop".to_string()];

    // context matches → matches
    assert!(r.matches("laptop", Some(&mobile_contexts), None, None));
    // context doesn't match → condition skipped → no conditions left → false
    assert!(!r.matches("laptop", Some(&desktop_contexts), None, None));
    assert!(!r.matches("laptop", None, None, None));
}

/// Verify that a rule with multiple conditions fires when any single condition matches (OR semantics).
#[test]
fn multi_condition_any_match() {
    let mut r = bare_rule("x");
    r.conditions.push(Condition {
        pattern: Some("laptop".to_string()),
        anchoring: Some(Anchoring::Contains),
        alternatives: None,
        context: None,
        filters: None,
    });
    r.conditions.push(Condition {
        pattern: Some("computer".to_string()),
        anchoring: Some(Anchoring::Contains),
        alternatives: None,
        context: None,
        filters: None,
    });
    assert!(r.matches("laptop", None, None, None));
    assert!(r.matches("computer", None, None, None));
    assert!(!r.matches("phone", None, None, None));
}

/// Verify that pins and hides from separate matching rules both appear in the accumulated effects.
#[test]
fn hide_and_pin_from_separate_rules() {
    let mut store = RuleStore::new();
    let mut r1 = rule_with_pattern("r1", "laptop", Anchoring::Contains);
    r1.consequence.promote = Some(vec![Promote::Single {
        object_id: "item1".to_string(),
        position: 0,
    }]);
    let mut r2 = rule_with_pattern("r2", "laptop", Anchoring::Contains);
    r2.consequence.hide = Some(vec![Hide {
        object_id: "item1".to_string(),
    }]);
    store.insert(r1);
    store.insert(r2);

    let effects = store.apply_rules("laptop", None, None, None);
    assert_eq!(effects.pins.len(), 1);
    assert_eq!(effects.hidden.len(), 1);
    assert_eq!(effects.pins[0].0, "item1");
    assert_eq!(effects.hidden[0], "item1");
}

/// Verify that multiple rules can pin different documents to the same position without conflict.
#[test]
fn multiple_pins_same_position() {
    let mut store = RuleStore::new();
    let mut r1 = rule_with_pattern("r1", "laptop", Anchoring::Contains);
    r1.consequence.promote = Some(vec![Promote::Single {
        object_id: "a".to_string(),
        position: 0,
    }]);
    let mut r2 = rule_with_pattern("r2", "laptop", Anchoring::Contains);
    r2.consequence.promote = Some(vec![Promote::Single {
        object_id: "b".to_string(),
        position: 0,
    }]);
    store.insert(r1);
    store.insert(r2);

    let effects = store.apply_rules("laptop", None, None, None);
    assert_eq!(effects.pins.len(), 2);
    assert!(effects.pins.iter().all(|(_, pos)| *pos == 0));
}

#[test]
fn context_only_condition_deserializes_and_matches() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "ctx-only",
        "conditions": [{ "context": "mobile" }],
        "consequence": { "userData": { "banner": "mobile-only" } }
    }))
    .expect("context-only conditions should deserialize");

    let matching_contexts = vec!["desktop".to_string(), "mobile".to_string()];
    let non_matching_contexts = vec!["desktop".to_string()];

    assert!(rule.matches("ignored-query", Some(&matching_contexts), None, None));
    assert!(!rule.matches("ignored-query", Some(&non_matching_contexts), None, None));
    assert!(!rule.matches("ignored-query", None, None, None));
}

#[test]
fn no_context_condition_matches_regardless_of_rule_contexts() {
    let rule = rule_with_pattern("x", "laptop", Anchoring::Contains);
    let mobile_contexts = vec!["mobile".to_string()];

    assert!(rule.matches("gaming laptop", None, None, None));
    assert!(rule.matches("gaming laptop", Some(&mobile_contexts), None, None));
}

#[test]
fn condition_with_context_and_filters_requires_filters_too() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "ctx-and-filter",
        "conditions": [{ "context": "mobile", "filters": "brand:Apple" }],
        "consequence": { "userData": { "banner": "mobile-filtered" } }
    }))
    .expect("context+filters condition should deserialize");
    let matching_contexts = vec!["mobile".to_string()];

    assert!(!rule.matches("ignored-query", Some(&matching_contexts), None, None));
}

/// Verify that a `ConsequenceQuery` in object form with `remove` deserializes and applies query rewriting correctly.
#[test]
fn object_query_deserializes_and_apply_query_rewrite_applies_edits() {
    let mut store = RuleStore::new();
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "edits-rule",
        "conditions": [{ "pattern": "cheap phone", "anchoring": "is" }],
        "consequence": {
            "params": {
                "query": {
                    "remove": ["cheap"]
                }
            }
        }
    }))
    .expect("object query form should deserialize");
    store.insert(rule);

    assert_eq!(
        store.apply_query_rewrite("cheap phone", None, None, None),
        Some("phone".to_string())
    );
}

#[test]
fn consequence_query_deserializes_from_plain_string() {
    let params: ConsequenceParams = serde_json::from_value(json!({
        "query": "replacement"
    }))
    .expect("string query form should deserialize");

    assert_eq!(
        params.query,
        Some(ConsequenceQuery::Literal("replacement".to_string()))
    );
}

/// Verify that `ConsequenceQuery` deserializes from a JSON object with the `edits` array containing typed edit operations.
#[test]
fn consequence_query_deserializes_from_edits_object() {
    let params: ConsequenceParams = serde_json::from_value(json!({
        "query": {
            "edits": [
                { "type": "remove", "delete": "word" }
            ]
        }
    }))
    .expect("edits query form should deserialize");

    match params.query {
        Some(ConsequenceQuery::Edits {
            remove: None,
            edits: Some(edits),
        }) => {
            assert_eq!(edits.len(), 1);
            assert_eq!(edits[0].edit_type, EditType::Remove);
            assert_eq!(edits[0].delete, "word");
            assert_eq!(edits[0].insert, None);
        }
        other => panic!("unexpected query value: {other:?}"),
    }
}

/// Verify that `ConsequenceQuery` deserializes from a JSON object with only the `remove` array.
#[test]
fn consequence_query_deserializes_from_remove_object() {
    let params: ConsequenceParams = serde_json::from_value(json!({
        "query": {
            "remove": ["word1", "word2"]
        }
    }))
    .expect("remove query form should deserialize");

    match params.query {
        Some(ConsequenceQuery::Edits {
            remove: Some(remove),
            edits: None,
        }) => {
            assert_eq!(remove, vec!["word1".to_string(), "word2".to_string()]);
        }
        other => panic!("unexpected query value: {other:?}"),
    }
}

#[test]
fn automatic_facet_filter_deserializes_from_string() {
    let filter: AutomaticFacetFilter =
        serde_json::from_value(json!("brand")).expect("string shorthand should deserialize");
    assert_eq!(
        filter,
        AutomaticFacetFilter {
            facet: "brand".to_string(),
            disjunctive: None,
            score: None,
            negative: None,
        }
    );
}

#[test]
fn automatic_facet_filter_deserializes_from_object() {
    let filter: AutomaticFacetFilter =
        serde_json::from_value(json!({ "facet": "brand", "disjunctive": true, "score": 2 }))
            .expect("object form should deserialize");
    assert_eq!(
        filter,
        AutomaticFacetFilter {
            facet: "brand".to_string(),
            disjunctive: Some(true),
            score: Some(2),
            negative: None,
        }
    );
}

#[test]
fn automatic_facet_filter_deserializes_negative_field() {
    let filter: AutomaticFacetFilter =
        serde_json::from_value(json!({ "facet": "color", "negative": true }))
            .expect("negative field should deserialize");
    assert_eq!(
        filter,
        AutomaticFacetFilter {
            facet: "color".to_string(),
            disjunctive: None,
            score: None,
            negative: Some(true),
        }
    );
}

/// Verify that `ConsequenceParams` with every field populated survives a serialize-deserialize roundtrip unchanged.
#[test]
fn consequence_params_roundtrip_with_all_new_fields() {
    let original = ConsequenceParams {
        query: Some(ConsequenceQuery::Edits {
            remove: Some(vec!["cheap".to_string()]),
            edits: Some(vec![Edit {
                edit_type: EditType::Replace,
                delete: "tv".to_string(),
                insert: Some("television".to_string()),
            }]),
        }),
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "brand".to_string(),
            disjunctive: Some(true),
            score: Some(1),
            negative: None,
        }]),
        automatic_optional_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "color".to_string(),
            disjunctive: None,
            score: Some(2),
            negative: Some(true),
        }]),
        rendering_content: Some(json!({ "redirect": { "url": "https://example.com" } })),
        filters: Some("brand:Apple".to_string()),
        facet_filters: Some(json!([["brand:Apple", "brand:Samsung"]])),
        numeric_filters: Some(json!([["price>10"]])),
        optional_filters: Some(json!([["brand:Apple<score=2>"]])),
        tag_filters: Some(json!([["_tags:featured"]])),
        around_lat_lng: Some("40.71,-74.01".to_string()),
        around_radius: Some(json!("all")),
        hits_per_page: Some(25),
        restrict_searchable_attributes: Some(vec!["name".to_string(), "brand".to_string()]),
    };

    let serialized = serde_json::to_value(&original).expect("serialize params");
    let deserialized: ConsequenceParams =
        serde_json::from_value(serialized).expect("deserialize params");
    assert_eq!(deserialized, original);
}

#[test]
fn consequence_params_sparse_serialization_omits_none_fields() {
    let params = ConsequenceParams {
        filters: Some("brand:Apple".to_string()),
        ..Default::default()
    };

    let value = serde_json::to_value(params).expect("serialize sparse params");
    assert_eq!(value["filters"], "brand:Apple");
    assert!(value.get("query").is_none());
    assert!(value.get("facetFilters").is_none());
    assert!(value.get("automaticFacetFilters").is_none());
}

#[test]
fn condition_with_filters_only_deserializes() {
    let condition: Condition = serde_json::from_value(json!({
        "filters": "brand:Apple"
    }))
    .expect("filters-only condition should deserialize");

    assert_eq!(condition.pattern, None);
    assert_eq!(condition.anchoring, None);
    assert_eq!(condition.filters, Some("brand:Apple".to_string()));
}

/// Verify that `RuleStore::search` does not match conditions that lack a pattern field.
#[test]
fn search_skips_conditions_without_pattern() {
    let mut store = RuleStore::new();
    let mut r = bare_rule("rule-a");
    r.conditions.push(Condition {
        pattern: None,
        anchoring: None,
        alternatives: None,
        context: Some("mobile".to_string()),
        filters: None,
    });
    store.insert(r);

    let (hits, total) = store.search("mobile", 0, 10);
    assert_eq!(total, 0);
    assert!(hits.is_empty());
}

/// Verify that facet filters, numeric filters, optional filters, and tag filters accumulate across rules while `params.filters` uses first-match-wins.
#[test]
fn apply_rules_accumulates_filter_and_facet_family_fields() {
    let mut store = RuleStore::new();

    let mut r1 = rule_with_pattern("r1", "phone", Anchoring::Contains);
    r1.consequence.params = Some(ConsequenceParams {
        filters: Some("brand:Apple".to_string()),
        facet_filters: Some(json!([["brand:Apple"]])),
        numeric_filters: Some(json!([["price>100"]])),
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "brand".to_string(),
            disjunctive: Some(true),
            score: Some(2),
            negative: None,
        }]),
        ..Default::default()
    });

    let mut r2 = rule_with_pattern("r2", "phone", Anchoring::Contains);
    r2.consequence.params = Some(ConsequenceParams {
        filters: Some("category:Phone".to_string()),
        optional_filters: Some(json!([["brand:Apple<score=3>"]])),
        tag_filters: Some(json!([["_tags:featured"]])),
        automatic_optional_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "color".to_string(),
            disjunctive: None,
            score: Some(1),
            negative: Some(true),
        }]),
        ..Default::default()
    });

    store.insert(r1);
    store.insert(r2);

    let effects = store.apply_rules("smart phone", None, None, None);
    // params.filters is first-match-wins, not accumulated across rules
    assert_eq!(effects.filters, Some("brand:Apple".to_string()));
    assert_eq!(effects.facet_filters, vec![json!([["brand:Apple"]])]);
    assert_eq!(effects.numeric_filters, vec![json!([["price>100"]])]);
    assert_eq!(
        effects.optional_filters,
        vec![json!([["brand:Apple<score=3>"]])]
    );
    assert_eq!(effects.tag_filters, vec![json!([["_tags:featured"]])]);
    assert_eq!(effects.automatic_facet_filters.len(), 1);
    assert_eq!(effects.automatic_optional_facet_filters.len(), 1);
}

/// Verify that `query_edits` in the effects comes from the first matching rule with query params.
#[test]
fn apply_rules_stores_query_edits_first_match_wins() {
    let mut store = RuleStore::new();

    let mut first = rule_with_pattern("r1", "phone", Anchoring::Contains);
    first.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Edits {
            remove: Some(vec!["cheap".to_string()]),
            edits: None,
        }),
        ..Default::default()
    });

    let mut second = rule_with_pattern("r2", "phone", Anchoring::Contains);
    second.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Literal("smartphone".to_string())),
        ..Default::default()
    });

    store.insert(first);
    store.insert(second);

    let effects = store.apply_rules("cheap phone", None, None, None);
    assert_eq!(
        effects.query_edits,
        Some(ConsequenceQuery::Edits {
            remove: Some(vec!["cheap".to_string()]),
            edits: None,
        })
    );
}

/// Verify that a query edit from an earlier rule changes the working query so a downstream rule's pattern no longer matches.
#[test]
fn apply_rules_cascade_invalidates_downstream_pattern_match_after_remove_edit() {
    let mut store = RuleStore::new();

    let mut first = rule_with_pattern("a-remove-cheap", "cheap", Anchoring::Contains);
    first.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Edits {
            remove: Some(vec!["cheap".to_string()]),
            edits: None,
        }),
        ..Default::default()
    });

    let mut second = rule_with_pattern("b-triggered-by-cheap", "cheap", Anchoring::Contains);
    second.consequence.user_data = Some(json!({"banner": "should-not-apply"}));

    store.insert(first);
    store.insert(second);

    let effects = store.apply_rules("cheap laptop", None, None, None);
    assert_eq!(effects.applied_rules, vec!["a-remove-cheap".to_string()]);
    assert!(effects.user_data.is_empty());
}

#[test]
fn apply_rules_populates_rewritten_query_for_edits() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "cheap laptop", Anchoring::Is);
    rule.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Edits {
            remove: Some(vec!["cheap".to_string()]),
            edits: None,
        }),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("cheap laptop", None, None, None);
    assert_eq!(effects.rewritten_query, Some("laptop".to_string()));
}

#[test]
fn apply_rules_populates_rewritten_query_for_literal() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("r1", "tv", Anchoring::Is);
    rule.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Literal("television".to_string())),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("tv", None, None, None);
    assert_eq!(effects.rewritten_query, Some("television".to_string()));
}

#[test]
fn apply_rules_no_rewrite_when_no_match() {
    let mut store = RuleStore::new();
    let rule = rule_with_pattern("r1", "tv", Anchoring::Is);
    store.insert(rule);

    let effects = store.apply_rules("phone", None, None, None);
    assert_eq!(effects.rewritten_query, None);
}

/// Verify that a `RuleStore` loads rules with the legacy plain-string query format and applies rewrites correctly.
#[test]
fn rule_store_loads_old_schema_query_string() {
    let temp_dir = TempDir::new().expect("create tempdir");
    let rules_path = temp_dir.path().join("rules.json");

    std::fs::write(
        &rules_path,
        serde_json::to_string(&vec![json!({
            "objectID": "legacy",
            "conditions": [{ "pattern": "tv", "anchoring": "is" }],
            "consequence": { "params": { "query": "television" } }
        })])
        .expect("serialize rules"),
    )
    .expect("write rules");

    let store = RuleStore::load(&rules_path).expect("load old schema rules");
    assert_eq!(
        store.apply_query_rewrite("tv", None, None, None),
        Some("television".to_string())
    );
}

/// Verify that saving and loading a `RuleStore` preserves all `ConsequenceParams` fields including filters, geo, and pagination.
#[test]
fn rule_store_save_and_load_preserves_new_consequence_fields() {
    let temp_dir = TempDir::new().expect("create tempdir");
    let rules_path = temp_dir.path().join("rules.json");

    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("new-fields", "phone", Anchoring::Contains);
    rule.consequence.params = Some(ConsequenceParams {
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "brand".to_string(),
            disjunctive: Some(true),
            score: Some(1),
            negative: None,
        }]),
        filters: Some("brand:Apple".to_string()),
        facet_filters: Some(json!([["brand:Apple"]])),
        numeric_filters: Some(json!([["price>500"]])),
        around_lat_lng: Some("40.71,-74.01".to_string()),
        around_radius: Some(json!(5000)),
        hits_per_page: Some(12),
        restrict_searchable_attributes: Some(vec!["name".to_string()]),
        ..Default::default()
    });
    store.insert(rule);
    store.save(&rules_path).expect("save rules");

    let loaded = RuleStore::load(&rules_path).expect("load saved rules");
    let effects = loaded.apply_rules("phone", None, None, None);
    assert_eq!(effects.filters, Some("brand:Apple".to_string()));
    assert_eq!(effects.facet_filters, vec![json!([["brand:Apple"]])]);
    assert_eq!(effects.numeric_filters, vec![json!([["price>500"]])]);
    assert_eq!(effects.automatic_facet_filters.len(), 1);
    assert_eq!(effects.around_lat_lng, Some("40.71,-74.01".to_string()));
    assert_eq!(effects.around_radius, Some(json!(5000)));
    assert_eq!(effects.hits_per_page, Some(12));
    assert_eq!(
        effects.restrict_searchable_attributes,
        Some(vec!["name".to_string()])
    );
}

/// Verify that `ConsequenceParams` serializes field names as camelCase, not snake_case.
#[test]
fn consequence_params_serializes_with_camel_case_keys() {
    let params = ConsequenceParams {
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "brand".to_string(),
            disjunctive: Some(true),
            score: None,
            negative: None,
        }]),
        facet_filters: Some(json!([["brand:Apple"]])),
        hits_per_page: Some(20),
        around_lat_lng: Some("40.71,-74.01".to_string()),
        around_radius: Some(json!(5000)),
        restrict_searchable_attributes: Some(vec!["name".to_string()]),
        tag_filters: Some(json!([["_tags:sale"]])),
        numeric_filters: Some(json!([["price>10"]])),
        optional_filters: Some(json!([["brand:Apple<score=2>"]])),
        ..Default::default()
    };

    let value = serde_json::to_value(&params).expect("serialize");
    // Verify camelCase keys exist (not snake_case)
    assert!(value.get("automaticFacetFilters").is_some());
    assert!(value.get("facetFilters").is_some());
    assert!(value.get("hitsPerPage").is_some());
    assert!(value.get("aroundLatLng").is_some());
    assert!(value.get("aroundRadius").is_some());
    assert!(value.get("restrictSearchableAttributes").is_some());
    assert!(value.get("tagFilters").is_some());
    assert!(value.get("numericFilters").is_some());
    assert!(value.get("optionalFilters").is_some());
    // Verify snake_case keys do NOT exist
    assert!(value.get("automatic_facet_filters").is_none());
    assert!(value.get("facet_filters").is_none());
    assert!(value.get("hits_per_page").is_none());
    assert!(value.get("around_lat_lng").is_none());
    assert!(value.get("around_radius").is_none());
    assert!(value.get("restrict_searchable_attributes").is_none());
    assert!(value.get("tag_filters").is_none());
    assert!(value.get("numeric_filters").is_none());
    assert!(value.get("optional_filters").is_none());
}

#[test]
fn filters_only_condition_requires_active_filters() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "filter-only",
        "conditions": [{ "filters": "brand:Apple" }],
        "consequence": { "userData": { "type": "filtered" } }
    }))
    .expect("filters-only rule should deserialize");

    assert!(!rule.matches("any query", None, None, None));
    assert!(!rule.matches("", None, None, None));
}

// --- Filter condition evaluation (attribute-scoped exact match) ---

#[test]
fn filter_condition_matches_when_search_has_same_filter() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "brand-filter",
        "conditions": [{ "filters": "brand:Apple" }],
        "consequence": { "userData": { "boost": true } }
    }))
    .unwrap();
    let search_filter = crate::filter_parser::parse_filter("brand:Apple").unwrap();
    assert!(rule.matches("any", None, Some(&search_filter), None));
}

#[test]
fn filter_condition_no_match_wrong_value() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "brand-filter",
        "conditions": [{ "filters": "brand:Apple" }],
        "consequence": { "userData": { "boost": true } }
    }))
    .unwrap();
    let search_filter = crate::filter_parser::parse_filter("brand:Samsung").unwrap();
    assert!(!rule.matches("any", None, Some(&search_filter), None));
}

#[test]
fn filter_condition_no_match_no_active_filters() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "brand-filter",
        "conditions": [{ "filters": "brand:Apple" }],
        "consequence": { "userData": { "boost": true } }
    }))
    .unwrap();
    assert!(!rule.matches("any", None, None, None));
}

#[test]
fn filter_condition_with_unsupported_numeric_expression_does_not_match() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "numeric-filter",
        "conditions": [{ "filters": "price > 100" }],
        "consequence": { "userData": { "boost": true } }
    }))
    .unwrap();
    let search_filter = crate::filter_parser::parse_filter("price > 100").unwrap();
    assert!(!rule.matches("any", None, Some(&search_filter), None));
}

#[test]
fn filter_condition_matches_with_extra_attribute_in_search() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "brand-filter",
        "conditions": [{ "filters": "brand:Apple" }],
        "consequence": { "userData": { "boost": true } }
    }))
    .unwrap();
    let search_filter =
        crate::filter_parser::parse_filter("brand:Apple AND category:Phone").unwrap();
    assert!(rule.matches("any", None, Some(&search_filter), None));
}

/// Verify that a condition with both a pattern and filters requires both to match simultaneously.
#[test]
fn filter_condition_and_pattern_both_must_match() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "combo",
        "conditions": [{ "pattern": "laptop", "anchoring": "contains", "filters": "brand:Apple" }],
        "consequence": { "userData": { "combo": true } }
    }))
    .unwrap();
    let search_filter = crate::filter_parser::parse_filter("brand:Apple").unwrap();

    // Both match
    assert!(rule.matches("gaming laptop", None, Some(&search_filter), None));
    // Pattern matches, filter doesn't
    let wrong_filter = crate::filter_parser::parse_filter("brand:Samsung").unwrap();
    assert!(!rule.matches("gaming laptop", None, Some(&wrong_filter), None));
    // Filter matches, pattern doesn't
    assert!(!rule.matches("phone", None, Some(&search_filter), None));
}

// --- Alternatives matching (typo/synonym) ---

#[test]
fn alternatives_typo_matches() {
    // "laptpo" is a typo of "laptop" (5+ chars → distance 1 allowed)
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "alt-typo",
        "conditions": [{ "pattern": "laptop", "anchoring": "contains", "alternatives": true }],
        "consequence": { "userData": { "alt": true } }
    }))
    .unwrap();
    assert!(rule.matches("laptpo", None, None, None));
}

#[test]
fn alternatives_typo_exact_still_matches() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "alt-exact",
        "conditions": [{ "pattern": "laptop", "anchoring": "contains", "alternatives": true }],
        "consequence": { "userData": { "alt": true } }
    }))
    .unwrap();
    assert!(rule.matches("laptop", None, None, None));
}

#[test]
fn alternatives_false_no_typo_match() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "no-alt",
        "conditions": [{ "pattern": "laptop", "anchoring": "contains", "alternatives": false }],
        "consequence": { "userData": { "alt": false } }
    }))
    .unwrap();
    assert!(!rule.matches("laptpo", None, None, None));
}

#[test]
fn alternatives_none_no_typo_match() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "none-alt",
        "conditions": [{ "pattern": "laptop", "anchoring": "contains" }],
        "consequence": { "userData": {} }
    }))
    .unwrap();
    assert!(!rule.matches("laptpo", None, None, None));
}

#[test]
fn alternatives_synonym_matches() {
    let mut synonyms = SynonymStore::new();
    synonyms.insert(crate::index::synonyms::Synonym::Regular {
        object_id: "tv-syn".to_string(),
        synonyms: vec!["tv".to_string(), "television".to_string()],
    });
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "alt-syn",
        "conditions": [{ "pattern": "tv", "anchoring": "contains", "alternatives": true }],
        "consequence": { "userData": { "syn": true } }
    }))
    .unwrap();
    assert!(rule.matches("television", None, None, Some(&synonyms)));
}

#[test]
fn alternatives_synonym_no_match_without_flag() {
    let mut synonyms = SynonymStore::new();
    synonyms.insert(crate::index::synonyms::Synonym::Regular {
        object_id: "tv-syn".to_string(),
        synonyms: vec!["tv".to_string(), "television".to_string()],
    });
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "no-alt-syn",
        "conditions": [{ "pattern": "tv", "anchoring": "contains", "alternatives": false }],
        "consequence": { "userData": {} }
    }))
    .unwrap();
    assert!(!rule.matches("television", None, None, Some(&synonyms)));
}

#[test]
fn alternatives_short_word_no_typo() {
    // "tv" is <5 chars → distance 0 required → "tb" should NOT match
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "short-alt",
        "conditions": [{ "pattern": "tv", "anchoring": "is", "alternatives": true }],
        "consequence": { "userData": {} }
    }))
    .unwrap();
    assert!(!rule.matches("tb", None, None, None));
}

// --- Rule processing order (objectID lexicographic) ---

/// Verify that rules are processed in lexicographic objectID order so first-match-wins is deterministic regardless of insertion order.
#[test]
fn rules_sorted_by_object_id_for_first_match_wins() {
    let mut store = RuleStore::new();

    // Insert "b-rule" first, then "a-rule" — "a-rule" should win
    let mut b = rule_with_pattern("b-rule", "phone", Anchoring::Contains);
    b.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Literal("b-rewrite".to_string())),
        hits_per_page: Some(50),
        ..Default::default()
    });

    let mut a = rule_with_pattern("a-rule", "phone", Anchoring::Contains);
    a.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Literal("a-rewrite".to_string())),
        hits_per_page: Some(10),
        ..Default::default()
    });

    store.insert(b);
    store.insert(a);

    let effects = store.apply_rules("phone", None, None, None);
    // "a-rule" sorts before "b-rule" → its query_edits win (first-match-wins)
    assert_eq!(
        effects.query_edits,
        Some(ConsequenceQuery::Literal("a-rewrite".to_string()))
    );
    assert_eq!(effects.hits_per_page, Some(10));
}

/// Verify that query rewrite uses objectID lexicographic order to determine the winning rule.
#[test]
fn query_rewrite_uses_objectid_order() {
    let mut store = RuleStore::new();

    let mut b = rule_with_pattern("z-rule", "phone", Anchoring::Contains);
    b.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Literal("z-rewrite".to_string())),
        ..Default::default()
    });

    let mut a = rule_with_pattern("a-rule", "phone", Anchoring::Contains);
    a.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Literal("a-rewrite".to_string())),
        ..Default::default()
    });

    store.insert(b);
    store.insert(a);

    assert_eq!(
        store.apply_query_rewrite("phone", None, None, None),
        Some("a-rewrite".to_string())
    );
}

// --- Conditionless rule restrictions ---

#[test]
fn conditionless_rule_hide_applies() {
    let mut store = RuleStore::new();
    let mut rule = bare_rule("conditionless-hide");
    rule.consequence.hide = Some(vec![Hide {
        object_id: "bad-doc".to_string(),
    }]);
    store.insert(rule);

    let effects = store.apply_rules("any query", None, None, None);
    assert_eq!(effects.hidden, vec!["bad-doc"]);
}

#[test]
fn conditionless_rule_params_filters_applies() {
    let mut store = RuleStore::new();
    let mut rule = bare_rule("conditionless-filter");
    rule.consequence.params = Some(ConsequenceParams {
        filters: Some("category:Sale".to_string()),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("any query", None, None, None);
    assert_eq!(effects.filters, Some("category:Sale".to_string()));
}

/// Verify that promotes are ignored for conditionless rules per the Algolia spec.
#[test]
fn conditionless_rule_promote_ignored() {
    let mut store = RuleStore::new();
    let mut rule = bare_rule("conditionless-promote");
    rule.consequence.promote = Some(vec![Promote::Single {
        object_id: "pinned".to_string(),
        position: 0,
    }]);
    store.insert(rule);

    let effects = store.apply_rules("any query", None, None, None);
    // promote must be ignored for conditionless rules
    assert!(effects.pins.is_empty());
    assert!(effects
        .applied_rules
        .contains(&"conditionless-promote".to_string()));
}

/// Verify that query edits are ignored for conditionless rules and do not appear in the effects.
#[test]
fn conditionless_rule_query_edits_ignored() {
    let mut store = RuleStore::new();
    let mut rule = bare_rule("conditionless-edits");
    rule.consequence.params = Some(ConsequenceParams {
        query: Some(ConsequenceQuery::Edits {
            remove: Some(vec!["cheap".to_string()]),
            edits: None,
        }),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("cheap laptop", None, None, None);
    // query_edits must be ignored for conditionless rules
    assert!(effects.query_edits.is_none());
}

/// Verify that automatic facet filters are ignored for conditionless rules per the Algolia spec.
#[test]
fn conditionless_rule_automatic_facet_filters_ignored() {
    let mut store = RuleStore::new();
    let mut rule = bare_rule("conditionless-aff");
    rule.consequence.params = Some(ConsequenceParams {
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "brand".to_string(),
            disjunctive: None,
            score: None,
            negative: None,
        }]),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("query", None, None, None);
    assert!(effects.automatic_facet_filters.is_empty());
}

/// Verify that a conditionless rule and a conditional rule both contribute their applicable effects to the same query.
#[test]
fn conditionless_plus_conditional_both_apply() {
    let mut store = RuleStore::new();

    // Conditionless: hide + user_data
    let mut conditionless = bare_rule("a-conditionless");
    conditionless.consequence.hide = Some(vec![Hide {
        object_id: "hidden".to_string(),
    }]);
    conditionless.consequence.user_data = Some(json!({"banner": "always"}));

    // Conditional: promote
    let mut conditional = rule_with_pattern("b-conditional", "laptop", Anchoring::Contains);
    conditional.consequence.promote = Some(vec![Promote::Single {
        object_id: "pinned".to_string(),
        position: 0,
    }]);

    store.insert(conditionless);
    store.insert(conditional);

    let effects = store.apply_rules("laptop", None, None, None);
    // Conditionless hide applies
    assert_eq!(effects.hidden, vec!["hidden"]);
    // Conditionless user_data applies
    assert_eq!(effects.user_data, vec![json!({"banner": "always"})]);
    // Conditional promote applies
    assert_eq!(effects.pins, vec![("pinned".to_string(), 0)]);
    // Both rules applied
    assert_eq!(effects.applied_rules.len(), 2);
}

/// Verify that scalar params (hits_per_page, around_lat_lng, around_radius, restrict_searchable_attributes) use first-match-wins across rules.
#[test]
fn scalar_params_first_match_wins() {
    let mut store = RuleStore::new();

    let mut r1 = rule_with_pattern("r1", "phone", Anchoring::Contains);
    r1.consequence.params = Some(ConsequenceParams {
        hits_per_page: Some(10),
        around_lat_lng: Some("40.71,-74.01".to_string()),
        around_radius: Some(json!(1000)),
        restrict_searchable_attributes: Some(vec!["name".to_string()]),
        ..Default::default()
    });

    let mut r2 = rule_with_pattern("r2", "phone", Anchoring::Contains);
    r2.consequence.params = Some(ConsequenceParams {
        hits_per_page: Some(50),
        around_lat_lng: Some("0.0,0.0".to_string()),
        around_radius: Some(json!("all")),
        restrict_searchable_attributes: Some(vec!["brand".to_string()]),
        ..Default::default()
    });

    store.insert(r1);
    store.insert(r2);

    let effects = store.apply_rules("smart phone", None, None, None);
    // First matching rule's values win for scalar params
    assert_eq!(effects.hits_per_page, Some(10));
    assert_eq!(effects.around_lat_lng, Some("40.71,-74.01".to_string()));
    assert_eq!(effects.around_radius, Some(json!(1000)));
    assert_eq!(
        effects.restrict_searchable_attributes,
        Some(vec!["name".to_string()])
    );
}

/// Verify that `params.filters` uses first-match-wins semantics across multiple matching rules.
#[test]
fn params_filters_first_match_wins() {
    let mut store = RuleStore::new();

    let mut first = rule_with_pattern("a-first", "phone", Anchoring::Contains);
    first.consequence.params = Some(ConsequenceParams {
        filters: Some("brand:Apple".to_string()),
        ..Default::default()
    });

    let mut second = rule_with_pattern("b-second", "phone", Anchoring::Contains);
    second.consequence.params = Some(ConsequenceParams {
        filters: Some("brand:Samsung".to_string()),
        ..Default::default()
    });

    store.insert(first);
    store.insert(second);

    let effects = store.apply_rules("smart phone", None, None, None);
    assert_eq!(effects.filters, Some("brand:Apple".to_string()));
}

// --- Multi-condition OR logic ---

/// Verify that a rule fires when only its pattern condition matches among pattern, context, and filter conditions.
#[test]
fn multi_condition_three_types_or_logic_pattern_fires() {
    // Rule with 3 condition types: pattern-only, context-only, filters-only.
    // ANY one matching should fire the rule.
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "multi-3",
        "conditions": [
            { "pattern": "laptop", "anchoring": "contains" },
            { "context": "mobile" },
            { "filters": "brand:Apple" }
        ],
        "consequence": { "userData": { "multi": true } }
    }))
    .unwrap();

    // Pattern matches, no context, no filters → should fire
    assert!(rule.matches("laptop", None, None, None));
}

/// Verify that a rule fires when only its context condition matches among pattern, context, and filter conditions.
#[test]
fn multi_condition_three_types_or_logic_context_fires() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "multi-3",
        "conditions": [
            { "pattern": "laptop", "anchoring": "contains" },
            { "context": "mobile" },
            { "filters": "brand:Apple" }
        ],
        "consequence": { "userData": { "multi": true } }
    }))
    .unwrap();

    let mobile_ctx = vec!["mobile".to_string()];
    // Context matches, query doesn't match pattern → should fire via context condition
    assert!(rule.matches("phone", Some(&mobile_ctx), None, None));
}

/// Verify that a rule fires when only its filter condition matches among pattern, context, and filter conditions.
#[test]
fn multi_condition_three_types_or_logic_filter_fires() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "multi-3",
        "conditions": [
            { "pattern": "laptop", "anchoring": "contains" },
            { "context": "mobile" },
            { "filters": "brand:Apple" }
        ],
        "consequence": { "userData": { "multi": true } }
    }))
    .unwrap();

    let search_filter = crate::filter_parser::parse_filter("brand:Apple").unwrap();
    // Filter matches, query doesn't match pattern, no matching context → should fire
    assert!(rule.matches("phone", None, Some(&search_filter), None));
}

/// Verify that a rule does not fire when none of its pattern, context, or filter conditions match.
#[test]
fn multi_condition_three_types_none_match() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "multi-3",
        "conditions": [
            { "pattern": "laptop", "anchoring": "contains" },
            { "context": "mobile" },
            { "filters": "brand:Apple" }
        ],
        "consequence": { "userData": { "multi": true } }
    }))
    .unwrap();

    let desktop_ctx = vec!["desktop".to_string()];
    let wrong_filter = crate::filter_parser::parse_filter("brand:Samsung").unwrap();
    // Nothing matches → should NOT fire
    assert!(!rule.matches("phone", Some(&desktop_ctx), Some(&wrong_filter), None));
}

#[test]
fn multi_condition_two_patterns_matches_either() {
    let rule: Rule = serde_json::from_value(json!({
        "objectID": "dual-pattern",
        "conditions": [
            { "pattern": "laptop", "anchoring": "contains" },
            { "pattern": "notebook", "anchoring": "contains" }
        ],
        "consequence": { "userData": { "matched": true } }
    }))
    .unwrap();

    assert!(rule.matches("gaming laptop", None, None, None));
    assert!(rule.matches("notebook computer", None, None, None));
    assert!(!rule.matches("phone", None, None, None));
}

// --- {facet:attrName} placeholder pattern matching ---

#[test]
fn facet_placeholder_matches_any_single_word() {
    let rule = rule_with_pattern("facet-rule", "{facet:genre}", Anchoring::Contains);
    assert!(rule.matches("comedy", None, None, None));
    assert!(rule.matches("action movies", None, None, None));
    assert!(rule.matches("best comedy films", None, None, None));
}

/// Verify that `{facet:attrName}` placeholders respect all anchoring modes: Is, StartsWith, EndsWith.
#[test]
fn facet_placeholder_is_anchoring() {
    // Single-token wildcard with Is: must be exactly one word
    let rule_is = rule_with_pattern("facet-is", "{facet:genre}", Anchoring::Is);
    assert!(rule_is.matches("comedy", None, None, None));
    assert!(!rule_is.matches("comedy movies", None, None, None));

    // Multi-token pattern with StartsWith: "{facet:genre} movies" starts with wildcard + "movies"
    let rule_starts = rule_with_pattern("facet-sw", "{facet:genre} movies", Anchoring::StartsWith);
    assert!(rule_starts.matches("comedy movies tonight", None, None, None));
    assert!(!rule_starts.matches("best comedy", None, None, None));

    // Multi-token pattern with EndsWith: "best {facet:genre}" ends with "best" + wildcard
    let rule_ends = rule_with_pattern("facet-ew", "best {facet:genre}", Anchoring::EndsWith);
    assert!(rule_ends.matches("the best comedy", None, None, None));
    assert!(!rule_ends.matches("comedy movies tonight", None, None, None));
}

#[test]
fn facet_placeholder_mixed_with_literal_tokens() {
    let rule = rule_with_pattern("mixed", "best {facet:genre} movies", Anchoring::Contains);
    assert!(rule.matches("best comedy movies", None, None, None));
    assert!(rule.matches("the best action movies ever", None, None, None));
    assert!(!rule.matches("best movies", None, None, None)); // missing the facet word
}

/// Verify that `{facet:attrName}` pattern placeholders capture the matched query word into `facet_captures`.
#[test]
fn facet_placeholder_captures_matched_word() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("genre-filter", "{facet:genre}", Anchoring::Contains);
    rule.consequence.params = Some(ConsequenceParams {
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "genre".to_string(),
            disjunctive: None,
            score: None,
            negative: None,
        }]),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("comedy", None, None, None);
    assert_eq!(effects.applied_rules, vec!["genre-filter"]);
    // The captured word should be stored so we can generate genre:comedy filter
    assert_eq!(
        effects.facet_captures.get("genre"),
        Some(&"comedy".to_string())
    );
}

/// Verify that `{facet:attrName}` captures the correct word when surrounded by literal tokens in a multi-word query.
#[test]
fn facet_placeholder_captures_correct_word_in_multi_word_query() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern(
        "genre-filter",
        "best {facet:genre} movies",
        Anchoring::Contains,
    );
    rule.consequence.params = Some(ConsequenceParams {
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "genre".to_string(),
            disjunctive: None,
            score: None,
            negative: None,
        }]),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("best action movies", None, None, None);
    assert_eq!(effects.applied_rules, vec!["genre-filter"]);
    assert_eq!(
        effects.facet_captures.get("genre"),
        Some(&"action".to_string())
    );
}

/// Verify that an automatic facet filter with a `{facet:attrName}` capture generates a mandatory filter expression.
#[test]
fn automatic_facet_filter_generates_mandatory_filter() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("genre-filter", "{facet:genre}", Anchoring::Contains);
    rule.consequence.params = Some(ConsequenceParams {
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "genre".to_string(),
            disjunctive: None,
            score: None,
            negative: None,
        }]),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("comedy", None, None, None);
    // The generated mandatory facet filters should be stored for downstream application
    assert_eq!(effects.generated_facet_filters.len(), 1);
    assert_eq!(
        effects.generated_facet_filters[0],
        GeneratedFacetFilter {
            expression: "genre:comedy".to_string(),
            disjunctive: false,
        }
    );
}

/// Verify that an automatic optional facet filter generates an optional filter expression with the configured score.
#[test]
fn automatic_optional_facet_filter_generates_optional_filter_with_score() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("genre-boost", "{facet:genre}", Anchoring::Contains);
    rule.consequence.params = Some(ConsequenceParams {
        automatic_optional_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "genre".to_string(),
            disjunctive: None,
            score: Some(5),
            negative: None,
        }]),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("comedy", None, None, None);
    assert_eq!(effects.generated_optional_facet_filters.len(), 1);
    assert_eq!(
        effects.generated_optional_facet_filters[0],
        ("genre".to_string(), "comedy".to_string(), 5)
    );
}

/// Verify that an automatic facet filter with `negative: true` generates a `NOT attr:value` filter expression.
#[test]
fn automatic_facet_filter_negative_generates_negated_filter() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("neg-filter", "{facet:genre}", Anchoring::Contains);
    rule.consequence.params = Some(ConsequenceParams {
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "genre".to_string(),
            disjunctive: None,
            score: None,
            negative: Some(true),
        }]),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("comedy", None, None, None);
    assert_eq!(effects.generated_facet_filters.len(), 1);
    assert_eq!(
        effects.generated_facet_filters[0],
        GeneratedFacetFilter {
            expression: "NOT genre:comedy".to_string(),
            disjunctive: false,
        }
    );
}

/// Verify that generated automatic facet filters coexist with explicit `params.filters` in the rule effects.
#[test]
fn automatic_facet_filters_combine_with_request_filters() {
    // Tested in integration test — generated filters AND-merge with request filters
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("brand-filter", "{facet:brand}", Anchoring::Contains);
    rule.consequence.params = Some(ConsequenceParams {
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "brand".to_string(),
            disjunctive: None,
            score: None,
            negative: None,
        }]),
        filters: Some("category:Electronics".to_string()),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("apple", None, None, None);
    assert_eq!(
        effects.generated_facet_filters,
        vec![GeneratedFacetFilter {
            expression: "brand:apple".to_string(),
            disjunctive: false,
        }]
    );
    assert_eq!(effects.filters, Some("category:Electronics".to_string()));
}

/// Verify that disjunctive automatic facet filters from multiple rules carry the disjunctive flag for OR grouping.
#[test]
fn automatic_facet_filter_disjunctive_groups_same_facet_with_or() {
    // Two rules both capture "genre" with disjunctive: true.
    // Generated filters should carry the disjunctive flag so the merge
    // layer groups them into an OR clause instead of AND.
    let mut store = RuleStore::new();

    let mut rule1 = rule_with_pattern("genre-rule-1", "{facet:genre}", Anchoring::StartsWith);
    rule1.consequence.params = Some(ConsequenceParams {
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "genre".to_string(),
            disjunctive: Some(true),
            score: None,
            negative: None,
        }]),
        ..Default::default()
    });
    store.insert(rule1);

    let mut rule2 = rule_with_pattern("genre-rule-2", "{facet:genre}", Anchoring::EndsWith);
    rule2.consequence.params = Some(ConsequenceParams {
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "genre".to_string(),
            disjunctive: Some(true),
            score: None,
            negative: None,
        }]),
        ..Default::default()
    });
    store.insert(rule2);

    // Query "comedy drama" — rule1 StartsWith captures "comedy", rule2 EndsWith captures "drama"
    let effects = store.apply_rules("comedy drama", None, None, None);
    assert_eq!(effects.generated_facet_filters.len(), 2);
    assert!(effects
        .generated_facet_filters
        .iter()
        .all(|f| f.disjunctive));
    let exprs: Vec<&str> = effects
        .generated_facet_filters
        .iter()
        .map(|f| f.expression.as_str())
        .collect();
    assert!(exprs.contains(&"genre:comedy"));
    assert!(exprs.contains(&"genre:drama"));
}

/// Verify that an automatic facet filter without the disjunctive flag generates a conjunctive (AND) filter.
#[test]
fn automatic_facet_filter_non_disjunctive_stays_conjunctive() {
    let mut store = RuleStore::new();
    let mut rule = rule_with_pattern("genre-filter", "{facet:genre}", Anchoring::Contains);
    rule.consequence.params = Some(ConsequenceParams {
        automatic_facet_filters: Some(vec![AutomaticFacetFilter {
            facet: "genre".to_string(),
            disjunctive: None,
            score: None,
            negative: None,
        }]),
        ..Default::default()
    });
    store.insert(rule);

    let effects = store.apply_rules("comedy", None, None, None);
    assert_eq!(effects.generated_facet_filters.len(), 1);
    assert!(!effects.generated_facet_filters[0].disjunctive);
}

// --- Rule-matching utility coverage (s40 test-audit, pre-split) ---

#[test]
fn tokenize_for_rule_matching_splits_and_lowercases() {
    let tokens = tokenize_for_rule_matching("Red Fox-Jumps! 42");
    assert_eq!(tokens, vec!["red", "fox", "jumps", "42"]);
}

#[test]
fn tokenize_for_rule_matching_handles_unicode_and_empty() {
    assert!(tokenize_for_rule_matching("").is_empty());
    assert!(tokenize_for_rule_matching("  ---  ").is_empty());
    let tokens = tokenize_for_rule_matching("café résumé");
    assert_eq!(tokens, vec!["café", "résumé"]);
}

#[test]
fn calculate_typo_distance_thresholds() {
    assert_eq!(calculate_typo_distance("hi"), 0, "<5 chars → exact only");
    assert_eq!(calculate_typo_distance("four"), 0, "4 chars → exact only");
    assert_eq!(calculate_typo_distance("fiver"), 1, "5 chars → 1 typo");
    assert_eq!(calculate_typo_distance("sixchars"), 1, "8 chars → 1 typo");
    assert_eq!(calculate_typo_distance("ninechars"), 2, "9 chars → 2 typos");
}

#[test]
fn fuzzy_word_match_respects_distance_thresholds() {
    assert!(fuzzy_word_match("hello", "hello"), "exact match");
    assert!(fuzzy_word_match("Hello", "hello"), "case insensitive");
    assert!(fuzzy_word_match("hello", "hallo"), "1 edit on 5-char word");
    assert!(!fuzzy_word_match("cat", "car"), "no fuzzy on <5 chars");
    assert!(
        fuzzy_word_match("searching", "seerching"),
        "1 edit on 9-char word"
    );
}

#[test]
fn parse_pattern_tokens_literal_and_facet_placeholders() {
    let tokens = parse_pattern_tokens("buy {facet:brand} shoes");
    assert_eq!(tokens.len(), 3);
    assert_eq!(tokens[0], PatternToken::Literal("buy".to_string()));
    assert_eq!(tokens[1], PatternToken::FacetCapture("brand".to_string()));
    assert_eq!(tokens[2], PatternToken::Literal("shoes".to_string()));
}

#[test]
fn parse_pattern_tokens_no_placeholders() {
    let tokens = parse_pattern_tokens("red shoes");
    assert_eq!(
        tokens,
        vec![
            PatternToken::Literal("red".to_string()),
            PatternToken::Literal("shoes".to_string()),
        ]
    );
}
#[test]
fn match_pattern_tokens_with_placeholders_anchoring_modes() {
    let query = vec!["buy".into(), "nike".into(), "shoes".into(), "now".into()];
    let pattern = vec![
        PatternToken::Literal("buy".to_string()),
        PatternToken::FacetCapture("brand".to_string()),
        PatternToken::Literal("shoes".to_string()),
    ];

    assert!(
        match_pattern_tokens_with_placeholders(&query, &pattern, &Anchoring::StartsWith),
        "pattern matches start of query"
    );
    assert!(
        match_pattern_tokens_with_placeholders(&query, &pattern, &Anchoring::Contains),
        "pattern found anywhere in query"
    );
    assert!(
        !match_pattern_tokens_with_placeholders(&query, &pattern, &Anchoring::EndsWith),
        "pattern does not match end of query"
    );
    assert!(
        !match_pattern_tokens_with_placeholders(&query, &pattern, &Anchoring::Is),
        "query has extra tokens so Is fails"
    );
}

#[test]
fn extract_facet_captures_returns_captured_values() {
    let captures = extract_facet_captures("buy nike shoes", "buy {facet:brand} shoes", None);
    assert_eq!(captures.get("brand").map(|s| s.as_str()), Some("nike"));
}

#[test]
fn extract_facet_captures_no_placeholder_returns_empty() {
    let captures = extract_facet_captures("buy shoes", "buy shoes", None);
    assert!(captures.is_empty());
}
