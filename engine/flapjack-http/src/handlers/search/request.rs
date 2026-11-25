use crate::dto::SearchRequest;

use super::experiments::ExperimentContext;

/// Extract userToken and client IP from request headers for analytics.
pub(super) fn extract_analytics_headers(
    request: &axum::extract::Request,
) -> (Option<String>, Option<String>, Option<String>) {
    let headers = request.headers();
    let user_token = headers
        .get("x-algolia-usertoken")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let user_ip = crate::middleware::extract_client_ip_opt(request).map(|ip| ip.to_string());
    let session_id = headers
        .get("x-algolia-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    (user_token, user_ip, session_id)
}

pub(super) struct SearchEventParams<'a> {
    pub req: &'a SearchRequest,
    pub query_id: Option<String>,
    pub index_name: String,
    pub nb_hits: usize,
    pub processing_time_ms: u32,
    pub page: usize,
    pub hits_per_page: usize,
    pub experiment_ctx: Option<&'a ExperimentContext>,
    pub country: Option<String>,
    pub region: Option<String>,
}

/// Builds an analytics `SearchEvent` from the completed search request parameters.
pub(super) fn build_search_event(
    params: &SearchEventParams<'_>,
) -> flapjack::analytics::schema::SearchEvent {
    let req = params.req;
    let analytics_tags = req.analytics_tags.as_ref().map(|tags| tags.join(","));
    let facets = req
        .facets
        .as_ref()
        .map(|facet_list| serde_json::to_string(facet_list).unwrap_or_default());
    flapjack::analytics::schema::SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: req.query.clone(),
        query_id: params.query_id.clone(),
        index_name: params.index_name.clone(),
        nb_hits: params.nb_hits as u32,
        processing_time_ms: params.processing_time_ms,
        user_token: req.user_token.clone(),
        user_ip: req.user_ip.clone(),
        filters: req.filters.clone(),
        facets,
        analytics_tags,
        page: params.page as u32,
        hits_per_page: params.hits_per_page as u32,
        has_results: params.nb_hits > 0,
        country: params.country.clone(),
        region: params.region.clone(),
        experiment_id: params.experiment_ctx.map(|ctx| ctx.experiment_id.clone()),
        variant_id: params.experiment_ctx.map(|ctx| ctx.variant_id.clone()),
        assignment_method: params
            .experiment_ctx
            .map(|ctx| ctx.assignment_method.clone()),
    }
}

/// Merges secured API key restrictions into the search request: AND-combines forced
/// filters, caps `hitsPerPage`, and overrides `userToken` if specified.
pub(super) fn merge_secured_filters(
    req: &mut SearchRequest,
    restrictions: &crate::auth::SecuredKeyRestrictions,
) {
    if let Some(ref forced_filters) = restrictions.filters {
        match &req.filters {
            Some(existing) => {
                req.filters = Some(format!("({}) AND ({})", existing, forced_filters));
            }
            None => {
                req.filters = Some(forced_filters.clone());
            }
        }
    }
    if let Some(hpp) = restrictions.hits_per_page {
        if req.hits_per_page.is_none_or(|h| h > hpp) {
            req.hits_per_page = Some(hpp);
        }
    }
    if let Some(ref forced_user_token) = restrictions.user_token {
        req.user_token = Some(forced_user_token.clone());
    }
}

/// Apply forced query parameters from the API key's `queryParameters` field.
/// Key params override user params (deterministic merge precedence).
/// Reuses `apply_params_string` to avoid duplicating param-parsing logic.
pub(super) fn apply_key_restrictions(
    req: &mut SearchRequest,
    api_key: &Option<crate::auth::ApiKey>,
) {
    let api_key = match api_key {
        Some(k) if !k.query_parameters.is_empty() => k,
        _ => return,
    };
    req.params = Some(api_key.query_parameters.clone());
    req.apply_params_string();
}

