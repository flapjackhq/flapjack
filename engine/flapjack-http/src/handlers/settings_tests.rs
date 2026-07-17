use super::*;
use crate::test_helpers::TestStateBuilder;
use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use axum::Router;
use tempfile::TempDir;
use tokio::time::{sleep, Duration};
use tower::ServiceExt;

fn settings_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/1/indexes/:indexName/settings",
            get(get_settings).post(set_settings),
        )
        .with_state(state)
}

async fn post_settings(app: &Router, body: &str) -> axum::http::Response<Body> {
    post_json(app, "/1/indexes/test_idx/settings", body).await
}

async fn post_json(app: &Router, uri: &str, body: &str) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Issue a `GET /1/indexes/test_idx/settings` request and deserialize the response body.
async fn get_settings_json(app: &Router) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/1/indexes/test_idx/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

async fn post_search_json(
    app: &Router,
    index_name: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/1/indexes/{index_name}/query"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "search request should succeed"
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn test_get_settings_corrupt_settings_file_returns_sanitized_500() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let settings_dir = tmp.path().join("test_idx");
    std::fs::create_dir_all(&settings_dir).unwrap();
    let settings_path = settings_dir.join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"queryType":"prefixLast","searchableAttributes":["title"]"#,
    )
    .unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/1/indexes/test_idx/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let message = json["message"]
        .as_str()
        .expect("500 response should include a string message");
    assert_eq!(message, "Internal server error");
    assert_eq!(json["status"], serde_json::json!(500));
    assert!(
        !message.contains("settings.json"),
        "500 message must not leak internal file paths: {message}"
    );
    assert!(
        !message.contains("JSON error"),
        "500 message must not leak serde details: {message}"
    );
}

#[tokio::test]
async fn test_set_settings_when_tenant_path_is_file_returns_sanitized_500() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    std::fs::write(tmp.path().join("test_idx"), b"not-a-directory").unwrap();

    let response = post_settings(&app, r#"{"searchableAttributes":["title"]}"#).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let message = json["message"]
        .as_str()
        .expect("500 response should include a string message");
    assert_eq!(message, "Internal server error");
    assert_eq!(json["status"], serde_json::json!(500));
    assert!(
        !message.contains("settings.json"),
        "500 message must not leak internal file paths: {message}"
    );
    assert!(
        !message.contains("not a directory"),
        "500 message must not leak OS-level IO text: {message}"
    );
}

/// Verify that posting an embedder configuration roundtrips through GET settings.
#[tokio::test]
async fn test_set_settings_with_embedders() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(
        &app,
        r#"{"embedders": {"default": {"source": "userProvided", "dimensions": 384}}}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(json["embedders"]["default"]["source"], "userProvided");
    assert_eq!(json["embedders"]["default"]["dimensions"], 384);
}

/// Test that allowCompressionOfIntegerArray: false roundtrips correctly.
/// Gap: existing integration test only covers `true`, not explicit `false`.
#[tokio::test]
async fn allow_compression_of_integer_array_false_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    // PUT with explicit false
    let resp = post_settings(&app, r#"{"allowCompressionOfIntegerArray": false}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // GET and verify false is preserved (not omitted)
    let json = get_settings_json(&app).await;
    assert_eq!(
        json["allowCompressionOfIntegerArray"],
        serde_json::json!(false),
        "allowCompressionOfIntegerArray: false should be preserved in roundtrip"
    );
}

/// Verify that an embedder missing required fields (e.g. `dimensions` for `openAi`) returns 400.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_set_settings_invalid_embedder_returns_400() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(&app, r#"{"embedders": {"myEmb": {"source": "openAi"}}}"#).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        body_str.contains("myEmb"),
        "error should mention embedder name: {}",
        body_str
    );
}

#[tokio::test]
async fn test_set_settings_without_embedders_no_embedders_in_response() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(&app, r#"{"searchableAttributes": ["title"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert!(
        json.get("embedders").is_none(),
        "response should not contain 'embedders' key"
    );
}

/// Verify that `renderingContent` with nested facet ordering roundtrips through GET settings.
#[tokio::test]
async fn test_set_settings_rendering_content_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(
        &app,
        r#"{
            "renderingContent": {
                "facetOrdering": {
                    "facets": { "order": ["brand", "category"] },
                    "values": {
                        "brand": {
                            "order": ["Apple", "Samsung"],
                            "sortRemainingBy": "alpha",
                            "hide": ["Unknown"]
                        }
                    }
                }
            }
        }"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(
        json["renderingContent"],
        serde_json::json!({
            "facetOrdering": {
                "facets": { "order": ["brand", "category"] },
                "values": {
                    "brand": {
                        "order": ["Apple", "Samsung"],
                        "sortRemainingBy": "alpha",
                        "hide": ["Unknown"]
                    }
                }
            }
        })
    );
}

#[tokio::test]
async fn test_set_settings_user_data_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(&app, r#"{"userData":{"custom":"data"}}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(json["userData"], serde_json::json!({"custom":"data"}));
}

/// Verify that GET settings redacts the AI provider API key to `<redacted>` in the response body while preserving the unredacted value on disk.
#[tokio::test]
async fn test_get_settings_redacts_ai_provider_api_key() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(Arc::clone(&state));

    let resp = post_settings(
        &app,
        r#"{"userData":{"aiProvider":{"baseUrl":"https://example.test/v1","apiKey":"secret-key"},"custom":"data"}}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(
        json["userData"]["aiProvider"]["apiKey"],
        serde_json::json!("<redacted>")
    );
    assert_eq!(
        json["userData"]["aiProvider"]["baseUrl"],
        serde_json::json!("https://example.test/v1")
    );
    assert_eq!(json["userData"]["custom"], serde_json::json!("data"));

    let settings_path = tmp.path().join("test_idx").join("settings.json");
    let on_disk = IndexSettings::load(&settings_path).unwrap();
    assert_eq!(
        on_disk.user_data.unwrap()["aiProvider"]["apiKey"],
        serde_json::json!("secret-key")
    );
}

/// Verify that GET settings redacts the embedder API key to `<redacted>` in the response body while preserving the unredacted value on disk.
#[tokio::test]
async fn test_get_settings_redacts_embedder_api_key() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(Arc::clone(&state));

    let resp = post_settings(
        &app,
        r#"{"embedders":{"openai":{"source":"openAi","apiKey":"sk-test-secret"}}}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(
        json["embedders"]["openai"]["apiKey"],
        serde_json::json!("<redacted>")
    );
    assert_eq!(
        json["embedders"]["openai"]["source"],
        serde_json::json!("openAi")
    );

    let settings_path = tmp.path().join("test_idx").join("settings.json");
    let on_disk = IndexSettings::load(&settings_path).unwrap();
    assert_eq!(
        on_disk.embedders.unwrap()["openai"]["apiKey"],
        serde_json::json!("sk-test-secret")
    );
}

/// Verify that a path traversal attempt in the index name (e.g., `..%2Fescape`) is rejected with HTTP 400 BAD_REQUEST.
#[tokio::test]
async fn test_get_settings_rejects_path_traversal_index_name() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/1/indexes/..%2Fescape/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_set_settings_enable_rules_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(&app, r#"{"enableRules":false}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(json["enableRules"], serde_json::json!(false));
}

#[tokio::test]
async fn test_set_settings_pagination_limited_to_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(&app, r#"{"paginationLimitedTo":50}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(json["paginationLimitedTo"], serde_json::json!(50));
}

