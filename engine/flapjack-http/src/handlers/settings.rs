use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(in crate::handlers) mod payload_merge;
mod replica_forwarding;

use self::payload_merge::{log_embedder_changes, merge_settings_payload};
use self::replica_forwarding::forward_settings_to_replicas;
use super::replicas::{
    clear_removed_replica_primary_links, is_virtual_settings_only_index,
    persist_replica_primary_links, standard_replicas_for_primary,
};
use super::AppState;
use crate::error_response::HandlerError;
use crate::extractors::ValidatedIndexName;
use flapjack::index::{
    settings::{IndexMode, IndexSettings, SemanticSearchSettings},
    SearchOptions,
};

const FACET_REINDEX_SCAN_PAGE_SIZE: usize = 1000;
const FACET_REINDEX_WRITE_BATCH_SIZE: usize = 250;

/// Deserializable payload for the `POST /1/indexes/{indexName}/settings` endpoint.
///
/// All fields are optional; only supplied fields are merged into the existing `IndexSettings`.
/// Unrecognized keys are captured in `other` via `#[serde(flatten)]` and reported as unsupported.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SetSettingsRequest {
    #[serde(rename = "attributesForFaceting")]
    pub attributes_for_faceting: Option<Vec<String>>,

    #[serde(rename = "searchableAttributes")]
    pub searchable_attributes: Option<Vec<String>>,

    #[serde(rename = "ranking")]
    pub ranking: Option<Vec<String>>,

    #[serde(rename = "customRanking")]
    pub custom_ranking: Option<Vec<String>>,

    #[serde(rename = "attributesToRetrieve")]
    pub attributes_to_retrieve: Option<Vec<String>>,

    #[serde(rename = "unretrievableAttributes")]
    pub unretrievable_attributes: Option<Vec<String>>,

    #[serde(rename = "attributesToHighlight")]
    pub attributes_to_highlight: Option<Vec<String>>,

    #[serde(rename = "attributesToSnippet")]
    pub attributes_to_snippet: Option<Vec<String>>,

    #[serde(rename = "highlightPreTag", skip_serializing_if = "Option::is_none")]
    pub highlight_pre_tag: Option<String>,

    #[serde(rename = "highlightPostTag", skip_serializing_if = "Option::is_none")]
    pub highlight_post_tag: Option<String>,

    #[serde(rename = "hitsPerPage", skip_serializing_if = "Option::is_none")]
    pub hits_per_page: Option<u32>,

    #[serde(
        rename = "minWordSizefor1Typo",
        skip_serializing_if = "Option::is_none"
    )]
    pub min_word_size_for_1_typo: Option<u32>,

    #[serde(
        rename = "minWordSizefor2Typos",
        skip_serializing_if = "Option::is_none"
    )]
    pub min_word_size_for_2_typos: Option<u32>,

    #[serde(rename = "maxValuesPerFacet", skip_serializing_if = "Option::is_none")]
    pub max_values_per_facet: Option<u32>,

    #[serde(rename = "paginationLimitedTo")]
    pub pagination_limited_to: Option<u32>,

    #[serde(rename = "attributeForDistinct")]
    pub attribute_for_distinct: Option<String>,

    pub distinct: Option<serde_json::Value>,

    #[serde(rename = "removeStopWords")]
    pub remove_stop_words: Option<flapjack::query::stopwords::RemoveStopWordsValue>,

    #[serde(rename = "ignorePlurals")]
    pub ignore_plurals: Option<flapjack::query::plurals::IgnorePluralsValue>,

    #[serde(rename = "queryLanguages")]
    pub query_languages: Option<Vec<String>>,

    #[serde(rename = "queryType")]
    pub query_type: Option<String>,

    #[serde(
        rename = "exactOnSingleWordQuery",
        skip_serializing_if = "Option::is_none"
    )]
    pub exact_on_single_word_query: Option<String>,

    #[serde(
        rename = "removeWordsIfNoResults",
        skip_serializing_if = "Option::is_none"
    )]
    pub remove_words_if_no_results: Option<String>,

    #[serde(rename = "separatorsToIndex", skip_serializing_if = "Option::is_none")]
    pub separators_to_index: Option<String>,

    #[serde(
        rename = "alternativesAsExact",
        skip_serializing_if = "Option::is_none"
    )]
    pub alternatives_as_exact: Option<Vec<String>>,

    #[serde(rename = "optionalWords", skip_serializing_if = "Option::is_none")]
    pub optional_words: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedders: Option<HashMap<String, serde_json::Value>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<IndexMode>,

    #[serde(rename = "semanticSearch", skip_serializing_if = "Option::is_none")]
    pub semantic_search: Option<SemanticSearchSettings>,

    #[serde(
        rename = "enablePersonalization",
        skip_serializing_if = "Option::is_none"
    )]
    pub enable_personalization: Option<bool>,

    #[serde(rename = "renderingContent", skip_serializing_if = "Option::is_none")]
    pub rendering_content: Option<serde_json::Value>,

    #[serde(rename = "userData", skip_serializing_if = "Option::is_none")]
    pub user_data: Option<serde_json::Value>,

    #[serde(rename = "enableRules", skip_serializing_if = "Option::is_none")]
    pub enable_rules: Option<bool>,

    #[serde(
        rename = "advancedSyntaxFeatures",
        skip_serializing_if = "Option::is_none"
    )]
    pub advanced_syntax_features: Option<Vec<String>>,

    #[serde(rename = "sortFacetValuesBy", skip_serializing_if = "Option::is_none")]
    pub sort_facet_values_by: Option<String>,

    #[serde(
        rename = "snippetEllipsisText",
        skip_serializing_if = "Option::is_none"
    )]
    pub snippet_ellipsis_text: Option<String>,

    #[serde(
        rename = "restrictHighlightAndSnippetArrays",
        skip_serializing_if = "Option::is_none"
    )]
    pub restrict_highlight_and_snippet_arrays: Option<bool>,

    #[serde(rename = "minProximity", skip_serializing_if = "Option::is_none")]
    pub min_proximity: Option<u32>,

    #[serde(
        rename = "disableExactOnAttributes",
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_exact_on_attributes: Option<Vec<String>>,

    #[serde(
        rename = "replaceSynonymsInHighlight",
        skip_serializing_if = "Option::is_none"
    )]
    pub replace_synonyms_in_highlight: Option<bool>,

    #[serde(
        rename = "attributeCriteriaComputedByMinProximity",
        skip_serializing_if = "Option::is_none"
    )]
    pub attribute_criteria_computed_by_min_proximity: Option<bool>,

    #[serde(rename = "enableReRanking", skip_serializing_if = "Option::is_none")]
    pub enable_re_ranking: Option<bool>,

    #[serde(
        rename = "disableTypoToleranceOnWords",
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_typo_tolerance_on_words: Option<Vec<String>>,

    #[serde(
        rename = "disableTypoToleranceOnAttributes",
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_typo_tolerance_on_attributes: Option<Vec<String>>,

    #[serde(rename = "replicas", skip_serializing_if = "Option::is_none")]
    pub replicas: Option<Vec<String>>,

    #[serde(
        rename = "numericAttributesForFiltering",
        alias = "numericAttributesToIndex",
        skip_serializing_if = "Option::is_none"
    )]
    pub numeric_attributes_for_filtering: Option<Vec<String>>,

    /// Deprecated Algolia alias of `searchableAttributes`. Persisted verbatim with no
    /// runtime behavior so legacy client payloads and migrated settings round-trip.
    #[serde(rename = "attributesToIndex", skip_serializing_if = "Option::is_none")]
    pub attributes_to_index: Option<Vec<String>>,

    #[serde(
        rename = "allowCompressionOfIntegerArray",
        skip_serializing_if = "Option::is_none"
    )]
    pub allow_compression_of_integer_array: Option<bool>,

    #[serde(
        rename = "relevancyStrictness",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub relevancy_strictness: Option<u32>,

    #[serde(flatten)]
    pub other: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SetSettingsResponse {
    #[serde(rename = "updatedAt")]
    pub updated_at: String,

    #[serde(rename = "taskID")]
    pub task_id: i64,

    #[serde(rename = "unsupportedParams", skip_serializing_if = "Option::is_none")]
    pub unsupported_params: Option<Vec<String>>,
}

