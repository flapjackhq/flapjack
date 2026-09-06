mod filter_parsing;
mod request_params;
mod response_types;

pub use response_types::*;

use flapjack::error::FlapjackError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

#[cfg(test)]
pub(crate) use flapjack::query::algolia_filters::{
    facet_filters_to_ast, numeric_filters_to_ast, parse_facet_filter_string,
    parse_numeric_filter_string, tag_filters_to_ast,
};
pub use flapjack::query::algolia_filters::{
    parse_optional_filters, parse_optional_filters_grouped,
};

// ── Algolia-compatible request limits ──────────────────────────────────────
/// Maximum query string length in bytes (Algolia enforces 512 bytes).
pub const MAX_QUERY_BYTES: usize = 512;
/// Maximum hits per page (Algolia caps at 1000).
pub const MAX_HITS_PER_PAGE: usize = 1_000;
/// Maximum reachable result offset (page * hitsPerPage). Algolia caps at 20 000.
pub const MAX_PAGINATION_OFFSET: usize = 20_000;
/// Maximum filter string length in bytes. Algolia allows up to 1000 filter
/// operations; we approximate with a generous byte cap on the raw string.
pub const MAX_FILTER_BYTES: usize = 4_096;
/// Maximum number of queries in a single batch (multi-index) search request.
pub const MAX_BATCH_SEARCH_QUERIES: usize = 50;

/// Custom deserializer that accepts both a single string and an array of strings.
/// e.g. `"facets": "brand"` → `Some(vec!["brand"])` and `"facets": ["brand","category"]` → `Some(vec!["brand","category"])`
fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct StringOrVec;

    impl<'de> de::Visitor<'de> for StringOrVec {
        type Value = Option<Vec<String>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string, an array of strings, or null")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(vec![v.to_string()]))
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(Some(vec![v]))
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut vec = Vec::new();
            while let Some(s) = seq.next_element::<String>()? {
                vec.push(s);
            }
            Ok(Some(vec))
        }
    }

    deserializer.deserialize_any(StringOrVec)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateIndexRequest {
    pub uid: String,
    #[serde(default)]
    pub schema: IndexSchema,
}

#[derive(Debug, Deserialize, Default, ToSchema)]
pub struct IndexSchema {
    #[serde(default)]
    pub text_fields: Vec<String>,
    #[serde(default)]
    pub integer_fields: Vec<String>,
    #[serde(default)]
    pub float_fields: Vec<String>,
    #[serde(default)]
    pub facet_fields: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum AddDocumentsRequest {
    Batch {
        requests: Vec<BatchOperation>,
    },
    Legacy {
        documents: Vec<HashMap<String, serde_json::Value>>,
    },
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchOperation {
    pub action: String,
    #[serde(default, rename = "indexName")]
    pub index_name: Option<String>,
    #[serde(default)]
    pub body: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    pub create_if_not_exists: Option<bool>,
}

fn default_semantic_ratio() -> f64 {
    0.5
}

fn default_embedder_name() -> String {
    "default".to_string()
}

fn default_federation_weight() -> f64 {
    1.0
}

/// Parameters for hybrid (keyword + vector) search.
///
/// Meilisearch-style: `"hybrid": {"semanticRatio": 0.8, "embedder": "mymodel"}`
/// Algolia-style: synthesized internally when `mode: "neuralSearch"`.
#[derive(Debug, Deserialize, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HybridSearchParams {
    #[serde(default = "default_semantic_ratio")]
    pub semantic_ratio: f64,
    #[serde(default = "default_embedder_name")]
    pub embedder: String,
}

impl HybridSearchParams {
    /// Clamp `semantic_ratio` to [0.0, 1.0].
    pub fn clamp_ratio(&mut self) {
        self.semantic_ratio = self.semantic_ratio.clamp(0.0, 1.0);
    }
}

#[derive(Debug, Deserialize, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FederationOptions {
    #[serde(default = "default_federation_weight")]
    pub weight: f64,
}

/// Batch search request supporting legacy parallel queries plus federated merge options.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchSearchRequest {
    pub requests: Vec<SearchRequest>,
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub federation: Option<crate::federation::FederationConfig>,
}

