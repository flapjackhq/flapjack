import { fileURLToPath } from 'node:url';
import type { Page } from '@playwright/test';
import { EXPECTED_COUNTS, TEST_INDEX } from '../../fixtures/test-data';
import { test, expect } from '../../fixtures/auth.fixture';
import {
  gotoIndexPage,
  gotoOverviewPage,
  waitForOverviewIndexRow,
  waitForSearchResultsOrEmptyState,
} from '../helpers';

const SCREENSHOT_TIMEOUT_MS = 10_000;

function readmeScreenshotPath(filename: string): string {
  return fileURLToPath(new URL(`../../../img/${filename}`, import.meta.url));
}

async function saveReadmeScreenshot(page: Page, filename: string): Promise<void> {
  await page.screenshot({
    path: readmeScreenshotPath(filename),
  });
}

// Readiness contracts shared by the default (non-destructive) validation tests
// and the opt-in refresh tests. Each asserts the exact screen content a README
// screenshot must capture, so it fails for a real render/readiness defect. The
// only difference between the two suites is whether a screenshot file is written.
async function assertOverviewScreenshotReady(page: Page): Promise<void> {
  await gotoOverviewPage(page);
  await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({
    timeout: SCREENSHOT_TIMEOUT_MS,
  });
  await expect(page.getByTestId('stat-card-indexes')).toBeVisible({
    timeout: SCREENSHOT_TIMEOUT_MS,
  });

  const seededRow = await waitForOverviewIndexRow(page, TEST_INDEX);
  await expect(seededRow).toBeVisible({ timeout: SCREENSHOT_TIMEOUT_MS });
}

async function assertSearchScreenshotReady(page: Page): Promise<void> {
  await gotoIndexPage(page, TEST_INDEX);
  await waitForSearchResultsOrEmptyState(page);

  await expect(page.getByPlaceholder('Search documents...')).toBeVisible({
    timeout: SCREENSHOT_TIMEOUT_MS,
  });
  await expect(page.getByRole('button', { name: 'Search', exact: true })).toBeVisible({
    timeout: SCREENSHOT_TIMEOUT_MS,
  });

  const resultsPanel = page.getByTestId('results-panel');
  await expect(resultsPanel).toBeVisible({ timeout: SCREENSHOT_TIMEOUT_MS });

  const resultCountText = await resultsPanel.getByTestId('results-count').textContent();
  const resultCount = Number(resultCountText?.replace(/[,\u00a0\u202f]/g, '') ?? '0');
  expect(resultCount).toBeGreaterThanOrEqual(EXPECTED_COUNTS.documents);

  await expect(
    resultsPanel.getByTestId('document-card').first(),
  ).toBeVisible({ timeout: 15_000 });
}

async function assertMigrateScreenshotReady(page: Page): Promise<void> {
  await page.goto('/migrate');
  await expect(page).toHaveURL(/\/migrate$/);
  await expect(page.getByRole('heading', { name: /migrate/i }).first()).toBeVisible({
    timeout: SCREENSHOT_TIMEOUT_MS,
  });
  await expect(page.getByRole('main')).toBeVisible({ timeout: SCREENSHOT_TIMEOUT_MS });
}

// Opt-in only: writes screenshot files under img/. Never runs on the default
// browser lane so the suite stays non-destructive unless explicitly requested.
function defineReadmeScreenshotRefreshTests() {
  test('Overview screenshot refresh uses seeded index readiness contract', async ({ page }) => {
    await assertOverviewScreenshotReady(page);
    // A stray modal would corrupt the captured README image.
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await saveReadmeScreenshot(page, 'dash_overview.png');
  });

  test('Search screenshot refresh uses seeded results readiness contract', async ({ page }) => {
    await assertSearchScreenshotReady(page);
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await saveReadmeScreenshot(page, 'dash_search.png');
  });

  test('Migrate screenshot refresh uses route-level readiness contract', async ({ page }) => {
    await assertMigrateScreenshotReady(page);
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await saveReadmeScreenshot(page, 'dash_migrate_alg.png');
  });
}

// Default lane: exercises the same readiness contracts without writing files, so
// a broken Overview/Search/Migrate screen fails the browser suite instead of
// being silently skipped behind an unset environment variable.
function defineReadmeScreenshotReadinessTests() {
  test('Overview screenshot surface stays render-ready', async ({ page }) => {
    await assertOverviewScreenshotReady(page);
    await expect(page.getByRole('dialog')).toHaveCount(0);
  });

  test('Search screenshot surface stays render-ready', async ({ page }) => {
    await assertSearchScreenshotReady(page);
    await expect(page.getByRole('dialog')).toHaveCount(0);
  });

  test('Migrate screenshot surface stays render-ready', async ({ page }) => {
    await assertMigrateScreenshotReady(page);
    await expect(page.getByRole('dialog')).toHaveCount(0);
  });
}

test.describe('README screenshots', () => {
  if (process.env.UPDATE_README_SCREENSHOTS) {
    defineReadmeScreenshotRefreshTests();
  } else {
    defineReadmeScreenshotReadinessTests();
  }
});
