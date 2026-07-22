mod key_store;
mod middleware;
mod route_acl;

pub use key_store::*;
pub use middleware::authenticate_and_authorize;
#[cfg(test)]
pub(crate) use middleware::{extract_index_name, is_public_path, route_exposure, RouteExposure};
pub(crate) use route_acl::is_acme_challenge_path;
pub use route_acl::required_acl_for_route;

#[cfg(test)]
use axum::http::Method;
use axum::{extract::Request, http::StatusCode, response::Response};
use dashmap::DashMap;
use flapjack::error::FlapjackError;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use crate::error_response::json_error;

pub(crate) const INVALID_API_CREDENTIALS_MESSAGE: &str = "Invalid Application-ID or API key";

pub(super) fn invalid_api_credentials_error() -> Response {
    json_error(StatusCode::FORBIDDEN, INVALID_API_CREDENTIALS_MESSAGE)
}

pub(crate) fn invalid_api_credentials_flapjack_error() -> FlapjackError {
    FlapjackError::Forbidden(INVALID_API_CREDENTIALS_MESSAGE.to_string())
}

/// Centralized index-authorization check used by both auth middleware and the
/// batch-search handler, which must validate `requests[].indexName` after
/// middleware auth has already succeeded on the wildcard batch route.
pub(crate) fn key_allows_index(
    api_key: &ApiKey,
    secured_restrictions: Option<&SecuredKeyRestrictions>,
    index_name: &str,
) -> bool {
    if !index_pattern_matches(&api_key.indexes, index_name) {
        return false;
    }

    if let Some(restrict_indices) =
        secured_restrictions.and_then(|restrictions| restrictions.restrict_indices.as_ref())
    {
        if !index_pattern_matches(restrict_indices, index_name) {
            return false;
        }
    }

    true
}

fn split_csv_trimmed(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|segment| segment.trim().to_string())
        .collect()
}

/// Convert epoch milliseconds to RFC-3339 string with fallback to current time.
/// Used for consistent timestamp formatting across key DTO responses.
pub fn epoch_millis_to_rfc3339(epoch_millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(epoch_millis)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| Utc::now().to_rfc3339())
}

/// Stored API key with salted SHA-256 hash, ACL permissions, index restrictions, rate-limit settings, and optional HMAC key for secured-key generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    /// SHA-256 hash of the key value (for authentication)
    pub hash: String,
    /// Unique salt for this key (hex-encoded)
    pub salt: String,
    /// HMAC verification key (for secured API key validation)
    /// NOTE: Held as plaintext in memory for HMAC verification and persisted
    /// encrypted at rest by the key-store material sidecar.
    /// Secured keys require parent-key material for HMAC validation.
    /// Admin keys should not be used as parents for secured keys and won't have this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hmac_key: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub acl: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub indexes: Vec<String>,
    #[serde(default, rename = "maxHitsPerQuery")]
    pub max_hits_per_query: i64,
    #[serde(default, rename = "maxQueriesPerIPPerHour")]
    pub max_queries_per_ip_per_hour: i64,
    #[serde(default, rename = "queryParameters")]
    pub query_parameters: String,
    #[serde(default)]
    pub referers: Vec<String>,
    #[serde(
        default,
        rename = "restrictSources",
        skip_serializing_if = "Option::is_none"
    )]
    pub restrict_sources: Option<Vec<String>>,
    #[serde(default)]
    pub validity: i64,
}

/// Algolia-compatible API response DTO for key GET/list endpoints.
/// Never exposes internal fields (hash, salt, hmac_key).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct KeyApiResponse {
    pub value: String,
    /// Epoch timestamp in milliseconds.
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub acl: Vec<String>,
    pub description: String,
    pub indexes: Vec<String>,
    #[serde(rename = "maxHitsPerQuery")]
    pub max_hits_per_query: i64,
    #[serde(rename = "maxQueriesPerIPPerHour")]
    pub max_queries_per_ip_per_hour: i64,
    #[serde(rename = "queryParameters")]
    pub query_parameters: String,
    pub referers: Vec<String>,
    #[serde(rename = "restrictSources", skip_serializing_if = "Option::is_none")]
    pub restrict_sources: Option<Vec<String>>,
    pub validity: i64,
}

