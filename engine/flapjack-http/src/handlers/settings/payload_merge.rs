use axum::http::StatusCode;
use std::collections::HashMap;

use super::SetSettingsRequest;
use flapjack::index::replica::{validate_replicas, ReplicaEntry};
use flapjack::index::settings::{
    detect_embedder_changes, DistinctValue, EmbedderChange, IndexMode, IndexSettings,
};

/// Merge a partial settings payload into the current settings state, returning
/// any validated replica entries that need follow-up link maintenance.
pub(super) fn merge_settings_payload(
    settings: &mut IndexSettings,
    mut payload: SetSettingsRequest,
    index_name: &str,
) -> Result<Option<Vec<ReplicaEntry>>, (StatusCode, String)> {
    merge_non_topology_settings_payload(settings, &mut payload)?;

    let validated_replicas =
        validate_and_apply_replicas(settings, payload.replicas.take(), index_name)?;
    warn_neural_without_embedders(settings);
    Ok(validated_replicas)
}

pub(in crate::handlers) fn merge_non_topology_settings_payload(
    settings: &mut IndexSettings,
    payload: &mut SetSettingsRequest,
) -> Result<(), (StatusCode, String)> {
    if let Some(distinct_value) = parse_distinct_value_strict(payload.distinct.take())
        .map_err(|message| (StatusCode::BAD_REQUEST, message.to_string()))?
    {
        settings.distinct = Some(distinct_value);
    }

    apply_search_config_fields(settings, payload);
    apply_response_and_display_fields(settings, payload);
    apply_embedders_update(settings, payload.embedders.take());
    Ok(())
}

/// Log embedder additions, removals, and mutations after a settings merge.
pub(super) fn log_embedder_changes(
    old: &Option<HashMap<String, serde_json::Value>>,
    settings: &IndexSettings,
) {
    for change in detect_embedder_changes(old, &settings.embedders) {
        match change {
            EmbedderChange::Modified(name) => {
                let name = log_safe_embedder_name(&name);
                tracing::warn!(
                    "embedder '{}' configuration changed; existing vectors may be stale",
                    name
                );
            }
            EmbedderChange::Removed(name) => {
                let name = log_safe_embedder_name(&name);
                tracing::warn!(
                    "embedder '{}' removed; associated vectors will be orphaned",
                    name
                );
            }
            EmbedderChange::Added(name) => {
                let name = log_safe_embedder_name(&name);
                tracing::info!("embedder '{}' configured", name);
            }
        }
    }
}

fn log_safe_embedder_name(name: &str) -> String {
    name.escape_debug().to_string()
}

/// Applies search-behavior fields from a settings payload to the index settings.
///
/// Covers fields that affect indexing, ranking, query interpretation, and search mode (16 fields).
/// Each field is consumed via `.take()` so the caller retains ownership of remaining fields.
fn apply_search_config_fields(settings: &mut IndexSettings, payload: &mut SetSettingsRequest) {
    if let Some(v) = payload.attributes_for_faceting.take() {
        settings.attributes_for_faceting = v;
    }
    if let Some(v) = payload.searchable_attributes.take() {
        settings.searchable_attributes = Some(v);
    }
    if let Some(v) = payload.ranking.take() {
        settings.ranking = Some(v);
    }
    if let Some(v) = payload.custom_ranking.take() {
        settings.custom_ranking = Some(v);
    }
    if let Some(v) = payload.pagination_limited_to.take() {
        settings.pagination_limited_to = v;
    }
    if let Some(v) = payload.remove_stop_words.take() {
        settings.remove_stop_words = v;
    }
    if let Some(v) = payload.ignore_plurals.take() {
        settings.ignore_plurals = v;
    }
    if let Some(v) = payload.query_languages.take() {
        settings.query_languages = v;
    }
    if let Some(v) = payload.query_type.take() {
        settings.query_type = v;
    }
    if let Some(v) = payload.hits_per_page.take() {
        settings.hits_per_page = v;
    }
    if let Some(v) = payload.min_word_size_for_1_typo.take() {
        settings.min_word_size_for_1_typo = v;
    }
    if let Some(v) = payload.min_word_size_for_2_typos.take() {
        settings.min_word_size_for_2_typos = v;
    }
    if let Some(v) = payload.max_values_per_facet.take() {
        settings.max_values_per_facet = v;
    }
    if let Some(v) = payload.exact_on_single_word_query.take() {
        settings.exact_on_single_word_query = v;
    }
    if let Some(v) = payload.remove_words_if_no_results.take() {
        settings.remove_words_if_no_results = v;
    }
    if let Some(v) = payload.separators_to_index.take() {
        settings.separators_to_index = v;
    }
    if let Some(v) = payload.alternatives_as_exact.take() {
        settings.alternatives_as_exact = v;
    }
    if let Some(v) = payload.numeric_attributes_for_filtering.take() {
        settings.numeric_attributes_for_filtering = Some(v);
    }
    if let Some(v) = payload.attributes_to_index.take() {
        settings.attributes_to_index = Some(v);
    }
    if let Some(v) = payload.allow_compression_of_integer_array.take() {
        settings.allow_compression_of_integer_array = Some(v);
    }
    if let Some(v) = payload.relevancy_strictness.take() {
        settings.relevancy_strictness = Some(v);
    }
    if let Some(v) = payload.mode.take() {
        settings.mode = Some(v);
    }
    if let Some(v) = payload.semantic_search.take() {
        settings.semantic_search = Some(v);
    }
    if let Some(v) = payload.enable_personalization.take() {
        settings.enable_personalization = Some(v);
    }
}

