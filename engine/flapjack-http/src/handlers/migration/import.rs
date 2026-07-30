use super::bulk_build::{flapjack_error, BulkBuildCounts, BulkBuildService};
#[cfg(test)]
use super::bulk_build::{
    AfterDocumentBatchWriteHook, BeforeDocumentBatchWriteHook, BulkBuildTestEvent,
    BulkBuildTestHooks,
};
use super::export::{export_algolia_source_for_import, AcceptedExport, ExportError};
use super::source_reader::MigrationSourceReader;
use super::spool::{
    MigrationImportOutcome, MigrationImportWarning, MigrationPhase, SpoolLimits, SpoolStore,
};
use super::translation::{
    translate_accepted_spool_payload, translate_accepted_spool_settings, warning_message,
    SettingsTranslationOutcome, TranslationOutcome, TranslationReport, TranslationReportEntry,
    TranslationSessionInstrumentation, TranslationStreamError,
};
use super::{
    algolia_error, migration_cancelled_error, spool_error, MigrateCount, MigrateError,
    MigrateFromAlgoliaResponse, MigrateWarning, MigrationPublicationMode,
};
use crate::error_response::json_error_parts;
use crate::handlers::index_resource_store::save_resource_batch;
use crate::handlers::settings::persist_index_settings;
use axum::{http::StatusCode, Json};
use flapjack::error::FlapjackError;
use flapjack::index::manager::publication::PreStagedPublication;
#[cfg(test)]
use flapjack::index::manager::publication::PublicationFaultPoint;
use flapjack::index::manager::validate_index_name;
use flapjack::index::rules::RuleStore;
use flapjack::index::synonyms::SynonymStore;
use flapjack::types::Document;
use flapjack::IndexManager;
use serde_json::Value;
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::env;
#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::SendError;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Barrier;
#[cfg(test)]
use std::thread;
#[cfg(test)]
use std::time::{Duration, Instant};
#[cfg(test)]
use tokio::sync::Notify;
use uuid::Uuid;

#[cfg(test)]
pub(super) const LIVE_IMPORT_PRE_ACTIVATION_SOURCE_ENV: &str =
    "FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_SOURCE";
#[cfg(test)]
pub(super) const LIVE_IMPORT_PRE_ACTIVATION_BARRIER_DIR_ENV: &str =
    "FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_BARRIER_DIR";
#[cfg(test)]
pub(super) const LIVE_IMPORT_POST_COMMIT_SOURCE_ENV: &str =
    "FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_POST_COMMIT_SOURCE";
#[cfg(test)]
pub(super) const LIVE_IMPORT_POST_COMMIT_BARRIER_DIR_ENV: &str =
    "FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_POST_COMMIT_BARRIER_DIR";
#[cfg(test)]
pub(super) const LIVE_IMPORT_BARRIER_OBSERVED_FILE: &str = "observed";
#[cfg(test)]
pub(super) const LIVE_IMPORT_BARRIER_RELEASE_FILE: &str = "release";
#[cfg(test)]
const LIVE_IMPORT_BARRIER_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(test)]
type AfterAcceptedExportHook = Arc<dyn Fn(&SpoolStore, Uuid) + Send + Sync>;
#[cfg(test)]
type BeforeActivationHook = Arc<dyn Fn() + Send + Sync>;
#[cfg(test)]
type BeforeReplicaMaterializationHook =
    Arc<dyn Fn(&str) -> Result<(), FlapjackError> + Send + Sync>;

#[cfg(test)]
#[derive(Clone, Default)]
pub(super) struct ImportTestHooks {
    after_accepted_export: Option<AfterAcceptedExportHook>,
    before_document_batch_write: Option<BeforeDocumentBatchWriteHook>,
    after_document_batch_write: Option<AfterDocumentBatchWriteHook>,
    before_activation: Option<BeforeActivationHook>,
    before_replica_materialization: Option<BeforeReplicaMaterializationHook>,
    bulk_build: BulkBuildTestHooks,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrivacyScrubBoundary {
    PreIntent,
    PostIntent,
    EngineCommit,
    PreAck,
    ResponseLoss,
    Restart,
    AckReplay,
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct PrivacyScrubBoundaryControl {
    pub(super) observed: Notify,
    pub(super) release: Notify,
}

#[cfg(test)]
#[derive(Default)]
pub struct PrivacyScrubTestHooks {
    enabled: HashSet<PrivacyScrubBoundary>,
    pre_intent: PrivacyScrubBoundaryControl,
    post_intent: PrivacyScrubBoundaryControl,
    engine_commit: PrivacyScrubBoundaryControl,
    pre_ack: PrivacyScrubBoundaryControl,
    response_loss: PrivacyScrubBoundaryControl,
    restart: PrivacyScrubBoundaryControl,
    ack_replay: PrivacyScrubBoundaryControl,
}

#[cfg(test)]
impl PrivacyScrubBoundary {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::PreIntent => "pre_intent",
            Self::PostIntent => "post_intent",
            Self::EngineCommit => "engine_commit",
            Self::PreAck => "pre_ack",
            Self::ResponseLoss => "response_loss",
            Self::Restart => "restart",
            Self::AckReplay => "ack_replay",
        }
    }
}

