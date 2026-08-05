//! Strict validation for Typesense collection-field schemas.

use serde_json::{Map, Value};

type FieldMemberValidator = fn(&Value, &str) -> Result<(), TypesenseSettingsError>;

/// Typesense 30.2's complete collection-field type vocabulary. Keeping this
/// version-bound contract explicit prevents future or misspelled types from
/// silently altering derived target attributes.
const TYPESENSE_FIELD_TYPES: [&str; 18] = [
    "string",
    "string[]",
    "int32",
    "int32[]",
    "int64",
    "int64[]",
    "float",
    "float[]",
    "bool",
    "bool[]",
    "geopoint",
    "geopoint[]",
    "geopolygon",
    "object",
    "object[]",
    "string*",
    "image",
    "auto",
];

/// Every member emitted for a Typesense 30.2 collection field. This table is the
/// single accepted-member contract and binds each member to its exact validator.
const ACCEPTED_FIELD_MEMBERS: [(&str, FieldMemberValidator); 24] = [
    ("name", validate_non_empty_string),
    ("type", validate_field_type),
    ("facet", validate_bool),
    ("optional", validate_bool),
    ("index", validate_bool),
    ("store", validate_bool),
    ("sort", validate_bool),
    ("infix", validate_bool),
    ("locale", validate_locale),
    ("num_dim", validate_nonzero_u64),
    ("vec_dist", validate_vector_distance),
    ("reference", validate_non_empty_string),
    ("range_index", validate_bool),
    ("stem", validate_bool),
    ("stem_dictionary", validate_string),
    ("token_separators", validate_single_character_array),
    ("symbols_to_index", validate_single_character_array),
    ("truncate_len", validate_u32),
    ("embed", validate_embed),
    ("nested", validate_bool),
    ("nested_array", validate_nested_array),
    ("async_reference", validate_bool),
    ("cascade_delete", validate_bool),
    ("hnsw_params", validate_hnsw_params),
];

const EMBED_MEMBERS: [(&str, FieldMemberValidator); 3] = [
    ("from", validate_non_empty_string_array),
    ("mapping", validate_non_empty_string_array),
    ("model_config", validate_model_config),
];

const HNSW_MEMBERS: [(&str, FieldMemberValidator); 4] = [
    ("M", validate_nonzero_u64),
    ("ef_construction", validate_nonzero_u64),
    ("max_elements", validate_nonzero_u64),
    ("ef", validate_nonzero_u64),
];

const MODEL_CONFIG_STRING_MEMBERS: [&str; 16] = [
    "access_token",
    "api_key",
    "client_id",
    "client_secret",
    "document_task",
    "indexing_prefix",
    "model_name",
    "path",
    "personalization_embedding_type",
    "personalization_model_id",
    "project_id",
    "query_prefix",
    "query_task",
    "refresh_token",
    "region",
    "url",
];

const MODEL_CONFIG_PERSONALIZATION_TYPES: [&str; 1] = ["recommendation"];

/// Standard Google service-account JSON key members preserved by Typesense
/// 30.2 collection responses. Keep this exact so genuine credentials remain
/// importable without allowing arbitrary nested settings through validation.
const SERVICE_ACCOUNT_MEMBERS: [(&str, FieldMemberValidator); 11] = [
    ("type", validate_non_empty_string),
    ("project_id", validate_non_empty_string),
    ("private_key_id", validate_non_empty_string),
    ("private_key", validate_non_empty_string),
    ("client_email", validate_non_empty_string),
    ("client_id", validate_non_empty_string),
    ("auth_uri", validate_non_empty_string),
    ("token_uri", validate_non_empty_string),
    ("auth_provider_x509_cert_url", validate_non_empty_string),
    ("client_x509_cert_url", validate_non_empty_string),
    ("universe_domain", validate_non_empty_string),
];

