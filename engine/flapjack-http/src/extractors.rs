//! Stub summary for extractors.rs.
use axum::{
    async_trait,
    extract::{FromRequestParts, Path},
    http::{request::Parts, StatusCode},
    response::Response,
};

use crate::error_response::json_error;

/// HTTP-side wrapper around `flapjack::validate_index_name` that maps
/// `FlapjackError` to `(StatusCode::BAD_REQUEST, String)` — the error
/// type used by most handler `Result` return types.
///
/// This is the single source of truth for the HTTP error mapping.
/// Both `ValidatedIndexName` and multi-param handlers delegate here.
pub fn validate_index_http(name: &str) -> Result<(), (StatusCode, String)> {
    flapjack::validate_index_name(name).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

/// Axum extractor that validates a single `Path<String>` as a safe index name.
///
/// Use in place of `Path(index_name): Path<String>` on handlers whose only
/// path parameter is an index name. Rejects invalid names with a 400 JSON
/// response consistent with the Algolia error format from Stage 3.
pub struct ValidatedIndexName(pub String);

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for ValidatedIndexName {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Path(index_name) = Path::<String>::from_request_parts(parts, state)
            .await
            .map_err(|e| json_error(StatusCode::BAD_REQUEST, e.to_string()))?;

        validate_index_http(&index_name).map_err(|(status, msg)| json_error(status, msg))?;

        Ok(ValidatedIndexName(index_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use axum::{body::Body, routing::get, Router};
    use tower::ServiceExt;

    // ── validate_index_http unit tests ──

    #[test]
    fn valid_index_name_passes() {
        assert!(validate_index_http("my-index").is_ok());
        assert!(validate_index_http("test_123").is_ok());
    }

    #[test]
    fn empty_name_rejected() {
        let err = validate_index_http("").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("empty"), "message was: {}", err.1);
    }

    #[test]
    fn path_traversal_rejected() {
        let err = validate_index_http("../etc").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("path traversal"), "message was: {}", err.1);
    }

    #[test]
    fn slash_rejected() {
        let err = validate_index_http("foo/bar").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    // ── ValidatedIndexName extractor integration tests ──

    async fn handler(ValidatedIndexName(name): ValidatedIndexName) -> String {
        format!("ok:{name}")
    }

    fn test_router() -> Router {
        Router::new().route("/:index_name", get(handler))
    }

    #[tokio::test]
    async fn extractor_accepts_valid_name() {
        let resp = test_router()
            .oneshot(
                Request::builder()
                    .uri("/my-index")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"ok:my-index");
    }
    /// TODO: Document extractor_rejects_path_traversal_with_400_json.
    #[tokio::test]
    async fn extractor_rejects_path_traversal_with_400_json() {
        let resp = test_router()
            .oneshot(
                Request::builder()
                    .uri("/..%2Fetc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], 400);
        assert!(json["message"].as_str().unwrap().contains("path traversal"));
    }
    /// TODO: Document extractor_rejects_null_byte_with_400.
    #[tokio::test]
    async fn extractor_rejects_null_byte_with_400() {
        let app = Router::new().route(
            "/idx/:name",
            get(|ValidatedIndexName(n): ValidatedIndexName| async move { n }),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/idx/%00")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
