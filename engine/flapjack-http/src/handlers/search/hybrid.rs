/// Hybrid (vector + keyword) search types and helpers for the single-search pipeline.
///
/// This module owns:
/// - The `HybridParams` type alias (feature-gated)
/// - `HybridSearchInputs` / `HybridFusionContext` carrier structs
/// - All vector-search helpers: embedding resolution, RRF fusion, and fallback messaging
/// - `apply_hybrid_fusion` — the post-search fusion step called from `single.rs`
/// - `resolve_hybrid_search_inputs` — the pre-search async resolver called from `single.rs`
use std::sync::Arc;

use flapjack::types::SearchResult;

use crate::handlers::AppState;

#[cfg(feature = "vector-search")]
use super::pipeline::hybrid_fetch_window;

// ---------------------------------------------------------------------------
// Type aliases (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "vector-search")]
pub(super) type HybridParams = crate::dto::HybridSearchParams;
#[cfg(not(feature = "vector-search"))]
pub(super) type HybridParams = ();

// ---------------------------------------------------------------------------
// Carrier structs
// ---------------------------------------------------------------------------

// Fields are consumed by vector-search-only helpers, so non-vector builds do not read them.
#[allow(dead_code)]
pub(super) struct HybridSearchInputs<'a> {
    pub(super) query_vector: &'a Option<Vec<f32>>,
    pub(super) hybrid_params: &'a Option<HybridParams>,
}

impl HybridSearchInputs<'_> {
    pub(super) fn is_hybrid_active(&self) -> bool {
        #[cfg(feature = "vector-search")]
        {
            self.query_vector.is_some()
        }
        #[cfg(not(feature = "vector-search"))]
        {
            false
        }
    }
}

// Fields are consumed by vector-search-only fusion paths, so non-vector builds do not read them.
#[allow(dead_code)]
pub(super) struct HybridFusionContext<'a> {
    pub(super) state: &'a Arc<AppState>,
    pub(super) effective_index: &'a str,
    pub(super) hybrid_inputs: HybridSearchInputs<'a>,
    pub(super) hits_per_page: usize,
    pub(super) offset: usize,
    pub(super) is_interleaving: bool,
    pub(super) pagination_limited_exceeded: bool,
}

// ---------------------------------------------------------------------------
// Hybrid helper functions (vector-search feature gate)
// ---------------------------------------------------------------------------

#[cfg(feature = "vector-search")]
pub(super) fn hybrid_fallback_message(reason: &str) -> String {
    format!(
        "Hybrid search unavailable: {}. Falling back to keyword search.",
        reason
    )
}

#[cfg(feature = "vector-search")]
pub(super) fn missing_hybrid_query_vector_fallback(
    hybrid_inputs: &HybridSearchInputs<'_>,
    is_interleaving: bool,
) -> Option<String> {
    (!is_interleaving
        && hybrid_inputs.query_vector.is_none()
        && hybrid_inputs.hybrid_params.is_some())
    .then(|| hybrid_fallback_message("no embedders configured"))
}

/// Fuses BM25 keyword results with vector search results using Reciprocal Rank
/// Fusion (RRF), weighted by `semantic_ratio`, then paginates the fused list.
#[cfg(feature = "vector-search")]
fn fuse_hybrid_search_results(
    state: &Arc<AppState>,
    effective_index: &str,
    result: &mut SearchResult,
    vector_results: &[flapjack::vector::VectorSearchResult],
    semantic_ratio: f64,
    hits_per_page: usize,
    offset: usize,
) {
    let bm25_ids: Vec<String> = result
        .documents
        .iter()
        .map(|document| document.document.id.clone())
        .collect();
    let fused_results = crate::fusion::rrf_fuse(&bm25_ids, vector_results, semantic_ratio, 60);
    let fused_documents = build_fused_documents(state, effective_index, result, &fused_results);
    replace_with_fused_page(result, fused_documents, hits_per_page, offset);
}

/// Builds the fused document list by matching RRF-ranked IDs to BM25 hits or
/// fetching vector-only documents from the index when not present in keyword results.
#[cfg(feature = "vector-search")]
fn build_fused_documents(
    state: &Arc<AppState>,
    effective_index: &str,
    result: &mut SearchResult,
    fused_results: &[crate::fusion::FusedResult],
) -> Vec<flapjack::types::ScoredDocument> {
    let mut bm25_documents: std::collections::HashMap<String, flapjack::types::ScoredDocument> =
        std::mem::take(&mut result.documents)
            .into_iter()
            .map(|document| (document.document.id.clone(), document))
            .collect();

    fused_results
        .iter()
        .filter_map(|fused_result| {
            build_fused_document(state, effective_index, &mut bm25_documents, fused_result)
        })
        .collect()
}