#[cfg(test)]
impl PrivacyScrubTestHooks {
    pub(super) fn with_boundaries(
        mut self,
        boundaries: impl IntoIterator<Item = PrivacyScrubBoundary>,
    ) -> Self {
        self.enabled.extend(boundaries);
        self
    }

    pub(super) fn control(&self, boundary: PrivacyScrubBoundary) -> &PrivacyScrubBoundaryControl {
        match boundary {
            PrivacyScrubBoundary::PreIntent => &self.pre_intent,
            PrivacyScrubBoundary::PostIntent => &self.post_intent,
            PrivacyScrubBoundary::EngineCommit => &self.engine_commit,
            PrivacyScrubBoundary::PreAck => &self.pre_ack,
            PrivacyScrubBoundary::ResponseLoss => &self.response_loss,
            PrivacyScrubBoundary::Restart => &self.restart,
            PrivacyScrubBoundary::AckReplay => &self.ack_replay,
        }
    }

    pub(super) async fn wait_at(&self, boundary: PrivacyScrubBoundary) {
        if !self.enabled.contains(&boundary) {
            return;
        }
        let control = self.control(boundary);
        control.observed.notify_one();
        control.release.notified().await;
    }
}

#[cfg(test)]
impl ImportTestHooks {
    pub(super) fn with_after_accepted_export(
        mut self,
        hook: impl Fn(&SpoolStore, Uuid) + Send + Sync + 'static,
    ) -> Self {
        self.after_accepted_export = Some(Arc::new(hook));
        self
    }

    pub(super) fn with_before_document_batch_write(
        mut self,
        hook: impl Fn(&[Document]) -> Result<(), FlapjackError> + Send + Sync + 'static,
    ) -> Self {
        self.before_document_batch_write = Some(Arc::new(hook));
        self
    }

    pub(super) fn with_after_document_batch_write(
        mut self,
        hook: impl Fn(&flapjack::types::TaskInfo) + Send + Sync + 'static,
    ) -> Self {
        self.after_document_batch_write = Some(Arc::new(hook));
        self
    }

    pub(super) fn with_before_activation(
        mut self,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.before_activation = Some(Arc::new(hook));
        self
    }

    pub(super) fn with_bulk_build_event_hook(
        mut self,
        hook: impl Fn(BulkBuildTestEvent) + Send + Sync + 'static,
    ) -> Self {
        self.bulk_build = self.bulk_build.with_event_hook(hook);
        self
    }

    pub(super) fn with_replacement_publication_fault(
        mut self,
        fault: PublicationFaultPoint,
    ) -> Self {
        self.bulk_build = self.bulk_build.with_replacement_publication_fault(fault);
        self
    }

    #[allow(dead_code)]
    pub(super) fn without_prepublication_validation(mut self) -> Self {
        self.bulk_build = self.bulk_build.without_prepublication_validation();
        self
    }

    /// Obstructs the sidecar write for selected derived replica names so a test
    /// can exercise post-activation failure without panicking the request.
    pub(super) fn with_before_replica_materialization(
        mut self,
        hook: impl Fn(&str) -> Result<(), FlapjackError> + Send + Sync + 'static,
    ) -> Self {
        self.before_replica_materialization = Some(Arc::new(hook));
        self
    }

    pub(super) fn with_before_activation_barrier(self, barrier: Arc<Barrier>) -> Self {
        self.with_before_activation(move || {
            barrier.wait();
        })
    }

    fn run_after_accepted_export(&self, spool: &SpoolStore, job_uuid: Uuid) {
        if let Some(hook) = &self.after_accepted_export {
            hook(spool, job_uuid);
        }
    }

    fn before_document_batch_write(&self) -> Option<BeforeDocumentBatchWriteHook> {
        self.before_document_batch_write.clone()
    }

    fn after_document_batch_write(&self) -> Option<AfterDocumentBatchWriteHook> {
        self.after_document_batch_write.clone()
    }

    fn run_before_activation(&self) {
        if let Some(hook) = &self.before_activation {
            hook();
        }
    }