/// Compute the effective hits cap from both the API key's `maxHitsPerQuery` and
/// secured key restrictions' `hits_per_page`. Returns the most restrictive (minimum).
pub(super) fn compute_hits_cap(
    api_key: &Option<crate::auth::ApiKey>,
    secured_restrictions: &Option<crate::auth::SecuredKeyRestrictions>,
) -> Option<usize> {
    let key_cap = api_key
        .as_ref()
        .filter(|k| k.max_hits_per_query > 0)
        .map(|k| k.max_hits_per_query as usize);
    let secured_cap = secured_restrictions.as_ref().and_then(|r| r.hits_per_page);
    match (key_cap, secured_cap) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

pub(super) fn can_see_unretrievable_attributes(api_key: &Option<crate::auth::ApiKey>) -> bool {
    api_key.as_ref().is_some_and(|key| {
        key.acl
            .iter()
            .any(|acl| acl == "seeUnretrievableAttributes")
    })
}

#[derive(Clone, Copy)]
pub(super) struct ParamsEchoOptions {
    pub hits_per_page: Option<usize>,
    pub include_sort: bool,
    pub include_empty_facets: bool,
    pub include_attributes_to_retrieve: bool,
    pub include_attributes_to_highlight: bool,
}

fn push_encoded_param<F>(parts: &mut Vec<String>, key: &str, value: &str, encode: &F)
where
    F: Fn(&str) -> String,
{
    parts.push(format!("{key}={}", encode(value)));
}

fn push_json_param<F, T>(parts: &mut Vec<String>, key: &str, value: &T, encode: &F)
where
    F: Fn(&str) -> String,
    T: serde::Serialize,
{
    let json_value = serde_json::to_string(value).unwrap_or_default();
    push_encoded_param(parts, key, &json_value, encode);
}

/// Serializes search request parameters into a URL query string for response echo.
pub(super) fn build_params_string<F>(
    req: &SearchRequest,
    options: ParamsEchoOptions,
    encode: F,
) -> String
where
    F: Fn(&str) -> String,
{
    let mut parts = Vec::new();
    if !req.query.is_empty() {
        push_encoded_param(&mut parts, "query", &req.query, &encode);
    }
    if let Some(hits_per_page) = options.hits_per_page {
        parts.push(format!("hitsPerPage={hits_per_page}"));
    }
    if let Some(offset) = req.offset {
        parts.push(format!("offset={offset}"));
        parts.push(format!("length={}", req.effective_hits_per_page()));
    }
    if req.page > 0 {
        parts.push(format!("page={}", req.page));
    }
    if let Some(filters) = req.filters.as_deref() {
        push_encoded_param(&mut parts, "filters", filters, &encode);
    }
    if let Some(numeric_filters) = req.numeric_filters.as_ref() {
        let raw = numeric_filters_to_params_value(numeric_filters);
        push_encoded_param(&mut parts, "numericFilters", &raw, &encode);
    }
    if options.include_sort {
        if let Some(sort_fields) = req.sort.as_ref().filter(|fields| !fields.is_empty()) {
            let sort_value = sort_fields.join(",");
            push_encoded_param(&mut parts, "sort", &sort_value, &encode);
        }
    }
    if let Some(facets) = req
        .facets
        .as_ref()
        .filter(|facets| options.include_empty_facets || !facets.is_empty())
    {
        push_json_param(&mut parts, "facets", facets, &encode);
    }
    if options.include_attributes_to_retrieve {
        if let Some(attributes_to_retrieve) = req.attributes_to_retrieve.as_ref() {
            push_json_param(
                &mut parts,
                "attributesToRetrieve",
                attributes_to_retrieve,
                &encode,
            );
        }
    }
    if options.include_attributes_to_highlight {
        if let Some(attributes_to_highlight) = req.attributes_to_highlight.as_ref() {
            push_json_param(
                &mut parts,
                "attributesToHighlight",
                attributes_to_highlight,
                &encode,
            );
        }
    }
    if let Some(min_proximity) = req.min_proximity {
        parts.push(format!("minProximity={min_proximity}"));
    }
    if let Some(around_lat_lng) = req.around_lat_lng.as_deref() {
        push_encoded_param(&mut parts, "aroundLatLng", around_lat_lng, &encode);
    }
    if let Some(enable_re_ranking) = req.enable_re_ranking {
        parts.push(format!("enableReRanking={enable_re_ranking}"));
    }
    parts.join("&")
}

/// Build a URL-encoded params string echoing the query parameters for stub responses.
pub(crate) fn build_params_echo(req: &SearchRequest) -> String {
    build_params_string(
        req,
        ParamsEchoOptions {
            hits_per_page: req.hits_per_page,
            include_sort: false,
            include_empty_facets: true,
            include_attributes_to_retrieve: true,
            include_attributes_to_highlight: true,
        },
        |value| url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>(),
    )
}

pub(super) fn numeric_filters_to_params_value(numeric_filters: &serde_json::Value) -> String {
    match numeric_filters {
        serde_json::Value::String(s) => s.clone(),
        _ => serde_json::to_string(numeric_filters).unwrap_or_default(),
    }
}

/// Resolve the effective search mode from per-query override and index settings.
///
/// Priority: query mode > settings mode > KeywordSearch default.
pub fn resolve_search_mode(
    query_mode: &Option<flapjack::index::settings::IndexMode>,
    settings: &flapjack::index::settings::IndexSettings,
) -> flapjack::index::settings::IndexMode {
    use flapjack::index::settings::IndexMode;
    if let Some(mode) = query_mode {
        return mode.clone();
    }
    if let Some(mode) = &settings.mode {
        return mode.clone();
    }
    IndexMode::KeywordSearch
}
