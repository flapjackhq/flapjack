//! Canonical normalizer for Typesense collection settings. It is the single owner
//! of the accepted-key table, strict payload validation, the field-eligibility
//! rule, and attributed warnings, mirroring `meilisearch_settings.rs`. It emits an
//! Algolia-shaped settings value that the shared `translate_settings` constructor
//! turns into `IndexSettings`; per-field schema flags are the only writers of
//! `attributesForFaceting`/`searchableAttributes`.

use super::translation_report::{
    warning_entry, ReportCode, ReportResource, TranslationReportEntry,
};
use super::typesense_field_validation::{
    settings_error, validate_fields_or_null, validate_shape, TypesenseSettingsError,
};
use serde_json::{Map, Value};

/// Validator for one accepted top-level Typesense settings key. A key outside this
/// table, or a value the validator rejects, fails the payload closed rather than
/// being silently dropped.
type SettingValidator = fn(&Value, &str) -> Result<(), TypesenseSettingsError>;
type SemanticValuePredicate = fn(&Value) -> bool;

/// The ten collection settings keys this migration understands. `fields` drives the
/// translated faceting/searchable lists; the rest are captured for attribution.
const ACCEPTED_SETTING_FIELDS: [(&str, SettingValidator); 10] = [
    ("default_sorting_field", validate_string_or_null),
    ("enable_nested_fields", validate_bool_or_null),
    ("fields", validate_fields_or_null),
    ("token_separators", validate_string_array_or_null),
    ("symbols_to_index", validate_string_array_or_null),
    ("synonym_sets", validate_string_array_or_null),
    ("curation_sets", validate_string_array_or_null),
    ("metadata", validate_object_or_null),
    ("facet_by", validate_string_array_or_null),
    ("query_by", validate_string_or_null),
];

/// Collection-level concepts that have no lossless Flapjack equivalent, warned at
/// `$.<key>` when the source supplies a semantically meaningful value.
/// `enable_nested_fields` and `fields` are deliberately absent: the former is a
/// translation control flag and the latter is attributed per offending field.
const UNMAPPED_COLLECTION_CONCEPTS: [&str; 8] = [
    "default_sorting_field",
    "token_separators",
    "symbols_to_index",
    "synonym_sets",
    "curation_sets",
    "metadata",
    "facet_by",
    "query_by",
];

/// Non-default field options whose source behavior cannot be preserved by the
/// target settings model. Default-valued response metadata is accepted quietly.
const UNMAPPED_FIELD_MEMBERS: [(&str, SemanticValuePredicate); 7] = [
    ("locale", is_non_empty_string),
    ("infix", is_true),
    ("stem", is_true),
    ("stem_dictionary", is_non_empty_string),
    ("token_separators", is_non_empty_array),
    ("symbols_to_index", is_non_empty_array),
    ("truncate_len", is_non_default_truncate_len),
];

const DEFAULT_TRUNCATE_LENGTH: u64 = 100;

pub(super) struct NormalizedTypesenseSettings {
    pub(super) value: Value,
    pub(super) warnings: Vec<TranslationReportEntry>,
}

pub(super) fn normalize_typesense_settings(
    raw: &Value,
) -> Result<NormalizedTypesenseSettings, TypesenseSettingsError> {
    let source = raw.as_object().ok_or_else(|| settings_error("$"))?;
    validate_accepted_fields(source)?;

    let attributes = derive_attributes(source);
    let mut normalized = Map::new();
    insert_when_non_empty(
        &mut normalized,
        "attributesForFaceting",
        attributes.faceting,
    );
    if source.get("fields").is_some_and(Value::is_array) {
        insert_attribute_list(
            &mut normalized,
            "searchableAttributes",
            attributes.searchable,
        );
    }

    Ok(NormalizedTypesenseSettings {
        value: Value::Object(normalized),
        warnings: build_warnings(source),
    })
}

/// Fails closed on any unknown top-level key or malformed value for an accepted
/// key, so strict validation lives in exactly one place.
fn validate_accepted_fields(source: &Map<String, Value>) -> Result<(), TypesenseSettingsError> {
    for (field, value) in source {
        let Some(validator) = accepted_setting_validator(field) else {
            return Err(settings_error(&field_path(field)));
        };
        validator(value, &field_path(field))?;
    }
    Ok(())
}

fn accepted_setting_validator(field: &str) -> Option<SettingValidator> {
    ACCEPTED_SETTING_FIELDS
        .iter()
        .find_map(|(known_field, validator)| (*known_field == field).then_some(*validator))
}

struct DerivedAttributes {
    faceting: Vec<String>,
    searchable: Vec<String>,
}

