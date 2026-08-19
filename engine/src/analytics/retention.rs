//! Stub summary for retention.rs.
use super::manifest::RollupManifest;
use std::path::Path;

const PARTITION_PREFIX: &str = "date=";
const PARTITION_DATE_FORMAT: &str = "%Y-%m-%d";
const RETENTION_INTERVAL_SECONDS: u64 = 86_400;

/// Delete Parquet partition directories older than the configured retention period.
///
/// Walks the analytics directory looking for `date=YYYY-MM-DD/` directories
/// and removes any that are older than `retention_days`.
pub fn cleanup_old_partitions(analytics_dir: &Path, retention_days: u32) -> Result<usize, String> {
    cleanup_old_partitions_at(analytics_dir, retention_days, chrono::Utc::now())
}

/// TODO: Document cleanup_old_partitions_at.
pub(crate) fn cleanup_old_partitions_at(
    analytics_dir: &Path,
    retention_days: u32,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<usize, String> {
    if retention_days == 0 {
        return Ok(0);
    }

    if !analytics_dir.exists() {
        return Ok(0);
    }

    let cutoff = cutoff_date(now, retention_days);
    let mut removed = 0;

    for index_dir in read_root_subdirectories(analytics_dir)? {
        let manifest_path = index_dir.join("rollups").join("manifest.json");
        let manifest = if manifest_path.exists() {
            RollupManifest::load(&manifest_path).ok()
        } else {
            None
        };
        for event_type_dir in read_child_subdirectories(&index_dir) {
            for partition_dir in read_child_subdirectories(&event_type_dir) {
                if remove_partition_if_expired(&partition_dir, cutoff, manifest.as_ref()) {
                    removed += 1;
                }
            }
        }
    }

    Ok(removed)
}

fn cutoff_date(now: chrono::DateTime<chrono::Utc>, retention_days: u32) -> chrono::NaiveDate {
    now.date_naive() - chrono::Duration::days(retention_days as i64)
}

fn read_root_subdirectories(path: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let entries = std::fs::read_dir(path).map_err(|e| format!("read_dir error: {}", e))?;
    Ok(filter_directory_entries(entries))
}

fn read_child_subdirectories(path: &Path) -> Vec<std::path::PathBuf> {
    match std::fs::read_dir(path) {
        Ok(entries) => filter_directory_entries(entries),
        Err(_) => Vec::new(),
    }
}

fn filter_directory_entries(entries: std::fs::ReadDir) -> Vec<std::path::PathBuf> {
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

fn partition_date_from_name(name: &str) -> Option<chrono::NaiveDate> {
    // Only `date=YYYY-MM-DD` partition directory names participate in retention cleanup.
    let date_str = name.strip_prefix(PARTITION_PREFIX)?;
    chrono::NaiveDate::parse_from_str(date_str, PARTITION_DATE_FORMAT).ok()
}

/// Delete a date-partitioned directory if its date falls before the retention cutoff.
/// Returns true on successful removal; logs a warning and returns false on failure.
fn remove_partition_if_expired(
    partition_dir: &Path,
    cutoff: chrono::NaiveDate,
    manifest: Option<&RollupManifest>,
) -> bool {
    let partition_name = partition_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let Some(partition_date) = partition_date_from_name(partition_name) else {
        return false;
    };
    // Retention keeps the cutoff day and newer data; delete only when `partition_date < cutoff`.
    if partition_date >= cutoff {
        return false;
    }

    if let Some(manifest) = manifest {
        let date_str = partition_date.format(PARTITION_DATE_FORMAT).to_string();
        let has_certified_rollup = manifest.has_certified_coverage(&date_str, "1hour")
            || manifest.has_certified_coverage(&date_str, "1day");
        if !has_certified_rollup {
            tracing::info!(
                "[analytics] Retaining raw partition {}: no certified rollup coverage",
                partition_dir.display()
            );
            return false;
        }
    }

    match std::fs::remove_dir_all(partition_dir) {
        Ok(()) => {
            tracing::info!(
                "[analytics] Removed old partition: {}",
                partition_dir.display()
            );
            true
        }
        Err(error) => {
            tracing::warn!(
                "[analytics] Failed to remove old partition {}: {}",
                partition_dir.display(),
                error
            );
            false
        }
    }
}

fn log_cleanup_result(phase: &str, cleanup_result: Result<usize, String>) {
    match cleanup_result {
        Ok(removed_partitions) if removed_partitions > 0 => {
            tracing::info!(
                "[analytics] {} cleanup: removed {} old partitions",
                phase,
                removed_partitions
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!("[analytics] {} cleanup error: {}", phase, error),
    }
}

fn run_cleanup_phase(analytics_dir: &Path, retention_days: u32, phase: &str) {
    if retention_days == 0 {
        return;
    }

    let cleanup_result = cleanup_old_partitions(analytics_dir, retention_days);
    log_cleanup_result(phase, cleanup_result);
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use std::{fs, path::Path, path::PathBuf};

    fn create_partition_dir(
        base: &Path,
        index: &str,
        event_type: &str,
        partition_name: &str,
        marker_file_name: Option<&str>,
    ) -> PathBuf {
        let partition_dir = base.join(index).join(event_type).join(partition_name);
        fs::create_dir_all(&partition_dir).unwrap();
        if let Some(file_name) = marker_file_name {
            fs::write(partition_dir.join(file_name), b"marker").unwrap();
        }
        partition_dir
    }

    fn create_event_file(base: &Path, index: &str, event_type: &str, file_name: &str) -> PathBuf {
        let event_dir = base.join(index).join(event_type);
        fs::create_dir_all(&event_dir).unwrap();
        let file_path = event_dir.join(file_name);
        fs::write(&file_path, b"event-file").unwrap();
        file_path
    }

    fn date(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn fixed_now(y: i32, m: u32, d: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_naive_utc_and_offset(
            date(y, m, d).and_hms_opt(12, 0, 0).unwrap(),
            chrono::Utc,
        )
    }

    #[test]
    fn partition_date_from_name_parses_valid_partition_name() {
        assert_eq!(
            partition_date_from_name("date=2020-01-01"),
            Some(chrono::NaiveDate::from_ymd_opt(2020, 1, 1).unwrap())
        );
    }

    #[test]
    fn partition_date_from_name_rejects_non_partition_names() {
        assert_eq!(partition_date_from_name("2020-01-01"), None);
        assert_eq!(partition_date_from_name("date=not-a-date"), None);
    }

    #[test]
    fn cleanup_nonexistent_dir_returns_zero() {
        let temp_dir = tempfile::tempdir().unwrap();
        let nonexistent = temp_dir.path().join("analytics_root_missing");
        assert_eq!(cleanup_old_partitions(&nonexistent, 30).unwrap(), 0);
        assert_eq!(cleanup_old_partitions(&nonexistent, 0).unwrap(), 0);
        assert!(!nonexistent.exists());
    }

    #[test]
    fn remove_partition_if_expired_removes_partition_before_cutoff() {
        let dir = tempfile::tempdir().unwrap();
        let partition = create_partition_dir(
            dir.path(),
            "myindex",
            "searches",
            "date=2024-03-31",
            Some("data.parquet"),
        );

        let removed = remove_partition_if_expired(&partition, date(2024, 4, 1), None);

        assert!(removed);
        assert!(!partition.exists());
    }

    #[test]
    fn remove_partition_if_expired_keeps_cutoff_day_partition() {
        let dir = tempfile::tempdir().unwrap();
        let partition = create_partition_dir(
            dir.path(),
            "myindex",
            "searches",
            "date=2024-04-01",
            Some("data.parquet"),
        );

        let removed = remove_partition_if_expired(&partition, date(2024, 4, 1), None);

        assert!(!removed);
        assert!(partition.exists());
    }

    #[test]
    fn remove_partition_if_expired_keeps_partition_after_cutoff() {
        let dir = tempfile::tempdir().unwrap();
        let partition = create_partition_dir(
            dir.path(),
            "myindex",
            "searches",
            "date=2024-04-02",
            Some("data.parquet"),
        );

        let removed = remove_partition_if_expired(&partition, date(2024, 4, 1), None);

        assert!(!removed);
        assert!(partition.exists());
    }

    /// TODO: Document remove_partition_if_expired_skips_malformed_or_non_partition_dirs.
    #[test]
    fn remove_partition_if_expired_skips_malformed_or_non_partition_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let malformed_text = create_partition_dir(
            dir.path(),
            "myindex",
            "searches",
            "date=not-a-date",
            Some("data.parquet"),
        );
        let malformed_calendar = create_partition_dir(
            dir.path(),
            "myindex",
            "searches",
            "date=2024-13-45",
            Some("data.parquet"),
        );
        let non_partition = create_partition_dir(
            dir.path(),
            "myindex",
            "searches",
            "not_a_partition",
            Some("data.parquet"),
        );
        let cutoff = date(2024, 4, 1);

        assert!(!remove_partition_if_expired(&malformed_text, cutoff, None));
        assert!(!remove_partition_if_expired(
            &malformed_calendar,
            cutoff,
            None
        ));
        assert!(!remove_partition_if_expired(&non_partition, cutoff, None));
        assert!(malformed_text.exists());
        assert!(malformed_calendar.exists());
        assert!(non_partition.exists());
    }

    /// TODO: Document cleanup_removes_old_partitions.
    #[test]
    fn cleanup_removes_old_partitions() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        // Create: base/myindex/searches/date=2020-01-01/  (very old)
        let old_part = create_partition_dir(
            base,
            "myindex",
            "searches",
            "date=2020-01-01",
            Some("data.parquet"),
        );

        let removed = cleanup_old_partitions(base, 30).unwrap();
        assert_eq!(removed, 1);
        assert!(!old_part.exists());
    }

    /// TODO: Document cleanup_keeps_recent_partitions.
    #[test]
    fn cleanup_keeps_recent_partitions() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let today = "2024-04-10".to_string();
        let recent_part = create_partition_dir(
            base,
            "myindex",
            "searches",
            &format!("date={}", today),
            Some("data.parquet"),
        );

        let removed = cleanup_old_partitions_at(base, 30, fixed_now(2024, 4, 10)).unwrap();
        assert_eq!(removed, 0);
        assert!(recent_part.exists());
    }

    #[test]
    fn cleanup_skips_non_date_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let non_date = create_partition_dir(base, "myindex", "searches", "not_a_date_dir", None);

        let removed = cleanup_old_partitions(base, 30).unwrap();
        assert_eq!(removed, 0);
        assert!(non_date.exists());
    }

    #[test]
    fn cleanup_handles_multiple_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let old1 = create_partition_dir(base, "idx_a", "searches", "date=2020-01-01", None);
        let old2 = create_partition_dir(base, "idx_b", "events", "date=2020-06-15", None);

        let removed = cleanup_old_partitions(base, 30).unwrap();
        assert_eq!(removed, 2);
        assert!(!old1.exists());
        assert!(!old2.exists());
    }

    /// TODO: Document cleanup_traversal_removes_only_old_partitions_and_keeps_files.
    #[test]
    fn cleanup_traversal_removes_only_old_partitions_and_keeps_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let now = fixed_now(2024, 4, 10);

        let old_a = create_partition_dir(
            base,
            "idx_a",
            "searches",
            "date=2024-03-31",
            Some("a.parquet"),
        );
        let old_b = create_partition_dir(
            base,
            "idx_b",
            "events",
            "date=2024-03-01",
            Some("b.parquet"),
        );
        let cutoff_day = create_partition_dir(
            base,
            "idx_c",
            "events",
            "date=2024-04-01",
            Some("cutoff.parquet"),
        );
        let recent_a = create_partition_dir(
            base,
            "idx_a",
            "searches",
            "date=2024-04-10",
            Some("recent-a.parquet"),
        );
        let recent_b = create_partition_dir(
            base,
            "idx_b",
            "events",
            "date=2024-04-02",
            Some("recent-b.parquet"),
        );
        let non_partition_file_a = create_event_file(base, "idx_a", "searches", "notes.txt");
        let non_partition_file_b = create_event_file(base, "idx_b", "events", "meta.json");

        let removed = cleanup_old_partitions_at(base, 9, now).unwrap();

        assert_eq!(removed, 2);
        assert!(!old_a.exists());
        assert!(!old_b.exists());
        assert!(cutoff_day.exists());
        assert!(recent_a.exists());
        assert!(recent_b.exists());
        assert!(non_partition_file_a.exists());
        assert!(non_partition_file_b.exists());
    }

    /// TODO: Document cleanup_with_zero_retention_days_removes_nothing.
    #[test]
    fn cleanup_with_zero_retention_days_removes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let old_partition = create_partition_dir(
            base,
            "idx_a",
            "searches",
            "date=2010-01-01",
            Some("old.parquet"),
        );
        let recent_partition = create_partition_dir(
            base,
            "idx_a",
            "searches",
            "date=2024-04-10",
            Some("new.parquet"),
        );

        let removed = cleanup_old_partitions(base, 0).unwrap();

        assert_eq!(removed, 0);
        assert!(old_partition.exists());
        assert!(recent_partition.exists());
    }
}

/// Run retention cleanup as a background task (daily).
pub async fn run_retention_loop(analytics_dir: std::path::PathBuf, retention_days: u32) {
    if retention_days == 0 {
        tracing::info!("[analytics] Retention cleanup disabled (retention_days=0)");
        return;
    }

    run_cleanup_phase(&analytics_dir, retention_days, "Startup");

    // Then every 24 hours
    let mut interval =
        tokio::time::interval(tokio::time::Duration::from_secs(RETENTION_INTERVAL_SECONDS));
    interval.tick().await; // skip first immediate tick
    loop {
        interval.tick().await;
        run_cleanup_phase(&analytics_dir, retention_days, "Retention");
    }
}
