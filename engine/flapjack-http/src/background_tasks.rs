//! Stub summary for engine/flapjack-http/src/background_tasks.rs.
use crate::handlers::migration::spool::{SpoolError, SpoolLimits, SpoolStore};
use crate::handlers::AppState;
use crate::server_init::InfrastructureState;
use crate::ssl_background::{ssl_background_plan, ConfiguredSsl};
use crate::tenant_dirs::{has_visible_tenant_dirs, visible_tenant_dir_names};
use flapjack::index::manager::publication::CrawlerRunStore;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
const HOUR_MS: i64 = 3_600_000;
const MIGRATION_SPOOL_GC_INTERVAL_ENV: &str = "FLAPJACK_MIGRATION_SPOOL_GC_INTERVAL_SECS";
const DEFAULT_MIGRATION_SPOOL_GC_INTERVAL_SECS: u64 = 300;
const AUTOHEAL_ENABLED_ENV: &str = "FLAPJACK_AUTOHEAL_ENABLED";
const ROLLUP_INTERVAL_ENV: &str = "FLAPJACK_ROLLUP_INTERVAL_SECS";
const SYNC_INTERVAL_ENV: &str = "FLAPJACK_SYNC_INTERVAL_SECS";

/// Server-wide liveness state for loops whose unexpected exit makes the node
/// operationally incomplete even though request handling can continue.
#[derive(Default)]
pub struct BackgroundTaskHealth {
    tasks: Mutex<BTreeMap<&'static str, bool>>,
}

impl BackgroundTaskHealth {
    fn record_running(&self, name: &'static str) {
        self.tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name, true);
    }

    fn record_exit(&self, name: &'static str) {
        self.tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name, false);
    }

    pub(crate) fn failed_tasks(&self) -> Vec<String> {
        self.tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|(_, running)| !*running)
            .map(|(name, _)| (*name).to_string())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn record_exit_for_test(&self, name: &'static str) {
        self.record_exit(name);
    }

    #[cfg(test)]
    pub(crate) fn is_running_for_test(&self, name: &'static str) -> bool {
        self.tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
            .copied()
            .unwrap_or(false)
    }
}

fn spawn_supervised<F>(health: Arc<BackgroundTaskHealth>, name: &'static str, future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    supervise_handle(health, name, tokio::spawn(future));
}

fn supervise_handle(
    health: Arc<BackgroundTaskHealth>,
    name: &'static str,
    worker: tokio::task::JoinHandle<()>,
) {
    health.record_running(name);
    // The monitor owns the worker handle, so completion and panic cannot be
    // silently discarded. Runtime shutdown drops both and does not create a
    // false unhealthy transition while the process is terminating.
    tokio::spawn(async move {
        let result = worker.await;
        health.record_exit(name);
        match result {
            Ok(()) => tracing::error!(task = name, "critical background task exited"),
            Err(error) => {
                tracing::error!(task = name, error = %error, "critical background task failed")
            }
        }
    });
}

fn positive_interval_secs(
    name: &str,
    configured: Option<&str>,
    default: u64,
) -> Result<u64, String> {
    let Some(raw) = configured else {
        return Ok(default);
    };
    raw.trim()
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| format!("{name} must be a positive integer, got {raw:?}"))
}

#[derive(Clone, Copy, Debug)]
struct BackgroundTaskIntervals {
    rollup_secs: u64,
    sync_secs: u64,
}

impl BackgroundTaskIntervals {
    fn from_env() -> Result<Self, String> {
        let rollup = std::env::var(ROLLUP_INTERVAL_ENV).ok();
        let sync = std::env::var(SYNC_INTERVAL_ENV).ok();
        Ok(Self {
            rollup_secs: positive_interval_secs(ROLLUP_INTERVAL_ENV, rollup.as_deref(), 300)?,
            sync_secs: positive_interval_secs(SYNC_INTERVAL_ENV, sync.as_deref(), 60)?,
        })
    }
}

/// Restore all tenant indexes from S3 snapshots when the data directory is empty.
///
/// Skip restoration if any tenant subdirectory already exists. Otherwise, list
/// all `snapshots/<tenant>/` prefixes in the configured S3 bucket, download the
/// latest snapshot for each tenant, and import it into the local data directory.
/// Failures for individual tenants are logged but do not abort the remaining
/// restores.
pub(crate) async fn auto_restore_from_s3(
    data_dir: &str,
    s3_config: &flapjack::index::s3::S3Config,
    manager: &std::sync::Arc<flapjack::IndexManager>,
) {
    let data_path = std::path::Path::new(data_dir);
    let has_tenants = has_visible_tenant_dirs(data_path).unwrap_or(false);
    if has_tenants {
        tracing::info!("Data dir has existing tenants, skipping S3 auto-restore");
        return;
    }

    tracing::info!("Empty data dir detected, attempting S3 auto-restore...");
    let tenant_ids = match list_s3_tenant_snapshots(s3_config).await {
        Some(ids) => ids,
        None => return,
    };

    for tid in &tenant_ids {
        restore_tenant_from_s3(manager, s3_config, tid).await;
    }
}

