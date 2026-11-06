//! Custom dictionaries API types and serialization for stopwords, plurals, and compounds, scoped per application rather than per index.

pub mod manager;
pub mod persistence;

use serde::{Deserialize, Serialize};

/// Default application ID for dictionary operations.
/// Core callers without application context use this application; never infer it from an index ID.
pub const DEFAULT_DICTIONARY_APPLICATION_ID: &str = "_default";
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

// ── Dictionary name enum ──────────────────────────────────────────────

/// The three dictionary types supported by the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum DictionaryName {
    Stopwords,
    Plurals,
    Compounds,
}

impl DictionaryName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stopwords => "stopwords",
            Self::Plurals => "plurals",
            Self::Compounds => "compounds",
        }
    }

    /// All valid dictionary names.
    pub fn all() -> &'static [DictionaryName] {
        &[Self::Stopwords, Self::Plurals, Self::Compounds]
    }
}

impl fmt::Display for DictionaryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DictionaryName {
    type Err = InvalidDictionaryName;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "stopwords" => Ok(Self::Stopwords),
            "plurals" => Ok(Self::Plurals),
            "compounds" => Ok(Self::Compounds),
            _ => Err(InvalidDictionaryName(s.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InvalidDictionaryName(pub String);

impl fmt::Display for InvalidDictionaryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid dictionary name '{}': must be one of stopwords, plurals, compounds",
            self.0
        )
    }
}

impl std::error::Error for InvalidDictionaryName {}

// ── Serde helpers for DictionaryName as map key ────────────────────────

impl Serialize for DictionaryName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DictionaryName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

// ── Entry types ────────────────────────────────────────────────────────

/// Entry state — "enabled" or "disabled". Custom entries default to "enabled".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum EntryState {
    #[default]
    Enabled,
    Disabled,
}

/// Entry type — "custom" for user-added entries, "standard" for built-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum EntryType {
    #[default]
    Custom,
    Standard,
}

/// A stopword dictionary entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopwordEntry {
    #[serde(rename = "objectID")]
    pub object_id: String,
    pub language: String,
    pub word: String,
    #[serde(default)]
    pub state: EntryState,
    #[serde(default, rename = "type")]
    pub entry_type: EntryType,
}

/// A plural dictionary entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluralEntry {
    #[serde(rename = "objectID")]
    pub object_id: String,
    pub language: String,
    pub words: Vec<String>,
    #[serde(default, rename = "type")]
    pub entry_type: EntryType,
}

/// A compound dictionary entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompoundEntry {
    #[serde(rename = "objectID")]
    pub object_id: String,
    pub language: String,
    pub word: String,
    pub decomposition: Vec<String>,
    #[serde(default, rename = "type")]
    pub entry_type: EntryType,
}

/// A type-erased dictionary entry for batch operations and search results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DictionaryEntry {
    Stopword(StopwordEntry),
    Plural(PluralEntry),
    Compound(CompoundEntry),
}

impl DictionaryEntry {
    pub fn object_id(&self) -> &str {
        match self {
            Self::Stopword(e) => &e.object_id,
            Self::Plural(e) => &e.object_id,
            Self::Compound(e) => &e.object_id,
        }
    }

    pub fn language(&self) -> &str {
        match self {
            Self::Stopword(e) => &e.language,
            Self::Plural(e) => &e.language,
            Self::Compound(e) => &e.language,
        }
    }
}

// ── Batch request/response ─────────────────────────────────────────────

/// Action in a batch request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum BatchAction {
    AddEntry,
    DeleteEntry,
}

/// A single operation in a batch request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BatchRequest {
    pub action: BatchAction,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub body: serde_json::Value,
}

/// Top-level batch request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BatchDictionaryRequest {
    #[serde(default)]
    pub clear_existing_dictionary_entries: bool,
    pub requests: Vec<BatchRequest>,
}

/// Mutating endpoint response (batch, set settings).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct MutationResponse {
    #[serde(rename = "taskID")]
    pub task_id: i64,
    pub updated_at: String,
}

// ── Search request/response ────────────────────────────────────────────

/// Dictionary search request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DictionarySearchRequest {
    pub query: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub page: Option<usize>,
    #[serde(default)]
    pub hits_per_page: Option<usize>,
}

