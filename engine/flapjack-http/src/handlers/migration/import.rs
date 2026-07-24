use super::export::{export_algolia_source_for_import, AcceptedExport, ExportError};
use super::source_reader::MigrationSourceReader;
use super::spool::{MigrationPhase, SpoolError, SpoolErrorKind, SpoolLimits, SpoolStore};
use super::translation::{
    translate_accepted_spool_payload, translate_accepted_spool_settings, warning_message,
    SettingsTranslationOutcome, TranslationOutcome, TranslationReport, TranslationReportEntry,
    TranslationSessionInstrumentation, TranslationStreamError,
};
use super::{
    algolia_error, MigrateCount, MigrateError, MigrateFromAlgoliaResponse, MigrateWarning,
    MigrationPublicationMode,
};
use crate::error_response::{json_error_parts, json_error_parts_with_code};
use crate::handlers::index_resource_store::{save_resource_batch, IndexResourceStore};
use crate::handlers::settings::persist_index_settings;
use axum::{http::StatusCode, Json};
use flapjack::error::FlapjackError;
use flapjack::index::manager::publication::{
    PreStagedActivationError, PreStagedPublication, PublicationTarget,
};
use flapjack::index::manager::validate_index_name;
use flapjack::index::rules::RuleStore;
use flapjack::index::synonyms::SynonymStore;
use flapjack::types::Document;
use flapjack::IndexManager;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SendError, SyncSender};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Barrier;
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use uuid::Uuid;

const MIGRATION_CANCELLED_CODE: &str = "migration_cancelled";
const MIGRATION_CANCELLED_MESSAGE: &str = "Algolia migration cancellation was requested";
pub(super) const LIVE_IMPORT_PRE_ACTIVATION_SOURCE_ENV: &str =
    "FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_SOURCE";
pub(super) const LIVE_IMPORT_PRE_ACTIVATION_BARRIER_DIR_ENV: &str =
    "FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_BARRIER_DIR";
pub(super) const LIVE_IMPORT_POST_COMMIT_SOURCE_ENV: &str =
    "FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_POST_COMMIT_SOURCE";
pub(super) const LIVE_IMPORT_POST_COMMIT_BARRIER_DIR_ENV: &str =
    "FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_POST_COMMIT_BARRIER_DIR";
