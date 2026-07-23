//! Convert documents between JSON, internal `Document`, and Tantivy representations, handling field splitting for search vs. filter indexes and geo-location extraction.
use crate::error::{FlapjackError, Result};
use crate::index::facet_translation::{extract_facet_paths, is_hierarchical_facet};
use crate::index::schema::Schema;
use crate::index::settings::IndexSettings;
#[cfg(feature = "decompound")]
use crate::query::decompound::decompound_for_lang;
use crate::text_normalization::{is_camel_case_attr_path, split_camel_case_words};
use crate::types::{Document, DocumentId, FieldValue};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use tantivy::schema::{Field, OwnedValue};
use tantivy::TantivyDocument;

/// Convert raw JSON into a `TantivyDocument` using explicit field handles.
///
/// Extracts `_id` or `objectID` as the document identifier, splits remaining fields into search and filter JSON objects, copies the search object into the exact-match field, and indexes string and hierarchical facet values.
///
/// # Arguments
///
/// * `json` - A JSON object containing the document. Must have an `_id` or `objectID` string field.
/// * `id_field` .. `facets_field` - Pre-resolved Tantivy field handles.
///
/// # Errors
///
/// Returns `FlapjackError::InvalidDocument` if the input is not a JSON object, or `FlapjackError::MissingField` if neither `_id` nor `objectID` is present.
pub fn json_to_tantivy_doc(
    json: &Value,
    id_field: Field,
    json_search_field: Field,
    json_filter_field: Field,
    json_exact_field: Field,
    facets_field: Field,
) -> Result<TantivyDocument> {
    let mut tantivy_doc = TantivyDocument::new();

    let obj = json
        .as_object()
        .ok_or_else(|| FlapjackError::InvalidDocument("Expected JSON object".to_string()))?;

    // Accept both "_id" (internal) and "objectID" (Algolia-compatible, user-facing)
    let id = obj
        .get("_id")
        .or_else(|| obj.get("objectID"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| FlapjackError::MissingField("objectID".to_string()))?;

    tantivy_doc.add_text(id_field, id);

    let mut json_fields = Map::new();
    for (key, val) in obj {
        if key == "_id" || key == "objectID" {
            continue;
        }
        json_fields.insert(key.clone(), val.clone());
    }

    let json_value = Value::Object(json_fields.clone());
    let (search_json, mut filter_json) = split_by_type(&json_value);
    if let Value::Object(ref mut filter_map) = filter_json {
        filter_map.insert("objectID".to_string(), Value::String(id.to_string()));
    }

    tantivy_doc.add_object(json_search_field, json_to_btree(&search_json)?);
    tantivy_doc.add_object(json_filter_field, json_to_btree(&filter_json)?);
    tantivy_doc.add_object(json_exact_field, json_to_btree(&search_json)?);

    for (field_name, value) in &json_fields {
        let paths = if is_hierarchical_facet(value) {
            extract_facet_paths(field_name, value)?
        } else if let Value::String(s) = value {
            vec![format!("/{}/{}", field_name, s)]
        } else {
            vec![]
        };

        for path in paths {
            tantivy_doc.add_facet(facets_field, tantivy::schema::Facet::from(&path));
        }
    }

    Ok(tantivy_doc)
}

pub struct DocumentConverter {
    id_field: Field,
    json_search_field: Field,
    json_filter_field: Field,
    json_exact_field: Field,
    facets_field: Field,
    geo_lat_field: Option<Field>,
    geo_lng_field: Option<Field>,
}

impl DocumentConverter {
    /// Create a converter by resolving the required Tantivy field handles from the schema.
    ///
    /// Geo-location fields (`_geo_lat`, `_geo_lng`) are optional; all other fields must be present.
    ///
    /// # Errors
    ///
    /// Returns `FlapjackError::FieldNotFound` if any required field (`_id`, `_json_search`, `_json_filter`, `_json_exact`, `_facets`) is missing from `tantivy_schema`.
    pub fn new(_schema: &Schema, tantivy_schema: &tantivy::schema::Schema) -> Result<Self> {
        let id_field = tantivy_schema
            .get_field("_id")
            .map_err(|_| FlapjackError::FieldNotFound("_id".to_string()))?;
        let json_search_field = tantivy_schema
            .get_field("_json_search")
            .map_err(|_| FlapjackError::FieldNotFound("_json_search".to_string()))?;
        let json_filter_field = tantivy_schema
            .get_field("_json_filter")
            .map_err(|_| FlapjackError::FieldNotFound("_json_filter".to_string()))?;
        let json_exact_field = tantivy_schema
            .get_field("_json_exact")
            .map_err(|_| FlapjackError::FieldNotFound("_json_exact".to_string()))?;
        let facets_field = tantivy_schema
            .get_field("_facets")
            .map_err(|_| FlapjackError::FieldNotFound("_facets".to_string()))?;
        let geo_lat_field = tantivy_schema.get_field("_geo_lat").ok();
        let geo_lng_field = tantivy_schema.get_field("_geo_lng").ok();

        Ok(DocumentConverter {
            id_field,
            json_search_field,
            json_filter_field,
            json_exact_field,
            facets_field,
            geo_lat_field,
            geo_lng_field,
        })
    }

    /// Convert an internal `Document` into a `TantivyDocument` ready for indexing.
    ///
    /// Extracts `_geoloc` into dedicated latitude/longitude fields, applies camel-case splitting and decompound expansion to the search copy, splits fields into search and filter JSON objects, injects `objectID` into the filter object, and indexes facet paths for all fields in the settings' facet set.
    ///
    /// # Arguments
    ///
    /// * `doc` - The source document.
    /// * `settings` - Optional index settings controlling text transformations and facet configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if JSON-to-BTreeMap conversion fails.
    pub fn to_tantivy(
        &self,
        doc: &Document,
        settings: Option<&IndexSettings>,
    ) -> Result<TantivyDocument> {
        let mut tantivy_doc = TantivyDocument::new();

        tantivy_doc.add_text(self.id_field, &doc.id);

        let mut json_fields = fields_to_json(&doc.fields);

        if let Value::Object(ref mut map) = json_fields {
            if let Some(geoloc) = map.remove("_geoloc") {
                if let Some((lat, lng)) = extract_geoloc(&geoloc) {
                    if let Some(f) = self.geo_lat_field {
                        tantivy_doc.add_f64(f, lat);
                    }
                    if let Some(f) = self.geo_lng_field {
                        tantivy_doc.add_f64(f, lng);
                    }
                }
                if let Value::Object(ref mut filter_map) = json_fields {
                    filter_map.insert("_geoloc".to_string(), geoloc.clone());
                }
            }
        }

        let mut search_json = json_fields.clone();
        if let Some(settings) = settings {
            apply_camel_case_splitting(&mut search_json, "", &settings.camel_case_attributes);

            #[cfg(feature = "decompound")]
            if let Some(decompounded_attributes) = &settings.decompounded_attributes {
                if !decompounded_attributes.is_empty() {
                    apply_decompound_expansion(&mut search_json, "", decompounded_attributes);
                }
            }
        }

        let (search_json, _) = split_by_type(&search_json);
        let (_, mut filter_json) = split_by_type(&json_fields);
        if let Value::Object(ref mut filter_map) = filter_json {
            filter_map.insert("objectID".to_string(), Value::String(doc.id.clone()));
        }

        tantivy_doc.add_object(self.json_search_field, json_to_btree(&search_json)?);
        tantivy_doc.add_object(self.json_filter_field, json_to_btree(&filter_json)?);
        tantivy_doc.add_object(self.json_exact_field, json_to_btree(&search_json)?);

        let facet_fields: std::collections::HashSet<String> =
            settings.map(|s| s.facet_set()).unwrap_or_default();

        for (field_name, value) in json_fields.as_object().unwrap() {
            let dominated = facet_fields.contains(field_name)
                || facet_fields
                    .iter()
                    .any(|f| f.starts_with(&format!("{}.", field_name)));
            if !dominated {
                continue;
            }

            let paths = if is_hierarchical_facet(value) {
                extract_facet_paths(field_name, value)?
            } else if let Value::String(s) = value {
                let truncated = if s.len() > 1000 {
                    &s[..1000]
                } else {
                    s.as_str()
                };
                vec![format!("/{}/{}", field_name, truncated)]
            } else if let Value::Number(n) = value {
                vec![format!("/{}/{}", field_name, n)]
            } else if let Value::Bool(b) = value {
                vec![format!("/{}/{}", field_name, b)]
            } else if let Value::Array(arr) = value {
                arr.iter()
                    .filter_map(|item| match item {
                        Value::String(s) => {
                            let truncated = if s.len() > 1000 {
                                &s[..1000]
                            } else {
                                s.as_str()
                            };
                            Some(format!("/{}/{}", field_name, truncated))
                        }
                        Value::Number(n) => Some(format!("/{}/{}", field_name, n)),
                        Value::Bool(b) => Some(format!("/{}/{}", field_name, b)),
                        _ => None,
                    })
                    .collect()
            } else {
                vec![]
            };

            for path in &paths {
                tantivy_doc.add_facet(self.facets_field, tantivy::schema::Facet::from(path));
            }
        }

        Ok(tantivy_doc)
    }

    /// Reconstruct an internal `Document` from a `TantivyDocument`.
    ///
    /// Reads the document ID from the `_id` field and rebuilds the field map from the `_json_filter` object. The `_ignored_doc_id` parameter is unused.
    ///
    /// # Errors
    ///
    /// Returns an error if the `_id` or `_json_filter` fields are missing or malformed.
    pub fn from_tantivy(
        &self,
        tantivy_doc: TantivyDocument,
        _tantivy_schema: &tantivy::schema::Schema,
        _ignored_doc_id: DocumentId,
    ) -> Result<Document> {
        let doc_id = tantivy_doc
            .get_first(self.id_field)
            .and_then(|v| {
                let owned: tantivy::schema::OwnedValue = v.into();
                match owned {
                    tantivy::schema::OwnedValue::Str(s) => Some(s),
                    _ => None,
                }
            })
            .ok_or_else(|| FlapjackError::MissingField("_id".to_string()))?;

        let json_value = tantivy_doc
            .get_first(self.json_filter_field)
            .ok_or_else(|| FlapjackError::MissingField("_json_filter".to_string()))?;

        let owned: OwnedValue = json_value.into();
        let fields = owned_value_to_fields(&owned)?;

        Ok(Document { id: doc_id, fields })
    }
}

/// Split camelCase tokens in-place within string fields that match the configured attribute paths.
///
/// Recurses into objects and arrays. Only transforms `Value::String` nodes whose accumulated dot-separated path appears in `camel_case_attributes`.
fn apply_camel_case_splitting(value: &mut Value, path: &str, camel_case_attributes: &[String]) {
    if camel_case_attributes.is_empty() {
        return;
    }
    match value {
        Value::String(text) if is_camel_case_attr_path(path, camel_case_attributes) => {
            *text = split_camel_case_words(text);
        }
        Value::Object(map) => {
            for (field_name, child) in map.iter_mut() {
                let child_path = if path.is_empty() {
                    field_name.clone()
                } else {
                    format!("{}.{}", path, field_name)
                };
                apply_camel_case_splitting(child, &child_path, camel_case_attributes);
            }
        }
        Value::Array(items) => {
            for item in items {
                apply_camel_case_splitting(item, path, camel_case_attributes);
            }
        }
        _ => {}
    }
}

/// Expand compound words in-place by appending decompounded tokens to matching string fields.
///
/// Recurses into objects and arrays. For each string field whose attribute path matches an entry in `decompounded_attributes`, every whitespace-delimited token is decompounded for each configured language and the resulting parts are appended to the original text separated by spaces.
#[cfg(feature = "decompound")]
fn apply_decompound_expansion(
    value: &mut Value,
    path: &str,
    decompounded_attributes: &std::collections::HashMap<String, Vec<String>>,
) {
    if decompounded_attributes.is_empty() {
        return;
    }

    match value {
        Value::String(text) => {
            let languages = is_decompound_attr_path(path, decompounded_attributes);
            if languages.is_empty() {
                return;
            }

            let mut extras = Vec::<String>::new();
            for token in text.split_whitespace() {
                let lower = token.to_lowercase();
                let stripped: String = lower
                    .chars()
                    .skip_while(|c| !c.is_alphanumeric())
                    .collect::<String>()
                    .trim_end_matches(|c: char| !c.is_alphanumeric())
                    .to_string();
                if stripped.is_empty() {
                    continue;
                }
                for lang in &languages {
                    if let Some(parts) = decompound_for_lang(&stripped, lang) {
                        for part in parts {
                            if !extras.iter().any(|p| p == &part) {
                                extras.push(part);
                            }
                        }
                    }
                }
            }

            if !extras.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&extras.join(" "));
            }
        }
        Value::Object(map) => {
            for (field_name, child) in map.iter_mut() {
                let child_path = if path.is_empty() {
                    field_name.clone()
                } else {
                    format!("{}.{}", path, field_name)
                };
                apply_decompound_expansion(child, &child_path, decompounded_attributes);
            }
        }
        Value::Array(items) => {
            for item in items {
                apply_decompound_expansion(item, path, decompounded_attributes);
            }
        }
        _ => {}
    }
}

