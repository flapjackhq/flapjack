# Flapjack - Project Overview

**Last updated:** 2026-06-05

## Mission

Flapjack is a drop-in replacement for Algolia: a typo-tolerant full-text search
engine with faceting, geo search, custom ranking, vector search, and click
analytics. It keeps the Algolia-compatible client and InstantSearch.js surface
working while running as a single static binary with data stored on disk.

## Highest Priority

Public paid beta is shipped. The current strategic order is:

1. **RF-4 - runbooks iteration.** Keep operational routing in
   [`engine/docs2/3_IMPLEMENTATION/OPERATIONS.md`](engine/docs2/3_IMPLEMENTATION/OPERATIONS.md)
   and continue folding incident learnings into the runbooks.
2. **PL-10 - write-path saturation under sustained load.** The v1.0.4 batch-size
   tuning knob and v1.0.5 `TUNABLE_VERIFIED` harness gate are shipped, but the
   single-writer Tantivy ceiling remains the architectural constraint for v1.1.
   Current evidence routing lives in [`ROADMAP.md`](ROADMAP.md).
3. **Post-ship HA/test-signal hygiene.** HA snapshot flake remediation is
   verified, and the remaining signal-protection work is tracked in
   [`ROADMAP.md`](ROADMAP.md).
4. **ADR-0005 OQ4 - cross-node failover idempotency dedup.** Node-local
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
