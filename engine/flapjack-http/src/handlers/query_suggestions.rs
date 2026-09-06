//! Stub summary for query_suggestions.rs.
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use flapjack::query_suggestions::{build_suggestions_index, QsConfig, QsConfigStore};
use flapjack::validate_index_name;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use super::AppState;
use crate::error_response::{json_error, HandlerError};

/// Standard success response for query-suggestions mutation operations.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QsMutationResponse {
    pub status: u16,
    pub message: String,
}

fn store(state: &AppState) -> QsConfigStore {
    QsConfigStore::new(&state.manager.base_path)
}

fn invalid_input_response(message: String) -> Response {
    json_error(StatusCode::BAD_REQUEST, message)
}

fn config_not_found_response(index_name: &str) -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        format!("No configuration found for '{}'.", index_name),
    )
}

fn mutation_success_response(message: &'static str) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "status": 200,
            "message": message,
        })),
    )
        .into_response()
}

fn validate_qs_index_name(index_name: &str) -> Result<(), String> {
    validate_index_name(index_name).map_err(|e| e.to_string())
}

fn validate_qs_config(config: &QsConfig) -> Result<(), String> {
    validate_qs_index_name(&config.index_name)?;
    for source in &config.source_indices {
        validate_qs_index_name(&source.index_name)?;
        if source.index_name == config.index_name {
            return Err(format!(
                "source index '{}' must differ from the suggestions destination",
                source.index_name
            ));
        }
    }
    Ok(())
}

fn store_error_response(error: std::io::Error) -> Response {
    if error.kind() == std::io::ErrorKind::InvalidInput {
        return json_error(StatusCode::BAD_REQUEST, error.to_string());
    }
    HandlerError::from(error.to_string()).into_response()
}

fn mark_build_running(store: &QsConfigStore, index_name: &str) {
    let mut status = store.load_status(index_name);
    status.is_running = true;
    let _ = store.save_status(&status);
}

/// GET /1/configs — list all Query Suggestions configurations
#[utoipa::path(
    get,
    path = "/1/configs",
    tag = "query-suggestions",
    responses(
        (status = 200, description = "All query suggestions configurations", body = [QsConfig]),
        (status = 500, description = "Query suggestions store read failed")
    ),
    security(("api_key" = []))
)]
pub async fn list_configs(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match store(&state).list_configs() {
        Ok(configs) => Json(json!(configs)).into_response(),
        Err(error) => HandlerError::from(error.to_string()).into_response(),
    }
}

/// Create a new query suggestions configuration and schedule the initial index build.
#[utoipa::path(
    post,
    path = "/1/configs",
    tag = "query-suggestions",
    request_body = QsConfig,
    responses(
        (status = 200, description = "Configuration created and build scheduled", body = QsMutationResponse),
        (status = 400, description = "Invalid query suggestions configuration"),
        (status = 409, description = "Configuration already exists")
    ),
    security(("api_key" = []))
)]
pub async fn create_config(
    State(state): State<Arc<AppState>>,
    Extension(mutation_permit): Extension<crate::pause_registry::MutationPermit>,
    Json(config): Json<QsConfig>,
) -> impl IntoResponse {
    if let Err(message) = validate_qs_config(&config) {
        return invalid_input_response(message);
    }

    let s = store(&state);

    if s.config_exists(&config.index_name) {
        return json_error(
            StatusCode::CONFLICT,
            format!(
                "A configuration for '{}' already exists.",
                config.index_name
            ),
        );
    }

    if let Err(e) = s.save_config(&config) {
        return store_error_response(e);
    }

    // Mark as running and fire off async build
    mark_build_running(&s, &config.index_name);
    spawn_build(Arc::clone(&state), config.clone(), mutation_permit);

    mutation_success_response(
        "Configuration was created, and a new indexing job has been scheduled.",
    )
}

