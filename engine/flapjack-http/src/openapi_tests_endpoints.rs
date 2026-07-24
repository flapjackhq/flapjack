use std::collections::BTreeSet;

use super::*;

const CLUSTER_STATUS_RESPONSE_SCHEMA: &str = "#/components/schemas/ClusterStatusResponse";
const CLUSTER_STATUS_STANDALONE_SCHEMA: &str =
    "#/components/schemas/ClusterStatusStandaloneResponse";
const CLUSTER_STATUS_HA_SCHEMA: &str = "#/components/schemas/ClusterStatusHaResponse";

/// Stage 7: Verify insights endpoints appear in the generated spec.
#[test]
fn insights_endpoints_are_documented() {
    let doc = openapi_json();

    assert_path_exists(&doc, "/1/events");
    assert_path_method(&doc, "/1/events", "post");

    assert_path_exists(&doc, "/1/events/debug");
    assert_path_method(&doc, "/1/events/debug", "get");

    assert_path_exists(&doc, "/1/usertokens/{userToken}");
    assert_path_method(&doc, "/1/usertokens/{userToken}", "delete");
}

/// Stage 7: Verify migration endpoints appear in the generated spec.
#[test]
fn migration_endpoints_are_documented() {
    let doc = openapi_json();

    assert_path_exists(&doc, "/1/migrate-from-algolia");
    assert_path_method(&doc, "/1/migrate-from-algolia", "post");
    assert_migration_operation_uses_api_key(&doc, "/1/migrate-from-algolia", "post");
    assert_migration_post_documents_admission_refusals(&doc);

    assert_path_exists(&doc, "/1/migrations/algolia");
    assert_path_method(&doc, "/1/migrations/algolia", "post");
    assert_migration_operation_uses_api_key(&doc, "/1/migrations/algolia", "post");
    assert_async_migration_post_documents_contract(&doc);

    assert_path_exists(&doc, "/1/migrations/algolia/{job_id}");
    assert_path_method(&doc, "/1/migrations/algolia/{job_id}", "get");
    assert_migration_operation_uses_api_key(&doc, "/1/migrations/algolia/{job_id}", "get");
    assert_async_migration_get_documents_contract(&doc);

    assert_path_exists(&doc, "/1/migrations/algolia/{job_id}/cancel");
    assert_path_method(&doc, "/1/migrations/algolia/{job_id}/cancel", "post");
    assert_migration_operation_uses_api_key(&doc, "/1/migrations/algolia/{job_id}/cancel", "post");
    assert_async_migration_cancel_documents_contract(&doc);

    assert_path_exists(&doc, "/1/algolia-list-indexes");
    assert_path_method(&doc, "/1/algolia-list-indexes", "post");
    assert_migration_operation_uses_api_key(&doc, "/1/algolia-list-indexes", "post");
}

fn assert_migration_operation_uses_api_key(doc: &serde_json::Value, path: &str, method: &str) {
    let escaped_path = path.replace('/', "~1");
    let security = doc
        .pointer(&format!("/paths/{escaped_path}/{method}/security"))
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("{path} {method} should document operation security"));

    assert!(
        security.iter().any(|entry| entry.get("api_key").is_some()),
        "{path} {method} should require api_key security in OpenAPI"
    );
}

fn assert_migration_post_documents_admission_refusals(doc: &serde_json::Value) {
    let responses = doc
        .pointer("/paths/~11~1migrate-from-algolia/post/responses")
        .and_then(|value| value.as_object())
        .expect("migration POST should document responses");

    for status in ["200", "400", "409", "502", "503"] {
        assert!(
            responses.contains_key(status),
            "migration POST should document restored {status} response"
        );
    }
    let description = responses
        .get("503")
        .and_then(|response| response.get("description"))
        .and_then(serde_json::Value::as_str)
        .expect("migration POST 503 should have a description");
    assert!(
        description.contains("migration_ha_unsupported"),
        "migration POST 503 should document HA refusal code"
    );
    assert!(
        !description.contains("migration_import_unavailable"),
        "migration POST 503 should not document removed import-unavailable refusal code"
    );
}

