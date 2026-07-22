use super::*;
use crate::openapi::{documented_openapi, DOCUMENTED_INTERNAL_MEMBERSHIP_PATHS};
use crate::openapi_test_helpers::{
    assert_add_peer_openapi_contract, assert_high_risk_mutation_contracts,
    assert_remove_peer_openapi_contract, schema_composition_refs, schema_ref,
};

fn openapi_json() -> serde_json::Value {
    serde_json::to_value(ApiDoc::openapi()).unwrap()
}

#[test]
fn openapi_membership_add_contract_is_documented() {
    assert_add_peer_openapi_contract(&openapi_json());
}

#[test]
fn openapi_membership_remove_contract_is_documented() {
    assert_remove_peer_openapi_contract(&openapi_json());
}

#[test]
fn openapi_membership_paths_are_hidden_when_auth_is_disabled() {
    let doc = serde_json::to_value(documented_openapi(false)).unwrap();
    let paths = doc
        .get("paths")
        .and_then(|value| value.as_object())
        .expect("spec must have paths");

    for path in DOCUMENTED_INTERNAL_MEMBERSHIP_PATHS {
        assert!(
            !paths.contains_key(path),
            "no-auth OpenAPI should not advertise {path}"
        );
    }
}

fn schema_contains_type(schema: &serde_json::Value, expected_type: &str) -> bool {
    match schema {
        serde_json::Value::Object(map) => {
            if map
                .get("type")
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == expected_type)
            {
                return true;
            }

            if map
                .get("type")
                .and_then(|value| value.as_array())
                .is_some_and(|values| {
                    values
                        .iter()
                        .any(|value| value.as_str() == Some(expected_type))
                })
            {
                return true;
            }

            map.values()
                .any(|value| schema_contains_type(value, expected_type))
        }
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| schema_contains_type(value, expected_type)),
        _ => false,
    }
}

fn schema_contains_ref(schema: &serde_json::Value, expected_ref: &str) -> bool {
    match schema {
        serde_json::Value::Object(map) => {
            if map
                .get("$ref")
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == expected_ref)
            {
                return true;
            }

            map.values()
                .any(|value| schema_contains_ref(value, expected_ref))
        }
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| schema_contains_ref(value, expected_ref)),
        _ => false,
    }
}

#[test]
fn key_endpoints_use_concrete_schema_components() {
    let doc = openapi_json();

    assert_eq!(
        doc.pointer("/paths/~11~1keys/get/responses/200/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/ListKeysResponse")
    );
    assert_eq!(
        doc.pointer(
            "/paths/~11~1keys~1{key}/get/responses/200/content/application~1json/schema/$ref"
        )
        .and_then(|v| v.as_str()),
        Some("#/components/schemas/KeyApiResponse")
    );
    assert_eq!(
        doc.pointer("/paths/~11~1keys/post/requestBody/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/CreateKeyRequest")
    );
    assert_eq!(
        doc.pointer("/paths/~11~1keys/post/responses/200/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/CreateKeyResponse")
    );
    assert_eq!(
        doc.pointer(
            "/paths/~11~1keys~1{key}/put/responses/200/content/application~1json/schema/$ref"
        )
        .and_then(|v| v.as_str()),
        Some("#/components/schemas/UpdateKeyResponse")
    );
    assert_eq!(
        doc.pointer(
            "/paths/~11~1keys~1{key}/delete/responses/200/content/application~1json/schema/$ref"
        )
        .and_then(|v| v.as_str()),
        Some("#/components/schemas/DeleteKeyResponse")
    );
    assert_eq!(
            doc.pointer(
                "/paths/~11~1keys~1{key}~1restore/post/responses/200/content/application~1json/schema/$ref"
            )
            .and_then(|v| v.as_str()),
            Some("#/components/schemas/RestoreKeyResponse")
        );
    assert_eq!(
            doc.pointer(
                "/paths/~11~1keys~1generateSecuredApiKey/post/responses/200/content/application~1json/schema/$ref"
            )
            .and_then(|v| v.as_str()),
            Some("#/components/schemas/GenerateSecuredKeyResponse")
        );
}

