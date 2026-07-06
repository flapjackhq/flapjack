use super::*;

mod plural_expansion;
mod query_execution;
mod search_helpers;

#[cfg(feature = "decompound")]
use plural_expansion::apply_decompound;
use plural_expansion::build_plural_map;
use query_execution::{FacetResultCache, PreparedSearchFilters};
use search_helpers::{
    build_execution_limits, build_search_parser, execute_ranked_search, execute_zero_limit_search,
    RankedSearchContext, ZeroLimitSearchContext,
};

/// Resolved index, searcher, and settings for a tenant search.
pub(super) struct ResolvedSearch {
    pub index: Arc<Index>,
    pub searcher: tantivy::Searcher,
    pub settings: Option<Arc<IndexSettings>>,
    pub relevance_config: RelevanceConfig,
}

/// Preprocessed query state produced before the main search loop.
pub(super) struct PreprocessedQuery {
    pub query_text_stopped: String,
    pub plural_map: Option<HashMap<String, Vec<String>>>,
    pub custom_normalization: Vec<(char, String)>,
    pub query_type: String,
    pub effective_exact_on_single_word_query: String,
    pub effective_disable_exact_on_attributes: Vec<String>,
    #[cfg(feature = "decompound")]
    pub decompound_enabled: bool,
    #[cfg(feature = "decompound")]
    pub decompound_langs: Vec<String>,
}

impl super::IndexManager {
    /// Load the tenant index, acquire a searcher, and resolve settings.
    pub(super) fn resolve_search_settings(
        &self,
        tenant_id: &str,
        settings_override: Option<&IndexSettings>,
    ) -> Result<ResolvedSearch> {
        let index = self.get_or_load(tenant_id)?;
        let reader = index.reader();
        let searcher = reader.searcher();
        let settings = settings_override
            .map(|s| Arc::new(s.clone()))
            .or_else(|| self.get_settings(tenant_id));
        if let Some(ref s) = settings {
            tracing::debug!("[SEARCH] Loaded settings query_type={}", s.query_type);
        }
        let relevance_config = RelevanceConfig {
            searchable_attributes: settings
                .as_ref()
                .and_then(|s| s.searchable_attributes.clone()),
            attribute_weights: HashMap::new(),
        };
        Ok(ResolvedSearch {
            index,
            searcher,
            settings,
            relevance_config,
        })
    }
}

pub(super) fn preprocess_query(
    tenant_id: &str,
    settings: &Option<Arc<IndexSettings>>,
    query_text: &str,
    opts: &SearchOptions<'_>,
    dictionary_manager: Option<&Arc<crate::dictionaries::manager::DictionaryManager>>,
) -> PreprocessedQuery {
    let qt = opts
        .query_type
        .unwrap_or_else(|| {
            settings
                .as_ref()
                .map(|s| s.query_type.as_str())
                .unwrap_or("prefixLast")
        })
        .to_string();

    let effective_exact_on_single_word_query = opts
        .exact_on_single_word_query
        .or(settings
            .as_ref()
            .map(|s| s.exact_on_single_word_query.as_str()))
        .unwrap_or("attribute")
        .to_string();

    let effective_disable_exact_on_attributes = opts
        .disable_exact_on_attributes
        .or(settings
            .as_ref()
            .and_then(|s| s.disable_exact_on_attributes.as_deref()))
        .unwrap_or(&[])
        .to_vec();

    let effective_stop_words = opts
        .remove_stop_words
        .or(settings.as_ref().map(|s| &s.remove_stop_words));
    let raw_effective_query_languages = opts
        .query_languages
        .map(|v| v.as_slice())
        .or(settings.as_ref().map(|s| s.query_languages.as_slice()))
        .unwrap_or(&[]);
    let effective_query_languages = normalize_query_languages(raw_effective_query_languages);

    let query_text_stopped = match effective_stop_words {
        Some(sw) => remove_stop_words_with_dictionary_manager(
            query_text,
            sw,
            &qt,
            &effective_query_languages,
            dictionary_manager,
            tenant_id,
        ),
        None => query_text.to_string(),
    };

    let custom_normalization: Vec<(char, String)> = settings
        .as_deref()
        .map(IndexSettings::flatten_custom_normalization)
        .unwrap_or_default();

    let plural_map = build_plural_map(
        tenant_id,
        settings,
        opts.ignore_plurals,
        &effective_query_languages,
        &query_text_stopped,
        dictionary_manager,
    );

    // Decompound processing: split compound words in Germanic languages.
    #[cfg(feature = "decompound")]
    let decompound_keep_diacritics = settings
        .as_ref()
        .map(|s| s.keep_diacritics_on_characters.as_str())
        .unwrap_or("");
    #[cfg(feature = "decompound")]
    let decompound_langs: Vec<String> = effective_query_languages
        .iter()
        .filter(|l| crate::query::decompound::supports_decompound(l))
        .cloned()
        .collect();
    #[cfg(feature = "decompound")]
    let decompound_enabled = opts.decompound_query.unwrap_or(true);
    #[cfg(feature = "decompound")]
    let plural_map = if decompound_enabled && !decompound_langs.is_empty() {
        apply_decompound(
            tenant_id,
            &query_text_stopped,
            plural_map,
            &decompound_langs,
            decompound_keep_diacritics,
            &custom_normalization,
            dictionary_manager,
        )
    } else {
        plural_map
    };
    #[cfg(not(feature = "decompound"))]
    let _ = opts.decompound_query; // suppress unused warning

    PreprocessedQuery {
        query_text_stopped,
        plural_map,
        custom_normalization,
        query_type: qt,
        effective_exact_on_single_word_query,
        effective_disable_exact_on_attributes,
        #[cfg(feature = "decompound")]
        decompound_enabled,
        #[cfg(feature = "decompound")]
        decompound_langs,
    }
}