/// Return the list of language codes for which a given attribute path should be decompounded.
///
/// Matches exact paths and dot-prefixed children (e.g., path `"desc.body"` matches attribute `"desc"`).
#[cfg(feature = "decompound")]
fn is_decompound_attr_path(
    path: &str,
    decompounded_attributes: &std::collections::HashMap<String, Vec<String>>,
) -> Vec<String> {
    decompounded_attributes
        .iter()
        .filter_map(|(lang, attributes)| {
            if attributes.iter().any(|attr| {
                path == attr
                    || path
                        .strip_prefix(attr.as_str())
                        .is_some_and(|suffix| suffix.starts_with('.'))
            }) {
                Some(lang.clone())
            } else {
                None
            }
        })
        .collect()
}

/// Convert a Tantivy `OwnedValue::Object` into a flat field map.
///
/// Null values inside the object are silently skipped.
///
/// # Returns
///
/// A `HashMap<String, FieldValue>` of the object's entries.
///
/// # Errors
///
/// Returns `FlapjackError::InvalidDocument` if `value` is not an `OwnedValue::Object`.
fn owned_value_to_fields(
    value: &OwnedValue,
) -> Result<std::collections::HashMap<String, FieldValue>> {
    match value {
        OwnedValue::Object(pairs) => {
            let mut fields = std::collections::HashMap::new();
            for (key, val) in pairs {
                if let Some(fv) = owned_to_field_value(val) {
                    fields.insert(key.clone(), fv);
                }
            }
            Ok(fields)
        }
        _ => Err(FlapjackError::InvalidDocument(
            "Expected object".to_string(),
        )),
    }
}

