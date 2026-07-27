use super::parity_fixtures::{
    build_parity_fixture, QuerySpec, CUSTOM_RANKING_QUERY, EXACT_NB_HITS_LIMIT,
    EXPECTED_SEGMENT_COUNT, FACET_EXPECTATIONS, FACET_QUERY, FILTER_TOTAL, GEO_QUERY,
    HIGHLIGHT_EXPECTATION, MULTI_WORD_SPECS, SEARCH_LIMIT, TEXT_SPECS, TOTAL_DOCS, TYPO_SPECS,
};
use super::{FacetSearchParams, QueryExecutor, QueryPhaseReport};
use crate::types::{FacetRequest, FieldValue, Filter, Sort, SortOrder};
use std::sync::Arc;
use tantivy::query::AllQuery;

fn object_id_sort() -> Sort {
    Sort::ByField {
        field: "objectID".to_string(),
        order: SortOrder::Asc,
    }
}

fn facet_requests() -> Vec<FacetRequest> {
    FACET_EXPECTATIONS
        .iter()
        .map(|expectation| FacetRequest {
            field: expectation.field.to_string(),
            path: format!("/{}", expectation.field),
            value_query: None,
        })
        .collect()
}

fn assert_phase_budget(
    family: &str,
    report: QueryPhaseReport,
    requires_fetch: bool,
    requires_facets: bool,
) {
    let attributed_ns = report
        .prepare_ns
        .checked_add(report.collect_ns)
        .and_then(|sum| sum.checked_add(report.rank_ns))
        .and_then(|sum| sum.checked_add(report.fetch_ns))
        .and_then(|sum| sum.checked_add(report.facet_extract_ns))
        .expect("phase durations must not overflow");
    let expected_unattributed_ns = report
        .total_ns
        .checked_sub(attributed_ns)
        .expect("attributed phases must not exceed total time");

    assert_eq!(
        report.unattributed_ns, expected_unattributed_ns,
        "{family}: residual"
    );
    assert_eq!(
        attributed_ns + report.unattributed_ns,
        report.total_ns,
        "{family}: reconciliation"
    );
    // Scheduler and clock noise may consume up to 2ms or 5% of total wall time.
    let residual_tolerance_ns = 2_000_000_u64.max(report.total_ns / 20);
    assert!(
        report.unattributed_ns <= residual_tolerance_ns,
        "{family}: unattributed {}ns exceeds {}ns",
        report.unattributed_ns,
        residual_tolerance_ns
    );
    assert!(report.collect_ns > 0, "{family}: collect phase");
    if requires_fetch {
        assert!(report.fetch_ns > 0, "{family}: fetch phase");
    }
    if requires_facets {
        assert!(report.facet_extract_ns > 0, "{family}: facet phase");
        assert!(
            report.facet_cardinality > 0,
            "{family}: facet collector participation"
        );
    }
}

fn execute_text_family(
    fixture: &super::parity_fixtures::ExecutorParityFixture,
    executor: &QueryExecutor,
    spec: &QuerySpec,
    filter: Option<&Filter>,
    limit: usize,
) -> QueryPhaseReport {
    executor
        .execute_with_sort(
            fixture.searcher(),
            fixture.text_query(spec),
            filter,
            Some(&object_id_sort()),
            limit,
            true,
        )
        .unwrap();
    executor.phase_report()
}