/// Prepare rule-filter merges, searchable-path weighting, and parser inputs.
pub(super) fn prepare_search_filters(
    manager: &super::IndexManager,
    tenant_id: &str,
    query_text: &str,
    resolved: &ResolvedSearch,
    preprocessed: &PreprocessedQuery,
    opts: &SearchOptions<'_>,
) -> Result<PreparedSearchFilters> {
    let SearchOptions {
        filter,
        limit,
        offset,
        enable_synonyms,
        enable_rules,
        rule_contexts,
        restrict_searchable_attrs,
        optional_filter_specs,
        sum_or_filters_scores,
        secured_hits_per_page_cap,
        ..
    } = *opts;

    let rules_enabled = enable_rules.unwrap_or(true);
    let synonyms_enabled = enable_synonyms.unwrap_or(true);
    let synonym_store = if synonyms_enabled {
        manager.get_synonyms(tenant_id)
    } else {
        None
    };

    let (query_text_rewritten, rule_effects) = if rules_enabled {
        if let Some(store) = manager.get_rules(tenant_id) {
            let effects =
                store.apply_rules(query_text, rule_contexts, filter, synonym_store.as_deref());
            let rewritten = effects
                .rewritten_query
                .clone()
                .unwrap_or_else(|| query_text.to_string());
            (rewritten, Some(effects))
        } else {
            (query_text.to_string(), None)
        }
    } else {
        (query_text.to_string(), None)
    };

    let configured_facet_set = resolved.settings.as_ref().map(|s| s.facet_set());
    let effective_params = build_effective_search_params(&SearchParamsInput {
        request_filter: filter,
        request_limit: limit,
        request_offset: offset,
        request_restrict_searchable_attrs: restrict_searchable_attrs,
        request_optional_filter_specs: optional_filter_specs,
        sum_or_filters_scores,
        exact_on_single_word_query_override: Some(
            preprocessed.effective_exact_on_single_word_query.as_str(),
        ),
        disable_exact_on_attributes_override: Some(
            preprocessed
                .effective_disable_exact_on_attributes
                .as_slice(),
        ),
        configured_facet_set: configured_facet_set.as_ref(),
        rule_effects: rule_effects.as_ref(),
        hits_per_page_cap: secured_hits_per_page_cap,
    })?;

    let parsed_query = query_text_rewritten.clone();
    let expanded_queries = if synonyms_enabled {
        if let Some(store) = synonym_store.as_ref() {
            store.expand_query(&query_text_rewritten)
        } else {
            vec![query_text_rewritten.clone()]
        }
    } else {
        vec![query_text_rewritten.clone()]
    };

    let schema = resolved.index.inner().schema();
    let json_search_field = schema
        .get_field("_json_search")
        .map_err(|_| FlapjackError::FieldNotFound("_json_search".to_string()))?;
    let json_exact_field = schema
        .get_field("_json_exact")
        .map_err(|_| FlapjackError::FieldNotFound("_json_exact".to_string()))?;

    let all_searchable_paths: Vec<String> = resolved.index.searchable_paths();
    let (searchable_paths, field_weights): (Vec<String>, Vec<f32>) =
        build_searchable_paths_with_weights(
            &all_searchable_paths,
            resolved.relevance_config.searchable_attributes.as_deref(),
        );

    let (searchable_paths, field_weights) = apply_restrict_searchable_attrs(
        searchable_paths,
        field_weights,
        effective_params.restrict_searchable_attrs.as_deref(),
    );

    Ok(PreparedSearchFilters {
        effective_params,
        query_text_rewritten,
        parsed_query,
        expanded_queries,
        searchable_paths,
        field_weights,
        schema,
        json_search_field,
        json_exact_field,
        rule_effects,
        synonym_store,
    })
}

