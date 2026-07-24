use super::source_reader::MigrationSourceReader;
use super::spool::{
    AsyncMigrationMetadata, MigrationDisposition, MigrationPhaseRecord, SpoolError, SpoolLimits,
    SpoolStore,
};
use super::{admit_async_migration_payload, algolia_error, import, migration_capacity_exhausted};
use super::{MigrateError, MigrateFromAlgoliaRequest};
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

#[derive(Clone)]
#[allow(dead_code)]
pub struct MigrationJobRunner {
    manager: Arc<IndexManager>,
    replication_manager: Option<Arc<ReplicationManager>>,
    capacity: Arc<Semaphore>,
    active: Arc<DashMap<Uuid, JoinHandle<()>>>,
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
        }
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
        let target_index =
            admit_async_migration_payload(self.replication_manager.as_ref(), &payload)?;
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| migration_capacity_exhausted())?;
        let spool = import::spool_for_manager(&self.manager)?;
        let reader = source_factory(&payload).map_err(algolia_error)?;
        let job_uuid = Uuid::new_v4();
        let phase_record = spool
            .create_async_migration_admission_for_owner(
                job_uuid,
                &target_index,
                authenticated_app_id.as_deref(),
            )
            .map_err(import::spool_error)?;

        self.spawn_import(job_uuid, target_index, reader, permit);
        Ok((job_uuid, phase_record))
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
        let target_index =
            admit_async_migration_payload(self.replication_manager.as_ref(), &payload)?;
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| migration_capacity_exhausted())?;
        let spool = import::spool_for_manager(&self.manager)?;
        let reader = source_factory(&payload).map_err(algolia_error)?;
        let job_uuid = Uuid::new_v4();
        let phase_record = spool
            .create_async_migration_admission_for_owner(
                job_uuid,
                &target_index,
                authenticated_app_id.as_deref(),
            )
            .map_err(import::spool_error)?;

        self.spawn_import_with_hooks(job_uuid, target_index, reader, permit, hooks);
        Ok((job_uuid, phase_record))
    }

    #[allow(dead_code)]
    fn spawn_import<R>(
        &self,
        job_uuid: Uuid,
        target_index: String,
        mut reader: R,
        permit: OwnedSemaphorePermit,
    ) where
        R: MigrationSourceReader + Send + 'static,
    {
        let import_manager = Arc::clone(&self.manager);
        let monitor_manager = Arc::clone(&self.manager);
        let active = Arc::clone(&self.active);
        let (published, published_rx) = oneshot::channel();
        let import_task = tokio::spawn(async move {
            import::import_from_admitted_source(
                &import_manager,
                job_uuid,
                target_index,
                &mut reader,
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
                    let _ = spool.fail_migration(job_uuid);
                }
            }
            drop(permit);
            let _ = published_rx.await;
            active.remove(&job_uuid);
        });
        self.active.insert(job_uuid, monitor);
        let _ = published.send(());
    }

    #[cfg(test)]
    fn spawn_import_with_hooks<R>(
        &self,
        job_uuid: Uuid,
        target_index: String,
        mut reader: R,
        permit: OwnedSemaphorePermit,
        hooks: import::ImportTestHooks,
    ) where
        R: MigrationSourceReader + Send + 'static,
    {
        let import_manager = Arc::clone(&self.manager);
        let monitor_manager = Arc::clone(&self.manager);
        let active = Arc::clone(&self.active);
        let (published, published_rx) = oneshot::channel();
        let import_task = tokio::spawn(async move {
            import::import_from_admitted_source_with_test_hooks(
                &import_manager,
                job_uuid,
                target_index,
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
                    let _ = spool.fail_migration(job_uuid);
                }
            }
            drop(permit);
            let _ = published_rx.await;
            active.remove(&job_uuid);
        });
        self.active.insert(job_uuid, monitor);
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
            if let Some((_, handle)) = self.active.remove(&job_uuid) {
                let _ = handle.await;
            }
        }
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
        if phase.cancel_requested {
            return self
                .recover_cancel_requested_async_job(spool, job_uuid, metadata, publication_reports)
                .await;
        }
        let Some(transaction_id) = &metadata.publication_transaction_id else {
            spool
                .fail_migration(job_uuid)
                .map_err(recovery_spool_error)?;
            return Ok(());
        };
        let report = proven_committed_report(metadata, publication_reports)?;
        if report.transaction_id.as_ref() != Some(transaction_id) {
            return Err(format!(
                "async migration recovery refused target '{}' for job {}: publication transaction mismatch",
                metadata.target_index, job_uuid
            ));
        }
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

    async fn recover_cancel_requested_async_job(
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
                return Err(format!(
                    "async migration recovery refused target '{}' for job {}: publication transaction mismatch",
                    metadata.target_index, job_uuid
                ));
            }
            if report.disposition == PublicationTargetDisposition::Loadable
                && report.phase == Some(PublicationPhase::Committed)
            {
                spool
                    .succeed_migration(job_uuid)
                    .map_err(recovery_spool_error)?;
                return Ok(());
            }
        }
        let target = PublicationTarget::new(metadata.target_index.clone()).map_err(|error| {
            format!(
                "async migration recovery refused target '{}' for job {}: {error}",
                metadata.target_index, job_uuid
            )
        })?;
        abort_unjournaled_publication(&self.manager.base_path, target, transaction_id).map_err(
            |error| {
                format!(
                    "async migration recovery failed aborting unjournaled publication '{}' for job {}: {error}",
                    metadata.target_index, job_uuid
                )
            },
        )?;
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
    let report = publication_reports
        .iter()
        .find(|report| report.target.as_str() == metadata.target_index)
        .ok_or_else(|| {
            format!(
                "async migration recovery refused target '{}': missing publication repair report",
                metadata.target_index
            )
        })?;
    if report.disposition != PublicationTargetDisposition::Loadable
        || report.phase != Some(PublicationPhase::Committed)
    {
        return Err(format!(
            "async migration recovery refused target '{}': publication evidence is not a committed loadable target",
            metadata.target_index
        ));
    }
    Ok(report)
}

