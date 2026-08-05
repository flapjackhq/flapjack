# 2026-07-30 Dashboard Join Audit Receipt

## Stage 1 Baseline

Stage 1 is measurement-only. Product code, tests, specs, runner scripts, Playwright config, and public ledger docs are out of scope for this stage. The dashboard is frozen; only bug fixes to keep it working are allowed in later stages, and new screens, routes, features, or visual redesigns are forbidden.

Owned evidence path for this stage:

- `engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_3_dashboard_join_audit_receipt.md`

Observed runner owners:

- `engine/s/test` forwards to `engine/_dev/s/test`.
- `engine/_dev/s/test --dashboard-full` runs dashboard unit tests, starts/ensures the server, runs Playwright smoke tests, waits for the Playwright webserver port to be free, then runs full Playwright e2e with `PLAYWRIGHT_E2E_WORKERS="${PLAYWRIGHT_E2E_WORKERS:-1}"`.

Observed dashboard command owners in `engine/dashboard/package.json`:

- `test:unit:run`: `vitest run`
- `test:e2e-ui:smoke`: `playwright test --project=e2e-ui tests/e2e-ui/smoke/`
- `test:e2e-ui:full`: `playwright test --project=e2e-ui tests/e2e-ui/full/`
- `test:e2e-api`: `playwright test --project=e2e-api`
- `lint:e2e`: `eslint --config tests/e2e-ui/eslint.config.mjs 'tests/e2e-ui/**/*.spec.ts' 'tests/e2e-ui/**/*.test.ts' 'tests/e2e-ui/helpers.ts' 'tests/e2e-ui/**/*_helpers.ts'`

Baseline metadata before suite execution:

```text
git rev-parse HEAD
9850d9dffae64dddd5e7d666eb8118a39cd144fd

uptime
 1:55  up 1 day,  5:04, 7 users, load averages: 46.36 21.87 19.08

date
Thu Jul 30 01:55:52 EDT 2026

git status --porcelain --untracked-files=all
<no output>
```

Observed spec inventory before suite execution:

```text
engine/dashboard/tests/e2e-ui/full/accessibility.spec.ts
engine/dashboard/tests/e2e-ui/full/analytics-conversions-mocked.spec.ts
engine/dashboard/tests/e2e-ui/full/analytics-deep.spec.ts
engine/dashboard/tests/e2e-ui/full/analytics.spec.ts
engine/dashboard/tests/e2e-ui/full/api-keys.spec.ts
engine/dashboard/tests/e2e-ui/full/auth-flow.spec.ts
engine/dashboard/tests/e2e-ui/full/chat.spec.ts
engine/dashboard/tests/e2e-ui/full/cluster_standalone_copy.spec.ts
engine/dashboard/tests/e2e-ui/full/connection-health.spec.ts
engine/dashboard/tests/e2e-ui/full/cross-page-flows.spec.ts
engine/dashboard/tests/e2e-ui/full/dictionaries.spec.ts
engine/dashboard/tests/e2e-ui/full/display-preferences.spec.ts
engine/dashboard/tests/e2e-ui/full/edge-cases.spec.ts
engine/dashboard/tests/e2e-ui/full/event-debugger-mocked.spec.ts
engine/dashboard/tests/e2e-ui/full/event-debugger.spec.ts
engine/dashboard/tests/e2e-ui/full/experiments.spec.ts
engine/dashboard/tests/e2e-ui/full/hybrid-search.spec.ts
engine/dashboard/tests/e2e-ui/full/large-index-perf.spec.ts
engine/dashboard/tests/e2e-ui/full/merchandising.spec.ts
engine/dashboard/tests/e2e-ui/full/metrics.spec.ts
engine/dashboard/tests/e2e-ui/full/migrate-algolia.spec.ts
engine/dashboard/tests/e2e-ui/full/migrate.spec.ts
engine/dashboard/tests/e2e-ui/full/navigation.spec.ts
engine/dashboard/tests/e2e-ui/full/overview.spec.ts
engine/dashboard/tests/e2e-ui/full/personalization.spec.ts
engine/dashboard/tests/e2e-ui/full/query-suggestions.spec.ts
engine/dashboard/tests/e2e-ui/full/readme-screenshots.spec.ts
engine/dashboard/tests/e2e-ui/full/recommendations.spec.ts
engine/dashboard/tests/e2e-ui/full/rules-form-mocked.spec.ts
engine/dashboard/tests/e2e-ui/full/rules.spec.ts
engine/dashboard/tests/e2e-ui/full/search-logs.spec.ts
engine/dashboard/tests/e2e-ui/full/search.spec.ts
engine/dashboard/tests/e2e-ui/full/security-sources.spec.ts
engine/dashboard/tests/e2e-ui/full/settings.spec.ts
engine/dashboard/tests/e2e-ui/full/synonyms.spec.ts
engine/dashboard/tests/e2e-ui/full/system.spec.ts
engine/dashboard/tests/e2e-ui/full/ux-regression.spec.ts
engine/dashboard/tests/e2e-ui/full/vector-settings.spec.ts
engine/dashboard/tests/e2e-ui/smoke/critical-paths.spec.ts
engine/dashboard/tests/e2e-ui/smoke/index-tab-bar.spec.ts
engine/dashboard/tests/e2e-ui/smoke/settings-tabs.spec.ts
engine/dashboard/tests/e2e-ui/smoke/sidebar-sections.spec.ts
engine/dashboard/tests/e2e-api/analytics-api-shapes.spec.ts
engine/dashboard/tests/e2e-api/analytics-data-api.spec.ts
engine/dashboard/tests/e2e-api/api-helpers.spec.ts
engine/dashboard/tests/e2e-api/demo-analytics-api.spec.ts
```

## Stage 1 Dashboard Full Run

Validation-cache check before command:

```text
HEAD=9850d9dffae64dddd5e7d666eb8118a39cd144fd
CLEAN_TREE=False
CACHE_HIT=False
```

Command:

```bash
cd engine && timeout 3600 ./s/test --dashboard-full > /tmp/jul30_12am_3_dashboard_full_run1.txt 2>&1; echo "DASHBOARD_FULL_EXIT=$?"; tail -60 /tmp/jul30_12am_3_dashboard_full_run1.txt
```

Exit:

```text
DASHBOARD_FULL_EXIT=1
```

Phase evidence:

```text
[1] Dashboard: Vitest unit tests
Test Files  78 passed (78)
     Tests  663 passed (663)
  ✓ Dashboard unit tests passed

[3] Dashboard: Playwright smoke tests
> flapjack-dashboard@1.0.10 test:e2e-ui:smoke
> playwright test --project=e2e-ui tests/e2e-ui/smoke/
Running 17 tests using 3 workers
  17 passed (9.5s)
  ✓ Dashboard smoke tests passed

[4] Dashboard: wait for Playwright webserver port release
  ✓ Dashboard Playwright webserver port is free

[5] Dashboard: Playwright full e2e
> flapjack-dashboard@1.0.10 test:e2e-ui:full
> playwright test --project=e2e-ui tests/e2e-ui/full/
Running 367 tests using 1 worker
  1 failed
    [e2e-ui] › tests/e2e-ui/full/migrate-algolia.spec.ts:58:3 › Algolia Migration (real browser) › migrate Algolia index via UI: fill form → migrate → verify success → browse
  8 skipped
  1 did not run
  357 passed (4.8m)
```

Full Playwright failure excerpt:

```text
1) [e2e-ui] › tests/e2e-ui/full/migrate-algolia.spec.ts:58:3 › Algolia Migration (real browser) › migrate Algolia index via UI: fill form → migrate → verify success → browse

    ApiError: Invalid Application-ID or API key

       at fixtures/algolia.fixture.ts:89

      87 |
      88 |   // Apply settings
    > 89 |   await client.setSettings({ indexName, indexSettings: SETTINGS });
         |   ^
```

Dashboard denominators:

| Sub-suite | Passed | Failed | Flaky | Skipped | Did not run | Total | Status |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Vitest unit | 663 | 0 | n/a | n/a | n/a | 663 | parseable |
| Playwright smoke | 17 | 0 | 0 | 0 | 0 | 17 | parseable |
| Playwright full | 357 | 1 | 0 | 8 | 1 | 367 | parseable failure |

Arithmetic:

- Vitest unit: 663 passed + 0 failed = 663 total; Vitest summary does not report flaky/skipped categories in this run.
- Playwright smoke: 17 passed + 0 failed + 0 flaky + 0 skipped + 0 did not run = 17 total.
- Playwright full: 357 passed + 1 failed + 0 flaky + 8 skipped + 1 did not run = 367 total.

Classification: Stage 1 measurement succeeded in producing a denominator. The suite is not green. The observed failure is in the full Playwright phase and occurs while seeding an external Algolia index with an invalid Application-ID or API key. No runner setup failure occurred before tests executed, so no runner gap spec is routed from Stage 1.

Source-level diagnosis:

```text
engine/dashboard/tests/e2e-ui/full/migrate-algolia.spec.ts:43-45
test.beforeAll(async () => {
  ctx = await seedAlgoliaIndex();
});

engine/dashboard/tests/fixtures/algolia.fixture.ts:71-72
return !!(process.env.ALGOLIA_APP_ID && process.env.ALGOLIA_ADMIN_KEY);

engine/dashboard/tests/fixtures/algolia.fixture.ts:79-89
const appId = process.env.ALGOLIA_APP_ID!;
const adminKey = process.env.ALGOLIA_ADMIN_KEY!;
const client = algoliasearch(appId, adminKey);
await client.setSettings({ indexName, indexSettings: SETTINGS });
```

The fixture fails closed only when the two environment variables are absent. In this run they were present enough for the migration spec to execute, but Algolia rejected them at `setSettings` with `Invalid Application-ID or API key`.

## Stage 1 API Contract Run

Validation-cache check before command:

```text
HEAD=9850d9dffae64dddd5e7d666eb8118a39cd144fd
CLEAN_TREE=False
CACHE_HIT=False
```

Command:

```bash
cd engine/dashboard && npm run test:e2e-api > /tmp/jul30_12am_3_e2e_api_exit.txt 2>&1; echo "E2E_API_EXIT=$?"; tail -60 /tmp/jul30_12am_3_e2e_api_exit.txt
```

Exit and summary:

```text
E2E_API_EXIT=0
Running 62 tests using 3 workers
  24 skipped
  38 passed (9.2s)
```

API denominator:

| Sub-suite | Passed | Failed | Flaky | Skipped | Total | Status |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Playwright e2e-api | 38 | 0 | 0 | 24 | 62 | parseable |

Arithmetic:

- Playwright e2e-api: 38 passed + 0 failed + 0 flaky + 24 skipped = 62 total.

## Stage 1 Validation Gates

Validation-cache checks before commands:

```text
COMMAND=grep -E '^(Running|[0-9]+ (passed|failed|flaky|skipped))' /tmp/jul30_12am_3_dashboard_full_run1.txt | tail -20
CLEAN_TREE=False
CACHE_HIT=False

COMMAND=cd engine/dashboard && npm run lint:e2e > /tmp/jul30_12am_3_lint_e2e_exit.txt 2>&1; echo "LINT_E2E_EXIT=$?"; tail -10 /tmp/jul30_12am_3_lint_e2e_exit.txt
CLEAN_TREE=False
CACHE_HIT=False

COMMAND=cd engine/dashboard && npx tsc --noEmit > /tmp/jul30_12am_3_tsc_exit.txt 2>&1; echo "TSC_EXIT=$?"; tail -10 /tmp/jul30_12am_3_tsc_exit.txt
CLEAN_TREE=False
CACHE_HIT=False
```

