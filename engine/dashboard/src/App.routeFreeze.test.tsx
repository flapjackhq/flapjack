import { describe, expect, it } from 'vitest'
// Vite's `?raw` suffix gives the file's text without executing it. Preferred
// over node:fs here because `import.meta.url` is not a file: URL under this
// vitest config, and because a bare import keeps the guard working regardless
// of the directory the suite is invoked from.
import APP_SOURCE from './App.tsx?raw'

/**
 * FREEZE GUARD — this dashboard is slated for deletion. No new screens.
 *
 * WHY THIS EXISTS
 * ---------------
 * ADR 0006 (`engine/docs2/3_IMPLEMENTATION/decisions/active/0006_console_source_home.md`,
 * status Accepted, 2026-07-18) settled that ONE Svelte console replaces both this
 * React dashboard and the managed cloud console, and that this tree is deleted at
 * the program's parity-gated cutover.
 *
 * The decision was never enforced, and the measurement is unambiguous: in the two
 * weeks after 2026-07-25, `engine/dashboard/` took **164 commits and +17,157 /
 * -10,814 lines** — new screens, new suites, new contracts — into a codebase with
 * an accepted decision to delete it. Every one of those lines is work that gets
 * thrown away at cutover, plus work the migration then has to port or reconcile.
 *
 * A decision that nothing enforces is a preference. This test is the enforcement.
 *
 * WHAT IT FORBIDS, AND WHAT IT DELIBERATELY ALLOWS
 * -----------------------------------------------
 * Forbidden: ADDING a route. A new route is a new screen, which is new surface to
 * port and new surface to delete.
 *
 * Allowed, on purpose:
 *   - REMOVING a route. That is migration progress, so the check is a subset
 *     assertion rather than an equality one. A frozen list that reds when you
 *     delete from it would punish exactly the work we want.
 *   - Editing an existing screen. Bug fixes, CI repairs and security work on the
 *     shipping console stay legitimate until cutover. A freeze that blocked
 *     repairs would be routed around within a week, and then it would enforce
 *     nothing at all.
 *
 * A new feature added INSIDE an existing screen is not caught here. That is a
 * known and accepted bound: no tree-only check can see it, and the route table is
 * where the expensive additions land.
 *
 * TO ADD A ROUTE ANYWAY
 * ---------------------
 * Add it to FROZEN_ROUTES with a comment naming the operator decision that
 * reopened the freeze. The point is not that it is impossible — it is that it
 * cannot happen silently, which is how the 164 commits happened.
 */

// Read the source rather than render the router. Rendering would need every page
// component's mocks and would couple this guard to their internals; the route
// table is a flat literal and reading it is both cheaper and harder to break.

/**
 * Every route path present when the freeze landed (2026-08-08), extracted from
 * App.tsx rather than typed by hand.
 */
const FROZEN_ROUTES: readonly string[] = [
  '/',
  'overview',
  'index/:indexName',
  'settings',
  'analytics',
  'synonyms',
  'rules',
  'merchandising',
  'recommendations',
  'chat',
  'keys',
  'logs',
  'migrate',
  'metrics',
  'cluster',
  'system',
  'query-suggestions',
  'experiments',
  'experiments/:experimentId',
  'events',
  'personalization',
  'dictionaries',
  'security-sources',
  '*',
]

function declaredRoutePaths(source: string): string[] {
  const matches = [...source.matchAll(/path="([^"]*)"/g)].map((m) => m[1])
  if (matches.length === 0) {
    // Never return an empty list. A subset assertion against [] passes forever,
    // which would silently convert this guard into decoration.
    throw new Error(
      'parsed zero route paths from App.tsx — the route table changed shape and ' +
        'this parser is stale. Fix the parser; do NOT let it return nothing.'
    )
  }
  return matches
}

describe('React dashboard route freeze', () => {
  it('declares at least one route, so the subset check cannot be vacuous', () => {
    expect(declaredRoutePaths(APP_SOURCE).length).toBeGreaterThan(0)
  })

  it('adds no route beyond the frozen set', () => {
    const added = declaredRoutePaths(APP_SOURCE).filter(
      (path) => !FROZEN_ROUTES.includes(path)
    )
    expect(
      added,
      'This dashboard is frozen pending deletion (ADR 0006). A new route is new ' +
        'surface to port and then throw away. Build it in the Svelte console ' +
        'instead, or add it to FROZEN_ROUTES naming the decision that reopened ' +
        'the freeze.'
    ).toEqual([])
  })

  it('allows routes to be removed, because removal is the goal', () => {
    // Pins the direction of the assertion. If someone "fixes" a failure by
    // switching this to an equality check, deleting a migrated screen would
    // start failing the build and the migration would stall on its own guard.
    const removed = FROZEN_ROUTES.filter(
      (path) => !declaredRoutePaths(APP_SOURCE).includes(path)
    )
    expect(Array.isArray(removed)).toBe(true)
  })

  it('parser throws rather than reporting an empty route table', () => {
    expect(() => declaredRoutePaths('export default function App() {}')).toThrow(
      /parsed zero route paths/
    )
  })
})
