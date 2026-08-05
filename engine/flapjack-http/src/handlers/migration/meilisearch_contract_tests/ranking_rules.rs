use super::*;

// Custom 1.50 split ranking orders are not generally representable by one
// Flapjack `attribute` criterion. If another rule separates `wordPosition` from
// `attributeRank`, accepting the payload would silently advance one source rule
// ahead of the separator. The translator must fail closed instead.
#[test]
fn meilisearch_custom_split_ranking_rules_fail_closed_when_order_is_ambiguous() {
    for (rules, expected_path) in [
        (
            vec![
                "words",
                "wordPosition",
                "typo",
                "attributeRank",
                "exactness",
            ],
            "$.rankingRules[1]",
        ),
        (
            vec!["words", "wordPosition", "typo", "exactness"],
            "$.rankingRules[1]",
        ),
        (
            vec!["words", "attribute", "wordPosition", "exactness"],
            "$.rankingRules[2]",
        ),
    ] {
        let mut failures = Vec::new();
        let mut warnings = Vec::new();
        assert!(
            translate_settings_for_provider(
                &json!({ "rankingRules": rules }),
                SettingsSourceProvider::Meilisearch,
                &mut failures,
                &mut warnings,
            )
            .is_none(),
            "ambiguous custom ranking rules {rules:?} must fail closed"
        );
        assert_eq!(failures.len(), 1);
        let failure = format!("{:?}", failures[0]);
        assert!(failure.contains("MalformedSettingsPayload"));
        assert!(
            failure.contains(expected_path),
            "failure {failure} must name {expected_path}"
        );
        assert!(warnings.is_empty());
    }
}

// `attribute` (pre-1.50) and `attributeRank` (1.50+) are two source names for
// the same Algolia `attribute` criterion. A source list carrying both would
// emit that criterion twice, and Algolia rejects a duplicated ranking
// criterion, so the translator must fail closed at the second family member
// rather than silently dropping one source rule. The reported index must track
// the second occurrence, not a fixed position.
#[test]
fn meilisearch_attribute_alias_pair_fails_closed_as_duplicate_criterion() {
    for (rules, expected_path) in [
        (
            vec!["words", "attribute", "attributeRank", "exactness"],
            "$.rankingRules[2]",
        ),
        (
            vec!["words", "attributeRank", "attribute", "exactness"],
            "$.rankingRules[2]",
        ),
        (
            vec!["words", "attributeRank", "typo", "attribute", "exactness"],
            "$.rankingRules[3]",
        ),
    ] {
        let mut failures = Vec::new();
        let mut warnings = Vec::new();
        assert!(
            translate_settings_for_provider(
                &json!({ "rankingRules": rules }),
                SettingsSourceProvider::Meilisearch,
                &mut failures,
                &mut warnings,
            )
            .is_none(),
            "duplicated attribute criterion {rules:?} must fail closed"
        );
        assert_eq!(failures.len(), 1);
        let failure = format!("{:?}", failures[0]);
        assert!(failure.contains("MalformedSettingsPayload"));
        assert!(
            failure.contains(expected_path),
            "failure {failure} must name {expected_path}"
        );
        assert!(warnings.is_empty());
    }
}

// The duplicate guard must not reject a single alias: each vocabulary on its
// own still translates to exactly one Algolia `attribute` criterion, in the
// source's position.
#[test]
fn meilisearch_single_attribute_alias_translates_to_one_criterion() {
    for alias in ["attribute", "attributeRank"] {
        let mut failures = Vec::new();
        let mut warnings = Vec::new();
        let translated = translate_settings_for_provider(
            &json!({ "rankingRules": ["words", "typo", alias, "exactness"] }),
            SettingsSourceProvider::Meilisearch,
            &mut failures,
            &mut warnings,
        )
        .expect("a single attribute alias must translate");

        assert!(failures.is_empty(), "unexpected failures: {failures:?}");
        assert_eq!(
            translated.ranking,
            Some(vec![
                "words".to_string(),
                "typo".to_string(),
                "attribute".to_string(),
                "exact".to_string(),
            ]),
            "alias {alias} must map to one attribute criterion"
        );
    }
}
