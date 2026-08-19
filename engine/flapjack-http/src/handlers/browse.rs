//! Stub summary for browse.rs.
use axum::{
    extract::{Path, State},
    Json,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::AppState;
use crate::dto::SearchRequest;
use crate::filter_parser::parse_filter;
use crate::handlers::search::build_params_echo;
use flapjack::error::FlapjackError;
use flapjack::index::SearchOptions;

use flapjack::types::field_value_to_json_value;

/// Deserialized body for the browse endpoint, supporting cursor-based pagination, optional query/filter parameters, and an Algolia-compatible `params` query-string override.
#[derive(Deserialize)]
pub struct BrowseRequest {
    #[serde(default)]
    pub cursor: Option<String>,

    #[serde(default)]
    pub query: String,

    #[serde(default)]
    pub filters: Option<String>,

    #[serde(default = "default_browse_hits_per_page")]
    #[serde(rename = "hitsPerPage")]
    pub hits_per_page: usize,

    #[serde(default, rename = "attributesToRetrieve")]
    pub attributes_to_retrieve: Option<Vec<String>>,

    #[serde(default)]
    pub params: Option<String>,
}

fn default_browse_hits_per_page() -> usize {
    1000
}

#[derive(Serialize, Deserialize)]
struct BrowseCursor {
    offset: usize,
    generation: u64,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    filters: Option<String>,
    #[serde(default, rename = "hitsPerPage")]
    hits_per_page: Option<usize>,
}

impl BrowseRequest {
    /// Merge the URL-encoded `params` query string into the top-level fields.
    ///
    /// Delegates to `SearchRequest::apply_params_string` so that key-value pairs inside `params` (e.g. `filters=...&hitsPerPage=50`) override the corresponding fields on this request. Consumes `self.params` in the process.
    fn apply_params_string(&mut self) {
        let mut req = SearchRequest {
            query: self.query.clone(),
            filters: self.filters.clone(),
            hits_per_page: Some(self.hits_per_page),
            attributes_to_retrieve: self.attributes_to_retrieve.clone(),
            params: self.params.take(),
            ..Default::default()
        };

        req.apply_params_string();

        self.query = req.query;
        self.filters = req.filters;
        self.hits_per_page = req.hits_per_page.unwrap_or(default_browse_hits_per_page());
        self.attributes_to_retrieve = req.attributes_to_retrieve;
    }
}

/// TODO: Document decode_and_validate_cursor.
fn decode_and_validate_cursor(
    cursor_str: &str,
    current_gen_hash: u64,
) -> Result<BrowseCursor, FlapjackError> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(cursor_str)
        .map_err(|_| FlapjackError::InvalidQuery("Invalid cursor".to_string()))?;
    let cursor_json = String::from_utf8(decoded)
        .map_err(|_| FlapjackError::InvalidQuery("Invalid cursor encoding".to_string()))?;
    let cursor: BrowseCursor = serde_json::from_str(&cursor_json)
        .map_err(|_| FlapjackError::InvalidQuery("Invalid cursor format".to_string()))?;

    if cursor.generation != current_gen_hash {
        return Err(FlapjackError::InvalidQuery(
            "Cursor is not valid anymore (index modified)".to_string(),
        ));
    }

    Ok(cursor)
}

/// TODO: Document resolve_browse_params.
fn resolve_browse_params(
    req: &BrowseRequest,
    cursor: Option<&BrowseCursor>,
) -> (String, Option<String>, usize) {
    let mut effective_query = req.query.clone();
    let mut effective_filters = req.filters.clone();
    let mut effective_hits_per_page = req.hits_per_page;

    if let Some(cursor) = cursor {
        effective_query = cursor.query.clone().unwrap_or_else(|| req.query.clone());
        // Cursor-driven pagination must keep the original filter context, including
        // "no filters", instead of allowing request-time overrides.
        effective_filters = cursor.filters.clone();
        effective_hits_per_page = cursor.hits_per_page.unwrap_or(req.hits_per_page);
    }

    (effective_query, effective_filters, effective_hits_per_page)
}

/// TODO: Document shape_browse_hits.
fn shape_browse_hits(
    documents: &[flapjack::types::ScoredDocument],
    attributes_to_retrieve: Option<&[String]>,
) -> Vec<serde_json::Value> {
    documents
        .iter()
        .map(|scored_doc| {
            let mut doc_map = serde_json::Map::new();
            doc_map.insert(
                "objectID".to_string(),
                serde_json::Value::String(scored_doc.document.id.clone()),
            );

            for (key, value) in &scored_doc.document.fields {
                if let Some(attrs) = attributes_to_retrieve {
                    if !attrs.contains(key) && !attrs.iter().any(|a| a == "*") {
                        continue;
                    }
                }
                doc_map.insert(key.clone(), field_value_to_json_value(value));
            }

            serde_json::Value::Object(doc_map)
        })
        .collect()
}

