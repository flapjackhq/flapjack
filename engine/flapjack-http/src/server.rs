use std::path::Path;
use std::sync::Arc;

use crate::background_tasks::spawn_background_tasks;
use crate::router::build_router;
use crate::server_init::{
    initialize_infrastructure, initialize_state, log_startup_summary, StartupSummary,
};
use crate::startup::{
    cors_origins_from_env, init_tracing, initialize_key_store, load_server_config,
    log_memory_configuration, print_startup_banner, shutdown_signal,
    shutdown_timeout_secs_from_env, AuthStatus, CorsMode,
};

#[cfg(test)]
#[path = "server_startup_tests.rs"]
mod startup_repair_tests;

/// Main server entry point: loads config, initializes infrastructure (key store, S3,
/// analytics, replication), builds the router, binds the listener, and runs the
/// HTTP server with graceful shutdown handling.
pub async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let startup_start = std::time::Instant::now();

    let server_config = load_server_config();

    #[cfg(feature = "otel")]
    let otel_guard = init_tracing();
    #[cfg(not(feature = "otel"))]
    init_tracing();

    log_memory_configuration();

    let cors_mode = cors_origins_from_env();
    let shutdown_timeout_secs = shutdown_timeout_secs_from_env();
    let data_dir = Path::new(&server_config.data_dir);
    let (key_store, admin_key, key_is_new) = initialize_key_store(&server_config, data_dir);

    let mut infrastructure =
        initialize_infrastructure(&server_config, data_dir, admin_key.clone()).await?;

    #[cfg(feature = "otel")]
    {
        infrastructure.otel_guard = otel_guard;
    }
    tracing::info!(
        env_mode = %server_config.env_mode,
        replication_peers = infrastructure.node_config.peers.len(),
        trusted_proxy_ranges = infrastructure.trusted_proxy_matcher.len(),
        "Server infrastructure initialized"
    );
    log_cors_mode(&cors_mode);
    log_startup_summary(&StartupSummary::from_infrastructure(
        &infrastructure,
        !server_config.no_auth,
    ));

    let state = initialize_state(
        &infrastructure,
        key_store.clone(),
        &server_config.data_dir,
        startup_start,
    )?;

    // Pre-serve barrier: repair node-local publication state, then catch up
    // from peers before accepting traffic.
    run_pre_serve_barrier(&state)
        .await
        .map_err(std::io::Error::other)?;

    spawn_background_tasks(&state, &infrastructure);

    let app = build_router(
        Arc::clone(&state),
        key_store,
        Arc::clone(&infrastructure.analytics_collector),
        Arc::clone(&infrastructure.trusted_proxy_matcher),
        cors_mode,
        data_dir,
    );

    let listener = tokio::net::TcpListener::bind(&infrastructure.bind_addr).await?;
    let auth_status = resolve_auth_status(&server_config, key_is_new, admin_key);
    print_startup_banner(
        &listener.local_addr()?.to_string(),
        auth_status,
        startup_start.elapsed().as_millis(),
        &server_config.data_dir,
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    let _ = run_graceful_shutdown(&mut infrastructure, &state, shutdown_timeout_secs).await;
    Ok(())
}

pub(crate) async fn run_pre_serve_barrier(
    state: &crate::handlers::AppState,
) -> Result<Vec<flapjack::index::manager::publication::PublicationRepairReport>, String> {
    run_pre_serve_barrier_with_catchup(state, crate::startup_catchup::run_pre_serve_catchup(state))
        .await
}

async fn run_pre_serve_barrier_with_catchup<Catchup>(
    state: &crate::handlers::AppState,
    catchup: Catchup,
) -> Result<Vec<flapjack::index::manager::publication::PublicationRepairReport>, String>
where
    Catchup: std::future::Future<Output = Result<(), String>>,
{
    let reports = state
        .manager
        .repair_publications_before_serve()
        .map_err(|error| format!("pre-serve publication repair failed: {error}"))?;
    catchup.await?;
    Ok(reports)
}

fn log_cors_mode(cors_mode: &CorsMode) {
    match cors_mode {
        CorsMode::LoopbackOnly => tracing::info!(
            "CORS: default loopback-only mode (non-loopback origins require FLAPJACK_ALLOWED_ORIGINS)"
        ),
        CorsMode::Restricted(origins) => {
            let configured_origins = origins
                .iter()
                .filter_map(|origin| origin.to_str().ok())
                .collect::<Vec<_>>()
                .join(", ");
            tracing::info!("CORS: restricted to [{}]", configured_origins);
        }
    }
}

