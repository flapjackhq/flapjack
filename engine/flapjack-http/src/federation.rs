//! Stub summary for federation.rs.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::HashMap;
use utoipa::ToSchema;

const DEFAULT_FEDERATION_OFFSET: usize = 0;
const DEFAULT_FEDERATION_LIMIT: usize = 20;
const RRF_K: f64 = 60.0;

/// Top-level federated batch settings shared by request parsing and OpenAPI.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FederationConfig {
    #[serde(default = "default_federation_offset")]
    pub offset: usize,
    #[serde(default = "default_federation_limit")]
    pub limit: usize,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub merge_facets: Option<Value>,
}

/// `_federation` metadata attached to every hit in a federated batch response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FederationMeta {
    pub index_name: String,
    pub queries_position: usize,
    pub weighted_ranking_score: f64,
}

/// A federated hit preserves arbitrary document fields while making `_federation`
/// metadata explicit in both runtime values and the published schema.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FederatedHit {
    #[serde(flatten)]
    pub document: HashMap<String, Value>,
    #[serde(rename = "_federation")]
    pub federation: FederationMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FederationCandidate {
    pub hit: Value,
    pub index_name: String,
    pub queries_position: usize,
    pub rank_in_index: usize,
    pub weight: f64,
}

/// Flat federated batch response returned when the top-level `federation` field is present.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FederatedResponse {
    pub hits: Vec<FederatedHit>,
    pub estimated_total_hits: usize,
    pub limit: usize,
    pub offset: usize,
    #[serde(skip_serializing_if = "Option::is_none", rename = "processingTimeMS")]
    pub processing_time_ms: Option<u64>,
}

/// Merges hits from multiple batch queries using weighted Reciprocal Rank Fusion,
/// de-duplicating by objectID and paginating per the federation config.
pub fn merge_federated_results(
    candidates: Vec<FederationCandidate>,
    estimated_total_hits_per_query: Vec<usize>,
    config: FederationConfig,
) -> FederatedResponse {
    let mut merged_hits: HashMap<String, AccumulatedHit> = HashMap::new();

    for (sequence_number, candidate) in candidates.iter().enumerate() {
        let contribution = candidate.weight / (RRF_K + candidate.rank_in_index as f64 + 1.0);
        let dedup_key = deduplication_key(candidate, sequence_number);
        let candidate_tie_break = (candidate.queries_position, candidate.rank_in_index);

        let merged = merged_hits
            .entry(dedup_key)
            .or_insert_with(|| AccumulatedHit::from_candidate(candidate.clone()));
        merged.weighted_ranking_score += contribution;

        let merged_tie_break = (merged.queries_position, merged.rank_in_index);
        if candidate_tie_break < merged_tie_break {
            merged.hit = candidate.hit.clone();
            merged.index_name = candidate.index_name.clone();
            merged.queries_position = candidate.queries_position;
            merged.rank_in_index = candidate.rank_in_index;
        }
    }

    let mut ranked_hits: Vec<AccumulatedHit> = merged_hits.into_values().collect();
    ranked_hits.sort_by(|left, right| {
        right
            .weighted_ranking_score
            .partial_cmp(&left.weighted_ranking_score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.queries_position.cmp(&right.queries_position))
            .then_with(|| left.rank_in_index.cmp(&right.rank_in_index))
    });

    let total_hits = ranked_hits.len();
    let start = config.offset.min(total_hits);
    let end = start.saturating_add(config.limit).min(total_hits);
    let hits = ranked_hits[start..end]
        .iter()
        .map(attach_federation_metadata)
        .collect();

    FederatedResponse {
        hits,
        estimated_total_hits: estimated_total_hits_per_query.into_iter().sum(),
        limit: config.limit,
        offset: config.offset,
        processing_time_ms: None,
    }
}

fn default_federation_offset() -> usize {
    DEFAULT_FEDERATION_OFFSET
}

fn default_federation_limit() -> usize {
    DEFAULT_FEDERATION_LIMIT
}

#[derive(Debug, Clone)]
struct AccumulatedHit {
    hit: Value,
    index_name: String,
    queries_position: usize,
    rank_in_index: usize,
    weighted_ranking_score: f64,
}

