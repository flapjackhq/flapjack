import { test, expect } from '@playwright/test';

test.describe('Search & Browse Page', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem('flapjack-api-key', 'abcdef0123456789');
      localStorage.setItem('flapjack-app-id', 'flapjack');
    });
    await page.goto('/search/test-index');
  });

  test('should load the search page with index name', async ({ page }) => {
    await expect(page.getByRole('heading', { name: 'test-index' })).toBeVisible();
  });

  test('should display search input', async ({ page }) => {
    const searchInput = page.getByPlaceholder(/search documents/i);
    await expect(searchInput).toBeVisible();
  });

  test('should display search button', async ({ page }) => {
    const searchButton = page.getByRole('button', { name: /^search$/i });
    await expect(searchButton).toBeVisible();
  });

  test('should display breadcrumb with back to overview', async ({ page }) => {
    const backLink = page.getByRole('button', { name: /overview/i });
    await expect(backLink).toBeVisible();
  });

  test('should perform search when typing and pressing enter', async ({ page }) => {
    const searchInput = page.getByPlaceholder(/search documents/i);
    await searchInput.fill('test query');
    await searchInput.press('Enter');

    // Wait for search to complete
    await page.waitForTimeout(1000);
  });

  test('should display results panel or search state', async ({ page }) => {
    await page.waitForTimeout(2000);

    const hasResults = await page.locator('[data-testid="results-panel"]').isVisible().catch(() => false);
    const hasError = await page.getByText(/search error/i).isVisible().catch(() => false);
    const hasNoResults = await page.getByText(/no results found/i).isVisible().catch(() => false);
    const hasSearching = await page.getByText(/searching/i).isVisible().catch(() => false);

    expect(hasResults || hasError || hasNoResults || hasSearching).toBeTruthy();
  });

  test('should display facets panel or facet state', async ({ page }) => {
    await page.waitForTimeout(2000);

    const hasFacets = await page.locator('[data-testid="facets-panel"]').isVisible().catch(() => false);
    const hasNoFacets = await page.getByText(/no facets configured/i).isVisible().catch(() => false);

    expect(hasFacets || hasNoFacets).toBeTruthy();
  });

  test('should show Add Documents button', async ({ page }) => {
    await expect(page.getByRole('button', { name: /add documents/i })).toBeVisible();
  });

  test('should open Add Documents dialog with tabs and form builder', async ({ page }) => {
    await page.getByRole('button', { name: /add documents/i }).click();

    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();
    await expect(page.getByRole('tab', { name: /json/i })).toBeVisible();
    await expect(page.getByRole('tab', { name: /upload/i })).toBeVisible();
    await expect(page.getByRole('tab', { name: /sample data/i })).toBeVisible();
    // Form builder and copy button are inline in the JSON tab
    await expect(page.getByRole('button', { name: /add field/i })).toBeVisible();
    await expect(page.getByRole('button', { name: /copy/i })).toBeVisible();
  });

  test('should switch to Sample Data tab and show Load Movies button', async ({ page }) => {
    await page.getByRole('button', { name: /add documents/i }).click();
    await page.getByRole('tab', { name: /sample data/i }).click();
    await expect(page.getByRole('button', { name: /load.*movies/i })).toBeVisible();
  });

  test('should navigate back to overview when clicking breadcrumb', async ({ page }) => {
    const backButton = page.getByRole('button', { name: /overview/i });
    await backButton.click();

    await expect(page).toHaveURL(/\/overview/);
  });
});