    fn run_before_replica_materialization(&self, derived_name: &str) -> Result<(), FlapjackError> {
        if let Some(hook) = &self.before_replica_materialization {
            hook(derived_name)?;
        }
        Ok(())
    }

    fn bulk_build_hooks(&self) -> BulkBuildTestHooks {
        self.bulk_build.clone()
    }
}

pub(super) async fn import_from_source<R>(
    state_manager: &Arc<IndexManager>,
    target_index: String,
    publication_mode: MigrationPublicationMode,
    reader: &mut R,
) -> Result<Json<MigrateFromAlgoliaResponse>, MigrateError>
where
    R: MigrationSourceReader,
{
    let spool = spool_for_manager(state_manager)?;
    let job_uuid = Uuid::new_v4();
    spool
        .create_migration_phase(job_uuid)
        .map_err(spool_error)?;
    import_from_admitted_source_inner(
        state_manager,
        &spool,
        job_uuid,
        target_index,
        publication_mode,
        reader,
        #[cfg(test)]
        ImportTestHooks::default(),
    )
    .await
}

#[cfg(test)]
pub(super) async fn import_from_source_with_test_hooks<R>(
    state_manager: &Arc<IndexManager>,
    target_index: String,
    publication_mode: MigrationPublicationMode,
    reader: &mut R,
    hooks: ImportTestHooks,
) -> Result<Json<MigrateFromAlgoliaResponse>, MigrateError>
where
    R: MigrationSourceReader,
{
    let spool = spool_for_manager(state_manager)?;
    let job_uuid = Uuid::new_v4();
    spool
        .create_migration_phase(job_uuid)
        .map_err(spool_error)?;
    import_from_admitted_source_inner(
        state_manager,
        &spool,
        job_uuid,
        target_index,
        publication_mode,
        reader,
        hooks,
    )
    .await
}

#[allow(dead_code)]
pub(super) async fn import_from_admitted_source<R>(
    state_manager: &Arc<IndexManager>,
    job_uuid: Uuid,
    target_index: String,
    publication_mode: MigrationPublicationMode,
    reader: &mut R,
) -> Result<Json<MigrateFromAlgoliaResponse>, MigrateError>
where
    R: MigrationSourceReader,
{
    import_from_admitted_source_inner(
        state_manager,
        &spool_for_manager(state_manager)?,
        job_uuid,
        target_index,
        publication_mode,
        reader,
        #[cfg(test)]
        ImportTestHooks::default(),
    )
    .await
}

#[cfg(test)]
pub(super) async fn import_from_admitted_source_with_test_hooks<R>(
    state_manager: &Arc<IndexManager>,
    job_uuid: Uuid,
    target_index: String,
    publication_mode: MigrationPublicationMode,
    reader: &mut R,
    hooks: ImportTestHooks,
) -> Result<Json<MigrateFromAlgoliaResponse>, MigrateError>
where
    R: MigrationSourceReader,
{
    import_from_admitted_source_inner(
        state_manager,
        &spool_for_manager(state_manager)?,
        job_uuid,
        target_index,
        publication_mode,
        reader,
        hooks,
    )
    .await
}

pub(super) fn spool_for_manager(
    state_manager: &Arc<IndexManager>,
) -> Result<SpoolStore, MigrateError> {
    SpoolStore::new(&state_manager.base_path, SpoolLimits::default()).map_err(spool_error)
}

