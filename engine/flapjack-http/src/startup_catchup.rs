//! Stub summary for /Users/stuart/parallel_development/flapjack_dev/may31_eve_2_ha_snapshot_flake_verify/flapjack_dev/engine/flapjack-http/src/startup_catchup.rs.
use crate::handlers::internal::apply_ops_to_manager;
use crate::handlers::AppState;
use flapjack::index::oplog::read_committed_seq;
use flapjack::index::snapshot::import_from_bytes;
use flapjack_replication::types::GetOpsResponse;
use std::collections::BTreeSet;
use std::sync::Arc;

/// Legacy helper that triggers delayed startup catch-up in the background.
/// Production bootstrap now uses `run_pre_serve_catchup` before serving.
pub fn spawn_startup_catchup(state: Arc<AppState>) {
    tokio::spawn(async move {
        run_startup_catchup(state).await;
    });
}

/// Legacy delayed startup catch-up path. Public for testing.
pub async fn run_startup_catchup(state: Arc<AppState>) {
    if state.replication_manager.is_none() {
        return; // Standalone mode — nothing to do
    }
    delayed_catchup_from_peers(&state).await;
}

/// Sleep briefly then run best-effort catch-up from peers. Used by the legacy
/// delayed startup path so the server is already accepting requests before
/// catch-up traffic begins.
async fn delayed_catchup_from_peers(state: &AppState) {
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    tracing::info!("[REPL-catchup] Starting startup catch-up from peers");
    run_lenient_catchup_round(state, "REPL-catchup", "delayed startup catch-up skipped").await;
    tracing::info!("[REPL-catchup] Startup catch-up complete");
}

/// Run one non-strict catch-up round and log failures as debug-only skips.
async fn run_lenient_catchup_round(state: &AppState, log_prefix: &str, skip_reason: &str) {
    if let Err(error) = catchup_all_tenants(state, log_prefix, false).await {
        tracing::debug!("[{}] {}: {}", log_prefix, skip_reason, error);
    }
}

pub async fn run_pre_serve_catchup(state: &AppState) -> Result<(), String> {
    let has_peers = state
        .replication_manager
        .as_ref()
        .is_some_and(|manager| manager.peer_count() > 0);
    if !has_peers {
        return Ok(());
    }

    let timeout_secs: u64 = std::env::var("FLAPJACK_STARTUP_CATCHUP_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let strict_bootstrap = startup_catchup_strict_bootstrap_enabled();
    let timeout = tokio::time::Duration::from_secs(timeout_secs);
    let deadline = tokio::time::Instant::now() + timeout;

    tracing::info!(
        "[REPL-catchup] Pre-serve catch-up starting (timeout={}s)",
        timeout_secs
    );
    if !strict_bootstrap {
        tracing::warn!(
            "[REPL-catchup] strict bootstrap disabled via FLAPJACK_STARTUP_CATCHUP_STRICT; node may start before all peers are reachable"
        );
    }

    execute_timed_catchup(state, timeout, timeout_secs, strict_bootstrap).await?;
    wait_for_write_queues(state, deadline, timeout_secs, strict_bootstrap).await
}

async fn execute_timed_catchup(
    state: &AppState,
    timeout: tokio::time::Duration,
    timeout_secs: u64,
    strict_bootstrap: bool,
) -> Result<(), String> {
    let catchup_result = tokio::time::timeout(
        timeout,
        catchup_all_tenants(state, "REPL-catchup", strict_bootstrap),
    )
    .await;

    match catchup_result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => strict_or_skip(
            strict_bootstrap,
            format!("Pre-serve catch-up failed: {}", error),
        ),
        Err(_) => strict_or_skip(
            strict_bootstrap,
            format!(
                "pre-serve catch-up timed out after {}s; refusing to serve stale data",
                timeout_secs
            ),
        ),
    }
}

/// If strict, logs at error level and returns `Err`; otherwise logs at warn
/// level and returns `Ok(())` so the caller can continue.
fn strict_or_skip(strict_bootstrap: bool, error: String) -> Result<(), String> {
    if strict_bootstrap {
        tracing::error!("[REPL-catchup] {}", error);
        Err(error)
    } else {
        tracing::warn!("[REPL-catchup] {} (non-strict, continuing)", error);
        Ok(())
    }
}

/// Wait for all write queues to commit enqueued catch-up ops before serving.
async fn wait_for_write_queues(
    state: &AppState,
    deadline: tokio::time::Instant,
    timeout_secs: u64,
    strict_bootstrap: bool,
) -> Result<(), String> {
    let remaining = remaining_pre_serve_budget(deadline);
    if state.manager.wait_for_pending_tasks(remaining).await {
        tracing::info!("[REPL-catchup] Pre-serve catch-up complete");
        Ok(())
    } else {
        handle_write_queue_timeout(strict_bootstrap, timeout_secs)
    }
}

fn remaining_pre_serve_budget(deadline: tokio::time::Instant) -> tokio::time::Duration {
    deadline
        .checked_duration_since(tokio::time::Instant::now())
        .unwrap_or(tokio::time::Duration::ZERO)
}

fn handle_write_queue_timeout(strict_bootstrap: bool, timeout_secs: u64) -> Result<(), String> {
    let error = format!(
        "write queues did not drain within {}s after pre-serve catch-up",
        timeout_secs
    );
    if strict_bootstrap {
        tracing::error!("[REPL-catchup] {}", error);
        Err(error)
    } else {
        tracing::warn!("[REPL-catchup] {} (non-strict, continuing)", error);
        Ok(())
    }
}

