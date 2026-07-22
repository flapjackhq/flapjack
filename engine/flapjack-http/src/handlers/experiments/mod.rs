#[cfg(test)]
use axum::http::StatusCode;
use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    response::Response,
    Json,
};
pub(super) use flapjack::experiments::config::QueryOverrides;
use flapjack::experiments::{
    config::{
        Experiment, ExperimentArm, ExperimentConclusion, ExperimentError, ExperimentStatus,
        PrimaryMetric,
    },
    metrics,
    store::ExperimentStore,
};
use std::sync::Arc;

use super::dto_algolia::{
    self, AlgoliaAbTestActionResponse, AlgoliaCreateAbTestRequest, AlgoliaCreateAbTestResponse,
    AlgoliaEstimateRequest, AlgoliaEstimateResponse, AlgoliaListAbTestsQuery,
    AlgoliaListAbTestsResponse,
};
use super::AppState;

mod promote;
mod resolve;
mod response_helpers;
mod schemas;
use promote::promote_variant_settings;
#[cfg(test)]
use resolve::require_numeric_id_mapping;
use resolve::{
    numeric_id_for_experiment, require_experiment_store, resolve_store_and_experiment_id,
    resolve_store_and_experiment_uuid,
};
pub use response_helpers::ConcludedExperimentResponse;
use response_helpers::{
    algolia_action_response, apply_metrics_to_algolia_response, attach_experiment_warning_header,
    concluded_experiment_response, experiment_error_to_response, lifecycle_action_response,
};
pub use schemas::{
    ArmResponse, BayesianResponse, ConcludeExperimentRequest, CreateExperimentRequest,
    GateResponse, GuardRailAlertResponse, InterleavingResponse, ListExperimentsQuery,
    ListExperimentsResponse, ResultsResponse, SignificanceResponse,
};

const DEFAULT_LIST_LIMIT: usize = 10;
const DEFAULT_LIST_OFFSET: usize = 0;
pub(super) const DEFAULT_ESTIMATE_DURATION_DAYS: i64 = 21;
pub(super) const ESTIMATE_TRAFFIC_LOOKBACK_DAYS: i64 = 30;
const EXPERIMENT_WARNING_HEADER_NAME: &str = "x-flapjack-warning";
const VARIANT_PROMOTION_FAILED_WARNING: &str = "variant-promotion-failed";
#[cfg(test)]
const DEFAULT_MINIMUM_DAYS: u32 = dto_algolia::DEFAULT_MINIMUM_DAYS;
fn validate_conclusion_winner(winner: Option<String>) -> Result<Option<String>, ExperimentError> {
    match winner {
        Some(w) if w == "control" || w == "variant" => Ok(Some(w)),
        Some(w) => Err(ExperimentError::InvalidConfig(format!(
            "winner must be 'control' or 'variant', got '{w}'"
        ))),
        None => Ok(None),
    }
}

fn should_promote_variant_settings(experiment: &Experiment) -> bool {
    experiment.conclusion.as_ref().is_some_and(|conclusion| {
        conclusion.promoted && conclusion.winner.as_deref() == Some("variant")
    })
}

/// Attempts variant promotion after a successful conclusion and returns the
/// warning header value when promotion fails but the conclusion itself succeeds.
fn promotion_warning_if_variant_promotion_fails(
    state: &AppState,
    experiment: &Experiment,
) -> Option<&'static str> {
    if !should_promote_variant_settings(experiment) {
        return None;
    }
    if let Err(error) = promote_variant_settings(state, experiment) {
        tracing::error!(
            experiment_id = %experiment.id,
            error = %error,
            "failed to promote variant settings"
        );
        // Conclude succeeded, promotion failed — return the experiment
        // with a warning header so the caller knows promotion was partial.
        return Some(VARIANT_PROMOTION_FAILED_WARNING);
    }
    None
}

