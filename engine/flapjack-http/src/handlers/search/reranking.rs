//! Reranking utilities for search results by click-through rate and optional filter criteria.
use std::collections::HashMap;

use flapjack::types::{Document, FieldValue, Filter, ScoredDocument};

pub(super) fn normalize_to_unit(value: f32, min: f32, max: f32) -> f32 {
    if (max - min).abs() < f32::EPSILON {
        0.5
    } else {
        (value - min) / (max - min)
    }
}

/// Rerank documents by fusing BM25 relevancy scores with click-through rate signals.
///
/// Normalizes both BM25 and CTR scores to [0, 1] and combines them as `alpha * normalized_bm25 + (1 - alpha) * normalized_ctr`. Returns documents unchanged if there are ≤1 documents or no click data. Sorts by fused score (descending), then CTR (descending), then original position (ascending) as tie-breakers.
///
/// # Arguments
///
/// * `documents` - Documents to rerank
/// * `click_counts` - Map of document IDs to click counts
/// * `relevancy_strictness` - Weight parameter (0-100, clamped and normalized to [0, 1]). Higher values prioritize BM25; defaults to 50 when None
///
/// # Returns
///
/// Reranked documents.
pub(super) fn rerank_by_ctr(
    documents: Vec<ScoredDocument>,
    click_counts: &HashMap<String, u64>,
    relevancy_strictness: Option<u32>,
) -> Vec<ScoredDocument> {
    if documents.len() <= 1 || click_counts.is_empty() {
        return documents;
    }

    let alpha = relevancy_strictness
        .map(|value| value.min(100) as f32 / 100.0)
        .unwrap_or(0.5);
    let mut ranked: Vec<(usize, f32, f32, ScoredDocument)> = documents
        .into_iter()
        .enumerate()
        .map(|(idx, doc)| {
            let ctr = *click_counts.get(&doc.document.id).unwrap_or(&0) as f32;
            (idx, 0.0, ctr, doc)
        })
        .collect();

    let bm25_min = ranked
        .iter()
        .map(|(_, _, _, doc)| doc.score)
        .fold(f32::INFINITY, f32::min);
    let bm25_max = ranked
        .iter()
        .map(|(_, _, _, doc)| doc.score)
        .fold(f32::NEG_INFINITY, f32::max);
    let ctr_min = ranked
        .iter()
        .map(|(_, _, ctr, _)| *ctr)
        .fold(f32::INFINITY, f32::min);
    let ctr_max = ranked
        .iter()
        .map(|(_, _, ctr, _)| *ctr)
        .fold(f32::NEG_INFINITY, f32::max);

    for (_, fused_score, ctr, doc) in &mut ranked {
        let normalized_bm25 = normalize_to_unit(doc.score, bm25_min, bm25_max);
        let normalized_ctr = normalize_to_unit(*ctr, ctr_min, ctr_max);
        *ctr = normalized_ctr;
        *fused_score = alpha * normalized_bm25 + (1.0 - alpha) * normalized_ctr;
    }

    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.0.cmp(&b.0))
    });

    ranked.into_iter().map(|(_, _, _, doc)| doc).collect()
}

/// Check whether a field value matches a target string value.
///
/// Performs type-appropriate comparison: case-insensitive for text and facet fields, numeric parsing for integer and date fields, epsilon-based comparison for floats, recursive matching for arrays, and always returns false for objects.
///
/// # Arguments
///
/// * `field_value` - The field value to test
/// * `target` - The target string to match against
///
/// # Returns
///
/// True if the field value matches the target, false otherwise.
pub(super) fn optional_filter_matches_field_value(field_value: &FieldValue, target: &str) -> bool {
    match field_value {
        FieldValue::Bool(value) => target
            .parse::<bool>()
            .map(|candidate| candidate == *value)
            .unwrap_or(false),
        FieldValue::Text(text) | FieldValue::Facet(text) => text.eq_ignore_ascii_case(target),
        FieldValue::Integer(value) | FieldValue::Date(value) => target
            .parse::<i64>()
            .map(|candidate| candidate == *value)
            .unwrap_or(false),
        FieldValue::Float(value) => target
            .parse::<f64>()
            .map(|candidate| (candidate - *value).abs() < f64::EPSILON)
            .unwrap_or(false),
        FieldValue::Array(values) => values
            .iter()
            .any(|item| optional_filter_matches_field_value(item, target)),
        FieldValue::Object(_) => false,
    }
}