fn startup_catchup_strict_bootstrap_enabled() -> bool {
    parse_strict_bootstrap_override(
        std::env::var("FLAPJACK_STARTUP_CATCHUP_STRICT")
            .ok()
            .as_deref(),
    )
}

fn parse_strict_bootstrap_override(raw_value: Option<&str>) -> bool {
    !matches!(
        raw_value
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("0") | Some("false")
    )
}

/// Run one round of catch-up from peers for all local tenants.
/// This is the core anti-entropy sync logic used by both startup catch-up
/// and the periodic background task (P0).
/// Public for testing.
pub async fn run_periodic_catchup(state: Arc<AppState>) {
    if state.replication_manager.is_none() {
        return;
    }
    run_lenient_catchup_round(&state, "REPL-sync", "periodic catch-up skipped").await;
}

/// Spawn a background task that runs catch-up from peers on a timer.
/// This is the P0 fix for network partition recovery without restart.
/// Configurable via FLAPJACK_SYNC_INTERVAL_SECS (default 60).
pub fn spawn_periodic_sync(state: Arc<AppState>, interval_secs: u64) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
        // If a sync cycle runs longer than the interval, skip missed ticks
        // rather than bursting back-to-back syncs.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await; // skip first immediate tick

        loop {
            interval.tick().await;
            tracing::debug!("[REPL-sync] Periodic sync starting");
            run_periodic_catchup(Arc::clone(&state)).await;
        }
    });
}

/// Core catch-up logic shared by startup and periodic sync.
/// Iterates tenant IDs discovered locally and from peers, compares local oplog
/// sequence against peers, and pulls any missed ops.
async fn catchup_all_tenants(
    state: &AppState,
    log_prefix: &str,
    strict_bootstrap: bool,
) -> Result<(), String> {
    let repl_mgr = match &state.replication_manager {
        Some(r) => Arc::clone(r),
        None => return Ok(()),
    };
    if repl_mgr.peer_count() == 0 {
        return Ok(());
    }

    let mut tenant_ids: BTreeSet<String> =
        match crate::tenant_dirs::visible_tenant_dir_names(&state.manager.base_path) {
            Ok(ids) => ids.into_iter().collect(),
            Err(e) => {
                let error = format!("cannot read data dir for startup catch-up: {e}");
                if strict_bootstrap {
                    return Err(error);
                }
                tracing::warn!("[{}] {}", log_prefix, error);
                return Ok(());
            }
        };
    let peer_tenants = if strict_bootstrap {
        repl_mgr.discover_tenants_from_peers_strict().await?
    } else {
        repl_mgr.discover_tenants_from_peers().await
    };
    tenant_ids.extend(peer_tenants);

    for tenant_id in tenant_ids {
        if let Err(error) = validate_tenant_id(&tenant_id) {
            if strict_bootstrap {
                return Err(format!("[{}] {}", log_prefix, error));
            }
            tracing::warn!("[{}] skipping invalid tenant id: {}", log_prefix, error);
            continue;
        }
        repush_failed_peer_ranges(state, &repl_mgr, &tenant_id, log_prefix, strict_bootstrap)
            .await?;
        catchup_single_tenant(state, &repl_mgr, &tenant_id, log_prefix, strict_bootstrap).await?;
    }

    Ok(())
}

fn retention_gap_detected(local_seq: u64, response: &GetOpsResponse) -> bool {
    if local_seq == 0 {
        return false;
    }

    response
        .oldest_retained_seq
        .is_some_and(|oldest_retained| local_seq < oldest_retained)
}

/// Downloads a snapshot from a replication peer and installs it as the tenant's
/// data directory, used during startup catchup when the local tenant is missing or stale.
async fn restore_tenant_from_snapshot(
    state: &AppState,
    repl_mgr: &Arc<flapjack_replication::manager::ReplicationManager>,
    tenant_id: &str,
    log_prefix: &str,
) -> Result<(), String> {
    let snapshot_bytes = match repl_mgr.download_snapshot_from_peer(tenant_id).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(format!(
                "[{}] failed to download snapshot for tenant '{}': {}",
                log_prefix, tenant_id, error
            ));
        }
    };

    if let Err((step, error)) = install_snapshot_bytes(&state.manager, tenant_id, &snapshot_bytes) {
        return Err(format!(
            "[{}] failed to install snapshot for tenant '{}' at step '{}': {}",
            log_prefix,
            tenant_id,
            step.as_tag(),
            error
        ));
    }

    tracing::info!(
        "[{}] Restored tenant '{}' from peer snapshot ({} bytes)",
        log_prefix,
        tenant_id,
        snapshot_bytes.len()
    );
    Ok(())
}

/// Stable, non-PII tag identifying which internal sub-step of `install_snapshot_bytes`
/// produced a failure. Surfaced as the `sub_step` field on sanitized 500 responses
/// so the failing branch is observable in logs/test output without leaking paths,
/// tenant ids, or raw error strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotInstallStep {
    ValidateTenantId,
    CleanStaging,
    RecoverInterrupted,
    ImportExtract,
    RenameTenantToBackup,
    RenameStagingToTenant,
}

impl SnapshotInstallStep {
    pub(crate) fn as_tag(self) -> &'static str {
        match self {
            SnapshotInstallStep::ValidateTenantId => "validate_tenant_id",
            SnapshotInstallStep::CleanStaging => "clean_staging",
            SnapshotInstallStep::RecoverInterrupted => "recover_interrupted",
            SnapshotInstallStep::ImportExtract => "import_extract",
            SnapshotInstallStep::RenameTenantToBackup => "rename_tenant_to_backup",
            SnapshotInstallStep::RenameStagingToTenant => "rename_staging_to_tenant",
        }
    }
}

