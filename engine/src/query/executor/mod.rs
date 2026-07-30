use crate::error::Result;
use crate::index::document::DocumentConverter;
use crate::index::settings::{strip_unordered_prefix, IndexSettings};
use crate::query::filter::FilterCompiler;
use crate::query::parser::ShortQueryPlaceholder;
use crate::types::{Filter, ScoredDocument, SearchResult};
use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tantivy::collector::Collector;
use tantivy::query::{
    BooleanQuery, BoostQuery, EnableScoring, Occur, Query as TantivyQuery, TermQuery,
};
use tantivy::schema::IndexRecordOption;
use tantivy::{Executor, Searcher};

/// Type alias: QueryExecutor stores settings as Arc to avoid cloning the
/// full IndexSettings struct on every search (it can be 1+ KB).
type SettingsRef = Option<Arc<IndexSettings>>;

#[cfg(test)]
mod bounded_tests;
mod facet_collector;
mod facets;
mod metrics;
#[cfg(test)]
mod metrics_tests;
#[cfg(test)]
mod parity_fixtures;
#[cfg(test)]
mod parity_tests;
mod relevance;
mod rules;
mod sorting;

pub use facets::FacetSearchParams;
#[cfg(test)]
pub(crate) use metrics::capture_query_phase_reports;
pub use metrics::{gather_query_phase_metric_families, QueryPhaseReport};
use metrics::{QueryExecutionPath, QueryPhase, QueryPhaseCell, QueryPhaseGuard};

fn group_doc_addresses_for_fetch(
    doc_addresses: Vec<(f32, tantivy::DocAddress)>,
) -> Vec<(usize, f32, tantivy::DocAddress)> {
    let mut fetch_plan = doc_addresses
        .into_iter()
        .enumerate()
        .map(|(result_position, (score, address))| (result_position, score, address))
        .collect::<Vec<_>>();
    fetch_plan.sort_unstable_by_key(|(_, _, address)| (address.segment_ord, address.doc_id));
    fetch_plan
}

#[cfg(test)]
mod document_fetch_tests {
    use super::group_doc_addresses_for_fetch;
    use tantivy::DocAddress;

    #[test]
    fn fetch_plan_groups_segments_and_preserves_result_positions() {
        let addresses = vec![
            (4.0, DocAddress::new(2, 9)),
            (3.0, DocAddress::new(0, 7)),
            (2.0, DocAddress::new(2, 3)),
            (1.0, DocAddress::new(0, 1)),
        ];

        let plan = group_doc_addresses_for_fetch(addresses);

        assert_eq!(
            plan,
            vec![
                (3, 1.0, DocAddress::new(0, 1)),
                (1, 3.0, DocAddress::new(0, 7)),
                (2, 2.0, DocAddress::new(2, 3)),
                (0, 4.0, DocAddress::new(2, 9)),
            ]
        );
    }
}

// Benchmark seams re-exported crate-internally so the executor performance
// harness in `crate::integ_tests::test_perf` measures the same frozen fixture,
// frozen query families, and thread-count knob the parity and bounded suites
// own — no second corpus, catalog, or env guard.
#[cfg(test)]
pub(crate) use bounded_tests::SearchThreadsEnvGuard;
#[cfg(test)]
pub(crate) use parity_fixtures::{build_parity_fixture, ExecutorParityFixture};
#[cfg(test)]
pub(crate) use parity_tests::{run_frozen_family, FrozenFamily};

pub struct QueryExecutor {
    pub(crate) converter: Arc<DocumentConverter>,
    pub(crate) filter_compiler: FilterCompiler,
    pub(crate) tantivy_schema: tantivy::schema::Schema,
    pub(crate) settings: SettingsRef,
    pub(crate) json_search_field: tantivy::schema::Field,
    pub(crate) searchable_paths: Vec<String>,
    /// Set of paths that have the `unordered(...)` modifier - position penalty is disabled
    pub(crate) unordered_paths: HashSet<String>,
    pub(crate) query_text: String,
    pub(crate) max_values_per_facet: Option<usize>,
    phase_cell: QueryPhaseCell,
}

impl QueryExecutor {
    /// Create a query executor with the schema's `_json_search` field, a document
    /// converter, and a default filter compiler.
    pub fn new(converter: Arc<DocumentConverter>, schema: tantivy::schema::Schema) -> Self {
        let json_search_field = schema
            .get_field("_json_search")
            .expect("_json_search field required");
        QueryExecutor {
            converter,
            filter_compiler: FilterCompiler::new(schema.clone()),
            tantivy_schema: schema,
            settings: None,
            json_search_field,
            searchable_paths: vec![],
            unordered_paths: HashSet::new(),
            query_text: String::new(),
            max_values_per_facet: None,
            phase_cell: QueryPhaseCell::default(),
        }
    }

