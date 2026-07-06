//! Facet collection and distinct deduplication for search results. Executes queries with tantivy collectors, extracts and trims facet counts, and deduplicates documents by attribute value with configurable group limits.
use super::QueryExecutor;
use crate::error::Result;
use crate::types::{FacetCount, FacetRequest, FacetStats, SearchResult, Sort};
use std::collections::HashMap;
use tantivy::collector::{Count, FacetCollector, TopDocs};
use tantivy::query::Query as TantivyQuery;
use tantivy::Searcher;

/// Groups sort, pagination, facet, and distinct parameters for faceted search execution.
pub struct FacetSearchParams<'a> {
    pub sort: Option<&'a Sort>,
    pub limit: usize,
    pub offset: usize,
    pub has_text_query: bool,
    pub facet_requests: Option<&'a [FacetRequest]>,
    pub distinct_count: Option<u32>,
}

impl QueryExecutor {
    /// Execute a search with optional facet requests, applying filters, sorting, and pagination. If facet requests are provided and non-empty, delegates to execute_with_facets_internal for full facet collection; otherwise executes a standard search without facets. Optionally deduplicates results by the configured distinct attribute.
    ///
    /// # Arguments
    /// - searcher: Tantivy searcher instance
    /// - query: The search query
    /// - filter: Optional filter to apply before searching
    /// - params: FacetSearchParams containing sort, limit, offset, has_text_query, facet_requests, and distinct_count
    ///
    /// # Returns
    /// SearchResult with documents, total count, and facet data. The facets map is empty if no facets were requested.
    pub fn execute_with_facets(
        &self,
        searcher: &Searcher,
        query: Box<dyn TantivyQuery>,
        filter: Option<&crate::types::Filter>,
        params: &FacetSearchParams<'_>,
    ) -> Result<SearchResult> {
        let tf0 = std::time::Instant::now();
        let final_query = self.apply_filter(query, filter)?;
        let tf1 = tf0.elapsed();

        if let Some(facets) = params.facet_requests {
            if facets.is_empty() {
                return Err(crate::error::FlapjackError::InvalidQuery(
                    "Empty facet request array".to_string(),
                ));
            }

            return self.execute_with_facets_internal(searcher, final_query, facets, params);
        }

        let tf2 = tf0.elapsed();
        let (documents, total) = match params.sort {
            None | Some(Sort::ByRelevance) => {
                let (docs, count) = self.execute_relevance_sort(
                    searcher,
                    final_query,
                    params.limit,
                    params.offset,
                )?;
                tracing::debug!(
                    "[EXEC] filter={:?} relevance_sort={:?}",
                    tf1,
                    tf0.elapsed().saturating_sub(tf2)
                );
                (docs, count)
            }
            Some(Sort::ByField { field, order }) => {
                if params.has_text_query {
                    let (docs, count) = self.execute_relevance_first_sort(
                        searcher,
                        final_query,
                        field,
                        order,
                        params.limit,
                        params.offset,
                    )?;
                    (docs, count)
                } else {
                    let (docs, count) = self.execute_pure_sort(
                        searcher,
                        final_query,
                        field,
                        order,
                        params.limit,
                        params.offset,
                    )?;
                    (docs, count)
                }
            }
        };

        let (documents, total) = if let Some(distinct) = params.distinct_count {
            if distinct > 0 {
                self.apply_distinct(documents, total, distinct)?
            } else {
                (documents, total)
            }
        } else {
            (documents, total)
        };

        Ok(SearchResult {
            documents,
            total,
            facets: HashMap::new(),
            facets_stats: HashMap::new(),
            user_data: Vec::new(),
            applied_rules: Vec::new(),
            parsed_query: self.query_text.clone(),
            exhaustive_facet_values: true,
            exhaustive_rules_match: true,
            query_after_removal: None,
            rendering_content: None,
            effective_around_lat_lng: None,
            effective_around_radius: None,
        })
    }

