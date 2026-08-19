//! Stub summary for mutation_parity.rs.
use serde::Serialize;

/// Deterministic inventory of the highest-risk mutation endpoints whose
/// contract drift is costly to catch late.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationParityKind {
    AlgoliaParity,
    FlapjackExtension,
}

impl MutationParityKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlgoliaParity => "Algolia parity",
            Self::FlapjackExtension => "Flapjack extension",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MutationParityCase {
    pub id: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub expected_status: u16,
    pub required_fields: &'static [&'static str],
    pub exact_fields: Option<&'static [&'static str]>,
    pub parity_kind: MutationParityKind,
    pub primary_source: &'static str,
    pub runtime_handler: &'static str,
    pub behavior_tests: &'static [&'static str],
    pub openapi_response_pointer: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MutationParityExportRow {
    pub id: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub parity_kind: &'static str,
}

pub fn exported_algolia_parity_cases() -> Vec<MutationParityExportRow> {
    HIGH_RISK_MUTATION_PARITY_CASES
        .iter()
        .filter(|case| case.parity_kind == MutationParityKind::AlgoliaParity)
        .map(|case| MutationParityExportRow {
            id: case.id,
            method: case.method,
            path: case.path,
            parity_kind: case.parity_kind.as_str(),
        })
        .collect()
}

