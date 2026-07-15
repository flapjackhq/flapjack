import { test, expect } from '../../fixtures/auth.fixture';
import {
  hasAlgoliaCredentials,
  MissingAlgoliaCredentialsError,
  resolveAlgoliaCredentialMode,
  seedAlgoliaIndex,
  cleanupMigrationIndexes,
  type AlgoliaTestContext,
} from '../../fixtures/algolia.fixture';
import { EXPECTED_COUNTS } from '../../fixtures/test-data';

/**
 * Algolia Migration — E2E-UI (real browser, real server, no mocks)
 *
 * Tests the full Algolia migration flow through the browser UI:
 * fill credentials → select index → click Migrate → verify success card → browse results.
 *
 * Requires Algolia credentials in .env.secret. Local runs skip when unavailable;
 * CI throws instead so the vendor-backed migration path cannot pass by absence.
 */

const credentialMode = resolveAlgoliaCredentialMode({
  hasCredentials: hasAlgoliaCredentials(),
  isCI: !!process.env.CI,
});

if (credentialMode === 'fail') {
  // A silent CI skip is indistinguishable from a pass in workflow status.
  throw new MissingAlgoliaCredentialsError();
}

const describeOrSkip = credentialMode === 'run' ? test.describe : test.describe.skip;

describeOrSkip('Algolia Migration (real browser)', () => {
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

  test.afterAll(async () => {
    if (!ctx) {
      return;
    }
    await cleanupMigrationIndexes(ctx);
  });

  test('migrate Algolia index via UI: fill form → migrate → verify success → browse', async ({ page }) => {
    const algoliaCtx = requireAlgoliaContext();

    // Navigate to Migrate page
    await page.goto('/migrate');
    await expect(page.getByRole('heading', { name: /migrate from algolia/i })).toBeVisible();

    // Fill in Algolia credentials
    await page.locator('#app-id').fill(algoliaCtx.appId);
    await page.locator('#api-key').fill(algoliaCtx.adminKey);
    await page.locator('#source-index').fill(algoliaCtx.indexName);

    // Enable overwrite
    const overwriteSwitch = page.locator('#overwrite');
    await overwriteSwitch.click();
    await expect(overwriteSwitch).toHaveAttribute('data-state', 'checked');

    // Verify Migrate button shows index name and is enabled
    const migrateButton = page.getByRole('button', {
      name: new RegExp(`Migrate.*"${algoliaCtx.indexName}"`),
    });
    await expect(migrateButton).toBeEnabled();

    // Click Migrate and verify user-visible success state instead of coupling
    // to transport timing for a single response packet.
    await migrateButton.click();

    // Wait for success card
    await expect(page.getByText('Migration complete')).toBeVisible({ timeout: 90_000 });

    // Verify imported counts using data-testid (not CSS class selectors)
    await expect(page.getByTestId('migrate-stat-documents')).toHaveText(String(EXPECTED_COUNTS.documents));
    await expect(page.getByTestId('migrate-stat-settings')).toHaveText('Applied');
    await expect(page.getByTestId('migrate-stat-synonyms')).toHaveText(String(EXPECTED_COUNTS.synonyms));
    await expect(page.getByTestId('migrate-stat-rules')).toHaveText(String(EXPECTED_COUNTS.rules));

    // Click "Browse Index" and verify navigation
    await page.getByRole('link', { name: 'Browse Index' }).click();
    await expect(page).toHaveURL(new RegExp(`/index/${encodeURIComponent(algoliaCtx.indexName)}`));

    // Verify documents are searchable
    await expect(page.getByRole('heading', { name: algoliaCtx.indexName })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId('results-panel')).toBeVisible({ timeout: 15_000 });

    const searchInput = page.getByPlaceholder(/search documents/i);
    await searchInput.fill('laptop');
    await searchInput.press('Enter');
    await expect(page.getByText('p01').first()).toBeVisible({ timeout: 10_000 });
  });

  test('invalid credentials show error state in UI', async ({ page }) => {
    await page.goto('/migrate');
    await page.locator('#app-id').fill('INVALID_APP_ID');
    await page.locator('#api-key').fill('invalid_key_0000000000');
    await page.locator('#source-index').fill('nonexistent-index');

    const migrateButton = page.getByRole('button', { name: /migrate/i });
    await migrateButton.click();

    await expect(page.getByText('Migration failed')).toBeVisible({ timeout: 15_000 });
  });
});
