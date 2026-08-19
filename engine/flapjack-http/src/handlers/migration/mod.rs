//! Stub summary for engine/flapjack-http/src/handlers/migration/mod.rs.
use axum::{
    body::Bytes,
    extract::{Extension, Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use flapjack::index::manager::publication::{
    verify_current_generation_evidence, PublicationStagingBaseline, PublicationTarget,
};
use flapjack::validate_index_name;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

#[allow(dead_code)]
mod algolia_client;
#[cfg(test)]
pub(crate) use algolia_client::{with_test_algolia_base_url_override, TEST_ALGOLIA_BASE_URL_ENV};
mod algolia_source_reader;
mod bulk_build;
pub mod bulk_replace;
mod export;
mod import;
mod job_runner;
#[allow(dead_code)]
mod meilisearch_client;
mod meilisearch_source_reader;
mod meilisearch_synonyms;
#[cfg(test)]
mod preview_tests;
pub mod privacy_scrub;
mod source_identity_partitions;
mod source_reader;
mod source_snapshot;
#[cfg(test)]
mod source_test_support;
pub(crate) mod spool;
mod translation;
mod typesense_client;
#[cfg(test)]
mod typesense_client_tests;
#[cfg(test)]
mod typesense_contract_tests;
#[cfg(test)]
mod typesense_field_validation_tests;
mod typesense_source_reader;

use super::AppState;
use crate::auth::AuthenticatedAppId;
use crate::error_response::{json_error_parts, json_error_parts_with_code};
use algolia_client::{AlgoliaClient, AlgoliaClientError, AlgoliaErrorKind, AlgoliaIndexRecord};
pub use bulk_replace::{
    cancel_bulk_replace_http, get_bulk_replace_status_http, submit_bulk_replace_http,
    BulkReplaceReceipt,
};
pub use job_runner::{MigrationJobRunner, DEFAULT_ASYNC_MIGRATION_CAPACITY};
pub(crate) use privacy_scrub::submit_privacy_scrub_http;
pub use privacy_scrub::{submit_privacy_scrub, PrivacyScrubAck, PrivacyScrubRequest};
use spool::{
    MigrationCancelRequest, MigrationDisposition, MigrationExportProgress, MigrationImportWarning,
    MigrationPhase, MigrationPhaseRecord, SpoolError, SpoolErrorKind,
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
const SOURCE_PROVIDER_PAYLOAD_MISMATCH_CODE: &str = "source_provider_payload_mismatch";
pub(super) const TYPESENSE_WRITE_FREEZE_REQUIRED_MESSAGE: &str = "Public export exposes no stable capture marker. Require an external write freeze/attestation and refuse capture when it cannot be established; pre/post count and creation time are only diagnostics.";
/// Upstream Meilisearch error code for a key that lacks the requested action.
const MEILISEARCH_INVALID_API_KEY_CODE: &str = "invalid_api_key";
const MIGRATION_ACK_TOO_EARLY_CODE: &str = "migration_ack_too_early";
const MIGRATION_ACK_TOO_EARLY_MESSAGE: &str =
    "Migration job must be terminal before it can be acknowledged";
const MIGRATION_ACK_STALE_GENERATION_CODE: &str = "migration_ack_stale_generation";
const MIGRATION_ACK_STALE_GENERATION_MESSAGE: &str =
    "Migration publication generation evidence is stale or unavailable";
const MIGRATION_RESUME_CLAIM_CONFLICT_CODE: &str = "migration_resume_claim_conflict";
const MIGRATION_RESUME_CLAIM_CONFLICT_MESSAGE: &str = "Migration resume was already claimed";
const MIGRATION_RESUME_NOT_AVAILABLE_CODE: &str = "migration_resume_not_available";
const MIGRATION_RESUME_NOT_AVAILABLE_MESSAGE: &str = "Migration resume is not available";

/// Request payload for migrating an index from Algolia to Flapjack.
///
/// Contains Algolia credentials, the source index name, and optional target
/// index settings. HA imports are refused before import admission. Standalone
/// synchronous requests create a fresh target by default; `overwrite=true`
/// replaces an existing target through the node-local fenced publication path.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
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

/// Request payload for migrating an index from Meilisearch Cloud to Flapjack.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrateFromMeilisearchRequest {
    pub endpoint: String,

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

/// Request payload for migrating a Typesense Cloud collection to Flapjack.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrateFromTypesenseRequest {
    pub node: String,

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

    #[serde(rename = "sourceWriteFrozen", default)]
    pub source_write_frozen: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationPreviewResponse {
    report: MigrationPreviewReport,
    source_counts: MigrationPreviewSourceCounts,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MigrationPreviewSourceCounts {
    indexes: usize,
    records: usize,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationPreviewReport {
    entries: Vec<MigrationPreviewReportEntry>,
    summary: MigrationPreviewReportSummary,
    report_digest: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationPreviewReportSummary {
    total_entries: usize,
    hard_rejections: usize,
    warnings: usize,
    scope_gaps: usize,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationPreviewReportEntry {
    severity: translation::ReportSeverity,
    code: translation::ReportCode,
    resource: translation::ReportResource,
    page_index: Option<usize>,
    item_index: Option<usize>,
    json_path: String,
}

impl From<translation::TranslationReport> for MigrationPreviewReport {
    fn from(report: translation::TranslationReport) -> Self {
        Self {
            entries: report.entries.into_iter().map(Into::into).collect(),
            summary: report.summary.into(),
            report_digest: report.report_digest,
        }
    }
}

impl From<translation::TranslationReportSummary> for MigrationPreviewReportSummary {
    fn from(summary: translation::TranslationReportSummary) -> Self {
        Self {
            total_entries: summary.total_entries,
            hard_rejections: summary.hard_rejections,
            warnings: summary.warnings,
            scope_gaps: summary.scope_gaps,
        }
    }
}

impl From<translation::TranslationReportEntry> for MigrationPreviewReportEntry {
    fn from(entry: translation::TranslationReportEntry) -> Self {
        Self {
            severity: entry.severity,
            code: entry.code,
            resource: entry.resource,
            page_index: entry.page_index,
            item_index: entry.item_index,
            json_path: entry.json_path,
        }
    }
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

    fn is_supported_source(&self) -> bool {
        matches!(self, Self::Algolia | Self::Meilisearch | Self::Typesense)
    }

    fn supports_preview(&self) -> bool {
        matches!(self, Self::Algolia | Self::Meilisearch | Self::Typesense)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resumable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(rename = "resumeHandle", skip_serializing_if = "Option::is_none")]
    pub resume_handle: Option<String>,
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
            resumable: None,
            operation: None,
            resume_handle: None,
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

    fn with_metadata_and_resume_handle(
        record: MigrationPhaseRecord,
        metadata: &spool::AsyncMigrationMetadata,
        resume_handle: Option<String>,
    ) -> Self {
        let mut response = Self::with_metadata(record, metadata);
        if let Some(handle) = resume_handle {
            response.resumable = Some(true);
            response.operation = Some("resume".to_string());
            response.resume_handle = Some(handle);
        }
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
    let indexes = fetch_algolia_index_records(&payload.app_id, &payload.api_key)
        .await?
        .into_iter()
        .map(|index| AlgoliaIndexInfo {
            name: index.name,
            entries: index.entries,
            updated_at: index.updated_at,
        })
        .collect();

    Ok(Json(ListAlgoliaIndexesResponse { indexes }))
}

/// Read the Algolia application's index listing for both the legacy
/// Algolia-shaped route and the provider-neutral discovery route, so credential
/// admission and upstream error mapping have a single owner.
async fn fetch_algolia_index_records(
    app_id: &str,
    api_key: &str,
) -> Result<Vec<AlgoliaIndexRecord>, MigrateError> {
    if app_id.is_empty() || api_key.is_empty() {
        return Err(json_error_parts(
            StatusCode::BAD_REQUEST,
            "appId and apiKey are required",
        ));
    }
    let client = AlgoliaClient::new(app_id, api_key).map_err(algolia_error)?;
    client.list_indexes().await.map_err(algolia_error)
}

// ── Provider-neutral source discovery ───────────────────────────────────

/// Discovery request body for a Meilisearch source. The Algolia arm reuses
/// [`ListAlgoliaIndexesRequest`] rather than owning a second `{appId, apiKey}`
/// schema.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ListMeilisearchIndexesRequest {
    pub endpoint: String,

    #[serde(rename = "apiKey")]
    pub api_key: String,
}

/// Discovery request body for a Typesense source.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ListTypesenseIndexesRequest {
    pub node: String,

    #[serde(rename = "apiKey")]
    pub api_key: String,
}

/// Caller-supplied discovery window, forwarded to the source provider verbatim.
/// Omitted bounds are not sent, so the upstream's own default window applies.
#[derive(Debug, Clone, Copy, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SourceIndexPageQuery {
    pub offset: Option<u64>,
    pub limit: Option<u64>,
}

/// A provider-native source creation timestamp.
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum SourceIndexCreatedAt {
    Rfc3339(String),
    UnixSeconds(u64),
}

/// One source index in the provider-neutral discovery response.
///
/// Every metadata field is always serialized — `null` where the provider does
/// not publish it — so clients see one stable shape across providers instead of
/// a per-provider key set.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceIndexSummary {
    pub name: String,
    #[schema(required)]
    pub primary_key: Option<String>,
    #[schema(required)]
    pub entries: Option<u64>,
    #[schema(required)]
    pub document_count: Option<u64>,
    /// Providers publish creation time in incompatible units (RFC 3339 for
    /// Meilisearch, epoch seconds for Typesense), so the upstream value is
    /// preserved rather than coerced into one of them.
    #[schema(required)]
    pub created_at: Option<SourceIndexCreatedAt>,
    #[schema(required)]
    pub updated_at: Option<String>,
    #[schema(required)]
    pub default_sorting_field: Option<String>,
}

impl SourceIndexSummary {
    fn named(name: String) -> Self {
        Self {
            name,
            primary_key: None,
            entries: None,
            document_count: None,
            created_at: None,
            updated_at: None,
            default_sorting_field: None,
        }
    }
}

/// Provider-neutral source discovery response. Pagination fields are present
/// only for providers that report their own window.
#[derive(Debug, Serialize, ToSchema)]
pub struct ListSourceIndexesResponse {
    pub indexes: Vec<SourceIndexSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

impl ListSourceIndexesResponse {
    fn unpaginated(indexes: Vec<SourceIndexSummary>) -> Self {
        Self {
            indexes,
            total: None,
            offset: None,
            limit: None,
        }
    }
}

type MigrateError = (StatusCode, Json<serde_json::Value>);

#[cfg(test)]
type TestMigrationSourceReaderBuilder = dyn Fn(
        AsyncMigrationSourceProvider,
    ) -> Result<Box<dyn source_reader::MigrationSourceReader + Send>, AlgoliaClientError>
    + Send
    + Sync;

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestMigrationSourceReaderFactory {
    build: Arc<TestMigrationSourceReaderBuilder>,
}

#[cfg(test)]
impl TestMigrationSourceReaderFactory {
    fn new(
        build: impl Fn(
                AsyncMigrationSourceProvider,
            )
                -> Result<Box<dyn source_reader::MigrationSourceReader + Send>, AlgoliaClientError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            build: Arc::new(build),
        }
    }

    fn build(
        &self,
        source_provider: AsyncMigrationSourceProvider,
    ) -> Result<Box<dyn source_reader::MigrationSourceReader + Send>, AlgoliaClientError> {
        (self.build)(source_provider)
    }
}

fn parse_submit_payload<P>(body: &[u8]) -> Result<P, MigrateError>
where
    P: for<'de> Deserialize<'de>,
{
    serde_json::from_slice(body)
        .map_err(|_| json_error_parts(StatusCode::BAD_REQUEST, "Invalid migration request body"))
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

pub(super) trait AsyncMigrationSubmitPayload {
    fn admit_async(
        &self,
        manager: &Arc<flapjack::IndexManager>,
        replication_manager: Option<&Arc<flapjack_replication::manager::ReplicationManager>>,
    ) -> Result<AdmittedMigration, MigrateError>;
}

trait MigrationPreviewPayload {
    fn validate_preview(&self) -> Result<(), MigrateError>;
    fn source_index(&self) -> &str;
    fn target_index(&self) -> &str;

    fn preview_settings_override(
        &self,
    ) -> Option<source_reader::SourceFuture<'_, serde_json::Value>> {
        None
    }
}

impl MigrationPreviewPayload for MigrateFromAlgoliaRequest {
    fn validate_preview(&self) -> Result<(), MigrateError> {
        validate_migration_request(self)
    }

    fn source_index(&self) -> &str {
        &self.source_index
    }

    fn target_index(&self) -> &str {
        migration_target_index(self)
    }
}

impl MigrationPreviewPayload for MigrateFromMeilisearchRequest {
    fn validate_preview(&self) -> Result<(), MigrateError> {
        validate_meilisearch_migration_request(self)
    }

    fn source_index(&self) -> &str {
        &self.source_index
    }

    fn target_index(&self) -> &str {
        meilisearch_target_index(self)
    }

    fn preview_settings_override(
        &self,
    ) -> Option<source_reader::SourceFuture<'_, serde_json::Value>> {
        Some(Box::pin(async move {
            let client =
                preview_meilisearch_client(self).map_err(source_reader::SourceExportError::from)?;
            client
                .read_source_settings()
                .await
                .map_err(map_preview_meilisearch_error)
                .map_err(Into::into)
        }))
    }
}

impl MigrationPreviewPayload for MigrateFromTypesenseRequest {
    fn validate_preview(&self) -> Result<(), MigrateError> {
        validate_typesense_migration_request(self)
    }

    fn source_index(&self) -> &str {
        &self.source_index
    }

    fn target_index(&self) -> &str {
        typesense_target_index(self)
    }

    // TypesenseSourceReader already captures settings_from_collection for the
    // source collection, so the default preview settings path is authoritative.
}

impl AsyncMigrationSubmitPayload for MigrateFromAlgoliaRequest {
    fn admit_async(
        &self,
        manager: &Arc<flapjack::IndexManager>,
        replication_manager: Option<&Arc<flapjack_replication::manager::ReplicationManager>>,
    ) -> Result<AdmittedMigration, MigrateError> {
        admit_migration_payload(manager, replication_manager, self)
    }
}

impl AsyncMigrationSubmitPayload for MigrateFromMeilisearchRequest {
    fn admit_async(
        &self,
        manager: &Arc<flapjack::IndexManager>,
        replication_manager: Option<&Arc<flapjack_replication::manager::ReplicationManager>>,
    ) -> Result<AdmittedMigration, MigrateError> {
        admit_meilisearch_migration_payload(manager, replication_manager, self)
    }
}

impl AsyncMigrationSubmitPayload for MigrateFromTypesenseRequest {
    fn admit_async(
        &self,
        manager: &Arc<flapjack::IndexManager>,
        replication_manager: Option<&Arc<flapjack_replication::manager::ReplicationManager>>,
    ) -> Result<AdmittedMigration, MigrateError> {
        admit_typesense_migration_payload(manager, replication_manager, self)
    }
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
        preview_request: $preview_request_ty:ty,
        preview_source_reader: $preview_source_reader:path,
        request: $request_ty:ty,
        source_reader: $source_reader:path,
        preview: $preview_fn:ident => $preview_path:literal,
        submit: $submit_fn:ident => $submit_path:literal,
        status: $status_fn:ident => $status_path:literal,
        cancel: $cancel_fn:ident => $cancel_path:literal,
        acknowledge: $acknowledge_fn:ident => $acknowledge_path:literal,
        resume: $resume_fn:ident => $resume_path:literal,
        list_indexes_request: $list_indexes_request_ty:ty,
        list_indexes: $list_indexes_fn:ident => $list_indexes_path:literal
    ) => {
        /// Preview source migration translation without admitting or publishing a job.
        #[utoipa::path(
            post,
            path = $preview_path,
            tag = "migration",
            request_body = $preview_request_ty,
            responses(
                (status = 200, description = "Advisory source migration translation report", body = MigrationPreviewResponse),
                (status = 400, description = "Invalid migration request or unsupported source provider"),
                (status = 502, description = "Upstream source provider request failed")
            ),
            security(("api_key" = []))
        )]
        pub async fn $preview_fn(
            Json(payload): Json<$preview_request_ty>,
        ) -> Result<Json<MigrationPreviewResponse>, MigrateError> {
            preview_source_migration(
                AsyncMigrationSourceProvider::$provider,
                payload,
                $preview_source_reader,
            )
            .await
        }

        /// Submit an asynchronous source migration.
        #[utoipa::path(
            post,
            path = $submit_path,
            tag = "migration",
            request_body = $request_ty,
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
            Json(payload): Json<$request_ty>,
        ) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError> {
            submit_source_migration_impl(
                AsyncMigrationSourceProvider::$provider,
                state,
                authenticated_app_id,
                payload,
                $source_reader,
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

        /// Resume an interrupted asynchronous Algolia source migration.
        #[utoipa::path(
            post,
            path = $resume_path,
            tag = "migration",
            request_body = $request_ty,
            params(
                ("job_id" = Uuid, Path, description = "Migration job UUID")
            ),
            responses(
                (status = 202, description = "Async source migration resume admitted", body = AsyncMigrationStatusResponse),
                (status = 400, description = "Invalid migration job UUID, request body, or unsupported source provider"),
                (status = 404, description = "No durable migration phase record is currently retained for the UUID"),
                (status = 409, description = "migration_resume_claim_conflict or migration_resume_not_available"),
                (status = 500, description = "Migration resume claim could not be persisted")
            ),
            security(("api_key" = []))
        )]
        pub async fn $resume_fn(
            State(state): State<Arc<AppState>>,
            Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
            headers: HeaderMap,
            AxumPath(job_id): AxumPath<String>,
            Json(payload): Json<MigrateFromAlgoliaRequest>,
        ) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError> {
            resume_source_migration(
                state,
                authenticated_owner_identity(authenticated_app_id, &headers),
                AsyncMigrationSourceProvider::$provider,
                job_id,
                payload,
                algolia_source_reader,
            )
            .await
        }

        /// List the source indexes reachable with the supplied source credentials.
        ///
        /// Documentation only: this generated function is never mounted.
        /// `register_source_migration_routes` serves every provider's
        /// `list-indexes` route through `list_source_indexes_http`, and this
        /// wrapper delegates to that same handler so the published operation
        /// cannot drift from the served one.
        #[utoipa::path(
            post,
            path = $list_indexes_path,
            tag = "migration",
            request_body = $list_indexes_request_ty,
            params(SourceIndexPageQuery),
            responses(
                (status = 200, description = "Source indexes reachable with the supplied credentials", body = ListSourceIndexesResponse),
                (status = 400, description = "Invalid discovery request, source_provider_payload_mismatch, or refused source endpoint"),
                (status = 403, description = "invalid_api_key: source credentials lack index-listing access"),
                (status = 502, description = "Upstream source provider request failed")
            ),
            security(("api_key" = []))
        )]
        pub async fn $list_indexes_fn(
            page: Query<SourceIndexPageQuery>,
            body: Bytes,
        ) -> Result<Json<ListSourceIndexesResponse>, MigrateError> {
            list_source_indexes_http(
                Some(Extension(AsyncMigrationSourceProvider::$provider)),
                page,
                body,
            )
            .await
        }
    };
}

