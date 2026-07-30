//! Query result sorting using tantivy fast columnar fields with JSON extraction fallback and cross-type value ordering (Integer < Float < Text).
use super::facet_collector::{PreparedFacetCollector, PreparedFacetCounts};
use super::metrics::{QueryExecutionPath, QueryPhase};
use super::QueryExecutor;
use crate::error::Result;
use crate::types::{ScoredDocument, SearchResult, Sort, SortOrder};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::Query as TantivyQuery;
use tantivy::Searcher;

/// Groups field-sort and pagination parameters for `execute_pure_sort_internal`.
pub(crate) struct SortExecutionParams<'a> {
    pub field: &'a str,
    pub order: &'a SortOrder,
    pub limit: usize,
    pub offset: usize,
}

impl QueryExecutor {
    /// Route a filtered search query to the appropriate sorting strategy: relevance-only (no Sort or ByRelevance), hybrid field-and-relevance sorting (ByField with text query), or pure field sorting (ByField without text query).
    pub fn execute_with_sort(
        &self,
        searcher: &Searcher,
        query: Box<dyn TantivyQuery>,
        filter: Option<&crate::types::Filter>,
        sort: Option<&Sort>,
        limit: usize,
        has_text_query: bool,
    ) -> Result<SearchResult> {
        let execution_path = match sort {
            None | Some(Sort::ByRelevance) => QueryExecutionPath::Relevance,
            Some(Sort::ByField { .. }) if has_text_query => QueryExecutionPath::SortFallback,
            Some(Sort::ByField { .. }) => QueryExecutionPath::SortFast,
        };
        let _phase_guard = self.begin_phase_report(searcher, execution_path);
        (|| {
            let expanded_query = self.expand_short_query_with_searcher(query, searcher)?;
            let prepare_started_at = std::time::Instant::now();
            let final_query = self.apply_filter(expanded_query, filter)?;
            self.observe_phase(QueryPhase::Prepare, prepare_started_at);

            let (documents, total) = match sort {
                None | Some(Sort::ByRelevance) => {
                    self.execute_relevance_sort(searcher, final_query, limit, 0)?
                }
                Some(Sort::ByField { field, order }) => {
                    if has_text_query {
                        self.execute_relevance_first_sort(
                            searcher,
                            final_query,
                            field,
                            order,
                            limit,
                            0,
                        )?
                    } else {
                        self.execute_pure_sort_fast(searcher, final_query, field, order, limit, 0)?
                    }
                }
            };

            Ok(self.build_result(documents, total))
        })()
    }

    /// Hybrid sort that fetches a large preliminary set (100× limit+offset, minimum 1000), sorts by field values, and returns the offset-limited slice. Used when both text query and field sorting are specified.
    pub(crate) fn execute_relevance_first_sort(
        &self,
        searcher: &Searcher,
        query: Box<dyn TantivyQuery>,
        field: &str,
        order: &SortOrder,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ScoredDocument>, usize)> {
        let prelim_limit = (limit + offset).saturating_mul(100).max(1000);
        let collector = TopDocs::with_limit(prelim_limit)
            .and_offset(offset)
            .order_by_score();
        let collect_started_at = std::time::Instant::now();
        let (total, prelim_results) =
            self.search_bounded(searcher, query.as_ref(), &(Count, collector))?;
        self.observe_phase(QueryPhase::Collect, collect_started_at);
        self.set_matched_docs(total);
        self.set_candidates_collected(prelim_results.len());
        let rank_started_at = std::time::Instant::now();
        let sorted_docs =
            self.sort_docs_by_json_field(searcher, prelim_results, field, order, limit, 0)?;
        self.observe_phase(QueryPhase::Rank, rank_started_at);
        let fetch_started_at = std::time::Instant::now();
        let documents = self.reconstruct_documents(searcher, sorted_docs)?;
        self.observe_phase(QueryPhase::Fetch, fetch_started_at);
        Ok((documents, total))
    }

