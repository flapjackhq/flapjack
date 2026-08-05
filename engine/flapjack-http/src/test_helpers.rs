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
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

use crate::geoip::GeoIpReader;
use crate::handlers::migration::TEST_ALGOLIA_BASE_URL_ENV;
use crate::handlers::AppState;

#[derive(Clone, Default)]
pub(crate) struct SharedLogBuffer {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl SharedLogBuffer {
    pub(crate) fn contents(&self) -> String {
        String::from_utf8(self.buffer.lock().unwrap().clone()).unwrap()
    }
}

pub(crate) struct SharedLogWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl io::Write for SharedLogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffer.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for SharedLogBuffer {
    type Writer = SharedLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        SharedLogWriter {
            buffer: Arc::clone(&self.buffer),
        }
    }
}

// ---------------------------------------------------------------------------
// Process-global env-var mutation helper
// ---------------------------------------------------------------------------

thread_local! {
    /// True while THIS thread holds [`ENV_MUTEX`].
    ///
    /// This is the single source of truth for "am I already inside the env
    /// lock?", and it is maintained by [`EnvMutex::lock`] itself, so every
    /// acquisition path keeps it accurate for free. It deliberately does not
    /// live next to any individual guard type: a flag maintained by only some
    /// of the acquisition paths is worse than no flag, because readers trust
    /// it and then deadlock. See `EnvMutex` for the full rationale.
    static ENV_LOCK_HELD_BY_THIS_THREAD: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Whether the calling thread currently holds [`ENV_MUTEX`].
///
/// Production code that only *reads* an env var under test (for example the
/// Algolia base-URL override) must consult this before taking the lock: its
/// callers are frequently already inside it, and `ENV_MUTEX` is not reentrant.
pub(crate) fn current_thread_holds_env_lock() -> bool {
    ENV_LOCK_HELD_BY_THIS_THREAD.with(std::cell::Cell::get)
}

/// Serializes all process-global env-var mutation in test code, and owns the
/// per-thread "I already hold this" flag that makes re-entrant *reads* safe.
///
/// Why a wrapper instead of a bare `std::sync::Mutex<()>`:
///
/// 1. **Re-entrancy is a deadlock, and it happened.** `std::sync::Mutex` is not
///    reentrant, so a thread that re-locks it blocks forever at 0% CPU with no
///    diagnostic. Tests hold this lock across `await` on purpose (the whole
///    point is that no other test may change the environment mid-request), and
///    the handler under test can reach code that reads an env var. That reader
///    needs to know the lock is already held. Keeping the flag *here* means
///    all ~60 `ENV_MUTEX.lock()` call sites maintain it without being touched;
///    the previous arrangement kept the flag next to one guard type, so every
///    direct `lock()` left it false and the reader self-deadlocked.
/// 2. **Re-entry now fails loudly instead of hanging.** A hang is the worst
///    failure mode available: it is indistinguishable from "slow", it wedges
///    the entire test binary rather than one test, and it leaves no evidence.
///    A panic naming the offending thread costs seconds.
/// 3. **Poison must not cascade.** One test panicking while holding this lock
///    would otherwise poison it for the rest of the run, and the dozens of
///    `.expect(...)` call sites would each turn that into a second failure,
///    burying the original. Recovery happens here, once. `lock` still returns
///    `Result` so existing `.expect(..)` / `unwrap_or_else(PoisonError::into_inner)`
///    call sites compile unchanged — it simply never returns `Err`.
pub(crate) struct EnvMutex(std::sync::Mutex<()>);

/// RAII holder for [`ENV_MUTEX`]. Clearing the thread-local flag happens in
/// `Drop` before the inner `MutexGuard` field is dropped, so the flag is never
/// observed as `false` by another thread that has just taken the lock.
pub(crate) struct EnvLockGuard {
    _inner: std::sync::MutexGuard<'static, ()>,
}

impl EnvMutex {
    pub(crate) fn lock(
        &'static self,
    ) -> Result<EnvLockGuard, std::sync::PoisonError<EnvLockGuard>> {
        assert!(
            !current_thread_holds_env_lock(),
            "this thread already holds the env lock; re-locking a non-reentrant \
             Mutex deadlocks forever. Read the value under the lock you already \
             hold, or consult `current_thread_holds_env_lock()` first."
        );
        // Poison is recovered here and never surfaced: the guarded value is `()`
        // with no invariants, so a prior panic leaves nothing inconsistent, and
        // propagating it would only convert one real failure into many.
        let inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ENV_LOCK_HELD_BY_THIS_THREAD.with(|held| held.set(true));
        Ok(EnvLockGuard { _inner: inner })
    }

    /// Non-blocking acquisition. `None` means another thread holds the lock.
    ///
    /// Used by fixtures that must *prove* contention rather than wait it out.
    /// Re-entry is still a programming error and still panics: returning `None`
    /// for it would let a caller silently proceed to mutate the environment
    /// while believing it had failed to acquire.
    pub(crate) fn try_lock(&'static self) -> Option<EnvLockGuard> {
        assert!(
            !current_thread_holds_env_lock(),
            "this thread already holds the env lock; `try_lock` cannot succeed \
             and returning None here would hide a re-entrancy bug"
        );
        let inner = match self.0.try_lock() {
            Ok(inner) => inner,
            // Same rationale as `lock`: `()` has no invariants to corrupt.
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        ENV_LOCK_HELD_BY_THIS_THREAD.with(|held| held.set(true));
        Some(EnvLockGuard { _inner: inner })
    }
}

impl Drop for EnvLockGuard {
    fn drop(&mut self) {
        ENV_LOCK_HELD_BY_THIS_THREAD.with(|held| held.set(false));
    }
}

/// Mutex that serializes all process-global env-var mutations in test code.
/// Every test that needs to set/unset an env var MUST hold this lock for the
/// duration of the mutation to avoid data races with parallel tests.
pub(crate) static ENV_MUTEX: EnvMutex = EnvMutex(std::sync::Mutex::new(()));

fn reject_algolia_base_url_bypass(name: &str) {
    if name == TEST_ALGOLIA_BASE_URL_ENV {
        panic!("{TEST_ALGOLIA_BASE_URL_ENV} must be managed with AlgoliaBaseUrlEnvGuard");
    }
}

fn assert_env_var_restore_guard_holds_env_lock() {
    assert!(
        current_thread_holds_env_lock(),
        "EnvVarRestoreGuard::set/remove must be called while holding ENV_MUTEX; \
         take ENV_MUTEX for the full lifetime of the returned guard"
    );
}

/// RAII guard that restores a single env var to its previous value on drop.
/// Obtain via [`with_env_var`].
pub(crate) struct EnvGuard {
    name: String,
    previous: Option<OsString>,
    _lock: EnvLockGuard,
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
    reject_algolia_base_url_bypass(name);
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
        reject_algolia_base_url_bypass(name);
        assert_env_var_restore_guard_holds_env_lock();
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }

    pub(crate) fn remove(name: &'static str) -> Self {
        reject_algolia_base_url_bypass(name);
        assert_env_var_restore_guard_holds_env_lock();
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

/// Canonical guard for every test that reads or writes Algolia's base-URL
/// override. The crate-wide environment mutex keeps request planning isolated
/// from every process-environment mutation in parallel tests.
pub(crate) struct AlgoliaBaseUrlEnvGuard {
    previous: Option<OsString>,
    _lock: EnvLockGuard,
}

impl AlgoliaBaseUrlEnvGuard {
    /// Plan against the real vendor hosts, with no override in effect.
    pub(crate) fn vendor_hosts() -> Self {
        let lock = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::var_os(TEST_ALGOLIA_BASE_URL_ENV);
        std::env::remove_var(TEST_ALGOLIA_BASE_URL_ENV);
        Self {
            previous,
            _lock: lock,
        }
    }

    /// Plan against `base_url` instead of the vendor hosts.
    pub(crate) fn overridden_to(base_url: &str) -> Self {
        let guard = Self::vendor_hosts();
        std::env::set_var(TEST_ALGOLIA_BASE_URL_ENV, base_url);
        guard
    }

    fn try_overridden_to(base_url: &str) -> Option<Self> {
        // `EnvMutex::try_lock` already recovers poison, so contention is the
        // only reason this can fail — which is exactly what the caller asserts.
        let lock = ENV_MUTEX.try_lock()?;
        let previous = std::env::var_os(TEST_ALGOLIA_BASE_URL_ENV);
        std::env::remove_var(TEST_ALGOLIA_BASE_URL_ENV);
        std::env::set_var(TEST_ALGOLIA_BASE_URL_ENV, base_url);
        Some(Self {
            previous,
            _lock: lock,
        })
    }
}

impl Drop for AlgoliaBaseUrlEnvGuard {
    fn drop(&mut self) {
        // Restore while the mutex is still held. The mutex field is released
        // only after this Drop implementation returns.
        restore_env_var(TEST_ALGOLIA_BASE_URL_ENV, self.previous.take());
    }
}

pub(crate) struct TestStateBuilder<'tmp> {
    tmp: &'tmp TempDir,
    analytics_engine: Option<Arc<AnalyticsQueryEngine>>,
    experiment_store: Option<Arc<ExperimentStore>>,
    geoip_reader: Option<Arc<GeoIpReader>>,
    replication_manager: Option<Arc<ReplicationManager>>,
    migration_capacity: usize,
    bulk_replace_max_bytes: u64,
}

impl<'tmp> TestStateBuilder<'tmp> {
    pub(crate) fn new(tmp: &'tmp TempDir) -> Self {
        Self {
            tmp,
            analytics_engine: None,
            experiment_store: None,
            geoip_reader: None,
            replication_manager: None,
            migration_capacity: crate::handlers::migration::DEFAULT_ASYNC_MIGRATION_CAPACITY,
            bulk_replace_max_bytes: crate::handlers::migration::spool::SpoolLimits::default()
                .max_bytes_per_job,
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

    pub(crate) fn with_migration_capacity(mut self, capacity: usize) -> Self {
        self.migration_capacity = capacity;
        self
    }

    pub(crate) fn with_bulk_replace_max_bytes(mut self, max_bytes: u64) -> Self {
        self.bulk_replace_max_bytes = max_bytes;
        self
    }

    pub(crate) fn build(self) -> AppState {
        let manager = flapjack::IndexManager::new(self.tmp.path());
        let dictionary_manager = Arc::new(flapjack::dictionaries::manager::DictionaryManager::new(
            self.tmp.path(),
        ));
        manager.set_dictionary_manager(Arc::clone(&dictionary_manager));

        let replication_manager = self.replication_manager;
        let migration_runner = Arc::new(crate::handlers::migration::MigrationJobRunner::new(
            Arc::clone(&manager),
            replication_manager.clone(),
            self.migration_capacity,
        ));

        AppState {
            manager,
            key_store: None,
            replication_manager,
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
            migration_runner,
            bulk_replace_max_bytes: self.bulk_replace_max_bytes,
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

pub(crate) async fn quiesced_snapshot_bytes(
    manager: &flapjack::IndexManager,
    tenant_id: &str,
) -> Vec<u8> {
    let index_path = manager.base_path.join(tenant_id);
    let _quiesce = manager
        .quiesce_tenant(&tenant_id.to_string())
        .await
        .expect("snapshot fixture export must quiesce the tenant");
    let export_tenant_id = tenant_id.to_string();
    tokio::task::spawn_blocking(move || {
        crate::snapshot_byte_ops::export_snapshot_bytes(&index_path, &export_tenant_id)
    })
    .await
    .expect("snapshot fixture export task must not panic")
    .expect("snapshot fixture bytes must export from a quiesced tenant")
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
        data_dir,
        crate::router::RouterConfig {
            cors_mode: crate::startup::CorsMode::LoopbackOnly,
            disable_dashboard: false,
            replication_api_key: None,
        },
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

/// Send a request with explicit headers and an optional socket peer through a router.
///
/// Auth transport tests need both cookie headers and a real `ConnectInfo` trust
/// boundary. Keeping that construction here prevents each contract test from
/// assembling a subtly different synthetic request.
pub(crate) async fn send_request_with_headers(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: Option<serde_json::Value>,
    headers: &[(&str, &str)],
    peer_addr: Option<SocketAddr>,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }

    let mut request = builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap();
    if let Some(peer_addr) = peer_addr {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(peer_addr));
    }

    app.clone().oneshot(request).await.unwrap()
}

// ---------------------------------------------------------------------------
// Writer-quiesce assertions over retained writer-lifecycle events
// ---------------------------------------------------------------------------

/// Number of retained `channel_closed` / `merge_quiesced` writer-lifecycle events
/// for the tenant. Each one is a persistent writer that was closed only after its
/// merge threads finished.
pub(crate) fn retained_channel_closed_count(tenant_id: &str) -> usize {
    flapjack::index::write_queue::writer_lifecycle_test_events(tenant_id)
        .iter()
        .filter(|event| event.reason == "channel_closed" && event.phase == "merge_quiesced")
        .count()
}

/// Assert the operation under test drained exactly one persistent writer through
/// merge quiescence.
pub(crate) fn assert_retained_channel_closed_delta(tenant_id: &str, before: usize, message: &str) {
    let after = retained_channel_closed_count(tenant_id);
    assert_eq!(after, before + 1, "{message}");
}

/// Assert the tenant's merge-quiescent writer close was recorded *before* the named
/// publication checkpoint, i.e. that quiesce fences the publication rather than
/// trailing it.
pub(crate) fn assert_quiescence_before_publication(
    tenant_id: &str,
    publication_phase: &'static str,
) {
    let events = flapjack::index::write_queue::writer_lifecycle_test_events(tenant_id);
    let quiesced_sequence = events
        .iter()
        .find(|event| event.reason == "channel_closed" && event.phase == "merge_quiesced")
        .map(|event| event.sequence)
        .unwrap_or_else(|| {
            panic!(
                "tenant {tenant_id} must retain a channel_closed merge-quiesced event: {events:?}"
            )
        });
    let publication_sequence = events
        .iter()
        .find(|event| event.phase == publication_phase)
        .map(|event| event.sequence)
        .unwrap_or_else(|| {
            panic!(
                "tenant {tenant_id} must retain publication event {publication_phase}: {events:?}"
            )
        });
    assert!(
        quiesced_sequence < publication_sequence,
        "tenant {tenant_id} must merge-quiesce before {publication_phase}; events: {events:?}"
    );
}

/// Answer exactly one HTTP request with a `302` and return the request line.
///
/// Redirect-refusal tests need a real status line on the wire — a closed socket
/// only proves a transport failure — so this serves plain HTTP on a caller-owned
/// loopback listener and reports what the client actually asked for.
pub(crate) fn serve_single_redirect(
    listener: tokio::net::TcpListener,
    location: String,
) -> tokio::task::JoinHandle<String> {
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (stream, _) = listener.accept().await.expect("redirect listener accept");
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .await
            .expect("redirect listener must read the request line");
        let mut header = String::new();
        loop {
            header.clear();
            reader
                .read_line(&mut header)
                .await
                .expect("redirect listener must read request headers");
            if header == "\r\n" || header.is_empty() {
                break;
            }
        }
        reader
            .into_inner()
            .write_all(
                format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("redirect listener must write the response");
        request_line.trim_end().to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // -----------------------------------------------------------------------
    // Env-lock re-entrancy contract  (regression: ROADMAP row `TEST-HANG-1`)
    // -----------------------------------------------------------------------
    //
    // WHY THIS EXISTS. `ENV_MUTEX` is a plain, NON-REENTRANT `std::sync::Mutex`.
    // Production code reachable from a served request reads the Algolia
    // base-URL override, and that reader must not take `ENV_MUTEX` when the
    // calling thread already holds it — a `std::sync::Mutex` re-locked by its
    // own holder blocks forever, at 0% CPU, with no diagnostic.
    //
    // The reader already had a "does this thread hold it?" escape hatch, but
    // the flag it consulted was maintained by exactly ONE of the several
    // acquisition paths. Any test taking `ENV_MUTEX.lock()` directly — the
    // common case, ~60 call sites — left the flag false, so the reader
    // re-locked and self-deadlocked. Measured 2026-08-03: a single-threaded
    // run of `router_tests::algolia_list_indexes_compat_contract_is_preserved`
    // sat at 0% CPU indefinitely with `read_override -> __psynch_mutexwait` on
    // the stack. Single-threaded means no other thread could have held the
    // lock, which is what makes this a self-deadlock rather than contention.
    //
    // The contract these tests pin: the holder flag is owned by the mutex
    // itself, so EVERY acquisition maintains it, and re-entry fails loudly
    // instead of hanging. A hang is the worst available failure mode because
    // it is indistinguishable from "slow" and it wedges the whole test binary.

    #[test]
    fn env_lock_marks_and_clears_the_current_thread_as_holder() {
        assert!(
            !current_thread_holds_env_lock(),
            "a thread that has not acquired the env lock must not report as holder"
        );

        {
            let _lock = ENV_MUTEX.lock().expect("env mutex should lock");
            assert!(
                current_thread_holds_env_lock(),
                "acquiring ENV_MUTEX directly must mark this thread as the holder; \
                 if it does not, any reader consulting this flag will re-lock and deadlock"
            );
        }

        assert!(
            !current_thread_holds_env_lock(),
            "dropping the env lock must clear the holder flag"
        );
    }

    #[test]
    fn env_lock_holder_flag_is_per_thread_not_global() {
        // Guards against a "fix" that uses a process-global bool: a thread that
        // does NOT hold the lock must never be told it does, or it would skip
        // acquiring and mutate the environment unsynchronized.
        let (held_ready_tx, held_ready_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();

        let holder = std::thread::spawn(move || {
            let _lock = ENV_MUTEX.lock().expect("env mutex should lock");
            held_ready_tx
                .send(())
                .expect("holder must signal readiness");
            release_rx.recv().expect("holder must await release");
        });

        held_ready_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("holder thread must acquire the env lock");

        // This thread never acquired it, so it is not the holder even though
        // another thread is.
        assert!(
            !current_thread_holds_env_lock(),
            "the holder flag must be thread-local, not a process-global bool"
        );

        release_tx.send(()).expect("release signal must send");
        holder.join().expect("holder thread must finish");
    }

    #[test]
    #[should_panic(expected = "already holds the env lock")]
    fn env_lock_reentry_on_the_same_thread_panics_instead_of_deadlocking() {
        // The guard must be able to fail. Without this, re-entry is an
        // unbounded hang; with it, the offending call site is named in seconds.
        let _outer = ENV_MUTEX.lock().expect("env mutex should lock");
        let _inner = ENV_MUTEX.lock().expect("unreachable: re-entry must panic");
    }

    #[test]
    fn env_var_restore_guard_requires_env_mutex_and_restores_value() {
        const NAME: &str = "FLAPJACK_ENV_RESTORE_GUARD_CONTRACT";
        {
            let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
            assert!(
                current_thread_holds_env_lock(),
                "test setup must hold ENV_MUTEX before mutating process environment"
            );
            std::env::remove_var(NAME);
        }

        let unguarded = std::panic::catch_unwind(|| {
            let _guard = EnvVarRestoreGuard::set(NAME, "unguarded");
        });
        assert!(
            unguarded.is_err(),
            "EnvVarRestoreGuard::set must reject callers that do not hold ENV_MUTEX"
        );
        {
            let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
            assert_eq!(std::env::var_os(NAME), None);
        }

        {
            let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
            let _guard = EnvVarRestoreGuard::set(NAME, "guarded");
            assert_eq!(std::env::var(NAME).as_deref(), Ok("guarded"));
        }
        {
            let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
            assert_eq!(
                std::env::var_os(NAME),
                None,
                "dropping EnvVarRestoreGuard must restore the prior absence"
            );
        }

        {
            let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
            assert!(
                current_thread_holds_env_lock(),
                "test prior-value setup must hold ENV_MUTEX before mutating process environment"
            );
            std::env::set_var(NAME, "prior");
        }
        {
            let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
            let _guard = EnvVarRestoreGuard::remove(NAME);
            assert_eq!(std::env::var_os(NAME), None);
        }
        {
            let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
            assert_eq!(
                std::env::var(NAME).as_deref(),
                Ok("prior"),
                "dropping EnvVarRestoreGuard must restore the prior value"
            );
        }
        {
            let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
            assert!(
                current_thread_holds_env_lock(),
                "test cleanup must hold ENV_MUTEX before mutating process environment"
            );
            std::env::remove_var(NAME);
        }
    }

    #[test]
    fn algolia_base_url_guard_prevents_cross_suite_overlap_and_restores_value() {
        let first_url = "http://127.0.0.1:18181/";
        let second_url = "http://127.0.0.1:28282/";
        let first = AlgoliaBaseUrlEnvGuard::overridden_to(first_url);
        let original = first.previous.clone();

        for bypass in [
            std::panic::catch_unwind(|| {
                let _bypass = EnvVarRestoreGuard::set(TEST_ALGOLIA_BASE_URL_ENV, second_url);
            }),
            std::panic::catch_unwind(|| {
                let _bypass = EnvVarRestoreGuard::remove(TEST_ALGOLIA_BASE_URL_ENV);
            }),
            std::panic::catch_unwind(|| {
                let _bypass = with_env_var(TEST_ALGOLIA_BASE_URL_ENV, second_url);
            }),
        ] {
            let panic = bypass.expect_err(
                "generic environment helpers must reject the canonical Algolia override",
            );
            let message = panic
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .expect("Algolia guard rejection must explain the required helper");
            assert!(
                message.contains("AlgoliaBaseUrlEnvGuard"),
                "Algolia guard rejection must point callers at the canonical helper: {message}"
            );
        }

        assert_eq!(std::env::var(TEST_ALGOLIA_BASE_URL_ENV).unwrap(), first_url);
        let overlapped = std::thread::spawn(move || {
            AlgoliaBaseUrlEnvGuard::try_overridden_to(second_url).is_some()
        })
        .join()
        .expect("contending Algolia fixture thread must complete");
        assert!(
            !overlapped,
            "router and client-policy fixtures must not hold different Algolia overrides concurrently"
        );
        assert_eq!(std::env::var(TEST_ALGOLIA_BASE_URL_ENV).unwrap(), first_url);

        drop(first);
        let second = AlgoliaBaseUrlEnvGuard::overridden_to(second_url);
        assert_eq!(
            second.previous, original,
            "the first guard must restore the value observed by its successor"
        );
        assert_eq!(
            std::env::var(TEST_ALGOLIA_BASE_URL_ENV).unwrap(),
            second_url
        );
        drop(second);

        let restored = AlgoliaBaseUrlEnvGuard::vendor_hosts();
        assert_eq!(
            restored.previous, original,
            "the second guard must restore the original value before its successor acquires the mutex"
        );
    }

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
