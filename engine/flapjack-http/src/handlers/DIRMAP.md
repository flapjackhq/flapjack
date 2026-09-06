<!-- [scrai:start] -->
## handlers

| File | Summary |
| --- | --- |
| analytics_dto.rs | Stub summary for engine/flapjack-http/src/handlers/analytics_dto.rs. |
| analytics_tests.rs | Stub summary for analytics_tests.rs. |
| browse.rs | Stub summary for browse.rs. |
| chat.rs | Stub summary for chat.rs. |
| chat_tests.rs | Stub summary for chat_tests.rs. |
| dashboard.rs | Axum handler that serves the embedded single-page dashboard application, with MIME detection and client-side routing fallback. |
| dictionaries.rs | Stub summary for engine/flapjack-http/src/handlers/dictionaries.rs. |
| dto_algolia.rs | Define Algolia-compatible request/response DTOs and bidirectional conversion between Algolia and internal experiment formats. |
| dto_algolia_tests.rs | Test conversion and serialization of Algolia A/B test request DTOs to internal Experiment types, including field mapping, validation, and timestamp handling. |
| experiments_tests.rs | Stub summary for experiments_tests.rs. |
| facets.rs | Handlers for the facet-value search endpoint, supporting query highlighting, filtering, and configurable sort order. |
| health.rs | Health check handler that reports server status, resource usage, memory pressure, and build metadata. |
| index_resource_store.rs | Stub summary for index_resource_store.rs. |
| indices.rs | Handlers for index CRUD operations (create, delete, list, clear, compact, copy/move) with Algolia-compatible pagination, oplog replication, and replica-aware clearing. |
| insights.rs | Algolia Insights API-compatible event ingestion, debug event inspection, and GDPR user token deletion handlers. |
| internal.rs | Stub summary for engine/flapjack-http/src/handlers/internal.rs. |
| internal_ops.rs | Stub summary for engine/flapjack-http/src/handlers/internal_ops.rs. |
| internal_tests.rs | Stub summary for internal_tests.rs. |
| keys.rs | Stub summary for engine/flapjack-http/src/handlers/keys.rs. |
| metrics.rs | Stub summary for metrics.rs. |
| metrics_latency_tests.rs | Test that request latency metrics are correctly collected and exposed via the metrics handler. |
| mod.rs | Root handler module that defines `AppState` and re-exports all HTTP handler functions. |
| objects_tests.rs | Stub summary for objects_tests.rs. |
| personalization.rs | Stub summary for engine/flapjack-http/src/handlers/personalization.rs. |
| query_suggestions.rs | Stub summary for query_suggestions.rs. |
| readiness.rs | Stub summary for readiness.rs. |
| recommend.rs | HTTP handler for the batched recommendations endpoint, dispatching to trending-items, trending-facets, related-products, bought-together, and looking-similar models with validation, rule application, and replica resolution. |
| recommend_rules.rs | CRUD handlers for recommend rules scoped by index and recommendation model, supporting get, put, delete, batch, and search operations. |
| replicas.rs | Helpers for managing replica indexes: resolving virtual-vs-physical search targets, persisting and clearing primary links, and mirroring document writes/deletes to standard replicas. |
| rules.rs | Stub summary for rules.rs. |
| settings.rs | Stub summary for settings.rs. |
| settings_tests.rs | Stub summary for settings_tests.rs. |
| snapshot.rs | Stub summary for snapshot.rs. |
| synonyms.rs | Stub summary for synonyms.rs. |
| tasks.rs | Axum handlers for querying indexing-task status with an Algolia-compatible response shape. |
| usage.rs | Algolia-compatible usage statistics endpoints (`GET /1/usage/:statistic` and `GET /1/usage/:statistic/:indexName`) that merge persisted historical snapshots with live in-memory counters and return time-series data points. |
| usage_tests.rs | Usage endpoint tests for live counters, current gauges, and persisted

historical counter snapshots. |
| wire_format_tests.rs | SDK wire-format verification tests that lock protocol-level behavior for the Algolia-compatible HTTP API, covering header validation, CORS preflight, params-string decoding, error envelope formatting, rate limiting, and connection handling. |

| Directory | Summary |
| --- | --- |
| analytics | This analytics module implements Algolia-compatible HTTP endpoints for search analytics, including read endpoints for metrics like clicks and conversions and geo/device/revenue analytics, with support for cluster fan-out and comprehensive validation. |
| experiments | The experiments directory contains handlers for A/B testing functionality, with estimate.rs providing statistical calculations for experiment sample sizes and duration based on significance targets and traffic. |
| internal_ops | — |
| migration | This migration module handles data migration from Algolia to Flapjack, providing functionality for reading source data, translating Algolia schemas and objects to Flapjack format, spooling/buffering records for import/export, and running async migration jobs with resume capability. |
| objects | The objects directory contains HTTP handlers for managing documents and batch operations in the Flapjack search engine API, with batch.rs implementing bulk document operations and mod.rs serving as the module root. |
| search | This directory contains HTTP handlers for Flapjack's search API, implementing multiple search modes including single-query, batch multi-index, geo-spatial, hybrid, and personalization-aware search with reranking by click-through rates. |
| settings | The settings directory handles HTTP handlers for managing configuration and settings, with payload_merge.rs managing the merging of settings payloads and replica_forwarding.rs coordinating settings changes across replicated instances. |
<!-- [scrai:end] -->
