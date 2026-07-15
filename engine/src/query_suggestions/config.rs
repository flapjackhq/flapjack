//! Manages Query Suggestions configuration files, build status records, and newline-delimited JSON logs stored in a .query_suggestions directory with path traversal protection.
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

fn default_min_hits() -> u64 {
    5
}

fn default_min_letters() -> usize {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct QsFacet {
    pub attribute: String,
    pub amount: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct QsSourceIndex {
    pub index_name: String,
    #[serde(default = "default_min_hits")]
    pub min_hits: u64,
    #[serde(default = "default_min_letters")]
    pub min_letters: usize,
    #[serde(default)]
    pub facets: Vec<QsFacet>,
    #[serde(default)]
    pub generate: Vec<Vec<String>>,
    #[serde(default)]
    pub analytics_tags: Vec<String>,
    #[serde(default)]
    pub replicas: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct QsConfig {
    pub index_name: String,
    pub source_indices: Vec<QsSourceIndex>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub allow_special_characters: bool,
    #[serde(default)]
    pub enable_personalization: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BuildStatus {
    pub index_name: String,
    #[serde(default)]
    pub is_running: bool,
    pub last_built_at: Option<String>,
    pub last_successful_built_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub context_level: u8,
}

/// Manages Query Suggestions config/status/log files on disk.
///
/// Files are stored at `{base_dir}/.query_suggestions/`:
/// - `{indexName}.json` — config
/// - `{indexName}.status.json` — build status
/// - `{indexName}.log.jsonl` — build log (newline-delimited JSON, capped at 1000 lines)
pub struct QsConfigStore {
    dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QsTargetArtifactPaths {
    pub root_dir: PathBuf,
    pub config_path: PathBuf,
    pub status_path: PathBuf,
    pub log_path: PathBuf,
}

fn validate_store_index_name(index_name: &str) -> std::io::Result<()> {
    crate::validate_index_name(index_name).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid indexName '{}': {}", index_name, e),
        )
    })
}

impl QsConfigStore {
    pub fn new(base_dir: &Path) -> Self {
        let dir = base_dir.join(".query_suggestions");
        Self { dir }
    }

    pub fn target_artifact_paths(
        &self,
        index_name: &str,
    ) -> std::io::Result<QsTargetArtifactPaths> {
        Ok(QsTargetArtifactPaths {
            root_dir: self.dir.clone(),
            config_path: self.config_path(index_name)?,
            status_path: self.status_path(index_name)?,
            log_path: self.log_path(index_name)?,
        })
    }

    fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)
    }

    fn config_path(&self, index_name: &str) -> std::io::Result<PathBuf> {
        validate_store_index_name(index_name)?;
        Ok(self.dir.join(format!("{}.json", index_name)))
    }

    fn status_path(&self, index_name: &str) -> std::io::Result<PathBuf> {
        validate_store_index_name(index_name)?;
        Ok(self.dir.join(format!("{}.status.json", index_name)))
    }

    fn log_path(&self, index_name: &str) -> std::io::Result<PathBuf> {
        validate_store_index_name(index_name)?;
        Ok(self.dir.join(format!("{}.log.jsonl", index_name)))
    }

    pub fn config_exists(&self, index_name: &str) -> bool {
        self.config_path(index_name)
            .map(|path| path.exists())
            .unwrap_or(false)
    }

    pub fn save_config(&self, config: &QsConfig) -> std::io::Result<()> {
        self.ensure_dir()?;
        validate_store_index_name(&config.index_name)?;
        for source in &config.source_indices {
            validate_store_index_name(&source.index_name)?;
        }
        let path = self.config_path(&config.index_name)?;
        let json = serde_json::to_string_pretty(config).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    pub fn load_config(&self, index_name: &str) -> std::io::Result<Option<QsConfig>> {
        let path = self.config_path(index_name)?;
        if !path.exists() {
            return Ok(None);
        }
        let json = std::fs::read_to_string(path)?;
        let config = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Some(config))
    }

    /// Load all query suggestions configs from disk, excluding .status.json and .log.jsonl files.
    ///
    /// # Returns
    ///
    /// Vector of all successfully deserialized QsConfig objects, or empty vector if directory does not exist. Silently skips malformed files.
    pub fn list_configs(&self) -> std::io::Result<Vec<QsConfig>> {
        if !self.dir.exists() {
            return Ok(vec![]);
        }
        let mut configs = vec![];
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            let fname = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            // Only plain config files: {indexName}.json — not *.status.json
            if fname.ends_with(".json") && !fname.ends_with(".status.json") {
                if let Ok(json) = std::fs::read_to_string(&path) {
                    if let Ok(config) = serde_json::from_str::<QsConfig>(&json) {
                        configs.push(config);
                    }
                }
            }
        }
        Ok(configs)
    }

    pub fn delete_config(&self, index_name: &str) -> std::io::Result<bool> {
        let path = self.config_path(index_name)?;
        if path.exists() {
            std::fs::remove_file(path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Load the build status for an index, or return a default status if not found.
    ///
    /// # Arguments
    ///
    /// * `index_name` - The query suggestions index name.
    ///
    /// # Returns
    ///
    /// BuildStatus with persisted state if file exists and is valid, otherwise default BuildStatus with is_running=false and no timestamps. Never returns an error.
    pub fn load_status(&self, index_name: &str) -> BuildStatus {
        let Ok(path) = self.status_path(index_name) else {
            return BuildStatus {
                index_name: index_name.to_string(),
                ..Default::default()
            };
        };
        if path.exists() {
            if let Ok(json) = std::fs::read_to_string(&path) {
                if let Ok(status) = serde_json::from_str::<BuildStatus>(&json) {
                    return status;
                }
            }
        }
        BuildStatus {
            index_name: index_name.to_string(),
            ..Default::default()
        }
    }

    pub fn save_status(&self, status: &BuildStatus) -> std::io::Result<()> {
        self.ensure_dir()?;
        let path = self.status_path(&status.index_name)?;
        let json = serde_json::to_string(status).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// Write log entries to an index's log file in newline-delimited JSON format.
    ///
    /// # Arguments
    ///
    /// * `index_name` - The query suggestions index name.
    /// * `entries` - Log entries to append; no-op if empty.
    pub fn append_log(&self, index_name: &str, entries: &[LogEntry]) -> std::io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        self.ensure_dir()?;
        let path = self.log_path(index_name)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        for entry in entries {
            let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
            writeln!(file, "{}", line)?;
        }
        Ok(())
    }

    /// Truncate log to at most `max_lines` most-recent entries.
    pub fn truncate_log(&self, index_name: &str, max_lines: usize) -> std::io::Result<()> {
        let path = self.log_path(index_name)?;
        if !path.exists() {
            return Ok(());
        }
        let content = std::fs::read_to_string(&path)?;
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() <= max_lines {
            return Ok(());
        }
        let keep = &lines[lines.len() - max_lines..];
        let new_content = keep.join("\n") + "\n";
        std::fs::write(path, new_content)
    }

    /// Load all log entries for an index from its log file in order.
    ///
    /// # Arguments
    ///
    /// * `index_name` - The query suggestions index name.
    ///
    /// # Returns
    ///
    /// Vector of LogEntry objects in file order, or empty vector if log file does not exist or cannot be read.
    pub fn read_logs(&self, index_name: &str) -> Vec<LogEntry> {
        let Ok(path) = self.log_path(index_name) else {
            return vec![];
        };
        if !path.exists() {
            return vec![];
        }
        std::fs::read_to_string(&path)
            .ok()
            .map(|content| {
                content
                    .lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a test QsConfig with one source index and default parameters.
    fn make_config(index_name: &str, source: &str) -> QsConfig {
        QsConfig {
            index_name: index_name.to_string(),
            source_indices: vec![QsSourceIndex {
                index_name: source.to_string(),
                min_hits: 5,
                min_letters: 4,
                facets: vec![],
                generate: vec![],
                analytics_tags: vec![],
                replicas: false,
            }],
            languages: vec![],
            exclude: vec![],
            allow_special_characters: false,
            enable_personalization: false,
        }
    }

    #[test]
    fn config_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        let config = make_config("my_suggestions", "products");
        store.save_config(&config).unwrap();
        let loaded = store.load_config("my_suggestions").unwrap().unwrap();
        assert_eq!(loaded.index_name, "my_suggestions");
        assert_eq!(loaded.source_indices[0].index_name, "products");
        assert_eq!(loaded.source_indices[0].min_hits, 5);
    }

    #[test]
    fn load_nonexistent_config_returns_none() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        assert!(store.load_config("ghost").unwrap().is_none());
    }

    #[test]
    fn list_configs_returns_all() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        store.save_config(&make_config("a", "src_a")).unwrap();
        store.save_config(&make_config("b", "src_b")).unwrap();
        let list = store.list_configs().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn delete_config_returns_true_then_false() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        store.save_config(&make_config("del_me", "x")).unwrap();
        assert!(store.delete_config("del_me").unwrap());
        assert!(!store.delete_config("del_me").unwrap());
    }

    #[test]
    fn status_defaults_to_not_running() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        let status = store.load_status("no_build_yet");
        assert!(!status.is_running);
        assert!(status.last_built_at.is_none());
    }

    #[test]
    fn status_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        let status = BuildStatus {
            index_name: "test".to_string(),
            is_running: false,
            last_built_at: Some("2026-02-19T12:00:00Z".to_string()),
            last_successful_built_at: Some("2026-02-19T12:00:00Z".to_string()),
        };
        store.save_status(&status).unwrap();
        let loaded = store.load_status("test");
        assert_eq!(loaded.last_built_at.unwrap(), "2026-02-19T12:00:00Z");
    }

    /// Verify that log entries can be appended and read back in order with all fields preserved.
    #[test]
    fn log_append_and_read() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        let entries = vec![
            LogEntry {
                timestamp: "2026-02-19T12:00:00Z".to_string(),
                level: "INFO".to_string(),
                message: "Build started".to_string(),
                context_level: 1,
            },
            LogEntry {
                timestamp: "2026-02-19T12:00:01Z".to_string(),
                level: "INFO".to_string(),
                message: "Build complete: 42 suggestions".to_string(),
                context_level: 1,
            },
        ];
        store.append_log("test", &entries).unwrap();
        let logs = store.read_logs("test");
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].message, "Build started");
    }

    /// Verify that truncate_log correctly retains only the most recent entries when the log exceeds max_lines.
    #[test]
    fn log_truncates_to_max_lines() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        let entries: Vec<LogEntry> = (0..10)
            .map(|i| LogEntry {
                timestamp: "2026-02-19T00:00:00Z".to_string(),
                level: "INFO".to_string(),
                message: format!("entry {}", i),
                context_level: 1,
            })
            .collect();
        store.append_log("test", &entries).unwrap();
        store.truncate_log("test", 5).unwrap();
        let logs = store.read_logs("test");
        assert_eq!(logs.len(), 5);
        assert_eq!(logs[0].message, "entry 5");
        assert_eq!(logs[4].message, "entry 9");
    }

    #[test]
    fn save_config_rejects_path_traversal_index_name() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        let cfg = make_config("../keys", "products");

        let err = store
            .save_config(&cfg)
            .expect_err("path traversal index name must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            !tmp.path().join("keys.json").exists(),
            "must not create files outside .query_suggestions directory"
        );
    }

    /// Verify that save_status rejects index names with path traversal patterns.
    #[test]
    fn save_status_rejects_path_traversal_index_name() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        let status = BuildStatus {
            index_name: "../keys".to_string(),
            is_running: true,
            last_built_at: None,
            last_successful_built_at: None,
        };

        let err = store
            .save_status(&status)
            .expect_err("path traversal index name must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            !tmp.path().join("keys.status.json").exists(),
            "must not create files outside .query_suggestions directory"
        );
    }
}
