use std::collections::HashMap;
use std::sync::Arc;

use flapjack::types::{ScoredDocument, SearchResult};

use crate::dto::SearchRequest;
use crate::handlers::replicas::is_virtual_settings_only_index;
use crate::handlers::AppState;

use super::geo::{apply_rule_geo_overrides, best_geoloc_for_filter, extract_all_geolocs};
use super::highlight::collect_facet_values;
use super::personalization::{apply_personalization_boost_in_tiers, PersonalizationContext};
use super::pipeline::{PreparedSearchParams, SearchPhaseWitness, TransformOutputs};
use super::reranking::{
    document_matches_filter, rerank_by_ctr, rerank_documents_by_optional_filters,
};

type GeoDistanceMap = HashMap<String, (f64, f64, f64)>;

pub(super) struct TransformContext<'a> {
    pub(super) state: &'a Arc<AppState>,
    pub(super) req: &'a SearchRequest,
    pub(super) params: &'a PreparedSearchParams,
    pub(super) effective_index: &'a str,
    pub(super) personalization_ctx: Option<&'a PersonalizationContext>,
    pub(super) is_interleaving: bool,
    pub(super) search_phase_witness: SearchPhaseWitness,
}

/// Apply all post-search transforms: CTR re-ranking, optional filter re-ranking,
/// geo filtering/sorting/pagination, faceting-after-distinct, and personalization.
///
/// Returns `TransformOutputs` containing geo distances and automatic radius needed
/// by the response formatter.
pub(super) fn apply_reranking_and_transforms(
    result: &mut SearchResult,
    context: TransformContext<'_>,
) -> TransformOutputs {
    let TransformContext {
        state,
        req,
        params,
        effective_index,
        personalization_ctx,
        is_interleaving,
        search_phase_witness,
    } = context;

    apply_ctr_reranking(state, req, params, result, effective_index, is_interleaving);

    if let Some(groups) = params.optional_filter_groups.as_ref() {
        if !is_interleaving && !params.is_hybrid_active {
            result.documents = rerank_documents_by_optional_filters(
                std::mem::take(&mut result.documents),
                groups,
                params.sum_or_filters_scores,
                req.page,
                params.hits_per_page,
            );
        }
    }

    let geo_params = apply_rule_geo_overrides(
        params.geo_params.clone(),
        result.effective_around_lat_lng.as_deref(),
        result.effective_around_radius.as_ref(),
    );

    let (geo_distances, automatic_radius) = apply_geo_filtering(req, params, result, &geo_params);
    recompute_facets_after_distinct(req, params, result);

    if params.sort.is_none() && !geo_params.has_geo_filter() {
        if let Some(personalization) = personalization_ctx {
            apply_personalization_boost_in_tiers(&mut result.documents, personalization);
        }
    }

    if params.should_window_for_personalization {
        let page_start = req.page.saturating_mul(params.hits_per_page);
        if page_start >= result.documents.len() {
            result.documents.clear();
        } else {
            let page_end = (page_start + params.hits_per_page).min(result.documents.len());
            result.documents = result.documents[page_start..page_end].to_vec();
        }
    }

    TransformOutputs::new(geo_distances, automatic_radius, search_phase_witness)
}

/// Applies click-through-rate re-ranking when `enableReRanking` is active, fetching
/// click counts from the analytics engine and adjusting document order by CTR signals.
fn apply_ctr_reranking(
    state: &Arc<AppState>,
    req: &SearchRequest,
    params: &PreparedSearchParams,
    result: &mut SearchResult,
    effective_index: &str,
    is_interleaving: bool,
) {
    let enable_re_ranking = req
        .enable_re_ranking
        .or(params
            .loaded_settings
            .as_ref()
            .and_then(|s| s.enable_re_ranking))
        .unwrap_or(true);

    if !enable_re_ranking || is_interleaving {
        return;
    }

    let analytics_engine = match state.analytics_engine.as_ref() {
        Some(e) => e,
        None => {
            tracing::debug!(
                "enableReRanking: analytics engine unavailable, keeping original ranking"
            );
            return;
        }
    };

    let object_ids: Vec<String> = result
        .documents
        .iter()
        .map(|doc| doc.document.id.clone())
        .collect();
    if object_ids.is_empty() {
        return;
    }

    // For virtual replicas, use the fully-resolved effective strictness (query param →
    // stored setting → 100). For non-virtual indices, pass only the explicit query param
    // so rerank_by_ctr() uses its own default (50) when no strictness is specified.
    let is_virtual_replica = is_virtual_settings_only_index(state, effective_index);
    let ctr_strictness = if is_virtual_replica {
        Some(params.effective_relevancy_strictness)
    } else {
        req.relevancy_strictness
    };

    apply_click_counts(
        analytics_engine,
        effective_index,
        &object_ids,
        result,
        req.re_ranking_apply_filter.as_deref(),
        ctr_strictness,
    );
}

