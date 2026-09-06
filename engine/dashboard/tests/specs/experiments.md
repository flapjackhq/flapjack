# Experiments — Shipping React Dashboard Behavior

Routes: `/experiments` and `/experiments/:experimentId`

This is the current React dashboard behavior owner. It does not describe the future
`engine/console` Svelte migration.

This spec certifies Full/OSS dashboard conformance only. The React dashboard is disabled in managed
PBV4, so no React assertion implies PBV4 exposure. The managed PBV4 engine/API rejects
`promoted: true`; current Full-profile React promotion behavior remains unchanged.

## experiments-1: List and create

1. Open `/experiments` and see the page-specific heading and existing experiments.
2. See exact name, index, status, primary metric, traffic split, started date, and allowed actions.
3. Open `Create Experiment`, complete the four visible steps, review the exact configuration, and
   create it.
4. See the created experiment in the list and clean it up through the maintained fixture.

Loading, empty, and server-error states remain page-specific. A running experiment cannot be
deleted. Stop and delete require the shipped confirmation dialog.

## experiments-2: Real lifecycle

1. Launch one deterministically seeded experiment through the visible create wizard.
2. Collect controlled stable-userToken results through the API fixture owner, not the browser.
3. See exact arm, gate, significance, quality, and recommendation values in detail.
4. Stop, declare an eligible winner with a visible reason, and see the stored conclusion under the
   current Full/OSS presentation.
5. Delete the terminal experiment and verify it leaves the list.

Exact statistics and gate readiness belong to engine/API known-answer tests. Browser setup must not
exploit NaN-to-zero behavior, probe hundreds of searches to discover arms, call a route API instead
of the claimed UI action, or use sleeps/retries to force readiness.

## experiments-3: Safety and responsive conformance

- Invalid transitions, missing variants, unsafe promotion, and profile/ACL exclusions fail before
  effects in domain/API owners and render safe UI errors.
- The real lifecycle runs once at the existing desktop viewport, then the same journey narrows to
  390px and proves list controls, cards, and dialogs remain visible and operable without repeating
  lifecycle traffic.
- Act and Assert use visible roles/labels/content. Arrange may use maintained fixtures. No raw
  selectors, DOM evaluation, forced actions, or arbitrary sleeps.

## Automated owners and PBV4 dependency

- Components: `src/pages/Experiments.test.tsx`, `src/pages/ExperimentDetail.test.tsx`,
  `src/components/experiments/__tests__/CreateExperimentDialog.test.tsx`, and the experiment view
  model/normalization tests.
- Browser: `tests/e2e-ui/full/experiments.spec.ts` is one visible Full/OSS lifecycle that finishes
  with the thin 390px proof. Its bounded stable-userToken batch fixture performs no NaN shortcut,
  sequential arm-discovery loop, sleep, or API substitution for a claimed UI action/assertion.
- Exact minimum-N and lifecycle values remain owned by deterministic engine/API known-answer tests.
  The managed profile still keeps the dashboard disabled.
