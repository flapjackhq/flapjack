mod comparison;
mod criteria;

pub(super) use comparison::*;
pub(super) use criteria::*;

use super::*;
use crate::index::DEFAULT_RELEVANCY_STRICTNESS;

pub(super) struct RankedDoc {
    typo_bucket: u8,
    proximity_bucket: u8,
    exact_vs_prefix: u8,
    best_attribute_index: usize,
    tuned_score: f32,
    optional_filter_score: f32,
    words_matched: usize,
    custom_values: Vec<RankingSortValue>,
    doc_id: String,
    doc_len_tokens: usize,
    doc: ScoredDocument,
}

/// Compare two documents across the configured ranking criteria (typo, proximity,
/// attribute, exact, words, geo, filters) in order, returning the first non-equal
/// comparison. Falls through to custom ranking if all built-in criteria tie.
fn compare_builtin_ranking_criteria(
    a: &RankedDoc,
    b: &RankedDoc,
    ranking_criteria: &[RankingCriterion],
    all_query_words_optional: bool,
) -> Ordering {
    for criterion in ranking_criteria {
        let comparison = match criterion {
            RankingCriterion::Typo => a.typo_bucket.cmp(&b.typo_bucket),
            RankingCriterion::Geo => Ordering::Equal,
            RankingCriterion::Words => {
                if all_query_words_optional {
                    b.words_matched.cmp(&a.words_matched)
                } else {
                    Ordering::Equal
                }
            }
            RankingCriterion::Filters => b
                .optional_filter_score
                .partial_cmp(&a.optional_filter_score)
                .unwrap_or(Ordering::Equal),
            RankingCriterion::Proximity => a.proximity_bucket.cmp(&b.proximity_bucket),
            RankingCriterion::Attribute => a.best_attribute_index.cmp(&b.best_attribute_index),
            RankingCriterion::Exact => a.exact_vs_prefix.cmp(&b.exact_vs_prefix),
        };

        if comparison != Ordering::Equal {
            return comparison;
        }
    }

    Ordering::Equal
}

pub(super) fn count_matched_query_words(query_terms: &[String], doc_tokens: &[String]) -> usize {
    if query_terms.is_empty() || doc_tokens.is_empty() {
        return 0;
    }

    let normalized_doc_tokens: HashSet<&str> =
        doc_tokens.iter().map(|token| token.as_str()).collect();
    let mut unique_terms = HashSet::new();
    query_terms
        .iter()
        .filter(|term| unique_terms.insert(term.as_str()))
        .filter(|term| normalized_doc_tokens.contains(term.as_str()))
        .count()
}

/// Compute the proximity score for a document given query terms and per-attribute tokens.
///
/// Algolia proximity: for each pair of adjacent query terms (term_i, term_{i+1}),
/// find the minimum positional distance within any single attribute, clamp to at
/// least `min_proximity`, then sum across all adjacent pairs.
/// Single-term queries return 0. Lower is better.
pub(super) fn compute_proximity_score(
    query_terms: &[String],
    tokens_by_path: &[(usize, Vec<String>)],
    prefix_eligible: &[bool],
    min_proximity: u32,
    unordered_path_indexes: &HashSet<usize>,
) -> u32 {
    if query_terms.len() <= 1 {
        return 0;
    }

    let mut total: u32 = 0;

    for pair_idx in 0..query_terms.len() - 1 {
        let term_a = &query_terms[pair_idx];
        let term_b = &query_terms[pair_idx + 1];
        let b_is_prefix = prefix_eligible.get(pair_idx + 1).copied().unwrap_or(false);
        let a_is_prefix = prefix_eligible.get(pair_idx).copied().unwrap_or(false);
        let mut best_pair_distance: u32 = u32::MAX;

        for (path_idx, tokens) in tokens_by_path {
            let positions_a = find_term_positions(tokens, term_a.as_str(), a_is_prefix);
            let positions_b = find_term_positions(tokens, term_b.as_str(), b_is_prefix);
            let pair_distance = min_distance_sorted(&positions_a, &positions_b);
            if pair_distance == u32::MAX {
                continue;
            }
            if unordered_path_indexes.contains(path_idx) {
                // `unordered(attr)` removes positional penalty for this attribute.
                // As soon as both terms are present, this path contributes the neutral
                // minimum-proximity distance regardless of actual word positions.
                best_pair_distance = best_pair_distance.min(min_proximity);
            } else {
                best_pair_distance = best_pair_distance.min(pair_distance);
            }

            if best_pair_distance == min_proximity {
                break; // Can't do better across attributes either
            }
        }

        // Sentinel: if no attribute had both terms, use a large distance
        if best_pair_distance == u32::MAX {
            best_pair_distance = 100;
        }

        // Clamp per-pair distance to at least min_proximity
        best_pair_distance = best_pair_distance.max(min_proximity);

        total = total.saturating_add(best_pair_distance);
    }

    total
}

