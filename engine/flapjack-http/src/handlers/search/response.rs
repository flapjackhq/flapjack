//! Stub summary for response.rs.
use axum::Json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use flapjack::error::FlapjackError;
use flapjack::index::rules::merge_json_values;
use flapjack::query::highlighter::{extract_query_words, parse_snippet_spec, Highlighter};
use flapjack::types::field_value_to_json_value;

use crate::dto::SearchRequest;
use crate::handlers::AppState;

use super::experiments::ExperimentContext;
use super::geo::resolve_country_region_from_ip;
use super::highlight::{
    highlight_value_map_to_json, restrict_highlight_array, restrict_snippet_array,
    snippet_value_map_to_json,
};
use super::pipeline::{PreparedSearchParams, TransformOutputs};
use super::request::{
    build_params_string, build_search_event, ParamsEchoOptions, SearchEventParams,
};
use super::synonyms::map_synonym_matches;

pub(super) struct ResponseAssemblyContext<'a> {
    pub(super) state: &'a Arc<AppState>,
    pub(super) index_name: &'a str,
    pub(super) effective_index: &'a str,
    pub(super) queue_wait: Duration,
    pub(super) search_elapsed: Duration,
    pub(super) start: Instant,
    pub(super) query_id: Option<String>,
    pub(super) experiment_ctx: Option<ExperimentContext>,
    pub(super) fallback_message: Option<String>,
    pub(super) transform_outputs: TransformOutputs,
    pub(super) can_see_unretrievable_attributes: bool,
    pub(super) applied_relevancy_strictness: Option<u32>,
    pub(super) is_virtual_replica_search: bool,
}

fn build_response_params_string(req: &SearchRequest, hits_per_page: usize) -> String {
    build_params_string(
        req,
        ParamsEchoOptions {
            hits_per_page: Some(hits_per_page),
            include_sort: true,
            include_empty_facets: false,
            include_attributes_to_retrieve: false,
            include_attributes_to_highlight: false,
        },
        |value| urlencoding::encode(value).into_owned(),
    )
}

/// Expands the query word set with synonym matches from the index's synonym store,
/// building a synonym map for the highlighter to mark synonym-matched terms.
fn expand_query_words_with_synonyms(
    req: &SearchRequest,
    state: &Arc<AppState>,
    effective_index: &str,
    original_query_words: &[String],
) -> (Vec<String>, HashMap<String, HashSet<String>>) {
    let mut query_words: Vec<String> = original_query_words.to_vec();
    let mut synonym_map: HashMap<String, HashSet<String>> = HashMap::new();

    if !req.enable_synonyms.unwrap_or(true) {
        return (query_words, synonym_map);
    }
    let synonym_store = match state.manager.get_synonyms(effective_index) {
        Some(s) => s,
        None => return (query_words, synonym_map),
    };

    let expanded_queries = synonym_store.expand_query(&req.query);
    let mut all_words: HashSet<String> = query_words.iter().cloned().collect();
    for expanded in &expanded_queries {
        for word in extract_query_words(expanded) {
            all_words.insert(word);
        }
    }
    query_words = all_words.into_iter().collect();

    for original in original_query_words {
        let original_lower = original.to_lowercase();
        let mut synonyms = HashSet::new();
        synonyms.insert(original_lower.clone());
        for word in &query_words {
            let word_lower = word.to_lowercase();
            if !original_query_words
                .iter()
                .any(|o| o.to_lowercase() == word_lower)
            {
                synonyms.insert(word_lower);
            }
        }
        synonym_map.insert(original_lower, synonyms);
    }

    (query_words, synonym_map)
}

/// Constructs a `Highlighter` with custom pre/post tags and snippet ellipsis text
/// from request params or index settings.
fn build_highlighter(req: &SearchRequest, params: &PreparedSearchParams) -> Highlighter {
    let saved_settings = params.loaded_settings.as_deref();
    let pre_tag = req
        .highlight_pre_tag
        .as_ref()
        .or_else(|| saved_settings.and_then(|settings| settings.highlight_pre_tag.as_ref()));
    let post_tag = req
        .highlight_post_tag
        .as_ref()
        .or_else(|| saved_settings.and_then(|settings| settings.highlight_post_tag.as_ref()));
    let base = match (pre_tag, post_tag) {
        (Some(pre), Some(post)) => Highlighter::new(pre.clone(), post.clone()),
        _ => Highlighter::default(),
    };

    let ellipsis = req.snippet_ellipsis_text.as_ref().or_else(|| {
        params
            .loaded_settings
            .as_ref()
            .and_then(|s| s.snippet_ellipsis_text.as_ref())
    });

    match ellipsis {
        Some(e) => base.with_snippet_ellipsis(e.clone()),
        None => base,
    }
}

