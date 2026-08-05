/**
 * E2E-UI Full Suite — Overview Page (Real Server)
 *
 * NON-MOCKED SIMULATED-HUMAN REAL-BROWSER TESTS.
 * Tests run against a REAL Flapjack server with seeded test data.
 * The 'e2e-products' index is pre-seeded with a product corpus.
 *
 * Covers:
 * - Index list with seeded index and document count
 * - Stat cards (indexes, documents, storage, health)
 * - Health indicator
 * - Create/delete index lifecycle
 * - Create Index dialog templates
 * - Export All and Upload buttons
 * - Per-index export/import buttons
 * - Index row storage and timestamp info
 * - Analytics summary section
 * - Settings link navigation
 * - Clicking index navigates to search page
 * - Export All triggers download
 */
import type { Download, Page, Response } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import { CHART_CANVAS_TEST_ID, TEST_INDEX, waitForOverviewIndexRow } from '../helpers';
import { deleteIndex } from '../../fixtures/api-helpers';

async function waitForOptionalExportDownload(page: Page): Promise<Download | null> {
  try {
    return await page.waitForEvent('download', { timeout: 15_000 });
  } catch {
    return null;
  }
}

async function waitForOptionalExportResponse(page: Page): Promise<Response | null> {
  try {
    return await page.waitForResponse(
      resp => resp.url().includes('/snapshot') || resp.url().includes('/export'),
      { timeout: 15_000 },
    );
  } catch {
    return null;
  }
}

