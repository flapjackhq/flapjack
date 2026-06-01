//! Stub summary for /Users/stuart/parallel_development/flapjack_dev/may31_eve_2_ha_snapshot_flake_verify/flapjack_dev/engine/tests/common/state.rs.
use axum::{
    middleware,
    routing::{delete, get, post},
    Router,
};
use flapjack_http::startup::CorsMode;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir as RawTempDir;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;

use flapjack_replication::{
    config::{NodeConfig, PeerConfig},
    manager::ReplicationManager,
};

const SERVER_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

/// A running test server that can be stopped gracefully so its data dir
/// can be reused for a restart-in-place test.
pub struct TestNode {
    pub addr: String,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    stopped_rx: Option<std::sync::mpsc::Receiver<()>>,
    handle: tokio::task::JoinHandle<()>,
}

impl TestNode {
    fn begin_shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }

    fn wait_for_exit_blocking(&mut self, timeout: Duration) -> bool {
        self.stopped_rx
            .take()
            .map(|rx| rx.recv_timeout(timeout).is_ok())
            .unwrap_or(true)
    }

    /// Stop this node gracefully and wait for the server task to exit.
    pub async fn stop(mut self) {
        self.begin_shutdown();
        let _ = self.handle.await;
    }
}

/// Test temp-dir wrapper that owns a spawned test server lifecycle.
///
/// When this wrapper is dropped, it sends shutdown to the attached server and
/// aborts the task as a final fallback so integration tests don't leak
/// fire-and-forget server tasks between cases.
pub struct TempDir {
    inner: RawTempDir,
    node: Option<TestNode>,
}

impl TempDir {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            inner: RawTempDir::new()?,
            node: None,
        })
    }

    pub fn path(&self) -> &Path {
        self.inner.path()
    }

    fn attach_node(&mut self, node: TestNode) {
        self.node = Some(node);
    }
}

impl Deref for TempDir {
    type Target = RawTempDir;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for TempDir {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Some(mut node) = self.node.take() {
            node.begin_shutdown();
            if !node.wait_for_exit_blocking(SERVER_SHUTDOWN_TIMEOUT) {
                node.handle.abort();
            }
        }
    }
}

pub async fn spawn_server() -> (String, TempDir) {
    spawn_server_with_key(None).await
}

fn apply_standard_test_http_layers(app: Router) -> Router {
    app.layer(middleware::from_fn(
        flapjack_http::middleware::normalize_content_type,
    ))
    .layer(middleware::from_fn(
        flapjack_http::middleware::ensure_json_errors,
    ))
    .layer(CorsLayer::very_permissive().max_age(std::time::Duration::from_secs(86400)))
    .layer(middleware::from_fn(
        flapjack_http::middleware::allow_private_network,
    ))
}

fn apply_test_app_id_layer(app: Router) -> Router {
    app.layer(middleware::from_fn(
        |mut request: axum::extract::Request, next: middleware::Next| async move {
            if request
                .extensions()
                .get::<flapjack_http::auth::AuthenticatedAppId>()
                .is_none()
            {
                let application_id = flapjack_http::auth::request_application_id(&request)
                    .unwrap_or_else(|| {
                        flapjack::dictionaries::DEFAULT_DICTIONARY_TENANT.to_string()
                    });
                request
                    .extensions_mut()
                    .insert(flapjack_http::auth::AuthenticatedAppId(application_id));
            }
            next.run(request).await
        },
    ))
}

fn analytics_config(data_dir: &Path, flush_size: usize) -> flapjack::analytics::AnalyticsConfig {
    flapjack::analytics::AnalyticsConfig {
        enabled: true,
        data_dir: data_dir.join("analytics"),
        flush_interval_secs: 3600,
        flush_size,
        retention_days: 90,
    }
}

fn build_analytics_runtime(
    config: flapjack::analytics::AnalyticsConfig,
) -> (
    Arc<flapjack::analytics::AnalyticsCollector>,
    Arc<flapjack::analytics::AnalyticsQueryEngine>,
) {
    let collector = flapjack::analytics::AnalyticsCollector::new(config.clone());
    let engine = Arc::new(flapjack::analytics::AnalyticsQueryEngine::new(config));
    (collector, engine)
}