/// Atomically installs a compressed snapshot as a tenant's data directory: extracts
/// to a staging path, backs up the existing tenant dir, then renames staging into place.
///
/// Returns the failing `SnapshotInstallStep` tag alongside the underlying error
/// string so callers can surface a stable, non-leaking sub-step tag on the 500
/// response while logging the full error server-side. Stage 1 diagnosis
/// (`docs/research/may31_pm_ha_snapshot_flake_diagnosis.md`) identified the
/// `RenameTenantToBackup` / `RenameStagingToTenant` branches as the
/// timing-dependent producers of the HA snapshot-restore-under-load flake on
/// Linux tmpfs; the other branches fail deterministically on bad data or
/// missing state. The renames at lines 325/336 use a bounded retry loop to
/// absorb transient `EBUSY`/`ENOTEMPTY` from concurrent `unload_tenant` /
/// lingering file handles.
pub(crate) fn install_snapshot_bytes(
    manager: &flapjack::IndexManager,
    tenant_id: &str,
    snapshot_bytes: &[u8],
) -> Result<(), (SnapshotInstallStep, String)> {
    validate_tenant_id(tenant_id)
        .map_err(|error| (SnapshotInstallStep::ValidateTenantId, error))?;
    let tenant_path = manager.base_path.join(tenant_id);
    let staging_path = manager
        .base_path
        .join(format!(".{tenant_id}.snapshot_restore_staging"));
    let backup_path = manager
        .base_path
        .join(format!(".{tenant_id}.snapshot_restore_backup"));

    remove_path_if_exists(&staging_path)
        .map_err(|error| (SnapshotInstallStep::CleanStaging, error))?;
    recover_interrupted_snapshot_restore(&tenant_path, &backup_path)
        .map_err(|error| (SnapshotInstallStep::RecoverInterrupted, error))?;

    if let Err(error) = import_from_bytes(snapshot_bytes, &staging_path) {
        let _ = remove_path_if_exists(&staging_path);
        return Err((
            SnapshotInstallStep::ImportExtract,
            format!("import snapshot bytes into staging failed: {error}"),
        ));
    }

    manager.unload_tenant(tenant_id);
    let tenant_existed = tenant_path.exists();
    if tenant_existed {
        if let Err(error) = rename_with_transient_retry(&tenant_path, &backup_path) {
            let _ = remove_path_if_exists(&staging_path);
            return Err((
                SnapshotInstallStep::RenameTenantToBackup,
                format!(
                    "move existing tenant dir '{}' to backup '{}' failed: {}",
                    tenant_path.display(),
                    backup_path.display(),
                    error
                ),
            ));
        }
    }

    if let Err(error) = rename_with_transient_retry(&staging_path, &tenant_path) {
        let _ = remove_path_if_exists(&staging_path);
        let rollback_error =
            restore_backup_after_failed_activation(tenant_existed, &backup_path, &tenant_path)
                .err();
        return Err((
            SnapshotInstallStep::RenameStagingToTenant,
            match rollback_error {
                Some(rollback_error) => format!(
                    "activate staged snapshot '{}' -> '{}' failed: {}; rollback failed: {}",
                    staging_path.display(),
                    tenant_path.display(),
                    error,
                    rollback_error
                ),
                None => format!(
                    "activate staged snapshot '{}' -> '{}' failed: {}",
                    staging_path.display(),
                    tenant_path.display(),
                    error
                ),
            },
        ));
    }

    if tenant_existed {
        let _ = remove_path_if_exists(&backup_path);
    }
    manager.unload_tenant(tenant_id);
    Ok(())
}

/// Linux ephemeral-tmpfs renames can transiently fail with `EBUSY` or `ENOTEMPTY`
/// when concurrent `unload_tenant` / open file handles haven't fully released the
/// directory yet. Retry briefly (≤200 ms total) before surfacing the IO error.
///
/// Bounded to absorb the race without masking persistent failures: if rename keeps
/// failing past the budget, the underlying error still propagates with its raw
/// `io::Error` so callers see the real reason in logs.
fn rename_with_transient_retry(
    from: &std::path::Path,
    to: &std::path::Path,
) -> std::io::Result<()> {
    const MAX_TOTAL_MS: u64 = 200;
    const SLEEP_MS: u64 = 10;
    let start = std::time::Instant::now();
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if !is_transient_rename_error(&error)
                    || start.elapsed().as_millis() as u64 >= MAX_TOTAL_MS
                {
                    return Err(error);
                }
                std::thread::sleep(std::time::Duration::from_millis(SLEEP_MS));
            }
        }
    }
}

fn is_transient_rename_error(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        // EBUSY = 16 (Linux), ENOTEMPTY = 39 (Linux) / 66 (macOS)
        Some(16) | Some(39) | Some(66)
    ) || matches!(
        error.kind(),
        std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::ResourceBusy
    )
}

