use crate::index::SearchOptions;
use crate::types::{FacetRequest, Filter, SearchResult, Sort};
use crate::{error::Result, IndexManager};

/// Build `SearchOptions` from the legacy positional-argument test helpers.
fn legacy_search_options<'a>(
    filter: Option<&'a Filter>,
    sort: Option<&'a Sort>,
    limit: usize,
    offset: usize,
    facets: Option<&'a [FacetRequest]>,
    distinct: Option<u32>,
    max_values_per_facet: Option<usize>,
) -> SearchOptions<'a> {
    SearchOptions {
        filter,
        sort,
        limit,
        offset,
        facets,
        distinct,
        max_values_per_facet,
        ..Default::default()
    }
}

/// Adapt legacy integration test search patterns to the new `SearchOptions`-based API.
///
/// Provides three convenience methods (`search_with_facets`, `search_with_facets_and_distinct`,
/// `search_full`) that transform positional parameters into a `SearchOptions` struct and delegate
/// to `search_with_options`. Implemented by `IndexManager` to keep integration test call sites
/// compiling during API migration.
///
/// This trait is test-only compatibility glue and is intentionally not used by production code.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
/// TODO: Document SearchCompat.
#[allow(clippy::too_many_arguments)] // Test-only shim preserves legacy lib-test callsites while production stays `SearchOptions`-based.
pub(crate) trait SearchCompat {
    fn search_with_facets(
        &self,
        tenant_id: &str,
        query_text: &str,
        filter: Option<&Filter>,
        sort: Option<&Sort>,
        limit: usize,
        offset: usize,
        facets: Option<&[FacetRequest]>,
    ) -> Result<SearchResult> {
        self.search_full(
            tenant_id, query_text, filter, sort, limit, offset, facets, None, None,
        )
    }

    fn search_with_facets_and_distinct(
        &self,
        tenant_id: &str,
        query_text: &str,
        filter: Option<&Filter>,
        sort: Option<&Sort>,
        limit: usize,
        offset: usize,
        facets: Option<&[FacetRequest]>,
        distinct: Option<u32>,
    ) -> Result<SearchResult> {
        self.search_full(
            tenant_id, query_text, filter, sort, limit, offset, facets, distinct, None,
        )
    }

    fn search_full(
        &self,
        tenant_id: &str,
        query_text: &str,
        filter: Option<&Filter>,
        sort: Option<&Sort>,
        limit: usize,
        offset: usize,
        facets: Option<&[FacetRequest]>,
        distinct: Option<u32>,
        max_values_per_facet: Option<usize>,
    ) -> Result<SearchResult>;
}

