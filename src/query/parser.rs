use crate::error::Result;
use crate::types::Query;

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |
        '\u{3400}'..='\u{4DBF}' |
        '\u{F900}'..='\u{FAFF}' |
        '\u{2E80}'..='\u{2EFF}' |
        '\u{3000}'..='\u{303F}' |
        '\u{3040}'..='\u{309F}' |
        '\u{30A0}'..='\u{30FF}' |
        '\u{31F0}'..='\u{31FF}' |
        '\u{AC00}'..='\u{D7AF}' |
        '\u{1100}'..='\u{11FF}' |
        '\u{20000}'..='\u{2A6DF}' |
        '\u{2A700}'..='\u{2B73F}' |
        '\u{2B740}'..='\u{2B81F}' |
        '\u{2B820}'..='\u{2CEAF}'
    )
}

fn split_cjk_aware(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if is_cjk(c) {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            tokens.push(c.to_string());
        } else if c.is_alphanumeric() {
            current.push(c);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}
use tantivy::query::{Query as TantivyQuery, Scorer, Weight};
use tantivy::schema::Schema as TantivySchema;
use tantivy::DocSet;

#[derive(Debug, Clone)]
pub struct ShortQueryPlaceholder {
    pub marker: ShortQueryMarker,
}

impl TantivyQuery for ShortQueryPlaceholder {
    fn weight(
        &self,
        _enable_scoring: tantivy::query::EnableScoring,
    ) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(ShortQueryWeight))
    }
}

struct ShortQueryWeight;

impl Weight for ShortQueryWeight {
    fn scorer(
        &self,
        _reader: &tantivy::SegmentReader,
        _boost: tantivy::Score,
    ) -> tantivy::Result<Box<dyn Scorer>> {
        Ok(Box::new(EmptyScorer))
    }

    fn explain(
        &self,
        _reader: &tantivy::SegmentReader,
        _doc: tantivy::DocId,
    ) -> tantivy::Result<tantivy::query::Explanation> {
        Ok(tantivy::query::Explanation::new(
            "ShortQueryPlaceholder",
            0.0,
        ))
    }
}

struct EmptyScorer;

impl DocSet for EmptyScorer {
    fn advance(&mut self) -> tantivy::DocId {
        tantivy::TERMINATED
    }

    fn doc(&self) -> tantivy::DocId {
        tantivy::TERMINATED
    }

    fn size_hint(&self) -> u32 {
        0
    }
}

impl Scorer for EmptyScorer {
    fn score(&mut self) -> tantivy::Score {
        0.0
    }
}

pub struct QueryParser {
    fields: Vec<tantivy::schema::Field>,
    json_exact_field: Option<tantivy::schema::Field>,
    weights: Vec<f32>,
    searchable_paths: Vec<String>,
    query_type: String,
    plural_map: Option<std::collections::HashMap<String, Vec<String>>>,
}

#[derive(Debug, Clone)]
pub struct ShortQueryMarker {
    pub token: String,
    pub paths: Vec<String>,
    pub weights: Vec<f32>,
    pub field: tantivy::schema::Field,
}

impl QueryParser {
    pub fn new(_schema: &TantivySchema, default_fields: Vec<tantivy::schema::Field>) -> Self {
        let weights = vec![1.0; default_fields.len()];
        QueryParser {
            fields: default_fields,
            json_exact_field: None,
            weights,
            searchable_paths: vec![],
            query_type: "prefixLast".to_string(),
            plural_map: None,
        }
    }

    pub fn new_with_weights(
        _schema: &TantivySchema,
        fields: Vec<tantivy::schema::Field>,
        weights: Vec<f32>,
        searchable_paths: Vec<String>,
    ) -> Self {
        assert_eq!(
            weights.len(),
            searchable_paths.len(),
            "Weights and searchable_paths must match"
        );
        QueryParser {
            fields,
            json_exact_field: None,
            weights,
            searchable_paths,
            query_type: "prefixLast".to_string(),
            plural_map: None,
        }
    }

