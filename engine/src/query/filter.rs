use crate::error::Result;
use crate::index::settings::IndexSettings;
use crate::types::Filter;
use std::collections::HashSet;
use tantivy::query::{AllQuery, BooleanQuery, Occur, Query};
use tantivy::schema::Schema;

pub struct FilterCompiler {
    query_parser: tantivy::query::QueryParser,
}

impl FilterCompiler {
    pub fn new(schema: Schema) -> Self {
        let json_filter_field = schema
            .get_field("_json_filter")
            .expect("_json_filter field must exist in schema");

        let query_parser = tantivy::query::QueryParser::new(
            schema,
            vec![json_filter_field],
            tantivy::tokenizer::TokenizerManager::default(),
        );

        FilterCompiler { query_parser }
    }

    const MAX_FILTER_DEPTH: usize = 10;
    const MAX_BOOLEAN_CLAUSES: usize = 1000;

    /// Allow only identifier-like JSON filter paths so user-controlled field names
    /// cannot inject Tantivy query syntax into `_json_filter.<field>:...`.
    fn json_filter_field_path(&self, field: &str) -> Result<String> {
        if field.is_empty()
            || !field
                .split('.')
                .all(|segment| !segment.is_empty() && Self::is_safe_field_segment(segment))
        {
            return Err(crate::error::FlapjackError::InvalidQuery(format!(
                "Invalid filter field '{}'",
                field
            )));
        }

        Ok(format!("_json_filter.{}", field))
    }

    fn is_safe_field_segment(segment: &str) -> bool {
        segment
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }

    fn escape_quoted_query_value(value: &str) -> String {
        value.replace('\\', "\\\\").replace('"', "\\\"")
    }

    fn is_safe_bare_text_char(ch: char) -> bool {
        ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')
    }

    /// Compile a `Filter` tree into a Tantivy query, enforcing clause-count and
    /// nesting-depth limits. Falls back to facet-only validation when available.
    pub fn compile(
        &self,
        filter: &Filter,
        settings: Option<&IndexSettings>,
    ) -> Result<Box<dyn Query>> {
        let clause_count = self.count_clauses(filter);
        if clause_count > Self::MAX_BOOLEAN_CLAUSES {
            return Err(crate::error::FlapjackError::InvalidQuery(format!(
                "Filter has {} clauses, exceeds maximum {}",
                clause_count,
                Self::MAX_BOOLEAN_CLAUSES
            )));
        }

        let facet_set = settings.map(|s| s.facet_set()).unwrap_or_default();

        if !self.is_valid_for_facet_set(filter, &facet_set) {
            return Ok(Box::new(tantivy::query::EmptyQuery));
        }

        if self.has_not(filter) {
            self.compile_with_hybrid(filter, 0)
        } else {
            let query_string = self.to_query_string(filter)?;
            self.query_parser
                .parse_query(&query_string)
                .map_err(|e| crate::error::FlapjackError::InvalidQuery(e.to_string()))
        }
    }

    /// Check that every text-equality field in the filter tree exists in the facet set;
    /// numeric and range filters are always valid.
    fn is_valid_for_facet_set(&self, filter: &Filter, facet_set: &HashSet<String>) -> bool {
        match filter {
            Filter::Equals { field, value } => {
                if matches!(value, crate::types::FieldValue::Text(_)) {
                    facet_set.contains(field)
                } else {
                    true
                }
            }
            Filter::And(filters) | Filter::Or(filters) => filters
                .iter()
                .all(|f| self.is_valid_for_facet_set(f, facet_set)),
            Filter::Not(inner) => self.is_valid_for_facet_set(inner, facet_set),
            _ => true,
        }
    }

    fn has_not(&self, filter: &Filter) -> bool {
        match filter {
            Filter::Not(_) | Filter::NotEquals { .. } => true,
            Filter::And(filters) | Filter::Or(filters) => filters.iter().any(|f| self.has_not(f)),
            _ => false,
        }
    }

