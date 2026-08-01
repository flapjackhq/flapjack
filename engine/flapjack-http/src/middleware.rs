use axum::{
    extract::Request,
    http::{header::CONTENT_TYPE, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::Response,
};
use ipnet::IpNet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tracing::Instrument;

use crate::error_response::json_error;

pub const DEFAULT_TRUSTED_PROXY_CIDRS: &str = "127.0.0.0/8,::1/128";
pub const REQUEST_ID_HEADER_NAME: &str = "x-request-id";
pub const REQUEST_TIMEOUT_MESSAGE: &str = "Request timed out";

#[derive(Clone, Debug)]
pub struct RequestId(pub String);

fn canonical_request_id(headers: &HeaderMap) -> String {
    headers
        .get(REQUEST_ID_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

#[derive(Debug, Clone, Default)]
pub struct TrustedProxyMatcher {
    networks: Vec<IpNet>,
}

impl TrustedProxyMatcher {
    pub fn from_csv(raw: &str) -> Result<Self, String> {
        let mut networks = Vec::new();
        for entry in raw
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            let parsed = entry
                .parse::<IpNet>()
                .map_err(|_| format!("Invalid trusted proxy CIDR: {entry}"))?;
            networks.push(parsed.trunc());
        }
        Ok(Self { networks })
    }

    pub fn from_env_var(var_name: &str) -> Result<Self, String> {
        match std::env::var(var_name) {
            Ok(raw) => Self::from_optional_csv(Some(&raw)),
            Err(std::env::VarError::NotPresent) => Self::from_optional_csv(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("{var_name} must be valid UTF-8"))
            }
        }
    }

    pub fn from_optional_csv(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim).filter(|value| !value.is_empty()) {
            None => Self::from_csv(DEFAULT_TRUSTED_PROXY_CIDRS),
            Some(value)
                if value.eq_ignore_ascii_case("off") || value.eq_ignore_ascii_case("none") =>
            {
                Ok(Self::default())
            }
            Some(value) => Self::from_csv(value),
        }
    }

    pub fn is_trusted(&self, ip: IpAddr) -> bool {
        self.networks.iter().any(|network| network.contains(&ip))
    }

    pub fn len(&self) -> usize {
        self.networks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.networks.is_empty()
    }
}

fn parse_x_forwarded_for(headers: &HeaderMap) -> Vec<IpAddr> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|raw| raw.split(','))
        .filter_map(|item| item.trim().parse().ok())
        .collect()
}

fn parse_x_real_ip(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.trim().parse().ok())
}

/// Resolve the originating client IP from `X-Forwarded-For` or `X-Real-IP` headers.
///
/// Walks the XFF chain from rightmost (nearest) to leftmost (furthest) hop and returns the first IP not matched by `trusted_proxy_matcher`. If every hop is trusted, returns the leftmost entry. Falls back to `X-Real-IP`, then to `peer_ip`.
///
/// # Arguments
///
/// * `headers` - Request headers potentially containing `X-Forwarded-For` and `X-Real-IP`.
/// * `peer_ip` - The socket-level peer address (always trusted).
/// * `trusted_proxy_matcher` - CIDR matcher identifying known proxy addresses.
fn trusted_forwarded_client_ip(
    headers: &HeaderMap,
    peer_ip: IpAddr,
    trusted_proxy_matcher: &TrustedProxyMatcher,
) -> Option<IpAddr> {
    let forwarded_chain = parse_x_forwarded_for(headers);
    if !forwarded_chain.is_empty() {
        // Walk from nearest hop to furthest hop. The first untrusted hop is the
        // boundary client address; this avoids trusting spoofed left-most values.
        for candidate in forwarded_chain.iter().rev().copied() {
            if !trusted_proxy_matcher.is_trusted(candidate) {
                return Some(candidate);
            }
        }
        // If every hop is trusted, use the furthest forwarded value.
        return forwarded_chain.first().copied();
    }

    parse_x_real_ip(headers).or(Some(peer_ip))
}