Transcript-denominator probe:

```bash
grep -E '^(Running|[0-9]+ (passed|failed|flaky|skipped))' /tmp/jul30_12am_3_dashboard_full_run1.txt | tail -20
```

Output:

```text
Running 17 tests using 3 workers
Running 367 tests using 1 worker
```

Note: the exact grep probe did not print the Playwright summary lines because the captured transcript prefixes those lines with terminal control bytes, so they do not start with `[0-9]+`. The surrounding transcript markers above still make the smoke and full denominators parseable.

E2E lint gate:

```bash
cd engine/dashboard && npm run lint:e2e > /tmp/jul30_12am_3_lint_e2e_exit.txt 2>&1; echo "LINT_E2E_EXIT=$?"; tail -10 /tmp/jul30_12am_3_lint_e2e_exit.txt
```

Output:

```text
LINT_E2E_EXIT=0

> flapjack-dashboard@1.0.10 lint:e2e
> eslint --config tests/e2e-ui/eslint.config.mjs 'tests/e2e-ui/**/*.spec.ts' 'tests/e2e-ui/**/*.test.ts' 'tests/e2e-ui/helpers.ts' 'tests/e2e-ui/**/*_helpers.ts'
```

TypeScript gate:

```bash
cd engine/dashboard && npx tsc --noEmit > /tmp/jul30_12am_3_tsc_exit.txt 2>&1; echo "TSC_EXIT=$?"; tail -10 /tmp/jul30_12am_3_tsc_exit.txt
```

Output:

```text
TSC_EXIT=0
```

## Stage 1 Status

At `9850d9dffae64dddd5e7d666eb8118a39cd144fd`, `./s/test --dashboard-full` returned `1` with vitest `663/663`, smoke `17/17`, full `357 passed / 1 failed / 0 flaky / 8 skipped / 1 did not run / 367 total`, and `npm run test:e2e-api` returned `0` with API `38 passed / 0 failed / 0 flaky / 24 skipped / 62 total`, on host load `46.36 21.87 19.08`.

Final `git status --porcelain --untracked-files=all`:

```text
 M docs/live-state/jun04_pm_lane_c_baseline/20260604T191244Z/movies_seed_verify.json
?? engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_3_dashboard_join_audit_receipt.md
```

The receipt is the only Stage 1 deliverable. Stage 1 also left a tracked live-state delta in `docs/live-state/jun04_pm_lane_c_baseline/20260604T191244Z/movies_seed_verify.json`; that rewrite came from the exercised full-suite harness, not from an intentional receipt edit.

## Stage 1 Verification Rerun - 2026-07-30 02:21 EDT

This section preserves the original Stage 1 baseline above and records a verification rerun at the current `HEAD`.

Baseline metadata before/around rerun:

```text
git rev-parse HEAD
16cb6ecf4c86e6701b2e68ec81a5d3701e204477

uptime
 2:21  up 1 day,  5:30, 7 users, load averages: 13.50 24.75 38.64

date
Thu Jul 30 02:21:25 EDT 2026

git status --porcelain --untracked-files=all
 M docs/live-state/jun04_pm_lane_c_baseline/20260604T191244Z/movies_seed_verify.json
```

Observed spec inventory counts:

```text
ls engine/dashboard/tests/e2e-ui/full/*.spec.ts | wc -l
      38
ls engine/dashboard/tests/e2e-ui/smoke/*.spec.ts | wc -l
       4
ls engine/dashboard/tests/e2e-api/*.spec.ts | wc -l
       4
```

Validation-cache import proof:

```text
/Users/stuart/repos/gridl/mike_dev/matt_root/matt/validation_cache.py
```

### Dashboard Full Rerun

Validation-cache check before command:

```text
HEAD=16cb6ecf4c86e6701b2e68ec81a5d3701e204477
CLEAN_TREE=True
CACHE_HIT=False
```

Command:

```bash
cd engine && timeout 3600 ./s/test --dashboard-full > /tmp/jul30_12am_3_dashboard_full_run1.txt 2>&1; echo "DASHBOARD_FULL_EXIT=$?"; tail -60 /tmp/jul30_12am_3_dashboard_full_run1.txt
```

Exit:

```text
DASHBOARD_FULL_EXIT=1
```

Phase evidence:

```text
[1] Dashboard: Vitest unit tests
Test Files  78 passed (78)
     Tests  663 passed (663)

[3] Dashboard: Playwright smoke tests
> flapjack-dashboard@1.0.10 test:e2e-ui:smoke
> playwright test --project=e2e-ui tests/e2e-ui/smoke/
Running 17 tests using 3 workers
  17 passed (11.2s)
  ✓ Dashboard smoke tests passed

[4] Dashboard: wait for Playwright webserver port release
  ✓ Dashboard Playwright webserver port is free

[5] Dashboard: Playwright full e2e
> flapjack-dashboard@1.0.10 test:e2e-ui:full
> playwright test --project=e2e-ui tests/e2e-ui/full/
Running 367 tests using 1 worker
  1 failed
    [e2e-ui] › tests/e2e-ui/full/migrate-algolia.spec.ts:58:3 › Algolia Migration (real browser) › migrate Algolia index via UI: fill form → migrate → verify success → browse
  8 skipped
  1 did not run
  357 passed (4.1m)
```

Full Playwright failure excerpt:

```text
ApiError: Invalid Application-ID or API key

   at fixtures/algolia.fixture.ts:89

  87 |
  88 |   // Apply settings
> 89 |   await client.setSettings({ indexName, indexSettings: SETTINGS });
     |   ^
```

Source-level diagnosis at current `HEAD`:

```text
engine/dashboard/tests/e2e-ui/full/migrate-algolia.spec.ts:43-44
test.beforeAll(async () => {
  ctx = await seedAlgoliaIndex();
});

engine/dashboard/tests/fixtures/algolia.fixture.ts:71-72
export function hasAlgoliaCredentials(): boolean {
  return !!(process.env.ALGOLIA_APP_ID && process.env.ALGOLIA_ADMIN_KEY);
}

engine/dashboard/tests/fixtures/algolia.fixture.ts:79-89
export async function seedAlgoliaIndex(): Promise<AlgoliaTestContext> {
  const appId = process.env.ALGOLIA_APP_ID!;
  const adminKey = process.env.ALGOLIA_ADMIN_KEY!;
  ...
  await client.setSettings({ indexName, indexSettings: SETTINGS });
}
```

The rerun reproduced the same Stage 1 measurement failure family: the runner reached Vitest, smoke Playwright, and full Playwright; full Playwright failed while seeding the external Algolia index because the supplied Algolia credentials were rejected. No product code, tests, specs, runner scripts, Playwright config, or public ledger docs were edited.

Dashboard rerun denominators:

| Sub-suite | Passed | Failed | Flaky | Skipped | Did not run | Total | Status |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Vitest unit | 663 | 0 | n/a | n/a | n/a | 663 | parseable |
| Playwright smoke | 17 | 0 | 0 | 0 | 0 | 17 | parseable |
| Playwright full | 357 | 1 | 0 | 8 | 1 | 367 | parseable failure |

Arithmetic:

- Vitest unit: 663 passed + 0 failed = 663 total; Vitest summary does not report flaky/skipped categories in this run.
- Playwright smoke: 17 passed + 0 failed + 0 flaky + 0 skipped + 0 did not run = 17 total.
- Playwright full: 357 passed + 1 failed + 0 flaky + 8 skipped + 1 did not run = 367 total.

### API Contract Rerun

Validation-cache check before command:

```text
HEAD=16cb6ecf4c86e6701b2e68ec81a5d3701e204477
CLEAN_TREE=False
CACHE_HIT=False
```

Command:

```bash
cd engine/dashboard && npm run test:e2e-api > /tmp/jul30_12am_3_e2e_api_exit.txt 2>&1; echo "E2E_API_EXIT=$?"; tail -60 /tmp/jul30_12am_3_e2e_api_exit.txt
```

Exit and summary:

```text
E2E_API_EXIT=0
Running 62 tests using 3 workers
  24 skipped
  38 passed (3.2s)
```

API denominator:

| Sub-suite | Passed | Failed | Flaky | Skipped | Total | Status |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Playwright e2e-api | 38 | 0 | 0 | 24 | 62 | parseable |

Arithmetic:

- Playwright e2e-api: 38 passed + 0 failed + 0 flaky + 24 skipped = 62 total.

### Validation Gate Reruns

Validation-cache checks before commands:

```text
COMMAND=grep -E '^(Running|[0-9]+ (passed|failed|flaky|skipped))' /tmp/jul30_12am_3_dashboard_full_run1.txt | tail -20
CLEAN_TREE=False
CACHE_HIT=False

COMMAND=cd engine/dashboard && npm run lint:e2e > /tmp/jul30_12am_3_lint_e2e_exit.txt 2>&1; echo "LINT_E2E_EXIT=$?"; tail -10 /tmp/jul30_12am_3_lint_e2e_exit.txt
CLEAN_TREE=False
CACHE_HIT=False

COMMAND=cd engine/dashboard && npx tsc --noEmit > /tmp/jul30_12am_3_tsc_exit.txt 2>&1; echo "TSC_EXIT=$?"; tail -10 /tmp/jul30_12am_3_tsc_exit.txt
CLEAN_TREE=False
CACHE_HIT=False
```

Transcript-denominator probe:

```bash
grep -E '^(Running|[0-9]+ (passed|failed|flaky|skipped))' /tmp/jul30_12am_3_dashboard_full_run1.txt | tail -20
```

Output:

```text
Running 17 tests using 3 workers
Running 367 tests using 1 worker
```

Note: as in the original Stage 1 run, the exact grep probe did not print Playwright summary lines because those transcript lines are prefixed with terminal control bytes.

E2E lint gate:

```bash
cd engine/dashboard && npm run lint:e2e > /tmp/jul30_12am_3_lint_e2e_exit.txt 2>&1; echo "LINT_E2E_EXIT=$?"; tail -10 /tmp/jul30_12am_3_lint_e2e_exit.txt
```

Output:

```text
LINT_E2E_EXIT=0

> flapjack-dashboard@1.0.10 lint:e2e
> eslint --config tests/e2e-ui/eslint.config.mjs 'tests/e2e-ui/**/*.spec.ts' 'tests/e2e-ui/**/*.test.ts' 'tests/e2e-ui/helpers.ts' 'tests/e2e-ui/**/*_helpers.ts'
```

TypeScript gate:

```bash
cd engine/dashboard && npx tsc --noEmit > /tmp/jul30_12am_3_tsc_exit.txt 2>&1; echo "TSC_EXIT=$?"; tail -10 /tmp/jul30_12am_3_tsc_exit.txt
```

Output:

```text
TSC_EXIT=0
```

An earlier grouped-shell attempt produced `TSC_EXIT=1` with `zsh:cd:44: no such file or directory: engine/dashboard` because the shell was already inside `engine/dashboard` after the lint command. That was a verification-invocation error; the exact TypeScript command rerun from the repo root passed as shown above.

