use crate::mutation_parity::HIGH_RISK_MUTATION_PARITY_CASES;
use crate::openapi::{DOCUMENTED_INTERNAL_MEMBERSHIP_PATHS, DOCUMENTED_MEMBERSHIP_SCHEMA_NAMES};

fn operation<'a>(doc: &'a serde_json::Value, path: &str, method: &str) -> &'a serde_json::Value {
    let escaped_path = path.replace('/', "~1");
    doc.pointer(&format!("/paths/{escaped_path}/{method}"))
        .unwrap_or_else(|| panic!("expected {method} operation on OpenAPI path {path}"))
}

fn assert_schema_ref(
    operation: &serde_json::Value,
    pointer: &str,
    expected_schema: &str,
    context: &str,
) {
    assert_eq!(
        operation.pointer(pointer).and_then(|value| value.as_str()),
        Some(expected_schema),
        "{context} should use {expected_schema}"
    );
}

fn assert_response_statuses(
    operation: &serde_json::Value,
    expected_statuses: &[&str],
    context: &str,
) {
    let responses = operation
        .get("responses")
        .and_then(|value| value.as_object())
        .unwrap_or_else(|| panic!("{context} should document responses"));

    for status in expected_statuses {
        assert!(
            responses.contains_key(*status),
            "{context} should document response {status}"
        );
    }
}

fn assert_api_key_security(operation: &serde_json::Value, context: &str) {
    let security = operation
        .get("security")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("{context} should document operation security"));

    assert!(
        security
            .iter()
            .any(|requirement| requirement.get("api_key").is_some()),
        "{context} should require api_key security"
    );
}

pub(crate) fn assert_add_peer_openapi_contract(doc: &serde_json::Value) {
    let path = DOCUMENTED_INTERNAL_MEMBERSHIP_PATHS[0];
    const CONTEXT: &str = "POST /internal/cluster/peers";
    let operation = operation(doc, path, "post");

    assert_schema_ref(
        operation,
        "/requestBody/content/application~1json/schema/$ref",
        "#/components/schemas/AddPeerRequest",
        CONTEXT,
    );
    assert_schema_ref(
        operation,
        "/responses/200/content/application~1json/schema/$ref",
        "#/components/schemas/AddPeerResponse",
        CONTEXT,
    );
    assert_response_statuses(operation, &["200", "400", "403", "409"], CONTEXT);
    assert_api_key_security(operation, CONTEXT);

    for schema in &DOCUMENTED_MEMBERSHIP_SCHEMA_NAMES[..2] {
        assert!(
            doc.pointer(&format!("/components/schemas/{schema}"))
                .is_some(),
            "{CONTEXT} should register {schema}"
        );
    }
}

pub(crate) fn assert_remove_peer_openapi_contract(doc: &serde_json::Value) {
    let path = DOCUMENTED_INTERNAL_MEMBERSHIP_PATHS[1];
    const CONTEXT: &str = "DELETE /internal/cluster/peers/{node_id}";
    let operation = operation(doc, path, "delete");

    assert_schema_ref(
        operation,
        "/responses/200/content/application~1json/schema/$ref",
        "#/components/schemas/RemovePeerResponse",
        CONTEXT,
    );
    assert_response_statuses(operation, &["200", "400", "403", "404"], CONTEXT);
    assert_api_key_security(operation, CONTEXT);

    let node_id_parameter = operation
        .get("parameters")
        .and_then(|value| value.as_array())
        .and_then(|parameters| {
            parameters.iter().find(|parameter| {
                parameter.get("name").and_then(|value| value.as_str()) == Some("node_id")
            })
        })
        .expect("DELETE membership contract should document node_id");
    assert_eq!(
        node_id_parameter.get("in").and_then(|value| value.as_str()),
        Some("path"),
        "node_id should be a path parameter"
    );
    assert_eq!(
        node_id_parameter
            .get("required")
            .and_then(|value| value.as_bool()),
        Some(true),
        "node_id path parameter should be required"
    );
    assert!(
        doc.pointer(&format!(
            "/components/schemas/{}",
            DOCUMENTED_MEMBERSHIP_SCHEMA_NAMES[2]
        ))
        .is_some(),
        "{CONTEXT} should register RemovePeerResponse"
    );
}

