# Flapjack - Project Overview

**Last updated:** 2026-08-04

## Mission

Flapjack is a drop-in replacement for Algolia: a typo-tolerant full-text search
engine with faceting, geo search, custom ranking, vector search, and click
analytics. It keeps the Algolia-compatible client and InstantSearch.js surface
working while running as a single static binary with data stored on disk.

## Highest Priority

**Adoption status, stated once so no other document has to guess: Flapjack has no
users and no customers.** Releases exist (`v1.0.10`, 2026-06-09) and the source is
public, but nobody is running it in production, Flapjack Cloud is not live, and
no revenue exists. Corrected 2026-08-02: an earlier commercial-adoption claim
was false and had been propagating into risk arguments that assumed real
workloads. Two consequences bind every planning
decision below. **(1) Breaking changes are cheap** — there is no installed base
to migrate, so prefer the correct design over the compatible one wherever they
conflict, except on the Algolia-compatible wire surface, which is the product.
**(2) Nothing is urgent because it is "in production" — the urgency is that we
cannot yet honestly ask anyone to adopt it.** The bar is not "no known incidents";
it is "we can prove the claims we make."

The current strategic order is:

1. **Security follow-through — the `SEC-W4` hygiene residuals.** The serving-path
   security story has stopped being a capability question: HTTPS serves from
   operator-supplied PEM files or from ACME-issued material that rotates on a
   running listener without a restart, replication peers carry a mandatory
   credential over non-cleartext transport, and the console holds no key material
   in browser storage. What is left is hygiene, and it is small: a disposition for
   the production moderate advisories sitting below the audit gate's
   high-and-above threshold, and the plaintext tarball snapshot helpers
   (`SEC-G5`), which have been carried unscheduled across four consecutive batches
   and should get a lane or an explicit acceptance rather than a fifth roll
   forward. Owner and exit: [`ROADMAP.md`](ROADMAP.md) row `SEC-W4`, which routes
   the control-level gap detail.

2. **RF-4 — runbooks iteration.** Keep operational routing in
   [`engine/docs2/3_IMPLEMENTATION/OPERATIONS.md`](engine/docs2/3_IMPLEMENTATION/OPERATIONS.md)
   and keep folding incident learnings into the runbooks.

3. **PL-10 — write-path saturation under sustained load.** The single-writer
   Tantivy ceiling remains the architectural constraint for v1.1. The repaired
   reference contract proves 1,000,000 compact records through the frozen
   text-search gates; the 1,000,000 standard-record run reached a measured
   facet-query p95 ceiling, not a published scale record. Further scale work
   needs a *new falsifiable question* beyond the verified floor rather than
   another run. Do not profile the superseded July 25 failures as current
   defects. Owner and exit: [`ROADMAP.md`](ROADMAP.md) row `PL-10`; measured
   curves live only in
   [`engine/loadtest/BENCHMARKS.md`](engine/loadtest/BENCHMARKS.md).

4. **Post-ship HA/test-signal hygiene.** HA snapshot flake remediation is
   verified; remaining signal-protection work is tracked in
   [`ROADMAP.md`](ROADMAP.md).

5. **Backend↔frontend joined proof (JOIN-1).** 90 backend capability rows, and
   for three consecutive lanes zero of them had a current passing joined proof.
   Two corrections landed 2026-08-03 and both must be inherited: **the joinable
   denominator is 59, not 90** (27 rows are API-, config-, or CLI-only and four
   more have no candidate spec, so `0 / 90` was unreachable by construction), and
   **the number is now a command** rather than a hand audit — a Playwright `json`
   reporter, a row→key manifest, and `engine/dashboard/scripts/join_proof_report.mjs`
   make it a side effect of any full-suite run. **Policy reversal, 2026-08-02: the
   React dashboard is no longer frozen and is not scheduled for replacement.** It
   is the product's console and is maintained and extended like any other surface;
   every "deferred to the Svelte console" disposition recorded before that date is
   void. So this priority is both *proof* and *repair*.

6. **Migration deferred-scope follow-through.** Create-only import, fenced
   existing-target overwrite, and interrupted-job resume all ship on the
   synchronous and authenticated async Algolia paths. HA-converging import stays
   refused under [`ROADMAP.md`](ROADMAP.md) row `MIG-7`. Pre-launch source
   migration is expanding to Meilisearch and Typesense through one
   provider-neutral lifecycle; discovery ships for all three providers, and
   resume remains Algolia-only.

7. **ADR-0005 OQ4 — cross-node failover idempotency dedup.** Node-local
   restart-durable idempotency ships; cross-node dedup remains a v1.1 planned
   item.

Release history and shipped-feature lineage stay in [`CHANGELOG.md`](CHANGELOG.md)
and [`engine/docs2/FEATURES.md`](engine/docs2/FEATURES.md). `PROJECT_OVERVIEW.md`
owns mission and priority order; it does not duplicate that status ledger.

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
