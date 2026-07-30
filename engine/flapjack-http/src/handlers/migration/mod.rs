use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use flapjack::index::manager::publication::{
    verify_current_generation_evidence, PublicationGenerationEvidence, PublicationStagingBaseline,
    PublicationTarget,
};
use flapjack::index::rules::RuleStore;
use flapjack::index::synonyms::SynonymStore;
use flapjack::validate_index_name;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

#[allow(dead_code)]
mod algolia_client;
mod bulk_build;
pub mod bulk_replace;
mod export;
mod import;
mod job_runner;
mod source_identity_partitions;
mod source_reader;
mod source_snapshot;
#[cfg(test)]
mod source_test_support;
pub(crate) mod spool;
mod translation;

use super::AppState;
use crate::auth::AuthenticatedAppId;
use crate::error_response::{json_error_parts, json_error_parts_with_code};
use crate::handlers::index_resource_store::{delete_resource_item, load_existing_store};
use algolia_client::{AlgoliaClient, AlgoliaClientError, AlgoliaErrorKind};
pub use bulk_replace::{
    cancel_bulk_replace_http, get_bulk_replace_status_http, submit_bulk_replace_http,
    BulkReplaceReceipt,
};
pub use job_runner::{MigrationJobRunner, DEFAULT_ASYNC_MIGRATION_CAPACITY};
use spool::{
    MigrationCancelRequest, MigrationDisposition, MigrationExportProgress, MigrationImportWarning,
    MigrationPhase, MigrationPhaseRecord, PrivacyScrubAdmission, PrivacyScrubIntent,
    PrivacyScrubIntentFields, SpoolError, SpoolErrorKind,
};

const MIGRATION_CANCELLED_CODE: &str = "migration_cancelled";
const MIGRATION_CANCELLED_MESSAGE: &str = "Algolia migration cancellation was requested";
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
const SOURCE_PROVIDER_UNSUPPORTED_CODE: &str = "source_provider_unsupported";
const SOURCE_PROVIDER_UNSUPPORTED_MESSAGE: &str = "Source provider is not supported";
const PRIVACY_SCRUB_UNKNOWN_TARGET_CODE: &str = "privacy_scrub_unknown_target";
const PRIVACY_SCRUB_STALE_GENERATION_CODE: &str = "privacy_scrub_stale_generation";
const PRIVACY_SCRUB_MISMATCHED_INTENT_CODE: &str = "privacy_scrub_mismatched_intent";
const PRIVACY_SCRUB_INTERRUPTED_RETRYABLE_CODE: &str = "privacy_scrub_interrupted_retryable";
const MIGRATION_ACK_TOO_EARLY_CODE: &str = "migration_ack_too_early";
const MIGRATION_ACK_TOO_EARLY_MESSAGE: &str =
    "Migration job must be terminal before it can be acknowledged";
const MIGRATION_ACK_STALE_GENERATION_CODE: &str = "migration_ack_stale_generation";
const MIGRATION_ACK_STALE_GENERATION_MESSAGE: &str =
    "Migration publication generation evidence is stale or unavailable";

/// Request payload for migrating an index from Algolia to Flapjack.
///
/// Contains Algolia credentials, the source index name, and optional target
/// index settings. HA imports are refused before import admission. Standalone
/// synchronous requests create a fresh target by default; `overwrite=true`
/// replaces an existing target through the node-local fenced publication path.
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

    /// Replace the target index only on node-local migration endpoints. HA
    /// imports still refuse overwrite requests.
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AsyncMigrationSourceProvider {
    #[default]
    Algolia,
    Meilisearch,
    Typesense,
}

impl AsyncMigrationSourceProvider {
    pub(crate) const PUBLIC: [Self; 3] = [Self::Algolia, Self::Meilisearch, Self::Typesense];

