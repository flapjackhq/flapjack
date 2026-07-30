use super::metrics::QueryPhase;
use super::sorting::SortValue;
use super::QueryExecutor;
use crate::error::Result;
use crate::types::ScoredDocument;
use tantivy::collector::{Count, TopDocs};
use tantivy::postings::Postings;
use tantivy::query::Query as TantivyQuery;
use tantivy::schema::{document::ReferenceValue, Value};
use tantivy::{DocSet, Searcher, TERMINATED};

impl QueryExecutor {
    fn ordered_searchable_paths(&self) -> Vec<&String> {
        self.searchable_paths
            .iter()
            .filter(|p| !self.unordered_paths.contains((*p).as_str()))
            .take(2)
            .collect()
    }

    /// Execute a relevance-sorted search: collect top-K candidates by BM25 score,
    /// then apply tier-2 ranking (attribute position, custom ranking) to re-sort.
    pub(crate) fn execute_relevance_sort(
        &self,
        searcher: &Searcher,
        query: Box<dyn TantivyQuery>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<ScoredDocument>, usize)> {
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

        let collect_started_at = std::time::Instant::now();
        let (total, mut top_docs) = self.search_bounded(
            searcher,
            query.as_ref(),
            &(Count, TopDocs::with_limit(prelim_limit).order_by_score()),
        )?;
        self.observe_phase(QueryPhase::Collect, collect_started_at);
        self.set_matched_docs(total);
        self.set_candidates_collected(top_docs.len());

        let rank_started_at = std::time::Instant::now();
        let query_terms: Vec<String> = self
            .query_text
            .split_whitespace()
            .map(|s| s.to_lowercase())
            .collect();
        top_docs = self.apply_tier2_and_custom_ranking(searcher, top_docs, &query_terms)?;
        self.observe_phase(QueryPhase::Rank, rank_started_at);

        let final_docs = top_docs.into_iter().skip(offset).take(limit).collect();
        let fetch_started_at = std::time::Instant::now();
        let documents = self.reconstruct_documents(searcher, final_docs)?;
        self.observe_phase(QueryPhase::Fetch, fetch_started_at);
        let report = self.phase_report();
        tracing::debug!(
            "[REL] search={:?} tier2={:?} reconstruct={:?} total_hits={}",
            std::time::Duration::from_nanos(report.collect_ns),
            std::time::Duration::from_nanos(report.rank_ns),
            std::time::Duration::from_nanos(report.fetch_ns),
            total
        );
        Ok((documents, total))
    }

