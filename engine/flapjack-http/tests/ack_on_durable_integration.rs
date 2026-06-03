//! Stub summary for /Users/stuart/parallel_development/flapjack_dev/jun02_pm_1_admin_key_rotation_race_fix/flapjack_dev/engine/flapjack-http/tests/ack_on_durable_integration.rs.
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Extension;
use axum::Json;
use dashmap::DashMap;
use flapjack::dictionaries::manager::DictionaryManager;
use flapjack::error::FlapjackError;
use flapjack::index::settings::IndexSettings;
use flapjack::recommend::RecommendConfig;
use flapjack::types::{Document, FieldValue, TaskStatus};
use flapjack::IndexManager;
use flapjack_http::auth::AuthenticatedAppId;
use flapjack_http::handlers::metrics::MetricsState;
use flapjack_http::handlers::{add_documents, delete_object, AppState};
use flapjack_http::idempotency::IdempotencyCache;
use flapjack_http::pause_registry::PausedIndexes;
use flapjack_http::usage_middleware::TenantUsageCounters;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::time::{sleep, Instant};

// Stage-1 contract anchor: engine/src/index/write_queue/mod.rs defines
// WRITE_QUEUE_CHANNEL_CAPACITY as 2_000 at HEAD, but that constant is private
// to the crate and inaccessible from integration tests.
const WRITE_QUEUE_CHANNEL_CAPACITY: usize = 2_000;
const DEFAULT_WRITE_DURABLE_TIMEOUT_MS_FOR_TEST: u64 = 30_000;
static DURABLE_TIMEOUT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn make_state(tmp: &TempDir) -> Arc<AppState> {
    let manager = IndexManager::new(tmp.path());
    let dictionary_manager = Arc::new(DictionaryManager::new(tmp.path()));
    manager.set_dictionary_manager(Arc::clone(&dictionary_manager));

    Arc::new(AppState {
        manager,
        key_store: None,
        replication_manager: None,
        ssl_manager: None,
        analytics_engine: None,
        recommend_config: RecommendConfig::default(),
        experiment_store: None,
        dictionary_manager,
        metrics_state: Some(MetricsState::new()),
        usage_counters: Arc::new(DashMap::<String, TenantUsageCounters>::new()),
        usage_persistence: None,
        paused_indexes: PausedIndexes::new(),
        geoip_reader: None,
        notification_service: None,
        start_time: std::time::Instant::now(),
        conversation_store: flapjack_http::conversation_store::ConversationStore::default_shared(),
        embedder_store: Arc::new(flapjack_http::embedder_store::EmbedderStore::new()),
        idempotency_cache: Arc::new(IdempotencyCache::new(Duration::from_secs(300))),
    })
}

async fn post_single_doc(
    state: &Arc<AppState>,
    index: &str,
    object_id: &str,
) -> axum::http::Response<Body> {
    let body = json!({
        "objectID": object_id,
        "title": format!("title-{object_id}"),
    });

    add_documents(
        State(Arc::clone(state)),
        Extension(AuthenticatedAppId("ack-test-app".to_string())),
        Path(index.to_string()),
        HeaderMap::new(),
        Json(body),
    )
    .await
    .expect("handler should produce response")
}

async fn delete_single_doc(
    state: &Arc<AppState>,
    index: &str,
    object_id: &str,
) -> Result<axum::response::Response, FlapjackError> {
    delete_object(
        State(Arc::clone(state)),
        Path((index.to_string(), object_id.to_string())),
    )
    .await
}

async fn response_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    serde_json::from_slice(&bytes).expect("response body should be valid json")
}

fn extract_numeric_task_id(payload: &Value, context: &str) -> i64 {
    payload["taskID"]
        .as_i64()
        .unwrap_or_else(|| panic!("{context} response should include numeric taskID: {payload}"))
}

async fn wait_for_task_terminal(
    manager: &Arc<IndexManager>,
    numeric_task_id: i64,
    timeout: Duration,
) -> TaskStatus {
    let deadline = Instant::now() + timeout;
    let key = numeric_task_id.to_string();

    loop {
        let status = manager
            .get_task(&key)
            .expect("task should be present")
            .status;
        match status {
            TaskStatus::Enqueued | TaskStatus::Processing => {
                if Instant::now() >= deadline {
                    panic!("task {numeric_task_id} did not reach terminal status in {timeout:?}");
                }
                sleep(Duration::from_millis(10)).await;
            }
            done => return done,
        }
    }
}

