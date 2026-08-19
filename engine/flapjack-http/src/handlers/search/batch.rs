//! Stub summary for batch.rs.
use std::sync::Arc;

use axum::{extract::State, Json};

use flapjack::error::FlapjackError;

use super::request::{
    apply_key_restrictions, build_params_echo, can_see_unretrievable_attributes, compute_hits_cap,
    extract_analytics_headers, merge_secured_filters,
};
use super::single::search_single_with_secured_hits_per_page_cap;
use crate::dto::{BatchSearchRequest, SearchRequest};
use crate::federation::{merge_federated_results, FederationCandidate, FederationConfig};
use crate::handlers::AppState;

struct PreparedBatchQuery {
    position: usize,
    index_name: String,
    request: SearchRequest,
    secured_hits_per_page_cap: Option<usize>,
    can_see_unretrievable_attributes: bool,
}

struct FederationQueryMetadata {
    index_name: String,
    queries_position: usize,
    weight: f64,
}

fn validate_batch_query_type(query_type: Option<&str>) -> Result<(), FlapjackError> {
    match query_type {
        None | Some("default") | Some("facet") => Ok(()),
        Some(query_type) => Err(FlapjackError::InvalidQuery(format!(
            "Invalid query type: '{}'. Valid values are 'default' and 'facet'.",
            query_type
        ))),
    }
}

fn skipped_batch_response(index_name: &str, request: &SearchRequest) -> serde_json::Value {
    serde_json::json!({
        "hits": [],
        "nbHits": 0,
        "page": 0,
        "nbPages": 0,
        "hitsPerPage": 0,
        "processingTimeMS": 1,
        "params": build_params_echo(request),
        "index": index_name,
        "processed": false
    })
}

fn effective_federation_weight(request: &SearchRequest) -> f64 {
    request
        .federation_options
        .as_ref()
        .map(|options| options.weight)
        .unwrap_or(1.0)
}

fn federation_fetch_hits_per_page(federation: &FederationConfig, hits_cap: Option<usize>) -> usize {
    let federation_window = federation.offset.saturating_add(federation.limit).max(1);

    hits_cap
        .map(|cap| federation_window.min(cap))
        .unwrap_or(federation_window)
}

fn normalize_request_for_federation(
    request: &mut SearchRequest,
    federation: &FederationConfig,
    hits_cap: Option<usize>,
) {
    // Federated ranking always starts from each query's top results. Ignore
    // per-query responseFields because the merge path requires hits + nbHits.
    request.page = 0;
    request.hits_per_page = Some(federation_fetch_hits_per_page(federation, hits_cap));
    request.response_fields = None;
}

/// Validates federation config constraints: rejects `stopIfEnoughMatches` strategy
/// and unsupported facet merging when federation is enabled.
fn validate_batch_federation_options(
    federation: Option<&FederationConfig>,
    stop_if_enough: bool,
) -> Result<(), FlapjackError> {
    let Some(federation_config) = federation else {
        return Ok(());
    };

    if stop_if_enough {
        return Err(FlapjackError::InvalidQuery(
            "strategy=stopIfEnoughMatches is not supported with federation".to_string(),
        ));
    }

    if federation_config.merge_facets.is_some() {
        return Err(FlapjackError::InvalidQuery(
            "Facet merging in federated search is not yet supported".to_string(),
        ));
    }

    Ok(())
}

fn collect_federation_query_metadata(
    prepared: &[PreparedBatchQuery],
) -> Vec<FederationQueryMetadata> {
    prepared
        .iter()
        .map(|query| FederationQueryMetadata {
            index_name: query.index_name.clone(),
            queries_position: query.position,
            weight: effective_federation_weight(&query.request),
        })
        .collect()
}

