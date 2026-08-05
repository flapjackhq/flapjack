//! Dashboard session exchange and logout handlers.

use std::sync::Arc;

use axum::extract::{Extension, Json, Request};
use axum::http::{header::SET_COOKIE, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::auth::session::DashboardSessionStore;
use crate::auth::session_cookie::{
    clear_session_cookie_header, presented_session_token, session_cookie_header,
};
use crate::auth::{invalid_api_credentials_error, KeyStore};
use crate::error_response::json_error;
use crate::middleware::TransportSecurity;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionExchangeRequest {
    api_key: String,
}

#[derive(Serialize)]
struct SessionExchangeResponse {
    authenticated: bool,
}

pub(crate) async fn exchange_dashboard_session(
    Extension(key_store): Extension<Arc<KeyStore>>,
    Extension(session_store): Extension<Arc<DashboardSessionStore>>,
    transport: TransportSecurity,
    Json(payload): Json<SessionExchangeRequest>,
) -> Response {
    if !key_store.is_admin(&payload.api_key) {
        return invalid_api_credentials_error();
    }

    let token = match session_store.mint_session() {
        Ok(token) => token,
        Err(error) => {
            tracing::error!(%error, "Failed to persist dashboard session");
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
        }
    };
    let mut response = axum::Json(SessionExchangeResponse {
        authenticated: true,
    })
    .into_response();
    let cookie = session_cookie_header(transport, &token);
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("session token is ASCII"),
    );
    response
}

pub(crate) async fn logout_dashboard_session(
    Extension(session_store): Extension<Arc<DashboardSessionStore>>,
    transport: TransportSecurity,
    request: Request,
) -> Response {
    let token = presented_session_token(&request);
    if let Some(token) = token {
        if let Err(error) = session_store.revoke_session(&token) {
            tracing::error!(%error, "Failed to revoke dashboard session");
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
        }
    }

    let cookie = clear_session_cookie_header(transport);
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("session cookie attributes are ASCII"),
    );
    response
}
