use axum::http::Method;

pub(crate) fn is_acme_challenge_path(path: &str) -> bool {
    path.starts_with("/.well-known/acme-challenge/")
}

fn read_or_write_acl(
    method: &Method,
    read_acl: &'static str,
    write_acl: &'static str,
) -> Option<&'static str> {
    Some(if *method == Method::GET {
        read_acl
    } else {
        write_acl
    })
}

/// Maps an HTTP method and path to the ACL permission string required for access.
/// Returns `None` for public paths (ACME challenges) that need no authentication.
pub fn required_acl_for_route(method: &Method, path: &str) -> Option<&'static str> {
    if is_acme_challenge_path(path) {
        return None;
    }

    if let Some(acl) = fixed_path_acl(method, path) {
        return Some(acl);
    }

    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if let Some(acl) = indexes_acl(method, &parts) {
        return acl;
    }
    if let Some(acl) = dictionaries_acl(method, &parts) {
        return Some(acl);
    }
    if tasks_acl(&parts) {
        return Some("search");
    }

    None
}

/// Resolves ACL for non-index routes: keys, usage, analytics, personalization, logs,
/// configs, metrics, internal endpoints, A/B tests, events, and user-token deletion.
fn fixed_path_acl(method: &Method, path: &str) -> Option<&'static str> {
    if path == "/1/migrate-from-algolia" || path == "/1/algolia-list-indexes" {
        return Some("admin");
    }
    if path.starts_with("/1/migrations/") {
        return Some("admin");
    }
    if path.starts_with("/1/keys") || path.starts_with("/1/security/sources") {
        return Some("admin");
    }
    if path.starts_with("/1/usage") {
        return Some("usage");
    }
    if path.starts_with("/1/strategies/personalization") || path.starts_with("/1/profiles/") {
        return Some("personalization");
    }
    if path.starts_with("/1/logs") {
        return Some("logs");
    }
    if path.starts_with("/1/configs") {
        return read_or_write_acl(method, "settings", "editSettings");
    }
    if path == "/metrics" || path.starts_with("/internal/") {
        return Some("admin");
    }
    if matches!(
        path,
        "/2/analytics/seed" | "/2/analytics/clear" | "/2/analytics/cleanup" | "/2/analytics/flush"
    ) {
        return Some("admin");
    }
    if path.starts_with("/2/abtests") {
        return Some(if path == "/2/abtests/estimate" || *method == Method::GET {
            "analytics"
        } else {
            "editSettings"
        });
    }
    if path.starts_with("/2/") {
        return Some("analytics");
    }
    if path == "/1/events" || path == "/1/events/debug" {
        return Some("search");
    }
    if *method == Method::DELETE && path.starts_with("/1/usertokens/") {
        return Some("deleteObject");
    }
    None
}

/// Resolves ACL for `/1/indexes/...` routes based on path depth and HTTP method.
/// Returns `None` (outer Option) if the path doesn't match the indexes prefix.
fn indexes_acl(method: &Method, parts: &[&str]) -> Option<Option<&'static str>> {
    if parts.len() == 2 && parts[0] == "1" && parts[1] == "indexes" {
        return Some(match *method {
            Method::GET => Some("listIndexes"),
            Method::POST => Some("addObject"),
            _ => None,
        });
    }

    if !(parts.len() >= 3 && parts[0] == "1" && parts[1] == "indexes") {
        return None;
    }

    if parts.len() == 3 && !parts[2].is_empty() {
        return Some(match *method {
            Method::GET => Some("search"),
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
            Method::GET => Some("search"),
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