/// Re-rank search results using the full ranking pipeline (built-in criteria,
/// custom ranking, optional filters, BM25 tie-breaking). Builds a
/// `Stage2RankingContext` once and sorts in place.
pub(super) fn sort_results_with_stage2_ranking(
    all_results: &mut Vec<ScoredDocument>,
    params: Stage2RankingContext<'_>,
) {
    let Stage2RankingContext {
        query_text,
        searchable_paths,
        settings,
        synonym_store,
        plural_map,
        query_type,
        optional_filter_groups,
        sum_or_filters_scores,
        exact_on_single_word_query,
        disable_exact_on_attributes,
        custom_normalization,
        keep_diacritics_on_characters,
        camel_case_attributes,
        all_query_words_optional,
        relevancy_strictness,
        min_proximity,
    } = params;

    if all_results.len() < 2 {
        return;
    }

    let query_terms = tokenize_for_typo_bucket(
        query_text,
        keep_diacritics_on_characters,
        custom_normalization,
    );
    let ranking_criteria = parse_ranking_criteria(settings);
    let custom_specs = parse_custom_ranking_specs(settings);
    let prefix_eligible = compute_prefix_eligible(query_type, query_terms.len(), query_text);
    let effective_min_proximity = min_proximity
        .or(settings.and_then(|s| s.min_proximity))
        .unwrap_or(1);
    let min_word_size_for_1_typo = settings
        .map(|s| s.min_word_size_for_1_typo as usize)
        .unwrap_or(4);
    let min_word_size_for_2_typos = settings
        .map(|s| s.min_word_size_for_2_typos as usize)
        .unwrap_or(8);
    let attribute_criteria_computed_by_min_proximity = settings
        .and_then(|s| s.attribute_criteria_computed_by_min_proximity)
        .unwrap_or(false);
    // `unordered(attr)` disables the position/proximity signal for that attribute.
    let unordered_paths: HashSet<String> = settings
        .and_then(|s| s.searchable_attributes.as_ref())
        .map(|attrs| {
            attrs
                .iter()
                .filter_map(|attr| {
                    let stripped = strip_unordered_prefix(attr);
                    (stripped != attr).then_some(stripped.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    let unordered_path_indexes: HashSet<usize> = searchable_paths
        .iter()
        .enumerate()
        .filter_map(|(idx, path)| unordered_paths.contains(path).then_some(idx))
        .collect();
    let term_alternatives = build_term_alternatives(
        &query_terms,
        settings
            .map(|s| s.alternatives_as_exact.as_slice())
            .unwrap_or(&[]),
        synonym_store,
        plural_map,
    );

    let mut keyed_docs: Vec<RankedDoc> = std::mem::take(all_results)
        .into_iter()
        .map(|doc| {
            let tokens_by_path = collect_doc_tokens_by_path(
                &doc.document,
                searchable_paths,
                keep_diacritics_on_characters,
                custom_normalization,
                camel_case_attributes,
            );
            let doc_tokens: Vec<String> = tokens_by_path
                .iter()
                .flat_map(|(_, tokens)| tokens.iter().cloned())
                .collect();
            let doc_tokens = if doc_tokens.is_empty() {
                // Fallback: collect from all fields (same as collect_doc_tokens)
                let mut fallback = Vec::new();
                for (path, value) in &doc.document.fields {
                    collect_tokens_for_field_value(
                        value,
                        &mut fallback,
                        keep_diacritics_on_characters,
                        custom_normalization,
                        camel_case_attributes,
                        path,
                    );
                }
                fallback
            } else {
                doc_tokens
            };
            let typo_bucket = compute_typo_bucket_from_tokens(
                &query_terms,
                &doc_tokens,
                &prefix_eligible,
                min_word_size_for_1_typo,
                min_word_size_for_2_typos,
            );
            let exact_vs_prefix = compute_exact_vs_prefix_bucket(
                &query_terms,
                &tokens_by_path,
                searchable_paths,
                &prefix_eligible,
                &term_alternatives,
                exact_on_single_word_query,
                disable_exact_on_attributes,
            );
            let best_attribute_index = compute_best_attribute_index(
                &query_terms,
                &tokens_by_path,
                &AttributeRankingConfig {
                    prefix_eligible: &prefix_eligible,
                    min_word_size_for_1_typo,
                    min_word_size_for_2_typos,
                    attribute_criteria_computed_by_min_proximity,
                    min_proximity: effective_min_proximity,
                    unordered_path_indexes: &unordered_path_indexes,
                },
            );
            let proximity_bucket = compute_proximity_score(
                &query_terms,
                &tokens_by_path,
                &prefix_eligible,
                effective_min_proximity,
                &unordered_path_indexes,
            )
            .min(255) as u8;
            let optional_filter_score = optional_filter_groups
                .map(|groups| {
                    compute_optional_filter_score(&doc.document, groups, sum_or_filters_scores)
                })
                .unwrap_or(0.0);
            let words_matched = if all_query_words_optional {
                count_matched_query_words(&query_terms, &doc_tokens)
            } else {
                0
            };
            let custom_values = custom_specs
                .iter()
                .map(|spec| extract_custom_ranking_value(&doc.document, &spec.field))
                .collect();
            let doc_id = doc.document.id.clone();
            let doc_len_tokens = doc_tokens.len().max(1);
            RankedDoc {
                typo_bucket,
                proximity_bucket,
                exact_vs_prefix,
                best_attribute_index,
                tuned_score: doc.score,
                optional_filter_score,
                words_matched,
                custom_values,
                doc_id,
                doc_len_tokens,
                doc,
            }
        })
        .collect();

    let avg_doc_len_tokens = keyed_docs
        .iter()
        .map(|e| e.doc_len_tokens as f32)
        .sum::<f32>()
        / keyed_docs.len() as f32;
    for entry in &mut keyed_docs {
        let tuned = tune_bm25_score_for_short_fields(
            entry.tuned_score,
            entry.doc_len_tokens,
            avg_doc_len_tokens,
        );
        entry.tuned_score = tuned;
        entry.doc.score = tuned;
    }

    let default_ordering = |a: &RankedDoc, b: &RankedDoc| {
        compare_builtin_ranking_criteria(a, b, &ranking_criteria, all_query_words_optional)
            .then_with(|| compare_custom_values(&a.custom_values, &b.custom_values, &custom_specs))
            .then_with(|| {
                b.tuned_score
                    .partial_cmp(&a.tuned_score)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| a.doc_id.cmp(&b.doc_id))
    };

    let effective_strictness = relevancy_strictness.unwrap_or(DEFAULT_RELEVANCY_STRICTNESS);

    // For strictness 1–99: filter docs below (strictness/100) * max_tuned_score
    if effective_strictness > 0 && effective_strictness < 100 {
        if let Some(max_score) = keyed_docs.iter().map(|d| d.tuned_score).reduce(f32::max) {
            let threshold = (effective_strictness as f32 / 100.0) * max_score;
            keyed_docs.retain(|d| d.tuned_score >= threshold);
        }
    }

    keyed_docs.sort_by(|a, b| match effective_strictness {
        0 => {
            // Pure custom ranking: custom_values → doc_id (textual relevance ignored)
            compare_custom_values(&a.custom_values, &b.custom_values, &custom_specs)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        }
        1..=99 => {
            // Custom ranking first, then textual tiers as tiebreaker
            compare_custom_values(&a.custom_values, &b.custom_values, &custom_specs)
                .then_with(|| default_ordering(a, b))
        }
        _ => default_ordering(a, b),
    });

    *all_results = keyed_docs.into_iter().map(|e| e.doc).collect();
}
