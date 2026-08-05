/**
 * E2E-UI Full Suite — Cross-Page Flows (Real Server)
 *
 * NON-MOCKED SIMULATED-HUMAN REAL-BROWSER TESTS.
 * These tests verify data consistency and navigation between pages.
 * Each test starts on one page, performs an action, navigates to another page,
 * and verifies the effect is reflected there.
 *
 * Covers:
 * - Overview → click index → Search page loads with correct index
 * - Create Index on Overview → Add docs → Search → Delete → verify gone
 * - Merchandising → pin → save as rule → Rules page verifies rule → cleanup
 * - System Indexes tab → click index → lands on correct search page
 * - Settings → modify attribute → save → Search → verify → revert
 */
import { test, expect } from '../../fixtures/auth.fixture';
import { TEST_INDEX, getSidebar, waitForOverviewIndexRow, waitForSearchResultsOrEmptyState } from '../helpers';
import { deleteIndex, addDocuments, searchIndex, getSettings, updateSettings } from '../../fixtures/api-helpers';
import { createIsolatedMerchandisingScenario } from '../scenario-helpers';

test.describe('Cross-Page Flows', () => {

  // ---------- Overview → Search ----------

  test('clicking index on Overview navigates to its Search page', async ({ page }) => {
    await page.goto('/overview');
    await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 10_000 });
    const indexRow = await waitForOverviewIndexRow(page, TEST_INDEX);

    // Click the index name to navigate
    await indexRow.getByRole('heading', { name: TEST_INDEX }).click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}`));

    // Search page should load with results panel
    await waitForSearchResultsOrEmptyState(page);
  });

  // ---------- Full Index Lifecycle: Create → Docs → Search → Delete ----------

  test('create index, add documents, search, then delete', async ({ page, request }) => {
    const tempIndex = `e2e-lifecycle-${Date.now()}`;

    // Clean up any leftover from previous failed runs
    await deleteIndex(request, tempIndex);

    // Step 1: Create index on Overview page
    await page.goto('/overview');
    await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 10_000 });

    await page.getByRole('button', { name: /create.*index/i }).click();
    const createDialog = page.getByRole('dialog');
    await expect(createDialog).toBeVisible();
    await createDialog.getByLabel('Index Name', { exact: true }).fill(tempIndex);
    await createDialog.getByRole('button', { name: /create index/i }).click();
    await expect(createDialog).not.toBeVisible({ timeout: 10_000 });

    // Wait for the actual overview row, not the creation toast text.
    const overviewRow = await waitForOverviewIndexRow(page, tempIndex);

    // Step 2: Add a document via batch API (the /documents POST may not auto-create)
    await addDocuments(request, tempIndex, [
      { objectID: 'lifecycle-1', name: 'Lifecycle Test Product', brand: 'TestBrand' },
    ]);

    // Wait for indexing to complete by polling search
    await expect(async () => {
      const body = await searchIndex(request, tempIndex, '');
      expect(body.nbHits ?? 0).toBeGreaterThan(0);
    }).toPass({ timeout: 15_000 });

    // Step 3: Open the new index through visible UI on Overview
    await overviewRow.getByRole('heading', { name: tempIndex }).click();
    await waitForSearchResultsOrEmptyState(page);

    // Search for the document we added
    const searchInput = page.getByPlaceholder(/search/i).first();
    await searchInput.fill('Lifecycle');
    await searchInput.press('Enter');

    // Verify result appears
    await expect(page.getByText('Lifecycle Test Product').first()).toBeVisible({ timeout: 15_000 });

    // Step 4: Return to Overview and delete the index through visible UI
    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'Overview' }).click();
    await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 10_000 });
    const refreshedOverviewRow = await waitForOverviewIndexRow(page, tempIndex);

    await refreshedOverviewRow.getByTitle(`Delete index "${tempIndex}"`).click();
    const deleteDialog = page.getByRole('dialog');
    await expect(deleteDialog).toBeVisible({ timeout: 10_000 });
    await deleteDialog.getByRole('button', { name: 'Delete' }).click();
    await expect(refreshedOverviewRow).not.toBeVisible({ timeout: 10_000 });
  });

  // ---------- Merchandising → Save as Rule → Rules Page ----------

  test('pin result in Merchandising, save as rule, verify on Rules page', async ({ page, request }) => {
    const isolatedScenario = await createIsolatedMerchandisingScenario(request, 'cross-page-save-rule');

    try {
      // Navigate to Merchandising
      await page.goto(`/index/${isolatedScenario.indexName}/merchandising`);
      await expect(page.getByText('Merchandising Studio').first()).toBeVisible({ timeout: 15_000 });

      // Search for the isolated fixture
      const searchInput = page.getByPlaceholder(/search query to merchandise/i);
      await searchInput.fill(isolatedScenario.searchQuery);
      await page.getByRole('button', { name: /^Search$/i }).click();

      const cards = page.getByTestId('merch-card');
      await expect(cards.first()).toBeVisible({ timeout: 10_000 });

      // Pin the first result
      const firstCard = cards.first();
      const pinBtn = firstCard.getByRole('button', { name: /pin/i });
      await pinBtn.first().click();
      await expect(page.getByText(/Pinned #/i).first()).toBeVisible({ timeout: 5_000 });

      // Save as Rule
      const saveBtn = page.getByRole('button', { name: /Save as Rule/i });
      await expect(saveBtn).toBeVisible();

      const responsePromise = page.waitForResponse(
        resp => resp.url().includes('/rules'),
        { timeout: 10_000 }
      );
      await saveBtn.click();
      await responsePromise;

      // Navigate to Rules page via shared index tab bar
      await page.getByTestId('index-tab-rules').click();
      await expect(page).toHaveURL(new RegExp(`/index/${isolatedScenario.indexName}/rules`));
      await expect(page.getByRole('button', { name: /add rule/i })).toBeVisible({ timeout: 15_000 });

      // Verify the saved merchandising rule uses the isolated query pattern
      await expect(page.getByTestId('rules-list').getByText(isolatedScenario.searchQuery).first()).toBeVisible({
        timeout: 10_000,
      });
    } finally {
      await deleteIndex(request, isolatedScenario.indexName);
    }
  });

  // ---------- System Indexes Tab → Search Page ----------

  test('clicking index in System Indexes tab navigates to search page', async ({ page }) => {
    await page.goto('/system');
    await expect(page.getByRole('heading', { name: /system/i })).toBeVisible({ timeout: 10_000 });

    // Switch to Indexes tab
    await page.getByRole('tab', { name: /indexes/i }).click();
    const indexLink = page.getByTestId('index-link-e2e-products');
    await expect(indexLink).toBeVisible({ timeout: 15_000 });

    // Click the index link
    await indexLink.click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}`));

    // Search page should load
    await waitForSearchResultsOrEmptyState(page);
  });

  // ---------- Settings → Save → Reload → Verify Persistence ----------

  test('settings changes persist after save and page reload', async ({ page, request }) => {
    // Get original settings for restore
    const originalSettings = await getSettings(request, TEST_INDEX);

    try {
      // Navigate to Settings page
      await page.goto(`/index/${TEST_INDEX}/settings`);
      await expect(page.getByRole('heading', { name: /settings/i })).toBeVisible({ timeout: 10_000 });
      await expect(page.getByText('Searchable Attributes').first()).toBeVisible({ timeout: 10_000 });

      // Toggle a seeded searchable attribute to trigger dirty form state
      const tagsChip = page.getByTestId('attr-chip-tags').first();
      await expect(tagsChip).toBeVisible({ timeout: 10_000 });
      await tagsChip.click();
      await expect(tagsChip).toHaveAttribute('aria-pressed', 'false');

      const saveButton = page.getByRole('button', { name: /save/i });
      await expect(saveButton).toBeVisible({ timeout: 5_000 });

      const saveResponsePromise = page.waitForResponse(
        (response) =>
          response.request().method() === 'PUT' &&
          response.url().includes(`/1/indexes/${TEST_INDEX}/settings`) &&
          (response.status() === 200 || response.status() === 207),
        { timeout: 15_000 },
      );
      await Promise.all([
        saveButton.click(),
        saveResponsePromise,
      ]);
      await expect(saveButton).not.toBeVisible({ timeout: 10_000 });

      await page.reload();
      await expect(page.getByRole('heading', { name: /settings/i })).toBeVisible({ timeout: 10_000 });
      const reloadedTagsChip = page.getByTestId('attr-chip-tags').first();
      await expect(reloadedTagsChip).toBeVisible({ timeout: 10_000 });
      await expect(reloadedTagsChip).toHaveAttribute('aria-pressed', 'false');
    } finally {
      // Restore original settings to avoid test pollution
      await updateSettings(request, TEST_INDEX, originalSettings);
    }
  });

  // ---------- Search with Analytics → Analytics Page ----------

  test('analytics page loads with seeded data', async ({ page }) => {
    // Navigate directly to Analytics page (seeded data should already exist)
    await page.goto(`/index/${TEST_INDEX}/analytics`);
    await expect(page.getByTestId('analytics-heading')).toBeVisible({ timeout: 15_000 });

    // Analytics page should show KPI cards with seeded data
    await expect(page.getByTestId('kpi-cards')).toBeVisible({ timeout: 10_000 });
  });

  // ---------- Overview Analytics Summary → Analytics Page ----------

  test('Overview analytics section links to full Analytics page', async ({ page }) => {
    await page.goto('/overview');
    await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 10_000 });

    // Analytics card should be present
    const analyticsCard = page.getByTestId('overview-analytics');
    await expect(analyticsCard).toBeVisible({ timeout: 10_000 });
    await expect(analyticsCard.getByText('Search Analytics')).toBeVisible();

    // The "View Details" link should be present — click it
    const viewLink = analyticsCard.getByText('View Details');
    await expect(viewLink).toBeVisible({ timeout: 5_000 });
    await viewLink.click();
    // Should navigate to the analytics page
    await expect(page).toHaveURL(/analytics/);
    await expect(page.getByTestId('analytics-heading')).toBeVisible({ timeout: 10_000 });
  });

  // ---------- Full Navigation Cycle ----------

  test('Overview → Search → Settings → Rules → Synonyms → back to Overview', async ({ page }) => {
    // Start on Overview
    await page.goto('/overview');
    await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText(TEST_INDEX).first()).toBeVisible();

    // Navigate to Search via index click
    await page.getByText(TEST_INDEX).first().click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}`));
    await waitForSearchResultsOrEmptyState(page);

    // Navigate to Settings via shared index tab bar
    await page.getByTestId('index-tab-settings').click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}/settings`));
    await expect(page.getByRole('heading', { name: /settings/i })).toBeVisible({ timeout: 10_000 });

    // Navigate to Rules via shared index tab bar
    await page.getByTestId('index-tab-rules').click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}/rules`));
    await expect(page.getByRole('button', { name: /add rule/i })).toBeVisible({ timeout: 15_000 });

    // Navigate to Synonyms via shared index tab bar
    await page.getByTestId('index-tab-synonyms').click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}/synonyms`));
    await expect(page.getByRole('button', { name: /add synonym/i })).toBeVisible({ timeout: 15_000 });

    // Navigate back to Overview via sidebar
    const sidebar = getSidebar(page);
    await sidebar.getByText('Overview').first().click();
    await expect(page).toHaveURL(/\/overview/);
    await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 10_000 });
  });
});