    #[allow(dead_code)]
    pub(crate) fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Algolia => Some("algolia"),
            Self::Meilisearch => Some("meilisearch"),
            Self::Typesense => Some("typesense"),
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "algolia" => Some(Self::Algolia),
            "meilisearch" => Some(Self::Meilisearch),
            "typesense" => Some(Self::Typesense),
            _ => None,
        }
    }

    fn is_algolia(&self) -> bool {
        *self == Self::Algolia
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MigrationTopology {
    SingleNodeOnly,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
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
    pub target_index: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology: Option<MigrationTopology>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export_progress: Option<AsyncMigrationExportProgress>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_at: Option<chrono::DateTime<chrono::Utc>>,
    // Present only for a successfully activated import; carried verbatim from the
    // durable outcome, never fabricated as zeros for running/failed/cancelled jobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_applied: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objects_imported: Option<MigrateCount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonyms_imported: Option<MigrateCount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules_imported: Option<MigrateCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<MigrateWarning>,
}

impl From<MigrationImportWarning> for MigrateWarning {
    fn from(warning: MigrationImportWarning) -> Self {
        Self {
            code: warning.code,
            message: warning.message,
            resource: warning.resource,
            page_index: warning.page_index,
            item_index: warning.item_index,
            json_path: warning.json_path,
        }
    }
}

impl From<MigrationPhaseRecord> for AsyncMigrationStatusResponse {
    fn from(record: MigrationPhaseRecord) -> Self {
        let show_import_outcome =
            record.disposition == MigrationDisposition::Succeeded && record.terminal_at.is_some();
        let (settings_applied, objects_imported, synonyms_imported, rules_imported, warnings) =
            match (show_import_outcome, record.import_outcome) {
                (true, Some(outcome)) => (
                    Some(outcome.settings_applied),
                    Some(MigrateCount {
                        imported: outcome.objects_imported,
                    }),
                    Some(MigrateCount {
                        imported: outcome.synonyms_imported,
                    }),
                    Some(MigrateCount {
                        imported: outcome.rules_imported,
                    }),
                    outcome
                        .warnings
                        .into_iter()
                        .map(MigrateWarning::from)
                        .collect(),
                ),
                _ => (None, None, None, None, Vec::new()),
            };
        Self {
            job_id: record.job_uuid,
            phase: record.phase.into(),
            disposition: record.disposition.into(),
            target_index: None,
            topology: None,
            export_progress: record.export_progress.map(Into::into),
            created_at: record.created_at,
            updated_at: record.updated_at,
            terminal_at: record.terminal_at,
            settings_applied,
            objects_imported,
            synonyms_imported,
            rules_imported,
            warnings,
        }
    }
}

impl AsyncMigrationStatusResponse {
    fn with_metadata(
        record: MigrationPhaseRecord,
        metadata: &spool::AsyncMigrationMetadata,
    ) -> Self {
        let mut response = Self::from(record);
        response.target_index = Some(metadata.target_index.clone());
        response.topology = metadata.topology;
        response
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

#[derive(Debug, Deserialize, ToSchema)]
pub struct PrivacyScrubRequest {
    #[serde(rename = "scrubId")]
    pub scrub_id: String,
    pub tenant: String,
    #[serde(rename = "expectedGeneration")]
    pub expected_generation: String,
    #[serde(default, rename = "objectIDs")]
    pub object_ids: Vec<String>,
    #[serde(default, rename = "synonymIDs")]
    pub synonym_ids: Vec<String>,
    #[serde(default, rename = "ruleIDs")]
    pub rule_ids: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PrivacyScrubAck {
    #[serde(rename = "scrubId")]
    pub scrub_id: String,
    pub disposition: String,
}

#[derive(Clone, Copy)]
pub(super) enum MigrationPublicationMode {
    CreateOnly,
    ReplaceExisting {
        staging_baseline: PublicationStagingBaseline,
    },
}

pub(super) struct AdmittedMigration {
    target_index: String,
    publication_mode: MigrationPublicationMode,
}

/// One-click migration from Algolia to Flapjack.
///
/// Validates the requested source and target shape, refuses HA requests before
/// import admission, then synchronously imports the Algolia source into a
/// Flapjack target. The default path is create-only; node-local
/// `overwrite=true` replaces an existing target through the fenced publication
/// owner. A successful response reports the imported source counts after
/// reading back the activated target; an overwrite target can additionally
/// contain acknowledged writes replayed during activation. This lane has no
/// durable async job id and returns a fixed `taskID` of `0`.
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

macro_rules! define_source_migration_openapi_lifecycle {
    (
        $provider:ident,
        submit: $submit_fn:ident => $submit_path:literal,
        status: $status_fn:ident => $status_path:literal,
        cancel: $cancel_fn:ident => $cancel_path:literal,
        acknowledge: $acknowledge_fn:ident => $acknowledge_path:literal
    ) => {
        /// Submit an asynchronous source migration.
        #[utoipa::path(
            post,
            path = $submit_path,
            tag = "migration",
            request_body = MigrateFromAlgoliaRequest,
            responses(
                (status = 202, description = "Async source migration admitted", body = AsyncMigrationStatusResponse),
                (status = 400, description = "Invalid migration request or unsupported source payload"),
                (status = 500, description = "Migration admission persistence failed"),
                (status = 502, description = "Upstream source provider request failed"),
                (status = 503, description = "migration_ha_unsupported or migration_capacity_exhausted")
            ),
            security(("api_key" = []))
        )]
        pub async fn $submit_fn(
            State(state): State<Arc<AppState>>,
            Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
            Json(payload): Json<MigrateFromAlgoliaRequest>,
        ) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError> {
            submit_algolia_migration_impl(
                AsyncMigrationSourceProvider::$provider,
                state,
                authenticated_app_id,
                payload,
                algolia_source_reader,
            )
            .await
        }

        /// Return the durable status for an asynchronous source migration.
        #[utoipa::path(
            get,
            path = $status_path,
            tag = "migration",
            params(
                ("job_id" = Uuid, Path, description = "Migration job UUID")
            ),
            responses(
                (status = 200, description = "Durable async source migration status", body = AsyncMigrationStatusResponse),
                (status = 400, description = "Invalid migration job UUID"),
                (status = 404, description = "No durable migration phase record is currently retained for the UUID"),
                (status = 500, description = "Migration status record could not be read")
            ),
            security(("api_key" = []))
        )]
        pub async fn $status_fn(
            State(state): State<Arc<AppState>>,
            Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
            AxumPath(job_id): AxumPath<String>,
        ) -> Result<Json<AsyncMigrationStatusResponse>, MigrateError> {
            get_source_migration_status(
                state,
                authenticated_app_id,
                job_id,
                Some(AsyncMigrationSourceProvider::$provider),
            )
            .await
        }

        /// Request cooperative cancellation for an asynchronous source migration.
        #[utoipa::path(
            post,
            path = $cancel_path,
            tag = "migration",
            params(
                ("job_id" = Uuid, Path, description = "Migration job UUID")
            ),
            responses(
                (status = 200, description = "Durable async source migration status after cancel request", body = AsyncMigrationStatusResponse),
                (status = 400, description = "Invalid migration job UUID"),
                (status = 404, description = "No durable migration phase record is currently retained for the UUID"),
                (status = 409, description = "cancel_too_late"),
                (status = 500, description = "Migration cancel request could not be persisted")
            ),
            security(("api_key" = []))
        )]
        pub async fn $cancel_fn(
            State(state): State<Arc<AppState>>,
            Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
            AxumPath(job_id): AxumPath<String>,
        ) -> Result<Json<AsyncMigrationStatusResponse>, MigrateError> {
            cancel_source_migration(
                state,
                authenticated_app_id,
                job_id,
                Some(AsyncMigrationSourceProvider::$provider),
            )
            .await
        }

        /// Confirm that the control plane observed a terminal source migration.
        #[utoipa::path(
            post,
            path = $acknowledge_path,
            tag = "migration",
            params(
                ("job_id" = Uuid, Path, description = "Migration job UUID")
            ),
            responses(
                (status = 204, description = "Terminal migration acknowledged"),
                (status = 400, description = "Invalid migration job UUID"),
                (status = 404, description = "No durable migration phase record is currently retained for the UUID"),
                (status = 409, description = "migration_ack_too_early"),
                (status = 500, description = "Migration status record could not be read")
            ),
            security(("api_key" = []))
        )]
        pub async fn $acknowledge_fn(
            State(state): State<Arc<AppState>>,
            Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
            headers: HeaderMap,
            AxumPath(job_id): AxumPath<String>,
        ) -> Result<StatusCode, MigrateError> {
            acknowledge_source_migration(
                state,
                authenticated_owner_identity(authenticated_app_id, &headers),
                job_id,
                Some(AsyncMigrationSourceProvider::$provider),
            )
            .await
        }
    };
}

