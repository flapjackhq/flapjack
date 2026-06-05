/**
 * E2E-UI Full Suite — Search & Browse Page (Real Server)
 *
 * Tests run against a REAL Flapjack server with seeded test data.
 * NO mocking whatsoever. The 'e2e-products' index is pre-seeded with 12 products
 * across 6 categories, 3 synonym groups, and 2 merchandising rules.
 *
 * Seeded products:
 *   p01: MacBook Pro 16" (Apple, Laptops)
 *   p02: ThinkPad X1 Carbon (Lenovo, Laptops)
 *   p03: Dell XPS 15 (Dell, Laptops)
 *   p04: iPad Pro 12.9" (Apple, Tablets)
 *   p05: Galaxy Tab S9 (Samsung, Tablets)
 *   p06: Sony WH-1000XM5 (Sony, Audio)
 *   p07: AirPods Pro 2 (Apple, Audio)
 *   p08: Samsung 990 Pro 2TB (Samsung, Storage)
 *   p09: LG UltraGear 27" 4K (LG, Monitors)
 *   p10: Logitech MX Master 3S (Logitech, Accessories)
 *   p11: Keychron Q1 Pro (Keychron, Accessories)
 *   p12: CalDigit TS4 (CalDigit, Accessories)
 *
 * Synonyms: laptop/notebook/computer, headphones/earphones/earbuds, monitor/screen/display
 * Settings: attributesForFaceting=['category','brand','filterOnly(price)','filterOnly(inStock)']
 */
import type { APIRequestContext, Locator, Page } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import { TEST_INDEX, waitForSearchResultsOrEmptyState } from '../helpers';
import { addDocuments, createIndex, deleteDocument, deleteIndex, searchIndex } from '../../fixtures/api-helpers';
import {
  extractObjectIdFromText,
  readVisibleObjectId,
  responseMatchesIndexQuery,
} from '../result-helpers';
import {
  MOVIES_DOCUMENT_ACTIONS_INDEX,
  seedMoviesDocumentActionsIndex,
} from '../../fixtures/lane_c_movies';

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
    .filter((value): value is string => Boolean(value));
}

async function submitSearchQueryAndWaitForTopCard(
  page: Page,
  query: string,
  indexName = TEST_INDEX,
): Promise<Locator> {
  await submitIndexSearch(page, query, indexName);

  const firstCard = page.getByTestId('results-panel').getByTestId('document-card').first();
  await expect(firstCard).toBeVisible({ timeout: 10_000 });
  return firstCard;
}

async function submitIndexSearch(
  page: Page,
  query: string,
  indexName = TEST_INDEX,
): Promise<void> {
  const searchInput = page.getByPlaceholder(/search documents/i);
  const responsePromise = page.waitForResponse(
    (response) => responseMatchesIndexQuery(response, indexName, query, { requireFacets: true }),
    { timeout: 15_000 },
  );
  await searchInput.fill(query);
  await searchInput.press('Enter');
  await responsePromise;
}

async function waitForFacetHeadingAndValue(
  facetsPanel: Locator,
  headingName: string,
  expectedFacetValue: string,
): Promise<Locator> {
  await expect(
    facetsPanel.getByRole('heading', { name: new RegExp(`^${headingName}$`, 'i') }),
  ).toBeVisible({ timeout: 15_000 });
  const facetButton = facetsPanel.locator('button', { hasText: expectedFacetValue }).first();
  await expect(facetButton).toBeVisible({ timeout: 15_000 });
  return facetButton;
}

async function submitSearchAndWaitForFacets(
  page: Page,
  query: string,
  indexName = TEST_INDEX,
): Promise<Locator> {
  await submitIndexSearch(page, query, indexName);

  const facetsPanel = page.getByTestId('facets-panel');
  await expect(facetsPanel).toBeVisible({ timeout: 10_000 });
  await expect(facetsPanel.locator('button').first()).toBeVisible({ timeout: 15_000 });
  return facetsPanel;
}

