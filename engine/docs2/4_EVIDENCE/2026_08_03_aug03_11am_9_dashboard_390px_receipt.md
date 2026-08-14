# Dashboard 390px Shared-Layout Lane Receipt

Date: 2026-08-03
Lane: `aug03_11am_9_dashboard_390px_shared_layout`
Stage: final lane receipt (Stages 1-4) — diagnosis, UX decision, shared-shell fix, and 390px closing proof
Stage 1 measured product HEAD: `fccd67e79198de987840d4f862d8b8b7929873cc`
Stage 2 fix commit: `2055c9783 Fix dashboard shell containment at 390px`
Stage 3 proof HEAD: `a37c65e86` (audit row closed at `d06131b30`)

This receipt is the single evidence source for the lane. The Stage 1 UX design
artifact below is preserved unchanged; the Stage 2 implementation/spec evidence
and the Stage 3 rendered-browser closing proof are added as new sections. This
lane changes no product code, test oracle, or ledger file at closeout — ledger
application is routed to `L11`.

## Task

read and operate a Flapjack index from a 390px-wide screen

The acceptance boundary is a usable desktop-first console at 390px: global page
chrome must not create document-level horizontal scrolling, navigation and core
index operations must remain reachable, and intrinsically wide content may
scroll inside a bounded local container. This stage does not define a
mobile-optimised product.

## Evidence Provenance And Baseline

The route-audit owners are:

- `engine/dashboard/tests/e2e-ui/route_audit_manifest.ts:20-57`, which enumerates
  24 user-facing patterns and excludes only the fallback shell;
- `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts:21-32`, which owns
  the 390x844 Chromium viewport;
- `engine/dashboard/tests/e2e-ui/stage3_route_audit_390.spec.ts:46-87`, which
  visits the 23 ready routes, delegates width measurement to the fixture, writes
  evidence, and asserts the denominator; and
- `engine/dashboard/tests/fixtures/viewport_overflow.ts:3-11`, the single owner
  of the document-width calculation.

The L4 dispatch precondition was present at measured HEAD: its lane source and
`engine/docs2/4_EVIDENCE/2026_08_03_aug03_11am_4_route_audit_denominator_receipt.md`
exist, while `docs/screen_specs/_audit.md:21` records `pages tested: 23`.

After stale `/tmp/l9_before.*` evidence was cleared, the exact lint and browser
commands completed successfully. The first local attempts exposed two setup
preconditions rather than product-test defects: lockfile dependencies were
absent (`eslint: command not found`), then no server was listening at
`127.0.0.1:7700`. `npm ci` and an isolated `flapjack-server --no-auth` process
restored those prerequisites. The unchanged commands then reported:

```text
LINT_E2E_EXIT=0
AUDIT_BEFORE_EXIT=0
3 passed (15.1s)
```

The contract parser output was:

```text
tested 23 usable 0
```

This is customer-visible browser evidence, not a static CSS inference: a real
Chromium instance rendered every route at 390x844 against a seeded real server.

## Current Flow

`Layout.tsx:15-32` builds one full-height column: `Header`, the conditional
disconnected banner, the dev-mode panel, a flex row containing `Sidebar` and
`main`, then `ApiLogger`. At widths below the `md` breakpoint, the sidebar is
already removed from layout and opens as a fixed overlay
(`Sidebar.tsx:39-55`). The main element currently owns page scrolling with
`flex-1 overflow-auto` (`Layout.tsx:25-29`).

Index pages insert `IndexTabBar` above their route outlet
(`IndexLayout.tsx:11-15`). Its list intentionally has intrinsic max-content
width, but its direct parent owns `overflow-x-auto`
(`IndexTabBar.tsx:30-41`). The global header instead keeps a full branding and
status group beside an uncollapsed action group (`Header.tsx:56-88` and
`Header.tsx:151-183`). The collapsed API logger likewise places the log label
and a route-dependent last-request string beside fixed Export and Clear actions
without a shrinkable flex seam (`ApiLogger.tsx:19-46`).

## Rendered Diagnosis

The existing boolean fixture was temporarily instrumented in place; no
`page.evaluate` was added to the spec. For every route it reported viewport,
document, body, and root widths; the widest unclipped element; its bounds,
client and scroll widths; and its ancestor chain. The instrumented audit passed
all three Playwright projects, emitted 23 diagnostics, and was then restored
byte-for-byte to its original implementation. `npm run lint:e2e` and the
original audit are green for the restored tree through the required validation
cache.

