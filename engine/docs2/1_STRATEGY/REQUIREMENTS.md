# Requirements

**Goal:** Algolia drop-in replacement for cost-sensitive ecommerce

**Target:** Shopify/WooCommerce merchants paying $50-500/month

**Value prop:** 10x cost reduction, 99% feature parity

**Customer acquisition:** Not pursuing customers until Phase 2-3 complete. Current focus is technical completeness and validation.

## Phase 1: Core Indexing + Validation ✅ COMPLETE

**Implemented:**
- Schemaless JSON upload (dual fields)
- All CRUD operations (batch + single object)
- Faceted search with hierarchical drill-down (selective indexing)
- Query with typo tolerance + prefix search
- Numeric/string filters (Algolia syntax)
- Multi-field queries with boost weights
- SDK v5 contract test suite (24/24 passing)
- Settings API (faceting, searchableAttributes, ranking)
- Browse, deleteByQuery, bulk operations
- Algolia compatibility test suite (8/8 passing)

**Exit criteria met:**
- ✅ 24/24 SDK contract tests passing
- ✅ 8/8 Algolia comparison tests passing
- ✅ Immediate consistency for single operations
- ✅ Eventual consistency for batch operations
- ✅ Facet indexing matches Algolia behavior
- ✅ No breaking API changes needed

**Time:** 5 days (Jan 22-23, 2026)

## Phase 2: Settings & Query Features

- Configurable ranking formulas
- Synonyms management
- Stop words configuration
- Distinct parameter (variant deduplication)
- Multi-query support (disjunctive faceting)

Exit: Feature-complete for ecommerce migration

## Phase 3: Enterprise

- Geo-search
- Replication (multi-node)
- Analytics API
- A/B testing

## Memory Safety (Critical Differentiator)

See: docs2/3_IMPLEMENTATION/ARCHITECTURE.md

- Per-tenant buffer: 31MB enforced
- Concurrent writers: 40 system-wide enforced
- Record size (HTTP layer): 100KB default, configurable via `FLAPJACK_MAX_RECORD_BYTES` → HTTP 413
- Document size (write queue backstop): 3MB enforced
- No OOM crashes (Meilisearch/Algolia pain point)

## Performance Targets

- Query P99: <50ms (text + filter + sort + facet)
- Write P99: <50ms (batched commits)
- Migration downtime: <1s per tenant
- Uptime: 99.9% (Algolia Standard parity)

## Capacity Model

See: docs2/1_STRATEGY/CONSTRAINTS.md

- Target: 600 tenants per 4GB node
- Cost: <$0.05/tenant/month infrastructure
- Pricing: $1-2/tenant/month (50-80% undercut Algolia)

## Explicit Non-Requirements

- Perfect API compatibility (prevents differentiation)
- Meilisearch/Typesense/Elasticsearch support (different markets)
- Multi-region writes (Phase 3+)
- ML/semantic search (different product)
