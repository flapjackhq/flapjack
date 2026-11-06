//! Query parameter resolution and stopword filtering for search operations, including filter merging, language selection, and stopword removal with dictionary manager fallback.
use super::*;

pub(super) struct EffectiveSearchParams {
    pub(super) filter: Option<Filter>,
    pub(super) limit: usize,
    pub(super) offset: usize,
    pub(super) restrict_searchable_attrs: Option<Vec<String>>,
    pub(super) optional_filter_specs: Option<Vec<Vec<(String, String, f32)>>>,
    pub(super) sum_or_filters_scores: bool,
    pub(super) exact_on_single_word_query: String,
    pub(super) disable_exact_on_attributes: Vec<String>,
    pub(super) around_lat_lng: Option<String>,
    pub(super) around_radius: Option<serde_json::Value>,
}

/// Merge two filters into a single AND filter, flattening nested AND clauses when possible.
///
/// # Behavior
/// If either input is already Filter::And, its components are combined into a single flat Filter::And. This avoids deeply nested AND structures.
pub(super) fn merge_filters_with_and(left: Filter, right: Filter) -> Filter {
    match (left, right) {
        (Filter::And(mut left_parts), Filter::And(mut right_parts)) => {
            left_parts.append(&mut right_parts);
            Filter::And(left_parts)
        }
        (Filter::And(mut left_parts), right_filter) => {
            left_parts.push(right_filter);
            Filter::And(left_parts)
        }
        (left_filter, Filter::And(mut right_parts)) => {
            let mut merged = Vec::with_capacity(right_parts.len() + 1);
            merged.push(left_filter);
            merged.append(&mut right_parts);
            Filter::And(merged)
        }
        (left_filter, right_filter) => Filter::And(vec![left_filter, right_filter]),
    }
}

fn merge_filter(current: &mut Option<Filter>, next: Filter) {
    *current = Some(match current.take() {
        Some(existing) => merge_filters_with_and(existing, next),
        None => next,
    });
}

fn merge_parsed_filters<T>(
    current: &mut Option<Filter>,
    values: &[T],
    parse: impl Fn(&T) -> Option<Filter>,
) {
    for value in values {
        if let Some(parsed) = parse(value) {
            merge_filter(current, parsed);
        }
    }
}

/// Groups input parameters for `build_effective_search_params`.
pub(super) struct SearchParamsInput<'a> {
    pub(super) request_filter: Option<&'a Filter>,
    pub(super) request_limit: usize,
    pub(super) request_offset: usize,
    pub(super) request_restrict_searchable_attrs: Option<&'a [String]>,
    pub(super) request_optional_filter_specs: super::OptionalFilterSpecs<'a>,
    pub(super) sum_or_filters_scores: bool,
    pub(super) exact_on_single_word_query_override: Option<&'a str>,
    pub(super) disable_exact_on_attributes_override: Option<&'a [String]>,
    pub(super) configured_facet_set: Option<&'a std::collections::HashSet<String>>,
    pub(super) rule_effects: Option<&'a RuleEffects>,
    pub(super) hits_per_page_cap: Option<usize>,
}

