use utoipa::OpenApi;

pub(crate) const DOCUMENTED_INTERNAL_MEMBERSHIP_PATHS: [&str; 2] = [
    "/internal/cluster/peers",
    "/internal/cluster/peers/{node_id}",
];
pub(crate) const DOCUMENTED_MEMBERSHIP_SCHEMA_NAMES: [&str; 3] =
    ["AddPeerRequest", "AddPeerResponse", "RemovePeerResponse"];

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Flapjack API",
        version = "0.1.0",
        description = "Drop-in replacement for Algolia search. Provides full-text search with filters, facets, geo-search, and more.",
        license(name = "MIT OR Apache-2.0"),
        contact(
            name = "Flapjack",
            url = "https://github.com/flapjackhq/flapjack"
        )
    ),
    servers(
        (url = "http://localhost:7700", description = "Local development"),
        (url = "https://fj-us-west-1.flapjack.foo", description = "Production US West")
    ),
    paths(
        crate::handlers::health::health,
        crate::handlers::internal::add_cluster_peer,
        crate::handlers::internal::remove_cluster_peer,
        crate::handlers::indices::create_index,
        crate::handlers::indices::delete_index,
        crate::handlers::indices::list_indices,
        crate::handlers::indices::clear_index,
        crate::handlers::indices::operation_index,
        crate::handlers::search::search,
        crate::handlers::search::search_get,
        crate::handlers::search::batch_search,
        crate::handlers::objects::add_documents,
        crate::handlers::objects::add_record_auto_id,
        crate::handlers::objects::get_object,
        crate::handlers::objects::delete_object,
        crate::handlers::objects::put_object,
        crate::handlers::objects::partial_update_object,
        crate::handlers::objects::get_objects,
        crate::handlers::objects::delete_by_query,
        crate::handlers::browse::browse_index,
        crate::handlers::facets::search_facet_values,
        crate::handlers::settings::get_settings,
        crate::handlers::settings::set_settings,
        crate::handlers::settings::set_settings_put_doc,
        crate::handlers::tasks::get_task,
        crate::handlers::tasks::get_task_for_index,
        crate::handlers::synonyms::get_synonym,
        crate::handlers::synonyms::save_synonym,
        crate::handlers::synonyms::delete_synonym,
        crate::handlers::synonyms::save_synonyms,
        crate::handlers::synonyms::clear_synonyms,
        crate::handlers::synonyms::search_synonyms,
        crate::handlers::rules::get_rule,
        crate::handlers::rules::save_rule,
        crate::handlers::rules::delete_rule,
        crate::handlers::rules::save_rules,
        crate::handlers::rules::clear_rules,
        crate::handlers::rules::search_rules,
        crate::handlers::keys::create_key,
        crate::handlers::keys::list_keys,
        crate::handlers::keys::get_key,
        crate::handlers::keys::update_key,
        crate::handlers::keys::delete_key,
        crate::handlers::keys::restore_key,
        crate::handlers::keys::generate_secured_key,
        crate::handlers::snapshot::export_snapshot,
        crate::handlers::snapshot::import_snapshot,
        crate::handlers::snapshot::snapshot_to_s3,
        crate::handlers::snapshot::restore_from_s3,
        crate::handlers::snapshot::list_s3_snapshots,
        crate::handlers::security_sources::get_security_sources,
        crate::handlers::security_sources::replace_security_sources,
        crate::handlers::security_sources::append_security_source,
        crate::handlers::security_sources::delete_security_source,
        // Stage 7: Recommendations
        crate::handlers::recommend::recommend,
        // Stage 7: Personalization
        crate::handlers::personalization::set_personalization_strategy,
        crate::handlers::personalization::get_personalization_strategy,
        crate::handlers::personalization::get_user_profile,
        crate::handlers::personalization::delete_user_profile,
        // Stage 7: Experiments
        crate::handlers::experiments::create_experiment,
        crate::handlers::experiments::list_experiments,
        crate::handlers::experiments::get_experiment,
        crate::handlers::experiments::update_experiment,
        crate::handlers::experiments::delete_experiment,
        crate::handlers::experiments::start_experiment,
        crate::handlers::experiments::stop_experiment,
        crate::handlers::experiments::conclude_experiment,
        crate::handlers::experiments::estimate::estimate_ab_test,
        crate::handlers::experiments::results::get_experiment_results,
        // Stage 7: Analytics (read)
        crate::handlers::analytics::get_top_searches,
        crate::handlers::analytics::get_search_count,
        crate::handlers::analytics::get_no_results,
        crate::handlers::analytics::get_no_result_rate,
        crate::handlers::analytics::get_no_clicks,
        crate::handlers::analytics::get_no_click_rate,
        crate::handlers::analytics::get_click_through_rate,
        crate::handlers::analytics::get_average_click_position,
        crate::handlers::analytics::get_click_positions,
        crate::handlers::analytics::get_add_to_cart_rate,
        crate::handlers::analytics::get_purchase_rate,
        crate::handlers::analytics::get_conversion_rate,
        crate::handlers::analytics::get_top_hits,
        crate::handlers::analytics::get_top_filters,
        crate::handlers::analytics::get_filter_values,
        crate::handlers::analytics::get_filters_no_results,
        crate::handlers::analytics::get_users_count,
        crate::handlers::analytics::get_overview,
        crate::handlers::analytics::get_device_breakdown,
        crate::handlers::analytics::get_geo_breakdown,
        crate::handlers::analytics::get_geo_top_searches,
        crate::handlers::analytics::get_geo_regions,
        crate::handlers::analytics::get_revenue,
        crate::handlers::analytics::get_countries,
        crate::handlers::analytics::get_analytics_status,
        // Stage 7: Analytics (mutations)
        crate::handlers::analytics::seed_analytics,
        crate::handlers::analytics::flush_analytics,
        crate::handlers::analytics::clear_analytics,
        crate::handlers::analytics::cleanup_analytics,
        // Stage 7: Query Suggestions
        crate::handlers::query_suggestions::list_configs,
        crate::handlers::query_suggestions::create_config,
        crate::handlers::query_suggestions::get_config,
        crate::handlers::query_suggestions::update_config,
        crate::handlers::query_suggestions::delete_config,
        crate::handlers::query_suggestions::get_status,
        crate::handlers::query_suggestions::get_logs,
        crate::handlers::query_suggestions::trigger_build,
        // Stage 7: Dictionaries
        crate::handlers::dictionaries::dictionary_batch,
        crate::handlers::dictionaries::dictionary_search,
        crate::handlers::dictionaries::dictionary_get_settings,
        crate::handlers::dictionaries::dictionary_set_settings,
        crate::handlers::dictionaries::dictionary_list_languages,
        // Stage 7: Insights
        crate::handlers::insights::post_events,
        crate::handlers::insights::get_debug_events,
        crate::handlers::insights::delete_usertoken,
        // Stage 7: Migration
        crate::handlers::migration::migrate_from_algolia,
        crate::handlers::migration::submit_algolia_migration,
        crate::handlers::migration::get_algolia_migration_status,
        crate::handlers::migration::cancel_algolia_migration,
        crate::handlers::migration::list_algolia_indexes,
        // Stage 7: Usage
        crate::handlers::usage::usage_global,
        crate::handlers::usage::usage_per_index,
        // Stage 7: Chat
        crate::handlers::chat::chat_index,
    ),
    components(
        schemas(
            crate::handlers::internal::AddPeerRequest,
            crate::handlers::internal::AddPeerResponse,
            crate::handlers::internal::RemovePeerResponse,
            crate::dto::CreateIndexRequest,
            crate::dto::IndexSchema,
            crate::dto::CreateIndexResponse,
            crate::dto::DeleteIndexResponse,
            crate::handlers::indices::ListIndexItem,
            crate::handlers::indices::ListIndicesResponse,
            crate::handlers::indices::OperationIndexRequest,
            crate::dto::SearchRequest,
            crate::dto::FederationOptions,
            crate::dto::BatchSearchRequest,
            crate::dto::BatchSearchLegacyResponse,
            crate::dto::BatchSearchResponse,
            crate::dto::SearchResponse,
            crate::dto::SearchExhaustive,
            crate::dto::SearchProcessingTimings,
            crate::dto::SearchFacetStatsSummary,
            crate::dto::SearchAppliedRule,
            crate::federation::FederationConfig,
            crate::federation::FederatedHit,
            crate::federation::FederationMeta,
            crate::federation::FederatedResponse,
            crate::dto::AddDocumentsRequest,
            crate::dto::BatchOperation,
            crate::dto::AddDocumentsResponse,
            crate::dto::BatchWriteResponse,
            crate::dto::SaveObjectResponse,
            crate::dto::PutObjectResponse,
            crate::dto::DeleteObjectResponse,
            crate::dto::PartialUpdateObjectResponse,
            crate::dto::GetObjectsRequest,
            crate::dto::GetObjectRequest,
            crate::dto::GetObjectsResponse,
            crate::dto::DeleteByQueryRequest,
            crate::dto::SearchFacetValuesRequest,
            crate::dto::SearchFacetValuesResponse,
            crate::dto::FacetHit,
            crate::handlers::tasks::AlgoliaTaskResponse,
            crate::auth::KeyApiResponse,
            crate::handlers::keys::CreateKeyRequest,
            crate::handlers::keys::CreateKeyResponse,
            crate::handlers::keys::ListKeysResponse,
            crate::handlers::keys::UpdateKeyResponse,
            crate::handlers::keys::DeleteKeyResponse,
            crate::handlers::keys::RestoreKeyResponse,
            crate::handlers::keys::GenerateSecuredKeyRequest,
            crate::handlers::keys::GenerateSecuredKeyResponse,
            crate::handlers::keys::SecuredKeyRestrictions,
            crate::security_sources::SecuritySourceEntry,
            crate::handlers::security_sources::SourceMutationTimestampResponse,
            // Stage 7: Recommendations
            crate::handlers::recommend::RecommendBatchRequest,
            crate::handlers::recommend::RecommendRequest,
            crate::handlers::recommend::RecommendBatchResponse,
            crate::handlers::recommend::RecommendResult,
            // Stage 7: Personalization
            flapjack::personalization::PersonalizationStrategy,
            flapjack::personalization::EventScoring,
            flapjack::personalization::FacetScoring,
            flapjack::personalization::PersonalizationProfile,
            crate::handlers::personalization::SetStrategyResponse,
            crate::handlers::personalization::DeleteProfileResponse,
            // Stage 7: Experiments (Algolia DTOs)
            crate::handlers::dto_algolia::AlgoliaCreateAbTestRequest,
            crate::handlers::dto_algolia::AlgoliaCreateAbTestResponse,
            crate::handlers::dto_algolia::AlgoliaAbTestActionResponse,
            crate::handlers::dto_algolia::AlgoliaListAbTestsResponse,
            crate::handlers::dto_algolia::AlgoliaAbTest,
            crate::handlers::dto_algolia::AlgoliaVariant,
            crate::handlers::dto_algolia::AlgoliaConfiguration,
            crate::handlers::dto_algolia::AlgoliaMinimumDetectableEffect,
            crate::handlers::dto_algolia::AlgoliaOutliersSetting,
            crate::handlers::dto_algolia::AlgoliaEmptySearchSetting,
            crate::handlers::dto_algolia::AlgoliaFeatureFilters,
            crate::handlers::dto_algolia::AlgoliaFilterEffects,
            crate::handlers::dto_algolia::AlgoliaFilterEffectsEntry,
            crate::handlers::dto_algolia::AlgoliaCreateVariant,
            crate::handlers::dto_algolia::AlgoliaCreateConfiguration,
            crate::handlers::dto_algolia::AlgoliaMetricDef,
            crate::handlers::dto_algolia::AlgoliaEstimateRequest,
            crate::handlers::dto_algolia::AlgoliaEstimateConfiguration,
            crate::handlers::dto_algolia::AlgoliaEstimateVariant,
            crate::handlers::dto_algolia::AlgoliaEstimateResponse,
            crate::handlers::experiments::CreateExperimentRequest,
            crate::handlers::experiments::ConcludeExperimentRequest,
            crate::handlers::experiments::ConcludedExperimentResponse,
            crate::handlers::experiments::ResultsResponse,
            crate::handlers::experiments::InterleavingResponse,
            crate::handlers::experiments::GuardRailAlertResponse,
            crate::handlers::experiments::GateResponse,
            crate::handlers::experiments::ArmResponse,
            crate::handlers::experiments::SignificanceResponse,
            crate::handlers::experiments::BayesianResponse,
            flapjack::experiments::config::Experiment,
            flapjack::experiments::config::ExperimentStatus,
            flapjack::experiments::config::ExperimentArm,
            flapjack::experiments::config::QueryOverrides,
            flapjack::experiments::config::PrimaryMetric,
            flapjack::experiments::config::ExperimentConclusion,
            // Stage 7: Analytics
            crate::handlers::analytics::SeedRequest,
            crate::handlers::analytics::AnalyticsDateCount,
            crate::handlers::analytics::AnalyticsTopSearchEntry,
            crate::handlers::analytics::AnalyticsTopSearchesResponse,
            crate::handlers::analytics::AnalyticsTopHitsResponse,
            crate::handlers::analytics::AnalyticsHitEntry,
            crate::handlers::analytics::AnalyticsFiltersResponse,
            crate::handlers::analytics::AnalyticsFilterEntry,
            crate::handlers::analytics::AnalyticsFilterValuesResponse,
            crate::handlers::analytics::AnalyticsValueCount,
            crate::handlers::analytics::AnalyticsCountWithDatesResponse,
            crate::handlers::analytics::AnalyticsRateDateEntry,
            crate::handlers::analytics::AnalyticsRateWithDatesResponse,
            crate::handlers::analytics::AnalyticsAverageClickPositionDate,
            crate::handlers::analytics::AnalyticsAverageClickPositionResponse,
            crate::handlers::analytics::AnalyticsClickPositionBucket,
            crate::handlers::analytics::AnalyticsClickPositionsResponse,
            crate::handlers::analytics::AnalyticsCountryCount,
            crate::handlers::analytics::AnalyticsCountriesResponse,
            crate::handlers::analytics::AnalyticsGeoBreakdownResponse,
            crate::handlers::analytics::AnalyticsPlatformCount,
            crate::handlers::analytics::AnalyticsPlatformDateCount,
            crate::handlers::analytics::AnalyticsDeviceBreakdownResponse,
            crate::handlers::analytics::AnalyticsGeoTopSearchesResponse,
            crate::handlers::analytics::AnalyticsRegionCount,
            crate::handlers::analytics::AnalyticsGeoRegionsResponse,
            crate::handlers::analytics::AnalyticsCurrencyRevenue,
            crate::handlers::analytics::AnalyticsRevenueDateEntry,
            crate::handlers::analytics::AnalyticsRevenueResponse,
            crate::handlers::analytics::AnalyticsOverviewIndexSummary,
            crate::handlers::analytics::AnalyticsOverviewResponse,
            crate::handlers::analytics::AnalyticsSeedResponse,
            crate::handlers::analytics::AnalyticsFlushResponse,
            crate::handlers::analytics::AnalyticsClearResponse,
            crate::handlers::analytics::AnalyticsStatusResponse,
            crate::handlers::analytics::AnalyticsCleanupResponse,
            // Stage 7: Query Suggestions (core crate types)
            flapjack::query_suggestions::QsConfig,
            flapjack::query_suggestions::QsSourceIndex,
            flapjack::query_suggestions::config::QsFacet,
            flapjack::query_suggestions::BuildStatus,
            flapjack::query_suggestions::LogEntry,
            crate::handlers::query_suggestions::QsMutationResponse,
            // Stage 7: Dictionaries (core crate types)
            flapjack::dictionaries::BatchAction,
            flapjack::dictionaries::BatchRequest,
            flapjack::dictionaries::BatchDictionaryRequest,
            flapjack::dictionaries::DictionaryName,
            flapjack::dictionaries::MutationResponse,
            flapjack::dictionaries::DictionarySearchRequest,
            flapjack::dictionaries::DictionarySearchResponse,
            flapjack::dictionaries::DictionarySettings,
            flapjack::dictionaries::LanguageDictionaryCounts,
            flapjack::dictionaries::DictionaryCount,
            // Stage 7: Insights
            crate::handlers::insights::InsightsRequest,
            flapjack::analytics::schema::InsightEvent,
            // Stage 7: Migration
            crate::handlers::migration::MigrateFromAlgoliaRequest,
            crate::handlers::migration::MigrateFromAlgoliaResponse,
            crate::handlers::migration::MigrateWarning,
            crate::handlers::migration::MigrateCount,
            crate::handlers::migration::AsyncMigrationStatusResponse,
            crate::handlers::migration::AsyncMigrationPhase,
            crate::handlers::migration::AsyncMigrationDisposition,
            crate::handlers::migration::AsyncMigrationExportProgress,
            crate::handlers::migration::ListAlgoliaIndexesRequest,
            crate::handlers::migration::AlgoliaIndexInfo,
            crate::handlers::migration::ListAlgoliaIndexesResponse,
            // Stage 7: Chat
            crate::handlers::chat::ChatRequest,
            crate::handlers::chat::ChatResponse,
        )
    ),
    tags(
        (name = "health", description = "Health check"),
        (name = "indices", description = "Index management operations"),
        (name = "search", description = "Search and query operations"),
        (name = "documents", description = "Document CRUD operations"),
        (name = "settings", description = "Index settings"),
        (name = "synonyms", description = "Synonym management"),
        (name = "rules", description = "Query rules"),
        (name = "keys", description = "API key management"),
        (name = "security", description = "Security source allowlist"),
        (name = "snapshots", description = "Backup and restore operations"),
        (name = "tasks", description = "Task status endpoints"),
        (name = "recommendations", description = "Recommendation endpoints"),
        (name = "personalization", description = "Personalization strategy and profile endpoints"),
        (name = "experiments", description = "A/B test experiment endpoints"),
        (name = "analytics", description = "Analytics read endpoints"),
        (name = "analytics-operations", description = "Analytics mutation/operations endpoints"),
        (name = "query-suggestions", description = "Query suggestions configuration and build endpoints"),
        (name = "dictionaries", description = "Dictionary management endpoints"),
        (name = "insights", description = "Insights event endpoints"),
        (name = "migration", description = "Algolia migration endpoints"),
        (name = "usage", description = "Usage statistics endpoints"),
        (name = "chat", description = "AI chat endpoints"),
    ),
    modifiers(&SecurityAddon, &ChatSseAddon),
)]
pub struct ApiDoc;

