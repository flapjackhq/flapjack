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
use flapjack::index::s3::S3Config;
use std::{path::PathBuf, sync::Arc};
use utoipa::ToSchema;

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

    // Drain and merge-quiesce the persistent writer so the gzip/tar read sees a
    // quiesced generation rather than a mid-commit snapshot. The guard stays held
    // across the blocking read so no replacement writer can commit into the tree
    // while it is being packed.
    let _quiesce = match state.manager.quiesce_tenant(&index_name.to_string()).await {
        Ok(quiesce) => quiesce,
        Err(error) => return internal_error("Export quiesce failed", error),
    };

    // Synchronous gzip+tar I/O is moved off the tokio worker pool so it
    // cannot starve sibling async tasks (health checks, task polling) on
    // CPU-constrained runners. Stage 1 RCA:
    // engine/docs/research/jun02_snapshot_flake_stage1.md (defect 1).
    let export_index_name = index_name.clone();
    let export_result = tokio::task::spawn_blocking(move || {
        crate::snapshot_byte_ops::export_snapshot_bytes(&index_path, &export_index_name)
    })
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
    // The destination writer is drained and merge-quiesced before the snapshot
    // is installed; the synchronous gzip+tar decode and directory-rename
    // plumbing then run off the tokio worker pool inside `restore_snapshot_bytes`
    // so they cannot starve sibling async tasks (health checks, task polling) on
    // CPU-constrained runners. Stage 1 RCA:
    // engine/docs/research/jun02_snapshot_flake_stage1.md (defect 1).
    match crate::startup_catchup::restore_snapshot_bytes(&state.manager, &index_name, body.to_vec())
        .await
    {
        Ok(()) => Json(serde_json::json!({ "status": "imported" })).into_response(),
        Err((step, error)) => snapshot_install_error("Import failed", step, error),
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

    // Drain and merge-quiesce the persistent writer, then run the synchronous
    // gzip+tar export off the async worker pool through the shared byte seam. The
    // guard is held across the read so no replacement writer can race the pack.
    let quiesce = match state.manager.quiesce_tenant(&index_name.to_string()).await {
        Ok(quiesce) => quiesce,
        Err(error) => return internal_error("Export quiesce failed", error),
    };
    let export_index_name = index_name.clone();
    let bytes = match tokio::task::spawn_blocking(move || {
        crate::snapshot_byte_ops::export_snapshot_bytes(&index_path, &export_index_name)
    })
    .await
    {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => return internal_error("Export failed", e),
        Err(join_error) => return internal_error("Export failed (join)", join_error),
    };
    drop(quiesce);

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
    // Quiesce the destination writer and run the synchronous install off the
    // async worker pool via the shared restore path.
    match crate::startup_catchup::restore_snapshot_bytes(&state.manager, &index_name, data).await {
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
        export_snapshot, import_snapshot, list_s3_snapshots, snapshot_capability, snapshot_to_s3,
        validate_restore_key_override,
    };
    use crate::handlers::AppState;
    use crate::test_helpers::{
        assert_quiescence_before_publication, assert_retained_channel_closed_delta, body_json,
        quiesced_snapshot_bytes, retained_channel_closed_count, EnvVarRestoreGuard,
        TestStateBuilder, ENV_MUTEX,
    };
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        response::Response,
        routing::{get, post},
        Router,
    };
    use flapjack::types::{Document, FieldValue};
    use std::{collections::HashMap, sync::Arc};
    use tempfile::TempDir;
    use tower::ServiceExt;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    fn test_document(id: &str, title: &str) -> Document {
        Document {
            id: id.to_string(),
            fields: HashMap::from([("title".to_string(), FieldValue::Text(title.to_string()))]),
        }
    }

    fn assert_document_title(state: &Arc<AppState>, tenant_id: &str, object_id: &str, title: &str) {
        let document = state
            .manager
            .get_document(tenant_id, object_id)
            .unwrap()
            .unwrap_or_else(|| panic!("tenant {tenant_id} must contain document {object_id}"));
        assert_eq!(
            document.fields.get("title"),
            Some(&FieldValue::Text(title.to_string()))
        );
    }

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
        flapjack::index::write_queue::clear_writer_lifecycle_test_events();
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
        let snapshot_bytes = quiesced_snapshot_bytes(&state.manager, "products").await;
        assert_quiescence_before_publication("products", "snapshot_export_read");

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
    async fn import_snapshot_into_absent_tenant_survives_quiesce_fence_repair() {
        flapjack::index::write_queue::clear_writer_lifecycle_test_events();
        let source_tmp = TempDir::new().unwrap();
        let source = TestStateBuilder::new(&source_tmp).build_shared();
        source.manager.create_tenant("products").unwrap();
        source
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
        let snapshot_bytes = quiesced_snapshot_bytes(&source.manager, "products").await;
        assert_quiescence_before_publication("products", "snapshot_export_read");

        let destination_tmp = TempDir::new().unwrap();
        let destination = TestStateBuilder::new(&destination_tmp).build_shared();
        assert!(
            !destination.manager.base_path.join("products").exists(),
            "destination specimen must begin without a live tenant tree"
        );
        let app = Router::new()
            .route("/1/indexes/:indexName/import", post(import_snapshot))
            .with_state(Arc::clone(&destination));

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
        assert_eq!(
            body_json(response).await,
            serde_json::json!({
                "status": "imported"
            })
        );
        let restored = destination
            .manager
            .search("products", "", None, None, 10)
            .unwrap();
        assert_eq!(restored.total, 1);
        assert_document_title(&destination, "products", "1", "snapshot source");
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn snapshot_to_s3_reopens_write_admission_before_upload_completes() {
        let s3 = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(30)))
            .mount(&s3)
            .await;

        let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
        let _bucket = EnvVarRestoreGuard::set("FLAPJACK_S3_BUCKET", "snapshot-bucket");
        let _region = EnvVarRestoreGuard::set("FLAPJACK_S3_REGION", "us-east-1");
        let _endpoint = EnvVarRestoreGuard::set("FLAPJACK_S3_ENDPOINT", &s3.uri());
        let _access_key = EnvVarRestoreGuard::set("AWS_ACCESS_KEY_ID", "test-access-key");
        let _secret_key = EnvVarRestoreGuard::set("AWS_SECRET_ACCESS_KEY", "test-secret-key");

        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let tenant_id = "s3_guard_lifetime";
        state.manager.create_tenant(tenant_id).unwrap();
        state
            .manager
            .add_documents_sync(tenant_id, vec![test_document("before", "before snapshot")])
            .await
            .unwrap();

        let app = Router::new()
            .route("/1/indexes/:indexName/snapshot", post(snapshot_to_s3))
            .with_state(Arc::clone(&state));
        let request_task = tokio::spawn(
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/1/indexes/{tenant_id}/snapshot"))
                    .body(Body::empty())
                    .unwrap(),
            ),
        );

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if s3
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .any(|request| request.method.as_str() == "PUT")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("snapshot upload must begin after local export completes");

        let write_result = state
            .manager
            .add_documents_sync(
                tenant_id,
                vec![test_document("during_upload", "during upload")],
            )
            .await;
        request_task.abort();
        let _ = request_task.await;

        write_result
            .expect("tenant write admission must reopen while the completed snapshot bytes upload");
        assert_document_title(&state, tenant_id, "during_upload", "during upload");
    }

    #[tokio::test]
    async fn snapshot_restore_into_tenant_with_live_writer_quiesces_before_rename() {
        flapjack::index::write_queue::clear_writer_lifecycle_test_events();
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let tenant_id = "stage4_snapshot_restore_quiesce";
        state.manager.create_tenant(tenant_id).unwrap();
        state
            .manager
            .add_documents_sync(
                tenant_id,
                vec![
                    test_document("restored_one", "restored first"),
                    test_document("restored_two", "restored second"),
                ],
            )
            .await
            .unwrap();
        let snapshot_bytes = quiesced_snapshot_bytes(&state.manager, tenant_id).await;
        assert_quiescence_before_publication(tenant_id, "snapshot_export_read");
        state
            .manager
            .add_documents_sync(
                tenant_id,
                vec![test_document("stale_live_writer", "stale live writer")],
            )
            .await
            .unwrap();
        let merge_wait_before = retained_channel_closed_count(tenant_id);

        let app = Router::new()
            .route("/1/indexes/:indexName/import", post(import_snapshot))
            .with_state(Arc::clone(&state));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/1/indexes/{tenant_id}/import"))
                    .body(Body::from(snapshot_bytes))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_json(response).await,
            serde_json::json!({
                "status": "imported"
            })
        );
        assert_retained_channel_closed_delta(
            tenant_id,
            merge_wait_before,
            "snapshot restore must drain and merge-quiesce the destination writer before rename",
        );
        assert_quiescence_before_publication(tenant_id, "snapshot_restore_publication");
        let restored = state.manager.search(tenant_id, "", None, None, 10).unwrap();
        assert_eq!(
            restored.total, 2,
            "snapshot restore must expose exactly the restored generation"
        );
        assert_document_title(&state, tenant_id, "restored_one", "restored first");
        assert_document_title(&state, tenant_id, "restored_two", "restored second");
        assert!(
            state
                .manager
                .get_document(tenant_id, "stale_live_writer")
                .unwrap()
                .is_none(),
            "snapshot restore must remove stale live-writer documents"
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
}