    pub fn with_max_values_per_facet(mut self, max: Option<usize>) -> Self {
        self.max_values_per_facet = max;
        self
    }

    /// Apply index settings: extract searchable attributes (with `unordered()` tracking),
    /// configure typo tolerance, stop words, synonyms, and custom ranking.
    pub fn with_settings(mut self, settings: SettingsRef) -> Self {
        // Reset derived path state on each call so repeated builder usage stays correct.
        self.searchable_paths.clear();
        self.unordered_paths.clear();

        if let Some(ref s) = settings {
            if let Some(ref attrs) = s.searchable_attributes {
                // Strip `unordered(...)` wrapper and track which paths are unordered.
                self.searchable_paths = attrs
                    .iter()
                    .map(|a| {
                        let stripped = strip_unordered_prefix(a).to_string();
                        if stripped != *a {
                            self.unordered_paths.insert(stripped.clone());
                        }
                        stripped
                    })
                    .collect();
            }
        }
        self.settings = settings;
        self
    }

    pub fn with_query(mut self, query_text: String) -> Self {
        self.query_text = query_text;
        self
    }

    pub fn execute(
        &self,
        searcher: &Searcher,
        query: Box<dyn TantivyQuery>,
        filter: Option<&Filter>,
        limit: usize,
    ) -> Result<SearchResult> {
        self.execute_with_sort(searcher, query, filter, None, limit, false)
    }

    pub fn phase_report(&self) -> QueryPhaseReport {
        self.phase_cell.report()
    }

