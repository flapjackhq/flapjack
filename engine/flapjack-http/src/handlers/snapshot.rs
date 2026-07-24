use super::AppState;
use crate::error_response::json_error;
use crate::extractors::ValidatedIndexName;
use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use flapjack::error::FlapjackError;
use flapjack::index::s3::S3Config;
use flapjack::index::snapshot::export_to_bytes;
use std::{path::PathBuf, sync::Arc};
use utoipa::ToSchema;

const SNAPSHOT_EXPORT_MAX_ATTEMPTS: usize = 3;

#[derive(serde::Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotBackend {
    S3,
}

#[derive(serde::Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotCapabilityState {
    NotConfigured,
    /// Configuration is present, but credentials, bucket existence, and
    /// backend reachability have not been verified.
    ConfiguredUnverified,
}

#[derive(serde::Serialize, ToSchema)]
pub struct SnapshotCapabilityResponse {
    backend: SnapshotBackend,
    state: SnapshotCapabilityState,
    #[schema(required)]
    bucket: Option<String>,
}

#[utoipa::path(
    get,
    path = "/internal/snapshots/capability",
    tag = "internal",
    responses(
        (status = 200, description = "Snapshot backend capability", body = SnapshotCapabilityResponse)
    ),
    security(("api_key" = []))
)]
pub async fn snapshot_capability() -> Json<SnapshotCapabilityResponse> {
    let (state, bucket) = match S3Config::from_env() {
        None => (SnapshotCapabilityState::NotConfigured, None),
        Some(config) => (
            SnapshotCapabilityState::ConfiguredUnverified,
            Some(config.bucket_name),
        ),
    };

    Json(SnapshotCapabilityResponse {
        backend: SnapshotBackend::S3,
        state,
        bucket,
    })
}

fn s3_config_or_error(message: &'static str) -> Result<S3Config, Box<Response>> {
    S3Config::from_env().ok_or_else(|| {
        Box::new(json_error(StatusCode::SERVICE_UNAVAILABLE, message).into_response())
    })
}

fn index_path_or_404(state: &AppState, index_name: &str) -> Result<PathBuf, Box<Response>> {
    let index_path = state.manager.base_path.join(index_name);
    if index_path.exists() {
        Ok(index_path)
    } else {
        Err(Box::new(
            json_error(StatusCode::NOT_FOUND, "Index not found").into_response(),
        ))
    }
}

fn internal_error(prefix: &str, error: impl std::fmt::Display) -> Response {
    tracing::error!("{prefix}: {error}");
    json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
}

/// Sanitized 500 response that preserves the standard `{message, status}` wire
/// format and adds a stable, enum-bounded `sub_step` tag identifying which
/// internal step of `install_snapshot_bytes` failed. The tag is non-PII (no
/// path / tenant / error-string data) and exists so the failing branch is
/// observable in test output and operator logs without leaking the underlying
/// error prose. `message` MUST remain exactly `"Internal server error"` —
/// downstream tests in `test_snapshot_import_failure_contract.rs` lock this
/// leak-prevention contract.
fn snapshot_install_error(
    prefix: &str,
    step: crate::startup_catchup::SnapshotInstallStep,
    error: impl std::fmt::Display,
) -> Response {
    tracing::error!("{prefix} at step '{}': {error}", step.as_tag());
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({
            "message": "Internal server error",
            "status": StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
            "sub_step": step.as_tag(),
        })),
    )
        .into_response()
}

fn should_retry_export_error(error: &FlapjackError) -> bool {
    matches!(error, FlapjackError::Io(_))
}

fn export_with_retry(
    mut export_once: impl FnMut() -> Result<Vec<u8>, FlapjackError>,
) -> Result<Vec<u8>, FlapjackError> {
    let mut attempt = 1usize;
    loop {
        match export_once() {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                if attempt >= SNAPSHOT_EXPORT_MAX_ATTEMPTS || !should_retry_export_error(&error) {
                    return Err(error);
                }
                tracing::warn!(
                    attempt,
                    max_attempts = SNAPSHOT_EXPORT_MAX_ATTEMPTS,
                    error = %error,
                    "Transient snapshot export failed; retrying"
                );
                attempt += 1;
            }
        }
    }
}

fn snapshot_retention() -> usize {
    std::env::var("FLAPJACK_SNAPSHOT_RETENTION")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(24)
}

/// Validates that a user-supplied restore key references the correct index and has proper format.
fn validate_restore_key_override(index_name: &str, key: &str) -> Result<(), Box<Response>> {
    let expected_prefix = format!("snapshots/{index_name}/");
    let Some(file_name) = key.strip_prefix(&expected_prefix) else {
        return Err(Box::new(
            json_error(
                StatusCode::BAD_REQUEST,
                "key must reference a snapshot for the requested index",
            )
            .into_response(),
        ));
    };

    if file_name.is_empty() || file_name.contains('/') || !file_name.ends_with(".tar.gz") {
        return Err(Box::new(
            json_error(
                StatusCode::BAD_REQUEST,
                "key must reference a snapshot for the requested index",
            )
            .into_response(),
        ));
    }

    Ok(())
}

