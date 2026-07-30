use super::bounded_tests::{wait_for_pool_quiescence, SearchThreadsEnvGuard, TEST_THREAD_COUNT};
use super::parity_fixtures::{build_parity_fixture, geoloc, laptop_filter, ExecutorParityFixture};
use super::{FacetSearchParams, QueryPhaseReport};
use crate::query::geo::{AroundPrecisionConfig, AroundRadius, GeoParams, GeoPoint};
use crate::query::highlighter::{HighlightValue, Highlighter, MatchLevel};
use crate::types::{FacetRequest, FieldValue, Filter, Sort, SortOrder};
use tantivy::query::AllQuery;

fn ids(result: &crate::types::SearchResult) -> Vec<&str> {
    result
        .documents
        .iter()
        .map(|doc| doc.document.id.as_str())
        .collect()
}

fn object_id_sort() -> Sort {
    Sort::ByField {
        field: "objectID".to_string(),
        order: SortOrder::Asc,
    }
}

fn facet_requests() -> Vec<FacetRequest> {
    vec![
        FacetRequest {
            field: "category".to_string(),
            path: "/category".to_string(),
            value_query: None,
        },
        FacetRequest {
            field: "brand".to_string(),
            path: "/brand".to_string(),
            value_query: None,
        },
        FacetRequest {
            field: "tags".to_string(),
            path: "/tags".to_string(),
            value_query: None,
        },
        FacetRequest {
            field: "price".to_string(),
            path: "/price".to_string(),
            value_query: None,
        },
    ]
}

fn pager_filter() -> Filter {
    Filter::Equals {
        field: "category".to_string(),
        value: FieldValue::Text("Pager".to_string()),
    }
}

fn wireless_filter() -> Filter {
    Filter::Equals {
        field: "facetGroup".to_string(),
        value: FieldValue::Text("wireless".to_string()),
    }
}

fn brand_request(value_query: Option<&str>) -> [FacetRequest; 1] {
    [FacetRequest {
        field: "brand".to_string(),
        path: "/brand".to_string(),
        value_query: value_query.map(str::to_string),
    }]
}

fn execute_all_query_with_facets(
    fixture: &ExecutorParityFixture,
    executor: &crate::QueryExecutor,
    filter: Option<&Filter>,
    requests: &[FacetRequest],
    limit: usize,
) -> crate::types::SearchResult {
    executor
        .execute_with_facets(
            fixture.searcher(),
            Box::new(AllQuery),
            filter,
            &FacetSearchParams {
                sort: None,
                limit,
                offset: 0,
                has_text_query: false,
                facet_requests: Some(requests),
                distinct_count: None,
            },
        )
        .unwrap()
}

fn assert_facet_values(
    result: &crate::types::SearchResult,
    field: &str,
    expected: &[(&str, u64)],
    context: &str,
) {
    let actual: Vec<(&str, u64)> = result.facets[field]
        .iter()
        .map(|count| (count.path.as_str(), count.count))
        .collect();
    assert_eq!(actual, expected, "{context}");
}

fn assert_price_stats(result: &crate::types::SearchResult) {
    let price_stats = result
        .facets_stats
        .get("price")
        .expect("wireless price facet stats");
    assert_eq!(price_stats.min, 45.0);
    assert_eq!(price_stats.max, 330.0);
    assert_eq!(price_stats.sum, 1124.0);
    assert!((price_stats.avg - (1124.0 / 6.0)).abs() < 1e-12);
}

fn assert_wireless_brand_case(
    fixture: &ExecutorParityFixture,
    label: &'static str,
    value_query: Option<&str>,
    expected: &[(&str, u64)],
    exhaustive: bool,
) -> PhaseFingerprint {
    let requests = brand_request(value_query);
    let filter = wireless_filter();
    let executor = fixture.executor("").with_max_values_per_facet(Some(2));
    let result = execute_all_query_with_facets(fixture, &executor, Some(&filter), &requests, 0);
    assert_facet_values(&result, "brand", expected, label);
    assert_eq!(result.exhaustive_facet_values, exhaustive, "{label}");
    fingerprint(label, &executor)
}