#[tokio::test]
async fn test_set_settings_reindexes_existing_documents_for_facets() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(Arc::clone(&state));
    let index_name = "facet_reindex_idx";

    state.manager.create_tenant(index_name).unwrap();
    let docs = vec![
        flapjack::Document {
            id: "1".to_string(),
            fields: HashMap::from([
                (
                    "title".to_string(),
                    flapjack::FieldValue::Text("pancake recipes".to_string()),
                ),
                (
                    "tags".to_string(),
                    flapjack::FieldValue::Text("breakfast".to_string()),
                ),
            ]),
        },
        flapjack::Document {
            id: "2".to_string(),
            fields: HashMap::from([
                (
                    "title".to_string(),
                    flapjack::FieldValue::Text("waffle guide".to_string()),
                ),
                (
                    "tags".to_string(),
                    flapjack::FieldValue::Text("breakfast".to_string()),
                ),
            ]),
        },
        flapjack::Document {
            id: "3".to_string(),
            fields: HashMap::from([
                (
                    "title".to_string(),
                    flapjack::FieldValue::Text("omelette handbook".to_string()),
                ),
                (
                    "tags".to_string(),
                    flapjack::FieldValue::Text("breakfast".to_string()),
                ),
            ]),
        },
    ];
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let response =
        post_settings_for(&app, index_name, r#"{"attributesForFaceting":["tags"]}"#).await;
    assert_eq!(response.status(), StatusCode::OK);

    let search = post_search_json(
        &app,
        index_name,
        serde_json::json!({
            "query": "",
            "facets": ["tags"]
        }),
    )
    .await;

    assert_eq!(search["nbHits"], 3);
    assert_eq!(search["facets"]["tags"]["breakfast"], 3);
}

/// Verify that the faceting rebuild path works across multiple scan pages and
/// bounded write batches without buffering every full document at once.
#[tokio::test]
async fn test_set_settings_reindexes_existing_documents_for_facets_across_batches() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(Arc::clone(&state));
    let index_name = "facet_reindex_batches_idx";

    state.manager.create_tenant(index_name).unwrap();
    let docs = (0..1001)
        .map(|i| flapjack::Document {
            id: format!("{i}"),
            fields: HashMap::from([
                (
                    "title".to_string(),
                    flapjack::FieldValue::Text(format!("breakfast doc {i}")),
                ),
                (
                    "tags".to_string(),
                    flapjack::FieldValue::Text("breakfast".to_string()),
                ),
            ]),
        })
        .collect();
    state
        .manager
        .add_documents_sync(index_name, docs)
        .await
        .unwrap();

    let response =
        post_settings_for(&app, index_name, r#"{"attributesForFaceting":["tags"]}"#).await;
    assert_eq!(response.status(), StatusCode::OK);

    let search = post_search_json(
        &app,
        index_name,
        serde_json::json!({
            "query": "",
            "facets": ["tags"]
        }),
    )
    .await;

    assert_eq!(search["nbHits"], 1001);
    assert_eq!(search["facets"]["tags"]["breakfast"], 1001);
}

/// Verify that embedder configuration is persisted to `settings.json` on disk.
#[tokio::test]
async fn test_set_settings_embedders_persist_to_disk() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    post_settings(
        &app,
        r#"{"embedders": {"default": {"source": "userProvided", "dimensions": 256}}}"#,
    )
    .await;

    // Load directly from disk
    let settings_path = tmp.path().join("test_idx").join("settings.json");
    let loaded = IndexSettings::load(&settings_path).unwrap();
    let emb = loaded
        .embedders
        .as_ref()
        .expect("embedders should be persisted");
    assert_eq!(emb["default"]["dimensions"], 256);
}

/// Verify that updating an unrelated field does not discard previously configured embedders.
#[tokio::test]
async fn test_set_settings_preserves_embedders_on_other_update() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    // First: set embedders
    post_settings(
        &app,
        r#"{"embedders": {"default": {"source": "userProvided", "dimensions": 384}}}"#,
    )
    .await;

    // Second: update a different field (no embedders in payload)
    post_settings(&app, r#"{"attributesForFaceting": ["category"]}"#).await;

    let json = get_settings_json(&app).await;
    assert_eq!(
        json["embedders"]["default"]["dimensions"], 384,
        "embedders should be preserved when not in payload"
    );
}

/// Verify that a partial settings update preserves previously stored fields that are omitted from the payload.
#[tokio::test]
async fn test_merge_settings_payload_partial_update_preserves_omitted_fields() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    // Seed a multi-field baseline that spans regular fields plus merge-sensitive embedders/replicas.
    let resp = post_settings(
        &app,
        r#"{
            "searchableAttributes": ["title", "description"],
            "queryType": "prefixNone",
            "embedders": {"default": {"source": "userProvided", "dimensions": 384}},
            "replicas": ["products_price_asc"]
        }"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Update only one field; omitted baseline fields must remain unchanged.
    let resp = post_settings(&app, r#"{"paginationLimitedTo": 75}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(
        json["searchableAttributes"],
        serde_json::json!(["title", "description"])
    );
    assert_eq!(json["queryType"], serde_json::json!("prefixNone"));
    assert_eq!(json["replicas"], serde_json::json!(["products_price_asc"]));
    assert_eq!(
        json["embedders"]["default"]["source"],
        serde_json::json!("userProvided")
    );
    assert_eq!(
        json["embedders"]["default"]["dimensions"],
        serde_json::json!(384)
    );
    assert_eq!(json["paginationLimitedTo"], serde_json::json!(75));
}

#[tokio::test]
async fn test_set_settings_rejects_malformed_distinct() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    for payload in [
        r#"{"distinct": "2"}"#,
        r#"{"distinct": 4294967296}"#,
        r#"{"distinct": -1}"#,
    ] {
        let resp = post_settings(&app, payload).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}

// ── Mode and SemanticSearch handler tests (5.8) ──

#[tokio::test]
async fn test_set_settings_mode_neural_search() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(&app, r#"{"mode": "neuralSearch"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(json["mode"], "neuralSearch");
}

#[tokio::test]
async fn test_set_settings_mode_keyword_search() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(&app, r#"{"mode": "keywordSearch"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(json["mode"], "keywordSearch");
}

#[tokio::test]
async fn test_set_settings_mode_default_not_in_response() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let json = get_settings_json(&app).await;
    assert!(
        json.get("mode").is_none(),
        "fresh index should not have 'mode' key in response"
    );
    assert!(
        json.get("semanticSearch").is_none(),
        "fresh index should not have 'semanticSearch' key in response"
    );
}

