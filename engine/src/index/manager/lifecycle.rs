use super::recovery::{RecoveryDocumentContext, RecoverySeqWindow};
use super::*;
use crate::index::oplog::{read_committed_seq, write_committed_seq, OpLog, OpLogEntry, OPLOG_DIR};
use crate::index::version_store::{VersionStore, VersionStoreError, VERSION_STORE_DIR};
#[cfg(test)]
use publication::activate_publication_for_test;
#[cfg(any(test, feature = "test-support"))]
use publication::PublicationFaultPoint;
use publication::{
    activate_publication_with_fence, invalid_publication, read_publication_epoch,
    read_strict_committed_seq, PreStagedActivationError, PreStagedPublication,
    PublicationActivationInputs, PublicationArtifactPlan, PublicationFenceEvidence,
    PublicationGenerationEvidence, PublicationPaths, PublicationPhase, PublicationStagingBaseline,
    PublicationTarget, PublicationTransactionId, PublicationWatermark, TantivyManagedInventory,
};
use std::error::Error;
#[cfg(test)]
use std::sync::{Arc as StdArc, Mutex as StdMutex, OnceLock as StdOnceLock};

#[cfg(test)]
type ReplacementReopenProofHook =
    StdArc<dyn Fn(&super::IndexManager, &str, &mut publication::PublicationJournal) + Send + Sync>;

#[cfg(test)]
static REPLACEMENT_REOPEN_PROOF_HOOK: StdOnceLock<StdMutex<Option<ReplacementReopenProofHook>>> =
    StdOnceLock::new();

#[cfg(test)]
type ImportStagingProofHook = StdArc<dyn Fn(&str) + Send + Sync>;

#[cfg(test)]
static IMPORT_STAGING_PROOF_HOOK: StdOnceLock<StdMutex<Option<ImportStagingProofHook>>> =
    StdOnceLock::new();

#[cfg(test)]
pub(crate) struct ReplacementReopenProofHookGuard {
    previous: Option<ReplacementReopenProofHook>,
}

#[cfg(test)]
pub(crate) struct ImportStagingProofHookGuard {
    previous: Option<ImportStagingProofHook>,
}

#[cfg(test)]
impl Drop for ReplacementReopenProofHookGuard {
    fn drop(&mut self) {
        *replacement_reopen_proof_hook().lock().unwrap() = self.previous.take();
    }
}

#[cfg(test)]
impl Drop for ImportStagingProofHookGuard {
    fn drop(&mut self) {
        *import_staging_proof_hook().lock().unwrap() = self.previous.take();
    }
}

#[cfg(test)]
fn replacement_reopen_proof_hook() -> &'static StdMutex<Option<ReplacementReopenProofHook>> {
    REPLACEMENT_REOPEN_PROOF_HOOK.get_or_init(|| StdMutex::new(None))
}

#[cfg(test)]
fn import_staging_proof_hook() -> &'static StdMutex<Option<ImportStagingProofHook>> {
    IMPORT_STAGING_PROOF_HOOK.get_or_init(|| StdMutex::new(None))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublicationArtifactMode {
    MoveWithSource,
    PreserveDestination,
}

enum TenantQuiesceFence {
    Admission {
        _fence: publication::PublicationAdmissionFence,
    },
    Epoch(publication::PublicationEpochFence),
}

struct ReplacementTenantQuiesce {
    quiesce: TenantQuiesce,
}

impl ReplacementTenantQuiesce {
    fn epoch_fence(&self) -> &publication::PublicationEpochFence {
        match &self.quiesce._publication_fence {
            TenantQuiesceFence::Epoch(fence) => fence,
            TenantQuiesceFence::Admission { .. } => {
                unreachable!("replacement quiesce always owns an epoch fence")
            }
        }
    }
}

enum DestinationPublicationFence {
    Move(publication::PublicationEpochFence),
    Replacement(ReplacementTenantQuiesce),
}

impl DestinationPublicationFence {
    fn epoch_fence(&self) -> &publication::PublicationEpochFence {
        match self {
            Self::Move(fence) => fence,
            Self::Replacement(quiesce) => quiesce.epoch_fence(),
        }
    }
}

impl PublicationArtifactMode {
    fn operation_name(self) -> &'static str {
        match self {
            Self::MoveWithSource => "move",
            Self::PreserveDestination => "replace",
        }
    }
}