    /// Execute a search with facet collection and optional distinct deduplication. Handles both relevance-based and field-based sorting with optional custom ranking, applies pagination with document reconstruction, and extracts count-based and numeric statistics for facets. When limit and offset are both 0, skips document reconstruction but still collects complete facet data.
    ///
    /// # Arguments
    /// - searcher: Tantivy searcher instance
    /// - query: The search query (already filtered)
    /// - facet_requests: Facet paths to collect
    /// - params: FacetSearchParams containing sort, limit, offset, has_text_query, and distinct_count
    ///
    /// # Returns
    /// SearchResult with documents (sorted and paginated), total count, facet counts sorted by frequency, statistics for numeric facets (min/max/avg/sum), and exhaustive_facet_values flag indicating if all unique facet values are included.
    pub(crate) fn execute_with_facets_internal(
        &self,
        searcher: &Searcher,
        query: Box<dyn TantivyQuery>,
        facet_requests: &[FacetRequest],
        params: &FacetSearchParams<'_>,
    ) -> Result<SearchResult> {
        let sort = params.sort;
        let limit = params.limit;
        let offset = params.offset;
        let has_text_query = params.has_text_query;
        let distinct_count = params.distinct_count;

        let mut facet_collector = FacetCollector::for_field("_facets");
        for req in facet_requests {
            facet_collector.add_facet(&req.path);
        }

        tracing::debug!(
            "[FACET_SORT] sort={:?} has_text_query={} limit={} offset={}",
            sort,
            has_text_query,
            limit,
            offset
        );

        if limit == 0 && offset == 0 {
            let (count, facets) = searcher.search(query.as_ref(), &(Count, facet_collector))?;
            let (documents, total) = if let Some(distinct) = distinct_count {
                if distinct > 0 {
                    self.apply_distinct(Vec::new(), count, distinct)?
                } else {
                    (Vec::new(), count)
                }
            } else {
                (Vec::new(), count)
            };
            let (facets, facets_stats, exhaustive_facet_values) =
                self.extract_facet_counts_and_stats(&facets, facet_requests);
            return Ok(SearchResult {
                documents,
                total,
                facets,
                facets_stats,
                user_data: Vec::new(),
                applied_rules: Vec::new(),
                parsed_query: self.query_text.clone(),
                exhaustive_facet_values,
                exhaustive_rules_match: true,
                query_after_removal: None,
                rendering_content: None,
                effective_around_lat_lng: None,
                effective_around_radius: None,
            });
        }

        let (documents, total, facet_counts) = match sort {
            None | Some(Sort::ByRelevance) => {
                let fi0 = std::time::Instant::now();
                let prelim_limit = if self
                    .settings
                    .as_ref()
                    .and_then(|s| s.custom_ranking.as_ref())
                    .is_some()
                {
                    (limit + offset).saturating_mul(3).max(50)
                } else {
                    limit + offset
                };
                let top_collector = TopDocs::with_limit(prelim_limit).order_by_score();
                let (count, mut top_docs, facets) =
                    searcher.search(query.as_ref(), &(Count, top_collector, facet_collector))?;
                let fi1 = fi0.elapsed();
                let query_terms: Vec<String> = self
                    .query_text
                    .split_whitespace()
                    .map(|s| s.to_lowercase())
                    .collect();
                top_docs = self.apply_tier2_and_custom_ranking(searcher, top_docs, &query_terms)?;
                let fi2 = fi0.elapsed();
                let final_docs = top_docs.into_iter().skip(offset).take(limit).collect();
                let docs = self.reconstruct_documents(searcher, final_docs)?;
                tracing::debug!(
                    "[FACET_INT] search={:?} tier2={:?} reconstruct={:?} prelim_limit={} count={}",
                    fi1,
                    fi2.saturating_sub(fi1),
                    fi0.elapsed().saturating_sub(fi2),
                    prelim_limit,
                    count
                );
                (docs, count, facets)
            }
            Some(Sort::ByField { field, order }) => {
                if has_text_query {
                    let prelim_limit = (limit + offset).saturating_mul(3).max(50);
                    let top_collector = TopDocs::with_limit(prelim_limit)
                        .and_offset(offset)
                        .order_by_score();
                    let (count, prelim, facets) = searcher
                        .search(query.as_ref(), &(Count, top_collector, facet_collector))?;
                    let sorted =
                        self.sort_docs_by_json_field(searcher, prelim, field, order, limit, 0)?;
                    (self.reconstruct_documents(searcher, sorted)?, count, facets)
                } else {
                    self.execute_pure_sort_internal(
                        searcher,
                        query,
                        &super::sorting::SortExecutionParams {
                            field,
                            order,
                            limit,
                            offset,
                        },
                        Some(facet_collector),
                    )?
                }
            }
        };

        let (documents, total) = if let Some(distinct) = distinct_count {
            if distinct > 0 {
                self.apply_distinct(documents, total, distinct)?
            } else {
                (documents, total)
            }
        } else {
            (documents, total)
        };

        let (facets, facets_stats, exhaustive_facet_values) =
            self.extract_facet_counts_and_stats(&facet_counts, facet_requests);
        Ok(SearchResult {
            documents,
            total,
            facets,
            facets_stats,
            user_data: Vec::new(),
            applied_rules: Vec::new(),
            parsed_query: self.query_text.clone(),
            exhaustive_facet_values,
            exhaustive_rules_match: true,
            query_after_removal: None,
            rendering_content: None,
            effective_around_lat_lng: None,
            effective_around_radius: None,
        })
    }

