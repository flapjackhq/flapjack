use crate::usage_middleware::TenantUsageCounters;
use chrono::NaiveDate;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
/// Per-index usage numbers for a single day.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexUsageSnapshot {
    pub search_operations: u64,
    pub total_write_operations: u64,
    pub total_read_operations: u64,
    pub records: u64,
    pub bytes_received: u64,
    pub search_results_total: u64,
    pub documents_deleted: u64,
    /// `None` preserves the distinction between absent legacy data and an explicit zero.
    #[serde(default)]
    pub documents_count: Option<u64>,
    #[serde(default)]
    pub storage_bytes: Option<u64>,
}

impl IndexUsageSnapshot {
    /// View this day's persisted gauge options through the shared gauge type,
    /// so gauge-name lookups have exactly one owner (`CapturedUsageGauges::get`).
    pub fn captured_gauges(&self) -> CapturedUsageGauges {
        CapturedUsageGauges {
            documents_count: self.documents_count,
            storage_bytes: self.storage_bytes,
        }
    }
}

/// Immutable per-index gauges captured at rollover time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapturedUsageGauges {
    pub documents_count: Option<u64>,
    pub storage_bytes: Option<u64>,
}

impl CapturedUsageGauges {
    pub fn get(&self, stat: &str) -> Option<u64> {
        match stat {
            "documents_count" => self.documents_count,
            "storage_bytes" => self.storage_bytes,
            _ => None,
        }
    }
}

/// A complete snapshot for one calendar day.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyUsageSnapshot {
    pub date: String,
    pub indexes: HashMap<String, IndexUsageSnapshot>,
}

/// Manages on-disk usage snapshots.
pub struct UsagePersistence {
    usage_dir: PathBuf,
}

impl UsagePersistence {
    /// Create a new persistence store, ensuring the `_usage` directory exists.
    pub fn new(data_dir: &Path) -> io::Result<Self> {
        let usage_dir = data_dir.join("_usage");
        std::fs::create_dir_all(&usage_dir)?;
        Ok(Self { usage_dir })
    }

    /// Path to the snapshot file for a given date.
    fn snapshot_path(&self, date: &str) -> PathBuf {
        self.usage_dir.join(format!("{}.json", date))
    }

    /// Atomically write a snapshot: write to tmp file, then rename.
    pub fn save_snapshot(
        &self,
        date: &str,
        counters: &DashMap<String, TenantUsageCounters>,
    ) -> io::Result<()> {
        let snapshot = self.counters_to_snapshot(date, counters);
        self.write_snapshot(date, &snapshot)
    }

    /// Save counters with caller-owned gauges captured at rollover time.
    pub fn save_snapshot_with_gauges(
        &self,
        date: &str,
        counters: &DashMap<String, TenantUsageCounters>,
        gauges: &HashMap<String, CapturedUsageGauges>,
    ) -> io::Result<()> {
        let snapshot = self.counters_and_gauges_to_snapshot(date, counters, gauges);
        self.write_snapshot(date, &snapshot)
    }

    fn write_snapshot(&self, date: &str, snapshot: &DailyUsageSnapshot) -> io::Result<()> {
        let json = serde_json::to_string_pretty(&snapshot).map_err(io::Error::other)?;

        let final_path = self.snapshot_path(date);
        let tmp_path = self.usage_dir.join(format!("{}.json.tmp", date));

        std::fs::write(&tmp_path, json.as_bytes())?;
        std::fs::rename(&tmp_path, &final_path)?;

        Ok(())
    }

