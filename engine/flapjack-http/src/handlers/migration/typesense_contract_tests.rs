use super::source_reader::{accept_source_export, TypesenseSourceReader};
use super::source_test_support::{typesense_observation, RecordingSink, ScriptedTypesenseSource};
use super::translation::{translate_settings_for_provider, ReportCode, SettingsSourceProvider};
use super::{ensure_source_provider_supported, AsyncMigrationSourceProvider};
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

fn serialized_settings(settings: &IndexSettings) -> Value {
    serde_json::to_value(settings).expect("IndexSettings must serialize for full-field comparison")
}

/// Reconstructs the settings payload exactly as production spools it: the
/// collection schema with the non-settings `name`/`documents` keys stripped (see
/// `settings_from_collection` in `typesense_client.rs`).
fn collection_settings(bundle: &Value, name: &str) -> Value {
    let mut settings = bundle["source"]["collections"]
        .as_array()
        .expect("fixture collections must be an array")
        .iter()
        .find(|collection| collection["name"] == name)
        .unwrap_or_else(|| panic!("fixture must contain collection {name}"))
        .as_object()
        .expect("fixture collection must be an object")
        .clone();
    settings.remove("name");
    settings.remove("documents");
    Value::Object(settings)
}

/// Translates a Typesense settings payload through the shared provider dispatch,
/// asserting it does not fail closed, and returns the settings plus the attributed
/// warning `(code, json_path)` pairs in emission order.
fn typesense_settings_and_warnings(settings: &Value) -> (IndexSettings, Vec<(ReportCode, String)>) {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let translated = translate_settings_for_provider(
        settings,
        SettingsSourceProvider::Typesense,
        &mut failures,
        &mut warnings,
    )
    .expect("valid Typesense settings must translate");
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");
    let attributed = warnings
        .iter()
        .map(|warning| (warning.code, warning.json_path.clone()))
        .collect();
    (translated, attributed)
}

async fn capture_typesense_contract_documents(
    documents: Vec<Vec<Value>>,
) -> Result<RecordingSink, String> {
    let count = documents.iter().map(Vec::len).sum::<usize>() as u64;
    let source = ScriptedTypesenseSource::with_passes(
        typesense_observation("fj_ts_migration_products", count),
        json!({}),
        vec![documents.clone(), documents],
    );
    let mut reader = TypesenseSourceReader::from_source("fj_ts_migration_products", source);
    let mut sink = RecordingSink::default();

    accept_source_export(
        AsyncMigrationSourceProvider::Typesense,
        &mut reader,
        &mut sink,
    )
    .await
    .map(|_| sink)
    .map_err(|error| error.safe_message().to_string())
}

#[tokio::test]
async fn typesense_stable_id_mapping_uses_the_production_document_record_path() {
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

    let duplicate_across_pages = vec![
        vec![documents[0].clone(), documents[1].clone()],
        vec![documents[0].clone()],
    ];
    let rejection_specs = [
        vec![missing_with_synthesized_object_id],
        vec![non_string],
        vec![duplicate],
        duplicate_across_pages,
    ];
    let mut rejection_messages = Vec::new();
    for specimen in rejection_specs {
        match capture_typesense_contract_documents(specimen).await {
            Ok(_) => panic!("invalid Typesense documents must be rejected"),
            Err(message) => rejection_messages.push(message),
        }
    }
    let sink = capture_typesense_contract_documents(vec![documents.clone()])
        .await
        .unwrap();

    assert_eq!(
        rejection_messages,
        vec![
            "Typesense document id is invalid".to_string(),
            "Typesense document id is invalid".to_string(),
            "Typesense document id is invalid".to_string(),
            "duplicate source objectID".to_string(),
        ],
        "missing, synthesized, non-string, and duplicate IDs must fail through the source reader"
    );
    assert_eq!(
        sink.document_pages.concat(),
        expected_ids,
        "Typesense source identity must come from the required vendor id field"
    );
    assert_eq!(
        sink.raw_document_pages.concat(),
        documents
            .iter()
            .zip(expected_ids)
            .map(|(document, stable_id)| json!({
                "stableId": stable_id,
                "payload": document,
            }))
            .collect::<Vec<_>>(),
        "Typesense capture must preserve provider-native fields without synthesizing objectID"
    );
}