/// Convert a single Tantivy `OwnedValue` into a `FieldValue`.
///
/// Returns `None` for `Null` and unrecognized variants. `U64` values are cast to `i64`.
fn owned_to_field_value(value: &OwnedValue) -> Option<FieldValue> {
    match value {
        OwnedValue::Null => None,
        OwnedValue::Str(s) => Some(FieldValue::Text(s.clone())),
        OwnedValue::I64(i) => Some(FieldValue::Integer(*i)),
        OwnedValue::U64(u) => Some(FieldValue::Integer(*u as i64)),
        OwnedValue::F64(f) => Some(FieldValue::Float(*f)),
        OwnedValue::Bool(b) => Some(FieldValue::Bool(*b)),
        OwnedValue::Array(arr) => {
            let items: Vec<FieldValue> = arr.iter().filter_map(owned_to_field_value).collect();
            if items.is_empty() {
                None
            } else {
                Some(FieldValue::Array(items))
            }
        }
        OwnedValue::Object(pairs) => {
            let mut map = std::collections::HashMap::new();
            for (k, v) in pairs {
                if let Some(fv) = owned_to_field_value(v) {
                    map.insert(k.clone(), fv);
                }
            }
            if map.is_empty() {
                None
            } else {
                Some(FieldValue::Object(map))
            }
        }
        _ => None,
    }
}

