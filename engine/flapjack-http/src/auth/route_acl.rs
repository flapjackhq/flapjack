use super::PRIVATE_MIGRATION_ACL;
use axum::http::Method;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteAcl {
    Required(&'static str),
    PeerOrAdmin,
    Public,
    Unmapped,
}

pub(crate) fn is_acme_challenge_path(path: &str) -> bool {
    path.starts_with("/.well-known/acme-challenge/")
}

fn is_read_method(method: &Method) -> bool {
    *method == Method::GET || *method == Method::HEAD
}

fn read_or_write_acl(
    method: &Method,
    read_acl: &'static str,
    write_acl: &'static str,
) -> Option<&'static str> {
    Some(if is_read_method(method) {
        read_acl
    } else {
        write_acl
    })
}

/// Maps an HTTP method and path to its authorization requirement.
pub fn required_acl_for_route(method: &Method, path: &str) -> RouteAcl {
    if is_acme_challenge_path(path) {
        // Route exposure normally short-circuits public ACME requests before ACL
        // evaluation. Keep this defensive result so direct mapper callers cannot
        // mistake a public route for an unmapped protected route.
        return RouteAcl::Public;
    }

    if let Some(acl) = fixed_path_acl(method, path) {
        return acl;
    }

    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if let Some(indexes_acl) = indexes_acl(method, &parts) {
        return indexes_acl.map_or(RouteAcl::Unmapped, RouteAcl::Required);
    }
    if let Some(acl) = dictionaries_acl(method, &parts) {
        return RouteAcl::Required(acl);
    }
    if tasks_acl(&parts) {
        return RouteAcl::Required("search");
    }

    RouteAcl::Unmapped
}

/// Resolves ACL for non-index routes: keys, usage, analytics, personalization, logs,
/// configs, metrics, internal endpoints, A/B tests, events, and user-token deletion.
fn fixed_path_acl(method: &Method, path: &str) -> Option<RouteAcl> {
    if path == "/1/dashboard/session" {
        return match *method {
            Method::POST => Some(RouteAcl::Public),
            Method::DELETE => Some(RouteAcl::Required("admin")),
            _ => None,
        };
    }
    if *method == Method::POST && path == "/1/migrations/privacy-scrub" {
        return Some(RouteAcl::Required(PRIVATE_MIGRATION_ACL));
    }
    if path == "/1/migrate-from-algolia" || path == "/1/algolia-list-indexes" {
        return Some(RouteAcl::Required("admin"));
    }
    if path.starts_with("/1/migrations/") {
        return Some(RouteAcl::Required("admin"));
    }
    if path.starts_with("/1/keys") || path.starts_with("/1/security/sources") {
        return Some(RouteAcl::Required("admin"));
    }
    if path.starts_with("/1/usage") {
        return Some(RouteAcl::Required("usage"));
    }
    if path.starts_with("/1/strategies/personalization") || path.starts_with("/1/profiles/") {
        return Some(RouteAcl::Required("personalization"));
    }
    if path.starts_with("/1/logs") {
        return Some(RouteAcl::Required("logs"));
    }
    if path.starts_with("/1/configs") {
        return read_or_write_acl(method, "settings", "editSettings").map(RouteAcl::Required);
    }
    if path == "/metrics" {
        return Some(RouteAcl::Required("admin"));
    }
    if is_peer_or_admin_internal_route(method, path) {
        return Some(RouteAcl::PeerOrAdmin);
    }
    if path.starts_with("/internal/") {
        return Some(RouteAcl::Required("admin"));
    }
    if matches!(
        path,
        "/2/analytics/seed" | "/2/analytics/clear" | "/2/analytics/cleanup" | "/2/analytics/flush"
    ) {
        return Some(RouteAcl::Required("admin"));
    }
    if path.starts_with("/2/abtests") {
        return Some(RouteAcl::Required(
            if path == "/2/abtests/estimate" || is_read_method(method) {
                "analytics"
            } else {
                "editSettings"
            },
        ));
    }
    if path.starts_with("/2/") {
        return Some(RouteAcl::Required("analytics"));
    }
    if path == "/1/events" || path == "/1/events/debug" {
        return Some(RouteAcl::Required("search"));
    }
    if *method == Method::DELETE && path.starts_with("/1/usertokens/") {
        return Some(RouteAcl::Required("deleteObject"));
    }
    None
}