define_source_migration_openapi_lifecycle!(
    Algolia,
    submit: submit_algolia_migration => "/1/migrations/algolia",
    status: get_algolia_migration_status => "/1/migrations/algolia/{job_id}",
    cancel: cancel_algolia_migration => "/1/migrations/algolia/{job_id}/cancel",
    acknowledge: acknowledge_algolia_migration => "/1/migrations/algolia/{job_id}/acknowledge"
);
define_source_migration_openapi_lifecycle!(
    Meilisearch,
    submit: submit_meilisearch_migration => "/1/migrations/meilisearch",
    status: get_meilisearch_migration_status => "/1/migrations/meilisearch/{job_id}",
    cancel: cancel_meilisearch_migration => "/1/migrations/meilisearch/{job_id}/cancel",
    acknowledge: acknowledge_meilisearch_migration => "/1/migrations/meilisearch/{job_id}/acknowledge"
);
define_source_migration_openapi_lifecycle!(
    Typesense,
    submit: submit_typesense_migration => "/1/migrations/typesense",
    status: get_typesense_migration_status => "/1/migrations/typesense/{job_id}",
    cancel: cancel_typesense_migration => "/1/migrations/typesense/{job_id}/cancel",
    acknowledge: acknowledge_typesense_migration => "/1/migrations/typesense/{job_id}/acknowledge"
);

pub(crate) async fn submit_algolia_migration_http(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    source_provider: Option<Extension<AsyncMigrationSourceProvider>>,
    headers: HeaderMap,
    Json(payload): Json<MigrateFromAlgoliaRequest>,
) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError> {
    submit_algolia_migration_impl(
        source_provider
            .map(|Extension(provider)| provider)
            .unwrap_or_default(),
        state,
        authenticated_owner_identity(authenticated_app_id, &headers),
        payload,
        algolia_source_reader,
    )
    .await
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
    submit_source_migration_with_test_source_factory(
        AsyncMigrationSourceProvider::Algolia,
        State(state),
        Extension(AuthenticatedAppId(authenticated_app_id)),
        Json(payload),
        source_factory,
    )
    .await
}

#[cfg(test)]
async fn submit_source_migration_with_test_source_factory<F, R>(
    source_provider: AsyncMigrationSourceProvider,
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    Json(payload): Json<MigrateFromAlgoliaRequest>,
    source_factory: F,
) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError>
where
    F: FnOnce(&MigrateFromAlgoliaRequest) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send + 'static,
{
    submit_algolia_migration_impl(
        source_provider,
        state,
        authenticated_app_id,
        payload,
        source_factory,
    )
    .await
}

async fn submit_algolia_migration_impl<F, R>(
    source_provider: AsyncMigrationSourceProvider,
    state: Arc<AppState>,
    authenticated_app_id: String,
    payload: MigrateFromAlgoliaRequest,
    source_factory: F,
) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError>
where
    F: FnOnce(&MigrateFromAlgoliaRequest) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send + 'static,
{
    ensure_source_provider_supported(source_provider)?;
    let (_job_uuid, phase_record) = state
        .migration_runner
        .submit_algolia_import_for_owner(payload, Some(authenticated_app_id), source_factory)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(AsyncMigrationStatusResponse::from(phase_record)),
    ))
}

