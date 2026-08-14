// These guards intentionally serialize process-environment overrides across
// complete async specimens; releasing them at awaits would make the tests race.
#![allow(clippy::await_holding_lock)]

use super::*;
use crate::error::FlapjackError;
use crate::index::memory::{MemoryBudget, MemoryBudgetConfig};
use once_cell::sync::Lazy;
use prometheus::{Encoder, TextEncoder};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

const WRITE_QUEUE_BATCH_SIZE_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_BATCH_SIZE";
const WRITE_QUEUE_WRITER_IDLE_TIMEOUT_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_WRITER_IDLE_TIMEOUT_MS";
const JULY_22_TIMEOUT_RISK_PENDING_ADMISSIONS: usize = 690;
/// Measured settled ceiling for the *online* write-path specimens (128/256
/// tiny per-write segments). `SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND` spans
/// both input regimes, so its upper bound (the staged-bulk shape) is far above
/// what the online path may settle to; asserting only the band would let an
/// online merge regression more than double the settled segment count
/// unnoticed. The online guards therefore assert this measured sub-range too,
/// and the bulk guard asserts it must be exceeded — one band, two regimes,
/// each pinned to what it actually measures.
const ONLINE_SPECIMEN_SETTLED_MAX: usize = 4;
static WRITE_QUEUE_ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[test]
fn dropped_worker_completion_reporter_records_abnormal_termination() {
    let completion = WriteQueueWorkerCompletion::new();
    drop(completion.reporter("panic_completion_tenant".to_string()));

    let result = completion
        .wait_timeout(Duration::ZERO)
        .expect("abnormal worker termination must wake blocking completion waiters");
    assert_eq!(
        result.unwrap_err().to_string(),
        "Tantivy error: write queue worker for panic_completion_tenant stopped before reporting completion: worker thread unwound"
    );
}

type WriteQueueHandle = tokio::task::JoinHandle<crate::error::Result<()>>;
type WriteQueueTasks = Arc<dashmap::DashMap<String, TaskInfo>>;
type WriteQueueSetup = (WriteQueue, WriteQueueHandle, WriteQueueTasks);
type GatedWriteQueueSetup = (
    WriteQueue,
    WriteQueueHandle,
    WriteQueueTasks,
    Arc<WriteQueueWorkerGate>,
);
type OplogWriteQueueSetup = (
    WriteQueue,
    WriteQueueHandle,
    WriteQueueTasks,
    Arc<crate::index::oplog::OpLog>,
);
type OplogWriteQueueSetupWithIndex = (
    Arc<crate::index::Index>,
    WriteQueue,
    WriteQueueHandle,
    WriteQueueTasks,
    Arc<crate::index::oplog::OpLog>,
);
type BudgetedWriteQueueSetup = (
    Arc<crate::index::Index>,
    WriteQueue,
    WriteQueueHandle,
    WriteQueueTasks,
);

struct WriteQueueEnvVarRestoreGuard {
    name: &'static str,
    previous_value: Option<String>,
}

impl WriteQueueEnvVarRestoreGuard {
    fn apply(name: &'static str, env_value: Option<&str>) -> Self {
        let previous_value = std::env::var(name).ok();
        match env_value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
        Self {
            name,
            previous_value,
        }
    }
}

impl Drop for WriteQueueEnvVarRestoreGuard {
    fn drop(&mut self) {
        match &self.previous_value {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

fn apply_write_queue_env_overrides(
    overrides: &[(&'static str, Option<&str>)],
) -> Vec<WriteQueueEnvVarRestoreGuard> {
    overrides
        .iter()
        .map(|(name, value)| WriteQueueEnvVarRestoreGuard::apply(name, *value))
        .collect()
}

fn with_write_queue_batch_size_env<T>(env_value: Option<&str>, test_body: impl FnOnce() -> T) -> T {
    let _guard = WRITE_QUEUE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    with_write_queue_batch_size_env_locked(env_value, test_body)
}

fn with_write_queue_channel_capacity_env<T>(
    env_value: Option<&str>,
    test_body: impl FnOnce() -> T,
) -> T {
    let _guard = WRITE_QUEUE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _restore_guard =
        WriteQueueEnvVarRestoreGuard::apply(WRITE_QUEUE_CHANNEL_CAPACITY_ENV_VAR, env_value);
    test_body()
}

async fn add_documents_and_wait_for_test(
    manager: &crate::index::manager::IndexManager,
    tenant_id: &str,
    docs: Vec<Document>,
) -> crate::error::Result<TaskInfo> {
    manager
        .add_documents_durable_with_timeout_for_test(tenant_id, docs, WRITE_QUEUE_PROGRESS_TIMEOUT)
        .await
}

/// Applies a temporary batch-size env value while the caller holds WRITE_QUEUE_ENV_LOCK.
fn with_write_queue_batch_size_env_locked<T>(
    env_value: Option<&str>,
    test_body: impl FnOnce() -> T,
) -> T {
    let _restore_guard =
        WriteQueueEnvVarRestoreGuard::apply(WRITE_QUEUE_BATCH_SIZE_ENV_VAR, env_value);
    test_body()
}

#[test]
fn test_batch_flush_decision_uses_resolved_batch_size_snapshot() {
    with_write_queue_batch_size_env(Some("3"), || {
        let resolved_batch_size = write_queue_batch_size();
        let _restore_guard =
            WriteQueueEnvVarRestoreGuard::apply(WRITE_QUEUE_BATCH_SIZE_ENV_VAR, Some("1"));

        assert!(
            !should_flush_pending_batch(2, resolved_batch_size),
            "pending len should use queue-start batch-size snapshot"
        );
        assert!(
            should_flush_pending_batch(3, resolved_batch_size),
            "pending len at snapshot threshold should flush"
        );
    });
}

#[test]
fn test_with_write_queue_batch_size_env_restores_env_after_panic() {
    let _guard = WRITE_QUEUE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _baseline_guard =
        WriteQueueEnvVarRestoreGuard::apply(WRITE_QUEUE_BATCH_SIZE_ENV_VAR, Some("before-panic"));

    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_write_queue_batch_size_env_locked(Some("during-panic"), || {
            panic!("intentional panic to verify restoration guard");
        });
    }));
    assert!(panic_result.is_err(), "test setup should panic");

    let current_value = std::env::var(WRITE_QUEUE_BATCH_SIZE_ENV_VAR).ok();
    assert_eq!(
        current_value.as_deref(),
        Some("before-panic"),
        "helper must restore env even when closure panics"
    );
}

#[test]
fn write_queue_env_overrides_restore_batch_and_merge_policy_after_panic() {
    let _lock = WRITE_QUEUE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous_values = [
        (
            WRITE_QUEUE_BATCH_SIZE_ENV_VAR,
            std::env::var(WRITE_QUEUE_BATCH_SIZE_ENV_VAR).ok(),
        ),
        (
            WRITE_QUEUE_MIN_MERGE_SEGMENTS_ENV_VAR,
            std::env::var(WRITE_QUEUE_MIN_MERGE_SEGMENTS_ENV_VAR).ok(),
        ),
        (
            WRITE_QUEUE_MAX_DOCS_BEFORE_MERGE_ENV_VAR,
            std::env::var(WRITE_QUEUE_MAX_DOCS_BEFORE_MERGE_ENV_VAR).ok(),
        ),
    ];

    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _override_guards = apply_write_queue_env_overrides(&[
            (WRITE_QUEUE_BATCH_SIZE_ENV_VAR, Some("1")),
            (WRITE_QUEUE_MIN_MERGE_SEGMENTS_ENV_VAR, Some("2")),
            (WRITE_QUEUE_MAX_DOCS_BEFORE_MERGE_ENV_VAR, Some("1000")),
        ]);
        panic!("intentional panic to verify multi-env restoration guard");
    }));
    assert!(panic_result.is_err(), "test setup should panic");

    for (name, previous_value) in previous_values {
        assert_eq!(
            std::env::var(name).ok(),
            previous_value,
            "{name} should be restored after panic"
        );
    }
}

/// Core helper: create a write queue wired to the given index.
fn setup_write_queue_with_index(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    index: Arc<crate::index::Index>,
) -> WriteQueueSetup {
    setup_write_queue_with_index_and_overrides(
        tmp,
        tenant_id,
        index,
        WriteQueueTestOverrides::default(),
    )
}

fn setup_gated_write_queue_with_index(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    index: Arc<crate::index::Index>,
) -> GatedWriteQueueSetup {
    let worker_gate = Arc::new(WriteQueueWorkerGate::closed());
    let (tx, handle, tasks) = setup_write_queue_with_index_and_overrides(
        tmp,
        tenant_id,
        index,
        WriteQueueTestOverrides {
            worker_start_gate: Some(Arc::clone(&worker_gate)),
            ..Default::default()
        },
    );
    (tx, handle, tasks, worker_gate)
}

fn setup_gated_write_queue(tmp: &tempfile::TempDir, tenant_id: &str) -> GatedWriteQueueSetup {
    let tenant_path = tmp.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    setup_gated_write_queue_with_index(tmp, tenant_id, index)
}

fn setup_write_queue_with_index_and_overrides(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    index: Arc<crate::index::Index>,
    test_overrides: WriteQueueTestOverrides,
) -> WriteQueueSetup {
    let tasks: Arc<dashmap::DashMap<String, TaskInfo>> = Arc::new(dashmap::DashMap::new());
    let facet_cache = Arc::new(dashmap::DashMap::new());

    #[cfg(feature = "vector-search")]
    let vector_ctx = VectorWriteContext::new(Arc::new(dashmap::DashMap::new()));
    #[cfg(not(feature = "vector-search"))]
    let vector_ctx = VectorWriteContext::new();
    let admission_store =
        Arc::new(admission::WriteAdmissionStore::open(tmp.path(), tenant_id).unwrap());

    let (tx, handle, _cancellation, _completion) = create_write_queue(WriteQueueContext {
        tenant_id: tenant_id.to_string(),
        index,
        tasks: Arc::clone(&tasks),
        base_path: tmp.path().to_path_buf(),
        oplog: None,
        admission_store,
        facet_cache,
        vector_ctx,
        queue_metrics_id: 0,
        writer_buffer_size: crate::index::Index::DEFAULT_BUFFER_SIZE,
        test_overrides,
    })
    .unwrap();

    (tx, handle, tasks)
}

/// Convenience helper: create an index in a tenant subdirectory and wire up a queue.
fn setup_write_queue(tmp: &tempfile::TempDir, tenant_id: &str) -> WriteQueueSetup {
    let tenant_path = tmp.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    setup_write_queue_with_index(tmp, tenant_id, index)
}

fn setup_write_queue_with_oplog_and_overrides(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    index: Arc<crate::index::Index>,
    test_overrides: WriteQueueTestOverrides,
) -> OplogWriteQueueSetup {
    let tenant_path = tmp.path().join(tenant_id);
    let tasks: Arc<dashmap::DashMap<String, TaskInfo>> = Arc::new(dashmap::DashMap::new());
    let facet_cache = Arc::new(dashmap::DashMap::new());
    #[cfg(feature = "vector-search")]
    let vector_ctx = VectorWriteContext::new(Arc::new(dashmap::DashMap::new()));
    #[cfg(not(feature = "vector-search"))]
    let vector_ctx = VectorWriteContext::new();
    let admission_store =
        Arc::new(admission::WriteAdmissionStore::open(tmp.path(), tenant_id).unwrap());
    let oplog = Arc::new(
        crate::index::oplog::OpLog::open(&tenant_path.join("oplog"), tenant_id, "test_node")
            .unwrap(),
    );

    let (tx, handle, _cancellation, _completion) = create_write_queue(WriteQueueContext {
        tenant_id: tenant_id.to_string(),
        index,
        tasks: Arc::clone(&tasks),
        base_path: tmp.path().to_path_buf(),
        oplog: Some(Arc::clone(&oplog)),
        admission_store,
        facet_cache,
        vector_ctx,
        queue_metrics_id: 0,
        writer_buffer_size: crate::index::Index::DEFAULT_BUFFER_SIZE,
        test_overrides,
    })
    .unwrap();

    (tx, handle, tasks, oplog)
}

fn setup_write_queue_with_oplog(tmp: &tempfile::TempDir, tenant_id: &str) -> OplogWriteQueueSetup {
    let tenant_path = tmp.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    setup_write_queue_with_oplog_and_overrides(
        tmp,
        tenant_id,
        index,
        WriteQueueTestOverrides::default(),
    )
}

fn setup_gated_write_queue_with_oplog_and_overrides(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    index: Arc<crate::index::Index>,
    test_overrides: WriteQueueTestOverrides,
) -> (OplogWriteQueueSetupWithIndex, Arc<WriteQueueWorkerGate>) {
    let worker_gate = Arc::new(WriteQueueWorkerGate::closed());
    let (tx, handle, tasks, oplog) = setup_write_queue_with_oplog_and_overrides(
        tmp,
        tenant_id,
        Arc::clone(&index),
        WriteQueueTestOverrides {
            worker_start_gate: Some(Arc::clone(&worker_gate)),
            ..test_overrides
        },
    );
    ((index, tx, handle, tasks, oplog), worker_gate)
}

fn setup_write_queue_with_budget(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    budget: Arc<MemoryBudget>,
) -> BudgetedWriteQueueSetup {
    setup_write_queue_with_budget_and_overrides(
        tmp,
        tenant_id,
        budget,
        WriteQueueTestOverrides::default(),
    )
}

fn setup_write_queue_with_budget_and_overrides(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    budget: Arc<MemoryBudget>,
    test_overrides: WriteQueueTestOverrides,
) -> BudgetedWriteQueueSetup {
    let tenant_path = tmp.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index =
        Arc::new(crate::index::Index::create_with_budget(&tenant_path, schema, budget).unwrap());
    let (tx, handle, tasks) = setup_write_queue_with_index_and_overrides(
        tmp,
        tenant_id,
        Arc::clone(&index),
        test_overrides,
    );
    (index, tx, handle, tasks)
}

fn text_document(id: &str, field: &str, value: &str) -> crate::types::Document {
    crate::types::Document {
        id: id.to_string(),
        fields: HashMap::from([(
            field.to_string(),
            crate::types::FieldValue::Text(value.to_string()),
        )]),
    }
}

fn dur1_replicated_documents(
    object_ids: &[&str],
) -> Vec<(crate::types::Document, ReplicatedWriteOrigin)> {
    object_ids
        .iter()
        .enumerate()
        .map(|(index, object_id)| {
            (
                text_document(
                    object_id,
                    "title",
                    &format!("DUR-1 replicated document {index}"),
                ),
                ReplicatedWriteOrigin::new(
                    10_000 + index as u64,
                    format!("dur1-replica-node-{index}"),
                ),
            )
        })
        .collect()
}

fn assert_dur1_admission_records_drained(temp_dir: &tempfile::TempDir, tenant_id: &str) {
    assert!(
        crate::index::write_queue::admission::WriteAdmissionStore::open(temp_dir.path(), tenant_id)
            .unwrap()
            .load_records()
            .unwrap()
            .is_empty(),
        "durable write admission records must be drained after restart reconciliation"
    );
}

#[derive(Debug, PartialEq, Eq)]
struct CommittedIndexSnapshot {
    meta_json: String,
    segment_ids: Vec<String>,
}

fn committed_index_snapshot(tenant_path: &Path) -> CommittedIndexSnapshot {
    let meta_json = std::fs::read_to_string(tenant_path.join("meta.json")).unwrap();
    let parsed_meta: serde_json::Value = serde_json::from_str(&meta_json).unwrap();
    let mut segment_ids: Vec<String> = parsed_meta
        .get("segments")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("meta.json must contain segments array: {meta_json}"))
        .iter()
        .map(|segment| {
            segment
                .get("segment_id")
                .or_else(|| segment.get("id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("segment entry must expose an id: {segment:?}"))
                .to_string()
        })
        .collect();
    segment_ids.sort();
    CommittedIndexSnapshot {
        meta_json,
        segment_ids,
    }
}

fn assert_dur1_visible_documents(
    manager: &crate::index::manager::IndexManager,
    tenant_id: &str,
    expected_ids: &[&str],
    absent_ids: &[&str],
    context: &str,
) {
    let observed_ids: Vec<&str> = expected_ids
        .iter()
        .chain(absent_ids.iter())
        .copied()
        .filter(|object_id| {
            manager
                .get_document(tenant_id, object_id)
                .unwrap()
                .is_some()
        })
        .collect();
    assert_eq!(
        observed_ids, expected_ids,
        "{context} visible objectID set must match expected ids"
    );
    assert_eq!(
        manager.tenant_doc_count(tenant_id),
        Some(expected_ids.len() as u64),
        "{context} must expose exactly the expected searchable document count"
    );
}

fn oplog_task_id(entry: &crate::index::oplog::OpLogEntry) -> String {
    entry
        .payload
        .get("_flapjack_task_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("oplog entry must include task id: {entry:?}"))
        .to_string()
}

fn oplog_object_id(entry: &crate::index::oplog::OpLogEntry) -> String {
    entry
        .payload
        .get("objectID")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("oplog entry must include objectID: {entry:?}"))
        .to_string()
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(write_queue_commit_failure_hook)]
async fn dur1_failed_durable_write_stays_absent_after_restart() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let tenant_id = "dur1_failed_durable_write_restart";
    let baseline_id = "dur1_baseline";
    let rejected_ids = ["dur1_rejected_a", "dur1_rejected_b"];
    let manager =
        crate::index::manager::IndexManager::new_with_node_id(temp_dir.path(), "local-node");
    manager.create_tenant(tenant_id).unwrap();
    manager
        .add_documents_sync(
            tenant_id,
            vec![text_document(baseline_id, "title", "DUR-1 baseline")],
        )
        .await
        .unwrap();
    let tenant_path = temp_dir.path().join(tenant_id);
    let before_failure_snapshot = committed_index_snapshot(&tenant_path);

    let _fault = crate::index::write_queue::fail_next_finalization_for_test(
        tenant_id,
        FinalizationFaultPoint::BeforeTantivyCommit,
    );
    let task = manager
        .admit_replicated_documents_durable_for_test(
            tenant_id,
            dur1_replicated_documents(&rejected_ids),
        )
        .unwrap();

    assert!(
        manager.wait_for_write_durable(&task.id).await.is_err(),
        "injected durable write failure must reach the waiting caller"
    );
    let canonical_task = manager.get_task(&task.id).unwrap();
    assert_eq!(
        canonical_task.id, task.id,
        "canonical task lookup must resolve to the failed durable write"
    );
    let numeric_task = manager.get_task(&task.numeric_id.to_string()).unwrap();
    assert_eq!(
        numeric_task.id, task.id,
        "numeric task alias must resolve to the failed durable write"
    );
    assert_eq!(
        numeric_task.numeric_id, task.numeric_id,
        "numeric task alias must preserve the failed durable write numeric id"
    );
    assert!(
        matches!(canonical_task.status, crate::types::TaskStatus::Failed(_)),
        "failed durable write canonical task lookup must be terminal Failed"
    );
    assert!(
        matches!(numeric_task.status, crate::types::TaskStatus::Failed(_)),
        "failed durable write numeric task alias must be terminal Failed"
    );
    let failed_write_handle = manager
        .write_task_handles
        .get(tenant_id)
        .map(|entry| entry.clone())
        .expect("failed durable write must leave a tenant worker handle to drain");
    assert!(
        failed_write_handle
            .drain(tenant_id.to_string())
            .await
            .is_err(),
        "failed durable write worker must terminate with the injected commit error"
    );
    assert_eq!(
        committed_index_snapshot(&tenant_path),
        before_failure_snapshot,
        "BeforeTantivyCommit must not change committed meta.json or committed segment ids"
    );
    assert_dur1_visible_documents(
        &manager,
        tenant_id,
        &[baseline_id],
        &rejected_ids,
        "in-process after injected pre-commit failure",
    );
    manager.unload(&tenant_id.to_string()).unwrap();

    drop(manager);
    let restarted_manager =
        crate::index::manager::IndexManager::new_with_node_id(temp_dir.path(), "local-node");
    restarted_manager.get_or_load(tenant_id).unwrap();
    assert_dur1_visible_documents(
        &restarted_manager,
        tenant_id,
        &[baseline_id],
        &rejected_ids,
        "first restart",
    );
    assert_dur1_admission_records_drained(&temp_dir, tenant_id);

    drop(restarted_manager);
    let second_restart =
        crate::index::manager::IndexManager::new_with_node_id(temp_dir.path(), "local-node");
    second_restart.get_or_load(tenant_id).unwrap();
    assert_dur1_visible_documents(
        &second_restart,
        tenant_id,
        &[baseline_id],
        &rejected_ids,
        "second restart",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dur1_successful_durable_write_survives_restart() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let tenant_id = "dur1_successful_durable_write_restart";
    let baseline_id = "dur1_success_baseline";
    let accepted_ids = ["dur1_accepted_a", "dur1_accepted_b"];
    let manager =
        crate::index::manager::IndexManager::new_with_node_id(temp_dir.path(), "local-node");
    manager.create_tenant(tenant_id).unwrap();
    manager
        .add_documents_sync(
            tenant_id,
            vec![text_document(
                baseline_id,
                "title",
                "DUR-1 negative-control baseline",
            )],
        )
        .await
        .unwrap();

    let task = manager
        .admit_replicated_documents_durable_for_test(
            tenant_id,
            dur1_replicated_documents(&accepted_ids),
        )
        .unwrap();
    manager.wait_for_write_durable(&task.id).await.unwrap();

    drop(manager);
    let restarted_manager =
        crate::index::manager::IndexManager::new_with_node_id(temp_dir.path(), "local-node");
    restarted_manager.get_or_load(tenant_id).unwrap();
    assert_dur1_visible_documents(
        &restarted_manager,
        tenant_id,
        &[baseline_id, accepted_ids[0], accepted_ids[1]],
        &[],
        "successful restart",
    );
    assert_dur1_admission_records_drained(&temp_dir, tenant_id);
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(write_queue_commit_failure_hook)]
async fn compensation_preserves_concurrent_metadata_oplog_append() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let tenant_id = "compensation_preserves_metadata";
    let baseline_id = "metadata_compensation_baseline";
    let rejected_ids = [
        "metadata_compensation_rejected_a",
        "metadata_compensation_rejected_b",
    ];
    let manager = Arc::new(crate::index::manager::IndexManager::new_with_node_id(
        temp_dir.path(),
        "local-node",
    ));
    manager.create_tenant(tenant_id).unwrap();
    manager
        .add_documents_sync(
            tenant_id,
            vec![text_document(
                baseline_id,
                "title",
                "metadata compensation baseline",
            )],
        )
        .await
        .unwrap();

    let hook_manager = Arc::clone(&manager);
    let hook_admission_path = temp_dir.path().to_path_buf();
    let admission_was_durable_before_retraction = Arc::new(AtomicBool::new(false));
    let hook_observation = Arc::clone(&admission_was_durable_before_retraction);
    let _metadata_append =
        crate::index::write_queue::set_compensation_before_oplog_retraction_hook_for_test(
            Arc::new(move |hook_tenant_id| {
                let records = crate::index::write_queue::admission::WriteAdmissionStore::open(
                    &hook_admission_path,
                    hook_tenant_id,
                )
                .unwrap()
                .load_records()
                .unwrap();
                assert!(
                    !records.is_empty(),
                    "admission replay must remain durable until oplog retraction succeeds"
                );
                hook_observation.store(true, Ordering::SeqCst);
                hook_manager.append_oplog(
                    hook_tenant_id,
                    "settings",
                    serde_json::json!({"searchableAttributes": ["title"]}),
                );
            }),
        );
    let _fault = crate::index::write_queue::fail_next_finalization_for_test(
        tenant_id,
        FinalizationFaultPoint::BeforeTantivyCommit,
    );
    let task = manager
        .admit_replicated_documents_durable_for_test(
            tenant_id,
            dur1_replicated_documents(&rejected_ids),
        )
        .unwrap();

    assert!(manager.wait_for_write_durable(&task.id).await.is_err());
    assert!(
        admission_was_durable_before_retraction.load(Ordering::SeqCst),
        "compensation must observe admission before retracting the oplog"
    );
    let handle = manager
        .write_task_handles
        .get(tenant_id)
        .map(|entry| entry.clone())
        .expect("failed durable write must leave a tenant worker handle to drain");
    assert!(handle.drain(tenant_id.to_string()).await.is_err());

    let tenant_path = temp_dir.path().join(tenant_id);
    let committed_seq = crate::index::oplog::read_committed_seq(&tenant_path);
    let oplog = manager.get_oplog(tenant_id).unwrap();
    let entries = oplog.read_since(0).unwrap();
    assert!(
        entries.iter().any(|entry| entry.op_type == "settings"),
        "compensation must not delete unrelated synchronous metadata rows: {entries:?}"
    );
    assert!(
        oplog.current_seq() >= committed_seq,
        "compensation must not rewind the oplog tail below committed_seq; current_seq={}, committed_seq={committed_seq}",
        oplog.current_seq()
    );
}