/// Dictionary search response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DictionarySearchResponse {
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<Object>))]
    pub hits: Vec<serde_json::Value>,
    pub page: usize,
    pub nb_hits: usize,
    pub nb_pages: usize,
}

// ── Settings ───────────────────────────────────────────────────────────

/// Per-tenant dictionary settings.
///
/// The only field is `disableStandardEntries`, a nested map:
/// `{ "stopwords": { "en": true }, "plurals": { "fr": true }, ... }`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DictionarySettings {
    #[serde(default, deserialize_with = "deserialize_disable_standard_entries")]
    pub disable_standard_entries: HashMap<DictionaryName, HashMap<String, bool>>,
}

fn deserialize_disable_standard_entries<'de, D>(
    deserializer: D,
) -> Result<HashMap<DictionaryName, HashMap<String, bool>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<HashMap<DictionaryName, Option<HashMap<String, bool>>>>::deserialize(
        deserializer,
    )?
    .unwrap_or_default();
    Ok(raw
        .into_iter()
        .filter_map(|(dict_name, language_map)| language_map.map(|map| (dict_name, map)))
        .collect())
}

impl DictionarySettings {
    /// Check if standard entries are disabled for a given dictionary type and language.
    pub fn is_standard_disabled(&self, dict: DictionaryName, lang: &str) -> bool {
        self.disable_standard_entries
            .get(&dict)
            .and_then(|langs| langs.get(lang))
            .copied()
            .unwrap_or(false)
    }
}

// ── Languages response ─────────────────────────────────────────────────

/// Per-language custom entry counts for the languages endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LanguageDictionaryCounts {
    pub stopwords: Option<DictionaryCount>,
    pub plurals: Option<DictionaryCount>,
    pub compounds: Option<DictionaryCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DictionaryCount {
    pub nb_custom_entries: usize,
}

// ── Error type ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum DictionaryError {
    InvalidDictionaryName(InvalidDictionaryName),
    MissingObjectId,
    InvalidEntry(String),
    IoError(std::io::Error),
    SerdeError(serde_json::Error),
}

impl fmt::Display for DictionaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDictionaryName(e) => write!(f, "{}", e),
            Self::MissingObjectId => write!(f, "objectID is required on all dictionary entries"),
            Self::InvalidEntry(msg) => write!(f, "invalid dictionary entry: {}", msg),
            Self::IoError(e) => write!(f, "dictionary I/O error: {}", e),
            Self::SerdeError(e) => write!(f, "dictionary serialization error: {}", e),
        }
    }
}

impl std::error::Error for DictionaryError {}

impl From<std::io::Error> for DictionaryError {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e)
    }
}

impl From<serde_json::Error> for DictionaryError {
    fn from(e: serde_json::Error) -> Self {
        Self::SerdeError(e)
    }
}

