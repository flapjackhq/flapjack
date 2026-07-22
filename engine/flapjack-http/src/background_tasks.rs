use crate::handlers::AppState;
use crate::server_init::InfrastructureState;
use crate::tenant_dirs::{has_visible_tenant_dirs, visible_tenant_dir_names};
use std::sync::Arc;
use std::time::Instant;

#[cfg(test)]
const HOUR_MS: i64 = 3_600_000;

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
    _manager: &std::sync::Arc<flapjack::IndexManager>,
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
        restore_tenant_from_s3(s3_config, tid, data_path).await;
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
    s3_config: &flapjack::index::s3::S3Config,
    tid: &str,
    data_path: &std::path::Path,
) {
    if extract_s3_snapshot_tenant_id(&format!("snapshots/{tid}/")).is_none() {
        tracing::warn!("S3 auto-restore: refusing path-unsafe tenant id {:?}", tid);
        return;
    }
    match flapjack::index::s3::download_latest_snapshot(s3_config, tid).await {
        Ok((key, data)) => {
            let index_path = data_path.join(tid);
            if let Err(e) = flapjack::index::snapshot::import_from_bytes(&data, &index_path) {
                tracing::error!("S3 auto-restore: failed to import {}: {}", tid, e);
                return;
            }
            tracing::info!(
                "S3 auto-restore: restored {} from {} ({} bytes)",
                tid,
                key,
                data.len()
            );
        }
        Err(e) => tracing::warn!("S3 auto-restore: no snapshot for {}: {}", tid, e),
    }
}

/// Run an infinite loop that periodically snapshots every tenant index to S3.
pub(crate) async fn scheduled_s3_backups(
    data_dir: String,
    s3_config: flapjack::index::s3::S3Config,
    _manager: std::sync::Arc<flapjack::IndexManager>,
    interval_secs: u64,
) {
    let data_path = std::path::PathBuf::from(data_dir);
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
    interval.tick().await;
    loop {
        interval.tick().await;
        run_scheduled_s3_backup_pass(&s3_config, data_path.as_path()).await;
    }
}

/// Runs a single scheduled S3 backup pass: iterates all tenant directories and
/// uploads a fresh snapshot for each to the configured S3 bucket.
async fn run_scheduled_s3_backup_pass(
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
        backup_tenant_to_s3(s3_config, tenant, data_path).await;
    }

    tracing::info!(
        "[BACKUP] Scheduled snapshot complete ({} tenants)",
        tenant_dirs.len()
    );
}

