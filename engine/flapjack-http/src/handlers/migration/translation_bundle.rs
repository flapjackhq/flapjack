use crate::handlers::settings::{
    payload_merge::{merge_non_topology_settings_payload, validate_and_apply_replicas},
    SetSettingsRequest,
};
use flapjack::index::replica::{parse_replica_entry, validate_replicas, ReplicaEntry};
use flapjack::index::rules::Rule;
use flapjack::index::settings::{
    is_known_source_ranking_token, is_ranking_criterion_token, parse_custom_ranking_token,
    IndexSettings,
};
use flapjack::index::synonyms::Synonym;
use flapjack::types::Document;
use serde::de::{DeserializeOwned, IntoDeserializer};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

use super::push_unique_entry;
use super::translation_report::{
    hard_entry, warning_entry, ReportCode, ReportResource, TranslationReportEntry,
};

pub(super) const MATCHING_CRITICAL_REPLICA_FIELDS: [&str; 14] = [
    "attributesForFaceting",
    "camelCaseAttributes",
    "customNormalization",
    "decompoundedAttributes",
    "disableExactOnAttributes",
    "disablePrefixOnAttributes",
    "disableTypoToleranceOnAttributes",
    "disableTypoToleranceOnWords",
    "indexLanguages",
    "keepDiacriticsOnCharacters",
    "numericAttributesForFiltering",
    "optionalWords",
    "searchableAttributes",
    "separatorsToIndex",
];
const NUMERIC_ATTRIBUTES_FOR_FILTERING_FIELD: &str = "numericAttributesForFiltering";
const NUMERIC_ATTRIBUTES_TO_INDEX_ALIAS: &str = "numericAttributesToIndex";
const SEARCHABLE_ATTRIBUTES_FIELD: &str = "searchableAttributes";
const ATTRIBUTES_TO_INDEX_ALIAS: &str = "attributesToIndex";
const EMPTY_LIST_MATCHES_OMITTED_FIELDS: [&str; 3] = [
    "disableExactOnAttributes",
    "disableTypoToleranceOnAttributes",
    "disableTypoToleranceOnWords",
];