/// The result-defining subset of `QueryPhaseReport`, captured per execute call.
///
/// Timing fields and `cold` are deliberately excluded: they are wall-clock and
/// reader-generation dependent, so they legitimately differ between two runs of
/// the same query and cannot witness an executor parity regression. Everything
/// retained here is derived from what the collectors saw, so it must be
/// identical on the single-thread and bounded multithread paths.
#[derive(Debug, PartialEq, Eq)]
struct PhaseFingerprint {
    label: &'static str,
    execution_path: &'static str,
    matched_docs: usize,
    visited_segments: usize,
    candidates_collected: usize,
    facet_cardinality: usize,
}

fn fingerprint(label: &'static str, executor: &crate::QueryExecutor) -> PhaseFingerprint {
    let report = executor.phase_report();
    PhaseFingerprint {
        label,
        execution_path: report.execution_path,
        matched_docs: report.matched_docs,
        visited_segments: report.visited_segments,
        candidates_collected: report.candidates_collected,
        facet_cardinality: report.facet_cardinality,
    }
}

fn assert_text_family(fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
    let sort = object_id_sort();
    let mut fingerprints = Vec::new();

    for spec in super::parity_fixtures::TEXT_SPECS {
        let executor = fixture.executor(spec.query);
        let result = executor
            .execute_with_sort(
                fixture.searcher(),
                fixture.text_query(spec),
                None,
                Some(&sort),
                super::parity_fixtures::SEARCH_LIMIT,
                true,
            )
            .unwrap();

        assert_eq!(ids(&result), spec.expected_ids, "{}", spec.name);
        assert_eq!(result.total, spec.expected_total, "{}", spec.name);
        fingerprints.push(fingerprint(spec.name, &executor));
    }

    fingerprints
}

fn assert_typo_family(fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
    let sort = object_id_sort();
    let mut fingerprints = Vec::new();

    for spec in super::parity_fixtures::TYPO_SPECS {
        let executor = fixture.executor(spec.query);
        let result = executor
            .execute_with_sort(
                fixture.searcher(),
                fixture.text_query(spec),
                None,
                Some(&sort),
                super::parity_fixtures::SEARCH_LIMIT,
                true,
            )
            .unwrap();

        assert_eq!(ids(&result), spec.expected_ids, "{}", spec.name);
        assert_eq!(result.total, spec.expected_total, "{}", spec.name);
        fingerprints.push(fingerprint(spec.name, &executor));
    }

    fingerprints
}

fn assert_multi_word_family(fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
    let sort = object_id_sort();
    let mut fingerprints = Vec::new();

    for spec in super::parity_fixtures::MULTI_WORD_SPECS {
        let executor = fixture.executor(spec.query);
        let result = executor
            .execute_with_sort(
                fixture.searcher(),
                fixture.text_query(spec),
                None,
                Some(&sort),
                super::parity_fixtures::SEARCH_LIMIT,
                true,
            )
            .unwrap();

        assert_eq!(ids(&result), spec.expected_ids, "{}", spec.name);
        assert_eq!(result.total, spec.expected_total, "{}", spec.name);
        fingerprints.push(fingerprint(spec.name, &executor));
    }

    fingerprints
}

fn assert_facet_family(fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
    let requests = facet_requests();
    let spec = &super::parity_fixtures::FACET_QUERY;
    let filter = wireless_filter();
    let executor = fixture.executor(spec.query);
    let result = execute_all_query_with_facets(fixture, &executor, Some(&filter), &requests, 5);

    assert_eq!(ids(&result), spec.expected_ids);
    assert_eq!(result.total, spec.expected_total);
    assert!(result.exhaustive_facet_values);
    for expected in super::parity_fixtures::FACET_EXPECTATIONS {
        assert_facet_values(&result, expected.field, expected.values, expected.field);
    }
    assert_price_stats(&result);

    let all_query_executor = fixture.executor("");
    let all_query_result = execute_all_query_with_facets(
        fixture,
        &all_query_executor,
        Some(&pager_filter()),
        &requests[0..1],
        3,
    );
    for doc in all_query_result.documents {
        assert_eq!(
            doc.score.to_bits(),
            super::parity_fixtures::ALL_QUERY_FILTER_SCORE_BITS
        );
    }

    vec![
        fingerprint("facet/wireless", &executor),
        assert_wireless_brand_case(
            fixture,
            "facet/limited",
            None,
            &[("Sony", 3), ("Apple", 1)],
            false,
        ),
        assert_wireless_brand_case(
            fixture,
            "facet/value_query_exact",
            Some("sony"),
            &[("Sony", 3)],
            true,
        ),
        assert_wireless_brand_case(
            fixture,
            "facet/value_query_truncated",
            Some("o"),
            &[("Sony", 3), ("Bose", 1)],
            false,
        ),
        fingerprint("facet/all_query_filter_scores", &all_query_executor),
    ]
}

