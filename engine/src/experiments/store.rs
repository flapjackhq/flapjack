//! Persistent, file-backed store for A/B experiment lifecycle management with atomic writes, numeric ID mapping, and single-active-per-index enforcement.
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};

use dashmap::DashMap;

use super::config::{Experiment, ExperimentConclusion, ExperimentError, ExperimentStatus};

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub struct ExperimentFilter {
    pub index_name: Option<String>,
    pub status: Option<ExperimentStatus>,
}

pub struct ExperimentStore {
    experiments: DashMap<String, Experiment>,
    active_by_index: DashMap<String, String>,
    /// Maps UUID string IDs → sequential integer IDs (Algolia-compatible).
    id_to_numeric: DashMap<String, i64>,
    /// Reverse map: integer ID → UUID string.
    numeric_to_id: DashMap<i64, String>,
    /// Next integer ID to assign.
    next_numeric_id: AtomicI64,
    dir: PathBuf,
}

impl ExperimentStore {
    pub fn new(data_dir: &std::path::Path) -> Result<Self, ExperimentError> {
        let dir = data_dir.join(".experiments");
        std::fs::create_dir_all(&dir)?;
        let store = Self {
            experiments: DashMap::new(),
            active_by_index: DashMap::new(),
            id_to_numeric: DashMap::new(),
            numeric_to_id: DashMap::new(),
            next_numeric_id: AtomicI64::new(1),
            dir,
        };
        store.load_all(true)?;
        Ok(store)
    }

    /// Hydrate existing experiment state without creating directories or
    /// republishing the numeric-ID map. Release-fenced startup uses this path;
    /// later admitted mutations lazily initialize missing persistence through
    /// the canonical `persist_json` owner.
    pub fn load_only(data_dir: &std::path::Path) -> Result<Self, ExperimentError> {
        let store = Self {
            experiments: DashMap::new(),
            active_by_index: DashMap::new(),
            id_to_numeric: DashMap::new(),
            numeric_to_id: DashMap::new(),
            next_numeric_id: AtomicI64::new(1),
            dir: data_dir.join(".experiments"),
        };
        store.load_all(false)?;
        Ok(store)
    }