/// Verify that the list-indices endpoint uses a concrete `ListIndicesResponse` schema with properly typed `ListIndexItem` fields.
#[test]
fn list_indices_uses_concrete_response_schema() {
    let doc = openapi_json();

    assert_eq!(
        doc.pointer("/paths/~11~1indexes/get/responses/200/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/ListIndicesResponse")
    );
    assert_eq!(
        doc.pointer("/components/schemas/ListIndicesResponse/properties/items/items/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/ListIndexItem")
    );
    assert_eq!(
        doc.pointer("/components/schemas/ListIndexItem/properties/lastBuildTimeS/type")
            .and_then(|v| v.as_str()),
        Some("integer")
    );
}

/// Helper: assert that a given path key exists in the generated spec.
fn assert_path_exists(doc: &serde_json::Value, path_key: &str) {
    let paths = doc.get("paths").expect("spec must have paths");
    assert!(
        paths.get(path_key).is_some(),
        "expected path '{}' in OpenAPI spec, found: {:?}",
        path_key,
        paths.as_object().map(|m| m.keys().collect::<Vec<_>>())
    );
}

/// Helper: assert that a given HTTP method exists on a path.
fn assert_path_method(doc: &serde_json::Value, path_key: &str, method: &str) {
    let paths = doc.get("paths").expect("spec must have paths");
    let path_obj = paths
        .get(path_key)
        .unwrap_or_else(|| panic!("path '{}' missing from spec", path_key));
    assert!(
        path_obj.get(method).is_some(),
        "expected method '{}' on path '{}', found: {:?}",
        method,
        path_key,
        path_obj.as_object().map(|m| m.keys().collect::<Vec<_>>())
    );
}

fn assert_component_schema_exists(doc: &serde_json::Value, schema_name: &str) {
    assert!(
        doc.pointer(&format!("/components/schemas/{schema_name}"))
            .is_some(),
        "expected OpenAPI component schema {schema_name}"
    );
}

fn assert_operation_uses_api_key(doc: &serde_json::Value, path: &str) {
    let escaped_path = path.replace('/', "~1");
    let security = doc
        .pointer(&format!("/paths/{escaped_path}/post/security"))
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("{path} POST should document operation security"));

    assert!(
        security.iter().any(|entry| entry.get("api_key").is_some()),
        "{path} POST should require api_key security"
    );
}

/// Stage 7: Verify recommendation endpoint appears in the generated spec.
#[test]
fn recommendations_endpoint_is_documented() {
    let doc = openapi_json();
    assert_path_exists(&doc, "/1/indexes/*/recommendations");
    assert_path_method(&doc, "/1/indexes/*/recommendations", "post");
    assert_eq!(
            doc.pointer(
                "/paths/~11~1indexes~1*~1recommendations/post/requestBody/content/application~1json/schema/$ref"
            )
            .and_then(|v| v.as_str()),
            Some("#/components/schemas/RecommendBatchRequest")
        );
    assert_eq!(
            doc.pointer(
                "/paths/~11~1indexes~1*~1recommendations/post/responses/200/content/application~1json/schema/$ref"
            )
            .and_then(|v| v.as_str()),
            Some("#/components/schemas/RecommendBatchResponse")
        );
    let paths = doc.get("paths").expect("spec must have paths");
    assert!(
        paths
            .get("/1/indexes/{indexName}/recommendations")
            .is_none(),
        "unexpected parameterized recommendations path; router exposes wildcard variant"
    );
}

/// Stage 7: Verify personalization strategy endpoints appear in the generated spec.
#[test]
fn personalization_strategy_endpoints_are_documented() {
    let doc = openapi_json();
    assert_path_exists(&doc, "/1/strategies/personalization");
    assert_path_method(&doc, "/1/strategies/personalization", "post");
    assert_path_method(&doc, "/1/strategies/personalization", "get");
}

