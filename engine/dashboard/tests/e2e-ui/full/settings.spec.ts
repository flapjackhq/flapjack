/**
 * E2E-UI Full Suite — Settings Page (Real Server)
 *
 * NON-MOCKED SIMULATED-HUMAN REAL-BROWSER TESTS.
 * Tests run against a REAL Flapjack server with seeded test data.
 *
 * Settings:
 *   searchableAttributes: ['name', 'description', 'brand', 'category', 'tags']
 *   attributesForFaceting: ['category', 'brand', 'filterOnly(price)', 'filterOnly(inStock)']
 *   customRanking: ['desc(rating)', 'asc(price)']
 *
 * Covers:
 * - Searchable attributes display
 * - Faceting configuration display
 * - JSON editor toggle
 * - Ranking/custom ranking display
 * - Compact index button (visible + clickable)
 * - FilterOnly facets display
 * - Breadcrumb navigation
 * - All major sections present
 * - Save settings + verify persistence after reload
 * - Reset button reverts changes
 */
import type { Locator, Page } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import { TEST_INDEX, SETTINGS_TAB_ASSERTIONS } from '../helpers';
import {
  addDocuments,
  createIndex,
  deleteIndex,
  getSettings,
  searchIndex,
  updateSettings,
} from '../../fixtures/api-helpers';

const EXPLICIT_QUERY_TYPE_OPTIONS = ['prefixAll', 'prefixNone'] as const;
const SEARCH_TOGGLE_CHIP = 'name';

function buildSettingsTestIndexName(scope: string): string {
  const suffix = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  return `e2e-settings-${scope}-${suffix}`;
}

async function createSettingsMirrorIndex(
  request: Parameters<typeof getSettings>[0],
  scope: string,
): Promise<string> {
  const indexName = buildSettingsTestIndexName(scope);
  const baselineSettings = await getSettings(request, TEST_INDEX);
  const baselineDocuments = (await searchIndex(request, TEST_INDEX, '')).hits;

  await deleteIndex(request, indexName);
  try {
    await createIndex(request, indexName);
    if (Array.isArray(baselineDocuments) && baselineDocuments.length > 0) {
      const documentsToMirror = baselineDocuments.filter(
        (document): document is Record<string, unknown> =>
          typeof document === 'object' && document !== null,
      );
      await addDocuments(request, indexName, documentsToMirror);
      await expect
        .poll(async () => (await searchIndex(request, indexName, '')).nbHits ?? 0, {
          timeout: 15_000,
        })
        .toBeGreaterThanOrEqual(documentsToMirror.length);
    }
    await updateSettings(request, indexName, baselineSettings);
    return indexName;
  } catch (error) {
    await deleteIndex(request, indexName);
    throw error;
  }
}

async function gotoSettings(page: Page, indexName: string) {
  await page.goto(`/index/${indexName}/settings`);
  await expect(page.getByRole('heading', { name: /settings/i })).toBeVisible({ timeout: 10_000 });
}

async function selectTab(page: Page, tabLabel: string) {
  const tab = page.getByRole('tab', { name: tabLabel });
  await tab.click();
  await expect(tab).toHaveAttribute('aria-selected', 'true');
}

function getSettingsPanel(page: Page, tabLabel: string) {
  return page.getByRole('tabpanel', { name: tabLabel });
}

async function saveSettings(page: Page) {
  const saveBtn = page.getByRole('button', { name: /save/i });
  await expect(saveBtn).toBeVisible({ timeout: 5_000 });

  const responsePromise = page.waitForResponse(
    (resp) => resp.url().includes('/settings') && (resp.status() === 200 || resp.status() === 202),
    { timeout: 15_000 }
  );
  await saveBtn.click();
  await responsePromise;
}

async function reloadSettings(page: Page, tabLabel: string) {
  await page.reload();
  await expect(page.getByRole('heading', { name: /settings/i })).toBeVisible({ timeout: 10_000 });
  await selectTab(page, tabLabel);
}

function chooseAlternateQueryType(currentQueryType: string): typeof EXPLICIT_QUERY_TYPE_OPTIONS[number] {
  const targetQueryType = EXPLICIT_QUERY_TYPE_OPTIONS.find((option) => option !== currentQueryType);
  if (!targetQueryType) {
    throw new Error(`No alternate queryType found for current value: ${currentQueryType}`);
  }
  return targetQueryType;
}

function readStringArraySetting(
  settings: Record<string, unknown>,
  key: string,
): string[] {
  const value = settings[key];
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter((item): item is string => typeof item === 'string');
}