pub(super) fn execute_search_query(
    manager: &super::IndexManager,
    tenant_id: &str,
    query_text: &str,
    resolved: &ResolvedSearch,
    preprocessed: &PreprocessedQuery,
    prepared: &PreparedSearchFilters,
    opts: &SearchOptions<'_>,
) -> Result<SearchResult> {
    let SearchOptions {
        sort,
        facets,
        distinct,
        max_values_per_facet,
        ..
    } = *opts;

    let effective_params = &prepared.effective_params;

    let default_sort_owned = if sort.is_none() && query_text.trim().is_empty() {
        Some(Sort::ByField {
            field: "objectID".to_string(),
            order: crate::types::SortOrder::Desc,
        })
    } else {
        None
    };
    let effective_sort: Option<&Sort> = sort.or(default_sort_owned.as_ref());
    let parser = build_search_parser(resolved, preprocessed, prepared, opts);
    let (facet_cache_key, facet_result) = lookup_cached_facets(
        manager,
        tenant_id,
        resolved,
        prepared,
        facets,
        max_values_per_facet,
    );

    let effective_around_lat_lng = effective_params.around_lat_lng.clone();
    let effective_around_radius = effective_params.around_radius.clone();
    if effective_params.limit == 0 {
        return execute_zero_limit_search(
            manager,
            resolved,
            prepared,
            &parser,
            ZeroLimitSearchContext {
                facets,
                max_values_per_facet,
                facet_cache_key: facet_cache_key.as_ref(),
                facet_result,
                effective_around_lat_lng,
                effective_around_radius,
            },
        );
    }

    let (effective_limit, allow_split_alternatives) =
        build_execution_limits(prepared, preprocessed);

    let ranked_search = execute_ranked_search(
        manager,
        resolved,
        preprocessed,
        prepared,
        &parser,
        RankedSearchContext {
            opts,
            effective_sort,
            facets,
            distinct,
            max_values_per_facet,
            query_text,
            facet_cache_key: facet_cache_key.as_ref(),
            facet_result,
            effective_limit,
            allow_split_alternatives,
        },
    )?;

    Ok(finalize_search_result(
        prepared,
        ranked_search.ruled_result,
        ranked_search.facet_result,
        effective_around_lat_lng,
        effective_around_radius,
    ))
}

struct RuleEffectsResult {
    documents: Vec<crate::types::ScoredDocument>,
    total: usize,
    user_data: Vec<serde_json::Value>,
    applied_rules: Vec<String>,
    rendering_content: Option<serde_json::Value>,
}

