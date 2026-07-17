/**
 * E2E-UI Full Suite — Migrate Page (Real Server)
 *
 * NON-MOCKED SIMULATED-HUMAN REAL-BROWSER TESTS.
 * Tests the Migrate from Algolia page against a real Flapjack backend.
 * Since we cannot test actual Algolia migration without real credentials,
 * tests focus on form UI, validation, toggle behavior, error states,
 * and info section content.
 *
 * Pre-requisites:
 *   - Flapjack server running on the repo-local configured backend port
 *   - Vite dev server on the repo-local configured dashboard port
 *
 * Covers:
 * - All form sections visible on load (credentials, index, overwrite, info)
 * - Migrate button disabled when credentials empty
 * - Filling credentials + source index enables migrate button
 * - Migrate button text includes effective target index name
 * - API key field toggles between password/text with eye button
 * - Overwrite toggle switches on and off
 * - Target index placeholder mirrors source index name
 * - Submitting with invalid credentials shows error card
 * - Clearing fields re-disables migrate button
 * - Info section shows all three info items
 * - Custom target index overrides source name in button text
 */
import { test, expect } from '../../fixtures/auth.fixture';

test.describe('Migrate Page', () => {
  function sourceIndexInput(page: import('@playwright/test').Page) {
    return page.getByLabel('Source Index (Algolia)');
  }

  function targetIndexInput(page: import('@playwright/test').Page) {
    return page.getByLabel(/Target Index \(Flapjack\)/);
  }

  test.beforeEach(async ({ page }) => {
    await page.goto('/migrate');
    await expect(page.getByRole('heading', { name: /migrate from algolia/i })).toBeVisible({ timeout: 10_000 });
  });

  test('page loads with all form sections visible', async ({ page }) => {
    // Credentials card
    await expect(page.getByText('Algolia Credentials')).toBeVisible();
    await expect(page.getByLabel('Application ID')).toBeVisible();
    await expect(page.getByLabel('Admin API Key')).toBeVisible();

    // Index card
    await expect(page.getByText('Source Index (Algolia)')).toBeVisible();
    await expect(page.getByText(/Target Index/)).toBeVisible();

    // Overwrite toggle
    await expect(page.getByText('Overwrite if exists')).toBeVisible();
    await expect(page.getByRole('switch')).toBeVisible();

    // Info section
    await expect(page.getByText('What gets migrated:')).toBeVisible();
    await expect(page.getByText('Credentials:')).toBeVisible();
    await expect(page.getByText('Large indexes:')).toBeVisible();
  });

  test('migrate button is disabled when credentials are empty', async ({ page }) => {
    const migrateBtn = page.getByRole('button', { name: /migrate/i });
    await expect(migrateBtn).toBeVisible();
    await expect(migrateBtn).toBeDisabled();
  });

  test('filling credentials and source index enables migrate button', async ({ page }) => {
    await page.getByLabel('Application ID').fill('test-app-id');
    await page.getByLabel('Admin API Key').fill('test-api-key');
    await sourceIndexInput(page).fill('test-index');

    const migrateBtn = page.getByRole('button', { name: /migrate/i });
    await expect(migrateBtn).toBeEnabled();
    // Button text should include the effective target index name
    await expect(migrateBtn).toContainText('test-index');
  });

  test('API key field toggles visibility with eye button', async ({ page }) => {
    const keyInput = page.getByLabel('Admin API Key');
    await expect(keyInput).toHaveAttribute('type', 'password');

    // Fill a value so we can verify toggle
    await keyInput.fill('secret-key');

    // Click the eye toggle button
    const toggleBtn = page.getByTestId('toggle-api-key-visibility');
    await toggleBtn.click();
    await expect(keyInput).toHaveAttribute('type', 'text');

    // Click again to hide
    await toggleBtn.click();
    await expect(keyInput).toHaveAttribute('type', 'password');
  });

  test('overwrite toggle can be switched on and off', async ({ page }) => {
    const toggle = page.getByRole('switch');
    // Initially off
    await expect(toggle).toHaveAttribute('data-state', 'unchecked');

    // Turn on
    await toggle.click();
    await expect(toggle).toHaveAttribute('data-state', 'checked');

    // Turn off
    await toggle.click();
    await expect(toggle).toHaveAttribute('data-state', 'unchecked');
  });

  test('target index defaults to source index name when left blank', async ({ page }) => {
    const sourceInput = sourceIndexInput(page);
    await sourceInput.fill('my-products');

    // The target input placeholder should reflect the source name
    const targetInput = targetIndexInput(page);
    await expect(targetInput).toHaveAttribute('placeholder', 'my-products');
  });

  test('custom target index overrides source name in button text', async ({ page }) => {
    await page.getByLabel('Application ID').fill('test-app-id');
    await page.getByLabel('Admin API Key').fill('test-api-key');
    await sourceIndexInput(page).fill('source-idx');
    await targetIndexInput(page).fill('custom-target');

    const migrateBtn = page.getByRole('button', { name: /migrate/i });
    await expect(migrateBtn).toBeEnabled();
    // Button should show the custom target name, not the source name
    await expect(migrateBtn).toContainText('custom-target');
  });

  test('clearing source index re-disables migrate button', async ({ page }) => {
    // Fill all fields to enable the button
    await page.getByLabel('Application ID').fill('test-app-id');
    await page.getByLabel('Admin API Key').fill('test-api-key');
    await sourceIndexInput(page).fill('test-index');

    const migrateBtn = page.getByRole('button', { name: /migrate/i });
    await expect(migrateBtn).toBeEnabled();

    // Clear the source index
    await sourceIndexInput(page).clear();

    // Button should become disabled again
    await expect(migrateBtn).toBeDisabled();
  });

  test('clearing app ID re-disables migrate button', async ({ page }) => {
    await page.getByLabel('Application ID').fill('test-app-id');
    await page.getByLabel('Admin API Key').fill('test-api-key');
    await sourceIndexInput(page).fill('test-index');

    const migrateBtn = page.getByRole('button', { name: /migrate/i });
    await expect(migrateBtn).toBeEnabled();

    // Clear the app ID
    await page.getByLabel('Application ID').clear();
    await expect(migrateBtn).toBeDisabled();
  });

  test('submitting with invalid credentials shows error', async ({ page }) => {
    const importUnavailableMessage =
      'Migration import leg is not implemented; no data was written, and this endpoint will not report success it did not perform.';

    // Fill in fake credentials
    await page.getByLabel('Application ID').fill('fake-app-id');
    await page.getByLabel('Admin API Key').fill('fake-api-key');
    await sourceIndexInput(page).fill('nonexistent-index');

    // Click migrate
    await page.getByRole('button', { name: /migrate/i }).click();

    // Should show an error card after the request fails
    const errorCard = page.getByTestId('migration-error-card');
    await expect(errorCard).toContainText(importUnavailableMessage, { timeout: 15_000 });
    await expect(errorCard).toContainText('migration_import_unavailable');
  });

  test('migration attempts do not expose Algolia API keys in API Logs', async ({ page }) => {
    const uniqueApiKey = `fake-api-key-${Date.now()}`;

    await page.getByLabel('Application ID').fill('fake-app-id');
    await page.getByLabel('Admin API Key').fill(uniqueApiKey);
    await sourceIndexInput(page).fill('nonexistent-index');

    await page.getByRole('button', { name: /migrate/i }).click();
    await expect(page.getByText(/migration failed/i)).toBeVisible({ timeout: 15_000 });

    // Generate a normal logged request so the logs page proves the logger still works.
    await page.goto('/overview');
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({ timeout: 10_000 });

    await page.goto('/logs');
    await expect(page.getByRole('heading', { name: /api log/i })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId('logs-list')).toBeVisible({ timeout: 10_000 });

    const filterInput = page.getByPlaceholder(/filter by url, method, or body/i);
    await filterInput.fill(uniqueApiKey);
    await expect(page.getByRole('heading', { name: /no api logs/i })).toBeVisible({ timeout: 10_000 });
  });

  test('info section describes what gets migrated', async ({ page }) => {
    // Verify all three info paragraphs are present
    await expect(page.getByText(/Settings.*searchable attributes.*facets.*ranking/i)).toBeVisible();
    await expect(page.getByText(/API key is sent directly.*not stored or logged/i)).toBeVisible();
    await expect(page.getByText(/Documents are fetched in batches/i)).toBeVisible();
  });

  test('target index field shows helper text about defaulting', async ({ page }) => {
    await expect(page.getByText('Defaults to the source index name if left blank.')).toBeVisible();
  });

  test('API key field shows security note', async ({ page }) => {
    await expect(page.getByText('Needs read access. Not stored anywhere.')).toBeVisible();
  });
});