impl KeyApiResponse {
    /// Map an internal ApiKey + its plaintext value to a safe API response DTO.
    pub fn from_api_key(key: &ApiKey, value: String) -> Self {
        Self {
            value,
            created_at: key.created_at,
            acl: key.acl.clone(),
            description: key.description.clone(),
            indexes: key.indexes.clone(),
            max_hits_per_query: key.max_hits_per_query,
            max_queries_per_ip_per_hour: key.max_queries_per_ip_per_hour,
            query_parameters: key.query_parameters.clone(),
            referers: key.referers.clone(),
            restrict_sources: key.restrict_sources.clone(),
            validity: key.validity,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedAppId(pub String);

pub fn request_application_id(request: &Request) -> Option<String> {
    request
        .headers()
        .get("x-algolia-application-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Canonical set of valid Algolia ACL values. Algolia has NO `admin` ACL.
pub const VALID_ACLS: &[&str] = &[
    "search",
    "browse",
    "addObject",
    "deleteObject",
    "deleteIndex",
    "settings",
    "editSettings",
    "analytics",
    "recommendation",
    "usage",
    "logs",
    "listIndexes",
    "seeUnretrievableAttributes",
    "inference",
    "personalization",
];

/// Validate that all ACL strings are in the canonical set.
/// Returns Err with the first invalid ACL string if validation fails.
pub fn validate_acls(acls: &[String]) -> Result<(), String> {
    for acl in acls {
        if !VALID_ACLS.contains(&acl.as_str()) {
            return Err(acl.clone());
        }
    }
    Ok(())
}

/// Per-key, per-IP rate limiter for `maxQueriesPerIPPerHour`.
/// Key: (key_hash, ip_addr), Value: (request_count, window_start).
#[derive(Clone)]
pub struct RateLimiter {
    counters: Arc<DashMap<(String, IpAddr), (u64, Instant)>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            counters: Arc::new(DashMap::new()),
        }
    }

    /// Check and increment the rate counter. Returns true if the request is allowed.
    pub fn check_and_increment(&self, key_hash: &str, ip: IpAddr, max_per_hour: u64) -> bool {
        let key = (key_hash.to_string(), ip);
        let mut entry = self.counters.entry(key).or_insert((0, Instant::now()));
        let (count, window_start) = entry.value_mut();

        // Reset window if more than 1 hour has elapsed
        if window_start.elapsed() >= std::time::Duration::from_secs(3600) {
            *count = 0;
            *window_start = Instant::now();
        }

        if *count >= max_per_hour {
            return false;
        }
        *count += 1;
        true
    }
}

/// Check if a referer matches any pattern in the allowlist.
/// Supports glob-style patterns: `*.example.com`, `https://shop.example.com/*`.
pub fn referer_matches(referer: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let referer_lower = referer.to_lowercase();
    let referer_host = referer_host(&referer_lower);
    patterns.iter().any(|pattern| {
        let pat = pattern.to_lowercase();
        referer_pattern_matches(&pat, &referer_lower, referer_host)
    })
}

fn parse_source_network(source: &str) -> Option<ipnet::IpNet> {
    source
        .parse::<ipnet::IpNet>()
        .or_else(|_| source.parse::<IpAddr>().map(ipnet::IpNet::from))
        .ok()
}

enum ApiKeyRestrictSource<'a> {
    Network(ipnet::IpNet),
    RefererPattern(&'a str),
}

/// Validates a referer pattern for use in API key `restrictSources`.
/// Accepts `*`, simple leading/trailing globs with a valid hostname or IP, and http(s) URLs.
fn is_valid_api_key_referer_pattern(pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern == "*" {
        return true;
    }

    let core = pattern.trim_start_matches('*').trim_end_matches('*');
    if core.is_empty() || core.contains('*') || core.chars().any(char::is_whitespace) {
        return false;
    }

    if core.contains("://") && !(core.starts_with("http://") || core.starts_with("https://")) {
        return false;
    }

    let Some(host) = referer_host(core) else {
        return false;
    };
    let host = host.trim_start_matches('.');

    !host.is_empty()
        && (host.eq_ignore_ascii_case("localhost")
            || host.contains('.')
            || host.parse::<IpAddr>().is_ok())
}

/// Parses a restrict-source entry as either a CIDR/IP network or a referer glob pattern.
/// Returns `Err` if the entry is neither a valid network address nor a valid referer pattern.
fn parse_api_key_restrict_source(source: &str) -> Result<Option<ApiKeyRestrictSource<'_>>, String> {
    let source = source.trim();
    if source.is_empty() {
        return Ok(None);
    }

    if let Some(network) = parse_source_network(source) {
        return Ok(Some(ApiKeyRestrictSource::Network(network)));
    }

    if is_valid_api_key_referer_pattern(source) {
        return Ok(Some(ApiKeyRestrictSource::RefererPattern(source)));
    }

    Err(source.to_string())
}

/// Extracts the hostname from a URL or referer string, stripping scheme, port, path,
/// query, and fragment. Handles IPv6 bracket notation (`[::1]:8080`).
fn referer_host(referer: &str) -> Option<&str> {
    // Strip scheme
    let after_scheme = referer
        .strip_prefix("https://")
        .or_else(|| referer.strip_prefix("http://"))
        .unwrap_or(referer);
    // Take the authority without any path/query/fragment.
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);

    if authority.is_empty() {
        return None;
    }

    if let Some(bracketed) = authority.strip_prefix('[') {
        return bracketed.split_once(']').map(|(host, _)| host);
    }

    match authority.rsplit_once(':') {
        Some((host, port))
            if authority.matches(':').count() == 1
                && !host.is_empty()
                && !port.is_empty()
                && port.chars().all(|ch| ch.is_ascii_digit()) =>
        {
            Some(host)
        }
        _ => Some(authority),
    }
}

/// Match a value against a simple glob pattern supporting leading `*`, trailing `*`, both (`*inner*`), or exact equality.
fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.starts_with('*') && pattern.ends_with('*') && pattern.len() > 2 {
        let inner = &pattern[1..pattern.len() - 1];
        return value.contains(inner);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return value.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    pattern == value
}

fn referer_pattern_matches(pattern: &str, referer_lower: &str, referer_host: Option<&str>) -> bool {
    if pattern == "*" {
        return true;
    }

    glob_match(pattern, referer_lower) || referer_host.is_some_and(|host| glob_match(pattern, host))
}

#[derive(Debug, Clone, Default)]
pub struct SecuredKeyRestrictions {
    pub filters: Option<String>,
    pub valid_until: Option<i64>,
    pub restrict_indices: Option<Vec<String>>,
    pub user_token: Option<String>,
    pub hits_per_page: Option<usize>,
    pub restrict_sources: Option<String>,
}

impl SecuredKeyRestrictions {
    /// Parse a URL-encoded query string into restriction fields.
    ///
    /// `restrictIndices` accepts both JSON array and comma-separated formats. Unknown parameters are silently ignored.
    fn from_params(params: &str) -> Self {
        let mut filters = None;
        let mut valid_until = None;
        let mut restrict_indices = None;
        let mut user_token = None;
        let mut hits_per_page = None;
        let mut restrict_sources = None;

        for (key, value) in url::form_urlencoded::parse(params.as_bytes()) {
            match key.as_ref() {
                "filters" => filters = Some(value.into_owned()),
                "validUntil" => valid_until = value.parse().ok(),
                "restrictIndices" => {
                    if let Ok(v) = serde_json::from_str::<Vec<String>>(&value) {
                        restrict_indices = Some(v);
                    } else {
                        restrict_indices = Some(split_csv_trimmed(value.as_ref()));
                    }
                }
                "userToken" => user_token = Some(value.into_owned()),
                "hitsPerPage" => hits_per_page = value.parse().ok(),
                "restrictSources" => restrict_sources = Some(value.into_owned()),
                _ => {}
            }
        }

        SecuredKeyRestrictions {
            filters,
            valid_until,
            restrict_indices,
            user_token,
            hits_per_page,
            restrict_sources,
        }
    }
}

pub fn generate_secured_api_key(parent_key: &str, params: &str) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(parent_key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(params.as_bytes());
    let hmac_hex = hex::encode(mac.finalize().into_bytes());
    let combined = format!("{}{}", hmac_hex, params);
    BASE64.encode(combined.as_bytes())
}

/// Decode a Base64-encoded secured API key, verify its HMAC against all keys in the store that have an `hmac_key`, and enforce `validUntil` expiry.
///
/// # Returns
///
/// `Some((parent_key, restrictions))` if the HMAC is valid and the key has not expired, `None` otherwise.
pub fn validate_secured_key(
    encoded: &str,
    key_store: &KeyStore,
) -> Option<(ApiKey, SecuredKeyRestrictions)> {
    let decoded = BASE64.decode(encoded.as_bytes()).ok()?;
    let decoded_str = String::from_utf8(decoded).ok()?;

    if decoded_str.len() < 64 {
        return None;
    }

    let hmac_bytes = hex::decode(&decoded_str[..64]).ok()?;
    let params = &decoded_str[64..];

    let data = key_store.data.read().unwrap();
    for key in &data.keys {
        let Some(hmac_key_value) = key.hmac_key.as_deref() else {
            continue;
        };

        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(hmac_key_value.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(params.as_bytes());
        if mac.verify_slice(&hmac_bytes).is_err() {
            continue;
        }

        let restrictions = SecuredKeyRestrictions::from_params(params);
        if restrictions
            .valid_until
            .is_some_and(|valid_until| Utc::now().timestamp() > valid_until)
        {
            return None;
        }

        return Some((key.clone(), restrictions));
    }
    None
}

/// Check whether `index_name` matches any pattern in `patterns`.
///
/// Supports exact matches, `*` (match all), prefix globs (`prod_*`), suffix globs (`*_prod`), and contains globs (`*prod*`). An empty pattern list matches everything.
pub fn index_pattern_matches(patterns: &[String], index_name: &str) -> bool {
    if patterns.is_empty() {
        return true;
    }
    patterns
        .iter()
        .any(|pattern| glob_match(pattern.as_str(), index_name))
}

#[cfg(test)]
fn matches_restrict_sources(sources: &[String], ip: IpAddr) -> bool {
    matches_restrict_sources_iter(sources.iter().map(String::as_str), ip)
}

pub fn validate_restrict_sources_entries(sources: &[String]) -> Result<(), String> {
    validate_api_key_restrict_sources_iter(sources.iter().map(String::as_str))
}

pub fn validate_restrict_sources_csv(restrict_sources: &str) -> Result<(), String> {
    validate_restrict_sources_iter(restrict_sources.split(','))
}

fn validate_api_key_restrict_sources_iter<'a>(
    sources: impl Iterator<Item = &'a str>,
) -> Result<(), String> {
    for source in sources {
        parse_api_key_restrict_source(source)?;
    }
    Ok(())
}

fn validate_restrict_sources_iter<'a>(
    sources: impl Iterator<Item = &'a str>,
) -> Result<(), String> {
    for source in sources.map(str::trim) {
        if source.is_empty() {
            continue;
        }
        if parse_source_network(source).is_none() {
            return Err(source.to_string());
        }
    }
    Ok(())
}