/// Assembles the federated search response by collecting hits from all queries into
/// ranked candidates, merging via `merge_federated_results`, and serializing to JSON.
fn build_federated_response_value(
    query_results: Vec<serde_json::Value>,
    query_metadata: Vec<FederationQueryMetadata>,
    federation_config: FederationConfig,
    processing_time_ms: u128,
) -> Result<serde_json::Value, FlapjackError> {
    if query_results.len() != query_metadata.len() {
        return Err(FlapjackError::InvalidQuery(
            "Federated search failed: result count mismatch".to_string(),
        ));
    }

    let mut candidates = Vec::new();
    let mut estimated_total_hits_per_query = Vec::with_capacity(query_results.len());

    for (result, metadata) in query_results.into_iter().zip(query_metadata) {
        let estimated_total_hits = result
            .get("nbHits")
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as usize;
        estimated_total_hits_per_query.push(estimated_total_hits);

        let hits = result
            .get("hits")
            .and_then(|value| value.as_array())
            .ok_or_else(|| {
                FlapjackError::InvalidQuery(
                    "Federated search failed: each query result must include a hits array"
                        .to_string(),
                )
            })?;

        for (rank_in_index, hit) in hits.iter().enumerate() {
            candidates.push(FederationCandidate {
                hit: hit.clone(),
                index_name: metadata.index_name.clone(),
                queries_position: metadata.queries_position,
                rank_in_index,
                weight: metadata.weight,
            });
        }
    }

    let mut merged = merge_federated_results(
        candidates,
        estimated_total_hits_per_query,
        federation_config,
    );
    merged.processing_time_ms = Some(processing_time_ms as u64);
    serde_json::to_value(merged)
        .map_err(|error| FlapjackError::InvalidQuery(format!("Federated response error: {error}")))
}