/// Builds the Algolia-compatible facet distribution map from raw facet counts,
/// applying alpha or count-based sorting per `sortFacetValuesBy`.
fn build_facet_distribution(
    req: &SearchRequest,
    result: &mut flapjack::types::SearchResult,
    params: &PreparedSearchParams,
) -> Option<std::collections::HashMap<String, serde_json::Value>> {
    req.facets.as_ref()?;
    if result.total == 0 || result.facets.is_empty() {
        return Some(std::collections::HashMap::new());
    }

    let sort_alpha = req
        .sort_facet_values_by
        .as_deref()
        .or(params
            .loaded_settings
            .as_ref()
            .and_then(|s| s.sort_facet_values_by.as_deref()))
        .unwrap_or("count")
        == "alpha";

    Some(
        std::mem::take(&mut result.facets)
            .into_iter()
            .map(|(field, mut counts)| {
                if sort_alpha {
                    counts.sort_by(|a, b| a.path.cmp(&b.path));
                }
                let facet_map: serde_json::Map<String, serde_json::Value> = counts
                    .into_iter()
                    .map(|fc| (fc.path, serde_json::json!(fc.count)))
                    .collect();
                (field, serde_json::Value::Object(facet_map))
            })
            .collect(),
    )
}

struct ResponseExtras<'a> {
    facet_distribution: Option<std::collections::HashMap<String, serde_json::Value>>,
    automatic_radius: Option<u64>,
    query_id: &'a Option<String>,
    fallback_message: &'a Option<String>,
    experiment_ctx: &'a Option<super::experiments::ExperimentContext>,
    effective_index: &'a str,
    index_name: &'a str,
    applied_relevancy_strictness: Option<u32>,
    is_virtual_replica_search: bool,
}

/// Returns whether the response should include the `nbSortedHits` field.
fn should_emit_nb_sorted_hits(
    params: &PreparedSearchParams,
    applied_relevancy_strictness: Option<u32>,
) -> bool {
    if params.sort.is_some() {
        return true;
    }

    let strictness = applied_relevancy_strictness.unwrap_or(100);
    let has_custom_ranking = params
        .loaded_settings
        .as_ref()
        .and_then(|settings| settings.custom_ranking.as_ref())
        .is_some_and(|specs| !specs.is_empty());
    strictness < 100 && has_custom_ranking
}

fn strictness_for_virtual_replica_extension(
    applied_relevancy_strictness: Option<u32>,
    is_virtual_replica_search: bool,
) -> Option<u32> {
    let strictness = applied_relevancy_strictness?;
    (is_virtual_replica_search && strictness < 100).then_some(strictness)
}

