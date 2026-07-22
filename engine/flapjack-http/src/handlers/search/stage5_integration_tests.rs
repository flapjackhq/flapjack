//! Handler-level integration tests for the search endpoint. Covers GeoIP resolution, graceful degradation when services are disabled, analytics event persistence, and GET/POST response equivalence.
use super::*;
use crate::handlers::AppState;
use crate::test_helpers::body_json;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    routing::{get, post},
    Router,
};
use flapjack::types::{Document, FieldValue};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

fn make_doc(id: &str, title: &str) -> Document {
    let mut fields = HashMap::new();
    fields.insert("title".to_string(), FieldValue::Text(title.to_string()));
    Document {
        id: id.to_string(),
        fields,
    }
}

fn make_state(tmp: &TempDir, geoip: Option<Arc<crate::geoip::GeoIpReader>>) -> Arc<AppState> {
    let builder = crate::test_helpers::TestStateBuilder::new(tmp);
    match geoip {
        Some(reader) => builder.with_geoip(reader).build_shared(),
        None => builder.build_shared(),
    }
}

fn search_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/1/indexes/:indexName/query", post(search).get(search_get))
        .route("/1/indexes/:indexName", get(search_get))
        .with_state(state)
}

/// Execute a POST request to the search endpoint with an x-forwarded-for IP header.
///
/// # Arguments
/// - `app`: The search router instance
/// - `index`: The index name to query
/// - `body`: The search request body as JSON
/// - `forwarded_for`: The IP address to set in the x-forwarded-for header
///
/// # Returns
/// The raw HTTP response from the search endpoint.
async fn post_search_with_ip(
    app: &Router,
    index: &str,
    body: Value,
    forwarded_for: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/1/indexes/{index}/query"))
                .header("content-type", "application/json")
                .header("x-forwarded-for", forwarded_for)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn post_search_simple(app: &Router, index: &str, body: Value) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/1/indexes/{index}/query"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[path = "stage5_integration_tests/search_pagination_known_answer.rs"]
mod search_pagination_known_answer;

// ── GeoIP + Search: handler-level integration ──

/// Handler-level: aroundLatLngViaIP=true with geoip_reader=None degrades gracefully.
#[tokio::test]
async fn handler_via_ip_degrades_when_no_geoip_reader() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp, None);
    state.manager.create_tenant("geo_deg").unwrap();
    state
        .manager
        .add_documents_sync("geo_deg", vec![make_doc("d1", "laptop")])
        .await
        .unwrap();

    let app = search_app(state);
    let resp = post_search_simple(
        &app,
        "geo_deg",
        json!({"query": "laptop", "aroundLatLngViaIP": true}),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(
        body["nbHits"].as_u64().unwrap() >= 1,
        "search should return results even without GeoIP"
    );
}

/// Handler-level: aroundLatLngViaIP=true with real GeoIP reader resolves coords.
/// Skipped if FLAPJACK_TEST_GEOIP_DB is not set.
#[tokio::test]
async fn handler_via_ip_resolves_with_geoip_reader() {
    let db_path = std::env::var("FLAPJACK_TEST_GEOIP_DB").unwrap_or_default();
    if db_path.is_empty() {
        eprintln!(
            "Skipping handler_via_ip_resolves_with_geoip_reader: FLAPJACK_TEST_GEOIP_DB not set"
        );
        return;
    }
    let reader =
        crate::geoip::GeoIpReader::new(std::path::Path::new(&db_path)).expect("Should load MMDB");

    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp, Some(Arc::new(reader)));
    state.manager.create_tenant("geo_res").unwrap();
    state
        .manager
        .add_documents_sync("geo_res", vec![make_doc("d1", "laptop")])
        .await
        .unwrap();

    let app = search_app(state);
    let resp = post_search_with_ip(
        &app,
        "geo_res",
        json!({"query": "laptop", "aroundLatLngViaIP": true}),
        "8.8.8.8",
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["nbHits"].as_u64().unwrap() >= 1);
}

/// aroundPrecision + minimumAroundRadius persist after IP-derived center resolution.
#[test]
fn via_ip_preserves_precision_and_min_radius() {
    let db_path = std::env::var("FLAPJACK_TEST_GEOIP_DB").unwrap_or_default();
    if db_path.is_empty() {
        eprintln!(
            "Skipping via_ip_preserves_precision_and_min_radius: FLAPJACK_TEST_GEOIP_DB not set"
        );
        return;
    }
    let reader =
        crate::geoip::GeoIpReader::new(std::path::Path::new(&db_path)).expect("Should load MMDB");
    let geoip = Some(Arc::new(reader));

    let mut req = SearchRequest {
        around_lat_lng_via_ip: Some(true),
        around_lat_lng: None,
        user_ip: Some("8.8.8.8".to_string()),
        around_precision: Some(json!(100)),
        minimum_around_radius: Some(1000),
        ..Default::default()
    };

    resolve_around_lat_lng_via_ip(&mut req, &geoip);

    assert!(
        req.around_lat_lng.is_some(),
        "aroundLatLng should be set from GeoIP"
    );
    assert_eq!(
        req.around_precision,
        Some(json!(100)),
        "aroundPrecision must be preserved"
    );
    assert_eq!(
        req.minimum_around_radius,
        Some(1000),
        "minimumAroundRadius must be preserved"
    );
}

