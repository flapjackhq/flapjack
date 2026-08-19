//! Stub summary for mod.rs.
use super::*;
use crate::handlers::personalization::{
    delete_user_profile, get_user_profile, set_personalization_strategy,
};
use crate::handlers::recommend::recommend;
use crate::handlers::settings::set_settings;
use crate::test_helpers::body_json;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    routing::{delete, get, post},
    Router,
};
use flapjack::analytics::schema::InsightEvent;
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig, AnalyticsQueryEngine};
use flapjack::experiments::{
    assignment::{self, AssignmentMethod},
    config::{Experiment, ExperimentArm, ExperimentStatus, PrimaryMetric, QueryOverrides},
};
use flapjack::types::Document;
use flapjack::types::FieldValue;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use tempfile::TempDir;
use tower::ServiceExt;

#[cfg(feature = "vector-search")]
use axum::extract::State;

/// Extract objectID values from a search response body's `hits` array.
fn hit_ids(body: &Value) -> Vec<String> {
    match body["hits"].as_array() {
        Some(hits) => hits
            .iter()
            .filter_map(|hit| hit["objectID"].as_str().map(|id| id.to_string()))
            .collect(),
        None => Vec::new(),
    }
}

fn scored_doc(id: &str, score: f32) -> ScoredDocument {
    ScoredDocument {
        document: Document {
            id: id.to_string(),
            fields: HashMap::new(),
        },
        score,
    }
}

/// Assert that a document with high click counts is promoted ahead of higher-scoring documents.
#[test]
fn rerank_by_ctr_promotes_clicked_doc_with_lower_bm25() {
    let docs = vec![
        scored_doc("adidas-1", 10.0),
        scored_doc("nike-1", 9.0),
        scored_doc("puma-1", 8.0),
    ];
    let click_counts = HashMap::from([
        ("adidas-1".to_string(), 0_u64),
        ("nike-1".to_string(), 100_u64),
        ("puma-1".to_string(), 0_u64),
    ]);

    let reranked = rerank_by_ctr(docs, &click_counts, None);
    let ids: Vec<String> = reranked.into_iter().map(|doc| doc.document.id).collect();

    assert_eq!(ids[0], "nike-1");
}

#[test]
fn rerank_by_ctr_empty_clicks_keeps_order() {
    let docs = vec![
        scored_doc("doc-1", 3.0),
        scored_doc("doc-2", 2.0),
        scored_doc("doc-3", 1.0),
    ];
    let click_counts: HashMap<String, u64> = HashMap::new();

    let reranked = rerank_by_ctr(docs, &click_counts, None);
    let ids: Vec<String> = reranked.into_iter().map(|doc| doc.document.id).collect();

    assert_eq!(ids, vec!["doc-1", "doc-2", "doc-3"]);
}

/// Assert that documents with equal click counts and BM25 scores preserve their original ordering.
#[test]
fn rerank_by_ctr_stable_tie_breaking_keeps_original_order() {
    let docs = vec![
        scored_doc("doc-a", 5.0),
        scored_doc("doc-b", 5.0),
        scored_doc("doc-c", 5.0),
    ];
    let click_counts = HashMap::from([
        ("doc-a".to_string(), 10_u64),
        ("doc-b".to_string(), 10_u64),
        ("doc-c".to_string(), 10_u64),
    ]);

    let reranked = rerank_by_ctr(docs, &click_counts, None);
    let ids: Vec<String> = reranked.into_iter().map(|doc| doc.document.id).collect();

    assert_eq!(ids, vec!["doc-a", "doc-b", "doc-c"]);
}

/// Confirm that re-ranking with all equal BM25 scores does not panic.
#[test]
fn rerank_by_ctr_all_bm25_scores_equal_does_not_panic() {
    let docs = vec![
        scored_doc("doc-1", 5.0),
        scored_doc("doc-2", 5.0),
        scored_doc("doc-3", 5.0),
    ];
    let click_counts = HashMap::from([
        ("doc-1".to_string(), 1_u64),
        ("doc-2".to_string(), 50_u64),
        ("doc-3".to_string(), 5_u64),
    ]);

    let reranked = rerank_by_ctr(docs, &click_counts, None);
    let ids: Vec<String> = reranked.into_iter().map(|doc| doc.document.id).collect();

    assert_eq!(ids[0], "doc-2");
}

/// Assert that strictness=100 preserves original text ranking despite high click counts.
#[test]
fn rerank_by_ctr_strictness_hundred_keeps_textual_order() {
    let docs = vec![
        scored_doc("adidas-1", 10.0),
        scored_doc("nike-1", 9.0),
        scored_doc("puma-1", 8.0),
    ];
    let click_counts = HashMap::from([
        ("adidas-1".to_string(), 0_u64),
        ("nike-1".to_string(), 500_u64),
        ("puma-1".to_string(), 0_u64),
    ]);

    let reranked = rerank_by_ctr(docs, &click_counts, Some(100));
    let ids: Vec<String> = reranked.into_iter().map(|doc| doc.document.id).collect();
    assert_eq!(ids, vec!["adidas-1", "nike-1", "puma-1"]);
}

/// Verify that strictness=0 makes click-through rate the dominant ranking signal.
#[test]
fn rerank_by_ctr_strictness_zero_makes_ctr_dominant() {
    let docs = vec![
        scored_doc("adidas-1", 10.0),
        scored_doc("nike-1", 9.0),
        scored_doc("puma-1", 8.0),
    ];
    let click_counts = HashMap::from([
        ("adidas-1".to_string(), 0_u64),
        ("nike-1".to_string(), 500_u64),
        ("puma-1".to_string(), 0_u64),
    ]);

    let reranked = rerank_by_ctr(docs, &click_counts, Some(0));
    let ids: Vec<String> = reranked.into_iter().map(|doc| doc.document.id).collect();
    assert_eq!(ids[0], "nike-1");
}

#[test]
fn document_matches_filter_not_with_unsupported_clause_returns_false() {
    let filter = flapjack::filter_parser::parse_filter("NOT (price > 100)").unwrap();
    let mut fields = HashMap::new();
    fields.insert("brand".to_string(), FieldValue::Facet("Nike".to_string()));
    let doc = Document {
        id: "doc-1".to_string(),
        fields,
    };

    assert!(
        !document_matches_filter(&doc, &filter),
        "unsupported filter operators should not match for reranking subset selection"
    );
}

/// Assert that NOT filters with equals clauses correctly identify non-matching documents.
#[test]
fn document_matches_filter_not_with_supported_equals_clause_works() {
    let filter = flapjack::filter_parser::parse_filter("NOT brand:Nike").unwrap();

    let mut nike_fields = HashMap::new();
    nike_fields.insert("brand".to_string(), FieldValue::Facet("Nike".to_string()));
    let nike_doc = Document {
        id: "nike-1".to_string(),
        fields: nike_fields,
    };

    let mut adidas_fields = HashMap::new();
    adidas_fields.insert("brand".to_string(), FieldValue::Facet("Adidas".to_string()));
    let adidas_doc = Document {
        id: "adidas-1".to_string(),
        fields: adidas_fields,
    };

    assert!(!document_matches_filter(&nike_doc, &filter));
    assert!(document_matches_filter(&adidas_doc, &filter));
}

/// Create a tenant index and synchronously add documents to it, optionally setting index settings.
async fn create_index_with_docs(
    state: &Arc<AppState>,
    index_name: &str,
    docs: Vec<Vec<(&str, &str)>>,
) {
    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        attributes_for_faceting: vec!["searchable(category)".to_string()],
        ..Default::default()
    };
    save_index_settings(state, index_name, &settings);
    for (i, doc) in docs.iter().enumerate() {
        let mut fields = std::collections::HashMap::new();
        for (k, v) in doc {
            fields.insert(k.to_string(), flapjack::FieldValue::Text(v.to_string()));
        }
        let document = flapjack::Document {
            id: format!("doc_{index_name}_{i}"),
            fields,
        };
        state
            .manager
            .add_documents_sync(index_name, vec![document])
            .await
            .unwrap();
    }
}

// ── extract_analytics_headers ──

/// Build an HTTP request for analytics testing with optional headers, peer address, and trusted proxy CIDR configuration.
fn analytics_request(
    header_pairs: &[(&str, &str)],
    peer: Option<&str>,
    trusted_proxy_cidrs: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder().uri("/1/indexes/products/query");
    for (name, value) in header_pairs {
        builder = builder.header(*name, *value);
    }

    let mut request = builder.body(Body::empty()).unwrap();

    if let Some(raw_peer) = peer {
        let socket_addr: std::net::SocketAddr = raw_peer.parse().expect("valid socket address");
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(socket_addr));
    }

    if let Some(raw_cidrs) = trusted_proxy_cidrs {
        request.extensions_mut().insert(std::sync::Arc::new(
            crate::middleware::TrustedProxyMatcher::from_csv(raw_cidrs)
                .expect("valid trusted proxy CIDRs"),
        ));
    }

    request
}

#[test]
fn analytics_headers_empty() {
    let request = analytics_request(&[], None, None);
    let (token, ip, session_id) = extract_analytics_headers(&request);
    assert!(token.is_none());
    assert!(ip.is_none());
    assert!(session_id.is_none());
}

#[test]
fn analytics_headers_user_token() {
    let request = analytics_request(&[("x-algolia-usertoken", "user123")], None, None);
    let (token, _, _) = extract_analytics_headers(&request);
    assert_eq!(token, Some("user123".to_string()));
}

#[test]
fn analytics_headers_uses_first_untrusted_from_right_when_peer_is_trusted_proxy() {
    let request = analytics_request(
        &[("x-forwarded-for", "1.2.3.4, 5.6.7.8")],
        Some("127.0.0.77:7700"),
        Some("127.0.0.0/8"),
    );
    let (_, ip, _) = extract_analytics_headers(&request);
    assert_eq!(ip, Some("5.6.7.8".to_string()));
}

#[test]
fn analytics_headers_can_resolve_origin_when_intermediate_proxy_is_trusted() {
    let request = analytics_request(
        &[("x-forwarded-for", "1.2.3.4, 5.6.7.8")],
        Some("127.0.0.77:7700"),
        Some("127.0.0.0/8,5.6.7.0/24"),
    );
    let (_, ip, _) = extract_analytics_headers(&request);
    assert_eq!(ip, Some("1.2.3.4".to_string()));
}

#[test]
fn analytics_headers_real_ip_fallback_when_peer_is_trusted_proxy() {
    let request = analytics_request(
        &[("x-real-ip", "10.0.0.1")],
        Some("127.0.0.77:7700"),
        Some("127.0.0.0/8"),
    );
    let (_, ip, _) = extract_analytics_headers(&request);
    assert_eq!(ip, Some("10.0.0.1".to_string()));
}

#[test]
fn analytics_headers_untrusted_proxy_uses_peer_ip() {
    let request = analytics_request(
        &[("x-forwarded-for", "1.2.3.4"), ("x-real-ip", "10.0.0.1")],
        Some("203.0.113.9:9000"),
        Some("127.0.0.0/8"),
    );
    let (_, ip, _) = extract_analytics_headers(&request);
    assert_eq!(ip, Some("203.0.113.9".to_string()));
}

#[test]
fn analytics_headers_session_id() {
    let request = analytics_request(&[("x-algolia-session-id", "sid-123")], None, None);
    let (_, _, session_id) = extract_analytics_headers(&request);
    assert_eq!(session_id, Some("sid-123".to_string()));
}

// ── extract_single_geoloc ──

#[test]
fn geoloc_from_float_object() {
    let mut map = HashMap::new();
    map.insert("lat".to_string(), FieldValue::Float(48.8566));
    map.insert("lng".to_string(), FieldValue::Float(2.3522));
    let val = FieldValue::Object(map);
    let result = extract_single_geoloc(&val);
    assert_eq!(result, Some((48.8566, 2.3522)));
}

#[test]
fn geoloc_from_integer_object() {
    let mut map = HashMap::new();
    map.insert("lat".to_string(), FieldValue::Integer(48));
    map.insert("lng".to_string(), FieldValue::Integer(2));
    let val = FieldValue::Object(map);
    let result = extract_single_geoloc(&val);
    assert_eq!(result, Some((48.0, 2.0)));
}

#[test]
fn geoloc_missing_lat() {
    let mut map = HashMap::new();
    map.insert("lng".to_string(), FieldValue::Float(2.0));
    let val = FieldValue::Object(map);
    assert_eq!(extract_single_geoloc(&val), None);
}

#[test]
fn geoloc_missing_lng() {
    let mut map = HashMap::new();
    map.insert("lat".to_string(), FieldValue::Float(48.0));
    let val = FieldValue::Object(map);
    assert_eq!(extract_single_geoloc(&val), None);
}

#[test]
fn geoloc_wrong_type() {
    let val = FieldValue::Text("not a geoloc".into());
    assert_eq!(extract_single_geoloc(&val), None);
}

#[test]
fn geoloc_string_lat_returns_none() {
    let mut map = HashMap::new();
    map.insert("lat".to_string(), FieldValue::Text("48.0".into()));
    map.insert("lng".to_string(), FieldValue::Float(2.0));
    let val = FieldValue::Object(map);
    assert_eq!(extract_single_geoloc(&val), None);
}

// ── extract_all_geolocs ──

#[test]
fn all_geolocs_none() {
    assert!(extract_all_geolocs(None).is_empty());
}

#[test]
fn all_geolocs_single_object() {
    let mut map = HashMap::new();
    map.insert("lat".to_string(), FieldValue::Float(48.0));
    map.insert("lng".to_string(), FieldValue::Float(2.0));
    let val = FieldValue::Object(map);
    let result = extract_all_geolocs(Some(&val));
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], (48.0, 2.0));
}

#[test]
fn all_geolocs_array() {
    let mut m1 = HashMap::new();
    m1.insert("lat".to_string(), FieldValue::Float(48.0));
    m1.insert("lng".to_string(), FieldValue::Float(2.0));
    let mut m2 = HashMap::new();
    m2.insert("lat".to_string(), FieldValue::Float(40.7));
    m2.insert("lng".to_string(), FieldValue::Float(-74.0));
    let val = FieldValue::Array(vec![FieldValue::Object(m1), FieldValue::Object(m2)]);
    let result = extract_all_geolocs(Some(&val));
    assert_eq!(result.len(), 2);
}

#[test]
fn all_geolocs_non_object_value() {
    let val = FieldValue::Text("nope".into());
    assert!(extract_all_geolocs(Some(&val)).is_empty());
}

/// Verify that rule overrides replace center/radius and clear bounding boxes and polygons.
#[test]
fn apply_rule_geo_overrides_replaces_request_center_and_radius() {
    let request_geo = flapjack::query::geo::GeoParams {
        around: Some(flapjack::query::geo::GeoPoint {
            lat: 40.7128,
            lng: -74.0060,
        }),
        around_radius: Some(flapjack::query::geo::AroundRadius::Meters(1000)),
        bounding_boxes: vec![flapjack::query::geo::BoundingBox {
            p1_lat: 42.0,
            p1_lng: -88.0,
            p2_lat: 40.0,
            p2_lng: -73.0,
        }],
        polygons: vec![vec![(40.0, -80.0), (41.0, -81.0), (42.0, -79.0)]],
        around_precision: flapjack::query::geo::AroundPrecisionConfig::default(),
        minimum_around_radius: None,
    };

    let overridden = apply_rule_geo_overrides(
        request_geo,
        Some("34.0522, -118.2437"),
        Some(&json!(300000)),
    );

    let center = overridden.around.expect("rule aroundLatLng should apply");
    assert!((center.lat - 34.0522).abs() < 0.0001);
    assert!((center.lng + 118.2437).abs() < 0.0001);
    assert!(overridden.bounding_boxes.is_empty());
    assert!(overridden.polygons.is_empty());
    assert!(matches!(
        overridden.around_radius,
        Some(flapjack::query::geo::AroundRadius::Meters(300000))
    ));
}

/// Confirm that rule overrides update radius when the request center is already set.
#[test]
fn apply_rule_geo_overrides_updates_radius_when_center_exists() {
    let request_geo = flapjack::query::geo::GeoParams {
        around: Some(flapjack::query::geo::GeoPoint {
            lat: 40.7128,
            lng: -74.0060,
        }),
        around_radius: Some(flapjack::query::geo::AroundRadius::Meters(1000)),
        bounding_boxes: vec![],
        polygons: vec![],
        around_precision: flapjack::query::geo::AroundPrecisionConfig::default(),
        minimum_around_radius: None,
    };

    let overridden = apply_rule_geo_overrides(request_geo, None, Some(&json!("all")));

    assert!(matches!(
        overridden.around_radius,
        Some(flapjack::query::geo::AroundRadius::All)
    ));
}

/// Assert that params echo includes numericFilters as a string when provided as a string.
#[test]
fn build_params_echo_includes_numeric_filters_string() {
    let req = SearchRequest {
        query: "laptop".to_string(),
        numeric_filters: Some(json!("price > 10")),
        ..Default::default()
    };
    let params = build_params_echo(&req);
    let parsed: HashMap<String, String> = url::form_urlencoded::parse(params.as_bytes())
        .into_owned()
        .collect();

    assert_eq!(
        parsed.get("numericFilters"),
        Some(&"price > 10".to_string())
    );
}

/// Assert that params echo includes numericFilters as a JSON array when provided as an array.
#[test]
fn build_params_echo_includes_numeric_filters_array() {
    let numeric_filters = json!(["price > 10", "quantity < 20"]);
    let req = SearchRequest {
        numeric_filters: Some(numeric_filters.clone()),
        ..Default::default()
    };
    let params = build_params_echo(&req);
    let parsed: HashMap<String, String> = url::form_urlencoded::parse(params.as_bytes())
        .into_owned()
        .collect();

    assert_eq!(
        parsed.get("numericFilters"),
        Some(&numeric_filters.to_string())
    );
}

/// Assert that params echo includes aroundLatLng when provided.
#[test]
fn build_params_echo_includes_around_lat_lng() {
    let req = SearchRequest {
        query: "".to_string(),
        around_lat_lng: Some("40.7128,-74.0060".to_string()),
        ..Default::default()
    };
    let params = build_params_echo(&req);
    let parsed: HashMap<String, String> = url::form_urlencoded::parse(params.as_bytes())
        .into_owned()
        .collect();

    assert_eq!(
        parsed.get("aroundLatLng"),
        Some(&"40.7128,-74.0060".to_string())
    );
}

