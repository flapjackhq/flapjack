use super::PRIVATE_MIGRATION_ACL;
use axum::http::Method;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteAcl {
    Required(&'static str),
    PeerOrAdmin,
    Public,
    Unmapped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteEffect {
    Read,
    Mutation,
    FenceControl,
    Unmapped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RoutePolicy {
    pub acl: RouteAcl,
    pub effect: RouteEffect,
}

const fn policy(acl: RouteAcl, effect: RouteEffect) -> RoutePolicy {
    RoutePolicy { acl, effect }
}

const fn required(acl: &'static str, effect: RouteEffect) -> RoutePolicy {
    policy(RouteAcl::Required(acl), effect)
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
    route_policy(method, path).acl
}

pub(crate) fn route_policy(method: &Method, path: &str) -> RoutePolicy {
    if is_acme_challenge_path(path) {
        // Route exposure normally short-circuits public ACME requests before ACL
        // evaluation. Keep this defensive result so direct mapper callers cannot
        // mistake a public route for an unmapped protected route.
        return policy(RouteAcl::Public, RouteEffect::Read);
    }

    if let Some(policy) = fixed_path_policy(method, path) {
        return policy;
    }

    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if let Some(indexes_policy) = indexes_policy(method, &parts) {
        return indexes_policy.unwrap_or(policy(RouteAcl::Unmapped, RouteEffect::Unmapped));
    }
    if let Some(policy) = dictionaries_policy(method, &parts) {
        return policy;
    }
    if tasks_acl(&parts) {
        return required("search", RouteEffect::Read);
    }

    policy(RouteAcl::Unmapped, RouteEffect::Unmapped)
}

/// Resolves ACL for non-index routes: keys, usage, analytics, personalization, logs,
/// configs, metrics, internal endpoints, A/B tests, events, and user-token deletion.
fn fixed_path_policy(method: &Method, path: &str) -> Option<RoutePolicy> {
    if path == "/1/dashboard/session" {
        return match *method {
            Method::POST => Some(policy(RouteAcl::Public, RouteEffect::Mutation)),
            Method::DELETE => Some(required("admin", RouteEffect::Mutation)),
            _ => None,
        };
    }
    if *method == Method::POST && path == "/1/migrations/privacy-scrub" {
        return Some(required(PRIVATE_MIGRATION_ACL, RouteEffect::Mutation));
    }
    if path == "/1/migrate-from-algolia" {
        return Some(required("admin", RouteEffect::Mutation));
    }
    if path == "/1/algolia-list-indexes" {
        return Some(required("admin", RouteEffect::Read));
    }
    if path.starts_with("/1/migrations/") {
        let read = is_read_method(method)
            || (*method == Method::POST
                && (path.ends_with("/preview") || path.ends_with("/list-indexes")));
        return Some(required(
            "admin",
            if read {
                RouteEffect::Read
            } else {
                RouteEffect::Mutation
            },
        ));
    }
    if path.starts_with("/1/keys") || path.starts_with("/1/security/sources") {
        return Some(required(
            "admin",
            if is_read_method(method) {
                RouteEffect::Read
            } else {
                RouteEffect::Mutation
            },
        ));
    }
    if path.starts_with("/1/usage") {
        return Some(required("usage", RouteEffect::Read));
    }
    if path.starts_with("/1/strategies/personalization") || path.starts_with("/1/profiles/") {
        return Some(required(
            "personalization",
            if is_read_method(method) {
                RouteEffect::Read
            } else {
                RouteEffect::Mutation
            },
        ));
    }
    if path.starts_with("/1/logs") {
        return Some(required("logs", RouteEffect::Read));
    }
    if path.starts_with("/1/configs") {
        return read_or_write_acl(method, "settings", "editSettings").map(|acl| {
            required(
                acl,
                if is_read_method(method) {
                    RouteEffect::Read
                } else {
                    RouteEffect::Mutation
                },
            )
        });
    }
    if path == "/metrics" {
        return Some(required("admin", RouteEffect::Read));
    }
    if matches!(
        (method, path),
        (&Method::GET, "/internal/release-write-fence/status")
            | (&Method::POST, "/internal/release-write-fence/acquire")
            | (&Method::POST, "/internal/release-write-fence/release")
    ) {
        return Some(required("admin", RouteEffect::FenceControl));
    }
    if is_peer_or_admin_internal_route(method, path) {
        return Some(policy(
            RouteAcl::PeerOrAdmin,
            if is_read_method(method) {
                RouteEffect::Read
            } else {
                RouteEffect::Mutation
            },
        ));
    }
    if path.starts_with("/internal/") {
        let read = is_read_method(method)
            || (*method == Method::POST
                && path.starts_with("/internal/indexes/")
                && path.ends_with("/count"));
        return Some(required(
            "admin",
            if read {
                RouteEffect::Read
            } else {
                RouteEffect::Mutation
            },
        ));
    }
    if matches!(
        path,
        "/2/analytics/seed" | "/2/analytics/clear" | "/2/analytics/cleanup" | "/2/analytics/flush"
    ) {
        return Some(required("admin", RouteEffect::Mutation));
    }
    if path.starts_with("/2/abtests") {
        let read = path == "/2/abtests/estimate" || is_read_method(method);
        return Some(required(
            if read { "analytics" } else { "editSettings" },
            if read {
                RouteEffect::Read
            } else {
                RouteEffect::Mutation
            },
        ));
    }
    if path.starts_with("/2/") {
        return Some(required(
            "analytics",
            if is_read_method(method) {
                RouteEffect::Read
            } else {
                RouteEffect::Mutation
            },
        ));
    }
    if path == "/1/events" {
        return Some(required("search", RouteEffect::Mutation));
    }
    if path == "/1/events/debug" {
        return Some(required("analytics", RouteEffect::Read));
    }
    if *method == Method::DELETE && path.starts_with("/1/usertokens/") {
        return Some(required("deleteObject", RouteEffect::Mutation));
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
fn indexes_policy(method: &Method, parts: &[&str]) -> Option<Option<RoutePolicy>> {
    if parts.len() == 2 && parts[0] == "1" && parts[1] == "indexes" {
        return Some(match *method {
            Method::GET | Method::HEAD => Some(required("listIndexes", RouteEffect::Read)),
            Method::POST => Some(required("addObject", RouteEffect::Mutation)),
            _ => None,
        });
    }

    if !(parts.len() >= 3 && parts[0] == "1" && parts[1] == "indexes") {
        return None;
    }

    if parts.len() == 3 && !parts[2].is_empty() {
        return Some(match *method {
            Method::GET | Method::HEAD => Some(required("search", RouteEffect::Read)),
            Method::DELETE => Some(required("deleteIndex", RouteEffect::Mutation)),
            Method::POST => Some(required("addObject", RouteEffect::Mutation)),
            _ => None,
        });
    }

    if parts.len() >= 4 {
        return Some(index_nested_policy(method, parts));
    }

    Some(None)
}

/// Resolves ACL for nested index sub-routes (`/1/indexes/{name}/{action}`):
/// query, batch, settings, synonyms, rules, browse, chat, snapshots, and more.
fn index_nested_policy(method: &Method, parts: &[&str]) -> Option<RoutePolicy> {
    if parts.len() == 5 && parts[4] == "partial" {
        return Some(required("addObject", RouteEffect::Mutation));
    }
    if parts.len() >= 7 && parts[4] == "recommend" && parts[5] == "rules" {
        return match parts[6] {
            "batch" => Some(required("editSettings", RouteEffect::Mutation)),
            "search" => Some(required("settings", RouteEffect::Read)),
            _ => read_or_write_acl(method, "settings", "editSettings").map(|acl| {
                required(
                    acl,
                    if is_read_method(method) {
                        RouteEffect::Read
                    } else {
                        RouteEffect::Mutation
                    },
                )
            }),
        };
    }

    match parts[3] {
        "query" | "queries" | "objects" | "facets" | "task" => {
            Some(required("search", RouteEffect::Read))
        }
        "browse" => Some(required("browse", RouteEffect::Read)),
        "chat" => Some(required("inference", RouteEffect::Read)),
        "batch" | "operation" => Some(required("addObject", RouteEffect::Mutation)),
        "clear" | "deleteByQuery" => Some(required("deleteObject", RouteEffect::Mutation)),
        "compact" | "export" | "import" | "snapshot" | "restore" | "snapshots" => {
            Some(required("admin", RouteEffect::Mutation))
        }
        "settings" | "synonyms" | "rules" => read_or_write_acl(method, "settings", "editSettings")
            .map(|acl| {
                required(
                    acl,
                    if is_read_method(method) {
                        RouteEffect::Read
                    } else {
                        RouteEffect::Mutation
                    },
                )
            }),
        "recommendations" => Some(required("recommendation", RouteEffect::Read)),
        _ if parts.len() == 4 => match *method {
            Method::GET | Method::HEAD => Some(required("search", RouteEffect::Read)),
            Method::PUT => Some(required("addObject", RouteEffect::Mutation)),
            Method::DELETE => Some(required("deleteObject", RouteEffect::Mutation)),
            _ => Some(required("admin", RouteEffect::Mutation)),
        },
        _ => Some(required("admin", RouteEffect::Mutation)),
    }
}

fn dictionaries_policy(method: &Method, parts: &[&str]) -> Option<RoutePolicy> {
    if !(parts.len() >= 4 && parts[0] == "1" && parts[1] == "dictionaries") {
        return None;
    }

    match parts[3] {
        "batch" => Some(required("editSettings", RouteEffect::Mutation)),
        "search" | "languages" => Some(required("settings", RouteEffect::Read)),
        "settings" => read_or_write_acl(method, "settings", "editSettings").map(|acl| {
            required(
                acl,
                if is_read_method(method) {
                    RouteEffect::Read
                } else {
                    RouteEffect::Mutation
                },
            )
        }),
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