async fn setup_compensation_manager(
    base_path: &std::path::Path,
    tenant_id: &str,
    baseline_id: &str,
) -> Arc<crate::index::manager::IndexManager> {
    let manager = crate::index::manager::IndexManager::new_with_node_id(base_path, "local-node");
    manager.create_tenant(tenant_id).unwrap();
    manager
        .add_documents_sync(
            tenant_id,
            vec![text_document(baseline_id, "title", "compensation baseline")],
        )
        .await
        .unwrap();
    manager
}

async fn assert_successful_compensation_stays_absent() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let tenant_id = "compensation_success_absent";
    let baseline_id = "compensation_baseline";
    let rejected_ids = ["compensation_rejected_a", "compensation_rejected_b"];
    let manager = setup_compensation_manager(temp_dir.path(), tenant_id, baseline_id).await;

    let _fault = crate::index::write_queue::fail_next_finalization_for_test(
        tenant_id,
        FinalizationFaultPoint::BeforeTantivyCommit,
    );
    let task = manager
        .admit_replicated_documents_durable_for_test(
            tenant_id,
            dur1_replicated_documents(&rejected_ids),
        )
        .unwrap();
    assert!(manager.wait_for_write_durable(&task.id).await.is_err());
    assert!(
        matches!(
            manager.get_task(&task.id).map(|task| task.status),
            Ok(crate::types::TaskStatus::Failed(_))
        ),
        "successful compensation must still certify the task terminal Failed"
    );
    let handle = manager
        .write_task_handles
        .get(tenant_id)
        .map(|entry| entry.clone())
        .expect("failed durable write must leave a tenant worker handle to drain");
    assert!(handle.drain(tenant_id.to_string()).await.is_err());
    manager.unload(&tenant_id.to_string()).unwrap();
    drop(manager);

    for context in ["first restart", "second restart"] {
        let restarted =
            crate::index::manager::IndexManager::new_with_node_id(temp_dir.path(), "local-node");
        restarted.get_or_load(tenant_id).unwrap();
        assert_dur1_visible_documents(
            &restarted,
            tenant_id,
            &[baseline_id],
            &rejected_ids,
            context,
        );
        assert_dur1_admission_records_drained(&temp_dir, tenant_id);
        drop(restarted);
    }
}

fn assert_task_replay_routes_absent(
    manager: &crate::index::manager::IndexManager,
    base_path: &std::path::Path,
    tenant_id: &str,
    task_id: &str,
) {
    let (admission_records, oplog_task_ids) =
        task_replay_routes(manager, base_path, tenant_id, task_id);
    assert!(
        admission_records.is_empty(),
        "a public timeout must not leave admission replay records"
    );
    assert!(
        !oplog_task_ids.contains(task_id),
        "a public timeout must not leave task-tagged oplog rows"
    );
}

