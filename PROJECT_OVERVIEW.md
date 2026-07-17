# Flapjack - Project Overview

**Last updated:** 2026-07-16

## Mission

Flapjack is a drop-in replacement for Algolia: a typo-tolerant full-text search
engine with faceting, geo search, custom ranking, vector search, and click
analytics. It keeps the Algolia-compatible client and InstantSearch.js surface
working while running as a single static binary with data stored on disk.

## Highest Priority

Public paid beta is shipped. The current strategic order is:

1. **MIG-1 - `/1/migrate-from-algolia` reports success without importing.**
   Migration off Algolia is the front door for the users this project exists to
   win: a drop-in replacement that cannot ingest a competitor's index has no
   adoption path. `main` currently tells that user their migration completed and
   leaves them an empty index, which is worse than an honest failure. A release
   hold is in effect until this lands. See [`ROADMAP.md`](ROADMAP.md).
2. **MIG-2 / MIG-3 - restore the import leg truthfully.** Translation matrix,
   then spool → translate → staged publish → target index, proven end-to-end
   against a real Algolia account.
3. **RF-4 - runbooks iteration.** Keep operational routing in
   [`engine/docs2/3_IMPLEMENTATION/OPERATIONS.md`](engine/docs2/3_IMPLEMENTATION/OPERATIONS.md)
   and continue folding incident learnings into the runbooks.
4. **PL-10 - write-path saturation under sustained load.** The v1.0.4 batch-size
   tuning knob and v1.0.5 `TUNABLE_VERIFIED` harness gate are shipped, but the
   single-writer Tantivy ceiling remains the architectural constraint for v1.1.
   Current evidence routing lives in [`ROADMAP.md`](ROADMAP.md).
5. **Post-ship HA/test-signal hygiene.** HA snapshot flake remediation is
   verified, and the remaining signal-protection work is tracked in
   [`ROADMAP.md`](ROADMAP.md).
6. **ADR-0005 OQ4 - cross-node failover idempotency dedup.** Node-local
   restart-durable idempotency is shipped; cross-node dedup remains a v1.1
   planned item.

Release history and shipped-feature lineage stay in [`CHANGELOG.md`](CHANGELOG.md)
and [`engine/docs2/FEATURES.md`](engine/docs2/FEATURES.md). `PROJECT_OVERVIEW.md`
does not duplicate that status ledger.

## Scope

- Maintain Algolia API compatibility for existing client integrations.
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
