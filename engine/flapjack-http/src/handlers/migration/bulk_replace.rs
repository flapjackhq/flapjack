//! Admin-only node-local NDJSON bulk replacement.

use super::bulk_build::BulkBuildService;
use super::spool::{
    AsyncMigrationPublicationSemantic, MigrationImportOutcome, MigrationPhase,
    ResourceDenominators, SpoolLimits, SpoolStore,
};
use super::{
    authenticated_owner_identity, cancel_source_migration, get_source_migration_status, import,
    migration_capacity_exhausted, spool_error, AsyncMigrationDisposition, AsyncMigrationPhase,
    AsyncMigrationStatusResponse, MigrateError, MigrationPublicationMode, MigrationTopology,
};
use crate::auth::AuthenticatedAppId;
use crate::error_response::json_error_parts;
use crate::handlers::AppState;
use axum::body::Body;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header::CONTENT_LENGTH, HeaderMap, StatusCode};
use axum::Json;
use flapjack::validate_index_name;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio_stream::StreamExt;
use utoipa::ToSchema;
use uuid::Uuid;

pub(crate) const BULK_REPLACE_MAX_BYTES_ENV: &str = "FLAPJACK_BULK_REPLACE_MAX_BYTES";
const BULK_REPLACE_PAGE_DOCUMENTS: usize = 500;
const BULK_REPLACE_PAGE_BYTES: usize = 1024 * 1024;
const BULK_REPLACE_TOO_LARGE_MESSAGE: &str =
    "Bulk replacement payload exceeds the configured limit";
const HA_UNSUPPORTED_MESSAGE: &str =
    "Migration is only supported when no replication peers are configured";

#[derive(Debug, Deserialize)]
pub struct BulkReplaceQuery {
    #[serde(rename = "indexName")]
    index_name: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BulkReplaceReceipt {
    #[serde(rename = "jobID")]
    job_id: Uuid,
    target_index: String,
    topology: MigrationTopology,
    phase: AsyncMigrationPhase,
    disposition: AsyncMigrationDisposition,
}

/// Admit a node-local atomic replacement from an NDJSON request body.
#[utoipa::path(
    post,
    path = "/1/migrations/bulk-replace",
    tag = "migration",
    params(
        ("indexName" = String, Query, description = "Target index to replace atomically")
    ),
    request_body(
        content = String,
        content_type = "application/x-ndjson",
        description = "One JSON object per line; every object requires a non-empty objectID"
    ),
    responses(
        (status = 202, description = "Node-local bulk replacement job admitted", body = BulkReplaceReceipt),
        (status = 400, description = "Invalid target index or NDJSON document"),
        (status = 403, description = "Missing, invalid, or non-admin API key"),
        (status = 413, description = "Bulk replacement payload exceeds the configured limit"),
        (status = 429, description = "Migration job capacity is exhausted"),
        (status = 503, description = "Bulk replacement is unavailable while replication peers are configured")
    ),
    security(("api_key" = []))
)]
pub async fn submit_bulk_replace_http(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    Extension(mutation_permit): Extension<crate::pause_registry::MutationPermit>,
    headers: HeaderMap,
    Query(query): Query<BulkReplaceQuery>,
    body: Body,
) -> Result<(StatusCode, Json<BulkReplaceReceipt>), MigrateError> {
    validate_index_name(&query.index_name)
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
    if state
        .replication_manager
        .as_ref()
        .is_some_and(|manager| manager.peer_count() > 0)
    {
        return Err(crate::error_response::json_error_parts_with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "migration_ha_unsupported",
            HA_UNSUPPORTED_MESSAGE,
        ));
    }

    // Refusing an oversized declaration here avoids wasted spool I/O. The streaming admission
    // remains the authoritative runtime cap for missing, malformed, or dishonest declarations.
    if headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|declared_bytes| declared_bytes > state.bulk_replace_max_bytes)
    {
        return Err(json_error_parts(
            StatusCode::PAYLOAD_TOO_LARGE,
            BULK_REPLACE_TOO_LARGE_MESSAGE,
        ));
    }

    let staging_baseline = state
        .manager
        .capture_replacement_staging_baseline(&query.index_name)
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
    let (publication_mode, publication_semantic) =
        if state.manager.base_path.join(&query.index_name).exists() {
            (
                MigrationPublicationMode::ReplaceExisting { staging_baseline },
                AsyncMigrationPublicationSemantic::ReplaceExisting,
            )
        } else {
            (
                MigrationPublicationMode::CreateOnly,
                AsyncMigrationPublicationSemantic::CreateOnly,
            )
        };
    let permit = state
        .migration_runner
        .acquire_bulk_replace_permit()
        .map_err(|_| migration_capacity_exhausted())?;
    let owner = authenticated_owner_identity(authenticated_app_id, &headers);
    let spool = import::spool_for_manager(&state.manager)?;
    let job_uuid = Uuid::new_v4();
    let phase = spool
        .create_bulk_replace_admission_for_owner(
            job_uuid,
            &query.index_name,
            &owner,
            publication_semantic,
        )
        .map_err(spool_error)?;

    let admitted =
        admit_streamed_payload(&spool, job_uuid, state.bulk_replace_max_bytes, body).await;
    if let Err(error) = admitted {
        let _ = spool.fail_migration(job_uuid);
        let _ = spool.delete_job_if_terminal(job_uuid);
        return Err(error);
    }

    let target_index = query.index_name;
    state.migration_runner.spawn_bulk_replace(
        job_uuid,
        target_index.clone(),
        publication_mode,
        permit,
        mutation_permit,
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(BulkReplaceReceipt {
            job_id: job_uuid,
            target_index,
            topology: MigrationTopology::SingleNodeOnly,
            phase: phase.phase.into(),
            disposition: phase.disposition.into(),
        }),
    ))
}