/// Attaches optional fields to the JSON response: parsed query, facets, facet stats,
/// user data, query ID, A/B test metadata, geo radius, applied rules, and `responseFields` filtering.
fn apply_optional_response_fields(
    response: &mut serde_json::Value,
    result: &flapjack::types::SearchResult,
    req: &SearchRequest,
    params: &PreparedSearchParams,
    extras: ResponseExtras<'_>,
) {
    let ResponseExtras {
        facet_distribution,
        automatic_radius,
        query_id,
        fallback_message,
        experiment_ctx,
        effective_index,
        index_name,
        applied_relevancy_strictness,
        is_virtual_replica_search,
    } = extras;
    if let Some(ref qar) = result.query_after_removal {
        response["queryAfterRemoval"] = serde_json::json!(qar);
    } else {
        response["parsedQuery"] = serde_json::json!(result.parsed_query);
    }

    if should_emit_nb_sorted_hits(params, applied_relevancy_strictness) {
        response["nbSortedHits"] = serde_json::json!(result.total);
    }
    if let Some(strictness) = strictness_for_virtual_replica_extension(
        applied_relevancy_strictness,
        is_virtual_replica_search,
    ) {
        if response_field_is_requested(req.response_fields.as_ref(), "appliedRelevancyStrictness") {
            response["appliedRelevancyStrictness"] = serde_json::json!(strictness);
        }
    }
    if req.facets.is_some() {
        response["exhaustiveFacetsCount"] = serde_json::json!(true);
    }

    match facet_distribution {
        Some(facets) if facets.is_empty() => {
            response["facets"] = serde_json::json!({});
        }
        Some(facets) => {
            response["facets"] = serde_json::Value::Object(facets.into_iter().collect());
        }
        None => {}
    }

    if req.facets.is_some() {
        let stats_obj: serde_json::Map<String, serde_json::Value> = result
            .facets_stats
            .iter()
            .map(|(field, stats)| {
                (field.clone(), serde_json::json!({"min": stats.min, "max": stats.max, "avg": stats.avg, "sum": stats.sum}))
            })
            .collect();
        response["facets_stats"] = serde_json::Value::Object(stats_obj);
    }

    if let Some(settings_user_data) = params
        .loaded_settings
        .as_ref()
        .and_then(|s| s.redacted_user_data())
    {
        response["userData"] = settings_user_data;
    }
    if !result.user_data.is_empty() {
        response["userData"] = serde_json::json!(result.user_data);
    }
    if let Some(auto_r) = automatic_radius {
        response["automaticRadius"] = serde_json::json!(auto_r.to_string());
    }
    if !result.applied_rules.is_empty() {
        response["appliedRules"] = serde_json::Value::Array(
            result
                .applied_rules
                .iter()
                .map(|id| serde_json::json!({"objectID": id}))
                .collect(),
        );
    }
    if let Some(ref qid) = query_id {
        response["queryID"] = serde_json::json!(qid);
    }
    if let Some(ref msg) = fallback_message {
        response["message"] = serde_json::json!(msg);
    }

    if let Some(ref fields) = req.response_fields {
        if !fields.contains(&"*".to_string()) {
            let response_obj = response.as_object_mut().unwrap();
            let keys: Vec<String> = response_obj.keys().cloned().collect();
            for key in keys {
                if !fields.contains(&key) {
                    response_obj.remove(&key);
                }
            }
        }
    }

    if let Some(ref ctx) = experiment_ctx {
        if response_field_is_requested(req.response_fields.as_ref(), "abTestID") {
            response["abTestID"] = serde_json::json!(ctx.experiment_id);
        }
        response["abTestVariantID"] = serde_json::json!(ctx.variant_id);
        if let Some(ref interleaved_teams) = ctx.interleaved_teams {
            if response_field_is_requested(req.response_fields.as_ref(), "interleavedTeams") {
                response["interleavedTeams"] = serde_json::to_value(interleaved_teams).unwrap();
            }
        }
    }
    if effective_index != index_name
        && response_field_is_requested(req.response_fields.as_ref(), "indexUsed")
    {
        response["indexUsed"] = serde_json::json!(effective_index);
    }
}

fn response_field_is_requested(response_fields: Option<&Vec<String>>, key: &str) -> bool {
    match response_fields {
        None => true,
        Some(fields) => fields.iter().any(|field| field == "*" || field == key),
    }
}