#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/queries",
    tag = "search",
    params(
        ("indexName" = String, Path, description = "Index to search")
    ),
    request_body(
        content = crate::dto::BatchSearchRequest,
        description = "Batch search request with multiple queries"
    ),
    responses(
        (status = 200, description = "Batch search results", body = crate::dto::BatchSearchResponse),
        (status = 404, description = "Index not found")
    ),
    security(
        ("api_key" = [])
    )
)]
/// Execute multiple search queries in a single request, optionally merging results via federation.
pub async fn batch_search(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let secured_restrictions = request
        .extensions()
        .get::<crate::auth::SecuredKeyRestrictions>()
        .cloned();
    let api_key = request.extensions().get::<crate::auth::ApiKey>().cloned();
    let paid_beta_v1_customer = request
        .extensions()
        .get::<crate::api_profile::PaidBetaV1CustomerRequest>()
        .is_some();
    let dictionary_lookup_tenant = request
        .extensions()
        .get::<crate::auth::AuthenticatedAppId>()
        .map(|id| id.0.clone());
    let (user_token_header, user_ip, session_id_header) = extract_analytics_headers(&request);
    let body_bytes = axum::body::to_bytes(request.into_body(), 10_000_000)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Failed to read body: {}", e)))?;
    let body: serde_json::Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| FlapjackError::InvalidQuery(format!("Invalid JSON: {}", e)))?;
    let batch: BatchSearchRequest = if paid_beta_v1_customer {
        crate::api_profile::prepare_paid_beta_v1_batch(body, api_key.as_ref())?
    } else {
        serde_json::from_value(body).map_err(|e| {
            tracing::error!("Batch search deserialization failed: {}", e);
            FlapjackError::InvalidQuery(format!("Invalid batch search: {}", e))
        })?
    };

    if batch.requests.len() > crate::dto::MAX_BATCH_SEARCH_QUERIES {
        return Err(FlapjackError::InvalidQuery(format!(
            "Batch search exceeds maximum of {} queries (got {})",
            crate::dto::MAX_BATCH_SEARCH_QUERIES,
            batch.requests.len()
        )));
    }

    // Validate strategy value
    match batch.strategy.as_deref() {
        None | Some("none") | Some("stopIfEnoughMatches") => {}
        Some(s) => {
            return Err(FlapjackError::InvalidQuery(format!(
                "Invalid strategy: '{}'. Valid values are 'none' and 'stopIfEnoughMatches'.",
                s
            )));
        }
    }
    let stop_if_enough = batch.strategy.as_deref() == Some("stopIfEnoughMatches");
    validate_batch_federation_options(batch.federation.as_ref(), stop_if_enough)?;
    let federation = batch.federation.clone();

    // Validate all requests up front.
    let hits_cap = compute_hits_cap(&api_key, &secured_restrictions);
    let can_see_unretrievable = can_see_unretrievable_attributes(&api_key);
    let mut prepared = Vec::new();
    for (position, mut request) in batch.requests.into_iter().enumerate() {
        request.apply_params_string();
        apply_key_restrictions(&mut request, &api_key);
        if request.user_token.is_none() {
            request.user_token = user_token_header.clone();
        }
        if request.session_id.is_none() {
            request.session_id = session_id_header.clone();
        }
        request.user_ip = user_ip.clone();
        // Enforce the shared index-authorization rules per query because the
        // middleware only authorizes the wildcard batch route itself.
        if let Some(ref restrictions) = secured_restrictions {
            merge_secured_filters(&mut request, restrictions);
        }
        if let (Some(key), Some(index_name)) = (api_key.as_ref(), request.index_name.as_ref()) {
            if !crate::auth::key_allows_index(key, secured_restrictions.as_ref(), index_name) {
                return Err(crate::auth::invalid_api_credentials_flapjack_error());
            }
        }
        request.validate()?;
        // Validate query type up-front so a bad type in query N rejects the
        // whole batch before any queries begin executing.
        validate_batch_query_type(request.query_type.as_deref())?;
        if federation.is_some() && request.query_type.as_deref() == Some("facet") {
            return Err(FlapjackError::InvalidQuery(
                "Facet queries (type=facet) are not supported with federation".to_string(),
            ));
        }
        if let Some(federation_config) = federation.as_ref() {
            normalize_request_for_federation(&mut request, federation_config, hits_cap);
        }
        let index_name = request
            .index_name
            .clone()
            .ok_or_else(|| FlapjackError::InvalidQuery("Missing indexName".to_string()))?;
        prepared.push(PreparedBatchQuery {
            position,
            index_name,
            request,
            secured_hits_per_page_cap: hits_cap,
            can_see_unretrievable_attributes: can_see_unretrievable,
        });
    }

    let start = std::time::Instant::now();
    if let Some(federation_config) = federation {
        let metadata = collect_federation_query_metadata(&prepared);
        let query_results =
            execute_batch_parallel(state, prepared, dictionary_lookup_tenant).await?;
        let processing_time_ms = start.elapsed().as_millis();
        let response = build_federated_response_value(
            query_results,
            metadata,
            federation_config,
            processing_time_ms,
        )?;
        return Ok(Json(response));
    }

    let results = if stop_if_enough {
        execute_batch_stop_if_enough(state, prepared, dictionary_lookup_tenant).await?
    } else {
        execute_batch_parallel(state, prepared, dictionary_lookup_tenant).await?
    };

    Ok(Json(serde_json::json!({"results": results})))
}

/// Executes a single search query within a multi-query batch request.
async fn execute_single_batch_query(
    state: Arc<AppState>,
    index_name: String,
    req: SearchRequest,
    secured_hits_per_page_cap: Option<usize>,
    can_see_unretrievable_attributes: bool,
    dictionary_lookup_tenant: Option<String>,
) -> Result<serde_json::Value, FlapjackError> {
    if req.query_type.as_deref() == Some("facet") {
        let facet_name = req.facet.clone().unwrap_or_default();
        let facet_query = req.facet_query.clone().unwrap_or_default();
        let max_facet_hits = req.max_facet_hits.unwrap_or(10);
        let filters = req.filters.clone();
        return crate::handlers::facets::search_facet_values_inline(
            state,
            &index_name,
            &facet_name,
            &facet_query,
            max_facet_hits,
            filters.as_deref(),
            req.sort_facet_values_by.as_deref(),
        )
        .await;
    }

    let result = search_single_with_secured_hits_per_page_cap(
        State(state),
        index_name,
        req,
        secured_hits_per_page_cap,
        can_see_unretrievable_attributes,
        dictionary_lookup_tenant,
    )
    .await?;
    Ok(result.0)
}

