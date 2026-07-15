use crate::analytics_cluster;
use crate::auth::KeyStore;
use crate::conversation_store::ConversationStore;
use crate::handlers::AppState;
use crate::middleware::{TrustedProxyMatcher, DEFAULT_TRUSTED_PROXY_CIDRS};
use crate::notifications::{init_global_notifier, NotificationService};
use crate::pause_registry;
use crate::startup::ServerConfig;
use crate::usage_persistence::UsagePersistence;
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig, AnalyticsQueryEngine};
use flapjack::dictionaries::manager::DictionaryManager;
use flapjack::experiments::store::ExperimentStore;
use flapjack::recommend::RecommendConfig;
use flapjack::IndexManager;
use flapjack_replication::config::NodeConfig;
use flapjack_replication::manager::ReplicationManager;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::handlers::metrics::MetricsState;
use crate::usage_middleware::TenantUsageCounters;
use dashmap::DashMap;

pub(crate) struct InfrastructureState {
    pub manager: Arc<IndexManager>,
    pub dictionary_manager: Arc<DictionaryManager>,
    pub node_config: NodeConfig,
    pub bind_addr: String,
    pub trusted_proxy_matcher: Arc<TrustedProxyMatcher>,
    pub replication_manager: Option<Arc<ReplicationManager>>,
    pub ssl_manager: Option<Arc<flapjack::SslManager>>,
    pub analytics_config: AnalyticsConfig,
    pub analytics_collector: Arc<AnalyticsCollector>,
    pub analytics_engine: Arc<AnalyticsQueryEngine>,
    pub metrics_state: Option<MetricsState>,
    pub usage_counters: Arc<DashMap<String, TenantUsageCounters>>,
    pub usage_persistence: Option<Arc<UsagePersistence>>,
    pub geoip_reader: Option<Arc<crate::geoip::GeoIpReader>>,
    pub notification_service: Option<Arc<NotificationService>>,
    pub s3_config: Option<flapjack::index::s3::S3Config>,
    pub s3_snapshot_interval_secs: Option<u64>,
    #[cfg(feature = "otel")]
    pub otel_guard: Option<crate::otel::OtelGuard>,
}

/// Lightweight startup dependency projection used for summary logging and unit tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupSummary {
    pub s3_snapshots_enabled: bool,
    pub s3_snapshot_interval_secs: Option<u64>,
    pub replication_peer_count: usize,
    pub ssl_enabled: bool,
    pub analytics_enabled: bool,
    pub geoip_enabled: bool,
    pub vector_search_compiled: bool,
    pub auth_enabled: bool,
}

impl StartupSummary {
    pub(crate) fn from_infrastructure(infra: &InfrastructureState, auth_enabled: bool) -> Self {
        Self {
            s3_snapshots_enabled: infra.s3_config.is_some(),
            s3_snapshot_interval_secs: infra.s3_snapshot_interval_secs,
            replication_peer_count: infra.node_config.peers.len(),
            ssl_enabled: infra.ssl_manager.is_some(),
            analytics_enabled: infra.analytics_config.enabled,
            geoip_enabled: infra.geoip_reader.is_some(),
            vector_search_compiled: cfg!(feature = "vector-search"),
            auth_enabled,
        }
    }
}

pub(crate) fn log_startup_summary(summary: &StartupSummary) {
    tracing::info!(
        s3_snapshots_enabled = summary.s3_snapshots_enabled,
        s3_snapshot_interval_secs = ?summary.s3_snapshot_interval_secs,
        replication_peer_count = summary.replication_peer_count,
        ssl_enabled = summary.ssl_enabled,
        analytics_enabled = summary.analytics_enabled,
        geoip_enabled = summary.geoip_enabled,
        vector_search_compiled = summary.vector_search_compiled,
        auth_enabled = summary.auth_enabled,
        "[startup] Dependency status summary"
    );
}

