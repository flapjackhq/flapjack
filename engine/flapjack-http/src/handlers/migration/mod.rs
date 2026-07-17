use axum::{extract::State, http::StatusCode, Json};
use flapjack::validate_index_name;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

// MIG-1 temporarily refuses destination import, leaving source-import client
// methods dormant while `list_algolia_indexes` still uses this shared module.
// MIG-3 removes the refusal and makes these paths live again.
#[allow(dead_code)]
mod algolia_client;
#[cfg(test)]
#[allow(dead_code)]
mod export;
#[cfg(test)]
mod source_reader;
mod source_snapshot;
#[cfg(test)]
mod source_test_support;
#[cfg(test)]
mod spool;
mod translation;

use super::AppState;
use crate::error_response::{json_error_parts, json_error_parts_with_code};
use algolia_client::{AlgoliaClient, AlgoliaClientError, AlgoliaErrorKind};

const MIGRATION_IMPORT_UNAVAILABLE_CODE: &str = "migration_import_unavailable";
const MIGRATION_IMPORT_UNAVAILABLE_MESSAGE: &str = "Migration import leg is not implemented; no data was written, and this endpoint will not report success it did not perform.";
const MIGRATION_HA_UNSUPPORTED_CODE: &str = "migration_ha_unsupported";
const MIGRATION_HA_UNSUPPORTED_MESSAGE: &str = "Algolia migration import is unavailable on HA clusters until MIG-7 supplies a costed convergence protocol.";

/// Request payload for migrating an index from Algolia to Flapjack.
///
/// Contains Algolia credentials, the source index name, and optional target
/// index settings. Valid requests on HA clusters are refused before import
/// admission; standalone requests are temporarily refused without writing data
/// until the destination import leg is implemented.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MigrateFromAlgoliaRequest {
    #[serde(rename = "appId")]
    pub app_id: String,

    #[serde(rename = "apiKey")]
    pub api_key: String,

    #[serde(rename = "sourceIndex")]
    pub source_index: String,

    #[serde(rename = "targetIndex")]
    pub target_index: Option<String>,

    /// Reserved for the future import leg. Valid requests are currently refused
    /// without deleting or overwriting any target index.
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MigrateFromAlgoliaResponse {
    pub status: String,
    pub settings: bool,
    pub synonyms: MigrateCount,
    pub rules: MigrateCount,
    pub objects: MigrateCount,
    #[serde(rename = "taskID")]
    pub task_id: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MigrateCount {
    pub imported: usize,
}

// ── List Algolia indexes ────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct ListAlgoliaIndexesRequest {
    #[serde(rename = "appId")]
    pub app_id: String,

    #[serde(rename = "apiKey")]
    pub api_key: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AlgoliaIndexInfo {
    pub name: String,
    pub entries: u64,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ListAlgoliaIndexesResponse {
    pub indexes: Vec<AlgoliaIndexInfo>,
}

/// List all indexes available in an Algolia application.
///
/// Validates that `appId` and `apiKey` are non-empty, then calls the Algolia
/// `/1/indexes` endpoint and returns index name, entry count, and last-updated
/// timestamp for each index. Returns 400 if credentials are missing or 502 if
/// the upstream Algolia call fails.
#[utoipa::path(
    post,
    path = "/1/algolia-list-indexes",
    tag = "migration",
    request_body = ListAlgoliaIndexesRequest,
    responses(
        (status = 200, description = "Available Algolia indexes", body = ListAlgoliaIndexesResponse),
        (status = 400, description = "Missing Algolia credentials"),
        (status = 502, description = "Upstream Algolia request failed")
    ),
    security(("api_key" = []))
)]
pub async fn list_algolia_indexes(
    Json(payload): Json<ListAlgoliaIndexesRequest>,
) -> Result<Json<ListAlgoliaIndexesResponse>, (StatusCode, Json<serde_json::Value>)> {
    if payload.app_id.is_empty() || payload.api_key.is_empty() {
        return Err(json_error_parts(
            StatusCode::BAD_REQUEST,
            "appId and apiKey are required",
        ));
    }

    let client = AlgoliaClient::new(&payload.app_id, &payload.api_key).map_err(algolia_error)?;
    let indexes = client
        .list_indexes()
        .await
        .map_err(algolia_error)?
        .into_iter()
        .map(|index| AlgoliaIndexInfo {
            name: index.name,
            entries: index.entries,
            updated_at: index.updated_at,
        })
        .collect();

    Ok(Json(ListAlgoliaIndexesResponse { indexes }))
}