/// Backs up a single tenant's index data to S3 via snapshot.
async fn backup_tenant_to_s3(
    s3_config: &flapjack::index::s3::S3Config,
    tenant: &str,
    data_path: &std::path::Path,
) {
    let bytes = match export_tenant_snapshot(tenant, data_path) {
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

fn export_tenant_snapshot(tenant: &str, data_path: &std::path::Path) -> Option<Vec<u8>> {
    let index_path = data_path.join(tenant);
    match flapjack::index::snapshot::export_to_bytes(&index_path) {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::error!("[BACKUP] export {} failed: {}", tenant, e);
            None
        }
    }
}

async fn enforce_backup_retention(s3_config: &flapjack::index::s3::S3Config, tenant: &str) {
    let retention = std::env::var("FLAPJACK_SNAPSHOT_RETENTION")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(24);
    let _ = flapjack::index::s3::enforce_retention(s3_config, tenant, retention).await;
}

/// Spawn all server background tasks, including analytics, replication, and maintenance loops.
pub(crate) fn spawn_background_tasks(state: &Arc<AppState>, infrastructure: &InfrastructureState) {
    spawn_ssl_renewal(infrastructure);
    spawn_analytics_tasks(infrastructure);
    spawn_s3_backup_task(infrastructure);
    spawn_replication_tasks(state, infrastructure);
    spawn_usage_rollup_task(state);
    spawn_metrics_refresh_task(state);
    spawn_usage_alert_task(state);
}

fn spawn_ssl_renewal(infrastructure: &InfrastructureState) {
    if let Some(ssl_manager) = infrastructure.ssl_manager.as_ref() {
        let ssl_manager = Arc::clone(ssl_manager);
        tokio::spawn(async move { ssl_manager.start_renewal_loop().await });
        tracing::info!("[SSL] Auto-renewal enabled (checks every 24h)");
    }
}

/// Spawns background tasks for analytics rollup and retention cleanup.
fn spawn_analytics_tasks(infrastructure: &InfrastructureState) {
    if !infrastructure.analytics_config.enabled {
        tracing::info!("[analytics] Analytics disabled");
        return;
    }

    let collector = Arc::clone(&infrastructure.analytics_collector);
    tokio::spawn(async move { collector.run_flush_loop().await });

    let retention_dir = infrastructure.analytics_config.data_dir.clone();
    let retention_days = infrastructure.analytics_config.retention_days;
    tokio::spawn(async move {
        flapjack::analytics::retention::run_retention_loop(retention_dir, retention_days).await;
    });

    tracing::info!(
        "[analytics] Analytics enabled (flush every {}s, retain {}d)",
        infrastructure.analytics_config.flush_interval_secs,
        infrastructure.analytics_config.retention_days
    );

    spawn_local_rollup_generation_task(infrastructure);

    if let Some(cluster) = crate::analytics_cluster::get_global_cluster() {
        let rollup_interval_secs = std::env::var("FLAPJACK_ROLLUP_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);
        let local_node_id = cluster.node_id().to_string();
        crate::rollup_broadcaster::spawn_rollup_broadcaster(
            Arc::clone(&infrastructure.analytics_engine),
            infrastructure.analytics_config.clone(),
            cluster,
            local_node_id,
            rollup_interval_secs,
        );
        tracing::info!(
            "[ROLLUP-BROADCAST] Broadcaster started (interval={}s)",
            rollup_interval_secs
        );
    }
}

fn spawn_local_rollup_generation_task(infrastructure: &InfrastructureState) {
    let rollup_interval_secs = std::env::var("FLAPJACK_ROLLUP_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let collector = Arc::clone(&infrastructure.analytics_collector);
    let analytics_config = infrastructure.analytics_config.clone();
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_secs(rollup_interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            interval.tick().await;
            run_local_rollup_generation_pass(&analytics_config, &collector);
        }
    });
    tracing::info!(
        "[analytics] Local rollup generation enabled (interval={}s)",
        rollup_interval_secs
    );
}

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
        match flapjack::analytics::writer::flush_rollup_window_with_event_count(
            analytics_config,
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

/// Spawns a periodic S3 snapshot backup task for all tenants.
fn spawn_s3_backup_task(infrastructure: &InfrastructureState) {
    if let Some(s3_config) = infrastructure.s3_config.as_ref() {
        if let Some(interval_secs) = infrastructure.s3_snapshot_interval_secs {
            let data_dir = infrastructure
                .manager
                .base_path
                .to_string_lossy()
                .to_string();
            let manager = Arc::clone(&infrastructure.manager);
            let config = s3_config.clone();
            tokio::spawn(async move {
                scheduled_s3_backups(data_dir, config, manager, interval_secs).await;
            });
            tracing::info!("Scheduled S3 backups every {}s", interval_secs);
        }
    }
}

/// Spawns replication health probe and periodic peer-sync tasks.
fn spawn_replication_tasks(state: &Arc<AppState>, infrastructure: &InfrastructureState) {
    if let Some(replication_manager) = infrastructure.replication_manager.as_ref() {
        replication_manager.start_health_probe(10);
        // NOTE: One-shot startup catch-up moved to server.rs as a pre-serve barrier
        // (run_pre_serve_catchup). Only periodic sync remains as a background task.
        let sync_interval: u64 = std::env::var("FLAPJACK_SYNC_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        crate::startup_catchup::spawn_periodic_sync(Arc::clone(state), sync_interval);
        tracing::info!(
            "[REPL-sync] Periodic anti-entropy sync enabled (interval={}s)",
            sync_interval
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
    manager: &flapjack::IndexManager,
    storage_gauges: Option<&dashmap::DashMap<String, u64>>,
) -> std::io::Result<String> {
    let completed_day = completed_utc_day(now);
    let captured_gauges = crate::usage_capture::capture_live_gauges(manager, storage_gauges);
    persistence.rollup_with_gauges(&completed_day, counters, &captured_gauges)?;
    Ok(completed_day)
}

/// Spawns a periodic task to flush in-memory usage counters and live gauges to disk.
fn spawn_usage_rollup_task(state: &Arc<AppState>) {
    if let Some(persistence) = state.usage_persistence.clone() {
        let counters = Arc::clone(&state.usage_counters);
        let manager = Arc::clone(&state.manager);
        let storage_gauges = state.metrics_state.clone().map(|m| m.storage_gauges);
        tokio::spawn(async move {
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
                match run_usage_rollover(
                    rollup_now,
                    &persistence,
                    &counters,
                    &manager,
                    storage_gauges.as_deref(),
                ) {
                    Ok(_) => {
                        tracing::info!("[usage] Daily rollup complete (date={})", completed_day)
                    }
                    Err(e) => tracing::error!(
                        "[usage] Daily rollup failed (date={}): {}",
                        completed_day,
                        e
                    ),
                }
            }
        });
    }
}

/// Spawns a periodic task to refresh per-index Prometheus metric gauges.
fn spawn_metrics_refresh_task(state: &Arc<AppState>) {
    let manager = Arc::clone(&state.manager);
    if let Some(ms) = state.metrics_state.clone() {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let storage = manager.all_tenant_storage();
                ms.storage_gauges.clear();
                for (tenant, bytes) in storage {
                    ms.storage_gauges.insert(tenant, bytes);
                }
            }
        });
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
    tokio::spawn(async move {
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
    });
    tracing::info!(
        "[notifications] Usage threshold alerts enabled (searches={}, writes={})",
        search_threshold,
        write_threshold
    );
}

#[cfg(test)]
mod tests {
    use super::{
        completed_utc_day, extract_s3_snapshot_tenant_id, rollup_window_bounds_ms,
        run_usage_rollover, HOUR_MS,
    };
    use crate::test_helpers::{with_env_var, TestStateBuilder};
    use crate::usage_persistence::UsagePersistence;
    use chrono::TimeZone;
    use std::sync::atomic::Ordering;

    #[test]
    fn completed_utc_day_returns_prior_day_at_and_after_midnight() {
        let midnight = chrono::Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 0).unwrap();
        assert_eq!(completed_utc_day(midnight), "2026-07-19");

        let one_second_after = chrono::Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 1).unwrap();
        assert_eq!(completed_utc_day(one_second_after), "2026-07-19");

        let just_before_midnight = chrono::Utc
            .with_ymd_and_hms(2026, 7, 19, 23, 59, 59)
            .unwrap();
        assert_eq!(completed_utc_day(just_before_midnight), "2026-07-18");
    }

    #[test]
    fn s3_snapshot_tenant_prefix_rejects_path_traversal_components() {
        assert_eq!(
            extract_s3_snapshot_tenant_id("snapshots/products/"),
            Some("products".to_string())
        );
        assert_eq!(
            extract_s3_snapshot_tenant_id("snapshots/products_v2-2026/"),
            Some("products_v2-2026".to_string())
        );

        for prefix in [
            "snapshots/../",
            "snapshots/./",
            "snapshots//",
            "snapshots/nested/index/",
            "snapshots\\windows\\",
            "not-snapshots/products/",
        ] {
            assert_eq!(
                extract_s3_snapshot_tenant_id(prefix),
                None,
                "{prefix} must not become a local restore path component"
            );
        }
    }

    #[tokio::test]
    async fn one_shot_rollover_persists_completed_day_and_resets_counters() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let gauges = state.metrics_state.as_ref().unwrap().storage_gauges.clone();

        // Seed all seven counter fields with distinct non-zero values.
        {
            let entry = state
                .usage_counters
                .entry("products".to_string())
                .or_default();
            entry.search_count.store(11, Ordering::Relaxed);
            entry.write_count.store(22, Ordering::Relaxed);
            entry.read_count.store(33, Ordering::Relaxed);
            entry.bytes_in.store(44, Ordering::Relaxed);
            entry.search_results_total.store(55, Ordering::Relaxed);
            entry.documents_indexed_total.store(66, Ordering::Relaxed);
            entry.documents_deleted_total.store(77, Ordering::Relaxed);
        }

        // Wake just after midnight: the just-completed day is 2026-07-19.
        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 5).unwrap();
        let completed_day = run_usage_rollover(
            now,
            &persistence,
            &state.usage_counters,
            &state.manager,
            Some(&gauges),
        )
        .unwrap();

        assert_eq!(completed_day, "2026-07-19");
        assert!(
            tmp.path().join("_usage/2026-07-19.json").exists(),
            "completed-day snapshot must be written"
        );
        assert!(
            !tmp.path().join("_usage/2026-07-20.json").exists(),
            "the newly-started day must not be persisted"
        );

        // Persisted snapshot preserves the exact seeded counter values.
        let snapshot = persistence
            .load_snapshot("2026-07-19")
            .unwrap()
            .expect("completed-day snapshot should load");
        let products = &snapshot.indexes["products"];
        assert_eq!(products.search_operations, 11);
        assert_eq!(products.total_write_operations, 22);
        assert_eq!(products.total_read_operations, 33);
        assert_eq!(products.bytes_received, 44);
        assert_eq!(products.search_results_total, 55);
        assert_eq!(products.records, 66);
        assert_eq!(products.documents_deleted, 77);

        // Live atomics are reset to zero after the helper returns.
        let entry = state.usage_counters.get("products").unwrap();
        assert_eq!(entry.search_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.write_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.read_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.bytes_in.load(Ordering::Relaxed), 0);
        assert_eq!(entry.search_results_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_indexed_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_deleted_total.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn one_shot_rollover_unions_gauges_and_preserves_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let gauges = state.metrics_state.as_ref().unwrap().storage_gauges.clone();
        gauges.clear();

        // "products": loaded with 3 documents and a storage gauge.
        state.manager.create_tenant("products").unwrap();
        state
            .manager
            .add_documents_sync(
                "products",
                (0..3u64)
                    .map(|i| flapjack::types::Document {
                        id: format!("products_{i}"),
                        fields: std::collections::HashMap::new(),
                    })
                    .collect(),
            )
            .await
            .unwrap();
        gauges.insert("products".to_string(), 12_345);

        // "storage_only": gauge-only index, not loaded — unions into the snapshot.
        gauges.insert("storage_only".to_string(), 4_096);

        // "empty": explicit-zero storage gauge, not loaded — Some(0) must survive.
        gauges.insert("empty".to_string(), 0);

        // "counter_only": counter-backed, not loaded, no gauge — gauges stay None.
        {
            let entry = state
                .usage_counters
                .entry("counter_only".to_string())
                .or_default();
            entry.search_count.store(11, Ordering::Relaxed);
            entry.write_count.store(22, Ordering::Relaxed);
            entry.read_count.store(33, Ordering::Relaxed);
            entry.bytes_in.store(44, Ordering::Relaxed);
            entry.search_results_total.store(55, Ordering::Relaxed);
            entry.documents_indexed_total.store(66, Ordering::Relaxed);
            entry.documents_deleted_total.store(77, Ordering::Relaxed);
        }

        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 5).unwrap();
        run_usage_rollover(
            now,
            &persistence,
            &state.usage_counters,
            &state.manager,
            Some(&gauges),
        )
        .unwrap();

        let snapshot = persistence
            .load_snapshot("2026-07-19")
            .unwrap()
            .expect("completed-day snapshot should load");

        // Union of counter-backed and gauge-only indexes.
        let mut names: Vec<_> = snapshot.indexes.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["counter_only", "empty", "products", "storage_only"]
        );

        let products = &snapshot.indexes["products"];
        assert_eq!(products.documents_count, Some(3));
        assert_eq!(products.storage_bytes, Some(12_345));

        let storage_only = &snapshot.indexes["storage_only"];
        assert_eq!(storage_only.documents_count, None);
        assert_eq!(storage_only.storage_bytes, Some(4_096));

        let empty = &snapshot.indexes["empty"];
        assert_eq!(empty.documents_count, None);
        assert_eq!(empty.storage_bytes, Some(0));

        let counter_only = &snapshot.indexes["counter_only"];
        assert_eq!(counter_only.documents_count, None);
        assert_eq!(counter_only.storage_bytes, None);
        assert_eq!(counter_only.search_operations, 11);

        // The captured gauge source is not mutated by rollover.
        assert_eq!(gauges.len(), 3);
        assert_eq!(*gauges.get("products").unwrap().value(), 12_345);
        assert_eq!(*gauges.get("storage_only").unwrap().value(), 4_096);
        assert_eq!(*gauges.get("empty").unwrap().value(), 0);

        // Only the seven usage counter atomics are reset.
        let entry = state.usage_counters.get("counter_only").unwrap();
        assert_eq!(entry.search_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.write_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.read_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.bytes_in.load(Ordering::Relaxed), 0);
        assert_eq!(entry.search_results_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_indexed_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_deleted_total.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn rollup_window_targets_last_completed_hour() {
        let now_ms = (10 * HOUR_MS) + 123;
        let (start_ms, end_ms) = rollup_window_bounds_ms(now_ms);
        assert_eq!(start_ms, 9 * HOUR_MS);
        assert_eq!(end_ms, 10 * HOUR_MS);
    }

    #[test]
    fn rollup_window_uses_completed_override_window_when_override_is_valid() {
        let _guard = with_env_var("FLAPJACK_ROLLUP_WINDOW_OVERRIDE_MS", "60000");
        let now_ms = (10 * HOUR_MS) + (2 * 60_000) + 12_345;
        let (start_ms, end_ms) = rollup_window_bounds_ms(now_ms);
        assert_eq!(start_ms, (10 * HOUR_MS) + 60_000);
        assert_eq!(end_ms, (10 * HOUR_MS) + (2 * 60_000));
    }

    #[test]
    fn rollup_window_falls_back_to_hour_bounds_when_override_is_invalid() {
        let now_ms = (10 * HOUR_MS) + 123;
        for invalid_override in ["not-a-number", "0", "-60000"] {
            let _guard = with_env_var("FLAPJACK_ROLLUP_WINDOW_OVERRIDE_MS", invalid_override);
            let (start_ms, end_ms) = rollup_window_bounds_ms(now_ms);
            assert_eq!(start_ms, 9 * HOUR_MS);
            assert_eq!(end_ms, 10 * HOUR_MS);
        }
    }
}