pub(crate) async fn initialize_infrastructure(
    server_config: &ServerConfig,
    data_dir: &Path,
    admin_key: Option<String>,
) -> Result<InfrastructureState, Box<dyn std::error::Error>> {
    let manager = IndexManager::new(data_dir);
    let dictionary_manager = Arc::new(flapjack::dictionaries::manager::DictionaryManager::new(
        data_dir,
    ));
    manager.set_dictionary_manager(Arc::clone(&dictionary_manager));

    let node_config =
        flapjack_replication::config::NodeConfig::load_or_default(Path::new(data_dir));
    log_bind_address_resolution(&node_config, server_config, data_dir);
    let bind_addr = node_config.bind_addr.clone();

    let trusted_proxy_matcher = initialize_trusted_proxies()?;
    initialize_analytics_cluster(&node_config);
    let replication_manager = initialize_replication(&node_config, admin_key);
    let ssl_manager = initialize_ssl_manager().await;
    let (s3_config, s3_snapshot_interval_secs) = initialize_s3(data_dir, &manager).await;

    let (analytics_config, analytics_collector, analytics_engine) =
        initialize_analytics_subsystem();
    manager.set_analytics_config(analytics_config.clone());
    let metrics_state = Some(crate::handlers::metrics::MetricsState::new());
    let usage_counters = Arc::new(dashmap::DashMap::new());
    let usage_persistence = initialize_usage_persistence(data_dir, &usage_counters);
    let geoip_reader = initialize_geoip(data_dir);
    let notification_service = initialize_notification_service().await;

    Ok(InfrastructureState {
        manager,
        dictionary_manager,
        node_config,
        bind_addr,
        trusted_proxy_matcher,
        replication_manager,
        ssl_manager,
        analytics_config,
        analytics_collector,
        analytics_engine,
        metrics_state,
        usage_counters,
        usage_persistence,
        geoip_reader,
        notification_service,
        s3_config,
        s3_snapshot_interval_secs,
        #[cfg(feature = "otel")]
        otel_guard: None,
    })
}

/// Logs the resolved bind address, node identity, and data directory at startup.
fn log_bind_address_resolution(
    node_config: &NodeConfig,
    server_config: &ServerConfig,
    data_dir: &Path,
) {
    let node_json_path = data_dir.join("node.json");
    if !node_json_path.exists() {
        return;
    }
    if node_config.bind_addr != server_config.bind_addr {
        tracing::warn!(
            requested = %server_config.bind_addr,
            node_json_bind_addr = %node_config.bind_addr,
            node_json = %node_json_path.display(),
            "bind address loaded from node.json overrides FLAPJACK_BIND_ADDR"
        );
    } else {
        tracing::info!(
            node_json_bind_addr = %node_config.bind_addr,
            node_json = %node_json_path.display(),
            "bind address loaded from node.json"
        );
    }
}

/// Parses `FLAPJACK_TRUSTED_PROXY_CIDRS` into a `TrustedProxyMatcher` for
/// extracting real client IPs from forwarded headers behind reverse proxies.
fn initialize_trusted_proxies() -> Result<Arc<TrustedProxyMatcher>, Box<dyn std::error::Error>> {
    let trusted_proxy_cidrs_raw = std::env::var("FLAPJACK_TRUSTED_PROXY_CIDRS").ok();
    let matcher = Arc::new(
        TrustedProxyMatcher::from_optional_csv(trusted_proxy_cidrs_raw.as_deref())
            .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?,
    );
    if matcher.is_empty() {
        tracing::info!(
            "Trusted proxy header forwarding disabled (set FLAPJACK_TRUSTED_PROXY_CIDRS to CIDRs to enable)"
        );
    } else if trusted_proxy_cidrs_raw.is_some() {
        tracing::info!(
            trusted_proxy_ranges = matcher.len(),
            "Trusted proxy header forwarding enabled via FLAPJACK_TRUSTED_PROXY_CIDRS"
        );
    } else {
        tracing::info!(
            trusted_proxy_ranges = matcher.len(),
            default_trusted_proxy_cidrs = DEFAULT_TRUSTED_PROXY_CIDRS,
            "Trusted proxy header forwarding enabled with secure defaults"
        );
    }
    Ok(matcher)
}

fn initialize_analytics_cluster(node_config: &NodeConfig) {
    if let Some(cluster_client) = analytics_cluster::AnalyticsClusterClient::new(node_config) {
        analytics_cluster::set_global_cluster(cluster_client);
        tracing::info!(
            "[HA-analytics] Cluster analytics enabled: fan-out to {} peers",
            node_config.peers.len()
        );
    }
}