/// Build the final JSON response from search results, applying highlighting,
/// facet formatting, and assembling the Algolia-compatible response envelope.
/// Also records analytics and increments usage counters (fire-and-forget side effects).
pub(super) fn format_search_response(
    mut result: flapjack::types::SearchResult,
    req: &SearchRequest,
    params: &PreparedSearchParams,
    context: ResponseAssemblyContext<'_>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let ResponseAssemblyContext {
        state,
        index_name,
        effective_index,
        queue_wait,
        search_elapsed,
        start,
        query_id,
        experiment_ctx,
        fallback_message,
        transform_outputs,
        can_see_unretrievable_attributes,
        applied_relevancy_strictness,
        is_virtual_replica_search,
    } = context;

    let TransformOutputs {
        geo_distances,
        automatic_radius,
        ..
    } = transform_outputs;

    let original_query_words = extract_query_words(&req.query);
    let (query_words, synonym_map) =
        expand_query_words_with_synonyms(req, state, effective_index, &original_query_words);
    let highlighter = build_highlighter(req, params);

    let highlight_start = Instant::now();
    let hit_context = HitBuildContext {
        highlighter: &highlighter,
        query_words: &query_words,
        original_query_words: &original_query_words,
        synonym_map: &synonym_map,
        geo_distances: &geo_distances,
    };
    let hits: Vec<serde_json::Value> = result
        .documents
        .iter()
        .map(|doc| {
            build_hit_object(
                doc,
                req,
                params,
                &hit_context,
                can_see_unretrievable_attributes,
            )
        })
        .collect();
    let highlight_elapsed = highlight_start.elapsed();

    let facet_distribution = build_facet_distribution(req, &mut result, params);

    let hits_per_page = req.effective_hits_per_page();
    let nb_pages = if result.total > 0 && hits_per_page > 0 {
        result.total.div_ceil(hits_per_page)
    } else {
        0
    };
    let total_elapsed = start.elapsed();

    let mut rendering_content = params
        .loaded_settings
        .as_ref()
        .and_then(|s| s.rendering_content.clone())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(rc) = &result.rendering_content {
        merge_json_values(&mut rendering_content, rc);
    }

    let mut response = serde_json::json!({
        "hits": hits,
        "nbHits": result.total,
        "page": req.page,
        "nbPages": nb_pages,
        "hitsPerPage": hits_per_page,
        "processingTimeMS": total_elapsed.as_millis() as u64,
        "serverTimeMS": total_elapsed.as_millis() as u64,
        "query": req.query,
        "params": build_response_params_string(req, hits_per_page),
        "exhaustive": {
            "nbHits": true, "typo": true,
            "facetValues": if req.facets.is_some() { result.exhaustive_facet_values } else { true },
            "rulesMatch": result.exhaustive_rules_match
        },
        "exhaustiveNbHits": true,
        "exhaustiveTypo": true,
        "index": index_name,
        "renderingContent": rendering_content,
        "serverUsed": hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
        "_automaticInsights": false,
        "processingTimingsMS": {
            "queue": queue_wait.as_micros() as u64,
            "search": search_elapsed.as_micros() as u64,
            "highlight": highlight_elapsed.as_micros() as u64,
            "total": total_elapsed.as_micros() as u64
        }
    });

    if req.facets.is_some() {
        response["exhaustive"]["facetsCount"] = serde_json::json!(true);
    }

    apply_optional_response_fields(
        &mut response,
        &result,
        req,
        params,
        ResponseExtras {
            facet_distribution,
            automatic_radius,
            query_id: &query_id,
            fallback_message: &fallback_message,
            experiment_ctx: &experiment_ctx,
            effective_index,
            index_name,
            applied_relevancy_strictness,
            is_virtual_replica_search,
        },
    );

    // Increment usage counter: search_results_total
    {
        let entry = state
            .usage_counters
            .entry(effective_index.to_string())
            .or_default();
        entry
            .search_results_total
            .fetch_add(result.total as u64, std::sync::atomic::Ordering::Relaxed);
    }

    // Record analytics event (fire-and-forget, never blocks search response)
    if req.analytics != Some(false) {
        if let Some(collector) = flapjack::analytics::get_global_collector() {
            let (country, region) =
                resolve_country_region_from_ip(&req.user_ip, &state.geoip_reader);
            collector.record_search(build_search_event(&SearchEventParams {
                req,
                query_id: query_id.clone(),
                index_name: effective_index.to_string(),
                nb_hits: result.total,
                processing_time_ms: total_elapsed.as_millis() as u32,
                page: req.page,
                hits_per_page,
                experiment_ctx: experiment_ctx.as_ref(),
                country,
                region,
            }));
        }
    }

    Ok(Json(response))
}

struct HitBuildContext<'a> {
    highlighter: &'a Highlighter,
    query_words: &'a [String],
    original_query_words: &'a [String],
    synonym_map: &'a HashMap<String, HashSet<String>>,
    geo_distances: &'a HashMap<String, (f64, f64, f64)>,
}