    /// Sort a filtered query using tantivy's fast columnar fields for efficiency. Treat objectID specially via the _id FAST field; other fields use _json_filter. Fall back to in-memory sorting if the fast column does not exist.
    pub(crate) fn execute_pure_sort_fast(
        &self,
        searcher: &Searcher,
        query: Box<dyn TantivyQuery>,
        field: &str,
        order: &SortOrder,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ScoredDocument>, usize)> {
        // Special case: objectID uses the _id FAST field
        if field == "objectID" {
            let is_ascending = matches!(order, SortOrder::Asc);

            let collector = TopDocs::with_limit(limit + offset).tweak_score(
                move |segment_reader: &tantivy::SegmentReader| {
                    let ff = segment_reader.fast_fields();
                    // ff.str() takes &str, returns Result<Option<StrColumn>>
                    let str_col: Option<tantivy::columnar::StrColumn> =
                        ff.str("_id").ok().flatten();

                    move |doc_id: tantivy::DocId, _original_score: tantivy::Score| {
                        if let Some(ref col) = str_col {
                            // Get term ordinal - ordinals are sorted lexicographically
                            let ord = col.term_ords(doc_id).next().unwrap_or(0);
                            if is_ascending {
                                -(ord as f32)
                            } else {
                                ord as f32
                            }
                        } else {
                            0.0f32
                        }
                    }
                },
            );

            let collect_started_at = std::time::Instant::now();
            let (total, top_docs) =
                self.search_bounded(searcher, query.as_ref(), &(Count, collector))?;
            self.observe_phase(QueryPhase::Collect, collect_started_at);
            self.set_matched_docs(total);
            self.set_candidates_collected(top_docs.len());

            let docs_to_skip = offset.min(top_docs.len());
            let doc_addresses: Vec<(f32, tantivy::DocAddress)> =
                top_docs.into_iter().skip(docs_to_skip).collect();

            let fetch_started_at = std::time::Instant::now();
            let documents = self.reconstruct_documents(searcher, doc_addresses)?;
            self.observe_phase(QueryPhase::Fetch, fetch_started_at);
            return Ok((documents, total));
        }

        // Generic path for other fields
        let fast_field_path = format!("_json_filter.{}", field);
        let is_ascending = matches!(order, SortOrder::Asc);
        let path_clone = fast_field_path.clone();

        let has_fast_column = if let Some(segment) = searcher.segment_readers().first() {
            let ff = segment.fast_fields();
            let col: Option<tantivy::columnar::Column<f64>> =
                ff.column_opt(&path_clone).ok().flatten();
            col.is_some()
        } else {
            false
        };

        if !has_fast_column {
            tracing::warn!(
                "[SORT] No fast column for {}, using fallback",
                fast_field_path
            );
            self.set_execution_path(QueryExecutionPath::SortFallback);
            return self.execute_pure_sort_fallback(searcher, query, field, order, limit, offset);
        }

        let collector = TopDocs::with_limit(limit + offset).tweak_score(
            move |segment_reader: &tantivy::SegmentReader| {
                let ff = segment_reader.fast_fields();
                let col: Option<tantivy::columnar::Column<f64>> =
                    ff.column_opt(&path_clone).ok().flatten();

                move |doc_id: tantivy::DocId, _original_score: tantivy::Score| {
                    if let Some(ref c) = col {
                        let val = c.first(doc_id).unwrap_or(if is_ascending {
                            f64::MAX
                        } else {
                            f64::MIN
                        });
                        if is_ascending {
                            -val as f32
                        } else {
                            val as f32
                        }
                    } else if is_ascending {
                        f32::MIN
                    } else {
                        f32::MAX
                    }
                }
            },
        );

        let collect_started_at = std::time::Instant::now();
        let (total, top_docs) =
            self.search_bounded(searcher, query.as_ref(), &(Count, collector))?;
        self.observe_phase(QueryPhase::Collect, collect_started_at);
        self.set_matched_docs(total);
        self.set_candidates_collected(top_docs.len());

        let docs_to_skip = offset.min(top_docs.len());
        let doc_addresses: Vec<(f32, tantivy::DocAddress)> =
            top_docs.into_iter().skip(docs_to_skip).collect();

        let fetch_started_at = std::time::Instant::now();
        let documents = self.reconstruct_documents(searcher, doc_addresses)?;
        self.observe_phase(QueryPhase::Fetch, fetch_started_at);
        Ok((documents, total))
    }

