//! Tokenization utilities for extracting and normalizing searchable text from structured document fields.
use super::*;

/// Build Algolia-compatible `queryAfterRemoval` markup.
///
/// Kept words appear as plain text; removed words are wrapped in `<em>` tags.
/// Word order is preserved from the original query.
pub(super) fn build_query_after_removal_markup(
    words: &[&str],
    strategy: &str,
    drop_count: usize,
) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(words.len());
    for (i, word) in words.iter().enumerate() {
        let is_removed = match strategy {
            "lastWords" => i >= words.len() - drop_count,
            "firstWords" => i < drop_count,
            _ => false,
        };
        if is_removed {
            parts.push(format!("<em>{}</em>", word));
        } else {
            parts.push(word.to_string());
        }
    }
    parts.join(" ")
}

pub(super) fn push_unique_terms(
    target: &mut Vec<String>,
    values: impl IntoIterator<Item = String>,
) {
    for value in values {
        if !target.contains(&value) {
            target.push(value);
        }
    }
}

/// Split text into lowercased alphanumeric tokens after normalization.
///
/// Non-alphanumeric characters act as token boundaries and are discarded.
///
/// # Arguments
/// - `text`: The text to tokenize
/// - `keep_diacritics_on_characters`: Characters to preserve diacritics on during normalization
/// - `custom_normalization`: Character-to-string replacements for custom normalization
///
/// # Returns
/// A vector of lowercased alphanumeric tokens in order of appearance.
pub(super) fn tokenize_for_typo_bucket(
    text: &str,
    keep_diacritics_on_characters: &str,
    custom_normalization: &[(char, String)],
) -> Vec<String> {
    let text = normalize_for_search(text, keep_diacritics_on_characters, custom_normalization);
    let mut tokens = Vec::new();
    let mut current = String::new();

    for c in text.chars() {
        if c.is_alphanumeric() {
            for lowered in c.to_lowercase() {
                current.push(lowered);
            }
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

/// Recursively extract and normalize searchable tokens from a FieldValue.
///
/// For Text and Facet values, applies camelCase splitting if configured, then tokenizes and appends to output. For Arrays and Objects, recurses into each element. Other types yield no tokens.
///
/// # Arguments
/// - `value`: The FieldValue to extract tokens from
/// - `out`: Mutable vector to append collected tokens to
/// - `keep_diacritics_on_characters`: Characters to preserve diacritics on during normalization
/// - `custom_normalization`: Character-to-string replacements for custom normalization
/// - `camel_case_attributes`: Paths where camelCase splitting should be applied
/// - `path`: Current attribute path (for camelCase configuration lookup)
pub(super) fn collect_tokens_for_field_value(
    value: &FieldValue,
    out: &mut Vec<String>,
    keep_diacritics_on_characters: &str,
    custom_normalization: &[(char, String)],
    camel_case_attributes: &[String],
    path: &str,
) {
    match value {
        FieldValue::Text(s) | FieldValue::Facet(s) => {
            if is_camel_case_attr_path(path, camel_case_attributes) {
                out.extend(tokenize_for_typo_bucket(
                    &split_camel_case_words(s),
                    keep_diacritics_on_characters,
                    custom_normalization,
                ));
            } else {
                out.extend(tokenize_for_typo_bucket(
                    s,
                    keep_diacritics_on_characters,
                    custom_normalization,
                ));
            }
        }
        FieldValue::Array(items) => {
            for item in items {
                collect_tokens_for_field_value(
                    item,
                    out,
                    keep_diacritics_on_characters,
                    custom_normalization,
                    camel_case_attributes,
                    path,
                );
            }
        }
        FieldValue::Object(map) => {
            for nested in map.values() {
                collect_tokens_for_field_value(
                    nested,
                    out,
                    keep_diacritics_on_characters,
                    custom_normalization,
                    camel_case_attributes,
                    path,
                );
            }
        }
        _ => {}
    }
}

/// Recursively traverse a FieldValue along a dot-separated path to extract searchable tokens.
///
/// Follows each path component through nested Objects, processes all elements in Arrays, and delegates to collect_tokens_for_field_value when the path is fully traversed.
///
/// # Arguments
/// - `value`: The FieldValue to traverse
/// - `remaining_path`: Path components yet to follow (slices deeper with each recursion)
/// - `out`: Mutable vector to append collected tokens to
/// - `keep_diacritics_on_characters`: Characters to preserve diacritics on during normalization
/// - `custom_normalization`: Character-to-string replacements for custom normalization
/// - `camel_case_attributes`: Paths where camelCase splitting should be applied
/// - `path`: Full path traversed so far (for camelCase configuration lookup)
pub(super) fn collect_tokens_for_path_value(
    value: &FieldValue,
    remaining_path: &[&str],
    out: &mut Vec<String>,
    keep_diacritics_on_characters: &str,
    custom_normalization: &[(char, String)],
    camel_case_attributes: &[String],
    path: &str,
) {
    if remaining_path.is_empty() {
        collect_tokens_for_field_value(
            value,
            out,
            keep_diacritics_on_characters,
            custom_normalization,
            camel_case_attributes,
            path,
        );
        return;
    }

    match value {
        FieldValue::Object(map) => {
            if let Some(next) = map.get(remaining_path[0]) {
                let next_path = if path.is_empty() {
                    remaining_path[0].to_string()
                } else {
                    format!("{}.{}", path, remaining_path[0])
                };
                collect_tokens_for_path_value(
                    next,
                    &remaining_path[1..],
                    out,
                    keep_diacritics_on_characters,
                    custom_normalization,
                    camel_case_attributes,
                    &next_path,
                );
            }
        }
        FieldValue::Array(items) => {
            for item in items {
                collect_tokens_for_path_value(
                    item,
                    remaining_path,
                    out,
                    keep_diacritics_on_characters,
                    custom_normalization,
                    camel_case_attributes,
                    path,
                );
            }
        }
        _ => {}
    }
}

/// Extract searchable tokens from a document for each configured path.
///
/// Iterates over searchable paths, navigates each path in the document structure, and collects normalized tokens. Returns index-token pairs only for paths that contain tokens.
///
/// # Arguments
/// - `document`: The document to extract tokens from
/// - `searchable_paths`: Dot-separated field paths to extract (e.g., "author.name", "tags")
/// - `keep_diacritics_on_characters`: Characters to preserve diacritics on during normalization
/// - `custom_normalization`: Character-to-string replacements for custom normalization
/// - `camel_case_attributes`: Paths where camelCase splitting should be applied
///
/// # Returns
/// Vector of (path_index, token_vector) tuples for each searchable path containing tokens.
pub(crate) fn collect_doc_tokens_by_path(
    document: &Document,
    searchable_paths: &[String],
    keep_diacritics_on_characters: &str,
    custom_normalization: &[(char, String)],
    camel_case_attributes: &[String],
) -> Vec<(usize, Vec<String>)> {
    let mut result = Vec::new();

    for (idx, path) in searchable_paths.iter().enumerate() {
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            continue;
        }
        let mut path_tokens = Vec::new();
        if let Some(root) = document.fields.get(parts[0]) {
            collect_tokens_for_path_value(
                root,
                &parts[1..],
                &mut path_tokens,
                keep_diacritics_on_characters,
                custom_normalization,
                camel_case_attributes,
                parts[0],
            );
        }
        if !path_tokens.is_empty() {
            result.push((idx, path_tokens));
        }
    }

    result
}
