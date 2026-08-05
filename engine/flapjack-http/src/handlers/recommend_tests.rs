use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use chrono::Utc;
use flapjack::{
    analytics::schema::InsightEvent as SchemaInsightEvent,
    analytics::{AnalyticsCollector, AnalyticsConfig, AnalyticsQueryEngine},
    index::settings::IndexSettings,
    types::{Document, FieldValue},
};
use std::collections::HashMap;
use tempfile::TempDir;
use tower::ServiceExt;

fn recommend_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/1/indexes/:_wildcard/recommendations", post(recommend))
        .with_state(state)
}

/// Send a POST request to the recommend endpoint and return the status code and parsed JSON body.
///
/// # Arguments
///
/// * `app` - The Axum router under test.
/// * `body` - JSON request body conforming to `RecommendBatchRequest`.
async fn post_recommend(app: &Router, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/*/recommendations")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    (status, json)
}

// ── C0: Validation tests (RED) ──────────────────────────────────────

/// Verify that an unsupported model name is rejected with 400 Bad Request.
#[tokio::test]
async fn recommend_unsupported_model_returns_400() {
    let tmp = TempDir::new().unwrap();
    let app = recommend_router(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

    let (status, _) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "magic-model",
                "threshold": 50
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Verify that omitting the required `threshold` field is rejected with 400 Bad Request.
#[tokio::test]
async fn recommend_missing_threshold_returns_400() {
    let tmp = TempDir::new().unwrap();
    let app = recommend_router(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

    let (status, _) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-items"
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Verify that a threshold value exceeding 100 is rejected with 400 Bad Request.
#[tokio::test]
async fn recommend_threshold_over_100_returns_400() {
    let tmp = TempDir::new().unwrap();
    let app = recommend_router(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

    let (status, _) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-items",
                "threshold": 101
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Verify that `maxRecommendations` above the maximum allowed value is rejected with 400 Bad Request.
#[tokio::test]
async fn recommend_max_recommendations_over_30_returns_400() {
    let tmp = TempDir::new().unwrap();
    let app = recommend_router(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

    let (status, _) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-items",
                "threshold": 50,
                "maxRecommendations": 31
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Verify that `maxRecommendations` of zero is rejected with 400 Bad Request.
#[tokio::test]
async fn recommend_max_recommendations_zero_returns_400() {
    let tmp = TempDir::new().unwrap();
    let app = recommend_router(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

    let (status, _) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-items",
                "threshold": 50,
                "maxRecommendations": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Verify that omitting `objectID` for the `related-products` model is rejected with 400 Bad Request.
#[tokio::test]
async fn recommend_missing_object_id_for_related_products_returns_400() {
    let tmp = TempDir::new().unwrap();
    let app = recommend_router(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

    let (status, _) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "threshold": 50
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Verify that a whitespace-only `objectID` for the `related-products` model is rejected with 400 Bad Request.
#[tokio::test]
async fn recommend_empty_object_id_for_related_products_returns_400() {
    let tmp = TempDir::new().unwrap();
    let app = recommend_router(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

    let (status, _) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "   ",
                "threshold": 50
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Verify that omitting `objectID` for the `bought-together` model is rejected with 400 Bad Request.
#[tokio::test]
async fn recommend_missing_object_id_for_bought_together_returns_400() {
    let tmp = TempDir::new().unwrap();
    let app = recommend_router(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

    let (status, _) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "bought-together",
                "threshold": 50
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Verify that omitting `facetName` for the `trending-facets` model is rejected with 400 Bad Request.
#[tokio::test]
async fn recommend_missing_facet_name_for_trending_facets_returns_400() {
    let tmp = TempDir::new().unwrap();
    let app = recommend_router(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

    let (status, _) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-facets",
                "threshold": 50
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Verify that a whitespace-only `facetName` for the `trending-facets` model is rejected with 400 Bad Request.
#[tokio::test]
async fn recommend_empty_facet_name_for_trending_facets_returns_400() {
    let tmp = TempDir::new().unwrap();
    let app = recommend_router(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

    let (status, _) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-facets",
                "threshold": 50,
                "facetName": " "
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

fn analytics_config(tmp: &TempDir) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 3600,
        flush_size: 100_000,
        retention_days: 90,
    }
}

/// Create an `AppState` with a fully configured analytics subsystem backed by the given temp directory.
///
/// # Returns
///
/// A tuple of the shared `AppState` and the `AnalyticsCollector` used to record test events.
fn make_test_state_with_analytics(tmp: &TempDir) -> (Arc<AppState>, Arc<AnalyticsCollector>) {
    let analytics_config = analytics_config(tmp);
    let collector = AnalyticsCollector::new(analytics_config.clone());
    let analytics_engine = Arc::new(AnalyticsQueryEngine::new(analytics_config));
    (
        crate::test_helpers::TestStateBuilder::new(tmp)
            .with_analytics_engine(analytics_engine)
            .build_shared(),
        collector,
    )
}

async fn insert_recommend_docs(state: &Arc<AppState>, index_name: &str, docs: Vec<Document>) {
    state.manager.create_tenant(index_name).unwrap();
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();
}

fn save_settings(state: &Arc<AppState>, index_name: &str, settings: &IndexSettings) {
    let dir = state.manager.base_path.join(index_name);
    std::fs::create_dir_all(&dir).unwrap();
    settings.save(dir.join("settings.json")).unwrap();
    state.manager.invalidate_settings_cache(index_name);
}

fn text_searchable_settings() -> IndexSettings {
    IndexSettings {
        searchable_attributes: Some(vec!["name".to_string()]),
        ..Default::default()
    }
}

#[cfg(feature = "vector-search")]
fn user_provided_embedder_settings(dimensions: usize) -> IndexSettings {
    let mut embedders = HashMap::new();
    embedders.insert(
        "default".to_string(),
        serde_json::json!({
            "source": "userProvided",
            "dimensions": dimensions
        }),
    );
    IndexSettings {
        embedders: Some(embedders),
        ..Default::default()
    }
}

#[cfg(feature = "vector-search")]
fn embedder_and_text_searchable_settings(dimensions: usize) -> IndexSettings {
    let mut settings = text_searchable_settings();
    settings.embedders = user_provided_embedder_settings(dimensions).embedders;
    settings
}

/// Build a `Document` with a name field and an optional 3-dimensional vector embedding under the `default` embedder key.
#[cfg(feature = "vector-search")]
fn make_vector_doc(id: &str, name: &str, vector: Option<[f32; 3]>) -> Document {
    let mut fields = HashMap::from([("name".to_string(), FieldValue::Text(name.to_string()))]);
    if let Some([x, y, z]) = vector {
        fields.insert(
            "_vectors".to_string(),
            FieldValue::Object(HashMap::from([(
                "default".to_string(),
                FieldValue::Array(vec![
                    FieldValue::Float(x as f64),
                    FieldValue::Float(y as f64),
                    FieldValue::Float(z as f64),
                ]),
            )])),
        );
    }
    Document {
        id: id.to_string(),
        fields,
    }
}

/// Build a text-only document from the named fields used by recommendation fixtures.
fn make_text_doc(id: &str, fields: &[(&str, &str)]) -> Document {
    Document {
        id: id.to_string(),
        fields: fields
            .iter()
            .map(|(name, value)| ((*name).to_string(), FieldValue::Text((*value).to_string())))
            .collect(),
    }
}

const LOOKING_SIMILAR_TEXT_CORPUS: &[(&str, &str)] = &[
    (
        "seed",
        "Wireless Bluetooth Headphones with active noise cancelling",
    ),
    (
        "near",
        "Wireless Bluetooth Headphones with active noise cancelling travel",
    ),
    ("mid", "Wireless Bluetooth Headphones"),
    ("zero_overlap", "Ceramic coffee grinder"),
];

fn shared_vocabulary_text_docs() -> Vec<Document> {
    LOOKING_SIMILAR_TEXT_CORPUS
        .iter()
        .map(|(id, name)| make_text_doc(id, &[("name", name)]))
        .collect()
}

#[cfg(feature = "vector-search")]
fn zero_overlap_nearest_vector_docs() -> Vec<Document> {
    LOOKING_SIMILAR_TEXT_CORPUS
        .iter()
        .map(|(id, name)| {
            let vector = match *id {
                "seed" => Some([1.0, 0.0, 0.0]),
                "zero_overlap" => Some([0.99, 0.01, 0.0]),
                "near" => Some([0.0, 1.0, 0.0]),
                "mid" => Some([0.0, 0.0, 1.0]),
                _ => None,
            };
            make_vector_doc(id, name, vector)
        })
        .collect()
}

async fn text_looking_similar_app(tmp: &TempDir) -> Router {
    let state = crate::test_helpers::TestStateBuilder::new(tmp).build_shared();
    insert_recommend_docs(&state, "products", shared_vocabulary_text_docs()).await;
    save_settings(&state, "products", &text_searchable_settings());
    recommend_router(state)
}

fn assert_term_fallback_hits(body: &serde_json::Value, expected_object_ids: Vec<&str>) {
    let hits = body["results"][0]["hits"].as_array().expect("hits array");
    let object_ids = hits
        .iter()
        .map(|hit| hit["objectID"].as_str().expect("objectID"))
        .collect::<Vec<_>>();
    assert!(!object_ids.contains(&"seed"));
    assert!(!object_ids.contains(&"zero_overlap"));
    assert_eq!(object_ids, expected_object_ids);

    let scores = hits
        .iter()
        .map(|hit| hit["_score"].as_u64().expect("numeric _score"))
        .collect::<Vec<_>>();
    assert_eq!(scores.first().copied(), Some(100));
    assert!(scores.windows(2).all(|pair| pair[0] >= pair[1]));
    assert!(scores.iter().all(|score| *score <= 100));
}

/// Build a conversion `InsightEvent` with the given index, user token, object ID, event name, and timestamp.
fn make_conversion_event(
    index_name: &str,
    user_token: &str,
    object_id: &str,
    event_name: &str,
    event_timestamp_ms: i64,
) -> SchemaInsightEvent {
    SchemaInsightEvent {
        event_type: "conversion".to_string(),
        event_subtype: None,
        event_name: event_name.to_string(),
        index: index_name.to_string(),
        user_token: user_token.to_string(),
        authenticated_user_token: None,
        query_id: None,
        object_ids: vec![object_id.to_string()],
        object_ids_alt: vec![],
        positions: None,
        timestamp: Some(event_timestamp_ms),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

fn record_conversion_events(collector: &AnalyticsCollector, events: Vec<SchemaInsightEvent>) {
    for event in events {
        collector.record_insight(event);
    }
    collector.flush_insights();
}

/// Verify that a batched request returns one result per sub-request in the original request order.
#[tokio::test]
async fn recommend_batched_preserves_ordering() {
    let tmp = TempDir::new().unwrap();
    let (state, _) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state);

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [
                {
                    "indexName": "products",
                    "model": "trending-items",
                    "threshold": 0
                },
                {
                    "indexName": "products",
                    "model": "trending-items",
                    "threshold": 50
                }
            ]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "Should return one result per request");
    // Both should have hits array and processingTimeMS
    for result in results {
        assert!(result["hits"].is_array());
        assert!(result["processingTimeMS"].is_number());
    }
}

/// Verify that trending-items results are ordered by descending score computed from conversion frequency weighted by recency, with scores normalized to 0–100.
#[tokio::test]
async fn recommend_trending_items_orders_by_frequency_and_recency() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "sku-001".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Alpha".to_string()),
                )]),
            },
            Document {
                id: "sku-002".to_string(),
                fields: HashMap::from([("name".to_string(), FieldValue::Text("Beta".to_string()))]),
            },
            Document {
                id: "sku-003".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Gamma".to_string()),
                )]),
            },
            Document {
                id: "sku-004".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Delta".to_string()),
                )]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    let day_ms = 24 * 60 * 60 * 1000;
    record_conversion_events(
        &collector,
        vec![
            // sku-001: highest score (3 relatively recent events)
            make_conversion_event("products", "user-a", "sku-001", "conversion", now_ms),
            make_conversion_event(
                "products",
                "user-b",
                "sku-001",
                "conversion",
                now_ms - day_ms,
            ),
            make_conversion_event(
                "products",
                "user-c",
                "sku-001",
                "conversion",
                now_ms - 2 * day_ms,
            ),
            // sku-002: second highest (2 events)
            make_conversion_event(
                "products",
                "user-a",
                "sku-002",
                "conversion",
                now_ms - 2 * day_ms,
            ),
            make_conversion_event(
                "products",
                "user-d",
                "sku-002",
                "conversion",
                now_ms - 3 * day_ms,
            ),
            // sku-003: lower score
            make_conversion_event(
                "products",
                "user-e",
                "sku-003",
                "conversion",
                now_ms - 5 * day_ms,
            ),
            // sku-004: lowest score
            make_conversion_event(
                "products",
                "user-f",
                "sku-004",
                "conversion",
                now_ms - 6 * day_ms,
            ),
        ],
    );

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-items",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"]
        .as_array()
        .expect("response should contain hits");

    let object_ids: Vec<&str> = hits
        .iter()
        .map(|hit| {
            hit["objectID"]
                .as_str()
                .expect("objectID should be present")
        })
        .collect();
    let scores: Vec<u32> = hits
        .iter()
        .map(|hit| hit["_score"].as_u64().expect("score should be present") as u32)
        .collect();

    assert_eq!(object_ids, vec!["sku-001", "sku-002", "sku-003", "sku-004"]);
    assert!(scores.windows(2).all(|w| w[0] >= w[1]));
    assert!(scores.iter().all(|score| *score <= 100));
}

/// Verify that trending-items respects `facetName` and `facetValue` parameters to filter results to only matching documents.
#[tokio::test]
async fn recommend_trending_items_facet_filtering() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "nike-1".to_string(),
                fields: HashMap::from([
                    (
                        "name".to_string(),
                        FieldValue::Text("Nike Runner".to_string()),
                    ),
                    ("brand".to_string(), FieldValue::Text("Nike".to_string())),
                ]),
            },
            Document {
                id: "nike-2".to_string(),
                fields: HashMap::from([
                    (
                        "name".to_string(),
                        FieldValue::Text("Nike Trainer".to_string()),
                    ),
                    ("brand".to_string(), FieldValue::Text("Nike".to_string())),
                ]),
            },
            Document {
                id: "adidas-1".to_string(),
                fields: HashMap::from([
                    (
                        "name".to_string(),
                        FieldValue::Text("Adidas Runner".to_string()),
                    ),
                    ("brand".to_string(), FieldValue::Text("Adidas".to_string())),
                ]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    record_conversion_events(
        &collector,
        vec![
            make_conversion_event("products", "user-a", "nike-1", "conversion", now_ms),
            make_conversion_event("products", "user-b", "nike-2", "conversion", now_ms),
            make_conversion_event("products", "user-c", "adidas-1", "conversion", now_ms),
        ],
    );

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-items",
                "threshold": 0,
                "facetName": "brand",
                "facetValue": "Nike"
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"]
        .as_array()
        .expect("response should contain hits");
    assert_eq!(hits.len(), 2);

    let object_ids: Vec<&str> = hits
        .iter()
        .map(|hit| {
            hit["objectID"]
                .as_str()
                .expect("objectID should be present")
        })
        .collect();
    assert_eq!(object_ids, vec!["nike-1", "nike-2"]);
}

/// Regression: maxRecommendations must not truncate candidates BEFORE facet filtering.
/// Items lexicographically prior to the facet-matching items must not crowd them out.
#[tokio::test]
async fn recommend_trending_items_facet_filter_with_max_recommendations() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    // "aaa-*" docs have no brand → they sort first lexicographically (same score)
    // "zzz-*" docs have brand=Nike → they sort after aaa-* lexicographically
    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "aaa-1".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Anon 1".to_string()),
                )]),
            },
            Document {
                id: "aaa-2".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Anon 2".to_string()),
                )]),
            },
            Document {
                id: "zzz-nike-1".to_string(),
                fields: HashMap::from([
                    ("name".to_string(), FieldValue::Text("Nike Z1".to_string())),
                    ("brand".to_string(), FieldValue::Text("Nike".to_string())),
                ]),
            },
            Document {
                id: "zzz-nike-2".to_string(),
                fields: HashMap::from([
                    ("name".to_string(), FieldValue::Text("Nike Z2".to_string())),
                    ("brand".to_string(), FieldValue::Text("Nike".to_string())),
                ]),
            },
        ],
    )
    .await;

    // All 4 docs get equal conversion scores
    let now_ms = Utc::now().timestamp_millis();
    record_conversion_events(
        &collector,
        vec![
            make_conversion_event("products", "user-a", "aaa-1", "conversion", now_ms),
            make_conversion_event("products", "user-b", "aaa-2", "conversion", now_ms),
            make_conversion_event("products", "user-c", "zzz-nike-1", "conversion", now_ms),
            make_conversion_event("products", "user-d", "zzz-nike-2", "conversion", now_ms),
        ],
    );

    // maxRecommendations=2: must return 2 Nike items, not 0 (which would happen
    // if truncation fired before facet filtering, leaving only aaa-1/aaa-2)
    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-items",
                "threshold": 0,
                "facetName": "brand",
                "facetValue": "Nike",
                "maxRecommendations": 2
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"]
        .as_array()
        .expect("response should contain hits");
    assert_eq!(
        hits.len(),
        2,
        "should return both Nike items despite non-Nike items ranking first lex"
    );

    let object_ids: Vec<&str> = hits
        .iter()
        .map(|hit| {
            hit["objectID"]
                .as_str()
                .expect("objectID should be present")
        })
        .collect();
    assert!(object_ids.contains(&"zzz-nike-1"));
    assert!(object_ids.contains(&"zzz-nike-2"));
}

/// Verify that trending-items excludes hits below the threshold and caps results at `maxRecommendations`.
#[tokio::test]
async fn recommend_trending_items_respects_threshold_and_max_recommendations() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "p1".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("First".to_string()),
                )]),
            },
            Document {
                id: "p2".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Second".to_string()),
                )]),
            },
            Document {
                id: "p3".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Third".to_string()),
                )]),
            },
            Document {
                id: "p4".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Fourth".to_string()),
                )]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    let day_ms = 24 * 60 * 60 * 1000;
    record_conversion_events(
        &collector,
        vec![
            make_conversion_event("products", "user-1", "p1", "conversion", now_ms),
            make_conversion_event("products", "user-2", "p1", "conversion", now_ms),
            make_conversion_event("products", "user-3", "p1", "conversion", now_ms),
            make_conversion_event("products", "user-1", "p2", "conversion", now_ms - day_ms),
            make_conversion_event("products", "user-2", "p2", "conversion", now_ms - day_ms),
            make_conversion_event(
                "products",
                "user-3",
                "p3",
                "conversion",
                now_ms - 6 * day_ms,
            ),
            make_conversion_event(
                "products",
                "user-4",
                "p4",
                "conversion",
                now_ms - 6 * day_ms,
            ),
        ],
    );

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-items",
                "threshold": 40,
                "maxRecommendations": 2
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"]
        .as_array()
        .expect("response should contain hits");
    assert_eq!(hits.len(), 2);
    let object_ids: Vec<&str> = hits
        .iter()
        .map(|hit| {
            hit["objectID"]
                .as_str()
                .expect("objectID should be present")
        })
        .collect();
    assert_eq!(object_ids, vec!["p1", "p2"]);
    assert!(hits
        .iter()
        .all(|hit| hit["_score"].as_u64().expect("score should be present") >= 40));
}

/// Verify that both standard and virtual replicas resolve to their primary index for recommendation data, producing identical results.
#[tokio::test]
async fn recommend_replicas_follow_primary_scope() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    let primary_index_name = "products";
    let standard_replica_name = "products_standard";
    let virtual_replica_name = "products_recommend_replica_virtual";

    // Seed primary scope with three products and deterministic trending scores.
    insert_recommend_docs(
        &state,
        primary_index_name,
        vec![
            Document {
                id: "seed-1".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Seed 1".to_string()),
                )]),
            },
            Document {
                id: "seed-2".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Seed 2".to_string()),
                )]),
            },
            Document {
                id: "seed-3".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Seed 3".to_string()),
                )]),
            },
        ],
    )
    .await;

    save_settings(
        &state,
        primary_index_name,
        &IndexSettings {
            replicas: Some(vec![
                standard_replica_name.to_string(),
                format!("virtual({virtual_replica_name})"),
            ]),
            ..Default::default()
        },
    );
    // mirror docs into the standard replica so replica-level indexing has a local copy
    insert_recommend_docs(
        &state,
        standard_replica_name,
        vec![
            Document {
                id: "seed-1".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Seed 1".to_string()),
                )]),
            },
            Document {
                id: "seed-2".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Seed 2".to_string()),
                )]),
            },
            Document {
                id: "seed-3".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Seed 3".to_string()),
                )]),
            },
        ],
    )
    .await;
    save_settings(
        &state,
        standard_replica_name,
        &IndexSettings {
            primary: Some(primary_index_name.to_string()),
            ..Default::default()
        },
    );
    save_settings(
        &state,
        virtual_replica_name,
        &IndexSettings {
            primary: Some(primary_index_name.to_string()),
            ..Default::default()
        },
    );

    let now_ms = chrono::Utc::now().timestamp_millis();
    record_conversion_events(
        &collector,
        vec![
            make_conversion_event(primary_index_name, "user-a", "seed-1", "conversion", now_ms),
            make_conversion_event(primary_index_name, "user-a", "seed-1", "conversion", now_ms),
            make_conversion_event(primary_index_name, "user-a", "seed-1", "conversion", now_ms),
            make_conversion_event(primary_index_name, "user-b", "seed-2", "conversion", now_ms),
            make_conversion_event(primary_index_name, "user-b", "seed-2", "conversion", now_ms),
            make_conversion_event(primary_index_name, "user-c", "seed-3", "conversion", now_ms),
        ],
    );

    let request = serde_json::json!({
        "requests": [{
            "indexName": primary_index_name,
            "model": "trending-items",
            "threshold": 0
        }]
    });
    let (primary_status, primary_body) = post_recommend(&app, request.clone()).await;
    assert_eq!(primary_status, StatusCode::OK);
    let primary_hits = primary_body["results"][0]["hits"]
        .as_array()
        .expect("response should contain hits");
    assert_eq!(primary_hits.len(), 3);

    let standard_request = serde_json::json!({
        "requests": [{
            "indexName": standard_replica_name,
            "model": "trending-items",
            "threshold": 0
        }]
    });
    let (standard_status, standard_body) = post_recommend(&app, standard_request).await;
    assert_eq!(standard_status, StatusCode::OK);
    let standard_hits = standard_body["results"][0]["hits"]
        .as_array()
        .expect("response should contain hits");

    let virtual_request = serde_json::json!({
        "requests": [{
            "indexName": virtual_replica_name,
            "model": "trending-items",
            "threshold": 0
        }]
    });
    let (virtual_status, virtual_body) = post_recommend(&app, virtual_request).await;
    assert_eq!(virtual_status, StatusCode::OK);
    let virtual_hits = virtual_body["results"][0]["hits"]
        .as_array()
        .expect("response should contain hits");

    assert_eq!(
        primary_hits, standard_hits,
        "standard replica should produce same recommendation behavior as primary"
    );
    assert_eq!(
        primary_hits, virtual_hits,
        "virtual replica should produce same recommendation behavior as primary"
    );
}

// ── C2: Trending Facets tests (RED) ──────────────────────────────

/// Verify that trending-facets returns facet value hits with `facetName`, `facetValue`, and `_score` fields, ordered by descending score.
#[tokio::test]
async fn recommend_trending_facets_returns_top_facet_hits_with_scores() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "p-nike-1".to_string(),
                fields: HashMap::from([
                    (
                        "name".to_string(),
                        FieldValue::Text("Nike Alpha".to_string()),
                    ),
                    ("brand".to_string(), FieldValue::Facet("Nike".to_string())),
                ]),
            },
            Document {
                id: "p-nike-2".to_string(),
                fields: HashMap::from([
                    (
                        "name".to_string(),
                        FieldValue::Text("Nike Beta".to_string()),
                    ),
                    ("brand".to_string(), FieldValue::Facet("Nike".to_string())),
                ]),
            },
            Document {
                id: "p-adidas-1".to_string(),
                fields: HashMap::from([
                    (
                        "name".to_string(),
                        FieldValue::Text("Adidas Speed".to_string()),
                    ),
                    ("brand".to_string(), FieldValue::Facet("Adidas".to_string())),
                ]),
            },
            Document {
                id: "p-puma-1".to_string(),
                fields: HashMap::from([
                    (
                        "name".to_string(),
                        FieldValue::Text("Puma Core".to_string()),
                    ),
                    ("brand".to_string(), FieldValue::Facet("Puma".to_string())),
                ]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    record_conversion_events(
        &collector,
        vec![
            make_conversion_event("products", "user-a", "p-nike-1", "conversion", now_ms),
            make_conversion_event("products", "user-b", "p-nike-2", "conversion", now_ms),
            make_conversion_event("products", "user-c", "p-adidas-1", "conversion", now_ms),
            make_conversion_event("products", "user-d", "p-puma-1", "conversion", now_ms),
        ],
    );

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-facets",
                "threshold": 0,
                "facetName": "brand"
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"]
        .as_array()
        .expect("response should contain hits");

    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0]["facetName"].as_str(), Some("brand"));

    let values: Vec<&str> = hits
        .iter()
        .map(|hit| {
            hit["facetValue"]
                .as_str()
                .expect("facetValue should be present")
        })
        .collect();
    assert_eq!(values, vec!["Nike", "Adidas", "Puma"]);

    let scores: Vec<u32> = hits
        .iter()
        .map(|hit| hit["_score"].as_u64().expect("score should be present") as u32)
        .collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]));
}

/// Verify that trending-facets returns an empty hits array when the requested facet name does not exist in the index.
#[tokio::test]
async fn recommend_trending_facets_unknown_facet_name_returns_empty_hits() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    insert_recommend_docs(
        &state,
        "products",
        vec![Document {
            id: "p1".to_string(),
            fields: HashMap::from([
                (
                    "name".to_string(),
                    FieldValue::Text("Blue Shirt".to_string()),
                ),
                ("brand".to_string(), FieldValue::Facet("Acme".to_string())),
            ]),
        }],
    )
    .await;

    record_conversion_events(
        &collector,
        vec![make_conversion_event(
            "products",
            "user-a",
            "p1",
            "conversion",
            Utc::now().timestamp_millis(),
        )],
    );

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-facets",
                "threshold": 0,
                "facetName": "not-a-real-facet"
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"]
        .as_array()
        .expect("response should contain hits");
    assert!(hits.is_empty());
}

/// Verify that trending-facets excludes facet values below the threshold and caps results at `maxRecommendations`.
#[tokio::test]
async fn recommend_trending_facets_respects_threshold_and_max_recommendations() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "p-nike-1".to_string(),
                fields: HashMap::from([
                    (
                        "name".to_string(),
                        FieldValue::Text("Nike Alpha".to_string()),
                    ),
                    ("brand".to_string(), FieldValue::Facet("Nike".to_string())),
                ]),
            },
            Document {
                id: "p-nike-2".to_string(),
                fields: HashMap::from([
                    (
                        "name".to_string(),
                        FieldValue::Text("Nike Beta".to_string()),
                    ),
                    ("brand".to_string(), FieldValue::Facet("Nike".to_string())),
                ]),
            },
            Document {
                id: "p-adidas-1".to_string(),
                fields: HashMap::from([
                    (
                        "name".to_string(),
                        FieldValue::Text("Adidas Speed".to_string()),
                    ),
                    ("brand".to_string(), FieldValue::Facet("Adidas".to_string())),
                ]),
            },
            Document {
                id: "p-puma-1".to_string(),
                fields: HashMap::from([
                    (
                        "name".to_string(),
                        FieldValue::Text("Puma Core".to_string()),
                    ),
                    ("brand".to_string(), FieldValue::Facet("Puma".to_string())),
                ]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    record_conversion_events(
        &collector,
        vec![
            make_conversion_event("products", "user-a", "p-nike-1", "conversion", now_ms),
            make_conversion_event("products", "user-b", "p-nike-2", "conversion", now_ms),
            make_conversion_event("products", "user-c", "p-nike-2", "conversion", now_ms),
            make_conversion_event("products", "user-d", "p-adidas-1", "conversion", now_ms),
            make_conversion_event("products", "user-e", "p-adidas-1", "conversion", now_ms),
            make_conversion_event("products", "user-f", "p-puma-1", "conversion", now_ms),
        ],
    );

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "trending-facets",
                "facetName": "brand",
                "threshold": 60,
                "maxRecommendations": 2
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"]
        .as_array()
        .expect("response should contain hits");
    let values: Vec<&str> = hits
        .iter()
        .map(|hit| {
            hit["facetValue"]
                .as_str()
                .expect("facetValue should be present")
        })
        .collect();
    let scores: Vec<u32> = hits
        .iter()
        .map(|hit| hit["_score"].as_u64().expect("score should be present") as u32)
        .collect();

    assert_eq!(hits.len(), 2);
    assert_eq!(values, vec!["Nike", "Adidas"]);
    assert!(scores.iter().all(|score| *score >= 60));
    assert!(scores.windows(2).all(|w| w[0] >= w[1]));
}

// ── C3: Related Products tests (RED) ─────────────────────────────────────

/// Build a click `InsightEvent` with the given index, user token, object ID, and timestamp.
fn make_click_event(
    index_name: &str,
    user_token: &str,
    object_id: &str,
    event_timestamp_ms: i64,
) -> SchemaInsightEvent {
    SchemaInsightEvent {
        event_type: "click".to_string(),
        event_subtype: None,
        event_name: "Clicked Item".to_string(),
        index: index_name.to_string(),
        user_token: user_token.to_string(),
        authenticated_user_token: None,
        query_id: None,
        object_ids: vec![object_id.to_string()],
        object_ids_alt: vec![],
        positions: None,
        timestamp: Some(event_timestamp_ms),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

/// Build a purchase `InsightEvent` (conversion with `purchase` subtype) with the given index, user token, object ID, and timestamp.
fn make_purchase_event(
    index_name: &str,
    user_token: &str,
    object_id: &str,
    event_timestamp_ms: i64,
) -> SchemaInsightEvent {
    SchemaInsightEvent {
        event_type: "conversion".to_string(),
        event_subtype: Some("purchase".to_string()),
        event_name: "Purchased Item".to_string(),
        index: index_name.to_string(),
        user_token: user_token.to_string(),
        authenticated_user_token: None,
        query_id: None,
        object_ids: vec![object_id.to_string()],
        object_ids_alt: vec![],
        positions: None,
        timestamp: Some(event_timestamp_ms),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

/// Verify that related-products ranks co-occurring items by session overlap count, excludes the seed, and normalizes scores to 0–100.
#[tokio::test]
async fn recommend_related_products_cooccurrence_ranking() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    // Insert products p1-p4
    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "p1".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 1".to_string()),
                )]),
            },
            Document {
                id: "p2".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 2".to_string()),
                )]),
            },
            Document {
                id: "p3".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 3".to_string()),
                )]),
            },
            Document {
                id: "p4".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 4".to_string()),
                )]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    // User A interacts with [p1, p2, p3], User B interacts with [p1, p2, p4]
    record_conversion_events(
        &collector,
        vec![
            make_conversion_event("products", "user-a", "p1", "conversion", now_ms),
            make_conversion_event("products", "user-a", "p2", "conversion", now_ms),
            make_conversion_event("products", "user-a", "p3", "conversion", now_ms),
            make_conversion_event("products", "user-b", "p1", "conversion", now_ms),
            make_conversion_event("products", "user-b", "p2", "conversion", now_ms),
            make_conversion_event("products", "user-b", "p4", "conversion", now_ms),
        ],
    );

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "p1",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");

    let object_ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();
    let scores: Vec<u32> = hits
        .iter()
        .map(|h| h["_score"].as_u64().expect("_score") as u32)
        .collect();

    // p1 must be excluded (it's the seed)
    assert!(
        !object_ids.contains(&"p1"),
        "seed objectID must not appear in results"
    );
    // p2 co-occurs with both users → highest score
    assert_eq!(
        object_ids[0], "p2",
        "p2 should be ranked first (co-occurs with both users)"
    );
    // p3 and p4 each co-occur with one user → same score, lexicographic tie-breaking
    assert_eq!(object_ids[1], "p3");
    assert_eq!(object_ids[2], "p4");
    // Scores must be descending and in 0-100 range
    assert!(scores.windows(2).all(|w| w[0] >= w[1]));
    assert!(scores.iter().all(|s| *s <= 100));
    assert_eq!(scores[0], 100);
}

/// Verify that related-products excludes the seed objectID and filters out items whose co-occurrence score falls below the threshold.
#[tokio::test]
async fn recommend_related_products_excludes_seed_and_applies_threshold() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "seed".to_string(),
                fields: HashMap::from([("name".to_string(), FieldValue::Text("Seed".to_string()))]),
            },
            Document {
                id: "high".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("High Score".to_string()),
                )]),
            },
            Document {
                id: "low".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Low Score".to_string()),
                )]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    // "high" co-occurs with seed in both users; "low" only in one user
    record_conversion_events(
        &collector,
        vec![
            make_conversion_event("products", "user-a", "seed", "conversion", now_ms),
            make_conversion_event("products", "user-a", "high", "conversion", now_ms),
            make_conversion_event("products", "user-b", "seed", "conversion", now_ms),
            make_conversion_event("products", "user-b", "high", "conversion", now_ms),
            make_conversion_event("products", "user-c", "seed", "conversion", now_ms),
            make_conversion_event("products", "user-c", "low", "conversion", now_ms),
        ],
    );

    // threshold=80 should include "high" (score=100) but exclude "low" (score=50)
    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "seed",
                "threshold": 80
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");

    let object_ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();

    assert!(
        !object_ids.contains(&"seed"),
        "seed must be excluded from results"
    );
    assert!(
        object_ids.contains(&"high"),
        "high-score item should be included"
    );
    assert!(
        !object_ids.contains(&"low"),
        "low-score item below threshold should be excluded"
    );
    assert!(hits.iter().all(|h| h["_score"].as_u64().unwrap() >= 80));
}

/// Verify that related-products ignores co-occurrence events older than the lookback window (30 days).
#[tokio::test]
async fn recommend_related_products_ignores_events_beyond_lookback_window() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "seed".to_string(),
                fields: HashMap::from([("name".to_string(), FieldValue::Text("Seed".to_string()))]),
            },
            Document {
                id: "recent".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Recent".to_string()),
                )]),
            },
            Document {
                id: "old".to_string(),
                fields: HashMap::from([("name".to_string(), FieldValue::Text("Old".to_string()))]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    let day_ms: i64 = 24 * 60 * 60 * 1000;
    // CO_OCCURRENCE_LOOKBACK_DAYS = 30; events older than 30 days are excluded
    let old_ts = now_ms - 31 * day_ms; // 31 days ago → beyond window
    let recent_ts = now_ms - day_ms; // 1 day ago → within window

    record_conversion_events(
        &collector,
        vec![
            // "old" co-occurs with seed only in old events (beyond window)
            make_conversion_event("products", "user-a", "seed", "conversion", old_ts),
            make_conversion_event("products", "user-a", "old", "conversion", old_ts),
            // "recent" co-occurs with seed in recent events (within window)
            make_conversion_event("products", "user-b", "seed", "conversion", recent_ts),
            make_conversion_event("products", "user-b", "recent", "conversion", recent_ts),
        ],
    );

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "seed",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");
    let object_ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();

    assert!(object_ids.contains(&"recent"), "recent item should appear");
    assert!(
        !object_ids.contains(&"old"),
        "item from old events (beyond lookback) should not appear"
    );
}

// ── C4: Bought Together tests (RED) ──────────────────────────────────────

/// Verify that bought-together computes co-occurrence from purchase events only, excludes the seed, and ranks by descending normalized score.
#[tokio::test]
async fn recommend_bought_together_purchase_cooccurrence() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "p1".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 1".to_string()),
                )]),
            },
            Document {
                id: "p2".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 2".to_string()),
                )]),
            },
            Document {
                id: "p3".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 3".to_string()),
                )]),
            },
            Document {
                id: "p4".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 4".to_string()),
                )]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    // User A purchases [p1, p2, p3], User B purchases [p1, p2, p4]
    record_conversion_events(
        &collector,
        vec![
            make_purchase_event("products", "user-a", "p1", now_ms),
            make_purchase_event("products", "user-a", "p2", now_ms),
            make_purchase_event("products", "user-a", "p3", now_ms),
            make_purchase_event("products", "user-b", "p1", now_ms),
            make_purchase_event("products", "user-b", "p2", now_ms),
            make_purchase_event("products", "user-b", "p4", now_ms),
        ],
    );

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "bought-together",
                "objectID": "p1",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");

    let object_ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();
    let scores: Vec<u32> = hits
        .iter()
        .map(|h| h["_score"].as_u64().expect("_score") as u32)
        .collect();

    // p1 is seed — excluded
    assert!(
        !object_ids.contains(&"p1"),
        "seed must not appear in results"
    );
    // p2 bought with p1 by both users → highest score
    assert_eq!(object_ids[0], "p2", "p2 should be ranked first");
    // Scores descending, in 0-100 range
    assert!(scores.windows(2).all(|w| w[0] >= w[1]));
    assert!(scores.iter().all(|s| *s <= 100));
    assert_eq!(scores[0], 100);
}

/// Verify that bought-together ignores click and non-purchase conversion events, returning only items that share purchase sessions with the seed.
#[tokio::test]
async fn recommend_bought_together_ignores_non_purchase_events() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "seed".to_string(),
                fields: HashMap::from([("name".to_string(), FieldValue::Text("Seed".to_string()))]),
            },
            Document {
                id: "purchased".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Purchased".to_string()),
                )]),
            },
            Document {
                id: "clicked".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Clicked".to_string()),
                )]),
            },
            Document {
                id: "converted".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Converted Non-Purchase".to_string()),
                )]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    record_conversion_events(
        &collector,
        vec![
            // Only "purchased" has purchase subtype — should appear in results
            make_purchase_event("products", "user-a", "seed", now_ms),
            make_purchase_event("products", "user-a", "purchased", now_ms),
            // Click events for seed + clicked — should be ignored for bought-together
            make_click_event("products", "user-b", "seed", now_ms),
            make_click_event("products", "user-b", "clicked", now_ms),
            // Non-purchase conversion events for seed + converted — should be ignored
            make_conversion_event("products", "user-c", "seed", "conversion", now_ms),
            make_conversion_event("products", "user-c", "converted", "conversion", now_ms),
        ],
    );

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "bought-together",
                "objectID": "seed",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");
    let object_ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();

    assert!(
        object_ids.contains(&"purchased"),
        "purchase item should appear"
    );
    assert!(
        !object_ids.contains(&"clicked"),
        "click-only item must not appear"
    );
    assert!(
        !object_ids.contains(&"converted"),
        "non-purchase conversion must not appear"
    );
}

/// Verify that bought-together excludes items below the threshold and caps results at `maxRecommendations`.
#[tokio::test]
async fn recommend_bought_together_respects_threshold_and_max_recommendations() {
    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());

    insert_recommend_docs(
        &state,
        "products",
        vec![
            Document {
                id: "seed".to_string(),
                fields: HashMap::from([("name".to_string(), FieldValue::Text("Seed".to_string()))]),
            },
            Document {
                id: "freq".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Frequent".to_string()),
                )]),
            },
            Document {
                id: "rare".to_string(),
                fields: HashMap::from([("name".to_string(), FieldValue::Text("Rare".to_string()))]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    // "freq" bought with seed by 2 users → score=100
    // "rare" bought with seed by 1 user → score=50
    record_conversion_events(
        &collector,
        vec![
            make_purchase_event("products", "user-a", "seed", now_ms),
            make_purchase_event("products", "user-a", "freq", now_ms),
            make_purchase_event("products", "user-b", "seed", now_ms),
            make_purchase_event("products", "user-b", "freq", now_ms),
            make_purchase_event("products", "user-c", "seed", now_ms),
            make_purchase_event("products", "user-c", "rare", now_ms),
        ],
    );

    // threshold=80 excludes "rare" (score=50); maxRecommendations=1 limits to "freq"
    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "bought-together",
                "objectID": "seed",
                "threshold": 80,
                "maxRecommendations": 1
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["objectID"].as_str(), Some("freq"));
    assert_eq!(hits[0]["_score"].as_u64().unwrap(), 100);
}

/// Verify that looking-similar returns documents ordered by descending vector cosine similarity to the seed, excluding the seed itself.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn recommend_looking_similar_orders_by_vector_similarity_and_excludes_seed() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let app = recommend_router(state.clone());
    state.manager.create_tenant("products").unwrap();
    save_settings(&state, "products", &user_provided_embedder_settings(3));

    state
        .manager
        .add_documents_sync(
            "products",
            vec![
                make_vector_doc("seed", "Seed", Some([1.0, 0.0, 0.0])),
                make_vector_doc("near", "Near", Some([0.99, 0.01, 0.0])),
                make_vector_doc("mid", "Mid", Some([0.7, 0.3, 0.0])),
                make_vector_doc("far", "Far", Some([0.0, 1.0, 0.0])),
            ],
        )
        .await
        .unwrap();

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "looking-similar",
                "objectID": "seed",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");
    let object_ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();
    assert_eq!(
        object_ids,
        vec!["near", "mid", "far"],
        "hits should be sorted by vector similarity"
    );
    assert!(!object_ids.contains(&"seed"), "seed must be excluded");

    let scores: Vec<u64> = hits
        .iter()
        .map(|h| h["_score"].as_u64().expect("numeric _score"))
        .collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]));
    assert!(scores.iter().all(|s| *s <= 100));
}

/// Verify that looking-similar excludes documents below the similarity threshold and caps results at `maxRecommendations`.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn recommend_looking_similar_applies_threshold_and_max_recommendations() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let app = recommend_router(state.clone());
    state.manager.create_tenant("products").unwrap();
    save_settings(&state, "products", &user_provided_embedder_settings(3));

    state
        .manager
        .add_documents_sync(
            "products",
            vec![
                make_vector_doc("seed", "Seed", Some([1.0, 0.0, 0.0])),
                make_vector_doc("near", "Near", Some([0.99, 0.01, 0.0])),
                make_vector_doc("mid", "Mid", Some([0.7, 0.3, 0.0])),
                make_vector_doc("far", "Far", Some([0.0, 1.0, 0.0])),
            ],
        )
        .await
        .unwrap();

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "looking-similar",
                "objectID": "seed",
                "threshold": 80,
                "maxRecommendations": 1
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["objectID"].as_str(), Some("near"));
    assert!(hits[0]["_score"].as_u64().unwrap() >= 80);
}

/// Catch a configured vector index suppressing term fallback when the seed has no embedding.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn recommend_looking_similar_seed_without_vector_falls_back_to_term_similarity() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let app = recommend_router(state.clone());
    state.manager.create_tenant("products").unwrap();
    let settings = embedder_and_text_searchable_settings(3);
    save_settings(&state, "products", &settings);

    state
        .manager
        .add_documents_sync(
            "products",
            LOOKING_SIMILAR_TEXT_CORPUS
                .iter()
                .map(|(id, name)| {
                    let vector = match *id {
                        "seed" => None,
                        "near" => Some([0.99, 0.01, 0.0]),
                        "mid" => Some([0.7, 0.3, 0.0]),
                        _ => Some([0.0, 1.0, 0.0]),
                    };
                    make_vector_doc(id, name, vector)
                })
                .collect(),
        )
        .await
        .unwrap();

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "looking-similar",
                "objectID": "seed",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_term_fallback_hits(&body, vec!["near", "mid"]);
}

/// Catch a text-only index returning empty hits merely because no vectors are configured.
#[tokio::test]
async fn recommend_looking_similar_index_without_vectors_falls_back_to_term_similarity() {
    let tmp = TempDir::new().unwrap();
    let app = text_looking_similar_app(&tmp).await;

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "looking-similar",
                "objectID": "seed",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_term_fallback_hits(&body, vec!["near", "mid"]);
}

/// Catch default-feature binaries silently returning no capped looking-similar recommendations.
#[tokio::test]
async fn recommend_looking_similar_returns_hits_without_vector_search_feature() {
    let tmp = TempDir::new().unwrap();
    let app = text_looking_similar_app(&tmp).await;

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "looking-similar",
                "objectID": "seed",
                "threshold": 0,
                "maxRecommendations": 1
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_term_fallback_hits(&body, vec!["near"]);
}

/// Catch a strict vector threshold falling through to term fallback instead of
/// returning the surviving vector hit.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn recommend_looking_similar_strict_threshold_does_not_fall_back() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let app = recommend_router(state.clone());
    state.manager.create_tenant("products").unwrap();
    save_settings(&state, "products", &user_provided_embedder_settings(3));

    state
        .manager
        .add_documents_sync("products", zero_overlap_nearest_vector_docs())
        .await
        .unwrap();

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "looking-similar",
                "objectID": "seed",
                "threshold": 100
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["objectID"].as_str(), Some("zero_overlap"));
    assert_eq!(hits[0]["_score"].as_u64(), Some(100));

    let object_ids = hits
        .iter()
        .map(|hit| hit["objectID"].as_str().expect("objectID"))
        .collect::<Vec<_>>();
    assert!(!object_ids.contains(&"near"));
    assert!(!object_ids.contains(&"mid"));
}

/// Catch an empty vector candidate set falling through to term fallback; the
/// same text corpus returns ["near", "mid"] through the term fallback.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn recommend_looking_similar_seed_only_vector_index_does_not_fall_back() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let app = recommend_router(state.clone());
    state.manager.create_tenant("products").unwrap();
    let settings = embedder_and_text_searchable_settings(3);
    save_settings(&state, "products", &settings);

    state
        .manager
        .add_documents_sync(
            "products",
            LOOKING_SIMILAR_TEXT_CORPUS
                .iter()
                .map(|(id, name)| {
                    let vector = (*id == "seed").then_some([1.0, 0.0, 0.0]);
                    make_vector_doc(id, name, vector)
                })
                .collect(),
        )
        .await
        .unwrap();

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "looking-similar",
                "objectID": "seed",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");
    assert_eq!(hits, &Vec::<serde_json::Value>::new());
}

/// Catch `compute_looking_similar` probing vectors before `get_or_load`; unlike
/// manager/tests.rs:5875, this proves the recommend endpoint uses the loaded
/// vector index instead of term fallback for a cold tenant.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn recommend_looking_similar_cold_tenant_uses_vector_path() {
    let tmp = TempDir::new().unwrap();
    let warm_state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    warm_state.manager.create_tenant("products").unwrap();
    save_settings(&warm_state, "products", &user_provided_embedder_settings(3));
    warm_state
        .manager
        .add_documents_sync("products", zero_overlap_nearest_vector_docs())
        .await
        .unwrap();

    let cold_state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let app = recommend_router(cold_state);

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "looking-similar",
                "objectID": "seed",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");
    assert_eq!(hits[0]["objectID"].as_str(), Some("zero_overlap"));
}

// ── C6.4: Recommend rules application ───────────────────────────────

/// Helper: set up cooccurrence data so related-products for "p1"
/// returns p2 (score 100), p3, p4 in that order.
async fn setup_cooccurrence_data(state: &Arc<AppState>, collector: &AnalyticsCollector) {
    insert_recommend_docs(
        state,
        "products",
        vec![
            Document {
                id: "p1".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 1".to_string()),
                )]),
            },
            Document {
                id: "p2".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 2".to_string()),
                )]),
            },
            Document {
                id: "p3".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 3".to_string()),
                )]),
            },
            Document {
                id: "p4".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 4".to_string()),
                )]),
            },
            Document {
                id: "p5".to_string(),
                fields: HashMap::from([(
                    "name".to_string(),
                    FieldValue::Text("Product 5".to_string()),
                )]),
            },
        ],
    )
    .await;

    let now_ms = Utc::now().timestamp_millis();
    record_conversion_events(
        collector,
        vec![
            // User A interacts with [p1, p2, p3], User B with [p1, p2, p4]
            make_conversion_event("products", "user-a", "p1", "conversion", now_ms),
            make_conversion_event("products", "user-a", "p2", "conversion", now_ms),
            make_conversion_event("products", "user-a", "p3", "conversion", now_ms),
            make_conversion_event("products", "user-b", "p1", "conversion", now_ms),
            make_conversion_event("products", "user-b", "p2", "conversion", now_ms),
            make_conversion_event("products", "user-b", "p4", "conversion", now_ms),
        ],
    );
}

/// Verify that a promote rule inserts the specified product at the target position in the results, hydrating it from the index if not already present.
#[tokio::test]
async fn recommend_rules_promote_pins_product_to_position() {
    use flapjack::recommend::rules::{
        self, PromoteObject, RecommendRule, RecommendRuleConsequence,
    };

    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());
    setup_cooccurrence_data(&state, &collector).await;

    // Save a rule that promotes p5 (not in cooccurrence results) to position 0
    rules::save_rules_batch(
        &state.manager.base_path,
        "products",
        "related-products",
        vec![RecommendRule {
            object_id: "promo-rule-1".to_string(),
            condition: None,
            consequence: Some(RecommendRuleConsequence {
                hide: None,
                promote: Some(vec![PromoteObject {
                    object_id: "p5".to_string(),
                    position: 0,
                }]),
                params: None,
            }),
            description: Some("Pin p5 first".to_string()),
            enabled: true,
            operation: None,
        }],
        false,
    )
    .unwrap();

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "p1",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");
    assert!(
        hits.len() >= 2,
        "should have at least promoted + cooccurrence hits"
    );
    assert_eq!(
        hits[0]["objectID"].as_str(),
        Some("p5"),
        "promoted product should be at position 0"
    );
    assert_eq!(
        hits[0]["name"].as_str(),
        Some("Product 5"),
        "promoted hit should include full document fields"
    );
}

/// Verify that a hide rule removes the specified product from the recommendation results.
#[tokio::test]
async fn recommend_rules_hide_excludes_product() {
    use flapjack::recommend::rules::{self, HideObject, RecommendRule, RecommendRuleConsequence};

    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());
    setup_cooccurrence_data(&state, &collector).await;

    // Save a rule that hides p2 (which would be the top cooccurrence result)
    rules::save_rules_batch(
        &state.manager.base_path,
        "products",
        "related-products",
        vec![RecommendRule {
            object_id: "hide-rule-1".to_string(),
            condition: None,
            consequence: Some(RecommendRuleConsequence {
                hide: Some(vec![HideObject {
                    object_id: "p2".to_string(),
                }]),
                promote: None,
                params: None,
            }),
            description: Some("Hide p2".to_string()),
            enabled: true,
            operation: None,
        }],
        false,
    )
    .unwrap();

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "p1",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");
    let object_ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();
    assert!(
        !object_ids.contains(&"p2"),
        "hidden product p2 should not appear in results"
    );
    assert!(!hits.is_empty(), "should still have other results");
}

/// Verify that a rule with `enabled: false` has no effect on recommendation results.
#[tokio::test]
async fn recommend_rules_disabled_rule_not_applied() {
    use flapjack::recommend::rules::{self, HideObject, RecommendRule, RecommendRuleConsequence};

    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());
    setup_cooccurrence_data(&state, &collector).await;

    // Save a disabled rule that hides p2
    rules::save_rules_batch(
        &state.manager.base_path,
        "products",
        "related-products",
        vec![RecommendRule {
            object_id: "disabled-rule".to_string(),
            condition: None,
            consequence: Some(RecommendRuleConsequence {
                hide: Some(vec![HideObject {
                    object_id: "p2".to_string(),
                }]),
                promote: None,
                params: None,
            }),
            description: Some("Disabled hide rule".to_string()),
            enabled: false,
            operation: None,
        }],
        false,
    )
    .unwrap();

    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "p1",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let hits = body["results"][0]["hits"].as_array().expect("hits array");
    let object_ids: Vec<&str> = hits
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();
    assert!(
        object_ids.contains(&"p2"),
        "disabled rule should not hide p2 — p2 should still appear"
    );
}

/// Verify that a rule with a `filters` condition only applies when the request's `queryParameters.filters` matches the condition value.
#[tokio::test]
async fn recommend_rules_filter_condition_must_match_to_apply() {
    use flapjack::recommend::rules::{self, HideObject, RecommendRule, RecommendRuleConsequence};

    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());
    setup_cooccurrence_data(&state, &collector).await;

    rules::save_rules_batch(
        &state.manager.base_path,
        "products",
        "related-products",
        vec![RecommendRule {
            object_id: "conditional-hide".to_string(),
            condition: Some(flapjack::recommend::rules::RecommendRuleCondition {
                filters: Some("brand:Nike".to_string()),
                context: None,
            }),
            consequence: Some(RecommendRuleConsequence {
                hide: Some(vec![HideObject {
                    object_id: "p2".to_string(),
                }]),
                promote: None,
                params: None,
            }),
            description: Some("Hide p2 when filter matches".to_string()),
            enabled: true,
            operation: None,
        }],
        false,
    )
    .unwrap();

    // Rule should not apply without matching request filters.
    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "p1",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let object_ids: Vec<&str> = body["results"][0]["hits"]
        .as_array()
        .expect("hits array")
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();
    assert!(
        object_ids.contains(&"p2"),
        "rule without matching condition should not apply"
    );

    // Rule should apply when queryParameters includes matching filters.
    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "p1",
                "threshold": 0,
                "queryParameters": {
                    "filters": "brand:Nike"
                }
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let object_ids: Vec<&str> = body["results"][0]["hits"]
        .as_array()
        .expect("hits array")
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();
    assert!(
        !object_ids.contains(&"p2"),
        "rule should apply when filters match"
    );
}

/// Verify that a rule with a `context` condition only applies when the request's `queryParameters.ruleContexts` contains the matching context value.
#[tokio::test]
async fn recommend_rules_context_condition_must_match() {
    use flapjack::recommend::rules::{self, HideObject, RecommendRule, RecommendRuleConsequence};

    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());
    setup_cooccurrence_data(&state, &collector).await;

    rules::save_rules_batch(
        &state.manager.base_path,
        "products",
        "related-products",
        vec![RecommendRule {
            object_id: "context-hide".to_string(),
            condition: Some(flapjack::recommend::rules::RecommendRuleCondition {
                filters: None,
                context: Some("homepage".to_string()),
            }),
            consequence: Some(RecommendRuleConsequence {
                hide: Some(vec![HideObject {
                    object_id: "p3".to_string(),
                }]),
                promote: None,
                params: None,
            }),
            description: Some("Hide p3 for homepage context".to_string()),
            enabled: true,
            operation: None,
        }],
        false,
    )
    .unwrap();

    // Rule should not apply without context.
    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "p1",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let object_ids: Vec<&str> = body["results"][0]["hits"]
        .as_array()
        .expect("hits array")
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();
    assert!(
        object_ids.contains(&"p3"),
        "rule with context mismatch should not apply"
    );

    // Rule should apply when ruleContexts contains matching context.
    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "p1",
                "threshold": 0,
                "queryParameters": {
                    "ruleContexts": ["homepage"]
                }
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let object_ids: Vec<&str> = body["results"][0]["hits"]
        .as_array()
        .expect("hits array")
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();
    assert!(
        !object_ids.contains(&"p3"),
        "rule should apply when context matches"
    );
}

/// C6.5: Rules are model-scoped — a rule saved for `bought-together` must not
/// affect `related-products` recommendations.
#[tokio::test]
async fn recommend_rules_are_model_scoped() {
    use flapjack::recommend::rules::{self, HideObject, RecommendRule, RecommendRuleConsequence};

    let tmp = TempDir::new().unwrap();
    let (state, collector) = make_test_state_with_analytics(&tmp);
    let app = recommend_router(state.clone());
    setup_cooccurrence_data(&state, &collector).await;

    // Save a hide rule for `bought-together` that hides p2
    rules::save_rules_batch(
        &state.manager.base_path,
        "products",
        "bought-together",
        vec![RecommendRule {
            object_id: "hide-p2-bought".to_string(),
            condition: None,
            consequence: Some(RecommendRuleConsequence {
                hide: Some(vec![HideObject {
                    object_id: "p2".to_string(),
                }]),
                promote: None,
                params: None,
            }),
            description: Some("Hide p2 for bought-together only".to_string()),
            enabled: true,
            operation: None,
        }],
        false,
    )
    .unwrap();

    // `related-products` recommendations for p1 should still include p2
    let (status, body) = post_recommend(
        &app,
        serde_json::json!({
            "requests": [{
                "indexName": "products",
                "model": "related-products",
                "objectID": "p1",
                "threshold": 0
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let object_ids: Vec<&str> = body["results"][0]["hits"]
        .as_array()
        .expect("hits array")
        .iter()
        .map(|h| h["objectID"].as_str().expect("objectID"))
        .collect();
    assert!(
        object_ids.contains(&"p2"),
        "bought-together rule must not affect related-products — p2 should be visible"
    );
}
