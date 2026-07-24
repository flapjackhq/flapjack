use axum::{extract::State, Json};
use serde::Serialize;
use std::sync::Arc;
use utoipa::ToSchema;

use super::AppState;

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublicBuildInfo {
    schema_version: u8,
    version: String,
    profile: String,
    capabilities: flapjack::BuildCapabilities,
}

impl From<&flapjack::BuildInfo> for PublicBuildInfo {
    fn from(build: &flapjack::BuildInfo) -> Self {
        Self {
            schema_version: build.schema_version,
            version: build.version.clone(),
            profile: build.profile.clone(),
            capabilities: build.capabilities.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    status: &'static str,
    version: String,
    build: PublicBuildInfo,
    uptime_secs: u64,
    capabilities: flapjack::BuildCapabilities,
    active_writers: usize,
    max_concurrent_writers: usize,
    facet_cache_entries: usize,
    facet_cache_cap: usize,
    heap_allocated_mb: usize,
    system_limit_mb: usize,
    pressure_level: String,
    allocator: &'static str,
    tenants_loaded: usize,
}

/// Health check endpoint
#[utoipa::path(
    get,
    path = "/health",
    tag = "health",
    responses(
        (status = 200, description = "Server is healthy", body = HealthResponse)
    )
)]
pub async fn health(State(_state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let build = flapjack::build_info();
    let budget = flapjack::get_global_budget();
    let observer = flapjack::MemoryObserver::global();
    let mem_stats = observer.stats();

    Json(HealthResponse {
        status: "ok",
        version: build.version.clone(),
        build: PublicBuildInfo::from(build),
        uptime_secs: _state.start_time.elapsed().as_secs(),
        capabilities: build.capabilities.clone(),
        active_writers: budget.active_writers(),
        max_concurrent_writers: budget.max_concurrent_writers(),
        facet_cache_entries: _state.manager.facet_cache.len(),
        facet_cache_cap: _state
            .manager
            .facet_cache_cap
            .load(std::sync::atomic::Ordering::Relaxed),
        heap_allocated_mb: mem_stats.heap_allocated_bytes / (1024 * 1024),
        system_limit_mb: mem_stats.system_limit_bytes / (1024 * 1024),
        pressure_level: mem_stats.pressure_level.to_string(),
        allocator: mem_stats.allocator,
        tenants_loaded: _state.manager.loaded_count(),
    })
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
    async fn health_omits_sensitive_build_fingerprint_fields() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();

        let json = request_health_json(state).await;

        assert_eq!(json["build"]["schemaVersion"], 1);
        assert_eq!(
            json["build"]["version"],
            serde_json::to_value(&flapjack::build_info().version).unwrap()
        );
        assert!(json["build"]["profile"].is_string());
        assert_eq!(
            json["build"]["capabilities"],
            serde_json::to_value(&flapjack::build_info().capabilities).unwrap()
        );
        for field in [
            "revision",
            "revisionKnown",
            "dirty",
            "dirtyKnown",
            "workspaceDigest",
            "target",
            "features",
        ] {
            assert!(
                json["build"].get(field).is_none(),
                "public /health must not expose build fingerprint field: {field}"
            );
        }
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

    /// Pin the `/health` fields consumed by the System screen.
    #[tokio::test]
    async fn ops_contract_health_matches_system_consumer_fields() {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        state.manager.create_tenant("t1").unwrap();
        state.manager.create_tenant("t2").unwrap();

        let uptime_before = state.start_time.elapsed().as_secs();
        let expected_facet_cache_cap = state
            .manager
            .facet_cache_cap
            .load(std::sync::atomic::Ordering::Relaxed);
        let expected_max_concurrent_writers =
            flapjack::get_global_budget().max_concurrent_writers();
        let expected_system_limit_mb = flapjack::MemoryObserver::global()
            .stats()
            .system_limit_bytes
            / (1024 * 1024);

        let json = request_health_json(Arc::clone(&state)).await;

        let uptime_after = state.start_time.elapsed().as_secs();
        let active_writers = json["active_writers"].as_u64().unwrap();
        let max_concurrent_writers = json["max_concurrent_writers"].as_u64().unwrap();

        assert_eq!(json["status"], "ok");
        assert_eq!(
            json["version"],
            serde_json::to_value(&flapjack::build_info().version).unwrap()
        );
        assert_eq!(
            json["capabilities"],
            serde_json::to_value(&flapjack::build_info().capabilities).unwrap()
        );
        assert!(
            active_writers <= max_concurrent_writers,
            "active writer telemetry must not exceed the configured writer budget"
        );
        assert_eq!(
            max_concurrent_writers, expected_max_concurrent_writers as u64,
            "writer budget telemetry must expose the configured writer limit"
        );
        assert_eq!(json["facet_cache_entries"], 0);
        assert_eq!(json["facet_cache_cap"], expected_facet_cache_cap);
        assert_eq!(json["tenants_loaded"], serde_json::json!(2));
        assert!(
            (uptime_before..=uptime_after).contains(&json["uptime_secs"].as_u64().unwrap()),
            "reported uptime must be sampled during the request"
        );
        assert!(json["heap_allocated_mb"].as_u64().is_some());
        assert_eq!(
            json["system_limit_mb"],
            serde_json::json!(expected_system_limit_mb),
            "memory telemetry must expose the observer's effective system limit"
        );
        assert!(
            ["normal", "elevated", "critical"].contains(&json["pressure_level"].as_str().unwrap()),
            "pressure telemetry must use the closed wire token set"
        );
        assert_eq!(
            json["allocator"],
            flapjack::MemoryObserver::allocator_name()
        );
    }

    /// Verify public /health keeps only the intended top-level metadata surface.
    #[tokio::test]
    async fn ops_contract_health_uses_explicit_top_level_metadata_allowlist() {
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