/// TODO: Document build_next_cursor.
fn build_next_cursor(
    next_offset: usize,
    total: usize,
    current_gen_hash: u64,
    query: &str,
    filters: Option<&str>,
    hits_per_page: usize,
) -> Option<String> {
    if next_offset >= total {
        return None;
    }

    let cursor = BrowseCursor {
        offset: next_offset,
        generation: current_gen_hash,
        query: Some(query.to_string()),
        filters: filters.map(ToString::to_string),
        hits_per_page: Some(hits_per_page),
    };
    let cursor_json = serde_json::to_string(&cursor).unwrap();
    Some(base64::engine::general_purpose::STANDARD.encode(cursor_json.as_bytes()))
}

/// Browse all documents in an index with cursor-based pagination.
///
/// Supports Algolia-compatible browse payloads, including `params` overrides,
/// stable cursor validation, filter parsing, and attribute projection for
/// large index scans that should not rely on ranked search pagination.
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/browse",
    tag = "documents",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    request_body(content = serde_json::Value, description = "Browse request with optional cursor"),
    responses(
        (status = 200, description = "Documents page with cursor", body = serde_json::Value),
        (status = 404, description = "Index not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn browse_index(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
    Json(mut req): Json<BrowseRequest>,
) -> Result<Json<serde_json::Value>, FlapjackError> {
    req.apply_params_string();

    let index = state.manager.get_or_load(&index_name)?;
    let reader = index.reader();
    let searcher = reader.searcher();
    let current_generation = searcher
        .segment_readers()
        .iter()
        .map(|sr| sr.segment_id().uuid_string())
        .collect::<Vec<_>>()
        .join("-");
    let current_gen_hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        current_generation.hash(&mut hasher);
        hasher.finish()
    };

    let cursor = if let Some(cursor_str) = req.cursor.as_deref() {
        Some(decode_and_validate_cursor(cursor_str, current_gen_hash)?)
    } else {
        None
    };

    let offset = cursor.as_ref().map(|c| c.offset).unwrap_or(0);
    let (effective_query, effective_filters, effective_hits_per_page) =
        resolve_browse_params(&req, cursor.as_ref());

    let filter = if let Some(filter_str) = &effective_filters {
        Some(
            parse_filter(filter_str)
                .map_err(|e| FlapjackError::InvalidQuery(format!("Filter parse error: {}", e)))?,
        )
    } else {
        None
    };

    let hits_per_page = effective_hits_per_page.min(1000);
    if hits_per_page == 0 {
        return Err(FlapjackError::InvalidQuery(
            "hitsPerPage must be between 1 and 1000".to_string(),
        ));
    }

    let result = state.manager.search_with_options(
        &index_name,
        &effective_query,
        &SearchOptions {
            filter: filter.as_ref(),
            limit: hits_per_page,
            offset,
            ..Default::default()
        },
    )?;
    // Browsing is cursor-based iteration across the full result set; paginationLimitedTo is
    // intentionally not enforced here (Algolia browse semantics expect all docs can be scanned).

    let total = result.total;
    let page_docs = &result.documents;

    let hits = shape_browse_hits(page_docs, req.attributes_to_retrieve.as_deref());

    let next_offset = offset + hits.len();
    let next_cursor = build_next_cursor(
        next_offset,
        total,
        current_gen_hash,
        &effective_query,
        effective_filters.as_deref(),
        hits_per_page,
    );

    let nb_pages = if total == 0 {
        0
    } else {
        total.div_ceil(hits_per_page)
    };
    let params_echo = build_params_echo(&SearchRequest {
        query: effective_query.clone(),
        filters: effective_filters.clone(),
        hits_per_page: Some(hits_per_page),
        attributes_to_retrieve: req.attributes_to_retrieve.clone(),
        ..Default::default()
    });

    let mut response = serde_json::Map::new();
    response.insert("hits".to_string(), serde_json::Value::Array(hits));
    response.insert("nbHits".to_string(), serde_json::json!(total));
    response.insert("page".to_string(), serde_json::json!(0));
    response.insert("nbPages".to_string(), serde_json::json!(nb_pages));
    response.insert("hitsPerPage".to_string(), serde_json::json!(hits_per_page));
    response.insert(
        "query".to_string(),
        serde_json::Value::String(effective_query),
    );
    response.insert("params".to_string(), serde_json::Value::String(params_echo));
    response.insert(
        "cursor".to_string(),
        match next_cursor {
            Some(cursor) => serde_json::Value::String(cursor),
            None => serde_json::Value::Null,
        },
    );

    Ok(Json(serde_json::Value::Object(response)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_cursor(cursor: BrowseCursor) -> String {
        let cursor_json = serde_json::to_string(&cursor).expect("cursor json");
        base64::engine::general_purpose::STANDARD.encode(cursor_json.as_bytes())
    }

    /// TODO: Document decode_and_validate_cursor_accepts_valid_cursor.
    #[test]
    fn decode_and_validate_cursor_accepts_valid_cursor() {
        let current_gen_hash = 777_u64;
        let encoded = encode_cursor(BrowseCursor {
            offset: 15,
            generation: current_gen_hash,
            query: Some("hello".to_string()),
            filters: Some("category:books".to_string()),
            hits_per_page: Some(33),
        });

        let parsed = decode_and_validate_cursor(&encoded, current_gen_hash).expect("valid cursor");
        assert_eq!(parsed.offset, 15);
        assert_eq!(parsed.generation, current_gen_hash);
        assert_eq!(parsed.query.as_deref(), Some("hello"));
        assert_eq!(parsed.filters.as_deref(), Some("category:books"));
        assert_eq!(parsed.hits_per_page, Some(33));
    }

    #[test]
    fn decode_and_validate_cursor_rejects_invalid_base64() {
        let err = match decode_and_validate_cursor("%%%not-base64%%%", 123_u64) {
            Ok(_) => panic!("expected invalid cursor"),
            Err(err) => err,
        };
        match err {
            FlapjackError::InvalidQuery(message) => assert_eq!(message, "Invalid cursor"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// TODO: Document decode_and_validate_cursor_rejects_mismatched_generation.
    #[test]
    fn decode_and_validate_cursor_rejects_mismatched_generation() {
        let encoded = encode_cursor(BrowseCursor {
            offset: 0,
            generation: 99,
            query: None,
            filters: None,
            hits_per_page: None,
        });
        let err = match decode_and_validate_cursor(&encoded, 100) {
            Ok(_) => panic!("expected stale cursor"),
            Err(err) => err,
        };
        match err {
            FlapjackError::InvalidQuery(message) => {
                assert_eq!(message, "Cursor is not valid anymore (index modified)")
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn decode_and_validate_cursor_rejects_invalid_utf8_payload() {
        let encoded = base64::engine::general_purpose::STANDARD.encode([0xff, 0xfe, 0xfd]);
        let err = match decode_and_validate_cursor(&encoded, 123_u64) {
            Ok(_) => panic!("expected invalid utf-8 cursor payload"),
            Err(err) => err,
        };
        match err {
            FlapjackError::InvalidQuery(message) => {
                assert_eq!(message, "Invalid cursor encoding")
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn decode_and_validate_cursor_rejects_invalid_json_payload() {
        let encoded = base64::engine::general_purpose::STANDARD.encode("not-json");
        let err = match decode_and_validate_cursor(&encoded, 123_u64) {
            Ok(_) => panic!("expected invalid json cursor payload"),
            Err(err) => err,
        };
        match err {
            FlapjackError::InvalidQuery(message) => {
                assert_eq!(message, "Invalid cursor format")
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// TODO: Document resolve_browse_params_prefers_cursor_values.
    #[test]
    fn resolve_browse_params_prefers_cursor_values() {
        let req = BrowseRequest {
            cursor: None,
            query: "from-request".to_string(),
            filters: Some("brand:acme".to_string()),
            hits_per_page: 50,
            attributes_to_retrieve: None,
            params: None,
        };
        let cursor = BrowseCursor {
            offset: 10,
            generation: 1,
            query: Some("from-cursor".to_string()),
            filters: Some("brand:globex".to_string()),
            hits_per_page: Some(70),
        };

        let (query, filters, hits_per_page) = resolve_browse_params(&req, Some(&cursor));
        assert_eq!(query, "from-cursor");
        assert_eq!(filters.as_deref(), Some("brand:globex"));
        assert_eq!(hits_per_page, 70);
    }

    /// TODO: Document resolve_browse_params_cursor_without_filters_clears_request_filters.
    #[test]
    fn resolve_browse_params_cursor_without_filters_clears_request_filters() {
        let req = BrowseRequest {
            cursor: None,
            query: "from-request".to_string(),
            filters: Some("brand:acme".to_string()),
            hits_per_page: 50,
            attributes_to_retrieve: None,
            params: None,
        };
        let cursor = BrowseCursor {
            offset: 10,
            generation: 1,
            query: Some("from-cursor".to_string()),
            filters: None,
            hits_per_page: Some(70),
        };

        let (query, filters, hits_per_page) = resolve_browse_params(&req, Some(&cursor));
        assert_eq!(query, "from-cursor");
        assert!(filters.is_none());
        assert_eq!(hits_per_page, 70);
    }

    #[test]
    fn build_next_cursor_returns_none_when_offset_reaches_total() {
        let next = build_next_cursor(20, 20, 42, "q", Some("category:books"), 20);
        assert!(next.is_none());
    }

    #[test]
    fn build_next_cursor_encodes_query_and_filters_context() {
        let encoded = build_next_cursor(3, 10, 42, "q", Some("category:books"), 20)
            .expect("next cursor should exist");
        let parsed = decode_and_validate_cursor(&encoded, 42).expect("roundtrip");

        assert_eq!(parsed.offset, 3);
        assert_eq!(parsed.generation, 42);
        assert_eq!(parsed.query.as_deref(), Some("q"));
        assert_eq!(parsed.filters.as_deref(), Some("category:books"));
        assert_eq!(parsed.hits_per_page, Some(20));
    }
}
