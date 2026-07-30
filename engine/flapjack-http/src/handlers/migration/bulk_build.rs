//! Shared owner for node-local staged bulk builds and atomic publication.

use super::source_identity_partitions::{SourceIdentityError, SourceIdentityValidation};
use super::spool::{MigrationPhase, SpoolError, SpoolStore};
use super::{migration_cancelled_error, spool_error, MigrateError, MigrationPublicationMode};
use crate::error_response::json_error_parts;
use crate::handlers::index_resource_store::{load_existing_store, IndexResourceStore};
use axum::http::StatusCode;
use flapjack::error::FlapjackError;
#[cfg(test)]
use flapjack::index::manager::publication::PublicationFaultPoint;
use flapjack::index::manager::publication::{
    PreStagedActivationError, PreStagedPublication, PublicationTarget,
};
use flapjack::index::rules::RuleStore;
use flapjack::index::synonyms::SynonymStore;
use flapjack::types::Document;
use flapjack::IndexManager;
use std::error::Error;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread::JoinHandle;
use uuid::Uuid;

#[cfg(test)]
pub(super) type BeforeDocumentBatchWriteHook =
    Arc<dyn Fn(&[Document]) -> Result<(), FlapjackError> + Send + Sync>;
#[cfg(test)]
pub(super) type AfterDocumentBatchWriteHook = Arc<dyn Fn(&flapjack::types::TaskInfo) + Send + Sync>;
#[cfg(test)]
type BulkBuildEventHook = Arc<dyn Fn(BulkBuildTestEvent) + Send + Sync>;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BulkBuildTestEvent {
    StagingWriterMergeQuiesced,
    PrepublicationValidationStarting,
    PrepublicationValidationVerdict,
    ActivationFence,
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct BulkBuildTestHooks {
    event_hook: Option<BulkBuildEventHook>,
    replacement_publication_fault: Option<PublicationFaultPoint>,
    prepublication_validation_enabled: bool,
}

#[cfg(test)]
impl Default for BulkBuildTestHooks {
    fn default() -> Self {
        Self {
            event_hook: None,
            replacement_publication_fault: None,
            prepublication_validation_enabled: true,
        }
    }
}

#[cfg(test)]
impl BulkBuildTestHooks {
    pub(super) fn with_event_hook(
        mut self,
        hook: impl Fn(BulkBuildTestEvent) + Send + Sync + 'static,
    ) -> Self {
        self.event_hook = Some(Arc::new(hook));
        self
    }

    #[allow(dead_code)]
    pub(super) fn without_prepublication_validation(mut self) -> Self {
        self.prepublication_validation_enabled = false;
        self
    }

    pub(super) fn with_replacement_publication_fault(
        mut self,
        fault: PublicationFaultPoint,
    ) -> Self {
        self.replacement_publication_fault = Some(fault);
        self
    }

    fn emit(&self, event: BulkBuildTestEvent) {
        if let Some(hook) = &self.event_hook {
            hook(event);
        }
    }
}

/// Cancellation input shared by translation and the staged bulk-build owner.
#[derive(Clone)]
pub(super) struct MigrationCancellationCheck {
    spool: SpoolStore,
    job_uuid: Uuid,
}

impl MigrationCancellationCheck {
    pub(super) fn new(spool: &SpoolStore, job_uuid: Uuid) -> Self {
        Self {
            spool: spool.clone(),
            job_uuid,
        }
    }

    pub(super) fn cancel_requested(&self) -> Result<bool, SpoolError> {
        self.spool.cancel_requested(self.job_uuid)
    }

    pub(super) fn check(&self) -> Result<(), MigrateError> {
        match self.cancel_requested() {
            Ok(false) => Ok(()),
            Ok(true) => Err(migration_cancelled_error()),
            Err(error) => Err(spool_error(error)),
        }
    }
}

/// Counts verified from one staged or activated bulk-build generation.
#[derive(Debug, Clone, Copy)]
pub(super) struct BulkBuildCounts {
    pub(super) settings: bool,
    pub(super) documents: usize,
    pub(super) rules: usize,
    pub(super) synonyms: usize,
}

/// Staging index created and owned by [`BulkBuildService`].
pub(super) struct BulkBuildStaging {
    manager: Arc<IndexManager>,
    tenant: String,
}

impl BulkBuildStaging {
    pub(super) fn manager(&self) -> &Arc<IndexManager> {
        &self.manager
    }

    pub(super) fn tenant(&self) -> &str {
        &self.tenant
    }
}

/// Coordinates preparation, staging lifecycle, verification, and activation.
pub(super) struct BulkBuildService<'a> {
    state_manager: &'a Arc<IndexManager>,
    spool: &'a SpoolStore,
    job_uuid: Uuid,
    target_index: &'a str,
    cancellation: MigrationCancellationCheck,
    #[cfg(test)]
    test_hooks: BulkBuildTestHooks,
}

