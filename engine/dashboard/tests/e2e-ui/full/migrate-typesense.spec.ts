import { test, expect } from '../../fixtures/auth.fixture';
import type { Page } from '@playwright/test';
import {
  expectMigrationPreviewReport,
  expectMigrationWriteUnavailable,
  type MigrationPreviewOracle,
} from '../../fixtures/migration-preview.fixture';
import {
  assertIndexNotCreated,
  cleanupMigratedIndexes,
  cleanupSourceContainer,
  startTypesenseSource,
  type SourceProviderContext,
} from '../../fixtures/source-provider.fixture';

const targetIndex = `fj_e2e_typesense_migration_${Date.now()}`;
const dryRunTargetIndex = `fj_e2e_typesense_dry_run_${Date.now()}`;
const TYPESENSE_PREVIEW_ORACLE: MigrationPreviewOracle = {
  summary: {
    totalEntries: 12,
    hardRejections: 0,
    warnings: 7,
    scopeGaps: 5,
  },
  entry: {
    severity: 'Warning',
    code: 'TypesenseSettingNotMigrated',
    resource: 'Settings',
    jsonPath: '$.symbols_to_index',
  },
};

async function expectAsyncMigrationContract(page: Page): Promise<void> {
  await expect(page.getByText(/Job ID/i)).toBeVisible();
  await expect(page.getByText(/Phase/i)).toBeVisible();
  await expect(page.getByText(/Disposition/i)).toBeVisible();
}

async function fillTypesenseCredentials(
  page: Page,
  typesenseSource: SourceProviderContext,
): Promise<void> {
  const providerControl = page.getByRole('button', { name: 'Typesense', exact: true });
  await expect(providerControl).toBeVisible();
  await providerControl.click();
  await page.getByLabel('Node URL', { exact: true }).fill(typesenseSource.endpoint);
  await page.getByLabel('API Key', { exact: true }).fill(typesenseSource.apiKey);
}

async function discoverAndSelectTypesenseSource(
  page: Page,
  typesenseSource: SourceProviderContext,
): Promise<void> {
  await page.getByRole('button', { name: 'Discover sources', exact: true }).click();
  await page.getByLabel('Source collection', { exact: true }).click();
  const sourceOption = page.getByRole('option', { name: new RegExp(`${typesenseSource.sourceName}.*2`) });
  await expect(sourceOption).toContainText(typesenseSource.sourceName);
  await sourceOption.click();
}

async function attestTypesenseWriteFreeze(page: Page): Promise<void> {
  const attestation = page.getByTestId('typesense-source-write-frozen');
  await expect(attestation).toBeVisible();
  await expect(attestation).not.toBeChecked();
  await attestation.check();
}

async function requireWriteFreezeControl(page: Page) {
  const previewButton = page.getByRole('button', { name: /preview migration/i });
  const attestation = page.getByTestId('typesense-source-write-frozen');
  if (await attestation.count() === 0) {
    const previewResponse = page.waitForResponse((response) => (
      response.request().method() === 'POST'
      && response.url().endsWith('/1/migrations/typesense/preview')
    ));
    await previewButton.click();
    await previewResponse;
    console.error('WRITE_FREEZE_BROWSER_RED=unchecked_control_sent_request');
    throw new Error('Typesense preview dispatched without a write-freeze control');
  }

  return { attestation, previewButton };
}

test.describe('Typesense migration (real browser)', () => {
  let source: SourceProviderContext | undefined;

  test.describe.configure({ timeout: 120_000 });

  function requireSource(): SourceProviderContext {
    if (!source) {
      throw new Error('Typesense source fixture was not started');
    }
    return source;
  }

  test.beforeAll(async ({ request }) => {
    source = await startTypesenseSource(request);
  });

  test.afterAll(async ({ request }) => {
    await cleanupMigratedIndexes(request, [targetIndex, dryRunTargetIndex]);
    if (source) {
      await cleanupSourceContainer(source);
    }
  });

  test('does not dispatch preview while the Typesense write freeze is unchecked', async ({ page }) => {
    const typesenseSource = requireSource();

    await page.goto('/migrate');
    await fillTypesenseCredentials(page, typesenseSource);
    await discoverAndSelectTypesenseSource(page, typesenseSource);
    await page.getByLabel(/Target Index \(Flapjack\)/).fill(dryRunTargetIndex);

    const { attestation, previewButton } = await requireWriteFreezeControl(page);
    await expect(attestation).toHaveAccessibleName(
      /I have paused writes to the selected Typesense collection for the complete migration/i,
    );
    await expect(attestation).not.toBeChecked();
    await expect(previewButton).toBeDisabled();
  });

  test('migrate Typesense collection via UI: discover → migrate → verify success → browse', async ({ page }) => {
    const typesenseSource = requireSource();

    await page.goto('/migrate');
    await expect(page.getByRole('heading', { name: /migrate/i })).toContainText('Migrate');
    await fillTypesenseCredentials(page, typesenseSource);
    await discoverAndSelectTypesenseSource(page, typesenseSource);

    await page.getByLabel(/Target Index \(Flapjack\)/).fill(targetIndex);
    await attestTypesenseWriteFreeze(page);
    await page.getByRole('button', { name: /preview migration/i }).click();
    await expectMigrationPreviewReport(page, TYPESENSE_PREVIEW_ORACLE);
    await page.getByRole('button', { name: /^submit migration$/i }).click();

    await expectAsyncMigrationContract(page);
    await expect(page.getByText('Migration complete', { exact: true })).toBeVisible({ timeout: 90_000 });
    await expect(page.getByTestId('migrate-stat-documents')).toHaveText('2');
    await page.getByRole('link', { name: 'Browse Index', exact: true }).click();
    await expect(page).toHaveURL(new RegExp(`/index/${targetIndex}$`));

    await page.getByPlaceholder(/search documents/i).fill('Espresso');
    await page.getByPlaceholder(/search documents/i).press('Enter');
    const seededDocument = page.getByTestId('document-card').filter({ hasText: 'prod_1' });
    await expect(seededDocument).toHaveCount(1, { timeout: 10_000 });
    await expect(seededDocument).toContainText('Espresso');
  });

  test('dry-run preview shows Typesense warning and scope-gap report @typesense-loopback-opt-in', async ({ page, request }) => {
    const typesenseSource = requireSource();

    await page.goto('/migrate');
    await expect(page.getByRole('heading', { name: /migrate/i })).toContainText('Migrate');
    await fillTypesenseCredentials(page, typesenseSource);
    await discoverAndSelectTypesenseSource(page, typesenseSource);

    await page.getByLabel(/Target Index \(Flapjack\)/).fill(dryRunTargetIndex);
    await attestTypesenseWriteFreeze(page);
    const previewButton = page.getByRole('button', { name: /preview migration/i });
    await expect(previewButton).toBeVisible();
    await expect(previewButton).toBeEnabled();
    await expectMigrationWriteUnavailable(page, 'Typesense', dryRunTargetIndex);
    await previewButton.click();

    await expectMigrationPreviewReport(page, TYPESENSE_PREVIEW_ORACLE);
    await assertIndexNotCreated(request, dryRunTargetIndex);
    await expect(page.getByRole('button', { name: /^submit migration$/i })).toBeEnabled();
  });
});
