use super::*;
use crate::error::FlapjackError;
use crate::index::memory::{MemoryBudget, MemoryBudgetConfig};
use once_cell::sync::Lazy;
use prometheus::{Encoder, TextEncoder};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};

const WRITE_QUEUE_BATCH_SIZE_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_BATCH_SIZE";
const WRITE_QUEUE_WRITER_IDLE_TIMEOUT_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_WRITER_IDLE_TIMEOUT_MS";
const JULY_22_TIMEOUT_RISK_PENDING_ADMISSIONS: usize = 690;
static WRITE_QUEUE_ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

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
) -> (
    WriteQueue,
    tokio::task::JoinHandle<crate::error::Result<()>>,
    Arc<dashmap::DashMap<String, TaskInfo>>,
) {
    setup_write_queue_with_index_and_overrides(
        tmp,
        tenant_id,
        index,
        WriteQueueTestOverrides::default(),
    )
}

fn setup_write_queue_with_index_and_overrides(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    index: Arc<crate::index::Index>,
    test_overrides: WriteQueueTestOverrides,
) -> (
    WriteQueue,
    tokio::task::JoinHandle<crate::error::Result<()>>,
    Arc<dashmap::DashMap<String, TaskInfo>>,
) {
    let tasks: Arc<dashmap::DashMap<String, TaskInfo>> = Arc::new(dashmap::DashMap::new());
    let facet_cache = Arc::new(dashmap::DashMap::new());
    let lww_map = Arc::new(dashmap::DashMap::new());

    #[cfg(feature = "vector-search")]
    let vector_ctx = VectorWriteContext::new(Arc::new(dashmap::DashMap::new()));
    #[cfg(not(feature = "vector-search"))]
    let vector_ctx = VectorWriteContext::new();
    let admission_store =
        Arc::new(admission::WriteAdmissionStore::open(tmp.path(), tenant_id).unwrap());

    let (tx, handle) = create_write_queue(WriteQueueContext {
        tenant_id: tenant_id.to_string(),
        index,
        tasks: Arc::clone(&tasks),
        base_path: tmp.path().to_path_buf(),
        oplog: None,
        admission_store,
        facet_cache,
        lww_map,
        vector_ctx,
        queue_metrics_id: 0,
        test_overrides,
    })
    .unwrap();

    (tx, handle, tasks)
}

/// Convenience helper: create an index in a tenant subdirectory and wire up a queue.
fn setup_write_queue(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
) -> (
    WriteQueue,
    tokio::task::JoinHandle<crate::error::Result<()>>,
    Arc<dashmap::DashMap<String, TaskInfo>>,
) {
    let tenant_path = tmp.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    setup_write_queue_with_index(tmp, tenant_id, index)
}

fn setup_write_queue_with_oplog(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
) -> (
    WriteQueue,
    tokio::task::JoinHandle<crate::error::Result<()>>,
    Arc<dashmap::DashMap<String, TaskInfo>>,
    Arc<crate::index::oplog::OpLog>,
) {
    let tenant_path = tmp.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    let tasks: Arc<dashmap::DashMap<String, TaskInfo>> = Arc::new(dashmap::DashMap::new());
    let facet_cache = Arc::new(dashmap::DashMap::new());
    let lww_map = Arc::new(dashmap::DashMap::new());
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

    let (tx, handle) = create_write_queue(WriteQueueContext {
        tenant_id: tenant_id.to_string(),
        index,
        tasks: Arc::clone(&tasks),
        base_path: tmp.path().to_path_buf(),
        oplog: Some(Arc::clone(&oplog)),
        admission_store,
        facet_cache,
        lww_map,
        vector_ctx,
        queue_metrics_id: 0,
        test_overrides: WriteQueueTestOverrides::default(),
    })
    .unwrap();

    (tx, handle, tasks, oplog)
}

fn setup_write_queue_with_budget(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    budget: Arc<MemoryBudget>,
) -> (
    Arc<crate::index::Index>,
    WriteQueue,
    tokio::task::JoinHandle<crate::error::Result<()>>,
    Arc<dashmap::DashMap<String, TaskInfo>>,
) {
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
) -> (
    Arc<crate::index::Index>,
    WriteQueue,
    tokio::task::JoinHandle<crate::error::Result<()>>,
    Arc<dashmap::DashMap<String, TaskInfo>>,
) {
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

include!("write_queue/backpressure_tests.rs");

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
}

