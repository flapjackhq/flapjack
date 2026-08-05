use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use flapjack::query::geo::GeoParams;
use flapjack::types::{FacetRequest, Sort};

const HYBRID_FETCH_WINDOW_BUFFER: usize = 50;
const HYBRID_FETCH_WINDOW_MIN: usize = 200;

/// Module-private struct carrying resolved search parameters between pipeline phases.
/// This is NOT a public API and NOT a duplicate of Stage 4's `SearchOptions` — it exists
/// solely to avoid passing ~17 loose arguments between the pipeline phase functions.
pub(super) struct PreparedSearchParams {
    pub filter: Option<flapjack::types::Filter>,
    pub sort: Option<Sort>,
    pub loaded_settings: Option<Arc<flapjack::index::settings::IndexSettings>>,
    pub effective_relevancy_strictness: u32,
    pub facet_requests: Option<Vec<FacetRequest>>,
    pub distinct_count: Option<u32>,
    pub geo_params: GeoParams,
    pub hits_per_page: usize,
    pub fetch_limit: usize,
    pub fetch_offset: usize,
    pub typo_tolerance: Option<bool>,
    pub optional_filter_groups: Option<Vec<Vec<(String, String, f32)>>>,
    pub sum_or_filters_scores: bool,
    pub effective_enable_rules: Option<bool>,
    pub pagination_limited_exceeded: bool,
    pub all_query_words_optional: bool,
    pub should_window_for_personalization: bool,
    pub is_hybrid_active: bool,
}

/// Outputs produced by the reranking/transform phase that the response formatter needs.
pub(super) struct TransformOutputs {
    pub geo_distances: HashMap<String, (f64, f64, f64)>,
    pub automatic_radius: Option<u64>,
    _search_phase_witness: SearchPhaseWitness,
}

impl TransformOutputs {
    pub(super) fn new(
        geo_distances: HashMap<String, (f64, f64, f64)>,
        automatic_radius: Option<u64>,
        search_phase_witness: SearchPhaseWitness,
    ) -> Self {
        Self {
            geo_distances,
            automatic_radius,
            _search_phase_witness: search_phase_witness,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct SearchPhaseWitness {
    _private: (),
}

pub(super) fn hybrid_fetch_window(hits_per_page: usize, page: usize) -> usize {
    hits_per_page
        .saturating_mul(page.saturating_add(1))
        .saturating_add(HYBRID_FETCH_WINDOW_BUFFER)
        .max(HYBRID_FETCH_WINDOW_MIN)
}

pub(super) fn measure_pipeline_elapsed<T, F>(start: Instant, phase: F) -> (T, Duration)
where
    F: FnOnce() -> T,
{
    let output = phase();
    (output, start.elapsed())
}

pub(super) fn measure_search_phase<T, F>(start: Instant, phase: F) -> (T, Duration)
where
    F: FnOnce(SearchPhaseWitness) -> T,
{
    measure_pipeline_elapsed(start, || phase(SearchPhaseWitness { _private: () }))
}
