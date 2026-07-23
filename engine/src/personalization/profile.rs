//! Personalization profile computation and storage. Scores and normalizes user event affinities across facets to 0-20 range, persisting profiles to JSON.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::types::{Document, FieldValue};

pub const STRATEGY_FILENAME: &str = "personalization_strategy.json";
pub const MAX_EVENTS: usize = 15;
pub const MAX_FACETS: usize = 15;
const MAX_AFFINITY: f64 = 20.0;
const NINETY_DAYS_MS: i64 = 90 * 24 * 60 * 60 * 1000;
const VALID_EVENT_TYPES: &[&str] = &["click", "conversion", "view"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PersonalizationStrategy {
    pub events_scoring: Vec<EventScoring>,
    pub facets_scoring: Vec<FacetScoring>,
    pub personalization_impact: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct EventScoring {
    pub event_name: String,
    pub event_type: String,
    pub score: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct FacetScoring {
    pub facet_name: String,
    pub score: u32,
}

impl PersonalizationStrategy {
    /// Check all configuration constraints: personalizationImpact 0-100, max 15 events and 15 facets, event/facet scores 1-100, and event types in [click, conversion, view].
    pub fn validate(&self) -> Result<(), String> {
        if self.personalization_impact > 100 {
            return Err("personalizationImpact must be between 0 and 100".to_string());
        }

        if self.events_scoring.len() > MAX_EVENTS {
            return Err(format!(
                "eventsScoring cannot have more than {} entries",
                MAX_EVENTS
            ));
        }

        if self.facets_scoring.len() > MAX_FACETS {
            return Err(format!(
                "facetsScoring cannot have more than {} entries",
                MAX_FACETS
            ));
        }

        for event in &self.events_scoring {
            if !VALID_EVENT_TYPES.contains(&event.event_type.as_str()) {
                return Err(format!(
                    "invalid eventType '{}': must be one of click, conversion, view",
                    event.event_type
                ));
            }
            if event.score == 0 || event.score > 100 {
                return Err("event score must be between 1 and 100".to_string());
            }
        }

        for facet in &self.facets_scoring {
            if facet.score == 0 || facet.score > 100 {
                return Err("facet score must be between 1 and 100".to_string());
            }
        }

        Ok(())
    }

    fn event_score_map(&self) -> HashMap<(String, String), u32> {
        self.events_scoring
            .iter()
            .map(|e| ((e.event_name.clone(), e.event_type.clone()), e.score))
            .collect()
    }

    fn facet_score_map(&self) -> HashMap<String, u32> {
        self.facets_scoring
            .iter()
            .map(|f| (f.facet_name.clone(), f.score))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInsightEvent {
    pub event_type: String,
    pub event_name: String,
    pub timestamp_ms: i64,
    pub facets: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputedProfile {
    pub last_event_at_ms: Option<i64>,
    pub scores: BTreeMap<String, BTreeMap<String, u32>>,
}

impl ComputedProfile {
    pub fn to_profile(&self, user_token: &str) -> PersonalizationProfile {
        PersonalizationProfile {
            user_token: user_token.to_string(),
            last_event_at: self
                .last_event_at_ms
                .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                .map(|dt| dt.to_rfc3339()),
            scores: self.scores.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PersonalizationProfile {
    pub user_token: String,
    pub last_event_at: Option<String>,
    pub scores: BTreeMap<String, BTreeMap<String, u32>>,
}

/// Score events against strategy rules and normalize affinity scores to 0-20 range.
///
/// Raw scores multiply event_score × facet_score per unique facet value. Scores are normalized so the strongest affinity equals exactly 20. Events older than 90 days are excluded.
pub fn compute_profile(
    strategy: &PersonalizationStrategy,
    events: &[ResolvedInsightEvent],
    now_ms: i64,
) -> ComputedProfile {
    let cutoff_ms = now_ms.saturating_sub(NINETY_DAYS_MS);
    let event_scores = strategy.event_score_map();
    let facet_scores = strategy.facet_score_map();

    let mut last_event_at_ms: Option<i64> = None;
    let mut raw_scores: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();

    for event in events {
        if event.timestamp_ms < cutoff_ms {
            continue;
        }

        let Some(event_score) =
            event_scores.get(&(event.event_name.clone(), event.event_type.clone()))
        else {
            continue;
        };

        let mut contributed = false;

        for (facet_name, facet_values) in &event.facets {
            let Some(facet_score) = facet_scores.get(facet_name) else {
                continue;
            };

            let increment = (*event_score as u64) * (*facet_score as u64);
            let values_entry = raw_scores.entry(facet_name.clone()).or_default();

            let mut seen_values = HashSet::new();
            for value in facet_values {
                if value.is_empty() {
                    continue;
                }
                if !seen_values.insert(value.clone()) {
                    continue;
                }

                let score_entry = values_entry.entry(value.clone()).or_insert(0);
                *score_entry += increment;
                contributed = true;
            }
        }

        if contributed {
            last_event_at_ms = Some(
                last_event_at_ms.map_or(event.timestamp_ms, |prev| prev.max(event.timestamp_ms)),
            );
        }
    }

    let max_raw = raw_scores
        .values()
        .flat_map(|facet_map| facet_map.values())
        .copied()
        .max()
        .unwrap_or(0);

    if max_raw == 0 {
        return ComputedProfile {
            last_event_at_ms,
            scores: BTreeMap::new(),
        };
    }

    let mut normalized_scores: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    for (facet_name, values) in raw_scores {
        let mut facet_scores_out = BTreeMap::new();
        for (facet_value, raw) in values {
            let normalized = ((raw as f64 / max_raw as f64) * MAX_AFFINITY).round();
            let clamped = normalized.clamp(0.0, MAX_AFFINITY) as u32;
            if clamped > 0 {
                facet_scores_out.insert(facet_value, clamped);
            }
        }
        if !facet_scores_out.is_empty() {
            normalized_scores.insert(facet_name, facet_scores_out);
        }
    }

    ComputedProfile {
        last_event_at_ms,
        scores: normalized_scores,
    }
}

#[cfg(feature = "analytics")]
#[derive(Debug, Clone)]
struct InsightRow {
    event_type: String,
    event_name: String,
    timestamp_ms: i64,
    object_ids: Vec<String>,
}

/// Query analytics for user events, extract document facets from raw events, and compute personalization profile.
///
/// # Arguments
/// * `manager` - IndexManager for retrieving documents by ID
/// * `analytics_engine` - AnalyticsQueryEngine for querying events
/// * `strategy` - PersonalizationStrategy with scoring configuration
/// * `user_token` - User identifier (1-129 alphanumeric/dash/underscore chars)
/// * `now_ms` - Current timestamp in milliseconds for cutoff calculation
///
/// # Returns
/// ComputedProfile if successful; error if strategy validation fails or queries error
#[cfg(feature = "analytics")]
pub async fn compute_profile_from_storage(
    manager: &crate::IndexManager,
    analytics_engine: &crate::analytics::AnalyticsQueryEngine,
    strategy: &PersonalizationStrategy,
    user_token: &str,
    now_ms: i64,
) -> Result<ComputedProfile, String> {
    strategy.validate()?;
    validate_user_token_component(user_token)?;

    let cutoff_ms = now_ms.saturating_sub(NINETY_DAYS_MS);
    let escaped_user_token = sanitize_sql_eq(user_token);
    let mut resolved_events: Vec<ResolvedInsightEvent> = Vec::new();

    let indices = analytics_engine.list_analytics_indices()?;
    for index_name in indices {
        let sql = format!(
            "SELECT event_type, event_name, object_ids, timestamp_ms FROM events WHERE user_token = '{}' AND timestamp_ms >= {}",
            escaped_user_token, cutoff_ms
        );

        let rows = analytics_engine.query_events(&index_name, &sql).await?;
        let insight_rows = parse_insight_rows(rows);

        for row in insight_rows {
            for object_id in row.object_ids {
                let document = match manager.get_document(&index_name, &object_id) {
                    Ok(Some(doc)) => doc,
                    Ok(None) => continue,
                    Err(_) => continue,
                };

                let mut facets = BTreeMap::new();
                for facet in &strategy.facets_scoring {
                    let values = extract_facet_values(&document, &facet.facet_name);
                    if !values.is_empty() {
                        facets.insert(facet.facet_name.clone(), values);
                    }
                }

                if facets.is_empty() {
                    continue;
                }

                resolved_events.push(ResolvedInsightEvent {
                    event_type: row.event_type.clone(),
                    event_name: row.event_name.clone(),
                    timestamp_ms: row.timestamp_ms,
                    facets,
                });
            }
        }
    }

    Ok(compute_profile(strategy, &resolved_events, now_ms))
}

#[cfg(feature = "analytics")]
pub async fn compute_and_cache_profile(
    store: &PersonalizationProfileStore,
    manager: &crate::IndexManager,
    analytics_engine: &crate::analytics::AnalyticsQueryEngine,
    strategy: &PersonalizationStrategy,
    user_token: &str,
    now_ms: i64,
) -> Result<PersonalizationProfile, String> {
    let computed =
        compute_profile_from_storage(manager, analytics_engine, strategy, user_token, now_ms)
            .await?;
    let profile = computed.to_profile(user_token);
    store.save_profile(&profile)?;
    Ok(profile)
}

#[cfg(feature = "analytics")]
fn sanitize_sql_eq(s: &str) -> String {
    s.replace('\'', "''")
}

/// Parse JSON rows from analytics into InsightRow structs, skipping rows with missing or invalid fields.
#[cfg(feature = "analytics")]
fn parse_insight_rows(rows: Vec<serde_json::Value>) -> Vec<InsightRow> {
    let mut out = Vec::new();

    for row in rows {
        let Some(obj) = row.as_object() else {
            continue;
        };

        let Some(event_type) = obj
            .get("event_type")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };

        let Some(event_name) = obj
            .get("event_name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };

        let Some(timestamp_ms) = obj.get("timestamp_ms").and_then(parse_timestamp_ms) else {
            continue;
        };

        let object_ids = obj
            .get("object_ids")
            .map(parse_object_ids)
            .unwrap_or_default();

        if object_ids.is_empty() {
            continue;
        }

        out.push(InsightRow {
            event_type,
            event_name,
            timestamp_ms,
            object_ids,
        });
    }

    out
}

#[cfg(feature = "analytics")]
fn parse_timestamp_ms(value: &serde_json::Value) -> Option<i64> {
    if let Some(ms) = value.as_i64() {
        return Some(ms);
    }
    if let Some(ms) = value.as_u64() {
        return i64::try_from(ms).ok();
    }
    if let Some(ms) = value.as_f64() {
        return Some(ms as i64);
    }
    value.as_str().and_then(|s| s.parse::<i64>().ok())
}

#[cfg(feature = "analytics")]
fn parse_object_ids(value: &serde_json::Value) -> Vec<String> {
    if let Some(as_str) = value.as_str() {
        return serde_json::from_str::<Vec<String>>(as_str).unwrap_or_default();
    }

    if let Some(arr) = value.as_array() {
        return arr
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect();
    }

    Vec::new()
}

/// Extract all field values for a facet path from a document, supporting nested dot notation.
///
/// Returns deduplicated string representations of Text, Facet, Integer, Float, Date fields and recursively nested Array/Object structures.
pub fn extract_facet_values(document: &Document, facet_name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let parts: Vec<&str> = facet_name.split('.').collect();
    if parts.is_empty() {
        return out;
    }

    let Some(root) = document.fields.get(parts[0]) else {
        return out;
    };

    collect_values_for_path(root, &parts[1..], &mut out);

    let mut seen = HashSet::new();
    out.retain(|v| seen.insert(v.clone()));
    out
}

/// Recursively traverse a FieldValue along a dot-separated path, collecting terminal values at each step.
fn collect_values_for_path(value: &FieldValue, remaining_path: &[&str], out: &mut Vec<String>) {
    if remaining_path.is_empty() {
        collect_terminal_values(value, out);
        return;
    }

    match value {
        FieldValue::Object(map) => {
            if let Some(next) = map.get(remaining_path[0]) {
                collect_values_for_path(next, &remaining_path[1..], out);
            }
        }
        FieldValue::Array(items) => {
            for item in items {
                collect_values_for_path(item, remaining_path, out);
            }
        }
        _ => {}
    }
}

/// Recursively collect string representations of all terminal field values, flattening nested Array and Object structures.
fn collect_terminal_values(value: &FieldValue, out: &mut Vec<String>) {
    match value {
        FieldValue::Bool(b) => out.push(b.to_string()),
        FieldValue::Text(s) | FieldValue::Facet(s) => {
            if !s.is_empty() {
                out.push(s.clone());
            }
        }
        FieldValue::Integer(i) => out.push(i.to_string()),
        FieldValue::Float(f) => {
            if f.fract() == 0.0 {
                out.push((*f as i64).to_string());
            } else {
                out.push(f.to_string());
            }
        }
        FieldValue::Date(d) => out.push(d.to_string()),
        FieldValue::Array(items) => {
            for item in items {
                collect_terminal_values(item, out);
            }
        }
        FieldValue::Object(map) => {
            for value in map.values() {
                collect_terminal_values(value, out);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct PersonalizationProfileStore {
    base_path: PathBuf,
}

impl PersonalizationProfileStore {
    pub fn new(base_path: impl AsRef<Path>) -> Self {
        Self {
            base_path: base_path.as_ref().to_path_buf(),
        }
    }

    pub fn strategy_path(&self) -> PathBuf {
        self.base_path.join(STRATEGY_FILENAME)
    }

    pub fn profile_path(&self, user_token: &str) -> Result<PathBuf, String> {
        validate_user_token_component(user_token)?;
        Ok(self
            .base_path
            .join("personalization")
            .join("profiles")
            .join(format!("{}.json", user_token)))
    }

    pub fn load_profile(&self, user_token: &str) -> Result<Option<PersonalizationProfile>, String> {
        let path = self.profile_path(user_token)?;
        if !path.exists() {
            return Ok(None);
        }

        let data = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read profile '{}': {}", path.display(), e))?;
        let profile = serde_json::from_str(&data)
            .map_err(|e| format!("failed to parse profile '{}': {}", path.display(), e))?;
        Ok(Some(profile))
    }

    /// Persist a profile to JSON on disk, creating parent directories as needed.
    ///
    /// # Arguments
    /// * `profile` - PersonalizationProfile to write
    ///
    /// # Returns
    /// Ok(()) on success; error if directory creation or serialization fails
    pub fn save_profile(&self, profile: &PersonalizationProfile) -> Result<(), String> {
        let path = self.profile_path(&profile.user_token)?;
        let dir = path
            .parent()
            .ok_or_else(|| "profile path should have a parent directory".to_string())?;
        std::fs::create_dir_all(dir).map_err(|e| {
            format!(
                "failed to create profile directory '{}': {}",
                dir.display(),
                e
            )
        })?;

        let json = serde_json::to_string_pretty(profile)
            .map_err(|e| format!("failed to serialize profile: {}", e))?;
        std::fs::write(&path, json)
            .map_err(|e| format!("failed to write profile '{}': {}", path.display(), e))?;
        Ok(())
    }

    pub fn delete_profile(&self, user_token: &str) -> Result<bool, String> {
        let path = self.profile_path(user_token)?;
        if !path.exists() {
            return Ok(false);
        }

        std::fs::remove_file(&path)
            .map_err(|e| format!("failed to delete profile '{}': {}", path.display(), e))?;
        Ok(true)
    }
}

fn validate_user_token_component(user_token: &str) -> Result<(), String> {
    if user_token.is_empty() || user_token.len() > 129 {
        return Err("userToken must be 1-129 characters".to_string());
    }
    if !user_token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("userToken must contain only [a-zA-Z0-9\\-_]".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a PersonalizationStrategy for testing with view and conversion events scoring brand and category facets.
    fn sample_strategy() -> PersonalizationStrategy {
        PersonalizationStrategy {
            events_scoring: vec![
                EventScoring {
                    event_name: "Product viewed".to_string(),
                    event_type: "view".to_string(),
                    score: 10,
                },
                EventScoring {
                    event_name: "Product purchased".to_string(),
                    event_type: "conversion".to_string(),
                    score: 50,
                },
            ],
            facets_scoring: vec![
                FacetScoring {
                    facet_name: "brand".to_string(),
                    score: 70,
                },
                FacetScoring {
                    facet_name: "category".to_string(),
                    score: 30,
                },
            ],
            personalization_impact: 80,
        }
    }

    /// Verify that profiles are correctly scored and normalized to 0-20 range, with events older than 90 days excluded.
    #[test]
    fn compute_profile_scores_and_normalizes_to_0_20() {
        let now_ms = 1_750_000_000_000_i64;
        let events = vec![
            ResolvedInsightEvent {
                event_type: "view".to_string(),
                event_name: "Product viewed".to_string(),
                timestamp_ms: now_ms - 1_000,
                facets: BTreeMap::from([
                    ("brand".to_string(), vec!["Nike".to_string()]),
                    ("category".to_string(), vec!["Shoes".to_string()]),
                ]),
            },
            ResolvedInsightEvent {
                event_type: "view".to_string(),
                event_name: "Product viewed".to_string(),
                timestamp_ms: now_ms - 2_000,
                facets: BTreeMap::from([
                    ("brand".to_string(), vec!["Nike".to_string()]),
                    ("category".to_string(), vec!["Shoes".to_string()]),
                ]),
            },
            ResolvedInsightEvent {
                event_type: "conversion".to_string(),
                event_name: "Product purchased".to_string(),
                timestamp_ms: now_ms - 3_000,
                facets: BTreeMap::from([
                    ("brand".to_string(), vec!["Adidas".to_string()]),
                    ("category".to_string(), vec!["Shoes".to_string()]),
                ]),
            },
            // Older than 90 days: should be ignored entirely.
            ResolvedInsightEvent {
                event_type: "conversion".to_string(),
                event_name: "Product purchased".to_string(),
                timestamp_ms: now_ms - NINETY_DAYS_MS - 1_000,
                facets: BTreeMap::from([
                    ("brand".to_string(), vec!["Puma".to_string()]),
                    ("category".to_string(), vec!["Shoes".to_string()]),
                ]),
            },
        ];

        let computed = compute_profile(&sample_strategy(), &events, now_ms);

        assert_eq!(computed.scores["brand"]["Adidas"], 20);
        assert_eq!(computed.scores["brand"]["Nike"], 8);
        assert_eq!(computed.scores["category"]["Shoes"], 12);
        assert!(
            computed
                .scores
                .get("brand")
                .and_then(|m| m.get("Puma"))
                .is_none(),
            "events older than 90 days must be ignored"
        );
    }

    #[test]
    fn compute_profile_user_with_no_events_returns_empty() {
        let computed = compute_profile(&sample_strategy(), &[], 1_750_000_000_000_i64);
        assert!(computed.scores.is_empty());
        assert!(computed.last_event_at_ms.is_none());
    }

    /// Verify that the highest-scoring affinity normalizes to exactly 20 with weaker affinities proportionally lower.
    #[test]
    fn compute_profile_normalization_strongest_is_exactly_20() {
        let now_ms = 1_750_000_000_000_i64;
        let events = vec![
            ResolvedInsightEvent {
                event_type: "conversion".to_string(),
                event_name: "Product purchased".to_string(),
                timestamp_ms: now_ms - 1_000,
                facets: BTreeMap::from([("brand".to_string(), vec!["Adidas".to_string()])]),
            },
            ResolvedInsightEvent {
                event_type: "view".to_string(),
                event_name: "Product viewed".to_string(),
                timestamp_ms: now_ms - 2_000,
                facets: BTreeMap::from([("brand".to_string(), vec!["Nike".to_string()])]),
            },
        ];

        let computed = compute_profile(&sample_strategy(), &events, now_ms);
        assert_eq!(computed.scores["brand"]["Adidas"], 20);
        assert!(
            computed.scores["brand"]["Nike"] < 20,
            "weaker affinities should be lower than strongest"
        );
    }

    /// Verify that nested dot notation paths like 'categories.lvl1' are correctly extracted from document fields.
    #[test]
    fn extract_facet_values_supports_nested_paths() {
        let doc = Document {
            id: "p1".to_string(),
            fields: HashMap::from([
                ("brand".to_string(), FieldValue::Text("Nike".to_string())),
                (
                    "categories".to_string(),
                    FieldValue::Object(HashMap::from([
                        ("lvl0".to_string(), FieldValue::Text("Shoes".to_string())),
                        (
                            "lvl1".to_string(),
                            FieldValue::Text("Shoes > Running".to_string()),
                        ),
                    ])),
                ),
            ]),
        };

        assert_eq!(
            extract_facet_values(&doc, "brand"),
            vec!["Nike".to_string()]
        );
        assert_eq!(
            extract_facet_values(&doc, "categories.lvl1"),
            vec!["Shoes > Running".to_string()]
        );
    }

    /// Verify that profiles persist to disk correctly and can be loaded, updated, and deleted.
    #[test]
    fn profile_store_round_trips_profile_json() {
        let tmp = TempDir::new().unwrap();
        let store = PersonalizationProfileStore::new(tmp.path());

        let profile = PersonalizationProfile {
            user_token: "user-123".to_string(),
            last_event_at: Some("2026-02-25T00:00:00Z".to_string()),
            scores: BTreeMap::from([(
                "brand".to_string(),
                BTreeMap::from([("Nike".to_string(), 20)]),
            )]),
        };

        store.save_profile(&profile).unwrap();
        let loaded = store.load_profile("user-123").unwrap().unwrap();
        assert_eq!(loaded, profile);
        assert!(store.delete_profile("user-123").unwrap());
        assert!(store.load_profile("user-123").unwrap().is_none());
    }
}
