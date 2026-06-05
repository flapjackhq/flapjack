/**
 * E2E-UI Full Suite — Hybrid Search Controls (Real Server)
 *
 * NON-MOCKED SIMULATED-HUMAN REAL-BROWSER TESTS.
 * Tests run against a REAL Flapjack server with seeded test data.
 *
 * Uses a dedicated index (e2e-hybrid) to avoid race conditions with
 * vector-settings tests that run in parallel on the shared e2e-products index.
 *
 * Covers:
 * - Hybrid controls hidden when no embedders configured
 * - Hybrid controls visible when embedders configured
 * - Semantic ratio slider label updates
 * - Search results appear when hybrid search is active
 */
import type { APIRequestContext, Page } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import { waitForSearchResultsOrEmptyState } from '../helpers';
import {
  addDocuments,
  createIndex,
  deleteIndex,
  configureEmbedder,
  clearEmbedders,
  addDocumentsWithVectors,
  isVectorSearchEnabled,
  searchIndex,
  waitForEmbedder,
  waitForNoEmbedders,
} from '../../fixtures/api-helpers';

function extractObjectIds(hits: unknown[] | undefined): string[] {
  if (!Array.isArray(hits)) {
    return [];
  }

  return hits
    .map((hit) => {
      if (!hit || typeof hit !== 'object') {
        return '';
      }
      const objectID = (hit as Record<string, unknown>).objectID;
      return typeof objectID === 'string' ? objectID : '';
    })
    .filter((value): value is string => value.length > 0);
}

async function waitForQueryHit(
  request: APIRequestContext,
  indexName: string,
  query: string,
  expectedObjectId?: string,
): Promise<void> {
  await expect(async () => {
    const response = await searchIndex(request, indexName, query);
    const objectIds = extractObjectIds(response.hits);
    if (expectedObjectId) {
      expect(objectIds).toContain(expectedObjectId);
      return;
    }
    expect(response.nbHits ?? 0).toBeGreaterThan(0);
  }).toPass({ timeout: 15_000 });
}

async function expectHybridControlsUnavailable(page: Page): Promise<void> {
  await waitForSearchResultsOrEmptyState(page);
  await expect(page.getByTestId('hybrid-controls')).not.toBeVisible();
  await expect(page.getByTestId('vector-status-badge-disabled')).not.toBeVisible();
}