async fn import_from_admitted_source_inner<R>(
    state_manager: &Arc<IndexManager>,
    spool: &SpoolStore,
    job_uuid: Uuid,
    target_index: String,
    publication_mode: MigrationPublicationMode,
    reader: &mut R,
    #[cfg(test)] hooks: ImportTestHooks,
) -> Result<Json<MigrateFromAlgoliaResponse>, MigrateError>
where
    R: MigrationSourceReader,
{
    #[cfg(not(test))]
    let bulk_build = BulkBuildService::new(state_manager, spool, job_uuid, target_index.as_str());
    #[cfg(test)]
    let bulk_build = BulkBuildService::new(
        state_manager,
        spool,
        job_uuid,
        target_index.as_str(),
        hooks.bulk_build_hooks(),
    );
    let cancellation = bulk_build.cancellation();
    let export = settle_import_result(
        spool,
        job_uuid,
        export_algolia_source_for_import(spool, job_uuid, reader)
            .await
            .map_err(export_error),
    )?;
    #[cfg(test)]
    hooks.run_after_accepted_export(spool, export.job_uuid);
    settle_import_result(spool, job_uuid, cancellation.check())?;
    let publication = settle_import_result(spool, job_uuid, bulk_build.prepare_publication())?;

    let ((), publication) = abort_publication_on_error(
        spool,
        job_uuid,
        transition_import_phase(spool, job_uuid, MigrationPhase::Staging),
        publication,
    )?;
    let staging_result = stage_import_export(
        &bulk_build,
        spool,
        &publication,
        &export,
        &target_index,
        #[cfg(test)]
        hooks.clone(),
    )
    .await;
    let (staged, publication) =
        abort_publication_on_error(spool, job_uuid, staging_result, publication)?;

    let (reservation, publication) = abort_publication_on_error(
        spool,
        job_uuid,
        cancellation.check().and_then(|()| {
            ReplicaNameReservation::claim(
                &state_manager.base_path,
                staged
                    .replica_settings
                    .iter()
                    .map(|translation| translation.derived_entry.name()),
            )
        }),
        publication,
    )?;

    let ((), publication) = abort_publication_on_error(
        spool,
        job_uuid,
        transition_import_phase(spool, job_uuid, MigrationPhase::Activating),
        publication,
    )?;
    #[cfg(test)]
    hooks.run_before_activation();
    let ((), publication) = abort_publication_on_error(
        spool,
        job_uuid,
        wait_for_live_import_barrier(
            &export.source_index_name,
            job_uuid,
            LiveImportBarrier::PreActivation,
        ),
        publication,
    )?;
    // Entering `activate_create_only()` reaches `reserve_publication_target`,
    // the create-only point of no return. Before then, a cancellation may still
    // abort the unjournaled transaction; once journaled, `abort()` must refuse.
    let ((), publication) =
        abort_publication_on_error(spool, job_uuid, cancellation.check(), publication)?;
    settle_import_result(
        spool,
        job_uuid,
        bulk_build.activate(publication, publication_mode).await,
    )?;
    settle_import_result(
        spool,
        job_uuid,
        wait_for_live_import_barrier(
            &export.source_index_name,
            job_uuid,
            LiveImportBarrier::PostCommit,
        ),
    )?;
    // The primary is committed, so the claims are now the sidecar homes rather
    // than releasable reservations. Disarm before any further fallible step.
    reservation.disarm();
    settle_import_result(
        spool,
        job_uuid,
        refresh_target(state_manager, &target_index),
    )?;
    let activated_counts = settle_import_result(spool, job_uuid, bulk_build.activated_counts())?;

    let sidecar_warnings = materialize_replica_sidecars(
        state_manager,
        &target_index,
        &staged.replica_settings,
        #[cfg(test)]
        &hooks,
    );

    let response = settle_import_result(
        spool,
        job_uuid,
        activated_response(staged, activated_counts, publication_mode, sidecar_warnings),
    )?;
    // Carry the already computed activation facts into durable job state so the
    // async status endpoint can report them; `activated_response` remains the
    // sole owner of these counts and warnings.
    let outcome = import_outcome_from_response(&response);
    spool
        .record_import_outcome(job_uuid, outcome)
        .map_err(spool_error)?;
    spool
        .succeed_migration(job_uuid, None)
        .map_err(spool_error)?;
    let _ = spool.delete_export_artifacts(export.job_uuid, &export.source_identity_digest);
    Ok(Json(response))
}

/// Snapshot the successful import response into the spool-owned persistence type.
/// This copies the counts and warnings verbatim; it never recomputes them.
fn import_outcome_from_response(response: &MigrateFromAlgoliaResponse) -> MigrationImportOutcome {
    MigrationImportOutcome {
        settings_applied: response.settings,
        objects_imported: response.objects.imported,
        synonyms_imported: response.synonyms.imported,
        rules_imported: response.rules.imported,
        warnings: response
            .warnings
            .iter()
            .map(import_outcome_warning)
            .collect(),
    }
}

fn import_outcome_warning(warning: &MigrateWarning) -> MigrationImportWarning {
    MigrationImportWarning {
        code: warning.code.clone(),
        message: warning.message.clone(),
        resource: warning.resource.clone(),
        page_index: warning.page_index,
        item_index: warning.item_index,
        json_path: warning.json_path.clone(),
    }
}

async fn stage_import_export(
    bulk_build: &BulkBuildService<'_>,
    spool: &SpoolStore,
    publication: &PreStagedPublication,
    export: &AcceptedExport,
    target_index: &str,
    #[cfg(test)] hooks: ImportTestHooks,
) -> Result<StagedImport, MigrateError> {
    stage_export(
        bulk_build,
        spool,
        publication,
        StageExportInput {
            source_index_name: &export.source_index_name,
            target_index,
            job_uuid: export.job_uuid,
            replica_settings: export.replica_settings.clone(),
        },
        #[cfg(test)]
        hooks,
    )
    .await
}

