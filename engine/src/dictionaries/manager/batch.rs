use super::*;

impl DictionaryManager {
    /// Execute a batch of dictionary mutations for the given tenant + dictionary type.
    pub fn batch(
        &self,
        application_id: &str,
        dict_name: DictionaryName,
        request: &BatchDictionaryRequest,
    ) -> Result<MutationResponse, DictionaryError> {
        let store = self.store_for(application_id)?;
        // Hold through load, full batch validation, atomic publication and acknowledgement.
        // Other applications have independent locks and can continue writing.
        let lock = self.mutation_lock(application_id);
        let _guard = lock
            .lock()
            .map_err(|_| std::io::Error::other("dictionary mutation lock poisoned"))?;

        store.reject_managed_mutation()?;
        match dict_name {
            DictionaryName::Stopwords => self.batch_stopwords(&store, request)?,
            DictionaryName::Plurals => self.batch_plurals(&store, request)?,
            DictionaryName::Compounds => self.batch_compounds(&store, request)?,
        }

        Ok(self.make_mutation_response())
    }

    /// Apply a batch of stopword mutations (add/upsert/delete) to the given store.
    ///
    /// When `clear_existing_dictionary_entries` is set, all existing entries are discarded before processing the request list. Each added entry has its language code normalized and its `objectID` deduplicated via upsert semantics.
    fn batch_stopwords(
        &self,
        store: &DictionaryStore,
        request: &BatchDictionaryRequest,
    ) -> Result<(), DictionaryError> {
        let mut entries = store.load_stopwords()?;
        self.apply_stopwords(&mut entries, request)?;
        store.save_stopwords(&entries)
    }

    pub(super) fn apply_stopwords(
        &self,
        entries: &mut Vec<StopwordEntry>,
        request: &BatchDictionaryRequest,
    ) -> Result<(), DictionaryError> {
        if request.clear_existing_dictionary_entries {
            entries.clear();
        }

        #[cfg(test)]
        super::concurrency_tests::after_batch_read();

        for req in &request.requests {
            self.validate_object_id(&req.body)?;
            match req.action {
                BatchAction::AddEntry => {
                    let mut entry: StopwordEntry = serde_json::from_value(req.body.clone())
                        .map_err(|e| DictionaryError::InvalidEntry(e.to_string()))?;
                    entry.language = self.normalize_language(&entry.language)?;
                    // Upsert: remove existing entry with same objectID
                    entries.retain(|e| e.object_id != entry.object_id);
                    entries.push(entry);
                }
                BatchAction::DeleteEntry => {
                    let object_id = self.extract_object_id(&req.body)?;
                    entries.retain(|e| e.object_id != object_id);
                }
            }
        }

        Ok(())
    }

    /// Apply a batch of plural-set mutations (add/upsert/delete) to the given store.
    ///
    /// Behaves identically to `batch_stopwords` but operates on `PluralEntry` records. Language codes are normalized and `objectID` collisions are resolved by replacement.
    fn batch_plurals(
        &self,
        store: &DictionaryStore,
        request: &BatchDictionaryRequest,
    ) -> Result<(), DictionaryError> {
        let mut entries = store.load_plurals()?;
        self.apply_plurals(&mut entries, request)?;
        store.save_plurals(&entries)
    }

    pub(super) fn apply_plurals(
        &self,
        entries: &mut Vec<PluralEntry>,
        request: &BatchDictionaryRequest,
    ) -> Result<(), DictionaryError> {
        if request.clear_existing_dictionary_entries {
            entries.clear();
        }

        #[cfg(test)]
        super::concurrency_tests::after_batch_read();

        for req in &request.requests {
            self.validate_object_id(&req.body)?;
            match req.action {
                BatchAction::AddEntry => {
                    let mut entry: PluralEntry = serde_json::from_value(req.body.clone())
                        .map_err(|e| DictionaryError::InvalidEntry(e.to_string()))?;
                    entry.language = self.normalize_language(&entry.language)?;
                    entries.retain(|e| e.object_id != entry.object_id);
                    entries.push(entry);
                }
                BatchAction::DeleteEntry => {
                    let object_id = self.extract_object_id(&req.body)?;
                    entries.retain(|e| e.object_id != object_id);
                }
            }
        }

        Ok(())
    }

    /// Apply a batch of compound-word mutations (add/upsert/delete) to the given store.
    ///
    /// Behaves identically to `batch_stopwords` but operates on `CompoundEntry` records, each containing a word and its decomposition parts.
    fn batch_compounds(
        &self,
        store: &DictionaryStore,
        request: &BatchDictionaryRequest,
    ) -> Result<(), DictionaryError> {
        let mut entries = store.load_compounds()?;
        self.apply_compounds(&mut entries, request)?;
        store.save_compounds(&entries)
    }

    pub(super) fn apply_compounds(
        &self,
        entries: &mut Vec<CompoundEntry>,
        request: &BatchDictionaryRequest,
    ) -> Result<(), DictionaryError> {
        if request.clear_existing_dictionary_entries {
            entries.clear();
        }

        #[cfg(test)]
        super::concurrency_tests::after_batch_read();

        for req in &request.requests {
            self.validate_object_id(&req.body)?;
            match req.action {
                BatchAction::AddEntry => {
                    let mut entry: CompoundEntry = serde_json::from_value(req.body.clone())
                        .map_err(|e| DictionaryError::InvalidEntry(e.to_string()))?;
                    entry.language = self.normalize_language(&entry.language)?;
                    entries.retain(|e| e.object_id != entry.object_id);
                    entries.push(entry);
                }
                BatchAction::DeleteEntry => {
                    let object_id = self.extract_object_id(&req.body)?;
                    entries.retain(|e| e.object_id != object_id);
                }
            }
        }

        Ok(())
    }

    fn validate_object_id(&self, body: &serde_json::Value) -> Result<(), DictionaryError> {
        match body.get("objectID") {
            Some(serde_json::Value::String(s)) if !s.is_empty() => Ok(()),
            Some(serde_json::Value::String(_)) => Err(DictionaryError::MissingObjectId),
            _ => Err(DictionaryError::MissingObjectId),
        }
    }

    fn extract_object_id(&self, body: &serde_json::Value) -> Result<String, DictionaryError> {
        self.validate_object_id(body)?;
        Ok(body["objectID"].as_str().unwrap().to_string())
    }
}