    /// Begin a serialized phase report for one execute call. The returned
    /// guard holds the executor's execution lock and finishes the report on
    /// drop, so concurrent executions cannot interleave counters.
    pub(in crate::query::executor) fn begin_phase_report(
        &self,
        searcher: &Searcher,
        execution_path: QueryExecutionPath,
    ) -> QueryPhaseGuard<'_> {
        self.phase_cell
            .begin(&self.converter, searcher, execution_path)
    }

    pub(in crate::query::executor) fn set_execution_path(
        &self,
        execution_path: QueryExecutionPath,
    ) {
        self.phase_cell.set_execution_path(execution_path);
    }

    pub(in crate::query::executor) fn observe_phase(&self, phase: QueryPhase, started_at: Instant) {
        self.phase_cell.observe(phase, started_at.elapsed());
    }

    pub(in crate::query::executor) fn set_matched_docs(&self, matched_docs: usize) {
        self.phase_cell.set_matched_docs(matched_docs);
    }

    pub(in crate::query::executor) fn set_candidates_collected(&self, candidates_collected: usize) {
        self.phase_cell
            .set_candidates_collected(candidates_collected);
    }

    pub(in crate::query::executor) fn add_facet_cardinality(&self, facet_cardinality: usize) {
        self.phase_cell.add_facet_cardinality(facet_cardinality);
    }

    pub(crate) fn apply_filter(
        &self,
        query: Box<dyn TantivyQuery>,
        filter: Option<&Filter>,
    ) -> Result<Box<dyn TantivyQuery>> {
        if let Some(f) = filter {
            let filter_query = self.filter_compiler.compile(f, self.settings.as_deref())?;
            Ok(Box::new(BooleanQuery::new(vec![
                (Occur::Must, query),
                (Occur::Must, filter_query),
            ])))
        } else {
            Ok(query)
        }
    }

    /// Wraps the query with Should + BoostQuery clauses for optional filters.
    /// Documents matching the optional filters get a score boost; non-matching
    /// documents are NOT excluded from results.
    pub fn apply_optional_boosts(
        &self,
        query: Box<dyn TantivyQuery>,
        specs: &[(String, String, f32)],
    ) -> Result<Box<dyn TantivyQuery>> {
        if specs.is_empty() {
            return Ok(query);
        }
        let json_filter_field = self
            .tantivy_schema
            .get_field("_json_filter")
            .map_err(|_| crate::error::FlapjackError::FieldNotFound("_json_filter".to_string()))?;

        let mut clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = vec![(Occur::Must, query)];

        for (field, value, score) in specs {
            // Build a term query on _json_filter.{field} for the value
            let term_text = format!("{}\0s{}", field, value.to_lowercase());
            let term = tantivy::Term::from_field_text(json_filter_field, &term_text);
            let term_query: Box<dyn TantivyQuery> =
                Box::new(TermQuery::new(term, IndexRecordOption::Basic));
            let boosted: Box<dyn TantivyQuery> = if *score != 1.0 {
                Box::new(BoostQuery::new(term_query, *score))
            } else {
                term_query
            };
            clauses.push((Occur::Should, boosted));
        }

        Ok(Box::new(BooleanQuery::new(clauses)))
    }

    // Expands short queries (≤2 chars) by enumerating matching terms from the index.
    // Recursively handles nested BooleanQueries containing ShortQueryPlaceholders.
    /// Replace `ShortQueryPlaceholder` nodes with real term queries by enumerating
    /// matching terms from the searcher's index segments.
    pub(crate) fn expand_short_query_with_searcher(
        &self,
        query: Box<dyn TantivyQuery>,
        searcher: &Searcher,
    ) -> Result<Box<dyn TantivyQuery>> {
        self.phase_cell.ensure_started();
        let started_at = Instant::now();
        let result = self.expand_short_query(query, searcher);
        self.observe_phase(QueryPhase::Prepare, started_at);
        result
    }

    fn expand_short_query(
        &self,
        query: Box<dyn TantivyQuery>,
        searcher: &Searcher,
    ) -> Result<Box<dyn TantivyQuery>> {
        let query_any = query.as_any();

        if let Some(placeholder) = query_any.downcast_ref::<ShortQueryPlaceholder>() {
            return self.expand_placeholder(placeholder, searcher);
        }

        if let Some(bool_query) = query_any.downcast_ref::<BooleanQuery>() {
            let clauses = bool_query.clauses();
            let mut new_clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = Vec::new();
            let mut changed = false;

            for (occur, sub_query) in clauses {
                if sub_query.as_any().is::<ShortQueryPlaceholder>() {
                    let placeholder = sub_query
                        .as_any()
                        .downcast_ref::<ShortQueryPlaceholder>()
                        .unwrap();
                    let expanded = self.expand_placeholder(placeholder, searcher)?;
                    new_clauses.push((*occur, expanded));
                    changed = true;
                } else if sub_query.as_any().is::<BooleanQuery>() {
                    let expanded = self.expand_short_query(
                        Box::new(
                            sub_query
                                .as_any()
                                .downcast_ref::<BooleanQuery>()
                                .unwrap()
                                .clone(),
                        ),
                        searcher,
                    )?;
                    new_clauses.push((*occur, expanded));
                    changed = true;
                } else {
                    new_clauses.push((*occur, sub_query.box_clone()));
                }
            }

            if changed {
                return Ok(Box::new(BooleanQuery::new(new_clauses)));
            }
        }

        Ok(query)
    }

    /// Expand a single placeholder by iterating the term dictionary for each searchable
    /// field, collecting prefix-matching terms up to a configurable limit.
    fn expand_placeholder(
        &self,
        placeholder: &ShortQueryPlaceholder,
        searcher: &Searcher,
    ) -> Result<Box<dyn TantivyQuery>> {
        let marker = &placeholder.marker;
        let mut term_queries: Vec<(Occur, Box<dyn TantivyQuery>)> = Vec::new();

        if let Some(segment) = searcher.segment_readers().first() {
            let inv_index = segment.inverted_index(marker.field)?;

            // Limit searchable paths and terms-per-path for short queries to
            // keep the resulting BooleanQuery manageable. EdgeNgramTokenFilter
            // passes tokens shorter than min_gram through unchanged, so short
            // query tokens are not guaranteed to have universal 1-char edge
            // n-gram coverage across all indexed words. These tighter caps
            // bound Boolean clause growth for 1-char queries (3 paths × 20
            // terms = 60 clauses) vs 2-char queries (5 paths × 50 terms =
            // 250 clauses).
            let is_single_char = marker.token.chars().count() == 1;
            let max_paths = if is_single_char { 3 } else { 5 }.min(marker.paths.len());
            let max_terms_per_path: usize = if is_single_char { 20 } else { 50 };
            for (path_idx, path) in marker.paths.iter().take(max_paths).enumerate() {
                let weight = marker.weights.get(path_idx).copied().unwrap_or(1.0);
                let prefix_bytes = format!("{}\0s{}", path, marker.token).into_bytes();
                let mut upper_bound = prefix_bytes.clone();
                upper_bound.push(0xFF);
                let mut terms = inv_index
                    .terms()
                    .range()
                    .ge(&prefix_bytes)
                    .lt(&upper_bound)
                    .into_stream()?;
                let mut count = 0;

                while terms.advance() && count < max_terms_per_path {
                    let term_bytes = terms.key();
                    let term = tantivy::Term::from_field_bytes(marker.field, term_bytes);
                    let term_query: Box<dyn TantivyQuery> = Box::new(TermQuery::new(
                        term,
                        IndexRecordOption::WithFreqsAndPositions,
                    ));
                    let boosted: Box<dyn TantivyQuery> = if weight != 1.0 {
                        Box::new(tantivy::query::BoostQuery::new(term_query, weight))
                    } else {
                        term_query
                    };
                    term_queries.push((Occur::Should, boosted));
                    count += 1;
                }
            }
        }

        if term_queries.is_empty() {
            Ok(Box::new(tantivy::query::EmptyQuery))
        } else {
            Ok(Box::new(BooleanQuery::new(term_queries)))
        }
    }

    pub(crate) fn reconstruct_documents(
        &self,
        searcher: &Searcher,
        doc_addresses: Vec<(f32, tantivy::DocAddress)>,
    ) -> Result<Vec<ScoredDocument>> {
        let document_count = doc_addresses.len();
        let mut documents_by_result_position = std::iter::repeat_with(|| None)
            .take(document_count)
            .collect::<Vec<_>>();

        // Fetch was 35.654067% of the measured facet path. Segment/doc ordering
        // improves stored-field locality while result positions preserve exact
        // hit order; document_fetch_tests and executor parity pin both sides.
        for (result_position, score, doc_address) in group_doc_addresses_for_fetch(doc_addresses) {
            let tantivy_doc = searcher.doc(doc_address)?;
            let document =
                self.converter
                    .from_tantivy(tantivy_doc, &self.tantivy_schema, String::new())?;
            documents_by_result_position[result_position] =
                Some(ScoredDocument { document, score });
        }

        Ok(documents_by_result_position
            .into_iter()
            .map(|document| document.expect("every fetch-plan entry must produce one document"))
            .collect())
    }

    /// Run one collector expression against `searcher` on the bounded executor.
    ///
    /// This is the single production entry point for collector-based search.
    /// Callers build their collector tuples exactly as they would for
    /// `Searcher::search`; only the dispatch is centralized here, so executor
    /// selection, pool reuse, and the in-flight budget have one owner.
    ///
    /// Any degraded resolution — a single-thread request, an unavailable pool,
    /// or an exhausted budget — runs the identical collector on the caller's
    /// own thread instead. Results therefore never depend on which path a
    /// given search took; only latency does.
    pub(in crate::query::executor) fn search_bounded<C: Collector>(
        &self,
        searcher: &Searcher,
        query: &dyn TantivyQuery,
        collector: &C,
    ) -> Result<C::Fruit> {
        match acquire_bounded_execution() {
            Some(permit) => Ok(searcher.search_with_executor(
                query,
                collector,
                permit.executor(),
                enable_scoring_for(searcher, collector),
            )?),
            None => Ok(searcher.search(query, collector)?),
        }
    }

    /// Assemble scored documents and total hit count into a `SearchResult`.
    pub(crate) fn build_result(
        &self,
        documents: Vec<ScoredDocument>,
        total: usize,
    ) -> SearchResult {
        SearchResult {
            documents,
            total,
            facets: std::collections::HashMap::new(),
            facets_stats: std::collections::HashMap::new(),
            user_data: Vec::new(),
            applied_rules: Vec::new(),
            parsed_query: self.query_text.clone(),
            exhaustive_facet_values: true,
            exhaustive_rules_match: true,
            query_after_removal: None,
            rendering_content: None,
            effective_around_lat_lng: None,
            effective_around_radius: None,
        }
    }
}