/// Creates a new experiment and returns the Algolia-compatible action payload
/// using the stable numeric experiment identifier.
#[utoipa::path(
    post,
    path = "/2/abtests",
    tag = "experiments",
    request_body(content = AlgoliaCreateAbTestRequest, description = "A/B test creation payload"),
    responses(
        (status = 200, description = "Experiment created", body = AlgoliaCreateAbTestResponse),
        (status = 400, description = "Invalid experiment configuration"),
        (status = 500, description = "Experiment missing numeric ID mapping"),
        (status = 409, description = "Experiment already exists"),
        (status = 503, description = "Experiment store unavailable")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn create_experiment(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AlgoliaCreateAbTestRequest>,
) -> Response {
    let store = match require_experiment_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };

    let experiment = match dto_algolia::algolia_create_to_experiment(&body) {
        Ok(exp) => exp,
        Err(msg) => {
            return experiment_error_to_response(ExperimentError::InvalidConfig(msg));
        }
    };

    let index_name = experiment.index_name.clone();
    let created = match store.create(experiment) {
        Ok(created) => created,
        Err(err) => return experiment_error_to_response(err),
    };
    let numeric_id = match numeric_id_for_experiment(store, &created.id) {
        Ok(numeric_id) => numeric_id,
        Err(response) => return response,
    };

    algolia_action_response(numeric_id, index_name)
}

/// Lists experiments with Algolia-compatible pagination and optional exact, prefix, or suffix index-name filtering.
#[utoipa::path(
    get,
    path = "/2/abtests",
    tag = "experiments",
    params(
        ("offset" = Option<usize>, Query, description = "Pagination offset"),
        ("limit" = Option<usize>, Query, description = "Maximum number of experiments to return"),
        ("indexName" = Option<String>, Query, description = "Filter experiments by exact index name"),
        ("indexPrefix" = Option<String>, Query, description = "Filter experiments by index name prefix"),
        ("indexSuffix" = Option<String>, Query, description = "Filter experiments by index name suffix")
    ),
    responses(
        (status = 200, description = "Experiments list", body = AlgoliaListAbTestsResponse),
        (status = 500, description = "Experiment missing numeric ID mapping"),
        (status = 503, description = "Experiment store unavailable")
    ),
    security(
        ("api_key" = [])
    )
)]
#[allow(clippy::result_large_err)]
pub async fn list_experiments(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AlgoliaListAbTestsQuery>,
) -> Response {
    let store = match require_experiment_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };

    let mut experiments = store.list(None);

    // Apply Algolia-style index filters. Exact, prefix, and suffix filters compose
    // by intersection here, before numeric-ID pairing, so that `count`, `total`, and
    // the missing-match `abtests: null` semantics are all computed from the filtered set.
    if let Some(ref name) = params.index_name {
        experiments.retain(|e| e.index_name == *name);
    }
    if let Some(ref prefix) = params.index_prefix {
        experiments.retain(|e| e.index_name.starts_with(prefix.as_str()));
    }
    if let Some(ref suffix) = params.index_suffix {
        experiments.retain(|e| e.index_name.ends_with(suffix.as_str()));
    }

    // Pair each experiment with its numeric ID for deterministic sort and DTO mapping.
    let mut exp_with_ids: Vec<(Experiment, i64)> = match experiments
        .into_iter()
        .map(|e| {
            let numeric_id = numeric_id_for_experiment(store, &e.id)?;
            Ok((e, numeric_id))
        })
        .collect::<Result<Vec<_>, Response>>()
    {
        Ok(exp_with_ids) => exp_with_ids,
        Err(response) => return response,
    };
    exp_with_ids.sort_by(|(a, a_id), (b, b_id)| {
        a.created_at.cmp(&b.created_at).then_with(|| a_id.cmp(b_id))
    });

    let total = exp_with_ids.len();
    let offset = params.offset.unwrap_or(DEFAULT_LIST_OFFSET);
    let limit = params.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    let page: Vec<(Experiment, i64)> = exp_with_ids.into_iter().skip(offset).take(limit).collect();
    let count = page.len();

    let abtests: Vec<_> = page
        .iter()
        .map(|(exp, numeric_id)| {
            dto_algolia::experiment_to_algolia_with_updated_at(
                exp,
                *numeric_id,
                store.get_last_updated_ms(&exp.id),
            )
        })
        .collect();

    let abtests = if total == 0 { None } else { Some(abtests) };

    Json(AlgoliaListAbTestsResponse {
        abtests,
        count,
        total,
    })
    .into_response()
}

