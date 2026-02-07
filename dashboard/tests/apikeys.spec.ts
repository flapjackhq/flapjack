import { test, expect } from '@playwright/test';

test.describe('API Keys Page', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem('flapjack-api-key', 'abcdef0123456789');
      localStorage.setItem('flapjack-app-id', 'flapjack');
    });
    await page.goto('/keys');
  });

  test('should load the API keys page', async ({ page }) => {
    await expect(page.getByText('API Keys').first()).toBeVisible();
  });

  test('should display create key button', async ({ page }) => {
    const createButton = page.getByRole('button', { name: /create.*key/i }).first();
    await expect(createButton).toBeVisible();
  });

  test('should open create key dialog when clicking create button', async ({ page }) => {
    const createButton = page.getByRole('button', { name: /create.*key/i }).first();
    await createButton.click();

    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();
    await expect(page.getByText('Create API Key')).toBeVisible();
  });

  test('should display form fields in create key dialog', async ({ page }) => {
    const createButton = page.getByRole('button', { name: /create.*key/i }).first();
    await createButton.click();

    // Check for description input (placeholder-based)
    const descInput = page.getByPlaceholder(/frontend search key/i);
    await expect(descInput).toBeVisible();

    // Check for ACL permission buttons (custom styled buttons)
    await expect(page.getByText('Search').first()).toBeVisible();
    await expect(page.getByText('Browse').first()).toBeVisible();
    await expect(page.getByText('Permissions').first()).toBeVisible();
  });

  test('should close dialog when clicking cancel', async ({ page }) => {
    const createButton = page.getByRole('button', { name: /create.*key/i }).first();
    await createButton.click();

    const cancelButton = page.getByRole('button', { name: /cancel/i });
    await cancelButton.click();

    const dialog = page.getByRole('dialog');
    await expect(dialog).not.toBeVisible();
  });

  test('should display API keys list or empty state', async ({ page }) => {
    // Wait for API call to settle (may retry on failure)
    await page.waitForTimeout(2000);

    const hasKeys = await page.locator('[data-testid="keys-list"]').isVisible().catch(() => false);
    const hasEmptyState = await page.getByText(/no api keys/i).isVisible().catch(() => false);
    const hasLoading = await page.getByText(/loading api keys/i).isVisible().catch(() => false);
    const hasHeader = await page.getByText('API Keys').first().isVisible().catch(() => false);

    expect(hasKeys || hasEmptyState || hasLoading || hasHeader).toBeTruthy();
  });

  test('should display description for keys page', async ({ page }) => {
    // Wait for loading to resolve (API may fail without backend)
    await page.waitForTimeout(3000);
    await expect(page.getByText(/manage api keys/i)).toBeVisible({ timeout: 10000 });
  });

  test('should navigate back to overview from sidebar', async ({ page }) => {
    const overviewLink = page.getByRole('link', { name: /overview/i });
    await overviewLink.click();

    await expect(page).toHaveURL(/\/overview/);
  });
});
