<!-- [scrai:start] -->
## src

| File | Summary |
| --- | --- |
| build_info.rs | Stub summary for engine/src/build_info.rs. |
| error.rs | Define the unified error type for the Flapjack engine, mapping each variant to an HTTP status code and an Algolia-compatible JSON error response with sanitized messages for internal failures. |
| filter_parser.rs | Nom-based parser for boolean filter expressions (comparisons, AND/OR/NOT, grouping) and a `filter_implies` check that determines whether search filters satisfy a rule condition's facet requirements using attribute-scoped exact-match semantics. |
| language.rs | Stub summary for engine/src/language.rs. |
| lib.rs | # Flapjack



A full-text search engine library with typo tolerance, faceting, and

Algolia-compatible document conventions. |
| security.rs | Stub summary for security.rs. |
| security_tests.rs | Stub summary for security_tests.rs. |
| text_normalization.rs | Text normalization utilities for search, including diacritic removal with exceptions, character folding, and camelCase word splitting. |
| types.rs | Type definitions and conversions for documents, field values, filters, search queries, and results. |

| Directory | Summary |
| --- | --- |
| analytics | The analytics directory implements a comprehensive search analytics engine powered by DataFusion and Parquet, tracking search events, click-throughs, and conversions with efficient querying, aggregation, and merge strategies. |
| dictionaries | The dictionaries module manages per-tenant custom dictionaries for stopwords, plurals, and compounds with atomic on-disk persistence, exposed through a centralized DictionaryManager entry point. |
| experiments | The experiments module provides a complete A/B testing framework for search variants, including deterministic user-to-variant assignment via hashing, result interleaving, statistical analysis (delta-method, Welch's t-tests, Bayesian methods), and persistent experiment lifecycle management backed by atomic file writes and Parquet metrics aggregation. |
| index | The index module provides the core indexing and search coordination layer for Flapjack, managing schema definitions, document conversion, settings, write queues, metadata persistence, and the full index lifecycle including recovery and publication. |
| integ_tests | Integration tests for the Flapjack library that are inlined to avoid nextest process-per-test overhead, exercising library APIs directly without HTTP servers or cross-crate type sharing. |
| personalization | Computes per-user personalization profiles by building affinities from interaction events and document facets using a weighted scoring formula, with results normalized so the strongest affinity equals 20. |
| query | The query module handles end-to-end search query processing, converting user input into Tantivy query trees with support for typo tolerance, fuzzy matching, phrase queries, multilingual plural expansion, and stopword filtering. |
| query_suggestions | The query_suggestions module manages configuration files, build status records, and newline-delimited JSON logs for query suggestion generation, storing them in a protected .query_suggestions directory. |
| recommend | The recommend directory implements a multi-model recommendation engine that generates suggestions for trending items, related products, bought-together recommendations, and similar items using analytics events, vector embeddings, and co-occurrence analysis. |
| tokenizer | The tokenizer module provides text segmentation and filtering for search indexing, with specialized support for CJK (Chinese, Japanese, Korean) languages and edge n-gram filtering for prefix-based matching in typeahead and search features. |
| vector | Implements vector search capabilities with HNSW-based indexing for persistent storage, including embedder configuration and extraction of user-provided vector embeddings from documents with per-embedder validation. |
<!-- [scrai:end] -->
