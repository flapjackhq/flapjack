/**
 * E2E-UI Full Suite — Vector Search Settings (Real Server)
 *
 * NON-MOCKED SIMULATED-HUMAN REAL-BROWSER TESTS.
 * Tests run against a REAL Flapjack server with seeded test data.
 *
 * Covers:
 * - Search mode section display and mode switching
 * - Embedder configuration via Add Embedder dialog
 * - Embedder deletion via confirm dialog
 * - Settings persistence after save + reload
 */
import type { Page, Response } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import { waitForSearchResultsOrEmptyState } from '../helpers';
import {
  configureEmbedder,
  createIndex,
  deleteIndex,
  getSettings,
  readEmbeddersFromSettings,
  skipWhenVectorSearchDisabled,
  updateSettings,
  waitForEmbedder,
  waitForEmbedderRemoval,
} from '../../fixtures/api-helpers';

const VECTOR_ENABLED_MESSAGE = 'Vector-settings e2e flows require a vector-search-enabled build';

async function openVectorTab(page: Page) {
  await page.getByRole('tab', { name: 'Vector / AI' }).click();
  await expect(page.getByRole('tabpanel', { name: 'Vector / AI' })).toBeVisible({
    timeout: 10_000,
  });
}

function isSettingsUpdateResponse(response: Response, indexName: string): boolean {
  return (
    response.request().method() === 'PUT' &&
    response.url().includes(`/indexes/${indexName}/settings`) &&
    [200, 202].includes(response.status())
  );
}

async function saveVectorSettings(page: Page, indexName: string): Promise<void> {
  const saveButton = page.getByRole('button', { name: /save/i });
  await expect(saveButton).toBeVisible({ timeout: 5_000 });
  const saveResponsePromise = page.waitForResponse(
    (response) => isSettingsUpdateResponse(response, indexName),
    { timeout: 15_000 },
  );
  await saveButton.click();
  await saveResponsePromise;
}