async fn seed_committed_doc(
    state: &Arc<AppState>,
    manager: &Arc<IndexManager>,
    tenant: &str,
    object_id: &str,
    context: &str,
) {
    let seed_resp = post_single_doc(state, tenant, object_id).await;
    assert_eq!(
        seed_resp.status(),
        StatusCode::OK,
        "{context}: seed write must ACK 200 before exercising the delete path"
    );
    let seed_payload = response_json(seed_resp).await;
    let seed_task = extract_numeric_task_id(&seed_payload, context);
    let seed_status = wait_for_task_terminal(manager, seed_task, Duration::from_secs(5)).await;
    assert!(
        matches!(seed_status, TaskStatus::Succeeded),
        "{context}: seed write must durably commit before the delete probe, got {seed_status:?}"
    );
}

fn tenant_string_task_ids_for_test(manager: &Arc<IndexManager>, tenant: &str) -> HashSet<String> {
    manager
        .tenant_tasks_snapshot_for_test(tenant)
        .into_iter()
        .map(|task| task.id)
        .collect()
}

fn accepted_primary_delete_task_id_for_test(
    manager: &Arc<IndexManager>,
    tenant: &str,
    baseline_task_ids: &HashSet<String>,
    context: &str,
) -> i64 {
    manager
        .tenant_tasks_snapshot_for_test(tenant)
        .into_iter()
        .find(|task| !baseline_task_ids.contains(&task.id))
        .map(|task| task.numeric_id)
        .unwrap_or_else(|| {
            panic!("{context}: delete should create one new primary task id for error reporting")
        })
}

fn make_doc(id: usize) -> Document {
    Document {
        id: format!("doc-{id}"),
        fields: std::collections::HashMap::from([(
            "title".to_string(),
            FieldValue::Text(format!("queued-{id}")),
        )]),
    }
}

fn configure_primary_standard_replica(base: &std::path::Path, primary: &str, replica: &str) {
    let primary_dir = base.join(primary);
    std::fs::create_dir_all(&primary_dir).expect("primary dir should exist");
    let settings_path = primary_dir.join("settings.json");
    let mut settings = if settings_path.exists() {
        IndexSettings::load(&settings_path).expect("existing primary settings should load")
    } else {
        IndexSettings::default()
    };
    settings.replicas = Some(vec![replica.to_string()]);
    settings
        .save(&settings_path)
        .expect("replica config should persist");
}

struct DurableTimeoutEnvOverrideGuard {
    previous: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for DurableTimeoutEnvOverrideGuard {
    fn drop(&mut self) {
        // SAFETY: Guard owns serialization lock for the full override lifetime,
        // so no sibling test in this binary can mutate the same env var while
        // the restore runs.
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var("FLAPJACK_WRITE_DURABLE_TIMEOUT_MS", value),
                None => std::env::remove_var("FLAPJACK_WRITE_DURABLE_TIMEOUT_MS"),
            }
        }
    }
}

fn set_durable_timeout_env_for_test(timeout_ms: u64) -> DurableTimeoutEnvOverrideGuard {
    let lock = DURABLE_TIMEOUT_ENV_LOCK
        .lock()
        .expect("durable-timeout env lock should not be poisoned");
    let previous = std::env::var("FLAPJACK_WRITE_DURABLE_TIMEOUT_MS").ok();
    // SAFETY: Writes are serialized by the process-wide lock above, and the Drop
    // impl restores prior state before releasing that lock.
    unsafe {
        std::env::set_var("FLAPJACK_WRITE_DURABLE_TIMEOUT_MS", timeout_ms.to_string());
    }
    DurableTimeoutEnvOverrideGuard {
        previous,
        _lock: lock,
    }
}

fn set_default_durable_timeout_env_for_test() -> DurableTimeoutEnvOverrideGuard {
    // Rust integration tests share one process environment while the harness runs
    // tests concurrently. Tests that assert normal 200 durability must hold the
    // same lock as timeout-injection tests so they never inherit a sibling's
    // intentionally tiny `FLAPJACK_WRITE_DURABLE_TIMEOUT_MS`.
    set_durable_timeout_env_for_test(DEFAULT_WRITE_DURABLE_TIMEOUT_MS_FOR_TEST)
}

#[test]
fn test_durable_timeout_env_override_requires_isolation() {
    let (lock_held_tx, lock_held_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _worker_override = set_durable_timeout_env_for_test(111);
        lock_held_tx
            .send(())
            .expect("worker should signal once lock is held");
        std::thread::sleep(Duration::from_millis(150));
    });

    lock_held_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("worker should acquire lock before main-thread assertion");
    let started = std::time::Instant::now();
    let _main_override = set_durable_timeout_env_for_test(222);
    let waited = started.elapsed();
    let observed = std::env::var("FLAPJACK_WRITE_DURABLE_TIMEOUT_MS")
        .expect("timeout override should be visible while test holds it");
    drop(_main_override);
    worker.join().expect("worker thread should join");

    assert_eq!(
        observed, "222",
        "main thread should observe its own override value once it acquires the lock"
    );
    assert!(
        waited >= Duration::from_millis(100),
        "second override should block until first guard releases lock; waited={waited:?}"
    );
}