/// Algolia-compatible search request supporting both JSON body fields and URL-encoded `params`.
///
/// Covers keyword search, facet lookups, geo filtering, analytics flags, hybrid
/// search controls, and federated batch-only options such as per-query weights.
#[derive(Debug, Default, Deserialize, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    #[serde(default)]
    pub index_name: Option<String>,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub filters: Option<String>,
    #[serde(default)]
    pub hits_per_page: Option<usize>,
    #[serde(default)]
    pub page: usize,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub facets: Option<Vec<String>>,
    #[serde(default)]
    pub sort: Option<Vec<String>>,
    #[serde(default)]
    pub distinct: Option<serde_json::Value>,
    #[serde(default)]
    pub highlight_pre_tag: Option<String>,
    #[serde(default)]
    pub highlight_post_tag: Option<String>,
    #[serde(default, rename = "attributesToRetrieve")]
    pub attributes_to_retrieve: Option<Vec<String>>,
    #[serde(default, rename = "attributesToHighlight")]
    pub attributes_to_highlight: Option<Vec<String>>,
    #[serde(default, rename = "attributesToSnippet")]
    pub attributes_to_snippet: Option<Vec<String>>,
    #[serde(default, rename = "queryType")]
    pub query_type_prefix: Option<String>,
    #[serde(default, rename = "typoTolerance")]
    pub typo_tolerance: Option<serde_json::Value>,
    #[serde(default, rename = "advancedSyntax")]
    pub advanced_syntax: Option<bool>,
    #[serde(default, rename = "removeWordsIfNoResults")]
    pub remove_words_if_no_results: Option<String>,
    #[serde(default, rename = "optionalFilters")]
    pub optional_filters: Option<serde_json::Value>,
    #[serde(default, rename = "enableSynonyms")]
    pub enable_synonyms: Option<bool>,
    #[serde(default, rename = "enableRules")]
    pub enable_rules: Option<bool>,
    #[serde(default, rename = "ruleContexts")]
    pub rule_contexts: Option<Vec<String>>,
    #[serde(default, rename = "restrictSearchableAttributes")]
    pub restrict_searchable_attributes: Option<Vec<String>>,
    #[serde(default, rename = "facetFilters")]
    pub facet_filters: Option<serde_json::Value>,
    #[serde(default, rename = "numericFilters")]
    pub numeric_filters: Option<serde_json::Value>,
    #[serde(default, rename = "tagFilters")]
    pub tag_filters: Option<serde_json::Value>,
    #[serde(default, rename = "maxValuesPerFacet")]
    pub max_values_per_facet: Option<usize>,
    #[serde(default)]
    pub analytics: Option<bool>,
    #[serde(default, rename = "clickAnalytics")]
    pub click_analytics: Option<bool>,
    #[serde(default, rename = "analyticsTags")]
    pub analytics_tags: Option<Vec<String>>,
    /// URL-encoded params string (used by multi-query). Merged during deserialization.
    #[serde(default)]
    pub params: Option<String>,
    #[serde(default, rename = "type")]
    pub query_type: Option<String>,
    /// Facet name for type=facet multi-search queries
    #[serde(default)]
    pub facet: Option<String>,
    /// Facet query string for type=facet multi-search queries
    #[serde(default, rename = "facetQuery")]
    pub facet_query: Option<String>,
    /// Max facet hits for type=facet multi-search queries
    #[serde(default, rename = "maxFacetHits")]
    pub max_facet_hits: Option<usize>,
    #[serde(default, rename = "getRankingInfo")]
    pub get_ranking_info: Option<bool>,
    #[serde(default, rename = "responseFields")]
    pub response_fields: Option<Vec<String>>,
    #[serde(default, rename = "aroundLatLng")]
    pub around_lat_lng: Option<String>,
    #[serde(default, rename = "aroundRadius")]
    pub around_radius: Option<serde_json::Value>,
    #[serde(default, rename = "insideBoundingBox")]
    pub inside_bounding_box: Option<serde_json::Value>,
    #[serde(default, rename = "insidePolygon")]
    pub inside_polygon: Option<serde_json::Value>,
    #[serde(default, rename = "aroundPrecision")]
    pub around_precision: Option<serde_json::Value>,
    #[serde(default, rename = "minimumAroundRadius")]
    pub minimum_around_radius: Option<u64>,
    #[serde(default, rename = "userToken")]
    pub user_token: Option<String>,
    #[serde(default, rename = "enablePersonalization")]
    pub enable_personalization: Option<bool>,
    #[serde(default, rename = "enableReRanking")]
    pub enable_re_ranking: Option<bool>,
    #[serde(default, rename = "reRankingApplyFilter")]
    pub re_ranking_apply_filter: Option<String>,
    #[serde(default, rename = "personalizationImpact")]
    pub personalization_impact: Option<u32>,
    #[serde(default, rename = "personalizationFilters")]
    pub personalization_filters: Option<Vec<String>>,
    #[serde(default, rename = "sessionID", alias = "sessionId")]
    pub session_id: Option<String>,
    /// Client IP — not deserialized from JSON, set by handler from headers
    #[serde(skip)]
    pub user_ip: Option<String>,
    #[serde(default, rename = "aroundLatLngViaIP")]
    pub around_lat_lng_via_ip: Option<bool>,
    #[serde(default, rename = "removeStopWords")]
    pub remove_stop_words: Option<flapjack::query::stopwords::RemoveStopWordsValue>,
    #[serde(default, rename = "ignorePlurals")]
    pub ignore_plurals: Option<flapjack::query::plurals::IgnorePluralsValue>,
    #[serde(default, rename = "queryLanguages")]
    pub query_languages: Option<Vec<String>>,
    #[serde(default, rename = "naturalLanguages")]
    pub natural_languages: Option<Vec<String>>,
    /// Whether to split compound words in Germanic languages (de, nl, fi, da, sv, no).
    /// Default: true (matching Algolia behavior). Set to false to disable.
    #[serde(default = "default_decompound_query", rename = "decompoundQuery")]
    pub decompound_query: Option<bool>,
    #[serde(default)]
    pub mode: Option<flapjack::index::settings::IndexMode>,
    #[serde(default)]
    pub hybrid: Option<HybridSearchParams>,
    #[serde(default, rename = "federationOptions")]
    pub federation_options: Option<FederationOptions>,

    // ── Stage 4: Missing structural search parameters ──
    #[serde(default, rename = "advancedSyntaxFeatures")]
    pub advanced_syntax_features: Option<Vec<String>>,
    #[serde(default, rename = "sortFacetValuesBy")]
    pub sort_facet_values_by: Option<String>,
    #[serde(default, rename = "facetingAfterDistinct")]
    pub faceting_after_distinct: Option<bool>,
    #[serde(default, rename = "sumOrFiltersScores")]
    pub sum_or_filters_scores: Option<bool>,
    #[serde(default, rename = "snippetEllipsisText")]
    pub snippet_ellipsis_text: Option<String>,
    #[serde(default, rename = "restrictHighlightAndSnippetArrays")]
    pub restrict_highlight_and_snippet_arrays: Option<bool>,
    #[serde(default, rename = "minProximity")]
    pub min_proximity: Option<u32>,
    #[serde(default, rename = "disableExactOnAttributes")]
    pub disable_exact_on_attributes: Option<Vec<String>>,
    #[serde(default, rename = "exactOnSingleWordQuery")]
    pub exact_on_single_word_query: Option<String>,
    #[serde(default, rename = "alternativesAsExact")]
    pub alternatives_as_exact: Option<Vec<String>>,
    #[serde(default, rename = "replaceSynonymsInHighlight")]
    pub replace_synonyms_in_highlight: Option<bool>,
    #[serde(default, rename = "enableABTest")]
    pub enable_ab_test: Option<bool>,
    #[serde(default, rename = "percentileComputation")]
    pub percentile_computation: Option<bool>,
    #[serde(default, rename = "similarQuery")]
    pub similar_query: Option<String>,
    #[serde(default, rename = "relevancyStrictness")]
    pub relevancy_strictness: Option<u32>,
}