pub(super) async fn stage_accepted_bulk_replace(
    bulk_build: &BulkBuildService<'_>,
    spool: &SpoolStore,
    publication: &PreStagedPublication,
    job_uuid: Uuid,
    target_index: &str,
) -> Result<BulkBuildCounts, MigrateError> {
    let staged = stage_export(
        bulk_build,
        spool,
        publication,
        StageExportInput {
            source_index_name: target_index,
            target_index,
            job_uuid,
            replica_settings: BTreeMap::new(),
        },
        #[cfg(test)]
        ImportTestHooks::default(),
    )
    .await?;
    Ok(staged.counts)
}

async fn stage_export(
    bulk_build: &BulkBuildService<'_>,
    spool: &SpoolStore,
    publication: &PreStagedPublication,
    input: StageExportInput<'_>,
    #[cfg(test)] hooks: ImportTestHooks,
) -> Result<StagedImport, MigrateError> {
    let cancellation = bulk_build.cancellation();
    cancellation.check()?;
    let staging = bulk_build.create_staging(publication)?;
    let staging_manager = staging.manager();
    let staging_tenant = staging.tenant();
    cancellation.check()?;
    let accepted = spool
        .accepted_artifacts(input.job_uuid)
        .map_err(spool_error)?;
    cancellation.check()?;
    let translated_settings =
        match translate_accepted_spool_settings(&accepted).map_err(spool_error)? {
            SettingsTranslationOutcome::Translated(settings) => settings,
            SettingsTranslationOutcome::Rejected(report) => {
                return Err(rejected_translation(report));
            }
        };
    cancellation.check()?;
    persist_translated_settings(staging_manager, staging_tenant, &translated_settings).map_err(
        |_| json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
    )?;
    let mut instrumentation = TranslationSessionInstrumentation::default();
    #[cfg(test)]
    let (document_sender, document_writer) = bulk_build.spawn_document_writer(
        &staging,
        hooks.before_document_batch_write(),
        hooks.after_document_batch_write(),
    );
    #[cfg(not(test))]
    let (document_sender, document_writer) = bulk_build.spawn_document_writer(&staging);
    let translation_result = translate_accepted_spool_payload(
        accepted,
        input.source_index_name.to_string(),
        input.target_index.to_string(),
        input.replica_settings,
        &mut instrumentation,
        || cancellation.cancel_requested(),
        |batch| document_sender.send(batch),
    );
    drop(document_sender);
    bulk_build
        .join_document_writer(document_writer)
        .map_err(flapjack_error)?;
    let outcome = translation_result.map_err(translation_error)?;
    let translated = match outcome {
        TranslationOutcome::Translated(translated) => translated,
        TranslationOutcome::Rejected(report) => {
            return Err(rejected_translation(report));
        }
    };

    cancellation.check()?;
    let report = translated.report.clone();
    let replica_settings = translated.bundle.replica_settings.clone();
    let source_identity_validation = translated.source_identity_validation;
    persist_translated_resources(staging_manager, staging_tenant, translated).map_err(|_| {
        json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    })?;
    let counts = bulk_build
        .finish_staging(&staging, source_identity_validation)
        .await?;
    Ok(StagedImport {
        counts,
        report,
        replica_settings,
    })
}

struct StageExportInput<'a> {
    source_index_name: &'a str,
    target_index: &'a str,
    job_uuid: Uuid,
    replica_settings: BTreeMap<String, Value>,
}

fn rejected_translation(report: impl serde::Serialize) -> MigrateError {
    json_error_parts(
        StatusCode::BAD_REQUEST,
        serde_json::to_string(&report).unwrap_or_else(|_| {
            "Algolia migration import translation rejected source payload".to_string()
        }),
    )
}

fn persist_translated_settings(
    staging_manager: &IndexManager,
    staging_tenant: &str,
    settings: &flapjack::index::settings::IndexSettings,
) -> Result<(), crate::error_response::HandlerError> {
    persist_index_settings(staging_manager, staging_tenant, settings)
}

fn persist_translated_resources(
    staging_manager: &IndexManager,
    staging_tenant: &str,
    translated: Box<super::translation::TranslatedSpoolPayload>,
) -> Result<(), crate::error_response::HandlerError> {
    persist_translated_settings(staging_manager, staging_tenant, &translated.bundle.settings)?;
    save_resource_batch::<RuleStore, _>(
        staging_manager,
        staging_tenant,
        translated.bundle.rules,
        true,
    )
    .map_err(crate::error_response::HandlerError::from)?;
    save_resource_batch::<SynonymStore, _>(
        staging_manager,
        staging_tenant,
        translated.bundle.synonyms,
        true,
    )
    .map_err(crate::error_response::HandlerError::from)?;
    Ok(())
}

