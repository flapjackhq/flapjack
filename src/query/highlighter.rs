use crate::types::{Document, FieldValue};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
}

impl Default for Highlighter {
    fn default() -> Self {
        Self {
            pre_tag: "<em>".to_string(),
            post_tag: "</em>".to_string(),
        }
    }
}

impl Highlighter {
    pub fn new(pre_tag: String, post_tag: String) -> Self {
        Self { pre_tag, post_tag }
    }

    pub fn highlight_document(
        &self,
        doc: &Document,
        query_words: &[String],
        searchable_paths: &[String],
    ) -> HashMap<String, HighlightValue> {
        let mut result = HashMap::new();

        for (field_name, field_value) in &doc.fields {
            if field_name == "objectID" {
                continue;
            }
            let is_searchable = searchable_paths.is_empty()
                || searchable_paths
                    .iter()
                    .any(|p| p == field_name || field_name.starts_with(&format!("{}.", p)));
            if is_searchable {
                result.insert(
                    field_name.clone(),
                    self.highlight_field_value(
                        field_value,
                        query_words,
                        field_name,
                        searchable_paths,
                    ),
                );
            }
        }

        result
    }

    fn highlight_field_value(
        &self,
        value: &FieldValue,
        query_words: &[String],
        field_path: &str,
        searchable_paths: &[String],
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
                        self.highlight_field_value(v, query_words, &nested_path, searchable_paths),
                    );
                }
                HighlightValue::Object(obj_result)
            }
            _ => HighlightValue::Single(self.no_match(self.field_value_to_string(value))),
        }
    }

    fn highlight_text(&self, text: &str, query_words: &[String]) -> HighlightResult {
        let text_lower = text.to_lowercase();
        let mut matched_words = Vec::new();
        let mut match_positions = Vec::new();

        // 1. Exact substring matching for each query word
        for word in query_words {
            let word_lower = word.to_lowercase();
            let mut start = 0;

            while let Some(pos) = text_lower[start..].find(&word_lower) {
                let absolute_pos = start + pos;
                matched_words.push(word.clone());
                match_positions.push((absolute_pos, absolute_pos + word.len()));
                start = absolute_pos + word.len();
            }
        }

        // 2. Split matching: for each query word >= 4 chars, try inserting a space
        //    at each position to match split forms (e.g., "hotdog" -> "hot dog")
        for word in query_words {
            let word_lower = word.to_lowercase();
            let chars: Vec<char> = word_lower.chars().collect();
            if chars.len() < 4 {
                continue;
            }
            for split_pos in 2..chars.len().saturating_sub(1) {
                let first: String = chars[..split_pos].iter().collect();
                let second: String = chars[split_pos..].iter().collect();
                if second.len() < 2 {
                    continue;
                }
                let split_form = format!("{} {}", first, second);
                let mut start = 0;
                while let Some(pos) = text_lower[start..].find(&split_form) {
                    let absolute_pos = start + pos;
                    matched_words.push(word.clone());
                    match_positions.push((absolute_pos, absolute_pos + split_form.len()));
                    start = absolute_pos + split_form.len();
                }
            }
        }

        // 3. Concat matching: for adjacent query word pairs, try concatenated form
        //    (e.g., "ear" + "buds" -> try matching "earbuds" in text)
        if query_words.len() >= 2 {
            for i in 0..query_words.len() - 1 {
                let concat = format!(
                    "{}{}",
                    query_words[i].to_lowercase(),
                    query_words[i + 1].to_lowercase()
                );
                let mut start = 0;
                while let Some(pos) = text_lower[start..].find(&concat) {
                    let absolute_pos = start + pos;
                    matched_words.push(query_words[i].clone());
                    matched_words.push(query_words[i + 1].clone());
                    match_positions.push((absolute_pos, absolute_pos + concat.len()));
                    start = absolute_pos + concat.len();
                }
            }
        }

        let text_words: Vec<(usize, &str)> = {
            let mut words = Vec::new();
            let mut current_start = 0;
            for (idx, ch) in text.char_indices() {
                if !ch.is_alphanumeric() {
                    if current_start < idx {
                        words.push((current_start, &text[current_start..idx]));
                    }
                    current_start = idx + ch.len_utf8();
                }
            }
            if current_start < text.len() {
                words.push((current_start, &text[current_start..]));
            }
            words
        };

        // 4. Fuzzy matching per word boundary
        for (word_start, text_word) in &text_words {
            let text_word_lower = text_word.to_lowercase();
            for query_word in query_words {
                let query_lower = query_word.to_lowercase();
                if query_lower.len() >= 4 && text_word_lower.len() >= 4 {
                    let distance = strsim::damerau_levenshtein(&query_lower, &text_word_lower);
                    let max_distance = if query_lower.len() >= 8 { 2 } else { 1 };
                    if distance <= max_distance && distance > 0 {
                        matched_words.push(query_word.clone());
                        let highlight_len = query_lower.len().min(text_word.len());
                        match_positions.push((*word_start, word_start + highlight_len));
                    } else if text_word_lower.chars().count() > query_lower.chars().count() {
                        let prefix: String = text_word_lower
                            .chars()
                            .take(query_lower.chars().count())
                            .collect();
                        let prefix_distance = strsim::damerau_levenshtein(&query_lower, &prefix);
                        if prefix_distance <= max_distance {
                            matched_words.push(query_word.clone());
                            let highlight_end = text_word
                                .char_indices()
                                .nth(query_lower.chars().count())
                                .map(|(i, _)| word_start + i)
                                .unwrap_or(word_start + text_word.len());
                            match_positions.push((*word_start, highlight_end));
                        }
                    }
                    if query_lower.chars().count() >= 4 {
                        let query_suffix: String = query_lower.chars().skip(1).collect();
                        let suffix_len = query_suffix.chars().count();
                        if text_word_lower.chars().count() >= suffix_len && suffix_len >= 3 {
                            let text_prefix: String =
                                text_word_lower.chars().take(suffix_len).collect();
                            let suffix_distance =
                                strsim::damerau_levenshtein(&query_suffix, &text_prefix);
                            if suffix_distance <= 1 {
                                matched_words.push(query_word.clone());
                                let highlight_end = text_word
                                    .char_indices()
                                    .nth(suffix_len)
                                    .map(|(i, _)| word_start + i)
                                    .unwrap_or(word_start + text_word.len());
                                match_positions.push((*word_start, highlight_end));
                            }
                        }
                    }
                }
            }
        }

        if matched_words.is_empty() {
            return self.no_match(text.to_string());
        }

        // Merge overlapping/adjacent positions into single spans
        match_positions.sort_by_key(|(start, _)| *start);
        match_positions.dedup();
        let match_positions = Self::merge_positions(match_positions);

        let highlighted = self.apply_highlights(text, &match_positions);

        let unique_matched: std::collections::HashSet<_> = matched_words.iter().collect();
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

pub fn extract_query_words(query_text: &str) -> Vec<String> {
    query_text
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .collect()
}