    /// Deduplicate documents by attribute value, keeping at most distinct_count documents per group. Skips documents missing the distinct attribute. Returns group count as total.
    ///
    /// # Arguments
    /// - `documents`: Search result documents to deduplicate
    /// - `original_total`: Original hit count before deduplication
    /// - `distinct_count`: Maximum documents to keep per group
    ///
    /// # Returns
    /// Deduplicated documents and number of unique groups.
    pub(crate) fn apply_distinct(
        &self,
        documents: Vec<crate::types::ScoredDocument>,
        original_total: usize,
        distinct_count: u32,
    ) -> Result<(Vec<crate::types::ScoredDocument>, usize)> {
        let attr_name = match &self.settings {
            Some(s) => match &s.attribute_for_distinct {
                Some(attr) => attr,
                None => return Ok((documents, original_total)),
            },
            None => return Ok((documents, original_total)),
        };

        let mut seen: HashMap<String, u32> = HashMap::new();
        let mut deduplicated = Vec::new();

        for doc in documents {
            let key = match doc.document.fields.get(attr_name) {
                Some(crate::types::FieldValue::Text(s)) => s.clone(),
                Some(crate::types::FieldValue::Integer(i)) => i.to_string(),
                Some(crate::types::FieldValue::Float(f)) => f.round().to_string(),
                _ => continue,
            };

            let count = seen.entry(key.clone()).or_insert(0);
            if *count < distinct_count {
                *count += 1;
                deduplicated.push(doc);
            }
        }

        let group_count = if deduplicated.is_empty() {
            0
        } else {
            seen.len()
        };
        Ok((deduplicated, group_count))
    }

