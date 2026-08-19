//! Stub summary for single_interleaving.rs.
use std::collections::HashMap;
use std::sync::Arc;

use flapjack::error::FlapjackError;
use flapjack::experiments::interleaving::{team_draft_interleave, Team};
use flapjack::types::SearchResult;

use crate::dto::SearchRequest;
use crate::handlers::replicas::resolve_search_target;
use crate::handlers::AppState;

use super::experiments::ExperimentContext;
use super::geo::apply_rule_geo_overrides;
use super::hybrid::HybridSearchInputs;
use super::pipeline::{hybrid_fetch_window, PreparedSearchParams};
use super::single_support::resolve_effective_relevancy_strictness;

pub(super) struct CoreSearchContext<'a> {
    pub(super) state: &'a Arc<AppState>,
    pub(super) effective_index: &'a str,
    pub(super) dictionary_lookup_tenant: &'a str,
    pub(super) req: &'a SearchRequest,
    pub(super) params: &'a mut PreparedSearchParams,
    pub(super) experiment_ctx: Option<ExperimentContext>,
    pub(super) assignment_query_id: &'a str,
    pub(super) hybrid_inputs: HybridSearchInputs<'a>,
    pub(super) secured_hits_per_page_cap: Option<usize>,
}

#[derive(Clone, Copy)]
struct SearchInvocation<'a> {
    state: &'a Arc<AppState>,
    req: &'a SearchRequest,
    params: &'a PreparedSearchParams,
    secured_hits_per_page_cap: Option<usize>,
    dictionary_lookup_tenant: &'a str,
}

impl SearchInvocation<'_> {
    /// Executes a search against the given tenant index, resolving virtual replica
    /// settings and dictionary scope before delegating to the core search engine.
    fn run(
        self,
        tenant_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<SearchResult, FlapjackError> {
        let resolved_target = resolve_search_target(self.state, tenant_id);
        let dictionary_lookup_tenant = if resolved_target.settings_override.is_some() {
            tenant_id
        } else {
            self.dictionary_lookup_tenant
        };
        let effective_relevancy_strictness = resolve_effective_relevancy_strictness(
            self.req,
            resolved_target.settings_override.as_ref(),
        );

        self.state
            .manager
            .search_full_with_stop_words_with_hits_per_page_cap(
                &resolved_target.data_index,
                &self.req.query,
                &flapjack::index::SearchOptions {
                    filter: self.params.filter.as_ref(),
                    sort: self.params.sort.as_ref(),
                    limit,
                    offset,
                    facets: self.params.facet_requests.as_deref(),
                    distinct: self.params.distinct_count,
                    max_values_per_facet: self.req.max_values_per_facet,
                    remove_stop_words: self.req.remove_stop_words.as_ref(),
                    ignore_plurals: self.req.ignore_plurals.as_ref(),
                    query_languages: self.req.query_languages.as_ref(),
                    query_type: self.req.query_type_prefix.as_deref(),
                    typo_tolerance: self.params.typo_tolerance,
                    advanced_syntax: self.req.advanced_syntax,
                    remove_words_if_no_results: self.req.remove_words_if_no_results.as_deref(),
                    optional_filter_specs: self.params.optional_filter_groups.as_deref(),
                    sum_or_filters_scores: self.params.sum_or_filters_scores,
                    exact_on_single_word_query: self.req.exact_on_single_word_query.as_deref(),
                    disable_exact_on_attributes: self.req.disable_exact_on_attributes.as_deref(),
                    enable_synonyms: self.req.enable_synonyms,
                    enable_rules: self.params.effective_enable_rules,
                    rule_contexts: self.req.rule_contexts.as_deref(),
                    restrict_searchable_attrs: self.req.restrict_searchable_attributes.as_deref(),
                    secured_hits_per_page_cap: self.secured_hits_per_page_cap,
                    decompound_query: self.req.decompound_query,
                    settings_override: resolved_target.settings_override.as_ref(),
                    dictionary_lookup_tenant: Some(dictionary_lookup_tenant),
                    all_query_words_optional: self.params.all_query_words_optional,
                    relevancy_strictness: Some(effective_relevancy_strictness),
                    min_proximity: self.req.min_proximity,
                    advanced_syntax_features: self.req.advanced_syntax_features.as_deref(),
                    ranking_synonym_store: None,
                    ranking_plural_map: None,
                },
            )
    }
}

struct CoreSearchOutcome {
    result: SearchResult,
    experiment_ctx: Option<ExperimentContext>,
    is_interleaving: bool,
}