    /// Re-rank BM25 candidates by attribute position (min term position per document)
    /// and then by custom ranking fields (asc/desc per `customRanking` settings).
    pub(crate) fn apply_tier2_and_custom_ranking(
        &self,
        searcher: &Searcher,
        docs: Vec<(f32, tantivy::DocAddress)>,
        query_terms: &[String],
    ) -> Result<Vec<(f32, tantivy::DocAddress)>> {
        let custom_ranking = match &self.settings {
            Some(s) => match &s.custom_ranking {
                Some(cr) if !cr.is_empty() => cr,
                _ => return self.apply_tier2_only(searcher, docs, query_terms),
            },
            None => return self.apply_tier2_only(searcher, docs, query_terms),
        };

        let tantivy_schema = searcher.schema();
        let json_filter_field = tantivy_schema
            .get_field("_json_filter")
            .map_err(|_| crate::error::FlapjackError::FieldNotFound("_json_filter".to_string()))?;

        let mut ranking_specs: Vec<(String, bool)> = Vec::new();

        for spec in custom_ranking {
            let (direction, attr) = if let Some(attr) = spec.strip_prefix("desc(") {
                (false, attr.trim_end_matches(')'))
            } else if let Some(attr) = spec.strip_prefix("asc(") {
                (true, attr.trim_end_matches(')'))
            } else {
                continue;
            };

            ranking_specs.push((attr.to_string(), direction));
        }

        if ranking_specs.is_empty() {
            return self.apply_tier2_only(searcher, docs, query_terms);
        }

        let mut scored: Vec<(u32, Vec<SortValue>, f32, tantivy::DocAddress)> = Vec::new();

        for (score, addr) in docs {
            let doc: tantivy::TantivyDocument = searcher.doc(addr)?;

            let min_position = self.extract_min_position(searcher, addr, query_terms)?;

            let mut values = Vec::new();

            for (attr_name, _) in &ranking_specs {
                let val = if let Some(json_value) = doc.get_first(json_filter_field) {
                    if let ReferenceValue::Object(obj) = json_value.as_value() {
                        let mut found = SortValue::Missing;
                        for (path, compact_val) in obj {
                            if path == attr_name {
                                found = match compact_val.as_value() {
                                    ReferenceValue::Leaf(leaf) => match leaf {
                                        tantivy::schema::document::ReferenceValueLeaf::I64(i) => {
                                            SortValue::Integer(i)
                                        }
                                        tantivy::schema::document::ReferenceValueLeaf::U64(u) => {
                                            SortValue::Integer(u as i64)
                                        }
                                        tantivy::schema::document::ReferenceValueLeaf::F64(f) => {
                                            SortValue::Float(f)
                                        }
                                        tantivy::schema::document::ReferenceValueLeaf::Str(s) => {
                                            SortValue::Text(s.to_string())
                                        }
                                        _ => SortValue::Missing,
                                    },
                                    _ => SortValue::Missing,
                                };
                                break;
                            }
                        }
                        found
                    } else {
                        SortValue::Missing
                    }
                } else {
                    SortValue::Missing
                };
                values.push(val);
            }
            scored.push((min_position, values, score, addr));
        }

        scored.sort_by(|a, b| {
            let pos_cmp = a.0.cmp(&b.0);
            if pos_cmp != std::cmp::Ordering::Equal {
                return pos_cmp;
            }

            for (idx, (_, asc)) in ranking_specs.iter().enumerate() {
                let cmp = match (&a.1[idx], &b.1[idx]) {
                    (SortValue::Missing, SortValue::Missing) => std::cmp::Ordering::Equal,
                    (SortValue::Missing, _) => std::cmp::Ordering::Greater,
                    (_, SortValue::Missing) => std::cmp::Ordering::Less,
                    _ => {
                        if *asc {
                            a.1[idx].cmp(&b.1[idx])
                        } else {
                            b.1[idx].cmp(&a.1[idx])
                        }
                    }
                };
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            a.3.doc_id.cmp(&b.3.doc_id)
        });

        Ok(scored
            .into_iter()
            .map(|(_, _, score, addr)| (score, addr))
            .collect())
    }

    /// Apply attribute-position ranking without custom ranking fields; used when
    /// no `customRanking` is configured.
    pub(crate) fn apply_tier2_only(
        &self,
        searcher: &Searcher,
        docs: Vec<(f32, tantivy::DocAddress)>,
        query_terms: &[String],
    ) -> Result<Vec<(f32, tantivy::DocAddress)>> {
        use std::collections::HashMap;
        use tantivy::schema::IndexRecordOption;

        let mut doc_positions: HashMap<(u32, u32), u32> = HashMap::with_capacity(docs.len());
        let mut candidates_by_segment = vec![Vec::new(); searcher.segment_readers().len()];
        for doc in &docs {
            doc_positions.insert((doc.1.segment_ord, doc.1.doc_id), u32::MAX);
            candidates_by_segment[doc.1.segment_ord as usize].push(doc.1.doc_id);
        }
        for candidates in &mut candidates_by_segment {
            candidates.sort_unstable();
            candidates.dedup();
        }

        // Only apply position penalty to ordered paths (unordered paths skip position scoring)
        let top_paths = self.ordered_searchable_paths();

        for (segment_ord, candidates) in candidates_by_segment.iter().enumerate() {
            if candidates.is_empty() {
                continue;
            }
            let segment_reader = searcher.segment_reader(segment_ord as u32);
            let inverted_index = segment_reader.inverted_index(self.json_search_field)?;

            for path in &top_paths {
                for term_text in query_terms {
                    let full_term = format!("{}\0s{}", path, term_text);
                    let term = tantivy::Term::from_field_text(self.json_search_field, &full_term);

                    if let Some(mut postings) = inverted_index
                        .read_postings(&term, IndexRecordOption::WithFreqsAndPositions)?
                    {
                        // The measured matrix put collection first (94.694843%
                        // multi-word, 50.190622% facet). This adjacent postings
                        // walk now seeks only through collected candidates while
                        // preserving exact min-position ranking; executor parity
                        // and the local cold facet gate cover the invariant/win.
                        let mut current_doc = postings.doc();
                        let mut positions = Vec::new();
                        for &candidate_doc in candidates {
                            if current_doc == TERMINATED {
                                break;
                            }
                            if current_doc < candidate_doc {
                                current_doc = postings.seek(candidate_doc);
                            }
                            if current_doc == candidate_doc {
                                positions.clear();
                                positions.reserve(postings.term_freq() as usize);
                                postings.positions(&mut positions);
                                if let Some(&first_pos) = positions.first() {
                                    let min_pos = doc_positions
                                        .get_mut(&(segment_ord as u32, candidate_doc))
                                        .expect("collected candidate position must exist");
                                    *min_pos = (*min_pos).min(first_pos);
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut scored: Vec<(u32, f32, tantivy::DocAddress)> = docs
            .into_iter()
            .map(|(score, addr)| {
                let pos = doc_positions
                    .get(&(addr.segment_ord, addr.doc_id))
                    .copied()
                    .unwrap_or(u32::MAX);
                (pos, score, addr)
            })
            .collect();

        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.doc_id.cmp(&b.2.doc_id)));

        Ok(scored
            .into_iter()
            .map(|(_, score, addr)| (score, addr))
            .collect())
    }

    /// Find the minimum term position across all searchable fields for a document,
    /// used as the attribute-ranking signal (lower position = better match).
    pub(crate) fn extract_min_position(
        &self,
        searcher: &Searcher,
        addr: tantivy::DocAddress,
        query_terms: &[String],
    ) -> Result<u32> {
        use tantivy::schema::IndexRecordOption;

        let segment_reader = searcher.segment_reader(addr.segment_ord);
        let inverted_index = segment_reader.inverted_index(self.json_search_field)?;

        let mut min_pos = u32::MAX;

        // Only check top 2 ordered searchable paths (same selection as apply_tier2_only).
        for path in self.ordered_searchable_paths() {
            for term_text in query_terms {
                let full_term = format!("{}\0s{}", path, term_text);
                let term = tantivy::Term::from_field_text(self.json_search_field, &full_term);

                if let Some(mut postings) =
                    inverted_index.read_postings(&term, IndexRecordOption::WithFreqsAndPositions)?
                {
                    // `DocSet::seek` requires target >= current doc. If the first posting
                    // is already beyond our target doc, this term cannot match the doc.
                    let current_doc = postings.doc();
                    if current_doc == TERMINATED || current_doc > addr.doc_id {
                        continue;
                    }
                    // Use seek() to jump directly to the target doc instead of walking
                    // through every posting.
                    let doc_id = if current_doc == addr.doc_id {
                        current_doc
                    } else {
                        postings.seek(addr.doc_id)
                    };
                    if doc_id == addr.doc_id {
                        let mut positions: Vec<u32> =
                            Vec::with_capacity(postings.term_freq() as usize);
                        postings.positions(&mut positions);
                        if let Some(&first_pos) = positions.first() {
                            min_pos = min_pos.min(first_pos);
                        }
                    }
                }
            }
        }

        Ok(min_pos)
    }
}
