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
import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import type { APIRequestContext, Page, Response } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import { waitForSearchResultsOrEmptyState } from '../helpers';
import {
  P20_TEXT_ONLY_CONTROL_TEST_TITLE,
  installTextOnlyNegativeControlCapability,
  isP20TextOnlyNegativeControl,
  waitForTextOnlyNegativeControlReadiness,
} from '../../fixtures/p20_negative_control';
import {
  addDocumentsWithVectors,
  configureEmbedder,
  createIndex,
  deleteIndex,
  getSettings,
  readEmbeddersFromSettings,
  searchIndex,
  skipWhenVectorSearchDisabled,
  updateSettings,
  waitForEmbedder,
  waitForEmbedderRemoval,
  waitForSearchableObjectIds,
} from '../../fixtures/api-helpers';

const VECTOR_ENABLED_MESSAGE = 'Vector-settings e2e flows require a vector-search-enabled build';
const VECTOR_PROOF_QUERY = 'ergonomic posture';
const VECTOR_PROOF_TARGET_ID = 'semantic-chair';
const VECTOR_PROOF_TARGET_NAME = 'Lumbar Support Chair';
const VECTOR_PROOF_KEYWORD_DECOY_ID = 'keyword-decoy';
const VECTOR_PROOF_KEYWORD_DECOY_NAME = 'Ergonomic Posture Keyword Decoy';
const VECTOR_PROOF_STANDING_DESK_ID = 'standing-desk';
const VECTOR_PROOF_STANDING_DESK_NAME = 'Standing Desk Converter';
const VECTOR_PROOF_SUPPORTING_IDS = [
  VECTOR_PROOF_KEYWORD_DECOY_ID,
  VECTOR_PROOF_STANDING_DESK_ID,
];
const VECTOR_PROOF_TARGET_DOCUMENTS = [
  {
    objectID: VECTOR_PROOF_TARGET_ID,
    name: VECTOR_PROOF_TARGET_NAME,
    description: 'Adjustable seating with lower-back support for long focus sessions.',
    _vectors: { default: [0.99, 0.05, 0] },
  },
];
const VECTOR_PROOF_SUPPORTING_DOCUMENTS = [
  {
    objectID: VECTOR_PROOF_KEYWORD_DECOY_ID,
    name: VECTOR_PROOF_KEYWORD_DECOY_NAME,
    description: 'Keyword-heavy document intentionally far from the semantic query vector.',
    _vectors: { default: [0, 0, 1] },
  },
  {
    objectID: VECTOR_PROOF_STANDING_DESK_ID,
    name: VECTOR_PROOF_STANDING_DESK_NAME,
    description: 'Workspace riser for alternating between sitting and standing.',
    _vectors: { default: [0, 1, 0] },
  },
];
const P20_TEXT_ONLY_NEGATIVE_CONTROL = isP20TextOnlyNegativeControl();

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

function vectorForText(text: string): number[] {
  return text.toLowerCase().includes(VECTOR_PROOF_QUERY) ? [1, 0, 0] : [0, 0, 1];
}

async function readRequestBody(request: NodeJS.ReadableStream): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of request) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  return Buffer.concat(chunks).toString('utf8');
}

function parseEmbedderInput(body: string): string {
  try {
    const parsed = JSON.parse(body) as { input?: unknown };
    return typeof parsed.input === 'string' ? parsed.input : '';
  } catch {
    return '';
  }
}

async function startDeterministicEmbedder(): Promise<{ server: Server; url: string }> {
  const server = createServer(async (request, response) => {
    const input = parseEmbedderInput(await readRequestBody(request));
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ embedding: vectorForText(input) }));
  });

  await new Promise<void>((resolve) => {
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address() as AddressInfo;
  return { server, url: `http://127.0.0.1:${address.port}/embed` };
}

async function stopDeterministicEmbedder(server: Server): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
    server.closeAllConnections();
  });
}

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

