use crate::index::document::DocumentConverter;
use once_cell::sync::Lazy;
use prometheus::{core::Collector, proto::MetricFamily, HistogramOpts, HistogramVec};
#[cfg(test)]
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};
use tantivy::Searcher;

const QUERY_PHASE_METRIC_NAME: &str = "flapjack_query_phase_seconds";
const QUERY_PHASE_METRIC_HELP: &str = "Query executor phase duration in seconds";

const PHASE_NAMES: [&str; 6] = [
    "prepare",
    "collect",
    "rank",
    "fetch",
    "facet_extract",
    "unattributed",
];
const EXECUTION_PATH_NAMES: [&str; 5] = [
    "relevance",
    "relevance_facets",
    "sort_fast",
    "sort_fallback",
    "count_only",
];

static QUERY_PHASE_CLOCK: Lazy<Instant> = Lazy::new(Instant::now);
static SEARCHER_GENERATIONS: Lazy<SearcherGenerationTracker<DocumentConverter>> =
    Lazy::new(SearcherGenerationTracker::default);
static QUERY_PHASE_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    let histogram = HistogramVec::new(
        HistogramOpts::new(QUERY_PHASE_METRIC_NAME, QUERY_PHASE_METRIC_HELP),
        &["phase", "execution_path"],
    )
    .expect("query phase histogram should be constructible");
    for phase in PHASE_NAMES {
        for execution_path in EXECUTION_PATH_NAMES {
            histogram.with_label_values(&[phase, execution_path]);
        }
    }
    histogram
});

struct TrackedSearcherGeneration<T: ?Sized> {
    index_identity: Weak<T>,
    generation_id: u64,
}

/// Retains one generation per live Flapjack index without keeping unloaded indexes alive.
pub(super) struct SearcherGenerationTracker<T: ?Sized> {
    current_generations: Mutex<HashMap<usize, TrackedSearcherGeneration<T>>>,
}

impl<T: ?Sized> Default for SearcherGenerationTracker<T> {
    fn default() -> Self {
        Self {
            current_generations: Mutex::new(HashMap::new()),
        }
    }
}

impl<T: ?Sized> SearcherGenerationTracker<T> {
    pub(super) fn observe(&self, index_identity: &Arc<T>, generation_id: u64) -> bool {
        let mut generations = self
            .current_generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let identity_key = Arc::as_ptr(index_identity) as *const () as usize;
        let weak_identity = Arc::downgrade(index_identity);
        if let Some(current) = generations.get_mut(&identity_key) {
            if Weak::ptr_eq(&current.index_identity, &weak_identity) {
                let cold = generation_id > current.generation_id;
                current.generation_id = current.generation_id.max(generation_id);
                return cold;
            }
        }

        generations.retain(|_, entry| entry.index_identity.strong_count() > 0);
        generations.insert(
            identity_key,
            TrackedSearcherGeneration {
                index_identity: weak_identity,
                generation_id,
            },
        );
        true
    }

