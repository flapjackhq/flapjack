# Route-Audit Denominator Completion Receipt

Date: 2026-08-03
Lane: `aug03_11am_4_route_audit_denominator_completion`
Product-validation base HEAD: `cb5a191098d0fc94128aef743eb3a656b4022e3e`
Scope: evidence receipt only. This stage changes no product code, dependency files,
or ledger files.

## Contract Provenance

The Stage 3 contract was re-read at
`chats/icg/aug03_11am_4_route_audit_denominator_completion.md`. It contains no
pinned-receipt declaration matching the contract's 40-character SHA form, so this
receipt preserves no pinned declaration and synthesizes none.

The denominator facts below come from the current owners, not from an older
receipt:

- `engine/dashboard/tests/e2e-ui/route_audit_manifest.ts:20` owns
  `APP_USER_FACING_ROUTE_PATTERNS` with 24 route patterns.
- `engine/dashboard/tests/e2e-ui/route_audit_manifest.ts:47` owns the one
  remaining exclusion: `*`, reason `fallback_shell`.
- Therefore the audited denominator moved from `22 -> 23` routes when
  `/experiments/:experimentId` joined the audited set.
- `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts:79` asserts the
  resulting evidence has exactly 23 rows and line 80 asserts that the experiment
  detail route is present.

## Deterministic Experiment Readiness

`seedRouteAuditExperiment` and `assertExperimentReady` in
`engine/dashboard/tests/fixtures/experiment-seed.ts:85` provide one readiness
contract:

1. stale lane-owned experiments are cleaned up;
2. the fixture creates the experiment through the product API;
3. the create response supplies the runtime experiment ID;
4. readiness fetches that runtime ID through the product API's by-ID endpoint;
5. readiness asserts the fetched name equals the expected generated name; and
6. a readiness failure deletes the just-created experiment before rethrowing.

Callers receive the runtime ID only after the by-ID read and expected-name
assertion have passed.

## Stage 1 And Stage 2 Evidence Provenance

The Stage 1 red-to-green source is the run handoff
`session_handoffs/stage_01/s04_build_deterministic-experiment-fixture.md` from
this lane's handoff root. It records the valid red boundary as
`FIXTURE_UNIT_EXIT=1` because `./experiment-seed` did not yet exist, followed at
clean committed HEAD `fb6b10610` by `FIXTURE_UNIT_EXIT=0` with 2 tests passing.
No standalone Stage 1 transcript file is available in this worktree, so no
transcript output is reconstructed here. The Stage 1 clean-review handoff
`stage_01/s08_clean_review_all-tests-passing-stage-done.md` independently
accepted that recorded red boundary and the green fixture contract.

The Stage 2 reviewed green source is
`stage_02/s23_clean_review_pagination-fix-passes.md` in the same handoff root. It
records the focused fixture Vitest as 1 file and 6 tests passing. The subsequent
Stage 2 stage review at `stage_02/s24_stage_review_readiness-gate-and-dead-export.md`
records the fixture in a combined focused Vitest run with 11 tests passing and a
23-route browser audit with the same summary shape as below. Those reviewed
handoffs are historical comparison points; the next section is the fresh proof
at this receipt's HEAD.

## Fresh HEAD Validation

All commands below ran on 2026-08-03 from the product-validation base HEAD and
were recorded through the required validation cache. The only subsequent repo
delta is this receipt; the same commands are rerun after its commit as the final
acceptance gate.

From `engine/dashboard`:

```text
npm run lint:e2e
LINT_E2E_EXIT=0

npx vitest run tests/fixtures/experiment-seed.test.ts
FIXTURE_UNIT_EXIT=0
Test Files  1 passed (1)
Tests       6 passed (6)

STAGE3_ROUTE_AUDIT_OUTPUT=/tmp/l4_stage3_route_audit_390.json npm run test:e2e-ui -- tests/e2e-ui/stage3_route_audit_390.spec.ts > /tmp/l4_stage3_route_audit_390.txt 2>&1
AUDIT_EXIT=0
3 passed (12.6s)
```

The deterministic JSON parser asserted the complete expected tuple and printed:

```text
tested 23
usable 0
has_experiment_detail True
unexpected_issues []
```

This fresh audit agrees with Stage 2's reviewed 23-route evidence while replacing
that handoff as the current proof. All 23 audited routes still exhibit the known
390px horizontal-overflow issue; remediation remains owned downstream and is not
part of this receipt stage.

## Ledger And Coordination Hand-Off

`docs/screen_specs/_audit.md:62` already closes `AUD-EXP-001`: the deterministic
fixture, detail readiness assertion, and 23-route browser proof are present
together. This stage does not edit that file.

When Wave 3 opens, `_audit.md` single-writer ownership switches from this lane to
`L9`. `AUD-SHARED-001` at `docs/screen_specs/_audit.md:61` is untouched here even
though its `22/22` wording is now `L9`'s reconciliation work.

The merge target is `main`. Coordination requires an ordinary clean-review merge
commit via `batman land`; `L8` and `L9` dispatch only after this lane merges. No
other lane writes `docs/screen_specs/_audit.md` in Wave 1.

The origin probe printed:

```text
LEDGER_GATE_ON_ORIGIN_MAIN=1
```

### Proposed ledger text for L11

Record that the authenticated dashboard route-audit denominator is 23 routes:
24 user-facing patterns minus the single `*` fallback-shell exclusion. The
experiment detail route is now included through its deterministic runtime
fixture, while the measured 390px result remains 23 tested and 0 usable pending
`L9`'s shared-layout work.

This proposal is deliberately routed through `L11`, this batch's single ledger
writer. `L11`'s source enumerates the exact receipt filenames it must apply.
Using the gate's prohibited correction-marker text before `L11` applies the edit
would hold workspace gates red across the batch, so this receipt uses the heading
above and leaves ledger files untouched.

## Scope Boundaries

This stage does not modify `ROADMAP.md`, `PROJECT_OVERVIEW.md`,
`engine/docs2/FEATURES.md`, `CHANGELOG.md`, `engine/dashboard/src/**`,
`engine/dashboard/package.json`, `engine/package.json`, `engine/Cargo.toml`, or
`engine/Cargo.lock`. The file/scope and marker guards below are the closing proof.

```text
RECEIPT_EXIT=0
LEDGER_UNTOUCHED_EXIT=0
NO_SRC_CHANGE_EXIT=0
NO_DEP_CHANGE_EXIT=0
LEDGER_GATE_ON_ORIGIN_MAIN=1
NO_LEDGER_MARKER_EXIT=0
```
