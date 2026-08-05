import type { APIRequestContext, Locator, Page } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import { getSidebar } from '../helpers';
import { addDocuments, createIndex, deleteIndex, searchIndex } from '../../fixtures/api-helpers';
import { PRODUCTS, displayPreferencesFixtures } from '../../fixtures/test-data';
import { responseMatchesIndexQuery } from '../result-helpers';

const TARGET_INDEX = `e2e-display-preferences-${Date.now()}`;
const FULL_PREFERENCES = displayPreferencesFixtures.preferences.full;

interface ExpectedBrowseProduct {
  objectID: string;
  name: string;
  description: string;
  image_url: string;
  brand: string;
  category: string;
}

const CONSUMED_FIELDS = [
  FULL_PREFERENCES.titleAttribute,
  FULL_PREFERENCES.subtitleAttribute,
  FULL_PREFERENCES.imageAttribute,
  ...FULL_PREFERENCES.tagAttributes,
].filter(Boolean);

function firstDocumentCard(page: Page): Locator {
  return page.getByTestId('results-panel').getByTestId('document-card').first();
}

function fieldRow(card: Locator, fieldName: string): Locator {
  return card.getByText(`${fieldName}:`, { exact: true });
}

async function waitForIndexBrowsePage(page: Page, indexName: string): Promise<void> {
  const initialQueryResponse = page.waitForResponse(
    (response) => responseMatchesIndexQuery(response, indexName),
    { timeout: 15_000 },
  );
  await page.goto(`/index/${indexName}`);
  await initialQueryResponse;
  await expect(page.getByTestId('results-panel')).toBeVisible({ timeout: 15_000 });
  await expect(firstDocumentCard(page)).toBeVisible({ timeout: 10_000 });
}

async function readExpectedFirstBrowseProduct(
  request: APIRequestContext,
  indexName: string,
): Promise<ExpectedBrowseProduct> {
  const response = await searchIndex(request, indexName, '', { hitsPerPage: 20 });
  const firstHit = response.hits?.[0];

  expect(firstHit).toBeTruthy();
  expect(firstHit).toEqual(expect.objectContaining({
    objectID: expect.any(String),
    name: expect.any(String),
    description: expect.any(String),
    image_url: expect.any(String),
    brand: expect.any(String),
    category: expect.any(String),
  }));

  return firstHit as ExpectedBrowseProduct;
}

async function seedDisplayPreferencesIndex(
  request: APIRequestContext,
  indexName: string,
): Promise<ExpectedBrowseProduct> {
  await deleteIndex(request, indexName);
  await createIndex(request, indexName);
  await addDocuments(request, indexName, PRODUCTS);

  await expect
    .poll(async () => (await searchIndex(request, indexName, '')).nbHits ?? 0, {
      timeout: 15_000,
    })
    .toBeGreaterThanOrEqual(PRODUCTS.length);

  return readExpectedFirstBrowseProduct(request, indexName);
}

async function verifyDefaultBrowseCard(
  page: Page,
  expectedFirstProduct: ExpectedBrowseProduct,
): Promise<void> {
  const card = firstDocumentCard(page);
  const visibleFieldRows = card.getByText(
    /(name|description|brand|category|price|rating|image_url|tags):/,
    { exact: false },
  );
  await expect(card).toContainText(expectedFirstProduct.objectID);
  await expect(card.getByTestId('document-card-configured-header')).toHaveCount(0);
  await expect(visibleFieldRows.first()).toBeVisible();
}