fn settings_file_path(base_path: &Path, index_name: &str) -> PathBuf {
    base_path.join(index_name).join("settings.json")
}

fn load_settings_or_default(settings_path: &Path) -> Result<IndexSettings, HandlerError> {
    if settings_path.exists() {
        // Persisted settings parse failures are internal corruption and must be 500s.
        IndexSettings::load(settings_path).map_err(|error| match error {
            flapjack::error::FlapjackError::Json(parse_error) => HandlerError::from(format!(
                "Failed to parse persisted settings file '{}': {}",
                settings_path.display(),
                parse_error
            )),
            other => HandlerError::from(other.to_string()),
        })
    } else {
        Ok(IndexSettings::default())
    }
}

fn unsupported_settings_params(payload: &SetSettingsRequest) -> Vec<String> {
    let mut unsupported = Vec::new();
    if let Some(other) = &payload.other {
        unsupported.extend(other.keys().cloned());
    }
    unsupported
}

fn faceting_configuration_changed(
    previous_settings: &IndexSettings,
    next_settings: &IndexSettings,
) -> bool {
    previous_settings.attributes_for_faceting != next_settings.attributes_for_faceting
}

/// TODO: Document collect_index_documents.
fn collect_index_document_ids(
    state: &Arc<AppState>,
    index_name: &str,
) -> Result<Vec<String>, HandlerError> {
    let mut object_ids = Vec::new();
    let mut offset = 0usize;

    loop {
        let result = state
            .manager
            .search_with_options(
                index_name,
                "",
                &SearchOptions {
                    limit: FACET_REINDEX_SCAN_PAGE_SIZE,
                    offset,
                    ..Default::default()
                },
            )
            .map_err(|error| HandlerError::from(error.to_string()))?;

        if result.documents.is_empty() {
            break;
        }

        offset += result.documents.len();
        object_ids.extend(
            result
                .documents
                .into_iter()
                .map(|scored| scored.document.id),
        );

        if offset >= result.total {
            break;
        }
    }

    Ok(object_ids)
}

