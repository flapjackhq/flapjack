# Flapjack - Project Overview

**Last updated:** 2026-08-12

## Mission

Flapjack is a drop-in replacement for Algolia: a typo-tolerant full-text search engine with faceting, geo search, custom ranking, vector search, and click analytics.
It keeps the Algolia-compatible client and InstantSearch.js surface working while running as a single static binary with data stored on disk.

## Highest Priority

**Adoption status, stated once so no other document has to guess: Flapjack has no users and no customers.** `v1.0.11` published 2026-08-06 and the source is public,
but nobody runs it in production, Flapjack Cloud is not live, and no revenue exists. Corrected 2026-08-02: an earlier commercial-adoption claim was false and had
been propagating into risk arguments that assumed real workloads. **(1) Breaking changes are cheap** — no installed base to migrate, so prefer the correct design
over the compatible one wherever they conflict, except on the Algolia-compatible wire surface, which is the product. **(2) Nothing is urgent because it is "in
production" — the urgency is that we cannot yet honestly ask anyone to adopt it.** The bar is not "no known incidents"; it is "we can prove the claims we make."

**Current priority order, row state, and executable exits live only in [`ROADMAP.md`](ROADMAP.md), especially `## Up Next`.** This overview owns the enduring reason for that order: local correctness comes before outward publication, adoption claims require fail-capable evidence, and build-heavy proofs must not overlap when host contention would invalidate them. Keeping the numbered work list out of this file prevents two priority owners from drifting apart.

Release history and shipped-feature lineage stay in [`CHANGELOG.md`](CHANGELOG.md) and
[`engine/docs2/FEATURES.md`](engine/docs2/FEATURES.md). `PROJECT_OVERVIEW.md` owns
mission, scope, adoption truth, and product-surface policy; it does not duplicate the open-work ledger.

## Product surfaces

`engine/dashboard/` (React) is **the shipping UI, and it is frozen against new screens as of 2026-08-08.** This section is the single owner of that policy:
`.scrai/overview.md` — which assembles into `CLAUDE.md` and `AGENTS.md` — deliberately states no scheduling policy and points every agent here, so this is
the only place the answer exists. **Corrected 2026-08-08: this section previously read "not frozen and not scheduled for replacement," which was false in both
halves and contradicted this file's own product-surface policy two sections above it.** That is `DOC-SSOT-1`'s defect recurring one day after it closed — not as a stale
restatement this time, but as an owner that disagreed with itself, which is the harder version to notice.

The policy, sourced from the decision rather than restated from memory: [ADR 0006](engine/docs2/3_IMPLEMENTATION/decisions/active/0006_console_source_home.md)
has been **Accepted since 2026-07-18** — one Svelte console replaces both this dashboard and the managed cloud console, and this tree is deleted at the
program's **parity-gated** cutover (the program's phases live in the private `fjcloud_dev` console-unification program plan, not here). Two consequences
that are easy to get backwards:

- **Deletion is gated on parity, not scheduled on a date.** ADR 0006 states `engine/dashboard` remains the shipping UI until that cutover and that nothing in
  this repo changed at decision time. Do not read the freeze as "already replaced," and do not stop fixing it.
- **The freeze forbids ADDING a route, not editing one.** Enforced by `engine/dashboard/src/App.routeFreeze.test.tsx`, which pins the 24 route paths in
  `App.tsx`. Removing a route is allowed because that is migration progress; bugfix, CI, and security work inside existing screens stays legitimate until
  cutover, because a freeze that blocks repairs gets routed around within a week. A feature added inside an existing screen is not caught — a stated bound.

The enforcement exists because the decision alone did not hold: in the two weeks after 2026-07-25 this tree took 164 commits and +17,157 / −10,814 lines into a
codebase already decided for deletion. Shipped dashboard capability is described in [`engine/docs2/FEATURES.md`](engine/docs2/FEATURES.md), not here.

## Scope

- Maintain Algolia API compatibility for existing client integrations.
- Provide one-time migration into Flapjack from Algolia, Meilisearch, and
  Typesense through shared lifecycle/publication owners and source-specific
  adapters.
- Keep search latency low and memory usage bounded under realistic workloads.
- Extend analytics, vector search, HA, and operational tooling without increasing
  operator complexity.
- Keep public documentation routed through canonical owners: `PROJECT_OVERVIEW.md` for
  mission and priority order, [`ROADMAP.md`](ROADMAP.md) for open work,
  [`engine/docs2/FEATURES.md`](engine/docs2/FEATURES.md) for shipped capability status,
  and [`CHANGELOG.md`](CHANGELOG.md) for release history.

## Non-Goals

- Recreating Algolia's hosted control plane or proprietary infrastructure.
- Moving public roadmap state into private chats, evidence bundles, or ad hoc
  release notes.
- Treating beta release history as the active work ledger.
- Weakening durability, API compatibility, or validation gates to improve raw
  throughput numbers.