fn restore_backup_after_failed_activation(
    tenant_existed: bool,
    backup_path: &std::path::Path,
    tenant_path: &std::path::Path,
) -> Result<(), String> {
    if !tenant_existed {
        return Ok(());
    }

    rename_with_transient_retry(backup_path, tenant_path).map_err(|error| {
        format!(
            "restore previous tenant dir '{}' from backup '{}' failed: {}",
            tenant_path.display(),
            backup_path.display(),
            error
        )
    })
}
fn validate_tenant_id(tenant_id: &str) -> Result<(), String> {
    flapjack::validate_index_name(tenant_id)
        .map_err(|error| format!("invalid tenant id '{}': {}", tenant_id, error))
}
/// Recovers from a crash during snapshot restore: if a backup dir exists but the
/// tenant dir is missing, restores from the backup to avoid data loss.
fn recover_interrupted_snapshot_restore(
    tenant_path: &std::path::Path,
    backup_path: &std::path::Path,
) -> Result<(), String> {
    if !backup_path.exists() {
        return Ok(());
    }

    if tenant_path.exists() {
        return remove_path_if_exists(backup_path);
    }

    std::fs::rename(backup_path, tenant_path).map_err(|error| {
        format!(
            "restore interrupted snapshot backup '{}' -> '{}' failed: {}",
            backup_path.display(),
            tenant_path.display(),
            error
        )
    })
}

fn remove_path_if_exists(path: &std::path::Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }

    if path.is_dir() {
        std::fs::remove_dir_all(path)
            .map_err(|error| format!("remove dir '{}' failed: {}", path.display(), error))
    } else {
        std::fs::remove_file(path)
            .map_err(|error| format!("remove file '{}' failed: {}", path.display(), error))
    }
}

fn read_local_ops_since(
    state: &AppState,
    tenant_id: &str,
    since_seq: u64,
) -> Result<Vec<flapjack::index::oplog::OpLogEntry>, String> {
    let Some(oplog) = state.manager.get_oplog(tenant_id) else {
        return Ok(Vec::new());
    };
    oplog.read_since(since_seq).map_err(|error| {
        format!(
            "failed to read local oplog for tenant '{}' since seq {}: {}",
            tenant_id, since_seq, error
        )
    })
}

/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
/// TODO: Document repush_failed_peer_ranges.
#[allow(clippy::cognitive_complexity)] // Per-peer retry/strict-mode handling is centralized here to keep startup catch-up failure semantics in one place.
async fn repush_failed_peer_ranges(
    state: &AppState,
    repl_mgr: &Arc<flapjack_replication::manager::ReplicationManager>,
    tenant_id: &str,
    log_prefix: &str,
    strict_bootstrap: bool,
) -> Result<(), String> {
    let Some(peer_cursors) = repl_mgr.get_peer_cursors(tenant_id) else {
        return Ok(());
    };

    for peer_cursor in peer_cursors.iter() {
        let peer_id = peer_cursor.key().clone();
        let cursor = peer_cursor.value().clone();
        if cursor.last_delivery_error.is_none() {
            continue;
        }

        let since_seq = cursor.last_acked_seq.unwrap_or(0);
        let pending_ops = match read_local_ops_since(state, tenant_id, since_seq) {
            Ok(ops) => ops,
            Err(error) => {
                if strict_bootstrap {
                    return Err(format!("[{}] {}", log_prefix, error));
                }
                tracing::warn!("[{}] {}", log_prefix, error);
                continue;
            }
        };
        if pending_ops.is_empty() {
            continue;
        }

        tracing::warn!(
            "[{}] re-pushing {} pending ops to failed peer {} for tenant '{}' from seq {}",
            log_prefix,
            pending_ops.len(),
            peer_id,
            tenant_id,
            since_seq
        );
        if let Err(error) = repl_mgr
            .replicate_ops_to_peer(tenant_id, &peer_id, pending_ops)
            .await
        {
            if strict_bootstrap {
                return Err(format!(
                    "[{}] failed to re-push pending ops for tenant '{}' to peer {}: {}",
                    log_prefix, tenant_id, peer_id, error
                ));
            }
            tracing::warn!(
                "[{}] failed to re-push pending ops for tenant '{}' to peer {}: {}",
                log_prefix,
                tenant_id,
                peer_id,
                error
            );
        }
    }

    Ok(())
}

/// Handle a fetch-ops failure: strict mode returns an error to abort catch-up,
/// lenient mode logs and returns `Ok(())` so the caller can skip this tenant.
fn handle_fetch_error(
    error: String,
    log_prefix: &str,
    tenant_id: &str,
    strict_bootstrap: bool,
) -> Result<(), String> {
    if strict_bootstrap {
        Err(format!(
            "[{}] failed to fetch missed ops for tenant '{}': {}",
            log_prefix, tenant_id, error
        ))
    } else {
        tracing::debug!(
            "[{}] Could not reach peer for '{}': {}",
            log_prefix,
            tenant_id,
            error
        );
        Ok(())
    }
}

/// Catches up a single tenant by fetching missed oplog entries from a replication
/// peer, falling back to full snapshot download if the peer's log has been truncated.
async fn catchup_single_tenant(
    state: &AppState,
    repl_mgr: &Arc<flapjack_replication::manager::ReplicationManager>,
    tenant_id: &str,
    log_prefix: &str,
    strict_bootstrap: bool,
) -> Result<(), String> {
    validate_tenant_id(tenant_id).map_err(|error| format!("[{}] {}", log_prefix, error))?;
    let tenant_path = state.manager.base_path.join(tenant_id);
    let local_seq = read_committed_seq(&tenant_path);
    let response = match fetch_missed_ops(repl_mgr, tenant_id, local_seq, strict_bootstrap).await {
        Ok(response) => response,
        Err(error) => {
            return handle_fetch_error(error, log_prefix, tenant_id, strict_bootstrap);
        }
    };

    if retention_gap_detected(local_seq, &response) {
        tracing::warn!(
            "[{}] Tenant '{}' is behind retained peer history (local_seq={}, peer_seq={}, oldest_retained_seq={:?}); restoring full snapshot",
            log_prefix,
            tenant_id,
            local_seq,
            response.current_seq,
            response.oldest_retained_seq
        );
        return restore_tenant_from_snapshot(state, repl_mgr, tenant_id, log_prefix).await;
    }

    if response.ops.is_empty() {
        tracing::debug!("[{}] Tenant '{}' is up-to-date", log_prefix, tenant_id);
        return Ok(());
    }

    tracing::info!(
        "[{}] {} missed ops for tenant '{}' (local_seq={})",
        log_prefix,
        response.ops.len(),
        tenant_id,
        local_seq
    );
    apply_and_log_ops(&state.manager, tenant_id, &response.ops, log_prefix).await
}