#[tokio::test(flavor = "current_thread")]
async fn write_path_exit_gate_on_local_standard_specimen() {
    const DOCUMENT_COUNT: usize = 128;
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage7_write_path_exit_gate";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    stage_5_settings()
        .save(tmp.path().join(tenant_id).join("settings.json"))
        .unwrap();
    manager.invalidate_settings_cache(tenant_id);
    install_stage_5_geo_rule(&manager, tenant_id);

    for document in stage_5_corpus(DOCUMENT_COUNT) {
        manager
            .add_documents_durable(tenant_id, vec![document])
            .await
            .expect("every acknowledged Stage 7 write must commit durably");
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
async fn count_latency_stays_under_gate_during_writes() {
    const BULK_DOCUMENT_COUNT: usize = 4_000;
    const COUNT_LATENCY_GATE: Duration = Duration::from_millis(250);
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage7_count_latency";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    manager
        .add_documents_durable(
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
    manager
        .add_documents_durable(tenant_id, documents)
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

async fn wait_for_task_success(tasks: &dashmap::DashMap<String, TaskInfo>, task_id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
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

    let observation = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let observation = observed_segments(index.as_ref());
            if observation.live_docs == COMMIT_COUNT as u64
                && observation.live_segment_count < COMMIT_COUNT / 2
                && observation.orphan_file_set_ids.is_empty()
            {
                return observation;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("worker-owned merge owner should converge 100 commits before shutdown");

    drop(tx);
    handle.await.unwrap().unwrap();

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

    let converged = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let observation = observed_segments(index.as_ref());
            if observation.live_docs == 2
                && observation.live_segment_count == 1
                && observation.orphan_file_set_ids.is_empty()
            {
                return observation;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("worker-owned merge owner should converge consecutive commits before shutdown");

    assert_eq!(
        write_queue_counter_value(WRITE_QUEUE_WRITER_OPENS_METRIC_NAME, &tenant_id),
        1,
        "tenant worker should open exactly one writer"
    );
    assert!(
        write_queue_counter_value(WRITE_QUEUE_COMMITS_METRIC_NAME, &tenant_id) >= 2,
        "tenant worker should record at least two successful commits"
    );
    assert!(
        converged.orphan_file_set_ids.is_empty(),
        "converged index should not leave orphan file sets; got {converged:?}"
    );

    drop(tx);
    handle.await.unwrap().unwrap();
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

#[tokio::test(flavor = "current_thread")]
async fn idle_writer_eviction_releases_budget_and_allows_more_tenants() {
    let tmp = tempfile::TempDir::new().unwrap();
    let shared_budget = Arc::new(MemoryBudget::new(MemoryBudgetConfig {
        max_concurrent_writers: 1,
        ..Default::default()
    }));
    let mut queues = Vec::new();

    for tenant_number in 0..3 {
        let tenant_id = format!("idle_eviction_tenant_{tenant_number}");
        let (index, tx, handle, tasks) = setup_write_queue_with_budget_and_overrides(
            &tmp,
            &tenant_id,
            Arc::clone(&shared_budget),
            WriteQueueTestOverrides {
                batch_size: Some(1),
                writer_idle_timeout: Some(Duration::from_millis(25)),
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
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            indexed_document_count(index.as_ref()),
            1,
            "tenant {tenant_number} must retain its known-answer searchable write"
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
        batch_one,
    );
    let (_index_b, tx_b, handle_b, tasks_b) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        "yield_metric_b",
        Arc::clone(&shared_budget),
        batch_one,
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
    tokio::time::timeout(Duration::from_secs(1), async {
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
        ),
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
        batch_one,
    );
    let (index_b, tx_b, handle_b, tasks_b) = setup_write_queue_with_budget_and_overrides(
        &tmp,
        "busy_budget_b",
        Arc::clone(&shared_budget),
        batch_one,
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
    tokio::time::timeout(Duration::from_secs(1), async {
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
    tokio::time::timeout(Duration::from_secs(2), async {
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
        async move { acquire_writer_for_queue(&index, "writer_contention_tenant").await }
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
    let (tx, handle, tasks) = with_write_queue_channel_capacity_env(Some("1500"), || {
        setup_write_queue(&tmp, "burst_tenant")
    });

    // Warm up the queue using shared helpers so this regression stays on the
    // same lifecycle path as existing write-queue tests.
    let warmup_task = register_task(tasks.as_ref(), "burst_warmup", 1, 1);
    enqueue_write(
        &tx,
        warmup_task.clone(),
        vec![WriteAction::Add(text_document("warmup", "name", "warmup"))],
    )
    .await;
    wait_for_write_queue_settle().await;
    assert_task_succeeded(tasks.as_ref(), &warmup_task, 1);

    // current_thread + tight try_send loop intentionally prevents the queue
    // task from draining during this burst, so capacity behavior is deterministic.
    const REQUIRED_BURST_OPS: usize = 1_200;
    let mut burst_task_ids = Vec::with_capacity(REQUIRED_BURST_OPS);
    for i in 0..REQUIRED_BURST_OPS {
        let task_id = register_task(tasks.as_ref(), &format!("burst_task_{i}"), i as i64 + 2, 1);
        burst_task_ids.push(task_id.clone());
        let send_result = tx.try_send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Delete(format!("burst_missing_doc_{i}"))],
        });
        assert!(
            send_result.is_ok(),
            "queue filled too early at burst op {i}; expected to admit at least {REQUIRED_BURST_OPS} ops"
        );
    }

    drop(tx);
    handle.await.unwrap().unwrap();

    for task_id in burst_task_ids {
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

#[tokio::test(flavor = "current_thread")]
async fn shutdown_drains_acknowledged_writes_and_waits_for_merges() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = format!("shutdown_drain_{}", uuid::Uuid::new_v4().simple());
    let tenant_path = tmp.path().join(&tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let schema = crate::index::schema::Schema::builder().build();
    let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
    let (tx, handle, tasks) = setup_write_queue_with_index(&tmp, &tenant_id, Arc::clone(&index));
    let metric_observation_guard = writer_lifecycle::WriteQueueTenantMetrics::for_queue(&tenant_id);
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
    let metrics_text = write_queue_phase_metrics_text();
    assert!(
        metrics_text.lines().any(|line| {
            line.starts_with("flapjack_write_queue_writer_closes_total")
                && line.contains(&format!("tenant=\"{tenant_id}\""))
                && line.contains("reason=\"channel_closed\"")
                && line.ends_with(" 1")
        }),
        "shutdown must record one channel-closed writer drain; got:\n{metrics_text}"
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
    let (tx, handle, tasks) =
        setup_write_queue_with_index(&tmp, "amortization_tenant", Arc::clone(&index));

    // current_thread + tight try_send loop intentionally keeps control in this
    // task until all ops are enqueued, so timeout-driven queue draining cannot
    // interleave with this burst.
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
async fn write_phase_metrics_separate_prepare_commit_reload_lww_and_oplog() {
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
        "lww_update",
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

    finalization::append_batch_to_oplog(None, "task_none", &[], &[], tenant_id).unwrap();
    finalization::append_batch_to_oplog(Some(&oplog), "task_empty", &[], &[], tenant_id).unwrap();

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
    tokio::time::timeout(Duration::from_secs(1), async {
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
        lww_map: Arc::new(dashmap::DashMap::new()),
        #[cfg(feature = "vector-search")]
        vector_ctx: VectorWriteContext::new(Arc::new(dashmap::DashMap::new())),
        #[cfg(not(feature = "vector-search"))]
        vector_ctx: VectorWriteContext::new(),
        queue_metrics_id: tenant_metrics.queue_metrics_id(),
        test_overrides: WriteQueueTestOverrides {
            batch_size: Some(1),
            ..Default::default()
        },
    };

    run_replay_startup(&ctx, vec![record]).unwrap();

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
    let lww_map = Arc::new(dashmap::DashMap::new());
    let vector_indices: Arc<
        dashmap::DashMap<String, Arc<std::sync::RwLock<crate::vector::index::VectorIndex>>>,
    > = Arc::new(dashmap::DashMap::new());

    let vector_ctx = VectorWriteContext::new(vector_indices);
    let admission_store =
        Arc::new(admission::WriteAdmissionStore::open(tmp.path(), tenant_id).unwrap());

    let (tx, handle) = create_write_queue(WriteQueueContext {
        tenant_id: tenant_id.to_string(),
        index,
        tasks: Arc::clone(&tasks),
        base_path: tmp.path().to_path_buf(),
        oplog: None,
        admission_store,
        facet_cache,
        lww_map,
        vector_ctx,
        queue_metrics_id: 0,
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
mod auto_embed_tests {
    use super::*;
    use crate::security::test_helpers::AllowLocalUrlsGuard;
    use crate::types::FieldValue;
    use serial_test::serial;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // These tests exercise the FULL production hydration path: write a
    // settings.json with a loopback wiremock URL, then load + create
    // embedders through the write queue exactly as a tenant would at
    // runtime. `IndexSettings::load` now runs the SSOT SSRF check at the
    // disk-load trust boundary, so these tests must opt in via the same
    // FLAPJACK_AI_ALLOW_LOCAL_URLS env var an operator would set to run a
    // local model server. The `#[serial]` annotation (already present on
    // some tests in this module) is extended to the shared
    // `flapjack_outbound_url_policy` key so the env-coupled tests across
    // vector::config, security, write_queue, and manager don't race.
    //
    // Tests that construct EmbedderConfig literals and call constructors
    // directly (see vector::embedder_tests) do NOT need this guard —
    // constructors skip URL safety by design.

    type VectorIndicesMap =
        Arc<dashmap::DashMap<String, Arc<std::sync::RwLock<crate::vector::index::VectorIndex>>>>;
    type EmbedderWriteQueueSetup = (
        WriteQueue,
        tokio::task::JoinHandle<crate::error::Result<()>>,
        Arc<dashmap::DashMap<String, TaskInfo>>,
        VectorIndicesMap,
    );
    type OplogWriteQueueSetup = (
        WriteQueue,
        tokio::task::JoinHandle<crate::error::Result<()>>,
        Arc<dashmap::DashMap<String, TaskInfo>>,
        VectorIndicesMap,
        Arc<crate::index::oplog::OpLog>,
    );

    fn setup_write_queue_core(
        tmp: &tempfile::TempDir,
        tenant_id: &str,
        embedder_settings: Option<HashMap<String, serde_json::Value>>,
        oplog: Option<Arc<crate::index::oplog::OpLog>>,
    ) -> EmbedderWriteQueueSetup {
        let tenant_path = tmp.path().join(tenant_id);
        std::fs::create_dir_all(&tenant_path).unwrap();

        let settings = crate::index::settings::IndexSettings {
            embedders: embedder_settings,
            ..Default::default()
        };
        let settings_json = serde_json::to_string_pretty(&settings).unwrap();
        std::fs::write(tenant_path.join("settings.json"), settings_json).unwrap();

        let schema = crate::index::schema::Schema::builder().build();
        let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());

        let tasks: Arc<dashmap::DashMap<String, TaskInfo>> = Arc::new(dashmap::DashMap::new());
        let facet_cache = Arc::new(dashmap::DashMap::new());
        let lww_map = Arc::new(dashmap::DashMap::new());
        let vector_indices: VectorIndicesMap = Arc::new(dashmap::DashMap::new());
        let vector_ctx = VectorWriteContext::new(Arc::clone(&vector_indices));
        let admission_store =
            Arc::new(admission::WriteAdmissionStore::open(tmp.path(), tenant_id).unwrap());

        let (tx, handle) = create_write_queue(WriteQueueContext {
            tenant_id: tenant_id.to_string(),
            index,
            tasks: Arc::clone(&tasks),
            base_path: tmp.path().to_path_buf(),
            oplog,
            admission_store,
            facet_cache,
            lww_map,
            vector_ctx,
            queue_metrics_id: 0,
            test_overrides: Default::default(),
        })
        .unwrap();

        (tx, handle, tasks, vector_indices)
    }

    /// Helper to create a write queue with embedder settings (no oplog).
    fn setup_write_queue_with_embedder(
        tmp: &tempfile::TempDir,
        tenant_id: &str,
        embedder_settings: Option<HashMap<String, serde_json::Value>>,
    ) -> EmbedderWriteQueueSetup {
        setup_write_queue_core(tmp, tenant_id, embedder_settings, None)
    }

    /// Create REST embedder config JSON (single-input template).
    fn rest_embedder_config(server_uri: &str, dimensions: usize) -> serde_json::Value {
        serde_json::json!({
            "source": "rest",
            "url": format!("{}/embed", server_uri),
            "request": {"input": "{{text}}"},
            "response": {"embedding": "{{embedding}}"},
            "dimensions": dimensions
        })
    }

    /// Create batch REST embedder config JSON.
    fn rest_embedder_batch_config(server_uri: &str, dimensions: usize) -> serde_json::Value {
        serde_json::json!({
            "source": "rest",
            "url": format!("{}/embed", server_uri),
            "request": {"inputs": ["{{text}}", "{{..}}"]},
            "response": {"embeddings": ["{{embedding}}", "{{..}}"]},
            "dimensions": dimensions
        })
    }

    // ── Add/Upsert tests ──

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_auto_embed_on_add() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [0.1, 0.2, 0.3]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_with_embedder(&tmp, "embed_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "embed_add_task", 1, 1);

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("Hello vectors".to_string()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        let final_task = tasks.get(&task_id).unwrap();
        assert!(
            matches!(final_task.status, TaskStatus::Succeeded),
            "task should succeed, got: {:?}",
            final_task.status
        );

        // Verify vector index was auto-created and has the document
        assert!(
            vector_indices.contains_key("embed_t"),
            "vector index should be auto-created"
        );
        let vi_lock = vector_indices.get("embed_t").unwrap();
        let vi = vi_lock.read().unwrap();
        assert_eq!(vi.len(), 1, "vector index should have 1 document");

        let results = vi.search(&[0.1, 0.2, 0.3], 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].doc_id, "doc1");
    }

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_auto_embed_on_upsert_replaces_vector() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        use wiremock::matchers::body_string_contains;

        let server = MockServer::start().await;
        // Use body content matching to return different vectors for
        // each request — deterministic, no reliance on mock ordering.
        Mock::given(method("POST"))
            .and(body_string_contains("first version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [1.0, 0.0, 0.0]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("updated version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [0.0, 0.0, 1.0]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_with_embedder(&tmp, "upsert_t", Some(embedders));

        // Add initial doc — body contains "first version" → gets [1,0,0]
        let task1 = register_task(tasks.as_ref(), "upsert_vec_t1", 1, 1);
        tx.send(WriteOp {
            task_id: task1.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("first version".into()),
                )]),
            })],
        })
        .await
        .unwrap();

        wait_for_write_queue_settle().await;

        // Verify initial vector is [1,0,0]
        {
            let vi_lock = vector_indices.get("upsert_t").unwrap();
            let vi = vi_lock.read().unwrap();
            assert_eq!(vi.len(), 1);
            let results = vi.search(&[1.0, 0.0, 0.0], 1).unwrap();
            assert_eq!(results[0].doc_id, "doc1");
            assert!(
                results[0].distance < 0.01,
                "initial vector should be close to [1,0,0], distance={}",
                results[0].distance
            );
        }

        // Upsert same doc — body contains "updated version" → gets [0,0,1]
        let task2 = register_task(tasks.as_ref(), "upsert_vec_t2", 2, 1);
        tx.send(WriteOp {
            task_id: task2.clone(),
            actions: vec![WriteAction::Upsert(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("updated version".into()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        let vi_lock = vector_indices.get("upsert_t").unwrap();
        let vi = vi_lock.read().unwrap();
        assert_eq!(vi.len(), 1, "should still have just 1 document");

        // Vector should now be [0,0,1] — verify it actually changed
        let results = vi.search(&[0.0, 0.0, 1.0], 1).unwrap();
        assert_eq!(results[0].doc_id, "doc1");
        assert!(
            results[0].distance < 0.01,
            "upserted vector should be close to [0,0,1], distance={}",
            results[0].distance
        );
    }

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_batch_embed_multiple_docs() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embeddings": [
                    [0.1, 0.0, 0.0],
                    [0.0, 0.2, 0.0],
                    [0.0, 0.0, 0.3],
                    [0.4, 0.0, 0.0],
                    [0.0, 0.5, 0.0]
                ]
            })))
            .expect(1) // Exactly 1 HTTP request for all 5 docs
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            rest_embedder_batch_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_with_embedder(&tmp, "batch_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "batch_task", 1, 5);

        let actions: Vec<WriteAction> = (1..=5)
            .map(|i| {
                WriteAction::Add(crate::types::Document {
                    id: format!("doc{i}"),
                    fields: HashMap::from([(
                        "title".to_string(),
                        FieldValue::Text(format!("Document {i}")),
                    )]),
                })
            })
            .collect();

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions,
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        let vi_lock = vector_indices.get("batch_t").unwrap();
        let vi = vi_lock.read().unwrap();
        assert_eq!(vi.len(), 5, "all 5 docs should be in vector index");
    }

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_vector_index_auto_created() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [0.1, 0.2, 0.3]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_with_embedder(&tmp, "autocreate_t", Some(embedders));

        // No VectorIndex exists yet
        assert!(!vector_indices.contains_key("autocreate_t"));

        let task_id = register_task(tasks.as_ref(), "autocreate_task", 1, 1);

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("first doc".into()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        assert!(
            vector_indices.contains_key("autocreate_t"),
            "VectorIndex should be auto-created on first doc"
        );
        let vi_lock = vector_indices.get("autocreate_t").unwrap();
        let vi = vi_lock.read().unwrap();
        assert_eq!(vi.dimensions(), 3, "dimensions should match embedding size");
        assert_eq!(vi.len(), 1);
    }

    // ── User-provided vector tests ──

    #[tokio::test]
    async fn test_vectors_field_used_directly() {
        let server = MockServer::start().await;
        // Zero HTTP requests expected for userProvided
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        );

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_with_embedder(&tmp, "userprov_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "userprov_task", 1, 1);

        let mut fields = HashMap::new();
        fields.insert("title".to_string(), FieldValue::Text("Hello".to_string()));
        let mut vectors_map = HashMap::new();
        vectors_map.insert(
            "default".to_string(),
            FieldValue::Array(vec![
                FieldValue::Float(0.1),
                FieldValue::Float(0.2),
                FieldValue::Float(0.3),
            ]),
        );
        fields.insert("_vectors".to_string(), FieldValue::Object(vectors_map));

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields,
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // Vector should be stored directly from _vectors
        assert!(vector_indices.contains_key("userprov_t"));
        let vi_lock = vector_indices.get("userprov_t").unwrap();
        let vi = vi_lock.read().unwrap();
        assert_eq!(vi.len(), 1);
        let results = vi.search(&[0.1, 0.2, 0.3], 1).unwrap();
        assert_eq!(results[0].doc_id, "doc1");
    }

    #[tokio::test]
    async fn test_vectors_field_wrong_dimensions_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        );

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_with_embedder(&tmp, "wrongdim_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "wrongdim_task", 1, 2);

        // Good doc: correct dimensions
        let mut fields_ok = HashMap::new();
        fields_ok.insert(
            "title".to_string(),
            FieldValue::Text("Good doc".to_string()),
        );
        let mut vectors_ok = HashMap::new();
        vectors_ok.insert(
            "default".to_string(),
            FieldValue::Array(vec![
                FieldValue::Float(0.1),
                FieldValue::Float(0.2),
                FieldValue::Float(0.3),
            ]),
        );
        fields_ok.insert("_vectors".to_string(), FieldValue::Object(vectors_ok));

        // Bad doc: wrong dimensions (2 instead of 3)
        let mut fields_bad = HashMap::new();
        fields_bad.insert("title".to_string(), FieldValue::Text("Bad doc".to_string()));
        let mut vectors_bad = HashMap::new();
        vectors_bad.insert(
            "default".to_string(),
            FieldValue::Array(vec![FieldValue::Float(0.1), FieldValue::Float(0.2)]),
        );
        fields_bad.insert("_vectors".to_string(), FieldValue::Object(vectors_bad));

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![
                WriteAction::Add(crate::types::Document {
                    id: "good".to_string(),
                    fields: fields_ok,
                }),
                WriteAction::Add(crate::types::Document {
                    id: "bad".to_string(),
                    fields: fields_bad,
                }),
            ],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        let final_task = tasks.get(&task_id).unwrap();
        assert!(matches!(final_task.status, TaskStatus::Succeeded));

        // Good doc should be in vector index
        let vi_lock = vector_indices.get("wrongdim_t").unwrap();
        let vi = vi_lock.read().unwrap();
        assert_eq!(vi.len(), 1, "only good doc should be in vector index");

        // Bad doc should be rejected
        assert!(
            !final_task.rejected_documents.is_empty(),
            "bad doc should be rejected"
        );
    }

    // ── Fallback/error tests ──

    #[tokio::test]
    async fn test_no_embed_without_embedder_config() {
        let tmp = tempfile::TempDir::new().unwrap();

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_with_embedder(&tmp, "noembed_t", None);

        let task_id = register_task(tasks.as_ref(), "noembed_task", 1, 1);

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("no embedder".into()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        let final_task = tasks.get(&task_id).unwrap();
        assert!(matches!(final_task.status, TaskStatus::Succeeded));
        assert_eq!(final_task.indexed_documents, 1);

        // No VectorIndex should be created
        assert!(
            !vector_indices.contains_key("noembed_t"),
            "no vector index without embedder config"
        );
    }

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_embed_failure_does_not_block_tantivy() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        let server = MockServer::start().await;
        // Server returns 500 — embedding fails
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_with_embedder(&tmp, "fail_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "fail_task", 1, 1);

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("failing embed".into()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // Document should still be indexed in Tantivy
        let final_task = tasks.get(&task_id).unwrap();
        assert!(
            matches!(final_task.status, TaskStatus::Succeeded),
            "task should succeed despite embed failure"
        );
        assert_eq!(
            final_task.indexed_documents, 1,
            "doc should be indexed in Tantivy"
        );

        // VectorIndex should NOT have the doc
        let vi_count = vector_indices
            .get("fail_t")
            .map(|r| r.read().unwrap().len())
            .unwrap_or(0);
        assert_eq!(
            vi_count, 0,
            "vector index should be empty after embed failure"
        );
    }

    #[tokio::test]
    async fn test_user_provided_source_no_vectors_field_skipped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        );

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_with_embedder(&tmp, "novec_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "novec_task", 1, 1);

        // Document without _vectors field + userProvided source
        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("no vectors".into()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        let final_task = tasks.get(&task_id).unwrap();
        assert!(matches!(final_task.status, TaskStatus::Succeeded));
        assert_eq!(final_task.indexed_documents, 1);

        // No vector stored
        let vi_count = vector_indices
            .get("novec_t")
            .map(|r| r.read().unwrap().len())
            .unwrap_or(0);
        assert_eq!(vi_count, 0, "no vectors should be stored");
    }

    // ── Delete tests ──

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_delete_removes_from_vector_index() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [0.5, 0.5, 0.5]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_with_embedder(&tmp, "del_vec_t", Some(embedders));

        // Add a document
        let task1 = register_task(tasks.as_ref(), "del_vec_t1", 1, 1);
        tx.send(WriteOp {
            task_id: task1.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("to be deleted".into()),
                )]),
            })],
        })
        .await
        .unwrap();

        wait_for_write_queue_settle().await;

        // Delete the document
        let task2 = register_task(tasks.as_ref(), "del_vec_t2", 2, 1);
        tx.send(WriteOp {
            task_id: task2.clone(),
            actions: vec![WriteAction::Delete("doc1".to_string())],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        let vi_lock = vector_indices.get("del_vec_t").unwrap();
        let vi = vi_lock.read().unwrap();
        assert_eq!(
            vi.len(),
            0,
            "doc should be removed from vector index after delete"
        );
    }

    #[tokio::test]
    async fn test_delete_nonexistent_in_vector_index_silent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        );

        let (tx, handle, tasks, _vector_indices) =
            setup_write_queue_with_embedder(&tmp, "delnone_t", Some(embedders));

        // Delete a doc that was never added
        let task_id = register_task(tasks.as_ref(), "delnone_task", 1, 1);
        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Delete("nonexistent".to_string())],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        let final_task = tasks.get(&task_id).unwrap();
        assert!(
            matches!(final_task.status, TaskStatus::Succeeded),
            "delete should succeed even for nonexistent doc"
        );
    }

    // ── Stripping test ──

    #[tokio::test]
    async fn test_vectors_field_stripped_from_tantivy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tenant_id = "strip_t";
        let tenant_path = tmp.path().join(tenant_id);
        std::fs::create_dir_all(&tenant_path).unwrap();

        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        );
        let settings = crate::index::settings::IndexSettings {
            embedders: Some(embedders),
            ..Default::default()
        };
        std::fs::write(
            tenant_path.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        let schema = crate::index::schema::Schema::builder().build();
        let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());

        let tasks: Arc<dashmap::DashMap<String, TaskInfo>> = Arc::new(dashmap::DashMap::new());
        let facet_cache = Arc::new(dashmap::DashMap::new());
        let lww_map = Arc::new(dashmap::DashMap::new());
        let vector_indices: VectorIndicesMap = Arc::new(dashmap::DashMap::new());
        let vector_ctx = VectorWriteContext::new(Arc::clone(&vector_indices));
        let admission_store =
            Arc::new(admission::WriteAdmissionStore::open(tmp.path(), tenant_id).unwrap());

        let (tx, handle) = create_write_queue(WriteQueueContext {
            tenant_id: tenant_id.to_string(),
            index: Arc::clone(&index),
            tasks: Arc::clone(&tasks),
            base_path: tmp.path().to_path_buf(),
            oplog: None,
            admission_store,
            facet_cache,
            lww_map,
            vector_ctx,
            queue_metrics_id: 0,
            test_overrides: Default::default(),
        })
        .unwrap();

        let task_id = register_task(tasks.as_ref(), "strip_task", 1, 1);

        let mut fields = HashMap::new();
        fields.insert(
            "title".to_string(),
            FieldValue::Text("test stripping".to_string()),
        );
        let mut vectors_map = HashMap::new();
        vectors_map.insert(
            "default".to_string(),
            FieldValue::Array(vec![
                FieldValue::Float(0.1),
                FieldValue::Float(0.2),
                FieldValue::Float(0.3),
            ]),
        );
        fields.insert("_vectors".to_string(), FieldValue::Object(vectors_map));

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields,
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // Vector should be in VectorIndex
        assert!(vector_indices.contains_key(tenant_id));

        // Read back from Tantivy — _vectors should NOT be stored
        index.reader().reload().unwrap();
        let searcher = index.reader().searcher();
        let top_docs = searcher
            .search(
                &tantivy::query::AllQuery,
                &tantivy::collector::TopDocs::with_limit(10).order_by_score(),
            )
            .unwrap();
        assert_eq!(top_docs.len(), 1, "should have 1 document in Tantivy");

        let doc: tantivy::TantivyDocument = searcher.doc(top_docs[0].1).unwrap();
        let tantivy_schema = index.inner().schema();
        // Import the Document trait for to_json()
        use tantivy::schema::document::Document as TantivyDocTrait;
        let doc_json_str = doc.to_json(&tantivy_schema);
        assert!(
            !doc_json_str.contains("_vectors"),
            "_vectors should be stripped from Tantivy document, got: {doc_json_str}"
        );
    }

    // ── Vector index disk persistence tests (8.1) ──

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_vector_index_saved_after_commit() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [0.1, 0.2, 0.3]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, _vector_indices) =
            setup_write_queue_with_embedder(&tmp, "save_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "save_task", 1, 2);

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![
                WriteAction::Add(crate::types::Document {
                    id: "doc1".to_string(),
                    fields: HashMap::from([(
                        "title".to_string(),
                        FieldValue::Text("First document".to_string()),
                    )]),
                }),
                WriteAction::Add(crate::types::Document {
                    id: "doc2".to_string(),
                    fields: HashMap::from([(
                        "title".to_string(),
                        FieldValue::Text("Second document".to_string()),
                    )]),
                }),
            ],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // Verify vector files exist on disk
        let vectors_dir = tmp.path().join("save_t").join("vectors");
        assert!(
            vectors_dir.join("index.usearch").exists(),
            "index.usearch should exist on disk after commit"
        );
        assert!(
            vectors_dir.join("id_map.json").exists(),
            "id_map.json should exist on disk after commit"
        );

        // Load from disk and verify searchable with correct dimensions
        let loaded =
            crate::vector::index::VectorIndex::load(&vectors_dir, usearch::ffi::MetricKind::Cos)
                .unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.dimensions(), 3);

        let results = loaded.search(&[0.1, 0.2, 0.3], 2).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_vector_index_save_reflects_deletes() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [0.5, 0.5, 0.5]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, _vector_indices) =
            setup_write_queue_with_embedder(&tmp, "savedel_t", Some(embedders));

        // Add two docs
        let task1 = register_task(tasks.as_ref(), "savedel_t1", 1, 2);
        tx.send(WriteOp {
            task_id: task1.clone(),
            actions: vec![
                WriteAction::Add(crate::types::Document {
                    id: "doc1".to_string(),
                    fields: HashMap::from([(
                        "title".to_string(),
                        FieldValue::Text("First".to_string()),
                    )]),
                }),
                WriteAction::Add(crate::types::Document {
                    id: "doc2".to_string(),
                    fields: HashMap::from([(
                        "title".to_string(),
                        FieldValue::Text("Second".to_string()),
                    )]),
                }),
            ],
        })
        .await
        .unwrap();

        wait_for_write_queue_settle().await;

        // Delete one doc
        let task2 = register_task(tasks.as_ref(), "savedel_t2", 2, 1);
        tx.send(WriteOp {
            task_id: task2.clone(),
            actions: vec![WriteAction::Delete("doc1".to_string())],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // Load from disk and verify doc1 is not in the index
        let vectors_dir = tmp.path().join("savedel_t").join("vectors");
        let loaded =
            crate::vector::index::VectorIndex::load(&vectors_dir, usearch::ffi::MetricKind::Cos)
                .unwrap();
        assert_eq!(loaded.len(), 1, "only doc2 should remain after delete");

        let results = loaded.search(&[0.5, 0.5, 0.5], 1).unwrap();
        assert_eq!(results[0].doc_id, "doc2");
    }

    #[tokio::test]
    async fn test_vector_save_skipped_when_no_vector_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No embedder configured
        let (tx, handle, tasks, _vector_indices) =
            setup_write_queue_with_embedder(&tmp, "novec_save_t", None);
        let baseline = histogram_count(
            "flapjack_write_queue_phase_seconds",
            &[("phase", PHASE_VECTOR_SAVE)],
        );

        let task_id = register_task(tasks.as_ref(), "novec_save_task", 1, 1);

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("no vectors".into()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // No vectors/ directory should exist
        let vectors_dir = tmp.path().join("novec_save_t").join("vectors");
        assert!(
            !vectors_dir.exists(),
            "vectors/ directory should not be created without embedder"
        );
        assert_eq!(
            histogram_count(
                "flapjack_write_queue_phase_seconds",
                &[("phase", PHASE_VECTOR_SAVE)]
            ),
            baseline,
            "text-only batches must not emit vector_save phase observations"
        );
    }

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_vector_index_save_reflects_upserts() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        let server = MockServer::start().await;
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        Mock::given(method("POST"))
            .respond_with(move |_req: &wiremock::Request| {
                let n = call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // First call returns [0.1, 0.2, 0.3], second returns [0.9, 0.8, 0.7]
                let vec = if n == 0 {
                    vec![0.1, 0.2, 0.3]
                } else {
                    vec![0.9, 0.8, 0.7]
                };
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "embedding": vec
                }))
            })
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, _vector_indices) =
            setup_write_queue_with_embedder(&tmp, "upsert_save_t", Some(embedders));

        // Add doc1
        let task1 = register_task(tasks.as_ref(), "upsert_t1", 1, 1);
        tx.send(WriteOp {
            task_id: task1.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("original".to_string()),
                )]),
            })],
        })
        .await
        .unwrap();

        wait_for_write_queue_settle().await;

        // Upsert doc1 with new content (gets new embedding)
        let task2 = register_task(tasks.as_ref(), "upsert_t2", 2, 1);
        tx.send(WriteOp {
            task_id: task2.clone(),
            actions: vec![WriteAction::Upsert(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("updated".to_string()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // Load from disk and verify only 1 doc with updated vector
        let vectors_dir = tmp.path().join("upsert_save_t").join("vectors");
        let loaded =
            crate::vector::index::VectorIndex::load(&vectors_dir, usearch::ffi::MetricKind::Cos)
                .unwrap();
        assert_eq!(loaded.len(), 1, "upsert should replace, not duplicate");

        let results = loaded.search(&[0.9, 0.8, 0.7], 1).unwrap();
        assert_eq!(results[0].doc_id, "doc1");
    }

    // ── Oplog vector storage tests (8.7) ──

    fn setup_write_queue_with_oplog(
        tmp: &tempfile::TempDir,
        tenant_id: &str,
        embedder_settings: Option<HashMap<String, serde_json::Value>>,
    ) -> OplogWriteQueueSetup {
        let tenant_path = tmp.path().join(tenant_id);
        std::fs::create_dir_all(&tenant_path).unwrap();

        let oplog_dir = tenant_path.join("oplog");
        let oplog =
            Arc::new(crate::index::oplog::OpLog::open(&oplog_dir, tenant_id, "test_node").unwrap());

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_core(tmp, tenant_id, embedder_settings, Some(Arc::clone(&oplog)));

        (tx, handle, tasks, vector_indices, oplog)
    }

    fn extract_oplog_vectors(oplog: &crate::index::oplog::OpLog, embedder_name: &str) -> Vec<f64> {
        let entries = oplog.read_since(0).unwrap();
        let upsert = entries
            .iter()
            .find(|e| e.op_type == "upsert")
            .expect("should have an upsert entry");
        let body = upsert.payload.get("body").expect("upsert should have body");
        let vectors = body.get("_vectors").expect("body should contain _vectors");
        let embedder_vec = vectors
            .get(embedder_name)
            .unwrap_or_else(|| panic!("_vectors should have '{embedder_name}' embedder"));
        embedder_vec
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect()
    }

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_computed_vectors_stored_in_oplog() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [0.1, 0.2, 0.3]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, _vi, oplog) =
            setup_write_queue_with_oplog(&tmp, "oplog_vec_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "oplog_vec_task", 1, 1);

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("test oplog vectors".to_string()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // Read oplog and verify computed vectors are stored
        let vec_array = extract_oplog_vectors(&oplog, "default");
        assert_eq!(vec_array.len(), 3);
        assert!((vec_array[0] - 0.1).abs() < 0.01);
        assert!((vec_array[1] - 0.2).abs() < 0.01);
        assert!((vec_array[2] - 0.3).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_user_provided_vectors_preserved_in_oplog() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            serde_json::json!({
                "source": "userProvided",
                "dimensions": 3
            }),
        );

        let (tx, handle, tasks, _vi, oplog) =
            setup_write_queue_with_oplog(&tmp, "oplog_user_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "oplog_user_task", 1, 1);

        let mut fields = HashMap::new();
        fields.insert(
            "title".to_string(),
            FieldValue::Text("user vectors".to_string()),
        );
        let mut vectors_map = HashMap::new();
        vectors_map.insert(
            "default".to_string(),
            FieldValue::Array(vec![
                FieldValue::Float(1.0),
                FieldValue::Float(0.0),
                FieldValue::Float(0.0),
            ]),
        );
        fields.insert("_vectors".to_string(), FieldValue::Object(vectors_map));

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields,
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // Read oplog and verify user-provided vectors are preserved
        let vec_array = extract_oplog_vectors(&oplog, "default");
        assert_eq!(vec_array, vec![1.0, 0.0, 0.0]);
    }

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_oplog_vectors_contain_all_embedder_results() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [0.5, 0.5, 0.5]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        // Two REST embedders with different names
        embedders.insert(
            "embedder_a".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );
        embedders.insert(
            "embedder_b".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, _vi, oplog) =
            setup_write_queue_with_oplog(&tmp, "oplog_multi_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "oplog_multi_task", 1, 1);

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("multi embedder doc".to_string()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // Read oplog and verify both embedders' vectors are present
        let vec_a = extract_oplog_vectors(&oplog, "embedder_a");
        assert_eq!(vec_a.len(), 3);

        let vec_b = extract_oplog_vectors(&oplog, "embedder_b");
        assert_eq!(vec_b.len(), 3);
    }

    #[tokio::test]
    #[serial(flapjack_outbound_url_policy)]
    async fn test_fingerprint_saved_alongside_vector_index() {
        let _allow_local = AllowLocalUrlsGuard::enable();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": [0.1, 0.2, 0.3]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            rest_embedder_config(&server.uri(), 3),
        );

        let (tx, handle, tasks, _vi, _oplog) =
            setup_write_queue_with_oplog(&tmp, "fp_save_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "fp_save_task", 1, 1);

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("fingerprint test".to_string()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // Verify fingerprint.json exists alongside vector files
        let vectors_dir = tmp.path().join("fp_save_t").join("vectors");
        assert!(
            vectors_dir.join("index.usearch").exists(),
            "index.usearch should exist"
        );
        assert!(
            vectors_dir.join("fingerprint.json").exists(),
            "fingerprint.json should exist alongside vector files"
        );

        // Load and verify fingerprint content
        let fp = crate::vector::config::EmbedderFingerprint::load(&vectors_dir).unwrap();
        assert_eq!(fp.version, 1);
        assert_eq!(fp.embedders.len(), 1);
        assert_eq!(fp.embedders[0].name, "default");
        assert_eq!(
            fp.embedders[0].source,
            crate::vector::config::EmbedderSource::Rest
        );
        assert_eq!(fp.embedders[0].dimensions, 3);
    }

    // ── FastEmbed integration tests (9.16) ──

    /// Verify that the local FastEmbed model (BGESmallENV15) automatically embeds a document on add and produces 384-dimensional vectors in the VectorIndex.
    #[cfg(feature = "vector-search-local")]
    #[tokio::test]
    // Concurrent ONNX model cache initialization can race and flake with
    // "Failed to retrieve onnx/model.onnx" when these tests run in parallel.
    #[serial]
    async fn test_fastembed_auto_embed_on_add() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            serde_json::json!({ "source": "fastEmbed" }),
        );

        let (tx, handle, tasks, vector_indices) =
            setup_write_queue_with_embedder(&tmp, "fe_embed_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "fe_embed_task", 1, 1);

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("Hello local embedding".to_string()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        let final_task = tasks.get(&task_id).unwrap();
        assert!(
            matches!(final_task.status, TaskStatus::Succeeded),
            "task should succeed, got: {:?}",
            final_task.status
        );

        // Verify vector index was auto-created with correct dimensions
        assert!(
            vector_indices.contains_key("fe_embed_t"),
            "vector index should be auto-created for fastembed"
        );
        let vi_lock = vector_indices.get("fe_embed_t").unwrap();
        let vi = vi_lock.read().unwrap();
        assert_eq!(vi.len(), 1, "vector index should have 1 document");
        assert_eq!(
            vi.dimensions(),
            384,
            "BGESmallENV15 default model should produce 384-dim vectors"
        );
    }

    #[cfg(feature = "vector-search-local")]
    #[tokio::test]
    #[serial]
    async fn test_fastembed_vectors_in_oplog() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut embedders = HashMap::new();
        embedders.insert(
            "default".to_string(),
            serde_json::json!({ "source": "fastEmbed" }),
        );

        let (tx, handle, tasks, _vi, oplog) =
            setup_write_queue_with_oplog(&tmp, "fe_oplog_t", Some(embedders));

        let task_id = register_task(tasks.as_ref(), "fe_oplog_task", 1, 1);

        tx.send(WriteOp {
            task_id: task_id.clone(),
            actions: vec![WriteAction::Add(crate::types::Document {
                id: "doc1".to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text("oplog fastembed test".to_string()),
                )]),
            })],
        })
        .await
        .unwrap();

        drop(tx);
        handle.await.unwrap().unwrap();

        // Read oplog and verify computed vectors are stored
        let vec_array = extract_oplog_vectors(&oplog, "default");
        assert_eq!(
            vec_array.len(),
            384,
            "fastembed BGESmallENV15 should produce 384-dim vectors in oplog"
        );
    }
}