fn activated_response(
    staged: StagedImport,
    activated: BulkBuildCounts,
    publication_mode: MigrationPublicationMode,
    sidecar_warnings: Vec<MigrateWarning>,
) -> Result<MigrateFromAlgoliaResponse, MigrateError> {
    // Replacement activation replays acknowledged mutations from
    // `(baseline_seq, write_watermark]` after installing the staged source.
    // Its reopened target can therefore legitimately differ from the source
    // object count. Create-only publication has no replay window.
    if (matches!(publication_mode, MigrationPublicationMode::CreateOnly)
        && activated.documents != staged.counts.documents)
        || activated.rules != staged.counts.rules
        || activated.synonyms != staged.counts.synonyms
    {
        return Err(json_error_parts(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error",
        ));
    }
    Ok(MigrateFromAlgoliaResponse {
        status: "complete".to_string(),
        settings: staged.counts.settings,
        synonyms: MigrateCount {
            imported: staged.counts.synonyms,
        },
        rules: MigrateCount {
            imported: staged.counts.rules,
        },
        objects: MigrateCount {
            imported: staged.counts.documents,
        },
        // Translation warnings first, then runtime sidecar warnings in replica
        // order, so an existing warning never shifts position.
        warnings: migrate_warnings(&staged.report)
            .into_iter()
            .chain(sidecar_warnings)
            .collect(),
        // Synchronous import has no durable async job id to resolve.
        task_id: 0,
    })
}

/// Attempts each replica sidecar independently, collecting warnings for any
/// that fail after the primary is already committed.
fn materialize_replica_sidecars(
    state_manager: &Arc<IndexManager>,
    target_index: &str,
    replica_settings: &[super::translation::ReplicaSettingsTranslation],
    #[cfg(test)] hooks: &ImportTestHooks,
) -> Vec<MigrateWarning> {
    let mut sidecar_warnings = Vec::new();
    for replica_translation in replica_settings {
        let derived_name = replica_translation.derived_entry.name();
        #[cfg(test)]
        let obstruction = hooks.run_before_replica_materialization(derived_name);
        #[cfg(not(test))]
        let obstruction: Result<(), FlapjackError> = Ok(());

        let materialized = obstruction.and_then(|()| {
            crate::handlers::replicas::persist_replica_primary_link_with_settings(
                state_manager,
                target_index,
                &replica_translation.derived_entry,
                Some(&replica_translation.settings),
            )
        });
        if materialized.is_err() {
            sidecar_warnings.push(replica_sidecar_not_materialized(derived_name, target_index));
        }
    }
    sidecar_warnings
}

/// Claims each derived replica directory before the primary is published, so a
/// concurrent import cannot take a name this attempt is about to use.
///
/// Release is driven by `Drop` rather than explicit calls because cleanup must
/// survive every early return *and* every unwind between reservation and
/// activation; an explicit call site can only cover the returns it knows about.
struct ReplicaNameReservation {
    claimed: Vec<PathBuf>,
    armed: bool,
}

impl ReplicaNameReservation {
    /// Claims `base_path/<derived_name>` for each name, releasing everything
    /// already claimed if any name is taken.
    fn claim<'a>(
        base_path: &Path,
        derived_names: impl Iterator<Item = &'a str>,
    ) -> Result<Self, MigrateError> {
        let mut reservation = Self {
            claimed: Vec::new(),
            armed: true,
        };
        for derived_name in derived_names {
            validate_index_name(derived_name).map_err(flapjack_error)?;
            let replica_path = base_path.join(derived_name);
            // create_dir (never create_dir_all) makes the claim atomic: the
            // AlreadyExists arm *is* the collision check, so there is no
            // window between testing for the name and taking it.
            match std::fs::create_dir(&replica_path) {
                Ok(()) => reservation.claimed.push(replica_path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(flapjack_error(FlapjackError::IndexAlreadyExists(
                        derived_name.to_string(),
                    )));
                }
                Err(_) => {
                    return Err(json_error_parts(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Internal server error",
                    ));
                }
            }
        }
        Ok(reservation)
    }

    /// Hands the claimed directories to the sidecar writer once the primary is
    /// committed: from here they are replica homes, not releasable claims.
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for ReplicaNameReservation {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for replica_path in &self.claimed {
            // remove_dir only succeeds while the claim is still empty, so a
            // directory that somehow gained content is never destroyed here.
            let _ = std::fs::remove_dir(replica_path);
        }
    }
}