/// Environment variable selecting how many Tantivy worker threads a single
/// search may fan out across.
///
/// Resolution contract, owned by [`resolve_search_threads`]:
/// - unset -> [`DEFAULT_SEARCH_THREADS`]
/// - not a base-10 unsigned integer (empty, negative, fractional, overflowing)
///   -> `1`
/// - `0` -> `1`
/// - `1` -> `Executor::single_thread()`, i.e. collection on the caller thread
/// - `n > 1` -> the one cached `Executor::multi_thread(n, ..)` pool for `n`
///
/// Beyond resolution, execution falls back to `Executor::single_thread()`
/// whenever the pool is unavailable or its in-flight budget is exhausted. A
/// degraded executor never turns into a failed query, so this variable can only
/// change latency, never results.
const SEARCH_THREADS_ENV: &str = "FLAPJACK_SEARCH_THREADS";

/// Worker-thread count used when [`SEARCH_THREADS_ENV`] is unset.
///
/// `1` keeps the default path byte-identical to the pre-bounded-executor
/// engine. Stage 3's frozen-matrix benchmark found no reliable multithread win
/// at the measured load level, so the production default remains single-threaded.
/// Future measurements can change only this constant; no call site, resolver
/// branch, or cache key depends on its value.
const DEFAULT_SEARCH_THREADS: usize = 1;