const VECTOR_FIELD_MEMBERS: [&str; 4] = ["num_dim", "vec_dist", "embed", "hnsw_params"];
const VECTOR_INDEX_MEMBERS: [&str; 2] = ["vec_dist", "hnsw_params"];
const REFERENCE_FIELD_MEMBERS: [&str; 2] = ["async_reference", "cascade_delete"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TypesenseSettingsError {
    pub(super) json_path: String,
}

pub(super) fn validate_fields_or_null(
    value: &Value,
    path: &str,
) -> Result<(), TypesenseSettingsError> {
    if value.is_null() {
        return Ok(());
    }
    let fields = value.as_array().ok_or_else(|| settings_error(path))?;
    for (index, field) in fields.iter().enumerate() {
        validate_field(field, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

/// Every captured member is validated at its exact path. Unknown members fail
/// closed so future source semantics cannot be silently discarded.
fn validate_field(field: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    let field = field.as_object().ok_or_else(|| settings_error(path))?;
    for (member, value) in field {
        let member_path = format!("{path}.{member}");
        let Some(validator) = accepted_field_member_validator(member) else {
            return Err(settings_error(&member_path));
        };
        validator(value, &member_path)?;
    }
    require_field_member(field, "name", path)?;
    require_field_member(field, "type", path)?;
    validate_field_member_applicability(field, path)
}

fn validate_field_member_applicability(
    field: &Map<String, Value>,
    path: &str,
) -> Result<(), TypesenseSettingsError> {
    let is_float_array = field.get("type").and_then(Value::as_str) == Some("float[]");
    for member in VECTOR_FIELD_MEMBERS {
        if field.contains_key(member) && !is_float_array {
            return Err(settings_error(&format!("{path}.{member}")));
        }
    }

    let declares_vector_index = field.contains_key("num_dim") || field.contains_key("embed");
    for member in VECTOR_INDEX_MEMBERS {
        if field.contains_key(member) && !declares_vector_index {
            return Err(settings_error(&format!("{path}.{member}")));
        }
    }

    for member in REFERENCE_FIELD_MEMBERS {
        if field.contains_key(member) && !field.contains_key("reference") {
            return Err(settings_error(&format!("{path}.{member}")));
        }
    }

    Ok(())
}

fn accepted_field_member_validator(member: &str) -> Option<FieldMemberValidator> {
    ACCEPTED_FIELD_MEMBERS
        .iter()
        .find_map(|(known_member, validator)| (*known_member == member).then_some(*validator))
}

fn require_field_member(
    field: &Map<String, Value>,
    member: &str,
    path: &str,
) -> Result<(), TypesenseSettingsError> {
    validate_shape(field.contains_key(member), &format!("{path}.{member}"))
}

fn validate_non_empty_string(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(value.as_str().is_some_and(|value| !value.is_empty()), path)
}

fn validate_string(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(value.is_string(), path)
}

fn validate_field_type(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(
        value
            .as_str()
            .is_some_and(|field_type| TYPESENSE_FIELD_TYPES.contains(&field_type)),
        path,
    )
}

fn validate_bool(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(value.is_boolean(), path)
}

/// Typesense 30.2 preserves its empty default, the `de_en` special case, and any
/// two-byte ASCII locale token in collection responses.
fn validate_locale(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(
        value.as_str().is_some_and(|locale| {
            locale.is_empty() || locale == "de_en" || (locale.len() == 2 && locale.is_ascii())
        }),
        path,
    )
}

fn validate_nonzero_u64(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(value.as_u64().is_some_and(|value| value > 0), path)
}

fn validate_vector_distance(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(
        value
            .as_str()
            .is_some_and(|distance| matches!(distance, "cosine" | "ip")),
        path,
    )
}

fn validate_single_character_array(
    value: &Value,
    path: &str,
) -> Result<(), TypesenseSettingsError> {
    validate_shape(
        value.as_array().is_some_and(|items| {
            items.iter().all(|item| {
                item.as_str()
                    .is_some_and(|character| character.chars().count() == 1)
            })
        }),
        path,
    )
}

fn validate_u32(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(
        value
            .as_u64()
            .is_some_and(|value| u32::try_from(value).is_ok()),
        path,
    )
}

fn validate_embed(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    let object = validate_exact_object(value, path, &EMBED_MEMBERS, &["from", "model_config"])?;
    if let Some(mapping) = object.get("mapping") {
        validate_embed_mapping(mapping, object, path)?;
    }
    Ok(())
}

fn validate_hnsw_params(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_exact_object(value, path, &HNSW_MEMBERS, &["M", "ef_construction"])?;
    Ok(())
}

fn validate_model_config(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    let config = value.as_object().ok_or_else(|| settings_error(path))?;
    for (member, value) in config {
        let member_path = format!("{path}.{member}");
        if MODEL_CONFIG_STRING_MEMBERS.contains(&member.as_str()) {
            validate_non_empty_string(value, &member_path)?;
        } else if member == "personalization_type" {
            validate_personalization_type(value, &member_path)?;
        } else if member == "service_account" {
            validate_exact_object(
                value,
                &member_path,
                &SERVICE_ACCOUNT_MEMBERS,
                &["client_email", "private_key"],
            )?;
        } else {
            return Err(settings_error(&member_path));
        }
    }
    require_field_member(config, "model_name", path)
}

fn validate_exact_object<'a>(
    value: &'a Value,
    path: &str,
    accepted_members: &[(&str, FieldMemberValidator)],
    required_members: &[&str],
) -> Result<&'a Map<String, Value>, TypesenseSettingsError> {
    let object = value.as_object().ok_or_else(|| settings_error(path))?;
    for (member, value) in object {
        let member_path = format!("{path}.{member}");
        let Some((_, validator)) = accepted_members
            .iter()
            .find(|(known_member, _)| *known_member == member)
        else {
            return Err(settings_error(&member_path));
        };
        validator(value, &member_path)?;
    }
    for member in required_members {
        require_field_member(object, member, path)?;
    }
    Ok(object)
}

fn validate_non_empty_string_array(
    value: &Value,
    path: &str,
) -> Result<(), TypesenseSettingsError> {
    validate_shape(
        value.as_array().is_some_and(|items| {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.as_str().is_some_and(|value| !value.is_empty()))
        }),
        path,
    )
}

fn validate_embed_mapping(
    value: &Value,
    embed: &Map<String, Value>,
    path: &str,
) -> Result<(), TypesenseSettingsError> {
    validate_non_empty_string_array(value, &format!("{path}.mapping"))?;
    let mapping_len = value.as_array().map_or(0, Vec::len);
    let from_len = embed
        .get("from")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    validate_shape(mapping_len == from_len, &format!("{path}.mapping"))
}

fn validate_personalization_type(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(
        value
            .as_str()
            .is_some_and(|value| MODEL_CONFIG_PERSONALIZATION_TYPES.contains(&value)),
        path,
    )
}

fn validate_nested_array(value: &Value, path: &str) -> Result<(), TypesenseSettingsError> {
    validate_shape(value.as_u64().is_some_and(|value| value <= 2), path)
}

pub(super) fn validate_shape(valid: bool, path: &str) -> Result<(), TypesenseSettingsError> {
    valid.then_some(()).ok_or_else(|| settings_error(path))
}

pub(super) fn settings_error(path: &str) -> TypesenseSettingsError {
    TypesenseSettingsError {
        json_path: path.to_string(),
    }
}