/// Fetches a single experiment by numeric ID or UUID and enriches the payload
/// with analytics-backed results metadata when available.
#[utoipa::path(
    get,
    path = "/2/abtests/{id}",
    tag = "experiments",
    params(
        ("id" = String, Path, description = "Experiment identifier (numeric ID or UUID)")
    ),
    responses(
        (status = 200, description = "Experiment details", body = dto_algolia::AlgoliaAbTest),
        (status = 500, description = "Experiment missing numeric ID mapping"),
        (status = 404, description = "Experiment not found"),
        (status = 503, description = "Experiment store unavailable")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn get_experiment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let (store, uuid, numeric_id) = match resolve_store_and_experiment_id(&state, &id) {
        Ok(values) => values,
        Err(response) => return response,
    };

    match store.get(&uuid) {
        Ok(experiment) => {
            let mut payload = dto_algolia::experiment_to_algolia_with_updated_at(
                &experiment,
                numeric_id,
                store.get_last_updated_ms(&experiment.id),
            );

            if let Some(engine) = state.analytics_engine.as_ref() {
                let index_names = results::resolve_experiment_index_names(&experiment);
                let index_name_refs = results::index_name_refs(&index_names);

                match metrics::get_experiment_metrics(
                    &experiment.id,
                    &index_name_refs,
                    &engine.config().data_dir,
                    experiment.winsorization_cap,
                )
                .await
                {
                    Ok(aggregates) => apply_metrics_to_algolia_response(&mut payload, &aggregates),
                    Err(err) => {
                        tracing::warn!(
                            "failed to hydrate abtest metrics for {}: {}",
                            experiment.id,
                            err
                        );
                    }
                }
            }

            Json(payload).into_response()
        }
        Err(err) => experiment_error_to_response(err),
    }
}

/// Replace an experiment's mutable configuration fields while preserving its
/// identity and lifecycle state.
///
/// Merges the request body with the existing experiment, keeping immutable
/// identity and lifecycle fields while falling back to existing values for
/// optional settings omitted from the request.
#[utoipa::path(
    put,
    path = "/2/abtests/{id}",
    tag = "experiments",
    params(
        ("id" = String, Path, description = "Experiment identifier (numeric ID or UUID)")
    ),
    request_body(content = CreateExperimentRequest, description = "Updated experiment configuration"),
    responses(
        (status = 200, description = "Experiment updated", body = dto_algolia::AlgoliaAbTest),
        (status = 400, description = "Invalid experiment configuration"),
        (status = 500, description = "Experiment missing numeric ID mapping"),
        (status = 404, description = "Experiment not found"),
        (status = 409, description = "Experiment is in an invalid status for update"),
        (status = 503, description = "Experiment store unavailable")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn update_experiment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<CreateExperimentRequest>,
) -> Response {
    let (store, uuid, numeric_id) = match resolve_store_and_experiment_id(&state, &id) {
        Ok(values) => values,
        Err(response) => return response,
    };

    let existing = match store.get(&uuid) {
        Ok(experiment) => experiment,
        Err(err) => return experiment_error_to_response(err),
    };

    let updated = Experiment {
        id: existing.id,
        name: body.name,
        index_name: body.index_name,
        status: existing.status,
        traffic_split: body.traffic_split,
        control: body.control,
        variant: body.variant,
        primary_metric: body.primary_metric,
        created_at: existing.created_at,
        started_at: existing.started_at,
        ended_at: existing.ended_at,
        stopped_at: existing.stopped_at,
        minimum_days: body.minimum_days.unwrap_or(existing.minimum_days),
        winsorization_cap: body.winsorization_cap.or(existing.winsorization_cap),
        conclusion: existing.conclusion,
        interleaving: body.interleaving.or(existing.interleaving),
    };

    match store.update(updated) {
        Ok(experiment) => Json(dto_algolia::experiment_to_algolia_with_updated_at(
            &experiment,
            numeric_id,
            store.get_last_updated_ms(&experiment.id),
        ))
        .into_response(),
        Err(err) => experiment_error_to_response(err),
    }
}

