use crate::query::plurals::IgnorePluralsValue;
use crate::query::stopwords::RemoveStopWordsValue;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

/// Strip the `unordered(...)` wrapper from a searchable attribute if present.
/// Returns the inner attribute name (e.g., `"title"` from `"unordered(title)"`).
pub fn strip_unordered_prefix(attr: &str) -> &str {
    if let Some(inner) = attr.strip_prefix("unordered(") {
        if let Some(stripped) = inner.strip_suffix(")") {
            return stripped;
        }
    }
    attr
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankingCriterionToken {
    Typo,
    Geo,
    Words,
    Filters,
    Proximity,
    Attribute,
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomRankingDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCustomRankingToken {
    pub attribute: String,
    pub direction: CustomRankingDirection,
}

pub fn classify_ranking_criterion_token(token: &str) -> Option<RankingCriterionToken> {
    match token {
        "typo" => Some(RankingCriterionToken::Typo),
        "geo" => Some(RankingCriterionToken::Geo),
        "words" => Some(RankingCriterionToken::Words),
        "filters" => Some(RankingCriterionToken::Filters),
        "proximity" => Some(RankingCriterionToken::Proximity),
        "attribute" => Some(RankingCriterionToken::Attribute),
        "exact" => Some(RankingCriterionToken::Exact),
        _ => None,
    }
}

pub fn is_ranking_criterion_token(token: &str) -> bool {
    classify_ranking_criterion_token(token).is_some()
}

pub fn is_known_source_ranking_token(token: &str) -> bool {
    token == "custom"
        || is_ranking_criterion_token(token)
        || parse_custom_ranking_token(token).is_some()
}

pub fn parse_custom_ranking_token(token: &str) -> Option<ParsedCustomRankingToken> {
    if let Some(attribute) = token.strip_prefix("desc(") {
        return Some(ParsedCustomRankingToken {
            attribute: attribute.trim_end_matches(')').to_string(),
            direction: CustomRankingDirection::Desc,
        });
    }
    if let Some(attribute) = token.strip_prefix("asc(") {
        return Some(ParsedCustomRankingToken {
            attribute: attribute.trim_end_matches(')').to_string(),
            direction: CustomRankingDirection::Asc,
        });
    }
    None
}

fn default_hits_per_page() -> u32 {
    20
}

fn serialize_vec_as_null_if_empty<S>(vec: &Vec<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if vec.is_empty() {
        serializer.serialize_none()
    } else {
        vec.serialize(serializer)
    }
}

fn deserialize_null_as_empty_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Vec<String>>::deserialize(deserializer).map(|opt| opt.unwrap_or_default())
}

fn remove_stop_words_is_default(v: &RemoveStopWordsValue) -> bool {
    matches!(v, RemoveStopWordsValue::Disabled)
}

fn ignore_plurals_is_default(v: &IgnorePluralsValue) -> bool {
    matches!(v, IgnorePluralsValue::Disabled)
}

fn vec_is_empty(v: &[String]) -> bool {
    v.is_empty()
}

#[path = "settings_embedders.rs"]
mod embedders;
#[path = "settings_redaction.rs"]
mod redaction;

pub use embedders::{detect_embedder_changes, EmbedderChange};
pub use redaction::REDACTED_SECRET;

use redaction::{
    redact_embedder_secrets, redact_user_data_secrets, restore_embedder_secrets,
    restore_user_data_secrets,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum IndexMode {
    KeywordSearch,
    NeuralSearch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SemanticSearchSettings {
    #[serde(rename = "eventSources", skip_serializing_if = "Option::is_none")]
    pub event_sources: Option<Vec<String>>,
}

/// Hold all index configuration settings including ranking, faceting, searchable attributes, typo tolerance, embedders, and mode. Serialized as camelCase JSON for Algolia API compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexSettings {
    #[serde(
        rename = "attributesForFaceting",
        serialize_with = "serialize_vec_as_null_if_empty",
        deserialize_with = "deserialize_null_as_empty_vec"
    )]
    pub attributes_for_faceting: Vec<String>,

    #[serde(rename = "searchableAttributes")]
    pub searchable_attributes: Option<Vec<String>>,

    #[serde(rename = "ranking")]
    pub ranking: Option<Vec<String>>,

    #[serde(rename = "customRanking")]
    pub custom_ranking: Option<Vec<String>>,

    #[serde(
        rename = "relevancyStrictness",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub relevancy_strictness: Option<u32>,

    #[serde(rename = "attributesToRetrieve")]
    pub attributes_to_retrieve: Option<Vec<String>>,

    #[serde(rename = "unretrievableAttributes")]
    pub unretrievable_attributes: Option<Vec<String>>,

    #[serde(rename = "attributesToHighlight")]
    pub attributes_to_highlight: Option<Vec<String>>,

    #[serde(rename = "attributesToSnippet")]
    pub attributes_to_snippet: Option<Vec<String>>,

    #[serde(rename = "highlightPreTag")]
    pub highlight_pre_tag: Option<String>,

    #[serde(rename = "highlightPostTag")]
    pub highlight_post_tag: Option<String>,

    #[serde(rename = "hitsPerPage", default = "default_hits_per_page")]
    pub hits_per_page: u32,

    #[serde(rename = "minWordSizefor1Typo")]
    pub min_word_size_for_1_typo: u32,

    #[serde(rename = "minWordSizefor2Typos")]
    pub min_word_size_for_2_typos: u32,

    #[serde(rename = "maxValuesPerFacet")]
    pub max_values_per_facet: u32,

    #[serde(rename = "paginationLimitedTo")]
    pub pagination_limited_to: u32,

    #[serde(rename = "exactOnSingleWordQuery")]
    pub exact_on_single_word_query: String,

    #[serde(rename = "queryType")]
    pub query_type: String,

    #[serde(rename = "removeWordsIfNoResults")]
    pub remove_words_if_no_results: String,

    #[serde(rename = "separatorsToIndex")]
    pub separators_to_index: String,

    #[serde(
        rename = "keepDiacriticsOnCharacters",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub keep_diacritics_on_characters: String,

    #[serde(
        rename = "customNormalization",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub custom_normalization: Option<HashMap<String, HashMap<String, String>>>,

    #[serde(
        rename = "camelCaseAttributes",
        default,
        skip_serializing_if = "vec_is_empty"
    )]
    pub camel_case_attributes: Vec<String>,

    #[serde(
        rename = "decompoundedAttributes",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub decompounded_attributes: Option<HashMap<String, Vec<String>>>,

    #[serde(
        rename = "alternativesAsExact",
        serialize_with = "serialize_vec_as_null_if_empty",
        deserialize_with = "deserialize_null_as_empty_vec"
    )]
    pub alternatives_as_exact: Vec<String>,

    #[serde(
        rename = "optionalWords",
        serialize_with = "serialize_vec_as_null_if_empty",
        deserialize_with = "deserialize_null_as_empty_vec"
    )]
    pub optional_words: Vec<String>,

    #[serde(
        rename = "numericAttributesForFiltering",
        alias = "numericAttributesToIndex"
    )]
    pub numeric_attributes_for_filtering: Option<Vec<String>>,

    #[serde(rename = "attributesToIndex", skip_serializing_if = "Option::is_none")]
    pub attributes_to_index: Option<Vec<String>>,

    pub version: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonyms: Option<serde_json::Value>,

    #[serde(rename = "attributeForDistinct")]
    pub attribute_for_distinct: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub distinct: Option<DistinctValue>,

    #[serde(
        rename = "removeStopWords",
        default,
        skip_serializing_if = "remove_stop_words_is_default"
    )]
    pub remove_stop_words: RemoveStopWordsValue,

    #[serde(
        rename = "queryLanguages",
        default,
        skip_serializing_if = "vec_is_empty"
    )]
    pub query_languages: Vec<String>,

    #[serde(
        rename = "indexLanguages",
        default,
        skip_serializing_if = "vec_is_empty"
    )]
    pub index_languages: Vec<String>,

    #[serde(
        rename = "ignorePlurals",
        default,
        skip_serializing_if = "ignore_plurals_is_default"
    )]
    pub ignore_plurals: IgnorePluralsValue,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedders: Option<HashMap<String, serde_json::Value>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<IndexMode>,

    #[serde(rename = "semanticSearch", skip_serializing_if = "Option::is_none")]
    pub semantic_search: Option<SemanticSearchSettings>,

    #[serde(
        rename = "enablePersonalization",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub enable_personalization: Option<bool>,

    #[serde(rename = "renderingContent", skip_serializing_if = "Option::is_none")]
    pub rendering_content: Option<serde_json::Value>,

    #[serde(rename = "userData", skip_serializing_if = "Option::is_none")]
    pub user_data: Option<serde_json::Value>,

    #[serde(rename = "enableRules", skip_serializing_if = "Option::is_none")]
    pub enable_rules: Option<bool>,

    // ── Stage 4: Missing structural settings parameters ──
    #[serde(
        rename = "advancedSyntaxFeatures",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub advanced_syntax_features: Option<Vec<String>>,

    #[serde(
        rename = "sortFacetValuesBy",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sort_facet_values_by: Option<String>,

    #[serde(
        rename = "snippetEllipsisText",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub snippet_ellipsis_text: Option<String>,

    #[serde(
        rename = "restrictHighlightAndSnippetArrays",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub restrict_highlight_and_snippet_arrays: Option<bool>,

    #[serde(
        rename = "minProximity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub min_proximity: Option<u32>,

    #[serde(
        rename = "disableExactOnAttributes",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_exact_on_attributes: Option<Vec<String>>,

    #[serde(
        rename = "replaceSynonymsInHighlight",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub replace_synonyms_in_highlight: Option<bool>,

    #[serde(
        rename = "attributeCriteriaComputedByMinProximity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub attribute_criteria_computed_by_min_proximity: Option<bool>,
    #[serde(
        rename = "enableReRanking",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub enable_re_ranking: Option<bool>,
    #[serde(
        rename = "disableTypoToleranceOnWords",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_typo_tolerance_on_words: Option<Vec<String>>,
    #[serde(
        rename = "disableTypoToleranceOnAttributes",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_typo_tolerance_on_attributes: Option<Vec<String>>,

    /// No-op compatibility field: accepted and persisted but has no behavioral effect.
    #[serde(
        rename = "allowCompressionOfIntegerArray",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub allow_compression_of_integer_array: Option<bool>,

    // ── Replicas (§10) ──
    #[serde(rename = "replicas", default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<Vec<String>>,

    /// Read-only: set on replica indexes to point back to the primary.
    #[serde(rename = "primary", default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum DistinctValue {
    Bool(bool),
    Integer(u32),
}

