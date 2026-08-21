//! Stub summary for retention.rs.
use super::{
    config::AnalyticsConfig,
    manifest::{manifest_artifact_path, RollupManifest},
    schema::rollup_schema_version_u32,
};
use std::path::{Path, PathBuf};

const PARTITION_PREFIX: &str = "date=";
const PARTITION_DATE_FORMAT: &str = "%Y-%m-%d";
const RETENTION_INTERVAL_SECONDS: u64 = 86_400;

/// Delete canonical analytics partitions older than the configured retention period.
///
/// Insight-event partitions expire by age. Raw-search partitions expire only
/// after the rollup manifest certifies hourly or daily coverage for that date;
/// missing, unreadable, or symlinked certification fails closed. Traversal does
/// not follow symlinked analytics roots or descendants.
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

    match std::fs::symlink_metadata(analytics_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "refusing symlinked analytics root {}",
                analytics_dir.display()
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(format!(
                "analytics root is not a directory: {}",
                analytics_dir.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(format!(
                "failed to stat analytics root {}: {error}",
                analytics_dir.display()
            ));
        }
    }

    let cutoff = cutoff_date(now, retention_days);
    let mut removed = 0;

    for index_dir in read_root_subdirectories(analytics_dir)? {
        let Some(index_name) = index_dir
            .file_name()
            .and_then(AnalyticsConfig::value_from_path_component)
        else {
            tracing::warn!(
                "[analytics] Skipping retention for undecodable index directory {}",
                index_dir.display()
            );
            continue;
        };
        let paths = AnalyticsConfig::target_artifact_paths_in(analytics_dir, &index_name);
        let manifest = match load_search_rollup_manifest(
            &paths.rollup_manifest_path,
            &paths.rollups_dir,
            &index_name,
        ) {
            Ok(manifest) => Some(manifest),
            Err(error) => {
                tracing::warn!(
                    "[analytics] Retaining raw search partitions for {}: cannot read rollup manifest: {}",
                    index_name,
                    error
                );
                None
            }
        };

        for partition_dir in read_child_subdirectories(&paths.searches_dir)? {
            if remove_partition_if_expired(&partition_dir, cutoff, manifest.as_ref(), true) {
                removed += 1;
            }
        }
        for partition_dir in read_child_subdirectories(&paths.events_dir)? {
            if remove_partition_if_expired(&partition_dir, cutoff, None, false) {
                removed += 1;
            }
        }
    }

    Ok(removed)
}

struct SearchRollupCertification {
    manifest: RollupManifest,
    rollups_dir: PathBuf,
}

impl SearchRollupCertification {
    fn has_certified_artifacts(&self, date: &str) -> Result<bool, String> {
        for tier in ["1day", "1hour"] {
            if !self.manifest.has_certified_coverage(date, tier) {
                continue;
            }
            let tier_dir = self.rollups_dir.join(tier);
            validate_canonical_directory(&tier_dir, "certified rollup tier")?;
            let date_state = self
                .manifest
                .tiers
                .get(tier)
                .and_then(|state| state.dates.get(date))
                .ok_or_else(|| format!("certified {tier} state is missing for {date}"))?;
            for window in &date_state.windows {
                let artifact = manifest_artifact_path(&tier_dir, &window.file)?;
                let metadata = std::fs::symlink_metadata(&artifact).map_err(|error| {
                    format!(
                        "certified rollup artifact is missing: {}: {error}",
                        artifact.display()
                    )
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(format!(
                        "certified rollup artifact is not a regular file: {}",
                        artifact.display()
                    ));
                }
            }
            return Ok(true);
        }
        Ok(false)
    }
}

fn load_search_rollup_manifest(
    path: &Path,
    rollups_dir: &Path,
    expected_index: &str,
) -> Result<SearchRollupCertification, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("manifest has no parent: {}", path.display()))?;
    validate_canonical_directory(parent, "rollups")?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("failed to stat {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("refusing symlinked manifest {}", path.display()));
    }
    if !metadata.is_file() {
        return Err(format!("manifest is not a file: {}", path.display()));
    }
    let manifest = RollupManifest::load(path).map_err(|error| error.to_string())?;
    if manifest.index != expected_index {
        return Err(format!(
            "rollup manifest index mismatch: expected '{expected_index}', found '{}'",
            manifest.index
        ));
    }
    let expected_schema = rollup_schema_version_u32();
    if manifest.schema_version != expected_schema {
        return Err(format!(
            "rollup manifest schema mismatch: expected {expected_schema}, found {}",
            manifest.schema_version
        ));
    }
    Ok(SearchRollupCertification {
        manifest,
        rollups_dir: rollups_dir.to_path_buf(),
    })
}