/// Stage 7: Verify personalization profile endpoints appear in the generated spec.
#[test]
fn personalization_profile_endpoints_are_documented() {
    let doc = openapi_json();
    assert_path_exists(&doc, "/1/profiles/personalization/{userToken}");
    assert_path_method(&doc, "/1/profiles/personalization/{userToken}", "get");
    assert_path_exists(&doc, "/1/profiles/{userToken}");
    assert_path_method(&doc, "/1/profiles/{userToken}", "delete");
}

/// Stage 7: Verify experiment lifecycle endpoints appear in the generated spec.
#[test]
fn experiments_endpoints_are_documented() {
    let doc = openapi_json();

    // CRUD
    assert_path_exists(&doc, "/2/abtests");
    assert_path_method(&doc, "/2/abtests", "post");
    assert_path_method(&doc, "/2/abtests", "get");

    // Estimate
    assert_path_exists(&doc, "/2/abtests/estimate");
    assert_path_method(&doc, "/2/abtests/estimate", "post");

    // Single experiment
    assert_path_exists(&doc, "/2/abtests/{id}");
    assert_path_method(&doc, "/2/abtests/{id}", "get");
    assert_path_method(&doc, "/2/abtests/{id}", "put");
    assert_path_method(&doc, "/2/abtests/{id}", "delete");

    // Lifecycle actions
    assert_path_exists(&doc, "/2/abtests/{id}/start");
    assert_path_method(&doc, "/2/abtests/{id}/start", "post");
    assert_path_exists(&doc, "/2/abtests/{id}/stop");
    assert_path_method(&doc, "/2/abtests/{id}/stop", "post");
    assert_path_exists(&doc, "/2/abtests/{id}/conclude");
    assert_path_method(&doc, "/2/abtests/{id}/conclude", "post");

    // Results
    assert_path_exists(&doc, "/2/abtests/{id}/results");
    assert_path_method(&doc, "/2/abtests/{id}/results", "get");
}

#[test]
fn abtests_list_exposes_index_name_query_parameter() {
    let doc = openapi_json();
    let operation = doc
        .pointer("/paths/~12~1abtests/get")
        .expect("list experiments operation should exist");
    let parameters = operation["parameters"]
        .as_array()
        .expect("list experiments parameters should be an array");
    let index_name_parameters: Vec<_> = parameters
        .iter()
        .filter(|parameter| parameter["name"] == "indexName")
        .collect();

    let expected_summary = "Lists experiments with Algolia-compatible pagination and optional exact, prefix, or suffix index-name filtering.";
    assert_eq!(
        (index_name_parameters.len(), operation["summary"].as_str()),
        (1, Some(expected_summary))
    );
    let index_name_parameter = index_name_parameters[0];
    assert_eq!(index_name_parameter["in"], "query");
    assert!(index_name_parameter
        .get("required")
        .is_none_or(|required| required == false));
    assert!(schema_contains_type(
        &index_name_parameter["schema"],
        "string"
    ));
}