fn assert_async_migration_post_documents_contract(doc: &serde_json::Value) {
    let responses = doc
        .pointer("/paths/~11~1migrations~1algolia/post/responses")
        .and_then(|value| value.as_object())
        .expect("async migration POST should document responses");

    for status in ["202", "400", "500", "502", "503"] {
        assert!(
            responses.contains_key(status),
            "async migration POST should document {status} response"
        );
    }
    assert_eq!(
        doc.pointer("/paths/~11~1migrations~1algolia/post/responses/202/content/application~1json/schema/$ref")
            .and_then(|value| value.as_str()),
        Some("#/components/schemas/AsyncMigrationStatusResponse")
    );
}

fn assert_async_migration_get_documents_contract(doc: &serde_json::Value) {
    let responses = doc
        .pointer("/paths/~11~1migrations~1algolia~1{job_id}/get/responses")
        .and_then(|value| value.as_object())
        .expect("async migration GET should document responses");

    for status in ["200", "400", "404", "500"] {
        assert!(
            responses.contains_key(status),
            "async migration GET should document {status} response"
        );
    }
    assert_eq!(
        doc.pointer("/paths/~11~1migrations~1algolia~1{job_id}/get/responses/200/content/application~1json/schema/$ref")
            .and_then(|value| value.as_str()),
        Some("#/components/schemas/AsyncMigrationStatusResponse")
    );

    let parameters = doc
        .pointer("/paths/~11~1migrations~1algolia~1{job_id}/get/parameters")
        .and_then(|value| value.as_array())
        .expect("async migration GET should document parameters");
    let job_id = parameters
        .iter()
        .find(|parameter| parameter.get("name").and_then(|value| value.as_str()) == Some("job_id"))
        .expect("async migration GET should document job_id path parameter");
    assert_eq!(
        job_id.get("in").and_then(|value| value.as_str()),
        Some("path")
    );
    assert_eq!(
        job_id
            .pointer("/schema/format")
            .and_then(|value| value.as_str()),
        Some("uuid")
    );
}

fn assert_async_migration_cancel_documents_contract(doc: &serde_json::Value) {
    let responses = doc
        .pointer("/paths/~11~1migrations~1algolia~1{job_id}~1cancel/post/responses")
        .and_then(|value| value.as_object())
        .expect("async migration cancel POST should document responses");

    for status in ["200", "400", "404", "409", "500"] {
        assert!(
            responses.contains_key(status),
            "async migration cancel POST should document {status} response"
        );
    }
    assert_eq!(
        doc.pointer("/paths/~11~1migrations~1algolia~1{job_id}~1cancel/post/responses/200/content/application~1json/schema/$ref")
            .and_then(|value| value.as_str()),
        Some("#/components/schemas/AsyncMigrationStatusResponse")
    );
    let conflict_description = responses
        .get("409")
        .and_then(|response| response.get("description"))
        .and_then(serde_json::Value::as_str)
        .expect("async migration cancel 409 should have a description");
    assert!(
        conflict_description.contains("cancel_too_late"),
        "async migration cancel 409 should document cancel_too_late"
    );
}

/// Stage 7: Verify usage endpoints appear in the generated spec.
#[test]
fn usage_endpoints_are_documented() {
    let doc = openapi_json();

    assert_path_exists(&doc, "/1/usage/{statistic}");
    assert_path_method(&doc, "/1/usage/{statistic}", "get");

    assert_path_exists(&doc, "/1/usage/{statistic}/{indexName}");
    assert_path_method(&doc, "/1/usage/{statistic}/{indexName}", "get");
}

/// Stage 7: Verify chat endpoint appears in the generated spec.
#[test]
fn chat_endpoint_is_documented() {
    let doc = openapi_json();

    assert_path_exists(&doc, "/1/indexes/{indexName}/chat");
    assert_path_method(&doc, "/1/indexes/{indexName}/chat", "post");
}