test.describe('Vector Search Settings', () => {
  // Tests mutate a dedicated index and still run serially to avoid cross-test state races.
  test.describe.configure({ mode: 'serial' });

  let vectorTestIndex = '';
  let originalSettings: Record<string, unknown> | undefined;

  const getOriginalVectorMode = (): string => {
    const originalMode = originalSettings?.mode;
    return typeof originalMode === 'string' ? originalMode : 'keywordSearch';
  };

  const getOriginalEmbedders = (): Record<string, unknown> => {
    return readEmbeddersFromSettings(originalSettings ?? {});
  };

  test.beforeAll(async ({ request }) => {
    const uniqueSuffix = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    vectorTestIndex = `e2e-vector-settings-${uniqueSuffix}`;
    await deleteIndex(request, vectorTestIndex);
    await createIndex(request, vectorTestIndex);
  });

  test.afterAll(async ({ request }) => {
    if (vectorTestIndex) {
      await deleteIndex(request, vectorTestIndex);
    }
  });

  // ---- Load-and-verify (10.21 vector-settings-1) ----

  test('shows compiled-out messaging when vector capability is disabled', async ({
    page,
  }) => {
    await page.route('**/health', async (route) => {
      const response = await route.fetch();
      const health = await response.json();
      await route.fulfill({
        response,
        json: {
          ...health,
          capabilities: {
            ...health.capabilities,
            vectorSearch: false,
            vectorSearchLocal: false,
          },
        },
      });
    });

    await page.goto(`/index/${vectorTestIndex}/settings`);
    await expect(
      page.getByRole('heading', { name: /settings/i }),
    ).toBeVisible({ timeout: 10_000 });
    await openVectorTab(page);

    await expect(page.getByTestId('search-mode-compiled-out-warning')).toBeVisible();
    await expect(page.getByTestId('embedder-panel-compiled-out')).toBeVisible();
    await expect(
      page.getByTestId('embedder-panel').getByText('No embedders configured'),
    ).toBeHidden();
  });

  test.describe('vector-enabled settings flows', () => {
    test.beforeEach(async ({ request, page }, testInfo) => {
      await skipWhenVectorSearchDisabled(request, testInfo, VECTOR_ENABLED_MESSAGE);

      // Save original settings for cleanup
      originalSettings = await getSettings(request, vectorTestIndex);

      // Seed a userProvided embedder for tests that need existing embedders
      await configureEmbedder(request, vectorTestIndex, 'default', {
        source: 'userProvided',
        dimensions: 384,
      });

      await page.goto(`/index/${vectorTestIndex}/settings`);
      await expect(
        page.getByRole('heading', { name: /settings/i }),
      ).toBeVisible({ timeout: 10_000 });
      await openVectorTab(page);
    });

    test.afterEach(async ({ request }) => {
      if (!originalSettings) {
        return;
      }
      // Restore only the vector fields this spec mutates so we do not
      // overwrite unrelated settings from concurrent specs.
      try {
        await updateSettings(request, vectorTestIndex, {
          embedders: getOriginalEmbedders(),
          mode: getOriginalVectorMode(),
        });
      } finally {
        originalSettings = undefined;
      }
    });

    test('displays search mode and embedders sections with seeded data', async ({
      page,
    }) => {
      // Search Mode section
      await expect(page.getByTestId('search-mode-select')).toBeVisible({
        timeout: 10_000,
      });

      // Embedders section
      await expect(page.getByText('Embedders').first()).toBeVisible();
      await expect(
        page.getByText('Configure embedding models for vector search'),
      ).toBeVisible();

      // Seeded embedder card
      await expect(page.getByTestId('embedder-card-default')).toBeVisible();
      await expect(
        page.getByTestId('embedder-card-default').getByText('userProvided'),
      ).toBeVisible();
      await expect(
        page.getByTestId('embedder-card-default').getByText('384'),
      ).toBeVisible();
    });

    // ---- Set search mode (10.21 vector-settings-2) ----

    test('set search mode to Neural Search and verify persistence', async ({ page }) => {
      await expect(page.getByTestId('search-mode-select')).toBeVisible({
        timeout: 10_000,
      });

      // Select Neural Search
      await page.getByTestId('search-mode-select').selectOption('neuralSearch');

      await saveVectorSettings(page, vectorTestIndex);

      // Reload and verify persistence
      await page.reload();
      await expect(
        page.getByRole('heading', { name: /settings/i }),
      ).toBeVisible({ timeout: 10_000 });
      await openVectorTab(page);
      await expect(page.getByTestId('search-mode-select')).toHaveValue(
        'neuralSearch',
        { timeout: 10_000 },
      );
    });

    // ---- Add embedder (10.21 vector-settings-3) ----

    test('add userProvided embedder via dialog', async ({ page, request }) => {
      await expect(page.getByTestId('add-embedder-btn')).toBeVisible({
        timeout: 10_000,
      });

      // Click Add Embedder
      await page.getByTestId('add-embedder-btn').click();

      // Dialog should open
      await expect(page.getByTestId('embedder-dialog')).toBeVisible({
        timeout: 5_000,
      });

      // Fill form
      await page.getByTestId('embedder-name-input').fill('test-emb');
      await page.getByTestId('embedder-source-select').selectOption('userProvided');
      await page.getByTestId('embedder-dimensions-input').fill('384');

      // Save in dialog
      await page.getByTestId('embedder-save-btn').click();

      // Dialog should close, new card should appear
      await expect(page.getByTestId('embedder-dialog')).not.toBeVisible({
        timeout: 5_000,
      });
      await expect(page.getByTestId('embedder-card-test-emb')).toBeVisible();

      await saveVectorSettings(page, vectorTestIndex);
      await waitForEmbedder(request, vectorTestIndex, 'test-emb');

      // Reload and verify persistence
      await page.reload();
      await expect(
        page.getByRole('heading', { name: /settings/i }),
      ).toBeVisible({ timeout: 10_000 });
      await openVectorTab(page);
      await expect(page.getByTestId('embedder-card-test-emb')).toBeVisible({
        timeout: 10_000,
      });
    });

    // ---- Delete embedder (10.21 vector-settings-5) ----

    test('delete an embedder via confirm dialog', async ({ page, request }) => {
      // Verify seeded embedder exists
      await expect(page.getByTestId('embedder-card-default')).toBeVisible({
        timeout: 10_000,
      });

      // Click delete button
      await page.getByTestId('embedder-delete-default').click();

      // Confirm dialog should appear
      await expect(
        page.getByRole('heading', { name: /delete embedder/i }),
      ).toBeVisible({ timeout: 5_000 });
      await page.getByRole('button', { name: 'Confirm' }).click();

      // Card should disappear
      await expect(
        page.getByTestId('embedder-card-default'),
      ).not.toBeVisible({ timeout: 5_000 });

      await saveVectorSettings(page, vectorTestIndex);
      await waitForEmbedderRemoval(request, vectorTestIndex, 'default');

      // Reload and verify persistence
      await page.reload();
      await expect(
        page.getByRole('heading', { name: /settings/i }),
      ).toBeVisible({ timeout: 10_000 });
      await openVectorTab(page);
      // Should show "No embedders configured" in the embedder panel
      // (scoped to avoid matching the SearchModeSection warning badge)
      await expect(
        page.getByTestId('embedder-panel').getByText('No embedders configured'),
      ).toBeVisible({ timeout: 10_000 });
    });

    // ---- Persistence (10.21 vector-settings-6) ----

    test('embedder settings persist after save and navigation', async ({ page, request }) => {
      await expect(page.getByTestId('add-embedder-btn')).toBeVisible({
        timeout: 10_000,
      });

      // Add a new embedder
      await page.getByTestId('add-embedder-btn').click();
      await expect(page.getByTestId('embedder-dialog')).toBeVisible({
        timeout: 5_000,
      });
      await page.getByTestId('embedder-name-input').fill('persist-test');
      await page.getByTestId('embedder-source-select').selectOption('userProvided');
      await page.getByTestId('embedder-dimensions-input').fill('256');
      await page.getByTestId('embedder-save-btn').click();
      await expect(page.getByTestId('embedder-dialog')).not.toBeVisible({
        timeout: 5_000,
      });

      await saveVectorSettings(page, vectorTestIndex);
      await waitForEmbedder(request, vectorTestIndex, 'persist-test');

      // Navigate away to search page
      await page.goto(`/index/${vectorTestIndex}`);
      await waitForSearchResultsOrEmptyState(page);

      // Navigate back to settings
      await page.goto(`/index/${vectorTestIndex}/settings`);
      await expect(
        page.getByRole('heading', { name: /settings/i }),
      ).toBeVisible({ timeout: 10_000 });
      await openVectorTab(page);

      // Verify both embedders still present
      await expect(page.getByTestId('embedder-card-default')).toBeVisible({
        timeout: 10_000,
      });
      await expect(page.getByTestId('embedder-card-persist-test')).toBeVisible();
    });
  });
});