type MigrateError = (StatusCode, Json<serde_json::Value>);

/// One-click migration from Algolia to Flapjack.
///
/// Validates the requested source and target shape, then refuses HA clusters
/// before import admission. Standalone requests are temporarily refused without
/// reading Algolia or writing Flapjack data because the destination import leg
/// is not implemented.
#[utoipa::path(
    post,
    path = "/1/migrate-from-algolia",
    tag = "migration",
    request_body = MigrateFromAlgoliaRequest,
    responses(
        (status = 400, description = "Invalid migration request"),
        (status = 503, description = "migration_ha_unsupported or migration_import_unavailable")
    ),
    security(("api_key" = []))
)]
pub async fn migrate_from_algolia(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<MigrateFromAlgoliaRequest>,
) -> Result<Json<MigrateFromAlgoliaResponse>, MigrateError> {
    migrate_from_algolia_impl(state, payload, source_reader_unavailable_until_mig3).await
}

#[cfg(test)]
async fn migrate_from_algolia_with_test_source_factory<F, R>(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<MigrateFromAlgoliaRequest>,
    source_factory: F,
) -> Result<Json<MigrateFromAlgoliaResponse>, MigrateError>
where
    F: FnOnce(&MigrateFromAlgoliaRequest) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send,
{
    migrate_from_algolia_impl(state, payload, source_factory).await
}

async fn migrate_from_algolia_impl<F, R>(
    state: Arc<AppState>,
    payload: MigrateFromAlgoliaRequest,
    _source_factory: F,
) -> Result<Json<MigrateFromAlgoliaResponse>, MigrateError>
where
    F: FnOnce(&MigrateFromAlgoliaRequest) -> Result<R, AlgoliaClientError>,
{
    validate_migration_request(&payload)?;
    // Persistent v1 HA safety boundary. MIG-3 removes only the later
    // import-unavailable refusal; this guard remains unless ROADMAP.md MIG-7
    // supplies a costed convergence protocol for the node-local publication
    // guarantee in engine/src/index/manager/publication.rs:11.
    if state
        .replication_manager
        .as_ref()
        .is_some_and(|manager| manager.peer_count() > 0)
    {
        return Err(migration_ha_unsupported());
    }
    // Temporary MIG-1 honesty guard tracked by ROADMAP.md; MIG-3 removes this
    // once the destination import leg can write and verify target contents.
    Err(migration_import_unavailable())
}

fn source_reader_unavailable_until_mig3(
    _payload: &MigrateFromAlgoliaRequest,
) -> Result<(), AlgoliaClientError> {
    Ok(())
}

fn validate_migration_request(payload: &MigrateFromAlgoliaRequest) -> Result<(), MigrateError> {
    if payload.app_id.is_empty() || payload.api_key.is_empty() || payload.source_index.is_empty() {
        return Err(json_error_parts(
            StatusCode::BAD_REQUEST,
            "appId, apiKey, and sourceIndex are required",
        ));
    }

    let target_index = payload
        .target_index
        .as_deref()
        .unwrap_or(payload.source_index.as_str());
    validate_index_name(target_index)
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))
}

