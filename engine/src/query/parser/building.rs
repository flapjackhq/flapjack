use super::*;

impl QueryParser {
    /// If advanced syntax is enabled, attempt to parse the query as a quoted phrase
    /// or field-qualified expression; returns `None` if not applicable.
    pub(super) fn try_parse_advanced_syntax(
        &self,
        normalized_query_text: &str,
    ) -> Option<Result<Box<dyn TantivyQuery>>> {
        if !self.advanced_syntax {
            return None;
        }

        let (phrases, exclusions, remaining) =
            Self::preprocess_advanced_syntax(normalized_query_text);
        let effective_phrases = if self.is_advanced_feature_enabled("exactPhrase") {
            phrases
        } else {
            Vec::new()
        };
        let effective_exclusions = if self.is_advanced_feature_enabled("excludeWords") {
            exclusions
        } else {
            Vec::new()
        };
        let effective_remaining = if !self.is_advanced_feature_enabled("exactPhrase")
            || !self.is_advanced_feature_enabled("excludeWords")
        {
            self.rebuild_query_for_enabled_features(normalized_query_text)
        } else {
            remaining
        };

        if effective_phrases.is_empty() && effective_exclusions.is_empty() {
            None
        } else {
            Some(self.parse_with_advanced_syntax(
                &effective_remaining,
                &effective_phrases,
                &effective_exclusions,
                normalized_query_text.ends_with(' '),
            ))
        }
    }

    /// For single-token queries without a trailing space, emit a `ShortQueryPlaceholder`
    /// that defers expansion until a searcher is available for term enumeration.
    pub(super) fn try_parse_short_query(
        &self,
        tokens: &[String],
        has_trailing_space: bool,
    ) -> Option<Box<dyn TantivyQuery>> {
        if tokens.len() != 1 || tokens[0].chars().count() > 2 {
            return None;
        }

        tracing::trace!(
            "[PARSER] Short query detected: token={}, char_count={}, has_trailing_space={}",
            tokens[0],
            tokens[0].chars().count(),
            has_trailing_space
        );

        if has_trailing_space {
            return Some(self.build_exact_short_query(&tokens[0]));
        }

        let marker = self.build_short_query_marker(&tokens[0]);
        tracing::trace!(
            "[PARSER] Creating placeholder with {} paths",
            self.searchable_paths.len()
        );
        Some(Box::new(ShortQueryPlaceholder { marker }))
    }

    fn build_short_query_marker(&self, token: &str) -> ShortQueryMarker {
        ShortQueryMarker {
            token: token.to_string(),
            paths: self.searchable_paths.clone(),
            weights: self.weights.clone(),
            field: self.fields[0],
        }
    }

    /// Build an exact-match query for a single token across all searchable paths
    /// using the JSON exact field (or primary field as fallback).
    fn build_exact_short_query(&self, token: &str) -> Box<dyn TantivyQuery> {
        let target_field = self.json_exact_field.unwrap_or(self.fields[0]);
        let field_queries: Vec<QueryClause> = self
            .searchable_paths
            .iter()
            .enumerate()
            .map(|(path_idx, path)| {
                let term_text = format!("{}\0s{}", path, token);
                let term = tantivy::Term::from_field_text(target_field, &term_text);
                let token_query: Box<dyn TantivyQuery> = Box::new(tantivy::query::TermQuery::new(
                    term,
                    tantivy::schema::IndexRecordOption::WithFreqsAndPositions,
                ));
                (
                    tantivy::query::Occur::Should,
                    self.apply_query_weight(path_idx, token_query),
                )
            })
            .collect();
        Box::new(tantivy::query::BooleanQuery::new(field_queries))
    }

