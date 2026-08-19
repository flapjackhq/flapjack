//! Stub summary for replica_forwarding.rs.
use std::sync::Arc;

use super::settings_file_path;
use super::AppState;
use flapjack::index::settings::IndexSettings;

/// Forward the current settings from a primary index to all its configured replicas.
/// Preserves each replica's `primary` field (system-managed) while merging forwarded settings.
pub(super) fn forward_settings_to_replicas(
    state: &Arc<AppState>,
    primary_settings: &IndexSettings,
    attributes_for_faceting_provided: bool,
    query_languages_provided: bool,
) -> Result<(), flapjack::error::FlapjackError> {
    use flapjack::index::replica::parse_replica_entry;

    let Some(replicas) = &primary_settings.replicas else {
        return Ok(());
    };

    for replica_str in replicas {
        let replica_name = parse_replica_entry(replica_str)?.name().to_string();
        let settings_path = settings_file_path(&state.manager.base_path, &replica_name);
        if !settings_path.exists() {
            continue;
        }

        let mut replica_settings = IndexSettings::load(&settings_path)?;
        let preserved_primary = replica_settings.primary.clone();
        let preserved_replicas = replica_settings.replicas.clone();

        apply_forwarded_primary_fields(
            primary_settings,
            &mut replica_settings,
            attributes_for_faceting_provided,
            query_languages_provided,
        );

        replica_settings.primary = preserved_primary;
        replica_settings.replicas = preserved_replicas;

        replica_settings.save(&settings_path)?;
        state.manager.invalidate_settings_cache(&replica_name);
        state.manager.invalidate_facet_cache(&replica_name);
    }
    Ok(())
}

/// TODO: Document apply_forwarded_primary_fields.
fn apply_forwarded_primary_fields(
    primary_settings: &IndexSettings,
    replica_settings: &mut IndexSettings,
    attributes_for_faceting_provided: bool,
    query_languages_provided: bool,
) {
    if let Some(searchable_attributes) = &primary_settings.searchable_attributes {
        replica_settings.searchable_attributes = Some(searchable_attributes.clone());
    }
    if let Some(custom_ranking) = &primary_settings.custom_ranking {
        replica_settings.custom_ranking = Some(custom_ranking.clone());
    }
    if attributes_for_faceting_provided {
        replica_settings.attributes_for_faceting = primary_settings.attributes_for_faceting.clone();
    }
    if let Some(attributes_to_retrieve) = &primary_settings.attributes_to_retrieve {
        replica_settings.attributes_to_retrieve = Some(attributes_to_retrieve.clone());
    }
    if let Some(unretrievable_attributes) = &primary_settings.unretrievable_attributes {
        replica_settings.unretrievable_attributes = Some(unretrievable_attributes.clone());
    }
    if let Some(attribute_for_distinct) = &primary_settings.attribute_for_distinct {
        replica_settings.attribute_for_distinct = Some(attribute_for_distinct.clone());
    }
    if let Some(distinct) = &primary_settings.distinct {
        replica_settings.distinct = Some(distinct.clone());
    }
    if let Some(rendering_content) = &primary_settings.rendering_content {
        replica_settings.rendering_content = Some(rendering_content.clone());
    }
    if let Some(embedders) = &primary_settings.embedders {
        replica_settings.embedders = Some(embedders.clone());
    }
    if let Some(mode) = &primary_settings.mode {
        replica_settings.mode = Some(mode.clone());
    }
    if let Some(semantic_search) = &primary_settings.semantic_search {
        replica_settings.semantic_search = Some(semantic_search.clone());
    }
    if query_languages_provided {
        replica_settings.query_languages = primary_settings.query_languages.clone();
    }
    if let Some(numeric_attributes_for_filtering) =
        &primary_settings.numeric_attributes_for_filtering
    {
        replica_settings.numeric_attributes_for_filtering =
            Some(numeric_attributes_for_filtering.clone());
    }
}