define_source_migration_openapi_lifecycle!(
    Algolia,
    preview_request: MigrateFromAlgoliaRequest,
    preview_source_reader: algolia_source_reader,
    request: MigrateFromAlgoliaRequest,
    source_reader: algolia_source_reader,
    preview: preview_algolia_migration => "/1/migrations/algolia/preview",
    submit: submit_algolia_migration => "/1/migrations/algolia",
    status: get_algolia_migration_status => "/1/migrations/algolia/{job_id}",
    cancel: cancel_algolia_migration => "/1/migrations/algolia/{job_id}/cancel",
    acknowledge: acknowledge_algolia_migration => "/1/migrations/algolia/{job_id}/acknowledge",
    resume: resume_algolia_migration => "/1/migrations/algolia/{job_id}/resume",
    list_indexes_request: ListAlgoliaIndexesRequest,
    list_indexes: list_algolia_source_indexes_doc => "/1/migrations/algolia/list-indexes"
);
define_source_migration_openapi_lifecycle!(
    Meilisearch,
    preview_request: MigrateFromMeilisearchRequest,
    preview_source_reader: preview_meilisearch_source_reader,
    request: MigrateFromMeilisearchRequest,
    source_reader: meilisearch_source_reader,
    preview: preview_meilisearch_migration => "/1/migrations/meilisearch/preview",
    submit: submit_meilisearch_migration => "/1/migrations/meilisearch",
    status: get_meilisearch_migration_status => "/1/migrations/meilisearch/{job_id}",
    cancel: cancel_meilisearch_migration => "/1/migrations/meilisearch/{job_id}/cancel",
    acknowledge: acknowledge_meilisearch_migration => "/1/migrations/meilisearch/{job_id}/acknowledge",
    resume: resume_meilisearch_migration => "/1/migrations/meilisearch/{job_id}/resume",
    list_indexes_request: ListMeilisearchIndexesRequest,
    list_indexes: list_meilisearch_source_indexes_doc => "/1/migrations/meilisearch/list-indexes"
);
define_source_migration_openapi_lifecycle!(
    Typesense,
    preview_request: MigrateFromTypesenseRequest,
    preview_source_reader: preview_typesense_source_reader,
    request: MigrateFromTypesenseRequest,
    source_reader: typesense_source_reader,
    preview: preview_typesense_migration => "/1/migrations/typesense/preview",
    submit: submit_typesense_migration => "/1/migrations/typesense",
    status: get_typesense_migration_status => "/1/migrations/typesense/{job_id}",
    cancel: cancel_typesense_migration => "/1/migrations/typesense/{job_id}/cancel",
    acknowledge: acknowledge_typesense_migration => "/1/migrations/typesense/{job_id}/acknowledge",
    resume: resume_typesense_migration => "/1/migrations/typesense/{job_id}/resume",
    list_indexes_request: ListTypesenseIndexesRequest,
    list_indexes: list_typesense_source_indexes_doc => "/1/migrations/typesense/list-indexes"
);