/// Stage 7 bug fix: QS mutation endpoints must use shared QsMutationResponse schema,
/// not anonymous serde_json::Value.
#[test]
fn qs_mutation_responses_use_shared_schema() {
    let doc = openapi_json();

    let mutation_paths = [
        (
            "/paths/~11~1configs/post/responses/200/content/application~1json/schema/$ref",
            "create_config",
        ),
        (
            "/paths/~11~1configs~1{indexName}/put/responses/200/content/application~1json/schema/$ref",
            "update_config",
        ),
        (
            "/paths/~11~1configs~1{indexName}/delete/responses/200/content/application~1json/schema/$ref",
            "delete_config",
        ),
        (
            "/paths/~11~1configs~1{indexName}~1build/post/responses/200/content/application~1json/schema/$ref",
            "trigger_build",
        ),
    ];

    for (pointer, label) in &mutation_paths {
        assert_eq!(
            doc.pointer(pointer).and_then(|v| v.as_str()),
            Some("#/components/schemas/QsMutationResponse"),
            "{} 200 response must reference QsMutationResponse component schema",
            label
        );
    }
}

/// Stage 7 bug fix: Chat endpoint must document text/event-stream as an additional
/// 200-response content type alongside application/json.
#[test]
fn chat_endpoint_documents_sse_content_type() {
    let doc = openapi_json();

    let chat_200 = doc
        .pointer("/paths/~11~1indexes~1{indexName}~1chat/post/responses/200/content")
        .expect("chat 200 response should have content map");

    assert!(
        chat_200.get("application/json").is_some(),
        "chat 200 must include application/json content type"
    );
    assert!(
        chat_200.get("text/event-stream").is_some(),
        "chat 200 must include text/event-stream content type"
    );
}

#[test]
fn personalization_and_experiment_lifecycle_use_typed_schemas() {
    let doc = openapi_json();

    // Personalization
    assert_eq!(
        doc.pointer("/paths/~11~1strategies~1personalization/post/requestBody/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/PersonalizationStrategy")
    );
    assert_eq!(
        doc.pointer("/paths/~11~1strategies~1personalization/post/responses/200/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/SetStrategyResponse")
    );
    assert_eq!(
        doc.pointer("/paths/~11~1strategies~1personalization/get/responses/200/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/PersonalizationStrategy")
    );
    assert_eq!(
        doc.pointer("/paths/~11~1profiles~1personalization~1{userToken}/get/responses/200/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/PersonalizationProfile")
    );
    assert_eq!(
        doc.pointer("/paths/~11~1profiles~1{userToken}/delete/responses/200/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/DeleteProfileResponse")
    );

    // Experiment lifecycle + results
    assert_eq!(
        doc.pointer(
            "/paths/~12~1abtests~1{id}/put/requestBody/content/application~1json/schema/$ref"
        )
        .and_then(|v| v.as_str()),
        Some("#/components/schemas/CreateExperimentRequest")
    );
    assert_eq!(
        doc.pointer("/paths/~12~1abtests~1{id}~1conclude/post/requestBody/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/ConcludeExperimentRequest")
    );
    assert_eq!(
        doc.pointer("/paths/~12~1abtests~1{id}~1conclude/post/responses/200/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/ConcludedExperimentResponse")
    );
    assert_eq!(
        doc.pointer("/paths/~12~1abtests~1{id}~1results/get/responses/200/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/ResultsResponse")
    );
}

/// Stage 7 bug fix: search endpoint responses must use a shared SearchResponse schema
/// documenting response-side metadata fields (not just request-side vector/hybrid fields).
#[test]
fn search_response_schema_documents_metadata_fields() {
    let doc = openapi_json();

    assert_eq!(
        doc.pointer("/paths/~11~1indexes~1{indexName}~1query/post/responses/200/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/SearchResponse")
    );
    assert_eq!(
        doc.pointer("/paths/~11~1indexes~1{indexName}/get/responses/200/content/application~1json/schema/$ref")
            .and_then(|v| v.as_str()),
        Some("#/components/schemas/SearchResponse")
    );

    let props = doc
        .pointer("/components/schemas/SearchResponse/properties")
        .expect("SearchResponse must expose properties");

    for key in [
        "processingTimingsMS",
        "abTestID",
        "abTestVariantID",
        "interleavedTeams",
        "indexUsed",
        "appliedRelevancyStrictness",
    ] {
        assert!(
            props.get(key).is_some(),
            "SearchResponse missing metadata field '{}'",
            key
        );
    }
}

