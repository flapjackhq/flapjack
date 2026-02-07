import { test, expect } from '@playwright/test';

test.describe('Overview Page', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem('flapjack-api-key', 'abcdef0123456789');
      localStorage.setItem('flapjack-app-id', 'flapjack');
    });
    await page.goto('/');
  });

  test('should load the overview page', async ({ page }) => {
    await expect(page.getByText('Overview').first()).toBeVisible();
  });

  test('should display stats cards', async ({ page }) => {
    const statsCards = page.locator('[data-testid="stat-card"]');
    await expect(statsCards).toHaveCount(4);
  });

  test('should display indices section', async ({ page }) => {
    // The "Indices" heading is inside a CardTitle
    await expect(page.getByText('Indices').first()).toBeVisible();
  });

  test('should show empty state or index list', async ({ page }) => {
    // Wait for data to load
    await page.waitForTimeout(1000);

    // Should show either indices or empty state message
    const hasIndices = await page.locator('.border.rounded-md').first().isVisible().catch(() => false);
    const hasEmptyState = await page.getByText(/no indices/i).isVisible().catch(() => false);
    const hasLoading = await page.locator('.animate-spin').isVisible().catch(() => false);

    expect(hasIndices || hasEmptyState || hasLoading).toBeTruthy();
  });

  test('should display Create Index button', async ({ page }) => {
    const createButton = page.getByRole('button', { name: /create.*index/i });
    await expect(createButton).toBeVisible();
  });

  test('should open Create Index dialog', async ({ page }) => {
    const createButton = page.getByRole('button', { name: /create.*index/i }).first();
    await createButton.click();

    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole('heading', { name: 'Create Index' })).toBeVisible();
    await expect(page.getByPlaceholder(/products/i)).toBeVisible();
  });

  test('should close Create Index dialog on cancel', async ({ page }) => {
    await page.getByRole('button', { name: /create.*index/i }).click();
    await expect(page.getByRole('dialog')).toBeVisible();

    await page.getByRole('button', { name: /cancel/i }).click();
    await expect(page.getByRole('dialog')).not.toBeVisible();
  });

  test('should toggle dark mode', async ({ page }) => {
    const darkModeToggle = page.getByRole('button', { name: /toggle.*theme/i });
    await darkModeToggle.click();

    const html = page.locator('html');
    await expect(html).toHaveClass(/dark/);
  });
});
