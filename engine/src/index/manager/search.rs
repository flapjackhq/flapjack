use super::*;

impl super::IndexManager {
    /// Search within a tenant's index.
    ///
    /// # Arguments
    /// * `tenant_id` - Tenant identifier
    /// * `query_text` - Search query string
    /// * `filter` - Optional filter to apply
    /// * `sort` - Optional sort specification
    /// * `limit` - Maximum number of results
    pub fn search(
        &self,
        tenant_id: &str,
        query_text: &str,
        filter: Option<&Filter>,
        sort: Option<&Sort>,
        limit: usize,
    ) -> Result<SearchResult> {
        self.search_with_options(
            tenant_id,
            query_text,
            &SearchOptions {
                filter,
                sort,
                limit,
                ..Default::default()
            },
        )
    }

    pub fn search_with_options(
        &self,
        tenant_id: &str,
        query_text: &str,
        opts: &SearchOptions<'_>,
    ) -> Result<SearchResult> {
        self.search_full_with_stop_words_with_hits_per_page_cap(tenant_id, query_text, opts)
    }

    /// Top-level search entry point: resolve tenant settings, apply stop-word-aware
    /// query expansion, enforce `hitsPerPage` cap, and delegate to the query executor.
    pub fn search_full_with_stop_words_with_hits_per_page_cap(
        &self,
        tenant_id: &str,
        query_text: &str,
        opts: &SearchOptions<'_>,
    ) -> Result<SearchResult> {
        let resolved = self.resolve_search_settings(tenant_id, opts.settings_override)?;
        let application_id = opts
            .dictionary_application_id
            .unwrap_or(crate::dictionaries::DEFAULT_DICTIONARY_APPLICATION_ID);
        let captured = if opts.dictionary_snapshot.is_none() {
            self.dictionary_manager()
                .map(|manager| manager.snapshot(application_id))
                .transpose()
                .map_err(|error| {
                    FlapjackError::ApplicationConfigurationUnavailable(error.to_string())
                })?
        } else {
            None
        };
        let pinned = SearchOptions {
            dictionary_snapshot: opts.dictionary_snapshot.or(captured.as_ref()),
            ..*opts
        };
        let opts = &pinned;
        if let Some(snapshot) = opts.dictionary_snapshot {
            snapshot
                .require_application(application_id)
                .map_err(|error| FlapjackError::Io(error.to_string()))?;
        }
        let preprocessed = search_phases::preprocess_query(
            application_id,
            &resolved.settings,
            query_text,
            opts,
            opts.dictionary_snapshot,
        );
        let normalized_query = preprocessed.query_text_stopped.as_str();

        let prepared = search_phases::prepare_search_filters(
            self,
            tenant_id,
            normalized_query,
            &resolved,
            &preprocessed,
            opts,
        )?;
        let result = search_phases::execute_search_query(
            self,
            tenant_id,
            normalized_query,
            &resolved,
            &preprocessed,
            &prepared,
            opts,
        )?;

        let remove_strategy = opts
            .remove_words_if_no_results
            .or(resolved
                .settings
                .as_ref()
                .map(|s| s.remove_words_if_no_results.as_str()))
            .unwrap_or("none");

        if result.total == 0
            && result.documents.is_empty()
            && remove_strategy != "none"
            && !normalized_query.trim().is_empty()
        {
            if let Some(retry) = search_phases::apply_remove_words_fallback(
                self,
                tenant_id,
                normalized_query,
                remove_strategy,
                opts,
            ) {
                return Ok(retry);
            }
        }

        Ok(result)
    }
}