/// Serve `POST /1/migrations/{provider}/list-indexes` for every public provider.
///
/// The provider is already known from the mounted route, so each arm parses
/// exactly one request type instead of guessing from an untagged union. Source
/// credentials are used only to build the outbound client — they are never
/// logged nor persisted.
pub(crate) async fn list_source_indexes_http(
    source_provider: Option<Extension<AsyncMigrationSourceProvider>>,
    Query(page): Query<SourceIndexPageQuery>,
    body: Bytes,
) -> Result<Json<ListSourceIndexesResponse>, MigrateError> {
    let source_provider = source_provider
        .map(|Extension(provider)| provider)
        .unwrap_or_default();
    let response = match source_provider {
        AsyncMigrationSourceProvider::Algolia => {
            let payload: ListAlgoliaIndexesRequest =
                parse_source_discovery_payload(source_provider, &body)?;
            list_algolia_source_indexes(&payload).await?
        }
        AsyncMigrationSourceProvider::Meilisearch => {
            let payload: ListMeilisearchIndexesRequest =
                parse_source_discovery_payload(source_provider, &body)?;
            list_meilisearch_source_indexes(&payload, page).await?
        }
        AsyncMigrationSourceProvider::Typesense => {
            let payload: ListTypesenseIndexesRequest =
                parse_source_discovery_payload(source_provider, &body)?;
            list_typesense_source_indexes(&payload, page).await?
        }
    };
    Ok(Json(response))
}

