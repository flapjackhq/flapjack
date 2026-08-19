//! Stub summary for personalization.rs.
use axum::{
    extract::{Path, State},
    Json,
};
use serde::Serialize;
use std::sync::Arc;

use super::AppState;
use flapjack::error::FlapjackError;
use flapjack::personalization::{
    PersonalizationProfileStore, PersonalizationStrategy, STRATEGY_FILENAME,
};

// ── Response DTOs ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetStrategyResponse {
    pub updated_at: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProfileResponse {
    pub user_token: String,
    pub deleted_until: String,
}

fn strategy_path(state: &AppState) -> std::path::PathBuf {
    state.manager.base_path.join(STRATEGY_FILENAME)
}

fn profile_store(state: &AppState) -> PersonalizationProfileStore {
    PersonalizationProfileStore::new(&*state.manager.base_path)
}

/// POST /1/strategies/personalization — save strategy config
#[utoipa::path(
    post,
    path = "/1/strategies/personalization",
    tag = "personalization",
    request_body(content = PersonalizationStrategy, description = "Personalization strategy configuration"),
    responses(
        (status = 200, description = "Strategy saved", body = SetStrategyResponse),
        (status = 400, description = "Validation error")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn set_personalization_strategy(
    State(state): State<Arc<AppState>>,
    Json(strategy): Json<PersonalizationStrategy>,
) -> Result<Json<SetStrategyResponse>, FlapjackError> {
    strategy.validate().map_err(FlapjackError::InvalidQuery)?;

    let path = strategy_path(&state);
    let json =
        serde_json::to_string_pretty(&strategy).map_err(|e| FlapjackError::Json(e.to_string()))?;
    flapjack::index::atomic_write_file(&path, json.as_bytes())
        .map_err(|e| FlapjackError::Io(e.to_string()))?;

    let now = chrono::Utc::now().to_rfc3339();
    Ok(Json(SetStrategyResponse { updated_at: now }))
}

/// GET /1/strategies/personalization — load strategy config
#[utoipa::path(
    get,
    path = "/1/strategies/personalization",
    tag = "personalization",
    responses(
        (status = 200, description = "Current strategy", body = PersonalizationStrategy),
        (status = 404, description = "No strategy configured")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn get_personalization_strategy(
    State(state): State<Arc<AppState>>,
) -> Result<Json<PersonalizationStrategy>, FlapjackError> {
    let path = strategy_path(&state);
    if !path.exists() {
        return Err(FlapjackError::TenantNotFound(
            "No personalization strategy configured".to_string(),
        ));
    }

    let data = std::fs::read_to_string(&path).map_err(|e| FlapjackError::Io(e.to_string()))?;
    let strategy: PersonalizationStrategy = serde_json::from_str(&data)
        .map_err(|e| FlapjackError::Io(format!("corrupted strategy file: {}", e)))?;

    Ok(Json(strategy))
}

/// DELETE /1/strategies/personalization — remove strategy config.
pub async fn delete_personalization_strategy(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let path = strategy_path(&state);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(FlapjackError::Io(error.to_string())),
    }

    let now = chrono::Utc::now().to_rfc3339();
    Ok(Json(serde_json::json!({ "deletedAt": now })))
}

/// GET /1/profiles/personalization/{userToken} — compute/load user profile
#[utoipa::path(
    get,
    path = "/1/profiles/personalization/{userToken}",
    tag = "personalization",
    params(
        ("userToken" = String, Path, description = "User token identifier")
    ),
    responses(
        (status = 200, description = "User personalization profile", body = flapjack::personalization::PersonalizationProfile),
        (status = 404, description = "No profile found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn get_user_profile(
    State(state): State<Arc<AppState>>,
    Path(user_token): Path<String>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let store = profile_store(&state);

    // Validate user token format
    store
        .profile_path(&user_token)
        .map_err(FlapjackError::InvalidQuery)?;

    // Load strategy — required for profile computation
    let strategy_path = strategy_path(&state);
    if !strategy_path.exists() {
        return Err(FlapjackError::TenantNotFound(
            "No personalization strategy configured".to_string(),
        ));
    }
    let strategy_data =
        std::fs::read_to_string(&strategy_path).map_err(|e| FlapjackError::Io(e.to_string()))?;
    let strategy: PersonalizationStrategy = serde_json::from_str(&strategy_data)
        .map_err(|e| FlapjackError::Io(format!("corrupted strategy file: {}", e)))?;

    // Try to compute profile from analytics if available
    if let Some(ref analytics_engine) = state.analytics_engine {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let profile = flapjack::personalization::profile::compute_and_cache_profile(
            &store,
            &state.manager,
            analytics_engine,
            &strategy,
            &user_token,
            now_ms,
        )
        .await
        .map_err(FlapjackError::InvalidQuery)?;

        if profile.scores.is_empty() {
            return Err(FlapjackError::TenantNotFound(format!(
                "No profile found for user '{}'",
                user_token
            )));
        }

        return Ok(Json(serde_json::to_value(&profile).map_err(|e| {
            FlapjackError::Io(format!("failed to serialize profile: {}", e))
        })?));
    }

    // Fallback: try loading cached profile
    match store.load_profile(&user_token).map_err(FlapjackError::Io)? {
        Some(profile) if !profile.scores.is_empty() => {
            Ok(Json(serde_json::to_value(&profile).map_err(|e| {
                FlapjackError::Io(format!("failed to serialize profile: {}", e))
            })?))
        }
        _ => Err(FlapjackError::TenantNotFound(format!(
            "No profile found for user '{}'",
            user_token
        ))),
    }
}

/// DELETE /1/profiles/{userToken} — delete user profile (GDPR compliance)
#[utoipa::path(
    delete,
    path = "/1/profiles/{userToken}",
    tag = "personalization",
    params(
        ("userToken" = String, Path, description = "User token identifier")
    ),
    responses(
        (status = 200, description = "Profile deleted", body = DeleteProfileResponse)
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn delete_user_profile(
    State(state): State<Arc<AppState>>,
    Path(user_token): Path<String>,
) -> Result<Json<DeleteProfileResponse>, FlapjackError> {
    let store = profile_store(&state);

    // Validate user token format
    store
        .profile_path(&user_token)
        .map_err(FlapjackError::InvalidQuery)?;

    let _operation = store
        .begin_user_operation(&user_token)
        .await
        .map_err(FlapjackError::Io)?;

    store
        .delete_profile(&user_token)
        .map_err(FlapjackError::Io)?;

    let now = chrono::Utc::now().to_rfc3339();
    Ok(Json(DeleteProfileResponse {
        user_token,
        deleted_until: now,
    }))
}

#[cfg(test)]
#[path = "personalization_tests.rs"]
mod tests;