/// Fetches per-object click counts from analytics and delegates to CTR re-ranking,
/// optionally scoped to documents matching `reRankingApplyFilter`.
fn apply_click_counts(
    analytics_engine: &flapjack::analytics::AnalyticsQueryEngine,
    effective_index: &str,
    object_ids: &[String],
    result: &mut SearchResult,
    re_ranking_apply_filter: Option<&str>,
    ctr_relevancy_strictness: Option<u32>,
) {
    let click_counts_result = tokio::runtime::Handle::try_current()
        .map_err(|err| format!("tokio runtime unavailable: {}", err))
        .and_then(|handle| {
            handle.block_on(
                analytics_engine.get_click_counts_for_objects(effective_index, object_ids),
            )
        });

    match click_counts_result {
        Ok(click_counts) if !click_counts.is_empty() => {
            let documents = std::mem::take(&mut result.documents);
            result.documents = apply_ctr_with_filter(
                documents,
                &click_counts,
                re_ranking_apply_filter,
                ctr_relevancy_strictness,
            );
        }
        Ok(_) => {
            tracing::debug!("enableReRanking: no click data available, keeping original ranking")
        }
        Err(err) => tracing::debug!(
            "enableReRanking: click lookup failed, keeping original ranking: {}",
            err
        ),
    }
}

/// Partitions documents by an optional filter, applies CTR re-ranking only to matching
/// documents, then appends non-matching documents to preserve their original order.
fn apply_ctr_with_filter(
    documents: Vec<ScoredDocument>,
    click_counts: &HashMap<String, u64>,
    re_ranking_apply_filter: Option<&str>,
    ctr_relevancy_strictness: Option<u32>,
) -> Vec<ScoredDocument> {
    let filter_str = match re_ranking_apply_filter {
        Some(f) => f,
        None => {
            return rerank_by_ctr(documents, click_counts, ctr_relevancy_strictness);
        }
    };

    match flapjack::filter_parser::parse_filter(filter_str) {
        Ok(filter) => {
            let (matching, non_matching): (Vec<ScoredDocument>, Vec<ScoredDocument>) = documents
                .into_iter()
                .partition(|doc| document_matches_filter(&doc.document, &filter));
            if matching.is_empty() {
                tracing::debug!("enableReRanking: reRankingApplyFilter matched no hits, keeping original ranking");
                non_matching
            } else {
                let mut reranked = rerank_by_ctr(matching, click_counts, ctr_relevancy_strictness);
                reranked.extend(non_matching);
                reranked
            }
        }
        Err(err) => {
            tracing::debug!(
                "enableReRanking: invalid reRankingApplyFilter '{}': {}",
                filter_str,
                err
            );
            documents
        }
    }
}

/// Filters and sorts results by geographic proximity, computing per-document distances
/// from the geo center and paginating the geo-filtered result set.
fn apply_geo_filtering(
    req: &SearchRequest,
    params: &PreparedSearchParams,
    result: &mut SearchResult,
    geo_params: &flapjack::query::geo::GeoParams,
) -> (GeoDistanceMap, Option<u64>) {
    let mut geo_distances: GeoDistanceMap = HashMap::new();
    let mut automatic_radius: Option<u64> = None;

    if !geo_params.has_geo_filter() {
        return (geo_distances, automatic_radius);
    }

    let mut geo_docs: Vec<(ScoredDocument, Option<f64>)> = result
        .documents
        .drain(..)
        .filter_map(|scored_doc| {
            let geoloc = scored_doc.document.fields.get("_geoloc");
            let points = extract_all_geolocs(geoloc);
            let (lat, lng) = best_geoloc_for_filter(&points, geo_params)?;
            let dist = geo_params.distance_from_center(lat, lng);
            if let Some(d) = dist {
                geo_distances.insert(scored_doc.document.id.clone(), (d, lat, lng));
            }
            Some((scored_doc, dist))
        })
        .collect();

    automatic_radius = compute_automatic_radius(&mut geo_docs, geo_params);
    sort_around_results(&mut geo_docs, geo_params);

    let total_geo = geo_docs.len();
    let start = (req.page * params.hits_per_page).min(total_geo);
    let end = (start + params.hits_per_page).min(total_geo);
    let docs: Vec<ScoredDocument> = geo_docs[start..end]
        .iter()
        .map(|(d, _)| d.clone())
        .collect();

    *result = SearchResult {
        documents: docs,
        total: total_geo,
        facets: std::mem::take(&mut result.facets),
        facets_stats: std::mem::take(&mut result.facets_stats),
        user_data: std::mem::take(&mut result.user_data),
        applied_rules: std::mem::take(&mut result.applied_rules),
        parsed_query: result.parsed_query.clone(),
        exhaustive_facet_values: result.exhaustive_facet_values,
        exhaustive_rules_match: result.exhaustive_rules_match,
        query_after_removal: result.query_after_removal.clone(),
        rendering_content: result.rendering_content.clone(),
        effective_around_lat_lng: result.effective_around_lat_lng.clone(),
        effective_around_radius: result.effective_around_radius.clone(),
    };

    (geo_distances, automatic_radius)
}