    /// Serialize a `Filter` tree into a Tantivy query string, quoting text values
    /// that contain spaces or special characters.
    fn to_query_string(&self, filter: &Filter) -> Result<String> {
        match filter {
            Filter::Equals { field, value } => self.equals_query_string_for_value(field, value),
            Filter::Range { field, min, max } => Ok(format!(
                "{}:[{} TO {}]",
                self.json_filter_field_path(field)?,
                min,
                max
            )),
            Filter::GreaterThan { field, value } => match value {
                crate::types::FieldValue::Integer(i) => Ok(format!(
                    "{}:[{} TO *]",
                    self.json_filter_field_path(field)?,
                    i + 1
                )),
                crate::types::FieldValue::Float(_) => {
                    Err(crate::error::FlapjackError::InvalidQuery(format!(
                        "Exclusive '>' not supported on floats for field '{}'. Use '>='",
                        field
                    )))
                }
                crate::types::FieldValue::Date(d) => Ok(format!(
                    "{}:[{} TO *]",
                    self.json_filter_field_path(field)?,
                    d + 1
                )),
                _ => Err(crate::error::FlapjackError::InvalidQuery(
                    "GreaterThan only supports integer/date values".to_string(),
                )),
            },
            Filter::GreaterThanOrEqual { field, value } => Ok(format!(
                "{}:[{} TO *]",
                self.json_filter_field_path(field)?,
                self.format_range_value(value)?
            )),
            Filter::LessThan { field, value } => match value {
                crate::types::FieldValue::Integer(i) => Ok(format!(
                    "{}:[* TO {}]",
                    self.json_filter_field_path(field)?,
                    i - 1
                )),
                crate::types::FieldValue::Float(_) => {
                    Err(crate::error::FlapjackError::InvalidQuery(format!(
                        "Exclusive '<' not supported on floats for field '{}'. Use '<='",
                        field
                    )))
                }
                crate::types::FieldValue::Date(d) => Ok(format!(
                    "{}:[* TO {}]",
                    self.json_filter_field_path(field)?,
                    d - 1
                )),
                _ => Err(crate::error::FlapjackError::InvalidQuery(
                    "LessThan only supports integer/date values".to_string(),
                )),
            },
            Filter::LessThanOrEqual { field, value } => Ok(format!(
                "{}:[* TO {}]",
                self.json_filter_field_path(field)?,
                self.format_range_value(value)?
            )),
            Filter::And(filters) => {
                let parts: Result<Vec<_>> =
                    filters.iter().map(|f| self.to_query_string(f)).collect();
                Ok(format!("({})", parts?.join(" AND ")))
            }
            Filter::Or(filters) => {
                let parts: Result<Vec<_>> =
                    filters.iter().map(|f| self.to_query_string(f)).collect();
                Ok(format!("({})", parts?.join(" OR ")))
            }
            Filter::Not(_) | Filter::NotEquals { .. } => {
                Err(crate::error::FlapjackError::InvalidQuery(
                    "NOT filters must use hybrid compilation".to_string(),
                ))
            }
        }
    }

    fn format_value(&self, value: &crate::types::FieldValue) -> Result<String> {
        match value {
            crate::types::FieldValue::Object(_) => Err(crate::error::FlapjackError::InvalidQuery(
                "Object values cannot be used in filters directly".to_string(),
            )),
            crate::types::FieldValue::Array(_) => Err(crate::error::FlapjackError::InvalidQuery(
                "Array values cannot be used in filters directly".to_string(),
            )),
            crate::types::FieldValue::Bool(b) => Ok(b.to_string()),
            crate::types::FieldValue::Text(s) => Ok(self.format_text_value(s)),
            crate::types::FieldValue::Integer(i) => Ok(i.to_string()),
            crate::types::FieldValue::Float(f) => Ok(f.to_string()),
            crate::types::FieldValue::Date(d) => Ok(d.to_string()),
            crate::types::FieldValue::Facet(s) => Ok(self.format_facet_value(s)),
        }
    }

    fn equals_query_string_for_value(
        &self,
        field: &str,
        value: &crate::types::FieldValue,
    ) -> Result<String> {
        let field_path = self.json_filter_field_path(field)?;
        match value {
            crate::types::FieldValue::Integer(i) => Ok(format!("{}:[{} TO {}]", field_path, i, i)),
            crate::types::FieldValue::Float(f) => Ok(format!("{}:[{} TO {}]", field_path, f, f)),
            crate::types::FieldValue::Date(d) => Ok(format!("{}:[{} TO {}]", field_path, d, d)),
            _ => {
                let formatted = self.format_value(value)?;
                Ok(format!("{}:{}", field_path, formatted))
            }
        }
    }

    fn format_text_value(&self, value: &str) -> String {
        if value.chars().all(Self::is_safe_bare_text_char) {
            value.to_string()
        } else {
            format!("\"{}\"", Self::escape_quoted_query_value(value))
        }
    }

    fn format_facet_value(&self, value: &str) -> String {
        format!("\"{}\"", Self::escape_quoted_query_value(value))
    }

    fn format_range_value(&self, value: &crate::types::FieldValue) -> Result<String> {
        match value {
            crate::types::FieldValue::Integer(i) => Ok(i.to_string()),
            crate::types::FieldValue::Float(f) => Ok(f.to_string()),
            crate::types::FieldValue::Date(d) => Ok(d.to_string()),
            _ => Err(crate::error::FlapjackError::InvalidQuery(
                "Range queries only support numeric/date values".to_string(),
            )),
        }
    }

