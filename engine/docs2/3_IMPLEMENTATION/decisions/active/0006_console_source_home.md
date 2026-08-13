# Decision 0006: Console Source Home — the shared Svelte console lives in the Flapjack repo
<!-- markdownlint-disable MD013 -->

Date: 2026-07-18
Status: Accepted

## Purpose & Scope

Records the operator decision (2026-07-18) on the one contested question in the console-unification
program: which repository owns the shared console source. This ADR is the source of truth for the
ownership decision. The program itself (phases, QA gates, sequencing) is owned by the program plan
in the cloud-platform dev repo — fjcloud_dev `docs/design/console_unification_revised_plan.md`
(v7) — which was amended in place the same day to record this decision.

In scope: source home, dependency/pin direction, licensing owner. Out of scope: dispatching any
extraction or migration phase (the plan's P1–P5 own that), connector/plugin UI, and any change to
the shipping React dashboard before the plan's parity-gated cutover.

## Context

Two consoles exist for the same engine today: the managed cloud platform ships a Svelte 5 console
inside its SvelteKit app, and this repo ships the React `engine/dashboard` embedded in the release
binary via rust-embed (`engine/flapjack-http/build.rs`, `engine/flapjack-http/src/handlers/dashboard.rs`).
The console-unification program converges both on ONE Svelte implementation consumed by two thin
hosts (managed transport vs engine transport over one portable `ApiClient`). Every part of that
design survived seven adversarial review rounds except the source home: the plan's v1–v6 kept the
shared source in the cloud repo, with the OSS release build fetching it from the public fjcloud
mirror at a pinned SHA. Three independent review sessions concluded the opposite. Decision docs
argued both sides; the operator decided 2026-07-18.

## Decision

The shared console source lives in this repo, flapjack_dev (working target `engine/console/`;
exact layout finalized at the plan's P3 extraction phase). The cloud platform consumes a pinned
Flapjack revision (a 40-hex SHA lock file in its own web tree, fetched by its own CI/deploy).
Flapjack's release keeps building its entire product — engine and console — from its own tree;
no cross-repo fetch ever enters the OSS release path. The console is MIT like the rest of this
repo. Strategic framing is OSS-primary.

## Rationale

1. **Acyclic dependency direction.** The cloud platform already consumes Flapjack (the engine
   binary in its fleet images and in its e2e harness). The rejected direction would have created
   the first Flapjack→cloud-repo edge — a cross-repo cycle placing a closed repo on the OSS
   product's release path.
2. **Pin ground truth.** The rejected direction pinned OSS releases to the public fjcloud mirror —
   a derived, regenerable sync artifact whose entire history was scheduled for deletion and
   recreation at decision time. `flapjackhq/flapjack` has external consumers and releases; its
   history stability is a product requirement. An immutable-SHA pin belongs on the stable repo.
3. **Self-containment and licensing.** `git clone` + build keeps producing the whole MIT product
   from one repo. The alternative required granting MIT to a subtree of an unlicensed
   all-rights-reserved repo and made OSS builds depend on it — a flag for any downstream
   due-diligence scan.
4. **Ownership follows the stake.** After cutover the console is 100% of Flapjack's UI versus a
   minority of the cloud app's routes, and effort allocation is OSS-primary.

Cost accepted, stated fairly: managed-side console iteration becomes edit-here + pin-bump in the
cloud repo. Mitigations: sibling-checkout dev loop (the cloud repo already resolves the engine
binary that way) and one-line pin bumps batched like the SDK repos' established cross-repo cadence.
The extraction refactor cost is identical under either home (measured 2026-07-18: the cloud app's
`$lib/format` has 67 importers, 32 console / 35 managed) and was not a deciding factor.

## Consequences

- The engine and its console version together: a single commit here can adapt the console to an
  engine API change atomically, and in the OSS binary console and engine ship from the same commit.
- The engine API additions the shared console needs (extend `/1/usage` with the two point-in-time
  gauge statistics; an abtests exact-index filter param) become ordinary in-repo work. They are
  public surface, so their final shape gets a deliberate sign-off at the plan's P4 intake.
- `engine/dashboard` (React) remains the shipping UI until the plan's parity-gated P5 cutover;
  nothing in this repo changes at decision time. The cutover-adjacent proof obligations recorded
  in the plan's R4/R5 include the fail-closed embedded-asset gate (a release build must fail, not
  embed a stub, when real dashboard assets are absent) and the SPA fallback fix for client routes
  containing dots (legal in index names) in `engine/flapjack-http/src/handlers/dashboard.rs`.
- The cloud repo carries the pin and its bump runbook; its rollback is reverting the pin.

## References

- Program plan (SSOT for the program): fjcloud_dev `docs/design/console_unification_revised_plan.md`
  (v7, 2026-07-18) — its "Operator decisions — SETTLED" section records the companion answers
  (MIT license, OSS-primary framing, mirror-state disposition) and its amended R2 mirrors this ADR.
- The pro/contra decision docs that argued both sides were retired 2026-07-18 and are preserved in
  fjcloud_dev git history.