/// Execute all batch queries in parallel (strategy=none or default).
async fn execute_batch_parallel(
    state: Arc<AppState>,
    prepared: Vec<PreparedBatchQuery>,
    dictionary_lookup_tenant: Option<String>,
) -> Result<Vec<serde_json::Value>, FlapjackError> {
    let mut join_set = tokio::task::JoinSet::new();
    for query in prepared {
        let PreparedBatchQuery {
            position,
            index_name,
            request,
            secured_hits_per_page_cap,
            can_see_unretrievable_attributes,
        } = query;
        let state = state.clone();
        let dictionary_lookup_tenant = dictionary_lookup_tenant.clone();
        join_set.spawn(async move {
            let result = execute_single_batch_query(
                state,
                index_name,
                request,
                secured_hits_per_page_cap,
                can_see_unretrievable_attributes,
                dictionary_lookup_tenant,
            )
            .await?;
            Ok::<_, FlapjackError>((position, result))
        });
    }

    let mut indexed_results: Vec<(usize, serde_json::Value)> = Vec::with_capacity(join_set.len());
    while let Some(join_result) = join_set.join_next().await {
        let result = join_result
            .map_err(|e| FlapjackError::InvalidQuery(format!("Task join error: {}", e)))?;
        indexed_results.push(result?);
    }
    indexed_results.sort_by_key(|(i, _)| *i);
    Ok(indexed_results.into_iter().map(|(_, v)| v).collect())
}

/// Execute batch queries sequentially with early-stop (strategy=stopIfEnoughMatches).
/// Once a query returns >= hitsPerPage hits, remaining queries are skipped and
/// receive a stub response with `processed: false`.
async fn execute_batch_stop_if_enough(
    state: Arc<AppState>,
    prepared: Vec<PreparedBatchQuery>,
    dictionary_lookup_tenant: Option<String>,
) -> Result<Vec<serde_json::Value>, FlapjackError> {
    let total = prepared.len();
    let mut results: Vec<serde_json::Value> = Vec::with_capacity(total);
    let mut enough_found = false;

    for query in prepared {
        if enough_found {
            results.push(skipped_batch_response(&query.index_name, &query.request));
            continue;
        }

        let hits_per_page = query.request.effective_hits_per_page();
        let PreparedBatchQuery {
            index_name,
            request,
            secured_hits_per_page_cap,
            can_see_unretrievable_attributes,
            ..
        } = query;
        let result = execute_single_batch_query(
            state.clone(),
            index_name,
            request,
            secured_hits_per_page_cap,
            can_see_unretrievable_attributes,
            dictionary_lookup_tenant.clone(),
        )
        .await?;

        // Check if this query satisfied the threshold
        let nb_hits = result.get("nbHits").and_then(|v| v.as_u64()).unwrap_or(0);
        if nb_hits >= hits_per_page as u64 {
            enough_found = true;
        }

        results.push(result);
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::{federation_fetch_hits_per_page, normalize_request_for_federation};
    use crate::dto::SearchRequest;
    use crate::federation::FederationConfig;
    /// TODO: Document federation_fetch_hits_per_page_uses_only_top_level_window.
    #[test]
    fn federation_fetch_hits_per_page_uses_only_top_level_window() {
        let request = SearchRequest {
            page: 20,
            hits_per_page: Some(1_000),
            ..Default::default()
        };
        let federation = FederationConfig {
            offset: 1,
            limit: 3,
            merge_facets: None,
        };

        let fetch_hits_per_page = federation_fetch_hits_per_page(&federation, None);

        assert_eq!(
            fetch_hits_per_page, 4,
            "ignored per-query page and hitsPerPage must not expand the federated fetch window"
        );

        let mut normalized = request;
        normalize_request_for_federation(&mut normalized, &federation, None);
        assert_eq!(normalized.page, 0);
        assert_eq!(normalized.hits_per_page, Some(4));
    }

    #[test]
    fn federation_fetch_hits_per_page_still_honors_secured_cap() {
        let federation = FederationConfig {
            offset: 10,
            limit: 20,
            merge_facets: None,
        };

        assert_eq!(federation_fetch_hits_per_page(&federation, Some(7)), 7);
        assert_eq!(federation_fetch_hits_per_page(&federation, Some(50)), 30);
    }
}
