pub mod admin_key_persistence;
pub mod ai_provider;
pub mod analytics_cluster;
pub mod auth;
pub mod conversation_store;
#[cfg(test)]
mod dashboard_build;
pub mod dto;
pub mod filter_parser;
pub mod geoip;
pub mod handlers;
pub mod idempotency;
pub mod latency_middleware;
pub mod memory_middleware;
pub mod middleware;
pub mod mutation_parity;
pub mod notifications;
pub mod openapi;
pub mod openapi_export;
pub mod pause_registry;
pub mod rollup_broadcaster;
pub mod router;
pub mod security_sources;
pub mod server;
pub mod server_init;
pub mod startup;
pub mod startup_catchup;
pub(crate) mod tenant_dirs;

pub mod background_tasks;
pub(crate) mod usage_capture;
pub mod usage_middleware;
pub mod usage_persistence;

#[cfg(feature = "vector-search")]
pub mod embedder_store;
#[cfg(not(feature = "vector-search"))]
pub mod embedder_store {
    #[derive(Default)]
    pub struct EmbedderStore;

    impl EmbedderStore {
        pub fn new() -> Self {
            Self
        }
    }
}
pub mod error_response;
pub mod extractors;
pub mod federation;
#[cfg(feature = "vector-search")]
pub mod fusion;
#[cfg(feature = "otel")]
pub mod otel;

#[cfg(test)]
mod openapi_export_tests;
#[cfg(test)]
pub(crate) mod openapi_test_helpers;
#[cfg(test)]
#[path = "openapi_tests_legacy_filter.rs"]
mod openapi_tests;
#[cfg(test)]
mod operations_consumer_contract_tests;
#[cfg(test)]
mod router_tests;
#[cfg(test)]
pub(crate) mod test_helpers;

pub use server::serve;
