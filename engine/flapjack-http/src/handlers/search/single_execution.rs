use axum::{extract::State, Json};
use std::sync::Arc;
use std::time::Instant;

use flapjack::error::FlapjackError;
use flapjack::index::DEFAULT_RELEVANCY_STRICTNESS;

use crate::dto::SearchRequest;
use crate::handlers::replicas::resolve_search_target;
use crate::handlers::AppState;

use super::experiments::{resolve_experiment_context, ExperimentContext};
use super::geo::resolve_around_lat_lng_via_ip;
use super::hybrid::{
    apply_hybrid_fusion, resolve_hybrid_search_inputs, HybridFusionContext, HybridSearchInputs,
};
use super::personalization::{resolve_personalization_context, PersonalizationContext};
use super::pipeline::{measure_search_phase, PreparedSearchParams};
use super::response::{format_search_response, ResponseAssemblyContext};
use super::single_interleaving::{execute_core_search, CoreSearchContext};
use super::single_support::{
    apply_similar_query_override, build_facet_requests, normalize_query_languages,
    resolve_distinct_count, resolve_optional_filter_groups, resolve_search_sort,
    resolve_search_window, resolve_typo_tolerance,
};
use super::transforms::{apply_reranking_and_transforms, TransformContext};

/// Top-level single-index search orchestrator: validates the request, resolves
/// experiment/personalization/hybrid context, executes search on a blocking thread,
/// applies post-search transforms, and assembles the Algolia-compatible JSON response.
pub(super) async fn search_single_with_secured_hits_per_page_cap(
    State(state): State<Arc<AppState>>,
    index_name: String,
    mut req: SearchRequest,
    secured_hits_per_page_cap: Option<usize>,
    can_see_unretrievable: bool,
    dictionary_lookup_tenant: Option<String>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    req.validate()?;
    resolve_around_lat_lng_via_ip(&mut req, &state.geoip_reader);

    let enqueue_time = Instant::now();

    let query_id = if req.click_analytics == Some(true) {
        Some(hex::encode(uuid::Uuid::new_v4().as_bytes()))
    } else {
        None
    };
    let assignment_query_id = query_id
        .clone()
        .unwrap_or_else(|| hex::encode(uuid::Uuid::new_v4().as_bytes()));
    let (effective_index, experiment_ctx) =
        resolve_experiment_context(&state, &index_name, &mut req, &assignment_query_id);

    let (query_vector, hybrid_params) =
        resolve_hybrid_search_inputs(&state, &effective_index, &req).await;
    let dictionary_lookup_tenant = dictionary_lookup_tenant.unwrap_or_else(|| index_name.clone());

    let index_settings = state.manager.get_settings(&effective_index);
    let personalization_ctx =
        resolve_personalization_context(&state, &req, index_settings.as_deref()).await;

    tokio::task::spawn_blocking(move || {
        search_single_sync(SearchSyncContext {
            state,
            index_name,
            effective_index,
            dictionary_lookup_tenant,
            req,
            secured_hits_per_page_cap,
            can_see_unretrievable_attributes: can_see_unretrievable,
            enqueue_time,
            query_id,
            assignment_query_id,
            experiment_ctx,
            personalization_ctx,
            query_vector,
            hybrid_params,
        })
    })
    .await
    .map_err(|error| FlapjackError::InvalidQuery(format!("spawn_blocking join error: {}", error)))?
}

/// Resolves all search parameters from the request and index settings into a
/// `PreparedSearchParams` struct: filters, sort, facets, pagination, typo tolerance, and rules.
fn prepare_search_params(
    state: &AppState,
    effective_index: &str,
    req: &mut SearchRequest,
    has_personalization: bool,
) -> Result<PreparedSearchParams, FlapjackError> {
    let filter = req.build_combined_filter()?;
    let sort = resolve_search_sort(req);
    let loaded_settings = state.manager.get_settings(effective_index);
    if req.hits_per_page.is_none() {
        req.hits_per_page = loaded_settings
            .as_ref()
            .map(|settings| settings.hits_per_page as usize);
    }
    let facet_requests = build_facet_requests(req, loaded_settings.as_deref());
    let distinct_count = resolve_distinct_count(req, loaded_settings.as_deref());
    let geo_params = req.build_geo_params();
    let optional_filter_groups = resolve_optional_filter_groups(req);
    let search_window = resolve_search_window(
        req,
        has_personalization && sort.is_none() && !geo_params.has_geo_filter(),
        geo_params.has_geo_filter(),
        optional_filter_groups.is_some(),
    );
    let typo_tolerance = resolve_typo_tolerance(req);
    let sum_or_filters_scores = req.sum_or_filters_scores.unwrap_or(false);
    let effective_enable_rules = req.enable_rules.or(loaded_settings
        .as_ref()
        .and_then(|settings| settings.enable_rules));
    let pagination_limited_exceeded = loaded_settings.as_ref().is_some_and(|settings| {
        req.page.saturating_mul(search_window.hits_per_page)
            >= settings.pagination_limited_to as usize
    });
    let all_query_words_optional = apply_similar_query_override(req);
    normalize_query_languages(req);

    Ok(PreparedSearchParams {
        filter,
        sort,
        loaded_settings,
        effective_relevancy_strictness: req
            .relevancy_strictness
            .unwrap_or(DEFAULT_RELEVANCY_STRICTNESS),
        facet_requests,
        distinct_count,
        geo_params,
        hits_per_page: search_window.hits_per_page,
        fetch_limit: search_window.limit,
        fetch_offset: search_window.offset,
        typo_tolerance,
        optional_filter_groups,
        sum_or_filters_scores,
        effective_enable_rules,
        pagination_limited_exceeded,
        all_query_words_optional,
        should_window_for_personalization: search_window.should_window_for_personalization,
        is_hybrid_active: false, // set by execute_core_search
    })
}