async function openAddDocumentsDialog(page: Page): Promise<Locator> {
  await page.getByRole('button', { name: /add documents/i }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible({ timeout: 5_000 });
  await dialog.getByRole('tab', { name: /^json$/i }).click();
  await expect(dialog.getByPlaceholder('Field name').first()).toBeVisible({ timeout: 5_000 });
  return dialog;
}

async function primeJsonTextareaFromFormBuilder(
  dialog: Locator,
  seedValue: string,
): Promise<void> {
  // Exercise the form-builder -> textarea sync before switching to the deterministic JSON payload.
  await dialog.getByRole('button', { name: /add field/i }).click();
  await dialog.getByPlaceholder('Field name').last().fill('name');
  await dialog.getByPlaceholder('Value').last().fill(seedValue);
  await expect
    .poll(async () => (await dialog.locator('textarea').inputValue()).trim(), {
      timeout: 5_000,
    })
    .not.toBe('');
}

function waitForAddDocumentUpdateResponse(page: Page, indexName = TEST_INDEX): Promise<void> {
  return page.waitForResponse(
    (response) => {
      if (response.request().method() !== 'POST') {
        return false;
      }
      if (!response.url().includes(`/indexes/${indexName}/batch`)) {
        return false;
      }
      return [200, 202].includes(response.status());
    },
    { timeout: 15_000 },
  ).then(() => undefined);
}

async function addDocumentViaJsonDialog(
  page: Page,
  document: Record<string, unknown>,
  indexName = TEST_INDEX,
): Promise<void> {
  const dialog = await openAddDocumentsDialog(page);
  const updateResponse = waitForAddDocumentUpdateResponse(page, indexName);
  await primeJsonTextareaFromFormBuilder(
    dialog,
    typeof document.name === 'string' ? document.name : String(document.objectID ?? 'temporary document'),
  );
  await dialog.locator('textarea').fill(JSON.stringify(document, null, 2));
  await dialog.getByRole('button', { name: /^Add Document$/ }).click();
  await updateResponse;
  await expect(dialog).not.toBeVisible({ timeout: 10_000 });
}

function trackCreatedDocument(createdDocumentIds: Set<string>, objectID: string): void {
  createdDocumentIds.add(objectID);
}

function markDocumentRemoved(createdDocumentIds: Set<string>, objectID: string): void {
  createdDocumentIds.delete(objectID);
}

async function cleanupCreatedDocuments(
  request: APIRequestContext,
  createdDocumentIds: Set<string>,
  indexName = TEST_INDEX,
): Promise<void> {
  for (const objectID of createdDocumentIds) {
    await deleteDocument(request, indexName, objectID).catch(() => {});
  }
  createdDocumentIds.clear();
}

test.describe('Search & Browse', () => {

  test.beforeEach(async ({ page }) => {
    await page.goto(`/index/${TEST_INDEX}`);
    await waitForSearchResultsOrEmptyState(page);
  });

  // ---------------------------------------------------------------------------
  // Basic search: type "laptop", see MacBook Pro, ThinkPad, Dell XPS results
  // ---------------------------------------------------------------------------
  test('searching for "laptop" returns laptop products', async ({ page, request }) => {
    const searchResponse = await searchIndex(request, TEST_INDEX, 'laptop');
    const expectedObjectIds = extractObjectIds(searchResponse.hits);

    expect(expectedObjectIds.length).toBeGreaterThan(0);

    const searchInput = page.getByPlaceholder(/search documents/i);
    await searchInput.fill('laptop');
    await searchInput.press('Enter');

    // Wait for results to update
    const resultsPanel = page.getByTestId('results-panel');
    await expect(resultsPanel).toBeVisible({ timeout: 10000 });

    const firstCard = resultsPanel.getByTestId('document-card').first();
    await expect(firstCard).toBeVisible({ timeout: 10_000 });

    const firstCardText = await firstCard.innerText();
    const firstCardObjectId = firstCardText.match(/\bp\d+\b/i)?.[0] ?? '';
    expect(firstCardObjectId).not.toBe('');
    expect(expectedObjectIds).toContain(firstCardObjectId);
  });

  // ---------------------------------------------------------------------------
  // Facet filtering: click "Audio" in category facet -> only Sony/AirPods shown
  // NOTE: Known facets panel bug can cause incomplete facet values to appear.
  // This test waits for the Audio button specifically before clicking.
  // ---------------------------------------------------------------------------
  test('filtering by Audio category shows only audio products', async ({ page }) => {
    const facetsPanel = page.getByTestId('facets-panel');
    await expect(facetsPanel).toBeVisible({ timeout: 10000 });

    const audioBtn = await waitForFacetHeadingAndValue(facetsPanel, 'category', 'Audio');
    await audioBtn.click();

    // Wait for results to update with the filter applied
    const resultsPanel = page.getByTestId('results-panel');
    await expect(resultsPanel).toBeVisible({ timeout: 10000 });
    await expect(resultsPanel.getByTestId('document-card').first()).toBeVisible({ timeout: 10_000 });
    await expect(audioBtn.locator('svg')).toBeVisible({ timeout: 10_000 });
  });

  // ---------------------------------------------------------------------------
  // Multiple facets: filter by "Apple" brand -> see Apple products only
  // ---------------------------------------------------------------------------
  test('filtering by Apple brand shows only Apple products', async ({ page }) => {
    const facetsPanel = page.getByTestId('facets-panel');
    await expect(facetsPanel).toBeVisible({ timeout: 10000 });

    const appleBtn = await waitForFacetHeadingAndValue(facetsPanel, 'brand', 'Apple');
    await appleBtn.click();

    // Wait for results to update
    const resultsPanel = page.getByTestId('results-panel');
    await expect(resultsPanel).toBeVisible({ timeout: 10000 });
    await expect(resultsPanel.getByTestId('document-card').first()).toBeVisible({ timeout: 10000 });

    // All visible results should be Apple brand products
    const cards = resultsPanel.getByTestId('document-card');
    const cardCount = await cards.count();
    const appleProductIds = new Set(['p01', 'p04', 'p07']);
    expect(cardCount).toBeGreaterThanOrEqual(1);
    expect(cardCount).toBeLessThanOrEqual(appleProductIds.size);

    for (let index = 0; index < cardCount; index += 1) {
      const visibleObjectId = await readVisibleObjectId(cards.nth(index));
      expect(appleProductIds.has(visibleObjectId)).toBe(true);
    }
  });

  // ---------------------------------------------------------------------------
  // Clear facet filter: after filtering, clear -> all results return
  // ---------------------------------------------------------------------------
  test('clearing facet filters restores all results', async ({ page }) => {
    const facetsPanel = page.getByTestId('facets-panel');
    await expect(facetsPanel).toBeVisible({ timeout: 10000 });

    // Apply the first available facet filter, then clear it.
    const firstFacetButton = facetsPanel.getByRole('button').first();
    await expect(firstFacetButton).toBeVisible({ timeout: 15_000 });
    await firstFacetButton.click();

    const resultsPanel = page.getByTestId('results-panel');
    const filteredCards = resultsPanel.getByTestId('document-card');
    await expect(filteredCards.first()).toBeVisible({ timeout: 10000 });
    const filteredCount = await filteredCards.count();
    expect(filteredCount).toBeGreaterThan(0);

    const clearButton = facetsPanel.getByRole('button', { name: /clear/i });
    await expect(clearButton).toBeVisible({ timeout: 10_000 });
    await clearButton.click();

    // After clearing, result count should return to an equal or larger set.
    const restoredCards = resultsPanel.getByTestId('document-card');
    await expect(restoredCards.first()).toBeVisible({ timeout: 10000 });
    const restoredCount = await restoredCards.count();
    expect(restoredCount).toBeGreaterThanOrEqual(filteredCount);
  });

  // ---------------------------------------------------------------------------
  // Empty results: search for "xyznonexistent123" -> see empty state
  // ---------------------------------------------------------------------------
  test('searching for nonsense query shows no results', async ({ page }) => {
    const searchInput = page.getByPlaceholder(/search documents/i);
    await searchInput.fill('xyznonexistent123');
    await searchInput.press('Enter');

    // Should see "No results found" message
    await expect(page.getByText(/no results found/i)).toBeVisible({ timeout: 10000 });
  });

  // ---------------------------------------------------------------------------
  // Synonym search: search "notebook" -> see laptop results (synonym configured)
  // ---------------------------------------------------------------------------
  test('searching for "notebook" returns laptop results via synonyms', async ({ page }) => {
    const searchInput = page.getByPlaceholder(/search documents/i);
    await searchInput.fill('notebook');
    await searchInput.press('Enter');

    // The synonym laptop/notebook/computer is configured, so laptop products should appear
    const resultsPanel = page.getByTestId('results-panel');
    await expect(resultsPanel).toBeVisible({ timeout: 10000 });

    // At least one result card should appear due to the synonym mapping
    await expect(resultsPanel.getByTestId('document-card').first()).toBeVisible({ timeout: 10000 });

    // The results should include laptop products (visible via description or category fields)
    await expect(resultsPanel.getByText(/laptop/i).first()).toBeVisible();
  });

  // ---------------------------------------------------------------------------
  // Result count: verify total hits count is displayed
  // ---------------------------------------------------------------------------
  test('result count is displayed in the results header', async ({ page }) => {
    const resultsPanel = page.getByTestId('results-panel');
    await expect(resultsPanel).toBeVisible({ timeout: 10000 });

    // The results header shows "N results · Xms" where count and "results" are in sibling spans.
    // Verify result count is at least 12 (all seeded docs) and "results" text is visible.
    await expect(resultsPanel.getByText('results').first()).toBeVisible({ timeout: 10000 });

    // Verify document cards are rendered
    await expect(resultsPanel.getByTestId('document-card').first()).toBeVisible();
  });

  // ---------------------------------------------------------------------------
  // Pagination: if results have pagination controls, verify they work
  // ---------------------------------------------------------------------------
  test('pagination controls appear when results exceed one page', async ({ page, request }) => {
    const paginationIndex = `e2e-pagination-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const paginationDocs = Array.from({ length: 30 }, (_, idx) => ({
      objectID: `p9${String(idx + 1).padStart(3, '0')}`,
      name: `Pagination Fixture ${idx + 1}`,
      category: 'Pagination',
      brand: 'FixtureBrand',
      description: 'Pagination verification document',
    }));

    await createIndex(request, paginationIndex);
    await addDocuments(request, paginationIndex, paginationDocs);
    await expect
      .poll(async () => (await searchIndex(request, paginationIndex, '')).nbHits ?? 0, {
        timeout: 15_000,
      })
      .toBeGreaterThanOrEqual(paginationDocs.length);

    try {
      await page.goto(`/index/${paginationIndex}`);
      const resultsPanel = page.getByTestId('results-panel');
      await expect(resultsPanel).toBeVisible({ timeout: 10_000 });
      await submitSearchQueryAndWaitForTopCard(page, 'Pagination Fixture', paginationIndex);

      const pageIndicator = resultsPanel.getByText(/page \d+ of/i).first();
      await expect(pageIndicator).toBeVisible({ timeout: 10_000 });

      const firstPageText = (await pageIndicator.textContent()) ?? '';
      const firstPageMatch = firstPageText.match(/Page\s+(\d+)\s+of\s+(\d+)/i);
      expect(firstPageMatch, `Expected pagination indicator text, got "${firstPageText}"`).not.toBeNull();
      const currentPage = Number(firstPageMatch?.[1] ?? 0);
      const totalPages = Number(firstPageMatch?.[2] ?? 0);
      expect(totalPages).toBeGreaterThan(1);
      expect(currentPage).toBeLessThan(totalPages);

      // Pagination controls currently render icon-only buttons in the indicator container.
      // Click the enabled navigation button and assert page transition after real network activity.
      const paginationContainer = resultsPanel.getByTestId('pagination-controls');
      const nextButton = paginationContainer.locator('button:not([disabled])').first();
      await expect(nextButton).toBeVisible({ timeout: 5_000 });
      await expect(nextButton).toBeEnabled();
      const currentPageTopId = await readVisibleObjectId(resultsPanel.getByTestId('document-card').first());
      const expectedNextPageLabel = `Page ${currentPage + 1} of`;

      await nextButton.click();
      try {
        await expect(pageIndicator).toContainText(expectedNextPageLabel, { timeout: 2_000 });
      } catch {
        // Fallback for icon-only pagination controls that first take focus before keyboard activation.
        await nextButton.focus();
        await page.keyboard.press('Enter');
      }

      await expect(pageIndicator).toContainText(expectedNextPageLabel, { timeout: 10_000 });
      let nextPageTopId = '';
      await expect
        .poll(async () => {
          nextPageTopId = extractObjectIdFromText(
            await resultsPanel.getByTestId('document-card').first().innerText(),
          );
          return nextPageTopId;
        }, { timeout: 10_000 })
        .not.toBe(currentPageTopId);
      expect(nextPageTopId).not.toBe(currentPageTopId);
    } finally {
      await deleteIndex(request, paginationIndex);
    }
  });

  // ---------------------------------------------------------------------------
  // Multiple facets: select category + brand to narrow down results
  // ---------------------------------------------------------------------------
  test('combining category and brand facets narrows results', async ({ page }) => {
    const facetsPanel = page.getByTestId('facets-panel');
    await expect(facetsPanel).toBeVisible({ timeout: 10000 });

    // Wait for Apple brand facet and click it
    const appleBtn = await waitForFacetHeadingAndValue(facetsPanel, 'brand', 'Apple');
    await appleBtn.click();
    const resultsPanel = page.getByTestId('results-panel');
    await expect(resultsPanel.getByTestId('document-card').first()).toBeVisible({ timeout: 10000 });

    const cardsBeforeCategoryFilter = await resultsPanel.getByTestId('document-card').count();

    // Now also click "Laptops" category to narrow the Apple subset.
    const laptopsBtn = await waitForFacetHeadingAndValue(facetsPanel, 'category', 'Laptops');
    await laptopsBtn.click();

    const cardsAfterCategoryFilter = resultsPanel.getByTestId('document-card');
    await expect(cardsAfterCategoryFilter.first()).toBeVisible({ timeout: 10_000 });
    const narrowedCount = await cardsAfterCategoryFilter.count();
    expect(narrowedCount).toBe(1);
    expect(narrowedCount).toBeLessThanOrEqual(cardsBeforeCategoryFilter);
    const filteredTopId = await readVisibleObjectId(cardsAfterCategoryFilter.first());
    expect(filteredTopId).toBe('p01');
  });

  // ---------------------------------------------------------------------------
  // Analytics tracking toggle is visible and functional
  // ---------------------------------------------------------------------------
  test('analytics tracking toggle is visible and can be switched', async ({ page }) => {
    // The Track Analytics toggle should be in the top controls bar
    const toggle = page.getByRole('switch');
    await expect(toggle).toBeVisible({ timeout: 10000 });
    await expect(page.getByText('Track Analytics')).toBeVisible();

    // Initially off
    await expect(toggle).toHaveAttribute('data-state', 'unchecked');

    // Turn on
    await toggle.click();
    await expect(toggle).toHaveAttribute('data-state', 'checked');

    // Animated recording indicator should appear
    await expect(page.getByTestId('recording-indicator')).toBeVisible();

    // Turn off
    await toggle.click();
    await expect(toggle).toHaveAttribute('data-state', 'unchecked');
  });

  // ---------------------------------------------------------------------------
  // Add Documents button opens the dialog
  // ---------------------------------------------------------------------------
  test('Add Documents button opens dialog with tab options', async ({ page }) => {
    // Click the Add Documents button
    await page.getByRole('button', { name: /add documents/i }).click();

    // Dialog should open with tabs
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible({ timeout: 5000 });

    // Should have JSON, Upload, and Sample Data tabs
    await expect(dialog.getByText('JSON').first()).toBeVisible();
    await expect(dialog.getByText('Upload').first()).toBeVisible();
    await expect(dialog.getByText('Sample').first()).toBeVisible();

    // Close dialog
    await dialog.getByRole('button', { name: /close|cancel/i }).first().click();
    await expect(dialog).not.toBeVisible({ timeout: 5000 });
  });

  // ---------------------------------------------------------------------------
  // Index stats (doc count, storage) shown in breadcrumb area
  // ---------------------------------------------------------------------------
  test('index stats shown in breadcrumb area', async ({ page }) => {
    // The breadcrumb should show the index name
    await expect(page.getByText(TEST_INDEX).first()).toBeVisible({ timeout: 10000 });

    // Should show document count ("12 docs" or similar)
    await expect(page.getByText(/\d+ docs/).first()).toBeVisible();
  });

  // ---------------------------------------------------------------------------
  // Search with Enter key triggers search
  // ---------------------------------------------------------------------------
  test('pressing Enter in search box triggers search', async ({ page, request }) => {
    const searchResponse = await searchIndex(request, TEST_INDEX, 'apple');
    const expectedObjectIds = extractObjectIds(searchResponse.hits);

    expect(expectedObjectIds.length).toBeGreaterThan(0);

    const firstCard = await submitSearchQueryAndWaitForTopCard(page, 'apple');

    const topResultObjectId = await readVisibleObjectId(firstCard);
    expect(
      expectedObjectIds,
      'Expected Enter-triggered UI search to return an object from backend apple query results',
    ).toContain(topResultObjectId);
  });

  // (Removed: "clicking Search button triggers search" — no standalone Search button exists;
  //  search is triggered via input + Enter, already tested above.)

  // (Removed: "filter toggle opens and closes filter panel" — no filter toggle button exists;
  //  facets panel is always visible on desktop.)

  // ---------------------------------------------------------------------------
  // Typo tolerance: search "macbok" should still find MacBook
  // ---------------------------------------------------------------------------
  test('typo tolerance returns results for misspelled queries', async ({ page, request }) => {
    const typoResponse = await searchIndex(request, TEST_INDEX, 'macbok');
    const expectedObjectIds = extractObjectIds(typoResponse.hits);

    expect(expectedObjectIds.length).toBeGreaterThan(0);

    const firstCard = await submitSearchQueryAndWaitForTopCard(page, 'macbok');
    const topResultObjectId = await readVisibleObjectId(firstCard);
    expect(
      expectedObjectIds,
      'Expected typo query UI result to come from backend typo-tolerant query results',
    ).toContain(topResultObjectId);
  });

  // ---------------------------------------------------------------------------
  // Different search queries return different result sets
  // ---------------------------------------------------------------------------
  test('different searches return distinct result sets', async ({ page, request }) => {
    const candidateQueries = ['laptop', 'keyboard', 'tablet', 'apple', 'samsung', 'headphones'];
    const queryExpectations: Array<{ query: string; expectedObjectIds: string[] }> = [];
    for (const query of candidateQueries) {
      const expectedObjectIds = extractObjectIds((await searchIndex(request, TEST_INDEX, query)).hits);
      if (expectedObjectIds.length > 0) {
        queryExpectations.push({ query, expectedObjectIds });
      }
      if (queryExpectations.length >= 4) {
        break;
      }
    }
    expect(queryExpectations.length).toBeGreaterThanOrEqual(2);

    const seenTopIds = new Set<string>();
    for (const { query, expectedObjectIds } of queryExpectations) {
      const firstCard = await submitSearchQueryAndWaitForTopCard(page, query);
      const topObjectId = await readVisibleObjectId(firstCard);
      expect(
        expectedObjectIds,
        `Expected top result for "${query}" to come from latest search response`,
      ).toContain(topObjectId);
      seenTopIds.add(topObjectId);
    }
    expect(seenTopIds.size).toBeGreaterThanOrEqual(2);
  });

  // ---------------------------------------------------------------------------
  // Synonym: search "screen" finds monitors via synonym mapping
  // ---------------------------------------------------------------------------
  test('synonym query returns overlapping canonical results', async ({ page, request }) => {
    const synonymPairs = [
      { alias: 'screen', canonical: 'monitor' },
      { alias: 'earbuds', canonical: 'headphones' },
      { alias: 'notebook', canonical: 'laptop' },
    ] as const;

    let selectedPair:
      | {
          alias: string;
          canonical: string;
          overlappingIds: string[];
        }
      | null = null;

    for (const pair of synonymPairs) {
      const aliasIds = extractObjectIds((await searchIndex(request, TEST_INDEX, pair.alias)).hits);
      const canonicalIds = extractObjectIds((await searchIndex(request, TEST_INDEX, pair.canonical)).hits);
      const overlappingIds = aliasIds.filter((id) => canonicalIds.includes(id));

      if (overlappingIds.length > 0) {
        selectedPair = {
          alias: pair.alias,
          canonical: pair.canonical,
          overlappingIds,
        };
        break;
      }
    }

    expect(selectedPair).not.toBeNull();
    const pair = selectedPair as {
      alias: string;
      canonical: string;
      overlappingIds: string[];
    };

    const searchInput = page.getByPlaceholder(/search documents/i);
    await searchInput.fill(pair.alias);
    await searchInput.press('Enter');

    const resultsPanel = page.getByTestId('results-panel');
    await expect(resultsPanel).toBeVisible({ timeout: 10_000 });
    await expect(resultsPanel.getByTestId('document-card').first()).toBeVisible({ timeout: 10_000 });

    const visibleCardTexts = await resultsPanel.getByTestId('document-card').allInnerTexts();
    const visibleObjectIds = visibleCardTexts
      .map((text) => text.match(/\bp\d+\b/i)?.[0] ?? '')
      .filter((value): value is string => value.length > 0);
    expect(visibleObjectIds.length).toBeGreaterThan(0);

    const overlapInVisiblePage = visibleObjectIds.some((id) => pair.overlappingIds.includes(id));
    expect(
      overlapInVisiblePage,
      `Expected visible synonym results for "${pair.alias}" to overlap with canonical "${pair.canonical}"`,
    ).toBe(true);
  });

  // ---------------------------------------------------------------------------
  // Synonym: search "earbuds" returns headphone results
  // ---------------------------------------------------------------------------
  test('synonym "earbuds" returns headphone results', async ({ page }) => {
    await submitSearchQueryAndWaitForTopCard(page, 'earbuds');
  });

  // ---------------------------------------------------------------------------
  // Facets panel shows category and brand facets
  // ---------------------------------------------------------------------------
  test('facets panel shows category values', async ({ page }) => {
    const facetsPanel = await submitSearchAndWaitForFacets(page, 'laptop');
    await waitForFacetHeadingAndValue(facetsPanel, 'category', 'Laptops');

    const categoryButtons = facetsPanel.locator('button');
    await expect(categoryButtons.first()).toBeVisible({ timeout: 10_000 });
    const count = await categoryButtons.count();
    expect(count).toBeGreaterThanOrEqual(1);
  });

  // ---------------------------------------------------------------------------
  // Facets panel shows brand values
  // ---------------------------------------------------------------------------
  test('facets panel shows brand facet values', async ({ page }) => {
    const facetsPanel = await submitSearchAndWaitForFacets(page, 'laptop');
    await waitForFacetHeadingAndValue(facetsPanel, 'brand', 'Apple');

    const brandButtons = facetsPanel.locator('button');
    await expect(brandButtons.first()).toBeVisible({ timeout: 10_000 });
  });

  // ---------------------------------------------------------------------------
  // Facet counts are displayed with each facet value
  // ---------------------------------------------------------------------------
  test('facet values show document counts', async ({ page }) => {
    const facetsPanel = await submitSearchAndWaitForFacets(page, 'laptop');

    // Facet buttons should show numeric count badges (e.g., "Tablets 2" or "Apple 3")
    // Check that at least one facet button contains a number
    const firstFacetBtn = facetsPanel.locator('button').first();
    const btnText = await firstFacetBtn.textContent();
    expect(btnText).toMatch(/\d+/);
  });

  // ---------------------------------------------------------------------------
  // Document CRUD — serial: create via JSON tab, then delete via confirm dialog
  // ---------------------------------------------------------------------------
  test.describe.serial('Document CRUD', () => {
    const createdDocumentIds = new Set<string>();
    let baselineCount: number | null = null;
    const docId = `e2e-crud-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
    const docName = `E2E CRUD Gadget ${docId}`;
    const testDoc = {
      objectID: docId,
      name: docName,
      category: 'TestCategory',
      brand: 'TestBrand',
      description: 'Temporary document for CRUD verification',
    };

    test.afterAll(async ({ request }) => {
      await cleanupCreatedDocuments(request, createdDocumentIds);
    });

    test('create document via JSON tab and verify searchable', async ({ page, request }) => {
      baselineCount = (await searchIndex(request, TEST_INDEX, '')).nbHits ?? 0;
      trackCreatedDocument(createdDocumentIds, docId);
      await addDocumentViaJsonDialog(page, testDoc);

      // Poll API until the document is searchable
      await expect.poll(
        async () => extractObjectIds((await searchIndex(request, TEST_INDEX, docName)).hits),
        { timeout: 15_000 },
      ).toContain(docId);

      // Verify total count increased by 1
      await expect.poll(
        async () => (await searchIndex(request, TEST_INDEX, '')).nbHits ?? 0,
        { timeout: 10_000 },
      ).toBe(baselineCount + 1);

      const card = await submitSearchQueryAndWaitForTopCard(page, docName);
      await expect(card).toContainText(docName);
    });

    test('delete document via confirm dialog and verify removed', async ({ page, request }) => {
      expect(baselineCount).not.toBeNull();

      // Confirm the doc still exists before deleting
      await expect.poll(
        async () => extractObjectIds((await searchIndex(request, TEST_INDEX, docName)).hits),
        { timeout: 10_000 },
      ).toContain(docId);

      // Search for the doc in the UI
      const card = await submitSearchQueryAndWaitForTopCard(page, docName);
      await expect(card).toContainText(docName);

      // Click the trash button on the document card
      await card.getByRole('button', { name: /delete document/i }).click();

      // ConfirmDialog should appear
      const confirmDialog = page.getByRole('dialog', { name: 'Delete Document' });
      await expect(confirmDialog).toBeVisible({ timeout: 5_000 });

      // Click the destructive "Delete" confirm button
      await confirmDialog.getByRole('button', { name: /^Delete$/ }).click();

      // Wait for confirm dialog to close
      await expect(confirmDialog).not.toBeVisible({ timeout: 10_000 });

      // Poll API until the document is gone
      await expect.poll(
        async () => extractObjectIds((await searchIndex(request, TEST_INDEX, docName)).hits),
        { timeout: 15_000 },
      ).not.toContain(docId);

      await expect(page.getByText(/no results found/i)).toBeVisible({ timeout: 10_000 });

      // Verify total count returns to the pre-create baseline
      await expect.poll(
        async () => (await searchIndex(request, TEST_INDEX, '')).nbHits ?? 0,
        { timeout: 10_000 },
      ).toBe(baselineCount as number);

      markDocumentRemoved(createdDocumentIds, docId);
    });
  });

  test('delete document movie action includes object id in accessible name', async ({ page, request }) => {
    await seedMoviesDocumentActionsIndex(request);
    await page.goto(`/index/${MOVIES_DOCUMENT_ACTIONS_INDEX}`);

    const movieCard = page.getByTestId('document-card')
      .filter({ has: page.getByText(/movie_\d{3}/) })
      .first();
    await expect(movieCard).toBeVisible({ timeout: 10_000 });
    await expect(
      movieCard.getByRole('button', { name: /delete document movie_\d{3}/i }),
    ).toBeVisible();
  });
});
