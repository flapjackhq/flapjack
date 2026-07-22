//! Auth middleware for API key extraction and ACL-based authorization, validating keys and enforcing access controls for Algolia-compatible routes.
use axum::{
    extract::Request,
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::net::IpAddr;

use crate::error_response::json_error;

use super::{
    api_key_restrict_sources_match, invalid_api_credentials_error, key_allows_index,
    referer_matches, request_application_id, required_acl_for_route, restrict_sources_match,
    validate_secured_key, ApiKey, AuthenticatedAppId, KeyStore, RateLimiter,
    SecuredKeyRestrictions,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteExposure {
    Public,
    Disabled,
    Protected,
}

pub(crate) fn route_exposure(path: &str, disable_dashboard: bool) -> RouteExposure {
    if is_always_public_path(path) {
        return RouteExposure::Public;
    }

    if is_dashboard_or_docs_path(path) {
        if disable_dashboard {
            RouteExposure::Disabled
        } else {
            RouteExposure::Public
        }
    } else {
        RouteExposure::Protected
    }
}

#[cfg(test)]
pub(crate) fn is_public_path(path: &str, disable_dashboard: bool) -> bool {
    route_exposure(path, disable_dashboard) == RouteExposure::Public
}

fn is_always_public_path(path: &str) -> bool {
    path == "/health" || path == "/health/ready" || super::is_acme_challenge_path(path)
}

fn is_dashboard_or_docs_path(path: &str) -> bool {
    path == "/dashboard"
        || path.starts_with("/dashboard/")
        || path.starts_with("/swagger-ui")
        || path.starts_with("/api-docs")
}

fn is_own_key_read_request(method: &Method, path: &str, api_key_value: &str) -> bool {
    *method == Method::GET
        && path
            .strip_prefix("/1/keys/")
            .is_some_and(|suffix| suffix == api_key_value)
}

pub(crate) fn extract_index_name(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.len() >= 3 && parts[0] == "1" && parts[1] == "indexes" {
        let name = parts[2];
        // Skip wildcard "*" (multi-index batch path), "queries", and "objects"
        // — these are path markers, not real index names.
        if name != "queries" && name != "objects" && name != "*" {
            return Some(name.to_string());
        }
    }
    None
}

/// Extract API key from request headers or query string.
///
/// First checks the `x-algolia-api-key` header. If not found and the path is `/metrics`, returns `None` to prevent credential leakage via logs, shell history, or proxy access logs. Otherwise attempts to extract the key from the `x-algolia-api-key` query string parameter.
///
/// # Arguments
///
/// * `request` - The incoming HTTP request
///
/// # Returns
///
/// `Some(key)` if an API key is found, `None` otherwise.
fn extract_api_key(request: &Request) -> Option<String> {
    if let Some(val) = request.headers().get("x-algolia-api-key") {
        return val.to_str().ok().map(|s| s.to_string());
    }
    // `/metrics` is an operational admin endpoint, not an Algolia-compatible route.
    // Reject URL-borne credentials here so admin keys do not leak via logs,
    // shell history, proxy access logs, or referrer-like surfaces.
    if request.uri().path() == "/metrics" {
        return None;
    }
    if let Some(query) = request.uri().query() {
        for pair in query.split('&') {
            if let Some(val) = pair.strip_prefix("x-algolia-api-key=") {
                return Some(val.to_string());
            }
        }
    }
    None
}

fn lookup_authenticated_key(
    key_store: &KeyStore,
    api_key_value: &str,
) -> Option<(ApiKey, Option<SecuredKeyRestrictions>)> {
    match key_store.lookup(api_key_value) {
        Some(api_key) => Some((api_key, None)),
        None => validate_secured_key(api_key_value, key_store)
            .map(|(parent_key, restrictions)| (parent_key, Some(restrictions))),
    }
}

fn classify_auth_attempt_type(key_store: &KeyStore, api_key_value: &str) -> &'static str {
    if key_store.lookup(api_key_value).is_some() {
        return "direct";
    }
    if validate_secured_key(api_key_value, key_store).is_some() {
        return "secured";
    }
    "direct"
}

fn log_auth_failure(path: &str, auth_attempt_type: &str, reason: &str) {
    tracing::warn!(
        event = "security_audit_auth_failure",
        auth_attempt_type,
        reason,
        path,
        "security event: auth failure"
    );
}

fn ensure_key_is_not_expired(api_key: &ApiKey) -> Option<Response> {
    if api_key.validity <= 0 {
        return None;
    }

    let expires_at = api_key.created_at + (api_key.validity * 1000);
    if chrono::Utc::now().timestamp_millis() > expires_at {
        return Some(invalid_api_credentials_error());
    }

    None
}

/// Checks if the API key's ACL grants access to the requested route, returning 403 if not.
/// Admin routes require an admin key or a self-read of the key's own `/1/keys/{value}` path.
fn ensure_route_acl_allows_request(
    key_store: &KeyStore,
    api_key: &ApiKey,
    api_key_value: &str,
    method: &Method,
    path: &str,
) -> Option<Response> {
    let required_acl = required_acl_for_route(method, path)?;

    let has_access = if required_acl == "admin" {
        key_store.is_admin(api_key_value) || is_own_key_read_request(method, path, api_key_value)
    } else {
        api_key.acl.iter().any(|acl| acl == required_acl)
    };

    if has_access {
        None
    } else {
        Some(json_error(
            StatusCode::FORBIDDEN,
            "Method not allowed with this API key",
        ))
    }
}

/// Validates the request Referer header against the API key's allowed referer patterns.
/// Returns 403 if the key has a non-empty referers list and the request's Referer doesn't match.
fn ensure_referer_is_allowed(request: &Request, api_key: &ApiKey) -> Option<Response> {
    if api_key.referers.is_empty() {
        return None;
    }

    let referer = request
        .headers()
        .get("referer")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    if referer_matches(referer, &api_key.referers) {
        None
    } else {
        Some(json_error(StatusCode::FORBIDDEN, "Referer not allowed"))
    }
}

/// Validates the client IP and referer against the API key's `restrictSources` list
/// and any secured-key source restrictions. Returns 403 if the client is not allowed.
fn ensure_sources_allow_request(
    request: &Request,
    api_key: &ApiKey,
    secured_restrictions: Option<&SecuredKeyRestrictions>,
) -> Option<Response> {
    if let Some(restrict_sources) = api_key.restrict_sources.as_ref() {
        let client_ip = crate::middleware::extract_client_ip_opt(request);
        let referer = request
            .headers()
            .get("referer")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if !api_key_restrict_sources_match(restrict_sources, client_ip, referer) {
            return Some(invalid_api_credentials_error());
        }
    }

    if let Some(restrict_sources) =
        secured_restrictions.and_then(|restrictions| restrictions.restrict_sources.as_deref())
    {
        let client_ip = crate::middleware::extract_rate_limit_ip(request);
        if !restrict_sources_match(restrict_sources, client_ip) {
            return Some(invalid_api_credentials_error());
        }
    }

    None
}

/// Enforces per-key, per-IP hourly rate limiting via `maxQueriesPerIPPerHour`.
/// Returns 429 if the limit is exceeded for this key+IP combination.
fn ensure_rate_limit_allows_request(
    request: &Request,
    api_key: &ApiKey,
    client_ip: IpAddr,
) -> Option<Response> {
    if api_key.max_queries_per_ip_per_hour <= 0 {
        return None;
    }

    if let Some(rate_limiter) = request.extensions().get::<RateLimiter>().cloned() {
        if !rate_limiter.check_and_increment(
            &api_key.hash,
            client_ip,
            api_key.max_queries_per_ip_per_hour as u64,
        ) {
            return Some(json_error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many requests per IP per hour",
            ));
        }
    }

    None
}