async fn list_s3_tenant_snapshots(
    s3_config: &flapjack::index::s3::S3Config,
) -> Option<Vec<String>> {
    let tenant_ids = fetch_s3_tenant_prefixes(s3_config).await?;
    if tenant_ids.is_empty() {
        tracing::info!("S3 auto-restore: no snapshots found");
        return None;
    }
    tracing::info!(
        "S3 auto-restore: found {} tenants: {:?}",
        tenant_ids.len(),
        tenant_ids
    );
    Some(tenant_ids)
}

/// Fetches tenant prefixes from S3 for discovery of remotely-backed tenants.
async fn fetch_s3_tenant_prefixes(
    s3_config: &flapjack::index::s3::S3Config,
) -> Option<Vec<String>> {
    let bucket = s3_config
        .clone()
        .bucket_internal()
        .map_err(|e| {
            tracing::warn!("S3 auto-restore: couldn't create bucket client: {}", e);
        })
        .ok()?;
    let results = bucket
        .list("snapshots/".to_string(), Some("/".to_string()))
        .await
        .map_err(|e| tracing::warn!("S3 auto-restore: list failed: {}", e))
        .ok()?;
    let mut ids: Vec<String> = results
        .iter()
        .flat_map(|r| r.common_prefixes.iter().flatten())
        .filter_map(|p| extract_s3_snapshot_tenant_id(&p.prefix))
        .collect();
    ids.sort();
    ids.dedup();
    Some(ids)
}

/// TODO: Document extract_s3_snapshot_tenant_id.
fn extract_s3_snapshot_tenant_id(prefix: &str) -> Option<String> {
    let tenant = prefix
        .strip_prefix("snapshots/")
        .and_then(|s| s.strip_suffix("/"))?;
    if tenant.is_empty()
        || tenant == "."
        || tenant == ".."
        || tenant.contains('/')
        || tenant.contains('\\')
    {
        tracing::warn!(
            "S3 auto-restore: ignoring path-unsafe tenant snapshot prefix {:?}",
            prefix
        );
        return None;
    }
    Some(tenant.to_string())
}

/// Downloads and imports the latest S3 snapshot for a tenant during startup,
/// logging errors but not failing the boot sequence.
async fn restore_tenant_from_s3(
    manager: &Arc<flapjack::IndexManager>,
    s3_config: &flapjack::index::s3::S3Config,
    tid: &str,
) {
    if !is_path_safe_snapshot_tenant_id(tid) {
        return;
    }

    let Some((key, data)) = download_latest_tenant_snapshot(s3_config, tid).await else {
        return;
    };
    let data_len = data.len();
    if import_tenant_snapshot(manager, tid, data).await.is_none() {
        return;
    }
    tracing::info!(
        "S3 auto-restore: restored {} from {} ({} bytes)",
        tid,
        key,
        data_len
    );
}

fn is_path_safe_snapshot_tenant_id(tid: &str) -> bool {
    if extract_s3_snapshot_tenant_id(&format!("snapshots/{tid}/")).is_some() {
        return true;
    }
    tracing::warn!("S3 auto-restore: refusing path-unsafe tenant id {:?}", tid);
    false
}

async fn download_latest_tenant_snapshot(
    s3_config: &flapjack::index::s3::S3Config,
    tid: &str,
) -> Option<(String, Vec<u8>)> {
    match flapjack::index::s3::download_latest_snapshot(s3_config, tid).await {
        Ok(snapshot) => Some(snapshot),
        Err(error) => {
            tracing::warn!("S3 auto-restore: no snapshot for {}: {}", tid, error);
            None
        }
    }
}

async fn import_tenant_snapshot(
    manager: &Arc<flapjack::IndexManager>,
    tid: &str,
    data: Vec<u8>,
) -> Option<()> {
    match crate::startup_catchup::restore_snapshot_bytes(manager, tid, data).await {
        Ok(()) => Some(()),
        Err((step, error)) => {
            tracing::error!(
                "S3 auto-restore: failed to install {} at {}: {}",
                tid,
                step.as_tag(),
                error
            );
            None
        }
    }
}

/// Run an infinite loop that periodically snapshots every tenant index to S3.
pub(crate) async fn scheduled_s3_backups(
    data_dir: String,
    s3_config: flapjack::index::s3::S3Config,
    manager: std::sync::Arc<flapjack::IndexManager>,
    mutation_fence: crate::pause_registry::GlobalMutationFence,
    interval_secs: u64,
) {
    let data_path = std::path::PathBuf::from(data_dir);
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
    interval.tick().await;
    loop {
        interval.tick().await;
        if !run_background_mutation_if_admitted(&mutation_fence, || {
            run_scheduled_s3_backup_pass(&manager, &s3_config, data_path.as_path())
        })
        .await
        {
            tracing::debug!("[BACKUP] Release mutation fence active; scheduled snapshot skipped");
        }
    }
}

async fn run_background_mutation_if_admitted<Run, RunFuture>(
    mutation_fence: &crate::pause_registry::GlobalMutationFence,
    run: Run,
) -> bool
where
    Run: FnOnce() -> RunFuture,
    RunFuture: std::future::Future<Output = ()>,
{
    let Ok(_mutation_permit) = mutation_fence.admit_mutation().await else {
        return false;
    };
    run().await;
    true
}