    /// Limit facet values to the configured maximum per facet. Uses explicit max_values_per_facet override, falls back to settings, defaults to 100, capped at 1000.
    ///
    /// # Arguments
    /// - `facets`: HashMap of facet field names to their counts
    /// - `_requests`: Facet requests (currently unused)
    ///
    /// # Returns
    /// Facets with each field's counts limited to the maximum.
    pub(crate) fn trim_facet_counts(
        &self,
        facets: HashMap<String, Vec<FacetCount>>,
        _requests: &[FacetRequest],
    ) -> HashMap<String, Vec<FacetCount>> {
        let limit = self
            .max_values_per_facet
            .or_else(|| {
                self.settings
                    .as_ref()
                    .map(|s| s.max_values_per_facet as usize)
            })
            .unwrap_or(100)
            .min(1000);
        facets
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().take(limit).collect()))
            .collect()
    }

    /// Extract facet counts and numeric statistics from search results. For numeric facets, calculates min, max, sum, and average. Sets exhaustive_facet_values to false if any facet has more unique values than the limit.
    ///
    /// # Arguments
    /// - `facet_counts`: Tantivy facet counts from search
    /// - `requests`: Facet requests specifying paths and field names
    ///
    /// # Returns
    /// Tuple of (facet counts map, facet stats map, exhaustive_facet_values flag).
    pub(crate) fn extract_facet_counts_and_stats(
        &self,
        facet_counts: &tantivy::collector::FacetCounts,
        requests: &[FacetRequest],
    ) -> (
        HashMap<String, Vec<FacetCount>>,
        HashMap<String, FacetStats>,
        bool,
    ) {
        let limit = self
            .max_values_per_facet
            .or_else(|| {
                self.settings
                    .as_ref()
                    .map(|s| s.max_values_per_facet as usize)
            })
            .unwrap_or(100)
            .min(1000);

        let mut result = HashMap::new();
        let mut stats = HashMap::new();
        let mut exhaustive_facet_values = true;

        for req in requests {
            let value_query = req
                .value_query
                .as_deref()
                .filter(|q| !q.is_empty())
                .map(str::to_lowercase);
            let mut value_query_truncated = false;

            let counts: Vec<FacetCount> = if let Some(vq) = &value_query {
                // searchForFacetValues: scan ALL distinct values (same full
                // iteration the stats loop below already does), so matches
                // outside the top-`limit` by count are still found.
                let mut matches: Vec<FacetCount> = facet_counts
                    .get(&req.path)
                    .map(|(facet, count)| FacetCount {
                        path: clean_facet_path(req, facet),
                        count,
                    })
                    .filter(|fc| {
                        let leaf = fc.path.rsplit(" > ").next().unwrap_or(&fc.path);
                        leaf.to_lowercase().contains(vq)
                    })
                    .collect();
                matches.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.path.cmp(&b.path)));
                if matches.len() > limit {
                    value_query_truncated = true;
                    matches.truncate(limit);
                }
                matches
            } else {
                facet_counts
                    .top_k(&req.path, limit)
                    .into_iter()
                    .map(|(facet, count)| FacetCount {
                        path: clean_facet_path(req, facet),
                        count,
                    })
                    .collect()
            };

            let mut counts = counts;
            counts.sort_by_key(|b| std::cmp::Reverse(b.count));
            result
                .entry(req.field.clone())
                .or_insert_with(Vec::new)
                .extend(counts);

            let mut all_numeric = true;
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            let mut sum = 0.0_f64;
            let mut total_count = 0_u64;
            let mut unique_count = 0_usize;

            for (facet, count) in facet_counts.get(&req.path) {
                unique_count += 1;
                if !all_numeric {
                    continue;
                }
                let raw = clean_facet_path(req, facet);
                match raw.parse::<f64>() {
                    Ok(val) if val.is_finite() => {
                        if val < min {
                            min = val;
                        }
                        if val > max {
                            max = val;
                        }
                        sum += val * count as f64;
                        total_count += count;
                    }
                    _ => {
                        all_numeric = false;
                    }
                }
            }

            if value_query.is_some() {
                if value_query_truncated {
                    exhaustive_facet_values = false;
                }
            } else if unique_count > limit {
                exhaustive_facet_values = false;
            }

            if all_numeric && total_count > 0 {
                stats.insert(
                    req.field.clone(),
                    FacetStats {
                        min,
                        max,
                        avg: sum / total_count as f64,
                        sum,
                    },
                );
            }
        }

        (result, stats, exhaustive_facet_values)
    }
}

fn clean_facet_path(req: &FacetRequest, facet: &tantivy::schema::Facet) -> String {
    let path_str = facet.to_path_string();
    let trimmed = path_str.trim_start_matches('/');
    let prefix = format!("{}/", req.path.trim_start_matches('/'));
    trimmed.strip_prefix(&prefix).unwrap_or(trimmed).to_string()
}

#[cfg(test)]
mod tests {
    use crate::index::document::DocumentConverter;
    use crate::index::schema::Schema;
    use crate::index::settings::IndexSettings;
    use crate::query::executor::QueryExecutor;
    use crate::types::{Document, FacetCount, FacetRequest, FieldValue, ScoredDocument};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn make_executor(
        settings: Option<IndexSettings>,
        max_values_per_facet: Option<usize>,
    ) -> QueryExecutor {
        let schema = Schema::builder().build();
        let tantivy_schema = schema.to_tantivy();
        let converter = Arc::new(DocumentConverter::new(&schema, &tantivy_schema).unwrap());
        QueryExecutor::new(converter, tantivy_schema)
            .with_settings(settings.map(Arc::new))
            .with_max_values_per_facet(max_values_per_facet)
    }