| Sample | Structure | Viewport -> document | Widest unclipped offender | Causal evidence |
|---|---|---:|---|---|
| `/keys` | table/list-heavy | `390 -> 565px` | Header action group, `div.flex.items-center.gap-2`, text `API Docs`; bounds `289.09375..564.65625`, width/client/scroll `275.5625/276/276px` | Its header ancestor had `clientWidth=390`, `scrollWidth=565`, `overflow-x: visible`; the offender's right edge rounds to the document/body/root width. |
| `/index/movies/settings` | form-heavy index route | `390 -> 635px` | API logger action group, `div.flex.gap-2`, text `Export Clear`; bounds `460.75..634.84375`, width/client/scroll `174.09375/174/174px` | Its collapsed logger-row ancestor had `clientWidth=390`, `scrollWidth=635`, `overflow-x: visible`; the offender's right edge rounds to the document/body/root width. |
| `/overview` | card/stat-heavy | `390 -> 797px` | API logger action group, `div.flex.gap-2`, text `Export Clear`; bounds `622.5625..796.65625`, width/client/scroll `174.09375/174/174px` | Its row contained `Last: GET /2/overview?startDate=2026-07-27&endDate=2026-08-03 ...`, had `clientWidth=390`, `scrollWidth=797`, and the offender's right edge rounds to the document/body/root width. |

The settings route also supplies a useful negative control. Its 720px settings
tab group was clipped by a 342px `overflow-x-auto` parent, and its 681.22px
`IndexTabBar` list was clipped by the 342px `index-tab-bar-scroll` owner. Those
elements are wide by design but do not enlarge the document. This matches the
source contract at `IndexTabBar.tsx:31-41` and rules out `IndexLayout` as a
document-overflow owner.

The sampled routes therefore have two shared-shell causes:

1. `Header.tsx:56-88` creates a 565px floor because neither side of its
   `justify-between` row has a narrow-width prioritisation rule.
2. `ApiLogger.tsx:26-45` can exceed that floor according to the last request URL.
   The left group is intrinsic-width content, while Export and Clear remain a
   second intrinsic-width group. `Layout.tsx:31` places this logger at the root,
   outside the main scroller, so its overflow becomes document overflow.

No sampled route proved page-local ownership. The sidebar is already an overlay
at 390px and was closed during the measurements. The healthy-server baseline did
not render the disconnected banner; its unbroken host-bearing message at
`Layout.tsx:18-22` remains an explicit error-state risk to test in Stage 2, but
it is not needed to explain the reproduced healthy-state defect.

### Starting hypothesis disposition

The broad hypothesis that the defect is shared-shell-owned held. The narrower
hypothesis that adding `min-w-0` to the `Layout.tsx` main flex child is the fix
did not: sampled main content was bounded by the existing `overflow-auto`, while
unclipped sibling rows in Header and ApiLogger exactly determined document
width. `min-w-0` remains useful at shrinkable flex seams, but a main-only change
cannot resolve either measured owner.

## Alternatives

| Alternative | Benefits | Costs and evidence-based disposition |
|---|---|---|
| Shared shrink boundaries plus per-container scrolling | Fixes the two root owners once; preserves the existing route model and existing local scrollers; allows wide tables/tabs to remain desktop-shaped. | Requires explicit narrow-width priority in Header and a shrinkable/truncated API-log summary. **Chosen.** `min-w-0` is a supporting rule, not the entire fix. |
| Collapse the sidebar to an overlay drawer below a breakpoint | Maximises content width and preserves the desktop sidebar above `md`. | Already implemented at `Sidebar.tsx:39-55`, so it cannot fix an overflow reproduced with the sidebar closed. Keep the behavior; do not rebuild it. |
| Reflow wide tables and cards into stacked mobile presentations | Could create a polished phone-native experience and avoid local table scrolling. | High per-page cost, duplicates 23 route decisions, changes information hierarchy, and does not fix Header or ApiLogger. Rejected for this desktop-first usability lane. |
| Hide overflow on the document/root | Makes the boolean audit appear green with one CSS rule. | Clips controls and evidence instead of making the flow operable. Rejected because it would make the oracle pass for a real defect. |

## Chosen Stage 2 Flow And Contract

Stage 2 should implement the first alternative as one coherent shared-shell
change, after writing `_component_app_shell.md`:

1. Keep the current sidebar breakpoint and overlay interaction.
2. Make the main flex seam explicitly shrinkable while retaining its existing
   scroll ownership. Do not patch route pages unless the post-shell diagnostic
   proves a remaining page-owned document leak.
