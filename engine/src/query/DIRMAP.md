<!-- [scrai:start] -->
## query

| File | Summary |
| --- | --- |
| algolia_filters.rs | Algolia filter string parsing utilities for converting JSON specifications (facet, numeric, tag, optional) into Filter AST nodes with AND/OR composition. |
| decompound.rs | Stub summary for engine/src/query/decompound.rs. |
| filter.rs | Stub summary for engine/src/query/filter.rs. |
| fuzzy.rs | Fuzzy query building with automatic edit-distance adjustment based on term length. |
| geo.rs | Stub summary for geo.rs. |
| highlighter.rs | Stub summary for engine/src/query/highlighter.rs. |

| Directory | Summary |
| --- | --- |
| executor | The executor directory handles query result processing and transformation, including facet collection, sorting by columnar fields with fallback JSON extraction, relevance ranking, and rule application. |
| parser | The parser directory contains the QueryParser struct and its implementation for converting search queries into Tantivy query trees, supporting features like typo tolerance, fuzzy matching, advanced syntax (quoted phrases and word exclusions), per-field weighting, morphological stemming, and CJK-aware tokenization. |
| plurals | The plurals module provides multilingual plural expansion support for seven European languages (English, French, German, Spanish, Portuguese, Italian, and Dutch) using both rule-based and dictionary-driven pluralization strategies. |
| stopwords | Provides multilingual stopword filtering for search queries across 30 languages with configurable removal modes (disabled, all, or language-specific), supporting query-type semantics like preserving prefix tokens to maintain search intent. |
<!-- [scrai:end] -->