#[cfg(feature = "vector-search")]
fn build_fused_document(
    state: &Arc<AppState>,
    effective_index: &str,
    bm25_documents: &mut std::collections::HashMap<String, flapjack::types::ScoredDocument>,
    fused_result: &crate::fusion::FusedResult,
) -> Option<flapjack::types::ScoredDocument> {
    if let Some(scored_document) = bm25_documents.remove(&fused_result.doc_id) {
        return Some(fused_scored_document(
            scored_document.document,
            fused_result,
        ));
    }

    fetch_vector_only_fused_document(state, effective_index, fused_result)
}

#[cfg(feature = "vector-search")]
fn fused_scored_document(
    document: flapjack::types::Document,
    fused_result: &crate::fusion::FusedResult,
) -> flapjack::types::ScoredDocument {
    flapjack::types::ScoredDocument {
        document,
        score: fused_result.fused_score as f32,
    }
}

/// Fetches a document that appeared only in vector results (not in BM25 hits)
/// from the index store, assigning its RRF fused score.
#[cfg(feature = "vector-search")]
fn fetch_vector_only_fused_document(
    state: &Arc<AppState>,
    effective_index: &str,
    fused_result: &crate::fusion::FusedResult,
) -> Option<flapjack::types::ScoredDocument> {
    match state
        .manager
        .get_document(effective_index, &fused_result.doc_id)
    {
        Ok(Some(document)) => Some(fused_scored_document(document, fused_result)),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                "hybrid search: failed to fetch vector-only doc '{}': {}",
                fused_result.doc_id,
                error
            );
            None
        }
    }
}

#[cfg(feature = "vector-search")]
fn replace_with_fused_page(
    result: &mut SearchResult,
    fused_documents: Vec<flapjack::types::ScoredDocument>,
    hits_per_page: usize,
    offset: usize,
) {
    let total_fused = fused_documents.len();
    let page_start = offset.min(total_fused);
    let page_end = page_start.saturating_add(hits_per_page).min(total_fused);
    result.documents = fused_documents[page_start..page_end].to_vec();
    result.total = total_fused;
}

/// Executes a vector-only search against the tenant's vector index, returning
/// ranked results or a fallback message if the vector index is unavailable or empty.
#[cfg(feature = "vector-search")]
pub(super) fn hybrid_vector_results(
    context: &HybridFusionContext<'_>,
    query_vector: &[f32],
) -> Result<Vec<flapjack::vector::VectorSearchResult>, String> {
    let Some(vector_index) = context
        .state
        .manager
        .get_vector_index(context.effective_index)
    else {
        return Err(hybrid_fallback_message("no vector index for this tenant"));
    };

    let Ok(vector_index_guard) = vector_index.read() else {
        tracing::error!(
            "vector index read lock poisoned for '{}'",
            context.effective_index
        );
        return Err(hybrid_fallback_message("internal error"));
    };

    if vector_index_guard.is_empty() {
        return Err(hybrid_fallback_message("vector index is empty"));
    }

    let vector_fetch_limit = hybrid_fetch_window(context.hits_per_page, context.offset);
    vector_index_guard
        .search(query_vector, vector_fetch_limit)
        .map_err(|error| {
            tracing::warn!(
                "hybrid search: vector search failed for '{}': {}",
                context.effective_index,
                error
            );
            hybrid_fallback_message("vector search failed")
        })
}

/// Determines whether hybrid search is requested (explicit `hybrid` param or
/// NeuralSearch index mode) and returns the resolved `HybridParams` with defaults.
#[cfg(feature = "vector-search")]
pub(super) fn requested_hybrid_params(
    req: &crate::dto::SearchRequest,
    settings: Option<&flapjack::index::settings::IndexSettings>,
) -> Option<HybridParams> {
    use crate::dto::HybridSearchParams;
    use flapjack::index::settings::IndexMode;

    let hybrid_requested = req.hybrid.is_some()
        || settings.is_some_and(|settings| {
            matches!(
                super::request::resolve_search_mode(&req.mode, settings),
                IndexMode::NeuralSearch
            )
        });

    hybrid_requested.then(|| {
        req.hybrid.clone().unwrap_or(HybridSearchParams {
            semantic_ratio: 0.5,
            embedder: "default".to_string(),
        })
    })
}

