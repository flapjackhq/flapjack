use super::*;
use crate::index::memory::{MemoryBudget, MemoryBudgetConfig};
use crate::index::rules::GeneratedFacetFilter;
use crate::index::write_queue::admission::{WriteAdmissionRecord, WriteAdmissionStore};
use crate::index::write_queue::WriteOp;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const WRITE_DURABLE_TIMEOUT_ENV_VAR: &str = "FLAPJACK_WRITE_DURABLE_TIMEOUT_MS";
const WRITE_QUEUE_WRITER_ACQUIRE_TIMEOUT_ENV_VAR: &str =
    "FLAPJACK_WRITE_QUEUE_WRITER_ACQUIRE_TIMEOUT_MS";
const WRITE_QUEUE_ADMISSION_FAILURE_PROBE_LIMIT: usize = 2_050;

struct DurableWriteTimeoutEnvGuard {
    previous_value: Option<String>,
}

impl DurableWriteTimeoutEnvGuard {
    fn set(value: &str) -> Self {
        let previous_value = std::env::var(WRITE_DURABLE_TIMEOUT_ENV_VAR).ok();
        std::env::set_var(WRITE_DURABLE_TIMEOUT_ENV_VAR, value);
        Self { previous_value }
    }
}

impl Drop for DurableWriteTimeoutEnvGuard {
    fn drop(&mut self) {
        match &self.previous_value {
            Some(value) => std::env::set_var(WRITE_DURABLE_TIMEOUT_ENV_VAR, value),
            None => std::env::remove_var(WRITE_DURABLE_TIMEOUT_ENV_VAR),
        }
    }
}

struct WriteQueueWriterAcquireTimeoutEnvGuard {
    previous_value: Option<String>,
}

impl WriteQueueWriterAcquireTimeoutEnvGuard {
    fn set(value: &str) -> Self {
        let previous_value = std::env::var(WRITE_QUEUE_WRITER_ACQUIRE_TIMEOUT_ENV_VAR).ok();
        std::env::set_var(WRITE_QUEUE_WRITER_ACQUIRE_TIMEOUT_ENV_VAR, value);
        Self { previous_value }
    }
}

impl Drop for WriteQueueWriterAcquireTimeoutEnvGuard {
    fn drop(&mut self) {
        match &self.previous_value {
            Some(value) => std::env::set_var(WRITE_QUEUE_WRITER_ACQUIRE_TIMEOUT_ENV_VAR, value),
            None => std::env::remove_var(WRITE_QUEUE_WRITER_ACQUIRE_TIMEOUT_ENV_VAR),
        }
    }
}

struct TraversalEscapeDirGuard {
    path: PathBuf,
}

impl TraversalEscapeDirGuard {
    fn new(tmp: &TempDir, label: &str) -> (Self, String) {
        let salt = tmp
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "tmp".to_string());
        let escape_name = format!("{label}_{salt}");
        let path = tmp.path().join("..").join(&escape_name);
        (Self { path }, format!("../{escape_name}"))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TraversalEscapeDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}

fn write_queue_test_document(object_id: &str, title: &str) -> Document {
    Document {
        id: object_id.to_string(),
        fields: HashMap::from([("title".to_string(), FieldValue::Text(title.to_string()))]),
    }
}

fn tenant_task_key_snapshot(manager: &IndexManager, tenant_id: &str) -> BTreeSet<String> {
    let prefix = format!("task_{}_", tenant_id);
    manager
        .tasks
        .iter()
        .filter(|entry| entry.value().id.starts_with(&prefix))
        .map(|entry| entry.key().clone())
        .collect()
}

fn tenant_task_snapshot_contains(manager: &IndexManager, tenant_id: &str, task_id: &str) -> bool {
    manager
        .tenant_tasks_snapshot_for_test(tenant_id)
        .iter()
        .any(|task| task.id == task_id)
}

fn tenant_admission_record_count(base_path: &Path, tenant_id: &str) -> usize {
    let admission_path = base_path.join(tenant_id).join("write_admission");
    if !admission_path.is_dir() {
        return 0;
    }
    std::fs::read_dir(admission_path)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count()
}

fn write_corrupt_complete_admission_record(base_path: &Path, tenant_id: &str) {
    let admission_dir = base_path.join(tenant_id).join("write_admission");
    std::fs::create_dir_all(&admission_dir).unwrap();
    std::fs::write(
        admission_dir.join("00000000000000000001.json"),
        b"{not-json",
    )
    .unwrap();
}

#[test]
fn write_admission_checksum_round_trips_nested_float_payloads() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_admission_float_checksum";
    std::fs::create_dir_all(temp_dir.path().join(tenant_id)).unwrap();
    let store = WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap();
    let documents: Vec<Document> = (0..400)
        .map(|doc_id| {
            let points = (0..128)
                .map(|offset| {
                    let jitter = offset as f64 * 0.00001;
                    FieldValue::Object(HashMap::from([
                        (
                            "lat".to_string(),
                            FieldValue::Float(40.7128 + doc_id as f64 * 0.0001 + jitter),
                        ),
                        (
                            "lng".to_string(),
                            FieldValue::Float(-74.0060 + doc_id as f64 * 0.0001 - jitter),
                        ),
                    ]))
                })
                .collect();
            Document {
                id: format!("doc{doc_id}"),
                fields: HashMap::from([
                    (
                        "title".to_string(),
                        FieldValue::Text("hello world".to_string()),
                    ),
                    ("_geoloc".to_string(), FieldValue::Array(points)),
                ]),
            }
        })
        .collect();

    store
        .append_record(WriteAdmissionRecord::new(
            "task_float_checksum".to_string(),
            1,
            400,
            vec![WriteAction::Upsert(Document {
                id: "batch".to_string(),
                fields: HashMap::from([(
                    "documents".to_string(),
                    FieldValue::Array(
                        documents
                            .into_iter()
                            .map(|document| FieldValue::Object(document.fields))
                            .collect(),
                    ),
                )]),
            })],
        ))
        .unwrap();

    let records = store.load_records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task_id, "task_float_checksum");
}

#[derive(Clone, Copy, Debug)]
enum AdmissionCapacityEntryPoint {
    Insert,
    Upsert,
    ReplicationAdd,
    Delete,
    ReplicationDelete,
    Compact,
}

impl AdmissionCapacityEntryPoint {
    fn admit(self, manager: &IndexManager, tenant_id: &str) -> Result<TaskInfo> {
        match self {
            Self::Insert => manager.add_documents_insert(
                tenant_id,
                vec![write_queue_test_document("insert_capacity_doc", "coverage")],
            ),
            Self::Upsert => manager.add_documents(
                tenant_id,
                vec![write_queue_test_document("upsert_capacity_doc", "coverage")],
            ),
            Self::ReplicationAdd => manager.add_documents_for_replication(
                tenant_id,
                vec![write_queue_test_document(
                    "replication_add_capacity_doc",
                    "coverage",
                )],
            ),
            Self::Delete => {
                manager.delete_documents(tenant_id, vec!["delete_capacity_missing_doc".to_string()])
            }
            Self::ReplicationDelete => manager.delete_documents_for_replication(
                tenant_id,
                vec!["replication_delete_capacity_missing_doc".to_string()],
            ),
            Self::Compact => manager.compact_index(tenant_id),
        }
    }
}

fn assert_queue_full_preserves_pre_admission_state(
    entry_point: AdmissionCapacityEntryPoint,
    temp_dir: &TempDir,
) {
    let tenant_id = "write_queue_capacity_table";
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();
    fill_write_queue_without_admission(&manager, tenant_id);
    let before_keys = tenant_task_key_snapshot(&manager, tenant_id);
    let before_records = tenant_admission_record_count(temp_dir.path(), tenant_id);
    let result = entry_point.admit(&manager, tenant_id);
    assert!(
        matches!(result, Err(FlapjackError::QueueFull)),
        "expected QueueFull for {entry_point:?}, got {result:?}"
    );
    let after_keys = tenant_task_key_snapshot(&manager, tenant_id);
    let after_records = tenant_admission_record_count(temp_dir.path(), tenant_id);
    assert!(
        manager.abort_tenant_write_task_for_test(tenant_id),
        "tenant write_queue task should still be abortable after capacity probe"
    );
    let leaked_keys: Vec<_> = after_keys
        .difference(&before_keys)
        .take(4)
        .cloned()
        .collect();
    assert!(
        leaked_keys.is_empty(),
        "{entry_point:?} QueueFull admission failure must not allocate a canonical task id or numeric alias; leaked task keys: {leaked_keys:?}"
    );
    assert_eq!(
        after_records, before_records,
        "{entry_point:?} QueueFull admission failure must leave zero new admission records"
    );
}

fn fill_write_queue_without_admission(manager: &IndexManager, tenant_id: &str) {
    let index = manager.get_or_load(tenant_id).unwrap();
    let tx = manager
        .get_or_create_write_queue(tenant_id, &index)
        .unwrap();
    for i in 0..=WRITE_QUEUE_ADMISSION_FAILURE_PROBE_LIMIT {
        let result = tx.try_send(WriteOp {
            task_id: format!("synthetic_capacity_task_{i}"),
            actions: vec![WriteAction::Delete(format!("synthetic_missing_doc_{i}"))],
        });
        match result {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => return,
            Err(error) => panic!("capacity prefill should only stop at Full, got {error:?}"),
        }
    }
    panic!("capacity prefill did not reach QueueFull");
}

fn tenant_tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, current: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut entries: Vec<_> = std::fs::read_dir(current)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

async fn create_move_fixture(manager: &IndexManager, tenant: &str, marker: &str) {
    manager.create_tenant(tenant).unwrap();
    manager
        .add_documents_sync(
            tenant,
            vec![Document {
                id: format!("{marker}_document"),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text(format!("{marker} searchable")),
                )]),
            }],
        )
        .await
        .unwrap();
    let path = manager.base_path.join(tenant);
    std::fs::write(
        path.join("settings.json"),
        format!(r#"{{"marker":"{marker}"}}"#),
    )
    .unwrap();
    std::fs::create_dir_all(path.join("oplog")).unwrap();
    std::fs::write(path.join("oplog/move_test.jsonl"), marker).unwrap();
    std::fs::write(path.join("committed_seq"), "41").unwrap();
    manager.unload(&tenant.to_string()).unwrap();
}

#[tokio::test]
async fn move_index_replaces_destination_and_preserves_complete_source_tree() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    create_move_fixture(&manager, "source", "source_marker").await;
    create_move_fixture(&manager, "destination", "old_destination_marker").await;
    let expected_source = tenant_tree_bytes(&temp_dir.path().join("source"));

    let task = manager.move_index("source", "destination").await.unwrap();

    assert!(!temp_dir.path().join("source").exists());
    assert_eq!(
        tenant_tree_bytes(&temp_dir.path().join("destination")),
        expected_source
    );
    assert_eq!(
        std::fs::read_to_string(temp_dir.path().join("destination/settings.json")).unwrap(),
        r#"{"marker":"source_marker"}"#
    );
    assert_eq!(
        std::fs::read_to_string(temp_dir.path().join("destination/oplog/move_test.jsonl")).unwrap(),
        "source_marker"
    );
    assert_eq!(
        std::fs::read_to_string(temp_dir.path().join("destination/committed_seq")).unwrap(),
        "41"
    );
    let search = manager
        .search("destination", "source", None, None, 10)
        .unwrap();
    assert_eq!(search.total, 1);
    assert_eq!(search.documents[0].document.id, "source_marker_document");
    assert_eq!(
        manager.get_task(&task.numeric_id.to_string()).unwrap().id,
        task.id
    );
}

#[tokio::test]
async fn move_index_creates_destination_when_none_existed() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    create_move_fixture(&manager, "source", "only_source_marker").await;
    let expected_source = tenant_tree_bytes(&temp_dir.path().join("source"));

    let task = manager.move_index("source", "destination").await.unwrap();

    assert!(!temp_dir.path().join("source").exists());
    assert_eq!(
        tenant_tree_bytes(&temp_dir.path().join("destination")),
        expected_source
    );
    assert_eq!(
        manager.get_task(&task.numeric_id.to_string()).unwrap().id,
        task.id
    );
}

#[tokio::test]
async fn move_index_precommit_faults_preserve_source_and_destination_without_task() {
    use publication::PublicationFaultPoint;

    for fault in [
        PublicationFaultPoint::BeforeStagingDigest,
        PublicationFaultPoint::DuringStagingSync,
        PublicationFaultPoint::AfterPrepareJournal,
        PublicationFaultPoint::AfterTargetBackup,
        PublicationFaultPoint::AfterStagingPromote,
        PublicationFaultPoint::BeforeCommitJournal,
    ] {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        create_move_fixture(&manager, "source", "source_marker").await;
        create_move_fixture(&manager, "destination", "destination_marker").await;
        let source_before = tenant_tree_bytes(&temp_dir.path().join("source"));
        let destination_before = tenant_tree_bytes(&temp_dir.path().join("destination"));
        let tasks_before = manager.tasks.len();

        let result = manager
            .move_index_with_fault_for_test("source", "destination", fault)
            .await;

        assert!(result.is_err(), "fault {fault:?} must fail move_index");
        assert_eq!(
            tenant_tree_bytes(&temp_dir.path().join("source")),
            source_before
        );
        assert_eq!(
            tenant_tree_bytes(&temp_dir.path().join("destination")),
            destination_before
        );
        assert_eq!(manager.tasks.len(), tasks_before);
    }
}

#[tokio::test]
async fn move_index_retry_converges_after_commit_before_source_cleanup_fault() {
    use publication::PublicationFaultPoint;

    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    create_move_fixture(&manager, "source", "source_marker").await;
    create_move_fixture(&manager, "destination", "destination_marker").await;
    let source_before = tenant_tree_bytes(&temp_dir.path().join("source"));
    let tasks_before = manager.tasks.len();

    let interrupted = manager
        .move_index_with_fault_for_test(
            "source",
            "destination",
            PublicationFaultPoint::BeforeSourceCleanup,
        )
        .await;

    assert!(interrupted.is_err());
    assert_eq!(
        tenant_tree_bytes(&temp_dir.path().join("source")),
        source_before
    );
    assert_eq!(
        tenant_tree_bytes(&temp_dir.path().join("destination")),
        source_before
    );
    assert_eq!(manager.tasks.len(), tasks_before);

    let task = manager.move_index("source", "destination").await.unwrap();
    assert!(!temp_dir.path().join("source").exists());
    assert_eq!(
        tenant_tree_bytes(&temp_dir.path().join("destination")),
        source_before
    );
    assert_eq!(
        manager.get_task(&task.numeric_id.to_string()).unwrap().id,
        task.id
    );
}

#[test]
fn manager_mod_stays_under_hard_line_limit() {
    const MANAGER_MOD_HARD_LIMIT: usize = 800;

    let line_count = include_str!("mod.rs").lines().count();
    assert!(
        line_count <= MANAGER_MOD_HARD_LIMIT,
        "engine/src/index/manager/mod.rs must stay at or below {} lines (found {})",
        MANAGER_MOD_HARD_LIMIT,
        line_count
    );
}

#[tokio::test]
async fn reserve_numeric_task_id_skips_existing_alias_keys() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    let seed = 4242_i64;
    let task_a = TaskInfo::new("task_alias_a".to_string(), seed, 0);
    let task_b = TaskInfo::new("task_alias_b".to_string(), seed + 1, 0);

    manager.tasks.insert(task_a.id.clone(), task_a.clone());
    manager.tasks.insert(task_a.numeric_id.to_string(), task_a);
    manager.tasks.insert(task_b.id.clone(), task_b.clone());
    manager.tasks.insert(task_b.numeric_id.to_string(), task_b);

    let reserved = manager.reserve_numeric_task_id(seed);
    assert_eq!(reserved, seed + 2);
}

#[tokio::test]
async fn make_noop_task_registers_numeric_alias_lookup() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    let task = manager.make_noop_task("noop_task_alias").unwrap();
    let by_numeric_id = manager.get_task(&task.numeric_id.to_string()).unwrap();

    assert_eq!(by_numeric_id.id, task.id);
    assert!(matches!(by_numeric_id.status, TaskStatus::Succeeded));
}

#[tokio::test]
async fn evict_old_tasks_keeps_in_flight_tasks_and_reclaims_terminal_aliases_first() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    let tenant_id = "eviction_retention";

    let total_tasks = MAX_TASKS_PER_TENANT + 2;
    let base_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);

    for task_num in 0..total_tasks {
        let numeric_id = 10_000 + task_num as i64;
        let task_id = format!("task_{}_{}", tenant_id, task_num);
        let mut task = TaskInfo::new(task_id.clone(), numeric_id, 0);
        task.created_at = base_time + std::time::Duration::from_secs(task_num as u64);
        task.status = match task_num {
            0 => TaskStatus::Enqueued,
            1 | 2 => TaskStatus::Succeeded,
            _ => TaskStatus::Processing,
        };

        manager.tasks.insert(task_id, task.clone());
        manager.tasks.insert(numeric_id.to_string(), task);
    }

    manager.evict_old_tasks(tenant_id, MAX_TASKS_PER_TENANT);

    let oldest_in_flight = manager
        .get_task("task_eviction_retention_0")
        .expect("oldest in-flight task must not be evicted");
    assert!(matches!(oldest_in_flight.status, TaskStatus::Enqueued));

    for task_num in [1, 2] {
        let task_key = format!("task_{}_{}", tenant_id, task_num);
        let numeric_alias = (10_000 + task_num as i64).to_string();
        assert!(
            !manager.tasks.contains_key(&task_key),
            "terminal canonical task key should be evicted first"
        );
        assert!(
            !manager.tasks.contains_key(&numeric_alias),
            "terminal numeric alias key should be evicted with canonical key"
        );
    }
}

#[tokio::test]
async fn wait_for_write_durable_keeps_returned_taskid_resolvable_after_overflow_sweep() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    let tenant_id = "durable_task_lookup";

    let total_tasks = MAX_TASKS_PER_TENANT + 2;
    let base_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);

    for task_num in 0..total_tasks {
        let numeric_id = 30_000 + task_num as i64;
        let task_id = format!("task_{}_{}", tenant_id, task_num);
        let mut task = TaskInfo::new(task_id.clone(), numeric_id, 0);
        task.created_at = base_time + std::time::Duration::from_secs(task_num as u64);
        task.status = match task_num {
            0 => TaskStatus::Enqueued,
            1 => TaskStatus::Succeeded,
            _ => TaskStatus::Processing,
        };
        manager.tasks.insert(task_id, task.clone());
        manager.tasks.insert(numeric_id.to_string(), task);
    }

    manager.evict_old_tasks(tenant_id, MAX_TASKS_PER_TENANT);

    let completed_task_id = "task_durable_task_lookup_2";
    let completed_numeric_id = "30002";
    manager.tasks.alter(completed_task_id, |_, mut task| {
        task.status = TaskStatus::Succeeded;
        task
    });
    manager.tasks.alter(completed_numeric_id, |_, mut task| {
        task.status = TaskStatus::Succeeded;
        task
    });

    manager
        .wait_for_write_durable(completed_task_id)
        .await
        .expect("wait must observe terminal status");

    let by_numeric = manager
        .get_task(completed_numeric_id)
        .expect("durable write must leave returned numeric taskID resolvable");
    assert_eq!(by_numeric.id, completed_task_id);
    assert!(matches!(by_numeric.status, TaskStatus::Succeeded));
}

