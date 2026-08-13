use super::*;

#[tokio::test]
async fn source_capture_reads_proved_endpoints_and_requires_stable_collection_metadata() {
    let mut transport = ScriptedTransport {
        responses: VecDeque::from(capture_responses()),
        requests: Vec::new(),
    };
    let mut ids = Vec::new();
    let capture = capture_source_with_transport(&mut transport, "catalog", |documents| {
        ids.extend(
            documents
                .iter()
                .map(|document| document["id"].as_str().unwrap().to_string()),
        );
        Ok::<_, TypesenseClientError>(())
    })
    .await
    .unwrap();

    assert_eq!(capture.observation().source_name, "catalog");
    assert_eq!(capture.observation().document_count, 3);
    assert_eq!(capture.settings["default_sorting_field"], "price");
    assert_eq!(ids, vec!["prod_001", "prod_002", "prod_003"]);
    assert_eq!(
        transport
            .requests
            .iter()
            .map(|request| (request.method, request.path.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (TypesenseMethod::Get, "/collections/catalog"),
            (
                TypesenseMethod::Get,
                "/collections/catalog/documents/export"
            ),
            (TypesenseMethod::Get, "/collections/catalog"),
        ]
    );
}

#[tokio::test]
async fn collection_settings_preserve_linked_config_and_identity() {
    let mut transport = ScriptedTransport::with_json_responses([json!({
        "name": "catalog",
        "num_documents": 0,
        "created_at": 1785020400_u64,
        "fields": [
            {"name": "id", "type": "string"},
            {"name": "embedding", "type": "float[]", "num_dim": 384},
            {"name": "category_id", "type": "string", "reference": "categories.id"}
        ],
        "synonym_sets": ["catalog_synonyms"],
        "curation_sets": ["catalog_curations"]
    })]);

    let settings = read_settings_with_transport(&mut transport, "catalog")
        .await
        .unwrap();

    assert_eq!(settings["synonym_sets"], json!(["catalog_synonyms"]));
    assert_eq!(settings["curation_sets"], json!(["catalog_curations"]));
    assert_eq!(
        settings["fields"],
        json!([
            {"name": "id", "type": "string"},
            {"name": "embedding", "type": "float[]", "num_dim": 384},
            {"name": "category_id", "type": "string", "reference": "categories.id"}
        ]),
        "schema, vector, and reference metadata must reach attributed translation warnings"
    );
}

#[tokio::test]
async fn collection_settings_preserve_unproved_top_level_keys_for_translation_warnings() {
    let mut transport = ScriptedTransport::with_json_responses([json!({
        "name": "catalog",
        "num_documents": 0,
        "created_at": 1785020400_u64,
        "fields": [{"name": "id", "type": "string"}],
        "query_by": "title,description",
        "facet_by": ["category"],
        "default_sorting_field": null
    })]);

    let settings = read_settings_with_transport(&mut transport, "catalog")
        .await
        .unwrap();

    assert_eq!(settings["query_by"], "title,description");
    assert_eq!(settings["facet_by"], json!(["category"]));
    assert_eq!(settings["default_sorting_field"], Value::Null);
}

#[tokio::test]
async fn source_observation_changes_when_collection_schema_changes() {
    let mut first = ScriptedTransport::with_json_responses([collection(0)]);
    let mut changed_collection = collection(0);
    changed_collection["fields"] = json!([
        {"name": "id", "type": "string"},
        {"name": "category", "type": "string", "facet": true}
    ]);
    let mut second = ScriptedTransport::with_json_responses([changed_collection]);

    let before = observe_source_with_transport(&mut first, "catalog")
        .await
        .unwrap();
    let after = observe_source_with_transport(&mut second, "catalog")
        .await
        .unwrap();

    assert_ne!(
        before, after,
        "Typesense collection schema must participate in shared source identity"
    );
}

#[tokio::test]
async fn collection_name_mismatch_rejects_uncertified_alias_resolution() {
    let mut aliased_collection = collection(0);
    aliased_collection["name"] = json!("catalog_v2");
    let mut transport = ScriptedTransport::with_json_responses([aliased_collection]);

    let error = read_settings_with_transport(&mut transport, "catalog")
        .await
        .expect_err("a returned collection name must match the requested collection");

    assert_eq!(error.kind(), TypesenseErrorKind::Progress);
    assert_eq!(
        error.safe_message(),
        "Typesense source changed during export"
    );
    assert_error_is_sanitized(&error);
}

#[tokio::test]
async fn source_capture_rejects_schema_errors_and_source_drift() {
    let mut missing_name = capture_responses();
    missing_name[0] = json_response(json!({"num_documents": 3, "fields": []}));
    let mut missing_fields = capture_responses();
    let collection_without_fields = json!({
        "name": "catalog",
        "num_documents": 3,
        "created_at": 1785020400_u64
    });
    missing_fields[0] = json_response(collection_without_fields.clone());
    missing_fields[2] = json_response(collection_without_fields);
    let mut malformed_fields = capture_responses();
    let collection_with_malformed_fields = json!({
        "name": "catalog",
        "num_documents": 3,
        "created_at": 1785020400_u64,
        "fields": [{"name": "id", "type": "string"}, "invalid-field"]
    });
    malformed_fields[0] = json_response(collection_with_malformed_fields.clone());
    malformed_fields[2] = json_response(collection_with_malformed_fields);
    let mut metadata_drift = capture_responses();
    metadata_drift[2] = json_response(json!({
        "name": "catalog",
        "num_documents": 4,
        "created_at": 1785020400_u64,
        "fields": [{"name": "id", "type": "string"}]
    }));
    let mut document_count_drift = capture_responses();
    document_count_drift[1] = export_response(page(&["prod_001", "prod_002"]));

    for responses in [
        missing_name,
        missing_fields,
        malformed_fields,
        metadata_drift,
        document_count_drift,
    ] {
        let mut transport = ScriptedTransport {
            responses: VecDeque::from(responses),
            requests: Vec::new(),
        };
        let error = capture_source_with_transport(&mut transport, "catalog", |_| {
            Ok::<_, TypesenseClientError>(())
        })
        .await
        .unwrap_err();
        assert!(
            matches!(
                error.kind(),
                TypesenseErrorKind::Schema | TypesenseErrorKind::Progress
            ),
            "unexpected source capture error: {error:?}"
        );
        assert_error_is_sanitized(&error);
    }
}