/// The body field that identifies which provider a discovery payload was
/// written for.
fn source_discovery_discriminator(source_provider: AsyncMigrationSourceProvider) -> &'static str {
    match source_provider {
        AsyncMigrationSourceProvider::Algolia => "appId",
        AsyncMigrationSourceProvider::Meilisearch => "endpoint",
        AsyncMigrationSourceProvider::Typesense => "node",
    }
}

/// Parse a discovery body for the route's provider, refusing another provider's
/// discriminator first so a mislabelled payload is reported as a mismatch rather
/// than coerced into this provider's shape or rejected as generic bad JSON.
fn parse_source_discovery_payload<P>(
    source_provider: AsyncMigrationSourceProvider,
    body: &[u8],
) -> Result<P, MigrateError>
where
    P: for<'de> Deserialize<'de>,
{
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| json_error_parts(StatusCode::BAD_REQUEST, "Invalid migration request body"))?;
    for other_provider in AsyncMigrationSourceProvider::PUBLIC {
        if other_provider == source_provider {
            continue;
        }
        if value
            .get(source_discovery_discriminator(other_provider))
            .is_some()
        {
            return Err(source_provider_payload_mismatch(
                "Source discovery payload does not match source_provider",
            ));
        }
    }
    serde_json::from_value(value)
        .map_err(|_| json_error_parts(StatusCode::BAD_REQUEST, "Invalid migration request body"))
}

async fn list_algolia_source_indexes(
    payload: &ListAlgoliaIndexesRequest,
) -> Result<ListSourceIndexesResponse, MigrateError> {
    // Algolia's application-level listing has no caller-controlled window, so
    // the neutral response reports no pagination for this provider.
    let indexes = fetch_algolia_index_records(&payload.app_id, &payload.api_key)
        .await?
        .into_iter()
        .map(|index| SourceIndexSummary {
            entries: Some(index.entries),
            updated_at: Some(index.updated_at),
            ..SourceIndexSummary::named(index.name)
        })
        .collect();
    Ok(ListSourceIndexesResponse::unpaginated(indexes))
}

async fn list_meilisearch_source_indexes(
    payload: &ListMeilisearchIndexesRequest,
    page: SourceIndexPageQuery,
) -> Result<ListSourceIndexesResponse, MigrateError> {
    let client = meilisearch_discovery_client(&payload.endpoint, &payload.api_key)?;
    let listing = client
        .list_indexes(page.offset, page.limit)
        .await
        .map_err(meilisearch_error)?;
    let mut indexes = Vec::with_capacity(listing.results.len());
    for index in listing.results {
        let document_count = client
            .read_index_document_count(&index.uid)
            .await
            .map_err(meilisearch_error)?;
        indexes.push(SourceIndexSummary {
            primary_key: index.primary_key,
            document_count: Some(document_count),
            created_at: Some(SourceIndexCreatedAt::Rfc3339(index.created_at)),
            updated_at: Some(index.updated_at),
            ..SourceIndexSummary::named(index.uid)
        });
    }
    Ok(ListSourceIndexesResponse {
        indexes,
        total: Some(listing.total),
        offset: Some(listing.offset),
        limit: Some(listing.limit),
    })
}

async fn list_typesense_source_indexes(
    payload: &ListTypesenseIndexesRequest,
    page: SourceIndexPageQuery,
) -> Result<ListSourceIndexesResponse, MigrateError> {
    let client = typesense_discovery_client(&payload.node, &payload.api_key)?;
    // Typesense returns collections newest-first and reports no pagination
    // envelope, so the upstream order is the response order.
    let indexes = client
        .list_collections(page.offset, page.limit)
        .await
        .map_err(typesense_error)?
        .into_iter()
        .map(|collection| SourceIndexSummary {
            document_count: Some(collection.num_documents),
            created_at: Some(SourceIndexCreatedAt::UnixSeconds(collection.created_at)),
            default_sorting_field: collection.default_sorting_field,
            ..SourceIndexSummary::named(collection.name)
        })
        .collect();
    Ok(ListSourceIndexesResponse::unpaginated(indexes))
}

/// Admit a Meilisearch discovery endpoint through the production vendor policy,
/// falling back to the loopback seam for the live contract fixture. The seam is
/// reachable in the shipped profile only behind the existing
/// `FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=1` opt-in, matching the preview
/// admission owner [`preview_meilisearch_client`]. The production refusal is
/// what the caller sees when neither path admits the endpoint, so an
/// unrecognised host never leaks the loopback opt-in's existence.
fn meilisearch_discovery_client(
    endpoint: &str,
    api_key: &str,
) -> Result<meilisearch_client::MeilisearchClient, MigrateError> {
    let admitted = meilisearch_client::MeilisearchClient::new_discovery(endpoint, api_key);
    let admitted = admitted.or_else(|vendor_refusal| {
        meilisearch_client::MeilisearchClient::new_discovery_preview_loopback(endpoint, api_key)
            .map_err(|_| vendor_refusal)
    });
    admitted.map_err(meilisearch_error)
}

/// Typesense counterpart to [`meilisearch_discovery_client`].
fn typesense_discovery_client(
    node: &str,
    api_key: &str,
) -> Result<typesense_client::TypesenseClient, MigrateError> {
    let admitted = match typesense_client::TypesenseClient::new_discovery(node, api_key) {
        Err(vendor_refusal) if vendor_refusal.is_endpoint_not_allowed() => {
            typesense_client::TypesenseClient::new_discovery_preview_loopback(node, api_key)
                .map_err(|_| vendor_refusal)
        }
        admitted => admitted,
    };
    admitted.map_err(typesense_error)
}

pub(crate) async fn preview_algolia_migration_http(
    source_provider: Option<Extension<AsyncMigrationSourceProvider>>,
    #[cfg(test)] test_source_factory: Option<Extension<TestMigrationSourceReaderFactory>>,
    body: Bytes,
) -> Result<Json<MigrationPreviewResponse>, MigrateError> {
    let source_provider = source_provider
        .map(|Extension(provider)| provider)
        .unwrap_or_default();
    ensure_source_provider_preview_supported(source_provider)?;

    match source_provider {
        AsyncMigrationSourceProvider::Algolia => {
            let payload = parse_submit_payload(&body)?;
            #[cfg(test)]
            if let Some(Extension(factory)) = test_source_factory.as_ref() {
                return preview_source_migration_with_test_reader(source_provider, payload, |_| {
                    factory.build(source_provider)
                })
                .await;
            }
            preview_source_migration(source_provider, payload, algolia_source_reader).await
        }
        AsyncMigrationSourceProvider::Meilisearch => {
            let payload = parse_submit_payload(&body)?;
            #[cfg(test)]
            if let Some(Extension(factory)) = test_source_factory.as_ref() {
                return preview_source_migration_with_test_reader(source_provider, payload, |_| {
                    factory.build(source_provider)
                })
                .await;
            }
            preview_source_migration(source_provider, payload, preview_meilisearch_source_reader)
                .await
        }
        AsyncMigrationSourceProvider::Typesense => {
            let payload = parse_typesense_submit_payload(&body)?;
            #[cfg(test)]
            if let Some(Extension(factory)) = test_source_factory.as_ref() {
                return preview_source_migration_with_test_reader(source_provider, payload, |_| {
                    factory.build(source_provider)
                })
                .await;
            }
            preview_source_migration(source_provider, payload, preview_typesense_source_reader)
                .await
        }
    }
}

