use super::super::*;
use crate::index::settings::{classify_ranking_criterion_token, RankingCriterionToken};

#[derive(Clone, Copy)]
pub(super) enum RankingCriterion {
    Typo,
    Geo,
    Words,
    Filters,
    Proximity,
    Attribute,
    Exact,
}

const DEFAULT_RANKING_CRITERIA: [RankingCriterion; 7] = [
    RankingCriterion::Typo,
    RankingCriterion::Geo,
    RankingCriterion::Words,
    RankingCriterion::Filters,
    RankingCriterion::Proximity,
    RankingCriterion::Attribute,
    RankingCriterion::Exact,
];

/// Parse ranking criteria from index settings into the internal enum representation.
/// Falls back to the default order (typo, geo, words, filters, proximity, attribute,
/// exact) when settings are absent or empty.
pub(super) fn parse_ranking_criteria(settings: Option<&IndexSettings>) -> Vec<RankingCriterion> {
    let criteria: Vec<_> = settings
        .and_then(|index_settings| index_settings.ranking.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|criterion| classify_ranking_criterion_token(criterion).map(ranking_criterion))
        .collect();

    if criteria.is_empty() {
        DEFAULT_RANKING_CRITERIA.to_vec()
    } else {
        criteria
    }
}

fn ranking_criterion(token: RankingCriterionToken) -> RankingCriterion {
    match token {
        RankingCriterionToken::Typo => RankingCriterion::Typo,
        RankingCriterionToken::Geo => RankingCriterion::Geo,
        RankingCriterionToken::Words => RankingCriterion::Words,
        RankingCriterionToken::Filters => RankingCriterion::Filters,
        RankingCriterionToken::Proximity => RankingCriterion::Proximity,
        RankingCriterionToken::Attribute => RankingCriterion::Attribute,
        RankingCriterionToken::Exact => RankingCriterion::Exact,
    }
}

pub(in crate::index::manager) fn str_prefix_by_chars(input: &str, char_count: usize) -> String {
    input.chars().take(char_count).collect()
}

pub(in crate::index::manager) fn typo_distance_strict(
    query_term: &str,
    candidate_token: &str,
    allow_prefix: bool,
) -> usize {
    if query_term == candidate_token {
        return 0;
    }

    let query_len_chars = query_term.chars().count();
    let candidate_len_chars = candidate_token.chars().count();
    if query_len_chars == 0 || candidate_len_chars == 0 {
        return usize::MAX;
    }

    let full_distance = strsim::damerau_levenshtein(query_term, candidate_token);
    if candidate_len_chars >= query_len_chars && allow_prefix {
        let prefix = str_prefix_by_chars(candidate_token, query_len_chars);
        if prefix == query_term {
            return 0;
        }
        return full_distance.min(strsim::damerau_levenshtein(query_term, &prefix));
    }
    full_distance
}

/// Compute a typo-distance bucket for a document by matching query tokens against
/// document tokens. Uses strict distance (no prefix matching) and sums the best
/// per-token distances, capped at a configurable maximum.
pub(in crate::index::manager) fn compute_typo_bucket_from_tokens(
    query_terms: &[String],
    doc_tokens: &[String],
    prefix_eligible: &[bool],
    min_word_size_for_1_typo: usize,
    min_word_size_for_2_typos: usize,
) -> u8 {
    if query_terms.is_empty() {
        return 3;
    }

    if doc_tokens.is_empty() {
        return 3;
    }

    let mut worst_term_distance = 0usize;
    for (i, query_term) in query_terms.iter().enumerate() {
        let allow_prefix = prefix_eligible.get(i).copied().unwrap_or(false);
        let term_len = query_term.chars().count();
        let max_allowed_typos = max_allowed_typos_for_term_len(
            term_len,
            min_word_size_for_1_typo,
            min_word_size_for_2_typos,
        );
        let best = doc_tokens
            .iter()
            .map(|token| typo_distance_strict(query_term, token, allow_prefix))
            .min()
            .unwrap_or(usize::MAX);

        if best == usize::MAX || best > max_allowed_typos {
            return 3;
        }

        worst_term_distance = worst_term_distance.max(best);
        if worst_term_distance > 2 {
            return 3;
        }
    }

    match worst_term_distance {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => 3,
    }
}

