import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures/auth.fixture';
import {
  expectMigrationWriteUnavailable,
  migrationWriteActionName,
} from '../fixtures/migration-preview.fixture';
import {
  cleanupMigratedIndexes,
  cleanupSourceContainer,
  startMeilisearchSource,
  type SourceProviderContext,
} from '../fixtures/source-provider.fixture';

const refusedTargetIndex = `fj_e2e_meilisearch_migration_refused_${Date.now()}`;

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

test.describe('Meilisearch migration loopback refusal (real browser)', () => {
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
    await cleanupMigratedIndexes(request, [refusedTargetIndex]);
    if (source) {
      await cleanupSourceContainer(source);
    }
  });

  test('loopback write remains unavailable before preview @meilisearch-loopback-refusal', async ({ page }) => {
    const meilisearchSource = requireSource();

    await page.goto('/migrate');
    await fillMeilisearchCredentials(page, meilisearchSource);
    await page.getByLabel('Source index', { exact: true }).fill(meilisearchSource.sourceName);
    await page.getByLabel(/Target Index \(Flapjack\)/).fill(refusedTargetIndex);

    await expect(page.getByRole('button', { name: /preview migration/i })).toBeEnabled();
    await expectMigrationWriteUnavailable(page, 'Meilisearch', refusedTargetIndex);
    await expect(page.getByRole('button', {
      name: migrationWriteActionName('Meilisearch', refusedTargetIndex),
    })).toHaveCount(0);
  });

  test('preview loopback refusal names the Meilisearch opt-in required to continue @meilisearch-loopback-refusal', async ({ page }) => {
    const meilisearchSource = requireSource();

    await page.goto('/migrate');
    await fillMeilisearchCredentials(page, meilisearchSource);
    await page.getByLabel('Source index', { exact: true }).fill(meilisearchSource.sourceName);
    await page.getByLabel(/Target Index \(Flapjack\)/).fill(refusedTargetIndex);

    const previewButton = page.getByRole('button', { name: /preview migration/i });
    await expect(previewButton).toBeVisible();
    await expect(previewButton).toBeEnabled();
    await previewButton.click();

    const errorCard = page.getByTestId('migration-error-card');
    await expect(errorCard).toContainText('Meilisearch preview loopback endpoint is disabled');
    await expect(errorCard).toContainText('FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=1');
  });
});