fn lookup_cached_facets(
    manager: &super::IndexManager,
    tenant_id: &str,
    resolved: &ResolvedSearch,
    prepared: &PreparedSearchFilters,
    facets: Option<&[crate::types::FacetRequest]>,
    max_values_per_facet: Option<usize>,
) -> (Option<String>, Option<FacetResultCache>) {
    let Some(facet_reqs) = facets else {
        return (None, None);
    };

    // Cached entries are trimmed to maxValuesPerFacet and keyed without the
    // value query — a facet-value search must always scan the full value set.
    if facet_reqs
        .iter()
        .any(|req| req.value_query.as_deref().is_some_and(|q| !q.is_empty()))
    {
        return (None, None);
    }

    let mut facet_keys: Vec<String> = facet_reqs.iter().map(|req| req.field.clone()).collect();
    facet_keys.sort();
    let filter_hash = prepared
        .effective_params
        .filter
        .as_ref()
        .map(|filter| format!("{filter:?}"))
        .unwrap_or_default();
    let cache_key = format!("{}:{}:{}", tenant_id, filter_hash, facet_keys.join(","));
    let cached_result = manager.facet_cache.get(&cache_key).and_then(|cached| {
        let (timestamp, count, facets_map, facets_stats, cached_exhaustive_facets) =
            cached.as_ref();
        if timestamp.elapsed() >= std::time::Duration::from_secs(5) {
            tracing::debug!(
                "[FACET_CACHE] STALE ({}ms old)",
                timestamp.elapsed().as_millis()
            );
            return None;
        }

        tracing::debug!(
            "[FACET_CACHE] HIT ({}ms old)",
            timestamp.elapsed().as_millis()
        );
        let executor = QueryExecutor::new(resolved.index.converter(), prepared.schema.clone())
            .with_settings(resolved.settings.clone())
            .with_query(prepared.query_text_rewritten.clone())
            .with_max_values_per_facet(max_values_per_facet);
        let trimmed = executor.trim_facet_counts(facets_map.clone(), facet_reqs);
        let trimmed_further = facets_map.iter().any(|(field, original_counts)| {
            let trimmed_len = trimmed.get(field).map(|values| values.len()).unwrap_or(0);
            trimmed_len < original_counts.len()
        });
        Some((
            *count,
            trimmed,
            facets_stats.clone(),
            *cached_exhaustive_facets && !trimmed_further,
        ))
    });

    (Some(cache_key), cached_result)
}

fn sort_with_stage2_ranking(
    all_results: &mut Vec<crate::types::ScoredDocument>,
    effective_sort: Option<&Sort>,
    resolved: &ResolvedSearch,
    preprocessed: &PreprocessedQuery,
    prepared: &PreparedSearchFilters,
    opts: &SearchOptions<'_>,
) {
    if !matches!(effective_sort, None | Some(Sort::ByRelevance)) {
        return;
    }

    let SearchOptions {
        all_query_words_optional,
        relevancy_strictness,
        min_proximity,
        ranking_synonym_store,
        ranking_plural_map,
        ..
    } = *opts;
    sort_results_with_stage2_ranking(
        all_results,
        Stage2RankingContext {
            query_text: &prepared.parsed_query,
            searchable_paths: &prepared.searchable_paths,
            settings: resolved.settings.as_deref(),
            synonym_store: ranking_synonym_store.or(prepared.synonym_store.as_deref()),
            plural_map: ranking_plural_map.or(preprocessed.plural_map.as_ref()),
            query_type: preprocessed.query_type.as_str(),
            optional_filter_groups: prepared.effective_params.optional_filter_specs.as_deref(),
            sum_or_filters_scores: prepared.effective_params.sum_or_filters_scores,
            exact_on_single_word_query: &prepared.effective_params.exact_on_single_word_query,
            disable_exact_on_attributes: &prepared.effective_params.disable_exact_on_attributes,
            custom_normalization: &preprocessed.custom_normalization,
            keep_diacritics_on_characters: resolved
                .settings
                .as_ref()
                .map(|settings| settings.keep_diacritics_on_characters.as_str())
                .unwrap_or(""),
            camel_case_attributes: resolved
                .settings
                .as_ref()
                .map(|settings| settings.camel_case_attributes.as_slice())
                .unwrap_or(&[]),
            all_query_words_optional,
            relevancy_strictness,
            min_proximity,
        },
    );
}

