use super::*;
use flapjack::index::settings::DistinctValue;
use flapjack::types::FieldValue;
use serde_json::json;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::Path;

const STAGE1_MATRIX_EXPECTED_DENOMINATOR: usize = 80;
type ReportEntryContract = (
    ReportSeverity,
    ReportCode,
    ReportResource,
    Option<usize>,
    Option<usize>,
    String,
);

fn spool_payload(
    settings: serde_json::Value,
    document_pages: Vec<Vec<serde_json::Value>>,
    rule_pages: Vec<Vec<serde_json::Value>>,
    synonym_pages: Vec<Vec<serde_json::Value>>,
) -> SpoolTranslationInput {
    SpoolTranslationInput {
        settings,
        document_pages,
        rule_pages,
        synonym_pages,
    }
}

fn minimal_valid_settings() -> serde_json::Value {
    json!({"searchableAttributes": ["title"]})
}

fn minimal_rule(object_id: &str) -> serde_json::Value {
    json!({
        "objectID": object_id,
        "conditions": [{"pattern": "sale", "anchoring": "contains"}],
        "consequence": {
            "promote": [{"objectID": "doc-1", "position": 1}],
            "params": {
                "query": {"remove": ["cheap"], "edits": [{"type": "remove", "delete": "cheap"}]},
                "automaticFacetFilters": [{"facet": "brand", "score": 4}]
            }
        },
        "enabled": true
    })
}

fn minimal_synonym(object_id: &str) -> serde_json::Value {
    json!({
        "objectID": object_id,
        "type": "synonym",
        "synonyms": ["sneaker", "trainer"]
    })
}

fn translated(payload: SpoolTranslationInput) -> TranslatedSpoolPayload {
    match translate_spool_payload(payload) {
        TranslationOutcome::Translated(translated) => *translated,
        TranslationOutcome::Rejected(report) => {
            panic!("expected translated payload, got report {report:#?}")
        }
    }
}

fn rejected(payload: SpoolTranslationInput) -> TranslationReport {
    match translate_spool_payload(payload) {
        TranslationOutcome::Translated(translated) => {
            panic!("expected rejection, got translated payload {translated:#?}")
        }
        TranslationOutcome::Rejected(report) => report,
    }
}

fn hard_codes(report: &TranslationReport) -> Vec<ReportCode> {
    report
        .entries
        .iter()
        .filter(|entry| entry.severity == ReportSeverity::HardRejection)
        .map(|entry| entry.code)
        .collect()
}

fn entry_for_code(report: &TranslationReport, code: ReportCode) -> &TranslationReportEntry {
    report
        .entries
        .iter()
        .find(|entry| entry.code == code)
        .expect("report should contain code")
}

fn assert_single_hard_code(payload: SpoolTranslationInput, code: ReportCode) {
    let report = rejected(payload);
    assert_eq!(hard_codes(&report), vec![code]);
    assert_eq!(report.summary.hard_rejections, 1);
    assert!(report.report_digest.is_some());
}

fn entries_for_code(report: &TranslationReport, code: ReportCode) -> Vec<&TranslationReportEntry> {
    report
        .entries
        .iter()
        .filter(|entry| entry.code == code)
        .collect()
}

#[test]
fn matrix_denominator_is_explicit_stage3_oracle() {
    let denominator = stage1_matrix().len();

    println!("DENOMINATOR={denominator}");
    assert_ne!(denominator, 0, "VACUOUS matrix denominator");
    assert_eq!(denominator, STAGE1_MATRIX_EXPECTED_DENOMINATOR);
}

#[test]
fn matcher_inventory_matches_independent_stage1_list() {
    let actual: Vec<_> = stage1_matrix()
        .iter()
        .map(|row| (row.resource, row.matcher))
        .collect();

    let mut expected = Vec::new();
    expected.extend(
        [
            "attributesForFaceting",
            "searchableAttributes",
            "ranking",
            "customRanking",
            "attributesToRetrieve",
            "unretrievableAttributes",
            "attributesToHighlight",
            "attributesToSnippet",
            "paginationLimitedTo",
            "attributeForDistinct",
            "distinct",
            "highlightPreTag",
            "highlightPostTag",
            "hitsPerPage",
            "minWordSizefor1Typo",
            "minWordSizefor2Typos",
            "maxValuesPerFacet",
            "exactOnSingleWordQuery",
            "removeWordsIfNoResults",
            "separatorsToIndex",
            "alternativesAsExact",
            "optionalWords",
            "synonyms",
            "version",
            "removeStopWords",
            "ignorePlurals",
            "queryLanguages",
            "queryType",
            "embedders",
            "mode",
            "semanticSearch",
            "enablePersonalization",
            "renderingContent",
            "userData",
            "enableRules",
            "advancedSyntaxFeatures",
            "sortFacetValuesBy",
            "snippetEllipsisText",
            "restrictHighlightAndSnippetArrays",
            "minProximity",
            "disableExactOnAttributes",
            "replaceSynonymsInHighlight",
            "attributeCriteriaComputedByMinProximity",
            "enableReRanking",
            "disableTypoToleranceOnWords",
            "disableTypoToleranceOnAttributes",
            "replicas",
            "numericAttributesForFiltering",
            "numericAttributesToIndex",
            "allowCompressionOfIntegerArray",
            "relevancyStrictness",
        ]
        .map(|field| (ResourceKind::Settings, SourceMatcher::Field(field))),
    );
    expected.push((ResourceKind::Settings, SourceMatcher::UnknownClosedSchema));
    expected.extend([
        (ResourceKind::Document, SourceMatcher::Field("objectID")),
        (ResourceKind::Document, SourceMatcher::DocumentAttribute),
    ]);
    expected.extend(
        [
            "objectID",
            "conditions",
            "consequence",
            "description",
            "enabled",
            "validity",
        ]
        .map(|field| (ResourceKind::Rule, SourceMatcher::Field(field))),
    );
    expected.extend(
        [
            RuleSchemaMatcher::Condition,
            RuleSchemaMatcher::Consequence,
            RuleSchemaMatcher::ConsequenceParams,
            RuleSchemaMatcher::PromoteSingle,
            RuleSchemaMatcher::PromoteMultiple,
            RuleSchemaMatcher::Hide,
            RuleSchemaMatcher::TimeRange,
            RuleSchemaMatcher::AutomaticFacetFilter,
            RuleSchemaMatcher::ConsequenceQueryLiteral,
            RuleSchemaMatcher::ConsequenceQueryEdits,
            RuleSchemaMatcher::QueryEdit,
        ]
        .map(|matcher| (ResourceKind::Rule, SourceMatcher::RuleSchema(matcher))),
    );
    expected.push((ResourceKind::Rule, SourceMatcher::UnknownClosedSchema));
    expected.extend(
        ["objectID", "type"].map(|field| (ResourceKind::Synonym, SourceMatcher::Field(field))),
    );
    expected.extend(
        [
            SynonymSchemaMatcher::Regular,
            SynonymSchemaMatcher::OneWay,
            SynonymSchemaMatcher::AltCorrection1,
            SynonymSchemaMatcher::AltCorrection2,
            SynonymSchemaMatcher::Placeholder,
        ]
        .map(|matcher| (ResourceKind::Synonym, SourceMatcher::SynonymSchema(matcher))),
    );
    expected.push((ResourceKind::Synonym, SourceMatcher::UnknownClosedSchema));

    assert_eq!(actual, expected);
    assert_eq!(actual.len(), STAGE1_MATRIX_EXPECTED_DENOMINATOR);
}