async fn rebuild_documents_for_updated_faceting(
    state: &Arc<AppState>,
    index_name: &str,
) -> Result<(), HandlerError> {
    // Snapshot object IDs first, then reload documents in bounded batches so a
    // settings update cannot buffer the full index payload in request memory.
    let object_ids = collect_index_document_ids(state, index_name)?;
    if object_ids.is_empty() {
        return Ok(());
    }

    for object_id_batch in object_ids.chunks(FACET_REINDEX_WRITE_BATCH_SIZE) {
        let mut documents = Vec::with_capacity(object_id_batch.len());
        for object_id in object_id_batch {
            // A concurrent delete can remove a document after the ID snapshot
            // but before the rebuild pass; skipping it keeps the rebuild safe.
            if let Some(document) = state
                .manager
                .get_document(index_name, object_id)
                .map_err(|error| HandlerError::from(error.to_string()))?
            {
                documents.push(document);
            }
        }

        if documents.is_empty() {
            continue;
        }

        state
            .manager
            .add_documents_sync(index_name, documents)
            .await
            .map_err(|error| HandlerError::from(error.to_string()))?;
    }

    Ok(())
}

/// Parse an optional boolean query parameter, defaulting to `false` when absent.
pub(super) fn parse_bool_query_param(
    query_params: &HashMap<String, String>,
    key: &str,
) -> Result<bool, HandlerError> {
    query_params
        .get(key)
        .map(|value| {
            value
                .parse::<bool>()
                .map_err(|_| HandlerError::bad_request(format!("{key} must be 'true' or 'false'")))
        })
        .transpose()
        .map(|value| value.unwrap_or(false))
}

