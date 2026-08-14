//! HTTP composition of the core-owned Algolia filter parsers.
use flapjack::error::FlapjackError;
use flapjack::query::algolia_filters::{
    facet_filters_to_ast, numeric_filters_to_ast, tag_filters_to_ast,
};
use flapjack::types::Filter;

impl super::SearchRequest {
    /// Merge `filters`, `facet_filters`, `numeric_filters`, and `tag_filters` into a single `Filter` AST.
    ///
    /// Each source is parsed independently; results are AND-ed together. The raw
    /// `filters` string is fail-closed: malformed input returns `InvalidQuery`
    /// instead of being silently dropped into an unfiltered search. Returns
    /// `Ok(None)` when no valid filters are present.
    pub fn build_combined_filter(&self) -> Result<Option<Filter>, FlapjackError> {
        let mut parts: Vec<Filter> = Vec::new();

        if let Some(ref filter_str) = self.filters {
            let parsed = crate::filter_parser::parse_filter(filter_str).map_err(|error| {
                FlapjackError::InvalidQuery(format!("Filter parse error: {error}"))
            })?;
            parts.push(parsed);
        }

        if let Some(ref ff) = self.facet_filters {
            if let Some(filter) = facet_filters_to_ast(ff) {
                parts.push(filter);
            }
        }

        if let Some(ref nf) = self.numeric_filters {
            if let Some(filter) = numeric_filters_to_ast(nf) {
                parts.push(filter);
            }
        }

        if let Some(ref tf) = self.tag_filters {
            if let Some(filter) = tag_filters_to_ast(tf) {
                parts.push(filter);
            }
        }

        match parts.len() {
            0 => Ok(None),
            1 => Ok(Some(parts.remove(0))),
            _ => Ok(Some(Filter::And(parts))),
        }
    }
}