// ── merge_secured_filters ──

#[test]
fn merge_secured_filters_adds_to_empty() {
    let mut req = SearchRequest::default();
    let restrictions = crate::auth::SecuredKeyRestrictions {
        filters: Some("brand:Nike".to_string()),
        ..Default::default()
    };
    merge_secured_filters(&mut req, &restrictions);
    assert_eq!(req.filters, Some("brand:Nike".to_string()));
}

#[test]
fn merge_secured_filters_combines_with_existing() {
    let mut req = SearchRequest {
        filters: Some("color:Red".to_string()),
        ..Default::default()
    };
    let restrictions = crate::auth::SecuredKeyRestrictions {
        filters: Some("brand:Nike".to_string()),
        ..Default::default()
    };
    merge_secured_filters(&mut req, &restrictions);
    assert_eq!(
        req.filters,
        Some("(color:Red) AND (brand:Nike)".to_string())
    );
}

#[test]
fn merge_secured_filters_no_filters() {
    let mut req = SearchRequest::default();
    let restrictions = crate::auth::SecuredKeyRestrictions::default();
    merge_secured_filters(&mut req, &restrictions);
    assert!(req.filters.is_none());
}

#[test]
fn merge_secured_filters_caps_hits_per_page() {
    let mut req = SearchRequest {
        hits_per_page: Some(100),
        ..Default::default()
    };
    let restrictions = crate::auth::SecuredKeyRestrictions {
        hits_per_page: Some(20),
        ..Default::default()
    };
    merge_secured_filters(&mut req, &restrictions);
    assert_eq!(req.hits_per_page, Some(20));
}

#[test]
fn merge_secured_filters_no_cap_when_lower() {
    let mut req = SearchRequest {
        hits_per_page: Some(10),
        ..Default::default()
    };
    let restrictions = crate::auth::SecuredKeyRestrictions {
        hits_per_page: Some(20),
        ..Default::default()
    };
    merge_secured_filters(&mut req, &restrictions);
    assert_eq!(req.hits_per_page, Some(10));
}

#[test]
fn merge_secured_filters_applies_when_none() {
    let mut req = SearchRequest::default();
    assert!(req.hits_per_page.is_none()); // precondition
    let restrictions = crate::auth::SecuredKeyRestrictions {
        hits_per_page: Some(20),
        ..Default::default()
    };
    merge_secured_filters(&mut req, &restrictions);
    assert_eq!(req.hits_per_page, Some(20));
}

#[test]
fn merge_secured_filters_empty_filter_string() {
    let mut req = SearchRequest {
        filters: Some("color:Red".to_string()),
        ..Default::default()
    };
    let restrictions = crate::auth::SecuredKeyRestrictions {
        filters: Some("".to_string()),
        ..Default::default()
    };
    merge_secured_filters(&mut req, &restrictions);
    // Empty string still gets combined — caller should avoid passing empty
    assert_eq!(req.filters, Some("(color:Red) AND ()".to_string()));
}

#[test]
fn merge_secured_filters_both_filters_and_hpp() {
    let mut req = SearchRequest {
        hits_per_page: Some(100),
        ..Default::default()
    };
    let restrictions = crate::auth::SecuredKeyRestrictions {
        filters: Some("brand:Nike".to_string()),
        hits_per_page: Some(20),
        ..Default::default()
    };
    merge_secured_filters(&mut req, &restrictions);
    assert_eq!(req.filters, Some("brand:Nike".to_string()));
    assert_eq!(req.hits_per_page, Some(20));
}

#[test]
fn merge_secured_filters_sets_forced_user_token() {
    let mut req = SearchRequest::default();
    let restrictions = crate::auth::SecuredKeyRestrictions {
        user_token: Some("secured-user".to_string()),
        ..Default::default()
    };

    merge_secured_filters(&mut req, &restrictions);

    assert_eq!(req.user_token, Some("secured-user".to_string()));
}

#[test]
fn merge_secured_filters_overrides_request_user_token() {
    let mut req = SearchRequest {
        user_token: Some("request-user".to_string()),
        ..Default::default()
    };
    let restrictions = crate::auth::SecuredKeyRestrictions {
        user_token: Some("secured-user".to_string()),
        ..Default::default()
    };

    merge_secured_filters(&mut req, &restrictions);

    assert_eq!(req.user_token, Some("secured-user".to_string()));
}

// ── resolve_search_mode ──

#[test]
fn test_resolve_search_mode_query_overrides_settings() {
    use flapjack::index::settings::{IndexMode, IndexSettings};
    let query_mode = Some(IndexMode::NeuralSearch);
    let settings = IndexSettings {
        mode: Some(IndexMode::KeywordSearch),
        ..Default::default()
    };
    let result = resolve_search_mode(&query_mode, &settings);
    assert_eq!(result, IndexMode::NeuralSearch);
}

#[test]
fn test_resolve_search_mode_falls_back_to_settings() {
    use flapjack::index::settings::{IndexMode, IndexSettings};
    let query_mode = None;
    let settings = IndexSettings {
        mode: Some(IndexMode::NeuralSearch),
        ..Default::default()
    };
    let result = resolve_search_mode(&query_mode, &settings);
    assert_eq!(result, IndexMode::NeuralSearch);
}

#[test]
fn test_resolve_search_mode_both_none_is_keyword() {
    use flapjack::index::settings::{IndexMode, IndexSettings};
    let query_mode = None;
    let settings = IndexSettings::default();
    let result = resolve_search_mode(&query_mode, &settings);
    assert_eq!(result, IndexMode::KeywordSearch);
}

#[test]
fn test_resolve_search_mode_query_keyword_overrides_settings_neural() {
    // A per-query KeywordSearch must override an index-level NeuralSearch setting.
    // Critical: users must be able to opt out of hybrid search on a per-query basis.
    use flapjack::index::settings::{IndexMode, IndexSettings};
    let query_mode = Some(IndexMode::KeywordSearch);
    let settings = IndexSettings {
        mode: Some(IndexMode::NeuralSearch),
        ..Default::default()
    };
    let result = resolve_search_mode(&query_mode, &settings);
    assert_eq!(result, IndexMode::KeywordSearch);
}

#[test]
fn test_index_allows_personalization_default_true() {
    assert!(index_allows_personalization(None));
}

#[test]
fn test_index_allows_personalization_true_setting() {
    use flapjack::index::settings::IndexSettings;
    let settings = IndexSettings {
        enable_personalization: Some(true),
        ..Default::default()
    };
    assert!(index_allows_personalization(Some(&settings)));
}

#[test]
fn test_index_allows_personalization_false_setting() {
    use flapjack::index::settings::IndexSettings;
    let settings = IndexSettings {
        enable_personalization: Some(false),
        ..Default::default()
    };
    assert!(!index_allows_personalization(Some(&settings)));
}

#[test]
fn test_compute_personalization_impact_multiplier_handles_zero_inputs() {
    assert_eq!(compute_personalization_impact_multiplier(0, 100), None);
    assert_eq!(compute_personalization_impact_multiplier(100, 0), None);
    assert_eq!(compute_personalization_impact_multiplier(50, 40), Some(0.2));
}

#[test]
fn test_convert_profile_scores_to_affinity_preserves_nested_values() {
    let profile_scores = BTreeMap::from([(
        "brand".to_string(),
        BTreeMap::from([("nike".to_string(), 12_u32), ("adidas".to_string(), 8_u32)]),
    )]);

    let affinity_map = convert_profile_scores_to_affinity_map(profile_scores);
    let brand_scores = affinity_map.get("brand").expect("brand facet should exist");

    assert_eq!(brand_scores.get("nike"), Some(&12_u32));
    assert_eq!(brand_scores.get("adidas"), Some(&8_u32));
}

#[test]
fn test_resolve_search_mode_settings_keyword_propagates() {
    // Explicit KeywordSearch in settings should propagate (not be shadowed by default).
    use flapjack::index::settings::{IndexMode, IndexSettings};
    let query_mode = None;
    let settings = IndexSettings {
        mode: Some(IndexMode::KeywordSearch),
        ..Default::default()
    };
    let result = resolve_search_mode(&query_mode, &settings);
    assert_eq!(result, IndexMode::KeywordSearch);
}

// ── A6: apply_query_overrides ──

#[test]
fn apply_overrides_typo_tolerance() {
    let mut req = SearchRequest::default();
    let overrides = QueryOverrides {
        typo_tolerance: Some(json!(false)),
        ..Default::default()
    };
    apply_query_overrides(&mut req, &overrides);
    assert_eq!(req.typo_tolerance, Some(json!(false)));
}

#[test]
fn apply_overrides_enable_synonyms() {
    let mut req = SearchRequest::default();
    let overrides = QueryOverrides {
        enable_synonyms: Some(false),
        ..Default::default()
    };
    apply_query_overrides(&mut req, &overrides);
    assert_eq!(req.enable_synonyms, Some(false));
}

#[test]
fn apply_overrides_enable_rules() {
    let mut req = SearchRequest::default();
    let overrides = QueryOverrides {
        enable_rules: Some(false),
        ..Default::default()
    };
    apply_query_overrides(&mut req, &overrides);
    assert_eq!(req.enable_rules, Some(false));
}

#[test]
fn apply_overrides_rule_contexts() {
    let mut req = SearchRequest::default();
    let overrides = QueryOverrides {
        rule_contexts: Some(vec!["sale".to_string()]),
        ..Default::default()
    };
    apply_query_overrides(&mut req, &overrides);
    assert_eq!(req.rule_contexts, Some(vec!["sale".to_string()]));
}

#[test]
fn apply_overrides_filters() {
    let mut req = SearchRequest::default();
    let overrides = QueryOverrides {
        filters: Some("brand:Nike".to_string()),
        ..Default::default()
    };
    apply_query_overrides(&mut req, &overrides);
    assert_eq!(req.filters, Some("brand:Nike".to_string()));
}

#[test]
fn apply_overrides_optional_filters() {
    let mut req = SearchRequest::default();
    let overrides = QueryOverrides {
        optional_filters: Some(vec!["brand:Nike".to_string()]),
        ..Default::default()
    };
    apply_query_overrides(&mut req, &overrides);
    assert_eq!(req.optional_filters, Some(json!(["brand:Nike"])));
}

#[test]
fn apply_overrides_remove_words_if_no_results() {
    let mut req = SearchRequest::default();
    let overrides = QueryOverrides {
        remove_words_if_no_results: Some("lastWords".to_string()),
        ..Default::default()
    };
    apply_query_overrides(&mut req, &overrides);
    assert_eq!(
        req.remove_words_if_no_results,
        Some("lastWords".to_string())
    );
}

#[test]
fn apply_overrides_skips_none_fields() {
    let mut req = SearchRequest {
        filters: Some("existing".to_string()),
        enable_rules: Some(true),
        optional_filters: Some(json!(["old"])),
        ..Default::default()
    };
    let overrides = QueryOverrides::default();

    apply_query_overrides(&mut req, &overrides);

    assert_eq!(req.filters, Some("existing".to_string()));
    assert_eq!(req.enable_rules, Some(true));
    assert_eq!(req.optional_filters, Some(json!(["old"])));
}

#[test]
fn apply_overrides_does_not_clobber_existing() {
    let mut req = SearchRequest {
        filters: Some("existing".to_string()),
        ..Default::default()
    };
    let overrides = QueryOverrides {
        enable_synonyms: Some(false),
        ..Default::default()
    };

    apply_query_overrides(&mut req, &overrides);

    assert_eq!(req.enable_synonyms, Some(false));
    assert_eq!(req.filters, Some("existing".to_string()));
}

/// Verify that query overrides do not modify index-level configuration fields (custom_ranking, attribute_weights).
#[test]
fn apply_overrides_skips_index_level_fields() {
    let mut req = SearchRequest {
        filters: Some("existing".to_string()),
        enable_synonyms: Some(true),
        ..Default::default()
    };
    let overrides = QueryOverrides {
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        attribute_weights: Some(std::iter::once(("title".to_string(), 10.0)).collect()),
        ..Default::default()
    };

    apply_query_overrides(&mut req, &overrides);

    assert_eq!(
        req.filters,
        Some("existing".to_string()),
        "index-level overrides must not mutate query request fields"
    );
    assert_eq!(req.enable_synonyms, Some(true));
}

/// Verify that experiment filter overrides AND with pre-existing secured filters rather than replacing them, preserving security boundaries.
#[test]
fn apply_overrides_filters_merges_with_existing_secured_filters() {
    // Security: experiment filter overrides must AND with pre-existing
    // filters (e.g. from secured API keys), not replace them.
    let mut req = SearchRequest {
        filters: Some("(category:public)".to_string()),
        ..Default::default()
    };
    let overrides = QueryOverrides {
        filters: Some("brand:Nike".to_string()),
        ..Default::default()
    };
    apply_query_overrides(&mut req, &overrides);
    let filters = req.filters.unwrap();
    assert!(
        filters.contains("category:public"),
        "secured filter must be preserved, got: {}",
        filters
    );
    assert!(
        filters.contains("brand:Nike"),
        "experiment filter must be applied, got: {}",
        filters
    );
}

// ── A6: assignment_method_str ──

#[test]
fn assignment_method_to_string_user_token() {
    assert_eq!(
        assignment_method_str(&AssignmentMethod::UserToken),
        "user_token"
    );
}

#[test]
fn assignment_method_to_string_query_id() {
    assert_eq!(
        assignment_method_str(&AssignmentMethod::QueryId),
        "query_id"
    );
}

#[test]
fn assignment_method_to_string_session_id() {
    assert_eq!(
        assignment_method_str(&AssignmentMethod::SessionId),
        "session_id"
    );
}

// ── personalization_affinity_for_document ──

/// Verify that nested facet paths (e.g., categories.lvl0) are correctly resolved during personalization scoring.
#[test]
fn personalization_affinity_nested_facet_path() {
    // Bug: nested facet paths like "categories.lvl0" should be traversed
    // into the document's Object fields, not looked up as a flat key.
    let mut fields = HashMap::new();
    fields.insert("brand".to_string(), FieldValue::Facet("Nike".to_string()));
    let mut categories = HashMap::new();
    categories.insert("lvl0".to_string(), FieldValue::Facet("Shoes".to_string()));
    fields.insert("categories".to_string(), FieldValue::Object(categories));
    let doc = Document {
        id: "prod-1".to_string(),
        fields,
    };

    let mut affinity_scores = HashMap::new();
    // Flat facet — should work
    affinity_scores.insert(
        "brand".to_string(),
        [("Nike".to_string(), 10u32)].into_iter().collect(),
    );
    // Nested facet — must also work
    affinity_scores.insert(
        "categories.lvl0".to_string(),
        [("Shoes".to_string(), 5u32)].into_iter().collect(),
    );

    let score = personalization_affinity_for_document(&doc, &affinity_scores);
    // brand:Nike = 10, categories.lvl0:Shoes = 5 => total 15
    assert_eq!(
        score, 15.0,
        "nested facet path 'categories.lvl0' must be resolved via dot-path traversal"
    );
}

fn make_doc(id: &str, title: &str) -> Document {
    let mut fields = HashMap::new();
    fields.insert("title".to_string(), FieldValue::Text(title.to_string()));
    Document {
        id: id.to_string(),
        fields,
    }
}

/// Construct a Mode A experiment that applies query overrides to the variant arm.
fn mode_a_experiment(id: &str, index_name: &str) -> Experiment {
    Experiment {
        id: id.to_string(),
        name: format!("mode-a-{index_name}"),
        index_name: index_name.to_string(),
        status: ExperimentStatus::Draft,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(QueryOverrides {
                enable_synonyms: Some(false),
                ..Default::default()
            }),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: chrono::Utc::now().timestamp_millis(),
        started_at: None,
        ended_at: None,
        stopped_at: None,
        minimum_days: 14,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    }
}

/// Construct a Mode B experiment that routes traffic between a control and variant index.
fn mode_b_experiment(id: &str, index_name: &str, variant_index_name: &str) -> Experiment {
    Experiment {
        id: id.to_string(),
        name: format!("mode-b-{index_name}"),
        index_name: index_name.to_string(),
        status: ExperimentStatus::Draft,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: None,
            index_name: Some(variant_index_name.to_string()),
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: chrono::Utc::now().timestamp_millis(),
        started_at: None,
        ended_at: None,
        stopped_at: None,
        minimum_days: 14,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    }
}

fn interleaving_experiment(id: &str, index_name: &str, variant_index_name: &str) -> Experiment {
    let mut experiment = mode_b_experiment(id, index_name, variant_index_name);
    experiment.interleaving = Some(true);
    experiment
}

/// Create an AppState with experiment store and three Mode B index pairs plus active Mode A, B, and interleaving experiments.
async fn make_search_experiment_state(tmp: &TempDir) -> Arc<AppState> {
    let state = crate::test_helpers::TestStateBuilder::new(tmp)
        .with_experiments()
        .build_shared();
    let experiment_store = state
        .experiment_store
        .as_ref()
        .expect("experiment store should be configured")
        .clone();

    state.manager.create_tenant("products").unwrap();
    state
        .manager
        .add_documents_sync(
            "products",
            vec![
                make_doc("p1", "nike running shoe"),
                make_doc("p2", "adidas trail shoe"),
            ],
        )
        .await
        .unwrap();

    state.manager.create_tenant("products_mode_b").unwrap();
    state
        .manager
        .add_documents_sync(
            "products_mode_b",
            vec![make_doc("m1", "control index document")],
        )
        .await
        .unwrap();

    state
        .manager
        .create_tenant("products_mode_b_variant")
        .unwrap();
    state
        .manager
        .add_documents_sync(
            "products_mode_b_variant",
            vec![make_doc("mv1", "variant index document")],
        )
        .await
        .unwrap();

    state
        .manager
        .create_tenant("products_no_experiment")
        .unwrap();
    state
        .manager
        .add_documents_sync(
            "products_no_experiment",
            vec![make_doc("n1", "plain index document")],
        )
        .await
        .unwrap();

    state.manager.create_tenant("products_interleave").unwrap();
    state
        .manager
        .add_documents_sync(
            "products_interleave",
            vec![make_doc("ic1", "interleave control document")],
        )
        .await
        .unwrap();

    state
        .manager
        .create_tenant("products_interleave_variant")
        .unwrap();
    state
        .manager
        .add_documents_sync(
            "products_interleave_variant",
            vec![make_doc("iv1", "interleave variant document")],
        )
        .await
        .unwrap();

    experiment_store
        .create(mode_a_experiment("exp-mode-a", "products"))
        .unwrap();
    experiment_store.start("exp-mode-a").unwrap();

    experiment_store
        .create(mode_b_experiment(
            "exp-mode-b",
            "products_mode_b",
            "products_mode_b_variant",
        ))
        .unwrap();
    experiment_store.start("exp-mode-b").unwrap();

    experiment_store
        .create(interleaving_experiment(
            "exp-interleave",
            "products_interleave",
            "products_interleave_variant",
        ))
        .unwrap();
    experiment_store.start("exp-interleave").unwrap();

    state
}