fn migration_import_unavailable() -> MigrateError {
    json_error_parts_with_code(
        StatusCode::SERVICE_UNAVAILABLE,
        MIGRATION_IMPORT_UNAVAILABLE_CODE,
        MIGRATION_IMPORT_UNAVAILABLE_MESSAGE,
    )
}

fn migration_ha_unsupported() -> MigrateError {
    json_error_parts_with_code(
        StatusCode::SERVICE_UNAVAILABLE,
        MIGRATION_HA_UNSUPPORTED_CODE,
        MIGRATION_HA_UNSUPPORTED_MESSAGE,
    )
}

fn algolia_error(error: AlgoliaClientError) -> (StatusCode, Json<serde_json::Value>) {
    let status = match error.kind() {
        AlgoliaErrorKind::Validation => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_GATEWAY,
    };
    json_error_parts(status, error.safe_message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{body_json, TestStateBuilder};
    use axum::response::IntoResponse;
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn migration_dto_wire_contract() {
        let migrate_request: MigrateFromAlgoliaRequest = serde_json::from_value(json!({
            "appId": "APPID",
            "apiKey": "source-key",
            "sourceIndex": "products",
            "targetIndex": "products_copy"
        }))
        .expect("camelCase migration request should deserialize");
        assert_eq!(migrate_request.app_id, "APPID");
        assert_eq!(migrate_request.api_key, "source-key");
        assert_eq!(migrate_request.source_index, "products");
        assert_eq!(
            migrate_request.target_index.as_deref(),
            Some("products_copy")
        );
        assert!(
            !migrate_request.overwrite,
            "overwrite should default to false"
        );

        let list_request: ListAlgoliaIndexesRequest = serde_json::from_value(json!({
            "appId": "APPID",
            "apiKey": "source-key"
        }))
        .expect("camelCase list request should deserialize");
        assert_eq!(list_request.app_id, "APPID");
        assert_eq!(list_request.api_key, "source-key");

        let response = serde_json::to_value(MigrateFromAlgoliaResponse {
            status: "complete".to_string(),
            settings: true,
            synonyms: MigrateCount { imported: 2 },
            rules: MigrateCount { imported: 3 },
            objects: MigrateCount { imported: 5 },
            task_id: 42,
        })
        .expect("migration response should serialize");
        assert_eq!(response["taskID"], 42);
        assert!(response.get("task_id").is_none());

        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let migrate_error = migrate_from_algolia(
            State(state),
            Json(MigrateFromAlgoliaRequest {
                app_id: String::new(),
                api_key: String::new(),
                source_index: String::new(),
                target_index: None,
                overwrite: false,
            }),
        )
        .await
        .expect_err("empty migration credentials should fail validation");
        assert_eq!(migrate_error.0, StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(migrate_error.1.into_response()).await,
            json!({
                "message": "appId, apiKey, and sourceIndex are required",
                "status": 400
            })
        );

        let list_error = list_algolia_indexes(Json(ListAlgoliaIndexesRequest {
            app_id: String::new(),
            api_key: String::new(),
        }))
        .await
        .expect_err("empty list credentials should fail validation");
        assert_eq!(list_error.0, StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(list_error.1.into_response()).await,
            json!({
                "message": "appId and apiKey are required",
                "status": 400
            })
        );
    }

    #[test]
    fn migration_request_validation_preserves_target_index_contract() {
        let request = MigrateFromAlgoliaRequest {
            app_id: "APPID".to_string(),
            api_key: "source-key".to_string(),
            source_index: "products".to_string(),
            target_index: Some("../escape".to_string()),
            overwrite: false,
        };

        let error = validate_migration_request(&request)
            .expect_err("invalid targetIndex should fail before export starts");

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
    }
}

#[cfg(test)]
#[path = "source_snapshot_tests.rs"]
mod source_snapshot_tests;

#[cfg(test)]
#[path = "import_contract_tests.rs"]
mod import_contract_tests;

#[cfg(test)]
#[path = "source_reader_tests.rs"]
mod source_reader_tests;