/// M0B known-answer: per-field `facet`/`index`/`type` schema flags are the single
/// source of truth for `attributesForFaceting`/`searchableAttributes`, emitted in
/// source-schema order. Exact-set equality is the negative proof — the excluded
/// specimens named below (vector `embedding`, reference `category_id`, `object`
/// parent `metadata`, numeric `metadata.rating`/`metadata.dimensions.*`, and the
/// `index: false` `nullable_note`/`secret_note`) can never appear in either list.
#[test]
fn typesense_settings_m0b_known_answer_translates_schema_flags() {
    let bundle = expected_bundle();

    let (categories, _) = typesense_settings_and_warnings(&collection_settings(
        &bundle,
        "fj_ts_migration_categories",
    ));
    let expected_categories = IndexSettings {
        attributes_for_faceting: vec!["active".to_string(), "labels".to_string()],
        searchable_attributes: Some(vec![
            "name".to_string(),
            "labels".to_string(),
            "parent".to_string(),
        ]),
        ranking: None,
        ..Default::default()
    };
    assert_eq!(
        serialized_settings(&categories),
        serialized_settings(&expected_categories)
    );
    assert!(categories.ranking.is_none());
    assert!(categories.custom_ranking.is_none());

    let (products, _) =
        typesense_settings_and_warnings(&collection_settings(&bundle, "fj_ts_migration_products"));
    let expected_products = IndexSettings {
        attributes_for_faceting: vec![
            "price".to_string(),
            "available".to_string(),
            "tags".to_string(),
            "metadata.color".to_string(),
        ],
        searchable_attributes: Some(vec![
            "title".to_string(),
            "sku".to_string(),
            "tags".to_string(),
            "metadata.color".to_string(),
        ]),
        ranking: None,
        ..Default::default()
    };
    assert_eq!(
        serialized_settings(&products),
        serialized_settings(&expected_products)
    );
    assert!(products.ranking.is_none());
    assert!(products.custom_ranking.is_none());

    // Name the negative specimens so a regression to naive "all indexed fields"
    // behavior fails loudly rather than silently widening the lists.
    let searchable = products.searchable_attributes.clone().unwrap();
    for excluded in [
        "embedding",
        "category_id",
        "metadata",
        "metadata.rating",
        "metadata.dimensions.width_cm",
        "metadata.dimensions.height_cm",
        "nullable_note",
        "secret_note",
    ] {
        assert!(
            !searchable.contains(&excluded.to_string()),
            "{excluded} must not be searchable"
        );
        assert!(
            !products
                .attributes_for_faceting
                .contains(&excluded.to_string()),
            "{excluded} must not be a facet"
        );
    }
}

#[test]
fn typesense_settings_preserves_explicit_empty_searchable_attributes() {
    let (settings, warnings) = typesense_settings_and_warnings(&json!({
        "fields": [
            {"name": "hidden", "type": "string", "index": false},
            {"name": "price", "type": "float"}
        ]
    }));

    assert_eq!(settings.searchable_attributes, Some(Vec::new()));
    assert!(warnings.is_empty());
}

/// The warning set is exact. Schema flags that produced either target list do not
/// warn; `enable_nested_fields` is a translation control, not an unmapped concept;
/// there is no blanket `$.fields` warning. Every genuinely unmapped concept gets
/// exactly one attributed `TypesenseSettingNotMigrated` entry: the per-field vector
/// (`$.fields[10]`) and reference (`$.fields[11]`) schema intent, and the
/// direction-less `default_sorting_field` for both `priority` and `price`.
/// `facet_by`/`query_by` are per-query inputs with no per-field schema home, so
/// they warn rather than inventing a setting or a `customRanking` direction.
#[test]
fn typesense_settings_m0b_warning_set_is_exact() {
    let bundle = expected_bundle();

    let (_, categories_warnings) = typesense_settings_and_warnings(&collection_settings(
        &bundle,
        "fj_ts_migration_categories",
    ));
    assert_eq!(
        categories_warnings,
        vec![(
            ReportCode::TypesenseSettingNotMigrated,
            "$.default_sorting_field".to_string()
        )]
    );

    let (_, products_warnings) =
        typesense_settings_and_warnings(&collection_settings(&bundle, "fj_ts_migration_products"));
    assert_eq!(
        products_warnings,
        vec![
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.default_sorting_field".to_string()
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.token_separators".to_string()
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.symbols_to_index".to_string()
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.synonym_sets".to_string()
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.curation_sets".to_string()
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.fields[10]".to_string()
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.fields[11]".to_string()
            ),
        ]
    );

    let (_, per_query_warnings) = typesense_settings_and_warnings(&json!({
        "facet_by": ["category"],
        "query_by": "title,description",
    }));
    assert_eq!(
        per_query_warnings,
        vec![
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.facet_by".to_string()
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.query_by".to_string()
            ),
        ]
    );
}