fn search_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/1/indexes/:indexName/query", post(search))
        .route("/1/indexes/:indexName", get(search_get))
        .with_state(state)
}

fn batch_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/1/indexes/:indexName/queries", post(batch_search))
        .with_state(state)
}

/// Create a router with search, settings, personalization strategy, profile deletion, and recommendations endpoints.
fn search_personalization_recommend_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/1/indexes/:indexName/query", post(search))
        .route("/1/indexes/:indexName/settings", post(set_settings))
        .route(
            "/1/strategies/personalization",
            post(set_personalization_strategy),
        )
        .route(
            "/1/profiles/personalization/:userToken",
            get(get_user_profile),
        )
        .route("/1/profiles/:userToken", delete(delete_user_profile))
        .route("/1/indexes/:_wildcard/recommendations", post(recommend))
        .with_state(state)
}

/// Send a POST search request to the router with optional user token in headers.
async fn post_search(
    app: &Router,
    index_name: &str,
    body: Value,
    user_token: Option<&str>,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(format!("/1/indexes/{index_name}/query"))
        .header("content-type", "application/json");
    if let Some(token) = user_token {
        builder = builder.header("x-algolia-usertoken", token);
    }
    app.clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn get_search(app: &Router, uri: &str) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn post_batch_search(app: &Router, body: Value) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/indexes/*/queries")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn post_json_uri(app: &Router, uri: &str, body: Value) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Send a POST recommendations request to the router and return the status code with parsed JSON body.
async fn post_recommendation_request(app: &Router, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/indexes/*/recommendations")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, json)
}

/// Send a DELETE request to remove the personalization profile for a user token.
async fn delete_user_profile_request(app: &Router, user_token: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/1/profiles/{user_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, json)
}

/// Create an AppState with IndexManager, dictionary manager, metrics state, and optional AnalyticsQueryEngine.
fn make_basic_search_state_with_analytics(
    tmp: &TempDir,
    analytics_engine: Option<Arc<AnalyticsQueryEngine>>,
) -> Arc<AppState> {
    let builder = crate::test_helpers::TestStateBuilder::new(tmp);
    match analytics_engine {
        Some(engine) => builder.with_analytics_engine(engine).build_shared(),
        None => builder.build_shared(),
    }
}

fn make_basic_search_state(tmp: &TempDir) -> Arc<AppState> {
    make_basic_search_state_with_analytics(tmp, None)
}

fn test_analytics_config(tmp: &TempDir) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 3600,
        flush_size: 100_000,
        retention_days: 90,
    }
}

fn make_brand_doc(id: &str, title: &str, brand: &str) -> Document {
    let mut fields = HashMap::new();
    fields.insert("title".to_string(), FieldValue::Text(title.to_string()));
    fields.insert("brand".to_string(), FieldValue::Facet(brand.to_string()));
    Document {
        id: id.to_string(),
        fields,
    }
}

