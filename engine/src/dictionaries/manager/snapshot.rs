use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One complete, identity-bound application generation. A search retains this
/// value through preprocessing and fallback while later delivery proceeds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DictionarySnapshot {
    pub application_id: String,
    pub revision: u64,
    pub content_sha256: String,
    pub settings: DictionarySettings,
    pub stopwords: Vec<StopwordEntry>,
    pub plurals: Vec<PluralEntry>,
    pub compounds: Vec<CompoundEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum DictionaryCommand {
    Batch {
        dictionary: DictionaryName,
        request: BatchDictionaryRequest,
    },
    Settings {
        settings: DictionarySettings,
    },
}

impl DictionarySnapshot {
    pub fn empty(application_id: &str) -> Result<Self, DictionaryError> {
        let mut snapshot = Self {
            application_id: application_id.into(),
            revision: 0,
            content_sha256: String::new(),
            settings: DictionarySettings::default(),
            stopwords: Vec::new(),
            plurals: Vec::new(),
            compounds: Vec::new(),
        };
        snapshot.seal()?;
        Ok(snapshot)
    }

    pub(crate) fn require_application(&self, application_id: &str) -> Result<(), DictionaryError> {
        if application_id != self.application_id {
            return Err(DictionaryError::InvalidEntry(
                "dictionary snapshot application mismatch".into(),
            ));
        }
        Ok(())
    }

    fn computed_sha256(&self) -> Result<String, DictionaryError> {
        let mut value = serde_json::to_value(self)?;
        value["content_sha256"] = "".into();
        // Settings are maps: their insertion/hash iteration order is not identity.
        value.sort_all_objects();
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)))
    }

    fn seal(&mut self) -> Result<(), DictionaryError> {
        self.content_sha256 = self.computed_sha256()?;
        Ok(())
    }

    pub fn get_settings(
        &self,
        application_id: &str,
    ) -> Result<DictionarySettings, DictionaryError> {
        self.require_application(application_id)?;
        Ok(self.settings.clone())
    }
}

impl DictionaryManager {
    pub fn snapshot(&self, application_id: &str) -> Result<DictionarySnapshot, DictionaryError> {
        self.capture_snapshot(application_id, None)
    }

    /// A managed request requires an explicit generation even for revision zero.
    /// Missing/corrupt files never become the valid empty/default configuration.
    pub fn snapshot_at_least(
        &self,
        application_id: &str,
        revision: u64,
    ) -> Result<DictionarySnapshot, DictionaryError> {
        self.capture_snapshot(application_id, Some(revision))
    }

    fn capture_snapshot(
        &self,
        application_id: &str,
        required: Option<u64>,
    ) -> Result<DictionarySnapshot, DictionaryError> {
        let lock = self.mutation_lock(application_id);
        let _guard = lock
            .lock()
            .map_err(|_| std::io::Error::other("dictionary mutation lock poisoned"))?;
        let store = self.store_for(application_id)?;
        if let Some(snapshot) = store.load_configuration()? {
            self.validate_snapshot(application_id, &snapshot)?;
            if required.is_some_and(|revision| snapshot.revision < revision) {
                return Err(DictionaryError::InvalidEntry(
                    "required application configuration is stale".into(),
                ));
            }
            return Ok(snapshot);
        }
        if required.is_some() {
            return Err(DictionaryError::InvalidEntry(
                "required application configuration is missing".into(),
            ));
        }
        let mut snapshot = DictionarySnapshot {
            application_id: application_id.into(),
            revision: 0,
            content_sha256: String::new(),
            settings: store.load_settings()?,
            stopwords: store.load_stopwords()?,
            plurals: store.load_plurals()?,
            compounds: store.load_compounds()?,
        };
        snapshot.seal()?;
        Ok(snapshot)
    }

    /// Validate a command against a supplied canonical base without touching
    /// local files or activating it. The control plane commits this result first.
    pub fn prepare_configuration(
        &self,
        application_id: &str,
        base: &DictionarySnapshot,
        revision: u64,
        command: &DictionaryCommand,
    ) -> Result<DictionarySnapshot, DictionaryError> {
        self.validate_snapshot(application_id, base)?;
        if revision <= base.revision {
            return Err(DictionaryError::InvalidEntry(
                "configuration revision must advance".into(),
            ));
        }
        let mut next = base.clone();
        match command {
            DictionaryCommand::Batch {
                dictionary,
                request,
            } => match dictionary {
                DictionaryName::Stopwords => self.apply_stopwords(&mut next.stopwords, request)?,
                DictionaryName::Plurals => self.apply_plurals(&mut next.plurals, request)?,
                DictionaryName::Compounds => self.apply_compounds(&mut next.compounds, request)?,
            },
            DictionaryCommand::Settings { settings } => {
                next.settings = self.normalize_settings(settings)?
            }
        }
        next.revision = revision;
        next.seal()?;
        Ok(next)
    }

    pub fn validate_snapshot(
        &self,
        application_id: &str,
        snapshot: &DictionarySnapshot,
    ) -> Result<(), DictionaryError> {
        snapshot.require_application(application_id)?;
        if snapshot.content_sha256 != snapshot.computed_sha256()? {
            return Err(DictionaryError::InvalidEntry(
                "application configuration digest mismatch".into(),
            ));
        }
        if self.normalize_settings(&snapshot.settings)? != snapshot.settings {
            return Err(DictionaryError::InvalidEntry(
                "application configuration settings are not normalized".into(),
            ));
        }
        for entries in [
            serde_json::to_value(&snapshot.stopwords)?,
            serde_json::to_value(&snapshot.plurals)?,
            serde_json::to_value(&snapshot.compounds)?,
        ] {
            let mut ids = std::collections::HashSet::new();
            for entry in entries.as_array().expect("serialized entries are arrays") {
                let id = entry["objectID"].as_str().unwrap_or_default();
                let language = entry["language"].as_str().unwrap_or_default();
                if id.is_empty()
                    || !ids.insert(id)
                    || self.normalize_language(language)? != language
                {
                    return Err(DictionaryError::InvalidEntry(
                        "application configuration contains invalid or duplicate entries".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Publish one durable generation under the same lock as local mutations.
    /// Exact retries are safe; older or conflicting authority never overwrites it.
    pub fn install_configuration(
        &self,
        application_id: &str,
        snapshot: &DictionarySnapshot,
    ) -> Result<(), DictionaryError> {
        self.validate_snapshot(application_id, snapshot)?;
        let lock = self.mutation_lock(application_id);
        let _guard = lock
            .lock()
            .map_err(|_| std::io::Error::other("dictionary mutation lock poisoned"))?;
        let store = self.store_for(application_id)?;
        if let Some(current) = store.load_configuration()? {
            self.validate_snapshot(application_id, &current)?;
            if current.revision > snapshot.revision
                || (current.revision == snapshot.revision && current != *snapshot)
            {
                return Err(DictionaryError::InvalidEntry(
                    "application configuration conflicts with installed revision".into(),
                ));
            }
            // Even an identical retry must finish durability after a rename
            // whose directory sync failed. Republish the envelope, not the command.
        } else if !store.load_stopwords()?.is_empty()
            || !store.load_plurals()?.is_empty()
            || !store.load_compounds()?.is_empty()
            || store.load_settings()? != DictionarySettings::default()
        {
            return Err(DictionaryError::InvalidEntry("unversioned application configuration requires explicit reconciliation; data was not overwritten".into()));
        }
        store.save_configuration(snapshot)
    }
}
