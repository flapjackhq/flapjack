//! On-disk persistence for application dictionary data (stopwords, plurals, compounds, settings) using atomic temp-file-plus-rename writes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{
    CompoundEntry, DictionaryError, DictionaryName, DictionarySettings, PluralEntry, StopwordEntry,
};

pub(crate) const DICTIONARIES_DIR: &str = ".dictionaries";
pub(crate) const APPLICATIONS_DIR: &str = ".applications";

/// Manages on-disk storage for one application's dictionaries.
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

    /// Reject application IDs that could cause path traversal or filesystem issues.
    fn validate_application_id(application_id: &str) -> Result<(), DictionaryError> {
        if application_id.is_empty() || application_id == "." {
            return Err(DictionaryError::InvalidEntry(
                "application ID must name a nonempty path component other than '.'".to_string(),
            ));
        }
        if application_id.contains("..")
            || application_id.contains('/')
            || application_id.contains('\\')
            || application_id.contains('\0')
        {
            return Err(DictionaryError::InvalidEntry(
                "application ID contains invalid characters".to_string(),
            ));
        }
        Ok(())
    }

    /// Dictionaries are application-owned, never an index child.
    pub fn for_application(data_dir: &Path, application_id: &str) -> Result<Self, DictionaryError> {
        Self::validate_application_id(application_id)?;
        if application_id == APPLICATIONS_DIR {
            Self::validate_applications_directory(data_dir)?;
        } else {
            Self::reject_legacy_index_directory(&data_dir.join(application_id))?;
        }
        let applications = data_dir.join(APPLICATIONS_DIR);
        let application = applications.join(application_id);
        let dir = application.join(DICTIONARIES_DIR);
        for path in [&applications, &application, &dir] {
            match std::fs::symlink_metadata(path) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "application storage must be a real directory: {}",
                            path.display()
                        ),
                    )
                    .into())
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Self { dir })
    }

    /// Old index-shaped dictionary storage has no unambiguous application owner.
    /// Never reinterpret, migrate or discard it during admission.
    pub fn reject_legacy_index_directory(path: &Path) -> std::io::Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
            _ => {}
        }
        let legacy = path.join(DICTIONARIES_DIR);
        match std::fs::symlink_metadata(&legacy) {
            Ok(_) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                format!("unsupported legacy dictionary layout at {}; application dictionaries belong under .applications/<application-id>/.dictionaries; data was not moved or deleted", legacy.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Validate the explicit application namespace, including obsolete layouts
    /// whose application ID happened to be `.applications`.
    fn validate_applications_directory(data_dir: &Path) -> std::io::Result<()> {
        let applications = data_dir.join(APPLICATIONS_DIR);
        match std::fs::symlink_metadata(&applications) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    ".applications must be a real directory",
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        for application in applications.read_dir()? {
            let application = application?;
            if !application.file_type()?.is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "application storage must be a real directory: {}",
                        application.path().display()
                    ),
                ));
            }
            for artifact in application.path().read_dir()? {
                let artifact = artifact?;
                if artifact.file_name() != DICTIONARIES_DIR || !artifact.file_type()?.is_dir() {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                        format!("unsupported application storage layout at {}; expected .applications/<application-id>/.dictionaries; data was not moved or deleted", artifact.path().display())));
                }
            }
        }
        Ok(())
    }

    /// Read-only startup/inventory validation also catches obsolete reserved IDs.
    pub fn validate_data_layout(data_dir: &Path) -> std::io::Result<()> {
        Self::validate_applications_directory(data_dir)?;
        let entries = match data_dir.read_dir() {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_name() != APPLICATIONS_DIR && entry.file_type()?.is_dir() {
                Self::reject_legacy_index_directory(&entry.path())?;
            }
        }
        Ok(())
    }

    fn file_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.json", name))
    }

    fn read_file(&self, name: &str) -> Result<Option<Vec<u8>>, DictionaryError> {
        let path = self.file_path(name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "dictionary artifact must be a regular file",
                )
                .into())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        match std::fs::read(&path) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn save_json<T: serde::Serialize + ?Sized>(
        &self,
        name: &str,
        value: &T,
    ) -> Result<(), DictionaryError> {
        let data = serde_json::to_vec_pretty(value)?;
        std::fs::create_dir_all(&self.dir)?;
        crate::index::utils::atomic_write(&self.file_path(name), &data)?;
        Ok(())
    }

    pub(crate) fn load_configuration(
        &self,
    ) -> Result<Option<super::manager::DictionarySnapshot>, DictionaryError> {
        match self.read_file("configuration")? {
            Some(data) => Ok(Some(serde_json::from_slice(&data)?)),
            None => Ok(None),
        }
    }

    pub(crate) fn save_configuration(
        &self,
        configuration: &super::manager::DictionarySnapshot,
    ) -> Result<(), DictionaryError> {
        // Publish the application namespace entries as well as the leaf file.
        // Read paths never create directories; retries resync existing parents.
        let application = self
            .dir
            .parent()
            .ok_or_else(|| std::io::Error::other("dictionary application parent missing"))?;
        let applications = application
            .parent()
            .ok_or_else(|| std::io::Error::other("application namespace parent missing"))?;
        for path in [applications, application, self.dir.as_path()] {
            crate::index::utils::ensure_private_directory(path)?;
        }
        self.save_json("configuration", configuration)
    }

    pub(crate) fn reject_managed_mutation(&self) -> Result<(), DictionaryError> {
        if self.load_configuration()?.is_some() {
            return Err(DictionaryError::InvalidEntry("managed application configuration must be changed through its durable control-plane task".into()));
        }
        Ok(())
    }

    // ── Stopwords ─────────────────────────────────────────────────────

    pub fn load_stopwords(&self) -> Result<Vec<StopwordEntry>, DictionaryError> {
        match self.read_file("stopwords")? {
            Some(data) => Ok(serde_json::from_slice(&data)?),
            None => Ok(Vec::new()),
        }
    }

    pub fn save_stopwords(&self, entries: &[StopwordEntry]) -> Result<(), DictionaryError> {
        self.save_json("stopwords", entries)
    }

    // ── Plurals ───────────────────────────────────────────────────────

    pub fn load_plurals(&self) -> Result<Vec<PluralEntry>, DictionaryError> {
        match self.read_file("plurals")? {
            Some(data) => Ok(serde_json::from_slice(&data)?),
            None => Ok(Vec::new()),
        }
    }

    pub fn save_plurals(&self, entries: &[PluralEntry]) -> Result<(), DictionaryError> {
        self.save_json("plurals", entries)
    }

    // ── Compounds ─────────────────────────────────────────────────────

    pub fn load_compounds(&self) -> Result<Vec<CompoundEntry>, DictionaryError> {
        match self.read_file("compounds")? {
            Some(data) => Ok(serde_json::from_slice(&data)?),
            None => Ok(Vec::new()),
        }
    }

    pub fn save_compounds(&self, entries: &[CompoundEntry]) -> Result<(), DictionaryError> {
        self.save_json("compounds", entries)
    }

    // ── Settings ──────────────────────────────────────────────────────

    pub fn load_settings(&self) -> Result<DictionarySettings, DictionaryError> {
        match self.read_file("settings")? {
            Some(data) => Ok(serde_json::from_slice(&data)?),
            None => Ok(DictionarySettings::default()),
        }
    }

    pub fn save_settings(&self, settings: &DictionarySettings) -> Result<(), DictionaryError> {
        self.save_json("settings", settings)
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

    #[test]
    fn dictionary_atomic_write_removes_temp_after_failed_rename() {
        let (_tmp, store) = make_store();
        let target_path = store.file_path("stopwords");
        std::fs::create_dir(&target_path).unwrap();

        let error = store
            .save_stopwords(&[])
            .expect_err("renaming a completed temp file over a directory must fail");

        assert!(matches!(error, DictionaryError::IoError(_)));
        assert!(
            target_path.is_dir(),
            "rename must leave the target directory intact"
        );
        assert!(
            !target_path.is_file(),
            "failed rename must not publish a file"
        );
        assert!(
            !store.dir.join(".stopwords.tmp").exists(),
            "failed rename must remove the completed dictionary temp file"
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

    // ── For-application constructor ────────────────────────────────────────

    #[test]
    fn test_for_application_reads_are_pure_and_writes_create_reserved_dir() {
        let tmp = TempDir::new().unwrap();
        let store = DictionaryStore::for_application(tmp.path(), "my-tenant").unwrap();
        assert!(!store.dir.exists());
        assert_eq!(
            store.load_settings().unwrap(),
            DictionarySettings::default()
        );
        assert!(
            !tmp.path().join(".applications").exists(),
            "reading missing state must not create artifacts"
        );
        assert!(store.dir.ends_with(".applications/my-tenant/.dictionaries"));
        store.save_settings(&DictionarySettings::default()).unwrap();
        assert!(store.dir.join("settings.json").is_file());
        assert_eq!(
            DictionaryStore::for_application(tmp.path(), "my-tenant")
                .unwrap()
                .load_settings()
                .unwrap(),
            DictionarySettings::default()
        );
    }
}