impl Default for IndexSettings {
    /// Initialize settings with standard search-engine defaults: eight-criterion ranking, 20 hits per page, `prefixLast` query type, typo thresholds of 4/8, and all optional fields set to `None`.
    fn default() -> Self {
        IndexSettings {
            attributes_for_faceting: Vec::new(),
            searchable_attributes: None,
            ranking: Some(vec![
                "typo".to_string(),
                "geo".to_string(),
                "words".to_string(),
                "filters".to_string(),
                "proximity".to_string(),
                "attribute".to_string(),
                "exact".to_string(),
                "custom".to_string(),
            ]),
            custom_ranking: None,
            relevancy_strictness: None,
            attributes_to_retrieve: None,
            unretrievable_attributes: None,
            attributes_to_highlight: None,
            attributes_to_snippet: None,
            highlight_pre_tag: Some("<em>".to_string()),
            highlight_post_tag: Some("</em>".to_string()),
            hits_per_page: 20,
            min_word_size_for_1_typo: 4,
            min_word_size_for_2_typos: 8,
            max_values_per_facet: 100,
            pagination_limited_to: 1000,
            exact_on_single_word_query: "attribute".to_string(),
            query_type: "prefixLast".to_string(),
            remove_words_if_no_results: "none".to_string(),
            separators_to_index: "".to_string(),
            keep_diacritics_on_characters: "".to_string(),
            custom_normalization: None,
            camel_case_attributes: Vec::new(),
            decompounded_attributes: None,
            alternatives_as_exact: vec![
                "ignorePlurals".to_string(),
                "singleWordSynonym".to_string(),
            ],
            optional_words: Vec::new(),
            numeric_attributes_for_filtering: None,
            attributes_to_index: None,
            version: 1,
            synonyms: None,
            attribute_for_distinct: None,
            distinct: None,
            remove_stop_words: RemoveStopWordsValue::Disabled,
            query_languages: Vec::new(),
            index_languages: Vec::new(),
            ignore_plurals: IgnorePluralsValue::Disabled,
            embedders: None,
            mode: None,
            semantic_search: None,
            enable_personalization: None,
            rendering_content: None,
            user_data: None,
            enable_rules: None,
            advanced_syntax_features: None,
            sort_facet_values_by: None,
            snippet_ellipsis_text: None,
            restrict_highlight_and_snippet_arrays: None,
            min_proximity: None,
            disable_exact_on_attributes: None,
            disable_typo_tolerance_on_words: None,
            disable_typo_tolerance_on_attributes: None,
            replace_synonyms_in_highlight: None,
            attribute_criteria_computed_by_min_proximity: None,
            enable_re_ranking: None,
            allow_compression_of_integer_array: None,
            replicas: None,
            primary: None,
        }
    }
}