pub(crate) async fn submit_algolia_migration_http(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    source_provider: Option<Extension<AsyncMigrationSourceProvider>>,
    #[cfg(test)] test_source_factory: Option<Extension<TestMigrationSourceReaderFactory>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError> {
    let source_provider = source_provider
        .map(|Extension(provider)| provider)
        .unwrap_or_default();
    let owner = authenticated_owner_identity(authenticated_app_id, &headers);
    match source_provider {
        AsyncMigrationSourceProvider::Algolia => {
            let payload = parse_submit_payload(&body)?;
            #[cfg(test)]
            if let Some(Extension(factory)) = test_source_factory.as_ref() {
                return submit_source_migration_impl(
                    source_provider,
                    state,
                    owner,
                    payload,
                    |_: &MigrateFromAlgoliaRequest| factory.build(source_provider),
                )
                .await;
            }
            submit_source_migration_impl(
                source_provider,
                state,
                owner,
                payload,
                algolia_source_reader,
            )
            .await
        }
        AsyncMigrationSourceProvider::Meilisearch => {
            let payload = parse_submit_payload(&body)?;
            #[cfg(test)]
            if let Some(Extension(factory)) = test_source_factory.as_ref() {
                return submit_source_migration_impl(
                    source_provider,
                    state,
                    owner,
                    payload,
                    |_: &MigrateFromMeilisearchRequest| factory.build(source_provider),
                )
                .await;
            }
            submit_source_migration_impl(
                source_provider,
                state,
                owner,
                payload,
                meilisearch_source_reader,
            )
            .await
        }
        AsyncMigrationSourceProvider::Typesense => {
            let payload = parse_typesense_submit_payload(&body)?;
            #[cfg(test)]
            if let Some(Extension(factory)) = test_source_factory.as_ref() {
                return submit_source_migration_impl(
                    source_provider,
                    state,
                    owner,
                    payload,
                    |_: &MigrateFromTypesenseRequest| factory.build(source_provider),
                )
                .await;
            }
            submit_source_migration_impl(
                source_provider,
                state,
                owner,
                payload,
                typesense_source_reader,
            )
            .await
        }
    }
}

pub(crate) async fn resume_algolia_migration_http(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    source_provider: Option<Extension<AsyncMigrationSourceProvider>>,
    #[cfg(test)] test_source_factory: Option<Extension<TestMigrationSourceReaderFactory>>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError> {
    let source_provider = source_provider
        .map(|Extension(provider)| provider)
        .unwrap_or_default();
    ensure_resume_source_provider_supported(source_provider)?;
    let payload: MigrateFromAlgoliaRequest = parse_submit_payload(&body)?;
    let owner = authenticated_owner_identity(authenticated_app_id, &headers);
    #[cfg(test)]
    if let Some(Extension(factory)) = test_source_factory.as_ref() {
        return resume_source_migration(
            state,
            owner,
            source_provider,
            job_id,
            payload,
            |_: &MigrateFromAlgoliaRequest| factory.build(source_provider),
        )
        .await;
    }
    resume_source_migration(
        state,
        owner,
        source_provider,
        job_id,
        payload,
        algolia_source_reader,
    )
    .await
}

async fn resume_source_migration<F, R>(
    state: Arc<AppState>,
    owner: String,
    source_provider: AsyncMigrationSourceProvider,
    job_id: String,
    payload: MigrateFromAlgoliaRequest,
    source_factory: F,
) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError>
where
    F: FnOnce(&MigrateFromAlgoliaRequest) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send + 'static,
{
    ensure_resume_source_provider_supported(source_provider)?;
    let (job_uuid, checkpoint_handle) = resumable_algolia_job(&state, &owner, &job_id)?;
    let phase_record = state
        .migration_runner
        .resume_algolia_import(job_uuid, checkpoint_handle, payload, source_factory)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(AsyncMigrationStatusResponse::from(phase_record)),
    ))
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
    submit_source_migration_impl(
        source_provider,
        state,
        authenticated_app_id,
        payload,
        source_factory,
    )
    .await
}

#[derive(Default)]
struct PreviewSourceExport {
    settings: serde_json::Value,
    document_pages: Vec<Vec<serde_json::Value>>,
    rule_pages: Vec<Vec<serde_json::Value>>,
    rule_stable_id_pages: Vec<Vec<String>>,
    synonym_pages: Vec<Vec<serde_json::Value>>,
    synonym_stable_id_pages: Vec<Vec<String>>,
    replica_settings: std::collections::BTreeMap<String, serde_json::Value>,
}

impl PreviewSourceExport {
    fn record_count(&self) -> usize {
        self.document_pages.iter().map(Vec::len).sum()
    }

    fn into_translation_input(
        self,
        source_index_name: String,
        target_index_name: String,
        source_provider: AsyncMigrationSourceProvider,
    ) -> translation::SpoolTranslationInput {
        translation::SpoolTranslationInput {
            source_index_name,
            target_index_name,
            source_provider,
            settings: self.settings,
            document_pages: self.document_pages,
            rule_pages: self.rule_pages,
            rule_stable_id_pages: self.rule_stable_id_pages,
            synonym_pages: self.synonym_pages,
            synonym_stable_id_pages: self.synonym_stable_id_pages,
            replica_settings: self.replica_settings,
        }
    }
}

impl source_reader::SourceExportSink for PreviewSourceExport {
    fn commit_configuration(
        &mut self,
        artifact: &source_reader::SourceConfigurationArtifact,
    ) -> Result<(), source_reader::SourceExportError> {
        use source_reader::SourceConfigurationArtifact as Artifact;
        match artifact {
            Artifact::Settings { payload } => self.settings = payload.clone(),
            Artifact::Rules { records } => {
                let (page, stable_ids) = preview_configuration_page(records);
                self.rule_pages.push(page);
                self.rule_stable_id_pages.push(stable_ids);
            }
            Artifact::Synonyms { records } => {
                let (page, stable_ids) = preview_configuration_page(records);
                self.synonym_pages.push(page);
                self.synonym_stable_id_pages.push(stable_ids);
            }
            Artifact::ReplicaSettings {
                source_name,
                payload,
            } => {
                self.replica_settings
                    .insert(source_name.clone(), payload.clone());
            }
        }
        Ok(())
    }

    fn commit_document_page(
        &mut self,
        page: &[source_reader::SourceExportRecord],
    ) -> Result<(), source_reader::SourceExportError> {
        self.document_pages.push(
            page.iter()
                .map(|record| record.identity_payload())
                .collect(),
        );
        Ok(())
    }
}

fn preview_configuration_page(
    records: &[source_reader::SourceConfigurationRecord],
) -> (Vec<serde_json::Value>, Vec<String>) {
    records
        .iter()
        .map(|record| (record.identity_payload(), record.stable_id().to_string()))
        .unzip()
}

async fn preview_source_migration<P, F, R>(
    source_provider: AsyncMigrationSourceProvider,
    payload: P,
    source_factory: F,
) -> Result<Json<MigrationPreviewResponse>, MigrateError>
where
    P: MigrationPreviewPayload,
    F: FnOnce(&P) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send,
{
    preview_source_migration_inner(source_provider, payload, source_factory, true).await
}

#[cfg(test)]
async fn preview_source_migration_with_test_reader<P, F, R>(
    source_provider: AsyncMigrationSourceProvider,
    payload: P,
    source_factory: F,
) -> Result<Json<MigrationPreviewResponse>, MigrateError>
where
    P: MigrationPreviewPayload,
    F: FnOnce(&P) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send,
{
    preview_source_migration_inner(source_provider, payload, source_factory, false).await
}