#[tokio::test]
async fn test_set_settings_mode_preserves_on_other_update() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    // Set mode
    post_settings(&app, r#"{"mode": "neuralSearch"}"#).await;

    // Update a different field
    post_settings(&app, r#"{"searchableAttributes": ["title"]}"#).await;

    let json = get_settings_json(&app).await;
    assert_eq!(json["mode"], "neuralSearch", "mode should be preserved");
}

#[tokio::test]
async fn test_set_settings_mode_revert_to_keyword() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    post_settings(&app, r#"{"mode": "neuralSearch"}"#).await;
    post_settings(&app, r#"{"mode": "keywordSearch"}"#).await;

    let json = get_settings_json(&app).await;
    assert_eq!(json["mode"], "keywordSearch");
}

/// Verify that `semanticSearch.eventSources` roundtrips through GET settings.
#[tokio::test]
async fn test_set_settings_semantic_search() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(
        &app,
        r#"{"semanticSearch": {"eventSources": ["idx1", "idx2"]}}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    let event_sources = &json["semanticSearch"]["eventSources"];
    assert_eq!(event_sources[0], "idx1");
    assert_eq!(event_sources[1], "idx2");
}

/// Verify that `mode` and `embedders` can be set in a single request and both persist.
#[tokio::test]
async fn test_set_settings_mode_and_embedders_together() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(
        &app,
        r#"{"mode": "neuralSearch", "embedders": {"default": {"source": "userProvided", "dimensions": 384}}}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(json["mode"], "neuralSearch");
    assert_eq!(json["embedders"]["default"]["source"], "userProvided");
}

#[tokio::test]
async fn test_set_settings_neural_mode_no_embedders_warns() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    // Should succeed (200), not error — even though no embedders configured
    let resp = post_settings(&app, r#"{"mode": "neuralSearch"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 6.22: Verify that updating embedder settings invalidates the embedder cache.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_set_settings_embedders_invalidate_cache() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state.clone());

    // Set initial embedder config
    post_settings(
        &app,
        r#"{"embedders": {"default": {"source": "userProvided", "dimensions": 384}}}"#,
    )
    .await;

    // Create and cache an embedder via get_or_create
    let settings = flapjack::index::settings::IndexSettings::load(
        tmp.path().join("test_idx").join("settings.json"),
    )
    .unwrap();
    let e1 = state
        .embedder_store
        .get_or_create("test_idx", "default", &settings)
        .unwrap();

    // Now update embedder settings (different dimensions)
    post_settings(
        &app,
        r#"{"embedders": {"default": {"source": "userProvided", "dimensions": 768}}}"#,
    )
    .await;

    // Get the embedder again — should be a fresh instance (different Arc)
    let updated_settings = flapjack::index::settings::IndexSettings::load(
        tmp.path().join("test_idx").join("settings.json"),
    )
    .unwrap();
    let e2 = state
        .embedder_store
        .get_or_create("test_idx", "default", &updated_settings)
        .unwrap();
    assert!(
        !std::sync::Arc::ptr_eq(&e1, &e2),
        "Embedder should be re-created after settings update (cache invalidated)"
    );
    assert_eq!(
        e2.dimensions(),
        768,
        "New embedder should have updated dimensions"
    );
}

/// Verify that posting an empty embedders map clears previously configured embedders.
#[tokio::test]
async fn test_set_settings_clear_embedders() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    // First: set embedders
    post_settings(
        &app,
        r#"{"embedders": {"default": {"source": "userProvided", "dimensions": 384}}}"#,
    )
    .await;

    // Second: clear with empty map
    post_settings(&app, r#"{"embedders": {}}"#).await;

    let json = get_settings_json(&app).await;
    assert!(
        json.get("embedders").is_none(),
        "embedders should be cleared after empty map"
    );
}

// ── Replicas setting tests (§10) ──

/// Verify that standard and virtual replica names roundtrip through GET settings.
#[tokio::test]
async fn test_replicas_setting_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(
        &app,
        r#"{"replicas": ["products_price_asc", "virtual(products_relevance)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    let replicas = json["replicas"]
        .as_array()
        .expect("replicas should be an array");
    assert_eq!(replicas.len(), 2);
    assert_eq!(replicas[0], "products_price_asc");
    assert_eq!(replicas[1], "virtual(products_relevance)");
}

/// Verify that updating a non-replica field preserves the existing `replicas` list.
#[tokio::test]
async fn test_replicas_setting_preserves_on_other_update() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    // Set replicas
    post_settings(&app, r#"{"replicas": ["products_price_asc"]}"#).await;

    // Update a different field
    post_settings(&app, r#"{"searchableAttributes": ["title"]}"#).await;

    let json = get_settings_json(&app).await;
    let replicas = json["replicas"]
        .as_array()
        .expect("replicas should be preserved");
    assert_eq!(replicas.len(), 1);
    assert_eq!(replicas[0], "products_price_asc");
}

#[tokio::test]
async fn test_replicas_setting_reject_self_reference() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    // Use the index name "test_idx" since that's what post_settings uses
    let resp = post_settings(&app, r#"{"replicas": ["test_idx"]}"#).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_replicas_setting_reject_duplicates() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(&app, r#"{"replicas": ["replica_a", "replica_a"]}"#).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_replicas_setting_reject_invalid_virtual_syntax() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(&app, r#"{"replicas": ["virtual("]}"#).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_replicas_setting_reject_invalid_index_name() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(&app, r#"{"replicas": ["../products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_replicas_default_not_in_response() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let json = get_settings_json(&app).await;
    assert!(
        json.get("replicas").is_none(),
        "fresh index should not have 'replicas' key"
    );
}

/// Verify that posting `replicas: []` removes the replicas key from settings.
#[tokio::test]
async fn test_replicas_clear_with_empty_array() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    // Set replicas — must succeed before we can test clearing
    let resp = post_settings(&app, r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "setting replicas should succeed"
    );
    // Verify replicas are actually set
    let json = get_settings_json(&app).await;
    assert!(
        json.get("replicas").is_some(),
        "replicas should be set before clearing"
    );
    // Clear with empty array
    let resp = post_settings(&app, r#"{"replicas": []}"#).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "clearing replicas should succeed"
    );

    let json = get_settings_json(&app).await;
    assert!(
        json.get("replicas").is_none(),
        "replicas should be cleared after empty array"
    );
}

// ── A2: Replica write-sync tests ──

/// Build a router with settings + write + read + clear endpoints for replica sync tests.
fn replica_sync_router(state: Arc<AppState>) -> Router {
    use axum::routing::{get, post};
    crate::router::app_id_layer(
        Router::new()
            .route(
                "/1/indexes/:indexName/settings",
                get(get_settings).post(set_settings),
            )
            .route(
                "/1/indexes/:indexName/batch",
                post(crate::handlers::objects::add_documents),
            )
            .route(
                "/1/indexes/:indexName/:objectID",
                get(crate::handlers::objects::get_object)
                    .put(crate::handlers::objects::put_object)
                    .delete(crate::handlers::objects::delete_object),
            )
            .route(
                "/1/indexes/:indexName/:objectID/partial",
                post(crate::handlers::objects::partial_update_object),
            )
            .route(
                "/1/indexes/:indexName/clear",
                post(crate::handlers::indices::clear_index),
            )
            .route(
                "/1/indexes/:indexName/query",
                post(crate::handlers::search::search),
            )
            .route(
                "/1/indexes/:indexName",
                post(crate::handlers::objects::add_record_auto_id),
            )
            .with_state(state),
    )
}

/// POST /1/indexes/{idx}/settings with a custom index name
async fn post_settings_for(
    app: &Router,
    index_name: &str,
    body: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/1/indexes/{}/settings", index_name))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Issue a `GET /1/indexes/{index_name}/settings` request and deserialize the response body.
async fn get_settings_json_for(app: &Router, index_name: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/1/indexes/{}/settings", index_name))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// POST batch to add documents to an index
async fn batch_add_docs(
    app: &Router,
    index_name: &str,
    docs_json: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/1/indexes/{}/batch", index_name))
                .header("content-type", "application/json")
                .body(Body::from(docs_json.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// GET a single object from an index
async fn get_object_from(
    app: &Router,
    index_name: &str,
    object_id: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(format!("/1/indexes/{}/{}", index_name, object_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// DELETE a single object from an index
async fn delete_object_from(
    app: &Router,
    index_name: &str,
    object_id: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/1/indexes/{}/{}", index_name, object_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// PUT a single object into an index (sync — goes through handler layer)
async fn put_object_for(
    app: &Router,
    index_name: &str,
    object_id: &str,
    body: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/1/indexes/{}/{}", index_name, object_id))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// POST partial update for a single object
async fn partial_update_for(
    app: &Router,
    index_name: &str,
    object_id: &str,
    body: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/1/indexes/{}/{}/partial", index_name, object_id))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// POST clear on an index
async fn clear_index_req(app: &Router, index_name: &str) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/1/indexes/{}/clear", index_name))
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// POST a document without an explicit objectID, using the auto-ID endpoint.
async fn add_record_auto_id_for(
    app: &Router,
    index_name: &str,
    body: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/1/indexes/{}", index_name))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// POST a search query and return the ordered objectIDs from hits.
async fn search_hit_ids_for(app: &Router, index_name: &str, query: &str) -> Vec<String> {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/1/indexes/{}/query", index_name))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "query": query,
                        "hitsPerPage": 10
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
        "search should succeed for index {}",
        index_name
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let hits = json["hits"].as_array().expect("hits must be an array");
    hits.iter()
        .map(|hit| {
            hit["objectID"]
                .as_str()
                .expect("objectID must be a string")
                .to_string()
        })
        .collect()
}

async fn get_object_json(app: &Router, index_name: &str, object_id: &str) -> serde_json::Value {
    let resp = get_object_from(app, index_name, object_id).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected {}/{} to exist",
        index_name,
        object_id
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// Poll `GET /1/indexes/{index_name}/{object_id}` until the response status matches `expected_status`.
///
/// # Panics
///
/// Panics after 100 attempts (1 second total) if the expected status is never observed.
async fn wait_for_object_status(
    app: &Router,
    index_name: &str,
    object_id: &str,
    expected_status: StatusCode,
) {
    for _ in 0..100 {
        let resp = get_object_from(app, index_name, object_id).await;
        if resp.status() == expected_status {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "timed out waiting for {}/{} to reach status {}",
        index_name, object_id, expected_status
    );
}

/// Helper: create a Document with an id and a "name" text field.
fn make_doc(id: &str, name: &str) -> flapjack::Document {
    let mut fields = std::collections::HashMap::new();
    fields.insert(
        "name".to_string(),
        flapjack::FieldValue::Text(name.to_string()),
    );
    flapjack::Document {
        id: id.to_string(),
        fields,
    }
}

/// Verify that a PUT to the primary index synchronously mirrors the document to standard replicas.
#[tokio::test]
async fn test_replica_sync_put_object() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state.clone());

    // 1. Set replicas on primary
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Add documents to primary via HTTP PUT (goes through handler layer for sync)
    let resp = put_object_for(&app, "products", "1", r#"{"name": "Widget"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK, "PUT doc 1 to primary");
    let resp = put_object_for(&app, "products", "2", r#"{"name": "Gadget"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK, "PUT doc 2 to primary");

    // 3. Verify documents exist in primary
    let resp = get_object_from(&app, "products", "1").await;
    assert_eq!(resp.status(), StatusCode::OK, "doc should exist in primary");
    let resp = get_object_from(&app, "products", "2").await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "doc 2 should exist in primary"
    );

    // 4. Verify documents were synced to standard replica
    let resp = get_object_from(&app, "products_price_asc", "1").await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "doc '1' should be synced to standard replica"
    );
    let resp = get_object_from(&app, "products_price_asc", "2").await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "doc '2' should be synced to standard replica"
    );
}

/// Verify that batch-added documents on the primary are propagated to standard replicas.
#[tokio::test]
async fn test_replica_sync_batch_add_documents() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state.clone());

    // 1. Set replicas on primary
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Add documents via batch endpoint
    let resp = batch_add_docs(
        &app,
        "products",
        r#"{
            "requests": [
                {"action": "addObject", "body": {"objectID": "1", "name": "Widget"}},
                {"action": "addObject", "body": {"objectID": "2", "name": "Gadget"}}
            ]
        }"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "batch add to primary");

    // 3. Wait until async write queue has committed docs in primary.
    wait_for_object_status(&app, "products", "1", StatusCode::OK).await;
    wait_for_object_status(&app, "products", "2", StatusCode::OK).await;

    // 4. Verify docs are also present in standard replica.
    wait_for_object_status(&app, "products_price_asc", "1", StatusCode::OK).await;
    wait_for_object_status(&app, "products_price_asc", "2", StatusCode::OK).await;
}

/// Verify that a batch `deleteObject` on the primary is mirrored to standard replicas.
#[tokio::test]
async fn test_replica_sync_batch_delete_object() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state.clone());

    // 1. Set replicas on primary.
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Seed via the HTTP write path so primary + replica both have the same document.
    let resp = put_object_for(&app, "products", "1", r#"{"name": "Widget"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        get_object_from(&app, "products", "1").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        get_object_from(&app, "products_price_asc", "1")
            .await
            .status(),
        StatusCode::OK
    );

    // 3. Delete through batch action on primary.
    let resp = batch_add_docs(
        &app,
        "products",
        r#"{
            "requests": [
                {"action": "deleteObject", "body": {"objectID": "1"}}
            ]
        }"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 4. Verify delete mirrored to standard replica.
    assert_ne!(
        get_object_from(&app, "products", "1").await.status(),
        StatusCode::OK,
        "primary should be deleted through batch deleteObject"
    );
    assert_ne!(
        get_object_from(&app, "products_price_asc", "1")
            .await
            .status(),
        StatusCode::OK,
        "replica should mirror batch deleteObject from primary"
    );
}

/// Verify that write operations on the primary are not materialized into virtual replicas.
#[tokio::test]
async fn test_replica_sync_skips_virtual_replicas() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state.clone());

    // 1. Configure one standard and one virtual replica.
    let resp = post_settings_for(
        &app,
        "products",
        r#"{"replicas": ["products_price_asc", "virtual(products_relevance)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Write to primary.
    let resp = put_object_for(&app, "products", "1", r#"{"name": "Widget"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 3. Standard replica is synced.
    assert_eq!(
        get_object_from(&app, "products_price_asc", "1")
            .await
            .status(),
        StatusCode::OK,
        "standard replicas should be synced"
    );

    // 4. Virtual replica must not be materialized/synced during Stage 1 write propagation.
    assert_ne!(
        get_object_from(&app, "products_relevance", "1")
            .await
            .status(),
        StatusCode::OK,
        "virtual replicas are settings-only and must not receive Stage 1 writes"
    );
}

/// Verify that deleting a document from the primary also removes it from standard replicas.
#[tokio::test]
async fn test_replica_sync_delete_object() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state.clone());

    // 1. Set replicas on primary
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Add docs to primary and replica directly (setup)
    state
        .manager
        .add_documents_sync("products", vec![make_doc("1", "Widget")])
        .await
        .unwrap();
    state.manager.create_tenant("products_price_asc").unwrap();
    state
        .manager
        .add_documents_sync("products_price_asc", vec![make_doc("1", "Widget")])
        .await
        .unwrap();

    // 3. Intermediate assertion: doc exists in BOTH primary and replica
    let resp = get_object_from(&app, "products", "1").await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "setup: doc should exist in primary"
    );
    let resp = get_object_from(&app, "products_price_asc", "1").await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "setup: doc should exist in replica"
    );

    // 4. Delete from primary via HTTP
    let resp = delete_object_from(&app, "products", "1").await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 5. Verify deleted from primary
    let resp = get_object_from(&app, "products", "1").await;
    assert_ne!(
        resp.status(),
        StatusCode::OK,
        "doc should be deleted from primary"
    );

    // 6. Verify deleted from standard replica (RED: sync not yet implemented)
    let resp = get_object_from(&app, "products_price_asc", "1").await;
    assert_ne!(
        resp.status(),
        StatusCode::OK,
        "doc should be deleted from standard replica"
    );
}

/// Verify that clearing the primary index also clears all standard replicas.
#[tokio::test]
async fn test_replica_sync_clear_index() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state.clone());

    // 1. Set replicas on primary
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Add docs to primary and replica directly (setup)
    state
        .manager
        .add_documents_sync(
            "products",
            vec![make_doc("1", "Widget"), make_doc("2", "Gadget")],
        )
        .await
        .unwrap();
    state.manager.create_tenant("products_price_asc").unwrap();
    state
        .manager
        .add_documents_sync(
            "products_price_asc",
            vec![make_doc("1", "Widget"), make_doc("2", "Gadget")],
        )
        .await
        .unwrap();

    // 3. Intermediate assertion: docs exist in BOTH primary and replica
    let resp = get_object_from(&app, "products", "1").await;
    assert_eq!(resp.status(), StatusCode::OK, "setup: doc 1 in primary");
    let resp = get_object_from(&app, "products_price_asc", "1").await;
    assert_eq!(resp.status(), StatusCode::OK, "setup: doc 1 in replica");
    let resp = get_object_from(&app, "products_price_asc", "2").await;
    assert_eq!(resp.status(), StatusCode::OK, "setup: doc 2 in replica");

    // 4. Clear the primary index via HTTP
    let resp = clear_index_req(&app, "products").await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 5. Verify primary is cleared
    let resp = get_object_from(&app, "products", "1").await;
    assert_ne!(resp.status(), StatusCode::OK, "primary should be cleared");

    // 6. Verify standard replica is also cleared (RED: sync not yet implemented)
    let resp = get_object_from(&app, "products_price_asc", "1").await;
    assert_ne!(
        resp.status(),
        StatusCode::OK,
        "standard replica should be cleared when primary is cleared"
    );
    let resp = get_object_from(&app, "products_price_asc", "2").await;
    assert_ne!(
        resp.status(),
        StatusCode::OK,
        "all docs in standard replica should be cleared when primary is cleared"
    );
}

/// Verify that a partial update on the primary is propagated to standard replicas.
#[tokio::test]
async fn test_replica_sync_partial_update_object() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state.clone());

    // 1. Set replicas on primary
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Seed both primary and replica with the same doc.
    state
        .manager
        .add_documents_sync("products", vec![make_doc("1", "Widget")])
        .await
        .unwrap();
    state.manager.create_tenant("products_price_asc").unwrap();
    state
        .manager
        .add_documents_sync("products_price_asc", vec![make_doc("1", "Widget")])
        .await
        .unwrap();

    // 3. Verify setup before update (prevents vacuous pass).
    let primary_before = get_object_json(&app, "products", "1").await;
    let replica_before = get_object_json(&app, "products_price_asc", "1").await;
    assert_eq!(primary_before["name"], "Widget");
    assert_eq!(replica_before["name"], "Widget");

    // 4. Partial-update primary.
    let resp = partial_update_for(&app, "products", "1", r#"{"name":"Widget Updated"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 5. Primary and replica should both have updated field value.
    let primary_after = get_object_json(&app, "products", "1").await;
    let replica_after = get_object_json(&app, "products_price_asc", "1").await;
    assert_eq!(primary_after["name"], "Widget Updated");
    assert_eq!(
        replica_after["name"], "Widget Updated",
        "partial updates should be synced to standard replicas"
    );
}

/// Verify that a standard replica can maintain its own `customRanking` independent of the primary.
#[tokio::test]
async fn test_replica_sync_replica_custom_ranking_is_independent() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state);

    // 1. Configure primary with one standard replica.
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Set divergent custom ranking between primary and replica.
    let resp = post_settings_for(&app, "products", r#"{"customRanking": ["desc(price)"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = post_settings_for(
        &app,
        "products_price_asc",
        r#"{"customRanking": ["asc(price)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 3. Write docs only to primary; standard replica should receive mirrored docs.
    let resp = put_object_for(&app, "products", "1", r#"{"name":"shoe","price":20}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = put_object_for(&app, "products", "2", r#"{"name":"shoe","price":10}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = put_object_for(&app, "products", "3", r#"{"name":"shoe","price":30}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 4. Verify both indexes can return all docs for the same query.
    let primary_ids = search_hit_ids_for(&app, "products", "shoe").await;
    let replica_ids = search_hit_ids_for(&app, "products_price_asc", "shoe").await;

    // 5. Primary and replica should sort opposite by price due to independent settings.
    assert_eq!(
        primary_ids,
        vec!["3".to_string(), "1".to_string(), "2".to_string()],
        "primary should follow desc(price)"
    );
    assert_eq!(
        replica_ids,
        vec!["2".to_string(), "1".to_string(), "3".to_string()],
        "replica should follow asc(price)"
    );
}

/// Verify that declaring a virtual replica creates a settings-only directory with `primary` set.
#[tokio::test]
async fn test_virtual_replica_creates_settings_only_entry_with_primary() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state);

    let resp = post_settings_for(
        &app,
        "products",
        r#"{"replicas": ["virtual(products_relevance)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let virtual_settings = get_settings_json_for(&app, "products_relevance").await;
    assert_eq!(virtual_settings["primary"], "products");

    let virtual_index_path = tmp.path().join("products_relevance");
    assert!(virtual_index_path.join("settings.json").exists());
    assert!(
        !virtual_index_path.join("meta.json").exists(),
        "virtual replica should not have Tantivy physical index data"
    );
}

/// Verify that searching a virtual replica applies its own `customRanking` against the primary's data.
#[tokio::test]
async fn test_virtual_replica_search_uses_virtual_custom_ranking() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state);

    let resp = post_settings_for(
        &app,
        "products",
        r#"{"replicas": ["virtual(products_relevance)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = post_settings_for(&app, "products", r#"{"customRanking": ["desc(price)"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = post_settings_for(
        &app,
        "products_relevance",
        r#"{"customRanking": ["asc(price)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = put_object_for(&app, "products", "1", r#"{"name":"shoe","price":20}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = put_object_for(&app, "products", "2", r#"{"name":"shoe","price":10}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = put_object_for(&app, "products", "3", r#"{"name":"shoe","price":30}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let primary_ids = search_hit_ids_for(&app, "products", "shoe").await;
    let virtual_ids = search_hit_ids_for(&app, "products_relevance", "shoe").await;

    assert_eq!(
        primary_ids,
        vec!["3".to_string(), "1".to_string(), "2".to_string()],
        "primary should follow desc(price)"
    );
    assert_eq!(
        virtual_ids,
        vec!["2".to_string(), "1".to_string(), "3".to_string()],
        "virtual replica should follow asc(price) from its own settings"
    );
}