#[test]
fn experiment_estimate_and_results_endpoints_use_concrete_schemas() {
    let doc = openapi_json();

    assert_eq!(
        schema_ref(
            &doc,
            "/paths/~12~1abtests~1estimate/post/requestBody/content/application~1json/schema"
        ),
        Some("#/components/schemas/AlgoliaEstimateRequest")
    );
    assert_eq!(
        schema_ref(
            &doc,
            "/paths/~12~1abtests~1estimate/post/responses/200/content/application~1json/schema"
        ),
        Some("#/components/schemas/AlgoliaEstimateResponse")
    );
    assert_eq!(
        schema_ref(
            &doc,
            "/paths/~12~1abtests~1{id}~1results/get/responses/200/content/application~1json/schema"
        ),
        Some("#/components/schemas/ResultsResponse")
    );

    for (pointer, expected_ref, description) in [
        (
            "/components/schemas/ResultsResponse/properties/gate",
            "#/components/schemas/GateResponse",
            "gate",
        ),
        (
            "/components/schemas/ResultsResponse/properties/control",
            "#/components/schemas/ArmResponse",
            "control",
        ),
        (
            "/components/schemas/ResultsResponse/properties/variant",
            "#/components/schemas/ArmResponse",
            "variant",
        ),
        (
            "/components/schemas/ResultsResponse/properties/interleaving",
            "#/components/schemas/InterleavingResponse",
            "interleaving",
        ),
    ] {
        let schema = doc.pointer(pointer).expect("results property should exist");
        assert!(
            schema_contains_ref(schema, expected_ref),
            "{description} should reference {expected_ref}"
        );
    }

    for (pointer, description) in [
        (
            "/components/schemas/QueryOverrides/properties/typoTolerance",
            "queryOverrides.typoTolerance",
        ),
        (
            "/components/schemas/AlgoliaVariant/properties/customSearchParameters",
            "customSearchParameters",
        ),
        (
            "/components/schemas/AlgoliaVariant/properties/currencies",
            "currencies",
        ),
        (
            "/components/schemas/AlgoliaAbTest/properties/revenueSignificance",
            "revenueSignificance",
        ),
    ] {
        let schema = doc
            .pointer(pointer)
            .expect("dynamic object field should exist");
        assert!(
            schema_contains_type(schema, "object"),
            "{description} should be documented as object-valued"
        );
    }
}

#[test]
fn experiment_endpoints_document_internal_mapping_500s() {
    let doc = openapi_json();

    for (pointer, description) in [
        (
            "/paths/~12~1abtests/post/responses/500/description",
            "create experiment",
        ),
        (
            "/paths/~12~1abtests/get/responses/500/description",
            "list experiments",
        ),
        (
            "/paths/~12~1abtests~1{id}/get/responses/500/description",
            "get experiment",
        ),
        (
            "/paths/~12~1abtests~1{id}/put/responses/500/description",
            "update experiment",
        ),
        (
            "/paths/~12~1abtests~1{id}/delete/responses/500/description",
            "delete experiment",
        ),
        (
            "/paths/~12~1abtests~1{id}~1start/post/responses/500/description",
            "start experiment",
        ),
        (
            "/paths/~12~1abtests~1{id}~1stop/post/responses/500/description",
            "stop experiment",
        ),
        (
            "/paths/~12~1abtests~1{id}~1conclude/post/responses/500/description",
            "conclude experiment",
        ),
        (
            "/paths/~12~1abtests~1{id}~1results/get/responses/500/description",
            "results endpoint",
        ),
    ] {
        assert_eq!(
            doc.pointer(pointer).and_then(|value| value.as_str()),
            Some("Experiment missing numeric ID mapping"),
            "{description} should document the resolver's 500 path"
        );
    }
}

