//! Root handler module that defines `AppState` and re-exports all HTTP handler functions.
use crate::auth::KeyStore;
use crate::conversation_store::ConversationStore;
use crate::geoip::GeoIpReader;
use crate::idempotency::IdempotencyCache;
use crate::notifications::NotificationService;
use crate::pause_registry::PausedIndexes;
use crate::usage_middleware::TenantUsageCounters;
use crate::usage_persistence::UsagePersistence;
use dashmap::DashMap;
use flapjack::analytics::AnalyticsQueryEngine;
use flapjack::dictionaries::manager::DictionaryManager;
use flapjack::experiments::store::ExperimentStore;
use flapjack::recommend::RecommendConfig;
use flapjack::IndexManager;
use flapjack::SslManager;
use flapjack_replication::manager::ReplicationManager;
use std::sync::Arc;

pub mod analytics;
pub mod analytics_dto;
pub mod browse;
pub mod chat;
pub mod dashboard;
pub mod dictionaries;
pub mod dto_algolia;
pub mod experiments;
pub mod facets;
pub mod health;
mod index_resource_store;
pub mod indices;
pub mod insights;
pub mod internal;
mod internal_ops;
pub mod keys;
pub mod metrics;
pub mod migration;
pub mod objects;
pub mod personalization;
pub mod query_suggestions;
pub mod readiness;
pub mod recommend;
pub mod recommend_rules;
pub mod replicas;
pub mod rules;
pub mod search;
pub mod security_sources;
pub mod settings;
pub mod snapshot;
pub mod synonyms;
pub mod tasks;
pub mod usage;

/// Hold shared server state passed to all HTTP handlers via Axum's `State` extractor.
///
/// Wraps the core `IndexManager`, optional subsystems (authentication, replication, SSL,
/// analytics, experiments, metrics, GeoIP, notifications), and per-tenant usage tracking.
/// All fields are `Arc`-wrapped or cheap to clone so the struct can be shared across
/// concurrent request tasks.
pub struct AppState {
    pub manager: Arc<IndexManager>,
    pub key_store: Option<Arc<KeyStore>>,
    pub replication_manager: Option<Arc<ReplicationManager>>,
    pub ssl_manager: Option<Arc<SslManager>>,
    pub analytics_engine: Option<Arc<AnalyticsQueryEngine>>,
    pub recommend_config: RecommendConfig,
    pub experiment_store: Option<Arc<ExperimentStore>>,
    pub dictionary_manager: Arc<DictionaryManager>,
    pub metrics_state: Option<metrics::MetricsState>,
    pub usage_counters: Arc<DashMap<String, TenantUsageCounters>>,
    pub usage_persistence: Option<Arc<UsagePersistence>>,
    pub paused_indexes: PausedIndexes,
    pub geoip_reader: Option<Arc<GeoIpReader>>,
    pub notification_service: Option<Arc<NotificationService>>,
    pub migration_runner: Arc<migration::MigrationJobRunner>,
    pub start_time: std::time::Instant,
    pub conversation_store: Arc<ConversationStore>,
    pub embedder_store: Arc<crate::embedder_store::EmbedderStore>,
    /// Per-node TTL cache that dedups writes carrying an `X-Flapjack-Idempotency-Key`
    /// header (ADR 0005 / PL-8 write-loss recovery).
    pub idempotency_cache: Arc<IdempotencyCache>,
}

/// Compute nbPages safely for Algolia-style paginated responses.
/// `hitsPerPage=0` is treated as zero pages to avoid divide-by-zero panics.
pub(crate) fn safe_nb_pages(total_hits: usize, hits_per_page: usize) -> usize {
    if hits_per_page == 0 {
        0
    } else {
        total_hits.div_ceil(hits_per_page)
    }
}

pub use browse::browse_index;
pub use chat::chat_index;
pub use facets::{parse_facet_params, search_facet_values};
pub use health::health;
pub use indices::{
    clear_index, compact_index, create_index, delete_index, list_indices, operation_index,
};
pub use keys::{
    create_key, delete_key, generate_secured_key, get_key, list_keys, restore_key, update_key,
};
pub use metrics::metrics_handler;
pub use migration::{
    cancel_algolia_migration, get_algolia_migration_status, list_algolia_indexes,
    migrate_from_algolia, submit_algolia_migration,
};
pub use objects::{
    add_documents, add_record_auto_id, delete_by_query, delete_object, get_object, get_objects,
    partial_update_object, put_object,
};
pub use readiness::ready;
pub use rules::{clear_rules, delete_rule, get_rule, save_rule, save_rules, search_rules};
pub use search::{batch_search, search, search_get};
pub use security_sources::{
    append_security_source, delete_security_source, get_security_sources, replace_security_sources,
};
pub use settings::{get_settings, set_settings};
pub use synonyms::{
    clear_synonyms, delete_synonym, get_synonym, save_synonym, save_synonyms, search_synonyms,
};
pub use tasks::{get_task, get_task_for_index};
pub use usage::{usage_global, usage_per_index};

#[cfg(test)]
mod wire_format_tests;