#[test]
fn typesense_settings_warns_for_unmapped_dynamic_field_types() {
    let (settings, warnings) = typesense_settings_and_warnings(&json!({
        "fields": [
            {"name": "title", "type": "string"},
            {"name": "coerced_text", "type": "string*"},
            {"name": ".*", "type": "auto"}
        ]
    }));

    assert_eq!(
        settings.searchable_attributes,
        Some(vec!["title".to_string()])
    );
    assert_eq!(
        warnings,
        vec![
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.fields[1]".to_string()
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.fields[2]".to_string()
            ),
        ]
    );
}

#[test]
fn typesense_settings_translates_reference_facet_but_not_reference_searchability() {
    let (settings, warnings) = typesense_settings_and_warnings(&json!({
        "fields": [{
            "name": "category_id",
            "type": "string",
            "facet": true,
            "reference": "categories.id"
        }]
    }));

    assert_eq!(
        settings.attributes_for_faceting,
        vec!["category_id".to_string()]
    );
    assert_eq!(settings.searchable_attributes, Some(Vec::new()));
    assert_eq!(
        warnings,
        vec![(
            ReportCode::TypesenseSettingNotMigrated,
            "$.fields[0]".to_string()
        )]
    );
}

#[test]
fn typesense_settings_warns_for_regex_fields_without_emitting_literal_attributes() {
    let (settings, warnings) = typesense_settings_and_warnings(&json!({
        "enable_nested_fields": true,
        "fields": [
            {"name": "title", "type": "string"},
            {"name": "title_.*", "type": "string"},
            {"name": "facet_.*", "type": "auto", "facet": true, "index": false}
        ]
    }));

    assert_eq!(settings.attributes_for_faceting, Vec::<String>::new());
    assert_eq!(
        settings.searchable_attributes,
        Some(vec!["title".to_string()])
    );
    assert_eq!(
        warnings,
        vec![
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.fields[1]".to_string()
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.fields[2]".to_string()
            ),
        ]
    );
}

#[test]
fn typesense_settings_warns_at_exact_paths_for_unmapped_field_search_semantics() {
    let (settings, warnings) = typesense_settings_and_warnings(&json!({
        "fields": [
            {"name": "plain", "type": "string"},
            {"name": "localized", "type": "string", "locale": "ja"},
            {"name": "substring", "type": "string", "infix": true},
            {"name": "stemmed", "type": "string", "stem": true},
            {"name": "dictionary", "type": "string", "stem_dictionary": "products"},
            {"name": "hyphenated", "type": "string", "token_separators": ["-"]},
            {"name": "programming", "type": "string", "symbols_to_index": ["+"]},
            {"name": "bounded", "type": "string", "truncate_len": 80}
        ]
    }));

    assert_eq!(
        settings.searchable_attributes,
        Some(
            [
                "plain",
                "localized",
                "substring",
                "stemmed",
                "dictionary",
                "hyphenated",
                "programming",
                "bounded",
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        )
    );
    assert_eq!(
        warnings,
        [
            "$.fields[1].locale",
            "$.fields[2].infix",
            "$.fields[3].stem",
            "$.fields[4].stem_dictionary",
            "$.fields[5].token_separators",
            "$.fields[6].symbols_to_index",
            "$.fields[7].truncate_len",
        ]
        .into_iter()
        .map(|path| (ReportCode::TypesenseSettingNotMigrated, path.to_string()))
        .collect::<Vec<_>>()
    );
}

#[test]
fn typesense_settings_accepts_captured_response_only_field_metadata() {
    let (settings, warnings) = typesense_settings_and_warnings(&json!({
        "fields": [
            {
                "name": "metadata.color",
                "type": "string",
                "facet": false,
                "optional": false,
                "index": true,
                "store": true,
                "sort": false,
                "infix": false,
                "locale": "",
                "range_index": false,
                "stem": false,
                "stem_dictionary": "",
                "token_separators": [],
                "symbols_to_index": [],
                "truncate_len": 100,
                "nested": true,
                "nested_array": 2
            },
            {
                "name": "embedding",
                "type": "float[]",
                "facet": false,
                "optional": false,
                "index": true,
                "store": true,
                "sort": false,
                "infix": false,
                "locale": "",
                "range_index": false,
                "stem": false,
                "stem_dictionary": "",
                "token_separators": [],
                "symbols_to_index": [],
                "truncate_len": 100,
                "nested": false,
                "nested_array": 0,
                "num_dim": 3,
                "vec_dist": "ip",
                "embed": {
                    "from": ["metadata.color"],
                    "model_config": {"model_name": "ts/all-MiniLM-L12-v2"}
                },
                "hnsw_params": {"M": 16, "ef_construction": 200}
            },
            {
                "name": "category_id",
                "type": "string",
                "reference": "categories.id",
                "async_reference": true,
                "cascade_delete": false,
                "nested": false,
                "nested_array": 0
            }
        ],
        "enable_nested_fields": true
    }));

    assert_eq!(
        settings.searchable_attributes,
        Some(vec!["metadata.color".to_string()])
    );
    assert_eq!(
        warnings,
        vec![
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.fields[1]".to_string()
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.fields[2]".to_string()
            ),
        ]
    );
}

