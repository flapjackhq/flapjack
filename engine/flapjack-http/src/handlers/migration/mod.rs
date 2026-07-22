use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use flapjack::validate_index_name;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

#[allow(dead_code)]
mod algolia_client;
mod export;
mod import;
mod job_runner;
mod source_reader;
mod source_snapshot;
#[cfg(test)]
mod source_test_support;
pub(crate) mod spool;
mod translation;

use super::AppState;
use crate::auth::AuthenticatedAppId;
use crate::error_response::{json_error_parts, json_error_parts_with_code};
use algolia_client::{AlgoliaClient, AlgoliaClientError, AlgoliaErrorKind};
pub use job_runner::{MigrationJobRunner, DEFAULT_ASYNC_MIGRATION_CAPACITY};
use spool::{
    MigrationCancelRequest, MigrationDisposition, MigrationExportProgress, MigrationPhase,
    MigrationPhaseRecord, SpoolErrorKind,
};

const MIGRATION_HA_UNSUPPORTED_CODE: &str = "migration_ha_unsupported";
const MIGRATION_HA_UNSUPPORTED_MESSAGE: &str = "Algolia migration import is unavailable on HA clusters until MIG-7 supplies a costed convergence protocol.";
const MIGRATION_CAPACITY_EXHAUSTED_CODE: &str = "migration_capacity_exhausted";
const MIGRATION_CAPACITY_EXHAUSTED_MESSAGE: &str =
    "Algolia migration import capacity is exhausted; retry later.";
const MIGRATION_JOB_NOT_FOUND_CODE: &str = "migration_job_not_found";
const MIGRATION_JOB_NOT_FOUND_MESSAGE: &str = "Migration job not found";
const MIGRATION_CANCEL_TOO_LATE_CODE: &str = "cancel_too_late";
const MIGRATION_CANCEL_TOO_LATE_MESSAGE: &str =
    "Migration job has already reached the publication commit boundary";

