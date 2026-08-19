//! Stub summary for dictionaries.rs.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use flapjack::dictionaries::{
    BatchDictionaryRequest, DictionaryName, DictionarySearchRequest, DictionarySettings,
};
use std::sync::Arc;

use super::AppState;
use crate::auth::AuthenticatedAppId;
use crate::error_response::json_error;

fn dict_error_response(err: flapjack::dictionaries::DictionaryError) -> Response {
    use flapjack::dictionaries::DictionaryError;
    let (status, message) = match &err {
        DictionaryError::InvalidDictionaryName(_) => (StatusCode::BAD_REQUEST, err.to_string()),
        DictionaryError::MissingObjectId => (StatusCode::BAD_REQUEST, err.to_string()),
        DictionaryError::InvalidEntry(_) => (StatusCode::BAD_REQUEST, err.to_string()),
        DictionaryError::IoError(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal storage error".to_string(),
        ),
        DictionaryError::SerdeError(_) => (StatusCode::BAD_REQUEST, err.to_string()),
    };
    json_error(status, message)
}

fn bad_request_response(message: impl Into<String>) -> Response {
    json_error(StatusCode::BAD_REQUEST, message)
}

/// POST /1/dictionaries/{dictionaryName}/batch
#[utoipa::path(post, path = "/1/dictionaries/{dictionaryName}/batch", tag = "dictionaries",
    params(("dictionaryName" = DictionaryName, Path, description = "Dictionary name")),
    request_body = flapjack::dictionaries::BatchDictionaryRequest,
    responses(
        (status = 200, description = "Dictionary batch mutation result", body = flapjack::dictionaries::MutationResponse),
        (status = 400, description = "Invalid dictionary name or batch payload")
    ),
    security(("api_key" = [])))]
pub async fn dictionary_batch(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(tenant)): Extension<AuthenticatedAppId>,
    Path(dict_name_str): Path<String>,
    Json(raw_body): Json<serde_json::Value>,
) -> Response {
    let dict_name = match dict_name_str.parse::<DictionaryName>() {
        Ok(n) => n,
        Err(e) => return bad_request_response(e.to_string()),
    };
    let body = match serde_json::from_value::<BatchDictionaryRequest>(raw_body) {
        Ok(body) => body,
        Err(e) => return bad_request_response(format!("invalid dictionary batch request: {}", e)),
    };

    match state.dictionary_manager.batch(&tenant, dict_name, &body) {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e) => dict_error_response(e),
    }
}

/// POST /1/dictionaries/{dictionaryName}/search
#[utoipa::path(post, path = "/1/dictionaries/{dictionaryName}/search", tag = "dictionaries",
    params(("dictionaryName" = DictionaryName, Path, description = "Dictionary name")),
    request_body = DictionarySearchRequest,
    responses(
        (status = 200, description = "Dictionary search results", body = flapjack::dictionaries::DictionarySearchResponse),
        (status = 400, description = "Invalid dictionary name or search payload")
    ),
    security(("api_key" = [])))]
pub async fn dictionary_search(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(tenant)): Extension<AuthenticatedAppId>,
    Path(dict_name_str): Path<String>,
    Json(body): Json<DictionarySearchRequest>,
) -> Response {
    let dict_name = match dict_name_str.parse::<DictionaryName>() {
        Ok(n) => n,
        Err(e) => return bad_request_response(e.to_string()),
    };

    match state.dictionary_manager.search(&tenant, dict_name, &body) {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e) => dict_error_response(e),
    }
}

/// GET /1/dictionaries/*/settings
#[utoipa::path(get, path = "/1/dictionaries/{_wildcard}/settings", tag = "dictionaries",
    params(("_wildcard" = String, Path, description = "Wildcard placeholder")),
    responses(
        (status = 200, description = "Dictionary settings", body = DictionarySettings)
    ),
    security(("api_key" = [])))]
pub async fn dictionary_get_settings(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(tenant)): Extension<AuthenticatedAppId>,
) -> Response {
    match state.dictionary_manager.get_settings(&tenant) {
        Ok(settings) => (StatusCode::OK, Json(settings)).into_response(),
        Err(e) => dict_error_response(e),
    }
}

