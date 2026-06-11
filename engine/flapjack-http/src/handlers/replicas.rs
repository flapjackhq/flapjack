use super::AppState;
use flapjack::error::FlapjackError;
use flapjack::index::manager::validate_index_name;
use flapjack::index::replica::{parse_replica_entry, standard_replica_names, ReplicaEntry};
use flapjack::index::settings::IndexSettings;
use flapjack::types::Document;
use std::collections::HashSet;
use std::sync::Arc;

/// Load standard (non-virtual) replica names configured on a primary index.
pub(crate) fn standard_replicas_for_primary(
    state: &Arc<AppState>,
    primary_index_name: &str,
) -> Result<Vec<String>, FlapjackError> {
    // Replica settings live under the tenant data tree, so reject invalid names
    // before any filesystem join can escape the configured base path.
    validate_index_name(primary_index_name)?;
    let settings_path = state
        .manager
        .base_path
        .join(primary_index_name)
        .join("settings.json");
    if !settings_path.exists() {
        return Ok(Vec::new());
    }

    let settings = IndexSettings::load(&settings_path)?;
    Ok(settings
        .replicas
        .as_ref()
        .map(|replicas| standard_replica_names(replicas))
        .unwrap_or_default())
}

pub(crate) fn has_physical_index_data(state: &Arc<AppState>, index_name: &str) -> bool {
    if validate_index_name(index_name).is_err() {
        return false;
    }
    state
        .manager
        .base_path
        .join(index_name)
        .join("meta.json")
        .exists()
}

pub(crate) fn is_virtual_settings_only_index(state: &Arc<AppState>, index_name: &str) -> bool {
    let Some(settings) = state.manager.get_settings(index_name) else {
        return false;
    };
    settings.primary.is_some() && !has_physical_index_data(state, index_name)
}

pub(crate) fn reject_writes_to_virtual_replica(
    state: &Arc<AppState>,
    index_name: &str,
) -> Result<(), FlapjackError> {
    if is_virtual_settings_only_index(state, index_name) {
        return Err(FlapjackError::InvalidQuery(
            "Virtual replica indices are read-only. Write to the primary index instead."
                .to_string(),
        ));
    }
    Ok(())
}

pub(crate) struct ResolvedSearchTarget {
    pub data_index: String,
    pub settings_override: Option<IndexSettings>,
}

/// Determine which physical index to query and whether to apply a settings override.
///
/// For a virtual replica (has a `primary` link but no physical Tantivy data),
/// redirects the search to the primary index's data while returning the
/// virtual replica's settings as an override. For all other indexes the
/// requested name is used directly with no override.
///
/// # Arguments
///
/// * `state` - Shared application state containing the index manager.
/// * `requested_index` - The index name the caller asked to search.
///
/// # Returns
///
/// A `ResolvedSearchTarget` with the physical `data_index` to read from and
/// an optional `settings_override` when the request targeted a virtual replica.
pub(crate) fn resolve_search_target(
    state: &Arc<AppState>,
    requested_index: &str,
) -> ResolvedSearchTarget {
    let Some(settings) = state.manager.get_settings(requested_index) else {
        return ResolvedSearchTarget {
            data_index: requested_index.to_string(),
            settings_override: None,
        };
    };

    let Some(primary_index) = &settings.primary else {
        return ResolvedSearchTarget {
            data_index: requested_index.to_string(),
            settings_override: None,
        };
    };

    if has_physical_index_data(state, requested_index) {
        return ResolvedSearchTarget {
            data_index: requested_index.to_string(),
            settings_override: None,
        };
    }

    ResolvedSearchTarget {
        data_index: primary_index.clone(),
        settings_override: Some(settings.as_ref().clone()),
    }
}

/// Persist the read-only `primary` link for each configured replica.
///
/// Standard replicas are physical indexes (create_tenant), virtual replicas are
/// settings-only directories with `settings.json` and no Tantivy data files.
pub(crate) fn persist_replica_primary_links(
    state: &Arc<AppState>,
    primary_index_name: &str,
    replicas: &[ReplicaEntry],
) -> Result<(), FlapjackError> {
    validate_index_name(primary_index_name)?;
    for replica in replicas {
        let replica_name = replica.name();
        validate_index_name(replica_name)?;
        match replica {
            ReplicaEntry::Standard(_) => state.manager.create_tenant(replica_name)?,
            ReplicaEntry::Virtual(_) => {
                std::fs::create_dir_all(state.manager.base_path.join(replica_name))?;
            }
        }

        let settings_path = state
            .manager
            .base_path
            .join(replica_name)
            .join("settings.json");
        let mut settings = if settings_path.exists() {
            IndexSettings::load(&settings_path)?
        } else {
            IndexSettings::default()
        };
        settings.primary = Some(primary_index_name.to_string());
        settings.save(&settings_path)?;
        state.manager.invalidate_settings_cache(replica_name);
        state.manager.invalidate_facet_cache(replica_name);
    }
    Ok(())
}