/// Construct an InsightEvent representing a click action on a document.
fn make_click_event(
    user_token: &str,
    index_name: &str,
    object_id: &str,
    timestamp_ms: i64,
) -> InsightEvent {
    InsightEvent {
        event_type: "click".to_string(),
        event_subtype: None,
        event_name: "Product Clicked".to_string(),
        index: index_name.to_string(),
        user_token: user_token.to_string(),
        authenticated_user_token: None,
        query_id: None,
        object_ids: vec![object_id.to_string()],
        object_ids_alt: vec![],
        positions: Some(vec![1]),
        timestamp: Some(timestamp_ms),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

/// Record multiple identical click InsightEvents in the analytics collector for a user/index/document combination.
fn record_click_events(
    collector: &AnalyticsCollector,
    user_token: &str,
    index_name: &str,
    object_id: &str,
    count: usize,
    timestamp_ms: i64,
) {
    for _ in 0..count {
        collector.record_insight(make_click_event(
            user_token,
            index_name,
            object_id,
            timestamp_ms,
        ));
    }
}

/// Create a TempDir, router, analytics collector, and index configured for re-ranking tests with optional settings.
async fn setup_rerank_search_fixture(
    tmp: &TempDir,
    index_name: &str,
    docs: Vec<Document>,
    settings: Option<flapjack::index::settings::IndexSettings>,
) -> (Router, Arc<AnalyticsCollector>) {
    let analytics_cfg = test_analytics_config(tmp);
    let collector = AnalyticsCollector::new(analytics_cfg.clone());
    let analytics_engine = Arc::new(AnalyticsQueryEngine::new(analytics_cfg));
    let state = make_basic_search_state_with_analytics(tmp, Some(analytics_engine));

    state.manager.create_tenant(index_name).unwrap();
    if let Some(settings) = settings {
        save_index_settings(&state, index_name, &settings);
    }
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    (search_router(state), collector)
}

fn make_recommend_doc(id: &str, title: &str, brand: &str, category: &str) -> Document {
    let mut fields = HashMap::new();
    fields.insert("name".to_string(), FieldValue::Text(title.to_string()));
    fields.insert("title".to_string(), FieldValue::Text(title.to_string()));
    fields.insert("brand".to_string(), FieldValue::Facet(brand.to_string()));
    fields.insert(
        "category".to_string(),
        FieldValue::Facet(category.to_string()),
    );
    Document {
        id: id.to_string(),
        fields,
    }
}

/// Construct an InsightEvent representing a conversion/purchase action.
fn make_conversion_event(
    user_token: &str,
    index_name: &str,
    object_id: &str,
    timestamp_ms: i64,
) -> InsightEvent {
    InsightEvent {
        event_type: "conversion".to_string(),
        event_subtype: None,
        event_name: "Product Purchased".to_string(),
        index: index_name.to_string(),
        user_token: user_token.to_string(),
        authenticated_user_token: None,
        query_id: None,
        object_ids: vec![object_id.to_string()],
        object_ids_alt: vec![],
        positions: Some(vec![1]),
        timestamp: Some(timestamp_ms),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

/// Construct an InsightEvent representing a purchase/conversion action with subtype='purchase'.
fn make_purchase_event(
    user_token: &str,
    index_name: &str,
    object_id: &str,
    timestamp_ms: i64,
) -> InsightEvent {
    InsightEvent {
        event_type: "conversion".to_string(),
        event_subtype: Some("purchase".to_string()),
        event_name: "Purchased Item".to_string(),
        index: index_name.to_string(),
        user_token: user_token.to_string(),
        authenticated_user_token: None,
        query_id: None,
        object_ids: vec![object_id.to_string()],
        object_ids_alt: vec![],
        positions: Some(vec![1]),
        timestamp: Some(timestamp_ms),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

/// Populate an index with 20 documents across four brands and multiple categories for recommendation model testing.
async fn seed_stage6_recommend_lifecycle(state: &Arc<AppState>, index_name: &str) {
    let docs = vec![
        make_recommend_doc("p01", "Hydration Bottle", "Nike", "Outdoor"),
        make_recommend_doc("p02", "Trail Boots", "Nike", "Footwear"),
        make_recommend_doc("p03", "Running Jacket", "Nike", "Apparel"),
        make_recommend_doc("p04", "Training Socks", "Nike", "Accessories"),
        make_recommend_doc("p05", "Marathon Cap", "Nike", "Accessories"),
        make_recommend_doc("p06", "Adidas Hoodie", "Adidas", "Apparel"),
        make_recommend_doc("p07", "Adidas Shorts", "Adidas", "Apparel"),
        make_recommend_doc("p08", "Adidas Shorts 2", "Adidas", "Apparel"),
        make_recommend_doc("p09", "Adidas Jacket", "Adidas", "Apparel"),
        make_recommend_doc("p10", "Adidas Backpack", "Adidas", "Accessories"),
        make_recommend_doc("p11", "Puma Polo", "Puma", "Apparel"),
        make_recommend_doc("p12", "Puma Shorts", "Puma", "Apparel"),
        make_recommend_doc("p13", "Puma Hoodie", "Puma", "Apparel"),
        make_recommend_doc("p14", "Puma Socks", "Puma", "Accessories"),
        make_recommend_doc("p15", "Puma Running Cap", "Puma", "Accessories"),
        make_recommend_doc("p16", "Reebok Mat", "Reebok", "Accessories"),
        make_recommend_doc("p17", "Reebok Belt", "Reebok", "Accessories"),
        make_recommend_doc("p18", "Reebok Gloves", "Reebok", "Accessories"),
        make_recommend_doc("p19", "Reebok Shoes", "Reebok", "Footwear"),
        make_recommend_doc("p20", "Reebok Top", "Reebok", "Apparel"),
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();
}

fn record_events(collector: &Arc<AnalyticsCollector>, events: Vec<InsightEvent>) {
    for event in events {
        collector.record_insight(event);
    }
    collector.flush_all();
}

fn with_re_ranking_disabled(mut params: serde_json::Value) -> serde_json::Value {
    if let serde_json::Value::Object(ref mut map) = params {
        map.insert(
            "enableReRanking".to_string(),
            serde_json::Value::Bool(false),
        );
    }
    params
}

/// Create a TempDir, router, analytics collector, and two-product index configured for personalization tests.
async fn setup_personalization_search_fixture() -> (TempDir, Router, Arc<AnalyticsCollector>, String)
{
    let tmp = TempDir::new().unwrap();
    let analytics_cfg = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(analytics_cfg.clone());
    let analytics_engine = Arc::new(AnalyticsQueryEngine::new(analytics_cfg));

    let state = make_basic_search_state_with_analytics(&tmp, Some(analytics_engine));
    let index_name = "products_personalization";
    state.manager.create_tenant(index_name).unwrap();
    state
        .manager
        .add_documents_sync(
            index_name,
            vec![
                make_brand_doc("adidas-1", "running shoe red limited", "Adidas"),
                make_brand_doc("nike-1", "running shoe", "Nike"),
            ],
        )
        .await
        .unwrap();

    let app = search_personalization_recommend_router(state);
    (tmp, app, collector, index_name.to_string())
}

async fn setup_recommend_lifecycle_fixture(
    index_name: &str,
) -> (TempDir, Router, Arc<AnalyticsCollector>) {
    let tmp = TempDir::new().unwrap();
    let analytics_cfg = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(analytics_cfg.clone());
    let analytics_engine = Arc::new(AnalyticsQueryEngine::new(analytics_cfg));
    let state = make_basic_search_state_with_analytics(&tmp, Some(analytics_engine));

    state.manager.create_tenant(index_name).unwrap();
    seed_stage6_recommend_lifecycle(&state, index_name).await;

    let app = search_personalization_recommend_router(state);
    (tmp, app, collector)
}

/// Set up a personalization strategy and record click events to build a user profile with Nike brand affinity.
async fn configure_nike_profile(
    app: &Router,
    collector: &Arc<AnalyticsCollector>,
    index_name: &str,
    user_token: &str,
) {
    let strategy_resp = post_json_uri(
        app,
        "/1/strategies/personalization",
        json!({
            "eventsScoring": [
                { "eventName": "Product Clicked", "eventType": "click", "score": 100 }
            ],
            "facetsScoring": [
                { "facetName": "brand", "score": 100 }
            ],
            "personalizationImpact": 100
        }),
    )
    .await;
    assert_eq!(strategy_resp.status(), StatusCode::OK);

    let now_ms = chrono::Utc::now().timestamp_millis();
    for _ in 0..4 {
        collector.record_insight(make_click_event(user_token, index_name, "nike-1", now_ms));
    }
    collector.record_insight(make_click_event(user_token, index_name, "adidas-1", now_ms));
    collector.flush_all();

    let profile_resp = get_search(app, &format!("/1/profiles/personalization/{user_token}")).await;
    assert_eq!(profile_resp.status(), StatusCode::OK);
    let profile_body = body_json(profile_resp).await;
    assert_eq!(profile_body["userToken"], user_token);
    assert!(
        profile_body["scores"]["brand"]["Nike"]
            .as_u64()
            .unwrap_or_default()
            > profile_body["scores"]["brand"]["Adidas"]
                .as_u64()
                .unwrap_or_default(),
        "fixture should produce stronger Nike affinity: {profile_body}"
    );
}

/// Verify that virtual replicas inherit personalization profiles from click events recorded on the primary index.
#[tokio::test]
async fn search_personalization_applies_on_virtual_replica_using_primary_events() {
    let (_tmp, app, collector, primary_index_name) = setup_personalization_search_fixture().await;
    let virtual_index_name = "products_personalization_virtual";

    let replica_settings_resp = post_json_uri(
        &app,
        &format!("/1/indexes/{primary_index_name}/settings"),
        json!({ "replicas": [format!("virtual({virtual_index_name})")] }),
    )
    .await;
    assert_eq!(replica_settings_resp.status(), StatusCode::OK);

    configure_nike_profile(&app, &collector, &primary_index_name, "user-123").await;

    let baseline_virtual_resp = post_search(
        &app,
        virtual_index_name,
        json!({ "query": "running shoe" }),
        Some("user-123"),
    )
    .await;
    assert_eq!(baseline_virtual_resp.status(), StatusCode::OK);
    let baseline_virtual = body_json(baseline_virtual_resp).await;
    assert_eq!(
        baseline_virtual["hits"][0]["objectID"], "adidas-1",
        "without personalization, virtual replica should keep natural ranking"
    );

    let personalized_virtual_resp = post_search(
        &app,
        virtual_index_name,
        json!({
            "query": "running shoe",
            "enablePersonalization": true
        }),
        Some("user-123"),
    )
    .await;
    assert_eq!(personalized_virtual_resp.status(), StatusCode::OK);
    let personalized_virtual = body_json(personalized_virtual_resp).await;
    assert_eq!(
        personalized_virtual["hits"][0]["objectID"], "nike-1",
        "virtual replica search should apply personalization derived from primary index events"
    );
}

/// Assert that explicitly setting enablePersonalization=false preserves the natural text-based ranking.
#[tokio::test]
async fn search_personalization_disabled_keeps_natural_rank() {
    let (_tmp, app, collector, index_name) = setup_personalization_search_fixture().await;
    configure_nike_profile(&app, &collector, &index_name, "user-123").await;

    let default_resp = post_search(
        &app,
        &index_name,
        with_re_ranking_disabled(json!({
            "query": "running shoe"
        })),
        Some("user-123"),
    )
    .await;
    assert_eq!(default_resp.status(), StatusCode::OK);
    let default_body = body_json(default_resp).await;
    assert_eq!(default_body["hits"][0]["objectID"], "adidas-1");

    let disabled_resp = post_search(
        &app,
        &index_name,
        with_re_ranking_disabled(json!({
            "query": "running shoe",
            "enablePersonalization": false
        })),
        Some("user-123"),
    )
    .await;
    assert_eq!(disabled_resp.status(), StatusCode::OK);
    let disabled_body = body_json(disabled_resp).await;
    assert_eq!(
        disabled_body["hits"][0]["objectID"], "adidas-1",
        "explicitly disabling personalization must keep natural ranking"
    );
}

/// Assert that personalizationFilters override the user profile's affinity scores.
#[tokio::test]
async fn search_personalization_filters_override_profile_affinity() {
    let (_tmp, app, collector, index_name) = setup_personalization_search_fixture().await;
    configure_nike_profile(&app, &collector, &index_name, "user-123").await;

    let personalized_resp = post_search(
        &app,
        &index_name,
        with_re_ranking_disabled(json!({
            "query": "running shoe",
            "enablePersonalization": true
        })),
        Some("user-123"),
    )
    .await;
    assert_eq!(personalized_resp.status(), StatusCode::OK);
    let personalized_body = body_json(personalized_resp).await;
    assert_eq!(personalized_body["hits"][0]["objectID"], "nike-1");

    let filters_override_resp = post_search(
        &app,
        &index_name,
        json!({
            "query": "running shoe",
            "enablePersonalization": true,
            "personalizationFilters": ["brand:Adidas"]
        }),
        Some("user-123"),
    )
    .await;
    assert_eq!(filters_override_resp.status(), StatusCode::OK);
    let filters_override_body = body_json(filters_override_resp).await;
    assert_eq!(
        filters_override_body["hits"][0]["objectID"], "adidas-1",
        "personalizationFilters must override profile affinities"
    );
}

/// Confirm that personalizationFilters boost facets even without a user token.
#[tokio::test]
async fn search_personalization_filters_apply_without_user_token() {
    let (_tmp, app, _collector, index_name) = setup_personalization_search_fixture().await;

    let strategy_resp = post_json_uri(
        &app,
        "/1/strategies/personalization",
        json!({
            "eventsScoring": [
                { "eventName": "Product Clicked", "eventType": "click", "score": 100 }
            ],
            "facetsScoring": [
                { "facetName": "brand", "score": 100 }
            ],
            "personalizationImpact": 100
        }),
    )
    .await;
    assert_eq!(strategy_resp.status(), StatusCode::OK);

    let baseline_resp =
        post_search(&app, &index_name, json!({ "query": "running shoe" }), None).await;
    assert_eq!(baseline_resp.status(), StatusCode::OK);
    let baseline_body = body_json(baseline_resp).await;
    assert_eq!(baseline_body["hits"][0]["objectID"], "adidas-1");

    let filters_resp = post_search(
        &app,
        &index_name,
        json!({
            "query": "running shoe",
            "enablePersonalization": true,
            "personalizationFilters": ["brand:Nike"]
        }),
        None,
    )
    .await;
    assert_eq!(filters_resp.status(), StatusCode::OK);
    let filters_body = body_json(filters_resp).await;
    assert_eq!(
        filters_body["hits"][0]["objectID"], "nike-1",
        "personalizationFilters should boost matching facets even without userToken"
    );
}

/// Verify that providing personalizationFilters bypasses profile affinity even if filters are invalid.
#[tokio::test]
async fn search_personalization_filters_invalid_still_override_profile_affinity() {
    let (_tmp, app, collector, index_name) = setup_personalization_search_fixture().await;
    configure_nike_profile(&app, &collector, &index_name, "user-123").await;

    let personalized_resp = post_search(
        &app,
        &index_name,
        json!({
            "query": "running shoe",
            "enablePersonalization": true
        }),
        Some("user-123"),
    )
    .await;
    assert_eq!(personalized_resp.status(), StatusCode::OK);
    let personalized_body = body_json(personalized_resp).await;
    assert_eq!(personalized_body["hits"][0]["objectID"], "nike-1");

    let filters_override_resp = post_search(
        &app,
        &index_name,
        with_re_ranking_disabled(json!({
            "query": "running shoe",
            "enablePersonalization": true,
            "personalizationFilters": ["not-a-valid-filter"]
        })),
        Some("user-123"),
    )
    .await;
    assert_eq!(filters_override_resp.status(), StatusCode::OK);
    let filters_override_body = body_json(filters_override_resp).await;
    assert_eq!(
        filters_override_body["hits"][0]["objectID"], "adidas-1",
        "presence of personalizationFilters must bypass profile fallback, even if filters are invalid"
    );
}

/// Assert that index-level enablePersonalization=false prevents personalization despite query enabling it.
#[tokio::test]
async fn search_personalization_index_setting_false_overrides_query_flag() {
    let (_tmp, app, collector, index_name) = setup_personalization_search_fixture().await;
    configure_nike_profile(&app, &collector, &index_name, "user-123").await;

    let settings_resp = post_json_uri(
        &app,
        &format!("/1/indexes/{index_name}/settings"),
        json!({ "enablePersonalization": false }),
    )
    .await;
    assert_eq!(settings_resp.status(), StatusCode::OK);

    let personalized_resp = post_search(
        &app,
        &index_name,
        with_re_ranking_disabled(json!({
            "query": "running shoe",
            "enablePersonalization": true
        })),
        Some("user-123"),
    )
    .await;
    assert_eq!(personalized_resp.status(), StatusCode::OK);
    let personalized_body = body_json(personalized_resp).await;
    assert_eq!(
        personalized_body["hits"][0]["objectID"], "adidas-1",
        "index-level enablePersonalization=false must disable personalization"
    );
}

/// Verify that toggling enablePersonalization at the index level controls whether personalization boost applies.
#[tokio::test]
async fn search_personalization_index_setting_toggle_controls_boost() {
    let (_tmp, app, collector, index_name) = setup_personalization_search_fixture().await;
    configure_nike_profile(&app, &collector, &index_name, "user-123").await;

    let settings_true_resp = post_json_uri(
        &app,
        &format!("/1/indexes/{index_name}/settings"),
        json!({ "enablePersonalization": true }),
    )
    .await;
    assert_eq!(settings_true_resp.status(), StatusCode::OK);

    let enabled_resp = post_search(
        &app,
        &index_name,
        with_re_ranking_disabled(json!({
            "query": "running shoe",
            "enablePersonalization": true
        })),
        Some("user-123"),
    )
    .await;
    assert_eq!(enabled_resp.status(), StatusCode::OK);
    let enabled_body = body_json(enabled_resp).await;
    assert_eq!(
        enabled_body["hits"][0]["objectID"], "nike-1",
        "index-level enablePersonalization=true should allow personalization boost"
    );

    let settings_false_resp = post_json_uri(
        &app,
        &format!("/1/indexes/{index_name}/settings"),
        json!({ "enablePersonalization": false }),
    )
    .await;
    assert_eq!(settings_false_resp.status(), StatusCode::OK);

    let disabled_resp = post_search(
        &app,
        &index_name,
        with_re_ranking_disabled(json!({
            "query": "running shoe",
            "enablePersonalization": true
        })),
        Some("user-123"),
    )
    .await;
    assert_eq!(disabled_resp.status(), StatusCode::OK);
    let disabled_body = body_json(disabled_resp).await;
    assert_eq!(
        disabled_body["hits"][0]["objectID"], "adidas-1",
        "after toggling to false, personalization boost must stop"
    );
}

/// Verify that personalization reranking applies before pagination, allowing boosted docs to appear on page 1.
#[tokio::test]
async fn search_personalization_applies_before_pagination_windowing() {
    let (_tmp, app, collector, index_name) = setup_personalization_search_fixture().await;
    configure_nike_profile(&app, &collector, &index_name, "user-123").await;

    let baseline_resp = post_search(
        &app,
        &index_name,
        json!({
            "query": "running shoe",
            "hitsPerPage": 1
        }),
        Some("user-123"),
    )
    .await;
    assert_eq!(baseline_resp.status(), StatusCode::OK);
    let baseline_body = body_json(baseline_resp).await;
    assert_eq!(baseline_body["hits"][0]["objectID"], "adidas-1");

    let personalized_resp = post_search(
        &app,
        &index_name,
        json!({
            "query": "running shoe",
            "enablePersonalization": true,
            "hitsPerPage": 1
        }),
        Some("user-123"),
    )
    .await;
    assert_eq!(personalized_resp.status(), StatusCode::OK);
    let personalized_body = body_json(personalized_resp).await;
    assert_eq!(
        personalized_body["hits"][0]["objectID"], "nike-1",
        "personalization must rerank before page windowing"
    );
}

/// Personalized-search computation and unrestricted user-token deletion must
/// share the profile ordering owner, and an admitted post-purge computation
/// must not recreate an empty token-bearing profile file.
#[tokio::test]
async fn personalized_search_and_full_delete_share_user_ordering() {
    use crate::handlers::insights::{delete_usertoken, GdprDeleteState};
    use axum::extract::{Path, State};
    use flapjack::personalization::{
        PersonalizationProfile, PersonalizationProfileStore, PersonalizationStrategy,
    };
    use std::future::Future;
    use std::task::Poll;

    let tmp = TempDir::new().unwrap();
    let analytics_config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(analytics_config.clone());
    let analytics_engine = Arc::new(AnalyticsQueryEngine::new(analytics_config));
    let state = make_basic_search_state_with_analytics(&tmp, Some(Arc::clone(&analytics_engine)));
    let index_name = "products_personalization_ordering";
    state.manager.create_tenant(index_name).unwrap();
    state
        .manager
        .add_documents_sync(
            index_name,
            vec![
                make_brand_doc("target-product", "target shoe", "Nike"),
                make_brand_doc("control-product", "control shoe", "Adidas"),
            ],
        )
        .await
        .unwrap();

    let strategy = PersonalizationStrategy {
        events_scoring: vec![flapjack::personalization::EventScoring {
            event_name: "Product Clicked".to_string(),
            event_type: "click".to_string(),
            score: 100,
        }],
        facets_scoring: vec![flapjack::personalization::FacetScoring {
            facet_name: "brand".to_string(),
            score: 100,
        }],
        personalization_impact: 100,
    };
    let _ = set_personalization_strategy(State(Arc::clone(&state)), axum::Json(strategy.clone()))
        .await
        .unwrap();

    let now_ms = chrono::Utc::now().timestamp_millis();
    collector.record_insight(make_click_event(
        "target-user",
        index_name,
        "target-product",
        now_ms,
    ));
    collector.record_insight(make_click_event(
        "control-user",
        index_name,
        "control-product",
        now_ms,
    ));
    collector.flush_all();

    let store = PersonalizationProfileStore::new(tmp.path());
    let control = PersonalizationProfile {
        user_token: "control-user".to_string(),
        last_event_at: None,
        scores: BTreeMap::from([(
            "brand".to_string(),
            BTreeMap::from([("Adidas".to_string(), 20)]),
        )]),
    };
    store.save_profile(&control).unwrap();
    let probe = store.user_operation_probe_for_test("target-user").unwrap();

    let search_request = SearchRequest {
        enable_personalization: Some(true),
        user_token: Some("target-user".to_string()),
        ..Default::default()
    };
    let blocker = store.begin_user_operation("target-user").await.unwrap();
    let mut search_profile = Box::pin(resolve_personalization_context(
        &state,
        &search_request,
        None,
    ));
    let first_search_poll =
        std::future::poll_fn(|cx| Poll::Ready(search_profile.as_mut().poll(cx))).await;
    assert!(matches!(first_search_poll, Poll::Pending));
    assert_eq!(
        probe.admission_count(),
        2,
        "the personalized-search compute path must enter the profile operation owner"
    );
    drop(blocker);
    let computed = search_profile.await.unwrap();
    assert!(!computed.affinity_scores.is_empty());
    assert!(store.load_profile("target-user").unwrap().is_some());

    let blocker = store.begin_user_operation("target-user").await.unwrap();
    let gdpr_state = GdprDeleteState {
        analytics_collector: Arc::clone(&collector),
        profile_store_base_path: tmp.path().to_path_buf(),
        gdpr_notifier: None,
    };
    let mut full_delete = Box::pin(delete_usertoken(
        State(gdpr_state),
        Path("target-user".to_string()),
        None,
        None,
    ));
    let first_delete_poll =
        std::future::poll_fn(|cx| Poll::Ready(full_delete.as_mut().poll(cx))).await;
    assert!(matches!(first_delete_poll, Poll::Pending));
    assert_eq!(
        probe.admission_count(),
        4,
        "the unrestricted user-token DELETE must enter the profile operation owner"
    );
    let before_delete = analytics_engine
        .query_events(
            index_name,
            "SELECT user_token FROM events WHERE user_token = 'target-user'",
        )
        .await
        .unwrap();
    assert_eq!(before_delete.len(), 1, "purge ran before lock admission");
    drop(blocker);
    let _ = full_delete.await.unwrap();

    assert!(store.load_profile("target-user").unwrap().is_none());
    assert_eq!(store.load_profile("control-user").unwrap(), Some(control));
    let after_delete = analytics_engine
        .query_events(
            index_name,
            "SELECT user_token FROM events WHERE user_token = 'target-user'",
        )
        .await
        .unwrap();
    assert!(after_delete.is_empty());

    let after_purge = resolve_personalization_context(&state, &search_request, None).await;
    assert!(after_purge.is_none());
    assert!(
        store.load_profile("target-user").unwrap().is_none(),
        "post-purge empty computation must not recreate a token-bearing profile file"
    );
}

/// Test the complete personalization workflow: set strategy, build profile via clicks, verify ranking changes, and validate profile deletion.
#[tokio::test]
async fn stage6_personalization_lifecycle_d2() {
    let (_tmp, app, collector, index_name) = setup_personalization_search_fixture().await;

    configure_nike_profile(&app, &collector, &index_name, "user-123").await;

    let baseline_resp = post_search(
        &app,
        &index_name,
        with_re_ranking_disabled(json!({
            "query": "running shoe"
        })),
        Some("user-123"),
    )
    .await;
    assert_eq!(baseline_resp.status(), StatusCode::OK);
    let baseline = body_json(baseline_resp).await;
    assert_eq!(
        baseline["hits"][0]["objectID"], "adidas-1",
        "without personalization Adidas should rank first"
    );

    let profile_resp = get_search(&app, "/1/profiles/personalization/user-123").await;
    assert_eq!(profile_resp.status(), StatusCode::OK);
    let profile = body_json(profile_resp).await;
    assert_eq!(profile["userToken"], "user-123");
    assert!(
        profile["scores"]["brand"]["Nike"]
            .as_u64()
            .unwrap_or_default()
            > profile["scores"]["brand"]["Adidas"]
                .as_u64()
                .unwrap_or_default(),
    );

    let personalized_resp = post_search(
        &app,
        &index_name,
        with_re_ranking_disabled(json!({
            "query": "running shoe",
            "enablePersonalization": true
        })),
        Some("user-123"),
    )
    .await;
    assert_eq!(personalized_resp.status(), StatusCode::OK);
    let personalized_body = body_json(personalized_resp).await;
    assert_eq!(personalized_body["hits"][0]["objectID"], "nike-1");

    let (delete_status, delete_body) = delete_user_profile_request(&app, "user-123").await;
    assert_eq!(delete_status, StatusCode::OK);
    assert_eq!(delete_body["userToken"], "user-123");

    let deleted_profile_resp = get_search(&app, "/1/profiles/personalization/user-123").await;
    assert_eq!(deleted_profile_resp.status(), StatusCode::OK);
}

/// Test end-to-end recommendations: verify trending-items, related-products, bought-together, and trending-facets models with recorded events.
#[tokio::test]
async fn stage6_recommend_lifecycle_d3() {
    let index_name = "recommend_lifecycle";
    let (_tmp, app, collector) = setup_recommend_lifecycle_fixture(index_name).await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    record_events(
        &collector,
        vec![
            make_conversion_event("user-a", index_name, "p01", now_ms),
            make_conversion_event("user-a", index_name, "p01", now_ms + 1),
            make_conversion_event("user-a", index_name, "p02", now_ms + 2),
            make_conversion_event("user-a", index_name, "p03", now_ms + 3),
            make_conversion_event("user-a", index_name, "p04", now_ms + 4),
            make_conversion_event("user-a", index_name, "p05", now_ms + 5),
            make_conversion_event("user-b", index_name, "p01", now_ms + 6),
            make_conversion_event("user-b", index_name, "p02", now_ms + 7),
            make_conversion_event("user-b", index_name, "p03", now_ms + 8),
            make_conversion_event("user-b", index_name, "p06", now_ms + 9),
            make_conversion_event("user-b", index_name, "p07", now_ms + 10),
            make_conversion_event("user-c", index_name, "p01", now_ms + 11),
            make_conversion_event("user-c", index_name, "p02", now_ms + 12),
            make_conversion_event("user-c", index_name, "p08", now_ms + 13),
            make_conversion_event("user-c", index_name, "p09", now_ms + 14),
            make_conversion_event("user-c", index_name, "p10", now_ms + 15),
            make_conversion_event("user-d", index_name, "p01", now_ms + 16),
            make_conversion_event("user-d", index_name, "p02", now_ms + 17),
            make_conversion_event("user-d", index_name, "p06", now_ms + 18),
            make_conversion_event("user-d", index_name, "p07", now_ms + 19),
            make_conversion_event("user-d", index_name, "p08", now_ms + 20),
            make_conversion_event("user-e", index_name, "p06", now_ms + 21),
            make_conversion_event("user-e", index_name, "p07", now_ms + 22),
            make_conversion_event("user-e", index_name, "p08", now_ms + 23),
            make_conversion_event("user-e", index_name, "p09", now_ms + 24),
            make_conversion_event("user-e", index_name, "p03", now_ms + 25),
        ],
    );
    record_events(
        &collector,
        vec![
            make_purchase_event("user-a", index_name, "p01", now_ms + 101),
            make_purchase_event("user-a", index_name, "p16", now_ms + 102),
            make_purchase_event("user-b", index_name, "p01", now_ms + 103),
            make_purchase_event("user-b", index_name, "p16", now_ms + 104),
            make_purchase_event("user-c", index_name, "p01", now_ms + 105),
            make_purchase_event("user-c", index_name, "p16", now_ms + 106),
            make_purchase_event("user-d", index_name, "p01", now_ms + 107),
            make_purchase_event("user-d", index_name, "p17", now_ms + 108),
            make_purchase_event("user-e", index_name, "p01", now_ms + 109),
            make_purchase_event("user-e", index_name, "p17", now_ms + 110),
        ],
    );

    let (trending_status, trending_body) = post_recommendation_request(
        &app,
        json!({
            "requests": [{
                "indexName": index_name,
                "model": "trending-items",
                "threshold": 0,
                "maxRecommendations": 5
            }]
        }),
    )
    .await;
    assert_eq!(trending_status, StatusCode::OK);
    let trending_hits = trending_body["results"][0]["hits"]
        .as_array()
        .expect("trending-items should return hits array");
    assert_eq!(trending_hits[0]["objectID"], "p01");

    let (related_status, related_body) = post_recommendation_request(
        &app,
        json!({
            "requests": [{
                "indexName": index_name,
                "model": "related-products",
                "objectID": "p01",
                "threshold": 0,
                "maxRecommendations": 5
            }]
        }),
    )
    .await;
    assert_eq!(related_status, StatusCode::OK);
    let related_hits = related_body["results"][0]["hits"]
        .as_array()
        .expect("related-products should return hits array");
    assert_eq!(related_hits[0]["objectID"], "p02");
    let related_ids: Vec<&str> = related_hits
        .iter()
        .map(|hit| hit["objectID"].as_str().expect("objectID"))
        .collect();
    assert!(!related_ids.contains(&"p01"));

    let (bought_status, bought_body) = post_recommendation_request(
        &app,
        json!({
            "requests": [{
                "indexName": index_name,
                "model": "bought-together",
                "objectID": "p01",
                "threshold": 0,
                "maxRecommendations": 5
            }]
        }),
    )
    .await;
    assert_eq!(bought_status, StatusCode::OK);
    let bought_hits = bought_body["results"][0]["hits"]
        .as_array()
        .expect("bought-together should return hits array");
    assert_eq!(bought_hits[0]["objectID"], "p16");

    let (facet_status, facet_body) = post_recommendation_request(
        &app,
        json!({
            "requests": [{
                "indexName": index_name,
                "model": "trending-facets",
                "facetName": "brand",
                "threshold": 50,
                "maxRecommendations": 2
            }]
        }),
    )
    .await;
    assert_eq!(facet_status, StatusCode::OK);
    let facet_hits = facet_body["results"][0]["hits"]
        .as_array()
        .expect("trending-facets should return hits array");
    assert_eq!(facet_hits[0]["facetName"], "brand");
    assert_eq!(facet_hits[0]["facetValue"], "Nike");
    assert_eq!(facet_hits[1]["facetName"], "brand");
    assert_eq!(facet_hits[1]["facetValue"], "Adidas");
}

fn save_index_settings(
    state: &Arc<AppState>,
    index_name: &str,
    settings: &flapjack::index::settings::IndexSettings,
) {
    let as_json = serde_json::to_value(settings).unwrap();
    save_raw_settings_json(state, index_name, &as_json);
}

fn save_raw_settings_json(state: &Arc<AppState>, index_name: &str, settings: &Value) {
    let dir = state.manager.base_path.join(index_name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("settings.json"),
        serde_json::to_string_pretty(settings).unwrap(),
    )
    .unwrap();
    state.manager.invalidate_settings_cache(index_name);
}

fn save_rules_json(state: &Arc<AppState>, index_name: &str, rules: &Value) {
    let dir = state.manager.base_path.join(index_name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("rules.json"),
        serde_json::to_string_pretty(rules).unwrap(),
    )
    .unwrap();
    state.manager.invalidate_rules_cache(index_name);
}

/// Assert that facets_stats for numeric facets is included even when maxValuesPerFacet truncates values.
#[tokio::test]
async fn response_includes_numeric_facets_stats_even_when_facet_values_are_truncated() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "facet_stats_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        attributes_for_faceting: vec!["price".to_string(), "brand".to_string()],
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let mut d1 = HashMap::new();
    d1.insert(
        "title".to_string(),
        FieldValue::Text("item one".to_string()),
    );
    d1.insert("brand".to_string(), FieldValue::Text("Acme".to_string()));
    d1.insert("price".to_string(), FieldValue::Integer(1));

    let mut d2 = HashMap::new();
    d2.insert(
        "title".to_string(),
        FieldValue::Text("item two".to_string()),
    );
    d2.insert("brand".to_string(), FieldValue::Text("Acme".to_string()));
    d2.insert("price".to_string(), FieldValue::Integer(2));

    let mut d3 = HashMap::new();
    d3.insert(
        "title".to_string(),
        FieldValue::Text("item three".to_string()),
    );
    d3.insert("brand".to_string(), FieldValue::Text("Globex".to_string()));
    d3.insert("price".to_string(), FieldValue::Integer(3));

    state
        .manager
        .add_documents_sync(
            index_name,
            vec![
                Document {
                    id: "1".to_string(),
                    fields: d1,
                },
                Document {
                    id: "2".to_string(),
                    fields: d2,
                },
                Document {
                    id: "3".to_string(),
                    fields: d3,
                },
            ],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "",
            "facets": ["price", "brand"],
            "maxValuesPerFacet": 2
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    let facets_stats = body["facets_stats"]
        .as_object()
        .expect("facets_stats must be present when numeric facets are requested");
    assert!(
        facets_stats.get("brand").is_none(),
        "facets_stats must not include non-numeric facets"
    );
    let price_stats = facets_stats["price"]
        .as_object()
        .expect("price must include stats object");
    assert_eq!(price_stats["min"].as_f64(), Some(1.0));
    assert_eq!(price_stats["max"].as_f64(), Some(3.0));
    assert_eq!(price_stats["avg"].as_f64(), Some(2.0));
    assert_eq!(price_stats["sum"].as_f64(), Some(6.0));
}

/// Assert that facets_stats is returned as an empty object when requested but the search returns no matching documents.
#[tokio::test]
async fn response_returns_empty_facets_stats_when_there_are_no_hits() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "facet_stats_empty_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        attributes_for_faceting: vec!["price".to_string()],
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let mut fields = HashMap::new();
    fields.insert("title".to_string(), FieldValue::Text("item".to_string()));
    fields.insert("price".to_string(), FieldValue::Integer(10));
    state
        .manager
        .add_documents_sync(
            index_name,
            vec![Document {
                id: "1".to_string(),
                fields,
            }],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "does-not-exist",
            "facets": ["price"]
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body.get("facets_stats"),
        Some(&json!({})),
        "facets_stats must be an empty object when facets are requested but no numeric stats exist"
    );
}

/// Assert that enableReRanking parameter is echoed in the response params string.
#[tokio::test]
async fn enable_re_ranking_echoed_in_params() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(&state, "rerank_echo_idx", vec![vec![("title", "shoe")]]).await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        "rerank_echo_idx",
        json!({
            "query": "shoe",
            "enableReRanking": true
        }),
        None,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let params = body["params"].as_str().unwrap_or("");
    assert!(
        params.contains("enableReRanking=true"),
        "enableReRanking should echo in params, got: {params}"
    );
}

/// Verify that enableReRanking produces identical results to disabled re-ranking when there is no click data.
#[tokio::test]
async fn enable_re_ranking_no_op_identical_results() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "rerank_noop_idx",
        vec![
            vec![("title", "shoe one")],
            vec![("title", "shoe two")],
            vec![("title", "shoe three")],
        ],
    )
    .await;
    let app = search_router(state);

    let enabled_resp = post_search(
        &app,
        "rerank_noop_idx",
        json!({
            "query": "shoe",
            "enableReRanking": true
        }),
        None,
    )
    .await;
    assert_eq!(enabled_resp.status(), StatusCode::OK);
    let enabled_body = body_json(enabled_resp).await;

    let disabled_resp = post_search(
        &app,
        "rerank_noop_idx",
        json!({
            "query": "shoe",
            "enableReRanking": false
        }),
        None,
    )
    .await;
    assert_eq!(disabled_resp.status(), StatusCode::OK);
    let disabled_body = body_json(disabled_resp).await;

    assert_eq!(
        hit_ids(&enabled_body),
        hit_ids(&disabled_body),
        "enableReRanking must be a no-op for ranking behavior"
    );
}

/// Verify that enableReRanking=true activates re-ranking and promotes documents with click history.
#[tokio::test]
async fn enable_re_ranking_sentinel_promotes_clicked_doc() {
    let tmp = TempDir::new().unwrap();
    let index_name = "rerank_ctr_sentinel_idx";
    let (app, collector) = setup_rerank_search_fixture(
        &tmp,
        index_name,
        vec![
            make_brand_doc("adidas-1", "running shoe red limited", "Adidas"),
            make_brand_doc("nike-1", "running shoe", "Nike"),
        ],
        None,
    )
    .await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    record_click_events(&collector, "user-rerank", index_name, "nike-1", 6, now_ms);
    collector.record_insight(make_click_event(
        "user-rerank",
        index_name,
        "adidas-1",
        now_ms,
    ));
    collector.flush_all();

    let baseline_resp = post_search(
        &app,
        index_name,
        json!({
            "query": "running shoe",
            "enableReRanking": false
        }),
        None,
    )
    .await;
    assert_eq!(baseline_resp.status(), StatusCode::OK);
    let baseline_body = body_json(baseline_resp).await;
    assert_eq!(
        baseline_body["hits"][0]["objectID"].as_str(),
        Some("adidas-1"),
        "baseline ranking should remain text-first before reranking signal is applied"
    );

    let reranked_resp = post_search(
        &app,
        index_name,
        json!({
            "query": "running shoe",
            "enableReRanking": true
        }),
        None,
    )
    .await;
    assert_eq!(reranked_resp.status(), StatusCode::OK);
    let reranked_body = body_json(reranked_resp).await;
    assert_eq!(
        reranked_body["hits"][0]["objectID"].as_str(),
        Some("nike-1"),
        "enableReRanking sentinel: click-heavy doc should be promoted when reranking is enabled"
    );
}

/// Confirm that re-ranking operates correctly in current_thread runtime.
#[tokio::test(flavor = "current_thread")]
async fn enable_re_ranking_current_thread_runtime_does_not_panic() {
    let tmp = TempDir::new().unwrap();
    let index_name = "rerank_current_thread_idx";
    let (app, collector) = setup_rerank_search_fixture(
        &tmp,
        index_name,
        vec![
            make_brand_doc("adidas-1", "running shoe red limited", "Adidas"),
            make_brand_doc("nike-1", "running shoe", "Nike"),
        ],
        None,
    )
    .await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    record_click_events(&collector, "user-rerank", index_name, "nike-1", 4, now_ms);
    collector.flush_all();

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "running shoe",
            "enableReRanking": true
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Confirm that reRankingApplyFilter restricts re-ranking to documents matching the filter.
#[tokio::test]
async fn re_ranking_apply_filter_reranks_only_matching_subset() {
    let tmp = TempDir::new().unwrap();
    let index_name = "rerank_apply_filter_idx";
    let (app, collector) = setup_rerank_search_fixture(
        &tmp,
        index_name,
        vec![
            make_brand_doc("adidas-1", "running shoe red limited", "Adidas"),
            make_brand_doc("nike-1", "running shoe", "Nike"),
            make_brand_doc("nike-2", "running shoe blue", "Nike"),
            make_brand_doc("puma-1", "running shoe green", "Puma"),
        ],
        None,
    )
    .await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    record_click_events(&collector, "user-rerank", index_name, "nike-2", 8, now_ms);
    record_click_events(&collector, "user-rerank", index_name, "puma-1", 10, now_ms);
    collector.flush_all();

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "running shoe",
            "enableReRanking": true,
            "reRankingApplyFilter": "brand:Nike"
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let ids = hit_ids(&body);

    assert_eq!(ids[0], "nike-2");
    assert_eq!(ids[1], "nike-1");
    assert!(
        ids.iter().position(|id| id == "adidas-1").unwrap()
            < ids.iter().position(|id| id == "puma-1").unwrap(),
        "non-matching subset should keep original ordering"
    );
}

/// Confirm that index-level enable_re_ranking setting applies when the query omits enableReRanking.
#[tokio::test]
async fn enable_re_ranking_uses_index_setting_when_query_param_absent() {
    let tmp = TempDir::new().unwrap();
    let index_name = "rerank_setting_idx";
    let (app, collector) = setup_rerank_search_fixture(
        &tmp,
        index_name,
        vec![
            make_brand_doc("adidas-1", "running shoe red limited", "Adidas"),
            make_brand_doc("nike-1", "running shoe", "Nike"),
        ],
        Some(flapjack::index::settings::IndexSettings {
            enable_re_ranking: Some(true),
            ..Default::default()
        }),
    )
    .await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    record_click_events(&collector, "user-rerank", index_name, "nike-1", 6, now_ms);
    collector.flush_all();

    let resp = post_search(&app, index_name, json!({ "query": "running shoe" }), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["hits"][0]["objectID"].as_str(), Some("nike-1"));
}

/// Verify that query parameter enableReRanking overrides the index-level enable_re_ranking setting.
#[tokio::test]
async fn enable_re_ranking_query_param_overrides_index_setting() {
    let tmp = TempDir::new().unwrap();
    let index_name = "rerank_setting_override_idx";
    let (app, collector) = setup_rerank_search_fixture(
        &tmp,
        index_name,
        vec![
            make_brand_doc("adidas-1", "running shoe red limited", "Adidas"),
            make_brand_doc("nike-1", "running shoe", "Nike"),
        ],
        Some(flapjack::index::settings::IndexSettings {
            enable_re_ranking: Some(true),
            ..Default::default()
        }),
    )
    .await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    record_click_events(&collector, "user-rerank", index_name, "nike-1", 6, now_ms);
    collector.flush_all();

    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "running shoe",
            "enableReRanking": false
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["hits"][0]["objectID"].as_str(), Some("adidas-1"));
}

/// Assert that relevancyStrictness filtering occurs before re-ranking, preventing ineligible documents from being promoted.
#[tokio::test]
async fn relevancy_strictness_stacks_before_reranking() {
    let tmp = TempDir::new().unwrap();
    let index_name = "rerank_relevancy_strictness_idx";
    let (app, collector) = setup_rerank_search_fixture(
        &tmp,
        index_name,
        vec![
            make_brand_doc("adidas-1", "running shoe red limited", "Adidas"),
            make_brand_doc("nike-1", "running shoe", "Nike"),
        ],
        None,
    )
    .await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    record_click_events(&collector, "user-rerank", index_name, "nike-1", 6, now_ms);
    collector.flush_all();

    let baseline_resp = post_search(
        &app,
        index_name,
        json!({
            "query": "running shoe",
            "relevancyStrictness": 100,
            "enableReRanking": false
        }),
        None,
    )
    .await;
    assert_eq!(baseline_resp.status(), StatusCode::OK);
    let baseline_body = body_json(baseline_resp).await;
    assert_eq!(
        baseline_body["hits"][0]["objectID"].as_str(),
        Some("adidas-1")
    );

    let reranked_resp = post_search(
        &app,
        index_name,
        json!({
            "query": "running shoe",
            "relevancyStrictness": 100,
            "enableReRanking": true
        }),
        None,
    )
    .await;
    assert_eq!(reranked_resp.status(), StatusCode::OK);
    let reranked_body = body_json(reranked_resp).await;
    assert_eq!(
        reranked_body["hits"][0]["objectID"].as_str(),
        Some("adidas-1")
    );

    let ctr_dominant_resp = post_search(
        &app,
        index_name,
        json!({
            "query": "running shoe",
            "relevancyStrictness": 0,
            "enableReRanking": true
        }),
        None,
    )
    .await;
    assert_eq!(ctr_dominant_resp.status(), StatusCode::OK);
    let ctr_dominant_body = body_json(ctr_dominant_resp).await;
    assert_eq!(
        ctr_dominant_body["hits"][0]["objectID"].as_str(),
        Some("nike-1")
    );
}

/// Confirm that parsedQuery echoes the original search query when no preprocessing modifies it.
#[tokio::test]
async fn response_includes_parsed_query_when_no_preprocessing_applies() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "parsed_query_passthrough_idx";

    state.manager.create_tenant(index_name).unwrap();
    let mut fields = HashMap::new();
    fields.insert("title".to_string(), FieldValue::Text("shoe".to_string()));
    state
        .manager
        .add_documents_sync(
            index_name,
            vec![Document {
                id: "1".to_string(),
                fields,
            }],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(&app, index_name, json!({ "query": "shoe" }), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["parsedQuery"].as_str(),
        Some("shoe"),
        "parsedQuery must echo the search query when no preprocessing changes it"
    );
}

/// Confirm that parsedQuery reflects the query text after stopword removal.
#[tokio::test]
async fn response_parsed_query_reflects_stop_word_removal() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "parsed_query_stopwords_idx";

    state.manager.create_tenant(index_name).unwrap();
    let mut fields = HashMap::new();
    fields.insert(
        "title".to_string(),
        FieldValue::Text("best search engine".to_string()),
    );
    state
        .manager
        .add_documents_sync(
            index_name,
            vec![Document {
                id: "1".to_string(),
                fields,
            }],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "the best search",
            "removeStopWords": true
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["parsedQuery"].as_str(),
        Some("best search"),
        "parsedQuery must contain the post-stopword-removal query text"
    );
}

/// Verify that naturalLanguages parameter activates language-specific stopword removal.
#[tokio::test]
async fn natural_languages_activates_stop_word_removal() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "natural_languages_stopword_idx",
        vec![vec![("title", "la recherche moteur")]],
    )
    .await;
    let app = search_router(state);

    let with_natural_language = post_search(
        &app,
        "natural_languages_stopword_idx",
        json!({
            "query": "le recherche",
            "removeStopWords": true,
            "naturalLanguages": ["fr"]
        }),
        None,
    )
    .await;
    assert_eq!(with_natural_language.status(), StatusCode::OK);
    let with_natural_body = body_json(with_natural_language).await;
    assert_eq!(
        with_natural_body["parsedQuery"].as_str(),
        Some("recherche"),
        "naturalLanguages should drive French stopword removal"
    );

    let without_natural_language = post_search(
        &app,
        "natural_languages_stopword_idx",
        json!({
            "query": "le recherche",
            "removeStopWords": true
        }),
        None,
    )
    .await;
    assert_eq!(without_natural_language.status(), StatusCode::OK);
    let without_natural_body = body_json(without_natural_language).await;
    assert_eq!(
        without_natural_body["parsedQuery"].as_str(),
        Some("le recherche"),
        "without naturalLanguages, French stopwords should not be removed by default"
    );
}

