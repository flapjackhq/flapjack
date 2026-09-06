# Recommendations — Shipping React Dashboard Behavior

Route: `/index/:indexName/recommendations`

This is the current React dashboard behavior owner. It does not describe the future
`engine/console` Svelte migration. It certifies Full/OSS conformance only: the React dashboard is
disabled in managed PBV4, and no assertion here implies PBV4 dashboard exposure.

## recommendations-1: Closed model set and configuration

1. Open the route for a seeded index and see the page-specific `Recommendations` heading and exact
   index name.
2. See exactly `related-products`, `bought-together`, `trending-items`, `trending-facets`, and
   `looking-similar` in the model picker, with `related-products` selected initially.
3. See `objectID` only for models that require it; see required `facetName` and optional
   `facetValue` only for `trending-facets`; see no model-specific field for `trending-items`.
4. Invalid required values keep the visible submit action disabled.

## recommendations-2: Preview results and stale safety

1. Submit one seeded valid configuration through visible controls.
2. See fixture-exact page-unique result content; trending-facet hits render as
   `facetName: facetValue`, not raw JSON.
3. See a single aggregate empty state for a successful response with no hits and a safe visible
   alert for a failed response.
4. Change model or index and see prior results clear; an older in-flight result cannot overwrite
   the newer selection.

All-five-model known answers from owned index data and, where applicable, owned events belong to the real engine/handler KAT. The browser retains one thin
host-wiring journey and does not multiply five models across hosts and viewports.

## recommendations-3: Responsive conformance

- At 390px, configuration fields and submit remain visible in one column; long result content stays
  inside its card with no page-wide horizontal overflow.
- The real preview runs once at the existing desktop viewport, then the same journey narrows to
  390px and proves responsive reachability without replaying the preview.
- Act and Assert use visible roles/labels/content. Arrange may use maintained fixtures. No raw
  selectors, DOM evaluation, forced actions, or arbitrary sleeps.

## Automated owners

- Components/contracts: `src/pages/Recommendations.test.tsx`,
  `src/hooks/useRecommendations.test.tsx`, and `src/lib/recommendation-contract.ts`.
- Browser: `tests/e2e-ui/full/recommendations.spec.ts`.
- Engine/handler tests own exact five-model results, validation, profile/ACL exclusion, non-vector
  `looking-similar`, and cleanup. The fallback is not semantic/image-similarity parity.