fn is_peer_or_admin_internal_route(method: &Method, path: &str) -> bool {
    if *method == Method::GET {
        return matches!(
            path,
            "/internal/status"
                | "/internal/cluster/status"
                | "/internal/snapshots/capability"
                | "/internal/ops"
                | "/internal/tenants"
        ) || is_internal_snapshot_tenant_path(path);
    }

    *method == Method::POST && matches!(path, "/internal/replicate" | "/internal/analytics-rollup")
}

fn is_internal_snapshot_tenant_path(path: &str) -> bool {
    path.strip_prefix("/internal/snapshot/")
        .is_some_and(|tenant_id| !tenant_id.is_empty() && !tenant_id.contains('/'))
}

/// Resolves ACL for `/1/indexes/...` routes based on path depth and HTTP method.
/// Returns `None` (outer Option) if the path doesn't match the indexes prefix.
fn indexes_acl(method: &Method, parts: &[&str]) -> Option<Option<&'static str>> {
    if parts.len() == 2 && parts[0] == "1" && parts[1] == "indexes" {
        return Some(match *method {
            Method::GET | Method::HEAD => Some("listIndexes"),
            Method::POST => Some("addObject"),
            _ => None,
        });
    }

    if !(parts.len() >= 3 && parts[0] == "1" && parts[1] == "indexes") {
        return None;
    }

    if parts.len() == 3 && !parts[2].is_empty() {
        return Some(match *method {
            Method::GET | Method::HEAD => Some("search"),
            Method::DELETE => Some("deleteIndex"),
            Method::POST => Some("addObject"),
            _ => None,
        });
    }

    if parts.len() >= 4 {
        return Some(index_nested_acl(method, parts));
    }

    Some(None)
}

/// Resolves ACL for nested index sub-routes (`/1/indexes/{name}/{action}`):
/// query, batch, settings, synonyms, rules, browse, chat, snapshots, and more.
fn index_nested_acl(method: &Method, parts: &[&str]) -> Option<&'static str> {
    if parts.len() == 5 && parts[4] == "partial" {
        return Some("addObject");
    }
    if parts.len() >= 7 && parts[4] == "recommend" && parts[5] == "rules" {
        return match parts[6] {
            "batch" => Some("editSettings"),
            "search" => Some("settings"),
            _ => read_or_write_acl(method, "settings", "editSettings"),
        };
    }

    match parts[3] {
        "query" | "queries" | "objects" | "facets" | "task" => Some("search"),
        "browse" => Some("browse"),
        "chat" => Some("inference"),
        "batch" | "operation" => Some("addObject"),
        "clear" | "deleteByQuery" => Some("deleteObject"),
        "compact" | "export" | "import" | "snapshot" | "restore" | "snapshots" => Some("admin"),
        "settings" | "synonyms" | "rules" => read_or_write_acl(method, "settings", "editSettings"),
        "recommendations" => Some("recommendation"),
        _ if parts.len() == 4 => match *method {
            Method::GET | Method::HEAD => Some("search"),
            Method::PUT => Some("addObject"),
            Method::DELETE => Some("deleteObject"),
            _ => Some("admin"),
        },
        _ => Some("admin"),
    }
}

fn dictionaries_acl(method: &Method, parts: &[&str]) -> Option<&'static str> {
    if !(parts.len() >= 4 && parts[0] == "1" && parts[1] == "dictionaries") {
        return None;
    }

    match parts[3] {
        "batch" => Some("editSettings"),
        "search" | "languages" => Some("settings"),
        "settings" => read_or_write_acl(method, "settings", "editSettings"),
        _ => None,
    }
}

fn tasks_acl(parts: &[&str]) -> bool {
    parts.len() >= 2 && parts[0] == "1" && (parts[1] == "tasks" || parts[1] == "task")
}

// Stage 1 boundary contract: the closed `/internal/*` denominator and its
// peer-allowed / admin-only decisions. Kept in this module so the contract
// lives with its only mapper.
#[cfg(test)]
#[path = "../auth_tests/peer_boundary_route_acl_tests.rs"]
mod peer_boundary_route_acl_tests;