/// Return durable status for a node-local bulk replacement job.
#[utoipa::path(
    get,
    path = "/1/migrations/bulk-replace/{job_id}",
    tag = "migration",
    params(
        ("job_id" = Uuid, Path, description = "Bulk replacement job UUID")
    ),
    responses(
        (status = 200, description = "Durable bulk replacement job status", body = AsyncMigrationStatusResponse),
        (status = 400, description = "Invalid bulk replacement job UUID"),
        (status = 404, description = "No owned durable migration record is retained for the UUID"),
        (status = 500, description = "Migration status record could not be read")
    ),
    security(("api_key" = []))
)]
pub async fn get_bulk_replace_status_http(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Result<Json<AsyncMigrationStatusResponse>, MigrateError> {
    get_source_migration_status(
        state,
        authenticated_owner_identity(authenticated_app_id, &headers),
        job_id,
        None,
    )
    .await
}

/// Request cooperative cancellation of a node-local bulk replacement job.
#[utoipa::path(
    post,
    path = "/1/migrations/bulk-replace/{job_id}/cancel",
    tag = "migration",
    params(
        ("job_id" = Uuid, Path, description = "Bulk replacement job UUID")
    ),
    responses(
        (status = 202, description = "Durable bulk replacement status after cancellation request", body = AsyncMigrationStatusResponse),
        (status = 400, description = "Invalid bulk replacement job UUID"),
        (status = 404, description = "No owned durable migration record is retained for the UUID"),
        (status = 409, description = "Cancellation arrived after the publication commit boundary"),
        (status = 500, description = "Migration cancellation could not be persisted")
    ),
    security(("api_key" = []))
)]
pub async fn cancel_bulk_replace_http(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Result<(StatusCode, Json<AsyncMigrationStatusResponse>), MigrateError> {
    let response = cancel_source_migration(
        state,
        authenticated_owner_identity(authenticated_app_id, &headers),
        job_id,
        None,
    )
    .await?;
    Ok((StatusCode::ACCEPTED, response))
}

async fn admit_streamed_payload(
    spool: &SpoolStore,
    job_uuid: Uuid,
    max_bytes: u64,
    body: Body,
) -> Result<(), MigrateError> {
    let limits = SpoolLimits::default();
    let provisional_digest = hex::encode(Sha256::digest(job_uuid.as_bytes()));
    spool
        .create_export(
            job_uuid,
            &provisional_digest,
            ResourceDenominators {
                settings: 1,
                documents: limits.max_items_per_resource,
                rules: 0,
                synonyms: 0,
                config: 0,
            },
        )
        .map_err(spool_error)?;

    let settings = b"{}";
    spool
        .commit_settings_once(job_uuid, settings, &hex::encode(Sha256::digest(settings)))
        .map_err(spool_error)?;
    let mut stream = NdjsonSpoolStream::new(spool, job_uuid, max_bytes);
    let mut chunks = body.into_data_stream();
    while let Some(chunk) = chunks.next().await {
        stream.push(&chunk.map_err(|_| internal_error())?)?;
    }
    let (document_count, source_digest) = stream.finish()?;
    spool
        .seal_bulk_replace_export(job_uuid, &source_digest, document_count)
        .map_err(spool_error)?;
    spool
        .complete_documents(job_uuid, document_count, &source_digest)
        .map_err(spool_error)?;
    let empty_digest = hex::encode(Sha256::digest([]));
    spool
        .complete_rules(job_uuid, 0, &empty_digest)
        .map_err(spool_error)?;
    spool
        .complete_synonyms(job_uuid, 0, &empty_digest)
        .map_err(spool_error)?;
    spool.accept_export(job_uuid).map_err(spool_error)
}

struct NdjsonSpoolStream<'a> {
    spool: &'a SpoolStore,
    job_uuid: Uuid,
    max_bytes: u64,
    received_bytes: u64,
    pending: Vec<u8>,
    page: Vec<Value>,
    page_bytes: usize,
    document_count: u64,
    source_hasher: Sha256,
}