/// Construct effective search parameters by merging request parameters with rule effects, facet filters, numeric filters, tag filters, and optional filters, then apply any hits_per_page caps.
///
/// # Arguments
/// - `input` - Container with request parameters, rule effects, and configuration overrides.
///
/// # Returns
/// EffectiveSearchParams with merged and capped values, or error if rule filter expressions are invalid.
///
/// # Behavior
/// Rule effects are applied in order: filters, facet filters, numeric filters, tag filters, generated facet filters (with disjunctive grouping), and optional filters. Disjunctive filters for the same attribute are grouped into OR clauses; non-disjunctive filters are AND-merged individually. Limit and offset are adjusted based on rule hits_per_page or hits_per_page_cap, preserving the original page number.
pub(super) fn build_effective_search_params(
    input: &SearchParamsInput<'_>,
) -> Result<EffectiveSearchParams> {
    let request_filter = input.request_filter;
    let request_limit = input.request_limit;
    let request_offset = input.request_offset;
    let request_restrict_searchable_attrs = input.request_restrict_searchable_attrs;
    let request_optional_filter_specs = input.request_optional_filter_specs;
    let sum_or_filters_scores = input.sum_or_filters_scores;
    let exact_on_single_word_query_override = input.exact_on_single_word_query_override;
    let disable_exact_on_attributes_override = input.disable_exact_on_attributes_override;
    let configured_facet_set = input.configured_facet_set;
    let rule_effects = input.rule_effects;
    let hits_per_page_cap = input.hits_per_page_cap;
    let request_page = request_offset.checked_div(request_limit).unwrap_or(0);
    let mut filter = request_filter.cloned();
    let mut limit = request_limit;
    let mut offset = request_offset;
    let mut restrict_searchable_attrs =
        request_restrict_searchable_attrs.map(|attrs| attrs.to_vec());
    let mut optional_filter_specs = request_optional_filter_specs
        .map(|specs| specs.to_vec())
        .unwrap_or_default();
    let mut around_lat_lng = None;
    let mut around_radius = None;

    if let Some(effects) = rule_effects {
        if let Some(rule_filter_expr) = effects.filters.as_deref() {
            let parsed_rule_filter =
                crate::filter_parser::parse_filter(rule_filter_expr).map_err(|e| {
                    FlapjackError::InvalidQuery(format!(
                        "Invalid rule params.filters expression '{}': {}",
                        rule_filter_expr, e
                    ))
                })?;
            merge_filter(&mut filter, parsed_rule_filter);
        }

        merge_parsed_filters(&mut filter, &effects.facet_filters, facet_filters_to_ast);
        merge_parsed_filters(
            &mut filter,
            &effects.numeric_filters,
            numeric_filters_to_ast,
        );
        merge_parsed_filters(&mut filter, &effects.tag_filters, tag_filters_to_ast);

        // Wire generated mandatory facet filters from automaticFacetFilters.
        // Disjunctive filters for the same facet attribute are grouped into OR clauses;
        // non-disjunctive filters are AND-merged individually.
        {
            let mut disjunctive_groups: HashMap<String, Vec<Filter>> = HashMap::new();
            for gff in &effects.generated_facet_filters {
                let facet_attr = gff
                    .expression
                    .trim_start_matches("NOT ")
                    .split(':')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !configured_facet_set.is_some_and(|facets| facets.contains(&facet_attr)) {
                    continue;
                }
                let parsed = crate::filter_parser::parse_filter(&gff.expression).map_err(|e| {
                    FlapjackError::InvalidQuery(format!(
                        "Invalid generated automatic facet filter expression '{}': {}",
                        gff.expression, e
                    ))
                })?;
                if gff.disjunctive {
                    disjunctive_groups
                        .entry(facet_attr)
                        .or_default()
                        .push(parsed);
                } else {
                    merge_filter(&mut filter, parsed);
                }
            }
            // Merge each disjunctive group as an OR clause
            for (_attr, filters) in disjunctive_groups {
                let or_filter = match filters.len() {
                    0 => continue,
                    1 => filters.into_iter().next().unwrap(),
                    _ => Filter::Or(filters),
                };
                merge_filter(&mut filter, or_filter);
            }
        }

        for optional_filters in &effects.optional_filters {
            optional_filter_specs.extend(parse_optional_filters_grouped(optional_filters));
        }

        // Wire generated optional facet filters from automaticOptionalFacetFilters
        for (facet, value, score) in &effects.generated_optional_facet_filters {
            if !configured_facet_set.is_some_and(|facets| facets.contains(facet)) {
                continue;
            }
            optional_filter_specs.push(vec![(facet.clone(), value.clone(), *score as f32)]);
        }

        if let Some(rule_hits_per_page) = effects
            .hits_per_page
            .filter(|hits_per_page| request_limit > 0 && *hits_per_page > 0)
        {
            limit = rule_hits_per_page;
            offset = request_page.saturating_mul(rule_hits_per_page);
        }

        if let Some(rule_restrict_searchable_attrs) =
            effects.restrict_searchable_attributes.as_ref()
        {
            restrict_searchable_attrs = Some(rule_restrict_searchable_attrs.clone());
        }

        if let Some(rule_around_lat_lng) = effects.around_lat_lng.as_ref() {
            around_lat_lng = Some(rule_around_lat_lng.clone());
        }

        if let Some(rule_around_radius) = effects.around_radius.as_ref() {
            around_radius = Some(rule_around_radius.clone());
        }
    }

    if let Some(cap) = hits_per_page_cap.filter(|cap| *cap > 0) {
        if limit > cap {
            limit = cap;
            offset = request_page.saturating_mul(cap);
        }
    }

    Ok(EffectiveSearchParams {
        filter,
        limit,
        offset,
        restrict_searchable_attrs,
        optional_filter_specs: (!optional_filter_specs.is_empty()).then_some(optional_filter_specs),
        sum_or_filters_scores,
        exact_on_single_word_query: exact_on_single_word_query_override
            .unwrap_or("attribute")
            .to_string(),
        disable_exact_on_attributes: disable_exact_on_attributes_override.unwrap_or(&[]).to_vec(),
        around_lat_lng,
        around_radius,
    })
}

