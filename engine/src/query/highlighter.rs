use crate::types::{Document, FieldValue};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightResult {
    pub value: String,
    pub match_level: MatchLevel,
    pub matched_words: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fully_highlighted: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchLevel {
    None,
    Partial,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HighlightValue {
    Single(HighlightResult),
    Array(Vec<HighlightResult>),
    Object(HashMap<String, HighlightValue>),
}

pub struct Highlighter {
    pre_tag: String,
    post_tag: String,
    snippet_ellipsis_text: String,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self {
            pre_tag: "<em>".to_string(),
            post_tag: "</em>".to_string(),
            snippet_ellipsis_text: "\u{2026}".to_string(),
        }
    }
}

fn lowercased_query_words(query_words: &[String]) -> Vec<String> {
    query_words.iter().map(|word| word.to_lowercase()).collect()
}

fn all_query_words_found(query_words: &[String], matched_words: &[String]) -> bool {
    let unique_matched: HashSet<&str> = matched_words.iter().map(|word| word.as_str()).collect();
    query_words
        .iter()
        .all(|query_word| unique_matched.contains(query_word.as_str()))
}

/// Find query words that appear as exact substrings in the lowercased text
/// and record their byte positions for highlighting.
fn collect_exact_matches(
    text_lower: &str,
    query_words: &[String],
    query_words_lower: &[String],
    matched_words: &mut Vec<String>,
    match_positions: &mut Vec<(usize, usize)>,
) {
    for (query_index, word_lower) in query_words_lower.iter().enumerate() {
        let mut search_start = 0;
        while let Some(relative_pos) = text_lower[search_start..].find(word_lower.as_str()) {
            let absolute_pos = search_start + relative_pos;
            matched_words.push(query_words[query_index].clone());
            match_positions.push((absolute_pos, absolute_pos + word_lower.len()));
            search_start = absolute_pos + word_lower.len();
        }
    }
}

/// Find matches where a single query word is split across adjacent text words
/// (e.g. query "notebook" matches text "note book"), recording positions.
fn collect_split_matches(
    text_lower: &str,
    query_words: &[String],
    query_words_lower: &[String],
    matched_words: &mut Vec<String>,
    match_positions: &mut Vec<(usize, usize)>,
) {
    for (query_index, word_lower) in query_words_lower.iter().enumerate() {
        let chars: Vec<char> = word_lower.chars().collect();
        if chars.len() < 4 {
            continue;
        }

        for split_pos in 2..chars.len().saturating_sub(1) {
            let first_part: String = chars[..split_pos].iter().collect();
            let second_part: String = chars[split_pos..].iter().collect();
            if second_part.len() < 2 {
                continue;
            }

            let split_form = format!("{} {}", first_part, second_part);
            let mut search_start = 0;
            while let Some(relative_pos) = text_lower[search_start..].find(&split_form) {
                let absolute_pos = search_start + relative_pos;
                matched_words.push(query_words[query_index].clone());
                match_positions.push((absolute_pos, absolute_pos + split_form.len()));
                search_start = absolute_pos + split_form.len();
            }
        }
    }
}

/// Find matches where adjacent query words concatenate inside the text
/// (e.g. query "note book" matches text "notebook"), recording positions.
fn collect_concat_matches(
    text_lower: &str,
    query_words: &[String],
    query_words_lower: &[String],
    matched_words: &mut Vec<String>,
    match_positions: &mut Vec<(usize, usize)>,
) {
    if query_words_lower.len() < 2 {
        return;
    }

    for index in 0..query_words_lower.len() - 1 {
        let concat = format!(
            "{}{}",
            query_words_lower[index],
            query_words_lower[index + 1]
        );
        let mut search_start = 0;
        while let Some(relative_pos) = text_lower[search_start..].find(&concat) {
            let absolute_pos = search_start + relative_pos;
            matched_words.push(query_words[index].clone());
            matched_words.push(query_words[index + 1].clone());
            match_positions.push((absolute_pos, absolute_pos + concat.len()));
            search_start = absolute_pos + concat.len();
        }
    }
}

/// Split text into (byte_offset, word) pairs on non-alphanumeric boundaries.
fn split_text_words(text: &str) -> Vec<(usize, &str)> {
    let mut words = Vec::new();
    let mut current_start = 0;
    for (index, ch) in text.char_indices() {
        if !ch.is_alphanumeric() {
            if current_start < index {
                words.push((current_start, &text[current_start..index]));
            }
            current_start = index + ch.len_utf8();
        }
    }
    if current_start < text.len() {
        words.push((current_start, &text[current_start..]));
    }
    words
}

fn max_fuzzy_distance(query_len: usize) -> usize {
    if query_len >= 8 {
        2
    } else {
        1
    }
}

fn highlight_end_for_char_count(text_word: &str, word_start: usize, char_count: usize) -> usize {
    text_word
        .char_indices()
        .nth(char_count)
        .map(|(offset, _)| word_start + offset)
        .unwrap_or(word_start + text_word.len())
}

/// Find query words that fuzzy-match text words within Levenshtein distance 1-2
/// (scaled by word length), recording their positions for highlighting.
fn collect_fuzzy_matches(
    text: &str,
    query_words: &[String],
    query_words_lower: &[String],
    matched_words: &mut Vec<String>,
    match_positions: &mut Vec<(usize, usize)>,
) {
    let text_words = split_text_words(text);
    for (word_start, text_word) in text_words {
        let text_word_lower = text_word.to_lowercase();
        let text_word_chars = text_word_lower.chars().count();

        for (query_index, query_lower) in query_words_lower.iter().enumerate() {
            let query_chars = query_lower.chars().count();
            if query_chars < 4 || text_word_chars < 4 {
                continue;
            }

            let max_distance = max_fuzzy_distance(query_chars);
            let whole_word_distance = strsim::damerau_levenshtein(query_lower, &text_word_lower);
            if whole_word_distance <= max_distance && whole_word_distance > 0 {
                matched_words.push(query_words[query_index].clone());
                let highlight_len = query_chars.min(text_word.len());
                match_positions.push((word_start, word_start + highlight_len));
                continue;
            }

            if text_word_chars > query_chars {
                let prefix: String = text_word_lower.chars().take(query_chars).collect();
                let prefix_distance = strsim::damerau_levenshtein(query_lower, &prefix);
                if prefix_distance <= max_distance {
                    matched_words.push(query_words[query_index].clone());
                    let highlight_end =
                        highlight_end_for_char_count(text_word, word_start, query_chars);
                    match_positions.push((word_start, highlight_end));
                }
            }

            let query_suffix: String = query_lower.chars().skip(1).collect();
            let suffix_len = query_suffix.chars().count();
            if suffix_len < 3 || text_word_chars < suffix_len {
                continue;
            }

            let text_prefix: String = text_word_lower.chars().take(suffix_len).collect();
            let suffix_distance = strsim::damerau_levenshtein(&query_suffix, &text_prefix);
            if suffix_distance <= 1 {
                matched_words.push(query_words[query_index].clone());
                let highlight_end = highlight_end_for_char_count(text_word, word_start, suffix_len);
                match_positions.push((word_start, highlight_end));
            }
        }
    }
}

impl Highlighter {
    pub fn new(pre_tag: String, post_tag: String) -> Self {
        Self {
            pre_tag,
            post_tag,
            snippet_ellipsis_text: "\u{2026}".to_string(),
        }
    }

    pub fn with_snippet_ellipsis(mut self, text: String) -> Self {
        self.snippet_ellipsis_text = text;
        self
    }

    /// Highlight all highlightable fields in a document, returning a map of
    /// field paths to their highlighted values.
    pub fn highlight_document(
        &self,
        doc: &Document,
        query_words: &[String],
    ) -> HashMap<String, HighlightValue> {
        let mut result = HashMap::new();

        for (field_name, field_value) in &doc.fields {
            if field_name == "objectID" {
                continue;
            }
            // Algolia highlights ALL attributes with query words, not just
            // searchable ones.  The searchableAttributes setting only controls
            // which fields the *search* engine queries, not highlighting.
            result.insert(
                field_name.clone(),
                self.highlight_field_value(field_value, query_words, field_name),
            );
        }

        result
    }

    /// Recursively highlight a field value: descend into objects/arrays,
    /// apply text highlighting to string leaves.
    fn highlight_field_value(
        &self,
        value: &FieldValue,
        query_words: &[String],
        field_path: &str,
    ) -> HighlightValue {
        match value {
            FieldValue::Text(s) => HighlightValue::Single(self.highlight_text(s, query_words)),
            FieldValue::Array(items) => {
                let results: Vec<HighlightResult> = items
                    .iter()
                    .map(|item| match item {
                        FieldValue::Text(s) => self.highlight_text(s, query_words),
                        _ => self.no_match(self.field_value_to_string(item)),
                    })
                    .collect();
                HighlightValue::Array(results)
            }
            FieldValue::Object(map) => {
                let mut obj_result = HashMap::new();
                for (k, v) in map {
                    let nested_path = format!("{}.{}", field_path, k);
                    obj_result.insert(
                        k.clone(),
                        self.highlight_field_value(v, query_words, &nested_path),
                    );
                }
                HighlightValue::Object(obj_result)
            }
            _ => HighlightValue::Single(self.no_match(self.field_value_to_string(value))),
        }
    }

    /// Highlight a text string by collecting exact, split, concat, and fuzzy
    /// matches, then wrapping matched spans in `<em>` tags.
    pub fn highlight_text(&self, text: &str, query_words: &[String]) -> HighlightResult {
        let text_lower = text.to_lowercase();
        let mut matched_words = Vec::new();
        let mut match_positions = Vec::new();

        let query_words_lower = lowercased_query_words(query_words);
        collect_exact_matches(
            &text_lower,
            query_words,
            &query_words_lower,
            &mut matched_words,
            &mut match_positions,
        );

        if !all_query_words_found(query_words, &matched_words) {
            collect_split_matches(
                &text_lower,
                query_words,
                &query_words_lower,
                &mut matched_words,
                &mut match_positions,
            );
            collect_concat_matches(
                &text_lower,
                query_words,
                &query_words_lower,
                &mut matched_words,
                &mut match_positions,
            );
            collect_fuzzy_matches(
                text,
                query_words,
                &query_words_lower,
                &mut matched_words,
                &mut match_positions,
            );
        }

        if matched_words.is_empty() {
            return self.no_match(text.to_string());
        }

        // Merge overlapping/adjacent positions into single spans
        match_positions.sort_by_key(|(start, _)| *start);
        match_positions.dedup();
        let match_positions = Self::merge_positions(match_positions);

        let highlighted = self.apply_highlights(text, &match_positions);

        let unique_matched: HashSet<_> = matched_words.iter().collect();
        let match_level = if unique_matched.len() == query_words.len() {
            MatchLevel::Full
        } else {
            MatchLevel::Partial
        };

        let total_match_len: usize = match_positions.iter().map(|(s, e)| e - s).sum();
        let fully_highlighted = Some(total_match_len >= text.len());

        matched_words.sort();
        matched_words.dedup();

        HighlightResult {
            value: highlighted,
            match_level,
            matched_words,
            fully_highlighted,
        }
    }

    /// Merge overlapping or adjacent positions into single spans.
    fn merge_positions(positions: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
        if positions.is_empty() {
            return positions;
        }
        let mut merged: Vec<(usize, usize)> = Vec::new();
        let mut current = positions[0];
        for &(start, end) in &positions[1..] {
            if start <= current.1 {
                current.1 = current.1.max(end);
            } else {
                merged.push(current);
                current = (start, end);
            }
        }
        merged.push(current);
        merged
    }

    /// Insert `<em>`/`</em>` tags around the given byte-position ranges in the text,
    /// merging overlapping ranges and preserving surrounding content.
    fn apply_highlights(&self, text: &str, positions: &[(usize, usize)]) -> String {
        if positions.is_empty() {
            return text.to_string();
        }

        let mut result = String::new();
        let mut last_end = 0;

        for &(start, end) in positions {
            if start < last_end {
                continue;
            }

            result.push_str(&text[last_end..start]);
            result.push_str(&self.pre_tag);
            result.push_str(&text[start..end]);
            result.push_str(&self.post_tag);
            last_end = end;
        }

        result.push_str(&text[last_end..]);
        result
    }

    /// Generate a snippet for a document — truncated text around matches.
    pub fn snippet_document(
        &self,
        doc: &Document,
        query_words: &[String],
        snippet_specs: &[(&str, usize)],
    ) -> HashMap<String, SnippetValue> {
        let mut result = HashMap::new();
        for (attr, word_count) in snippet_specs {
            if *attr == "*" {
                // Snippet all text fields
                for (field_name, field_value) in &doc.fields {
                    if field_name == "objectID" {
                        continue;
                    }
                    result.insert(
                        field_name.clone(),
                        self.snippet_field_value(field_value, query_words, *word_count),
                    );
                }
            } else if let Some(field_value) = doc.fields.get(*attr) {
                result.insert(
                    attr.to_string(),
                    self.snippet_field_value(field_value, query_words, *word_count),
                );
            }
        }
        result
    }

    /// Extract a snippet (truncated highlight) from a field value, limiting to
    /// `word_count` words around the best match region.
    fn snippet_field_value(
        &self,
        value: &FieldValue,
        query_words: &[String],
        word_count: usize,
    ) -> SnippetValue {
        match value {
            FieldValue::Text(s) => {
                SnippetValue::Single(self.snippet_text(s, query_words, word_count))
            }
            FieldValue::Array(items) => {
                let results: Vec<SnippetResult> = items
                    .iter()
                    .map(|item| match item {
                        FieldValue::Text(s) => self.snippet_text(s, query_words, word_count),
                        _ => SnippetResult {
                            value: self.field_value_to_string(item),
                            match_level: MatchLevel::None,
                        },
                    })
                    .collect();
                SnippetValue::Array(results)
            }
            FieldValue::Object(map) => {
                let mut obj_result = HashMap::new();
                for (k, v) in map {
                    obj_result.insert(
                        k.clone(),
                        self.snippet_field_value(v, query_words, word_count),
                    );
                }
                SnippetValue::Object(obj_result)
            }
            _ => SnippetValue::Single(SnippetResult {
                value: self.field_value_to_string(value),
                match_level: MatchLevel::None,
            }),
        }
    }

    /// Generate a snippet by highlighting the text, then windowing around the first
    /// match to return at most `word_count` words with ellipsis markers.
    fn snippet_text(&self, text: &str, query_words: &[String], word_count: usize) -> SnippetResult {
        // First, get the full highlight result to find match positions
        let highlight = self.highlight_text(text, query_words);

        // If no match or text is short enough, return as-is (with highlight tags)
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.len() <= word_count {
            return SnippetResult {
                value: highlight.value,
                match_level: highlight.match_level,
            };
        }

        if matches!(highlight.match_level, MatchLevel::None) {
            // No match — take first N words and add ellipsis
            let truncated: String = words[..word_count.min(words.len())].join(" ");
            return SnippetResult {
                value: format!("{}{}", truncated, self.snippet_ellipsis_text),
                match_level: MatchLevel::None,
            };
        }

        // Find the word index where the first match occurs
        let text_lower = text.to_lowercase();
        let query_words_lower: Vec<String> = query_words.iter().map(|w| w.to_lowercase()).collect();

        let first_match_byte = query_words_lower
            .iter()
            .filter_map(|qw| text_lower.find(qw.as_str()))
            .min()
            .unwrap_or(0);

        // Find which word index corresponds to this byte offset
        let mut match_word_idx = 0;
        let mut byte_pos = 0;
        for (i, word) in words.iter().enumerate() {
            if let Some(pos) = text[byte_pos..].find(word) {
                let word_start = byte_pos + pos;
                if word_start + word.len() > first_match_byte {
                    match_word_idx = i;
                    break;
                }
                byte_pos = word_start + word.len();
            }
        }

        // Center the window around the match
        let half = word_count / 2;
        let start = match_word_idx.saturating_sub(half);
        let end = (start + word_count).min(words.len());
        let start = if end == words.len() && end > word_count {
            end - word_count
        } else {
            start
        };

        // Extract the snippet window and highlight it
        let snippet_words: Vec<&str> = words[start..end].to_vec();
        let snippet_text = snippet_words.join(" ");
        let snippet_highlight = self.highlight_text(&snippet_text, query_words);

        let mut value = String::new();
        if start > 0 {
            value.push_str(&self.snippet_ellipsis_text);
        }
        value.push_str(&snippet_highlight.value);
        if end < words.len() {
            value.push_str(&self.snippet_ellipsis_text);
        }

        SnippetResult {
            value,
            match_level: snippet_highlight.match_level,
        }
    }

    fn no_match(&self, value: String) -> HighlightResult {
        HighlightResult {
            value,
            match_level: MatchLevel::None,
            matched_words: Vec::new(),
            fully_highlighted: None,
        }
    }

    fn field_value_to_string(&self, value: &FieldValue) -> String {
        match value {
            FieldValue::Bool(b) => b.to_string(),
            FieldValue::Text(s) => s.clone(),
            FieldValue::Integer(i) => i.to_string(),
            FieldValue::Float(f) => f.to_string(),
            FieldValue::Date(d) => d.to_string(),
            FieldValue::Facet(s) => s.clone(),
            FieldValue::Array(_) => "[]".to_string(),
            FieldValue::Object(_) => "{}".to_string(),
        }
    }
}

/// A snippet result — same shape as HighlightResult but `value` is truncated text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetResult {
    pub value: String,
    pub match_level: MatchLevel,
}

