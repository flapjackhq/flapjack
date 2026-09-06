<!-- [scrai:start] -->
## dictionaries

| File | Summary |
| --- | --- |
| mod.rs | Custom dictionaries API types and serialization for stopwords, plurals, and compounds, scoped per-tenant rather than per-index. |
| persistence.rs | On-disk persistence for per-tenant dictionary data (stopwords, plurals, compounds, settings) using atomic temp-file-plus-rename writes. |

| Directory | Summary |
| --- | --- |
| manager | The manager directory contains the DictionaryManager, which serves as the single entry point for all dictionary operations across multi-tenant data directories. |
<!-- [scrai:end] -->
