//! On-disk persistence for per-tenant dictionary data (stopwords, plurals, compounds, settings) using atomic temp-file-plus-rename writes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{
    CompoundEntry, DictionaryError, DictionaryName, DictionarySettings, PluralEntry, StopwordEntry,
};

pub(crate) const DICTIONARIES_DIR: &str = ".dictionaries";

/// Manages on-disk storage for one tenant's dictionaries.
pub struct DictionaryStore {
    dir: PathBuf,
}

impl DictionaryStore {
    /// Create a new store rooted at the given directory.
    /// Creates the directory if it doesn't exist.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, DictionaryError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Construct the store path for a given tenant under a data directory.
    pub fn for_tenant(data_dir: &Path, tenant_id: &str) -> Result<Self, DictionaryError> {
        let dir = data_dir.join(tenant_id).join(DICTIONARIES_DIR);
        Self::new(dir)
    }

    fn file_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.json", name))
    }

    // ── Atomic write helper ───────────────────────────────────────────

    fn atomic_write(&self, name: &str, data: &[u8]) -> Result<(), DictionaryError> {
        let target = self.file_path(name);
        let tmp = self.dir.join(format!(".{}.tmp", name));
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, &target)?;
        Ok(())
    }

    fn read_file(&self, name: &str) -> Result<Option<Vec<u8>>, DictionaryError> {
        let path = self.file_path(name);
        match std::fs::read(&path) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ── Stopwords ─────────────────────────────────────────────────────

    pub fn load_stopwords(&self) -> Result<Vec<StopwordEntry>, DictionaryError> {
        match self.read_file("stopwords")? {
            Some(data) => Ok(serde_json::from_slice(&data)?),
            None => Ok(Vec::new()),
        }
    }

    pub fn save_stopwords(&self, entries: &[StopwordEntry]) -> Result<(), DictionaryError> {
        let data = serde_json::to_vec_pretty(entries)?;
        self.atomic_write("stopwords", &data)
    }

    // ── Plurals ───────────────────────────────────────────────────────

    pub fn load_plurals(&self) -> Result<Vec<PluralEntry>, DictionaryError> {
        match self.read_file("plurals")? {
            Some(data) => Ok(serde_json::from_slice(&data)?),
            None => Ok(Vec::new()),
        }
    }

    pub fn save_plurals(&self, entries: &[PluralEntry]) -> Result<(), DictionaryError> {
        let data = serde_json::to_vec_pretty(entries)?;
        self.atomic_write("plurals", &data)
    }

    // ── Compounds ─────────────────────────────────────────────────────

    pub fn load_compounds(&self) -> Result<Vec<CompoundEntry>, DictionaryError> {
        match self.read_file("compounds")? {
            Some(data) => Ok(serde_json::from_slice(&data)?),
            None => Ok(Vec::new()),
        }
    }

    pub fn save_compounds(&self, entries: &[CompoundEntry]) -> Result<(), DictionaryError> {
        let data = serde_json::to_vec_pretty(entries)?;
        self.atomic_write("compounds", &data)
    }

    // ── Settings ──────────────────────────────────────────────────────

    pub fn load_settings(&self) -> Result<DictionarySettings, DictionaryError> {
        match self.read_file("settings")? {
            Some(data) => Ok(serde_json::from_slice(&data)?),
            None => Ok(DictionarySettings::default()),
        }
    }

    pub fn save_settings(&self, settings: &DictionarySettings) -> Result<(), DictionaryError> {
        let data = serde_json::to_vec_pretty(settings)?;
        self.atomic_write("settings", &data)
    }

    // ── Convenience: count entries per language ────────────────────────

    /// Load all dictionary files and return per-language entry counts grouped by dictionary name.
    ///
    /// Iterates stopwords, plurals, and compounds, bucketing each entry by its
    /// `language` field. Languages with zero entries across all dictionaries are
    /// not included in the result.
    ///
    /// # Returns
    ///
    /// A nested map of `language → dictionary_name → count`.
    pub fn count_entries_by_language(
        &self,
    ) -> Result<HashMap<String, HashMap<DictionaryName, usize>>, DictionaryError> {
        let mut counts: HashMap<String, HashMap<DictionaryName, usize>> = HashMap::new();

        for entry in self.load_stopwords()? {
            *counts
                .entry(entry.language.clone())
                .or_default()
                .entry(DictionaryName::Stopwords)
                .or_default() += 1;
        }
        for entry in self.load_plurals()? {
            *counts
                .entry(entry.language.clone())
                .or_default()
                .entry(DictionaryName::Plurals)
                .or_default() += 1;
        }
        for entry in self.load_compounds()? {
            *counts
                .entry(entry.language.clone())
                .or_default()
                .entry(DictionaryName::Compounds)
                .or_default() += 1;
        }

        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionaries::{EntryState, EntryType};
    use tempfile::TempDir;

    fn make_store() -> (TempDir, DictionaryStore) {
        let tmp = TempDir::new().unwrap();
        let store = DictionaryStore::new(tmp.path().join("dicts")).unwrap();
        (tmp, store)
    }

    // ── Round-trip tests ──────────────────────────────────────────────

    /// Verify that stopword entries survive a save-then-load cycle and that an empty store returns no entries.
    #[test]
    fn test_stopwords_roundtrip() {
        let (_tmp, store) = make_store();

        // Empty initially
        assert!(store.load_stopwords().unwrap().is_empty());

        let entries = vec![
            StopwordEntry {
                object_id: "sw-1".into(),
                language: "en".into(),
                word: "the".into(),
                state: EntryState::Enabled,
                entry_type: EntryType::Custom,
            },
            StopwordEntry {
                object_id: "sw-2".into(),
                language: "fr".into(),
                word: "le".into(),
                state: EntryState::Enabled,
                entry_type: EntryType::Custom,
            },
        ];
        store.save_stopwords(&entries).unwrap();

        let loaded = store.load_stopwords().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].object_id, "sw-1");
        assert_eq!(loaded[1].word, "le");
    }

    /// Verify that plural entries survive a save-then-load cycle with their word lists intact.
    #[test]
    fn test_plurals_roundtrip() {
        let (_tmp, store) = make_store();
        assert!(store.load_plurals().unwrap().is_empty());

        let entries = vec![PluralEntry {
            object_id: "pl-1".into(),
            language: "en".into(),
            words: vec!["mouse".into(), "mice".into()],
            entry_type: EntryType::Custom,
        }];
        store.save_plurals(&entries).unwrap();

        let loaded = store.load_plurals().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].words, vec!["mouse", "mice"]);
    }

    /// Verify that compound entries survive a save-then-load cycle with their decomposition lists intact.
    #[test]
    fn test_compounds_roundtrip() {
        let (_tmp, store) = make_store();
        assert!(store.load_compounds().unwrap().is_empty());

        let entries = vec![CompoundEntry {
            object_id: "cp-1".into(),
            language: "de".into(),
            word: "Lebensversicherung".into(),
            decomposition: vec!["Leben".into(), "Versicherung".into()],
            entry_type: EntryType::Custom,
        }];
        store.save_compounds(&entries).unwrap();

        let loaded = store.load_compounds().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].decomposition, vec!["Leben", "Versicherung"]);
    }

    /// Verify that `DictionarySettings` round-trips through JSON, including per-language standard-entry disable flags.
    #[test]
    fn test_settings_roundtrip() {
        let (_tmp, store) = make_store();

        // Default when no file
        let settings = store.load_settings().unwrap();
        assert!(settings.disable_standard_entries.is_empty());

        let mut settings = DictionarySettings::default();
        settings.disable_standard_entries.insert(
            DictionaryName::Stopwords,
            [("fr".to_string(), true), ("en".to_string(), false)]
                .into_iter()
                .collect(),
        );
        settings.disable_standard_entries.insert(
            DictionaryName::Plurals,
            [("de".to_string(), true)].into_iter().collect(),
        );
        store.save_settings(&settings).unwrap();

        let loaded = store.load_settings().unwrap();
        assert_eq!(loaded, settings);
        assert!(loaded.is_standard_disabled(DictionaryName::Stopwords, "fr"));
        assert!(!loaded.is_standard_disabled(DictionaryName::Stopwords, "en"));
    }

    // ── Atomic write test ─────────────────────────────────────────────

    /// Verify that no temporary file remains on disk after an atomic write completes.
    #[test]
    fn test_atomic_write_no_partial() {
        let (_tmp, store) = make_store();

        // Write some entries
        let entries = vec![StopwordEntry {
            object_id: "sw-1".into(),
            language: "en".into(),
            word: "the".into(),
            state: EntryState::Enabled,
            entry_type: EntryType::Custom,
        }];
        store.save_stopwords(&entries).unwrap();

        // No temp file should remain
        let tmp_path = store.dir.join(".stopwords.tmp");
        assert!(
            !tmp_path.exists(),
            "temp file should be cleaned up after atomic write"
        );
    }

    // ── Overwrite test ────────────────────────────────────────────────

    /// Verify that saving new entries fully replaces the previous contents rather than appending.
    #[test]
    fn test_overwrite_replaces_entries() {
        let (_tmp, store) = make_store();

        let entries1 = vec![StopwordEntry {
            object_id: "sw-1".into(),
            language: "en".into(),
            word: "the".into(),
            state: EntryState::Enabled,
            entry_type: EntryType::Custom,
        }];
        store.save_stopwords(&entries1).unwrap();

        let entries2 = vec![StopwordEntry {
            object_id: "sw-2".into(),
            language: "fr".into(),
            word: "le".into(),
            state: EntryState::Enabled,
            entry_type: EntryType::Custom,
        }];
        store.save_stopwords(&entries2).unwrap();

        let loaded = store.load_stopwords().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].object_id, "sw-2");
    }

    // ── Count by language ─────────────────────────────────────────────

    /// Verify that `count_entries_by_language` tallies stopwords, plurals, and compounds per language correctly and omits languages with no entries.
    #[test]
    fn test_count_entries_by_language() {
        let (_tmp, store) = make_store();

        store
            .save_stopwords(&[
                StopwordEntry {
                    object_id: "sw-1".into(),
                    language: "en".into(),
                    word: "the".into(),
                    state: EntryState::Enabled,
                    entry_type: EntryType::Custom,
                },
                StopwordEntry {
                    object_id: "sw-2".into(),
                    language: "en".into(),
                    word: "a".into(),
                    state: EntryState::Enabled,
                    entry_type: EntryType::Custom,
                },
                StopwordEntry {
                    object_id: "sw-3".into(),
                    language: "fr".into(),
                    word: "le".into(),
                    state: EntryState::Enabled,
                    entry_type: EntryType::Custom,
                },
            ])
            .unwrap();
        store
            .save_plurals(&[PluralEntry {
                object_id: "pl-1".into(),
                language: "en".into(),
                words: vec!["mouse".into(), "mice".into()],
                entry_type: EntryType::Custom,
            }])
            .unwrap();

        let counts = store.count_entries_by_language().unwrap();
        assert_eq!(
            counts["en"][&DictionaryName::Stopwords],
            2,
            "en should have 2 stopwords"
        );
        assert_eq!(
            counts["fr"][&DictionaryName::Stopwords],
            1,
            "fr should have 1 stopword"
        );
        assert_eq!(
            counts["en"][&DictionaryName::Plurals],
            1,
            "en should have 1 plural"
        );
        assert!(
            !counts.contains_key("de"),
            "de should not appear (no entries)"
        );
    }

    // ── For-tenant constructor ────────────────────────────────────────

    #[test]
    fn test_for_tenant_creates_dir() {
        let tmp = TempDir::new().unwrap();
        let store = DictionaryStore::for_tenant(tmp.path(), "my-tenant").unwrap();
        assert!(store.dir.exists());
        assert!(store.dir.ends_with("my-tenant/.dictionaries"));
    }
}