fn initialize_replication(
    node_config: &NodeConfig,
    admin_key: Option<String>,
) -> Option<Arc<ReplicationManager>> {
    if !node_config.peers.is_empty() {
        tracing::info!("Replication enabled: {} peers", node_config.peers.len());
        let repl = ReplicationManager::new(node_config.clone(), admin_key);
        flapjack_replication::set_global_manager(Arc::clone(&repl));
        Some(repl)
    } else {
        tracing::info!("Replication disabled (no peers in node.json)");
        None
    }
}

/// Initializes the SSL/TLS manager from environment configuration.
async fn initialize_ssl_manager() -> Option<Arc<flapjack::SslManager>> {
    match flapjack::SslConfig::from_env() {
        Ok(ssl_config) => {
            tracing::info!(
                "[SSL] SSL management enabled for IP: {}",
                ssl_config.public_ip
            );
            match flapjack::SslManager::new(ssl_config).await {
                Ok(mgr) => {
                    flapjack_ssl::set_global_manager(Arc::clone(&mgr));
                    Some(mgr)
                }
                Err(e) => {
                    tracing::error!("[SSL] Failed to initialize SSL manager: {}", e);
                    None
                }
            }
        }
        Err(e) => {
            tracing::info!("[SSL] SSL management disabled: {}", e);
            None
        }
    }
}

/// Initializes S3 snapshot configuration and restores any existing remote snapshots.
async fn initialize_s3(
    data_dir: &Path,
    manager: &Arc<IndexManager>,
) -> (Option<flapjack::index::s3::S3Config>, Option<u64>) {
    if let Some(config) = flapjack::index::s3::S3Config::from_env() {
        crate::background_tasks::auto_restore_from_s3(
            &data_dir.to_string_lossy(),
            &config,
            manager,
        )
        .await;
        let interval_secs: u64 = std::env::var("FLAPJACK_SNAPSHOT_INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        (Some(config), Some(interval_secs).filter(|secs| *secs > 0))
    } else {
        (None, None)
    }
}

/// Initializes the analytics subsystem: config, event collector, and query engine.
fn initialize_analytics_subsystem() -> (
    AnalyticsConfig,
    Arc<AnalyticsCollector>,
    Arc<AnalyticsQueryEngine>,
) {
    let config = AnalyticsConfig::from_env();
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config.clone()));

    if config.enabled {
        flapjack::analytics::init_global_collector(Arc::clone(&collector));
        tracing::info!(
            "[analytics] Analytics enabled (flush every {}s, retain {}d)",
            config.flush_interval_secs,
            config.retention_days
        );
    } else {
        tracing::info!("[analytics] Analytics disabled");
    }
    (config, collector, engine)
}

/// Initializes per-tenant usage persistence and restores counters from disk.
fn initialize_usage_persistence(
    data_dir: &Path,
    usage_counters: &Arc<DashMap<String, TenantUsageCounters>>,
) -> Option<Arc<UsagePersistence>> {
    let persistence = match UsagePersistence::new(data_dir) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "[usage] Could not initialise usage persistence directory: {}",
                e
            );
            return None;
        }
    };
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    log_usage_reload_result(
        persistence.load_into_counters(&today, usage_counters),
        &today,
    );
    Some(Arc::new(persistence))
}

fn log_usage_reload_result(result: Result<bool, impl std::fmt::Display>, today: &str) {
    match result {
        Ok(true) => tracing::info!(
            "[usage] Reloaded today's usage counters from disk (date={})",
            today
        ),
        Ok(false) => tracing::info!(
            "[usage] No snapshot for today (date={}); starting fresh",
            today
        ),
        Err(e) => tracing::warn!("[usage] Failed to reload usage counters: {}", e),
    }
}

fn initialize_geoip(data_dir: &Path) -> Option<Arc<crate::geoip::GeoIpReader>> {
    let geoip_path = std::env::var("FLAPJACK_GEOIP_DB")
        .unwrap_or_else(|_| format!("{}/GeoLite2-City.mmdb", data_dir.to_string_lossy()));
    let reader = crate::geoip::GeoIpReader::new(Path::new(&geoip_path));
    if reader.is_some() {
        tracing::info!("[geoip] GeoIP database loaded from {}", geoip_path);
    } else {
        tracing::info!(
            "[geoip] GeoIP unavailable (no database at {}). IP geolocation disabled.",
            geoip_path
        );
    }
    reader.map(Arc::new)
}