#[tokio::test]
async fn wait_for_write_durable_reclaims_terminal_overflow_without_new_write() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    let tenant_id = "idle_tenant_reclaim";

    let total_tasks = MAX_TASKS_PER_TENANT + 2;
    let base_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);

    for task_num in 0..total_tasks {
        let numeric_id = 20_000 + task_num as i64;
        let task_id = format!("task_{}_{}", tenant_id, task_num);
        let mut task = TaskInfo::new(task_id.clone(), numeric_id, 0);
        task.created_at = base_time + std::time::Duration::from_secs(task_num as u64);
        task.status = match task_num {
            0 => TaskStatus::Enqueued,
            1 => TaskStatus::Succeeded,
            _ => TaskStatus::Processing,
        };
        manager.tasks.insert(task_id, task.clone());
        manager.tasks.insert(numeric_id.to_string(), task);
    }

    manager.evict_old_tasks(tenant_id, MAX_TASKS_PER_TENANT);

    let becoming_terminal_id = "task_idle_tenant_reclaim_2";
    let other_terminal_id = "task_idle_tenant_reclaim_4";
    let other_terminal_numeric_id = "20004";
    manager.tasks.alter(becoming_terminal_id, |_, mut task| {
        task.status = TaskStatus::Succeeded;
        task
    });
    manager.tasks.alter("20002", |_, mut task| {
        task.status = TaskStatus::Succeeded;
        task
    });
    manager.tasks.alter(other_terminal_id, |_, mut task| {
        task.status = TaskStatus::Succeeded;
        task
    });
    manager
        .tasks
        .alter(other_terminal_numeric_id, |_, mut task| {
            task.status = TaskStatus::Succeeded;
            task
        });

    manager
        .wait_for_write_durable(becoming_terminal_id)
        .await
        .expect("wait must observe terminal status");

    assert!(
        manager.tasks.contains_key(becoming_terminal_id),
        "durable wait must preserve the just-returned taskID for immediate lookup compatibility"
    );
    assert!(
        manager.tasks.contains_key("20002"),
        "numeric alias for just-returned taskID must remain resolvable immediately after success"
    );
    assert!(
        !manager.tasks.contains_key(other_terminal_id),
        "durable wait should still reclaim other terminal overflow tasks"
    );
    assert!(
        !manager.tasks.contains_key(other_terminal_numeric_id),
        "reclaimed terminal overflow task must remove numeric alias as well"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_persistent_admission_task_replays_after_restart_window() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_queue_restart_replay";
    let object_id = "write_queue_restart_replay_doc";

    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();
    let task_id = format!("task_{tenant_id}_legacy_replay");
    let numeric_id = 42;
    let store = WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap();
    store
        .append_record(WriteAdmissionRecord::new(
            task_id,
            numeric_id,
            1,
            vec![WriteAction::Upsert(write_queue_test_document(
                object_id,
                "restart replay coverage",
            ))],
        ))
        .unwrap();
    assert_eq!(tenant_admission_record_count(temp_dir.path(), tenant_id), 1);
    drop(manager);

    let restarted_manager = IndexManager::new(temp_dir.path());
    let replayed_document = wait_for_replayed_document(&restarted_manager, tenant_id, object_id)
        .await
        .expect("restart replay assertion: admitted write_queue task must exist after restart");
    assert_eq!(
        replayed_document.id, object_id,
        "restart replay must preserve the admitted document objectID"
    );
    assert!(
        matches!(
            replayed_document.fields.get("title"),
            Some(FieldValue::Text(title)) if title == "restart replay coverage"
        ),
        "restart replay must preserve the admitted document fields"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        0,
        "startup reconciliation must clean up the replayed legacy admission record"
    );
}

async fn wait_for_replayed_document(
    manager: &IndexManager,
    tenant_id: &str,
    object_id: &str,
) -> Option<Document> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(document) = manager
            .get_document(tenant_id, object_id)
            .expect("restart replay lookup must succeed")
        {
            return Some(document);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn add_documents_write_queue_full_does_not_allocate_task_aliases() {
    let temp_dir = TempDir::new().unwrap();
    assert_queue_full_preserves_pre_admission_state(AdmissionCapacityEntryPoint::Upsert, &temp_dir);
}

#[tokio::test(flavor = "current_thread")]
async fn delete_documents_write_queue_full_does_not_allocate_task_aliases() {
    let temp_dir = TempDir::new().unwrap();
    assert_queue_full_preserves_pre_admission_state(AdmissionCapacityEntryPoint::Delete, &temp_dir);
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(flapjack_write_durable_timeout_env)]
async fn live_write_admission_bypasses_persistence_and_keeps_manager_contract() {
    let _writer_timeout_guard = WriteQueueWriterAcquireTimeoutEnvGuard::set("5000");
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "live_admission_withdrawal";
    let object_id = "live_admission_doc";
    let tenant_path = temp_dir.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let manager = IndexManager::new(temp_dir.path());
    let index = Arc::new(
        Index::create_with_budget(
            &tenant_path,
            crate::index::schema::Schema::builder().build(),
            Arc::new(MemoryBudget::new(MemoryBudgetConfig {
                max_concurrent_writers: 1,
                ..Default::default()
            })),
        )
        .unwrap(),
    );
    manager
        .loaded
        .insert(tenant_id.to_string(), Arc::clone(&index));
    let held_writer = index
        .writer()
        .expect("test precondition: held writer must keep the queued write pending");

    let task = manager
        .add_documents(
            tenant_id,
            vec![write_queue_test_document(
                object_id,
                "live withdrawal coverage",
            )],
        )
        .expect("live admission must still enter through the manager write path");

    assert!(
        tenant_task_snapshot_contains(&manager, tenant_id, &task.id),
        "live admission must preserve the canonical task alias"
    );
    assert_eq!(
        manager.get_task(&task.numeric_id.to_string()).unwrap().id,
        task.id,
        "live admission must preserve the numeric task alias"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        0,
        "new live admissions must not create persistent admission records"
    );

    drop(held_writer);
    manager.wait_for_write_durable(&task.id).await.unwrap();
    let document = manager
        .get_document(tenant_id, object_id)
        .unwrap()
        .expect("durable live admission must be indexed successfully");
    assert!(
        matches!(
            document.fields.get("title"),
            Some(FieldValue::Text(title)) if title == "live withdrawal coverage"
        ),
        "durable live admission must preserve the indexed document fields"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        0,
        "successful live admission cleanup must not depend on a persistent record"
    );

    let capacity_temp_dir = TempDir::new().unwrap();
    assert_queue_full_preserves_pre_admission_state(
        AdmissionCapacityEntryPoint::Upsert,
        &capacity_temp_dir,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn durable_add_documents_write_queue_full_returns_queue_full_before_ack_wait() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "durable_write_queue_capacity";
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();
    fill_write_queue_without_admission(&manager, tenant_id);
    let before_keys = tenant_task_key_snapshot(&manager, tenant_id);
    let before_records = tenant_admission_record_count(temp_dir.path(), tenant_id);

    let result = manager
        .add_documents_durable(
            tenant_id,
            vec![write_queue_test_document(
                "durable_capacity_doc",
                "durable queue capacity coverage",
            )],
        )
        .await;

    assert!(
        matches!(result, Err(FlapjackError::QueueFull)),
        "durable write must return pre-admission QueueFull before ACK wait, got {result:?}"
    );
    if let Err(error) = &result {
        assert_eq!(error.status_code(), http::StatusCode::TOO_MANY_REQUESTS);
        assert!(
            !matches!(
                error,
                FlapjackError::WriteAckTimeout
                    | FlapjackError::TooManyConcurrentWrites { .. }
                    | FlapjackError::Tantivy(_)
                    | FlapjackError::Io(_)
            ),
            "durable full-channel admission must not surface a 5xx-path error, got {error:?}"
        );
    }
    assert_eq!(tenant_task_key_snapshot(&manager, tenant_id), before_keys);
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        before_records,
        "durable QueueFull must not allocate a new admission record"
    );
    assert!(
        manager.abort_tenant_write_task_for_test(tenant_id),
        "tenant write_queue task should still be abortable after durable capacity probe"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn closed_write_queue_preserves_queue_full_admission_contract() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "closed_write_queue_admission";
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();
    manager
        .add_documents(tenant_id, vec![write_queue_test_document("seed", "seed")])
        .unwrap();
    assert!(manager.abort_tenant_write_task_for_test(tenant_id));
    tokio::task::yield_now().await;

    let before_keys = tenant_task_key_snapshot(&manager, tenant_id);
    let before_records = tenant_admission_record_count(temp_dir.path(), tenant_id);
    let result = manager.delete_documents(tenant_id, vec!["seed".to_string()]);

    assert!(
        matches!(result, Err(FlapjackError::QueueFull)),
        "closed write queue must retain the legacy retryable QueueFull contract, got {result:?}"
    );
    assert_eq!(tenant_task_key_snapshot(&manager, tenant_id), before_keys);
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        before_records
    );
}

#[tokio::test(flavor = "current_thread")]
async fn every_write_queue_entrypoint_reserves_capacity_before_task_or_admission_allocation() {
    for entry_point in [
        AdmissionCapacityEntryPoint::Insert,
        AdmissionCapacityEntryPoint::Upsert,
        AdmissionCapacityEntryPoint::ReplicationAdd,
        AdmissionCapacityEntryPoint::Delete,
        AdmissionCapacityEntryPoint::ReplicationDelete,
        AdmissionCapacityEntryPoint::Compact,
    ] {
        let temp_dir = TempDir::new().unwrap();
        assert_queue_full_preserves_pre_admission_state(entry_point, &temp_dir);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn write_queue_pre_admission_store_io_error_rolls_back_without_task_aliases() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_admission_file_boundary";
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();
    let admission_path = temp_dir.path().join(tenant_id).join("write_admission");
    std::fs::write(&admission_path, b"not a directory").unwrap();
    let before_tasks = tenant_task_key_snapshot(&manager, tenant_id);

    let error = manager
        .add_documents(
            tenant_id,
            vec![write_queue_test_document(
                "pre_admission_io_failure_doc",
                "must not be admitted",
            )],
        )
        .expect_err("regular file at write_admission must fail admission append");

    assert!(
        !matches!(error, FlapjackError::QueueFull),
        "admission append failure must surface the underlying non-429 error"
    );
    assert_eq!(
        tenant_task_key_snapshot(&manager, tenant_id),
        before_tasks,
        "pre-admission append failure must not insert canonical or numeric task aliases"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        0,
        "failed append must leave no complete replayable admission record"
    );

    std::fs::remove_file(&admission_path).unwrap();
    let next_task = manager
        .add_documents(
            tenant_id,
            vec![write_queue_test_document(
                "pre_admission_slot_reuse_doc",
                "slot must be reusable",
            )],
        )
        .expect("released reservation must let the next valid operation enter the queue");
    assert!(
        tenant_task_snapshot_contains(&manager, tenant_id, &next_task.id),
        "valid retry must be admitted after the failed append releases its permit"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn write_queue_oplog_open_failure_rejects_write_before_task_allocation() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_admission_oplog_open_failure";
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();
    std::fs::write(
        temp_dir.path().join(tenant_id).join("oplog"),
        b"not a directory",
    )
    .unwrap();
    let before_tasks = tenant_task_key_snapshot(&manager, tenant_id);

    let error = manager
        .add_documents(
            tenant_id,
            vec![write_queue_test_document(
                "oplog_open_failure_doc",
                "must not be admitted",
            )],
        )
        .expect_err("oplog open failure must reject production write admission");

    assert!(
        !matches!(error, FlapjackError::QueueFull),
        "oplog open failure is reconciliation evidence failure, not capacity backpressure"
    );
    assert_eq!(
        tenant_task_key_snapshot(&manager, tenant_id),
        before_tasks,
        "oplog open failure must happen before canonical or numeric task allocation"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        0,
        "oplog open failure must not create admission records"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn write_queue_complete_admission_corruption_fails_tenant_open() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_queue_corrupt_admission";
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();
    drop(manager);

    write_corrupt_complete_admission_record(temp_dir.path(), tenant_id);

    let restarted = IndexManager::new(temp_dir.path());
    let error = restarted
        .get_document(tenant_id, "any")
        .expect_err("complete corrupt admission record must fail tenant open");
    assert!(
        matches!(error, FlapjackError::Json(_)),
        "complete admission corruption must surface a startup/open error, got {error:?}"
    );
    assert_eq!(
        restarted.loaded_count(),
        0,
        "failed admission startup validation must not leave the tenant cached as loaded"
    );
    let repeated_error = restarted
        .get_document(tenant_id, "any")
        .expect_err("complete corrupt admission record must keep failing tenant open");
    assert!(
        matches!(repeated_error, FlapjackError::Json(_)),
        "complete admission corruption must remain a sticky startup/open error, got {repeated_error:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn write_queue_create_tenant_corrupt_admission_failure_is_not_cached() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_queue_create_tenant_corrupt_admission";
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();
    drop(manager);

    write_corrupt_complete_admission_record(temp_dir.path(), tenant_id);

    let restarted = IndexManager::new(temp_dir.path());
    let error = restarted
        .create_tenant(tenant_id)
        .expect_err("existing tenant create/load must validate corrupt admission records");
    assert!(
        matches!(error, FlapjackError::Json(_)),
        "corrupt admission record must fail create_tenant startup validation, got {error:?}"
    );
    assert_eq!(
        restarted.loaded_count(),
        0,
        "create_tenant failure must not cache the tenant as loaded"
    );
    let repeated_error = restarted
        .get_document(tenant_id, "any")
        .expect_err("failed create_tenant validation must not let later loads bypass admission");
    assert!(
        matches!(repeated_error, FlapjackError::Json(_)),
        "later open must keep failing on corrupt admission, got {repeated_error:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn write_queue_incomplete_admission_tail_is_ignored_and_removed() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_queue_incomplete_admission";
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();
    drop(manager);

    let admission_dir = temp_dir.path().join(tenant_id).join("write_admission");
    std::fs::create_dir_all(&admission_dir).unwrap();
    let tail_path = admission_dir.join("00000000000000000001.tmp");
    std::fs::write(&tail_path, b"incomplete").unwrap();

    let restarted = IndexManager::new(temp_dir.path());
    assert!(
        restarted
            .get_document(tenant_id, "missing")
            .unwrap()
            .is_none(),
        "incomplete never-replayable admission tail must not block tenant open"
    );
    assert!(
        !tail_path.exists(),
        "incomplete admission tail should be removed during validation"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn write_queue_replay_failure_blocks_startup_before_live_admission() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_queue_replay_startup_failure";
    let object_id = "replay_startup_failure_doc";
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();
    drop(manager);

    let store = WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap();
    store
        .append_record(WriteAdmissionRecord::new(
            format!("task_{}_pending", tenant_id),
            10_001,
            1,
            vec![WriteAction::Upsert(write_queue_test_document(
                object_id,
                "must not replay through corrupt settings",
            ))],
        ))
        .unwrap();
    std::fs::write(
        temp_dir.path().join(tenant_id).join("settings.json"),
        b"{not-json",
    )
    .unwrap();

    let restarted = IndexManager::new(temp_dir.path());
    let startup_error = restarted
        .get_document(tenant_id, object_id)
        .expect_err("replay startup failure must fail tenant open before returning a live queue");
    assert!(
        matches!(startup_error, FlapjackError::Json(_)),
        "replay startup failure must surface its underlying error, got {startup_error:?}"
    );
    assert_eq!(
        restarted.loaded_count(),
        0,
        "replay startup failure must not cache the tenant as loaded"
    );

    let before_records = tenant_admission_record_count(temp_dir.path(), tenant_id);
    let live_error = restarted
        .add_documents(
            tenant_id,
            vec![write_queue_test_document(
                "live_after_replay_failure",
                "must not be admitted",
            )],
        )
        .expect_err("live write admission must stay blocked while replay startup fails");
    assert!(
        matches!(live_error, FlapjackError::Json(_)),
        "live write must see the replay startup error, got {live_error:?}"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        before_records,
        "live write must not append a new admission record behind failed startup replay"
    );
}

#[test]
fn write_admission_append_uses_recovered_sequence_state_without_scanning() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_admission_recovered_sequence";
    std::fs::create_dir_all(temp_dir.path().join(tenant_id)).unwrap();

    let initial_store = WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap();
    initial_store
        .append_record(WriteAdmissionRecord::new(
            format!("task_{tenant_id}_recovered"),
            19_999,
            1,
            vec![WriteAction::Delete("recovered_document".to_string())],
        ))
        .unwrap();
    drop(initial_store);

    let recovered_store = WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap();
    let recovery_load_count = recovered_store.load_records_unlocked_call_count_for_test();
    assert_eq!(
        recovery_load_count, 1,
        "open must perform one recovery scan"
    );

    let appended_sequences = ["first", "second"]
        .into_iter()
        .enumerate()
        .map(|(offset, suffix)| {
            recovered_store
                .append_record(WriteAdmissionRecord::new(
                    format!("task_{tenant_id}_{suffix}"),
                    20_000 + offset as i64,
                    1,
                    vec![WriteAction::Delete(format!("document_{offset}"))],
                ))
                .unwrap()
                .sequence
        })
        .collect::<Vec<_>>();

    assert_eq!(appended_sequences, vec![2, 3]);
    assert_eq!(
        recovered_store.load_records_unlocked_call_count_for_test(),
        recovery_load_count,
        "successful appends must use recovered sequence state without rescanning admission records"
    );
}

#[cfg(unix)]
#[test]
fn write_admission_first_directory_create_requires_tenant_directory_sync() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_admission_first_parent_sync";
    let tenant_path = temp_dir.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let store = WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap();
    std::fs::set_permissions(&tenant_path, std::fs::Permissions::from_mode(0o300)).unwrap();

    let result = store.append_record(WriteAdmissionRecord::new(
        format!("task_{}_first", tenant_id),
        20_001,
        1,
        vec![WriteAction::Delete("missing".to_string())],
    ));
    std::fs::set_permissions(&tenant_path, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(
        result.is_err(),
        "first write_admission directory creation must fail if the tenant directory cannot be synced"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        0,
        "failed first directory transition must not publish a replayable record"
    );
    assert!(
        !tenant_path.join("write_admission").exists(),
        "failed first directory transition must roll back the directory so retry repeats the parent sync"
    );

    let retry_task_id = format!("task_{}_retry", tenant_id);
    let retry_record = store
        .append_record(WriteAdmissionRecord::new(
            retry_task_id.clone(),
            20_002,
            1,
            vec![WriteAction::Delete("missing".to_string())],
        ))
        .expect("retry must durably admit after the tenant directory becomes syncable");
    assert_eq!(retry_record.task_id, retry_task_id);
    assert_eq!(
        retry_record.sequence, 2,
        "failed append sequence must remain consumed because gaps are harmless"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        1,
        "retry must publish exactly one replayable record"
    );
}

#[cfg(unix)]
#[test]
fn write_admission_last_directory_remove_requires_tenant_directory_sync() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_admission_last_parent_sync";
    let tenant_path = temp_dir.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let store = WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap();
    store
        .append_record(WriteAdmissionRecord::new(
            format!("task_{}_last", tenant_id),
            20_002,
            1,
            vec![WriteAction::Delete("missing".to_string())],
        ))
        .unwrap();
    std::fs::set_permissions(&tenant_path, std::fs::Permissions::from_mode(0o300)).unwrap();

    let result = store.remove_task(&format!("task_{}_last", tenant_id));
    std::fs::set_permissions(&tenant_path, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(
        result.is_err(),
        "last write_admission directory removal must fail if the tenant directory cannot be synced"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        1,
        "cleanup failure must preserve the replayable record for restart reconciliation"
    );
}

#[test]
fn write_admission_remove_directory_failure_restores_record() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_admission_remove_directory_failure";
    std::fs::create_dir_all(temp_dir.path().join(tenant_id)).unwrap();
    let store = WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap();
    let task_id = format!("task_{tenant_id}_last");
    store
        .append_record(WriteAdmissionRecord::new(
            task_id.clone(),
            25_001,
            1,
            vec![WriteAction::Delete("missing".to_string())],
        ))
        .unwrap();

    let admission_dir = temp_dir.path().join(tenant_id).join("write_admission");
    let unexpected_path = admission_dir.join("unexpected");
    let hook_path = unexpected_path.clone();
    store.set_before_empty_directory_remove_hook(move || {
        std::fs::write(hook_path, b"blocks directory removal").unwrap();
    });

    store
        .remove_task(&task_id)
        .expect_err("non-empty admission directory must fail last-record cleanup");
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        1,
        "directory cleanup failure must restore the removed admission record"
    );

    std::fs::remove_file(unexpected_path).unwrap();
    store
        .remove_task(&task_id)
        .expect("cleanup retry must remove the restored record");
    assert!(
        !admission_dir.exists(),
        "cleanup retry must remove the now-empty admission directory"
    );
}

#[cfg(unix)]
#[test]
fn write_admission_batch_cleanup_failure_preserves_all_replayable_records() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_admission_batch_cleanup_failure";
    let tenant_path = temp_dir.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let store = WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap();
    let task_ids = [
        format!("task_{}_first", tenant_id),
        format!("task_{}_second", tenant_id),
    ];
    for (offset, task_id) in task_ids.iter().enumerate() {
        store
            .append_record(WriteAdmissionRecord::new(
                task_id.clone(),
                30_000 + offset as i64,
                1,
                vec![WriteAction::Delete(format!("doc_{offset}"))],
            ))
            .unwrap();
    }
    std::fs::set_permissions(&tenant_path, std::fs::Permissions::from_mode(0o300)).unwrap();

    let result = store.remove_tasks(task_ids.iter().map(String::as_str));
    std::fs::set_permissions(&tenant_path, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(
        result.is_err(),
        "batched cleanup must fail if the tenant directory cannot be synced"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        task_ids.len(),
        "batched cleanup failure must preserve every replayable record in the committed batch"
    );

    store
        .remove_tasks(task_ids.iter().map(String::as_str))
        .expect("cleanup retry must use the unchanged live-record state");
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        0,
        "cleanup retry must durably remove every restored admission record"
    );
    assert!(
        !tenant_path.join("write_admission").exists(),
        "cleanup retry must remove the admission directory after its last live records"
    );
}

#[test]
#[serial_test::serial(write_admission_directory_lifecycle)]
fn write_queue_admission_append_and_last_record_removal_are_atomic() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_admission_concurrent_directory_lifecycle";
    std::fs::create_dir_all(temp_dir.path().join(tenant_id)).unwrap();
    let store = Arc::new(WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap());
    let removed_task_id = format!("task_{tenant_id}_removed");
    store
        .append_record(WriteAdmissionRecord::new(
            removed_task_id.clone(),
            40_001,
            1,
            vec![WriteAction::Delete("old_document".to_string())],
        ))
        .unwrap();

    let (removal_reached_tx, removal_reached_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_removal_tx, resume_removal_rx) = std::sync::mpsc::sync_channel(1);
    store.set_before_empty_directory_remove_hook(move || {
        removal_reached_tx.send(()).unwrap();
        resume_removal_rx.recv().unwrap();
    });

    let removal_store = Arc::clone(&store);
    let removal = std::thread::spawn(move || removal_store.remove_task(&removed_task_id));
    removal_reached_rx.recv().unwrap();

    let appended_task_id = format!("task_{tenant_id}_appended");
    let append_store = Arc::clone(&store);
    let append_task_id = appended_task_id.clone();
    let (append_contended_tx, append_contended_rx) = std::sync::mpsc::sync_channel(1);
    store.set_lifecycle_contention_hook(move || append_contended_tx.send(()).unwrap());
    let append = std::thread::spawn(move || {
        append_store.append_record(WriteAdmissionRecord::new(
            append_task_id,
            40_002,
            1,
            vec![WriteAction::Delete("new_document".to_string())],
        ))
    });
    append_contended_rx
        .recv()
        .expect("append must contend while last-record removal owns the directory lifecycle");
    resume_removal_tx.send(()).unwrap();
    removal
        .join()
        .unwrap()
        .expect("last-record removal must not fail when an append races it");
    append
        .join()
        .unwrap()
        .expect("append during last-record removal must succeed");

    let records = store.load_records().unwrap();
    assert_eq!(
        records
            .iter()
            .map(|record| record.task_id.as_str())
            .collect::<Vec<_>>(),
        vec![appended_task_id.as_str()],
        "the removed record must stay removed and the concurrently appended record must remain live"
    );
    assert!(
        temp_dir
            .path()
            .join(tenant_id)
            .join("write_admission")
            .is_dir(),
        "a live admission record must retain its store directory"
    );
}

/// Group-commit contract: concurrent admissions that are all staged (renamed into
/// place) before any of them publishes must share a single directory flush, and no
/// caller may return until its own record is crash-safe on disk. The `after_stage`
/// barrier makes the coalescing deterministic — instrumentation, not timing — so the
/// flush count is an exact `1`, not a flaky "usually fewer".
#[test]
fn write_queue_admission_concurrent_appends_coalesce_into_one_durable_flush() {
    const CONCURRENT_ADMISSIONS: usize = 4;

    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_admission_group_commit_flush";
    std::fs::create_dir_all(temp_dir.path().join(tenant_id)).unwrap();
    let store = Arc::new(WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap());

    // Hold every appender at a barrier after it has staged (renamed) its record but
    // before it publishes, so all renames are on disk before the first flush leader
    // captures its "all tickets up to here" bound.
    let staged_barrier = Arc::new(std::sync::Barrier::new(CONCURRENT_ADMISSIONS));
    store.set_after_stage_hook({
        let staged_barrier = Arc::clone(&staged_barrier);
        move || {
            staged_barrier.wait();
        }
    });

    let admissions = (0..CONCURRENT_ADMISSIONS)
        .map(|offset| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                store.append_record(WriteAdmissionRecord::new(
                    format!("task_group_commit_{offset}"),
                    50_000 + offset as i64,
                    1,
                    vec![WriteAction::Delete(format!("group_doc_{offset}"))],
                ))
            })
        })
        .collect::<Vec<_>>();

    let admitted_sequences = admissions
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .unwrap()
                .expect("admission must not return before its record is crash-safe")
                .sequence
        })
        .collect::<Vec<_>>();

    assert_eq!(
        store.directory_flush_call_count_for_test(),
        1,
        "concurrent admissions staged behind a shared barrier must coalesce into exactly one directory flush"
    );

    // Every admission that returned must be durably readable — proving no caller was
    // handed an admitted task before its own record was crash-safe.
    let mut recovered_task_ids = store
        .load_records()
        .unwrap()
        .into_iter()
        .map(|record| record.task_id)
        .collect::<Vec<_>>();
    recovered_task_ids.sort();
    let mut expected_task_ids = (0..CONCURRENT_ADMISSIONS)
        .map(|offset| format!("task_group_commit_{offset}"))
        .collect::<Vec<_>>();
    expected_task_ids.sort();
    assert_eq!(
        recovered_task_ids, expected_task_ids,
        "every returned admission must be durably persisted on disk"
    );

    let mut sorted_sequences = admitted_sequences;
    sorted_sequences.sort_unstable();
    assert_eq!(
        sorted_sequences,
        (1..=CONCURRENT_ADMISSIONS as u64).collect::<Vec<_>>(),
        "each concurrent admission must receive a unique gap-free sequence"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(flapjack_write_durable_timeout_env)]