fn apply_rule_effects(
    all_results: Vec<crate::types::ScoredDocument>,
    total: usize,
    resolved: &ResolvedSearch,
    prepared: &PreparedSearchFilters,
    query_text: &str,
    max_values_per_facet: Option<usize>,
) -> Result<RuleEffectsResult> {
    let Some(effects) = prepared.rule_effects.as_ref() else {
        return Ok(RuleEffectsResult {
            documents: all_results,
            total,
            user_data: Vec::new(),
            applied_rules: Vec::new(),
            rendering_content: None,
        });
    };

    let hidden_count = if effects.hidden.is_empty() {
        0
    } else {
        let hidden_ids: HashSet<&str> = effects.hidden.iter().map(|id| id.as_str()).collect();
        all_results
            .iter()
            .filter(|doc| hidden_ids.contains(doc.document.id.as_str()))
            .count()
    };
    let executor = QueryExecutor::new(resolved.index.converter(), prepared.schema.clone())
        .with_settings(resolved.settings.clone())
        .with_query(query_text.to_string())
        .with_max_values_per_facet(max_values_per_facet);
    let documents = executor.apply_rules_to_results(
        &resolved.searcher,
        all_results,
        effects,
        prepared.effective_params.filter.as_ref(),
    )?;

    Ok(RuleEffectsResult {
        documents,
        total: total.saturating_sub(hidden_count),
        user_data: effects.user_data.clone(),
        applied_rules: effects.applied_rules.clone(),
        rendering_content: effects.rendering_content.clone(),
    })
}

fn finalize_search_result(
    prepared: &PreparedSearchFilters,
    ruled_result: RuleEffectsResult,
    facet_result: Option<FacetResultCache>,
    effective_around_lat_lng: Option<String>,
    effective_around_radius: Option<serde_json::Value>,
) -> SearchResult {
    let ruled_count = ruled_result.documents.len();
    let start = prepared.effective_params.offset.min(ruled_count);
    let end = (start + prepared.effective_params.limit).min(ruled_count);
    let final_docs = ruled_result.documents[start..end].to_vec();
    let (facets_map, facets_stats, exhaustive_facet_values) = match facet_result {
        Some((_, facets, stats, exhaustive_facets)) => (facets, stats, exhaustive_facets),
        None => (HashMap::new(), HashMap::new(), true),
    };

    SearchResult {
        documents: final_docs,
        total: ruled_result.total,
        facets: facets_map,
        facets_stats,
        user_data: ruled_result.user_data,
        applied_rules: ruled_result.applied_rules,
        parsed_query: prepared.parsed_query.clone(),
        exhaustive_facet_values,
        exhaustive_rules_match: true,
        query_after_removal: None,
        rendering_content: ruled_result.rendering_content,
        effective_around_lat_lng,
        effective_around_radius,
    }
}

/// Filter the searchable paths and weights to only include attributes listed in
/// `restrictSearchableAttributes`. Falls back to the full set if the restriction
/// yields no matches.
fn apply_restrict_searchable_attrs(
    searchable_paths: Vec<String>,
    field_weights: Vec<f32>,
    restrict_searchable_attrs: Option<&[String]>,
) -> (Vec<String>, Vec<f32>) {
    if let Some(restrict) = restrict_searchable_attrs {
        let mut filtered_paths = Vec::new();
        let mut filtered_weights = Vec::new();
        for (index, path) in searchable_paths.iter().enumerate() {
            if restrict.iter().any(|value| value == path) {
                filtered_paths.push(path.clone());
                filtered_weights.push(field_weights[index]);
            }
        }
        if filtered_paths.is_empty() {
            (searchable_paths, field_weights)
        } else {
            (filtered_paths, filtered_weights)
        }
    } else {
        (searchable_paths, field_weights)
    }
}

/// Assign exponentially decaying weights (100^-rank) to searchable paths based on
/// their position in `searchableAttributes`. Unranked paths receive a weight of 1.0.
pub(super) fn build_searchable_paths_with_weights(
    all_searchable_paths: &[String],
    searchable_attributes: Option<&[String]>,
) -> (Vec<String>, Vec<f32>) {
    match searchable_attributes {
        Some(attrs) => {
            let mut weighted: Vec<(String, f32)> = Vec::new();
            let mut unweighted: Vec<String> = Vec::new();
            let mut configured_ranks: HashMap<String, usize> = HashMap::new();
            let mut next_rank = 0usize;
            let mut unordered_rank: Option<usize> = None;
            for attr in attrs {
                let stripped = strip_unordered_prefix(attr);
                if configured_ranks.contains_key(stripped) {
                    continue;
                }

                let rank = if stripped != attr {
                    *unordered_rank.get_or_insert_with(|| {
                        let current = next_rank;
                        next_rank += 1;
                        current
                    })
                } else {
                    let current = next_rank;
                    next_rank += 1;
                    current
                };

                configured_ranks.insert(stripped.to_string(), rank);
            }

            for path in all_searchable_paths {
                if let Some(pos) = configured_ranks.get(path) {
                    weighted.push((path.clone(), 100_f32.powi(-(*pos as i32))));
                } else {
                    unweighted.push(path.clone());
                }
            }

            weighted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let mut paths: Vec<String> = weighted.iter().map(|(p, _)| p.clone()).collect();
            let mut weights: Vec<f32> = weighted.iter().map(|(_, w)| *w).collect();
            let min_weight = weights.last().copied().unwrap_or(1.0) * 0.01;
            for path in unweighted {
                paths.push(path);
                weights.push(min_weight);
            }
            (paths, weights)
        }
        None => {
            let weights = vec![1.0; all_searchable_paths.len()];
            (all_searchable_paths.to_vec(), weights)
        }
    }
}