/// Verify that `relevancyStrictness` can be set on a virtual replica and is persisted.
#[tokio::test]
async fn test_virtual_replica_allows_relevancy_strictness_settings() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state);

    let resp = post_settings_for(
        &app,
        "products",
        r#"{"replicas": ["products_price_asc", "virtual(products_relevance)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp =
        post_settings_for(&app, "products_relevance", r#"{"relevancyStrictness": 90}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let settings = get_settings_json_for(&app, "products_relevance").await;
    assert_eq!(settings["relevancyStrictness"], serde_json::json!(90));
}

/// Verify that `relevancyStrictness` is rejected on primary indexes.
#[tokio::test]
async fn test_primary_rejects_relevancy_strictness_settings() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state);

    let resp = post_settings_for(&app, "products", r#"{"relevancyStrictness": 90}"#).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        body_str.contains("relevancyStrictness can only be set on virtual replica indices"),
        "unexpected error body: {body_str}"
    );
}

/// Verify that `relevancyStrictness` is rejected on standard replicas with physical index data.
#[tokio::test]
async fn test_standard_replica_rejects_relevancy_strictness_settings() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state);

    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp =
        post_settings_for(&app, "products_price_asc", r#"{"relevancyStrictness": 90}"#).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        body_str.contains("relevancyStrictness can only be set on virtual replica indices"),
        "unexpected error body: {body_str}"
    );
}