pub(crate) fn assert_cluster_status_openapi_contract(doc: &serde_json::Value) {
    let path = DOCUMENTED_INTERNAL_MEMBERSHIP_PATHS[2];
    const CONTEXT: &str = "GET /internal/cluster/status";
    let operation = operation(doc, path, "get");

    assert_schema_ref(
        operation,
        "/responses/200/content/application~1json/schema/$ref",
        "#/components/schemas/ClusterStatusResponse",
        CONTEXT,
    );
    assert_response_statuses(operation, &["200", "403"], CONTEXT);
    assert_api_key_security(operation, CONTEXT);

    for schema in [
        "ClusterStatusResponse",
        "ClusterStatusStandaloneResponse",
        "ClusterStatusHaResponse",
        "ClusterPeerStatus",
        "ClusterPeerHealthStatus",
        "AutohealPeerLifecycleResponse",
        "AutohealActionResponse",
    ] {
        assert!(
            doc.pointer(&format!("/components/schemas/{schema}"))
                .is_some(),
            "{CONTEXT} should register {schema}"
        );
    }

    assert_eq!(
        schema_ref(
            doc,
            "/components/schemas/ClusterStatusStandaloneResponse/properties/autoheal_peers/items",
        ),
        Some("#/components/schemas/AutohealPeerLifecycleResponse")
    );
    assert_eq!(
        schema_ref(
            doc,
            "/components/schemas/ClusterStatusHaResponse/properties/autoheal_peers/items",
        ),
        Some("#/components/schemas/AutohealPeerLifecycleResponse")
    );
    assert_eq!(
        schema_ref(
            doc,
            "/components/schemas/AutohealPeerLifecycleResponse/properties/action",
        ),
        Some("#/components/schemas/AutohealActionResponse")
    );
    assert!(doc
        .pointer("/components/schemas/AutohealPeerLifecycleResponse/properties/observation_count")
        .is_some());
    assert!(doc
        .pointer("/components/schemas/AutohealPeerLifecycleResponse/properties/decision")
        .is_some());
}

pub(crate) fn schema_ref<'a>(doc: &'a serde_json::Value, schema_pointer: &str) -> Option<&'a str> {
    if let Some(reference) = doc
        .pointer(&format!("{schema_pointer}/$ref"))
        .and_then(|value| value.as_str())
    {
        return Some(reference);
    }

    for composition_key in ["allOf", "anyOf", "oneOf"] {
        if let Some(reference) = doc
            .pointer(&format!("{schema_pointer}/{composition_key}"))
            .and_then(|value| value.as_array())
            .and_then(|variants| {
                variants
                    .iter()
                    .find_map(|variant| variant.get("$ref").and_then(|value| value.as_str()))
            })
        {
            return Some(reference);
        }
    }

    None
}

pub(crate) fn schema_composition_refs<'a>(
    doc: &'a serde_json::Value,
    schema_pointer: &str,
) -> Vec<&'a str> {
    for composition_key in ["oneOf", "anyOf", "allOf"] {
        if let Some(variants) = doc
            .pointer(&format!("{schema_pointer}/{composition_key}"))
            .and_then(|value| value.as_array())
        {
            return variants
                .iter()
                .filter_map(|variant| variant.get("$ref").and_then(|value| value.as_str()))
                .collect();
        }
    }

    Vec::new()
}

/// Shared assertion for the highest-risk mutation endpoints that Stage 1 uses as an OpenAPI guard.
pub(crate) fn assert_high_risk_mutation_contracts(doc: &serde_json::Value) {
    let paths = doc
        .get("paths")
        .and_then(|value| value.as_object())
        .expect("spec must have paths object");

    for case in HIGH_RISK_MUTATION_PARITY_CASES {
        let method = case.method.to_ascii_lowercase();
        let path_item = paths
            .get(case.path)
            .and_then(|value| value.as_object())
            .unwrap_or_else(|| panic!("expected path {} in OpenAPI doc", case.path));
        assert!(
            path_item.contains_key(&method),
            "expected method {} on path {} in OpenAPI doc",
            method,
            case.path
        );

        let response_schema_ref = doc
            .pointer(case.openapi_response_pointer)
            .and_then(|value| value.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "expected OpenAPI response schema ref for {} {} at {}",
                    case.method, case.path, case.openapi_response_pointer
                )
            });

        let schema_pointer = response_schema_ref.trim_start_matches('#');
        let required_fields = doc
            .pointer(schema_pointer)
            .and_then(|schema| schema.get("required"))
            .and_then(|value| value.as_array())
            .unwrap_or_else(|| {
                panic!("expected required fields at schema pointer {schema_pointer}")
            });

        for field in case.required_fields {
            assert!(
                required_fields
                    .iter()
                    .any(|value| value.as_str() == Some(*field)),
                "expected required field {field} for {} {}",
                case.method,
                case.path
            );
        }
    }
}
