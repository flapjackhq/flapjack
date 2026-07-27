use super::super::*;
use super::query_execution::{
    execute_expanded_queries, maybe_cache_facets, resolve_total_hits,
    ExpandedQueryExecutionContext, FacetResultCache, PreparedSearchFilters,
};
use super::{
    apply_rule_effects, sort_with_stage2_ranking, PreprocessedQuery, ResolvedSearch,
    RuleEffectsResult,
};

pub(super) struct ZeroLimitSearchContext<'a> {
    pub facets: Option<&'a [crate::types::FacetRequest]>,
    pub max_values_per_facet: Option<usize>,
    pub facet_cache_key: Option<&'a String>,
    pub facet_result: Option<FacetResultCache>,
    pub effective_around_lat_lng: Option<String>,
    pub effective_around_radius: Option<serde_json::Value>,
}

pub(super) struct RankedSearchContext<'a, 'b> {
    pub opts: &'a SearchOptions<'b>,
    pub effective_sort: Option<&'a Sort>,
    pub facets: Option<&'a [crate::types::FacetRequest]>,
    pub distinct: Option<u32>,
    pub max_values_per_facet: Option<usize>,
    pub query_text: &'a str,
    pub facet_cache_key: Option<&'a String>,
    pub facet_result: Option<FacetResultCache>,
    pub effective_limit: usize,
    pub allow_split_alternatives: bool,
}

pub(super) struct RankedSearchOutput {
    pub ruled_result: RuleEffectsResult,
    pub facet_result: Option<FacetResultCache>,
}

pub(super) fn build_search_parser(
    resolved: &ResolvedSearch,
    preprocessed: &PreprocessedQuery,
    prepared: &PreparedSearchFilters,
    opts: &SearchOptions<'_>,
) -> QueryParser {
    let SearchOptions {
        typo_tolerance: typo_tolerance_override,
        advanced_syntax: advanced_syntax_override,
        advanced_syntax_features: advanced_syntax_features_override,
        all_query_words_optional,
        ..
    } = *opts;
    let settings = &resolved.settings;
    let typo_enabled = typo_tolerance_override.unwrap_or(true);
    let min_word_1_typo = settings
        .as_ref()
        .map(|s| s.min_word_size_for_1_typo as usize)
        .unwrap_or(4);
    let min_word_2_typos = settings
        .as_ref()
        .map(|s| s.min_word_size_for_2_typos as usize)
        .unwrap_or(8);
    let disable_typo_words = settings
        .as_ref()
        .and_then(|s| s.disable_typo_tolerance_on_words.as_deref())
        .unwrap_or(&[]);
    let disable_typo_attrs = settings
        .as_ref()
        .and_then(|s| s.disable_typo_tolerance_on_attributes.as_deref())
        .unwrap_or(&[]);
    let adv_syntax = advanced_syntax_override.unwrap_or(false);
    let stemmer_lang = settings.as_ref().and_then(|s| {
        let cjk = crate::index::Index::needs_cjk_tokenizer(&s.index_languages);
        if cjk {
            None
        } else {
            crate::index::Index::stemmer_language_for_index(&s.index_languages)
        }
    });
    let parser = QueryParser::new_with_weights(
        &prepared.schema,
        vec![prepared.json_search_field],
        prepared.field_weights.clone(),
        prepared.searchable_paths.clone(),
    )
    .with_exact_field(prepared.json_exact_field)
    .with_indexed_separators(
        settings
            .as_ref()
            .map(|s| s.separators_to_index.chars().collect())
            .unwrap_or_default(),
    )
    .with_query_type(preprocessed.query_type.as_str())
    .with_typo_tolerance(typo_enabled)
    .with_disabled_typo_words(disable_typo_words.to_vec())
    .with_disabled_typo_attrs(disable_typo_attrs.to_vec())
    .with_keep_diacritics_on_characters(
        settings
            .as_ref()
            .map(|s| s.keep_diacritics_on_characters.as_str())
            .unwrap_or(""),
    )
    .with_custom_normalization(preprocessed.custom_normalization.clone())
    .with_min_word_size_for_1_typo(min_word_1_typo)
    .with_min_word_size_for_2_typos(min_word_2_typos)
    .with_advanced_syntax(adv_syntax)
    .with_all_optional(all_query_words_optional)
    .with_plural_map(preprocessed.plural_map.clone())
    .with_stemmer_language(stemmer_lang);
    if let Some(features) = advanced_syntax_features_override {
        parser.with_advanced_syntax_features(features.to_vec())
    } else if let Some(settings) = settings {
        if let Some(features) = &settings.advanced_syntax_features {
            parser.with_advanced_syntax_features(features.clone())
        } else {
            parser
        }
    } else {
        parser
    }
}