fn cutoff_date(now: chrono::DateTime<chrono::Utc>, retention_days: u32) -> chrono::NaiveDate {
    now.date_naive() - chrono::Duration::days(retention_days as i64)
}

fn read_root_subdirectories(path: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let entries = std::fs::read_dir(path).map_err(|e| format!("read_dir error: {}", e))?;
    Ok(filter_directory_entries(entries))
}

fn read_child_subdirectories(path: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("failed to stat {}: {error}", path.display())),
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "refusing symlinked analytics artifact directory {}",
            path.display()
        )),
        Ok(metadata) if !metadata.is_dir() => Err(format!(
            "analytics artifact path is not a directory: {}",
            path.display()
        )),
        Ok(_) => std::fs::read_dir(path)
            .map(filter_directory_entries)
            .map_err(|error| format!("failed to read {}: {error}", path.display())),
    }
}

fn validate_canonical_directory(path: &Path, description: &str) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("failed to stat {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "refusing symlinked {description} directory {}",
            path.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "{description} path is not a directory: {}",
            path.display()
        ));
    }
    Ok(())
}

fn filter_directory_entries(entries: std::fs::ReadDir) -> Vec<std::path::PathBuf> {
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .map(|entry| entry.path())
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
    manifest: Option<&SearchRollupCertification>,
    requires_certified_rollup: bool,
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

    if requires_certified_rollup {
        let date_str = partition_date.format(PARTITION_DATE_FORMAT).to_string();
        let has_certified_rollup = manifest
            .and_then(
                |certification| match certification.has_certified_artifacts(&date_str) {
                    Ok(certified) => Some(certified),
                    Err(error) => {
                        tracing::warn!(
                        "[analytics] Retaining raw partition {}: invalid rollup certification: {}",
                        partition_dir.display(),
                        error
                    );
                        None
                    }
                },
            )
            .unwrap_or(false);
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
    use crate::analytics::manifest::{WindowEntry, WindowStatus};
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

    fn manifest_path(base: &Path, index: &str) -> PathBuf {
        base.join(index).join("rollups").join("manifest.json")
    }

    fn save_empty_manifest(base: &Path, index: &str) {
        RollupManifest::new(index)
            .save(&manifest_path(base, index))
            .unwrap();
    }

    fn save_certified_hourly_manifest(base: &Path, index: &str, covered_date: &str) {
        let mut manifest = RollupManifest::new(index);
        let day_start_ms = chrono::NaiveDate::parse_from_str(covered_date, "%Y-%m-%d")
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let tier_dir = base.join(index).join("rollups").join("1hour");
        fs::create_dir_all(&tier_dir).unwrap();

        for hour in 0..24 {
            let start_ms = day_start_ms + hour * 3_600_000;
            let file = format!("rollup_1hour_{start_ms}_0.parquet");
            fs::write(tier_dir.join(&file), b"rollup-artifact").unwrap();
            manifest
                .record_window(
                    "1hour",
                    covered_date,
                    WindowEntry {
                        start_ms,
                        end_ms: start_ms + 3_600_000,
                        status: WindowStatus::Closed,
                        event_count: 1,
                        file,
                    },
                    &tier_dir,
                )
                .unwrap();
        }

        assert!(manifest.has_certified_coverage(covered_date, "1hour"));
        manifest.save(&manifest_path(base, index)).unwrap();
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

        let removed = remove_partition_if_expired(&partition, date(2024, 4, 1), None, false);

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

        let removed = remove_partition_if_expired(&partition, date(2024, 4, 1), None, false);

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

        let removed = remove_partition_if_expired(&partition, date(2024, 4, 1), None, false);

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

        assert!(!remove_partition_if_expired(
            &malformed_text,
            cutoff,
            None,
            false
        ));
        assert!(!remove_partition_if_expired(
            &malformed_calendar,
            cutoff,
            None,
            false
        ));
        assert!(!remove_partition_if_expired(
            &non_partition,
            cutoff,
            None,
            false
        ));
        assert!(malformed_text.exists());
        assert!(malformed_calendar.exists());
        assert!(non_partition.exists());
    }

    #[test]
    fn old_search_partition_without_manifest_is_retained() {
        let dir = tempfile::tempdir().unwrap();
        let old_search = create_partition_dir(
            dir.path(),
            "products",
            "searches",
            "date=2024-03-31",
            Some("searches.parquet"),
        );

        let removed = cleanup_old_partitions_at(dir.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 0);
        assert!(
            old_search.exists(),
            "raw searches must survive until a rollup manifest certifies coverage"
        );
    }

    #[test]
    fn old_search_partition_with_unreadable_manifest_is_retained() {
        let dir = tempfile::tempdir().unwrap();
        let old_search = create_partition_dir(
            dir.path(),
            "products",
            "searches",
            "date=2024-03-31",
            Some("searches.parquet"),
        );
        fs::create_dir_all(manifest_path(dir.path(), "products")).unwrap();

        let removed = cleanup_old_partitions_at(dir.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 0);
        assert!(
            old_search.exists(),
            "a manifest read error must fail closed instead of authorizing raw search deletion"
        );
    }

    #[cfg(unix)]
    #[test]
    fn old_search_partition_with_symlinked_manifest_is_retained() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let old_search = create_partition_dir(
            dir.path(),
            "products",
            "searches",
            "date=2024-03-31",
            Some("searches.parquet"),
        );
        save_certified_hourly_manifest(outside.path(), "external", "2024-03-31");
        let local_manifest = manifest_path(dir.path(), "products");
        fs::create_dir_all(local_manifest.parent().unwrap()).unwrap();
        symlink(manifest_path(outside.path(), "external"), local_manifest).unwrap();

        let removed = cleanup_old_partitions_at(dir.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 0);
        assert!(
            old_search.exists(),
            "an external manifest must never authorize raw search deletion"
        );
    }

    #[test]
    fn old_search_partition_without_certified_coverage_is_retained() {
        let dir = tempfile::tempdir().unwrap();
        let old_search = create_partition_dir(
            dir.path(),
            "products",
            "searches",
            "date=2024-03-31",
            Some("searches.parquet"),
        );
        save_empty_manifest(dir.path(), "products");

        let removed = cleanup_old_partitions_at(dir.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 0);
        assert!(old_search.exists());
    }

    #[test]
    fn old_search_partition_with_certified_coverage_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let old_search = create_partition_dir(
            dir.path(),
            "products",
            "searches",
            "date=2024-03-31",
            Some("searches.parquet"),
        );
        save_certified_hourly_manifest(dir.path(), "products", "2024-03-31");

        let removed = cleanup_old_partitions_at(dir.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 1);
        assert!(!old_search.exists());
    }

    #[test]
    fn copied_manifest_for_another_index_cannot_certify_search_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let old_search = create_partition_dir(
            dir.path(),
            "products",
            "searches",
            "date=2024-03-31",
            Some("searches.parquet"),
        );
        save_certified_hourly_manifest(dir.path(), "products", "2024-03-31");
        let path = manifest_path(dir.path(), "products");
        let mut manifest = RollupManifest::load(&path).unwrap();
        manifest.index = "another-index".to_string();
        manifest.save(&path).unwrap();

        let removed = cleanup_old_partitions_at(dir.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 0);
        assert!(old_search.exists());
    }

    #[test]
    fn future_schema_manifest_cannot_certify_search_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let old_search = create_partition_dir(
            dir.path(),
            "products",
            "searches",
            "date=2024-03-31",
            Some("searches.parquet"),
        );
        save_certified_hourly_manifest(dir.path(), "products", "2024-03-31");
        let path = manifest_path(dir.path(), "products");
        let mut manifest = RollupManifest::load(&path).unwrap();
        manifest.schema_version = rollup_schema_version_u32() + 1;
        manifest.save(&path).unwrap();

        let removed = cleanup_old_partitions_at(dir.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 0);
        assert!(old_search.exists());
    }

    #[test]
    fn missing_certified_rollup_artifact_retains_raw_searches() {
        let dir = tempfile::tempdir().unwrap();
        let old_search = create_partition_dir(
            dir.path(),
            "products",
            "searches",
            "date=2024-03-31",
            Some("searches.parquet"),
        );
        save_certified_hourly_manifest(dir.path(), "products", "2024-03-31");
        let manifest = RollupManifest::load(&manifest_path(dir.path(), "products")).unwrap();
        let missing_file = &manifest.tiers["1hour"].dates["2024-03-31"].windows[0].file;
        fs::remove_file(dir.path().join("products/rollups/1hour").join(missing_file)).unwrap();

        let removed = cleanup_old_partitions_at(dir.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 0);
        assert!(old_search.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_certified_rollup_artifact_retains_raw_searches() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let old_search = create_partition_dir(
            dir.path(),
            "products",
            "searches",
            "date=2024-03-31",
            Some("searches.parquet"),
        );
        save_certified_hourly_manifest(dir.path(), "products", "2024-03-31");
        let manifest = RollupManifest::load(&manifest_path(dir.path(), "products")).unwrap();
        let file_name = &manifest.tiers["1hour"].dates["2024-03-31"].windows[0].file;
        let artifact = dir.path().join("products/rollups/1hour").join(file_name);
        fs::remove_file(&artifact).unwrap();
        let external_artifact = outside.path().join("external.parquet");
        fs::write(&external_artifact, b"external").unwrap();
        symlink(&external_artifact, &artifact).unwrap();

        let removed = cleanup_old_partitions_at(dir.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 0);
        assert!(old_search.exists());
        assert!(external_artifact.exists());
    }

    #[test]
    fn old_event_partition_expires_independently_of_search_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let old_search = create_partition_dir(
            dir.path(),
            "products",
            "searches",
            "date=2024-03-31",
            Some("searches.parquet"),
        );
        let old_event = create_partition_dir(
            dir.path(),
            "products",
            "events",
            "date=2024-03-31",
            Some("events.parquet"),
        );
        save_empty_manifest(dir.path(), "products");

        let removed = cleanup_old_partitions_at(dir.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 1);
        assert!(
            old_search.exists(),
            "uncertified raw searches must remain while event retention proceeds"
        );
        assert!(
            !old_event.exists(),
            "insight-event expiry must not depend on unrelated search-rollup coverage"
        );
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
        save_certified_hourly_manifest(base, "myindex", "2020-01-01");

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
        save_certified_hourly_manifest(base, "idx_a", "2020-01-01");

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
        save_certified_hourly_manifest(base, "idx_a", "2024-03-31");

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

    #[cfg(unix)]
    #[test]
    fn cleanup_does_not_follow_symlinked_analytics_directories() {
        use std::os::unix::fs::symlink;

        let analytics = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let external_partition = create_partition_dir(
            outside.path(),
            "external",
            "events",
            "date=2024-03-31",
            Some("outside.parquet"),
        );
        symlink(
            outside.path().join("external"),
            analytics.path().join("linked-index"),
        )
        .unwrap();

        let removed =
            cleanup_old_partitions_at(analytics.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 0);
        assert!(
            external_partition.exists(),
            "retention must never traverse a symlink outside the analytics root"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_rejects_symlinked_searches_and_events_parents() {
        use std::os::unix::fs::symlink;

        for artifact in ["searches", "events"] {
            let analytics = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let external_partition = create_partition_dir(
                outside.path(),
                "external",
                artifact,
                "date=2024-03-31",
                Some("outside.parquet"),
            );
            let local_index = analytics.path().join("products");
            fs::create_dir_all(&local_index).unwrap();
            symlink(
                outside.path().join("external").join(artifact),
                local_index.join(artifact),
            )
            .unwrap();

            let error = cleanup_old_partitions_at(analytics.path(), 9, fixed_now(2024, 4, 10))
                .expect_err("a symlinked canonical artifact parent must fail closed");

            assert!(error.contains("symlinked analytics artifact directory"));
            assert!(external_partition.exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_rollups_parent_cannot_certify_search_deletion() {
        use std::os::unix::fs::symlink;

        let analytics = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let old_search = create_partition_dir(
            analytics.path(),
            "products",
            "searches",
            "date=2024-03-31",
            Some("searches.parquet"),
        );
        save_certified_hourly_manifest(outside.path(), "external", "2024-03-31");
        symlink(
            outside.path().join("external").join("rollups"),
            analytics.path().join("products").join("rollups"),
        )
        .unwrap();

        let removed =
            cleanup_old_partitions_at(analytics.path(), 9, fixed_now(2024, 4, 10)).unwrap();

        assert_eq!(removed, 0);
        assert!(
            old_search.exists(),
            "a symlinked rollups parent must not certify raw search deletion"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_rejects_symlinked_analytics_root() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let external_partition = create_partition_dir(
            outside.path(),
            "external",
            "events",
            "date=2024-03-31",
            Some("outside.parquet"),
        );
        let linked_root = parent.path().join("analytics");
        symlink(outside.path(), &linked_root).unwrap();

        let error = cleanup_old_partitions_at(&linked_root, 9, fixed_now(2024, 4, 10))
            .expect_err("a symlinked analytics root must fail closed");

        assert!(error.contains("symlinked analytics root"));
        assert!(external_partition.exists());
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