## Stage 1 Rerun Status

At `16cb6ecf4c86e6701b2e68ec81a5d3701e204477`, `./s/test --dashboard-full` returned `1` with vitest `663/663`, smoke `17/17`, full `357 passed / 1 failed / 0 flaky / 8 skipped / 1 did not run / 367 total`, and `npm run test:e2e-api` returned `0` with API `38 passed / 0 failed / 0 flaky / 24 skipped / 62 total`, on host load `13.50 24.75 38.64`.

Final rerun `git status --porcelain --untracked-files=all` before receipt edit:

```text
 M docs/live-state/jun04_pm_lane_c_baseline/20260604T191244Z/movies_seed_verify.json
```

The modified `docs/live-state/jun04_pm_lane_c_baseline/20260604T191244Z/movies_seed_verify.json` file remained the tracked live-state delta created by the original Stage 1 run; this verification rerun did not intentionally edit it.

## Stage 2 Credentialed Migration Triage

Stage 2 owner boundary: `engine/dashboard/tests/global-setup.ts::loadPlaywrightSecretEnv`,
`engine/dashboard/tests/fixtures/algolia.fixture.ts::hasAlgoliaCredentials` /
`seedAlgoliaIndex`, `engine/dashboard/tests/fixtures/algolia.fixture.test.ts`, and
`engine/dashboard/tests/e2e-ui/full/migrate-algolia.spec.ts`.

Baseline metadata before Stage 2 edits:

```text
git rev-parse HEAD
98ce178cdb1e6ceeda1ce80e1a5011558067758e

git status --porcelain --untracked-files=all
<no output>
```

### Stage 1 Specimen Reconfirmation

Stage 1 and the current focused rerun agree on the specimen:

```text
tests/e2e-ui/full/migrate-algolia.spec.ts:58:3
Algolia Migration (real browser) › migrate Algolia index via UI: fill form → migrate → verify success → browse

setup phase
test.beforeAll seeded the external Algolia source index before browser actions.

error
ApiError: Invalid Application-ID or API key
```

Source citations at `HEAD`:

```text
engine/dashboard/tests/global-setup.ts:26-38
loadPlaywrightSecretEnv resolves the configured secret env path and calls dotenv.config(...).

engine/dashboard/tests/fixtures/algolia.fixture.ts:71-72
hasAlgoliaCredentials returns true only when ALGOLIA_APP_ID and ALGOLIA_ADMIN_KEY are both present.

engine/dashboard/tests/fixtures/algolia.fixture.ts:79-89
seedAlgoliaIndex reads those two env values, creates the Algolia client, and first writes with client.setSettings(...).

engine/dashboard/tests/e2e-ui/full/migrate-algolia.spec.ts:24-28
missing credentials throw MissingAlgoliaCredentialsError at module load.

engine/dashboard/tests/e2e-ui/full/migrate-algolia.spec.ts:43-44
test.beforeAll calls seedAlgoliaIndex().

engine/dashboard/tests/e2e-ui/full/migrate-algolia.spec.ts:58
the browser contract test title starts after the failed beforeAll setup.
```

Git history checked for a bucket-2 claim:

```text
git log --oneline -- engine/dashboard/tests/fixtures/algolia.fixture.ts engine/dashboard/tests/e2e-ui/full/migrate-algolia.spec.ts engine/dashboard/tests/global-setup.ts | head -10
eab071939 Fix public CI validation gates
ab362812e Fail closed missing Algolia credentials
c8cb9840b Align migration browser error assertion
a91968f68 Strengthen live migration browser proof
f434443da matt: stage 1 checklist
886814792 Fix migration cleanup proof
9d23021a4 Clarify dashboard credentialed test behavior
29b11577a Wire dashboard Algolia credential guard proof
02c345b2f Fix dashboard Algolia CI wiring
d267d5ebd test: stabilize dashboard suites and flaky assertions
```

No dashboard-owned bucket-1 or bucket-2 defect was found. The computation is
fail-closed in the fixture/spec owner: missing env fails before execution, present
env reaches the vendor, and vendor rejection remains red. Bucket-4 flake is not
supported: the same invalid-credential family reproduced in the original Stage 1
run, the Stage 1 verification rerun, and the Stage 2 focused run with a healthy
local backend. Because the checklist did not define four bucket names in this
artifact, the Stage 2 disposition is explicit rather than forced into a stale
label: **non-dashboard runtime-input residual**.

### Focused Live Preconditions

Required command, run first exactly as written:

```bash
cd engine/dashboard && timeout 600 npx playwright test --project=e2e-ui tests/e2e-ui/full/migrate-algolia.spec.ts --workers=1
```

Validation-cache check:

```text
HEAD=98ce178cdb1e6ceeda1ce80e1a5011558067758e
CLEAN_TREE=True
CACHE_HIT=False
```

First exit:

```text
FOCUSED_MIGRATE_ALGOLIA_EXIT=1
Running 4 tests using 1 worker
2 failed
  [seed] › tests/e2e-ui/seed.setup.ts:19:1 › seed test data
  [cleanup] › tests/e2e-ui/cleanup.setup.ts:13:1 › cleanup test data
2 did not run
```

First-run setup blocker:

```text
Error: apiRequestContext.get: connect ECONNREFUSED 127.0.0.1:7700
GET http://127.0.0.1:7700/health
engine/dashboard/tests/e2e-ui/seed.setup.ts:21
```

The exact focused command does not start the backend; `engine/_dev/s/test
--dashboard-full` owns that precondition through `ensure_server`. A temporary
session-owned backend was then started with the same default test admin key and
an isolated `/tmp/fj-stage2-focused-live-*` data directory. With
`curl -sf http://127.0.0.1:7700/health` green immediately before and after the
run, the same focused command produced the credential specimen:

```text
FOCUSED_MIGRATE_ALGOLIA_LIVE_BACKEND_EXIT=1
Running 4 tests using 1 worker
1 failed
  [e2e-ui] › tests/e2e-ui/full/migrate-algolia.spec.ts:58:3 › Algolia Migration (real browser) › migrate Algolia index via UI: fill form → migrate → verify success → browse
1 did not run
2 passed (4.9s)

ApiError: Invalid Application-ID or API key
at fixtures/algolia.fixture.ts:89
at seedAlgoliaIndex (.../engine/dashboard/tests/fixtures/algolia.fixture.ts:89:3)
at .../engine/dashboard/tests/e2e-ui/full/migrate-algolia.spec.ts:44:11
```

Focused fixture policy evidence:

```text
cd engine/dashboard && npx vitest run tests/fixtures/algolia.fixture.test.ts
ALGOLIA_FIXTURE_VITEST_EXIT=0
Test Files  1 passed (1)
Tests       7 passed (7)
```

### Routed Gap Spec

Residual: the runtime provides `ALGOLIA_APP_ID` and `ALGOLIA_ADMIN_KEY` values
that pass the dashboard presence gate but are rejected by Algolia on the first
vendor write.

Smallest unblock: provision valid Algolia migration-test runtime inputs, or point
`FJ_SECRET_FILE` at an env file containing a valid application ID and admin key.
Do not change fail-closed semantics, do not treat vendor rejection as success,
and do not replace this proof with a mocked/local-only pass.

Owner seam: runtime credential configuration for dashboard Playwright execution.
The dashboard fixture/spec owner is only responsible for fail-closed presence
gating and real vendor seeding, both of which behaved as designed.

Focused command:

```bash
cd engine/dashboard && timeout 600 npx playwright test --project=e2e-ui tests/e2e-ui/full/migrate-algolia.spec.ts --workers=1
```

Proxy bias/tolerance: the fixture unit test proves missing/present env policy but
has zero tolerance for vendor credential validity; it cannot close the live
migration proof. Conditional disposition: after valid runtime inputs are
available, rerun the focused command and then Stage 3's two full-composition
proof runs.

Existing canonical tracker: none found in the stage checklist or receipt.

### Class Sweep

Sweep command:

```bash
rg -n "ALGOLIA_APP_ID|ALGOLIA_ADMIN_KEY|hasAlgoliaCredentials|seedAlgoliaIndex" engine/dashboard/tests
```

Files/specs inspected from the sweep:

```text
engine/dashboard/tests/global-setup.ts
engine/dashboard/tests/README.md
engine/dashboard/tests/fixtures/algolia.fixture.ts
engine/dashboard/tests/fixtures/algolia.fixture.test.ts
engine/dashboard/tests/e2e-ui/full/migrate-algolia.spec.ts
```

Arithmetic: 5 files matched; 1 executable fixture owner contained the env gate
and vendor seed (`algolia.fixture.ts`); 1 browser spec consumed the gate and seed
(`migrate-algolia.spec.ts`); 1 fixture unit test covered the policy; 2 files were
documentation/setup context. Additional dashboard-owned executable instances
found: 0. Additional fixes made: 0.

### Containing Evidence

Containing full e2e-ui command:

```bash
cd engine/dashboard && PLAYWRIGHT_E2E_WORKERS=1 npm run test:e2e-ui:full
```

Validation-cache check:

```text
HEAD=98ce178cdb1e6ceeda1ce80e1a5011558067758e
CLEAN_TREE=True
CACHE_HIT=False
```

Exit and denominator:

```text
E2E_UI_FULL_EXIT=1
Running 367 tests using 1 worker
144 failed
8 skipped
65 did not run
150 passed (9.7m)
```

Arithmetic: 150 passed + 144 failed + 0 flaky + 8 skipped + 65 did not run =
367 total. One failure is the classified Algolia runtime-input residual above.
The remaining failures are a containing-suite runtime locality residual: after
the suite had already run many tests, browser navigations began failing with
`net::ERR_CONNECTION_REFUSED at http://127.0.0.1:5177/...`, while the backend
health probe still passed after the run. Representative source/output:

```text
metrics.spec.ts:34
Error: page.goto: net::ERR_CONNECTION_REFUSED at http://127.0.0.1:5177/metrics

migrate.spec.ts:39
Error: page.goto: net::ERR_CONNECTION_REFUSED at http://127.0.0.1:5177/migrate
```

This residual is not the Stage 1 credential failure and no dashboard fixture/spec
edit was made from it in Stage 2.

### Stage 2 Validation Gates

```text
cd engine/dashboard && npm run test:unit:run
DASHBOARD_UNIT_EXIT=0
Test Files  78 passed (78)
Tests       663 passed (663)

cd engine/dashboard && npm run lint:e2e
LINT_E2E_EXIT=0

cd engine/dashboard && npx tsc --noEmit
TSC_EXIT=0

cd engine/dashboard && npm run lint
LINT_EXIT=0
```

### Stage 2 Triage Table

| Stage 1 failure | Classification | Owners / lines | Fix commit | Post-commit focused evidence | Containing evidence | Sweep denominator | Residual |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `migrate-algolia.spec.ts:58:3` before browser actions; `ApiError: Invalid Application-ID or API key` at `algolia.fixture.ts:89` | explicit classification gap: non-dashboard runtime-input residual; not bucket 1/2 dashboard-owned, not bucket 4 flake | `global-setup.ts:26-38`; `algolia.fixture.ts:71-72,79-89`; `migrate-algolia.spec.ts:24-28,43-44,58` | n/a | n/a; no dashboard-owned fix made. Focused rerun at `98ce178cdb1e6ceeda1ce80e1a5011558067758e` reproduced the same vendor rejection with backend healthy. | `test:e2e-ui:full` at `98ce178cdb1e6ceeda1ce80e1a5011558067758e`: 150 passed / 144 failed / 0 flaky / 8 skipped / 65 did not run / 367 total; includes same Algolia residual plus Vite-port refusal residual. | 5 matching files; 1 fixture owner, 1 consuming browser spec, 1 fixture unit test, 2 docs/setup context; 0 additional executable dashboard-owned instances; 0 fixes. | Provision valid Algolia runtime inputs without weakening fail-closed semantics; rerun focused proof. Separately investigate Vite webserver lifetime before using this session's containing-suite denominator as a green gate. |

