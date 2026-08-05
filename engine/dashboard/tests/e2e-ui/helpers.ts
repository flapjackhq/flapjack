/**
 */
/**
 * Shared helpers for e2e-ui tests.
 * NO MOCKS. These tests run against a real Flapjack server.
 */
import { expect, type Locator, type Page } from '@playwright/test';
import { API_BASE, API_HEADERS } from '../fixtures/local-instance';

// Backend connection
export { API_BASE, API_HEADERS };

// Test index — seeded in seed.setup.ts, cleaned in cleanup.setup.ts
export { TEST_INDEX } from '../fixtures/test-data';

// Re-export auth fixture so test files only need one import
export { test, expect } from '../fixtures/auth.fixture';

// Chart primitives carry these ids in the dashboard source, so specs can assert
// that Recharts drew a real surface and real marks without a raw `svg` locator.
// `src/lib/constants.ts` stays the single owner of the values.
export { CHART_CANVAS_TEST_ID, CHART_MARK_TEST_ID } from '../../src/lib/constants';

/**
 * Body rows inside `container` — the rows holding data cells, with header rows
 * excluded. `container` may be the page, a table, or any wrapper around one.
 *
 * Role-based, so table specs never need a raw `tbody tr` locator and never depend
 * on whether a table renders a `<thead>`.
 */
export function getTableBodyRows(container: Page | Locator): Locator {
  const page = 'page' in container ? container.page() : container;
  return container.getByRole('row').filter({ has: page.getByRole('cell') });
}

type SettingsTabAssertion = {
  tabLabel: string;
  panelAssertion: (panel: Locator) => Locator;
};

// Shared settings tab contract for smoke/full navigation assertions
export const SETTINGS_TAB_ASSERTIONS = [
  {
    tabLabel: 'Search',
    panelAssertion: (panel: Locator) => panel.getByText('Searchable Attributes'),
  },
  {
    tabLabel: 'Ranking',
    panelAssertion: (panel: Locator) => panel.getByText('Custom Ranking', { exact: true }),
  },
  {
    tabLabel: 'Language & Text',
    panelAssertion: (panel: Locator) => panel.getByTestId('query-languages-select'),
  },
  {
    tabLabel: 'Facets & Filters',
    panelAssertion: (panel: Locator) => panel.getByText('Attributes For Faceting'),
  },
  {
    tabLabel: 'Display',
    panelAssertion: (panel: Locator) => panel.getByText('Attributes To Retrieve'),
  },
  {
    tabLabel: 'Vector / AI',
    panelAssertion: (panel: Locator) => panel.getByLabel('AI Base URL'),
  },
] satisfies ReadonlyArray<SettingsTabAssertion>;

export function getOverviewIndexRow(page: Page, indexName: string): Locator {
  return page.getByTestId(`overview-index-row-${indexName}`);
}

export function getSidebar(page: Page): Locator {
  return page.getByRole('complementary');
}

export async function gotoOverviewPage(page: Page): Promise<void> {
  await page.goto('/overview');
  await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({ timeout: 10_000 });
}

export async function waitForSearchResultsOrEmptyState(
  page: Page,
  opts?: { requireResults?: boolean },
): Promise<void> {
  if (opts?.requireResults) {
    await expect(page.getByTestId('results-panel')).toBeVisible({ timeout: 15_000 });
    return;
  }
  // Valid dual-state: seeded index may have results or may be empty
  await expect(
    page.getByTestId('results-panel').or(page.getByText(/no results found/i)),
  ).toBeVisible({ timeout: 15_000 });
}

export async function gotoIndexPage(page: Page, indexName: string): Promise<void> {
  await page.goto(`/index/${encodeURIComponent(indexName)}`);
  await waitForSearchResultsOrEmptyState(page);
}

export async function waitForOverviewIndexRow(page: Page, indexName: string): Promise<Locator> {
  const row = getOverviewIndexRow(page, indexName);

  // Other full-suite workers create temporary indexes too, so the target row may
  // be pushed onto a later overview page before it becomes visible.
  for (let pageTurns = 0; pageTurns < 20; pageTurns++) {
    if ((await row.count()) > 0 && await row.first().isVisible()) {
      break;
    }

    const nextButton = page.getByRole('button', { name: /next/i });
    if ((await nextButton.count()) === 0 || await nextButton.isDisabled()) {
      break;
    }

    await nextButton.click();
    await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 5_000 });
  }

  await expect(row).toBeVisible({ timeout: 10_000 });
  await expect(row.getByRole('heading', { name: indexName })).toBeVisible({ timeout: 10_000 });
  return row;
}