#[derive(Debug, Clone)]
pub(in crate::handlers::migration) struct TranslationBundle {
    pub(in crate::handlers::migration) settings: IndexSettings,
    pub(in crate::handlers::migration) replica_settings: Vec<ReplicaSettingsTranslation>,
    pub(in crate::handlers::migration) rules: Vec<Rule>,
    pub(in crate::handlers::migration) synonyms: Vec<Synonym>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TypedTranslationFailure {
    pub(super) code: ReportCode,
    pub(super) resource: ReportResource,
    pub(super) page_index: Option<usize>,
    pub(super) item_index: Option<usize>,
    pub(super) json_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ReplicaTopologyTranslation {
    pub(super) source_entry: ReplicaEntry,
    pub(super) source_replica_name: String,
    pub(super) derived_entry: ReplicaEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReplicaTopologyTranslationError {
    pub(super) derived_target_name: String,
    pub(super) colliding_source_replica_names: Vec<String>,
}

#[derive(Debug, Clone)]
pub(in crate::handlers::migration) struct ReplicaSettingsTranslation {
    pub(in crate::handlers::migration) source_name: String,
    pub(in crate::handlers::migration) source_entry: ReplicaEntry,
    pub(in crate::handlers::migration) derived_entry: ReplicaEntry,
    pub(in crate::handlers::migration) settings: IndexSettings,
    pub(in crate::handlers::migration) source_relevancy_strictness: Option<u32>,
}

#[derive(Debug, Clone)]
pub(super) struct ReplicaSettingsTranslationResult {
    pub(super) replicas: Vec<ReplicaSettingsTranslation>,
    pub(super) report_entries: Vec<TranslationReportEntry>,
}

pub(super) struct PrimaryReplicaApplication {
    pub(super) replica_settings: Vec<ReplicaSettingsTranslation>,
    pub(super) report_entries: Vec<TranslationReportEntry>,
}

/// Translates the primary's replica topology and owned settings into the migration bundle.
pub(super) fn translate_and_apply_primary_replicas(
    settings: &mut IndexSettings,
    primary_settings_value: &Value,
    replica_settings: &BTreeMap<String, Value>,
    source_index_name: &str,
    target_index_name: &str,
) -> PrimaryReplicaApplication {
    let topology = match translate_replica_topology(
        primary_settings_value,
        source_index_name,
        target_index_name,
    ) {
        Ok(topology) => topology,
        Err(error) => {
            return PrimaryReplicaApplication {
                replica_settings: Vec::new(),
                report_entries: untranslatable_topology_entries(&error),
            };
        }
    };
    let translated =
        translate_replica_settings(primary_settings_value, replica_settings, &topology);
    let derived_replicas = translated
        .replicas
        .iter()
        .map(|replica| replica_entry_source_string(&replica.derived_entry))
        .collect::<Vec<_>>();

    let mut report_entries = translated.report_entries;
    if validate_and_apply_replicas(settings, Some(derived_replicas), target_index_name).is_err() {
        for entry in untranslatable_topology_entries(&annotate_replica_validation_error(
            target_index_name,
            &topology,
        )) {
            push_unique_entry(&mut report_entries, entry);
        }
    }

    PrimaryReplicaApplication {
        replica_settings: translated.replicas,
        report_entries,
    }
}

/// Topology that cannot be translated is the one case `ReplicaTopologyNotMigrated`
/// still owns after Stage 4. Name every replica the operator has to change: the
/// colliding source names when a derivation collapses two of them onto one target
/// name, otherwise the derived target name itself.
fn untranslatable_topology_entries(
    error: &ReplicaTopologyTranslationError,
) -> Vec<TranslationReportEntry> {
    let offending_names = if error.colliding_source_replica_names.is_empty() {
        std::slice::from_ref(&error.derived_target_name)
    } else {
        error.colliding_source_replica_names.as_slice()
    };
    offending_names
        .iter()
        .map(|name| {
            hard_entry(
                ReportCode::ReplicaTopologyNotMigrated,
                ReportResource::Settings,
                None,
                None,
                &format!(r#"$.replicas["{name}"]"#),
            )
        })
        .collect()
}

pub(in crate::handlers::migration) fn replica_entry_source_string(entry: &ReplicaEntry) -> String {
    match entry {
        ReplicaEntry::Standard(name) => name.clone(),
        ReplicaEntry::Virtual(name) => format!("virtual({name})"),
    }
}

/// Validates source topology and derives target-owned virtual replica names.
pub(super) fn translate_replica_topology(
    settings_value: &Value,
    source_index_name: &str,
    target_index_name: &str,
) -> Result<Vec<ReplicaTopologyTranslation>, ReplicaTopologyTranslationError> {
    let mut translations = Vec::new();
    let Some(raw_replicas) = settings_value.get("replicas") else {
        return Ok(translations);
    };
    let raw_replicas = raw_replica_strings(raw_replicas, target_index_name)?;
    let source_entries = validate_source_replica_topology(source_index_name, &raw_replicas)?;

    for source_entry in source_entries {
        let source_replica_name = source_entry.name().to_string();
        let derived_name =
            derive_target_replica_name(&source_replica_name, source_index_name, target_index_name);
        translations.push(ReplicaTopologyTranslation {
            source_entry,
            source_replica_name,
            derived_entry: ReplicaEntry::Virtual(derived_name),
        });
    }

    validate_derived_replica_topology(target_index_name, &translations)?;
    Ok(translations)
}

/// Translates each fetched replica's settings and reports fidelity gaps.
pub(super) fn translate_replica_settings(
    primary_settings_value: &Value,
    replica_settings: &BTreeMap<String, Value>,
    topology: &[ReplicaTopologyTranslation],
) -> ReplicaSettingsTranslationResult {
    let mut replicas = Vec::new();
    let mut report_entries = Vec::new();
    let primary_matching_values = normalized_matching_critical_values(primary_settings_value);

    warn_primary_relevancy_strictness(primary_settings_value, topology, &mut report_entries);
    for (topology_index, topology_entry) in topology.iter().enumerate() {
        warn_standard_to_virtual(topology_index, topology_entry, &mut report_entries);
        warn_strictness_semantic_mismatch(topology_entry, replica_settings, &mut report_entries);
        warn_matching_critical_divergence(
            &primary_matching_values,
            topology_entry,
            replica_settings,
            &mut report_entries,
        );

        let Some(replica_settings_value) =
            replica_settings.get(&topology_entry.source_replica_name)
        else {
            push_unique_entry(
                &mut report_entries,
                hard_entry(
                    ReportCode::MalformedSettingsPayload,
                    ReportResource::Settings,
                    None,
                    None,
                    &replica_settings_path(&topology_entry.source_replica_name),
                ),
            );
            continue;
        };

        let mut failures = Vec::new();
        let Some(mut settings) = translate_settings(replica_settings_value, &mut failures) else {
            push_prefixed_failures(
                &mut report_entries,
                &topology_entry.source_replica_name,
                failures,
            );
            continue;
        };
        if !failures.is_empty() {
            push_prefixed_failures(
                &mut report_entries,
                &topology_entry.source_replica_name,
                failures,
            );
            continue;
        }

        normalize_replica_ranking(
            &mut settings,
            replica_settings_value,
            &topology_entry.source_replica_name,
            &mut report_entries,
        );
        let source_relevancy_strictness = source_relevancy_strictness(replica_settings_value);
        settings.relevancy_strictness = source_relevancy_strictness;

        replicas.push(ReplicaSettingsTranslation {
            source_name: topology_entry.source_replica_name.clone(),
            source_entry: topology_entry.source_entry.clone(),
            derived_entry: topology_entry.derived_entry.clone(),
            settings,
            source_relevancy_strictness,
        });
    }

    ReplicaSettingsTranslationResult {
        replicas,
        report_entries,
    }
}

/// Warns when primary relevancyStrictness cannot be inherited by translated replicas.
fn warn_primary_relevancy_strictness(
    primary_settings_value: &Value,
    topology: &[ReplicaTopologyTranslation],
    report_entries: &mut Vec<TranslationReportEntry>,
) {
    if !topology.is_empty()
        && primary_settings_value
            .get("relevancyStrictness")
            .is_some_and(|value| !value.is_null())
    {
        push_unique_entry(
            report_entries,
            warning_entry(
                ReportCode::ReplicaPrimaryRelevancyStrictnessDropped,
                ReportResource::Settings,
                None,
                None,
                "$.relevancyStrictness",
            ),
        );
    }
}

/// Warns when a standard replica loses exhaustive-sort semantics as a virtual replica.
fn warn_standard_to_virtual(
    topology_index: usize,
    topology_entry: &ReplicaTopologyTranslation,
    report_entries: &mut Vec<TranslationReportEntry>,
) {
    if matches!(topology_entry.source_entry, ReplicaEntry::Standard(_)) {
        push_unique_entry(
            report_entries,
            warning_entry(
                ReportCode::ReplicaExhaustiveSortApproximated,
                ReportResource::Settings,
                None,
                None,
                &format!("$.replicas[{topology_index}]"),
            ),
        );
    }
}

/// Warns for fetched replicas because the vendors' relevancyStrictness semantics differ.
fn warn_strictness_semantic_mismatch(
    topology_entry: &ReplicaTopologyTranslation,
    replica_settings: &BTreeMap<String, Value>,
    report_entries: &mut Vec<TranslationReportEntry>,
) {
    if replica_settings.contains_key(&topology_entry.source_replica_name) {
        push_unique_entry(
            report_entries,
            warning_entry(
                ReportCode::ReplicaRelevancyStrictnessSemanticMismatch,
                ReportResource::Settings,
                None,
                None,
                &replica_settings_field_path(
                    &topology_entry.source_replica_name,
                    "relevancyStrictness",
                ),
            ),
        );
    }
}

/// Warns when replica-owned matching fields differ from the primary's effective values.
fn warn_matching_critical_divergence(
    primary_matching_values: &BTreeMap<&'static str, Option<Value>>,
    topology_entry: &ReplicaTopologyTranslation,
    replica_settings: &BTreeMap<String, Value>,
    report_entries: &mut Vec<TranslationReportEntry>,
) {
    let Some(replica_settings_value) = replica_settings.get(&topology_entry.source_replica_name)
    else {
        return;
    };
    let replica_matching_values = normalized_matching_critical_values(replica_settings_value);

    for field in MATCHING_CRITICAL_REPLICA_FIELDS {
        if primary_matching_values.get(field) != replica_matching_values.get(field) {
            push_unique_entry(
                report_entries,
                warning_entry(
                    ReportCode::ReplicaMatchingCriticalFieldDiverges,
                    ReportResource::Settings,
                    None,
                    None,
                    &replica_settings_field_path(&topology_entry.source_replica_name, field),
                ),
            );
        }
    }
}

fn normalized_matching_critical_values(
    settings_value: &Value,
) -> BTreeMap<&'static str, Option<Value>> {
    MATCHING_CRITICAL_REPLICA_FIELDS
        .into_iter()
        .map(|field| {
            (
                field,
                normalized_matching_critical_value(settings_value, field),
            )
        })
        .collect()
}

fn normalized_matching_critical_value(settings_value: &Value, field: &str) -> Option<Value> {
    let raw_value = raw_matching_critical_value(settings_value, field);
    let mut field_payload = serde_json::Map::new();
    if let Some(value) = raw_value {
        field_payload.insert(field.to_string(), value.clone());
    }

    match serde_json::from_value::<IndexSettings>(Value::Object(field_payload)) {
        Ok(settings) => {
            let normalized = serde_json::to_value(settings)
                .ok()
                .and_then(|settings| settings.get(field).cloned())
                .and_then(|value| normalized_default_equivalent_value(field, value));
            if normalized.is_none() && is_raw_only_matching_critical_field(field) {
                return raw_default_equivalent_value(raw_value);
            }
            normalized
        }
        Err(_) => raw_default_equivalent_value(raw_value),
    }
}

fn raw_matching_critical_value<'a>(settings_value: &'a Value, field: &str) -> Option<&'a Value> {
    if field == NUMERIC_ATTRIBUTES_FOR_FILTERING_FIELD {
        return settings_value
            .get(NUMERIC_ATTRIBUTES_FOR_FILTERING_FIELD)
            .or_else(|| settings_value.get(NUMERIC_ATTRIBUTES_TO_INDEX_ALIAS));
    }
    if field == SEARCHABLE_ATTRIBUTES_FIELD {
        return settings_value
            .get(SEARCHABLE_ATTRIBUTES_FIELD)
            .or_else(|| settings_value.get(ATTRIBUTES_TO_INDEX_ALIAS));
    }
    settings_value.get(field)
}

fn is_raw_only_matching_critical_field(field: &str) -> bool {
    field == "disablePrefixOnAttributes"
}

fn normalized_default_equivalent_value(field: &str, value: Value) -> Option<Value> {
    if EMPTY_LIST_MATCHES_OMITTED_FIELDS.contains(&field)
        && value.as_array().is_some_and(Vec::is_empty)
    {
        return None;
    }
    Some(value)
}

fn raw_default_equivalent_value(raw_value: Option<&Value>) -> Option<Value> {
    match raw_value {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.clone()),
    }
}

