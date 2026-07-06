use crate::dto::SearchRequest;
use flapjack::index::settings::IndexSettings;
use flapjack::index::DEFAULT_RELEVANCY_STRICTNESS;
use flapjack::types::{FacetRequest, Sort, SortOrder};

use super::personalization::PERSONALIZATION_RERANK_BUFFER;

#[cfg(test)]
use super::hybrid::HybridSearchInputs;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SearchWindow {
    pub(super) hits_per_page: usize,
    pub(super) limit: usize,
    pub(super) offset: usize,
    pub(super) should_window_for_personalization: bool,
}

/// Parses sort specifications (`field:asc`/`field:desc`) from the search request.
pub(super) fn resolve_search_sort(req: &SearchRequest) -> Option<Sort> {
    req.sort.as_ref().and_then(|sort_specs| {
        sort_specs.first().and_then(|first| {
            if first.ends_with(":asc") {
                Some(Sort::ByField {
                    field: first.trim_end_matches(":asc").to_string(),
                    order: SortOrder::Asc,
                })
            } else if first.ends_with(":desc") {
                Some(Sort::ByField {
                    field: first.trim_end_matches(":desc").to_string(),
                    order: SortOrder::Desc,
                })
            } else {
                None
            }
        })
    })
}

/// Builds the facet request list from the search request's `facets` param, expanding
/// `["*"]` to all configured facets and filtering against the index's allowed facet set.
pub(super) fn build_facet_requests(
    req: &SearchRequest,
    loaded_settings: Option<&IndexSettings>,
) -> Option<Vec<FacetRequest>> {
    let facets = req.facets.as_ref()?;
    let allowed_facets = loaded_settings.map(IndexSettings::facet_set);

    let effective_facets: Vec<String> = if facets.iter().any(|facet| facet == "*") {
        allowed_facets
            .as_ref()
            .map(|allowed| allowed.iter().cloned().collect())
            .unwrap_or_default()
    } else {
        facets
            .iter()
            .filter(|facet| {
                allowed_facets
                    .as_ref()
                    .is_none_or(|allowed| allowed.contains(facet.as_str()))
            })
            .cloned()
            .collect()
    };

    let facet_requests: Vec<FacetRequest> = effective_facets
        .into_iter()
        .map(|field| FacetRequest {
            path: format!("/{}", field),
            field,
            value_query: None,
        })
        .collect();

    if facet_requests.is_empty() {
        None
    } else {
        Some(facet_requests)
    }
}

/// Resolves the distinct deduplication count from the request or index settings.
pub(super) fn resolve_distinct_count(
    req: &SearchRequest,
    loaded_settings: Option<&IndexSettings>,
) -> Option<u32> {
    match &req.distinct {
        Some(serde_json::Value::Bool(true)) => loaded_settings
            .and_then(|settings| settings.distinct.as_ref())
            .map(|distinct| distinct.as_count())
            .or(Some(1)),
        Some(serde_json::Value::Bool(false)) => Some(0),
        Some(serde_json::Value::Number(number)) => number.as_u64().map(|value| value as u32),
        _ => loaded_settings
            .and_then(|settings| settings.distinct.as_ref())
            .map(|distinct| distinct.as_count()),
    }
}

pub(super) fn resolve_effective_relevancy_strictness(
    req: &SearchRequest,
    settings_override: Option<&IndexSettings>,
) -> u32 {
    req.relevancy_strictness
        .or_else(|| settings_override.and_then(|settings| settings.relevancy_strictness))
        .unwrap_or(DEFAULT_RELEVANCY_STRICTNESS)
}

/// Computes the internal search window size accounting for pagination, geo, and personalization.
pub(super) fn resolve_search_window(
    req: &SearchRequest,
    should_window_for_personalization: bool,
    has_geo_filter: bool,
    has_optional_filters: bool,
) -> SearchWindow {
    let hits_per_page = req.effective_hits_per_page();
    let (mut limit, mut offset) = if should_window_for_personalization {
        let limit = hits_per_page
            .saturating_mul(req.page.saturating_add(1))
            .saturating_add(PERSONALIZATION_RERANK_BUFFER);
        (limit.max(hits_per_page), 0)
    } else if has_geo_filter {
        (
            (hits_per_page + req.page * hits_per_page)
                .saturating_mul(10)
                .max(1000),
            0,
        )
    } else {
        (hits_per_page, req.page * hits_per_page)
    };

    if has_optional_filters && offset > 0 {
        limit = limit.saturating_add(offset);
        offset = 0;
    }

    SearchWindow {
        hits_per_page,
        limit,
        offset,
        should_window_for_personalization,
    }
}

pub(super) fn resolve_typo_tolerance(req: &SearchRequest) -> Option<bool> {
    match &req.typo_tolerance {
        Some(serde_json::Value::Bool(false)) => Some(false),
        Some(serde_json::Value::String(value)) if value == "false" => Some(false),
        _ => None,
    }
}

pub(super) fn resolve_optional_filter_groups(
    req: &SearchRequest,
) -> Option<Vec<Vec<(String, String, f32)>>> {
    req.optional_filters
        .as_ref()
        .map(crate::dto::parse_optional_filters_grouped)
        .filter(|groups| !groups.is_empty())
}

pub(super) fn apply_similar_query_override(req: &mut SearchRequest) -> bool {
    let Some(similar_query) = req.similar_query.take() else {
        return false;
    };

    req.query = similar_query;
    req.query_type_prefix = Some("prefixNone".to_string());
    req.remove_stop_words = Some(flapjack::query::stopwords::RemoveStopWordsValue::All);
    true
}

pub(super) fn normalize_query_languages(req: &mut SearchRequest) {
    if req.query_languages.is_none() {
        req.query_languages = req.natural_languages.take();
    }
}

#[test]
fn hybrid_search_inputs_reports_inactive_without_query_vector() {
    let query_vector = None;
    let hybrid_params = None;
    let inputs = HybridSearchInputs {
        query_vector: &query_vector,
        hybrid_params: &hybrid_params,
    };

    assert!(!inputs.is_hybrid_active());
}
#[test]
fn resolve_search_sort_parses_asc_and_desc_suffixes() {
    let req = SearchRequest {
        sort: Some(vec!["price:asc".to_string()]),
        ..Default::default()
    };
    assert!(matches!(
        resolve_search_sort(&req),
        Some(Sort::ByField {
            field,
            order: SortOrder::Asc,
        }) if field == "price"
    ));

    let req = SearchRequest {
        sort: Some(vec!["price:desc".to_string()]),
        ..Default::default()
    };
    assert!(matches!(
        resolve_search_sort(&req),
        Some(Sort::ByField {
            field,
            order: SortOrder::Desc,
        }) if field == "price"
    ));
}

#[test]
fn resolve_search_window_resets_offset_for_optional_filters() {
    let req = SearchRequest {
        hits_per_page: Some(20),
        page: 3,
        ..Default::default()
    };

    let window = resolve_search_window(&req, false, false, true);

    assert_eq!(window.limit, 80);
    assert_eq!(window.offset, 0);
}

#[test]
fn apply_similar_query_override_updates_query_defaults() {
    let mut req = SearchRequest {
        query: "ignored".to_string(),
        similar_query: Some("replacement".to_string()),
        ..Default::default()
    };

    assert!(apply_similar_query_override(&mut req));
    assert_eq!(req.query, "replacement");
    assert_eq!(req.query_type_prefix.as_deref(), Some("prefixNone"));
    assert_eq!(
        req.remove_stop_words,
        Some(flapjack::query::stopwords::RemoveStopWordsValue::All)
    );
}