fn fields_to_json(fields: &std::collections::HashMap<String, FieldValue>) -> Value {
    let mut map = Map::new();
    for (key, value) in fields {
        let json_value = crate::types::field_value_to_json_value(value);
        map.insert(key.clone(), json_value);
    }
    Value::Object(map)
}

fn json_to_btree(value: &Value) -> Result<BTreeMap<String, OwnedValue>> {
    match value {
        Value::Object(map) => {
            let mut btree = BTreeMap::new();
            for (k, v) in map {
                btree.insert(k.clone(), json_value_to_owned(v)?);
            }
            Ok(btree)
        }
        _ => Err(FlapjackError::InvalidDocument(
            "Expected JSON object".to_string(),
        )),
    }
}

/// Convert a `serde_json::Value` into a Tantivy `OwnedValue`.
///
/// Numbers are stored as `I64` when they fit, falling back to `U64`, then `F64`.
///
/// # Errors
///
/// Returns `FlapjackError::InvalidDocument` if a JSON number cannot be represented in any numeric type.
fn json_value_to_owned(value: &Value) -> Result<OwnedValue> {
    match value {
        Value::Null => Ok(OwnedValue::Null),
        Value::Bool(b) => Ok(OwnedValue::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(OwnedValue::I64(i))
            } else if let Some(u) = n.as_u64() {
                Ok(OwnedValue::U64(u))
            } else if let Some(f) = n.as_f64() {
                Ok(OwnedValue::F64(f))
            } else {
                Err(FlapjackError::InvalidDocument("Invalid number".to_string()))
            }
        }
        Value::String(s) => Ok(OwnedValue::Str(s.clone())),
        Value::Array(arr) => {
            let owned_arr: Result<Vec<OwnedValue>> = arr.iter().map(json_value_to_owned).collect();
            Ok(OwnedValue::Array(owned_arr?))
        }
        Value::Object(map) => {
            let mut pairs = Vec::new();
            for (k, v) in map {
                pairs.push((k.clone(), json_value_to_owned(v)?));
            }
            Ok(OwnedValue::Object(pairs))
        }
    }
}

