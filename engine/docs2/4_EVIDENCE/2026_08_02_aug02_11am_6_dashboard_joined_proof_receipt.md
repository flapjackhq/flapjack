# 2026-08-02 Dashboard Joined Proof Receipt

PURPOSE: Replace the stale `JOIN-1` uncertainty with a dated, load-bearing
receipt for lane L8 and later public-ledger correction work. This receipt uses
existing evidence only; it does not claim a second paid full-suite pass.

## Evidence Provenance

Stage 4 contract source:

- `chats/icg/aug02_11am_6_dashboard_joined_proof.md`
- No exact `<!-- matt:pinned-receipt-sha: <40-lowercase-hex-sha> -->`
  marker is present in that contract, so this receipt preserves no pinned
  receipt SHA and synthesizes none.

Existing owners re-read for this receipt:

- Backend capability matrix owner:
  `engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_3_dashboard_join_audit_receipt.md`
- Stage 3 route/remainder owner: `docs/screen_specs/_audit.md`
- Stage 1 capability probe owner:
  `engine/docs2/4_EVIDENCE/2026_08_02_aug02_11am_6_l6_capability_probe_receipt.md`
- Preserved Stage 2 evidence:
  `engine/dashboard/.evidence/l6-full-b9da7b10-s14`
- Preserved Stage 3 evidence:
  `engine/dashboard/.evidence/l6-full-3c2f2343-s26`
- Stage 3 focused proof owners:
  `/Users/stuart/.matt/projects/flapjack_dev-11edc47c/aug02_11am_6_dashboard_joined_proof.md-6db14b47/checklists/stage_03_checklist.md`
  and the Stage 3 handoffs under the same matt project directory.

Manifest verification was run before trusting the preserved evidence:

```text
node engine/loadtest/lib/evidence_manifest.mjs verify --root engine/dashboard/.evidence/l6-full-b9da7b10-s14 --manifest engine/dashboard/.evidence/l6-full-b9da7b10-s14/evidence_manifest.json
{
  "verdict": "PASS",
  "fileCount": 7,
  "totalBytes": 1279288
}

node engine/loadtest/lib/evidence_manifest.mjs verify --root engine/dashboard/.evidence/l6-full-3c2f2343-s26 --manifest engine/dashboard/.evidence/l6-full-3c2f2343-s26/evidence_manifest.json
{
  "verdict": "PASS",
  "fileCount": 11,
  "totalBytes": 1985776
}
```

## Stage 1 Arm And Bias

The Stage 1 probe used the native Playwright/Chromium arm on AL2023. The
container arm was not required.

Native-browser bias to carry forward: Playwright printed its unsupported-OS
fallback warning and downloaded the Ubuntu 24.04 Chromium build. The browser
launched successfully with the AL2023 `dnf` dependency set, but a
Chromium-level crash should be rechecked in the official Playwright container
before being classified as a product defect.

Probe locality:

| Field | Value |
| --- | --- |
| Region | `us-east-1` |
| Build-bearing instance type | `i4i.2xlarge` |
| Storage backing | EBS root plus EC2 NVMe instance store relocated to `/mnt/flapjack-build` |
| Browser arm | native Chromium on AL2023 |
| Container arm | not required |
| Probe spend | `0.0152` USD total across the Stage 1 probe pair |

The Stage 1/2 authored snippets named `c7i.2xlarge`, but the measured
build-bearing runs used `i4i.2xlarge` because the AL2023 root volume had only
about 6 GiB free and the `i4i.2xlarge` instance supplied the required local NVMe
instance store. Numbers measured on this lane are therefore `i4i.2xlarge`
numbers, not `c7i.2xlarge` reference-locality numbers.

## Remote Run Receipts

| Run | Git SHA | Instance type | Region | Launched | Evidence captured | Wall clock | Setup | Measure | Remote | Load / storage |
| --- | --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | --- |
| Stage 2 baseline `l6-full-b9da7b10-s14` | `b9da7b10d743d9df00dc0d4228ec36bdbf7fb3e9` | `i4i.2xlarge` | `us-east-1` | `2026-08-03T01:03:07Z` | `2026-08-03T01:14:06Z` | 659 s | 0 | 1 | 1 | load `4.08, 5.78, 3.33`; EBS root plus `/mnt/flapjack-build` NVMe |
| Stage 3 latest proof `l6-full-3c2f2343-s26` | `3c2f2343d5bf768ef64e64bf0184c1b399f8e300` | `i4i.2xlarge` | `us-east-1` | `2026-08-03T03:44:26Z` | `2026-08-03T04:00:56Z` | 990 s | 0 | 1 | 1 | load `4.24, 5.32, 2.88`; EBS root plus `/mnt/flapjack-build` NVMe |

