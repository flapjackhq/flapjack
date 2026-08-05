# Browser-Test Standard Conformance Receipt

PURPOSE: Record the measured browser-test lint delta and the contracts landed to
close it, so downstream dashboard lanes can use the conformance work without
re-deriving it.

Out of scope: re-running browser suites, re-measuring the Stage 1 corpus,
changing product or test code, changing lint configuration, renaming browser
specs, editing generated guidance, and changing project ledgers.

## Evidence Basis

This receipt transcribes the dry-run measurement from
`engine/docs2/4_EVIDENCE/2026_08_02_aug02_11am_5_gate_delta_dryrun.md` and the
landed contracts recorded in the Stage 2 checklist and its final review handoff.
No browser suite or replacement corpus sweep was run for this receipt.

## Dry-Run Denominator

The Stage 1 source values are unchanged:

```text
repo specs inventoried: 51
gate files swept: 49
hits: 234
fixable in-spec-or-helper: 155
needs-src-change: 27
waived: 52
```

Here, `waived: 52` is the Stage 1 triage category for table-structure selectors
that could have been proposed for an expanded allow-list. Stage 2 did not widen
the allow-list; it repaired those selectors with role-based or scoped permitted
locators instead.

The measured per-rule denominator was:

| Rule | Hits |
| --- | ---: |
| `playwright/no-raw-locators` | 115 |
| `playwright/no-conditional-in-test` | 44 |
| `playwright/no-useless-not` | 27 |
| `playwright/no-conditional-expect` | 18 |
| `playwright/expect-expect` | 7 |
| `no-restricted-syntax` | 6 |
| `playwright/prefer-web-first-assertions` | 5 |
| `playwright/missing-playwright-await` | 4 |
| `playwright/no-standalone-expect` | 4 |
| `playwright/consistent-spacing-between-blocks` | 2 |
| `playwright/no-skipped-test` | 1 |
| `playwright/no-useless-await` | 1 |

## Landed Browser Lint Contract

`engine/dashboard/tests/e2e-ui/eslint.config.mjs` now spreads
`playwright.configs["flat/recommended"]` and retains
`playwright.configs["flat/recommended"].rules`. It adds
`playwright/no-raw-locators` with `allowed: ["aside", "tr", "main", "option"]`.
The custom `no-restricted-syntax` block retains only restrictions not duplicated
by that rule: request member access, `evaluate`, `waitForTimeout`,
`dispatchEvent`, and `setExtraHTTPHeaders`. The config no longer cites
`BROWSER_TESTING_STANDARDS_2.md`. The `lint:e2e` script in
`engine/dashboard/package.json` enforces the gate with `--max-warnings 0` across
specs, focused helper tests, and shared browser-test helpers.

Stage 2 closure evidence records passing `npm run lint:e2e`, `npm run lint`,
`npm run check:tests`, `npx tsc --noEmit`, the full dashboard Vitest suite, and
the config conformance guard at the reviewed Stage 2 HEAD.

## Naming Resolution and Generated Guidance

Per `engine/dashboard/tests/README.md`, this repository keeps
`engine/dashboard/tests/e2e-ui/{smoke,full}` as its repo-local equivalent of the
browser-unmocked layout. Mocked browser specs are identified by the
`*-mocked.spec.ts` suffix. The strict browser-test lint and authoring rules apply
regardless of directory naming, and existing files are not to be renamed for
this resolution.

`engine/CLAUDE.md` is generated and still carries the naming contradiction from
its scrai source. That canonical source requires later repair; the generated
file must not be hand-edited in this lane.

## Consumer Contract

L6 must use the tightened browser-test authoring gate and may write
`docs/screen_specs/_audit.md`; L8 must write `docs/screen_specs/login.md` before
presentation code; and L10 must write `docs/screen_specs/cluster.md` before
presentation code. All three lanes must start from
`docs/screen_specs/_template.md` and follow the naming resolution in
`engine/dashboard/tests/README.md`.

## Ledger Disposition

No `ROADMAP.md` row corresponds to this lane. Therefore no
`ROADMAP CORRECTION REQUIRED` block is owed.