/// Request payload for migrating an index from Algolia to Flapjack.
///
/// Contains Algolia credentials, the source index name, and optional target
/// index settings. Valid requests on HA clusters are refused before import
/// admission; standalone requests synchronously export, translate, stage, and
/// create-only publish the target index.
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

    /// Reserved for a future replacement lane. The synchronous import path is
    /// create-only and refuses overwrite requests before import admission.
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<MigrateWarning>,
    #[serde(rename = "taskID")]
    pub task_id: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MigrateWarning {
    pub code: String,
    pub message: String,
    pub resource: String,
    #[serde(rename = "pageIndex", skip_serializing_if = "Option::is_none")]
    pub page_index: Option<usize>,
    #[serde(rename = "itemIndex", skip_serializing_if = "Option::is_none")]
    pub item_index: Option<usize>,
    #[serde(rename = "jsonPath")]
    pub json_path: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MigrateCount {
    pub imported: usize,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AsyncMigrationPhase {
    Submitted,
    Exporting,
    Preparing,
    Staging,
    Activating,
}

impl From<MigrationPhase> for AsyncMigrationPhase {
    fn from(phase: MigrationPhase) -> Self {
        match phase {
            MigrationPhase::Submitted => Self::Submitted,
            MigrationPhase::Exporting => Self::Exporting,
            MigrationPhase::Preparing => Self::Preparing,
            MigrationPhase::Staging => Self::Staging,
            MigrationPhase::Activating => Self::Activating,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AsyncMigrationDisposition {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl From<MigrationDisposition> for AsyncMigrationDisposition {
    fn from(disposition: MigrationDisposition) -> Self {
        match disposition {
            MigrationDisposition::Running => Self::Running,
            MigrationDisposition::Succeeded => Self::Succeeded,
            MigrationDisposition::Failed => Self::Failed,
            MigrationDisposition::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AsyncMigrationExportProgress {
    pub completed: u64,
    pub total: u64,
}

impl From<MigrationExportProgress> for AsyncMigrationExportProgress {
    fn from(progress: MigrationExportProgress) -> Self {
        Self {
            completed: progress.completed,
            total: progress.total,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AsyncMigrationStatusResponse {
    pub job_id: Uuid,
    pub phase: AsyncMigrationPhase,
    pub disposition: AsyncMigrationDisposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export_progress: Option<AsyncMigrationExportProgress>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<MigrationPhaseRecord> for AsyncMigrationStatusResponse {
    fn from(record: MigrationPhaseRecord) -> Self {
        Self {
            job_id: record.job_uuid,
            phase: record.phase.into(),
            disposition: record.disposition.into(),
            export_progress: record.export_progress.map(Into::into),
            created_at: record.created_at,
            updated_at: record.updated_at,
            terminal_at: record.terminal_at,
        }
    }
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
/// Validates the requested source and target shape, refuses overwrite and HA
/// requests before import admission, then synchronously imports the Algolia
/// source into a create-only Flapjack target. A successful response reports the
/// counts read back from the activated target; this lane has no durable async
/// job id and returns a fixed `taskID` of `0`.
#[utoipa::path(
    post,
    path = "/1/migrate-from-algolia",
    tag = "migration",
    request_body = MigrateFromAlgoliaRequest,
    responses(
        (status = 200, description = "Synchronous Algolia import completed", body = MigrateFromAlgoliaResponse),
        (status = 400, description = "Invalid migration request or unsupported source payload"),
        (status = 409, description = "Target index already exists"),
        (status = 502, description = "Upstream Algolia request failed"),
        (status = 503, description = "migration_ha_unsupported")
    ),
    security(("api_key" = []))
)]
pub async fn migrate_from_algolia(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<MigrateFromAlgoliaRequest>,
) -> Result<Json<MigrateFromAlgoliaResponse>, MigrateError> {
    migrate_from_algolia_impl(state, payload, algolia_source_reader).await
}

/// Additive async Algolia migration submission endpoint.
///
/// This route admits the same create-only migration request as the synchronous
/// `/1/migrate-from-algolia` endpoint, immediately returns the durable
/// admission snapshot, and leaves the synchronous endpoint unchanged.
#[utoipa::path(
    post,
    path = "/1/migrations/algolia",
    tag = "migration",
    request_body = MigrateFromAlgoliaRequest,
    responses(
        (status = 202, description = "Async Algolia migration admitted", body = AsyncMigrationStatusResponse),
        (status = 400, description = "Invalid migration request or unsupported source payload"),
        (status = 500, description = "Migration admission persistence failed"),
        (status = 502, description = "Upstream Algolia request failed"),
        (status = 503, description = "migration_ha_unsupported or migration_capacity_exhausted")
    ),
    security(("api_key" = []))
)]
pub async fn submit_algolia_migration(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    Json(payload): Json<MigrateFromAlgoliaRequest>,
) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError> {
    submit_algolia_migration_impl(state, authenticated_app_id, payload, algolia_source_reader).await
}

#[cfg(test)]
async fn submit_algolia_migration_with_test_source_factory<F, R>(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    Json(payload): Json<MigrateFromAlgoliaRequest>,
    source_factory: F,
) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError>
where
    F: FnOnce(&MigrateFromAlgoliaRequest) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send + 'static,
{
    submit_algolia_migration_impl(state, authenticated_app_id, payload, source_factory).await
}

async fn submit_algolia_migration_impl<F, R>(
    state: Arc<AppState>,
    authenticated_app_id: String,
    payload: MigrateFromAlgoliaRequest,
    source_factory: F,
) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError>
where
    F: FnOnce(&MigrateFromAlgoliaRequest) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send + 'static,
{
    let (_job_uuid, phase_record) = state
        .migration_runner
        .submit_algolia_import_for_owner(payload, Some(authenticated_app_id), source_factory)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(AsyncMigrationStatusResponse::from(phase_record)),
    ))
}

/// Return the durable status for an async Algolia migration job.
///
/// A 404 means no durable phase record is currently retained for the UUID; it
/// is not proof that the UUID never existed because future MIG-9 retention may
/// remove old records.
#[utoipa::path(
    get,
    path = "/1/migrations/algolia/{job_id}",
    tag = "migration",
    params(
        ("job_id" = Uuid, Path, description = "Migration job UUID")
    ),
    responses(
        (status = 200, description = "Durable async Algolia migration status", body = AsyncMigrationStatusResponse),
        (status = 400, description = "Invalid migration job UUID"),
        (status = 404, description = "No durable migration phase record is currently retained for the UUID"),
        (status = 500, description = "Migration status record could not be read")
    ),
    security(("api_key" = []))
)]
pub async fn get_algolia_migration_status(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<AsyncMigrationStatusResponse>, MigrateError> {
    let job_uuid = Uuid::parse_str(&job_id)
        .map_err(|_| json_error_parts(StatusCode::BAD_REQUEST, "job_id must be a valid UUID"))?;
    let spool = import::spool_for_manager(&state.manager)?;
    ensure_async_migration_owner(&spool, job_uuid, &authenticated_app_id)?;
    let phase_record = spool
        .read_migration_phase(job_uuid)
        .map_err(migration_status_spool_error)?;
    Ok(Json(AsyncMigrationStatusResponse::from(phase_record)))
}

/// Request cooperative cancellation for an async Algolia migration job.
///
/// Cancellation is durable and cooperative. Jobs that have not reached the
/// publication commit boundary observe the request at their next checkpoint;
/// terminal jobs are returned unchanged, and post-commit running jobs are too
/// late to cancel because Stage 2 never rolls back a committed target.
#[utoipa::path(
    post,
    path = "/1/migrations/algolia/{job_id}/cancel",
    tag = "migration",
    params(
        ("job_id" = Uuid, Path, description = "Migration job UUID")
    ),
    responses(
        (status = 200, description = "Durable async Algolia migration status after cancel request", body = AsyncMigrationStatusResponse),
        (status = 400, description = "Invalid migration job UUID"),
        (status = 404, description = "No durable migration phase record is currently retained for the UUID"),
        (status = 409, description = "cancel_too_late"),
        (status = 500, description = "Migration cancel request could not be persisted")
    ),
    security(("api_key" = []))
)]
pub async fn cancel_algolia_migration(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<AsyncMigrationStatusResponse>, MigrateError> {
    let job_uuid = Uuid::parse_str(&job_id)
        .map_err(|_| json_error_parts(StatusCode::BAD_REQUEST, "job_id must be a valid UUID"))?;
    let spool = import::spool_for_manager(&state.manager)?;
    ensure_async_migration_owner(&spool, job_uuid, &authenticated_app_id)?;
    match spool
        .request_async_migration_cancel(job_uuid)
        .map_err(migration_status_spool_error)?
    {
        MigrationCancelRequest::Requested(record) => {
            Ok(Json(AsyncMigrationStatusResponse::from(record)))
        }
        MigrationCancelRequest::TooLate(_) => Err(json_error_parts_with_code(
            StatusCode::CONFLICT,
            MIGRATION_CANCEL_TOO_LATE_CODE,
            MIGRATION_CANCEL_TOO_LATE_MESSAGE,
        )),
    }
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

#[cfg(test)]
async fn migrate_from_algolia_with_test_source_factory_and_hooks<F, R>(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<MigrateFromAlgoliaRequest>,
    source_factory: F,
    hooks: import::ImportTestHooks,
) -> Result<Json<MigrateFromAlgoliaResponse>, MigrateError>
where
    F: FnOnce(&MigrateFromAlgoliaRequest) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send,
{
    let target_index = admit_migration_request(&state, &payload)?;
    let mut reader = source_factory(&payload).map_err(algolia_error)?;
    import::import_from_source_with_test_hooks(&state.manager, target_index, &mut reader, hooks)
        .await
}

async fn migrate_from_algolia_impl<F, R>(
    state: Arc<AppState>,
    payload: MigrateFromAlgoliaRequest,
    source_factory: F,
) -> Result<Json<MigrateFromAlgoliaResponse>, MigrateError>
where
    F: FnOnce(&MigrateFromAlgoliaRequest) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send,
{
    let target_index = admit_migration_request(&state, &payload)?;
    let mut reader = source_factory(&payload).map_err(algolia_error)?;
    import::import_from_source(&state.manager, target_index, &mut reader).await
}

fn admit_migration_request(
    state: &AppState,
    payload: &MigrateFromAlgoliaRequest,
) -> Result<String, MigrateError> {
    admit_migration_payload(state.replication_manager.as_ref(), payload)
}

fn admit_migration_payload(
    replication_manager: Option<&Arc<flapjack_replication::manager::ReplicationManager>>,
    payload: &MigrateFromAlgoliaRequest,
) -> Result<String, MigrateError> {
    validate_migration_request(payload)?;
    if payload.overwrite {
        return Err(json_error_parts(
            StatusCode::BAD_REQUEST,
            "overwrite=true is not supported by Algolia migration import",
        ));
    }
    // Persistent v1 HA safety boundary. This guard remains unless ROADMAP.md
    // MIG-7 supplies a costed convergence protocol for the node-local
    // publication guarantee in engine/src/index/manager/publication.rs:11.
    if replication_manager.is_some_and(|manager| manager.peer_count() > 0) {
        return Err(migration_ha_unsupported());
    }
    Ok(migration_target_index(payload).to_string())
}

fn algolia_source_reader(
    payload: &MigrateFromAlgoliaRequest,
) -> Result<source_reader::AlgoliaSourceReader, AlgoliaClientError> {
    source_reader::AlgoliaSourceReader::new(
        &payload.app_id,
        &payload.api_key,
        &payload.source_index,
    )
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

fn migration_target_index(payload: &MigrateFromAlgoliaRequest) -> &str {
    payload
        .target_index
        .as_deref()
        .unwrap_or(payload.source_index.as_str())
}

fn migration_ha_unsupported() -> MigrateError {
    json_error_parts_with_code(
        StatusCode::SERVICE_UNAVAILABLE,
        MIGRATION_HA_UNSUPPORTED_CODE,
        MIGRATION_HA_UNSUPPORTED_MESSAGE,
    )
}

#[allow(dead_code)]
fn migration_capacity_exhausted() -> MigrateError {
    json_error_parts_with_code(
        StatusCode::SERVICE_UNAVAILABLE,
        MIGRATION_CAPACITY_EXHAUSTED_CODE,
        MIGRATION_CAPACITY_EXHAUSTED_MESSAGE,
    )
}

fn algolia_error(error: AlgoliaClientError) -> (StatusCode, Json<serde_json::Value>) {
    let status = match error.kind() {
        AlgoliaErrorKind::Validation => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_GATEWAY,
    };
    json_error_parts(status, error.safe_message())
}

fn migration_status_spool_error(error: spool::SpoolError) -> MigrateError {
    if error.kind() == SpoolErrorKind::JobNotFound {
        return json_error_parts_with_code(
            StatusCode::NOT_FOUND,
            MIGRATION_JOB_NOT_FOUND_CODE,
            MIGRATION_JOB_NOT_FOUND_MESSAGE,
        );
    }
    import::spool_error(error)
}

fn ensure_async_migration_owner(
    spool: &spool::SpoolStore,
    job_uuid: Uuid,
    authenticated_app_id: &str,
) -> Result<(), MigrateError> {
    let metadata = spool
        .read_async_migration_metadata(job_uuid)
        .map_err(migration_status_spool_error)?;
    if metadata.authenticated_app_id.as_deref() == Some(authenticated_app_id) {
        return Ok(());
    }
    Err(json_error_parts_with_code(
        StatusCode::NOT_FOUND,
        MIGRATION_JOB_NOT_FOUND_CODE,
        MIGRATION_JOB_NOT_FOUND_MESSAGE,
    ))
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
            warnings: Vec::new(),
            task_id: 42,
        })
        .expect("migration response should serialize");
        assert_eq!(response["taskID"], 42);
        assert!(response.get("warnings").is_none());
        assert!(response.get("task_id").is_none());

        // Clients read the wire shape, not the Rust struct: pin the camelCase
        // renames and the omission of absent page/item locations.
        let warning = serde_json::to_value(MigrateWarning {
            code: "ReplicaExhaustiveSortApproximated".to_string(),
            message: "approximated".to_string(),
            resource: "Settings".to_string(),
            page_index: None,
            item_index: None,
            json_path: "$.replicas[0]".to_string(),
        })
        .expect("migration warning should serialize");
        assert_eq!(
            warning,
            serde_json::json!({
                "code": "ReplicaExhaustiveSortApproximated",
                "message": "approximated",
                "resource": "Settings",
                "jsonPath": "$.replicas[0]",
            })
        );

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
#[path = "async_status_tests.rs"]
mod async_status_tests;

#[cfg(test)]
#[path = "source_snapshot_tests.rs"]
mod source_snapshot_tests;

#[cfg(test)]
#[path = "import_contract_tests.rs"]
mod import_contract_tests;

#[cfg(test)]
#[path = "source_reader_tests.rs"]
mod source_reader_tests;