// ── Degradation: both GeoIP and SES disabled ──

#[tokio::test]
async fn appstate_both_geoip_and_ses_disabled() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp, None);
    assert!(state.geoip_reader.is_none());
    assert!(state.notification_service.is_none());
}

/// Verify that search queries succeed when GeoIP resolution and notification service are both disabled.
///
/// When AppState lacks both geoip_reader and notification_service, a search request including aroundLatLngViaIP=true should still execute and return results.
#[tokio::test]
async fn search_works_with_both_geoip_and_ses_disabled() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp, None);
    state.manager.create_tenant("both_off").unwrap();
    state
        .manager
        .add_documents_sync("both_off", vec![make_doc("d1", "laptop")])
        .await
        .unwrap();

    let app = search_app(state);
    let resp = post_search_simple(
        &app,
        "both_off",
        json!({"query": "laptop", "aroundLatLngViaIP": true}),
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(
        body["nbHits"].as_u64().unwrap() >= 1,
        "search must work with both GeoIP and SES disabled"
    );
}

/// Disabled NotificationService methods do not panic.
#[test]
fn disabled_notifier_all_methods_no_panic() {
    let service = crate::notifications::NotificationService::disabled();
    assert!(!service.is_enabled());
    // All methods should be no-ops, no panic
    assert!(!service.send_usage_alert("idx", "searches", 100, 50));
    service.send_gdpr_confirmation("user_token_xyz");
    service.send_key_lifecycle("My Key", "created");
    service.send_key_lifecycle("My Key", "deleted");
}

// ── Analytics + GeoIP enrichment ──

/// Search event records country=None when GeoIP is unavailable; event still persisted.
#[tokio::test]
async fn analytics_search_event_persisted_without_geoip() {
    use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig, AnalyticsQueryEngine};

    let tmp = TempDir::new().unwrap();
    let config = AnalyticsConfig {
        enabled: true,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 3600,
        flush_size: 100_000,
        retention_days: 90,
    };
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));

    // Record a search event with country=None (simulates no GeoIP)
    let qid = "b".repeat(32);
    let event = flapjack::analytics::schema::SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: "laptop".to_string(),
        query_id: Some(qid),
        index_name: "products".to_string(),
        nb_hits: 5,
        processing_time_ms: 3,
        user_token: Some("user1".to_string()),
        user_ip: Some("192.168.1.1".to_string()),
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: None,
        region: None,
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    };
    collector.record_search(event);
    collector.flush_all();

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    // Event should be queryable via top_searches
    let searches = engine
        .top_searches(
            &flapjack::analytics::AnalyticsQueryParams {
                index_name: "products",
                start_date: &today,
                end_date: &today,
                limit: 10,
                tags: None,
            },
            false,
            None,
        )
        .await;
    assert!(
        searches.is_ok(),
        "top_searches should work even with country=None events"
    );

    // Countries should be empty since no country was enriched
    let countries = engine
        .countries(
            &flapjack::analytics::AnalyticsQueryParams {
                index_name: "products",
                start_date: &today,
                end_date: &today,
                limit: 10,
                tags: None,
            },
            0,
            None,
        )
        .await;
    assert!(countries.is_ok());
    let countries_val = countries.unwrap();
    let list = countries_val["countries"].as_array();
    assert!(
        list.is_none() || list.unwrap().is_empty(),
        "countries should be empty when events have no country enrichment"
    );
}

/// Search event with country field set propagates to /2/countries endpoint.
#[tokio::test]
async fn analytics_countries_reflects_enriched_events() {
    use axum::routing::get;
    use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig, AnalyticsQueryEngine};

    let tmp = TempDir::new().unwrap();
    let config = AnalyticsConfig {
        enabled: true,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 3600,
        flush_size: 100_000,
        retention_days: 90,
    };
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));

    // Record events with enriched country
    let qid1 = "c".repeat(32);
    let mut event1 = flapjack::analytics::schema::SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: "phone".to_string(),
        query_id: Some(qid1),
        index_name: "products".to_string(),
        nb_hits: 10,
        processing_time_ms: 2,
        user_token: None,
        user_ip: None,
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: Some("US".to_string()),
        region: Some("CA".to_string()),
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    };
    collector.record_search(event1.clone());

    // Second event from DE
    event1.query_id = Some("d".repeat(32));
    event1.country = Some("DE".to_string());
    event1.region = None;
    collector.record_search(event1);
    collector.flush_all();

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let app = Router::new()
        .route(
            "/2/countries",
            get(crate::handlers::analytics::get_countries),
        )
        .with_state(engine);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/2/countries?index=products&startDate={today}&endDate={today}"
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let countries = body["countries"]
        .as_array()
        .expect("should have countries array");
    assert_eq!(countries.len(), 2, "should have US and DE entries");

    let country_codes: Vec<&str> = countries
        .iter()
        .map(|c| c["country"].as_str().unwrap())
        .collect();
    assert!(country_codes.contains(&"US"));
    assert!(country_codes.contains(&"DE"));
}

