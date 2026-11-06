//! Request counting middleware for per-index usage metrics, tracking search, write, and read counts plus bytes ingested per index name.

use axum::{extract::Request, http::Method, middleware::Next, response::Response};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Per-index usage counters. All fields are atomically updated so the
/// struct can be shared across request handlers without locking.
pub struct TenantUsageCounters {
    pub search_count: AtomicU64,
    pub write_count: AtomicU64,
    pub read_count: AtomicU64,
    pub bytes_in: AtomicU64,
    pub search_results_total: AtomicU64,
    pub documents_indexed_total: AtomicU64,
    pub documents_deleted_total: AtomicU64,
}

impl TenantUsageCounters {
    pub fn new() -> Self {
        Self {
            search_count: AtomicU64::new(0),
            write_count: AtomicU64::new(0),
            read_count: AtomicU64::new(0),
            bytes_in: AtomicU64::new(0),
            search_results_total: AtomicU64::new(0),
            documents_indexed_total: AtomicU64::new(0),
            documents_deleted_total: AtomicU64::new(0),
        }
    }
}

impl Default for TenantUsageCounters {
    fn default() -> Self {
        Self::new()
    }
}

/// Classification of an index request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Search,
    Write,
    Read,
}

/// Extract the index name from a `/1/indexes/:indexName/...` URL path.
///
/// Returns `None` for paths that don't match the index pattern.
pub fn extract_index_name(path: &str) -> Option<String> {
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    if segments.next()? != "1" {
        return None;
    }
    if segments.next()? != "indexes" {
        return None;
    }
    let name = segments.next()?;
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// Classify a request as Search, Write, or Read based on HTTP method and
/// the path segment after the index name.
pub fn classify_request(method: &Method, path: &str) -> Option<RequestKind> {
    let index_name = extract_index_name(path)?;
    let suffix = path
        .strip_prefix(&format!("/1/indexes/{}", index_name))
        .unwrap_or("");
    let suffix = suffix.strip_prefix('/').unwrap_or(suffix);
    let first_segment = suffix.split('/').next().unwrap_or("");

    match (method, first_segment) {
        // Search operations
        (&Method::POST, "query") => Some(RequestKind::Search),
        (&Method::POST, "facets") if suffix.ends_with("/query") => Some(RequestKind::Search),

        // Read operations
        (&Method::POST, "objects") | (&Method::POST, "browse") => Some(RequestKind::Read),
        (&Method::GET, seg) if !seg.is_empty() => Some(RequestKind::Read),

        // Write operations
        (&Method::POST, "batch") | (&Method::POST, "deleteByQuery") => Some(RequestKind::Write),
        (&Method::PUT, _) | (&Method::DELETE, _) => Some(RequestKind::Write),
        // POST to index root (add_record_auto_id)
        (&Method::POST, "") => Some(RequestKind::Write),

        _ => None,
    }
}

/// Axum middleware that counts requests per index.
pub async fn usage_counting_layer(
    request: Request,
    next: Next,
    counters: &Arc<DashMap<String, TenantUsageCounters>>,
) -> Response {
    let path = request.uri().path().to_string();
    let method = request.method().clone();

    if let Some(index_name) = extract_index_name(&path) {
        // Batch paths are envelopes; the handler counts executed physical-index
        // operands. Do not create an envelope entry or attribute its bytes.
        if method == Method::POST && path == format!("/1/indexes/{index_name}/queries") {
            return next.run(request).await;
        }

        let content_length: u64 = request
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let entry = counters.entry(index_name).or_default();

        if content_length > 0 {
            entry.bytes_in.fetch_add(content_length, Ordering::Relaxed);
        }

        if let Some(kind) = classify_request(&method, &path) {
            match kind {
                RequestKind::Search => {
                    entry.search_count.fetch_add(1, Ordering::Relaxed);
                }
                RequestKind::Write => {
                    entry.write_count.fetch_add(1, Ordering::Relaxed);
                }
                RequestKind::Read => {
                    entry.read_count.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    next.run(request).await
}

#[cfg(test)]
#[path = "usage_middleware_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "usage_batch_tests.rs"]
mod batch_tests;