/// Stage 4: batch search must publish a typed federation contract for request + response.
#[test]
fn batch_search_endpoint_documents_federation_contract_shapes() {
    let doc = openapi_json();

    assert_eq!(
        doc.pointer(
            "/paths/~11~1indexes~1{indexName}~1queries/post/requestBody/content/application~1json/schema/$ref"
        )
        .and_then(|v| v.as_str()),
        Some("#/components/schemas/BatchSearchRequest")
    );

    assert_eq!(
        schema_ref(
            &doc,
            "/components/schemas/BatchSearchRequest/properties/federation"
        ),
        Some("#/components/schemas/FederationConfig")
    );

    assert_eq!(
        schema_ref(
            &doc,
            "/components/schemas/SearchRequest/properties/federationOptions"
        ),
        Some("#/components/schemas/FederationOptions")
    );

    let response_schema_pointer =
        "/paths/~11~1indexes~1{indexName}~1queries/post/responses/200/content/application~1json/schema";
    let response_schema_ref = schema_ref(&doc, response_schema_pointer);
    let variant_refs = if response_schema_ref == Some("#/components/schemas/BatchSearchResponse") {
        schema_composition_refs(&doc, "/components/schemas/BatchSearchResponse")
    } else {
        schema_composition_refs(&doc, response_schema_pointer)
    };
    assert!(
        !variant_refs.is_empty(),
        "batch_search 200 schema must declare response variants via oneOf/anyOf/allOf"
    );
    assert!(
        variant_refs.contains(&"#/components/schemas/BatchSearchLegacyResponse"),
        "batch_search oneOf must include legacy results[] response"
    );
    assert!(
        variant_refs.contains(&"#/components/schemas/FederatedResponse"),
        "batch_search oneOf must include federated hits response"
    );

    assert_eq!(
        doc.pointer("/components/schemas/BatchSearchLegacyResponse/properties/results/type")
            .and_then(|v| v.as_str()),
        Some("array")
    );
    assert_eq!(
        schema_ref(
            &doc,
            "/components/schemas/FederatedResponse/properties/hits/items"
        ),
        Some("#/components/schemas/FederatedHit")
    );
    assert_eq!(
        schema_ref(
            &doc,
            "/components/schemas/FederatedHit/properties/_federation"
        ),
        Some("#/components/schemas/FederationMeta")
    );

    for key in [
        "hits",
        "estimatedTotalHits",
        "offset",
        "limit",
        "processingTimeMS",
    ] {
        assert!(
            doc.pointer(&format!(
                "/components/schemas/FederatedResponse/properties/{key}"
            ))
            .is_some(),
            "FederatedResponse must include '{}' in schema",
            key
        );
    }

    let summary = doc
        .pointer("/paths/~11~1indexes~1{indexName}~1queries/post/summary")
        .and_then(|value| value.as_str())
        .expect("batch_search summary must exist");
    assert_ne!(summary, "TODO: Document batch_search.");

    let search_request_description = doc
        .pointer("/components/schemas/SearchRequest/description")
        .and_then(|value| value.as_str())
        .expect("SearchRequest description must exist");
    assert_ne!(search_request_description, "TODO: Document SearchRequest.");
}

/// Stage 7 bug fix: analytics/reporting endpoints must reference shared typed
/// response schemas instead of generic/implicit response bodies.
#[test]
fn analytics_endpoints_use_shared_response_schemas() {
    let doc = openapi_json();

    let checks = [
        (
            "/paths/~12~1searches/get/responses/200/content/application~1json/schema/$ref",
            "#/components/schemas/AnalyticsTopSearchesResponse",
        ),
        (
            "/paths/~12~1clicks~1clickThroughRate/get/responses/200/content/application~1json/schema/$ref",
            "#/components/schemas/AnalyticsRateWithDatesResponse",
        ),
        (
            "/paths/~12~1conversions~1revenue/get/responses/200/content/application~1json/schema/$ref",
            "#/components/schemas/AnalyticsRevenueResponse",
        ),
        (
            "/paths/~12~1countries/get/responses/200/content/application~1json/schema/$ref",
            "#/components/schemas/AnalyticsCountriesResponse",
        ),
        (
            "/paths/~12~1status/get/responses/200/content/application~1json/schema/$ref",
            "#/components/schemas/AnalyticsStatusResponse",
        ),
        (
            "/paths/~12~1analytics~1seed/post/responses/200/content/application~1json/schema/$ref",
            "#/components/schemas/AnalyticsSeedResponse",
        ),
        (
            "/paths/~12~1analytics~1cleanup/post/responses/200/content/application~1json/schema/$ref",
            "#/components/schemas/AnalyticsCleanupResponse",
        ),
    ];

    for (pointer, expected) in checks {
        assert_eq!(
            doc.pointer(pointer).and_then(|v| v.as_str()),
            Some(expected),
            "missing shared analytics response schema at {}",
            pointer
        );
    }
}

