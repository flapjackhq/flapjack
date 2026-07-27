use super::parity_fixtures::build_parity_fixture;
use super::FacetSearchParams;
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
    ]
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
    let fixture = build_parity_fixture();
    let sort = object_id_sort();

    for spec in super::parity_fixtures::TEXT_SPECS {
        let result = fixture
            .executor(spec.query)
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
    }
}

#[test]
fn typo_family_matches_frozen_benchmark_typos() {
    let fixture = build_parity_fixture();
    let sort = object_id_sort();

    for spec in super::parity_fixtures::TYPO_SPECS {
        let result = fixture
            .executor(spec.query)
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
    }
}

#[test]
fn multi_word_family_matches_frozen_benchmark_phrases() {
    let fixture = build_parity_fixture();
    let sort = object_id_sort();

    for spec in super::parity_fixtures::MULTI_WORD_SPECS {
        let result = fixture
            .executor(spec.query)
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
    }
}

#[test]
fn facet_family_asserts_counts_ordering_and_constant_scores() {
    let fixture = build_parity_fixture();
    let requests = facet_requests();
    let spec = &super::parity_fixtures::FACET_QUERY;
    let result = fixture
        .executor(spec.query)
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

    assert_eq!(ids(&result), spec.expected_ids);
    assert_eq!(result.total, spec.expected_total);
    assert!(result.exhaustive_facet_values);
    for expected in super::parity_fixtures::FACET_EXPECTATIONS {
        let actual: Vec<(&str, u64)> = result.facets[expected.field]
            .iter()
            .map(|count| (count.path.as_str(), count.count))
            .collect();
        assert_eq!(actual, expected.values, "{}", expected.field);
    }

    let all_query_result = fixture
        .executor("")
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
                offset: 0,
                has_text_query: false,
                facet_requests: Some(&requests[0..1]),
                distinct_count: None,
            },
        )
        .unwrap();
    for doc in all_query_result.documents {
        assert_eq!(
            doc.score.to_bits(),
            super::parity_fixtures::ALL_QUERY_FILTER_SCORE_BITS
        );
    }
}

#[test]
fn filter_family_returns_exact_hand_counted_hits() {
    let fixture = build_parity_fixture();
    let sort = object_id_sort();
    let filter = laptop_filter();
    let spec = super::parity_fixtures::QuerySpec {
        name: "laptop",
        query: "laptop",
        query_type: "prefixNone",
        expected_ids: super::parity_fixtures::FILTER_EXPECTED_IDS,
        expected_total: super::parity_fixtures::FILTER_TOTAL,
    };

    let result = fixture
        .executor(spec.query)
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
}

#[test]
fn pagination_family_uses_offset_capable_facet_path() {
    let fixture = build_parity_fixture();
    let requests = facet_requests();
    let filter = Filter::Equals {
        field: "category".to_string(),
        value: FieldValue::Text("Pager".to_string()),
    };
    let mut concatenated = Vec::new();

    for (page_idx, expected_ids) in super::parity_fixtures::PAGINATION_EXPECTED_PAGES
        .iter()
        .enumerate()
    {
        let result = fixture
            .executor("")
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
}

#[test]
fn exact_nb_hits_family_keeps_total_exact_above_requested_limit() {
    let fixture = build_parity_fixture();
    let sort = object_id_sort();
    let spec = super::parity_fixtures::QuerySpec {
        name: "exactprobe",
        query: "exactprobe",
        query_type: "prefixNone",
        expected_ids: &[],
        expected_total: 13,
    };
    let result = fixture
        .executor(spec.query)
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
    assert_eq!(result.total, spec.expected_total);
    assert!(result.total > super::parity_fixtures::EXACT_NB_HITS_LIMIT);
}

#[test]
fn geo_family_filters_executor_results_with_precise_distances() {
    let fixture = build_parity_fixture();
    let sort = object_id_sort();
    let spec = &super::parity_fixtures::GEO_QUERY;
    let result = fixture
        .executor(spec.query)
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
}

#[test]
fn highlight_family_asserts_exact_em_strings_and_match_levels() {
    let fixture = build_parity_fixture();
    let sort = object_id_sort();
    let spec = super::parity_fixtures::QuerySpec {
        name: "highlightprobe target",
        query: "highlightprobe target",
        query_type: "prefixNone",
        expected_ids: &[super::parity_fixtures::HIGHLIGHT_EXPECTATION.document_id],
        expected_total: 1,
    };
    let result = fixture
        .executor(spec.query)
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
}

#[test]
fn custom_ranking_family_uses_doc_id_tiebreak_for_equal_keys() {
    let fixture = build_parity_fixture();
    let spec = &super::parity_fixtures::CUSTOM_RANKING_QUERY;
    let result = fixture
        .custom_ranking_executor(spec.query)
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
}

fn laptop_filter() -> Filter {
    Filter::And(vec![
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
    ])
}

fn geoloc(doc: &crate::types::ScoredDocument) -> Option<(f64, f64)> {
    let FieldValue::Object(point) = doc.document.fields.get("_geoloc")? else {
        return None;
    };
    let lat = point.get("lat")?.as_float()?;
    let lng = point.get("lng")?.as_float()?;
    Some((lat, lng))
}
