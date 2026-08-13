/**
 * E2E-UI Full Suite — Migrate Page (Real Server)
 *
 * NON-MOCKED SIMULATED-HUMAN REAL-BROWSER TESTS.
 * Tests the Migrate from Algolia page against a real Flapjack backend.
 * Credential-free cases cover form UI, validation, toggles, and error states;
 * the dry-run case reuses the canonical real Algolia fixture.
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
 * - Receipt-backed dry-run report and target non-mutation
 */
import type { Page } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import {
  cleanupMigrationIndexes,
  seedAlgoliaIndex,
} from '../../fixtures/algolia.fixture';
import {
  expectMigrationPreviewReport,
  expectMigrationWriteUnavailable,
  type MigrationPreviewOracle,
} from '../../fixtures/migration-preview.fixture';
import { assertIndexNotCreated } from '../../fixtures/source-provider.fixture';

const ALGOLIA_PREVIEW_ORACLE: MigrationPreviewOracle = {
  summary: {
    totalEntries: 14,
    hardRejections: 0,
    warnings: 9,
    scopeGaps: 5,
  },
  entry: {
    severity: 'Warning',
    code: 'PersistedNoBehaviorSetting',
    resource: 'Settings',
    jsonPath: '$.hitsPerPage',
  },
};

test.describe('Migrate Page', () => {
  function sourceIndexInput(page: Page) {
    return page.getByLabel('Source Index (Algolia)');
  }

  function targetIndexInput(page: Page) {
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

  test('preview button is disabled when credentials are empty', async ({ page }) => {
    const previewButton = page.getByRole('button', { name: /preview migration/i });
    await expect(previewButton).toBeVisible();
    await expect(previewButton).toBeDisabled();
  });

  test('filling credentials and source index enables preview button', async ({ page }) => {
    await page.getByLabel('Application ID').fill('test-app-id');
    await page.getByLabel('Admin API Key').fill('test-api-key');
    await sourceIndexInput(page).fill('test-index');

    const previewButton = page.getByRole('button', { name: /preview migration/i });
    await expect(previewButton).toBeEnabled();
    await expectMigrationWriteUnavailable(page, 'Algolia', 'test-index');
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

  test('custom target index keeps write unavailable before preview', async ({ page }) => {
    await page.getByLabel('Application ID').fill('test-app-id');
    await page.getByLabel('Admin API Key').fill('test-api-key');
    await sourceIndexInput(page).fill('source-idx');
    await targetIndexInput(page).fill('custom-target');

    await expect(page.getByRole('button', { name: /preview migration/i })).toBeEnabled();
    await expectMigrationWriteUnavailable(page, 'Algolia', 'custom-target');
  });

  test('clearing source index re-disables preview button', async ({ page }) => {
    // Fill all fields to enable the button
    await page.getByLabel('Application ID').fill('test-app-id');
    await page.getByLabel('Admin API Key').fill('test-api-key');
    await sourceIndexInput(page).fill('test-index');

    const previewButton = page.getByRole('button', { name: /preview migration/i });
    await expect(previewButton).toBeEnabled();

    // Clear the source index
    await sourceIndexInput(page).clear();

    // Button should become disabled again
    await expect(previewButton).toBeDisabled();
  });

  test('clearing app ID re-disables preview button', async ({ page }) => {
    await page.getByLabel('Application ID').fill('test-app-id');
    await page.getByLabel('Admin API Key').fill('test-api-key');
    await sourceIndexInput(page).fill('test-index');

    const previewButton = page.getByRole('button', { name: /preview migration/i });
    await expect(previewButton).toBeEnabled();

    // Clear the app ID
    await page.getByLabel('Application ID').clear();
    await expect(previewButton).toBeDisabled();
  });

  test('previewing with invalid credentials shows error', async ({ page }) => {
    const sourceIndex = `nonexistent-index-${Date.now()}`;
    const fakeApiKey = 'fake-api-key';
    const rawUpstreamUrl = `https://fake-app-id-dsn.algolia.net/1/indexes/${sourceIndex}/settings`;

    // Fill in fake credentials
    await page.getByLabel('Application ID').fill('fake-app-id');
    await page.getByLabel('Admin API Key').fill(fakeApiKey);
    await sourceIndexInput(page).fill(sourceIndex);

    await page.getByRole('button', { name: /preview migration/i }).click();

    // Should show a sanitized error card after the request fails.
    const errorCard = page.getByTestId('migration-error-card');
    await expect(errorCard).toContainText('Algolia appId is invalid', { timeout: 15_000 });
    await expect(errorCard).not.toContainText(fakeApiKey);
    await expect(errorCard).not.toContainText(rawUpstreamUrl);
  });

  test('preview attempts do not expose Algolia API keys in API Logs', async ({ page }) => {
    const uniqueApiKey = `fake-api-key-${Date.now()}`;

    await page.getByLabel('Application ID').fill('fake-app-id');
    await page.getByLabel('Admin API Key').fill(uniqueApiKey);
    await sourceIndexInput(page).fill('nonexistent-index');

    await page.getByRole('button', { name: /preview migration/i }).click();
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

  test('dry-run preview shows Algolia warning and scope-gap report', async ({ page, request }, testInfo) => {
    test.setTimeout(120_000);
    const algolia = await seedAlgoliaIndex();

    try {
      await page.getByLabel('Application ID').fill(algolia.appId);
      await page.getByLabel('Admin API Key').fill(algolia.adminKey);
      await sourceIndexInput(page).fill(algolia.indexName);
      await targetIndexInput(page).fill(algolia.targetIndexName);

      const previewButton = page.getByRole('button', { name: /preview migration/i });
      await expect(previewButton).toBeVisible();
      await expect(previewButton).toBeEnabled();
      await expectMigrationWriteUnavailable(page, 'Algolia', algolia.targetIndexName);
      await previewButton.click();

      await expectMigrationPreviewReport(page, ALGOLIA_PREVIEW_ORACLE);
      await assertIndexNotCreated(request, algolia.targetIndexName);
      await expect(page.getByRole('button', { name: /^submit migration$/i })).toBeEnabled();
    } finally {
      const cleanupReceipt = await cleanupMigrationIndexes(algolia);
      await testInfo.attach('algolia-preview-cleanup-receipt', {
        body: JSON.stringify(cleanupReceipt, null, 2),
        contentType: 'application/json',
      });
    }
  });
});
