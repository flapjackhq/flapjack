//! Stub summary for keys.rs.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::sync::Arc;
use utoipa::ToSchema;

use crate::auth::{epoch_millis_to_rfc3339, ApiKey, KeyStore};
use crate::error_response::json_error;
use crate::security_audit::{emit_admin_action, Action, Actor, Outcome, Target};

/// Deserialize an incoming API key creation request with Algolia-compatible camelCase field names.
///
/// All fields except `acl` are optional and fall back to sensible defaults (empty strings, zero, or empty vecs) when absent.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateKeyRequest {
    pub acl: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub indexes: Option<Vec<String>>,
    #[serde(default, rename = "maxHitsPerQuery")]
    pub max_hits_per_query: Option<i64>,
    #[serde(default, rename = "maxQueriesPerIPPerHour")]
    pub max_queries_per_ip_per_hour: Option<i64>,
    #[serde(default, rename = "queryParameters")]
    pub query_parameters: Option<String>,
    #[serde(default)]
    pub referers: Option<Vec<String>>,
    #[serde(default, rename = "restrictSources")]
    pub restrict_sources: Option<Vec<String>>,
    #[serde(default)]
    pub validity: Option<i64>,
}

impl CreateKeyRequest {
    /// Converts this request into an `ApiKey` with empty hash/salt placeholders.
    fn into_api_key(self) -> ApiKey {
        ApiKey {
            hash: String::new(),
            salt: String::new(),
            hmac_key: None,
            created_at: 0,
            acl: self.acl,
            description: self.description.unwrap_or_default(),
            indexes: self.indexes.unwrap_or_default(),
            max_hits_per_query: self.max_hits_per_query.unwrap_or(0),
            max_queries_per_ip_per_hour: self.max_queries_per_ip_per_hour.unwrap_or(0),
            query_parameters: self.query_parameters.unwrap_or_default(),
            referers: self.referers.unwrap_or_default(),
            restrict_sources: self.restrict_sources,
            validity: self.validity.unwrap_or(0),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateKeyResponse {
    pub key: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ListKeysResponse {
    pub keys: Vec<crate::auth::KeyApiResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UpdateKeyResponse {
    pub key: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteKeyResponse {
    #[serde(rename = "deletedAt")]
    pub deleted_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RestoreKeyResponse {
    pub key: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GenerateSecuredKeyResponse {
    #[serde(rename = "securedApiKey")]
    pub secured_api_key: String,
}

/// Create a new API key with specified ACL permissions and restrictions.
#[utoipa::path(
    post,
    path = "/1/keys",
    tag = "keys",
    request_body(content = CreateKeyRequest, description = "Key configuration"),
    responses(
        (status = 200, description = "Key created", body = CreateKeyResponse)
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn create_key(
    State(key_store): State<Arc<KeyStore>>,
    Json(body): Json<CreateKeyRequest>,
) -> impl IntoResponse {
    if let Some(response) = validate_create_or_update_key_request(&body) {
        return response;
    }

    let key = body.into_api_key();
    let description = key.description.clone();
    let (created, plaintext_value) = match key_store.try_create_key(key) {
        Ok(created) => created,
        Err(error) => {
            tracing::error!(error = %error, "failed to persist created API key");
            emit_admin_action(
                Actor::admin_api_key(),
                Action::CreateKey,
                Target::api_key_collection(),
                Outcome::Failure,
                Some("persistence_failed"),
            );
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
        }
    };

    notify_key_lifecycle(&description, "created");

    let created_at = epoch_millis_to_rfc3339(created.created_at);
    emit_admin_action(
        Actor::admin_api_key(),
        Action::CreateKey,
        Target::api_key_fingerprint(&plaintext_value),
        Outcome::Success,
        None,
    );

    let response = CreateKeyResponse {
        key: plaintext_value,
        created_at,
    };

    (StatusCode::OK, Json(response)).into_response()
}

/// List all API keys
#[utoipa::path(
    get,
    path = "/1/keys",
    tag = "keys",
    responses(
        (status = 200, description = "List of keys", body = ListKeysResponse)
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn list_keys(State(key_store): State<Arc<KeyStore>>) -> impl IntoResponse {
    let keys = key_store.list_all_as_dto();
    Json(ListKeysResponse { keys })
}

/// Get an API key by value
#[utoipa::path(
    get,
    path = "/1/keys/{key}",
    tag = "keys",
    params(
        ("key" = String, Path, description = "API key value")
    ),
    responses(
        (status = 200, description = "Key details", body = crate::auth::KeyApiResponse),
        (status = 404, description = "Key not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn get_key(
    State(key_store): State<Arc<KeyStore>>,
    Path(key_value): Path<String>,
) -> impl IntoResponse {
    match key_store.lookup_as_dto(&key_value) {
        Some(dto) => Json(dto).into_response(),
        None => json_error(StatusCode::NOT_FOUND, "Key not found"),
    }
}

/// Update an API key
#[utoipa::path(
    put,
    path = "/1/keys/{key}",
    tag = "keys",
    params(
        ("key" = String, Path, description = "API key value")
    ),
    request_body(content = CreateKeyRequest, description = "Key updates"),
    responses(
        (status = 200, description = "Key updated", body = UpdateKeyResponse),
        (status = 404, description = "Key not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn update_key(
    State(key_store): State<Arc<KeyStore>>,
    Path(key_value): Path<String>,
    Json(body): Json<CreateKeyRequest>,
) -> impl IntoResponse {
    if let Some(response) = validate_create_or_update_key_request(&body) {
        return response;
    }

    let updated = body.into_api_key();

    match key_store.try_update_key(&key_value, updated) {
        Ok(Some(_)) => {
            emit_admin_action(
                Actor::admin_api_key(),
                Action::UpdateKey,
                Target::api_key_fingerprint(&key_value),
                Outcome::Success,
                None,
            );
            Json(UpdateKeyResponse {
                key: key_value,
                updated_at: current_timestamp(),
            })
            .into_response()
        }
        Ok(None) => {
            emit_admin_action(
                Actor::admin_api_key(),
                Action::UpdateKey,
                Target::api_key_fingerprint(&key_value),
                Outcome::Failure,
                Some("key_not_found"),
            );
            json_error(StatusCode::NOT_FOUND, "Key not found")
        }
        Err(error) => {
            tracing::error!(error = %error, "failed to persist updated API key");
            emit_admin_action(
                Actor::admin_api_key(),
                Action::UpdateKey,
                Target::api_key_fingerprint(&key_value),
                Outcome::Failure,
                Some("persistence_failed"),
            );
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        }
    }
}

/// Delete an API key
#[utoipa::path(
    delete,
    path = "/1/keys/{key}",
    tag = "keys",
    params(
        ("key" = String, Path, description = "API key value")
    ),
    responses(
        (status = 200, description = "Key deleted", body = DeleteKeyResponse),
        (status = 404, description = "Key not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn delete_key(
    State(key_store): State<Arc<KeyStore>>,
    Path(key_value): Path<String>,
) -> impl IntoResponse {
    if key_store.is_admin(&key_value) {
        emit_admin_action(
            Actor::admin_api_key(),
            Action::DeleteKey,
            Target::api_key_fingerprint(&key_value),
            Outcome::Failure,
            Some("cannot_delete_admin_key"),
        );
        return json_error(StatusCode::FORBIDDEN, "Cannot delete admin key");
    }

    // Look up description before deleting (for notification)
    let description = key_store
        .lookup(&key_value)
        .map(|k| k.description.clone())
        .unwrap_or_default();

    match key_store.try_delete_key(&key_value) {
        Ok(true) => {
            emit_admin_action(
                Actor::admin_api_key(),
                Action::DeleteKey,
                Target::api_key_fingerprint(&key_value),
                Outcome::Success,
                None,
            );
            notify_key_lifecycle(&description, "deleted");

            Json(DeleteKeyResponse {
                deleted_at: current_timestamp(),
            })
            .into_response()
        }
        Ok(false) => {
            emit_admin_action(
                Actor::admin_api_key(),
                Action::DeleteKey,
                Target::api_key_fingerprint(&key_value),
                Outcome::Failure,
                Some("key_not_found"),
            );
            json_error(StatusCode::NOT_FOUND, "Key not found")
        }
        Err(error) => {
            tracing::error!(error = %error, "failed to persist deleted API key");
            emit_admin_action(
                Actor::admin_api_key(),
                Action::DeleteKey,
                Target::api_key_fingerprint(&key_value),
                Outcome::Failure,
                Some("persistence_failed"),
            );
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        }
    }
}

/// Restore a deleted API key
#[utoipa::path(
    post,
    path = "/1/keys/{key}/restore",
    tag = "keys",
    params(
        ("key" = String, Path, description = "API key value")
    ),
    responses(
        (status = 200, description = "Key restored", body = RestoreKeyResponse),
        (status = 404, description = "Key not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn restore_key(
    State(key_store): State<Arc<KeyStore>>,
    Path(key_value): Path<String>,
) -> impl IntoResponse {
    match key_store.try_restore_key(&key_value) {
        Ok(Some(_)) => {
            emit_admin_action(
                Actor::admin_api_key(),
                Action::RestoreKey,
                Target::api_key_fingerprint(&key_value),
                Outcome::Success,
                None,
            );
            Json(RestoreKeyResponse {
                key: key_value,
                created_at: current_timestamp(),
            })
            .into_response()
        }
        Ok(None) => {
            emit_admin_action(
                Actor::admin_api_key(),
                Action::RestoreKey,
                Target::api_key_fingerprint(&key_value),
                Outcome::Failure,
                Some("key_not_found"),
            );
            json_error(StatusCode::NOT_FOUND, "Key not found")
        }
        Err(error) => {
            tracing::error!(error = %error, "failed to persist restored API key");
            emit_admin_action(
                Actor::admin_api_key(),
                Action::RestoreKey,
                Target::api_key_fingerprint(&key_value),
                Outcome::Failure,
                Some("persistence_failed"),
            );
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        }
    }
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenerateSecuredKeyRequest {
    pub parent_api_key: String,
    #[serde(default)]
    pub restrictions: SecuredKeyRestrictions,
}

#[derive(Debug, Deserialize, Default, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecuredKeyRestrictions {
    #[serde(default)]
    pub filters: Option<String>,
    #[serde(default)]
    pub valid_until: Option<i64>,
    #[serde(default)]
    pub restrict_indices: Option<Vec<String>>,
    #[serde(default)]
    pub user_token: Option<String>,
    #[serde(default)]
    pub hits_per_page: Option<usize>,
    #[serde(default)]
    pub restrict_sources: Option<String>,
}

impl SecuredKeyRestrictions {
    /// Serializes these restrictions into a URL query string for HMAC signing.
    fn to_query_params(&self) -> String {
        let mut params = Vec::new();
        push_encoded_query_param(&mut params, "filters", self.filters.as_deref());
        push_plain_query_param(&mut params, "validUntil", self.valid_until);
        push_json_query_param(
            &mut params,
            "restrictIndices",
            self.restrict_indices.as_ref(),
        );
        push_encoded_query_param(&mut params, "userToken", self.user_token.as_deref());
        push_plain_query_param(&mut params, "hitsPerPage", self.hits_per_page);
        push_encoded_query_param(
            &mut params,
            "restrictSources",
            self.restrict_sources.as_deref(),
        );
        params.join("&")
    }
}

fn validate_create_or_update_key_request(
    body: &CreateKeyRequest,
) -> Option<axum::response::Response> {
    validate_key_request_acls(&body.acl)
        .or_else(|| validate_request_restrict_sources_entries(body.restrict_sources.as_deref()))
}

fn validate_key_request_acls(acls: &[String]) -> Option<axum::response::Response> {
    crate::auth::validate_acls(acls)
        .err()
        .map(|invalid| json_error(StatusCode::BAD_REQUEST, format!("Invalid ACL: {invalid}")))
}

fn validate_request_restrict_sources_entries(
    restrict_sources: Option<&[String]>,
) -> Option<axum::response::Response> {
    restrict_sources.and_then(|entries| {
        crate::auth::validate_restrict_sources_entries(entries)
            .err()
            .map(invalid_restrict_sources_response)
    })
}

fn validate_request_restrict_sources_csv(
    restrict_sources: Option<&str>,
) -> Option<axum::response::Response> {
    restrict_sources.and_then(|entries| {
        crate::auth::validate_restrict_sources_csv(entries)
            .err()
            .map(invalid_restrict_sources_response)
    })
}

fn invalid_restrict_sources_response(invalid: String) -> axum::response::Response {
    json_error(
        StatusCode::BAD_REQUEST,
        format!("Invalid restrictSources entry: {invalid}"),
    )
}

fn notify_key_lifecycle(description: &str, event: &str) {
    if let Some(notifier) = crate::notifications::global_notifier() {
        notifier.send_key_lifecycle(description, event);
    }
}

fn current_timestamp() -> String {
    Utc::now().to_rfc3339()
}

fn push_encoded_query_param(parts: &mut Vec<String>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        parts.push(format!("{key}={}", urlencoding::encode(value)));
    }
}

fn push_plain_query_param<T: Display>(parts: &mut Vec<String>, key: &str, value: Option<T>) {
    if let Some(value) = value {
        parts.push(format!("{key}={value}"));
    }
}

fn push_json_query_param<T: Serialize>(parts: &mut Vec<String>, key: &str, value: Option<&T>) {
    if let Some(value) = value {
        let json = serde_json::to_string(value).unwrap_or_default();
        parts.push(format!("{key}={}", urlencoding::encode(&json)));
    }
}

/// Generate a secured API key with restrictions
#[utoipa::path(
    post,
    path = "/1/keys/generateSecuredApiKey",
    tag = "keys",
    request_body(content = GenerateSecuredKeyRequest, description = "Secured key restrictions"),
    responses(
        (status = 200, description = "Secured key generated", body = GenerateSecuredKeyResponse),
        (status = 400, description = "Invalid request")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn generate_secured_key(
    State(key_store): State<Arc<KeyStore>>,
    Json(body): Json<GenerateSecuredKeyRequest>,
) -> impl IntoResponse {
    if let Some(response) =
        validate_request_restrict_sources_csv(body.restrictions.restrict_sources.as_deref())
    {
        return response;
    }

    // Look up the parent key
    let parent_key = match key_store.lookup(&body.parent_api_key) {
        Some(k) => k,
        None => return json_error(StatusCode::NOT_FOUND, "Parent key not found"),
    };

    // Admin keys cannot be used as parents for secured keys
    if parent_key.hmac_key.is_none() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Cannot generate secured key from admin key",
        );
    }

    let params_str = body.restrictions.to_query_params();
    // Use the hmac_key for secured key generation
    let secured_key = crate::auth::generate_secured_api_key(&body.parent_api_key, &params_str);
    emit_admin_action(
        Actor::admin_api_key(),
        Action::GenerateSecuredKey,
        Target::api_key_fingerprint(&body.parent_api_key),
        Outcome::Success,
        None,
    );

    Json(GenerateSecuredKeyResponse {
        secured_api_key: secured_key,
    })
    .into_response()
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
