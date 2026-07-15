use axum::{extract::State, Json};
use std::sync::Arc;

use super::AppState;

/// Health check endpoint
#[utoipa::path(
    get,
    path = "/health",
    tag = "health",
    responses(
        (status = 200, description = "Server is healthy", body = serde_json::Value)
    )
)]
pub async fn health(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let build = flapjack::build_info();
    let budget = flapjack::get_global_budget();
    let observer = flapjack::MemoryObserver::global();
    let mem_stats = observer.stats();

    Json(serde_json::json!({
        "status": "ok",
        "version": build.version,
        "build": build,
        "uptime_secs": _state.start_time.elapsed().as_secs(),
        "capabilities": build.capabilities,
        "active_writers": budget.active_writers(),
        "max_concurrent_writers": budget.max_concurrent_writers(),
        "facet_cache_entries": _state.manager.facet_cache.len(),
        "facet_cache_cap": _state.manager.facet_cache_cap.load(std::sync::atomic::Ordering::Relaxed),
        "heap_allocated_mb": mem_stats.heap_allocated_bytes / (1024 * 1024),
        "system_limit_mb": mem_stats.system_limit_bytes / (1024 * 1024),
        "pressure_level": mem_stats.pressure_level.to_string(),
        "allocator": mem_stats.allocator,
        "tenants_loaded": _state.manager.loaded_count(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestStateBuilder;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use std::collections::HashSet;
    use tempfile::TempDir;
    use tower::ServiceExt;

    fn test_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/health", get(health))
            .with_state(state)
    }

    #[tokio::test]
    async fn health_exposes_canonical_build_info() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();

        let json = request_health_json(state).await;
        let expected_build = serde_json::to_value(flapjack::build_info()).unwrap();

        assert_eq!(json["build"], expected_build);
        assert_eq!(json["build"]["schemaVersion"], 1);
        assert!(json["build"]["version"].is_string());
        assert_eq!(
            json["build"]["revision"].is_null(),
            !json["build"]["revisionKnown"].as_bool().unwrap()
        );
        assert_eq!(
            json["build"]["dirty"].is_null(),
            !json["build"]["dirtyKnown"].as_bool().unwrap()
        );
        assert!(json["build"]["workspaceDigest"].is_string());
        assert!(json["build"]["profile"].is_string());
        assert!(json["build"]["target"].is_string());
        assert_eq!(
            json["build"]["features"],
            serde_json::to_value(&flapjack::build_info().features).unwrap()
        );
        assert_eq!(
            json["build"]["capabilities"],
            serde_json::to_value(&flapjack::build_info().capabilities).unwrap()
        );
    }

    async fn request_health_json(state: Arc<AppState>) -> serde_json::Value {
        let response = test_router(state)
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Verify the public health contract keeps required compatibility fields.
    #[tokio::test]
    async fn health_keeps_required_compatibility_fields() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        state.manager.create_tenant("t1").unwrap();
        state.manager.create_tenant("t2").unwrap();

        let json = request_health_json(state).await;
        let expected_build = serde_json::to_value(flapjack::build_info()).unwrap();

        assert_eq!(json["status"], "ok");
        assert_eq!(json["version"], expected_build["version"]);
        assert_eq!(json["build"], expected_build);
        assert!(json["active_writers"].is_number());
        assert!(json["max_concurrent_writers"].is_number());
        assert!(json["facet_cache_entries"].is_number());
        assert!(json["facet_cache_cap"].is_number());
        assert!(json["heap_allocated_mb"].is_number());
        assert!(json["system_limit_mb"].is_number());
        assert!(json["pressure_level"].is_string());
        assert!(json["allocator"].is_string());
        assert!(json["uptime_secs"].is_number());
        assert_eq!(
            json["capabilities"],
            serde_json::to_value(&flapjack::build_info().capabilities).unwrap()
        );
        assert_eq!(json["tenants_loaded"], serde_json::json!(2));
    }

    /// Verify public /health keeps only the intended top-level metadata surface.
    #[tokio::test]
    async fn health_uses_explicit_top_level_metadata_allowlist() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();

        let json = request_health_json(state).await;
        let actual_keys: HashSet<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected_keys: HashSet<&str> = [
            "status",
            "version",
            "build",
            "uptime_secs",
            "capabilities",
            "active_writers",
            "max_concurrent_writers",
            "facet_cache_entries",
            "facet_cache_cap",
            "heap_allocated_mb",
            "system_limit_mb",
            "pressure_level",
            "allocator",
            "tenants_loaded",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            actual_keys, expected_keys,
            "public /health must keep an exact allowlist contract"
        );

        let disallowed_fields = [
            "build_profile",
            "profile",
            "target",
            "features",
            "workspaceDigest",
        ];
        for field in disallowed_fields {
            assert!(
                json.get(field).is_none(),
                "public /health must not expose operational field: {field}"
            );
        }
    }
}