/// Best-effort client IP extraction.
///
/// Trust model:
/// - Always trust socket peer (`ConnectInfo`).
/// - Trust `X-Forwarded-For`/`X-Real-IP` only when the socket peer is in
///   `TrustedProxyMatcher` attached to request extensions.
pub fn extract_client_ip_opt(request: &Request) -> Option<IpAddr> {
    let peer_ip = request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    let trusted_proxy_matcher = request.extensions().get::<Arc<TrustedProxyMatcher>>();

    match (peer_ip, trusted_proxy_matcher) {
        (Some(peer_ip), Some(matcher)) if matcher.is_trusted(peer_ip) => {
            trusted_forwarded_client_ip(request.headers(), peer_ip, matcher.as_ref())
        }
        (Some(peer_ip), _) => Some(peer_ip),
        _ => None,
    }
}

/// Extract client IP with precedence:
/// trusted `X-Forwarded-For` -> trusted `X-Real-IP` -> socket peer (`ConnectInfo`) -> `127.0.0.1`.
pub fn extract_client_ip(request: &Request) -> IpAddr {
    extract_client_ip_opt(request).unwrap_or_else(|| "127.0.0.1".parse().unwrap())
}

/// Extract client IP for rate limiting purposes.
///
/// Uses the same resolution as `extract_client_ip`: when the peer is a
/// trusted proxy, resolves the rightmost untrusted XFF IP; otherwise uses
/// the peer IP directly.  XFF headers from untrusted peers are ignored to
/// prevent spoof-based rate-limit evasion.
pub fn extract_rate_limit_ip(request: &Request) -> IpAddr {
    extract_client_ip(request)
}

pub async fn normalize_content_type(mut request: Request, next: Next) -> Response {
    if request.method() == axum::http::Method::POST || request.method() == axum::http::Method::PUT {
        request
            .headers_mut()
            .insert(CONTENT_TYPE, "application/json".parse().unwrap());
    }
    next.run(request).await
}

/// Ensures all error responses (4xx, 5xx) are JSON `{ "message": "...", "status": N }`.
/// Catches Axum's built-in plain-text rejections (e.g. JSON parse failures) and wraps
/// them in the Algolia-compatible error format.
pub async fn ensure_json_errors(request: Request, next: Next) -> Response {
    let response = next.run(request).await;

    if !response.status().is_client_error() && !response.status().is_server_error() {
        return response;
    }

    let is_json = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("application/json"));

    if is_json {
        return response;
    }

    let status = response.status();
    let body_bytes = axum::body::to_bytes(response.into_body(), 10_000)
        .await
        .unwrap_or_default();
    let message =
        String::from_utf8(body_bytes.to_vec()).unwrap_or_else(|_| "Unknown error".to_string());
    let message = if status == StatusCode::REQUEST_TIMEOUT && message.is_empty() {
        REQUEST_TIMEOUT_MESSAGE.to_string()
    } else {
        message
    };

    json_error(status, message)
}

/// Chrome 142+ Private Network Access: when a public HTTPS site fetches localhost,
/// the preflight includes `Access-Control-Request-Private-Network: true`.
/// The server must respond with `Access-Control-Allow-Private-Network: true`.
pub async fn allow_private_network(request: Request, next: Next) -> Response {
    let needs_pna = request
        .headers()
        .get("access-control-request-private-network")
        .is_some();
    let mut response = next.run(request).await;
    if needs_pna {
        response.headers_mut().insert(
            "access-control-allow-private-network",
            "true".parse().unwrap(),
        );
    }
    response
}

pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let request_id = canonical_request_id(request.headers());
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let span = tracing::info_span!("http_request", request_id = tracing::field::Empty);
    span.record("request_id", tracing::field::display(&request_id));
    let mut response = next.run(request).instrument(span).await;
    response.headers_mut().insert(
        REQUEST_ID_HEADER_NAME,
        HeaderValue::from_str(&request_id).expect("request ID should be valid header value"),
    );
    response
}

#[cfg(test)]
#[path = "middleware_tests.rs"]
mod tests;
