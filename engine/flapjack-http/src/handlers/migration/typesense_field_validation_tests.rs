use super::translation::{translate_settings_for_provider, ReportCode, SettingsSourceProvider};
use serde_json::{json, Value};

fn assert_typesense_field_rejected(field: Value, expected_path: &str) {
    let settings = json!({"fields": [field]});
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

    let translated = translate_settings_for_provider(
        &settings,
        SettingsSourceProvider::Typesense,
        &mut failures,
        &mut warnings,
    );

    assert!(translated.is_none());
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].code, ReportCode::MalformedSettingsPayload);
    assert_eq!(failures[0].json_path, expected_path);
    assert!(warnings.is_empty());
}

#[test]
fn translate_typesense_malformed_and_unknown_settings_fail_closed() {
    let specimens = [
        (
            "default_sorting_field",
            json!(42),
            "$.default_sorting_field",
        ),
        (
            "enable_nested_fields",
            json!("true"),
            "$.enable_nested_fields",
        ),
        ("query_by", json!({"field": "title"}), "$.query_by"),
        ("facet_by", json!(["category", 42]), "$.facet_by"),
        (
            "fields",
            json!([{"name": "id"}, "not-a-field"]),
            "$.fields[0].type",
        ),
        ("token_separators", json!(["-", 42]), "$.token_separators"),
        (
            "symbols_to_index",
            json!(["+", false]),
            "$.symbols_to_index",
        ),
        ("synonym_sets", json!(["products", {}]), "$.synonym_sets"),
        (
            "curation_sets",
            json!(["featured", null]),
            "$.curation_sets",
        ),
        ("metadata", json!("opaque"), "$.metadata"),
        ("drop_tokens_threshold", json!(2), "$.drop_tokens_threshold"),
        (
            "prioritize_exact_match",
            json!(true),
            "$.prioritize_exact_match",
        ),
        ("future_setting", json!("opaque"), "$.future_setting"),
    ];

    for (field, malformed_value, expected_path) in specimens {
        let mut failures = Vec::new();
        let mut warnings = Vec::new();
        let settings = Value::Object([(field.to_string(), malformed_value)].into_iter().collect());

        let translated = translate_settings_for_provider(
            &settings,
            SettingsSourceProvider::Typesense,
            &mut failures,
            &mut warnings,
        );

        assert!(translated.is_none());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].code, ReportCode::MalformedSettingsPayload);
        assert_eq!(failures[0].json_path, expected_path);
        assert!(warnings.is_empty());
    }
}

#[test]
fn translate_typesense_malformed_field_flags_fail_closed_at_exact_path() {
    for (flag, malformed_value) in [("facet", json!("true")), ("index", json!("false"))] {
        let mut field = json!({"name": "title", "type": "string"});
        field[flag] = malformed_value;
        assert_typesense_field_rejected(field, &format!("$.fields[0].{flag}"));
    }
}

#[test]
fn translate_typesense_malformed_unsupported_field_metadata_fails_closed_at_exact_path() {
    let specimens = [
        ("num_dim", json!("3")),
        ("vec_dist", json!({})),
        ("reference", json!(false)),
    ];

    for (field, malformed_value) in specimens {
        let mut typed_field = json!({"name": "embedding", "type": "float[]"});
        typed_field[field] = malformed_value;
        assert_typesense_field_rejected(typed_field, &format!("$.fields[0].{field}"));
    }
}

#[test]
fn translate_typesense_malformed_required_field_strings_fail_closed_at_exact_path() {
    let specimens = [
        (
            json!({"name": "", "type": "string", "facet": true}),
            "$.fields[0].name",
        ),
        (
            json!({"name": "title", "type": "text", "index": true}),
            "$.fields[0].type",
        ),
    ];

    for (field, expected_path) in specimens {
        assert_typesense_field_rejected(field, expected_path);
    }
}

#[test]
fn translate_typesense_unknown_field_member_fails_closed_at_exact_path() {
    assert_typesense_field_rejected(
        json!({
            "name": "title",
            "type": "string",
            "future_field_option": true
        }),
        "$.fields[0].future_field_option",
    );
}