/// GET /1/configs/:indexName — get a single configuration
#[utoipa::path(get, path = "/1/configs/{indexName}", tag = "query-suggestions",
    params(("indexName" = String, Path, description = "Index name")),
    responses(
        (status = 200, description = "Query suggestions configuration", body = QsConfig),
        (status = 400, description = "Invalid index name"),
        (status = 404, description = "Configuration not found")
    ),
    security(("api_key" = [])))]
pub async fn get_config(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
) -> impl IntoResponse {
    if let Err(message) = validate_qs_index_name(&index_name) {
        return invalid_input_response(message);
    }

    match store(&state).load_config(&index_name) {
        Ok(Some(config)) => Json(json!(config)).into_response(),
        Ok(None) => config_not_found_response(&index_name),
        Err(e) => store_error_response(e),
    }
}

/// Update an existing query suggestions configuration.
#[utoipa::path(put, path = "/1/configs/{indexName}", tag = "query-suggestions",
    params(("indexName" = String, Path, description = "Index name")),
    request_body = QsConfig,
    responses(
        (status = 200, description = "Configuration updated and build scheduled", body = QsMutationResponse),
        (status = 400, description = "Invalid index name or configuration"),
        (status = 404, description = "Configuration not found"),
        (status = 409, description = "Build already in progress")
    ),
    security(("api_key" = [])))]
pub async fn update_config(
    State(state): State<Arc<AppState>>,
    Extension(mutation_permit): Extension<crate::pause_registry::MutationPermit>,
    Path(index_name): Path<String>,
    Json(mut config): Json<QsConfig>,
) -> impl IntoResponse {
    if let Err(message) = validate_qs_index_name(&index_name) {
        return invalid_input_response(message);
    }

    let s = store(&state);

    if !s.config_exists(&index_name) {
        return config_not_found_response(&index_name);
    }

    // Ensure the indexName in the body matches the path
    config.index_name = index_name;
    if let Err(message) = validate_qs_config(&config) {
        return invalid_input_response(message);
    }

    // Guard against concurrent builds: two simultaneous builds on the same staging
    // index would corrupt each other (both writing to {indexName}__building).
    let status = s.load_status(&config.index_name);
    if status.is_running {
        return json_error(
            StatusCode::CONFLICT,
            "A build is already in progress. Wait for it to finish before updating.",
        );
    }

    if let Err(e) = s.save_config(&config) {
        return store_error_response(e);
    }

    mark_build_running(&s, &config.index_name);
    spawn_build(Arc::clone(&state), config, mutation_permit);

    mutation_success_response(
        "Configuration was updated, and a new indexing job has been scheduled.",
    )
}

/// DELETE /1/configs/:indexName — delete configuration (does NOT delete the suggestions index)
#[utoipa::path(delete, path = "/1/configs/{indexName}", tag = "query-suggestions",
    params(("indexName" = String, Path, description = "Index name")),
    responses(
        (status = 200, description = "Configuration deleted", body = QsMutationResponse),
        (status = 400, description = "Invalid index name"),
        (status = 404, description = "Configuration not found")
    ),
    security(("api_key" = [])))]
pub async fn delete_config(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
) -> impl IntoResponse {
    if let Err(message) = validate_qs_index_name(&index_name) {
        return invalid_input_response(message);
    }

    let s = store(&state);
    if s.config_exists(&index_name) && s.load_status(&index_name).is_running {
        return json_error(
            StatusCode::CONFLICT,
            "A build is already in progress. Wait for it to finish before deleting.",
        );
    }

    match s.delete_config(&index_name) {
        Ok(true) => mutation_success_response("Configuration was deleted with success."),
        Ok(false) => config_not_found_response(&index_name),
        Err(e) => store_error_response(e),
    }
}

/// GET /1/configs/:indexName/status — build status
#[utoipa::path(get, path = "/1/configs/{indexName}/status", tag = "query-suggestions",
    params(("indexName" = String, Path, description = "Index name")),
    responses(
        (status = 200, description = "Current build status", body = flapjack::query_suggestions::BuildStatus),
        (status = 400, description = "Invalid index name"),
        (status = 404, description = "Configuration not found")
    ),
    security(("api_key" = [])))]
