use super::source_reader::normalize_typesense_document_page;
use super::translation::{
    translate_settings_for_provider, warning_message, ReportCode, SettingsSourceProvider,
};
use super::*;
use flapjack::index::settings::IndexSettings;
use serde_json::{json, Value};
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json")
}

fn m0b_public_contract_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../docs2/3_IMPLEMENTATION/2026_07_26_m0b_typesense_source_contract.md")
}

fn expected_bundle() -> Value {
    let path = fixture_path();
    serde_json::from_slice(
        &std::fs::read(&path)
            .unwrap_or_else(|error| panic!("fixture {} must be readable: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("fixture {} must be JSON: {error}", path.display()))
}

fn products_documents(bundle: &Value) -> Vec<Value> {
    bundle["source"]["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|collection| collection["name"] == "fj_ts_migration_products")
        .unwrap()["documents"]
        .as_array()
        .unwrap()
        .clone()
}

fn projected_object_ids(documents: &[Value]) -> Vec<String> {
    documents
        .iter()
        .map(|document| {
            document
                .get("objectID")
                .and_then(Value::as_str)
                .unwrap_or("<missing objectID>")
                .to_string()
        })
        .collect()
}

fn normalized_typesense_documents(documents: &[Value]) -> Result<Vec<Value>, String> {
    normalize_typesense_document_page(documents, "$.source.collections[1].documents")
}

fn serialized_settings(settings: &IndexSettings) -> Value {
    serde_json::to_value(settings).expect("IndexSettings must serialize for full-field comparison")
}

#[test]
fn typesense_stable_id_mapping_uses_the_document_id_field() {
    let bundle = expected_bundle();
    let documents = products_documents(&bundle);
    let expected_ids = vec![
        "prod_001".to_string(),
        "prod_002".to_string(),
        "prod_003".to_string(),
    ];

    let mut missing_with_synthesized_object_id = documents.clone();
    missing_with_synthesized_object_id[0]
        .as_object_mut()
        .unwrap()
        .remove("id");
    missing_with_synthesized_object_id[0]["objectID"] = json!("synthesized_001");

    let mut non_string = documents.clone();
    non_string[1]["id"] = json!(2002);

    let mut duplicate = documents.clone();
    duplicate[2]["id"] = json!("prod_001");

    let rejection_messages = [missing_with_synthesized_object_id, non_string, duplicate]
        .iter()
        .map(|specimen| normalized_typesense_documents(specimen).unwrap_err())
        .collect::<Vec<_>>();
    let normalized = normalized_typesense_documents(&documents).unwrap();

    assert_eq!(
        rejection_messages,
        vec![
            "$.source.collections[1].documents[0].id: missing Typesense id".to_string(),
            "$.source.collections[1].documents[1].id: Typesense id must be a string".to_string(),
            "$.source.collections[1].documents[2].id: duplicate Typesense id prod_001".to_string(),
        ],
        "missing, synthesized, non-string, and duplicate IDs must fail with exact paths"
    );
    assert_eq!(
        projected_object_ids(&normalized),
        expected_ids,
        "the shared source-reader seam still needs to project Typesense id into Flapjack objectID"
    );
}

#[test]
fn typesense_unproved_configuration_emits_attributed_warning_not_silent_loss() {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let translated = translate_settings_for_provider(
        &json!({
            "default_sorting_field": "price",
            "enable_nested_fields": true,
            "token_separators": ["-"],
            "symbols_to_index": ["#"]
        }),
        SettingsSourceProvider::Typesense,
        &mut failures,
        &mut warnings,
    )
    .expect("valid Typesense JSON settings should produce a translation report");
    let expected_settings = IndexSettings {
        ranking: None,
        ..Default::default()
    };

    assert!(failures.is_empty());
    assert_eq!(
        (
            serialized_settings(&translated),
            warnings
                .iter()
                .map(|warning| (
                    warning.code,
                    warning.json_path.as_str(),
                    warning_message(warning.code),
                ))
                .collect::<Vec<_>>(),
        ),
        (
            serialized_settings(&expected_settings),
            vec![
                (
                    ReportCode::TypesenseSettingNotMigrated,
                    "$.default_sorting_field",
                    Some("Typesense setting has no receipt-proved Flapjack equivalent and was not migrated."),
                ),
                (
                    ReportCode::TypesenseSettingNotMigrated,
                    "$.enable_nested_fields",
                    Some("Typesense setting has no receipt-proved Flapjack equivalent and was not migrated."),
                ),
                (
                    ReportCode::TypesenseSettingNotMigrated,
                    "$.token_separators",
                    Some("Typesense setting has no receipt-proved Flapjack equivalent and was not migrated."),
                ),
                (
                    ReportCode::TypesenseSettingNotMigrated,
                    "$.symbols_to_index",
                    Some("Typesense setting has no receipt-proved Flapjack equivalent and was not migrated."),
                ),
            ],
        )
    );
}

#[test]
fn typesense_unknown_top_level_settings_emit_attributed_warning_not_silent_loss() {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let translated = translate_settings_for_provider(
        &json!({
            "default_sorting_field": null,
            "facet_by": ["category"],
            "query_by": "title,description"
        }),
        SettingsSourceProvider::Typesense,
        &mut failures,
        &mut warnings,
    )
    .expect("well-formed but unproved Typesense settings should produce warnings");

    assert_eq!(
        serialized_settings(&translated),
        serialized_settings(&IndexSettings {
            ranking: None,
            ..Default::default()
        })
    );
    assert!(failures.is_empty());
    assert_eq!(
        warnings
            .iter()
            .map(|warning| (
                warning.code,
                warning.json_path.as_str(),
                warning_message(warning.code),
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.facet_by",
                Some("Typesense setting has no receipt-proved Flapjack equivalent and was not migrated."),
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.query_by",
                Some("Typesense setting has no receipt-proved Flapjack equivalent and was not migrated."),
            ),
        ]
    );
}

#[test]
fn typesense_malformed_unsupported_setting_values_fail_closed() {
    let specimens = [
        ("query_by", json!({"field": "title"})),
        ("facet_by", json!(["category", 42])),
        ("fields", json!([{"name": "id"}, "not-a-field"])),
        ("metadata", json!("opaque")),
        ("drop_tokens_threshold", json!(2)),
        ("prioritize_exact_match", json!(true)),
        ("future_setting", json!("opaque")),
    ];

    for (field, malformed_value) in specimens {
        let mut failures = Vec::new();
        let mut warnings = Vec::new();
        let settings = Value::Object([(field.to_string(), malformed_value)].into_iter().collect());

        let translated = translate_settings_for_provider(
            &settings,
            SettingsSourceProvider::Typesense,
            &mut failures,
            &mut warnings,
        );

        assert!(
            translated.is_none(),
            "malformed unsupported Typesense setting {field} must fail closed"
        );
        assert_eq!(failures.len(), 1);
        let failure = format!("{:?}", failures[0]);
        assert!(failure.contains("MalformedSettingsPayload"));
        assert!(failure.contains(&format!("$.{field}")));
        assert!(
            warnings.is_empty(),
            "a malformed setting must not be downgraded to a warning"
        );
    }
}

#[test]
fn typesense_contract_imports_m0b_fixture_and_public_contract_without_copying_shell_kat() {
    let bundle = expected_bundle();
    let public_contract = std::fs::read_to_string(m0b_public_contract_path())
        .expect("M0B public contract is readable");

    assert_eq!(
        bundle["contract"]["fixture_version"],
        "2026_07_26_m0b_typesense_migration"
    );
    assert!(bundle["contract"]["capture_requires_write_freeze"]
        .as_bool()
        .unwrap());
    assert!(
        public_contract.contains(
            "engine/tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json"
        ),
        "M0B public contract must own the committed Typesense fixture oracle"
    );
    assert!(
        public_contract.contains("Typesense KAT: `Results: 44/44 passed`."),
        "M0B public contract must preserve the Typesense oracle denominator"
    );
}

#[test]
fn typesense_source_provider_is_admitted_through_the_shared_lifecycle() {
    ensure_source_provider_supported(AsyncMigrationSourceProvider::Typesense)
        .expect("Stage 2/3 must admit Typesense through the shared source lifecycle");
}
