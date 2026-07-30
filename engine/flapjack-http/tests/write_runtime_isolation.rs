use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode};
use dashmap::DashMap;
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig, AnalyticsQueryEngine};
use flapjack::dictionaries::manager::DictionaryManager;
use flapjack::recommend::RecommendConfig;
use flapjack::types::TaskStatus;
use flapjack::IndexManager;
use flapjack_http::handlers::metrics::MetricsState;
use flapjack_http::handlers::migration::{MigrationJobRunner, DEFAULT_ASYNC_MIGRATION_CAPACITY};
use flapjack_http::handlers::AppState;
use flapjack_http::idempotency::IdempotencyCache;
use flapjack_http::middleware::TrustedProxyMatcher;
use flapjack_http::pause_registry::PausedIndexes;
use flapjack_http::router::{build_router, RouterConfig};
use flapjack_http::startup::CorsMode;
use flapjack_http::usage_middleware::TenantUsageCounters;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tower::ServiceExt;

const COMMIT_DELAY_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_TEST_COMMIT_DELAY_MS";
const COMMIT_DELAY_MS: u64 = 2_000;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(10);
const REQUIRED_SAMPLE_COUNT: usize = 100;
const LATENCY_LIMIT_MS: u128 = 250;
const INDEX_NAME: &str = "runtime_isolation";

struct EnvVarGuard(&'static str);

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        assert!(
            std::env::var_os(name).is_none(),
            "{name} must be unset before this isolated integration test"
        );
        std::env::set_var(name, value);
        Self(name)
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        std::env::remove_var(self.0);
    }
}

#[derive(Debug)]
struct LivenessSample {
    endpoint: &'static str,
    latency_ms: u128,
}

struct BatchOutcome {
    response: Response<Body>,
    elapsed: Duration,
}

fn make_state(tmp: &TempDir) -> Arc<AppState> {
    let manager = IndexManager::new(tmp.path());
    let dictionary_manager = Arc::new(DictionaryManager::new(tmp.path()));
    manager.set_dictionary_manager(Arc::clone(&dictionary_manager));

    Arc::new(AppState {
        manager: Arc::clone(&manager),
        key_store: None,
        replication_manager: None,
        ssl_manager: None,
        analytics_engine: Some(Arc::new(AnalyticsQueryEngine::new(AnalyticsConfig {
            enabled: true,
            data_dir: tmp.path().join("analytics"),
            flush_interval_secs: 3_600,
            flush_size: 100_000,
            retention_days: 90,
        }))),
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
        migration_runner: Arc::new(MigrationJobRunner::new(
            manager,
            None,
            DEFAULT_ASYNC_MIGRATION_CAPACITY,
        )),
        bulk_replace_max_bytes: 4 * 1024 * 1024 * 1024,
        idempotency_cache: Arc::new(IdempotencyCache::new(Duration::from_secs(300))),
    })
}

fn make_router(tmp: &TempDir) -> (axum::Router, Arc<AppState>) {
    let analytics = AnalyticsCollector::new(AnalyticsConfig {
        enabled: false,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 60,
        flush_size: 1_000,
        retention_days: 30,
    });
    let trusted_proxies =
        Arc::new(TrustedProxyMatcher::from_optional_csv(None).expect("valid default proxies"));
    let state = make_state(tmp);

    let router = build_router(
        Arc::clone(&state),
        None,
        analytics,
        trusted_proxies,
        tmp.path(),
        RouterConfig {
            cors_mode: CorsMode::LoopbackOnly,
            disable_dashboard: true,
        },
    );
    (router, state)
}

async fn request(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    let request_body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    app.clone()
        .oneshot(builder.body(request_body).expect("valid request"))
        .await
        .expect("router request succeeds")
}

async fn response_json(response: Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body is readable");
    serde_json::from_slice(&bytes).expect("response body is valid JSON")
}

async fn sample_liveness(
    app: axum::Router,
    batch_complete: Arc<AtomicBool>,
    ready: oneshot::Sender<()>,
) -> Vec<LivenessSample> {
    let schedule_start = Instant::now();
    ready.send(()).expect("test waits for sampler readiness");
    let mut samples = Vec::new();

    while samples.len() < REQUIRED_SAMPLE_COUNT || !batch_complete.load(Ordering::Acquire) {
        let scheduled_deadline = schedule_start + SAMPLE_INTERVAL * samples.len() as u32;
        tokio::time::sleep_until(scheduled_deadline).await;
        let (endpoint, uri) = if samples.len() % 2 == 0 {
            ("health", "/health".to_string())
        } else {
            ("count", format!("/1/usage/documents_count/{INDEX_NAME}"))
        };
        let response = request(&app, Method::GET, &uri, None).await;
        assert!(
            response.status().is_success(),
            "{endpoint} sampler request failed with {}",
            response.status()
        );
        samples.push(LivenessSample {
            endpoint,
            latency_ms: scheduled_deadline.elapsed().as_millis(),
        });
    }

    samples
}

fn endpoint_distribution(samples: &[LivenessSample], endpoint: &str) -> Vec<u128> {
    let mut distribution: Vec<u128> = samples
        .iter()
        .filter(|sample| sample.endpoint == endpoint)
        .map(|sample| sample.latency_ms)
        .collect();
    distribution.sort_unstable();
    distribution
}

fn p99(distribution: &[u128]) -> u128 {
    let rank = (99 * distribution.len()).div_ceil(100);
    distribution[rank - 1]
}