/// Applies response-shape and display-behavior fields from a settings payload.
///
/// Covers fields that control attribute retrieval, highlighting, snippeting,
/// proximity tuning, typo tolerance overrides, and miscellaneous display options (17 fields).
/// Each field is consumed via `.take()` so the caller retains ownership of remaining fields.
fn apply_response_and_display_fields(
    settings: &mut IndexSettings,
    payload: &mut SetSettingsRequest,
) {
    if let Some(v) = payload.attributes_to_retrieve.take() {
        settings.attributes_to_retrieve = Some(v);
    }
    if let Some(v) = payload.unretrievable_attributes.take() {
        settings.unretrievable_attributes = Some(v);
    }
    if let Some(v) = payload.attributes_to_highlight.take() {
        settings.attributes_to_highlight = Some(v);
    }
    if let Some(v) = payload.attributes_to_snippet.take() {
        settings.attributes_to_snippet = Some(v);
    }
    if let Some(v) = payload.highlight_pre_tag.take() {
        settings.highlight_pre_tag = Some(v);
    }
    if let Some(v) = payload.highlight_post_tag.take() {
        settings.highlight_post_tag = Some(v);
    }
    if let Some(v) = payload.optional_words.take() {
        settings.optional_words = v;
    }
    if let Some(v) = payload.attribute_for_distinct.take() {
        settings.attribute_for_distinct = Some(v);
    }
    if let Some(v) = payload.rendering_content.take() {
        settings.rendering_content = Some(v);
    }
    if let Some(v) = payload.user_data.take() {
        settings.user_data = Some(v);
    }
    if let Some(v) = payload.enable_rules.take() {
        settings.enable_rules = Some(v);
    }
    if let Some(v) = payload.advanced_syntax_features.take() {
        settings.advanced_syntax_features = Some(v);
    }
    if let Some(v) = payload.sort_facet_values_by.take() {
        settings.sort_facet_values_by = Some(v);
    }
    if let Some(v) = payload.snippet_ellipsis_text.take() {
        settings.snippet_ellipsis_text = Some(v);
    }
    if let Some(v) = payload.restrict_highlight_and_snippet_arrays.take() {
        settings.restrict_highlight_and_snippet_arrays = Some(v);
    }
    if let Some(v) = payload.min_proximity.take() {
        settings.min_proximity = Some(v);
    }
    if let Some(v) = payload.disable_exact_on_attributes.take() {
        settings.disable_exact_on_attributes = Some(v);
    }
    if let Some(v) = payload.replace_synonyms_in_highlight.take() {
        settings.replace_synonyms_in_highlight = Some(v);
    }
    if let Some(v) = payload.attribute_criteria_computed_by_min_proximity.take() {
        settings.attribute_criteria_computed_by_min_proximity = Some(v);
    }
    if let Some(v) = payload.enable_re_ranking.take() {
        settings.enable_re_ranking = Some(v);
    }
    if let Some(v) = payload.disable_typo_tolerance_on_words.take() {
        settings.disable_typo_tolerance_on_words = Some(v);
    }
    if let Some(v) = payload.disable_typo_tolerance_on_attributes.take() {
        settings.disable_typo_tolerance_on_attributes = Some(v);
    }
}

pub(in crate::handlers) fn parse_distinct_value_strict(
    raw: Option<serde_json::Value>,
) -> Result<Option<DistinctValue>, &'static str> {
    let Some(value) = raw else {
        return Ok(None);
    };

    match value {
        serde_json::Value::Bool(is_distinct) => Ok(Some(DistinctValue::Bool(is_distinct))),
        serde_json::Value::Number(number) => {
            let distinct_count = number
                .as_u64()
                .filter(|value| u32::try_from(*value).is_ok())
                .ok_or("distinct must be a boolean or a non-negative u32 integer")?;
            Ok(Some(DistinctValue::Integer(distinct_count as u32)))
        }
        _ => Err("distinct must be a boolean or a non-negative u32 integer"),
    }
}

fn apply_embedders_update(
    settings: &mut IndexSettings,
    embedders: Option<HashMap<String, serde_json::Value>>,
) {
    if let Some(map) = embedders {
        let filtered: HashMap<String, serde_json::Value> = map
            .into_iter()
            .filter(|(_, value)| !value.is_null())
            .collect();
        settings.embedders = (!filtered.is_empty()).then_some(filtered);
    }
}

/// Validates replica index names (must differ from the primary and use valid `virtual()` syntax) and returns parsed replica entries for creation.
pub(in crate::handlers) fn validate_and_apply_replicas(
    settings: &mut IndexSettings,
    replicas: Option<Vec<String>>,
    index_name: &str,
) -> Result<Option<Vec<ReplicaEntry>>, (StatusCode, String)> {
    let replicas = match replicas {
        Some(replicas) => replicas,
        None => return Ok(None),
    };
    if replicas.is_empty() {
        settings.replicas = None;
        return Ok(Some(Vec::new()));
    }
    let parsed = validate_replicas(index_name, &replicas)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    settings.replicas = Some(replicas);
    Ok(Some(parsed))
}

fn warn_neural_without_embedders(settings: &IndexSettings) {
    if settings.mode == Some(IndexMode::NeuralSearch) && settings.embedders.is_none() {
        tracing::warn!(
            "mode set to neuralSearch but no embedders configured; hybrid search will fall back to keyword-only until embedders are added"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::log_safe_embedder_name;

    #[test]
    fn log_safe_embedder_name_escapes_control_characters() {
        assert_eq!(
            log_safe_embedder_name("public\nfake=entry\tprovider"),
            "public\\nfake=entry\\tprovider"
        );
    }
}