    /// Sort documents when fast fields are unavailable by fetching surplus results (3× the limit), extracting sort values from stored JSON, sorting in memory, and returning the offset-limited slice.
    pub(crate) fn execute_pure_sort_fallback(
        &self,
        searcher: &Searcher,
        query: Box<dyn TantivyQuery>,
        field: &str,
        order: &SortOrder,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ScoredDocument>, usize)> {
        let fetch_limit = (limit + offset).saturating_mul(3).max(100);
        let collector = TopDocs::with_limit(fetch_limit).order_by_score();
        let collect_started_at = std::time::Instant::now();
        let (total, prelim_results) =
            self.search_bounded(searcher, query.as_ref(), &(Count, collector))?;
        self.observe_phase(QueryPhase::Collect, collect_started_at);
        self.set_matched_docs(total);
        self.set_candidates_collected(prelim_results.len());
        let rank_started_at = std::time::Instant::now();
        let sorted_docs =
            self.sort_docs_by_json_field(searcher, prelim_results, field, order, limit, offset)?;
        self.observe_phase(QueryPhase::Rank, rank_started_at);
        let fetch_started_at = std::time::Instant::now();
        let documents = self.reconstruct_documents(searcher, sorted_docs)?;
        self.observe_phase(QueryPhase::Fetch, fetch_started_at);
        Ok((documents, total))
    }

    pub(crate) fn execute_pure_sort(
        &self,
        searcher: &Searcher,
        query: Box<dyn TantivyQuery>,
        field: &str,
        order: &SortOrder,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ScoredDocument>, usize)> {
        self.execute_pure_sort_fast(searcher, query, field, order, limit, offset)
    }