/// Confirm that queryLanguages takes precedence over naturalLanguages for stopword removal.
#[tokio::test]
async fn query_languages_takes_precedence_over_natural_languages() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "natural_languages_precedence_idx",
        vec![vec![("title", "search engine")]],
    )
    .await;
    let app = search_router(state);

    let the_recherche = post_search(
        &app,
        "natural_languages_precedence_idx",
        json!({
            "query": "the recherche",
            "removeStopWords": true,
            "queryLanguages": ["en"],
            "naturalLanguages": ["fr"]
        }),
        None,
    )
    .await;
    assert_eq!(the_recherche.status(), StatusCode::OK);
    let the_recherche_body = body_json(the_recherche).await;
    assert_eq!(
        the_recherche_body["parsedQuery"].as_str(),
        Some("recherche"),
        "queryLanguages should take precedence and remove English stopwords"
    );

    let le_search = post_search(
        &app,
        "natural_languages_precedence_idx",
        json!({
            "query": "le search",
            "removeStopWords": true,
            "queryLanguages": ["en"],
            "naturalLanguages": ["fr"]
        }),
        None,
    )
    .await;
    assert_eq!(le_search.status(), StatusCode::OK);
    let le_search_body = body_json(le_search).await;
    assert_eq!(
        le_search_body["parsedQuery"].as_str(),
        Some("le search"),
        "with queryLanguages=[en], French stopword 'le' should remain"
    );
}

/// Verify that naturalLanguages drives language-specific stopword removal when removeStopWords is enabled at the index level.
#[tokio::test]
async fn natural_languages_with_settings_level_stop_words() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "settings_level_stopwords_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        remove_stop_words: flapjack::query::stopwords::RemoveStopWordsValue::All,
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let doc = Document {
        id: "1".to_string(),
        fields: HashMap::from([(
            "title".to_string(),
            FieldValue::Text("le chat noir".to_string()),
        )]),
    };
    state
        .manager
        .add_documents_sync(index_name, vec![doc])
        .await
        .unwrap();

    let app = search_router(state);

    let with_natural_languages = post_search(
        &app,
        index_name,
        json!({
            "query": "le chat",
            "naturalLanguages": ["fr"]
        }),
        None,
    )
    .await;
    assert_eq!(with_natural_languages.status(), StatusCode::OK);
    let with_nl_body = body_json(with_natural_languages).await;
    assert_eq!(
        with_nl_body["parsedQuery"].as_str(),
        Some("chat"),
        "naturalLanguages should drive French stopword removal with settings-level removeStopWords"
    );

    let without_natural_languages = post_search(
        &app,
        index_name,
        json!({
            "query": "le chat"
        }),
        None,
    )
    .await;
    assert_eq!(without_natural_languages.status(), StatusCode::OK);
    let without_nl_body = body_json(without_natural_languages).await;
    assert_eq!(
        without_nl_body["parsedQuery"].as_str(),
        Some("le chat"),
        "without naturalLanguages, 'le' should remain (English default doesn't strip French stopwords)"
    );
}