/// Extract a latitude/longitude pair from a `_geoloc` JSON value.
///
/// Accepts an object with `lat` and `lng` numeric fields, or an array whose first element is such an object. Returns `None` if the value is missing, malformed, or outside the valid coordinate range (lat \[-90, 90\], lng \[-180, 180\]).
fn extract_geoloc(value: &Value) -> Option<(f64, f64)> {
    match value {
        Value::Object(map) => {
            let lat = map.get("lat").and_then(|v| v.as_f64())?;
            let lng = map.get("lng").and_then(|v| v.as_f64())?;
            if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng) {
                Some((lat, lng))
            } else {
                None
            }
        }
        Value::Array(arr) => {
            if let Some(first) = arr.first() {
                extract_geoloc(first)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Partition a JSON value into a search component and a filter component.
///
/// Strings appear in both. Numbers and booleans appear only in the filter component. Arrays of strings are joined with spaces for search while the original array is kept for filtering. Null values are dropped from both. Objects are recursed into, with null-valued keys omitted.
fn split_by_type(value: &Value) -> (Value, Value) {
    match value {
        Value::Object(map) => {
            let mut search = Map::new();
            let mut filter = Map::new();
            for (k, v) in map {
                if v.is_null() {
                    continue;
                }
                let (s, f) = split_by_type(v);
                if !s.is_null() {
                    search.insert(k.clone(), s);
                }
                filter.insert(k.clone(), f);
            }
            (Value::Object(search), Value::Object(filter))
        }
        Value::Array(arr) => {
            let strings: Vec<String> = arr
                .iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect();
            let search_val = if strings.is_empty() {
                Value::Null
            } else {
                Value::String(strings.join(" "))
            };
            (search_val, value.clone())
        }
        Value::String(_) => (value.clone(), value.clone()),
        Value::Number(_) | Value::Bool(_) => (Value::Null, value.clone()),
        Value::Null => (Value::Null, Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use tantivy::schema::OwnedValue;

    // ── owned_to_field_value ──────────────────────────────────────────────

    #[test]
    fn owned_null_returns_none() {
        assert!(owned_to_field_value(&OwnedValue::Null).is_none());
    }

    #[test]
    fn owned_str_to_text() {
        let v = owned_to_field_value(&OwnedValue::Str("hello".to_string()));
        assert_eq!(v, Some(FieldValue::Text("hello".to_string())));
    }

    #[test]
    fn owned_i64_to_integer() {
        let v = owned_to_field_value(&OwnedValue::I64(42));
        assert_eq!(v, Some(FieldValue::Integer(42)));
    }

    #[test]
    fn owned_u64_to_integer() {
        let v = owned_to_field_value(&OwnedValue::U64(100));
        assert_eq!(v, Some(FieldValue::Integer(100)));
    }

    #[test]
    fn owned_f64_to_float() {
        let v = owned_to_field_value(&OwnedValue::F64(2.5));
        assert_eq!(v, Some(FieldValue::Float(2.5)));
    }

    #[test]
    fn owned_bool_to_bool() {
        let v = owned_to_field_value(&OwnedValue::Bool(true));
        assert_eq!(v, Some(FieldValue::Bool(true)));
    }

    #[test]
    fn owned_array_of_strings() {
        let arr = OwnedValue::Array(vec![
            OwnedValue::Str("a".to_string()),
            OwnedValue::Str("b".to_string()),
        ]);
        let v = owned_to_field_value(&arr);
        assert_eq!(
            v,
            Some(FieldValue::Array(vec![
                FieldValue::Text("a".to_string()),
                FieldValue::Text("b".to_string()),
            ]))
        );
    }

    #[test]
    fn owned_empty_array_returns_none() {
        let arr = OwnedValue::Array(vec![]);
        assert!(owned_to_field_value(&arr).is_none());
    }

    #[test]
    fn owned_array_with_only_nulls_returns_none() {
        let arr = OwnedValue::Array(vec![OwnedValue::Null, OwnedValue::Null]);
        assert!(owned_to_field_value(&arr).is_none());
    }

    #[test]
    fn owned_object_to_field_value() {
        let obj = OwnedValue::Object(vec![
            ("x".to_string(), OwnedValue::I64(1)),
            ("y".to_string(), OwnedValue::I64(2)),
        ]);
        match owned_to_field_value(&obj) {
            Some(FieldValue::Object(map)) => {
                assert_eq!(map.get("x"), Some(&FieldValue::Integer(1)));
                assert_eq!(map.get("y"), Some(&FieldValue::Integer(2)));
            }
            other => panic!("expected Object, got {:?}", other),
        }
    }

    #[test]
    fn owned_empty_object_returns_none() {
        let obj = OwnedValue::Object(vec![]);
        assert!(owned_to_field_value(&obj).is_none());
    }

    // ── owned_value_to_fields ────────────────────────────────────────────

    #[test]
    fn owned_value_to_fields_basic() {
        let obj = OwnedValue::Object(vec![
            ("name".to_string(), OwnedValue::Str("Laptop".to_string())),
            ("price".to_string(), OwnedValue::I64(999)),
        ]);
        let fields = owned_value_to_fields(&obj).unwrap();
        assert_eq!(
            fields.get("name"),
            Some(&FieldValue::Text("Laptop".to_string()))
        );
        assert_eq!(fields.get("price"), Some(&FieldValue::Integer(999)));
    }

    #[test]
    fn owned_value_to_fields_rejects_non_object() {
        let v = OwnedValue::Str("not an object".to_string());
        assert!(owned_value_to_fields(&v).is_err());
    }

    #[test]
    fn owned_value_to_fields_skips_null() {
        let obj = OwnedValue::Object(vec![
            ("a".to_string(), OwnedValue::Str("ok".to_string())),
            ("b".to_string(), OwnedValue::Null),
        ]);
        let fields = owned_value_to_fields(&obj).unwrap();
        assert_eq!(fields.len(), 1);
        assert!(fields.contains_key("a"));
    }

    // ── fields_to_json roundtrip ─────────────────────────────────────────

    #[test]
    fn fields_to_json_roundtrip() {
        let mut fields = HashMap::new();
        fields.insert("name".to_string(), FieldValue::Text("Widget".to_string()));
        fields.insert("price".to_string(), FieldValue::Integer(10));
        let json_val = fields_to_json(&fields);
        let obj = json_val.as_object().unwrap();
        assert_eq!(obj["name"], json!("Widget"));
        assert_eq!(obj["price"], json!(10));
    }

    // ── json_value_to_owned ──────────────────────────────────────────────

    #[test]
    fn json_null_to_owned() {
        let v = json_value_to_owned(&Value::Null).unwrap();
        assert!(matches!(v, OwnedValue::Null));
    }

    #[test]
    fn json_bool_to_owned() {
        let v = json_value_to_owned(&json!(true)).unwrap();
        assert!(matches!(v, OwnedValue::Bool(true)));
    }

    #[test]
    fn json_int_to_owned() {
        let v = json_value_to_owned(&json!(42)).unwrap();
        assert!(matches!(v, OwnedValue::I64(42)));
    }

    #[test]
    fn json_float_to_owned() {
        let v = json_value_to_owned(&json!(2.5)).unwrap();
        match v {
            OwnedValue::F64(f) => assert!((f - 2.5).abs() < 1e-10),
            other => panic!("expected F64, got {:?}", other),
        }
    }

    #[test]
    fn json_string_to_owned() {
        let v = json_value_to_owned(&json!("hello")).unwrap();
        assert!(matches!(v, OwnedValue::Str(ref s) if s == "hello"));
    }

    #[test]
    fn json_array_to_owned() {
        let v = json_value_to_owned(&json!([1, "two"])).unwrap();
        match v {
            OwnedValue::Array(arr) => assert_eq!(arr.len(), 2),
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn json_object_to_owned() {
        let v = json_value_to_owned(&json!({"a": 1})).unwrap();
        match v {
            OwnedValue::Object(pairs) => {
                assert_eq!(pairs.len(), 1);
                assert_eq!(pairs[0].0, "a");
            }
            other => panic!("expected Object, got {:?}", other),
        }
    }

    // ── json_to_btree ────────────────────────────────────────────────────

    #[test]
    fn json_to_btree_basic() {
        let val = json!({"name": "Widget", "price": 10});
        let btree = json_to_btree(&val).unwrap();
        assert!(matches!(btree.get("name"), Some(OwnedValue::Str(s)) if s == "Widget"));
        assert!(matches!(btree.get("price"), Some(OwnedValue::I64(10))));
    }

    #[test]
    fn json_to_btree_rejects_non_object() {
        assert!(json_to_btree(&json!("string")).is_err());
        assert!(json_to_btree(&json!(42)).is_err());
        assert!(json_to_btree(&json!([1, 2])).is_err());
    }

    // ── extract_geoloc ───────────────────────────────────────────────────

    #[test]
    fn extract_geoloc_valid() {
        let v = json!({"lat": 48.8566, "lng": 2.3522});
        assert_eq!(extract_geoloc(&v), Some((48.8566, 2.3522)));
    }

    #[test]
    fn extract_geoloc_out_of_range_lat() {
        let v = json!({"lat": 91.0, "lng": 0.0});
        assert_eq!(extract_geoloc(&v), None);
    }

    #[test]
    fn extract_geoloc_out_of_range_lng() {
        let v = json!({"lat": 0.0, "lng": 181.0});
        assert_eq!(extract_geoloc(&v), None);
    }

    #[test]
    fn extract_geoloc_missing_lat() {
        let v = json!({"lng": 2.0});
        assert_eq!(extract_geoloc(&v), None);
    }

    #[test]
    fn extract_geoloc_missing_lng() {
        let v = json!({"lat": 48.0});
        assert_eq!(extract_geoloc(&v), None);
    }

    #[test]
    fn extract_geoloc_from_array() {
        let v = json!([{"lat": 48.8566, "lng": 2.3522}]);
        assert_eq!(extract_geoloc(&v), Some((48.8566, 2.3522)));
    }

    #[test]
    fn extract_geoloc_empty_array() {
        let v = json!([]);
        assert_eq!(extract_geoloc(&v), None);
    }

    #[test]
    fn extract_geoloc_string_returns_none() {
        let v = json!("not a geoloc");
        assert_eq!(extract_geoloc(&v), None);
    }

    #[test]
    fn extract_geoloc_boundary_values() {
        assert_eq!(
            extract_geoloc(&json!({"lat": 90.0, "lng": 180.0})),
            Some((90.0, 180.0))
        );
        assert_eq!(
            extract_geoloc(&json!({"lat": -90.0, "lng": -180.0})),
            Some((-90.0, -180.0))
        );
    }

    // ── split_by_type ────────────────────────────────────────────────────

    #[test]
    fn split_string_goes_to_both() {
        let (s, f) = split_by_type(&json!("hello"));
        assert_eq!(s, json!("hello"));
        assert_eq!(f, json!("hello"));
    }

    #[test]
    fn split_number_goes_to_filter_only() {
        let (s, f) = split_by_type(&json!(42));
        assert_eq!(s, Value::Null);
        assert_eq!(f, json!(42));
    }

    #[test]
    fn split_bool_goes_to_filter_only() {
        let (s, f) = split_by_type(&json!(true));
        assert_eq!(s, Value::Null);
        assert_eq!(f, json!(true));
    }

    #[test]
    fn split_null_gives_both_null() {
        let (s, f) = split_by_type(&Value::Null);
        assert_eq!(s, Value::Null);
        assert_eq!(f, Value::Null);
    }

    #[test]
    fn split_string_array_joins_for_search() {
        let v = json!(["red", "blue", "green"]);
        let (s, f) = split_by_type(&v);
        assert_eq!(s, json!("red blue green"));
        assert_eq!(f, json!(["red", "blue", "green"]));
    }

    #[test]
    fn split_numeric_array_no_search() {
        let v = json!([1, 2, 3]);
        let (s, _f) = split_by_type(&v);
        assert_eq!(s, Value::Null);
    }

    #[test]
    fn split_object_recurses() {
        let v = json!({"title": "Laptop", "price": 999});
        let (s, f) = split_by_type(&v);
        let search_obj = s.as_object().unwrap();
        let filter_obj = f.as_object().unwrap();
        // "title" is a string → in both search and filter
        assert_eq!(search_obj.get("title"), Some(&json!("Laptop")));
        assert_eq!(filter_obj.get("title"), Some(&json!("Laptop")));
        // "price" is a number → only in filter
        assert!(search_obj.get("price").is_none());
        assert_eq!(filter_obj.get("price"), Some(&json!(999)));
    }

    #[test]
    fn split_object_skips_null_fields() {
        let v = json!({"title": "Laptop", "removed": null});
        let (s, f) = split_by_type(&v);
        let search_obj = s.as_object().unwrap();
        let filter_obj = f.as_object().unwrap();
        assert!(search_obj.get("removed").is_none());
        assert!(filter_obj.get("removed").is_none());
    }
}