    /// Sort a filtered query by a single field without text relevance, optionally collecting facets. Attempt tantivy fast-field sorting; fall back to in-memory JSON extraction if the fast column is unavailable. Return documents, total count, and facet results.
    pub(crate) fn execute_pure_sort_internal(
        &self,
        searcher: &Searcher,
        query: Box<dyn TantivyQuery>,
        sort_params: &SortExecutionParams<'_>,
        facet_collector: Option<PreparedFacetCollector>,
    ) -> Result<(Vec<ScoredDocument>, usize, PreparedFacetCounts)> {
        let field = sort_params.field;
        let order = sort_params.order;
        let limit = sort_params.limit;
        let offset = sort_params.offset;
        if field == "objectID" {
            let is_ascending = matches!(order, SortOrder::Asc);

            let collector = TopDocs::with_limit(limit + offset).tweak_score(
                move |segment_reader: &tantivy::SegmentReader| {
                    let ff = segment_reader.fast_fields();
                    let str_col: Option<tantivy::columnar::StrColumn> =
                        ff.str("_id").ok().flatten();
                    move |doc_id: tantivy::DocId, _original_score: tantivy::Score| {
                        if let Some(ref col) = str_col {
                            let ord = col.term_ords(doc_id).next().unwrap_or(0);
                            if is_ascending {
                                -(ord as f32)
                            } else {
                                ord as f32
                            }
                        } else {
                            0.0f32
                        }
                    }
                },
            );

            let collect_started_at = std::time::Instant::now();
            let (total, top_docs, facets) = if let Some(fc) = facet_collector {
                let (count, docs, f) =
                    self.search_bounded(searcher, query.as_ref(), &(Count, collector, fc))?;
                (count, docs, f)
            } else {
                let (count, docs) =
                    self.search_bounded(searcher, query.as_ref(), &(Count, collector))?;
                (count, docs, PreparedFacetCounts::default())
            };
            self.observe_phase(QueryPhase::Collect, collect_started_at);
            self.set_matched_docs(total);
            self.set_candidates_collected(top_docs.len());

            let docs_to_skip = offset.min(top_docs.len());
            let doc_addresses: Vec<(f32, tantivy::DocAddress)> =
                top_docs.into_iter().skip(docs_to_skip).collect();

            let fetch_started_at = std::time::Instant::now();
            let documents = self.reconstruct_documents(searcher, doc_addresses)?;
            self.observe_phase(QueryPhase::Fetch, fetch_started_at);
            return Ok((documents, total, facets));
        }

        let fast_field_path = format!("_json_filter.{}", field);
        let is_ascending = matches!(order, SortOrder::Asc);
        let path_clone = fast_field_path.clone();

        let has_fast_column = if let Some(segment) = searcher.segment_readers().first() {
            let ff = segment.fast_fields();
            let col: Option<tantivy::columnar::Column<f64>> =
                ff.column_opt(&path_clone).ok().flatten();
            col.is_some()
        } else {
            false
        };

        if has_fast_column {
            let collector = TopDocs::with_limit(limit + offset).tweak_score(
                move |segment_reader: &tantivy::SegmentReader| {
                    let ff = segment_reader.fast_fields();
                    let col: Option<tantivy::columnar::Column<f64>> =
                        ff.column_opt(&path_clone).ok().flatten();
                    move |doc_id: tantivy::DocId, _original_score: tantivy::Score| {
                        if let Some(ref c) = col {
                            let val = c.first(doc_id).unwrap_or(if is_ascending {
                                f64::MAX
                            } else {
                                f64::MIN
                            });
                            if is_ascending {
                                -val as f32
                            } else {
                                val as f32
                            }
                        } else if is_ascending {
                            f32::MIN
                        } else {
                            f32::MAX
                        }
                    }
                },
            );

            let collect_started_at = std::time::Instant::now();
            let (total, top_docs, facets) = if let Some(fc) = facet_collector {
                let (count, docs, f) =
                    self.search_bounded(searcher, query.as_ref(), &(Count, collector, fc))?;
                (count, docs, f)
            } else {
                let (count, docs) =
                    self.search_bounded(searcher, query.as_ref(), &(Count, collector))?;
                (count, docs, PreparedFacetCounts::default())
            };
            self.observe_phase(QueryPhase::Collect, collect_started_at);
            self.set_matched_docs(total);
            self.set_candidates_collected(top_docs.len());

            let docs_to_skip = offset.min(top_docs.len());
            let doc_addresses: Vec<(f32, tantivy::DocAddress)> =
                top_docs.into_iter().skip(docs_to_skip).collect();

            let fetch_started_at = std::time::Instant::now();
            let documents = self.reconstruct_documents(searcher, doc_addresses)?;
            self.observe_phase(QueryPhase::Fetch, fetch_started_at);
            return Ok((documents, total, facets));
        }

        tracing::warn!(
            "[SORT] No fast column for {}, using fallback in sort_internal",
            fast_field_path
        );
        let fetch_limit = (limit + offset).saturating_mul(3).max(100);
        let collector = TopDocs::with_limit(fetch_limit).order_by_score();

        let collect_started_at = std::time::Instant::now();
        let (total, prelim_results, facets) = if let Some(fc) = facet_collector {
            let (count, docs, f) =
                self.search_bounded(searcher, query.as_ref(), &(Count, collector, fc))?;
            (count, docs, f)
        } else {
            let (count, docs) =
                self.search_bounded(searcher, query.as_ref(), &(Count, collector))?;
            (count, docs, PreparedFacetCounts::default())
        };
        self.observe_phase(QueryPhase::Collect, collect_started_at);
        self.set_matched_docs(total);
        self.set_candidates_collected(prelim_results.len());

        let rank_started_at = std::time::Instant::now();
        let sorted_docs =
            self.sort_docs_by_json_field(searcher, prelim_results, field, order, limit, offset)?;
        self.observe_phase(QueryPhase::Rank, rank_started_at);
        let fetch_started_at = std::time::Instant::now();
        let documents = self.reconstruct_documents(searcher, sorted_docs)?;
        self.observe_phase(QueryPhase::Fetch, fetch_started_at);

        Ok((documents, total, facets))
    }