pub(in crate::index::manager) fn classify_match(
    query_term: &str,
    candidate_token: &str,
) -> (usize, bool) {
    if query_term == candidate_token {
        return (0, false); // exact match
    }

    let query_len_chars = query_term.chars().count();
    let candidate_len_chars = candidate_token.chars().count();
    if query_len_chars == 0 || candidate_len_chars == 0 {
        return (usize::MAX, false);
    }

    if candidate_len_chars >= query_len_chars {
        let prefix = str_prefix_by_chars(candidate_token, query_len_chars);
        if prefix == query_term {
            return (0, true); // prefix match — distance 0 but prefix-only
        }
        // Not a prefix match — compare full and prefix edit distances
        let full_distance = strsim::damerau_levenshtein(query_term, candidate_token);
        let prefix_distance = strsim::damerau_levenshtein(query_term, &prefix);
        (full_distance.min(prefix_distance), false)
    } else {
        (
            strsim::damerau_levenshtein(query_term, candidate_token),
            false,
        )
    }
}

/// Determine which query terms are prefix-eligible based on `query_type`.
/// - `prefixAll` → all true
/// - `prefixNone` → all false
/// - `prefixLast` → only the last term, and only if the query doesn't end with a space
pub(in crate::index::manager) fn compute_prefix_eligible(
    query_type: &str,
    num_terms: usize,
    query_text: &str,
) -> Vec<bool> {
    match query_type {
        "prefixAll" => vec![true; num_terms],
        "prefixNone" => vec![false; num_terms],
        _ => {
            // prefixLast (default)
            let mut flags = vec![false; num_terms];
            if num_terms > 0 && !query_text.ends_with(' ') {
                flags[num_terms - 1] = true;
            }
            flags
        }
    }
}

pub(in crate::index::manager) type TermAlternatives = Vec<Vec<String>>;

/// Build alternative term forms (plurals, decompound parts) for each query token
/// from the plural expansion map. Returns a vec parallel to the query tokens, where
/// each entry lists the original term plus any expansions.
pub(in crate::index::manager) fn build_term_alternatives(
    query_terms: &[String],
    alternatives_as_exact: &[String],
    synonym_store: Option<&SynonymStore>,
    plural_map: Option<&HashMap<String, Vec<String>>>,
) -> TermAlternatives {
    let include_ignore_plurals = alternatives_as_exact.iter().any(|v| v == "ignorePlurals");
    let include_single_word_synonym = alternatives_as_exact
        .iter()
        .any(|v| v == "singleWordSynonym");
    let include_multi_word_synonym = alternatives_as_exact
        .iter()
        .any(|v| v == "multiWordsSynonym");

    let collect_synonym_candidates = |alternatives: &mut Vec<String>, synonyms: &[String]| {
        for candidate in synonyms {
            let normalized = candidate.trim().to_lowercase();
            if normalized.is_empty() {
                continue;
            }
            let token_count = normalized.split_whitespace().count();
            let should_include = (token_count == 1 && include_single_word_synonym)
                || (token_count > 1 && include_multi_word_synonym);
            if should_include {
                push_unique_terms(alternatives, std::iter::once(normalized));
            }
        }
    };

    query_terms
        .iter()
        .map(|term| {
            let mut alternatives = vec![term.clone()];

            if include_ignore_plurals {
                if let Some(forms) = plural_map.and_then(|m| m.get(term)) {
                    push_unique_terms(&mut alternatives, forms.iter().cloned());
                }
            }

            if include_single_word_synonym || include_multi_word_synonym {
                if let Some(store) = synonym_store {
                    for synonym in store.values() {
                        match synonym {
                            Synonym::Regular { synonyms, .. }
                                if synonyms
                                    .iter()
                                    .any(|value| value.eq_ignore_ascii_case(term)) =>
                            {
                                collect_synonym_candidates(&mut alternatives, synonyms);
                            }
                            Synonym::OneWay {
                                input, synonyms, ..
                            } if input.eq_ignore_ascii_case(term) => {
                                collect_synonym_candidates(&mut alternatives, synonyms);
                            }
                            _ => {}
                        }
                    }
                }
            }

            alternatives
        })
        .collect()
}

/// Find all character positions where a term (or any of its alternatives) appears
/// in a field's text. Returns a sorted, deduplicated list of positions.
pub(in crate::index::manager) fn find_term_positions(
    tokens: &[String],
    term: &str,
    is_prefix: bool,
) -> Vec<usize> {
    tokens
        .iter()
        .enumerate()
        .filter(|(_, tok)| {
            if is_prefix {
                tok.starts_with(term)
            } else {
                tok.as_str() == term
            }
        })
        .map(|(pos, _)| pos)
        .collect()
}