async fn run_analytics_retention_pass_if_admitted(
    mutation_fence: &crate::pause_registry::GlobalMutationFence,
    analytics_dir: &std::path::Path,
    retention_days: u32,
    phase: &str,
) -> bool {
    run_background_mutation_if_admitted(mutation_fence, || async move {
        match flapjack::analytics::retention::cleanup_old_partitions(analytics_dir, retention_days)
        {
            Ok(removed) => tracing::info!(removed, phase, "[analytics] Retention cleanup complete"),
            Err(error) => tracing::warn!(error, phase, "[analytics] Retention cleanup failed"),
        }
    })
    .await
}

async fn run_startup_analytics_retention_pass(
    mutation_fence: &crate::pause_registry::GlobalMutationFence,
    analytics_dir: &std::path::Path,
    retention_days: u32,
) {
    if !run_analytics_retention_pass_if_admitted(
        mutation_fence,
        analytics_dir,
        retention_days,
        "Startup",
    )
    .await
    {
        tracing::debug!("[analytics] Release mutation fence active; startup retention skipped");
    }
}

async fn run_scheduled_analytics_retention_pass(
    mutation_fence: &crate::pause_registry::GlobalMutationFence,
    analytics_dir: &std::path::Path,
    retention_days: u32,
) {
    if !run_analytics_retention_pass_if_admitted(
        mutation_fence,
        analytics_dir,
        retention_days,
        "Retention",
    )
    .await
    {
        tracing::debug!("[analytics] Release mutation fence active; retention pass skipped");
    }
}

async fn run_analytics_retention_loop(
    mutation_fence: crate::pause_registry::GlobalMutationFence,
    analytics_dir: std::path::PathBuf,
    retention_days: u32,
) {
    if retention_days == 0 {
        tracing::info!("[analytics] Retention cleanup disabled (retention_days=0)");
        return;
    }

    run_startup_analytics_retention_pass(&mutation_fence, &analytics_dir, retention_days).await;

    let mut interval = tokio::time::interval(Duration::from_secs(86_400));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval.tick().await;
    loop {
        interval.tick().await;
        run_scheduled_analytics_retention_pass(&mutation_fence, &analytics_dir, retention_days)
            .await;
    }
}

/// Runs a single scheduled S3 backup pass: iterates all tenant directories and
/// uploads a fresh snapshot for each to the configured S3 bucket.
async fn run_scheduled_s3_backup_pass(
    manager: &Arc<flapjack::IndexManager>,
    s3_config: &flapjack::index::s3::S3Config,
    data_path: &std::path::Path,
) {
    tracing::info!("[BACKUP] Starting scheduled S3 snapshot...");

    let tenant_dirs = match visible_tenant_dir_names(data_path) {
        Ok(dirs) => dirs,
        Err(error) => {
            tracing::error!("[BACKUP] Failed to read data dir: {}", error);
            return;
        }
    };

    for tenant in &tenant_dirs {
        backup_tenant_to_s3(manager, s3_config, tenant, data_path).await;
    }

    tracing::info!(
        "[BACKUP] Scheduled snapshot complete ({} tenants)",
        tenant_dirs.len()
    );
}

/// Backs up a single tenant's index data to S3 via snapshot.
async fn backup_tenant_to_s3(
    manager: &Arc<flapjack::IndexManager>,
    s3_config: &flapjack::index::s3::S3Config,
    tenant: &str,
    data_path: &std::path::Path,
) {
    let bytes = match export_tenant_snapshot(manager, tenant, data_path).await {
        Some(b) => b,
        None => return,
    };
    match flapjack::index::s3::upload_snapshot(s3_config, tenant, &bytes).await {
        Ok(key) => {
            enforce_backup_retention(s3_config, tenant).await;
            tracing::info!("[BACKUP] {} -> {} ({} bytes)", tenant, key, bytes.len());
        }
        Err(e) => tracing::error!("[BACKUP] upload {} failed: {}", tenant, e),
    }
}

async fn export_tenant_snapshot(
    manager: &Arc<flapjack::IndexManager>,
    tenant: &str,
    data_path: &std::path::Path,
) -> Option<Vec<u8>> {
    // Drain and merge-quiesce any live writer so the scheduled backup captures a
    // quiesced generation, then run the blocking gzip/tar export off the async
    // worker pool through the shared byte seam.
    let _quiesce = quiesce_backup_tenant(manager, tenant).await?;
    let index_path = data_path.join(tenant);
    let tenant_owned = tenant.to_string();
    match tokio::task::spawn_blocking(move || {
        crate::snapshot_byte_ops::export_snapshot_bytes(&index_path, &tenant_owned)
    })
    .await
    {
        Ok(Ok(b)) => Some(b),
        Ok(Err(e)) => {
            tracing::error!("[BACKUP] export {} failed: {}", tenant, e);
            None
        }
        Err(join_error) => {
            tracing::error!("[BACKUP] export {} task failed: {}", tenant, join_error);
            None
        }
    }
}