/// Checks that the API key (and any secured-key index restrictions) permits access
/// to the index named in the URL path. Returns 403 if the index is not allowed.
fn ensure_index_access_is_allowed(
    path: &str,
    api_key: &ApiKey,
    secured_restrictions: Option<&SecuredKeyRestrictions>,
) -> Option<Response> {
    let index_name = extract_index_name(path)?;

    if !key_allows_index(api_key, secured_restrictions, &index_name) {
        return Some(invalid_api_credentials_error());
    }

    None
}

/// Axum middleware that enforces API key authentication and ACL-based authorization.
///
/// Skips auth for OPTIONS requests and public paths. Validates the key (direct lookup or secured-key HMAC), checks ACL permissions, referer/source allowlists, rate limits, index restrictions, and key expiry. Injects `AuthenticatedAppId`, the authenticated `ApiKey`, and optional `SecuredKeyRestrictions` into request extensions for downstream handlers.
pub async fn authenticate_and_authorize(
    request: Request,
    next: Next,
    disable_dashboard: bool,
) -> Result<Response, Response> {
    if request.method() == Method::OPTIONS {
        return Ok(next.run(request).await);
    }

    let path = request.uri().path().to_string();

    match route_exposure(&path, disable_dashboard) {
        RouteExposure::Public => return Ok(next.run(request).await),
        RouteExposure::Disabled => return Err(StatusCode::NOT_FOUND.into_response()),
        RouteExposure::Protected => {}
    }

    let Some(key_store) = request.extensions().get::<std::sync::Arc<KeyStore>>() else {
        return Err(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error",
        ));
    };
    let key_store = key_store.clone();

    let application_id_opt = request_application_id(&request);

    let api_key_value = match extract_api_key(&request) {
        Some(key_value) => key_value,
        None => {
            log_auth_failure(&path, "missing", "api_key_missing");
            return Err(invalid_api_credentials_error());
        }
    };

    let application_id = match application_id_opt {
        Some(id) => id,
        // Allow admin-key-only metrics scraping for metering agents.
        None if path == "/metrics" && key_store.is_admin(&api_key_value) => String::new(),
        None => {
            log_auth_failure(
                &path,
                classify_auth_attempt_type(&key_store, &api_key_value),
                "application_id_missing",
            );
            return Err(invalid_api_credentials_error());
        }
    };

    let (api_key, secured_restrictions) = match lookup_authenticated_key(&key_store, &api_key_value)
    {
        Some(authenticated) => authenticated,
        None => {
            log_auth_failure(
                &path,
                classify_auth_attempt_type(&key_store, &api_key_value),
                "invalid_credentials",
            );
            return Err(invalid_api_credentials_error());
        }
    };
    if let Some(response) = ensure_key_is_not_expired(&api_key) {
        return Err(response);
    }
    if let Some(response) = ensure_route_acl_allows_request(
        &key_store,
        &api_key,
        &api_key_value,
        request.method(),
        &path,
    ) {
        return Err(response);
    }
    if let Some(response) = ensure_referer_is_allowed(&request, &api_key) {
        return Err(response);
    }

    if let Some(response) =
        ensure_sources_allow_request(&request, &api_key, secured_restrictions.as_ref())
    {
        return Err(response);
    }
    let client_ip = crate::middleware::extract_rate_limit_ip(&request);
    if let Some(response) = ensure_rate_limit_allows_request(&request, &api_key, client_ip) {
        return Err(response);
    }
    if let Some(response) =
        ensure_index_access_is_allowed(&path, &api_key, secured_restrictions.as_ref())
    {
        return Err(response);
    }

    let mut request = request;
    // Store auth context for downstream handlers (search and dictionaries rely on these values).
    request
        .extensions_mut()
        .insert(AuthenticatedAppId(application_id));
    request.extensions_mut().insert(api_key);
    if let Some(restrictions) = secured_restrictions {
        request.extensions_mut().insert(restrictions);
    }

    Ok(next.run(request).await)
}