    pub fn with_exact_field(mut self, field: tantivy::schema::Field) -> Self {
        self.json_exact_field = Some(field);
        self
    }

    pub fn with_query_type(mut self, query_type: &str) -> Self {
        self.query_type = query_type.to_string();
        self
    }

    pub fn with_plural_map(
        mut self,
        plural_map: Option<std::collections::HashMap<String, Vec<String>>>,
    ) -> Self {
        self.plural_map = plural_map;
        self
    }

    pub fn parse(&self, query: &Query) -> Result<Box<dyn TantivyQuery>> {
        let has_trailing_space = query.text.ends_with(' ');
        let text = query.text.to_lowercase().trim_end_matches('*').to_string();
        let tokens: Vec<String> = split_cjk_aware(&text);

        tracing::info!(
            "[PARSER] parse() called: query='{}', tokens={:?}, searchable_paths={:?}",
            query.text,
            tokens,
            self.searchable_paths
        );

        if tokens.is_empty() {
            return Ok(Box::new(tantivy::query::AllQuery));
        }

        // SHORT QUERY PATH: Single token ≤2 chars uses prefix enumeration
        // BUT: if trailing space, treat as exact match (no prefix)
        if tokens.len() == 1 && tokens[0].chars().count() <= 2 {
            tracing::info!(
                "[PARSER] Short query detected: token={}, char_count={}, has_trailing_space={}",
                tokens[0],
                tokens[0].chars().count(),
                has_trailing_space
            );

            if has_trailing_space {
                // Trailing space = exact match required, use _json_exact field
                // For short tokens with trailing space, this likely returns 0 results (intended)
                let target_field = self.json_exact_field.unwrap_or(self.fields[0]);
                let mut field_queries: Vec<(tantivy::query::Occur, Box<dyn TantivyQuery>)> =
                    Vec::new();

                for (path_idx, path) in self.searchable_paths.iter().enumerate() {
                    let term_text = format!("{}\0s{}", path, tokens[0]);
                    let term = tantivy::Term::from_field_text(target_field, &term_text);
                    let token_query: Box<dyn TantivyQuery> =
                        Box::new(tantivy::query::TermQuery::new(
                            term,
                            tantivy::schema::IndexRecordOption::WithFreqsAndPositions,
                        ));
                    let weight = if path_idx < self.weights.len() {
                        self.weights[path_idx]
                    } else {
                        1.0
                    };
                    let boosted_query: Box<dyn TantivyQuery> = if weight != 1.0 {
                        Box::new(tantivy::query::BoostQuery::new(token_query, weight))
                    } else {
                        token_query
                    };
                    field_queries.push((tantivy::query::Occur::Should, boosted_query));
                }

                return Ok(Box::new(tantivy::query::BooleanQuery::new(field_queries)));
            }

            let marker = ShortQueryMarker {
                token: tokens[0].to_string(),
                paths: self.searchable_paths.clone(),
                weights: self.weights.clone(),
                field: self.fields[0],
            };
            tracing::info!(
                "[PARSER] Creating placeholder with {} paths",
                self.searchable_paths.len()
            );
            return Ok(Box::new(ShortQueryPlaceholder { marker }));
        }

        tracing::info!(
            "QueryParser: query='{}', searchable_paths={:?}, weights={:?}, tokens={:?}",
            query.text,
            self.searchable_paths,
            self.weights,
            tokens
        );

        let json_search_field = self.fields[0];
        let mut word_queries: Vec<(tantivy::query::Occur, Box<dyn TantivyQuery>)> = Vec::new();

        let last_idx = tokens.len() - 1;
        for (token_idx, token) in tokens.iter().enumerate() {
            let is_last = token_idx == last_idx;
            let is_prefix = match self.query_type.as_str() {
                "prefixAll" => true,
                "prefixNone" => false,
                _ => is_last && !has_trailing_space,
            };

            tracing::trace!(
                "[PARSER] token='{}' len={} is_last={} is_prefix={} query_type={}",
                token,
                token.len(),
                is_last,
                is_prefix,
                self.query_type
            );

            if token.chars().count() <= 2 && is_prefix {
                let marker = ShortQueryMarker {
                    token: token.to_string(),
                    paths: self.searchable_paths.clone(),
                    weights: self.weights.clone(),
                    field: self.fields[0],
                };
                word_queries.push((
                    tantivy::query::Occur::Must,
                    Box::new(ShortQueryPlaceholder { marker }),
                ));
                continue;
            }

            let mut field_queries: Vec<(tantivy::query::Occur, Box<dyn TantivyQuery>)> = Vec::new();

            let target_field = if is_prefix {
                json_search_field
            } else {
                self.json_exact_field.unwrap_or(json_search_field)
            };

            let plural_forms: Vec<String> = self
                .plural_map
                .as_ref()
                .and_then(|m| m.get(token.as_str()))
                .map(|forms| {
                    forms
                        .iter()
                        .filter(|f| f.as_str() != token.as_str())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            for (path_idx, path) in self.searchable_paths.iter().enumerate() {
                let term_text = format!("{}\0s{}", path, token);
                let term = tantivy::Term::from_field_text(target_field, &term_text);

                let distance = if token.len() >= 4 { 1 } else { 0 };
                tracing::trace!(
                    "[PARSER] token='{}' path='{}' is_prefix={} field={:?}",
                    token,
                    path,
                    is_prefix,
                    if is_prefix { "search" } else { "exact" }
                );

                let token_query: Box<dyn TantivyQuery> = if distance > 0 {
                    let exact = Box::new(tantivy::query::TermQuery::new(
                        term.clone(),
                        tantivy::schema::IndexRecordOption::WithFreqsAndPositions,
                    ));
                    let fuzzy = Box::new(tantivy::query::FuzzyTermQuery::new(term, distance, true));
                    Box::new(tantivy::query::BooleanQuery::new(vec![
                        (tantivy::query::Occur::Should, exact),
                        (tantivy::query::Occur::Should, fuzzy),
                    ]))
                } else {
                    Box::new(tantivy::query::TermQuery::new(
                        term,
                        tantivy::schema::IndexRecordOption::WithFreqsAndPositions,
                    ))
                };

                let token_query: Box<dyn TantivyQuery> = if !plural_forms.is_empty() {
                    let mut plural_clauses: Vec<(tantivy::query::Occur, Box<dyn TantivyQuery>)> =
                        vec![(tantivy::query::Occur::Should, token_query)];
                    for plural in &plural_forms {
                        let plural_term_text = format!("{}\0s{}", path, plural);
                        let plural_term =
                            tantivy::Term::from_field_text(target_field, &plural_term_text);
                        let plural_q: Box<dyn TantivyQuery> =
                            Box::new(tantivy::query::TermQuery::new(
                                plural_term,
                                tantivy::schema::IndexRecordOption::WithFreqsAndPositions,
                            ));
                        plural_clauses.push((tantivy::query::Occur::Should, plural_q));
                    }
                    Box::new(tantivy::query::BooleanQuery::new(plural_clauses))
                } else {
                    token_query
                };

                let weight = if path_idx < self.weights.len() {
                    self.weights[path_idx]
                } else {
                    1.0
                };
                let boosted_query: Box<dyn TantivyQuery> = if weight != 1.0 {
                    Box::new(tantivy::query::BoostQuery::new(token_query, weight))
                } else {
                    token_query
                };

                field_queries.push((tantivy::query::Occur::Should, boosted_query));
            }

            word_queries.push((
                tantivy::query::Occur::Must,
                Box::new(tantivy::query::BooleanQuery::new(field_queries)),
            ));
        }

        Ok(Box::new(tantivy::query::BooleanQuery::new(word_queries)))
    }

    pub fn fields(&self) -> &[tantivy::schema::Field] {
        &self.fields
    }

    pub fn extract_terms(&self, query: &Query) -> Vec<String> {
        split_cjk_aware(&query.text.to_lowercase())
            .into_iter()
            .map(|s| s.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}
