use super::super::*;

// ── required_acl_for_route ──

fn assert_required_acl(method: Method, path: &str, acl: &'static str) {
    assert_eq!(required_acl_for_route(&method, path), Some(acl));
}

fn assert_public_route(method: Method, path: &str) {
    assert_eq!(required_acl_for_route(&method, path), None);
}

#[test]
fn acl_keys_admin() {
    assert_required_acl(Method::GET, "/1/keys", "admin");
    assert_required_acl(Method::POST, "/1/keys", "admin");
}

/// Verify that all `/1/security/sources` sub-routes (GET, PUT, POST append, DELETE) require the `admin` ACL.
#[test]
fn acl_security_sources_requires_admin() {
    for method_and_path in [
        (Method::GET, "/1/security/sources"),
        (Method::PUT, "/1/security/sources"),
        (Method::POST, "/1/security/sources/append"),
        (Method::DELETE, "/1/security/sources/10.0.0.0%2F24"),
    ] {
        assert_required_acl(method_and_path.0, method_and_path.1, "admin");
    }
}

#[test]
fn acl_analytics_endpoint() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/2/searches"),
        Some("analytics")
    );
}

#[test]
fn acl_analytics_maintenance_routes_require_admin() {
    for method_and_path in [
        (Method::POST, "/2/analytics/seed"),
        (Method::DELETE, "/2/analytics/clear"),
        (Method::POST, "/2/analytics/cleanup"),
        (Method::POST, "/2/analytics/flush"),
    ] {
        assert_required_acl(method_and_path.0, method_and_path.1, "admin");
    }
}

/// Verify that AB test endpoints enforce ACL restrictions: GET requests require `analytics` permission, POST/PUT/DELETE requests require `editSettings` permission.
#[test]
fn acl_abtests_reads_require_analytics_and_writes_require_edit_settings() {
    for method_and_path in [
        (Method::GET, "/2/abtests"),
        (Method::GET, "/2/abtests/123"),
        (Method::GET, "/2/abtests/123/results"),
        (Method::POST, "/2/abtests/estimate"),
    ] {
        assert_required_acl(method_and_path.0, method_and_path.1, "analytics");
    }

    for method_and_path in [
        (Method::POST, "/2/abtests"),
        (Method::PUT, "/2/abtests/123"),
        (Method::DELETE, "/2/abtests/123"),
        (Method::POST, "/2/abtests/123/start"),
        (Method::POST, "/2/abtests/123/stop"),
        (Method::POST, "/2/abtests/123/conclude"),
    ] {
        assert_required_acl(method_and_path.0, method_and_path.1, "editSettings");
    }
}

#[test]
fn acl_events_search() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/events"),
        Some("search")
    );
}

#[test]
fn acl_usertoken_delete_requires_delete_object() {
    assert_eq!(
        required_acl_for_route(&Method::DELETE, "/1/usertokens/user_123"),
        Some("deleteObject")
    );
}

#[test]
fn acl_list_indexes() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/1/indexes"),
        Some("listIndexes")
    );
}

#[test]
fn acl_get_single_index_route_requires_search() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/1/indexes/products"),
        Some("search")
    );
}

#[test]
fn acl_search_query() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/indexes/products/query"),
        Some("search")
    );
}

#[test]
fn acl_chat_requires_inference() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/indexes/products/chat"),
        Some("inference")
    );
}

#[test]
fn acl_browse() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/indexes/products/browse"),
        Some("browse")
    );
}

#[test]
fn acl_batch_add_object() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/indexes/products/batch"),
        Some("addObject")
    );
}

#[test]
fn acl_settings_get() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/1/indexes/products/settings"),
        Some("settings")
    );
}

#[test]
fn acl_settings_put() {
    assert_eq!(
        required_acl_for_route(&Method::PUT, "/1/indexes/products/settings"),
        Some("editSettings")
    );
}

#[test]
fn acl_delete_index() {
    assert_eq!(
        required_acl_for_route(&Method::DELETE, "/1/indexes/products"),
        Some("deleteIndex")
    );
}

#[test]
fn acl_clear_delete_object() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/indexes/products/clear"),
        Some("deleteObject")
    );
}

#[test]
fn acl_tasks() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/1/tasks/123"),
        Some("search")
    );
}

#[test]
fn acl_task_singular_route() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/1/task/123"),
        Some("search")
    );
}

#[test]
fn acl_index_task_route_requires_search() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/1/indexes/products/task/123"),
        Some("search")
    );
}