impl From<InvalidDictionaryName> for DictionaryError {
    fn from(e: InvalidDictionaryName) -> Self {
        Self::InvalidDictionaryName(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── DictionaryName tests ──────────────────────────────────────────

    #[test]
    fn test_dictionary_name_parse_valid() {
        assert_eq!(
            "stopwords".parse::<DictionaryName>().unwrap(),
            DictionaryName::Stopwords
        );
        assert_eq!(
            "plurals".parse::<DictionaryName>().unwrap(),
            DictionaryName::Plurals
        );
        assert_eq!(
            "compounds".parse::<DictionaryName>().unwrap(),
            DictionaryName::Compounds
        );
    }

    #[test]
    fn test_dictionary_name_parse_case_insensitive() {
        assert_eq!(
            "STOPWORDS".parse::<DictionaryName>().unwrap(),
            DictionaryName::Stopwords
        );
        assert_eq!(
            "Plurals".parse::<DictionaryName>().unwrap(),
            DictionaryName::Plurals
        );
        assert_eq!(
            "COMPOUNDS".parse::<DictionaryName>().unwrap(),
            DictionaryName::Compounds
        );
    }

    #[test]
    fn test_dictionary_name_parse_invalid() {
        assert!("synonyms".parse::<DictionaryName>().is_err());
        assert!("".parse::<DictionaryName>().is_err());
        assert!("stop_words".parse::<DictionaryName>().is_err());
    }

    #[test]
    fn test_dictionary_name_display() {
        assert_eq!(DictionaryName::Stopwords.to_string(), "stopwords");
        assert_eq!(DictionaryName::Plurals.to_string(), "plurals");
        assert_eq!(DictionaryName::Compounds.to_string(), "compounds");
    }

    #[test]
    fn test_dictionary_name_roundtrip() {
        for name in DictionaryName::all() {
            let s = name.to_string();
            let parsed: DictionaryName = s.parse().unwrap();
            assert_eq!(*name, parsed);
        }
    }

    // ── Entry serialization tests ─────────────────────────────────────

    /// Verify StopwordEntry serializes to camelCase JSON with correct field renames and round-trips losslessly.
    #[test]
    fn test_stopword_entry_serde() {
        let entry = StopwordEntry {
            object_id: "sw-1".into(),
            language: "en".into(),
            word: "the".into(),
            state: EntryState::Enabled,
            entry_type: EntryType::Custom,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["objectID"], "sw-1");
        assert_eq!(json["language"], "en");
        assert_eq!(json["word"], "the");
        assert_eq!(json["state"], "enabled");
        assert_eq!(json["type"], "custom");

        let roundtrip: StopwordEntry = serde_json::from_value(json).unwrap();
        assert_eq!(roundtrip, entry);
    }

    #[test]
    fn test_plural_entry_serde() {
        let entry = PluralEntry {
            object_id: "pl-1".into(),
            language: "en".into(),
            words: vec!["mouse".into(), "mice".into()],
            entry_type: EntryType::Custom,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["objectID"], "pl-1");
        assert_eq!(json["words"], serde_json::json!(["mouse", "mice"]));
        assert_eq!(json["type"], "custom");

        let roundtrip: PluralEntry = serde_json::from_value(json).unwrap();
        assert_eq!(roundtrip, entry);
    }

    /// Verify CompoundEntry serializes to camelCase JSON with decomposition array and round-trips losslessly.
    #[test]
    fn test_compound_entry_serde() {
        let entry = CompoundEntry {
            object_id: "cp-1".into(),
            language: "de".into(),
            word: "Lebensversicherung".into(),
            decomposition: vec!["Leben".into(), "Versicherung".into()],
            entry_type: EntryType::Custom,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["objectID"], "cp-1");
        assert_eq!(json["word"], "Lebensversicherung");
        assert_eq!(
            json["decomposition"],
            serde_json::json!(["Leben", "Versicherung"])
        );

        let roundtrip: CompoundEntry = serde_json::from_value(json).unwrap();
        assert_eq!(roundtrip, entry);
    }

    // ── Settings tests ────────────────────────────────────────────────

    #[test]
    fn test_settings_default_empty() {
        let settings = DictionarySettings::default();
        assert!(!settings.is_standard_disabled(DictionaryName::Stopwords, "en"));
        assert!(!settings.is_standard_disabled(DictionaryName::Plurals, "fr"));
    }

    #[test]
    fn test_settings_disable_standard() {
        let mut settings = DictionarySettings::default();
        settings.disable_standard_entries.insert(
            DictionaryName::Stopwords,
            [("fr".to_string(), true)].into_iter().collect(),
        );

        assert!(settings.is_standard_disabled(DictionaryName::Stopwords, "fr"));
        assert!(!settings.is_standard_disabled(DictionaryName::Stopwords, "en"));
        assert!(!settings.is_standard_disabled(DictionaryName::Plurals, "fr"));
    }

    /// Verify `DictionarySettings` deserializes from the Algolia-compatible JSON shape and round-trips correctly.
    #[test]
    fn test_settings_serde_algolia_shape() {
        let json = serde_json::json!({
            "disableStandardEntries": {
                "stopwords": { "en": false, "fr": true },
                "plurals": { "de": true },
                "compounds": { "nl": true }
            }
        });
        let settings: DictionarySettings = serde_json::from_value(json.clone()).unwrap();
        assert!(settings.is_standard_disabled(DictionaryName::Stopwords, "fr"));
        assert!(!settings.is_standard_disabled(DictionaryName::Stopwords, "en"));
        assert!(settings.is_standard_disabled(DictionaryName::Plurals, "de"));
        assert!(settings.is_standard_disabled(DictionaryName::Compounds, "nl"));

        // Roundtrip
        let serialized = serde_json::to_value(&settings).unwrap();
        assert_eq!(
            serialized["disableStandardEntries"]["stopwords"]["fr"],
            true
        );
        assert_eq!(serialized["disableStandardEntries"]["plurals"]["de"], true);
    }

    /// Verify that a `null` value for a dictionary type in `disableStandardEntries` deserializes without error and is treated as unset.
    #[test]
    fn test_settings_serde_accepts_null_dictionary_type_map() {
        let json = serde_json::json!({
            "disableStandardEntries": {
                "stopwords": { "fr": true },
                "compounds": null
            }
        });

        let settings: DictionarySettings = serde_json::from_value(json).unwrap();
        assert!(settings.is_standard_disabled(DictionaryName::Stopwords, "fr"));
        assert!(
            !settings
                .disable_standard_entries
                .contains_key(&DictionaryName::Compounds),
            "null dictionary maps should be accepted and treated as unset"
        );
    }

    /// Verify `LanguageDictionaryCounts` serializes `None` variants as explicit JSON `null` rather than omitting the fields.
    #[test]
    fn test_languages_counts_serializes_null_for_missing_dictionary_types() {
        let counts = LanguageDictionaryCounts {
            stopwords: Some(DictionaryCount {
                nb_custom_entries: 2,
            }),
            plurals: None,
            compounds: None,
        };

        let json = serde_json::to_value(&counts).unwrap();
        assert_eq!(json["stopwords"]["nbCustomEntries"], 2);
        assert!(
            json.get("plurals").is_some(),
            "plurals field should be present"
        );
        assert!(
            json["plurals"].is_null(),
            "plurals should serialize as explicit null when absent"
        );
        assert!(
            json["compounds"].is_null(),
            "compounds should serialize as explicit null when absent"
        );
    }

    // ── Batch request serde ───────────────────────────────────────────

    /// Verify `BatchDictionaryRequest` deserializes camelCase JSON with mixed add/delete actions and the `clearExistingDictionaryEntries` flag.
    #[test]
    fn test_batch_request_serde() {
        let json = serde_json::json!({
            "clearExistingDictionaryEntries": true,
            "requests": [
                {
                    "action": "addEntry",
                    "body": {
                        "objectID": "sw-1",
                        "language": "en",
                        "word": "the",
                        "state": "enabled",
                        "type": "custom"
                    }
                },
                {
                    "action": "deleteEntry",
                    "body": { "objectID": "sw-2" }
                }
            ]
        });
        let req: BatchDictionaryRequest = serde_json::from_value(json).unwrap();
        assert!(req.clear_existing_dictionary_entries);
        assert_eq!(req.requests.len(), 2);
        assert_eq!(req.requests[0].action, BatchAction::AddEntry);
        assert_eq!(req.requests[1].action, BatchAction::DeleteEntry);
    }

    // ── Search request serde ──────────────────────────────────────────

    #[test]
    fn test_search_request_defaults() {
        let json = serde_json::json!({ "query": "test" });
        let req: DictionarySearchRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.query, "test");
        assert!(req.language.is_none());
        assert!(req.page.is_none());
        assert!(req.hits_per_page.is_none());
    }

    // ── MutationResponse serde ────────────────────────────────────────

    #[test]
    fn test_mutation_response_serde() {
        let resp = MutationResponse {
            task_id: 42,
            updated_at: "2026-02-24T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["taskID"], 42);
        assert_eq!(json["updatedAt"], "2026-02-24T00:00:00Z");
    }

    // ── DictionaryEntry helpers ───────────────────────────────────────

    /// Verify `DictionaryEntry::object_id` and `DictionaryEntry::language` delegate correctly for each variant.
    #[test]
    fn test_entry_object_id_and_language() {
        let sw = DictionaryEntry::Stopword(StopwordEntry {
            object_id: "sw-1".into(),
            language: "en".into(),
            word: "the".into(),
            state: EntryState::Enabled,
            entry_type: EntryType::Custom,
        });
        assert_eq!(sw.object_id(), "sw-1");
        assert_eq!(sw.language(), "en");

        let pl = DictionaryEntry::Plural(PluralEntry {
            object_id: "pl-1".into(),
            language: "fr".into(),
            words: vec!["oeil".into(), "yeux".into()],
            entry_type: EntryType::Custom,
        });
        assert_eq!(pl.object_id(), "pl-1");
        assert_eq!(pl.language(), "fr");
    }
}