fn default_trusted_proxy_matcher() -> Arc<flapjack_http::middleware::TrustedProxyMatcher> {
    Arc::new(
        flapjack_http::middleware::TrustedProxyMatcher::from_optional_csv(None)
            .expect("default trusted proxy CIDRs must parse"),
    )
}

/// Build canonical integration-test `AppState` with shared defaults.
///
/// Callers must keep the backing `TempDir`/`data_dir` alive for the full lifetime of the
/// returned state because `IndexManager`, dictionary files, and analytics paths are rooted there.
/// This helper owns `AppState` field assembly; if a caller injects an existing manager, this
/// constructor still wires it with a dictionary manager via `set_dictionary_manager(...)`.
pub fn make_test_app_state(
    data_dir: &Path,
    manager_override: Option<Arc<flapjack::IndexManager>>,
    key_store: Option<Arc<flapjack_http::auth::KeyStore>>,
    replication_manager: Option<Arc<flapjack_replication::manager::ReplicationManager>>,
    analytics_engine: Option<Arc<flapjack::analytics::AnalyticsQueryEngine>>,
    experiment_store: Option<Arc<flapjack::experiments::store::ExperimentStore>>,
    metrics_state: Option<flapjack_http::handlers::metrics::MetricsState>,
) -> Arc<flapjack_http::handlers::AppState> {
    let manager = manager_override.unwrap_or_else(|| flapjack::IndexManager::new(data_dir));
    let dictionary_manager = Arc::new(flapjack::dictionaries::manager::DictionaryManager::new(
        data_dir,
    ));
    manager.set_dictionary_manager(Arc::clone(&dictionary_manager));

    Arc::new(flapjack_http::handlers::AppState {
        manager,
        key_store,
        replication_manager,
        ssl_manager: None,
        analytics_engine,
        recommend_config: flapjack::recommend::RecommendConfig::default(),
        experiment_store,
        dictionary_manager,
        metrics_state,
        usage_counters: Arc::new(dashmap::DashMap::new()),
        paused_indexes: flapjack_http::pause_registry::PausedIndexes::new(),
        usage_persistence: None,
        geoip_reader: None,
        notification_service: None,
        start_time: std::time::Instant::now(),
        conversation_store: flapjack_http::conversation_store::ConversationStore::default_shared(),
        embedder_store: Arc::new(flapjack_http::embedder_store::EmbedderStore::new()),
        idempotency_cache: Arc::new(flapjack_http::idempotency::IdempotencyCache::new(
            std::time::Duration::from_secs(300),
        )),
    })
}

async fn wait_for_health(addrs: &[&str], attempts: usize) {
    let client = reqwest::Client::new();
    let mut last_unready_addr = "";
    for _ in 0..attempts {
        let mut all_ready = true;
        for addr in addrs {
            let health_ok = client
                .get(format!("http://{addr}/health"))
                .send()
                .await
                .map(|response| response.status().is_success())
                .unwrap_or(false);
            let readiness_ok = client
                .get(format!("http://{addr}/health/ready"))
                .send()
                .await
                .map(|response| response.status().is_success())
                .unwrap_or(false);
            if !health_ok || !readiness_ok {
                last_unready_addr = addr;
                all_ready = false;
                break;
            }
        }
        if all_ready {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }

    panic!(
        "timed out waiting for /health and /health/ready readiness probes at {}",
        last_unready_addr
    );
}

pub(crate) async fn spawn_router(app: Router, temp_dir: &mut TempDir) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let node = serve_with_shutdown(listener, app);
    let addr = node.addr.clone();
    temp_dir.attach_node(node);
    wait_for_health(&[&addr], 100).await;
    addr
}