/// Verify that GET object on a virtual replica returns 404 since it has no physical documents.
#[tokio::test]
async fn test_virtual_replica_get_object_returns_404() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state);

    let resp = post_settings_for(
        &app,
        "products",
        r#"{"replicas": ["virtual(products_relevance)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = put_object_for(&app, "products", "1", r#"{"name":"shoe","price":20}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = get_object_from(&app, "products_relevance", "1").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Verify that PUT, batch, and auto-ID writes to a virtual replica are rejected with 400.
#[tokio::test]
async fn test_virtual_replica_rejects_writes_with_400() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state);

    let resp = post_settings_for(
        &app,
        "products",
        r#"{"replicas": ["virtual(products_relevance)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let put_resp = put_object_for(
        &app,
        "products_relevance",
        "1",
        r#"{"name":"shoe","price":20}"#,
    )
    .await;
    assert_eq!(put_resp.status(), StatusCode::BAD_REQUEST);

    let batch_resp = batch_add_docs(
        &app,
        "products_relevance",
        r#"{
            "requests": [
                {"action":"addObject","body":{"objectID":"1","name":"shoe","price":20}}
            ]
        }"#,
    )
    .await;
    assert_eq!(batch_resp.status(), StatusCode::BAD_REQUEST);

    let add_resp =
        add_record_auto_id_for(&app, "products_relevance", r#"{"name":"shoe","price":20}"#).await;
    assert_eq!(add_resp.status(), StatusCode::BAD_REQUEST);
}

