//! Algolia filter string parsing utilities for converting JSON specifications (facet, numeric, tag, optional) into Filter AST nodes with AND/OR composition.
use crate::types::{FieldValue, Filter};

/// Extract field and value from a "field:value" string with optional negation prefix `-`. Returns a Filter::Equals or Filter::Not wrapping it, or None if invalid.
pub fn parse_facet_filter_string(s: &str) -> Option<Filter> {
    let s = s.trim();
    let (negated, s) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else {
        (false, s)
    };
    let colon_pos = s.find(':')?;
    let field = s[..colon_pos].to_string();
    let value = s[colon_pos + 1..]
        .trim_matches('"')
        .trim_matches('\'')
        .to_string();
    let filter = Filter::Equals {
        field,
        value: FieldValue::Text(value),
    };
    if negated {
        Some(Filter::Not(Box::new(filter)))
    } else {
        Some(filter)
    }
}

/// Convert Algolia facet filter JSON into a Filter AST. Nested string arrays form OR groups; outer elements are AND'd together.
pub fn facet_filters_to_ast(value: &serde_json::Value) -> Option<Filter> {
    match value {
        serde_json::Value::Array(items) => {
            let mut and_parts: Vec<Filter> = Vec::new();
            for item in items {
                match item {
                    serde_json::Value::Array(or_items) => {
                        let or_filters: Vec<Filter> = or_items
                            .iter()
                            .filter_map(|v| v.as_str().and_then(parse_facet_filter_string))
                            .collect();
                        match or_filters.len() {
                            0 => {}
                            1 => and_parts.push(or_filters.into_iter().next().unwrap()),
                            _ => and_parts.push(Filter::Or(or_filters)),
                        }
                    }
                    serde_json::Value::String(s) => {
                        if let Some(f) = parse_facet_filter_string(s) {
                            and_parts.push(f);
                        }
                    }
                    _ => {}
                }
            }
            match and_parts.len() {
                0 => None,
                1 => Some(and_parts.remove(0)),
                _ => Some(Filter::And(and_parts)),
            }
        }
        serde_json::Value::String(s) => parse_facet_filter_string(s),
        _ => None,
    }
}

/// Extract field and numeric value from a comparison string (e.g., "price>=10"). Supports >=, <=, !=, >, <, = operators with integer or float values.
pub fn parse_numeric_filter_string(s: &str) -> Option<Filter> {
    let s = s.trim();
    let ops = [">=", "<=", "!=", ">", "<", "="];
    for op in &ops {
        if let Some(pos) = s.find(op) {
            let field = s[..pos].trim().to_string();
            let val_str = s[pos + op.len()..].trim();
            let value = if let Ok(i) = val_str.parse::<i64>() {
                FieldValue::Integer(i)
            } else if let Ok(f) = val_str.parse::<f64>() {
                FieldValue::Float(f)
            } else {
                return None;
            };
            return Some(match *op {
                ">=" => Filter::GreaterThanOrEqual { field, value },
                "<=" => Filter::LessThanOrEqual { field, value },
                ">" => Filter::GreaterThan { field, value },
                "<" => Filter::LessThan { field, value },
                "!=" => Filter::NotEquals { field, value },
                "=" => Filter::Equals { field, value },
                _ => return None,
            });
        }
    }
    None
}

/// Convert Algolia numeric filter JSON into a Filter AST. Nested string arrays form OR groups; outer elements are AND'd together.
pub fn numeric_filters_to_ast(value: &serde_json::Value) -> Option<Filter> {
    match value {
        serde_json::Value::Array(items) => {
            let mut and_parts: Vec<Filter> = Vec::new();
            for item in items {
                match item {
                    serde_json::Value::Array(or_items) => {
                        let or_filters: Vec<Filter> = or_items
                            .iter()
                            .filter_map(|v| v.as_str().and_then(parse_numeric_filter_string))
                            .collect();
                        match or_filters.len() {
                            0 => {}
                            1 => and_parts.push(or_filters.into_iter().next().unwrap()),
                            _ => and_parts.push(Filter::Or(or_filters)),
                        }
                    }
                    serde_json::Value::String(s) => {
                        if let Some(f) = parse_numeric_filter_string(s) {
                            and_parts.push(f);
                        }
                    }
                    _ => {}
                }
            }
            match and_parts.len() {
                0 => None,
                1 => Some(and_parts.remove(0)),
                _ => Some(Filter::And(and_parts)),
            }
        }
        serde_json::Value::String(s) => parse_numeric_filter_string(s),
        _ => None,
    }
}