fn build_query_suggestions_test_routes(state: Arc<flapjack_http::handlers::AppState>) -> Router {
    let public_health_routes =
        flapjack_http::router::build_public_health_routes().with_state(state.clone());

    let query_suggestions_routes = Router::new()
        .route("/1/indexes", post(flapjack_http::handlers::create_index))
        .route("/1/indexes", get(flapjack_http::handlers::list_indices))
        .route(
            "/1/indexes/:indexName/batch",
            post(flapjack_http::handlers::add_documents),
        )
        .route(
            "/1/indexes/:indexName/query",
            post(flapjack_http::handlers::search).get(flapjack_http::handlers::search_get),
        )
        .route("/1/task/:task_id", get(flapjack_http::handlers::get_task))
        .route("/1/tasks/:task_id", get(flapjack_http::handlers::get_task))
        .route(
            "/1/configs",
            get(flapjack_http::handlers::query_suggestions::list_configs)
                .post(flapjack_http::handlers::query_suggestions::create_config),
        )
        .route(
            "/1/configs/:indexName",
            get(flapjack_http::handlers::query_suggestions::get_config)
                .put(flapjack_http::handlers::query_suggestions::update_config)
                .delete(flapjack_http::handlers::query_suggestions::delete_config),
        )
        .route(
            "/1/configs/:indexName/status",
            get(flapjack_http::handlers::query_suggestions::get_status),
        )
        .route(
            "/1/configs/:indexName/build",
            post(flapjack_http::handlers::query_suggestions::trigger_build),
        )
        .route(
            "/1/logs/:indexName",
            get(flapjack_http::handlers::query_suggestions::get_logs),
        )
        .with_state(state);

    Router::new()
        .merge(public_health_routes)
        .merge(query_suggestions_routes)
}

pub fn build_test_app_for_local_requests(admin_key: Option<&str>) -> (Router, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let app = build_test_app_for_data_dir(temp_dir.path(), admin_key);
    (app, temp_dir)
}

pub fn build_test_app_for_existing_data_dir(data_dir: &Path, admin_key: Option<&str>) -> Router {
    build_test_app_for_data_dir(data_dir, admin_key)
}

fn build_test_app_for_data_dir(data_dir: &Path, admin_key: Option<&str>) -> Router {
    let (analytics_collector, analytics_engine) =
        build_analytics_runtime(analytics_config(data_dir, 10_000));

    let key_store = admin_key.map(|k| {
        let ks = Arc::new(flapjack_http::auth::KeyStore::load_or_create(data_dir, k));
        // Write admin key to .admin_key file for consistency
        std::fs::write(data_dir.join(".admin_key"), k).ok();
        ks
    });

    let state = make_test_app_state(
        data_dir,
        None,
        key_store.clone(),
        None,
        Some(Arc::clone(&analytics_engine)),
        Some(Arc::new(
            flapjack::experiments::store::ExperimentStore::new(data_dir).unwrap(),
        )),
        None,
    );

    flapjack_http::router::build_router(
        state,
        key_store,
        analytics_collector,
        default_trusted_proxy_matcher(),
        CorsMode::LoopbackOnly,
        data_dir,
    )
}

pub async fn spawn_server_with_key(admin_key: Option<&str>) -> (String, TempDir) {
    let (app, mut temp_dir) = build_test_app_for_local_requests(admin_key);
    let addr = spawn_router(app, &mut temp_dir).await;

    (addr, temp_dir)
}