#[utoipa::path(
    post,
    path = "/1/migrations/privacy-scrub",
    tag = "migration",
    request_body = PrivacyScrubRequest,
    responses(
        (status = 202, description = "Privacy scrub exact-absence ACK", body = PrivacyScrubAck),
        (status = 400, description = "Invalid privacy scrub request"),
        (status = 403, description = "Private migration credential required"),
        (status = 409, description = "Privacy scrub refused or retryable")
    ),
    security(("private_migration" = []))
)]
pub async fn submit_privacy_scrub(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    #[cfg(test)] hooks: Option<Extension<Arc<import::PrivacyScrubTestHooks>>>,
    Json(payload): Json<PrivacyScrubRequest>,
) -> Result<(StatusCode, Json<PrivacyScrubAck>), MigrateError> {
    #[cfg(test)]
    let hooks = hooks.map(|Extension(hooks)| hooks);
    submit_privacy_scrub_impl(
        state,
        authenticated_app_id,
        payload,
        #[cfg(test)]
        hooks,
    )
    .await
}

pub(crate) async fn submit_privacy_scrub_http(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    headers: HeaderMap,
    #[cfg(test)] hooks: Option<Extension<Arc<import::PrivacyScrubTestHooks>>>,
    Json(payload): Json<PrivacyScrubRequest>,
) -> Result<(StatusCode, Json<PrivacyScrubAck>), MigrateError> {
    #[cfg(test)]
    let hooks = hooks.map(|Extension(hooks)| hooks);
    submit_privacy_scrub_impl(
        state,
        authenticated_owner_identity(authenticated_app_id, &headers),
        payload,
        #[cfg(test)]
        hooks,
    )
    .await
}

async fn submit_privacy_scrub_impl(
    state: Arc<AppState>,
    authenticated_app_id: String,
    payload: PrivacyScrubRequest,
    #[cfg(test)] hooks: Option<Arc<import::PrivacyScrubTestHooks>>,
) -> Result<(StatusCode, Json<PrivacyScrubAck>), MigrateError> {
    validate_privacy_scrub_request(&payload)?;
    verify_privacy_scrub_generation(&state, &payload)?;
    let spool = import::spool_for_manager(&state.manager)?;
    if privacy_scrub_terminal_replay(&spool, &authenticated_app_id, &payload)? {
        #[cfg(test)]
        wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::AckReplay).await;
        return Ok(privacy_scrub_ack(&payload.scrub_id));
    }
    #[cfg(test)]
    wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::PreIntent).await;
    let requested_intent = PrivacyScrubIntent::from_fields(PrivacyScrubIntentFields {
        scrub_id: payload.scrub_id.clone(),
        tenant: payload.tenant.clone(),
        expected_generation: payload.expected_generation.clone(),
        object_ids: payload.object_ids.clone(),
        synonym_ids: payload.synonym_ids.clone(),
        rule_ids: payload.rule_ids.clone(),
        authenticated_app_id,
        created_at: chrono::Utc::now(),
    });
    let admission = spool
        .admit_privacy_scrub_intent(Uuid::new_v4(), requested_intent)
        .map_err(privacy_scrub_spool_error)?;
    let (job_uuid, phase, intent, duplicate) = match admission {
        PrivacyScrubAdmission::Created {
            job_uuid,
            phase,
            intent,
        } => (job_uuid, phase, intent, false),
        PrivacyScrubAdmission::Duplicate {
            job_uuid,
            phase,
            intent,
        } => (job_uuid, phase, intent, true),
    };

    if duplicate
        && phase.disposition == MigrationDisposition::Succeeded
        && phase.terminal_at.is_some()
    {
        #[cfg(test)]
        wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::AckReplay).await;
        return Ok(privacy_scrub_ack(&intent.scrub_id));
    }
    if duplicate {
        if settle_privacy_scrub_success_if_exact_absence(&state, &spool, job_uuid, &intent)? {
            #[cfg(test)]
            wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::AckReplay).await;
            return Ok(privacy_scrub_ack(&intent.scrub_id));
        }
        return Err(privacy_scrub_conflict(
            PRIVACY_SCRUB_INTERRUPTED_RETRYABLE_CODE,
            "Privacy scrub is interrupted and retryable",
        ));
    }

    #[cfg(test)]
    wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::PostIntent).await;
    run_privacy_scrub(&state, &spool, job_uuid, &intent).await?;
    #[cfg(test)]
    wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::EngineCommit).await;
    settle_privacy_scrub_success(&spool, job_uuid)?;
    #[cfg(test)]
    wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::PreAck).await;
    #[cfg(test)]
    wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::ResponseLoss).await;
    Ok(privacy_scrub_ack(&intent.scrub_id))
}