impl SearchRequest {
    pub fn effective_hits_per_page(&self) -> usize {
        self.hits_per_page.unwrap_or(20)
    }

    /// Validates this search request against Algolia-compatible limits and structural constraints.
    pub fn validate(&self) -> Result<(), FlapjackError> {
        self.validate_request_limits()?;
        self.validate_federation_options()?;
        self.validate_structural_params()?;
        Ok(())
    }

    /// Checks query length, hitsPerPage, pagination depth, filter length, and personalization impact.
    fn validate_request_limits(&self) -> Result<(), FlapjackError> {
        if self.query.len() > MAX_QUERY_BYTES {
            return Err(FlapjackError::InvalidQuery(format!(
                "Query exceeds maximum length of {} bytes (got {})",
                MAX_QUERY_BYTES,
                self.query.len()
            )));
        }

        if let Some(hpp) = self.hits_per_page {
            if hpp > MAX_HITS_PER_PAGE {
                return Err(FlapjackError::InvalidQuery(format!(
                    "hitsPerPage exceeds maximum of {} (got {})",
                    MAX_HITS_PER_PAGE, hpp
                )));
            }
        }

        let hpp = self.effective_hits_per_page();
        let offset = self.page.saturating_mul(hpp);
        if offset > MAX_PAGINATION_OFFSET {
            return Err(FlapjackError::InvalidQuery(format!(
                "Pagination offset (page {} * hitsPerPage {}) exceeds maximum of {}",
                self.page, hpp, MAX_PAGINATION_OFFSET
            )));
        }

        if let Some(ref f) = self.filters {
            if f.len() > MAX_FILTER_BYTES {
                return Err(FlapjackError::InvalidQuery(format!(
                    "Filter string exceeds maximum length of {} bytes (got {})",
                    MAX_FILTER_BYTES,
                    f.len()
                )));
            }
        }

        if let Some(v) = self.personalization_impact {
            if v > 100 {
                return Err(FlapjackError::InvalidQuery(format!(
                    "personalizationImpact must be between 0 and 100 (got {})",
                    v
                )));
            }
        }

        Ok(())
    }