// ── A6: `primary` read-only field ──────────────────────────────────

/// Verify that GET settings on a standard replica includes the `primary` field.
#[tokio::test]
async fn test_a6_get_settings_on_replica_returns_primary() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state);

    // Create primary with a standard replica
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // GET settings on the replica should include `primary`
    let settings = get_settings_json_for(&app, "products_price_asc").await;
    assert_eq!(
        settings["primary"], "products",
        "standard replica settings should include 'primary' pointing to primary index"
    );
}

/// Verify that GET settings on a primary index does not include a `primary` field.
#[tokio::test]
async fn test_a6_get_settings_on_primary_omits_primary() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state);

    // Create primary with a standard replica
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // GET settings on the primary should NOT include `primary`
    let settings = get_settings_json_for(&app, "products").await;
    assert!(
        settings.get("primary").is_none(),
        "primary index should NOT have 'primary' field in its settings, got: {:?}",
        settings.get("primary")
    );
}

/// Verify that a user-supplied `primary` field in a settings update is silently ignored.
#[tokio::test]
async fn test_a6_put_primary_on_replica_is_ignored() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router(state);

    // Create primary with replica
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify primary is "products" initially
    let settings = get_settings_json_for(&app, "products_price_asc").await;
    assert_eq!(settings["primary"], "products");

    // Attempt to change `primary` via settings PUT — should be ignored
    let resp = post_settings_for(
        &app,
        "products_price_asc",
        r#"{"primary": "evil_index", "customRanking": ["asc(price)"]}"#,
    )
    .await;
    assert_eq!(
        resp.status().as_u16() / 100,
        2,
        "settings update should succeed"
    );

    // Verify `primary` is still "products", not "evil_index"
    let settings = get_settings_json_for(&app, "products_price_asc").await;
    assert_eq!(
        settings["primary"], "products",
        "user-supplied `primary` should be ignored; system value should be preserved"
    );
    // But customRanking should have been applied
    assert_eq!(
        settings["customRanking"],
        serde_json::json!(["asc(price)"]),
        "other settings should still apply normally"
    );
}

// ── A5: `forwardToReplicas` query parameter ────────────────────────

/// Build a router that adds synonym and rule batch endpoints on top of the base replica-sync router.
fn replica_sync_router_with_synonyms_rules(state: Arc<AppState>) -> Router {
    use axum::routing::{get, post};
    crate::router::app_id_layer(
        Router::new()
            .route(
                "/1/indexes/:indexName/settings",
                get(get_settings).post(set_settings),
            )
            .route(
                "/1/indexes/:indexName/batch",
                post(crate::handlers::objects::add_documents),
            )
            .route(
                "/1/indexes/:indexName/:objectID",
                get(crate::handlers::objects::get_object)
                    .put(crate::handlers::objects::put_object)
                    .delete(crate::handlers::objects::delete_object),
            )
            .route(
                "/1/indexes/:indexName/:objectID/partial",
                post(crate::handlers::objects::partial_update_object),
            )
            .route(
                "/1/indexes/:indexName/clear",
                post(crate::handlers::indices::clear_index),
            )
            .route(
                "/1/indexes/:indexName/query",
                post(crate::handlers::search::search),
            )
            .route(
                "/1/indexes/:indexName",
                post(crate::handlers::objects::add_record_auto_id),
            )
            .route(
                "/1/indexes/:indexName/synonyms/batch",
                post(crate::handlers::synonyms::save_synonyms),
            )
            .route(
                "/1/indexes/:indexName/rules/batch",
                post(crate::handlers::rules::save_rules),
            )
            .with_state(state),
    )
}

/// POST settings to a named index, optionally appending `?forwardToReplicas=true`.
async fn post_settings_for_with_forward(
    app: &Router,
    index_name: &str,
    body: &str,
    forward: bool,
) -> axum::http::Response<Body> {
    let uri = if forward {
        format!("/1/indexes/{}/settings?forwardToReplicas=true", index_name)
    } else {
        format!("/1/indexes/{}/settings", index_name)
    };
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// POST a synonyms batch to the named index, optionally appending `?forwardToReplicas=true`.
async fn post_synonyms_batch(
    app: &Router,
    index_name: &str,
    body: &str,
    forward: bool,
) -> axum::http::Response<Body> {
    let uri = if forward {
        format!(
            "/1/indexes/{}/synonyms/batch?forwardToReplicas=true",
            index_name
        )
    } else {
        format!("/1/indexes/{}/synonyms/batch", index_name)
    };
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Verify that `forwardToReplicas=true` propagates settings to both standard and virtual replicas.
#[tokio::test]
async fn test_a5_forward_to_replicas_propagates_settings() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router_with_synonyms_rules(state);

    // Create primary with standard + virtual replicas
    let resp = post_settings_for(
        &app,
        "products",
        r#"{"replicas": ["products_price_asc", "virtual(products_relevance)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Update primary settings with forwardToReplicas=true
    let resp = post_settings_for_with_forward(
        &app,
        "products",
        r#"{"searchableAttributes": ["name", "description"]}"#,
        true,
    )
    .await;
    assert_eq!(resp.status().as_u16() / 100, 2);

    // Verify standard replica got the setting
    let std_settings = get_settings_json_for(&app, "products_price_asc").await;
    assert_eq!(
        std_settings["searchableAttributes"],
        serde_json::json!(["name", "description"]),
        "standard replica should receive forwarded settings"
    );

    // Verify virtual replica got the setting
    let virt_settings = get_settings_json_for(&app, "products_relevance").await;
    assert_eq!(
        virt_settings["searchableAttributes"],
        serde_json::json!(["name", "description"]),
        "virtual replica should receive forwarded settings"
    );
}

/// Verify that omitting `forwardToReplicas` leaves replica settings unchanged.
#[tokio::test]
async fn test_a5_no_forward_does_not_propagate_settings() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router_with_synonyms_rules(state);

    // Create primary with a standard replica
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Set initial searchableAttributes on the replica so we have a baseline
    let resp = post_settings_for(
        &app,
        "products_price_asc",
        r#"{"searchableAttributes": ["name"]}"#,
    )
    .await;
    assert_eq!(resp.status().as_u16() / 100, 2);

    // Update primary settings WITHOUT forwardToReplicas (default)
    let resp = post_settings_for(
        &app,
        "products",
        r#"{"searchableAttributes": ["name", "description", "brand"]}"#,
    )
    .await;
    assert_eq!(resp.status().as_u16() / 100, 2);

    // Verify replica was NOT updated
    let replica_settings = get_settings_json_for(&app, "products_price_asc").await;
    assert_eq!(
        replica_settings["searchableAttributes"],
        serde_json::json!(["name"]),
        "replica settings should not change without forwardToReplicas"
    );
}

/// Verify that a synonym batch with `forwardToReplicas=true` propagates synonyms to replicas.
#[tokio::test]
async fn test_a5_forward_to_replicas_synonyms_batch() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router_with_synonyms_rules(state);

    // Create primary with a standard replica
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Batch synonyms on primary with forwardToReplicas=true
    let resp = post_synonyms_batch(
        &app,
        "products",
        r#"[{"objectID": "syn1", "type": "synonym", "synonyms": ["phone", "mobile", "cell"]}]"#,
        true,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify the synonym was propagated to the replica
    let synonyms_path = tmp.path().join("products_price_asc").join("synonyms.json");
    assert!(
        synonyms_path.exists(),
        "synonyms.json should exist on replica after forwardToReplicas"
    );
    let replica_synonyms: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&synonyms_path).unwrap()).unwrap();
    let syns = replica_synonyms
        .as_array()
        .expect("synonyms should be array");
    assert!(
        syns.iter().any(|s| s["objectID"] == "syn1"),
        "replica should have syn1 after forward"
    );
}