/// Carries all state needed to run the synchronous search pipeline on a blocking
/// thread: resolved params, auth context, experiment/personalization/hybrid state.
struct SearchSyncContext {
    state: Arc<AppState>,
    index_name: String,
    effective_index: String,
    dictionary_lookup_tenant: String,
    req: SearchRequest,
    secured_hits_per_page_cap: Option<usize>,
    can_see_unretrievable_attributes: bool,
    enqueue_time: Instant,
    query_id: Option<String>,
    assignment_query_id: String,
    experiment_ctx: Option<ExperimentContext>,
    personalization_ctx: Option<PersonalizationContext>,
    query_vector: Option<Vec<f32>>,
    hybrid_params: Option<super::hybrid::HybridParams>,
}

/// Top-level synchronous search orchestrator. Calls the extracted phase functions
/// in sequence, threading `PreparedSearchParams` and intermediate results between them.
fn search_single_sync(
    context: SearchSyncContext,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let SearchSyncContext {
        state,
        index_name,
        effective_index,
        dictionary_lookup_tenant,
        mut req,
        secured_hits_per_page_cap,
        can_see_unretrievable_attributes,
        enqueue_time,
        query_id,
        assignment_query_id,
        experiment_ctx,
        personalization_ctx,
        query_vector,
        hybrid_params,
    } = context;

    let queue_wait = enqueue_time.elapsed();
    let start = Instant::now();

    let mut params = prepare_search_params(
        &state,
        &effective_index,
        &mut req,
        personalization_ctx.is_some(),
    )?;

    let (mut result, experiment_ctx, is_interleaving) = execute_core_search(CoreSearchContext {
        state: &state,
        effective_index: &effective_index,
        dictionary_lookup_tenant: &dictionary_lookup_tenant,
        req: &req,
        params: &mut params,
        experiment_ctx,
        assignment_query_id: &assignment_query_id,
        hybrid_inputs: HybridSearchInputs {
            query_vector: &query_vector,
            hybrid_params: &hybrid_params,
        },
        secured_hits_per_page_cap,
    })?;

    let ((fallback_message, transform_outputs), search_elapsed) =
        measure_search_phase(start, |search_phase_witness| {
            let fallback_message = apply_hybrid_fusion(
                &HybridFusionContext {
                    state: &state,
                    effective_index: &effective_index,
                    hybrid_inputs: HybridSearchInputs {
                        query_vector: &query_vector,
                        hybrid_params: &hybrid_params,
                    },
                    hits_per_page: params.hits_per_page,
                    page: req.page,
                    is_interleaving,
                    pagination_limited_exceeded: params.pagination_limited_exceeded,
                },
                &mut result,
            );

            let transform_outputs = apply_reranking_and_transforms(
                &mut result,
                TransformContext {
                    state: &state,
                    req: &req,
                    params: &params,
                    effective_index: &effective_index,
                    personalization_ctx: personalization_ctx.as_ref(),
                    is_interleaving,
                    search_phase_witness,
                },
            );

            (fallback_message, transform_outputs)
        });

    let applied_relevancy_strictness = Some(params.effective_relevancy_strictness);
    let is_virtual_replica_search = resolve_search_target(&state, &effective_index)
        .settings_override
        .is_some();

    format_search_response(
        result,
        &req,
        &params,
        ResponseAssemblyContext {
            state: &state,
            index_name: &index_name,
            effective_index: &effective_index,
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
        },
    )
}