#[test]
fn health_endpoint_publishes_exact_response_schema() {
    let doc = openapi_json();

    assert_get_response_ref(&doc, "/health", "#/components/schemas/HealthResponse");
    assert_exact_property_set(
        &doc,
        "/components/schemas/HealthResponse",
        &[
            "status",
            "version",
            "build",
            "uptime_secs",
            "capabilities",
            "active_writers",
            "max_concurrent_writers",
            "facet_cache_entries",
            "facet_cache_cap",
            "heap_allocated_mb",
            "system_limit_mb",
            "pressure_level",
            "allocator",
            "tenants_loaded",
        ],
    );
    assert_eq!(
        schema_ref(&doc, "/components/schemas/HealthResponse/properties/build"),
        Some("#/components/schemas/PublicBuildInfo"),
        "/health build field must use the public-safe build schema"
    );
    assert_required_fields(
        &doc,
        "/components/schemas/PublicBuildInfo",
        &["schemaVersion", "version", "profile", "capabilities"],
    );
    assert_exact_property_set(
        &doc,
        "/components/schemas/PublicBuildInfo",
        &["schemaVersion", "version", "profile", "capabilities"],
    );
    for property in [
        "revision",
        "revisionKnown",
        "dirty",
        "dirtyKnown",
        "workspaceDigest",
        "target",
        "features",
    ] {
        assert!(
            doc.pointer(&format!(
                "/components/schemas/PublicBuildInfo/properties/{property}"
            ))
            .is_none(),
            "public /health build schema must not publish {property}"
        );
    }
    assert_no_operation_security(&doc, "/health", "get");
}

#[test]
fn replication_status_endpoint_publishes_exact_response_schema() {
    let doc = openapi_json();

    assert_get_response_ref(
        &doc,
        "/internal/status",
        "#/components/schemas/ReplicationStatusResponse",
    );
    assert_exact_property_set(
        &doc,
        "/components/schemas/ReplicationStatusResponse",
        &[
            "node_id",
            "replication_enabled",
            "peer_count",
            "ssl_renewal",
            "storage_total_bytes",
            "tenant_count",
            "vector_memory_bytes",
        ],
    );
    assert_required_fields(
        &doc,
        "/components/schemas/ReplicationStatusResponse",
        &[
            "node_id",
            "replication_enabled",
            "peer_count",
            "ssl_renewal",
            "storage_total_bytes",
            "tenant_count",
            "vector_memory_bytes",
        ],
    );
    assert_required_fields(
        &doc,
        "/components/schemas/SslRenewalStatus",
        &[
            "enabled",
            "status",
            "error",
            "cert_expires_in_days",
            "next_check",
        ],
    );
    for property in ["error", "cert_expires_in_days", "next_check"] {
        assert_nullable_schema(
            &doc,
            &format!("/components/schemas/SslRenewalStatus/properties/{property}"),
        );
    }
}

#[test]
fn snapshot_capability_endpoint_publishes_exact_response_schema() {
    let doc = openapi_json();

    assert_get_response_ref(
        &doc,
        "/internal/snapshots/capability",
        "#/components/schemas/SnapshotCapabilityResponse",
    );
    assert_exact_property_set(
        &doc,
        "/components/schemas/SnapshotCapabilityResponse",
        &["backend", "state", "bucket"],
    );
    assert_required_fields(
        &doc,
        "/components/schemas/SnapshotCapabilityResponse",
        &["backend", "state", "bucket"],
    );
    assert_nullable_schema(
        &doc,
        "/components/schemas/SnapshotCapabilityResponse/properties/bucket",
    );
}

