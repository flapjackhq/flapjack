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
            if source.index_name == config.index_name {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "source index '{}' must differ from the suggestions destination",
                        source.index_name
                    ),
                ));
            }
        }
        let path = self.config_path(&config.index_name)?;
        let json = serde_json::to_string_pretty(config).map_err(std::io::Error::other)?;
        crate::index::atomic_write_file(&path, json.as_bytes())
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
        let paths = self.target_artifact_paths(index_name)?;
        if !paths.config_path.exists() {
            return Ok(false);
        }
        for sidecar in [&paths.status_path, &paths.log_path] {
            if sidecar.exists() {
                std::fs::remove_file(sidecar)?;
            }
        }
        std::fs::remove_file(paths.config_path)?;
        Ok(true)
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
        crate::index::atomic_write_file(&path, json.as_bytes())
    }

    /// Persist a terminal failed build using the Algolia-compatible status
    /// fields: `lastBuiltAt` advances while `lastSuccessfulBuiltAt` does not.
    /// The paired ERROR log carries the human-readable reason.
    pub fn record_failed_build(&self, index_name: &str, message: &str) -> std::io::Result<()> {
        self.record_failed_build_before_terminal(index_name, message, || {})
    }

    fn record_failed_build_before_terminal<F>(
        &self,
        index_name: &str,
        message: &str,
        before_terminal_status: F,
    ) -> std::io::Result<()>
    where
        F: FnOnce(),
    {
        let now = chrono::Utc::now().to_rfc3339();
        let mut status = self.load_status(index_name);
        self.append_log(
            index_name,
            &[LogEntry {
                timestamp: now.clone(),
                level: "ERROR".to_string(),
                message: message.to_string(),
                context_level: 0,
            }],
        )?;
        self.truncate_log(index_name, 1000)?;
        before_terminal_status();
        status.is_running = false;
        status.last_built_at = Some(now);
        self.save_status(&status)
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
        crate::index::atomic_write_file(&path, new_content.as_bytes())
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
    fn stage3_load_config_reports_invalid_data_for_malformed_json() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        store.ensure_dir().unwrap();
        std::fs::write(store.dir.join("broken.json"), "{not json").unwrap();

        let err = store
            .load_config("broken")
            .expect_err("malformed config JSON must surface InvalidData");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
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
    fn stage3_list_configs_skips_only_malformed_configs() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        store
            .save_config(&make_config("valid", "src_valid"))
            .unwrap();
        store.ensure_dir().unwrap();
        std::fs::write(store.dir.join("malformed.json"), "{not json").unwrap();
        std::fs::write(
            store.dir.join("valid.status.json"),
            serde_json::to_string(&BuildStatus {
                index_name: "valid".to_string(),
                is_running: true,
                last_built_at: None,
                last_successful_built_at: None,
            })
            .unwrap(),
        )
        .unwrap();

        let list = store.list_configs().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].index_name, "valid");
    }

    #[test]
    fn delete_config_returns_true_then_false() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        store.save_config(&make_config("del_me", "x")).unwrap();
        store
            .save_status(&BuildStatus {
                index_name: "del_me".to_string(),
                is_running: false,
                last_built_at: Some("2026-08-18T00:00:00Z".to_string()),
                last_successful_built_at: Some("2026-08-18T00:00:00Z".to_string()),
            })
            .unwrap();
        store
            .append_log(
                "del_me",
                &[LogEntry {
                    timestamp: "2026-08-18T00:00:00Z".to_string(),
                    level: "INFO".to_string(),
                    message: "built".to_string(),
                    context_level: 1,
                }],
            )
            .unwrap();
        assert!(store.delete_config("del_me").unwrap());
        let paths = store.target_artifact_paths("del_me").unwrap();
        assert!(!paths.config_path.exists());
        assert!(!paths.status_path.exists());
        assert!(!paths.log_path.exists());
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
    fn stage3_load_status_returns_default_for_malformed_json() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        store.ensure_dir().unwrap();
        std::fs::write(store.dir.join("broken.status.json"), "{not json").unwrap();

        let status = store.load_status("broken");
        assert_eq!(status.index_name, "broken");
        assert!(!status.is_running);
        assert!(status.last_built_at.is_none());
        assert!(status.last_successful_built_at.is_none());
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

    #[test]
    fn failed_build_advances_attempt_without_claiming_success() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        store
            .save_config(&make_config("failed", "products"))
            .unwrap();
        store
            .record_failed_build("failed", "analytics engine not initialized")
            .unwrap();

        let status = store.load_status("failed");
        assert!(!status.is_running);
        assert!(status.last_built_at.is_some());
        assert!(status.last_successful_built_at.is_none());
        let logs = store.read_logs("failed");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, "ERROR");
        assert_eq!(logs[0].message, "analytics engine not initialized");
    }

    #[test]
    fn failed_build_log_error_keeps_status_running_and_delete_blocked() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        store
            .save_config(&make_config("failed", "products"))
            .unwrap();
        store
            .save_status(&BuildStatus {
                index_name: "failed".to_string(),
                is_running: true,
                last_built_at: None,
                last_successful_built_at: None,
            })
            .unwrap();
        let log_path = store.log_path("failed").unwrap();
        std::fs::create_dir_all(&log_path).unwrap();

        assert!(store
            .record_failed_build("failed", "forced log persistence failure")
            .is_err());
        assert!(
            store.load_status("failed").is_running,
            "a failed log write must not publish terminal status and admit deletion"
        );
        assert!(store.config_exists("failed"));
    }

    #[test]
    fn failed_build_delete_race_leaves_zero_control_residue() {
        let tmp = TempDir::new().unwrap();
        let store = std::sync::Arc::new(QsConfigStore::new(tmp.path()));
        store
            .save_config(&make_config("failed", "products"))
            .unwrap();
        store
            .save_status(&BuildStatus {
                index_name: "failed".to_string(),
                is_running: true,
                last_built_at: None,
                last_successful_built_at: None,
            })
            .unwrap();

        let (log_persisted_tx, log_persisted_rx) = std::sync::mpsc::sync_channel(0);
        let (publish_terminal_tx, publish_terminal_rx) = std::sync::mpsc::sync_channel(0);
        let worker_store = std::sync::Arc::clone(&store);
        let worker = std::thread::spawn(move || {
            worker_store.record_failed_build_before_terminal(
                "failed",
                "deterministic failure",
                || {
                    log_persisted_tx.send(()).unwrap();
                    publish_terminal_rx.recv().unwrap();
                },
            )
        });

        log_persisted_rx.recv().unwrap();
        assert!(store.load_status("failed").is_running);
        assert!(
            store.config_exists("failed") && store.load_status("failed").is_running,
            "DELETE must remain blocked until all failure-sidecar writes finish"
        );
        publish_terminal_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();

        assert!(!store.load_status("failed").is_running);
        assert!(store.delete_config("failed").unwrap());
        let paths = store.target_artifact_paths("failed").unwrap();
        assert!(!paths.config_path.exists());
        assert!(!paths.status_path.exists());
        assert!(!paths.log_path.exists());
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

    #[test]
    fn stage3_read_logs_keeps_valid_lines_and_omits_malformed_jsonl() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        store.ensure_dir().unwrap();
        let valid_one = LogEntry {
            timestamp: "2026-07-31T00:00:00Z".to_string(),
            level: "INFO".to_string(),
            message: "Build café started".to_string(),
            context_level: 1,
        };
        let valid_two = LogEntry {
            timestamp: "2026-07-31T00:00:01Z".to_string(),
            level: "WARN".to_string(),
            message: "東京 fallback retained".to_string(),
            context_level: 2,
        };
        std::fs::write(
            store.dir.join("logs.log.jsonl"),
            format!(
                "{}\n{{not json\n{}\n",
                serde_json::to_string(&valid_one).unwrap(),
                serde_json::to_string(&valid_two).unwrap()
            ),
        )
        .unwrap();

        let logs = store.read_logs("logs");
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].message, "Build café started");
        assert_eq!(logs[1].message, "東京 fallback retained");
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

    #[test]
    fn stage3_multibyte_names_stay_under_query_suggestions_directory() {
        let tmp = TempDir::new().unwrap();
        let store = QsConfigStore::new(tmp.path());
        let config = make_config("suggestions_東京", "products_é");
        store.save_config(&config).unwrap();

        let loaded = store.load_config("suggestions_東京").unwrap().unwrap();
        assert_eq!(loaded.index_name, "suggestions_東京");
        assert_eq!(loaded.source_indices[0].index_name, "products_é");

        let paths = store.target_artifact_paths("suggestions_東京").unwrap();
        assert_eq!(paths.root_dir, tmp.path().join(".query_suggestions"));
        assert!(paths.config_path.starts_with(&paths.root_dir));
        assert!(paths.status_path.starts_with(&paths.root_dir));
        assert!(paths.log_path.starts_with(&paths.root_dir));
        assert!(store.target_artifact_paths("../escape").is_err());
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