function chooseTargetLanguages(originalSettings: Record<string, unknown>): string[] {
  const originalLanguages = readStringArraySetting(originalSettings, 'queryLanguages');
  return originalLanguages.join(',') === 'de,es' ? ['en', 'fr'] : ['de', 'es'];
}

function chooseDistinctAttribute(originalSettings: Record<string, unknown>): string {
  const originalAttribute = typeof originalSettings.attributeForDistinct === 'string'
    ? originalSettings.attributeForDistinct
    : '';
  return originalAttribute === 'category' ? 'brand' : 'category';
}

async function ensureSwitchChecked(switchControl: Locator) {
  if ((await switchControl.getAttribute('aria-checked')) !== 'true') {
    await switchControl.click();
  }
}

function appendFacetAttribute(originalFacets: string, facetAttribute: string): string {
  return originalFacets ? `${originalFacets}, ${facetAttribute}` : facetAttribute;
}

test.describe('Settings Page', () => {
  // This spec mutates shared index settings and must run serially to avoid
  // cross-test races when Playwright fullyParallel is enabled. It uses its
  // own mirrored index so those mutations never leak into other spec files.
  test.describe.configure({ mode: 'serial' });
  let settingsTestIndex = '';

  test.beforeAll(async ({ request }) => {
    settingsTestIndex = await createSettingsMirrorIndex(request, 'primary');
  });

  test.afterAll(async ({ request }) => {
    if (settingsTestIndex) {
      await deleteIndex(request, settingsTestIndex);
    }
  });

  test.beforeEach(async ({ page }) => {
    await gotoSettings(page, settingsTestIndex);
  });

  test('displays searchable attributes from seeded settings', async ({ page }) => {
    const expectedAttributes = ['name', 'description', 'brand', 'category', 'tags'];
    const expectedFieldChips = ['name', 'brand', 'category'];

    await selectTab(page, 'Search');
    const searchPanel = getSettingsPanel(page, 'Search');
    await expect(searchPanel.getByRole('heading', { name: 'Search' })).toBeVisible();

    for (const attr of expectedFieldChips) {
      await expect(searchPanel.getByTestId(`attr-chip-${attr}`)).toBeVisible();
    }

    const searchableAttributesInput = searchPanel.getByPlaceholder('title, description, tags');
    await expect(searchableAttributesInput).toBeVisible();
    const configuredAttributes = (await searchableAttributesInput.inputValue())
      .split(',')
      .map((value) => value.trim())
      .filter(Boolean);
    expect(configuredAttributes).toEqual(expect.arrayContaining(expectedAttributes));

    await expect(searchPanel.getByText('Searchable Attributes')).toBeVisible();
  });

  test('displays faceting attributes from seeded settings', async ({ page }) => {
    await selectTab(page, 'Facets & Filters');
    const facetsPanel = page.getByRole('tabpanel', { name: 'Facets & Filters' });

    await expect(facetsPanel.getByRole('heading', { name: 'Facets & Filters' })).toBeVisible({ timeout: 10_000 });
    await expect(facetsPanel.getByText('Attributes For Faceting')).toBeVisible();
    await expect(facetsPanel.getByTestId('attr-chip-category')).toBeVisible();
    await expect(facetsPanel.getByTestId('attr-chip-brand')).toBeVisible();
  });

  test('toggling JSON view shows raw settings JSON', async ({ page }) => {
    await selectTab(page, 'Search');
    const searchPanel = getSettingsPanel(page, 'Search');
    await expect(searchPanel.getByText('Searchable Attributes')).toBeVisible({ timeout: 10_000 });

    const jsonToggle = page.getByRole('button', { name: /json/i });
    await expect(jsonToggle).toBeVisible();
    await jsonToggle.click();

    await expect(page.getByText(/searchableAttributes/).first()).toBeVisible({ timeout: 15_000 });

    await jsonToggle.click();
    await expect(searchPanel.getByText('Searchable Attributes')).toBeVisible({ timeout: 10_000 });
  });

  test('displays ranking and custom ranking configuration', async ({ page }) => {
    await selectTab(page, 'Ranking');
    const rankingPanel = page.getByRole('tabpanel', { name: 'Ranking' });

    await expect(rankingPanel.getByRole('heading', { name: 'Ranking' })).toBeVisible({ timeout: 10_000 });
    await expect(rankingPanel.getByText('Ranking Criteria', { exact: true })).toBeVisible();
    await expect(rankingPanel.getByText('Custom Ranking', { exact: true })).toBeVisible();
    await expect(rankingPanel.getByPlaceholder('desc(popularity), asc(price)')).toHaveValue(/desc\(rating\)/);
    await expect(rankingPanel.getByPlaceholder('desc(popularity), asc(price)')).toHaveValue(/asc\(price\)/);
  });

  test('compact index button is visible and enabled', async ({ page }) => {
    const compactBtn = page.getByRole('button', { name: /compact/i });
    await expect(compactBtn).toBeVisible({ timeout: 10_000 });
    await expect(compactBtn).toContainText(/compact index/i);
    await expect(compactBtn).toBeEnabled();
  });

  test('compact index button click triggers compaction', async ({ page }) => {
    const compactBtn = page.getByRole('button', { name: /compact/i });
    await expect(compactBtn).toBeVisible({ timeout: 10_000 });

    const responsePromise = page.waitForResponse(
      (resp) => resp.url().includes('/compact'),
      { timeout: 15_000 }
    );
    await compactBtn.click();

    const response = await responsePromise;
    expect([200, 202]).toContain(response.status());
  });

  test('displays filterOnly faceting attributes', async ({ page }) => {
    await selectTab(page, 'Facets & Filters');
    const facetingTextarea = page
      .getByRole('tabpanel', { name: 'Facets & Filters' })
      .getByPlaceholder('category, brand, color');

    await expect(facetingTextarea).toHaveValue(/filterOnly\(price\)/);
    await expect(facetingTextarea).toHaveValue(/filterOnly\(inStock\)/);
  });

  test('settings page has breadcrumb back to index', async ({ page }) => {
    const breadcrumbLink = page.getByRole('main').getByRole('link', { name: settingsTestIndex });
    await expect(breadcrumbLink).toBeVisible({ timeout: 10_000 });
    await expect(breadcrumbLink).toHaveAttribute('href', `/index/${settingsTestIndex}`);
  });

  test('settings form shows all major sections', async ({ page }) => {
    for (const { tabLabel, panelAssertion } of SETTINGS_TAB_ASSERTIONS) {
      await selectTab(page, tabLabel);
      await expect(panelAssertion(getSettingsPanel(page, tabLabel))).toBeVisible({ timeout: 10_000 });
    }
  });

  test('Reset button appears after form modification and reverts changes', async ({ page }) => {
    await selectTab(page, 'Search');

    const toggleChip = getSettingsPanel(page, 'Search').getByTestId(`attr-chip-${SEARCH_TOGGLE_CHIP}`);
    await expect(toggleChip).toBeVisible({ timeout: 10_000 });

    await expect(page.getByRole('button', { name: /reset/i })).toBeHidden();
    await expect(page.getByRole('button', { name: /save/i })).toBeHidden();

    await toggleChip.click();
    await expect(toggleChip).toHaveAttribute('aria-pressed', 'false');

    const resetBtn = page.getByRole('button', { name: /reset/i });
    const saveBtn = page.getByRole('button', { name: /save/i });
    await expect(resetBtn).toBeVisible({ timeout: 5_000 });
    await expect(saveBtn).toBeVisible();

    await resetBtn.click();

    await expect(resetBtn).not.toBeVisible({ timeout: 5_000 });
    await expect(saveBtn).toBeHidden();
    await expect(toggleChip).toHaveAttribute('aria-pressed', 'true');
  });

  test('save settings persists changes after reload', async ({ page, request }) => {
    await selectTab(page, 'Search');

    const originalSettings = await getSettings(request, settingsTestIndex);

    try {
      const toggleChip = getSettingsPanel(page, 'Search').getByTestId(`attr-chip-${SEARCH_TOGGLE_CHIP}`);
      await expect(toggleChip).toBeVisible({ timeout: 10_000 });

      await toggleChip.click();
      await saveSettings(page);
      await reloadSettings(page, 'Search');

      const reloadedToggleChip = getSettingsPanel(page, 'Search').getByTestId(`attr-chip-${SEARCH_TOGGLE_CHIP}`);
      await expect(reloadedToggleChip).toBeVisible({ timeout: 10_000 });
      await expect(reloadedToggleChip).toHaveAttribute('aria-pressed', 'false');
    } finally {
      await updateSettings(request, settingsTestIndex, originalSettings);
    }
  });

  test('search tab query type persists after save and reload', async ({ page, request }) => {
    await selectTab(page, 'Search');

    const originalSettings = await getSettings(request, settingsTestIndex);
    const queryTypeSelect = getSettingsPanel(page, 'Search').getByRole('combobox');
    const currentQueryType = await queryTypeSelect.inputValue();
    const targetQueryType = chooseAlternateQueryType(currentQueryType);

    try {
      await queryTypeSelect.selectOption(targetQueryType);
      await expect(queryTypeSelect).toHaveValue(targetQueryType);

      await saveSettings(page);
      await reloadSettings(page, 'Search');
      await expect(getSettingsPanel(page, 'Search').getByRole('combobox')).toHaveValue(targetQueryType);
    } finally {
      await updateSettings(request, settingsTestIndex, originalSettings);
    }
  });

  test('language and text tab query languages persist after save and reload', async ({ page, request }) => {
    await selectTab(page, 'Language & Text');

    const originalSettings = await getSettings(request, settingsTestIndex);
    const targetLanguages = chooseTargetLanguages(originalSettings);

    try {
      const queryLanguagesSelect = getSettingsPanel(page, 'Language & Text').getByTestId('query-languages-select');
      await queryLanguagesSelect.selectOption(targetLanguages);
      await expect(queryLanguagesSelect).toHaveValues(targetLanguages);

      await saveSettings(page);
      await reloadSettings(page, 'Language & Text');

      await expect(
        getSettingsPanel(page, 'Language & Text').getByTestId('query-languages-select'),
      ).toHaveValues(targetLanguages);
    } finally {
      await updateSettings(request, settingsTestIndex, originalSettings);
    }
  });

  test('ranking tab distinct settings persist after save and reload', async ({ page, request }) => {
    await selectTab(page, 'Ranking');

    const originalSettings = await getSettings(request, settingsTestIndex);
    const targetAttribute = chooseDistinctAttribute(originalSettings);

    try {
      const rankingPanel = getSettingsPanel(page, 'Ranking');
      const distinctSwitch = rankingPanel.getByTestId('distinct-enabled-switch');
      await expect(distinctSwitch).toBeVisible();
      await ensureSwitchChecked(distinctSwitch);

      const attributeInput = rankingPanel.getByPlaceholder('sku');
      await expect(attributeInput).toBeVisible({ timeout: 5_000 });
      await attributeInput.fill(targetAttribute);

      await saveSettings(page);
      await reloadSettings(page, 'Ranking');
      const reloadedRankingPanel = getSettingsPanel(page, 'Ranking');
      await expect(reloadedRankingPanel.getByTestId('distinct-enabled-switch')).toHaveAttribute('aria-checked', 'true');
      await expect(reloadedRankingPanel.getByPlaceholder('sku')).toHaveValue(targetAttribute);
    } finally {
      await updateSettings(request, settingsTestIndex, originalSettings);
    }
  });

  test('JSON view reflects draft changes and Reset restores saved values', async ({ page }) => {
    await selectTab(page, 'Search');
    const searchPanel = getSettingsPanel(page, 'Search');
    const toggleChip = searchPanel.getByTestId(`attr-chip-${SEARCH_TOGGLE_CHIP}`);
    await expect(toggleChip).toBeVisible({ timeout: 10_000 });
    await expect(toggleChip).toHaveAttribute('aria-pressed', 'true');

    // Make a draft change on the Search tab
    await toggleChip.click();
    await expect(toggleChip).toHaveAttribute('aria-pressed', 'false');
    await expect(page.getByRole('button', { name: /reset/i })).toBeVisible({ timeout: 5_000 });

    // Open JSON view — exercises the currentSettings = { ...settings, ...formData } merge
    // path in Settings.tsx. The Monaco editor value is JSON.stringify(currentSettings).
    const jsonToggle = page.getByRole('button', { name: /json/i });
    await jsonToggle.click();
    await expect(page.getByText(/Raw settings for/)).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText(/searchableAttributes/).first()).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText('"name",', { exact: true })).toHaveCount(0);
    // Dirty controls remain visible while in JSON view
    await expect(page.getByRole('button', { name: /reset/i })).toBeVisible();

    // Reset while still in JSON view — dirty controls should disappear
    await page.getByRole('button', { name: /reset/i }).click();
    await expect(page.getByRole('button', { name: /reset/i })).not.toBeVisible({ timeout: 5_000 });
    await expect(page.getByRole('button', { name: /save/i })).toBeHidden();

    // Reopen form view — saved value should be restored (formData cleared by Reset)
    await jsonToggle.click();
    await expect(getSettingsPanel(page, 'Search').getByTestId(`attr-chip-${SEARCH_TOGGLE_CHIP}`))
      .toHaveAttribute('aria-pressed', 'true');
  });

  test('facets and filters persistence with reindex warning lifecycle', async ({ page, request }) => {
    await selectTab(page, 'Facets & Filters');
    const facetsPanel = getSettingsPanel(page, 'Facets & Filters');

    const originalSettings = await getSettings(request, settingsTestIndex);
    const facetTextarea = facetsPanel.getByPlaceholder('category, brand, color');
    await expect(facetTextarea).toBeVisible({ timeout: 10_000 });

    try {
      // Append a new facet attribute
      const originalFacets = await facetTextarea.inputValue();
      const updatedFacets = appendFacetAttribute(originalFacets, 'tags');
      await facetTextarea.fill(updatedFacets);

      // "Reindex needed" warning should appear when facets differ from saved
      await expect(facetsPanel.getByText('Reindex needed')).toBeVisible({ timeout: 5_000 });

      // Save — clears formData so saved matches current, warning disappears
      await saveSettings(page);
      await expect(page.getByRole('button', { name: /save/i })).not.toBeVisible({ timeout: 5_000 });
      await expect(facetsPanel.getByText('Reindex needed')).not.toBeVisible({ timeout: 5_000 });
      await expect(facetsPanel.getByText('Up to date')).toBeVisible({ timeout: 5_000 });

      // Reload and verify the new facets persisted
      await reloadSettings(page, 'Facets & Filters');
      const reloadedTextarea = getSettingsPanel(page, 'Facets & Filters').getByPlaceholder('category, brand, color');
      await expect(reloadedTextarea).toHaveValue(/tags/);

      // Verify via API
      const savedSettings = await getSettings(request, settingsTestIndex);
      expect(savedSettings.attributesForFaceting).toContain('tags');
    } finally {
      await updateSettings(request, settingsTestIndex, originalSettings);
    }
  });

  test('display tab unretrievableAttributes persist and Reset restores saved value', async ({ page, request }) => {
    await selectTab(page, 'Display');
    const displayPanel = getSettingsPanel(page, 'Display');

    const originalSettings = await getSettings(request, settingsTestIndex);
    const unretrievableTextarea = displayPanel.getByPlaceholder('internal_notes, supplier_cost');
    await expect(unretrievableTextarea).toBeVisible({ timeout: 10_000 });

    try {
      // Edit unretrievableAttributes and save
      await unretrievableTextarea.fill('internal_notes');
      await saveSettings(page);
      await expect(page.getByRole('button', { name: /save/i })).not.toBeVisible({ timeout: 5_000 });

      // Reload and verify persistence
      await reloadSettings(page, 'Display');
      const reloadedTextarea = getSettingsPanel(page, 'Display').getByPlaceholder('internal_notes, supplier_cost');
      await expect(reloadedTextarea).toHaveValue(/internal_notes/);

      // Make a second unsaved edit
      await reloadedTextarea.fill('internal_notes, supplier_cost');
      await expect(page.getByRole('button', { name: /reset/i })).toBeVisible({ timeout: 5_000 });

      // Reset — should restore the persisted value (internal_notes only)
      await page.getByRole('button', { name: /reset/i }).click();
      await expect(page.getByRole('button', { name: /reset/i })).not.toBeVisible({ timeout: 5_000 });
      await expect(page.getByRole('button', { name: /save/i })).toBeHidden();
      await expect(reloadedTextarea).toHaveValue('internal_notes');
    } finally {
      await updateSettings(request, settingsTestIndex, originalSettings);
    }
  });
});

test.describe('Settings Page narrow viewport', () => {
  let narrowViewportSettingsIndex = '';

  test.beforeAll(async ({ request }) => {
    narrowViewportSettingsIndex = await createSettingsMirrorIndex(request, 'narrow');
  });

  test.afterAll(async ({ request }) => {
    if (narrowViewportSettingsIndex) {
      await deleteIndex(request, narrowViewportSettingsIndex);
    }
  });

  test('shows tabs in DOM and active content at narrow width', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await gotoSettings(page, narrowViewportSettingsIndex);

    await expect(page.getByRole('tab')).toHaveCount(SETTINGS_TAB_ASSERTIONS.length);
    await expect(
      SETTINGS_TAB_ASSERTIONS[0].panelAssertion(getSettingsPanel(page, SETTINGS_TAB_ASSERTIONS[0].tabLabel))
    ).toBeVisible({ timeout: 10_000 });

    const vectorTab = page.getByRole('tab', { name: 'Vector / AI' });
    await vectorTab.scrollIntoViewIfNeeded();
    await vectorTab.click();
    await expect(vectorTab).toHaveAttribute('aria-selected', 'true');
    await expect(page.getByLabel('AI Base URL')).toBeVisible({ timeout: 10_000 });
  });
});