    /// Extract sort values from stored JSON documents, apply field and order sorting, and return the offset-limited slice. Delegate cross-type comparison to SortValue::cmp.
    pub(crate) fn sort_docs_by_json_field(
        &self,
        searcher: &Searcher,
        prelim_results: Vec<(tantivy::Score, tantivy::DocAddress)>,
        field: &str,
        order: &SortOrder,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<(f32, tantivy::DocAddress)>> {
        let json_filter_field = self
            .tantivy_schema
            .get_field("_json_filter")
            .map_err(|_| crate::error::FlapjackError::FieldNotFound("_json_filter".to_string()))?;

        let mut scored_docs: Vec<(SortValue, f32, tantivy::DocAddress)> = Vec::new();

        for (score, addr) in prelim_results {
            let doc: tantivy::TantivyDocument = searcher.doc(addr)?;
            let sort_value = if let Some(json_val) = doc.get_first(json_filter_field) {
                let owned: tantivy::schema::OwnedValue = json_val.into();
                self.extract_sort_value_from_json(&owned, field)
            } else {
                SortValue::Missing
            };
            if scored_docs.len() < 5 {
                tracing::trace!(
                    "[SORT_VALUE] objectID extraction: field={} value={:?}",
                    field,
                    sort_value
                );
            }
            scored_docs.push((sort_value, score, addr));
        }

        match order {
            SortOrder::Asc => scored_docs.sort_by(|a, b| a.0.cmp(&b.0)),
            SortOrder::Desc => scored_docs.sort_by(|a, b| b.0.cmp(&a.0)),
        }

        let start = offset.min(scored_docs.len());
        let end = (offset + limit).min(scored_docs.len());

        Ok(scored_docs[start..end]
            .iter()
            .map(|(_, score, addr)| (*score, *addr))
            .collect())
    }

    fn extract_sort_value_from_json(
        &self,
        json: &tantivy::schema::OwnedValue,
        field_path: &str,
    ) -> SortValue {
        let parts: Vec<&str> = field_path.split('.').collect();
        let force_string = field_path == "objectID";
        self.extract_nested_value(json, &parts, force_string)
    }

    /// Recursively traverse a dot-separated field path through nested JSON objects, returning the final value converted to SortValue. If force_string is true, convert numeric values to text strings.
    fn extract_nested_value(
        &self,
        value: &tantivy::schema::OwnedValue,
        path: &[&str],
        force_string: bool,
    ) -> SortValue {
        if path.is_empty() {
            return match value {
                tantivy::schema::OwnedValue::I64(i) => {
                    if force_string {
                        SortValue::Text(i.to_string())
                    } else {
                        SortValue::Integer(*i)
                    }
                }
                tantivy::schema::OwnedValue::U64(u) => {
                    if force_string {
                        SortValue::Text(u.to_string())
                    } else {
                        SortValue::Integer(*u as i64)
                    }
                }
                tantivy::schema::OwnedValue::F64(f) => SortValue::Float(*f),
                tantivy::schema::OwnedValue::Str(s) => {
                    if force_string {
                        SortValue::Text(s.clone())
                    } else if let Ok(i) = s.parse::<i64>() {
                        SortValue::Integer(i)
                    } else {
                        SortValue::Text(s.clone())
                    }
                }
                tantivy::schema::OwnedValue::Date(dt) => {
                    SortValue::Integer(dt.into_timestamp_secs())
                }
                _ => SortValue::Missing,
            };
        }

        match value {
            tantivy::schema::OwnedValue::Object(pairs) => {
                for (key, val) in pairs {
                    if key == path[0] {
                        return self.extract_nested_value(val, &path[1..], force_string);
                    }
                }
                SortValue::Missing
            }
            _ => SortValue::Missing,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SortValue {
    Integer(i64),
    Float(f64),
    Text(String),
    Missing,
}

impl Eq for SortValue {}

impl PartialOrd for SortValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SortValue {
    /// Compare two SortValues using total ordering: Missing sorts before all other variants, then Integer < Float < Text. Within the same variant, use natural numeric or lexicographic ordering.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (SortValue::Missing, SortValue::Missing) => std::cmp::Ordering::Equal,
            (SortValue::Missing, _) => std::cmp::Ordering::Less,
            (_, SortValue::Missing) => std::cmp::Ordering::Greater,
            (SortValue::Integer(a), SortValue::Integer(b)) => a.cmp(b),
            (SortValue::Float(a), SortValue::Float(b)) => {
                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
            }
            (SortValue::Text(a), SortValue::Text(b)) => a.cmp(b),
            (SortValue::Integer(_), _) => std::cmp::Ordering::Less,
            (SortValue::Float(_), SortValue::Text(_)) => std::cmp::Ordering::Less,
            (SortValue::Float(_), SortValue::Integer(_)) => std::cmp::Ordering::Greater,
            (SortValue::Text(_), _) => std::cmp::Ordering::Greater,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SortValue;
    use std::cmp::Ordering;

    // --- SortValue::cmp (Ord) ---

    #[test]
    fn missing_equals_missing() {
        assert_eq!(SortValue::Missing.cmp(&SortValue::Missing), Ordering::Equal);
    }

    #[test]
    fn missing_less_than_anything() {
        assert_eq!(
            SortValue::Missing.cmp(&SortValue::Integer(0)),
            Ordering::Less
        );
        assert_eq!(
            SortValue::Missing.cmp(&SortValue::Float(0.0)),
            Ordering::Less
        );
        assert_eq!(
            SortValue::Missing.cmp(&SortValue::Text("a".into())),
            Ordering::Less
        );
    }

    #[test]
    fn anything_greater_than_missing() {
        assert_eq!(
            SortValue::Integer(0).cmp(&SortValue::Missing),
            Ordering::Greater
        );
        assert_eq!(
            SortValue::Float(0.0).cmp(&SortValue::Missing),
            Ordering::Greater
        );
        assert_eq!(
            SortValue::Text("a".into()).cmp(&SortValue::Missing),
            Ordering::Greater
        );
    }

    /// Verify that integer SortValues compare using standard numeric ordering.
    #[test]
    fn integer_ordering() {
        assert_eq!(
            SortValue::Integer(1).cmp(&SortValue::Integer(2)),
            Ordering::Less
        );
        assert_eq!(
            SortValue::Integer(5).cmp(&SortValue::Integer(5)),
            Ordering::Equal
        );
        assert_eq!(
            SortValue::Integer(10).cmp(&SortValue::Integer(3)),
            Ordering::Greater
        );
        assert_eq!(
            SortValue::Integer(-1).cmp(&SortValue::Integer(0)),
            Ordering::Less
        );
    }

    #[test]
    fn float_ordering() {
        assert_eq!(
            SortValue::Float(1.0).cmp(&SortValue::Float(2.0)),
            Ordering::Less
        );
        assert_eq!(
            SortValue::Float(2.5).cmp(&SortValue::Float(2.5)),
            Ordering::Equal
        );
        assert_eq!(
            SortValue::Float(5.0).cmp(&SortValue::Float(2.0)),
            Ordering::Greater
        );
    }

    #[test]
    fn text_ordering() {
        assert_eq!(
            SortValue::Text("apple".into()).cmp(&SortValue::Text("banana".into())),
            Ordering::Less
        );
        assert_eq!(
            SortValue::Text("z".into()).cmp(&SortValue::Text("a".into())),
            Ordering::Greater
        );
        assert_eq!(
            SortValue::Text("same".into()).cmp(&SortValue::Text("same".into())),
            Ordering::Equal
        );
    }

    /// Verify that SortValues enforce consistent cross-type ordering (Integer < Float < Text) regardless of numeric magnitude.
    #[test]
    fn cross_type_ordering_integer_lt_float_lt_text() {
        // Integer < Float < Text (from the match arms)
        assert_eq!(
            SortValue::Integer(999).cmp(&SortValue::Float(0.0)),
            Ordering::Less
        );
        assert_eq!(
            SortValue::Integer(999).cmp(&SortValue::Text("a".into())),
            Ordering::Less
        );
        assert_eq!(
            SortValue::Float(999.0).cmp(&SortValue::Text("a".into())),
            Ordering::Less
        );

        assert_eq!(
            SortValue::Float(0.0).cmp(&SortValue::Integer(999)),
            Ordering::Greater
        );
        assert_eq!(
            SortValue::Text("a".into()).cmp(&SortValue::Integer(999)),
            Ordering::Greater
        );
        assert_eq!(
            SortValue::Text("a".into()).cmp(&SortValue::Float(999.0)),
            Ordering::Greater
        );
    }

    #[test]
    fn sort_value_partial_eq() {
        assert_eq!(SortValue::Integer(42), SortValue::Integer(42));
        assert_ne!(SortValue::Integer(42), SortValue::Integer(43));
        assert_eq!(SortValue::Text("hi".into()), SortValue::Text("hi".into()));
        assert_eq!(SortValue::Missing, SortValue::Missing);
    }

    /// Verify that Vec::sort respects the Ord implementation, placing Missing first, then Integer, Float, and Text in ascending order within a mixed vector.
    #[test]
    fn vec_sort_uses_ord() {
        let mut v = vec![
            SortValue::Text("b".into()),
            SortValue::Missing,
            SortValue::Integer(5),
            SortValue::Float(1.0),
        ];
        v.sort();
        assert_eq!(
            v,
            vec![
                SortValue::Missing,
                SortValue::Integer(5),
                SortValue::Float(1.0),
                SortValue::Text("b".into()),
            ]
        );
    }
}
