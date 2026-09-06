//! Stub summary for engine/flapjack-http/src/server.rs.
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::background_tasks::spawn_background_tasks;
use crate::router::{build_router, RouterConfig};
use crate::server_init::{
    initialize_infrastructure, initialize_state, log_startup_summary, StartupSummary,
};
use crate::startup::{
    cors_origins_from_env, exit_for_startup_auth_validation_error, init_tracing,
    initialize_key_store_for_mode, load_server_config, log_memory_configuration,
    print_startup_banner, shutdown_signal, shutdown_timeout_secs_from_env,
    validate_startup_auth_policy, AuthStatus, CorsMode, StartupAuthValidationOutcome,
    StartupPersistenceMode, NO_AUTH_PUBLIC_BIND_WARNING,
};
use crate::tls_serve;
use flapjack_replication::config::NodeConfig;
use serde::{Deserialize, Serialize};

const SHUTDOWN_INVENTORY_FILE: &str = "shutdown-inventory.json";

#[cfg(test)]
#[path = "server_startup_tests.rs"]
mod startup_repair_tests;

/// Main server entry point: loads config, initializes infrastructure (key store, S3,
/// analytics, replication), builds the router, binds the listener, and runs the
/// HTTP server with graceful shutdown handling.
pub async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let startup_start = std::time::Instant::now();

    // Tracing is installed before config load so startup validation warnings
    // (unauthenticated replication peers, cleartext peers) reach the log.
    #[cfg(feature = "otel")]
    let otel_guard = init_tracing();
    #[cfg(not(feature = "otel"))]
    init_tracing();

    let server_config = load_server_config().map_err(std::io::Error::other)?;
    let loaded_tls = server_config
        .tls_paths
        .as_ref()
        .map(tls_serve::load_tls_config)
        .transpose()
        .map_err(std::io::Error::other)?;

    log_memory_configuration();

    let cors_mode = cors_origins_from_env();
    let shutdown_timeout_secs = shutdown_timeout_secs_from_env();
    let data_dir = Path::new(&server_config.data_dir);
    let global_mutation_fence = crate::pause_registry::GlobalMutationFence::open(data_dir)?;
    let persistence_mode = startup_persistence_mode(&global_mutation_fence).await;
    // Topology was already parsed and validated by `load_server_config`.
    let node_config = server_config.node_config.clone();
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
    let initialized_keys =
        initialize_key_store_for_mode(&server_config, data_dir, persistence_mode)
            .map_err(std::io::Error::other)?;
    let key_store = initialized_keys.key_store;
    let admin_key = initialized_keys.admin_key;
    let key_is_new = initialized_keys.key_is_new;
    let mut infrastructure = initialize_server_infrastructure_with_fence(
        &server_config,
        data_dir,
        admin_key.clone(),
        node_config,
        global_mutation_fence,
        persistence_mode,
    )
    .await?;
    if let Some(loaded_tls) = loaded_tls.as_ref() {
        infrastructure.tls_resolver = Some(Arc::clone(&loaded_tls.resolver));
    }

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

    let app = build_router(
        Arc::clone(&state),
        key_store,
        Arc::clone(&infrastructure.analytics_collector),
        Arc::clone(&infrastructure.trusted_proxy_matcher),
        data_dir,
        RouterConfig {
            cors_mode,
            disable_dashboard: server_config.disable_dashboard,
            replication_api_key: server_config.replication_api_key_env.clone(),
            api_profile: server_config.api_profile,
        },
    );

    let listener = tokio::net::TcpListener::bind(&infrastructure.bind_addr).await?;
    // SSL renewal may immediately request a certificate. Bind first so Pebble or a
    // production CA can queue its HTTP-01 request until Axum begins accepting it.
    spawn_background_tasks(&state, &infrastructure).map_err(std::io::Error::other)?;
    let auth_status = resolve_auth_status(&server_config, key_is_new, admin_key);
    let use_tls = loaded_tls.is_some();
    print_startup_banner(
        &listener.local_addr()?.to_string(),
        if use_tls { "https" } else { "http" },
        auth_status,
        startup_start.elapsed().as_millis(),
        &server_config.data_dir,
    );

    let make_service = app.into_make_service_with_connect_info::<SocketAddr>();
    match loaded_tls {
        Some(loaded_tls) => {
            tls_serve::serve_tls(listener, make_service, loaded_tls.config, shutdown_signal())
                .await
                .map_err(std::io::Error::other)?;
        }
        None => {
            let (shutdown_started_tx, shutdown_started_rx) = tokio::sync::oneshot::channel();
            let shutdown = async move {
                shutdown_signal().await;
                let _ = shutdown_started_tx.send(());
            };
            let serve = axum::serve(listener, make_service).with_graceful_shutdown(shutdown);
            await_plaintext_connection_drain(
                async move { serve.await },
                async move {
                    let _ = shutdown_started_rx.await;
                },
                tokio::time::Duration::from_secs(shutdown_timeout_secs),
            )
            .await?;
        }
    }

    let receipt_path = shutdown_inventory_receipt_path(&state.manager.base_path)?;
    if receipt_path.exists() {
        std::fs::remove_file(&receipt_path)?;
    }
    let shutdown_outcome =
        run_graceful_shutdown(&mut infrastructure, &state, shutdown_timeout_secs).await;
    require_drained_shutdown(shutdown_outcome)?;
    if let Some(fence) = state.global_mutation_fence.status().await {
        persist_shutdown_inventory_receipt(&state.manager, &receipt_path, &fence.transaction_id)?;
    }
    Ok(())
}