pub async fn spawn_server_with_qs_analytics(source_index_name: &str) -> (String, TempDir) {
    let mut temp_dir = TempDir::new().unwrap();

    let analytics_config = analytics_config(temp_dir.path(), 100_000);

    // Seed 30 days of analytics directly to disk (no HTTP roundtrip needed)
    flapjack::analytics::seed::seed_analytics(&analytics_config, source_index_name, 30)
        .expect("Failed to seed analytics data");

    let (analytics_collector, analytics_engine) = build_analytics_runtime(analytics_config);

    let state = make_test_app_state(
        temp_dir.path(),
        None,
        None,
        None,
        Some(Arc::clone(&analytics_engine)),
        None,
        None,
    );

    let analytics_routes = Router::new()
        .route(
            "/2/conversions/addToCartRate",
            get(flapjack_http::handlers::analytics::get_add_to_cart_rate),
        )
        .route(
            "/2/conversions/purchaseRate",
            get(flapjack_http::handlers::analytics::get_purchase_rate),
        )
        .route(
            "/2/conversions/revenue",
            get(flapjack_http::handlers::analytics::get_revenue),
        )
        .route(
            "/2/countries",
            get(flapjack_http::handlers::analytics::get_countries),
        )
        .with_state(analytics_engine);

    let app = build_query_suggestions_test_routes(state)
        .merge(analytics_routes)
        .merge(
            Router::new()
                .route(
                    "/1/events",
                    post(flapjack_http::handlers::insights::post_events),
                )
                .with_state(analytics_collector.clone()),
        )
        .merge(
            Router::new()
                .route(
                    "/1/usertokens/:userToken",
                    delete(flapjack_http::handlers::insights::delete_usertoken),
                )
                .with_state(flapjack_http::handlers::insights::GdprDeleteState {
                    analytics_collector,
                    profile_store_base_path: temp_dir.path().to_path_buf(),
                }),
        );

    let app = apply_standard_test_http_layers(app);

    let addr = spawn_router(app, &mut temp_dir).await;

    (addr, temp_dir)
}

/// Spawn a Query Suggestions test server without an analytics engine so tests can
/// validate graceful behavior when `AppState.analytics_engine` is `None`.
pub async fn spawn_server_without_analytics() -> (String, TempDir) {
    let mut temp_dir = TempDir::new().unwrap();
    let state = make_test_app_state(temp_dir.path(), None, None, None, None, None, None);

    let app = apply_standard_test_http_layers(build_query_suggestions_test_routes(state));

    let addr = spawn_router(app, &mut temp_dir).await;

    (addr, temp_dir)
}