/// Check whether a document matches a filter, returning None for unsupported filter types.
///
/// Supports Equals (text and facet values only), And, Or, and Not filters. Returns Some(false) if the document lacks the required field or if Equals uses a non-text value. Recursively evaluates nested filters.
///
/// # Arguments
///
/// * `doc` - The document to evaluate
/// * `filter` - The filter to match
///
/// # Returns
///
/// Some(true) if the document matches, Some(false) if it doesn't, None if the filter type is unsupported.
pub(super) fn document_matches_filter_supported(doc: &Document, filter: &Filter) -> Option<bool> {
    match filter {
        Filter::Equals { field, value } => {
            let Some(field_value) = doc.fields.get(field) else {
                return Some(false);
            };
            match value {
                FieldValue::Text(text) | FieldValue::Facet(text) => {
                    Some(optional_filter_matches_field_value(field_value, text))
                }
                _ => None,
            }
        }
        Filter::And(filters) => {
            let mut all_match = true;
            for inner_filter in filters {
                match document_matches_filter_supported(doc, inner_filter) {
                    Some(true) => {}
                    Some(false) => all_match = false,
                    None => return None,
                }
            }
            Some(all_match)
        }
        Filter::Or(filters) => {
            let mut any_match = false;
            for inner_filter in filters {
                match document_matches_filter_supported(doc, inner_filter) {
                    Some(true) => any_match = true,
                    Some(false) => {}
                    None => return None,
                }
            }
            Some(any_match)
        }
        Filter::Not(inner_filter) => {
            document_matches_filter_supported(doc, inner_filter).map(|v| !v)
        }
        _ => None,
    }
}

pub(super) fn document_matches_filter(doc: &Document, filter: &Filter) -> bool {
    document_matches_filter_supported(doc, filter).unwrap_or(false)
}

/// Calculate the aggregate optional filter score for a document.
///
/// For each group of filter criteria, computes a group score: if `sum_or_filters_scores` is true, sums the scores of all matching filters; otherwise takes the maximum. The final score is the sum across all groups.
///
/// # Arguments
///
/// * `doc` - The document to score
/// * `groups` - Groups of (field, value, score) filter tuples
/// * `sum_or_filters_scores` - If true, sum matching scores per group; if false, take the maximum
///
/// # Returns
///
/// Total optional filter score.
pub(super) fn optional_filter_score_for_document(
    doc: &Document,
    groups: &[Vec<(String, String, f32)>],
    sum_or_filters_scores: bool,
) -> f32 {
    groups
        .iter()
        .map(|group| {
            if sum_or_filters_scores {
                group
                    .iter()
                    .filter_map(|(field, value, score)| {
                        let field_value = doc.fields.get(field)?;
                        optional_filter_matches_field_value(field_value, value).then_some(*score)
                    })
                    .sum::<f32>()
            } else {
                group
                    .iter()
                    .filter_map(|(field, value, score)| {
                        let field_value = doc.fields.get(field)?;
                        optional_filter_matches_field_value(field_value, value).then_some(*score)
                    })
                    .reduce(f32::max)
                    .unwrap_or(0.0)
            }
        })
        .sum()
}

/// Rerank documents by optional filter match scores and apply pagination.
///
/// Scores each document using optional filters, sorts by score (descending) with original position as a stable tie-breaker, then returns a paginated slice of the results.
///
/// # Arguments
///
/// * `documents` - Documents to rerank and paginate
/// * `groups` - Groups of (field, value, score) filter criteria
/// * `sum_or_filters_scores` - If true, sum matching scores per group; if false, take the maximum
/// * `page` - Zero-based page number
/// * `hits_per_page` - Number of results per page
///
/// # Returns
///
/// Paginated reranked documents.
pub(super) fn rerank_documents_by_optional_filters(
    documents: Vec<ScoredDocument>,
    groups: &[Vec<(String, String, f32)>],
    sum_or_filters_scores: bool,
    page: usize,
    hits_per_page: usize,
) -> Vec<ScoredDocument> {
    let mut ranked: Vec<(usize, f32, ScoredDocument)> = documents
        .into_iter()
        .enumerate()
        .map(|(idx, doc)| {
            let optional_score =
                optional_filter_score_for_document(&doc.document, groups, sum_or_filters_scores);
            (idx, optional_score, doc)
        })
        .collect();

    // Prefer higher optional filter score; keep original engine order as a stable tie-breaker.
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let start = page.saturating_mul(hits_per_page).min(ranked.len());
    ranked
        .into_iter()
        .skip(start)
        .take(hits_per_page)
        .map(|(_, _, doc)| doc)
        .collect()
}