#[test]
fn cold_query_phase_budget_attributes_count_rank_fetch_and_facet() {
    let fixture = build_parity_fixture();

    for (family, spec) in [
        ("text", &TEXT_SPECS[0]),
        ("typo", &TYPO_SPECS[0]),
        ("multi_word", &MULTI_WORD_SPECS[0]),
    ] {
        let executor = fixture.executor(spec.query);
        let report = execute_text_family(&fixture, &executor, spec, None, SEARCH_LIMIT);
        assert_phase_budget(family, report, true, false);
    }

    let facet_filter = Filter::Equals {
        field: "facetGroup".to_string(),
        value: FieldValue::Text("wireless".to_string()),
    };
    let requests = facet_requests();
    let facet_executor = fixture.executor(FACET_QUERY.query);
    facet_executor
        .execute_with_facets(
            fixture.searcher(),
            Box::new(AllQuery),
            Some(&facet_filter),
            &FacetSearchParams {
                sort: None,
                limit: 5,
                offset: 0,
                has_text_query: false,
                facet_requests: Some(&requests),
                distinct_count: None,
            },
        )
        .unwrap();
    assert_phase_budget("facet", facet_executor.phase_report(), true, true);

    let filter_spec = QuerySpec {
        name: "laptop",
        query: "laptop",
        query_type: "prefixNone",
        expected_ids: &[],
        expected_total: FILTER_TOTAL,
    };
    let laptop_filter = Filter::And(vec![
        Filter::GreaterThanOrEqual {
            field: "price".to_string(),
            value: FieldValue::Integer(500),
        },
        Filter::LessThanOrEqual {
            field: "price".to_string(),
            value: FieldValue::Integer(2500),
        },
        Filter::Equals {
            field: "inStock".to_string(),
            value: FieldValue::Bool(true),
        },
        Filter::Equals {
            field: "releaseYear".to_string(),
            value: FieldValue::Integer(2024),
        },
    ]);
    let filter_executor = fixture.executor(filter_spec.query);
    assert_phase_budget(
        "filter",
        execute_text_family(
            &fixture,
            &filter_executor,
            &filter_spec,
            Some(&laptop_filter),
            SEARCH_LIMIT,
        ),
        true,
        false,
    );

    let pagination_executor = fixture.executor("");
    pagination_executor
        .execute_with_facets(
            fixture.searcher(),
            Box::new(AllQuery),
            Some(&Filter::Equals {
                field: "category".to_string(),
                value: FieldValue::Text("Pager".to_string()),
            }),
            &FacetSearchParams {
                sort: None,
                limit: 3,
                offset: 3,
                has_text_query: false,
                facet_requests: Some(&requests[0..1]),
                distinct_count: None,
            },
        )
        .unwrap();
    assert_phase_budget("pagination", pagination_executor.phase_report(), true, true);

    let exact_spec = QuerySpec {
        name: "exactprobe",
        query: "exactprobe",
        query_type: "prefixNone",
        expected_ids: &[],
        expected_total: 13,
    };
    let exact_executor = fixture.executor(exact_spec.query);
    assert_phase_budget(
        "exact_nb_hits",
        execute_text_family(
            &fixture,
            &exact_executor,
            &exact_spec,
            None,
            EXACT_NB_HITS_LIMIT,
        ),
        true,
        false,
    );

    for (family, spec) in [
        ("geo", &GEO_QUERY),
        (
            "highlight",
            &QuerySpec {
                name: "highlightprobe target",
                query: "highlightprobe target",
                query_type: "prefixNone",
                expected_ids: &[HIGHLIGHT_EXPECTATION.document_id],
                expected_total: 1,
            },
        ),
    ] {
        let executor = fixture.executor(spec.query);
        assert_phase_budget(
            family,
            execute_text_family(&fixture, &executor, spec, None, SEARCH_LIMIT),
            true,
            false,
        );
    }

    let custom_executor = fixture.custom_ranking_executor(CUSTOM_RANKING_QUERY.query);
    custom_executor
        .execute_with_sort(
            fixture.searcher(),
            fixture.text_query(&CUSTOM_RANKING_QUERY),
            None,
            None,
            SEARCH_LIMIT,
            true,
        )
        .unwrap();
    assert_phase_budget(
        "custom_ranking",
        custom_executor.phase_report(),
        true,
        false,
    );
}

#[test]
fn metrics_report_matched_docs_visited_segments_and_facet_cardinality() {
    let fixture = build_parity_fixture();
    let custom_executor = fixture.custom_ranking_executor(CUSTOM_RANKING_QUERY.query);
    custom_executor
        .execute_with_sort(
            fixture.searcher(),
            fixture.text_query(&CUSTOM_RANKING_QUERY),
            None,
            None,
            SEARCH_LIMIT,
            true,
        )
        .unwrap();
    let custom_report = custom_executor.phase_report();
    assert_eq!(
        custom_report.matched_docs,
        CUSTOM_RANKING_QUERY.expected_total
    );
    assert_eq!(custom_report.visited_segments, EXPECTED_SEGMENT_COUNT);
    assert_eq!(
        custom_report.candidates_collected,
        CUSTOM_RANKING_QUERY.expected_total
    );

    let requests = facet_requests();
    let facet_executor = fixture.executor(FACET_QUERY.query);
    facet_executor
        .execute_with_facets(
            fixture.searcher(),
            Box::new(AllQuery),
            Some(&Filter::Equals {
                field: "facetGroup".to_string(),
                value: FieldValue::Text("wireless".to_string()),
            }),
            &FacetSearchParams {
                sort: None,
                limit: 5,
                offset: 0,
                has_text_query: false,
                facet_requests: Some(&requests),
                distinct_count: None,
            },
        )
        .unwrap();
    let facet_report = facet_executor.phase_report();
    assert_eq!(facet_report.matched_docs, FACET_QUERY.expected_total);
    assert_eq!(facet_report.visited_segments, EXPECTED_SEGMENT_COUNT);
    assert_eq!(
        facet_report.facet_cardinality,
        FACET_EXPECTATIONS
            .iter()
            .map(|expectation| expectation.values.len())
            .sum::<usize>()
    );
}

