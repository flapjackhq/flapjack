use super::*;
use axum::{
    http::{Method, StatusCode},
    routing::{delete, get, post},
    Router,
};
use flapjack::analytics::schema::InsightEvent;
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig, AnalyticsQueryEngine};
use flapjack::personalization::PersonalizationProfile;
use flapjack::{Document, FieldValue};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use tempfile::TempDir;

/// Construct an `AppState` in a temp directory with an optional analytics query engine for testing.
///
/// # Arguments
///
/// * `tmp` - Temporary directory used as the data root.
/// * `analytics_engine` - Optional analytics engine; `None` disables analytics-based profile computation.
fn make_state_with_analytics(
    tmp: &TempDir,
    analytics_engine: Option<Arc<AnalyticsQueryEngine>>,
) -> Arc<AppState> {
    let builder = crate::test_helpers::TestStateBuilder::new(tmp);
    match analytics_engine {
        Some(engine) => builder.with_analytics_engine(engine).build_shared(),
        None => builder.build_shared(),
    }
}

fn make_state(tmp: &TempDir) -> Arc<AppState> {
    make_state_with_analytics(tmp, None)
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

fn make_product_doc(id: &str, brand: &str) -> Document {
    let mut fields = HashMap::new();
    fields.insert("brand".to_string(), FieldValue::Facet(brand.to_string()));
    Document {
        id: id.to_string(),
        fields,
    }
}

/// Build an `InsightEvent` of type "view" for the given user, object, and timestamp.
fn make_view_event(user_token: &str, object_id: &str, timestamp_ms: i64) -> InsightEvent {
    InsightEvent {
        event_type: "view".to_string(),
        event_subtype: None,
        event_name: "Product viewed".to_string(),
        index: "products".to_string(),
        user_token: user_token.to_string(),
        authenticated_user_token: None,
        query_id: None,
        object_ids: vec![object_id.to_string()],
        object_ids_alt: vec![],
        positions: None,
        timestamp: Some(timestamp_ms),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

fn personalization_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/1/strategies/personalization",
            post(set_personalization_strategy)
                .get(get_personalization_strategy)
                .delete(delete_personalization_strategy),
        )
        .route(
            "/1/profiles/personalization/:userToken",
            get(get_user_profile),
        )
        .route("/1/profiles/:userToken", delete(delete_user_profile))
        .with_state(state)
}

/// Send a JSON request to the router and return the status code and parsed JSON body.
///
/// # Arguments
///
/// * `app` - The test router.
/// * `method` - HTTP method.
/// * `uri` - Request URI.
/// * `body` - JSON payload.
async fn send_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = crate::test_helpers::send_json_request(app, method, uri, body).await;
    let status = resp.status();
    let json = crate::test_helpers::body_json(resp).await;
    (status, json)
}

async fn send_get(app: &Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = crate::test_helpers::send_empty_request(app, Method::GET, uri).await;
    let status = resp.status();
    let json = crate::test_helpers::body_json(resp).await;
    (status, json)
}

async fn send_delete(app: &Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = crate::test_helpers::send_empty_request(app, Method::DELETE, uri).await;
    let status = resp.status();
    let json = crate::test_helpers::body_json(resp).await;
    (status, json)
}

fn valid_strategy() -> serde_json::Value {
    json!({
        "eventsScoring": [
            { "eventName": "Add to cart", "eventType": "conversion", "score": 50 },
            { "eventName": "Product viewed", "eventType": "view", "score": 10 }
        ],
        "facetsScoring": [
            { "facetName": "brand", "score": 70 },
            { "facetName": "category", "score": 30 }
        ],
        "personalizationImpact": 75
    })
}

// --- B1: Strategy CRUD Integration Tests ---

