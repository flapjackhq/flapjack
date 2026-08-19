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
exact layout finalized at the plan's P3 extraction phase). The selected cloud consumption
mechanism is an exact Flapjack revision carried by an additive, non-editable vendor snapshot plus a
lock and staleness check; no public build-time fetch or delete-capable sync enters the cloud build.
Flapjack's release keeps building its entire product — engine and console — from its own tree, so
no cross-repo fetch enters the OSS release path. The console follows the repository's root license
map: currently Elastic License 2.0, while the SDK and integration exceptions remain owned by
`NOTICE` and their path-specific licenses. Strategic framing is OSS-primary.

## Rationale

1. **Acyclic dependency direction.** The cloud platform already consumes Flapjack (the engine
   binary in its fleet images and in its e2e harness). The rejected direction would have created
   the first Flapjack→cloud-repo edge — a cross-repo cycle placing a closed repo on the OSS
   product's release path.
2. **Pin ground truth.** The rejected direction pinned OSS releases to the public fjcloud mirror —
   a derived, regenerable sync artifact whose entire history was scheduled for deletion and
   recreation at decision time. `flapjackhq/flapjack` has external consumers and releases; its
   history stability is a product requirement. An immutable-SHA pin belongs on the stable repo.
3. **Self-containment and licensing.** `git clone` + build keeps producing the engine and console
   from one repository under its root license map. The alternative required a separately licensed
   subtree in an otherwise unlicensed cloud repository and made OSS builds depend on it — a flag
   for any downstream due-diligence scan.
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
- The cloud repo will carry the lock, additive vendor snapshot, staleness check, and bump runbook;
  its rollback will revert the lock and snapshot together.

## References

- Program plan (SSOT for the program): fjcloud_dev `docs/design/console_unification_revised_plan.md`
  (v7, 2026-07-18) — its "Operator decisions — SETTLED" section records the companion answers
  (root license map, OSS-primary framing, mirror-state disposition) and its amended R2 mirrors this
  ADR.
- The pro/contra decision docs that argued both sides were retired 2026-07-18 and are preserved in
  fjcloud_dev git history.

## R2 re-derivation (2026-08-08)

R2 RE-DERIVATION VERDICT: KEEP FLAPJACK AS THE ONLY EDITABLE CONSOLE SOURCE HOME.

Current evidence re-measured all four rationale headings: 0 were falsified, 4 remain supported,
and 0 were unmeasured, so the declared threshold of at least two falsifications was not met.
Dependency-direction wording is narrowed to the OSS build/release path: later Flapjack operational
tests may read a fjcloud checkout, but no fjcloud source fetch enters the Flapjack product build or
release. The licensing premise is corrected: commit f8e00a8a5 relicensed the engine and dashboard
to Elastic License 2.0; the rationale that survives is single-repository self-containment and
license ownership, not the obsolete claim that the whole product is MIT.

Consumption mechanism remains owned by fjcloud's console-unification R2. At re-derivation time,
the public pinned fetch was INADMISSIBLE: the prod manifest was 494 commits (148 first-parent
commits) behind live flapjack origin/main, and the prod workflow contract recognized 0/8 required
export slots. R2's selected fallback is an additive, non-editable vendor snapshot at pin-bump time
with a staleness check and no delete semantics. Flapjack remains the only editable source; no fork,
second editable copy, or debbie cross-project export is authorized.