async function expectKeywordOnlyMissesVectorTarget(
  request: APIRequestContext,
  indexName: string,
): Promise<void> {
  await expect(async () => {
    const response = await searchIndex(request, indexName, VECTOR_PROOF_QUERY, {
      hitsPerPage: 10,
      mode: 'keywordSearch',
    });
    expect(extractObjectIds(response.hits)).not.toContain(VECTOR_PROOF_TARGET_ID);
  }).toPass({ timeout: 15_000 });
}

async function waitForP20TargetReadiness(
  request: APIRequestContext,
  indexName: string,
): Promise<void> {
  if (P20_TEXT_ONLY_NEGATIVE_CONTROL) {
    await waitForTextOnlyNegativeControlReadiness({
      request,
      indexName,
      targetObjectId: VECTOR_PROOF_TARGET_ID,
    });
    return;
  }

  await waitForSearchableObjectIds(
    request,
    indexName,
    VECTOR_PROOF_QUERY,
    [VECTOR_PROOF_TARGET_ID],
    { hitsPerPage: 10, mode: 'neuralSearch' },
  );
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
      await route.fulfill({
        status: 200,
        json: {
          status: 'ok',
          capabilities: { vectorSearch: false, vectorSearchLocal: false },
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
      // The text-only negative control stubs the browser capability answer for its own single
      // test only; every other test keeps the real capability gate and skips when disabled.
      if (
        P20_TEXT_ONLY_NEGATIVE_CONTROL &&
        testInfo.title === P20_TEXT_ONLY_CONTROL_TEST_TITLE
      ) {
        await installTextOnlyNegativeControlCapability(page);
      } else {
        await skipWhenVectorSearchDisabled(request, testInfo, VECTOR_ENABLED_MESSAGE);
      }

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

    test('set search mode to Neural Search and verify persistence', async ({ page, request }) => {
      test.setTimeout(60_000);
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

      const embedder = await startDeterministicEmbedder();
      try {
        await configureEmbedder(request, vectorTestIndex, 'default', {
          source: 'rest',
          url: embedder.url,
          dimensions: 3,
          request: { input: '{{text}}' },
          response: { embedding: '{{embedding}}' },
        });
        await waitForEmbedder(request, vectorTestIndex, 'default');
        await addDocumentsWithVectors(
          request,
          vectorTestIndex,
          VECTOR_PROOF_SUPPORTING_DOCUMENTS,
        );
        await waitForSearchableObjectIds(
          request,
          vectorTestIndex,
          '',
          VECTOR_PROOF_SUPPORTING_IDS,
          { hitsPerPage: 10, mode: 'keywordSearch' },
        );
        await addDocumentsWithVectors(
          request,
          vectorTestIndex,
          VECTOR_PROOF_TARGET_DOCUMENTS,
        );
        await waitForP20TargetReadiness(request, vectorTestIndex);
        await expectKeywordOnlyMissesVectorTarget(request, vectorTestIndex);

        await page.goto(`/index/${vectorTestIndex}`);
        await expect(page.getByTestId('hybrid-controls')).toBeVisible({
          timeout: 15_000,
        });
        await page.getByTestId('semantic-ratio-slider').fill('1');
        await expect(page.getByTestId('semantic-ratio-label')).toHaveText(
          'Semantic only',
        );

        const searchInput = page.getByPlaceholder(/search documents/i);
        await searchInput.fill(VECTOR_PROOF_QUERY);
        await searchInput.press('Enter');

        const semanticResult = page
          .getByTestId('results-panel')
          .getByTestId('document-card')
          .filter({ hasText: VECTOR_PROOF_TARGET_ID });
        await expect(semanticResult).toBeVisible({ timeout: 15_000 });
        await expect(semanticResult).toContainText(VECTOR_PROOF_TARGET_NAME);
      } finally {
        await stopDeterministicEmbedder(embedder.server);
      }
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