    /// Build a boolean query from multiple tokens, applying per-token prefix/exact
    /// mode, word splitting, and optional-word semantics when `all_optional` is set.
    pub(super) fn build_multi_token_query(
        &self,
        normalized_query_text: &str,
        raw_query_text: &str,
        tokens: &[String],
        has_trailing_space: bool,
    ) -> Box<dyn TantivyQuery> {
        tracing::trace!(
            "QueryParser: query='{}', searchable_paths={:?}, weights={:?}, tokens={:?}",
            raw_query_text,
            self.searchable_paths,
            self.weights,
            tokens
        );

        let json_search_field = self.fields[0];
        let max_fuzzy_paths = self.max_fuzzy_paths(tokens.len());
        let last_idx = tokens.len() - 1;
        let word_queries: Vec<QueryClause> = tokens
            .iter()
            .enumerate()
            .map(|(token_idx, token)| {
                self.build_word_query(
                    token,
                    token_idx,
                    last_idx,
                    has_trailing_space,
                    max_fuzzy_paths,
                    json_search_field,
                )
            })
            .collect();

        tracing::trace!(
            "[PARSER] multi-token parse built {} clauses for '{}'",
            word_queries.len(),
            normalized_query_text
        );
        Box::new(tantivy::query::BooleanQuery::new(word_queries))
    }

    fn max_fuzzy_paths(&self, token_count: usize) -> usize {
        if token_count >= 3 {
            2.min(self.searchable_paths.len())
        } else {
            4.min(self.searchable_paths.len())
        }
    }

    /// Build the query clause for a single token, choosing prefix vs exact mode based
    /// on position (last token + no trailing space = prefix), and applying word splitting.
    fn build_word_query(
        &self,
        token: &str,
        token_idx: usize,
        last_idx: usize,
        has_trailing_space: bool,
        max_fuzzy_paths: usize,
        json_search_field: tantivy::schema::Field,
    ) -> QueryClause {
        let is_prefix = self.is_prefix_token(token_idx, last_idx, has_trailing_space);
        tracing::trace!(
            "[PARSER] token='{}' len={} is_last={} is_prefix={} query_type={}",
            token,
            token.len(),
            token_idx == last_idx,
            is_prefix,
            self.query_type
        );

        if token.chars().count() <= 2 && is_prefix {
            let marker = self.build_short_query_marker(token);
            return (
                tantivy::query::Occur::Must,
                Box::new(ShortQueryPlaceholder { marker }),
            );
        }

        let target_field = if is_prefix {
            json_search_field
        } else {
            self.json_exact_field.unwrap_or(json_search_field)
        };
        let token_lc = token.to_lowercase();
        let plural_forms = self.plural_forms_for_token(token);
        let field_queries: Vec<QueryClause> = self
            .searchable_paths
            .iter()
            .enumerate()
            .map(|(path_idx, path)| {
                (
                    tantivy::query::Occur::Should,
                    self.build_field_query(FieldQueryContext {
                        token,
                        token_lc: &token_lc,
                        path,
                        path_idx,
                        is_prefix,
                        target_field,
                        json_search_field,
                        max_fuzzy_paths,
                        plural_forms: &plural_forms,
                    }),
                )
            })
            .collect();

        let occur = if self.all_optional {
            tantivy::query::Occur::Should
        } else {
            tantivy::query::Occur::Must
        };
        (
            occur,
            Box::new(tantivy::query::BooleanQuery::new(field_queries)),
        )
    }

    fn is_prefix_token(&self, token_idx: usize, last_idx: usize, has_trailing_space: bool) -> bool {
        match self.query_type.as_str() {
            "prefixAll" => true,
            "prefixNone" => false,
            _ => token_idx == last_idx && !has_trailing_space,
        }
    }