pub fn documented_openapi(auth_enabled: bool) -> utoipa::openapi::OpenApi {
    let mut document = ApiDoc::openapi();
    if auth_enabled {
        return document;
    }

    for path in DOCUMENTED_INTERNAL_MEMBERSHIP_PATHS {
        document.paths.paths.remove(path);
    }

    if let Some(components) = document.components.as_mut() {
        for schema in DOCUMENTED_MEMBERSHIP_SCHEMA_NAMES {
            components.schemas.remove(schema);
        }
    }

    document
}

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "api_key",
                utoipa::openapi::security::SecurityScheme::ApiKey(
                    utoipa::openapi::security::ApiKey::Header(
                        utoipa::openapi::security::ApiKeyValue::new("x-algolia-api-key"),
                    ),
                ),
            );
        }
    }
}

/// Adds `text/event-stream` as an additional 200-response content type on the
/// chat endpoint, since `utoipa::path` proc macro only supports a single
/// content type per response status.
struct ChatSseAddon;

impl utoipa::Modify for ChatSseAddon {
    /// Patches the OpenAPI spec to add SSE `text/event-stream` response to the chat endpoint.
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::Content;

        let chat_path = "/1/indexes/{indexName}/chat";
        if let Some(path_item) = openapi.paths.paths.get_mut(chat_path) {
            if let Some(op) = path_item.post.as_mut() {
                if let Some(utoipa::openapi::RefOr::T(resp)) = op.responses.responses.get_mut("200")
                {
                    let sse_content = Content::new(Some(
                        utoipa::openapi::ObjectBuilder::new()
                            .schema_type(utoipa::openapi::schema::Type::String)
                            .description(Some("Server-Sent Events stream of chat chunks"))
                            .build(),
                    ));
                    resp.content
                        .insert("text/event-stream".to_string(), sse_content);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "openapi_tests.rs"]
mod tests;