/// AppState construction succeeds with notification_service set to a disabled notifier.
#[tokio::test]
async fn appstate_construction_with_disabled_notifier() {
    let tmp = TempDir::new().unwrap();
    let notifier = Arc::new(crate::notifications::NotificationService::disabled());
    let mut state = crate::test_helpers::TestStateBuilder::new(&tmp).build();
    state.notification_service = Some(notifier);
    let state = Arc::new(state);
    // Notification service is set but disabled — should not panic
    assert!(state.notification_service.is_some());
    assert!(!state.notification_service.as_ref().unwrap().is_enabled());
}

// ── Stage 5: Wire-Format Parity ────────────────────────────────────────────

/// GET /1/indexes/:indexName/query alias returns equivalent response payload
/// to POST /1/indexes/:indexName/query (same field semantics, structure, types).
#[tokio::test]
async fn get_query_alias_matches_post_query_shape() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp, None);

    // Create index with sample documents
    state.manager.create_tenant("test_idx").unwrap();
    let settings = flapjack::index::settings::IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    let settings_json = serde_json::to_value(&settings).unwrap();
    let settings_path = tmp.path().join("test_idx").join("settings.json");
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(&settings_path, settings_json.to_string()).unwrap();

    // Add documents
    state
        .manager
        .add_documents_sync(
            "test_idx",
            vec![
                make_doc("doc1", "hello world"),
                make_doc("doc2", "hello again"),
            ],
        )
        .await
        .unwrap();

    let app = search_app(state);

    // POST search
    let post_resp = post_search_simple(&app, "test_idx", json!({"query": "hello"})).await;
    assert_eq!(post_resp.status(), StatusCode::OK);
    let post_body = body_json(post_resp).await;

    // GET search (alias) - use the /query path for parity testing
    let get_resp = get_search(&app, "/1/indexes/test_idx/query?query=hello").await;
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_body = body_json(get_resp).await;

    // Verify both responses have identical field structure
    let post_keys: std::collections::HashSet<String> =
        post_body.as_object().unwrap().keys().cloned().collect();
    let get_keys: std::collections::HashSet<String> =
        get_body.as_object().unwrap().keys().cloned().collect();

    // Core fields that must be present in both
    let core_fields = [
        "hits",
        "nbHits",
        "page",
        "nbPages",
        "hitsPerPage",
        "processingTimeMS",
        "query",
        "params",
        "exhaustive",
        "exhaustiveNbHits",
        "exhaustiveTypo",
        "index",
    ];
    for field in &core_fields {
        assert!(
            post_keys.contains(*field),
            "POST response missing core field: {}",
            field
        );
        assert!(
            get_keys.contains(*field),
            "GET response missing core field: {}",
            field
        );
    }

    // Verify type parity for key fields
    assert_eq!(
        post_body["nbHits"].as_u64(),
        get_body["nbHits"].as_u64(),
        "nbHits should match between POST and GET"
    );
    assert_eq!(
        post_body["hits"].as_array().map(|a| a.len()),
        get_body["hits"].as_array().map(|a| a.len()),
        "hits array length should match between POST and GET"
    );
    assert_eq!(
        post_body["page"].as_u64(),
        get_body["page"].as_u64(),
        "page should match between POST and GET"
    );
    assert_eq!(
        post_body["nbPages"].as_u64(),
        get_body["nbPages"].as_u64(),
        "nbPages should match between POST and GET"
    );
    assert_eq!(
        post_body["hitsPerPage"].as_u64(),
        get_body["hitsPerPage"].as_u64(),
        "hitsPerPage should match between POST and GET"
    );

    // Verify hits have objectID in both
    if let Some(post_hits) = post_body["hits"].as_array() {
        if let Some(get_hits) = get_body["hits"].as_array() {
            for (i, (post_hit, get_hit)) in post_hits.iter().zip(get_hits.iter()).enumerate() {
                assert!(
                    post_hit.get("objectID").is_some(),
                    "POST hit {} missing objectID",
                    i
                );
                assert!(
                    get_hit.get("objectID").is_some(),
                    "GET hit {} missing objectID",
                    i
                );
                assert_eq!(
                    post_hit["objectID"], get_hit["objectID"],
                    "objectID should match at index {}",
                    i
                );
            }
        }
    }
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