fn task_replay_routes(
    manager: &crate::index::manager::IndexManager,
    base_path: &std::path::Path,
    tenant_id: &str,
    _task_id: &str,
) -> (
    Vec<crate::index::write_queue::admission::WriteAdmissionRecord>,
    BTreeSet<String>,
) {
    let admission_records =
        crate::index::write_queue::admission::WriteAdmissionStore::open(base_path, tenant_id)
            .unwrap()
            .load_records()
            .unwrap();
    let oplog_task_ids: BTreeSet<String> = manager
        .get_oplog(tenant_id)
        .unwrap()
        .read_since(0)
        .unwrap()
        .iter()
        .filter_map(|entry| {
            entry
                .payload
                .get("_flapjack_task_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    (admission_records, oplog_task_ids)
}

async fn wait_for_task_replay_routes_absent(
    manager: &crate::index::manager::IndexManager,
    base_path: &std::path::Path,
    tenant_id: &str,
    task_id: &str,
) {
    tokio::time::timeout(WRITE_QUEUE_PROGRESS_TIMEOUT, async {
        loop {
            let (admission_records, oplog_task_ids) =
                task_replay_routes(manager, base_path, tenant_id, task_id);
            if admission_records.is_empty() && !oplog_task_ids.contains(task_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("stopped write compensation should remove replay routes before timeout");
}

async fn assert_public_timeout_retries_compensation() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let tenant_id = "compensation_failure_fail_closed";
    let baseline_id = "compensation_fc_baseline";
    let rejected_ids = ["compensation_fc_rejected_a", "compensation_fc_rejected_b"];
    let manager = setup_compensation_manager(temp_dir.path(), tenant_id, baseline_id).await;

    let _commit_fault = crate::index::write_queue::fail_next_finalization_for_test(
        tenant_id,
        FinalizationFaultPoint::BeforeTantivyCommit,
    );
    let _compensation_fault = crate::index::write_queue::fail_next_compensation_for_test(tenant_id);
    let task = manager
        .admit_replicated_documents_durable_for_test(
            tenant_id,
            dur1_replicated_documents(&rejected_ids),
        )
        .unwrap();
    let handle = manager
        .write_task_handles
        .get(tenant_id)
        .map(|entry| entry.clone())
        .expect("fail-closed durable write must leave a tenant worker handle to drain");
    assert!(
        handle.drain(tenant_id.to_string()).await.is_err(),
        "test precondition: the injected compensation failure must stop the worker before the public timeout"
    );

    let durable_result = manager
        .wait_for_write_durable_with_timeout_for_test(&task.id, Duration::from_millis(25))
        .await;
    assert!(
        matches!(durable_result, Err(FlapjackError::WriteAckTimeout)),
        "the bounded public waiter must surface its client error path, got {durable_result:?}"
    );
    let post_stop_result = manager
        .wait_for_write_durable_with_timeout_for_test(&task.id, WRITE_QUEUE_PROGRESS_TIMEOUT)
        .await;
    assert!(
        matches!(post_stop_result, Err(FlapjackError::WriteAckTimeout)),
        "a stopped task whose replay routes are cleaned up must remain a retryable public timeout, got {post_stop_result:?}"
    );

    assert!(
        !matches!(
            manager.get_task(&task.id).map(|task| task.status),
            Ok(crate::types::TaskStatus::Failed(_))
        ),
        "a batch whose retraction failed must NOT be certified terminal Failed"
    );

    wait_for_task_replay_routes_absent(&manager, temp_dir.path(), tenant_id, &task.id).await;
    assert_task_replay_routes_absent(&manager, temp_dir.path(), tenant_id, &task.id);

    manager.unload(&tenant_id.to_string()).unwrap();
    drop(manager);
    for context in [
        "first public-waiter restart",
        "second public-waiter restart",
    ] {
        let restarted =
            crate::index::manager::IndexManager::new_with_node_id(temp_dir.path(), "local-node");
        restarted.get_or_load(tenant_id).unwrap();
        assert_dur1_visible_documents(
            &restarted,
            tenant_id,
            &[baseline_id],
            &rejected_ids,
            context,
        );
        assert_dur1_admission_records_drained(&temp_dir, tenant_id);
        drop(restarted);
    }
}

async fn assert_persistent_failure_uses_durable_ack() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let tenant_id = "compensation_persistent_failure";
    let baseline_id = "compensation_persistent_baseline";
    let accepted_ids = ["compensation_persistent_a", "compensation_persistent_b"];
    let manager = setup_compensation_manager(temp_dir.path(), tenant_id, baseline_id).await;

    let _commit_fault = crate::index::write_queue::fail_next_finalization_for_test(
        tenant_id,
        FinalizationFaultPoint::BeforeTantivyCommit,
    );
    let _compensation_fault =
        crate::index::write_queue::fail_compensation_attempts_for_test(tenant_id, 2);
    let task = manager
        .admit_replicated_documents_durable_for_test(
            tenant_id,
            dur1_replicated_documents(&accepted_ids),
        )
        .unwrap();

    wait_for_persistent_compensation_durable_ack(&manager, &task.id).await;
    assert_eq!(
        crate::index::write_queue::compensation_fault_attempts_remaining_for_test(tenant_id),
        0,
        "the worker and bounded waiter must each reach the compensation seam"
    );
    assert!(
        !matches!(
            manager.get_task(&task.id).map(|task| task.status),
            Ok(crate::types::TaskStatus::Failed(_))
        ),
        "durably acknowledged replay must not expose a terminal failure"
    );
    manager.unload(&tenant_id.to_string()).unwrap();
    drop(manager);

    let restarted =
        crate::index::manager::IndexManager::new_with_node_id(temp_dir.path(), "local-node");
    restarted.get_or_load(tenant_id).unwrap();
    assert_dur1_visible_documents(
        &restarted,
        tenant_id,
        &[baseline_id, accepted_ids[0], accepted_ids[1]],
        &[],
        "durable acknowledgement recovery",
    );
    assert_dur1_admission_records_drained(&temp_dir, tenant_id);
}

async fn wait_for_persistent_compensation_durable_ack(
    manager: &crate::index::manager::IndexManager,
    task_id: &str,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match manager
            .wait_for_write_durable_with_timeout_for_test(task_id, Duration::from_millis(25))
            .await
        {
            Ok(()) => return,
            Err(FlapjackError::WriteAckTimeout) if std::time::Instant::now() < deadline => {
                tokio::task::yield_now().await;
            }
            result => panic!(
                "a persistent compensation failure must become a durable acknowledgement before the bounded deadline, got {result:?}"
            ),
        }
    }
}

/// The durable-wait window is 25ms; progress is published every 20ms so each
/// publication lands inside a fresh idle window but the final `Succeeded` lands
/// at virtual ~60ms — well past the original 25ms window. A total-elapsed fence
/// (the defect this stage removed) would time the task out at 30ms and redden.
const PROGRESS_CONTRACT_WINDOW: Duration = Duration::from_millis(25);
const PROGRESS_CONTRACT_STEP: Duration = Duration::from_millis(20);

// `start_paused` runs the whole test on tokio's virtual clock: the fence's idle
// deadline, its 10ms poll sleeps, and the `advance()` calls below all read one
// clock the test controls, so ordering is fixed by the durations here and never
// by scheduler jitter under `cargo test --workspace` contention.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn durable_wait_deadline_is_reset_by_observable_task_progress() {
    let tmp = tempfile::TempDir::new().unwrap();
    let manager = crate::index::manager::IndexManager::new(tmp.path());

    let stalled_task_id = "task_progress_contract_stalled_1".to_string();
    manager.insert_task_for_test(TaskInfo::new(stalled_task_id.clone(), 1, 3));
    let stalled = manager
        .wait_for_write_durable_with_timeout_for_test(&stalled_task_id, PROGRESS_CONTRACT_WINDOW)
        .await;
    assert!(
        matches!(stalled, Err(FlapjackError::WriteAckTimeout)),
        "a non-terminal task with no observed progress inside the durable window must fail closed, got {stalled:?}"
    );

    let progressing_task_id = "task_progress_contract_progressing_2".to_string();
    let mut progressing_task = TaskInfo::new(progressing_task_id.clone(), 2, 3);
    progressing_task.status = TaskStatus::Processing;
    manager.insert_task_for_test(progressing_task);

    let waiter = {
        let manager = Arc::clone(&manager);
        let task_id = progressing_task_id.clone();
        tokio::spawn(async move {
            manager
                .wait_for_write_durable_with_timeout_for_test(&task_id, PROGRESS_CONTRACT_WINDOW)
                .await
        })
    };

    // Let the waiter record the task's initial (zero-progress) state and arm its
    // first idle sleep before any progress is published.
    tokio::time::sleep(Duration::from_millis(1)).await;

    // Test-owned seam: publish each progress step first, then advance the virtual
    // clock by less than one window so the waiter observes the update and resets
    // its deadline. No real sleeps, so the waiter can only advance after the
    // update it depends on has already been published.
    for indexed_documents in 1..=3 {
        let mut task = manager.get_task(&progressing_task_id).unwrap();
        task.indexed_documents = indexed_documents;
        if indexed_documents == 3 {
            task.status = TaskStatus::Succeeded;
        }
        manager.insert_task_for_test(task);
        tokio::time::advance(PROGRESS_CONTRACT_STEP).await;
    }

    let progressed = waiter.await.unwrap();
    assert!(
        matches!(progressed, Ok(())),
        "slow but steady progress beyond the original durable window must remain acknowledged, got {progressed:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn durable_wait_keeps_configured_wall_clock_bound_when_runtime_is_descheduled() {
    use std::future::Future;
    use std::task::Poll;

    const IDLE_BUDGET: Duration = Duration::from_millis(25);
    const RUNTIME_STALL: Duration = Duration::from_millis(75);

    let tmp = tempfile::TempDir::new().unwrap();
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    let task_id = "runtime_starvation_contract".to_string();
    let mut task = TaskInfo::new(task_id.clone(), 1, 1);
    task.status = TaskStatus::Processing;
    manager.insert_task_for_test(task);

    let mut waiter =
        Box::pin(manager.wait_for_write_durable_with_timeout_for_test(&task_id, IDLE_BUDGET));
    let initial_poll = std::future::poll_fn(|cx| Poll::Ready(waiter.as_mut().poll(cx))).await;
    assert!(
        matches!(initial_poll, Poll::Pending),
        "a processing task must arm the durable-wait poll timer"
    );

    // Model the union run's runtime starvation: neither the waiter nor its poll
    // timer can run during this interval, so it cannot observe task idleness.
    std::thread::sleep(RUNTIME_STALL);
    tokio::time::sleep(Duration::from_millis(1)).await;
    let post_stall_poll = std::future::poll_fn(|cx| Poll::Ready(waiter.as_mut().poll(cx))).await;
    assert!(
        matches!(post_stall_poll, Poll::Ready(Err(FlapjackError::WriteAckTimeout))),
        "a stalled runtime must not stretch the configured durable timeout, got {post_stall_poll:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(write_queue_commit_failure_hook)]
/// DUR-1 fail-closed contract for commit-failure compensation.
///
/// Covers successful cleanup, a transient failure retried before public 503,
/// and persistent failure converted to durable acknowledgement for recovery.
async fn compensation_failure_is_fail_closed() {
    assert_successful_compensation_stays_absent().await;
    assert_public_timeout_retries_compensation().await;
    assert_persistent_failure_uses_durable_ack().await;
}

/// DUR-1: a batch that fails after appending part of its oplog prefix but before
/// its Tantivy commit must retract that whole prefix, so a restart cannot
/// resurrect the published-but-uncommitted op. (Before the fail-closed
/// compensation this batch left the first op's oplog row published, and recovery
/// replayed it — the exact durability defect DUR-1 closes.)
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(write_queue_commit_failure_hook)]
async fn oplog_append_boundary_retracts_failed_batch_prefix() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let tenant_id = "oplog_append_boundary_prefix";
    let baseline_id = "oplog_prefix_baseline";
    let first_id = "oplog_prefix_first";
    let second_id = "oplog_prefix_second";
    let baseline_manager =
        crate::index::manager::IndexManager::new_with_node_id(temp_dir.path(), "local-node");
    baseline_manager.create_tenant(tenant_id).unwrap();
    baseline_manager
        .add_documents_sync(
            tenant_id,
            vec![text_document(baseline_id, "title", "oplog prefix baseline")],
        )
        .await
        .unwrap();
    drop(baseline_manager);

    let tenant_path = temp_dir.path().join(tenant_id);
    let index = Arc::new(crate::index::Index::open(&tenant_path).unwrap());
    let ((index, tx, handle, tasks, oplog), worker_gate) =
        setup_gated_write_queue_with_oplog_and_overrides(
            &temp_dir,
            tenant_id,
            index,
            WriteQueueTestOverrides {
                batch_size: Some(2),
                ..Default::default()
            },
        );
    let pre_batch_oplog_seq = oplog.current_seq();
    let first_task = register_task(tasks.as_ref(), "oplog_prefix_task_1", 2, 1);
    let second_task = register_task(tasks.as_ref(), "oplog_prefix_task_2", 3, 1);
    enqueue_write(
        &tx,
        first_task.clone(),
        vec![WriteAction::Upsert(text_document(
            first_id,
            "title",
            "oplog prefix first",
        ))],
    )
    .await;
    enqueue_write(
        &tx,
        second_task.clone(),
        vec![WriteAction::Upsert(text_document(
            second_id,
            "title",
            "oplog prefix second",
        ))],
    )
    .await;

    let _fault = crate::index::write_queue::fail_next_finalization_for_test(
        tenant_id,
        FinalizationFaultPoint::AfterOplogAppendBeforeTantivyCommit,
    );
    worker_gate.release();
    drop(tx);
    let queue_result = handle.await.unwrap();
    assert!(
        queue_result.is_err(),
        "injected post-oplog failure must terminate the queue worker"
    );
    assert_task_failed(tasks.as_ref(), &first_task);
    assert_task_failed(tasks.as_ref(), &second_task);

    let entries = oplog.read_since(pre_batch_oplog_seq).unwrap();
    let observed_prefix: Vec<(u64, String, String)> = entries
        .iter()
        .map(|entry| (entry.seq, oplog_task_id(entry), oplog_object_id(entry)))
        .collect();
    assert_eq!(
        observed_prefix,
        Vec::new(),
        "compensation must retract the whole failed batch's oplog prefix, including the first published op; pre_batch_seq={pre_batch_oplog_seq}, observed prefix was {observed_prefix:?}"
    );

    drop(index);
    let restarted_manager =
        crate::index::manager::IndexManager::new_with_node_id(temp_dir.path(), "local-node");
    restarted_manager.get_or_load(tenant_id).unwrap();
    assert_dur1_visible_documents(
        &restarted_manager,
        tenant_id,
        &[baseline_id],
        &[first_id, second_id],
        "restart after retracted oplog prefix",
    );
    assert_dur1_admission_records_drained(&temp_dir, tenant_id);
}

struct PartialAppendRestartExpectation<'a> {
    baseline_id: &'a str,
    batch_ids: [&'a str; 2],
    pre_batch_oplog_seq: u64,
    client_saw_failure: bool,
}

fn assert_partial_append_restart_outcome(
    temp_dir: &tempfile::TempDir,
    tenant_id: &str,
    oplog: &crate::index::oplog::OpLog,
    expectation: PartialAppendRestartExpectation<'_>,
) {
    let restarted =
        crate::index::manager::IndexManager::new_with_node_id(temp_dir.path(), "local-node");
    restarted.get_or_load(tenant_id).unwrap();

    // Both arms are contractual. Arm A retracts every replay route before a
    // failed verdict; Arm B gives an honest durable acknowledgement and must
    // recover every acknowledged document. Do not simplify this to one arm.
    if expectation.client_saw_failure {
        assert_dur1_visible_documents(
            &restarted,
            tenant_id,
            &[expectation.baseline_id],
            &expectation.batch_ids,
            "failed partial oplog append after restart",
        );
        let replayable_task_rows: Vec<(String, String)> = oplog
            .read_since(expectation.pre_batch_oplog_seq)
            .unwrap()
            .iter()
            .map(|entry| (oplog_task_id(entry), oplog_object_id(entry)))
            .collect();
        assert_eq!(
            replayable_task_rows,
            Vec::new(),
            "a client-visible failure must leave no replayable task rows"
        );
    } else {
        assert_dur1_visible_documents(
            &restarted,
            tenant_id,
            &[
                expectation.baseline_id,
                expectation.batch_ids[0],
                expectation.batch_ids[1],
            ],
            &[],
            "durably acknowledged partial oplog append after restart",
        );
    }
    assert_dur1_admission_records_drained(temp_dir, tenant_id);
}

/// DUR-1: an I/O failure from inside the oplog append must not let the client
/// observe failure while leaving the task replayable after restart.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(write_queue_commit_failure_hook)]
async fn oplog_append_io_failure_before_acknowledgement_is_fail_closed() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let tenant_id = "oplog_append_io_failure_before_ack";
    let baseline_id = "oplog_append_io_baseline";
    let batch_ids = ["oplog_append_io_first", "oplog_append_io_second"];
    let baseline_manager =
        setup_compensation_manager(temp_dir.path(), tenant_id, baseline_id).await;
    baseline_manager.graceful_shutdown().await;
    drop(baseline_manager);

    let tenant_path = temp_dir.path().join(tenant_id);
    let index = Arc::new(crate::index::Index::open(&tenant_path).unwrap());
    let ((index, tx, handle, tasks, oplog), worker_gate) =
        setup_gated_write_queue_with_oplog_and_overrides(
            &temp_dir,
            tenant_id,
            index,
            WriteQueueTestOverrides {
                batch_size: Some(2),
                ..Default::default()
            },
        );
    let pre_batch_oplog_seq = oplog.current_seq();
    let first_task = register_task(tasks.as_ref(), "oplog_append_io_task_1", 2, 1);
    let second_task = register_task(tasks.as_ref(), "oplog_append_io_task_2", 3, 1);
    enqueue_write(
        &tx,
        first_task.clone(),
        vec![WriteAction::Upsert(text_document(
            batch_ids[0],
            "title",
            "oplog append I/O first",
        ))],
    )
    .await;
    enqueue_write(
        &tx,
        second_task.clone(),
        vec![WriteAction::Upsert(text_document(
            batch_ids[1],
            "title",
            "oplog append I/O second",
        ))],
    )
    .await;

    // Unlike AfterOplogAppendBeforeTantivyCommit, this fault fires after the
    // first task-tagged row is durable but before current_seq advances. That
    // partial append is the EIO/ENOSPC surface whose replay contract matters.
    let fault = crate::index::write_queue::fail_next_finalization_for_test(
        tenant_id,
        FinalizationFaultPoint::DuringOplogAppendAfterPartialDurableWrite,
    );
    worker_gate.release();
    drop(tx);
    let queue_result = handle.await.unwrap();
    assert!(
        fault.was_triggered(),
        "the guard must prove the mid-append fault fired before either contractual arm is accepted; queue_result={queue_result:?}"
    );

    let client_saw_failure = [first_task.as_str(), second_task.as_str()]
        .iter()
        .all(|task_id| {
            tasks
                .get(*task_id)
                .is_some_and(|task| matches!(task.status, TaskStatus::Failed(_)))
        });
    let client_saw_success = [first_task.as_str(), second_task.as_str()]
        .iter()
        .all(|task_id| task_succeeded(tasks.as_ref(), task_id));
    assert!(
        client_saw_failure ^ client_saw_success,
        "the batch must expose one consistent client outcome; queue_result={queue_result:?}"
    );

    if client_saw_failure {
        assert_task_failed(tasks.as_ref(), &first_task);
        assert_task_failed(tasks.as_ref(), &second_task);
    } else {
        assert_task_succeeded(tasks.as_ref(), &first_task, 1);
        assert_task_succeeded(tasks.as_ref(), &second_task, 1);
    }
    drop(index);
    assert_partial_append_restart_outcome(
        &temp_dir,
        tenant_id,
        oplog.as_ref(),
        PartialAppendRestartExpectation {
            baseline_id,
            batch_ids,
            pre_batch_oplog_seq,
            client_saw_failure,
        },
    );
}

include!("write_queue/backpressure_tests.rs");
include!("write_queue/backpressure_artifact_tests.rs");

fn task_succeeded(tasks: &dashmap::DashMap<String, TaskInfo>, task_id: &str) -> bool {
    tasks
        .get(task_id)
        .is_some_and(|task| matches!(task.status, crate::types::TaskStatus::Succeeded))
}

fn assert_task_succeeded(
    tasks: &dashmap::DashMap<String, TaskInfo>,
    task_id: &str,
    indexed_documents: usize,
) {
    let final_task = tasks.get(task_id).unwrap();
    assert!(
        task_succeeded(tasks, task_id),
        "task should succeed, got: {:?}",
        final_task.status
    );
    assert_eq!(final_task.indexed_documents, indexed_documents);
}

fn assert_task_failed(tasks: &dashmap::DashMap<String, TaskInfo>, task_id: &str) {
    let final_task = tasks.get(task_id).unwrap();
    assert!(
        matches!(final_task.status, crate::types::TaskStatus::Failed(_)),
        "task should fail, got: {:?}",
        final_task.status
    );
    assert_eq!(
        final_task.indexed_documents, 0,
        "failed tasks should not report committed documents"
    );
}

fn register_task(
    tasks: &dashmap::DashMap<String, TaskInfo>,
    task_id: &str,
    batch_number: i64,
    indexed_documents: usize,
) -> String {
    let task_id = task_id.to_string();
    tasks.insert(
        task_id.clone(),
        TaskInfo::new(task_id.clone(), batch_number, indexed_documents),
    );
    task_id
}

async fn enqueue_write(tx: &WriteQueue, task_id: String, actions: Vec<WriteAction>) {
    tx.send(WriteOp { task_id, actions }).await.unwrap();
}

fn enqueue_write_without_draining_burst(
    tx: &WriteQueue,
    task_id: String,
    actions: Vec<WriteAction>,
) {
    tx.try_send(WriteOp { task_id, actions }).unwrap();
}

fn indexed_document_count(index: &crate::index::Index) -> usize {
    index
        .reader()
        .searcher()
        .segment_readers()
        .iter()
        .map(|segment| segment.num_docs() as usize)
        .sum()
}

fn searchable_segment_count(index: &crate::index::Index) -> usize {
    index.reader().searcher().segment_readers().len()
}

fn observed_segments(index: &crate::index::Index) -> segment_observation::SegmentObservation {
    index.reader().reload().unwrap();
    segment_observation::observe_segments(index).unwrap()
}

async fn wait_for_write_queue_settle() {
    tokio::time::sleep(Duration::from_millis(200)).await;
}

fn write_queue_phase_metrics_text() -> String {
    let mut encoded = Vec::new();
    TextEncoder::new()
        .encode(&gather_write_queue_phase_metric_families(), &mut encoded)
        .unwrap();
    String::from_utf8(encoded).unwrap()
}

#[derive(Clone, Copy)]
struct MergePolicyCandidate {
    name: &'static str,
    min_merge_segments: Option<usize>,
    max_docs_before_merge: Option<usize>,
    selectable: bool,
}

#[derive(Debug, PartialEq)]
struct MergePolicyQueryOutcome {
    ids: Vec<String>,
    total: usize,
    facet_counts: BTreeMap<String, Vec<(String, u64)>>,
    effective_around_lat_lng: Option<String>,
    effective_around_radius: Option<serde_json::Value>,
}

#[derive(Debug)]
struct MergePolicyExperimentRow {
    name: &'static str,
    selectable: bool,
    settled_index_bytes: u64,
    live_segment_count: usize,
    live_docs: u64,
    peak_orphan_file_sets: usize,
    settled_orphan_file_sets: usize,
    import_wall_ms: u128,
    cold_latencies_us: Vec<u128>,
    warm_latencies_us: Vec<u128>,
    query_outcomes: Vec<MergePolicyQueryOutcome>,
}

#[derive(Clone, Copy)]
struct WriterIdleTimeoutCandidate {
    name: &'static str,
    timeout: Duration,
}

#[derive(Debug)]
struct WriterIdleBurstRow {
    candidate_name: &'static str,
    candidate_timeout: Duration,
    resume_gap: Duration,
    n: usize,
    second_write_wall_ms: u128,
    writer_open_delta: u64,
    commit_delta: u64,
    idle_merge_wait_delta: u64,
}

#[derive(Debug)]
struct WriterIdleAdmissionRow {
    candidate_name: &'static str,
    candidate_timeout: Duration,
    tenant_count: usize,
    admitted_tenants: usize,
    admission_wait_ms: Vec<u128>,
    idle_merge_wait_delta: u64,
    final_active_writers: usize,
}

#[derive(Debug)]
struct WriterIdleExperimentRows {
    trace_ack_ms: Vec<u128>,
    resume_gaps: Vec<Duration>,
    projected_full_runtime_ms: u128,
    burst_rows: Vec<WriterIdleBurstRow>,
    admission_rows: Vec<WriterIdleAdmissionRow>,
}

fn stage_5_merge_policy_candidates(doc_count: usize) -> Vec<MergePolicyCandidate> {
    vec![
        MergePolicyCandidate {
            name: "unmerged_reference",
            min_merge_segments: Some(doc_count + 1),
            max_docs_before_merge: Some(doc_count + 1),
            selectable: false,
        },
        MergePolicyCandidate {
            name: "default_log_merge_policy",
            min_merge_segments: None,
            max_docs_before_merge: None,
            selectable: true,
        },
        MergePolicyCandidate {
            name: "min8_target64",
            min_merge_segments: Some(8),
            max_docs_before_merge: Some((doc_count / 64).max(1)),
            selectable: true,
        },
        MergePolicyCandidate {
            name: "min8_target32",
            min_merge_segments: Some(8),
            max_docs_before_merge: Some((doc_count / 32).max(1)),
            selectable: true,
        },
        MergePolicyCandidate {
            name: "min8_target16",
            min_merge_segments: Some(8),
            max_docs_before_merge: Some((doc_count / 16).max(1)),
            selectable: true,
        },
        MergePolicyCandidate {
            name: "min8_target8",
            min_merge_segments: Some(8),
            max_docs_before_merge: Some((doc_count / 8).max(1)),
            selectable: true,
        },
        MergePolicyCandidate {
            name: "min4_target64",
            min_merge_segments: Some(4),
            max_docs_before_merge: Some((doc_count / 64).max(1)),
            selectable: true,
        },
        MergePolicyCandidate {
            name: "min4_target32",
            min_merge_segments: Some(4),
            max_docs_before_merge: Some((doc_count / 32).max(1)),
            selectable: true,
        },
        MergePolicyCandidate {
            name: "min4_target16",
            min_merge_segments: Some(4),
            max_docs_before_merge: Some((doc_count / 16).max(1)),
            selectable: true,
        },
        MergePolicyCandidate {
            name: "min4_target8",
            min_merge_segments: Some(4),
            max_docs_before_merge: Some((doc_count / 8).max(1)),
            selectable: true,
        },
    ]
}

fn stage_5_document(index: usize) -> crate::types::Document {
    let category = match index % 4 {
        0 => "tools",
        1 => "books",
        2 => "garden",
        _ => "kitchen",
    };
    let color = match index % 3 {
        0 => "red",
        1 => "blue",
        _ => "green",
    };
    let city = match index % 3 {
        0 => "new york",
        1 => "boston",
        _ => "chicago",
    };
    crate::types::Document::from_json(&serde_json::json!({
        "objectID": format!("stage5_doc_{index:03}"),
        "title": format!("alpha beta {category} {color} geo {city}"),
        "description": format!("deterministic merge policy specimen row {index}"),
        "category": category,
        "color": color,
        "price": index as i64,
        "rating": (1000 - index as i64),
        "_geoloc": {
            "lat": 40.7000 + (index as f64 * 0.0001),
            "lng": -74.0000 - (index as f64 * 0.0001)
        }
    }))
    .unwrap()
}

fn stage_5_corpus(doc_count: usize) -> Vec<crate::types::Document> {
    (0..doc_count).map(stage_5_document).collect()
}

fn stage_5_settings() -> crate::index::settings::IndexSettings {
    let mut settings = crate::index::settings::IndexSettings::default_with_facets(vec![
        "category".to_string(),
        "color".to_string(),
    ]);
    settings.searchable_attributes = Some(vec![
        "title".to_string(),
        "description".to_string(),
        "category".to_string(),
        "color".to_string(),
    ]);
    settings.custom_ranking = Some(vec!["asc(price)".to_string()]);
    settings
}

fn install_stage_5_geo_rule(manager: &Arc<crate::index::manager::IndexManager>, tenant_id: &str) {
    let rule = serde_json::json!({
        "objectID": "stage5_geo_search_probe",
        "conditions": [
            {
                "pattern": "stage5_geo_probe",
                "anchoring": "is"
            }
        ],
        "consequence": {
            "params": {
                "query": "",
                "aroundLatLng": "40.7000,-74.0000",
                "aroundRadius": "all"
            }
        }
    });
    let rules_path = manager.base_path.join(tenant_id).join("rules.json");
    std::fs::write(
        &rules_path,
        serde_json::to_string_pretty(&vec![rule]).unwrap(),
    )
    .unwrap();
    manager.invalidate_rules_cache(tenant_id);
}

async fn drain_stage_5_manager(
    manager: &Arc<crate::index::manager::IndexManager>,
    tenant_id: &str,
) {
    let handle = manager
        .write_task_handles
        .get(tenant_id)
        .map(|entry| entry.clone());
    manager.write_queues.remove(tenant_id);
    if let Some(handle) = handle {
        handle.drain(tenant_id.to_string()).await.unwrap();
    }
}

fn observe_stage_5_manager_segments(
    manager: &Arc<crate::index::manager::IndexManager>,
    tenant_id: &str,
) -> segment_observation::SegmentObservation {
    let index = manager
        .loaded
        .get(tenant_id)
        .unwrap_or_else(|| panic!("tenant {tenant_id} should be loaded"));
    observed_segments(&index)
}

fn stage_5_query_outcome(
    manager: &crate::index::manager::IndexManager,
    tenant_id: &str,
    query: &str,
    opts: &crate::index::SearchOptions<'_>,
    include_ids: bool,
) -> (MergePolicyQueryOutcome, u128) {
    let started_at = std::time::Instant::now();
    let result = manager.search_with_options(tenant_id, query, opts).unwrap();
    let effective_around_lat_lng = result.effective_around_lat_lng.clone();
    let effective_around_radius = result.effective_around_radius.clone();
    let elapsed_us = started_at.elapsed().as_micros();
    let ids = if include_ids {
        result
            .documents
            .iter()
            .map(|scored| scored.document.id.clone())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let facet_counts = result
        .facets
        .into_iter()
        .map(|(field, counts)| {
            (
                field,
                counts
                    .into_iter()
                    .map(|count| (count.path, count.count))
                    .collect(),
            )
        })
        .collect();
    (
        MergePolicyQueryOutcome {
            ids,
            total: result.total,
            facet_counts,
            effective_around_lat_lng,
            effective_around_radius,
        },
        elapsed_us,
    )
}

fn run_stage_5_query_suite(
    manager: &crate::index::manager::IndexManager,
    tenant_id: &str,
) -> (Vec<MergePolicyQueryOutcome>, Vec<u128>) {
    let facet_requests = vec![crate::types::FacetRequest {
        field: "category".to_string(),
        path: "/category".to_string(),
        value_query: None,
    }];
    let category_filter = crate::types::Filter::Equals {
        field: "category".to_string(),
        value: crate::types::FieldValue::Text("tools".to_string()),
    };
    let price_sort = crate::types::Sort::ByField {
        field: "price".to_string(),
        order: crate::types::SortOrder::Asc,
    };
    let price_desc_sort = crate::types::Sort::ByField {
        field: "price".to_string(),
        order: crate::types::SortOrder::Desc,
    };
    let specs = [
        (
            "alpha",
            true,
            crate::index::SearchOptions {
                sort: Some(&price_sort),
                limit: 10,
                ..Default::default()
            },
        ),
        (
            "alpha beta",
            true,
            crate::index::SearchOptions {
                sort: Some(&price_sort),
                limit: 10,
                typo_tolerance: Some(true),
                query_type: Some("prefixNone"),
                ..Default::default()
            },
        ),
        (
            "alpah",
            true,
            crate::index::SearchOptions {
                sort: Some(&price_sort),
                limit: 10,
                typo_tolerance: Some(true),
                ..Default::default()
            },
        ),
        (
            "alpha",
            false,
            crate::index::SearchOptions {
                sort: Some(&price_sort),
                limit: 10,
                facets: Some(&facet_requests),
                ..Default::default()
            },
        ),
        (
            "alpha",
            true,
            crate::index::SearchOptions {
                filter: Some(&category_filter),
                sort: Some(&price_sort),
                limit: 10,
                ..Default::default()
            },
        ),
        (
            "alpha",
            true,
            crate::index::SearchOptions {
                sort: Some(&price_desc_sort),
                limit: 10,
                ..Default::default()
            },
        ),
        (
            "alpha",
            true,
            crate::index::SearchOptions {
                sort: Some(&price_sort),
                limit: 5,
                offset: 5,
                ..Default::default()
            },
        ),
        (
            "stage5_geo_probe",
            true,
            crate::index::SearchOptions {
                sort: Some(&price_sort),
                limit: 512,
                ..Default::default()
            },
        ),
    ];

    specs
        .iter()
        .map(|(query, include_ids, opts)| {
            stage_5_query_outcome(manager, tenant_id, query, opts, *include_ids)
        })
        .unzip()
}

#[tokio::test(flavor = "current_thread")]
async fn stage_5_query_suite_includes_real_geo_search() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage5_geo_query_contract";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    stage_5_settings()
        .save(tmp.path().join(tenant_id).join("settings.json"))
        .unwrap();
    manager.invalidate_settings_cache(tenant_id);
    install_stage_5_geo_rule(&manager, tenant_id);

    for doc in stage_5_corpus(9) {
        manager
            .add_documents_sync(tenant_id, vec![doc])
            .await
            .unwrap();
    }
    drain_stage_5_manager(&manager, tenant_id).await;

    let (outcomes, _) = run_stage_5_query_suite(&manager, tenant_id);
    let geo_outcome = outcomes
        .iter()
        .find(|outcome| outcome.effective_around_lat_lng.is_some())
        .expect("Stage 5 matrix must include a real geo-search row");

    assert_eq!(
        geo_outcome.effective_around_lat_lng.as_deref(),
        Some("40.7000,-74.0000")
    );
    assert_eq!(
        geo_outcome.effective_around_radius,
        Some(serde_json::json!("all"))
    );
    assert_eq!(
        geo_outcome.ids,
        vec![
            "stage5_doc_000",
            "stage5_doc_001",
            "stage5_doc_002",
            "stage5_doc_003",
            "stage5_doc_004",
            "stage5_doc_005",
            "stage5_doc_006",
            "stage5_doc_007",
            "stage5_doc_008",
        ],
        "aroundRadius=all should keep every fixture row in distance order"
    );
}

async fn run_stage_5_candidate(
    candidate: MergePolicyCandidate,
    doc_count: usize,
) -> MergePolicyExperimentRow {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = format!(
        "stage5_{}_{}",
        candidate.name,
        uuid::Uuid::new_v4().simple()
    );
    let min_merge_segments = candidate.min_merge_segments.map(|value| value.to_string());
    let max_docs_before_merge = candidate
        .max_docs_before_merge
        .map(|value| value.to_string());
    let override_guards = apply_write_queue_env_overrides(&[
        (WRITE_QUEUE_BATCH_SIZE_ENV_VAR, Some("1")),
        (
            WRITE_QUEUE_MIN_MERGE_SEGMENTS_ENV_VAR,
            min_merge_segments.as_deref(),
        ),
        (
            WRITE_QUEUE_MAX_DOCS_BEFORE_MERGE_ENV_VAR,
            max_docs_before_merge.as_deref(),
        ),
        (WRITE_QUEUE_WRITER_IDLE_TIMEOUT_ENV_VAR, Some("1")),
    ]);

    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(&tenant_id).unwrap();
    stage_5_settings()
        .save(tmp.path().join(&tenant_id).join("settings.json"))
        .unwrap();
    manager.invalidate_settings_cache(&tenant_id);
    install_stage_5_geo_rule(&manager, &tenant_id);

    let mut peak_orphan_file_sets = 0usize;
    let import_started_at = std::time::Instant::now();
    for doc in stage_5_corpus(doc_count) {
        manager
            .add_documents_sync(&tenant_id, vec![doc])
            .await
            .unwrap();
        let observation = observe_stage_5_manager_segments(&manager, &tenant_id);
        peak_orphan_file_sets = peak_orphan_file_sets.max(observation.orphan_file_set_ids.len());
    }
    let import_wall_ms = import_started_at.elapsed().as_millis();
    drain_stage_5_manager(&manager, &tenant_id).await;
    let settled = observe_stage_5_manager_segments(&manager, &tenant_id);
    let (cold_outcomes, cold_latencies_us) = run_stage_5_query_suite(&manager, &tenant_id);
    let (warm_outcomes, warm_latencies_us) = run_stage_5_query_suite(&manager, &tenant_id);
    assert_eq!(
        cold_outcomes, warm_outcomes,
        "{} cold/warm exact-result parity failed",
        candidate.name
    );
    drop(override_guards);

    MergePolicyExperimentRow {
        name: candidate.name,
        selectable: candidate.selectable,
        settled_index_bytes: settled.index_bytes,
        live_segment_count: settled.live_segment_count,
        live_docs: settled.live_docs,
        peak_orphan_file_sets,
        settled_orphan_file_sets: settled.orphan_file_set_ids.len(),
        import_wall_ms,
        cold_latencies_us,
        warm_latencies_us,
        query_outcomes: warm_outcomes,
    }
}

fn assert_stage_5_candidate_matrix(
    rows: &[MergePolicyExperimentRow],
    doc_count: usize,
    expected_selected_name: &str,
) {
    let baseline = rows
        .first()
        .expect("candidate matrix should include an unmerged reference row");
    assert_eq!(baseline.name, "unmerged_reference");
    assert_eq!(baseline.live_docs, doc_count as u64);

    for row in rows {
        assert_eq!(row.live_docs, doc_count as u64, "{} live docs", row.name);
        assert_eq!(
            row.settled_orphan_file_sets, 0,
            "{} left settled orphan file sets: {row:?}",
            row.name
        );
        assert!(
            row.import_wall_ms > 0,
            "{} should record import wall time",
            row.name
        );
        assert_eq!(
            row.cold_latencies_us.len(),
            row.warm_latencies_us.len(),
            "{} cold/warm latency sample counts should match",
            row.name
        );
        assert!(
            row.peak_orphan_file_sets <= doc_count,
            "{} peak orphan count should be bounded by specimen size",
            row.name
        );
        assert_eq!(
            row.query_outcomes, baseline.query_outcomes,
            "{} query parity against unmerged reference failed",
            row.name
        );
    }

    let selected = rows
        .iter()
        .skip(1)
        .find(|row| {
            row.selectable
                && row.settled_index_bytes < baseline.settled_index_bytes
                && row.settled_orphan_file_sets == 0
                && row.query_outcomes == baseline.query_outcomes
        })
        .expect("one selectable candidate should beat the unmerged baseline");
    assert_eq!(selected.name, expected_selected_name);
}

fn stage_5_matrix_summary(rows: &[MergePolicyExperimentRow]) -> String {
    rows.iter()
        .map(|row| {
            format!(
                "{} bytes={} segments={} docs={} peak_orphans={} settled_orphans={} import_ms={} cold_us={:?} warm_us={:?}",
                row.name,
                row.settled_index_bytes,
                row.live_segment_count,
                row.live_docs,
                row.peak_orphan_file_sets,
                row.settled_orphan_file_sets,
                row.import_wall_ms,
                row.cold_latencies_us,
                row.warm_latencies_us
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn run_stage_5_candidate_matrix(doc_count: usize) -> Vec<MergePolicyExperimentRow> {
    let single_candidate = stage_5_merge_policy_candidates(doc_count)
        .into_iter()
        .find(|candidate| candidate.name == "unmerged_reference")
        .unwrap();
    let single_started_at = std::time::Instant::now();
    let single_row = run_stage_5_candidate(single_candidate, doc_count).await;
    let projected_full_ms = single_started_at.elapsed().as_millis()
        * stage_5_merge_policy_candidates(doc_count).len() as u128;
    assert!(
        projected_full_ms < 900_000,
        "projected Stage 5 matrix runtime {projected_full_ms}ms exceeds stage budget; shrink specimen"
    );

    let mut rows = vec![single_row];
    for candidate in stage_5_merge_policy_candidates(doc_count)
        .into_iter()
        .skip(1)
    {
        rows.push(run_stage_5_candidate(candidate, doc_count).await);
    }
    rows
}

fn writer_idle_timeout_candidates() -> Vec<WriterIdleTimeoutCandidate> {
    vec![
        WriterIdleTimeoutCandidate {
            name: "5s",
            timeout: Duration::from_secs(5),
        },
        WriterIdleTimeoutCandidate {
            name: "15s",
            timeout: Duration::from_secs(15),
        },
        WriterIdleTimeoutCandidate {
            name: "30s",
            timeout: Duration::from_secs(30),
        },
        WriterIdleTimeoutCandidate {
            name: "60s",
            timeout: Duration::from_secs(60),
        },
    ]
}

async fn run_writer_idle_trace_probe() -> Vec<u128> {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = format!("idle_trace_probe_{}", uuid::Uuid::new_v4().simple());
    let (_index, tx, handle, tasks) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        &tenant_id,
        Arc::new(MemoryBudget::new(MemoryBudgetConfig::default())),
        WriteQueueTestOverrides {
            batch_size: Some(1),
            writer_idle_timeout: Some(Duration::from_secs(120)),
            ..Default::default()
        },
    );
    let mut ack_ms = Vec::new();
    for sample in 0..3 {
        let task_id = register_task(
            tasks.as_ref(),
            &format!("idle_trace_probe_task_{sample}"),
            sample + 1,
            1,
        );
        let started_at = Instant::now();
        enqueue_write(
            &tx,
            task_id.clone(),
            vec![WriteAction::Add(text_document(
                &format!("idle_trace_probe_doc_{sample}"),
                "name",
                "idle trace probe",
            ))],
        )
        .await;
        wait_for_task_success(tasks.as_ref(), &task_id).await;
        ack_ms.push(started_at.elapsed().as_millis());
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    drop(tx);
    handle.await.unwrap().unwrap();
    ack_ms
}

fn writer_idle_resume_gaps_from_trace(ack_ms: &[u128]) -> Vec<Duration> {
    assert!(!ack_ms.is_empty(), "trace probe must produce ack samples");
    let mut sorted = ack_ms.to_vec();
    sorted.sort_unstable();
    let p95_ms = sorted[sorted.len() - 1].max(1);
    [(100, 10_000), (200, 25_000), (300, 35_000)]
        .into_iter()
        .map(|(multiplier, lower_bound_ms)| {
            let derived_ms = p95_ms.saturating_mul(multiplier);
            Duration::from_millis(derived_ms.max(lower_bound_ms).min(35_000) as u64)
        })
        .collect()
}

fn projected_writer_idle_runtime_ms(
    candidates: &[WriterIdleTimeoutCandidate],
    resume_gaps: &[Duration],
    admission_tenant_count: usize,
    trace_probe_ms: u128,
) -> u128 {
    let burst_sleep_ms =
        candidates.len() as u128 * resume_gaps.iter().map(|gap| gap.as_millis()).sum::<u128>();
    let admission_sleep_ms = candidates
        .iter()
        .map(|candidate| {
            candidate.timeout.as_millis() * admission_tenant_count.saturating_sub(1) as u128
        })
        .sum::<u128>();
    trace_probe_ms + burst_sleep_ms + admission_sleep_ms
}

async fn write_one_idle_timeout_experiment_doc(
    tx: &WriteQueue,
    tasks: &dashmap::DashMap<String, TaskInfo>,
    task_id: String,
    doc_id: String,
    batch_number: i64,
) -> Duration {
    let task_id = register_task(tasks, &task_id, batch_number, 1);
    let started_at = Instant::now();
    enqueue_write(
        tx,
        task_id.clone(),
        vec![WriteAction::Add(text_document(
            &doc_id,
            "name",
            "idle timeout experiment",
        ))],
    )
    .await;
    wait_for_task_success(tasks, &task_id).await;
    started_at.elapsed()
}

async fn run_writer_idle_burst_candidate(
    candidate: WriterIdleTimeoutCandidate,
    resume_gap: Duration,
) -> WriterIdleBurstRow {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = format!(
        "idle_burst_{}_{}",
        candidate.name,
        uuid::Uuid::new_v4().simple()
    );
    let (_index, tx, handle, tasks) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        &tenant_id,
        Arc::new(MemoryBudget::new(MemoryBudgetConfig::default())),
        WriteQueueTestOverrides {
            batch_size: Some(1),
            writer_idle_timeout: Some(candidate.timeout),
            ..Default::default()
        },
    );
    write_one_idle_timeout_experiment_doc(
        &tx,
        tasks.as_ref(),
        "idle_burst_first".to_string(),
        "idle_burst_doc_first".to_string(),
        1,
    )
    .await;
    let idle_wait_before = writer_merge_wait_count(&tenant_id, "idle_timeout");
    tokio::time::sleep(resume_gap).await;
    let opens_before = write_queue_counter_value(WRITE_QUEUE_WRITER_OPENS_METRIC_NAME, &tenant_id);
    let commits_before = write_queue_counter_value(WRITE_QUEUE_COMMITS_METRIC_NAME, &tenant_id);
    let second_write_wall = write_one_idle_timeout_experiment_doc(
        &tx,
        tasks.as_ref(),
        "idle_burst_second".to_string(),
        "idle_burst_doc_second".to_string(),
        2,
    )
    .await;
    let row = WriterIdleBurstRow {
        candidate_name: candidate.name,
        candidate_timeout: candidate.timeout,
        resume_gap,
        n: 1,
        second_write_wall_ms: second_write_wall.as_millis(),
        writer_open_delta: write_queue_counter_value(
            WRITE_QUEUE_WRITER_OPENS_METRIC_NAME,
            &tenant_id,
        ) - opens_before,
        commit_delta: write_queue_counter_value(WRITE_QUEUE_COMMITS_METRIC_NAME, &tenant_id)
            - commits_before,
        idle_merge_wait_delta: writer_merge_wait_count(&tenant_id, "idle_timeout")
            - idle_wait_before,
    };
    drop(tx);
    handle.await.unwrap().unwrap();
    row
}

async fn run_writer_idle_admission_candidate(
    candidate: WriterIdleTimeoutCandidate,
) -> WriterIdleAdmissionRow {
    const TENANT_COUNT: usize = 2;
    let tmp = tempfile::TempDir::new().unwrap();
    let shared_budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let mut queues = Vec::new();
    let mut admission_wait_ms = Vec::new();
    let mut tenant_ids = Vec::new();
    for tenant_number in 0..TENANT_COUNT {
        let started_at = Instant::now();
        if tenant_number > 0 {
            tokio::time::timeout(candidate.timeout + Duration::from_secs(5), async {
                while shared_budget.active_writers() != 0 {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("previous idle tenant must release its writer slot before the next tenant");
        }
        let tenant_id = format!(
            "idle_admission_{}_{}_{}",
            candidate.name,
            tenant_number,
            uuid::Uuid::new_v4().simple()
        );
        let (_index, tx, handle, tasks) = setup_write_queue_with_budget_and_overrides(
            &tmp,
            &tenant_id,
            Arc::clone(&shared_budget),
            WriteQueueTestOverrides {
                batch_size: Some(1),
                writer_idle_timeout: Some(candidate.timeout),
                ..Default::default()
            },
        );
        write_one_idle_timeout_experiment_doc(
            &tx,
            tasks.as_ref(),
            format!("idle_admission_task_{tenant_number}"),
            format!("idle_admission_doc_{tenant_number}"),
            tenant_number as i64 + 1,
        )
        .await;
        admission_wait_ms.push(started_at.elapsed().as_millis());
        tenant_ids.push(tenant_id);
        queues.push((tx, handle));
    }
    let idle_merge_wait_delta = tenant_ids
        .iter()
        .map(|tenant_id| writer_merge_wait_count(tenant_id, "idle_timeout"))
        .sum();
    let row = WriterIdleAdmissionRow {
        candidate_name: candidate.name,
        candidate_timeout: candidate.timeout,
        tenant_count: TENANT_COUNT,
        admitted_tenants: admission_wait_ms.len(),
        admission_wait_ms,
        idle_merge_wait_delta,
        final_active_writers: shared_budget.active_writers(),
    };
    for (tx, handle) in queues {
        drop(tx);
        handle.await.unwrap().unwrap();
    }
    row
}

async fn run_writer_idle_timeout_candidate_matrix() -> WriterIdleExperimentRows {
    let candidates = writer_idle_timeout_candidates();
    let trace_started_at = Instant::now();
    let trace_ack_ms = run_writer_idle_trace_probe().await;
    let trace_probe_ms = trace_started_at.elapsed().as_millis();
    let resume_gaps = writer_idle_resume_gaps_from_trace(&trace_ack_ms);
    let projected_full_runtime_ms =
        projected_writer_idle_runtime_ms(&candidates, &resume_gaps, 2, trace_probe_ms);
    assert!(
        projected_full_runtime_ms < 600_000,
        "projected Stage 6 idle-timeout matrix runtime {projected_full_runtime_ms}ms exceeds timeout 600"
    );

    let mut burst_rows = Vec::new();
    for candidate in &candidates {
        for resume_gap in &resume_gaps {
            burst_rows.push(run_writer_idle_burst_candidate(*candidate, *resume_gap).await);
        }
    }
    let mut admission_rows = Vec::new();
    for candidate in candidates {
        admission_rows.push(run_writer_idle_admission_candidate(candidate).await);
    }

    WriterIdleExperimentRows {
        trace_ack_ms,
        resume_gaps,
        projected_full_runtime_ms,
        burst_rows,
        admission_rows,
    }
}

fn writer_idle_timeout_matrix_summary(rows: &WriterIdleExperimentRows) -> String {
    let mut lines = vec![
        format!("trace_ack_ms={:?}", rows.trace_ack_ms),
        format!("resume_gaps={:?}", rows.resume_gaps),
        format!("projected_full_runtime_ms={}", rows.projected_full_runtime_ms),
        "burst candidate timeout gap n second_write_ms writer_open_delta commit_delta idle_merge_wait_delta".to_string(),
    ];
    lines.extend(rows.burst_rows.iter().map(|row| {
        format!(
            "burst {} {:?} {:?} {} {} {} {} {}",
            row.candidate_name,
            row.candidate_timeout,
            row.resume_gap,
            row.n,
            row.second_write_wall_ms,
            row.writer_open_delta,
            row.commit_delta,
            row.idle_merge_wait_delta
        )
    }));
    lines.push(
        "admission candidate timeout tenants admitted admission_wait_ms idle_merge_wait_delta final_active_writers"
            .to_string(),
    );
    lines.extend(rows.admission_rows.iter().map(|row| {
        format!(
            "admission {} {:?} {} {} {:?} {} {}",
            row.candidate_name,
            row.candidate_timeout,
            row.tenant_count,
            row.admitted_tenants,
            row.admission_wait_ms,
            row.idle_merge_wait_delta,
            row.final_active_writers
        )
    }));
    lines.join("\n")
}

fn assert_writer_idle_timeout_candidate_matrix(rows: &WriterIdleExperimentRows) {
    assert!(!rows.resume_gaps.is_empty(), "resume gaps must be measured");
    for row in &rows.burst_rows {
        assert!(row.n > 0, "burst row must not be vacuous: {row:?}");
        assert_eq!(row.commit_delta, 1, "second write must commit: {row:?}");
    }
    for row in &rows.admission_rows {
        assert_eq!(
            row.admitted_tenants, row.tenant_count,
            "every over-limit tenant should eventually admit through idle eviction: {row:?}"
        );
        assert!(
            row.idle_merge_wait_delta > 0,
            "admission must use idle-timeout merge-quiescent close: {row:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "Stage 6 evidence harness intentionally sleeps across idle-timeout candidates; run explicitly, not in the parallel write_queue sweep"]
async fn writer_idle_timeout_candidate_matrix_selects_default() {
    let _env_lock = WRITE_QUEUE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let rows = run_writer_idle_timeout_candidate_matrix().await;
    eprintln!(
        "Stage 6 writer idle-timeout matrix:\n{}",
        writer_idle_timeout_matrix_summary(&rows)
    );
    assert_writer_idle_timeout_candidate_matrix(&rows);
}

#[tokio::test(flavor = "current_thread")]
async fn merge_policy_converges_to_selected_segment_band() {
    let tmp = tempfile::TempDir::new().unwrap();
    let budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let doc_count = 128;
    let tenant_id = "selected_policy_convergence";
    let (index, tx, handle, tasks) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        tenant_id,
        budget,
        WriteQueueTestOverrides {
            batch_size: Some(1),
            ..Default::default()
        },
    );
    for doc_index in 0..doc_count {
        let task_id = register_task(
            tasks.as_ref(),
            &format!("selected_policy_{doc_index}"),
            1,
            1,
        );
        enqueue_write(
            &tx,
            task_id.clone(),
            vec![WriteAction::Add(stage_5_document(doc_index))],
        )
        .await;
        for _ in 0..500 {
            if task_succeeded(tasks.as_ref(), &task_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_task_succeeded(tasks.as_ref(), &task_id, 1);
    }
    drop(tx);
    handle.await.unwrap().unwrap();
    let observation = observed_segments(&index);
    let (min_segments, max_segments) = SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND;

    // The specimen writes 128 one-document queue tasks with batch size 1, so
    // the unmerged shape is 128 live segments. The selected Stage 5 policy must
    // settle that deterministic corpus into the measured production band.
    assert_eq!(observation.live_docs, doc_count as u64);
    assert!(
        (min_segments..=max_segments).contains(&observation.live_segment_count),
        "selected policy settled outside measured band {min_segments}..={max_segments}: {observation:?}"
    );
    assert!(
        observation.live_segment_count <= ONLINE_SPECIMEN_SETTLED_MAX,
        "online specimen must settle to at most {ONLINE_SPECIMEN_SETTLED_MAX} segments; the band's \
         upper reach belongs to the staged-bulk regime: {observation:?}"
    );
}

/// A staged bulk build commits large checkpoint-sized segments rather than the
/// tiny per-write segments of the online path, so the *same* selected merge
/// policy settles it into the upper reach of the canonical band instead of the
/// 2..=4 shape the small online specimens converge to. This is the real
/// bulk-build proof the Stage 6 review demanded: it measures the settled
/// segment count from actual behavior — an online 128-document specimen is not
/// a substitute — and proves the reconciled
/// `SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND` upper bound is load-bearing
/// rather than a recorded number echoed back. The 20-commit x 1000-document
/// corpus reproduces the measured staged-bulk settled shape (6 segments here;
/// the scale_ladder probe measured 8 at 50k and 9 at 100k) while staying fast
/// enough for the parallel lib sweep.
#[tokio::test(flavor = "current_thread")]
async fn bulk_scale_build_settles_within_selected_segment_band() {
    const COMMIT_COUNT: usize = 20;
    const DOCUMENTS_PER_COMMIT: usize = 1_000;
    const TOTAL_DOCUMENTS: usize = COMMIT_COUNT * DOCUMENTS_PER_COMMIT;

    let tmp = tempfile::TempDir::new().unwrap();
    let budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let tenant_id = "bulk_scale_segment_band";
    let (index, tx, handle, tasks) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        tenant_id,
        budget,
        WriteQueueTestOverrides {
            batch_size: Some(DOCUMENTS_PER_COMMIT),
            ..Default::default()
        },
    );

    let mut document_index = 0usize;
    for commit_index in 0..COMMIT_COUNT {
        let mut actions = Vec::with_capacity(DOCUMENTS_PER_COMMIT);
        for _ in 0..DOCUMENTS_PER_COMMIT {
            actions.push(WriteAction::Add(stage_5_document(document_index)));
            document_index += 1;
        }
        let task_id = register_task(
            tasks.as_ref(),
            &format!("bulk_scale_commit_{commit_index}"),
            1,
            DOCUMENTS_PER_COMMIT,
        );
        enqueue_write(&tx, task_id.clone(), actions).await;
        for _ in 0..2_000 {
            if task_succeeded(tasks.as_ref(), &task_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_task_succeeded(tasks.as_ref(), &task_id, DOCUMENTS_PER_COMMIT);
    }
    drop(tx);
    handle.await.unwrap().unwrap();

    let observation = observed_segments(&index);
    let (min_segments, max_segments) = SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND;

    assert_eq!(
        observation.live_docs, TOTAL_DOCUMENTS as u64,
        "every staged bulk document must survive merge settlement: {observation:?}"
    );
    assert!(
        (min_segments..=max_segments).contains(&observation.live_segment_count),
        "staged bulk build settled outside the selected band {min_segments}..={max_segments}: {observation:?}"
    );
    assert!(
        observation.live_segment_count > ONLINE_SPECIMEN_SETTLED_MAX,
        "staged bulk build must settle denser than the online {ONLINE_SPECIMEN_SETTLED_MAX}-segment specimens so the band's upper reach is exercised: {observation:?}"
    );
    assert!(
        observation.orphan_file_set_ids.is_empty(),
        "settled bulk observation must not retain stale file sets: {observation:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn write_path_exit_gate_on_local_standard_specimen() {
    const DOCUMENT_COUNT: usize = 128;
    let _env_lock = WRITE_QUEUE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _override_guards = apply_write_queue_env_overrides(&[
        (WRITE_QUEUE_BATCH_SIZE_ENV_VAR, None),
        (WRITE_QUEUE_MIN_MERGE_SEGMENTS_ENV_VAR, None),
        (WRITE_QUEUE_MAX_DOCS_BEFORE_MERGE_ENV_VAR, None),
        (WRITE_QUEUE_WRITER_IDLE_TIMEOUT_ENV_VAR, None),
    ]);
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage7_write_path_exit_gate";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    stage_5_settings()
        .save(tmp.path().join(tenant_id).join("settings.json"))
        .unwrap();
    manager.invalidate_settings_cache(tenant_id);
    install_stage_5_geo_rule(&manager, tenant_id);

    // A bare durable-ack failure here cannot be told apart from a slow host, so
    // record which write stalled and for how long against the rest of the run.
    let mut write_durations = Vec::with_capacity(DOCUMENT_COUNT);
    for (write_index, document) in stage_5_corpus(DOCUMENT_COUNT).into_iter().enumerate() {
        let started_at = std::time::Instant::now();
        let outcome = manager
            .add_documents_durable(tenant_id, vec![document])
            .await;
        write_durations.push(started_at.elapsed());
        if let Err(error) = outcome {
            panic!(
                "every acknowledged Stage 7 write must commit durably; \
                 write {write_index} of {DOCUMENT_COUNT} failed with {error:?} after {:?}, \
                 slowest preceding write {:?}",
                write_durations[write_index],
                write_durations[..write_index].iter().max()
            );
        }
    }
    assert!(
        crate::index::write_queue::admission::WriteAdmissionStore::open(tmp.path(), tenant_id)
            .unwrap()
            .load_records()
            .unwrap()
            .is_empty(),
        "acknowledged writes must not leave replay records pending"
    );

    drain_stage_5_manager(&manager, tenant_id).await;
    let first_settled = observe_stage_5_manager_segments(&manager, tenant_id);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second_settled = observe_stage_5_manager_segments(&manager, tenant_id);
    let (min_segments, max_segments) = SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND;

    assert_eq!(first_settled.live_docs, DOCUMENT_COUNT as u64);
    assert_eq!(second_settled.live_docs, DOCUMENT_COUNT as u64);
    assert!(
        (min_segments..=max_segments).contains(&second_settled.live_segment_count),
        "settled Stage 7 specimen is outside selected band {min_segments}..={max_segments}: {second_settled:?}"
    );
    assert!(
        second_settled.live_segment_count <= ONLINE_SPECIMEN_SETTLED_MAX,
        "online exit-gate specimen must settle to at most {ONLINE_SPECIMEN_SETTLED_MAX} segments; \
         the band's upper reach belongs to the staged-bulk regime: {second_settled:?}"
    );
    assert!(
        first_settled.orphan_file_set_ids.is_empty()
            && second_settled.orphan_file_set_ids.is_empty(),
        "settled observations must not contain stale file sets: first={first_settled:?}, second={second_settled:?}"
    );
    assert_eq!(
        second_settled.managed_index_file_count, first_settled.managed_index_file_count,
        "managed index files grew after settlement"
    );
    assert_eq!(
        second_settled.live_segment_ids, first_settled.live_segment_ids,
        "live segment set changed after merge settlement"
    );

    drop(manager);
    let restarted_manager = crate::index::manager::IndexManager::new(tmp.path());
    let (query_outcomes, _) = run_stage_5_query_suite(&restarted_manager, tenant_id);
    assert_eq!(
        query_outcomes[0].ids.first().map(String::as_str),
        Some("stage5_doc_000"),
        "ascending-rank sentinel must remain rank 1 after manager rebuild"
    );
    assert_eq!(
        query_outcomes[5].ids.first().map(String::as_str),
        Some("stage5_doc_127"),
        "descending-rank sentinel must remain rank 1 after manager rebuild"
    );
    assert_eq!(
        restarted_manager.tenant_doc_count(tenant_id),
        Some(DOCUMENT_COUNT as u64),
        "manager rebuild must expose every acknowledged write exactly once"
    );
    assert!(
        crate::index::write_queue::admission::WriteAdmissionStore::open(tmp.path(), tenant_id)
            .unwrap()
            .load_records()
            .unwrap()
            .is_empty(),
        "manager rebuild must not replay already-acknowledged writes"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_timeout_write_queue_helper_uses_durable_admission_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "write_queue_helper_durable_store";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    let durable_append_seen = Arc::new(AtomicBool::new(false));
    manager
        .set_write_admission_after_stage_hook_for_test(tenant_id, {
            let durable_append_seen = Arc::clone(&durable_append_seen);
            move || durable_append_seen.store(true, Ordering::Release)
        })
        .unwrap();

    add_documents_and_wait_for_test(
        &manager,
        tenant_id,
        vec![text_document(
            "durable_append_doc",
            "name",
            "durable admission must be used",
        )],
    )
    .await
    .unwrap();

    assert!(
        durable_append_seen.load(Ordering::Acquire),
        "explicit-timeout write-queue helper must route through durable admission append"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn count_latency_stays_under_gate_during_writes() {
    const BULK_DOCUMENT_COUNT: usize = 4_000;
    const COUNT_LATENCY_GATE: Duration = Duration::from_millis(250);
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage7_count_latency";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    add_documents_and_wait_for_test(
        &manager,
        tenant_id,
        vec![text_document("count_seed", "name", "count latency seed")],
    )
    .await
    .unwrap();
    assert_eq!(manager.tenant_doc_count(tenant_id), Some(1));

    let samples = Arc::new(Mutex::new(Vec::<(u64, Duration)>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = std::thread::spawn({
        let manager = Arc::clone(&manager);
        let samples = Arc::clone(&samples);
        let stop = Arc::clone(&stop);
        move || {
            while !stop.load(Ordering::Acquire) {
                if finalization::commit_is_in_progress_for_test(tenant_id) {
                    let started_at = std::time::Instant::now();
                    let count = manager
                        .tenant_doc_count(tenant_id)
                        .expect("sampler tenant must stay loaded");
                    samples.lock().unwrap().push((count, started_at.elapsed()));
                }
                std::thread::sleep(Duration::from_micros(50));
            }
        }
    });

    let documents = (0..BULK_DOCUMENT_COUNT)
        .map(|index| {
            text_document(
                &format!("count_doc_{index}"),
                "name",
                "count latency bulk write",
            )
        })
        .collect();
    add_documents_and_wait_for_test(&manager, tenant_id, documents)
        .await
        .expect("bulk write must commit");
    let expected_final_count = BULK_DOCUMENT_COUNT as u64 + 1;
    stop.store(true, Ordering::Release);
    sampler.join().unwrap();
    assert_eq!(
        manager.tenant_doc_count(tenant_id),
        Some(expected_final_count),
        "durable write must publish the final document count"
    );

    let samples = samples.lock().unwrap();
    assert!(
        !samples.is_empty(),
        "zero count-latency samples during writer.commit is not valid evidence"
    );
    assert!(
        samples.iter().all(|(count, _)| *count == 1),
        "count reads during writer.commit must expose the last published reader state: {samples:?}"
    );
    let mut latencies = samples
        .iter()
        .map(|(_, latency)| *latency)
        .collect::<Vec<_>>();
    latencies.sort_unstable();
    let p95 = latencies[(latencies.len() * 95 / 100).min(latencies.len() - 1)];
    let max = *latencies.last().unwrap();
    eprintln!(
        "Stage 7 count latency: samples={} p95_us={} max_us={} gate_ms={}",
        latencies.len(),
        p95.as_micros(),
        max.as_micros(),
        COUNT_LATENCY_GATE.as_millis()
    );
    assert!(
        max < COUNT_LATENCY_GATE,
        "tenant_doc_count exceeded {:?} during a durable write: p95={p95:?}, max={max:?}",
        COUNT_LATENCY_GATE
    );
}

#[tokio::test(flavor = "current_thread")]
async fn move_index_drains_source_persistent_writer_before_publication() {
    const DOCUMENT_COUNT: usize = 2_000;
    let tmp = tempfile::TempDir::new().unwrap();
    let source = "move_source_with_active_writer";
    let destination = "move_destination_after_drain";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(source).unwrap();

    let documents = (0..DOCUMENT_COUNT)
        .map(|index| {
            text_document(
                &format!("move_doc_{index}"),
                "name",
                "source drain publication specimen",
            )
        })
        .collect();
    manager.add_documents(source, documents).unwrap();

    manager
        .move_index(source, destination)
        .await
        .expect("move must drain the source worker before publishing its files");

    let moved_results = manager
        .search(
            destination,
            "source drain publication specimen",
            None,
            None,
            DOCUMENT_COUNT + 1,
        )
        .expect("moved index should be searchable");
    assert_eq!(
        moved_results.total, DOCUMENT_COUNT,
        "every source write accepted before the move must be published at the destination"
    );
    assert!(
        !tmp.path().join(source).exists(),
        "successful move must remove the drained source directory"
    );
    assert!(
        !manager.write_task_handles.contains_key(source),
        "source write-task ownership must be retired before move returns"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "Stage 5 evidence harness mutates process env; run explicitly, not in the parallel write_queue sweep"]
async fn merge_policy_candidate_matrix_selects_first_passing_policy() {
    let _env_lock = WRITE_QUEUE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let rows = run_stage_5_candidate_matrix(128).await;
    eprintln!("Stage 5 base matrix:\n{}", stage_5_matrix_summary(&rows));
    assert_stage_5_candidate_matrix(&rows, 128, "default_log_merge_policy");

    let larger_rows = run_stage_5_candidate_matrix(256).await;
    eprintln!(
        "Stage 5 larger matrix:\n{}",
        stage_5_matrix_summary(&larger_rows)
    );
    assert_stage_5_candidate_matrix(&larger_rows, 256, "default_log_merge_policy");
}

fn assert_metric_sample(
    metrics_text: &str,
    metric_name: &str,
    labels: &[(&str, &str)],
    value: u64,
) {
    let expected_labels = labels
        .iter()
        .map(|(name, value)| format!("{name}=\"{value}\""))
        .collect::<Vec<_>>();
    let found = metrics_text.lines().any(|line| {
        line.starts_with(metric_name)
            && expected_labels.iter().all(|label| line.contains(label))
            && line.ends_with(&format!(" {value}"))
    });
    assert!(
        found,
        "expected sample {metric_name}{{{}}} {value}, got:\n{metrics_text}",
        expected_labels.join(",")
    );
}

fn write_queue_counter_value(metric_name: &str, tenant_id: &str) -> u64 {
    write_queue_counter_value_with_labels(metric_name, &[("tenant", tenant_id)])
}

fn write_queue_metric_value(metric_name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    let metrics_text = write_queue_phase_metrics_text();
    metrics_text.lines().find_map(|line| {
        let has_expected_labels = labels
            .iter()
            .all(|(name, value)| line.contains(&format!("{name}=\"{value}\"")));
        if line.starts_with(metric_name) && has_expected_labels {
            line.rsplit_once(' ')
                .and_then(|(_, value)| value.parse::<f64>().ok())
        } else {
            None
        }
    })
}

fn assert_histogram_count_at_least(
    metrics_text: &str,
    metric_name: &str,
    labels: &[(&str, &str)],
    minimum_count: u64,
) {
    let count_metric_name = format!("{metric_name}_count");
    let expected_labels = labels
        .iter()
        .map(|(name, value)| format!("{name}=\"{value}\""))
        .collect::<Vec<_>>();
    let count = metrics_text
        .lines()
        .find_map(|line| {
            if line.starts_with(&count_metric_name)
                && expected_labels.iter().all(|label| line.contains(label))
            {
                line.rsplit_once(' ')
                    .and_then(|(_, value)| value.parse::<u64>().ok())
            } else {
                None
            }
        })
        .unwrap_or(0);
    assert!(
        count >= minimum_count,
        "expected {count_metric_name}{{{}}} >= {minimum_count}, got {count}; metrics:\n{metrics_text}",
        expected_labels.join(",")
    );
}

fn histogram_count(metric_name: &str, labels: &[(&str, &str)]) -> u64 {
    let count_metric_name = format!("{metric_name}_count");
    write_queue_metric_value(&count_metric_name, labels)
        .unwrap_or(0.0)
        .round() as u64
}

fn writer_merge_wait_count(tenant_id: &str, reason: &str) -> u64 {
    histogram_count(
        WRITE_QUEUE_WRITER_MERGE_WAIT_METRIC_NAME,
        &[("tenant", tenant_id), ("reason", reason)],
    )
}

fn write_queue_counter_value_with_labels(metric_name: &str, labels: &[(&str, &str)]) -> u64 {
    let metrics_text = write_queue_phase_metrics_text();
    metrics_text
        .lines()
        .find_map(|line| {
            let has_expected_labels = labels
                .iter()
                .all(|(name, value)| line.contains(&format!("{name}=\"{value}\"")));
            if line.starts_with(metric_name) && has_expected_labels {
                line.rsplit_once(' ')
                    .and_then(|(_, value)| value.parse::<u64>().ok())
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn write_queue_counter_has_tenant(metric_name: &str, tenant_id: &str) -> bool {
    write_queue_phase_metrics_text().lines().any(|line| {
        line.starts_with(metric_name) && line.contains(&format!("tenant=\"{tenant_id}\""))
    })
}

/// Wall-clock budget for waiting on background write-queue progress.
///
/// This is a liveness bound, not a correctness threshold: the assertions around
/// each wait are what prove the behavior. It is deliberately generous because the
/// suite runs thousands of tests in parallel on shared hosts, where seconds of
/// scheduling delay are normal and say nothing about the write queue. A real
/// stall — a contended writer that is never yielded, a task that never commits,
/// a merge that never converges — still turns every wait below red.
const WRITE_QUEUE_PROGRESS_TIMEOUT: Duration = Duration::from_secs(30);

async fn wait_for_task_success(tasks: &dashmap::DashMap<String, TaskInfo>, task_id: &str) {
    tokio::time::timeout(WRITE_QUEUE_PROGRESS_TIMEOUT, async {
        while !task_succeeded(tasks, task_id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("write queue task should succeed before timeout");
}

async fn enqueue_writes_until_stopped(
    tx: WriteQueue,
    tasks: Arc<dashmap::DashMap<String, TaskInfo>>,
    stop: Arc<AtomicBool>,
) -> Vec<String> {
    let mut task_ids = Vec::new();
    while !stop.load(Ordering::Acquire) {
        let sequence = task_ids.len();
        let task_id = register_task(
            tasks.as_ref(),
            &format!("busy_tenant_a_task_{sequence}"),
            sequence as i64 + 2,
            1,
        );
        enqueue_write(
            &tx,
            task_id.clone(),
            vec![WriteAction::Add(text_document(
                &format!("busy_a_doc_{sequence}"),
                "name",
                "continuous tenant A write",
            ))],
        )
        .await;
        task_ids.push(task_id);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    task_ids
}

#[test]
fn test_write_queue_batch_size_uses_default_when_env_unset() {
    with_write_queue_batch_size_env(None, || {
        assert_eq!(write_queue_batch_size(), DEFAULT_WRITE_QUEUE_BATCH_SIZE);
    });
}

#[test]
fn segment_health_reports_live_stale_and_docs_per_segment() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = format!("segment_health_{}", uuid::Uuid::new_v4().simple());
    let tenant_path = tmp.path().join(&tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = crate::index::Index::create(&tenant_path, schema).unwrap();

    let mut writer = index.writer().unwrap();
    index
        .add_documents(
            &mut writer,
            vec![
                text_document("doc_1", "name", "live document"),
                text_document("doc_2", "name", "deleted document"),
            ],
        )
        .unwrap();
    writer.commit().unwrap();
    writer.delete_term(tantivy::Term::from_field_text(
        index.inner().schema().get_field("_id").unwrap(),
        "doc_2",
    ));
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();
    index.reader().reload().unwrap();

    let orphan_segment_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let orphan_path = tenant_path.join(format!("{orphan_segment_id}.store"));
    std::fs::File::create(&orphan_path)
        .unwrap()
        .write_all(b"orphan-bytes")
        .unwrap();

    let live_metas = index.inner().searchable_segment_metas().unwrap();
    let expected_live_segment_ids = live_metas
        .iter()
        .map(|meta| meta.id().uuid_string())
        .collect::<BTreeSet<_>>();
    let expected_per_segment_doc_counts = live_metas
        .iter()
        .map(|meta| {
            (
                meta.id().uuid_string(),
                u64::from(meta.max_doc() - meta.num_deleted_docs()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let expected_live_docs = expected_per_segment_doc_counts.values().sum::<u64>();
    let expected_index_file_count = index.inner().directory().list_managed_files().len() as u64;
    let expected_index_bytes = crate::index::storage_size::dir_size_bytes(&tenant_path).unwrap();

    let observation = segment_observation::observe_segments(&index).unwrap();
    assert_eq!(observation.live_segment_ids, expected_live_segment_ids);
    assert_eq!(observation.live_segment_count, live_metas.len());
    assert_eq!(
        observation.per_segment_doc_counts,
        expected_per_segment_doc_counts
    );
    assert_eq!(observation.live_docs, expected_live_docs);
    assert_eq!(
        observation.managed_index_file_count,
        expected_index_file_count
    );
    assert_eq!(observation.index_bytes, expected_index_bytes);
    assert_eq!(
        observation.orphan_file_set_ids,
        BTreeSet::from([orphan_segment_id.to_string()])
    );

    observe_write_queue_segment_health(&tenant_id, &observation);
    let metrics_text = write_queue_phase_metrics_text();
    assert_metric_sample(
        &metrics_text,
        "flapjack_write_queue_live_segments",
        &[("tenant", &tenant_id)],
        live_metas.len() as u64,
    );
    assert_metric_sample(
        &metrics_text,
        "flapjack_write_queue_live_docs",
        &[("tenant", &tenant_id)],
        expected_live_docs,
    );
    for (segment_id, doc_count) in &observation.per_segment_doc_counts {
        assert_metric_sample(
            &metrics_text,
            "flapjack_write_queue_documents_per_segment",
            &[("tenant", &tenant_id), ("segment", segment_id)],
            *doc_count,
        );
    }
    assert_metric_sample(
        &metrics_text,
        "flapjack_write_queue_index_files",
        &[("tenant", &tenant_id)],
        expected_index_file_count,
    );
    assert_metric_sample(
        &metrics_text,
        "flapjack_write_queue_index_bytes",
        &[("tenant", &tenant_id)],
        expected_index_bytes,
    );
    assert_metric_sample(
        &metrics_text,
        "flapjack_write_queue_orphan_file_sets",
        &[("tenant", &tenant_id)],
        1,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn hundred_commits_do_not_leave_hundred_flush_segments() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = format!("hundred_commits_{}", uuid::Uuid::new_v4().simple());
    let tenant_path = tmp.path().join(&tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    let (tx, handle, tasks) = setup_write_queue_with_index_and_overrides(
        &tmp,
        &tenant_id,
        Arc::clone(&index),
        WriteQueueTestOverrides {
            batch_size: Some(1),
            min_merge_segments: Some(2),
            max_docs_before_merge: Some(1000),
            writer_idle_timeout: None,
            ..Default::default()
        },
    );

    const COMMIT_COUNT: usize = 100;
    for i in 0..COMMIT_COUNT {
        let task_id = register_task(
            tasks.as_ref(),
            &format!("hundred_commits_task_{i}"),
            i as i64 + 1,
            1,
        );
        enqueue_write(
            &tx,
            task_id.clone(),
            vec![WriteAction::Add(text_document(
                &format!("doc_{i}"),
                "name",
                "merge candidate",
            ))],
        )
        .await;
        wait_for_task_success(tasks.as_ref(), &task_id).await;
    }

    drop(tx);
    tokio::time::timeout(WRITE_QUEUE_PROGRESS_TIMEOUT, handle)
        .await
        .expect("worker should finish channel-closed merge quiescence before timeout")
        .expect("write queue worker task should join successfully")
        .expect("write queue worker should shut down successfully");

    let observation = observed_segments(index.as_ref());

    assert_eq!(
        observation.live_docs, COMMIT_COUNT as u64,
        "100 single-document commits should leave exactly 100 live docs; got {observation:?}"
    );
    assert!(
        observation.live_segment_count < COMMIT_COUNT / 2,
        "test merge policy should keep live segments materially below {COMMIT_COUNT}; got {observation:?}"
    );
    assert!(
        observation.orphan_file_set_ids.is_empty(),
        "merge cleanup should not leave orphan file sets; got {observation:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn merge_owner_survives_consecutive_commits() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = format!("merge_owner_{}", uuid::Uuid::new_v4().simple());
    let tenant_path = tmp.path().join(&tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    let (tx, handle, tasks) = setup_write_queue_with_index_and_overrides(
        &tmp,
        &tenant_id,
        Arc::clone(&index),
        WriteQueueTestOverrides {
            batch_size: Some(1),
            min_merge_segments: Some(2),
            max_docs_before_merge: Some(1000),
            writer_idle_timeout: None,
            ..Default::default()
        },
    );

    for i in 0..2 {
        let task_id = register_task(
            tasks.as_ref(),
            &format!("merge_owner_task_{i}"),
            i as i64 + 1,
            1,
        );
        enqueue_write(
            &tx,
            task_id.clone(),
            vec![WriteAction::Add(text_document(
                &format!("doc_{i}"),
                "name",
                "merge owner",
            ))],
        )
        .await;
        wait_for_task_success(tasks.as_ref(), &task_id).await;
    }

    assert_eq!(
        write_queue_counter_value(WRITE_QUEUE_WRITER_OPENS_METRIC_NAME, &tenant_id),
        1,
        "tenant worker should open exactly one writer"
    );
    assert!(
        write_queue_counter_value(WRITE_QUEUE_COMMITS_METRIC_NAME, &tenant_id) >= 2,
        "tenant worker should record at least two successful commits"
    );

    drop(tx);
    tokio::time::timeout(WRITE_QUEUE_PROGRESS_TIMEOUT, handle)
        .await
        .expect("worker should finish channel-closed merge quiescence before timeout")
        .expect("write queue worker task should join successfully")
        .expect("write queue worker should shut down successfully");

    let lifecycle_events = writer_lifecycle::writer_lifecycle_test_events(&tenant_id);
    assert!(
        lifecycle_events
            .iter()
            .any(|event| event.reason == "channel_closed" && event.phase == "merge_quiesced"),
        "worker shutdown should retain channel-closed merge quiescence before segment census; got {lifecycle_events:?}"
    );

    let converged = observed_segments(index.as_ref());
    assert_eq!(
        converged.live_docs, 2,
        "converged index should retain both committed docs; got {converged:?}"
    );
    assert_eq!(
        converged.live_segment_count, 1,
        "converged index should have exactly one live segment after worker-owned merge quiescence; got {converged:?}"
    );
    assert!(
        converged.orphan_file_set_ids.is_empty(),
        "converged index should not leave orphan file sets; got {converged:?}"
    );
}

#[test]
fn test_write_queue_batch_size_uses_env_override_when_valid() {
    with_write_queue_batch_size_env(Some("64"), || {
        assert_eq!(write_queue_batch_size(), 64);
    });
}

#[test]
fn test_write_queue_batch_size_falls_back_on_malformed_env() {
    with_write_queue_batch_size_env(Some("not-a-number"), || {
        assert_eq!(write_queue_batch_size(), DEFAULT_WRITE_QUEUE_BATCH_SIZE);
    });
}

#[test]
fn test_write_queue_batch_size_falls_back_on_zero_env() {
    with_write_queue_batch_size_env(Some("0"), || {
        assert_eq!(write_queue_batch_size(), DEFAULT_WRITE_QUEUE_BATCH_SIZE);
    });
}

#[test]
fn writer_idle_timeout_uses_env_override_when_valid() {
    let _env_lock = WRITE_QUEUE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env_guards =
        apply_write_queue_env_overrides(&[(WRITE_QUEUE_WRITER_IDLE_TIMEOUT_ENV_VAR, Some("25"))]);

    assert_eq!(
        writer_lifecycle::configured_writer_idle_timeout(),
        Duration::from_millis(25)
    );
}

#[test]
fn writer_idle_timeout_falls_back_to_selected_default_when_env_missing_or_malformed() {
    let _env_lock = WRITE_QUEUE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _missing_guard =
        WriteQueueEnvVarRestoreGuard::apply(WRITE_QUEUE_WRITER_IDLE_TIMEOUT_ENV_VAR, None);
    assert_eq!(
        writer_lifecycle::configured_writer_idle_timeout(),
        writer_lifecycle::DEFAULT_WRITER_IDLE_TIMEOUT,
        "missing env should use the selected default owner"
    );
    drop(_missing_guard);

    let _malformed_guard = WriteQueueEnvVarRestoreGuard::apply(
        WRITE_QUEUE_WRITER_IDLE_TIMEOUT_ENV_VAR,
        Some("not-a-number"),
    );
    assert_eq!(
        writer_lifecycle::configured_writer_idle_timeout(),
        writer_lifecycle::DEFAULT_WRITER_IDLE_TIMEOUT,
        "malformed env should use the selected default owner"
    );
}

#[test]
fn default_write_queue_channel_capacity_stays_below_timeout_risk_depth() {
    with_write_queue_channel_capacity_env(None, || {
        assert!(
            write_queue_channel_capacity() < JULY_22_TIMEOUT_RISK_PENDING_ADMISSIONS,
            "default write queue channel capacity must reject before the July 22 observed {JULY_22_TIMEOUT_RISK_PENDING_ADMISSIONS} pending-admission timeout-risk depth"
        );
    });
}

#[tokio::test]
async fn test_multiple_queues_progress_under_tight_writer_budget() {
    let tmp = tempfile::TempDir::new().unwrap();
    let shared_budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let (index_a, tx_a, handle_a, tasks_a) =
        setup_write_queue_with_budget(&tmp, "budget_a", Arc::clone(&shared_budget));
    let (index_b, tx_b, handle_b, tasks_b) =
        setup_write_queue_with_budget(&tmp, "budget_b", Arc::clone(&shared_budget));

    let task_a = register_task(tasks_a.as_ref(), "budget_task_a", 1, 1);
    enqueue_write(
        &tx_a,
        task_a.clone(),
        vec![WriteAction::Add(text_document("a1", "name", "A"))],
    )
    .await;

    let task_b = register_task(tasks_b.as_ref(), "budget_task_b", 2, 1);
    enqueue_write(
        &tx_b,
        task_b.clone(),
        vec![WriteAction::Add(text_document("b1", "name", "B"))],
    )
    .await;

    tokio::join!(
        wait_for_task_success(tasks_a.as_ref(), &task_a),
        wait_for_task_success(tasks_b.as_ref(), &task_b)
    );
    assert_eq!(
        shared_budget.active_writers(),
        1,
        "one-slot budget must have exactly one active writer"
    );
    assert!(
        !shared_budget.has_writer_waiters(),
        "writer waiter registration must retire after acquisition"
    );

    let count_a = indexed_document_count(index_a.as_ref());
    assert_eq!(
        count_a, 1,
        "tenant A document should be searchable while both queues remain open"
    );

    let count_b = indexed_document_count(index_b.as_ref());
    assert_eq!(
        count_b, 1,
        "tenant B document should be searchable while both queues remain open"
    );

    drop(tx_a);
    drop(tx_b);
    handle_a.await.unwrap().unwrap();
    handle_b.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn writer_memory_admission_counts_persistent_writer_budget() {
    let tmp = tempfile::TempDir::new().unwrap();
    let shared_budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let (_index, tx, handle, tasks) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        "persistent_writer_budget",
        Arc::clone(&shared_budget),
        WriteQueueTestOverrides {
            batch_size: Some(1),
            ..Default::default()
        },
    );
    let task_id = register_task(tasks.as_ref(), "persistent_writer_budget_task", 1, 1);
    enqueue_write(
        &tx,
        task_id.clone(),
        vec![WriteAction::Add(text_document(
            "persistent_writer_budget_doc",
            "name",
            "persistent writer",
        ))],
    )
    .await;
    wait_for_task_success(tasks.as_ref(), &task_id).await;

    assert_eq!(
        shared_budget.active_writers(),
        1,
        "an open queue writer must continue consuming its memory-budget slot"
    );
    let direct_writer_result = shared_budget.acquire_writer();
    assert!(
        matches!(
            direct_writer_result,
            Err(FlapjackError::TooManyConcurrentWrites { current: 2, max: 1 })
        ),
        "a persistent queue writer must prevent a second writer from over-allocating the budget"
    );

    drop(tx);
    handle.await.unwrap().unwrap();
}

async fn assert_idle_writer_eviction_releases_budget_and_allows_more_tenants(
    writer_idle_timeout: Option<Duration>,
    idle_wait_timeout: Duration,
) {
    let tmp = tempfile::TempDir::new().unwrap();
    let shared_budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let mut queues = Vec::new();

    for tenant_number in 0..3 {
        let tenant_id = format!("idle_eviction_tenant_{tenant_number}");
        let merge_wait_before = writer_merge_wait_count(&tenant_id, "idle_timeout");
        let (index, tx, handle, tasks) = setup_write_queue_with_budget_and_overrides(
            &tmp,
            &tenant_id,
            Arc::clone(&shared_budget),
            WriteQueueTestOverrides {
                batch_size: Some(1),
                writer_idle_timeout,
                ..Default::default()
            },
        );
        let task_id = register_task(
            tasks.as_ref(),
            &format!("idle_eviction_task_{tenant_number}"),
            tenant_number + 1,
            1,
        );
        enqueue_write(
            &tx,
            task_id.clone(),
            vec![WriteAction::Add(text_document(
                &format!("idle_eviction_doc_{tenant_number}"),
                "name",
                "known answer",
            ))],
        )
        .await;
        wait_for_task_success(tasks.as_ref(), &task_id).await;
        wait_for_writer_merge_wait_count(
            &tenant_id,
            "idle_timeout",
            merge_wait_before + 1,
            idle_wait_timeout,
        )
        .await;
        assert_eq!(
            indexed_document_count(index.as_ref()),
            1,
            "tenant {tenant_number} must retain its known-answer searchable write"
        );
        assert_eq!(
            writer_merge_wait_count(&tenant_id, "idle_timeout") - merge_wait_before,
            1,
            "idle eviction for tenant {tenant_number} must close through the merge-quiescent writer lifecycle"
        );
        queues.push((tx, handle));
    }

    assert_eq!(
        shared_budget.active_writers(),
        0,
        "every idle queue must release its persistent writer budget slot"
    );
    for (tx, handle) in queues {
        drop(tx);
        handle.await.unwrap().unwrap();
    }
}

async fn wait_for_writer_merge_wait_count(
    tenant_id: &str,
    reason: &str,
    expected_count: u64,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        let observed = writer_merge_wait_count(tenant_id, reason);
        if observed >= expected_count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for writer merge-wait count {expected_count} for tenant {tenant_id}; observed {observed}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn idle_writer_eviction_releases_budget_and_allows_more_tenants() {
    assert_idle_writer_eviction_releases_budget_and_allows_more_tenants(
        Some(Duration::from_millis(25)),
        Duration::from_secs(2),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "selected 30s default is intentionally too slow for the parallel unit sweep; Stage 6 ignored matrix protects this timing path"]
async fn selected_default_idle_writer_eviction_releases_budget_and_allows_more_tenants() {
    let _env_lock = WRITE_QUEUE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env_guard =
        WriteQueueEnvVarRestoreGuard::apply(WRITE_QUEUE_WRITER_IDLE_TIMEOUT_ENV_VAR, None);
    assert_idle_writer_eviction_releases_budget_and_allows_more_tenants(
        None,
        writer_lifecycle::DEFAULT_WRITER_IDLE_TIMEOUT + Duration::from_millis(500),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn contention_yield_records_writer_close_reason() {
    let tmp = tempfile::TempDir::new().unwrap();
    let shared_budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let batch_one = WriteQueueTestOverrides {
        batch_size: Some(1),
        ..Default::default()
    };
    let (_index_a, tx_a, handle_a, tasks_a) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        "yield_metric_a",
        Arc::clone(&shared_budget),
        batch_one.clone(),
    );
    let (_index_b, tx_b, handle_b, tasks_b) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        "yield_metric_b",
        Arc::clone(&shared_budget),
        batch_one.clone(),
    );

    let initial_a = register_task(tasks_a.as_ref(), "yield_metric_a_initial", 1, 1);
    enqueue_write(
        &tx_a,
        initial_a.clone(),
        vec![WriteAction::Add(text_document(
            "yield_metric_a_doc_1",
            "name",
            "initial writer owner",
        ))],
    )
    .await;
    wait_for_task_success(tasks_a.as_ref(), &initial_a).await;

    let waiter_yield_count_before = write_queue_counter_value_with_labels(
        WRITE_QUEUE_WRITER_CLOSES_METRIC_NAME,
        &[("tenant", "yield_metric_a"), ("reason", "waiter_yield")],
    );
    let task_b = register_task(tasks_b.as_ref(), "yield_metric_b_task", 1, 1);
    enqueue_write(
        &tx_b,
        task_b.clone(),
        vec![WriteAction::Add(text_document(
            "yield_metric_b_doc",
            "name",
            "waiting writer",
        ))],
    )
    .await;
    tokio::time::timeout(WRITE_QUEUE_PROGRESS_TIMEOUT, async {
        while !shared_budget.has_writer_waiters() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("tenant B should register writer contention");

    let yielding_a = register_task(tasks_a.as_ref(), "yield_metric_a_yield", 2, 1);
    enqueue_write(
        &tx_a,
        yielding_a.clone(),
        vec![WriteAction::Add(text_document(
            "yield_metric_a_doc_2",
            "name",
            "yield boundary",
        ))],
    )
    .await;
    tokio::join!(
        wait_for_task_success(tasks_a.as_ref(), &yielding_a),
        wait_for_task_success(tasks_b.as_ref(), &task_b)
    );

    assert_eq!(
        write_queue_counter_value_with_labels(
            WRITE_QUEUE_WRITER_CLOSES_METRIC_NAME,
            &[("tenant", "yield_metric_a"), ("reason", "waiter_yield")],
        ) - waiter_yield_count_before,
        1,
        "a contention-driven writer release must be counted exactly once"
    );

    drop(tx_a);
    drop(tx_b);
    handle_a.await.unwrap().unwrap();
    handle_b.await.unwrap().unwrap();
    assert!(
        !write_queue_counter_has_tenant(WRITE_QUEUE_WRITER_CLOSES_METRIC_NAME, "yield_metric_a"),
        "retired contention metrics must not retain the waiter-yield tenant label"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn busy_tenant_yields_contended_writer_after_commit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let shared_budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let batch_one = WriteQueueTestOverrides {
        batch_size: Some(1),
        ..Default::default()
    };
    let (index_a, tx_a, handle_a, tasks_a) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        "busy_budget_a",
        Arc::clone(&shared_budget),
        batch_one.clone(),
    );
    let (index_b, tx_b, handle_b, tasks_b) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        "busy_budget_b",
        Arc::clone(&shared_budget),
        batch_one.clone(),
    );

    let initial_a = register_task(tasks_a.as_ref(), "busy_tenant_a_initial", 1, 1);
    enqueue_write(
        &tx_a,
        initial_a.clone(),
        vec![WriteAction::Add(text_document(
            "busy_a_initial",
            "name",
            "initial tenant A write",
        ))],
    )
    .await;
    wait_for_task_success(tasks_a.as_ref(), &initial_a).await;

    let task_b = register_task(tasks_b.as_ref(), "busy_tenant_b_task", 1, 1);
    enqueue_write(
        &tx_b,
        task_b.clone(),
        vec![WriteAction::Add(text_document(
            "busy_b_doc",
            "name",
            "tenant B progress",
        ))],
    )
    .await;
    tokio::time::timeout(WRITE_QUEUE_PROGRESS_TIMEOUT, async {
        while !shared_budget.has_writer_waiters() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("tenant B should register writer contention");

    let stop_writes = Arc::new(AtomicBool::new(false));
    let busy_producer = tokio::spawn(enqueue_writes_until_stopped(
        tx_a.clone(),
        Arc::clone(&tasks_a),
        Arc::clone(&stop_writes),
    ));
    tokio::time::timeout(WRITE_QUEUE_PROGRESS_TIMEOUT, async {
        while !task_succeeded(tasks_b.as_ref(), &task_b)
            || write_queue_counter_value(WRITE_QUEUE_COMMITS_METRIC_NAME, "busy_budget_a") < 2
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("tenant B should commit while tenant A keeps the queue continuously busy");

    stop_writes.store(true, Ordering::Release);
    let produced_task_ids = busy_producer.await.unwrap();
    assert!(
        !produced_task_ids.is_empty(),
        "tenant A should commit after tenant B begins waiting"
    );
    for task_id in &produced_task_ids {
        wait_for_task_success(tasks_a.as_ref(), task_id).await;
    }
    assert_eq!(indexed_document_count(index_b.as_ref()), 1);
    assert_eq!(
        indexed_document_count(index_a.as_ref()),
        produced_task_ids.len() + 1
    );

    drop(tx_a);
    drop(tx_b);
    handle_a.await.unwrap().unwrap();
    handle_b.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn contended_idle_queue_keeps_merge_owner_until_backlog_converges() {
    let tmp = tempfile::TempDir::new().unwrap();
    let shared_budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let tenant_a = format!("yield_merge_a_{}", uuid::Uuid::new_v4().simple());
    let tenant_b = format!("yield_merge_b_{}", uuid::Uuid::new_v4().simple());
    let tenant_path_a = tmp.path().join(&tenant_a);
    let tenant_path_b = tmp.path().join(&tenant_b);
    std::fs::create_dir_all(&tenant_path_a).unwrap();
    std::fs::create_dir_all(&tenant_path_b).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index_a = Arc::new(
        crate::index::Index::create_with_budget(
            &tenant_path_a,
            schema.clone(),
            Arc::clone(&shared_budget),
        )
        .unwrap(),
    );
    let index_b = Arc::new(
        crate::index::Index::create_with_budget(&tenant_path_b, schema, shared_budget).unwrap(),
    );
    let (tx_a, handle_a, tasks_a) = setup_write_queue_with_index_and_overrides(
        &tmp,
        &tenant_a,
        Arc::clone(&index_a),
        WriteQueueTestOverrides {
            batch_size: Some(1),
            min_merge_segments: Some(2),
            max_docs_before_merge: Some(1000),
            writer_idle_timeout: None,
            ..Default::default()
        },
    );
    let (tx_b, handle_b, tasks_b) =
        setup_write_queue_with_index(&tmp, &tenant_b, Arc::clone(&index_b));

    const TENANT_A_COMMITS: usize = 60;
    for i in 0..TENANT_A_COMMITS {
        let task_id = register_task(
            tasks_a.as_ref(),
            &format!("yield_merge_a_task_{i}"),
            i as i64 + 1,
            1,
        );
        enqueue_write(
            &tx_a,
            task_id.clone(),
            vec![WriteAction::Add(text_document(
                &format!("doc_a_{i}"),
                "name",
                "tenant A merge backlog",
            ))],
        )
        .await;
        wait_for_task_success(tasks_a.as_ref(), &task_id).await;
    }

    let task_b = register_task(tasks_b.as_ref(), "yield_merge_b_task", 1, 1);
    enqueue_write(
        &tx_b,
        task_b.clone(),
        vec![WriteAction::Add(text_document(
            "doc_b_1",
            "name",
            "tenant B progress",
        ))],
    )
    .await;

    let observation = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if task_succeeded(tasks_b.as_ref(), &task_b) {
                let observation = observed_segments(index_a.as_ref());
                if observation.live_docs == TENANT_A_COMMITS as u64
                    && observation.live_segment_count < TENANT_A_COMMITS / 2
                    && observation.orphan_file_set_ids.is_empty()
                {
                    return observation;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("tenant A merges should converge before yielding its writer to tenant B");

    assert_eq!(
        indexed_document_count(index_b.as_ref()),
        1,
        "tenant B document should become searchable while tenant A queue stays open"
    );
    assert!(
        observation.orphan_file_set_ids.is_empty(),
        "tenant A merge cleanup should survive cross-tenant contention; got {observation:?}"
    );

    drop(tx_a);
    drop(tx_b);
    handle_a.await.unwrap().unwrap();
    handle_b.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn retired_tenant_metric_labels_do_not_accumulate() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_prefix = format!("metric_churn_{}", uuid::Uuid::new_v4().simple());

    for tenant_number in 0..3 {
        let tenant_id = format!("{tenant_prefix}_{tenant_number}");
        let (tx, handle, tasks) = setup_write_queue(&tmp, &tenant_id);

        let task_id = register_task(
            tasks.as_ref(),
            &format!("metric_churn_task_{tenant_number}"),
            tenant_number + 1,
            1,
        );
        enqueue_write(
            &tx,
            task_id.clone(),
            vec![WriteAction::Add(text_document(
                &format!("metric_doc_{tenant_number}"),
                "name",
                "metric churn",
            ))],
        )
        .await;
        wait_for_task_success(tasks.as_ref(), &task_id).await;

        assert!(
            write_queue_counter_has_tenant(WRITE_QUEUE_WRITER_OPENS_METRIC_NAME, &tenant_id),
            "active tenant should expose its writer-open counter"
        );
        assert!(
            write_queue_counter_has_tenant(WRITE_QUEUE_COMMITS_METRIC_NAME, &tenant_id),
            "active tenant should expose its commit counter"
        );

        drop(tx);
        handle.await.unwrap().unwrap();

        assert!(
            !write_queue_counter_has_tenant(WRITE_QUEUE_WRITER_OPENS_METRIC_NAME, &tenant_id),
            "retired tenant writer-open label should be removed"
        );
        assert!(
            !write_queue_counter_has_tenant(WRITE_QUEUE_COMMITS_METRIC_NAME, &tenant_id),
            "retired tenant commit label should be removed"
        );
        assert!(
            !write_queue_counter_has_tenant(WRITE_QUEUE_WRITER_LIFETIME_METRIC_NAME, &tenant_id),
            "retired tenant writer-lifetime label should be removed"
        );
        assert!(
            !write_queue_counter_has_tenant(WRITE_QUEUE_WRITER_MERGE_WAIT_METRIC_NAME, &tenant_id),
            "retired tenant merge-wait label should be removed"
        );
    }
}

#[test]
fn stale_metric_guard_does_not_remove_recreated_queue_labels() {
    let tenant_id = format!("metric_recreate_{}", uuid::Uuid::new_v4().simple());
    let old_metrics = writer_lifecycle::WriteQueueTenantMetrics::for_queue(&tenant_id);
    observe_write_queue_writer_opened(&tenant_id);
    observe_write_queue_commit_succeeded(&tenant_id);
    let new_metrics = writer_lifecycle::WriteQueueTenantMetrics::for_queue(&tenant_id);
    observe_write_queue_writer_opened(&tenant_id);
    observe_write_queue_commit_succeeded(&tenant_id);

    drop(old_metrics);

    assert!(
        write_queue_counter_has_tenant(WRITE_QUEUE_WRITER_OPENS_METRIC_NAME, &tenant_id),
        "old queue teardown must not unregister a replacement queue's writer-open label"
    );
    assert!(
        write_queue_counter_has_tenant(WRITE_QUEUE_COMMITS_METRIC_NAME, &tenant_id),
        "old queue teardown must not unregister a replacement queue's commit label"
    );

    drop(new_metrics);
}

#[test]
fn replacement_queue_drop_preserves_older_writer_open_state() {
    let tenant_id = format!("writer_open_owner_{}", uuid::Uuid::new_v4().simple());
    let old_metrics = writer_lifecycle::WriteQueueTenantMetrics::for_queue(&tenant_id);
    writer_lifecycle::record_writer_open_state_for_test(&tenant_id, old_metrics.queue_metrics_id());
    let replacement_metrics = writer_lifecycle::WriteQueueTenantMetrics::for_queue(&tenant_id);

    drop(replacement_metrics);

    assert_eq!(
        writer_lifecycle::writer_open_queue_metrics_id_for_test(&tenant_id),
        Some(old_metrics.queue_metrics_id()),
        "replacement queue teardown must not remove an older live writer's open state"
    );
    assert!(
        writer_lifecycle::remove_writer_open_state_for_test(
            &tenant_id,
            old_metrics.queue_metrics_id()
        ),
        "the older writer close path must still own and remove its open state"
    );

    drop(old_metrics);
}

#[test]
fn newest_replacement_drop_keeps_older_queue_metric_labels_registered() {
    let tenant_id = format!("metric_live_old_{}", uuid::Uuid::new_v4().simple());
    let older_metrics = writer_lifecycle::WriteQueueTenantMetrics::for_queue(&tenant_id);
    observe_write_queue_writer_opened(&tenant_id);
    observe_write_queue_commit_succeeded(&tenant_id);
    observe_write_queue_writer_lifetime(&tenant_id, Duration::from_millis(1));
    observe_write_queue_writer_merge_wait(&tenant_id, "channel_closed", Duration::from_millis(1));
    observe_write_queue_writer_closed(&tenant_id, "channel_closed");
    observe_write_queue_gc_removed_files(&tenant_id, 1);
    let observation = segment_observation::SegmentObservation {
        live_segment_ids: BTreeSet::from(["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()]),
        live_segment_count: 1,
        live_docs: 3,
        per_segment_doc_counts: BTreeMap::from([(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            3,
        )]),
        managed_index_file_count: 5,
        index_bytes: 11,
        orphan_file_set_ids: BTreeSet::from(["cccccccccccccccccccccccccccccccc".to_string()]),
    };
    observe_write_queue_segment_health(&tenant_id, &observation);
    observe_write_queue_settled_index_bytes(&tenant_id, &observation);

    let replacement_metrics = writer_lifecycle::WriteQueueTenantMetrics::for_queue(&tenant_id);
    drop(replacement_metrics);

    for metric_name in [
        WRITE_QUEUE_WRITER_OPENS_METRIC_NAME,
        WRITE_QUEUE_COMMITS_METRIC_NAME,
        WRITE_QUEUE_WRITER_CLOSES_METRIC_NAME,
        WRITE_QUEUE_LIVE_SEGMENTS_METRIC_NAME,
        WRITE_QUEUE_LIVE_DOCS_METRIC_NAME,
        WRITE_QUEUE_DOCUMENTS_PER_SEGMENT_METRIC_NAME,
        WRITE_QUEUE_INDEX_FILES_METRIC_NAME,
        WRITE_QUEUE_INDEX_BYTES_METRIC_NAME,
        WRITE_QUEUE_ORPHAN_FILE_SETS_METRIC_NAME,
        WRITE_QUEUE_WRITER_LIFETIME_METRIC_NAME,
        WRITE_QUEUE_WRITER_MERGE_WAIT_METRIC_NAME,
        WRITE_QUEUE_GC_REMOVED_FILES_METRIC_NAME,
        WRITE_QUEUE_SETTLED_INDEX_BYTES_METRIC_NAME,
    ] {
        assert!(
            write_queue_counter_has_tenant(metric_name, &tenant_id),
            "newest queue teardown must not retire {metric_name} while an older queue remains live"
        );
    }

    drop(older_metrics);
    assert!(
        !write_queue_counter_has_tenant(WRITE_QUEUE_WRITER_OPENS_METRIC_NAME, &tenant_id),
        "final queue teardown must retire tenant metric labels"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_acquire_writer_for_queue_returns_writer_contention_error_not_queue_full() {
    let tmp = tempfile::TempDir::new().unwrap();
    let shared_budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let tenant_path = tmp.path().join("writer_contention_tenant");
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(
        crate::index::Index::create_with_budget(&tenant_path, schema, shared_budget).unwrap(),
    );
    let _held_writer = index.writer().unwrap();

    let acquire = tokio::spawn({
        let index = Arc::clone(&index);
        async move {
            acquire_writer_for_queue(
                &index,
                "writer_contention_tenant",
                crate::index::Index::DEFAULT_BUFFER_SIZE,
            )
            .await
        }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(31)).await;

    let result = acquire
        .await
        .expect("writer acquisition task must not panic");
    let Err(error) = result else {
        panic!("held writer slot must exhaust writer acquisition retries");
    };
    assert!(
        matches!(
            error,
            FlapjackError::TooManyConcurrentWrites { current: _, max: 1 }
        ),
        "writer contention must surface TooManyConcurrentWrites, never QueueFull; got {error:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_write_queue_absorbs_1500_op_burst_without_queue_full() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _env_lock = WRITE_QUEUE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _capacity_guard =
        WriteQueueEnvVarRestoreGuard::apply(WRITE_QUEUE_CHANNEL_CAPACITY_ENV_VAR, Some("1500"));
    let (tx, handle, tasks, worker_gate) = setup_gated_write_queue(&tmp, "burst_tenant");

    // The dedicated writer worker runs on its own OS thread, so this gate is
    // the deterministic synchronization point that proves admission capacity
    // before any consumer-side draining can mask a regression.
    const REQUIRED_BURST_OPS: usize = 1_200;
    let mut burst_task_ids = Vec::with_capacity(REQUIRED_BURST_OPS);
    let mut first_rejected_op = None;
    for i in 0..REQUIRED_BURST_OPS {
        let task_id = register_task(tasks.as_ref(), &format!("burst_task_{i}"), i as i64 + 2, 1);
        let send_result = tx.try_send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Delete(format!("burst_missing_doc_{i}"))],
        });
        if send_result.is_err() {
            first_rejected_op = Some(i);
            break;
        }
        burst_task_ids.push(task_id);
    }

    worker_gate.release();
    drop(tx);
    handle.await.unwrap().unwrap();

    assert_eq!(
        first_rejected_op, None,
        "queue filled too early at burst op {}; expected to admit at least {REQUIRED_BURST_OPS} ops",
        first_rejected_op.unwrap_or(REQUIRED_BURST_OPS)
    );

    for task_id in burst_task_ids {
        wait_for_task_success(tasks.as_ref(), &task_id).await;
        assert_task_succeeded(tasks.as_ref(), &task_id, 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_write_queue_close_flush_commits_once() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_path = tmp.path().join("batch_commit_tenant");
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    let (tx, handle, tasks) =
        setup_write_queue_with_index(&tmp, "batch_commit_tenant", Arc::clone(&index));

    // This pushes a sub-threshold batch (10 ops < WRITE_QUEUE_BATCH_SIZE = 32)
    // and relies on channel-close flush to drain it: the regression is that the
    // close-triggered flush still produces a single searchable commit, not one
    // segment per queued op.
    for batch_number in 0..10 {
        let task_id = register_task(
            tasks.as_ref(),
            &format!("batch_commit_task_{batch_number}"),
            batch_number + 1,
            1,
        );
        enqueue_write(
            &tx,
            task_id,
            vec![WriteAction::Add(text_document(
                &format!("doc_{batch_number}"),
                "name",
                "batched",
            ))],
        )
        .await;
    }

    drop(tx);
    handle.await.unwrap().unwrap();

    index.reader().reload().unwrap();
    assert_eq!(
        indexed_document_count(index.as_ref()),
        10,
        "all queued writes should still be committed"
    );
    assert_eq!(
        searchable_segment_count(index.as_ref()),
        1,
        "a channel-close flush of a sub-threshold batch should commit once instead of producing one segment per queued op"
    );
}

#[test]
fn injected_commit_delay_applies_only_to_the_registered_tenant() {
    let stalled_tenant = "commit_delay_seam_stalled";
    let bystander_tenant = "commit_delay_seam_bystander";
    let injected_delay = Duration::from_millis(250);
    assert_eq!(
        write_queue_test_commit_delay(stalled_tenant),
        None,
        "no commit stall should be injected before a test registers one"
    );

    {
        let _stall = delay_commits_for_test(stalled_tenant, injected_delay);
        assert_eq!(
            write_queue_test_commit_delay(stalled_tenant),
            Some(injected_delay),
            "the registering tenant's commits must observe the injected stall"
        );
        assert_eq!(
            write_queue_test_commit_delay(bystander_tenant),
            None,
            "an injected commit stall must not reach write queues of tenants owned by other tests in the shared lib-test process"
        );
    }

    assert_eq!(
        write_queue_test_commit_delay(stalled_tenant),
        None,
        "dropping the guard must clear the injected stall"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_drains_acknowledged_writes_and_waits_for_merges() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = format!("shutdown_drain_{}", uuid::Uuid::new_v4().simple());
    let tenant_path = tmp.path().join(&tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig::default()));
    let active_writers_before = budget.active_writers();
    let index = Arc::new(
        crate::index::Index::create_with_budget(&tenant_path, schema, Arc::clone(&budget)).unwrap(),
    );
    let (tx, handle, tasks) = setup_write_queue_with_index(&tmp, &tenant_id, Arc::clone(&index));
    let metric_observation_guard = writer_lifecycle::WriteQueueTenantMetrics::for_queue(&tenant_id);
    let merge_wait_before = writer_merge_wait_count(&tenant_id, "channel_closed");
    let additions = [
        ("shutdown_add_one", "shutdown_doc_one"),
        ("shutdown_add_two", "shutdown_doc_two"),
    ];
    let mut task_ids = Vec::new();

    for (batch_number, (task_suffix, document_id)) in additions.iter().enumerate() {
        let task_id = register_task(
            tasks.as_ref(),
            &format!("{tenant_id}_{task_suffix}"),
            batch_number as i64 + 1,
            1,
        );
        enqueue_write(
            &tx,
            task_id.clone(),
            vec![WriteAction::Add(text_document(
                document_id,
                "name",
                "shutdown drain",
            ))],
        )
        .await;
        task_ids.push(task_id);
    }
    let delete_task = register_task(tasks.as_ref(), &format!("{tenant_id}_delete"), 3, 1);
    enqueue_write(
        &tx,
        delete_task.clone(),
        vec![WriteAction::Delete("shutdown_doc_one".to_string())],
    )
    .await;
    task_ids.push(delete_task);

    drop(tx);
    handle.await.unwrap().unwrap();

    let observation = observed_segments(index.as_ref());
    assert_eq!(
        observation.live_docs, 1,
        "two acknowledged adds minus one acknowledged delete must leave one live document"
    );
    for task_id in &task_ids {
        assert!(
            tasks.get(task_id).is_some_and(|task| {
                matches!(task.status, TaskStatus::Succeeded | TaskStatus::Failed(_))
            }),
            "shutdown must leave task {task_id} in a terminal state"
        );
    }
    assert_eq!(
        writer_merge_wait_count(&tenant_id, "channel_closed") - merge_wait_before,
        1,
        "shutdown must wait for merge quiescence exactly once before closing its writer"
    );
    assert_eq!(
        budget.active_writers(),
        active_writers_before,
        "shutdown must return the active-writer budget to its pre-queue baseline"
    );
    drop(metric_observation_guard);
}

/// Regression gate for PL-10v2 commit-amortization tuning.
///
/// Pushing 63 ops faster than the 100 ms flush deadline through a single queue
/// must coalesce into ≤ 2 Tantivy commits (and thus ≤ 2 searchable segments).
/// With the legacy `WRITE_QUEUE_BATCH_SIZE = 10`, the same workload would
/// produce 7 size-triggered batches plus 1 close-triggered batch (≥ 7
/// segments), surfacing the multi_phase commit-pipeline saturation observed in
/// `docs/reference/research/pl10_write_bottleneck_20260528T033040Z_classification.md`
/// (commit_writer_with_panic_guard at 30.37% of total phase seconds, nested
/// inside commit_batch at 33.54%).
#[tokio::test(flavor = "current_thread")]
async fn test_write_queue_amortizes_commits_under_fast_push() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_path = tmp.path().join("amortization_tenant");
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    let (tx, handle, tasks, worker_gate) =
        setup_gated_write_queue_with_index(&tmp, "amortization_tenant", Arc::clone(&index));

    // The dedicated writer worker is held until all producers finish, so
    // timeout-driven draining cannot interleave with this burst.
    //
    // 63 sits below Tantivy's LogMergePolicy min_merge threshold so per-batch
    // segments stay observable post-drain.
    const FAST_PUSH_OPS: usize = 63;
    for i in 0..FAST_PUSH_OPS {
        let task_id = register_task(
            tasks.as_ref(),
            &format!("amortization_task_{i}"),
            i as i64 + 1,
            1,
        );
        enqueue_write_without_draining_burst(
            &tx,
            task_id,
            vec![WriteAction::Add(text_document(
                &format!("doc_{i}"),
                "name",
                "amortization",
            ))],
        );
    }

    worker_gate.release();
    drop(tx);
    handle.await.unwrap().unwrap();

    index.reader().reload().unwrap();
    assert_eq!(
        indexed_document_count(index.as_ref()),
        FAST_PUSH_OPS,
        "every queued document should still be committed and searchable"
    );

    let segments = searchable_segment_count(index.as_ref());
    assert!(
        segments <= 2,
        "expected ≤ 2 segments after {FAST_PUSH_OPS} fast-pushed ops to amortize Tantivy commit cost; got {segments}"
    );
}

#[tokio::test]
async fn test_batch_settings_load_failure_marks_all_tasks_failed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "invalid_settings_tenant";
    let tenant_path = tmp.path().join(tenant_id);
    let (tx, handle, tasks) = setup_write_queue(&tmp, tenant_id);

    std::fs::write(tenant_path.join("settings.json"), "{ invalid json").unwrap();

    let task_1 = register_task(tasks.as_ref(), "invalid_settings_task_1", 1, 1);
    let task_2 = register_task(tasks.as_ref(), "invalid_settings_task_2", 2, 1);
    enqueue_write(
        &tx,
        task_1.clone(),
        vec![WriteAction::Add(text_document("doc1", "name", "Alice"))],
    )
    .await;
    enqueue_write(
        &tx,
        task_2.clone(),
        vec![WriteAction::Add(text_document("doc2", "name", "Bob"))],
    )
    .await;

    drop(tx);
    let queue_result = handle.await.unwrap();
    assert!(
        queue_result.is_err(),
        "invalid tenant settings should fail the batch flush"
    );

    assert_task_failed(tasks.as_ref(), &task_1);
    assert_task_failed(tasks.as_ref(), &task_2);
}

#[tokio::test]
async fn delete_term_probe_counts_upsert_but_not_add() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (tx, handle, tasks) = setup_write_queue(&tmp, "delete_term_probe_tenant");

    let upsert_task = register_task(tasks.as_ref(), "delete_term_probe_upsert", 1, 3);
    enqueue_write(
        &tx,
        upsert_task.clone(),
        vec![
            WriteAction::Upsert(text_document("upsert_1", "name", "One")),
            WriteAction::Upsert(text_document("upsert_2", "name", "Two")),
            WriteAction::Upsert(text_document("upsert_3", "name", "Three")),
        ],
    )
    .await;
    wait_for_task_success(tasks.as_ref(), &upsert_task).await;
    assert_eq!(
        delete_term_observation(&tasks.get(&upsert_task).unwrap()),
        DeleteTermObservation {
            explicit_delete_actions: 0,
            document_write_delete_terms: 3,
        }
    );

    let add_task = register_task(tasks.as_ref(), "delete_term_probe_add", 2, 3);
    enqueue_write(
        &tx,
        add_task.clone(),
        vec![
            WriteAction::Add(text_document("add_1", "name", "One")),
            WriteAction::Add(text_document("add_2", "name", "Two")),
            WriteAction::Add(text_document("add_3", "name", "Three")),
        ],
    )
    .await;
    wait_for_task_success(tasks.as_ref(), &add_task).await;
    assert_eq!(
        delete_term_observation(&tasks.get(&add_task).unwrap()),
        DeleteTermObservation {
            explicit_delete_actions: 0,
            document_write_delete_terms: 0,
        },
        "add-mode staging must not inherit an earlier task's delete-term count"
    );

    let delete_task = register_task(tasks.as_ref(), "delete_term_probe_delete", 3, 1);
    enqueue_write(
        &tx,
        delete_task.clone(),
        vec![WriteAction::Delete("upsert_1".to_string())],
    )
    .await;
    drop(tx);
    handle.await.unwrap().unwrap();
    assert_eq!(
        delete_term_observation(&tasks.get(&delete_task).unwrap()),
        DeleteTermObservation {
            explicit_delete_actions: 1,
            document_write_delete_terms: 0,
        },
        "explicit deletes must not pollute the document-write delete-term count"
    );
}

#[tokio::test]
async fn test_commit_batch_basic_add() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (tx, handle, tasks) = setup_write_queue(&tmp, "test_tenant");

    let task_id = register_task(tasks.as_ref(), "test_task_1", 1, 2);

    let doc1 = text_document("doc1", "name", "Alice");
    let doc2 = text_document("doc2", "name", "Bob");

    enqueue_write(
        &tx,
        task_id.clone(),
        vec![WriteAction::Add(doc1), WriteAction::Add(doc2)],
    )
    .await;

    drop(tx);
    handle.await.unwrap().unwrap();

    assert_task_succeeded(tasks.as_ref(), &task_id, 2);
}

#[tokio::test]
async fn test_commit_batch_upsert() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (tx, handle, tasks) = setup_write_queue(&tmp, "upsert_tenant");

    // Add a document first
    let task_id_1 = register_task(tasks.as_ref(), "upsert_task_1", 1, 1);
    let doc = text_document("doc1", "name", "Alice");
    enqueue_write(&tx, task_id_1.clone(), vec![WriteAction::Add(doc)]).await;

    // Give the write queue time to process
    wait_for_write_queue_settle().await;

    // Upsert the same doc with updated content
    let task_id_2 = register_task(tasks.as_ref(), "upsert_task_2", 2, 1);
    let doc_updated = text_document("doc1", "name", "Alice Updated");
    enqueue_write(
        &tx,
        task_id_2.clone(),
        vec![WriteAction::Upsert(doc_updated)],
    )
    .await;

    drop(tx);
    handle.await.unwrap().unwrap();

    assert_task_succeeded(tasks.as_ref(), &task_id_2, 1);
}

#[tokio::test]
async fn test_commit_batch_delete() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (tx, handle, tasks) = setup_write_queue(&tmp, "delete_tenant");

    // Add a document first
    let task_id_1 = register_task(tasks.as_ref(), "del_task_1", 1, 1);
    let doc = text_document("doc1", "name", "Alice");
    enqueue_write(&tx, task_id_1.clone(), vec![WriteAction::Add(doc)]).await;

    // Give the write queue time to process
    wait_for_write_queue_settle().await;

    // Delete the doc
    let task_id_2 = register_task(tasks.as_ref(), "del_task_2", 2, 1);
    enqueue_write(
        &tx,
        task_id_2.clone(),
        vec![WriteAction::Delete("doc1".to_string())],
    )
    .await;

    drop(tx);
    handle.await.unwrap().unwrap();

    // Delete counts as 1 indexed document (it's a successful write operation)
    assert_task_succeeded(tasks.as_ref(), &task_id_2, 1);
}

#[tokio::test]
async fn test_write_queue_phase_metrics_records_batch_lifecycle_series() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (tx, handle, tasks) = setup_write_queue(&tmp, "phase_metrics_tenant");

    let task_id = register_task(tasks.as_ref(), "phase_metrics_task_1", 1, 1);
    enqueue_write(
        &tx,
        task_id.clone(),
        vec![WriteAction::Add(text_document(
            "doc1",
            "name",
            "Phase Metric",
        ))],
    )
    .await;

    drop(tx);
    handle.await.unwrap().unwrap();
    assert_task_succeeded(tasks.as_ref(), &task_id, 1);

    let metrics_text = write_queue_phase_metrics_text();
    for phase in [
        "process_writes",
        "flush_pending_batch",
        "commit_batch",
        "commit_writer_with_panic_guard",
        "finalize_committed_batch",
    ] {
        assert!(
            metrics_text.lines().any(|line| {
                line.starts_with("flapjack_write_queue_phase_seconds_count")
                    && line.contains(&format!("phase=\"{phase}\""))
            }),
            "expected phase histogram sample for {phase}, got:\n{metrics_text}"
        );
    }
}

// Serialized against `oplog_append_phase_ignores_noop_paths`: both read/write the
// process-global `oplog_append` phase histogram, and this test is the only default-run
// producer of a real append, so the no-op test's exact-equality snapshot must not
// overlap this test's appends.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(oplog_append_phase_metric)]
async fn write_phase_metrics_separate_prepare_commit_reload_versions_and_oplog() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = format!("phase_detail_{}", uuid::Uuid::new_v4().simple());
    let (tx, handle, tasks, oplog) = setup_write_queue_with_oplog(&tmp, &tenant_id);
    let baseline_oplog_append_count = histogram_count(
        "flapjack_write_queue_phase_seconds",
        &[("phase", "oplog_append")],
    );

    let add_task = register_task(tasks.as_ref(), "phase_detail_add", 1, 1);
    enqueue_write(
        &tx,
        add_task.clone(),
        vec![WriteAction::Upsert(text_document(
            "phase_doc",
            "name",
            "Phase Detail",
        ))],
    )
    .await;
    wait_for_task_success(tasks.as_ref(), &add_task).await;

    let delete_task = register_task(tasks.as_ref(), "phase_detail_delete", 2, 1);
    enqueue_write(
        &tx,
        delete_task.clone(),
        vec![WriteAction::Delete("phase_doc".to_string())],
    )
    .await;
    wait_for_task_success(tasks.as_ref(), &delete_task).await;

    drop(tx);
    handle.await.unwrap().unwrap();
    assert_eq!(
        oplog.current_seq(),
        2,
        "the specimen must write one upsert and one delete to the real oplog"
    );

    let metrics_text = write_queue_phase_metrics_text();
    for phase in [
        "document_conversion",
        "delete_staging",
        "add_staging",
        "writer_commit",
        "reader_reload",
        "metadata_persistence",
        "version_store_update",
        "oplog_commit_state_persistence",
    ] {
        assert_histogram_count_at_least(
            &metrics_text,
            "flapjack_write_queue_phase_seconds",
            &[("phase", phase)],
            1,
        );
    }
    assert_histogram_count_at_least(
        &metrics_text,
        "flapjack_write_queue_phase_seconds",
        &[("phase", "oplog_append")],
        baseline_oplog_append_count + 2,
    );
}

#[test]
fn add_staging_phase_is_recorded_by_tantivy_write_not_preparation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_path = tmp.path().join("add_staging_owner");
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    let mut writer = index.writer().unwrap();
    let id_field = index.inner().schema().get_field("_id").unwrap();
    let mut prepared = PreparedWriteOperation::new("task_add_staging".into(), "1".into());

    let mut preparation_context = WritePreparationContext {
        index: &index,
        settings: None,
        writer: &mut writer,
        id_field,
        #[cfg(feature = "vector-search")]
        embedder_configs: &[],
    };
    let (_, preparation_observations) =
        count_write_queue_phase_observations_for_test(PHASE_ADD_STAGING, || {
            prepare_document_write(
                &mut preparation_context,
                &mut prepared,
                text_document("doc_add_staging", "name", "Add Staging"),
                DocumentWriteMode::Add,
            );
        });
    assert_eq!(
        preparation_observations, 0,
        "preparation bookkeeping must not emit add_staging observations"
    );

    let (write_result, write_observations) =
        count_write_queue_phase_observations_for_test(PHASE_ADD_STAGING, || {
            finalization::write_valid_documents(&mut writer, &prepared.valid_docs)
        });
    write_result.unwrap();
    assert_eq!(
        write_observations, 1,
        "one Tantivy add_document call must emit exactly one add_staging observation"
    );
}

#[test]
fn legacy_replicated_actions_replay_without_inventing_oplog_origin() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_path = tmp.path().join("legacy_unproven_origin");
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    let mut writer = index.writer().unwrap();
    let id_field = index.inner().schema().get_field("_id").unwrap();
    let mut prepared = PreparedWriteOperation::new("legacy_task".into(), "1".into());
    let mut preparation_context = WritePreparationContext {
        index: &index,
        settings: None,
        writer: &mut writer,
        id_field,
        #[cfg(feature = "vector-search")]
        embedder_configs: &[],
    };

    prepare_write_actions(
        &mut preparation_context,
        &mut prepared,
        vec![
            WriteAction::UpsertNoLwwUpdate(text_document("legacy_upsert", "name", "Legacy Upsert")),
            WriteAction::DeleteNoLwwUpdate("legacy_delete".to_string()),
        ],
    )
    .unwrap();

    assert_eq!(prepared.valid_docs.len(), 1);
    assert_eq!(prepared.deleted_ids, vec!["legacy_delete"]);
    assert!(
        prepared.oplog_ops.is_empty(),
        "legacy records must replay Tantivy mutations without publishing fabricated origin tuples"
    );
}

// Serialized with the real-append test above so the exact-equality snapshot below is
// not perturbed by a concurrent producer of the global `oplog_append` phase histogram.
#[test]
#[serial_test::serial(oplog_append_phase_metric)]
fn oplog_append_phase_ignores_noop_paths() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "oplog_noop_phase";
    let tenant_path = tmp.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let oplog = Arc::new(
        crate::index::oplog::OpLog::open(&tenant_path.join("oplog"), tenant_id, "test_node")
            .unwrap(),
    );
    let baseline = histogram_count(
        "flapjack_write_queue_phase_seconds",
        &[("phase", "oplog_append")],
    );

    finalization::append_batch_to_oplog(None, "task_none", &[], tenant_id).unwrap();
    finalization::append_batch_to_oplog(Some(&oplog), "task_empty", &[], tenant_id).unwrap();

    assert_eq!(
        histogram_count(
            "flapjack_write_queue_phase_seconds",
            &[("phase", "oplog_append")]
        ),
        baseline,
        "oplog append latency should only record attempted appends"
    );
    assert_eq!(oplog.current_seq(), 0, "empty append path should not write");
}

#[tokio::test(flavor = "current_thread")]
async fn writer_lifetime_metrics_survive_multiple_commits() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = format!("writer_lifetime_{}", uuid::Uuid::new_v4().simple());
    let waiter_tenant_id = format!("writer_lifetime_waiter_{}", uuid::Uuid::new_v4().simple());
    let shared_budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let (_index, tx, handle, tasks) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        &tenant_id,
        Arc::clone(&shared_budget),
        WriteQueueTestOverrides {
            batch_size: Some(1),
            min_merge_segments: Some(2),
            max_docs_before_merge: Some(1000),
            writer_idle_timeout: None,
            ..Default::default()
        },
    );
    let (_waiter_index, waiter_tx, waiter_handle, waiter_tasks) =
        setup_write_queue_with_budget(&tmp, &waiter_tenant_id, Arc::clone(&shared_budget));

    const COMMIT_COUNT: usize = 2;
    for i in 0..COMMIT_COUNT {
        let task_id = register_task(
            tasks.as_ref(),
            &format!("writer_lifetime_task_{i}"),
            i as i64 + 1,
            1,
        );
        enqueue_write(
            &tx,
            task_id.clone(),
            vec![WriteAction::Add(text_document(
                &format!("writer_lifetime_doc_{i}"),
                "name",
                "Writer Lifetime",
            ))],
        )
        .await;
        wait_for_task_success(tasks.as_ref(), &task_id).await;
    }

    let waiter_task = register_task(waiter_tasks.as_ref(), "writer_lifetime_waiter_task", 1, 1);
    enqueue_write(
        &waiter_tx,
        waiter_task.clone(),
        vec![WriteAction::Add(text_document(
            "writer_lifetime_waiter_doc",
            "name",
            "Writer Lifetime Waiter",
        ))],
    )
    .await;
    tokio::time::timeout(WRITE_QUEUE_PROGRESS_TIMEOUT, async {
        while !shared_budget.has_writer_waiters() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("waiter tenant should contend for the persistent writer slot");
    wait_for_task_success(waiter_tasks.as_ref(), &waiter_task).await;

    assert_eq!(
        write_queue_counter_value(WRITE_QUEUE_WRITER_OPENS_METRIC_NAME, &tenant_id),
        1,
        "persistent writer should open exactly once for the tenant"
    );
    assert!(
        write_queue_counter_value(WRITE_QUEUE_COMMITS_METRIC_NAME, &tenant_id)
            >= COMMIT_COUNT as u64,
        "persistent writer should record at least {COMMIT_COUNT} commits"
    );

    let metrics_text = write_queue_phase_metrics_text();
    assert_histogram_count_at_least(
        &metrics_text,
        "flapjack_write_queue_writer_lifetime_seconds",
        &[("tenant", &tenant_id)],
        1,
    );
    assert!(
        write_queue_metric_value(
            "flapjack_write_queue_writer_lifetime_seconds_sum",
            &[("tenant", &tenant_id)]
        )
        .unwrap_or(0.0)
            >= 0.02,
        "writer lifetime should span the multi-commit window; metrics:\n{metrics_text}"
    );
    assert_histogram_count_at_least(
        &metrics_text,
        "flapjack_write_queue_writer_merge_wait_seconds",
        &[("tenant", &tenant_id), ("reason", "waiter_yield")],
        1,
    );

    drop(tx);
    drop(waiter_tx);
    handle.await.unwrap().unwrap();
    waiter_handle.await.unwrap().unwrap();
}

#[test]
fn startup_replay_closes_measured_writer_lifecycle() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = format!("startup_replay_{}", uuid::Uuid::new_v4().simple());
    let tenant_path = tmp.path().join(&tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    let tasks: Arc<dashmap::DashMap<String, TaskInfo>> = Arc::new(dashmap::DashMap::new());
    let admission_store =
        Arc::new(admission::WriteAdmissionStore::open(tmp.path(), &tenant_id).unwrap());
    let task_id = "startup_replay_task".to_string();
    let record = admission_store
        .append_record(admission::WriteAdmissionRecord::new(
            admission::WriteAdmissionTicket::new(
                tenant_id.clone(),
                crate::index::manager::publication::PublicationEpoch(0),
            ),
            task_id.clone(),
            1,
            1,
            vec![WriteAction::Add(text_document(
                "startup_replay_doc",
                "name",
                "replay lifecycle",
            ))],
        ))
        .unwrap();
    tasks.insert(task_id.clone(), record.task_info());
    tasks.insert(record.numeric_id.to_string(), record.task_info());
    let tenant_metrics = writer_lifecycle::WriteQueueTenantMetrics::for_queue(&tenant_id);
    let ctx = WriteQueueContext {
        tenant_id: tenant_id.clone(),
        index,
        tasks: Arc::clone(&tasks),
        base_path: tmp.path().to_path_buf(),
        oplog: None,
        admission_store,
        facet_cache: Arc::new(dashmap::DashMap::new()),
        #[cfg(feature = "vector-search")]
        vector_ctx: VectorWriteContext::new(Arc::new(dashmap::DashMap::new())),
        #[cfg(not(feature = "vector-search"))]
        vector_ctx: VectorWriteContext::new(),
        queue_metrics_id: tenant_metrics.queue_metrics_id(),
        writer_buffer_size: crate::index::Index::DEFAULT_BUFFER_SIZE,
        test_overrides: WriteQueueTestOverrides {
            batch_size: Some(1),
            ..Default::default()
        },
    };

    let (cancellation, _cancellation_rx) = write_queue_cancellation_channel();
    run_replay_startup(&ctx, vec![record], &cancellation).unwrap();

    let metrics_text = write_queue_phase_metrics_text();
    assert_eq!(
        write_queue_counter_value(WRITE_QUEUE_WRITER_OPENS_METRIC_NAME, &tenant_id),
        1,
        "replay should open exactly one measured writer"
    );
    assert_histogram_count_at_least(
        &metrics_text,
        WRITE_QUEUE_WRITER_LIFETIME_METRIC_NAME,
        &[("tenant", &tenant_id)],
        1,
    );
    assert_histogram_count_at_least(
        &metrics_text,
        WRITE_QUEUE_WRITER_MERGE_WAIT_METRIC_NAME,
        &[("tenant", &tenant_id), ("reason", "startup_replay")],
        1,
    );
    assert_eq!(
        write_queue_counter_value_with_labels(
            WRITE_QUEUE_WRITER_CLOSES_METRIC_NAME,
            &[("tenant", &tenant_id), ("reason", "startup_replay")]
        ),
        1,
        "replay writer close should use the lifecycle close-reason metric"
    );
}

/// Verify that `VectorWriteContext` shares the same `DashMap` instance via `Arc`, so mutations through the map are visible through the context.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_write_context_shares_dashmap() {
    // Verify that VectorWriteContext properly shares the same DashMap instance
    let vector_indices: Arc<
        dashmap::DashMap<String, Arc<std::sync::RwLock<crate::vector::index::VectorIndex>>>,
    > = Arc::new(dashmap::DashMap::new());

    let ctx = VectorWriteContext::new(Arc::clone(&vector_indices));

    // Insert into the shared DashMap
    let vi = crate::vector::index::VectorIndex::new(3, usearch::ffi::MetricKind::Cos).unwrap();
    vector_indices.insert(
        "test_tenant".to_string(),
        Arc::new(std::sync::RwLock::new(vi)),
    );

    // The context should see the same data (same Arc)
    assert!(ctx.vector_indices.contains_key("test_tenant"));
    assert_eq!(ctx.vector_indices.len(), 1);
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_create_write_queue_with_vector_indices() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "vec_tenant";
    let tenant_path = tmp.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());

    let tasks: Arc<dashmap::DashMap<String, TaskInfo>> = Arc::new(dashmap::DashMap::new());
    let facet_cache = Arc::new(dashmap::DashMap::new());
    let vector_indices: Arc<
        dashmap::DashMap<String, Arc<std::sync::RwLock<crate::vector::index::VectorIndex>>>,
    > = Arc::new(dashmap::DashMap::new());

    let vector_ctx = VectorWriteContext::new(vector_indices);
    let admission_store =
        Arc::new(admission::WriteAdmissionStore::open(tmp.path(), tenant_id).unwrap());

    let (tx, handle, _cancellation, _completion) = create_write_queue(WriteQueueContext {
        tenant_id: tenant_id.to_string(),
        index,
        tasks: Arc::clone(&tasks),
        base_path: tmp.path().to_path_buf(),
        oplog: None,
        admission_store,
        facet_cache,
        vector_ctx,
        queue_metrics_id: 0,
        writer_buffer_size: crate::index::Index::DEFAULT_BUFFER_SIZE,
        test_overrides: Default::default(),
    })
    .unwrap();

    let task_id = register_task(tasks.as_ref(), "vec_task_1", 1, 1);

    let doc = crate::types::Document {
        id: "doc1".to_string(),
        fields: HashMap::from([(
            "title".to_string(),
            crate::types::FieldValue::Text("Hello vectors".to_string()),
        )]),
    };

    enqueue_write(&tx, task_id.clone(), vec![WriteAction::Add(doc)]).await;

    drop(tx);
    handle.await.unwrap().unwrap();

    assert_task_succeeded(tasks.as_ref(), &task_id, 1);
}

// ── Auto-embedding integration tests (7.11) ──

#[cfg(feature = "vector-search")]
#[path = "write_queue_auto_embed_tests.rs"]
mod auto_embed_tests;