#[test]
fn typesense_settings_accepts_typesense_30_2_nested_vector_response_shapes() {
    let (settings, warnings) = typesense_settings_and_warnings(&json!({
        "fields": [
            {"name": "title", "type": "string"},
            {
                "name": "embedding",
                "type": "float[]",
                "num_dim": 3,
                "vec_dist": "cosine",
                "embed": {
                    "from": ["title"],
                    "mapping": ["document_title"],
                    "model_config": {
                        "model_name": "openai/text-embedding-3-small",
                        "api_key": "fixture-key",
                        "url": "https://embeddings.example.test",
                        "path": "/v1/embeddings"
                    }
                },
                "hnsw_params": {
                    "M": 16,
                    "ef_construction": 200,
                    "max_elements": 10000,
                    "ef": 10
                }
            },
            {
                "name": "personalized_embedding",
                "type": "float[]",
                "num_dim": 3,
                "embed": {
                    "from": ["title"],
                    "model_config": {
                        "model_name": "ts/personalization",
                        "personalization_type": "recommendation"
                    }
                }
            }
        ]
    }));

    assert_eq!(
        settings.searchable_attributes,
        Some(vec!["title".to_string()])
    );
    assert_eq!(
        warnings,
        vec![
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.fields[1]".to_string()
            ),
            (
                ReportCode::TypesenseSettingNotMigrated,
                "$.fields[2]".to_string()
            ),
        ]
    );
}

#[test]
fn typesense_settings_accepts_complete_gcp_service_account_response_shape() {
    let (settings, warnings) = typesense_settings_and_warnings(&json!({
        "fields": [
            {"name": "title", "type": "string"},
            {
                "name": "embedding",
                "type": "float[]",
                "num_dim": 3,
                "embed": {
                    "from": ["title"],
                    "model_config": {
                        "model_name": "google/gemini-embedding-001",
                        "service_account": {
                            "type": "service_account",
                            "project_id": "fixture-project",
                            "private_key_id": "fixture-key-id",
                            "private_key": "fixture-private-key",
                            "client_email": "embeddings@example.test",
                            "client_id": "fixture-client-id",
                            "auth_uri": "https://accounts.googleapis.test/o/oauth2/auth",
                            "token_uri": "https://oauth2.googleapis.test/token",
                            "auth_provider_x509_cert_url": "https://www.googleapis.test/oauth2/v1/certs",
                            "client_x509_cert_url": "https://www.googleapis.test/robot/v1/metadata/x509/embeddings%40example.test",
                            "universe_domain": "googleapis.test"
                        }
                    }
                }
            }
        ]
    }));

    assert_eq!(
        settings.searchable_attributes,
        Some(vec!["title".to_string()])
    );
    assert_eq!(
        warnings,
        vec![(
            ReportCode::TypesenseSettingNotMigrated,
            "$.fields[1]".to_string()
        )]
    );
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