/// Compute the minimum pairwise distance between two sorted position lists.
/// Returns `usize::MAX` if either list is empty.
pub(in crate::index::manager) fn min_distance_sorted(
    positions_a: &[usize],
    positions_b: &[usize],
) -> u32 {
    if positions_a.is_empty() || positions_b.is_empty() {
        return u32::MAX;
    }

    let mut best = u32::MAX;
    let mut i = 0;
    let mut j = 0;
    while i < positions_a.len() && j < positions_b.len() {
        let dist = (positions_a[i] as i64 - positions_b[j] as i64).unsigned_abs() as u32;
        best = best.min(dist);
        if best == 1 {
            break;
        }
        if positions_a[i] < positions_b[j] {
            i += 1;
        } else {
            j += 1;
        }
    }

    best
}

pub(in crate::index::manager) fn contains_contiguous_subsequence(
    tokens: &[String],
    sequence: &[String],
) -> bool {
    if sequence.is_empty() || sequence.len() > tokens.len() {
        return false;
    }
    tokens
        .windows(sequence.len())
        .any(|window| window.iter().zip(sequence.iter()).all(|(a, b)| a == b))
}

/// Check whether an attribute path is disabled for exact-tier consideration.
/// Uses the nested-path rule: `d == path || path.starts_with("d.")`.
fn is_path_disabled_for_exact(path: &str, disable_exact_on_attributes: &[String]) -> bool {
    disable_exact_on_attributes
        .iter()
        .any(|d| d == path || path.starts_with(&format!("{}.", d)))
}

/// Partition document tokens into eligible and disabled sets based on
/// `disable_exact_on_attributes`. Returns `(eligible, disabled)` where each
/// entry is a slice of the document's tokens for that attribute.
fn partition_tokens_by_exact_eligibility<'a>(
    tokens_by_path: &'a [(usize, Vec<String>)],
    searchable_paths: &[String],
    disable_exact_on_attributes: &[String],
) -> (Vec<&'a [String]>, Vec<&'a [String]>) {
    let mut eligible: Vec<&[String]> = Vec::new();
    let mut disabled: Vec<&[String]> = Vec::new();
    for (path_idx, tokens) in tokens_by_path {
        let path = searchable_paths
            .get(*path_idx)
            .map(|s| s.as_str())
            .unwrap_or("");
        if is_path_disabled_for_exact(path, disable_exact_on_attributes) {
            disabled.push(tokens.as_slice());
        } else {
            eligible.push(tokens.as_slice());
        }
    }
    (eligible, disabled)
}

/// Split raw alternative forms (which may contain multi-word strings like "new york")
/// into deduplicated single-word and multi-word alternative lists. Falls back to the
/// original query term if no single-word alternatives remain.
fn normalize_alternatives(
    query_term: &str,
    raw_alternatives: &[String],
) -> (Vec<String>, Vec<Vec<String>>) {
    let mut single_word: Vec<String> = Vec::new();
    let mut multi_word: Vec<Vec<String>> = Vec::new();
    for alternative in raw_alternatives {
        let tokens: Vec<String> = alternative
            .split_whitespace()
            .map(|part| part.to_string())
            .collect();
        if tokens.len() <= 1 {
            if let Some(token) = tokens.first() {
                if !single_word.contains(token) {
                    single_word.push(token.clone());
                }
            }
        } else if !multi_word.contains(&tokens) {
            multi_word.push(tokens);
        }
    }
    if single_word.is_empty() {
        single_word.push(query_term.to_string());
    }
    (single_word, multi_word)
}

/// Check whether any eligible attribute satisfies the exact-match criterion.
/// In "attribute" mode (single-word query), the entire attribute value must match.
/// In "word" mode or multi-word queries, any individual token match counts.
fn has_exact_match(
    eligible: &[&[String]],
    single_word_alts: &[String],
    multi_word_alts: &[Vec<String>],
    use_attribute_mode: bool,
) -> bool {
    if use_attribute_mode {
        // Algolia "attribute" mode: attribute must contain ONLY this single term.
        // For multi-word synonyms, the synonym must cover the entire attribute value.
        eligible.iter().any(|tokens| {
            (tokens.len() == 1
                && single_word_alts
                    .iter()
                    .any(|alternative| tokens[0] == *alternative))
                || multi_word_alts.iter().any(|alternative| {
                    tokens.len() == alternative.len()
                        && contains_contiguous_subsequence(tokens, alternative)
                })
        })
    } else {
        // "word" mode or multi-word query: any matching token counts.
        eligible.iter().any(|tokens| {
            tokens.iter().any(|token| {
                single_word_alts
                    .iter()
                    .any(|alternative| token == alternative)
            }) || multi_word_alts
                .iter()
                .any(|alternative| contains_contiguous_subsequence(tokens, alternative))
        })
    }
}