fn assert_filter_family(fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
    let sort = object_id_sort();
    let filter = laptop_filter();
    let spec = super::parity_fixtures::QuerySpec {
        name: "laptop",
        query: "laptop",
        query_type: "prefixNone",
        expected_ids: super::parity_fixtures::FILTER_EXPECTED_IDS,
        expected_total: super::parity_fixtures::FILTER_TOTAL,
    };

    let executor = fixture.executor(spec.query);
    let result = executor
        .execute_with_sort(
            fixture.searcher(),
            fixture.text_query(&spec),
            Some(&filter),
            Some(&sort),
            super::parity_fixtures::SEARCH_LIMIT,
            true,
        )
        .unwrap();

    assert_eq!(ids(&result), spec.expected_ids);
    assert_eq!(result.total, spec.expected_total);

    vec![fingerprint("filter/laptop", &executor)]
}

fn assert_pagination_family(fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
    let requests = facet_requests();
    let filter = pager_filter();
    let mut concatenated = Vec::new();
    let mut fingerprints = Vec::new();

    for (page_idx, expected_ids) in super::parity_fixtures::PAGINATION_EXPECTED_PAGES
        .iter()
        .enumerate()
    {
        let executor = fixture.executor("");
        let result = executor
            .execute_with_facets(
                fixture.searcher(),
                Box::new(AllQuery),
                Some(&filter),
                &FacetSearchParams {
                    sort: None,
                    limit: super::parity_fixtures::PAGE_LIMIT,
                    offset: page_idx * super::parity_fixtures::PAGE_LIMIT,
                    has_text_query: false,
                    facet_requests: Some(&requests[0..1]),
                    distinct_count: None,
                },
            )
            .unwrap();

        let actual_ids = ids(&result);
        assert_eq!(actual_ids, *expected_ids, "page {page_idx}");
        assert_eq!(result.total, super::parity_fixtures::PAGINATION_TOTAL);
        concatenated.extend(actual_ids.into_iter().map(String::from));
        fingerprints.push(fingerprint("pagination/page", &executor));
    }

    let expected: Vec<String> = super::parity_fixtures::PAGINATION_EXPECTED_PAGES
        .iter()
        .flat_map(|page| page.iter().copied().map(String::from))
        .collect();
    concatenated.sort();
    let mut deduped = concatenated.clone();
    deduped.dedup();
    assert_eq!(deduped, expected);
    assert_eq!(concatenated, expected);

    fingerprints
}

fn assert_exact_nb_hits_family(fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
    let sort = object_id_sort();
    let spec = super::parity_fixtures::QuerySpec {
        name: "exactprobe",
        query: "exactprobe",
        query_type: "prefixNone",
        expected_ids: &[],
        expected_total: 13,
    };
    let executor = fixture.executor(spec.query);
    let result = executor
        .execute_with_sort(
            fixture.searcher(),
            fixture.text_query(&spec),
            None,
            Some(&sort),
            super::parity_fixtures::EXACT_NB_HITS_LIMIT,
            true,
        )
        .unwrap();

    // Tripwire against later approximate nbHits: this count is hand-enumerated and exceeds the returned hit limit.
    assert_eq!(
        ids(&result),
        super::parity_fixtures::EXACT_NB_HITS_EXPECTED_IDS
    );
    assert_eq!(result.total, spec.expected_total);
    assert!(result.total > super::parity_fixtures::EXACT_NB_HITS_LIMIT);

    vec![fingerprint("exact_nb_hits/exactprobe", &executor)]
}