#[test]
fn document_reserved_fields_take_precedence_over_attribute_catch_all() {
    let row = resolve_source_field(ResourceKind::Document, "objectID");

    assert_eq!(row.matcher, SourceMatcher::Field("objectID"));
    assert_eq!(row.disposition, Disposition::Exact);
}

#[test]
fn warning_rows_are_unique_when_present() {
    let mut seen = HashSet::new();

    for row in stage1_matrix()
        .iter()
        .filter(|row| row.warning_code.is_some())
    {
        assert!(seen.insert((row.matcher, row.warning_code)));
    }
}

#[test]
fn closed_schema_unknown_fields_are_rejected_by_resource() {
    for resource in [
        ResourceKind::Settings,
        ResourceKind::Rule,
        ResourceKind::Synonym,
    ] {
        let row = resolve_source_field(resource, "notAFlapjackOwnedField");

        assert_eq!(row.matcher, SourceMatcher::UnknownClosedSchema);
        assert_eq!(row.disposition, Disposition::Rejected);
    }
}

#[test]
fn document_attributes_use_exact_json_catch_all() {
    let row = resolve_source_field(ResourceKind::Document, "merchant_color");

    assert_eq!(row.matcher, SourceMatcher::DocumentAttribute);
    assert_eq!(row.disposition, Disposition::Exact);
    assert_eq!(row.round_trip, RoundTripOracle::JsonIdentity);
}

#[test]
fn replica_topology_settings_are_canonical_hard_rejections() {
    for field in ["replicas", "relevancyStrictness"] {
        let row = resolve_source_field(ResourceKind::Settings, field);

        assert_eq!(row.target_owner, TargetOwner::TranslationReport);
        assert_eq!(row.disposition, Disposition::Rejected);
        assert_eq!(
            row.rejection_code,
            Some(ReportCode::ReplicaTopologyNotMigrated)
        );
        assert_eq!(row.owner_path_precondition, OwnerPathPrecondition::None);
    }
}

#[test]
fn supported_synonym_payloads_resolve_to_schema_rows() {
    let cases = [
        (
            r#"{"objectID":"regular","type":"synonym","synonyms":["sneaker","trainer"]}"#,
            SynonymSchemaMatcher::Regular,
        ),
        (
            r#"{"objectID":"one-way","type":"onewaysynonym","input":"tee","synonyms":["t-shirt"]}"#,
            SynonymSchemaMatcher::OneWay,
        ),
        (
            r#"{"objectID":"alt-1","type":"altcorrection1","word":"sneeker","corrections":["sneaker"]}"#,
            SynonymSchemaMatcher::AltCorrection1,
        ),
        (
            r#"{"objectID":"alt-2","type":"altcorrection2","word":"sneekers","corrections":["sneakers"]}"#,
            SynonymSchemaMatcher::AltCorrection2,
        ),
        (
            r#"{"objectID":"placeholder","type":"placeholder","placeholder":"brand","replacements":["Nike"]}"#,
            SynonymSchemaMatcher::Placeholder,
        ),
    ];

    for (source, expected_matcher) in cases {
        let mut source: serde_json::Value = serde_json::from_str(source).unwrap();
        assert_eq!(
            resolve_source_schema(ResourceKind::Synonym, &source).matcher,
            SourceMatcher::SynonymSchema(expected_matcher)
        );

        source["bogus"] = serde_json::json!(1);
        let rejected = resolve_source_schema(ResourceKind::Synonym, &source);
        assert_eq!(rejected.matcher, SourceMatcher::UnknownClosedSchema);
        assert_eq!(rejected.disposition, Disposition::Rejected);
    }
}

#[test]
fn translates_complete_spool_payload_and_preserves_resource_order() {
    let payload = spool_payload(
        json!({
            "searchableAttributes": ["title", "brand"],
            "attributesForFaceting": ["brand", "category"],
            "attributesToRetrieve": ["title", "private_cost"],
            "unretrievableAttributes": ["private_cost"],
            "attributeForDistinct": "sku",
            "distinct": 2,
            "numericAttributesToIndex": ["price", "inventory"],
            "allowCompressionOfIntegerArray": true
        }),
        vec![
            vec![
                json!({
                    "objectID": "doc-1",
                    "title": "Trail Shoe",
                    "brand": "North",
                    "private_cost": 42,
                    "specs": {"weight": 12},
                    "tags": ["trail", "shoe"]
                }),
                json!({"objectID": "doc-2", "title": "City Shoe", "price": 89.5}),
            ],
            vec![json!({"objectID": "doc-3", "title": "Court Shoe"})],
        ],
        vec![vec![minimal_rule("rule-1"), minimal_rule("rule-2")]],
        vec![vec![
            minimal_synonym("syn-1"),
            json!({
                "objectID": "syn-2",
                "type": "onewaysynonym",
                "input": "tee",
                "synonyms": ["t-shirt"]
            }),
        ]],
    );

    let translated = translated(payload);

    assert_eq!(
        translated
            .bundle
            .documents
            .iter()
            .map(|document| document.id.as_str())
            .collect::<Vec<_>>(),
        vec!["doc-1", "doc-2", "doc-3"]
    );
    assert_eq!(
        translated.bundle.documents[0].fields["title"],
        FieldValue::Text("Trail Shoe".to_string())
    );
    assert_eq!(
        translated.bundle.documents[0].fields["private_cost"],
        FieldValue::Integer(42)
    );
    assert_eq!(
        translated.bundle.settings.searchable_attributes,
        Some(vec!["title".to_string(), "brand".to_string()])
    );
    assert_eq!(
        translated.bundle.settings.attributes_for_faceting,
        vec!["brand".to_string(), "category".to_string()]
    );
    assert_eq!(
        translated.bundle.settings.unretrievable_attributes,
        Some(vec!["private_cost".to_string()])
    );
    assert_eq!(
        translated.bundle.settings.distinct,
        Some(DistinctValue::Integer(2))
    );
    assert_eq!(
        translated.bundle.settings.numeric_attributes_for_filtering,
        Some(vec!["price".to_string(), "inventory".to_string()])
    );
    assert!(translated.bundle.settings.replicas.is_none());
    assert!(translated.bundle.settings.relevancy_strictness.is_none());
    assert_eq!(
        translated
            .bundle
            .rules
            .iter()
            .map(|rule| rule.object_id.as_str())
            .collect::<Vec<_>>(),
        vec!["rule-1", "rule-2"]
    );
    assert_eq!(
        translated
            .bundle
            .synonyms
            .iter()
            .map(|synonym| synonym.object_id())
            .collect::<Vec<_>>(),
        vec!["syn-1", "syn-2"]
    );
    assert_eq!(
        serde_json::to_value(&translated.bundle.synonyms[1]).unwrap(),
        json!({"type": "onewaysynonym", "objectID": "syn-2", "input": "tee", "synonyms": ["t-shirt"]})
    );
    assert_eq!(
        serde_json::to_value(&translated.bundle.rules[0]).unwrap()["consequence"]["params"]
            ["automaticFacetFilters"][0],
        json!({"facet": "brand", "score": 4})
    );

    let expected_codes = vec![
        ReportCode::ProductNotMigrated,
        ReportCode::ProductNotMigrated,
        ReportCode::ProductNotMigrated,
        ReportCode::ProductNotMigrated,
        ReportCode::ProductNotMigrated,
        ReportCode::PersistedNoBehaviorSetting,
    ];
    assert_eq!(
        translated
            .report
            .entries
            .iter()
            .map(|entry| entry.code)
            .collect::<Vec<_>>(),
        expected_codes
    );
    assert_eq!(translated.report.summary.hard_rejections, 0);
    assert_eq!(translated.report.summary.warnings, 1);
    assert_eq!(translated.report.summary.scope_gaps, 5);
    assert!(translated.report.report_digest.is_some());
}

