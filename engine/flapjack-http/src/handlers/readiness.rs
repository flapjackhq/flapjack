//! Stub summary for readiness.rs.
use crate::error_response::HandlerError;
use crate::tenant_dirs::visible_tenant_dir_names;
use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;

use super::AppState;

fn readiness_service_unavailable() -> HandlerError {
    HandlerError::Custom {
        status: StatusCode::SERVICE_UNAVAILABLE,
        message: "Service unavailable".to_string(),
    }
}

/// Readiness probe: returns 200 with loaded/expected tenant counts for orchestrators.
pub async fn ready(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, HandlerError> {
    let failed_tasks = state.background_task_health.failed_tasks();
    if !failed_tasks.is_empty() {
        tracing::warn!(tasks = ?failed_tasks, "readiness probe found exited background tasks");
        return Err(readiness_service_unavailable());
    }

    let mut visible_tenants =
        visible_tenant_dir_names(&state.manager.base_path).map_err(|error| {
            tracing::warn!(
                "readiness probe failed to inspect tenant dirs at {:?}: {}",
                state.manager.base_path,
                error
            );
            readiness_service_unavailable()
        })?;

    visible_tenants.sort();
    for index_id in &visible_tenants {
        state.manager.get_or_load(index_id).map_err(|error| {
            tracing::warn!(index_id, %error, "readiness could not load a durable index");
            readiness_service_unavailable()
        })?;
    }
    let Some(first_searchable_tenant) = visible_tenants.first() else {
        return Ok(Json(serde_json::json!({ "ready": true })));
    };

    state
        .manager
        .search(first_searchable_tenant, "", None, None, 1)
        .map_err(|error| {
            tracing::warn!(
                "readiness probe failed to search tenant {}: {}",
                first_searchable_tenant,
                error
            );
            readiness_service_unavailable()
        })?;

    Ok(Json(serde_json::json!({ "ready": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestStateBuilder;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use tempfile::TempDir;
    use tower::ServiceExt;

    /// TODO: Document readiness_response_json.
    async fn readiness_response_json(app: Router) -> (StatusCode, serde_json::Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        (status, json)
    }

    #[tokio::test]
    async fn ready_returns_ready_when_no_visible_tenants_exist() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let app = Router::new()
            .route("/health/ready", get(ready))
            .with_state(state);

        let (status, body) = readiness_response_json(app).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({ "ready": true }));
    }

    #[tokio::test]
    async fn ready_returns_unavailable_after_a_background_task_exits() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        state
            .background_task_health
            .record_exit_for_test("analytics-flush");
        let app = Router::new()
            .route("/health/ready", get(ready))
            .with_state(state);

        let (status, body) = readiness_response_json(app).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body,
            serde_json::json!({
                "message": "Service unavailable",
                "status": 503
            })
        );
    }

    #[tokio::test]
    async fn ready_returns_ready_when_first_visible_tenant_is_searchable() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        state.manager.create_tenant("products").unwrap();
        let app = Router::new()
            .route("/health/ready", get(ready))
            .with_state(state);

        let (status, body) = readiness_response_json(app).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({ "ready": true }));
    }

    #[tokio::test]
    async fn storage_ownership_ready_rejects_damaged_index_beside_healthy_index() {
        for (index_id, corrupt_meta) in [
            ("zeta", false),
            ("zeta", true),
            ("personalization_strategy.json", false),
        ] {
            let tmp = TempDir::new().unwrap();
            let state = TestStateBuilder::new(&tmp).build_shared();
            state.manager.create_tenant("alpha").unwrap();
            let damaged = tmp.path().join(index_id);
            std::fs::create_dir(&damaged).unwrap();
            if corrupt_meta {
                std::fs::write(damaged.join("meta.json"), b"invalid").unwrap();
            }
            let app = Router::new()
                .route("/health/ready", get(ready))
                .with_state(state);
            let (status, body) = readiness_response_json(app).await;
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "damaged index must not disappear behind healthy alpha: {body}"
            );
        }
    }

    #[tokio::test]
    async fn ready_rejects_legacy_application_directory_beside_an_index() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        state.manager.create_tenant("zeta").unwrap();
        std::fs::create_dir_all(tmp.path().join("flapjack").join(".dictionaries")).unwrap();
        let app = Router::new()
            .route("/health/ready", get(ready))
            .with_state(state);

        let (status, body) = readiness_response_json(app).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body,
            serde_json::json!({ "message": "Service unavailable", "status": 503 })
        );
    }

    #[tokio::test]
    async fn ready_rejects_legacy_application_directory_without_an_index() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        std::fs::create_dir_all(tmp.path().join("flapjack").join(".dictionaries")).unwrap();
        let app = Router::new()
            .route("/health/ready", get(ready))
            .with_state(state);

        let (status, body) = readiness_response_json(app).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body,
            serde_json::json!({ "message": "Service unavailable", "status": 503 })
        );
    }
    /// TODO: Document ready_returns_canonical_503_when_visible_tenant_discovery_fails.
    #[tokio::test]
    async fn ready_returns_canonical_503_when_visible_tenant_discovery_fails() {
        let tmp = TempDir::new().unwrap();
        let data_file = tmp.path().join("not-a-directory");
        std::fs::write(&data_file, "x").unwrap();
        let base_state = TestStateBuilder::new(&tmp).build();
        let state = Arc::new(AppState {
            manager: flapjack::IndexManager::new(&data_file),
            ..base_state
        });
        let app = Router::new()
            .route("/health/ready", get(ready))
            .with_state(state);

        let (status, body) = readiness_response_json(app).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body,
            serde_json::json!({
                "message": "Service unavailable",
                "status": 503
            }),
            "canonical readiness failures must keep the shared 503 envelope stable"
        );
    }
}
