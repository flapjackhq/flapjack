use super::*;
#[cfg(test)]
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Default deadline for durable HTTP writes, in milliseconds.
///
/// Overridable at runtime via `FLAPJACK_WRITE_DURABLE_TIMEOUT_MS`. Bounds how long
/// a durable write handler will wait for the write-queue consumer to commit before
/// returning a retriable `WriteAckTimeout` (503). The default is generous so normal
/// commits never trip it; the bound exists only to keep an HTTP request from hanging
/// forever if the consumer task dies mid-restart (PL-13 silent-drop failure mode).
const DEFAULT_WRITE_DURABLE_TIMEOUT_MS: u64 = 30_000;

#[derive(Clone, Copy)]
enum WriteAdmissionMode {
    Live,
    Durable,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteAdmissionCheckpoint {
    Captured,
    Validated,
}

#[cfg(test)]
type WriteAdmissionCheckpointHook =
    Arc<dyn Fn(&str, WriteAdmissionCheckpoint) + Send + Sync + 'static>;

#[cfg(test)]
static WRITE_ADMISSION_CHECKPOINT_HOOK: OnceLock<Mutex<Option<WriteAdmissionCheckpointHook>>> =
    OnceLock::new();

#[cfg(test)]
pub(crate) struct WriteAdmissionCheckpointHookGuard {
    previous: Option<WriteAdmissionCheckpointHook>,
}

#[cfg(test)]
impl Drop for WriteAdmissionCheckpointHookGuard {
    fn drop(&mut self) {
        *write_admission_checkpoint_hook().lock().unwrap() = self.previous.take();
    }
}

#[cfg(test)]
fn write_admission_checkpoint_hook() -> &'static Mutex<Option<WriteAdmissionCheckpointHook>> {
    WRITE_ADMISSION_CHECKPOINT_HOOK.get_or_init(|| Mutex::new(None))
}

impl super::IndexManager {
    #[cfg(test)]
    pub(crate) fn set_write_admission_checkpoint_hook_for_test(
        hook: impl Fn(&str, WriteAdmissionCheckpoint) + Send + Sync + 'static,
    ) -> WriteAdmissionCheckpointHookGuard {
        let mut slot = write_admission_checkpoint_hook().lock().unwrap();
        WriteAdmissionCheckpointHookGuard {
            previous: slot.replace(Arc::new(hook)),
        }
    }

