//! Shared test utilities for handler unit tests within the `flapjack-http` crate.
//!
//! This module is `#[cfg(test)]`-gated and `pub(crate)`, so it is only available to
//! inline `#[cfg(test)]` modules inside `flapjack-http/src/`. Integration tests in
//! `engine/tests/` must use `tests/common/mod.rs` instead.

use axum::body::Body;
use axum::http::{Method, Request};
use flapjack::analytics::{AnalyticsConfig, AnalyticsQueryEngine};
use flapjack::experiments::store::ExperimentStore;
use flapjack::recommend::RecommendConfig;
use flapjack_replication::manager::ReplicationManager;
use std::ffi::OsString;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

use crate::geoip::GeoIpReader;
use crate::handlers::AppState;

// ---------------------------------------------------------------------------
// Process-global env-var mutation helper
// ---------------------------------------------------------------------------

/// Mutex that serializes all process-global env-var mutations in test code.
/// Every test that needs to set/unset an env var MUST hold this lock for the
/// duration of the mutation to avoid data races with parallel tests.
pub(crate) static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard that restores a single env var to its previous value on drop.
/// Obtain via [`with_env_var`].
pub(crate) struct EnvGuard {
    name: String,
    previous: Option<OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        restore_env_var(&self.name, self.previous.take());
    }
}

/// Set an env var for the duration of the returned guard's lifetime.
/// The previous value (or absence) is restored when the guard drops.
/// Acquires `ENV_MUTEX` so concurrent tests cannot observe partial state.
pub(crate) fn with_env_var(name: &str, value: &str) -> EnvGuard {
    let lock = ENV_MUTEX.lock().expect("env mutex poisoned");
    let previous = std::env::var_os(name);
    std::env::set_var(name, value);
    EnvGuard {
        name: name.to_owned(),
        previous,
        _lock: lock,
    }
}

pub(crate) fn restore_env_var(name: &str, previous: Option<OsString>) {
    match previous {
        Some(value) => std::env::set_var(name, value),
        None => std::env::remove_var(name),
    }
}

pub(crate) struct EnvVarRestoreGuard {
    name: &'static str,
    previous: Option<OsString>,
}

impl EnvVarRestoreGuard {
    pub(crate) fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }

    pub(crate) fn remove(name: &'static str) -> Self {
        let previous = std::env::var_os(name);
        std::env::remove_var(name);
        Self { name, previous }
    }
}

impl Drop for EnvVarRestoreGuard {
    fn drop(&mut self) {
        restore_env_var(self.name, self.previous.take());
    }
}

pub(crate) struct TestStateBuilder<'tmp> {
    tmp: &'tmp TempDir,
    analytics_engine: Option<Arc<AnalyticsQueryEngine>>,
    experiment_store: Option<Arc<ExperimentStore>>,
    geoip_reader: Option<Arc<GeoIpReader>>,
    replication_manager: Option<Arc<ReplicationManager>>,
}

impl<'tmp> TestStateBuilder<'tmp> {
    pub(crate) fn new(tmp: &'tmp TempDir) -> Self {
        Self {
            tmp,
            analytics_engine: None,
            experiment_store: None,
            geoip_reader: None,
            replication_manager: None,
        }
    }

    pub(crate) fn with_analytics(mut self) -> Self {
        let analytics_config = AnalyticsConfig {
            enabled: true,
            data_dir: self.tmp.path().join("analytics"),
            flush_interval_secs: 3600,
            flush_size: 100_000,
            retention_days: 90,
        };
        self.analytics_engine = Some(Arc::new(AnalyticsQueryEngine::new(analytics_config)));
        self
    }

    pub(crate) fn with_analytics_engine(
        mut self,
        analytics_engine: Arc<AnalyticsQueryEngine>,
    ) -> Self {
        self.analytics_engine = Some(analytics_engine);
        self
    }

    pub(crate) fn with_experiments(mut self) -> Self {
        self.experiment_store = Some(Arc::new(ExperimentStore::new(self.tmp.path()).unwrap()));
        self
    }

    pub(crate) fn with_geoip(mut self, geoip_reader: Arc<GeoIpReader>) -> Self {
        self.geoip_reader = Some(geoip_reader);
        self
    }

    pub(crate) fn with_replication_manager(
        mut self,
        replication_manager: Arc<ReplicationManager>,
    ) -> Self {
        self.replication_manager = Some(replication_manager);
        self
    }