/// Check whether any token set contains a word-level exact or prefix match for the
/// given alternatives. Used as the fallback when the mode-specific exact check fails:
/// a match here means the term was found but doesn't qualify as "exact" → tier 1.
fn any_fallback_match(
    token_sets: &[&[String]],
    single_word_alts: &[String],
    multi_word_alts: &[Vec<String>],
) -> bool {
    token_sets.iter().any(|tokens| {
        tokens.iter().any(|token| {
            single_word_alts
                .iter()
                .any(|alternative| token == alternative || classify_match(alternative, token).1)
        }) || multi_word_alts
            .iter()
            .any(|alternative| contains_contiguous_subsequence(tokens, alternative))
    })
}

pub(in crate::index::manager) fn compute_exact_vs_prefix_bucket(
    query_terms: &[String],
    tokens_by_path: &[(usize, Vec<String>)],
    searchable_paths: &[String],
    prefix_eligible: &[bool],
    term_alternatives: &[Vec<String>],
    exact_on_single_word_query: &str,
    disable_exact_on_attributes: &[String],
) -> u8 {
    let is_single_word = query_terms.len() == 1;

    // "none" mode: exact tier disabled for single-word queries.
    if is_single_word && exact_on_single_word_query == "none" {
        return 0;
    }

    let (eligible, disabled) = partition_tokens_by_exact_eligibility(
        tokens_by_path,
        searchable_paths,
        disable_exact_on_attributes,
    );
    let use_attribute_mode = is_single_word && exact_on_single_word_query == "attribute";

    for (i, query_term) in query_terms.iter().enumerate() {
        if !prefix_eligible.get(i).copied().unwrap_or(false) {
            continue; // non-prefix terms can't produce prefix-tier distinctions
        }

        let raw_alts = term_alternatives
            .get(i)
            .filter(|alts| !alts.is_empty())
            .map(|alts| alts.as_slice())
            .unwrap_or_else(|| std::slice::from_ref(query_term));
        let (single_word_alts, multi_word_alts) = normalize_alternatives(query_term, raw_alts);

        if has_exact_match(
            &eligible,
            &single_word_alts,
            &multi_word_alts,
            use_attribute_mode,
        ) {
            continue; // exact match found for this term
        }

        // No exact match by the current mode's definition. A fallback match (word-level
        // exact or prefix) in either eligible or disabled attributes → tier 1.
        if any_fallback_match(&eligible, &single_word_alts, &multi_word_alts) {
            return 1;
        }
        if any_fallback_match(&disabled, &single_word_alts, &multi_word_alts) {
            return 1;
        }
    }
    0 // all prefix-eligible terms have exact matches (or no prefix-eligible terms)
}

/// Returns the lowest `searchable_paths` index where any query term matches any
/// token, respecting per-term prefix eligibility and typo-length thresholds.
/// Returns `usize::MAX` if no match found in any listed attribute.
pub(in crate::index::manager) struct AttributeRankingConfig<'a> {
    pub prefix_eligible: &'a [bool],
    pub min_word_size_for_1_typo: usize,
    pub min_word_size_for_2_typos: usize,
    pub attribute_criteria_computed_by_min_proximity: bool,
    pub min_proximity: u32,
    pub unordered_path_indexes: &'a HashSet<usize>,
}

/// Find the best (lowest) attribute index where any query token appears, using
/// the searchable attributes ordering. Lower indices rank higher.
pub(in crate::index::manager) fn compute_best_attribute_index(
    query_terms: &[String],
    tokens_by_path: &[(usize, Vec<String>)],
    config: &AttributeRankingConfig<'_>,
) -> usize {
    if config.attribute_criteria_computed_by_min_proximity {
        return compute_best_attribute_by_proximity(
            query_terms,
            tokens_by_path,
            config.prefix_eligible,
            config.min_proximity,
            config.unordered_path_indexes,
        );
    }

    let mut best = usize::MAX;
    for &(path_idx, ref tokens) in tokens_by_path {
        if path_idx >= best {
            continue; // can't improve
        }
        for (i, query_term) in query_terms.iter().enumerate() {
            let allow_prefix = config.prefix_eligible.get(i).copied().unwrap_or(false);
            let term_len = query_term.chars().count();
            let max_allowed_typos = max_allowed_typos_for_term_len(
                term_len,
                config.min_word_size_for_1_typo,
                config.min_word_size_for_2_typos,
            );
            let matched = tokens.iter().any(|token| {
                typo_distance_strict(query_term, token, allow_prefix) <= max_allowed_typos
            });
            if matched {
                best = path_idx;
                break; // found match in this path, move to next path
            }
        }
    }
    best
}