/// Recursive snippet value (mirrors HighlightValue).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SnippetValue {
    Single(SnippetResult),
    Array(Vec<SnippetResult>),
    Object(HashMap<String, SnippetValue>),
}

/// Parse "attribute:N" snippet spec. Returns (attribute_name, word_count).
pub fn parse_snippet_spec(spec: &str) -> (&str, usize) {
    if let Some(colon) = spec.rfind(':') {
        let attr = &spec[..colon];
        let count = spec[colon + 1..].parse::<usize>().unwrap_or(10);
        (attr, count)
    } else {
        (spec, 10)
    }
}

pub fn extract_query_words(query_text: &str) -> Vec<String> {
    query_text
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h() -> Highlighter {
        Highlighter::default()
    }

    // --- extract_query_words ---

    #[test]
    fn qw_basic() {
        assert_eq!(extract_query_words("hello world"), vec!["hello", "world"]);
    }

    #[test]
    fn qw_lowercases() {
        assert_eq!(extract_query_words("Hello WORLD"), vec!["hello", "world"]);
    }

    #[test]
    fn qw_empty() {
        let r: Vec<String> = extract_query_words("");
        assert!(r.is_empty());
    }

    #[test]
    fn qw_extra_spaces() {
        assert_eq!(
            extract_query_words("  hello   world  "),
            vec!["hello", "world"]
        );
    }

    // --- parse_snippet_spec ---

    #[test]
    fn snippet_spec_with_count() {
        assert_eq!(parse_snippet_spec("title:5"), ("title", 5));
    }

    #[test]
    fn snippet_spec_no_colon() {
        assert_eq!(parse_snippet_spec("title"), ("title", 10));
    }

    #[test]
    fn snippet_spec_star() {
        assert_eq!(parse_snippet_spec("*:3"), ("*", 3));
    }

    #[test]
    fn snippet_spec_invalid_count_defaults_10() {
        assert_eq!(parse_snippet_spec("title:abc"), ("title", 10));
    }

    #[test]
    fn all_query_words_found_requires_every_query_word() {
        let query_words = vec!["hello".to_string(), "world".to_string()];
        assert!(all_query_words_found(
            &query_words,
            &[
                "hello".to_string(),
                "world".to_string(),
                "hello".to_string()
            ],
        ));
        assert!(!all_query_words_found(&query_words, &["hello".to_string()],));
    }

    // --- merge_positions ---

    #[test]
    fn merge_empty() {
        assert_eq!(Highlighter::merge_positions(vec![]), vec![]);
    }

    #[test]
    fn merge_single() {
        assert_eq!(Highlighter::merge_positions(vec![(0, 5)]), vec![(0, 5)]);
    }

    #[test]
    fn merge_non_overlapping() {
        let r = Highlighter::merge_positions(vec![(0, 3), (5, 8)]);
        assert_eq!(r, vec![(0, 3), (5, 8)]);
    }

    #[test]
    fn merge_overlapping() {
        let r = Highlighter::merge_positions(vec![(0, 5), (3, 8)]);
        assert_eq!(r, vec![(0, 8)]);
    }

    #[test]
    fn merge_adjacent_fuses() {
        // start == end of previous → fused (start <= current.1)
        let r = Highlighter::merge_positions(vec![(0, 5), (5, 8)]);
        assert_eq!(r, vec![(0, 8)]);
    }

    #[test]
    fn merge_three_into_one() {
        let r = Highlighter::merge_positions(vec![(0, 4), (2, 6), (5, 9)]);
        assert_eq!(r, vec![(0, 9)]);
    }

    // --- apply_highlights ---

    #[test]
    fn apply_prefix() {
        assert_eq!(
            h().apply_highlights("hello world", &[(0, 5)]),
            "<em>hello</em> world"
        );
    }

    #[test]
    fn apply_suffix() {
        assert_eq!(
            h().apply_highlights("hello world", &[(6, 11)]),
            "hello <em>world</em>"
        );
    }

    #[test]
    fn apply_middle() {
        assert_eq!(
            h().apply_highlights("hello world foo", &[(6, 11)]),
            "hello <em>world</em> foo"
        );
    }

    #[test]
    fn apply_multiple_spans() {
        let r = h().apply_highlights("hello world", &[(0, 5), (6, 11)]);
        assert_eq!(r, "<em>hello</em> <em>world</em>");
    }

    #[test]
    fn apply_empty_positions() {
        assert_eq!(h().apply_highlights("hello", &[]), "hello");
    }

    #[test]
    fn apply_custom_tags() {
        let h = Highlighter::new("<b>".to_string(), "</b>".to_string());
        assert_eq!(
            h.apply_highlights("hello world", &[(0, 5)]),
            "<b>hello</b> world"
        );
    }

    // --- highlight_text: exact matching ---

    #[test]
    fn hl_no_match() {
        let r = h().highlight_text("hello world", &["xyz".to_string()]);
        assert!(matches!(r.match_level, MatchLevel::None));
        assert!(r.matched_words.is_empty());
        assert_eq!(r.value, "hello world");
        assert!(r.fully_highlighted.is_none());
    }

    #[test]
    fn hl_single_word_full_match() {
        let r = h().highlight_text("hello world", &["hello".to_string()]);
        assert!(matches!(r.match_level, MatchLevel::Full));
        assert_eq!(r.matched_words, vec!["hello"]);
        assert_eq!(r.value, "<em>hello</em> world");
    }

    #[test]
    fn hl_two_words_full_match() {
        let r = h().highlight_text("hello world", &["hello".to_string(), "world".to_string()]);
        assert!(matches!(r.match_level, MatchLevel::Full));
        assert_eq!(r.value, "<em>hello</em> <em>world</em>");
    }

    #[test]
    fn hl_partial_match() {
        let r = h().highlight_text("hello world", &["hello".to_string(), "xyz".to_string()]);
        assert!(matches!(r.match_level, MatchLevel::Partial));
        assert_eq!(r.matched_words, vec!["hello"]);
    }

    #[test]
    fn hl_case_insensitive() {
        let r = h().highlight_text("Hello World", &["hello".to_string()]);
        assert!(matches!(r.match_level, MatchLevel::Full));
        // Original casing preserved in output
        assert_eq!(r.value, "<em>Hello</em> World");
    }

    #[test]
    fn hl_multiple_occurrences() {
        let r = h().highlight_text("cat and cat", &["cat".to_string()]);
        assert!(matches!(r.match_level, MatchLevel::Full));
        assert_eq!(r.value, "<em>cat</em> and <em>cat</em>");
    }

    #[test]
    fn hl_fully_highlighted_true() {
        let r = h().highlight_text("cat", &["cat".to_string()]);
        assert_eq!(r.fully_highlighted, Some(true));
    }

    #[test]
    fn hl_fully_highlighted_false() {
        let r = h().highlight_text("cat and dog", &["cat".to_string()]);
        assert_eq!(r.fully_highlighted, Some(false));
    }

    #[test]
    fn hl_matched_words_deduped_and_sorted() {
        // "cat" appears twice → matched_words should have it once
        let r = h().highlight_text("cat cat", &["cat".to_string()]);
        assert_eq!(r.matched_words, vec!["cat"]);
    }

    // --- highlight_text: split matching ---

    #[test]
    fn hl_split_match() {
        // query "hotdog" → split to "hot dog" → matches text "hot dog collar"
        let r = h().highlight_text("hot dog collar", &["hotdog".to_string()]);
        assert!(matches!(r.match_level, MatchLevel::Full));
        assert!(r.value.starts_with("<em>hot dog</em>"));
    }

    // --- highlight_text: concat matching ---

    #[test]
    fn hl_concat_match() {
        // query ["ear", "buds"] → concat "earbuds" → matches text "wireless earbuds sale"
        let r = h().highlight_text(
            "wireless earbuds sale",
            &["ear".to_string(), "buds".to_string()],
        );
        assert!(matches!(r.match_level, MatchLevel::Full));
        assert!(r.value.contains("<em>earbuds</em>"));
    }

    // --- highlight_text: fuzzy matching ---

    #[test]
    fn hl_fuzzy_transposition() {
        // "laptpo" is 1 transposition away from "laptop"
        let r = h().highlight_text("laptop sale", &["laptpo".to_string()]);
        assert!(matches!(r.match_level, MatchLevel::Full));
        assert!(r.value.contains("<em>laptop</em>"));
    }

    #[test]
    fn hl_fuzzy_no_match_short_word() {
        // "cat" is 3 chars — below the fuzzy threshold of 4, so "cot" should NOT fuzzy-match
        let r = h().highlight_text("cot sale", &["cat".to_string()]);
        assert!(matches!(r.match_level, MatchLevel::None));
    }

    // --- highlight_document ---

    /// Verify that highlight_document includes all non-objectID fields in output.
    ///
    /// Constructs a document with title and brand fields, calls highlight_document, and asserts both fields are present in results while objectID is excluded.
    #[test]
    fn hl_document_all_fields_included() {
        use crate::types::{Document, FieldValue};
        use std::collections::HashMap;
        let mut fields = HashMap::new();
        fields.insert(
            "title".to_string(),
            FieldValue::Text("Gaming Laptop".to_string()),
        );
        fields.insert("brand".to_string(), FieldValue::Text("Dell".to_string()));
        let doc = Document {
            id: "1".to_string(),
            fields,
        };
        let result = h().highlight_document(&doc, &["gaming".to_string()]);
        assert!(result.contains_key("title"));
        assert!(result.contains_key("brand"));
        assert!(!result.contains_key("objectID"));
    }

    /// Verify that highlight_document correctly handles array fields.
    ///
    /// Constructs a document with a tags array field, calls highlight_document, and asserts the result contains HighlightValue::Array for the tags field.
    #[test]
    fn hl_document_array_field() {
        use crate::types::{Document, FieldValue};
        use std::collections::HashMap;
        let mut fields = HashMap::new();
        fields.insert(
            "tags".to_string(),
            FieldValue::Array(vec![
                FieldValue::Text("laptop".to_string()),
                FieldValue::Text("gaming".to_string()),
            ]),
        );
        let doc = Document {
            id: "1".to_string(),
            fields,
        };
        let result = h().highlight_document(&doc, &["laptop".to_string()]);
        assert!(matches!(result.get("tags"), Some(HighlightValue::Array(_))));
    }

    #[test]
    fn hl_document_bool_field_uses_boolean_string() {
        use crate::types::{Document, FieldValue};
        use std::collections::HashMap;

        let mut fields = HashMap::new();
        fields.insert("featured".to_string(), FieldValue::Bool(true));
        let doc = Document {
            id: "1".to_string(),
            fields,
        };

        let result = h().highlight_document(&doc, &["false".to_string()]);
        let highlight = match result.get("featured") {
            Some(HighlightValue::Single(highlight)) => highlight,
            other => panic!("expected single highlight for bool field, got {other:?}"),
        };

        assert_eq!(highlight.value, "true");
        assert!(matches!(highlight.match_level, MatchLevel::None));
    }
}