/// Verify that POST returns a 200 with an RFC 3339 `updatedAt` timestamp.
#[tokio::test]
async fn test_post_strategy_returns_updated_at() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        valid_strategy(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body["updatedAt"].as_str().is_some(),
        "response should contain updatedAt: {body}"
    );
    // Verify it's a valid RFC3339 timestamp
    let ts = body["updatedAt"].as_str().unwrap();
    assert!(
        chrono::DateTime::parse_from_rfc3339(ts).is_ok(),
        "updatedAt should be valid RFC3339: {ts}"
    );
}

/// Verify that a strategy saved via POST is returned identically by GET.
#[tokio::test]
async fn test_strategy_round_trips_exactly() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    let strategy = valid_strategy();
    let (post_status, _) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        strategy.clone(),
    )
    .await;
    assert_eq!(post_status, StatusCode::OK);

    let (get_status, get_body) = send_get(&app, "/1/strategies/personalization").await;

    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(
        get_body, strategy,
        "GET should return exact same config as POST"
    );
}

/// Reject a personalizationImpact value above 100.
#[tokio::test]
async fn test_post_strategy_invalid_impact_too_high() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    let mut strategy = valid_strategy();
    strategy["personalizationImpact"] = json!(101);

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        strategy,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "101 should be rejected: {body}"
    );
}

/// Accept personalizationImpact of 0 (disables personalization).
#[tokio::test]
async fn test_post_strategy_impact_zero_is_valid() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    let mut strategy = valid_strategy();
    strategy["personalizationImpact"] = json!(0);

    let (status, _) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        strategy,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "0 should be valid (means disabled)");
}

/// Reject an event scoring entry whose eventType is not one of the allowed variants.
#[tokio::test]
async fn test_post_strategy_invalid_event_type() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    let strategy = json!({
        "eventsScoring": [
            { "eventName": "Clicked", "eventType": "purchase", "score": 10 }
        ],
        "facetsScoring": [],
        "personalizationImpact": 50
    });

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        strategy,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "invalid eventType: {body}");
    assert!(
        body["message"].as_str().unwrap().contains("eventType"),
        "error should mention eventType: {body}"
    );
}

/// Reject event and facet scoring entries that have a score of 0.
#[tokio::test]
async fn test_post_strategy_score_zero_rejected() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    // Event score = 0
    let strategy = json!({
        "eventsScoring": [
            { "eventName": "Clicked", "eventType": "click", "score": 0 }
        ],
        "facetsScoring": [],
        "personalizationImpact": 50
    });
    let (status, _) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        strategy,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "event score 0 should be rejected"
    );

    // Facet score = 0
    let strategy2 = json!({
        "eventsScoring": [],
        "facetsScoring": [
            { "facetName": "brand", "score": 0 }
        ],
        "personalizationImpact": 50
    });
    let (status2, _) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        strategy2,
    )
    .await;
    assert_eq!(
        status2,
        StatusCode::BAD_REQUEST,
        "facet score 0 should be rejected"
    );
}

/// Reject a strategy containing more than 15 event scoring entries.
#[tokio::test]
async fn test_post_strategy_too_many_events() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    let events: Vec<serde_json::Value> = (0..16)
        .map(|i| json!({ "eventName": format!("Event{i}"), "eventType": "click", "score": 10 }))
        .collect();

    let strategy = json!({
        "eventsScoring": events,
        "facetsScoring": [],
        "personalizationImpact": 50
    });

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        strategy,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, ">15 events: {body}");
}

/// Reject a strategy containing more than 15 facet scoring entries.
#[tokio::test]
async fn test_post_strategy_too_many_facets() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    let facets: Vec<serde_json::Value> = (0..16)
        .map(|i| json!({ "facetName": format!("facet{i}"), "score": 10 }))
        .collect();

    let strategy = json!({
        "eventsScoring": [],
        "facetsScoring": facets,
        "personalizationImpact": 50
    });

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        strategy,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, ">15 facets: {body}");
}