async fn quiesce_backup_tenant(
    manager: &Arc<flapjack::IndexManager>,
    tenant: &str,
) -> Option<flapjack::index::manager::TenantQuiesce> {
    match manager.quiesce_tenant(&tenant.to_string()).await {
        Ok(quiesce) => Some(quiesce),
        Err(error) => {
            tracing::error!("[BACKUP] quiesce {} failed: {}", tenant, error);
            None
        }
    }
}

async fn enforce_backup_retention(s3_config: &flapjack::index::s3::S3Config, tenant: &str) {
    let retention = std::env::var("FLAPJACK_SNAPSHOT_RETENTION")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(24);
    match flapjack::index::s3::enforce_retention(s3_config, tenant, retention).await {
        Ok(_) => {}
        Err(error) => {
            tracing::error!(
                tenant = tenant,
                "[BACKUP] retention delete failed: {}",
                error
            );
        }
    }
}

/// Spawn all server background tasks, including analytics, replication, and maintenance loops.
pub(crate) fn spawn_background_tasks(
    state: &Arc<AppState>,
    infrastructure: &InfrastructureState,
) -> Result<StorageMaintenanceRegistration, String> {
    // Validate every externally configured duration before spawning anything.
    // This avoids a partial background subsystem when one Tokio interval would
    // otherwise panic after the server has begun accepting traffic.
    let intervals = BackgroundTaskIntervals::from_env()?;
    let health = Arc::clone(&state.background_task_health);
    spawn_ssl_renewal(infrastructure, &health);
    spawn_analytics_tasks(state, infrastructure, intervals.rollup_secs, &health);
    spawn_s3_backup_task(state, infrastructure, &health);
    spawn_replication_tasks(state, infrastructure, intervals.sync_secs, &health);
    let storage_maintenance = spawn_storage_maintenance_task(state);
    spawn_usage_rollup_task(state);
    spawn_metrics_refresh_task(state);
    spawn_usage_alert_task(state);
    Ok(storage_maintenance)
}

fn migration_spool_gc_interval_secs() -> u64 {
    std::env::var(MIGRATION_SPOOL_GC_INTERVAL_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_MIGRATION_SPOOL_GC_INTERVAL_SECS)
}

fn parse_autoheal_enabled(value: Option<&str>) -> Result<bool, String> {
    let Some(raw_value) = value else {
        return Ok(false);
    };
    match raw_value.trim() {
        value if value.eq_ignore_ascii_case("true") => Ok(true),
        value if value.eq_ignore_ascii_case("false") => Ok(false),
        _ => Err(raw_value.to_string()),
    }
}

fn autoheal_enabled_from_env() -> bool {
    match parse_autoheal_enabled(std::env::var(AUTOHEAL_ENABLED_ENV).ok().as_deref()) {
        Ok(enabled) => enabled,
        Err(value) => {
            tracing::warn!(
                "[autoheal] ignoring invalid {} value {:?}; using false",
                AUTOHEAL_ENABLED_ENV,
                value
            );
            false
        }
    }
}

/// Compile-time evidence that the required storage-maintenance loop was
/// registered by server startup. Returning this from `spawn_background_tasks`
/// makes deleting or commenting its production registration a build failure.
pub(crate) struct StorageMaintenanceRegistration;

fn spawn_storage_maintenance_task(state: &Arc<AppState>) -> StorageMaintenanceRegistration {
    let interval_secs = migration_spool_gc_interval_secs();
    // Do not create the spool layout while a persisted release fence is active.
    // The first admitted maintenance pass initializes the ordinary store and
    // every later pass reuses it.
    let spool_store = Arc::new(Mutex::new(None::<SpoolStore>));
    let spool_data_root = state.manager.base_path.clone();
    let crawler_store = CrawlerRunStore::new(&state.manager.base_path);
    let mutation_fence = state.global_mutation_fence.clone();

    spawn_supervised(
        Arc::clone(&state.background_task_health),
        "storage-maintenance",
        async move {
            run_migration_spool_gc_loop(Duration::from_secs(interval_secs), move || {
                let spool_store = spool_store.clone();
                let spool_data_root = spool_data_root.clone();
                let crawler_store = crawler_store.clone();
                let mutation_fence = mutation_fence.clone();
                async move {
                    let Ok(_mutation_permit) = mutation_fence.admit_mutation().await else {
                        return Ok(());
                    };
                    let spool_store = {
                        let mut initialized = spool_store
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if initialized.is_none() {
                            match SpoolStore::new(&spool_data_root, SpoolLimits::default()) {
                                Ok(store) => *initialized = Some(store),
                                Err(error) => tracing::error!(
                                    "[maintenance] Migration spool GC disabled: {}",
                                    error
                                ),
                            }
                        }
                        initialized.clone()
                    };
                    let now_unix_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?
                        .as_millis()
                        .try_into()
                        .map_err(|_| "system clock exceeds crawler timestamp range".to_string())?;
                    run_storage_maintenance_pass(spool_store.as_ref(), &crawler_store, now_unix_ms)
                        .map(|_| ())
                }
            })
            .await;
        },
    );
    tracing::info!(
        "[maintenance] Storage cleanup enabled (interval={}s)",
        interval_secs
    );
    StorageMaintenanceRegistration
}

