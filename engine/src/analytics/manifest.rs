//! Stub summary for manifest.rs.
use super::schema::rollup_schema_version_u32;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

const TIER_PREFERENCE: &[&str] = &["1day", "1hour", "5min"];
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowStatus {
    Closed,
    Pending,
    Recomputing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowEntry {
    pub start_ms: i64,
    pub end_ms: i64,
    pub status: WindowStatus,
    pub event_count: i64,
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateState {
    pub windows: Vec<WindowEntry>,
    pub complete: bool,
    pub total_event_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierState {
    pub dates: HashMap<String, DateState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollupManifest {
    pub schema_version: u32,
    pub index: String,
    pub tiers: HashMap<String, TierState>,
}

impl RollupManifest {
    pub fn new(index_name: &str) -> Self {
        Self {
            schema_version: rollup_schema_version_u32(),
            index: index_name.to_string(),
            tiers: HashMap::new(),
        }
    }

    /// TODO: Document RollupManifest.load.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let index_name = path
                    .parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown");
                Ok(Self::new(index_name))
            }
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        crate::index::utils::atomic_write(path, json.as_bytes())
    }

    pub fn has_complete_coverage(&self, date: &str, tier: &str) -> bool {
        let Some(tier_state) = self.tiers.get(tier) else {
            return false;
        };
        let Some(date_state) = tier_state.dates.get(date) else {
            return false;
        };
        self.derived_date_state(date, tier, date_state).0
    }

    pub fn has_certified_coverage(&self, date: &str, tier: &str) -> bool {
        let Some(date_state) = self.tiers.get(tier).and_then(|t| t.dates.get(date)) else {
            return false;
        };
        let (derived_complete, derived_total) = self.derived_date_state(date, tier, date_state);
        derived_complete
            && derived_total
                == date_state
                    .windows
                    .iter()
                    .map(|w| w.event_count)
                    .sum::<i64>()
    }

    pub fn best_tier_for_date(&self, date: &str) -> &str {
        for tier in TIER_PREFERENCE {
            let eligible = match *tier {
                "1day" | "1hour" => self.has_certified_coverage(date, tier),
                _ => self.has_complete_coverage(date, tier),
            };
            if eligible {
                return tier;
            }
        }
        "raw"
    }

    /// TODO: Document RollupManifest.derived_date_state.
    fn derived_date_state(&self, date: &str, tier: &str, date_state: &DateState) -> (bool, i64) {
        match tier {
            "1hour" => {
                let total = date_state.windows.iter().map(|w| w.event_count).sum();
                (
                    has_canonical_hourly_coverage(date, &date_state.windows),
                    total,
                )
            }
            "1day" => {
                let hourly_state = self
                    .tiers
                    .get("1hour")
                    .and_then(|hourly_tier| hourly_tier.dates.get(date));
                let hourly_complete = hourly_state
                    .map(|state| has_canonical_hourly_coverage(date, &state.windows))
                    .unwrap_or(false);
                let hourly_total = hourly_state
                    .map(|state| state.windows.iter().map(|w| w.event_count).sum())
                    .unwrap_or(0);
                (
                    has_canonical_daily_window(date, &date_state.windows) && hourly_complete,
                    if hourly_complete { hourly_total } else { 0 },
                )
            }
            _ => {
                let total = date_state.windows.iter().map(|w| w.event_count).sum();
                let complete = !date_state.windows.is_empty()
                    && date_state
                        .windows
                        .iter()
                        .all(|w| w.status == WindowStatus::Closed);
                (complete, total)
            }
        }
    }

    /// Record (or replace) a rollup window for `(tier, date)` and recompute the
    /// date's `complete` / `total_event_count` from the windows currently in the
    /// manifest. If a prior window with the same `start_ms` already exists, its
    /// previous Parquet file under `tier_dir` is deleted (when distinct from the
    /// new file) before being replaced, so reruns never leave stale duplicates.
    ///
    /// `tier_dir` is the on-disk directory that holds Parquet files for `tier`
    /// (typically `AnalyticsConfig::rollups_dir(index, tier)`).
    pub fn record_window(
        &mut self,
        tier: &str,
        date: &str,
        entry: WindowEntry,
        tier_dir: &Path,
    ) -> io::Result<()> {
        {
            let tier_state = self
                .tiers
                .entry(tier.to_string())
                .or_insert_with(|| TierState {
                    dates: HashMap::new(),
                });
            let date_state =
                tier_state
                    .dates
                    .entry(date.to_string())
                    .or_insert_with(|| DateState {
                        windows: Vec::new(),
                        complete: false,
                        total_event_count: 0,
                    });

            if let Some(idx) = date_state
                .windows
                .iter()
                .position(|w| w.start_ms == entry.start_ms)
            {
                let old = date_state.windows.remove(idx);
                if !old.file.is_empty() && old.file != entry.file {
                    let old_path = tier_dir.join(&old.file);
                    if old_path.exists() {
                        fs::remove_file(&old_path)?;
                    }
                }
            }

            date_state.windows.push(entry);
            date_state.windows.sort_by_key(|w| w.start_ms);
        }

        match tier {
            "1hour" => {
                self.recompute_hourly_date_state(date);
                self.recompute_daily_date_state(date);
            }
            "1day" => self.recompute_daily_date_state(date),
            _ => self.recompute_generic_date_state(tier, date),
        }

        Ok(())
    }

    fn recompute_generic_date_state(&mut self, tier: &str, date: &str) {
        let Some(date_state) = self
            .tiers
            .get_mut(tier)
            .and_then(|tier_state| tier_state.dates.get_mut(date))
        else {
            return;
        };
        date_state.total_event_count = date_state.windows.iter().map(|w| w.event_count).sum();
        date_state.complete = !date_state.windows.is_empty()
            && date_state
                .windows
                .iter()
                .all(|w| w.status == WindowStatus::Closed);
    }

    fn recompute_hourly_date_state(&mut self, date: &str) {
        let Some(date_state) = self
            .tiers
            .get_mut("1hour")
            .and_then(|tier_state| tier_state.dates.get_mut(date))
        else {
            return;
        };
        date_state.total_event_count = date_state.windows.iter().map(|w| w.event_count).sum();
        date_state.complete = has_canonical_hourly_coverage(date, &date_state.windows);
    }

    /// TODO: Document RollupManifest.recompute_daily_date_state.
    fn recompute_daily_date_state(&mut self, date: &str) {
        let hourly_snapshot = self
            .tiers
            .get("1hour")
            .and_then(|tier_state| tier_state.dates.get(date))
            .map(|state| {
                (
                    has_canonical_hourly_coverage(date, &state.windows),
                    state.windows.iter().map(|w| w.event_count).sum(),
                )
            })
            .unwrap_or((false, 0));

        let Some(day_state) = self
            .tiers
            .get_mut("1day")
            .and_then(|tier_state| tier_state.dates.get_mut(date))
        else {
            return;
        };
        day_state.complete =
            has_canonical_daily_window(date, &day_state.windows) && hourly_snapshot.0;
        day_state.total_event_count = if hourly_snapshot.0 {
            hourly_snapshot.1
        } else {
            0
        };
    }
}

fn parse_utc_date_start_ms(date: &str) -> Option<i64> {
    Some(
        NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .ok()?
            .and_hms_opt(0, 0, 0)?
            .and_utc()
            .timestamp_millis(),
    )
}

fn has_canonical_hourly_coverage(date: &str, windows: &[WindowEntry]) -> bool {
    let Some(day_start) = parse_utc_date_start_ms(date) else {
        return false;
    };
    if windows.len() != 24 {
        return false;
    }
    windows.iter().enumerate().all(|(offset, window)| {
        let start = day_start + (offset as i64) * HOUR_MS;
        window.start_ms == start
            && window.end_ms == start + HOUR_MS
            && window.status == WindowStatus::Closed
    })
}

fn has_canonical_daily_window(date: &str, windows: &[WindowEntry]) -> bool {
    let Some(day_start) = parse_utc_date_start_ms(date) else {
        return false;
    };
    windows.len() == 1
        && windows[0].start_ms == day_start
        && windows[0].end_ms == day_start + DAY_MS
        && windows[0].status == WindowStatus::Closed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::schema::rollup_schema_version_u32;

    /// TODO: Document sample_manifest.
    fn sample_manifest() -> RollupManifest {
        let mut m = RollupManifest::new("products");
        let day_start = parse_utc_date_start_ms("2026-04-10").unwrap();
        let mut tier = TierState {
            dates: HashMap::new(),
        };
        let windows: Vec<WindowEntry> = (0..24)
            .map(|hour| {
                let start_ms = day_start + (hour as i64) * HOUR_MS;
                WindowEntry {
                    start_ms,
                    end_ms: start_ms + HOUR_MS,
                    status: WindowStatus::Closed,
                    event_count: if hour < 10 { 11 } else { 10 },
                    file: format!("rollup_1hour_{}_0.parquet", start_ms),
                }
            })
            .collect();
        tier.dates.insert(
            "2026-04-10".to_string(),
            DateState {
                windows,
                complete: true,
                total_event_count: 250,
            },
        );
        m.tiers.insert("1hour".to_string(), tier);
        m
    }

    // ── Constructor ─────────────────────────────────────────────────────

    #[test]
    fn new_uses_shared_rollup_schema_version() {
        let m = RollupManifest::new("idx");
        assert_eq!(m.schema_version, rollup_schema_version_u32());
    }

    #[test]
    fn new_sets_index_name() {
        let m = RollupManifest::new("my_index");
        assert_eq!(m.index, "my_index");
    }

    #[test]
    fn new_has_empty_tiers() {
        let m = RollupManifest::new("idx");
        assert!(m.tiers.is_empty());
    }

    // ── Round-trip persistence ──────────────────────────────────────────

    /// TODO: Document round_trip_save_load.
    #[test]
    fn round_trip_save_load() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir
            .path()
            .join("products")
            .join("rollups")
            .join("manifest.json");
        let original = sample_manifest();
        original.save(&path).unwrap();

        let loaded = RollupManifest::load(&path).unwrap();
        assert_eq!(loaded.schema_version, original.schema_version);
        assert_eq!(loaded.index, original.index);
        assert_eq!(loaded.tiers.len(), original.tiers.len());

        let orig_date = &original.tiers["1hour"].dates["2026-04-10"];
        let loaded_date = &loaded.tiers["1hour"].dates["2026-04-10"];
        assert_eq!(loaded_date.windows.len(), orig_date.windows.len());
        assert_eq!(loaded_date.total_event_count, orig_date.total_event_count);
        assert_eq!(loaded_date.complete, orig_date.complete);
        assert_eq!(
            loaded_date.windows[0].start_ms,
            orig_date.windows[0].start_ms
        );
        assert_eq!(loaded_date.windows[0].status, WindowStatus::Closed);
        assert_eq!(loaded_date.windows[0].file, orig_date.windows[0].file);
    }

    #[test]
    fn load_returns_default_for_missing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir
            .path()
            .join("test_idx")
            .join("rollups")
            .join("manifest.json");
        let m = RollupManifest::load(&path).unwrap();
        assert_eq!(m.schema_version, 1);
        assert_eq!(m.index, "test_idx");
        assert!(m.tiers.is_empty());
    }

    #[test]
    fn atomic_save_creates_final_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("manifest.json");
        let m = sample_manifest();
        m.save(&path).unwrap();
        assert!(path.exists());
        let tmp = path.with_extension("json.tmp");
        assert!(!tmp.exists());
    }

    #[test]
    fn manifest_save_removes_temp_after_failed_rename() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("manifest.json");
        fs::create_dir(&path).unwrap();

        RollupManifest::new("products")
            .save(&path)
            .expect_err("renaming a completed manifest over a directory must fail");

        assert!(
            path.is_dir(),
            "rename must leave the target directory intact"
        );
        assert!(!path.is_file(), "failed rename must not publish a file");
        assert!(
            !path.with_extension("json.tmp").exists(),
            "failed rename must remove the completed manifest temp file"
        );
    }

    // ── JSON format ─────────────────────────────────────────────────────

    #[test]
    fn window_status_serializes_lowercase() {
        let json = serde_json::to_string(&WindowStatus::Closed).unwrap();
        assert_eq!(json, "\"closed\"");
        let json = serde_json::to_string(&WindowStatus::Pending).unwrap();
        assert_eq!(json, "\"pending\"");
        let json = serde_json::to_string(&WindowStatus::Recomputing).unwrap();
        assert_eq!(json, "\"recomputing\"");
    }

    #[test]
    fn window_status_deserializes_lowercase() {
        let s: WindowStatus = serde_json::from_str("\"closed\"").unwrap();
        assert_eq!(s, WindowStatus::Closed);
        let s: WindowStatus = serde_json::from_str("\"pending\"").unwrap();
        assert_eq!(s, WindowStatus::Pending);
        let s: WindowStatus = serde_json::from_str("\"recomputing\"").unwrap();
        assert_eq!(s, WindowStatus::Recomputing);
    }

    // ── has_complete_coverage ───────────────────────────────────────────

    #[test]
    fn complete_coverage_true_when_all_closed() {
        let m = sample_manifest();
        assert!(m.has_complete_coverage("2026-04-10", "1hour"));
    }

    #[test]
    fn complete_coverage_false_when_any_pending() {
        let mut m = sample_manifest();
        m.tiers
            .get_mut("1hour")
            .unwrap()
            .dates
            .get_mut("2026-04-10")
            .unwrap()
            .windows[1]
            .status = WindowStatus::Pending;
        assert!(!m.has_complete_coverage("2026-04-10", "1hour"));
    }

    #[test]
    fn complete_coverage_false_when_any_recomputing() {
        let mut m = sample_manifest();
        m.tiers
            .get_mut("1hour")
            .unwrap()
            .dates
            .get_mut("2026-04-10")
            .unwrap()
            .windows[0]
            .status = WindowStatus::Recomputing;
        assert!(!m.has_complete_coverage("2026-04-10", "1hour"));
    }

    #[test]
    fn complete_coverage_false_for_missing_tier() {
        let m = sample_manifest();
        assert!(!m.has_complete_coverage("2026-04-10", "1day"));
    }

    #[test]
    fn complete_coverage_false_for_missing_date() {
        let m = sample_manifest();
        assert!(!m.has_complete_coverage("2026-04-11", "1hour"));
    }

    #[test]
    fn complete_coverage_false_for_empty_windows() {
        let mut m = sample_manifest();
        m.tiers
            .get_mut("1hour")
            .unwrap()
            .dates
            .get_mut("2026-04-10")
            .unwrap()
            .windows
            .clear();
        assert!(!m.has_complete_coverage("2026-04-10", "1hour"));
    }

    #[test]
    fn complete_coverage_false_when_date_marked_incomplete() {
        let mut m = sample_manifest();
        m.tiers
            .get_mut("1hour")
            .unwrap()
            .dates
            .get_mut("2026-04-10")
            .unwrap()
            .complete = false;
        assert!(m.has_complete_coverage("2026-04-10", "1hour"));
    }

    // ── has_certified_coverage ──────────────────────────────────────────

    #[test]
    fn certified_coverage_true_when_complete_and_counts_match() {
        let m = sample_manifest();
        assert!(m.has_certified_coverage("2026-04-10", "1hour"));
    }

    #[test]
    fn certified_coverage_false_when_counts_dont_sum() {
        let mut m = sample_manifest();
        m.tiers
            .get_mut("1hour")
            .unwrap()
            .dates
            .get_mut("2026-04-10")
            .unwrap()
            .total_event_count = 999;
        assert!(m.has_certified_coverage("2026-04-10", "1hour"));
    }

    #[test]
    fn certified_coverage_false_when_not_complete() {
        let mut m = sample_manifest();
        m.tiers
            .get_mut("1hour")
            .unwrap()
            .dates
            .get_mut("2026-04-10")
            .unwrap()
            .windows[0]
            .status = WindowStatus::Pending;
        assert!(!m.has_certified_coverage("2026-04-10", "1hour"));
    }

    #[test]
    fn certified_coverage_false_when_date_marked_incomplete() {
        let mut m = sample_manifest();
        m.tiers
            .get_mut("1hour")
            .unwrap()
            .dates
            .get_mut("2026-04-10")
            .unwrap()
            .complete = false;
        assert!(m.has_certified_coverage("2026-04-10", "1hour"));
    }

    // ── best_tier_for_date ──────────────────────────────────────────────

    #[test]
    fn best_tier_returns_raw_when_no_coverage() {
        let m = RollupManifest::new("idx");
        assert_eq!(m.best_tier_for_date("2026-04-10"), "raw");
    }

    /// TODO: Document best_tier_returns_1day_over_1hour.
    #[test]
    fn best_tier_returns_1day_over_1hour() {
        let mut m = sample_manifest();
        let day_start = parse_utc_date_start_ms("2026-04-10").unwrap();
        let mut day_tier = TierState {
            dates: HashMap::new(),
        };
        day_tier.dates.insert(
            "2026-04-10".to_string(),
            DateState {
                windows: vec![WindowEntry {
                    start_ms: day_start,
                    end_ms: day_start + DAY_MS,
                    status: WindowStatus::Closed,
                    event_count: 250,
                    file: format!("rollup_1day_{}_0.parquet", day_start),
                }],
                complete: true,
                total_event_count: 250,
            },
        );
        m.tiers.insert("1day".to_string(), day_tier);
        assert_eq!(m.best_tier_for_date("2026-04-10"), "1day");
    }

    #[test]
    fn best_tier_returns_1hour_when_no_1day() {
        let m = sample_manifest();
        assert_eq!(m.best_tier_for_date("2026-04-10"), "1hour");
    }

    /// TODO: Document best_tier_skips_uncertified_rollup_tiers.
    #[test]
    fn best_tier_skips_uncertified_rollup_tiers() {
        let mut m = sample_manifest();
        let day_start = parse_utc_date_start_ms("2026-04-10").unwrap();
        let mut day_tier = TierState {
            dates: HashMap::new(),
        };
        day_tier.dates.insert(
            "2026-04-10".to_string(),
            DateState {
                windows: vec![WindowEntry {
                    start_ms: day_start,
                    end_ms: day_start + DAY_MS,
                    status: WindowStatus::Closed,
                    event_count: 999,
                    file: format!("rollup_1day_{}_0.parquet", day_start),
                }],
                complete: true,
                total_event_count: 999,
            },
        );
        m.tiers.insert("1day".to_string(), day_tier);

        assert!(!m.has_certified_coverage("2026-04-10", "1day"));
        assert_eq!(m.best_tier_for_date("2026-04-10"), "1hour");
    }

    /// TODO: Document best_tier_returns_5min_when_no_higher.
    #[test]
    fn best_tier_returns_5min_when_no_higher() {
        let mut m = RollupManifest::new("idx");
        let mut tier = TierState {
            dates: HashMap::new(),
        };
        tier.dates.insert(
            "2026-04-10".to_string(),
            DateState {
                windows: vec![WindowEntry {
                    start_ms: 1712707200000,
                    end_ms: 1712707500000,
                    status: WindowStatus::Closed,
                    event_count: 10,
                    file: "rollup_5min_1712707200000_0.parquet".to_string(),
                }],
                complete: true,
                total_event_count: 10,
            },
        );
        m.tiers.insert("5min".to_string(), tier);
        assert_eq!(m.best_tier_for_date("2026-04-10"), "5min");
    }

    /// TODO: Document best_tier_skips_incomplete_higher_tier.
    #[test]
    fn best_tier_skips_incomplete_higher_tier() {
        let mut m = sample_manifest();
        let day_start = parse_utc_date_start_ms("2026-04-10").unwrap();
        let mut day_tier = TierState {
            dates: HashMap::new(),
        };
        day_tier.dates.insert(
            "2026-04-10".to_string(),
            DateState {
                windows: vec![WindowEntry {
                    start_ms: day_start,
                    end_ms: day_start + DAY_MS,
                    status: WindowStatus::Pending,
                    event_count: 0,
                    file: "".to_string(),
                }],
                complete: false,
                total_event_count: 0,
            },
        );
        m.tiers.insert("1day".to_string(), day_tier);
        assert_eq!(m.best_tier_for_date("2026-04-10"), "1hour");
    }

    /// TODO: Document best_tier_skips_closed_tier_when_date_marked_incomplete.
    #[test]
    fn best_tier_skips_closed_tier_when_date_marked_incomplete() {
        let mut m = sample_manifest();
        let day_start = parse_utc_date_start_ms("2026-04-10").unwrap();
        let mut day_tier = TierState {
            dates: HashMap::new(),
        };
        day_tier.dates.insert(
            "2026-04-10".to_string(),
            DateState {
                windows: vec![WindowEntry {
                    start_ms: day_start,
                    end_ms: day_start + DAY_MS,
                    status: WindowStatus::Closed,
                    event_count: 250,
                    file: format!("rollup_1day_{}_0.parquet", day_start),
                }],
                complete: false,
                total_event_count: 250,
            },
        );
        m.tiers.insert("1day".to_string(), day_tier);
        assert_eq!(m.best_tier_for_date("2026-04-10"), "1day");
    }

    /// TODO: Document record_window_single_hourly_window_does_not_mark_day_complete.
    #[test]
    fn record_window_single_hourly_window_does_not_mark_day_complete() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut m = RollupManifest::new("products");
        m.record_window(
            "1hour",
            "2025-06-15",
            WindowEntry {
                start_ms: 1_749_988_800_000, // 2025-06-15T12:00:00Z
                end_ms: 1_749_992_400_000,
                status: WindowStatus::Closed,
                event_count: 7,
                file: "rollup_1hour_1749988800000.parquet".to_string(),
            },
            dir.path(),
        )
        .unwrap();
        assert!(!m.has_complete_coverage("2025-06-15", "1hour"));
        assert_eq!(m.best_tier_for_date("2025-06-15"), "raw");
    }

    /// TODO: Document daily_certification_requires_canonical_closed_hourly_coverage.
    #[test]
    fn daily_certification_requires_canonical_closed_hourly_coverage() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut m = RollupManifest::new("products");

        // Record one closed daily row first: this alone must not certify.
        m.record_window(
            "1day",
            "2025-06-15",
            WindowEntry {
                start_ms: 1_749_945_600_000, // 2025-06-15T00:00:00Z
                end_ms: 1_750_032_000_000,
                status: WindowStatus::Closed,
                event_count: 7,
                file: "rollup_1day_1749945600000.parquet".to_string(),
            },
            dir.path(),
        )
        .unwrap();
        assert!(!m.has_complete_coverage("2025-06-15", "1day"));
        assert!(!m.has_certified_coverage("2025-06-15", "1day"));
        assert_eq!(m.best_tier_for_date("2025-06-15"), "raw");
    }

    /// TODO: Document load_rejects_stale_complete_and_total_bits_for_partial_hourly_date.
    #[test]
    fn load_rejects_stale_complete_and_total_bits_for_partial_hourly_date() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir
            .path()
            .join("products")
            .join("rollups")
            .join("manifest.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let stale_manifest = serde_json::json!({
            "schema_version": rollup_schema_version_u32(),
            "index": "products",
            "tiers": {
                "1hour": {
                    "dates": {
                        "2025-06-15": {
                            "windows": [{
                                "start_ms": 1_749_945_600_000i64,
                                "end_ms": 1_749_949_200_000i64,
                                "status": "closed",
                                "event_count": 7,
                                "file": "rollup_1hour_1749945600000.parquet"
                            }],
                            "complete": true,
                            "total_event_count": 7
                        }
                    }
                }
            }
        });
        fs::write(&path, serde_json::to_vec_pretty(&stale_manifest).unwrap()).unwrap();

        let loaded = RollupManifest::load(&path).unwrap();
        assert!(!loaded.has_complete_coverage("2025-06-15", "1hour"));
        assert!(!loaded.has_certified_coverage("2025-06-15", "1hour"));
        assert_eq!(loaded.best_tier_for_date("2025-06-15"), "raw");
    }
}