#[test]
fn exact_document_and_settings_rows_persist_payload_values() {
    for field in ["attributesForFaceting", "searchableAttributes"] {
        let row = resolve_source_field(ResourceKind::Settings, field);
        assert_eq!(row.disposition, Disposition::Exact);
        assert_eq!(row.target_owner, TargetOwner::SettingsPayloadMerge);
    }
    let document_attribute = resolve_source_field(ResourceKind::Document, "title");
    assert_eq!(document_attribute.disposition, Disposition::Exact);
    assert_eq!(document_attribute.target_owner, TargetOwner::DocumentJson);

    let translated = translated(spool_payload(
        json!({
            "attributesForFaceting": ["brand"],
            "searchableAttributes": ["title"],
            "ranking": ["typo", "words"],
            "customRanking": ["desc(popularity)"],
            "attributesToRetrieve": ["title", "brand"],
            "unretrievableAttributes": ["cost"],
            "paginationLimitedTo": 250,
            "attributeForDistinct": "sku",
            "removeStopWords": ["en"],
            "ignorePlurals": ["en"],
            "queryLanguages": ["en"],
            "queryType": "prefixLast",
            "enablePersonalization": true,
            "userData": {"owner": "migration"},
            "enableRules": false,
            "advancedSyntaxFeatures": ["exactPhrase"],
            "sortFacetValuesBy": "alpha",
            "snippetEllipsisText": "...",
            "restrictHighlightAndSnippetArrays": true,
            "minProximity": 2,
            "disableExactOnAttributes": ["description"],
            "replaceSynonymsInHighlight": true,
            "attributeCriteriaComputedByMinProximity": true,
            "enableReRanking": false,
            "disableTypoToleranceOnWords": ["sku"],
            "disableTypoToleranceOnAttributes": ["brand"],
            "numericAttributesForFiltering": ["price"]
        }),
        vec![vec![json!({
            "objectID": "doc-1",
            "title": "Trail Shoe",
            "brand": "North",
            "price": 129
        })]],
        vec![],
        vec![],
    ));

    let settings = &translated.bundle.settings;
    assert_eq!(settings.attributes_for_faceting, vec!["brand"]);
    assert_eq!(
        settings.searchable_attributes,
        Some(vec!["title".to_string()])
    );
    assert_eq!(
        settings.ranking,
        Some(vec!["typo".to_string(), "words".to_string()])
    );
    assert_eq!(
        settings.custom_ranking,
        Some(vec!["desc(popularity)".to_string()])
    );
    assert_eq!(
        settings.attributes_to_retrieve,
        Some(vec!["title".to_string(), "brand".to_string()])
    );
    assert_eq!(
        settings.unretrievable_attributes,
        Some(vec!["cost".to_string()])
    );
    assert_eq!(settings.pagination_limited_to, 250);
    assert_eq!(settings.attribute_for_distinct, Some("sku".to_string()));
    assert_eq!(settings.query_languages, vec!["en"]);
    assert_eq!(settings.query_type, "prefixLast");
    assert_eq!(settings.enable_personalization, Some(true));
    assert_eq!(settings.user_data, Some(json!({"owner": "migration"})));
    assert_eq!(settings.enable_rules, Some(false));
    assert_eq!(
        settings.advanced_syntax_features,
        Some(vec!["exactPhrase".to_string()])
    );
    assert_eq!(settings.sort_facet_values_by.as_deref(), Some("alpha"));
    assert_eq!(settings.snippet_ellipsis_text.as_deref(), Some("..."));
    assert_eq!(settings.restrict_highlight_and_snippet_arrays, Some(true));
    assert_eq!(settings.min_proximity, Some(2));
    assert_eq!(
        settings.disable_exact_on_attributes,
        Some(vec!["description".to_string()])
    );
    assert_eq!(settings.replace_synonyms_in_highlight, Some(true));
    assert_eq!(
        settings.attribute_criteria_computed_by_min_proximity,
        Some(true)
    );
    assert_eq!(settings.enable_re_ranking, Some(false));
    assert_eq!(
        settings.disable_typo_tolerance_on_words,
        Some(vec!["sku".to_string()])
    );
    assert_eq!(
        settings.disable_typo_tolerance_on_attributes,
        Some(vec!["brand".to_string()])
    );
    assert_eq!(
        settings.numeric_attributes_for_filtering,
        Some(vec!["price".to_string()])
    );

    assert_eq!(translated.bundle.documents[0].id, "doc-1");
    assert_eq!(
        translated.bundle.documents[0].fields["title"],
        FieldValue::Text("Trail Shoe".to_string())
    );
    assert_eq!(
        translated.bundle.documents[0].fields["price"],
        FieldValue::Integer(129)
    );
}

#[test]
fn transformed_settings_distinct_and_numeric_attributes_to_index_persist() {
    for field in ["distinct", "numericAttributesToIndex"] {
        let row = resolve_source_field(ResourceKind::Settings, field);
        assert_eq!(row.disposition, Disposition::Transformed);
        assert_eq!(row.target_owner, TargetOwner::SettingsPayloadMerge);
    }

    let translated = translated(spool_payload(
        json!({
            "distinct": true,
            "numericAttributesToIndex": ["price", "inventory"]
        }),
        vec![],
        vec![],
        vec![],
    ));

    assert_eq!(
        translated.bundle.settings.distinct,
        Some(DistinctValue::Bool(true))
    );
    assert_eq!(
        translated.bundle.settings.numeric_attributes_for_filtering,
        Some(vec!["price".to_string(), "inventory".to_string()])
    );
}