/// Remove `primary` link from replicas removed from a primary's replicas list.
pub(crate) fn clear_removed_replica_primary_links(
    state: &Arc<AppState>,
    primary_index_name: &str,
    previous_replicas: Option<&[String]>,
    next_replicas: &[ReplicaEntry],
) -> Result<(), FlapjackError> {
    validate_index_name(primary_index_name)?;
    let previous = previous_replicas.unwrap_or(&[]);
    if previous.is_empty() {
        return Ok(());
    }

    let next_names: HashSet<String> = next_replicas
        .iter()
        .map(|entry| entry.name().to_string())
        .collect();

    for old in previous {
        let Ok(parsed) = parse_replica_entry(old) else {
            continue;
        };
        let replica_name = parsed.name();
        if next_names.contains(replica_name) {
            continue;
        }

        let settings_path = state
            .manager
            .base_path
            .join(replica_name)
            .join("settings.json");
        if !settings_path.exists() {
            continue;
        }

        let mut settings = IndexSettings::load(&settings_path)?;
        if settings.primary.as_deref() == Some(primary_index_name) {
            settings.primary = None;
            settings.save(&settings_path)?;
            state.manager.invalidate_settings_cache(replica_name);
            state.manager.invalidate_facet_cache(replica_name);
        }
    }
    Ok(())
}

/// Mirror add/update writes from primary to all configured standard replicas.
pub(crate) async fn sync_add_documents_to_standard_replicas(
    state: &Arc<AppState>,
    primary_index_name: &str,
    documents: &[Document],
) -> Result<(), FlapjackError> {
    if documents.is_empty() {
        return Ok(());
    }

    let replica_names = standard_replicas_for_primary(state, primary_index_name)?;
    for replica_name in replica_names {
        state.manager.create_tenant(&replica_name)?;
        state
            .manager
            .add_documents_sync(&replica_name, documents.to_vec())
            .await?;
    }
    Ok(())
}

/// Mirror deletes from primary to all configured standard replicas.
pub(crate) async fn sync_delete_documents_to_standard_replicas(
    state: &Arc<AppState>,
    primary_index_name: &str,
    object_ids: &[String],
) -> Result<(), FlapjackError> {
    if object_ids.is_empty() {
        return Ok(());
    }

    let replica_names = standard_replicas_for_primary(state, primary_index_name)?;
    for replica_name in replica_names {
        state.manager.create_tenant(&replica_name)?;
        state
            .manager
            .delete_documents_durable(&replica_name, object_ids.to_vec())
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashmap::DashMap;
    use flapjack::dictionaries::manager::DictionaryManager;
    use flapjack::recommend::RecommendConfig;
    use flapjack::IndexManager;
    use tempfile::TempDir;

    fn make_state(base: &std::path::Path) -> Arc<AppState> {
        let manager = IndexManager::new(base);
        let dictionary_manager = Arc::new(DictionaryManager::new(base));
        manager.set_dictionary_manager(Arc::clone(&dictionary_manager));

        Arc::new(AppState {
            manager,
            key_store: None,
            replication_manager: None,
            ssl_manager: None,
            analytics_engine: None,
            recommend_config: RecommendConfig::default(),
            experiment_store: None,
            dictionary_manager,
            metrics_state: None,
            usage_counters: Arc::new(DashMap::new()),
            usage_persistence: None,
            paused_indexes: crate::pause_registry::PausedIndexes::new(),
            geoip_reader: None,
            notification_service: None,
            start_time: std::time::Instant::now(),
            conversation_store: crate::conversation_store::ConversationStore::default_shared(),
            embedder_store: Arc::new(crate::embedder_store::EmbedderStore::new()),
            idempotency_cache: Arc::new(
                crate::idempotency::IdempotencyCache::from_env_with_data_dir(base),
            ),
        })
    }

    #[tokio::test]
    async fn standard_replicas_for_primary_rejects_path_traversal_name() {
        let temp_dir = TempDir::new().expect("temp dir");
        let state = make_state(temp_dir.path());

        let err = standard_replicas_for_primary(&state, "../escape").expect_err("must reject");

        assert!(
            matches!(err, FlapjackError::InvalidQuery(_)),
            "expected invalid query error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn persist_replica_primary_links_rejects_path_traversal_primary() {
        let temp_dir = TempDir::new().expect("temp dir");
        let state = make_state(temp_dir.path());

        let err = persist_replica_primary_links(
            &state,
            "../escape",
            &[ReplicaEntry::Virtual("replica_virtual".to_string())],
        )
        .expect_err("must reject");

        assert!(
            matches!(err, FlapjackError::InvalidQuery(_)),
            "expected invalid query error, got {err:?}"
        );
    }
}