async fn await_plaintext_connection_drain<Serve, ShutdownStarted>(
    serve: Serve,
    shutdown_started: ShutdownStarted,
    timeout: tokio::time::Duration,
) -> std::io::Result<()>
where
    Serve: std::future::Future<Output = std::io::Result<()>>,
    ShutdownStarted: std::future::Future<Output = ()>,
{
    tokio::pin!(serve);
    tokio::select! {
        result = &mut serve => return result,
        () = shutdown_started => {}
    }

    tokio::time::timeout(timeout, serve).await.map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "plaintext graceful connection drain timed out after {}ms",
                timeout.as_millis()
            ),
        )
    })?
}

#[cfg(test)]
pub(crate) async fn initialize_server_infrastructure(
    server_config: &crate::startup::ServerConfig,
    data_dir: &Path,
    admin_key: Option<String>,
    node_config: NodeConfig,
) -> Result<crate::server_init::InfrastructureState, Box<dyn std::error::Error>> {
    let global_mutation_fence = crate::pause_registry::GlobalMutationFence::open(data_dir)?;
    let persistence_mode = startup_persistence_mode(&global_mutation_fence).await;
    initialize_server_infrastructure_with_fence(
        server_config,
        data_dir,
        admin_key,
        node_config,
        global_mutation_fence,
        persistence_mode,
    )
    .await
}

async fn initialize_server_infrastructure_with_fence(
    server_config: &crate::startup::ServerConfig,
    data_dir: &Path,
    admin_key: Option<String>,
    node_config: NodeConfig,
    global_mutation_fence: crate::pause_registry::GlobalMutationFence,
    persistence_mode: StartupPersistenceMode,
) -> Result<crate::server_init::InfrastructureState, Box<dyn std::error::Error>> {
    initialize_infrastructure(
        server_config,
        data_dir,
        admin_key,
        node_config,
        global_mutation_fence,
        persistence_mode,
    )
    .await
}

async fn startup_persistence_mode(
    global_mutation_fence: &crate::pause_registry::GlobalMutationFence,
) -> StartupPersistenceMode {
    if global_mutation_fence.status().await.is_some() {
        StartupPersistenceMode::FenceActive
    } else {
        StartupPersistenceMode::Ordinary
    }
}

pub(crate) async fn run_pre_serve_barrier(
    state: &crate::handlers::AppState,
) -> Result<Vec<flapjack::index::manager::publication::PublicationRepairReport>, String> {
    run_pre_serve_barrier_with_catchup(state, crate::startup_catchup::run_pre_serve_catchup(state))
        .await
}