impl<'a> NdjsonSpoolStream<'a> {
    fn new(spool: &'a SpoolStore, job_uuid: Uuid, max_bytes: u64) -> Self {
        Self {
            spool,
            job_uuid,
            max_bytes,
            received_bytes: 0,
            pending: Vec::new(),
            page: Vec::new(),
            page_bytes: 0,
            document_count: 0,
            source_hasher: Sha256::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<(), MigrateError> {
        self.received_bytes = self.received_bytes.saturating_add(chunk.len() as u64);
        if self.received_bytes > self.max_bytes {
            return Err(json_error_parts(
                StatusCode::PAYLOAD_TOO_LARGE,
                BULK_REPLACE_TOO_LARGE_MESSAGE,
            ));
        }
        self.pending.extend_from_slice(chunk);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=newline).collect::<Vec<_>>();
            self.push_line(&line[..line.len() - 1])?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(u64, String), MigrateError> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.push_line(&line)?;
        }
        self.commit_page()?;
        Ok((
            self.document_count,
            hex::encode(self.source_hasher.finalize()),
        ))
    }

    fn push_line(&mut self, line: &[u8]) -> Result<(), MigrateError> {
        let line = trim_ascii(line);
        if line.is_empty() {
            return Ok(());
        }
        let value: Value = serde_json::from_slice(line).map_err(|_| {
            json_error_parts(
                StatusCode::BAD_REQUEST,
                "Bulk replacement body must be NDJSON",
            )
        })?;
        let has_object_id = value
            .get("objectID")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .is_some();
        if !has_object_id {
            return Err(json_error_parts(
                StatusCode::BAD_REQUEST,
                "Every bulk replacement document must have a non-empty objectID",
            ));
        }
        self.source_hasher.update(line);
        self.source_hasher.update(b"\n");
        self.page_bytes += line.len();
        self.document_count += 1;
        self.page.push(value);
        if self.page.len() >= BULK_REPLACE_PAGE_DOCUMENTS
            || self.page_bytes >= BULK_REPLACE_PAGE_BYTES
        {
            self.commit_page()?;
        }
        Ok(())
    }

    fn commit_page(&mut self) -> Result<(), MigrateError> {
        if self.page.is_empty() {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&self.page).map_err(|_| internal_error())?;
        let ids = self
            .page
            .iter()
            .map(|value| value["objectID"].as_str().expect("validated objectID"))
            .collect::<Vec<_>>();
        self.spool
            .commit_document_page_with_ids(self.job_uuid, &bytes, &ids)
            .map_err(spool_error)?;
        self.page.clear();
        self.page_bytes = 0;
        Ok(())
    }
}

pub(crate) fn configured_bulk_cap(default: u64) -> Result<u64, String> {
    match std::env::var(BULK_REPLACE_MAX_BYTES_ENV) {
        Ok(value) => value
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| "Bulk replacement byte limit is invalid".to_string()),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("Bulk replacement byte limit is not valid unicode".to_string())
        }
    }
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