Stage 2 conclusion: the sole Stage 1 red is not dashboard-owned. The frozen React
dashboard fixture/spec correctly refuses absent credentials and correctly fails
on vendor rejection. No product code, test, spec, runner, Playwright config,
screen, route, retry, skip, `test.fixme`, or assertion weakening was introduced.

## Stage 3 Independent Full-Composition Verification

Stage 3 observed the canonical `engine/s/test` forwarder and the
`engine/_dev/s/test --dashboard-full` owner without editing either owner,
dashboard product code, tests, specs, or Playwright configuration. Both
independent current-source commands were:

```bash
cd engine
timeout 3600 ./s/test --dashboard-full > /tmp/jul30_12am_3_dashboard_full_run2.txt 2>&1; run_exit=$?; echo "DASHBOARD_FULL_RUN2_EXIT=$run_exit"; tail -60 /tmp/jul30_12am_3_dashboard_full_run2.txt
```

```bash
cd engine
timeout 3600 ./s/test --dashboard-full > /tmp/jul30_12am_3_dashboard_full_run3.txt 2>&1; run_exit=$?; echo "DASHBOARD_FULL_RUN3_EXIT=$run_exit"; tail -60 /tmp/jul30_12am_3_dashboard_full_run3.txt
```

Both preflights were clean at:

```text
HEAD=ddb6fccef82af3e43eedf88778a89f28dd2cbe33
MERGE_BASE_HEAD_MAIN=2cfcfbcd856e15d4bf4da35dfe50ebfe9b571f84
```

### Run 2

Preflight:

```text
DATE=Thu Jul 30 03:07:44 EDT 2026
UPTIME=3:07 up 1 day, 6:16, 7 users, load averages: 23.08 52.28 59.43
STATUS_PORCELAIN_UNTRACKED_ALL=<empty>
```

The command printed `DASHBOARD_FULL_RUN2_EXIT=1`. Control-byte-normalized,
phase-bounded output was:

```text
[1] Dashboard: Vitest unit tests
Test Files  78 passed (78)
Tests       663 passed (663)
[2] Starting flapjack server
Server failed to start
```

Phases `[3] Dashboard: Playwright smoke tests` and `[5] Dashboard: Playwright
full e2e` are absent because the canonical runner exited at phase `[2]`.
Accordingly both Playwright phases are `UNPARSEABLE / DID NOT RUN`, not
zero-test passes. The report and result trees present after this run were last
modified at 02:50:00, before the 03:07:44 preflight. Their archives are retained
as explicitly stale previous-run trees and are not run-2 Playwright evidence.

An isolated same-SHA, same-host startup diagnostic became healthy on
`127.0.0.1:7700` at probe second 2. Its transcript hash is
`684e739c8f16c2c22b0db54bd8fc7f86069a435976dd2621fa6ce6d7ce2a27aa`.
The run-2 startup failure therefore did not reproduce in isolation.

### Run 3

Run 3 began only after run 2 exited and its transcript and stale-tree
classification were archived.

```text
DATE=Thu Jul 30 03:12:19 EDT 2026
UPTIME=3:12 up 1 day, 6:20, 7 users, load averages: 25.37 35.87 50.05
STATUS_PORCELAIN_UNTRACKED_ALL=<empty>
HEAD=ddb6fccef82af3e43eedf88778a89f28dd2cbe33
```

Control-byte-normalized, phase-bounded output was:

```text
[1] Dashboard: Vitest unit tests
Test Files  78 passed (78)
Tests       663 passed (663)
[3] Dashboard: Playwright smoke tests
Running 17 tests using 3 workers
17 passed (11.2s)
[5] Dashboard: Playwright full e2e
Running 367 tests using 1 worker
1 failed
8 skipped
1 did not run
357 passed (3.8m)
ApiError: Invalid Application-ID or API key
Serving HTML report at http://localhost:9323. Press Ctrl+C to quit.
```

The sole failing title was:

```text
[e2e-ui] › tests/e2e-ui/full/migrate-algolia.spec.ts:58:3
› Algolia Migration (real browser)
› migrate Algolia index via UI: fill form → migrate → verify success → browse
```

The specified exit echo was not reached because the non-CI HTML reporter served
the report indefinitely after the red summary. Two terminal interrupts did not
return the tracked command; the exact owned report process, PID 92312, was then
sent `SIGINT`, after which the execution tool returned 1. No
`ERR_CONNECTION_REFUSED` or `127.0.0.1:5177` refusal occurred in run 3.

### Denominators and Comparison

| Run | Vitest | Smoke passed / failed / flaky / skipped / did not run / total | Full passed / failed / flaky / skipped / did not run / total | Observed command exit | Preflight load |
| --- | --- | --- | --- | --- | --- |
| 2 | 663 / 663 | UNPARSEABLE / DID NOT RUN | UNPARSEABLE / DID NOT RUN | 1 | 23.08 / 52.28 / 59.43 |
| 3 | 663 / 663 | 17 / 0 / 0 / 0 / 0 / 17 | 357 / 1 / 0 / 8 / 1 / 367 | execution tool 1 after exact-PID interrupt | 25.37 / 35.87 / 50.05 |

Run-3 arithmetic is exact:

```text
smoke: 17 + 0 + 0 + 0 + 0 = 17
full: 357 + 1 + 0 + 8 + 1 = 367
```

The two runs used the same source but did not produce the same execution
boundary. Run 2 stopped before either Playwright phase; run 3 completed all test
summaries but did not return naturally after its red full suite. One fuller run
does not overrule the incomplete run, and both observed executions were
nonzero.

### Merge-Base Provenance

A disposable detached worktree at
`/tmp/jul30-stage3-mergebase.obgO27/worktree` was verified at:

```text
2cfcfbcd856e15d4bf4da35dfe50ebfe9b571f84
```

It reused the already-built merge-base binary through a worktree-local
`engine/target` symlink and received the same present Algolia input variables,
host locality, canonical sequence, smoke worker count, full worker count, and
3600-second bound. The faithful command was:

```bash
cd engine
timeout 3600 ./s/test --dashboard-full > /tmp/jul30_12am_3_dashboard_full_merge_base.txt 2>&1; run_exit=$?; echo "DASHBOARD_FULL_MERGE_BASE_EXIT=$run_exit"; tail -60 /tmp/jul30_12am_3_dashboard_full_merge_base.txt
```

Preflight load was `27.27 / 16.99 / 29.87`. The exact same Algolia failure
title and same post-summary HTML-report hang reproduced:

```text
Test Files  78 passed (78)
Tests       663 passed (663)
Running 17 tests using 3 workers
17 passed (8.3s)
Running 367 tests using 1 worker
1 failed
8 skipped
1 did not run
357 passed (3.9m)
Serving HTML report at http://localhost:9323. Press Ctrl+C to quit.
```

Arithmetic remained `17 + 0 + 0 + 0 + 0 = 17` and
`357 + 1 + 0 + 8 + 1 = 367`. The exact owned merge-base report PID was sent
`SIGINT`; the shell then printed `DASHBOARD_FULL_MERGE_BASE_EXIT=130`.
Postflight load was `57.48 / 26.98 / 28.67`. The scratch worktree was verified
and removed after its transcript and Playwright trees were archived.

The invalid Algolia credential residual and the HTML-report non-return are
therefore faithful merge-base reproductions and pre-existing at the comparison
boundary. Run 2's phase-2 startup failure is **INCONCLUSIVE**: it did not
reproduce in the isolated current-source startup probe or the faithful
merge-base composition, and host load was not identical. It is not classified
as either a current regression or a pre-existing red.

The canonical run also rewrote the tracked
`docs/live-state/jun04_pm_lane_c_baseline/20260604T191244Z/movies_seed_verify.json`
in both the current and merge-base worktrees. This was a generated evidence
rewrite, not a Stage 3 source edit. The current worktree copy was restored to its
clean preflight `HEAD` value, and the merge-base copy disappeared with the
verified scratch worktree. This is a runner/test-harness side-effect gap, not
product evidence for the dashboard join.

### Residual Gap Specifications

1. **Algolia runtime input, pre-existing red.** The supplied
   `ALGOLIA_APP_ID` and `ALGOLIA_ADMIN_KEY` pass the presence check but the
   vendor rejects the first write. The smallest unblock is valid credential
   provisioning. The owner is the dashboard Playwright runtime secret-input
   seam; `tests/fixtures/algolia.fixture.ts` and
   `tests/e2e-ui/full/migrate-algolia.spec.ts` correctly remain fail-closed.
   The faithful path is the full composition with valid inputs. The fixture
   unit test is only a presence-policy proxy and has no tolerance for vendor
   validity, so it cannot close this residual. Conditional disposition: rerun
   the focused migration spec, then both full compositions, after provisioning.
2. **HTML report non-return, pre-existing runner gap.** Local
   `playwright.config.ts` owns `reporter: 'html'`; the canonical full runner
   invokes the red suite without a noninteractive HTML-open override. The
   smallest unblock is an owner-aligned runner/config change plus an automated
   contract proving a failed Playwright command returns nonzero without manual
   interruption. The faithful path is the full composition; a direct
   `PLAYWRIGHT_HTML_OPEN=never` invocation is a biased proxy because it is not
   the canonical owner command. Conditional disposition: return this gap to the
   runner owner; Stage 3 does not edit that owner.
3. **Run-2 startup, inconclusive runner gap.** `engine/_dev/s/test` owns
   `ensure_server`; the smallest unblock is preserved startup diagnostics and a
   falsifiable startup-lifetime contract under preceding suite load. An
   isolated binary start is a lower-load proxy and therefore cannot close the
   comparison. Conditional disposition: retain the run-2 red and route the gap
   to the runner owner; do not call it pre-existing or current-only.
4. **Tracked evidence mutation, pre-existing runner/test-harness gap.** The
   reusable movie seeder writes into the Lane C evidence bundle when that
   runtime input is present, so the canonical full suite can dirty a clean
   checkout. The smallest unblock is an isolated temporary verification
   destination for ordinary full-suite execution plus a test asserting the
   tracked tree stays unchanged. Conditional disposition: route to the
   runner/test-fixture owner; Stage 3 restored only its generated delta.

### Preserved Evidence

Evidence directory:

```text
/Users/stuart/.matt/projects/flapjack_dev-63d06a34/jul30_12am_3_dashboard_join_truth_up.md-e6c91c9e/artifacts/stage_03
```