struct InterleavingRequest<'a> {
    effective_index: &'a str,
    variant_index: String,
    assignment_query_id: &'a str,
}

struct InterleavingPage<'a> {
    interleave_k: usize,
    hits_per_page: usize,
    page: usize,
    assignment_query_id: &'a str,
}

/// TODO: Document execute_core_search.
pub(super) fn execute_core_search(
    context: CoreSearchContext<'_>,
) -> Result<(SearchResult, Option<ExperimentContext>, bool), FlapjackError> {
    let CoreSearchContext {
        state,
        effective_index,
        dictionary_lookup_tenant,
        req,
        params,
        experiment_ctx,
        assignment_query_id,
        hybrid_inputs,
        secured_hits_per_page_cap,
    } = context;

    apply_hybrid_fetch_window(req, params, hybrid_inputs);
    params.effective_relevancy_strictness = resolve_effective_relevancy_strictness(
        req,
        resolve_search_target(state, effective_index)
            .settings_override
            .as_ref(),
    );

    let invocation = SearchInvocation {
        state,
        req,
        params,
        secured_hits_per_page_cap,
        dictionary_lookup_tenant,
    };
    let CoreSearchOutcome {
        result,
        experiment_ctx,
        is_interleaving,
    } = run_initial_search(
        invocation,
        effective_index,
        experiment_ctx,
        assignment_query_id,
    )?;
    let result = rerun_search_for_rule_geo(invocation, effective_index, result, is_interleaving)?;

    Ok((result, experiment_ctx, is_interleaving))
}

fn apply_hybrid_fetch_window(
    req: &SearchRequest,
    params: &mut PreparedSearchParams,
    hybrid_inputs: HybridSearchInputs<'_>,
) {
    params.is_hybrid_active = hybrid_inputs.is_hybrid_active();
    if params.is_hybrid_active {
        params.fetch_limit = hybrid_fetch_window(params.hits_per_page, req.page);
        params.fetch_offset = 0;
    }
}

/// Merges control and variant search results using team-draft interleaving for
/// A/B test experiments, paginating the interleaved list and recording team assignments.
fn build_interleaved_result(
    control_result: SearchResult,
    variant_result: SearchResult,
    page: InterleavingPage<'_>,
    experiment_ctx: Option<&mut ExperimentContext>,
) -> SearchResult {
    let control_ids: Vec<&str> = control_result
        .documents
        .iter()
        .map(|doc| doc.document.id.as_str())
        .collect();
    let variant_ids: Vec<&str> = variant_result
        .documents
        .iter()
        .map(|doc| doc.document.id.as_str())
        .collect();
    let interleaved = team_draft_interleave(
        &control_ids,
        &variant_ids,
        page.interleave_k,
        experiment_ctx
            .as_ref()
            .map(|context| context.experiment_id.as_str())
            .unwrap_or(""),
        page.assignment_query_id,
    );

    let control_map: HashMap<String, flapjack::types::ScoredDocument> = control_result
        .documents
        .iter()
        .cloned()
        .map(|doc| (doc.document.id.clone(), doc))
        .collect();
    let variant_map: HashMap<String, flapjack::types::ScoredDocument> = variant_result
        .documents
        .iter()
        .cloned()
        .map(|doc| (doc.document.id.clone(), doc))
        .collect();

    let interleaved_docs_with_team: Vec<(flapjack::types::ScoredDocument, String)> = interleaved
        .into_iter()
        .filter_map(|item| {
            let team_label = match item.team {
                Team::A => "control",
                Team::B => "variant",
            };
            let doc = match item.team {
                Team::A => control_map
                    .get(&item.doc_id)
                    .cloned()
                    .or_else(|| variant_map.get(&item.doc_id).cloned()),
                Team::B => variant_map
                    .get(&item.doc_id)
                    .cloned()
                    .or_else(|| control_map.get(&item.doc_id).cloned()),
            };
            doc.map(|scored_doc| (scored_doc, team_label.to_string()))
        })
        .collect();

    let total_interleaved = interleaved_docs_with_team.len();
    let page_start = (page.page * page.hits_per_page).min(total_interleaved);
    let page_end = (page_start + page.hits_per_page).min(total_interleaved);
    let page_slice = &interleaved_docs_with_team[page_start..page_end];

    if let Some(context) = experiment_ctx {
        context.interleaved_teams = Some(
            page_slice
                .iter()
                .map(|(doc, team)| (doc.document.id.clone(), team.clone()))
                .collect(),
        );
    }

    SearchResult {
        documents: page_slice.iter().map(|(doc, _)| doc.clone()).collect(),
        total: total_interleaved,
        facets: control_result.facets,
        facets_stats: control_result.facets_stats,
        user_data: control_result.user_data,
        applied_rules: control_result.applied_rules,
        parsed_query: control_result.parsed_query,
        exhaustive_facet_values: control_result.exhaustive_facet_values,
        exhaustive_rules_match: control_result.exhaustive_rules_match,
        query_after_removal: control_result.query_after_removal,
        rendering_content: control_result.rendering_content,
        effective_around_lat_lng: control_result.effective_around_lat_lng,
        effective_around_radius: control_result.effective_around_radius,
    }
}