fn push_prefixed_failures(
    report_entries: &mut Vec<TranslationReportEntry>,
    source_replica_name: &str,
    failures: Vec<TypedTranslationFailure>,
) {
    for failure in failures {
        push_unique_entry(
            report_entries,
            hard_entry(
                failure.code,
                failure.resource,
                failure.page_index,
                failure.item_index,
                &prefix_replica_settings_path(source_replica_name, &failure.json_path),
            ),
        );
    }
}

/// Lifts replica ranking sort tokens ahead of enabled custom ranking criteria.
fn normalize_replica_ranking(
    settings: &mut IndexSettings,
    replica_settings_value: &Value,
    source_replica_name: &str,
    report_entries: &mut Vec<TranslationReportEntry>,
) {
    let lifted_custom_ranking =
        lifted_custom_ranking_tokens(replica_settings_value, source_replica_name, report_entries);
    let recognized_ranking = recognized_ranking_tokens(replica_settings_value);
    let custom_ranking_is_enabled = source_ranking_enables_custom_ranking(replica_settings_value);

    if let Some(ranking) = recognized_ranking {
        settings.ranking = Some(ranking);
    }

    let existing_custom_ranking = settings.custom_ranking.take();
    let mut custom_ranking = lifted_custom_ranking;
    if custom_ranking_is_enabled {
        if let Some(existing) = existing_custom_ranking {
            custom_ranking.extend(existing);
        }
    }
    settings.custom_ranking = (!custom_ranking.is_empty()).then_some(custom_ranking);
}