pub const HIGH_RISK_MUTATION_PARITY_CASES: &[MutationParityCase] = &[
    MutationParityCase {
        id: "keys.create",
        method: "POST",
        path: "/1/keys",
        expected_status: 200,
        required_fields: &["key", "createdAt"],
        exact_fields: Some(&["createdAt", "key"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source: "engine/docs2/2_REFERENCE/algolia/openapi/search/paths/keys/keys.yml::post",
        runtime_handler: "engine/flapjack-http/src/handlers/keys.rs::create_key",
        behavior_tests: &[
            "engine/tests/test_sdk_contract_keys.rs::create_key_returns_camelcase_envelope",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~11~1keys/post/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "keys.update",
        method: "PUT",
        path: "/1/keys/{key}",
        expected_status: 200,
        required_fields: &["key", "updatedAt"],
        exact_fields: Some(&["key", "updatedAt"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source: "engine/docs2/2_REFERENCE/algolia/openapi/search/paths/keys/key.yml::put",
        runtime_handler: "engine/flapjack-http/src/handlers/keys.rs::update_key",
        behavior_tests: &[
            "engine/tests/test_sdk_contract_keys.rs::update_key_returns_camelcase_envelope",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~11~1keys~1{key}/put/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "keys.delete",
        method: "DELETE",
        path: "/1/keys/{key}",
        expected_status: 200,
        required_fields: &["deletedAt"],
        exact_fields: Some(&["deletedAt"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source: "engine/docs2/2_REFERENCE/algolia/openapi/search/paths/keys/key.yml::delete",
        runtime_handler: "engine/flapjack-http/src/handlers/keys.rs::delete_key",
        behavior_tests: &[
            "engine/tests/test_sdk_contract_keys.rs::delete_key_returns_camelcase_envelope",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~11~1keys~1{key}/delete/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "keys.restore",
        method: "POST",
        path: "/1/keys/{key}/restore",
        expected_status: 200,
        required_fields: &["key", "createdAt"],
        exact_fields: Some(&["createdAt", "key"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source:
            "engine/docs2/2_REFERENCE/algolia/openapi/search/paths/keys/restoreApiKey.yml::post",
        runtime_handler: "engine/flapjack-http/src/handlers/keys.rs::restore_key",
        behavior_tests: &[
            "engine/tests/test_sdk_contract_keys.rs::restore_key_returns_camelcase_envelope",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~11~1keys~1{key}~1restore/post/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "abtests.create",
        method: "POST",
        path: "/2/abtests",
        expected_status: 200,
        required_fields: &["abTestID", "index", "taskID"],
        exact_fields: Some(&["abTestID", "index", "taskID"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source:
            "engine/docs2/2_REFERENCE/algolia/openapi/abtesting/paths/abtests.yml::post",
        runtime_handler: "engine/flapjack-http/src/handlers/experiments/mod.rs::create_experiment",
        behavior_tests: &[
            "engine/flapjack-http/src/handlers/experiments_tests.rs::create_experiment_returns_200",
            "engine/tests/test_stage4_sdk_smoke.rs::ab_lifecycle_smoke_populates_variant_metrics_and_honors_stop_side_effects",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~12~1abtests/post/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "abtests.update",
        method: "PUT",
        path: "/2/abtests/{id}",
        expected_status: 200,
        required_fields: &["abTestID", "createdAt", "name", "status", "updatedAt", "variants"],
        exact_fields: None,
        parity_kind: MutationParityKind::FlapjackExtension,
        primary_source: "local extension: no official bundled Algolia PUT /2/abtests/{id} contract is present in engine/docs2/2_REFERENCE/algolia/openapi/abtesting/",
        runtime_handler: "engine/flapjack-http/src/handlers/experiments/mod.rs::update_experiment",
        behavior_tests: &[
            "engine/flapjack-http/src/handlers/experiments_tests.rs::update_draft_experiment_returns_200",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~12~1abtests~1{id}/put/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "abtests.delete",
        method: "DELETE",
        path: "/2/abtests/{id}",
        expected_status: 200,
        required_fields: &["abTestID", "index", "taskID"],
        exact_fields: Some(&["abTestID", "index", "taskID"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source:
            "engine/docs2/2_REFERENCE/algolia/openapi/abtesting/paths/abtest.yml::delete",
        runtime_handler: "engine/flapjack-http/src/handlers/experiments/mod.rs::delete_experiment",
        behavior_tests: &[
            "engine/flapjack-http/src/handlers/experiments_tests.rs::delete_draft_experiment_returns_200_action_shape",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~12~1abtests~1{id}/delete/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "abtests.start",
        method: "POST",
        path: "/2/abtests/{id}/start",
        expected_status: 200,
        required_fields: &["abTestID", "index", "taskID"],
        exact_fields: Some(&["abTestID", "index", "taskID"]),
        parity_kind: MutationParityKind::FlapjackExtension,
        primary_source: "local extension: no official bundled Algolia start endpoint is present in engine/docs2/2_REFERENCE/algolia/openapi/abtesting/",
        runtime_handler: "engine/flapjack-http/src/handlers/experiments/mod.rs::start_experiment",
        behavior_tests: &[
            "engine/flapjack-http/src/handlers/experiments_tests.rs::start_experiment_returns_200",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~12~1abtests~1{id}~1start/post/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "abtests.stop",
        method: "POST",
        path: "/2/abtests/{id}/stop",
        expected_status: 200,
        required_fields: &["abTestID", "index", "taskID"],
        exact_fields: Some(&["abTestID", "index", "taskID"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source:
            "engine/docs2/2_REFERENCE/algolia/openapi/abtesting/paths/stopABTest.yml::post",
        runtime_handler: "engine/flapjack-http/src/handlers/experiments/mod.rs::stop_experiment",
        behavior_tests: &[
            "engine/tests/test_experiments.rs::test_start_stop_lifecycle",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~12~1abtests~1{id}~1stop/post/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "abtests.conclude",
        method: "POST",
        path: "/2/abtests/{id}/conclude",
        expected_status: 200,
        required_fields: &["conclusion", "id", "status"],
        exact_fields: None,
        parity_kind: MutationParityKind::FlapjackExtension,
        primary_source: "local extension: no official bundled Algolia conclude endpoint is present in engine/docs2/2_REFERENCE/algolia/openapi/abtesting/",
        runtime_handler: "engine/flapjack-http/src/handlers/experiments/mod.rs::conclude_experiment",
        behavior_tests: &[
            "engine/flapjack-http/src/handlers/experiments_tests.rs::conclude_experiment_returns_200_and_sets_conclusion",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~12~1abtests~1{id}~1conclude/post/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "indexes.create",
        method: "POST",
        path: "/1/indexes",
        expected_status: 200,
        required_fields: &["createdAt", "uid"],
        exact_fields: Some(&["createdAt", "uid"]),
        parity_kind: MutationParityKind::FlapjackExtension,
        primary_source: "local extension: engine/flapjack-http/src/handlers/indices.rs::create_index",
        runtime_handler: "engine/flapjack-http/src/handlers/indices.rs::create_index",
        behavior_tests: &[
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~11~1indexes/post/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "indexes.delete",
        method: "DELETE",
        path: "/1/indexes/{indexName}",
        expected_status: 200,
        required_fields: &["deletedAt", "taskID"],
        exact_fields: Some(&["deletedAt", "taskID"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source:
            "engine/docs2/2_REFERENCE/algolia/openapi/search/paths/indices/index.yml::delete",
        runtime_handler: "engine/flapjack-http/src/handlers/indices.rs::delete_index",
        behavior_tests: &[
            "engine/tests/test_sdk_contract_crud.rs::delete_index_returns_task_id_and_deleted_at",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~11~1indexes~1{indexName}/delete/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "objects.save_auto_id",
        method: "POST",
        path: "/1/indexes/{indexName}",
        expected_status: 201,
        required_fields: &["createdAt", "objectID", "taskID"],
        exact_fields: Some(&["createdAt", "objectID", "taskID"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source:
            "engine/docs2/2_REFERENCE/algolia/openapi/search/paths/objects/objects.yml::post",
        runtime_handler: "engine/flapjack-http/src/handlers/objects/mod.rs::add_record_auto_id",
        behavior_tests: &[
            "engine/tests/test_sdk_contract_crud.rs::create_via_add_object_returns_algolia_envelope",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~11~1indexes~1{indexName}/post/responses/201/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "objects.batch",
        method: "POST",
        path: "/1/indexes/{indexName}/batch",
        expected_status: 200,
        required_fields: &["objectIDs", "taskID"],
        exact_fields: Some(&["objectIDs", "taskID"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source:
            "engine/docs2/2_REFERENCE/algolia/openapi/search/paths/objects/batch.yml::post",
        runtime_handler: "engine/flapjack-http/src/handlers/objects/mod.rs::add_documents",
        behavior_tests: &[
            "engine/tests/test_sdk_contract_crud.rs::batch_mixed_actions_return_task_id_and_object_ids",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~11~1indexes~1{indexName}~1batch/post/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "objects.delete",
        method: "DELETE",
        path: "/1/indexes/{indexName}/{objectID}",
        expected_status: 200,
        required_fields: &["deletedAt", "taskID"],
        exact_fields: Some(&["deletedAt", "taskID"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source:
            "engine/docs2/2_REFERENCE/algolia/openapi/search/paths/objects/object.yml::delete",
        runtime_handler: "engine/flapjack-http/src/handlers/objects/mod.rs::delete_object",
        behavior_tests: &[
            "engine/tests/test_sdk_contract_crud.rs::delete_object_returns_task_id_and_deleted_at",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~11~1indexes~1{indexName}~1{objectID}/delete/responses/200/content/application~1json/schema/$ref",
    },
    MutationParityCase {
        id: "objects.partial",
        method: "POST",
        path: "/1/indexes/{indexName}/{objectID}/partial",
        expected_status: 200,
        required_fields: &["objectID", "taskID", "updatedAt"],
        exact_fields: Some(&["objectID", "taskID", "updatedAt"]),
        parity_kind: MutationParityKind::AlgoliaParity,
        primary_source:
            "engine/docs2/2_REFERENCE/algolia/openapi/search/paths/objects/partialUpdate.yml::post",
        runtime_handler: "engine/flapjack-http/src/handlers/objects/mod.rs::partial_update_object",
        behavior_tests: &[
            "engine/tests/test_sdk_contract_crud.rs::partial_update_returns_object_id_task_id_updated_at",
            "engine/tests/test_mutation_parity.rs::high_risk_mutation_endpoints_match_expected_status_and_envelopes",
        ],
        openapi_response_pointer:
            "/paths/~11~1indexes~1{indexName}~1{objectID}~1partial/post/responses/200/content/application~1json/schema/$ref",
    },
];

#[cfg(test)]
mod tests {
    use super::{
        exported_algolia_parity_cases, MutationParityKind, HIGH_RISK_MUTATION_PARITY_CASES,
    };

    /// TODO: Document exported_algolia_parity_cases_include_only_algolia_rows.
    #[test]
    fn exported_algolia_parity_cases_include_only_algolia_rows() {
        let exported_rows = exported_algolia_parity_cases();
        let expected_algolia_count = HIGH_RISK_MUTATION_PARITY_CASES
            .iter()
            .filter(|case| case.parity_kind == MutationParityKind::AlgoliaParity)
            .count();
        let extension_count = HIGH_RISK_MUTATION_PARITY_CASES
            .iter()
            .filter(|case| case.parity_kind == MutationParityKind::FlapjackExtension)
            .count();

        assert!(
            extension_count > 0,
            "fixture sanity check: test requires at least one FlapjackExtension row"
        );
        assert_eq!(
            exported_rows.len(),
            expected_algolia_count,
            "export helper should include every Algolia parity row and no extras"
        );
        assert!(
            exported_rows
                .iter()
                .all(|row| row.parity_kind == MutationParityKind::AlgoliaParity.as_str()),
            "export helper should exclude FlapjackExtension rows"
        );
    }
}