3. Give Header a narrow-width priority contract: preserve navigation, compact
   product identity, connection/settings access, and current task status; hide
   or compact low-priority Beta/API-doc/dev/theme affordances below the chosen
   breakpoint. Its two flex groups must fit within 390px rather than overlap and
   extend the document.
4. Make the collapsed API-log left region `min-w-0` and flexible, truncate the
   route-dependent last-request summary, and keep the Export/Clear group
   reachable and non-shrinking. Expanded log content must scroll inside the log
   panel, never the document.
5. Allow intrinsically wide route content to scroll only inside its existing or
   nearest semantic container. The settings and index-tab scrollers are the
   reference behavior.
6. In the disconnected state, wrap or break the banner message within 390px;
   in loading and healthy states, reserve only the narrow Header space defined
   by the component spec.

This is the smallest coherent option because it changes the shared owners that
all routes inherit, leaves already-correct sidebar and tab behavior intact, and
does not invent 23 page-specific mobile layouts.

## Risks, Ownership Gap, And Conditional Disposition

- The lane's exclusive Stage 2 owner list at
  `chats/icg/aug03_11am_9_dashboard_390px_shared_layout.md:137-147` names
  `Layout.tsx`, `Sidebar.tsx`, `IndexLayout.tsx`, and conditional `Header.tsx`,
  but not `ApiLogger.tsx`. Rendered evidence proves `ApiLogger.tsx:26-45` is a
  root owner on settings and overview. Before Stage 2 edits, its dispatch must
  explicitly authorize `engine/dashboard/src/components/layout/ApiLogger.tsx`
  or route that edit to an owner. If authorization is not added, park the
  implementation rather than ship a header-only or clipping workaround that
  leaves the defect live.
- Responsive hiding must not make required actions unreachable. The component
  spec must name which Header actions remain direct at 390px and which are
  deliberately nonessential to the task.
- Text truncation needs a stable accessible name/title so the current request
  remains inspectable without determining layout width.
- The disconnected banner was not present in the healthy baseline. Its targeted
  browser test is mandatory before calling the app-shell state contract closed.

Open implementation question: who owns the newly proven `ApiLogger.tsx` edit in
Stage 2? Diagnosis, UX choice, and the conditional disposition are otherwise
closed.

## Targeted Test Plan

Use the existing 23-route owner rather than creating per-page rewrites:

1. Add fixture-owned, known-answer tests around the width oracle so a deliberate
   overflowing specimen is unusable and a locally-scrolling wide specimen is
   usable. Keep spec bodies free of diagnostic `page.evaluate` calls.
2. Run `npm run lint:e2e`, source lint, TypeScript, and focused component tests
   for Header, Layout/Sidebar, IndexTabBar, and ApiLogger behavior.
3. Add targeted 390x844 interactions for: opening and closing the sidebar;
   reaching retained Header actions; reading/expanding the API logger without
   document overflow; scrolling index tabs locally; and rendering a long-host
   disconnected banner without document overflow.
4. Rerun `stage3_route_audit_390.spec.ts` against a real seeded server. It must
   report exactly `tested 23 usable 23`, with no readiness/setup issue disguised
   as overflow and no page-specific audit copies.

The live failure specimen for the closing proof is current HEAD: Header produces
565px document width on `/keys`, while ApiLogger produces 635px and 797px on the
settings and overview samples. A regression to either behavior makes the audit
red.

## Stage 2 Implementation Evidence

Stage 2 landed the chosen shared-shell containment change as commit
`2055c9783 Fix dashboard shell containment at 390px` (owner authorization for the
app-shell overflow owner at `2eeaf04ac`), and wrote the App Shell component spec
`docs/screen_specs/_component_app_shell.md` before editing. The spec enumerates
the App Shell owners as `Layout`, `Header`, `Sidebar`, `IndexTabBar`, and
`ApiLogger`, and states the narrow-width priority contract: at 390px with the
sidebar closed, Header and ApiLogger controls must fit without widening the
document, the ApiLogger last-request summary truncates before Export or Clear
becomes unreachable, and intrinsically wide route/log content scrolls in its
local semantic container (`IndexTabBar` `overflow-x-auto` is the reference).

The `ApiLogger.tsx` ownership gap flagged in Stage 1 was resolved: the rendered
culprits — `Header.tsx` `div.flex.items-center.gap-2` and `ApiLogger.tsx`
`div.flex.gap-2` / `div.flex.items-center.gap-4` — were all fixed, and the closed
audit row `AUD-SHARED-001` names the fixed owners as `Layout.tsx`, `Header.tsx`,
and `ApiLogger.tsx`.

## Stage 3 Closing Proof