/// Convert Algolia tag filter JSON into Filter AST with equality filters on the "_tags" field. Nested arrays form OR groups; outer elements are AND'd.
pub fn tag_filters_to_ast(value: &serde_json::Value) -> Option<Filter> {
    match value {
        serde_json::Value::Array(items) => {
            let mut and_parts: Vec<Filter> = Vec::new();
            for item in items {
                match item {
                    serde_json::Value::Array(or_items) => {
                        let or_filters: Vec<Filter> = or_items
                            .iter()
                            .filter_map(|v| {
                                v.as_str().map(|s| Filter::Equals {
                                    field: "_tags".to_string(),
                                    value: FieldValue::Text(s.to_string()),
                                })
                            })
                            .collect();
                        match or_filters.len() {
                            0 => {}
                            1 => and_parts.push(or_filters.into_iter().next().unwrap()),
                            _ => and_parts.push(Filter::Or(or_filters)),
                        }
                    }
                    serde_json::Value::String(s) => {
                        and_parts.push(Filter::Equals {
                            field: "_tags".to_string(),
                            value: FieldValue::Text(s.to_string()),
                        });
                    }
                    _ => {}
                }
            }
            match and_parts.len() {
                0 => None,
                1 => Some(and_parts.remove(0)),
                _ => Some(Filter::And(and_parts)),
            }
        }
        serde_json::Value::String(s) => Some(Filter::Equals {
            field: "_tags".to_string(),
            value: FieldValue::Text(s.to_string()),
        }),
        _ => None,
    }
}

/// Extract field, value, and optional score from a specification like "brand:apple<score=2>". Negation prefix `-` negates the score; non-finite scores default to 1.0 or -1.0.
fn parse_optional_filter_one(s: &str) -> Option<(String, String, f32)> {
    let s = s.trim();
    let (s, score) = if let Some(idx) = s.find("<score=") {
        let rest = &s[idx + 7..];
        let end = rest.find('>').unwrap_or(rest.len());
        let sc: f32 = rest[..end]
            .trim()
            .parse()
            .ok()
            .filter(|v: &f32| v.is_finite())
            .unwrap_or(1.0);
        (&s[..idx], sc)
    } else {
        (s, 1.0)
    };
    let (s, negated) = match s.strip_prefix('-') {
        Some(rest) => (rest, true),
        None => (s, false),
    };
    let colon = s.find(':')?;
    let field = s[..colon].trim().to_string();
    let value = s[colon + 1..]
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string();
    if field.is_empty() || value.is_empty() {
        return None;
    }
    let score = if negated { -score } else { score };
    Some((field, value, score))
}