/// Verify that naturalLanguages from params string maps to queryLanguages when queryLanguages is absent.
#[tokio::test]
async fn natural_languages_fallback_sentinel_from_params_string() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "natural_languages_params_sentinel_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        remove_stop_words: flapjack::query::stopwords::RemoveStopWordsValue::All,
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let doc = Document {
        id: "1".to_string(),
        fields: HashMap::from([(
            "title".to_string(),
            FieldValue::Text("le chat noir".to_string()),
        )]),
    };
    state
        .manager
        .add_documents_sync(index_name, vec![doc])
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(
        &app,
        index_name,
        json!({
            "params": "query=le%20chat&removeStopWords=true&naturalLanguages=%5B%22fr%22%5D"
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["parsedQuery"].as_str(),
        Some("chat"),
        "naturalLanguages from params string should map to queryLanguages when queryLanguages is absent"
    );
}

/// Verify that naturalLanguages enables language-specific plural expansion matching (e.g., French journal→journaux).
#[tokio::test]
async fn natural_languages_drives_ignore_plurals_french_irregular_pair() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "natural_languages_ignore_plurals_fr_idx";

    create_index_with_docs(&state, index_name, vec![vec![("title", "journaux")]]).await;
    let app = search_router(state);

    let without_natural_languages = post_search(
        &app,
        index_name,
        json!({
            "query": "journal",
            "ignorePlurals": true,
            "typoTolerance": false
        }),
        None,
    )
    .await;
    assert_eq!(without_natural_languages.status(), StatusCode::OK);
    let without_natural_languages_body = body_json(without_natural_languages).await;
    assert!(
        hit_ids(&without_natural_languages_body).is_empty(),
        "without naturalLanguages/queryLanguages fallback, English plural expansion should not match 'journaux'"
    );

    let with_natural_languages = post_search(
        &app,
        index_name,
        json!({
            "query": "journal",
            "ignorePlurals": true,
            "typoTolerance": false,
            "naturalLanguages": ["fr"]
        }),
        None,
    )
    .await;
    assert_eq!(with_natural_languages.status(), StatusCode::OK);
    let with_natural_languages_body = body_json(with_natural_languages).await;
    assert_eq!(
        hit_ids(&with_natural_languages_body),
        vec![format!("doc_{}_0", index_name)],
        "naturalLanguages fallback should drive French plural expansion (journal -> journaux)"
    );
}

/// Assert that the exhaustive object contains all required fields: nbHits, typo, facetsCount, facetValues, and rulesMatch.
#[tokio::test]
async fn response_exhaustive_object_includes_required_subfields() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "exhaustive_required_fields_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        attributes_for_faceting: vec!["category".to_string()],
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let mut d1 = HashMap::new();
    d1.insert(
        "title".to_string(),
        FieldValue::Text("red shoe".to_string()),
    );
    d1.insert(
        "category".to_string(),
        FieldValue::Text("footwear".to_string()),
    );

    let mut d2 = HashMap::new();
    d2.insert(
        "title".to_string(),
        FieldValue::Text("blue hat".to_string()),
    );
    d2.insert(
        "category".to_string(),
        FieldValue::Text("headwear".to_string()),
    );

    state
        .manager
        .add_documents_sync(
            index_name,
            vec![
                Document {
                    id: "1".to_string(),
                    fields: d1,
                },
                Document {
                    id: "2".to_string(),
                    fields: d2,
                },
            ],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "",
            "facets": ["category"]
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let exhaustive = body["exhaustive"]
        .as_object()
        .expect("exhaustive must be an object");

    assert_eq!(
        exhaustive.get("nbHits").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(exhaustive.get("typo").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        exhaustive.get("facetsCount").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        exhaustive.get("facetValues").and_then(|v| v.as_bool()),
        Some(true),
        "facetValues should be true when facet values are complete"
    );
    assert_eq!(
        exhaustive.get("rulesMatch").and_then(|v| v.as_bool()),
        Some(true),
        "rulesMatch should be true when rules evaluation is exhaustive"
    );
}

/// Assert that exhaustive.facetValues is false when maxValuesPerFacet truncates the facet list.
#[tokio::test]
async fn response_exhaustive_facet_values_false_when_facet_values_are_truncated() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "exhaustive_facet_values_truncated_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        attributes_for_faceting: vec!["category".to_string()],
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let mut d1 = HashMap::new();
    d1.insert(
        "title".to_string(),
        FieldValue::Text("item one".to_string()),
    );
    d1.insert("category".to_string(), FieldValue::Text("c1".to_string()));

    let mut d2 = HashMap::new();
    d2.insert(
        "title".to_string(),
        FieldValue::Text("item two".to_string()),
    );
    d2.insert("category".to_string(), FieldValue::Text("c2".to_string()));

    let mut d3 = HashMap::new();
    d3.insert(
        "title".to_string(),
        FieldValue::Text("item three".to_string()),
    );
    d3.insert("category".to_string(), FieldValue::Text("c3".to_string()));

    state
        .manager
        .add_documents_sync(
            index_name,
            vec![
                Document {
                    id: "1".to_string(),
                    fields: d1,
                },
                Document {
                    id: "2".to_string(),
                    fields: d2,
                },
                Document {
                    id: "3".to_string(),
                    fields: d3,
                },
            ],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(
        &app,
        index_name,
        json!({
            "query": "",
            "facets": ["category"],
            "maxValuesPerFacet": 2
        }),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(
        body["exhaustive"]["facetValues"].as_bool(),
        Some(false),
        "facetValues must be false when facet values are truncated by maxValuesPerFacet"
    );
    assert_eq!(
        body["exhaustive"]["rulesMatch"].as_bool(),
        Some(true),
        "rulesMatch should still be true when rules are evaluated without timeout"
    );
}

/// Assert that serverUsed is a non-empty string and _automaticInsights is present as a boolean.
#[tokio::test]
async fn response_includes_server_used_and_automatic_insights() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "metadata_fields_idx";

    state.manager.create_tenant(index_name).unwrap();

    let mut d1 = HashMap::new();
    d1.insert("title".to_string(), FieldValue::Text("hello".to_string()));

    state
        .manager
        .add_documents_sync(
            index_name,
            vec![Document {
                id: "1".to_string(),
                fields: d1,
            }],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(&app, index_name, json!({"query": ""}), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    // serverUsed must be a non-empty string
    let server_used = body["serverUsed"]
        .as_str()
        .expect("serverUsed must be a string");
    assert!(
        !server_used.is_empty(),
        "serverUsed must be a non-empty string"
    );

    // _automaticInsights must be a boolean (false for now)
    assert_eq!(
        body["_automaticInsights"].as_bool(),
        Some(false),
        "_automaticInsights must be false"
    );
}

/// Assert that userData from index settings is included in the search response.
#[tokio::test]
async fn response_includes_settings_user_data_metadata() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "settings_user_data_idx";

    state.manager.create_tenant(index_name).unwrap();
    save_raw_settings_json(
        &state,
        index_name,
        &json!({
            "userData": {
                "custom": "data"
            }
        }),
    );

    let mut fields = HashMap::new();
    fields.insert("title".to_string(), FieldValue::Text("hello".to_string()));
    state
        .manager
        .add_documents_sync(
            index_name,
            vec![Document {
                id: "1".to_string(),
                fields,
            }],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(&app, index_name, json!({"query": ""}), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(body["userData"], json!({"custom": "data"}));
}

/// Verify that AI provider API keys in userData are redacted to '<redacted>' while preserving other fields.
#[tokio::test]
async fn response_redacts_ai_provider_api_key_in_settings_user_data() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "settings_user_data_redacted_idx";

    state.manager.create_tenant(index_name).unwrap();
    save_raw_settings_json(
        &state,
        index_name,
        &json!({
            "userData": {
                "aiProvider": {
                    "baseUrl": "https://example.test/v1",
                    "apiKey": "super-secret-key"
                },
                "custom": "data"
            }
        }),
    );

    let mut fields = HashMap::new();
    fields.insert("title".to_string(), FieldValue::Text("hello".to_string()));
    state
        .manager
        .add_documents_sync(
            index_name,
            vec![Document {
                id: "1".to_string(),
                fields,
            }],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(&app, index_name, json!({"query": ""}), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(
        body["userData"]["aiProvider"]["apiKey"],
        json!("<redacted>")
    );
    assert_eq!(
        body["userData"]["aiProvider"]["baseUrl"],
        json!("https://example.test/v1")
    );
    assert_eq!(body["userData"]["custom"], json!("data"));
}

/// Enforce that pagination respects paginationLimitedTo, returning empty hits array when exceeded.
#[tokio::test]
async fn response_enforces_pagination_limited_to_from_settings() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "pagination_limited_to_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        pagination_limited_to: 50,
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let docs: Vec<Document> = (0..120)
        .map(|i| {
            let mut fields = HashMap::new();
            fields.insert(
                "title".to_string(),
                FieldValue::Text(format!("pagination doc {i}")),
            );
            Document {
                id: format!("doc_{i}"),
                fields,
            }
        })
        .collect();
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let app = search_router(state);

    let allowed = post_search(
        &app,
        index_name,
        json!({"query": "pagination", "page": 1, "hitsPerPage": 20}),
        None,
    )
    .await;
    assert_eq!(allowed.status(), StatusCode::OK);
    let allowed_body = body_json(allowed).await;
    assert_eq!(allowed_body["nbHits"].as_u64(), Some(120));
    assert_eq!(
        allowed_body["hits"].as_array().map(|hits| hits.len()),
        Some(20)
    );

    let boundary = post_search(
        &app,
        index_name,
        json!({"query": "pagination", "page": 2, "hitsPerPage": 25}),
        None,
    )
    .await;
    assert_eq!(boundary.status(), StatusCode::OK);
    let boundary_body = body_json(boundary).await;
    assert_eq!(
        boundary_body["hits"].as_array().map(|hits| hits.len()),
        Some(0),
        "hits must be empty when page*hitsPerPage reaches paginationLimitedTo"
    );

    let blocked = post_search(
        &app,
        index_name,
        json!({"query": "pagination", "page": 5, "hitsPerPage": 20}),
        None,
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::OK);
    let blocked_body = body_json(blocked).await;
    assert_eq!(
        blocked_body["nbHits"].as_u64(),
        Some(120),
        "nbHits must still reflect total matching hits when pagination limit blocks hits"
    );
    assert_eq!(
        blocked_body["hits"].as_array().map(|hits| hits.len()),
        Some(0),
        "hits must be empty when page*hitsPerPage exceeds paginationLimitedTo"
    );
}

/// An omitted request window uses the saved hitsPerPage value, while an explicit
/// request value remains authoritative.
#[tokio::test]
async fn response_uses_saved_hits_per_page_when_request_omits_it() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "saved_hits_per_page_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        hits_per_page: 2,
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);
    state
        .manager
        .add_documents_sync(
            index_name,
            (1..=5)
                .map(|number| Document {
                    id: format!("pbv2-trail-{number:03}"),
                    fields: HashMap::from([(
                        "title".to_string(),
                        FieldValue::Text(format!("trail jacket {number}")),
                    )]),
                })
                .collect(),
        )
        .await
        .unwrap();

    let app = search_router(state.clone());
    let saved_default = post_search(&app, index_name, json!({"query": "trail"}), None).await;
    assert_eq!(saved_default.status(), StatusCode::OK);
    let saved_default = body_json(saved_default).await;
    assert_eq!(saved_default["hitsPerPage"], 2);
    assert_eq!(saved_default["nbPages"], 3);
    assert_eq!(saved_default["hits"].as_array().map(Vec::len), Some(2));

    let request_override = post_search(
        &app,
        index_name,
        json!({"query": "trail", "hitsPerPage": 3}),
        None,
    )
    .await;
    assert_eq!(request_override.status(), StatusCode::OK);
    let request_override = body_json(request_override).await;
    assert_eq!(request_override["hitsPerPage"], 3);
    assert_eq!(request_override["hits"].as_array().map(Vec::len), Some(3));

    save_index_settings(
        &state,
        index_name,
        &flapjack::index::settings::IndexSettings {
            searchable_attributes: Some(vec!["title".to_string()]),
            hits_per_page: 0,
            ..Default::default()
        },
    );
    let zero_default = post_search(&app, index_name, json!({"query": "trail"}), None).await;
    assert_eq!(zero_default.status(), StatusCode::OK);
    let zero_default = body_json(zero_default).await;
    assert_eq!(zero_default["hitsPerPage"], 0);
    assert_eq!(zero_default["hits"].as_array().map(Vec::len), Some(0));
}

/// Verify that unretrievableAttributes are hidden unless the API key has seeUnretrievableAttributes ACL.
#[tokio::test]
async fn response_unretrievable_attributes_respects_see_unretrievable_acl() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "unretrievable_acl_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "secret_field".to_string()]),
        unretrievable_attributes: Some(vec!["secret_field".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let mut fields = HashMap::new();
    fields.insert(
        "title".to_string(),
        FieldValue::Text("visible title".to_string()),
    );
    fields.insert(
        "secret_field".to_string(),
        FieldValue::Text("hidden value".to_string()),
    );
    state
        .manager
        .add_documents_sync(
            index_name,
            vec![Document {
                id: "doc1".to_string(),
                fields,
            }],
        )
        .await
        .unwrap();

    let app = search_router(state);

    let normal_resp = post_search(&app, index_name, json!({"query": "visible"}), None).await;
    assert_eq!(normal_resp.status(), StatusCode::OK);
    let normal_body = body_json(normal_resp).await;
    assert!(normal_body["hits"][0].get("secret_field").is_none());

    let wildcard_retrieve_resp = post_search(
        &app,
        index_name,
        json!({
            "query": "visible",
            "attributesToRetrieve": ["*"]
        }),
        None,
    )
    .await;
    assert_eq!(wildcard_retrieve_resp.status(), StatusCode::OK);
    let wildcard_retrieve_body = body_json(wildcard_retrieve_resp).await;
    assert!(
        wildcard_retrieve_body["hits"][0]
            .get("secret_field")
            .is_none(),
        "unretrievableAttributes must still hide secret_field even with attributesToRetrieve=[\"*\"]"
    );

    let explicit_retrieve_resp = post_search(
        &app,
        index_name,
        json!({
            "query": "visible",
            "attributesToRetrieve": ["secret_field", "title"]
        }),
        None,
    )
    .await;
    assert_eq!(explicit_retrieve_resp.status(), StatusCode::OK);
    let explicit_retrieve_body = body_json(explicit_retrieve_resp).await;
    assert!(
        explicit_retrieve_body["hits"][0]
            .get("secret_field")
            .is_none(),
        "unretrievableAttributes must still hide secret_field even when explicitly requested"
    );

    let highlight_and_snippet_resp = post_search(
        &app,
        index_name,
        json!({
            "query": "hidden",
            "attributesToSnippet": ["secret_field:10"]
        }),
        None,
    )
    .await;
    assert_eq!(highlight_and_snippet_resp.status(), StatusCode::OK);
    let highlight_and_snippet_body = body_json(highlight_and_snippet_resp).await;
    assert!(
        highlight_and_snippet_body["hits"][0]["_highlightResult"]
            .get("secret_field")
            .is_none(),
        "unretrievableAttributes must also hide secret_field in _highlightResult"
    );
    assert!(
        highlight_and_snippet_body["hits"][0]["_snippetResult"]
            .get("secret_field")
            .is_none(),
        "unretrievableAttributes must also hide secret_field in _snippetResult"
    );

    let mut privileged_req = Request::builder()
        .method(Method::POST)
        .uri(format!("/1/indexes/{index_name}/query"))
        .header("content-type", "application/json")
        .body(Body::from(json!({"query": "visible"}).to_string()))
        .unwrap();
    privileged_req.extensions_mut().insert(crate::auth::ApiKey {
        hash: "test-hash".to_string(),
        salt: "test-salt".to_string(),
        hmac_key: None,
        created_at: 0,
        acl: vec![
            "search".to_string(),
            "seeUnretrievableAttributes".to_string(),
        ],
        description: "test key".to_string(),
        indexes: Vec::new(),
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: Vec::new(),
        restrict_sources: None,
        validity: 0,
    });

    let privileged_resp = app.clone().oneshot(privileged_req).await.unwrap();
    assert_eq!(privileged_resp.status(), StatusCode::OK);
    let privileged_body = body_json(privileged_resp).await;
    assert_eq!(
        privileged_body["hits"][0]["secret_field"].as_str(),
        Some("hidden value")
    );
}

/// Confirm that index-level enableRules setting applies when the request omits an explicit enableRules flag.
#[tokio::test]
async fn response_uses_index_settings_enable_rules_when_request_omits_flag() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "settings_enable_rules_idx";

    state.manager.create_tenant(index_name).unwrap();
    save_raw_settings_json(
        &state,
        index_name,
        &json!({
            "enableRules": false
        }),
    );

    let mut promo = HashMap::new();
    promo.insert(
        "title".to_string(),
        FieldValue::Text("promoted laptop".to_string()),
    );
    let mut regular = HashMap::new();
    regular.insert(
        "title".to_string(),
        FieldValue::Text("regular laptop".to_string()),
    );
    state
        .manager
        .add_documents_sync(
            index_name,
            vec![
                Document {
                    id: "promo".to_string(),
                    fields: promo,
                },
                Document {
                    id: "regular".to_string(),
                    fields: regular,
                },
            ],
        )
        .await
        .unwrap();

    save_rules_json(
        &state,
        index_name,
        &json!([{
            "objectID": "pin-rule",
            "conditions": [{ "anchoring": "contains", "pattern": "laptop" }],
            "consequence": {
                "promote": [{ "objectID": "promo", "position": 0 }]
            }
        }]),
    );

    let app = search_router(state);

    let rules_enabled_resp = post_search(
        &app,
        index_name,
        json!({"query": "laptop", "enableRules": true}),
        None,
    )
    .await;
    assert_eq!(rules_enabled_resp.status(), StatusCode::OK);
    let rules_enabled_body = body_json(rules_enabled_resp).await;
    assert_eq!(
        rules_enabled_body["appliedRules"],
        json!([{ "objectID": "pin-rule" }])
    );

    let rules_disabled_by_settings =
        post_search(&app, index_name, json!({"query": "laptop"}), None).await;
    assert_eq!(rules_disabled_by_settings.status(), StatusCode::OK);
    let rules_disabled_body = body_json(rules_disabled_by_settings).await;
    assert!(
        rules_disabled_body.get("appliedRules").is_none(),
        "appliedRules must be absent when enableRules=false in settings and request omits enableRules"
    );
}

/// Assert that nbSortedHits field is present and equals nbHits when a sort parameter is applied.
#[tokio::test]
async fn response_includes_nb_sorted_hits_when_sort_active() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index_name = "nb_sorted_hits_idx";

    state.manager.create_tenant(index_name).unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, index_name, &settings);

    let mut d1 = HashMap::new();
    d1.insert("title".to_string(), FieldValue::Text("apple".to_string()));
    d1.insert("price".to_string(), FieldValue::Float(10.0));

    let mut d2 = HashMap::new();
    d2.insert("title".to_string(), FieldValue::Text("banana".to_string()));
    d2.insert("price".to_string(), FieldValue::Float(5.0));

    state
        .manager
        .add_documents_sync(
            index_name,
            vec![
                Document {
                    id: "1".to_string(),
                    fields: d1,
                },
                Document {
                    id: "2".to_string(),
                    fields: d2,
                },
            ],
        )
        .await
        .unwrap();

    let app = search_router(state);

    // With sort active, nbSortedHits should be present
    let resp = post_search(
        &app,
        index_name,
        json!({"query": "", "sort": ["price:asc"]}),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    let nb_sorted = body["nbSortedHits"]
        .as_u64()
        .expect("nbSortedHits must be present when sort is active");
    assert_eq!(
        nb_sorted,
        body["nbHits"].as_u64().unwrap(),
        "nbSortedHits should equal nbHits"
    );

    // Without sort, nbSortedHits should be absent
    let resp2 = post_search(&app, index_name, json!({"query": ""}), None).await;
    assert_eq!(resp2.status(), StatusCode::OK);
    let body2 = body_json(resp2).await;
    assert!(
        body2.get("nbSortedHits").is_none(),
        "nbSortedHits must be absent when no sort is active"
    );
}

fn find_user_token_for_arm(experiment: &Experiment, target_arm: &str) -> String {
    for i in 0..100_000 {
        let candidate = format!("tok-{i}");
        let assignment = assignment::assign_variant(experiment, Some(&candidate), None, "qid");
        if assignment.arm == target_arm {
            return candidate;
        }
    }
    panic!("failed to find user token for target arm: {target_arm}");
}

#[tokio::test]
async fn resolve_experiment_context_uses_session_id_when_user_token_missing() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let mut req = SearchRequest {
        session_id: Some("sid-456".to_string()),
        ..Default::default()
    };

    let (_effective_index, ctx) =
        resolve_experiment_context(&state, "products", &mut req, "query-fallback-id");

    let ctx = ctx.expect("active experiment must produce experiment context");
    assert_eq!(ctx.assignment_method, "session_id");
}

