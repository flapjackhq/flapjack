use std::path::{Path, PathBuf};

pub const DEFAULT_ANALYTICS_RETENTION_DAYS: u32 = 90;

/// Configuration for the analytics subsystem, loaded from environment variables.
#[derive(Debug, Clone)]
pub struct AnalyticsConfig {
    /// Whether analytics collection is enabled.
    pub enabled: bool,
    /// Base directory for analytics Parquet files.
    pub data_dir: PathBuf,
    /// How often to flush buffered events to disk (seconds).
    pub flush_interval_secs: u64,
    /// Flush when buffer reaches this many events.
    pub flush_size: usize,
    /// Delete Parquet files older than this many days.
    pub retention_days: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsTargetArtifactPaths {
    pub data_dir: PathBuf,
    pub index_root: PathBuf,
    pub searches_dir: PathBuf,
    pub events_dir: PathBuf,
    pub rollups_dir: PathBuf,
    pub rollup_manifest_path: PathBuf,
}

impl AnalyticsConfig {
    fn path_component(value: &str) -> String {
        let safe_literal = !value.is_empty()
            && value != "."
            && value != ".."
            && !value.starts_with("_fj_")
            && value.bytes().all(
                |byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'),
            );

        if safe_literal {
            return value.to_string();
        }

        // Encode unsafe path components instead of joining them directly. This
        // keeps user-controlled names from introducing absolute paths, `..`
        // segments, or platform separators under the analytics data directory.
        let mut encoded = String::from("_fj_");
        for byte in value.as_bytes() {
            use std::fmt::Write;
            write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
        }
        encoded
    }

    /// Load config from environment variables with sensible defaults.
    pub fn from_env() -> Self {
        let base_data_dir =
            std::env::var("FLAPJACK_DATA_DIR").unwrap_or_else(|_| "./data".to_string());
        let analytics_dir = std::env::var("FLAPJACK_ANALYTICS_DIR")
            .unwrap_or_else(|_| format!("{}/analytics", base_data_dir));

        Self::with_data_dir(PathBuf::from(analytics_dir))
    }

    /// Build the default analytics configuration for an explicit engine data directory.
    pub fn for_data_dir(data_dir: &Path) -> Self {
        Self::with_data_dir(data_dir.join("analytics"))
    }

    fn with_data_dir(data_dir: PathBuf) -> Self {
        Self {
            enabled: std::env::var("FLAPJACK_ANALYTICS_ENABLED")
                .map(|v| v != "false" && v != "0")
                .unwrap_or(true),
            data_dir,
            flush_interval_secs: std::env::var("FLAPJACK_ANALYTICS_FLUSH_INTERVAL")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60),
            flush_size: std::env::var("FLAPJACK_ANALYTICS_FLUSH_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_000),
            retention_days: std::env::var("FLAPJACK_ANALYTICS_RETENTION_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_ANALYTICS_RETENTION_DAYS),
        }
    }

    /// Config with analytics disabled (for tests).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            data_dir: PathBuf::from("/tmp/flapjack-analytics-disabled"),
            flush_interval_secs: 3600,
            flush_size: 100_000,
            retention_days: DEFAULT_ANALYTICS_RETENTION_DAYS,
        }
    }

    /// Path to search events for a given index.
    pub fn searches_dir(&self, index_name: &str) -> PathBuf {
        self.data_dir
            .join(Self::path_component(index_name))
            .join("searches")
    }

    /// Path to insight events for a given index.
    pub fn events_dir(&self, index_name: &str) -> PathBuf {
        self.data_dir
            .join(Self::path_component(index_name))
            .join("events")
    }

    /// Path to rollup Parquet files for a given index and tier.
    pub fn rollups_dir(&self, index_name: &str, tier: &str) -> PathBuf {
        self.data_dir
            .join(Self::path_component(index_name))
            .join("rollups")
            .join(Self::path_component(tier))
    }

    /// Path to the rollup manifest JSON for a given index.
    pub fn rollup_manifest_path(&self, index_name: &str) -> PathBuf {
        self.data_dir
            .join(Self::path_component(index_name))
            .join("rollups")
            .join("manifest.json")
    }