#[test]
fn query_phase_histogram_preseeds_every_phase_and_execution_path() {
    let families = super::gather_query_phase_metric_families();
    let family = families
        .iter()
        .find(|family| family.get_name() == "flapjack_query_phase_seconds")
        .expect("query phase histogram family");
    let actual_labels: std::collections::HashSet<(String, String)> = family
        .get_metric()
        .iter()
        .map(|metric| {
            let labels: std::collections::HashMap<&str, &str> = metric
                .get_label()
                .iter()
                .map(|label| (label.get_name(), label.get_value()))
                .collect();
            (
                labels["phase"].to_string(),
                labels["execution_path"].to_string(),
            )
        })
        .collect();
    let expected_labels: std::collections::HashSet<(String, String)> = [
        "prepare",
        "collect",
        "rank",
        "fetch",
        "facet_extract",
        "unattributed",
    ]
    .into_iter()
    .flat_map(|phase| {
        [
            "relevance",
            "relevance_facets",
            "sort_fast",
            "sort_fallback",
            "count_only",
        ]
        .into_iter()
        .map(move |path| (phase.to_string(), path.to_string()))
    })
    .collect();

    assert_eq!(actual_labels, expected_labels);
}

#[test]
fn independent_indexes_with_matching_reader_generations_are_both_cold() {
    let first_fixture = build_parity_fixture();
    let second_fixture = build_parity_fixture();
    assert_eq!(
        first_fixture.searcher().generation().generation_id(),
        second_fixture.searcher().generation().generation_id(),
        "fixture readers must reproduce the generation-ID collision"
    );

    let first_executor = first_fixture.executor("");
    first_executor
        .execute_with_facets(
            first_fixture.searcher(),
            Box::new(AllQuery),
            None,
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
    let second_executor = second_fixture.executor("");
    second_executor
        .execute_with_facets(
            second_fixture.searcher(),
            Box::new(AllQuery),
            None,
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

    assert!(first_executor.phase_report().cold);
    assert!(second_executor.phase_report().cold);

    let repeated_executor = first_fixture.executor("");
    repeated_executor
        .execute_with_facets(
            first_fixture.searcher(),
            Box::new(AllQuery),
            None,
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
    assert!(!repeated_executor.phase_report().cold);
}

#[test]
fn generation_tracker_retains_only_live_indexes_current_generation() {
    let tracker = super::metrics::SearcherGenerationTracker::<()>::default();
    let first_index = Arc::new(());
    let second_index = Arc::new(());

    assert!(tracker.observe(&first_index, 0));
    assert!(!tracker.observe(&first_index, 0));
    assert!(tracker.observe(&second_index, 0));
    assert_eq!(tracker.tracked_index_count(), 2);

    assert!(tracker.observe(&first_index, 1));
    assert!(!tracker.observe(&first_index, 1));
    assert_eq!(
        tracker.tracked_index_count(),
        2,
        "generation changes must replace state rather than append it"
    );

    drop(second_index);
    let replacement_index = Arc::new(());
    assert!(tracker.observe(&replacement_index, 0));
    assert_eq!(
        tracker.tracked_index_count(),
        2,
        "dead index identities must be pruned before retaining a new index"
    );
}

#[test]
fn zero_limit_count_only_applies_distinct_contract() {
    let fixture = build_parity_fixture();
    let wireless_filter = Filter::Equals {
        field: "facetGroup".to_string(),
        value: FieldValue::Text("wireless".to_string()),
    };

    // Control: without distinct, the zero-limit count-only shortcut reports
    // the raw matched-document count.
    let plain_executor = fixture.executor("");
    let plain = plain_executor
        .execute_with_facets(
            fixture.searcher(),
            Box::new(AllQuery),
            Some(&wireless_filter),
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
    assert_eq!(plain.total, FACET_QUERY.expected_total);
    assert!(plain.documents.is_empty());

    // Zero-limit searches carry no documents, so apply_distinct deduplicates an
    // empty document set and reports zero groups. The faceted zero-limit branch
    // has applied this contract since before this stage; the no-facet
    // count-only shortcut must agree with it exactly.
    let requests = facet_requests();
    for (shape, facet_requests) in [
        ("no_facets", None),
        ("with_facets", Some(requests.as_slice())),
    ] {
        let executor = fixture.distinct_executor("", "brand");
        let result = executor
            .execute_with_facets(
                fixture.searcher(),
                Box::new(AllQuery),
                Some(&wireless_filter),
                &FacetSearchParams {
                    sort: None,
                    limit: 0,
                    offset: 0,
                    has_text_query: false,
                    facet_requests,
                    distinct_count: Some(1),
                },
            )
            .unwrap();
        assert_eq!(result.total, 0, "{shape}: distinct zero-limit total");
        assert!(result.documents.is_empty(), "{shape}: documents");
    }
}

#[test]
fn concurrent_executions_emit_independent_phase_reports() {
    const HEAVY_ITERATIONS: usize = 30;
    const COUNT_ONLY_ITERATIONS: usize = 300;

    let fixture = build_parity_fixture();
    let executor = Arc::new(fixture.executor(""));
    let start = Arc::new(std::sync::Barrier::new(2));

    let heavy_handle = {
        let executor = Arc::clone(&executor);
        let searcher = fixture.searcher().clone();
        let start = Arc::clone(&start);
        std::thread::spawn(move || {
            super::capture_query_phase_reports(|| {
                start.wait();
                let sort = object_id_sort();
                for _ in 0..HEAVY_ITERATIONS {
                    executor
                        .execute_with_sort(
                            &searcher,
                            Box::new(AllQuery),
                            None,
                            Some(&sort),
                            SEARCH_LIMIT,
                            true,
                        )
                        .unwrap();
                }
            })
            .1
        })
    };

    let count_only_handle = {
        let executor = Arc::clone(&executor);
        let searcher = fixture.searcher().clone();
        let start = Arc::clone(&start);
        std::thread::spawn(move || {
            super::capture_query_phase_reports(|| {
                start.wait();
                let filter = Filter::Equals {
                    field: "facetGroup".to_string(),
                    value: FieldValue::Text("wireless".to_string()),
                };
                for _ in 0..COUNT_ONLY_ITERATIONS {
                    executor
                        .execute_with_facets(
                            &searcher,
                            Box::new(AllQuery),
                            Some(&filter),
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
                }
            })
            .1
        })
    };

    let heavy_reports = heavy_handle.join().unwrap();
    let count_only_reports = count_only_handle.join().unwrap();

    assert_eq!(heavy_reports.len(), HEAVY_ITERATIONS);
    for report in &heavy_reports {
        assert_eq!(report.execution_path, "sort_fallback", "heavy: path");
        assert_eq!(report.matched_docs, TOTAL_DOCS, "heavy: matched_docs");
        assert_eq!(
            report.visited_segments, EXPECTED_SEGMENT_COUNT,
            "heavy: visited_segments"
        );
        assert_eq!(
            report.candidates_collected, TOTAL_DOCS,
            "heavy: candidates_collected"
        );
        assert_phase_budget("heavy", *report, true, false);
    }

    assert_eq!(count_only_reports.len(), COUNT_ONLY_ITERATIONS);
    for report in &count_only_reports {
        assert_eq!(report.execution_path, "count_only", "count_only: path");
        assert_eq!(
            report.matched_docs, FACET_QUERY.expected_total,
            "count_only: matched_docs"
        );
        assert_eq!(
            report.visited_segments, EXPECTED_SEGMENT_COUNT,
            "count_only: visited_segments"
        );
        assert_eq!(
            report.candidates_collected, 0,
            "count_only: candidates_collected"
        );
        assert_eq!(report.facet_cardinality, 0, "count_only: facet_cardinality");
        assert_phase_budget("count_only", *report, false, false);
    }
}