    #[cfg(test)]
    fn run_write_admission_checkpoint_for_test(
        tenant_id: &str,
        checkpoint: WriteAdmissionCheckpoint,
    ) {
        let hook = write_admission_checkpoint_hook().lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(tenant_id, checkpoint);
        }
    }

    fn get_or_create_admission_store(&self, tenant_id: &str) -> Result<Arc<WriteAdmissionStore>> {
        if let Some(store) = self.admission_stores.get(tenant_id) {
            return Ok(Arc::clone(store.value()));
        }

        let store = Arc::new(WriteAdmissionStore::open(&self.base_path, tenant_id)?);
        Ok(Arc::clone(
            self.admission_stores
                .entry(tenant_id.to_string())
                .or_insert(store)
                .value(),
        ))
    }

    /// Parse a canonical task key (`task_<tenant>_<suffix>`) and return the tenant id.
    ///
    /// Numeric aliases and malformed IDs return `None`.
    fn tenant_id_from_task_key(task_id: &str) -> Option<&str> {
        let remainder = task_id.strip_prefix("task_")?;
        let (tenant_id, _) = remainder.rsplit_once('_')?;
        if tenant_id.is_empty() {
            None
        } else {
            Some(tenant_id)
        }
    }

    fn error_from_terminal_task_failure(message: &str) -> FlapjackError {
        if let Some((current, max)) = parse_writer_contention_failure(message) {
            return FlapjackError::TooManyConcurrentWrites { current, max };
        }
        FlapjackError::Tantivy(message.to_string())
    }

    /// Get or create a write queue for the given tenant.
    ///
    /// DRY helper — all write paths (add, delete, compact) go through this.
    /// Handles oplog creation, write queue spawning, and vector context setup.
    pub(super) fn get_or_create_write_queue(
        &self,
        tenant_id: &str,
        index: &Arc<Index>,
    ) -> Result<WriteQueue> {
        if let Some(queue) = self.write_queues.get(tenant_id) {
            return Ok(queue.clone());
        }

        let oplog = self.get_or_create_oplog_result(tenant_id)?;
        let admission_store = self.get_or_create_admission_store(tenant_id)?;
        let entry = self
            .write_queues
            .entry(tenant_id.to_string())
            .or_try_insert_with(|| -> Result<WriteQueue> {
                #[cfg(feature = "vector-search")]
                let vector_ctx = VectorWriteContext::new(Arc::clone(&self.vector_indices));
                #[cfg(not(feature = "vector-search"))]
                let vector_ctx = VectorWriteContext::new();
                let (queue, handle) = create_write_queue(WriteQueueContext {
                    tenant_id: tenant_id.to_string(),
                    index: Arc::clone(index),
                    _writers: Arc::clone(&self.writers),
                    tasks: Arc::clone(&self.tasks),
                    base_path: self.base_path.clone(),
                    oplog: Some(Arc::clone(&oplog)),
                    admission_store: Arc::clone(&admission_store),
                    facet_cache: Arc::clone(&self.facet_cache),
                    lww_map: Arc::clone(&self.lww_map),
                    vector_ctx,
                })?;
                self.write_task_handles
                    .insert(tenant_id.to_string(), WriteTaskHandle::new(handle));
                Ok(queue)
            })?;
        Ok(entry.clone())
    }

    /// Add documents to a tenant's index.
    ///
    /// Creates a writer, adds documents, and commits immediately.
    /// For production, this should be batched via background commit thread.
    pub fn add_documents_insert(&self, tenant_id: &str, docs: Vec<Document>) -> Result<TaskInfo> {
        self.add_documents_inner(tenant_id, docs, false, false)
    }

    pub fn add_documents(&self, tenant_id: &str, docs: Vec<Document>) -> Result<TaskInfo> {
        self.add_documents_inner(tenant_id, docs, true, false)
    }

    /// Like `add_documents` but uses `UpsertNoLwwUpdate` so the write_queue does NOT
    /// overwrite lww_map entries — for use by replication (apply_ops_to_manager) which
    /// has already recorded the correct op timestamp in lww_map before calling this.
    pub fn add_documents_for_replication(
        &self,
        tenant_id: &str,
        docs: Vec<Document>,
    ) -> Result<TaskInfo> {
        self.add_documents_inner(tenant_id, docs, true, true)
    }

    /// Core document-add path: load the tenant index, create a task, evict stale
    /// tasks, and send an Add/Upsert/UpsertNoLwwUpdate `WriteOp` to the write
    /// queue. Returns `QueueFull` on backpressure.
    fn add_documents_inner(
        &self,
        tenant_id: &str,
        docs: Vec<Document>,
        upsert: bool,
        no_lww_update: bool,
    ) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;

        let actions = if upsert {
            if no_lww_update {
                docs.into_iter()
                    .map(WriteAction::UpsertNoLwwUpdate)
                    .collect()
            } else {
                docs.into_iter().map(WriteAction::Upsert).collect()
            }
        } else {
            docs.into_iter().map(WriteAction::Add).collect()
        };
        self.admit_write_actions(tenant_id, &index, actions, WriteAdmissionMode::Live)
    }

    /// Queue document deletions by object ID with LWW tracking. Creates a task
    /// and sends `Delete` actions to the tenant's write queue.
    pub fn delete_documents(&self, tenant_id: &str, object_ids: Vec<String>) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;

        let actions = object_ids.into_iter().map(WriteAction::Delete).collect();
        self.admit_write_actions(tenant_id, &index, actions, WriteAdmissionMode::Live)
    }

    /// Test-only seam: abort a tenant's write task to simulate a restart after
    /// enqueue and before durable commit acknowledgment.
    ///
    /// Returns `true` when a task handle existed and was aborted; `false` when no
    /// active task handle was registered for the tenant.
    pub fn abort_tenant_write_task_for_test(&self, tenant_id: &str) -> bool {
        if let Some((_, handle)) = self.write_task_handles.remove(tenant_id) {
            handle.abort();
            true
        } else {
            false
        }
    }

    /// Test-only seam: snapshot string-keyed write tasks for one tenant so
    /// integration tests can synchronize on "task accepted and queued" before
    /// inducing a write-task abort.
    pub fn tenant_tasks_snapshot_for_test(&self, tenant_id: &str) -> Vec<TaskInfo> {
        let prefix = format!("task_{}_", tenant_id);
        let mut tasks: Vec<TaskInfo> = self
            .tasks
            .iter()
            .filter(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.value().clone())
            .collect();
        tasks.sort_by_key(|task| task.created_at);
        tasks
    }

    /// Queue document deletions without updating the LWW map — the caller
    /// (replication) has already recorded the correct timestamps before queuing.
    pub fn delete_documents_for_replication(
        &self,
        tenant_id: &str,
        object_ids: Vec<String>,
    ) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;

        let actions = object_ids
            .into_iter()
            .map(WriteAction::DeleteNoLwwUpdate)
            .collect();
        self.admit_write_actions(tenant_id, &index, actions, WriteAdmissionMode::Live)
    }

    /// Queue a segment compaction for the tenant. Creates a task and sends a
    /// single `Compact` action to the write queue.
    pub fn compact_index(&self, tenant_id: &str) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;

        self.admit_write_actions(
            tenant_id,
            &index,
            vec![WriteAction::Compact],
            WriteAdmissionMode::Live,
        )
    }

    fn admit_write_actions(
        &self,
        tenant_id: &str,
        index: &Arc<Index>,
        actions: Vec<WriteAction>,
        admission_mode: WriteAdmissionMode,
    ) -> Result<TaskInfo> {
        let target = publication::PublicationTarget::new(tenant_id)?;
        let observed_epoch = publication::capture_publication_epoch(&self.base_path, &target)
            .map_err(|error| Self::admission_epoch_error(tenant_id, error))?;
        #[cfg(test)]
        Self::run_write_admission_checkpoint_for_test(
            tenant_id,
            WriteAdmissionCheckpoint::Captured,
        );
        if let Some(tx) = self.write_queues.get(tenant_id).map(|queue| queue.clone()) {
            // Preserve the pre-admission API contract from `try_send`: callers can retry
            // both capacity pressure and a queue consumer that is being restarted.
            let permit = tx.try_reserve().map_err(|_| FlapjackError::QueueFull)?;
            let admission_guard = publication::try_validate_publication_epoch_admission(
                &self.base_path,
                &target,
                observed_epoch,
            )
            .map_err(|error| Self::admission_epoch_error(tenant_id, error))?;
            #[cfg(test)]
            Self::run_write_admission_checkpoint_for_test(
                tenant_id,
                WriteAdmissionCheckpoint::Validated,
            );
            return self.send_admitted_write(
                tenant_id,
                actions,
                admission_mode,
                admission_guard,
                permit,
            );
        }
        let admission_guard = publication::try_validate_publication_epoch_admission(
            &self.base_path,
            &target,
            observed_epoch,
        )
        .map_err(|error| Self::admission_epoch_error(tenant_id, error))?;
        #[cfg(test)]
        Self::run_write_admission_checkpoint_for_test(
            tenant_id,
            WriteAdmissionCheckpoint::Validated,
        );
        let tx = self.get_or_create_write_queue(tenant_id, index)?;
        // Preserve the pre-admission API contract from `try_send`: callers can retry
        // both capacity pressure and a queue consumer that is being restarted.
        let permit = tx.try_reserve().map_err(|_| FlapjackError::QueueFull)?;

        self.send_admitted_write(tenant_id, actions, admission_mode, admission_guard, permit)
    }

    fn send_admitted_write(
        &self,
        tenant_id: &str,
        actions: Vec<WriteAction>,
        admission_mode: WriteAdmissionMode,
        admission_guard: publication::PublicationEpochAdmissionGuard,
        permit: tokio::sync::mpsc::Permit<'_, WriteOp>,
    ) -> Result<TaskInfo> {
        let numeric_id = self.next_numeric_task_id();
        let task_id = format!("task_{}_{}", tenant_id, uuid::Uuid::new_v4());
        let received_documents = actions.len();
        let ticket = WriteAdmissionTicket::new(tenant_id.to_string(), admission_guard.observed());
        let record = WriteAdmissionRecord::new(
            ticket,
            task_id.clone(),
            numeric_id,
            received_documents,
            actions,
        );
        let record = match admission_mode {
            WriteAdmissionMode::Live => record,
            WriteAdmissionMode::Durable => self
                .get_or_create_admission_store(tenant_id)?
                .append_record(record)?,
        };
        let task = record.task_info();
        self.tasks.insert(task_id.clone(), task.clone());
        self.tasks.insert(numeric_id.to_string(), task.clone());
        self.evict_old_tasks(tenant_id, MAX_TASKS_PER_TENANT);

        permit.send(record.write_op());
        drop(admission_guard);
        Ok(task)
    }

    fn admission_epoch_error(
        tenant_id: &str,
        error: publication::PublicationEpochAdmissionError,
    ) -> FlapjackError {
        match error {
            publication::PublicationEpochAdmissionError::Busy
            | publication::PublicationEpochAdmissionError::Stale { .. } => {
                FlapjackError::IndexPaused(tenant_id.to_string())
            }
            publication::PublicationEpochAdmissionError::Epoch(
                publication::PublicationEpochError::CorruptState { path },
            ) => FlapjackError::Io(format!(
                "publication epoch admission evidence is corrupt for {tenant_id} at {}",
                path.display()
            )),
            publication::PublicationEpochAdmissionError::Epoch(
                publication::PublicationEpochError::Io { path, source },
            ) => FlapjackError::Io(format!(
                "publication epoch admission evidence failed for {tenant_id} at {}: {source}",
                path.display()
            )),
            publication::PublicationEpochAdmissionError::Epoch(error) => FlapjackError::Io(
                format!("publication epoch admission failed for {tenant_id}: {error}"),
            ),
        }
    }

    /// Resolve the durable-write deadline from `FLAPJACK_WRITE_DURABLE_TIMEOUT_MS`,
    /// falling back to [`DEFAULT_WRITE_DURABLE_TIMEOUT_MS`] when unset or unparseable.
    fn durable_write_timeout() -> Duration {
        let ms = std::env::var("FLAPJACK_WRITE_DURABLE_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_WRITE_DURABLE_TIMEOUT_MS);
        Duration::from_millis(ms)
    }

    /// Poll a task's status until it reaches a terminal state, sleeping 10ms between
    /// checks. This is the single source of truth for the "wait until durable" loop
    /// shared by the unbounded `*_sync` helpers and the bounded `*_durable` paths.
    ///
    /// `timeout` selects the waiting policy:
    /// - `None` — poll indefinitely (internal callers and tests rely on this).
    /// - `Some(d)` — return [`FlapjackError::WriteAckTimeout`] if the task has not
    ///   reached a terminal state by `now + d`, so an HTTP write handler cannot hang
    ///   forever when the write-queue consumer dies before committing (PL-13).
    ///
    /// A terminal `Succeeded` resolves to `Ok(())`; a terminal `Failed` propagates the
    /// underlying message as [`FlapjackError::Tantivy`], which maps to a 5xx response.
    async fn await_task_terminal(&self, task_id: &str, timeout: Option<Duration>) -> Result<()> {
        let deadline = timeout.map(|d| Instant::now() + d);
        loop {
            let status = self.get_task(task_id)?;
            match &status.status {
                TaskStatus::Enqueued | TaskStatus::Processing => {
                    if let Some(deadline) = deadline {
                        if Instant::now() >= deadline {
                            return Err(FlapjackError::WriteAckTimeout);
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                TaskStatus::Succeeded => {
                    // Sweep terminal overflow as soon as a write reaches completion so
                    // idle tenants do not stay above retention cap until another write.
                    if let Some(tenant_id) = Self::tenant_id_from_task_key(task_id) {
                        self.evict_old_tasks(tenant_id, MAX_TASKS_PER_TENANT);
                        // Preserve the just-observed terminal task so the caller's
                        // returned taskID remains immediately queryable via both
                        // canonical and numeric alias lookup paths.
                        if !self.tasks.contains_key(task_id) {
                            self.tasks.insert(task_id.to_string(), status.clone());
                            self.tasks
                                .insert(status.numeric_id.to_string(), status.clone());
                        }
                    }
                    return Ok(());
                }
                TaskStatus::Failed(e) => {
                    if let Some(tenant_id) = Self::tenant_id_from_task_key(task_id) {
                        self.evict_old_tasks(tenant_id, MAX_TASKS_PER_TENANT);
                        if !self.tasks.contains_key(task_id) {
                            self.tasks.insert(task_id.to_string(), status.clone());
                            self.tasks
                                .insert(status.numeric_id.to_string(), status.clone());
                        }
                    }
                    return Err(Self::error_from_terminal_task_failure(e));
                }
            }
        }
    }

    /// Wait until a queued write task is durably committed to Tantivy, bounded by
    /// `FLAPJACK_WRITE_DURABLE_TIMEOUT_MS` (default 30s).
    ///
    /// This is the bounded counterpart to the unbounded `*_sync` poll. HTTP write
    /// handlers call it after enqueuing so they can report durability while still
    /// holding the enqueued [`TaskInfo`] — letting a failure response carry the
    /// `taskID` per the Algolia write contract. Returns
    /// [`FlapjackError::WriteAckTimeout`] (503) if the consumer does not ack within
    /// the deadline, or the underlying commit error (5xx) if the commit failed.
    pub async fn wait_for_write_durable(&self, task_id: &str) -> Result<()> {
        self.await_task_terminal(task_id, Some(Self::durable_write_timeout()))
            .await
    }

    /// Add documents and wait until the write queue has durably committed them to
    /// Tantivy, bounded by `FLAPJACK_WRITE_DURABLE_TIMEOUT_MS` (default 30s).
    ///
    /// HTTP add handlers use this instead of fire-and-forget [`add_documents`] so a
    /// 200 response means the write is on disk — closing the PL-13 silent-drop where
    /// an enqueued-but-uncommitted write was ACKed before the consumer committed it.
    /// Returns [`FlapjackError::QueueFull`] (429) on backpressure,
    /// [`FlapjackError::WriteAckTimeout`] (503) if the consumer does not ack in time,
    /// or the underlying commit error (5xx). Replication paths intentionally keep
    /// using the fire-and-forget variant and are not routed through here.
    pub async fn add_documents_durable(
        &self,
        tenant_id: &str,
        docs: Vec<Document>,
    ) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;
        let actions = docs.into_iter().map(WriteAction::Upsert).collect();
        let task =
            self.admit_write_actions(tenant_id, &index, actions, WriteAdmissionMode::Durable)?;
        self.wait_for_write_durable(&task.id).await?;
        Ok(task)
    }

    /// Delete documents and wait until the write queue has durably committed the
    /// deletes to Tantivy, bounded by `FLAPJACK_WRITE_DURABLE_TIMEOUT_MS`.
    ///
    /// User-thread delete handlers use this so an accepting-node restart yields a
    /// bounded retriable timeout instead of an unbounded hang.
    pub async fn delete_documents_durable(
        &self,
        tenant_id: &str,
        object_ids: Vec<String>,
    ) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;
        let actions = object_ids.into_iter().map(WriteAction::Delete).collect();
        let task =
            self.admit_write_actions(tenant_id, &index, actions, WriteAdmissionMode::Durable)?;
        self.wait_for_write_durable(&task.id).await?;
        Ok(task)
    }

    /// Compact an index and wait for the operation to complete.
    pub async fn compact_index_sync(&self, tenant_id: &str) -> Result<()> {
        let task = self.compact_index(tenant_id)?;
        self.await_task_terminal(&task.id, None).await
    }

    /// Insert documents (non-upsert) and poll until the task succeeds or fails.
    /// Async wrapper around `add_documents_insert`.
    pub async fn add_documents_insert_sync(
        &self,
        tenant_id: &str,
        docs: Vec<Document>,
    ) -> Result<()> {
        let task = self.add_documents_insert(tenant_id, docs)?;
        self.await_task_terminal(&task.id, None).await
    }

    pub async fn add_documents_sync(&self, tenant_id: &str, docs: Vec<Document>) -> Result<()> {
        let task = self.add_documents(tenant_id, docs)?;
        self.await_task_terminal(&task.id, None).await
    }

    /// Delete documents and poll until the task succeeds or fails. Async wrapper
    /// around `delete_documents`.
    pub async fn delete_documents_sync(
        &self,
        tenant_id: &str,
        object_ids: Vec<String>,
    ) -> Result<()> {
        let task = self.delete_documents(tenant_id, object_ids)?;
        self.await_task_terminal(&task.id, None).await
    }

    /// Like `delete_documents_sync` but skips lww_map update in write_queue — for replication.
    pub async fn delete_documents_sync_for_replication(
        &self,
        tenant_id: &str,
        object_ids: Vec<String>,
    ) -> Result<()> {
        let task = self.delete_documents_for_replication(tenant_id, object_ids)?;
        self.await_task_terminal(&task.id, None).await
    }
}

fn parse_writer_contention_failure(message: &str) -> Option<(usize, usize)> {
    let details = message.strip_prefix("Too many concurrent writes: ")?;
    let (current, max) = details.split_once(" active, max ")?;
    Some((current.parse().ok()?, max.parse().ok()?))
}

#[cfg(test)]
#[path = "write_mutation_fence_tests.rs"]
mod mutation_fence;