    /// Load a snapshot for a given date, returning `None` if no file exists.
    pub fn load_snapshot(&self, date: &str) -> io::Result<Option<DailyUsageSnapshot>> {
        let path = self.snapshot_path(date);
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path)?;
        let snapshot: DailyUsageSnapshot = serde_json::from_str(&data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Some(snapshot))
    }

    /// Load a saved snapshot back into the live counters (used at startup).
    pub fn load_into_counters(
        &self,
        date: &str,
        counters: &DashMap<String, TenantUsageCounters>,
    ) -> io::Result<bool> {
        match self.load_snapshot(date)? {
            Some(snapshot) => {
                for (index_name, stats) in snapshot.indexes {
                    let entry = counters.entry(index_name).or_default();
                    entry
                        .search_count
                        .fetch_add(stats.search_operations, Ordering::Relaxed);
                    entry
                        .write_count
                        .fetch_add(stats.total_write_operations, Ordering::Relaxed);
                    entry
                        .read_count
                        .fetch_add(stats.total_read_operations, Ordering::Relaxed);
                    entry
                        .bytes_in
                        .fetch_add(stats.bytes_received, Ordering::Relaxed);
                    entry
                        .search_results_total
                        .fetch_add(stats.search_results_total, Ordering::Relaxed);
                    entry
                        .documents_indexed_total
                        .fetch_add(stats.records, Ordering::Relaxed);
                    entry
                        .documents_deleted_total
                        .fetch_add(stats.documents_deleted, Ordering::Relaxed);
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Perform a daily rollup: save the current counters as the day's
    /// snapshot, then reset all counters to zero.
    pub fn rollup(
        &self,
        date: &str,
        counters: &DashMap<String, TenantUsageCounters>,
    ) -> io::Result<()> {
        self.rollup_with_gauges(date, counters, &HashMap::new())
    }

    /// Roll up counters with caller-owned gauges captured at rollover time.
    pub fn rollup_with_gauges(
        &self,
        date: &str,
        counters: &DashMap<String, TenantUsageCounters>,
        gauges: &HashMap<String, CapturedUsageGauges>,
    ) -> io::Result<()> {
        self.save_snapshot_with_gauges(date, counters, gauges)?;
        Self::reset_counters(counters);
        Ok(())
    }

    fn reset_counters(counters: &DashMap<String, TenantUsageCounters>) {
        for entry in counters.iter() {
            entry.search_count.store(0, Ordering::Relaxed);
            entry.write_count.store(0, Ordering::Relaxed);
            entry.read_count.store(0, Ordering::Relaxed);
            entry.bytes_in.store(0, Ordering::Relaxed);
            entry.search_results_total.store(0, Ordering::Relaxed);
            entry.documents_indexed_total.store(0, Ordering::Relaxed);
            entry.documents_deleted_total.store(0, Ordering::Relaxed);
        }
    }

    /// Load all snapshots in the inclusive date range `[start, end]`.
    ///
    /// Days with no snapshot file are silently skipped; the returned vec
    /// contains only days that have data, in chronological order.
    pub fn load_date_range(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> io::Result<Vec<(NaiveDate, DailyUsageSnapshot)>> {
        let mut result = Vec::new();
        let mut current = start;
        while current <= end {
            let date_str = current.format("%Y-%m-%d").to_string();
            if let Some(snapshot) = self.load_snapshot(&date_str)? {
                result.push((current, snapshot));
            }
            match current.succ_opt() {
                Some(next) => current = next,
                None => break,
            }
        }
        Ok(result)
    }

    /// List all snapshot dates available on disk.
    pub fn available_dates(&self) -> io::Result<Vec<String>> {
        let mut dates = Vec::new();
        for entry in std::fs::read_dir(&self.usage_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(date) = name.strip_suffix(".json") {
                if !date.ends_with(".tmp") {
                    dates.push(date.to_string());
                }
            }
        }
        dates.sort();
        Ok(dates)
    }

    /// Convert live counters into a snapshot struct.
    fn counters_to_snapshot(
        &self,
        date: &str,
        counters: &DashMap<String, TenantUsageCounters>,
    ) -> DailyUsageSnapshot {
        self.counters_and_gauges_to_snapshot(date, counters, &HashMap::new())
    }

    fn counters_and_gauges_to_snapshot(
        &self,
        date: &str,
        counters: &DashMap<String, TenantUsageCounters>,
        gauges: &HashMap<String, CapturedUsageGauges>,
    ) -> DailyUsageSnapshot {
        let mut indexes = HashMap::new();
        for entry in counters.iter() {
            let stats = IndexUsageSnapshot {
                search_operations: entry.search_count.load(Ordering::Relaxed),
                total_write_operations: entry.write_count.load(Ordering::Relaxed),
                total_read_operations: entry.read_count.load(Ordering::Relaxed),
                records: entry.documents_indexed_total.load(Ordering::Relaxed),
                bytes_received: entry.bytes_in.load(Ordering::Relaxed),
                search_results_total: entry.search_results_total.load(Ordering::Relaxed),
                documents_deleted: entry.documents_deleted_total.load(Ordering::Relaxed),
                documents_count: None,
                storage_bytes: None,
            };
            indexes.insert(entry.key().clone(), stats);
        }
        for (index_name, captured) in gauges {
            let stats = indexes.entry(index_name.clone()).or_default();
            stats.documents_count = captured.documents_count;
            stats.storage_bytes = captured.storage_bytes;
        }
        DailyUsageSnapshot {
            date: date.to_string(),
            indexes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use tempfile::TempDir;

    /// Build a `DashMap` pre-loaded with a single "products" index whose counters have known non-zero values for every tracked metric.
    fn make_counters() -> DashMap<String, TenantUsageCounters> {
        let counters: DashMap<String, TenantUsageCounters> = DashMap::new();
        {
            let entry = counters.entry("products".to_string()).or_default();
            entry.search_count.fetch_add(10, Ordering::Relaxed);
            entry.write_count.fetch_add(5, Ordering::Relaxed);
            entry.read_count.fetch_add(3, Ordering::Relaxed);
            entry.bytes_in.fetch_add(1024, Ordering::Relaxed);
            entry
                .documents_indexed_total
                .fetch_add(42, Ordering::Relaxed);
            entry.search_results_total.fetch_add(100, Ordering::Relaxed);
            entry
                .documents_deleted_total
                .fetch_add(2, Ordering::Relaxed);
        }
        counters
    }

    fn assert_zero_counters(snapshot: &IndexUsageSnapshot) {
        assert_eq!(snapshot.search_operations, 0);
        assert_eq!(snapshot.total_write_operations, 0);
        assert_eq!(snapshot.total_read_operations, 0);
        assert_eq!(snapshot.records, 0);
        assert_eq!(snapshot.bytes_received, 0);
        assert_eq!(snapshot.search_results_total, 0);
        assert_eq!(snapshot.documents_deleted, 0);
    }

    #[test]
    fn legacy_snapshot_without_gauges_loads_as_absent() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let legacy_json = r#"{
            "date": "2026-02-23",
            "indexes": {
                "legacy": {
                    "search_operations": 11,
                    "total_write_operations": 22,
                    "total_read_operations": 33,
                    "records": 44,
                    "bytes_received": 55,
                    "search_results_total": 66,
                    "documents_deleted": 77
                }
            }
        }"#;
        std::fs::write(persistence.snapshot_path("2026-02-23"), legacy_json).unwrap();

        let snapshot = persistence
            .load_snapshot("2026-02-23")
            .unwrap()
            .expect("legacy snapshot should load");
        let legacy = &snapshot.indexes["legacy"];
        assert_eq!(legacy.search_operations, 11);
        assert_eq!(legacy.total_write_operations, 22);
        assert_eq!(legacy.total_read_operations, 33);
        assert_eq!(legacy.records, 44);
        assert_eq!(legacy.bytes_received, 55);
        assert_eq!(legacy.search_results_total, 66);
        assert_eq!(legacy.documents_deleted, 77);
        assert_eq!(legacy.documents_count, None);
        assert_eq!(legacy.storage_bytes, None);
    }

    #[test]
    fn gauge_aware_save_preserves_optional_values_and_unions_indexes() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let counters = make_counters();
        let gauges = HashMap::from([
            (
                "products".to_string(),
                CapturedUsageGauges {
                    documents_count: Some(900),
                    storage_bytes: None,
                },
            ),
            (
                "archive".to_string(),
                CapturedUsageGauges {
                    documents_count: None,
                    storage_bytes: Some(8_192),
                },
            ),
            (
                "empty".to_string(),
                CapturedUsageGauges {
                    documents_count: Some(0),
                    storage_bytes: Some(0),
                },
            ),
        ]);

        persistence
            .save_snapshot_with_gauges("2026-02-26", &counters, &gauges)
            .unwrap();

        let snapshot = persistence
            .load_snapshot("2026-02-26")
            .unwrap()
            .expect("snapshot should exist");
        let mut names: Vec<_> = snapshot.indexes.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["archive", "empty", "products"]);

        let products = &snapshot.indexes["products"];
        assert_eq!(products.search_operations, 10);
        assert_eq!(products.total_write_operations, 5);
        assert_eq!(products.total_read_operations, 3);
        assert_eq!(products.records, 42);
        assert_eq!(products.bytes_received, 1024);
        assert_eq!(products.search_results_total, 100);
        assert_eq!(products.documents_deleted, 2);
        assert_eq!(products.documents_count, Some(900));
        assert_eq!(products.storage_bytes, None);

        let archive = &snapshot.indexes["archive"];
        assert_zero_counters(archive);
        assert_eq!(archive.documents_count, None);
        assert_eq!(archive.storage_bytes, Some(8_192));

        let empty = &snapshot.indexes["empty"];
        assert_zero_counters(empty);
        assert_eq!(empty.documents_count, Some(0));
        assert_eq!(empty.storage_bytes, Some(0));
    }

    #[test]
    fn gauge_aware_rollup_resets_only_counters() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let counters = make_counters();
        let gauges = HashMap::from([(
            "products".to_string(),
            CapturedUsageGauges {
                documents_count: Some(42),
                storage_bytes: Some(4_096),
            },
        )]);
        let captured_gauges = gauges.clone();

        persistence
            .rollup_with_gauges("2026-02-26", &counters, &gauges)
            .unwrap();

        let entry = counters.get("products").unwrap();
        assert_eq!(entry.search_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.write_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.read_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.bytes_in.load(Ordering::Relaxed), 0);
        assert_eq!(entry.search_results_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_indexed_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_deleted_total.load(Ordering::Relaxed), 0);
        assert_eq!(gauges, captured_gauges);

        let snapshot = persistence
            .load_snapshot("2026-02-26")
            .unwrap()
            .expect("snapshot should exist");
        assert_eq!(snapshot.indexes["products"].documents_count, Some(42));
        assert_eq!(snapshot.indexes["products"].storage_bytes, Some(4_096));
    }

    /// Verify that saving counters to disk and reloading them into a fresh `DashMap` produces identical values, simulating a server restart.
    #[test]
    fn usage_counters_survive_server_restart() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let date = "2026-02-26";

        // Simulate server running: save current counters
        let counters = make_counters();
        persistence.save_snapshot(date, &counters).unwrap();

        // Simulate restart: fresh counters, reload from disk
        let fresh_counters: DashMap<String, TenantUsageCounters> = DashMap::new();
        let loaded = persistence
            .load_into_counters(date, &fresh_counters)
            .unwrap();
        assert!(loaded, "should have loaded snapshot from disk");

        let entry = fresh_counters
            .get("products")
            .expect("products should exist");
        assert_eq!(entry.search_count.load(Ordering::Relaxed), 10);
        assert_eq!(entry.write_count.load(Ordering::Relaxed), 5);
        assert_eq!(entry.read_count.load(Ordering::Relaxed), 3);
        assert_eq!(entry.bytes_in.load(Ordering::Relaxed), 1024);
        assert_eq!(entry.documents_indexed_total.load(Ordering::Relaxed), 42);
        assert_eq!(entry.search_results_total.load(Ordering::Relaxed), 100);
        assert_eq!(entry.documents_deleted_total.load(Ordering::Relaxed), 2);
    }

    /// Verify that `rollup` writes a date-keyed JSON file under `_usage/` and that the loaded snapshot contains the expected index data.
    #[test]
    fn usage_daily_rollup_creates_date_keyed_snapshots() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();

        let counters = make_counters();
        persistence.rollup("2026-02-25", &counters).unwrap();

        // The date-keyed file should exist
        let path = tmp.path().join("_usage/2026-02-25.json");
        assert!(path.exists(), "snapshot file should be created");

        // Load it back and verify
        let snapshot = persistence
            .load_snapshot("2026-02-25")
            .unwrap()
            .expect("snapshot should exist");
        assert_eq!(snapshot.date, "2026-02-25");
        assert!(
            snapshot.indexes.contains_key("products"),
            "snapshot should contain products index"
        );
        assert_eq!(snapshot.indexes["products"].search_operations, 10);
    }

    /// Verify that all per-index atomic counters are zeroed after a daily rollup persists the snapshot to disk.
    #[test]
    fn usage_counters_reset_after_rollup() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();

        let counters = make_counters();
        persistence.rollup("2026-02-25", &counters).unwrap();

        // All counters should be zeroed after rollup
        let entry = counters.get("products").expect("entry still exists");
        assert_eq!(
            entry.search_count.load(Ordering::Relaxed),
            0,
            "search_count should be 0 after rollup"
        );
        assert_eq!(
            entry.write_count.load(Ordering::Relaxed),
            0,
            "write_count should be 0 after rollup"
        );
        assert_eq!(
            entry.read_count.load(Ordering::Relaxed),
            0,
            "read_count should be 0 after rollup"
        );
        assert_eq!(
            entry.bytes_in.load(Ordering::Relaxed),
            0,
            "bytes_in should be 0 after rollup"
        );
        assert_eq!(
            entry.documents_indexed_total.load(Ordering::Relaxed),
            0,
            "documents_indexed_total should be 0 after rollup"
        );
        assert_eq!(
            entry.search_results_total.load(Ordering::Relaxed),
            0,
            "search_results_total should be 0 after rollup"
        );
        assert_eq!(
            entry.documents_deleted_total.load(Ordering::Relaxed),
            0,
            "documents_deleted_total should be 0 after rollup"
        );
    }

    /// Verify that the on-disk snapshot is well-formed JSON containing the required `date` and `indexes` fields and round-trips through `DailyUsageSnapshot` deserialization.
    #[test]
    fn usage_persisted_files_are_valid_json() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let counters = make_counters();

        persistence.save_snapshot("2026-02-26", &counters).unwrap();

        let path = tmp.path().join("_usage/2026-02-26.json");
        let raw = std::fs::read_to_string(&path).unwrap();

        // Must parse as valid JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).expect("persisted file must be valid JSON");

        // Must have required top-level fields
        assert!(parsed["date"].is_string(), "must have date field");
        assert!(parsed["indexes"].is_object(), "must have indexes object");

        // Re-parse as our typed struct to verify round-trip
        let snapshot: DailyUsageSnapshot =
            serde_json::from_str(&raw).expect("must deserialize to DailyUsageSnapshot");
        assert_eq!(snapshot.date, "2026-02-26");
        assert_eq!(snapshot.indexes["products"].search_operations, 10);
    }

    /// Verify that `save_snapshot` leaves no `.tmp` file behind after the atomic write-then-rename completes and that the final file is loadable.
    #[test]
    fn usage_rollup_write_is_atomic() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let counters = make_counters();

        persistence.save_snapshot("2026-02-26", &counters).unwrap();

        // The tmp file should NOT exist after a successful write
        let tmp_path = tmp.path().join("_usage/2026-02-26.json.tmp");
        assert!(
            !tmp_path.exists(),
            "tmp file should not remain after atomic write"
        );

        // The final file should exist and be readable
        let final_path = tmp.path().join("_usage/2026-02-26.json");
        assert!(final_path.exists(), "final snapshot file should exist");

        let snapshot = persistence
            .load_snapshot("2026-02-26")
            .unwrap()
            .expect("should load successfully");
        assert_eq!(snapshot.indexes["products"].search_operations, 10);
    }

    #[test]
    fn load_into_counters_returns_false_when_no_snapshot() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let counters: DashMap<String, TenantUsageCounters> = DashMap::new();

        let loaded = persistence
            .load_into_counters("2026-01-01", &counters)
            .unwrap();
        assert!(!loaded, "should return false when no snapshot exists");
        assert!(counters.is_empty(), "counters should remain empty");
    }

    #[test]
    fn available_dates_lists_snapshots() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let counters = make_counters();

        persistence.save_snapshot("2026-02-24", &counters).unwrap();
        persistence.save_snapshot("2026-02-25", &counters).unwrap();
        persistence.save_snapshot("2026-02-26", &counters).unwrap();

        let dates = persistence.available_dates().unwrap();
        assert_eq!(dates, vec!["2026-02-24", "2026-02-25", "2026-02-26"]);
    }

    /// Verify that a snapshot containing multiple distinct indexes round-trips correctly, preserving each index's independent counter values.
    #[test]
    fn multiple_indexes_preserved_in_snapshot() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let counters: DashMap<String, TenantUsageCounters> = DashMap::new();

        counters
            .entry("idx_a".to_string())
            .or_default()
            .search_count
            .fetch_add(5, Ordering::Relaxed);
        counters
            .entry("idx_b".to_string())
            .or_default()
            .write_count
            .fetch_add(8, Ordering::Relaxed);

        persistence.save_snapshot("2026-02-26", &counters).unwrap();

        let snapshot = persistence
            .load_snapshot("2026-02-26")
            .unwrap()
            .expect("snapshot should exist");
        assert_eq!(snapshot.indexes["idx_a"].search_operations, 5);
        assert_eq!(snapshot.indexes["idx_b"].total_write_operations, 8);
    }

    // ── billing contract lifecycle tests ──

    /// Full billing lifecycle: set all 7 counters → rollup (saves + resets) → reload into
    /// fresh counters → verify all 7 fields match originals. This guards the contract that
    /// billing data is preserved through the daily reset cycle and survives a simulated restart.
    #[test]
    fn billing_counters_survive_rollup_and_reload_cycle() {
        let tmp = TempDir::new().unwrap();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();

        let counters: DashMap<String, TenantUsageCounters> = DashMap::new();
        {
            let entry = counters.entry("billing_lifecycle".to_string()).or_default();
            entry.search_count.store(100, Ordering::Relaxed);
            entry.write_count.store(200, Ordering::Relaxed);
            entry.read_count.store(300, Ordering::Relaxed);
            entry.bytes_in.store(400, Ordering::Relaxed);
            entry.search_results_total.store(500, Ordering::Relaxed);
            entry.documents_indexed_total.store(600, Ordering::Relaxed);
            entry.documents_deleted_total.store(700, Ordering::Relaxed);
        }

        // Rollup: persist snapshot then reset all counters to zero
        persistence.rollup("2026-03-15", &counters).unwrap();

        // Counters must be zeroed after rollup
        let entry = counters.get("billing_lifecycle").unwrap();
        assert_eq!(entry.search_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.write_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.read_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.bytes_in.load(Ordering::Relaxed), 0);
        assert_eq!(entry.search_results_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_indexed_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_deleted_total.load(Ordering::Relaxed), 0);
        drop(entry);

        // Simulate restart: fresh counters, reload from persisted snapshot
        let fresh_counters: DashMap<String, TenantUsageCounters> = DashMap::new();
        let loaded = persistence
            .load_into_counters("2026-03-15", &fresh_counters)
            .unwrap();
        assert!(loaded, "should reload persisted snapshot");

        // All 7 billing fields must match original values
        let restored = fresh_counters.get("billing_lifecycle").unwrap();
        assert_eq!(restored.search_count.load(Ordering::Relaxed), 100);
        assert_eq!(restored.write_count.load(Ordering::Relaxed), 200);
        assert_eq!(restored.read_count.load(Ordering::Relaxed), 300);
        assert_eq!(restored.bytes_in.load(Ordering::Relaxed), 400);
        assert_eq!(restored.search_results_total.load(Ordering::Relaxed), 500);
        assert_eq!(
            restored.documents_indexed_total.load(Ordering::Relaxed),
            600
        );
        assert_eq!(
            restored.documents_deleted_total.load(Ordering::Relaxed),
            700
        );
    }
}