async fn write_queue_committed_unpruned_admission_reconciles_without_reapplying() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_queue_committed_unpruned";
    let object_id = "committed_unpruned_doc";
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();
    let task = manager
        .add_documents(
            tenant_id,
            vec![write_queue_test_document(
                object_id,
                "original committed value",
            )],
        )
        .unwrap();
    manager.wait_for_write_durable(&task.id).await.unwrap();
    let seq_before = manager.get_oplog(tenant_id).unwrap().current_seq();

    let store = WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap();
    store
        .append_record(WriteAdmissionRecord::new(
            task.id.clone(),
            task.numeric_id,
            1,
            vec![WriteAction::Upsert(write_queue_test_document(
                object_id,
                "replayed side effect",
            ))],
        ))
        .unwrap();
    assert_eq!(tenant_admission_record_count(temp_dir.path(), tenant_id), 1);
    drop(manager);

    let restarted = IndexManager::new(temp_dir.path());
    let document = restarted
        .get_document(tenant_id, object_id)
        .unwrap()
        .expect("committed document must remain visible after restart");
    assert!(
        matches!(
            document.fields.get("title"),
            Some(FieldValue::Text(title)) if title == "original committed value"
        ),
        "committed admission reconciliation must not replay duplicate side effects"
    );
    assert_eq!(
        restarted.get_oplog(tenant_id).unwrap().current_seq(),
        seq_before,
        "reconciled committed admission must not append oplog side effects again"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        0,
        "committed admission record must be pruned during startup reconciliation"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(flapjack_write_durable_timeout_env)]