impl super::IndexManager {
    #[cfg(test)]
    pub(crate) fn set_replacement_reopen_proof_hook_for_test(
        hook: impl Fn(&super::IndexManager, &str, &mut publication::PublicationJournal)
            + Send
            + Sync
            + 'static,
    ) -> ReplacementReopenProofHookGuard {
        let mut slot = replacement_reopen_proof_hook().lock().unwrap();
        ReplacementReopenProofHookGuard {
            previous: slot.replace(StdArc::new(hook)),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_import_staging_proof_hook_for_test(
        hook: impl Fn(&str) + Send + Sync + 'static,
    ) -> ImportStagingProofHookGuard {
        let mut slot = import_staging_proof_hook().lock().unwrap();
        ImportStagingProofHookGuard {
            previous: slot.replace(StdArc::new(hook)),
        }
    }

    /// Create or load a tenant index, initializing default settings if the index is new.
    ///
    /// # Arguments
    ///
    /// * `tenant_id` - The tenant identifier, validated as a valid index name.
    ///
    /// # Returns
    ///
    /// Ok(()) if the tenant already exists and is loaded, or if creation succeeds. Errors if tenant ID is invalid or index creation fails.
    pub fn create_tenant(&self, tenant_id: &str) -> Result<()> {
        validate_index_name(tenant_id)?;
        if self.loaded.contains_key(tenant_id) {
            return Ok(());
        }

        let path = self.base_path.join(tenant_id);
        if path.exists() {
            let index_languages = Self::read_index_languages(&path);
            let indexed_separators = Self::read_indexed_separators(&path);
            let keep_diacritics_on_characters = Self::read_keep_diacritics_on_characters(&path);
            let custom_normalization = Self::read_custom_normalization(&path);
            let index = Arc::new(
                Index::open_with_languages_indexed_separators_and_keep_diacritics(
                    &path,
                    crate::index::get_global_budget(),
                    &index_languages,
                    &indexed_separators,
                    &keep_diacritics_on_characters,
                    &custom_normalization,
                )?,
            );
            self.publish_loaded_runtime_state_if_unfenced(tenant_id, index)?;
            return Ok(());
        }

        std::fs::create_dir_all(&path)?;
        let schema = crate::index::schema::Schema::builder().build();
        // New index: no settings yet, default to CJK-aware tokenizer
        let index = Arc::new(Index::create(&path, schema)?);
        self.loaded.insert(tenant_id.to_string(), index);

        let settings_path = path.join("settings.json");
        if !settings_path.exists() {
            let default_settings = IndexSettings::default();
            default_settings.save(&settings_path)?;
        }

        // Persist index creation metadata
        crate::index::index_metadata::IndexMetadata::load_or_create(&path)?;
        write_committed_seq(&path, 0)?;

        Ok(())
    }

    /// Remove a tenant from the loaded cache without touching on-disk state.
    pub fn unload_tenant(&self, tenant_id: &str) {
        self.loaded.remove(tenant_id);
    }

    /// Hold this tenant in the production write-backpressure registry for an
    /// integration test, clearing the pause when the returned guard is dropped.
    #[cfg(feature = "test-support")]
    pub fn hold_write_backpressure_pause_for_test_support(
        &self,
        tenant_id: &str,
    ) -> Result<impl Drop> {
        validate_index_name(tenant_id)?;
        crate::index::write_queue::backpressure::hold_non_improving_pause_for_test(
            &self.base_path,
            tenant_id,
        )
    }

    pub(super) fn cache_loaded_index(&self, tenant_id: &str, index: Arc<Index>) -> Arc<Index> {
        let _ = index.searchable_paths();
        self.loaded
            .insert(tenant_id.to_string(), Arc::clone(&index));
        index
    }

    /// Unload a tenant's index from memory.
    ///
    /// Removes the index from the cache, closing all file handles.
    /// Required before export/migration to ensure clean state.
    fn clear_tenant_runtime_state(&self, tenant_id: &TenantId) {
        self.oplogs.remove(tenant_id);
        self.loaded.remove(tenant_id);
        self.tenant_load_locks.remove(tenant_id);
        self.settings_cache.remove(tenant_id);
        self.rules_cache.remove(tenant_id);
        self.synonyms_cache.remove(tenant_id);
        #[cfg(feature = "vector-search")]
        self.vector_indices.remove(tenant_id);
    }

    pub fn unload(&self, tenant_id: &TenantId) -> Result<()> {
        self.invalidate_facet_cache(tenant_id);
        self.write_queues.remove(tenant_id);
        self.clear_tenant_runtime_state(tenant_id);
        Ok(())
    }

    /// Delete a tenant's index and all on-disk files, removing it from all runtime caches.
    ///
    /// Quiesces the tenant (draining and merge-quiescing the persistent writer,
    /// then clearing runtime caches) and removes the directory. The removal retry
    /// loop is only defense in depth against transient filesystem errors now that
    /// quiesce guarantees no merge thread is still writing segment files.
    ///
    /// # Arguments
    ///
    /// * `tenant_id` - The tenant to delete.
    ///
    /// # Returns
    ///
    /// Ok(()) on successful deletion, or an error if the tenant doesn't exist or removal fails after retries.
    pub async fn delete_tenant(&self, tenant_id: &TenantId) -> Result<()> {
        validate_index_name(tenant_id)?;
        let path = self.base_path.join(tenant_id);
        if !path.exists() {
            return Err(FlapjackError::TenantNotFound(tenant_id.to_string()));
        }

        // Quiesce is the canonical guarantee that no persistent writer or merge
        // thread is still writing into the tree before we remove it. A failed
        // drain must abort deletion because no safe removal guarantee exists.
        let _quiesce = self.quiesce_tenant(tenant_id).await?;
        self.admission_stores.remove(tenant_id);

        #[cfg(debug_assertions)]
        crate::index::write_queue::record_writer_lifecycle_publication_checkpoint(
            tenant_id,
            "manager_delete_publication",
        );

        // `quiesce_tenant` above is now the guarantee that the persistent writer
        // and its merge threads have already finished. This retry loop remains
        // only as defense in depth against transient filesystem errors (a slow
        // antivirus scan, a lingering external handle) and no longer relies on
        // merge threads still draining after the writer was dropped.
        let mut last_err = None;
        for _ in 0..10 {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                // The path can disappear after the existence check due to a concurrent delete.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        if let Some(e) = last_err {
            return Err(e.into());
        }
        crate::index::write_queue::backpressure::remove_tenant_state(&self.base_path, tenant_id);
        Ok(())
    }

    /// Queue an asynchronous tenant export to the given destination path. Creates a
    /// task, sends an `Export` command to the task queue, and returns the task ID
    /// for polling.
    pub fn export_tenant(&self, tenant_id: &TenantId, dest_path: PathBuf) -> Result<String> {
        validate_index_name(tenant_id)?;
        if dest_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(FlapjackError::InvalidQuery(
                "export destination must not contain '..' path traversal".into(),
            ));
        }
        let numeric_id = self.next_numeric_task_id();
        let task_id = format!("export_{}_{}", tenant_id, uuid::Uuid::new_v4());
        let task = TaskInfo::new(task_id.clone(), numeric_id, 0);
        self.task_queue
            .enqueue_export(task, tenant_id.clone(), dest_path)?;

        Ok(task_id)
    }

    pub async fn export_tenant_wait(&self, tenant_id: &TenantId, dest_path: PathBuf) -> Result<()> {
        let task_id = self.export_tenant(tenant_id, dest_path)?;

        loop {
            let status = self.get_task(&task_id)?;
            match status.status {
                TaskStatus::Enqueued | TaskStatus::Processing => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                }
                TaskStatus::Succeeded => return Ok(()),
                TaskStatus::Failed(e) => return Err(FlapjackError::Tantivy(e)),
            }
        }
    }