#[test]
fn experiment_schema_renamed_properties_and_enum_shapes() {
    let doc = openapi_json();

    let ab_test_props = doc
        .pointer("/components/schemas/AlgoliaAbTest/properties")
        .expect("AlgoliaAbTest properties should exist");
    assert!(
        ab_test_props.get("abTestID").is_some(),
        "should use explicit rename abTestID"
    );
    assert!(
        ab_test_props.get("createdAt").is_some(),
        "should be camelCase createdAt"
    );

    let create_resp_props = doc
        .pointer("/components/schemas/AlgoliaCreateAbTestResponse/properties")
        .expect("AlgoliaCreateAbTestResponse properties should exist");
    assert!(
        create_resp_props.get("abTestID").is_some(),
        "should use explicit rename abTestID"
    );
    assert!(
        create_resp_props.get("taskID").is_some(),
        "should use explicit rename taskID"
    );

    let action_resp_props = doc
        .pointer("/components/schemas/AlgoliaAbTestActionResponse/properties")
        .expect("AlgoliaAbTestActionResponse properties should exist");
    assert!(
        action_resp_props.get("abTestID").is_some(),
        "action response should use abTestID"
    );
    assert!(
        action_resp_props.get("taskID").is_some(),
        "action response should use taskID"
    );

    let variant_props = doc
        .pointer("/components/schemas/AlgoliaVariant/properties")
        .expect("AlgoliaVariant properties should exist");
    assert!(
        variant_props.get("trafficPercentage").is_some(),
        "should be camelCase trafficPercentage"
    );
    assert!(
        variant_props.get("customSearchParameters").is_some(),
        "should be camelCase customSearchParameters"
    );

    let config_props = doc
        .pointer("/components/schemas/AlgoliaConfiguration/properties")
        .expect("AlgoliaConfiguration properties should exist");
    assert!(
        config_props.get("minimumDetectableEffect").is_some(),
        "should be camelCase minimumDetectableEffect"
    );

    let results_props = doc
        .pointer("/components/schemas/ResultsResponse/properties")
        .expect("ResultsResponse properties should exist");
    assert!(
        results_props.get("experimentID").is_some(),
        "should use explicit rename experimentID"
    );
    assert!(
        results_props.get("trafficSplit").is_some(),
        "should be camelCase trafficSplit"
    );
    assert!(
        results_props.get("cupedApplied").is_some(),
        "should be camelCase cupedApplied"
    );
    assert!(
        results_props.get("guardRailAlerts").is_some(),
        "should be camelCase guardRailAlerts"
    );

    let qo_props = doc
        .pointer("/components/schemas/QueryOverrides/properties")
        .expect("QueryOverrides properties should exist");
    assert!(
        qo_props.get("typoTolerance").is_some(),
        "should be camelCase typoTolerance"
    );
    assert!(
        qo_props.get("enableSynonyms").is_some(),
        "should be camelCase enableSynonyms"
    );
    assert!(
        qo_props.get("customRanking").is_some(),
        "should be camelCase customRanking"
    );
    assert!(
        qo_props.get("attributeWeights").is_some(),
        "should be camelCase attributeWeights"
    );

    let primary_metric = doc
        .pointer("/components/schemas/PrimaryMetric")
        .expect("PrimaryMetric schema should exist");
    let enum_values = primary_metric
        .get("enum")
        .and_then(|value| value.as_array())
        .expect("PrimaryMetric should have enum values");
    let enum_strings: Vec<&str> = enum_values
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    for value in [
        "ctr",
        "conversionRate",
        "revenuePerSearch",
        "zeroResultRate",
        "abandonmentRate",
    ] {
        assert!(
            enum_strings.contains(&value),
            "PrimaryMetric should contain {value}"
        );
    }

    let exp_status = doc
        .pointer("/components/schemas/ExperimentStatus")
        .expect("ExperimentStatus schema should exist");
    let status_values = exp_status
        .get("enum")
        .and_then(|value| value.as_array())
        .expect("ExperimentStatus should have enum values");
    let status_strings: Vec<&str> = status_values
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    for value in ["draft", "running", "stopped", "concluded"] {
        assert!(
            status_strings.contains(&value),
            "ExperimentStatus should contain {value}"
        );
    }
}

#[test]
fn high_risk_mutation_openapi_contracts_match_shared_matrix() {
    let doc = openapi_json();
    assert_high_risk_mutation_contracts(&doc);
}

/// Stage 7: Verify SearchRequest schema includes vector/hybrid fields.
#[test]
fn search_request_schema_includes_vector_hybrid_fields() {
    let doc = openapi_json();
    let search_schema = doc
        .pointer("/components/schemas/SearchRequest/properties")
        .expect("SearchRequest schema should have properties");

    assert!(
        search_schema.get("mode").is_some(),
        "SearchRequest should document 'mode' field"
    );
    assert!(
        search_schema.get("hybrid").is_some(),
        "SearchRequest should document 'hybrid' field"
    );
    assert!(
        search_schema.get("relevancyStrictness").is_some(),
        "SearchRequest should document 'relevancyStrictness' field"
    );
}

