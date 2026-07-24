use super::recovery::RecoverySeqWindow;
use super::*;
use crate::index::oplog::{read_committed_seq, write_committed_seq, OpLog, OpLogEntry, OPLOG_DIR};
#[cfg(test)]
use publication::{activate_publication_for_test, PublicationFaultPoint};
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
pub(crate) struct ReplacementReopenProofHookGuard {
    previous: Option<ReplacementReopenProofHook>,
}

#[cfg(test)]
impl Drop for ReplacementReopenProofHookGuard {
    fn drop(&mut self) {
        *replacement_reopen_proof_hook().lock().unwrap() = self.previous.take();
    }
}

#[cfg(test)]
fn replacement_reopen_proof_hook() -> &'static StdMutex<Option<ReplacementReopenProofHook>> {
    REPLACEMENT_REOPEN_PROOF_HOOK.get_or_init(|| StdMutex::new(None))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublicationArtifactMode {
    MoveWithSource,
    PreserveDestination,
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
            self.get_or_create_write_queue(tenant_id, &index)?;
            self.cache_loaded_index(tenant_id, index);
            #[cfg(feature = "vector-search")]
            self.load_vector_index(tenant_id, &path);
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

        Ok(())
    }

    /// Remove a tenant from the loaded cache without touching on-disk state.
    pub fn unload_tenant(&self, tenant_id: &str) {
        self.loaded.remove(tenant_id);
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
        self.writers.remove(tenant_id);
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
    /// Unloads the tenant, stops its write task, and removes the directory. Retries removal up to 10 times (50ms intervals) to handle Tantivy merge threads that may still be writing after the IndexWriter is dropped.
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
        self.invalidate_facet_cache(tenant_id);
        self.write_queues.remove(tenant_id);

        if let Some(handle) = self
            .write_task_handles
            .get(tenant_id)
            .map(|entry| entry.value().clone())
        {
            let _ = handle.drain(tenant_id.clone()).await;
            self.write_task_handles
                .remove_if(tenant_id, |_, current| current.same_handle(&handle));
        }

        self.admission_stores.remove(tenant_id);
        self.clear_tenant_runtime_state(tenant_id);

        let path = self.base_path.join(tenant_id);
        if !path.exists() {
            return Err(FlapjackError::TenantNotFound(tenant_id.to_string()));
        }

        // Retry remove_dir_all to handle Tantivy merge threads that may still
        // be writing segment files after the IndexWriter is dropped. The drop
        // signals merge threads to stop but doesn't wait for them to finish.
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
        Ok(())
    }

    /// Queue an asynchronous tenant export to the given destination path. Creates a
    /// task, sends an `Export` command to the task queue, and returns the task ID
    /// for polling.
    pub fn export_tenant(&self, tenant_id: &TenantId, dest_path: PathBuf) -> Result<String> {
        validate_index_name(tenant_id)?;
        let numeric_id = self.next_numeric_task_id();
        let task_id = format!("export_{}_{}", tenant_id, uuid::Uuid::new_v4());
        let task = TaskInfo::new(task_id.clone(), numeric_id, 0);
        self.tasks.insert(task_id.clone(), task.clone());
        self.tasks.insert(numeric_id.to_string(), task.clone());

        let tenant_id_clone = tenant_id.clone();
        let sender = self.task_queue.sender.clone();
        let task_id_clone = task_id.clone();

        tokio::spawn(async move {
            let _ = sender
                .send(crate::index::task_queue::TaskCommand::Export {
                    task_id: task_id_clone,
                    tenant_id: tenant_id_clone,
                    dest_path,
                })
                .await;
        });

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
    /// Copies the directory to the base path under the tenant ID.
    /// Does NOT load the index (caller must call get_or_load).
    pub fn import_tenant(&self, tenant_id: &TenantId, src_path: &Path) -> Result<()> {
        validate_index_name(tenant_id)?;
        let dest_path = self.base_path.join(tenant_id);
        std::fs::create_dir_all(&dest_path)?;

        copy_dir_recursive(src_path, &dest_path)?;

        Ok(())
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
        validate_index_name(destination)?;
        let source_path = publication.paths().staging.clone();
        if !source_path.exists() {
            return self.make_noop_task(destination);
        }
        let target = PublicationTarget::new(destination)?;
        let publication_epoch_fence =
            self.advance_destination_publication_epoch(destination, &target)?;

        self.drain_target_write_queue(&destination.to_string())
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
            publication_epoch_fence.previous(),
            publication_epoch_fence.advanced(),
            staging_baseline,
            PublicationWatermark::new(watermark),
        )?;
        self.invalidate_facet_cache(destination);
        self.clear_tenant_runtime_state(&destination.to_string());

        let mut journal = publication
            .activate_with_fence(fence_evidence)
            .map_err(pre_staged_activation_error)?;
        ensure_committed_move(&journal)?;
        self.run_replacement_reopen_proof_hook(destination, &mut journal);
        self.certify_replacement_reopen(
            destination,
            PublicationArtifactMode::PreserveDestination,
            &journal,
        )?;
        drop(publication_epoch_fence);
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
        let publication_epoch_fence =
            self.advance_destination_publication_epoch(destination, &target)?;

        self.unload(&source.to_string())?;
        self.drain_target_write_queue(&destination.to_string())
            .await?;
        // After the target drain, no old-epoch mutation can still transition to
        // succeeded. Stage 3 proves the strict `committed_seq = W` replacement
        // contract against the quiesced destination and carries the resulting
        // fence evidence into activation, all before any live-target mutation.
        let fence_evidence = self.stage_replacement_fence_evidence(
            source,
            destination,
            artifact_mode,
            staging_baseline,
            &publication_epoch_fence,
        )?;
        self.invalidate_facet_cache(destination);
        self.clear_tenant_runtime_state(&destination.to_string());

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
        drop(publication_epoch_fence);
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
        self.replay_document_ops(
            destination,
            &source_index,
            source_path,
            &document_ops,
            RecoverySeqWindow {
                committed_seq: baseline,
                final_seq: watermark,
            },
            settings.as_ref(),
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
        if staged_oplog_dir.exists() {
            std::fs::remove_dir_all(&staged_oplog_dir)?;
        }
        copy_dir_recursive(&destination_path.join(OPLOG_DIR), &staged_oplog_dir)?;
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
        let handles: Vec<_> = self
            .write_task_handles
            .iter()
            .map(|r| r.key().clone())
            .collect();
        for tenant_id in handles {
            if let Err(error) = self.drain_target_write_queue(&tenant_id).await {
                tracing::error!(
                    "[shutdown] Write queue for '{}' failed: {}",
                    tenant_id,
                    error
                );
            }
        }
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