fn source_ranking_enables_custom_ranking(replica_settings_value: &Value) -> bool {
    ranking_array(replica_settings_value)
        .map(|ranking| ranking.contains(&"custom"))
        .unwrap_or(true)
}

fn lifted_custom_ranking_tokens(
    replica_settings_value: &Value,
    source_replica_name: &str,
    report_entries: &mut Vec<TranslationReportEntry>,
) -> Vec<String> {
    ranking_array(replica_settings_value)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, token)| {
            if parse_custom_ranking_token(token).is_some() {
                return Some(token.to_string());
            }
            if !is_known_source_ranking_token(token) {
                push_unique_entry(
                    report_entries,
                    warning_entry(
                        ReportCode::ReplicaUnknownRankingToken,
                        ReportResource::Settings,
                        None,
                        None,
                        &replica_settings_indexed_field_path(source_replica_name, "ranking", index),
                    ),
                );
            }
            None
        })
        .collect()
}

fn recognized_ranking_tokens(replica_settings_value: &Value) -> Option<Vec<String>> {
    ranking_array(replica_settings_value).map(|ranking| {
        ranking
            .iter()
            .filter(|token| is_ranking_criterion_token(token))
            .map(|token| (*token).to_string())
            .collect()
    })
}

fn ranking_array(replica_settings_value: &Value) -> Option<Vec<&str>> {
    replica_settings_value
        .get("ranking")
        .and_then(Value::as_array)
        .map(|ranking| ranking.iter().filter_map(Value::as_str).collect())
}