    pub fn target_artifact_paths(&self, index_name: &str) -> AnalyticsTargetArtifactPaths {
        let index_root = self.data_dir.join(Self::path_component(index_name));
        AnalyticsTargetArtifactPaths {
            data_dir: self.data_dir.clone(),
            searches_dir: self.searches_dir(index_name),
            events_dir: self.events_dir(index_name),
            rollups_dir: index_root.join("rollups"),
            rollup_manifest_path: self.rollup_manifest_path(index_name),
            index_root,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_not_enabled() {
        let cfg = AnalyticsConfig::disabled();
        assert!(!cfg.enabled);
    }

    #[test]
    fn disabled_config_has_sensible_defaults() {
        let cfg = AnalyticsConfig::disabled();
        assert_eq!(cfg.retention_days, DEFAULT_ANALYTICS_RETENTION_DAYS);
        assert_eq!(cfg.flush_interval_secs, 3600);
        assert_eq!(cfg.flush_size, 100_000);
    }

    #[test]
    fn canonical_retention_default_is_90() {
        assert_eq!(DEFAULT_ANALYTICS_RETENTION_DAYS, 90);
    }

    #[test]
    fn searches_dir_correct_path() {
        let cfg = AnalyticsConfig::disabled();
        let dir = cfg.searches_dir("my_index");
        assert!(dir.ends_with("my_index/searches"));
    }

    #[test]
    fn events_dir_correct_path() {
        let cfg = AnalyticsConfig::disabled();
        let dir = cfg.events_dir("my_index");
        assert!(dir.ends_with("my_index/events"));
    }

    #[test]
    fn searches_dir_different_indexes_differ() {
        let cfg = AnalyticsConfig::disabled();
        assert_ne!(cfg.searches_dir("idx_a"), cfg.searches_dir("idx_b"));
    }

    #[test]
    fn events_dir_different_indexes_differ() {
        let cfg = AnalyticsConfig::disabled();
        assert_ne!(cfg.events_dir("idx_a"), cfg.events_dir("idx_b"));
    }

    #[test]
    fn rollups_dir_correct_path() {
        let cfg = AnalyticsConfig::disabled();
        let dir = cfg.rollups_dir("my_index", "5min");
        assert!(dir.ends_with("my_index/rollups/5min"));
    }

    #[test]
    fn rollups_dir_different_tiers() {
        let cfg = AnalyticsConfig::disabled();
        assert_ne!(
            cfg.rollups_dir("idx", "5min"),
            cfg.rollups_dir("idx", "1hour")
        );
    }

    #[test]
    fn rollups_dir_different_indexes() {
        let cfg = AnalyticsConfig::disabled();
        assert_ne!(
            cfg.rollups_dir("idx_a", "1day"),
            cfg.rollups_dir("idx_b", "1day")
        );
    }

    #[test]
    fn rollup_manifest_path_correct_path() {
        let cfg = AnalyticsConfig::disabled();
        let path = cfg.rollup_manifest_path("products");
        assert!(path.ends_with("products/rollups/manifest.json"));
    }

    #[test]
    fn rollup_manifest_path_different_indexes() {
        let cfg = AnalyticsConfig::disabled();
        assert_ne!(
            cfg.rollup_manifest_path("idx_a"),
            cfg.rollup_manifest_path("idx_b")
        );
    }

    #[test]
    fn analytics_paths_encode_traversal_index_names() {
        let cfg = AnalyticsConfig::disabled();
        let path = cfg.rollup_manifest_path("../outside");

        assert_eq!(
            path,
            cfg.data_dir
                .join("_fj_2e2e2f6f757473696465")
                .join("rollups")
                .join("manifest.json")
        );
        assert!(path.starts_with(&cfg.data_dir));
    }

    #[test]
    fn analytics_paths_encode_absolute_tier_names() {
        let cfg = AnalyticsConfig::disabled();
        let path = cfg.rollups_dir("products", "/tmp/outside");

        assert_eq!(
            path,
            cfg.data_dir
                .join("products")
                .join("rollups")
                .join("_fj_2f746d702f6f757473696465")
        );
        assert!(path.starts_with(&cfg.data_dir));
    }
}
