//! Auth middleware for API key extraction and ACL-based authorization, validating keys and enforcing access controls for Algolia-compatible routes.
use axum::{
    extract::Request,
    http::{header, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::net::IpAddr;

use crate::api_profile::{
    is_paid_beta_v3_customer_path, ApiProfile, PaidBetaV1CustomerRequest,
    PaidBetaV3CustomerRequest, PAID_BETA_V1_APPLICATION_ID, PAID_BETA_V1_DIRECT_SEARCH_PATH,
};
use crate::error_response::json_error;
use crate::security_audit::{self, Actor, AuditPath, Target};

use super::route_acl::RouteAcl;
use super::session::DashboardSessionStore;
use super::session_cookie::presented_session_token;
use super::{
    api_key_restrict_sources_match, canonical_request_credential, invalid_api_credentials_error,
    key_allows_index, referer_matches, required_acl_for_route, restrict_sources_match,
    validate_secured_key, ApiKey, AuthenticatedAppId, CanonicalRequestCredential, KeyStore,
    RateLimiter, ReplicationPeerCredential, SecuredKeyRestrictions,
    REPLICATION_PEER_APPLICATION_ID,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteExposure {
    Public,
    Disabled,
    Protected,
}

pub(crate) const DASHBOARD_SESSION_EXCHANGES_PER_IP_PER_HOUR: u64 = 10;
const DASHBOARD_SESSION_EXCHANGE_RATE_LIMIT_BUCKET: &str = "dashboard-session-exchange";

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
/// Query transport is limited to the read-only Search/Browse ACL surfaces used
/// by official browser clients. Indexing, debugger, administration, migration,
/// replication, and every unmapped route remain header-only.
///
/// # Arguments
///
/// * `request` - The incoming HTTP request
///
/// # Returns
///
/// `Some(key)` if an API key is found, `None` otherwise.
fn extract_api_key_for_route(
    request: &Request,
    route_acl: RouteAcl,
) -> Result<Option<CanonicalRequestCredential>, ()> {
    let credential = canonical_request_credential(request, "x-algolia-api-key")?;
    if credential
        .as_ref()
        .is_some_and(|presented| presented.from_query && requires_header_credentials(route_acl))
    {
        return Err(());
    }
    Ok(credential)
}

fn extract_bearer_api_key(request: &Request) -> Option<String> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty() && !value.contains(char::is_whitespace))
        .map(str::to_string)
}

#[cfg(test)]
fn extract_api_key(request: &Request) -> Option<String> {
    extract_api_key_for_route(
        request,
        required_acl_for_route(request.method(), request.uri().path()),
    )
    .ok()
    .flatten()
    .map(|credential| credential.value)
}

fn requires_header_credentials(route_acl: RouteAcl) -> bool {
    !matches!(
        route_acl,
        RouteAcl::Required("search") | RouteAcl::Required("browse")
    )
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

fn log_auth_failure(path: &str, auth_attempt_type: &'static str, reason: &'static str) {
    security_audit::emit_auth_failure(AuditPath::for_auth_route(path), auth_attempt_type, reason);
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
#[cfg(test)]
fn ensure_route_acl_allows_request(
    key_store: &KeyStore,
    api_key: &ApiKey,
    api_key_value: &str,
    method: &Method,
    path: &str,
) -> Option<Response> {
    ensure_route_acl_allows_request_for_acl(
        key_store,
        api_key,
        api_key_value,
        required_acl_for_route(method, path),
        method,
        path,
    )
}

fn ensure_route_acl_allows_request_for_acl(
    key_store: &KeyStore,
    api_key: &ApiKey,
    api_key_value: &str,
    route_acl: RouteAcl,
    method: &Method,
    path: &str,
) -> Option<Response> {
    let has_access = match route_acl {
        RouteAcl::Public => return None,
        // route_acl_denies_unmapped_route_by_default and
        // unmapped_route_refusal_carries_the_json_error_envelope require
        // fall-through to json_error(FORBIDDEN, "Method not allowed with this API key").
        RouteAcl::Unmapped => false,
        RouteAcl::Required("admin") => {
            key_store.is_admin(api_key_value)
                || is_own_key_read_request(method, path, api_key_value)
        }
        RouteAcl::PeerOrAdmin => key_store.is_admin(api_key_value),
        RouteAcl::Required(required_acl) => api_key.acl.iter().any(|acl| acl == required_acl),
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

fn is_configured_replication_peer_request(
    request: &Request,
    route_acl: RouteAcl,
    application_id: &str,
    api_key_value: &str,
) -> bool {
    if route_acl != RouteAcl::PeerOrAdmin || application_id != REPLICATION_PEER_APPLICATION_ID {
        return false;
    }

    request
        .extensions()
        .get::<ReplicationPeerCredential>()
        .is_some_and(|credential| credential.matches_secret(api_key_value))
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

fn ensure_dashboard_session_exchange_rate_limit(request: &Request) -> Option<Response> {
    let Some(rate_limiter) = request.extensions().get::<RateLimiter>() else {
        return Some(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error",
        ));
    };
    let client_ip = crate::middleware::extract_rate_limit_ip(request);
    if rate_limiter.check_and_increment(
        DASHBOARD_SESSION_EXCHANGE_RATE_LIMIT_BUCKET,
        client_ip,
        DASHBOARD_SESSION_EXCHANGES_PER_IP_PER_HOUR,
    ) {
        None
    } else {
        Some(json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many dashboard session exchange attempts per IP per hour",
        ))
    }
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
    let path = request.uri().path().to_string();
    let route_acl = required_acl_for_route(request.method(), &path);
    let api_profile = request
        .extensions()
        .get::<ApiProfile>()
        .copied()
        .unwrap_or_default();

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

    let customer_path_is_visible = match api_profile {
        ApiProfile::PaidBetaV1 => path == PAID_BETA_V1_DIRECT_SEARCH_PATH,
        ApiProfile::PaidBetaV3 => {
            request.method() == Method::POST && is_paid_beta_v3_customer_path(&path)
        }
        ApiProfile::Full => true,
    };
    let credential_error = || {
        log_auth_failure(&path, "malformed", "credential_sources_disagree");
        if customer_path_is_visible {
            invalid_api_credentials_error()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    };
    let application_id_credential =
        canonical_request_credential(&request, "x-algolia-application-id")
            .map_err(|()| credential_error())?;
    if application_id_credential
        .as_ref()
        .is_some_and(|presented| presented.from_query && requires_header_credentials(route_acl))
    {
        return Err(credential_error());
    }
    let application_id_opt = application_id_credential.map(|credential| credential.value);
    let standard_api_key_credential =
        extract_api_key_for_route(&request, route_acl).map_err(|()| credential_error())?;
    let standard_api_key = standard_api_key_credential.map(|credential| credential.value);
    let session_is_valid = request
        .extensions()
        .get::<std::sync::Arc<DashboardSessionStore>>()
        .zip(presented_session_token(&request))
        .is_some_and(|(store, token)| store.validate_session(&token));

    if !customer_path_is_visible {
        let admin_request = standard_api_key
            .as_deref()
            .is_some_and(|key| key_store.is_admin(key));
        let peer_request = standard_api_key.as_deref().is_some_and(|key| {
            key_store.lookup(key).is_none()
                && application_id_opt.as_deref().is_some_and(|application_id| {
                    is_configured_replication_peer_request(&request, route_acl, application_id, key)
                })
        });
        let unauthenticated_session_exchange = request.method() == Method::POST
            && path == "/1/dashboard/session"
            && standard_api_key.is_none()
            && request.headers().get(header::AUTHORIZATION).is_none();
        if !admin_request && !peer_request && !session_is_valid && !unauthenticated_session_exchange
        {
            return Err(StatusCode::NOT_FOUND.into_response());
        }
    }

    if request.method() == Method::OPTIONS {
        return Ok(next.run(request).await);
    }

    if route_acl == RouteAcl::Public {
        if request.method() == Method::POST && path == "/1/dashboard/session" {
            if let Some(response) = ensure_dashboard_session_exchange_rate_limit(&request) {
                return Err(response);
            }
        }
        return Ok(next.run(request).await);
    }

    let pbv1_customer_route = api_profile == ApiProfile::PaidBetaV1
        && path == PAID_BETA_V1_DIRECT_SEARCH_PATH
        && !session_is_valid
        && !standard_api_key
            .as_deref()
            .is_some_and(|key| key_store.is_admin(key));
    let pbv3_customer_route = api_profile == ApiProfile::PaidBetaV3
        && request.method() == Method::POST
        && is_paid_beta_v3_customer_path(&path)
        && !session_is_valid
        && !standard_api_key
            .as_deref()
            .is_some_and(|key| key_store.is_admin(key));
    let presented_key = if pbv1_customer_route {
        extract_bearer_api_key(&request)
    } else {
        standard_api_key
    };
    let (api_key_value, authenticated_by_session) = match presented_key {
        Some(key_value) => (key_value, false),
        None => {
            if session_is_valid {
                (key_store.admin_key_value(), true)
            } else {
                log_auth_failure(&path, "missing", "api_key_missing");
                return Err(invalid_api_credentials_error());
            }
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

    if (pbv1_customer_route || pbv3_customer_route) && application_id != PAID_BETA_V1_APPLICATION_ID
    {
        log_auth_failure(&path, "direct", "application_id_invalid");
        return Err(invalid_api_credentials_error());
    }

    if authenticated_by_session && application_id == REPLICATION_PEER_APPLICATION_ID {
        log_auth_failure(&path, "session", "peer_application_id_forbidden");
        return Err(invalid_api_credentials_error());
    }

    // Existing KeyStore identities take precedence over the separately
    // configured peer secret. Otherwise reusing a restricted API key as the
    // peer secret would silently promote every holder of that key to the peer
    // tier and bypass its ACL, expiry, source, and index restrictions.
    let authenticated_key = lookup_authenticated_key(&key_store, &api_key_value);
    if authenticated_key.is_none()
        && is_configured_replication_peer_request(
            &request,
            route_acl,
            &application_id,
            &api_key_value,
        )
    {
        let mut request = request;
        request
            .extensions_mut()
            .insert(AuthenticatedAppId(application_id));
        return Ok(next.run(request).await);
    }

    let (api_key, secured_restrictions) = match authenticated_key {
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
    if (pbv1_customer_route || pbv3_customer_route) && secured_restrictions.is_some() {
        return Err(invalid_api_credentials_error());
    }
    if let Some(response) = ensure_key_is_not_expired(&api_key) {
        return Err(response);
    }
    let authorization_acl = if pbv1_customer_route || pbv3_customer_route {
        RouteAcl::Required("search")
    } else {
        route_acl
    };
    if let Some(response) = ensure_route_acl_allows_request_for_acl(
        &key_store,
        &api_key,
        &api_key_value,
        authorization_acl,
        request.method(),
        &path,
    ) {
        return Err(response);
    }
    if pbv3_customer_route {
        let exact_managed_search_scope = api_key.acl.len() == 2
            && api_key.acl.iter().any(|acl| acl == "search")
            && api_key.acl.iter().any(|acl| acl == "browse")
            && api_key.indexes.len() == 1;
        if !exact_managed_search_scope {
            return Err(invalid_api_credentials_error());
        }
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

    let successful_admin_target = (key_store.is_admin(&api_key_value)
        && matches!(
            route_acl,
            RouteAcl::Required("admin") | RouteAcl::PeerOrAdmin
        ))
    .then(|| Target::route_pattern(AuditPath::for_auth_route(&path)));
    if let Some(target) = successful_admin_target {
        security_audit::emit_auth_success(Actor::admin_api_key(), target);
    }

    let mut request = request;
    // Store auth context for downstream handlers (search and dictionaries rely on these values).
    request
        .extensions_mut()
        .insert(AuthenticatedAppId(application_id));
    request.extensions_mut().insert(api_key);
    if pbv1_customer_route {
        request.extensions_mut().insert(PaidBetaV1CustomerRequest);
    }
    if pbv3_customer_route {
        request.extensions_mut().insert(PaidBetaV3CustomerRequest);
    }
    if let Some(restrictions) = secured_restrictions {
        request.extensions_mut().insert(restrictions);
    }

    Ok(next.run(request).await)
}

// Stage 1 boundary contract: peer-vs-admin enforcement through the real
// middleware, using the existing KeyStore and request-extension seams.
#[cfg(test)]
#[path = "../auth_tests/peer_boundary_middleware_tests.rs"]
mod peer_boundary_middleware_tests;