/// Poll until a document exists in the target tenant.
pub async fn wait_for_document_exists(
    manager: &flapjack::IndexManager,
    tenant: &str,
    doc_id: &str,
) {
    for _ in 0..200 {
        if manager.get_document(tenant, doc_id).unwrap().is_some() {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {tenant}/{doc_id} to exist");
}

/// Poll until a text field reaches the expected value.
pub async fn wait_for_document_text_field(
    manager: &flapjack::IndexManager,
    tenant: &str,
    doc_id: &str,
    field: &str,
    expected: &str,
) {
    for _ in 0..200 {
        if let Ok(Some(doc)) = manager.get_document(tenant, doc_id) {
            if matches!(doc.fields.get(field), Some(flapjack::types::FieldValue::Text(value)) if value == expected)
            {
                return;
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {tenant}/{doc_id} field {field}={expected}");
}

// ── Replication test helpers ──────────────────────────────────────────────────

/// Build an Axum router with core write/read handlers + all internal replication
/// endpoints. No auth, no analytics, no QS. Used by replication test helpers.
///
/// Internal routes mounted: /internal/replicate, /internal/ops,
/// /internal/status, /internal/cluster/status, /internal/analytics-rollup.
fn build_node_router(state: Arc<flapjack_http::handlers::AppState>) -> Router {
    let public_health_routes =
        flapjack_http::router::build_public_health_routes().with_state(state.clone());

    let internal = Router::new()
        .route(
            "/internal/replicate",
            post(flapjack_http::handlers::internal::replicate_ops),
        )
        .route(
            "/internal/ops",
            get(flapjack_http::handlers::internal::get_ops),
        )
        .route(
            "/internal/tenants",
            get(flapjack_http::handlers::internal::list_tenants),
        )
        .route(
            "/internal/snapshot/:tenantId",
            get(flapjack_http::handlers::internal::internal_snapshot),
        )
        .route(
            "/internal/status",
            get(flapjack_http::handlers::internal::replication_status),
        )
        .route(
            "/internal/cluster/status",
            get(flapjack_http::handlers::internal::cluster_status),
        )
        .route(
            "/internal/analytics-rollup",
            post(flapjack_http::handlers::internal::receive_analytics_rollup),
        )
        .route(
            "/internal/rollup-cache",
            get(flapjack_http::handlers::internal::rollup_cache_status),
        )
        .with_state(state.clone());

    let docs = Router::new()
        .route(
            "/1/indexes",
            post(flapjack_http::handlers::create_index).get(flapjack_http::handlers::list_indices),
        )
        .route(
            "/1/indexes/:indexName/settings",
            get(flapjack_http::handlers::get_settings).put(flapjack_http::handlers::set_settings),
        )
        .route(
            "/1/indexes/:indexName/batch",
            post(flapjack_http::handlers::add_documents),
        )
        .route(
            "/1/indexes/:indexName/query",
            post(flapjack_http::handlers::search).get(flapjack_http::handlers::search_get),
        )
        .route(
            "/1/indexes/:indexName/clear",
            post(flapjack_http::handlers::clear_index),
        )
        .route(
            "/1/indexes/:indexName/operation",
            post(flapjack_http::handlers::operation_index),
        )
        .route(
            "/1/indexes/:indexName/synonyms/:objectID",
            get(flapjack_http::handlers::get_synonym)
                .put(flapjack_http::handlers::save_synonym)
                .delete(flapjack_http::handlers::delete_synonym),
        )
        .route(
            "/1/indexes/:indexName/synonyms/batch",
            post(flapjack_http::handlers::save_synonyms),
        )
        .route(
            "/1/indexes/:indexName/rules/:objectID",
            get(flapjack_http::handlers::get_rule)
                .put(flapjack_http::handlers::save_rule)
                .delete(flapjack_http::handlers::delete_rule),
        )
        .route(
            "/1/indexes/:indexName/rules/batch",
            post(flapjack_http::handlers::save_rules),
        )
        .route(
            "/1/indexes/:indexName/:objectID",
            get(flapjack_http::handlers::get_object)
                .delete(flapjack_http::handlers::delete_object)
                .put(flapjack_http::handlers::put_object),
        )
        .route("/1/task/:task_id", get(flapjack_http::handlers::get_task))
        .route("/1/tasks/:task_id", get(flapjack_http::handlers::get_task))
        .with_state(state);

    apply_test_app_id_layer(
        Router::new()
            .merge(public_health_routes)
            .merge(internal)
            .merge(docs),
    )
}

fn replication_manager_for_peers(
    node_id: &str,
    peers: Vec<(String, String)>,
    admin_key: Option<&str>,
) -> Arc<ReplicationManager> {
    let peer_configs = peers
        .into_iter()
        .map(|(peer_id, addr)| PeerConfig {
            node_id: peer_id,
            addr,
        })
        .collect();
    ReplicationManager::new(
        NodeConfig {
            node_id: node_id.to_string(),
            bind_addr: "127.0.0.1:0".to_string(),
            peers: peer_configs,
        },
        admin_key.map(|value| value.to_string()),
    )
}

/// Build replication-aware AppState for direct catch-up tests without binding a server.
pub fn build_replication_state_for_existing_dir_with_peers(
    data_dir: &Path,
    node_id: &str,
    peers: Vec<(String, String)>,
) -> Arc<flapjack_http::handlers::AppState> {
    let repl_mgr = replication_manager_for_peers(node_id, peers, None);
    make_test_app_state(data_dir, None, None, Some(repl_mgr), None, None, None)
}

/// Spawn a standalone server with all write endpoints + internal replication
/// endpoints. The replication_manager is None (standalone mode), but /internal/ops
/// still serves oplog entries — used for startup catch-up testing.
///
/// The `_node_id` parameter is unused for now but makes call-sites self-documenting.
pub async fn spawn_server_with_internal(_node_id: &str) -> (String, TempDir) {
    let mut temp_dir = TempDir::new().unwrap();
    let state = make_test_app_state(temp_dir.path(), None, None, None, None, None, None);

    let app = build_node_router(state);
    let addr = spawn_router(app, &mut temp_dir).await;

    (addr, temp_dir)
}

/// Bind two listeners and create a replication pair that points each node at the other.
async fn bind_replication_pair(
    node_a_id: &str,
    node_b_id: &str,
    admin_key: Option<&str>,
) -> (
    TcpListener,
    TcpListener,
    Arc<ReplicationManager>,
    Arc<ReplicationManager>,
) {
    let listener_a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listener_b = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_a = listener_a.local_addr().unwrap();
    let addr_b = listener_b.local_addr().unwrap();
    let make = |id: &str, bind: std::net::SocketAddr, peer_id: &str, peer: std::net::SocketAddr| {
        ReplicationManager::new(
            NodeConfig {
                node_id: id.to_string(),
                bind_addr: bind.to_string(),
                peers: vec![PeerConfig {
                    node_id: peer_id.to_string(),
                    addr: format!("http://{peer}"),
                }],
            },
            admin_key.map(|k| k.to_string()),
        )
    };
    let repl_a = make(node_a_id, addr_a, node_b_id, addr_b);
    let repl_b = make(node_b_id, addr_b, node_a_id, addr_a);
    (listener_a, listener_b, repl_a, repl_b)
}

/// Spawn a replication pair with node lifecycles tied to TempDir drop.
/// Delegates to `spawn_stoppable_replication_pair` and attaches nodes to dirs.
pub async fn spawn_replication_pair(
    node_a_id: &str,
    node_b_id: &str,
) -> (String, String, TempDir, TempDir) {
    let (node_a, node_b, mut tmp_a, mut tmp_b) =
        spawn_stoppable_replication_pair(node_a_id, node_b_id).await;
    let addr_a = node_a.addr.clone();
    let addr_b = node_b.addr.clone();
    tmp_a.attach_node(node_a);
    tmp_b.attach_node(node_b);
    (addr_a, addr_b, tmp_a, tmp_b)
}

pub async fn spawn_authenticated_replication_pair(
    node_a_id: &str,
    node_b_id: &str,
    admin_key: &str,
) -> (String, String, String, TempDir, TempDir) {
    let (listener_a, listener_b, repl_a, repl_b) =
        bind_replication_pair(node_a_id, node_b_id, Some(admin_key)).await;

    let mut tmp_a = TempDir::new().unwrap();
    let mut tmp_b = TempDir::new().unwrap();

    let key_store_a = Arc::new(flapjack_http::auth::KeyStore::load_or_create(
        tmp_a.path(),
        admin_key,
    ));
    let key_store_b = Arc::new(flapjack_http::auth::KeyStore::load_or_create(
        tmp_b.path(),
        admin_key,
    ));

    let (collector_a, analytics_engine_a) =
        build_analytics_runtime(analytics_config(tmp_a.path(), 10_000));
    let (collector_b, analytics_engine_b) =
        build_analytics_runtime(analytics_config(tmp_b.path(), 10_000));

    let state_a = make_test_app_state(
        tmp_a.path(),
        None,
        Some(key_store_a.clone()),
        Some(repl_a),
        Some(analytics_engine_a),
        None,
        None,
    );
    let state_b = make_test_app_state(
        tmp_b.path(),
        None,
        Some(key_store_b.clone()),
        Some(repl_b),
        Some(analytics_engine_b),
        None,
        None,
    );

    let trusted = default_trusted_proxy_matcher();

    let app_a = flapjack_http::router::build_router(
        state_a,
        Some(key_store_a),
        collector_a,
        trusted.clone(),
        CorsMode::LoopbackOnly,
        tmp_a.path(),
    );
    let app_b = flapjack_http::router::build_router(
        state_b,
        Some(key_store_b),
        collector_b,
        trusted,
        CorsMode::LoopbackOnly,
        tmp_b.path(),
    );

    let node_a = serve_with_shutdown(listener_a, app_a);
    let node_b = serve_with_shutdown(listener_b, app_b);
    let addr_a_str = node_a.addr.clone();
    let addr_b_str = node_b.addr.clone();
    tmp_a.attach_node(node_a);
    tmp_b.attach_node(node_b);
    wait_for_health(&[&addr_a_str, &addr_b_str], 200).await;

    (addr_a_str, addr_b_str, admin_key.to_string(), tmp_a, tmp_b)
}

// ── Restartable node helpers ──────────────────────────────────────────────────

fn serve_with_shutdown(listener: TcpListener, app: Router) -> TestNode {
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (stopped_tx, stopped_rx) = std::sync::mpsc::channel();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        let _ = stopped_tx.send(());
    });
    TestNode {
        addr,
        shutdown_tx: Some(tx),
        stopped_rx: Some(stopped_rx),
        handle,
    }
}

pub async fn spawn_stoppable_replication_pair(
    node_a_id: &str,
    node_b_id: &str,
) -> (TestNode, TestNode, TempDir, TempDir) {
    let (listener_a, listener_b, repl_a, repl_b) =
        bind_replication_pair(node_a_id, node_b_id, None).await;

    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();

    let state_a = make_test_app_state(tmp_a.path(), None, None, Some(repl_a), None, None, None);
    let state_b = make_test_app_state(tmp_b.path(), None, None, Some(repl_b), None, None, None);

    let app_a = build_node_router(state_a);
    let app_b = build_node_router(state_b);

    let node_a = serve_with_shutdown(listener_a, app_a);
    let node_b = serve_with_shutdown(listener_b, app_b);
    wait_for_health(&[&node_a.addr, &node_b.addr], 200).await;

    (node_a, node_b, tmp_a, tmp_b)
}

/// Spawn a replication node on an existing data directory (for restart tests).
/// Runs pre-serve catch-up from peers before accepting traffic.
pub async fn spawn_replication_node_on_existing_dir(
    data_dir: &Path,
    node_id: &str,
    peer_url: &str,
    peer_id: &str,
) -> TestNode {
    try_spawn_replication_node_on_existing_dir(data_dir, node_id, peer_url, peer_id)
        .await
        .expect("replication node should finish pre-serve catch-up before serving")
}

pub async fn try_spawn_replication_node_on_existing_dir(
    data_dir: &Path,
    node_id: &str,
    peer_url: &str,
    peer_id: &str,
) -> Result<TestNode, String> {
    try_spawn_replication_node_on_existing_dir_with_peers(
        data_dir,
        node_id,
        vec![(peer_id.to_string(), peer_url.to_string())],
    )
    .await
}

/// Spawn a replication node on an existing data directory with multiple peers.
pub async fn try_spawn_replication_node_on_existing_dir_with_peers(
    data_dir: &Path,
    node_id: &str,
    peers: Vec<(String, String)>,
) -> Result<TestNode, String> {
    let repl_mgr = replication_manager_for_peers(node_id, peers, None);

    let state = make_test_app_state(data_dir, None, None, Some(repl_mgr), None, None, None);

    // Run pre-serve catch-up: fetch missed ops from peers before accepting traffic.
    flapjack_http::startup_catchup::run_pre_serve_catchup(&state).await?;

    let app = build_node_router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let node = serve_with_shutdown(listener, app);
    wait_for_health(&[&node.addr], 100).await;
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::{build_node_router, make_test_app_state};
    use axum::http::{Method, StatusCode};
    use flapjack::dictionaries::DEFAULT_DICTIONARY_TENANT;
    use tempfile::tempdir;

    #[tokio::test]
    async fn build_node_router_injects_default_app_id_for_internal_ops_requests() {
        let tmp = tempdir().expect("tempdir");
        let state = make_test_app_state(tmp.path(), None, None, None, None, None, None);
        let app = build_node_router(state);

        let response = crate::common::http::send_empty_response(
            &app,
            Method::GET,
            &format!("/internal/ops?tenant_id={DEFAULT_DICTIONARY_TENANT}&since_seq=0"),
        )
        .await;
        let status = response.status();
        let body = crate::common::http::parse_response_json(response).await;

        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "default app-id layer should let internal ops reach the normal tenant-not-found path instead of a missing-extension 500: {body}"
        );
        assert_eq!(body["message"], "Tenant not found");
    }
}