#[test]
fn source_reader_vendor_default_settings_are_accepted() {
    let translated = translated(spool_payload(
        json!({
            "minWordSizefor1Typo": 4,
            "minWordSizefor2Typos": 8,
            "hitsPerPage": 20,
            "maxValuesPerFacet": 100,
            "version": 1,
            "searchableAttributes": ["title"],
            "numericAttributesToIndex": ["price"],
            "attributesToRetrieve": null,
            "distinct": true,
            "unretrievableAttributes": ["secret_note"],
            "optionalWords": null,
            "attributesForFaceting": ["brand"],
            "attributesToSnippet": null,
            "attributesToHighlight": null,
            "paginationLimitedTo": 1000,
            "attributeForDistinct": null,
            "exactOnSingleWordQuery": "attribute",
            "synonyms": [["sneaker", "trainer"]],
            "ranking": ["typo", "geo", "words", "filters", "proximity", "attribute", "exact", "custom"],
            "customRanking": null,
            "separatorsToIndex": "",
            "removeWordsIfNoResults": "none",
            "queryType": "prefixLast",
            "highlightPreTag": "<em>",
            "highlightPostTag": "</em>",
            "alternativesAsExact": ["ignorePlurals", "singleWordSynonym"]
        }),
        vec![],
        vec![],
        vec![],
    ));

    assert_eq!(
        translated.bundle.settings.searchable_attributes,
        Some(vec!["title".to_string()])
    );
    assert_eq!(translated.bundle.settings.min_word_size_for_1_typo, 4);
    assert_eq!(translated.bundle.settings.min_word_size_for_2_typos, 8);
    assert_eq!(translated.bundle.settings.hits_per_page, 20);
    assert_eq!(translated.bundle.settings.max_values_per_facet, 100);
    assert_eq!(translated.bundle.settings.attributes_to_highlight, None);
    assert_eq!(translated.bundle.settings.attributes_to_snippet, None);
    assert_eq!(
        translated.bundle.settings.highlight_pre_tag.as_deref(),
        Some("<em>")
    );
    assert_eq!(
        translated.bundle.settings.highlight_post_tag.as_deref(),
        Some("</em>")
    );
    assert_eq!(
        translated.bundle.settings.exact_on_single_word_query,
        "attribute"
    );
    assert_eq!(
        translated.bundle.settings.remove_words_if_no_results,
        "none"
    );
    assert_eq!(translated.bundle.settings.separators_to_index, "");
    assert_eq!(
        translated.bundle.settings.alternatives_as_exact,
        vec!["ignorePlurals", "singleWordSynonym"]
    );
    assert!(translated.bundle.settings.optional_words.is_empty());
    assert_eq!(translated.bundle.settings.synonyms, None);
    assert_eq!(translated.bundle.settings.version, 1);
    assert_eq!(
        report_entry_contract(&translated.report.entries),
        vec![
            (
                ReportSeverity::ScopeGap,
                ReportCode::ProductNotMigrated,
                ReportResource::Analytics,
                None,
                None,
                "$".to_string()
            ),
            (
                ReportSeverity::ScopeGap,
                ReportCode::ProductNotMigrated,
                ReportResource::ApiKeys,
                None,
                None,
                "$".to_string()
            ),
            (
                ReportSeverity::ScopeGap,
                ReportCode::ProductNotMigrated,
                ReportResource::Events,
                None,
                None,
                "$".to_string()
            ),
            (
                ReportSeverity::ScopeGap,
                ReportCode::ProductNotMigrated,
                ReportResource::Experiments,
                None,
                None,
                "$".to_string()
            ),
            (
                ReportSeverity::ScopeGap,
                ReportCode::ProductNotMigrated,
                ReportResource::Recommend,
                None,
                None,
                "$".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.attributesToHighlight".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.attributesToSnippet".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.highlightPostTag".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.highlightPreTag".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.hitsPerPage".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.optionalWords".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::ReadOnlySourceField,
                ReportResource::Settings,
                None,
                None,
                "$.synonyms".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::ReadOnlySourceField,
                ReportResource::Settings,
                None,
                None,
                "$.version".to_string()
            ),
        ]
    );
    assert_eq!(translated.report.summary.hard_rejections, 0);
    assert_eq!(translated.report.summary.warnings, 8);
}

#[test]
fn vendor_settings_fields_merge_non_default_values_into_existing_owner() {
    let translated = translated(spool_payload(
        json!({
            "attributesToHighlight": ["title"],
            "attributesToSnippet": ["body:10"],
            "highlightPostTag": "</mark>",
            "minWordSizefor2Typos": 11,
            "maxValuesPerFacet": 17,
            "exactOnSingleWordQuery": "none",
            "removeWordsIfNoResults": "lastWords",
            "separatorsToIndex": "#+",
            "alternativesAsExact": ["ignorePlurals"],
            "optionalWords": ["shoe"]
        }),
        vec![],
        vec![],
        vec![],
    ));

    let settings = translated.bundle.settings;
    assert_eq!(settings.attributes_to_highlight, Some(vec!["title".into()]));
    assert_eq!(settings.attributes_to_snippet, Some(vec!["body:10".into()]));
    assert_eq!(settings.highlight_post_tag.as_deref(), Some("</mark>"));
    assert_eq!(settings.min_word_size_for_2_typos, 11);
    assert_eq!(settings.max_values_per_facet, 17);
    assert_eq!(settings.exact_on_single_word_query, "none");
    assert_eq!(settings.remove_words_if_no_results, "lastWords");
    assert_eq!(settings.separators_to_index, "#+");
    assert_eq!(settings.alternatives_as_exact, vec!["ignorePlurals"]);
    assert_eq!(settings.optional_words, vec!["shoe"]);
    assert_eq!(translated.report.summary.hard_rejections, 0);
    assert_eq!(translated.report.summary.warnings, 4);
}

#[test]
fn warned_allow_compression_setting_persists_and_reports_warning() {
    let translated = translated(spool_payload(
        json!({"allowCompressionOfIntegerArray": false}),
        vec![],
        vec![],
        vec![],
    ));

    assert_eq!(
        translated
            .bundle
            .settings
            .allow_compression_of_integer_array,
        Some(false)
    );
    let warning = entry_for_code(&translated.report, ReportCode::PersistedNoBehaviorSetting);
    assert_eq!(warning.severity, ReportSeverity::Warning);
    assert_eq!(warning.resource, ReportResource::Settings);
    assert_eq!(warning.json_path, "$.allowCompressionOfIntegerArray");
    assert_eq!(translated.report.summary.warnings, 1);
}

#[test]
fn hard_rejected_settings_emit_canonical_codes_and_paths() {
    let report = rejected(spool_payload(
        json!({
            "replicas": ["products_price_asc"],
            "relevancyStrictness": 90,
            "notAFlapjackOwnedField": true
        }),
        vec![],
        vec![],
        vec![],
    ));

    assert_eq!(
        hard_codes(&report),
        vec![
            ReportCode::UnsupportedSourceField,
            ReportCode::ReplicaTopologyNotMigrated,
            ReportCode::ReplicaTopologyNotMigrated,
        ]
    );
    assert_eq!(
        entries_for_code(&report, ReportCode::ReplicaTopologyNotMigrated)
            .into_iter()
            .map(|entry| entry.json_path.as_str())
            .collect::<Vec<_>>(),
        vec!["$.relevancyStrictness", "$.replicas"]
    );
    let unknown = entry_for_code(&report, ReportCode::UnsupportedSourceField);
    assert_eq!(unknown.resource, ReportResource::Settings);
    assert_eq!(unknown.json_path, "$.notAFlapjackOwnedField");
}

