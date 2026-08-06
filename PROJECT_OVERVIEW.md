# Flapjack - Project Overview

**Last updated:** 2026-08-05

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

1. **`REL-11` — the release path, which now gates every other item in this list.**
   The 2026-08-05 v1.0.11 cut was attempted and stopped on two packaging defects,
   so users still resolve `v1.0.10` from 2026-06-09. It gates both channels, not
   just OSS: Flapjack Cloud bakes its engine AMI from the same published release
   archive and manifest, so **no engine capability described below has actually
   reached a user by either route.** Ranked first because it is the only row whose
   closure changes that. Owner and exit: [`ROADMAP.md`](ROADMAP.md) row `REL-11`.

2. **`NIGHT-1` — twenty consecutive red nights on the public mirror.** Every
   `nightly.yml` run from 2026-07-17 to 2026-08-05 failed in `Migration import
   contract`, and the current cause makes it report red while the product passes.
   Ranked second because it means the only recurring real-Algolia end-to-end guard
   produced no signal across the whole period the migration feature set — the one
   being sold — was rebuilt. Owner and exit: [`ROADMAP.md`](ROADMAP.md) row `NIGHT-1`.

3. **Security follow-through — the `SEC-W4` hygiene residuals.** The serving-path
   security story has stopped being a capability question: HTTPS serves from
   operator-supplied PEM files or from ACME-issued material that rotates on a
   running listener without a restart, replication peers carry a mandatory
   credential over non-cleartext transport, and the console holds no key material
   in browser storage. What is left is hygiene, and it is small: a disposition for
   the production moderate advisories sitting below the audit gate's
   high-and-above threshold, and the plaintext tarball snapshot helpers
   (`SEC-G5`), which have now been carried unscheduled across five consecutive
   batches and are scheduled a wave slot or an explicit written acceptance rather
   than a sixth roll forward. Owner and exit: [`ROADMAP.md`](ROADMAP.md) row `SEC-W4`, which routes
   the control-level gap detail.

4. **RF-4 — runbooks iteration.** Keep operational routing in
   [`engine/docs2/3_IMPLEMENTATION/OPERATIONS.md`](engine/docs2/3_IMPLEMENTATION/OPERATIONS.md)
   and keep folding incident learnings into the runbooks.

5. **PL-10 — write-path saturation under sustained load.** The single-writer
   Tantivy ceiling remains the architectural constraint for v1.1. Two standing
   warnings: further scale work needs a *new falsifiable question* beyond the
   verified floor rather than another run, and the superseded July 25 failures
   must not be profiled as current defects. Owner and exit:
   [`ROADMAP.md`](ROADMAP.md) row `PL-10`; measured curves live only in
   [`engine/loadtest/BENCHMARKS.md`](engine/loadtest/BENCHMARKS.md).

6. **Post-ship HA/test-signal hygiene.** HA snapshot flake remediation is
   verified; remaining signal-protection work is tracked in
   [`ROADMAP.md`](ROADMAP.md).

7. **Backend↔frontend joined proof (JOIN-1).** Two corrections from 2026-08-03
   must be inherited by anyone reading this: **the joinable denominator is 59, not
   90**, and **the number is now a command**, not a hand audit — so do not
   re-derive it. **Policy reversal, 2026-08-02: the React dashboard is no longer
   frozen and is not scheduled for replacement.** It is the product's console and
   is maintained like any other surface; every "deferred to the Svelte console"
   disposition recorded before that date is void. Owner and exit:
   [`ROADMAP.md`](ROADMAP.md) row `JOIN-1`.

8. **Migration deferred-scope follow-through.** Create-only import, fenced
   existing-target overwrite, and interrupted-job resume all ship on the
   synchronous and authenticated async Algolia paths. HA-converging import stays
   refused under [`ROADMAP.md`](ROADMAP.md) row `MIG-7`. Pre-launch source
   migration is expanding to Meilisearch and Typesense through one
   provider-neutral lifecycle; discovery ships for all three providers, and
   resume remains Algolia-only.

9. **ADR-0005 OQ4 — cross-node failover idempotency dedup.** Node-local
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
