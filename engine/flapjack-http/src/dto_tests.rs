use super::*;

// ── effective_hits_per_page ──

#[test]
fn effective_hits_per_page_default() {
    let req = SearchRequest::default();
    assert_eq!(req.effective_hits_per_page(), 20);
}

#[test]
fn effective_hits_per_page_custom() {
    let req = SearchRequest {
        hits_per_page: Some(50),
        ..Default::default()
    };
    assert_eq!(req.effective_hits_per_page(), 50);
}

#[test]
fn search_request_deserializes_hits_per_page_number() {
    let req: SearchRequest = serde_json::from_str(r#"{"hitsPerPage":25}"#).unwrap();
    assert_eq!(req.hits_per_page, Some(25));
}

#[test]
fn search_request_deserializes_hits_per_page_null_as_none() {
    let req: SearchRequest = serde_json::from_str(r#"{"hitsPerPage":null}"#).unwrap();
    assert_eq!(req.hits_per_page, None);
}

#[test]
fn search_request_rejects_non_numeric_hits_per_page() {
    let result = serde_json::from_str::<SearchRequest>(r#"{"hitsPerPage":"abc"}"#);
    assert!(result.is_err());
}

#[test]
fn search_facet_values_request_deserializes_sort_facet_values_by() {
    let json = r#"{"facetQuery":"ni","sortFacetValuesBy":"alpha"}"#;
    let req: SearchFacetValuesRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.facet_query, "ni");
    assert_eq!(req.sort_facet_values_by, Some("alpha".to_string()));
}

/// Verify that `SearchFacetValuesRequest::validate` accepts `"count"`, `"alpha"`, and `None`, but rejects invalid values.
#[test]
fn search_facet_values_request_validates_sort_facet_values_by() {
    // Valid values should pass
    let mut req = SearchFacetValuesRequest {
        facet_query: String::new(),
        filters: None,
        max_facet_hits: 10,
        sort_facet_values_by: Some("count".to_string()),
    };
    assert!(req.validate().is_ok());

    req.sort_facet_values_by = Some("alpha".to_string());
    assert!(req.validate().is_ok());

    req.sort_facet_values_by = None;
    assert!(req.validate().is_ok());

    // Invalid value should return error
    req.sort_facet_values_by = Some("foobar".to_string());
    let err = req.validate().unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("sortFacetValuesBy"),
        "error should mention the field: {}",
        msg
    );
}

// ── apply_params_string ──

#[test]
fn apply_params_string_sets_query() {
    let mut req = SearchRequest {
        params: Some("query=hello".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.query, "hello");
}

#[test]
fn apply_params_string_overrides_existing_query() {
    // Verified against live Algolia API (2026-02-23): params string takes priority
    let mut req = SearchRequest {
        query: "existing".to_string(),
        params: Some("query=new".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.query, "new",
        "params string must override top-level JSON query"
    );
}

#[test]
fn apply_params_string_sets_hits_per_page() {
    let mut req = SearchRequest {
        params: Some("hitsPerPage=5".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.hits_per_page, Some(5));
}

#[test]
fn apply_params_string_sets_page() {
    let mut req = SearchRequest {
        params: Some("page=3".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.page, 3);
}

#[test]
fn apply_params_string_sets_filters() {
    let mut req = SearchRequest {
        params: Some("filters=brand%3ANike".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.filters, Some("brand:Nike".to_string()));
}

#[test]
fn apply_params_string_empty_noop() {
    let mut req = SearchRequest {
        params: Some("".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert!(req.query.is_empty());
}

#[test]
fn apply_params_string_none_noop() {
    let mut req = SearchRequest::default();
    req.apply_params_string();
    assert!(req.query.is_empty());
}

#[test]
fn apply_params_string_multiple_fields() {
    let mut req = SearchRequest {
        params: Some("query=test&hitsPerPage=10&page=2&analytics=true".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.query, "test");
    assert_eq!(req.hits_per_page, Some(10));
    assert_eq!(req.page, 2);
    assert_eq!(req.analytics, Some(true));
}

#[test]
fn apply_params_string_sets_session_id() {
    let mut req = SearchRequest {
        params: Some("sessionID=sid-123".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.session_id.as_deref(), Some("sid-123"));
}

#[test]
fn apply_params_string_sets_enable_personalization() {
    let mut req = SearchRequest {
        params: Some("enablePersonalization=true".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.enable_personalization, Some(true));
}

#[test]
fn apply_params_string_sets_enable_re_ranking() {
    let mut req = SearchRequest {
        params: Some("enableReRanking=true".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.enable_re_ranking, Some(true));
}

#[test]
fn apply_params_string_sets_enable_re_ranking_false() {
    let mut req = SearchRequest {
        params: Some("enableReRanking=false".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.enable_re_ranking, Some(false));
}

#[test]
fn apply_params_string_sets_re_ranking_apply_filter() {
    let mut req = SearchRequest {
        params: Some("reRankingApplyFilter=brand%3ANike".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.re_ranking_apply_filter, Some("brand:Nike".to_string()));
}

#[test]
fn apply_params_string_sets_natural_languages() {
    let mut req = SearchRequest {
        params: Some("naturalLanguages=[\"fr\"]".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.natural_languages, Some(vec!["fr".to_string()]));
}

#[test]
fn natural_languages_json_deserialization() {
    let json = r#"{"naturalLanguages":["fr","de"]}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.natural_languages,
        Some(vec!["fr".to_string(), "de".to_string()])
    );
}

#[test]
fn apply_params_string_sets_personalization_impact() {
    let mut req = SearchRequest {
        params: Some("personalizationImpact=70".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.personalization_impact, Some(70));
}

#[test]
fn apply_params_string_sets_personalization_filters() {
    let mut req = SearchRequest {
        params: Some(
            "personalizationFilters=%5B%22brand%3ANike%22%2C%22category%3AShoes%22%5D".to_string(),
        ),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.personalization_filters,
        Some(vec!["brand:Nike".to_string(), "category:Shoes".to_string(),])
    );
}

// ── apply_params_string: params string OVERRIDES top-level JSON ──
// Verified against live Algolia API (2026-02-23): params string always wins.

#[test]
fn apply_params_string_overrides_existing_analytics() {
    let mut req = SearchRequest {
        analytics: Some(false),
        params: Some("analytics=true".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.analytics,
        Some(true),
        "params string must override top-level analytics"
    );
}

#[test]
fn apply_params_string_overrides_existing_click_analytics() {
    let mut req = SearchRequest {
        click_analytics: Some(true),
        params: Some("clickAnalytics=false".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.click_analytics,
        Some(false),
        "params string must override top-level clickAnalytics"
    );
}

#[test]
fn apply_params_string_overrides_existing_get_ranking_info() {
    let mut req = SearchRequest {
        get_ranking_info: Some(true),
        params: Some("getRankingInfo=false".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.get_ranking_info,
        Some(false),
        "params string must override top-level getRankingInfo"
    );
}

#[test]
fn apply_params_string_overrides_existing_around_lat_lng_via_ip() {
    let mut req = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        params: Some("aroundLatLngViaIP=false".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.around_lat_lng_via_ip,
        Some(false),
        "params string must override top-level aroundLatLngViaIP"
    );
}

#[test]
fn apply_params_string_overrides_existing_decompound_query() {
    let mut req = SearchRequest {
        decompound_query: Some(true),
        params: Some("decompoundQuery=false".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.decompound_query,
        Some(false),
        "params string must override top-level decompoundQuery"
    );
}

#[test]
fn apply_params_string_overrides_existing_page() {
    let mut req = SearchRequest {
        page: 5,
        params: Some("page=2".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.page, 2, "params string must override top-level page");
}

#[test]
fn apply_params_string_overrides_existing_hits_per_page() {
    let mut req = SearchRequest {
        hits_per_page: Some(20),
        params: Some("hitsPerPage=5".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.hits_per_page,
        Some(5),
        "params string must override top-level hitsPerPage"
    );
}

#[test]
fn apply_params_string_overrides_existing_filters() {
    let mut req = SearchRequest {
        filters: Some("brand:Nike".to_string()),
        params: Some("filters=brand%3AAdidas".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.filters,
        Some("brand:Adidas".to_string()),
        "params string must override top-level filters"
    );
}

// ── parse_facet_filter_string ──

#[test]
fn parse_facet_filter_basic() {
    let f = parse_facet_filter_string("brand:Nike").unwrap();
    match f {
        flapjack::types::Filter::Equals { field, value } => {
            assert_eq!(field, "brand");
            assert_eq!(value, flapjack::types::FieldValue::Text("Nike".to_string()));
        }
        _ => panic!("expected Equals"),
    }
}

#[test]
fn parse_facet_filter_negated() {
    let f = parse_facet_filter_string("-brand:Nike").unwrap();
    match f {
        flapjack::types::Filter::Not(inner) => match *inner {
            flapjack::types::Filter::Equals { field, value } => {
                assert_eq!(field, "brand");
                assert_eq!(value, flapjack::types::FieldValue::Text("Nike".to_string()));
            }
            _ => panic!("expected Equals inside Not"),
        },
        _ => panic!("expected Not"),
    }
}

#[test]
fn parse_facet_filter_quoted_value() {
    let f = parse_facet_filter_string("brand:\"Air Max\"").unwrap();
    match f {
        flapjack::types::Filter::Equals { value, .. } => {
            assert_eq!(
                value,
                flapjack::types::FieldValue::Text("Air Max".to_string())
            );
        }
        _ => panic!("expected Equals"),
    }
}

#[test]
fn parse_facet_filter_no_colon() {
    assert!(parse_facet_filter_string("nocolon").is_none());
}

// ── parse_numeric_filter_string ──

#[test]
fn parse_numeric_equals() {
    let f = parse_numeric_filter_string("price=100").unwrap();
    match f {
        flapjack::types::Filter::Equals { field, value } => {
            assert_eq!(field, "price");
            assert_eq!(value, flapjack::types::FieldValue::Integer(100));
        }
        _ => panic!("expected Equals"),
    }
}

#[test]
fn parse_numeric_gte() {
    let f = parse_numeric_filter_string("price>=50").unwrap();
    match f {
        flapjack::types::Filter::GreaterThanOrEqual { field, value } => {
            assert_eq!(field, "price");
            assert_eq!(value, flapjack::types::FieldValue::Integer(50));
        }
        _ => panic!("expected GreaterThanOrEqual"),
    }
}

#[test]
fn parse_numeric_lt() {
    let f = parse_numeric_filter_string("price<200").unwrap();
    match f {
        flapjack::types::Filter::LessThan { field, value } => {
            assert_eq!(field, "price");
            assert_eq!(value, flapjack::types::FieldValue::Integer(200));
        }
        _ => panic!("expected LessThan"),
    }
}

#[test]
fn parse_numeric_float() {
    let f = parse_numeric_filter_string("rating>=4.5").unwrap();
    match f {
        flapjack::types::Filter::GreaterThanOrEqual { field, value } => {
            assert_eq!(field, "rating");
            assert_eq!(value, flapjack::types::FieldValue::Float(4.5));
        }
        _ => panic!("expected GreaterThanOrEqual"),
    }
}

#[test]
fn parse_numeric_not_equals() {
    let f = parse_numeric_filter_string("status!=0").unwrap();
    match f {
        flapjack::types::Filter::NotEquals { field, value } => {
            assert_eq!(field, "status");
            assert_eq!(value, flapjack::types::FieldValue::Integer(0));
        }
        _ => panic!("expected NotEquals"),
    }
}

#[test]
fn parse_numeric_invalid_value() {
    assert!(parse_numeric_filter_string("price=abc").is_none());
}

// ── facet_filters_to_ast ──

#[test]
fn facet_filters_single_string() {
    let v = serde_json::json!("brand:Nike");
    let f = facet_filters_to_ast(&v).unwrap();
    match f {
        flapjack::types::Filter::Equals { field, value } => {
            assert_eq!(field, "brand");
            assert_eq!(value, flapjack::types::FieldValue::Text("Nike".to_string()));
        }
        _ => panic!("expected Equals"),
    }
}

/// Verify that a flat JSON array of facet filter strings produces an AND filter combining both conditions.
#[test]
fn facet_filters_array_and() {
    let v = serde_json::json!(["brand:Nike", "color:Red"]);
    let f = facet_filters_to_ast(&v).unwrap();
    match f {
        flapjack::types::Filter::And(parts) => {
            assert_eq!(parts.len(), 2);
            // Verify both filters parsed correctly
            match &parts[0] {
                flapjack::types::Filter::Equals { field, value } => {
                    assert_eq!(field, "brand");
                    assert_eq!(
                        *value,
                        flapjack::types::FieldValue::Text("Nike".to_string())
                    );
                }
                _ => panic!("expected Equals for first filter"),
            }
        }
        _ => panic!("expected And"),
    }
}

/// Verify that a nested JSON array produces an AND of an OR group and a plain equality filter.
#[test]
fn facet_filters_nested_or() {
    use flapjack::types::{FieldValue, Filter};
    let v = serde_json::json!([["brand:Nike", "brand:Adidas"], "color:Red"]);
    let f = facet_filters_to_ast(&v).unwrap();
    match f {
        Filter::And(parts) => {
            assert_eq!(parts.len(), 2);
            match &parts[0] {
                Filter::Or(or_parts) => {
                    assert_eq!(or_parts.len(), 2);
                    match &or_parts[0] {
                        Filter::Equals { field, value } => {
                            assert_eq!(field, "brand");
                            assert_eq!(*value, FieldValue::Text("Nike".to_string()));
                        }
                        _ => panic!("expected Equals for or_parts[0]"),
                    }
                    match &or_parts[1] {
                        Filter::Equals { field, value } => {
                            assert_eq!(field, "brand");
                            assert_eq!(*value, FieldValue::Text("Adidas".to_string()));
                        }
                        _ => panic!("expected Equals for or_parts[1]"),
                    }
                }
                _ => panic!("expected Or"),
            }
            match &parts[1] {
                Filter::Equals { field, value } => {
                    assert_eq!(field, "color");
                    assert_eq!(*value, FieldValue::Text("Red".to_string()));
                }
                _ => panic!("expected Equals for parts[1]"),
            }
        }
        _ => panic!("expected And"),
    }
}

#[test]
fn facet_filters_empty_array() {
    let v = serde_json::json!([]);
    assert!(facet_filters_to_ast(&v).is_none());
}

// ── numeric_filters_to_ast ──

#[test]
fn numeric_filters_single_string() {
    let v = serde_json::json!("price>=10");
    let f = numeric_filters_to_ast(&v).unwrap();
    match f {
        flapjack::types::Filter::GreaterThanOrEqual { field, value } => {
            assert_eq!(field, "price");
            assert_eq!(value, flapjack::types::FieldValue::Integer(10));
        }
        _ => panic!("expected GreaterThanOrEqual"),
    }
}

/// Verify that a flat JSON array of numeric filter strings produces an AND filter with both conditions.
#[test]
fn numeric_filters_array_and() {
    let v = serde_json::json!(["price>=10", "price<=100"]);
    let f = numeric_filters_to_ast(&v).unwrap();
    match f {
        flapjack::types::Filter::And(parts) => {
            assert_eq!(parts.len(), 2);
            match &parts[0] {
                flapjack::types::Filter::GreaterThanOrEqual { field, value } => {
                    assert_eq!(field, "price");
                    assert_eq!(*value, flapjack::types::FieldValue::Integer(10));
                }
                _ => panic!("expected GreaterThanOrEqual"),
            }
            match &parts[1] {
                flapjack::types::Filter::LessThanOrEqual { field, value } => {
                    assert_eq!(field, "price");
                    assert_eq!(*value, flapjack::types::FieldValue::Integer(100));
                }
                _ => panic!("expected LessThanOrEqual"),
            }
        }
        _ => panic!("expected And"),
    }
}

// ── tag_filters_to_ast ──

#[test]
fn tag_filters_single_string() {
    let v = serde_json::json!("electronics");
    let f = tag_filters_to_ast(&v).unwrap();
    match f {
        flapjack::types::Filter::Equals { field, value } => {
            assert_eq!(field, "_tags");
            assert_eq!(
                value,
                flapjack::types::FieldValue::Text("electronics".to_string())
            );
        }
        _ => panic!("expected Equals"),
    }
}

#[test]
fn tag_filters_array_and() {
    let v = serde_json::json!(["electronics", "sale"]);
    let f = tag_filters_to_ast(&v).unwrap();
    match f {
        flapjack::types::Filter::And(parts) => assert_eq!(parts.len(), 2),
        _ => panic!("expected And"),
    }
}

#[test]
fn tag_filters_nested_or() {
    let v = serde_json::json!([["electronics", "books"], "sale"]);
    let f = tag_filters_to_ast(&v).unwrap();
    match f {
        flapjack::types::Filter::And(parts) => {
            assert_eq!(parts.len(), 2);
            match &parts[0] {
                flapjack::types::Filter::Or(or_parts) => assert_eq!(or_parts.len(), 2),
                _ => panic!("expected Or"),
            }
        }
        _ => panic!("expected And"),
    }
}

// ── parse_optional_filters ──

#[test]
fn optional_filters_single_string() {
    let v = serde_json::json!("category:Book");
    let specs = parse_optional_filters(&v);
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].0, "category");
    assert_eq!(specs[0].1, "Book");
    assert_eq!(specs[0].2, 1.0);
}

#[test]
fn optional_filters_flat_array() {
    let v = serde_json::json!(["category:Book", "author:John"]);
    let specs = parse_optional_filters(&v);
    assert_eq!(
        specs,
        vec![
            ("category".to_string(), "Book".to_string(), 1.0),
            ("author".to_string(), "John".to_string(), 1.0)
        ]
    );
}

#[test]
fn optional_filters_nested_or() {
    let v = serde_json::json!([["category:Book", "category:Movie"], "author:John"]);
    let specs = parse_optional_filters(&v);
    assert_eq!(
        specs,
        vec![
            ("category".to_string(), "Book".to_string(), 1.0),
            ("category".to_string(), "Movie".to_string(), 1.0),
            ("author".to_string(), "John".to_string(), 1.0)
        ]
    );
}

#[test]
fn optional_filters_empty_value() {
    let v = serde_json::json!(null);
    let specs = parse_optional_filters(&v);
    assert!(specs.is_empty());
}

#[test]
fn http_filter_helpers_match_the_core_owner_for_every_algolia_input_form() {
    let facet_cases = [
        serde_json::json!("brand:Nike"),
        serde_json::json!("-brand:'Air Max'"),
        serde_json::json!(["brand:Nike", "color:Red"]),
        serde_json::json!([["brand:Nike", "brand:Adidas"], "color:Red"]),
        serde_json::json!([42, [false], "brand:Nike"]),
        serde_json::json!(null),
    ];
    for input in facet_cases {
        assert_eq!(
            facet_filters_to_ast(&input),
            flapjack::query::algolia_filters::facet_filters_to_ast(&input),
            "facet filter drift for {input}"
        );
    }

    let numeric_cases = [
        serde_json::json!("price>=10"),
        serde_json::json!("price<=10.5"),
        serde_json::json!(["price>1", "stock!=0"]),
        serde_json::json!([["price=10", "price<0"], "rating>=-1.5"]),
        serde_json::json!(["price=invalid", 42]),
        serde_json::json!(null),
    ];
    for input in numeric_cases {
        assert_eq!(
            numeric_filters_to_ast(&input),
            flapjack::query::algolia_filters::numeric_filters_to_ast(&input),
            "numeric filter drift for {input}"
        );
    }

    let tag_cases = [
        serde_json::json!("featured"),
        serde_json::json!(["featured", "sale"]),
        serde_json::json!([["featured", "sale"], "clearance"]),
        serde_json::json!([false, [42], "featured"]),
        serde_json::json!(null),
    ];
    for input in tag_cases {
        assert_eq!(
            tag_filters_to_ast(&input),
            flapjack::query::algolia_filters::tag_filters_to_ast(&input),
            "tag filter drift for {input}"
        );
    }
}

// ── deserialize_string_or_vec ──

#[test]
fn search_request_facets_string() {
    let json = r#"{"facets": "brand"}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.facets, Some(vec!["brand".to_string()]));
}

#[test]
fn search_request_facets_array() {
    let json = r#"{"facets": ["brand", "category"]}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.facets,
        Some(vec!["brand".to_string(), "category".to_string()])
    );
}

#[test]
fn search_request_facets_null() {
    let json = r#"{"facets": null}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert!(req.facets.is_none());
}

#[test]
fn search_request_facets_missing() {
    let json = r#"{}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert!(req.facets.is_none());
}

// ── build_combined_filter ──

#[test]
fn build_combined_filter_none_when_empty() {
    let req = SearchRequest::default();
    assert!(req.build_combined_filter().unwrap().is_none());
}

#[test]
fn build_combined_filter_filters_only() {
    let req = SearchRequest {
        filters: Some("brand:Nike".to_string()),
        ..Default::default()
    };
    let f = req.build_combined_filter().unwrap().unwrap();
    // Should be a single filter, not wrapped in And
    match f {
        flapjack::types::Filter::Equals { field, .. } => assert_eq!(field, "brand"),
        _ => panic!("expected Equals from filter string, got {:?}", f),
    }
}

#[test]
fn build_combined_filter_facet_filters_only() {
    let req = SearchRequest {
        facet_filters: Some(serde_json::json!("color:Red")),
        ..Default::default()
    };
    let f = req.build_combined_filter().unwrap().unwrap();
    match f {
        flapjack::types::Filter::Equals { field, .. } => assert_eq!(field, "color"),
        _ => panic!("expected Equals from facet filter"),
    }
}

#[test]
fn build_combined_filter_combines_multiple_with_and() {
    let req = SearchRequest {
        filters: Some("brand:Nike".to_string()),
        facet_filters: Some(serde_json::json!("color:Red")),
        ..Default::default()
    };
    let f = req.build_combined_filter().unwrap().unwrap();
    match f {
        flapjack::types::Filter::And(parts) => assert_eq!(parts.len(), 2),
        _ => panic!("expected And when combining filters + facet_filters"),
    }
}

#[test]
fn build_combined_filter_all_three_types() {
    let req = SearchRequest {
        filters: Some("brand:Nike".to_string()),
        facet_filters: Some(serde_json::json!("color:Red")),
        numeric_filters: Some(serde_json::json!("price>=10")),
        ..Default::default()
    };
    let f = req.build_combined_filter().unwrap().unwrap();
    match f {
        flapjack::types::Filter::And(parts) => assert_eq!(parts.len(), 3),
        _ => panic!("expected And with 3 parts"),
    }
}

#[test]
fn build_combined_filter_with_tag_filters() {
    let req = SearchRequest {
        tag_filters: Some(serde_json::json!("electronics")),
        ..Default::default()
    };
    let f = req.build_combined_filter().unwrap().unwrap();
    match f {
        flapjack::types::Filter::Equals { field, .. } => assert_eq!(field, "_tags"),
        _ => panic!("expected Equals from tag filter"),
    }
}

#[test]
fn build_combined_filter_invalid_filter_string_returns_error() {
    let req = SearchRequest {
        filters: Some(":::invalid:::".to_string()),
        facet_filters: Some(serde_json::json!("color:Red")),
        ..Default::default()
    };
    let err = req
        .build_combined_filter()
        .expect_err("invalid filter must be rejected");
    assert!(err.to_string().contains("Filter parse error"));
}

// ── parse_numeric_filter_string edge cases ──

#[test]
fn parse_numeric_negative_value() {
    let f = parse_numeric_filter_string("temp=-10").unwrap();
    match f {
        flapjack::types::Filter::Equals { field, value } => {
            assert_eq!(field, "temp");
            assert_eq!(value, flapjack::types::FieldValue::Integer(-10));
        }
        _ => panic!("expected Equals"),
    }
}

#[test]
fn parse_numeric_negative_float() {
    let f = parse_numeric_filter_string("rating>=-1.5").unwrap();
    match f {
        flapjack::types::Filter::GreaterThanOrEqual { field, value } => {
            assert_eq!(field, "rating");
            assert_eq!(value, flapjack::types::FieldValue::Float(-1.5));
        }
        _ => panic!("expected GreaterThanOrEqual"),
    }
}

#[test]
fn parse_numeric_no_operator() {
    assert!(parse_numeric_filter_string("justanumber").is_none());
}

#[test]
fn parse_numeric_gt() {
    let f = parse_numeric_filter_string("count>5").unwrap();
    match f {
        flapjack::types::Filter::GreaterThan { field, value } => {
            assert_eq!(field, "count");
            assert_eq!(value, flapjack::types::FieldValue::Integer(5));
        }
        _ => panic!("expected GreaterThan"),
    }
}

#[test]
fn parse_numeric_lte() {
    let f = parse_numeric_filter_string("count<=99").unwrap();
    match f {
        flapjack::types::Filter::LessThanOrEqual { field, value } => {
            assert_eq!(field, "count");
            assert_eq!(value, flapjack::types::FieldValue::Integer(99));
        }
        _ => panic!("expected LessThanOrEqual"),
    }
}

// ── malformed input edge cases ──

#[test]
fn parse_facet_filter_empty_string() {
    assert!(parse_facet_filter_string("").is_none());
}

#[test]
fn parse_facet_filter_multiple_colons() {
    // "a:b:c" — should take first colon, value is "b:c"
    let f = parse_facet_filter_string("a:b:c").unwrap();
    match f {
        flapjack::types::Filter::Equals { field, value } => {
            assert_eq!(field, "a");
            assert_eq!(value, flapjack::types::FieldValue::Text("b:c".to_string()));
        }
        _ => panic!("expected Equals"),
    }
}

#[test]
fn facet_filters_non_string_in_array_skipped() {
    // Arrays with non-string values should be silently skipped
    let v = serde_json::json!([123, "brand:Nike"]);
    let f = facet_filters_to_ast(&v);
    // Should still produce a result (the valid string filter)
    assert!(f.is_some());
}

#[test]
fn numeric_filters_empty_array() {
    let v = serde_json::json!([]);
    assert!(numeric_filters_to_ast(&v).is_none());
}

#[test]
fn tag_filters_empty_array() {
    let v = serde_json::json!([]);
    assert!(tag_filters_to_ast(&v).is_none());
}

#[test]
fn search_request_facets_empty_array() {
    let json = r#"{"facets": []}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.facets, Some(vec![]));
}

// ── HybridSearchParams tests (6.1) ──

#[test]
fn test_search_request_hybrid_from_json() {
    let json = r#"{"query": "test", "hybrid": {"semanticRatio": 0.8, "embedder": "mymodel"}}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    let hybrid = req.hybrid.unwrap();
    assert!((hybrid.semantic_ratio - 0.8).abs() < f64::EPSILON);
    assert_eq!(hybrid.embedder, "mymodel");
}

#[test]
fn test_search_request_hybrid_defaults() {
    let json = r#"{"query": "test", "hybrid": {}}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    let hybrid = req.hybrid.unwrap();
    assert!((hybrid.semantic_ratio - 0.5).abs() < f64::EPSILON);
    assert_eq!(hybrid.embedder, "default");
}

#[test]
fn test_search_request_hybrid_none_by_default() {
    let json = r#"{"query": "test"}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert!(req.hybrid.is_none());
}

#[test]
fn test_search_request_hybrid_from_params_string() {
    let mut req: SearchRequest =
        serde_json::from_str(r#"{"params": "query=test&hybrid=%7B%22semanticRatio%22%3A0.7%7D"}"#)
            .unwrap();
    req.apply_params_string();
    let hybrid = req.hybrid.unwrap();
    assert!((hybrid.semantic_ratio - 0.7).abs() < f64::EPSILON);
}

#[test]
fn test_search_request_hybrid_semantic_ratio_clamped() {
    // > 1.0 clamped to 1.0
    let json = r#"{"query": "test", "hybrid": {"semanticRatio": 1.5}}"#;
    let mut req: SearchRequest = serde_json::from_str(json).unwrap();
    req.clamp_hybrid_ratio();
    let hybrid = req.hybrid.unwrap();
    assert!((hybrid.semantic_ratio - 1.0).abs() < f64::EPSILON);

    // < 0.0 clamped to 0.0
    let json = r#"{"query": "test", "hybrid": {"semanticRatio": -0.5}}"#;
    let mut req: SearchRequest = serde_json::from_str(json).unwrap();
    req.clamp_hybrid_ratio();
    let hybrid = req.hybrid.unwrap();
    assert!(hybrid.semantic_ratio.abs() < f64::EPSILON);
}

// ── SearchRequest mode tests (5.12) ──

#[test]
fn test_search_request_mode_from_json() {
    use flapjack::index::settings::IndexMode;
    let json = r#"{"query": "test", "mode": "neuralSearch"}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.mode, Some(IndexMode::NeuralSearch));
}

#[test]
fn test_search_request_mode_default_none() {
    let json = r#"{"query": "test"}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert!(req.mode.is_none());
}

#[test]
fn test_search_request_mode_from_params_string() {
    use flapjack::index::settings::IndexMode;
    let mut req: SearchRequest =
        serde_json::from_str(r#"{"params": "query=test&mode=neuralSearch"}"#).unwrap();
    req.apply_params_string();
    assert_eq!(req.mode, Some(IndexMode::NeuralSearch));
}

#[test]
fn test_search_request_mode_keyword_from_params() {
    use flapjack::index::settings::IndexMode;
    let mut req: SearchRequest =
        serde_json::from_str(r#"{"params": "mode=keywordSearch"}"#).unwrap();
    req.apply_params_string();
    assert_eq!(req.mode, Some(IndexMode::KeywordSearch));
}

#[test]
fn test_search_request_decompound_query_default_true() {
    let req: SearchRequest = serde_json::from_str(r#"{"query": "test"}"#).unwrap();
    assert_eq!(req.decompound_query, Some(true));
}

// ── validate() — Algolia-compatible request limits ──

#[test]
fn validate_query_at_limit_ok() {
    let req = SearchRequest {
        query: "a".repeat(MAX_QUERY_BYTES),
        ..Default::default()
    };
    assert!(req.validate().is_ok());
}

#[test]
fn validate_query_over_limit_rejected() {
    let req = SearchRequest {
        query: "a".repeat(MAX_QUERY_BYTES + 1),
        ..Default::default()
    };
    let err = req.validate().unwrap_err();
    assert!(err.to_string().contains("Query exceeds maximum length"));
}

#[test]
fn validate_empty_query_ok() {
    let req = SearchRequest::default();
    assert!(req.validate().is_ok());
}

#[test]
fn validate_hits_per_page_at_limit_ok() {
    let req = SearchRequest {
        hits_per_page: Some(MAX_HITS_PER_PAGE),
        ..Default::default()
    };
    assert!(req.validate().is_ok());
}

#[test]
fn validate_hits_per_page_over_limit_rejected() {
    let req = SearchRequest {
        hits_per_page: Some(MAX_HITS_PER_PAGE + 1),
        ..Default::default()
    };
    let err = req.validate().unwrap_err();
    assert!(err.to_string().contains("hitsPerPage exceeds maximum"));
}

#[test]
fn validate_pagination_depth_over_limit_rejected() {
    let req = SearchRequest {
        page: 21,
        hits_per_page: Some(1000),
        ..Default::default()
    };
    let err = req.validate().unwrap_err();
    assert!(err.to_string().contains("Pagination offset"));
}

#[test]
fn validate_pagination_depth_at_limit_ok() {
    let req = SearchRequest {
        page: 20,
        hits_per_page: Some(1000),
        ..Default::default()
    };
    assert!(req.validate().is_ok());
}

#[test]
fn validate_filter_string_over_limit_rejected() {
    let req = SearchRequest {
        filters: Some("x".repeat(MAX_FILTER_BYTES + 1)),
        ..Default::default()
    };
    let err = req.validate().unwrap_err();
    assert!(err.to_string().contains("Filter string exceeds maximum"));
}

#[test]
fn validate_filter_string_at_limit_ok() {
    let req = SearchRequest {
        filters: Some("x".repeat(MAX_FILTER_BYTES)),
        ..Default::default()
    };
    assert!(req.validate().is_ok());
}

#[test]
fn validate_personalization_impact_at_limit_ok() {
    let req = SearchRequest {
        personalization_impact: Some(100),
        ..Default::default()
    };
    assert!(req.validate().is_ok());
}

#[test]
fn validate_personalization_impact_over_limit_rejected() {
    let req = SearchRequest {
        personalization_impact: Some(101),
        ..Default::default()
    };
    let err = req.validate().unwrap_err();
    assert!(err
        .to_string()
        .contains("personalizationImpact must be between 0 and 100"));
}

#[test]
fn search_request_deserializes_federation_options_default_weight() {
    let req: SearchRequest =
        serde_json::from_str(r#"{"query":"shoe","federationOptions":{}}"#).unwrap();
    let options = req
        .federation_options
        .expect("federationOptions should deserialize");
    assert!((options.weight - 1.0).abs() < f64::EPSILON);
}

#[test]
fn search_request_deserializes_federation_options_custom_weight() {
    let req: SearchRequest =
        serde_json::from_str(r#"{"query":"shoe","federationOptions":{"weight":2.5}}"#).unwrap();
    let options = req
        .federation_options
        .expect("federationOptions should deserialize");
    assert!((options.weight - 2.5).abs() < f64::EPSILON);
}

#[test]
fn search_request_without_federation_options_defaults_to_none() {
    let req: SearchRequest = serde_json::from_str(r#"{"query":"shoe"}"#).unwrap();
    assert!(
        req.federation_options.is_none(),
        "federationOptions should default to None when omitted"
    );
}

#[test]
fn validate_federation_weight_must_be_finite_and_positive() {
    let invalid_weights = [0.0, -1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN];

    for weight in invalid_weights {
        let request = SearchRequest {
            federation_options: Some(FederationOptions { weight }),
            ..Default::default()
        };
        let error = request.validate().expect_err("weight {weight} should fail");
        assert!(
            error.to_string().contains("federationOptions.weight"),
            "invalid federation weight should reference federationOptions.weight, got: {error}"
        );
    }
}

// ── Stage 4: JSON deserialization of new fields ──

#[test]
fn stage4_json_advanced_syntax_features() {
    let json = r#"{"query":"test","advancedSyntaxFeatures":["exactPhrase","excludeWords"]}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.advanced_syntax_features,
        Some(vec!["exactPhrase".to_string(), "excludeWords".to_string()])
    );
}

#[test]
fn stage4_json_sort_facet_values_by() {
    let json = r#"{"query":"test","sortFacetValuesBy":"alpha"}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.sort_facet_values_by, Some("alpha".to_string()));
}

#[test]
fn stage4_json_faceting_after_distinct() {
    let json = r#"{"query":"test","facetingAfterDistinct":true}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.faceting_after_distinct, Some(true));
}

#[test]
fn stage4_json_sum_or_filters_scores() {
    let json = r#"{"query":"test","sumOrFiltersScores":true}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.sum_or_filters_scores, Some(true));
}

#[test]
fn stage4_json_snippet_ellipsis_text() {
    let json = r#"{"query":"test","snippetEllipsisText":"..."}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.snippet_ellipsis_text, Some("...".to_string()));
}

#[test]
fn stage4_json_snippet_ellipsis_text_empty() {
    let json = r#"{"query":"test","snippetEllipsisText":""}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.snippet_ellipsis_text, Some("".to_string()));
}

#[test]
fn stage4_json_restrict_highlight_and_snippet_arrays() {
    let json = r#"{"query":"test","restrictHighlightAndSnippetArrays":true}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.restrict_highlight_and_snippet_arrays, Some(true));
}

#[test]
fn stage4_json_min_proximity() {
    let json = r#"{"query":"test","minProximity":3}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.min_proximity, Some(3));
}

#[test]
fn stage4_json_disable_exact_on_attributes() {
    let json = r#"{"query":"test","disableExactOnAttributes":["description","content"]}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.disable_exact_on_attributes,
        Some(vec!["description".to_string(), "content".to_string()])
    );
}

#[test]
fn stage4_json_exact_on_single_word_query() {
    let json = r#"{"query":"test","exactOnSingleWordQuery":"word"}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.exact_on_single_word_query, Some("word".to_string()));
}

#[test]
fn stage4_json_alternatives_as_exact() {
    let json = r#"{"query":"test","alternativesAsExact":["ignorePlurals","multiWordsSynonym"]}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        req.alternatives_as_exact,
        Some(vec![
            "ignorePlurals".to_string(),
            "multiWordsSynonym".to_string()
        ])
    );
}

#[test]
fn stage4_json_replace_synonyms_in_highlight() {
    let json = r#"{"query":"test","replaceSynonymsInHighlight":true}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.replace_synonyms_in_highlight, Some(true));
}

#[test]
fn stage4_json_enable_ab_test() {
    let json = r#"{"query":"test","enableABTest":false}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.enable_ab_test, Some(false));
}

#[test]
fn stage4_json_percentile_computation() {
    let json = r#"{"query":"test","percentileComputation":false}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.percentile_computation, Some(false));
}

/// Verify that all Stage 4 structural search parameters default to `None` when absent from JSON input.
#[test]
fn stage4_json_defaults_none() {
    let json = r#"{"query":"test"}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert!(req.advanced_syntax_features.is_none());
    assert!(req.sort_facet_values_by.is_none());
    assert!(req.faceting_after_distinct.is_none());
    assert!(req.sum_or_filters_scores.is_none());
    assert!(req.snippet_ellipsis_text.is_none());
    assert!(req.restrict_highlight_and_snippet_arrays.is_none());
    assert!(req.min_proximity.is_none());
    assert!(req.disable_exact_on_attributes.is_none());
    assert!(req.exact_on_single_word_query.is_none());
    assert!(req.alternatives_as_exact.is_none());
    assert!(req.replace_synonyms_in_highlight.is_none());
    assert!(req.enable_ab_test.is_none());
    assert!(req.percentile_computation.is_none());
    assert!(req.similar_query.is_none());
}

// ── Stage 4: params string parsing ──

#[test]
fn stage4_params_advanced_syntax_features() {
    let mut req = SearchRequest {
        params: Some(r#"advancedSyntaxFeatures=["exactPhrase"]"#.to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.advanced_syntax_features,
        Some(vec!["exactPhrase".to_string()])
    );
}

#[test]
fn stage4_params_sort_facet_values_by() {
    let mut req = SearchRequest {
        params: Some("sortFacetValuesBy=alpha".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.sort_facet_values_by, Some("alpha".to_string()));
}

#[test]
fn stage4_params_faceting_after_distinct() {
    let mut req = SearchRequest {
        params: Some("facetingAfterDistinct=true".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.faceting_after_distinct, Some(true));
}

#[test]
fn stage4_params_sum_or_filters_scores() {
    let mut req = SearchRequest {
        params: Some("sumOrFiltersScores=true".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.sum_or_filters_scores, Some(true));
}

#[test]
fn stage4_params_snippet_ellipsis_text() {
    let mut req = SearchRequest {
        params: Some("snippetEllipsisText=---".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.snippet_ellipsis_text, Some("---".to_string()));
}

#[test]
fn stage4_params_restrict_highlight_and_snippet_arrays() {
    let mut req = SearchRequest {
        params: Some("restrictHighlightAndSnippetArrays=true".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.restrict_highlight_and_snippet_arrays, Some(true));
}

#[test]
fn stage4_params_min_proximity() {
    let mut req = SearchRequest {
        params: Some("minProximity=5".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.min_proximity, Some(5));
}

#[test]
fn stage4_params_disable_exact_on_attributes() {
    let mut req = SearchRequest {
        params: Some(r#"disableExactOnAttributes=["title","body"]"#.to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.disable_exact_on_attributes,
        Some(vec!["title".to_string(), "body".to_string()])
    );
}

#[test]
fn stage4_params_exact_on_single_word_query() {
    let mut req = SearchRequest {
        params: Some("exactOnSingleWordQuery=none".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.exact_on_single_word_query, Some("none".to_string()));
}

#[test]
fn stage4_params_alternatives_as_exact() {
    let mut req = SearchRequest {
        params: Some(r#"alternativesAsExact=["singleWordSynonym"]"#.to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.alternatives_as_exact,
        Some(vec!["singleWordSynonym".to_string()])
    );
}

#[test]
fn stage4_params_replace_synonyms_in_highlight() {
    let mut req = SearchRequest {
        params: Some("replaceSynonymsInHighlight=true".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.replace_synonyms_in_highlight, Some(true));
}

#[test]
fn stage4_params_enable_ab_test() {
    let mut req = SearchRequest {
        params: Some("enableABTest=false".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.enable_ab_test, Some(false));
}

#[test]
fn stage4_params_percentile_computation() {
    let mut req = SearchRequest {
        params: Some("percentileComputation=false".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.percentile_computation, Some(false));
}

// ── Stage 4: validation ──

#[test]
fn stage4_validate_advanced_syntax_features_valid() {
    let req = SearchRequest {
        advanced_syntax_features: Some(vec!["exactPhrase".to_string(), "excludeWords".to_string()]),
        ..Default::default()
    };
    assert!(req.validate().is_ok());
}

#[test]
fn stage4_validate_advanced_syntax_features_invalid() {
    let req = SearchRequest {
        advanced_syntax_features: Some(vec!["badValue".to_string()]),
        ..Default::default()
    };
    let err = req.validate().unwrap_err();
    assert!(err.to_string().contains("Invalid advancedSyntaxFeatures"));
}

#[test]
fn stage4_validate_sort_facet_values_by_valid() {
    for v in &["count", "alpha"] {
        let req = SearchRequest {
            sort_facet_values_by: Some(v.to_string()),
            ..Default::default()
        };
        assert!(req.validate().is_ok());
    }
}

#[test]
fn stage4_validate_sort_facet_values_by_invalid() {
    let req = SearchRequest {
        sort_facet_values_by: Some("invalid".to_string()),
        ..Default::default()
    };
    let err = req.validate().unwrap_err();
    assert!(err.to_string().contains("Invalid sortFacetValuesBy"));
}

#[test]
fn stage4_validate_exact_on_single_word_query_valid() {
    for v in &["attribute", "none", "word"] {
        let req = SearchRequest {
            exact_on_single_word_query: Some(v.to_string()),
            ..Default::default()
        };
        assert!(req.validate().is_ok());
    }
}

#[test]
fn stage4_validate_exact_on_single_word_query_invalid() {
    let req = SearchRequest {
        exact_on_single_word_query: Some("invalid".to_string()),
        ..Default::default()
    };
    let err = req.validate().unwrap_err();
    assert!(err.to_string().contains("Invalid exactOnSingleWordQuery"));
}

#[test]
fn stage4_validate_alternatives_as_exact_valid() {
    let req = SearchRequest {
        alternatives_as_exact: Some(vec![
            "ignorePlurals".to_string(),
            "singleWordSynonym".to_string(),
            "multiWordsSynonym".to_string(),
        ]),
        ..Default::default()
    };
    assert!(req.validate().is_ok());
}

#[test]
fn stage4_validate_alternatives_as_exact_invalid() {
    let req = SearchRequest {
        alternatives_as_exact: Some(vec!["badValue".to_string()]),
        ..Default::default()
    };
    let err = req.validate().unwrap_err();
    assert!(err.to_string().contains("Invalid alternativesAsExact"));
}

#[test]
fn stage4_validate_min_proximity_valid_range() {
    for v in 1..=7 {
        let req = SearchRequest {
            min_proximity: Some(v),
            ..Default::default()
        };
        assert!(req.validate().is_ok(), "minProximity={} should be valid", v);
    }
}

#[test]
fn stage4_validate_min_proximity_zero_rejected() {
    let req = SearchRequest {
        min_proximity: Some(0),
        ..Default::default()
    };
    let err = req.validate().unwrap_err();
    assert!(err
        .to_string()
        .contains("minProximity must be between 1 and 7"));
}

#[test]
fn stage4_validate_min_proximity_too_large_rejected() {
    let req = SearchRequest {
        min_proximity: Some(100),
        ..Default::default()
    };
    let err = req.validate().unwrap_err();
    assert!(err
        .to_string()
        .contains("minProximity must be between 1 and 7"));
}

// ── Stage 4: IndexSettings field ──

#[test]
fn stage4_index_settings_attribute_criteria_computed_by_min_proximity() {
    let json = r#"{"attributeCriteriaComputedByMinProximity": true}"#;
    let settings: flapjack::index::settings::IndexSettings = serde_json::from_str(json).unwrap();
    assert_eq!(
        settings.attribute_criteria_computed_by_min_proximity,
        Some(true)
    );
}

#[test]
fn stage4_index_settings_attribute_criteria_default_none() {
    let settings = flapjack::index::settings::IndexSettings::default();
    assert!(settings
        .attribute_criteria_computed_by_min_proximity
        .is_none());
}

#[test]
fn stage4_search_request_has_no_attribute_criteria_field() {
    // attributeCriteriaComputedByMinProximity is settings-only, NOT in SearchRequest.
    // Verify unknown input is ignored and not represented when re-serialized.
    let json = r#"{"query":"test","attributeCriteriaComputedByMinProximity":true}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.query, "test");
    let out = format!("{:?}", req);
    assert!(
        !out.contains("attribute_criteria_computed_by_min_proximity"),
        "SearchRequest must not expose settings-only field"
    );
}

// ── Stage 4: IndexSettings settings-level fields ──

#[test]
fn stage4_index_settings_advanced_syntax_features() {
    let json = r#"{"advancedSyntaxFeatures": ["exactPhrase"]}"#;
    let settings: flapjack::index::settings::IndexSettings = serde_json::from_str(json).unwrap();
    assert_eq!(
        settings.advanced_syntax_features,
        Some(vec!["exactPhrase".to_string()])
    );
}

#[test]
fn stage4_index_settings_sort_facet_values_by() {
    let json = r#"{"sortFacetValuesBy": "alpha"}"#;
    let settings: flapjack::index::settings::IndexSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.sort_facet_values_by, Some("alpha".to_string()));
}

#[test]
fn stage4_index_settings_min_proximity() {
    let json = r#"{"minProximity": 3}"#;
    let settings: flapjack::index::settings::IndexSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.min_proximity, Some(3));
}

// ── Stage 4: similarQuery DTO tests ──

#[test]
fn stage4_similar_query_json_parsing() {
    let json = r#"{"query":"test","similarQuery":"red running shoes"}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.similar_query, Some("red running shoes".to_string()));
}

#[test]
fn stage4_similar_query_params_string() {
    let mut req = SearchRequest {
        params: Some("similarQuery=red running shoes".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.similar_query, Some("red running shoes".to_string()));
}

#[test]
fn stage4_similar_query_empty_string() {
    // Empty string should still parse as Some("")
    let json = r#"{"query":"test","similarQuery":""}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.similar_query, Some("".to_string()));
}

#[test]
fn stage4_similar_query_coexists_with_query() {
    // Both fields should be independently accessible
    let json = r#"{"query":"shoes","similarQuery":"red running shoes lightweight"}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.query, "shoes");
    assert_eq!(
        req.similar_query,
        Some("red running shoes lightweight".to_string())
    );
}

// ── Stage 5a: relevancyStrictness DTO tests ──

#[test]
fn relevancy_strictness_json_parsing() {
    let json = r#"{"query":"test","relevancyStrictness":50}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.relevancy_strictness, Some(50));
}

#[test]
fn relevancy_strictness_params_string() {
    let mut req = SearchRequest {
        params: Some("relevancyStrictness=75".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.relevancy_strictness, Some(75));
}

#[test]
fn relevancy_strictness_default_is_none() {
    let json = r#"{"query":"test"}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.relevancy_strictness, None);
}

#[test]
fn relevancy_strictness_coexists_with_other_params() {
    let json = r#"{"query":"shoes","relevancyStrictness":0,"hitsPerPage":20}"#;
    let req: SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.relevancy_strictness, Some(0));
    assert_eq!(req.hits_per_page, Some(20));
    assert_eq!(req.query, "shoes");
}

// ── Stage 5: params parsing characterization ──
// These tests pin behavioral contracts before the apply_params_string decomposition.

/// Comprehensive mixed override: every group of params overrides corresponding JSON fields.
#[test]
fn params_override_all_groups_simultaneously() {
    let mut req = SearchRequest {
        query: "original".to_string(),
        hits_per_page: Some(20),
        page: 0,
        filters: Some("brand:Nike".to_string()),
        analytics: Some(false),
        highlight_pre_tag: Some("<b>".to_string()),
        query_type_prefix: Some("prefixAll".to_string()),
        around_lat_lng: Some("40.0,-74.0".to_string()),
        enable_personalization: Some(false),
        params: Some(
            "query=overridden\
             &hitsPerPage=5\
             &page=3\
             &filters=brand%3AAdidas\
             &analytics=true\
             &highlightPreTag=%3Cem%3E\
             &queryType=prefixLast\
             &aroundLatLng=48.8%2C2.3\
             &enablePersonalization=true"
                .to_string(),
        ),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.query, "overridden");
    assert_eq!(req.hits_per_page, Some(5));
    assert_eq!(req.page, 3);
    assert_eq!(req.filters.as_deref(), Some("brand:Adidas"));
    assert_eq!(req.analytics, Some(true));
    assert_eq!(req.highlight_pre_tag.as_deref(), Some("<em>"));
    assert_eq!(req.query_type_prefix.as_deref(), Some("prefixLast"));
    assert_eq!(req.around_lat_lng.as_deref(), Some("48.8,2.3"));
    assert_eq!(req.enable_personalization, Some(true));
}

/// Invalid numeric/bool values in params must be no-ops (field retains its prior value).
#[test]
fn params_invalid_values_are_noops() {
    let mut req = SearchRequest {
        hits_per_page: Some(20),
        page: 5,
        analytics: Some(true),
        advanced_syntax: Some(true),
        params: Some(
            "hitsPerPage=notanumber\
             &page=abc\
             &analytics=maybe\
             &advancedSyntax=nope"
                .to_string(),
        ),
        ..Default::default()
    };
    req.apply_params_string();
    // hitsPerPage: invalid parse → field unchanged
    assert_eq!(req.hits_per_page, Some(20));
    // page: invalid parse → unwrap_or(0), so becomes 0 (documented behavior)
    assert_eq!(req.page, 0);
    // analytics: invalid bool parse → field unchanged
    assert_eq!(req.analytics, Some(true));
    // advancedSyntax: invalid bool parse → field unchanged
    assert_eq!(req.advanced_syntax, Some(true));
}

/// Calling apply_params_string twice is a no-op on the second call (params consumed via take()).
#[test]
fn params_consumed_only_once() {
    let mut req = SearchRequest {
        query: "first".to_string(),
        params: Some("query=second".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.query, "second");
    assert!(
        req.params.is_none(),
        "params must be consumed after first call"
    );
    // Second call should be a no-op
    req.query = "third".to_string();
    req.apply_params_string();
    assert_eq!(
        req.query, "third",
        "second apply_params_string must not change anything"
    );
}

/// Unknown keys in the params string are silently ignored.
#[test]
fn params_unknown_keys_ignored() {
    let mut req = SearchRequest {
        params: Some("unknownKey=foo&alsoUnknown=bar&query=kept".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(req.query, "kept");
}

/// JSON-encoded array values in params are properly parsed.
#[test]
fn params_json_array_values_parsed() {
    let mut req = SearchRequest {
        params: Some(
            r#"facets=["brand","color"]&attributesToHighlight=["title"]&facetFilters=[["brand:Nike"]]"#
                .to_string(),
        ),
        ..Default::default()
    };
    req.apply_params_string();
    assert_eq!(
        req.facets,
        Some(vec!["brand".to_string(), "color".to_string()])
    );
    assert_eq!(req.attributes_to_highlight, Some(vec!["title".to_string()]));
    assert!(req.facet_filters.is_some());
}

/// Malformed JSON array values in params that support JSON are no-ops or use fallback parsing.
#[test]
fn params_malformed_json_uses_fallback() {
    let mut req = SearchRequest {
        params: Some("facets=brand,color&typoTolerance=true".to_string()),
        ..Default::default()
    };
    req.apply_params_string();
    // facets: JSON parse fails → falls back to comma-split
    assert_eq!(
        req.facets,
        Some(vec!["brand".to_string(), "color".to_string()])
    );
    // typoTolerance: JSON parse fails → falls back to "true"/"false" literal match
    assert_eq!(req.typo_tolerance, Some(serde_json::Value::Bool(true)));
}
