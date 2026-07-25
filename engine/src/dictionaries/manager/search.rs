use super::*;

impl DictionaryManager {
    /// Search dictionary entries for a given tenant + dictionary type.
    pub fn search(
        &self,
        tenant_id: &str,
        dict_name: DictionaryName,
        request: &DictionarySearchRequest,
    ) -> Result<DictionarySearchResponse, DictionaryError> {
        let store = self.store_for(tenant_id)?;
        let page = request.page.unwrap_or(0);
        let hits_per_page = request.hits_per_page.unwrap_or(20).clamp(1, 1000);
        let query_lower = request.query.to_lowercase();
        let language = request
            .language
            .as_ref()
            .map(|lang| self.normalize_language(lang))
            .transpose()?;

        // Collect matching entries as JSON values
        let mut hits: Vec<serde_json::Value> = match dict_name {
            DictionaryName::Stopwords => {
                let entries = store.load_stopwords()?;
                entries
                    .into_iter()
                    .filter(|e| self.stopword_matches(e, &query_lower, &language))
                    .map(|e| serde_json::to_value(e).unwrap())
                    .collect()
            }
            DictionaryName::Plurals => {
                let entries = store.load_plurals()?;
                entries
                    .into_iter()
                    .filter(|e| self.plural_matches(e, &query_lower, &language))
                    .map(|e| serde_json::to_value(e).unwrap())
                    .collect()
            }
            DictionaryName::Compounds => {
                let entries = store.load_compounds()?;
                entries
                    .into_iter()
                    .filter(|e| self.compound_matches(e, &query_lower, &language))
                    .map(|e| serde_json::to_value(e).unwrap())
                    .collect()
            }
        };

        // Deterministic sort by objectID
        hits.sort_by(|a, b| {
            let a_id = a["objectID"].as_str().unwrap_or("");
            let b_id = b["objectID"].as_str().unwrap_or("");
            a_id.cmp(b_id)
        });

        let nb_hits = hits.len();
        let nb_pages = if nb_hits == 0 {
            0
        } else {
            nb_hits.div_ceil(hits_per_page)
        };

        // Paginate
        let start = page.saturating_mul(hits_per_page);
        let page_hits = if start < nb_hits {
            hits[start..(start + hits_per_page).min(nb_hits)].to_vec()
        } else {
            Vec::new()
        };

        Ok(DictionarySearchResponse {
            hits: page_hits,
            page,
            nb_hits,
            nb_pages,
        })
    }

    /// Return whether a stopword entry matches the search criteria.
    ///
    /// Matches when the entry's language equals the filter (if provided) and the lowercased word or objectID contains the query substring. An empty query matches all entries in the language.
    fn stopword_matches(
        &self,
        entry: &StopwordEntry,
        query_lower: &str,
        language: &Option<String>,
    ) -> bool {
        if let Some(lang) = language {
            if entry.language != *lang {
                return false;
            }
        }
        if query_lower.is_empty() {
            return true;
        }
        entry.word.to_lowercase().contains(query_lower)
            || entry.object_id.to_lowercase().contains(query_lower)
    }

    /// Return whether a plural entry matches the search criteria.
    ///
    /// Matches when the entry's language passes the filter and any word in the equivalence set, or the objectID, contains the query substring (case-insensitive).
    fn plural_matches(
        &self,
        entry: &PluralEntry,
        query_lower: &str,
        language: &Option<String>,
    ) -> bool {
        if let Some(lang) = language {
            if entry.language != *lang {
                return false;
            }
        }
        if query_lower.is_empty() {
            return true;
        }
        entry
            .words
            .iter()
            .any(|w| w.to_lowercase().contains(query_lower))
            || entry.object_id.to_lowercase().contains(query_lower)
    }

    /// Return whether a compound entry matches the search criteria.
    ///
    /// Matches when the entry's language passes the filter and the compound word, any decomposition part, or the objectID contains the query substring (case-insensitive).
    fn compound_matches(
        &self,
        entry: &CompoundEntry,
        query_lower: &str,
        language: &Option<String>,
    ) -> bool {
        if let Some(lang) = language {
            if entry.language != *lang {
                return false;
            }
        }
        if query_lower.is_empty() {
            return true;
        }
        entry.word.to_lowercase().contains(query_lower)
            || entry
                .decomposition
                .iter()
                .any(|d| d.to_lowercase().contains(query_lower))
            || entry.object_id.to_lowercase().contains(query_lower)
    }
}