/// Tests whether a client IP matches any CIDR/IP entry in a restrict-sources iterator.
/// Returns `true` if the iterator is empty (no restrictions) or contains a matching network.
fn matches_restrict_sources_iter<'a>(sources: impl Iterator<Item = &'a str>, ip: IpAddr) -> bool {
    let mut saw_source = false;
    let mut saw_match = false;

    for source in sources.map(str::trim) {
        if source.is_empty() {
            continue;
        }

        saw_source = true;
        let Some(network) = parse_source_network(source) else {
            return false;
        };
        if network.contains(&ip) {
            saw_match = true;
        }
    }

    !saw_source || saw_match
}

pub(super) fn restrict_sources_match(restrict_sources: &str, ip: IpAddr) -> bool {
    matches_restrict_sources_iter(restrict_sources.split(','), ip)
}

/// Checks whether a client IP or referer matches any entry in an API key's
/// `restrictSources` list, which may contain both CIDR networks and referer glob patterns.
pub(super) fn api_key_restrict_sources_match(
    entries: &[String],
    client_ip: Option<IpAddr>,
    referer: &str,
) -> bool {
    if entries.is_empty() {
        return true;
    }

    let referer_lower = referer.to_lowercase();
    let referer_host = referer_host(&referer_lower);
    let mut saw_source = false;

    for entry in entries {
        let source = match parse_api_key_restrict_source(entry) {
            Ok(Some(source)) => source,
            Ok(None) => continue,
            Err(_) => return false,
        };
        saw_source = true;

        match source {
            ApiKeyRestrictSource::Network(network) => {
                if client_ip.is_some_and(|ip| network.contains(&ip)) {
                    return true;
                }
            }
            ApiKeyRestrictSource::RefererPattern(pattern) => {
                let pattern = pattern.to_lowercase();
                if referer_pattern_matches(&pattern, &referer_lower, referer_host) {
                    return true;
                }
            }
        }
    }

    !saw_source
}

#[cfg(test)]
#[path = "../auth_tests.rs"]
mod tests;
