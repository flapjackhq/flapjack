# Dashboard Joined-Proof Audit

PURPOSE: Track Stage 3 joined-proof state for dashboard user-facing routes in one place.

Source route owner: `engine/dashboard/tests/e2e-ui/route_audit_manifest.ts::APP_USER_FACING_ROUTE_PATTERNS`.

Measurement locality:
- Stage 2 remote baseline: `i4i.2xlarge`, EBS root plus NVMe instance-store build mount, load average `4.08, 5.78, 3.33`.
- Stage 3 local API/browser probes: local macOS workspace with patched `flapjack` debug server on isolated ports.
- Stage 3 remote proof: `i4i.2xlarge`, us-east-1, EBS root plus NVMe instance-store build mount.

Stage 2 baseline counts:
- `unit=697 passed`
- `smoke=17 passed`
- `full-ui=unparseable`
- `e2e-api=52 passed / 3 failed / 7 did not run`

Stage 3 local deltas:
- Analytics API focused proof passed against patched backend: `10 passed`.
- Fixture proof passed: `analytics-seed.test.ts` and `algolia.fixture.test.ts` -> `13 passed`.
- 390px route audit: pages tested: 23
- 390px route audit: pages usable: 23

Stage 3 remote result:
- Full UI was parseable with authorized Algolia credentials: `347 passed / 1 failed / 20 skipped`; the single order-sensitive README readiness failure was subsequently fixed and its focused browser spec passed `5 passed` at HEAD.
- E2E API passed: `62 passed`.

## Route Status

| ID | Route pattern | Stage 3 joined-proof status | Owner spec/test | Shared defect |
| --- | --- | --- | --- | --- |
| AUD-001 | `/` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/overview.spec.ts` | None |
| AUD-002 | `/overview` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/overview.spec.ts` | None |
| AUD-003 | `/index/:indexName` | Ready-state parseable with seeded `movies` index; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/search.spec.ts` | None |
| AUD-004 | `/index/:indexName/settings` | Ready-state parseable with seeded `movies` index; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/settings.spec.ts` | None |
| AUD-005 | `/index/:indexName/analytics` | Ready-state parseable with seeded `movies` index; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/analytics.spec.ts` | None |
| AUD-006 | `/index/:indexName/synonyms` | Ready-state parseable with seeded `movies` index; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/synonyms.spec.ts` | None |
| AUD-007 | `/index/:indexName/rules` | Ready-state parseable with seeded `movies` index; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/rules.spec.ts` | None |
| AUD-008 | `/index/:indexName/merchandising` | Ready-state parseable with seeded `movies` index; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/merchandising.spec.ts` | None |
| AUD-009 | `/index/:indexName/recommendations` | Ready-state parseable with seeded `movies` index; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/recommendations.spec.ts` | None |
| AUD-010 | `/index/:indexName/chat` | Ready-state parseable with seeded `movies` index; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/chat.spec.ts` | None |
| AUD-011 | `/keys` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/api-keys.spec.ts` | None |
| AUD-012 | `/logs` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/search-logs.spec.ts` | None |
| AUD-013 | `/migrate` | Ready-state parseable; Algolia migration collected with authorized credentials; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/migrate.spec.ts`; `engine/dashboard/tests/e2e-ui/full/migrate-algolia.spec.ts` | None |
| AUD-014 | `/metrics` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/metrics.spec.ts` | None |
| AUD-015 | `/cluster` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/src/pages/Cluster.test.tsx` | None |
| AUD-016 | `/system` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/system.spec.ts` | None |
| AUD-017 | `/query-suggestions` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/query-suggestions.spec.ts` | None |
| AUD-018 | `/experiments` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/experiments.spec.ts` | None |
| AUD-019 | `/experiments/:experimentId` | Ready-state parseable with seeded experiment; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/experiments.spec.ts` | None |
| AUD-020 | `/events` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/event-debugger.spec.ts` | None |
| AUD-021 | `/personalization` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/personalization.spec.ts` | None |
| AUD-022 | `/dictionaries` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/dictionaries.spec.ts` | None |
| AUD-023 | `/security-sources` | Ready-state parseable; 390px usable (verified 23/23 at HEAD a37c65e86). | `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts`; `engine/dashboard/tests/e2e-ui/full/security-sources.spec.ts` | None |
| AUD-024 | `*` | Not audited in Stage 3 because this is the fallback-only not-found shell, not an authenticated dashboard surface. | `engine/dashboard/tests/e2e-ui/route_audit_manifest.ts` | None |