#[test]
fn operations_internal_read_endpoints_require_api_key() {
    let doc = openapi_json();

    for path in [
        "/internal/status",
        "/internal/cluster/status",
        "/internal/snapshots/capability",
    ] {
        assert_get_operation_uses_api_key(&doc, path);
    }
}

#[test]
fn cluster_status_endpoint_publishes_boolean_discriminated_union() {
    let doc = openapi_json();

    assert_get_response_ref(
        &doc,
        "/internal/cluster/status",
        CLUSTER_STATUS_RESPONSE_SCHEMA,
    );
    let variants = schema_composition_refs(&doc, "/components/schemas/ClusterStatusResponse");
    assert_eq!(
        variants,
        vec![CLUSTER_STATUS_STANDALONE_SCHEMA, CLUSTER_STATUS_HA_SCHEMA],
        "cluster status must publish exactly standalone and HA branches"
    );

    assert_exact_property_set(
        &doc,
        "/components/schemas/ClusterStatusStandaloneResponse",
        &[
            "node_id",
            "replication_enabled",
            "peers",
            "autoheal_enabled",
            "autoheal_peers",
        ],
    );
    assert_required_fields(
        &doc,
        "/components/schemas/ClusterStatusStandaloneResponse",
        &["node_id", "replication_enabled", "peers"],
    );
    assert_fixed_bool_property(
        &doc,
        "/components/schemas/ClusterStatusStandaloneResponse",
        "replication_enabled",
        false,
    );
    assert_eq!(
        doc.pointer(
            "/components/schemas/ClusterStatusStandaloneResponse/properties/peers/maxItems"
        )
        .and_then(|value| value.as_u64()),
        Some(0),
        "standalone cluster branch must constrain peers to zero rows"
    );
    assert_eq!(
        schema_ref(
            &doc,
            "/components/schemas/ClusterStatusStandaloneResponse/properties/autoheal_peers/items"
        ),
        Some("#/components/schemas/AutohealPeerLifecycleResponse"),
        "standalone branch auto-heal rows must reuse AutohealPeerLifecycleResponse"
    );
    assert!(
        doc.pointer("/components/schemas/ClusterStatusStandaloneResponse/properties/peers_total")
            .is_none(),
        "standalone branch must not document peers_total"
    );
    assert!(
        doc.pointer("/components/schemas/ClusterStatusStandaloneResponse/properties/peers_healthy")
            .is_none(),
        "standalone branch must not document peers_healthy"
    );

    assert_exact_property_set(
        &doc,
        "/components/schemas/ClusterStatusHaResponse",
        &[
            "node_id",
            "replication_enabled",
            "peers_total",
            "peers_healthy",
            "peers",
            "autoheal_enabled",
            "autoheal_peers",
        ],
    );
    assert_required_fields(
        &doc,
        "/components/schemas/ClusterStatusHaResponse",
        &[
            "node_id",
            "replication_enabled",
            "peers_total",
            "peers_healthy",
            "peers",
        ],
    );
    assert_fixed_bool_property(
        &doc,
        "/components/schemas/ClusterStatusHaResponse",
        "replication_enabled",
        true,
    );
    assert_eq!(
        schema_ref(
            &doc,
            "/components/schemas/ClusterStatusHaResponse/properties/peers/items"
        ),
        Some("#/components/schemas/ClusterPeerStatus"),
        "HA branch peer rows must reuse ClusterPeerStatus"
    );
    assert_eq!(
        schema_ref(
            &doc,
            "/components/schemas/ClusterStatusHaResponse/properties/autoheal_peers/items"
        ),
        Some("#/components/schemas/AutohealPeerLifecycleResponse"),
        "HA branch auto-heal rows must reuse AutohealPeerLifecycleResponse"
    );
    assert_exact_property_set(
        &doc,
        "/components/schemas/ClusterPeerStatus",
        &["peer_id", "addr", "status", "last_success_secs_ago"],
    );
    assert_required_fields(
        &doc,
        "/components/schemas/ClusterPeerStatus",
        &["peer_id", "addr", "status", "last_success_secs_ago"],
    );
    assert_nullable_schema(
        &doc,
        "/components/schemas/ClusterPeerStatus/properties/last_success_secs_ago",
    );
}