async function openDisplayPreferencesModal(page: Page): Promise<Locator> {
  await page.getByRole('button', { name: 'Display Preferences' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible({ timeout: 10_000 });
  await expect(dialog.getByRole('heading', { name: 'Display Preferences' })).toBeVisible();
  return dialog;
}

async function saveFullPreferences(page: Page): Promise<void> {
  const dialog = await openDisplayPreferencesModal(page);

  await dialog
    .getByLabel('Title field', { exact: true })
    .selectOption(FULL_PREFERENCES.titleAttribute as string);
  await dialog
    .getByLabel('Subtitle field', { exact: true })
    .selectOption(FULL_PREFERENCES.subtitleAttribute as string);
  await dialog
    .getByLabel('Image field', { exact: true })
    .selectOption(FULL_PREFERENCES.imageAttribute as string);

  for (const tagField of FULL_PREFERENCES.tagAttributes) {
    await dialog.getByTestId(`attr-chip-${tagField}`).click();
  }

  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(dialog).toBeHidden();
}

async function verifyFullPreferenceCard(
  page: Page,
  expectedFirstProduct: ExpectedBrowseProduct,
): Promise<void> {
  const card = firstDocumentCard(page);
  await expect(card.getByTestId('document-card-title')).toHaveText(expectedFirstProduct.name);
  await expect(card.getByTestId('document-card-subtitle')).toHaveText(expectedFirstProduct.description);
  await expect(card.getByTestId('document-card-image')).toHaveAttribute('src', expectedFirstProduct.image_url);

  await expect(card.getByText(expectedFirstProduct.category, { exact: true })).toBeVisible();
  await expect(card.getByText(expectedFirstProduct.brand, { exact: true })).toBeVisible();

  for (const consumedField of CONSUMED_FIELDS) {
    await expect(fieldRow(card, consumedField)).toHaveCount(0);
  }
}

async function createTemporaryIsolationIndex(
  request: APIRequestContext,
  indexName: string,
): Promise<void> {
  await createIndex(request, indexName);
  await addDocuments(request, indexName, [
    {
      objectID: 'temp-01',
      name: 'Temporary Product',
      description: 'Temporary isolation description',
      image_url: 'https://cdn.example.test/products/temp-01.jpg',
      category: 'Temporary',
      brand: 'Isolation',
      tags: ['temporary'],
      price: 1,
      rating: 5,
      inStock: true,
    },
  ]);

  await expect
    .poll(async () => (await searchIndex(request, indexName, '')).nbHits ?? 0, {
      timeout: 15_000,
    })
    .toBeGreaterThanOrEqual(1);
}

test.describe('Display Preferences', () => {
  let expectedFirstProduct: ExpectedBrowseProduct;

  test.beforeAll(async ({ request }) => {
    expectedFirstProduct = await seedDisplayPreferencesIndex(request, TARGET_INDEX);
  });

  test.afterAll(async ({ request }) => {
    await deleteIndex(request, TARGET_INDEX);
  });

  test.beforeEach(async ({ page }) => {
    await waitForIndexBrowsePage(page, TARGET_INDEX);
  });

  test('loads seeded Browse content before modal interactions', async ({ page }) => {
    await verifyDefaultBrowseCard(page, expectedFirstProduct);
    await expect(firstDocumentCard(page)).toContainText(expectedFirstProduct.objectID);
  });

  test('opens the modal with title/subtitle/image/tag controls and index field options', async ({ page }) => {
    await verifyDefaultBrowseCard(page, expectedFirstProduct);

    const dialog = await openDisplayPreferencesModal(page);
    await expect(
      dialog.getByText('Configure browse card fields for this index. Changes are saved per index.'),
    ).toBeVisible();

    await expect(dialog.getByLabel('Title field', { exact: true })).toBeVisible();
    await expect(dialog.getByLabel('Subtitle field', { exact: true })).toBeVisible();
    await expect(dialog.getByLabel('Image field', { exact: true })).toBeVisible();
    await expect(dialog.getByText('Tag fields')).toBeVisible();

    const expectedFieldOptions = [
      FULL_PREFERENCES.titleAttribute,
      FULL_PREFERENCES.subtitleAttribute,
      FULL_PREFERENCES.imageAttribute,
      ...FULL_PREFERENCES.tagAttributes,
    ].filter(Boolean);

    const titleSelect = dialog.getByLabel('Title field', { exact: true });
    for (const fieldName of expectedFieldOptions) {
      await expect(titleSelect.getByRole('option', { name: fieldName, exact: true })).toHaveCount(1);
    }

    for (const tagField of FULL_PREFERENCES.tagAttributes) {
      await expect(dialog.getByTestId(`attr-chip-${tagField}`)).toBeVisible();
    }
  });

  test('saves full preferences and renders configured title/subtitle/image/tags', async ({ page }) => {
    await verifyDefaultBrowseCard(page, expectedFirstProduct);

    await saveFullPreferences(page);
    await verifyFullPreferenceCard(page, expectedFirstProduct);
    await expect(firstDocumentCard(page).getByTestId('document-card-title')).toHaveText(expectedFirstProduct.name);
  });

  test('clears preferences and reverts cards to default field-value rendering', async ({ page }) => {
    await verifyDefaultBrowseCard(page, expectedFirstProduct);

    await saveFullPreferences(page);
    await verifyFullPreferenceCard(page, expectedFirstProduct);

    const dialog = await openDisplayPreferencesModal(page);
    await dialog.getByRole('button', { name: 'Clear' }).click();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await expect(dialog).toBeHidden();

    await verifyDefaultBrowseCard(page, expectedFirstProduct);

    await page.reload();
    await expect(firstDocumentCard(page)).toBeVisible({ timeout: 10_000 });
    await verifyDefaultBrowseCard(page, expectedFirstProduct);
  });

  test('persists saved preferences across page navigation and refresh', async ({ page }) => {
    await verifyDefaultBrowseCard(page, expectedFirstProduct);

    await saveFullPreferences(page);
    await verifyFullPreferenceCard(page, expectedFirstProduct);

    await page.getByRole('link', { name: 'Overview' }).first().click();
    await expect(page).toHaveURL(/\/overview/);
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({ timeout: 10_000 });

    await getSidebar(page).getByText(TARGET_INDEX).first().click();
    await expect(page).toHaveURL(new RegExp(`/index/${TARGET_INDEX}`));
    await expect(firstDocumentCard(page)).toBeVisible({ timeout: 10_000 });
    await verifyFullPreferenceCard(page, expectedFirstProduct);

    await page.reload();
    await expect(firstDocumentCard(page)).toBeVisible({ timeout: 10_000 });
    await verifyFullPreferenceCard(page, expectedFirstProduct);
  });

  test('keeps preferences isolated per index', async ({ page, request }) => {
    await verifyDefaultBrowseCard(page, expectedFirstProduct);

    await saveFullPreferences(page);
    await verifyFullPreferenceCard(page, expectedFirstProduct);

    const tempIndexName = `e2e-display-prefs-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    await createTemporaryIsolationIndex(request, tempIndexName);

    try {
      await waitForIndexBrowsePage(page, tempIndexName);

      const card = firstDocumentCard(page);
      const visibleFieldRows = card.getByText(
        /(name|description|brand|category|price|rating|image_url|tags):/,
        { exact: false },
      );
      await expect(card.getByTestId('document-card-configured-header')).toHaveCount(0);
      await expect(visibleFieldRows.first()).toBeVisible();
      await expect(card).toContainText('temp-01');
    } finally {
      await deleteIndex(request, tempIndexName);
    }
  });
});
