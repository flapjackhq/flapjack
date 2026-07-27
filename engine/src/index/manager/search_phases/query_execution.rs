use super::super::*;

pub(super) type FacetResultCache = (
    usize,
    HashMap<String, Vec<crate::types::FacetCount>>,
    HashMap<String, crate::types::FacetStats>,
    bool,
);

pub(super) type OptionalFilterGroup = Vec<(String, String, f32)>;

/// Prepared filter/query state that feeds the main Tantivy execution phase.
pub(in crate::index::manager) struct PreparedSearchFilters {
    pub effective_params: EffectiveSearchParams,
    pub query_text_rewritten: String,
    pub parsed_query: String,
    pub expanded_queries: Vec<String>,
    pub searchable_paths: Vec<String>,
    pub field_weights: Vec<f32>,
    pub schema: tantivy::schema::Schema,
    pub json_search_field: tantivy::schema::Field,
    pub json_exact_field: tantivy::schema::Field,
    pub rule_effects: Option<RuleEffects>,
    pub synonym_store: Option<Arc<SynonymStore>>,
}

/// Shared read-only context for the expanded-query execution loop. Holds references
/// to the index, searcher, parser, filters, facet requests, and split-alternative
/// settings to avoid threading individual parameters.
pub(super) struct ExpandedQueryExecutionContext<'a> {
    pub manager: &'a super::super::IndexManager,
    pub index: &'a Arc<Index>,
    pub searcher: &'a tantivy::Searcher,
    pub settings: &'a Option<Arc<IndexSettings>>,
    pub schema: &'a tantivy::schema::Schema,
    pub parser: &'a QueryParser,
    pub effective_sort: Option<&'a Sort>,
    pub effective_filter: Option<&'a crate::types::Filter>,
    pub optional_filter_specs: Option<&'a [OptionalFilterGroup]>,
    pub facets: Option<&'a [crate::types::FacetRequest]>,
    pub distinct: Option<u32>,
    pub max_values_per_facet: Option<usize>,
    pub effective_limit: usize,
    pub facet_cache_key: Option<&'a String>,
    pub allow_split_alternatives: bool,
    pub query_text: &'a str,
    pub json_exact_field: tantivy::schema::Field,
    pub searchable_paths: &'a [String],
}

/// Cache facet results for reuse across expanded queries. Evicts the oldest entry
/// when the cache is at capacity. No-ops when no cache key is provided.
pub(super) fn maybe_cache_facets(
    manager: &super::super::IndexManager,
    facet_cache_key: Option<&String>,
    total: usize,
    facets_map: &HashMap<String, Vec<crate::types::FacetCount>>,
    facets_stats: &HashMap<String, crate::types::FacetStats>,
    exhaustive_facet_values: bool,
) {
    let Some(cache_key) = facet_cache_key else {
        return;
    };

    if manager.facet_cache.len()
        >= manager
            .facet_cache_cap
            .load(std::sync::atomic::Ordering::Relaxed)
    {
        // Capture the key in a separate scope so iterator/entry guards are
        // dropped before we take a write lock via `remove`.
        let evict_key = {
            let mut iter = manager.facet_cache.iter();
            iter.next().map(|entry| entry.key().clone())
        };
        if let Some(evict_key) = evict_key {
            manager.facet_cache.remove(&evict_key);
        }
    }
    manager.facet_cache.insert(
        cache_key.clone(),
        Arc::new((
            std::time::Instant::now(),
            total,
            facets_map.clone(),
            facets_stats.clone(),
            exhaustive_facet_values,
        )),
    );
}