/// Stage 7: Verify recommendation and experiment schemas are registered as components.
#[test]
fn stage7_schemas_are_registered() {
    let doc = openapi_json();
    let schemas = doc
        .pointer("/components/schemas")
        .expect("spec must have component schemas");

    // Recommend
    assert!(
        schemas.get("RecommendBatchRequest").is_some(),
        "RecommendBatchRequest schema missing"
    );
    assert!(
        schemas.get("RecommendBatchResponse").is_some(),
        "RecommendBatchResponse schema missing"
    );

    // Experiments (Algolia DTOs)
    assert!(
        schemas.get("AlgoliaCreateAbTestRequest").is_some(),
        "AlgoliaCreateAbTestRequest schema missing"
    );
    assert!(
        schemas.get("AlgoliaListAbTestsResponse").is_some(),
        "AlgoliaListAbTestsResponse schema missing"
    );
}

/// Stage 7: Verify analytics read endpoints appear in the generated spec.
#[test]
fn analytics_read_endpoints_are_documented() {
    let doc = openapi_json();

    let analytics_paths = [
        ("/2/searches", "get"),
        ("/2/searches/count", "get"),
        ("/2/searches/noResults", "get"),
        ("/2/searches/noResultRate", "get"),
        ("/2/searches/noClicks", "get"),
        ("/2/searches/noClickRate", "get"),
        ("/2/clicks/clickThroughRate", "get"),
        ("/2/clicks/averageClickPosition", "get"),
        ("/2/clicks/positions", "get"),
        ("/2/conversions/conversionRate", "get"),
        ("/2/conversions/addToCartRate", "get"),
        ("/2/conversions/purchaseRate", "get"),
        ("/2/conversions/revenue", "get"),
        ("/2/hits", "get"),
        ("/2/filters", "get"),
        ("/2/filters/noResults", "get"),
        ("/2/filters/{attribute}", "get"),
        ("/2/users/count", "get"),
        ("/2/status", "get"),
        ("/2/devices", "get"),
        ("/2/countries", "get"),
        ("/2/geo", "get"),
        ("/2/geo/{country}", "get"),
        ("/2/geo/{country}/regions", "get"),
        ("/2/overview", "get"),
    ];

    for (path, method) in &analytics_paths {
        assert_path_exists(&doc, path);
        assert_path_method(&doc, path, method);
    }
}

/// Stage 7: Verify analytics mutation endpoints appear in the generated spec.
#[test]
fn analytics_mutation_endpoints_are_documented() {
    let doc = openapi_json();

    assert_path_exists(&doc, "/2/analytics/seed");
    assert_path_method(&doc, "/2/analytics/seed", "post");
    assert_path_exists(&doc, "/2/analytics/clear");
    assert_path_method(&doc, "/2/analytics/clear", "delete");
    assert_path_exists(&doc, "/2/analytics/flush");
    assert_path_method(&doc, "/2/analytics/flush", "post");
    assert_path_exists(&doc, "/2/analytics/cleanup");
    assert_path_method(&doc, "/2/analytics/cleanup", "post");
}

/// Stage 7: Verify query suggestions endpoints appear in the generated spec.
#[test]
fn query_suggestions_endpoints_are_documented() {
    let doc = openapi_json();

    assert_path_exists(&doc, "/1/configs");
    assert_path_method(&doc, "/1/configs", "get");
    assert_path_method(&doc, "/1/configs", "post");

    assert_path_exists(&doc, "/1/configs/{indexName}");
    assert_path_method(&doc, "/1/configs/{indexName}", "get");
    assert_path_method(&doc, "/1/configs/{indexName}", "put");
    assert_path_method(&doc, "/1/configs/{indexName}", "delete");

    assert_path_exists(&doc, "/1/configs/{indexName}/status");
    assert_path_method(&doc, "/1/configs/{indexName}/status", "get");

    assert_path_exists(&doc, "/1/configs/{indexName}/build");
    assert_path_method(&doc, "/1/configs/{indexName}/build", "post");

    assert_path_exists(&doc, "/1/logs/{indexName}");
    assert_path_method(&doc, "/1/logs/{indexName}", "get");
}

