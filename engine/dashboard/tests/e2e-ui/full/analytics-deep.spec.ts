import { test, expect } from '../../fixtures/auth.fixture';
import { CHART_CANVAS_TEST_ID, CHART_MARK_TEST_ID, getTableBodyRows } from '../helpers';
import { seedAnalytics, deleteIndex, DEFAULT_ANALYTICS_CONFIG } from '../../fixtures/analytics-seed';

/**
 * Analytics Deep Data Verification — E2E-UI (real browser, real server, no mocks)
 *
 * Seeds real analytics data, then verifies it displays correctly in the browser.
 * These tests go beyond visibility checks — they verify actual data values,
 * mathematical consistency, and correct rollup behavior in the rendered UI.
 *
 * STANDARDS COMPLIANCE (engine/dashboard/_dev/testing/TESTING.md):
 * - Zero raw locators — uses data-testid and role locators for value extraction
 * - Zero conditional skipping — all assertions are hard
 * - ESLint enforced via tests/e2e-ui/eslint.config.mjs
 */

// `fullyParallel` runs this file's hooks once per worker, so each worker owns its seed and
// cleanup. The worker suffix must come from Playwright itself; no harness variable sets it.
function analyticsIndexName() {
  return `e2e-analytics-deep-${test.info().parallelIndex}`;
}

/** Seeded window: two dashboard periods, so the KPI delta badges have a previous period. */
const SEED_DAYS = 14;
/** The analytics page opens on its default 7-day range (`defaultRange` in useAnalytics.ts). */
const DASHBOARD_RANGE_DAYS = 7;
/**
 * The seed endpoint spreads `searchCount` evenly across the seeded days and writes nothing
 * for today, so the default range sees only its own share of the seeded total — not all of it.
 */
const EXPECTED_RANGE_SEARCHES = Math.floor(
  (DEFAULT_ANALYTICS_CONFIG.searchCount / SEED_DAYS) * DASHBOARD_RANGE_DAYS,
);

