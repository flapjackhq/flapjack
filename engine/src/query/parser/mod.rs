use crate::error::Result;
use crate::text_normalization::normalize_for_search;
use crate::types::Query;

/// Determine if a character is within CJK (Chinese, Japanese, Korean) or related Unicode ranges, including Han ideographs, Hiragana, Katakana, Hangul, and combining marks.
pub(crate) fn is_cjk(c: char) -> bool {
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

#[cfg(test)]
fn split_cjk_aware(text: &str) -> Vec<String> {
    split_cjk_aware_with_indexed_separators(text, &[])
}

/// Tokenize text by splitting on CJK character boundaries, preserving alphanumeric sequences, and treating indexed separators as individual tokens.
///
/// # Arguments
/// - text: The string to tokenize
/// - indexed_separators: Characters to treat as separate tokens (e.g., punctuation)
fn split_cjk_aware_with_indexed_separators(text: &str, indexed_separators: &[char]) -> Vec<String> {
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
        } else if indexed_separators.contains(&c) {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            tokens.push(c.to_string());
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

mod building;
type QueryClause = (tantivy::query::Occur, Box<dyn TantivyQuery>);

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

/// Parse search queries with configurable prefix/exact matching, fuzzy correction, advanced syntax (quoted phrases and word exclusion), per-field weighting, and morphological stemming.
pub struct QueryParser {
    fields: Vec<tantivy::schema::Field>,
    json_exact_field: Option<tantivy::schema::Field>,
    weights: Vec<f32>,
    searchable_paths: Vec<String>,
    indexed_separators: Vec<char>,
    keep_diacritics_on_characters: String,
    custom_normalization: Vec<(char, String)>,
    query_type: String,
    plural_map: Option<std::collections::HashMap<String, Vec<String>>>,
    disabled_typo_words: std::collections::HashSet<String>,
    disabled_typo_attrs: std::collections::HashSet<String>,
    typo_tolerance: bool,
    min_word_size_for_1_typo: usize,
    min_word_size_for_2_typos: usize,
    all_optional: bool,
    advanced_syntax: bool,
    /// Controls which advanced syntax features are enabled when `advanced_syntax` is true.
    /// None means both features enabled (backward compatible default).
    /// Some(set) enables only the listed features: "exactPhrase" and/or "excludeWords".
    advanced_syntax_features: Option<Vec<String>>,
    /// Stemmer language for query-side morphological normalization.
    /// When set, exact (non-prefix) query tokens are stemmed before index lookup,
    /// matching the stemmed forms stored at index time by the `simple` tokenizer.
    stemmer_language: Option<tantivy::tokenizer::Language>,
}

#[derive(Debug, Clone)]
pub struct ShortQueryMarker {
    pub token: String,
    pub paths: Vec<String>,
    pub weights: Vec<f32>,
    pub field: tantivy::schema::Field,
}

struct FieldQueryContext<'a> {
    token: &'a str,
    token_lc: &'a str,
    path: &'a str,
    path_idx: usize,
    is_prefix: bool,
    target_field: tantivy::schema::Field,
    json_search_field: tantivy::schema::Field,
    max_fuzzy_paths: usize,
    plural_forms: &'a [String],
}

impl QueryParser {
    /// Create a parser with equal weights across all default fields, prefix-last
    /// query type, and default typo tolerance thresholds (1 typo at 4 chars, 2 at 8).
    pub fn new(_schema: &TantivySchema, default_fields: Vec<tantivy::schema::Field>) -> Self {
        let weights = vec![1.0; default_fields.len()];
        QueryParser {
            fields: default_fields,
            json_exact_field: None,
            weights,
            searchable_paths: vec![],
            query_type: "prefixLast".to_string(),
            indexed_separators: Vec::new(),
            keep_diacritics_on_characters: String::new(),
            custom_normalization: Vec::new(),
            plural_map: None,
            disabled_typo_words: std::collections::HashSet::new(),
            disabled_typo_attrs: std::collections::HashSet::new(),
            typo_tolerance: true,
            min_word_size_for_1_typo: 4,
            min_word_size_for_2_typos: 8,
            all_optional: false,
            advanced_syntax: false,
            advanced_syntax_features: None,
            stemmer_language: None,
        }
    }

    /// Create a parser with explicit per-field weights and searchable attribute paths.
    /// Panics if weights and searchable_paths have different lengths.
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
            indexed_separators: Vec::new(),
            keep_diacritics_on_characters: String::new(),
            custom_normalization: Vec::new(),
            plural_map: None,
            disabled_typo_words: std::collections::HashSet::new(),
            disabled_typo_attrs: std::collections::HashSet::new(),
            typo_tolerance: true,
            min_word_size_for_1_typo: 4,
            min_word_size_for_2_typos: 8,
            all_optional: false,
            advanced_syntax: false,
            advanced_syntax_features: None,
            stemmer_language: None,
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

    pub fn with_typo_tolerance(mut self, enabled: bool) -> Self {
        self.typo_tolerance = enabled;
        self
    }

    pub fn with_indexed_separators(mut self, separators: Vec<char>) -> Self {
        self.indexed_separators = separators;
        self
    }

    pub fn with_keep_diacritics_on_characters(
        mut self,
        keep_diacritics_on_characters: &str,
    ) -> Self {
        self.keep_diacritics_on_characters = keep_diacritics_on_characters.to_string();
        self
    }

    pub fn with_custom_normalization(mut self, custom_normalization: Vec<(char, String)>) -> Self {
        self.custom_normalization = custom_normalization;
        self
    }

    pub fn with_disabled_typo_words(mut self, words: Vec<String>) -> Self {
        self.disabled_typo_words = words.into_iter().map(|word| word.to_lowercase()).collect();
        self
    }

    pub fn with_disabled_typo_attrs(mut self, attrs: Vec<String>) -> Self {
        self.disabled_typo_attrs = attrs.into_iter().collect();
        self
    }

    pub fn with_min_word_size_for_1_typo(mut self, size: usize) -> Self {
        self.min_word_size_for_1_typo = size;
        self
    }

    pub fn with_min_word_size_for_2_typos(mut self, size: usize) -> Self {
        self.min_word_size_for_2_typos = size;
        self
    }

    pub fn with_advanced_syntax(mut self, enabled: bool) -> Self {
        self.advanced_syntax = enabled;
        self
    }

    pub fn with_all_optional(mut self, enabled: bool) -> Self {
        self.all_optional = enabled;
        self
    }

    pub fn with_advanced_syntax_features(mut self, features: Vec<String>) -> Self {
        self.advanced_syntax_features = Some(features);
        self
    }

    /// Check if a specific advanced syntax feature is enabled.
    /// When `advanced_syntax_features` is None, all features are enabled (default).
    fn is_advanced_feature_enabled(&self, feature: &str) -> bool {
        match &self.advanced_syntax_features {
            None => true, // default: all enabled
            Some(features) => features.iter().any(|f| f == feature),
        }
    }

    pub fn with_plural_map(
        mut self,
        plural_map: Option<std::collections::HashMap<String, Vec<String>>>,
    ) -> Self {
        self.plural_map = plural_map;
        self
    }

    pub fn with_stemmer_language(mut self, language: Option<tantivy::tokenizer::Language>) -> Self {
        self.stemmer_language = language;
        self
    }

    /// Apply the configured stemmer to a single already-lowercased token.
    /// Returns the stemmed form, or the original token if no stemmer is set.
    fn stem_token<'a>(&self, token: &'a str) -> std::borrow::Cow<'a, str> {
        let Some(lang) = self.stemmer_language else {
            return std::borrow::Cow::Borrowed(token);
        };
        let mut analyzer =
            tantivy::tokenizer::TextAnalyzer::builder(tantivy::tokenizer::RawTokenizer::default())
                .filter(tantivy::tokenizer::LowerCaser)
                .filter(tantivy::tokenizer::Stemmer::new(lang))
                .build();
        let mut stream = analyzer.token_stream(token);
        if stream.advance() {
            std::borrow::Cow::Owned(stream.token().text.clone())
        } else {
            std::borrow::Cow::Borrowed(token)
        }
    }

    /// Parse a query into a Tantivy query tree: normalize text, try advanced syntax,
    /// then dispatch to short-query or multi-token builder based on token count.
    pub fn parse(&self, query: &Query) -> Result<Box<dyn TantivyQuery>> {
        let normalized_query_text = normalize_for_search(
            &query.text,
            &self.keep_diacritics_on_characters,
            &self.custom_normalization,
        );
        if let Some(advanced_query) = self.try_parse_advanced_syntax(&normalized_query_text) {
            return advanced_query;
        }

        let has_trailing_space = normalized_query_text.ends_with(' ');
        let text = normalized_query_text.trim_end_matches('*').to_string();
        let tokens = split_cjk_aware_with_indexed_separators(&text, &self.indexed_separators);

        tracing::trace!(
            "[PARSER] parse() called: query='{}', tokens={:?}, searchable_paths={:?}",
            normalized_query_text,
            tokens,
            self.searchable_paths
        );

        if tokens.is_empty() {
            return Ok(Box::new(tantivy::query::AllQuery));
        }

        if let Some(short_query) = self.try_parse_short_query(&tokens, has_trailing_space) {
            return Ok(short_query);
        }

        Ok(self.build_multi_token_query(
            &normalized_query_text,
            &query.text,
            &tokens,
            has_trailing_space,
        ))
    }
}

#[cfg(test)]
mod tests;
