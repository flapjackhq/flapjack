import { test, expect } from '../../fixtures/auth.fixture';
import {
  hasAlgoliaCredentials,
  MissingAlgoliaCredentialsError,
  resolveAlgoliaCredentialMode,
  seedAlgoliaIndex,
  cleanupMigrationIndexes,
  type AlgoliaTestContext,
} from '../../fixtures/algolia.fixture';
import { EXPECTED_COUNTS, PRODUCTS } from '../../fixtures/test-data';

/**
 * Algolia Migration — E2E-UI (real browser, real server, no mocks)
 *
 * Tests the full Algolia migration flow through the browser UI:
 * fill credentials → select index → click Migrate → verify success card → browse results.
 *
 * Requires Algolia credentials in .env.secret. Missing credentials fail at
 * module load so the vendor-backed migration path cannot pass by absence.
 */

const credentialMode = resolveAlgoliaCredentialMode({
  hasCredentials: hasAlgoliaCredentials(),
});

if (credentialMode === 'fail') {
  throw new MissingAlgoliaCredentialsError();
}

test.describe('Algolia Migration (real browser)', () => {
  let ctx: AlgoliaTestContext | undefined;

  test.describe.configure({ timeout: 120_000 });

  function requireAlgoliaContext(): AlgoliaTestContext {
    if (!ctx) {
      throw new Error('Algolia test context was not seeded');
    }
    return ctx;
  }

  test.beforeAll(async () => {
    ctx = await seedAlgoliaIndex();
  });

  test.afterAll(async ({}, testInfo) => {
    if (!ctx) {
      return;
    }
    const cleanupReceipt = await cleanupMigrationIndexes(ctx);
    await testInfo.attach('migration-cleanup-receipt', {
      body: JSON.stringify(cleanupReceipt, null, 2),
      contentType: 'application/json',
    });
  });

  test('migrate Algolia index via UI: fill form → migrate → verify success → browse', async ({ page }) => {
    const algoliaCtx = requireAlgoliaContext();

    // Navigate to Migrate page
    await page.goto('/migrate');
    await expect(page.getByRole('heading', { name: /migrate from algolia/i })).toBeVisible();

    // Fill in Algolia credentials
    await page.getByLabel('Application ID').fill(algoliaCtx.appId);
    await page.getByLabel('Admin API Key').fill(algoliaCtx.adminKey);
    await page.getByLabel('Source Index (Algolia)').fill(algoliaCtx.indexName);
    await page.getByLabel(/Target Index \(Flapjack\)/).fill(algoliaCtx.targetIndexName);

    // MIG-5 is deferred: this proof exercises the shipped create-only default.
    await expect(page.getByRole('switch', { name: 'Overwrite if exists' })).toHaveAttribute(
      'data-state',
      'unchecked',
    );

    // Verify Migrate button shows index name and is enabled
    const migrateButton = page.getByRole('button', {
      name: new RegExp(`Migrate.*"${algoliaCtx.targetIndexName}"`),
    });
    await expect(migrateButton).toBeEnabled();

    // Click Migrate and verify user-visible success state instead of coupling
    // to transport timing for a single response packet.
    await migrateButton.click();

    // Wait for success card
    await expect(page.getByText('Migration complete')).toBeVisible({ timeout: 90_000 });
    await expect(
      page.getByText(`Index ${algoliaCtx.targetIndexName} is ready.`),
    ).toBeVisible();

    // Verify imported counts using data-testid (not CSS class selectors)
    await expect(page.getByTestId('migrate-stat-documents')).toHaveText(String(EXPECTED_COUNTS.documents));
    await expect(page.getByTestId('migrate-stat-settings')).toHaveText('Applied');
    await expect(page.getByTestId('migrate-stat-synonyms')).toHaveText(String(EXPECTED_COUNTS.synonyms));
    await expect(page.getByTestId('migrate-stat-rules')).toHaveText(String(EXPECTED_COUNTS.rules));

    // Click "Browse Index" and verify navigation
    await page.getByRole('link', { name: 'Browse Index' }).click();
    await expect(page).toHaveURL(
      new RegExp(`/index/${encodeURIComponent(algoliaCtx.targetIndexName)}$`),
    );

    // Verify documents are searchable
    await expect(page.getByRole('heading', { name: algoliaCtx.targetIndexName })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId('results-panel')).toBeVisible({ timeout: 15_000 });

    const searchInput = page.getByPlaceholder(/search documents/i);
    await searchInput.fill(PRODUCTS[0].name);
    await searchInput.press('Enter');
    const knownDocument = page.getByTestId('document-card').filter({ hasText: PRODUCTS[0].objectID });
    await expect(knownDocument).toHaveCount(1, { timeout: 10_000 });
    await expect(knownDocument).toContainText(PRODUCTS[0].name);
    await expect(knownDocument).toContainText(PRODUCTS[0].description);
  });

  test('invalid credentials show error state in UI', async ({ page }) => {
    const algoliaCtx = requireAlgoliaContext();

    await page.goto('/migrate');
    await page.getByLabel('Application ID').fill(algoliaCtx.appId);
    await page.getByLabel('Admin API Key').fill('invalid_key_0000000000');
    await page.getByLabel('Source Index (Algolia)').fill(algoliaCtx.indexName);
    await page.getByLabel(/Target Index \(Flapjack\)/).fill(algoliaCtx.invalidTargetIndexName);

    const migrateButton = page.getByRole('button', { name: /migrate/i });
    await migrateButton.click();

    const errorCard = page.getByTestId('migration-error-card');
    await expect(errorCard).toContainText('Migration failed', { timeout: 15_000 });
    await expect(errorCard).toContainText('Algolia returned 403 Forbidden');
    await expect(errorCard).toContainText('Invalid Application-ID or API key');
  });
});
