//! Stub summary for single.rs.
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Json,
};
use std::sync::Arc;

use flapjack::error::FlapjackError;

use crate::dto::SearchRequest;
use crate::handlers::AppState;

use super::request::{
    apply_key_restrictions, can_see_unretrievable_attributes, compute_hits_cap,
    extract_analytics_headers, merge_secured_filters,
};
pub(super) use super::single_execution::search_single_with_secured_hits_per_page_cap;

pub async fn search_single(
    State(state): State<Arc<AppState>>,
    index_name: String,
    req: SearchRequest,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    search_single_with_secured_hits_per_page_cap(State(state), index_name, req, None, false, None)
        .await
}

/// Shared auth/analytics extraction from an incoming request.
struct ExtractedRequestContext {
    secured_restrictions: Option<crate::auth::SecuredKeyRestrictions>,
    api_key: Option<crate::auth::ApiKey>,
    dictionary_lookup_tenant: Option<String>,
    user_token_header: Option<String>,
    user_ip: Option<String>,
    session_id_header: Option<String>,
}

impl ExtractedRequestContext {
    /// TODO: Document ExtractedRequestContext.from_request.
    fn from_request(request: &axum::extract::Request) -> Self {
        let secured_restrictions = request
            .extensions()
            .get::<crate::auth::SecuredKeyRestrictions>()
            .cloned();
        let api_key = request.extensions().get::<crate::auth::ApiKey>().cloned();
        let dictionary_lookup_tenant = request
            .extensions()
            .get::<crate::auth::AuthenticatedAppId>()
            .map(|id| id.0.clone());
        let (user_token_header, user_ip, session_id_header) = extract_analytics_headers(request);
        Self {
            secured_restrictions,
            api_key,
            dictionary_lookup_tenant,
            user_token_header,
            user_ip,
            session_id_header,
        }
    }

    /// Apply auth restrictions and analytics headers to the search request.
    fn apply_to(&self, req: &mut SearchRequest) {
        if let Some(ref restrictions) = self.secured_restrictions {
            merge_secured_filters(req, restrictions);
        }
        apply_key_restrictions(req, &self.api_key);
        if req.user_token.is_none() {
            req.user_token = self.user_token_header.clone();
        }
        if req.session_id.is_none() {
            req.session_id = self.session_id_header.clone();
        }
        req.user_ip = self.user_ip.clone();
    }

    fn hits_cap(&self) -> Option<usize> {
        compute_hits_cap(&self.api_key, &self.secured_restrictions)
    }

    fn can_see_unretrievable(&self) -> bool {
        can_see_unretrievable_attributes(&self.api_key)
    }
}

/// Search an index with full-text query and filters
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/query",
    tag = "search",
    params(
        ("indexName" = String, Path, description = "Index to search")
    ),
    request_body(content = SearchRequest, description = "Search parameters including query, filters, facets, and pagination"),
    responses(
        (status = 200, description = "Search results with hits and facets", body = crate::dto::SearchResponse),
        (status = 404, description = "Index not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn search(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, FlapjackError> {
    let ctx = ExtractedRequestContext::from_request(&request);
    let body_bytes = axum::body::to_bytes(request.into_body(), 10_000_000)
        .await
        .map_err(|e| FlapjackError::InvalidQuery(format!("Failed to read body: {}", e)))?;
    let mut req: SearchRequest = serde_json::from_slice(&body_bytes)
        .map_err(|e| FlapjackError::InvalidQuery(format!("Invalid JSON: {}", e)))?;
    req.apply_params_string();
    ctx.apply_to(&mut req);
    let Json(response) = search_single_with_secured_hits_per_page_cap(
        State(state),
        index_name,
        req,
        ctx.hits_cap(),
        ctx.can_see_unretrievable(),
        ctx.dictionary_lookup_tenant,
    )
    .await?;

    if let Some(qid) = response.get("queryID").and_then(|v| v.as_str()) {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-algolia-query-id", qid.parse().unwrap());
        Ok((headers, Json(response)).into_response())
    } else {
        Ok(Json(response).into_response())
    }
}

/// Search an index using query-string parameters on GET /1/indexes/{indexName}
#[utoipa::path(
    get,
    path = "/1/indexes/{indexName}",
    tag = "search",
    params(
        ("indexName" = String, Path, description = "Index to search")
    ),
    responses(
        (status = 200, description = "Search results with hits and facets", body = crate::dto::SearchResponse),
        (status = 404, description = "Index not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn search_get(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    let ctx = ExtractedRequestContext::from_request(&request);
    let raw_query = request.uri().query().unwrap_or("").to_string();

    let mut req = SearchRequest {
        params: Some(raw_query.clone()),
        ..Default::default()
    };
    req.apply_params_string();
    ctx.apply_to(&mut req);
    let mut response = search_single_with_secured_hits_per_page_cap(
        State(state),
        index_name,
        req,
        ctx.hits_cap(),
        ctx.can_see_unretrievable(),
        ctx.dictionary_lookup_tenant,
    )
    .await?
    .0;
    response["params"] = serde_json::Value::String(raw_query);
    Ok(Json(response))
}