#[test]
fn acl_tasks_collection_route_requires_search() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/1/tasks"),
        Some("search")
    );
}

#[test]
fn acl_dictionaries_batch_edit_settings() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/dictionaries/stopwords/batch"),
        Some("editSettings")
    );
}

#[test]
fn acl_dictionaries_search_settings() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/dictionaries/stopwords/search"),
        Some("settings")
    );
}

#[test]
fn acl_dictionaries_settings_and_languages() {
    assert_required_acl(Method::GET, "/1/dictionaries/*/settings", "settings");
    assert_required_acl(Method::PUT, "/1/dictionaries/*/settings", "editSettings");
    assert_required_acl(Method::GET, "/1/dictionaries/*/languages", "settings");
}

// ── /internal/* ACL ──

#[test]
fn acl_internal_replicate_requires_admin() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/internal/replicate"),
        Some("admin")
    );
}

#[test]
fn acl_internal_ops_requires_admin() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/internal/ops"),
        Some("admin")
    );
}

#[test]
fn acl_internal_cluster_remove_peer_requires_admin() {
    assert_eq!(
        required_acl_for_route(&Method::DELETE, "/internal/cluster/peers/node-b"),
        Some("admin")
    );
}

#[test]
fn acl_internal_pause_requires_admin() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/internal/pause/myindex"),
        Some("admin")
    );
}

#[test]
fn acl_internal_storage_requires_admin() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/internal/storage"),
        Some("admin")
    );
}

#[test]
fn acl_internal_cluster_add_peer_requires_admin() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/internal/cluster/peers"),
        Some("admin")
    );
}

#[test]
fn acl_metrics_requires_admin() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/metrics"),
        Some("admin")
    );
}

#[test]
fn acl_usage_endpoint_requires_usage_acl() {
    assert_required_acl(Method::GET, "/1/usage/search_operations", "usage");
    assert_required_acl(Method::GET, "/1/usage/search_operations/my_index", "usage");
}

#[test]
fn acl_migration_proxy_endpoints_require_admin() {
    assert_required_acl(Method::POST, "/1/migrate-from-algolia", "admin");
    assert_required_acl(Method::POST, "/1/algolia-list-indexes", "admin");
}

#[test]
fn acl_async_migration_routes_require_admin_with_segment_safe_prefix() {
    assert_required_acl(Method::POST, "/1/migrations/algolia", "admin");
    assert_required_acl(
        Method::GET,
        "/1/migrations/algolia/01890f8e-8b28-78e8-b542-8cfdcb2d4f24",
        "admin",
    );
    assert_required_acl(
        Method::POST,
        "/1/migrations/algolia/01890f8e-8b28-78e8-b542-8cfdcb2d4f24/cancel",
        "admin",
    );
    assert_public_route(Method::POST, "/1/migrations-evil/algolia");
}

#[test]
fn acl_internal_acme_challenge_requires_admin() {
    // Only root-mounted ACME challenge routes are public. `/internal/*` stays admin-only.
    assert_required_acl(
        Method::GET,
        "/internal/.well-known/acme-challenge/Gs7BzSSSj3b7bXBFQ1DLx0iKs",
        "admin",
    );
}

#[test]
fn acl_root_acme_challenge_is_public() {
    // Route mounted by server.rs is `/.well-known/acme-challenge/:token`.
    assert_public_route(
        Method::GET,
        "/.well-known/acme-challenge/Gs7BzSSSj3b7bXBFQ1DLx0iKs",
    );
}

#[test]
fn public_path_helper_includes_root_acme_route() {
    assert!(is_public_path(
        "/.well-known/acme-challenge/token-123",
        false
    ));
    assert!(is_public_path("/health", false));
    assert!(!is_public_path("/internal/replicate", false));
}

#[test]
fn readiness_health_route_is_public_while_metrics_remains_admin_only() {
    assert_public_route(Method::GET, "/health/ready");
    assert!(is_public_path("/health/ready", false));
    assert_required_acl(Method::GET, "/metrics", "admin");
    assert!(!is_public_path("/metrics", false));
}

#[test]
fn acme_challenge_path_helper_matches_root_route_only() {
    assert!(is_acme_challenge_path(
        "/.well-known/acme-challenge/token-123"
    ));
    assert!(!is_acme_challenge_path(
        "/internal/.well-known/acme-challenge/token-123"
    ));
    assert!(!is_acme_challenge_path("/internal/replicate"));
}

