use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use flapjack::error::FlapjackError;
use serde_json::json;

pub fn json_error_parts(
    status: StatusCode,
    message: impl Into<String>,
) -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        status,
        axum::Json(json!({
            "message": message.into(),
            "status": status.as_u16(),
        })),
    )
}

pub fn json_error_parts_with_code(
    status: StatusCode,
    code: impl Into<String>,
    message: impl Into<String>,
) -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        status,
        axum::Json(json!({
            "message": message.into(),
            "status": status.as_u16(),
            "code": code.into(),
        })),
    )
}

pub fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    json_error_parts(status, message).into_response()
}

/// Unified error type for HTTP handlers that need both `FlapjackError` semantics
/// (auto-sanitized 500s, variant-driven status codes) and explicit status/message
/// overrides (e.g. 404 with entity-specific text).
///
/// All rendering produces the standard `{message, status}` JSON wire format.
/// Internal error sanitization is delegated to `FlapjackError::IntoResponse`.
pub enum HandlerError {
    /// Delegate status code, message text, and 500 sanitization to `FlapjackError`.
    Core(FlapjackError),
    /// Explicit status and message for cases where no `FlapjackError` variant
    /// carries the right combination (e.g. 404 with a custom entity ID in the message).
    Custom { status: StatusCode, message: String },
    /// Explicit status, code, and message for handlers that need a stable
    /// machine-readable error code without changing the standard JSON fields.
    Coded {
        status: StatusCode,
        code: String,
        message: String,
    },
}

impl IntoResponse for HandlerError {
    fn into_response(self) -> Response {
        match self {
            HandlerError::Core(e) => e.into_response(),
            HandlerError::Custom { status, message } => json_error(status, message),
            HandlerError::Coded {
                status,
                code,
                message,
            } => json_error_parts_with_code(status, code, message).into_response(),
        }
    }
}

impl From<std::io::Error> for HandlerError {
    fn from(e: std::io::Error) -> Self {
        HandlerError::Core(FlapjackError::from(e))
    }
}

impl From<serde_json::Error> for HandlerError {
    fn from(e: serde_json::Error) -> Self {
        HandlerError::Core(FlapjackError::from(e))
    }
}

impl From<FlapjackError> for HandlerError {
    fn from(e: FlapjackError) -> Self {
        HandlerError::Core(e)
    }
}

impl From<String> for HandlerError {
    /// Treat bare `String` errors as internal storage failures → sanitized 500.
    /// The real error is logged server-side; the response says "Internal server error".
    fn from(e: String) -> Self {
        tracing::error!("internal error: {e}");
        HandlerError::Core(FlapjackError::Io(e))
    }
}

impl From<(StatusCode, String)> for HandlerError {
    /// Bridge for helpers like `validate_index_http` that return `(StatusCode, String)`.
    fn from((status, message): (StatusCode, String)) -> Self {
        HandlerError::Custom { status, message }
    }
}

impl HandlerError {
    pub fn not_found(message: impl Into<String>) -> Self {
        HandlerError::Custom {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        HandlerError::Custom {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    pub fn coded(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        HandlerError::Coded {
            status,
            code: code.into(),
            message: message.into(),
        }
    }

    /// Force a sanitized 500 for server-side storage errors, regardless of the
    /// underlying error type. Use this for load/save operations where any failure
    /// (including serde parse errors from corrupt stored data) is a server fault,
    /// not a client input error.
    pub fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!("internal error: {e}");
        HandlerError::Custom {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "Internal server error".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn response_json(resp: Response) -> (StatusCode, serde_json::Value) {
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    #[test]
    fn json_error_parts_with_code_preserves_standard_shape() {
        let (status, axum::Json(body)) = json_error_parts_with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "migration_import_unavailable",
            "Migration import is temporarily unavailable",
        );

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body,
            json!({
                "message": "Migration import is temporarily unavailable",
                "status": 503,
                "code": "migration_import_unavailable"
            })
        );
    }

    #[tokio::test]
    async fn handler_error_coded_delegates_to_coded_json_helper() {
        let he = HandlerError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "migration_import_unavailable",
            "Migration import is temporarily unavailable",
        );
        let (status, json) = response_json(he.into_response()).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            json,
            json!({
                "message": "Migration import is temporarily unavailable",
                "status": 503,
                "code": "migration_import_unavailable"
            })
        );
    }

    // ── FlapjackError contract: std::io::Error → sanitized 500 ──

    #[test]
    fn io_error_through_flapjack_produces_sanitized_500() {
        let fe = FlapjackError::from(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "/secret/path/data.db not found",
        ));
        assert_eq!(fe.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn serde_error_through_flapjack_produces_400() {
        let se: serde_json::Error = serde_json::from_str::<String>("not json").unwrap_err();
        let fe = FlapjackError::from(se);
        assert_eq!(fe.status_code(), StatusCode::BAD_REQUEST);
    }

    // ── HandlerError adapter: delegates to FlapjackError ──
    #[tokio::test]
    async fn handler_error_core_io_produces_sanitized_500_json() {
        let he = HandlerError::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "/var/data/secret.db: permission denied",
        ));
        let (status, json) = response_json(he.into_response()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["status"], 500);
        assert_eq!(json["message"], "Internal server error");
        assert!(
            !json["message"]
                .as_str()
                .unwrap()
                .contains("/var/data/secret.db"),
            "internal paths must not leak in 500 responses"
        );
    }

    #[tokio::test]
    async fn handler_error_core_serde_json_produces_standard_400_json() {
        let serde_error = serde_json::from_str::<String>("not json").unwrap_err();
        let (status, json) = response_json(HandlerError::from(serde_error).into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["status"], 400);
        assert!(
            json["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("JSON error:")),
            "serde_json::Error responses should use the standard client-visible JSON message"
        );
    }

    #[tokio::test]
    async fn handler_error_custom_404_preserves_message() {
        let he = HandlerError::not_found("ObjectID abc123 does not exist");
        let (status, json) = response_json(he.into_response()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["status"], 404);
        assert_eq!(json["message"], "ObjectID abc123 does not exist");
    }

    #[tokio::test]
    async fn handler_error_custom_400_preserves_message() {
        let he = HandlerError::bad_request("hitsPerPage must be greater than 0");
        let (status, json) = response_json(he.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["status"], 400);
        assert_eq!(json["message"], "hitsPerPage must be greater than 0");
    }

    #[tokio::test]
    async fn handler_error_from_flapjack_delegates_correctly() {
        let he = HandlerError::from(FlapjackError::InvalidQuery("bad filter syntax".into()));
        let (status, json) = response_json(he.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["status"], 400);
        assert_eq!(json["message"], "bad filter syntax");
    }

    #[tokio::test]
    async fn handler_error_internal_sanitizes_any_error_to_500() {
        // Simulate a serde error from corrupt stored data — should still be 500, not 400
        let serde_err = serde_json::from_str::<Vec<String>>("not json").unwrap_err();
        let he = HandlerError::internal(serde_err);
        let (status, json) = response_json(he.into_response()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["status"], 500);
        assert_eq!(json["message"], "Internal server error");
    }

    #[tokio::test]
    async fn handler_error_from_status_string_tuple_preserves_status_and_message() {
        let tuple = (StatusCode::BAD_REQUEST, "Invalid index name".to_string());
        let he = HandlerError::from(tuple);
        let (status, json) = response_json(he.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["status"], 400);
        assert_eq!(json["message"], "Invalid index name");
    }
}
