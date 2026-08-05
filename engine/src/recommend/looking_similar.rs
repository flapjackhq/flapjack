//! Similarity engine for the looking-similar recommendation model.
//!
//! The model prefers configured vector similarity, then falls back to shared
//! searchable-term/content similarity when vectors are unavailable. The fallback
//! keeps default-feature release builds useful without presenting term matching
//! as vector search.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::error::FlapjackError;
use crate::index::manager::tokenization::collect_doc_tokens_by_path;
use crate::index::settings::{strip_unordered_prefix, IndexSettings};
use crate::index::{Index, SearchOptions};
use crate::query::parser::is_cjk;
use crate::query::stopwords::english_stop_words;
use crate::types::Document;
use crate::IndexManager;

const CANDIDATE_LIMIT: usize = 50;

/// A scored recommendation hit for looking-similar.
#[derive(Debug, Clone)]
pub struct LookingSimilarHit {
    pub object_id: String,
    pub score: u32, // 0-100
    pub document: Option<Document>,
}

/// Find documents similar to a seed.
///
/// Configured vector similarity is preferred when it can score the seed;
/// otherwise the model falls back to shared searchable-term/content similarity.
/// Non-tenant-not-found index access errors still surface to the caller.
///
/// # Arguments
///
/// * `manager` - IndexManager containing recommendation indices
/// * `index_name` - Name of the search index to query
/// * `seed_object_id` - ID of the document to find similar matches for
/// * `threshold` - Minimum similarity score (0-100) for results to include
/// * `max_recommendations` - Maximum number of results to return
///
/// # Returns
///
/// `LookingSimilarHit` results ranked by descending similarity score, or an
/// error string if the selected search path cannot be accessed. Scores are
/// min/max normalized relative to the returned candidate set across the fixed
/// candidate window before threshold filtering and truncation. Empty hits are
/// valid when the index or seed is missing, the seed has no usable terms, or the
/// chosen similarity path finds no qualifying candidates after thresholding.
pub fn compute_looking_similar(
    manager: &Arc<IndexManager>,
    index_name: &str,
    seed_object_id: &str,
    threshold: u32,
    max_recommendations: u32,
) -> Result<Vec<LookingSimilarHit>, String> {
    let index = match manager.get_or_load(index_name) {
        Ok(index) => index,
        Err(FlapjackError::TenantNotFound(_)) => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };

    if let Some(hits) = try_vector_similar(
        manager,
        index_name,
        seed_object_id,
        threshold,
        max_recommendations,
    )? {
        return Ok(hits);
    }

    compute_term_similar(
        manager,
        &index,
        index_name,
        seed_object_id,
        threshold,
        max_recommendations,
    )
}