## Shared Defects And Remainders

| ID | Classification | Owner files | Evidence | Falsifiable exit | Proposed ROADMAP row |
| --- | --- | --- | --- | --- | --- |
| AUD-SHARED-001 | Closed shared defect | `engine/dashboard/src/components/layout/Layout.tsx`; `engine/dashboard/src/components/layout/Header.tsx`; `engine/dashboard/src/components/layout/ApiLogger.tsx` | Stage 2 landed the shared-shell containment fix (`2055c9783 Fix dashboard shell containment at 390px`). After-audit at HEAD `a37c65e86` records pages tested 23, pages usable 23, with document-overflow rows 0 and positive-culprit rows 0 (`AUDIT_AFTER_EXIT=0`, `ALL_USABLE_EXIT=0`). Negative control confirms the proof is falsifiable: reverting `Header.tsx` to its pre-Stage-2 form reintroduced `div.flex.items-center.gap-2` overflow (document 565px > viewport 390px) and turned the audit red at the all-usable assertion (`AUDIT_NEGATIVE_EXIT=1`, 23/23 unusable), then a clean restore returned the audit to 23/23 usable. | Rerun `STAGE3_ROUTE_AUDIT_OUTPUT=/tmp/l9_after.json npm run test:e2e-ui -- tests/e2e-ui/stage3_route_audit_390.spec.ts`; evidence records pages tested 23 and pages usable 23 with no horizontal overflow. | `PR-13 dashboard mobile route usability: remove shared 390px horizontal overflow across authenticated dashboard routes; exit is 23/23 audited routes usable at 390px under the route audit owner.` |
| AUD-EXP-001 | Closed fixture remainder | `engine/dashboard/tests/fixtures/experiment-seed.ts`; `engine/dashboard/tests/e2e-ui/route_audit_manifest.ts`; `engine/dashboard/tests/e2e-ui/full/experiments.spec.ts` | `seedRouteAuditExperiment` verifies the runtime record by ID; fresh route-audit JSON measured 23 routes including `/experiments/:experimentId`. | Closed: the deterministic fixture, detail readiness assertion, and 23-route browser proof are present together. | None — closed by the denominator-completion lane. |

## Stage 3 Fixed Defects

| ID | Owner files | Proof |
| --- | --- | --- |
| FIX-API-001 | `engine/src/analytics/query/mod.rs`; `engine/flapjack-http/src/handlers/analytics/read_endpoints.rs`; `engine/flapjack-http/src/handlers/analytics/mod.rs` | `/2/overview?index=<name>` now aggregates only the requested index; focused Rust proof `cargo test -p flapjack-http -- handlers::analytics::tests::overview_endpoint_filters_to_requested_index` passed. |
| FIX-FIXTURE-001 | `engine/dashboard/tests/fixtures/analytics-seed.ts`; `engine/dashboard/tests/fixtures/analytics-seed.test.ts` | `seedAnalytics` clears the stage-owned index before seeding and mismatch errors name the index; focused Vitest proof passed. |
| FIX-SPEC-001 | `engine/dashboard/tests/e2e-api/analytics-data-api.spec.ts` | Analytics API verification runs serially against fixed index names; patched-backend proof passed `10 passed`. |
| FIX-SPEC-002 | `engine/dashboard/tests/fixtures/api-helpers.ts`; vector browser specs | Vector-enabled browser cases skip only when the health capability is not explicitly enabled; focused fixture and browser proofs pass. |
| FIX-SPEC-003 | `engine/dashboard/tests/e2e-ui/full/readme-screenshots.spec.ts` | Search readiness no longer depends on one seed product being first; focused browser proof passed `5 passed`. |
