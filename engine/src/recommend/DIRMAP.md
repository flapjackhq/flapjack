<!-- [scrai:start] -->
## recommend

| File | Summary |
| --- | --- |
| cooccurrence.rs | Co-occurrence engine for related-products and bought-together models.



Builds per-user item interaction sets from insight events, computes

item-item co-occurrence counts, and returns scored recommendations. |
| looking_similar.rs | Vector similarity engine for the looking-similar recommendation model using vector embeddings. |
| mod.rs | Recommendation engine for trending items, facets, related products, and bought-together models using analytics insight events. |
| rules.rs | Storage layer for recommendation rules per index and model, supporting CRUD operations, batch upsert/delete, searching with pagination, and path traversal protection. |
| trending.rs | Trending items and trending facets aggregation.



Queries conversion events from the analytics engine and computes

trending scores based on frequency weighted by recency. |
<!-- [scrai:end] -->
