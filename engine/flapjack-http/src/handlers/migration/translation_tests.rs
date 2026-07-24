use super::super::spool::{
    AcceptedSpoolReader, PublicExportView, ResourceDenominators, SpoolError, SpoolLimits,
    SpoolStore,
};
use super::translation_bundle::{translate_replica_settings, translate_replica_topology};
use super::*;
use flapjack::index::replica::ReplicaEntry;
use flapjack::index::settings::DistinctValue;
use flapjack::types::{Document, FieldValue};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

const STAGE1_MATRIX_EXPECTED_DENOMINATOR: usize = 81;
type ReportEntryContract = (
    ReportSeverity,
    ReportCode,
    ReportResource,
    Option<usize>,
    Option<usize>,
    String,
);

const TEST_SOURCE_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct AcceptedSpoolFixture {
    _tmp: TempDir,
    reader: AcceptedSpoolReader,
}

fn create_export_for_test(
    store: &SpoolStore,
    job_uuid: uuid::Uuid,
    source_identity_digest: &str,
    denominators: ResourceDenominators,
) -> Result<PublicExportView, SpoolError> {
    store.create_migration_phase(job_uuid)?;
    store.create_export(job_uuid, source_identity_digest, denominators)
}

fn spool_payload(
    settings: serde_json::Value,
    document_pages: Vec<Vec<serde_json::Value>>,
    rule_pages: Vec<Vec<serde_json::Value>>,
    synonym_pages: Vec<Vec<serde_json::Value>>,
) -> SpoolTranslationInput {
    SpoolTranslationInput {
        source_index_name: "products".to_string(),
        target_index_name: "shop".to_string(),
        settings,
        document_pages,
        rule_pages,
        synonym_pages,
        replica_settings: BTreeMap::new(),
    }
}

fn accepted_spool_fixture(
    settings: serde_json::Value,
    document_pages: Vec<Vec<serde_json::Value>>,
    rule_pages: Vec<Vec<serde_json::Value>>,
    synonym_pages: Vec<Vec<serde_json::Value>>,
) -> AcceptedSpoolFixture {
    let tmp = TempDir::new().unwrap();
    let store = SpoolStore::new(tmp.path(), SpoolLimits::default()).unwrap();
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        TEST_SOURCE_DIGEST,
        denominators_for_pages(&document_pages, &rule_pages, &synonym_pages),
    )
    .unwrap();

    let settings_bytes = serde_json::to_vec(&settings).unwrap();
    store
        .commit_settings_once(view.job_uuid, &settings_bytes, TEST_SOURCE_DIGEST)
        .unwrap();
    for page in &document_pages {
        let ids = object_ids(page);
        let id_refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
        store
            .commit_document_page_with_ids(
                view.job_uuid,
                &serde_json::to_vec(page).unwrap(),
                &id_refs,
            )
            .unwrap();
    }
    for page in &rule_pages {
        let ids = object_ids(page);
        let id_refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
        store
            .commit_rule_page_with_ids(view.job_uuid, &serde_json::to_vec(page).unwrap(), &id_refs)
            .unwrap();
    }
    for page in &synonym_pages {
        let ids = object_ids(page);
        let id_refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
        store
            .commit_synonym_page_with_ids(
                view.job_uuid,
                &serde_json::to_vec(page).unwrap(),
                &id_refs,
            )
            .unwrap();
    }

    store
        .complete_documents(
            view.job_uuid,
            count_pages(&document_pages),
            TEST_SOURCE_DIGEST,
        )
        .unwrap();
    store
        .complete_rules(view.job_uuid, count_pages(&rule_pages), TEST_SOURCE_DIGEST)
        .unwrap();
    store
        .complete_synonyms(
            view.job_uuid,
            count_pages(&synonym_pages),
            TEST_SOURCE_DIGEST,
        )
        .unwrap();
    store.accept_export(view.job_uuid).unwrap();

    AcceptedSpoolFixture {
        reader: store.accepted_artifacts(view.job_uuid).unwrap(),
        _tmp: tmp,
    }
}

fn denominators_for_pages(
    document_pages: &[Vec<serde_json::Value>],
    rule_pages: &[Vec<serde_json::Value>],
    synonym_pages: &[Vec<serde_json::Value>],
) -> ResourceDenominators {
    ResourceDenominators {
        settings: 1,
        documents: count_pages(document_pages),
        rules: count_pages(rule_pages),
        synonyms: count_pages(synonym_pages),
        config: 0,
    }
}

fn count_pages(pages: &[Vec<serde_json::Value>]) -> u64 {
    pages.iter().map(|page| page.len() as u64).sum()
}