async fn post_admission_write_queue_abort_returns_write_ack_timeout_and_keeps_task() {
    let _timeout_guard = DurableWriteTimeoutEnvGuard::set("25");
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_queue_durable_timeout";
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant(tenant_id).unwrap();

    let task = manager
        .add_documents(
            tenant_id,
            vec![write_queue_test_document(
                "write_queue_timeout_doc",
                "post admission timeout coverage",
            )],
        )
        .unwrap();
    assert!(
        tenant_task_snapshot_contains(&manager, tenant_id, &task.id),
        "accepted write_queue task must be visible before aborting the consumer"
    );
    assert!(
        manager.abort_tenant_write_task_for_test(tenant_id),
        "tenant write_queue consumer must be aborted after admission"
    );

    let result = manager.wait_for_write_durable(&task.id).await;
    assert!(
        matches!(result, Err(FlapjackError::WriteAckTimeout)),
        "stalled post-admission write_queue consumer must surface WriteAckTimeout, got {result:?}"
    );
    let canonical_task = manager
        .get_task(&task.id)
        .expect("accepted task must remain inspectable after durable timeout");
    assert!(
        matches!(canonical_task.status, TaskStatus::Enqueued),
        "accepted task must remain pending after durable timeout, got {:?}",
        canonical_task.status
    );
    assert_eq!(
        manager.get_task(&task.numeric_id.to_string()).unwrap().id,
        task.id,
        "numeric task alias must remain inspectable after durable timeout"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(flapjack_write_durable_timeout_env)]
async fn post_admission_write_queue_writer_slot_contention_returns_too_many_concurrent_writes() {
    let _durable_timeout_guard = DurableWriteTimeoutEnvGuard::set("750");
    let _writer_timeout_guard = WriteQueueWriterAcquireTimeoutEnvGuard::set("25");
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "write_queue_writer_slot_contention";
    let tenant_path = temp_dir.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let manager = IndexManager::new(temp_dir.path());
    let index = Arc::new(
        Index::create_with_budget(
            &tenant_path,
            crate::index::schema::Schema::builder().build(),
            Arc::new(MemoryBudget::new(MemoryBudgetConfig {
                max_concurrent_writers: 1,
                ..Default::default()
            })),
        )
        .unwrap(),
    );
    manager
        .loaded
        .insert(tenant_id.to_string(), Arc::clone(&index));
    let _held_writer = index
        .writer()
        .expect("test precondition: held writer must consume the tenant's only writer slot");

    let task = manager
        .add_documents(
            tenant_id,
            vec![write_queue_test_document(
                "writer_slot_contention_doc",
                "writer contention coverage",
            )],
        )
        .expect("writer-slot contention is post-admission and must not reject as QueueFull");
    assert!(
        tenant_task_snapshot_contains(&manager, tenant_id, &task.id),
        "task must be admitted before writer-slot contention is observed"
    );

    let result = manager.wait_for_write_durable(&task.id).await;
    assert!(
        matches!(
            result,
            Err(FlapjackError::TooManyConcurrentWrites { current: _, max: 1 })
        ),
        "post-admission writer-slot contention must surface TooManyConcurrentWrites/503, not QueueFull/429 or timeout; got {result:?}"
    );
    let failed_task = manager
        .get_task(&task.id)
        .expect("failed admitted task must remain inspectable by canonical task id");
    assert!(
        matches!(&failed_task.status, TaskStatus::Failed(message) if message.contains("Too many concurrent writes")),
        "failed task must retain the writer-slot contention failure, got {:?}",
        failed_task.status
    );
    assert_eq!(
        manager.get_task(&task.numeric_id.to_string()).unwrap().id,
        task.id,
        "numeric task alias must remain inspectable after writer-slot contention"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn legacy_compact_admission_cleanup_failure_does_not_report_success() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "compact_cleanup_failure";
    let initial_manager = IndexManager::new(temp_dir.path());
    initial_manager.create_tenant(tenant_id).unwrap();
    drop(initial_manager);

    let task_id = format!("task_{tenant_id}_legacy_compact");
    let numeric_id = 42;
    let store = WriteAdmissionStore::open(temp_dir.path(), tenant_id).unwrap();
    store
        .append_record(WriteAdmissionRecord::new(
            task_id.clone(),
            numeric_id,
            0,
            vec![WriteAction::Compact],
        ))
        .unwrap();
    let admission_dir = temp_dir.path().join(tenant_id).join("write_admission");
    assert_eq!(tenant_admission_record_count(temp_dir.path(), tenant_id), 1);
    std::fs::set_permissions(&admission_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    let restarted = IndexManager::new(temp_dir.path());
    let result = restarted.get_or_load(tenant_id);
    std::fs::set_permissions(&admission_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(
        result.is_err(),
        "cleanup failure during legacy compact replay must fail tenant open instead of reporting success"
    );
    let status = restarted.get_task(&task_id).unwrap().status;
    assert!(
        matches!(status, TaskStatus::Failed(_)),
        "cleanup failure after legacy compact replay must fail the task instead of reporting success; got {status:?}"
    );
    assert_eq!(
        restarted.get_task(&numeric_id.to_string()).unwrap().id,
        task_id,
        "failed legacy compact task must keep its numeric alias inspectable"
    );
    assert_eq!(
        tenant_admission_record_count(temp_dir.path(), tenant_id),
        1,
        "failed legacy compact cleanup must leave the replayable compact record for restart handling"
    );
}

#[tokio::test]
async fn recovery_phase_helpers_are_callable() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("recovery_helpers").unwrap();

    let tenant_path = temp_dir.path().join("recovery_helpers");
    let oplog = manager.get_or_create_oplog("recovery_helpers").unwrap();
    let ops = oplog.read_since(0).unwrap();
    let index = manager.get_or_load("recovery_helpers").unwrap();

    manager.rebuild_lww_map("recovery_helpers", &oplog).unwrap();
    manager
        .replay_config_ops("recovery_helpers", &tenant_path, &ops)
        .unwrap();
    let settings = manager
        .load_settings_after_config("recovery_helpers", &tenant_path)
        .unwrap();
    manager
        .replay_document_ops(
            "recovery_helpers",
            &index,
            &tenant_path,
            &ops,
            super::recovery::RecoverySeqWindow {
                committed_seq: 0,
                final_seq: ops.last().map(|entry| entry.seq).unwrap_or(0),
            },
            settings.as_ref(),
        )
        .unwrap();
    #[cfg(feature = "vector-search")]
    manager.rebuild_vector_index("recovery_helpers", &tenant_path, &ops);
}

#[tokio::test]
async fn replay_config_ops_surfaces_settings_write_failures() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    let tenant_path = temp_dir.path().join("missing_recovery_settings_path");

    let oplog_dir = temp_dir.path().join("replay_config_ops_oplog");
    let oplog = OpLog::open(&oplog_dir, "missing_recovery_settings_path", "test_node").unwrap();
    oplog
        .append(
            "settings",
            serde_json::json!({
                "searchableAttributes": ["title"]
            }),
        )
        .unwrap();
    let ops = oplog.read_since(0).unwrap();

    let result = manager.replay_config_ops("missing_recovery_settings_path", &tenant_path, &ops);
    assert!(
        result.is_err(),
        "settings replay should fail when tenant path does not exist"
    );
}

#[tokio::test]
async fn read_committed_seq_does_not_require_tenant_load() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    let tenant_path = temp_dir.path().join("persisted_seq_only");
    std::fs::create_dir_all(&tenant_path).unwrap();
    crate::index::oplog::write_committed_seq(&tenant_path, 17).unwrap();

    assert!(
        manager.loaded.get("persisted_seq_only").is_none(),
        "test precondition: tenant must not be loaded"
    );

    let committed_seq = crate::index::oplog::read_committed_seq(&tenant_path);
    assert_eq!(committed_seq, 17);

    assert!(
        manager.loaded.get("persisted_seq_only").is_none(),
        "reading committed_seq from disk must not load tenant into memory"
    );
}

fn setup_tenant_with_pending_document_recovery(base_path: &Path, tenant_id: &str) {
    let tenant_path = base_path.join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();

    let schema = crate::index::schema::Schema::builder().build();
    let _ = crate::index::Index::create(&tenant_path, schema).unwrap();

    crate::index::settings::IndexSettings::default()
        .save(tenant_path.join("settings.json"))
        .unwrap();

    let oplog_dir = tenant_path.join("oplog");
    let oplog = OpLog::open(&oplog_dir, tenant_id, "test_node").unwrap();
    oplog
        .append(
            "upsert",
            serde_json::json!({
                "objectID": "doc1",
                "body": {
                    "objectID": "doc1",
                    "title": "Concurrent recovery fixture"
                }
            }),
        )
        .unwrap();

    std::fs::write(tenant_path.join("committed_seq"), "0").unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_or_load_serializes_concurrent_recovery_for_same_tenant() {
    let temp_dir = TempDir::new().unwrap();
    let tenant_id = "concurrent_recovery";
    setup_tenant_with_pending_document_recovery(temp_dir.path(), tenant_id);

    let manager = IndexManager::new(temp_dir.path());
    let barrier = Arc::new(std::sync::Barrier::new(5));
    let mut handles = Vec::new();

    for _ in 0..4 {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::task::spawn_blocking(move || {
            barrier.wait();
            manager.get_or_load(tenant_id).map(|index| {
                let reader = index.reader();
                reader.searcher().num_docs()
            })
        }));
    }

    barrier.wait();

    for handle in handles {
        let load_result = handle.await.unwrap();
        assert!(
            load_result.is_ok(),
            "concurrent get_or_load should not race recovery: {:?}",
            load_result
        );
        assert_eq!(load_result.unwrap(), 1);
    }

    assert_eq!(
        crate::index::oplog::read_committed_seq(&temp_dir.path().join(tenant_id)),
        1,
        "successful recovery should advance committed_seq exactly once"
    );
    assert_eq!(
        manager.loaded_count(),
        1,
        "tenant should only be loaded once"
    );
    assert!(
        manager.get_document(tenant_id, "doc1").unwrap().is_some(),
        "recovered document should remain queryable after concurrent loads"
    );
}

#[test]
fn build_effective_search_params_errors_on_invalid_generated_facet_filter() {
    let configured_facets = std::collections::HashSet::from([String::from("genre")]);
    let effects = RuleEffects {
        generated_facet_filters: vec![GeneratedFacetFilter {
            expression: "genre:".to_string(),
            disjunctive: false,
        }],
        ..Default::default()
    };

    let result = build_effective_search_params(&SearchParamsInput {
        request_filter: None,
        request_limit: 10,
        request_offset: 0,
        request_restrict_searchable_attrs: None,
        request_optional_filter_specs: None,
        sum_or_filters_scores: false,
        exact_on_single_word_query_override: None,
        disable_exact_on_attributes_override: None,
        configured_facet_set: Some(&configured_facets),
        rule_effects: Some(&effects),
        hits_per_page_cap: None,
    });
    assert!(result.is_err());
    let err = result.err().unwrap().to_string();
    assert!(err.contains("Invalid generated automatic facet filter expression"));
}

#[test]
fn build_effective_search_params_ignores_generated_optional_facet_filter_without_faceting() {
    let configured_facets = std::collections::HashSet::from([String::from("brand")]);
    let effects = RuleEffects {
        generated_optional_facet_filters: vec![("genre".to_string(), "comedy".to_string(), 42)],
        ..Default::default()
    };

    let result = build_effective_search_params(&SearchParamsInput {
        request_filter: None,
        request_limit: 10,
        request_offset: 0,
        request_restrict_searchable_attrs: None,
        request_optional_filter_specs: None,
        sum_or_filters_scores: false,
        exact_on_single_word_query_override: None,
        disable_exact_on_attributes_override: None,
        configured_facet_set: Some(&configured_facets),
        rule_effects: Some(&effects),
        hits_per_page_cap: None,
    })
    .expect("optional facet filters on non-faceted attributes should be ignored");

    assert!(
        result.optional_filter_specs.is_none(),
        "non-configured generated optional facet filters must not be appended"
    );
}

#[test]
fn bm25_short_field_correction_factor_has_expected_directionality() {
    let avg_doc_len_tokens = 4.0;

    let short_factor = bm25_short_field_correction_factor(2, avg_doc_len_tokens);
    let avg_factor = bm25_short_field_correction_factor(4, avg_doc_len_tokens);
    let long_factor = bm25_short_field_correction_factor(6, avg_doc_len_tokens);

    assert!(
        short_factor < 1.0,
        "short docs should be penalized when lowering b"
    );
    assert!(
        (avg_factor - 1.0).abs() < 1e-6,
        "avg-length docs should keep nearly identical score"
    );
    assert!(
        long_factor > 1.0,
        "long docs should be boosted when lowering b"
    );
}

#[test]
fn typo_distance_strict_disables_prefix_shortcut_when_not_allowed() {
    assert_eq!(
        typo_distance_strict("red", "redness", true),
        0,
        "allow_prefix=true should keep existing prefix-as-zero behavior"
    );
    assert!(
        typo_distance_strict("red", "redness", false) > 0,
        "allow_prefix=false must not treat prefix-only matches as distance 0"
    );
}

#[test]
fn compute_best_attribute_index_respects_prefix_eligibility() {
    let query_terms = vec!["red".to_string(), "shoe".to_string()];
    let prefix_eligible = vec![false, true]; // prefixLast for "red shoe"
    let tokens_by_path = vec![
        (0usize, vec!["redness".to_string()]),
        (1usize, vec!["shoe".to_string()]),
    ];

    assert_eq!(
        compute_best_attribute_index(
            &query_terms,
            &tokens_by_path,
            &AttributeRankingConfig {
                prefix_eligible: &prefix_eligible,
                min_word_size_for_1_typo: 4,
                min_word_size_for_2_typos: 8,
                attribute_criteria_computed_by_min_proximity: false,
                min_proximity: 1,
                unordered_path_indexes: &std::collections::HashSet::new(),
            },
        ),
        1,
        "Non-prefix term 'red' should not match title token 'redness' under prefixLast"
    );
}

#[test]
fn compute_typo_bucket_rejects_short_word_typos() {
    let query_terms = vec!["cat".to_string()];
    let doc_tokens = vec!["cut".to_string()];
    let prefix_eligible = vec![false];

    assert_eq!(
        compute_typo_bucket_from_tokens(&query_terms, &doc_tokens, &prefix_eligible, 4, 8),
        3,
        "Length-3 terms must not be treated as typo-tolerant matches in bucket recomputation"
    );
}

#[test]
fn compute_best_attribute_index_rejects_short_word_typos() {
    let query_terms = vec!["cat".to_string()];
    let prefix_eligible = vec![false];
    let tokens_by_path = vec![
        (0usize, vec!["cut".to_string()]),
        (1usize, vec!["cat".to_string()]),
    ];

    assert_eq!(
        compute_best_attribute_index(
            &query_terms,
            &tokens_by_path,
            &AttributeRankingConfig {
                prefix_eligible: &prefix_eligible,
                min_word_size_for_1_typo: 4,
                min_word_size_for_2_typos: 8,
                attribute_criteria_computed_by_min_proximity: false,
                min_proximity: 1,
                unordered_path_indexes: &std::collections::HashSet::new(),
            },
        ),
        1,
        "Length-3 term typo in higher-priority attribute must not outrank exact match in lower attribute"
    );
}

#[test]
fn compute_best_attribute_index_preserves_unordered_attribute_priority() {
    let query_terms = vec!["apple".to_string()];
    let prefix_eligible = vec![false];
    let unordered_path_indexes = std::collections::HashSet::from([1usize, 2usize]);
    let tokens_by_path = vec![
        // Matching unordered path should keep its configured attribute priority.
        (2usize, vec!["apple".to_string()]),
        (3usize, vec!["apple".to_string()]),
    ];

    assert_eq!(
        compute_best_attribute_index(
            &query_terms,
            &tokens_by_path,
            &AttributeRankingConfig {
                prefix_eligible: &prefix_eligible,
                min_word_size_for_1_typo: 4,
                min_word_size_for_2_typos: 8,
                attribute_criteria_computed_by_min_proximity: false,
                min_proximity: 1,
                unordered_path_indexes: &unordered_path_indexes,
            },
        ),
        2,
        "unordered() must not rewrite attribute priority to the first unordered slot"
    );
}

#[test]
fn compute_best_attribute_by_proximity_single_term_preserves_raw_attribute_priority() {
    let query_terms = vec!["apple".to_string()];
    let prefix_eligible = vec![false];
    let unordered_path_indexes = std::collections::HashSet::from([1usize, 4usize]);
    let tokens_by_path = vec![
        // Matching unordered path should keep path index 4, not normalize to 1.
        (4usize, vec!["apple".to_string()]),
        (6usize, vec!["apple".to_string()]),
    ];

    assert_eq!(
        compute_best_attribute_by_proximity(
            &query_terms,
            &tokens_by_path,
            &prefix_eligible,
            1,
            &unordered_path_indexes,
        ),
        4,
        "single-term min-proximity attribute criterion must preserve unordered attribute priority"
    );
}

#[test]
fn compute_best_attribute_by_proximity_unordered_paths_ignore_position_penalty() {
    let query_terms = vec!["hello".to_string(), "world".to_string()];
    let prefix_eligible = vec![false, false];
    let unordered_path_indexes = std::collections::HashSet::from([0usize]);
    let tokens_by_path = vec![
        // Earlier unordered attribute should tie at neutral min-proximity even with a gap.
        (
            0usize,
            vec![
                "hello".to_string(),
                "alpha".to_string(),
                "world".to_string(),
            ],
        ),
        // Later ordered attribute has better literal proximity but lower attribute priority.
        (1usize, vec!["hello".to_string(), "world".to_string()]),
    ];

    assert_eq!(
        compute_best_attribute_by_proximity(
            &query_terms,
            &tokens_by_path,
            &prefix_eligible,
            1,
            &unordered_path_indexes,
        ),
        0,
        "unordered() should neutralize position penalty when attribute criteria are computed by min proximity"
    );
}

#[test]
fn searchable_attribute_duplicate_entries_do_not_change_unique_rank_weights() {
    fn weight_for_path(paths: &[String], weights: &[f32], target: &str) -> f32 {
        let path_index = paths
            .iter()
            .position(|path| path == target)
            .expect("expected path in weighted searchable paths");
        weights[path_index]
    }

    let all_searchable_paths = vec![
        "title".to_string(),
        "subtitle".to_string(),
        "body".to_string(),
    ];
    let unique_config = vec!["title".to_string(), "body".to_string()];
    let duplicate_config = vec!["title".to_string(), "title".to_string(), "body".to_string()];

    let (unique_paths, unique_weights) = super::search_phases::build_searchable_paths_with_weights(
        &all_searchable_paths,
        Some(unique_config.as_slice()),
    );
    let (duplicate_paths, duplicate_weights) =
        super::search_phases::build_searchable_paths_with_weights(
            &all_searchable_paths,
            Some(duplicate_config.as_slice()),
        );

    let unique_body_weight = weight_for_path(&unique_paths, &unique_weights, "body");
    let duplicate_body_weight = weight_for_path(&duplicate_paths, &duplicate_weights, "body");

    assert!(
        (unique_body_weight - duplicate_body_weight).abs() < f32::EPSILON,
        "duplicate configured attributes must not consume rank slots and demote later fields"
    );
}

fn make_optional_filter_test_doc(id: &str, brand: &str, color: &str) -> Document {
    let mut fields = HashMap::new();
    fields.insert("brand".to_string(), FieldValue::Text(brand.to_string()));
    fields.insert("color".to_string(), FieldValue::Text(color.to_string()));
    Document {
        id: id.to_string(),
        fields,
    }
}

#[test]
fn optional_filter_score_max_per_group_default() {
    let doc = make_optional_filter_test_doc("d1", "Apple", "Red");
    let groups = vec![
        vec![
            ("brand".to_string(), "Apple".to_string(), 2.0),
            ("color".to_string(), "Red".to_string(), 2.0),
        ],
        vec![("color".to_string(), "Green".to_string(), 3.0)],
    ];
    let score = compute_optional_filter_score(&doc, &groups, false);
    assert_eq!(score, 2.0);
}

#[test]
fn optional_filter_score_max_per_group_other_doc() {
    let doc = make_optional_filter_test_doc("d2", "Samsung", "Green");
    let groups = vec![
        vec![
            ("brand".to_string(), "Apple".to_string(), 2.0),
            ("color".to_string(), "Red".to_string(), 2.0),
        ],
        vec![("color".to_string(), "Green".to_string(), 3.0)],
    ];
    let score = compute_optional_filter_score(&doc, &groups, false);
    assert_eq!(score, 3.0);
}

#[test]
fn optional_filter_score_sum_mode() {
    let doc = make_optional_filter_test_doc("d1", "Apple", "Red");
    let groups = vec![
        vec![
            ("brand".to_string(), "Apple".to_string(), 2.0),
            ("color".to_string(), "Red".to_string(), 2.0),
        ],
        vec![("color".to_string(), "Green".to_string(), 3.0)],
    ];
    let score = compute_optional_filter_score(&doc, &groups, true);
    assert_eq!(score, 4.0);
}

#[test]
fn optional_filter_score_sum_mode_other_doc() {
    let doc = make_optional_filter_test_doc("d2", "Samsung", "Green");
    let groups = vec![
        vec![
            ("brand".to_string(), "Apple".to_string(), 2.0),
            ("color".to_string(), "Red".to_string(), 2.0),
        ],
        vec![("color".to_string(), "Green".to_string(), 3.0)],
    ];
    let score = compute_optional_filter_score(&doc, &groups, true);
    assert_eq!(score, 3.0);
}

#[test]
fn optional_filter_score_no_match() {
    let doc = make_optional_filter_test_doc("d3", "Nokia", "Blue");
    let groups = vec![
        vec![
            ("brand".to_string(), "Apple".to_string(), 2.0),
            ("color".to_string(), "Red".to_string(), 2.0),
        ],
        vec![("color".to_string(), "Green".to_string(), 3.0)],
    ];
    let score = compute_optional_filter_score(&doc, &groups, false);
    assert_eq!(score, 0.0);
}

#[test]
fn optional_filter_score_case_insensitive() {
    let doc = make_optional_filter_test_doc("d1", "Apple", "Red");
    let groups = vec![vec![("brand".to_string(), "apple".to_string(), 2.0)]];
    let score = compute_optional_filter_score(&doc, &groups, false);
    assert_eq!(score, 2.0);
}

#[test]
fn optional_filter_score_negative_not_clamped_to_zero() {
    // A group where the only matching filter has a negative score (e.g., from `-brand:Apple`)
    // Default (max-per-group) mode must NOT clamp negative scores to 0.0.
    let doc = make_optional_filter_test_doc("d1", "Apple", "Red");
    let groups = vec![vec![("brand".to_string(), "Apple".to_string(), -1.0)]];
    let score = compute_optional_filter_score(&doc, &groups, false);
    assert_eq!(
        score, -1.0,
        "negative score must not be clamped to 0.0 in max-per-group mode"
    );
}

#[test]
fn optional_filter_score_negative_in_sum_mode() {
    // Sum mode: negative scores should contribute negative values.
    let doc = make_optional_filter_test_doc("d1", "Apple", "Red");
    let groups = vec![
        vec![("brand".to_string(), "Apple".to_string(), -1.0)],
        vec![("color".to_string(), "Red".to_string(), 3.0)],
    ];
    let score = compute_optional_filter_score(&doc, &groups, true);
    assert_eq!(score, 2.0, "sum mode: -1.0 + 3.0 = 2.0");
}

#[test]
fn optional_filter_score_negative_mixed_group() {
    // Group with both positive and negative matching filters: max should pick the highest.
    let doc = make_optional_filter_test_doc("d1", "Apple", "Red");
    let groups = vec![vec![
        ("brand".to_string(), "Apple".to_string(), -2.0),
        ("color".to_string(), "Red".to_string(), 3.0),
    ]];
    let score = compute_optional_filter_score(&doc, &groups, false);
    assert_eq!(score, 3.0, "max of -2.0 and 3.0 should be 3.0");
}

#[test]
fn optional_filter_score_no_match_group_contributes_zero() {
    // A group with no matching filters should contribute 0.0, not affect total.
    let doc = make_optional_filter_test_doc("d1", "Apple", "Red");
    let groups = vec![
        vec![("brand".to_string(), "Samsung".to_string(), 5.0)], // no match
        vec![("color".to_string(), "Red".to_string(), 2.0)],     // matches
    ];
    let score = compute_optional_filter_score(&doc, &groups, false);
    assert_eq!(
        score, 2.0,
        "no-match group contributes 0.0, match group contributes 2.0"
    );
}

#[test]
fn parse_custom_ranking_specs_ignores_unknown_entries_and_preserves_order() {
    let settings = IndexSettings {
        custom_ranking: Some(vec![
            "desc(priority)".to_string(),
            "unknown(field)".to_string(),
            "asc(name)".to_string(),
            "desc(created_at)".to_string(),
        ]),
        ..Default::default()
    };

    let specs = parse_custom_ranking_specs(Some(&settings));

    assert_eq!(specs.len(), 3, "only asc()/desc() entries should be kept");
    assert_eq!(specs[0].field, "priority");
    assert!(!specs[0].asc, "desc() must set asc=false");
    assert_eq!(specs[1].field, "name");
    assert!(specs[1].asc, "asc() must set asc=true");
    assert_eq!(
        specs[2].field, "created_at",
        "parser must preserve input ordering for stable ranking behavior"
    );
    assert!(!specs[2].asc);
}

#[test]
fn extract_custom_ranking_value_handles_nested_numeric_text_and_missing_paths() {
    let document = Document {
        id: "d1".to_string(),
        fields: HashMap::from([
            (
                "meta".to_string(),
                FieldValue::Object(HashMap::from([
                    ("priority".to_string(), FieldValue::Text("42".to_string())),
                    ("score".to_string(), FieldValue::Float(9.5)),
                    ("label".to_string(), FieldValue::Text("XL".to_string())),
                ])),
            ),
            (
                "published_at".to_string(),
                FieldValue::Date(1_720_000_000_000),
            ),
        ]),
    };

    assert_eq!(
        extract_custom_ranking_value(&document, "meta.priority"),
        RankingSortValue::Integer(42),
        "numeric text must be parsed as integer for custom ranking comparisons"
    );
    assert_eq!(
        extract_custom_ranking_value(&document, "meta.score"),
        RankingSortValue::Float(9.5)
    );
    assert_eq!(
        extract_custom_ranking_value(&document, "meta.label"),
        RankingSortValue::Text("XL".to_string()),
        "non-numeric text must remain textual"
    );
    assert_eq!(
        extract_custom_ranking_value(&document, "published_at"),
        RankingSortValue::Integer(1_720_000_000_000),
        "dates are ranked as integer timestamps"
    );
    assert_eq!(
        extract_custom_ranking_value(&document, "meta.missing"),
        RankingSortValue::Missing
    );
    assert_eq!(
        extract_custom_ranking_value(&document, "missing.root"),
        RankingSortValue::Missing
    );
}

#[test]
fn compare_custom_values_keeps_missing_values_last_for_asc_and_desc() {
    let specs = vec![CustomRankingSpec {
        field: "priority".to_string(),
        asc: false,
    }];

    let missing = vec![RankingSortValue::Missing];
    let present = vec![RankingSortValue::Integer(10)];

    assert_eq!(
        compare_custom_values(&missing, &present, &specs),
        Ordering::Greater,
        "missing value must rank after present value even for desc()"
    );
    assert_eq!(
        compare_custom_values(&present, &missing, &specs),
        Ordering::Less,
        "present value must rank before missing value even for desc()"
    );

    let asc_specs = vec![CustomRankingSpec {
        field: "priority".to_string(),
        asc: true,
    }];
    assert_eq!(
        compare_custom_values(&missing, &present, &asc_specs),
        Ordering::Greater,
        "missing value must also rank after present value for asc()"
    );
}

#[test]
fn optional_filter_path_matching_supports_nested_object_arrays_and_scalar_arrays() {
    let document = Document {
        id: "d1".to_string(),
        fields: HashMap::from([
            (
                "variants".to_string(),
                FieldValue::Array(vec![
                    FieldValue::Object(HashMap::from([
                        ("color".to_string(), FieldValue::Text("Red".to_string())),
                        ("size".to_string(), FieldValue::Integer(42)),
                    ])),
                    FieldValue::Object(HashMap::from([(
                        "color".to_string(),
                        FieldValue::Text("Blue".to_string()),
                    )])),
                ]),
            ),
            (
                "tags".to_string(),
                FieldValue::Array(vec![
                    FieldValue::Text("Sale".to_string()),
                    FieldValue::Text("Featured".to_string()),
                ]),
            ),
        ]),
    };

    assert!(
        doc_matches_optional_filter_spec(&document, "variants.color", "blue"),
        "array-of-object traversal should match nested string values case-insensitively"
    );
    assert!(
        doc_matches_optional_filter_spec(&document, "variants.size", "42"),
        "numeric comparisons should parse string expected values"
    );
    assert!(
        doc_matches_optional_filter_spec(&document, "tags", "sale"),
        "direct array field path should recurse into scalar arrays"
    );
    assert!(
        !doc_matches_optional_filter_spec(&document, "variants.color", "green"),
        "non-existent optional filter values must not match"
    );
}

#[test]
fn count_matched_query_words_deduplicates_query_terms() {
    let query_terms = vec![
        "red".to_string(),
        "red".to_string(),
        "shoes".to_string(),
        "shoes".to_string(),
    ];
    let doc_tokens = vec!["red".to_string(), "shoes".to_string(), "sale".to_string()];

    assert_eq!(
        count_matched_query_words(&query_terms, &doc_tokens),
        2,
        "duplicate query terms should not inflate `words` ranking criterion"
    );
}

// --- Ranking criteria utility coverage (s40 test-audit, batch 2) ---

#[test]
fn compare_ranking_sort_value_orders_same_type_correctly() {
    assert_eq!(
        compare_ranking_sort_value(
            &RankingSortValue::Integer(10),
            &RankingSortValue::Integer(20)
        ),
        Ordering::Less
    );
    assert_eq!(
        compare_ranking_sort_value(
            &RankingSortValue::Float(std::f64::consts::PI),
            &RankingSortValue::Float(std::f64::consts::E)
        ),
        Ordering::Greater
    );
    assert_eq!(
        compare_ranking_sort_value(
            &RankingSortValue::Text("apple".to_string()),
            &RankingSortValue::Text("banana".to_string())
        ),
        Ordering::Less
    );
}

#[test]
fn compare_ranking_sort_value_missing_sorts_below_all_present() {
    assert_eq!(
        compare_ranking_sort_value(&RankingSortValue::Missing, &RankingSortValue::Integer(0)),
        Ordering::Less,
        "Missing must sort below Integer in raw value comparison"
    );
    assert_eq!(
        compare_ranking_sort_value(&RankingSortValue::Missing, &RankingSortValue::Missing),
        Ordering::Equal
    );
}

#[test]
fn compare_ranking_sort_value_cross_type_ordering_is_deterministic() {
    // Integer < Float < Text (when comparing across types)
    assert_eq!(
        compare_ranking_sort_value(
            &RankingSortValue::Integer(100),
            &RankingSortValue::Float(1.0)
        ),
        Ordering::Less,
        "Integer sorts before Float in cross-type comparison"
    );
    assert_eq!(
        compare_ranking_sort_value(
            &RankingSortValue::Float(1.0),
            &RankingSortValue::Text("z".to_string())
        ),
        Ordering::Less,
        "Float sorts before Text in cross-type comparison"
    );
    assert_eq!(
        compare_ranking_sort_value(
            &RankingSortValue::Integer(100),
            &RankingSortValue::Text("a".to_string())
        ),
        Ordering::Less,
        "Integer sorts before Text in cross-type comparison"
    );
}

#[test]
fn min_distance_sorted_returns_minimum_gap_between_two_sorted_lists() {
    assert_eq!(
        min_distance_sorted(&[0, 5, 10], &[3, 7, 12]),
        2,
        "closest pair is (5,3) with distance 2"
    );
    assert_eq!(
        min_distance_sorted(&[0, 10], &[1, 11]),
        1,
        "adjacent positions yield distance 1"
    );
}

#[test]
fn min_distance_sorted_empty_input_returns_max() {
    assert_eq!(min_distance_sorted(&[], &[1, 2, 3]), u32::MAX);
    assert_eq!(min_distance_sorted(&[1], &[]), u32::MAX);
}

#[test]
fn contains_contiguous_subsequence_detects_exact_window_matches() {
    let tokens: Vec<String> = vec!["the", "red", "fox", "jumps"]
        .into_iter()
        .map(String::from)
        .collect();

    assert!(contains_contiguous_subsequence(
        &tokens,
        &["red", "fox"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    ));
    assert!(!contains_contiguous_subsequence(
        &tokens,
        &["red", "jumps"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    ));
    assert!(
        !contains_contiguous_subsequence(&tokens, &[]),
        "empty subsequence should return false"
    );
}

#[test]
fn max_allowed_typos_for_term_len_respects_thresholds() {
    // Default Algolia thresholds: 1 typo at 4 chars, 2 typos at 8 chars
    assert_eq!(max_allowed_typos_for_term_len(3, 4, 8), 0);
    assert_eq!(max_allowed_typos_for_term_len(4, 4, 8), 1);
    assert_eq!(max_allowed_typos_for_term_len(7, 4, 8), 1);
    assert_eq!(max_allowed_typos_for_term_len(8, 4, 8), 2);
    assert_eq!(max_allowed_typos_for_term_len(20, 4, 8), 2);
}

#[test]
fn str_prefix_by_chars_handles_unicode_boundaries() {
    assert_eq!(str_prefix_by_chars("hello", 3), "hel");
    assert_eq!(str_prefix_by_chars("café", 3), "caf");
    assert_eq!(str_prefix_by_chars("日本語テスト", 2), "日本");
    assert_eq!(
        str_prefix_by_chars("hi", 10),
        "hi",
        "shorter than char_count returns full string"
    );
}

#[test]
fn classify_match_distinguishes_exact_prefix_and_fuzzy() {
    let (dist, is_prefix) = classify_match("red", "red");
    assert_eq!(dist, 0);
    assert!(!is_prefix, "identical strings are exact, not prefix");

    let (dist, is_prefix) = classify_match("red", "redwood");
    assert_eq!(dist, 0);
    assert!(is_prefix, "candidate starting with query is a prefix match");

    let (dist, is_prefix) = classify_match("red", "rod");
    assert!(dist > 0);
    assert!(!is_prefix, "edit-distance match is not prefix");
}

#[test]
fn find_term_positions_exact_vs_prefix_mode() {
    let tokens: Vec<String> = vec!["apple", "app", "application", "banana"]
        .into_iter()
        .map(String::from)
        .collect();

    assert_eq!(
        find_term_positions(&tokens, "app", false),
        vec![1],
        "exact mode should only match 'app'"
    );
    assert_eq!(
        find_term_positions(&tokens, "app", true),
        vec![0, 1, 2],
        "prefix mode should match 'apple', 'app', and 'application'"
    );
}

#[test]
fn compute_prefix_eligible_modes() {
    assert_eq!(
        compute_prefix_eligible("prefixAll", 3, "red fox "),
        vec![true, true, true]
    );
    assert_eq!(
        compute_prefix_eligible("prefixNone", 3, "red fox"),
        vec![false, false, false]
    );
    assert_eq!(
        compute_prefix_eligible("prefixLast", 3, "red fox j"),
        vec![false, false, true],
        "prefixLast enables only the final term"
    );
    assert_eq!(
        compute_prefix_eligible("prefixLast", 3, "red fox j "),
        vec![false, false, false],
        "trailing space disables prefix on last term"
    );
}

// --- A4: exactOnSingleWordQuery unit tests ---

#[test]
fn exact_vs_prefix_attribute_mode_single_token_attribute_is_exact() {
    // "attribute" mode: single-word query "red" against doc with title:"Red" (1 token → exact attribute match)
    let query_terms = vec!["red".to_string()];
    let tokens_by_path = vec![(0usize, vec!["red".to_string()])];
    let searchable_paths = vec!["title".to_string()];
    let prefix_eligible = vec![true];
    let result = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &[],
        "attribute",
        &[],
    );
    assert_eq!(
        result, 0,
        "single-token attribute match should be exact (0) in 'attribute' mode"
    );
}

#[test]
fn exact_vs_prefix_attribute_mode_multi_token_attribute_is_prefix() {
    // "attribute" mode: single-word query "red" against doc with title:"Red Shoes" (2 tokens → not full attribute)
    let query_terms = vec!["red".to_string()];
    let tokens_by_path = vec![(0usize, vec!["red".to_string(), "shoes".to_string()])];
    let searchable_paths = vec!["title".to_string()];
    let prefix_eligible = vec![true];
    let result = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &[],
        "attribute",
        &[],
    );
    assert_eq!(
        result, 1,
        "multi-token attribute should not count as exact in 'attribute' mode → prefix tier (1)"
    );
}

#[test]
fn exact_vs_prefix_word_mode_any_token_match_is_exact() {
    // "word" mode: single-word query "red" — any token match is exact, including in multi-token attributes
    let query_terms = vec!["red".to_string()];
    let tokens_by_path = vec![(0usize, vec!["red".to_string(), "shoes".to_string()])];
    let searchable_paths = vec!["title".to_string()];
    let prefix_eligible = vec![true];
    let result = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &[],
        "word",
        &[],
    );
    assert_eq!(
        result, 0,
        "'word' mode: any matching token counts as exact → 0"
    );
}