/// Computes an automatic geo radius when `aroundRadius` is not explicitly set,
/// using a density heuristic (first 1000 results) and `minimumAroundRadius` floor.
fn compute_automatic_radius(
    geo_docs: &mut Vec<(ScoredDocument, Option<f64>)>,
    geo_params: &flapjack::query::geo::GeoParams,
) -> Option<u64> {
    if !geo_params.has_around() || geo_params.around_radius.is_some() {
        return None;
    }
    sort_geo_docs_by_distance(geo_docs);
    let target_count = 1000.min(geo_docs.len());
    let density_radius = if target_count > 0 && target_count < geo_docs.len() {
        geo_docs[target_count - 1].1.unwrap_or(0.0) as u64
    } else {
        geo_docs
            .last()
            .and_then(|d| d.1)
            .map(|d| d as u64)
            .unwrap_or(0)
    };
    let effective = match geo_params.minimum_around_radius {
        Some(min_r) => density_radius.max(min_r),
        None => density_radius,
    };
    let effective_f = effective as f64;
    geo_docs.retain(|(_doc, dist)| dist.map(|d| d <= effective_f + 1.0).unwrap_or(false));
    Some(effective)
}

/// Sorts geo search results by distance from the `aroundLatLng` point.
fn sort_around_results(
    geo_docs: &mut [(ScoredDocument, Option<f64>)],
    geo_params: &flapjack::query::geo::GeoParams,
) {
    if !geo_params.has_around() {
        return;
    }
    if geo_params.around_precision.fixed.is_some() || !geo_params.around_precision.ranges.is_empty()
    {
        geo_docs.sort_by(|a, b| {
            let ba = geo_params
                .around_precision
                .bucket_distance(a.1.unwrap_or(f64::MAX));
            let bb = geo_params
                .around_precision
                .bucket_distance(b.1.unwrap_or(f64::MAX));
            ba.cmp(&bb)
        });
    } else {
        sort_geo_docs_by_distance(geo_docs);
    }
}

fn sort_geo_docs_by_distance(geo_docs: &mut [(ScoredDocument, Option<f64>)]) {
    geo_docs.sort_by(|a, b| {
        let da = a.1.unwrap_or(f64::MAX);
        let db = b.1.unwrap_or(f64::MAX);
        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Recomputes facet counts from the de-duplicated document set when
/// `facetingAfterDistinct` is enabled and distinct grouping is active.
fn recompute_facets_after_distinct(
    req: &SearchRequest,
    params: &PreparedSearchParams,
    result: &mut SearchResult,
) {
    let faceting_after_distinct = req.faceting_after_distinct.unwrap_or(false);
    if !faceting_after_distinct
        || !params.distinct_count.map(|d| d > 0).unwrap_or(false)
        || result.facets.is_empty()
    {
        return;
    }

    let mut recomputed: HashMap<String, Vec<flapjack::types::FacetCount>> = HashMap::new();
    let facet_fields: Vec<String> = result.facets.keys().cloned().collect();
    for field in &facet_fields {
        let mut counts: HashMap<String, u64> = HashMap::new();
        for doc in &result.documents {
            if let Some(fv) = doc.document.fields.get(field) {
                collect_facet_values(fv, &mut counts);
            }
        }
        let mut facet_counts: Vec<flapjack::types::FacetCount> = counts
            .into_iter()
            .map(|(path, count)| flapjack::types::FacetCount { path, count })
            .collect();
        facet_counts.sort_by(|a, b| b.count.cmp(&a.count).then(a.path.cmp(&b.path)));
        recomputed.insert(field.clone(), facet_counts);
    }
    result.facets = recomputed;
}