/// TODO: Document run_pre_serve_barrier_with_catchup.
async fn run_pre_serve_barrier_with_catchup<Catchup>(
    state: &crate::handlers::AppState,
    catchup: Catchup,
) -> Result<Vec<flapjack::index::manager::publication::PublicationRepairReport>, String>
where
    Catchup: std::future::Future<Output = Result<(), String>>,
{
    let _mutation_permit = match state.global_mutation_fence.admit_mutation().await {
        Ok(permit) => permit,
        Err(_) => {
            tracing::info!(
                "release mutation fence is active; skipping pre-serve repair, migration recovery, and catch-up"
            );
            return Ok(Vec::new());
        }
    };
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShutdownWaitOutcome {
    Drained,
    TimedOut,
    Failed(String),
}

/// Convert the internal drain outcome into the process-level service result.
/// A timeout means migrations or queued writes may not have reached their
/// durable boundary, so returning success would let an updater activate new
/// bytes under a false clean-stop claim.
fn require_drained_shutdown(outcome: ShutdownWaitOutcome) -> std::io::Result<()> {
    match outcome {
        ShutdownWaitOutcome::Drained => Ok(()),
        ShutdownWaitOutcome::TimedOut => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Flapjack shutdown timed out before migrations and write queues drained",
        )),
        ShutdownWaitOutcome::Failed(error) => Err(std::io::Error::other(format!(
            "Flapjack shutdown failed before all write queues drained: {error}"
        ))),
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DataRootIdentity {
    path: String,
    device: u64,
    inode: u64,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShutdownInventoryReceipt {
    schema_version: u8,
    kind: &'static str,
    runtime: flapjack::BuildInfo,
    transaction_id: String,
    data_root: DataRootIdentity,
    inventory: Vec<crate::handlers::internal::ReleaseInventoryEntry>,
}

fn shutdown_inventory_receipt_path(data_root: &Path) -> std::io::Result<PathBuf> {
    let parent = data_root.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Flapjack data root must have a parent for the shutdown inventory receipt",
        )
    })?;
    Ok(parent.join(SHUTDOWN_INVENTORY_FILE))
}

fn persist_shutdown_inventory_receipt(
    manager: &flapjack::IndexManager,
    receipt_path: &Path,
    transaction_id: &str,
) -> std::io::Result<()> {
    let canonical_data_root = manager.base_path.canonicalize()?;
    let metadata = canonical_data_root.metadata()?;
    #[cfg(unix)]
    let (device, inode) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Flapjack release inventory requires Unix data-root identity",
    ));

    let inventory = crate::handlers::internal::canonical_release_inventory(manager)
        .map_err(std::io::Error::other)?;
    let receipt = ShutdownInventoryReceipt {
        schema_version: 1,
        kind: "flapjack_shutdown_inventory",
        runtime: flapjack::build_info().clone(),
        transaction_id: transaction_id.to_string(),
        data_root: DataRootIdentity {
            path: canonical_data_root.display().to_string(),
            device,
            inode,
        },
        inventory,
    };
    let mut payload = serde_json::to_vec(&receipt).map_err(std::io::Error::other)?;
    payload.push(b'\n');
    flapjack::index::atomic_write_private_file(receipt_path, &payload)
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
    ShutdownFuture: std::future::Future<Output = Result<(), String>>,
{
    flush_then_wait_for_migration_and_manager_shutdown(
        shutdown_timeout_secs,
        flush_analytics,
        std::future::ready(()),
        manager_shutdown,
    )
    .await
}

/// TODO: Document flush_then_wait_for_migration_and_manager_shutdown.
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
    ShutdownFuture: std::future::Future<Output = Result<(), String>>,
{
    flush_analytics();
    match tokio::time::timeout(
        tokio::time::Duration::from_secs(shutdown_timeout_secs),
        async move {
            let ((), manager_result) = tokio::join!(migration_shutdown, manager_shutdown);
            manager_result
        },
    )
    .await
    {
        Ok(Ok(())) => ShutdownWaitOutcome::Drained,
        Ok(Err(error)) => ShutdownWaitOutcome::Failed(error),
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
    ShutdownFuture: std::future::Future<Output = Result<(), String>>,
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

/// TODO: Document full_graceful_shutdown_with_migrations.
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
    ShutdownFuture: std::future::Future<Output = Result<(), String>>,
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

fn flush_analytics_before_shutdown(
    analytics_enabled: bool,
    collector: &flapjack::analytics::AnalyticsCollector,
) {
    if analytics_enabled {
        collector.flush_all();
        collector.shutdown();
    }
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
            flush_analytics_before_shutdown(analytics_enabled, &analytics_collector);
            if analytics_enabled {
                tracing::info!("[shutdown] Analytics buffers flushed");
            }
        },
        state.migration_runner.drain_active_imports(),
        async {
            crate::handlers::internal::prepare_release_inventory(&state.manager)?;
            state
                .manager
                .drain_all_write_queues()
                .await
                .map_err(|error| error.to_string())
        },
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
        ShutdownWaitOutcome::Failed(ref error) => {
            tracing::error!(
                error,
                "[shutdown] Write queue drain failed; refusing clean service stop"
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