/// Runs both control and variant searches for an interleaving experiment, falling
/// back to control-only results if the variant index is not found.
fn run_interleaved_search(
    invocation: SearchInvocation<'_>,
    request: InterleavingRequest<'_>,
    mut experiment_ctx: Option<ExperimentContext>,
) -> Result<CoreSearchOutcome, FlapjackError> {
    let interleave_k = invocation
        .params
        .fetch_limit
        .saturating_add(invocation.params.fetch_offset)
        .max(invocation.params.hits_per_page);
    let control_result = invocation.run(request.effective_index, interleave_k, 0)?;

    match invocation.run(&request.variant_index, interleave_k, 0) {
        Ok(variant_result) => Ok(CoreSearchOutcome {
            result: build_interleaved_result(
                control_result,
                variant_result,
                InterleavingPage {
                    interleave_k,
                    hits_per_page: invocation.params.hits_per_page,
                    page: invocation.req.page,
                    assignment_query_id: request.assignment_query_id,
                },
                experiment_ctx.as_mut(),
            ),
            experiment_ctx,
            is_interleaving: true,
        }),
        Err(FlapjackError::TenantNotFound(_)) => Ok(CoreSearchOutcome {
            result: invocation.run(
                request.effective_index,
                invocation.params.fetch_limit,
                invocation.params.fetch_offset,
            )?,
            experiment_ctx: None,
            is_interleaving: false,
        }),
        Err(error) => Err(error),
    }
}

/// Dispatches the initial search: returns empty results if pagination limit is exceeded,
/// runs interleaved search if an experiment is active, or performs a standard search.
fn run_initial_search(
    invocation: SearchInvocation<'_>,
    effective_index: &str,
    experiment_ctx: Option<ExperimentContext>,
    assignment_query_id: &str,
) -> Result<CoreSearchOutcome, FlapjackError> {
    if invocation.params.pagination_limited_exceeded {
        let mut limited = invocation.run(effective_index, 0, 0)?;
        limited.documents.clear();
        return Ok(CoreSearchOutcome {
            result: limited,
            experiment_ctx,
            is_interleaving: false,
        });
    }

    if let Some(variant_index) = experiment_ctx
        .as_ref()
        .and_then(|context| context.interleaving_variant_index.clone())
    {
        return run_interleaved_search(
            invocation,
            InterleavingRequest {
                effective_index,
                variant_index,
                assignment_query_id,
            },
            experiment_ctx,
        );
    }

    Ok(CoreSearchOutcome {
        result: invocation.run(
            effective_index,
            invocation.params.fetch_limit,
            invocation.params.fetch_offset,
        )?,
        experiment_ctx,
        is_interleaving: false,
    })
}

/// Re-executes the search with a wider fetch window when query rules inject geo
/// parameters (aroundLatLng/aroundRadius) that were not present in the original request.
fn rerun_search_for_rule_geo(
    invocation: SearchInvocation<'_>,
    effective_index: &str,
    mut result: SearchResult,
    is_interleaving: bool,
) -> Result<SearchResult, FlapjackError> {
    if invocation.params.pagination_limited_exceeded
        || is_interleaving
        || invocation.params.geo_params.has_geo_filter()
    {
        return Ok(result);
    }

    let rule_geo_params = apply_rule_geo_overrides(
        invocation.params.geo_params.clone(),
        result.effective_around_lat_lng.as_deref(),
        result.effective_around_radius.as_ref(),
    );
    if !rule_geo_params.has_geo_filter() {
        return Ok(result);
    }

    let geo_fetch_limit = (invocation.params.hits_per_page
        + invocation.req.page * invocation.params.hits_per_page)
        .saturating_mul(10)
        .max(1000);
    result = invocation.run(effective_index, geo_fetch_limit, 0)?;
    Ok(result)
}
