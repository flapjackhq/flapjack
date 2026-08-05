//! Cookie transport encoding for dashboard sessions.

use axum::extract::Request;
use axum::http::header::COOKIE;

use crate::middleware::TransportSecurity;

pub(crate) const DASHBOARD_SESSION_COOKIE_NAME: &str = "flapjack_dashboard_session";

pub(crate) fn session_cookie_header(transport: TransportSecurity, token: &str) -> String {
    cookie_header(transport, token, false)
}

pub(crate) fn clear_session_cookie_header(transport: TransportSecurity) -> String {
    cookie_header(transport, "", true)
}

fn cookie_header(transport: TransportSecurity, token: &str, clear: bool) -> String {
    let mut attributes = vec![
        format!("{DASHBOARD_SESSION_COOKIE_NAME}={token}"),
        // The whole point of moving off localStorage: script running in the
        // dashboard origin — including anything injected into it — must not be
        // able to read the session token back out.
        "HttpOnly".to_string(),
        // A cross-site request must not be able to ride the operator's session.
        "SameSite=Strict".to_string(),
        // The dashboard calls API routes outside /dashboard, so the cookie must
        // cover the whole origin.
        "Path=/".to_string(),
    ];
    if clear {
        attributes.push("Max-Age=0".to_string());
    }
    // Flapjack serves plaintext HTTP by default. An unconditional Secure flag
    // would silently break session login for local single-binary operators.
    if transport.is_secure() {
        attributes.push("Secure".to_string());
    }
    attributes.join("; ")
}

pub(crate) fn presented_session_token(request: &Request) -> Option<String> {
    request
        .headers()
        .get_all(COOKIE)
        .iter()
        .filter_map(|header| header.to_str().ok())
        .flat_map(|header| header.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find_map(|(name, value)| {
            (name.trim() == DASHBOARD_SESSION_COOKIE_NAME && !value.is_empty())
                .then(|| value.to_string())
        })
}