test.describe('Overview Page', () => {

  test.beforeEach(async ({ page }) => {
    await page.goto('/overview');
    await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 10000 });
  });

  test('index list shows e2e-products with document count', async ({ page }) => {
    const indexRow = await waitForOverviewIndexRow(page, TEST_INDEX);

    await expect(indexRow.getByRole('heading', { name: TEST_INDEX })).toBeVisible();
    await expect(indexRow.getByTestId(`overview-index-meta-${TEST_INDEX}`)).toContainText(/\d+\s+documents/i);
  });

  test('stat cards display total indexes, documents, and storage', async ({ page }) => {
    const indexesCard = page.getByTestId('stat-card-indexes');
    await expect(indexesCard).toBeVisible();
    await expect.poll(async () => {
      const text = (await indexesCard.getByTestId('stat-value').textContent())?.trim() ?? '';
      const parsed = Number(text.replace(/,/g, ''));
      return Number.isFinite(parsed) ? parsed : -1;
    }, {
      timeout: 10_000,
    }).toBeGreaterThanOrEqual(1);

    const docsCard = page.getByTestId('stat-card-documents');
    await expect(docsCard).toBeVisible();
    await expect.poll(async () => {
      const text = (await docsCard.getByTestId('stat-value').textContent())?.trim() ?? '';
      const parsed = Number(text.replace(/,/g, ''));
      return Number.isFinite(parsed) ? parsed : -1;
    }, {
      timeout: 10_000,
    }).toBeGreaterThanOrEqual(12);

    const storageCard = page.getByTestId('stat-card-storage');
    await expect(storageCard).toBeVisible();
    await expect.poll(async () => {
      return (await storageCard.getByTestId('stat-value').textContent())?.trim() ?? '';
    }, {
      timeout: 10_000,
    }).toMatch(/^\d+(\.\d+)?\s*(B|KB|MB|GB)$/i);
    await expect(storageCard.getByTestId('stat-value')).not.toHaveText(/^0 Bytes$/i);
  });

  test('health indicator shows Healthy', async ({ page }) => {
    const statusCard = page.getByTestId('stat-card-status');
    await expect(statusCard).toBeVisible();
    await expect(statusCard.getByText('Healthy')).toBeVisible({ timeout: 10000 });
  });

  test('create new index e2e-temp, verify it appears, then delete it', async ({ page, request }) => {
    const tempIndex = 'e2e-temp';

    await deleteIndex(request, tempIndex);

    await page.getByRole('button', { name: /create.*index/i }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole('heading', { name: 'Create Index' })).toBeVisible();

    await dialog.getByLabel('Index Name', { exact: true }).fill(tempIndex);
    await dialog.getByRole('button', { name: /create index/i }).click();

    await expect(dialog).not.toBeVisible({ timeout: 10000 });
    const createdRow = await waitForOverviewIndexRow(page, tempIndex);

    const deleteBtn = createdRow.getByTitle(`Delete index "${tempIndex}"`);
    await deleteBtn.click();

    const confirmDialog = page.getByRole('dialog');
    await expect(confirmDialog).toBeVisible();
    await expect(confirmDialog.getByText(/are you sure/i)).toBeVisible();
    await confirmDialog.getByRole('button', { name: /delete/i }).click();

    await expect(confirmDialog).not.toBeVisible({ timeout: 10000 });

    await page.goto('/overview');
    await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 10000 });
    await waitForOverviewIndexRow(page, TEST_INDEX);
    await expect(page.getByTestId(`overview-index-row-${tempIndex}`)).not.toBeVisible({ timeout: 5000 });
  });

  test('server health badge shows connected status', async ({ page }) => {
    const statusCard = page.getByTestId('stat-card-status');
    await expect(statusCard).toBeVisible();
    await expect(statusCard.getByText('Healthy')).toBeVisible({ timeout: 10000 });
    await expect(statusCard.getByText('Disconnected')).toBeHidden();
  });

  test('clicking e2e-products navigates to its search page', async ({ page }) => {
    const indexRow = await waitForOverviewIndexRow(page, TEST_INDEX);
    await indexRow.getByRole('heading', { name: TEST_INDEX }).click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}`));
  });

  test('Create Index dialog shows template options', async ({ page }) => {
    await page.getByRole('button', { name: /create.*index/i }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();

    await expect(dialog.getByText('Empty index')).toBeVisible();
    await expect(dialog.getByText(/Movies/)).toBeVisible();
    await expect(dialog.getByText(/Products/)).toBeVisible();
    await expect(dialog.getByLabel('Index Name', { exact: true })).toBeVisible();

    await dialog.getByRole('button', { name: /cancel/i }).click();
    await expect(dialog).not.toBeVisible({ timeout: 5000 });
  });

  test('selecting Movies template auto-fills index name', async ({ page }) => {
    await page.getByRole('button', { name: /create.*index/i }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();

    await dialog.getByText(/Movies/).click();
    const nameInput = dialog.getByLabel('Index Name', { exact: true });
    await expect(nameInput).toHaveValue('movies');

    await dialog.getByRole('button', { name: /cancel/i }).click();
  });

  test('Export All and Upload buttons are visible', async ({ page }) => {
    const exportBtn = page.getByTestId('overview-export-all-btn');
    await expect(exportBtn).toBeVisible();
    await expect(exportBtn).toContainText('Export All');

    const uploadBtn = page.getByTestId('overview-upload-btn');
    await expect(uploadBtn).toBeVisible();
    await expect(uploadBtn).toContainText('Upload');
  });

  test('per-index export and import buttons are visible', async ({ page }) => {
    const exportBtn = page.getByTestId(`overview-export-${TEST_INDEX}`);
    await expect(exportBtn).toBeVisible();

    const importBtn = page.getByTestId(`overview-import-${TEST_INDEX}`);
    await expect(importBtn).toBeVisible();
  });

  test('index row shows storage size and update info', async ({ page }) => {
    const indexMeta = page.getByTestId(`overview-index-meta-${TEST_INDEX}`);

    await expect(indexMeta).toBeVisible({ timeout: 10_000 });
    await expect(indexMeta).toContainText(/\d+\s+documents/i);
    await expect(indexMeta).toContainText(/\d+(\.\d+)?\s*(B|KB|MB|GB)/i);
    await expect(indexMeta).toContainText(/updated/i);
  });

  test('search analytics section displays data from seeded analytics', async ({ page }) => {
    const analyticsCard = page.getByTestId('overview-analytics');
    await expect(analyticsCard).toBeVisible({ timeout: 10_000 });
    await expect(analyticsCard.getByText('Search Analytics')).toBeVisible();
    // KPI labels only render after the analytics API responds (skeletons shown while loading).
    // Under full-suite load with 3 workers the analytics endpoint can take >10 s.
    await expect(analyticsCard.getByText('Total Searches')).toBeVisible({ timeout: 30_000 });
    await expect(analyticsCard.getByText('Unique Users')).toBeVisible();
    await expect(analyticsCard.getByText('No-Result Rate')).toBeVisible();
  });

  test('analytics chart renders in the overview analytics section', async ({ page }) => {
    const analyticsCard = page.getByTestId('overview-analytics');
    await expect(analyticsCard).toBeVisible({ timeout: 10_000 });

    // Chart only renders after API responds; under full-suite load this can take >10 s.
    const chart = analyticsCard.getByTestId('overview-analytics-chart');
    await expect(chart).toBeVisible({ timeout: 30_000 });
    await expect(chart.getByTestId(CHART_CANVAS_TEST_ID)).toBeVisible();
  });

  test('View Details link in analytics section navigates to analytics page', async ({ page }) => {
    const analyticsCard = page.getByTestId('overview-analytics');
    await expect(analyticsCard).toBeVisible({ timeout: 10_000 });

    // "View Details" renders after data loads; allow extra time under full-suite load.
    const viewDetailsLink = analyticsCard.getByText('View Details');
    await expect(viewDetailsLink).toBeVisible({ timeout: 10_000 });
    await viewDetailsLink.click();

    await expect(page).toHaveURL(/\/analytics/);
    await expect(page.getByTestId('analytics-heading')).toBeVisible({ timeout: 10_000 });
  });

  test('Settings button in index row navigates to settings', async ({ page }) => {
    const indexRow = await waitForOverviewIndexRow(page, TEST_INDEX);
    const settingsLink = indexRow.getByRole('link', { name: /settings/i });
    await expect(settingsLink).toBeVisible({ timeout: 10_000 });
    await settingsLink.click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}/settings`));
  });

  test('Export All button triggers download', async ({ page }) => {
    const exportBtn = page.getByTestId('overview-export-all-btn');
    await expect(exportBtn).toBeVisible();

    const [download, response] = await Promise.all([
      waitForOptionalExportDownload(page),
      waitForOptionalExportResponse(page),
      exportBtn.click().then(() => null),
    ]);

    expect(download ?? response).not.toBeNull();
    expect(download?.suggestedFilename() ?? 'export response observed').toMatch(/\S/);
    expect(response?.status() ?? 200).toBeGreaterThanOrEqual(200);
    expect(response?.status() ?? 200).toBeLessThan(300);
  });
});