fn assert_geo_family(fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
    let sort = object_id_sort();
    let spec = &super::parity_fixtures::GEO_QUERY;
    let executor = fixture.executor(spec.query);
    let result = executor
        .execute_with_sort(
            fixture.searcher(),
            fixture.text_query(spec),
            None,
            Some(&sort),
            super::parity_fixtures::SEARCH_LIMIT,
            true,
        )
        .unwrap();
    assert_eq!(ids(&result), spec.expected_ids);
    assert_eq!(result.total, spec.expected_total);

    let geo = GeoParams {
        around: Some(GeoPoint { lat: 0.0, lng: 0.0 }),
        around_radius: Some(AroundRadius::Meters(200)),
        bounding_boxes: Vec::new(),
        polygons: Vec::new(),
        around_precision: AroundPrecisionConfig::default(),
        minimum_around_radius: None,
    };
    let mut filtered = Vec::new();
    for doc in &result.documents {
        if let Some((lat, lng)) = geoloc(doc) {
            if geo.filter_point(lat, lng) {
                filtered.push((
                    doc.document.id.as_str(),
                    geo.distance_from_center(lat, lng).unwrap(),
                ));
            }
        }
    }

    assert_eq!(
        filtered.len(),
        super::parity_fixtures::GEO_FILTERED_DISTANCES.len()
    );
    for ((actual_id, actual_distance), (expected_id, expected_distance)) in filtered
        .iter()
        .zip(super::parity_fixtures::GEO_FILTERED_DISTANCES)
    {
        assert_eq!(actual_id, expected_id);
        assert!(
            (actual_distance - expected_distance).abs() <= 1e-6,
            "{actual_id}: {actual_distance} != {expected_distance}"
        );
    }

    vec![fingerprint("geo/around_origin", &executor)]
}

fn assert_highlight_family(fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
    let sort = object_id_sort();
    let spec = super::parity_fixtures::QuerySpec {
        name: "highlightprobe target",
        query: "highlightprobe target",
        query_type: "prefixNone",
        expected_ids: &[super::parity_fixtures::HIGHLIGHT_EXPECTATION.document_id],
        expected_total: 1,
    };
    let executor = fixture.executor(spec.query);
    let result = executor
        .execute_with_sort(
            fixture.searcher(),
            fixture.text_query(&spec),
            None,
            Some(&sort),
            1,
            true,
        )
        .unwrap();
    let doc = &result.documents[0].document;
    let query_words: Vec<String> = super::parity_fixtures::HIGHLIGHT_QUERY_WORDS
        .iter()
        .map(|word| word.to_string())
        .collect();
    let highlighted = Highlighter::new("<em>".to_string(), "</em>".to_string())
        .highlight_document(doc, &query_words);

    match &highlighted[super::parity_fixtures::HIGHLIGHT_EXPECTATION.field] {
        HighlightValue::Single(value) => {
            assert_eq!(
                value.value,
                super::parity_fixtures::HIGHLIGHT_EXPECTATION.value
            );
            assert!(matches!(value.match_level, MatchLevel::Full));
        }
        _ => panic!("expected single highlight value"),
    }

    vec![fingerprint("highlight/target", &executor)]
}

fn assert_custom_ranking_family(fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
    let spec = &super::parity_fixtures::CUSTOM_RANKING_QUERY;
    let executor = fixture.custom_ranking_executor(spec.query);
    let result = executor
        .execute_with_sort(
            fixture.searcher(),
            fixture.text_query(spec),
            None,
            None,
            super::parity_fixtures::SEARCH_LIMIT,
            true,
        )
        .unwrap();

    assert_eq!(ids(&result), spec.expected_ids);
    assert_eq!(result.total, spec.expected_total);

    vec![fingerprint("custom_ranking/tiebreak", &executor)]
}

fn assert_count_only_path(fixture: &ExecutorParityFixture) -> PhaseFingerprint {
    let executor = fixture.executor("");
    let result = executor
        .execute_with_facets(
            fixture.searcher(),
            Box::new(AllQuery),
            Some(&pager_filter()),
            &FacetSearchParams {
                sort: None,
                limit: 0,
                offset: 0,
                has_text_query: false,
                facet_requests: None,
                distinct_count: None,
            },
        )
        .unwrap();

    assert!(result.documents.is_empty());
    assert_eq!(result.total, super::parity_fixtures::PAGINATION_TOTAL);
    let fingerprint = fingerprint("execution_path/count_only", &executor);
    assert_eq!(fingerprint.execution_path, "count_only");
    fingerprint
}