fn assert_get_response_ref(doc: &serde_json::Value, path: &str, expected_schema: &str) {
    let escaped_path = path.replace('/', "~1");
    assert_eq!(
        doc.pointer(&format!(
            "/paths/{escaped_path}/get/responses/200/content/application~1json/schema/$ref"
        ))
        .and_then(|value| value.as_str()),
        Some(expected_schema),
        "GET {path} 200 response must reference {expected_schema}"
    );
}

fn assert_get_operation_uses_api_key(doc: &serde_json::Value, path: &str) {
    let escaped_path = path.replace('/', "~1");
    let security = doc
        .pointer(&format!("/paths/{escaped_path}/get/security"))
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("GET {path} should document operation security"));

    assert!(
        security.iter().any(|entry| entry.get("api_key").is_some()),
        "GET {path} should require api_key security in OpenAPI"
    );
}

fn assert_no_operation_security(doc: &serde_json::Value, path: &str, method: &str) {
    let escaped_path = path.replace('/', "~1");
    match doc.pointer(&format!("/paths/{escaped_path}/{method}/security")) {
        None => {}
        Some(value) => assert!(
            value.as_array().is_some_and(Vec::is_empty),
            "{method} {path} should not document security"
        ),
    }
}

fn assert_exact_property_set(doc: &serde_json::Value, schema_pointer: &str, expected: &[&str]) {
    let actual = doc
        .pointer(&format!("{schema_pointer}/properties"))
        .and_then(|value| value.as_object())
        .unwrap_or_else(|| panic!("{schema_pointer} should expose properties"))
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();

    assert_eq!(
        actual, expected,
        "{schema_pointer} should expose exactly the expected properties"
    );
}

fn assert_required_fields(doc: &serde_json::Value, schema_pointer: &str, expected: &[&str]) {
    let actual = doc
        .pointer(&format!("{schema_pointer}/required"))
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("{schema_pointer} should declare required fields"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("required field names must be strings")
        })
        .collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();

    assert_eq!(
        actual, expected,
        "{schema_pointer} should require exactly the expected fields"
    );
}

fn assert_fixed_bool_property(
    doc: &serde_json::Value,
    schema_pointer: &str,
    property: &str,
    expected: bool,
) {
    let property_schema_pointer = format!("{schema_pointer}/properties/{property}");
    let property_schema = doc
        .pointer(&property_schema_pointer)
        .unwrap_or_else(|| panic!("{property_schema_pointer} should exist"));
    let enum_values = property_schema
        .get("enum")
        .and_then(|value| value.as_array())
        .map(Vec::as_slice)
        .or_else(|| property_schema.get("const").map(std::slice::from_ref));

    assert!(
        enum_values.is_some_and(|values| values == [serde_json::Value::Bool(expected)]),
        "{property_schema_pointer} must be fixed to {expected}; got {property_schema:?}"
    );
}

fn assert_nullable_schema(doc: &serde_json::Value, schema_pointer: &str) {
    let schema = doc
        .pointer(schema_pointer)
        .unwrap_or_else(|| panic!("{schema_pointer} should exist"));
    let nullable = schema
        .get("nullable")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
        || schema
            .get("type")
            .and_then(|value| value.as_array())
            .is_some_and(|types| types.iter().any(|value| value.as_str() == Some("null")))
        || schema
            .get("anyOf")
            .and_then(|value| value.as_array())
            .is_some_and(|variants| variants.iter().any(serde_json::Value::is_null));

    assert!(nullable, "{schema_pointer} should allow null");
}

// --- Stage 7 Security Review: regression guards for auth-correct OpenAPI exposure ---

#[test]
fn health_endpoint_has_no_security_requirement() {
    let doc = openapi_json();
    let health_get = doc
        .pointer("/paths/~1health/get")
        .expect("/health GET should exist");

    // No "security" key, or empty array, means public
    match health_get.get("security") {
        None => {}
        Some(arr) => {
            assert!(
                arr.as_array().is_none_or(|a| a.is_empty()),
                "/health must not require auth, found: {:?}",
                arr
            );
        }
    }
}