#[tokio::test]
async fn resolve_experiment_context_prefers_user_token_over_session_id() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let mut req = SearchRequest {
        user_token: Some("tok-priority".to_string()),
        session_id: Some("sid-ignored".to_string()),
        ..Default::default()
    };

    let (_effective_index, ctx) =
        resolve_experiment_context(&state, "products", &mut req, "query-fallback-id");

    let ctx = ctx.expect("active experiment must produce experiment context");
    assert_eq!(ctx.assignment_method, "user_token");
}

#[tokio::test]
async fn resolve_experiment_context_falls_back_to_query_id_without_stable_ids() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let mut req = SearchRequest::default();

    let (_effective_index, ctx) =
        resolve_experiment_context(&state, "products", &mut req, "query-only-id");

    let ctx = ctx.expect("active experiment must produce experiment context");
    assert_eq!(ctx.assignment_method, "query_id");
}

// ── A6 integration tests: search + experiments ──

/// Assert that responses include abTestID and abTestVariantID when an active experiment runs.
#[tokio::test]
async fn search_with_active_experiment_is_annotated() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let app = search_router(state);

    let resp = post_search(&app, "products", json!({ "query": "shoe" }), Some("user-a")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["abTestID"], "exp-mode-a",
        "abTestID must match the experiment ID"
    );
    let variant_id = body["abTestVariantID"].as_str().unwrap();
    assert!(
        variant_id == "control" || variant_id == "variant",
        "abTestVariantID must be 'control' or 'variant', got: {variant_id}"
    );
    assert_eq!(
        body["index"], "products",
        "response index must be the originally-requested index"
    );
    assert!(
        body.get("indexUsed").is_none(),
        "Mode A should not set indexUsed"
    );
}

/// Assert that abTestID and abTestVariantID are absent when no active experiment runs on the index.
#[tokio::test]
async fn search_without_active_experiment_has_no_ab_fields() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        "products_no_experiment",
        json!({ "query": "plain" }),
        Some("user-a"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body.get("abTestID").is_none());
    assert!(body.get("abTestVariantID").is_none());
}

/// Verify that enableABTest=false prevents experiment rerouting and removes A/B annotation fields.
#[tokio::test]
async fn search_enable_ab_test_false_skips_experiment_assignment() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let experiment = state
        .experiment_store
        .as_ref()
        .unwrap()
        .get("exp-mode-b")
        .unwrap();
    let variant_token = find_user_token_for_arm(&experiment, "variant");
    let app = search_router(state);

    let resp = post_search(
        &app,
        "products_mode_b",
        json!({
            "query": "variant",
            "enableABTest": false
        }),
        Some(&variant_token),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(
        body["index"], "products_mode_b",
        "opting out must keep the original index name in the response"
    );
    assert!(
        body.get("indexUsed").is_none(),
        "opting out must not reroute to experiment variant index"
    );
    assert!(
        body.get("abTestID").is_none(),
        "opting out must remove A/B annotation fields"
    );
    assert!(
        body.get("abTestVariantID").is_none(),
        "opting out must remove variant annotation fields"
    );
    assert_eq!(
        body["nbHits"].as_u64(),
        Some(0),
        "control index has no matches for query='variant'; variant reroute must be disabled"
    );
}

/// Validate that Mode B experiment variant routing reports the rerouted index name in the indexUsed field.
#[tokio::test]
async fn mode_b_variant_reroutes_shows_index_used() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let experiment = state
        .experiment_store
        .as_ref()
        .unwrap()
        .get("exp-mode-b")
        .unwrap();
    let variant_token = find_user_token_for_arm(&experiment, "variant");
    let app = search_router(state);

    let resp = post_search(
        &app,
        "products_mode_b",
        json!({ "query": "document" }),
        Some(&variant_token),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["index"], "products_mode_b");
    assert_eq!(body["indexUsed"], "products_mode_b_variant");
    assert_eq!(body["hits"][0]["objectID"], "mv1");
}

/// Verify that control arm in Mode B experiments remains on the original index without rerouting.
#[tokio::test]
async fn mode_b_control_stays_on_original_index() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let experiment = state
        .experiment_store
        .as_ref()
        .unwrap()
        .get("exp-mode-b")
        .unwrap();
    let control_token = find_user_token_for_arm(&experiment, "control");
    let app = search_router(state);

    let resp = post_search(
        &app,
        "products_mode_b",
        json!({ "query": "document" }),
        Some(&control_token),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["index"], "products_mode_b",
        "control arm must stay on original index"
    );
    assert!(
        body.get("indexUsed").is_none(),
        "control arm must not set indexUsed"
    );
    assert_eq!(
        body["abTestID"], "exp-mode-b",
        "abTestID must be present for control arm"
    );
    assert_eq!(
        body["abTestVariantID"], "control",
        "control arm must report variant_id as 'control'"
    );
    assert_eq!(
        body["hits"][0]["objectID"], "m1",
        "control arm must serve documents from original index"
    );
}

/// Verify that interleaving experiments merge results from both control and variant indexes.
#[tokio::test]
async fn test_interleaving_experiment_returns_interleaved_results() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        "products_interleave",
        json!({ "query": "interleave" }),
        Some("user-a"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["abTestID"], "exp-interleave");
    assert_eq!(
        body["abTestVariantID"], "interleaved",
        "interleaving requests should be marked as interleaved"
    );

    let hits = body["hits"]
        .as_array()
        .expect("interleaving response must include hits array");
    let hit_ids: std::collections::HashSet<String> = hits
        .iter()
        .map(|hit| {
            hit["objectID"]
                .as_str()
                .expect("every hit must include objectID")
                .to_string()
        })
        .collect();

    assert!(
        hit_ids.contains("ic1"),
        "interleaving should include control index result"
    );
    assert!(
        hit_ids.contains("iv1"),
        "interleaving should include variant index result"
    );
}

/// Verify that interleaved results include interleavedTeams mapping attributing each hit to control or variant.
#[tokio::test]
async fn test_interleaving_experiment_annotates_response_with_team_attribution() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let app = search_router(state);

    let resp = post_search(
        &app,
        "products_interleave",
        json!({ "query": "interleave" }),
        Some("user-a"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let teams = body["interleavedTeams"]
        .as_object()
        .expect("interleaving response must include interleavedTeams mapping");

    let hits = body["hits"]
        .as_array()
        .expect("interleaving response must include hits array");
    for hit in hits {
        let object_id = hit["objectID"]
            .as_str()
            .expect("every hit must include objectID");
        let team = teams
            .get(object_id)
            .and_then(|v| v.as_str())
            .expect("each hit must have a team attribution");
        assert!(
            team == "control" || team == "variant",
            "team attribution must be control or variant, got: {team}"
        );
    }
}

/// Confirm that standard Mode B experiments do not include interleavedTeams metadata.
#[tokio::test]
async fn test_interleaving_experiment_preserves_non_interleaving_behavior() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let experiment = state
        .experiment_store
        .as_ref()
        .unwrap()
        .get("exp-mode-b")
        .unwrap();
    let variant_token = find_user_token_for_arm(&experiment, "variant");
    let app = search_router(state);

    let resp = post_search(
        &app,
        "products_mode_b",
        json!({ "query": "document" }),
        Some(&variant_token),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["abTestID"], "exp-mode-b");
    assert_eq!(body["abTestVariantID"], "variant");
    assert!(
        body.get("interleavedTeams").is_none(),
        "standard Mode B responses must not include interleaving metadata"
    );
    assert_eq!(body["indexUsed"], "products_mode_b_variant");
}

/// Verify that interleaving falls back to control index when variant index does not exist, omitting experiment fields from response.
#[tokio::test]
async fn test_interleaving_falls_back_when_variant_index_missing() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp)
        .with_experiments()
        .build_shared();
    let experiment_store = state
        .experiment_store
        .as_ref()
        .expect("experiment store should be configured")
        .clone();

    state
        .manager
        .create_tenant("products_interleave_missing_variant")
        .unwrap();
    state
        .manager
        .add_documents_sync(
            "products_interleave_missing_variant",
            vec![make_doc("cm1", "control fallback document")],
        )
        .await
        .unwrap();

    experiment_store
        .create(interleaving_experiment(
            "exp-interleave-missing",
            "products_interleave_missing_variant",
            "products_interleave_missing_variant_v2",
        ))
        .unwrap();
    experiment_store.start("exp-interleave-missing").unwrap();

    let app = search_router(state);
    let resp = post_search(
        &app,
        "products_interleave_missing_variant",
        json!({ "query": "fallback" }),
        Some("user-a"),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["hits"][0]["objectID"], "cm1",
        "missing variant index should fall back to control search results"
    );
    assert!(
        body.get("interleavedTeams").is_none(),
        "control fallback should not expose interleaving attribution"
    );
    assert!(
        body.get("abTestID").is_none(),
        "control fallback should not annotate response with experiment ID"
    );
    assert!(
        body.get("abTestVariantID").is_none(),
        "control fallback should not annotate response with variant ID"
    );
}

/// Verify that search results are annotated with experiment fields even when no user token is provided, using query-level fallback assignment.
#[tokio::test]
async fn query_id_fallback_still_annotates_response() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let app = search_router(state);

    let resp = post_search(&app, "products", json!({ "query": "shoe" }), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["abTestID"], "exp-mode-a",
        "abTestID must match experiment ID even without user token"
    );
    let variant_id = body["abTestVariantID"].as_str().unwrap();
    assert!(
        variant_id == "control" || variant_id == "variant",
        "abTestVariantID must be 'control' or 'variant', got: {variant_id}"
    );
}

/// Verify that query-level fallback assignment produces different variant assignments across multiple independent queries when clickAnalytics is false.
#[tokio::test]
async fn query_id_fallback_without_click_analytics_varies_across_queries() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let app = search_router(state);
    let mut seen_arms = std::collections::HashSet::new();

    for _ in 0..32 {
        let resp = post_search(
            &app,
            "products",
            json!({ "query": "shoe", "clickAnalytics": false }),
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body.get("queryID").is_none());
        let variant = body["abTestVariantID"].as_str().unwrap().to_string();
        seen_arms.insert(variant);
    }

    assert!(
        seen_arms.len() > 1,
        "query-id fallback should vary assignment across independent queries"
    );
}

/// Verify that batch search results include experiment annotation fields when an active experiment exists.
#[tokio::test]
async fn batch_search_with_active_experiment_is_annotated() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let app = Router::new()
        .route("/1/indexes/:indexName/queries", post(batch_search))
        .with_state(state);

    let resp = post_batch_search(
        &app,
        json!({
            "requests": [
                { "indexName": "products", "query": "shoe" }
            ]
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["results"][0]["abTestID"], "exp-mode-a",
        "batch abTestID must match experiment ID"
    );
    let variant_id = body["results"][0]["abTestVariantID"].as_str().unwrap();
    assert!(
        variant_id == "control" || variant_id == "variant",
        "batch abTestVariantID must be 'control' or 'variant', got: {variant_id}"
    );
}

/// Verify that each query in a batch search request is independently annotated with experiment ID and variant ID.
#[tokio::test]
async fn batch_search_multiple_active_queries_each_annotated() {
    let tmp = TempDir::new().unwrap();
    let state = make_search_experiment_state(&tmp).await;
    let app = Router::new()
        .route("/1/indexes/:indexName/queries", post(batch_search))
        .with_state(state);

    let resp = post_batch_search(
        &app,
        json!({
            "requests": [
                { "indexName": "products", "query": "shoe" },
                { "indexName": "products", "query": "running" }
            ]
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let results = body["results"]
        .as_array()
        .expect("batch response must contain a results array");
    assert_eq!(
        results.len(),
        2,
        "batch response must include one result object per request"
    );

    for result in results {
        assert_eq!(
            result["abTestID"], "exp-mode-a",
            "every active-experiment batch result must include abTestID"
        );
        let variant_id = result["abTestVariantID"]
            .as_str()
            .expect("abTestVariantID must be a string");
        assert!(
            variant_id == "control" || variant_id == "variant",
            "abTestVariantID must be 'control' or 'variant', got: {variant_id}"
        );
    }
}

/// Verify that analytics events populated with an ExperimentContext record the experiment ID, variant ID, and assignment method.
#[test]
fn search_event_includes_experiment_fields() {
    let req = SearchRequest {
        query: "shoe".to_string(),
        user_token: Some("user-a".to_string()),
        user_ip: Some("10.0.0.1".to_string()),
        analytics_tags: Some(vec!["ab".to_string()]),
        facets: Some(vec!["brand".to_string()]),
        ..Default::default()
    };
    let event = build_search_event(&SearchEventParams {
        req: &req,
        query_id: Some("qid123".to_string()),
        index_name: "products".to_string(),
        nb_hits: 4,
        processing_time_ms: 8,
        page: 0,
        hits_per_page: 20,
        experiment_ctx: Some(&ExperimentContext {
            experiment_id: "exp-123".to_string(),
            variant_id: "variant".to_string(),
            assignment_method: "user_token".to_string(),
            interleaving_variant_index: None,
            interleaved_teams: None,
        }),
        country: None,
        region: None,
    });

    assert_eq!(event.experiment_id.as_deref(), Some("exp-123"));
    assert_eq!(event.variant_id.as_deref(), Some("variant"));
    assert_eq!(event.assignment_method.as_deref(), Some("user_token"));
    assert_eq!(event.index_name, "products");
}

/// Verify that Mode B analytics events record the effective (variant) index name, not the original index.
#[test]
fn search_event_mode_b_uses_effective_index() {
    let req = SearchRequest {
        query: "shoe".to_string(),
        user_token: Some("user-a".to_string()),
        ..Default::default()
    };
    // In Mode B, the caller passes effective_index (variant index) to build_search_event
    let event = build_search_event(&SearchEventParams {
        req: &req,
        query_id: Some("qid789".to_string()),
        index_name: "products_variant".to_string(),
        nb_hits: 3,
        processing_time_ms: 6,
        page: 0,
        hits_per_page: 20,
        experiment_ctx: Some(&ExperimentContext {
            experiment_id: "exp-mode-b".to_string(),
            variant_id: "variant".to_string(),
            assignment_method: "user_token".to_string(),
            interleaving_variant_index: None,
            interleaved_teams: None,
        }),
        country: None,
        region: None,
    });

    assert_eq!(
        event.index_name, "products_variant",
        "Mode B analytics event must use effective_index (variant index), not original"
    );
    assert_eq!(event.experiment_id.as_deref(), Some("exp-mode-b"));
    assert_eq!(event.variant_id.as_deref(), Some("variant"));
}

/// Verify that analytics events without an active experiment record None for all experiment-related fields.
#[test]
fn search_event_without_experiment_has_none_fields() {
    let req = SearchRequest {
        query: "shoe".to_string(),
        ..Default::default()
    };
    let event = build_search_event(&SearchEventParams {
        req: &req,
        query_id: Some("qid456".to_string()),
        index_name: "products".to_string(),
        nb_hits: 2,
        processing_time_ms: 5,
        page: 0,
        hits_per_page: 20,
        experiment_ctx: None,
        country: None,
        region: None,
    });

    assert!(
        event.experiment_id.is_none(),
        "no experiment should produce None experiment_id"
    );
    assert!(
        event.variant_id.is_none(),
        "no experiment should produce None variant_id"
    );
    assert!(
        event.assignment_method.is_none(),
        "no experiment should produce None assignment_method"
    );
}

// ── X-Algolia-Query-ID response header tests ──

/// Verify that the X-Algolia-Query-ID response header is set and matches the response body queryID when clickAnalytics is true.
#[tokio::test]
async fn search_with_click_analytics_returns_query_id_header() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    state.manager.create_tenant("qid_idx").unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, "qid_idx", &settings);
    state
        .manager
        .add_documents_sync(
            "qid_idx",
            vec![Document {
                id: "doc1".to_string(),
                fields: vec![("title".to_string(), FieldValue::Text("hello world".into()))]
                    .into_iter()
                    .collect(),
            }],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(
        &app,
        "qid_idx",
        json!({"query": "hello", "clickAnalytics": true}),
        None,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let header_val = resp
        .headers()
        .get("x-algolia-query-id")
        .expect("X-Algolia-Query-ID header must be present when clickAnalytics is true")
        .to_str()
        .unwrap()
        .to_string();

    let body = body_json(resp).await;
    let body_qid = body["queryID"].as_str().unwrap();
    assert_eq!(header_val, body_qid, "header must match body queryID");
}

/// Verify that the X-Algolia-Query-ID response header is not set when clickAnalytics is false.
#[tokio::test]
async fn search_without_click_analytics_omits_query_id_header() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    state.manager.create_tenant("no_qid_idx").unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, "no_qid_idx", &settings);
    state
        .manager
        .add_documents_sync(
            "no_qid_idx",
            vec![Document {
                id: "doc1".to_string(),
                fields: vec![("title".to_string(), FieldValue::Text("hello".into()))]
                    .into_iter()
                    .collect(),
            }],
        )
        .await
        .unwrap();

    let app = search_router(state);
    let resp = post_search(
        &app,
        "no_qid_idx",
        json!({"query": "hello", "clickAnalytics": false}),
        None,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("x-algolia-query-id").is_none(),
        "X-Algolia-Query-ID header must NOT be present when clickAnalytics is false"
    );
}

/// Verify that the X-Algolia-Query-ID response header is not set for batch search requests.
#[tokio::test]
async fn batch_search_does_not_set_query_id_header() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    state.manager.create_tenant("batch_qid_idx").unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, "batch_qid_idx", &settings);
    state
        .manager
        .add_documents_sync(
            "batch_qid_idx",
            vec![Document {
                id: "doc1".to_string(),
                fields: vec![("title".to_string(), FieldValue::Text("test".into()))]
                    .into_iter()
                    .collect(),
            }],
        )
        .await
        .unwrap();

    let app = Router::new()
        .route("/1/indexes/:indexName/queries", post(batch_search))
        .with_state(state);

    let resp = post_batch_search(
        &app,
        json!({
            "requests": [{
                "indexName": "batch_qid_idx",
                "query": "test",
                "clickAnalytics": true
            }]
        }),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("x-algolia-query-id").is_none(),
        "Batch search must NOT set X-Algolia-Query-ID header (individual queryIDs are in the body)"
    );
}