/// Reports a replica whose sidecar could not be written after the primary was
/// already committed.
///
/// This is a runtime warning built here rather than a translation-report entry:
/// the failure happens after translation has finished, and `translation_report`
/// remains the sole owner of report-derived warnings.
fn replica_sidecar_not_materialized(derived_name: &str, primary_index: &str) -> MigrateWarning {
    MigrateWarning {
        code: "ReplicaSidecarNotMaterialized".to_string(),
        message: format!(
            "Replica '{derived_name}' was not materialized. The imported primary '{primary_index}' \
             is committed and unaffected. To recreate the replica link, re-POST the complete \
             replicas list to /1/indexes/{primary_index}/settings. That repair restores the \
             virtual replica link only; replica-specific translated settings that never reached \
             disk are not recovered by it."
        ),
        resource: "Settings".to_string(),
        page_index: None,
        item_index: None,
        json_path: format!("replicas.{derived_name}"),
    }
}

fn migrate_warnings(report: &TranslationReport) -> Vec<MigrateWarning> {
    report.entries.iter().filter_map(migrate_warning).collect()
}

fn migrate_warning(entry: &TranslationReportEntry) -> Option<MigrateWarning> {
    let message = warning_message(entry.code)?;
    Some(MigrateWarning {
        code: report_variant_string(entry.code),
        message: message.to_string(),
        resource: report_variant_string(entry.resource),
        page_index: entry.page_index,
        item_index: entry.item_index,
        json_path: entry.json_path.clone(),
    })
}

fn report_variant_string<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

pub(super) fn refresh_target(
    manager: &Arc<IndexManager>,
    target_index: &str,
) -> Result<(), MigrateError> {
    manager
        .unload(&target_index.to_string())
        .map_err(flapjack_error)?;
    manager.invalidate_settings_cache(target_index);
    manager.invalidate_rules_cache(target_index);
    manager.invalidate_synonyms_cache(target_index);
    manager.invalidate_facet_cache(target_index);
    Ok(())
}

fn transition_import_phase(
    spool: &SpoolStore,
    job_uuid: Uuid,
    phase: MigrationPhase,
) -> Result<(), MigrateError> {
    spool
        .transition_migration_phase(job_uuid, phase)
        .map(|_| ())
        .map_err(spool_error)
}

#[derive(Clone, Copy)]
pub(super) enum LiveImportBarrier {
    PreActivation,
    PostCommit,
}

impl LiveImportBarrier {
    #[cfg(test)]
    fn env_names(self) -> (&'static str, &'static str) {
        match self {
            Self::PreActivation => (
                LIVE_IMPORT_PRE_ACTIVATION_SOURCE_ENV,
                LIVE_IMPORT_PRE_ACTIVATION_BARRIER_DIR_ENV,
            ),
            Self::PostCommit => (
                LIVE_IMPORT_POST_COMMIT_SOURCE_ENV,
                LIVE_IMPORT_POST_COMMIT_BARRIER_DIR_ENV,
            ),
        }
    }
}

#[cfg(not(test))]
fn wait_for_live_import_barrier(
    _source_name: &str,
    _job_uuid: Uuid,
    _barrier: LiveImportBarrier,
) -> Result<(), MigrateError> {
    Ok(())
}

#[cfg(test)]
fn wait_for_live_import_barrier(
    source_name: &str,
    job_uuid: Uuid,
    barrier: LiveImportBarrier,
) -> Result<(), MigrateError> {
    wait_for_live_import_barrier_with_timeout(
        source_name,
        job_uuid,
        barrier,
        LIVE_IMPORT_BARRIER_TIMEOUT,
    )
}

#[cfg(test)]
pub(super) fn wait_for_live_import_barrier_with_timeout(
    source_name: &str,
    job_uuid: Uuid,
    barrier: LiveImportBarrier,
    timeout: Duration,
) -> Result<(), MigrateError> {
    let (source_env, dir_env) = barrier.env_names();
    let Ok(target_source) = env::var(source_env) else {
        return Ok(());
    };
    if target_source != source_name {
        return Ok(());
    }
    let Ok(barrier_dir) = env::var(dir_env) else {
        return Ok(());
    };
    if barrier_dir.is_empty() {
        return Ok(());
    }

    let barrier_dir = PathBuf::from(barrier_dir);
    fs::create_dir_all(&barrier_dir).map_err(|_| live_import_barrier_error())?;
    fs::write(
        barrier_dir.join(LIVE_IMPORT_BARRIER_OBSERVED_FILE),
        job_uuid.to_string(),
    )
    .map_err(|_| live_import_barrier_error())?;

    let release_file = barrier_dir.join(LIVE_IMPORT_BARRIER_RELEASE_FILE);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if release_file.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }

    Err(live_import_barrier_error())
}