test.describe('Hybrid Search Controls', () => {
  // Tests modify shared index settings — must run serially (not in parallel)
  test.describe.configure({ mode: 'serial' });
  let hybridIndex = '';
  let vectorSearchEnabled = true;

  test.beforeAll(async ({ request }) => {
    const uniqueSuffix = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    hybridIndex = `e2e-hybrid-${uniqueSuffix}`;
    vectorSearchEnabled = await isVectorSearchEnabled(request);

    await deleteIndex(request, hybridIndex);
    await createIndex(request, hybridIndex);

    await addDocuments(request, hybridIndex, [
      { objectID: 'h-1', name: 'Hybrid Doc Alpha', category: 'Test' },
      { objectID: 'h-2', name: 'Hybrid Doc Beta', category: 'Test' },
    ]);
    await waitForQueryHit(request, hybridIndex, 'hybrid');

    // Explicitly clear embedders and verify persistence before UI assertions.
    await clearEmbedders(request, hybridIndex);
    await waitForNoEmbedders(request, hybridIndex);
  });

  test.afterAll(async ({ request }) => {
    if (hybridIndex) {
      await deleteIndex(request, hybridIndex);
    }
  });

  test('hybrid controls hidden when no embedders configured', async ({
    request,
    page,
  }) => {
    // Index has no embedders configured
    await waitForNoEmbedders(request, hybridIndex);
    await page.goto(`/index/${hybridIndex}`);
    await waitForSearchResultsOrEmptyState(page);

    // Hybrid controls should NOT be visible
    await expect(page.getByTestId('hybrid-controls')).not.toBeVisible();
  });

  test('hybrid controls stay hidden when vector capability is compiled out', async ({
    request,
    page,
  }) => {
    await configureEmbedder(request, hybridIndex, 'default', {
      source: 'userProvided',
      dimensions: 384,
    });
    await waitForEmbedder(request, hybridIndex, 'default');

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

    await page.goto(`/index/${hybridIndex}`);
    await waitForSearchResultsOrEmptyState(page);
    await expect(page.getByTestId('hybrid-controls')).not.toBeVisible();
  });

  test('disabled capability helper waits for browse results before passing', async ({
    request,
    page,
  }) => {
    await configureEmbedder(request, hybridIndex, 'default', {
      source: 'userProvided',
      dimensions: 384,
    });
    await waitForEmbedder(request, hybridIndex, 'default');

    let searchRequestCompleted = false;
    await page.route(`**/1/indexes/${hybridIndex}/query`, async (route) => {
      await new Promise<void>((resolve) => setTimeout(resolve, 250));
      await route.continue();
      searchRequestCompleted = true;
    });

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

    await page.goto(`/index/${hybridIndex}`);
    await expectHybridControlsUnavailable(page);

    expect(searchRequestCompleted).toBe(true);
  });

  test('hybrid controls visible when embedders configured', async ({
    request,
    page,
  }) => {
    // Seed embedder
    await configureEmbedder(request, hybridIndex, 'default', {
      source: 'userProvided',
      dimensions: 384,
    });
    await waitForEmbedder(request, hybridIndex, 'default');

    await page.goto(`/index/${hybridIndex}`);
    await waitForSearchResultsOrEmptyState(page);

    if (!vectorSearchEnabled) {
      await expectHybridControlsUnavailable(page);
      return;
    }

    // Hybrid controls should be visible
    await expect(page.getByTestId('hybrid-controls')).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByText('Hybrid Search')).toBeVisible();
    await expect(page.getByTestId('semantic-ratio-slider')).toBeVisible();
    await expect(page.getByTestId('semantic-ratio-label')).toBeVisible();
    await expect(page.getByTestId('semantic-ratio-label')).toHaveText(
      'Balanced',
    );
  });

  test('semantic ratio slider updates label', async ({ request, page }) => {
    // Ensure embedder is configured (may already be from previous test)
    await configureEmbedder(request, hybridIndex, 'default', {
      source: 'userProvided',
      dimensions: 384,
    });
    await waitForEmbedder(request, hybridIndex, 'default');

    await page.goto(`/index/${hybridIndex}`);
    if (!vectorSearchEnabled) {
      await expectHybridControlsUnavailable(page);
      return;
    }
    await expect(page.getByTestId('hybrid-controls')).toBeVisible({ timeout: 15_000 });

    // Change slider value to 1.0 (semantic only)
    const slider = page.getByTestId('semantic-ratio-slider');
    await slider.fill('1');

    // Label should update
    await expect(page.getByTestId('semantic-ratio-label')).toHaveText(
      'Semantic only',
    );

    // Change to 0 (keyword only)
    await slider.fill('0');
    await expect(page.getByTestId('semantic-ratio-label')).toHaveText(
      'Keyword only',
    );
  });

  test('search results appear with hybrid search active', async ({
    request,
    page,
  }) => {
    // Seed embedder + docs with vectors
    await configureEmbedder(request, hybridIndex, 'default', {
      source: 'userProvided',
      dimensions: 384,
    });
    await waitForEmbedder(request, hybridIndex, 'default');

    await addDocumentsWithVectors(request, hybridIndex, [
      {
        objectID: 'vec-1',
        name: 'Vector Laptop',
        category: 'Laptops',
        _vectors: { default: new Array(384).fill(0.1) },
      },
      {
        objectID: 'vec-2',
        name: 'Vector Phone',
        category: 'Phones',
        _vectors: { default: new Array(384).fill(0.2) },
      },
    ]);
    await waitForQueryHit(request, hybridIndex, 'laptop', 'vec-1');

    await page.goto(`/index/${hybridIndex}`);
    if (!vectorSearchEnabled) {
      await expectHybridControlsUnavailable(page);
      return;
    }
    await expect(page.getByTestId('hybrid-controls')).toBeVisible({ timeout: 15_000 });

    // Perform a search
    const searchInput = page.getByPlaceholder(/search documents/i);
    await searchInput.fill('laptop');
    await searchInput.press('Enter');

    // Verify actual result content appears (not just that the panel exists)
    await expect(page.getByText('Vector Laptop')).toBeVisible({
      timeout: 10_000,
    });
  });
});
