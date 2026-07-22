use super::super::*;
use crate::index::settings::{parse_custom_ranking_token, CustomRankingDirection};

#[derive(Debug, Clone)]
pub(in crate::index::manager) struct CustomRankingSpec {
    pub field: String,
    pub asc: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::index::manager) enum RankingSortValue {
    Integer(i64),
    Float(f64),
    Text(String),
    Missing,
}

/// Parse custom ranking specifications from index settings into `(field_path,
/// direction)` pairs. Each spec is formatted as `asc(field)` or `desc(field)`.
pub(in crate::index::manager) fn parse_custom_ranking_specs(
    settings: Option<&IndexSettings>,
) -> Vec<CustomRankingSpec> {
    let mut specs = Vec::new();
    let Some(custom_ranking) = settings.and_then(|s| s.custom_ranking.as_ref()) else {
        return specs;
    };

    for spec in custom_ranking {
        if let Some(parsed) = parse_custom_ranking_token(spec) {
            specs.push(CustomRankingSpec {
                field: parsed.attribute,
                asc: parsed.direction == CustomRankingDirection::Asc,
            });
        }
    }

    specs
}

/// Compare two `SortValue` entries for ranking. Handles numeric, string, and
/// boolean types with null-last semantics and directional ordering (asc/desc).
pub(in crate::index::manager) fn compare_ranking_sort_value(
    a: &RankingSortValue,
    b: &RankingSortValue,
) -> Ordering {
    match (a, b) {
        (RankingSortValue::Missing, RankingSortValue::Missing) => Ordering::Equal,
        (RankingSortValue::Missing, _) => Ordering::Less,
        (_, RankingSortValue::Missing) => Ordering::Greater,
        (RankingSortValue::Integer(x), RankingSortValue::Integer(y)) => x.cmp(y),
        (RankingSortValue::Float(x), RankingSortValue::Float(y)) => {
            x.partial_cmp(y).unwrap_or(Ordering::Equal)
        }
        (RankingSortValue::Text(x), RankingSortValue::Text(y)) => x.cmp(y),
        (RankingSortValue::Integer(_), _) => Ordering::Less,
        (RankingSortValue::Float(_), RankingSortValue::Text(_)) => Ordering::Less,
        (RankingSortValue::Float(_), _) => Ordering::Greater,
        (RankingSortValue::Text(_), _) => Ordering::Greater,
    }
}

/// Compare two `FieldValue` entries for custom ranking. Coerces across types
/// (numeric comparison when both are numbers, string fallback), with null or
/// missing values sorting last.
pub(in crate::index::manager) fn compare_custom_values(
    a_values: &[RankingSortValue],
    b_values: &[RankingSortValue],
    specs: &[CustomRankingSpec],
) -> Ordering {
    for (idx, spec) in specs.iter().enumerate() {
        let a = a_values.get(idx).unwrap_or(&RankingSortValue::Missing);
        let b = b_values.get(idx).unwrap_or(&RankingSortValue::Missing);
        let cmp = match (a, b) {
            (RankingSortValue::Missing, RankingSortValue::Missing) => Ordering::Equal,
            (RankingSortValue::Missing, _) => Ordering::Greater,
            (_, RankingSortValue::Missing) => Ordering::Less,
            _ => {
                let value_cmp = compare_ranking_sort_value(a, b);
                if spec.asc {
                    value_cmp
                } else {
                    value_cmp.reverse()
                }
            }
        };
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    Ordering::Equal
}

/// Extract a `FieldValue` from a document's JSON by dotted field path. Traverses
/// nested objects and returns `None` if any path segment is missing.
pub(in crate::index::manager) fn extract_custom_ranking_value(
    document: &Document,
    field_path: &str,
) -> RankingSortValue {
    let parts: Vec<&str> = field_path.split('.').collect();
    if parts.is_empty() {
        return RankingSortValue::Missing;
    }
    let Some(mut current) = document.fields.get(parts[0]) else {
        return RankingSortValue::Missing;
    };
    for part in &parts[1..] {
        current = match current {
            FieldValue::Object(map) => {
                let Some(next) = map.get(*part) else {
                    return RankingSortValue::Missing;
                };
                next
            }
            _ => return RankingSortValue::Missing,
        };
    }
    match current {
        FieldValue::Integer(i) => RankingSortValue::Integer(*i),
        FieldValue::Date(i) => RankingSortValue::Integer(*i),
        FieldValue::Float(f) => RankingSortValue::Float(*f),
        FieldValue::Text(s) => match s.parse::<i64>() {
            Ok(i) => RankingSortValue::Integer(i),
            Err(_) => RankingSortValue::Text(s.clone()),
        },
        FieldValue::Facet(s) => RankingSortValue::Text(s.clone()),
        _ => RankingSortValue::Missing,
    }
}

/// Pre-computed context for stage-2 re-ranking. Holds parsed ranking criteria,
/// custom ranking specs, searchable attributes, typo settings, and optional
/// filter scores to avoid recomputation per comparison.
pub(in crate::index::manager) struct Stage2RankingContext<'a> {
    pub query_text: &'a str,
    pub searchable_paths: &'a [String],
    pub settings: Option<&'a IndexSettings>,
    pub synonym_store: Option<&'a SynonymStore>,
    pub plural_map: Option<&'a HashMap<String, Vec<String>>>,
    pub query_type: &'a str,
    pub optional_filter_groups: super::super::OptionalFilterSpecs<'a>,
    pub sum_or_filters_scores: bool,
    pub exact_on_single_word_query: &'a str,
    pub disable_exact_on_attributes: &'a [String],
    pub custom_normalization: &'a [(char, String)],
    pub keep_diacritics_on_characters: &'a str,
    pub camel_case_attributes: &'a [String],
    pub all_query_words_optional: bool,
    pub relevancy_strictness: Option<u32>,
    pub min_proximity: Option<u32>,
}