pub(super) fn canonical_query_language(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(code) = trimmed.parse::<crate::language::LanguageCode>() {
        return Some(code.as_str().to_string());
    }
    Some(trimmed.to_ascii_lowercase())
}

pub(super) fn normalize_query_languages(query_languages: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    for raw in query_languages {
        if let Some(lang) = canonical_query_language(raw) {
            if seen.insert(lang.clone()) {
                normalized.push(lang);
            }
        }
    }
    normalized
}

/// Determine which languages' stopwords to remove based on the configured stopword removal setting.
///
/// # Returns
/// Empty vector if stopword removal is disabled; the normalized query languages (or ["en"] if empty) if set to All; otherwise the normalized languages from the setting.
pub(super) fn resolve_stopword_languages(
    setting: &crate::query::stopwords::RemoveStopWordsValue,
    normalized_query_languages: &[String],
) -> Vec<String> {
    match setting {
        crate::query::stopwords::RemoveStopWordsValue::Disabled => Vec::new(),
        crate::query::stopwords::RemoveStopWordsValue::All => {
            if normalized_query_languages.is_empty() {
                vec!["en".to_string()]
            } else {
                normalized_query_languages.to_vec()
            }
        }
        crate::query::stopwords::RemoveStopWordsValue::Languages(langs) => {
            normalize_query_languages(langs)
        }
    }
}

/// Remove stopwords from a query using languages from the dictionary manager, with fallback to built-in stopwords, preserving prefix tokens and trailing whitespace.
///
/// # Arguments
/// - `query` - The search query string to filter.
/// - `setting` - The stopword removal configuration (disabled, all languages, or specific languages).
/// - `query_type` - Either "prefixAll", "prefixLast", or other; determines which tokens are prefix tokens (excluded from stopword removal).
/// - `normalized_query_languages` - Pre-normalized query language codes.
/// - `dictionary_manager` - Optional manager for loading custom stopwords; falls back to built-in if unavailable.
///
/// # Behavior
/// Prefix tokens (all words for "prefixAll", last word for "prefixLast" without trailing space) are never removed. Stopwords are loaded from the dictionary manager first, falling back to built-in stopwords if custom lookup fails. Preserves the original trailing space if present.
pub(super) fn remove_stop_words_with_dictionary_manager(
    query: &str,
    setting: &crate::query::stopwords::RemoveStopWordsValue,
    query_type: &str,
    normalized_query_languages: &[String],
    dictionary_manager: Option<&crate::dictionaries::manager::DictionarySnapshot>,
    tenant_id: &str,
) -> String {
    let dict_tenant = tenant_id;
    let langs = resolve_stopword_languages(setting, normalized_query_languages);
    if langs.is_empty() {
        return query.to_string();
    }

    let mut all_stop_words: HashSet<String> = HashSet::new();
    for lang in langs {
        if let Some(dm) = dictionary_manager {
            match dm.effective_stopwords(dict_tenant, &lang) {
                Ok(words) => {
                    all_stop_words.extend(words.into_iter().map(|w| w.to_lowercase()));
                    continue;
                }
                Err(err) => {
                    tracing::warn!(
                        language = %lang,
                        error = %err,
                        "Failed to load custom stopwords; falling back to built-in stopwords"
                    );
                }
            }
        }
        if let Some(sw) = crate::query::stopwords::stopwords_for_lang(&lang) {
            all_stop_words.extend(sw.into_iter().map(|w| w.to_string()));
        }
    }

    if all_stop_words.is_empty() {
        return query.to_string();
    }

    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return query.to_string();
    }

    let trailing_space = query.ends_with(' ');
    let last_idx = words.len().saturating_sub(1);
    let filtered: Vec<&str> = words
        .iter()
        .enumerate()
        .filter_map(|(i, w)| {
            let is_prefix_token = match query_type {
                "prefixAll" => true,
                "prefixLast" => i == last_idx && !trailing_space,
                _ => false,
            };
            if is_prefix_token || !all_stop_words.contains(w.to_lowercase().as_str()) {
                Some(*w)
            } else {
                None
            }
        })
        .collect();

    if filtered.is_empty() {
        return query.to_string();
    }

    let mut out = filtered.join(" ");
    if trailing_space {
        out.push(' ');
    }
    out
}