fn resolve_auth_status(
    config: &crate::startup::ServerConfig,
    key_is_new: bool,
    admin_key: Option<String>,
) -> AuthStatus {
    if config.no_auth {
        AuthStatus::Disabled
    } else if key_is_new {
        AuthStatus::NewKey(admin_key.unwrap_or_default())
    } else {
        AuthStatus::KeyInFile
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownWaitOutcome {
    Drained,
    TimedOut,
}

/// Flushes analytics data then waits for the index manager to complete its graceful
/// shutdown (flushing write queues), with a configurable timeout.
async fn flush_then_wait_for_manager_shutdown<FlushFn, ShutdownFuture>(
    shutdown_timeout_secs: u64,
    flush_analytics: FlushFn,
    manager_shutdown: ShutdownFuture,
) -> ShutdownWaitOutcome
where
    FlushFn: FnOnce(),
    ShutdownFuture: std::future::Future<Output = ()>,
{
    flush_analytics();

    match tokio::time::timeout(
        tokio::time::Duration::from_secs(shutdown_timeout_secs),
        manager_shutdown,
    )
    .await
    {
        Ok(()) => ShutdownWaitOutcome::Drained,
        Err(_) => ShutdownWaitOutcome::TimedOut,
    }
}

/// Run the full shutdown sequence: analytics flush, manager drain (with timeout),
/// then OTEL provider shutdown. The `otel_shutdown` closure runs unconditionally
/// after the manager drain path, even when the drain times out.
async fn full_graceful_shutdown<FlushFn, ShutdownFuture, OtelFn>(
    shutdown_timeout_secs: u64,
    flush_analytics: FlushFn,
    manager_shutdown: ShutdownFuture,
    otel_shutdown: OtelFn,
) -> ShutdownWaitOutcome
where
    FlushFn: FnOnce(),
    ShutdownFuture: std::future::Future<Output = ()>,
    OtelFn: FnOnce(),
{
    let outcome = flush_then_wait_for_manager_shutdown(
        shutdown_timeout_secs,
        flush_analytics,
        manager_shutdown,
    )
    .await;

    otel_shutdown();

    outcome
}

/// Orchestrates graceful shutdown by flushing analytics, waiting for
/// index manager shutdown, and then shutting down OTEL tracing. Logs
/// whether draining completed in time.
async fn run_graceful_shutdown(
    infrastructure: &mut crate::server_init::InfrastructureState,
    state: &Arc<crate::handlers::AppState>,
    shutdown_timeout_secs: u64,
) -> ShutdownWaitOutcome {
    tracing::info!(
        timeout_env_var = "FLAPJACK_SHUTDOWN_TIMEOUT_SECS",
        timeout_secs = shutdown_timeout_secs,
        "[shutdown] Server stopped accepting connections, cleaning up..."
    );

    // Clone analytics refs upfront so the flush closure doesn't borrow
    // infrastructure, leaving it free for the mutable OTEL shutdown closure.
    let analytics_enabled = infrastructure.analytics_config.enabled;
    let analytics_collector = Arc::clone(&infrastructure.analytics_collector);

    let outcome = full_graceful_shutdown(
        shutdown_timeout_secs,
        move || {
            if analytics_enabled {
                analytics_collector.shutdown();
                tracing::info!("[shutdown] Analytics buffers flushed");
            }
        },
        state.manager.graceful_shutdown(),
        || shutdown_otel_provider(infrastructure),
    )
    .await;

    match outcome {
        ShutdownWaitOutcome::Drained => {
            tracing::info!(
                timeout_env_var = "FLAPJACK_SHUTDOWN_TIMEOUT_SECS",
                timeout_secs = shutdown_timeout_secs,
                "[shutdown] All write queues drained before deadline"
            );
        }
        ShutdownWaitOutcome::TimedOut => {
            tracing::warn!(
                timeout_env_var = "FLAPJACK_SHUTDOWN_TIMEOUT_SECS",
                timeout_secs = shutdown_timeout_secs,
                "[shutdown] Write queue drain timed out; forcing exit may drop queued writes (data-loss risk)"
            );
        }
    }

    outcome
}

/// Shut down the OTEL trace provider if one was initialized, flushing
/// any pending spans. No-op when the `otel` feature is disabled or when
/// no endpoint was configured at startup.
fn shutdown_otel_provider(infrastructure: &mut crate::server_init::InfrastructureState) {
    #[cfg(feature = "otel")]
    if let Some(guard) = infrastructure.otel_guard.take() {
        match guard.shutdown() {
            Ok(()) => tracing::info!("[shutdown] OTEL trace provider shut down"),
            Err(e) => tracing::warn!("[shutdown] OTEL trace provider shutdown error: {e}"),
        }
    }

    #[cfg(not(feature = "otel"))]
    let _ = infrastructure;
}

#[cfg(test)]
mod tests {
    use super::{
        flush_then_wait_for_manager_shutdown, full_graceful_shutdown, ShutdownWaitOutcome,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    /// Ensures the shutdown helper reports success when the manager drain
    /// completes before the configured timeout.
    #[tokio::test]
    async fn shutdown_wait_helper_returns_drained_when_manager_completes_before_deadline() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let flush_events = Arc::clone(&events);
        let manager_events = Arc::clone(&events);

        let outcome = flush_then_wait_for_manager_shutdown(
            1,
            move || flush_events.lock().unwrap().push("analytics-flushed"),
            async move {
                manager_events.lock().unwrap().push("manager-wait-begins");
                tokio::time::sleep(Duration::from_millis(10)).await;
            },
        )
        .await;

        assert_eq!(outcome, ShutdownWaitOutcome::Drained);
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["analytics-flushed", "manager-wait-begins"]
        );
    }
    /// Ensures the shutdown helper reports a timeout when the manager drain
    /// exceeds the configured deadline.
    #[tokio::test]
    async fn shutdown_wait_helper_returns_timed_out_when_manager_exceeds_deadline() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let flush_events = Arc::clone(&events);
        let manager_events = Arc::clone(&events);

        let outcome = flush_then_wait_for_manager_shutdown(
            1,
            move || flush_events.lock().unwrap().push("analytics-flushed"),
            async move {
                manager_events.lock().unwrap().push("manager-wait-begins");
                tokio::time::sleep(Duration::from_secs(5)).await;
            },
        )
        .await;

        assert_eq!(outcome, ShutdownWaitOutcome::TimedOut);
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["analytics-flushed", "manager-wait-begins"]
        );
    }

    /// Ensures graceful shutdown flushes analytics, waits for the manager, and
    /// then shuts OTEL down in that order on the success path.
    #[tokio::test]
    async fn full_graceful_shutdown_calls_otel_after_manager_drain() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let flush_events = Arc::clone(&events);
        let manager_events = Arc::clone(&events);
        let otel_events = Arc::clone(&events);

        let outcome = full_graceful_shutdown(
            5,
            move || flush_events.lock().unwrap().push("analytics-flushed"),
            async move {
                manager_events.lock().unwrap().push("manager-drained");
                tokio::time::sleep(Duration::from_millis(10)).await;
            },
            move || otel_events.lock().unwrap().push("otel-shutdown"),
        )
        .await;

        assert_eq!(outcome, ShutdownWaitOutcome::Drained);
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["analytics-flushed", "manager-drained", "otel-shutdown"],
            "shutdown order must be: analytics flush → manager drain → otel shutdown"
        );
    }

    /// Ensures OTEL shutdown still runs when the manager drain times out so
    /// tracing flush semantics stay deterministic.
    #[tokio::test]
    async fn full_graceful_shutdown_calls_otel_even_after_timeout() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let flush_events = Arc::clone(&events);
        let manager_events = Arc::clone(&events);
        let otel_events = Arc::clone(&events);

        let outcome = full_graceful_shutdown(
            1,
            move || flush_events.lock().unwrap().push("analytics-flushed"),
            async move {
                manager_events.lock().unwrap().push("manager-wait-begins");
                tokio::time::sleep(Duration::from_secs(5)).await;
            },
            move || otel_events.lock().unwrap().push("otel-shutdown"),
        )
        .await;

        assert_eq!(outcome, ShutdownWaitOutcome::TimedOut);
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["analytics-flushed", "manager-wait-begins", "otel-shutdown"],
            "OTEL shutdown must run even when manager drain times out"
        );
    }
}