#[test]
fn exact_vs_prefix_none_mode_always_exact_for_single_word() {
    // "none" mode: exact tier disabled for single-word queries → always return 0
    let query_terms = vec!["red".to_string()];
    let tokens_by_path = vec![(0usize, vec!["red".to_string(), "shoes".to_string()])];
    let searchable_paths = vec!["title".to_string()];
    let prefix_eligible = vec![true];
    let result = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &[],
        "none",
        &[],
    );
    assert_eq!(
        result, 0,
        "'none' mode disables exact distinction for single-word queries → always 0"
    );
}

#[test]
fn exact_vs_prefix_multi_word_query_unaffected_by_exact_on_single_word_setting() {
    // Multi-word query: "attribute" setting has no effect — uses word semantics
    // query "red shoes", doc has both tokens → exact for "shoes" (prefix-eligible last term)
    let query_terms = vec!["red".to_string(), "shoes".to_string()];
    let tokens_by_path = vec![(0usize, vec!["red".to_string(), "shoes".to_string()])];
    let searchable_paths = vec!["title".to_string()];
    let prefix_eligible = vec![false, true];
    let result = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &[],
        "attribute",
        &[],
    );
    assert_eq!(
        result, 0,
        "multi-word query uses word semantics regardless of exactOnSingleWordQuery"
    );
}

// --- A3: disableExactOnAttributes unit tests ---

#[test]
fn exact_vs_prefix_disable_exact_on_attributes_excludes_disabled_from_exact_check() {
    // Exact match only in disabled attribute (title), description only has prefix match → prefix tier
    let query_terms = vec!["red".to_string()];
    let tokens_by_path = vec![
        (0usize, vec!["red".to_string()]), // title (disabled) — 1 token, exact attribute match if not disabled
        (1usize, vec!["red".to_string(), "shoes".to_string()]), // description (enabled) — 2 tokens, not exact in attribute mode
    ];
    let searchable_paths = vec!["title".to_string(), "description".to_string()];
    let prefix_eligible = vec![true];
    let result = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &[],
        "attribute",
        &["title".to_string()],
    );
    assert_eq!(
        result, 1,
        "title disabled: only description counts; description has prefix-only → tier 1"
    );
}

#[test]
fn exact_vs_prefix_without_disable_title_gives_exact() {
    // Same doc, same settings, but title NOT disabled → exact via single-token title
    let query_terms = vec!["red".to_string()];
    let tokens_by_path = vec![
        (0usize, vec!["red".to_string()]),
        (1usize, vec!["red".to_string(), "shoes".to_string()]),
    ];
    let searchable_paths = vec!["title".to_string(), "description".to_string()];
    let prefix_eligible = vec![true];
    let result = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &[],
        "attribute",
        &[],
    );
    assert_eq!(
        result, 0,
        "title enabled: single-token exact match in title → exact tier (0)"
    );
}

#[test]
fn exact_vs_prefix_disabled_attr_only_match_returns_non_exact() {
    // Doc matches "red" ONLY on disabled attribute (description), title has no match
    let query_terms = vec!["red".to_string()];
    let tokens_by_path = vec![
        (0usize, vec!["blue".to_string()]), // title (enabled) — no match
        (1usize, vec!["red".to_string()]),  // description (disabled) — match
    ];
    let searchable_paths = vec!["title".to_string(), "description".to_string()];
    let prefix_eligible = vec![true];
    let result = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &[],
        "attribute",
        &["description".to_string()],
    );
    assert_eq!(
        result, 1,
        "match only on disabled attribute should NOT get exact tier credit"
    );
}

#[test]
fn alternatives_as_exact_ignore_plurals_counts_plural_as_exact() {
    let query_terms = vec!["shoe".to_string()];
    let tokens_by_path = vec![(0usize, vec!["shoes".to_string()])];
    let searchable_paths = vec!["title".to_string()];
    let prefix_eligible = vec![true];
    let plural_map = HashMap::from([(
        "shoe".to_string(),
        vec!["shoe".to_string(), "shoes".to_string()],
    )]);

    let no_alternatives = build_term_alternatives(&query_terms, &[], None, Some(&plural_map));
    let with_ignore_plurals = build_term_alternatives(
        &query_terms,
        &["ignorePlurals".to_string()],
        None,
        Some(&plural_map),
    );

    let no_alternatives_bucket = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &no_alternatives,
        "word",
        &[],
    );
    let with_ignore_plurals_bucket = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &with_ignore_plurals,
        "word",
        &[],
    );

    assert_eq!(
        no_alternatives_bucket, 1,
        "without alternativesAsExact, plural-only hit should stay non-exact"
    );
    assert_eq!(
        with_ignore_plurals_bucket, 0,
        "ignorePlurals should promote plural form to exact"
    );
}

#[test]
fn alternatives_as_exact_single_word_synonym_counts_synonym_as_exact() {
    let query_terms = vec!["trousers".to_string()];
    let tokens_by_path = vec![(0usize, vec!["pants".to_string(), "trousersly".to_string()])];
    let searchable_paths = vec!["title".to_string()];
    let prefix_eligible = vec![true];
    let mut synonym_store = SynonymStore::new();
    synonym_store.insert(Synonym::Regular {
        object_id: "syn-1".to_string(),
        synonyms: vec!["pants".to_string(), "trousers".to_string()],
    });

    let no_alternatives = build_term_alternatives(&query_terms, &[], Some(&synonym_store), None);
    let with_single_word_synonym = build_term_alternatives(
        &query_terms,
        &["singleWordSynonym".to_string()],
        Some(&synonym_store),
        None,
    );

    let no_alternatives_bucket = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &no_alternatives,
        "word",
        &[],
    );
    let with_single_word_synonym_bucket = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &with_single_word_synonym,
        "word",
        &[],
    );

    assert_eq!(
        no_alternatives_bucket, 1,
        "without alternativesAsExact, only prefix-quality signal should be non-exact"
    );
    assert_eq!(
        with_single_word_synonym_bucket, 0,
        "singleWordSynonym should promote synonym token hit to exact"
    );
}

#[test]
fn alternatives_as_exact_multi_word_synonym_counts_contiguous_sequence_as_exact() {
    let query_terms = vec!["ny".to_string()];
    let tokens_by_path = vec![(
        0usize,
        vec!["new".to_string(), "york".to_string(), "nyc".to_string()],
    )];
    let searchable_paths = vec!["title".to_string()];
    let prefix_eligible = vec![true];
    let mut synonym_store = SynonymStore::new();
    synonym_store.insert(Synonym::OneWay {
        object_id: "syn-2".to_string(),
        input: "ny".to_string(),
        synonyms: vec!["new york".to_string()],
    });

    let no_alternatives = build_term_alternatives(&query_terms, &[], Some(&synonym_store), None);
    let with_multi_word_synonym = build_term_alternatives(
        &query_terms,
        &["multiWordsSynonym".to_string()],
        Some(&synonym_store),
        None,
    );

    let no_alternatives_bucket = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &no_alternatives,
        "word",
        &[],
    );
    let with_multi_word_synonym_bucket = compute_exact_vs_prefix_bucket(
        &query_terms,
        &tokens_by_path,
        &searchable_paths,
        &prefix_eligible,
        &with_multi_word_synonym,
        "word",
        &[],
    );

    assert_eq!(
        no_alternatives_bucket, 1,
        "without alternativesAsExact, this should remain a non-exact prefix scenario"
    );
    assert_eq!(
        with_multi_word_synonym_bucket, 0,
        "multiWordsSynonym should treat contiguous 'new york' sequence as exact"
    );
}

#[test]
fn ranking_attribute_before_exact_per_algolia_default() {
    let mut all_results = vec![
        ScoredDocument {
            // Prefix-only match in higher-priority attribute (title, index 0)
            document: Document {
                id: "doc_attribute".to_string(),
                fields: HashMap::from([
                    ("title".to_string(), FieldValue::Text("reddish".to_string())),
                    (
                        "description".to_string(),
                        FieldValue::Text("blue".to_string()),
                    ),
                ]),
            },
            score: 10.0,
        },
        ScoredDocument {
            // Exact match in lower-priority attribute (description, index 1)
            document: Document {
                id: "doc_exact".to_string(),
                fields: HashMap::from([
                    ("title".to_string(), FieldValue::Text("blue".to_string())),
                    (
                        "description".to_string(),
                        FieldValue::Text("red".to_string()),
                    ),
                ]),
            },
            score: 10.0,
        },
    ];

    sort_results_with_stage2_ranking(
        &mut all_results,
        Stage2RankingContext {
            query_text: "red",
            searchable_paths: &["title".to_string(), "description".to_string()],
            settings: None,
            synonym_store: None,
            plural_map: None,
            query_type: "prefixLast",
            optional_filter_groups: None,
            sum_or_filters_scores: false,
            exact_on_single_word_query: "attribute",
            disable_exact_on_attributes: &[],
            custom_normalization: &[],
            keep_diacritics_on_characters: "",
            camel_case_attributes: &[],
            all_query_words_optional: false,
            relevancy_strictness: None,
            min_proximity: None,
        },
    );

    assert_eq!(
        all_results[0].document.id, "doc_attribute",
        "attribute criterion must outrank exact criterion (Algolia default order)"
    );
}

#[test]
fn ranking_setting_can_put_exact_before_attribute() {
    let mut all_results = vec![
        ScoredDocument {
            document: Document {
                id: "doc_attribute".to_string(),
                fields: HashMap::from([
                    ("title".to_string(), FieldValue::Text("reddish".to_string())),
                    (
                        "description".to_string(),
                        FieldValue::Text("blue".to_string()),
                    ),
                ]),
            },
            score: 10.0,
        },
        ScoredDocument {
            document: Document {
                id: "doc_exact".to_string(),
                fields: HashMap::from([
                    ("title".to_string(), FieldValue::Text("blue".to_string())),
                    (
                        "description".to_string(),
                        FieldValue::Text("red".to_string()),
                    ),
                ]),
            },
            score: 10.0,
        },
    ];
    let settings = IndexSettings {
        ranking: Some(vec![
            "typo".to_string(),
            "geo".to_string(),
            "words".to_string(),
            "filters".to_string(),
            "proximity".to_string(),
            "exact".to_string(),
            "attribute".to_string(),
            "custom".to_string(),
        ]),
        ..Default::default()
    };

    sort_results_with_stage2_ranking(
        &mut all_results,
        Stage2RankingContext {
            query_text: "red",
            searchable_paths: &["title".to_string(), "description".to_string()],
            settings: Some(&settings),
            synonym_store: None,
            plural_map: None,
            query_type: "prefixLast",
            optional_filter_groups: None,
            sum_or_filters_scores: false,
            exact_on_single_word_query: "attribute",
            disable_exact_on_attributes: &[],
            custom_normalization: &[],
            keep_diacritics_on_characters: "",
            camel_case_attributes: &[],
            all_query_words_optional: false,
            relevancy_strictness: None,
            min_proximity: None,
        },
    );

    assert_eq!(
        all_results[0].document.id, "doc_exact",
        "ranking setting should allow exact to outrank attribute when reordered"
    );
}

#[test]
fn attribute_criteria_computed_by_min_proximity_changes_attribute_winner() {
    // Keep proximity/effectively all earlier tiers tied via minProximity clamp and equal doc lengths.
    // Doc A defaults to attribute 0 (title) due first-match behavior, but has the full term pair
    // only in attribute 1 with a worse distance. Doc B matches only in attribute 1.
    let base_results = vec![
        ScoredDocument {
            document: Document {
                id: "z_doc_a".to_string(),
                fields: HashMap::from([
                    ("title".to_string(), FieldValue::Text("red".to_string())),
                    (
                        "description".to_string(),
                        FieldValue::Text("red x x x shoes".to_string()),
                    ),
                ]),
            },
            score: 10.0,
        },
        ScoredDocument {
            document: Document {
                id: "a_doc_b".to_string(),
                fields: HashMap::from([
                    ("title".to_string(), FieldValue::Text("blue".to_string())),
                    (
                        "description".to_string(),
                        FieldValue::Text("red shoes x x x".to_string()),
                    ),
                ]),
            },
            score: 10.0,
        },
    ];

    let mut default_ranked = base_results.clone();
    sort_results_with_stage2_ranking(
        &mut default_ranked,
        Stage2RankingContext {
            query_text: "red shoes",
            searchable_paths: &["title".to_string(), "description".to_string()],
            settings: None,
            synonym_store: None,
            plural_map: None,
            query_type: "prefixNone",
            optional_filter_groups: None,
            sum_or_filters_scores: false,
            exact_on_single_word_query: "attribute",
            disable_exact_on_attributes: &[],
            custom_normalization: &[],
            keep_diacritics_on_characters: "",
            camel_case_attributes: &[],
            all_query_words_optional: false,
            relevancy_strictness: None,
            min_proximity: Some(10),
        },
    );
    assert_eq!(
        default_ranked[0].document.id, "z_doc_a",
        "default behavior uses first matching attribute index (attribute 0 beats attribute 1)"
    );

    let mut min_proximity_ranked = base_results;
    let settings = IndexSettings {
        attribute_criteria_computed_by_min_proximity: Some(true),
        ..Default::default()
    };
    sort_results_with_stage2_ranking(
        &mut min_proximity_ranked,
        Stage2RankingContext {
            query_text: "red shoes",
            searchable_paths: &["title".to_string(), "description".to_string()],
            settings: Some(&settings),
            synonym_store: None,
            plural_map: None,
            query_type: "prefixNone",
            optional_filter_groups: None,
            sum_or_filters_scores: false,
            exact_on_single_word_query: "attribute",
            disable_exact_on_attributes: &[],
            custom_normalization: &[],
            keep_diacritics_on_characters: "",
            camel_case_attributes: &[],
            all_query_words_optional: false,
            relevancy_strictness: None,
            min_proximity: Some(10),
        },
    );
    assert_eq!(
        min_proximity_ranked[0].document.id, "a_doc_b",
        "min-proximity attribute mode should demote doc A's attribute-0 single-term match"
    );
}

