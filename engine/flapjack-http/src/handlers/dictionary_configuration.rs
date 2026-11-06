//! Node-admin transport for managed application configuration. Preparation is
//! pure; only installation publishes the already-durable canonical generation.
use super::AppState;
use crate::auth::{AuthenticatedAdminKey, AuthenticatedAppId, RequiredApplicationConfiguration};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use flapjack::dictionaries::manager::{DictionaryCommand, DictionarySnapshot};
use std::sync::Arc;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareConfiguration {
    pub base: DictionarySnapshot,
    pub revision: u64,
    pub command: DictionaryCommand,
}

pub(crate) async fn prepare(
    State(state): State<Arc<AppState>>,
    Extension(_admin): Extension<AuthenticatedAdminKey>,
    Extension(AuthenticatedAppId(application)): Extension<AuthenticatedAppId>,
    Json(request): Json<PrepareConfiguration>,
) -> Response {
    match state.dictionary_manager.prepare_configuration(
        &application,
        &request.base,
        request.revision,
        &request.command,
    ) {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => crate::error_response::json_error(StatusCode::BAD_REQUEST, error.to_string()),
    }
}

pub(crate) async fn install(
    State(state): State<Arc<AppState>>,
    Extension(_admin): Extension<AuthenticatedAdminKey>,
    Extension(AuthenticatedAppId(application)): Extension<AuthenticatedAppId>,
    Json(snapshot): Json<DictionarySnapshot>,
) -> Response {
    match state.dictionary_manager.install_configuration(&application, &snapshot) {
        Ok(()) => Json(serde_json::json!({"revision": snapshot.revision, "content_sha256": snapshot.content_sha256})).into_response(),
        Err(error) => crate::error_response::json_error(StatusCode::CONFLICT, error.to_string()),
    }
}

/// Read the application owner without activating or mutating configuration.
pub(crate) async fn current(
    State(state): State<Arc<AppState>>,
    Extension(_admin): Extension<AuthenticatedAdminKey>,
    Extension(AuthenticatedAppId(application)): Extension<AuthenticatedAppId>,
    required: Option<Extension<RequiredApplicationConfiguration>>,
) -> Response {
    match state.dictionary_manager.snapshot_at_least(
        &application,
        required.map_or(0, |Extension(required)| required.0),
    ) {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(_) => crate::error_response::json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "application configuration unavailable",
        ),
    }
}