impl<'a> BulkBuildService<'a> {
    pub(super) fn new(
        state_manager: &'a Arc<IndexManager>,
        spool: &'a SpoolStore,
        job_uuid: Uuid,
        target_index: &'a str,
        #[cfg(test)] test_hooks: BulkBuildTestHooks,
    ) -> Self {
        Self {
            state_manager,
            spool,
            job_uuid,
            target_index,
            cancellation: MigrationCancellationCheck::new(spool, job_uuid),
            #[cfg(test)]
            test_hooks,
        }
    }

    pub(super) fn cancellation(&self) -> MigrationCancellationCheck {
        self.cancellation.clone()
    }

    pub(super) fn prepare_publication(&self) -> Result<PreStagedPublication, MigrateError> {
        let target =
            PublicationTarget::new(self.target_index.to_string()).map_err(flapjack_error)?;
        self.spool
            .transition_migration_phase(self.job_uuid, MigrationPhase::Preparing)
            .map_err(spool_error)?;
        let publication = PreStagedPublication::prepare(&self.state_manager.base_path, target)
            .map_err(flapjack_error)?;
        self.spool
            .record_async_publication_receipt_if_present(
                self.job_uuid,
                publication.transaction_id().clone(),
                Some(publication.generation().clone()),
            )
            .map_err(spool_error)?;
        Ok(publication)
    }

    pub(super) fn create_staging(
        &self,
        publication: &PreStagedPublication,
    ) -> Result<BulkBuildStaging, MigrateError> {
        self.cancellation.check()?;
        let staging_parent = publication
            .paths()
            .staging
            .parent()
            .expect("publication staging path should have a transaction namespace");
        let tenant = publication
            .paths()
            .staging
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(internal_error)?
            .to_string();
        let manager = IndexManager::new_for_bulk_build(
            staging_parent,
            flapjack::index::BulkBuildWriterConfig::from_env(),
        );
        self.cancellation.check()?;
        manager.create_tenant(&tenant).map_err(flapjack_error)?;
        Ok(BulkBuildStaging { manager, tenant })
    }