/// Parse Algolia `optionalFilters` JSON into grouped OR clauses.
/// Each inner vec is an OR-group; single-entry groups are AND-level clauses.
pub fn parse_optional_filters_grouped(
    value: &serde_json::Value,
) -> Vec<Vec<(String, String, f32)>> {
    let mut groups = Vec::new();
    match value {
        serde_json::Value::String(s) => {
            if let Some(spec) = parse_optional_filter_one(s) {
                groups.push(vec![spec]);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                match item {
                    serde_json::Value::String(s) => {
                        if let Some(spec) = parse_optional_filter_one(s) {
                            groups.push(vec![spec]);
                        }
                    }
                    serde_json::Value::Array(or_items) => {
                        let mut group = Vec::new();
                        for sub in or_items {
                            if let Some(s) = sub.as_str() {
                                if let Some(spec) = parse_optional_filter_one(s) {
                                    group.push(spec);
                                }
                            }
                        }
                        if !group.is_empty() {
                            groups.push(group);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    groups
}

/// Parse Algolia `optionalFilters` JSON into `(field, value, score)` tuples.
pub fn parse_optional_filters(value: &serde_json::Value) -> Vec<(String, String, f32)> {
    parse_optional_filters_grouped(value)
        .into_iter()
        .flat_map(|group| group.into_iter())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Test verifying that a single nested OR array in facet filters produces an Or filter, with single-entry AND groups collapsed.
    #[test]
    fn facet_filters_nested_or() {
        // Single inner OR-array: outer And collapses to the single Or directly.
        let f = facet_filters_to_ast(&json!([["brand:Apple", "brand:Samsung"]])).unwrap();
        match f {
            Filter::Or(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    Filter::Equals { field, value } => {
                        assert_eq!(field, "brand");
                        assert_eq!(*value, FieldValue::Text("Apple".to_string()));
                    }
                    _ => panic!("expected Equals for parts[0]"),
                }
                match &parts[1] {
                    Filter::Equals { field, value } => {
                        assert_eq!(field, "brand");
                        assert_eq!(*value, FieldValue::Text("Samsung".to_string()));
                    }
                    _ => panic!("expected Equals for parts[1]"),
                }
            }
            _ => panic!("expected Or filter from single inner-array OR group"),
        }
    }

    #[test]
    fn numeric_filters_single_string() {
        let f = numeric_filters_to_ast(&json!("price>=10")).unwrap();
        match f {
            Filter::GreaterThanOrEqual { field, .. } => assert_eq!(field, "price"),
            _ => panic!("expected greater-than-or-equal filter"),
        }
    }

    #[test]
    fn tag_filters_string() {
        let f = tag_filters_to_ast(&json!("featured")).unwrap();
        match f {
            Filter::Equals { field, value } => {
                assert_eq!(field, "_tags");
                assert_eq!(value, FieldValue::Text("featured".to_string()));
            }
            _ => panic!("expected equals filter"),
        }
    }

    #[test]
    fn optional_filters_score_suffix() {
        let specs = parse_optional_filters(&json!("brand:apple<score=2>"));
        assert_eq!(specs, vec![("brand".to_string(), "apple".to_string(), 2.0)]);
    }

    /// Test verifying that nested OR arrays in optional filter groups remain distinct from AND-level entries.
    #[test]
    fn optional_filters_grouped_preserves_nested_or() {
        let grouped = parse_optional_filters_grouped(&json!([
            ["brand:Apple<score=2>", "color:Red<score=2>"],
            "color:Green<score=3>"
        ]));
        assert_eq!(grouped.len(), 2);
        // OR group: both entries with correct field, value, and score
        assert_eq!(grouped[0].len(), 2);
        assert_eq!(
            grouped[0][0],
            ("brand".to_string(), "Apple".to_string(), 2.0)
        );
        assert_eq!(grouped[0][1], ("color".to_string(), "Red".to_string(), 2.0));
        // AND-level single entry
        assert_eq!(grouped[1].len(), 1);
        assert_eq!(
            grouped[1][0],
            ("color".to_string(), "Green".to_string(), 3.0)
        );
    }

    #[test]
    fn negative_optional_filter_negates_score() {
        let specs = parse_optional_filters(&json!("-brand:Nike"));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].0, "brand");
        assert_eq!(specs[0].1, "Nike");
        assert!(
            specs[0].2 < 0.0,
            "negative filter must produce negative score, got {}",
            specs[0].2
        );
    }

    #[test]
    fn negative_optional_filter_negates_explicit_score() {
        let specs = parse_optional_filters(&json!("-brand:Nike<score=5>"));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].0, "brand");
        assert_eq!(specs[0].1, "Nike");
        assert_eq!(
            specs[0].2, -5.0,
            "negative filter must negate explicit score"
        );
    }

    #[test]
    fn optional_filter_non_finite_score_defaults_to_one() {
        let specs = parse_optional_filters(&json!("brand:Nike<score=NaN>"));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0], ("brand".to_string(), "Nike".to_string(), 1.0));
    }

    #[test]
    fn negative_optional_filter_non_finite_score_defaults_to_negative_one() {
        let specs = parse_optional_filters(&json!("-brand:Nike<score=inf>"));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0], ("brand".to_string(), "Nike".to_string(), -1.0));
    }

    #[test]
    fn optional_filter_trims_field_and_value_whitespace() {
        let specs = parse_optional_filters(&json!(" brand : Nike "));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0], ("brand".to_string(), "Nike".to_string(), 1.0));
    }

    #[test]
    fn optional_filter_rejects_empty_field_or_value() {
        assert!(parse_optional_filters(&json!(":Nike")).is_empty());
        assert!(parse_optional_filters(&json!("brand:")).is_empty());
        assert!(parse_optional_filters(&json!("  :  ")).is_empty());
    }

    #[test]
    fn optional_filter_parses_whitespace_padded_score_suffix() {
        let specs = parse_optional_filters(&json!("-brand:Nike<score= 3 >"));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0], ("brand".to_string(), "Nike".to_string(), -3.0));
    }
}