#[cfg(test)]
fn live_import_barrier_error() -> MigrateError {
    json_error_parts(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Migration import live barrier was not released",
    )
}

fn settle_import_result<T>(
    spool: &SpoolStore,
    job_uuid: Uuid,
    result: Result<T, MigrateError>,
) -> Result<T, MigrateError> {
    result.map_err(|error| settle_failed_or_cancelled_migration(spool, job_uuid, error))
}

pub(super) fn abort_publication_on_error<T>(
    spool: &SpoolStore,
    job_uuid: Uuid,
    result: Result<T, MigrateError>,
    publication: PreStagedPublication,
) -> Result<(T, PreStagedPublication), MigrateError> {
    match result {
        Ok(value) => Ok((value, publication)),
        Err(error) => {
            let _ = publication.abort();
            Err(settle_failed_or_cancelled_migration(spool, job_uuid, error))
        }
    }
}

fn settle_failed_or_cancelled_migration(
    spool: &SpoolStore,
    job_uuid: Uuid,
    error: MigrateError,
) -> MigrateError {
    if is_migration_cancelled_error(&error) {
        return cancel_migration(spool, job_uuid, error);
    }
    fail_migration(spool, job_uuid, error)
}

fn cancel_migration(spool: &SpoolStore, job_uuid: Uuid, error: MigrateError) -> MigrateError {
    match spool.cancel_migration(job_uuid).and_then(|_| {
        spool
            .delete_export_artifacts_if_present(job_uuid)
            .map(|_| ())
    }) {
        Ok(_) => error,
        Err(settlement_error) => spool_error(settlement_error),
    }
}

fn fail_migration(spool: &SpoolStore, job_uuid: Uuid, error: MigrateError) -> MigrateError {
    tracing::error!(
        %job_uuid,
        status = %error.0,
        body = %error.1.0,
        "Algolia migration import failed"
    );
    match spool.fail_migration(job_uuid) {
        Ok(_) => error,
        Err(settlement_error) => spool_error(settlement_error),
    }
}

fn export_error(error: ExportError) -> MigrateError {
    match error {
        ExportError::Source(error) => algolia_error(error),
        ExportError::Spool(error) => spool_error(error),
        ExportError::Cancelled => migration_cancelled_error(),
    }
}

fn translation_error(error: TranslationStreamError<SendError<Vec<Document>>>) -> MigrateError {
    match error {
        TranslationStreamError::Spool(error) => spool_error(error),
        TranslationStreamError::Cancelled => migration_cancelled_error(),
        TranslationStreamError::Emit(_) => {
            json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        }
        TranslationStreamError::Identity(error) => {
            json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, error.safe_message())
        }
    }
}

fn is_migration_cancelled_error(error: &MigrateError) -> bool {
    error.1 .0.get("code").and_then(serde_json::Value::as_str)
        == Some(super::MIGRATION_CANCELLED_CODE)
}

#[derive(Debug, Clone)]
struct StagedImport {
    counts: BulkBuildCounts,
    report: TranslationReport,
    replica_settings: Vec<super::translation::ReplicaSettingsTranslation>,
}

#[cfg(test)]
mod tests {
    use super::{translation_error, ReplicaNameReservation};
    use crate::handlers::migration::source_identity_partitions::SourceIdentityError;
    use crate::handlers::migration::translation::TranslationStreamError;
    use axum::http::StatusCode;
    use std::io;
    use tempfile::TempDir;

    #[test]
    fn replica_name_reservation_rejects_path_traversal_names() {
        let tmp = TempDir::new().unwrap();
        let escaped_path = tmp.path().parent().unwrap().join("escape");

        let error = ReplicaNameReservation::claim(tmp.path(), ["../escape"].into_iter())
            .err()
            .expect("reservation must reject path traversal names");

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(error.1 .0["status"], 400);
        assert!(
            !escaped_path.exists(),
            "reservation must not create directories outside the base path"
        );
    }

    #[test]
    fn translation_identity_infrastructure_error_is_scrubbed_http_failure() {
        let error = translation_error(TranslationStreamError::Identity(SourceIdentityError::Io(
            io::Error::other("secret-spool-path"),
        )));

        assert_eq!(error.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error.1 .0["status"], 500);
        assert_eq!(
            error.1 .0["message"],
            "source identity partition I/O failed"
        );
        assert!(!error.1 .0.to_string().contains("secret-spool-path"));
    }
}
