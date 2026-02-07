import { test, expect } from '@playwright/test';

test.describe('Navigation & Layout', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem('flapjack-api-key', 'abcdef0123456789');
      localStorage.setItem('flapjack-app-id', 'flapjack');
    });
    await page.goto('/');
  });

  test('should display header with title', async ({ page }) => {
    const header = page.locator('header');
    await expect(header).toBeVisible();

    await expect(page.getByText('Flapjack').first()).toBeVisible();
  });

  test('should display sidebar navigation', async ({ page }) => {
    const sidebar = page.locator('aside');
    await expect(sidebar).toBeVisible();

    // Check for navigation links within the sidebar
    await expect(sidebar.getByRole('link', { name: /overview/i })).toBeVisible();
    await expect(sidebar.getByRole('link', { name: /api keys/i })).toBeVisible();
    await expect(sidebar.getByRole('link', { name: /settings/i })).toBeVisible();
  });

  test('should navigate between pages using sidebar', async ({ page }) => {
    // Navigate to API Keys
    await page.getByRole('link', { name: /api keys/i }).click();
    await expect(page).toHaveURL(/\/keys/);

    // Navigate to Settings
    await page.getByRole('link', { name: /settings/i }).click();
    await expect(page).toHaveURL(/\/settings/);

    // Navigate back to Overview
    await page.getByRole('link', { name: /overview/i }).click();
    await expect(page).toHaveURL(/\/overview/);
  });

  test('should highlight active navigation item', async ({ page }) => {
    // Go to API Keys page
    await page.goto('/keys');

    const keysLink = page.getByRole('link', { name: /api keys/i });
    // Active links get bg-primary class
    await expect(keysLink).toHaveClass(/bg-primary/);
  });

  test('should show connection status in header', async ({ page }) => {
    // Wait for health check to resolve
    await page.waitForTimeout(2000);

    // Should show one of: Connected, No API Key, Disconnected, or Connecting...
    const connected = page.getByText('Connected');
    const noKey = page.getByText('No API Key');
    const disconnected = page.getByText('Disconnected');
    const connecting = page.getByText('Connecting...');

    const hasConnected = await connected.isVisible().catch(() => false);
    const hasNoKey = await noKey.isVisible().catch(() => false);
    const hasDisconnected = await disconnected.isVisible().catch(() => false);
    const hasConnecting = await connecting.isVisible().catch(() => false);

    expect(hasConnected || hasNoKey || hasDisconnected || hasConnecting).toBeTruthy();
  });

  test('should preserve dark mode preference across navigation', async ({ page }) => {
    const darkModeToggle = page.getByRole('button', { name: /toggle.*theme/i });
    await darkModeToggle.click();

    // Navigate to another page
    await page.getByRole('link', { name: /api keys/i }).click();

    const html = page.locator('html');
    await expect(html).toHaveClass(/dark/);
  });

  test('should handle 404 for unknown routes', async ({ page }) => {
    await page.goto('/this-route-does-not-exist');

    await expect(page.getByText(/page not found/i)).toBeVisible();
  });

  test('should open connection settings dialog', async ({ page }) => {
    const settingsButton = page.getByRole('button', { name: /connection settings/i });
    await settingsButton.click();

    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();
    await expect(page.getByText(/admin api key/i)).toBeVisible();
  });
});