    /// Import a tenant's index from a source path.
    ///
    /// Quiesces any live destination writer through the canonical
    /// [`Self::quiesce_tenant`] contract, replaces the destination directory with
    /// the source contents, and leaves the tenant unloaded so the next access
    /// reopens exactly the imported generation. Draining the destination writer
    /// before publication is why this must be async: the persistent writer can
    /// only merge-quiesce by yielding to the runtime.
    pub async fn import_tenant(&self, tenant_id: &TenantId, src_path: &Path) -> Result<()> {
        validate_index_name(tenant_id)?;
        let base_path = self.base_path.clone();
        let target_tenant = tenant_id.clone();
        let source = src_path.to_path_buf();
        let publication = tokio::task::spawn_blocking(move || {
            stage_tenant_import(&base_path, target_tenant, &source)
        })
        .await
        .map_err(|error| {
            FlapjackError::Io(format!(
                "tenant import staging task failed for {tenant_id}: {error}"
            ))
        })??;

        // Stage before fencing so a source-copy failure neither lengthens the
        // destination outage nor leaves a lock-only namespace for an absent
        // tenant. Activation still holds the quiesce guard throughout.
        let _quiesce = match self.quiesce_tenant(tenant_id).await {
            Ok(quiesce) => quiesce,
            Err(error) => {
                return Err(
                    abort_import_after_quiesce_failure(publication, tenant_id, error).await,
                );
            }
        };

        #[cfg(debug_assertions)]
        crate::index::write_queue::record_writer_lifecycle_publication_checkpoint(
            tenant_id,
            "manager_import_publication",
        );

        tokio::task::spawn_blocking(move || {
            publication
                .activate()
                .map_err(pre_staged_activation_error)?;
            Ok(())
        })
        .await
        .map_err(|error| {
            FlapjackError::Io(format!(
                "tenant import activation task failed for {tenant_id}: {error}"
            ))
        })?
    }

    /// Move an index from source to destination path, cleaning up existing state.
    ///
    /// Validates both names, unloads the source tenant, removes the destination if it exists (whether in memory or on disk), then renames the source directory.
    ///
    /// # Arguments
    ///
    /// * `source` - The source index name.
    /// * `destination` - The destination index name.
    ///
    /// # Returns
    ///
    /// Ok with a TaskInfo for the operation, or an error if validation fails or the source doesn't exist.
    pub async fn move_index(&self, source: &str, destination: &str) -> Result<TaskInfo> {
        self.move_index_with_publication(
            source,
            destination,
            PublicationArtifactMode::MoveWithSource,
            None,
            None,
        )
        .await
    }

    /// Replace a live index's tenant contents while retaining its target-keyed sidecars.
    ///
    /// The source must be a staging tenant for the same logical index. Unlike
    /// [`Self::move_index`], this operation leaves destination query-suggestions
    /// and analytics control data under the destination key.
    pub(crate) async fn replace_index_contents(
        &self,
        source: &str,
        destination: &str,
        staging_baseline: PublicationStagingBaseline,
    ) -> Result<TaskInfo> {
        self.move_index_with_publication(
            source,
            destination,
            PublicationArtifactMode::PreserveDestination,
            Some(staging_baseline),
            None,
        )
        .await
    }

    pub async fn replace_index_contents_from_pre_staged(
        &self,
        publication: PreStagedPublication,
        destination: &str,
        staging_baseline: PublicationStagingBaseline,
    ) -> Result<TaskInfo> {
        self.replace_index_contents_from_pre_staged_inner(
            publication,
            destination,
            staging_baseline,
            #[cfg(feature = "test-support")]
            None,
        )
        .await
    }

    #[cfg(feature = "test-support")]
    pub async fn replace_index_contents_from_pre_staged_with_publication_fault_for_test_support(
        &self,
        publication: PreStagedPublication,
        destination: &str,
        staging_baseline: PublicationStagingBaseline,
        fault: PublicationFaultPoint,
    ) -> Result<TaskInfo> {
        self.replace_index_contents_from_pre_staged_inner(
            publication,
            destination,
            staging_baseline,
            Some(fault),
        )
        .await
    }

    async fn replace_index_contents_from_pre_staged_inner(
        &self,
        publication: PreStagedPublication,
        destination: &str,
        staging_baseline: PublicationStagingBaseline,
        #[cfg(feature = "test-support")] publication_fault: Option<PublicationFaultPoint>,
    ) -> Result<TaskInfo> {
        validate_index_name(destination)?;
        let source_path = publication.paths().staging.clone();
        if !source_path.exists() {
            return self.make_noop_task(destination);
        }
        let target = PublicationTarget::new(destination)?;
        let replacement_quiesce = self
            .quiesce_replacement_tenant(destination, &target)
            .await?;
        let destination_path = self.base_path.join(destination);
        let watermark = self.stage_replacement_from_drained_destination(
            "staging",
            destination,
            &source_path,
            &destination_path,
            staging_baseline,
        )?;
        let fence_evidence = PublicationFenceEvidence::new(
            replacement_quiesce.epoch_fence().previous(),
            replacement_quiesce.epoch_fence().advanced(),
            staging_baseline,
            PublicationWatermark::new(watermark),
        )?;

        #[cfg(debug_assertions)]
        crate::index::write_queue::record_writer_lifecycle_publication_checkpoint(
            destination,
            "manager_replace_publication",
        );
        #[cfg(feature = "test-support")]
        let activation = match publication_fault {
            Some(fault) => publication
                .activate_with_fence_and_checkpoint_panic_for_test_support(fence_evidence, fault),
            None => publication.activate_with_fence(fence_evidence),
        };
        #[cfg(not(feature = "test-support"))]
        let activation = publication.activate_with_fence(fence_evidence);
        let mut journal = activation.map_err(pre_staged_activation_error)?;
        ensure_committed_move(&journal)?;
        self.clear_tenant_runtime_state(&destination.to_string());
        self.run_replacement_reopen_proof_hook(destination, &mut journal);
        self.certify_replacement_reopen(
            destination,
            PublicationArtifactMode::PreserveDestination,
            &journal,
        )?;
        self.make_noop_task(destination)
    }

