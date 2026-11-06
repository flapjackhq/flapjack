//! Provides the DictionaryManager, the single entry point for all dictionary operations including batch mutations, search, settings, and per-language counts across applications.

use dashmap::DashMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use super::persistence::DictionaryStore;
use super::{
    BatchAction, BatchDictionaryRequest, CompoundEntry, DictionaryCount, DictionaryError,
    DictionaryName, DictionarySearchRequest, DictionarySearchResponse, DictionarySettings,
    EntryState, LanguageDictionaryCounts, MutationResponse, PluralEntry, StopwordEntry,
};

mod batch;
mod effective;
mod search;
mod snapshot;
pub use snapshot::{DictionaryCommand, DictionarySnapshot};

/// Application-level dictionary manager.
///
/// In a multi-tenant setup, dictionaries are per-application. The manager uses the
/// data directory root and resolves per-application stores on demand.
/// Concurrent writers for a data root must share this manager (normally via Arc).
/// Mutation locks do not coordinate independent managers or other processes.
pub struct DictionaryManager {
    data_dir: PathBuf,
    next_task_id: AtomicI64,
    mutation_locks: DashMap<String, Arc<Mutex<()>>>,
}

impl DictionaryManager {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            next_task_id: AtomicI64::new(1),
            mutation_locks: DashMap::new(),
        }
    }

    fn store_for(&self, application_id: &str) -> Result<DictionaryStore, DictionaryError> {
        DictionaryStore::for_application(&self.data_dir, application_id)
    }

    fn mutation_lock(&self, application_id: &str) -> Arc<Mutex<()>> {
        let lock = self
            .mutation_locks
            .entry(application_id.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        // Test-only scheduling observes contention without holding a DashMap guard.
        #[cfg(test)]
        if matches!(lock.try_lock(), Err(std::sync::TryLockError::WouldBlock)) {
            concurrency_tests::after_batch_read();
        }
        lock
    }

    fn make_mutation_response(&self) -> MutationResponse {
        let task_id = self.next_task_id.fetch_add(1, Ordering::SeqCst);
        MutationResponse {
            task_id,
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    fn normalize_language(&self, raw: &str) -> Result<String, DictionaryError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(DictionaryError::InvalidEntry(
                "language is required".to_string(),
            ));
        }

        let parsed = trimmed
            .parse::<crate::language::LanguageCode>()
            .map_err(|_| {
                DictionaryError::InvalidEntry(format!("unsupported language code '{}'", raw))
            })?;
        Ok(parsed.as_str().to_string())
    }

    // ── Settings ──────────────────────────────────────────────────────

    pub fn get_settings(
        &self,
        application_id: &str,
    ) -> Result<DictionarySettings, DictionaryError> {
        Ok(self.snapshot(application_id)?.settings)
    }

    /// Persist dictionary settings for an application, normalizing all language codes.
    ///
    /// # Returns
    ///
    /// A `MutationResponse` with a monotonically increasing task ID and an RFC 3339 timestamp.
    pub fn set_settings(
        &self,
        application_id: &str,
        settings: &DictionarySettings,
    ) -> Result<MutationResponse, DictionaryError> {
        let normalized = self.normalize_settings(settings)?;

        let store = self.store_for(application_id)?;
        let lock = self.mutation_lock(application_id);
        let _guard = lock
            .lock()
            .map_err(|_| std::io::Error::other("dictionary mutation lock poisoned"))?;
        store.reject_managed_mutation()?;
        store.save_settings(&normalized)?;
        Ok(self.make_mutation_response())
    }

    pub(super) fn normalize_settings(
        &self,
        settings: &DictionarySettings,
    ) -> Result<DictionarySettings, DictionaryError> {
        let mut normalized = DictionarySettings::default();
        for (dict_name, language_map) in &settings.disable_standard_entries {
            let normalized_map = normalized
                .disable_standard_entries
                .entry(*dict_name)
                .or_default();
            for (language, disabled) in language_map {
                let normalized_language = self.normalize_language(language)?;
                normalized_map.insert(normalized_language, *disabled);
            }
        }

        Ok(normalized)
    }

    // ── Languages ─────────────────────────────────────────────────────

    /// Returns per-language custom entry counts across all dictionary types.
    pub fn list_languages(
        &self,
        application_id: &str,
    ) -> Result<HashMap<String, LanguageDictionaryCounts>, DictionaryError> {
        self.list_languages_snapshot(&self.snapshot(application_id)?)
    }

    pub fn list_languages_snapshot(
        &self,
        snapshot: &DictionarySnapshot,
    ) -> Result<HashMap<String, LanguageDictionaryCounts>, DictionaryError> {
        let mut counts: HashMap<String, HashMap<DictionaryName, usize>> = HashMap::new();
        for (name, languages) in [
            (
                DictionaryName::Stopwords,
                snapshot
                    .stopwords
                    .iter()
                    .map(|e| &e.language)
                    .collect::<Vec<_>>(),
            ),
            (
                DictionaryName::Plurals,
                snapshot
                    .plurals
                    .iter()
                    .map(|e| &e.language)
                    .collect::<Vec<_>>(),
            ),
            (
                DictionaryName::Compounds,
                snapshot
                    .compounds
                    .iter()
                    .map(|e| &e.language)
                    .collect::<Vec<_>>(),
            ),
        ] {
            for language in languages {
                *counts
                    .entry(language.clone())
                    .or_default()
                    .entry(name)
                    .or_default() += 1;
            }
        }

        let mut result = HashMap::new();
        for (lang, type_counts) in counts {
            let stopwords_count = type_counts.get(&DictionaryName::Stopwords).copied();
            let plurals_count = type_counts.get(&DictionaryName::Plurals).copied();
            let compounds_count = type_counts.get(&DictionaryName::Compounds).copied();

            result.insert(
                lang,
                LanguageDictionaryCounts {
                    stopwords: stopwords_count.map(|n| DictionaryCount {
                        nb_custom_entries: n,
                    }),
                    plurals: plurals_count.map(|n| DictionaryCount {
                        nb_custom_entries: n,
                    }),
                    compounds: compounds_count.map(|n| DictionaryCount {
                        nb_custom_entries: n,
                    }),
                },
            );
        }

        Ok(result)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
mod concurrency_tests;

#[cfg(test)]
mod configuration_tests;