fn object_ids(page: &[serde_json::Value]) -> Vec<String> {
    page.iter()
        .map(|item| {
            item.get("objectID")
                .and_then(serde_json::Value::as_str)
                .expect("accepted spool fixture items need objectID values")
                .to_string()
        })
        .collect()
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

fn document_page(start: usize, count: usize) -> Vec<serde_json::Value> {
    (start..start + count)
        .map(|index| {
            json!({
                "objectID": format!("doc-{index:04}"),
                "title": format!("Document {index}"),
                "page_marker": start,
                "score": index as i64
            })
        })
        .collect()
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

fn translated_documents(translated: &TranslatedSpoolPayload) -> Vec<&Document> {
    translated.document_batches.iter().flatten().collect()
}

mod replica_topology_translation {
    use super::*;

    #[test]
    fn standard_and_virtual_source_entries_derive_virtual_targets_with_source_kind() {
        let translated = translate_replica_topology(
            &json!({
                "replicas": [
                    "products_price_asc",
                    "virtual(products_relevance)"
                ]
            }),
            "products",
            "shop",
        )
        .expect("valid replica topology should translate");

        assert_eq!(
            translated,
            vec![
                super::super::translation_bundle::ReplicaTopologyTranslation {
                    source_entry: ReplicaEntry::Standard("products_price_asc".to_string()),
                    source_replica_name: "products_price_asc".to_string(),
                    derived_entry: ReplicaEntry::Virtual("shop_price_asc".to_string()),
                },
                super::super::translation_bundle::ReplicaTopologyTranslation {
                    source_entry: ReplicaEntry::Virtual("products_relevance".to_string()),
                    source_replica_name: "products_relevance".to_string(),
                    derived_entry: ReplicaEntry::Virtual("shop_relevance".to_string()),
                },
            ]
        );
    }

    #[test]
    fn prefix_replacement_is_byte_exact_case_sensitive_and_separator_preserving() {
        let translated = translate_replica_topology(
            &json!({
                "replicas": [
                    "prod_price_asc",
                    "prod-price-desc",
                    "production_x",
                    "Prod_x",
                    "shop_prod_y",
                    "aprod"
                ]
            }),
            "prod",
            "shop",
        )
        .expect("valid boundary topology should translate");

        assert_eq!(
            derived_virtual_names(&translated),
            vec![
                "shop_price_asc",
                "shop-price-desc",
                "production_x",
                "Prod_x",
                "shop_prod_y",
                "aprod",
            ]
        );
    }

    #[test]
    fn validation_error_names_self_referencing_derived_target() {
        let error = translate_replica_topology(&json!({"replicas": ["shop"]}), "products", "shop")
            .expect_err("derived self-reference must be rejected by replica validation");

        assert_eq!(error.derived_target_name, "shop");
        assert!(error.colliding_source_replica_names.is_empty());
    }

    #[test]
    fn validation_error_rejects_source_self_references_before_derivation() {
        for source_replica in ["products", "virtual(products)"] {
            let error = translate_replica_topology(
                &json!({"replicas": [source_replica]}),
                "products",
                "shop",
            )
            .expect_err("source self-reference must be rejected by replica validation");

            assert_eq!(error.derived_target_name, "products");
            assert!(error.colliding_source_replica_names.is_empty());
        }
    }

    #[test]
    fn validation_error_names_source_duplicate_before_derivation() {
        let error = translate_replica_topology(
            &json!({"replicas": ["products_price_asc", "virtual(products_price_asc)"]}),
            "products",
            "shop",
        )
        .expect_err("source duplicate names must be rejected before derivation");

        assert_eq!(error.derived_target_name, "products_price_asc");
        assert!(error.colliding_source_replica_names.is_empty());
    }

    #[test]
    fn duplicate_derived_name_error_names_both_source_replicas() {
        let error = translate_replica_topology(
            &json!({"replicas": ["products_price_asc", "shop_price_asc"]}),
            "products",
            "shop",
        )
        .expect_err("duplicate derived names must be rejected by replica validation");

        assert_eq!(error.derived_target_name, "shop_price_asc");
        assert_eq!(
            error.colliding_source_replica_names,
            vec!["products_price_asc", "shop_price_asc"]
        );
    }

    /// A collision that makes topology untranslatable must reach the report with
    /// the offending source replica names in the path — `ReplicaTopologyNotMigrated`
    /// is still the owner of that case after Stage 4 activation, and the actionable
    /// names Stage 2 computes must not be flattened into a bare `$.replicas`.
    #[test]
    fn untranslatable_topology_collision_reports_both_source_replicas() {
        let report = rejected(spool_payload(
            json!({"replicas": ["products_price_asc", "shop_price_asc"]}),
            vec![],
            vec![],
            vec![],
        ));

        assert!(hard_codes(&report).contains(&ReportCode::ReplicaTopologyNotMigrated));
        let paths = report
            .entries
            .iter()
            .filter(|entry| entry.code == ReportCode::ReplicaTopologyNotMigrated)
            .map(|entry| entry.json_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                r#"$.replicas["products_price_asc"]"#,
                r#"$.replicas["shop_price_asc"]"#
            ]
        );
    }

    fn derived_virtual_names(
        translated: &[super::super::translation_bundle::ReplicaTopologyTranslation],
    ) -> Vec<&str> {
        translated
            .iter()
            .map(|entry| match &entry.derived_entry {
                ReplicaEntry::Virtual(name) => name.as_str(),
                ReplicaEntry::Standard(name) => panic!("derived entry must be virtual: {name}"),
            })
            .collect()
    }
}

mod replica_ranking_translation {
    use super::super::translation_bundle::{translate_settings, MATCHING_CRITICAL_REPLICA_FIELDS};
    use super::super::translation_report::warning_message;
    use super::*;

    #[test]
    fn warning_message_contract_maps_only_warning_codes() {
        let cases = [
            (
                ReportCode::ProductNotMigrated,
                None,
            ),
            (
                ReportCode::PersistedNoBehaviorSetting,
                Some("Source setting is preserved for compatibility but has no Flapjack behavior."),
            ),
            (
                ReportCode::ReadOnlySourceField,
                Some("Source field is read-only in Flapjack and is not applied during migration."),
            ),
            (
                ReportCode::ReplicaTopologyNotMigrated,
                Some("Replica topology contains an entry that cannot be translated, such as a malformed, self-referential, or colliding replica."),
            ),
            (
                ReportCode::UnsupportedSourceField,
                None,
            ),
            (
                ReportCode::UnsupportedRuleSchema,
                None,
            ),
            (
                ReportCode::UnsupportedSynonymSchema,
                None,
            ),
            (
                ReportCode::InvalidObjectId,
                None,
            ),
            (
                ReportCode::DuplicateObjectId,
                None,
            ),
            (
                ReportCode::MalformedSettingsPayload,
                None,
            ),
            (
                ReportCode::MalformedDocumentPayload,
                None,
            ),
            (
                ReportCode::MalformedRulePayload,
                None,
            ),
            (
                ReportCode::MalformedSynonymPayload,
                None,
            ),
            (
                ReportCode::ReplicaUnknownRankingToken,
                Some("Replica ranking token is not recognized by Flapjack and was ignored."),
            ),
            (
                ReportCode::ReplicaExhaustiveSortApproximated,
                Some("Algolia standard replica exhaustive sorting is approximated as a Flapjack virtual replica."),
            ),
            (
                ReportCode::ReplicaPrimaryRelevancyStrictnessDropped,
                Some("Primary relevancyStrictness is not applied to translated replica settings."),
            ),
            (
                ReportCode::ReplicaRelevancyStrictnessSemanticMismatch,
                Some("Algolia relevancyStrictness semantics differ from Flapjack deterministic-query ranking and may not produce identical ordering."),
            ),
            (
                ReportCode::ReplicaMatchingCriticalFieldDiverges,
                Some("Replica setting changes matching-critical behavior that virtual replicas cannot independently reproduce."),
            ),
        ];

        for (code, expected) in cases {
            assert_eq!(warning_message(code), expected, "{code:?}");
        }
    }

    #[test]
    fn matching_critical_field_inventory_is_canonical() {
        assert_eq!(
            MATCHING_CRITICAL_REPLICA_FIELDS,
            [
                "attributesForFaceting",
                "camelCaseAttributes",
                "customNormalization",
                "decompoundedAttributes",
                "disableExactOnAttributes",
                "disablePrefixOnAttributes",
                "disableTypoToleranceOnAttributes",
                "disableTypoToleranceOnWords",
                "indexLanguages",
                "keepDiacriticsOnCharacters",
                "numericAttributesForFiltering",
                "optionalWords",
                "searchableAttributes",
                "separatorsToIndex",
            ]
        );
    }

    #[test]
    fn g1_standard_replica_preserves_ranking_and_reports_exhaustive_sort_approximation() {
        let primary = json!({
            "replicas": ["products_price_desc"],
            "ranking": ["typo"],
            "customRanking": ["desc(primary_only)"]
        });
        let topology = replica_topology(&primary);
        let carried = BTreeMap::from([(
            "products_price_desc".to_string(),
            json!({
                "ranking": ["desc(price)", "typo", "custom"],
                "customRanking": ["asc(popularity)"]
            }),
        )]);

        let translated = translate_replica_settings(&primary, &carried, &topology);

        assert_eq!(
            translated
                .replicas
                .iter()
                .map(|replica| (
                    replica.source_name.as_str(),
                    replica.source_entry.clone(),
                    replica.derived_entry.clone(),
                    replica.settings.ranking.clone(),
                    replica.settings.custom_ranking.clone(),
                ))
                .collect::<Vec<_>>(),
            vec![(
                "products_price_desc",
                ReplicaEntry::Standard("products_price_desc".to_string()),
                ReplicaEntry::Virtual("shop_price_desc".to_string()),
                Some(vec!["typo".to_string()]),
                Some(vec![
                    "desc(price)".to_string(),
                    "asc(popularity)".to_string(),
                ]),
            )]
        );
        assert_eq!(
            report_entry_contract(&translated.report_entries),
            vec![
                warning_contract(
                    ReportCode::ReplicaExhaustiveSortApproximated,
                    "$.replicas[0]",
                ),
                warning_contract(
                    ReportCode::ReplicaRelevancyStrictnessSemanticMismatch,
                    r#"$.replicaSettings["products_price_desc"].relevancyStrictness"#,
                ),
            ]
        );
    }

    #[test]
    fn g2_numeric_filter_alias_divergence_preserves_value_and_reports_canonical_path() {
        let primary = json!({
            "replicas": ["virtual(products_price_desc)"],
            "ranking": ["typo"],
            "numericAttributesForFiltering": ["price"]
        });
        let topology = replica_topology(&primary);
        let carried = BTreeMap::from([(
            "products_price_desc".to_string(),
            json!({
                "ranking": ["typo"],
                "numericAttributesToIndex": ["inventory"]
            }),
        )]);

        let translated = translate_replica_settings(&primary, &carried, &topology);

        assert_eq!(translated.replicas.len(), 1);
        let replica = &translated.replicas[0];
        assert_eq!(replica.source_name, "products_price_desc");
        assert_eq!(
            replica.derived_entry,
            ReplicaEntry::Virtual("shop_price_desc".to_string())
        );
        assert_eq!(replica.settings.ranking, Some(vec!["typo".to_string()]));
        assert_eq!(replica.settings.custom_ranking, None);
        assert_eq!(
            replica.settings.numeric_attributes_for_filtering,
            Some(vec!["inventory".to_string()])
        );
        assert_eq!(
            report_entry_contract(&translated.report_entries),
            vec![
                warning_contract(
                    ReportCode::ReplicaRelevancyStrictnessSemanticMismatch,
                    r#"$.replicaSettings["products_price_desc"].relevancyStrictness"#,
                ),
                warning_contract(
                    ReportCode::ReplicaMatchingCriticalFieldDiverges,
                    r#"$.replicaSettings["products_price_desc"].numericAttributesForFiltering"#,
                ),
            ]
        );
    }

    #[test]
    fn g3_strictness_preserves_replica_value_and_reports_primary_and_semantic_gaps() {
        let primary = json!({
            "replicas": ["virtual(products_relevance)"],
            "ranking": ["typo"],
            "relevancyStrictness": 90
        });
        let topology = replica_topology(&primary);
        let carried = BTreeMap::from([(
            "products_relevance".to_string(),
            json!({
                "ranking": ["typo"],
                "relevancyStrictness": 80
            }),
        )]);

        let mut primary_failures = Vec::new();
        let translated_primary = translate_settings(&primary, &mut primary_failures).unwrap();
        assert!(primary_failures.is_empty());
        assert_eq!(translated_primary.relevancy_strictness, None);

        let translated = translate_replica_settings(&primary, &carried, &topology);

        assert_eq!(translated.replicas.len(), 1);
        let replica = &translated.replicas[0];
        assert_eq!(replica.settings.relevancy_strictness, Some(80));
        assert_eq!(replica.source_relevancy_strictness, Some(80));
        assert_eq!(
            report_entry_contract(&translated.report_entries),
            vec![
                warning_contract(
                    ReportCode::ReplicaPrimaryRelevancyStrictnessDropped,
                    "$.relevancyStrictness",
                ),
                warning_contract(
                    ReportCode::ReplicaRelevancyStrictnessSemanticMismatch,
                    r#"$.replicaSettings["products_relevance"].relevancyStrictness"#,
                ),
            ]
        );
    }

    #[test]
    fn standard_and_virtual_replicas_use_their_own_carried_ranking_settings() {
        let primary = json!({
            "replicas": ["products_price_desc", "virtual(products_relevance)"],
            "ranking": ["typo"],
            "customRanking": ["desc(primary_only)"],
            "searchableAttributes": ["title"]
        });
        let topology = replica_topology(&primary);
        let carried = BTreeMap::from([
            (
                "products_price_desc".to_string(),
                json!({
                    "ranking": ["desc(price)", "typo", "geo", "words", "filters", "proximity", "attribute", "exact", "custom"],
                    "customRanking": ["asc(name)"],
                    "searchableAttributes": ["sku"]
                }),
            ),
            (
                "products_relevance".to_string(),
                json!({
                    "ranking": ["asc(popularity)", "exact", "attribute", "words", "custom"],
                    "customRanking": ["desc(updated_at)"],
                    "searchableAttributes": ["title"]
                }),
            ),
        ]);

        let translated = translate_replica_settings(&primary, &carried, &topology);

        assert_eq!(
            report_entry_contract(&translated.report_entries),
            vec![
                warning_contract(
                    ReportCode::ReplicaExhaustiveSortApproximated,
                    "$.replicas[0]",
                ),
                warning_contract(
                    ReportCode::ReplicaRelevancyStrictnessSemanticMismatch,
                    r#"$.replicaSettings["products_price_desc"].relevancyStrictness"#,
                ),
                warning_contract(
                    ReportCode::ReplicaMatchingCriticalFieldDiverges,
                    r#"$.replicaSettings["products_price_desc"].searchableAttributes"#,
                ),
                warning_contract(
                    ReportCode::ReplicaRelevancyStrictnessSemanticMismatch,
                    r#"$.replicaSettings["products_relevance"].relevancyStrictness"#,
                ),
            ]
        );
        assert_eq!(
            translated
                .replicas
                .iter()
                .map(|replica| (
                    replica.source_name.as_str(),
                    replica.source_entry.clone(),
                    replica.derived_entry.clone(),
                    replica.settings.ranking.clone(),
                    replica.settings.custom_ranking.clone(),
                    replica.source_relevancy_strictness,
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "products_price_desc",
                    ReplicaEntry::Standard("products_price_desc".to_string()),
                    ReplicaEntry::Virtual("shop_price_desc".to_string()),
                    Some(vec![
                        "typo".to_string(),
                        "geo".to_string(),
                        "words".to_string(),
                        "filters".to_string(),
                        "proximity".to_string(),
                        "attribute".to_string(),
                        "exact".to_string(),
                    ]),
                    Some(vec!["desc(price)".to_string(), "asc(name)".to_string()]),
                    None,
                ),
                (
                    "products_relevance",
                    ReplicaEntry::Virtual("products_relevance".to_string()),
                    ReplicaEntry::Virtual("shop_relevance".to_string()),
                    Some(vec![
                        "exact".to_string(),
                        "attribute".to_string(),
                        "words".to_string(),
                    ]),
                    Some(vec![
                        "asc(popularity)".to_string(),
                        "desc(updated_at)".to_string(),
                    ]),
                    None,
                ),
            ]
        );
    }

    #[test]
    fn exhaustive_sort_ranking_lifts_desc_price_first() {
        let primary = ranking_primary_settings();
        let topology = replica_topology(&primary);
        let carried = BTreeMap::from([
            (
                "products_price_desc".to_string(),
                json!({
                    "ranking": ["desc(price)", "asc(popularity)", "words", "exact", "custom"],
                    "customRanking": ["desc(updated_at)"]
                }),
            ),
            (
                "products_relevance".to_string(),
                json!({"ranking": ["typo"]}),
            ),
        ]);

        let translated = translate_replica_settings(&primary, &carried, &topology);
        let price_replica = translated
            .replicas
            .iter()
            .find(|replica| replica.source_name == "products_price_desc")
            .unwrap();

        assert_eq!(
            price_replica.settings.custom_ranking.as_deref(),
            Some(
                [
                    "desc(price)".to_string(),
                    "asc(popularity)".to_string(),
                    "desc(updated_at)".to_string(),
                ]
                .as_slice()
            )
        );
    }

    #[test]
    fn custom_ranking_only_survives_when_source_ranking_keeps_custom() {
        let primary = ranking_primary_settings();
        let topology = replica_topology(&primary);
        let carried = BTreeMap::from([
            (
                "products_price_desc".to_string(),
                json!({
                    "ranking": ["typo", "custom"],
                    "customRanking": ["desc(price)"]
                }),
            ),
            (
                "products_relevance".to_string(),
                json!({
                    "ranking": ["typo"],
                    "customRanking": ["desc(updated_at)"]
                }),
            ),
        ]);

        let translated = translate_replica_settings(&primary, &carried, &topology);

        assert!(entries_for_code_in(
            &translated.report_entries,
            ReportCode::ReplicaUnknownRankingToken,
        )
        .is_empty());
        assert_eq!(
            translated
                .replicas
                .iter()
                .map(|replica| (
                    replica.source_name.as_str(),
                    replica.settings.ranking.clone(),
                    replica.settings.custom_ranking.clone(),
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "products_price_desc",
                    Some(vec!["typo".to_string()]),
                    Some(vec!["desc(price)".to_string()]),
                ),
                ("products_relevance", Some(vec!["typo".to_string()]), None,),
            ]
        );
    }

    #[test]
    fn matching_critical_divergence_uses_normalized_settings_semantics() {
        let primary = json!({
            "replicas": ["products_price_desc", "virtual(products_relevance)"],
            "ranking": ["typo"],
            "customRanking": ["desc(primary_only)"]
        });
        let topology = replica_topology(&primary);
        let carried = BTreeMap::from([
            (
                "products_price_desc".to_string(),
                json!({
                    "ranking": ["typo"],
                    "attributesForFaceting": [],
                    "camelCaseAttributes": [],
                    "customNormalization": null,
                    "decompoundedAttributes": null,
                    "disableExactOnAttributes": null,
                    "disablePrefixOnAttributes": null,
                    "disableTypoToleranceOnAttributes": null,
                    "disableTypoToleranceOnWords": null,
                    "indexLanguages": [],
                    "keepDiacriticsOnCharacters": "",
                    "numericAttributesForFiltering": null,
                    "optionalWords": [],
                    "searchableAttributes": null,
                    "separatorsToIndex": ""
                }),
            ),
            (
                "products_relevance".to_string(),
                json!({
                    "ranking": ["typo"],
                    "searchableAttributes": ["title"]
                }),
            ),
        ]);

        let translated = translate_replica_settings(&primary, &carried, &topology);
        let paths = entries_for_code_in(
            &translated.report_entries,
            ReportCode::ReplicaMatchingCriticalFieldDiverges,
        )
        .into_iter()
        .map(|entry| entry.json_path.as_str())
        .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![r#"$.replicaSettings["products_relevance"].searchableAttributes"#]
        );
    }

    #[test]
    fn numeric_filtering_legacy_alias_warns_at_canonical_matching_path() {
        let primary = json!({
            "replicas": ["virtual(products_price_desc)"],
            "ranking": ["typo"],
            "numericAttributesToIndex": ["price"]
        });
        let topology = replica_topology(&primary);
        let carried = BTreeMap::from([(
            "products_price_desc".to_string(),
            json!({
                "ranking": ["typo"],
                "numericAttributesToIndex": ["inventory"]
            }),
        )]);

        let translated = translate_replica_settings(&primary, &carried, &topology);
        let paths = entries_for_code_in(
            &translated.report_entries,
            ReportCode::ReplicaMatchingCriticalFieldDiverges,
        )
        .into_iter()
        .map(|entry| entry.json_path.as_str())
        .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![r#"$.replicaSettings["products_price_desc"].numericAttributesForFiltering"#]
        );
    }

    #[test]
    fn report_codes_and_paths_cover_ranking_and_fidelity_limits() {
        let primary = json!({
            "replicas": ["products_price_desc", "virtual(products_relevance)"],
            "ranking": ["typo"],
            "customRanking": ["desc(primary_only)"],
            "relevancyStrictness": 90,
            "searchableAttributes": ["title"],
            "attributesForFaceting": ["brand"],
            "attributeForDistinct": "sku",
            "userData": {"owner": "primary"}
        });
        let topology = replica_topology(&primary);
        let mut carried = complete_replica_settings();
        carried.insert(
            "products_price_desc".to_string(),
            json!({
                "ranking": ["bogus", "desc(price)", "another_unknown"],
                "relevancyStrictness": 80,
                "searchableAttributes": ["sku"],
                "attributesForFaceting": ["brand", "category"],
                "attributeForDistinct": "variant",
                "userData": {"owner": "replica"}
            }),
        );
        carried.insert(
            "products_relevance".to_string(),
            json!({
                "ranking": ["typo"],
                "searchableAttributes": ["title"],
                "attributesForFaceting": ["brand"]
            }),
        );

        let translated = translate_replica_settings(&primary, &carried, &topology);

        assert_eq!(
            report_entry_contract(&translated.report_entries),
            vec![
                warning_contract(
                    ReportCode::ReplicaPrimaryRelevancyStrictnessDropped,
                    "$.relevancyStrictness",
                ),
                warning_contract(
                    ReportCode::ReplicaExhaustiveSortApproximated,
                    "$.replicas[0]",
                ),
                warning_contract(
                    ReportCode::ReplicaRelevancyStrictnessSemanticMismatch,
                    r#"$.replicaSettings["products_price_desc"].relevancyStrictness"#,
                ),
                warning_contract(
                    ReportCode::ReplicaMatchingCriticalFieldDiverges,
                    r#"$.replicaSettings["products_price_desc"].attributesForFaceting"#,
                ),
                warning_contract(
                    ReportCode::ReplicaMatchingCriticalFieldDiverges,
                    r#"$.replicaSettings["products_price_desc"].searchableAttributes"#,
                ),
                warning_contract(
                    ReportCode::ReplicaUnknownRankingToken,
                    r#"$.replicaSettings["products_price_desc"].ranking[0]"#,
                ),
                warning_contract(
                    ReportCode::ReplicaUnknownRankingToken,
                    r#"$.replicaSettings["products_price_desc"].ranking[2]"#,
                ),
                warning_contract(
                    ReportCode::ReplicaRelevancyStrictnessSemanticMismatch,
                    r#"$.replicaSettings["products_relevance"].relevancyStrictness"#,
                ),
            ]
        );

        for entry in translated.report_entries {
            assert!(
                warning_message(entry.code).is_some(),
                "warning code must have canonical prose: {:?}",
                entry.code
            );
        }
    }

    #[test]
    fn matching_critical_field_warnings_are_parameterized_from_inventory() {
        let primary = primary_matching_critical_settings();
        let topology = replica_topology(&primary);
        let replica_fields = MATCHING_CRITICAL_REPLICA_FIELDS
            .iter()
            .map(|field| ((*field).to_string(), json!([format!("replica_{field}")])))
            .collect::<serde_json::Map<_, _>>();
        let carried = BTreeMap::from([
            (
                "products_price_desc".to_string(),
                serde_json::Value::Object(replica_fields),
            ),
            (
                "products_relevance".to_string(),
                json!({"ranking": ["typo"]}),
            ),
        ]);

        let translated = translate_replica_settings(&primary, &carried, &topology);
        let paths = entries_for_code_in(
            &translated.report_entries,
            ReportCode::ReplicaMatchingCriticalFieldDiverges,
        )
        .into_iter()
        .map(|entry| entry.json_path.as_str())
        .collect::<Vec<_>>();

        assert_eq!(
            paths,
            MATCHING_CRITICAL_REPLICA_FIELDS
                .iter()
                .map(|field| format!(r#"$.replicaSettings["products_price_desc"].{field}"#))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn omitted_replica_matching_critical_field_warns_against_non_default_primary() {
        let primary = json!({
            "replicas": ["virtual(products_price_desc)"],
            "ranking": ["typo"],
            "searchableAttributes": ["title"]
        });
        let topology = replica_topology(&primary);
        let carried = BTreeMap::from([(
            "products_price_desc".to_string(),
            json!({"ranking": ["typo"]}),
        )]);

        let translated = translate_replica_settings(&primary, &carried, &topology);
        let paths = entries_for_code_in(
            &translated.report_entries,
            ReportCode::ReplicaMatchingCriticalFieldDiverges,
        )
        .into_iter()
        .map(|entry| entry.json_path.as_str())
        .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![r#"$.replicaSettings["products_price_desc"].searchableAttributes"#]
        );
    }

    #[test]
    fn empty_optional_matching_critical_lists_match_omitted_defaults() {
        let primary = json!({
            "replicas": ["virtual(products_price_desc)"],
            "ranking": ["typo"]
        });
        let topology = replica_topology(&primary);
        let carried = BTreeMap::from([(
            "products_price_desc".to_string(),
            json!({
                "ranking": ["typo"],
                "disableExactOnAttributes": [],
                "disableTypoToleranceOnAttributes": [],
                "disableTypoToleranceOnWords": []
            }),
        )]);

        let translated = translate_replica_settings(&primary, &carried, &topology);

        assert!(entries_for_code_in(
            &translated.report_entries,
            ReportCode::ReplicaMatchingCriticalFieldDiverges,
        )
        .is_empty());
    }

    #[test]
    fn missing_or_malformed_carried_replica_settings_fail_closed() {
        let primary = ranking_primary_settings();
        let topology = replica_topology(&primary);
        let carried = BTreeMap::from([(
            "products_price_desc".to_string(),
            json!({"ranking": "not an array"}),
        )]);

        let translated = translate_replica_settings(&primary, &carried, &topology);

        assert!(translated.replicas.is_empty());
        assert_eq!(
            report_entry_contract(&translated.report_entries),
            vec![
                warning_contract(
                    ReportCode::ReplicaExhaustiveSortApproximated,
                    "$.replicas[0]",
                ),
                warning_contract(
                    ReportCode::ReplicaRelevancyStrictnessSemanticMismatch,
                    r#"$.replicaSettings["products_price_desc"].relevancyStrictness"#,
                ),
                hard_contract(
                    ReportCode::MalformedSettingsPayload,
                    r#"$.replicaSettings["products_price_desc"].ranking"#,
                ),
                hard_contract(
                    ReportCode::MalformedSettingsPayload,
                    r#"$.replicaSettings["products_relevance"]"#,
                ),
            ]
        );
    }

    fn ranking_primary_settings() -> serde_json::Value {
        json!({
            "replicas": ["products_price_desc", "virtual(products_relevance)"],
            "ranking": ["typo"],
            "customRanking": ["desc(primary_only)"]
        })
    }

    fn primary_matching_critical_settings() -> serde_json::Value {
        json!({
            "replicas": ["virtual(products_price_desc)"],
            "ranking": ["typo"],
            "attributesForFaceting": ["brand"],
            "camelCaseAttributes": ["primaryCamel"],
            "customNormalization": {"default": {"é": "e"}},
            "decompoundedAttributes": {"de": ["primary"]},
            "disableExactOnAttributes": ["description"],
            "disablePrefixOnAttributes": ["sku"],
            "disableTypoToleranceOnAttributes": ["brand"],
            "disableTypoToleranceOnWords": ["serial"],
            "indexLanguages": ["en"],
            "keepDiacriticsOnCharacters": "é",
            "numericAttributesForFiltering": ["price"],
            "optionalWords": ["sale"],
            "searchableAttributes": ["title"],
            "separatorsToIndex": "-"
        })
    }

    fn complete_replica_settings() -> BTreeMap<String, serde_json::Value> {
        BTreeMap::from([
            (
                "products_price_desc".to_string(),
                json!({
                    "ranking": ["desc(price)", "typo"],
                    "customRanking": ["asc(name)"],
                    "relevancyStrictness": 80,
                    "searchableAttributes": ["sku"]
                }),
            ),
            (
                "products_relevance".to_string(),
                json!({
                    "ranking": ["asc(popularity)", "exact"],
                    "customRanking": ["desc(updated_at)"],
                    "relevancyStrictness": 50
                }),
            ),
        ])
    }

    fn replica_topology(
        primary_settings: &serde_json::Value,
    ) -> Vec<super::super::translation_bundle::ReplicaTopologyTranslation> {
        translate_replica_topology(primary_settings, "products", "shop").unwrap()
    }

    fn warning_contract(code: ReportCode, path: &str) -> ReportEntryContract {
        (
            ReportSeverity::Warning,
            code,
            ReportResource::Settings,
            None,
            None,
            path.to_string(),
        )
    }

    fn hard_contract(code: ReportCode, path: &str) -> ReportEntryContract {
        (
            ReportSeverity::HardRejection,
            code,
            ReportResource::Settings,
            None,
            None,
            path.to_string(),
        )
    }

    fn entries_for_code_in(
        entries: &[TranslationReportEntry],
        code: ReportCode,
    ) -> Vec<&TranslationReportEntry> {
        entries.iter().filter(|entry| entry.code == code).collect()
    }
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
    let unique_matchers: HashSet<_> = actual.iter().copied().collect();
    assert_eq!(
        unique_matchers.len(),
        actual.len(),
        "each resource matcher must have exactly one compatibility row"
    );

    let mut expected = Vec::new();
    expected.extend(
        [
            "attributesForFaceting",
            "searchableAttributes",
            "attributesToIndex",
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

// Stage 4 flips the `replicas` and `relevancyStrictness` topology rows from hard
// rejections to migrated/translated rows. The two tests below assert that
// post-Stage-4 contract and are therefore INTENTIONALLY RED until Stage 4 lands.
// They must never be "fixed" by weakening the assertions or flipping a production
// matrix row in this stage — they are the falsifiable proof that Stage 1 did not
// silently activate replica migration.
#[test]
fn replicas_matrix_row_remains_red_until_stage4() {
    let row = resolve_source_field(ResourceKind::Settings, "replicas");

    assert_ne!(
        row.disposition,
        Disposition::Rejected,
        "Stage 4 must migrate replica topology instead of hard-rejecting it"
    );
    assert_ne!(
        row.target_owner,
        TargetOwner::TranslationReport,
        "a migrated replicas row is owned by settings translation, not the report"
    );
    assert_eq!(
        row.rejection_code, None,
        "a migrated replicas row carries no rejection code"
    );
    assert_eq!(row.owner_path_precondition, OwnerPathPrecondition::None);
    let _ = ReportCode::ReplicaTopologyNotMigrated;
}

#[test]
fn relevancy_strictness_matrix_row_remains_red_until_stage4() {
    let row = resolve_source_field(ResourceKind::Settings, "relevancyStrictness");

    assert_ne!(
        row.disposition,
        Disposition::Rejected,
        "Stage 4 must migrate relevancyStrictness instead of hard-rejecting it"
    );
    assert_ne!(
        row.target_owner,
        TargetOwner::TranslationReport,
        "a migrated relevancyStrictness row is owned by settings translation, not the report"
    );
    assert_eq!(
        row.rejection_code, None,
        "a migrated relevancyStrictness row carries no rejection code"
    );
    assert_eq!(row.owner_path_precondition, OwnerPathPrecondition::None);
    let _ = ReportCode::ReplicaTopologyNotMigrated;
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
    let translated = translated(complete_spool_payload());

    assert_complete_spool_documents(&translated);
    assert_complete_spool_settings(&translated);
    assert_complete_spool_replica_settings(&translated);
    assert_complete_spool_rules_and_synonyms(&translated);
    assert_complete_spool_report(&translated);
}

fn complete_spool_payload() -> SpoolTranslationInput {
    let mut payload = spool_payload(
        json!({
            "searchableAttributes": ["title", "brand"],
            "attributesForFaceting": ["brand", "category"],
            "attributesToRetrieve": ["title", "private_cost"],
            "unretrievableAttributes": ["private_cost"],
            "attributeForDistinct": "sku",
            "distinct": 2,
            "numericAttributesToIndex": ["price", "inventory"],
            "allowCompressionOfIntegerArray": true,
            "replicas": ["products_price_desc", "virtual(products_relevance)"],
            "relevancyStrictness": 90
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
    payload.replica_settings = BTreeMap::from([
        (
            "products_price_desc".to_string(),
            json!({
                "ranking": ["desc(price)", "typo"],
                "customRanking": ["asc(name)"],
                "relevancyStrictness": 80
            }),
        ),
        (
            "products_relevance".to_string(),
            json!({
                "ranking": ["asc(popularity)", "exact"],
                "relevancyStrictness": 50
            }),
        ),
    ]);
    payload
}

fn assert_complete_spool_documents(translated: &TranslatedSpoolPayload) {
    assert_eq!(
        translated_documents(translated)
            .into_iter()
            .map(|document| document.id.as_str())
            .collect::<Vec<_>>(),
        vec!["doc-1", "doc-2", "doc-3"]
    );
    assert_eq!(
        translated_documents(translated)[0].fields["title"],
        FieldValue::Text("Trail Shoe".to_string())
    );
    assert_eq!(
        translated_documents(translated)[0].fields["private_cost"],
        FieldValue::Integer(42)
    );
}

fn assert_complete_spool_settings(translated: &TranslatedSpoolPayload) {
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
    assert_eq!(
        translated.bundle.settings.replicas,
        Some(vec![
            "virtual(shop_price_desc)".to_string(),
            "virtual(shop_relevance)".to_string()
        ])
    );
    assert!(translated.bundle.settings.relevancy_strictness.is_none());
}

fn assert_complete_spool_replica_settings(translated: &TranslatedSpoolPayload) {
    assert_eq!(translated.bundle.replica_settings.len(), 2);
    assert_eq!(
        translated.bundle.replica_settings[0].source_name,
        "products_price_desc"
    );
    assert!(matches!(
        &translated.bundle.replica_settings[0].derived_entry,
        ReplicaEntry::Virtual(name) if name == "shop_price_desc"
    ));
    assert_eq!(
        translated.bundle.replica_settings[0]
            .settings
            .custom_ranking,
        Some(vec!["desc(price)".to_string()])
    );
    assert_eq!(
        translated.bundle.replica_settings[0].source_relevancy_strictness,
        Some(80)
    );
    assert_eq!(
        translated.bundle.replica_settings[1].source_name,
        "products_relevance"
    );
    assert!(matches!(
        &translated.bundle.replica_settings[1].derived_entry,
        ReplicaEntry::Virtual(name) if name == "shop_relevance"
    ));
    assert_eq!(
        translated.bundle.replica_settings[1].source_relevancy_strictness,
        Some(50)
    );
}

fn assert_complete_spool_rules_and_synonyms(translated: &TranslatedSpoolPayload) {
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
}

fn assert_complete_spool_report(translated: &TranslatedSpoolPayload) {
    assert!(translated
        .report
        .entries
        .iter()
        .all(|entry| entry.code != ReportCode::ReplicaTopologyNotMigrated));
    assert_eq!(translated.report.summary.hard_rejections, 0);
    assert!(
        translated.report.summary.warnings >= 4,
        "replica activation must carry topology/ranking warnings"
    );
    assert_eq!(translated.report.summary.scope_gaps, 5);
    assert_eq!(
        entries_for_code(
            &translated.report,
            ReportCode::ReplicaPrimaryRelevancyStrictnessDropped
        )
        .len(),
        1
    );
    assert_eq!(
        entries_for_code(
            &translated.report,
            ReportCode::ReplicaRelevancyStrictnessSemanticMismatch
        )
        .len(),
        2
    );
    assert!(translated.report.report_digest.is_some());
}

#[test]
fn translates_accepted_spool_in_bounded_document_batches() {
    let fixture = accepted_spool_fixture(
        json!({"searchableAttributes": ["title"], "attributesForFaceting": ["page_marker"]}),
        vec![
            document_page(0, 700),
            document_page(700, 301),
            document_page(1001, 204),
        ],
        vec![vec![minimal_rule("rule-1")]],
        vec![vec![minimal_synonym("syn-1")]],
    );
    let mut document_batches = Vec::new();
    let mut instrumentation = TranslationSessionInstrumentation::default();

    let translated = match translate_accepted_spool_payload(
        fixture.reader,
        "products".to_string(),
        "shop".to_string(),
        BTreeMap::new(),
        &mut instrumentation,
        || Ok(false),
        |batch| {
            document_batches.push(batch);
            Ok::<(), std::convert::Infallible>(())
        },
    )
    .expect("accepted spool artifacts should read")
    {
        TranslationOutcome::Translated(translated) => *translated,
        TranslationOutcome::Rejected(report) => {
            panic!("expected accepted spool to translate, got report {report:#?}")
        }
    };

    assert_eq!(
        document_batches.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![1_000, 205]
    );
    assert_eq!(instrumentation.document_pages_seen, 3);
    assert_eq!(instrumentation.max_live_decoded_pages, 1);
    assert_eq!(instrumentation.max_pending_documents, 1_000);
    assert_eq!(instrumentation.document_batches_emitted, vec![1_000, 205]);
    assert_eq!(
        document_batches
            .iter()
            .flatten()
            .map(|document| document.id.as_str())
            .collect::<Vec<_>>()
            .len(),
        1_205
    );
    assert_eq!(document_batches[0][0].id, "doc-0000");
    assert_eq!(
        document_batches[0][0].fields["title"],
        FieldValue::Text("Document 0".to_string())
    );
    assert_eq!(document_batches[0][999].id, "doc-0999");
    assert_eq!(document_batches[1][0].id, "doc-1000");
    assert_eq!(
        document_batches[1][204].fields["score"],
        FieldValue::Integer(1204)
    );
    assert_eq!(
        translated.bundle.settings.searchable_attributes,
        Some(vec!["title".to_string()])
    );
    assert_eq!(
        translated
            .bundle
            .rules
            .iter()
            .map(|rule| rule.object_id.as_str())
            .collect::<Vec<_>>(),
        vec!["rule-1"]
    );
    assert_eq!(
        translated
            .bundle
            .synonyms
            .iter()
            .map(|synonym| synonym.object_id())
            .collect::<Vec<_>>(),
        vec!["syn-1"]
    );
    assert_eq!(translated.report.summary.hard_rejections, 0);
    assert_eq!(translated.report.summary.scope_gaps, 5);
    assert!(
        translated.document_batches.is_empty(),
        "accepted-spool translation should stream document batches instead of retaining them"
    );
    assert_eq!(
        translated.report.report_digest.as_deref(),
        Some("0d6865142f81127352eeacc2b34f56741ec13147fa8da9c5dd681ad8f9ca2d68")
    );
}

#[test]
fn accepted_spool_translation_preserves_malformed_typed_payload_paths() {
    let fixture = accepted_spool_fixture(
        minimal_valid_settings(),
        vec![vec![json!({"objectID": "doc-1", "title": "ok"})]],
        vec![vec![json!({
            "objectID": "rule-1",
            "conditions": "bad",
            "consequence": {}
        })]],
        vec![vec![
            minimal_synonym("syn-1"),
            json!({"objectID": "syn-2", "type": "synonym", "synonyms": "bad"}),
        ]],
    );
    let mut document_batches = Vec::new();
    let mut instrumentation = TranslationSessionInstrumentation::default();

    let report = match translate_accepted_spool_payload(
        fixture.reader,
        "products".to_string(),
        "shop".to_string(),
        BTreeMap::new(),
        &mut instrumentation,
        || Ok(false),
        |batch| {
            document_batches.push(batch);
            Ok::<(), std::convert::Infallible>(())
        },
    )
    .expect("accepted spool artifacts should read")
    {
        TranslationOutcome::Translated(translated) => {
            panic!("expected accepted spool rejection, got translated payload {translated:#?}")
        }
        TranslationOutcome::Rejected(report) => report,
    };

    assert_eq!(
        hard_report_entry_contract(&report),
        vec![
            (
                ReportSeverity::HardRejection,
                ReportCode::MalformedRulePayload,
                ReportResource::Rule,
                Some(0),
                Some(0),
                "$.conditions".to_string(),
            ),
            (
                ReportSeverity::HardRejection,
                ReportCode::MalformedSynonymPayload,
                ReportResource::Synonym,
                Some(0),
                Some(1),
                "$".to_string(),
            ),
        ]
    );
    assert_eq!(document_batches.len(), 1);
    assert_eq!(document_batches[0][0].id, "doc-1");
    assert_eq!(instrumentation.max_live_decoded_pages, 1);
    assert_eq!(instrumentation.max_pending_documents, 1);
}

fn replica_settings_map() -> BTreeMap<String, serde_json::Value> {
    let mut map = BTreeMap::new();
    map.insert(
        "products_price_asc".to_string(),
        json!({"ranking": ["desc(price)"], "relevancyStrictness": 80}),
    );
    map.insert(
        "products_relevance".to_string(),
        json!({"ranking": ["asc(popularity)"]}),
    );
    map
}

/// The exact nonzero replica-settings map reaches the accepted-spool translation
/// entry point and is observed there — counted, never applied to settings.
#[test]
fn accepted_spool_translation_observes_carried_replica_settings_count() {
    let fixture = accepted_spool_fixture(minimal_valid_settings(), vec![], vec![], vec![]);
    let mut instrumentation = TranslationSessionInstrumentation::default();

    let outcome = translate_accepted_spool_payload(
        fixture.reader,
        "products".to_string(),
        "shop".to_string(),
        replica_settings_map(),
        &mut instrumentation,
        || Ok(false),
        |_batch| Ok::<(), std::convert::Infallible>(()),
    )
    .expect("accepted spool artifacts should read");

    assert!(matches!(outcome, TranslationOutcome::Translated(_)));
    assert_eq!(instrumentation.replica_settings_count, 2);
}

/// The same observation holds through the in-memory translation entry point.
#[test]
fn in_memory_translation_observes_carried_replica_settings_count() {
    let input = SpoolTranslationInput {
        source_index_name: "products".to_string(),
        target_index_name: "shop".to_string(),
        settings: minimal_valid_settings(),
        document_pages: vec![],
        rule_pages: vec![],
        synonym_pages: vec![],
        replica_settings: replica_settings_map(),
    };
    let mut instrumentation = TranslationSessionInstrumentation::default();

    let outcome = translate_spool_input(input, &mut instrumentation, |_batch| {
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("in-memory translation should not fail on pages");

    assert!(matches!(outcome, TranslationOutcome::Translated(_)));
    assert_eq!(instrumentation.replica_settings_count, 2);
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

    let translated = translated(exact_document_and_settings_payload());
    assert_exact_settings_payload_values(&translated);
    assert_exact_document_payload_values(&translated);
}

fn exact_document_and_settings_payload() -> SpoolTranslationInput {
    spool_payload(
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
    )
}

fn assert_exact_settings_payload_values(translated: &TranslatedSpoolPayload) {
    assert_exact_settings_payload_core_values(translated);
    assert_exact_settings_payload_advanced_values(translated);
}

fn assert_exact_settings_payload_core_values(translated: &TranslatedSpoolPayload) {
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
}

fn assert_exact_settings_payload_advanced_values(translated: &TranslatedSpoolPayload) {
    let settings = &translated.bundle.settings;
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
}

fn assert_exact_document_payload_values(translated: &TranslatedSpoolPayload) {
    assert_eq!(translated_documents(translated)[0].id, "doc-1");
    assert_eq!(
        translated_documents(translated)[0].fields["title"],
        FieldValue::Text("Trail Shoe".to_string())
    );
    assert_eq!(
        translated_documents(translated)[0].fields["price"],
        FieldValue::Integer(129)
    );
}

#[test]
fn transformed_settings_distinct_and_deprecated_aliases_persist() {
    for field in ["distinct", "attributesToIndex", "numericAttributesToIndex"] {
        let row = resolve_source_field(ResourceKind::Settings, field);
        assert_eq!(row.disposition, Disposition::Transformed);
        assert_eq!(row.target_owner, TargetOwner::SettingsPayloadMerge);
    }

    let translated = translated(spool_payload(
        json!({
            "distinct": true,
            "attributesToIndex": ["title", "brand"],
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
    assert_eq!(
        translated.bundle.settings.searchable_attributes,
        Some(vec!["title".to_string(), "brand".to_string()])
    );
}

/// The deprecated alias must preserve search behavior, while the canonical field
/// wins when both forms are present in a source settings response.
#[test]
fn legacy_attributes_to_index_transforms_with_canonical_field_precedence() {
    let row = resolve_source_field(ResourceKind::Settings, "attributesToIndex");
    assert_eq!(row.disposition, Disposition::Transformed);
    assert_eq!(row.warning_code, None);
    assert_eq!(row.target_owner, TargetOwner::SettingsPayloadMerge);

    let translated = translated(spool_payload(
        json!({
            "searchableAttributes": ["canonical_title"],
            "attributesToIndex": ["legacy_title"]
        }),
        vec![],
        vec![],
        vec![],
    ));

    assert_eq!(
        translated.bundle.settings.searchable_attributes,
        Some(vec!["canonical_title".to_string()])
    );
    assert!(translated
        .report
        .entries
        .iter()
        .all(|entry| entry.json_path != "$.attributesToIndex"));
    assert_eq!(translated.report.summary.hard_rejections, 0);
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
            "attributesToIndex": null,
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
fn primary_relevancy_strictness_without_replicas_does_not_emit_replica_warning() {
    let translated = translated(spool_payload(
        json!({"searchableAttributes": ["title"], "relevancyStrictness": 90}),
        vec![],
        vec![],
        vec![],
    ));

    assert!(
        entries_for_code(
            &translated.report,
            ReportCode::ReplicaPrimaryRelevancyStrictnessDropped
        )
        .is_empty(),
        "replica-only warning should not fire when no replica topology is translated"
    );
    assert_eq!(translated.report.summary.warnings, 0);
}

#[test]
fn hard_rejected_settings_emit_canonical_codes_and_paths() {
    let mut payload = spool_payload(
        json!({
            "replicas": ["products_price_asc"],
            "relevancyStrictness": 90,
            "notAFlapjackOwnedField": true
        }),
        vec![],
        vec![],
        vec![],
    );
    payload.replica_settings = BTreeMap::from([(
        "products_price_asc".to_string(),
        json!({"ranking": ["desc(price)"], "relevancyStrictness": 80}),
    )]);
    let report = rejected(payload);

    assert_eq!(
        hard_codes(&report),
        vec![ReportCode::UnsupportedSourceField]
    );
    assert!(entries_for_code(&report, ReportCode::ReplicaTopologyNotMigrated).is_empty());
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

    assert!(translated.document_batches.is_empty());
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

    assert_eq!(translated_documents(&translated)[0].id, "shared");
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
    let mut payload = spool_payload(
        json!({"replicas": ["products_price_asc"], "relevancyStrictness": 90}),
        vec![],
        vec![],
        vec![],
    );
    payload.replica_settings = BTreeMap::from([(
        "products_price_asc".to_string(),
        json!({"ranking": ["desc(price)"], "relevancyStrictness": 80}),
    )]);
    let translated = translated(payload);

    assert_eq!(
        translated.bundle.settings.replicas,
        Some(vec!["virtual(shop_price_asc)".to_string()])
    );
    assert!(translated.bundle.settings.relevancy_strictness.is_none());
    assert!(translated
        .report
        .entries
        .iter()
        .all(|entry| entry.code != ReportCode::ReplicaTopologyNotMigrated));
    let report = &translated.report;
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

    assert_positive_live_documents(&translated);
    assert_positive_live_settings(&translated);
    assert_positive_live_rules(&translated);
    assert_positive_live_synonyms(&translated);
    assert_positive_live_report(&translated);
}

fn assert_positive_live_documents(translated: &TranslatedSpoolPayload) {
    assert_eq!(
        translated_documents(translated)
            .into_iter()
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
}

fn assert_positive_live_settings(translated: &TranslatedSpoolPayload) {
    assert_positive_live_filter_settings(translated);
    assert_positive_live_ranking_settings(translated);
}

fn assert_positive_live_filter_settings(translated: &TranslatedSpoolPayload) {
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
}

fn assert_positive_live_ranking_settings(translated: &TranslatedSpoolPayload) {
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
}

fn assert_positive_live_rules(translated: &TranslatedSpoolPayload) {
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
}

fn assert_positive_live_synonyms(translated: &TranslatedSpoolPayload) {
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
}

fn assert_positive_live_report(translated: &TranslatedSpoolPayload) {
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