#[test]
fn public_path_helper_excludes_internal_acme_route() {
    assert!(!is_public_path(
        "/internal/.well-known/acme-challenge/token-123",
        false
    ));
}

#[test]
fn public_path_helper_only_exposes_dashboard_mount() {
    for path in [
        "/dashboard",
        "/dashboard/",
        "/dashboard/index.html",
        "/swagger-ui",
        "/swagger-ui/",
        "/api-docs/openapi.json",
    ] {
        assert_eq!(
            route_exposure(path, false),
            RouteExposure::Public,
            "path: {path}"
        );
        assert!(is_public_path(path, false), "path: {path}");

        assert_eq!(
            route_exposure(path, true),
            RouteExposure::Disabled,
            "path: {path}"
        );
        assert!(!is_public_path(path, true), "path: {path}");
    }

    assert_eq!(
        route_exposure("/dashboard-admin", false),
        RouteExposure::Protected
    );
    assert_eq!(
        route_exposure("/dashboard-admin", true),
        RouteExposure::Protected
    );
    assert!(!is_public_path("/dashboard-admin", false));
    assert!(!is_public_path("/dashboard-admin", true));
}
#[test]
fn acl_personalization_strategy_requires_personalization() {
    assert_required_acl(
        Method::GET,
        "/1/strategies/personalization",
        "personalization",
    );
    assert_required_acl(
        Method::POST,
        "/1/strategies/personalization",
        "personalization",
    );
    assert_required_acl(
        Method::DELETE,
        "/1/strategies/personalization",
        "personalization",
    );
}

#[test]
fn acl_personalization_profile_requires_personalization() {
    assert_required_acl(
        Method::GET,
        "/1/profiles/personalization/user123",
        "personalization",
    );
    assert_required_acl(Method::DELETE, "/1/profiles/user123", "personalization");
}

#[test]
fn acl_recommendations_requires_recommendation() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/indexes/my_index/recommendations"),
        Some("recommendation")
    );
}

#[test]
fn acl_partial_update_post_requires_add_object() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/indexes/products/abc123/partial"),
        Some("addObject")
    );
}

/// Verify that export, import, snapshot, restore, and snapshots routes all require the `admin` ACL.
#[test]
fn acl_snapshot_routes_require_admin() {
    for method_and_path in [
        (Method::GET, "/1/indexes/products/export"),
        (Method::POST, "/1/indexes/products/import"),
        (Method::POST, "/1/indexes/products/snapshot"),
        (Method::POST, "/1/indexes/products/restore"),
        (Method::GET, "/1/indexes/products/snapshots"),
    ] {
        assert_required_acl(method_and_path.0, method_and_path.1, "admin");
    }
}

#[test]
fn acl_compact_requires_admin() {
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/indexes/products/compact"),
        Some("admin")
    );
}

/// Verify that recommend-rules sub-routes map batch to `editSettings`, search to `settings`, GET to `settings`, and DELETE to `editSettings`.
#[test]
fn acl_recommend_rules_routes_require_settings_acls() {
    assert_required_acl(
        Method::POST,
        "/1/indexes/products/related-products/recommend/rules/batch",
        "editSettings",
    );
    assert_required_acl(
        Method::POST,
        "/1/indexes/products/related-products/recommend/rules/search",
        "settings",
    );
    assert_required_acl(
        Method::GET,
        "/1/indexes/products/related-products/recommend/rules/rule_1",
        "settings",
    );
    assert_required_acl(
        Method::DELETE,
        "/1/indexes/products/related-products/recommend/rules/rule_1",
        "editSettings",
    );
}

#[test]
fn acl_logs_requires_logs() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/1/logs/my_index"),
        Some("logs")
    );
}

#[test]
fn acl_query_suggestions_config_get_requires_settings() {
    assert_required_acl(Method::GET, "/1/configs", "settings");
    assert_required_acl(Method::GET, "/1/configs/products", "settings");
    assert_required_acl(Method::GET, "/1/configs/products/status", "settings");
}

/// Verify that mutating Query Suggestions config routes (POST, PUT, DELETE) require the `editSettings` ACL.
#[test]
fn acl_query_suggestions_config_write_requires_edit_settings() {
    for method_and_path in [
        (Method::POST, "/1/configs"),
        (Method::PUT, "/1/configs/products"),
        (Method::DELETE, "/1/configs/products"),
        (Method::POST, "/1/configs/products/build"),
    ] {
        assert_required_acl(method_and_path.0, method_and_path.1, "editSettings");
    }
}