    fn plural_forms_for_token(&self, token: &str) -> Vec<String> {
        self.plural_map
            .as_ref()
            .and_then(|map| map.get(token))
            .map(|forms| {
                forms
                    .iter()
                    .filter(|form| form.as_str() != token)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Build a field-level query for a single token against one searchable path,
    /// applying stemming, typo tolerance, and plural wrapping as configured.
    fn build_field_query(&self, context: FieldQueryContext<'_>) -> Box<dyn TantivyQuery> {
        let effective_token: std::borrow::Cow<str> = if context.is_prefix {
            std::borrow::Cow::Borrowed(context.token)
        } else {
            self.stem_token(context.token)
        };
        let term_text = format!("{}\0s{}", context.path, effective_token);
        let term = tantivy::Term::from_field_text(context.target_field, &term_text);
        let distance = self.typo_distance_for_path(&context);
        tracing::trace!(
            "[PARSER] token='{}' path='{}' is_prefix={} field={:?}",
            context.token,
            context.path,
            context.is_prefix,
            if context.is_prefix { "search" } else { "exact" }
        );

        let token_query = self.build_term_query(&context, term, &term_text, distance);
        let plural_query = self.wrap_plural_forms(token_query, &context);
        self.apply_query_weight(context.path_idx, plural_query)
    }

    /// Compute the allowed Levenshtein distance for a token on a given path,
    /// respecting disabled-typo attributes/words and min-word-size thresholds.
    fn typo_distance_for_path(&self, context: &FieldQueryContext<'_>) -> u8 {
        if !self.typo_tolerance
            || self.disabled_typo_attrs.contains(context.path)
            || self.disabled_typo_words.contains(context.token_lc)
            || context.path_idx >= context.max_fuzzy_paths
        {
            return 0;
        }

        if context.token.len() >= self.min_word_size_for_2_typos {
            2
        } else if context.token.len() >= self.min_word_size_for_1_typo {
            1
        } else {
            0
        }
    }

    /// Build a fuzzy or exact term query for a token, adding a stripped-first-character
    /// fallback term query for short-prefix resilience when fuzzy matching is enabled.
    fn build_term_query(
        &self,
        context: &FieldQueryContext<'_>,
        term: tantivy::Term,
        term_text: &str,
        distance: u8,
    ) -> Box<dyn TantivyQuery> {
        if distance == 0 {
            return Box::new(tantivy::query::TermQuery::new(
                term,
                tantivy::schema::IndexRecordOption::WithFreqsAndPositions,
            ));
        }

        let exact = Box::new(tantivy::query::TermQuery::new(
            term.clone(),
            tantivy::schema::IndexRecordOption::WithFreqsAndPositions,
        ));
        let fuzzy_term = if context.is_prefix {
            let fuzzy_field = self.json_exact_field.unwrap_or(context.target_field);
            tantivy::Term::from_field_text(fuzzy_field, term_text)
        } else {
            term
        };
        let fuzzy = Box::new(tantivy::query::FuzzyTermQuery::new(
            fuzzy_term, distance, true,
        ));
        let mut clauses: Vec<QueryClause> = vec![
            (
                tantivy::query::Occur::Should,
                exact as Box<dyn TantivyQuery>,
            ),
            (
                tantivy::query::Occur::Should,
                fuzzy as Box<dyn TantivyQuery>,
            ),
        ];
        if let Some(stripped_query) = self.build_first_character_fallback(
            context.token,
            context.path,
            context.json_search_field,
            context.is_prefix,
        ) {
            clauses.push((tantivy::query::Occur::Should, stripped_query));
        }
        Box::new(tantivy::query::BooleanQuery::new(clauses))
    }

    /// Build a fallback query that drops the first character from the token and searches
    /// the search field for the remaining suffix, helping prefix queries survive a typo
    /// or omission in the first character.
    fn build_first_character_fallback(
        &self,
        token: &str,
        path: &str,
        json_search_field: tantivy::schema::Field,
        is_prefix: bool,
    ) -> Option<Box<dyn TantivyQuery>> {
        if !is_prefix || token.len() < 4 {
            return None;
        }

        let stripped = &token[token
            .char_indices()
            .nth(1)
            .map(|(index, _)| index)
            .unwrap_or(1)..];
        if stripped.len() < 3 {
            return None;
        }

        let stripped_term_text = format!("{}\0s{}", path, stripped);
        let stripped_term = tantivy::Term::from_field_text(json_search_field, &stripped_term_text);
        Some(Box::new(tantivy::query::TermQuery::new(
            stripped_term,
            tantivy::schema::IndexRecordOption::WithFreqsAndPositions,
        )))
    }

    /// Wrap a token query with OR-combined plural/singular alternatives from the
    /// plural map, so both "shoe" and "shoes" match regardless of query form.
    fn wrap_plural_forms(
        &self,
        token_query: Box<dyn TantivyQuery>,
        context: &FieldQueryContext<'_>,
    ) -> Box<dyn TantivyQuery> {
        if context.plural_forms.is_empty() {
            return token_query;
        }

        let mut plural_clauses: Vec<QueryClause> =
            vec![(tantivy::query::Occur::Should, token_query)];
        for plural in context.plural_forms {
            let effective_plural: std::borrow::Cow<'_, str> = if context.is_prefix {
                std::borrow::Cow::Borrowed(plural.as_str())
            } else {
                self.stem_token(plural)
            };
            let plural_term_text = format!("{}\0s{}", context.path, effective_plural);
            let plural_term =
                tantivy::Term::from_field_text(context.target_field, &plural_term_text);
            let plural_query: Box<dyn TantivyQuery> = Box::new(tantivy::query::TermQuery::new(
                plural_term,
                tantivy::schema::IndexRecordOption::WithFreqsAndPositions,
            ));
            plural_clauses.push((tantivy::query::Occur::Should, plural_query));
        }

        Box::new(tantivy::query::BooleanQuery::new(plural_clauses))
    }

    fn apply_query_weight(
        &self,
        path_idx: usize,
        token_query: Box<dyn TantivyQuery>,
    ) -> Box<dyn TantivyQuery> {
        let weight = self.weights.get(path_idx).copied().unwrap_or(1.0);
        if weight == 1.0 {
            token_query
        } else {
            Box::new(tantivy::query::BoostQuery::new(token_query, weight))
        }
    }

    /// Rebuild query text treating disabled advanced syntax features as regular text.
    fn rebuild_query_for_enabled_features(&self, text: &str) -> String {
        let exact_phrase_enabled = self.is_advanced_feature_enabled("exactPhrase");
        let exclude_words_enabled = self.is_advanced_feature_enabled("excludeWords");

        let mut remaining = String::new();
        let mut chars = text.chars().peekable();
        while let Some(&c) = chars.peek() {
            if c == '"' {
                if exact_phrase_enabled {
                    chars.next();
                    while let Some(&nc) = chars.peek() {
                        if nc == '"' {
                            chars.next();
                            break;
                        }
                        chars.next();
                    }
                } else {
                    chars.next();
                    while let Some(&nc) = chars.peek() {
                        if nc == '"' {
                            chars.next();
                            break;
                        }
                        remaining.push(nc);
                        chars.next();
                    }
                }
            } else if c == '-' && (remaining.is_empty() || remaining.ends_with(' ')) {
                if exclude_words_enabled {
                    chars.next();
                    while let Some(&nc) = chars.peek() {
                        if nc.is_whitespace() {
                            break;
                        }
                        chars.next();
                    }
                } else {
                    chars.next();
                    while let Some(&nc) = chars.peek() {
                        if nc.is_whitespace() {
                            break;
                        }
                        remaining.push(nc);
                        chars.next();
                    }
                }
            } else {
                remaining.push(c);
                chars.next();
            }
        }
        remaining.trim().to_string()
    }

    pub fn fields(&self) -> &[tantivy::schema::Field] {
        &self.fields
    }

    pub fn extract_terms(&self, query: &Query) -> Vec<String> {
        split_cjk_aware_with_indexed_separators(
            &normalize_for_search(
                &query.text,
                &self.keep_diacritics_on_characters,
                &self.custom_normalization,
            ),
            &self.indexed_separators,
        )
        .into_iter()
        .map(|s| s.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|s| !s.is_empty())
        .collect()
    }

    /// Extract "quoted phrases" and -exclusion terms from query text.
    pub(super) fn preprocess_advanced_syntax(text: &str) -> (Vec<String>, Vec<String>, String) {
        let mut phrases = Vec::new();
        let mut exclusions = Vec::new();
        let mut remaining = String::new();

        let mut chars = text.chars().peekable();
        while let Some(&c) = chars.peek() {
            if c == '"' {
                chars.next();
                let mut phrase = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc == '"' {
                        chars.next();
                        break;
                    }
                    phrase.push(nc);
                    chars.next();
                }
                let trimmed = phrase.trim().to_string();
                if !trimmed.is_empty() {
                    phrases.push(trimmed);
                }
            } else if c == '-' && (remaining.is_empty() || remaining.ends_with(' ')) {
                chars.next();
                let mut word = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc.is_whitespace() {
                        break;
                    }
                    word.push(nc);
                    chars.next();
                }
                if !word.is_empty() {
                    exclusions.push(word);
                }
            } else {
                remaining.push(c);
                chars.next();
            }
        }
        (phrases, exclusions, remaining.trim().to_string())
    }