Cost estimate uses the script-owned `i4i.2xlarge` rate of `0.6860` USD/hour:

| Run | Formula | Estimated cost |
| --- | --- | ---: |
| Stage 2 baseline | `0.6860 * 659 / 3600` | `0.1256` USD |
| Stage 3 latest proof | `0.6860 * 990 / 3600` | `0.1887` USD |

## Before And After Suite Table

| Suite | Stage 2 baseline `l6-full-b9da7b10-s14` | Stage 3 latest remote proof `l6-full-3c2f2343-s26` | Current Stage 4 disposition |
| --- | --- | --- | --- |
| Dashboard unit | `697 passed` | `705 passed` | improved and parseable |
| Playwright smoke | `17 passed` | `17 passed` | unchanged green smoke |
| Full UI | `UNPARSEABLE` | `347 passed / 1 failed / 20 skipped` | parseable red; the single README readiness failure was fixed after the run and proved with focused `5 passed`, but no second remote full-suite pass is claimed |
| E2E API | `52 passed / 3 failed / 7 did not run` | `62 passed` | green in latest remote proof |
| Compound remote status | `L6_SUITE_STATUS dashboard_full=1 e2e_api=1` | `L6_SUITE_STATUS dashboard_full=1 e2e_api=0` | dashboard full still nonzero because full UI was red; API green |

## Per-Spec And Focused Proof Table

| Evidence | Scope | Result | Disposition |
| --- | --- | --- | --- |
| Stage 2 full UI | Dashboard full Playwright phase | `UNPARSEABLE` | Cannot promote joined proof rows. |
| Stage 2 e2e-api | API Playwright specs | `52 passed / 3 failed / 7 did not run` | API red at baseline. |
| Stage 3 full UI | Full browser suite | `347 passed / 1 failed / 20 skipped` | Parseable red. The failed spec was `readme-screenshots.spec.ts`. |
| Stage 3 e2e-api | API Playwright specs | `62 passed` | Latest remote API proof is green. |
| Stage 3 vector focused browser proof | `chat.spec.ts`, `hybrid-search.spec.ts`, `navigation.spec.ts`, `vector-settings.spec.ts` | `28 passed / 15 skipped` | Vector capability gating fixed on the default vector-disabled build without claiming a fresh remote full-suite pass. |
| Stage 3 README focused browser proof | `readme-screenshots.spec.ts` | `5 passed` | Search readiness/order sensitivity fixed after the remote s26 run. |

## Computed JOIN-1 Backend Denominator

The July 30 matrix remains the canonical 90-row backend capability owner. Stage
4 applies only current passing proof keys to those rows and does not create a
parallel capability matrix.

The current joined proof count is still:

```text
joined proof yes                                      0
joined proof partial                                  0
dashboard route without current joined proof         63
API only                                             19
CLI only                                              1
config only                                           7
residual                                              0
                                                      --
backend denominator                                  90
0 + 0 + 63 + 19 + 1 + 7 + 0 = 90
```

The reason the numerator remains `0 / 90` is narrow and mechanical: the
dashboard route and candidate proof-key mapping exists, and the Stage 3 suite
became parseable, but this lane did not rerun every candidate proof key at a
single clean audited SHA after the later focused fixes. Focused proofs close
their named defects; they do not convert unexecuted matrix rows into current
joined proof.

## 390px Denominator

The current route/remainder owner is `docs/screen_specs/_audit.md`.

| Measurement | Value |
| --- | ---: |
| Pages tested at 390px | 22 |
| Pages usable at 390px | 0 |

The preserved remainder is `AUD-SHARED-001`: 22/22 audited routes had
horizontal overflow at a 390px viewport. The route not in that denominator is
`/experiments/:experimentId`, tracked by `AUD-EXP-001` until a deterministic
experiment-detail fixture is promoted.

## Defects Fixed

Fixed defects are owned by `docs/screen_specs/_audit.md` and the Stage 3 focused
proof notes:

| ID | Owner files | Proof |
| --- | --- | --- |
| `FIX-API-001` | `engine/src/analytics/query/mod.rs`; `engine/flapjack-http/src/handlers/analytics/read_endpoints.rs`; `engine/flapjack-http/src/handlers/analytics/mod.rs` | `/2/overview?index=<name>` now aggregates only the requested index; focused Rust proof passed. |
| `FIX-FIXTURE-001` | `engine/dashboard/tests/fixtures/analytics-seed.ts`; `engine/dashboard/tests/fixtures/analytics-seed.test.ts` | `seedAnalytics` clears and verifies the stage-owned index; focused Vitest proof passed. |
| `FIX-SPEC-001` | `engine/dashboard/tests/e2e-api/analytics-data-api.spec.ts` | Analytics API verification runs serially against fixed names; patched-backend proof passed `10 passed`. |
| `FIX-SPEC-002` | `engine/dashboard/tests/fixtures/api-helpers.ts`; vector browser specs | Vector-enabled browser cases skip only when the health capability is not explicitly enabled; focused browser proof passed `28 passed / 15 skipped`. |
| `FIX-SPEC-003` | `engine/dashboard/tests/e2e-ui/full/readme-screenshots.spec.ts` | Search readiness no longer depends on one seed product being first; focused browser proof passed `5 passed`. |

## Defects Filed

Remaining defects are cited from `docs/screen_specs/_audit.md` instead of
duplicated into a second tracker:

| ID | Classification | Owner files | Falsifiable exit | Proposed row text |
| --- | --- | --- | --- | --- |
| `AUD-SHARED-001` | Too-large product defect | `engine/dashboard/src/components/layout/Layout.tsx`; `engine/dashboard/src/components/layout/Sidebar.tsx`; `engine/dashboard/src/components/layout/IndexLayout.tsx`; route-level pages under `engine/dashboard/src/` as needed | Rerun `STAGE3_ROUTE_AUDIT_OUTPUT=/tmp/stage3_route_audit_390.json npm run test:e2e-ui -- tests/e2e-ui/stage3_route_audit_390.spec.ts`; evidence records pages tested 22 and pages usable 22 with no horizontal overflow. | `PR-13 dashboard mobile route usability: remove shared 390px horizontal overflow across authenticated dashboard routes; exit is 22/22 audited routes usable at 390px under the route audit owner.` |
| `AUD-EXP-001` | Fixture remainder | `engine/dashboard/tests/e2e-ui/route_audit_manifest.ts`; `engine/dashboard/tests/e2e-ui/full/experiments.spec.ts` | Promote a deterministic experiment fixture, include `/experiments/:experimentId` in `buildDashboardRouteAudit`, and rerun the 390px route audit with the route counted. | `Dashboard experiment detail route audit: add deterministic experiment fixture so /experiments/:experimentId is included in joined route proof.` |

## ROADMAP CORRECTION REQUIRED

Replace `JOIN-1` with:

```markdown
JOIN-1 dashboard/backend joined proof: current dated receipt measures 0 / 90 backend capability rows with current passing joined proof, 0 partial, 63 dashboard-route rows without current joined proof, 19 API-only, one CLI-only, seven config-only, and zero residual; the latest remote dashboard composition on `i4i.2xlarge` in `us-east-1` is parseable but not green (`347 passed / 1 failed / 20 skipped` full UI, `62 passed` e2e-api), with later focused fixes for vector capability gating and README search readiness. Narrow remaining work to rerunning the joined proof keys at one clean SHA and to the named non-console/API/CLI/config rows instead of claiming route existence as proof.
```

Replace `PR-13` with:

```markdown
PR-13 dashboard mobile route usability: measured 22 tested / 0 usable authenticated dashboard routes at 390px; the current shared layout has horizontal overflow across the audited route set. Owner row `AUD-SHARED-001` exits only when the route audit records 22 tested / 22 usable with no horizontal overflow; `AUD-EXP-001` separately owns promoting `/experiments/:experimentId` into the audit denominator.
```

## Explicit Non-Goals

- No product code changed for this Stage 4 receipt.
- No new paid remote full-suite run was performed.
- No public ledger file was edited: `ROADMAP.md`, `PROJECT_OVERVIEW.md`,
  `engine/docs2/FEATURES.md`, and `CHANGELOG.md` remain untouched by this
  receipt stage.
- No new capability matrix was created; the July 30 receipt remains the
  canonical backend-row owner.
- No Stage 3 focused proof was described as a second remote full-suite pass.