#[cfg(feature = "vector-search")]
fn try_vector_similar(
    manager: &Arc<IndexManager>,
    index_name: &str,
    seed_object_id: &str,
    threshold: u32,
    max_recommendations: u32,
) -> Result<Option<Vec<LookingSimilarHit>>, String> {
    let Some(vector_index) = manager.get_vector_index(index_name) else {
        return Ok(None);
    };

    let vi = vector_index
        .read()
        .map_err(|e| format!("vector index read lock poisoned: {e}"))?;
    if vi.is_empty() {
        return Ok(None);
    }

    let Some(seed_vector) = vi
        .get(seed_object_id)
        .map_err(|e| format!("failed to load seed vector: {e}"))?
    else {
        return Ok(None);
    };

    let search_limit = vi.len().max(1);
    let raw = vi
        .search(&seed_vector, search_limit)
        .map_err(|e| format!("vector search failed: {e}"))?;

    let candidates = raw
        .into_iter()
        .filter(|r| r.doc_id != seed_object_id)
        .map(|r| (r.doc_id, -r.distance))
        .collect::<Vec<_>>();
    let ranked = normalize_and_rank(candidates, threshold, max_recommendations);
    drop(vi);

    // Vector search owns only object IDs, so it must hydrate after ranking. The
    // term path below reuses documents already hydrated by the search pipeline.
    let hits = ranked
        .into_iter()
        .map(|(object_id, score)| {
            let document = get_document_or_error(manager, index_name, &object_id)?;
            Ok(LookingSimilarHit {
                object_id,
                score,
                document,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Some(hits))
}

#[cfg(not(feature = "vector-search"))]
fn try_vector_similar(
    _manager: &Arc<IndexManager>,
    _index_name: &str,
    _seed_object_id: &str,
    _threshold: u32,
    _max_recommendations: u32,
) -> Result<Option<Vec<LookingSimilarHit>>, String> {
    Ok(None)
}

/// Map higher-is-better relevance values onto the public 0-100 score, then
/// threshold, deterministically order, and truncate the results.
fn normalize_and_rank(
    candidates: Vec<(String, f32)>,
    threshold: u32,
    max_recommendations: u32,
) -> Vec<(String, u32)> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let min_relevance = candidates
        .iter()
        .map(|(_, relevance)| *relevance)
        .fold(f32::INFINITY, f32::min);
    let max_relevance = candidates
        .iter()
        .map(|(_, relevance)| *relevance)
        .fold(f32::NEG_INFINITY, f32::max);
    let spread = max_relevance - min_relevance;

    let mut ranked = candidates
        .into_iter()
        .map(|(object_id, relevance)| {
            let score = if spread.abs() < f32::EPSILON {
                100
            } else {
                (((relevance - min_relevance) / spread) * 100.0)
                    .round()
                    .clamp(0.0, 100.0) as u32
            };
            (object_id, score)
        })
        .filter(|(_, score)| *score >= threshold)
        .collect::<Vec<_>>();

    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    ranked.truncate(max_recommendations as usize);
    ranked
}

fn get_document_or_error(
    manager: &Arc<IndexManager>,
    index_name: &str,
    object_id: &str,
) -> Result<Option<Document>, String> {
    manager
        .get_document(index_name, object_id)
        .map_err(|error| error.to_string())
}

/// Bounds on seed-term extraction. Declared in Stage 1 so the red tests compile; Stage 2
/// implements the body and BINDS to these names and values rather than redeclaring them.
///
/// The two caps must not make each other unreachable. Shortest admissible non-CJK token is
/// MIN_SEED_TERM_CHARS = 3, costing 4 chars with its separator, so a 64-char budget would
/// cap Latin input at 16 terms and MAX_SEED_TERMS = 25 could NEVER bind -- making
/// `seed_terms_are_capped` arithmetically unsatisfiable. 200 leaves both live: 25 terms
/// averaging 7 characters. For CJK the character budget is the real bound, because the
/// query parser re-splits CJK to one token per character (query/parser/mod.rs:38-44).
const MAX_SEED_TERMS: usize = 25;
const MIN_SEED_TERM_CHARS: usize = 3;
const MAX_SEED_QUERY_CHARS: usize = 200;

fn seed_terms(
    document: &Document,
    searchable_attributes: &[String],
    keep_diacritics_on_characters: &str,
    custom_normalization: &[(char, String)],
    camel_case_attributes: &[String],
) -> Vec<String> {
    let stop_words = english_stop_words();
    let mut seen = HashSet::new();
    let mut terms = Vec::new();
    let mut query_chars = 0;

    let tokens = collect_doc_tokens_by_path(
        document,
        searchable_attributes,
        keep_diacritics_on_characters,
        custom_normalization,
        camel_case_attributes,
    );

    for token in tokens.into_iter().flat_map(|(_, tokens)| tokens) {
        let is_short_non_cjk = token.chars().count() < MIN_SEED_TERM_CHARS
            && !token.chars().next().is_some_and(is_cjk);
        if stop_words.contains(token.as_str()) || is_short_non_cjk || !seen.insert(token.clone()) {
            continue;
        }

        let next_query_chars = query_chars + token.chars().count() + 1;
        if next_query_chars > MAX_SEED_QUERY_CHARS {
            break;
        }
        query_chars = next_query_chars;
        terms.push(token);
    }

    terms.truncate(MAX_SEED_TERMS);
    terms
}

fn compute_term_similar(
    manager: &Arc<IndexManager>,
    index: &Index,
    index_name: &str,
    seed_object_id: &str,
    threshold: u32,
    max_recommendations: u32,
) -> Result<Vec<LookingSimilarHit>, String> {
    let Some(seed) = get_document_or_error(manager, index_name, seed_object_id)? else {
        return Ok(Vec::new());
    };

    let settings = manager.get_settings(index_name);
    let (attributes, restrict_paths) = resolve_searchable_attributes(index, settings.as_deref());
    if restrict_paths.is_empty() {
        return Ok(Vec::new());
    }

    let custom_normalization = settings
        .as_deref()
        .map(IndexSettings::flatten_custom_normalization)
        .unwrap_or_default();
    let keep_diacritics = settings
        .as_deref()
        .map(|value| value.keep_diacritics_on_characters.as_str())
        .unwrap_or("");
    let camel_case_attributes = settings
        .as_deref()
        .map(|value| value.camel_case_attributes.as_slice())
        .unwrap_or(&[]);
    let terms = seed_terms(
        &seed,
        &attributes,
        keep_diacritics,
        &custom_normalization,
        camel_case_attributes,
    );
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let result = manager
        .search_with_options(
            index_name,
            &terms.join(" "),
            &SearchOptions {
                all_query_words_optional: true,
                query_type: Some("prefixNone"),
                typo_tolerance: Some(false),
                limit: CANDIDATE_LIMIT,
                enable_rules: Some(false),
                enable_synonyms: Some(false),
                restrict_searchable_attrs: Some(&restrict_paths),
                ..Default::default()
            },
        )
        .map_err(|error| error.to_string())?;

    let mut documents = HashMap::new();
    let mut candidates = Vec::new();
    for scored_document in result.documents {
        let object_id = scored_document.document.id.clone();
        if object_id != seed_object_id {
            candidates.push((object_id.clone(), scored_document.score));
            documents.insert(object_id, scored_document.document);
        }
    }

    Ok(
        normalize_and_rank(candidates, threshold, max_recommendations)
            .into_iter()
            .filter_map(|(object_id, score)| {
                documents
                    .remove(&object_id)
                    .map(|document| LookingSimilarHit {
                        object_id,
                        score,
                        document: Some(document),
                    })
            })
            .collect(),
    )
}

fn resolve_searchable_attributes(
    index: &Index,
    settings: Option<&IndexSettings>,
) -> (Vec<String>, Vec<String>) {
    let mut searchable_paths = index.searchable_paths();
    searchable_paths.sort();

    let attributes = settings
        .and_then(|value| value.searchable_attributes.as_ref())
        .filter(|configured| !configured.is_empty())
        .map(|configured| {
            configured
                .iter()
                .map(|attribute| strip_unordered_prefix(attribute).to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| searchable_paths.clone());
    let restrict_paths = searchable_paths
        .into_iter()
        .filter(|path| {
            attributes.iter().any(|attribute| {
                path == attribute
                    || path
                        .strip_prefix(attribute)
                        .is_some_and(|suffix| suffix.starts_with('.'))
            })
        })
        .collect();

    (attributes, restrict_paths)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tempfile::TempDir;

    use super::*;
    use crate::index::settings::IndexSettings;
    use crate::query::stopwords::RemoveStopWordsValue;
    use crate::types::FieldValue;

    const INDEX_NAME: &str = "products";

    fn text_document(id: &str, fields: &[(&str, &str)]) -> Document {
        Document {
            id: id.to_string(),
            fields: fields
                .iter()
                .map(|(name, value)| ((*name).to_string(), FieldValue::Text((*value).to_string())))
                .collect::<HashMap<_, _>>(),
        }
    }

    async fn configured_manager(
        searchable_attributes: &[&str],
        documents: Vec<Document>,
    ) -> (TempDir, Arc<IndexManager>) {
        let settings = IndexSettings {
            searchable_attributes: Some(
                searchable_attributes
                    .iter()
                    .map(|attribute| (*attribute).to_string())
                    .collect(),
            ),
            ..Default::default()
        };
        configured_manager_with_settings(settings, documents).await
    }

    async fn configured_manager_with_settings(
        settings: IndexSettings,
        documents: Vec<Document>,
    ) -> (TempDir, Arc<IndexManager>) {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant(INDEX_NAME).unwrap();

        settings
            .save(temp_dir.path().join(INDEX_NAME).join("settings.json"))
            .unwrap();
        manager.invalidate_settings_cache(INDEX_NAME);
        manager
            .add_documents_sync(INDEX_NAME, documents)
            .await
            .unwrap();

        (temp_dir, manager)
    }

    fn shared_vocabulary_documents() -> Vec<Document> {
        vec![
            text_document(
                "seed",
                &[(
                    "name",
                    "Wireless Bluetooth Headphones with active noise cancelling",
                )],
            ),
            text_document(
                "all_terms",
                &[(
                    "name",
                    "Wireless Bluetooth Headphones with active noise cancelling travel",
                )],
            ),
            text_document(
                "five_terms",
                &[("name", "Wireless Bluetooth Headphones noise cancelling")],
            ),
            text_document("three_terms", &[("name", "Wireless Bluetooth Headphones")]),
            text_document("two_terms", &[("name", "Wireless Headphones")]),
            text_document("one_term", &[("name", "Bluetooth speaker")]),
            text_document("zero_overlap", &[("name", "Ceramic coffee grinder")]),
        ]
    }

    fn compute(
        manager: &Arc<IndexManager>,
        seed_object_id: &str,
        threshold: u32,
        max_recommendations: u32,
    ) -> Vec<LookingSimilarHit> {
        compute_looking_similar(
            manager,
            INDEX_NAME,
            seed_object_id,
            threshold,
            max_recommendations,
        )
        .unwrap()
    }

    fn object_ids(hits: &[LookingSimilarHit]) -> Vec<String> {
        hits.iter().map(|hit| hit.object_id.clone()).collect()
    }

    fn assert_normalized_score_order(hits: &[LookingSimilarHit]) {
        assert_eq!(hits.first().map(|hit| hit.score), Some(100));
        assert!(hits.windows(2).all(|pair| pair[0].score >= pair[1].score));
        assert!(hits.iter().all(|hit| hit.score <= 100));
    }

    #[tokio::test]
    async fn document_read_errors_are_not_treated_as_missing() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());

        let result = get_document_or_error(&manager, "../invalid", "seed");

        assert!(result.is_err());
    }

    /// Catches a fallback that returns no hits or ranks documents independently of shared seed vocabulary.
    #[tokio::test]
    async fn term_fallback_ranks_by_shared_vocabulary() {
        let (_temp_dir, manager) =
            configured_manager(&["name"], shared_vocabulary_documents()).await;

        let hits = compute(&manager, "seed", 0, 10);
        let ids = object_ids(&hits);

        assert!(!ids.contains(&"zero_overlap".to_string()));
        assert_eq!(
            ids,
            vec![
                "all_terms",
                "five_terms",
                "three_terms",
                "two_terms",
                "one_term",
            ]
        );
        assert_normalized_score_order(&hits);
    }

    /// Catches a fallback that recommends the seed itself while otherwise returning valid matches.
    #[tokio::test]
    async fn term_fallback_excludes_seed() {
        let (_temp_dir, manager) =
            configured_manager(&["name"], shared_vocabulary_documents()).await;

        let ids = object_ids(&compute(&manager, "seed", 0, 10));

        assert!(ids.contains(&"all_terms".to_string()));
        assert!(!ids.contains(&"seed".to_string()));
    }

    /// Catches an unknown seed object being treated as an error or as an empty browse query.
    #[tokio::test]
    async fn term_fallback_unknown_object_id_returns_empty() {
        let (_temp_dir, manager) =
            configured_manager(&["name"], shared_vocabulary_documents()).await;

        let hits = compute(&manager, "missing", 0, 10);

        assert!(hits.is_empty());
    }

    /// Catches an unusable seed becoming an empty query that recommends the entire index.
    #[tokio::test]
    async fn term_fallback_seed_with_no_usable_terms_returns_empty() {
        let documents = vec![
            text_document("seed", &[("sku", "A1")]),
            text_document("other", &[("sku", "B2")]),
        ];
        let (_temp_dir, manager) = configured_manager(&["sku"], documents).await;

        let hits = compute(&manager, "seed", 0, 10);

        assert!(hits.is_empty());
    }

    /// Catches a missing index leaking `TenantNotFound` through the Algolia-compatible endpoint.
    #[tokio::test]
    async fn term_fallback_missing_index_returns_empty_not_error() {
        let (temp_dir, manager) = configured_manager(
            &["name"],
            vec![text_document("control", &[("name", "control document")])],
        )
        .await;

        let result = compute_looking_similar(&manager, "missing", "seed", 0, 10);

        drop(temp_dir);
        assert!(matches!(result, Ok(hits) if hits.is_empty()));
    }

    /// Catches score normalization over the requested result window instead of a fixed candidate pool.
    #[tokio::test]
    async fn term_fallback_middle_score_is_independent_of_max_recommendations() {
        let (_temp_dir, manager) =
            configured_manager(&["name"], shared_vocabulary_documents()).await;

        let short_window = compute(&manager, "seed", 0, 2);
        let long_window = compute(&manager, "seed", 0, 5);

        assert!(
            short_window.len() >= 2 && long_window.len() >= 2,
            "both result windows must contain the middle-ranked candidate"
        );
        assert_eq!(short_window[1].object_id, long_window[1].object_id);
        assert_eq!(short_window[1].score, long_window[1].score);
    }

    /// Catches threshold filtering before normalization or a fixture tie at the normalized maximum.
    #[tokio::test]
    async fn term_fallback_respects_threshold() {
        let documents = vec![
            text_document(
                "seed",
                &[("name", "wireless bluetooth headphones zephyrium")],
            ),
            // The rare shared term deliberately breaks the top-score tie.
            text_document(
                "strict_winner",
                &[("name", "wireless bluetooth headphones zephyrium travel")],
            ),
            text_document("runner_up", &[("name", "wireless bluetooth headphones")]),
            text_document("partial", &[("name", "wireless headphones")]),
        ];
        let (_temp_dir, manager) = configured_manager(&["name"], documents).await;

        let hits = compute(&manager, "seed", 100, 10);

        assert_eq!(object_ids(&hits), vec!["strict_winner"]);
        assert_eq!(hits[0].score, 100);
    }

    /// Catches `maxRecommendations` being ignored after term candidates are ranked.
    #[tokio::test]
    async fn term_fallback_truncates_to_max_recommendations() {
        let (_temp_dir, manager) =
            configured_manager(&["name"], shared_vocabulary_documents()).await;

        let hits = compute(&manager, "seed", 0, 2);

        assert_eq!(object_ids(&hits), vec!["all_terms", "five_terms"]);
    }

    /// Catches fallback-local stopword rules diverging from the index setting and short Latin tokens producing matches.
    #[tokio::test]
    async fn term_fallback_ignores_stop_words_and_short_tokens() {
        let documents = vec![
            text_document("seed", &[("name", "the xy wireless headphones")]),
            text_document("real_overlap", &[("name", "wireless carrying case")]),
            text_document("noise_only", &[("name", "the xy")]),
        ];
        let settings = IndexSettings {
            searchable_attributes: Some(vec!["name".to_string()]),
            remove_stop_words: RemoveStopWordsValue::All,
            ..Default::default()
        };
        let (_temp_dir, manager) = configured_manager_with_settings(settings, documents).await;

        let ids = object_ids(&compute(&manager, "seed", 0, 10));

        assert!(ids.contains(&"real_overlap".to_string()));
        assert!(!ids.contains(&"noise_only".to_string()));
    }

    /// Catches seed extraction treating `unordered(...)` as a literal field name.
    #[tokio::test]
    async fn term_fallback_handles_unordered_searchable_attributes() {
        let documents = vec![
            text_document(
                "seed",
                &[
                    ("title", "Wireless Headphones"),
                    ("description", "Active noise cancelling"),
                ],
            ),
            text_document(
                "match",
                &[
                    ("title", "Wireless Headphones"),
                    ("description", "Travel audio case"),
                ],
            ),
            text_document("other", &[("title", "Ceramic grinder")]),
        ];
        let (_temp_dir, manager) =
            configured_manager(&["unordered(title)", "description"], documents).await;

        let hits = compute(&manager, "seed", 0, 10);

        assert_eq!(object_ids(&hits), vec!["match"]);
        assert_normalized_score_order(&hits);
    }

    /// Catches a blanket Latin minimum-length rule discarding short CJK searchable fields.
    #[tokio::test]
    async fn term_fallback_returns_hits_for_short_cjk_fields() {
        let documents = vec![
            text_document("seed", &[("city", "東京"), ("brand", "資生堂")]),
            text_document("tokyo_match", &[("city", "東京"), ("brand", "花王")]),
            text_document("other", &[("city", "大阪"), ("brand", "花王")]),
        ];
        let (_temp_dir, manager) = configured_manager(&["city", "brand"], documents).await;

        let hits = compute(&manager, "seed", 0, 10);

        assert_eq!(object_ids(&hits), vec!["tokyo_match"]);
        assert_normalized_score_order(&hits);
    }

    /// Catches extraction ignoring `MAX_SEED_TERMS` when the character budget has not bound first.
    #[test]
    fn seed_terms_are_capped() {
        let value = (0..40)
            .map(|index| format!("w{index:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        let document = text_document("seed", &[("name", &value)]);

        let terms = seed_terms(&document, &["name".to_string()], "", &[], &[]);

        assert_eq!(terms.len(), MAX_SEED_TERMS);
    }

    /// Catches changing `MIN_SEED_TERM_CHARS` without preserving the short-token boundary.
    #[test]
    fn seed_terms_respects_minimum_latin_term_length() {
        let too_short = "x".repeat(MIN_SEED_TERM_CHARS - 1);
        let long_enough = "y".repeat(MIN_SEED_TERM_CHARS);
        let value = format!("{too_short} {long_enough}");
        let document = text_document("seed", &[("name", &value)]);

        let terms = seed_terms(&document, &["name".to_string()], "", &[], &[]);

        assert_eq!(terms, vec![long_enough]);
    }

    /// Catches seed extraction delegating stop-word removal to index search settings.
    #[test]
    fn seed_terms_removes_english_stop_words() {
        let document = text_document("seed", &[("name", "the wireless and headphones")]);

        let terms = seed_terms(&document, &["name".to_string()], "", &[], &[]);

        assert_eq!(terms, vec!["wireless", "headphones"]);
    }

    /// Catches extraction exceeding the query budget or satisfying it by dropping every seed term.
    #[test]
    fn seed_terms_respects_character_budget() {
        let long_tokens = ['a', 'b', 'c', 'd', 'e']
            .into_iter()
            .map(|character| character.to_string().repeat(60))
            .collect::<Vec<_>>()
            .join(" ");
        let document = text_document("seed", &[("name", &long_tokens)]);

        let terms = seed_terms(&document, &["name".to_string()], "", &[], &[]);
        let joined_length = terms.join(" ").chars().count();

        assert!(!terms.is_empty());
        assert!(joined_length <= MAX_SEED_QUERY_CHARS);
    }
}