    /// Checks that federation weight is finite and positive.
    fn validate_federation_options(&self) -> Result<(), FlapjackError> {
        if let Some(federation_options) = self.federation_options.as_ref() {
            if !federation_options.weight.is_finite() || federation_options.weight <= 0.0 {
                return Err(FlapjackError::InvalidQuery(
                    "federationOptions.weight must be a finite number greater than 0".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Validates queryType, advancedSyntaxFeatures, sortFacetValuesBy, exactOnSingleWordQuery,
    /// alternativesAsExact, and minProximity.
    fn validate_structural_params(&self) -> Result<(), FlapjackError> {
        if let Some(ref value) = self.query_type_prefix {
            if value != "prefixAll" && value != "prefixLast" && value != "prefixNone" {
                return Err(FlapjackError::InvalidQuery(format!(
                    "Invalid queryType value: \"{}\". Must be \"prefixAll\", \"prefixLast\", or \"prefixNone\"",
                    value
                )));
            }
        }

        if let Some(ref features) = self.advanced_syntax_features {
            for f in features {
                if f != "exactPhrase" && f != "excludeWords" {
                    return Err(FlapjackError::InvalidQuery(format!(
                        "Invalid advancedSyntaxFeatures value: \"{}\". Must be \"exactPhrase\" or \"excludeWords\"",
                        f
                    )));
                }
            }
        }

        if let Some(ref v) = self.sort_facet_values_by {
            validate_sort_facet_values_by(v)?;
        }

        if let Some(ref v) = self.exact_on_single_word_query {
            if v != "attribute" && v != "none" && v != "word" {
                return Err(FlapjackError::InvalidQuery(format!(
                    "Invalid exactOnSingleWordQuery value: \"{}\". Must be \"attribute\", \"none\", or \"word\"",
                    v
                )));
            }
        }

        if let Some(ref alts) = self.alternatives_as_exact {
            for a in alts {
                if a != "ignorePlurals" && a != "singleWordSynonym" && a != "multiWordsSynonym" {
                    return Err(FlapjackError::InvalidQuery(format!(
                        "Invalid alternativesAsExact value: \"{}\". Must be \"ignorePlurals\", \"singleWordSynonym\", or \"multiWordsSynonym\"",
                        a
                    )));
                }
            }
        }

        if let Some(v) = self.min_proximity {
            if !(1..=7).contains(&v) {
                return Err(FlapjackError::InvalidQuery(format!(
                    "minProximity must be between 1 and 7 (got {})",
                    v
                )));
            }
        }

        Ok(())
    }

    /// Clamp hybrid search ratio to [0.0, 1.0] if present.
    pub fn clamp_hybrid_ratio(&mut self) {
        if let Some(ref mut h) = self.hybrid {
            h.clamp_ratio();
        }
    }

    // apply_params_string, apply_* methods, and build_geo_params are in dto/request_params.rs
    // build_combined_filter and filter AST helpers are in dto/filter_parsing.rs
}

fn default_decompound_query() -> Option<bool> {
    Some(true)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetObjectsRequest {
    pub requests: Vec<GetObjectRequest>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetObjectRequest {
    pub index_name: String,
    #[serde(rename = "objectID")]
    pub object_id: String,
    #[serde(default)]
    pub attributes_to_retrieve: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteByQueryRequest {
    #[serde(default)]
    pub filters: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchFacetValuesRequest {
    #[serde(rename = "facetQuery")]
    pub facet_query: String,

    #[serde(default)]
    pub filters: Option<String>,

    #[serde(default = "default_max_facet_hits")]
    #[serde(rename = "maxFacetHits")]
    pub max_facet_hits: usize,

    #[serde(default, rename = "sortFacetValuesBy")]
    pub sort_facet_values_by: Option<String>,
}

fn default_max_facet_hits() -> usize {
    10
}

fn validate_sort_facet_values_by(value: &str) -> Result<(), FlapjackError> {
    if value != "count" && value != "alpha" {
        return Err(FlapjackError::InvalidQuery(format!(
            "Invalid sortFacetValuesBy value: \"{}\". Must be \"count\" or \"alpha\"",
            value
        )));
    }
    Ok(())
}

impl SearchFacetValuesRequest {
    pub fn validate(&self) -> Result<(), FlapjackError> {
        if let Some(ref v) = self.sort_facet_values_by {
            validate_sort_facet_values_by(v)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "dto_tests.rs"]
mod tests;