fn assert_sort_fast_path(fixture: &ExecutorParityFixture) -> PhaseFingerprint {
    let sort = object_id_sort();
    let executor = fixture.executor("");
    let result = executor
        .execute_with_sort(
            fixture.searcher(),
            Box::new(AllQuery),
            Some(&pager_filter()),
            Some(&sort),
            super::parity_fixtures::PAGE_LIMIT,
            false,
        )
        .unwrap();

    assert_eq!(
        ids(&result),
        super::parity_fixtures::PAGINATION_EXPECTED_PAGES[0]
    );
    assert_eq!(result.total, super::parity_fixtures::PAGINATION_TOTAL);
    let fingerprint = fingerprint("execution_path/sort_fast", &executor);
    assert_eq!(fingerprint.execution_path, "sort_fast");
    fingerprint
}

/// Run every frozen query family plus the otherwise-unreached count-only and
/// pure-sort paths against one fixture. Sharing a single fixture keeps the
/// eight-segment layout — and therefore the segment merge order under test —
/// fixed across executor configurations and repeated rounds.
fn assert_all_parity_families(fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
    let mut fingerprints = Vec::new();
    fingerprints.extend(assert_text_family(fixture));
    fingerprints.extend(assert_typo_family(fixture));
    fingerprints.extend(assert_multi_word_family(fixture));
    fingerprints.extend(assert_facet_family(fixture));
    fingerprints.extend(assert_filter_family(fixture));
    fingerprints.extend(assert_pagination_family(fixture));
    fingerprints.extend(assert_exact_nb_hits_family(fixture));
    fingerprints.extend(assert_geo_family(fixture));
    fingerprints.extend(assert_highlight_family(fixture));
    fingerprints.extend(assert_custom_ranking_family(fixture));
    fingerprints.push(assert_count_only_path(fixture));
    fingerprints.push(assert_sort_fast_path(fixture));
    fingerprints
}

/// The ten frozen query families Stage 2 locked down, addressed as one
/// benchmarkable catalog so the executor performance harness measures exactly
/// the queries the parity suite already asserts — never a second corpus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrozenFamily {
    Text,
    Typo,
    MultiWord,
    Facet,
    Filter,
    Pagination,
    ExactNbHits,
    Geo,
    Highlight,
    CustomRanking,
}

impl FrozenFamily {
    /// Every frozen family, in the fixed order the parity sweep runs them.
    pub(crate) const ALL: &'static [FrozenFamily] = &[
        FrozenFamily::Text,
        FrozenFamily::Typo,
        FrozenFamily::MultiWord,
        FrozenFamily::Facet,
        FrozenFamily::Filter,
        FrozenFamily::Pagination,
        FrozenFamily::ExactNbHits,
        FrozenFamily::Geo,
        FrozenFamily::Highlight,
        FrozenFamily::CustomRanking,
    ];

    /// Stable machine-readable family label for benchmark rows.
    pub(crate) fn label(self) -> &'static str {
        match self {
            FrozenFamily::Text => "text",
            FrozenFamily::Typo => "typo",
            FrozenFamily::MultiWord => "multi_word",
            FrozenFamily::Facet => "facet",
            FrozenFamily::Filter => "filter",
            FrozenFamily::Pagination => "pagination",
            FrozenFamily::ExactNbHits => "exact_nb_hits",
            FrozenFamily::Geo => "geo",
            FrozenFamily::Highlight => "highlight",
            FrozenFamily::CustomRanking => "custom_ranking",
        }
    }

    fn assert_family(self, fixture: &ExecutorParityFixture) -> Vec<PhaseFingerprint> {
        match self {
            FrozenFamily::Text => assert_text_family(fixture),
            FrozenFamily::Typo => assert_typo_family(fixture),
            FrozenFamily::MultiWord => assert_multi_word_family(fixture),
            FrozenFamily::Facet => assert_facet_family(fixture),
            FrozenFamily::Filter => assert_filter_family(fixture),
            FrozenFamily::Pagination => assert_pagination_family(fixture),
            FrozenFamily::ExactNbHits => assert_exact_nb_hits_family(fixture),
            FrozenFamily::Geo => assert_geo_family(fixture),
            FrozenFamily::Highlight => assert_highlight_family(fixture),
            FrozenFamily::CustomRanking => assert_custom_ranking_family(fixture),
        }
    }
}

