//! Stub summary for promote.rs.
use super::*;

/// Apply the winning variant's settings to the main index. Mode B copies all settings from a dedicated variant index. Mode A applies promotable fields (custom_ranking, remove_words_if_no_results) from query overrides, logging any query-time-only fields that cannot be persisted. Invalidates the main index's settings cache.
///
/// # Arguments
///
/// * `state` - Application state with index manager and base path
/// * `experiment` - The concluded experiment with variant settings to promote
///
/// # Returns
///
/// `Ok(())` on successful promotion, or `Err(msg)` if settings files cannot be loaded or saved.
pub(super) fn promote_variant_settings(
    state: &AppState,
    experiment: &Experiment,
) -> Result<(), String> {
    let main_index = &experiment.index_name;

    if let Some(variant_index) = experiment.variant.index_name.as_deref() {
        promote_mode_b_settings(state, main_index, variant_index)?;
    } else if let Some(overrides) = experiment.variant.query_overrides.as_ref() {
        promote_mode_a_overrides(state, main_index, overrides)?;
    }

    Ok(())
}

/// TODO: Document promote_mode_b_settings.
fn promote_mode_b_settings(
    state: &AppState,
    main_index: &str,
    variant_index: &str,
) -> Result<(), String> {
    use flapjack::index::settings::IndexSettings;

    flapjack::validate_index_name(main_index).map_err(|e| format!("invalid index name: {}", e))?;
    flapjack::validate_index_name(variant_index)
        .map_err(|e| format!("invalid index name: {}", e))?;

    let variant_settings_path = state
        .manager
        .base_path
        .join(variant_index)
        .join("settings.json");
    let main_settings_path = state
        .manager
        .base_path
        .join(main_index)
        .join("settings.json");

    let variant_settings = IndexSettings::load(&variant_settings_path)
        .map_err(|error| format!("failed to load variant index settings: {}", error))?;
    variant_settings
        .save(&main_settings_path)
        .map_err(|error| format!("failed to save promoted settings: {}", error))?;
    state.manager.invalidate_settings_cache(main_index);

    tracing::info!(
        "promoted Mode B settings from {} to {}",
        variant_index,
        main_index
    );
    Ok(())
}

/// TODO: Document promote_mode_a_overrides.
fn promote_mode_a_overrides(
    state: &AppState,
    main_index: &str,
    overrides: &QueryOverrides,
) -> Result<(), String> {
    use flapjack::index::settings::IndexSettings;

    flapjack::validate_index_name(main_index).map_err(|e| format!("invalid index name: {}", e))?;

    let main_settings_path = state
        .manager
        .base_path
        .join(main_index)
        .join("settings.json");

    let mut settings = IndexSettings::load(&main_settings_path)
        .map_err(|error| format!("failed to load main index settings: {}", error))?;

    if let Some(custom_ranking) = overrides.custom_ranking.as_ref() {
        settings.custom_ranking = Some(custom_ranking.clone());
    }
    if let Some(remove_words_if_no_results) = overrides.remove_words_if_no_results.as_ref() {
        settings.remove_words_if_no_results = remove_words_if_no_results.clone();
    }

    let query_only_fields = collect_query_only_override_fields(overrides);
    if !query_only_fields.is_empty() {
        tracing::warn!(
            "Mode A promote: skipping query-time-only fields {:?} (no index-level equivalent)",
            query_only_fields
        );
    }

    settings
        .save(&main_settings_path)
        .map_err(|error| format!("failed to save promoted settings: {}", error))?;
    state.manager.invalidate_settings_cache(main_index);

    tracing::info!("promoted Mode A overrides to index {}", main_index);
    Ok(())
}

/// TODO: Document collect_query_only_override_fields.
fn collect_query_only_override_fields(overrides: &QueryOverrides) -> Vec<&'static str> {
    [
        overrides.typo_tolerance.as_ref().map(|_| "typoTolerance"),
        overrides.enable_synonyms.as_ref().map(|_| "enableSynonyms"),
        overrides.enable_rules.as_ref().map(|_| "enableRules"),
        overrides.rule_contexts.as_ref().map(|_| "ruleContexts"),
        overrides.filters.as_ref().map(|_| "filters"),
        overrides
            .optional_filters
            .as_ref()
            .map(|_| "optionalFilters"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestStateBuilder;
    use flapjack::index::settings::IndexSettings;
    use tempfile::TempDir;

    fn make_test_state(tmp: &TempDir) -> std::sync::Arc<AppState> {
        TestStateBuilder::new(tmp).with_experiments().build_shared()
    }

    /// TODO: Document promote_mode_b_rejects_path_traversal_variant_index.
    #[tokio::test]
    async fn promote_mode_b_rejects_path_traversal_variant_index() {
        let tmp = TempDir::new().unwrap();
        let state = make_test_state(&tmp);

        state.manager.create_tenant("products").unwrap();

        let main_settings_path = tmp.path().join("products").join("settings.json");
        let main_before = IndexSettings::load(&main_settings_path).unwrap();
        assert!(main_before.custom_ranking.is_none());

        let escape_dir = tmp.path().join("../escape_promote_mode_b");
        std::fs::create_dir_all(&escape_dir).unwrap();
        let escape_settings_path = escape_dir.join("settings.json");
        let mut escape_settings = main_before.clone();
        escape_settings.custom_ranking = Some(vec!["desc(leaked)".to_string()]);
        escape_settings.save(&escape_settings_path).unwrap();

        let error = promote_mode_b_settings(&state, "products", "../escape_promote_mode_b")
            .expect_err("path traversal variant index should be rejected");
        assert!(
            error.contains("invalid index name"),
            "unexpected error for traversal attempt: {error}"
        );

        let main_after = IndexSettings::load(&main_settings_path).unwrap();
        assert!(
            main_after.custom_ranking.is_none(),
            "rejected traversal must not overwrite the main index settings"
        );
    }

    /// TODO: Document promote_mode_a_rejects_path_traversal_main_index.
    #[tokio::test]
    async fn promote_mode_a_rejects_path_traversal_main_index() {
        let tmp = TempDir::new().unwrap();
        let state = make_test_state(&tmp);

        let error = promote_mode_a_overrides(
            &state,
            "../escape_promote_mode_a",
            &QueryOverrides {
                custom_ranking: Some(vec!["desc(popularity)".to_string()]),
                ..Default::default()
            },
        )
        .expect_err("path traversal main index should be rejected");
        assert!(
            error.contains("invalid index name"),
            "unexpected error for traversal attempt: {error}"
        );
    }

    /// TODO: Document collect_query_only_override_fields_returns_only_query_time_keys.
    #[test]
    fn collect_query_only_override_fields_returns_only_query_time_keys() {
        let overrides = QueryOverrides {
            typo_tolerance: Some(serde_json::json!("strict")),
            enable_synonyms: Some(true),
            enable_rules: Some(false),
            rule_contexts: Some(vec!["promo".to_string()]),
            filters: Some("brand:apple".to_string()),
            optional_filters: Some(vec!["price<1000".to_string()]),
            custom_ranking: Some(vec!["desc(popularity)".to_string()]),
            attribute_weights: Some(std::collections::HashMap::from([(
                "name".to_string(),
                2.0_f32,
            )])),
            remove_words_if_no_results: Some("lastWords".to_string()),
        };

        let query_only_fields = collect_query_only_override_fields(&overrides);
        assert_eq!(
            query_only_fields,
            vec![
                "typoTolerance",
                "enableSynonyms",
                "enableRules",
                "ruleContexts",
                "filters",
                "optionalFilters",
            ]
        );
    }
}