async fn preview_source_migration_inner<P, F, R>(
    source_provider: AsyncMigrationSourceProvider,
    payload: P,
    source_factory: F,
    fetch_preview_settings: bool,
) -> Result<Json<MigrationPreviewResponse>, MigrateError>
where
    P: MigrationPreviewPayload,
    F: FnOnce(&P) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send,
{
    ensure_source_provider_preview_supported(source_provider)?;
    payload.validate_preview()?;
    let source_index_name = payload.source_index().to_string();
    let target_index_name = payload.target_index().to_string();
    let mut reader = source_factory(&payload).map_err(algolia_error)?;
    source_reader::admit_source_provider(source_provider, reader.source_provider())
        .map_err(source_export_error)?;

    reader
        .observe_quiescent_source()
        .await
        .map_err(source_export_error)?;
    let mut export = PreviewSourceExport::default();
    source_reader::read_source_snapshot(&mut reader, &mut export)
        .await
        .map_err(source_export_error)?;
    if fetch_preview_settings {
        if let Some(settings) = payload.preview_settings_override() {
            export.settings = settings.await.map_err(source_export_error)?;
        }
    }
    let records = export.record_count();
    let input =
        export.into_translation_input(source_index_name, target_index_name, source_provider);
    let report = translation::translate_spool_report(input).map_err(|error| {
        json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, error.safe_message())
    })?;

    // Preview ends before import.rs::import_accepted_export_inner enters
    // BulkBuildService::{prepare_publication,create_staging,activate}, and before job/spool admission.
    Ok(Json(MigrationPreviewResponse {
        report: report.into(),
        source_counts: MigrationPreviewSourceCounts {
            indexes: 1,
            records,
        },
    }))
}

/// TODO: Document submit_source_migration_impl.
async fn submit_source_migration_impl<P, F, R>(
    source_provider: AsyncMigrationSourceProvider,
    state: Arc<AppState>,
    authenticated_app_id: String,
    payload: P,
    source_factory: F,
) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError>
where
    P: AsyncMigrationSubmitPayload,
    F: FnOnce(&P) -> Result<R, AlgoliaClientError>,
    R: source_reader::MigrationSourceReader + Send + 'static,
{
    ensure_source_provider_supported(source_provider)?;
    let (_job_uuid, phase_record) = state
        .migration_runner
        .submit_source_import_for_owner(
            source_provider,
            payload,
            Some(authenticated_app_id),
            source_factory,
        )
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(AsyncMigrationStatusResponse::from(phase_record)),
    ))
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
    let resume_handle = spool
        .resumable_export_handle(job_uuid)
        .map_err(migration_status_spool_error)?;
    Ok(Json(
        AsyncMigrationStatusResponse::with_metadata_and_resume_handle(
            phase_record,
            &metadata,
            resume_handle,
        ),
    ))
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
        import::SourceImportRequest {
            expected_provider: AsyncMigrationSourceProvider::Algolia,
            target_index: admitted.target_index,
            publication_mode: admitted.publication_mode,
        },
        &mut reader,
        hooks,
    )
    .await
}

/// TODO: Document migrate_from_algolia_impl.
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
        import::SourceImportRequest {
            expected_provider: AsyncMigrationSourceProvider::Algolia,
            target_index: admitted.target_index,
            publication_mode: admitted.publication_mode,
        },
        &mut reader,
    )
    .await
}

/// TODO: Document admit_migration_request.
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
    admit_source_migration_target(
        manager,
        replication_manager,
        migration_target_index(payload),
        payload.overwrite,
    )
}

fn admit_meilisearch_migration_payload(
    manager: &Arc<flapjack::IndexManager>,
    replication_manager: Option<&Arc<flapjack_replication::manager::ReplicationManager>>,
    payload: &MigrateFromMeilisearchRequest,
) -> Result<AdmittedMigration, MigrateError> {
    validate_meilisearch_migration_request(payload)?;
    admit_source_migration_target(
        manager,
        replication_manager,
        meilisearch_target_index(payload),
        payload.overwrite,
    )
}

fn admit_typesense_migration_payload(
    manager: &Arc<flapjack::IndexManager>,
    replication_manager: Option<&Arc<flapjack_replication::manager::ReplicationManager>>,
    payload: &MigrateFromTypesenseRequest,
) -> Result<AdmittedMigration, MigrateError> {
    validate_typesense_migration_request(payload)?;
    admit_source_migration_target(
        manager,
        replication_manager,
        typesense_target_index(payload),
        payload.overwrite,
    )
}