/// Collect the expected filter values for a dotted field path from the optional
/// filter specs. Returns the values and their associated boost scores.
pub(in crate::index::manager) fn resolve_optional_filter_values_for_path<'a>(
    document: &'a Document,
    field_path: &str,
) -> Vec<&'a FieldValue> {
    let parts: Vec<&str> = field_path
        .split('.')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return Vec::new();
    }
    let Some(root) = document.fields.get(parts[0]) else {
        return Vec::new();
    };

    let mut current_values = vec![root];
    for part in &parts[1..] {
        let mut next_values = Vec::new();
        for value in current_values {
            match value {
                FieldValue::Object(map) => {
                    if let Some(next) = map.get(*part) {
                        next_values.push(next);
                    }
                }
                FieldValue::Array(items) => {
                    for item in items {
                        if let FieldValue::Object(map) = item {
                            if let Some(next) = map.get(*part) {
                                next_values.push(next);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if next_values.is_empty() {
            return Vec::new();
        }
        current_values = next_values;
    }

    current_values
}

/// Test whether a document's field value matches an optional filter value, with
/// type coercion (string-to-number, bool-to-string) for cross-type comparison.
pub(in crate::index::manager) fn field_value_matches_optional_filter_value(
    value: &FieldValue,
    expected: &str,
) -> bool {
    match value {
        FieldValue::Text(s) | FieldValue::Facet(s) => s.eq_ignore_ascii_case(expected),
        FieldValue::Integer(i) | FieldValue::Date(i) => expected
            .parse::<i64>()
            .map(|parsed| parsed == *i)
            .unwrap_or(false),
        FieldValue::Float(f) => expected
            .parse::<f64>()
            .map(|parsed| (parsed - *f).abs() <= f64::EPSILON)
            .unwrap_or(false),
        FieldValue::Array(items) => items
            .iter()
            .any(|item| field_value_matches_optional_filter_value(item, expected)),
        FieldValue::Object(map) => map
            .values()
            .any(|nested| field_value_matches_optional_filter_value(nested, expected)),
    }
}

pub(in crate::index::manager) fn doc_matches_optional_filter_spec(
    document: &Document,
    field: &str,
    value: &str,
) -> bool {
    resolve_optional_filter_values_for_path(document, field)
        .into_iter()
        .any(|field_value| field_value_matches_optional_filter_value(field_value, value))
}

/// Sum the boost scores for all optional filter specs whose values match the
/// document's fields. Higher scores indicate stronger optional filter affinity.
pub(in crate::index::manager) fn compute_optional_filter_score(
    document: &Document,
    groups: &[Vec<(String, String, f32)>],
    sum_mode: bool,
) -> f32 {
    if groups.is_empty() {
        return 0.0;
    }

    if sum_mode {
        return groups
            .iter()
            .flat_map(|group| group.iter())
            .filter_map(|(field, value, score)| {
                doc_matches_optional_filter_spec(document, field, value).then_some(*score)
            })
            .sum();
    }

    groups
        .iter()
        .map(|group| {
            group
                .iter()
                .filter_map(|(field, value, score)| {
                    doc_matches_optional_filter_spec(document, field, value).then_some(*score)
                })
                .reduce(f32::max)
                .unwrap_or(0.0)
        })
        .sum()
}
