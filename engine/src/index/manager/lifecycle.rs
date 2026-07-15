use super::*;
use publication::{
    activate_publication, PublicationArtifactPlan, PublicationGenerationEvidence, PublicationPaths,
    PublicationPhase, PublicationTarget, PublicationTransactionId, TantivyManagedInventory,
};
#[cfg(test)]
use publication::{activate_publication_for_test, PublicationFaultPoint};

impl super::IndexManager {
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

        if let Some((_, handle)) = self.write_task_handles.remove(tenant_id) {
            let _ = handle.await;
        }

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
        self.move_index_with_publication(source, destination, None)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn move_index_with_fault_for_test(
        &self,
        source: &str,
        destination: &str,
        fault: PublicationFaultPoint,
    ) -> Result<TaskInfo> {
        self.move_index_with_publication(source, destination, Some(fault))
            .await
    }

    async fn move_index_with_publication(
        &self,
        source: &str,
        destination: &str,
        #[cfg(test)] fault: Option<PublicationFaultPoint>,
        #[cfg(not(test))] _fault: Option<()>,
    ) -> Result<TaskInfo> {
        validate_index_name(source)?;
        validate_index_name(destination)?;
        let src_path = self.base_path.join(source);
        if !src_path.exists() {
            return self.make_noop_task(source);
        }

        self.unload(&source.to_string())?;
        self.unload(&destination.to_string())?;

        let target = PublicationTarget::new(destination)?;
        let transaction =
            PublicationTransactionId::new(format!("move_{}", uuid::Uuid::new_v4().simple()))?;
        let paths = PublicationPaths::new(&self.base_path, &target, &transaction);
        std::fs::create_dir_all(paths.staging.parent().ok_or_else(|| {
            FlapjackError::InvalidQuery("publication staging path has no parent".into())
        })?)?;
        if paths.staging.exists() {
            std::fs::remove_dir_all(&paths.staging)?;
        }
        copy_dir_recursive(&src_path, &paths.staging)?;

        let inventory = TantivyManagedInventory::from_existing_trees([
            paths.staging.as_path(),
            paths.target.as_path(),
        ])?;
        let artifacts = PublicationArtifactPlan::for_move(
            &self.base_path,
            &self.publication_analytics_config(),
            source,
            &target,
            &transaction,
        )?;
        artifacts.stage()?;
        let generation = PublicationGenerationEvidence::new(format!(
            "move_{}_to_{}_{}",
            source,
            destination,
            uuid::Uuid::new_v4().simple()
        ))?;
        let manifest = artifacts.manifest();
        #[cfg(test)]
        if let Some(fault) = fault {
            let journal = activate_publication_for_test(
                &paths,
                target,
                transaction,
                generation,
                manifest,
                &inventory,
                fault,
            )?;
            ensure_committed_move(&journal)?;
            if fault == PublicationFaultPoint::BeforeSourceCleanup {
                return Err(FlapjackError::InvalidQuery(
                    "injected publication fault before source cleanup".into(),
                ));
            }
            artifacts.remove_source()?;
            std::fs::remove_dir_all(&src_path)?;
            return self.make_noop_task(destination);
        }
        let journal = activate_publication(
            &paths,
            target,
            transaction,
            generation,
            manifest,
            &inventory,
        )?;
        ensure_committed_move(&journal)?;
        artifacts.remove_source()?;
        std::fs::remove_dir_all(&src_path)?;
        self.make_noop_task(destination)
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
        // Drop all senders — receivers will get None and flush pending ops
        self.write_queues.clear();

        // Drain and await all write task handles
        let handles: Vec<_> = self
            .write_task_handles
            .iter()
            .map(|r| r.key().clone())
            .collect();
        for tenant_id in handles {
            if let Some((_, handle)) = self.write_task_handles.remove(&tenant_id) {
                Self::log_write_queue_shutdown_outcome(&tenant_id, handle.await);
            }
        }
    }

    /// Log the result of a write-queue task shutdown: success, application error,
    /// or task panic.
    fn log_write_queue_shutdown_outcome(
        tenant_id: &str,
        shutdown_result: std::result::Result<Result<()>, tokio::task::JoinError>,
    ) {
        match shutdown_result {
            Ok(Ok(())) => {
                tracing::info!("[shutdown] Write queue for '{}' drained", tenant_id);
            }
            Ok(Err(error)) => {
                tracing::error!(
                    "[shutdown] Write queue for '{}' exited with error: {}",
                    tenant_id,
                    error
                );
            }
            Err(join_error) => {
                tracing::error!(
                    "[shutdown] Write queue task for '{}' panicked: {}",
                    tenant_id,
                    join_error
                );
            }
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
