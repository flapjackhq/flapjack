//! Implement Algolia-compatible query rules engine with pattern matching, query rewriting, facet filtering, and rule evaluation with first-match-wins semantics.

mod evaluation;
mod matching;

use matching::apply_query_edits_to_text;

#[cfg(test)]
use matching::{
    calculate_typo_distance, extract_facet_captures, fuzzy_word_match,
    match_pattern_tokens_with_placeholders, parse_pattern_tokens, tokenize_for_rule_matching,
    PatternToken,
};

use crate::error::Result;
use crate::filter_parser;
use crate::index::synonyms::SynonymStore;
use crate::types::Filter;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const MAX_HIDDEN_OBJECT_IDS_PER_RULE: usize = 50;

/// Represent a single query rule with conditions, consequences, validity ranges, and an enabled flag.
///
/// A rule matches when it is enabled, currently valid, and at least one of its conditions is satisfied (OR logic). Conditionless rules always match but have restricted consequence behavior: promotes, query edits, and automatic facet filters are ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    #[serde(rename = "objectID")]
    pub object_id: String,

    #[serde(default)]
    pub conditions: Vec<Condition>,

    pub consequence: Consequence,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub validity: Option<Vec<TimeRange>>,
}

/// Represent a single condition within a rule, evaluated as part of the rule's OR-combined condition list.
///
/// A condition can match on query pattern, rule context, active filters, or any combination. All specified fields within a single condition must match (AND logic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchoring: Option<Anchoring>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternatives: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Anchoring {
    Is,
    StartsWith,
    EndsWith,
    Contains,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeRange {
    pub from: i64,
    pub until: i64,
}