    pub(super) fn spawn_document_writer(
        &self,
        staging: &BulkBuildStaging,
        #[cfg(test)] before_write: Option<BeforeDocumentBatchWriteHook>,
        #[cfg(test)] after_write: Option<AfterDocumentBatchWriteHook>,
    ) -> (
        SyncSender<Vec<Document>>,
        JoinHandle<Result<(), FlapjackError>>,
    ) {
        let (document_sender, document_receiver) =
            std::sync::mpsc::sync_channel::<Vec<Document>>(1);
        let staging_manager = Arc::clone(&staging.manager);
        let staging_tenant = staging.tenant.clone();
        let cancellation = self.cancellation.clone();
        let writer = std::thread::spawn(move || -> Result<(), FlapjackError> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            while let Ok(batch) = document_receiver.recv() {
                #[cfg(test)]
                if let Some(hook) = &before_write {
                    hook(&batch)?;
                }
                if cancellation
                    .cancel_requested()
                    .map_err(|error| FlapjackError::Io(error.to_string()))?
                {
                    continue;
                }
                let task = runtime.block_on(
                    staging_manager.add_documents_insert_durable(&staging_tenant, batch),
                )?;
                #[cfg(test)]
                if let Some(hook) = &after_write {
                    let committed_task = staging_manager.get_task(&task.id)?;
                    hook(&committed_task);
                }
                #[cfg(not(test))]
                let _ = task;
            }
            // This drain is NOT redundant with the one in `finish_staging`, even though both
            // call `drain_all_write_queues`. The staging tenant's write tasks were spawned on
            // THIS thread's current-thread runtime by `add_documents_insert_durable` above, so
            // they can only be driven to completion from inside this runtime; awaiting the same
            // handles from the caller's runtime after this thread exits never completes them.
            // This call quiesces the write worker; `finish_staging`'s call is the publication
            // path's own quiescence gate before validation and activation. Removing either one
            // breaks staged bulk builds — verified: deleting this drain fails 10 migration
            // tests including `bulk_build_quiesces_staging_writer_before_activation`.
            runtime.block_on(staging_manager.drain_all_write_queues())?;
            Ok(())
        });
        (document_sender, writer)
    }

    pub(super) fn join_document_writer(
        &self,
        document_writer: JoinHandle<Result<(), FlapjackError>>,
    ) -> Result<(), FlapjackError> {
        document_writer.join().map_err(|_| {
            FlapjackError::Io("migration staging document writer panicked".to_string())
        })?
    }

    pub(super) async fn finish_staging(
        &self,
        staging: &BulkBuildStaging,
        source_identity_validation: SourceIdentityValidation,
    ) -> Result<BulkBuildCounts, MigrateError> {
        staging
            .manager
            .drain_all_write_queues()
            .await
            .map_err(flapjack_error)?;
        staging
            .manager
            .unload(&staging.tenant)
            .map_err(flapjack_error)?;
        #[cfg(test)]
        self.test_hooks
            .emit(BulkBuildTestEvent::StagingWriterMergeQuiesced);
        staging
            .manager
            .scrub_transient_runtime_artifacts(&staging.tenant)
            .map_err(flapjack_error)?;
        self.cancellation.check()?;
        let counts = self.counts_for(&staging.manager, &staging.tenant)?;
        self.validate_before_publication(source_identity_validation)?;
        Ok(counts)
    }

    pub(super) async fn activate(
        &self,
        publication: PreStagedPublication,
        publication_mode: MigrationPublicationMode,
    ) -> Result<(), MigrateError> {
        #[cfg(test)]
        self.test_hooks.emit(BulkBuildTestEvent::ActivationFence);
        match publication_mode {
            MigrationPublicationMode::CreateOnly => {
                publication
                    .activate_create_only()
                    .map_err(activation_error)?;
            }
            MigrationPublicationMode::ReplaceExisting { staging_baseline } => {
                #[cfg(test)]
                let activation = match self.test_hooks.replacement_publication_fault {
                    Some(fault) => {
                        self.state_manager
                            .replace_index_contents_from_pre_staged_with_publication_fault_for_test_support(
                                publication,
                                self.target_index,
                                staging_baseline,
                                fault,
                            )
                            .await
                    }
                    None => {
                        self.state_manager
                            .replace_index_contents_from_pre_staged(
                                publication,
                                self.target_index,
                                staging_baseline,
                            )
                            .await
                    }
                };
                #[cfg(not(test))]
                let activation = self
                    .state_manager
                    .replace_index_contents_from_pre_staged(
                        publication,
                        self.target_index,
                        staging_baseline,
                    )
                    .await;
                activation.map_err(flapjack_error)?;
            }
        }
        Ok(())
    }

    pub(super) fn activated_counts(&self) -> Result<BulkBuildCounts, MigrateError> {
        self.counts_for(self.state_manager, self.target_index)
    }

    fn counts_for(
        &self,
        manager: &Arc<IndexManager>,
        tenant: &str,
    ) -> Result<BulkBuildCounts, MigrateError> {
        let index = manager.get_or_load(tenant).map_err(flapjack_error)?;
        let documents = index.reader().searcher().num_docs() as usize;
        let rules = resource_count::<RuleStore>(manager, tenant)?;
        let synonyms = resource_count::<SynonymStore>(manager, tenant)?;
        let settings = manager.get_settings(tenant).is_some();
        if !settings {
            return Err(internal_error());
        }
        tracing::debug!(
            target_index = self.target_index,
            documents,
            rules,
            synonyms,
            "validated bulk-build generation counts"
        );
        Ok(BulkBuildCounts {
            settings,
            documents,
            rules,
            synonyms,
        })
    }

    fn validate_before_publication(
        &self,
        validation: SourceIdentityValidation,
    ) -> Result<(), MigrateError> {
        #[cfg(test)]
        self.test_hooks
            .emit(BulkBuildTestEvent::PrepublicationValidationStarting);
        #[cfg(test)]
        let result = if self.test_hooks.prepublication_validation_enabled {
            validation.into_result()
        } else {
            Ok(())
        };
        #[cfg(not(test))]
        let result = validation.into_result();
        #[cfg(test)]
        self.test_hooks
            .emit(BulkBuildTestEvent::PrepublicationValidationVerdict);
        result.map_err(source_identity_validation_error)
    }
}

fn resource_count<S>(manager: &Arc<IndexManager>, tenant: &str) -> Result<usize, MigrateError>
where
    S: IndexResourceStore,
{
    Ok(load_existing_store::<S>(manager, tenant)
        .map_err(flapjack_error)?
        .map(|store| store.count())
        .unwrap_or(0))
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
    internal_error()
}

pub(super) fn flapjack_error(error: FlapjackError) -> MigrateError {
    json_error_parts(error.status_code(), error.api_message())
}

fn internal_error() -> MigrateError {
    json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
}

fn source_identity_validation_error(error: SourceIdentityError) -> MigrateError {
    match error {
        SourceIdentityError::Duplicate { first, second } => json_error_parts(
            StatusCode::BAD_REQUEST,
            format!(
                "duplicate source objectID first appeared at page {}, item {} and again at page {}, item {}",
                first.0, first.1, second.0, second.1
            ),
        ),
        _ => internal_error(),
    }
}