async fn run_migration_spool_gc_loop<RunPass, PassFuture, PassError>(
    interval_duration: Duration,
    mut run_pass: RunPass,
) where
    RunPass: FnMut() -> PassFuture,
    PassFuture: Future<Output = Result<(), PassError>>,
    PassError: fmt::Display,
{
    let mut interval = tokio::time::interval(interval_duration);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval.tick().await;
    loop {
        interval.tick().await;
        if let Err(error) = run_pass().await {
            tracing::error!("[maintenance] Storage cleanup pass failed: {}", error);
        }
    }
}

fn run_migration_spool_gc_pass(store: &SpoolStore) -> Result<(), SpoolError> {
    store.collect_garbage()
}

fn run_storage_maintenance_pass(
    spool_store: Option<&SpoolStore>,
    crawler_store: &CrawlerRunStore,
    now_unix_ms: u64,
) -> Result<usize, String> {
    let spool_error = spool_store
        .and_then(|store| run_migration_spool_gc_pass(store).err())
        .map(|error| error.to_string());
    let crawler_result = crawler_store.prune(now_unix_ms);

    if let Ok(removed) = crawler_result.as_ref() {
        if *removed > 0 {
            tracing::info!(removed, "[maintenance] Pruned acknowledged crawler runs");
        }
    }

    match (spool_error, crawler_result) {
        (None, Ok(removed)) => Ok(removed),
        (Some(spool_error), Ok(_)) => Err(format!("migration spool GC failed: {spool_error}")),
        (None, Err(crawler_error)) => Err(format!("crawler retention failed: {crawler_error}")),
        (Some(spool_error), Err(crawler_error)) => Err(format!(
            "migration spool GC failed: {spool_error}; crawler retention failed: {crawler_error}"
        )),
    }
}