#[test]
fn attribute_criteria_computed_by_min_proximity_single_term_no_effect() {
    let base_results = vec![
        ScoredDocument {
            document: Document {
                id: "doc_a".to_string(),
                fields: HashMap::from([
                    ("title".to_string(), FieldValue::Text("red".to_string())),
                    (
                        "description".to_string(),
                        FieldValue::Text("blue".to_string()),
                    ),
                ]),
            },
            score: 10.0,
        },
        ScoredDocument {
            document: Document {
                id: "doc_b".to_string(),
                fields: HashMap::from([
                    ("title".to_string(), FieldValue::Text("blue".to_string())),
                    (
                        "description".to_string(),
                        FieldValue::Text("red".to_string()),
                    ),
                ]),
            },
            score: 10.0,
        },
    ];

    let mut default_ranked = base_results.clone();
    sort_results_with_stage2_ranking(
        &mut default_ranked,
        Stage2RankingContext {
            query_text: "red",
            searchable_paths: &["title".to_string(), "description".to_string()],
            settings: None,
            synonym_store: None,
            plural_map: None,
            query_type: "prefixNone",
            optional_filter_groups: None,
            sum_or_filters_scores: false,
            exact_on_single_word_query: "attribute",
            disable_exact_on_attributes: &[],
            custom_normalization: &[],
            keep_diacritics_on_characters: "",
            camel_case_attributes: &[],
            all_query_words_optional: false,
            relevancy_strictness: None,
            min_proximity: None,
        },
    );

    let settings = IndexSettings {
        attribute_criteria_computed_by_min_proximity: Some(true),
        ..Default::default()
    };
    let mut min_proximity_ranked = base_results;
    sort_results_with_stage2_ranking(
        &mut min_proximity_ranked,
        Stage2RankingContext {
            query_text: "red",
            searchable_paths: &["title".to_string(), "description".to_string()],
            settings: Some(&settings),
            synonym_store: None,
            plural_map: None,
            query_type: "prefixNone",
            optional_filter_groups: None,
            sum_or_filters_scores: false,
            exact_on_single_word_query: "attribute",
            disable_exact_on_attributes: &[],
            custom_normalization: &[],
            keep_diacritics_on_characters: "",
            camel_case_attributes: &[],
            all_query_words_optional: false,
            relevancy_strictness: None,
            min_proximity: None,
        },
    );

    assert_eq!(
        default_ranked
            .iter()
            .map(|doc| doc.document.id.as_str())
            .collect::<Vec<_>>(),
        min_proximity_ranked
            .iter()
            .map(|doc| doc.document.id.as_str())
            .collect::<Vec<_>>(),
        "single-term queries should not change attribute ordering under min-proximity mode"
    );
}

#[test]
fn sort_results_with_stage2_ranking_filters_below_relevancy_strictness_threshold() {
    let mut all_results = vec![
        ScoredDocument {
            document: Document {
                id: "high_relevance".to_string(),
                fields: HashMap::from([
                    ("title".to_string(), FieldValue::Text("foo".to_string())),
                    ("priority".to_string(), FieldValue::Integer(100)),
                ]),
            },
            score: 10.0,
        },
        ScoredDocument {
            document: Document {
                id: "mid_relevance".to_string(),
                fields: HashMap::from([
                    ("title".to_string(), FieldValue::Text("foo".to_string())),
                    ("priority".to_string(), FieldValue::Integer(50)),
                ]),
            },
            score: 7.0,
        },
        ScoredDocument {
            document: Document {
                id: "low_relevance".to_string(),
                fields: HashMap::from([
                    ("title".to_string(), FieldValue::Text("foo".to_string())),
                    ("priority".to_string(), FieldValue::Integer(10)),
                ]),
            },
            score: 1.0,
        },
    ];

    let settings = IndexSettings {
        custom_ranking: Some(vec!["desc(priority)".to_string()]),
        ..Default::default()
    };

    sort_results_with_stage2_ranking(
        &mut all_results,
        Stage2RankingContext {
            query_text: "foo",
            searchable_paths: &["title".to_string()],
            settings: Some(&settings),
            synonym_store: None,
            plural_map: None,
            query_type: "attribute",
            optional_filter_groups: None,
            sum_or_filters_scores: false,
            exact_on_single_word_query: "attribute",
            disable_exact_on_attributes: &[],
            custom_normalization: &[],
            keep_diacritics_on_characters: "",
            camel_case_attributes: &[],
            all_query_words_optional: false,
            relevancy_strictness: Some(50),
            min_proximity: None,
        },
    );

    assert_eq!(
        all_results.len(),
        2,
        "relevancyStrictness=50 should filter out low-scoring docs"
    );
    assert_eq!(
        all_results[0].document.id, "high_relevance",
        "highest scoring/priority doc should remain first"
    );
    assert_eq!(
        all_results[1].document.id, "mid_relevance",
        "remaining docs should stay sorted by custom ranking"
    );
}

#[test]
fn proximity_two_word_query_closer_doc_ranks_first() {
    let mut all_results = vec![
        ScoredDocument {
            document: Document {
                id: "far".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("red big leather shoes".to_string()),
                )]),
            },
            score: 10.0,
        },
        ScoredDocument {
            document: Document {
                id: "close".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("red shoes".to_string()),
                )]),
            },
            score: 10.0,
        },
    ];

    sort_results_with_stage2_ranking(
        &mut all_results,
        Stage2RankingContext {
            query_text: "red shoes",
            searchable_paths: &["title".to_string()],
            settings: None,
            synonym_store: None,
            plural_map: None,
            query_type: "prefixNone",
            optional_filter_groups: None,
            sum_or_filters_scores: false,
            exact_on_single_word_query: "attribute",
            disable_exact_on_attributes: &[],
            custom_normalization: &[],
            keep_diacritics_on_characters: "",
            camel_case_attributes: &[],
            all_query_words_optional: false,
            relevancy_strictness: None,
            min_proximity: None,
        },
    );

    assert_eq!(
        all_results[0].document.id, "close",
        "closer proximity (adjacent terms) must rank before farther proximity"
    );
}

#[test]
fn proximity_single_term_query_both_docs_equal_bucket() {
    // Both docs have the same structure/length so BM25 scores are equal
    let mut all_results = vec![
        ScoredDocument {
            document: Document {
                id: "doc_b".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("shoes blue".to_string()),
                )]),
            },
            score: 10.0,
        },
        ScoredDocument {
            document: Document {
                id: "doc_a".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("shoes pink".to_string()),
                )]),
            },
            score: 10.0,
        },
    ];

    sort_results_with_stage2_ranking(
        &mut all_results,
        Stage2RankingContext {
            query_text: "shoes",
            searchable_paths: &["title".to_string()],
            settings: None,
            synonym_store: None,
            plural_map: None,
            query_type: "prefixNone",
            optional_filter_groups: None,
            sum_or_filters_scores: false,
            exact_on_single_word_query: "attribute",
            disable_exact_on_attributes: &[],
            custom_normalization: &[],
            keep_diacritics_on_characters: "",
            camel_case_attributes: &[],
            all_query_words_optional: false,
            relevancy_strictness: None,
            min_proximity: None,
        },
    );

    // Single-term: proximity bucket = 0 for both. Falls through to doc_id tiebreaker.
    assert_eq!(
        all_results[0].document.id, "doc_a",
        "single-term query: proximity is 0 for both, should fall through to tiebreaker"
    );
}

#[test]
fn proximity_three_term_query_sum_of_adjacent_pairs() {
    // Query "a b c"
    // Doc1: "a b x x x x c" → dist(a,b)=1, dist(b,c)=5 → sum=6
    // Doc2: "a x x b x c"   → dist(a,b)=3, dist(b,c)=2 → sum=5
    // Doc2 should rank first (lower sum)
    let mut all_results = vec![
        ScoredDocument {
            document: Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("a b x x x x c".to_string()),
                )]),
            },
            score: 10.0,
        },
        ScoredDocument {
            document: Document {
                id: "doc2".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("a x x b x c".to_string()),
                )]),
            },
            score: 10.0,
        },
    ];

    sort_results_with_stage2_ranking(
        &mut all_results,
        Stage2RankingContext {
            query_text: "a b c",
            searchable_paths: &["title".to_string()],
            settings: None,
            synonym_store: None,
            plural_map: None,
            query_type: "prefixNone",
            optional_filter_groups: None,
            sum_or_filters_scores: false,
            exact_on_single_word_query: "attribute",
            disable_exact_on_attributes: &[],
            custom_normalization: &[],
            keep_diacritics_on_characters: "",
            camel_case_attributes: &[],
            all_query_words_optional: false,
            relevancy_strictness: None,
            min_proximity: None,
        },
    );

    assert_eq!(
        all_results[0].document.id, "doc2",
        "three-term query: sum of adjacent-pair distances should determine ordering (5 < 6)"
    );
}

#[test]
fn proximity_min_proximity_clamps_pair_distances() {
    // With minProximity=3:
    // All docs have 5 tokens to equalize BM25 scores.
    // Doc1: "red shoes x x x"     → raw dist=1, clamped to 3 → sum=3
    // Doc2: "red x shoes x x"     → raw dist=2, clamped to 3 → sum=3 (tied with doc1)
    // Doc3: "red x x x shoes"     → raw dist=4, stays 4      → sum=4 (ranks last)
    let mut all_results = vec![
        ScoredDocument {
            document: Document {
                id: "doc3_far".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("red x x x shoes".to_string()),
                )]),
            },
            score: 10.0,
        },
        ScoredDocument {
            document: Document {
                id: "doc1_close".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("red shoes x x x".to_string()),
                )]),
            },
            score: 10.0,
        },
        ScoredDocument {
            document: Document {
                id: "doc2_medium".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("red x shoes x x".to_string()),
                )]),
            },
            score: 10.0,
        },
    ];

    sort_results_with_stage2_ranking(
        &mut all_results,
        Stage2RankingContext {
            query_text: "red shoes",
            searchable_paths: &["title".to_string()],
            settings: None,
            synonym_store: None,
            plural_map: None,
            query_type: "prefixNone",
            optional_filter_groups: None,
            sum_or_filters_scores: false,
            exact_on_single_word_query: "attribute",
            disable_exact_on_attributes: &[],
            custom_normalization: &[],
            keep_diacritics_on_characters: "",
            camel_case_attributes: &[],
            all_query_words_optional: false,
            relevancy_strictness: None,
            min_proximity: Some(3),
        },
    );

    // doc1 and doc2 should be tied (both clamped to 3) — tiebroken by doc_id
    assert_eq!(
        all_results[0].document.id, "doc1_close",
        "minProximity=3: docs with raw dist 1 and 2 both clamp to 3, tiebroken by id"
    );
    assert_eq!(
        all_results[1].document.id, "doc2_medium",
        "minProximity=3: doc2 also clamped to 3, tied with doc1"
    );
    assert_eq!(
        all_results[2].document.id, "doc3_far",
        "minProximity=3: doc3 has raw dist 4 > 3, ranks last"
    );
}

#[tokio::test]
async fn tenant_doc_count_returns_correct_count() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("t1").unwrap();

    let docs = vec![
        Document {
            id: "d1".to_string(),
            fields: HashMap::from([(
                "name".to_string(),
                crate::types::FieldValue::Text("Alice".to_string()),
            )]),
        },
        Document {
            id: "d2".to_string(),
            fields: HashMap::from([(
                "name".to_string(),
                crate::types::FieldValue::Text("Bob".to_string()),
            )]),
        },
        Document {
            id: "d3".to_string(),
            fields: HashMap::from([(
                "name".to_string(),
                crate::types::FieldValue::Text("Carol".to_string()),
            )]),
        },
    ];
    manager.add_documents_sync("t1", docs).await.unwrap();

    let count = manager.tenant_doc_count("t1");
    assert_eq!(count, Some(3), "should have 3 docs after adding 3");
}

#[tokio::test]
async fn tenant_doc_count_returns_none_for_unloaded() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    assert_eq!(manager.tenant_doc_count("nonexistent"), None);
}

#[tokio::test]
async fn loaded_tenant_ids_returns_correct_ids() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("alpha").unwrap();
    manager.create_tenant("beta").unwrap();

    let mut ids = manager.loaded_tenant_ids();
    ids.sort();
    assert_eq!(ids, vec!["alpha", "beta"]);
}

#[tokio::test]
async fn loaded_tenant_ids_empty_when_no_tenants() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    assert!(manager.loaded_tenant_ids().is_empty());
}

#[tokio::test]
async fn all_tenant_oplog_seqs_returns_seqs_after_writes() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("t1").unwrap();

    let docs = vec![Document {
        id: "d1".to_string(),
        fields: HashMap::from([(
            "name".to_string(),
            crate::types::FieldValue::Text("Alice".to_string()),
        )]),
    }];
    manager.add_documents_sync("t1", docs).await.unwrap();

    let seqs = manager.all_tenant_oplog_seqs();
    assert!(!seqs.is_empty(), "should have at least one entry");
    let (tid, seq) = &seqs[0];
    assert_eq!(tid, "t1");
    assert!(*seq > 0, "seq should be > 0 after a write");
}

// ── Vector index store tests (6.11) ──

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_index_store_and_retrieve() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    manager.set_vector_index("tenant1", vi);

    let retrieved = manager.get_vector_index("tenant1");
    assert!(retrieved.is_some());
    let lock = retrieved.unwrap();
    let guard = lock.read().unwrap();
    assert_eq!(guard.dimensions(), 3);
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_index_missing_returns_none() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    assert!(manager.get_vector_index("nonexistent").is_none());
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_index_search_through_manager() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.add("doc2", &[0.0, 1.0, 0.0]).unwrap();
    vi.add("doc3", &[0.0, 0.0, 1.0]).unwrap();
    manager.set_vector_index("t1", vi);

    let lock = manager.get_vector_index("t1").unwrap();
    let guard = lock.read().unwrap();
    let results = guard.search(&[1.0, 0.0, 0.0], 2).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].doc_id, "doc1");
}

// ── Multi-tenant vector isolation test ──

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_tenant_isolation() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Tenant A: 3-dim vectors about "cats"
    let mut vi_a = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi_a.add("cat1", &[1.0, 0.0, 0.0]).unwrap();
    vi_a.add("cat2", &[0.9, 0.1, 0.0]).unwrap();
    vi_a.add("cat3", &[0.8, 0.2, 0.0]).unwrap();
    manager.set_vector_index("tenant_a", vi_a);

    // Tenant B: 3-dim vectors about "dogs" (orthogonal direction)
    let mut vi_b = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi_b.add("dog1", &[0.0, 0.0, 1.0]).unwrap();
    vi_b.add("dog2", &[0.0, 0.1, 0.9]).unwrap();
    manager.set_vector_index("tenant_b", vi_b);

    // Search tenant A — must only return tenant A's docs
    {
        let lock = manager.get_vector_index("tenant_a").unwrap();
        let guard = lock.read().unwrap();
        let results = guard.search(&[1.0, 0.0, 0.0], 10).unwrap();
        assert_eq!(results.len(), 3, "tenant_a should have exactly 3 vectors");
        for r in &results {
            assert!(
                r.doc_id.starts_with("cat"),
                "tenant_a search returned '{}' which belongs to tenant_b",
                r.doc_id
            );
        }
    }

    // Search tenant B — must only return tenant B's docs
    {
        let lock = manager.get_vector_index("tenant_b").unwrap();
        let guard = lock.read().unwrap();
        let results = guard.search(&[0.0, 0.0, 1.0], 10).unwrap();
        assert_eq!(results.len(), 2, "tenant_b should have exactly 2 vectors");
        for r in &results {
            assert!(
                r.doc_id.starts_with("dog"),
                "tenant_b search returned '{}' which belongs to tenant_a",
                r.doc_id
            );
        }
    }

    // Verify tenant C (nonexistent) returns None
    assert!(
        manager.get_vector_index("tenant_c").is_none(),
        "nonexistent tenant should return None"
    );

    // Delete tenant A's index, verify tenant B is unaffected
    manager.vector_indices.remove("tenant_a");
    assert!(manager.get_vector_index("tenant_a").is_none());
    {
        let lock = manager.get_vector_index("tenant_b").unwrap();
        let guard = lock.read().unwrap();
        assert_eq!(
            guard.len(),
            2,
            "tenant_b should be unaffected by tenant_a removal"
        );
    }
}

#[tokio::test]
async fn all_tenant_oplog_seqs_empty_when_no_oplogs() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    // Create tenant but don't write anything (no oplog created)
    manager.create_tenant("t1").unwrap();
    let seqs = manager.all_tenant_oplog_seqs();
    assert!(seqs.is_empty(), "no oplog loaded means empty result");
}