pub(super) fn execute_zero_limit_search(
    manager: &IndexManager,
    resolved: &ResolvedSearch,
    prepared: &PreparedSearchFilters,
    parser: &QueryParser,
    context: ZeroLimitSearchContext<'_>,
) -> Result<SearchResult> {
    let (total, facets_map, facets_stats, exhaustive_facet_values) = match context.facet_result {
        Some((count, facets, stats, exhaustive_facets)) => {
            (count, facets, stats, exhaustive_facets)
        }
        None => execute_uncached_zero_limit_search(manager, resolved, prepared, parser, &context)?,
    };

    Ok(SearchResult {
        documents: Vec::new(),
        total,
        facets: facets_map,
        facets_stats,
        user_data: Vec::new(),
        applied_rules: Vec::new(),
        parsed_query: prepared.parsed_query.clone(),
        exhaustive_facet_values,
        exhaustive_rules_match: true,
        query_after_removal: None,
        rendering_content: None,
        effective_around_lat_lng: context.effective_around_lat_lng,
        effective_around_radius: context.effective_around_radius,
    })
}

fn execute_uncached_zero_limit_search(
    manager: &IndexManager,
    resolved: &ResolvedSearch,
    prepared: &PreparedSearchFilters,
    parser: &QueryParser,
    context: &ZeroLimitSearchContext<'_>,
) -> Result<FacetResultCache> {
    let primary_query = crate::types::Query {
        text: prepared.query_text_rewritten.clone(),
    };
    let parsed = parser.parse(&primary_query)?;
    let executor = QueryExecutor::new(resolved.index.converter(), prepared.schema.clone())
        .with_settings(resolved.settings.clone())
        .with_query(prepared.query_text_rewritten.clone())
        .with_max_values_per_facet(context.max_values_per_facet);
    let result = executor.execute_with_facets(
        &resolved.searcher,
        parsed,
        prepared.effective_params.filter.as_ref(),
        &crate::query::executor::FacetSearchParams {
            sort: None,
            limit: 0,
            offset: 0,
            has_text_query: !prepared.query_text_rewritten.trim().is_empty(),
            facet_requests: context.facets,
            distinct_count: None,
        },
    )?;

    if context.facets.is_some() {
        maybe_cache_facets(
            manager,
            context.facet_cache_key,
            result.total,
            &result.facets,
            &result.facets_stats,
            result.exhaustive_facet_values,
        );
    }

    Ok((
        result.total,
        result.facets,
        result.facets_stats,
        result.exhaustive_facet_values,
    ))
}

pub(super) fn execute_ranked_search(
    manager: &IndexManager,
    resolved: &ResolvedSearch,
    preprocessed: &PreprocessedQuery,
    prepared: &PreparedSearchFilters,
    parser: &QueryParser,
    context: RankedSearchContext<'_, '_>,
) -> Result<RankedSearchOutput> {
    let (mut all_results, query_totals, facet_result) = execute_expanded_queries(
        prepared.expanded_queries.clone(),
        context.facet_result,
        &ExpandedQueryExecutionContext {
            manager,
            index: &resolved.index,
            searcher: &resolved.searcher,
            settings: &resolved.settings,
            schema: &prepared.schema,
            parser,
            effective_sort: context.effective_sort,
            effective_filter: prepared.effective_params.filter.as_ref(),
            optional_filter_specs: prepared.effective_params.optional_filter_specs.as_deref(),
            facets: context.facets,
            distinct: context.distinct,
            max_values_per_facet: context.max_values_per_facet,
            effective_limit: context.effective_limit,
            facet_cache_key: context.facet_cache_key,
            allow_split_alternatives: context.allow_split_alternatives,
            query_text: context.query_text,
            json_exact_field: prepared.json_exact_field,
            searchable_paths: &prepared.searchable_paths,
        },
    )?;

    sort_with_stage2_ranking(
        &mut all_results,
        context.effective_sort,
        resolved,
        preprocessed,
        prepared,
        context.opts,
    );

    let total = resolve_total_hits(&query_totals, all_results.len(), context.effective_limit);
    let ruled_result = apply_rule_effects(
        all_results,
        total,
        resolved,
        prepared,
        context.query_text,
        context.max_values_per_facet,
    )?;

    Ok(RankedSearchOutput {
        ruled_result,
        facet_result,
    })
}