// ── Content-Type handling verification tests ──

/// Router that includes normalize_content_type middleware (matches production stack)
fn search_router_with_middleware(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/1/indexes/:indexName/query", post(search))
        .route("/1/indexes/:indexName/queries", post(batch_search))
        .layer(axum::middleware::from_fn(
            crate::middleware::normalize_content_type,
        ))
        .with_state(state)
}

/// Construct and send a POST request to the search endpoint with optional Content-Type header.
async fn post_search_with_content_type(
    app: &Router,
    index_name: &str,
    body: &str,
    content_type: Option<&str>,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(format!("/1/indexes/{index_name}/query"));
    if let Some(ct) = content_type {
        builder = builder.header("content-type", ct);
    }
    app.clone()
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap()
}

/// Verify that search requests with application/json; charset=utf-8 Content-Type are accepted.
#[tokio::test]
async fn post_search_with_charset_content_type_accepted() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    state.manager.create_tenant("ct_charset_idx").unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, "ct_charset_idx", &settings);

    let app = search_router_with_middleware(state);
    let resp = post_search_with_content_type(
        &app,
        "ct_charset_idx",
        r#"{"query": "hello"}"#,
        Some("application/json; charset=utf-8"),
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "application/json; charset=utf-8 must be accepted"
    );
}

/// Verify that search requests with plain application/json Content-Type are accepted.
#[tokio::test]
async fn post_search_with_plain_json_content_type_accepted() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    state.manager.create_tenant("ct_plain_idx").unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, "ct_plain_idx", &settings);

    let app = search_router_with_middleware(state);
    let resp = post_search_with_content_type(
        &app,
        "ct_plain_idx",
        r#"{"query": "hello"}"#,
        Some("application/json"),
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "application/json must be accepted"
    );
}

/// Verify that search requests without a Content-Type header are accepted and normalized by middleware.
#[tokio::test]
async fn post_search_with_no_content_type_accepted() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    state.manager.create_tenant("ct_none_idx").unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, "ct_none_idx", &settings);

    let app = search_router_with_middleware(state);
    let resp =
        post_search_with_content_type(&app, "ct_none_idx", r#"{"query": "hello"}"#, None).await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "missing Content-Type must be accepted (middleware normalizes it)"
    );
}

/// Verify that search requests with text/plain Content-Type are accepted and normalized by middleware.
#[tokio::test]
async fn post_search_with_text_plain_content_type_accepted() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    state.manager.create_tenant("ct_text_idx").unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, "ct_text_idx", &settings);

    let app = search_router_with_middleware(state);
    let resp = post_search_with_content_type(
        &app,
        "ct_text_idx",
        r#"{"query": "hello"}"#,
        Some("text/plain"),
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "text/plain Content-Type must be accepted (middleware normalizes it)"
    );
}

/// Verify that batch search requests with application/json; charset=utf-8 Content-Type are accepted.
#[tokio::test]
async fn post_batch_with_charset_content_type_accepted() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    state.manager.create_tenant("ct_batch_idx").unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    save_index_settings(&state, "ct_batch_idx", &settings);

    let app = search_router_with_middleware(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/indexes/*/queries")
                .header("content-type", "application/json; charset=utf-8")
                .body(Body::from(
                    json!({
                        "requests": [{
                            "indexName": "ct_batch_idx",
                            "query": "test"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "batch search with charset Content-Type must be accepted"
    );
}

// ── aroundLatLngViaIP resolution tests ──

/// Verify that coordinates are correctly resolved from a GeoIP lookup when aroundLatLngViaIP is enabled.
#[test]
fn around_lat_lng_via_ip_helper_requires_enabled_and_no_conflicting_geo_anchors() {
    let disabled = SearchRequest {
        around_lat_lng_via_ip: Some(false),
        ..Default::default()
    };
    assert!(!should_resolve_around_lat_lng_via_ip(&disabled));

    let explicit_center = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        around_lat_lng: Some("48.8566,2.3522".to_string()),
        ..Default::default()
    };
    assert!(!should_resolve_around_lat_lng_via_ip(&explicit_center));

    let with_box = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        inside_bounding_box: Some(serde_json::json!([[47.0, 1.0, 42.0, 7.0]])),
        ..Default::default()
    };
    assert!(!should_resolve_around_lat_lng_via_ip(&with_box));

    let with_polygon = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        inside_polygon: Some(serde_json::json!([[47.0, 1.0, 42.0, 7.0, 44.0, 3.0]])),
        ..Default::default()
    };
    assert!(!should_resolve_around_lat_lng_via_ip(&with_polygon));

    let eligible = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        ..Default::default()
    };
    assert!(should_resolve_around_lat_lng_via_ip(&eligible));
}

#[test]
fn parse_client_ip_for_geo_returns_none_for_missing_or_invalid_values() {
    assert_eq!(parse_client_ip_for_geo(None), None);
    assert_eq!(parse_client_ip_for_geo(Some("not-an-ip")), None);
    assert_eq!(
        parse_client_ip_for_geo(Some("8.8.8.8")),
        Some("8.8.8.8".parse().unwrap())
    );
}
/// TODO: Document resolve_geoip_lookup_ip_requires_eligible_request_and_valid_user_ip.
#[test]
fn resolve_geoip_lookup_ip_requires_eligible_request_and_valid_user_ip() {
    let disabled = SearchRequest {
        around_lat_lng_via_ip: Some(false),
        user_ip: Some("8.8.8.8".to_string()),
        ..Default::default()
    };
    assert_eq!(resolve_geoip_lookup_ip(&disabled), None);

    let missing_ip = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        user_ip: None,
        ..Default::default()
    };
    assert_eq!(resolve_geoip_lookup_ip(&missing_ip), None);

    let invalid_ip = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        user_ip: Some("nope".to_string()),
        ..Default::default()
    };
    assert_eq!(resolve_geoip_lookup_ip(&invalid_ip), None);

    let valid_ip = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        user_ip: Some("8.8.8.8".to_string()),
        ..Default::default()
    };
    assert_eq!(
        resolve_geoip_lookup_ip(&valid_ip),
        Some("8.8.8.8".parse().unwrap())
    );
}

#[test]
fn resolve_geoip_reader_returns_none_when_unavailable() {
    let no_reader: Option<Arc<crate::geoip::GeoIpReader>> = None;
    assert!(resolve_geoip_reader(&no_reader).is_none());
}
/// TODO: Document around_lat_lng_via_ip_resolves_coords_from_geoip.
#[test]
fn around_lat_lng_via_ip_resolves_coords_from_geoip() {
    // Requires a real MMDB file — skip if not available
    let db_path = std::env::var("FLAPJACK_TEST_GEOIP_DB").unwrap_or_default();
    if db_path.is_empty() {
        eprintln!(
            "Skipping around_lat_lng_via_ip_resolves_coords_from_geoip: \
             FLAPJACK_TEST_GEOIP_DB not set"
        );
        return;
    }
    let reader = crate::geoip::GeoIpReader::new(std::path::Path::new(&db_path))
        .expect("Should load MMDB from FLAPJACK_TEST_GEOIP_DB");
    let geoip = Some(Arc::new(reader));

    let mut req = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        around_lat_lng: None,
        user_ip: Some("8.8.8.8".to_string()), // Google DNS, resolves to US coords
        ..Default::default()
    };

    resolve_around_lat_lng_via_ip(&mut req, &geoip);

    // After resolution, around_lat_lng should be set to coordinates
    assert!(
        req.around_lat_lng.is_some(),
        "around_lat_lng should be set from GeoIP lookup"
    );
    let coords = req.around_lat_lng.unwrap();
    let parts: Vec<&str> = coords.split(',').collect();
    assert_eq!(parts.len(), 2, "coords should be 'lat,lng' format");
    let lat: f64 = parts[0].parse().expect("lat should be f64");
    let lng: f64 = parts[1].parse().expect("lng should be f64");
    // Google DNS resolves to somewhere in the US
    assert!(
        lat > 20.0 && lat < 60.0,
        "lat {} should be in US range",
        lat
    );
    assert!(
        lng > -130.0 && lng < -60.0,
        "lng {} should be in US range",
        lng
    );
}

/// Verify that an explicit aroundLatLng value takes precedence over GeoIP-based resolution.
#[test]
fn around_lat_lng_via_ip_explicit_around_lat_lng_takes_precedence() {
    let mut req = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        around_lat_lng: Some("48.8566,2.3522".to_string()), // Paris
        user_ip: Some("8.8.8.8".to_string()),
        ..Default::default()
    };

    resolve_around_lat_lng_via_ip(&mut req, &None);

    // Explicit aroundLatLng must not be overwritten
    assert_eq!(
        req.around_lat_lng.as_deref(),
        Some("48.8566,2.3522"),
        "explicit aroundLatLng should take precedence"
    );
}

/// Verify that coordinate resolution is skipped gracefully when no GeoIP reader is configured.
#[test]
fn around_lat_lng_via_ip_degrades_when_no_geoip_reader() {
    let mut req = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        around_lat_lng: None,
        user_ip: Some("8.8.8.8".to_string()),
        ..Default::default()
    };

    // No GeoIP reader available — should degrade gracefully
    resolve_around_lat_lng_via_ip(&mut req, &None);

    assert!(
        req.around_lat_lng.is_none(),
        "should not set coords when GeoIP reader is unavailable"
    );
}

/// Verify that private IP addresses do not resolve to coordinates, even with a valid GeoIP database available.
#[test]
fn around_lat_lng_via_ip_degrades_for_private_ip() {
    // Even with a real reader, private IPs should not resolve.
    // Test with None reader to verify no panic; the GeoIpReader.lookup()
    // already filters private IPs (tested in geoip.rs).
    let mut req = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        around_lat_lng: None,
        user_ip: Some("192.168.1.1".to_string()),
        ..Default::default()
    };

    resolve_around_lat_lng_via_ip(&mut req, &None);

    assert!(
        req.around_lat_lng.is_none(),
        "private IP should not resolve to coords"
    );

    // Also test with real DB if available
    let db_path = std::env::var("FLAPJACK_TEST_GEOIP_DB").unwrap_or_default();
    if !db_path.is_empty() {
        let reader = crate::geoip::GeoIpReader::new(std::path::Path::new(&db_path))
            .expect("Should load MMDB");
        let geoip = Some(Arc::new(reader));
        let mut req2 = SearchRequest {
            around_lat_lng_via_ip: Some(true),
            around_lat_lng: None,
            user_ip: Some("192.168.1.1".to_string()),
            ..Default::default()
        };
        resolve_around_lat_lng_via_ip(&mut req2, &geoip);
        assert!(
            req2.around_lat_lng.is_none(),
            "private IP should not resolve even with a real GeoIP reader"
        );
    }
}

/// Verify that setting aroundLatLngViaIP to false or None does not trigger coordinate resolution.
#[test]
fn around_lat_lng_via_ip_false_does_not_activate() {
    let mut req = SearchRequest {
        around_lat_lng_via_ip: Some(false),
        around_lat_lng: None,
        user_ip: Some("8.8.8.8".to_string()),
        ..Default::default()
    };

    resolve_around_lat_lng_via_ip(&mut req, &None);
    assert!(
        req.around_lat_lng.is_none(),
        "aroundLatLngViaIP=false should not trigger resolution"
    );

    // Also test absent (None)
    let mut req2 = SearchRequest {
        around_lat_lng_via_ip: None,
        around_lat_lng: None,
        user_ip: Some("8.8.8.8".to_string()),
        ..Default::default()
    };

    resolve_around_lat_lng_via_ip(&mut req2, &None);
    assert!(
        req2.around_lat_lng.is_none(),
        "aroundLatLngViaIP=None should not trigger resolution"
    );
}

/// Verify that aroundLatLngViaIP resolution is skipped when insideBoundingBox or insidePolygon constraints are present.
#[test]
fn around_lat_lng_via_ip_ignored_with_bounding_box() {
    let mut req = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        around_lat_lng: None,
        user_ip: Some("8.8.8.8".to_string()),
        inside_bounding_box: Some(serde_json::json!([[47.0, 1.0, 42.0, 7.0]])),
        ..Default::default()
    };

    resolve_around_lat_lng_via_ip(&mut req, &None);
    assert!(
        req.around_lat_lng.is_none(),
        "aroundLatLngViaIP should be ignored when insideBoundingBox is set"
    );

    // Same for insidePolygon
    let mut req2 = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        around_lat_lng: None,
        user_ip: Some("8.8.8.8".to_string()),
        inside_polygon: Some(serde_json::json!([[47.0, 1.0, 42.0, 7.0, 44.0, 3.0]])),
        ..Default::default()
    };

    resolve_around_lat_lng_via_ip(&mut req2, &None);
    assert!(
        req2.around_lat_lng.is_none(),
        "aroundLatLngViaIP should be ignored when insidePolygon is set"
    );
}

// ── Country/Region enrichment from GeoIP ────────────────────────

#[test]
fn resolve_country_region_graceful_degradation() {
    // No user IP
    assert_eq!(resolve_country_region_from_ip(&None, &None), (None, None));
    // Invalid IP string
    assert_eq!(
        resolve_country_region_from_ip(&Some("not-an-ip".to_string()), &None),
        (None, None),
    );
    // Valid IP but no reader
    assert_eq!(
        resolve_country_region_from_ip(&Some("8.8.8.8".to_string()), &None),
        (None, None),
    );
}

/// Verify that country and region are correctly resolved from a real GeoIP database for public IPs, and remain None for private IPs.
#[test]
fn resolve_country_region_with_real_db() {
    let db_path = std::env::var("FLAPJACK_TEST_GEOIP_DB").unwrap_or_default();
    if db_path.is_empty() {
        eprintln!("Skipping: FLAPJACK_TEST_GEOIP_DB not set");
        return;
    }
    let reader =
        crate::geoip::GeoIpReader::new(std::path::Path::new(&db_path)).expect("Should load MMDB");
    let geoip = Some(std::sync::Arc::new(reader));

    // Public IP should resolve to a country
    let (country, _region) = resolve_country_region_from_ip(&Some("8.8.8.8".to_string()), &geoip);
    assert_eq!(country.as_deref(), Some("US"));

    // Private IP should return None even with a valid reader
    let (country, region) =
        resolve_country_region_from_ip(&Some("192.168.1.1".to_string()), &geoip);
    assert_eq!(country, None);
    assert_eq!(region, None);
}

// ── Hybrid search integration tests (6.17) ──
// Behind vector-search feature flag. These exercise the full search_single path.

#[cfg(feature = "vector-search")]
mod auto_embed_integration_tests;
mod batch_federation;
mod batch_multi_index;
mod batch_multi_index_error_handling;
#[cfg(feature = "vector-search-local")]
mod fastembed_integration_tests;
#[cfg(not(feature = "vector-search-local"))]
#[cfg(feature = "vector-search")]
mod fastembed_rejected_tests;
mod get_search_query_endpoint;
mod get_search_query_string;
#[cfg(feature = "vector-search")]
mod hybrid_search_tests;
mod proximity_ranking_tests;
mod request_limits;
mod response_fields_experiment_tests;
mod stage4_structural_search_params;
mod stage4b_pipeline_regressions;
mod stage5a_relevancy_strictness;
mod stage5b_virtual_replica_relevancy;