    /// Load all persisted experiment JSON files and the numeric ID mapping from the store directory.
    ///
    /// Applies backward-compatibility fixups for older records that stored stop time in `ended_at`,
    /// validates each experiment, registers running experiments in the active-by-index map, and
    /// assigns numeric IDs to any experiments that lack one.
    ///
    /// # Errors
    ///
    /// Returns an error if any persisted experiment fails validation or if multiple running
    /// experiments target the same index.
    fn load_all(&self, persist_repairs: bool) -> Result<(), ExperimentError> {
        if !self.dir.exists() {
            return Ok(());
        }
        // Load the numeric ID mapping if it exists.
        let mapping_path = self.dir.join("_id_map.json");
        if mapping_path.exists() {
            let data = std::fs::read_to_string(&mapping_path)?;
            let map: std::collections::HashMap<String, i64> = serde_json::from_str(&data)?;
            let mut max_id = 0i64;
            for (uuid, numeric) in &map {
                self.id_to_numeric.insert(uuid.clone(), *numeric);
                self.numeric_to_id.insert(*numeric, uuid.clone());
                if *numeric > max_id {
                    max_id = *numeric;
                }
            }
            self.next_numeric_id.store(max_id + 1, Ordering::SeqCst);
        }

        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json")
                && !path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('_') || n.ends_with(".tmp"))
            {
                let data = std::fs::read_to_string(&path)?;
                let mut experiment: Experiment = serde_json::from_str(&data)?;
                if (experiment.status == ExperimentStatus::Stopped
                    || experiment.status == ExperimentStatus::Concluded)
                    && experiment.stopped_at.is_none()
                {
                    // Backward compatibility: older persisted records stored stop time in ended_at.
                    experiment.stopped_at = experiment.ended_at;
                }
                experiment.validate()?;
                self.register_active_if_running(&experiment)?;
                // Assign a numeric ID if not already mapped.
                if !self.id_to_numeric.contains_key(&experiment.id) {
                    self.assign_numeric_id(&experiment.id);
                }
                self.experiments.insert(experiment.id.clone(), experiment);
            }
        }
        // Persist mapping in case new IDs were assigned during load.
        if persist_repairs {
            self.persist_id_map()?;
        }
        Ok(())
    }

    /// Record the experiment as the active experiment for its index if it is in `Running` status.
    ///
    /// Enforces the single-active-experiment-per-index invariant: returns `InvalidConfig` if a
    /// different experiment is already active on the same index. No-ops for non-running experiments
    /// or if the same experiment is already registered.
    fn register_active_if_running(&self, experiment: &Experiment) -> Result<(), ExperimentError> {
        if experiment.status != ExperimentStatus::Running {
            return Ok(());
        }
        if let Some(existing_id) = self.active_by_index.get(&experiment.index_name) {
            if existing_id.value() != &experiment.id {
                return Err(ExperimentError::InvalidConfig(format!(
                    "multiple running experiments for index '{}': '{}' and '{}'",
                    experiment.index_name,
                    existing_id.value(),
                    experiment.id
                )));
            }
            return Ok(());
        }
        self.active_by_index
            .insert(experiment.index_name.clone(), experiment.id.clone());
        Ok(())
    }

    fn unregister_active_if_running(&self, experiment: &Experiment) {
        if experiment.status != ExperimentStatus::Running {
            return;
        }
        let should_remove = self
            .active_by_index
            .get(&experiment.index_name)
            .is_some_and(|entry| entry.value() == &experiment.id);
        if should_remove {
            self.active_by_index.remove(&experiment.index_name);
        }
    }

    fn assign_numeric_id(&self, uuid: &str) -> i64 {
        let id = self.next_numeric_id.fetch_add(1, Ordering::SeqCst);
        self.id_to_numeric.insert(uuid.to_string(), id);
        self.numeric_to_id.insert(id, uuid.to_string());
        id
    }

    fn persist_json<T: serde::Serialize>(
        &self,
        file_name: &str,
        value: &T,
    ) -> Result<(), ExperimentError> {
        std::fs::create_dir_all(&self.dir)?;
        let data = serde_json::to_string_pretty(value)?;
        crate::index::utils::atomic_write(&self.dir.join(file_name), data.as_bytes())?;
        Ok(())
    }

    fn persist_id_map(&self) -> Result<(), ExperimentError> {
        let map: std::collections::HashMap<String, i64> = self
            .id_to_numeric
            .iter()
            .map(|entry| (entry.key().clone(), *entry.value()))
            .collect();
        self.persist_json("_id_map.json", &map)
    }

    /// Get the numeric (Algolia-compatible) integer ID for a UUID experiment.
    pub fn get_numeric_id(&self, uuid: &str) -> Option<i64> {
        self.id_to_numeric.get(uuid).map(|v| *v)
    }

    /// Get the UUID for a numeric ID.
    pub fn get_uuid_for_numeric(&self, numeric_id: i64) -> Option<String> {
        self.numeric_to_id.get(&numeric_id).map(|v| v.clone())
    }

    /// Look up an experiment by its integer ID.
    pub fn get_by_numeric_id(&self, numeric_id: i64) -> Result<Experiment, ExperimentError> {
        let uuid = self
            .get_uuid_for_numeric(numeric_id)
            .ok_or_else(|| ExperimentError::NotFound(numeric_id.to_string()))?;
        self.get(&uuid)
    }

    /// Returns the persisted experiment file modification time (ms since Unix epoch).
    pub fn get_last_updated_ms(&self, id: &str) -> Option<i64> {
        let path = self.dir.join(format!("{id}.json"));
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;
        let millis = modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis();
        i64::try_from(millis).ok()
    }

    /// Serialize an experiment and durably publish it as `<id>.json` through the
    /// canonical atomic-write owner. This helper owns only path/data assembly;
    /// temp naming, `sync_all`, atomic rename, cleanup, and parent fsync belong
    /// to `crate::index::utils::atomic_write`.
    fn persist_experiment(&self, experiment: &Experiment) -> Result<(), ExperimentError> {
        self.persist_json(&format!("{}.json", experiment.id), experiment)
    }

    pub fn create(&self, experiment: Experiment) -> Result<Experiment, ExperimentError> {
        experiment.validate()?;
        if self.experiments.contains_key(&experiment.id) {
            return Err(ExperimentError::AlreadyExists(experiment.id));
        }
        self.register_active_if_running(&experiment)?;
        self.persist_experiment(&experiment)?;
        self.assign_numeric_id(&experiment.id);
        self.persist_id_map()?;
        self.experiments
            .insert(experiment.id.clone(), experiment.clone());
        Ok(experiment)
    }

    pub fn get(&self, id: &str) -> Result<Experiment, ExperimentError> {
        self.experiments
            .get(id)
            .map(|e| e.clone())
            .ok_or_else(|| ExperimentError::NotFound(id.to_string()))
    }

    /// Return all experiments, optionally filtered by index name and/or status.
    ///
    /// # Arguments
    ///
    /// * `filter` — When `None`, returns every experiment. When `Some`, only experiments matching
    ///   all specified filter fields are included.
    pub fn list(&self, filter: Option<ExperimentFilter>) -> Vec<Experiment> {
        self.experiments
            .iter()
            .filter(|entry| {
                if let Some(ref f) = filter {
                    if let Some(ref idx) = f.index_name {
                        if &entry.value().index_name != idx {
                            return false;
                        }
                    }
                    if let Some(ref status) = f.status {
                        if &entry.value().status != status {
                            return false;
                        }
                    }
                }
                true
            })
            .map(|entry| entry.value().clone())
            .collect()
    }

    pub fn update(&self, experiment: Experiment) -> Result<Experiment, ExperimentError> {
        let existing = self.get(&experiment.id)?;
        if existing.status != ExperimentStatus::Draft {
            return Err(ExperimentError::InvalidStatus(format!(
                "{:?}",
                existing.status
            )));
        }
        experiment.validate()?;
        self.persist_experiment(&experiment)?;
        self.experiments
            .insert(experiment.id.clone(), experiment.clone());
        Ok(experiment)
    }

    /// Transition a draft experiment to `Running` status and register it as active for its index.
    ///
    /// Sets `started_at` to the current wall-clock time.
    ///
    /// # Errors
    ///
    /// Returns `InvalidStatus` if the experiment is not in `Draft` status or if another
    /// experiment is already running on the same index.
    pub fn start(&self, id: &str) -> Result<Experiment, ExperimentError> {
        let mut experiment = self.get(id)?;
        if experiment.status != ExperimentStatus::Draft {
            return Err(ExperimentError::InvalidStatus(format!(
                "{:?}",
                experiment.status
            )));
        }
        // Prevent multiple active experiments on the same index
        if self.get_active_for_index(&experiment.index_name).is_some() {
            return Err(ExperimentError::InvalidStatus(format!(
                "index '{}' already has a running experiment",
                experiment.index_name
            )));
        }
        experiment.status = ExperimentStatus::Running;
        experiment.started_at = Some(now_ms());
        self.persist_experiment(&experiment)?;
        self.experiments.insert(id.to_string(), experiment.clone());
        self.active_by_index
            .insert(experiment.index_name.clone(), experiment.id.clone());
        Ok(experiment)
    }

    /// Transition an experiment from `Running` or `Draft` to `Stopped` status.
    ///
    /// Sets `stopped_at` to the current wall-clock time and removes the experiment from the
    /// active-by-index map if it was running.
    ///
    /// # Errors
    ///
    /// Returns `InvalidStatus` if the experiment is already `Stopped` or `Concluded`.
    pub fn stop(&self, id: &str) -> Result<Experiment, ExperimentError> {
        let mut experiment = self.get(id)?;
        if experiment.status != ExperimentStatus::Running
            && experiment.status != ExperimentStatus::Draft
        {
            return Err(ExperimentError::InvalidStatus(format!(
                "{:?}",
                experiment.status
            )));
        }
        let running_snapshot = experiment.clone();
        experiment.status = ExperimentStatus::Stopped;
        experiment.stopped_at = Some(now_ms());
        self.persist_experiment(&experiment)?;
        self.experiments.insert(id.to_string(), experiment.clone());
        self.unregister_active_if_running(&running_snapshot);
        Ok(experiment)
    }

    /// Transition an experiment to `Concluded` status and attach the statistical conclusion.
    ///
    /// Accepts experiments in `Running` or `Stopped` status. If the experiment is still running,
    /// it is unregistered from the active-by-index map. The `stopped_at` timestamp is preserved
    /// if already set (from a prior stop), otherwise set to now.
    ///
    /// # Arguments
    ///
    /// * `id` — Experiment UUID.
    /// * `conclusion` — Statistical results and winner declaration.
    ///
    /// # Returns
    ///
    /// The updated experiment with `Concluded` status.
    ///
    /// # Errors
    ///
    /// Returns `InvalidStatus` if the experiment is in `Draft` or already `Concluded`.
    pub fn conclude(
        &self,
        id: &str,
        conclusion: ExperimentConclusion,
    ) -> Result<Experiment, ExperimentError> {
        let mut experiment = self.get(id)?;
        if experiment.status != ExperimentStatus::Running
            && experiment.status != ExperimentStatus::Stopped
        {
            return Err(ExperimentError::InvalidStatus(format!(
                "{:?}",
                experiment.status
            )));
        }
        if experiment.status == ExperimentStatus::Running {
            self.unregister_active_if_running(&experiment);
        }
        experiment.status = ExperimentStatus::Concluded;
        if experiment.stopped_at.is_none() {
            experiment.stopped_at = Some(now_ms());
        }
        experiment.conclusion = Some(conclusion);
        self.persist_experiment(&experiment)?;
        self.experiments.insert(id.to_string(), experiment.clone());
        Ok(experiment)
    }

    /// Remove an experiment from the store, its on-disk JSON file, and the numeric ID mapping.
    ///
    /// # Errors
    ///
    /// Returns `InvalidStatus` if the experiment is currently `Running` (stop it first).
    /// Returns `NotFound` if no experiment with the given ID exists.
    pub fn delete(&self, id: &str) -> Result<(), ExperimentError> {
        let experiment = self.get(id)?;
        if experiment.status == ExperimentStatus::Running {
            return Err(ExperimentError::InvalidStatus(format!(
                "{:?}",
                experiment.status
            )));
        }
        let path = self.dir.join(format!("{}.json", id));
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        if let Some((_, removed)) = self.experiments.remove(id) {
            self.unregister_active_if_running(&removed);
        }
        // Clean up numeric ID mapping.
        if let Some((_, numeric)) = self.id_to_numeric.remove(id) {
            self.numeric_to_id.remove(&numeric);
        }
        self.persist_id_map()?;
        Ok(())
    }

    pub fn get_active_for_index(&self, index_name: &str) -> Option<Experiment> {
        let active_id = self.active_by_index.get(index_name)?.value().clone();
        let experiment = self.experiments.get(&active_id).map(|entry| entry.clone());
        match experiment {
            Some(exp)
                if exp.status == ExperimentStatus::Running && exp.index_name == index_name =>
            {
                Some(exp)
            }
            _ => {
                self.active_by_index.remove(index_name);
                None
            }
        }
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