/// Execute each expanded query variant in sequence, deduplicating results by document
/// ID. Caches facets from the first query, generates split-word alternatives when
/// results are insufficient, and stops once the effective limit is reached.
pub(super) fn execute_expanded_queries(
    mut expanded_queries: Vec<String>,
    mut facet_result: Option<FacetResultCache>,
    context: &ExpandedQueryExecutionContext<'_>,
) -> Result<(
    Vec<crate::types::ScoredDocument>,
    Vec<usize>,
    Option<FacetResultCache>,
)> {
    let mut all_results = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut query_totals = Vec::new();
    let mut split_alternatives_generated = false;
    let mut query_idx = 0;

    while query_idx < expanded_queries.len() {
        let result = execute_single_expanded_query(
            &expanded_queries[query_idx],
            query_idx,
            facet_result.is_some(),
            context,
        )?;

        if query_idx == 0 && facet_result.is_none() && !result.facets.is_empty() {
            maybe_cache_facets(
                context.manager,
                context.facet_cache_key,
                result.total,
                &result.facets,
                &result.facets_stats,
                result.exhaustive_facet_values,
            );
            facet_result = Some((
                result.total,
                result.facets.clone(),
                result.facets_stats.clone(),
                result.exhaustive_facet_values,
            ));
        }

        query_totals.push(result.total);
        for doc in result.documents {
            if seen_ids.insert(doc.document.id.clone()) {
                all_results.push(doc);
            }
        }

        query_idx += 1;
        if all_results.len() >= context.effective_limit {
            break;
        }

        if should_generate_split_alternatives(
            query_idx,
            expanded_queries.len(),
            split_alternatives_generated,
            context.query_text,
            context.allow_split_alternatives,
            all_results.len(),
            context.effective_limit,
        ) {
            split_alternatives_generated = true;
            append_split_alternatives(
                &mut expanded_queries,
                context.searcher,
                context.json_exact_field,
                context.searchable_paths,
            );
        }
    }

    Ok((all_results, query_totals, facet_result))
}

/// Execute one expanded query: parse it, then let `QueryExecutor` own expansion,
/// optional boosts, filtering, collection, and phase attribution as one invocation.
fn execute_single_expanded_query(
    expanded_query: &str,
    query_idx: usize,
    has_cached_facets: bool,
    context: &ExpandedQueryExecutionContext<'_>,
) -> Result<crate::types::SearchResult> {
    let tq0 = std::time::Instant::now();
    let query = crate::types::Query {
        text: expanded_query.to_string(),
    };
    let parsed_query = context.parser.parse(&query)?;
    let tq1 = tq0.elapsed();

    let executor = QueryExecutor::new(context.index.converter(), context.schema.clone())
        .with_settings(context.settings.clone())
        .with_query(expanded_query.to_string())
        .with_max_values_per_facet(context.max_values_per_facet);

    let optional_boosts = context
        .optional_filter_specs
        .into_iter()
        .flatten()
        .flat_map(|group| group.iter().cloned())
        .collect::<Vec<_>>();

    let inline_facets = if query_idx == 0 && !has_cached_facets {
        context.facets
    } else {
        None
    };

    let result = executor.execute_with_facets_and_optional_boosts(
        context.searcher,
        parsed_query,
        context.effective_filter,
        &crate::query::executor::FacetSearchParams {
            sort: context.effective_sort,
            limit: context.effective_limit,
            offset: 0,
            has_text_query: !expanded_query.trim().is_empty(),
            facet_requests: inline_facets,
            distinct_count: if query_idx == 0 {
                context.distinct
            } else {
                None
            },
        },
        &optional_boosts,
    )?;
    let report = executor.phase_report();
    tracing::debug!(
        "[QUERY_PREP] parse={:?} executor_prepare={:?} query='{}'",
        tq1,
        std::time::Duration::from_nanos(report.prepare_ns),
        expanded_query
    );
    Ok(result)
}

/// Generate word-split alternatives for each base query (e.g. "shoerack" → "shoe
/// rack") by checking the index for exact matches on substrings. Caps total
/// expanded queries at 15.
fn append_split_alternatives(
    expanded_queries: &mut Vec<String>,
    searcher: &tantivy::Searcher,
    json_exact_field: tantivy::schema::Field,
    searchable_paths: &[String],
) {
    let base_queries = expanded_queries.clone();
    for base_query in &base_queries {
        let alternatives = crate::query::splitting::generate_alternatives(
            base_query,
            searcher,
            json_exact_field,
            searchable_paths,
        );
        for alternative in alternatives {
            if !expanded_queries.contains(&alternative) {
                expanded_queries.push(alternative);
            }
        }
        if expanded_queries.len() >= 15 {
            break;
        }
    }
}

pub(super) fn should_generate_split_alternatives(
    current_query_index: usize,
    total_expanded_queries: usize,
    split_alternatives_generated: bool,
    original_query_text: &str,
    allow_split_alternatives: bool,
    collected_result_count: usize,
    effective_limit: usize,
) -> bool {
    current_query_index == total_expanded_queries
        && !split_alternatives_generated
        && !original_query_text.trim().is_empty()
        && allow_split_alternatives
        && collected_result_count < effective_limit
}