/// Rank by the sum of minimum pairwise distances between consecutive query terms
/// in each attribute. Lower total proximity scores rank higher.
pub(in crate::index::manager) fn compute_best_attribute_by_proximity(
    query_terms: &[String],
    tokens_by_path: &[(usize, Vec<String>)],
    prefix_eligible: &[bool],
    min_proximity: u32,
    unordered_path_indexes: &HashSet<usize>,
) -> usize {
    if query_terms.is_empty() {
        return usize::MAX;
    }

    if query_terms.len() == 1 {
        let term = &query_terms[0];
        let is_prefix = prefix_eligible.first().copied().unwrap_or(false);
        let mut best_path = usize::MAX;
        for (path_idx, tokens) in tokens_by_path {
            if !find_term_positions(tokens, term, is_prefix).is_empty() {
                best_path = best_path.min(*path_idx);
            }
        }
        return best_path;
    }

    let mut best_path = usize::MAX;
    let mut best_score = u32::MAX;

    for (path_idx, tokens) in tokens_by_path {
        let mut attr_total = 0u32;
        let mut has_any_pair = false;

        for pair_idx in 0..query_terms.len() - 1 {
            let term_a = &query_terms[pair_idx];
            let term_b = &query_terms[pair_idx + 1];
            let a_is_prefix = prefix_eligible.get(pair_idx).copied().unwrap_or(false);
            let b_is_prefix = prefix_eligible.get(pair_idx + 1).copied().unwrap_or(false);
            let positions_a = find_term_positions(tokens, term_a, a_is_prefix);
            let positions_b = find_term_positions(tokens, term_b, b_is_prefix);
            let pair_distance = min_distance_sorted(&positions_a, &positions_b);
            if pair_distance == u32::MAX {
                continue;
            }
            has_any_pair = true;
            let effective_distance = if unordered_path_indexes.contains(path_idx) {
                min_proximity
            } else {
                pair_distance.max(min_proximity)
            };
            attr_total = attr_total.saturating_add(effective_distance);
        }

        if !has_any_pair {
            continue;
        }

        if attr_total < best_score || (attr_total == best_score && *path_idx < best_path) {
            best_score = attr_total;
            best_path = *path_idx;
        }
    }

    best_path
}

// Tantivy BM25 constants in our fork are fixed at k1=1.2, b=0.75.
// Stage 2 tunes only length normalization behavior for short product fields.
const BM25_K1: f32 = 1.2;
const BM25_SOURCE_B: f32 = 0.75;
const BM25_TARGET_B_SHORT_FIELDS: f32 = 0.4;

pub(in crate::index::manager) fn bm25_length_normalization_denominator(
    doc_len_tokens: usize,
    avg_doc_len_tokens: f32,
    b: f32,
) -> f32 {
    if avg_doc_len_tokens <= 0.0 {
        return 1.0;
    }
    let dl = doc_len_tokens.max(1) as f32;
    1.0 + BM25_K1 * (1.0 - b + b * (dl / avg_doc_len_tokens))
}

/// Compute a correction factor for BM25 scoring on short fields (fewer than 5
/// tokens). Boosts exact or near-exact matches in short fields to compensate
/// for BM25's length normalization bias.
pub(in crate::index::manager) fn bm25_short_field_correction_factor(
    doc_len_tokens: usize,
    avg_doc_len_tokens: f32,
) -> f32 {
    let source =
        bm25_length_normalization_denominator(doc_len_tokens, avg_doc_len_tokens, BM25_SOURCE_B);
    let target = bm25_length_normalization_denominator(
        doc_len_tokens,
        avg_doc_len_tokens,
        BM25_TARGET_B_SHORT_FIELDS,
    );
    if target <= 0.0 {
        1.0
    } else {
        source / target
    }
}

pub(in crate::index::manager) fn tune_bm25_score_for_short_fields(
    raw_score: f32,
    doc_len_tokens: usize,
    avg_doc_len_tokens: f32,
) -> f32 {
    raw_score * bm25_short_field_correction_factor(doc_len_tokens, avg_doc_len_tokens)
}

pub(in crate::index::manager) fn max_allowed_typos_for_term_len(
    term_len: usize,
    min_word_size_for_1_typo: usize,
    min_word_size_for_2_typos: usize,
) -> usize {
    if term_len >= min_word_size_for_2_typos {
        2
    } else if term_len >= min_word_size_for_1_typo {
        1
    } else {
        0
    }
}
