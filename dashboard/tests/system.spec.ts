import { test, expect } from '@playwright/test';

test.describe('System Page', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem('flapjack-api-key', 'abcdef0123456789');
      localStorage.setItem('flapjack-app-id', 'flapjack');
    });
    await page.goto('/system');
  });

  test('should load the system page', async ({ page }) => {
    await expect(page.getByText('System').first()).toBeVisible();
  });

  test('should display health tab by default', async ({ page }) => {
    await expect(page.getByRole('tab', { name: /health/i })).toBeVisible();
    await expect(page.getByRole('tab', { name: /indices/i })).toBeVisible();
    await expect(page.getByRole('tab', { name: /replication/i })).toBeVisible();
  });

  test('should show health status or error', async ({ page }) => {
    await page.waitForTimeout(3000);

    const hasStatus = await page.getByText(/status/i).first().isVisible().catch(() => false);
    const hasFailed = await page.getByText(/failed to fetch/i).isVisible().catch(() => false);

    expect(hasStatus || hasFailed).toBeTruthy();
  });

  test('should switch to indices tab', async ({ page }) => {
    await page.getByRole('tab', { name: /indices/i }).click();

    await page.waitForTimeout(2000);

    const hasIndices = await page.getByText(/total indices/i).isVisible().catch(() => false);
    const hasError = await page.getByText(/unable to load/i).isVisible().catch(() => false);

    expect(hasIndices || hasError).toBeTruthy();
  });

  test('should switch to replication tab', async ({ page }) => {
    await page.getByRole('tab', { name: /replication/i }).click();

    await page.waitForTimeout(2000);

    const hasNodeId = await page.getByText(/node id/i).isVisible().catch(() => false);
    const hasUnavailable = await page.getByText(/unavailable/i).isVisible().catch(() => false);

    expect(hasNodeId || hasUnavailable).toBeTruthy();
  });

  test('should navigate to system from sidebar', async ({ page }) => {
    await page.goto('/overview');
    await page.getByRole('link', { name: /system/i }).click();
    await expect(page).toHaveURL(/\/system/);
  });
});