/// Derives the Algolia-shaped faceting/searchable lists from per-field schema flags
/// in source-schema order. A reference relationship does not make the field's
/// independent facet flag untranslatable, but vector/reference fields remain
/// ineligible for full-text search. Regex fields contribute to neither static list;
/// nested (dotted) leaves are eligible only when the collection enables nested
/// fields.
fn derive_attributes(source: &Map<String, Value>) -> DerivedAttributes {
    let nested_enabled = source
        .get("enable_nested_fields")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut faceting = Vec::new();
    let mut searchable = Vec::new();

    for field in fields_array(source) {
        let Some(field) = field.as_object() else {
            continue;
        };
        let Some(name) = field.get("name").and_then(Value::as_str) else {
            continue;
        };
        if is_regex_field_name(name) {
            continue;
        }
        if is_nested_name(name) && !nested_enabled {
            continue;
        }
        if field_flag(field, "facet", false) {
            faceting.push(name.to_string());
        }
        if !has_unmapped_field_semantics(field)
            && field_flag(field, "index", true)
            && is_textual_type(field.get("type").and_then(Value::as_str))
        {
            searchable.push(name.to_string());
        }
    }

    DerivedAttributes {
        faceting,
        searchable,
    }
}

/// Attributes every unmapped concept: one warning per non-empty collection-level
/// concept plus one per unsupported field, keyed to the offending
/// `$.fields[<index>]`. Dynamic and regex fields cannot be represented losslessly
/// by a static Flapjack attribute declaration. Non-default field search behavior
/// is attributed to its exact member path.
fn build_warnings(source: &Map<String, Value>) -> Vec<TranslationReportEntry> {
    let mut warnings = Vec::new();
    for field in UNMAPPED_COLLECTION_CONCEPTS {
        if source.get(field).is_some_and(has_semantic_value) {
            warnings.push(warning(&field_path(field)));
        }
    }
    for (index, field) in fields_array(source).iter().enumerate() {
        let Some(field) = field.as_object() else {
            continue;
        };
        let field_path = format!("$.fields[{index}]");
        if has_unmapped_field_intent(field) {
            warnings.push(warning(&field_path));
        }
        for (member, has_unmapped_value) in UNMAPPED_FIELD_MEMBERS {
            if field.get(member).is_some_and(has_unmapped_value) {
                warnings.push(warning(&format!("{field_path}.{member}")));
            }
        }
    }
    warnings
}

fn fields_array(source: &Map<String, Value>) -> &[Value] {
    source
        .get("fields")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Vector and reference configuration has no Flapjack equivalent. Fields that
/// carry it are excluded from full-text search and warned, while independently
/// translatable flags such as `facet` remain eligible.
fn has_unmapped_field_semantics(field: &Map<String, Value>) -> bool {
    field.contains_key("num_dim")
        || field.contains_key("vec_dist")
        || field.contains_key("reference")
        || field.contains_key("embed")
        || field.contains_key("hnsw_params")
}

fn has_unmapped_field_intent(field: &Map<String, Value>) -> bool {
    has_unmapped_field_semantics(field)
        || field
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(is_regex_field_name)
        || (field_flag(field, "index", true)
            && field
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(is_dynamic_field_type))
}

fn is_dynamic_field_type(field_type: &str) -> bool {
    matches!(field_type, "string*" | "auto")
}

fn is_regex_field_name(name: &str) -> bool {
    name.contains(".*")
}

fn is_nested_name(name: &str) -> bool {
    name.contains('.')
}

fn is_textual_type(field_type: Option<&str>) -> bool {
    matches!(field_type, Some("string") | Some("string[]"))
}

fn field_flag(field: &Map<String, Value>, key: &str, default: bool) -> bool {
    field.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn insert_when_non_empty(target: &mut Map<String, Value>, key: &str, values: Vec<String>) {
    if !values.is_empty() {
        insert_attribute_list(target, key, values);
    }
}

fn insert_attribute_list(target: &mut Map<String, Value>, key: &str, values: Vec<String>) {
    target.insert(
        key.to_string(),
        Value::Array(values.into_iter().map(Value::String).collect()),
    );
}

/// Mirrors the "source supplied a meaningful value" test used by
/// `meilisearch_settings.rs`: null and empty collections carry no intent to warn.
fn has_semantic_value(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        _ => true,
    }
}

fn is_true(value: &Value) -> bool {
    value.as_bool() == Some(true)
}

fn is_non_empty_string(value: &Value) -> bool {
    value.as_str().is_some_and(|value| !value.is_empty())
}

fn is_non_empty_array(value: &Value) -> bool {
    value.as_array().is_some_and(|values| !values.is_empty())
}

fn is_non_default_truncate_len(value: &Value) -> bool {
    value.as_u64() != Some(DEFAULT_TRUNCATE_LENGTH)
}

fn validate_string_or_null(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(value.is_null() || value.is_string(), path)
}

fn validate_bool_or_null(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(value.is_null() || value.is_boolean(), path)
}

fn validate_object_or_null(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(value.is_null() || value.is_object(), path)
}

fn validate_string_array_or_null(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(
        value.is_null()
            || value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
        path,
    )
}

fn field_path(field: &str) -> String {
    format!("$.{field}")
}

fn warning(path: &str) -> TranslationReportEntry {
    warning_entry(
        ReportCode::TypesenseSettingNotMigrated,
        ReportResource::Settings,
        None,
        None,
        path,
    )
}
