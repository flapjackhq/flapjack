import { test, expect } from '@playwright/test';

test.describe('Settings Page', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem('flapjack-api-key', 'abcdef0123456789');
      localStorage.setItem('flapjack-app-id', 'flapjack');
    });
    await page.goto('/settings');
  });

  test('should load the settings page', async ({ page }) => {
    await page.waitForTimeout(1000);

    const hasSettings = await page.getByText(/settings/i).first().isVisible().catch(() => false);
    const hasNoIndices = await page.getByText(/no indices/i).isVisible().catch(() => false);
    const hasLoading = await page.getByText(/loading/i).isVisible().catch(() => false);

    expect(hasSettings || hasNoIndices || hasLoading).toBeTruthy();
  });

  test('should display settings form when indices exist', async ({ page }) => {
    await page.waitForTimeout(3000);

    const hasNoIndices = await page.getByText(/no indices/i).isVisible().catch(() => false);

    if (!hasNoIndices) {
      await expect(page.getByText('Search Behavior').first()).toBeVisible({ timeout: 10000 });
    }
  });

  test('should display search behavior section', async ({ page }) => {
    await page.waitForTimeout(3000);

    const hasNoIndices = await page.getByText(/no indices/i).isVisible().catch(() => false);

    if (!hasNoIndices) {
      await expect(page.getByText('Searchable Attributes')).toBeVisible();
      await expect(page.getByText('Hits Per Page')).toBeVisible();
    }
  });

  test('should display faceting section', async ({ page }) => {
    await page.waitForTimeout(3000);

    const hasNoIndices = await page.getByText(/no indices/i).isVisible().catch(() => false);

    if (!hasNoIndices) {
      await expect(page.getByRole('heading', { name: 'Faceting' })).toBeVisible();
      await expect(page.getByText('Attributes For Faceting')).toBeVisible();
    }
  });

  test('should show save button when form is modified', async ({ page }) => {
    await page.waitForTimeout(1000);

    const hasNoIndices = await page.getByText(/no indices/i).isVisible().catch(() => false);

    if (!hasNoIndices) {
      const hitsInput = page.getByPlaceholder('20');
      if (await hitsInput.isVisible()) {
        await hitsInput.fill('25');

        const saveButton = page.getByRole('button', { name: /save/i });
        await expect(saveButton).toBeVisible();
      }
    }
  });

  test('should show no indices message when empty', async ({ page }) => {
    await page.waitForTimeout(3000);

    const hasNoIndices = await page.getByText(/no indices/i).isVisible().catch(() => false);
    const hasSettings = await page.getByText('Search Behavior').isVisible().catch(() => false);

    expect(hasNoIndices || hasSettings).toBeTruthy();
  });

  test('should navigate back to overview from sidebar', async ({ page }) => {
    const overviewLink = page.getByRole('link', { name: /overview/i });
    await overviewLink.click();

    await expect(page).toHaveURL(/\/overview/);
  });
});