impl DistinctValue {
    pub fn as_count(&self) -> u32 {
        match self {
            DistinctValue::Bool(false) => 0,
            DistinctValue::Bool(true) => 1,
            DistinctValue::Integer(n) => *n,
        }
    }
}

impl IndexSettings {
    /// Extract single-character normalization mappings from the `"default"` script in `custom_normalization`, lowercasing both keys and replacement values.
    ///
    /// Multi-character keys and multi-codepoint lowercase expansions are silently skipped.
    ///
    /// # Arguments
    ///
    /// * `settings` - The index settings containing the optional `custom_normalization` map.
    ///
    /// # Returns
    ///
    /// A sorted `Vec<(char, String)>` of lowercase source characters to their normalized replacements.
    pub fn flatten_custom_normalization(settings: &IndexSettings) -> Vec<(char, String)> {
        let mut by_char: BTreeMap<char, String> = BTreeMap::new();

        if let Some(default_map) = settings
            .custom_normalization
            .as_ref()
            .and_then(|map| map.get("default"))
        {
            let mut entries: Vec<(&String, &String)> = default_map.iter().collect();
            entries.sort_by_key(|(left_key, _)| *left_key);

            for (char_key, replacement) in entries {
                let mut chars = char_key.chars();
                let c = match chars.next() {
                    Some(value) if chars.next().is_none() => value,
                    _ => continue,
                };
                let mut lowered = c.to_lowercase();
                let lower = match lowered.next() {
                    Some(value) if lowered.next().is_none() => value,
                    _ => continue,
                };
                let normalized_replacement = replacement
                    .chars()
                    .flat_map(|ch| ch.to_lowercase())
                    .collect::<String>();
                by_char.insert(lower, normalized_replacement);
            }
        }

        by_char.into_iter().collect()
    }

