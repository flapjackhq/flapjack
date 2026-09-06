<!-- [scrai:start] -->
## manager

| File | Summary |
| --- | --- |
| config.rs | Stub summary for engine/src/index/manager/config.rs. |
| lifecycle.rs | Stub summary for engine/src/index/manager/lifecycle.rs. |
| lifecycle_move_tests.rs | Stub summary for engine/src/index/manager/lifecycle_move_tests.rs. |
| mod.rs | Stub summary for engine/src/index/manager/mod.rs. |
| publication.rs | Stub summary for engine/src/index/manager/publication.rs. |
| publication_startup_tests.rs | Stub summary for engine/src/index/manager/publication_startup_tests.rs. |
| query.rs | Query parameter resolution and stopword filtering for search operations, including filter merging, language selection, and stopword removal with dictionary manager fallback. |
| recovery.rs | Stub summary for engine/src/index/manager/recovery.rs. |
| search.rs | Stub summary for engine/src/index/manager/search.rs. |
| tests.rs | Stub summary for engine/src/index/manager/tests.rs. |
| tokenization.rs | Tokenization utilities for extracting and normalizing searchable text from structured document fields. |
| vector.rs | Stub summary for engine/src/index/manager/vector.rs. |
| write.rs | Stub summary for engine/src/index/manager/write.rs. |

| Directory | Summary |
| --- | --- |
| publication | This directory implements the index publication pipeline, including scanning for corrupted indexes, detecting faults, executing repairs, and managing publication state through digest/inventory tracking. |
| ranking | The ranking module implements Flapjack's multi-criteria search result ranking pipeline, which sorts documents by built-in criteria (typo tolerance, proximity, attribute position, exact vs prefix matches, matched query words, and optional filters) before falling back to custom ranking and BM25 tie-breaking. |
| search_phases | The search_phases module orchestrates query parsing, execution, and limit calculation for the `execute_search_query` function, with support for plural expansion and helper utilities. |
<!-- [scrai:end] -->
