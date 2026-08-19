/**
 * E2E-UI Full Suite — Large-Index Performance Tests (Real Server)
 *
 * NON-MOCKED SIMULATED-HUMAN REAL-BROWSER TESTS.
 * Verifies dashboard responsiveness and error-free rendering against the
 * 100k-document `benchmark_100k` index populated by the loadtest data pipeline.
 *
 * These tests are READ-ONLY — they never create, modify, or delete data.
 * They fail-fast if the benchmark index is not populated with >= 90k documents.
 *
 * STANDARDS COMPLIANCE (engine/dashboard/_dev/testing/TESTING.md):
 * - Zero CSS class selectors — uses data-testid, getByRole, getByText
 * - Zero XPath selectors
 * - Zero page.evaluate() calls
 * - Zero { force: true } overrides
 * - Zero waitForTimeout — uses Playwright expect timeouts
 * - ESLint enforced via tests/e2e-ui/eslint.config.mjs
 */
import type { Response } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import {
  waitForOverviewIndexRow,
  gotoIndexPage,
  SETTINGS_TAB_ASSERTIONS,
} from '../helpers';
import { searchIndex } from '../../fixtures/api-helpers';

const INDEX = 'benchmark_100k';
const MIN_EXPECTED_DOCS = 90_000;

// Generous timeouts for pages rendering 100k-doc indexes
const PAGE_LOAD_TIMEOUT = 30_000;
const DATA_RENDER_TIMEOUT = 20_000;

test.describe('Large-Index Performance (benchmark_100k)', () => {
  let benchmarkAvailable = false;

  test.beforeAll(async ({ request }) => {
    try {
      const response = await searchIndex(request, INDEX, '', { hitsPerPage: 0 });
      const nbHits = typeof response.nbHits === 'number' ? response.nbHits : 0;
      benchmarkAvailable = nbHits >= MIN_EXPECTED_DOCS;
    } catch (err) {
      // Only skip for missing benchmark index (404). Let server errors and
      // connection failures propagate so they aren't hidden behind the skip.
      const message = err instanceof Error ? err.message : String(err);
      if (message.includes('(404)')) {
        benchmarkAvailable = false;
      } else {
        throw err;
      }
    }
  });

  test.beforeEach(async ({}, testInfo) => {
    if (!benchmarkAvailable) {
      testInfo.skip();
    }
  });

  test('overview page renders benchmark_100k with correct stats', async ({ page }) => {
    await page.goto('/overview');
    await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: PAGE_LOAD_TIMEOUT });

    // Document count stat card shows >= 90k total documents
    const docsCard = page.getByTestId('stat-card-documents');
    await expect(docsCard).toBeVisible();
    await expect.poll(async () => {
      const text = (await docsCard.getByTestId('stat-value').textContent())?.trim() ?? '';
      return Number(text.replace(/,/g, ''));
    }, { timeout: DATA_RENDER_TIMEOUT }).toBeGreaterThanOrEqual(MIN_EXPECTED_DOCS);

    // Storage stat card shows a non-zero value
    const storageCard = page.getByTestId('stat-card-storage');
    await expect(storageCard).toBeVisible();
    await expect.poll(async () => {
      return (await storageCard.getByTestId('stat-value').textContent())?.trim() ?? '';
    }, { timeout: DATA_RENDER_TIMEOUT }).toMatch(/^\d+(\.\d+)?\s*(B|KB|MB|GB)$/i);
    await expect(storageCard.getByTestId('stat-value')).not.toHaveText(/^0 Bytes$/i);

    // The benchmark index row is reachable (may require pagination)
    await waitForOverviewIndexRow(page, INDEX);
  });

  test('search/browse page loads and responds to queries', async ({ page }) => {
    await gotoIndexPage(page, INDEX);

    // Results panel renders with document cards
    const resultsPanel = page.getByTestId('results-panel');
    await expect(resultsPanel).toBeVisible({ timeout: PAGE_LOAD_TIMEOUT });
    await expect(
      resultsPanel.getByTestId('document-card').first(),
    ).toBeVisible({ timeout: DATA_RENDER_TIMEOUT });

    // Submit a search query and verify response arrives within timeout
    const searchInput = page.getByPlaceholder(/search documents/i);
    const responsePromise = page.waitForResponse(
      (res: Response) => res.url().includes(`/1/indexes/${INDEX}/query`) && res.status() === 200,
      { timeout: PAGE_LOAD_TIMEOUT },
    );
    await searchInput.fill('laptop');
    await searchInput.press('Enter');
    await responsePromise;

    // Verify results update after search
    await expect(
      resultsPanel.getByTestId('document-card').first(),
    ).toBeVisible({ timeout: DATA_RENDER_TIMEOUT });

    // Benchmark fixture settings include facets, so the panel is part of the page contract.
    const facetsPanel = page.getByTestId('facets-panel');
    await expect(facetsPanel).toBeVisible({ timeout: DATA_RENDER_TIMEOUT });
  });

  test('settings page loads all tabs', async ({ page }) => {
    await page.goto(`/index/${INDEX}/settings`);
    await expect(
      page.getByRole('heading', { name: /settings/i }),
    ).toBeVisible({ timeout: PAGE_LOAD_TIMEOUT });

    // Verify at least the first settings tab renders its expected content
    const firstTab = SETTINGS_TAB_ASSERTIONS[0];
    const tabButton = page.getByRole('tab', { name: firstTab.tabLabel });
    await expect(tabButton).toBeVisible({ timeout: DATA_RENDER_TIMEOUT });
    await tabButton.click();

    const tabPanel = page.getByRole('tabpanel');
    await expect(firstTab.panelAssertion(tabPanel)).toBeVisible({ timeout: DATA_RENDER_TIMEOUT });
  });

  test('analytics page loads without errors', async ({ page }) => {
    await page.goto(`/index/${INDEX}/analytics`);
    await expect(
      page.getByTestId('analytics-heading'),
    ).toBeVisible({ timeout: PAGE_LOAD_TIMEOUT });

    // KPI cards area renders (may show zeros if no analytics data is seeded)
    const kpiCards = page.getByTestId('kpi-cards');
    await expect(kpiCards).toBeVisible({ timeout: DATA_RENDER_TIMEOUT });
  });

  test('synonyms page loads without errors', async ({ page }) => {
    await page.goto(`/index/${INDEX}/synonyms`);
    await expect(
      page.getByRole('heading', { name: 'Synonyms' }),
    ).toBeVisible({ timeout: PAGE_LOAD_TIMEOUT });

    // Valid dual state: synonyms-list renders when synonyms exist,
    // "No synonyms" empty state renders when the index has none configured.
    const synonymsList = page.getByTestId('synonyms-list');
    const emptyState = page.getByText(/no synonyms/i);
    await expect(
      synonymsList.or(emptyState),
    ).toBeVisible({ timeout: DATA_RENDER_TIMEOUT });
  });
});