async fn initialize_notification_service() -> Option<Arc<NotificationService>> {
    let service = Arc::new(NotificationService::new_from_env().await);
    init_global_notifier(Arc::clone(&service));
    if service.is_enabled() {
        tracing::info!("[notifications] Notification service ready");
    }
    Some(service)
}

/// Build the shared AppState once infrastructure, auth, and startup timing are ready.
pub(crate) fn initialize_state(
    infrastructure: &InfrastructureState,
    key_store: Option<Arc<KeyStore>>,
    data_dir: &str,
    startup_start: Instant,
) -> Result<Arc<AppState>, Box<dyn std::error::Error>> {
    Ok(Arc::new(AppState {
        manager: Arc::clone(&infrastructure.manager),
        key_store: key_store.clone(),
        replication_manager: infrastructure.replication_manager.clone(),
        ssl_manager: infrastructure.ssl_manager.clone(),
        analytics_engine: Some(Arc::clone(&infrastructure.analytics_engine)),
        recommend_config: RecommendConfig::from_env(),
        experiment_store: Some(Arc::new(ExperimentStore::new(Path::new(data_dir))?)),
        dictionary_manager: Arc::clone(&infrastructure.dictionary_manager),
        metrics_state: infrastructure.metrics_state.clone(),
        usage_counters: Arc::clone(&infrastructure.usage_counters),
        usage_persistence: infrastructure.usage_persistence.clone(),
        paused_indexes: pause_registry::PausedIndexes::new(),
        geoip_reader: infrastructure.geoip_reader.clone(),
        notification_service: infrastructure.notification_service.clone(),
        start_time: startup_start,
        conversation_store: ConversationStore::default_shared(),
        embedder_store: Arc::new(crate::embedder_store::EmbedderStore::new()),
        idempotency_cache: Arc::new(
            crate::idempotency::IdempotencyCache::from_env_with_data_dir(Path::new(data_dir)),
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::{log_startup_summary, StartupSummary};
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Clone)]
    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    impl TestWriter {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }

        fn output(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl std::io::Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TestWriter {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
    #[test]
    fn startup_summary_struct_fields_reflect_values() {
        let summary = StartupSummary {
            s3_snapshots_enabled: true,
            s3_snapshot_interval_secs: Some(900),
            replication_peer_count: 4,
            ssl_enabled: true,
            analytics_enabled: false,
            geoip_enabled: true,
            vector_search_compiled: true,
            auth_enabled: false,
        };

        assert!(summary.s3_snapshots_enabled);
        assert_eq!(summary.s3_snapshot_interval_secs, Some(900));
        assert_eq!(summary.replication_peer_count, 4);
        assert!(summary.ssl_enabled);
        assert!(!summary.analytics_enabled);
        assert!(summary.geoip_enabled);
        assert!(summary.vector_search_compiled);
        assert!(!summary.auth_enabled);
    }
    #[test]
    fn log_startup_summary_emits_single_structured_info_event() {
        let summary = StartupSummary {
            s3_snapshots_enabled: true,
            s3_snapshot_interval_secs: Some(600),
            replication_peer_count: 2,
            ssl_enabled: true,
            analytics_enabled: true,
            geoip_enabled: false,
            vector_search_compiled: true,
            auth_enabled: true,
        };
        let writer = TestWriter::new();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(writer.clone()),
        );

        tracing::subscriber::with_default(subscriber, || {
            log_startup_summary(&summary);
        });

        let output = writer.output();
        let lines: Vec<&str> = output.trim().lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "summary logger should emit exactly one event"
        );

        let parsed: Value = serde_json::from_str(lines[0]).expect("log line should be valid JSON");
        let fields = parsed
            .get("fields")
            .and_then(Value::as_object)
            .expect("JSON log should include fields object");

        assert_eq!(
            fields.get("message").and_then(Value::as_str),
            Some("[startup] Dependency status summary")
        );
        assert_eq!(
            fields.get("s3_snapshots_enabled").and_then(Value::as_bool),
            Some(true)
        );
        assert!(fields.contains_key("s3_snapshot_interval_secs"));
        assert_eq!(
            fields.get("replication_peer_count").and_then(Value::as_u64),
            Some(2)
        );
        assert_eq!(
            fields.get("ssl_enabled").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            fields.get("analytics_enabled").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            fields.get("geoip_enabled").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            fields
                .get("vector_search_compiled")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            fields.get("auth_enabled").and_then(Value::as_bool),
            Some(true)
        );
    }
}