fn source_relevancy_strictness(replica_settings_value: &Value) -> Option<u32> {
    replica_settings_value
        .get("relevancyStrictness")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn prefix_replica_settings_path(source_replica_name: &str, path: &str) -> String {
    let root = replica_settings_path(source_replica_name);
    match path.strip_prefix("$.") {
        Some(suffix) => format!("{root}.{suffix}"),
        None if path == "$" => root,
        None => format!("{root}{path}"),
    }
}

fn replica_settings_path(source_replica_name: &str) -> String {
    format!(r#"$.replicaSettings["{source_replica_name}"]"#)
}

fn replica_settings_field_path(source_replica_name: &str, field: &str) -> String {
    format!(r#"$.replicaSettings["{source_replica_name}"].{field}"#)
}

fn replica_settings_indexed_field_path(
    source_replica_name: &str,
    field: &str,
    index: usize,
) -> String {
    format!(r#"$.replicaSettings["{source_replica_name}"].{field}[{index}]"#)
}

fn raw_replica_strings(
    raw_replicas: &Value,
    target_index_name: &str,
) -> Result<Vec<String>, ReplicaTopologyTranslationError> {
    let Some(raw_replicas) = raw_replicas.as_array() else {
        return Err(replica_topology_error(target_index_name));
    };

    let mut replica_strings = Vec::with_capacity(raw_replicas.len());
    for raw_replica in raw_replicas {
        let Some(raw_replica) = raw_replica.as_str() else {
            return Err(replica_topology_error(target_index_name));
        };
        replica_strings.push(raw_replica.to_string());
    }

    Ok(replica_strings)
}

fn validate_source_replica_topology(
    source_index_name: &str,
    raw_replicas: &[String],
) -> Result<Vec<ReplicaEntry>, ReplicaTopologyTranslationError> {
    validate_replicas(source_index_name, raw_replicas)
        .map_err(|_| source_replica_topology_error(source_index_name, raw_replicas))
}

pub(super) fn translate_settings(
    settings_value: &Value,
    failures: &mut Vec<TypedTranslationFailure>,
) -> Option<IndexSettings> {
    let mut settings = IndexSettings::default();
    let settings_value = fold_deprecated_settings_aliases(settings_value);
    let mut payload = match deserialize_with_path::<SetSettingsRequest>(settings_value) {
        Ok(payload) => payload,
        Err(json_path) => {
            failures.push(failure(
                ReportCode::MalformedSettingsPayload,
                ReportResource::Settings,
                None,
                None,
                json_path,
            ));
            return None;
        }
    };

    if merge_non_topology_settings_payload(&mut settings, &mut payload).is_err() {
        failures.push(failure(
            ReportCode::MalformedSettingsPayload,
            ReportResource::Settings,
            None,
            None,
            "$.distinct".to_string(),
        ));
        return None;
    }
    settings.relevancy_strictness = None;
    Some(settings)
}

fn fold_deprecated_settings_aliases(settings_value: &Value) -> Value {
    let Some(settings_object) = settings_value.as_object() else {
        return settings_value.clone();
    };
    let mut folded = settings_object.clone();
    if let Some(alias_value) = folded.remove(ATTRIBUTES_TO_INDEX_ALIAS) {
        folded
            .entry(SEARCHABLE_ATTRIBUTES_FIELD.to_string())
            .or_insert(alias_value);
    }
    Value::Object(folded)
}

pub(super) fn translate_document(
    document: &Value,
    page_index: usize,
    item_index: usize,
    failures: &mut Vec<TypedTranslationFailure>,
) -> Option<Document> {
    match Document::from_json(document) {
        Ok(document) => Some(document),
        Err(_) => {
            failures.push(failure(
                ReportCode::MalformedDocumentPayload,
                ReportResource::Document,
                Some(page_index),
                Some(item_index),
                "$".to_string(),
            ));
            None
        }
    }
}

pub(super) fn translate_serde_value<T: DeserializeOwned>(
    value: &Value,
    page_index: usize,
    item_index: usize,
    code: ReportCode,
    resource: ReportResource,
    failures: &mut Vec<TypedTranslationFailure>,
) -> Option<T> {
    match deserialize_with_path(value.clone()) {
        Ok(value) => Some(value),
        Err(json_path) => {
            failures.push(failure(
                code,
                resource,
                Some(page_index),
                Some(item_index),
                json_path,
            ));
            None
        }
    }
}

fn deserialize_with_path<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_path_to_error::deserialize(value.into_deserializer())
        .map_err(|error| canonical_json_path(&error.path().to_string()))
}

fn canonical_json_path(path: &str) -> String {
    if path.is_empty() || path == "." {
        "$".to_string()
    } else if path.starts_with('[') {
        format!("${path}")
    } else {
        format!("$.{path}")
    }
}

fn failure(
    code: ReportCode,
    resource: ReportResource,
    page_index: Option<usize>,
    item_index: Option<usize>,
    json_path: String,
) -> TypedTranslationFailure {
    TypedTranslationFailure {
        code,
        resource,
        page_index,
        item_index,
        json_path,
    }
}

fn derive_target_replica_name(
    source_replica_name: &str,
    source_index_name: &str,
    target_index_name: &str,
) -> String {
    for separator in ["_", "-"] {
        let source_prefix = format!("{source_index_name}{separator}");
        if let Some(suffix) = source_replica_name.strip_prefix(&source_prefix) {
            return format!("{target_index_name}{separator}{suffix}");
        }
    }
    source_replica_name.to_string()
}

fn validate_derived_replica_topology(
    target_index_name: &str,
    translations: &[ReplicaTopologyTranslation],
) -> Result<(), ReplicaTopologyTranslationError> {
    let derived_entries = translations
        .iter()
        .map(canonical_virtual_replica_string)
        .collect::<Vec<_>>();

    validate_replicas(target_index_name, &derived_entries)
        .map(|_| ())
        .map_err(|_| annotate_replica_validation_error(target_index_name, translations))
}

fn canonical_virtual_replica_string(translation: &ReplicaTopologyTranslation) -> String {
    format!("virtual({})", translation.derived_entry.name())
}

fn annotate_replica_validation_error(
    target_index_name: &str,
    translations: &[ReplicaTopologyTranslation],
) -> ReplicaTopologyTranslationError {
    let derived_target_name = first_invalid_derived_replica_name(target_index_name, translations)
        .or_else(|| {
            translations
                .first()
                .map(|entry| entry.derived_entry.name().to_string())
        })
        .unwrap_or_default();
    let colliding_source_replica_names =
        colliding_source_names(&derived_target_name, translations).unwrap_or_default();

    ReplicaTopologyTranslationError {
        derived_target_name,
        colliding_source_replica_names,
    }
}

fn source_replica_topology_error(
    source_index_name: &str,
    raw_replicas: &[String],
) -> ReplicaTopologyTranslationError {
    let source_replica_name = first_invalid_source_replica_name(source_index_name, raw_replicas)
        .unwrap_or_else(|| source_index_name.to_string());
    replica_topology_error(&source_replica_name)
}

fn first_invalid_source_replica_name(
    source_index_name: &str,
    raw_replicas: &[String],
) -> Option<String> {
    let mut seen_names = HashSet::new();

    for raw_replica in raw_replicas {
        let source_entry = match parse_replica_entry(raw_replica) {
            Ok(source_entry) => source_entry,
            Err(_) => return Some(raw_replica.clone()),
        };
        let source_replica_name = source_entry.name();

        if source_replica_name == source_index_name {
            return Some(source_replica_name.to_string());
        }

        if !seen_names.insert(source_replica_name.to_string()) {
            return Some(source_replica_name.to_string());
        }
    }

    None
}

fn first_invalid_derived_replica_name(
    target_index_name: &str,
    translations: &[ReplicaTopologyTranslation],
) -> Option<String> {
    let mut seen_names = HashSet::new();

    for translation in translations {
        let derived_target_name = translation.derived_entry.name();

        if derived_target_name == target_index_name {
            return Some(derived_target_name.to_string());
        }

        if !seen_names.insert(derived_target_name.to_string()) {
            return Some(derived_target_name.to_string());
        }
    }

    None
}

fn colliding_source_names(
    derived_target_name: &str,
    translations: &[ReplicaTopologyTranslation],
) -> Option<Vec<String>> {
    let mut source_names_by_derived_name: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for translation in translations {
        source_names_by_derived_name
            .entry(translation.derived_entry.name())
            .or_default()
            .push(translation.source_replica_name.clone());
    }
    source_names_by_derived_name
        .remove(derived_target_name)
        .filter(|source_names| source_names.len() > 1)
}

fn replica_topology_error(derived_target_name: &str) -> ReplicaTopologyTranslationError {
    ReplicaTopologyTranslationError {
        derived_target_name: derived_target_name.to_string(),
        colliding_source_replica_names: Vec::new(),
    }
}