| Specimen | SHA-256 |
| --- | --- |
| run-2 transcript | `6529a1133063a095f34d6cbef53bd04e29d10f2744d5492ae4defacc13d5e53e` |
| run-2 stale prior report archive | `79fa61c4125bce2a61e461025ed5cbea01e94a6aad1e2723df11d0794901283a` |
| run-2 stale prior results archive | `cd10e2a29b858e3b7b77e9dfa291b6536827941cdb84697b6e45cc61c5d24403` |
| run-3 transcript | `f23a4ae19d570b05d5104866f54c051adc98b0936560a25e2394e8e3f5b88710` |
| run-3 report archive | `47d8e7616c123aaff9a1a1c0e2a95d1e21d076f980e23ea1dd7a1a091e78916e` |
| run-3 results archive | `eb170ce87eb02cd0dc504b208713a6d0ebfe7838e656a63af68d58890a22a661` |
| merge-base transcript | `f1c9c213b64785cd781b4013cadc6d475c31e7db2aaed90d25bfa930efa4000e` |
| merge-base report archive | `05cdad943cb3793f3fe2f6ff1339b41c2b552fa282d87794f3b272cfae28cadf` |
| merge-base results archive | `1c4826c06f82805c8e9fdc4db4850cef4640c2cb9e26ef1ee833d27fcc2bfcbc` |

Every listed transcript and archive was nonempty when hashed. Run-2's two
Playwright archives are retained only to prove they were stale and are excluded
from run-2 denominators.

### Stage 3 Falsifiable Statement

At `ddb6fccef82af3e43eedf88778a89f28dd2cbe33`, run 2 of
`./s/test --dashboard-full` returned 1 with Vitest 663/663, smoke
UNPARSEABLE/DID NOT RUN, and full UNPARSEABLE/DID NOT RUN at preflight load
23.08/52.28/59.43; run 3 returned execution-tool exit 1 after exact-PID
interruption with Vitest 663/663, smoke 17/0/0/0/0/17, and full
357/1/0/8/1/367 at preflight load 25.37/35.87/50.05. Residuals are the invalid
Algolia runtime inputs and pre-existing HTML-report non-return, the
INCONCLUSIVE run-2 server-start failure, and the pre-existing tracked-evidence
mutation. The two independent runs do not establish a passing full composition.

## Stage 4 Backend-Frontend Join Matrix

### Snapshot and denominator

This is a dated evidence snapshot, not a capability, route, or test owner.
`engine/docs2/FEATURES.md` owns the capability labels,
`engine/dashboard/src/App.tsx::App` owns dashboard routes, the cited production
files own non-dashboard seams, and the bounded Playwright corpus plus Stage 1-3
execution evidence owns proof status.

Pre-edit snapshot:

```text
HEAD=fe8a3393871109b6175aa545e8fdee9ceba84921
DATE=2026-07-30 03:55:45 EDT
git status --porcelain --untracked-files=all=<empty>
```

Stage 3's actual proof executions were at
`ddb6fccef82af3e43eedf88778a89f28dd2cbe33` (Stage 3 receipt lines 836-885);
its clean-review handoff was later at
`12a1a2a685d76984658a09053b818aa60f891f24`. Neither equals the Stage 4
audited SHA. Therefore every candidate spec below is conservatively classified
`not executed at audited SHA`; Stage 3's pass/fail outcomes are historical
context only and are not transplanted to this SHA.

The named `FEATURES.md` tables contain 92 rows:

```text
Search 19 + High Availability 1 + Indexing & Records 11 + Index Settings 15
+ Analytics & Insights 8 + Personalization & AI 6 + API Keys & Security 6
+ Dictionaries 4 + Infrastructure 11 + Operational / Observability 5
+ Algolia migration 6 = 92
92 - Dashboard Migrate page - Backend ↔ frontend joined end-to-end = 90 backend rows
```

The two excluded migration labels are `Dashboard Migrate page`
(`FEATURES.md:299`) and `Backend ↔ frontend joined end-to-end`
(`FEATURES.md:300`). They are respectively a frontend row and the summary claim
this matrix audits, not backend capabilities.

### Route denominator

Direct derivation from `App.tsx:62-88` produces 24 user-facing patterns:
`/`, `/overview`, `/index/:indexName`, `/index/:indexName/settings`,
`/index/:indexName/analytics`, `/index/:indexName/synonyms`,
`/index/:indexName/rules`, `/index/:indexName/merchandising`,
`/index/:indexName/recommendations`, `/index/:indexName/chat`, `/keys`,
`/logs`, `/migrate`, `/metrics`, `/cluster`, `/system`,
`/query-suggestions`, `/experiments`, `/experiments/:experimentId`, `/events`,
`/personalization`, `/dictionaries`, `/security-sources`, and `*`.

These are separate source facts:

- 24 derived user-facing patterns, including two index routes and `*`;
- 24 unique raw `path=` attributes: `/`, `overview`, `index/:indexName`,
  `settings`, `analytics`, `synonyms`, `rules`, `merchandising`,
  `recommendations`, `chat`, `keys`, `logs`, `migrate`, `metrics`, `cluster`,
  `system`, `query-suggestions`, `experiments`, `experiments/:experimentId`,
  `events`, `personalization`, `dictionaries`, `security-sources`, and `*`;
- 2 index routes have no `path=` attribute (`App.tsx:63,66`);
- 22 lazy components are declared at `App.tsx:15-36`; `Overview` serves two
  patterns and the wildcard has no lazy component.

The cross-check list `APP_USER_FACING_ROUTE_PATTERNS`
(`route_audit_manifest.ts:19-44`) exactly matches all 24 derived patterns.
`buildDashboardRouteAudit` plus `EXCLUDED_DASHBOARD_ROUTES` is designed to
cover that list (`route_audit_manifest.ts:46-62,295-302`); the exclusions are
`*` and `/experiments/:experimentId`. There is no App/manifest pattern drift.
There is public-summary drift: `FEATURES.md:11-12` still says 21 lazy components
and 37 full e2e-ui specs, while this snapshot has 22 and 38.

### Production-owner and proof keys

Production owner keys keep the matrix readable without creating a second
definition:

- O1 search and settings: `engine/flapjack-http/src/router.rs:162-233`,
  `handlers/search/single.rs:20-151`, and `handlers/settings.rs:406-580`.
- O2 objects, browse, index operations, and tasks:
  `router.rs:165-288`, `handlers/objects/mod.rs:285-1039`,
  `handlers/browse.rs:196`, `handlers/indices.rs:139-781`, and
  `handlers/tasks.rs:38-64`.
- O3 CLI ingestion: `engine/flapjack-server/src/main.rs:46-50,176-179`,
  `ingest.rs:31-92`, and `ingest_replace.rs:203-303`.
- O4 analytics/insights: `engine/flapjack-http/src/router.rs:70-78,277-280`
  and `engine/src/analytics/config.rs:18-101`.
- O5 personalization, recommendations, chat, and experiments:
  `handlers/personalization.rs:52-206`, `handlers/recommend.rs:165`,
  `handlers/chat.rs:63`, and `handlers/experiments/mod.rs:114-499`.
- O6 keys, dictionaries, and security sources:
  `router.rs:108-127`, `handlers/keys.rs:113-419`,
  `handlers/dictionaries.rs:45-136`, and
  `handlers/security_sources.rs:36-111`.
- O7 operations/infrastructure: `router.rs:80-105,205-225,384-438`,
  `handlers/internal.rs:236-1005`, `handlers/metrics.rs:15`,
  `handlers/health.rs:56`, and `handlers/readiness.rs:27`.
- O8 migration and publication: `router.rs:133-159,264-275`,
  `handlers/migration/mod.rs:436-605`,
  `handlers/migration/bulk_replace.rs:55-217`, and
  `engine/src/index/manager/publication.rs`.
- O9 runtime configuration: `engine/flapjack-http/src/background_tasks.rs:15`,
  `startup.rs:58-210`, `server.rs:153,300-334`, and
  `engine/src/analytics/config.rs:87`.

Every proof key names an exact Playwright title from the bounded corpus. All
keys have the disposition **not executed at audited SHA**:

- P1 `e2e-ui/full/search.spec.ts:192` — `searching for "laptop" returns laptop products`
- P2 `e2e-ui/full/search.spec.ts:520` — `typo tolerance returns results for misspelled queries`
- P3 `e2e-ui/full/search.spec.ts:220` — `filtering by Audio category shows only audio products`
- P4 `e2e-ui/full/synonyms.spec.ts:63` — `create and delete a multi-way synonym`
- P5 `e2e-ui/full/rules.spec.ts:112` — `form mode create/read/delete is deterministic and restores seeded baseline`
- P6 `e2e-ui/full/settings.spec.ts:263` — `save settings persists changes after reload`
- P7 `e2e-ui/full/settings.spec.ts:284` — `search tab query type persists after save and reload`
- P8 `e2e-ui/full/settings.spec.ts:308` — `language and text tab query languages persist after save and reload`
- P9 `e2e-ui/full/settings.spec.ts:334` — `ranking tab distinct settings persist after save and reload`
- P10 `e2e-ui/full/search.spec.ts:689` — `create document via JSON tab and verify searchable`
- P11 `e2e-ui/full/search.spec.ts:710` — `delete document via confirm dialog and verify removed`
- P13 `e2e-ui/full/analytics.spec.ts:42` — `Overview tab loads with KPI cards showing data`
- P14 `e2e-ui/full/event-debugger.spec.ts:108` — `seeded events appear in the event table`
- P15 `e2e-ui/full/experiments.spec.ts:364` — `create dialog submit creates an experiment through the UI flow`
- P16 `e2e-ui/full/metrics.spec.ts:62` — `Overview tab shows aggregate request cards with numeric values`
- P17 `e2e-ui/full/personalization.spec.ts:24` — `uses starter strategy defaults, persists event and facet edits, and unlocks profile lookup after save`
- P18 `e2e-ui/full/recommendations.spec.ts:197` — `all five recommendation models: model switching enforces inputs and renders result-or-empty states`
- P19 `e2e-ui/full/chat.spec.ts:166` — `sends query, displays answer with sources, and supports multi-turn`
- P20 `e2e-ui/full/vector-settings.spec.ts:182` — `set search mode to Neural Search and verify persistence`
- P21 `e2e-ui/full/api-keys.spec.ts:112` — `create then delete an API key`
- P22 `e2e-ui/full/security-sources.spec.ts:58` — `create-delete lifecycle preserves badge and row counts`
- P23 `e2e-ui/full/dictionaries.spec.ts:44` — `switches between Stopwords, Plurals, and Compounds tabs`
- P24 `e2e-ui/full/system.spec.ts:60` — `Health tab shows server status as ok`
- P25 `e2e-ui/full/system.spec.ts:234` — `Replication tab shows replication enabled/disabled status`
- P26 `e2e-ui/full/system.spec.ts:288` — `Snapshots tab shows S3 Backups section`
- P27 `e2e-ui/full/system.spec.ts:128` — `Health tab shows tenants loaded card`
- P28 `e2e-ui/full/system.spec.ts:137` — `Health tab shows memory card with heap usage and progress bar`
- P29 `e2e-ui/full/migrate-algolia.spec.ts:58` — `migrate Algolia index via UI: fill form → migrate → verify success → browse`

The listed P-keys are candidate joined tests, not current proof. A `none` cell means no
bounded-corpus test was found for that row. Route-only navigation tests and
`assertDashboardRouteCoverage` are deliberately omitted because they do not
exercise the backend capability.

### Stage 5 settings-surface correction