#[test]
fn translate_typesense_malformed_captured_field_members_fail_closed_at_exact_path() {
    let specimens = [
        ("optional", json!("false")),
        ("store", json!(1)),
        ("sort", json!("true")),
        ("infix", json!([])),
        ("locale", json!(false)),
        ("range_index", json!("false")),
        ("stem", json!(1)),
        ("stem_dictionary", json!([])),
        ("token_separators", json!(["--"])),
        ("symbols_to_index", json!([7])),
        ("truncate_len", json!(-1)),
        ("nested", json!("false")),
        ("nested_array", json!(-1)),
        ("async_reference", json!("true")),
        ("cascade_delete", json!(0)),
        ("embed", json!([])),
        ("hnsw_params", json!(false)),
    ];

    for (member, malformed_value) in specimens {
        let mut field = json!({"name": "title", "type": "string"});
        field[member] = malformed_value;
        assert_typesense_field_rejected(field, &format!("$.fields[0].{member}"));
    }

    for (member, malformed_value) in [
        ("vec_dist", json!("euclidean")),
        ("reference", json!("")),
        ("locale", json!("english")),
        ("nested_array", json!(3)),
        ("truncate_len", json!(u64::from(u32::MAX) + 1)),
    ] {
        let mut field = json!({"name": "title", "type": "string"});
        field[member] = malformed_value;
        assert_typesense_field_rejected(field, &format!("$.fields[0].{member}"));
    }
}

#[test]
fn translate_typesense_invalid_field_member_combinations_fail_closed_at_exact_path() {
    let specimens = [
        (
            json!({"name": "title", "type": "string", "num_dim": 3}),
            "$.fields[0].num_dim",
        ),
        (
            json!({"name": "title", "type": "string", "vec_dist": "cosine"}),
            "$.fields[0].vec_dist",
        ),
        (
            json!({
                "name": "title",
                "type": "string",
                "embed": {
                    "from": ["title"],
                    "model_config": {"model_name": "ts/all-MiniLM-L12-v2"}
                }
            }),
            "$.fields[0].embed",
        ),
        (
            json!({
                "name": "title",
                "type": "string",
                "hnsw_params": {"M": 16, "ef_construction": 200}
            }),
            "$.fields[0].hnsw_params",
        ),
        (
            json!({"name": "category_id", "type": "string", "async_reference": true}),
            "$.fields[0].async_reference",
        ),
        (
            json!({"name": "category_id", "type": "string", "cascade_delete": false}),
            "$.fields[0].cascade_delete",
        ),
    ];

    for (field, expected_path) in specimens {
        assert_typesense_field_rejected(field, expected_path);
    }
}