fn spawn_ssl_renewal(infrastructure: &InfrastructureState, health: &Arc<BackgroundTaskHealth>) {
    if let Some(config) = infrastructure
        .managed_ssl
        .as_ref()
        .and_then(|configured| configured.deferred_config.clone())
    {
        let mutation_fence = infrastructure.global_mutation_fence.clone();
        spawn_supervised(Arc::clone(health), "ssl-renewal-deferred", async move {
            // A fenced target must serve existing material without contacting
            // ACME. Poll the persisted fence and initialize the ordinary owner
            // only after the release transaction durably reopens mutations.
            loop {
                if mutation_fence.status().await.is_none() {
                    let _permit = match mutation_fence.admit_mutation().await {
                        Ok(permit) => permit,
                        Err(_) => continue,
                    };
                    match flapjack::SslManager::new(config).await {
                        Ok(manager) => {
                            flapjack_ssl::set_global_manager(Arc::clone(&manager));
                            drop(_permit);
                            manager.start_renewal_loop().await;
                        }
                        Err(error) => {
                            tracing::error!(error = %error, "[SSL] Deferred initialization failed");
                        }
                    }
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        tracing::info!("[SSL] Auto-renewal deferred until release fence opens");
    }
    let plan = ssl_background_plan(
        infrastructure
            .managed_ssl
            .as_ref()
            .map(|configured| ConfiguredSsl {
                manager: configured.manager.as_ref(),
                material_dir: configured.material_dir.as_path(),
            }),
        infrastructure.tls_resolver.as_ref(),
    );

    if let Some(ssl_manager) = plan.renewal_manager {
        let ssl_manager = Arc::clone(ssl_manager);
        spawn_supervised(Arc::clone(health), "ssl-renewal", async move {
            ssl_manager.start_renewal_loop().await
        });
        tracing::info!("[SSL] Auto-renewal enabled (checks every 24h)");
    }

    let Some(observer) = plan.material_observer else {
        return;
    };
    let resolver = Arc::clone(observer.resolver);
    let material_dir = observer.material_dir.to_path_buf();
    let expiry_manager = observer.expiry_manager.map(Arc::clone);
    spawn_supervised(Arc::clone(health), "tls-material-observer", async move {
        crate::tls_serve::run_tls_material_observer(
            resolver,
            material_dir,
            move || {
                let expiry_manager = expiry_manager.clone();
                async move {
                    match expiry_manager {
                        Some(ssl_manager) => ssl_manager.get_status().await.cert_expires_in_days,
                        None => None,
                    }
                }
            },
            |_| {},
        )
        .await
    });
    tracing::info!("[TLS] Certificate material observer enabled");
}

/// Spawns background tasks for analytics rollup and retention cleanup.
fn spawn_analytics_tasks(
    state: &Arc<AppState>,
    infrastructure: &InfrastructureState,
    rollup_interval_secs: u64,
    health: &Arc<BackgroundTaskHealth>,
) {
    if !infrastructure.analytics_config.enabled {
        tracing::info!("[analytics] Analytics disabled");
        return;
    }

    let collector = Arc::clone(&infrastructure.analytics_collector);
    let flush_interval_secs = infrastructure.analytics_config.flush_interval_secs;
    let mutation_fence = state.global_mutation_fence.clone();
    spawn_supervised(Arc::clone(health), "analytics-flush", async move {
        let mut interval = tokio::time::interval(Duration::from_secs(flush_interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            interval.tick().await;
            if !run_background_mutation_if_admitted(&mutation_fence, || async {
                collector.run_periodic_flush_pass();
            })
            .await
            {
                tracing::debug!(
                    "[analytics] Release mutation fence active; periodic flush skipped"
                );
            }
        }
    });

    let retention_dir = infrastructure.analytics_config.data_dir.clone();
    let retention_days = infrastructure.analytics_config.retention_days;
    let mutation_fence = state.global_mutation_fence.clone();
    spawn_supervised(Arc::clone(health), "analytics-retention", async move {
        run_analytics_retention_loop(mutation_fence, retention_dir, retention_days).await;
    });

    tracing::info!(
        "[analytics] Analytics enabled (flush every {}s, retain {}d)",
        infrastructure.analytics_config.flush_interval_secs,
        infrastructure.analytics_config.retention_days
    );

    spawn_local_rollup_generation_task(state, infrastructure, rollup_interval_secs, health);

    if let Some(cluster) = crate::analytics_cluster::get_global_cluster() {
        let local_node_id = cluster.node_id().to_string();
        let handle = crate::rollup_broadcaster::spawn_rollup_broadcaster(
            Arc::clone(&infrastructure.analytics_engine),
            infrastructure.analytics_config.clone(),
            cluster,
            local_node_id,
            rollup_interval_secs,
            state.global_mutation_fence.clone(),
        );
        supervise_handle(Arc::clone(health), "rollup-broadcast", handle);
        tracing::info!(
            "[ROLLUP-BROADCAST] Broadcaster started (interval={}s)",
            rollup_interval_secs
        );
    }
}

/// TODO: Document spawn_local_rollup_generation_task.
fn spawn_local_rollup_generation_task(
    state: &Arc<AppState>,
    infrastructure: &InfrastructureState,
    rollup_interval_secs: u64,
    health: &Arc<BackgroundTaskHealth>,
) {
    let collector = Arc::clone(&infrastructure.analytics_collector);
    let analytics_config = infrastructure.analytics_config.clone();
    let mutation_fence = state.global_mutation_fence.clone();
    spawn_supervised(Arc::clone(health), "local-rollup-generation", async move {
        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_secs(rollup_interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            interval.tick().await;
            if !run_background_mutation_if_admitted(&mutation_fence, || async {
                run_local_rollup_generation_pass(&analytics_config, &collector);
            })
            .await
            {
                tracing::debug!(
                    "[analytics] Release mutation fence active; local rollup generation skipped"
                );
            }
        }
    });
    tracing::info!(
        "[analytics] Local rollup generation enabled (interval={}s)",
        rollup_interval_secs
    );
}

/// TODO: Document run_local_rollup_generation_pass.
fn run_local_rollup_generation_pass(
    analytics_config: &flapjack::analytics::AnalyticsConfig,
    collector: &Arc<flapjack::analytics::AnalyticsCollector>,
) {
    let indexes = discover_rollup_indexes(analytics_config);
    if indexes.is_empty() {
        return;
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let (hour_start_ms, hour_end_ms) = rollup_window_bounds_ms(now_ms);

    for index_name in indexes {
        let started = Instant::now();
        match collector.flush_rollup_window_with_event_count(
            &index_name,
            "1hour",
            hour_start_ms,
            hour_end_ms,
        ) {
            Ok((_path, event_count)) => {
                collector.record_rollup_generation_sample(
                    started.elapsed().as_secs_f64() * 1000.0,
                    event_count,
                    hour_end_ms,
                );
            }
            Err(error) => {
                tracing::debug!(
                    "[analytics] rollup generation skipped for index={} window_start_ms={} reason={}",
                    index_name,
                    hour_start_ms,
                    error
                );
            }
        }
    }
}

fn rollup_window_bounds_ms(now_ms: i64) -> (i64, i64) {
    // Keep the HTTP-layer scheduler aligned with the core analytics writer's
    // single owner for the test-only rollup-window override contract.
    let window_ms = flapjack::analytics::resolved_hourly_rollup_window_ms();
    let window_end_ms = now_ms.div_euclid(window_ms) * window_ms;
    let window_start_ms = window_end_ms - window_ms;
    (window_start_ms, window_end_ms)
}

/// TODO: Document discover_rollup_indexes.
fn discover_rollup_indexes(analytics_config: &flapjack::analytics::AnalyticsConfig) -> Vec<String> {
    let mut indexes = Vec::new();
    let Ok(entries) = std::fs::read_dir(&analytics_config.data_dir) else {
        return indexes;
    };

    for entry in entries.flatten() {
        let index_path = entry.path();
        if !index_path.is_dir() {
            continue;
        }
        if !index_path.join("searches").is_dir() {
            continue;
        }
        if let Some(index_name) = index_path.file_name().and_then(|name| name.to_str()) {
            indexes.push(index_name.to_string());
        }
    }
    indexes.sort();
    indexes.dedup();
    indexes
}

#[cfg(test)]
mod snapshot_restore_tests {
    use super::import_tenant_snapshot;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn s3_auto_restore_rejects_foreign_tenant_snapshot_without_live_residue() {
        let source_tmp = TempDir::new().unwrap();
        let source_manager = flapjack::IndexManager::new(source_tmp.path());
        source_manager.create_tenant("foreign_source").unwrap();
        source_manager
            .add_documents_sync(
                "foreign_source",
                vec![flapjack::types::Document::from_json(&serde_json::json!({
                    "objectID": "foreign_doc",
                    "title": "foreign generation"
                }))
                .unwrap()],
            )
            .await
            .unwrap();
        source_manager.graceful_shutdown().await;
        let snapshot = flapjack::index::snapshot::export_to_bytes(
            &source_manager.base_path.join("foreign_source"),
        )
        .unwrap();

        let destination_tmp = TempDir::new().unwrap();
        let destination_manager = Arc::new(flapjack::IndexManager::new(destination_tmp.path()));

        assert!(
            import_tenant_snapshot(&destination_manager, "expected_target", snapshot)
            .await
            .is_none(),
            "auto-restore must reject a snapshot whose retained tenant identity does not match the S3 prefix"
        );
        assert!(
            !destination_tmp.path().join("expected_target").exists(),
            "a rejected S3 snapshot must not leave a partial live tenant tree"
        );
    }
}

/// Spawns a periodic S3 snapshot backup task for all tenants.
fn spawn_s3_backup_task(
    state: &Arc<AppState>,
    infrastructure: &InfrastructureState,
    health: &Arc<BackgroundTaskHealth>,
) {
    if let Some(s3_config) = infrastructure.s3_config.as_ref() {
        if let Some(interval_secs) = infrastructure.s3_snapshot_interval_secs {
            let data_dir = infrastructure
                .manager
                .base_path
                .to_string_lossy()
                .to_string();
            let manager = Arc::clone(&infrastructure.manager);
            let config = s3_config.clone();
            let mutation_fence = state.global_mutation_fence.clone();
            spawn_supervised(Arc::clone(health), "s3-backup", async move {
                scheduled_s3_backups(data_dir, config, manager, mutation_fence, interval_secs)
                    .await;
            });
            tracing::info!("Scheduled S3 backups every {}s", interval_secs);
        }
    }
}

/// Spawns replication health probe and periodic peer-sync tasks.
fn spawn_replication_tasks(
    state: &Arc<AppState>,
    infrastructure: &InfrastructureState,
    sync_interval_secs: u64,
    health: &Arc<BackgroundTaskHealth>,
) {
    if let Some(replication_manager) = infrastructure.replication_manager.as_ref() {
        let autoheal_enabled = autoheal_enabled_from_env();
        let mutation_fence = state.global_mutation_fence.clone();
        replication_manager.start_health_probe_with_admission(10, autoheal_enabled, move || {
            let mutation_fence = mutation_fence.clone();
            async move { mutation_fence.admit_mutation().await.ok() }
        });
        let health_probe = Arc::clone(replication_manager);
        spawn_supervised(
            Arc::clone(health),
            "replication-health-watchdog",
            async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if !health_probe.health_probe_is_running() {
                        return;
                    }
                }
            },
        );
        tracing::info!("[autoheal] enabled={}", autoheal_enabled);
        // NOTE: One-shot startup catch-up moved to server.rs as a pre-serve barrier
        // (run_pre_serve_catchup). Only periodic sync remains as a background task.
        let handle =
            crate::startup_catchup::spawn_periodic_sync(Arc::clone(state), sync_interval_secs);
        supervise_handle(Arc::clone(health), "replication-sync", handle);
        tracing::info!(
            "[REPL-sync] Periodic anti-entropy sync enabled (interval={}s)",
            sync_interval_secs
        );
    }
}

/// Compute the just-completed UTC day for `now`, formatted as the date string
/// consumed by `UsagePersistence`. At or after midnight this is the prior
/// calendar day, so a rollover that wakes just after midnight persists the day
/// that just ended rather than the newly-started day.
fn completed_utc_day(now: chrono::DateTime<chrono::Utc>) -> String {
    (now.date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

/// Run one daily usage rollover: capture live gauges through the single-owner
/// `usage_capture` path, then persist counters and gauges into the completed
/// day's snapshot in one atomic write before `UsagePersistence` resets the
/// counters. Returns the completed day on success.
fn run_usage_rollover(
    now: chrono::DateTime<chrono::Utc>,
    persistence: &crate::usage_persistence::UsagePersistence,
    counters: &dashmap::DashMap<String, crate::usage_middleware::TenantUsageCounters>,
    metrics_state: Option<&crate::handlers::metrics::MetricsState>,
) -> std::io::Result<String> {
    let completed_day = completed_utc_day(now);
    let captured_gauges = crate::usage_capture::capture_live_gauges(metrics_state);
    persistence.rollup_with_gauges(&completed_day, counters, &captured_gauges)?;
    Ok(completed_day)
}

/// Spawns a periodic task to flush in-memory usage counters and live gauges to disk.
fn spawn_usage_rollup_task(state: &Arc<AppState>) {
    if let Some(persistence) = state.usage_persistence.clone() {
        let counters = Arc::clone(&state.usage_counters);
        let metrics_state = state.metrics_state.clone();
        let mutation_fence = state.global_mutation_fence.clone();
        spawn_supervised(
            Arc::clone(&state.background_task_health),
            "usage-rollup",
            async move {
                loop {
                    let now = chrono::Utc::now();
                    let tomorrow = (now + chrono::Duration::days(1))
                        .date_naive()
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_utc();
                    let secs_until_midnight = (tomorrow - now).num_seconds().max(1) as u64;
                    tokio::time::sleep(tokio::time::Duration::from_secs(secs_until_midnight)).await;

                    // Capture `now` after waking so the completed-day helper selects
                    // the just-completed UTC date rather than the new current day.
                    let rollup_now = chrono::Utc::now();
                    let completed_day = completed_utc_day(rollup_now);
                    if !run_background_mutation_if_admitted(&mutation_fence, || async {
                        match run_usage_rollover(
                            rollup_now,
                            &persistence,
                            &counters,
                            metrics_state.as_ref(),
                        ) {
                            Ok(_) => tracing::info!(
                                "[usage] Daily rollup complete (date={})",
                                completed_day
                            ),
                            Err(e) => tracing::error!(
                                "[usage] Daily rollup failed (date={}): {}",
                                completed_day,
                                e
                            ),
                        }
                    })
                    .await
                    {
                        tracing::debug!(
                            "[usage] Release mutation fence active; daily rollup skipped"
                        );
                    }
                }
            },
        );
    }
}

/// Build a complete per-index gauge generation off-lock, then publish it once.
///
/// Inventory failure retains the entire prior generation. Per-index read
/// failures retain that dimension's last known value (or unknown when none was
/// ever published). Only the authoritative visible-directory inventory removes
/// an index from the next generation.
pub(crate) fn refresh_metrics_snapshot(
    manager: &flapjack::IndexManager,
    metrics_state: &crate::handlers::metrics::MetricsState,
) -> std::io::Result<()> {
    let tenant_ids = visible_tenant_dir_names(&manager.base_path)?;
    let previous = metrics_state.index_gauge_snapshot();
    let mut next = crate::handlers::metrics::IndexGaugeSnapshot::new();

    for tenant_id in tenant_ids {
        let prior = previous.get(&tenant_id).copied().unwrap_or_default();
        let documents_count = match manager.tenant_durable_doc_count(&tenant_id) {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(
                    tenant = %tenant_id,
                    error = %error,
                    "metrics refresh retained prior document count"
                );
                prior.documents_count
            }
        };
        let storage_bytes = match manager.try_tenant_storage_bytes(&tenant_id) {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(
                    tenant = %tenant_id,
                    error = %error,
                    "metrics refresh retained prior storage measurement"
                );
                prior.storage_bytes
            }
        };
        next.insert(
            tenant_id,
            crate::handlers::metrics::IndexGaugeValues {
                documents_count,
                storage_bytes,
            },
        );
    }

    metrics_state.replace_index_gauges(next);
    Ok(())
}

/// Spawns a periodic task to refresh per-index Prometheus metric gauges.
fn spawn_metrics_refresh_task(state: &Arc<AppState>) {
    let manager = Arc::clone(&state.manager);
    if let Some(ms) = state.metrics_state.clone() {
        spawn_supervised(
            Arc::clone(&state.background_task_health),
            "metrics-refresh",
            async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    if let Err(error) = refresh_metrics_snapshot(&manager, &ms) {
                        tracing::warn!(
                            error = %error,
                            "metrics refresh retained prior generation after inventory failure"
                        );
                    }
                }
            },
        );
    }
}

/// Spawns a periodic task to check usage thresholds and send alert notifications.
fn spawn_usage_alert_task(state: &Arc<AppState>) {
    let search_threshold: u64 = std::env::var("FLAPJACK_USAGE_ALERT_THRESHOLD_SEARCHES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let write_threshold: u64 = std::env::var("FLAPJACK_USAGE_ALERT_THRESHOLD_WRITES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if search_threshold == 0 && write_threshold == 0 {
        return;
    }
    let counters = Arc::clone(&state.usage_counters);
    spawn_supervised(
        Arc::clone(&state.background_task_health),
        "usage-alerts",
        async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                if let Some(notifier) = crate::notifications::global_notifier() {
                    crate::notifications::check_usage_thresholds(
                        notifier,
                        &counters,
                        search_threshold,
                        write_threshold,
                    );
                }
            }
        },
    );
    tracing::info!(
        "[notifications] Usage threshold alerts enabled (searches={}, writes={})",
        search_threshold,
        write_threshold
    );
}

#[cfg(test)]
mod tests {
    include!("background_tasks_tests.rs");

    #[tokio::test]
    async fn release_fence_suppresses_scheduled_backup_mutation_pass() {
        let temp = tempfile::TempDir::new().unwrap();
        let data_root = temp.path().join("flapjack/data");
        std::fs::create_dir_all(&data_root).unwrap();
        let fence = crate::pause_registry::GlobalMutationFence::open(&data_root).unwrap();
        fence.acquire("release-backup-1").await.unwrap();

        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran_in_pass = std::sync::Arc::clone(&ran);
        assert!(
            !super::run_background_mutation_if_admitted(&fence, || async move {
                ran_in_pass.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .await
        );
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));

        fence.release("release-backup-1").await.unwrap();
        assert!(
            super::run_background_mutation_if_admitted(&fence, || async {
                ran.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .await
        );
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    }
}