/// Deletes an experiment by numeric ID or UUID and returns the Algolia-style
/// action envelope for the affected experiment.
#[utoipa::path(
    delete,
    path = "/2/abtests/{id}",
    tag = "experiments",
    params(
        ("id" = String, Path, description = "Experiment identifier (numeric ID or UUID)")
    ),
    responses(
        (status = 200, description = "Experiment deleted", body = AlgoliaAbTestActionResponse),
        (status = 500, description = "Experiment missing numeric ID mapping"),
        (status = 404, description = "Experiment not found"),
        (status = 409, description = "Cannot delete a running experiment"),
        (status = 503, description = "Experiment store unavailable")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn delete_experiment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let (store, uuid, numeric_id) = match resolve_store_and_experiment_id(&state, &id) {
        Ok(values) => values,
        Err(response) => return response,
    };

    let experiment = match store.get(&uuid) {
        Ok(exp) => exp,
        Err(err) => return experiment_error_to_response(err),
    };

    let index_name = experiment.index_name.clone();
    match store.delete(&uuid) {
        Ok(()) => algolia_action_response(numeric_id, index_name),
        Err(err) => experiment_error_to_response(err),
    }
}

/// Start a previously created experiment, beginning traffic assignment.
///
/// Resolves the experiment by numeric or UUID path parameter, then transitions
/// its status to running via the store.
///
/// # Returns
///
/// JSON action response with the experiment's numeric ID and index name,
/// or an error if the experiment is not found or not in a startable state.
#[utoipa::path(
    post,
    path = "/2/abtests/{id}/start",
    tag = "experiments",
    params(
        ("id" = String, Path, description = "Experiment identifier (numeric ID or UUID)")
    ),
    responses(
        (status = 200, description = "Experiment started", body = AlgoliaAbTestActionResponse),
        (status = 500, description = "Experiment missing numeric ID mapping"),
        (status = 404, description = "Experiment not found"),
        (status = 409, description = "Experiment is in an invalid status for start"),
        (status = 503, description = "Experiment store unavailable")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn start_experiment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    lifecycle_action_response(&state, &id, ExperimentStore::start)
}

/// Stop a running experiment, freezing its traffic assignment.
///
/// Resolves the experiment by numeric or UUID path parameter, then transitions
/// its status to stopped via the store.
///
/// # Returns
///
/// JSON action response with the experiment's numeric ID and index name,
/// or an error if the experiment is not found or not in a stoppable state.
#[utoipa::path(
    post,
    path = "/2/abtests/{id}/stop",
    tag = "experiments",
    params(
        ("id" = String, Path, description = "Experiment identifier (numeric ID or UUID)")
    ),
    responses(
        (status = 200, description = "Experiment stopped", body = AlgoliaAbTestActionResponse),
        (status = 500, description = "Experiment missing numeric ID mapping"),
        (status = 404, description = "Experiment not found"),
        (status = 409, description = "Experiment is in an invalid status for stop"),
        (status = 503, description = "Experiment store unavailable")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn stop_experiment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    lifecycle_action_response(&state, &id, ExperimentStore::stop)
}

/// Concludes an experiment, optionally promotes the winning variant settings,
/// and surfaces partial-promotion failures via a warning header.
#[utoipa::path(
    post,
    path = "/2/abtests/{id}/conclude",
    tag = "experiments",
    params(
        ("id" = String, Path, description = "Experiment identifier (numeric ID or UUID)")
    ),
    request_body(content = ConcludeExperimentRequest, description = "Conclusion summary and winner declaration"),
    responses(
        (status = 200, description = "Experiment concluded", body = ConcludedExperimentResponse),
        (status = 400, description = "Invalid conclusion payload"),
        (status = 500, description = "Experiment missing numeric ID mapping"),
        (status = 404, description = "Experiment not found"),
        (status = 409, description = "Experiment is in an invalid status for conclude"),
        (status = 503, description = "Experiment store unavailable")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn conclude_experiment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<ConcludeExperimentRequest>,
) -> Response {
    let (store, uuid) = match resolve_store_and_experiment_uuid(&state, &id) {
        Ok(values) => values,
        Err(response) => return response,
    };

    let winner = match validate_conclusion_winner(body.winner) {
        Ok(winner) => winner,
        Err(err) => return experiment_error_to_response(err),
    };

    let conclusion = ExperimentConclusion {
        winner,
        reason: body.reason,
        control_metric: body.control_metric,
        variant_metric: body.variant_metric,
        confidence: body.confidence,
        significant: body.significant,
        promoted: body.promoted,
    };

    match store.conclude(&uuid, conclusion) {
        Ok(experiment) => {
            let promotion_warning =
                promotion_warning_if_variant_promotion_fails(&state, &experiment);
            match concluded_experiment_response(experiment) {
                Ok(response) => attach_experiment_warning_header(
                    Json(response).into_response(),
                    promotion_warning,
                ),
                Err(response) => attach_experiment_warning_header(response, promotion_warning),
            }
        }
        Err(err) => experiment_error_to_response(err),
    }
}