/// Verify that forwarding empty `attributesForFaceting` and `queryLanguages` clears them on replicas.
#[tokio::test]
async fn test_a5_forward_to_replicas_propagates_empty_list_clears() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router_with_synonyms_rules(state);

    // Create primary with a standard replica
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Seed replica with non-empty values so clear behavior is observable
    let resp = post_settings_for(
        &app,
        "products_price_asc",
        r#"{"attributesForFaceting": ["category"], "queryLanguages": ["en"]}"#,
    )
    .await;
    assert_eq!(resp.status().as_u16() / 100, 2);

    // Forward empty lists from primary; replica should be cleared to empty lists too
    let resp = post_settings_for_with_forward(
        &app,
        "products",
        r#"{"attributesForFaceting": [], "queryLanguages": []}"#,
        true,
    )
    .await;
    assert_eq!(resp.status().as_u16() / 100, 2);

    let replica_settings = get_settings_json_for(&app, "products_price_asc").await;
    assert_eq!(
        replica_settings["attributesForFaceting"],
        serde_json::Value::Null,
        "forwardToReplicas should propagate empty attributesForFaceting to clear replica"
    );
    assert_eq!(
        replica_settings
            .get("queryLanguages")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        serde_json::Value::Null,
        "forwardToReplicas should propagate empty queryLanguages to clear replica"
    );
}

/// POST a rules batch to the named index, optionally appending `?forwardToReplicas=true`.
async fn post_rules_batch(
    app: &Router,
    index_name: &str,
    body: &str,
    forward: bool,
) -> axum::http::Response<Body> {
    let uri = if forward {
        format!(
            "/1/indexes/{}/rules/batch?forwardToReplicas=true",
            index_name
        )
    } else {
        format!("/1/indexes/{}/rules/batch", index_name)
    };
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Verify that a rules batch with `forwardToReplicas=true` propagates rules to replicas.
#[tokio::test]
async fn test_a5_forward_to_replicas_rules_batch() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router_with_synonyms_rules(state);

    // Create primary with a standard replica
    let resp = post_settings_for(&app, "products", r#"{"replicas": ["products_price_asc"]}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Batch rules on primary with forwardToReplicas=true
    let resp = post_rules_batch(
        &app,
        "products",
        r#"[{"objectID": "rule1", "condition": {"pattern": "phone", "anchoring": "contains"}, "consequence": {"params": {"query": "smartphone"}}}]"#,
        true,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify the rule was propagated to the replica
    let rules_path = tmp.path().join("products_price_asc").join("rules.json");
    assert!(
        rules_path.exists(),
        "rules.json should exist on replica after forwardToReplicas"
    );
    let replica_rules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rules_path).unwrap()).unwrap();
    let rules = replica_rules.as_array().expect("rules should be array");
    assert!(
        rules.iter().any(|r| r["objectID"] == "rule1"),
        "replica should have rule1 after forward"
    );
}

// ── A7: List indices — replica fields ──────────────────────────────

/// Verify that `GET /1/indexes` includes `replicas`, `primary`, and `virtual` metadata on the appropriate entries.
#[tokio::test]
async fn test_a7_list_indices_shows_replica_metadata() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = crate::router::app_id_layer(
        Router::new()
            .route(
                "/1/indexes/:indexName/settings",
                axum::routing::get(get_settings).post(set_settings),
            )
            .route(
                "/1/indexes",
                axum::routing::get(crate::handlers::indices::list_indices),
            )
            .route(
                "/1/indexes/:indexName/batch",
                axum::routing::post(crate::handlers::objects::add_documents),
            )
            .with_state(state),
    );

    // Create primary with standard + virtual replicas
    let resp = post_settings_for(
        &app,
        "products",
        r#"{"replicas": ["products_price_asc", "virtual(products_relevance)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Add a doc to primary so it shows up in list
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/products/batch")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"requests": [{"action": "addObject", "body": {"objectID": "1", "name": "test"}}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // GET /1/indexes
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/1/indexes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = json["items"].as_array().unwrap();

    // Find primary
    let primary = items.iter().find(|i| i["name"] == "products").unwrap();
    assert_eq!(
        primary["replicas"],
        serde_json::json!(["products_price_asc", "virtual(products_relevance)"]),
        "primary should list its replicas"
    );
    assert!(
        primary.get("primary").is_none() || primary["primary"].is_null(),
        "primary should NOT have a 'primary' field"
    );

    // Find standard replica
    let std_replica = items
        .iter()
        .find(|i| i["name"] == "products_price_asc")
        .unwrap();
    assert_eq!(
        std_replica["primary"], "products",
        "standard replica should have primary field"
    );
    assert!(
        std_replica.get("virtual").is_none() || std_replica["virtual"].is_null(),
        "standard replica should NOT be marked virtual"
    );

    // Find virtual replica
    let virt_replica = items
        .iter()
        .find(|i| i["name"] == "products_relevance")
        .unwrap();
    assert_eq!(
        virt_replica["primary"], "products",
        "virtual replica should have primary field"
    );
    assert_eq!(
        virt_replica["virtual"], true,
        "virtual replica should be marked virtual: true"
    );
}

// ── D1: Replica Sort E2E Lifecycle Test ────────────────────────────