/// Define the actions taken when a rule matches: promoting/hiding documents, applying search parameter overrides, and attaching user data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Consequence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promote: Option<Vec<Promote>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide: Option<Vec<Hide>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_promotes: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<ConsequenceParams>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ConsequenceQuery {
    Literal(String),
    Edits {
        #[serde(skip_serializing_if = "Option::is_none")]
        remove: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        edits: Option<Vec<Edit>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EditType {
    Remove,
    Replace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Edit {
    #[serde(rename = "type")]
    pub edit_type: EditType,
    pub delete: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AutomaticFacetFilter {
    pub facet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disjunctive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negative: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AutomaticFacetFilterSerde {
    Shorthand(String),
    Expanded {
        facet: String,
        disjunctive: Option<bool>,
        score: Option<i32>,
        negative: Option<bool>,
    },
}

impl<'de> Deserialize<'de> for AutomaticFacetFilter {
    /// Deserialize from either a plain string shorthand (facet name only) or an expanded object with `facet`, `disjunctive`, `score`, and `negative` fields.
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match AutomaticFacetFilterSerde::deserialize(deserializer)? {
            AutomaticFacetFilterSerde::Shorthand(facet) => Ok(Self {
                facet,
                disjunctive: None,
                score: None,
                negative: None,
            }),
            AutomaticFacetFilterSerde::Expanded {
                facet,
                disjunctive,
                score,
                negative,
            } => Ok(Self {
                facet,
                disjunctive,
                score,
                negative,
            }),
        }
    }
}

/// Search parameter overrides applied when a rule matches.
///
/// Support query rewrites, automatic facet filters, explicit filter expressions, geo parameters, pagination, rendering content, and searchable attribute restrictions. Serialized with camelCase keys for Algolia API compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConsequenceParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<ConsequenceQuery>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_facet_filters: Option<Vec<AutomaticFacetFilter>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_optional_facet_filters: Option<Vec<AutomaticFacetFilter>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendering_content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facet_filters: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub numeric_filters: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_filters: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_filters: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub around_lat_lng: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub around_radius: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hits_per_page: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restrict_searchable_attributes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Promote {
    Single {
        #[serde(rename = "objectID")]
        object_id: String,
        position: usize,
    },
    Multiple {
        #[serde(rename = "objectIDs")]
        object_ids: Vec<String>,
        position: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hide {
    #[serde(rename = "objectID")]
    pub object_id: String,
}

/// A generated mandatory facet filter expression with its disjunctive flag.
/// When `disjunctive` is true, multiple filters for the same facet attribute
/// are combined with OR instead of AND.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFacetFilter {
    pub expression: String,
    pub disjunctive: bool,
}

/// Accumulated effects from evaluating all matching rules against a query.
///
/// Contains pins, hides, filters, query rewrites, facet captures, rendering content, and other search parameter overrides collected during rule evaluation.
#[derive(Debug, Default, Clone)]
pub struct RuleEffects {
    pub pins: Vec<(String, usize)>,
    pub hidden: Vec<String>,
    pub filter_promotes: Option<bool>,
    pub user_data: Vec<serde_json::Value>,
    pub applied_rules: Vec<String>,
    pub filters: Option<String>,
    pub facet_filters: Vec<serde_json::Value>,
    pub numeric_filters: Vec<serde_json::Value>,
    pub optional_filters: Vec<serde_json::Value>,
    pub tag_filters: Vec<serde_json::Value>,
    pub automatic_facet_filters: Vec<AutomaticFacetFilter>,
    pub automatic_optional_facet_filters: Vec<AutomaticFacetFilter>,
    /// Captured facet values from `{facet:attrName}` pattern placeholders.
    /// Maps attribute name → captured query word.
    pub facet_captures: HashMap<String, String>,
    /// Generated mandatory facet filter expressions with disjunctive flag.
    pub generated_facet_filters: Vec<GeneratedFacetFilter>,
    /// Generated optional facet filter expressions: (facet, value, score).
    pub generated_optional_facet_filters: Vec<(String, String, i32)>,
    pub around_lat_lng: Option<String>,
    pub around_radius: Option<serde_json::Value>,
    pub hits_per_page: Option<usize>,
    pub restrict_searchable_attributes: Option<Vec<String>>,
    pub query_edits: Option<ConsequenceQuery>,
    pub rewritten_query: Option<String>,
    pub rendering_content: Option<serde_json::Value>,
}

/// Recursively merge an overlay JSON value into a base value.
///
/// For objects, overlay keys are merged into the base recursively. For all other types, the overlay replaces the base value entirely.
pub fn merge_json_values(base: &mut serde_json::Value, overlay: &serde_json::Value) {
    match (base, overlay) {
        (serde_json::Value::Object(base_obj), serde_json::Value::Object(overlay_obj)) => {
            for (key, overlay_value) in overlay_obj {
                match base_obj.get_mut(key) {
                    Some(base_value) => merge_json_values(base_value, overlay_value),
                    None => {
                        base_obj.insert(key.clone(), overlay_value.clone());
                    }
                }
            }
        }
        (base_slot, overlay_value) => {
            *base_slot = overlay_value.clone();
        }
    }
}

pub struct RuleStore {
    rules: IndexMap<String, Rule>,
}

impl Default for RuleStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleStore {
    pub fn new() -> Self {
        RuleStore {
            rules: IndexMap::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let rules: Vec<Rule> = serde_json::from_str(&content)?;

        let mut store = RuleStore::new();
        for rule in rules {
            store.rules.insert(rule.object_id.clone(), rule);
        }
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let rules: Vec<&Rule> = self.rules.values().collect();
        let content = serde_json::to_string_pretty(&rules)?;
        crate::index::atomic_write_file(path, content.as_bytes())?;
        Ok(())
    }

    pub fn get(&self, object_id: &str) -> Option<&Rule> {
        self.rules.get(object_id)
    }

    pub fn insert(&mut self, rule: Rule) {
        self.rules.insert(rule.object_id.clone(), rule);
    }

    pub fn remove(&mut self, object_id: &str) -> Option<Rule> {
        self.rules.shift_remove(object_id)
    }

    pub fn clear(&mut self) {
        self.rules.clear();
    }

    pub fn all(&self) -> Vec<Rule> {
        self.rules.values().cloned().collect()
    }

    /// Search rules by query string, matching against objectID, description, and condition patterns.
    ///
    /// Return paginated results sorted by objectID. An empty query matches all rules.
    pub fn search(&self, query: &str, page: usize, hits_per_page: usize) -> (Vec<Rule>, usize) {
        let query_lower = query.to_lowercase();

        let mut matching: Vec<Rule> = self
            .rules
            .values()
            .filter(|rule| {
                if query.is_empty() {
                    return true;
                }

                if rule.object_id.to_lowercase().contains(&query_lower) {
                    return true;
                }

                if let Some(ref desc) = rule.description {
                    if desc.to_lowercase().contains(&query_lower) {
                        return true;
                    }
                }

                for condition in &rule.conditions {
                    if let Some(pattern) = &condition.pattern {
                        if pattern.to_lowercase().contains(&query_lower) {
                            return true;
                        }
                    }
                }

                false
            })
            .cloned()
            .collect();

        matching.sort_by(|a, b| a.object_id.cmp(&b.object_id));

        let total = matching.len();
        let start = page.saturating_mul(hits_per_page);
        let end = start.saturating_add(hits_per_page).min(total);

        let hits = if start < total {
            matching[start..end].to_vec()
        } else {
            Vec::new()
        };

        (hits, total)
    }
}

#[cfg(test)]
#[path = "../rules_tests.rs"]
mod tests;