/// Run one frozen family under the current `FLAPJACK_SEARCH_THREADS` arm and
/// return each execute's `QueryPhaseReport` paired with the query label the
/// parity suite assigns it.
///
/// This reuses the `assert_*_family` owners verbatim — so the benchmark
/// measures the asserted queries and nothing else — and pairs their
/// per-execute fingerprints with the reports captured for the same executes.
/// Both are produced in execution order, one per execute, so the zip is exact.
pub(crate) fn run_frozen_family(
    family: FrozenFamily,
    fixture: &ExecutorParityFixture,
) -> Vec<(&'static str, QueryPhaseReport)> {
    let (fingerprints, reports) =
        super::capture_query_phase_reports(|| family.assert_family(fixture));
    assert_eq!(
        fingerprints.len(),
        reports.len(),
        "each frozen-family execute must emit exactly one captured phase report"
    );
    fingerprints
        .into_iter()
        .map(|fingerprint| fingerprint.label)
        .zip(reports)
        .collect()
}

#[test]
fn parity_fixture_records_deterministic_segment_count() {
    let fixture = build_parity_fixture();
    let second_fixture = build_parity_fixture();

    assert_eq!(
        fixture.segment_count(),
        super::parity_fixtures::EXPECTED_SEGMENT_COUNT
    );
    assert!(fixture.segment_count() >= 2);
    assert_eq!(
        second_fixture.segment_count(),
        super::parity_fixtures::EXPECTED_SEGMENT_COUNT
    );
}

#[test]
fn text_family_matches_frozen_benchmark_terms() {
    assert_text_family(&build_parity_fixture());
}

#[test]
fn typo_family_matches_frozen_benchmark_typos() {
    assert_typo_family(&build_parity_fixture());
}

#[test]
fn multi_word_family_matches_frozen_benchmark_phrases() {
    assert_multi_word_family(&build_parity_fixture());
}

#[test]
fn facet_family_asserts_counts_ordering_and_constant_scores() {
    assert_facet_family(&build_parity_fixture());
}

#[test]
fn filter_family_returns_exact_hand_counted_hits() {
    assert_filter_family(&build_parity_fixture());
}

#[test]
fn pagination_family_uses_offset_capable_facet_path() {
    assert_pagination_family(&build_parity_fixture());
}

#[test]
fn exact_nb_hits_family_keeps_total_exact_above_requested_limit() {
    assert_exact_nb_hits_family(&build_parity_fixture());
}

#[test]
fn geo_family_filters_executor_results_with_precise_distances() {
    assert_geo_family(&build_parity_fixture());
}

#[test]
fn highlight_family_asserts_exact_em_strings_and_match_levels() {
    assert_highlight_family(&build_parity_fixture());
}

#[test]
fn custom_ranking_family_uses_doc_id_tiebreak_for_equal_keys() {
    assert_custom_ranking_family(&build_parity_fixture());
}

/// Full ten-family sweeps executed on the bounded multithread path. Repetition
/// is what turns a one-shot equality check into a stability check: per-segment
/// task scheduling differs between rounds, so a merge-order-dependent
/// regression that hides on the first round surfaces on a later one.
#[test]
#[serial_test::serial(flapjack_search_threads_env)]
fn executor_parallelism_is_bounded_and_deterministic() {
    let fixture = build_parity_fixture();

    // Every family below also asserts against the frozen single-thread
    // benchmark constants: hit IDs, surfaced score bits, total hits, facet
    // ordering, and exact phase fingerprints all share one frozen owner.
    let single_thread_fingerprints = {
        let _env = SearchThreadsEnvGuard::set("1");
        assert_all_parity_families(&fixture)
    };

    let _env = SearchThreadsEnvGuard::set(&TEST_THREAD_COUNT.to_string());
    let pool = super::bounded_pool(TEST_THREAD_COUNT).expect("bounded pool for 2 threads");
    pool.reset_counters();

    for round in 0..3 {
        let bounded_fingerprints = assert_all_parity_families(&fixture);
        assert_eq!(
            bounded_fingerprints, single_thread_fingerprints,
            "bounded round {round} diverged from the single-thread phase reports"
        );
    }

    let counters = wait_for_pool_quiescence(&pool);
    assert_eq!(counters.budget, TEST_THREAD_COUNT);
    assert!(
        counters.multithread_executions > 0,
        "no search took the bounded multithread path, so this parity sweep proved nothing"
    );
    assert_eq!(
        counters.in_flight, 0,
        "every permit must be released once the sweep finishes"
    );
    assert!(
        counters.in_flight_high_water <= TEST_THREAD_COUNT,
        "in-flight high water {} exceeded the budget of {TEST_THREAD_COUNT}",
        counters.in_flight_high_water
    );
}