    #[cfg(test)]
    pub(super) fn tracked_index_count(&self) -> usize {
        self.current_generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

#[cfg(test)]
thread_local! {
    static CAPTURED_QUERY_PHASE_REPORTS: RefCell<Option<Vec<QueryPhaseReport>>> =
        const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum QueryExecutionPath {
    Relevance,
    RelevanceFacets,
    SortFast,
    SortFallback,
    CountOnly,
}

impl QueryExecutionPath {
    fn as_str(self) -> &'static str {
        EXECUTION_PATH_NAMES[self as usize]
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::RelevanceFacets,
            2 => Self::SortFast,
            3 => Self::SortFallback,
            4 => Self::CountOnly,
            _ => Self::Relevance,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum QueryPhase {
    Prepare,
    Collect,
    Rank,
    Fetch,
    FacetExtract,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueryPhaseReport {
    pub prepare_ns: u64,
    pub collect_ns: u64,
    pub rank_ns: u64,
    pub fetch_ns: u64,
    pub facet_extract_ns: u64,
    pub unattributed_ns: u64,
    pub total_ns: u64,
    pub matched_docs: usize,
    pub visited_segments: usize,
    pub candidates_collected: usize,
    pub facet_cardinality: usize,
    pub cold: bool,
    pub execution_path: &'static str,
}

#[derive(Default)]
pub(super) struct QueryPhaseCell {
    execution_lock: Mutex<()>,
    started_ns: AtomicU64,
    prepare_ns: AtomicU64,
    collect_ns: AtomicU64,
    rank_ns: AtomicU64,
    fetch_ns: AtomicU64,
    facet_extract_ns: AtomicU64,
    total_ns: AtomicU64,
    matched_docs: AtomicU64,
    visited_segments: AtomicU64,
    candidates_collected: AtomicU64,
    facet_cardinality: AtomicU64,
    cold: AtomicBool,
    execution_path: AtomicU8,
}

/// Serializes phase attribution for one execute call so overlapping executions
/// sharing a `QueryExecutor` cannot reset, accumulate, or finish each other's
/// counters. The report is finished on drop, keeping report emission inside
/// the serialized section even on early returns and errors.
pub(super) struct QueryPhaseGuard<'a> {
    cell: &'a QueryPhaseCell,
    _execution_lock: MutexGuard<'a, ()>,
}

impl Drop for QueryPhaseGuard<'_> {
    fn drop(&mut self) {
        self.cell.finish();
    }
}

impl QueryPhaseCell {
    pub(super) fn ensure_started(&self) {
        let now_ns = monotonic_ns();
        let _ = self
            .started_ns
            .compare_exchange(0, now_ns, Ordering::Relaxed, Ordering::Relaxed);
    }

    pub(super) fn begin(
        &self,
        index_identity: &Arc<DocumentConverter>,
        searcher: &Searcher,
        execution_path: QueryExecutionPath,
    ) -> QueryPhaseGuard<'_> {
        let execution_lock = self
            .execution_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.total_ns.load(Ordering::Relaxed) > 0 {
            self.reset();
        }
        self.ensure_started();
        self.execution_path
            .store(execution_path as u8, Ordering::Relaxed);
        self.visited_segments
            .store(searcher.segment_readers().len() as u64, Ordering::Relaxed);
        let generation_id = searcher.generation().generation_id();
        let cold = SEARCHER_GENERATIONS.observe(index_identity, generation_id);
        self.cold.store(cold, Ordering::Relaxed);
        QueryPhaseGuard {
            cell: self,
            _execution_lock: execution_lock,
        }
    }

    pub(super) fn set_execution_path(&self, execution_path: QueryExecutionPath) {
        self.execution_path
            .store(execution_path as u8, Ordering::Relaxed);
    }

    pub(super) fn observe(&self, phase: QueryPhase, duration: Duration) {
        let elapsed_ns = duration.as_nanos().min(u64::MAX as u128) as u64;
        self.phase_counter(phase)
            .fetch_add(elapsed_ns, Ordering::Relaxed);
    }

    pub(super) fn set_matched_docs(&self, matched_docs: usize) {
        self.matched_docs
            .store(matched_docs as u64, Ordering::Relaxed);
    }

    pub(super) fn set_candidates_collected(&self, candidates_collected: usize) {
        self.candidates_collected
            .store(candidates_collected as u64, Ordering::Relaxed);
    }

    pub(super) fn add_facet_cardinality(&self, facet_cardinality: usize) {
        self.facet_cardinality
            .fetch_add(facet_cardinality as u64, Ordering::Relaxed);
    }

    pub(super) fn finish(&self) {
        let total_ns = monotonic_ns().saturating_sub(self.started_ns.load(Ordering::Relaxed));
        self.total_ns.store(total_ns, Ordering::Relaxed);
        let report = self.report();
        for (phase, duration_ns) in [
            ("prepare", report.prepare_ns),
            ("collect", report.collect_ns),
            ("rank", report.rank_ns),
            ("fetch", report.fetch_ns),
            ("facet_extract", report.facet_extract_ns),
            ("unattributed", report.unattributed_ns),
        ] {
            QUERY_PHASE_SECONDS
                .with_label_values(&[phase, report.execution_path])
                .observe(duration_ns as f64 / 1_000_000_000.0);
        }
        capture_query_phase_report(report);
    }

    pub(super) fn report(&self) -> QueryPhaseReport {
        let prepare_ns = self.prepare_ns.load(Ordering::Relaxed);
        let collect_ns = self.collect_ns.load(Ordering::Relaxed);
        let rank_ns = self.rank_ns.load(Ordering::Relaxed);
        let fetch_ns = self.fetch_ns.load(Ordering::Relaxed);
        let facet_extract_ns = self.facet_extract_ns.load(Ordering::Relaxed);
        let total_ns = self.total_ns.load(Ordering::Relaxed);
        let attributed_ns = prepare_ns
            .saturating_add(collect_ns)
            .saturating_add(rank_ns)
            .saturating_add(fetch_ns)
            .saturating_add(facet_extract_ns);

        QueryPhaseReport {
            prepare_ns,
            collect_ns,
            rank_ns,
            fetch_ns,
            facet_extract_ns,
            unattributed_ns: total_ns.saturating_sub(attributed_ns),
            total_ns,
            matched_docs: self.matched_docs.load(Ordering::Relaxed) as usize,
            visited_segments: self.visited_segments.load(Ordering::Relaxed) as usize,
            candidates_collected: self.candidates_collected.load(Ordering::Relaxed) as usize,
            facet_cardinality: self.facet_cardinality.load(Ordering::Relaxed) as usize,
            cold: self.cold.load(Ordering::Relaxed),
            execution_path: QueryExecutionPath::from_u8(
                self.execution_path.load(Ordering::Relaxed),
            )
            .as_str(),
        }
    }

    fn phase_counter(&self, phase: QueryPhase) -> &AtomicU64 {
        match phase {
            QueryPhase::Prepare => &self.prepare_ns,
            QueryPhase::Collect => &self.collect_ns,
            QueryPhase::Rank => &self.rank_ns,
            QueryPhase::Fetch => &self.fetch_ns,
            QueryPhase::FacetExtract => &self.facet_extract_ns,
        }
    }

    fn reset(&self) {
        self.started_ns.store(0, Ordering::Relaxed);
        for counter in [
            &self.prepare_ns,
            &self.collect_ns,
            &self.rank_ns,
            &self.fetch_ns,
            &self.facet_extract_ns,
            &self.total_ns,
            &self.matched_docs,
            &self.visited_segments,
            &self.candidates_collected,
            &self.facet_cardinality,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
        self.cold.store(false, Ordering::Relaxed);
    }
}

pub fn gather_query_phase_metric_families() -> Vec<MetricFamily> {
    QUERY_PHASE_SECONDS
        .collect()
        .into_iter()
        .filter(|family| !family.get_metric().is_empty())
        .collect()
}

#[cfg(test)]
fn capture_query_phase_report(report: QueryPhaseReport) {
    CAPTURED_QUERY_PHASE_REPORTS.with(|captured| {
        if let Some(reports) = captured.borrow_mut().as_mut() {
            reports.push(report);
        }
    });
}

#[cfg(not(test))]
fn capture_query_phase_report(_report: QueryPhaseReport) {}

#[cfg(test)]
pub(crate) fn capture_query_phase_reports<T>(
    operation: impl FnOnce() -> T,
) -> (T, Vec<QueryPhaseReport>) {
    CAPTURED_QUERY_PHASE_REPORTS.with(|captured| {
        assert!(
            captured.borrow_mut().replace(Vec::new()).is_none(),
            "query phase report captures must not be nested"
        );
    });
    let result = operation();
    let reports = CAPTURED_QUERY_PHASE_REPORTS.with(|captured| {
        captured
            .borrow_mut()
            .take()
            .expect("query phase report capture must be active")
    });
    (result, reports)
}

fn monotonic_ns() -> u64 {
    (QUERY_PHASE_CLOCK.elapsed().as_nanos().min(u64::MAX as u128) as u64).saturating_add(1)
}