    /// Build a query combining phrases (Must), exclusions (MustNot), and remaining text.
    fn parse_with_advanced_syntax(
        &self,
        remaining_text: &str,
        phrases: &[String],
        exclusions: &[String],
        _has_trailing_space: bool,
    ) -> Result<Box<dyn TantivyQuery>> {
        let json_search_field = self.fields[0];
        let exact_field = self.json_exact_field.unwrap_or(json_search_field);

        let mut clauses: Vec<(tantivy::query::Occur, Box<dyn TantivyQuery>)> = Vec::new();

        // Parse remaining text as a normal query
        if !remaining_text.trim().is_empty() {
            let sub_query = Query {
                text: remaining_text.to_string(),
            };
            let normal_parser = QueryParser {
                advanced_syntax: false,
                ..self.clone_parser()
            };
            if let Ok(q) = normal_parser.parse(&sub_query) {
                clauses.push((tantivy::query::Occur::Must, q));
            }
        }

        // Phrase queries: all words in the phrase must match (exact, on same field paths)
        for phrase in phrases {
            let words: Vec<String> = phrase
                .to_lowercase()
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            if words.is_empty() {
                continue;
            }
            let mut phrase_clauses: Vec<(tantivy::query::Occur, Box<dyn TantivyQuery>)> =
                Vec::new();
            for word in &words {
                let mut field_queries: Vec<(tantivy::query::Occur, Box<dyn TantivyQuery>)> =
                    Vec::new();
                for (path_idx, path) in self.searchable_paths.iter().enumerate() {
                    let term_text = format!("{}\0s{}", path, word);
                    let term = tantivy::Term::from_field_text(exact_field, &term_text);
                    let tq: Box<dyn TantivyQuery> = Box::new(tantivy::query::TermQuery::new(
                        term,
                        tantivy::schema::IndexRecordOption::WithFreqs,
                    ));
                    let weight = self.weights.get(path_idx).copied().unwrap_or(1.0);
                    field_queries.push((
                        tantivy::query::Occur::Should,
                        Box::new(tantivy::query::BoostQuery::new(tq, weight)),
                    ));
                }
                phrase_clauses.push((
                    tantivy::query::Occur::Must,
                    Box::new(tantivy::query::BooleanQuery::new(field_queries)),
                ));
            }
            clauses.push((
                tantivy::query::Occur::Must,
                Box::new(tantivy::query::BooleanQuery::new(phrase_clauses)),
            ));
        }

        // Exclusion queries: MustNot for each excluded term
        for exclusion in exclusions {
            let word = exclusion.to_lowercase();
            let mut field_queries: Vec<(tantivy::query::Occur, Box<dyn TantivyQuery>)> = Vec::new();
            for path in &self.searchable_paths {
                let term_text = format!("{}\0s{}", path, word);
                let term = tantivy::Term::from_field_text(exact_field, &term_text);
                let tq: Box<dyn TantivyQuery> = Box::new(tantivy::query::TermQuery::new(
                    term,
                    tantivy::schema::IndexRecordOption::WithFreqs,
                ));
                field_queries.push((tantivy::query::Occur::Should, tq));
            }
            clauses.push((
                tantivy::query::Occur::MustNot,
                Box::new(tantivy::query::BooleanQuery::new(field_queries)),
            ));
        }

        if clauses.is_empty() {
            return Ok(Box::new(tantivy::query::AllQuery));
        }

        Ok(Box::new(tantivy::query::BooleanQuery::new(clauses)))
    }

    /// Clone parser fields without the Clone trait (for recursion avoidance)
    fn clone_parser(&self) -> QueryParser {
        QueryParser {
            fields: self.fields.clone(),
            json_exact_field: self.json_exact_field,
            weights: self.weights.clone(),
            searchable_paths: self.searchable_paths.clone(),
            query_type: self.query_type.clone(),
            plural_map: self.plural_map.clone(),
            indexed_separators: self.indexed_separators.clone(),
            keep_diacritics_on_characters: self.keep_diacritics_on_characters.clone(),
            custom_normalization: self.custom_normalization.clone(),
            disabled_typo_words: self.disabled_typo_words.clone(),
            disabled_typo_attrs: self.disabled_typo_attrs.clone(),
            typo_tolerance: self.typo_tolerance,
            min_word_size_for_1_typo: self.min_word_size_for_1_typo,
            min_word_size_for_2_typos: self.min_word_size_for_2_typos,
            all_optional: self.all_optional,
            advanced_syntax: self.advanced_syntax,
            advanced_syntax_features: self.advanced_syntax_features.clone(),
            stemmer_language: self.stemmer_language,
        }
    }
}
