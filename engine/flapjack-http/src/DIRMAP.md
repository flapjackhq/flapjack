<!-- [scrai:start] -->
## src

| File | Summary |
| --- | --- |
| admin_key_persistence.rs | Stub summary for engine/flapjack-http/src/admin_key_persistence.rs. |
| ai_provider.rs | Stub summary for ai_provider.rs. |
| analytics_cluster.rs | Analytics cluster fan-out coordinator.



When peers are configured, queries all peers in parallel and merges results.

Each peer receives the same analytics query with `X-Flapjack-Local-Only: true`

to prevent re-entrant fan-out. |
| auth_tests.rs | Stub summary for engine/flapjack-http/src/auth_tests.rs. |
| background_tasks.rs | Stub summary for background_tasks.rs. |
| conversation_store.rs | In-memory conversation store for multi-turn RAG chat with bounded history and TTL-based eviction. |
| dto.rs | Algolia-compatible search request and response DTOs with URL-encoded params merging, facet/numeric/tag filter AST conversion, and request validation. |
| error_response.rs | Stub summary for engine/flapjack-http/src/error_response.rs. |
| extractors.rs | Stub summary for engine/flapjack-http/src/extractors.rs. |
| federation.rs | Stub summary for engine/flapjack-http/src/federation.rs. |
| filter_parser.rs | Re-exports the filter parser from the core `flapjack` crate.



The parser was moved to core so that `Rule::matches()` can parse

condition filter strings without a cross-crate dependency. |
| idempotency.rs | Stub summary for idempotency.rs. |
| latency_middleware.rs | Observes HTTP request duration and publishes Prometheus metrics with route template, method, and status class labels. |
| memory_middleware.rs | Axum middleware that enforces memory-pressure load shedding by rejecting or limiting requests and adjusting facet cache capacity based on the current pressure level. |
| middleware.rs | Axum middleware for trusted-proxy-aware client IP extraction, content-type normalization, JSON error wrapping, and Private Network Access preflight handling. |
| mutation_parity.rs | Stub summary for mutation_parity.rs. |
| notifications.rs | Stub summary for engine/flapjack-http/src/notifications.rs. |
| openapi.rs | Stub summary for openapi.rs. |
| openapi_export_tests.rs | Stub summary for openapi_export_tests.rs. |
| openapi_test_helpers.rs | Stub summary for engine/flapjack-http/src/openapi_test_helpers.rs. |
| openapi_tests.rs | Stub summary for openapi_tests.rs. |
| openapi_tests_endpoints.rs | Stub summary for engine/flapjack-http/src/openapi_tests_endpoints.rs. |
| otel.rs | OpenTelemetry initialization module.



Provides `try_init_otel_layer()` as the single entrypoint for OTEL setup.

Reads `OTEL_EXPORTER_OTLP_ENDPOINT` and returns `None` when unset/empty,

or `Some((layer, guard))` when configured. |
| pause_registry.rs | Thread-safe registry tracking which indexes are paused, used to reject writes with 503 during migration. |
| rollup_broadcaster.rs | Stub summary for engine/flapjack-http/src/rollup_broadcaster.rs. |
| router.rs | Stub summary for router.rs. |
| router_inline_tests.rs | Stub summary for router_inline_tests.rs. |
| router_tests.rs | Stub summary for router_tests.rs. |
| security_sources.rs | IP-allowlist security sources with persistent JSON-backed store, cached CIDR matcher, and Axum middleware for request filtering. |
| server.rs | Stub summary for server.rs. |
| server_init.rs | Stub summary for engine/flapjack-http/src/server_init.rs. |
| server_shutdown_tests.rs | Stub summary for engine/flapjack-http/src/server_shutdown_tests.rs. |
| server_startup_tests.rs | Stub summary for engine/flapjack-http/src/server_startup_tests.rs. |
| startup.rs | Stub summary for startup.rs. |
| startup_catchup.rs | Stub summary for startup_catchup.rs. |
| startup_tests.rs | Stub summary for startup_tests.rs. |
| tenant_dirs.rs | Stub summary for engine/flapjack-http/src/tenant_dirs.rs. |
| usage_middleware.rs | Request counting middleware for per-index usage metrics, tracking search, write, and read counts plus bytes ingested per index name. |
| usage_persistence.rs | Tests for `UsagePersistence`: atomic snapshot writes, save/load round-trips, daily rollup with counter reset, multi-index fidelity, and JSON validity. |

| Directory | Summary |
| --- | --- |
| auth | The auth directory handles API authentication and access control for Flapjack's Algolia-compatible routes. |
| auth_tests | The auth_tests directory contains unit tests for the HTTP server's authentication and authorization mechanisms, covering key store management, authentication middleware, source-based access restrictions, and route-level ACL enforcement. |
| bin | The bin directory contains utility binaries for Flapjack's operational tasks: analytics_retention_probe probes analytics data retention behavior, export-openapi exports the API specification, and parity_export exports compatibility or parity data. |
| dto | This module defines the HTTP API's data transfer objects, including request parameters and response types, with specialized support for parsing and converting Algolia-compatible filters across facet, numeric, and tag dimensions. |
| handlers | The handlers directory implements Flapjack's complete Algolia-compatible HTTP API layer, organized into functional domains (search, documents, indices, analytics, recommendations, settings, experiments, migration) with support for batch operations, replication, and analytics ingestion. |
<!-- [scrai:end] -->