// ── Sub-modules ──────────────────────────────────────────────────
pub(crate) mod estimate;
pub(crate) mod results;

pub use estimate::estimate_ab_test;
pub use results::get_experiment_results;

#[cfg(test)]
use flapjack::experiments::stats;
#[cfg(test)]
use results::*;

#[cfg(test)]
mod review_regression_tests {
    use super::*;
    use crate::test_helpers::{body_json, send_empty_request, send_json_request, TestStateBuilder};
    use axum::{
        http::{Method, StatusCode},
        routing::post,
        Router,
    };
    use tempfile::TempDir;

    fn conclude_test_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/2/abtests", post(create_experiment))
            .route("/2/abtests/:id/start", post(start_experiment))
            .route("/2/abtests/:id/conclude", post(conclude_experiment))
            .with_state(state)
    }

    /// Concluding an experiment should preserve the successful response while
    /// surfacing variant-promotion failures through a warning header.
    #[tokio::test]
    async fn conclude_experiment_sets_warning_header_when_variant_promotion_fails() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp)
            .with_experiments()
            .build_shared();
        let app = conclude_test_router(state.clone());

        state.manager.create_tenant("products").unwrap();

        let create_resp = send_json_request(
            &app,
            Method::POST,
            "/2/abtests",
            serde_json::json!({
                "name": "Promotion warning test",
                "variants": [
                    { "index": "products", "trafficPercentage": 50, "description": "control" },
                    { "index": "products_v2", "trafficPercentage": 50, "description": "variant" }
                ],
                "endAt": "2099-01-01T00:00:00Z",
                "metrics": [{ "name": "clickThroughRate" }]
            }),
        )
        .await;
        assert_eq!(create_resp.status(), StatusCode::OK);
        let id = body_json(create_resp).await["abTestID"].as_i64().unwrap();

        let start_resp =
            send_empty_request(&app, Method::POST, &format!("/2/abtests/{id}/start")).await;
        assert_eq!(start_resp.status(), StatusCode::OK);

        let conclude_resp = send_json_request(
            &app,
            Method::POST,
            &format!("/2/abtests/{id}/conclude"),
            serde_json::json!({
                "winner": "variant",
                "reason": "Promotion should warn when variant settings are missing",
                "controlMetric": 0.12,
                "variantMetric": 0.14,
                "confidence": 0.97,
                "significant": true,
                "promoted": true
            }),
        )
        .await;

        assert_eq!(conclude_resp.status(), StatusCode::OK);
        assert_eq!(
            conclude_resp
                .headers()
                .get(EXPERIMENT_WARNING_HEADER_NAME)
                .and_then(|value| value.to_str().ok()),
            Some(VARIANT_PROMOTION_FAILED_WARNING)
        );
    }

    #[test]
    fn require_numeric_id_mapping_returns_internal_error_when_mapping_missing() {
        let response = require_numeric_id_mapping("exp-123", None)
            .expect_err("missing mapping should return a handler error response");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn require_numeric_id_mapping_preserves_existing_numeric_id() {
        assert_eq!(
            require_numeric_id_mapping("exp-123", Some(42)).unwrap(),
            42,
            "existing numeric mapping should pass through unchanged"
        );
    }
}

#[cfg(test)]
#[path = "../experiments_tests.rs"]
mod tests;