#[test]
fn translate_typesense_malformed_vector_objects_fail_closed_at_exact_inner_path() {
    let specimens = [
        (
            "embed",
            json!({"future_option": true}),
            "$.fields[0].embed.future_option",
        ),
        (
            "embed",
            json!({
                "from": "title",
                "model_config": {"model_name": "ts/all-MiniLM-L12-v2"}
            }),
            "$.fields[0].embed.from",
        ),
        (
            "embed",
            json!({
                "from": ["title"],
                "model_config": {"model_name": "ts/all-MiniLM-L12-v2", "future_option": true}
            }),
            "$.fields[0].embed.model_config.future_option",
        ),
        (
            "embed",
            json!({"from": ["title"], "model_config": {}}),
            "$.fields[0].embed.model_config.model_name",
        ),
        (
            "hnsw_params",
            json!({"M": 16, "ef_construction": 200, "future_option": true}),
            "$.fields[0].hnsw_params.future_option",
        ),
        (
            "embed",
            json!({
                "from": ["title"],
                "mapping": "document_title",
                "model_config": {"model_name": "ts/all-MiniLM-L12-v2"}
            }),
            "$.fields[0].embed.mapping",
        ),
        (
            "embed",
            json!({
                "from": ["title"],
                "mapping": ["document_title", "extra"],
                "model_config": {"model_name": "ts/all-MiniLM-L12-v2"}
            }),
            "$.fields[0].embed.mapping",
        ),
        (
            "embed",
            json!({
                "from": ["title"],
                "model_config": {
                    "model_name": "openai/text-embedding-3-small",
                    "path": ""
                }
            }),
            "$.fields[0].embed.model_config.path",
        ),
        (
            "embed",
            json!({
                "from": ["title"],
                "model_config": {
                    "model_name": "ts/personalization",
                    "personalization_type": "search"
                }
            }),
            "$.fields[0].embed.model_config.personalization_type",
        ),
        (
            "embed",
            json!({
                "from": ["title"],
                "model_config": {
                    "model_name": "google/gemini-embedding-001",
                    "service_account": {
                        "client_email": "embeddings@example.test",
                        "private_key": "fixture-private-key",
                        "token_uri": ""
                    }
                }
            }),
            "$.fields[0].embed.model_config.service_account.token_uri",
        ),
        (
            "embed",
            json!({
                "from": ["title"],
                "model_config": {
                    "model_name": "google/gemini-embedding-001",
                    "service_account": {
                        "client_email": "embeddings@example.test",
                        "private_key": "fixture-private-key",
                        "future_option": true
                    }
                }
            }),
            "$.fields[0].embed.model_config.service_account.future_option",
        ),
        (
            "hnsw_params",
            json!({"M": 16, "ef_construction": 200, "max_elements": 0}),
            "$.fields[0].hnsw_params.max_elements",
        ),
        (
            "hnsw_params",
            json!({"M": 16, "ef_construction": 200, "ef": "10"}),
            "$.fields[0].hnsw_params.ef",
        ),
        (
            "hnsw_params",
            json!({"M": 0, "ef_construction": 200}),
            "$.fields[0].hnsw_params.M",
        ),
        (
            "hnsw_params",
            json!({"M": 16, "ef_construction": "200"}),
            "$.fields[0].hnsw_params.ef_construction",
        ),
    ];

    for (member, malformed_value, expected_path) in specimens {
        let mut field = json!({"name": "embedding", "type": "float[]"});
        field[member] = malformed_value;
        assert_typesense_field_rejected(field, expected_path);
    }
}

#[test]
fn translate_typesense_malformed_service_account_members_fail_closed_at_exact_path() {
    let members = [
        "type",
        "project_id",
        "private_key_id",
        "private_key",
        "client_email",
        "client_id",
        "auth_uri",
        "token_uri",
        "auth_provider_x509_cert_url",
        "client_x509_cert_url",
        "universe_domain",
    ];

    for member in members {
        let mut service_account = json!({
            "client_email": "embeddings@example.test",
            "private_key": "fixture-private-key"
        });
        service_account[member] = json!("");
        assert_typesense_field_rejected(
            json!({
                "name": "embedding",
                "type": "float[]",
                "num_dim": 3,
                "embed": {
                    "from": ["title"],
                    "model_config": {
                        "model_name": "google/gemini-embedding-001",
                        "service_account": service_account
                    }
                }
            }),
            &format!("$.fields[0].embed.model_config.service_account.{member}"),
        );
    }
}

#[test]
fn typesense_settings_accepts_server_returned_locale_shapes_with_attributed_warnings() {
    for locale in ["jp", "zz", "1!", "A_", "JA", "de_en"] {
        let settings = json!({"fields": [{"name": "title", "type": "string", "locale": locale}]});
        let mut failures = Vec::new();
        let mut warnings = Vec::new();

        let translated = translate_settings_for_provider(
            &settings,
            SettingsSourceProvider::Typesense,
            &mut failures,
            &mut warnings,
        )
        .unwrap_or_else(|| panic!("Typesense 30.2 response locale {locale:?} must translate"));

        assert!(failures.is_empty());
        assert_eq!(
            translated.searchable_attributes,
            Some(vec!["title".to_string()])
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, ReportCode::TypesenseSettingNotMigrated);
        assert_eq!(warnings[0].json_path, "$.fields[0].locale");
    }
}

#[test]
fn translate_typesense_locale_values_rejected_by_source_fail_closed_at_exact_path() {
    for malformed_locale in ["éx", "english", "en-US"] {
        assert_typesense_field_rejected(
            json!({"name": "title", "type": "string", "locale": malformed_locale}),
            "$.fields[0].locale",
        );
    }
}