#[cfg(unix)]
fn set_tenant_permissions_read_only(base: &std::path::Path, tenant: &str) {
    use std::os::unix::fs::PermissionsExt;
    let tenant_path = base.join(tenant);
    let mut perms = std::fs::metadata(&tenant_path)
        .expect("tenant dir metadata should exist")
        .permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&tenant_path, perms)
        .expect("tenant dir should become read-only to force commit failure");
}

#[cfg(unix)]
fn set_tenant_permissions_writable(base: &std::path::Path, tenant: &str) {
    use std::os::unix::fs::PermissionsExt;
    let tenant_path = base.join(tenant);
    let mut perms = std::fs::metadata(&tenant_path)
        .expect("tenant dir metadata should exist")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&tenant_path, perms)
        .expect("tenant dir permissions should be restorable");
}

#[tokio::test(flavor = "current_thread")]
async fn test_http_200_implies_tantivy_durable() {
    let _durable_timeout_override = set_default_durable_timeout_env_for_test();
    let tmp = TempDir::new().expect("tempdir should create");
    let state = make_state(&tmp);
    let manager = Arc::clone(&state.manager);
    let tenant = "durability_red";
    manager
        .create_tenant(tenant)
        .expect("tenant should exist before single-doc add path");

    let warmup_resp = post_single_doc(&state, tenant, "warmup-doc").await;
    assert_eq!(
        warmup_resp.status(),
        StatusCode::OK,
        "warm-up write must ACK 200"
    );
    let warmup_payload = response_json(warmup_resp).await;
    let warmup_task = extract_numeric_task_id(&warmup_payload, "warm-up");
    let warmup_status = wait_for_task_terminal(&manager, warmup_task, Duration::from_secs(5)).await;
    assert!(
        matches!(warmup_status, TaskStatus::Succeeded),
        "warm-up write should commit before inducing the defect, got {warmup_status:?}"
    );
    let write_resp = post_single_doc(&state, tenant, "lost-after-200").await;
    assert_eq!(
        write_resp.status(),
        StatusCode::OK,
        "defect-triggering HTTP write must ACK 200"
    );

    drop(state);

    let reopened = IndexManager::new(tmp.path());
    reopened
        .create_tenant(tenant)
        .expect("tenant should reopen from disk");

    let persisted = reopened
        .get_document(tenant, "lost-after-200")
        .expect("document lookup should succeed");

    assert!(
        persisted.is_some(),
        "expected ACKed doc to be durable after restart; HEAD silently drops it"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_queue_full_returns_429_retry_after() {
    let tmp = TempDir::new().expect("tempdir should create");
    let state = make_state(&tmp);
    let tenant = "queue_full_guardrail";

    state
        .manager
        .create_tenant(tenant)
        .expect("tenant should create");

    let mut overflow_error: Option<FlapjackError> = None;

    for i in 0..=WRITE_QUEUE_CHANNEL_CAPACITY {
        let result = state.manager.add_documents(tenant, vec![make_doc(i)]);
        if i < WRITE_QUEUE_CHANNEL_CAPACITY {
            assert!(
                result.is_ok(),
                "enqueue {} should succeed before capacity is exceeded; got {result:?}",
                i + 1
            );
        } else {
            overflow_error = result.err();
        }
    }

    let error = overflow_error.expect("capacity+1 enqueue must fail with QueueFull");
    assert!(
        matches!(error, FlapjackError::QueueFull),
        "overflow must return QueueFull (429 path), got {error:?}"
    );

    let response = error.into_response();
    assert_eq!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "QueueFull must map to HTTP 429"
    );
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok());
    assert_eq!(
        retry_after,
        Some("1"),
        "QueueFull response must include Retry-After: 1"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_commit_failure_returns_5xx() {
    let _durable_timeout_override = set_default_durable_timeout_env_for_test();
    let tmp = TempDir::new().expect("tempdir should create");
    let state = make_state(&tmp);
    let tenant = "commit_fail_red";
    state
        .manager
        .create_tenant(tenant)
        .expect("tenant should exist before single-doc add path");

    let warmup_resp = post_single_doc(&state, tenant, "warmup").await;
    assert_eq!(
        warmup_resp.status(),
        StatusCode::OK,
        "warm-up write must ACK 200"
    );
    let warmup_payload = response_json(warmup_resp).await;
    let warmup_task = extract_numeric_task_id(&warmup_payload, "warm-up");
    let warmup_status =
        wait_for_task_terminal(&state.manager, warmup_task, Duration::from_secs(5)).await;
    assert!(
        matches!(warmup_status, TaskStatus::Succeeded),
        "warm-up write should commit before inducing commit failure, got {warmup_status:?}"
    );

    #[cfg(unix)]
    set_tenant_permissions_read_only(tmp.path(), tenant);

    let write_resp = post_single_doc(&state, tenant, "commit-should-fail").await;
    let write_status = write_resp.status();
    let write_payload = response_json(write_resp).await;
    let write_task = extract_numeric_task_id(&write_payload, "commit-failure write");
    let queued_status =
        wait_for_task_terminal(&state.manager, write_task, Duration::from_secs(5)).await;
    assert!(
        matches!(queued_status, TaskStatus::Failed(_)),
        "filesystem seam must produce an actual failed queued task before asserting HTTP contract, got {queued_status:?}"
    );

    #[cfg(unix)]
    set_tenant_permissions_writable(tmp.path(), tenant);

    assert!(
        write_status.is_server_error(),
        "commit failure must surface as 5xx; got {} (false ACK indicates defect)",
        write_status
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_delete_object_restart_returns_bounded_503() {
    let tmp = TempDir::new().expect("tempdir should create");
    let state = make_state(&tmp);
    let manager = Arc::clone(&state.manager);
    let tenant = "delete_restart_red";
    manager
        .create_tenant(tenant)
        .expect("tenant should exist before delete path");

    seed_committed_doc(&state, &manager, tenant, "seed-doc", "delete restart probe").await;
    assert!(
        manager
            .get_document(tenant, "seed-doc")
            .expect("seed lookup should succeed")
            .is_some(),
        "seed precondition failed: expected object to exist before delete"
    );

    let _timeout_override = set_durable_timeout_env_for_test(100);

    // Keep the queue busy so the delete stays enqueued long enough for us to
    // abort the tenant write task after enqueue and before commit.
    for i in 0..40 {
        manager
            .add_documents(tenant, vec![make_doc(i)])
            .expect("backlog enqueue should succeed");
    }

    let baseline_task_ids = tenant_string_task_ids_for_test(&manager, tenant);
    let state_for_delete = Arc::clone(&state);
    let delete_future =
        tokio::spawn(async move { delete_single_doc(&state_for_delete, tenant, "seed-doc").await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let saw_new_queued_task = manager
                .tenant_tasks_snapshot_for_test(tenant)
                .into_iter()
                .any(|task| {
                    !baseline_task_ids.contains(&task.id)
                        && matches!(task.status, TaskStatus::Enqueued | TaskStatus::Processing)
                });
            if saw_new_queued_task {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("delete task should reach enqueued/processing before induced restart");
    let accepted_delete_task_id = accepted_primary_delete_task_id_for_test(
        &manager,
        tenant,
        &baseline_task_ids,
        "delete restart probe",
    );
    assert!(
        manager.abort_tenant_write_task_for_test(tenant),
        "write task must be present so restart defect can be induced"
    );

    let bounded = tokio::time::timeout(Duration::from_millis(300), delete_future).await;

    let joined = bounded.expect("delete request should complete within bounded timeout window");
    let delete_result = joined.expect("delete task should join cleanly");
    let response = delete_result.expect(
        "delete should render a retriable timeout response when write task dies after enqueue",
    );

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "timed-out durable delete must map to retriable 503"
    );
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok());
    assert_eq!(
        retry_after,
        Some("1"),
        "timed-out durable delete must include Retry-After: 1"
    );
    let response_body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("timed-out delete error body should be readable");
    let response_json: Value = serde_json::from_slice(&response_body)
        .expect("timed-out delete error should be valid json");
    assert_eq!(
        response_json["taskID"].as_i64(),
        Some(accepted_delete_task_id),
        "timed-out durable delete must preserve the exact accepted delete taskID"
    );
    let response_status = response_json["status"]
        .as_u64()
        .expect("error response status should be numeric");
    assert_eq!(
        response_status, 503,
        "timed-out durable delete payload status should be 503"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_delete_object_replica_durable_failure_preserves_task_id() {
    let _durable_timeout_override = set_default_durable_timeout_env_for_test();
    let tmp = TempDir::new().expect("tempdir should create");
    let state = make_state(&tmp);
    let manager = Arc::clone(&state.manager);
    let tenant = "delete_replica_timeout";
    let replica = "delete_replica_timeout_std";
    manager
        .create_tenant(tenant)
        .expect("primary tenant should be creatable");
    manager
        .create_tenant(replica)
        .expect("replica tenant should be creatable");
    configure_primary_standard_replica(tmp.path(), tenant, replica);

    seed_committed_doc(
        &state,
        &manager,
        tenant,
        "seed-doc",
        "replica durable failure probe",
    )
    .await;

    #[cfg(unix)]
    set_tenant_permissions_read_only(tmp.path(), replica);

    let primary_baseline_task_ids = tenant_string_task_ids_for_test(&manager, tenant);
    let response = delete_single_doc(&state, tenant, "seed-doc")
        .await
        .expect("accepted primary delete must surface mapped durable failure response when replica commit fails");

    #[cfg(unix)]
    set_tenant_permissions_writable(tmp.path(), replica);

    assert!(
        response.status().is_server_error(),
        "replica durable failure must surface as server error"
    );
    let accepted_primary_delete_task_id = accepted_primary_delete_task_id_for_test(
        &manager,
        tenant,
        &primary_baseline_task_ids,
        "replica durable failure probe",
    );
    let response_json = response_json(response).await;
    assert_eq!(
        response_json["taskID"].as_i64(),
        Some(accepted_primary_delete_task_id),
        "accepted delete must preserve the exact primary taskID when replica durable leg fails after primary accept"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_delete_object_replica_queue_full_preserves_task_id_and_retry_after() {
    let _durable_timeout_override = set_default_durable_timeout_env_for_test();
    let tmp = TempDir::new().expect("tempdir should create");
    let state = make_state(&tmp);
    let manager = Arc::clone(&state.manager);
    let tenant = "delete_replica_queue_full";
    let replica = "delete_replica_queue_full_std";
    manager
        .create_tenant(tenant)
        .expect("primary tenant should be creatable");
    manager
        .create_tenant(replica)
        .expect("replica tenant should be creatable");
    configure_primary_standard_replica(tmp.path(), tenant, replica);

    let seed_resp = post_single_doc(&state, tenant, "seed-doc").await;
    assert_eq!(
        seed_resp.status(),
        StatusCode::OK,
        "seed write must ACK 200 before replica QueueFull probe"
    );
    let seed_payload = response_json(seed_resp).await;
    let seed_task = extract_numeric_task_id(&seed_payload, "seed");
    let seed_status = wait_for_task_terminal(&manager, seed_task, Duration::from_secs(5)).await;
    assert!(
        matches!(seed_status, TaskStatus::Succeeded),
        "seed write should commit before replica QueueFull probe, got {seed_status:?}"
    );

    assert!(
        manager.abort_tenant_write_task_for_test(replica),
        "replica write task should be abortable before forcing QueueFull"
    );

    let mut overflow_error: Option<FlapjackError> = None;
    for i in 0..=WRITE_QUEUE_CHANNEL_CAPACITY {
        let result = manager.add_documents(replica, vec![make_doc(i)]);
        if i < WRITE_QUEUE_CHANNEL_CAPACITY {
            assert!(
                result.is_ok(),
                "replica enqueue {} should succeed before capacity is exceeded; got {result:?}",
                i + 1
            );
        } else {
            overflow_error = result.err();
        }
    }
    let error = overflow_error.expect("replica capacity+1 enqueue must fail with QueueFull");
    assert!(
        matches!(error, FlapjackError::QueueFull),
        "replica overflow must return QueueFull, got {error:?}"
    );

    let primary_baseline_task_ids = tenant_string_task_ids_for_test(&manager, tenant);

    let response = delete_single_doc(&state, tenant, "seed-doc")
        .await
        .expect("accepted primary delete must map replica QueueFull into task-aware response");
    assert_eq!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "replica QueueFull should surface as retryable 429"
    );
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok());
    assert_eq!(
        retry_after,
        Some("1"),
        "accepted delete with replica QueueFull must preserve Retry-After: 1"
    );
    let accepted_primary_delete_task_id = accepted_primary_delete_task_id_for_test(
        &manager,
        tenant,
        &primary_baseline_task_ids,
        "replica QueueFull probe",
    );
    let response_json = response_json(response).await;
    assert_eq!(
        response_json["taskID"].as_i64(),
        Some(accepted_primary_delete_task_id),
        "accepted delete must preserve the exact primary taskID when replica QueueFull occurs after primary accept"
    );
}