pub(super) const LIVE_IMPORT_BARRIER_OBSERVED_FILE: &str = "observed";
pub(super) const LIVE_IMPORT_BARRIER_RELEASE_FILE: &str = "release";
const LIVE_IMPORT_BARRIER_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(test)]
type AfterAcceptedExportHook = Arc<dyn Fn(&SpoolStore, Uuid) + Send + Sync>;
#[cfg(test)]
type BeforeDocumentBatchWriteHook =
    Arc<dyn Fn(&[Document]) -> Result<(), FlapjackError> + Send + Sync>;
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
    before_activation: Option<BeforeActivationHook>,
    before_replica_materialization: Option<BeforeReplicaMaterializationHook>,
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

    pub(super) fn with_before_activation(
        mut self,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.before_activation = Some(Arc::new(hook));
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

    fn run_before_document_batch_write(&self, batch: &[Document]) -> Result<(), FlapjackError> {
        if let Some(hook) = &self.before_document_batch_write {
            hook(batch)?;
        }
        Ok(())
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
        MigrationPublicationMode::CreateOnly,
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
        MigrationPublicationMode::CreateOnly,
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
    let cancellation = MigrationCancellationCheck::new(spool, job_uuid);
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
    let publication = prepare_import_publication(state_manager, &target_index, spool, job_uuid)?;

    let ((), publication) = abort_publication_on_error(
        spool,
        job_uuid,
        transition_import_phase(spool, job_uuid, MigrationPhase::Staging),
        publication,
    )?;
    let staging_result = stage_import_export(
        spool,
        &publication,
        &export,
        &target_index,
        cancellation.clone(),
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
    activate_staged_publication(
        state_manager,
        spool,
        job_uuid,
        publication,
        &target_index,
        publication_mode,
    )
    .await?;
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
        activated_response(
            state_manager,
            &target_index,
            staged,
            publication_mode,
            sidecar_warnings,
        ),
    )?;
    spool.succeed_migration(job_uuid).map_err(spool_error)?;
    let _ = spool.delete_export_artifacts(export.job_uuid, &export.source_identity_digest);
    Ok(Json(response))
}

async fn activate_staged_publication(
    state_manager: &Arc<IndexManager>,
    spool: &SpoolStore,
    job_uuid: Uuid,
    publication: PreStagedPublication,
    target_index: &str,
    publication_mode: MigrationPublicationMode,
) -> Result<(), MigrateError> {
    match publication_mode {
        MigrationPublicationMode::CreateOnly => {
            settle_import_result(
                spool,
                job_uuid,
                publication.activate_create_only().map_err(activation_error),
            )?;
        }
        MigrationPublicationMode::ReplaceExisting { staging_baseline } => {
            settle_import_result(
                spool,
                job_uuid,
                state_manager
                    .replace_index_contents_from_pre_staged(
                        publication,
                        target_index,
                        staging_baseline,
                    )
                    .await
                    .map_err(flapjack_error),
            )?;
        }
    }
    Ok(())
}

fn prepare_import_publication(
    state_manager: &Arc<IndexManager>,
    target_index: &str,
    spool: &SpoolStore,
    job_uuid: Uuid,
) -> Result<PreStagedPublication, MigrateError> {
    let target = settle_import_result(
        spool,
        job_uuid,
        PublicationTarget::new(target_index.to_string()).map_err(flapjack_error),
    )?;
    settle_import_result(
        spool,
        job_uuid,
        transition_import_phase(spool, job_uuid, MigrationPhase::Preparing),
    )?;
    let publication = settle_import_result(
        spool,
        job_uuid,
        PreStagedPublication::prepare(&state_manager.base_path, target).map_err(flapjack_error),
    )?;
    settle_import_result(
        spool,
        job_uuid,
        spool
            .record_async_publication_transaction_if_present(
                job_uuid,
                publication.transaction_id().clone(),
            )
            .map_err(spool_error),
    )?;
    Ok(publication)
}

async fn stage_import_export(
    spool: &SpoolStore,
    publication: &PreStagedPublication,
    export: &AcceptedExport,
    target_index: &str,
    cancellation: MigrationCancellationCheck,
    #[cfg(test)] hooks: ImportTestHooks,
) -> Result<StagedImport, MigrateError> {
    stage_export(
        spool,
        publication,
        StageExportInput {
            source_index_name: &export.source_index_name,
            target_index,
            job_uuid: export.job_uuid,
            replica_settings: export.replica_settings.clone(),
        },
        cancellation,
        #[cfg(test)]
        hooks,
    )
    .await
}

async fn stage_export(
    spool: &SpoolStore,
    publication: &PreStagedPublication,
    input: StageExportInput<'_>,
    cancellation: MigrationCancellationCheck,
    #[cfg(test)] hooks: ImportTestHooks,
) -> Result<StagedImport, MigrateError> {
    cancellation.check()?;
    let staging_parent = publication
        .paths()
        .staging
        .parent()
        .expect("publication staging path should have a transaction namespace");
    let staging_tenant = publication
        .paths()
        .staging
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?
        .to_string();
    let staging_manager = IndexManager::new(staging_parent);
    cancellation.check()?;
    staging_manager
        .create_tenant(&staging_tenant)
        .map_err(flapjack_error)?;
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
    persist_translated_settings(&staging_manager, &staging_tenant, &translated_settings).map_err(
        |_| json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
    )?;
    let mut instrumentation = TranslationSessionInstrumentation::default();
    #[cfg(test)]
    let (document_sender, document_writer) = spawn_staging_document_writer(
        Arc::clone(&staging_manager),
        staging_tenant.clone(),
        cancellation.clone(),
        hooks,
    );
    #[cfg(not(test))]
    let (document_sender, document_writer) = spawn_staging_document_writer(
        Arc::clone(&staging_manager),
        staging_tenant.clone(),
        cancellation.clone(),
    );
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
    join_document_writer(document_writer).map_err(flapjack_error)?;
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
    persist_translated_resources(&staging_manager, &staging_tenant, translated).map_err(|_| {
        json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    })?;
    staging_manager
        .unload(&staging_tenant)
        .map_err(flapjack_error)?;
    cancellation.check()?;
    let counts = verify_staged_counts(&staging_manager, &staging_tenant, input.target_index)?;
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

fn spawn_staging_document_writer(
    staging_manager: Arc<IndexManager>,
    staging_tenant: String,
    cancellation: MigrationCancellationCheck,
    #[cfg(test)] hooks: ImportTestHooks,
) -> (
    SyncSender<Vec<Document>>,
    JoinHandle<Result<(), FlapjackError>>,
) {
    let (document_sender, document_receiver) = std::sync::mpsc::sync_channel::<Vec<Document>>(1);
    let writer = std::thread::spawn(move || -> Result<(), FlapjackError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        while let Ok(batch) = document_receiver.recv() {
            #[cfg(test)]
            hooks.run_before_document_batch_write(&batch)?;
            if cancellation
                .cancel_requested()
                .map_err(|error| FlapjackError::Io(error.to_string()))?
            {
                continue;
            }
            runtime
                .block_on(staging_manager.add_documents_durable(&staging_tenant, batch))
                .map(|_| ())?;
        }
        Ok(())
    });
    (document_sender, writer)
}

fn join_document_writer(
    document_writer: JoinHandle<Result<(), FlapjackError>>,
) -> Result<(), FlapjackError> {
    document_writer
        .join()
        .map_err(|_| FlapjackError::Io("migration staging document writer panicked".to_string()))?
}

fn verify_staged_counts(
    staging_manager: &Arc<IndexManager>,
    staging_tenant: &str,
    target_index: &str,
) -> Result<StagedCounts, MigrateError> {
    let documents = document_count(staging_manager, staging_tenant)?;
    let rules = resource_count::<RuleStore>(staging_manager, staging_tenant)?;
    let synonyms = resource_count::<SynonymStore>(staging_manager, staging_tenant)?;
    let settings = staging_manager.get_settings(staging_tenant).is_some();
    if !settings {
        return Err(json_error_parts(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error",
        ));
    }
    tracing::debug!(
        target_index,
        documents,
        rules,
        synonyms,
        "validated staged migration import counts"
    );
    Ok(StagedCounts {
        settings,
        documents,
        rules,
        synonyms,
    })
}

fn activated_response(
    manager: &Arc<IndexManager>,
    target_index: &str,
    staged: StagedImport,
    publication_mode: MigrationPublicationMode,
    sidecar_warnings: Vec<MigrateWarning>,
) -> Result<MigrateFromAlgoliaResponse, MigrateError> {
    let objects = document_count(manager, target_index)?;
    let rules = resource_count::<RuleStore>(manager, target_index)?;
    let synonyms = resource_count::<SynonymStore>(manager, target_index)?;
    // Replacement activation replays acknowledged mutations from
    // `(baseline_seq, write_watermark]` after installing the staged source.
    // Its reopened target can therefore legitimately differ from the source
    // object count. Create-only publication has no replay window.
    if (matches!(publication_mode, MigrationPublicationMode::CreateOnly)
        && objects != staged.counts.documents)
        || rules != staged.counts.rules
        || synonyms != staged.counts.synonyms
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

fn refresh_target(manager: &Arc<IndexManager>, target_index: &str) -> Result<(), MigrateError> {
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

fn abort_publication_on_error<T>(
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

fn document_count(manager: &Arc<IndexManager>, index_name: &str) -> Result<usize, MigrateError> {
    let index = manager.get_or_load(index_name).map_err(flapjack_error)?;
    Ok(index.reader().searcher().num_docs() as usize)
}

fn resource_count<S>(manager: &Arc<IndexManager>, index_name: &str) -> Result<usize, MigrateError>
where
    S: IndexResourceStore,
{
    Ok(
        crate::handlers::index_resource_store::load_existing_store::<S>(manager, index_name)
            .map_err(flapjack_error)?
            .map(|store| store.count())
            .unwrap_or(0),
    )
}

fn export_error(error: ExportError) -> MigrateError {
    match error {
        ExportError::Source(error) => algolia_error(error),
        ExportError::Spool(error) => spool_error(error),
        ExportError::Cancelled => migration_cancelled_error(),
    }
}

pub(super) fn spool_error(error: SpoolError) -> MigrateError {
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
        | SpoolErrorKind::SourceIdentityMismatch
        | SpoolErrorKind::ResourceVerificationFailed
        | SpoolErrorKind::ResourceComplete
        | SpoolErrorKind::ResourcesIncomplete
        | SpoolErrorKind::CancelRequested
        | SpoolErrorKind::JobTerminal
        | SpoolErrorKind::JobNotAccepted
        | SpoolErrorKind::UnsupportedArtifactKind
        | SpoolErrorKind::InvalidPhaseTransition => StatusCode::BAD_REQUEST,
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

fn translation_error(error: TranslationStreamError<SendError<Vec<Document>>>) -> MigrateError {
    match error {
        TranslationStreamError::Spool(error) => spool_error(error),
        TranslationStreamError::Cancelled => migration_cancelled_error(),
        TranslationStreamError::Emit(_) => {
            json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        }
    }
}

fn migration_cancelled_error() -> MigrateError {
    json_error_parts_with_code(
        StatusCode::CONFLICT,
        MIGRATION_CANCELLED_CODE,
        MIGRATION_CANCELLED_MESSAGE,
    )
}

fn is_migration_cancelled_error(error: &MigrateError) -> bool {
    error.1 .0.get("code").and_then(serde_json::Value::as_str) == Some(MIGRATION_CANCELLED_CODE)
}

#[derive(Clone)]
struct MigrationCancellationCheck {
    spool: SpoolStore,
    job_uuid: Uuid,
}

impl MigrationCancellationCheck {
    fn new(spool: &SpoolStore, job_uuid: Uuid) -> Self {
        Self {
            spool: spool.clone(),
            job_uuid,
        }
    }

    fn cancel_requested(&self) -> Result<bool, SpoolError> {
        self.spool.cancel_requested(self.job_uuid)
    }

    fn check(&self) -> Result<(), MigrateError> {
        match self.cancel_requested() {
            Ok(false) => Ok(()),
            Ok(true) => Err(migration_cancelled_error()),
            Err(error) => Err(spool_error(error)),
        }
    }
}

fn activation_error(error: PreStagedActivationError) -> MigrateError {
    let mut source = error.source();
    while let Some(error_source) = source {
        if let Some(error) = error_source.downcast_ref::<FlapjackError>() {
            if matches!(error, FlapjackError::IndexAlreadyExists(_)) {
                return flapjack_error(error.clone());
            }
        }
        source = error_source.source();
    }
    json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
}

fn flapjack_error(error: FlapjackError) -> MigrateError {
    json_error_parts(error.status_code(), error.api_message())
}

#[derive(Debug, Clone, Copy)]
struct StagedCounts {
    settings: bool,
    documents: usize,
    rules: usize,
    synonyms: usize,
}

#[derive(Debug, Clone)]
struct StagedImport {
    counts: StagedCounts,
    report: TranslationReport,
    replica_settings: Vec<super::translation::ReplicaSettingsTranslation>,
}

#[cfg(test)]
mod tests {
    use super::ReplicaNameReservation;
    use axum::http::StatusCode;
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
}