/// End-to-end lifecycle test: create a primary with standard and virtual replicas, add 10 products, verify independent sort orders, forward settings, delete a product, and confirm consistency across all views.
#[tokio::test]
async fn test_d1_replica_sort_e2e_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router_with_synonyms_rules(state);

    // 1. Create "products" index with standard + virtual replicas
    let resp = post_settings_for(
        &app,
        "products",
        r#"{"replicas": ["products_price_asc", "virtual(products_relevance)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Set customRanking on standard replica: sort by price ascending
    let resp = post_settings_for(
        &app,
        "products_price_asc",
        r#"{"customRanking": ["asc(price)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 3. Set customRanking on virtual replica: sort by popularity descending
    let resp = post_settings_for(
        &app,
        "products_relevance",
        r#"{"customRanking": ["desc(popularity)"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 4. Add 10 products with varying prices and popularity scores to primary
    let products = [
        (
            "1",
            r#"{"name":"product alpha","price":50,"popularity":200}"#,
        ),
        (
            "2",
            r#"{"name":"product beta","price":10,"popularity":800}"#,
        ),
        (
            "3",
            r#"{"name":"product gamma","price":90,"popularity":100}"#,
        ),
        (
            "4",
            r#"{"name":"product delta","price":30,"popularity":600}"#,
        ),
        (
            "5",
            r#"{"name":"product epsilon","price":70,"popularity":400}"#,
        ),
        (
            "6",
            r#"{"name":"product zeta","price":20,"popularity":900}"#,
        ),
        ("7", r#"{"name":"product eta","price":80,"popularity":300}"#),
        (
            "8",
            r#"{"name":"product theta","price":40,"popularity":700}"#,
        ),
        (
            "9",
            r#"{"name":"product iota","price":60,"popularity":500}"#,
        ),
        (
            "10",
            r#"{"name":"product kappa","price":100,"popularity":50}"#,
        ),
    ];
    for (id, body) in &products {
        let resp = put_object_for(&app, "products", id, body).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "failed to add product {}",
            id
        );
    }

    // 5. Search primary — default ranking (no customRanking set), all 10 returned
    let primary_ids = search_hit_ids_for(&app, "products", "product").await;
    assert_eq!(
        primary_ids.len(),
        10,
        "primary should return all 10 products"
    );

    // 6. Search standard replica — sorted by price ascending
    //    Expected: 2(10), 6(20), 4(30), 8(40), 1(50), 9(60), 5(70), 7(80), 3(90), 10(100)
    let price_asc_ids = search_hit_ids_for(&app, "products_price_asc", "product").await;
    assert_eq!(
        price_asc_ids,
        vec!["2", "6", "4", "8", "1", "9", "5", "7", "3", "10"],
        "standard replica should sort by price ascending"
    );

    // 7. Search virtual replica — ranked by desc(popularity)
    //    Expected: 6(900), 2(800), 8(700), 4(600), 9(500), 5(400), 7(300), 1(200), 3(100), 10(50)
    let popularity_ids = search_hit_ids_for(&app, "products_relevance", "product").await;
    assert_eq!(
        popularity_ids,
        vec!["6", "2", "8", "4", "9", "5", "7", "1", "3", "10"],
        "virtual replica should sort by popularity descending"
    );

    // 8. Update settings on primary with forwardToReplicas=true — verify replicas updated
    let resp = post_settings_for_with_forward(
        &app,
        "products",
        r#"{"searchableAttributes": ["name", "description"]}"#,
        true,
    )
    .await;
    assert_eq!(resp.status().as_u16() / 100, 2);

    let std_settings = get_settings_json_for(&app, "products_price_asc").await;
    assert_eq!(
        std_settings["searchableAttributes"],
        serde_json::json!(["name", "description"]),
        "standard replica should receive forwarded searchableAttributes"
    );
    let virt_settings = get_settings_json_for(&app, "products_relevance").await;
    assert_eq!(
        virt_settings["searchableAttributes"],
        serde_json::json!(["name", "description"]),
        "virtual replica should receive forwarded searchableAttributes"
    );

    // 9. Delete product 6 from primary — verify gone from all views
    let resp = delete_object_from(&app, "products", "6").await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "delete from primary should succeed"
    );

    // Verify gone from primary
    let resp = get_object_from(&app, "products", "6").await;
    assert_ne!(
        resp.status(),
        StatusCode::OK,
        "product 6 should be deleted from primary"
    );

    // Verify gone from standard replica (synced on delete)
    let resp = get_object_from(&app, "products_price_asc", "6").await;
    assert_ne!(
        resp.status(),
        StatusCode::OK,
        "product 6 should be deleted from standard replica"
    );

    // Verify absent from virtual replica search (reads primary data)
    let virtual_ids_after = search_hit_ids_for(&app, "products_relevance", "product").await;
    assert_eq!(
        virtual_ids_after.len(),
        9,
        "virtual replica should return 9 products after delete"
    );
    assert!(
        !virtual_ids_after.contains(&"6".to_string()),
        "deleted product should not appear in virtual replica search"
    );
}

/// Verify that `queryType` roundtrips through POST and GET settings.
#[tokio::test]
async fn test_query_type_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    // Default should be "prefixLast"
    let json = get_settings_json(&app).await;
    assert_eq!(
        json["queryType"], "prefixLast",
        "default queryType should be prefixLast"
    );

    // Set to prefixAll
    let resp = post_settings(&app, r#"{"queryType":"prefixAll"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(
        json["queryType"], "prefixAll",
        "queryType should persist as prefixAll after update"
    );

    // Set to prefixNone
    let resp = post_settings(&app, r#"{"queryType":"prefixNone"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = get_settings_json(&app).await;
    assert_eq!(
        json["queryType"], "prefixNone",
        "queryType should persist as prefixNone after update"
    );
}

/// Verify that `queryType` is persisted to disk and survives a reload.
#[tokio::test]
async fn test_query_type_persists_to_disk() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    post_settings(&app, r#"{"queryType":"prefixNone"}"#).await;

    let settings_path = tmp.path().join("test_idx").join("settings.json");
    let loaded = IndexSettings::load(&settings_path).unwrap();
    assert_eq!(
        loaded.query_type, "prefixNone",
        "queryType should be persisted to disk"
    );
}
#[tokio::test]
async fn test_ranking_roundtrip_is_supported() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(
        &app,
        r#"{"ranking":["typo","geo","words","filters","proximity","exact","attribute","custom"]}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        response_json.get("unsupportedParams").is_none(),
        "ranking should no longer be reported as unsupported"
    );

    let json = get_settings_json(&app).await;
    assert_eq!(
        json["ranking"],
        serde_json::json!([
            "typo",
            "geo",
            "words",
            "filters",
            "proximity",
            "exact",
            "attribute",
            "custom"
        ]),
        "ranking should persist through GET after update"
    );
}
#[tokio::test]
async fn test_stage4_structural_settings_roundtrip_is_supported() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_settings(
        &app,
        r#"{
            "advancedSyntaxFeatures": ["exactPhrase", "excludeWords"],
            "sortFacetValuesBy": "alpha",
            "snippetEllipsisText": "...",
            "restrictHighlightAndSnippetArrays": true,
            "minProximity": 4,
            "disableExactOnAttributes": ["sku"],
            "replaceSynonymsInHighlight": false,
            "attributeCriteriaComputedByMinProximity": true,
            "enableReRanking": false,
            "disableTypoToleranceOnWords": ["iphone"],
            "disableTypoToleranceOnAttributes": ["sku"]
        }"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        response_json.get("unsupportedParams").is_none(),
        "stage 4 structural settings should not be reported as unsupported"
    );

    let json = get_settings_json(&app).await;
    assert_eq!(
        json["advancedSyntaxFeatures"],
        serde_json::json!(["exactPhrase", "excludeWords"])
    );
    assert_eq!(json["sortFacetValuesBy"], serde_json::json!("alpha"));
    assert_eq!(json["snippetEllipsisText"], serde_json::json!("..."));
    assert_eq!(
        json["restrictHighlightAndSnippetArrays"],
        serde_json::json!(true)
    );
    assert_eq!(json["minProximity"], serde_json::json!(4));
    assert_eq!(json["disableExactOnAttributes"], serde_json::json!(["sku"]));
    assert_eq!(json["replaceSynonymsInHighlight"], serde_json::json!(false));
    assert_eq!(
        json["attributeCriteriaComputedByMinProximity"],
        serde_json::json!(true)
    );
    assert_eq!(json["enableReRanking"], serde_json::json!(false));
    assert_eq!(
        json["disableTypoToleranceOnWords"],
        serde_json::json!(["iphone"])
    );
    assert_eq!(
        json["disableTypoToleranceOnAttributes"],
        serde_json::json!(["sku"])
    );
}
#[tokio::test]
async fn test_forward_to_replicas_rejects_invalid_boolean_value() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = settings_router(state);

    let resp = post_json(
        &app,
        "/1/indexes/test_idx/settings?forwardToReplicas=maybe",
        r#"{"searchableAttributes":["title"]}"#,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_synonyms_batch_rejects_invalid_forward_to_replicas_boolean_value() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router_with_synonyms_rules(state);

    let resp = post_json(
        &app,
        "/1/indexes/test_idx/synonyms/batch?forwardToReplicas=maybe",
        r#"[{"objectID":"syn1","type":"synonym","synonyms":["phone","mobile"]}]"#,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_synonyms_batch_rejects_invalid_replace_existing_boolean_value() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router_with_synonyms_rules(state);

    let resp = post_json(
        &app,
        "/1/indexes/test_idx/synonyms/batch?replaceExistingSynonyms=maybe",
        r#"[{"objectID":"syn1","type":"synonym","synonyms":["phone","mobile"]}]"#,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_rules_batch_rejects_invalid_forward_to_replicas_boolean_value() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router_with_synonyms_rules(state);

    let resp = post_json(
        &app,
        "/1/indexes/test_idx/rules/batch?forwardToReplicas=maybe",
        r#"[{"objectID":"rule1","condition":{"pattern":"phone","anchoring":"contains"},"consequence":{"params":{"query":"smartphone"}}}]"#,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_rules_batch_rejects_invalid_clear_existing_boolean_value() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router_with_synonyms_rules(state);

    let resp = post_json(
        &app,
        "/1/indexes/test_idx/rules/batch?clearExistingRules=maybe",
        r#"[{"objectID":"rule1","condition":{"pattern":"phone","anchoring":"contains"},"consequence":{"params":{"query":"smartphone"}}}]"#,
    )
    .await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