/// Stage 7: Verify dictionary endpoints appear in the generated spec.
#[test]
fn dictionaries_endpoints_are_documented() {
    let doc = openapi_json();

    assert_path_exists(&doc, "/1/dictionaries/{dictionaryName}/batch");
    assert_path_method(&doc, "/1/dictionaries/{dictionaryName}/batch", "post");

    assert_path_exists(&doc, "/1/dictionaries/{dictionaryName}/search");
    assert_path_method(&doc, "/1/dictionaries/{dictionaryName}/search", "post");

    assert_path_exists(&doc, "/1/dictionaries/{_wildcard}/settings");
    assert_path_method(&doc, "/1/dictionaries/{_wildcard}/settings", "get");
    assert_path_method(&doc, "/1/dictionaries/{_wildcard}/settings", "put");

    assert_path_exists(&doc, "/1/dictionaries/{_wildcard}/languages");
    assert_path_method(&doc, "/1/dictionaries/{_wildcard}/languages", "get");
}

/// Stage 7: Ensure dictionaryName path params stay constrained to DictionaryName enum.
#[test]
fn dictionaries_dictionary_name_param_uses_enum_schema() {
    let doc = openapi_json();

    let param_schema_ref = doc
        .pointer("/paths/~11~1dictionaries~1{dictionaryName}~1batch/post/parameters/0/schema/$ref")
        .expect("dictionaryName path parameter schema ref should exist");

    assert_eq!(
        param_schema_ref, "#/components/schemas/DictionaryName",
        "dictionaryName must reference DictionaryName enum schema instead of free-form string"
    );
}

/// Stage 7: Ensure typed request bodies are wired to shared component schemas.
#[test]
fn stage7_typed_request_bodies_use_component_schemas() {
    let doc = openapi_json();
    assert_operation_uses_api_key(&doc, "/1/migrate-from-algolia");
    assert_operation_uses_api_key(&doc, "/1/migrations/algolia");
    assert_operation_uses_api_key(&doc, "/1/algolia-list-indexes");
    for schema_name in [
        "MigrateFromAlgoliaRequest",
        "ListAlgoliaIndexesRequest",
        "AsyncMigrationStatusResponse",
        "AsyncMigrationPhase",
        "AsyncMigrationDisposition",
        "AsyncMigrationExportProgress",
    ] {
        assert_component_schema_exists(&doc, schema_name);
    }

    assert_eq!(
        doc.pointer("/paths/~11~1configs/post/requestBody/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/QsConfig")
    );
    assert_eq!(
            doc.pointer("/paths/~11~1configs~1{indexName}/put/requestBody/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/QsConfig")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1dictionaries~1{dictionaryName}~1batch/post/requestBody/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/BatchDictionaryRequest")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1dictionaries~1{dictionaryName}~1search/post/requestBody/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/DictionarySearchRequest")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1dictionaries~1{_wildcard}~1settings/put/requestBody/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/DictionarySettings")
        );
    assert_eq!(
        doc.pointer("/paths/~11~1events/post/requestBody/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/InsightsRequest")
    );
    assert_eq!(
            doc.pointer("/paths/~11~1migrate-from-algolia/post/requestBody/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/MigrateFromAlgoliaRequest")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1migrations~1algolia/post/requestBody/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/MigrateFromAlgoliaRequest")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1algolia-list-indexes/post/requestBody/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/ListAlgoliaIndexesRequest")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1indexes~1{indexName}~1chat/post/requestBody/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/ChatRequest")
        );
}