    fn scored_doc(id: &str, fields: Vec<(&str, FieldValue)>) -> ScoredDocument {
        ScoredDocument {
            document: Document {
                id: id.to_string(),
                fields: fields
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            },
            score: 1.0,
        }
    }

    /// Helper to construct a HashMap of facet counts from field names and count entries. Used in tests to build expected data structures.
    fn facet_counts(entries: Vec<(&str, Vec<(&str, u64)>)>) -> HashMap<String, Vec<FacetCount>> {
        entries
            .into_iter()
            .map(|(field, counts)| {
                (
                    field.to_string(),
                    counts
                        .into_iter()
                        .map(|(path, count)| FacetCount {
                            path: path.to_string(),
                            count,
                        })
                        .collect(),
                )
            })
            .collect()
    }

    fn empty_requests() -> Vec<FacetRequest> {
        vec![]
    }

    // --- trim_facet_counts ---

    #[test]
    fn trim_default_limit_100() {
        let executor = make_executor(None, None);
        let entries: Vec<(&str, u64)> = (0..150).map(|i| ("val", i as u64)).collect();
        let input = facet_counts(vec![("category", entries)]);
        let result = executor.trim_facet_counts(input, &empty_requests());
        assert_eq!(result["category"].len(), 100);
    }

    #[test]
    fn trim_explicit_override() {
        let executor = make_executor(None, Some(5));
        let entries: Vec<(&str, u64)> = (0..20).map(|i| ("val", i as u64)).collect();
        let input = facet_counts(vec![("category", entries)]);
        let result = executor.trim_facet_counts(input, &empty_requests());
        assert_eq!(result["category"].len(), 5);
    }

    #[test]
    fn trim_from_settings() {
        let settings = IndexSettings {
            max_values_per_facet: 3,
            ..IndexSettings::default()
        };
        let executor = make_executor(Some(settings), None);
        let entries: Vec<(&str, u64)> = (0..10).map(|i| ("val", i as u64)).collect();
        let input = facet_counts(vec![("category", entries)]);
        let result = executor.trim_facet_counts(input, &empty_requests());
        assert_eq!(result["category"].len(), 3);
    }

    #[test]
    fn trim_capped_at_1000() {
        let executor = make_executor(None, Some(5000));
        let entries: Vec<(&str, u64)> = (0..2000).map(|i| ("val", i as u64)).collect();
        let input = facet_counts(vec![("category", entries)]);
        let result = executor.trim_facet_counts(input, &empty_requests());
        assert_eq!(result["category"].len(), 1000);
    }

    #[test]
    fn trim_empty_facets() {
        let executor = make_executor(None, None);
        let input: HashMap<String, Vec<FacetCount>> = HashMap::new();
        let result = executor.trim_facet_counts(input, &empty_requests());
        assert!(result.is_empty());
    }

    #[test]
    fn trim_override_beats_settings() {
        let settings = IndexSettings {
            max_values_per_facet: 50,
            ..IndexSettings::default()
        };
        let executor = make_executor(Some(settings), Some(2));
        let entries: Vec<(&str, u64)> = (0..20).map(|i| ("val", i as u64)).collect();
        let input = facet_counts(vec![("category", entries)]);
        let result = executor.trim_facet_counts(input, &empty_requests());
        assert_eq!(result["category"].len(), 2);
    }

    // --- apply_distinct ---

    #[test]
    fn distinct_no_settings_passthrough() {
        let executor = make_executor(None, None);
        let docs = vec![
            scored_doc("1", vec![("cat", FieldValue::Text("a".into()))]),
            scored_doc("2", vec![("cat", FieldValue::Text("a".into()))]),
        ];
        let (result, total) = executor.apply_distinct(docs, 10, 1).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(total, 10);
    }

    #[test]
    fn distinct_no_attribute_passthrough() {
        let settings = IndexSettings {
            attribute_for_distinct: None,
            ..IndexSettings::default()
        };
        let executor = make_executor(Some(settings), None);
        let docs = vec![
            scored_doc("1", vec![("cat", FieldValue::Text("a".into()))]),
            scored_doc("2", vec![("cat", FieldValue::Text("a".into()))]),
        ];
        let (result, total) = executor.apply_distinct(docs, 10, 1).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(total, 10);
    }