pub(super) fn resolve_total_hits(
    query_totals: &[usize],
    result_count: usize,
    effective_limit: usize,
) -> usize {
    if query_totals.len() == 1 {
        query_totals[0]
    } else if result_count < effective_limit {
        result_count
    } else {
        query_totals.iter().copied().max().unwrap_or(result_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_facet_data() -> (
        HashMap<String, Vec<crate::types::FacetCount>>,
        HashMap<String, crate::types::FacetStats>,
    ) {
        let facets_map = HashMap::from([(
            "brand".to_string(),
            vec![crate::types::FacetCount {
                path: "Apple".to_string(),
                count: 3,
            }],
        )]);
        let facets_stats = HashMap::from([(
            "price".to_string(),
            crate::types::FacetStats {
                min: 1.0,
                max: 9.0,
                avg: 5.0,
                sum: 15.0,
            },
        )]);
        (facets_map, facets_stats)
    }

    #[tokio::test]
    async fn maybe_cache_facets_without_key_is_noop() {
        let temp_dir = TempDir::new().unwrap();
        let manager = super::super::IndexManager::new(temp_dir.path());
        let (facets_map, facets_stats) = sample_facet_data();

        maybe_cache_facets(&manager, None, 7, &facets_map, &facets_stats, true);

        assert!(
            manager.facet_cache.is_empty(),
            "facet cache should remain unchanged when no cache key is provided"
        );
    }
    #[tokio::test]
    async fn maybe_cache_facets_inserts_expected_payload() {
        let temp_dir = TempDir::new().unwrap();
        let manager = super::super::IndexManager::new(temp_dir.path());
        let (facets_map, facets_stats) = sample_facet_data();
        let cache_key = "facet:k1".to_string();

        maybe_cache_facets(
            &manager,
            Some(&cache_key),
            7,
            &facets_map,
            &facets_stats,
            true,
        );

        let cached = manager
            .facet_cache
            .get(&cache_key)
            .expect("expected cache entry for inserted key")
            .value()
            .clone();
        assert_eq!(cached.1, 7, "cached total should match provided total");
        assert!(cached.4, "cached exhaustive flag should be preserved");
        assert_eq!(
            cached
                .2
                .get("brand")
                .expect("expected cached brand facet counts")[0]
                .count,
            3
        );
        assert_eq!(
            cached
                .3
                .get("price")
                .expect("expected cached price facet stats")
                .avg,
            5.0
        );
    }
    #[tokio::test]
    async fn maybe_cache_facets_evicts_existing_entry_when_at_capacity() {
        let temp_dir = TempDir::new().unwrap();
        let manager = super::super::IndexManager::new(temp_dir.path());
        manager
            .facet_cache_cap
            .store(1, std::sync::atomic::Ordering::Relaxed);
        let (facets_map, facets_stats) = sample_facet_data();
        let first_key = "facet:first".to_string();
        let second_key = "facet:second".to_string();

        maybe_cache_facets(
            &manager,
            Some(&first_key),
            2,
            &facets_map,
            &facets_stats,
            false,
        );
        maybe_cache_facets(
            &manager,
            Some(&second_key),
            5,
            &facets_map,
            &facets_stats,
            true,
        );

        assert_eq!(
            manager.facet_cache.len(),
            1,
            "cache cap of 1 should be enforced"
        );
        assert!(
            manager.facet_cache.contains_key(&second_key),
            "most recent insertion should remain in cache after eviction"
        );
    }
    #[test]
    fn should_generate_split_alternatives_requires_full_condition_set() {
        assert!(should_generate_split_alternatives(
            2,
            2,
            false,
            "shoe rack",
            true,
            0,
            20,
        ));

        assert!(!should_generate_split_alternatives(
            1,
            2,
            false,
            "shoe rack",
            true,
            0,
            20,
        ));
        assert!(!should_generate_split_alternatives(
            2,
            2,
            true,
            "shoe rack",
            true,
            0,
            20,
        ));
        assert!(!should_generate_split_alternatives(
            2, 2, false, "", true, 0, 20,
        ));
        assert!(!should_generate_split_alternatives(
            2,
            2,
            false,
            "shoe rack",
            false,
            0,
            20,
        ));
        assert!(!should_generate_split_alternatives(
            2,
            2,
            false,
            "shoe rack",
            true,
            20,
            20,
        ));
    }

    #[test]
    fn resolved_total_prefers_max_when_multiple_queries_hit_limit() {
        assert_eq!(resolve_total_hits(&[12], 5, 20), 12);
        assert_eq!(resolve_total_hits(&[8, 5], 10, 20), 10);
        assert_eq!(resolve_total_hits(&[8, 15], 20, 20), 15);
        assert_eq!(resolve_total_hits(&[], 0, 20), 0);
        assert_eq!(resolve_total_hits(&[], 20, 20), 20);
    }
}
