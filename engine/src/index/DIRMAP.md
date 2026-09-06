<!-- [scrai:start] -->
## index

| File | Summary |
| --- | --- |
| document.rs | Convert documents between JSON, internal `Document`, and Tantivy representations, handling field splitting for search vs. |
| facet_translation.rs | Utilities for translating between Algolia-style hierarchical facet values and Tantivy facet path representations. |
| index_metadata.rs | Durable per-index metadata.



Persisted as `index_meta.json` inside each index directory.

Tracks `created_at` (RFC3339) and `last_build_time_s` (seconds for last build).

Can be read without loading the full Tantivy index. |
| memory.rs | Stub summary for engine/src/index/memory.rs. |
| memory_observer.rs | Stub summary for engine/src/index/memory_observer.rs. |
| mod.rs | Stub summary for engine/src/index/mod.rs. |
| node_id.rs | Helpers for resolving the process node identifier used in oplog and LWW state. |
| oplog.rs | Stub summary for engine/src/index/oplog.rs. |
| relevance.rs | Relevance configuration for controlling per-attribute search weights, supporting both explicit overrides and positional exponential decay defaults. |
| replica.rs | Parse, validate, and classify replica entries (standard vs. |
| rules_tests.rs | Stub summary for engine/src/index/rules_tests.rs. |
| s3.rs | Stub summary for engine/src/index/s3.rs. |
| schema.rs | Defines the Flapjack Schema and SchemaBuilder types that describe index field layouts and convert them into the hardcoded dual-JSON-field tantivy schema used at index time. |
| settings.rs | Stub summary for settings.rs. |
| settings_redaction.rs | Manages redaction and restoration of sensitive fields in JSON user settings and embedder configurations, replacing secrets with placeholders on export and restoring them from cached state on import. |
| settings_tests.rs | Stub summary for engine/src/index/settings_tests.rs. |
| snapshot.rs | Stub summary for snapshot.rs. |
| storage_size.rs | Per-tenant disk usage calculator providing a recursive, symlink-safe directory size function used by metrics and internal storage endpoints. |
| synonyms.rs | Synonym storage and query-expansion engine supporting Algolia-compatible synonym types (regular, one-way, alt-correction, placeholder) with persistence, text search, and pagination. |
| task_queue.rs | Background task queue that serializes long-running index operations (currently export) via a bounded mpsc channel, tracking each task's lifecycle in a shared `DashMap`. |
| utils.rs | Filesystem helpers for recursive directory copying with temporary-file filtering. |
| write_queue_tests.rs | Stub summary for write_queue_tests.rs. |

| Directory | Summary |
| --- | --- |
| manager | The manager module orchestrates the lifecycle and query execution for Flapjack's search indexes, handling configuration, recovery, publication, tokenization, and vector operations while delegating ranking and search-phase coordination to specialized submodules. |
| rules | The rules directory implements an Algolia-compatible query rules engine that matches patterns, rewrites queries, applies facet filters, and evaluates rules using first-match-wins semantics. |
| settings_tests | — |
| write_queue | Write_queue manages asynchronous batching and queueing of document write operations (add, upsert, delete) to the Tantivy index, with metrics tracking for each phase and integration with vector embeddings, operation logs, and facet caching. |
<!-- [scrai:end] -->