pub(super) async fn run_bulk_replace(
    manager: &Arc<flapjack::IndexManager>,
    job_uuid: Uuid,
    target_index: String,
    publication_mode: MigrationPublicationMode,
    #[cfg(test)] test_hooks: super::bulk_build::BulkBuildTestHooks,
) -> Result<(), MigrateError> {
    let spool = import::spool_for_manager(manager)?;
    #[cfg(not(test))]
    let build = BulkBuildService::new(manager, &spool, job_uuid, &target_index);
    #[cfg(test)]
    let build = BulkBuildService::new(manager, &spool, job_uuid, &target_index, test_hooks);
    let publication = build.prepare_publication()?;
    let ((), publication) = import::abort_publication_on_error(
        &spool,
        job_uuid,
        spool
            .transition_migration_phase(job_uuid, MigrationPhase::Staging)
            .map(|_| ())
            .map_err(spool_error),
        publication,
    )?;
    let staging_result =
        import::stage_accepted_bulk_replace(&build, &spool, &publication, job_uuid, &target_index)
            .await;
    let ((), publication) = import::abort_publication_on_error(
        &spool,
        job_uuid,
        staging_result.map(|_| ()),
        publication,
    )?;
    let ((), publication) = import::abort_publication_on_error(
        &spool,
        job_uuid,
        spool
            .transition_migration_phase(job_uuid, MigrationPhase::Activating)
            .map(|_| ())
            .map_err(spool_error),
        publication,
    )?;
    let ((), publication) = import::abort_publication_on_error(
        &spool,
        job_uuid,
        build.cancellation().check(),
        publication,
    )?;
    build.activate(publication, publication_mode).await?;
    import::refresh_target(manager, &target_index)?;
    let counts = build.activated_counts()?;
    spool
        .record_import_outcome(
            job_uuid,
            MigrationImportOutcome {
                settings_applied: counts.settings,
                objects_imported: counts.documents,
                synonyms_imported: counts.synonyms,
                rules_imported: counts.rules,
                warnings: Vec::new(),
            },
        )
        .map_err(spool_error)?;
    spool
        .succeed_migration(job_uuid, None)
        .map_err(spool_error)?;
    let _ = spool.delete_export_artifacts_if_present(job_uuid);
    Ok(())
}

fn internal_error() -> MigrateError {
    json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestStateBuilder;
    use axum::{
        body::Bytes,
        http::{header, Request},
        routing::post,
        Router,
    };
    use std::{
        convert::Infallible,
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll},
    };
    use tempfile::TempDir;
    use tokio::sync::Notify;
    use tokio_stream::Stream;
    use tower::ServiceExt;

    struct FirstPollPendingStream {
        polls: Arc<AtomicUsize>,
        first_poll: Arc<Notify>,
    }

    impl Stream for FirstPollPendingStream {
        type Item = Result<Bytes, Infallible>;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            this.polls.fetch_add(1, Ordering::SeqCst);
            this.first_poll.notify_one();
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn oversized_declared_content_length_is_rejected_before_body_poll() {
        const MAX_BYTES: u64 = 32;

        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp)
            .with_bulk_replace_max_bytes(MAX_BYTES)
            .build_shared();
        let mutation_permit = state.global_mutation_fence.try_admit_mutation().unwrap();
        let app = Router::new()
            .route("/1/migrations/bulk-replace", post(submit_bulk_replace_http))
            .with_state(state);
        let polls = Arc::new(AtomicUsize::new(0));
        let first_poll = Arc::new(Notify::new());
        let body = Body::from_stream(FirstPollPendingStream {
            polls: Arc::clone(&polls),
            first_poll: Arc::clone(&first_poll),
        });
        let mut request = Request::builder()
            .method("POST")
            .uri("/1/migrations/bulk-replace?indexName=declared_over_cap")
            .header(header::CONTENT_LENGTH, MAX_BYTES + 1)
            .body(body)
            .unwrap();
        request
            .extensions_mut()
            .insert(AuthenticatedAppId("bulk-owner-app".to_string()));
        request.extensions_mut().insert(mutation_permit);

        let mut response = Box::pin(app.oneshot(request));
        tokio::select! {
            biased;
            response = &mut response => {
                let response = response.unwrap();
                assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
                assert_eq!(
                    polls.load(Ordering::SeqCst),
                    0,
                    "an oversized declared upload must be refused before polling its body",
                );
            }
            _ = first_poll.notified() => {
                assert_eq!(
                    polls.load(Ordering::SeqCst),
                    0,
                    "an oversized declared upload must be refused before polling its body",
                );
            }
        }
    }
}