/// Downloads a snapshot payload from S3, using a specific key override or the latest snapshot for the index.
async fn download_restore_payload(
    s3_config: &S3Config,
    index_name: &str,
    key_override: Option<String>,
) -> Result<(String, Vec<u8>), Box<Response>> {
    if let Some(key) = key_override {
        validate_restore_key_override(index_name, &key)?;
        let data = flapjack::index::s3::download_snapshot(s3_config, &key)
            .await
            .map_err(|error| Box::new(internal_error("S3 download failed", error)))?;
        Ok((key, data))
    } else {
        flapjack::index::s3::download_latest_snapshot(s3_config, index_name)
            .await
            .map_err(|error| {
                Box::new(json_error(StatusCode::NOT_FOUND, error.to_string()).into_response())
            })
    }
}

/// Exports an index as a compressed snapshot file returned as a binary download response.
#[utoipa::path(
    get,
    path = "/1/indexes/{indexName}/export",
    tag = "snapshots",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    responses(
        (status = 200, description = "Snapshot file", body = Vec<u8>),
        (status = 404, description = "Index not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn export_snapshot(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
) -> Response {
    let index_path = match index_path_or_404(state.as_ref(), &index_name) {
        Ok(path) => path,
        Err(response) => return *response,
    };

    // Synchronous gzip+tar I/O is moved off the tokio worker pool so it
    // cannot starve sibling async tasks (health checks, task polling) on
    // CPU-constrained runners. Stage 1 RCA:
    // engine/docs/research/jun02_snapshot_flake_stage1.md (defect 1).
    let export_result =
        tokio::task::spawn_blocking(move || export_with_retry(|| export_to_bytes(&index_path)))
            .await;

    match export_result {
        Ok(Ok(bytes)) => {
            let headers = [
                ("Content-Type", "application/gzip"),
                (
                    "Content-Disposition",
                    &format!("attachment; filename=\"{}.tar.gz\"", index_name),
                ),
            ];
            (headers, bytes).into_response()
        }
        Ok(Err(e)) => internal_error("Export failed", e),
        Err(join_error) => internal_error("Export failed (join)", join_error),
    }
}

/// Import index from uploaded snapshot
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/import",
    tag = "snapshots",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    request_body(content = Vec<u8>, description = "Snapshot tar.gz file"),
    responses(
        (status = 200, description = "Import successful", body = serde_json::Value),
        (status = 500, description = "Import failed")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn import_snapshot(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
    body: Bytes,
) -> Response {
    // Synchronous gzip+tar decode and directory-rename plumbing is moved
    // off the tokio worker pool so it cannot starve sibling async tasks
    // (health checks, task polling) on CPU-constrained runners. Stage 1
    // RCA: engine/docs/research/jun02_snapshot_flake_stage1.md (defect 1).
    let manager = state.manager.clone();
    let install_result = tokio::task::spawn_blocking(move || {
        crate::startup_catchup::install_snapshot_bytes(&manager, &index_name, &body)
    })
    .await;

    match install_result {
        Ok(Ok(())) => Json(serde_json::json!({ "status": "imported" })).into_response(),
        Ok(Err((step, error))) => snapshot_install_error("Import failed", step, error),
        Err(join_error) => snapshot_install_error(
            "Import failed",
            crate::startup_catchup::SnapshotInstallStep::ImportExtract,
            join_error,
        ),
    }
}

/// Uploads an index snapshot to the configured S3 bucket, returning the snapshot key on success.
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/snapshot",
    tag = "snapshots",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    responses(
        (status = 200, description = "Snapshot uploaded to S3", body = serde_json::Value),
        (status = 503, description = "S3 not configured"),
        (status = 404, description = "Index not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn snapshot_to_s3(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
) -> Response {
    let s3_config = match s3_config_or_error(
        "S3 not configured. Set FLAPJACK_S3_BUCKET and FLAPJACK_S3_REGION.",
    ) {
        Ok(config) => config,
        Err(response) => return *response,
    };
    let index_path = match index_path_or_404(state.as_ref(), &index_name) {
        Ok(path) => path,
        Err(response) => return *response,
    };

    let bytes = match export_to_bytes(&index_path) {
        Ok(b) => b,
        Err(e) => return internal_error("Export failed", e),
    };

    match flapjack::index::s3::upload_snapshot(&s3_config, &index_name, &bytes).await {
        Ok(key) => {
            let _ = flapjack::index::s3::enforce_retention(
                &s3_config,
                &index_name,
                snapshot_retention(),
            )
            .await;

            Json(serde_json::json!({
                "status": "uploaded",
                "key": key,
                "size_bytes": bytes.len(),
            }))
            .into_response()
        }
        Err(e) => internal_error("S3 upload failed", e),
    }
}

/// Restores an index from an S3 snapshot, downloading and installing the snapshot bytes into the local index directory.
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/restore",
    tag = "snapshots",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    request_body(content = serde_json::Value, description = "Restore options with snapshot ID"),
    responses(
        (status = 200, description = "Restore successful", body = serde_json::Value),
        (status = 503, description = "S3 not configured"),
        (status = 404, description = "Snapshot not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn restore_from_s3(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
    body: Option<Json<serde_json::Value>>,
) -> Response {
    let s3_config = match s3_config_or_error("S3 not configured") {
        Ok(config) => config,
        Err(response) => return *response,
    };

    let key_override = body.and_then(|b| b.get("key").and_then(|v| v.as_str()).map(String::from));

    let (key, data) = match download_restore_payload(&s3_config, &index_name, key_override).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };

    let data_len = data.len();
    match crate::startup_catchup::install_snapshot_bytes(&state.manager, &index_name, &data) {
        Ok(()) => Json(serde_json::json!({
            "status": "restored",
            "key": key,
            "size_bytes": data_len,
        }))
        .into_response(),
        Err((step, error)) => snapshot_install_error("Restore failed", step, error),
    }
}

/// List available S3 snapshots for an index
#[utoipa::path(
    get,
    path = "/1/indexes/{indexName}/snapshots",
    tag = "snapshots",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    responses(
        (status = 200, description = "List of snapshots", body = serde_json::Value),
        (status = 503, description = "S3 not configured")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn list_s3_snapshots(ValidatedIndexName(index_name): ValidatedIndexName) -> Response {
    let s3_config = match s3_config_or_error("S3 not configured") {
        Ok(config) => config,
        Err(response) => return *response,
    };

    match flapjack::index::s3::list_snapshots(&s3_config, &index_name).await {
        Ok(keys) => Json(serde_json::json!({ "snapshots": keys })).into_response(),
        Err(e) => internal_error("List snapshots failed", e),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        export_snapshot, import_snapshot, list_s3_snapshots, snapshot_capability,
        validate_restore_key_override,
    };
    use crate::test_helpers::{body_json, EnvVarRestoreGuard, TestStateBuilder, ENV_MUTEX};
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        response::Response,
        routing::{get, post},
        Router,
    };
    use flapjack::index::snapshot::export_to_bytes;
    use flapjack::types::{Document, FieldValue};
    use flapjack::FlapjackError;
    use std::collections::HashMap;
    use tempfile::TempDir;
    use tower::ServiceExt;

    // The process-global environment lock must span each asynchronous handler
    // call so another test cannot change S3 configuration mid-request.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn snapshot_capability_unconfigured_returns_exact_closed_contract() {
        let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
        let _bucket = EnvVarRestoreGuard::remove("FLAPJACK_S3_BUCKET");

        let response_body = serde_json::to_value(snapshot_capability().await.0).unwrap();

        assert_eq!(
            response_body,
            serde_json::json!({
                "backend": "s3",
                "state": "not_configured",
                "bucket": null
            })
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn snapshot_capability_configured_unverified_preserves_bucket_without_leaking_configuration(
    ) {
        let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
        let _bucket = EnvVarRestoreGuard::set("FLAPJACK_S3_BUCKET", "snapshot-bucket");
        let _region = EnvVarRestoreGuard::set("FLAPJACK_S3_REGION", "sentinel-region");
        let _endpoint =
            EnvVarRestoreGuard::set("FLAPJACK_S3_ENDPOINT", "http://127.0.0.1:9/unreachable");
        let _access_key = EnvVarRestoreGuard::set("AWS_ACCESS_KEY_ID", "sentinel-access-key");
        let _secret_key = EnvVarRestoreGuard::set("AWS_SECRET_ACCESS_KEY", "sentinel-secret-key");

        let response_body = serde_json::to_value(snapshot_capability().await.0).unwrap();
        let unconfigured_body = serde_json::json!({
            "backend": "s3",
            "state": "not_configured",
            "bucket": null
        });

        assert_eq!(
            response_body,
            serde_json::json!({
                "backend": "s3",
                "state": "configured_unverified",
                "bucket": "snapshot-bucket"
            })
        );
        assert_ne!(response_body, unconfigured_body);

        let serialized = serde_json::to_string(&response_body).unwrap();
        for sentinel in [
            "sentinel-region",
            "http://127.0.0.1:9/unreachable",
            "sentinel-access-key",
            "sentinel-secret-key",
        ] {
            assert!(
                !serialized.contains(sentinel),
                "capability response leaked configuration value {sentinel}"
            );
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn snapshot_capability_preserves_empty_configured_bucket() {
        let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
        let _bucket = EnvVarRestoreGuard::set("FLAPJACK_S3_BUCKET", "");

        let response_body = serde_json::to_value(snapshot_capability().await.0).unwrap();

        assert_eq!(
            response_body,
            serde_json::json!({
                "backend": "s3",
                "state": "configured_unverified",
                "bucket": ""
            })
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn list_s3_snapshots_without_bucket_preserves_exact_service_unavailable_contract() {
        let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
        let _bucket = EnvVarRestoreGuard::remove("FLAPJACK_S3_BUCKET");
        let app = Router::new().route("/1/indexes/:indexName/snapshots", get(list_s3_snapshots));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/1/indexes/products/snapshots")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body_json(response).await,
            serde_json::json!({
                "message": "S3 not configured",
                "status": 503
            })
        );
    }

    #[tokio::test]
    async fn export_snapshot_missing_index_returns_json_without_router_error_wrapper() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let app = Router::new()
            .route("/1/indexes/:indexName/export", get(export_snapshot))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/1/indexes/missing/export")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("application/json"),
            "expected JSON error content-type, got: {content_type}"
        );
        assert_eq!(
            body_json(response).await,
            serde_json::json!({
                "message": "Index not found",
                "status": 404
            })
        );
    }
    #[tokio::test]
    async fn import_snapshot_success_returns_json_without_router_error_wrapper() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        state.manager.create_tenant("products").unwrap();
        state
            .manager
            .add_documents_sync(
                "products",
                vec![Document {
                    id: "1".to_string(),
                    fields: HashMap::from([(
                        "title".to_string(),
                        FieldValue::Text("snapshot source".to_string()),
                    )]),
                }],
            )
            .await
            .unwrap();
        let snapshot_bytes = export_to_bytes(&state.manager.base_path.join("products")).unwrap();

        let app = Router::new()
            .route("/1/indexes/:indexName/import", post(import_snapshot))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/1/indexes/products/import")
                    .body(Body::from(snapshot_bytes))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("application/json"),
            "expected JSON success content-type, got: {content_type}"
        );
        assert_eq!(
            body_json(response).await,
            serde_json::json!({
                "status": "imported"
            })
        );
    }

    #[tokio::test]
    async fn import_snapshot_invalid_payload_returns_sanitized_500_message() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let app = Router::new()
            .route("/1/indexes/:indexName/import", post(import_snapshot))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/1/indexes/products/import")
                    .body(Body::from("not-a-valid-snapshot".as_bytes().to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_json(response).await;
        let message = body["message"]
            .as_str()
            .expect("expected string message for 500 responses");
        assert_eq!(message, "Internal server error");
        assert_eq!(body["status"], serde_json::json!(500));
        assert!(
            !message.contains("Import failed:"),
            "500 response must not leak internal prefix text: {message}"
        );
        assert!(
            !message.contains("not-a-valid-snapshot"),
            "500 response must not leak backend error details: {message}"
        );
    }

    async fn assert_bad_request_message(response: Response, expected_message: &str) {
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await,
            serde_json::json!({
                "message": expected_message,
                "status": 400
            })
        );
    }

    #[tokio::test]
    async fn restore_key_override_rejects_cross_index_snapshot_keys() {
        let response =
            validate_restore_key_override("products", "snapshots/orders/20260329T120000Z.tar.gz")
                .unwrap_err();

        assert_bad_request_message(
            *response,
            "key must reference a snapshot for the requested index",
        )
        .await;
    }

    #[test]
    fn restore_key_override_accepts_requested_index_snapshot_keys() {
        assert!(validate_restore_key_override(
            "products",
            "snapshots/products/20260329T120000Z.tar.gz"
        )
        .is_ok());
    }

    #[test]
    fn export_with_retry_retries_transient_io_errors() {
        let mut attempts = 0usize;
        let bytes = super::export_with_retry(|| {
            attempts += 1;
            if attempts < 3 {
                Err(FlapjackError::Io("transient".to_string()))
            } else {
                Ok(vec![1, 2, 3])
            }
        })
        .expect("third attempt should succeed");
        assert_eq!(bytes, vec![1, 2, 3]);
        assert_eq!(attempts, 3, "must retry transient IO errors");
    }

    #[test]
    fn export_with_retry_does_not_retry_non_io_errors() {
        let mut attempts = 0usize;
        let error = super::export_with_retry(|| {
            attempts += 1;
            Err(FlapjackError::Config("not transient".to_string()))
        })
        .expect_err("non-IO errors should fail immediately");
        assert!(matches!(error, FlapjackError::Config(_)));
        assert_eq!(
            attempts, 1,
            "non-IO errors should not be retried because they are not transient file-churn failures"
        );
    }
}
