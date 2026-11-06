use super::*;

impl DictionarySnapshot {
    /// Get effective stopwords for a language, merging built-in + custom.
    /// If `disableStandardEntries` is true for this language, only custom entries are returned.
    pub fn effective_stopwords(
        &self,
        application_id: &str,
        lang: &str,
    ) -> Result<std::collections::HashSet<String>, DictionaryError> {
        self.require_application(application_id)?;
        let settings = &self.settings;
        let mut words = std::collections::HashSet::new();

        // Add built-in stopwords unless disabled
        if !settings.is_standard_disabled(DictionaryName::Stopwords, lang) {
            if let Some(builtin) = crate::query::stopwords::stopwords_for_lang(lang) {
                words.extend(builtin.into_iter().map(|s| s.to_string()));
            }
        }

        // Load custom entries once, then partition into enabled/disabled
        let custom = &self.stopwords;
        for entry in custom {
            if entry.language == lang && entry.state == EntryState::Enabled {
                words.insert(entry.word.clone());
            }
        }
        // Remove words that have a disabled custom entry (overrides built-in too)
        for entry in custom {
            if entry.language == lang && entry.state == EntryState::Disabled {
                words.remove(&entry.word);
            }
        }

        Ok(words)
    }

    /// Get custom plural equivalence sets for a language.
    ///
    /// Built-in plurals are rule-based (via `expand_plurals_for_lang`) and not
    /// returned here. The query pipeline checks `is_standard_disabled` separately
    /// to decide whether to run the built-in expander.
    pub fn custom_plural_sets(
        &self,
        application_id: &str,
        lang: &str,
    ) -> Result<Vec<Vec<String>>, DictionaryError> {
        self.require_application(application_id)?;
        let custom = self.plurals.clone();
        Ok(custom
            .into_iter()
            .filter(|e| e.language == lang)
            .map(|e| e.words)
            .collect())
    }

    /// Check if the built-in plural expander should be used for this language.
    pub fn use_builtin_plurals(
        &self,
        application_id: &str,
        lang: &str,
    ) -> Result<bool, DictionaryError> {
        let settings = self.get_settings(application_id)?;
        Ok(!settings.is_standard_disabled(DictionaryName::Plurals, lang))
    }

    /// Get effective compound decompositions for a language, merging built-in + custom.
    pub fn effective_compounds(
        &self,
        application_id: &str,
        lang: &str,
    ) -> Result<HashMap<String, Vec<String>>, DictionaryError> {
        self.require_application(application_id)?;
        let mut decompositions: HashMap<String, Vec<String>> = HashMap::new();

        // Built-in decompound data is handled by the GermanDecompounder etc.
        // Custom entries override/augment. If standard is disabled, the query
        // pipeline should skip the built-in decompounder for this language.
        // We only return the custom decomposition map here; the pipeline
        // checks `is_standard_disabled` separately.

        let custom = self.compounds.clone();
        for entry in custom {
            if entry.language == lang {
                decompositions.insert(entry.word.clone(), entry.decomposition);
            }
        }

        Ok(decompositions)
    }

    /// Check if standard entries are disabled for a dictionary type + language.
    pub fn is_standard_disabled(
        &self,
        application_id: &str,
        dict_name: DictionaryName,
        lang: &str,
    ) -> Result<bool, DictionaryError> {
        let settings = self.get_settings(application_id)?;
        Ok(settings.is_standard_disabled(dict_name, lang))
    }
}

impl DictionaryManager {
    pub fn effective_stopwords(
        &self,
        application_id: &str,
        lang: &str,
    ) -> Result<std::collections::HashSet<String>, DictionaryError> {
        let snapshot = self.snapshot(application_id)?;
        #[cfg(test)]
        super::concurrency_tests::after_effective_settings();
        snapshot.effective_stopwords(application_id, lang)
    }
    pub fn custom_plural_sets(
        &self,
        application_id: &str,
        lang: &str,
    ) -> Result<Vec<Vec<String>>, DictionaryError> {
        self.snapshot(application_id)?
            .custom_plural_sets(application_id, lang)
    }
    pub fn use_builtin_plurals(
        &self,
        application_id: &str,
        lang: &str,
    ) -> Result<bool, DictionaryError> {
        self.snapshot(application_id)?
            .use_builtin_plurals(application_id, lang)
    }
    pub fn effective_compounds(
        &self,
        application_id: &str,
        lang: &str,
    ) -> Result<HashMap<String, Vec<String>>, DictionaryError> {
        self.snapshot(application_id)?
            .effective_compounds(application_id, lang)
    }
    pub fn is_standard_disabled(
        &self,
        application_id: &str,
        name: DictionaryName,
        lang: &str,
    ) -> Result<bool, DictionaryError> {
        self.snapshot(application_id)?
            .is_standard_disabled(application_id, name, lang)
    }
}