    pub fn capture_replacement_staging_baseline(
        &self,
        destination: &str,
    ) -> Result<PublicationStagingBaseline> {
        validate_index_name(destination)?;
        Ok(PublicationStagingBaseline::new(read_committed_seq(
            &self.base_path.join(destination),
        )))
    }

    #[cfg(test)]
    pub(crate) async fn move_index_with_fault_for_test(
        &self,
        source: &str,
        destination: &str,
        fault: PublicationFaultPoint,
    ) -> Result<TaskInfo> {
        self.move_index_with_publication(
            source,
            destination,
            PublicationArtifactMode::MoveWithSource,
            None,
            Some(fault),
        )
        .await
    }

    async fn move_index_with_publication(
        &self,
        source: &str,
        destination: &str,
        artifact_mode: PublicationArtifactMode,
        staging_baseline: Option<PublicationStagingBaseline>,
        #[cfg(test)] fault: Option<PublicationFaultPoint>,
        #[cfg(not(test))] fault: Option<()>,
    ) -> Result<TaskInfo> {
        validate_index_name(source)?;
        validate_index_name(destination)?;
        let src_path = self.base_path.join(source);
        if !src_path.exists() {
            return self.make_noop_task(source);
        }

        let target = PublicationTarget::new(destination)?;
        let destination_publication_fence = match artifact_mode {
            PublicationArtifactMode::MoveWithSource => DestinationPublicationFence::Move(
                self.advance_destination_publication_epoch(destination, &target)?,
            ),
            PublicationArtifactMode::PreserveDestination => {
                DestinationPublicationFence::Replacement(
                    self.quiesce_replacement_tenant(destination, &target)
                        .await?,
                )
            }
        };

        self.drain_target_write_queue(&source.to_string()).await?;
        self.invalidate_facet_cache(source);
        self.clear_tenant_runtime_state(&source.to_string());
        if artifact_mode == PublicationArtifactMode::MoveWithSource {
            self.drain_target_write_queue(&destination.to_string())
                .await?;
        }
        // After the target drain, no old-epoch mutation can still transition to
        // succeeded. Stage 3 proves the strict `committed_seq = W` replacement
        // contract against the quiesced destination and carries the resulting
        // fence evidence into activation, all before any live-target mutation.
        let fence_evidence = self.stage_replacement_fence_evidence(
            source,
            destination,
            artifact_mode,
            staging_baseline,
            destination_publication_fence.epoch_fence(),
        )?;
        if artifact_mode == PublicationArtifactMode::MoveWithSource {
            self.invalidate_facet_cache(destination);
            self.clear_tenant_runtime_state(&destination.to_string());
        }

        let operation_name = artifact_mode.operation_name();
        let transaction = PublicationTransactionId::new(format!(
            "{}_{}",
            operation_name,
            uuid::Uuid::new_v4().simple()
        ))?;
        let (paths, inventory) = self.stage_publication_tree(source, &target, &transaction)?;
        let artifacts = match artifact_mode {
            PublicationArtifactMode::MoveWithSource => Some(PublicationArtifactPlan::for_move(
                &self.base_path,
                &self.publication_analytics_config(),
                source,
                &target,
                &transaction,
            )?),
            PublicationArtifactMode::PreserveDestination => None,
        };
        if let Some(artifacts) = &artifacts {
            artifacts.stage()?;
        }
        let generation = PublicationGenerationEvidence::new(format!(
            "{}_{}_to_{}_{}",
            operation_name,
            source,
            destination,
            uuid::Uuid::new_v4().simple()
        ))?;
        let manifest = artifacts
            .as_ref()
            .map(PublicationArtifactPlan::manifest)
            .unwrap_or_default();
        let mut journal = self.activate_lifecycle_publication(
            PublicationActivationInputs {
                paths: &paths,
                target,
                transaction_id: transaction,
                generation,
                manifest,
                inventory: &inventory,
            },
            fence_evidence,
            fault,
        )?;
        ensure_committed_move(&journal)?;
        if artifact_mode == PublicationArtifactMode::PreserveDestination {
            self.clear_tenant_runtime_state(&destination.to_string());
        }
        self.run_replacement_reopen_proof_hook(destination, &mut journal);
        self.certify_replacement_reopen(destination, artifact_mode, &journal)?;
        #[cfg(test)]
        if fault == Some(PublicationFaultPoint::BeforeSourceCleanup) {
            return Err(FlapjackError::InvalidQuery(
                "injected publication fault before source cleanup".into(),
            ));
        }
        if let Some(artifacts) = &artifacts {
            artifacts.remove_source()?;
        }
        std::fs::remove_dir_all(&src_path)?;
        drop(destination_publication_fence);
        self.make_noop_task(destination)
    }

    fn activate_lifecycle_publication(
        &self,
        inputs: PublicationActivationInputs<'_>,
        fence_evidence: Option<PublicationFenceEvidence>,
        #[cfg(test)] fault: Option<PublicationFaultPoint>,
        #[cfg(not(test))] _fault: Option<()>,
    ) -> Result<publication::PublicationJournal> {
        #[cfg(test)]
        if let Some(fault) = fault {
            // Fault injection is only wired for the move path, which carries no
            // fence evidence; the replacement path never injects faults here.
            debug_assert!(fence_evidence.is_none());
            return activate_publication_for_test(inputs, fault);
        }
        activate_publication_with_fence(inputs, fence_evidence)
    }

    fn run_replacement_reopen_proof_hook(
        &self,
        #[cfg_attr(not(test), allow(unused_variables))] destination: &str,
        #[cfg_attr(not(test), allow(unused_variables))]
        journal: &mut publication::PublicationJournal,
    ) {
        #[cfg(test)]
        if let Some(hook) = replacement_reopen_proof_hook().lock().unwrap().clone() {
            hook(self, destination, journal);
        }
    }