/// Build the query after removal markup for removeWordsIfNoResults retry.
/// Kept words are plain, removed words are wrapped in `<em>` tags.
pub(super) fn apply_remove_words_fallback(
    manager: &super::IndexManager,
    tenant_id: &str,
    query_text: &str,
    remove_strategy: &str,
    opts: &SearchOptions<'_>,
) -> Option<SearchResult> {
    let words: Vec<&str> = query_text.split_whitespace().collect();
    if words.len() <= 1 {
        return None;
    }
    let fallback_queries: Vec<(String, usize)> = match remove_strategy {
        "lastWords" => (1..words.len())
            .map(|drop| (words[..words.len() - drop].join(" "), drop))
            .collect(),
        "firstWords" => (1..words.len())
            .map(|drop| (words[drop..].join(" "), drop))
            .collect(),
        _ => return None,
    };
    for (fallback_q, drop_count) in &fallback_queries {
        let retry_opts = SearchOptions {
            remove_words_if_no_results: Some("none"),
            ..*opts
        };
        if let Ok(mut retry) = manager.search_full_with_stop_words_with_hits_per_page_cap(
            tenant_id,
            fallback_q,
            &retry_opts,
        ) {
            if retry.total > 0 {
                retry.query_after_removal = Some(build_query_after_removal_markup(
                    &words,
                    remove_strategy,
                    *drop_count,
                ));
                return Some(retry);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn lookup_cached_facets_returns_cached_hit_with_computed_key() {
        let temp_dir = TempDir::new().unwrap();
        let manager = super::super::IndexManager::new(temp_dir.path());
        let tenant_id = "tenant_lookup_cached_facets";
        manager.create_tenant(tenant_id).unwrap();

        let facet_requests = vec![crate::types::FacetRequest {
            field: "brand".to_string(),
            path: "/brand".to_string(),
            value_query: None,
        }];
        let opts = SearchOptions {
            facets: Some(&facet_requests),
            ..SearchOptions::default()
        };
        let query_text = "shoe";

        let resolved = manager.resolve_search_settings(tenant_id, None).unwrap();
        let preprocessed = preprocess_query(
            tenant_id,
            &resolved.settings,
            query_text,
            &opts,
            manager.dictionary_manager(),
        );
        let prepared = prepare_search_filters(
            &manager,
            tenant_id,
            query_text,
            &resolved,
            &preprocessed,
            &opts,
        )
        .unwrap();

        let expected_cache_key = format!("{tenant_id}::brand");
        let cached_facets = HashMap::from([(
            "brand".to_string(),
            vec![crate::types::FacetCount {
                path: "Nike".to_string(),
                count: 1,
            }],
        )]);
        let cached_stats = HashMap::new();
        manager.facet_cache.insert(
            expected_cache_key.clone(),
            Arc::new((
                std::time::Instant::now(),
                1,
                cached_facets.clone(),
                cached_stats.clone(),
                true,
            )),
        );

        let (cache_key, cache_result) = lookup_cached_facets(
            &manager,
            tenant_id,
            &resolved,
            &prepared,
            opts.facets,
            opts.max_values_per_facet,
        );

        assert_eq!(cache_key, Some(expected_cache_key));
        let (count, facets, stats, exhaustive) = cache_result.expect("expected cached facets");
        assert_eq!(count, 1);
        assert_eq!(facets["brand"][0].path, "Nike");
        assert_eq!(stats, cached_stats);
        assert!(exhaustive);
    }
}