async fn fetch_missed_ops(
    repl_mgr: &Arc<flapjack_replication::manager::ReplicationManager>,
    tenant_id: &str,
    local_seq: u64,
    strict_bootstrap: bool,
) -> Result<GetOpsResponse, String> {
    if strict_bootstrap {
        repl_mgr
            .catch_up_from_peer_with_metadata_strict(tenant_id, local_seq)
            .await
    } else {
        repl_mgr
            .catch_up_from_peer_with_metadata(tenant_id, local_seq)
            .await
    }
}

/// Applies a batch of oplog entries to a tenant via the index manager and logs
/// the resulting sequence number or error.
async fn apply_and_log_ops(
    manager: &flapjack::IndexManager,
    tenant_id: &str,
    ops: &[flapjack::index::oplog::OpLogEntry],
    log_prefix: &str,
) -> Result<(), String> {
    match apply_ops_to_manager(manager, tenant_id, ops).await {
        Ok(seq) => {
            tracing::info!(
                "[{}] Applied ops up to seq {} for tenant '{}'",
                log_prefix,
                seq,
                tenant_id
            );
            Ok(())
        }
        Err(error) => Err(format!(
            "[{}] failed to apply ops for '{}': {}",
            log_prefix, tenant_id, error
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{install_snapshot_bytes, parse_strict_bootstrap_override, retention_gap_detected};
    use crate::handlers::AppState;
    use axum::{extract::State, routing::get, routing::post, Json, Router};
    use flapjack::index::snapshot::export_to_bytes;
    use flapjack_replication::config::{NodeConfig, PeerConfig};
    use flapjack_replication::manager::ReplicationManager;
    use flapjack_replication::types::GetOpsResponse;
    use flapjack_replication::types::ReplicateOpsRequest;
    use std::sync::{Arc, Mutex, OnceLock};
    use tempfile::TempDir;
    use tokio::sync::oneshot;

    // Async-aware mutex so the guard can be held across the `.await` points in
    // these tests without tripping `clippy::await_holding_lock`. Serializes
    // process-global env-var access across the catchup tests.
    fn env_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// Non-strict pre-serve catchup must succeed even when peers are unreachable.
    /// Regression: execute_timed_catchup hard-coded strict_bootstrap=true, ignoring
    /// the FLAPJACK_STARTUP_CATCHUP_STRICT=0 env var.
    #[tokio::test]
    async fn pre_serve_catchup_succeeds_with_unreachable_peers_when_not_strict() {
        let _env_guard = env_lock().lock().await;
        // SAFETY: env vars are process-global; this test must not run in parallel
        // with other tests that read these vars.
        unsafe {
            std::env::set_var("FLAPJACK_STARTUP_CATCHUP_STRICT", "0");
            std::env::set_var("FLAPJACK_STARTUP_CATCHUP_TIMEOUT_SECS", "2");
        }

        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build();

        let node_config = flapjack_replication::config::NodeConfig {
            node_id: "test-node".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            peers: vec![flapjack_replication::config::PeerConfig {
                node_id: "unreachable-peer".to_string(),
                addr: "http://127.0.0.1:19999".to_string(),
            }],
        };
        let repl_mgr = flapjack_replication::manager::ReplicationManager::new(node_config, None);

        // Override replication_manager on the built state.
        let state = crate::handlers::AppState {
            replication_manager: Some(repl_mgr),
            ..state
        };

        let result = super::run_pre_serve_catchup(&state).await;

        unsafe {
            std::env::remove_var("FLAPJACK_STARTUP_CATCHUP_STRICT");
            std::env::remove_var("FLAPJACK_STARTUP_CATCHUP_TIMEOUT_SECS");
        }

        assert!(
            result.is_ok(),
            "non-strict pre-serve catchup with unreachable peers should succeed, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn pre_serve_catchup_fails_with_unreachable_peer_when_strict() {
        let _env_guard = env_lock().lock().await;
        // SAFETY: env vars are process-global; this test synchronizes reads/writes
        // via `env_lock` and restores the strict-mode defaults explicitly.
        unsafe {
            std::env::remove_var("FLAPJACK_STARTUP_CATCHUP_STRICT");
            std::env::remove_var("FLAPJACK_STARTUP_CATCHUP_TIMEOUT_SECS");
        }

        async fn spawn_tenant_listing_peer(
            addr: std::net::SocketAddr,
            state: Arc<AppState>,
        ) -> (oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
            let app = Router::new()
                .route(
                    "/internal/tenants",
                    get(crate::handlers::internal::list_tenants),
                )
                .with_state(state);

            let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
            let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
            let handle = tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .unwrap();
            });
            (shutdown_tx, handle)
        }

        let tmp = TempDir::new().unwrap();
        let peer_tmp = TempDir::new().unwrap();
        let peer_state = crate::test_helpers::TestStateBuilder::new(&peer_tmp).build_shared();

        let reserved_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = reserved_listener.local_addr().unwrap();
        drop(reserved_listener);

        let (shutdown_tx, server_handle) =
            spawn_tenant_listing_peer(peer_addr, Arc::clone(&peer_state)).await;

        let mut state = crate::test_helpers::TestStateBuilder::new(&tmp).build();
        state.replication_manager = Some(flapjack_replication::manager::ReplicationManager::new(
            NodeConfig {
                node_id: "test-node".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                peers: vec![
                    PeerConfig {
                        node_id: "reachable-peer".to_string(),
                        addr: format!("http://{}", peer_addr),
                    },
                    PeerConfig {
                        node_id: "unreachable-peer".to_string(),
                        addr: "http://127.0.0.1:1".to_string(),
                    },
                ],
            },
            None,
        ));
        let state = crate::handlers::AppState { ..state };

        let result = super::run_pre_serve_catchup(&state).await;
        let _ = shutdown_tx.send(());
        let _ = server_handle.await;

        assert!(
            result.as_ref().err().is_some_and(
                |error| error.contains("peer unreachable-peer tenant discovery failed")
            ),
            "strict pre-serve catchup must fail on partial peer coverage, got: {:?}",
            result
        );
    }

    #[test]
    fn strict_bootstrap_override_defaults_true() {
        assert!(parse_strict_bootstrap_override(None));
    }

    #[test]
    fn strict_bootstrap_override_accepts_false_values() {
        assert!(!parse_strict_bootstrap_override(Some("0")));
        assert!(!parse_strict_bootstrap_override(Some("false")));
        assert!(!parse_strict_bootstrap_override(Some("FALSE")));
    }

    #[test]
    fn strict_bootstrap_override_keeps_true_for_other_values() {
        assert!(parse_strict_bootstrap_override(Some("1")));
        assert!(parse_strict_bootstrap_override(Some("true")));
        assert!(parse_strict_bootstrap_override(Some("unexpected")));
    }

    #[tokio::test(start_paused = true)]
    async fn remaining_pre_serve_budget_shrinks_with_elapsed_time() {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);

        tokio::time::advance(tokio::time::Duration::from_secs(11)).await;
        assert_eq!(
            super::remaining_pre_serve_budget(deadline),
            tokio::time::Duration::from_secs(19)
        );

        tokio::time::advance(tokio::time::Duration::from_secs(19)).await;
        assert_eq!(
            super::remaining_pre_serve_budget(deadline),
            tokio::time::Duration::ZERO
        );
    }

    #[test]
    fn write_queue_timeout_is_error_in_strict_mode() {
        let result = super::handle_write_queue_timeout(true, 7);
        assert!(
            result
                .as_ref()
                .err()
                .is_some_and(|error| error.contains("write queues did not drain within 7s")),
            "strict mode must fail on write-queue timeout, got: {:?}",
            result
        );
    }

    #[test]
    fn write_queue_timeout_is_ok_in_non_strict_mode() {
        let result = super::handle_write_queue_timeout(false, 7);
        assert!(
            result.is_ok(),
            "non-strict mode should continue on write-queue timeout, got: {:?}",
            result
        );
    }

    #[test]
    fn retention_gap_true_when_local_seq_before_oldest_retained() {
        let response = GetOpsResponse {
            tenant_id: "t1".to_string(),
            ops: Vec::new(),
            current_seq: 250,
            oldest_retained_seq: Some(200),
            node_current_seqs: std::collections::BTreeMap::new(),
        };
        assert!(retention_gap_detected(150, &response));
    }

    #[test]
    fn retention_gap_false_for_brand_new_replica() {
        let response = GetOpsResponse {
            tenant_id: "t1".to_string(),
            ops: Vec::new(),
            current_seq: 250,
            oldest_retained_seq: Some(200),
            node_current_seqs: std::collections::BTreeMap::new(),
        };
        assert!(!retention_gap_detected(0, &response));
    }

    #[test]
    fn retention_gap_false_when_metadata_missing() {
        let response = GetOpsResponse {
            tenant_id: "t1".to_string(),
            ops: Vec::new(),
            current_seq: 25,
            oldest_retained_seq: None,
            node_current_seqs: std::collections::BTreeMap::new(),
        };
        assert!(!retention_gap_detected(10, &response));
    }

    #[test]
    fn retention_gap_false_when_local_seq_equals_oldest_retained() {
        let response = GetOpsResponse {
            tenant_id: "t1".to_string(),
            ops: Vec::new(),
            current_seq: 200,
            oldest_retained_seq: Some(200),
            node_current_seqs: std::collections::BTreeMap::new(),
        };
        // local_seq == oldest_retained means we are exactly caught up
        // to the oldest retained entry — not a gap.
        assert!(!retention_gap_detected(200, &response));
    }

    #[test]
    fn retention_gap_false_when_local_seq_above_oldest_retained() {
        let response = GetOpsResponse {
            tenant_id: "t1".to_string(),
            ops: Vec::new(),
            current_seq: 300,
            oldest_retained_seq: Some(200),
            node_current_seqs: std::collections::BTreeMap::new(),
        };
        // local_seq > oldest_retained means we are ahead of the
        // oldest retained entry — definitely not a gap.
        assert!(!retention_gap_detected(250, &response));
    }
    #[test]
    fn retention_gap_true_even_when_ops_present() {
        let dummy_op = flapjack::index::oplog::OpLogEntry {
            seq: 201,
            tenant_id: "t1".to_string(),
            node_id: "n1".to_string(),
            op_type: "upsert".to_string(),
            payload: serde_json::json!({"x": 1}),
            timestamp_ms: 0,
        };
        let response = GetOpsResponse {
            tenant_id: "t1".to_string(),
            ops: vec![dummy_op],
            current_seq: 300,
            oldest_retained_seq: Some(200),
            node_current_seqs: std::collections::BTreeMap::new(),
        };
        // Even when partial ops are returned (from oldest_retained
        // onwards), the gap is real — ops 151-199 are missing and a
        // full snapshot restore is needed.
        assert!(retention_gap_detected(150, &response));
    }
    #[tokio::test]
    async fn install_snapshot_bytes_keeps_existing_tenant_on_invalid_snapshot() {
        let tmp = TempDir::new().unwrap();
        let manager = flapjack::IndexManager::new(tmp.path());
        let tenant_id = "restore_target";
        let tenant_path = manager.base_path.join(tenant_id);
        std::fs::create_dir_all(&tenant_path).unwrap();
        let marker_path = tenant_path.join("marker.txt");
        std::fs::write(&marker_path, "keep-me").unwrap();

        let result = install_snapshot_bytes(&manager, tenant_id, b"not-a-valid-snapshot");
        assert!(result.is_err(), "invalid snapshot import should fail");
        assert!(
            marker_path.exists(),
            "existing tenant data should remain if snapshot import fails"
        );
    }
    #[tokio::test]
    async fn install_snapshot_bytes_replaces_existing_tenant_on_valid_snapshot() {
        let tmp = TempDir::new().unwrap();
        let manager = flapjack::IndexManager::new(tmp.path());
        let tenant_id = "restore_target";
        let tenant_path = manager.base_path.join(tenant_id);
        std::fs::create_dir_all(&tenant_path).unwrap();
        let old_marker = tenant_path.join("old.txt");
        std::fs::write(&old_marker, "old").unwrap();

        let snapshot_src = TempDir::new().unwrap();
        let new_marker_name = "new.txt";
        std::fs::write(snapshot_src.path().join(new_marker_name), "new").unwrap();
        let snapshot_bytes = export_to_bytes(snapshot_src.path()).unwrap();

        let result = install_snapshot_bytes(&manager, tenant_id, &snapshot_bytes);
        assert!(result.is_ok(), "valid snapshot install should succeed");
        assert!(
            !old_marker.exists(),
            "old tenant data should be replaced after successful snapshot install"
        );
        assert!(
            tenant_path.join(new_marker_name).exists(),
            "restored tenant should contain snapshot content"
        );
    }
    #[tokio::test]
    async fn install_snapshot_bytes_restores_backup_before_retrying_failed_snapshot() {
        let tmp = TempDir::new().unwrap();
        let manager = flapjack::IndexManager::new(tmp.path());
        let tenant_id = "restore_target";
        let tenant_path = manager.base_path.join(tenant_id);
        let backup_path = manager
            .base_path
            .join(format!(".{tenant_id}.snapshot_restore_backup"));
        std::fs::create_dir_all(&backup_path).unwrap();
        let marker_path = backup_path.join("marker.txt");
        std::fs::write(&marker_path, "keep-me").unwrap();

        let result = install_snapshot_bytes(&manager, tenant_id, b"not-a-valid-snapshot");
        assert!(result.is_err(), "invalid snapshot import should fail");
        assert!(
            tenant_path.join("marker.txt").exists(),
            "retry should restore the interrupted backup before attempting a new snapshot install"
        );
        assert!(
            !backup_path.exists(),
            "restored backup should be moved back to the active tenant path"
        );
    }

    #[test]
    fn restore_backup_after_failed_activation_surfaces_rollback_errors() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let backup_path = tmp.path().join(".tenant.snapshot_restore_backup");
        std::fs::create_dir_all(&tenant_path).unwrap();
        std::fs::write(tenant_path.join("occupied.txt"), "still here").unwrap();
        std::fs::create_dir_all(&backup_path).unwrap();
        std::fs::write(backup_path.join("backup.txt"), "restore me").unwrap();

        let error = super::restore_backup_after_failed_activation(true, &backup_path, &tenant_path)
            .expect_err("existing tenant path should make rollback restore fail");

        assert!(
            error.contains("restore previous tenant dir"),
            "rollback failure should identify the restore step, got: {error}"
        );
        assert!(
            error.contains("failed"),
            "rollback failure should preserve the underlying io error, got: {error}"
        );
    }

    #[tokio::test]
    async fn install_snapshot_bytes_rejects_path_traversal_tenant_id() {
        let tmp = TempDir::new().unwrap();
        let manager = flapjack::IndexManager::new(tmp.path());
        let victim = TempDir::new().unwrap();
        let victim_name = victim
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let marker = victim.path().join("marker.txt");
        std::fs::write(&marker, "keep-me").unwrap();

        let snapshot_src = TempDir::new().unwrap();
        std::fs::write(snapshot_src.path().join("new.txt"), "new").unwrap();
        let snapshot_bytes = export_to_bytes(snapshot_src.path()).unwrap();

        let malicious_tenant_id = format!("../{}", victim_name);
        let result = install_snapshot_bytes(&manager, &malicious_tenant_id, &snapshot_bytes);
        assert!(result.is_err(), "path traversal tenant id must be rejected");
        let (step, error_msg) = result.as_ref().expect_err("error tuple expected").clone();
        assert_eq!(
            step,
            super::SnapshotInstallStep::ValidateTenantId,
            "path traversal must fail at the validate_tenant_id step",
        );
        assert!(
            error_msg.contains("invalid tenant id"),
            "error should identify invalid tenant id, got: {:?}",
            error_msg
        );
        assert!(
            marker.exists(),
            "rejecting traversal tenant id must not modify sibling paths"
        );
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "keep-me",
            "victim marker content must remain unchanged"
        );
    }

    /// Periodic catch-up must proactively re-push only the missing seq range for peers
    /// left in `PeerCursor::Failed` state, before running the pull round.
    #[tokio::test]
    async fn periodic_catchup_repushes_failed_peer_missing_range_before_pull_round() {
        async fn spawn_internal_server_on(
            addr: std::net::SocketAddr,
            state: Arc<AppState>,
            captured_batches: Arc<Mutex<Vec<Vec<u64>>>>,
        ) -> (oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
            let app = Router::new()
                .route(
                    "/internal/replicate",
                    post(
                        move |State(state): State<Arc<AppState>>,
                              Json(req): Json<ReplicateOpsRequest>| {
                            let captured = Arc::clone(&captured_batches);
                            async move {
                                captured
                                    .lock()
                                    .unwrap()
                                    .push(req.ops.iter().map(|entry| entry.seq).collect());
                                crate::handlers::internal::replicate_ops(State(state), Json(req))
                                    .await
                            }
                        },
                    ),
                )
                .route("/internal/ops", get(crate::handlers::internal::get_ops))
                .route(
                    "/internal/tenants",
                    get(crate::handlers::internal::list_tenants),
                )
                .with_state(state);

            let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
            let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
            let handle = tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .unwrap();
            });
            (shutdown_tx, handle)
        }

        async fn wait_for_cursor<F>(manager: &ReplicationManager, tenant_id: &str, check: F) -> bool
        where
            F: Fn(&flapjack_replication::manager::PeerCursor) -> bool,
        {
            for _ in 0..300 {
                if manager
                    .get_peer_cursors(tenant_id)
                    .as_ref()
                    .and_then(|tenant| tenant.get("peer-b").map(|entry| entry.clone()))
                    .is_some_and(|cursor| check(&cursor))
                {
                    return true;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            }
            false
        }

        let tmp_local = TempDir::new().unwrap();
        let tmp_peer = TempDir::new().unwrap();
        let local_state = crate::test_helpers::TestStateBuilder::new(&tmp_local).build();
        let peer_state = crate::test_helpers::TestStateBuilder::new(&tmp_peer).build_shared();
        let peer_capture = Arc::new(Mutex::new(Vec::<Vec<u64>>::new()));

        local_state.manager.create_tenant("tenant-red").unwrap();
        let local_oplog = local_state
            .manager
            .get_or_create_oplog("tenant-red")
            .unwrap();
        local_oplog
            .append(
                "upsert",
                serde_json::json!({"objectID": "doc-1", "body": {"_id": "doc-1", "title": "Alpha"}}),
            )
            .unwrap();
        local_oplog
            .append(
                "upsert",
                serde_json::json!({"objectID": "doc-2", "body": {"_id": "doc-2", "title": "Beta"}}),
            )
            .unwrap();
        let local_ops = local_oplog.read_since(0).unwrap();
        assert_eq!(local_ops.len(), 2);

        let reserved_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = reserved_listener.local_addr().unwrap();
        drop(reserved_listener);

        let repl_mgr = ReplicationManager::new(
            NodeConfig {
                node_id: "node-a".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                peers: vec![PeerConfig {
                    node_id: "peer-b".to_string(),
                    addr: format!("http://{}", peer_addr),
                }],
            },
            None,
        );

        let local_state = Arc::new(AppState {
            replication_manager: Some(Arc::clone(&repl_mgr)),
            ..local_state
        });

        let (shutdown_tx, server_handle) = spawn_internal_server_on(
            peer_addr,
            Arc::clone(&peer_state),
            Arc::clone(&peer_capture),
        )
        .await;
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        repl_mgr
            .replicate_ops("tenant-red", vec![local_ops[0].clone()])
            .await;
        assert!(
            wait_for_cursor(&repl_mgr, "tenant-red", |cursor| {
                cursor.last_acked_seq == Some(1) && cursor.last_delivery_error.is_none()
            })
            .await,
            "initial push should ack seq 1"
        );

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
        peer_capture.lock().unwrap().clear();

        repl_mgr
            .replicate_ops("tenant-red", vec![local_ops[1].clone()])
            .await;
        assert!(
            wait_for_cursor(&repl_mgr, "tenant-red", |cursor| {
                cursor
                    .last_delivery_error
                    .as_ref()
                    .is_some_and(|error| !error.is_empty())
            })
            .await,
            "second push should move cursor to failed state"
        );

        let (shutdown_tx, server_handle) = spawn_internal_server_on(
            peer_addr,
            Arc::clone(&peer_state),
            Arc::clone(&peer_capture),
        )
        .await;
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        super::run_periodic_catchup(Arc::clone(&local_state)).await;

        for _ in 0..200 {
            if peer_state
                .manager
                .search("tenant-red", "Beta", None, None, 10)
                .map(|result| result.total == 1)
                .unwrap_or(false)
            {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }

        let replayed = peer_capture.lock().unwrap().clone();
        let last_batch = replayed.last().cloned().unwrap_or_default();
        assert_eq!(
            last_batch,
            vec![2],
            "periodic proactive re-push must replay only the missing range before pull catch-up"
        );

        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    }
}