/// Resolves the query embedding vector for hybrid search: checks the embedding
/// cache first, then falls back to computing a fresh embedding via the configured embedder.
#[cfg(feature = "vector-search")]
async fn resolve_hybrid_query_vector(
    state: &Arc<AppState>,
    effective_index: &str,
    req: &crate::dto::SearchRequest,
    settings: Option<&flapjack::index::settings::IndexSettings>,
    hybrid_params: &HybridParams,
) -> Option<Vec<f32>> {
    let embedder_name = hybrid_params.embedder.as_str();

    if let Some(cached_vector) = state
        .embedder_store
        .query_cache
        .get(embedder_name, &req.query)
    {
        return Some(cached_vector);
    }

    let settings = settings?;
    let embedder =
        match state
            .embedder_store
            .get_or_create(effective_index, embedder_name, settings)
        {
            Ok(embedder) => embedder,
            Err(error) => {
                tracing::warn!(
                    "hybrid search: embedder resolution failed for '{}': {}",
                    effective_index,
                    error
                );
                return None;
            }
        };

    match embedder.embed_query(&req.query).await {
        Ok(query_vector) => {
            state.embedder_store.query_cache.insert(
                embedder_name,
                &req.query,
                query_vector.clone(),
            );
            Some(query_vector)
        }
        Err(error) => {
            tracing::warn!(
                "hybrid search: embedding failed for '{}': {}",
                effective_index,
                error
            );
            None
        }
    }
}

/// Pre-search async resolver that determines hybrid params and computes the query
/// vector embedding. Returns `(None, None)` when hybrid search is not applicable.
#[cfg(feature = "vector-search")]
pub(super) async fn resolve_hybrid_search_inputs(
    state: &Arc<AppState>,
    effective_index: &str,
    req: &crate::dto::SearchRequest,
) -> (Option<Vec<f32>>, Option<HybridParams>) {
    let settings = state.manager.get_settings(effective_index);
    let Some(mut hybrid_params) = requested_hybrid_params(req, settings.as_deref()) else {
        return (None, None);
    };

    hybrid_params.clamp_ratio();
    if hybrid_params.semantic_ratio <= 0.0 {
        return (None, Some(hybrid_params));
    }

    let query_vector = resolve_hybrid_query_vector(
        state,
        effective_index,
        req,
        settings.as_deref(),
        &hybrid_params,
    )
    .await;
    (query_vector, Some(hybrid_params))
}

#[cfg(not(feature = "vector-search"))]
pub(super) async fn resolve_hybrid_search_inputs(
    _state: &Arc<AppState>,
    _effective_index: &str,
    _req: &crate::dto::SearchRequest,
) -> (Option<Vec<f32>>, Option<HybridParams>) {
    (None, None)
}

// ---------------------------------------------------------------------------
// Phase 3a: Hybrid search fusion (cfg-gated)
// ---------------------------------------------------------------------------

/// Fuses keyword and vector search results using reciprocal rank fusion.
#[cfg(feature = "vector-search")]
pub(super) fn apply_hybrid_fusion(
    context: &HybridFusionContext<'_>,
    result: &mut SearchResult,
) -> Option<String> {
    let Some((query_vector, hybrid_params)) = hybrid_query_inputs(context) else {
        return missing_hybrid_query_vector_fallback(
            &context.hybrid_inputs,
            context.is_interleaving,
        );
    };

    let vector_results = match hybrid_vector_results(context, query_vector) {
        Ok(vector_results) => vector_results,
        Err(fallback_message) => return Some(fallback_message),
    };

    fuse_hybrid_search_results(
        context.state,
        context.effective_index,
        result,
        &vector_results,
        hybrid_params.semantic_ratio,
        context.hits_per_page,
        context.offset,
    );
    None
}

#[cfg(feature = "vector-search")]
fn hybrid_query_inputs<'a>(
    context: &'a HybridFusionContext<'a>,
) -> Option<(&'a Vec<f32>, &'a HybridParams)> {
    if context.pagination_limited_exceeded || context.is_interleaving {
        return None;
    }

    Some((
        context.hybrid_inputs.query_vector.as_ref()?,
        context.hybrid_inputs.hybrid_params.as_ref()?,
    ))
}

#[cfg(not(feature = "vector-search"))]
pub(super) fn apply_hybrid_fusion(
    _context: &HybridFusionContext<'_>,
    _result: &mut SearchResult,
) -> Option<String> {
    None
}