Stage 5 rechecked settings surfaces against operable controls rather than route
adjacency. The dashboard owner exposes `searchableAttributes`, `hitsPerPage`,
and `queryType` (`SettingsForm.tsx:164-210`); ranking, custom ranking, and
distinct controls (`SettingsForm.tsx:214-284`); query languages, stop words,
plurals, and typo-size thresholds (`SettingsForm.tsx:288-369`);
`attributesForFaceting` (`SettingsForm.tsx:372-422`); retrieve,
unretrievable, highlight, and highlight-tag controls
(`SettingsTabContent.tsx:30-93`); and vector/AI controls
(`SettingsTabContent.tsx:125-188` plus `SearchModeSection.tsx` and
`EmbedderPanel.tsx`).

By contrast, the backend settings owner accepts `attributesToSnippet`,
`paginationLimitedTo`, `removeWordsIfNoResults`, `enableReRanking`,
`disableTypoToleranceOnAttributes`, `replicas`, and
`numericAttributesForFiltering` (`handlers/settings.rs:59-61,87-88,113-117,
196-218`). The dashboard `IndexSettings` contract and the operable controls
above do not expose those fields. Search-request-only `optionalFilters`,
`sumOrFiltersScores`, and `typoTolerance` likewise have no control in the
current dashboard source. `decompoundQuery` remains build/runtime/query
configuration rather than an operable Settings control. Composite rows retain
a dashboard surface only for their named supported member: highlight but not
snippet, `queryType` but not `removeWordsIfNoResults`, and `hitsPerPage` but
not `paginationLimitedTo`. These source facts correct the cells below without
promoting any proof disposition.

### Join matrix

| capability | source | surface | spec proof | joined | note |
| --- | --- | --- | --- | --- | --- |
| Full-text search (BM25 scoring) | `FEATURES.md:124` Search | dashboard route `/index/:indexName` | P1 — not executed at audited SHA | no | O1; candidate exercises real query/results but has no current-SHA disposition. |
| Typo tolerance | `FEATURES.md:125` Search | dashboard route `/index/:indexName` | P2 — not executed at audited SHA | no | O1. |
| Prefix search | `FEATURES.md:126` Search | dashboard route `/index/:indexName/settings` | P7 — not executed at audited SHA | no | O1; settings candidate covers `queryType`, not every prefix mode. |
| Exact phrase / word search | `FEATURES.md:127` Search | dashboard route `/index/:indexName` | P1 — not executed at audited SHA | no | O1; candidate does not isolate exact phrase/word mode. |
| Faceted search | `FEATURES.md:128` Search | dashboard route `/index/:indexName` | P3 — not executed at audited SHA | no | O1. |
| Numeric + string filters | `FEATURES.md:129` Search | dashboard route `/index/:indexName` | P3 — not executed at audited SHA | no | O1; candidate covers a string facet, not numeric/range syntax. |
| Geo search | `FEATURES.md:130` Search | dashboard route `/index/:indexName` | none | no | O1; no bounded real-UI geo-search proof found. |
| Synonyms | `FEATURES.md:131` Search | dashboard route `/index/:indexName/synonyms` | P4 — not executed at audited SHA | no | O1. |
| Query rules | `FEATURES.md:132` Search | dashboard route `/index/:indexName/rules` | P5 — not executed at audited SHA | no | O1. |
| Distinct (deduplication) | `FEATURES.md:133` Search | dashboard route `/index/:indexName/settings` | P9 — not executed at audited SHA | no | O1; candidate persists distinct settings but does not prove all grouping behavior. |
| Multi-index search | `FEATURES.md:134` Search | API only | none | no | O1; no dashboard multi-index query surface. |
| Highlight / snippet | `FEATURES.md:135` Search | dashboard route `/index/:indexName/settings` | P6 — not executed at audited SHA | no | O1; generic persistence candidate does not isolate rendering semantics. |
| Smart sorting | `FEATURES.md:136` Search | dashboard route `/index/:indexName` | P1 — not executed at audited SHA | no | O1; candidate does not cover all three sorting modes. |
| Custom ranking | `FEATURES.md:137` Search | dashboard route `/index/:indexName/settings` | P9 — not executed at audited SHA | no | O1. |
| Optional filters (soft boost) | `FEATURES.md:138` Search | API only | none | no | O1; `optionalFilters` is a search-request capability with no operable dashboard control. |
| Sum of filters scoring | `FEATURES.md:139` Search | API only | none | no | O1; `sumOrFiltersScores` is a search-request capability with no operable dashboard control. |
| Decompounding | `FEATURES.md:140` Search | config only | none | no | O9; build feature plus settings/query flags have no complete console workflow. |
| CJK tokenization | `FEATURES.md:141` Search | dashboard route `/index/:indexName/settings` | P8 — not executed at audited SHA | no | O1; language persistence is not CJK result proof. |
| Language-specific stemming | `FEATURES.md:142` Search | dashboard route `/index/:indexName/settings` | P8 — not executed at audited SHA | no | O1; language persistence is not stemming-result proof. |
| Dead-node auto-heal | `FEATURES.md:148` High Availability | config only | none | no | O9/O7; enabled by env and reported by cluster status, but `Cluster.tsx:248-290` renders no autoheal lifecycle fields. |
| Schemaless JSON upload | `FEATURES.md:154` Indexing & Records | dashboard route `/index/:indexName` | P10 — not executed at audited SHA | no | O2. |
| `flapjack ingest` beta | `FEATURES.md:155` Indexing & Records | CLI only | none | no | O3. |
| Atomic bulk-replace job API | `FEATURES.md:156` Indexing & Records | API only | none | no | O8/O3; CLI replace submits and polls, but dashboard has no job or cancel surface. |
| Single record CRUD | `FEATURES.md:157` Indexing & Records | dashboard route `/index/:indexName` | P10, P11 — not executed at audited SHA | no | O2. |
| Batch operations | `FEATURES.md:158` Indexing & Records | dashboard route `/index/:indexName` | P10 — not executed at audited SHA | no | O2; candidate submits a document but does not cover every batch action. |
| Browse (full index scan) | `FEATURES.md:159` Indexing & Records | dashboard route `/index/:indexName` | P1 — not executed at audited SHA | no | O2; visible search results are not full cursor-scan proof. |
| deleteByQuery | `FEATURES.md:160` Indexing & Records | API only | none | no | O2. |
| partialUpdateObjects | `FEATURES.md:161` Indexing & Records | API only | none | no | O2. |
| Index copy / move / clear | `FEATURES.md:162` Indexing & Records | API only | none | no | O2; dashboard index create/delete does not cover copy, move, and clear. |
| Replicas | `FEATURES.md:163` Indexing & Records | API only | none | no | O1/O2; the settings API accepts replica topology, but the dashboard Settings form has no replica control. |
| Task status API | `FEATURES.md:164` Indexing & Records | API only | none | no | O2. |
| searchableAttributes | `FEATURES.md:180` Index Settings | dashboard route `/index/:indexName/settings` | P6 — not executed at audited SHA | no | O1. |
| attributesForFaceting | `FEATURES.md:181` Index Settings | dashboard route `/index/:indexName/settings` | P6 — not executed at audited SHA | no | O1. |
| ranking (built-in criteria) | `FEATURES.md:182` Index Settings | dashboard route `/index/:indexName/settings` | P9 — not executed at audited SHA | no | O1. |
| customRanking | `FEATURES.md:183` Index Settings | dashboard route `/index/:indexName/settings` | P9 — not executed at audited SHA | no | O1. |
| attributesToRetrieve | `FEATURES.md:184` Index Settings | dashboard route `/index/:indexName/settings` | P6 — not executed at audited SHA | no | O1. |
| attributesToHighlight / Snippet | `FEATURES.md:185` Index Settings | dashboard route `/index/:indexName/settings` | P6 — not executed at audited SHA | no | O1; the route controls `attributesToHighlight`, but not backend-owned `attributesToSnippet`. |
| queryType / removeWordsIfNoResults | `FEATURES.md:186` Index Settings | dashboard route `/index/:indexName/settings` | P7 — not executed at audited SHA | no | O1; the route controls `queryType`, but not backend-owned `removeWordsIfNoResults`. |
| typoTolerance settings | `FEATURES.md:187` Index Settings | API only | none | no | O1; query-level `typoTolerance` has no operable dashboard control; the separate typo-size controls are the next row. |
| minWordSizeFor1/2Typos | `FEATURES.md:188` Index Settings | dashboard route `/index/:indexName/settings` | P6 — not executed at audited SHA | no | O1. |
| ignorePlurals / removeStopWords | `FEATURES.md:189` Index Settings | dashboard route `/index/:indexName/settings` | P8 — not executed at audited SHA | no | O1. |
| Pagination settings (hitsPerPage, paginationLimitedTo) | `FEATURES.md:190` Index Settings | dashboard route `/index/:indexName/settings` | P6 — not executed at audited SHA | no | O1; the route controls `hitsPerPage`, but not backend-owned `paginationLimitedTo`. |
| numericAttributesForFiltering | `FEATURES.md:191` Index Settings | API only | none | no | O1; the settings API accepts the field, but the dashboard Settings form has no control. |
| unretrievableAttributes | `FEATURES.md:192` Index Settings | dashboard route `/index/:indexName/settings` | P6 — not executed at audited SHA | no | O1. |
| disableTypoToleranceOnAttributes | `FEATURES.md:193` Index Settings | API only | none | no | O1; the settings API accepts the field, but the dashboard Settings form has no control. |
| All remaining Algolia settings | `FEATURES.md:194` Index Settings | API only | none | no | O1; the backend compatibility surface exceeds the bounded Settings form, so route adjacency cannot establish this closed-world claim. |
| Search query logs | `FEATURES.md:200` Analytics & Insights | dashboard route `/logs` | none | no | O4; bounded log specs exercise the dashboard's client-side API log, not backend search-query analytics. |
| Analytics API (top queries, no-results, no-clicks) | `FEATURES.md:201` Analytics & Insights | dashboard route `/index/:indexName/analytics` | P13 — not executed at audited SHA | no | O4; candidate covers top/no-result data, not every no-click mode. |
| Events / Insights API | `FEATURES.md:202` Analytics & Insights | dashboard route `/events` | P14 — not executed at audited SHA | no | O4; debugger viewing is not complete click/conversion/view ingestion proof. |
| Event Debugger | `FEATURES.md:203` Analytics & Insights | dashboard route `/events` | P14 — not executed at audited SHA | no | O4. |
| A/B Testing (experiments) | `FEATURES.md:204` Analytics & Insights | dashboard route `/experiments` | P15 — not executed at audited SHA | no | O5. |
| Usage metering | `FEATURES.md:205` Analytics & Insights | dashboard route `/metrics` | P16 — not executed at audited SHA | no | O4/O7; candidate checks aggregate request metrics, not every per-key/per-index counter. |
| Analytics retention cleanup | `FEATURES.md:206` Analytics & Insights | config only | none | no | O4/O9. |
| Durable analytics rollup storage | `FEATURES.md:207` Analytics & Insights | dashboard route `/index/:indexName/analytics` | P13 — not executed at audited SHA | no | O4; UI consumption cannot distinguish rollup planning from raw fallback. |
| Personalization API | `FEATURES.md:213` Personalization & AI | dashboard route `/personalization` | P17 — not executed at audited SHA | no | O5. |
| Personalization in search | `FEATURES.md:214` Personalization & AI | dashboard route `/personalization` | P17 — not executed at audited SHA | no | O5; strategy/profile workflow does not isolate personalized query ranking. |
| Recommendations API | `FEATURES.md:215` Personalization & AI | dashboard route `/index/:indexName/recommendations` | P18 — not executed at audited SHA | no | O5. |
| AI Search / RAG endpoint | `FEATURES.md:216` Personalization & AI | dashboard route `/index/:indexName/chat` | P19 — not executed at audited SHA | no | O5. |
| Re-ranking (enableReRanking) | `FEATURES.md:217` Personalization & AI | API only | none | no | O1/O5; the dashboard controls neural/keyword search mode, not the backend `enableReRanking` setting. |
| Vector search | `FEATURES.md:218` Personalization & AI | dashboard route `/index/:indexName/settings` | P20 — not executed at audited SHA | no | O1/O5; candidate covers mode persistence, not every shipped vector mode. |
| API Keys | `FEATURES.md:224` API Keys & Security | dashboard route `/keys` | P21 — not executed at audited SHA | no | O6. |
| ACL (Access Control Lists) | `FEATURES.md:225` API Keys & Security | dashboard route `/keys` | P21 — not executed at audited SHA | no | O6; lifecycle candidate does not exercise every ACL. |
| Key restrictions | `FEATURES.md:226` API Keys & Security | dashboard route `/keys` | P21 — not executed at audited SHA | no | O6; additional candidate coverage exists for index scope and restrictSources, not every restriction. |
| Rate limiting per key | `FEATURES.md:227` API Keys & Security | dashboard route `/keys` | none | no | O6; no bounded UI proof of limit configuration plus enforcement. |
| Security Sources / Vault | `FEATURES.md:228` API Keys & Security | dashboard route `/security-sources` | P22 — not executed at audited SHA | no | O6. |
| Secured API keys (signed) | `FEATURES.md:229` API Keys & Security | API only | none | no | O6. |
| Stop words dictionary | `FEATURES.md:235` Dictionaries | dashboard route `/dictionaries` | P23 — not executed at audited SHA | no | O6. |
| Plurals dictionary | `FEATURES.md:236` Dictionaries | dashboard route `/dictionaries` | P23 — not executed at audited SHA | no | O6. |
| Compounds dictionary | `FEATURES.md:237` Dictionaries | dashboard route `/dictionaries` | P23 — not executed at audited SHA | no | O6. |
| Custom entries | `FEATURES.md:238` Dictionaries | dashboard route `/dictionaries` | P23 — not executed at audited SHA | no | O6. |
| Multi-tenant isolation | `FEATURES.md:244` Infrastructure | dashboard route `/system` | P27 — not executed at audited SHA | no | O7; tenant count visibility is not isolation enforcement proof. |
| Oplog replication + startup catch-up | `FEATURES.md:245` Infrastructure | dashboard route `/system` | P25 — not executed at audited SHA | no | O7; status visibility is not catch-up behavior proof. |
| S3 snapshots | `FEATURES.md:246` Infrastructure | dashboard route `/system` | P26 — not executed at audited SHA | no | O7; section visibility is not snapshot/restore lifecycle proof. |
| Published operations APIs | `FEATURES.md:247` Infrastructure | dashboard route `/system` | P24 — not executed at audited SHA | no | O7; system health covers only part of the four-route contract. |
| SSL / TLS | `FEATURES.md:248` Infrastructure | dashboard route `/system` | none | no | O7; System renders renewal status when present, but no bounded joined TLS proof exists. |
| OpenAPI spec | `FEATURES.md:249` Infrastructure | API only | none | no | O7; Swagger/OpenAPI is not a dashboard product route in `App.tsx`. |
| Memory safety | `FEATURES.md:250` Infrastructure | dashboard route `/system` | P28 — not executed at audited SHA | no | O7; memory display does not prove 429/drop behavior. |
| Health endpoint | `FEATURES.md:251` Infrastructure | dashboard route `/system` | P24 — not executed at audited SHA | no | O7. |
| Readiness probe (`/health/ready`) | `FEATURES.md:252` Infrastructure | dashboard route `/system` | P24 — not executed at audited SHA | no | O7; health UI does not isolate readiness branches. |
| Request latency histograms | `FEATURES.md:253` Infrastructure | dashboard route `/metrics` | P16 — not executed at audited SHA | no | O7; aggregate cards do not prove histogram label normalization. |
| Error response parity | `FEATURES.md:254` Infrastructure | API only | none | no | O1/O2; no dashboard console surface can establish closed-world HTTP parity. |
| Request ID propagation (Stage 1) | `FEATURES.md:263` Operational / Observability | API only | none | no | O7; response header/span propagation has no dashboard console control. |
| JSON structured logging (Stage 2) | `FEATURES.md:264` Operational / Observability | config only | none | no | O9. |
| Configurable CORS origins (Stage 4) | `FEATURES.md:265` Operational / Observability | config only | none | no | O9. |
| Graceful shutdown timeout (Stage 5) | `FEATURES.md:266` Operational / Observability | config only | none | no | O9. |
| Startup dependency summary (Stage 6) | `FEATURES.md:267` Operational / Observability | config only | none | no | O9; emitted startup logs have no App route. |
| Source export: Algolia → durable on-disk spool (checkpointed, resumable) | `FEATURES.md:295` Algolia migration | dashboard route `/migrate` | P29 — not executed at audited SHA | no | O8; candidate is create-only and does not prove async/resume. |
| Translation: spool → Flapjack documents/settings/synonyms/rules | `FEATURES.md:296` Algolia migration | dashboard route `/migrate` | P29 — not executed at audited SHA | no | O8; candidate is create-only. |
| Import: translated content → target index via staged publication | `FEATURES.md:297` Algolia migration | dashboard route `/migrate` | P29 — not executed at audited SHA | no | O8; candidate is create-only, while sync and async overwrite are shipped. |
| Staged publication primitive (crash-safe, node-local) | `FEATURES.md:298` Algolia migration | API only | none | no | O8; internal publication primitive is reached by migration APIs, not directly operated in the console. |