/// Security review: only explicitly supported internal contracts may appear under `/internal/*`.
#[test]
fn openapi_documents_only_canonical_internal_contracts() {
    let doc = openapi_json();
    let paths = doc
        .get("paths")
        .and_then(|p| p.as_object())
        .expect("spec must have paths");

    let unexpected_internal_paths: Vec<&String> = paths
        .keys()
        .filter(|path| {
            path.starts_with("/internal/")
                && !crate::openapi::DOCUMENTED_INTERNAL_PATHS.contains(&path.as_str())
        })
        .collect();

    assert!(
        unexpected_internal_paths.is_empty(),
        "only canonical internal paths may appear in the public spec, found: {:?}",
        unexpected_internal_paths
    );
}

/// Security review: all documented customer-facing endpoints (everything except `/health`)
/// must declare a security requirement referencing the `api_key` scheme.
#[test]
fn all_non_health_endpoints_require_api_key_security() {
    let doc = openapi_json();
    let paths = doc
        .get("paths")
        .and_then(|p| p.as_object())
        .expect("spec must have paths");

    let mut missing_security = Vec::new();

    for (path_key, path_item) in paths {
        if path_key == "/health" {
            continue;
        }
        let methods = path_item.as_object().unwrap();
        for (method, operation) in methods {
            if !["get", "post", "put", "delete", "patch", "head", "options"]
                .contains(&method.as_str())
            {
                continue;
            }
            let has_api_key = operation
                .get("security")
                .and_then(|s| s.as_array())
                .map(|arr| {
                    arr.iter().any(|item| {
                        item.as_object()
                            .map(|obj| obj.contains_key("api_key"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);

            if !has_api_key {
                missing_security.push(format!("{} {}", method.to_uppercase(), path_key));
            }
        }
    }

    assert!(
        missing_security.is_empty(),
        "endpoints missing api_key security requirement: {:?}",
        missing_security
    );
}

/// Security review: the `api_key` security scheme must be defined in the spec
/// as an API key in the `x-algolia-api-key` header.
#[test]
fn api_key_security_scheme_is_defined() {
    let doc = openapi_json();

    let scheme_type = doc
        .pointer("/components/securitySchemes/api_key/type")
        .and_then(|v| v.as_str());
    assert_eq!(
        scheme_type,
        Some("apiKey"),
        "api_key scheme must be type apiKey"
    );

    let scheme_name = doc
        .pointer("/components/securitySchemes/api_key/name")
        .and_then(|v| v.as_str());
    assert_eq!(
        scheme_name,
        Some("x-algolia-api-key"),
        "api_key scheme must use x-algolia-api-key header"
    );

    let scheme_in = doc
        .pointer("/components/securitySchemes/api_key/in")
        .and_then(|v| v.as_str());
    assert_eq!(
        scheme_in,
        Some("header"),
        "api_key scheme must be in header"
    );
}

/// Verify that security-sources CRUD endpoints appear in the generated OpenAPI spec with correct request/response schemas.
#[test]
fn security_sources_endpoints_are_documented() {
    let doc = openapi_json();

    assert_eq!(
        doc.pointer(
            "/paths/~11~1security~1sources/get/responses/200/content/application~1json/schema/type"
        )
        .and_then(|v| v.as_str()),
        Some("array")
    );
    assert_eq!(
        doc.pointer(
            "/paths/~11~1security~1sources/put/requestBody/content/application~1json/schema/type"
        )
        .and_then(|v| v.as_str()),
        Some("array")
    );
    assert_eq!(
        doc.pointer(
            "/paths/~11~1security~1sources~1append/post/responses/200/content/application~1json/schema/$ref"
        )
        .and_then(|v| v.as_str()),
        Some("#/components/schemas/SourceMutationTimestampResponse")
    );
    assert_eq!(
        doc.pointer(
            "/paths/~11~1security~1sources~1{source}/delete/responses/200/content/application~1json/schema/$ref"
        )
        .and_then(|v| v.as_str()),
        Some("#/components/schemas/SourceMutationTimestampResponse")
    );
}