    fn certify_replacement_reopen(
        &self,
        destination: &str,
        artifact_mode: PublicationArtifactMode,
        journal: &publication::PublicationJournal,
    ) -> Result<()> {
        if artifact_mode == PublicationArtifactMode::MoveWithSource {
            return Ok(());
        }
        let fence = journal.fence_evidence.as_ref().ok_or_else(|| {
            invalid_publication(
                "committed journal missing replacement fence evidence before reopen",
            )
        })?;
        self.verify_replacement_epoch_reopen(destination, journal, fence)?;
        self.verify_replacement_watermark_reopen(destination, fence)
    }

    fn verify_replacement_epoch_reopen(
        &self,
        destination: &str,
        journal: &publication::PublicationJournal,
        fence: &PublicationFenceEvidence,
    ) -> Result<()> {
        let durable_epoch =
            read_publication_epoch(&self.base_path, &journal.target).map_err(|error| {
                invalid_publication(format!(
                    "durable publication epoch for {destination} is not readable before reopen: {error}"
                ))
            })?;
        if durable_epoch != fence.epoch_new() {
            return Err(invalid_publication(format!(
                "durable publication epoch for {destination} is {durable_epoch:?}, expected {:?} before reopen",
                fence.epoch_new()
            )));
        }
        Ok(())
    }

    fn verify_replacement_watermark_reopen(
        &self,
        destination: &str,
        fence: &PublicationFenceEvidence,
    ) -> Result<()> {
        let promoted_seq = read_strict_committed_seq(&self.base_path.join(destination))?;
        let watermark = fence.watermark().value();
        if promoted_seq != watermark {
            return Err(invalid_publication(format!(
                "promoted committed_seq for {destination} is {promoted_seq}, expected watermark {watermark} before reopen"
            )));
        }
        Ok(())
    }

    fn advance_destination_publication_epoch(
        &self,
        destination: &str,
        target: &PublicationTarget,
    ) -> Result<publication::PublicationEpochFence> {
        let observed_epoch = publication::capture_publication_epoch(&self.base_path, target)
            .map_err(|error| {
                FlapjackError::Io(format!(
                    "publication epoch capture failed for {destination}: {error:?}"
                ))
            })?;
        publication::compare_and_advance_publication_epoch(&self.base_path, target, observed_epoch)
            .map_err(|error| {
                FlapjackError::Io(format!(
                    "publication epoch advance failed for {destination}: {error}"
                ))
            })
    }

    fn stage_publication_tree(
        &self,
        source: &str,
        target: &PublicationTarget,
        transaction: &PublicationTransactionId,
    ) -> Result<(PublicationPaths, TantivyManagedInventory)> {
        let paths = PublicationPaths::new(&self.base_path, target, transaction);
        std::fs::create_dir_all(paths.staging.parent().ok_or_else(|| {
            FlapjackError::InvalidQuery("publication staging path has no parent".into())
        })?)?;
        if paths.staging.exists() {
            std::fs::remove_dir_all(&paths.staging)?;
        }
        copy_dir_recursive(&self.base_path.join(source), &paths.staging)?;

        let inventory = TantivyManagedInventory::from_existing_trees([
            paths.staging.as_path(),
            paths.target.as_path(),
        ])?;
        Ok((paths, inventory))
    }

    /// Prove and stage the strict replacement watermark contract for one
    /// destination that has already been drained. Returns the fence evidence to
    /// carry into activation: `None` for a plain move (no destination replay),
    /// `Some` for a replacement that proved `committed_seq = W`.
    fn stage_replacement_fence_evidence(
        &self,
        source: &str,
        destination: &str,
        artifact_mode: PublicationArtifactMode,
        staging_baseline: Option<PublicationStagingBaseline>,
        publication_epoch_fence: &publication::PublicationEpochFence,
    ) -> Result<Option<PublicationFenceEvidence>> {
        match (artifact_mode, staging_baseline) {
            (PublicationArtifactMode::MoveWithSource, None) => Ok(None),
            (PublicationArtifactMode::PreserveDestination, Some(staging_baseline)) => {
                let watermark = self.stage_replacement_from_drained_destination(
                    source,
                    destination,
                    &self.base_path.join(source),
                    &self.base_path.join(destination),
                    staging_baseline,
                )?;
                let fence = PublicationFenceEvidence::new(
                    publication_epoch_fence.previous(),
                    publication_epoch_fence.advanced(),
                    staging_baseline,
                    PublicationWatermark::new(watermark),
                )?;
                Ok(Some(fence))
            }
            _ => Err(invalid_publication(
                "replacement publication requires exactly one staging baseline",
            )),
        }
    }

    /// Prove `committed_seq = W` against the drained destination, replay the
    /// acknowledged `(baseline, W]` delta into the staged tree, align the staged
    /// oplog to the destination sequence domain, and strictly re-read the staged
    /// evidence. Returns the proven watermark `W` on success; any missing,
    /// malformed, non-contiguous, or mismatched evidence aborts before activation.
    fn stage_replacement_from_drained_destination(
        &self,
        source: &str,
        destination: &str,
        source_path: &Path,
        destination_path: &Path,
        staging_baseline: PublicationStagingBaseline,
    ) -> Result<u64> {
        let baseline = staging_baseline.value();

        let watermark = self.prove_drained_destination_watermark(destination, destination_path)?;
        if baseline > watermark {
            return Err(invalid_publication(format!(
                "replacement staging baseline {baseline} exceeds drained watermark {watermark}"
            )));
        }

        let delta = self
            .get_or_create_oplog_result(destination)?
            .read_since(baseline)?;
        Self::require_contiguous_delta(&delta, baseline, watermark)?;

        self.replay_delta_into_staged_tree(
            source,
            destination,
            source_path,
            &delta,
            baseline,
            watermark,
        )?;
        self.align_staged_oplog_to_destination(source, source_path, destination_path, watermark)?;
        self.verify_staged_watermark(source, source_path, watermark)?;
        Ok(watermark)
    }