/// Concurrent multithread searches admitted per pool worker thread.
///
/// Beyond this ceiling a search collects single-threaded on its own caller
/// thread rather than queueing behind the shared pool. That bounds pool
/// oversubscription — and therefore tail latency — without ever rejecting a
/// search.
///
/// `pub(crate)` so the executor benchmark can record the budget arm it measured
/// as row provenance; the value stays owned here.
pub(crate) const IN_FLIGHT_SEARCHES_PER_WORKER_THREAD: usize = 1;

/// Thread-name prefix for pool workers, so bounded search threads are
/// identifiable in stack dumps and profiles.
const SEARCH_POOL_THREAD_PREFIX: &str = "flapjack-search-";

/// Pool cache keyed only by resolved worker-thread count, so executor
/// ownership has exactly one source of truth: at most one pool build is
/// attempted per thread count for the lifetime of the process. Successful
/// pools are shared by every index and query; unavailable counts stay on the
/// single-thread path without retrying thread creation on every search.
static SEARCH_POOLS: Lazy<RwLock<HashMap<usize, SearchPoolCacheEntry>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

enum SearchPoolCacheEntry {
    Available(Arc<BoundedSearchPool>),
    Unavailable,
}

impl SearchPoolCacheEntry {
    fn pool(&self) -> Option<Arc<BoundedSearchPool>> {
        match self {
            SearchPoolCacheEntry::Available(pool) => Some(Arc::clone(pool)),
            SearchPoolCacheEntry::Unavailable => None,
        }
    }
}

/// One process-global Tantivy thread pool paired with the in-flight budget that
/// keeps concurrent searches from oversubscribing it.
///
/// The counters are always maintained rather than compiled out under `cfg(test)`
/// so the budget path under test is the same code production runs; only the
/// accessors that read them are test-only.
struct BoundedSearchPool {
    executor: Executor,
    budget: usize,
    in_flight: AtomicUsize,
    in_flight_high_water: AtomicUsize,
    multithread_executions: AtomicUsize,
}

impl BoundedSearchPool {
    fn new(executor: Executor, thread_count: usize) -> Self {
        BoundedSearchPool {
            executor,
            budget: thread_count.saturating_mul(IN_FLIGHT_SEARCHES_PER_WORKER_THREAD),
            in_flight: AtomicUsize::new(0),
            in_flight_high_water: AtomicUsize::new(0),
            multithread_executions: AtomicUsize::new(0),
        }
    }

    /// Reserve one in-flight slot, or return `None` when the budget is already
    /// fully committed. The returned permit releases the slot on drop, so every
    /// exit from a search — normal return, `?` propagation, or unwind — frees
    /// it exactly once.
    fn try_acquire(self: &Arc<Self>) -> Option<InFlightPermit> {
        let mut observed = self.in_flight.load(AtomicOrdering::Acquire);
        loop {
            if observed >= self.budget {
                return None;
            }
            match self.in_flight.compare_exchange_weak(
                observed,
                observed + 1,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            ) {
                Ok(_) => {
                    self.in_flight_high_water
                        .fetch_max(observed + 1, AtomicOrdering::AcqRel);
                    self.multithread_executions
                        .fetch_add(1, AtomicOrdering::Relaxed);
                    return Some(InFlightPermit {
                        pool: Arc::clone(self),
                    });
                }
                Err(actual) => observed = actual,
            }
        }
    }
}

/// RAII reservation of one bounded multithread execution slot. Holding this
/// permit is what makes a search eligible for the shared pool; dropping it
/// returns the slot to the budget.
struct InFlightPermit {
    pool: Arc<BoundedSearchPool>,
}