/// Builds a single Algolia-compatible hit JSON object from a scored document,
/// applying field retrieval, highlighting, snippeting, ranking info, and geo distance.
fn build_hit_object(
    scored_doc: &flapjack::types::ScoredDocument,
    req: &SearchRequest,
    params: &PreparedSearchParams,
    ctx: &HitBuildContext<'_>,
    can_see_unretrievable_attributes: bool,
) -> serde_json::Value {
    let mut doc_map = serde_json::Map::new();
    doc_map.insert(
        "objectID".to_string(),
        serde_json::Value::String(scored_doc.document.id.clone()),
    );

    for (key, value) in &scored_doc.document.fields {
        if let Some(ref settings) = params.loaded_settings {
            if !settings.should_retrieve_with_acl(key, can_see_unretrievable_attributes) {
                continue;
            }
        }
        if let Some(ref attrs) = req.attributes_to_retrieve {
            if !attrs.contains(key) && !attrs.iter().any(|a| a == "*") {
                continue;
            }
        }
        doc_map.insert(key.clone(), field_value_to_json_value(value));
    }

    let restrict_arrays = req
        .restrict_highlight_and_snippet_arrays
        .or(params
            .loaded_settings
            .as_ref()
            .and_then(|s| s.restrict_highlight_and_snippet_arrays))
        .unwrap_or(false);

    build_highlight_result(
        &mut doc_map,
        scored_doc,
        req,
        params,
        ctx,
        restrict_arrays,
        can_see_unretrievable_attributes,
    );
    build_snippet_result(
        &mut doc_map,
        scored_doc,
        req,
        params,
        ctx,
        restrict_arrays,
        can_see_unretrievable_attributes,
    );

    if req.get_ranking_info == Some(true) {
        doc_map.insert(
            "_rankingInfo".to_string(),
            build_ranking_info(scored_doc, params, ctx.geo_distances),
        );
    }

    serde_json::Value::Object(doc_map)
}

/// Builds the `_highlightResult` map for a document's matched attributes.
fn build_highlight_result(
    doc_map: &mut serde_json::Map<String, serde_json::Value>,
    scored_doc: &flapjack::types::ScoredDocument,
    req: &SearchRequest,
    params: &PreparedSearchParams,
    ctx: &HitBuildContext<'_>,
    restrict_arrays: bool,
    can_see_unretrievable_attributes: bool,
) {
    let attributes_to_highlight = req.attributes_to_highlight.as_ref().or_else(|| {
        params
            .loaded_settings
            .as_ref()
            .and_then(|settings| settings.attributes_to_highlight.as_ref())
    });
    if matches!(attributes_to_highlight, Some(attributes) if attributes.is_empty()) {
        return;
    }

    let mut highlight_map = ctx
        .highlighter
        .highlight_document(&scored_doc.document, ctx.query_words);
    if let Some(attributes) = attributes_to_highlight {
        highlight_map.retain(|attribute, _| {
            attributes
                .iter()
                .any(|selected| selected == "*" || selected == attribute)
        });
    }

    let replace_synonyms = req
        .replace_synonyms_in_highlight
        .or(params
            .loaded_settings
            .as_ref()
            .and_then(|s| s.replace_synonyms_in_highlight))
        .unwrap_or(false);
    if !replace_synonyms && !ctx.synonym_map.is_empty() {
        highlight_map = highlight_map
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    map_synonym_matches(v, ctx.original_query_words, ctx.synonym_map),
                )
            })
            .collect();
    }

    if let Some(ref settings) = params.loaded_settings {
        highlight_map.retain(|attr, _| {
            settings.should_retrieve_with_acl(attr, can_see_unretrievable_attributes)
        });
    }

    if restrict_arrays {
        highlight_map = highlight_map
            .into_iter()
            .map(|(k, v)| (k, restrict_highlight_array(v)))
            .collect();
    }

    doc_map.insert(
        "_highlightResult".to_string(),
        highlight_value_map_to_json(&highlight_map),
    );
}