pub async fn get_status(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
) -> impl IntoResponse {
    if let Err(message) = validate_qs_index_name(&index_name) {
        return invalid_input_response(message);
    }

    if !store(&state).config_exists(&index_name) {
        return config_not_found_response(&index_name);
    }
    let status = store(&state).load_status(&index_name);
    Json(json!(status)).into_response()
}

/// GET /1/logs/:indexName — build logs
#[utoipa::path(get, path = "/1/logs/{indexName}", tag = "query-suggestions",
    params(("indexName" = String, Path, description = "Index name")),
    responses(
        (status = 200, description = "Build logs", body = [flapjack::query_suggestions::LogEntry]),
        (status = 400, description = "Invalid index name")
    ),
    security(("api_key" = [])))]
pub async fn get_logs(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
) -> impl IntoResponse {
    if let Err(message) = validate_qs_index_name(&index_name) {
        return invalid_input_response(message);
    }

    let logs = store(&state).read_logs(&index_name);
    Json(json!(logs)).into_response()
}

/// Manually trigger a query suggestions index build for the specified configuration.
#[utoipa::path(post, path = "/1/configs/{indexName}/build", tag = "query-suggestions",
    params(("indexName" = String, Path, description = "Index name")),
    responses(
        (status = 200, description = "Build triggered", body = QsMutationResponse),
        (status = 400, description = "Invalid index name"),
        (status = 404, description = "Configuration not found"),
        (status = 409, description = "Build already in progress")
    ),
    security(("api_key" = [])))]
pub async fn trigger_build(
    State(state): State<Arc<AppState>>,
    Extension(mutation_permit): Extension<crate::pause_registry::MutationPermit>,
    Path(index_name): Path<String>,
) -> impl IntoResponse {
    if let Err(message) = validate_qs_index_name(&index_name) {
        return invalid_input_response(message);
    }

    let s = store(&state);

    let config = match s.load_config(&index_name) {
        Ok(Some(c)) => c,
        Ok(None) => return config_not_found_response(&index_name),
        Err(e) => {
            return store_error_response(e);
        }
    };

    let status = s.load_status(&index_name);
    if status.is_running {
        return json_error(
            StatusCode::CONFLICT,
            "A build is already in progress for this configuration.",
        );
    }

    mark_build_running(&s, &index_name);
    spawn_build(Arc::clone(&state), config, mutation_permit);

    mutation_success_response("Build triggered.")
}

/// Spawn a background build task.
fn spawn_build(
    state: Arc<AppState>,
    config: QsConfig,
    mutation_permit: crate::pause_registry::MutationPermit,
) {
    let manager = Arc::clone(&state.manager);
    let analytics_engine = state.analytics_engine.clone();
    let base_path = state.manager.base_path.clone();

    tokio::spawn(async move {
        let _mutation_permit = mutation_permit;
        let store = QsConfigStore::new(&base_path);

        let engine = match analytics_engine {
            Some(e) => e,
            None => {
                let message = "Build failed: analytics engine not initialized";
                tracing::warn!(
                    "[query-suggestions] Build skipped for '{}': analytics engine not initialized",
                    config.index_name
                );
                if let Err(error) = store.record_failed_build(&config.index_name, message) {
                    tracing::error!(
                        "[query-suggestions] Failed to persist terminal failure for '{}': {}",
                        config.index_name,
                        error
                    );
                }
                return;
            }
        };

        match build_suggestions_index(&config, &store, &manager, &engine).await {
            Ok(count) => tracing::info!(
                "[query-suggestions] Build complete for '{}': {} suggestions",
                config.index_name,
                count
            ),
            Err(e) => {
                tracing::error!(
                    "[query-suggestions] Build failed for '{}': {}",
                    config.index_name,
                    e
                );
                if let Err(error) =
                    store.record_failed_build(&config.index_name, &format!("Build failed: {e}"))
                {
                    tracing::error!(
                        "[query-suggestions] Failed to persist terminal failure for '{}': {}",
                        config.index_name,
                        error
                    );
                }
            }
        }
    });
}