#[tokio::test]
async fn test_get_strategy_404_when_not_set() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    let (status, _) = send_get(&app, "/1/strategies/personalization").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
/// TODO: Document test_delete_strategy_then_get_returns_404.
#[tokio::test]
async fn test_delete_strategy_then_get_returns_404() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    let (post_status, _) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        valid_strategy(),
    )
    .await;
    assert_eq!(post_status, StatusCode::OK);

    let (delete_status, delete_body) = send_delete(&app, "/1/strategies/personalization").await;
    assert_eq!(delete_status, StatusCode::OK);
    let deleted_at = delete_body["deletedAt"]
        .as_str()
        .expect("deletedAt should be present");
    assert!(
        chrono::DateTime::parse_from_rfc3339(deleted_at).is_ok(),
        "deletedAt should be RFC3339: {delete_body}"
    );

    let (get_status, _) = send_get(&app, "/1/strategies/personalization").await;
    assert_eq!(get_status, StatusCode::NOT_FOUND);
}
/// TODO: Document test_delete_strategy_is_idempotent_when_not_configured.
#[tokio::test]
async fn test_delete_strategy_is_idempotent_when_not_configured() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    let (delete_status, delete_body) = send_delete(&app, "/1/strategies/personalization").await;
    assert_eq!(delete_status, StatusCode::OK);
    let deleted_at = delete_body["deletedAt"]
        .as_str()
        .expect("deletedAt should be present");
    assert!(
        chrono::DateTime::parse_from_rfc3339(deleted_at).is_ok(),
        "deletedAt should be RFC3339: {delete_body}"
    );

    let (get_status, _) = send_get(&app, "/1/strategies/personalization").await;
    assert_eq!(get_status, StatusCode::NOT_FOUND);
}

// --- B3: User Profile Endpoints Integration Tests ---

/// Compute a user profile from recorded analytics events and verify facet scores reflect view counts.
#[tokio::test]
async fn test_get_profile_computes_from_insight_events() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());

    let state = make_state_with_analytics(&tmp, Some(Arc::new(AnalyticsQueryEngine::new(config))));
    state.manager.create_tenant("products").unwrap();
    state
        .manager
        .add_documents_sync(
            "products",
            vec![
                make_product_doc("prod-nike", "Nike"),
                make_product_doc("prod-adidas", "Adidas"),
            ],
        )
        .await
        .unwrap();

    let now_ms = chrono::Utc::now().timestamp_millis();
    collector.record_insight(make_view_event("user-123", "prod-nike", now_ms));
    collector.record_insight(make_view_event("user-123", "prod-nike", now_ms));
    collector.record_insight(make_view_event("user-123", "prod-adidas", now_ms));
    collector.flush_all();

    let app = personalization_router(state);
    let (post_status, _) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        valid_strategy(),
    )
    .await;
    assert_eq!(post_status, StatusCode::OK);

    let (status, body) = send_get(&app, "/1/profiles/personalization/user-123").await;
    assert_eq!(status, StatusCode::OK, "expected computed profile: {body}");
    assert_eq!(body["userToken"], "user-123");

    let last_event_at = body["lastEventAt"]
        .as_str()
        .expect("lastEventAt should be present");
    assert!(
        chrono::DateTime::parse_from_rfc3339(last_event_at).is_ok(),
        "lastEventAt should be RFC3339: {body}"
    );

    let nike = body["scores"]["brand"]["Nike"]
        .as_u64()
        .expect("Nike score should exist");
    let adidas = body["scores"]["brand"]["Adidas"]
        .as_u64()
        .expect("Adidas score should exist");
    assert!(
        nike > adidas,
        "Nike should score higher than Adidas: {body}"
    );
    assert!(
        nike <= 20 && adidas <= 20,
        "scores should be normalized to <=20: {body}"
    );
}