### Mutually exclusive totals

The matrix has no current passing joined proof because every candidate spec is
unexecuted at the audited SHA. Its canonical `surface` and `joined` cells yield:

```text
joined=yes                                      0
dashboard partial (joined=partial)              0
route exists but no current joined proof       63
API only                                       19
CLI only                                        1
config only                                     7
                                                --
backend denominator                            90
0 + 0 + 63 + 19 + 1 + 7 = 90
```

No residual category is needed.

### Missing console modes and open questions

Mode-level gaps on rows that otherwise have a dashboard route:

- **Synchronous Algolia overwrite has UI controls but no real joined proof at
  this SHA.** `Migrate.tsx:69-80,125-141` posts `overwrite` to the synchronous
  endpoint; `migrate.spec.ts:97` only toggles the control and P29 is create-only.
- **Asynchronous Algolia `overwrite=true` is absent from the console.**
  `handlers/migration/mod.rs:467-605` owns async submit/status/cancel/acknowledge,
  while `Migrate.tsx:69-80` calls only `/1/migrate-from-algolia`.
- **Migration cancel/status/acknowledge are absent from the console.** The
  production lifecycle is `router.rs:133-157`; `Migrate.tsx` has one synchronous
  mutation and no job ID or cancel action.
- **Runtime HA peer add/remove is absent from the console.**
  `handlers/internal.rs:734-820` owns the mutations, while
  `Cluster.tsx:248-290` only reads and renders status.
- **Dead-node auto-heal lifecycle is absent from the console.**
  `handlers/internal.rs:530-609` exposes autoheal fields, but `Cluster.tsx`
  renders only node/peer-health fields.
- **Atomic bulk-replace job cancellation is absent from both dashboard and
  ingest CLI.** `handlers/migration/bulk_replace.rs:186-217` owns cancellation;
  `ingest_replace.rs:268-303` polls terminal status without issuing cancel.
- **Backend settings absent from the console are API-only, not Settings-route
  surfaces.** There is no operable control for `replicas`,
  `numericAttributesForFiltering`, `disableTypoToleranceOnAttributes`,
  index/query `typoTolerance`, `enableReRanking`, or the closed-world “all
  remaining settings” claim. Search-request-only `optionalFilters` and
  `sumOrFiltersScores` are also absent. The composite rows expose only
  `attributesToHighlight` (not `attributesToSnippet`), `queryType` (not
  `removeWordsIfNoResults`), and `hitsPerPage` (not `paginationLimitedTo`).
  `decompoundQuery` has no dashboard control and remains in the config-only
  decompounding row.

The remaining explicitly non-console backend rows are the matrix's `API only`,
`CLI only`, and `config only` groups: multi-index search; deleteByQuery;
partialUpdateObjects; index copy/move/clear; task status; secured signed keys;
OpenAPI; error-response parity; request-ID propagation; staged publication;
optional filters; sum-of-filters scoring; replica topology; query-level typo
tolerance; numeric filtering attributes; typo-disabled attributes; remaining
Algolia settings; `enableReRanking`; `flapjack ingest`; decompounding
build/runtime configuration; auto-heal enablement; analytics retention; JSON
logging; CORS; shutdown timeout; and startup dependency summary. These are
classified by their current owner seam, not proposals for new UI.

Open questions:

1. Future console scope remains a product decision: this audit does not decide
   which API/CLI/config-only capabilities belong in the planned Svelte console.
2. A full Playwright execution at the audited SHA would be required to promote
   any matrix row to `yes` or `partial`; Stage 4/5 intentionally did not create
   or rerun joined-proof candidates after the SHA changed.

The route-count wording question is closed for Wave 4 by stating each source
fact separately: 24 derived patterns, 24 raw `path=` attributes, two
attribute-less index routes, and 22 lazy page components.

Stage 4 changed no dashboard product code, spec, runner, or
`engine/docs2/FEATURES.md`. Proposed public replacement text remains Stage
5/Wave 4 work.

### Contract probes

Run from repo root:

```text
$ grep -c 'lazy(' engine/dashboard/src/App.tsx
22
$ grep -oE 'path="[^"]+"' engine/dashboard/src/App.tsx | sort -u | wc -l
24
$ grep -c '<Route index' engine/dashboard/src/App.tsx
2
$ ls engine/dashboard/tests/e2e-ui/full/*.spec.ts | wc -l
38
$ ls engine/dashboard/tests/e2e-api/*.spec.ts 2>/dev/null | wc -l
4
$ test -s engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_3_dashboard_join_audit_receipt.md; echo "RECEIPT_EXIT=$?"
RECEIPT_EXIT=0
```

The first three outputs match the separately labeled 22 lazy components, 24
raw path attributes, and 2 index routes above. They are not the 24 derived
user-facing-pattern denominator even though the two current counts happen to
be equal: raw paths include the `/` layout parent and nested segments, while
derived patterns include the two attribute-less index routes.

Mechanical arithmetic falsification from the 90 canonical rows:

```bash
awk -F'|' '/^\| .* \| `FEATURES\.md:[0-9]+`/ {
  s=$4; j=$6
  gsub(/^[[:space:]]+|[[:space:]]+$/, "", s)
  gsub(/^[[:space:]]+|[[:space:]]+$/, "", j)
  rows++
  if (j=="yes") yes++
  else if (j=="partial") partial++
  else if (s ~ /^dashboard route /) dashboard_no++
  else if (s=="API only") api++
  else if (s=="CLI only") cli++
  else if (s=="config only") config++
  else residual++
} END {
  printf "ROWS=%d YES=%d PARTIAL=%d DASHBOARD_NO=%d API=%d CLI=%d CONFIG=%d RESIDUAL=%d SUM=%d\n",
    rows, yes, partial, dashboard_no, api, cli, config, residual,
    yes+partial+dashboard_no+api+cli+config+residual
}' engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_3_dashboard_join_audit_receipt.md
```

Output:

```text
ROWS=90 YES=0 PARTIAL=0 DASHBOARD_NO=63 API=19 CLI=1 CONFIG=7 RESIDUAL=0 SUM=90
```

## Stage 5 Closeout and Wave 4 Handoff

### Closeout preflight

Stage 5 began from a clean shared worktree, so there were no pre-existing
shared-worktree files to distinguish from the receipt edit:

```text
git rev-parse HEAD
a0440a06d963095884b141e53f5136d0c0e6b8f7

date -Iseconds
2026-07-30T04:20:56-04:00

git status --porcelain --untracked-files=all
<no output>
```

The Stage 1-3 evidence was re-read before closeout and remains intentionally
non-green and non-transplanted:

- Stage 1 preserves its clean starting denominator at
  `9850d9dffae64dddd5e7d666eb8118a39cd144fd`, including the complete observed
  spec inventory and runner-owner commands.
- Stage 2 preserves the non-dashboard Algolia runtime-input classification,
  the smallest valid-credential unblock, and the `5 files / 0 fixes` class
  sweep. It does not convert a presence-policy unit test into live vendor proof.
- Stage 3 preserves both independent current-source transcripts, their load
  values and exact denominators, the run-2 stale-artifact exclusion, all nine
  artifact hashes, four residual gap specifications, merge-base provenance,
  and the final statement that the two runs do not establish a passing full
  composition.

### Verbatim replacement for `FEATURES.md:11-13`

Wave 4 should replace the three existing status bullets with exactly:

```markdown
- **Dashboard UI:** `dashboard/src/App.tsx` defines 24 derived user-facing route patterns from 24 raw `path=` attributes and two attribute-less index routes, backed by 22 lazy page components; the wildcard has no lazy component and `Overview` serves two patterns. No scaffolded stubs remain.
- **E2E Browser Tests:** The current inventory is 46 Playwright spec files: 38 full e2e-ui specs, four smoke specs, and four e2e-api specs. The latest full dashboard-composition evidence is not green. At `ddb6fccef82af3e43eedf88778a89f28dd2cbe33`, run 2 of `./s/test --dashboard-full` returned 1 with Vitest 663/663 and both Playwright phases unparseable/did not run after server startup failed; run 3 reached Vitest 663/663, smoke 17/17, and full 357 passed / 1 failed / 8 skipped / 1 did not run, then required an exact-PID interrupt because the HTML reporter did not return. The residuals are invalid Algolia runtime inputs, the inconclusive run-2 startup failure, and pre-existing runner/report evidence gaps; see `4_EVIDENCE/2026_07_30_jul30_12am_3_dashboard_join_audit_receipt.md`.
- **Tour Video Walkthroughs:** 24/24 per-feature specs now have archived MP4 artifacts. The former vector/chat blockers (05/06) were closed on 2026-03-30 with dedicated tour specs plus default-build/vector+AI runtime wiring. Per-feature tours provide end-to-end workflow proof for core dashboard capabilities.
```

### Verbatim replacement for the dashboard claim at `FEATURES.md:328`

Wave 4 should replace the current E2E paragraph with exactly these two
paragraphs:

```markdown
The current inventory is 46 Playwright spec files: 38 full e2e-ui specs, four smoke specs, and four e2e-api specs. `dashboard/src/App.tsx` separately defines 24 derived user-facing route patterns from 24 raw `path=` attributes and two attribute-less index routes, backed by 22 lazy page components. At `ddb6fccef82af3e43eedf88778a89f28dd2cbe33`, run 2 of `./s/test --dashboard-full` returned 1 with Vitest 663/663, smoke UNPARSEABLE/DID NOT RUN, and full UNPARSEABLE/DID NOT RUN at preflight load 23.08/52.28/59.43; run 3 returned execution-tool exit 1 after exact-PID interruption with Vitest 663/663, smoke 17/0/0/0/0/17, and full 357/1/0/8/1/367 at preflight load 25.37/35.87/50.05. The dashboard composition is therefore not currently green: valid Algolia runtime inputs are still required, the run-2 startup failure remains inconclusive, and pre-existing runner/report evidence gaps prevent a naturally returning red-suite transcript.

The dated backend/frontend matrix audits 90 backend capability rows. At the audited SHA, 0 rows have current passing joined proof and 0 have current partial joined proof; 63 rows have an operable dashboard route but no current joined proof, 19 are API-only, one is CLI-only, and seven are config-only. Route existence, e2e-api coverage, unit coverage, and unexecuted Playwright candidates are not counted as joined proof. Composite Settings rows expose only the controls present in the React owner: highlight but not `attributesToSnippet`, `queryType` but not `removeWordsIfNoResults`, and `hitsPerPage` but not `paginationLimitedTo`.
```

The first paragraph's two run sentences are the exact Stage 3 current-source
facts in `Stage 3 Independent Full-Composition Verification` →
`Denominators and Comparison` and `Stage 3 Falsifiable Statement`. The second
paragraph is the corrected arithmetic from `Stage 4 Backend-Frontend Join
Matrix` → `Mutually exclusive totals`; it does not claim that an unexecuted
candidate passed.

### Unfixed routed gaps

Stage 5 made no React, route, control, spec, retry, skip, assertion, runner, or
public-ledger change. The gaps below remain unfixed because this lane's durable
deliverable is the receipt and each gap already has a narrower owner/proof path:

- valid Algolia migration credentials: runtime secret-input owner; rerun the
  focused live migration proof and then both canonical compositions without
  weakening the fail-closed fixture;
- Vite/webserver lifetime and canonical runner startup: runner/webserver owner;
  preserve diagnostics and prove the server lifetime under preceding-suite
  load rather than treating `ECONNREFUSED` or the run-2 stop as a pass;
- HTML report non-return: Playwright config/runner owner; prove a red canonical
  command returns nonzero unattended;
- migration async overwrite/status/cancel/acknowledge UI: migration-console
  owner; the current page invokes only the synchronous mutation;
- atomic bulk-replace cancellation: API/ingest-console owner; the dashboard has
  no job surface and the ingest CLI polls without canceling;
- runtime HA peer add/remove: cluster-console owner; the current page renders
  status only;
- backend-only settings: settings API owner until future-console scope is
  decided, including `optionalFilters`, `sumOrFiltersScores`,
  `attributesToSnippet`, `removeWordsIfNoResults`, `typoTolerance`,
  `paginationLimitedTo`, `numericAttributesForFiltering`,
  `disableTypoToleranceOnAttributes`, `replicas`, `enableReRanking`, remaining
  Algolia settings, and config-only `decompoundQuery`.

Open questions are limited to future product scope: which API/CLI/config-only
capabilities should become operable in the planned Svelte console. That
decision does not change this receipt's current-owner classification or its
zero-current-joined-proof result.

### Closing validation

The validation-cache helper resolved from its required canonical path,
`/Users/stuart/repos/gridl/mike_dev/matt_root/matt/validation_cache.py`. Every
command below was checked at pre-commit
`HEAD=a0440a06d963095884b141e53f5136d0c0e6b8f7`,
`clean_tree=false`; each lookup was a miss, so each command ran live and its
result was recorded under session `stage05-s22-build`.

Receipt existence:

```text
$ test -s engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_3_dashboard_join_audit_receipt.md; echo "RECEIPT_EXIT=$?"
RECEIPT_EXIT=0
```

Dashboard type and lint gates:

```text
$ cd engine/dashboard && npx tsc --noEmit; echo "TSC_EXIT=$?"
TSC_EXIT=0

$ cd engine/dashboard && npm run lint > /tmp/jul30_12am_3_lint_exit.txt 2>&1; echo "LINT_EXIT=$?"; tail -5 /tmp/jul30_12am_3_lint_exit.txt
LINT_EXIT=0

> flapjack-dashboard@1.0.10 lint
> eslint 'src/**/*.{ts,tsx}' --max-warnings 0

$ cd engine/dashboard && npm run lint:e2e > /tmp/jul30_12am_3_lint_e2e_exit.txt 2>&1; echo "LINT_E2E_EXIT=$?"; tail -5 /tmp/jul30_12am_3_lint_e2e_exit.txt
LINT_E2E_EXIT=0

> flapjack-dashboard@1.0.10 lint:e2e
> eslint --config tests/e2e-ui/eslint.config.mjs 'tests/e2e-ui/**/*.spec.ts' 'tests/e2e-ui/**/*.test.ts' 'tests/e2e-ui/helpers.ts' 'tests/e2e-ui/**/*_helpers.ts'
```

Corrected Stage 4 contract probes:

```text
$ grep -c 'lazy(' engine/dashboard/src/App.tsx
22
$ grep -oE 'path="[^"]+"' engine/dashboard/src/App.tsx | sort -u | wc -l
      24
$ grep -c '<Route index' engine/dashboard/src/App.tsx
2
$ ls engine/dashboard/tests/e2e-ui/full/*.spec.ts | wc -l
      38
$ ls engine/dashboard/tests/e2e-api/*.spec.ts 2>/dev/null | wc -l
       4
```

Corrected matrix arithmetic using the exact `awk` command in `Contract probes`:

```text
ROWS=90 YES=0 PARTIAL=0 DASHBOARD_NO=63 API=19 CLI=1 CONFIG=7 RESIDUAL=0 SUM=90
```

The live specimen that makes this guard red is any matrix cell whose `surface`
falls outside the seven closed categories or any added/removed backend row:
`SUM` then differs from `ROWS` or `ROWS` differs from the 90-row denominator.
The settings-owner audit above separately makes the semantic classification
falsifiable by requiring an operable control for every dashboard-surface field.

The semantic absence probe over the two Settings control owners also ran live:

```text
$ if rg -n 'sumOrFiltersScores|numericAttributesForFiltering|disableTypoToleranceOnAttributes|paginationLimitedTo|attributesToSnippet|removeWordsIfNoResults|typoTolerance|decompoundQuery|optionalFilters|replicas|enableReRanking' engine/dashboard/src/components/settings/SettingsForm.tsx engine/dashboard/src/components/settings/SettingsTabContent.tsx; then echo 'UNSUPPORTED_SETTINGS_UI_MATCH=1'; else echo 'UNSUPPORTED_SETTINGS_UI_MATCH=0'; fi
UNSUPPORTED_SETTINGS_UI_MATCH=0
```

Any newly added operable control for one of those fields makes the probe red and
requires the matrix cell and missing-console inventory to be re-audited.

Final pre-commit scoped-ownership gates:

```text
$ git diff --check
<no output>
$ git status --porcelain --untracked-files=all | head -20
 M engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_3_dashboard_join_audit_receipt.md
```

Only the lane-owned receipt is present. No shared-worktree file was cleaned,
reverted, staged, or modified by Stage 5.