    /// Recursively count the total number of leaf clauses in a filter tree
    /// for enforcement against `MAX_BOOLEAN_CLAUSES`.
    fn count_clauses(&self, filter: &Filter) -> usize {
        fn count_recursive(filter: &Filter) -> usize {
            match filter {
                Filter::Equals { .. }
                | Filter::NotEquals { .. }
                | Filter::GreaterThan { .. }
                | Filter::GreaterThanOrEqual { .. }
                | Filter::LessThan { .. }
                | Filter::LessThanOrEqual { .. }
                | Filter::Range { .. } => 1,
                Filter::Not(inner) => count_recursive(inner),
                Filter::And(filters) | Filter::Or(filters) => {
                    filters.iter().map(count_recursive).sum()
                }
            }
        }
        count_recursive(filter)
    }

    /// Recursively compile a filter tree into Tantivy boolean queries, enforcing
    /// `MAX_FILTER_DEPTH` and mapping each leaf/combinator to the appropriate query type.
    fn compile_with_hybrid(&self, filter: &Filter, depth: usize) -> Result<Box<dyn Query>> {
        if depth > Self::MAX_FILTER_DEPTH {
            return Err(crate::error::FlapjackError::InvalidQuery(format!(
                "Filter nesting exceeds {} levels",
                Self::MAX_FILTER_DEPTH
            )));
        }

        match filter {
            Filter::Not(inner) => {
                let inner_query = self.compile_with_hybrid(inner, depth + 1)?;
                Ok(Box::new(BooleanQuery::new(vec![
                    (Occur::Must, Box::new(AllQuery) as Box<dyn Query>),
                    (Occur::MustNot, inner_query),
                ])))
            }
            Filter::NotEquals { field, value } => {
                let equals_str = self.equals_query_string_for_value(field, value)?;
                let equals_query = self
                    .query_parser
                    .parse_query(&equals_str)
                    .map_err(|e| crate::error::FlapjackError::InvalidQuery(e.to_string()))?;
                Ok(Box::new(BooleanQuery::new(vec![
                    (Occur::Must, Box::new(AllQuery) as Box<dyn Query>),
                    (Occur::MustNot, equals_query),
                ])))
            }
            Filter::And(filters) => {
                let mut subqueries = Vec::new();
                for f in filters {
                    subqueries.push((Occur::Must, self.compile_with_hybrid(f, depth + 1)?));
                }
                Ok(Box::new(BooleanQuery::new(subqueries)))
            }
            Filter::Or(filters) => {
                let mut subqueries = Vec::new();
                for f in filters {
                    subqueries.push((Occur::Should, self.compile_with_hybrid(f, depth + 1)?));
                }
                Ok(Box::new(BooleanQuery::new(subqueries)))
            }
            _ => {
                let query_str = self.to_query_string(filter)?;
                self.query_parser
                    .parse_query(&query_str)
                    .map_err(|e| crate::error::FlapjackError::InvalidQuery(e.to_string()))
            }
        }
    }
}

#[cfg(test)]
#[path = "filter_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "filter_edge_tests.rs"]
mod edge_tests;

#[cfg(test)]
mod security_tests {
    use super::*;
    use crate::error::FlapjackError;
    use crate::index::schema::SchemaBuilder;
    use crate::types::FieldValue;

    fn make_compiler() -> FilterCompiler {
        let schema = SchemaBuilder::new().build();
        FilterCompiler::new(schema.to_tantivy())
    }

    #[test]
    fn invalid_filter_field_is_rejected_before_query_string_build() {
        let compiler = make_compiler();
        let filter = Filter::Equals {
            field: "category:admin".into(),
            value: FieldValue::Text("books".into()),
        };

        match compiler.to_query_string(&filter) {
            Err(FlapjackError::InvalidQuery(message)) => {
                assert!(message.contains("Invalid filter field"));
            }
            other => panic!("expected InvalidQuery, got {other:?}"),
        }
    }

    #[test]
    fn quoted_filter_values_escape_backslashes_and_quotes() {
        let compiler = make_compiler();

        assert_eq!(
            compiler
                .format_value(&FieldValue::Text("promo\\bundle\"today".into()))
                .unwrap(),
            "\"promo\\\\bundle\\\"today\""
        );
        assert_eq!(
            compiler
                .format_value(&FieldValue::Facet("brand\\\"name".into()))
                .unwrap(),
            "\"brand\\\\\\\"name\""
        );
    }

    #[test]
    fn bool_filter_values_serialize_without_quotes() {
        let compiler = make_compiler();
        let filter = Filter::Equals {
            field: "featured".into(),
            value: FieldValue::Bool(true),
        };

        assert_eq!(
            compiler.to_query_string(&filter).unwrap(),
            "_json_filter.featured:true"
        );
    }
}