    pub(crate) fn build(self) -> AppState {
        let manager = flapjack::IndexManager::new(self.tmp.path());
        let dictionary_manager = Arc::new(flapjack::dictionaries::manager::DictionaryManager::new(
            self.tmp.path(),
        ));
        manager.set_dictionary_manager(Arc::clone(&dictionary_manager));

        AppState {
            manager,
            key_store: None,
            replication_manager: self.replication_manager,
            ssl_manager: None,
            analytics_engine: self.analytics_engine,
            recommend_config: RecommendConfig::default(),
            experiment_store: self.experiment_store,
            dictionary_manager,
            metrics_state: Some(crate::handlers::metrics::MetricsState::new()),
            usage_counters: Arc::new(dashmap::DashMap::new()),
            usage_persistence: None,
            notification_service: None,
            paused_indexes: crate::pause_registry::PausedIndexes::new(),
            geoip_reader: self.geoip_reader,
            start_time: std::time::Instant::now(),
            conversation_store: crate::conversation_store::ConversationStore::default_shared(),
            embedder_store: Arc::new(crate::embedder_store::EmbedderStore::new()),
            idempotency_cache: Arc::new(
                crate::idempotency::IdempotencyCache::from_env_with_data_dir(self.tmp.path()),
            ),
        }
    }

    pub(crate) fn build_shared(self) -> Arc<AppState> {
        Arc::new(self.build())
    }
}

/// Parse an axum response body as JSON.
pub(crate) async fn body_json(resp: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Send a JSON request through a router and return the raw response.
pub(crate) async fn send_json_request(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: serde_json::Value,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Build a full test router backed by `build_router` for the given data directory.
pub(crate) fn build_test_router_for_data_dir(
    tmp: &TempDir,
    key_store: Option<Arc<crate::auth::KeyStore>>,
    data_dir: &std::path::Path,
) -> axum::Router {
    let state = TestStateBuilder::new(tmp).with_analytics().build_shared();
    let analytics_config = AnalyticsConfig {
        enabled: false,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 60,
        flush_size: 1000,
        retention_days: 30,
    };
    let analytics_collector = flapjack::analytics::AnalyticsCollector::new(analytics_config);
    let trusted_proxy_matcher =
        Arc::new(crate::middleware::TrustedProxyMatcher::from_optional_csv(None).unwrap());

    crate::router::build_router(
        state,
        key_store,
        analytics_collector,
        trusted_proxy_matcher,
        crate::startup::CorsMode::LoopbackOnly,
        data_dir,
    )
}

/// Build a full test router using `tmp.path()` as the data directory.
pub(crate) fn build_test_router(
    tmp: &TempDir,
    key_store: Option<Arc<crate::auth::KeyStore>>,
) -> axum::Router {
    build_test_router_for_data_dir(tmp, key_store, tmp.path())
}

/// Send a request with an empty body through a router and return the raw response.
pub(crate) async fn send_empty_request(
    app: &axum::Router,
    method: Method,
    uri: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[tokio::test]
    async fn test_state_builder_defaults_match_expected_shape() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build();
        let manager_dictionary_manager = state
            .manager
            .dictionary_manager()
            .expect("manager should have dictionary manager wired");

        assert_eq!(state.manager.base_path, tmp.path());
        assert!(state.key_store.is_none());
        assert!(state.replication_manager.is_none());
        assert!(state.ssl_manager.is_none());
        assert!(state.analytics_engine.is_none());
        assert!(state.experiment_store.is_none());
        assert!(Arc::ptr_eq(
            manager_dictionary_manager,
            &state.dictionary_manager
        ));
        assert!(state.metrics_state.is_some());
        assert!(state.usage_counters.is_empty());
        assert!(state.usage_persistence.is_none());
        assert!(state.geoip_reader.is_none());
        assert!(state.notification_service.is_none());
    }

    #[tokio::test]
    async fn test_state_builder_with_analytics_enables_analytics_engine() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).with_analytics().build();

        assert!(state.analytics_engine.is_some());
    }

    #[tokio::test]
    async fn test_state_builder_with_experiments_enables_experiment_store() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).with_experiments().build();

        assert!(state.experiment_store.is_some());
    }

    #[tokio::test]
    async fn test_state_builder_with_geoip_sets_geoip_reader() {
        let db_path = std::env::var("FLAPJACK_TEST_GEOIP_DB").unwrap_or_default();
        if db_path.is_empty() {
            eprintln!(
                "Skipping test_state_builder_with_geoip_sets_geoip_reader: FLAPJACK_TEST_GEOIP_DB not set"
            );
            return;
        }

        let tmp = TempDir::new().unwrap();
        let reader = crate::geoip::GeoIpReader::new(Path::new(&db_path))
            .expect("expected valid GeoIP db for FLAPJACK_TEST_GEOIP_DB");
        let state = TestStateBuilder::new(&tmp)
            .with_geoip(Arc::new(reader))
            .build();

        assert!(state.geoip_reader.is_some());
    }
}