/// Stage 7: Ensure typed responses use shared schemas instead of anonymous objects.
#[test]
fn stage7_typed_responses_use_component_schemas() {
    let doc = openapi_json();
    for schema_name in [
        "MigrateFromAlgoliaResponse",
        "MigrateWarning",
        "MigrateCount",
        "AlgoliaIndexInfo",
        "ListAlgoliaIndexesResponse",
        "AsyncMigrationStatusResponse",
        "AsyncMigrationPhase",
        "AsyncMigrationDisposition",
        "AsyncMigrationExportProgress",
    ] {
        assert_component_schema_exists(&doc, schema_name);
    }

    assert_eq!(
        schema_ref(
            &doc,
            "/components/schemas/AsyncMigrationStatusResponse/properties/exportProgress"
        ),
        Some("#/components/schemas/AsyncMigrationExportProgress")
    );
    assert_eq!(
        schema_ref(
            &doc,
            "/components/schemas/AsyncMigrationStatusResponse/properties/phase"
        ),
        Some("#/components/schemas/AsyncMigrationPhase")
    );
    assert_eq!(
        schema_ref(
            &doc,
            "/components/schemas/AsyncMigrationStatusResponse/properties/disposition"
        ),
        Some("#/components/schemas/AsyncMigrationDisposition")
    );
    assert!(
        doc.pointer("/components/schemas/AsyncMigrationStatusResponse/properties/overallProgress")
            .is_none(),
        "async migration status schema must not expose overall progress"
    );
    assert!(
        doc.pointer("/components/schemas/AsyncMigrationExportProgress/properties/ratio")
            .is_none(),
        "export progress schema must expose counts only"
    );
    assert_eq!(
        doc.pointer(
            "/components/schemas/MigrateFromAlgoliaResponse/properties/warnings/items/$ref"
        )
        .and_then(|v| v.as_str()),
        Some("#/components/schemas/MigrateWarning")
    );
    assert_eq!(
        doc.pointer("/paths/~11~1configs/get/responses/200/content/application~1json/schema/type")
            .and_then(|v| v.as_str()),
        Some("array")
    );
    assert_eq!(
        doc.pointer(
            "/paths/~11~1configs/get/responses/200/content/application~1json/schema/items/$ref"
        )
        .and_then(|v| v.as_str()),
        Some("#/components/schemas/QsConfig")
    );
    assert_eq!(
            doc.pointer("/paths/~11~1configs~1{indexName}/get/responses/200/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/QsConfig")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1configs~1{indexName}~1status/get/responses/200/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/BuildStatus")
        );
    assert_eq!(
        doc.pointer(
            "/paths/~11~1logs~1{indexName}/get/responses/200/content/application~1json/schema/type"
        )
        .and_then(|v| v.as_str()),
        Some("array")
    );
    assert_eq!(
            doc.pointer("/paths/~11~1logs~1{indexName}/get/responses/200/content/application~1json/schema/items/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/LogEntry")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1dictionaries~1{dictionaryName}~1batch/post/responses/200/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/MutationResponse")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1dictionaries~1{dictionaryName}~1search/post/responses/200/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/DictionarySearchResponse")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1dictionaries~1{_wildcard}~1settings/get/responses/200/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/DictionarySettings")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1dictionaries~1{_wildcard}~1settings/put/responses/200/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/MutationResponse")
        );
    // Migration import response-set coverage is owned by openapi_tests_endpoints.rs.
    assert_eq!(
            doc.pointer("/paths/~11~1migrations~1algolia/post/responses/202/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/AsyncMigrationStatusResponse")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1migrations~1algolia~1{job_id}/get/responses/200/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/AsyncMigrationStatusResponse")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1algolia-list-indexes/post/responses/200/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/ListAlgoliaIndexesResponse")
        );
    assert_eq!(
            doc.pointer("/paths/~11~1indexes~1{indexName}~1chat/post/responses/200/content/application~1json/schema/$ref")
                .and_then(|v| v.as_str()),
            Some("#/components/schemas/ChatResponse")
        );
}

#[test]
fn migration_tag_is_registered_once() {
    let doc = openapi_json();
    let migration_tags = doc
        .get("tags")
        .and_then(|value| value.as_array())
        .expect("spec must document tags")
        .iter()
        .filter(|tag| tag.get("name").and_then(|value| value.as_str()) == Some("migration"))
        .count();

    assert_eq!(
        migration_tags, 1,
        "migration tag should appear exactly once"
    );
}

#[path = "openapi_tests_endpoints.rs"]
mod endpoints;