#[test]
fn closed_unknown_fields_reject_settings_rules_and_synonyms() {
    let report = rejected(spool_payload(
        json!({"notAFlapjackOwnedField": true}),
        vec![],
        vec![vec![json!({
            "objectID": "rule-1",
            "consequence": {},
            "notAFlapjackOwnedField": true
        })]],
        vec![vec![json!({
            "objectID": "syn-1",
            "type": "synonym",
            "synonyms": ["a", "b"],
            "notAFlapjackOwnedField": true
        })]],
    ));

    assert_eq!(
        hard_codes(&report),
        vec![
            ReportCode::UnsupportedSourceField,
            ReportCode::UnsupportedSourceField,
            ReportCode::UnsupportedSynonymSchema,
        ]
    );
    let entries = report
        .entries
        .iter()
        .filter(|entry| entry.severity == ReportSeverity::HardRejection)
        .map(|entry| {
            (
                entry.resource,
                entry.json_path.as_str(),
                entry.page_index,
                entry.item_index,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        entries,
        vec![
            (
                ReportResource::Settings,
                "$.notAFlapjackOwnedField",
                None,
                None
            ),
            (
                ReportResource::Rule,
                "$.notAFlapjackOwnedField",
                Some(0),
                Some(0)
            ),
            (ReportResource::Synonym, "$", Some(0), Some(0)),
        ]
    );
}

#[test]
fn scope_gap_entries_have_deterministic_order() {
    let translated = translated(spool_payload(json!({}), vec![], vec![], vec![]));

    assert_eq!(
        translated
            .report
            .entries
            .iter()
            .filter(|entry| entry.severity == ReportSeverity::ScopeGap)
            .map(|entry| (entry.resource, entry.code, entry.json_path.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                ReportResource::Analytics,
                ReportCode::ProductNotMigrated,
                "$"
            ),
            (ReportResource::ApiKeys, ReportCode::ProductNotMigrated, "$"),
            (ReportResource::Events, ReportCode::ProductNotMigrated, "$"),
            (
                ReportResource::Experiments,
                ReportCode::ProductNotMigrated,
                "$"
            ),
            (
                ReportResource::Recommend,
                ReportCode::ProductNotMigrated,
                "$"
            ),
        ]
    );
}

#[test]
fn translates_empty_resource_pages() {
    let translated = translated(spool_payload(
        minimal_valid_settings(),
        vec![],
        vec![vec![]],
        vec![],
    ));

    assert!(translated.bundle.documents.is_empty());
    assert!(translated.bundle.rules.is_empty());
    assert!(translated.bundle.synonyms.is_empty());
    assert_eq!(
        translated.bundle.settings.searchable_attributes,
        Some(vec!["title".to_string()])
    );
    assert_eq!(translated.report.summary.scope_gaps, 5);
}

#[test]
fn same_object_id_in_different_resource_kinds_is_valid() {
    let translated = translated(spool_payload(
        minimal_valid_settings(),
        vec![vec![json!({"objectID": "shared", "title": "Document"})]],
        vec![vec![minimal_rule("shared")]],
        vec![vec![minimal_synonym("shared")]],
    ));

    assert_eq!(translated.bundle.documents[0].id, "shared");
    assert_eq!(translated.bundle.rules[0].object_id, "shared");
    assert_eq!(translated.bundle.synonyms[0].object_id(), "shared");
}

#[test]
fn rejects_invalid_and_duplicate_ids_with_page_coordinates() {
    for (payload, code, resource, page, item) in [
        (
            spool_payload(
                minimal_valid_settings(),
                vec![vec![json!({"title": "missing"})]],
                vec![],
                vec![],
            ),
            ReportCode::InvalidObjectId,
            ReportResource::Document,
            Some(0),
            Some(0),
        ),
        (
            spool_payload(
                minimal_valid_settings(),
                vec![
                    vec![json!({"objectID": "dup", "title": "first"})],
                    vec![json!({"objectID": "dup", "title": "second"})],
                ],
                vec![],
                vec![],
            ),
            ReportCode::DuplicateObjectId,
            ReportResource::Document,
            Some(1),
            Some(0),
        ),
        (
            spool_payload(
                minimal_valid_settings(),
                vec![],
                vec![vec![minimal_rule("dup")], vec![minimal_rule("dup")]],
                vec![],
            ),
            ReportCode::DuplicateObjectId,
            ReportResource::Rule,
            Some(1),
            Some(0),
        ),
        (
            spool_payload(
                minimal_valid_settings(),
                vec![],
                vec![],
                vec![vec![minimal_synonym("dup")], vec![minimal_synonym("dup")]],
            ),
            ReportCode::DuplicateObjectId,
            ReportResource::Synonym,
            Some(1),
            Some(0),
        ),
    ] {
        let report = rejected(payload);
        assert_eq!(hard_codes(&report), vec![code]);
        let entry = entry_for_code(&report, code);
        assert_eq!(entry.resource, resource);
        assert_eq!(entry.page_index, page);
        assert_eq!(entry.item_index, item);
        assert_eq!(entry.json_path, "$.objectID");
    }
}

#[test]
fn invalid_object_id_report_preserves_resource_coordinates() {
    for (payload, resource) in [
        (
            spool_payload(
                minimal_valid_settings(),
                vec![vec![json!({"title": "missing"})]],
                vec![],
                vec![],
            ),
            ReportResource::Document,
        ),
        (
            spool_payload(
                minimal_valid_settings(),
                vec![],
                vec![vec![json!({"conditions": [], "consequence": {}})]],
                vec![],
            ),
            ReportResource::Rule,
        ),
        (
            spool_payload(
                minimal_valid_settings(),
                vec![],
                vec![],
                vec![vec![json!({"type": "synonym", "synonyms": ["a"]})]],
            ),
            ReportResource::Synonym,
        ),
    ] {
        let report = rejected(payload);
        let entry = entry_for_code(&report, ReportCode::InvalidObjectId);
        assert_eq!(entry.severity, ReportSeverity::HardRejection);
        assert_eq!(entry.resource, resource);
        assert_eq!(entry.page_index, Some(0));
        assert_eq!(entry.item_index, Some(0));
        assert_eq!(entry.json_path, "$.objectID");
    }
}

#[test]
fn duplicate_object_id_report_is_scoped_per_resource() {
    let report = rejected(spool_payload(
        minimal_valid_settings(),
        vec![
            vec![json!({"objectID": "doc-dup", "title": "first"})],
            vec![json!({"objectID": "doc-dup", "title": "second"})],
        ],
        vec![vec![minimal_rule("rule-dup"), minimal_rule("rule-dup")]],
        vec![vec![minimal_synonym("syn-dup"), minimal_synonym("syn-dup")]],
    ));

    assert_eq!(
        entries_for_code(&report, ReportCode::DuplicateObjectId)
            .into_iter()
            .map(|entry| (
                entry.resource,
                entry.page_index,
                entry.item_index,
                entry.json_path.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            (ReportResource::Document, Some(1), Some(0), "$.objectID"),
            (ReportResource::Rule, Some(0), Some(1), "$.objectID"),
            (ReportResource::Synonym, Some(0), Some(1), "$.objectID"),
        ]
    );
}

#[test]
fn rejects_closed_schema_and_malformed_payloads() {
    for (payload, code) in [
        (
            spool_payload(json!({"typoTolerance": "strict"}), vec![], vec![], vec![]),
            ReportCode::UnsupportedSourceField,
        ),
        (
            spool_payload(
                json!({"notAFlapjackOwnedField": true}),
                vec![],
                vec![],
                vec![],
            ),
            ReportCode::UnsupportedSourceField,
        ),
        (
            spool_payload(
                minimal_valid_settings(),
                vec![],
                vec![vec![
                    json!({"objectID": "rule-1", "consequence": {}, "unknown": true}),
                ]],
                vec![],
            ),
            ReportCode::UnsupportedSourceField,
        ),
        (
            spool_payload(
                minimal_valid_settings(),
                vec![],
                vec![vec![json!({
                    "objectID": "rule-1",
                    "consequence": {"params": {"query": {"unsupported": true}}}
                })]],
                vec![],
            ),
            ReportCode::UnsupportedRuleSchema,
        ),
        (
            spool_payload(
                minimal_valid_settings(),
                vec![],
                vec![],
                vec![vec![
                    json!({"objectID": "syn-1", "type": "synonym", "synonyms": ["a"], "unknown": true}),
                ]],
            ),
            ReportCode::UnsupportedSynonymSchema,
        ),
        (
            spool_payload(json!({"distinct": "2"}), vec![], vec![], vec![]),
            ReportCode::MalformedSettingsPayload,
        ),
        (
            spool_payload(
                json!({"distinct": 4_294_967_296u64}),
                vec![],
                vec![],
                vec![],
            ),
            ReportCode::MalformedSettingsPayload,
        ),
        (
            spool_payload(
                json!({"paginationLimitedTo": "many"}),
                vec![],
                vec![],
                vec![],
            ),
            ReportCode::MalformedSettingsPayload,
        ),
        (
            spool_payload(
                minimal_valid_settings(),
                vec![vec![json!({"objectID": "doc-1", "title": "ok"})]],
                vec![vec![
                    json!({"objectID": "rule-1", "conditions": "bad", "consequence": {}}),
                ]],
                vec![],
            ),
            ReportCode::MalformedRulePayload,
        ),
        (
            spool_payload(
                minimal_valid_settings(),
                vec![],
                vec![],
                vec![vec![
                    json!({"objectID": "syn-1", "type": "synonym", "synonyms": "bad"}),
                ]],
            ),
            ReportCode::MalformedSynonymPayload,
        ),
    ] {
        assert_single_hard_code(payload, code);
    }
}

#[test]
fn malformed_payload_reports_cover_settings_document_rule_and_synonym_paths() {
    let report = rejected(spool_payload(
        json!({"distinct": "2"}),
        vec![vec![json!("not an object")]],
        vec![vec![json!({
            "objectID": "rule-1",
            "conditions": "bad",
            "consequence": {}
        })]],
        vec![vec![json!({
            "objectID": "syn-1",
            "type": "synonym",
            "synonyms": "bad"
        })]],
    ));

    assert_eq!(
        hard_codes(&report),
        vec![
            ReportCode::MalformedSettingsPayload,
            ReportCode::MalformedDocumentPayload,
            ReportCode::MalformedRulePayload,
            ReportCode::MalformedSynonymPayload,
        ]
    );

    for (code, resource, page, item, path) in [
        (
            ReportCode::MalformedSettingsPayload,
            ReportResource::Settings,
            None,
            None,
            "$.distinct",
        ),
        (
            ReportCode::MalformedDocumentPayload,
            ReportResource::Document,
            Some(0),
            Some(0),
            "$",
        ),
        (
            ReportCode::MalformedRulePayload,
            ReportResource::Rule,
            Some(0),
            Some(0),
            "$.conditions",
        ),
        (
            ReportCode::MalformedSynonymPayload,
            ReportResource::Synonym,
            Some(0),
            Some(0),
            "$",
        ),
    ] {
        let entry = entry_for_code(&report, code);
        assert_eq!(entry.resource, resource);
        assert_eq!(entry.page_index, page);
        assert_eq!(entry.item_index, item);
        assert_eq!(entry.json_path, path);
    }
}

#[test]
fn typed_failures_are_aggregated_without_duplicate_invalid_id_entries() {
    let report = rejected(spool_payload(
        minimal_valid_settings(),
        vec![vec![json!({"title": "missing object id"})]],
        vec![vec![json!({
            "objectID": "rule-1",
            "conditions": "bad",
            "consequence": {}
        })]],
        vec![vec![json!({
            "objectID": "syn-1",
            "type": "synonym",
            "synonyms": "bad"
        })]],
    ));

    assert_eq!(
        hard_codes(&report),
        vec![
            ReportCode::InvalidObjectId,
            ReportCode::MalformedRulePayload,
            ReportCode::MalformedSynonymPayload,
        ]
    );
    assert_eq!(
        entries_for_code(&report, ReportCode::InvalidObjectId).len(),
        1
    );
    assert_eq!(report.summary.hard_rejections, 3);
    assert_eq!(report.summary.total_entries, 8);
}

#[test]
fn reports_all_core_serde_failures_at_their_source_coordinates() {
    let report = rejected(spool_payload(
        minimal_valid_settings(),
        vec![],
        vec![
            vec![],
            vec![json!({
                "objectID": "rule-1",
                "conditions": "bad",
                "consequence": {}
            })],
        ],
        vec![
            vec![],
            vec![
                minimal_synonym("valid"),
                json!({
                    "objectID": "syn-1",
                    "type": "synonym",
                    "synonyms": "bad"
                }),
            ],
        ],
    ));

    assert_eq!(
        hard_codes(&report),
        vec![
            ReportCode::MalformedRulePayload,
            ReportCode::MalformedSynonymPayload,
        ]
    );
    let rule_entry = entry_for_code(&report, ReportCode::MalformedRulePayload);
    assert_eq!(rule_entry.resource, ReportResource::Rule);
    assert_eq!(rule_entry.page_index, Some(1));
    assert_eq!(rule_entry.item_index, Some(0));
    assert_eq!(rule_entry.json_path, "$.conditions");

    let synonym_entry = entry_for_code(&report, ReportCode::MalformedSynonymPayload);
    assert_eq!(synonym_entry.resource, ReportResource::Synonym);
    assert_eq!(synonym_entry.page_index, Some(1));
    assert_eq!(synonym_entry.item_index, Some(1));
    assert_eq!(synonym_entry.json_path, "$");
}

#[test]
fn rejects_topology_settings_and_scope_gaps_without_payload_fields() {
    let report = rejected(spool_payload(
        json!({"replicas": ["products_price_asc"], "relevancyStrictness": 90}),
        vec![],
        vec![],
        vec![],
    ));

    assert_eq!(
        hard_codes(&report),
        vec![
            ReportCode::ReplicaTopologyNotMigrated,
            ReportCode::ReplicaTopologyNotMigrated
        ]
    );
    for entry in report
        .entries
        .iter()
        .filter(|entry| entry.code == ReportCode::ReplicaTopologyNotMigrated)
    {
        assert_eq!(entry.resource, ReportResource::Settings);
        assert!(["$.replicas", "$.relevancyStrictness"].contains(&entry.json_path.as_str()));
    }
    assert_eq!(
        report
            .entries
            .iter()
            .filter(|entry| entry.code == ReportCode::ProductNotMigrated)
            .map(|entry| entry.resource)
            .collect::<Vec<_>>(),
        vec![
            ReportResource::Analytics,
            ReportResource::ApiKeys,
            ReportResource::Events,
            ReportResource::Experiments,
            ReportResource::Recommend,
        ]
    );
}

#[test]
fn every_rule_schema_matcher_has_an_owner_path_case() {
    let cases = [
        (
            RuleSchemaPath::Condition,
            serde_json::json!({ "filters": "brand:Nike" }),
            RuleSchemaMatcher::Condition,
        ),
        (
            RuleSchemaPath::Consequence,
            serde_json::json!({ "filterPromotes": true }),
            RuleSchemaMatcher::Consequence,
        ),
        (
            RuleSchemaPath::ConsequenceParams,
            serde_json::json!({ "filters": "brand:Nike" }),
            RuleSchemaMatcher::ConsequenceParams,
        ),
        (
            RuleSchemaPath::Promote,
            serde_json::json!({ "objectID": "sku-1", "position": 1 }),
            RuleSchemaMatcher::PromoteSingle,
        ),
        (
            RuleSchemaPath::Promote,
            serde_json::json!({ "objectIDs": ["sku-1", "sku-2"], "position": 1 }),
            RuleSchemaMatcher::PromoteMultiple,
        ),
        (
            RuleSchemaPath::Hide,
            serde_json::json!({ "objectID": "sku-1" }),
            RuleSchemaMatcher::Hide,
        ),
        (
            RuleSchemaPath::TimeRange,
            serde_json::json!({ "from": 1, "until": 2 }),
            RuleSchemaMatcher::TimeRange,
        ),
        (
            RuleSchemaPath::AutomaticFacetFilter,
            serde_json::json!({ "facet": "brand" }),
            RuleSchemaMatcher::AutomaticFacetFilter,
        ),
        (
            RuleSchemaPath::AutomaticFacetFilter,
            serde_json::json!("brand:Nike"),
            RuleSchemaMatcher::AutomaticFacetFilter,
        ),
        (
            RuleSchemaPath::ConsequenceQuery,
            serde_json::json!("boots"),
            RuleSchemaMatcher::ConsequenceQueryLiteral,
        ),
        (
            RuleSchemaPath::ConsequenceQuery,
            serde_json::json!({ "remove": ["cheap"] }),
            RuleSchemaMatcher::ConsequenceQueryEdits,
        ),
        (
            RuleSchemaPath::QueryEdit,
            serde_json::json!({ "type": "remove", "delete": "cheap" }),
            RuleSchemaMatcher::QueryEdit,
        ),
    ];

    for (path, mut source, expected_matcher) in cases {
        assert_eq!(
            resolve_rule_schema(path, &source).matcher,
            SourceMatcher::RuleSchema(expected_matcher)
        );

        if source.is_object() {
            source["bogus"] = serde_json::json!(1);
            let rejected = resolve_rule_schema(path, &source);
            assert_eq!(rejected.matcher, SourceMatcher::UnknownClosedSchema);
            assert_eq!(rejected.disposition, Disposition::Rejected);
        }
    }

    for (path, matcher) in [
        (RuleSchemaPath::Condition, RuleSchemaMatcher::Condition),
        (RuleSchemaPath::Consequence, RuleSchemaMatcher::Consequence),
        (
            RuleSchemaPath::ConsequenceParams,
            RuleSchemaMatcher::ConsequenceParams,
        ),
        (
            RuleSchemaPath::ConsequenceQuery,
            RuleSchemaMatcher::ConsequenceQueryEdits,
        ),
    ] {
        assert_eq!(
            resolve_rule_schema(path, &serde_json::json!({})).matcher,
            SourceMatcher::RuleSchema(matcher)
        );
    }
}

fn live_fixture_input_from_env() -> Option<SpoolTranslationInput> {
    let fixture_dir = match env::var("FLAPJACK_TRANSLATION_LIVE_FIXTURES") {
        Ok(path) => path,
        Err(_) => return None,
    };
    Some(load_live_fixture_input(Path::new(&fixture_dir)))
}

fn load_live_fixture_input(fixture_dir: &Path) -> SpoolTranslationInput {
    spool_payload(
        read_live_fixture_json(&fixture_dir.join("settings.json")),
        read_live_fixture_json(&fixture_dir.join("document_pages.json")),
        read_live_fixture_json(&fixture_dir.join("rule_pages.json")),
        read_live_fixture_json(&fixture_dir.join("synonym_pages.json")),
    )
}

fn read_live_fixture_json<T>(path: &Path) -> T
where
    T: serde::de::DeserializeOwned,
{
    let bytes = fs::read(path)
        .unwrap_or_else(|err| panic!("failed to read live fixture {}: {err}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|err| panic!("failed to parse live fixture {}: {err}", path.display()))
}

fn supported_live_baseline_input() -> SpoolTranslationInput {
    spool_payload(
        json!({
            "minWordSizefor1Typo": 4,
            "minWordSizefor2Typos": 8,
            "hitsPerPage": 20,
            "maxValuesPerFacet": 100,
            "version": 1,
            "searchableAttributes": ["title", "brand"],
            "numericAttributesToIndex": ["price"],
            "attributesToRetrieve": null,
            "distinct": true,
            "unretrievableAttributes": ["secret_note"],
            "optionalWords": null,
            "attributesForFaceting": ["brand"],
            "attributesToSnippet": null,
            "attributesToHighlight": null,
            "paginationLimitedTo": 1000,
            "attributeForDistinct": null,
            "exactOnSingleWordQuery": "attribute",
            "synonyms": [["sneaker", "trainer"]],
            "ranking": ["typo", "geo", "words", "filters", "proximity", "attribute", "exact", "custom"],
            "customRanking": null,
            "separatorsToIndex": "",
            "removeWordsIfNoResults": "none",
            "queryType": "prefixLast",
            "highlightPreTag": "<em>",
            "highlightPostTag": "</em>",
            "alternativesAsExact": ["ignorePlurals", "singleWordSynonym"]
        }),
        vec![vec![
            json!({
                "objectID": "live-doc-2",
                "title": "Live City Shoe",
                "brand": "South",
                "price": 89
            }),
            json!({
                "objectID": "live-doc-1",
                "title": "Live Trail Shoe",
                "brand": "North",
                "price": 129,
                "secret_note": "redacted"
            }),
        ]],
        vec![vec![json!({
            "objectID": "live-rule-1",
            "conditions": [{"pattern": "{facet:brand}", "anchoring": "is"}],
            "consequence": {
                "promote": [{"objectID": "live-doc-1", "position": 1}],
                "params": {
                    "automaticFacetFilters": [{"facet": "brand", "score": 4}]
                }
            },
            "enabled": true
        })]],
        vec![vec![
            json!({"objectID": "live-syn-2", "type": "onewaysynonym", "input": "tee", "synonyms": ["t-shirt"]}),
            json!({"objectID": "live-syn-1", "type": "synonym", "synonyms": ["sneaker", "trainer"]}),
        ]],
    )
}

fn report_entry_contract(entries: &[TranslationReportEntry]) -> Vec<ReportEntryContract> {
    entries
        .iter()
        .map(|entry| {
            (
                entry.severity,
                entry.code,
                entry.resource,
                entry.page_index,
                entry.item_index,
                entry.json_path.clone(),
            )
        })
        .collect()
}

fn hard_report_entry_contract(report: &TranslationReport) -> Vec<ReportEntryContract> {
    report_entry_contract(
        &report
            .entries
            .iter()
            .filter(|entry| entry.severity == ReportSeverity::HardRejection)
            .cloned()
            .collect::<Vec<_>>(),
    )
}

#[test]
fn live_positive_oracle_rejects_second_document_field_drift() {
    let mut input = supported_live_baseline_input();
    input.document_pages[0][1]["brand"] = json!("Altered");

    assert!(std::panic::catch_unwind(|| assert_positive_live_translation(input)).is_err());
}

#[test]
fn live_positive_oracle_rejects_rule_field_drift() {
    let mut input = supported_live_baseline_input();
    input.rule_pages[0][0]["consequence"]["params"]["automaticFacetFilters"][0]["score"] =
        json!(99);

    assert!(std::panic::catch_unwind(|| assert_positive_live_translation(input)).is_err());
}

#[test]
fn live_mutation_oracle_rejects_extra_hard_rejections() {
    let baseline = supported_live_baseline_input();

    assert!(std::panic::catch_unwind(|| {
        assert_live_mutation_report(
            &baseline,
            |input| {
                input.settings["typoTolerance"] = json!("strict");
                input.rule_pages[0][0]["unexpected"] = json!(true);
            },
            ReportCode::UnsupportedSourceField,
            ReportResource::Settings,
            None,
            None,
            "$.typoTolerance",
        );
    })
    .is_err());
}

fn assert_positive_live_translation(input: SpoolTranslationInput) {
    let translated = translated(input);

    assert_eq!(
        translated
            .bundle
            .documents
            .iter()
            .map(|document| document.to_json())
            .collect::<Vec<_>>(),
        vec![
            json!({
                "_id": "live-doc-2",
                "title": "Live City Shoe",
                "brand": "South",
                "price": 89
            }),
            json!({
                "_id": "live-doc-1",
                "title": "Live Trail Shoe",
                "brand": "North",
                "price": 129,
                "secret_note": "redacted"
            }),
        ]
    );
    assert_eq!(
        translated.bundle.settings.searchable_attributes,
        Some(vec!["title".to_string(), "brand".to_string()])
    );
    assert_eq!(
        translated.bundle.settings.attributes_for_faceting,
        vec!["brand".to_string()]
    );
    assert_eq!(
        translated.bundle.settings.unretrievable_attributes,
        Some(vec!["secret_note".to_string()])
    );
    assert_eq!(
        translated.bundle.settings.numeric_attributes_for_filtering,
        Some(vec!["price".to_string()])
    );
    assert_eq!(translated.bundle.settings.attributes_to_retrieve, None);
    assert_eq!(
        translated.bundle.settings.distinct,
        Some(DistinctValue::Bool(true))
    );
    assert_eq!(
        translated
            .bundle
            .settings
            .allow_compression_of_integer_array,
        None
    );
    assert_eq!(translated.bundle.settings.min_word_size_for_1_typo, 4);
    assert_eq!(translated.bundle.settings.min_word_size_for_2_typos, 8);
    assert_eq!(translated.bundle.settings.hits_per_page, 20);
    assert_eq!(translated.bundle.settings.max_values_per_facet, 100);
    assert_eq!(translated.bundle.settings.attributes_to_highlight, None);
    assert_eq!(translated.bundle.settings.attributes_to_snippet, None);
    assert_eq!(translated.bundle.settings.pagination_limited_to, 1000);
    assert_eq!(translated.bundle.settings.attribute_for_distinct, None);
    assert_eq!(
        translated.bundle.settings.exact_on_single_word_query,
        "attribute"
    );
    assert_eq!(translated.bundle.settings.synonyms, None);
    assert_eq!(
        translated.bundle.settings.ranking,
        Some(vec![
            "typo".to_string(),
            "geo".to_string(),
            "words".to_string(),
            "filters".to_string(),
            "proximity".to_string(),
            "attribute".to_string(),
            "exact".to_string(),
            "custom".to_string()
        ])
    );
    assert_eq!(translated.bundle.settings.custom_ranking, None);
    assert_eq!(translated.bundle.settings.separators_to_index, "");
    assert_eq!(
        translated.bundle.settings.remove_words_if_no_results,
        "none"
    );
    assert_eq!(translated.bundle.settings.query_type, "prefixLast");
    assert_eq!(
        translated.bundle.settings.highlight_pre_tag.as_deref(),
        Some("<em>")
    );
    assert_eq!(
        translated.bundle.settings.highlight_post_tag.as_deref(),
        Some("</em>")
    );
    assert_eq!(
        translated.bundle.settings.alternatives_as_exact,
        vec!["ignorePlurals", "singleWordSynonym"]
    );
    assert!(translated.bundle.settings.optional_words.is_empty());
    assert_eq!(
        translated
            .bundle
            .rules
            .iter()
            .map(|rule| serde_json::to_value(rule).unwrap())
            .collect::<Vec<_>>(),
        vec![json!({
            "objectID": "live-rule-1",
            "conditions": [{"pattern": "{facet:brand}", "anchoring": "is"}],
            "consequence": {
                "promote": [{"objectID": "live-doc-1", "position": 1}],
                "params": {
                    "automaticFacetFilters": [{"facet": "brand", "score": 4}]
                }
            },
            "enabled": true
        })]
    );
    assert_eq!(
        translated
            .bundle
            .synonyms
            .iter()
            .map(|synonym| serde_json::to_value(synonym).unwrap())
            .collect::<Vec<_>>(),
        vec![
            json!({"type": "onewaysynonym", "objectID": "live-syn-2", "input": "tee", "synonyms": ["t-shirt"]}),
            json!({"type": "synonym", "objectID": "live-syn-1", "synonyms": ["sneaker", "trainer"]}),
        ]
    );
    assert_eq!(
        report_entry_contract(&translated.report.entries),
        vec![
            (
                ReportSeverity::ScopeGap,
                ReportCode::ProductNotMigrated,
                ReportResource::Analytics,
                None,
                None,
                "$".to_string()
            ),
            (
                ReportSeverity::ScopeGap,
                ReportCode::ProductNotMigrated,
                ReportResource::ApiKeys,
                None,
                None,
                "$".to_string()
            ),
            (
                ReportSeverity::ScopeGap,
                ReportCode::ProductNotMigrated,
                ReportResource::Events,
                None,
                None,
                "$".to_string()
            ),
            (
                ReportSeverity::ScopeGap,
                ReportCode::ProductNotMigrated,
                ReportResource::Experiments,
                None,
                None,
                "$".to_string()
            ),
            (
                ReportSeverity::ScopeGap,
                ReportCode::ProductNotMigrated,
                ReportResource::Recommend,
                None,
                None,
                "$".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.attributesToHighlight".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.attributesToSnippet".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.highlightPostTag".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.highlightPreTag".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.hitsPerPage".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::PersistedNoBehaviorSetting,
                ReportResource::Settings,
                None,
                None,
                "$.optionalWords".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::ReadOnlySourceField,
                ReportResource::Settings,
                None,
                None,
                "$.synonyms".to_string()
            ),
            (
                ReportSeverity::Warning,
                ReportCode::ReadOnlySourceField,
                ReportResource::Settings,
                None,
                None,
                "$.version".to_string()
            ),
        ]
    );
    assert_eq!(translated.report.summary.hard_rejections, 0);
    assert_eq!(translated.report.summary.warnings, 8);
    assert_eq!(translated.report.summary.scope_gaps, 5);
    assert_eq!(translated.report.summary.total_entries, 13);
    assert!(translated.report.report_digest.is_some());
}

fn assert_live_mutation_report(
    baseline: &SpoolTranslationInput,
    mutate: impl FnOnce(&mut SpoolTranslationInput),
    code: ReportCode,
    resource: ReportResource,
    page_index: Option<usize>,
    item_index: Option<usize>,
    json_path: &str,
) {
    let mut input = baseline.clone();
    mutate(&mut input);
    let report = rejected(input);
    assert_eq!(
        hard_report_entry_contract(&report),
        vec![(
            ReportSeverity::HardRejection,
            code,
            resource,
            page_index,
            item_index,
            json_path.to_string(),
        )]
    );
}

#[test]
fn live_algolia_translation_fixtures() {
    let Some(baseline) = live_fixture_input_from_env() else {
        println!("SKIPPED: FLAPJACK_TRANSLATION_LIVE_FIXTURES unset");
        return;
    };

    assert_positive_live_translation(baseline.clone());
    assert_live_mutation_report(
        &baseline,
        |input| {
            input.settings["typoTolerance"] = json!("strict");
        },
        ReportCode::UnsupportedSourceField,
        ReportResource::Settings,
        None,
        None,
        "$.typoTolerance",
    );
    assert_live_mutation_report(
        &baseline,
        |input| {
            input.settings["replicas"] = json!(["live-replica"]);
        },
        ReportCode::ReplicaTopologyNotMigrated,
        ReportResource::Settings,
        None,
        None,
        "$.replicas",
    );
    assert_live_mutation_report(
        &baseline,
        |input| {
            input.document_pages[0][0] = json!("not-a-document");
        },
        ReportCode::MalformedDocumentPayload,
        ReportResource::Document,
        Some(0),
        Some(0),
        "$",
    );
    assert_live_mutation_report(
        &baseline,
        |input| {
            input.document_pages[0][0]
                .as_object_mut()
                .unwrap()
                .remove("objectID");
        },
        ReportCode::InvalidObjectId,
        ReportResource::Document,
        Some(0),
        Some(0),
        "$.objectID",
    );
    assert_live_mutation_report(
        &baseline,
        |input| {
            input.document_pages[0][1]["objectID"] = json!("live-doc-2");
        },
        ReportCode::DuplicateObjectId,
        ReportResource::Document,
        Some(0),
        Some(1),
        "$.objectID",
    );
    assert_live_mutation_report(
        &baseline,
        |input| {
            input.rule_pages[0][0]["conditions"][0]["unexpected"] = json!(true);
        },
        ReportCode::UnsupportedRuleSchema,
        ReportResource::Rule,
        Some(0),
        Some(0),
        "$.conditions[0]",
    );
    assert_live_mutation_report(
        &baseline,
        |input| {
            input.synonym_pages[0][0]["unexpected"] = json!(true);
        },
        ReportCode::UnsupportedSynonymSchema,
        ReportResource::Synonym,
        Some(0),
        Some(0),
        "$",
    );

    println!("LIVE_TRANSLATION_PASS=8");
}
