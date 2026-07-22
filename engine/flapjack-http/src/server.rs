use std::path::Path;
use std::sync::Arc;

use crate::background_tasks::spawn_background_tasks;
use crate::router::{build_router, RouterConfig};
use crate::server_init::{
    initialize_infrastructure, initialize_state, log_startup_summary, StartupSummary,
};
use crate::startup::{
    cors_origins_from_env, exit_for_startup_auth_validation_error, init_tracing,
    initialize_key_store, load_server_config, log_memory_configuration, print_startup_banner,
    shutdown_signal, shutdown_timeout_secs_from_env, validate_startup_auth_policy, AuthStatus,
    CorsMode, StartupAuthValidationOutcome, NO_AUTH_PUBLIC_BIND_WARNING,
};
use flapjack_replication::config::NodeConfig;

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
    let node_config = NodeConfig::load_or_default(data_dir);
    match validate_startup_auth_policy(
        &server_config.env_mode,
        server_config.no_auth,
        server_config.admin_key_env.as_deref(),
        &node_config.bind_addr,
        server_config.allow_no_auth_public_bind,
    ) {
        Ok(StartupAuthValidationOutcome::Accepted) => {}
        Ok(StartupAuthValidationOutcome::ExplicitlyAllowedPublicNoAuthBind) => {
            tracing::warn!("{}", NO_AUTH_PUBLIC_BIND_WARNING);
        }
        Err(error) => exit_for_startup_auth_validation_error(error),
    }
    let (key_store, admin_key, key_is_new) = initialize_key_store(&server_config, data_dir);

    let mut infrastructure =
        initialize_infrastructure(&server_config, data_dir, admin_key.clone(), node_config).await?;

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
        data_dir,
        RouterConfig {
            cors_mode,
            disable_dashboard: server_config.disable_dashboard,
        },
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
    state
        .migration_runner
        .recover_async_jobs_before_serve(&reports)
        .await
        .map_err(|error| format!("pre-serve async migration recovery failed: {error}"))?;
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
#[cfg(test)]
async fn flush_then_wait_for_manager_shutdown<FlushFn, ShutdownFuture>(
    shutdown_timeout_secs: u64,
    flush_analytics: FlushFn,
    manager_shutdown: ShutdownFuture,
) -> ShutdownWaitOutcome
where
    FlushFn: FnOnce(),
    ShutdownFuture: std::future::Future<Output = ()>,
{
    flush_then_wait_for_migration_and_manager_shutdown(
        shutdown_timeout_secs,
        flush_analytics,
        std::future::ready(()),
        manager_shutdown,
    )
    .await
}

async fn flush_then_wait_for_migration_and_manager_shutdown<
    FlushFn,
    MigrationFuture,
    ShutdownFuture,
>(
    shutdown_timeout_secs: u64,
    flush_analytics: FlushFn,
    migration_shutdown: MigrationFuture,
    manager_shutdown: ShutdownFuture,
) -> ShutdownWaitOutcome
where
    FlushFn: FnOnce(),
    MigrationFuture: std::future::Future<Output = ()>,
    ShutdownFuture: std::future::Future<Output = ()>,
{
    flush_analytics();
    match tokio::time::timeout(
        tokio::time::Duration::from_secs(shutdown_timeout_secs),
        async move {
            let ((), ()) = tokio::join!(migration_shutdown, manager_shutdown);
        },
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
#[cfg(test)]
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

async fn full_graceful_shutdown_with_migrations<FlushFn, MigrationFuture, ShutdownFuture, OtelFn>(
    shutdown_timeout_secs: u64,
    flush_analytics: FlushFn,
    migration_shutdown: MigrationFuture,
    manager_shutdown: ShutdownFuture,
    otel_shutdown: OtelFn,
) -> ShutdownWaitOutcome
where
    FlushFn: FnOnce(),
    MigrationFuture: std::future::Future<Output = ()>,
    ShutdownFuture: std::future::Future<Output = ()>,
    OtelFn: FnOnce(),
{
    let outcome = flush_then_wait_for_migration_and_manager_shutdown(
        shutdown_timeout_secs,
        flush_analytics,
        migration_shutdown,
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

    let outcome = full_graceful_shutdown_with_migrations(
        shutdown_timeout_secs,
        move || {
            if analytics_enabled {
                analytics_collector.shutdown();
                tracing::info!("[shutdown] Analytics buffers flushed");
            }
        },
        state.migration_runner.drain_active_imports(),
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
#[path = "server_shutdown_tests.rs"]
mod tests;
