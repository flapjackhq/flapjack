# Flapjack - Project Overview

**Last updated:** 2026-07-31

## Mission

Flapjack is a drop-in replacement for Algolia: a typo-tolerant full-text search
engine with faceting, geo search, custom ranking, vector search, and click
analytics. It keeps the Algolia-compatible client and InstantSearch.js surface
working while running as a single static binary with data stored on disk.

## Highest Priority

Public paid beta is shipped. The current strategic order is:

1. **DUR-1 - rejected writes replay into the index after restart.** A batch
   rejected to the client with HTTP `500` under disk exhaustion becomes visible
   after restart, reproduced 5/5. This is the only known correctness defect on
   `main`, and durability is the property the whole product rests on, so it
   outranks everything below it. Owners and the smallest unblocking change are
   in [`ROADMAP.md`](ROADMAP.md) row `DUR-1`.
2. **Security Wave 1 remainder - SEC-G3 and SEC-G8.** Response headers / CSP,
   the dashboard admin key in `localStorage`, and request timeout / concurrency
   limit / panic containment. The rest of Wave 1 shipped on 2026-07-31; these
   two were gated on `router.rs` and are now unblocked.
3. **RF-4 - runbooks iteration.** Keep operational routing in
   [`engine/docs2/3_IMPLEMENTATION/OPERATIONS.md`](engine/docs2/3_IMPLEMENTATION/OPERATIONS.md)
   and continue folding incident learnings into the runbooks.
4. **PL-10 - write-path saturation under sustained load.** The v1.0.4 batch-size
   tuning knob and v1.0.5 `TUNABLE_VERIFIED` harness gate are shipped, but the
   single-writer Tantivy ceiling remains the architectural constraint for v1.1.
   The repaired July 26 reference contract now proves 1,000,000 compact and
   1,000,000 standard records through every frozen text-search gate. Do not
   profile the superseded July 25 liveness/latency failures as current product
   defects; further scale work needs a new falsifiable question beyond the
   verified floor. Current open work lives in [`ROADMAP.md`](ROADMAP.md), and
   measured curves live only in
   [`engine/loadtest/BENCHMARKS.md`](engine/loadtest/BENCHMARKS.md).
5. **Post-ship HA/test-signal hygiene.** HA snapshot flake remediation is
   verified, and the remaining signal-protection work is tracked in
   [`ROADMAP.md`](ROADMAP.md).
6. **Backend↔frontend joined proof (JOIN-1).** The 2026-07-30 matrix found 0 of
   90 backend capability rows with a current passing joined proof. The
   credential residual that blocked re-proof is resolved; the runner and
   reporter residuals are not. The dashboard is frozen pending Svelte
   replacement, so this is about honest proof and reporting, not new screens.
7. **Migration deferred-scope follow-through.** Create-only import, fenced
   existing-target overwrite, and interrupted-job resume are all shipped on the
   synchronous and authenticated async Algolia paths as of 2026-07-31.
   HA-converging import remains refused under [`ROADMAP.md`](ROADMAP.md) row
   `MIG-7`. Pre-launch source migration is expanding to Meilisearch and
   Typesense through one provider-neutral lifecycle; M0 contract/security work
   must land before product adapters.
8. **ADR-0005 OQ4 - cross-node failover idempotency dedup.** Node-local
   restart-durable idempotency is shipped; cross-node dedup remains a v1.1
   planned item.

Release history and shipped-feature lineage stay in [`CHANGELOG.md`](CHANGELOG.md)
and [`engine/docs2/FEATURES.md`](engine/docs2/FEATURES.md). `PROJECT_OVERVIEW.md`
does not duplicate that status ledger.

## Scope

- Maintain Algolia API compatibility for existing client integrations.
- Provide one-time migration into Flapjack from Algolia, Meilisearch, and
  Typesense through shared lifecycle/publication owners and source-specific
  adapters.
- Keep search latency low and memory usage bounded under realistic workloads.
- Extend analytics, vector search, HA, and operational tooling without increasing
  operator complexity.
- Keep public documentation routed through canonical owners:
  `PROJECT_OVERVIEW.md` for mission and priority order, [`ROADMAP.md`](ROADMAP.md)
  for open work, [`engine/docs2/FEATURES.md`](engine/docs2/FEATURES.md) for
  shipped capability status, and [`CHANGELOG.md`](CHANGELOG.md) for release
  history.

## Non-Goals

- Recreating Algolia's hosted control plane or proprietary infrastructure.
- Moving public roadmap state into private chats, evidence bundles, or ad hoc
  release notes.
- Treating beta release history as the active work ledger.
- Weakening durability, API compatibility, or validation gates to improve raw
  throughput numbers.