// ── Vector index load-on-open tests (8.4) ──

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_load_vector_index_on_get_or_load() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "load_vec_t";
    let tenant_path = tmp.path().join(tenant_id);

    // Create a Tantivy index on disk
    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save settings with an embedder so load_vector_index proceeds past the
    // "no embedders configured" guard (added in 8.19).
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Manually save a VectorIndex with 3 docs (no fingerprint file → backward compat load)
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.add("doc2", &[0.0, 1.0, 0.0]).unwrap();
    vi.add("doc3", &[0.0, 0.0, 1.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    // Create IndexManager and get_or_load
    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    // Verify VectorIndex was loaded from disk
    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(vi_arc.is_some(), "VectorIndex should be loaded from disk");
    let vi_arc = vi_arc.unwrap();
    let guard = vi_arc.read().unwrap();
    assert_eq!(guard.len(), 3);
    assert_eq!(guard.dimensions(), 3);

    // Verify it's searchable
    let results = guard.search(&[1.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "doc1");
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_load_no_vectors_dir_ok() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "novecdir_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    // No VectorIndex should be loaded
    assert!(
        manager.get_vector_index(tenant_id).is_none(),
        "get_vector_index should return None when no vectors/ dir exists"
    );
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_load_corrupted_vector_index_logs_warning() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "corrupt_vec_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save settings with an embedder so load_vector_index actually attempts
    // VectorIndex::load (without this it returns early at the "no embedders
    // configured" guard, making the test a false positive).
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Write garbage to id_map.json (no fingerprint → backward compat, proceeds to load)
    let vectors_dir = tenant_path.join("vectors");
    std::fs::create_dir_all(&vectors_dir).unwrap();
    std::fs::write(vectors_dir.join("id_map.json"), "not valid json!!!").unwrap();

    let manager = IndexManager::new(tmp.path());
    // Should not error — gracefully skip corrupted vectors
    manager.get_or_load(tenant_id).unwrap();

    // VectorIndex should not be loaded
    assert!(
        manager.get_vector_index(tenant_id).is_none(),
        "corrupted vector index should not be loaded"
    );
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_create_tenant_loads_existing_vectors() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "create_load_t";
    let tenant_path = tmp.path().join(tenant_id);

    // Create tenant dir with Tantivy index
    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save settings with an embedder so load_vector_index proceeds past the
    // "no embedders configured" guard (added in 8.19).
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Save VectorIndex (no fingerprint file → backward compat load)
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.add("doc2", &[0.0, 1.0, 0.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();

    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(
        vi_arc.is_some(),
        "VectorIndex should be loaded on create_tenant"
    );
    let vi_arc = vi_arc.unwrap();
    let guard = vi_arc.read().unwrap();
    assert_eq!(guard.len(), 2);
}

// ── Vector recovery from oplog tests (8.10) ──

/// Helper: create a tenant dir with a Tantivy index and an oplog, then write oplog entries
/// with `_vectors` in the body. Returns the tenant path.
#[cfg(feature = "vector-search")]
fn setup_tenant_with_oplog_vectors(
    base_path: &Path,
    tenant_id: &str,
    ops: &[(String, serde_json::Value)],
) -> PathBuf {
    let tenant_path = base_path.join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();

    // Create a Tantivy index
    let schema = crate::index::schema::Schema::builder().build();
    let _ = crate::index::Index::create(&tenant_path, schema).unwrap();

    // Write default settings
    let settings = crate::index::settings::IndexSettings::default();
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Create oplog and write entries
    let oplog_dir = tenant_path.join("oplog");
    let oplog = OpLog::open(&oplog_dir, tenant_id, "test_node").unwrap();
    oplog.append_batch(ops).unwrap();

    // Write committed_seq=0 to force full replay
    std::fs::write(tenant_path.join("committed_seq"), "0").unwrap();

    tenant_path
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_vectors_from_oplog() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_vec_t";

    let ops = vec![
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc1",
                "body": {
                    "objectID": "doc1",
                    "title": "first",
                    "_vectors": {"default": [1.0, 0.0, 0.0]}
                }
            }),
        ),
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc2",
                "body": {
                    "objectID": "doc2",
                    "title": "second",
                    "_vectors": {"default": [0.0, 1.0, 0.0]}
                }
            }),
        ),
    ];

    setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    // Verify VectorIndex was rebuilt from oplog
    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(vi_arc.is_some(), "VectorIndex should be rebuilt from oplog");
    let vi_arc = vi_arc.unwrap();
    let guard = vi_arc.read().unwrap();
    assert_eq!(guard.len(), 2);

    let results = guard.search(&[1.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "doc1");
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_vectors_with_deletes() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_del_t";

    let ops = vec![
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc1",
                "body": {
                    "objectID": "doc1",
                    "title": "first",
                    "_vectors": {"default": [1.0, 0.0, 0.0]}
                }
            }),
        ),
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc2",
                "body": {
                    "objectID": "doc2",
                    "title": "second",
                    "_vectors": {"default": [0.0, 1.0, 0.0]}
                }
            }),
        ),
        (
            "delete".to_string(),
            serde_json::json!({"objectID": "doc1"}),
        ),
    ];

    setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(vi_arc.is_some(), "VectorIndex should exist after recovery");
    let vi_lock = vi_arc.unwrap();
    let guard = vi_lock.read().unwrap();
    assert_eq!(guard.len(), 1, "only doc2 should remain after delete");

    let results = guard.search(&[0.0, 1.0, 0.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "doc2");
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_no_vectors_in_old_oplog() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_novec_t";

    // Oplog entries without _vectors (pre-stage-8 format)
    let ops = vec![(
        "upsert".to_string(),
        serde_json::json!({
            "objectID": "doc1",
            "body": {"objectID": "doc1", "title": "old format doc"}
        }),
    )];

    setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    // No VectorIndex should be created
    assert!(
        manager.get_vector_index(tenant_id).is_none(),
        "no VectorIndex when oplog has no _vectors"
    );
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_vectors_after_clear_op() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_clear_t";

    let ops = vec![
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc1",
                "body": {
                    "objectID": "doc1",
                    "title": "first",
                    "_vectors": {"default": [1.0, 0.0, 0.0]}
                }
            }),
        ),
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc2",
                "body": {
                    "objectID": "doc2",
                    "title": "second",
                    "_vectors": {"default": [0.0, 1.0, 0.0]}
                }
            }),
        ),
        ("clear".to_string(), serde_json::json!({})),
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc3",
                "body": {
                    "objectID": "doc3",
                    "title": "third",
                    "_vectors": {"default": [0.0, 0.0, 1.0]}
                }
            }),
        ),
    ];

    setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(vi_arc.is_some(), "VectorIndex should exist after recovery");
    let vi_lock = vi_arc.unwrap();
    let guard = vi_lock.read().unwrap();
    assert_eq!(guard.len(), 1, "only doc3 should exist after clear + add");

    let results = guard.search(&[0.0, 0.0, 1.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "doc3");
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_vectors_saved_to_disk() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_disk_t";

    let ops = vec![(
        "upsert".to_string(),
        serde_json::json!({
            "objectID": "doc1",
            "body": {
                "objectID": "doc1",
                "title": "first",
                "_vectors": {"default": [1.0, 0.0, 0.0]}
            }
        }),
    )];

    let tenant_path = setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    // Verify vector files were saved to disk after recovery
    let vectors_dir = tenant_path.join("vectors");
    assert!(
        vectors_dir.join("index.usearch").exists(),
        "index.usearch should be saved after recovery"
    );
    assert!(
        vectors_dir.join("id_map.json").exists(),
        "id_map.json should be saved after recovery"
    );
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_recover_vectors_upsert_same_doc_twice() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "rec_dup_t";

    // Upsert doc1 with vector A, then upsert doc1 again with vector B
    let ops = vec![
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc1",
                "body": {
                    "objectID": "doc1",
                    "title": "first version",
                    "_vectors": {"default": [1.0, 0.0, 0.0]}
                }
            }),
        ),
        (
            "upsert".to_string(),
            serde_json::json!({
                "objectID": "doc1",
                "body": {
                    "objectID": "doc1",
                    "title": "second version",
                    "_vectors": {"default": [0.0, 1.0, 0.0]}
                }
            }),
        ),
    ];

    setup_tenant_with_oplog_vectors(tmp.path(), tenant_id, &ops);

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    let vi_arc = manager.get_vector_index(tenant_id);
    assert!(vi_arc.is_some(), "VectorIndex should exist after recovery");
    let vi_lock = vi_arc.unwrap();
    let guard = vi_lock.read().unwrap();
    assert_eq!(guard.len(), 1, "re-upsert should not duplicate doc1");

    // The vector should be the SECOND one (latest wins)
    let results = guard.search(&[0.0, 1.0, 0.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "doc1");
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_load_vector_index_skips_when_already_loaded() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "skip_load_t";
    let tenant_path = tmp.path().join(tenant_id);

    // Create tenant on disk
    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save a VectorIndex with 2 docs to disk
    let mut vi_disk = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi_disk.add("disk_doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi_disk.add("disk_doc2", &[0.0, 1.0, 0.0]).unwrap();
    vi_disk.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());

    // Pre-populate vector_indices with a DIFFERENT VectorIndex (1 doc)
    let mut vi_mem = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi_mem.add("mem_doc1", &[0.0, 0.0, 1.0]).unwrap();
    manager.set_vector_index(tenant_id, vi_mem);

    // Now call get_or_load — load_vector_index should skip because already populated
    manager.get_or_load(tenant_id).unwrap();

    // Verify we still have the in-memory version (1 doc), NOT the disk version (2 docs)
    let vi_arc = manager.get_vector_index(tenant_id).unwrap();
    let guard = vi_arc.read().unwrap();
    assert_eq!(
        guard.len(),
        1,
        "should keep in-memory index, not overwrite from disk"
    );
    let results = guard.search(&[0.0, 0.0, 1.0], 1).unwrap();
    assert_eq!(results[0].doc_id, "mem_doc1");
}

#[cfg(feature = "vector-search")]
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn test_full_crash_recovery_vectors_available() {
    use crate::security::test_helpers::AllowLocalUrlsGuard;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Full hydration path through IndexSettings::load with a wiremock
    // loopback URL — opt in to the SSRF policy like an operator running a
    // local model server would. See crate::security::test_helpers for the
    // discipline behind this guard.
    let _allow_local = AllowLocalUrlsGuard::enable();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "embedding": [0.7, 0.8, 0.9]
        })))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let tenant_id = "crash_rec_t";

    // Phase 1: Create manager, add docs with embedder, let commit happen
    {
        let manager = IndexManager::new(tmp.path());
        manager.create_tenant(tenant_id).unwrap();

        // Configure embedder in settings
        let tenant_path = tmp.path().join(tenant_id);
        let settings = crate::index::settings::IndexSettings {
            embedders: Some(HashMap::from([(
                "default".to_string(),
                serde_json::json!({
                    "source": "rest",
                    "url": format!("{}/embed", server.uri()),
                    "request": {"input": "{{text}}"},
                    "response": {"embedding": "{{embedding}}"},
                    "dimensions": 3
                }),
            )])),
            ..Default::default()
        };
        settings.save(tenant_path.join("settings.json")).unwrap();

        // Add docs through write queue (which creates oplog entries)
        let docs = vec![Document {
            id: "doc1".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                crate::types::FieldValue::Text("recovery test".to_string()),
            )]),
        }];
        manager.add_documents_sync(tenant_id, docs).await.unwrap();

        // Verify vectors exist in memory
        let vi_arc = manager.get_vector_index(tenant_id);
        assert!(vi_arc.is_some(), "vectors should be in memory after add");
    }

    // Phase 2: Simulate crash — create new IndexManager
    {
        let manager2 = IndexManager::new(tmp.path());
        manager2.get_or_load(tenant_id).unwrap();

        // Vectors should be loaded from disk (saved after commit)
        let vi_arc = manager2.get_vector_index(tenant_id);
        assert!(
            vi_arc.is_some(),
            "vectors should survive manager restart (loaded from disk)"
        );
        let vi_lock = vi_arc.unwrap();
        let guard = vi_lock.read().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard.dimensions(), 3);
    }
}

// ── Fingerprint integration tests (8.18) ──

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_fingerprint_match_loads_vectors() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "fp_match_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save settings with a rest embedder
    // url/request/response are required by IndexSettings::load (which now
    // runs full intake-style validation at the disk-load trust boundary
    // post-Plan-B SoC split). We use an RFC 5737 TEST-NET-3 address
    // (203.0.113.0/24) so the URL is a) syntactically valid, b) never
    // routes to a real host, and c) passes the SSRF policy with the env
    // var unset — this test cares about fingerprint matching, not about
    // outbound URL policy.
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "rest",
                "model": "text-embedding-3-small",
                "url": "http://203.0.113.42/embed",
                "request": {"input": "{{text}}"},
                "response": {"embedding": "{{embedding}}"},
                "dimensions": 3
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Save VectorIndex
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    // Save matching fingerprint
    let configs = vec![(
        "default".to_string(),
        crate::vector::config::EmbedderConfig {
            source: crate::vector::config::EmbedderSource::Rest,
            model: Some("text-embedding-3-small".into()),
            dimensions: Some(3),
            ..Default::default()
        },
    )];
    let fp = crate::vector::config::EmbedderFingerprint::from_configs(&configs, 3);
    fp.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    assert!(
        manager.get_vector_index(tenant_id).is_some(),
        "vectors should load when fingerprint matches"
    );
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_fingerprint_mismatch_skips_vectors() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "fp_mismatch_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Settings with model B
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "openAi",
                "model": "text-embedding-3-large",
                "dimensions": 3,
                "apiKey": "sk-test"
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Save VectorIndex
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    // Save fingerprint with model A (MISMATCH)
    let configs = vec![(
        "default".to_string(),
        crate::vector::config::EmbedderConfig {
            source: crate::vector::config::EmbedderSource::OpenAi,
            model: Some("text-embedding-3-small".into()),
            dimensions: Some(3),
            ..Default::default()
        },
    )];
    let fp = crate::vector::config::EmbedderFingerprint::from_configs(&configs, 3);
    fp.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    assert!(
        manager.get_vector_index(tenant_id).is_none(),
        "vectors should NOT load when fingerprint mismatches (model changed)"
    );
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_no_fingerprint_file_loads_vectors_anyway() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "nofp_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Save settings with embedder. See test_fingerprint_match_loads_vectors
    // for the rationale on the TEST-NET-3 URL and missing-fields fix —
    // this test exercises the same load path through IndexSettings::load.
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "rest",
                "model": "text-embedding-3-small",
                "url": "http://203.0.113.42/embed",
                "request": {"input": "{{text}}"},
                "response": {"embedding": "{{embedding}}"},
                "dimensions": 3
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Save VectorIndex but NO fingerprint.json (backward compat)
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    assert!(
        manager.get_vector_index(tenant_id).is_some(),
        "vectors should load when no fingerprint file exists (backward compat)"
    );
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_fingerprint_mismatch_template_change_skips() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let tenant_id = "fp_tmpl_t";
    let tenant_path = tmp.path().join(tenant_id);

    std::fs::create_dir_all(&tenant_path).unwrap();
    {
        let schema = crate::index::schema::Schema::builder().build();
        let _ = crate::index::Index::create(&tenant_path, schema).unwrap();
    }

    // Settings with NEW template
    let settings = crate::index::settings::IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({
                "source": "rest",
                "model": "model-a",
                "dimensions": 3,
                "documentTemplate": "{{doc.title}}"
            }),
        )])),
        ..Default::default()
    };
    settings.save(tenant_path.join("settings.json")).unwrap();

    // Save VectorIndex
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.save(&tenant_path.join("vectors")).unwrap();

    // Save fingerprint with OLD template (MISMATCH)
    let configs = vec![(
        "default".to_string(),
        crate::vector::config::EmbedderConfig {
            source: crate::vector::config::EmbedderSource::Rest,
            model: Some("model-a".into()),
            dimensions: Some(3),
            document_template: Some("{{doc.title}} {{doc.body}}".into()),
            ..Default::default()
        },
    )];
    let fp = crate::vector::config::EmbedderFingerprint::from_configs(&configs, 3);
    fp.save(&tenant_path.join("vectors")).unwrap();

    let manager = IndexManager::new(tmp.path());
    manager.get_or_load(tenant_id).unwrap();

    assert!(
        manager.get_vector_index(tenant_id).is_none(),
        "vectors should NOT load when document_template changed"
    );
}

// ── Memory accounting tests (8.21) ──

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_memory_usage_with_indices() {
    use usearch::ffi::MetricKind;
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("mem_t").unwrap();

    // Create a VectorIndex with some vectors
    let mut vi = crate::vector::index::VectorIndex::new(3, MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.add("doc2", &[0.0, 1.0, 0.0]).unwrap();
    vi.add("doc3", &[0.0, 0.0, 1.0]).unwrap();
    manager.set_vector_index("mem_t", vi);

    let usage = manager.vector_memory_usage();
    assert!(
        usage > 0,
        "vector_memory_usage should be > 0 when vectors exist, got {}",
        usage
    );
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_vector_memory_usage_no_indices() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let usage = manager.vector_memory_usage();
    assert_eq!(usage, 0, "vector_memory_usage should be 0 with no indices");
}

// ── HTTP integration tests (8.25) ──

#[cfg(feature = "vector-search")]
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn test_vectors_survive_manager_restart() {
    use crate::security::test_helpers::AllowLocalUrlsGuard;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _allow_local = AllowLocalUrlsGuard::enable();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "embedding": [0.5, 0.6, 0.7]
        })))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let tenant_id = "restart_surv_t";

    // Phase 1: Create manager, add docs with embedder, verify vectors exist
    {
        let manager = IndexManager::new(tmp.path());
        manager.create_tenant(tenant_id).unwrap();

        let tenant_path = tmp.path().join(tenant_id);
        let settings = crate::index::settings::IndexSettings {
            embedders: Some(HashMap::from([(
                "default".to_string(),
                serde_json::json!({
                    "source": "rest",
                    "url": format!("{}/embed", server.uri()),
                    "request": {"input": "{{text}}"},
                    "response": {"embedding": "{{embedding}}"},
                    "dimensions": 3
                }),
            )])),
            ..Default::default()
        };
        settings.save(tenant_path.join("settings.json")).unwrap();

        let docs = vec![
            Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    crate::types::FieldValue::Text("alpha bravo".to_string()),
                )]),
            },
            Document {
                id: "doc2".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    crate::types::FieldValue::Text("charlie delta".to_string()),
                )]),
            },
        ];
        manager.add_documents_sync(tenant_id, docs).await.unwrap();

        // Verify vectors exist in memory
        let vi_arc = manager
            .get_vector_index(tenant_id)
            .expect("vectors should exist");
        let guard = vi_arc.read().unwrap();
        assert_eq!(guard.len(), 2, "should have 2 vectors");
        // Verify search works
        let results = guard.search(&[0.5, 0.6, 0.7], 2).unwrap();
        assert_eq!(results.len(), 2, "search should return 2 results");
    }

    // Phase 2: Restart — create new IndexManager with same base_path
    {
        let manager2 = IndexManager::new(tmp.path());
        manager2.get_or_load(tenant_id).unwrap();

        // Vectors should be loaded from disk
        let vi_arc = manager2.get_vector_index(tenant_id);
        assert!(vi_arc.is_some(), "vectors should survive manager restart");

        let vi_lock = vi_arc.unwrap();
        let guard = vi_lock.read().unwrap();
        assert_eq!(guard.len(), 2, "should still have 2 vectors after restart");
        assert_eq!(guard.dimensions(), 3);

        // Verify search still works after restart
        let results = guard.search(&[0.5, 0.6, 0.7], 2).unwrap();
        assert_eq!(
            results.len(),
            2,
            "search should return 2 results after restart"
        );
    }
}

#[cfg(feature = "vector-search")]
#[tokio::test]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn test_vectors_lost_when_embedder_model_changes() {
    use crate::security::test_helpers::AllowLocalUrlsGuard;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _allow_local = AllowLocalUrlsGuard::enable();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "embedding": [0.1, 0.2, 0.3]
        })))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let tenant_id = "model_chg_t";
    let tenant_path = tmp.path().join(tenant_id);

    // Phase 1: Add docs with model A (REST embedder)
    {
        let manager = IndexManager::new(tmp.path());
        manager.create_tenant(tenant_id).unwrap();

        let settings = crate::index::settings::IndexSettings {
            embedders: Some(HashMap::from([(
                "default".to_string(),
                serde_json::json!({
                    "source": "rest",
                    "model": "model-a",
                    "url": format!("{}/embed", server.uri()),
                    "request": {"input": "{{text}}"},
                    "response": {"embedding": "{{embedding}}"},
                    "dimensions": 3
                }),
            )])),
            ..Default::default()
        };
        settings.save(tenant_path.join("settings.json")).unwrap();

        let docs = vec![Document {
            id: "doc1".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                crate::types::FieldValue::Text("test doc".to_string()),
            )]),
        }];
        manager.add_documents_sync(tenant_id, docs).await.unwrap();

        assert!(
            manager.get_vector_index(tenant_id).is_some(),
            "vectors should exist after Phase 1"
        );
    }

    // Phase 2: Change settings to model B, restart
    {
        let settings = crate::index::settings::IndexSettings {
            embedders: Some(HashMap::from([(
                "default".to_string(),
                serde_json::json!({
                    "source": "rest",
                    "model": "model-b",
                    "url": format!("{}/embed", server.uri()),
                    "request": {"input": "{{text}}"},
                    "response": {"embedding": "{{embedding}}"},
                    "dimensions": 3
                }),
            )])),
            ..Default::default()
        };
        settings.save(tenant_path.join("settings.json")).unwrap();

        let manager2 = IndexManager::new(tmp.path());
        manager2.get_or_load(tenant_id).unwrap();

        // Vectors should NOT be loaded — fingerprint mismatch
        assert!(
            manager2.get_vector_index(tenant_id).is_none(),
            "vectors should NOT load when embedder model changes (fingerprint mismatch)"
        );
    }
}

// ── validate_index_name ──

#[test]
fn index_name_valid() {
    assert!(validate_index_name("my-index_123").is_ok());
    assert!(validate_index_name("products").is_ok());
    assert!(validate_index_name("test.v2").is_ok());
}

#[test]
fn index_name_rejects_reserved_publication_roots() {
    for reserved in [".publication", ".publication_quarantine"] {
        let err = validate_index_name(reserved).unwrap_err().to_string();
        assert!(
            err.contains("reserved publication namespace"),
            "{reserved} should explain reserved namespace rejection, got: {err}"
        );
    }

    assert!(validate_index_name(".publication_archive").is_ok());
    assert!(validate_index_name("publication").is_ok());
    assert!(validate_index_name("test.v2").is_ok());
}

#[test]
fn index_name_rejects_path_traversal() {
    assert!(validate_index_name("../etc/passwd").is_err());
    assert!(validate_index_name("..").is_err());
    assert!(validate_index_name("foo/../../bar").is_err());
    assert!(validate_index_name("foo\\bar").is_err());
}

#[test]
fn index_name_rejects_empty() {
    assert!(validate_index_name("").is_err());
}

#[test]
fn index_name_rejects_null_bytes() {
    assert!(validate_index_name("test\0name").is_err());
}

#[test]
fn index_name_rejects_too_long() {
    let long_name = "a".repeat(MAX_INDEX_NAME_BYTES + 1);
    assert!(validate_index_name(&long_name).is_err());
}

#[tokio::test]
async fn create_tenant_rejects_path_traversal() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let result = manager.create_tenant("../escape");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("path traversal"), "got: {msg}");
}

#[tokio::test]
async fn create_tenant_rejects_reserved_publication_roots_before_directory_creation() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    for reserved in [".publication", ".publication_quarantine"] {
        let err = manager.create_tenant(reserved).unwrap_err().to_string();
        assert!(
            err.contains("reserved publication namespace"),
            "{reserved} should explain reserved namespace rejection, got: {err}"
        );
        assert!(
            !tmp.path().join(reserved).exists(),
            "{reserved} must not be created as a tenant directory"
        );
    }

    manager.create_tenant("test.v2").unwrap();
    assert!(tmp.path().join("test.v2").is_dir());
}

#[tokio::test]
async fn get_or_load_rejects_reserved_publication_roots_before_loading() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    for reserved in [".publication", ".publication_quarantine"] {
        std::fs::create_dir_all(tmp.path().join(reserved)).unwrap();
        let err = match manager.get_or_load(reserved) {
            Ok(_) => panic!("{reserved} must not load as a tenant"),
            Err(err) => err.to_string(),
        };
        assert!(
            err.contains("reserved publication namespace"),
            "{reserved} should explain reserved namespace rejection, got: {err}"
        );
        assert!(
            !manager
                .loaded_tenant_ids()
                .iter()
                .any(|tenant| tenant == reserved),
            "{reserved} must not populate loaded_tenant_ids"
        );
    }
}