fn admit_source_migration_target(
    manager: &Arc<flapjack::IndexManager>,
    replication_manager: Option<&Arc<flapjack_replication::manager::ReplicationManager>>,
    target_index: &str,
    overwrite: bool,
) -> Result<AdmittedMigration, MigrateError> {
    if replication_manager.is_some_and(|manager| manager.peer_count() > 0) {
        return Err(migration_ha_unsupported());
    }
    if overwrite {
        let staging_baseline = manager
            .capture_replacement_staging_baseline(target_index)
            .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
        return Ok(AdmittedMigration {
            target_index: target_index.to_string(),
            publication_mode: MigrationPublicationMode::ReplaceExisting { staging_baseline },
        });
    }
    Ok(AdmittedMigration {
        target_index: target_index.to_string(),
        publication_mode: MigrationPublicationMode::CreateOnly,
    })
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

/// Admit a Meilisearch submit endpoint through the production vendor policy,
/// falling back to the explicit loopback seam for the live contract fixture —
/// the same production-first shape as [`meilisearch_discovery_client`]. Both
/// served and generated submit handlers call this helper, while preview keeps
/// its separately asserted refusal semantics. The seam is reachable in every
/// profile only under `FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=1`, which gates
/// submit as well as preview and discovery. Production admission stays the
/// first branch in every build, and the production refusal is what the caller
/// sees when neither path admits the endpoint, so an unrecognised host never
/// leaks the loopback opt-in's existence.
fn meilisearch_source_reader(
    payload: &MigrateFromMeilisearchRequest,
) -> Result<
    source_reader::MeilisearchSourceReader<meilisearch_client::MeilisearchClient>,
    AlgoliaClientError,
> {
    let admitted = source_reader::MeilisearchSourceReader::new(
        &payload.endpoint,
        &payload.api_key,
        &payload.source_index,
    );
    admitted.or_else(|vendor_refusal| {
        meilisearch_loopback_source_reader(payload).map_err(|_| vendor_refusal)
    })
}

/// Single owner of the Meilisearch loopback source reader in every profile.
/// Submit reaches it only after production vendor admission refuses, and only
/// `FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=1` admits it. The literal-loopback
/// checks live inside
/// `MeilisearchClient::new_preview_loopback`, so it refuses before parsing an
/// attacker-controlled endpoint.
fn meilisearch_loopback_source_reader(
    payload: &MigrateFromMeilisearchRequest,
) -> Result<
    source_reader::MeilisearchSourceReader<meilisearch_client::MeilisearchClient>,
    AlgoliaClientError,
> {
    let source = meilisearch_client::MeilisearchClient::new_preview_loopback(
        &payload.endpoint,
        &payload.api_key,
        &payload.source_index,
    )
    .map_err(map_preview_meilisearch_error)?;
    Ok(source_reader::MeilisearchSourceReader::from_source(
        &payload.source_index,
        source,
    ))
}

/// Single owner of Meilisearch preview admission, shared by the preview
/// settings override and the preview source reader. Production Meilisearch
/// Cloud admission is the first branch in every profile; only the sanitized
/// "endpoint is not allowed" refusal hands the same `payload.endpoint` to the
/// literal-loopback fixture seam, which is reachable in the shipped profile
/// only behind the existing `FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=1` opt-in.
/// Loopback classification lives inside the client constructor, so this seam
/// never re-parses an endpoint. Preview reports the seam's own "loopback
/// endpoint is disabled" refusal, which
/// `preview_tests::meilisearch::meilisearch_preview_requires_explicit_loopback_opt_in`
/// asserts. Submit must not adopt this shape — its refusal is the vendor one,
/// so an unrecognised host never learns the opt-in exists.
fn preview_meilisearch_client(
    payload: &MigrateFromMeilisearchRequest,
) -> Result<meilisearch_client::MeilisearchClient, AlgoliaClientError> {
    let client = match meilisearch_client::MeilisearchClient::new(
        &payload.endpoint,
        &payload.api_key,
        &payload.source_index,
    ) {
        Err(vendor_refusal) if vendor_refusal.is_endpoint_not_allowed() => {
            meilisearch_client::MeilisearchClient::new_preview_loopback(
                &payload.endpoint,
                &payload.api_key,
                &payload.source_index,
            )
        }
        admitted => admitted,
    };
    client.map_err(map_preview_meilisearch_error)
}

fn preview_meilisearch_source_reader(
    payload: &MigrateFromMeilisearchRequest,
) -> Result<
    source_reader::MeilisearchSourceReader<meilisearch_client::MeilisearchClient>,
    AlgoliaClientError,
> {
    let source = preview_meilisearch_client(payload)?;
    Ok(source_reader::MeilisearchSourceReader::from_source(
        &payload.source_index,
        source,
    ))
}

fn map_preview_meilisearch_error(
    error: meilisearch_client::MeilisearchClientError,
) -> AlgoliaClientError {
    let kind = if error.kind() == meilisearch_client::MeilisearchErrorKind::Validation {
        AlgoliaErrorKind::Validation
    } else {
        AlgoliaErrorKind::Upstream
    };
    AlgoliaClientError::new(kind, error.safe_message())
}

/// Admit a Typesense submit endpoint through the production vendor policy,
/// falling back to the explicit loopback seam for the live contract fixture —
/// the same shape as [`typesense_discovery_client`] and [`meilisearch_source_reader`],
/// with both served and generated submit handlers calling this helper. Preview
/// keeps its separately asserted refusal semantics.
/// The seam is reachable in every profile only under
/// `FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1`, which gates submit as well as
/// preview and discovery. Production admission stays the first branch in every
/// build, and the production refusal is what the caller sees when neither path
/// admits the endpoint, so an unrecognised host never leaks the loopback
/// opt-in's existence.
fn typesense_source_reader(
    payload: &MigrateFromTypesenseRequest,
) -> Result<
    source_reader::TypesenseSourceReader<typesense_client::TypesenseClient>,
    AlgoliaClientError,
> {
    let admitted = source_reader::TypesenseSourceReader::new(
        &payload.node,
        &payload.api_key,
        &payload.source_index,
        payload.source_write_frozen,
    );
    admitted.or_else(|vendor_refusal| {
        typesense_loopback_source_reader(payload).map_err(|_| vendor_refusal)
    })
}

/// Single owner of the Typesense loopback source reader in every profile.
/// Submit reaches it only after production vendor admission refuses, and only
/// `FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1` admits it. The literal-loopback
/// checks live inside `TypesenseClient::new_preview_loopback`, so it refuses
/// before parsing an attacker-controlled endpoint.
fn typesense_loopback_source_reader(
    payload: &MigrateFromTypesenseRequest,
) -> Result<
    source_reader::TypesenseSourceReader<typesense_client::TypesenseClient>,
    AlgoliaClientError,
> {
    let source = typesense_client::TypesenseClient::new_preview_loopback(
        &payload.node,
        &payload.api_key,
        &payload.source_index,
    )
    .map_err(typesense_source_reader::map_typesense_client_error)?;
    Ok(source_reader::TypesenseSourceReader::from_source(
        &payload.source_index,
        source,
        payload.source_write_frozen,
    ))
}

#[cfg(debug_assertions)]
fn preview_typesense_source_reader(
    payload: &MigrateFromTypesenseRequest,
) -> Result<
    source_reader::TypesenseSourceReader<typesense_client::TypesenseClient>,
    AlgoliaClientError,
> {
    let source = preview_typesense_client(payload)?;
    Ok(source_reader::TypesenseSourceReader::from_source(
        &payload.source_index,
        source,
        payload.source_write_frozen,
    ))
}

#[cfg(not(debug_assertions))]
fn preview_typesense_source_reader(
    payload: &MigrateFromTypesenseRequest,
) -> Result<
    source_reader::TypesenseSourceReader<typesense_client::TypesenseClient>,
    AlgoliaClientError,
> {
    typesense_source_reader(payload)
}

/// Preserve production Typesense Cloud admission in debug builds, then fall
/// back to the explicit loopback fixture seam when vendor admission refuses the
/// endpoint. Release preview delegates directly to `typesense_source_reader`.
#[cfg(debug_assertions)]
fn preview_typesense_client(
    payload: &MigrateFromTypesenseRequest,
) -> Result<typesense_client::TypesenseClient, AlgoliaClientError> {
    let client = match typesense_client::TypesenseClient::new(
        &payload.node,
        &payload.api_key,
        &payload.source_index,
    ) {
        Ok(client) => Ok(client),
        Err(error) if error.is_endpoint_not_allowed() => {
            if !typesense_preview_loopback_candidate(&payload.node) {
                Err(error)
            } else {
                typesense_client::TypesenseClient::new_preview_loopback(
                    &payload.node,
                    &payload.api_key,
                    &payload.source_index,
                )
            }
        }
        Err(error) => Err(error),
    };
    client.map_err(typesense_source_reader::map_typesense_client_error)
}

#[cfg(debug_assertions)]
fn typesense_preview_loopback_candidate(node: &str) -> bool {
    typesense_client::parse_literal_loopback_url(node).is_ok()
}

fn parse_typesense_submit_payload(
    body: &[u8],
) -> Result<MigrateFromTypesenseRequest, MigrateError> {
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| json_error_parts(StatusCode::BAD_REQUEST, "Invalid migration request body"))?;
    if value.get("appId").is_some() && value.get("node").is_none() {
        return Err(source_provider_unsupported());
    }
    if value.get("endpoint").is_some() && value.get("node").is_none() {
        return Err(source_provider_payload_mismatch(
            "Typesense payload does not match source_provider",
        ));
    }
    serde_json::from_value(value)
        .map_err(|_| json_error_parts(StatusCode::BAD_REQUEST, "Invalid migration request body"))
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

fn validate_meilisearch_migration_request(
    payload: &MigrateFromMeilisearchRequest,
) -> Result<(), MigrateError> {
    if payload.endpoint.is_empty() || payload.api_key.is_empty() || payload.source_index.is_empty()
    {
        return Err(json_error_parts(
            StatusCode::BAD_REQUEST,
            "endpoint, apiKey, and sourceIndex are required",
        ));
    }
    meilisearch_client::validate_source_index(&payload.source_index)
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.safe_message()))?;

    let target_index = payload
        .target_index
        .as_deref()
        .unwrap_or(payload.source_index.as_str());
    validate_index_name(target_index)
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))
}