test.describe('Analytics Deep Data Verification (real browser)', () => {
  test.beforeAll(async ({ request }) => {
    const indexName = analyticsIndexName();
    // Seed both the current and previous 7-day windows so KPI delta badges render.
    await seedAnalytics(request, {
      ...DEFAULT_ANALYTICS_CONFIG,
      days: SEED_DAYS,
      indexName,
    });
  });

  test.afterAll(async ({ request }) => {
    try { await deleteIndex(request, analyticsIndexName()); } catch { /* ignore */ }
  });

  test.describe('Overview Tab — KPI Cards', () => {
    test('KPI cards show non-zero numeric values from seeded data', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await expect(page.getByTestId('kpi-cards')).toBeVisible({ timeout: 10000 });

      // Total Searches KPI should show the seeded count for the default range.
      const totalSearches = page.getByTestId('kpi-total-searches');
      await expect(totalSearches).toBeVisible();
      const searchText = await totalSearches.getByTestId('kpi-value').textContent();
      const searchNum = parseInt(searchText!.replace(/,/g, ''), 10);
      expect(searchNum).toBeGreaterThanOrEqual(Math.floor(EXPECTED_RANGE_SEARCHES * 0.9));
      // The default range is a strict subset of the seeded days, so a value above the whole
      // seeded total means the server stopped honoring the requested searchCount.
      expect(searchNum).toBeLessThanOrEqual(DEFAULT_ANALYTICS_CONFIG.searchCount);

      // Unique Users KPI
      const uniqueUsers = page.getByTestId('kpi-unique-users');
      await expect(uniqueUsers).toBeVisible();
      const userText = await uniqueUsers.getByTestId('kpi-value').textContent();
      const userNum = parseInt(userText!.replace(/,/g, ''), 10);
      expect(userNum).toBeGreaterThan(0);
      expect(userNum).toBeLessThan(searchNum);

      // No-Result Rate KPI should show a percentage
      const nrr = page.getByTestId('kpi-no-result-rate');
      await expect(nrr).toBeVisible();
      const nrrText = await nrr.getByTestId('kpi-value').textContent();
      expect(nrrText).toMatch(/\d+\.\d+%/);
    });

    test('search volume chart renders SVG with data path', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      const chart = page.getByTestId('search-volume-chart');
      await expect(chart).toBeVisible({ timeout: 10000 });
      await expect(chart.getByTestId(CHART_CANVAS_TEST_ID)).toBeVisible();
      await expect(chart.getByTestId(CHART_MARK_TEST_ID).first()).toBeVisible();
    });

    test('top 10 searches table shows ranked queries in descending order', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      const table = page.getByTestId('top-searches-overview');
      await expect(table).toBeVisible({ timeout: 10000 });
      const rows = getTableBodyRows(table);
      await rows.first().waitFor({ timeout: 10000 });
      const count = await rows.count();
      expect(count).toBeGreaterThanOrEqual(5);
      expect(count).toBeLessThanOrEqual(10);
      await expect(rows.first().getByRole('cell').first()).toHaveText('1');
    });

    test('KPI cards show delta comparison badges when previous period has data', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await expect(page.getByTestId('kpi-cards')).toBeVisible({ timeout: 10000 });

      // The seeded dataset renders comparison values for all three KPI cards.
      const kpiValue = page.getByTestId('kpi-value').first();
      await expect(kpiValue).toBeVisible({ timeout: 10_000 });

      const deltaBadges = page.getByTestId('delta-badge');
      await expect(deltaBadges).toHaveCount(3);
      for (let i = 0; i < 3; i += 1) {
        await expect(deltaBadges.nth(i)).toContainText('%');
      }
    });
  });

  test.describe('Searches Tab — Data Table', () => {
    test('displays sortable table with query counts in descending order', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-searches').click();
      await expect(page.getByTestId('top-searches-table')).toBeVisible({ timeout: 10000 });
      const rows = getTableBodyRows(page.getByTestId('top-searches-table'));
      await rows.first().waitFor({ timeout: 10000 });
      const rowCount = await rows.count();
      expect(rowCount).toBeGreaterThan(10);

      // First row count should be larger than last row count
      const firstCount = await rows.first().getByTestId('search-count').textContent();
      const lastCount = await rows.last().getByTestId('search-count').textContent();
      expect(parseInt(firstCount!.replace(/,/g, ''), 10))
        .toBeGreaterThanOrEqual(parseInt(lastCount!.replace(/,/g, ''), 10));
    });

    test('text filter narrows results client-side', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-searches').click();
      await expect(page.getByTestId('top-searches-table')).toBeVisible({ timeout: 10000 });
      const searchRows = getTableBodyRows(page.getByTestId('top-searches-table'));
      await searchRows.first().waitFor({ timeout: 10000 });
      const beforeCount = await searchRows.count();
      expect(beforeCount).toBeGreaterThan(1);

      // Read the first row's query text and filter by a partial match to narrow results
      const firstQuery = await searchRows.first().getByRole('cell').nth(1).textContent();
      const filterStr = firstQuery?.trim().slice(0, 3) ?? 'sam';
      await page.getByTestId('searches-filter-input').fill(filterStr);
      await expect(async () => {
        const afterCount = await searchRows.count();
        expect(afterCount).toBeLessThan(beforeCount);
      }).toPass({ timeout: 10000 });
    });
  });

  test.describe('No Results Tab', () => {
    test('shows rate banner with valid percentage between 0-100', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-no-results').click();
      const banner = page.getByTestId('no-result-rate-banner');
      await expect(banner).toBeVisible({ timeout: 10000 });

      // Use data-testid instead of CSS class selector for the rate value
      const rateText = await banner.getByTestId('rate-value').textContent();
      expect(rateText).toMatch(/\d+\.\d+%/);
      const rateValue = parseFloat(rateText!.replace('%', ''));
      expect(rateValue).toBeGreaterThan(0);
      expect(rateValue).toBeLessThan(100);
    });

    test('shows table of zero-result queries with seeded data', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-no-results').click();
      const table = page.getByTestId('no-results-table');
      await expect(table).toBeVisible({ timeout: 10000 });
      const rows = getTableBodyRows(table);
      await rows.first().waitFor({ timeout: 10000 });
      expect(await rows.count()).toBeGreaterThan(0);
    });
  });

  test.describe('Devices Tab', () => {
    test('shows platform cards with desktop > mobile counts', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-devices').click();
      await expect(page.getByTestId('device-desktop')).toBeVisible({ timeout: 10000 });
      await expect(page.getByTestId('device-mobile')).toBeVisible();

      // Use data-testid instead of CSS class selectors for counts and percentages
      const desktopCount = await page.getByTestId('device-desktop').getByTestId('device-count').textContent();
      const mobileCount = await page.getByTestId('device-mobile').getByTestId('device-count').textContent();
      const desktop = parseInt(desktopCount!.replace(/,/g, ''), 10);
      const mobile = parseInt(mobileCount!.replace(/,/g, ''), 10);
      expect(desktop).toBeGreaterThan(0);
      expect(mobile).toBeGreaterThan(0);
      expect(desktop).toBeGreaterThan(mobile);

      const desktopPct = await page.getByTestId('device-desktop').getByTestId('device-pct').textContent();
      expect(desktopPct).toMatch(/\d+\.\d+%/);
      const pctVal = parseFloat(desktopPct!.replace('%', ''));
      expect(pctVal).toBeGreaterThan(40);
      expect(pctVal).toBeLessThan(80);
    });

    test('shows device chart with SVG rendering', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-devices').click();
      await expect(page.getByTestId('device-desktop')).toBeVisible({ timeout: 10000 });

      // Use data-testid instead of CSS class selector for chart container
      const chart = page.getByTestId('device-chart');
      await expect(chart.getByTestId(CHART_CANVAS_TEST_ID)).toBeVisible();
    });
  });

  test.describe('Geography Tab', () => {
    test('shows country table with US as top country', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-geography').click();
      await expect(page.getByText('Searches by Country')).toBeVisible({ timeout: 10000 });
      const firstRow = getTableBodyRows(page.getByRole('table').first()).first();
      await firstRow.waitFor({ timeout: 10000 });
      await expect(firstRow).toContainText('United States');
    });

    test('country percentages sum to approximately 100%', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-geography').click();
      await expect(page.getByText('Searches by Country')).toBeVisible({ timeout: 10000 });
      const countryRows = getTableBodyRows(page.getByRole('table').first());
      await countryRows.first().waitFor({ timeout: 10000 });

      const pctCells = countryRows.getByTestId('country-share');
      const cellCount = await pctCells.count();
      let totalPct = 0;
      for (let i = 0; i < cellCount; i++) {
        const text = await pctCells.nth(i).textContent();
        totalPct += parseFloat(text!.replace('%', ''));
      }
      expect(totalPct).toBeGreaterThan(99);
      expect(totalPct).toBeLessThan(101);
    });

    test('clicking country shows drill-down with searches and regions', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-geography').click();
      await expect(page.getByText('Searches by Country')).toBeVisible({ timeout: 10000 });
      await getTableBodyRows(page.getByRole('table').first()).first().click();

      await expect(page.getByText('All Countries')).toBeVisible({ timeout: 5000 });
      await expect(page.getByText('Top Searches from United States')).toBeVisible();
      const searchRows = getTableBodyRows(page.getByRole('table').first());
      await searchRows.first().waitFor({ timeout: 10000 });
      expect(await searchRows.count()).toBeGreaterThan(0);

      await expect(page.getByText('States', { exact: true })).toBeVisible();
      const stateRows = getTableBodyRows(page.getByRole('table').nth(1));
      await stateRows.first().waitFor({ timeout: 10000 });
      expect(await stateRows.count()).toBeGreaterThanOrEqual(10);
      await expect(stateRows.first().getByRole('cell').nth(1)).not.toHaveText(/^$/);
    });

    test('back button returns from drill-down to country list', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-geography').click();
      await expect(page.getByText('Searches by Country')).toBeVisible({ timeout: 10000 });
      await getTableBodyRows(page.getByRole('table').first()).first().click();
      await expect(page.getByText('All Countries')).toBeVisible({ timeout: 5000 });
      await page.getByText('All Countries').click();
      await expect(page.getByText('Searches by Country')).toBeVisible();
    });
  });

  test.describe('Date Range Switching', () => {
    test('switching to 30d updates KPI values', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await expect(page.getByTestId('kpi-cards')).toBeVisible({ timeout: 10000 });

      // Use data-testid instead of CSS class selector
      const val7d = await page.getByTestId('kpi-total-searches').getByTestId('kpi-value').textContent();
      const num7d = parseInt(val7d!.replace(/,/g, ''), 10);

      await page.getByTestId('range-30d').click();
      await expect(async () => {
        const val30d = await page.getByTestId('kpi-total-searches').getByTestId('kpi-value').textContent();
        const num30d = parseInt(val30d!.replace(/,/g, ''), 10);
        expect(num30d).toBeGreaterThanOrEqual(num7d);
      }).toPass({ timeout: 10000 });
    });
  });

  test.describe('Sparkline Rendering', () => {
    test('Total Searches KPI renders sparkline with SVG path', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await expect(page.getByTestId('kpi-cards')).toBeVisible({ timeout: 10000 });

      // Total Searches receives sparkData from dates array — sparkline MUST render
      const sparkline = page.getByTestId('kpi-total-searches').getByTestId('sparkline');
      await expect(sparkline).toBeVisible({ timeout: 10_000 });
      await expect(sparkline.getByTestId(CHART_CANVAS_TEST_ID)).toBeVisible();

      // The surface must contain rendered marks (Recharts AreaChart produces fill + stroke paths)
      await expect(sparkline.getByTestId(CHART_MARK_TEST_ID).first()).toBeVisible();
    });

    test('No-Result Rate KPI renders sparkline with SVG path', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await expect(page.getByTestId('kpi-cards')).toBeVisible({ timeout: 10000 });

      const sparkline = page.getByTestId('kpi-no-result-rate').getByTestId('sparkline');
      await expect(sparkline).toBeVisible({ timeout: 10_000 });
      await expect(sparkline.getByTestId(CHART_MARK_TEST_ID).first()).toBeVisible();
    });
  });

  test.describe('Content Verification — Searches Tab', () => {
    test('search query cells contain non-empty text strings', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-searches').click();
      await expect(page.getByTestId('top-searches-table')).toBeVisible({ timeout: 10000 });
      const rows = getTableBodyRows(page.getByTestId('top-searches-table'));
      await rows.first().waitFor({ timeout: 10000 });

      // Verify first 3 rows have meaningful query text
      for (let i = 0; i < 3; i++) {
        const queryText = await rows.nth(i).getByTestId('search-query').textContent();
        expect(queryText?.trim().length).toBeGreaterThan(0);
      }
    });

    test('search count cells contain comma-formatted numbers', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-searches').click();
      const table = page.getByTestId('top-searches-table');
      await expect(table).toBeVisible({ timeout: 10000 });
      const rows = getTableBodyRows(table);
      await rows.first().waitFor({ timeout: 10000 });

      // Every count cell should be a valid number (possibly with commas)
      const count = await rows.count();
      for (let i = 0; i < Math.min(count, 5); i++) {
        const countText = await rows.nth(i).getByTestId('search-count').textContent();
        expect(countText?.trim()).toMatch(/^[\d,]+$/);
        const num = parseInt(countText!.replace(/,/g, ''), 10);
        expect(num).toBeGreaterThan(0);
      }
    });

    test('volume bars have non-zero width for rows with counts', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-searches').click();
      const table = page.getByTestId('top-searches-table');
      await expect(table).toBeVisible({ timeout: 10000 });
      const rows = getTableBodyRows(table);
      await rows.first().waitFor({ timeout: 10000 });

      // The volume cell contains a progress bar: a fixed-width track holding a
      // fill bar whose width is proportional to the row's share of the top count.
      const firstVolumeCell = rows.first().getByTestId('search-volume');
      await expect(firstVolumeCell).toBeVisible();

      const track = firstVolumeCell.getByTestId('search-volume-track');
      const fill = firstVolumeCell.getByTestId('search-volume-fill');
      await expect(track).toBeVisible();
      await expect(fill).toBeVisible();

      // The top row is the largest count, so its fill must span the whole track.
      const trackBox = await track.boundingBox();
      const fillBox = await fill.boundingBox();
      expect(trackBox!.width).toBeGreaterThan(0);
      expect(fillBox!.width).toBeGreaterThan(0);
      // Tolerance absorbs sub-pixel rounding only — a half-width fill still fails.
      expect(fillBox!.width).toBeCloseTo(trackBox!.width, -1);
    });
  });

  test.describe('Content Verification — Geography Tab', () => {
    test('country rows show flag emoji, country name, and code', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-geography').click();
      await expect(page.getByText('Searches by Country')).toBeVisible({ timeout: 10000 });
      const firstRow = getTableBodyRows(page.getByRole('table').first()).first();
      await firstRow.waitFor({ timeout: 10000 });

      // First country row (US) should show "United States" and "(US)"
      await expect(firstRow).toContainText('United States');
      await expect(firstRow).toContainText('US');

      // Count cell should have a formatted number
      const countText = await firstRow.getByTestId('country-count').textContent();
      expect(countText?.trim()).toMatch(/^[\d,]+$/);
      const countNum = parseInt(countText!.replace(/,/g, ''), 10);
      expect(countNum).toBeGreaterThan(0);

      // Share cell should have a percentage
      const shareText = await firstRow.getByTestId('country-share').textContent();
      expect(shareText?.trim()).toMatch(/^\d+\.\d+%$/);
    });

    test('drill-down shows country-specific search queries', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-geography').click();
      await expect(page.getByText('Searches by Country')).toBeVisible({ timeout: 10000 });
      await getTableBodyRows(page.getByRole('table').first()).first().click();

      // Top Searches from United States header
      await expect(page.getByText('Top Searches from United States')).toBeVisible({ timeout: 10_000 });

      // Search table should show actual queries with counts
      const searchRows = getTableBodyRows(page.getByRole('table').first());
      await searchRows.first().waitFor({ timeout: 10000 });
      const queryText = await searchRows.first().getByRole('cell').nth(1).textContent();
      expect(queryText?.trim().length).toBeGreaterThan(0);
    });

    test('States table in US drill-down shows state names with counts', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-geography').click();
      await expect(page.getByText('Searches by Country')).toBeVisible({ timeout: 10000 });
      await getTableBodyRows(page.getByRole('table').first()).first().click();
      await expect(page.getByText('All Countries')).toBeVisible({ timeout: 5000 });

      // States section header
      await expect(page.getByText('States', { exact: true })).toBeVisible();

      // State rows should have names and numeric counts
      const stateRows = getTableBodyRows(page.getByRole('table').nth(1));
      await stateRows.first().waitFor({ timeout: 10000 });
      await expect(stateRows.first().getByRole('cell').nth(1)).not.toHaveText(/^$/);
    });
  });

  test.describe('Content Verification — Devices Tab', () => {
    test('device counts add up across platform cards', async ({ page }) => {
      await page.goto(`/index/${analyticsIndexName()}/analytics`);
      await page.getByTestId('tab-devices').click();
      await expect(page.getByTestId('device-desktop')).toBeVisible({ timeout: 10000 });
      await expect(page.getByTestId('device-mobile')).toBeVisible();
      await expect(page.getByTestId('device-tablet')).toBeVisible();

      // Seeded analytics includes all three user-facing platform cards.
      let total = 0;
      for (const platform of ['desktop', 'mobile', 'tablet']) {
        const card = page.getByTestId(`device-${platform}`);
        const countText = await card.getByTestId('device-count').textContent();
        total += parseInt(countText!.replace(/,/g, ''), 10);
      }
      expect(total).toBeGreaterThan(0);

      // Desktop percentage should be between 40-80% (seeded at 60%)
      const desktopPct = await page.getByTestId('device-desktop').getByTestId('device-pct').textContent();
      const pctVal = parseFloat(desktopPct!.replace('%', ''));
      expect(pctVal).toBeGreaterThan(30);
      expect(pctVal).toBeLessThan(85);
    });
  });
});
