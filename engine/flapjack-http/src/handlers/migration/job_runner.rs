use super::source_reader::MigrationSourceReader;
use super::spool::{
    AsyncMigrationMetadata, AsyncMigrationPublicationSemantic, MigrationDisposition,
    MigrationPhaseRecord, SpoolError, SpoolLimits, SpoolStore, SPOOL_ROOT,
};
use super::{algolia_error, import, migration_capacity_exhausted};
use super::{
    resume_spool_error, AsyncMigrationSourceProvider, AsyncMigrationSubmitPayload, MigrateError,
    MigrateFromAlgoliaRequest, MigrationPublicationMode,
};
use dashmap::DashMap;
use flapjack::index::manager::publication::{
    abort_unjournaled_publication, PublicationPhase, PublicationRepairReport, PublicationTarget,
    PublicationTargetDisposition,
};
use flapjack::index::replica::parse_replica_entry;
use flapjack::index::settings::IndexSettings;
use flapjack::IndexManager;
use flapjack_replication::manager::ReplicationManager;
use std::io;
use std::sync::Arc;
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use uuid::Uuid;

pub const DEFAULT_ASYNC_MIGRATION_CAPACITY: usize = 2;

struct ActiveMigrationTask {
    token: Uuid,
    handle: JoinHandle<()>,
    operation: ActiveMigrationOperation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveMigrationOperation {
    Standard,
    Resume,
}

fn remove_active_if_token(
    active: &DashMap<Uuid, ActiveMigrationTask>,
    job_uuid: Uuid,
    token: Uuid,
) {
    active.remove_if(&job_uuid, |_, task| task.token == token);
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct MigrationJobRunner {
    manager: Arc<IndexManager>,
    replication_manager: Option<Arc<ReplicationManager>>,
    capacity: Arc<Semaphore>,
    active: Arc<DashMap<Uuid, ActiveMigrationTask>>,
    #[cfg(test)]
    bulk_replace_test_hooks: Arc<std::sync::Mutex<super::bulk_build::BulkBuildTestHooks>>,
}

impl MigrationJobRunner {
    pub fn new(
        manager: Arc<IndexManager>,
        replication_manager: Option<Arc<ReplicationManager>>,
        capacity: usize,
    ) -> Self {
        Self {
            manager,
            replication_manager,
            capacity: Arc::new(Semaphore::new(capacity)),
            active: Arc::new(DashMap::new()),
            #[cfg(test)]
            bulk_replace_test_hooks: Arc::new(std::sync::Mutex::new(Default::default())),
        }
    }

    pub(super) fn acquire_bulk_replace_permit(
        &self,
    ) -> Result<OwnedSemaphorePermit, tokio::sync::TryAcquireError> {
        self.capacity.clone().try_acquire_owned()
    }

    pub(super) fn spawn_bulk_replace(
        &self,
        job_uuid: Uuid,
        target_index: String,
        publication_mode: MigrationPublicationMode,
        permit: OwnedSemaphorePermit,
    ) {
        let manager = Arc::clone(&self.manager);
        let monitor_manager = Arc::clone(&self.manager);
        let active = Arc::clone(&self.active);
        let active_token = Uuid::new_v4();
        #[cfg(test)]
        let test_hooks = self.bulk_replace_test_hooks.lock().unwrap().clone();
        let (published, published_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            super::bulk_replace::run_bulk_replace(
                &manager,
                job_uuid,
                target_index,
                publication_mode,
                #[cfg(test)]
                test_hooks,
            )
            .await
        });
        let monitor = tokio::spawn(async move {
            let task_result = task.await;
            if !matches!(&task_result, Ok(Ok(()))) {
                tracing::error!(%job_uuid, result = ?task_result, "async bulk replacement failed");
                if let Ok(spool) = import::spool_for_manager(&monitor_manager) {
                    if spool.cancel_requested(job_uuid).unwrap_or(false) {
                        let _ = spool.cancel_migration(job_uuid);
                    } else {
                        let _ = spool.fail_migration(job_uuid);
                    }
                }
            }
            drop(permit);
            let _ = published_rx.await;
            remove_active_if_token(&active, job_uuid, active_token);
        });
        self.active.insert(
            job_uuid,
            ActiveMigrationTask {
                token: active_token,
                handle: monitor,
                operation: ActiveMigrationOperation::Standard,
            },
        );
        let _ = published.send(());
    }

    #[cfg(test)]
    pub(crate) fn set_bulk_replace_prepublication_hook_for_test(
        &self,
        hook: impl Fn() + Send + Sync + 'static,
    ) {
        let hooks =
            super::bulk_build::BulkBuildTestHooks::default().with_event_hook(move |event| {
                if event == super::bulk_build::BulkBuildTestEvent::PrepublicationValidationStarting
                {
                    hook();
                }
            });
        *self.bulk_replace_test_hooks.lock().unwrap() = hooks;
    }

    /// Admit and spawn an async Algolia import, returning the durable admission
    /// record committed by `SpoolStore`.
    #[allow(dead_code)]
    pub(super) async fn submit_algolia_import<F, R>(
        &self,
        payload: MigrateFromAlgoliaRequest,
        source_factory: F,
    ) -> Result<(Uuid, MigrationPhaseRecord), MigrateError>
    where
        F: FnOnce(
            &MigrateFromAlgoliaRequest,
        ) -> Result<R, super::algolia_client::AlgoliaClientError>,
        R: MigrationSourceReader + Send + 'static,
    {
        self.submit_algolia_import_for_owner(payload, None, source_factory)
            .await
    }

    pub(super) async fn submit_algolia_import_for_owner<F, R>(
        &self,
        payload: MigrateFromAlgoliaRequest,
        authenticated_app_id: Option<String>,
        source_factory: F,
    ) -> Result<(Uuid, MigrationPhaseRecord), MigrateError>
    where
        F: FnOnce(
            &MigrateFromAlgoliaRequest,
        ) -> Result<R, super::algolia_client::AlgoliaClientError>,
        R: MigrationSourceReader + Send + 'static,
    {
        self.submit_source_import_for_owner(
            AsyncMigrationSourceProvider::Algolia,
            payload,
            authenticated_app_id,
            source_factory,
        )
        .await
    }

    /// Admit and spawn an async Meilisearch import through the one shared
    /// submission core. This mirrors `submit_algolia_import_for_owner` and
    /// differs only in the request payload type and source-reader construction
    /// closure; production submits through `submit_source_import_for_owner`
    /// directly, so this named wrapper exists for provider-parity test intake.
    #[cfg(test)]
    pub(super) async fn submit_meilisearch_import_for_owner<F, R>(
        &self,
        payload: super::MigrateFromMeilisearchRequest,
        authenticated_app_id: Option<String>,
        source_factory: F,
    ) -> Result<(Uuid, MigrationPhaseRecord), MigrateError>
    where
        F: FnOnce(
            &super::MigrateFromMeilisearchRequest,
        ) -> Result<R, super::algolia_client::AlgoliaClientError>,
        R: MigrationSourceReader + Send + 'static,
    {
        self.submit_source_import_for_owner(
            AsyncMigrationSourceProvider::Meilisearch,
            payload,
            authenticated_app_id,
            source_factory,
        )
        .await
    }

    pub(super) async fn submit_source_import_for_owner<P, F, R>(
        &self,
        source_provider: AsyncMigrationSourceProvider,
        payload: P,
        authenticated_app_id: Option<String>,
        source_factory: F,
    ) -> Result<(Uuid, MigrationPhaseRecord), MigrateError>
    where
        P: AsyncMigrationSubmitPayload,
        F: FnOnce(&P) -> Result<R, super::algolia_client::AlgoliaClientError>,
        R: MigrationSourceReader + Send + 'static,
    {
        let admitted = payload.admit_async(&self.manager, self.replication_manager.as_ref())?;
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| migration_capacity_exhausted())?;
        let reader =
            construct_reader_preserving_spool_precedence(&self.manager, &payload, source_factory)?;
        let spool = import::spool_for_manager(&self.manager)?;
        let job_uuid = Uuid::new_v4();
        let publication_semantic = async_publication_semantic(admitted.publication_mode);
        let phase_record = spool
            .create_async_migration_admission_for_provider_owner(
                job_uuid,
                &admitted.target_index,
                authenticated_app_id.as_deref(),
                source_provider,
                publication_semantic,
            )
            .map_err(super::spool_error)?;

        self.spawn_import(
            job_uuid,
            import::SourceImportRequest {
                expected_provider: source_provider,
                target_index: admitted.target_index,
                publication_mode: admitted.publication_mode,
            },
            reader,
            permit,
        );
        Ok((job_uuid, phase_record))
    }

