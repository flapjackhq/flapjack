use super::*;
#[cfg(test)]
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
// The idle-progress deadline is measured on the same monotonic clock that
// `tokio::time::sleep` (below) drives the poll loop with, so a paused test clock
// controls the deadline and the sleeps together instead of racing wall time.
use tokio::time::Instant;

/// Default durable HTTP-write deadline, overridable through
/// `FLAPJACK_WRITE_DURABLE_TIMEOUT_MS`. It bounds dead-consumer waits without
/// affecting ordinary commits (PL-13 silent-drop failure mode).
const DEFAULT_WRITE_DURABLE_TIMEOUT_MS: u64 = 30_000;
const WRITE_DURABLE_FAIL_CLOSED_GRACE_MS: u64 = 1_000;
const WRITE_DURABLE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy)]
enum WriteAdmissionMode {
    Live,
    Durable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DurableWriteProgress {
    status: TaskStatus,
    received_documents: usize,
    indexed_documents: usize,
}

impl DurableWriteProgress {
    fn from_task(task: &TaskInfo) -> Self {
        Self {
            status: task.status.clone(),
            received_documents: task.received_documents,
            indexed_documents: task.indexed_documents,
        }
    }

    fn advanced_beyond(&self, previous: &Self) -> bool {
        self.status != previous.status
            || self.received_documents > previous.received_documents
            || self.indexed_documents > previous.indexed_documents
    }
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
type LoadWriteQueueCheckpointHook = Arc<dyn Fn(&str) + Send + Sync + 'static>;

#[cfg(test)]
static WRITE_ADMISSION_CHECKPOINT_HOOK: OnceLock<Mutex<Option<WriteAdmissionCheckpointHook>>> =
    OnceLock::new();

#[cfg(test)]
static LOAD_WRITE_QUEUE_CHECKPOINT_HOOK: OnceLock<Mutex<Option<LoadWriteQueueCheckpointHook>>> =
    OnceLock::new();

#[cfg(test)]
pub(crate) struct WriteAdmissionCheckpointHookGuard {
    previous: Option<WriteAdmissionCheckpointHook>,
}

#[cfg(test)]
pub(crate) struct LoadWriteQueueCheckpointHookGuard {
    previous: Option<LoadWriteQueueCheckpointHook>,
}

#[cfg(test)]
impl Drop for WriteAdmissionCheckpointHookGuard {
    fn drop(&mut self) {
        *write_admission_checkpoint_hook().lock().unwrap() = self.previous.take();
    }
}

#[cfg(test)]
impl Drop for LoadWriteQueueCheckpointHookGuard {
    fn drop(&mut self) {
        *load_write_queue_checkpoint_hook().lock().unwrap() = self.previous.take();
    }
}

#[cfg(test)]
fn write_admission_checkpoint_hook() -> &'static Mutex<Option<WriteAdmissionCheckpointHook>> {
    WRITE_ADMISSION_CHECKPOINT_HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn load_write_queue_checkpoint_hook() -> &'static Mutex<Option<LoadWriteQueueCheckpointHook>> {
    LOAD_WRITE_QUEUE_CHECKPOINT_HOOK.get_or_init(|| Mutex::new(None))
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
    pub(crate) fn set_load_write_queue_checkpoint_hook_for_test(
        hook: impl Fn(&str) + Send + Sync + 'static,
    ) -> LoadWriteQueueCheckpointHookGuard {
        let mut slot = load_write_queue_checkpoint_hook().lock().unwrap();
        LoadWriteQueueCheckpointHookGuard {
            previous: slot.replace(Arc::new(hook)),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_write_admission_after_stage_hook_for_test(
        &self,
        tenant_id: &str,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> Result<()> {
        self.get_or_create_admission_store(tenant_id)?
            .set_after_stage_hook(hook);
        Ok(())
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

    #[cfg(test)]
    fn run_load_write_queue_checkpoint_for_test(tenant_id: &str) {
        let hook = load_write_queue_checkpoint_hook().lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(tenant_id);
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

    /// Publish runtime state for a loaded tenant unless publication is fenced.
    ///
    /// Loading is not a write, so a fenced search still receives its opened index.
    /// The reservation covers eager writer creation and cache publication as one
    /// operation: a later fence waits for both, while a load that observes an
    /// existing fence returns without leaving pre-publication runtime state behind.
    /// The write side needs no such check because admission already holds the
    /// publication epoch admission guard.
    pub(super) fn publish_loaded_runtime_state_if_unfenced(
        &self,
        tenant_id: &str,
        index: Arc<Index>,
    ) -> Result<Arc<Index>> {
        let Ok(target) = publication::PublicationTarget::new(tenant_id) else {
            return Ok(index);
        };
        let runtime_index = Arc::clone(&index);
        match publication::run_if_publication_admission_unfenced(&self.base_path, &target, || {
            #[cfg(test)]
            Self::run_load_write_queue_checkpoint_for_test(tenant_id);
            self.get_or_create_write_queue(tenant_id, &runtime_index)?;
            #[cfg(feature = "vector-search")]
            self.load_vector_index(tenant_id, &self.base_path.join(tenant_id));
            Ok(self.cache_loaded_index(tenant_id, runtime_index))
        }) {
            Some(result) => result,
            None => {
                #[cfg(feature = "vector-search")]
                self.load_vector_index(tenant_id, &self.base_path.join(tenant_id));
                Ok(index)
            }
        }
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
                let (queue, handle, cancellation, worker_completion) =
                    create_write_queue(WriteQueueContext {
                        tenant_id: tenant_id.to_string(),
                        index: Arc::clone(index),
                        tasks: Arc::clone(&self.tasks),
                        base_path: self.base_path.clone(),
                        oplog: Some(Arc::clone(&oplog)),
                        admission_store: Arc::clone(&admission_store),
                        facet_cache: Arc::clone(&self.facet_cache),
                        vector_ctx,
                        queue_metrics_id: 0,
                        writer_buffer_size: self.write_queue_writer_buffer_size(),
                        #[cfg(test)]
                        test_overrides: Default::default(),
                    })?;
                self.write_task_handles.insert(
                    tenant_id.to_string(),
                    WriteTaskHandle::new_with_cancellation(handle, cancellation, worker_completion),
                );
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

    /// Queue replicated documents whose legacy admission format has no origin tuple.
    ///
    /// New replication writes use `add_documents_for_replication_with_origins`;
    /// this path remains for durable replay compatibility.
    pub fn add_documents_for_replication(
        &self,
        tenant_id: &str,
        docs: Vec<Document>,
    ) -> Result<TaskInfo> {
        self.add_documents_inner(tenant_id, docs, true, true)
    }

    pub fn add_documents_for_replication_with_origins(
        &self,
        tenant_id: &str,
        docs: Vec<(Document, ReplicatedWriteOrigin)>,
    ) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;
        let actions = docs
            .into_iter()
            .map(|(doc, origin)| WriteAction::UpsertWithOrigin { doc, origin })
            .collect();
        self.admit_write_actions(tenant_id, &index, actions, WriteAdmissionMode::Live)
    }

    /// Core document-add path: load the tenant index, create a task, evict stale
    /// tasks, and send the selected add/upsert `WriteOp` to the write
    /// queue. Returns `QueueFull` on backpressure.
    fn add_documents_inner(
        &self,
        tenant_id: &str,
        docs: Vec<Document>,
        upsert: bool,
        legacy_replication_without_origin: bool,
    ) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;

        let actions = if upsert {
            if legacy_replication_without_origin {
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

    /// Queue document deletions by object ID. Creates a task and sends `Delete`
    /// actions to the tenant's write queue.
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

    /// Queue replicated deletes whose legacy admission format has no origin tuple.
    ///
    /// New replication writes use `delete_documents_for_replication_with_origins`;
    /// this path remains for durable replay compatibility.
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

    pub fn delete_documents_for_replication_with_origins(
        &self,
        tenant_id: &str,
        deletes: Vec<(String, ReplicatedWriteOrigin)>,
    ) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;

        let actions = deletes
            .into_iter()
            .map(|(object_id, origin)| WriteAction::DeleteWithOrigin { object_id, origin })
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
        crate::index::write_queue::backpressure::ensure_bulk_admission_allowed(
            &self.base_path,
            tenant_id,
            index,
        )?;
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
        self.task_retention
            .insert(&self.tasks, tenant_id, task.clone(), MAX_TASKS_PER_TENANT);

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

    /// Single terminal-status loop for unbounded sync calls and bounded durable
    /// calls. A deadline invokes the fail-closed timeout policy; terminal failures
    /// preserve their underlying 5xx error.
    async fn await_task_terminal(&self, task_id: &str, timeout: Option<Duration>) -> Result<()> {
        let mut idle_deadline = timeout.map(|d| Instant::now() + d);
        let mut last_progress = None;
        loop {
            let status = self.get_task(task_id)?;
            let current_progress = DurableWriteProgress::from_task(&status);
            if last_progress
                .as_ref()
                .is_some_and(|previous| current_progress.advanced_beyond(previous))
            {
                idle_deadline = timeout.map(|d| Instant::now() + d);
            }
            last_progress = Some(current_progress);

            match &status.status {
                TaskStatus::Enqueued | TaskStatus::Processing => {
                    if let Some(deadline) = idle_deadline {
                        if Instant::now() >= deadline {
                            return self.resolve_durable_write_timeout(&status).await;
                        }
                    }
                    tokio::time::sleep_until(Instant::now() + WRITE_DURABLE_POLL_INTERVAL).await;
                }
                TaskStatus::Succeeded => {
                    // Sweep terminal overflow as soon as a write reaches completion so
                    // idle tenants do not stay above retention cap until another write.
                    if let Some(tenant_id) = Self::tenant_id_from_task_key(task_id) {
                        self.task_retention.insert(
                            &self.tasks,
                            tenant_id,
                            status.clone(),
                            MAX_TASKS_PER_TENANT,
                        );
                    }
                    return Ok(());
                }
                TaskStatus::Failed(e) => {
                    if let Some(tenant_id) = Self::tenant_id_from_task_key(task_id) {
                        self.task_retention.insert(
                            &self.tasks,
                            tenant_id,
                            status.clone(),
                            MAX_TASKS_PER_TENANT,
                        );
                    }
                    return Err(Self::error_from_terminal_task_failure(e));
                }
            }
        }
    }

    /// Resolve a timed-out write without returning a failure that recovery can contradict.
    ///
    /// A stopped, non-terminal task is an uncommitted task whose original
    /// compensation failed. Retrying the canonical compensation seam makes a 503
    /// safe when cleanup now succeeds. If cleanup still fails, durable admission
    /// remains the source of truth and the write is acknowledged for replay.
    async fn resolve_durable_write_timeout(&self, task: &TaskInfo) -> Result<()> {
        let Some(tenant_id) = Self::tenant_id_from_task_key(&task.id) else {
            return Err(FlapjackError::WriteAckTimeout);
        };
        if let Some(handle) = self
            .write_task_handles
            .get(tenant_id)
            .map(|entry| entry.clone())
        {
            if tokio::time::timeout(
                Duration::from_millis(WRITE_DURABLE_FAIL_CLOSED_GRACE_MS),
                handle.drain(tenant_id.to_string()),
            )
            .await
            .is_err()
            {
                return Err(FlapjackError::WriteAckTimeout);
            }
        } else if self
            .write_queues
            .get(tenant_id)
            .is_some_and(|queue| !queue.is_closed())
        {
            return Err(FlapjackError::WriteAckTimeout);
        }

        match self.get_task(&task.id).map(|current| current.status) {
            Ok(TaskStatus::Succeeded) => return Ok(()),
            Ok(TaskStatus::Failed(message)) => {
                return Err(Self::error_from_terminal_task_failure(&message))
            }
            Ok(TaskStatus::Enqueued | TaskStatus::Processing) => {}
            Err(error) => return Err(error),
        }

        let cleanup_result = self.compensate_stopped_uncommitted_task(tenant_id, &task.id);
        match cleanup_result {
            Ok(()) => Err(FlapjackError::WriteAckTimeout),
            Err(error) => {
                tracing::error!(
                    tenant_id,
                    task_id = %task.id,
                    %error,
                    "late write compensation failed; acknowledging durable admission for replay"
                );
                Ok(())
            }
        }
    }

    fn compensate_stopped_uncommitted_task(&self, tenant_id: &str, task_id: &str) -> Result<()> {
        let admission_store = self.get_or_create_admission_store(tenant_id)?;
        let oplog = self.get_or_create_oplog_result(tenant_id)?;
        crate::index::write_queue::compensate_uncommitted_tasks(
            crate::index::write_queue::DurableReplayState {
                tenant_id,
                admission_store: admission_store.as_ref(),
                oplog: Some(oplog.as_ref()),
            },
            0,
            &[task_id.to_string()],
        )
    }

    /// Bounded durable wait used by HTTP handlers. They retain the enqueued task
    /// while waiting, so failures preserve their `taskID`.
    ///
    /// `FLAPJACK_WRITE_DURABLE_TIMEOUT_MS` owns the deadline. A stopped task returns
    /// 503 only after its replay routes are removed; persistent cleanup failure is
    /// acknowledged for recovery. Active writes retain the retryable timeout.
    pub async fn wait_for_write_durable(&self, task_id: &str) -> Result<()> {
        self.await_task_terminal(task_id, Some(Self::durable_write_timeout()))
            .await
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_write_durable_with_timeout_for_test(
        &self,
        task_id: &str,
        timeout: Duration,
    ) -> Result<()> {
        self.await_task_terminal(task_id, Some(timeout)).await
    }

    #[cfg(test)]
    pub(crate) async fn add_documents_durable_with_timeout_for_test(
        &self,
        tenant_id: &str,
        docs: Vec<Document>,
        timeout: Duration,
    ) -> Result<TaskInfo> {
        let task = self.admit_documents_durable(tenant_id, docs)?;
        self.wait_for_write_durable_with_timeout_for_test(&task.id, timeout)
            .await?;
        Ok(task)
    }

    #[cfg(test)]
    pub(crate) async fn add_documents_insert_durable_with_timeout_for_test(
        &self,
        tenant_id: &str,
        docs: Vec<Document>,
        timeout: Duration,
    ) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;
        let actions = docs.into_iter().map(WriteAction::Add).collect();
        let task =
            self.admit_write_actions(tenant_id, &index, actions, WriteAdmissionMode::Durable)?;
        self.wait_for_write_durable_with_timeout_for_test(&task.id, timeout)
            .await?;
        Ok(task)
    }

    #[cfg(test)]
    pub(crate) fn insert_task_for_test(&self, task: TaskInfo) {
        self.tasks.insert(task.id.clone(), task.clone());
        self.tasks.insert(task.numeric_id.to_string(), task);
    }

    /// Persist the write-admission record and enqueue the documents without
    /// waiting for the worker's terminal acknowledgement.
    pub(super) fn admit_documents_durable(
        &self,
        tenant_id: &str,
        docs: Vec<Document>,
    ) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;
        let actions = docs.into_iter().map(WriteAction::Upsert).collect();
        self.admit_write_actions(tenant_id, &index, actions, WriteAdmissionMode::Durable)
    }

    #[cfg(test)]
    pub(crate) fn admit_replicated_documents_durable_for_test(
        &self,
        tenant_id: &str,
        docs: Vec<(Document, ReplicatedWriteOrigin)>,
    ) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;
        let actions = docs
            .into_iter()
            .map(|(doc, origin)| WriteAction::UpsertWithOrigin { doc, origin })
            .collect();
        self.admit_write_actions(tenant_id, &index, actions, WriteAdmissionMode::Durable)
    }

    /// Add documents and wait until the write queue has durably committed them to
    /// Tantivy, bounded by `FLAPJACK_WRITE_DURABLE_TIMEOUT_MS` (default 30s).
    ///
    /// HTTP add handlers use this instead of fire-and-forget [`add_documents`] so a
    /// 200 response means the write is on disk — closing the PL-13 silent-drop where
    /// an enqueued-but-uncommitted write was ACKed before the consumer committed it.
    /// Returns [`FlapjackError::QueueFull`] (429) on backpressure,
    /// [`FlapjackError::WriteAckTimeout`] (503) while the consumer is active or after
    /// safe compensation, or the underlying commit error (5xx). Replication uses its
    /// origin-aware terminal-wait helpers so a peer is acknowledged only after finalization.
    pub async fn add_documents_durable(
        &self,
        tenant_id: &str,
        docs: Vec<Document>,
    ) -> Result<TaskInfo> {
        let task = self.admit_documents_durable(tenant_id, docs)?;
        self.wait_for_write_durable(&task.id).await?;
        Ok(task)
    }

    /// Insert documents without upsert delete terms and wait for durable commit.
    ///
    /// Staged bulk builds already require unique object IDs, so they need durable
    /// acknowledgement without paying the delete-term work of online upserts.
    pub async fn add_documents_insert_durable(
        &self,
        tenant_id: &str,
        docs: Vec<Document>,
    ) -> Result<TaskInfo> {
        let index = self.get_or_load(tenant_id)?;
        let actions = docs.into_iter().map(WriteAction::Add).collect();
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

    pub async fn add_documents_sync_for_replication_with_origins(
        &self,
        tenant_id: &str,
        docs: Vec<(Document, ReplicatedWriteOrigin)>,
    ) -> Result<()> {
        let task = self.add_documents_for_replication_with_origins(tenant_id, docs)?;
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

    /// Await a legacy replicated delete which carries no origin tuple.
    pub async fn delete_documents_sync_for_replication(
        &self,
        tenant_id: &str,
        object_ids: Vec<String>,
    ) -> Result<()> {
        let task = self.delete_documents_for_replication(tenant_id, object_ids)?;
        self.await_task_terminal(&task.id, None).await
    }

    pub async fn delete_documents_sync_for_replication_with_origins(
        &self,
        tenant_id: &str,
        deletes: Vec<(String, ReplicatedWriteOrigin)>,
    ) -> Result<()> {
        let task = self.delete_documents_for_replication_with_origins(tenant_id, deletes)?;
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