pub(crate) async fn get_algolia_migration_status_http(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    source_provider: Option<Extension<AsyncMigrationSourceProvider>>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<AsyncMigrationStatusResponse>, MigrateError> {
    get_source_migration_status(
        state,
        authenticated_owner_identity(authenticated_app_id, &headers),
        job_id,
        Some(
            source_provider
                .map(|Extension(provider)| provider)
                .unwrap_or(AsyncMigrationSourceProvider::Algolia),
        ),
    )
    .await
}

pub(crate) async fn cancel_algolia_migration_http(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    source_provider: Option<Extension<AsyncMigrationSourceProvider>>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<AsyncMigrationStatusResponse>, MigrateError> {
    cancel_source_migration(
        state,
        authenticated_owner_identity(authenticated_app_id, &headers),
        job_id,
        Some(
            source_provider
                .map(|Extension(provider)| provider)
                .unwrap_or(AsyncMigrationSourceProvider::Algolia),
        ),
    )
    .await
}

pub(super) async fn get_source_migration_status(
    state: Arc<AppState>,
    owner_identity: String,
    job_id: String,
    expected_source_provider: Option<AsyncMigrationSourceProvider>,
) -> Result<Json<AsyncMigrationStatusResponse>, MigrateError> {
    let (spool, job_uuid) = owned_async_migration_job(&state, &owner_identity, &job_id)?;
    let metadata = spool
        .read_async_migration_metadata(job_uuid)
        .map_err(migration_status_spool_error)?;
    ensure_expected_source_provider(&metadata, expected_source_provider)?;
    let phase_record = spool
        .read_migration_phase(job_uuid)
        .map_err(migration_status_spool_error)?;
    Ok(Json(AsyncMigrationStatusResponse::with_metadata(
        phase_record,
        &metadata,
    )))
}

pub(super) async fn cancel_source_migration(
    state: Arc<AppState>,
    owner_identity: String,
    job_id: String,
    expected_source_provider: Option<AsyncMigrationSourceProvider>,
) -> Result<Json<AsyncMigrationStatusResponse>, MigrateError> {
    let (spool, job_uuid) = owned_async_migration_job(&state, &owner_identity, &job_id)?;
    let metadata = spool
        .read_async_migration_metadata(job_uuid)
        .map_err(migration_status_spool_error)?;
    ensure_expected_source_provider(&metadata, expected_source_provider)?;
    match spool
        .request_async_migration_cancel(job_uuid)
        .map_err(migration_status_spool_error)?
    {
        MigrationCancelRequest::Requested(record) => Ok(Json(
            AsyncMigrationStatusResponse::with_metadata(record, &metadata),
        )),
        MigrationCancelRequest::TooLate(_) => Err(json_error_parts_with_code(
            StatusCode::CONFLICT,
            MIGRATION_CANCEL_TOO_LATE_CODE,
            MIGRATION_CANCEL_TOO_LATE_MESSAGE,
        )),
    }
}

pub(crate) async fn acknowledge_algolia_migration_http(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    source_provider: Option<Extension<AsyncMigrationSourceProvider>>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
) -> Result<StatusCode, MigrateError> {
    acknowledge_source_migration(
        state,
        authenticated_owner_identity(authenticated_app_id, &headers),
        job_id,
        Some(
            source_provider
                .map(|Extension(provider)| provider)
                .unwrap_or(AsyncMigrationSourceProvider::Algolia),
        ),
    )
    .await
}

async fn acknowledge_source_migration(
    state: Arc<AppState>,
    owner_identity: String,
    job_id: String,
    expected_source_provider: Option<AsyncMigrationSourceProvider>,
) -> Result<StatusCode, MigrateError> {
    let (spool, job_uuid) = owned_async_migration_job(&state, &owner_identity, &job_id)?;
    let metadata = spool
        .read_async_migration_metadata(job_uuid)
        .map_err(migration_status_spool_error)?;
    ensure_expected_source_provider(&metadata, expected_source_provider)?;
    let phase_record = spool
        .read_migration_phase(job_uuid)
        .map_err(migration_status_spool_error)?;
    if phase_record.terminal_at.is_none()
        || phase_record.disposition == MigrationDisposition::Running
    {
        return Err(json_error_parts_with_code(
            StatusCode::CONFLICT,
            MIGRATION_ACK_TOO_EARLY_CODE,
            MIGRATION_ACK_TOO_EARLY_MESSAGE,
        ));
    }
    verify_async_migration_ack_generation(&state, &spool, job_uuid, &phase_record)?;
    Ok(StatusCode::NO_CONTENT)
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
    let admitted = admit_migration_request(&state, &payload)?;
    let mut reader = source_factory(&payload).map_err(algolia_error)?;
    import::import_from_source_with_test_hooks(
        &state.manager,
        admitted.target_index,
        admitted.publication_mode,
        &mut reader,
        hooks,
    )
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
    let admitted = admit_migration_request(&state, &payload)?;
    let mut reader = source_factory(&payload).map_err(algolia_error)?;
    import::import_from_source(
        &state.manager,
        admitted.target_index,
        admitted.publication_mode,
        &mut reader,
    )
    .await
}

fn admit_migration_request(
    state: &AppState,
    payload: &MigrateFromAlgoliaRequest,
) -> Result<AdmittedMigration, MigrateError> {
    admit_migration_payload(&state.manager, state.replication_manager.as_ref(), payload)
}

fn admit_migration_payload(
    manager: &Arc<flapjack::IndexManager>,
    replication_manager: Option<&Arc<flapjack_replication::manager::ReplicationManager>>,
    payload: &MigrateFromAlgoliaRequest,
) -> Result<AdmittedMigration, MigrateError> {
    validate_migration_request(payload)?;
    let target_index = migration_target_index(payload).to_string();
    if replication_manager.is_some_and(|manager| manager.peer_count() > 0) {
        return Err(migration_ha_unsupported());
    }
    if payload.overwrite {
        let staging_baseline = manager
            .capture_replacement_staging_baseline(&target_index)
            .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
        return Ok(AdmittedMigration {
            target_index,
            publication_mode: MigrationPublicationMode::ReplaceExisting { staging_baseline },
        });
    }
    Ok(AdmittedMigration {
        target_index,
        publication_mode: MigrationPublicationMode::CreateOnly,
    })
}

pub(super) fn admit_async_migration_payload(
    manager: &Arc<flapjack::IndexManager>,
    replication_manager: Option<&Arc<flapjack_replication::manager::ReplicationManager>>,
    payload: &MigrateFromAlgoliaRequest,
) -> Result<AdmittedMigration, MigrateError> {
    admit_migration_payload(manager, replication_manager, payload)
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

fn ensure_source_provider_supported(
    source_provider: AsyncMigrationSourceProvider,
) -> Result<(), MigrateError> {
    if source_provider.is_algolia() {
        Ok(())
    } else {
        Err(source_provider_unsupported())
    }
}

fn source_provider_unsupported() -> MigrateError {
    json_error_parts_with_code(
        StatusCode::BAD_REQUEST,
        SOURCE_PROVIDER_UNSUPPORTED_CODE,
        SOURCE_PROVIDER_UNSUPPORTED_MESSAGE,
    )
}

fn migration_cancelled_error() -> MigrateError {
    json_error_parts_with_code(
        StatusCode::CONFLICT,
        MIGRATION_CANCELLED_CODE,
        MIGRATION_CANCELLED_MESSAGE,
    )
}

fn spool_error(error: SpoolError) -> MigrateError {
    let status = match error.kind() {
        SpoolErrorKind::JobNotFound
        | SpoolErrorKind::PublicHandleNotFound
        | SpoolErrorKind::CheckpointHandleNotFound => StatusCode::NOT_FOUND,
        SpoolErrorKind::CompressedPageBytesExceeded
        | SpoolErrorKind::DecompressedPageBytesExceeded
        | SpoolErrorKind::ResourceItemCountExceeded
        | SpoolErrorKind::JobBytesExceeded
        | SpoolErrorKind::GlobalBytesExceeded
        | SpoolErrorKind::FreeSpaceFloor
        | SpoolErrorKind::StagedArtifactCountExceeded
        | SpoolErrorKind::StagedArtifactBytesExceeded
        | SpoolErrorKind::InvalidRelativePath
        | SpoolErrorKind::InvalidSourceIdentityDigest
        | SpoolErrorKind::InvalidCompletedResourceId
        | SpoolErrorKind::SourceIdentityMismatch
        | SpoolErrorKind::ResourceVerificationFailed
        | SpoolErrorKind::ResourceComplete
        | SpoolErrorKind::ResourcesIncomplete
        | SpoolErrorKind::CancelRequested
        | SpoolErrorKind::JobTerminal
        | SpoolErrorKind::JobNotAccepted
        | SpoolErrorKind::UnsupportedArtifactKind
        | SpoolErrorKind::InvalidPhaseTransition
        | SpoolErrorKind::PrivacyScrubIntentCollision => StatusCode::BAD_REQUEST,
        SpoolErrorKind::JobDeleting => StatusCode::CONFLICT,
        SpoolErrorKind::Io | SpoolErrorKind::ManifestCorrupt => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let message = if status == StatusCode::INTERNAL_SERVER_ERROR {
        "Internal server error".to_string()
    } else {
        error.to_string()
    };
    json_error_parts(status, message)
}

fn migration_status_spool_error(error: spool::SpoolError) -> MigrateError {
    if error.kind() == SpoolErrorKind::JobNotFound {
        return json_error_parts_with_code(
            StatusCode::NOT_FOUND,
            MIGRATION_JOB_NOT_FOUND_CODE,
            MIGRATION_JOB_NOT_FOUND_MESSAGE,
        );
    }
    spool_error(error)
}

fn ensure_async_migration_owner(
    spool: &spool::SpoolStore,
    job_uuid: Uuid,
    owner_identity: &str,
) -> Result<(), MigrateError> {
    let metadata = spool
        .read_async_migration_metadata(job_uuid)
        .map_err(migration_status_spool_error)?;
    if metadata.authenticated_app_id.as_deref() == Some(owner_identity) {
        return Ok(());
    }
    Err(migration_job_not_found())
}

fn ensure_expected_source_provider(
    metadata: &spool::AsyncMigrationMetadata,
    expected_source_provider: Option<AsyncMigrationSourceProvider>,
) -> Result<(), MigrateError> {
    let Some(expected) = expected_source_provider else {
        return Ok(());
    };
    if metadata.source_provider == expected
        && metadata.operation_kind == spool::AsyncMigrationOperationKind::SourceImport
    {
        return Ok(());
    }
    Err(migration_job_not_found())
}

fn migration_job_not_found() -> MigrateError {
    json_error_parts_with_code(
        StatusCode::NOT_FOUND,
        MIGRATION_JOB_NOT_FOUND_CODE,
        MIGRATION_JOB_NOT_FOUND_MESSAGE,
    )
}

fn owned_async_migration_job(
    state: &AppState,
    owner_identity: &str,
    job_id: &str,
) -> Result<(spool::SpoolStore, Uuid), MigrateError> {
    let job_uuid = Uuid::parse_str(job_id)
        .map_err(|_| json_error_parts(StatusCode::BAD_REQUEST, "job_id must be a valid UUID"))?;
    let spool = import::spool_for_manager(&state.manager)?;
    ensure_async_migration_owner(&spool, job_uuid, owner_identity)?;
    Ok((spool, job_uuid))
}

fn verify_async_migration_ack_generation(
    state: &AppState,
    spool: &spool::SpoolStore,
    job_uuid: Uuid,
    phase_record: &MigrationPhaseRecord,
) -> Result<(), MigrateError> {
    if phase_record.disposition != MigrationDisposition::Succeeded {
        return Ok(());
    }
    let metadata = spool
        .read_async_migration_metadata(job_uuid)
        .map_err(migration_status_spool_error)?;
    if metadata.publication_transaction_id.is_none() {
        return Ok(());
    }
    let target = PublicationTarget::new(metadata.target_index)
        .map_err(|_| stale_async_migration_ack_generation())?;
    let Some(expected_generation) = metadata.expected_publication_generation else {
        return Err(stale_async_migration_ack_generation());
    };
    verify_current_generation_evidence(&state.manager.base_path, &target, &expected_generation)
        .map_err(|_| stale_async_migration_ack_generation())
}

fn stale_async_migration_ack_generation() -> MigrateError {
    json_error_parts_with_code(
        StatusCode::CONFLICT,
        MIGRATION_ACK_STALE_GENERATION_CODE,
        MIGRATION_ACK_STALE_GENERATION_MESSAGE,
    )
}

fn validate_privacy_scrub_request(payload: &PrivacyScrubRequest) -> Result<(), MigrateError> {
    validate_component("scrubId", &payload.scrub_id)?;
    validate_index_name(&payload.tenant)
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
    PublicationGenerationEvidence::new(payload.expected_generation.clone())
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
    for (label, values) in [
        ("objectIDs", &payload.object_ids),
        ("synonymIDs", &payload.synonym_ids),
        ("ruleIDs", &payload.rule_ids),
    ] {
        for value in values {
            validate_component(label, value)?;
        }
    }
    Ok(())
}

fn validate_component(label: &str, value: &str) -> Result<(), MigrateError> {
    if value.trim().is_empty()
        || value != value.trim()
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(json_error_parts(
            StatusCode::BAD_REQUEST,
            format!("{label} must contain stable non-empty IDs"),
        ));
    }
    Ok(())
}

fn authenticated_owner_identity(authenticated_app_id: String, headers: &HeaderMap) -> String {
    let Some(api_key) = headers
        .get("x-algolia-api-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return authenticated_app_id;
    };

    format!(
        "{authenticated_app_id}:{}",
        hex::encode(Sha256::digest(api_key.as_bytes()))
    )
}

fn verify_privacy_scrub_generation(
    state: &AppState,
    payload: &PrivacyScrubRequest,
) -> Result<(), MigrateError> {
    if !state.manager.base_path.join(&payload.tenant).exists() {
        return Err(privacy_scrub_conflict(
            PRIVACY_SCRUB_UNKNOWN_TARGET_CODE,
            "Privacy scrub target is unknown",
        ));
    }
    let target = PublicationTarget::new(payload.tenant.clone())
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
    let expected = PublicationGenerationEvidence::new(payload.expected_generation.clone())
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
    verify_current_generation_evidence(&state.manager.base_path, &target, &expected).map_err(|_| {
        privacy_scrub_conflict(
            PRIVACY_SCRUB_STALE_GENERATION_CODE,
            "Privacy scrub generation evidence is stale or unavailable",
        )
    })
}

fn privacy_scrub_terminal_replay(
    spool: &spool::SpoolStore,
    authenticated_app_id: &str,
    payload: &PrivacyScrubRequest,
) -> Result<bool, MigrateError> {
    for job_uuid in spool.job_uuids().map_err(privacy_scrub_spool_error)? {
        let Some(intent) = spool
            .read_privacy_scrub_intent_if_exists(job_uuid)
            .map_err(privacy_scrub_spool_error)?
        else {
            continue;
        };
        if !privacy_scrub_payload_matches_intent(&intent, authenticated_app_id, payload) {
            continue;
        }
        let phase = spool
            .read_migration_phase(job_uuid)
            .map_err(privacy_scrub_spool_error)?;
        return Ok(
            phase.disposition == MigrationDisposition::Succeeded && phase.terminal_at.is_some()
        );
    }
    Ok(false)
}

fn privacy_scrub_payload_matches_intent(
    intent: &PrivacyScrubIntent,
    authenticated_app_id: &str,
    payload: &PrivacyScrubRequest,
) -> bool {
    intent.authenticated_app_id == authenticated_app_id
        && intent.scrub_id == payload.scrub_id
        && intent.tenant == payload.tenant
        && intent.expected_generation == payload.expected_generation
        && intent.object_ids == payload.object_ids
        && intent.synonym_ids == payload.synonym_ids
        && intent.rule_ids == payload.rule_ids
}

fn settle_privacy_scrub_success_if_exact_absence(
    state: &Arc<AppState>,
    spool: &spool::SpoolStore,
    job_uuid: Uuid,
    intent: &PrivacyScrubIntent,
) -> Result<bool, MigrateError> {
    if assert_privacy_scrub_absence(state, intent).is_err() {
        return Ok(false);
    }
    settle_privacy_scrub_success(spool, job_uuid)?;
    Ok(true)
}

fn settle_privacy_scrub_success(
    spool: &spool::SpoolStore,
    job_uuid: Uuid,
) -> Result<(), MigrateError> {
    let phase = spool
        .read_migration_phase(job_uuid)
        .map_err(privacy_scrub_spool_error)?;
    if phase.disposition == MigrationDisposition::Succeeded && phase.terminal_at.is_some() {
        return Ok(());
    }
    if phase.disposition != MigrationDisposition::Running || phase.terminal_at.is_some() {
        return Err(privacy_scrub_conflict(
            PRIVACY_SCRUB_INTERRUPTED_RETRYABLE_CODE,
            "Privacy scrub is interrupted and retryable",
        ));
    }
    for next_phase in privacy_scrub_remaining_phases(phase.phase) {
        spool
            .transition_migration_phase(job_uuid, *next_phase)
            .map_err(privacy_scrub_spool_error)?;
    }
    spool
        .succeed_migration(job_uuid, None)
        .map_err(privacy_scrub_spool_error)?;
    Ok(())
}

fn privacy_scrub_remaining_phases(phase: MigrationPhase) -> &'static [MigrationPhase] {
    match phase {
        MigrationPhase::Submitted => &[
            MigrationPhase::Exporting,
            MigrationPhase::Preparing,
            MigrationPhase::Staging,
            MigrationPhase::Activating,
        ],
        MigrationPhase::Exporting => &[
            MigrationPhase::Preparing,
            MigrationPhase::Staging,
            MigrationPhase::Activating,
        ],
        MigrationPhase::Preparing => &[MigrationPhase::Staging, MigrationPhase::Activating],
        MigrationPhase::Staging => &[MigrationPhase::Activating],
        MigrationPhase::Activating => &[],
    }
}

async fn run_privacy_scrub(
    state: &Arc<AppState>,
    spool: &spool::SpoolStore,
    job_uuid: Uuid,
    intent: &PrivacyScrubIntent,
) -> Result<(), MigrateError> {
    if !intent.object_ids.is_empty() {
        state
            .manager
            .delete_documents_durable(&intent.tenant, intent.object_ids.clone())
            .await
            .map_err(|error| {
                json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
            })?;
    }
    for synonym_id in &intent.synonym_ids {
        delete_resource_item::<SynonymStore>(state.manager.as_ref(), &intent.tenant, synonym_id)
            .map_err(|error| {
                json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
            })?;
        state.manager.append_oplog(
            &intent.tenant,
            "delete_synonym",
            serde_json::json!({"objectID": synonym_id}),
        );
    }
    for rule_id in &intent.rule_ids {
        delete_resource_item::<RuleStore>(state.manager.as_ref(), &intent.tenant, rule_id)
            .map_err(|error| {
                json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
            })?;
        state.manager.append_oplog(
            &intent.tenant,
            "delete_rule",
            serde_json::json!({"objectID": rule_id}),
        );
    }
    assert_privacy_scrub_absence(state, intent).map_err(|error| {
        let _ = spool.fail_migration(job_uuid);
        json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    })
}

fn assert_privacy_scrub_absence(
    state: &Arc<AppState>,
    intent: &PrivacyScrubIntent,
) -> Result<(), flapjack::error::FlapjackError> {
    for object_id in &intent.object_ids {
        if state
            .manager
            .get_document(&intent.tenant, object_id)?
            .is_some()
        {
            return Err(flapjack::error::FlapjackError::InvalidQuery(format!(
                "privacy scrub object {object_id} is still present"
            )));
        }
    }
    let synonyms = load_existing_store::<SynonymStore>(state.manager.as_ref(), &intent.tenant)?;
    for synonym_id in &intent.synonym_ids {
        if synonyms
            .as_ref()
            .and_then(|store| store.get(synonym_id))
            .is_some()
        {
            return Err(flapjack::error::FlapjackError::InvalidQuery(format!(
                "privacy scrub synonym {synonym_id} is still present"
            )));
        }
    }
    let rules = load_existing_store::<RuleStore>(state.manager.as_ref(), &intent.tenant)?;
    for rule_id in &intent.rule_ids {
        if rules
            .as_ref()
            .and_then(|store| store.get(rule_id))
            .is_some()
        {
            return Err(flapjack::error::FlapjackError::InvalidQuery(format!(
                "privacy scrub rule {rule_id} is still present"
            )));
        }
    }
    Ok(())
}

fn privacy_scrub_ack(scrub_id: &str) -> (StatusCode, Json<PrivacyScrubAck>) {
    (
        StatusCode::ACCEPTED,
        Json(PrivacyScrubAck {
            scrub_id: scrub_id.to_string(),
            disposition: "acknowledged".to_string(),
        }),
    )
}

fn privacy_scrub_conflict(code: &'static str, message: &'static str) -> MigrateError {
    json_error_parts_with_code(StatusCode::CONFLICT, code, message)
}

fn privacy_scrub_spool_error(error: spool::SpoolError) -> MigrateError {
    if error.kind() == SpoolErrorKind::PrivacyScrubIntentCollision {
        return privacy_scrub_conflict(
            PRIVACY_SCRUB_MISMATCHED_INTENT_CODE,
            "Privacy scrub intent identity does not match",
        );
    }
    spool_error(error)
}

#[cfg(test)]
async fn wait_privacy_scrub_boundary(
    hooks: &Option<Arc<import::PrivacyScrubTestHooks>>,
    boundary: import::PrivacyScrubBoundary,
) {
    if let Some(hooks) = hooks {
        hooks.wait_at(boundary).await;
    }
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
#[path = "source_identity_partitions_tests.rs"]
mod source_identity_partitions_tests;

#[cfg(test)]
#[path = "import_contract_tests.rs"]
mod import_contract_tests;

#[cfg(test)]
#[path = "source_reader_tests.rs"]
mod source_reader_tests;