/// PUT /1/dictionaries/*/settings
#[utoipa::path(put, path = "/1/dictionaries/{_wildcard}/settings", tag = "dictionaries",
    params(("_wildcard" = String, Path, description = "Wildcard placeholder")),
    request_body = DictionarySettings,
    responses(
        (status = 200, description = "Dictionary settings updated", body = flapjack::dictionaries::MutationResponse),
        (status = 400, description = "Invalid settings payload")
    ),
    security(("api_key" = [])))]
pub async fn dictionary_set_settings(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(tenant)): Extension<AuthenticatedAppId>,
    Json(body): Json<DictionarySettings>,
) -> Response {
    match state.dictionary_manager.set_settings(&tenant, &body) {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e) => dict_error_response(e),
    }
}

/// GET /1/dictionaries/*/languages
#[utoipa::path(get, path = "/1/dictionaries/{_wildcard}/languages", tag = "dictionaries",
    params(("_wildcard" = String, Path, description = "Wildcard placeholder")),
    responses(
        (status = 200, description = "Custom entry counts grouped by language", body = std::collections::HashMap<String, flapjack::dictionaries::LanguageDictionaryCounts>)
    ),
    security(("api_key" = [])))]
pub async fn dictionary_list_languages(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(tenant)): Extension<AuthenticatedAppId>,
) -> Response {
    match state.dictionary_manager.list_languages(&tenant) {
        Ok(langs) => (StatusCode::OK, Json(langs)).into_response(),
        Err(e) => dict_error_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use axum::Router;
    use tempfile::TempDir;
    use tower::ServiceExt;

    fn with_authenticated_app_id(mut request: Request<Body>, app_id: &str) -> Request<Body> {
        request
            .extensions_mut()
            .insert(crate::auth::AuthenticatedAppId(app_id.to_string()));
        request
    }

    fn app(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/1/dictionaries/:dictionaryName/batch",
                post(dictionary_batch),
            )
            .route(
                "/1/dictionaries/:dictionaryName/search",
                post(dictionary_search),
            )
            .with_state(state)
    }

    fn app_with_settings_routes(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/1/dictionaries/x/settings",
                get(dictionary_get_settings).put(dictionary_set_settings),
            )
            .route(
                "/1/dictionaries/x/languages",
                get(dictionary_list_languages),
            )
            .with_state(state)
    }

    /// Verify that a batch request with an unrecognized action (e.g. `updateEntry`) returns 400 and lists the valid actions (`addEntry`, `deleteEntry`) in the error message.
    #[tokio::test]
    async fn dictionary_batch_invalid_action_returns_400_with_actionable_message() {
        let tmp = TempDir::new().unwrap();
        let app = app(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

        let resp = app
            .oneshot(with_authenticated_app_id(
                Request::builder()
                    .method("POST")
                    .uri("/1/dictionaries/stopwords/batch")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "clearExistingDictionaryEntries": false,
                            "requests": [
                                {
                                    "action": "updateEntry",
                                    "body": { "objectID": "sw-1" }
                                }
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
                "test-app",
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let message = json["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("addEntry") && message.contains("deleteEntry"),
            "error should describe allowed action values: {:?}",
            json
        );
    }

    /// Verify that a stopword `addEntry` missing the required `word` field returns 400 with an error message naming the missing field.
    #[tokio::test]
    async fn dictionary_batch_malformed_stopword_entry_returns_400_with_field_error() {
        let tmp = TempDir::new().unwrap();
        let app = app(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

        let resp = app
            .oneshot(with_authenticated_app_id(
                Request::builder()
                    .method("POST")
                    .uri("/1/dictionaries/stopwords/batch")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "clearExistingDictionaryEntries": false,
                            "requests": [
                                {
                                    "action": "addEntry",
                                    "body": {
                                        "objectID": "sw-1",
                                        "language": "en"
                                    }
                                }
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
                "test-app",
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["message"]
                .as_str()
                .unwrap_or_default()
                .contains("word"),
            "error should mention the missing required field: {:?}",
            json
        );
    }

    /// Verify dictionary settings can be updated and read back, and languages endpoint remains callable.
    #[tokio::test]
    async fn dictionary_settings_round_trip_and_languages_endpoint_succeeds() {
        let tmp = TempDir::new().unwrap();
        let app = app_with_settings_routes(
            crate::test_helpers::TestStateBuilder::new(&tmp).build_shared(),
        );

        let set_resp = app
            .clone()
            .oneshot(with_authenticated_app_id(
                Request::builder()
                    .method("PUT")
                    .uri("/1/dictionaries/x/settings")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "disableStandardEntries": {
                                "stopwords": { "en": true }
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
                "test-app",
            ))
            .await
            .unwrap();
        assert_eq!(set_resp.status(), StatusCode::OK);

        let get_resp = app
            .clone()
            .oneshot(with_authenticated_app_id(
                Request::builder()
                    .method("GET")
                    .uri("/1/dictionaries/x/settings")
                    .body(Body::empty())
                    .unwrap(),
                "test-app",
            ))
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let get_json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(get_resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(get_json["disableStandardEntries"]["stopwords"]["en"]
            .as_bool()
            .unwrap_or(false));

        let langs_resp = app
            .oneshot(with_authenticated_app_id(
                Request::builder()
                    .method("GET")
                    .uri("/1/dictionaries/x/languages")
                    .body(Body::empty())
                    .unwrap(),
                "test-app",
            ))
            .await
            .unwrap();
        assert_eq!(langs_resp.status(), StatusCode::OK);
        let langs_json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(langs_resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(langs_json.as_object().is_some());
    }
    /// TODO: Document dictionary_handlers_isolate_data_per_authenticated_app_id.
    #[tokio::test]
    async fn dictionary_handlers_isolate_data_per_authenticated_app_id() {
        let tmp = TempDir::new().unwrap();
        let app = app(crate::test_helpers::TestStateBuilder::new(&tmp).build_shared());

        let mut app_a_write = Request::builder()
            .method("POST")
            .uri("/1/dictionaries/stopwords/batch")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "clearExistingDictionaryEntries": false,
                    "requests": [
                        {
                            "action": "addEntry",
                            "body": {
                                "objectID": "app-a-1",
                                "word": "alpha",
                                "language": "en"
                            }
                        }
                    ]
                })
                .to_string(),
            ))
            .unwrap();
        app_a_write
            .extensions_mut()
            .insert(crate::auth::AuthenticatedAppId("app-a".to_string()));

        let app_a_write_resp = app.clone().oneshot(app_a_write).await.unwrap();
        assert_eq!(app_a_write_resp.status(), StatusCode::OK);

        let mut app_b_write = Request::builder()
            .method("POST")
            .uri("/1/dictionaries/stopwords/batch")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "clearExistingDictionaryEntries": false,
                    "requests": [
                        {
                            "action": "addEntry",
                            "body": {
                                "objectID": "app-b-1",
                                "word": "beta",
                                "language": "en"
                            }
                        }
                    ]
                })
                .to_string(),
            ))
            .unwrap();
        app_b_write
            .extensions_mut()
            .insert(crate::auth::AuthenticatedAppId("app-b".to_string()));

        let app_b_write_resp = app.clone().oneshot(app_b_write).await.unwrap();
        assert_eq!(app_b_write_resp.status(), StatusCode::OK);

        let mut app_a_search = Request::builder()
            .method("POST")
            .uri("/1/dictionaries/stopwords/search")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "query": "alpha"
                })
                .to_string(),
            ))
            .unwrap();
        app_a_search
            .extensions_mut()
            .insert(crate::auth::AuthenticatedAppId("app-a".to_string()));
        let app_a_search_resp = app.clone().oneshot(app_a_search).await.unwrap();
        assert_eq!(app_a_search_resp.status(), StatusCode::OK);
        let app_a_search_json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(app_a_search_resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            app_a_search_json["nbHits"].as_u64(),
            Some(1),
            "app-a should see only its own alpha entry"
        );

        let mut app_b_search = Request::builder()
            .method("POST")
            .uri("/1/dictionaries/stopwords/search")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "query": "alpha"
                })
                .to_string(),
            ))
            .unwrap();
        app_b_search
            .extensions_mut()
            .insert(crate::auth::AuthenticatedAppId("app-b".to_string()));
        let app_b_search_resp = app.oneshot(app_b_search).await.unwrap();
        assert_eq!(app_b_search_resp.status(), StatusCode::OK);
        let app_b_search_json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(app_b_search_resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            app_b_search_json["nbHits"].as_u64(),
            Some(0),
            "app-b must not see app-a entries"
        );
    }
}