    pub fn load<P: AsRef<Path>>(path: P) -> crate::error::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let settings: IndexSettings = serde_json::from_str(&content)?;
        // Upgrade-path defense: a settings.json file written before the
        // OWASP audit introduced intake-side SSRF validation may carry an
        // embedder URL (e.g. http://127.0.0.1:11434/api/embeddings for
        // Ollama) that is unsafe under the new default-deny posture.
        // Constructor-time validation was removed as part of the Plan B
        // SoC split, so the trust boundary now lives at intake AND at
        // disk-load. The intake gate at `validate_embedders_inner` catches
        // new writes; this catches pre-existing on-disk state at hydration.
        //
        // The SSOT env-var opt-in (`FLAPJACK_AI_ALLOW_LOCAL_URLS`) is honored
        // via the composite `EmbedderConfig::validate()` that
        // `validate_embedders` invokes, so operators running a local model
        // server can opt back in symmetrically with the chat seam.
        //
        // Empty / `None` embedders are a no-op here, so the vast majority of
        // existing tenants and tests are unaffected.
        settings
            .validate_embedders()
            .map_err(crate::error::FlapjackError::Config)?;
        Ok(settings)
    }

    pub fn save<P: AsRef<Path>>(&self, path: P) -> crate::error::Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn facet_set(&self) -> HashSet<String> {
        self.attributes_for_faceting
            .iter()
            .map(|s| parse_facet_modifier(s))
            .collect()
    }

    pub fn searchable_facet_set(&self) -> HashSet<String> {
        self.attributes_for_faceting
            .iter()
            .filter(|s| s.starts_with("searchable("))
            .map(|s| parse_facet_modifier(s))
            .collect()
    }

    pub fn default_with_facets(facets: Vec<String>) -> Self {
        Self {
            attributes_for_faceting: facets,
            ..Self::default()
        }
    }

    pub fn should_retrieve(&self, field: &str) -> bool {
        self.should_retrieve_with_acl(field, false)
    }

    /// Determine whether a field should be included in retrieval results, respecting both `unretrievableAttributes` and `attributesToRetrieve`.
    ///
    /// # Arguments
    ///
    /// * `field` - The attribute name to check.
    /// * `can_see_unretrievable_attributes` - When `true`, bypass the unretrievable-attributes filter (admin ACL).
    ///
    /// # Returns
    ///
    /// `true` if the field should appear in the response.
    pub fn should_retrieve_with_acl(
        &self,
        field: &str,
        can_see_unretrievable_attributes: bool,
    ) -> bool {
        if !can_see_unretrievable_attributes {
            if let Some(unretrievable) = &self.unretrievable_attributes {
                if unretrievable.iter().any(|attr| attr == field) {
                    return false;
                }
            }
        }

        if let Some(retrievable) = &self.attributes_to_retrieve {
            if retrievable.iter().any(|attr| attr == "*") {
                return true;
            }
            return retrievable.iter().any(|attr| attr == field);
        }

        true
    }

    pub fn is_neural_search_active(&self) -> bool {
        matches!(self.mode, Some(IndexMode::NeuralSearch))
    }

    pub fn redacted_for_response(&self) -> Self {
        let mut clone = self.clone();
        clone.redact_response_secrets();
        clone
    }

    pub fn redacted_user_data(&self) -> Option<serde_json::Value> {
        let mut user_data = self.user_data.clone();
        redact_user_data_secrets(&mut user_data);
        user_data
    }

    pub fn restore_redacted_response_secrets(&mut self, previous: &Self) {
        restore_user_data_secrets(&mut self.user_data, previous.user_data.as_ref());
        restore_embedder_secrets(&mut self.embedders, previous.embedders.as_ref());
    }

    fn redact_response_secrets(&mut self) {
        redact_user_data_secrets(&mut self.user_data);
        redact_embedder_secrets(&mut self.embedders);
    }

    /// Validate embedder configurations. Returns Ok(()) if no embedders or if
    /// the vector-search feature is not enabled. With the feature, each config
    /// is parsed into EmbedderConfig and validated.
    pub fn validate_embedders(&self) -> Result<(), String> {
        let embedders = match &self.embedders {
            Some(map) if !map.is_empty() => map,
            _ => return Ok(()),
        };
        self.validate_embedders_inner(embedders)
    }

    /// Parse each embedder entry into `EmbedderConfig` and run its validation.
    ///
    /// Behind the `vector-search` feature flag; the non-feature build is a no-op.
    ///
    /// # Arguments
    ///
    /// * `embedders` - Map of embedder name to raw JSON configuration.
    ///
    /// # Returns
    ///
    /// `Err(String)` naming the offending embedder on parse or validation failure.
    #[cfg(feature = "vector-search")]
    fn validate_embedders_inner(
        &self,
        embedders: &HashMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        use crate::vector::config::EmbedderConfig;
        for (name, value) in embedders {
            if value.is_null() {
                continue;
            }
            let config: EmbedderConfig = serde_json::from_value(value.clone())
                .map_err(|e| format!("embedder '{}': {}", name, e))?;
            config
                .validate()
                .map_err(|e| format!("embedder '{}': {}", name, e))?;
        }
        Ok(())
    }

    #[cfg(not(feature = "vector-search"))]
    fn validate_embedders_inner(
        &self,
        embedders: &HashMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        let _ = embedders;
        Ok(())
    }
}

fn parse_facet_modifier(attr: &str) -> String {
    if let Some(stripped) = attr.strip_prefix("filterOnly(") {
        stripped.trim_end_matches(')').to_string()
    } else if let Some(stripped) = attr.strip_prefix("searchable(") {
        stripped.trim_end_matches(')').to_string()
    } else if let Some(stripped) = attr.strip_prefix("afterDistinct(") {
        stripped.trim_end_matches(')').to_string()
    } else {
        attr.to_string()
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
