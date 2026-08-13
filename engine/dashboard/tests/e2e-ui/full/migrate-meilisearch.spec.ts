import type { Page } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import {
  expectMigrationPreviewReport,
  expectMigrationWriteUnavailable,
  type MigrationPreviewOracle,
} from '../../fixtures/migration-preview.fixture';
import {
  assertIndexNotCreated,
  cleanupMigratedIndexes,
  cleanupSourceContainer,
  startMeilisearchSource,
  type SourceProviderContext,
} from '../../fixtures/source-provider.fixture';

const targetIndex = `fj_e2e_meilisearch_migration_${Date.now()}`;
const dryRunTargetIndex = `fj_e2e_meilisearch_dry_run_${Date.now()}`;
const MEILISEARCH_PREVIEW_ORACLE: MigrationPreviewOracle = {
  summary: {
    totalEntries: 11,
    hardRejections: 0,
    warnings: 6,
    scopeGaps: 5,
  },
  entry: {
    severity: 'Warning',
    code: 'MeilisearchDocumentOrderNotContractual',
    resource: 'Settings',
    jsonPath: '$.documents',
  },
};

async function fillMeilisearchCredentials(
  page: Page,
  context: SourceProviderContext,
): Promise<void> {
  const providerControl = page.getByRole('button', { name: 'Meilisearch', exact: true });
  await expect(providerControl).toBeVisible();
  await providerControl.click();
  await page.getByLabel('Endpoint', { exact: true }).fill(context.endpoint);
  await page.getByLabel('API Key', { exact: true }).fill(context.apiKey);
}

async function expectAsyncMigrationContract(page: Page): Promise<void> {
  await expect(page.getByText(/Job ID/i)).toBeVisible();
  await expect(page.getByText(/Phase/i)).toBeVisible();
  await expect(page.getByText(/Disposition/i)).toBeVisible();
}

async function discoverAndSelectMeilisearchSource(
  page: Page,
  meilisearchSource: SourceProviderContext,
): Promise<void> {
  await page.getByRole('button', { name: 'Discover sources', exact: true }).click();
  await page.getByLabel('Source index', { exact: true }).click();
  const sourceOption = page.getByRole('option', { name: new RegExp(`${meilisearchSource.sourceName}.*2`) });
  await expect(sourceOption).toContainText(meilisearchSource.sourceName);
  await sourceOption.click();
}

test.describe('Meilisearch migration (real browser)', () => {
  let source: SourceProviderContext | undefined;

  test.describe.configure({ timeout: 120_000 });

  function requireSource(): SourceProviderContext {
    if (!source) {
      throw new Error('Meilisearch source fixture was not started');
    }
    return source;
  }

  test.beforeAll(async ({ request }) => {
    source = await startMeilisearchSource(request);
  });

  test.afterAll(async ({ request }) => {
    await cleanupMigratedIndexes(request, [targetIndex, dryRunTargetIndex]);
    if (source) {
      await cleanupSourceContainer(source);
    }
  });

  test('migrate Meilisearch index via UI: discover → migrate → verify success → browse @meilisearch-loopback-opt-in', async ({ page }) => {
    const meilisearchSource = requireSource();

    await page.goto('/migrate');
    await expect(page.getByRole('heading', { name: /migrate/i })).toContainText('Migrate');
    await fillMeilisearchCredentials(page, meilisearchSource);

    await discoverAndSelectMeilisearchSource(page, meilisearchSource);

    await page.getByLabel(/Target Index \(Flapjack\)/).fill(targetIndex);
    await page.getByRole('button', { name: /preview migration/i }).click();
    await expectMigrationPreviewReport(page, MEILISEARCH_PREVIEW_ORACLE);
    await page.getByRole('button', { name: /^submit migration$/i }).click();

    await expectAsyncMigrationContract(page);
    await expect(page.getByText('Migration complete', { exact: true })).toBeVisible({ timeout: 90_000 });
    await expect(page.getByTestId('migrate-stat-documents')).toHaveText('2');
    await page.getByRole('link', { name: 'Browse Index', exact: true }).click();
    await expect(page).toHaveURL(new RegExp(`/index/${targetIndex}$`));

    await page.getByPlaceholder(/search documents/i).fill('Espresso Tamper');
    await page.getByPlaceholder(/search documents/i).press('Enter');
    const seededDocument = page.getByTestId('document-card').filter({ hasText: 'MEILI-001' });
    await expect(seededDocument).toHaveCount(1, { timeout: 10_000 });
    await expect(seededDocument).toContainText('Espresso Tamper');
  });

  test('dry-run preview shows Meilisearch warning and scope-gap report @meilisearch-loopback-opt-in', async ({ page, request }) => {
    const meilisearchSource = requireSource();

    await page.goto('/migrate');
    await expect(page.getByRole('heading', { name: /migrate/i })).toContainText('Migrate');
    await fillMeilisearchCredentials(page, meilisearchSource);
    await discoverAndSelectMeilisearchSource(page, meilisearchSource);

    await page.getByLabel(/Target Index \(Flapjack\)/).fill(dryRunTargetIndex);
    const previewButton = page.getByRole('button', { name: /preview migration/i });
    await expect(previewButton).toBeVisible();
    await expect(previewButton).toBeEnabled();
    await expectMigrationWriteUnavailable(page, 'Meilisearch', dryRunTargetIndex);
    await previewButton.click();

    await expectMigrationPreviewReport(page, MEILISEARCH_PREVIEW_ORACLE);
    await assertIndexNotCreated(request, dryRunTargetIndex);
    await expect(page.getByRole('button', { name: /^submit migration$/i })).toBeEnabled();
  });
});