    /// Define `W` as the drained destination oplog high-water mark and strictly
    /// require the old generation's durable `committed_seq` sidecar to equal it.
    /// The sidecar read is fail-closed (never the recovery fail-open reader), so a
    /// missing, non-regular, malformed, or drifted value refuses the replacement.
    fn prove_drained_destination_watermark(
        &self,
        destination: &str,
        destination_path: &Path,
    ) -> Result<u64> {
        let watermark = self.get_or_create_oplog_result(destination)?.current_seq();
        let durable = read_strict_committed_seq(destination_path).map_err(|error| {
            invalid_publication(format!(
                "drained destination committed_seq is not strict watermark proof: {error}"
            ))
        })?;
        if durable != watermark {
            return Err(invalid_publication(format!(
                "drained destination committed_seq {durable} does not equal watermark {watermark}"
            )));
        }
        Ok(watermark)
    }

    /// Require the retained destination delta to cover every sequence in
    /// `(baseline, watermark]` exactly once, contiguously. A truncated or gapped
    /// delta fails closed rather than certifying an incomplete prefix as `W`.
    fn require_contiguous_delta(delta: &[OpLogEntry], baseline: u64, watermark: u64) -> Result<()> {
        let expected_len = (watermark - baseline) as usize;
        if delta.len() != expected_len {
            return Err(invalid_publication(format!(
                "retained destination delta covers {} entries but (baseline {baseline}, watermark {watermark}] requires {expected_len}",
                delta.len()
            )));
        }
        for (offset, entry) in delta.iter().enumerate() {
            let expected_seq = baseline + 1 + offset as u64;
            if entry.seq != expected_seq {
                return Err(invalid_publication(format!(
                    "retained destination delta is not contiguous: expected seq {expected_seq}, found {}",
                    entry.seq
                )));
            }
        }
        Ok(())
    }