fn replica_names_for_primary(
    manager: &Arc<IndexManager>,
    primary: &str,
) -> Result<Vec<String>, String> {
    let settings_path = manager.base_path.join(primary).join("settings.json");
    let settings = IndexSettings::load(&settings_path).map_err(|error| {
        format!("async migration recovery could not read primary settings for '{primary}': {error}")
    })?;
    settings
        .replicas
        .unwrap_or_default()
        .into_iter()
        .map(|entry| {
            parse_replica_entry(&entry)
                .map(|parsed| parsed.name().to_string())
                .map_err(|error| {
                    format!(
                        "async migration recovery refused primary '{primary}': invalid replica entry '{entry}': {error}"
                    )
                })
        })
        .collect()
}

fn replica_is_job_owned(
    manager: &Arc<IndexManager>,
    replica_name: &str,
    primary: &str,
) -> Result<bool, String> {
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

fn directory_is_empty(path: &std::path::Path) -> io::Result<bool> {
    Ok(std::fs::read_dir(path)?.next().is_none())
}

fn recovery_spool_error(error: SpoolError) -> String {
    format!("async migration recovery spool error: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::migration::algolia_client::{
        AlgoliaClientError, AlgoliaErrorKind, AlgoliaIndexRecord,
    };
    use crate::handlers::migration::source_reader::{
        MigrationSourceReader, PageConsumer, SourceFuture,
    };
    use serde_json::Value;
    use tempfile::TempDir;

    struct UnusedReader;

    impl MigrationSourceReader for UnusedReader {
        fn app_id(&self) -> &str {
            "unused"
        }

        fn source_name(&self) -> &str {
            "unused"
        }

        fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, AlgoliaIndexRecord> {
            unreachable!("source reader construction fails before async import starts")
        }

        fn read_settings(&mut self) -> SourceFuture<'_, Value> {
            unreachable!("source reader construction fails before async import starts")
        }

        fn read_index_settings<'a>(&'a mut self, _index_name: &'a str) -> SourceFuture<'a, Value> {
            unreachable!("source reader construction fails before async import starts")
        }

        fn require_unretrievable_access<'a>(
            &'a mut self,
            _settings: &'a Value,
        ) -> SourceFuture<'a, ()> {
            unreachable!("source reader construction fails before async import starts")
        }

        fn read_documents<'a>(
            &'a mut self,
            _consume_page: &'a mut PageConsumer<'a>,
        ) -> SourceFuture<'a, ()> {
            unreachable!("source reader construction fails before async import starts")
        }

        fn read_rules<'a>(
            &'a mut self,
            _consume_page: &'a mut PageConsumer<'a>,
        ) -> SourceFuture<'a, ()> {
            unreachable!("source reader construction fails before async import starts")
        }

        fn read_synonyms<'a>(
            &'a mut self,
            _consume_page: &'a mut PageConsumer<'a>,
        ) -> SourceFuture<'a, ()> {
            unreachable!("source reader construction fails before async import starts")
        }
    }

    #[tokio::test]
    async fn source_factory_failure_does_not_persist_hidden_async_job() {
        let temp_dir = TempDir::new().unwrap();
        let manager = Arc::new(IndexManager::new(temp_dir.path()));
        let runner = MigrationJobRunner::new(Arc::clone(&manager), None, 1);
        let payload = MigrateFromAlgoliaRequest {
            app_id: "app".to_string(),
            api_key: "key".to_string(),
            source_index: "products".to_string(),
            target_index: Some("shop".to_string()),
            overwrite: false,
        };

        let error = runner
            .submit_algolia_import(payload, |_| {
                Err::<UnusedReader, _>(AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "source reader construction failed",
                ))
            })
            .await
            .expect_err("reader construction failure should reject submission");

        assert_eq!(error.0, axum::http::StatusCode::BAD_GATEWAY);
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
}