impl InFlightPermit {
    fn executor(&self) -> &Executor {
        &self.pool.executor
    }
}

impl Drop for InFlightPermit {
    fn drop(&mut self) {
        self.pool.in_flight.fetch_sub(1, AtomicOrdering::AcqRel);
    }
}

/// Resolve the requested worker-thread count from [`SEARCH_THREADS_ENV`].
/// See that constant's documentation for the full contract.
fn resolve_search_threads() -> usize {
    match std::env::var(SEARCH_THREADS_ENV) {
        Ok(raw) => raw.trim().parse::<usize>().unwrap_or(1).max(1),
        Err(_) => DEFAULT_SEARCH_THREADS,
    }
}

/// Return the shared pool for `thread_count`, building it on first use.
///
/// `None` means the pool is unavailable — the worker threads could not be
/// spawned, or the cache lock was poisoned by a panicking writer — and the
/// caller must collect on its own thread instead.
fn bounded_pool(thread_count: usize) -> Option<Arc<BoundedSearchPool>> {
    bounded_pool_with(
        thread_count,
        |thread_count, thread_prefix| match Executor::multi_thread(thread_count, thread_prefix) {
            Ok(executor) => Some(executor),
            Err(error) => {
                tracing::warn!(
                    thread_count,
                    error = %error,
                    "bounded search pool unavailable; using caller-thread execution"
                );
                None
            }
        },
    )
}

fn bounded_pool_with(
    thread_count: usize,
    create_executor: impl FnOnce(usize, &'static str) -> Option<Executor>,
) -> Option<Arc<BoundedSearchPool>> {
    if let Some(entry) = SEARCH_POOLS.read().ok()?.get(&thread_count) {
        return entry.pool();
    }
    let mut pools = SEARCH_POOLS.write().ok()?;
    // Another thread may have inserted between the read lock being released
    // and the write lock being taken; reuse either its pool or its memoized
    // unavailable state rather than attempting another build.
    if let Some(entry) = pools.get(&thread_count) {
        return entry.pool();
    }
    let entry = match create_executor(thread_count, SEARCH_POOL_THREAD_PREFIX) {
        Some(executor) => SearchPoolCacheEntry::Available(Arc::new(BoundedSearchPool::new(
            executor,
            thread_count,
        ))),
        None => SearchPoolCacheEntry::Unavailable,
    };
    let pool = entry.pool();
    pools.insert(thread_count, entry);
    pool
}

/// Reserve bounded multithread execution for one search, or `None` when the
/// caller should collect on its own thread. `None` covers every degraded
/// resolution: a single-thread request, an unavailable pool, and an exhausted
/// in-flight budget.
fn acquire_bounded_execution() -> Option<InFlightPermit> {
    let thread_count = resolve_search_threads();
    if thread_count <= 1 {
        return None;
    }
    bounded_pool(thread_count)?.try_acquire()
}

/// Mirror `Searcher::search`'s own scoring decision so a bounded search scores
/// identically to the single-thread call it replaces.
fn enable_scoring_for<'a, C: Collector>(
    searcher: &'a Searcher,
    collector: &C,
) -> EnableScoring<'a> {
    if collector.requires_scoring() {
        EnableScoring::enabled_from_searcher(searcher)
    } else {
        EnableScoring::disabled_from_searcher(searcher)
    }
}

/// Snapshot of one pool's budget accounting, used by tests to prove the
/// in-flight ceiling holds and that the bounded path was actually taken.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoundedPoolCounters {
    budget: usize,
    in_flight: usize,
    in_flight_high_water: usize,
    multithread_executions: usize,
}

#[cfg(test)]
impl BoundedSearchPool {
    fn counters(&self) -> BoundedPoolCounters {
        BoundedPoolCounters {
            budget: self.budget,
            in_flight: self.in_flight.load(AtomicOrdering::Acquire),
            in_flight_high_water: self.in_flight_high_water.load(AtomicOrdering::Acquire),
            multithread_executions: self.multithread_executions.load(AtomicOrdering::Acquire),
        }
    }

    /// Clear the observation counters so one test's high-water reading cannot
    /// be inflated by an earlier test sharing the same cached pool. The live
    /// `in_flight` count is budget state, not an observation, so it is left
    /// alone.
    fn reset_counters(&self) {
        self.in_flight_high_water.store(0, AtomicOrdering::Release);
        self.multithread_executions
            .store(0, AtomicOrdering::Release);
    }
}