pub(super) fn build_execution_limits(
    prepared: &PreparedSearchFilters,
    preprocessed: &PreprocessedQuery,
) -> (usize, bool) {
    let hidden_window_padding = prepared
        .rule_effects
        .as_ref()
        .map(|effects| effects.hidden.iter().collect::<HashSet<_>>().len())
        .unwrap_or(0);
    let effective_limit = prepared
        .effective_params
        .limit
        .saturating_add(prepared.effective_params.offset)
        .saturating_add(hidden_window_padding)
        .max(50);
    #[cfg(feature = "decompound")]
    let allow_split_alternatives =
        preprocessed.decompound_enabled || preprocessed.decompound_langs.is_empty();
    #[cfg(not(feature = "decompound"))]
    let allow_split_alternatives = true;
    #[cfg(not(feature = "decompound"))]
    let _ = preprocessed;

    (effective_limit, allow_split_alternatives)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const METRICS_TENANT_ID: &str = "tenant_zero_limit_metrics";

    fn prepared_filters_with_window(limit: usize, offset: usize) -> PreparedSearchFilters {
        let mut schema_builder = tantivy::schema::Schema::builder();
        let json_search_field =
            schema_builder.add_text_field("_json_search", tantivy::schema::TEXT);
        let json_exact_field = schema_builder.add_text_field("_json_exact", tantivy::schema::TEXT);
        let schema = schema_builder.build();

        PreparedSearchFilters {
            effective_params: EffectiveSearchParams {
                filter: None,
                limit,
                offset,
                restrict_searchable_attrs: None,
                optional_filter_specs: None,
                sum_or_filters_scores: false,
                exact_on_single_word_query: "attribute".to_string(),
                disable_exact_on_attributes: Vec::new(),
                around_lat_lng: None,
                around_radius: None,
            },
            query_text_rewritten: "pagination".to_string(),
            parsed_query: "pagination".to_string(),
            expanded_queries: vec!["pagination".to_string()],
            searchable_paths: vec!["title".to_string()],
            field_weights: vec![1.0],
            schema,
            json_search_field,
            json_exact_field,
            rule_effects: None,
            synonym_store: None,
        }
    }

    fn preprocessed_query() -> PreprocessedQuery {
        PreprocessedQuery {
            query_text_stopped: "pagination".to_string(),
            plural_map: None,
            custom_normalization: Vec::new(),
            query_type: "prefixLast".to_string(),
            effective_exact_on_single_word_query: "attribute".to_string(),
            effective_disable_exact_on_attributes: Vec::new(),
            #[cfg(feature = "decompound")]
            decompound_enabled: false,
            #[cfg(feature = "decompound")]
            decompound_langs: Vec::new(),
        }
    }

    fn metrics_document(id: &str, title: &str, brand: &str) -> crate::types::Document {
        crate::types::Document {
            id: id.to_string(),
            fields: HashMap::from([
                (
                    "title".to_string(),
                    crate::types::FieldValue::Text(title.to_string()),
                ),
                (
                    "brand".to_string(),
                    crate::types::FieldValue::Text(brand.to_string()),
                ),
            ]),
        }
    }

    async fn build_zero_limit_metrics_manager() -> (TempDir, Arc<IndexManager>, IndexSettings) {
        let temp_dir = TempDir::new().unwrap();
        let manager = super::super::IndexManager::new(temp_dir.path());
        manager.create_tenant(METRICS_TENANT_ID).unwrap();
        let settings = IndexSettings {
            attributes_for_faceting: vec!["brand".to_string()],
            searchable_attributes: Some(vec!["title".to_string()]),
            ..IndexSettings::default()
        };
        settings
            .save(
                &temp_dir
                    .path()
                    .join(METRICS_TENANT_ID)
                    .join(super::super::config::SETTINGS_FILE),
            )
            .unwrap();
        manager.invalidate_settings_cache(METRICS_TENANT_ID);
        manager
            .add_documents_sync(
                METRICS_TENANT_ID,
                vec![
                    metrics_document("one", "running shoe", "Nike"),
                    metrics_document("two", "trail shoe", "Adidas"),
                ],
            )
            .await
            .unwrap();
        (temp_dir, manager, settings)
    }

    #[test]
    fn build_execution_limits_uses_minimum_candidate_floor_for_stable_pagination() {
        let preprocessed = preprocessed_query();

        let first_page = prepared_filters_with_window(10, 0);
        let later_page = prepared_filters_with_window(10, 20);
        let wider_page = prepared_filters_with_window(40, 20);

        assert_eq!(build_execution_limits(&first_page, &preprocessed).0, 50);
        assert_eq!(build_execution_limits(&later_page, &preprocessed).0, 50);
        assert_eq!(build_execution_limits(&wider_page, &preprocessed).0, 60);
    }

    #[tokio::test]
    async fn zero_limit_production_searches_emit_count_only_query_phase_metrics() {
        let (_temp_dir, manager, settings) = build_zero_limit_metrics_manager().await;

        let (count_result, count_reports) =
            crate::query::executor::capture_query_phase_reports(|| {
                manager.search_with_options(
                    METRICS_TENANT_ID,
                    "shoe",
                    &SearchOptions {
                        limit: 0,
                        settings_override: Some(&settings),
                        ..SearchOptions::default()
                    },
                )
            });
        let count_result = count_result.unwrap();

        assert!(count_result.documents.is_empty());
        assert_eq!(count_result.total, 2);
        assert_eq!(count_reports.len(), 1);
        assert_eq!(count_reports[0].execution_path, "count_only");
        assert_eq!(count_reports[0].matched_docs, 2);
        assert!(count_reports[0].collect_ns > 0);
        assert_eq!(count_reports[0].facet_extract_ns, 0);

        let facet_requests = vec![crate::types::FacetRequest {
            field: "brand".to_string(),
            path: "/brand".to_string(),
            value_query: None,
        }];
        let (facet_result, facet_reports) =
            crate::query::executor::capture_query_phase_reports(|| {
                manager.search_with_options(
                    METRICS_TENANT_ID,
                    "shoe",
                    &SearchOptions {
                        limit: 0,
                        facets: Some(&facet_requests),
                        settings_override: Some(&settings),
                        ..SearchOptions::default()
                    },
                )
            });
        let facet_result = facet_result.unwrap();

        assert!(facet_result.documents.is_empty());
        assert_eq!(facet_result.total, 2);
        assert_eq!(facet_result.facets["brand"].len(), 2);
        assert_eq!(facet_reports.len(), 1);
        assert_eq!(facet_reports[0].execution_path, "count_only");
        assert_eq!(facet_reports[0].matched_docs, 2);
        assert!(facet_reports[0].collect_ns > 0);
        assert!(facet_reports[0].facet_extract_ns > 0);
    }

    #[tokio::test]
    async fn zero_limit_empty_facet_slice_preserves_count_only_result() {
        let (_temp_dir, manager, settings) = build_zero_limit_metrics_manager().await;
        let empty_facet_requests = [];

        let result = manager
            .search_with_options(
                METRICS_TENANT_ID,
                "shoe",
                &SearchOptions {
                    limit: 0,
                    facets: Some(&empty_facet_requests),
                    settings_override: Some(&settings),
                    ..SearchOptions::default()
                },
            )
            .unwrap();

        assert!(result.documents.is_empty());
        assert_eq!(result.total, 2);
        assert!(result.facets.is_empty());
        assert!(result.facets_stats.is_empty());
        assert!(result.exhaustive_facet_values);
    }

    #[tokio::test]
    async fn production_ranked_search_phase_report_reconciles_short_query() {
        const OPTIONAL_FILTER_COUNT: usize = 20_000;

        let (_temp_dir, manager, settings) = build_zero_limit_metrics_manager().await;
        let optional_filter_groups = (0..OPTIONAL_FILTER_COUNT)
            .map(|index| vec![("brand".to_string(), format!("missing_{index}"), 1.0)])
            .collect::<Vec<_>>();

        let (result, reports) = crate::query::executor::capture_query_phase_reports(|| {
            manager.search_with_options(
                METRICS_TENANT_ID,
                "sh",
                &SearchOptions {
                    limit: 2,
                    optional_filter_specs: Some(&optional_filter_groups),
                    settings_override: Some(&settings),
                    ..SearchOptions::default()
                },
            )
        });
        let result = result.unwrap();

        assert_eq!(result.total, 2);
        assert_eq!(reports.len(), 1);
        let report = reports[0];
        assert_eq!(report.execution_path, "relevance");
        assert_eq!(report.matched_docs, 2);
        assert_eq!(report.visited_segments, 1);
        assert_eq!(report.candidates_collected, 2);
        assert!(report.prepare_ns > 0);
        assert!(report.collect_ns > 0);
        assert!(report.fetch_ns > 0);
        let attributed_ns = report
            .prepare_ns
            .checked_add(report.collect_ns)
            .and_then(|total| total.checked_add(report.rank_ns))
            .and_then(|total| total.checked_add(report.fetch_ns))
            .and_then(|total| total.checked_add(report.facet_extract_ns))
            .expect("attributed query phases must not overflow");
        let unattributed_ns = report
            .total_ns
            .checked_sub(attributed_ns)
            .expect("attributed query phases must not exceed total time");
        assert_eq!(
            attributed_ns + unattributed_ns,
            report.total_ns,
            "ranked production report must reconcile exactly"
        );
        // Scheduler jitter below 2ms is noise; slower queries allow at most 5%.
        let residual_budget_ns = 2_000_000_u64.max(report.total_ns / 20);
        assert!(
            unattributed_ns <= residual_budget_ns,
            "production ranked-search unattributed {unattributed_ns}ns exceeds \
             {residual_budget_ns}ns for report {report:?}"
        );
    }
}