/// Builds the `_snippetResult` map with truncated highlighted excerpts.
fn build_snippet_result(
    doc_map: &mut serde_json::Map<String, serde_json::Value>,
    scored_doc: &flapjack::types::ScoredDocument,
    req: &SearchRequest,
    params: &PreparedSearchParams,
    ctx: &HitBuildContext<'_>,
    restrict_arrays: bool,
    can_see_unretrievable_attributes: bool,
) {
    let effective_snippet_attributes = req.attributes_to_snippet.as_ref().or_else(|| {
        params
            .loaded_settings
            .as_ref()
            .and_then(|settings| settings.attributes_to_snippet.as_ref())
    });
    let snippet_attrs = match effective_snippet_attributes {
        Some(attrs) if !attrs.is_empty() => attrs,
        _ => return,
    };

    let snippet_specs: Vec<(&str, usize)> = snippet_attrs
        .iter()
        .map(|s| parse_snippet_spec(s.as_str()))
        .collect();
    let mut snippet_map =
        ctx.highlighter
            .snippet_document(&scored_doc.document, ctx.query_words, &snippet_specs);

    if let Some(ref settings) = params.loaded_settings {
        snippet_map.retain(|attr, _| {
            settings.should_retrieve_with_acl(attr, can_see_unretrievable_attributes)
        });
    }
    if restrict_arrays {
        snippet_map = snippet_map
            .into_iter()
            .map(|(k, v)| (k, restrict_snippet_array(v)))
            .collect();
    }

    doc_map.insert(
        "_snippetResult".to_string(),
        snippet_value_map_to_json(&snippet_map),
    );
}

/// Builds `_rankingInfo` metadata (scores, geo distance, matched words) for a hit.
fn build_ranking_info(
    scored_doc: &flapjack::types::ScoredDocument,
    params: &PreparedSearchParams,
    geo_distances: &HashMap<String, (f64, f64, f64)>,
) -> serde_json::Value {
    let mut info = serde_json::json!({
        "nbTypos": 0,
        "firstMatchedWord": 0,
        "proximityDistance": 0,
        "userScore": 0,
        "geoDistance": 0,
        "geoPrecision": 1,
        "nbExactWords": 0,
        "words": 0,
        "filters": 0
    });
    if let Some(&(dist, lat, lng)) = geo_distances.get(&scored_doc.document.id) {
        let precision = if params.geo_params.around_precision.fixed.is_some()
            || !params.geo_params.around_precision.ranges.is_empty()
        {
            let bucket = params.geo_params.around_precision.bucket_distance(dist);
            (dist as u64).checked_div(bucket).unwrap_or(1)
        } else {
            1
        };
        info["geoDistance"] = serde_json::json!((dist as u64) / precision.max(1));
        info["geoPrecision"] = serde_json::json!(precision);
        info["matchedGeoLocation"] = serde_json::json!({
            "lat": lat, "lng": lng, "distance": dist as u64
        });
    }
    info
}

#[cfg(test)]
mod tests {
    use super::build_response_params_string;
    use crate::dto::SearchRequest;
    use serde_json::json;
    /// TODO: Document response_params_string_includes_encoded_core_fields.
    #[test]
    fn response_params_string_includes_encoded_core_fields() {
        let req = SearchRequest {
            query: "red shoes".to_string(),
            filters: Some("brand:Acme".to_string()),
            numeric_filters: Some(json!(["price>10"])),
            sort: Some(vec!["price:desc".to_string()]),
            facets: Some(vec!["brand".to_string(), "color".to_string()]),
            min_proximity: Some(2),
            enable_re_ranking: Some(true),
            ..Default::default()
        };

        let params = build_response_params_string(&req, 20);

        assert!(params.contains("query=red%20shoes"));
        assert!(params.contains("hitsPerPage=20"));
        assert!(params.contains("filters=brand%3AAcme"));
        assert!(params.contains("numericFilters=%5B%22price%3E10%22%5D"));
        assert!(params.contains("sort=price%3Adesc"));
        assert!(params.contains("facets=%5B%22brand%22%2C%22color%22%5D"));
        assert!(params.contains("minProximity=2"));
        assert!(params.contains("enableReRanking=true"));
    }

    #[test]
    fn response_params_string_omits_empty_query_and_zero_page() {
        let req = SearchRequest {
            page: 0,
            ..Default::default()
        };

        let params = build_response_params_string(&req, 10);

        assert_eq!(params, "hitsPerPage=10");
    }
}