/// Direct profile computation and standalone deletion must enter the same
/// per-user ordering owner used by their production handlers.
#[tokio::test]
async fn direct_profile_get_and_standalone_delete_share_user_ordering() {
    use axum::extract::{Path, State};
    use std::future::Future;
    use std::task::Poll;

    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let state = make_state_with_analytics(&tmp, Some(Arc::new(AnalyticsQueryEngine::new(config))));
    state.manager.create_tenant("products").unwrap();
    state
        .manager
        .add_documents_sync("products", vec![make_product_doc("prod-nike", "Nike")])
        .await
        .unwrap();

    let now_ms = chrono::Utc::now().timestamp_millis();
    collector.record_insight(make_view_event("target-user", "prod-nike", now_ms));
    collector.flush_all();

    let app = personalization_router(Arc::clone(&state));
    let (post_status, _) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        valid_strategy(),
    )
    .await;
    assert_eq!(post_status, StatusCode::OK);

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

    let blocker = store.begin_user_operation("target-user").await.unwrap();
    let mut profile_get = Box::pin(get_user_profile(
        State(Arc::clone(&state)),
        Path("target-user".to_string()),
    ));
    let first_get_poll =
        std::future::poll_fn(|cx| Poll::Ready(profile_get.as_mut().poll(cx))).await;
    assert!(matches!(first_get_poll, Poll::Pending));
    assert_eq!(
        probe.admission_count(),
        2,
        "the direct GET compute path must enter the profile operation owner"
    );
    drop(blocker);
    let profile = profile_get.await.unwrap().0;
    assert_eq!(profile["userToken"], "target-user");

    let blocker = store.begin_user_operation("target-user").await.unwrap();
    let mut profile_delete = Box::pin(delete_user_profile(
        State(Arc::clone(&state)),
        Path("target-user".to_string()),
    ));
    let first_delete_poll =
        std::future::poll_fn(|cx| Poll::Ready(profile_delete.as_mut().poll(cx))).await;
    assert!(matches!(first_delete_poll, Poll::Pending));
    assert_eq!(
        probe.admission_count(),
        4,
        "the standalone DELETE path must enter the profile operation owner"
    );
    drop(blocker);
    let _ = profile_delete.await.unwrap();

    assert!(store.load_profile("target-user").unwrap().is_none());
    assert_eq!(store.load_profile("control-user").unwrap(), Some(control));
}

/// Delete a previously cached profile and verify that a subsequent GET returns 404.
#[tokio::test]
async fn test_delete_profile_then_get_returns_404() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let store = PersonalizationProfileStore::new(tmp.path());

    let mut brand_scores = BTreeMap::new();
    brand_scores.insert("Nike".to_string(), 15);
    let mut scores = BTreeMap::new();
    scores.insert("brand".to_string(), brand_scores);
    store
        .save_profile(&PersonalizationProfile {
            user_token: "user-123".to_string(),
            last_event_at: Some(chrono::Utc::now().to_rfc3339()),
            scores,
        })
        .unwrap();

    let app = personalization_router(state);
    let (post_status, _) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        valid_strategy(),
    )
    .await;
    assert_eq!(post_status, StatusCode::OK);

    let (delete_status, delete_body) = send_delete(&app, "/1/profiles/user-123").await;
    assert_eq!(delete_status, StatusCode::OK);
    assert_eq!(delete_body["userToken"], "user-123");
    let deleted_until = delete_body["deletedUntil"]
        .as_str()
        .expect("deletedUntil should be present");
    assert!(
        chrono::DateTime::parse_from_rfc3339(deleted_until).is_ok(),
        "deletedUntil should be RFC3339: {delete_body}"
    );

    let (get_status, _) = send_get(&app, "/1/profiles/personalization/user-123").await;
    assert_eq!(get_status, StatusCode::NOT_FOUND);
}

/// Return 404 when requesting a profile for a user with no recorded events or cached data.
#[tokio::test]
async fn test_get_profile_404_for_nonexistent_user() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let app = personalization_router(state);

    let (post_status, _) = send_json(
        &app,
        Method::POST,
        "/1/strategies/personalization",
        valid_strategy(),
    )
    .await;
    assert_eq!(post_status, StatusCode::OK);

    let (status, body) = send_get(&app, "/1/profiles/personalization/no_such_user").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "expected 404: {body}");
}