#[tokio::test]
async fn read_side_getters_reject_path_traversal_tenant_ids() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Create files in a sibling path that would be reachable via "../..."
    // if tenant IDs were not validated at read boundaries.
    let (escape_dir, bad_id) =
        TraversalEscapeDirGuard::new(&tmp, "escape_getters_reject_path_traversal");
    std::fs::create_dir_all(escape_dir.path()).unwrap();
    IndexSettings::default()
        .save(escape_dir.path().join("settings.json"))
        .unwrap();
    RuleStore::new()
        .save(&escape_dir.path().join("rules.json"))
        .unwrap();
    SynonymStore::new()
        .save(escape_dir.path().join("synonyms.json"))
        .unwrap();

    assert!(manager.get_settings(&bad_id).is_none());
    assert!(manager.get_rules(&bad_id).is_none());
    assert!(manager.get_synonyms(&bad_id).is_none());
}

#[tokio::test]
async fn delete_tenant_rejects_path_traversal_tenant_ids() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let (escape_dir, bad_id) =
        TraversalEscapeDirGuard::new(&tmp, "escape_delete_reject_path_traversal");
    std::fs::create_dir_all(escape_dir.path()).unwrap();

    let result = manager.delete_tenant(&bad_id).await;
    assert!(result.is_err(), "delete_tenant should reject traversal IDs");
    assert!(
        escape_dir.path().exists(),
        "path traversal must not delete sibling paths"
    );
}

#[tokio::test]
async fn import_tenant_rejects_path_traversal_tenant_ids() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let src_path = tmp.path().join("import_src");
    std::fs::create_dir_all(&src_path).unwrap();
    std::fs::write(src_path.join("settings.json"), "{}").unwrap();

    let (escape_dir, bad_id) =
        TraversalEscapeDirGuard::new(&tmp, "escape_import_reject_path_traversal");
    let result = manager.import_tenant(&bad_id, &src_path);
    assert!(result.is_err(), "import_tenant should reject traversal IDs");
    assert!(
        !escape_dir.path().exists(),
        "path traversal must not create sibling destination paths"
    );
}

#[tokio::test]
async fn export_tenant_rejects_path_traversal_tenant_ids() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let (_escape_dir, bad_id) =
        TraversalEscapeDirGuard::new(&tmp, "escape_export_reject_path_traversal");
    let result = manager.export_tenant(&bad_id, tmp.path().join("export_target"));
    assert!(result.is_err(), "export_tenant should reject traversal IDs");
}

#[tokio::test]
async fn get_or_create_oplog_rejects_path_traversal_tenant_ids() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let (escape_dir, bad_id) =
        TraversalEscapeDirGuard::new(&tmp, "escape_oplog_reject_path_traversal");
    assert!(
        manager.get_or_create_oplog(&bad_id).is_none(),
        "get_or_create_oplog should reject traversal IDs"
    );
    assert!(
        !escape_dir.path().exists(),
        "path traversal must not create sibling oplog directories"
    );
}

#[tokio::test]
async fn tenant_storage_bytes_rejects_path_traversal_tenant_ids() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let (escape_dir, bad_id) =
        TraversalEscapeDirGuard::new(&tmp, "escape_storage_reject_path_traversal");
    std::fs::create_dir_all(escape_dir.path()).unwrap();
    std::fs::write(escape_dir.path().join("marker.txt"), "leak").unwrap();

    let leaked_bytes = manager.tenant_storage_bytes(&bad_id);
    assert_eq!(
        leaked_bytes, 0,
        "tenant_storage_bytes should not read outside base path"
    );
}

// ── Custom dictionary pipeline wiring tests ─────────────────────────

fn setup_manager_with_dictionaries(tmp: &TempDir) -> Arc<IndexManager> {
    let manager = IndexManager::new(tmp.path());
    let dm = Arc::new(crate::dictionaries::manager::DictionaryManager::new(
        tmp.path(),
    ));
    manager.set_dictionary_manager(dm);
    manager
}

#[tokio::test]
async fn test_custom_stopword_removes_term_from_query() {
    let tmp = TempDir::new().unwrap();
    let manager = setup_manager_with_dictionaries(&tmp);
    manager.create_tenant("t1").unwrap();

    // Doc 1 matches only "delta", Doc 2 matches only "alpha"
    let docs = vec![
        Document {
            id: "d1".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                crate::types::FieldValue::Text("delta waves".to_string()),
            )]),
        },
        Document {
            id: "d2".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                crate::types::FieldValue::Text("alpha particles".to_string()),
            )]),
        },
    ];
    manager.add_documents_sync("t1", docs).await.unwrap();

    let en_langs = vec!["en".to_string()];
    let before = manager
        .search_with_options(
            "t1",
            "alpha delta",
            &SearchOptions {
                limit: 10,
                remove_stop_words: Some(&crate::query::stopwords::RemoveStopWordsValue::All),
                query_languages: Some(&en_langs),
                query_type: Some("prefixNone"),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        before.total, 0,
        "without custom stopword wiring, 'alpha delta' requires both terms and returns no hits"
    );

    // Add "alpha" as a custom English stopword under the search tenant "t1".
    // Also add conflicting "_default" stopword data so wrong-tenant lookup is observable.
    // BUG (RED): The search path hardcodes DEFAULT_DICTIONARY_TENANT ("_default") in
    // remove_stop_words_with_dictionary_manager (query.rs:295), so this lookup reads
    // "_default" instead of tenant "t1" until Stage 3 threads tenant_id through preprocess_query.
    let dm = manager.dictionary_manager().unwrap();
    dm.batch(
        "t1",
        crate::dictionaries::DictionaryName::Stopwords,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "sw-alpha",
                    "language": "en",
                    "word": "alpha",
                    "state": "enabled",
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();
    dm.batch(
        crate::dictionaries::DEFAULT_DICTIONARY_TENANT,
        crate::dictionaries::DictionaryName::Stopwords,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "sw-delta-default",
                    "language": "en",
                    "word": "delta",
                    "state": "enabled",
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();

    // Search "alpha delta" with removeStopWords=All.
    // "alpha" should be custom-stopped → query becomes "delta" → only d1 matches.
    // Without wiring: "alpha" is NOT a built-in stopword, so query stays "alpha delta"
    // and both docs match.
    let result = manager
        .search_with_options(
            "t1",
            "alpha delta",
            &SearchOptions {
                limit: 10,
                remove_stop_words: Some(&crate::query::stopwords::RemoveStopWordsValue::All),
                query_languages: Some(&en_langs),
                query_type: Some("prefixNone"),
                ..Default::default()
            },
        )
        .unwrap();

    // Only d1 should match — tenant "t1" stopword "alpha" should be used.
    // If "_default" leaks in, "delta" is removed instead and d2 matches.
    assert_eq!(
        result.total, 1,
        "custom stopword 'alpha' should remove it from query, leaving only 'delta'"
    );
    assert_eq!(result.documents[0].document.id, "d1");
}

/// Regression: stopwords stored under `_default` must NOT bleed into tenant "t1" searches.
/// Currently FAILS (RED) because `remove_stop_words_with_dictionary_manager` hardcodes
/// `DEFAULT_DICTIONARY_TENANT`, causing `_default` entries to apply to ALL tenants.
#[tokio::test]
async fn test_stopword_isolation_no_cross_tenant_bleed() {
    let tmp = TempDir::new().unwrap();
    let manager = setup_manager_with_dictionaries(&tmp);
    manager.create_tenant("t1").unwrap();
    manager.create_tenant("t2").unwrap();

    let t1_docs = vec![
        Document {
            id: "d1".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                crate::types::FieldValue::Text("delta waves".to_string()),
            )]),
        },
        Document {
            id: "d2".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                crate::types::FieldValue::Text("alpha particles".to_string()),
            )]),
        },
    ];
    manager.add_documents_sync("t1", t1_docs).await.unwrap();
    let t2_docs = vec![
        Document {
            id: "tenant2-d1".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                crate::types::FieldValue::Text("delta comet".to_string()),
            )]),
        },
        Document {
            id: "tenant2-d2".to_string(),
            fields: HashMap::from([(
                "title".to_string(),
                crate::types::FieldValue::Text("alpha comet".to_string()),
            )]),
        },
    ];
    manager.add_documents_sync("t2", t2_docs).await.unwrap();

    // "t1" should remove "alpha", while "t2"/"_default" remove "delta".
    // A t1 search for "alpha delta" must therefore match d1 only.
    let dm = manager.dictionary_manager().unwrap();
    dm.batch(
        "t1",
        crate::dictionaries::DictionaryName::Stopwords,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "sw-alpha",
                    "language": "en",
                    "word": "alpha",
                    "state": "enabled",
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();
    dm.batch(
        "t2",
        crate::dictionaries::DictionaryName::Stopwords,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "sw-delta-tenant2",
                    "language": "en",
                    "word": "delta",
                    "state": "enabled",
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();
    dm.batch(
        crate::dictionaries::DEFAULT_DICTIONARY_TENANT,
        crate::dictionaries::DictionaryName::Stopwords,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "sw-delta-default",
                    "language": "en",
                    "word": "delta",
                    "state": "enabled",
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();

    let en_langs = vec!["en".to_string()];
    let result = manager
        .search_with_options(
            "t1",
            "alpha delta",
            &SearchOptions {
                limit: 10,
                remove_stop_words: Some(&crate::query::stopwords::RemoveStopWordsValue::All),
                query_languages: Some(&en_langs),
                query_type: Some("prefixNone"),
                ..Default::default()
            },
        )
        .unwrap();

    // Tenant "t1" must use its own stopword set ("alpha"), so query becomes "delta"
    // and returns d1. If "_default"/other-tenant data leaks in, "delta" is removed and d2 wins.
    assert_eq!(
        result.total, 1,
        "t1 search must route stopword lookup to t1 dictionary entries"
    );
    assert_eq!(result.documents[0].document.id, "d1");
}

#[tokio::test]
async fn test_custom_plural_expands_query() {
    let tmp = TempDir::new().unwrap();
    let manager = setup_manager_with_dictionaries(&tmp);
    manager.create_tenant("t1").unwrap();

    // Add document with "cacti" (custom plural of "cactus")
    let docs = vec![Document {
        id: "d1".to_string(),
        fields: HashMap::from([(
            "title".to_string(),
            crate::types::FieldValue::Text("beautiful cacti garden".to_string()),
        )]),
    }];
    manager.add_documents_sync("t1", docs).await.unwrap();

    let en_langs = vec!["en".to_string()];
    let before = manager
        .search_with_options(
            "t1",
            "cactus",
            &SearchOptions {
                limit: 10,
                ignore_plurals: Some(&crate::query::plurals::IgnorePluralsValue::All),
                query_languages: Some(&en_langs),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        before.total, 0,
        "without a custom plural entry, 'cactus' should not match 'cacti' here"
    );

    // Add custom plural pair [cactus, cacti] under the search tenant "t1".
    // BUG (RED): The search path hardcodes DEFAULT_DICTIONARY_TENANT ("_default") in
    // build_plural_language_spec (search_phases.rs:1117), so this lookup will miss
    // the tenant-scoped entry until Stage 3 threads tenant_id through preprocess_query.
    let dm = manager.dictionary_manager().unwrap();
    dm.batch(
        "t1",
        crate::dictionaries::DictionaryName::Plurals,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "pl-cactus",
                    "language": "en",
                    "words": ["cactus", "cacti"],
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();

    // Search for "cactus" with ignorePlurals=true — should expand to also match "cacti"
    let result = manager
        .search_with_options(
            "t1",
            "cactus",
            &SearchOptions {
                limit: 10,
                ignore_plurals: Some(&crate::query::plurals::IgnorePluralsValue::All),
                query_languages: Some(&en_langs),
                ..Default::default()
            },
        )
        .unwrap();

    assert_eq!(result.total, 1);
    assert_eq!(result.documents[0].document.id, "d1");
}

/// Regression: plurals stored under `_default` must NOT expand queries for tenant "t1".
/// Currently FAILS (RED) because `build_plural_language_spec` hardcodes
/// `DEFAULT_DICTIONARY_TENANT`, causing `_default` entries to apply to ALL tenants.
#[tokio::test]
async fn test_plural_isolation_no_cross_tenant_bleed() {
    let tmp = TempDir::new().unwrap();
    let manager = setup_manager_with_dictionaries(&tmp);
    manager.create_tenant("t1").unwrap();
    manager.create_tenant("t2").unwrap();

    let t1_docs = vec![Document {
        id: "d1".to_string(),
        fields: HashMap::from([(
            "title".to_string(),
            crate::types::FieldValue::Text("beautiful cacti garden".to_string()),
        )]),
    }];
    manager.add_documents_sync("t1", t1_docs).await.unwrap();
    let t2_docs = vec![Document {
        id: "tenant2-d1".to_string(),
        fields: HashMap::from([(
            "title".to_string(),
            crate::types::FieldValue::Text("beautiful cactuses greenhouse".to_string()),
        )]),
    }];
    manager.add_documents_sync("t2", t2_docs).await.unwrap();

    // "t1" should expand cactus->cacti, while "t2"/"_default" map cactus->cactuses.
    // A t1 search must therefore resolve to d1.
    let dm = manager.dictionary_manager().unwrap();
    dm.batch(
        "t1",
        crate::dictionaries::DictionaryName::Plurals,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "pl-cactus",
                    "language": "en",
                    "words": ["cactus", "cacti"],
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();
    dm.batch(
        "t2",
        crate::dictionaries::DictionaryName::Plurals,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "pl-cactus-tenant2",
                    "language": "en",
                    "words": ["cactus", "cactuses"],
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();
    dm.batch(
        crate::dictionaries::DEFAULT_DICTIONARY_TENANT,
        crate::dictionaries::DictionaryName::Plurals,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "pl-cactus-default",
                    "language": "en",
                    "words": ["cactus", "cactuses"],
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();

    let en_langs = vec!["en".to_string()];
    let result = manager
        .search_with_options(
            "t1",
            "cactus",
            &SearchOptions {
                limit: 10,
                ignore_plurals: Some(&crate::query::plurals::IgnorePluralsValue::All),
                query_languages: Some(&en_langs),
                ..Default::default()
            },
        )
        .unwrap();

    // Tenant "t1" configured [cactus, cacti], so cactus should match d1.
    // If "_default"/other-tenant data is used instead, expansion targets "cactuses" and d1 is missed.
    assert_eq!(result.total, 1, "t1 search must use t1 plural dictionary");
    assert_eq!(result.documents[0].document.id, "d1");
}

#[cfg(feature = "decompound")]
#[tokio::test]
async fn test_custom_compound_decomposition_expands_query() {
    let tmp = TempDir::new().unwrap();
    let manager = setup_manager_with_dictionaries(&tmp);
    manager.create_tenant("t1").unwrap();

    // Use a long first component (>12 chars) so split-alternative fallback cannot split it.
    // This makes the test validate the custom compound dictionary path specifically.
    let docs = vec![Document {
        id: "d1".to_string(),
        fields: HashMap::from([(
            "title".to_string(),
            crate::types::FieldValue::Text("ein xylophonographisch fest hier".to_string()),
        )]),
    }];
    manager.add_documents_sync("t1", docs).await.unwrap();

    let de_langs = vec!["de".to_string()];
    let before = manager
        .search_full_with_stop_words_with_hits_per_page_cap(
            "t1",
            "xylophonographischfest",
            &SearchOptions {
                limit: 10,
                query_languages: Some(&de_langs),
                decompound_query: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        before.total, 0,
        "without custom decomposition, this synthetic compound should not match"
    );

    // Add custom compound decomposition under the search tenant "t1".
    // BUG (RED): The search path hardcodes DEFAULT_DICTIONARY_TENANT ("_default") in
    // build_decompound_language_spec (search_phases.rs:1236), so this lookup will miss
    // the tenant-scoped entry until Stage 3 threads tenant_id through preprocess_query.
    let dm = manager.dictionary_manager().unwrap();
    dm.batch(
        "t1",
        crate::dictionaries::DictionaryName::Compounds,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "cp-xylophonographischfest",
                    "language": "de",
                    "word": "xylophonographischfest",
                    "decomposition": ["xylophonographisch", "fest"],
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();

    // Search with decompound enabled — should expand via custom decomposition.
    let result = manager
        .search_full_with_stop_words_with_hits_per_page_cap(
            "t1",
            "xylophonographischfest",
            &SearchOptions {
                limit: 10,
                query_languages: Some(&de_langs),
                decompound_query: Some(true),
                ..Default::default()
            },
        )
        .unwrap();

    assert_eq!(result.total, 1);
    assert_eq!(result.documents[0].document.id, "d1");
}

/// Regression: decompound entries under `_default` must NOT expand queries for tenant "t1".
/// Currently FAILS (RED) because `build_decompound_language_spec` hardcodes
/// `DEFAULT_DICTIONARY_TENANT`, causing `_default` entries to apply to ALL tenants.
#[cfg(feature = "decompound")]
#[tokio::test]
async fn test_decompound_isolation_no_cross_tenant_bleed() {
    let tmp = TempDir::new().unwrap();
    let manager = setup_manager_with_dictionaries(&tmp);
    manager.create_tenant("t1").unwrap();
    manager.create_tenant("t2").unwrap();

    let t1_docs = vec![Document {
        id: "d1".to_string(),
        fields: HashMap::from([(
            "title".to_string(),
            crate::types::FieldValue::Text("ein xylophonographisch fest hier".to_string()),
        )]),
    }];
    manager.add_documents_sync("t1", t1_docs).await.unwrap();
    let t2_docs = vec![Document {
        id: "tenant2-d1".to_string(),
        fields: HashMap::from([(
            "title".to_string(),
            crate::types::FieldValue::Text("ein xylophon graphischfest dort".to_string()),
        )]),
    }];
    manager.add_documents_sync("t2", t2_docs).await.unwrap();

    // "t1" should decompose to ["xylophonographisch","fest"], while "t2"/"_default"
    // use conflicting decomposition terms that cannot match d1.
    // Disable built-in decompound for "_default" so this test isolates tenant routing.
    let dm = manager.dictionary_manager().unwrap();
    dm.batch(
        "t1",
        crate::dictionaries::DictionaryName::Compounds,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "cp-xylophonographischfest",
                    "language": "de",
                    "word": "xylophonographischfest",
                    "decomposition": ["xylophonographisch", "fest"],
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();
    dm.batch(
        "t2",
        crate::dictionaries::DictionaryName::Compounds,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "cp-xylophonographischfest-tenant2",
                    "language": "de",
                    "word": "xylophonographischfest",
                    "decomposition": ["nonsenseteil", "ohnetreffer"],
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();
    let mut default_settings = crate::dictionaries::DictionarySettings::default();
    default_settings.disable_standard_entries.insert(
        crate::dictionaries::DictionaryName::Compounds,
        [("de".to_string(), true)].into_iter().collect(),
    );
    dm.set_settings(
        crate::dictionaries::DEFAULT_DICTIONARY_TENANT,
        &default_settings,
    )
    .unwrap();
    dm.batch(
        crate::dictionaries::DEFAULT_DICTIONARY_TENANT,
        crate::dictionaries::DictionaryName::Compounds,
        &crate::dictionaries::BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![crate::dictionaries::BatchRequest {
                action: crate::dictionaries::BatchAction::AddEntry,
                body: serde_json::json!({
                    "objectID": "cp-xylophonographischfest-default",
                    "language": "de",
                    "word": "xylophonographischfest",
                    "decomposition": ["nonsenseteil", "ohnetreffer"],
                    "type": "custom"
                }),
            }],
        },
    )
    .unwrap();

    let de_langs = vec!["de".to_string()];
    let result = manager
        .search_full_with_stop_words_with_hits_per_page_cap(
            "t1",
            "xylophonographischfest",
            &SearchOptions {
                limit: 10,
                query_languages: Some(&de_langs),
                decompound_query: Some(true),
                ..Default::default()
            },
        )
        .unwrap();

    // Tenant "t1" configured the matching decomposition for d1.
    // If "_default"/other-tenant mappings are used, expansion misses d1.
    assert_eq!(
        result.total, 1,
        "t1 search must use t1 decompound dictionary"
    );
    assert_eq!(result.documents[0].document.id, "d1");
}