fn validate_typesense_migration_request(
    payload: &MigrateFromTypesenseRequest,
) -> Result<(), MigrateError> {
    if payload.node.is_empty() || payload.api_key.is_empty() || payload.source_index.is_empty() {
        return Err(json_error_parts(
            StatusCode::BAD_REQUEST,
            "node, apiKey, and sourceIndex are required",
        ));
    }
    if !payload.source_write_frozen {
        return Err(json_error_parts(
            StatusCode::BAD_REQUEST,
            TYPESENSE_WRITE_FREEZE_REQUIRED_MESSAGE,
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

fn meilisearch_target_index(payload: &MigrateFromMeilisearchRequest) -> &str {
    payload
        .target_index
        .as_deref()
        .unwrap_or(payload.source_index.as_str())
}

fn typesense_target_index(payload: &MigrateFromTypesenseRequest) -> &str {
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

/// Map a neutral source-export failure onto the existing migration status
/// rules, so provider-neutral capture keeps a single status owner.
fn source_export_error(
    error: source_reader::SourceExportError,
) -> (StatusCode, Json<serde_json::Value>) {
    algolia_error(error.into_inner())
}

fn algolia_error(error: AlgoliaClientError) -> (StatusCode, Json<serde_json::Value>) {
    let status = match error.kind() {
        AlgoliaErrorKind::Validation => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_GATEWAY,
    };
    json_error_parts(status, error.safe_message())
}

fn meilisearch_error(error: meilisearch_client::MeilisearchClientError) -> MigrateError {
    match error.kind() {
        meilisearch_client::MeilisearchErrorKind::Validation => {
            json_error_parts(StatusCode::BAD_REQUEST, error.safe_message())
        }
        // A source key missing the `indexes.get` ACL fails upstream with 403
        // `invalid_api_key`; relay that code so the caller can fix the ACL
        // instead of reading a generic upstream failure.
        meilisearch_client::MeilisearchErrorKind::Forbidden => json_error_parts_with_code(
            StatusCode::FORBIDDEN,
            MEILISEARCH_INVALID_API_KEY_CODE,
            error.safe_message(),
        ),
        _ => json_error_parts(StatusCode::BAD_GATEWAY, error.safe_message()),
    }
}

fn typesense_error(error: typesense_client::TypesenseClientError) -> MigrateError {
    let status = match error.kind() {
        typesense_client::TypesenseErrorKind::Validation => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_GATEWAY,
    };
    json_error_parts(status, error.safe_message())
}

fn ensure_source_provider_supported(
    source_provider: AsyncMigrationSourceProvider,
) -> Result<(), MigrateError> {
    if source_provider.is_supported_source() {
        Ok(())
    } else {
        Err(source_provider_unsupported())
    }
}

fn ensure_source_provider_preview_supported(
    source_provider: AsyncMigrationSourceProvider,
) -> Result<(), MigrateError> {
    if source_provider.supports_preview() {
        Ok(())
    } else {
        Err(source_provider_unsupported())
    }
}

fn ensure_resume_source_provider_supported(
    source_provider: AsyncMigrationSourceProvider,
) -> Result<(), MigrateError> {
    if source_provider == AsyncMigrationSourceProvider::Algolia {
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

fn source_provider_payload_mismatch(message: &'static str) -> MigrateError {
    json_error_parts_with_code(
        StatusCode::BAD_REQUEST,
        SOURCE_PROVIDER_PAYLOAD_MISMATCH_CODE,
        message,
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
        SpoolErrorKind::JobNotFound | SpoolErrorKind::CheckpointHandleNotFound => {
            StatusCode::NOT_FOUND
        }
        #[cfg(test)]
        SpoolErrorKind::PublicHandleNotFound => StatusCode::NOT_FOUND,
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
        | SpoolErrorKind::SourceIdentityMismatch
        | SpoolErrorKind::ResourceVerificationFailed
        | SpoolErrorKind::ResourceComplete
        | SpoolErrorKind::ResourcesIncomplete
        | SpoolErrorKind::JobTerminal
        | SpoolErrorKind::JobNotAccepted
        | SpoolErrorKind::JobNotInterrupted
        | SpoolErrorKind::UnsupportedArtifactKind
        | SpoolErrorKind::UnsupportedSpoolFormat
        | SpoolErrorKind::InvalidPhaseTransition
        | SpoolErrorKind::PrivacyScrubIntentCollision => StatusCode::BAD_REQUEST,
        #[cfg(test)]
        SpoolErrorKind::InvalidCompletedResourceId => StatusCode::BAD_REQUEST,
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

pub(super) fn resume_spool_error(error: spool::SpoolError) -> MigrateError {
    match error.kind() {
        SpoolErrorKind::JobNotInterrupted => migration_resume_claim_conflict(),
        SpoolErrorKind::JobNotFound | SpoolErrorKind::CheckpointHandleNotFound => {
            migration_resume_not_available()
        }
        _ => spool_error(error),
    }
}

/// TODO: Document ensure_async_migration_owner.
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

fn resumable_algolia_job(
    state: &AppState,
    owner_identity: &str,
    job_id: &str,
) -> Result<(Uuid, String), MigrateError> {
    let job_uuid = Uuid::parse_str(job_id)
        .map_err(|_| json_error_parts(StatusCode::BAD_REQUEST, "job_id must be a valid UUID"))?;
    let spool = import::spool_for_manager(&state.manager)?;
    let metadata = spool
        .read_async_migration_metadata(job_uuid)
        .map_err(|error| match error.kind() {
            SpoolErrorKind::JobNotFound => migration_resume_not_available(),
            _ => migration_status_spool_error(error),
        })?;
    if metadata
        .authenticated_app_id
        .as_deref()
        .is_some_and(|owner| owner != owner_identity)
        || metadata.source_provider != AsyncMigrationSourceProvider::Algolia
        || metadata.operation_kind != spool::AsyncMigrationOperationKind::SourceImport
    {
        return Err(migration_job_not_found());
    }
    if let Some(handle) = spool
        .resumable_export_handle(job_uuid)
        .map_err(resume_spool_error)?
    {
        return Ok((job_uuid, handle));
    }
    let phase = spool
        .read_migration_phase(job_uuid)
        .map_err(resume_spool_error)?;
    if phase.phase == MigrationPhase::Exporting
        && phase.disposition == MigrationDisposition::Running
        && phase.terminal_at.is_none()
        && spool
            .export_lifecycle_is_running(job_uuid)
            .map_err(resume_spool_error)?
        && state.migration_runner.resume_claim_is_active(job_uuid)
    {
        return Err(migration_resume_claim_conflict());
    }
    Err(migration_resume_not_available())
}

fn migration_resume_claim_conflict() -> MigrateError {
    json_error_parts_with_code(
        StatusCode::CONFLICT,
        MIGRATION_RESUME_CLAIM_CONFLICT_CODE,
        MIGRATION_RESUME_CLAIM_CONFLICT_MESSAGE,
    )
}

fn migration_resume_not_available() -> MigrateError {
    json_error_parts_with_code(
        StatusCode::CONFLICT,
        MIGRATION_RESUME_NOT_AVAILABLE_CODE,
        MIGRATION_RESUME_NOT_AVAILABLE_MESSAGE,
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{body_json, TestStateBuilder};
    use axum::response::IntoResponse;
    use serde_json::json;
    use tempfile::TempDir;

    /// TODO: Document migration_dto_wire_contract.
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

    #[test]
    fn meilisearch_migration_request_validation_rejects_traversal_shaped_source_index() {
        let request = MigrateFromMeilisearchRequest {
            endpoint: "https://meilisearch.io".to_string(),
            api_key: "source-key".to_string(),
            source_index: "../escape".to_string(),
            target_index: Some("products".to_string()),
            overwrite: false,
        };

        let error = validate_meilisearch_migration_request(&request)
            .expect_err("traversal-shaped sourceIndex should fail before source admission");

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.1 .0,
            json!({"message": "Meilisearch source index is invalid", "status": 400})
        );
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
#[path = "meilisearch_contract_tests.rs"]
mod meilisearch_contract_tests;

#[cfg(test)]
#[path = "meilisearch_client_tests.rs"]
mod meilisearch_client_tests;

#[cfg(test)]
#[path = "source_reader_tests.rs"]
mod source_reader_tests;
