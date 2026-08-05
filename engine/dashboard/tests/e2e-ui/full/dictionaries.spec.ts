import { test, expect } from '../../fixtures/auth.fixture';
import { batchDictionaryEntries } from '../../fixtures/api-helpers';
import { resetAllDictionaries } from '../tenant-admin-helpers';
import {
  DICTIONARY_EMPTY_STATES,
  DICTIONARY_LABELS,
  getListTestId,
} from '../../../src/pages/dictionaries/shared';

const DICTIONARIES_URL = '/dictionaries';

test.describe('Dictionaries', () => {
  // Tests modify shared tenant-level dictionary state — must run serially
  test.describe.configure({ mode: 'serial' });

  test.beforeEach(async ({ page, request }) => {
    await resetAllDictionaries(request);
    await page.goto(DICTIONARIES_URL);
    await expect(page.getByRole('heading', { name: 'Dictionaries' })).toBeVisible({ timeout: 15_000 });
  });

  test.afterEach(async ({ request }) => {
    await resetAllDictionaries(request);
  });

  test('loads seeded stopword entry in default Stopwords view', async ({ page, request }) => {
    await batchDictionaryEntries(request, 'stopwords', [
      {
        objectID: 'e2e-stop-the',
        word: 'the',
        language: 'en',
        state: 'enabled',
      },
    ]);

    await page.reload();
    await expect(page.getByRole('heading', { name: 'Dictionaries' })).toBeVisible({ timeout: 15_000 });

    const stopwordsList = page.getByTestId(getListTestId('stopwords'));
    await expect(stopwordsList).toBeVisible({ timeout: 10_000 });
    await expect(stopwordsList.getByText('the')).toBeVisible({ timeout: 10_000 });
  });

  test('switches between Stopwords, Plurals, and Compounds tabs', async ({ page }) => {
    await expect(page.getByRole('heading', { name: DICTIONARY_LABELS.stopwords })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText(DICTIONARY_EMPTY_STATES.stopwords)).toBeVisible({ timeout: 10_000 });

    await page.getByTestId('dictionary-tab-plurals').click();
    await expect(page.getByRole('heading', { name: DICTIONARY_LABELS.plurals })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText(DICTIONARY_EMPTY_STATES.plurals)).toBeVisible({ timeout: 10_000 });

    await page.getByTestId('dictionary-tab-compounds').click();
    await expect(page.getByRole('heading', { name: DICTIONARY_LABELS.compounds })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText(DICTIONARY_EMPTY_STATES.compounds)).toBeVisible({ timeout: 10_000 });

    await page.getByTestId('dictionary-tab-stopwords').click();
    await expect(page.getByRole('heading', { name: DICTIONARY_LABELS.stopwords })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText(DICTIONARY_EMPTY_STATES.stopwords)).toBeVisible({ timeout: 10_000 });
  });

  test('stopword create-delete lifecycle preserves badge and row counts', async ({ page }) => {
    const stopwordsList = page.getByTestId(getListTestId('stopwords'));
    const stopwordRows = stopwordsList.getByTestId('dictionary-entry');

    // Capture baseline: panel badge shows "0 entries", empty state visible
    await expect(page.getByText('0 entries')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText(DICTIONARY_EMPTY_STATES.stopwords)).toBeVisible({ timeout: 10_000 });
    await expect(stopwordRows).toHaveCount(0);

    // Create a stopword via the dialog
    await page.getByTestId('add-dictionary-entry-btn').click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible({ timeout: 10_000 });
    await dialog.getByLabel('Word').fill('e2e-lifecycle-stop');
    await dialog.getByLabel('Language').selectOption('en');
    await dialog.getByLabel('State').selectOption('enabled');
    await dialog.getByRole('button', { name: 'Add Entry' }).click();

    // Verify count increased: badge shows "1", list has exactly one row
    await expect(stopwordsList).toBeVisible({ timeout: 10_000 });
    await expect(stopwordRows).toHaveCount(1);
    await expect(stopwordsList.getByText('e2e-lifecycle-stop')).toBeVisible({ timeout: 10_000 });

    const panelCountBadge = page.getByText('1 entries');
    await expect(panelCountBadge).toBeVisible({ timeout: 10_000 });

    // Delete the entry
    await stopwordsList.getByRole('button', { name: /delete/i }).first().click();

    // Verify counts return to baseline
    await expect(page.getByText(DICTIONARY_EMPTY_STATES.stopwords)).toBeVisible({ timeout: 10_000 });
    await expect(stopwordRows).toHaveCount(0);
    await expect(page.getByText('0 entries')).toBeVisible({ timeout: 10_000 });
  });

  test('adds a stopword entry through the dialog', async ({ page }) => {
    await page.getByTestId('add-dictionary-entry-btn').click();

    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible({ timeout: 10_000 });

    await dialog.getByLabel('Word').fill('e2e-added-stopword');
    await dialog.getByLabel('Language').selectOption('en');
    await dialog.getByLabel('State').selectOption('enabled');

    await dialog.getByRole('button', { name: 'Add Entry' }).click();

    const stopwordsList = page.getByTestId(getListTestId('stopwords'));
    await expect(stopwordsList.getByText('e2e-added-stopword')).toBeVisible({ timeout: 10_000 });
  });

  test('plural create-delete lifecycle preserves tab state', async ({ page }) => {
    // Switch to Plurals tab
    await page.getByTestId('dictionary-tab-plurals').click();
    await expect(page.getByRole('heading', { name: DICTIONARY_LABELS.plurals })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText(DICTIONARY_EMPTY_STATES.plurals)).toBeVisible({ timeout: 10_000 });

    // Create a plural via the dialog — description should render as "shoe, shoes"
    await page.getByTestId('add-dictionary-entry-btn').click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible({ timeout: 10_000 });
    await dialog.getByLabel('Words').fill('shoe, shoes');
    await dialog.getByLabel('Language').selectOption('en');
    await dialog.getByRole('button', { name: 'Add Entry' }).click();

    const pluralsList = page.getByTestId(getListTestId('plurals'));
    await expect(pluralsList).toBeVisible({ timeout: 10_000 });
    await expect(pluralsList.getByText('shoe, shoes')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText('1 entries')).toBeVisible({ timeout: 10_000 });

    // Delete and verify empty state returns
    await pluralsList.getByRole('button', { name: /delete/i }).first().click();
    await expect(page.getByText(DICTIONARY_EMPTY_STATES.plurals)).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText('0 entries')).toBeVisible({ timeout: 10_000 });
  });

  test('compound create-delete lifecycle preserves tab state', async ({ page }) => {
    // Switch to Compounds tab
    await page.getByTestId('dictionary-tab-compounds').click();
    await expect(page.getByRole('heading', { name: DICTIONARY_LABELS.compounds })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText(DICTIONARY_EMPTY_STATES.compounds)).toBeVisible({ timeout: 10_000 });

    // Create a compound — description should render as "notebook -> note + book"
    await page.getByTestId('add-dictionary-entry-btn').click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible({ timeout: 10_000 });
    await dialog.getByLabel('Word').fill('notebook');
    await dialog.getByLabel('Decomposition').fill('note, book');
    await dialog.getByLabel('Language').selectOption('en');
    await dialog.getByRole('button', { name: 'Add Entry' }).click();

    const compoundsList = page.getByTestId(getListTestId('compounds'));
    await expect(compoundsList).toBeVisible({ timeout: 10_000 });
    await expect(compoundsList.getByText('notebook -> note + book')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText('1 entries')).toBeVisible({ timeout: 10_000 });

    // Delete and verify empty state returns
    await compoundsList.getByRole('button', { name: /delete/i }).first().click();
    await expect(page.getByText(DICTIONARY_EMPTY_STATES.compounds)).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText('0 entries')).toBeVisible({ timeout: 10_000 });
  });

  test('deletes a seeded stopword entry through the UI', async ({ page, request }) => {
    await batchDictionaryEntries(request, 'stopwords', [
      {
        objectID: 'e2e-stop-delete',
        word: 'delete-me-stopword',
        language: 'en',
        state: 'enabled',
      },
    ]);

    await page.reload();
    await expect(page.getByRole('heading', { name: 'Dictionaries' })).toBeVisible({ timeout: 15_000 });

    const stopwordsList = page.getByTestId(getListTestId('stopwords'));
    await expect(stopwordsList.getByText('delete-me-stopword')).toBeVisible({ timeout: 10_000 });

    await stopwordsList.getByRole('button', { name: /delete/i }).first().click();

    await expect(stopwordsList.getByText('delete-me-stopword')).not.toBeVisible({ timeout: 10_000 });
  });

  test('deletes a seeded plural entry through the UI', async ({ page, request }) => {
    await batchDictionaryEntries(request, 'plurals', [
      {
        objectID: 'e2e-plural-delete',
        words: ['shoe', 'shoes'],
        language: 'en',
      },
    ]);

    await page.reload();
    await expect(page.getByRole('heading', { name: 'Dictionaries' })).toBeVisible({ timeout: 15_000 });

    await page.getByTestId('dictionary-tab-plurals').click();
    await expect(page.getByRole('heading', { name: DICTIONARY_LABELS.plurals })).toBeVisible({ timeout: 10_000 });

    const pluralsList = page.getByTestId(getListTestId('plurals'));
    await expect(pluralsList.getByText('shoe, shoes')).toBeVisible({ timeout: 10_000 });

    await pluralsList.getByRole('button', { name: /delete/i }).first().click();

    await expect(pluralsList.getByText('shoe, shoes')).not.toBeVisible({ timeout: 10_000 });
  });

  test('shows an empty state when the selected dictionary has no entries', async ({ page }) => {
    await expect(page.getByText(DICTIONARY_EMPTY_STATES.stopwords)).toBeVisible({ timeout: 10_000 });
  });
});