    pub(super) async fn resume_algolia_import<F, R>(
        &self,
        job_uuid: Uuid,
        checkpoint_handle: String,
        payload: MigrateFromAlgoliaRequest,
        source_factory: F,
    ) -> Result<MigrationPhaseRecord, MigrateError>
    where
        F: FnOnce(
            &MigrateFromAlgoliaRequest,
        ) -> Result<R, super::algolia_client::AlgoliaClientError>,
        R: MigrationSourceReader + Send + 'static,
    {
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| migration_capacity_exhausted())?;
        let spool = import::spool_for_manager(&self.manager)?;
        let metadata = spool
            .read_async_migration_metadata(job_uuid)
            .map_err(resume_spool_error)?;
        let target_index = metadata.target_index.clone();
        let publication_mode = self.resume_publication_mode(&metadata)?;
        let mut reader = source_factory(&payload).map_err(algolia_error)?;
        // Algolia is the only enabled resume path, so the resume request context
        // admits that provider explicitly rather than inferring it from the reader.
        let expected_provider = AsyncMigrationSourceProvider::Algolia;
        let checkpoint = super::export::claim_validated_algolia_resume(
            &spool,
            &mut reader,
            &checkpoint_handle,
            expected_provider,
        )
        .await
        .map_err(import::export_error)?;
        let phase_record = spool
            .read_migration_phase(job_uuid)
            .map_err(resume_spool_error)?;
        self.spawn_resume_import(
            job_uuid,
            import::SourceImportRequest {
                expected_provider,
                target_index,
                publication_mode,
            },
            reader,
            checkpoint,
            permit,
        );
        Ok(phase_record)
    }

    fn resume_publication_mode(
        &self,
        metadata: &AsyncMigrationMetadata,
    ) -> Result<MigrationPublicationMode, MigrateError> {
        match metadata.publication_semantic {
            AsyncMigrationPublicationSemantic::CreateOnly => {
                Ok(MigrationPublicationMode::CreateOnly)
            }
            AsyncMigrationPublicationSemantic::ReplaceExisting => {
                let staging_baseline = self
                    .manager
                    .capture_replacement_staging_baseline(&metadata.target_index)
                    .map_err(|error| {
                        super::json_error_parts(
                            axum::http::StatusCode::BAD_REQUEST,
                            error.to_string(),
                        )
                    })?;
                Ok(MigrationPublicationMode::ReplaceExisting { staging_baseline })
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn submit_algolia_import_with_test_hooks<F, R>(
        &self,
        payload: MigrateFromAlgoliaRequest,
        source_factory: F,
        hooks: import::ImportTestHooks,
    ) -> Result<(Uuid, MigrationPhaseRecord), MigrateError>
    where
        F: FnOnce(
            &MigrateFromAlgoliaRequest,
        ) -> Result<R, super::algolia_client::AlgoliaClientError>,
        R: MigrationSourceReader + Send + 'static,
    {
        self.submit_algolia_import_with_test_hooks_for_owner(payload, None, source_factory, hooks)
            .await
    }

    #[cfg(test)]
    pub(super) async fn submit_algolia_import_with_test_hooks_for_owner<F, R>(
        &self,
        payload: MigrateFromAlgoliaRequest,
        authenticated_app_id: Option<String>,
        source_factory: F,
        hooks: import::ImportTestHooks,
    ) -> Result<(Uuid, MigrationPhaseRecord), MigrateError>
    where
        F: FnOnce(
            &MigrateFromAlgoliaRequest,
        ) -> Result<R, super::algolia_client::AlgoliaClientError>,
        R: MigrationSourceReader + Send + 'static,
    {
        let admitted = payload.admit_async(&self.manager, self.replication_manager.as_ref())?;
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| migration_capacity_exhausted())?;
        let reader =
            construct_reader_preserving_spool_precedence(&self.manager, &payload, source_factory)?;
        let spool = import::spool_for_manager(&self.manager)?;
        let job_uuid = Uuid::new_v4();
        let publication_semantic = async_publication_semantic(admitted.publication_mode);
        let phase_record = spool
            .create_async_migration_admission_for_owner(
                job_uuid,
                &admitted.target_index,
                authenticated_app_id.as_deref(),
                publication_semantic,
            )
            .map_err(super::spool_error)?;

        self.spawn_import_with_hooks(
            job_uuid,
            import::SourceImportRequest {
                expected_provider: AsyncMigrationSourceProvider::Algolia,
                target_index: admitted.target_index,
                publication_mode: admitted.publication_mode,
            },
            reader,
            permit,
            hooks,
        );
        Ok((job_uuid, phase_record))
    }

    #[allow(dead_code)]
    fn spawn_import<R>(
        &self,
        job_uuid: Uuid,
        request: import::SourceImportRequest,
        mut reader: R,
        permit: OwnedSemaphorePermit,
    ) where
        R: MigrationSourceReader + Send + 'static,
    {
        let import_manager = Arc::clone(&self.manager);
        let monitor_manager = Arc::clone(&self.manager);
        let active = Arc::clone(&self.active);
        let active_token = Uuid::new_v4();
        let (published, published_rx) = oneshot::channel();
        let import_task = tokio::spawn(async move {
            import::import_from_admitted_source(&import_manager, job_uuid, request, &mut reader)
                .await
        });
        let monitor = tokio::spawn(async move {
            let result = import_task.await;
            if let Err(error) = result {
                tracing::error!(
                    %job_uuid,
                    error = %error,
                    "async Algolia migration task failed before settling"
                );
                if let Ok(spool) = import::spool_for_manager(&monitor_manager) {
                    if spool
                        .resumable_export_handle(job_uuid)
                        .ok()
                        .flatten()
                        .is_none()
                    {
                        let _ = spool.fail_migration(job_uuid);
                    }
                }
            }
            drop(permit);
            let _ = published_rx.await;
            remove_active_if_token(&active, job_uuid, active_token);
        });
        self.active.insert(
            job_uuid,
            ActiveMigrationTask {
                token: active_token,
                handle: monitor,
                operation: ActiveMigrationOperation::Standard,
            },
        );
        let _ = published.send(());
    }

    fn spawn_resume_import<R>(
        &self,
        job_uuid: Uuid,
        request: import::SourceImportRequest,
        mut reader: R,
        checkpoint: super::spool::ExportCheckpoint,
        permit: OwnedSemaphorePermit,
    ) where
        R: MigrationSourceReader + Send + 'static,
    {
        self.active.remove(&job_uuid);
        let import_manager = Arc::clone(&self.manager);
        let monitor_manager = Arc::clone(&self.manager);
        let active = Arc::clone(&self.active);
        let active_token = Uuid::new_v4();
        let (published, published_rx) = oneshot::channel();
        let import_task = tokio::spawn(async move {
            import::import_from_claimed_resume(
                &import_manager,
                job_uuid,
                request,
                &mut reader,
                checkpoint,
            )
            .await
        });
        let monitor = tokio::spawn(async move {
            let result = import_task.await;
            if let Err(error) = result {
                tracing::error!(
                    %job_uuid,
                    error = %error,
                    "async Algolia migration resume task failed before settling"
                );
                if let Ok(spool) = import::spool_for_manager(&monitor_manager) {
                    if spool
                        .resumable_export_handle(job_uuid)
                        .ok()
                        .flatten()
                        .is_none()
                    {
                        let _ = spool.fail_migration(job_uuid);
                    }
                }
            }
            drop(permit);
            let _ = published_rx.await;
            remove_active_if_token(&active, job_uuid, active_token);
        });
        self.active.insert(
            job_uuid,
            ActiveMigrationTask {
                token: active_token,
                handle: monitor,
                operation: ActiveMigrationOperation::Resume,
            },
        );
        let _ = published.send(());
    }

    #[cfg(test)]
    fn spawn_import_with_hooks<R>(
        &self,
        job_uuid: Uuid,
        request: import::SourceImportRequest,
        mut reader: R,
        permit: OwnedSemaphorePermit,
        hooks: import::ImportTestHooks,
    ) where
        R: MigrationSourceReader + Send + 'static,
    {
        let import_manager = Arc::clone(&self.manager);
        let monitor_manager = Arc::clone(&self.manager);
        let active = Arc::clone(&self.active);
        let active_token = Uuid::new_v4();
        let (published, published_rx) = oneshot::channel();
        let import_task = tokio::spawn(async move {
            import::import_from_admitted_source_with_test_hooks(
                &import_manager,
                job_uuid,
                request,
                &mut reader,
                hooks,
            )
            .await
        });
        let monitor = tokio::spawn(async move {
            let result = import_task.await;
            if let Err(error) = result {
                tracing::error!(
                    %job_uuid,
                    error = %error,
                    "async Algolia migration task failed before settling"
                );
                if let Ok(spool) = import::spool_for_manager(&monitor_manager) {
                    if spool
                        .resumable_export_handle(job_uuid)
                        .ok()
                        .flatten()
                        .is_none()
                    {
                        let _ = spool.fail_migration(job_uuid);
                    }
                }
            }
            drop(permit);
            let _ = published_rx.await;
            remove_active_if_token(&active, job_uuid, active_token);
        });
        self.active.insert(
            job_uuid,
            ActiveMigrationTask {
                token: active_token,
                handle: monitor,
                operation: ActiveMigrationOperation::Standard,
            },
        );
        let _ = published.send(());
    }

    pub(crate) async fn recover_async_jobs_before_serve(
        &self,
        publication_reports: &[PublicationRepairReport],
    ) -> Result<(), String> {
        let spool = SpoolStore::new(&self.manager.base_path, SpoolLimits::default())
            .map_err(recovery_spool_error)?;
        spool
            .recover_async_admissions()
            .map_err(recovery_spool_error)?;
        for job_uuid in spool.job_uuids().map_err(recovery_spool_error)? {
            let Some(metadata) = spool
                .read_async_migration_metadata_if_exists(job_uuid)
                .map_err(recovery_spool_error)?
            else {
                continue;
            };
            self.recover_async_job(&spool, job_uuid, &metadata, publication_reports)
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn drain_active_imports(&self) {
        let job_uuids = self
            .active
            .iter()
            .map(|entry| *entry.key())
            .collect::<Vec<_>>();
        for job_uuid in job_uuids {
            if let Some((_, task)) = self.active.remove(&job_uuid) {
                let _ = task.handle.await;
            }
        }
    }

    pub(super) fn resume_claim_is_active(&self, job_uuid: Uuid) -> bool {
        self.active
            .get(&job_uuid)
            .is_some_and(|task| task.operation == ActiveMigrationOperation::Resume)
    }

    async fn recover_async_job(
        &self,
        spool: &SpoolStore,
        job_uuid: Uuid,
        metadata: &AsyncMigrationMetadata,
        publication_reports: &[PublicationRepairReport],
    ) -> Result<(), String> {
        let phase = spool
            .read_migration_phase(job_uuid)
            .map_err(recovery_spool_error)?;
        if phase.disposition != MigrationDisposition::Running || phase.terminal_at.is_some() {
            return Ok(());
        }
        if spool
            .read_privacy_scrub_intent_if_exists(job_uuid)
            .map_err(recovery_spool_error)?
            .is_some()
        {
            return Ok(());
        }
        if phase.cancel_requested {
            return self
                .recover_cancel_requested_async_job(spool, job_uuid, metadata, publication_reports)
                .await;
        }
        if prepublication_export_can_be_interrupted(&phase, metadata) {
            // Both resume advertisement and the resume claim reject an
            // unsupported spool format, so preserving such an export as
            // resumable would leave the migration non-terminal and unresumable
            // forever. Settle it before any lifecycle work.
            if !spool
                .export_spool_format_is_supported(job_uuid)
                .map_err(recovery_spool_error)?
            {
                spool
                    .fail_migration(job_uuid)
                    .map_err(recovery_spool_error)?;
                return Ok(());
            }
            spool.recover().map_err(recovery_spool_error)?;
            if spool
                .recover_export_interruption(job_uuid)
                .map_err(recovery_spool_error)?
            {
                return Ok(());
            }
        }
        if metadata.publication_semantic == AsyncMigrationPublicationSemantic::ReplaceExisting {
            return self.recover_replacement_async_job(
                spool,
                job_uuid,
                metadata,
                publication_reports,
            );
        }
        self.recover_create_async_job(spool, job_uuid, metadata, publication_reports)
            .await
    }

    async fn recover_create_async_job(
        &self,
        spool: &SpoolStore,
        job_uuid: Uuid,
        metadata: &AsyncMigrationMetadata,
        publication_reports: &[PublicationRepairReport],
    ) -> Result<(), String> {
        let Some(transaction_id) = &metadata.publication_transaction_id else {
            spool
                .fail_migration(job_uuid)
                .map_err(recovery_spool_error)?;
            return Ok(());
        };
        let report = proven_committed_report(metadata, publication_reports)?;
        if report.transaction_id.as_ref() != Some(transaction_id) {
            return Err(publication_transaction_mismatch(metadata, job_uuid));
        }
        validate_recovery_index_name(&metadata.target_index, "target")?;
        self.remove_job_owned_replicas(&metadata.target_index)
            .await?;
        self.manager
            .delete_tenant(&metadata.target_index)
            .await
            .map_err(|error| {
                format!(
                    "async migration recovery failed deleting job-owned target '{}': {error}",
                    metadata.target_index
                )
            })?;
        spool
            .fail_migration(job_uuid)
            .map_err(recovery_spool_error)?;
        Ok(())
    }

    fn recover_replacement_async_job(
        &self,
        spool: &SpoolStore,
        job_uuid: Uuid,
        metadata: &AsyncMigrationMetadata,
        publication_reports: &[PublicationRepairReport],
    ) -> Result<(), String> {
        let Some(transaction_id) = &metadata.publication_transaction_id else {
            spool
                .fail_migration(job_uuid)
                .map_err(recovery_spool_error)?;
            return Ok(());
        };
        if let Some(report) = publication_report_for_target(metadata, publication_reports) {
            if report.transaction_id.as_ref() == Some(transaction_id)
                && report_is_journaled_loadable(report)
            {
                spool
                    .fail_migration(job_uuid)
                    .map_err(recovery_spool_error)?;
                return Ok(());
            }
            if report
                .transaction_id
                .as_ref()
                .is_some_and(|id| id != transaction_id)
                && !report_is_committed_loadable(report)
            {
                return Err(publication_transaction_mismatch(metadata, job_uuid));
            }
        }
        abort_async_publication_transaction(&self.manager, metadata, job_uuid, transaction_id)?;
        spool
            .fail_migration(job_uuid)
            .map_err(recovery_spool_error)?;
        Ok(())
    }

    async fn recover_cancel_requested_async_job(
        &self,
        spool: &SpoolStore,
        job_uuid: Uuid,
        metadata: &AsyncMigrationMetadata,
        publication_reports: &[PublicationRepairReport],
    ) -> Result<(), String> {
        if metadata.publication_semantic == AsyncMigrationPublicationSemantic::ReplaceExisting {
            return self.recover_cancel_requested_replacement_async_job(
                spool,
                job_uuid,
                metadata,
                publication_reports,
            );
        }
        self.recover_cancel_requested_create_async_job(
            spool,
            job_uuid,
            metadata,
            publication_reports,
        )
        .await
    }

    async fn recover_cancel_requested_create_async_job(
        &self,
        spool: &SpoolStore,
        job_uuid: Uuid,
        metadata: &AsyncMigrationMetadata,
        publication_reports: &[PublicationRepairReport],
    ) -> Result<(), String> {
        let Some(transaction_id) = &metadata.publication_transaction_id else {
            spool
                .cancel_migration(job_uuid)
                .map_err(recovery_spool_error)?;
            return Ok(());
        };
        if let Some(report) = publication_reports
            .iter()
            .find(|report| report.target.as_str() == metadata.target_index)
        {
            if report.transaction_id.as_ref() != Some(transaction_id) {
                return Err(publication_transaction_mismatch(metadata, job_uuid));
            }
            if report_is_committed_loadable(report) {
                spool
                    .succeed_migration(job_uuid, None)
                    .map_err(recovery_spool_error)?;
                return Ok(());
            }
        }
        abort_async_publication_transaction(&self.manager, metadata, job_uuid, transaction_id)?;
        spool
            .cancel_migration(job_uuid)
            .map_err(recovery_spool_error)?;
        Ok(())
    }

    fn recover_cancel_requested_replacement_async_job(
        &self,
        spool: &SpoolStore,
        job_uuid: Uuid,
        metadata: &AsyncMigrationMetadata,
        publication_reports: &[PublicationRepairReport],
    ) -> Result<(), String> {
        let Some(transaction_id) = &metadata.publication_transaction_id else {
            spool
                .cancel_migration(job_uuid)
                .map_err(recovery_spool_error)?;
            return Ok(());
        };
        if let Some(report) = publication_report_for_target(metadata, publication_reports) {
            if report.transaction_id.as_ref() == Some(transaction_id) {
                if report_is_committed_loadable(report) {
                    spool
                        .succeed_migration(job_uuid, None)
                        .map_err(recovery_spool_error)?;
                    return Ok(());
                }
                return Err(publication_transaction_mismatch(metadata, job_uuid));
            }
            if report.transaction_id.is_some() && !report_is_committed_loadable(report) {
                return Err(publication_transaction_mismatch(metadata, job_uuid));
            }
        }
        abort_async_publication_transaction(&self.manager, metadata, job_uuid, transaction_id)?;
        spool
            .cancel_migration(job_uuid)
            .map_err(recovery_spool_error)?;
        Ok(())
    }

    async fn remove_job_owned_replicas(&self, primary: &str) -> Result<(), String> {
        for replica_name in replica_names_for_primary(&self.manager, primary)? {
            if replica_is_job_owned(&self.manager, &replica_name, primary)? {
                self.manager.delete_tenant(&replica_name).await.map_err(|error| {
                    format!(
                        "async migration recovery failed deleting job-owned replica '{replica_name}': {error}"
                    )
                })?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn active_count_for_test(&self) -> usize {
        self.active.len()
    }
}

fn proven_committed_report<'a>(
    metadata: &AsyncMigrationMetadata,
    publication_reports: &'a [PublicationRepairReport],
) -> Result<&'a PublicationRepairReport, String> {
    let report = publication_report_for_target(metadata, publication_reports).ok_or_else(|| {
        format!(
            "async migration recovery refused target '{}': missing publication repair report",
            metadata.target_index
        )
    })?;
    if !report_is_committed_loadable(report) {
        return Err(format!(
            "async migration recovery refused target '{}': publication evidence is not a committed loadable target",
            metadata.target_index
        ));
    }
    Ok(report)
}

fn report_is_committed_loadable(report: &PublicationRepairReport) -> bool {
    report.disposition == PublicationTargetDisposition::Loadable
        && report.phase == Some(PublicationPhase::Committed)
}

fn report_is_journaled_loadable(report: &PublicationRepairReport) -> bool {
    report.disposition == PublicationTargetDisposition::Loadable && report.phase.is_some()
}

fn publication_report_for_target<'a>(
    metadata: &AsyncMigrationMetadata,
    publication_reports: &'a [PublicationRepairReport],
) -> Option<&'a PublicationRepairReport> {
    publication_reports
        .iter()
        .find(|report| report.target.as_str() == metadata.target_index)
}

fn publication_transaction_mismatch(metadata: &AsyncMigrationMetadata, job_uuid: Uuid) -> String {
    format!(
        "async migration recovery refused target '{}' for job {}: publication transaction mismatch",
        metadata.target_index, job_uuid
    )
}

fn abort_async_publication_transaction(
    manager: &Arc<IndexManager>,
    metadata: &AsyncMigrationMetadata,
    job_uuid: Uuid,
    transaction_id: &flapjack::index::manager::publication::PublicationTransactionId,
) -> Result<(), String> {
    let target = PublicationTarget::new(metadata.target_index.clone()).map_err(|error| {
        format!(
            "async migration recovery refused target '{}' for job {}: {error}",
            metadata.target_index, job_uuid
        )
    })?;
    abort_unjournaled_publication(&manager.base_path, target, transaction_id).map_err(|error| {
        format!(
            "async migration recovery failed aborting unjournaled publication '{}' for job {}: {error}",
            metadata.target_index, job_uuid
        )
    })
}

fn async_publication_semantic(
    publication_mode: MigrationPublicationMode,
) -> AsyncMigrationPublicationSemantic {
    match publication_mode {
        MigrationPublicationMode::CreateOnly => AsyncMigrationPublicationSemantic::CreateOnly,
        MigrationPublicationMode::ReplaceExisting { .. } => {
            AsyncMigrationPublicationSemantic::ReplaceExisting
        }
    }
}

fn construct_reader_preserving_spool_precedence<P, F, R>(
    manager: &Arc<IndexManager>,
    payload: &P,
    source_factory: F,
) -> Result<R, MigrateError>
where
    F: FnOnce(&P) -> Result<R, super::algolia_client::AlgoliaClientError>,
{
    match source_factory(payload) {
        Ok(reader) => Ok(reader),
        Err(source_error) => {
            if spool_root_entry_is_present(manager) {
                import::spool_for_manager(manager)?;
            } else {
                SpoolStore::probe_initial_creation(&manager.base_path)
                    .map_err(super::spool_error)?;
            }
            Err(algolia_error(source_error))
        }
    }
}

fn spool_root_entry_is_present(manager: &Arc<IndexManager>) -> bool {
    match manager.base_path.join(SPOOL_ROOT).symlink_metadata() {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

fn replica_names_for_primary(
    manager: &Arc<IndexManager>,
    primary: &str,
) -> Result<Vec<String>, String> {
    validate_recovery_index_name(primary, "primary")?;
    let settings_path = manager.base_path.join(primary).join("settings.json");
    let settings = IndexSettings::load(&settings_path).map_err(|error| {
        format!("async migration recovery could not read primary settings for '{primary}': {error}")
    })?;
    settings
        .replicas
        .unwrap_or_default()
        .into_iter()
        .map(|entry| {
            let name = parse_replica_entry(&entry)
                .map(|parsed| parsed.name().to_string())
                .map_err(|error| {
                    format!(
                        "async migration recovery refused primary '{primary}': invalid replica entry '{entry}': {error}"
                    )
                })?;
            validate_recovery_index_name(&name, "replica")?;
            Ok(name)
        })
        .collect()
}

fn replica_is_job_owned(
    manager: &Arc<IndexManager>,
    replica_name: &str,
    primary: &str,
) -> Result<bool, String> {
    validate_recovery_index_name(replica_name, "replica")?;
    validate_recovery_index_name(primary, "primary")?;
    let replica_path = manager.base_path.join(replica_name);
    if !replica_path.exists() {
        return Ok(false);
    }
    if directory_is_empty(&replica_path).map_err(|error| {
        format!("async migration recovery could not inspect replica '{replica_name}': {error}")
    })? {
        return Ok(true);
    }
    let settings_path = replica_path.join("settings.json");
    if !settings_path.exists() || replica_path.join("meta.json").exists() {
        return Ok(false);
    }
    let settings = IndexSettings::load(&settings_path).map_err(|error| {
        format!("async migration recovery could not read replica '{replica_name}': {error}")
    })?;
    Ok(settings.primary.as_deref() == Some(primary))
}

fn validate_recovery_index_name(name: &str, role: &str) -> Result<(), String> {
    PublicationTarget::new(name.to_string())
        .map(|_| ())
        .map_err(|error| format!("async migration recovery refused {role} '{name}': {error}"))
}

fn directory_is_empty(path: &std::path::Path) -> io::Result<bool> {
    Ok(std::fs::read_dir(path)?.next().is_none())
}

fn prepublication_export_can_be_interrupted(
    phase: &MigrationPhaseRecord,
    metadata: &AsyncMigrationMetadata,
) -> bool {
    phase.phase == super::spool::MigrationPhase::Exporting
        && phase.disposition == MigrationDisposition::Running
        && phase.terminal_at.is_none()
        && !phase.cancel_requested
        && metadata.publication_transaction_id.is_none()
        && metadata.source_provider == AsyncMigrationSourceProvider::Algolia
        && metadata.operation_kind == super::spool::AsyncMigrationOperationKind::SourceImport
}

fn recovery_spool_error(error: SpoolError) -> String {
    format!("async migration recovery spool error: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::migration::algolia_client::{AlgoliaClientError, AlgoliaErrorKind};
    use crate::handlers::migration::source_reader::{
        MigrationSourceReader, SourceConfigurationConsumer, SourceDocumentPageConsumer,
        SourceFuture, SourceObservation,
    };
    use tempfile::TempDir;

    struct UnusedReader;

    fn migration_runner_for_test(manager: Arc<IndexManager>) -> MigrationJobRunner {
        MigrationJobRunner::new(manager, None, 1)
    }

    fn valid_algolia_payload() -> MigrateFromAlgoliaRequest {
        MigrateFromAlgoliaRequest {
            app_id: "app".to_string(),
            api_key: "key".to_string(),
            source_index: "products".to_string(),
            target_index: Some("shop".to_string()),
            overwrite: false,
        }
    }

    fn assert_sanitized_spool_error(error: &MigrateError) {
        assert_eq!(error.0, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error.1 .0["status"], 500);
        assert_eq!(error.1 .0["message"], "Internal server error");
    }

    #[cfg(unix)]
    fn create_dangling_spool_root(temp_dir: &TempDir) {
        std::os::unix::fs::symlink(
            temp_dir.path().join("missing-spool-target"),
            temp_dir.path().join(SPOOL_ROOT),
        )
        .unwrap();
    }

    #[cfg(unix)]
    fn make_data_root_unwritable(temp_dir: &TempDir) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    }

    #[cfg(unix)]
    fn restore_data_root_permissions(temp_dir: &TempDir) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn source_reader_failure() -> AlgoliaClientError {
        AlgoliaClientError::new(
            AlgoliaErrorKind::Transport,
            "source reader construction failed",
        )
    }

    impl MigrationSourceReader for UnusedReader {
        fn source_provider(&self) -> AsyncMigrationSourceProvider {
            AsyncMigrationSourceProvider::Algolia
        }

        fn source_name(&self) -> &str {
            "unused"
        }

        fn observe_quiescent_source(&mut self) -> SourceFuture<'_, SourceObservation> {
            unreachable!("source reader construction fails before async import starts")
        }

        fn read_configuration<'a>(
            &'a mut self,
            _consume: &'a mut SourceConfigurationConsumer<'a>,
        ) -> SourceFuture<'a, ()> {
            unreachable!("source reader construction fails before async import starts")
        }

        fn read_document_records<'a>(
            &'a mut self,
            _consume_page: &'a mut SourceDocumentPageConsumer<'a>,
        ) -> SourceFuture<'a, ()> {
            unreachable!("source reader construction fails before async import starts")
        }
    }

    #[tokio::test]
    async fn source_factory_failure_does_not_persist_hidden_async_job() {
        let temp_dir = TempDir::new().unwrap();
        let manager = Arc::new(IndexManager::new(temp_dir.path()));
        let runner = migration_runner_for_test(Arc::clone(&manager));

        let error = runner
            .submit_algolia_import(valid_algolia_payload(), |_| {
                Err::<UnusedReader, _>(source_reader_failure())
            })
            .await
            .expect_err("reader construction failure should reject submission");

        assert_eq!(error.0, axum::http::StatusCode::BAD_GATEWAY);
        assert!(
            !manager.base_path.join(SPOOL_ROOT).exists(),
            "source reader construction failure must not create the migration spool root"
        );
        let spool = import::spool_for_manager(&manager).expect("spool store should open");
        assert!(
            spool
                .job_uuids()
                .expect("job listing should succeed")
                .is_empty(),
            "a submission that never returned 202 must not persist a hidden async job"
        );
        assert_eq!(runner.active_count_for_test(), 0);
    }

    #[tokio::test]
    async fn source_factory_failure_with_hooks_does_not_persist_hidden_async_job() {
        let temp_dir = TempDir::new().unwrap();
        let manager = Arc::new(IndexManager::new(temp_dir.path()));
        let runner = migration_runner_for_test(Arc::clone(&manager));

        let error = runner
            .submit_algolia_import_with_test_hooks(
                valid_algolia_payload(),
                |_| Err::<UnusedReader, _>(source_reader_failure()),
                import::ImportTestHooks::default(),
            )
            .await
            .expect_err("reader construction failure should reject submission");

        assert_eq!(error.0, axum::http::StatusCode::BAD_GATEWAY);
        assert!(
            !manager.base_path.join(SPOOL_ROOT).exists(),
            "hook-path source reader construction failure must not create the migration spool root"
        );
        let spool = import::spool_for_manager(&manager).expect("spool store should open");
        assert!(
            spool
                .job_uuids()
                .expect("job listing should succeed")
                .is_empty(),
            "a hook-path submission that never returned 202 must not persist a hidden async job"
        );
        assert_eq!(runner.active_count_for_test(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_factory_failure_with_clean_unwritable_root_preserves_spool_failure_precedence()
    {
        let temp_dir = TempDir::new().unwrap();
        let manager = Arc::new(IndexManager::new(temp_dir.path()));
        let runner = migration_runner_for_test(Arc::clone(&manager));
        make_data_root_unwritable(&temp_dir);

        let result = runner
            .submit_algolia_import(valid_algolia_payload(), |_| {
                Err::<UnusedReader, _>(source_reader_failure())
            })
            .await;
        restore_data_root_permissions(&temp_dir);
        let error = result
            .expect_err("clean-root spool outage should win over source construction failure");

        assert_sanitized_spool_error(&error);
        assert!(!manager.base_path.join(SPOOL_ROOT).exists());
        assert_eq!(runner.active_count_for_test(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_factory_failure_with_hooks_and_clean_unwritable_root_preserves_spool_failure_precedence(
    ) {
        let temp_dir = TempDir::new().unwrap();
        let manager = Arc::new(IndexManager::new(temp_dir.path()));
        let runner = migration_runner_for_test(Arc::clone(&manager));
        make_data_root_unwritable(&temp_dir);

        let result = runner
            .submit_algolia_import_with_test_hooks(
                valid_algolia_payload(),
                |_| Err::<UnusedReader, _>(source_reader_failure()),
                import::ImportTestHooks::default(),
            )
            .await;
        restore_data_root_permissions(&temp_dir);
        let error = result.expect_err(
            "hook-path clean-root spool outage should win over source construction failure",
        );

        assert_sanitized_spool_error(&error);
        assert!(!manager.base_path.join(SPOOL_ROOT).exists());
        assert_eq!(runner.active_count_for_test(), 0);
    }

    #[tokio::test]
    async fn source_factory_failure_preserves_spool_failure_precedence() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(SPOOL_ROOT), b"not a directory").unwrap();
        let manager = Arc::new(IndexManager::new(temp_dir.path()));
        let runner = migration_runner_for_test(Arc::clone(&manager));

        let error = runner
            .submit_algolia_import(valid_algolia_payload(), |_| {
                Err::<UnusedReader, _>(source_reader_failure())
            })
            .await
            .expect_err("spool outage should win over source construction failure");

        assert_sanitized_spool_error(&error);
        assert_eq!(runner.active_count_for_test(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_factory_failure_with_dangling_spool_symlink_preserves_spool_failure_precedence()
    {
        let temp_dir = TempDir::new().unwrap();
        create_dangling_spool_root(&temp_dir);
        let manager = Arc::new(IndexManager::new(temp_dir.path()));
        let runner = migration_runner_for_test(Arc::clone(&manager));

        let error = runner
            .submit_algolia_import(valid_algolia_payload(), |_| {
                Err::<UnusedReader, _>(source_reader_failure())
            })
            .await
            .expect_err("dangling spool outage should win over source construction failure");

        assert_sanitized_spool_error(&error);
        assert_eq!(runner.active_count_for_test(), 0);
    }

    #[tokio::test]
    async fn source_factory_failure_with_hooks_preserves_spool_failure_precedence() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(SPOOL_ROOT), b"not a directory").unwrap();
        let manager = Arc::new(IndexManager::new(temp_dir.path()));
        let runner = migration_runner_for_test(Arc::clone(&manager));

        let error = runner
            .submit_algolia_import_with_test_hooks(
                valid_algolia_payload(),
                |_| Err::<UnusedReader, _>(source_reader_failure()),
                import::ImportTestHooks::default(),
            )
            .await
            .expect_err("hook-path spool outage should win over source construction failure");

        assert_sanitized_spool_error(&error);
        assert_eq!(runner.active_count_for_test(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_factory_failure_with_hooks_and_dangling_spool_symlink_preserves_spool_failure_precedence(
    ) {
        let temp_dir = TempDir::new().unwrap();
        create_dangling_spool_root(&temp_dir);
        let manager = Arc::new(IndexManager::new(temp_dir.path()));
        let runner = migration_runner_for_test(Arc::clone(&manager));

        let error = runner
            .submit_algolia_import_with_test_hooks(
                valid_algolia_payload(),
                |_| Err::<UnusedReader, _>(source_reader_failure()),
                import::ImportTestHooks::default(),
            )
            .await
            .expect_err(
                "hook-path dangling spool outage should win over source construction failure",
            );

        assert_sanitized_spool_error(&error);
        assert_eq!(runner.active_count_for_test(), 0);
    }

    #[tokio::test]
    async fn replica_names_for_primary_rejects_path_traversal_names() {
        let temp_dir = TempDir::new().unwrap();
        let manager = Arc::new(IndexManager::new(temp_dir.path()));
        let escaped_primary = temp_dir.path().join("escaped-primary");
        std::fs::create_dir(&escaped_primary).unwrap();
        IndexSettings::default()
            .save(escaped_primary.join("settings.json"))
            .unwrap();

        let error = replica_names_for_primary(&manager, "../escaped-primary")
            .expect_err("recovery should reject traversal before reading sibling paths");

        assert!(error.contains("refused primary '../escaped-primary'"));
        assert!(escaped_primary.join("settings.json").exists());
    }

    #[tokio::test]
    async fn replica_is_job_owned_rejects_path_traversal_names() {
        let temp_dir = TempDir::new().unwrap();
        let manager = Arc::new(IndexManager::new(temp_dir.path()));
        let escaped_replica = temp_dir.path().join("escaped-replica");
        std::fs::create_dir(&escaped_replica).unwrap();
        IndexSettings {
            primary: Some("primary".to_string()),
            ..Default::default()
        }
        .save(escaped_replica.join("settings.json"))
        .unwrap();

        let error = replica_is_job_owned(&manager, "../escaped-replica", "primary")
            .expect_err("recovery should reject traversal before inspecting sibling replicas");

        assert!(error.contains("refused replica '../escaped-replica'"));
        assert!(escaped_replica.join("settings.json").exists());
    }
}