    /// Replay the acknowledged `(baseline, W]` document effects into the staged
    /// Tantivy tree using the one recovery-owned replay path, so the staged
    /// generation carries every drained write. Vector effects are rebuilt over the
    /// staged history plus the delta when the feature is enabled.
    fn replay_delta_into_staged_tree(
        &self,
        source: &str,
        destination: &str,
        source_path: &Path,
        delta: &[OpLogEntry],
        baseline: u64,
        watermark: u64,
    ) -> Result<()> {
        let document_ops = delta
            .iter()
            .filter(|entry| Self::is_document_recovery_op(entry.op_type.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if document_ops.is_empty() {
            return Ok(());
        }
        let source_index = self.open_tenant_index_without_write_queue(source_path)?;
        let settings = self.load_settings_after_config(source, source_path)?;
        self.recover_document_ops(
            RecoveryDocumentContext {
                tenant_id: destination,
                index: &source_index,
                tenant_path: source_path,
                seq_window: RecoverySeqWindow {
                    committed_seq: baseline,
                    final_seq: watermark,
                },
                settings: settings.as_ref(),
            },
            &document_ops,
        )?;
        #[cfg(feature = "vector-search")]
        {
            let staged_ops =
                OpLog::open(&source_path.join(OPLOG_DIR), source, "local")?.read_since(0)?;
            let combined = staged_ops
                .into_iter()
                .chain(delta.iter().cloned())
                .collect::<Vec<_>>();
            self.rebuild_vector_index(source, source_path, &combined);
        }
        Ok(())
    }

    /// Replace the staged oplog with the drained destination oplog so the staged
    /// tree's high-water mark is exactly `W` in the destination sequence domain,
    /// then persist staged `committed_seq = W`. This is why `W` cannot be certified
    /// by overwriting the sidecar on an independently numbered staged oplog.
    fn align_staged_oplog_to_destination(
        &self,
        source: &str,
        source_path: &Path,
        destination_path: &Path,
        watermark: u64,
    ) -> Result<()> {
        self.oplogs.remove(source);
        let staged_oplog_dir = source_path.join(OPLOG_DIR);
        replace_directory(&destination_path.join(OPLOG_DIR), &staged_oplog_dir)?;

        let source_version_store_dir = source_path.join(VERSION_STORE_DIR);
        let destination_version_store_dir = destination_path.join(VERSION_STORE_DIR);
        if source_version_store_dir.exists() || destination_version_store_dir.exists() {
            let staged_store =
                VersionStore::open(source_path).map_err(version_store_alignment_error)?;
            let destination_store =
                VersionStore::open(destination_path).map_err(version_store_alignment_error)?;
            staged_store
                .merge_destination_evidence(&destination_store, watermark)
                .map_err(version_store_alignment_error)?;
        }
        write_committed_seq(source_path, watermark)?;
        Ok(())
    }

    /// Strictly re-read the staged evidence and require both the durable
    /// `committed_seq` sidecar and the staged oplog maximum to equal `W`. Evicts
    /// the replay/alignment-opened staged oplog handle so a reused staging tenant
    /// reopens from the promoted-domain files.
    fn verify_staged_watermark(
        &self,
        source: &str,
        source_path: &Path,
        watermark: u64,
    ) -> Result<()> {
        let staged_committed = read_strict_committed_seq(source_path)?;
        if staged_committed != watermark {
            self.oplogs.remove(source);
            return Err(invalid_publication(format!(
                "staged committed_seq {staged_committed} does not equal watermark {watermark}"
            )));
        }
        let staged_current =
            OpLog::open(&source_path.join(OPLOG_DIR), source, "local")?.current_seq();
        self.oplogs.remove(source);
        if staged_current != watermark {
            return Err(invalid_publication(format!(
                "staged oplog maximum {staged_current} does not equal watermark {watermark}"
            )));
        }
        Ok(())
    }

    fn open_tenant_index_without_write_queue(&self, path: &Path) -> Result<Arc<Index>> {
        let index_languages = Self::read_index_languages(path);
        let indexed_separators = Self::read_indexed_separators(path);
        let keep_diacritics_on_characters = Self::read_keep_diacritics_on_characters(path);
        let custom_normalization = Self::read_custom_normalization(path);
        Index::open_with_languages_indexed_separators_and_keep_diacritics(
            path,
            crate::index::get_global_budget(),
            &index_languages,
            &indexed_separators,
            &keep_diacritics_on_characters,
            &custom_normalization,
        )
        .map(Arc::new)
    }

    /// Copy an index from source to destination, optionally filtering to specific configuration files.
    ///
    /// Validates both names and removes any existing destination. Copies the entire directory, or (if scope is specified) only the requested files ("settings", "synonyms", "rules"). If source doesn't exist, creates an empty tenant instead.
    ///
    /// # Arguments
    ///
    /// * `source` - The source index name.
    /// * `destination` - The destination index name.
    /// * `scope` - Optional list of config files to copy. If None, copies the entire index directory.
    ///
    /// # Returns
    ///
    /// Ok with a TaskInfo for the operation, or an error if validation fails.
    pub async fn copy_index(
        &self,
        source: &str,
        destination: &str,
        scope: Option<&[String]>,
    ) -> Result<TaskInfo> {
        validate_index_name(source)?;
        validate_index_name(destination)?;
        let src_path = self.base_path.join(source);

        if self.loaded.contains_key(destination) {
            self.delete_tenant(&destination.to_string()).await?;
        } else {
            let dest_path = self.base_path.join(destination);
            if dest_path.exists() {
                std::fs::remove_dir_all(&dest_path)?;
            }
        }

        if !src_path.exists() {
            self.create_tenant(destination)?;
            return self.make_noop_task(destination);
        }

        let dest_path = self.base_path.join(destination);

        match scope {
            None => {
                std::fs::create_dir_all(&dest_path)?;
                copy_dir_recursive(&src_path, &dest_path)?;
            }
            Some(scopes) => {
                self.create_tenant(destination)?;
                for s in scopes {
                    let filename = match s.as_str() {
                        "settings" => "settings.json",
                        "synonyms" => "synonyms.json",
                        "rules" => "rules.json",
                        _ => continue,
                    };
                    let src_file = src_path.join(filename);
                    if src_file.exists() {
                        std::fs::copy(&src_file, dest_path.join(filename))?;
                    }
                }
            }
        }

        self.make_noop_task(destination)
    }

    /// Gracefully shut down all write queues, flushing pending writes.
    ///
    /// Drops all write queue senders (triggering final batch flush in each
    /// write task), then awaits every write task handle to completion.
    pub async fn graceful_shutdown(&self) {
        let _ = self.drain_all_write_queues().await;
    }

    /// Drain every write queue and surface the first worker failure.
    ///
    /// This is the result-bearing form of [`Self::graceful_shutdown`] for
    /// publication paths that must abort instead of logging and continuing.
    pub async fn drain_all_write_queues(&self) -> Result<()> {
        let handles: Vec<_> = self
            .write_task_handles
            .iter()
            .map(|r| r.key().clone())
            .collect();
        let mut first_error = None;
        for tenant_id in handles {
            if let Err(error) = self.drain_target_write_queue(&tenant_id).await {
                tracing::error!(
                    "[shutdown] Write queue for '{}' failed: {}",
                    tenant_id,
                    error
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn drain_target_write_queue(&self, tenant_id: &TenantId) -> Result<()> {
        drop(self.write_queues.remove(tenant_id));
        let Some(handle) = self
            .write_task_handles
            .get(tenant_id)
            .map(|entry| entry.value().clone())
        else {
            return Ok(());
        };

        match handle.drain(tenant_id.clone()).await {
            Ok(()) => {
                tracing::info!("[shutdown] Write queue for '{}' drained", tenant_id);
                self.write_task_handles
                    .remove_if(tenant_id, |_, current| current.same_handle(&handle));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Canonical tenant quiesce contract shared by every path that removes,
    /// replaces, exports, imports, or publishes over a tenant directory.
    ///
    /// The ordered sequence is: stop write admission by dropping the write-queue
    /// sender, await the write worker's merge-quiescent close through
    /// [`Self::drain_target_write_queue`] (which retires the drained handle after
    /// merge threads finish), then clear every loaded runtime cache so the next
    /// access reopens the tenant from the freshly published on-disk generation.
    /// The worker's retained `channel_closed` / `merge_quiesced` writer-lifecycle
    /// event makes the ordered drain observable to callers and tests, and must be
    /// recorded before any publication checkpoint the caller emits.
    ///
    /// Runtime state is cleared even when the drain reports an error so a failed
    /// commit cannot leave a stale cached generation behind; the drain error is
    /// returned so every caller aborts before reading, publishing, or removing
    /// the tenant directory without the quiesce guarantee.
    ///
    /// The returned [`TenantQuiesce`] guard is the closing-worker admission
    /// handoff: admission stays fenced until the caller finishes reading,
    /// replacing, or removing the tenant tree and drops it.
    pub async fn quiesce_tenant(&self, tenant_id: &TenantId) -> Result<TenantQuiesce> {
        let target = PublicationTarget::new(tenant_id.as_str())?;
        // Fence admission *before* dropping the sender. Between sender removal and
        // the worker's merge-quiescent close there is no queue for a write to land
        // in, so an unfenced admission would spawn a replacement writer behind the
        // drain and race the caller's read/replace/remove of the same tree.
        let admission_fence = self
            .fence_quiesce_publication_admission(tenant_id, target)
            .await?;
        self.finish_tenant_quiesce(
            tenant_id,
            TenantQuiesceFence::Admission {
                _fence: admission_fence,
            },
        )
        .await
    }

    async fn quiesce_replacement_tenant(
        &self,
        tenant_id: &str,
        target: &PublicationTarget,
    ) -> Result<ReplacementTenantQuiesce> {
        let epoch_fence = self.advance_destination_publication_epoch(tenant_id, target)?;
        let tenant_id = tenant_id.to_string();
        let quiesce = self
            .finish_tenant_quiesce_preserving_runtime(
                &tenant_id,
                TenantQuiesceFence::Epoch(epoch_fence),
            )
            .await?;
        Ok(ReplacementTenantQuiesce { quiesce })
    }

    async fn finish_tenant_quiesce(
        &self,
        tenant_id: &TenantId,
        publication_fence: TenantQuiesceFence,
    ) -> Result<TenantQuiesce> {
        self.invalidate_facet_cache(tenant_id);
        let drain_result = self.drain_until_no_live_writer(tenant_id).await;
        self.clear_tenant_runtime_state(tenant_id);
        #[cfg(debug_assertions)]
        crate::index::write_queue::record_writer_lifecycle_publication_checkpoint(
            tenant_id,
            "manager_quiesce_admission_fenced",
        );
        drain_result.map(|()| TenantQuiesce {
            #[cfg(feature = "vector-search")]
            tenant_id: tenant_id.clone(),
            _publication_fence: publication_fence,
            #[cfg(feature = "vector-search")]
            vector_indices: Arc::clone(&self.vector_indices),
        })
    }

    async fn finish_tenant_quiesce_preserving_runtime(
        &self,
        tenant_id: &TenantId,
        publication_fence: TenantQuiesceFence,
    ) -> Result<TenantQuiesce> {
        self.invalidate_facet_cache(tenant_id);
        self.drain_until_no_live_writer(tenant_id).await?;
        #[cfg(debug_assertions)]
        crate::index::write_queue::record_writer_lifecycle_publication_checkpoint(
            tenant_id,
            "manager_quiesce_admission_fenced",
        );
        Ok(TenantQuiesce {
            #[cfg(feature = "vector-search")]
            tenant_id: tenant_id.clone(),
            _publication_fence: publication_fence,
            #[cfg(feature = "vector-search")]
            vector_indices: Arc::clone(&self.vector_indices),
        })
    }

    /// Drain the tenant until it has no live write worker at all.
    ///
    /// A read that loads the tenant creates its write queue eagerly, and it can pass
    /// its fence check in the instant before the fence is registered — so a single
    /// drain can retire the worker it saw while a straggler is still being inserted.
    /// The fence bounds this: no admission and no further load-path creation can
    /// start once it is held, so the loop converges on the workers that were already
    /// in flight when it was taken.
    async fn drain_until_no_live_writer(&self, tenant_id: &TenantId) -> Result<()> {
        loop {
            self.drain_target_write_queue(tenant_id).await?;
            if !self.write_task_handles.contains_key(tenant_id) {
                return Ok(());
            }
        }
    }

    async fn fence_quiesce_publication_admission(
        &self,
        tenant_id: &TenantId,
        target: PublicationTarget,
    ) -> Result<publication::PublicationAdmissionFence> {
        let base_path = self.base_path.clone();
        let fence_tenant_id = tenant_id.clone();
        tokio::task::spawn_blocking(move || {
            publication::fence_publication_admission(&base_path, &target).map_err(|error| {
                FlapjackError::Io(format!(
                    "tenant quiesce admission fence failed for {fence_tenant_id}: {error}"
                ))
            })
        })
        .await
        .map_err(|error| {
            FlapjackError::Io(format!(
                "tenant quiesce admission fence task failed for {tenant_id}: {error}"
            ))
        })?
    }
}

fn replace_directory(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }
    copy_dir_recursive(source, destination)
}

fn version_store_alignment_error(error: VersionStoreError) -> FlapjackError {
    FlapjackError::Io(format!("version-store alignment failed: {error}"))
}

fn stage_tenant_import(
    base_path: &Path,
    tenant_id: TenantId,
    source: &Path,
) -> Result<PreStagedPublication> {
    let target = PublicationTarget::new(tenant_id.clone())?;
    let publication = PreStagedPublication::prepare(base_path, target)?;
    #[cfg(test)]
    let hook = import_staging_proof_hook().lock().unwrap().clone();
    #[cfg(test)]
    if let Some(hook) = hook {
        hook(&tenant_id);
    }
    if let Err(error) = copy_dir_recursive(source, &publication.paths().staging) {
        if let Err(cleanup_error) = publication.abort() {
            return Err(FlapjackError::Io(format!(
                "tenant import staging failed: {error}; staging cleanup failed: {cleanup_error}"
            )));
        }
        return Err(error);
    }
    Ok(publication)
}

async fn abort_import_after_quiesce_failure(
    publication: PreStagedPublication,
    tenant_id: &str,
    quiesce_error: FlapjackError,
) -> FlapjackError {
    match tokio::task::spawn_blocking(move || publication.abort()).await {
        Ok(Ok(())) => quiesce_error,
        Ok(Err(cleanup_error)) => FlapjackError::Io(format!(
            "tenant import quiesce failed for {tenant_id}: {quiesce_error}; staging cleanup failed: {cleanup_error}"
        )),
        Err(join_error) => FlapjackError::Io(format!(
            "tenant import quiesce failed for {tenant_id}: {quiesce_error}; staging cleanup task failed: {join_error}"
        )),
    }
}

/// RAII proof that a tenant is quiesced.
///
/// While this guard is alive the tenant has no live persistent writer and its
/// write admission is fenced, so the holder can read, replace, or remove the tenant
/// tree knowing no writer can appear behind it. Dropping the guard re-opens
/// admission; the next admitted write creates the replacement worker.
#[must_use = "the tenant is only quiesced while this guard is held"]
pub struct TenantQuiesce {
    #[cfg(feature = "vector-search")]
    tenant_id: TenantId,
    _publication_fence: TenantQuiesceFence,
    #[cfg(feature = "vector-search")]
    vector_indices:
        Arc<DashMap<TenantId, Arc<std::sync::RwLock<crate::vector::index::VectorIndex>>>>,
}

#[cfg(feature = "vector-search")]
impl Drop for TenantQuiesce {
    fn drop(&mut self) {
        self.vector_indices.remove(&self.tenant_id);
    }
}

fn ensure_committed_move(journal: &publication::PublicationJournal) -> Result<()> {
    if journal.phase == PublicationPhase::Committed {
        Ok(())
    } else {
        Err(FlapjackError::InvalidQuery(format!(
            "move_index publication returned non-committed journal phase {:?}",
            journal.phase
        )))
    }
}

fn pre_staged_activation_error(error: PreStagedActivationError) -> FlapjackError {
    match error.source() {
        Some(source) => FlapjackError::InvalidQuery(format!(
            "pre-staged replacement activation failed at {:?}: {source}",
            error.stage()
        )),
        None => FlapjackError::InvalidQuery(format!(
            "pre-staged replacement activation failed at {:?}",
            error.stage()
        )),
    }
}