Source of truth: the closed `AUD-SHARED-001` row and summary counts in
`docs/screen_specs/_audit.md`. The rendered-browser after-audit at Stage 2's
merged HEAD `a37c65e86` reports:

```text
tested 23 usable 23
AUDIT_AFTER_EXIT=0
ALL_USABLE_EXIT=0
```

with document-overflow rows `0` and positive-culprit rows `0` across all 23
audited routes; `docs/screen_specs/_audit.md` records `pages tested: 23` and
`pages usable: 23`, and every `AUD-001`..`AUD-023` row now reads `390px usable`.

The proof is falsifiable, not tautological. The negative control reverted only
the working-tree copy of `Header.tsx` to its pre-Stage-2 form, which reintroduced
the `div.flex.items-center.gap-2` overflow (`document 565px > viewport 390px`) and
turned the audit red at the strengthened all-usable assertion:

```text
AUDIT_NEGATIVE_EXIT=1
```

with 23/23 routes unusable. A clean restore returned the audit to `tested 23
usable 23`. This is customer-visible browser evidence at 390x844 against a seeded
real server, measured by the single fixture owner
`engine/dashboard/tests/fixtures/viewport_overflow.ts`.

## Competitor And Scope Boundary

`engine/docs2/FEATURES.md:471,491` records PR-13 as deliberately low priority and
desktop-first, including the project comparison that Algolia's dashboard is
desktop-first. This lane therefore proves usability at 390px; it does not
mandate stacked mobile presentations, mobile navigation redesign, or feature
parity with a phone-native product.

Stage 1 changed no component, screen spec, audit ledger, product ledger, route
page, test spec, or fixture at closeout. The diagnostic fixture change was
temporary and restored before this receipt was written.

### Proposed ledger text for L11

`L11`, not this lane, owns ledger application. The following is the final proposed
replacement text; apply it verbatim, citing this receipt by filename.

**`engine/docs2/FEATURES.md`, `PR-13` status row (currently line 471).** The
existing row records the stale pre-fix baseline (`22` routes tested, `0` usable,
owners `Layout.tsx`, `Sidebar.tsx`, `IndexLayout.tsx`, exit `22/22`). Replace the
status cell and body with:

> 🟢 Shared 390px overflow closed (2026-08-03) — Admin dashboard responsive
> design for tablets/phones. **Measured and fixed: 23 authenticated routes tested
> at a 390px viewport, 23 usable** with no document-level horizontal overflow. The
> shared-shell defect tracked as `AUD-SHARED-001` in
> [`docs/screen_specs/_audit.md`](../../docs/screen_specs/_audit.md) is now
> **Closed**; the real owners were `Layout.tsx`, `Header.tsx`, and `ApiLogger.tsx`
> (not `Sidebar.tsx`/`IndexLayout.tsx`, which were ruled out by rendered
> evidence). Fixed by `2055c9783 Fix dashboard shell containment at 390px`. Exit
> met: the route audit records `tested 23 usable 23` with no horizontal overflow
> (`STAGE3_ROUTE_AUDIT_OUTPUT=/tmp/l9_after.json npm run test:e2e-ui -- tests/e2e-ui/stage3_route_audit_390.spec.ts`;
> `AUDIT_AFTER_EXIT=0`, `ALL_USABLE_EXIT=0`), and a negative control confirms the
> proof is falsifiable (`AUDIT_NEGATIVE_EXIT=1`, 23/23 unusable, before restore).
> `/experiments/:experimentId` is inside the 23-route denominator (`AUD-EXP-001`
> closed). This lane proves **usability at 390px only**: PR-13 remains deliberately
> low priority and desktop-first — an admin console is a desktop tool and Algolia's
> own dashboard is desktop-first, so this is not phone-native mobile optimisation.
> Do not spend a further lane here before `SEC-G3` and `JOIN-1`.

**Affected `ROADMAP.md` cross-reference (currently line 268, inside the `JOIN-1`
row).** That row cites `AUD-SHARED-001 22/22 routes overflow at 390px → PR-13`.
`L11` may update that parenthetical to `AUD-SHARED-001 closed: 23/23 routes usable
at 390px → PR-13` for consistency; no other `ROADMAP.md` row is affected. `L11`
owns whether to touch that narrative row.

This text is routed through `L11`, the batch's single ledger writer. This receipt
does not use the reserved ledger-correction sentinel and leaves all ledger files
(`engine/docs2/FEATURES.md`, `ROADMAP.md`, `PROJECT_OVERVIEW.md`, `CHANGELOG.md`)
untouched at closeout.
