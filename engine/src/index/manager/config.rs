use super::*;

pub(crate) const SETTINGS_FILE: &str = "settings.json";
pub(crate) const RULES_FILE: &str = "rules.json";
pub(crate) const SYNONYMS_FILE: &str = "synonyms.json";

impl super::IndexManager {
    fn tenant_dir_if_valid(&self, tenant_id: &str) -> Option<PathBuf> {
        if let Err(e) = validate_index_name(tenant_id) {
            tracing::warn!("Rejected invalid tenant/index name '{}': {}", tenant_id, e);
            return None;
        }
        Some(self.base_path.join(tenant_id))
    }

    pub fn get_settings(&self, tenant_id: &str) -> Option<Arc<IndexSettings>> {
        if let Some(cached) = self.settings_cache.get(tenant_id) {
            return Some(Arc::clone(&cached));
        }
        let path = self.tenant_dir_if_valid(tenant_id)?.join(SETTINGS_FILE);
        if path.exists() {
            if let Ok(s) = IndexSettings::load(&path) {
                let arc = Arc::new(s);
                self.settings_cache
                    .insert(tenant_id.to_string(), Arc::clone(&arc));
                return Some(arc);
            }
        }
        None
    }

    pub fn get_rules(&self, tenant_id: &str) -> Option<Arc<RuleStore>> {
        if let Some(cached) = self.rules_cache.get(tenant_id) {
            return Some(Arc::clone(&cached));
        }
        let path = self.tenant_dir_if_valid(tenant_id)?.join(RULES_FILE);
        if path.exists() {
            if let Ok(s) = RuleStore::load(&path) {
                let arc = Arc::new(s);
                self.rules_cache
                    .insert(tenant_id.to_string(), Arc::clone(&arc));
                return Some(arc);
            }
        }
        None
    }

    pub fn get_synonyms(&self, tenant_id: &str) -> Option<Arc<SynonymStore>> {
        if let Some(cached) = self.synonyms_cache.get(tenant_id) {
            return Some(Arc::clone(&cached));
        }
        let path = self.tenant_dir_if_valid(tenant_id)?.join(SYNONYMS_FILE);
        if path.exists() {
            if let Ok(s) = SynonymStore::load(&path) {
                let arc = Arc::new(s);
                self.synonyms_cache
                    .insert(tenant_id.to_string(), Arc::clone(&arc));
                return Some(arc);
            }
        }
        None
    }

    /// Remove the cached settings for a tenant and reload languages/separators
    /// from disk so subsequent searches pick up the latest configuration.
    pub fn invalidate_settings_cache(&self, tenant_id: &str) {
        self.settings_cache.remove(tenant_id);
        if let Some(index) = self.loaded.get(tenant_id) {
            let Some(path) = self.tenant_dir_if_valid(tenant_id) else {
                return;
            };
            let index_languages = Self::read_index_languages(&path);
            let indexed_separators = Self::read_indexed_separators(&path);
            let keep_diacritics_on_characters = Self::read_keep_diacritics_on_characters(&path);
            let custom_normalization = Self::read_custom_normalization(&path);
            index.reconfigure_tokenizers(
                &index_languages,
                &indexed_separators,
                &keep_diacritics_on_characters,
                &custom_normalization,
            );
        }
    }

    pub fn invalidate_rules_cache(&self, tenant_id: &str) {
        self.rules_cache.remove(tenant_id);
    }

    pub fn invalidate_synonyms_cache(&self, tenant_id: &str) {
        self.synonyms_cache.remove(tenant_id);
    }

    pub fn invalidate_facet_cache(&self, tenant_id: &str) {
        let prefix = format!("{}:", tenant_id);
        self.facet_cache
            .retain(|cache_key, _| !cache_key.starts_with(&prefix));
    }

    /// Read `indexLanguages` from settings.json at the given index path, for tokenizer selection.
    pub(super) fn read_index_languages(path: &std::path::Path) -> Vec<String> {
        let settings_path = path.join(SETTINGS_FILE);
        if settings_path.exists() {
            if let Ok(settings) = IndexSettings::load(&settings_path) {
                return settings.index_languages;
            }
        }
        Vec::new()
    }

    /// Read `separatorsToIndex` from settings.json at the given index path.
    pub(super) fn read_indexed_separators(path: &std::path::Path) -> Vec<char> {
        let settings_path = path.join(SETTINGS_FILE);
        if settings_path.exists() {
            if let Ok(settings) = IndexSettings::load(&settings_path) {
                return settings.separators_to_index.chars().collect();
            }
        }
        Vec::new()
    }

    pub(super) fn read_keep_diacritics_on_characters(path: &std::path::Path) -> String {
        let settings_path = path.join(SETTINGS_FILE);
        if settings_path.exists() {
            if let Ok(settings) = IndexSettings::load(&settings_path) {
                return settings.keep_diacritics_on_characters;
            }
        }
        String::new()
    }

    pub(super) fn read_custom_normalization(path: &std::path::Path) -> Vec<(char, String)> {
        let settings_path = path.join(SETTINGS_FILE);
        if settings_path.exists() {
            if let Ok(settings) = IndexSettings::load(&settings_path) {
                return IndexSettings::flatten_custom_normalization(&settings);
            }
        }
        Vec::new()
    }
}
