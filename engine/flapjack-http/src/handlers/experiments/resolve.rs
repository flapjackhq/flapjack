//! Stub summary for resolve.rs.
use super::{experiment_error_to_response, AppState};
use crate::error_response::json_error;
use axum::{http::StatusCode, response::Response};
use flapjack::experiments::{config::ExperimentError, store::ExperimentStore};

pub(super) fn experiment_store_unavailable_response() -> Response {
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "experiment store unavailable",
    )
}

#[allow(clippy::result_large_err)] // Response is the handler boundary type and boxing would add indirection to a simple guard helper.
pub(super) fn require_experiment_store(state: &AppState) -> Result<&ExperimentStore, Response> {
    state
        .experiment_store
        .as_deref()
        .ok_or_else(experiment_store_unavailable_response)
}

fn missing_numeric_id_response(experiment_id: &str) -> Response {
    tracing::error!(
        experiment_id = %experiment_id,
        "experiment missing numeric id mapping"
    );
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "experiment missing numeric id mapping",
    )
}

#[allow(clippy::result_large_err)] // Response is the handler boundary type and boxing would add indirection to a tiny consistency guard.
pub(super) fn require_numeric_id_mapping(
    experiment_id: &str,
    numeric_id: Option<i64>,
) -> Result<i64, Response> {
    numeric_id.ok_or_else(|| missing_numeric_id_response(experiment_id))
}

#[allow(clippy::result_large_err)] // Response is the handler boundary type and boxing would add indirection to a tiny helper.
pub(super) fn numeric_id_for_experiment(
    store: &ExperimentStore,
    experiment_id: &str,
) -> Result<i64, Response> {
    require_numeric_id_mapping(experiment_id, store.get_numeric_id(experiment_id))
}

/// TODO: Document resolve_experiment_id.
fn resolve_experiment_id(
    store: &ExperimentStore,
    id_str: &str,
) -> Result<(String, i64), ExperimentError> {
    if let Ok(numeric) = id_str.parse::<i64>() {
        let uuid = store
            .get_uuid_for_numeric(numeric)
            .ok_or_else(|| ExperimentError::NotFound(id_str.to_string()))?;
        return Ok((uuid, numeric));
    }

    let numeric = store
        .get_numeric_id(id_str)
        .ok_or_else(|| ExperimentError::NotFound(id_str.to_string()))?;
    Ok((id_str.to_string(), numeric))
}

#[allow(clippy::result_large_err)] // Response is the handler boundary type and boxing would add indirection for this small resolver.
pub(super) fn resolve_store_and_experiment_id<'a>(
    state: &'a AppState,
    id_str: &str,
) -> Result<(&'a ExperimentStore, String, i64), Response> {
    let store = require_experiment_store(state)?;
    let (uuid, numeric_id) = resolve_experiment_id_response(store, id_str)?;
    Ok((store, uuid, numeric_id))
}

#[allow(clippy::result_large_err)] // Response is the handler boundary type and boxing would add indirection for this small resolver.
pub(super) fn resolve_store_and_experiment_uuid<'a>(
    state: &'a AppState,
    id_str: &str,
) -> Result<(&'a ExperimentStore, String), Response> {
    let (store, uuid, _numeric_id) = resolve_store_and_experiment_id(state, id_str)?;
    Ok((store, uuid))
}

#[allow(clippy::result_large_err)] // Response is the handler boundary type and boxing would add indirection to a small helper.
fn resolve_experiment_id_response(
    store: &ExperimentStore,
    id_str: &str,
) -> Result<(String, i64), Response> {
    if id_str.parse::<i64>().is_err() && store.get_numeric_id(id_str).is_none() {
        match store.get(id_str) {
            Ok(_) => return Err(missing_numeric_id_response(id_str)),
            Err(err) => return Err(experiment_error_to_response(err)),
        }
    }

    resolve_experiment_id(store, id_str).map_err(experiment_error_to_response)
}