fn assert_liveness_distribution(samples: &[LivenessSample]) {
    let health = endpoint_distribution(samples, "health");
    let count = endpoint_distribution(samples, "count");
    assert!(!health.is_empty(), "health must have liveness samples");
    assert!(!count.is_empty(), "count must have liveness samples");

    let health_p99 = p99(&health);
    let count_p99 = p99(&count);
    let health_max = *health.last().expect("health distribution is non-empty");
    let count_max = *count.last().expect("count distribution is non-empty");
    assert!(
        health_p99 <= LATENCY_LIMIT_MS
            && count_p99 <= LATENCY_LIMIT_MS
            && health_max <= LATENCY_LIMIT_MS
            && count_max <= LATENCY_LIMIT_MS,
        "scheduled liveness exceeded {LATENCY_LIMIT_MS}ms: \
         health_samples={} health_p99={health_p99} health_max={health_max} health={health:?}; \
         count_samples={} count_p99={count_p99} count_max={count_max} count={count:?}",
        health.len(),
        count.len(),
    );
}

async fn latest_document_count(app: &axum::Router) -> u64 {
    let response = request(
        app,
        Method::GET,
        &format!("/1/usage/documents_count/{INDEX_NAME}"),
        None,
    )
    .await;
    assert!(response.status().is_success());
    let payload = response_json(response).await;
    payload["documents_count"]
        .as_array()
        .and_then(|series| series.last())
        .and_then(|sample| sample["v"].as_u64())
        .expect("usage response contains the current document count")
}

async fn prepare_index(app: &axum::Router) {
    let create = request(
        app,
        Method::POST,
        "/1/indexes",
        Some(json!({"uid": INDEX_NAME})),
    )
    .await;
    assert!(create.status().is_success(), "index setup must succeed");
    let health = request(app, Method::GET, "/health", None).await;
    assert_eq!(health.status(), StatusCode::OK, "health setup probe");
    let count = request(
        app,
        Method::GET,
        &format!("/1/usage/documents_count/{INDEX_NAME}"),
        None,
    )
    .await;
    assert!(count.status().is_success(), "count setup probe");
}

fn spawn_delayed_batch(
    app: axum::Router,
    batch_complete: Arc<AtomicBool>,
) -> JoinHandle<BatchOutcome> {
    tokio::spawn(async move {
        let started = Instant::now();
        let response = request(
            &app,
            Method::POST,
            &format!("/1/indexes/{INDEX_NAME}/batch"),
            Some(json!({
                "requests": [{
                    "action": "addObject",
                    "body": {"objectID": "blocked-commit", "title": "durable"}
                }]
            })),
        )
        .await;
        batch_complete.store(true, Ordering::Release);
        BatchOutcome {
            response,
            elapsed: started.elapsed(),
        }
    })
}

async fn join_liveness_and_batch(
    sampler: JoinHandle<Vec<LivenessSample>>,
    batch: JoinHandle<BatchOutcome>,
) -> (Vec<LivenessSample>, BatchOutcome) {
    let mut sampler = std::pin::pin!(sampler);
    let mut batch = std::pin::pin!(batch);

    tokio::select! {
        samples = &mut sampler => {
            let samples = samples.expect("sampler task completes");
            let outcome = batch.await.expect("batch task completes");
            (samples, outcome)
        }
        outcome = &mut batch => {
            let outcome = outcome.expect("batch task completes");
            let samples = sampler.await.expect("sampler task completes");
            (samples, outcome)
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn liveness_join_surfaces_batch_panic_without_waiting_for_sampler() {
    let sampler = tokio::spawn(async { std::future::pending::<Vec<LivenessSample>>().await });
    let batch = tokio::spawn(async {
        panic!("simulated batch task failure");
        #[allow(unreachable_code)]
        BatchOutcome {
            response: Response::new(Body::empty()),
            elapsed: Duration::ZERO,
        }
    });
    let joined = tokio::spawn(join_liveness_and_batch(sampler, batch));

    let result = tokio::time::timeout(Duration::from_millis(250), joined)
        .await
        .expect("batch task failure must be observed without waiting for the sampler");
    match result {
        Ok(_) => panic!("batch task failure must panic the liveness join"),
        Err(join_error) => assert!(join_error.is_panic()),
    }
}

async fn assert_committed_batch(app: &axum::Router, state: &Arc<AppState>, outcome: BatchOutcome) {
    assert!(
        outcome.response.status().is_success(),
        "batch request succeeds"
    );
    let payload = response_json(outcome.response).await;
    let task_id = payload["taskID"]
        .as_i64()
        .expect("batch response contains its numeric task identifier");
    assert_eq!(
        payload["objectIDs"],
        json!(["blocked-commit"]),
        "batch response identifies the committed object"
    );
    let task = state
        .manager
        .get_task(&task_id.to_string())
        .expect("returned task identifier resolves through the task owner");
    assert_eq!(task.numeric_id, task_id);
    assert_eq!(task.status, TaskStatus::Succeeded);
    assert!(
        outcome.elapsed >= Duration::from_millis(COMMIT_DELAY_MS),
        "debug commit delay must be enabled; batch completed in {:?}",
        outcome.elapsed
    );
    assert_eq!(latest_document_count(app).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn single_worker_runtime_serves_count_during_injected_two_second_commit() {
    let tmp = TempDir::new().expect("temporary data directory");
    let (app, state) = make_router(&tmp);
    prepare_index(&app).await;

    let _delay_guard = EnvVarGuard::set(COMMIT_DELAY_ENV_VAR, &COMMIT_DELAY_MS.to_string());
    let batch_complete = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = oneshot::channel();
    let sampler = tokio::spawn(sample_liveness(
        app.clone(),
        Arc::clone(&batch_complete),
        ready_tx,
    ));
    ready_rx.await.expect("separate sampler task became ready");
    let batch = spawn_delayed_batch(app.clone(), batch_complete);

    let (samples, outcome) = join_liveness_and_batch(sampler, batch).await;
    assert_committed_batch(&app, &state, outcome).await;
    assert_liveness_distribution(&samples);
}