impl SearchCompat for IndexManager {
    /// Bridge the old test helper signature into `search_with_options`.
    fn search_full(
        &self,
        tenant_id: &str,
        query_text: &str,
        filter: Option<&Filter>,
        sort: Option<&Sort>,
        limit: usize,
        offset: usize,
        facets: Option<&[FacetRequest]>,
        distinct: Option<u32>,
        max_values_per_facet: Option<usize>,
    ) -> Result<SearchResult> {
        let options = legacy_search_options(
            filter,
            sort,
            limit,
            offset,
            facets,
            distinct,
            max_values_per_facet,
        );
        self.search_with_options(tenant_id, query_text, &options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::FlapjackError;
    use crate::types::{FieldValue, SortOrder};
    use std::cell::RefCell;
    use tempfile::TempDir;

    #[derive(Debug, PartialEq)]
    struct RecordedCall {
        tenant_id: String,
        query_text: String,
        filter: bool,
        sort: bool,
        limit: usize,
        offset: usize,
        facets_len: Option<usize>,
        distinct: Option<u32>,
        max_values_per_facet: Option<usize>,
    }

    #[derive(Default)]
    struct RecordingCompat {
        last_call: RefCell<Option<RecordedCall>>,
    }

    fn result_document_ids(result: &SearchResult) -> Vec<String> {
        result
            .documents
            .iter()
            .map(|doc| doc.document.id.clone())
            .collect()
    }

    impl SearchCompat for RecordingCompat {
        fn search_full(
            &self,
            tenant_id: &str,
            query_text: &str,
            filter: Option<&Filter>,
            sort: Option<&Sort>,
            limit: usize,
            offset: usize,
            facets: Option<&[FacetRequest]>,
            distinct: Option<u32>,
            max_values_per_facet: Option<usize>,
        ) -> Result<SearchResult> {
            self.last_call.replace(Some(RecordedCall {
                tenant_id: tenant_id.to_string(),
                query_text: query_text.to_string(),
                filter: filter.is_some(),
                sort: sort.is_some(),
                limit,
                offset,
                facets_len: facets.map(|items| items.len()),
                distinct,
                max_values_per_facet,
            }));
            Err(FlapjackError::InvalidQuery("sentinel".to_string()))
        }
    }

    #[test]
    fn legacy_search_options_maps_all_legacy_fields() {
        let filter = Filter::Equals {
            field: "category".to_string(),
            value: FieldValue::Text("books".to_string()),
        };
        let sort = Sort::ByField {
            field: "price".to_string(),
            order: SortOrder::Desc,
        };
        let facets = vec![FacetRequest {
            field: "category".to_string(),
            path: "/category".to_string(),
        }];

        let options = legacy_search_options(
            Some(&filter),
            Some(&sort),
            17,
            3,
            Some(&facets),
            Some(9),
            Some(33),
        );

        assert!(options.filter.is_some());
        assert!(options.sort.is_some());
        assert_eq!(options.limit, 17);
        assert_eq!(options.offset, 3);
        assert_eq!(options.facets.map(|f| f.len()), Some(1));
        assert_eq!(options.distinct, Some(9));
        assert_eq!(options.max_values_per_facet, Some(33));
        assert!(options.query_type.is_none());
        assert!(options.optional_filter_specs.is_none());
        assert!(!options.sum_or_filters_scores);
    }

    #[test]
    fn search_with_facets_delegates_without_distinct() {
        let recorder = RecordingCompat::default();
        let filter = Filter::Equals {
            field: "category".to_string(),
            value: FieldValue::Text("books".to_string()),
        };
        let sort = Sort::ByField {
            field: "price".to_string(),
            order: SortOrder::Asc,
        };
        let facets = [FacetRequest {
            field: "category".to_string(),
            path: "/category".to_string(),
        }];

        let result = recorder.search_with_facets(
            "products",
            "laptop",
            Some(&filter),
            Some(&sort),
            12,
            4,
            Some(&facets),
        );

        assert!(matches!(
            result,
            Err(FlapjackError::InvalidQuery(ref message)) if message == "sentinel"
        ));
        assert_eq!(
            recorder.last_call.borrow().as_ref(),
            Some(&RecordedCall {
                tenant_id: "products".to_string(),
                query_text: "laptop".to_string(),
                filter: true,
                sort: true,
                limit: 12,
                offset: 4,
                facets_len: Some(1),
                distinct: None,
                max_values_per_facet: None,
            })
        );
    }

    #[test]
    fn search_with_facets_and_distinct_passes_distinct_through() {
        let recorder = RecordingCompat::default();
        let facets = [FacetRequest {
            field: "brand".to_string(),
            path: "/brand".to_string(),
        }];

        let result = recorder.search_with_facets_and_distinct(
            "products",
            "tablet",
            None,
            None,
            5,
            2,
            Some(&facets),
            Some(7),
        );

        assert!(matches!(
            result,
            Err(FlapjackError::InvalidQuery(ref message)) if message == "sentinel"
        ));
        assert_eq!(
            recorder.last_call.borrow().as_ref(),
            Some(&RecordedCall {
                tenant_id: "products".to_string(),
                query_text: "tablet".to_string(),
                filter: false,
                sort: false,
                limit: 5,
                offset: 2,
                facets_len: Some(1),
                distinct: Some(7),
                max_values_per_facet: None,
            })
        );
    }

    #[tokio::test]
    async fn search_full_matches_explicit_search_options_path() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("products").unwrap();

        let docs = [
            serde_json::json!({"objectID": "a", "title": "Laptop alpha"}),
            serde_json::json!({"objectID": "b", "title": "Laptop beta"}),
            serde_json::json!({"objectID": "c", "title": "Laptop gamma"}),
        ]
        .iter()
        .map(|json| crate::types::Document::from_json(json).unwrap())
        .collect();

        manager.add_documents_sync("products", docs).await.unwrap();

        let legacy_result = manager
            .search_full("products", "laptop", None, None, 2, 1, None, None, None)
            .unwrap();
        let explicit_options = legacy_search_options(None, None, 2, 1, None, None, None);
        let explicit_result = manager
            .search_with_options("products", "laptop", &explicit_options)
            .unwrap();

        assert_eq!(legacy_result.total, explicit_result.total);
        assert_eq!(
            legacy_result.documents.len(),
            explicit_result.documents.len()
        );
        assert_eq!(
            result_document_ids(&legacy_result),
            result_document_ids(&explicit_result)
        );
    }
}