    /// Verify that distinct_count=1 keeps only one document per attribute value group.
    #[test]
    fn distinct_count_1_deduplicates() {
        let settings = IndexSettings {
            attribute_for_distinct: Some("cat".to_string()),
            ..IndexSettings::default()
        };
        let executor = make_executor(Some(settings), None);
        let docs = vec![
            scored_doc("1", vec![("cat", FieldValue::Text("a".into()))]),
            scored_doc("2", vec![("cat", FieldValue::Text("a".into()))]),
            scored_doc("3", vec![("cat", FieldValue::Text("b".into()))]),
        ];
        let (result, total) = executor.apply_distinct(docs, 100, 1).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].document.id, "1");
        assert_eq!(result[1].document.id, "3");
        assert_eq!(total, 2); // 2 groups
    }

    /// Verify that distinct_count=2 allows up to two documents per attribute value group.
    #[test]
    fn distinct_count_2_allows_two_per_group() {
        let settings = IndexSettings {
            attribute_for_distinct: Some("cat".to_string()),
            ..IndexSettings::default()
        };
        let executor = make_executor(Some(settings), None);
        let docs = vec![
            scored_doc("1", vec![("cat", FieldValue::Text("a".into()))]),
            scored_doc("2", vec![("cat", FieldValue::Text("a".into()))]),
            scored_doc("3", vec![("cat", FieldValue::Text("a".into()))]),
            scored_doc("4", vec![("cat", FieldValue::Text("b".into()))]),
        ];
        let (result, total) = executor.apply_distinct(docs, 100, 2).unwrap();
        assert_eq!(result.len(), 3); // 2 from "a", 1 from "b"
        assert_eq!(total, 2); // 2 groups
    }

    /// Verify that documents without the configured distinct attribute are skipped entirely during deduplication.
    #[test]
    fn distinct_missing_field_skipped() {
        let settings = IndexSettings {
            attribute_for_distinct: Some("cat".to_string()),
            ..IndexSettings::default()
        };
        let executor = make_executor(Some(settings), None);
        let docs = vec![
            scored_doc("1", vec![("cat", FieldValue::Text("a".into()))]),
            scored_doc("2", vec![("other", FieldValue::Text("x".into()))]),
            scored_doc("3", vec![("cat", FieldValue::Text("b".into()))]),
        ];
        let (result, _total) = executor.apply_distinct(docs, 100, 1).unwrap();
        // doc "2" has no "cat" field → skipped entirely
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].document.id, "1");
        assert_eq!(result[1].document.id, "3");
    }

    #[test]
    fn distinct_integer_field() {
        let settings = IndexSettings {
            attribute_for_distinct: Some("level".to_string()),
            ..IndexSettings::default()
        };
        let executor = make_executor(Some(settings), None);
        let docs = vec![
            scored_doc("1", vec![("level", FieldValue::Integer(1))]),
            scored_doc("2", vec![("level", FieldValue::Integer(1))]),
            scored_doc("3", vec![("level", FieldValue::Integer(2))]),
        ];
        let (result, _) = executor.apply_distinct(docs, 100, 1).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn distinct_empty_docs_returns_zero_groups() {
        let settings = IndexSettings {
            attribute_for_distinct: Some("cat".to_string()),
            ..IndexSettings::default()
        };
        let executor = make_executor(Some(settings), None);
        let (result, total) = executor.apply_distinct(vec![], 100, 1).unwrap();
        assert_eq!(result.len(), 0);
        assert_eq!(total, 0);
    }

    #[test]
    fn distinct_zero_count_passthrough() {
        let settings = IndexSettings {
            attribute_for_distinct: Some("cat".to_string()),
            ..IndexSettings::default()
        };
        let executor = make_executor(Some(settings), None);
        let docs = vec![
            scored_doc("1", vec![("cat", FieldValue::Text("a".into()))]),
            scored_doc("2", vec![("cat", FieldValue::Text("a".into()))]),
        ];
        // distinct_count=0 is handled by the caller (execute_with_facets_and_distinct),
        // but apply_distinct itself would allow 0 per group → nothing passes
        let (result, _) = executor.apply_distinct(docs, 100, 0).unwrap();
        assert_eq!(result.len(), 0);
    }
}