/// Update the settings for the specified index.
#[utoipa::path(
    post,
    path = "/1/indexes/{indexName}/settings",
    tag = "settings",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    request_body(content = SetSettingsRequest, description = "Settings to update"),
    responses(
        (status = 200, description = "Settings updated successfully", body = SetSettingsResponse),
        (status = 400, description = "Invalid settings")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn set_settings(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
    Query(query_params): Query<HashMap<String, String>>,
    Json(payload): Json<SetSettingsRequest>,
) -> Result<impl IntoResponse, HandlerError> {
    let is_virtual_settings_only = is_virtual_settings_only_index(&state, &index_name);
    if payload.relevancy_strictness.is_some() && !is_virtual_settings_only {
        return Err(HandlerError::bad_request(
            "relevancyStrictness can only be set on virtual replica indices".to_string(),
        ));
    }

    let forward_to_replicas = parse_bool_query_param(&query_params, "forwardToReplicas")?;
    if !is_virtual_settings_only {
        state
            .manager
            .create_tenant(&index_name)
            .map_err(|error| HandlerError::from(error.to_string()))?;
    }

    let settings_path = settings_file_path(&state.manager.base_path, &index_name);
    let unsupported = unsupported_settings_params(&payload);
    let attributes_for_faceting_provided = payload.attributes_for_faceting.is_some();
    let query_languages_provided = payload.query_languages.is_some();
    #[cfg(feature = "vector-search")]
    let embedders_updated = payload.embedders.is_some();

    let mut settings = load_settings_or_default(&settings_path)?;
    let previous_settings = settings.clone();
    let previous_replicas = settings.replicas.clone();
    let old_embedders = settings.embedders.clone();

    let validated_replicas = merge_settings_payload(&mut settings, payload, &index_name)
        .map_err(|(status, message)| HandlerError::Custom { status, message })?;
    settings.restore_redacted_response_secrets(&previous_settings);
    settings
        .validate_embedders()
        .map_err(HandlerError::bad_request)?;
    log_embedder_changes(&old_embedders, &settings);
    let faceting_changed = faceting_configuration_changed(&previous_settings, &settings);

    save_settings(&settings, &settings_path)?;
    state.manager.invalidate_settings_cache(&index_name);
    state.manager.invalidate_facet_cache(&index_name);

    if let Some(ref replicas) = validated_replicas {
        clear_removed_replica_primary_links(
            &state,
            &index_name,
            previous_replicas.as_deref(),
            replicas,
        )
        .map_err(|error| HandlerError::from(error.to_string()))?;
        persist_replica_primary_links(&state, &index_name, replicas)
            .map_err(|error| HandlerError::from(error.to_string()))?;
    }

    if forward_to_replicas {
        forward_settings_to_replicas(
            &state,
            &settings,
            attributes_for_faceting_provided,
            query_languages_provided,
        )
        .map_err(|error| HandlerError::from(error.to_string()))?;
    }

    if faceting_changed && !is_virtual_settings_only {
        rebuild_documents_for_updated_faceting(&state, &index_name).await?;

        if forward_to_replicas {
            for replica_name in standard_replicas_for_primary(&state, &index_name)
                .map_err(|error| HandlerError::from(error.to_string()))?
            {
                rebuild_documents_for_updated_faceting(&state, &replica_name).await?;
            }
        }
    }

    #[cfg(feature = "vector-search")]
    if embedders_updated {
        state.embedder_store.invalidate(&index_name);
    }

    state.manager.append_oplog(
        &index_name,
        "settings",
        serde_json::to_value(&settings).unwrap_or_default(),
    );

    let noop_task = state
        .manager
        .make_noop_task(&index_name)
        .map_err(|error| HandlerError::from(error.to_string()))?;
    let response = SetSettingsResponse {
        updated_at: chrono::Utc::now().to_rfc3339(),
        task_id: noop_task.numeric_id,
        unsupported_params: if unsupported.is_empty() {
            None
        } else {
            Some(unsupported)
        },
    };

    let status = if response.unsupported_params.is_some() {
        StatusCode::MULTI_STATUS
    } else {
        StatusCode::OK
    };
    Ok((status, Json(response)))
}

#[utoipa::path(
    put,
    path = "/1/indexes/{indexName}/settings",
    tag = "settings",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    request_body(content = SetSettingsRequest, description = "Settings to update"),
    responses(
        (status = 200, description = "Settings updated successfully", body = SetSettingsResponse),
        (status = 400, description = "Invalid settings")
    ),
    security(
        ("api_key" = [])
    )
)]
#[allow(dead_code)]
pub(crate) async fn set_settings_put_doc() {}

fn save_settings(settings: &IndexSettings, path: &Path) -> Result<(), HandlerError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| HandlerError::from(error.to_string()))?;
    }
    settings.save(path).map_err(|error| match error {
        flapjack::error::FlapjackError::Json(serialize_error) => HandlerError::from(format!(
            "Failed to serialize settings file '{}': {}",
            path.display(),
            serialize_error
        )),
        other => HandlerError::from(other.to_string()),
    })
}

pub(in crate::handlers) fn persist_index_settings(
    manager: &flapjack::IndexManager,
    index_name: &str,
    settings: &IndexSettings,
) -> Result<(), HandlerError> {
    let settings_path = settings_file_path(&manager.base_path, index_name);
    save_settings(settings, &settings_path)?;
    manager.invalidate_settings_cache(index_name);
    manager.invalidate_facet_cache(index_name);
    Ok(())
}

/// Get index settings
#[utoipa::path(
    get,
    path = "/1/indexes/{indexName}/settings",
    tag = "settings",
    params(
        ("indexName" = String, Path, description = "Index name")
    ),
    responses(
        (status = 200, description = "Index settings", body = serde_json::Value),
        (status = 404, description = "Index not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
) -> Result<impl IntoResponse, HandlerError> {
    let settings_path = settings_file_path(&state.manager.base_path, &index_name);
    let settings = load_settings_or_default(&settings_path)?;

    Ok(Json(settings.redacted_for_response()))
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