impl AccumulatedHit {
    fn from_candidate(candidate: FederationCandidate) -> Self {
        Self {
            hit: candidate.hit,
            index_name: candidate.index_name,
            queries_position: candidate.queries_position,
            rank_in_index: candidate.rank_in_index,
            weighted_ranking_score: 0.0,
        }
    }
}

fn deduplication_key(candidate: &FederationCandidate, sequence_number: usize) -> String {
    if let Some(object_id) = candidate.hit.get("objectID") {
        if let Ok(serialized_object_id) = serde_json::to_string(object_id) {
            return format!("{}::{}", candidate.index_name, serialized_object_id);
        }
    }
    format!("{}::__sequence_{}", candidate.index_name, sequence_number)
}

/// Wraps an accumulated hit with federation metadata (index name, query position).
fn attach_federation_metadata(accumulated_hit: &AccumulatedHit) -> FederatedHit {
    let document = accumulated_hit
        .hit
        .as_object()
        .cloned()
        .map(|object| object.into_iter().collect())
        .unwrap_or_else(|| {
            // Search hits are expected to be JSON objects. Preserve unexpected
            // scalar or array payloads under `_raw` instead of dropping them.
            HashMap::from([("_raw".to_string(), accumulated_hit.hit.clone())])
        });

    FederatedHit {
        document,
        federation: FederationMeta {
            index_name: accumulated_hit.index_name.clone(),
            queries_position: accumulated_hit.queries_position,
            weighted_ranking_score: accumulated_hit.weighted_ranking_score,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// TODO: Document candidate.
    fn candidate(
        index_name: &str,
        object_id: &str,
        queries_position: usize,
        rank_in_index: usize,
        weight: f64,
    ) -> FederationCandidate {
        FederationCandidate {
            hit: json!({
                "objectID": object_id,
                "title": format!("{index_name}-{object_id}")
            }),
            index_name: index_name.to_string(),
            queries_position,
            rank_in_index,
            weight,
        }
    }

    fn ids(response: &FederatedResponse) -> Vec<String> {
        response
            .hits
            .iter()
            .map(|hit| {
                hit.document
                    .get("objectID")
                    .and_then(Value::as_str)
                    .expect("objectID should exist")
                    .to_string()
            })
            .collect()
    }

    /// TODO: Document equal_weight_two_index_merge_uses_rrf_ordering.
    #[test]
    fn equal_weight_two_index_merge_uses_rrf_ordering() {
        let candidates = vec![
            candidate("products", "p1", 0, 0, 1.0),
            candidate("products", "p2", 0, 1, 1.0),
            candidate("articles", "a1", 1, 0, 1.0),
            candidate("articles", "a2", 1, 1, 1.0),
        ];

        let response = merge_federated_results(
            candidates,
            vec![2, 2],
            FederationConfig {
                offset: 0,
                limit: 20,
                merge_facets: None,
            },
        );

        assert_eq!(ids(&response), vec!["p1", "a1", "p2", "a2"]);
    }

    /// TODO: Document weight_boosting_changes_cross_index_rank_order.
    #[test]
    fn weight_boosting_changes_cross_index_rank_order() {
        let candidates = vec![
            candidate("products", "p1", 0, 0, 1.3),
            candidate("products", "p2", 0, 1, 1.3),
            candidate("articles", "a1", 1, 0, 1.0),
        ];

        let response = merge_federated_results(
            candidates,
            vec![2, 1],
            FederationConfig {
                offset: 0,
                limit: 20,
                merge_facets: None,
            },
        );

        assert_eq!(ids(&response), vec!["p1", "p2", "a1"]);
    }

    /// TODO: Document tie_breaking_is_score_then_queries_position_then_rank.
    #[test]
    fn tie_breaking_is_score_then_queries_position_then_rank() {
        let candidates = vec![
            candidate("products", "rank0-q0", 0, 0, 61.0),
            candidate("products", "rank1-q0", 0, 1, 62.0),
            candidate("products", "rank0-q1", 1, 0, 61.0),
        ];

        let response = merge_federated_results(
            candidates,
            vec![2, 1],
            FederationConfig {
                offset: 0,
                limit: 20,
                merge_facets: None,
            },
        );

        assert_eq!(ids(&response), vec!["rank0-q0", "rank1-q0", "rank0-q1"]);
    }

    #[test]
    fn both_empty_inputs_return_empty_hits() {
        let response = merge_federated_results(
            Vec::new(),
            vec![0, 0],
            FederationConfig {
                offset: 0,
                limit: 20,
                merge_facets: None,
            },
        );

        assert!(response.hits.is_empty());
        assert_eq!(response.estimated_total_hits, 0);
    }

    /// TODO: Document single_index_preserves_original_rank_order.
    #[test]
    fn single_index_preserves_original_rank_order() {
        let candidates = vec![
            candidate("products", "p1", 0, 0, 1.0),
            candidate("products", "p2", 0, 1, 1.0),
            candidate("products", "p3", 0, 2, 1.0),
        ];

        let response = merge_federated_results(
            candidates,
            vec![3],
            FederationConfig {
                offset: 0,
                limit: 20,
                merge_facets: None,
            },
        );

        assert_eq!(ids(&response), vec!["p1", "p2", "p3"]);
        // Scores must decrease monotonically for later ranks in a single-index merge.
        for window in response.hits.windows(2) {
            assert!(
                window[0].federation.weighted_ranking_score
                    >= window[1].federation.weighted_ranking_score,
                "scores should decrease or stay equal with increasing rank"
            );
        }
    }

    /// TODO: Document duplicate_index_and_object_id_sum_scores_but_cross_index_duplicates_stay_distinct.
    #[test]
    fn duplicate_index_and_object_id_sum_scores_but_cross_index_duplicates_stay_distinct() {
        let candidates = vec![
            candidate("products", "shared", 0, 0, 1.0),
            candidate("products", "shared", 1, 2, 1.0),
            candidate("products", "other", 0, 1, 1.0),
            candidate("articles", "shared", 0, 0, 1.0),
        ];

        let response = merge_federated_results(
            candidates,
            vec![2, 2],
            FederationConfig {
                offset: 0,
                limit: 20,
                merge_facets: None,
            },
        );

        assert_eq!(response.hits.len(), 3);

        let top_hit = response.hits.first().expect("top hit should exist");
        assert_eq!(
            top_hit.document.get("objectID").and_then(Value::as_str),
            Some("shared")
        );
        assert_eq!(top_hit.federation.index_name, "products");

        let products_score = top_hit.federation.weighted_ranking_score;
        let expected = (1.0 / 61.0) + (1.0 / 63.0);
        assert!((products_score - expected).abs() < 1e-12);

        let shared_hits = response
            .hits
            .iter()
            .filter(|hit| hit.document.get("objectID").and_then(Value::as_str) == Some("shared"))
            .count();
        assert_eq!(shared_hits, 2);
    }

    /// TODO: Document pagination_is_applied_after_sort_and_metadata_is_attached.
    #[test]
    fn pagination_is_applied_after_sort_and_metadata_is_attached() {
        let candidates = vec![
            candidate("products", "p1", 0, 0, 1.0),
            candidate("articles", "a1", 1, 0, 1.0),
            candidate("products", "p2", 0, 1, 1.0),
            candidate("articles", "a2", 1, 1, 1.0),
            candidate("products", "p3", 0, 2, 1.0),
        ];

        let response = merge_federated_results(
            candidates,
            vec![3, 2, 4],
            FederationConfig {
                offset: 1,
                limit: 2,
                merge_facets: None,
            },
        );

        assert_eq!(response.estimated_total_hits, 9);
        assert_eq!(response.offset, 1);
        assert_eq!(response.limit, 2);
        assert_eq!(ids(&response), vec!["a1", "p2"]);

        for hit in &response.hits {
            assert!(!hit.federation.index_name.is_empty());
            assert!(hit.federation.weighted_ranking_score.is_finite());
        }
    }

    /// Offset past the end of all candidates returns an empty hits array
    /// without panicking — verifies the `.min(total_hits)` guard in pagination.
    #[test]
    fn offset_beyond_total_hits_returns_empty_hits() {
        let candidates = vec![
            candidate("products", "p1", 0, 0, 1.0),
            candidate("products", "p2", 0, 1, 1.0),
        ];

        let response = merge_federated_results(
            candidates,
            vec![2],
            FederationConfig {
                offset: 100,
                limit: 20,
                merge_facets: None,
            },
        );

        // Hits should be empty because offset exceeds candidate count.
        assert!(
            response.hits.is_empty(),
            "offset past end must yield zero hits"
        );
        // estimated_total_hits is the sum of per-query nbHits, independent of pagination.
        assert_eq!(response.estimated_total_hits, 2);
        assert_eq!(response.offset, 100);
        assert_eq!(response.limit, 20);
    }

    /// A query with weight=0.0 contributes zero RRF score, so its hits should
    /// rank below all non-zero-weight hits of the same depth.
    #[test]
    fn zero_weight_query_contributes_no_ranking_influence() {
        let candidates = vec![
            // Normal-weight query at position 0
            candidate("products", "p1", 0, 0, 1.0),
            // Zero-weight query at position 1
            candidate("articles", "a1", 1, 0, 0.0),
        ];

        let response = merge_federated_results(
            candidates,
            vec![1, 1],
            FederationConfig {
                offset: 0,
                limit: 20,
                merge_facets: None,
            },
        );

        assert_eq!(ids(&response), vec!["p1", "a1"]);
        // The zero-weight hit should have a weighted score of exactly 0.
        let zero_hit = &response.hits[1];
        assert_eq!(
            zero_hit.federation.weighted_ranking_score, 0.0,
            "zero-weight query should produce zero weighted score"
        );
        // The normal hit should have a positive score.
        let normal_hit = &response.hits[0];
        assert!(
            normal_hit.federation.weighted_ranking_score > 0.0,
            "normal-weight query should have positive weighted score"
        );
    }

    /// When a hit value is not a JSON object (e.g. a scalar string), the merge
    /// should preserve it under the `_raw` key instead of dropping it silently.
    #[test]
    fn non_object_hit_preserved_under_raw_key() {
        let non_object_candidate = FederationCandidate {
            hit: json!("just a string value"),
            index_name: "products".to_string(),
            queries_position: 0,
            rank_in_index: 0,
            weight: 1.0,
        };

        let response = merge_federated_results(
            vec![non_object_candidate],
            vec![1],
            FederationConfig {
                offset: 0,
                limit: 20,
                merge_facets: None,
            },
        );

        assert_eq!(response.hits.len(), 1);
        let hit = &response.hits[0];
        // Non-object hits get wrapped in `_raw` to prevent silent data loss.
        assert_eq!(
            hit.document.get("_raw"),
            Some(&json!("just a string value")),
            "non-object hit must be preserved under _raw key"
        );
        assert_eq!(hit.federation.index_name, "products");
    }

    /// Large offset combined with small limit should still work correctly,
    /// returning only the narrow window from the ranked results.
    #[test]
    fn large_offset_with_small_limit_returns_correct_window() {
        // Create 10 candidates across 2 indexes.
        let mut candidates = Vec::new();
        for i in 0..5 {
            candidates.push(candidate("alpha", &format!("a{i}"), 0, i, 1.0));
            candidates.push(candidate("beta", &format!("b{i}"), 1, i, 1.0));
        }

        let response = merge_federated_results(
            candidates,
            vec![5, 5],
            FederationConfig {
                offset: 8,
                limit: 1,
                merge_facets: None,
            },
        );

        // 10 total candidates, offset 8, limit 1 → should return exactly 1 hit.
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.estimated_total_hits, 10);
        assert_eq!(response.offset, 8);
        assert_eq!(response.limit, 1);
    }

    /// TODO: Document federation_candidate_serde_uses_camel_case_contract.
    #[test]
    fn federation_candidate_serde_uses_camel_case_contract() {
        let candidate = candidate("products", "p1", 2, 3, 1.3);
        let serialized = serde_json::to_value(&candidate).expect("candidate should serialize");

        assert_eq!(serialized["indexName"], "products");
        assert_eq!(serialized["queriesPosition"], 2);
        assert_eq!(serialized["rankInIndex"], 3);
        assert_eq!(serialized["weight"], 1.3);
        assert!(serialized.get("index_name").is_none());
        assert!(serialized.get("queries_position").is_none());
        assert!(serialized.get("rank_in_index").is_none());

        let deserialized: FederationCandidate =
            serde_json::from_value(serialized).expect("candidate should deserialize");
        assert_eq!(deserialized.index_name, "products");
        assert_eq!(deserialized.queries_position, 2);
        assert_eq!(deserialized.rank_in_index, 3);
        assert_eq!(deserialized.weight, 1.3);
    }
}
