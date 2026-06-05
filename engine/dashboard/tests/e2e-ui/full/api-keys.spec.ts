import type { Page } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import {
  createApiKey,
  createIndex,
  deleteApiKeysByDescriptionPrefix,
  deleteIndex,
} from '../../fixtures/api-helpers';

const API_KEYS_URL = '/keys';
const E2E_DESCRIPTION_PREFIX = 'E2E';

function prefixedDescription(name: string): string {
  return `${E2E_DESCRIPTION_PREFIX} ${name}`;
}

async function openCreateKeyDialog(page: Page) {
  await page.getByRole('button', { name: 'Create Key', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible({ timeout: 10_000 });
  return dialog;
}

test.describe('API Keys Page', () => {
  test.describe.configure({ mode: 'serial' });

  test.beforeEach(async ({ page, request }) => {
    await deleteApiKeysByDescriptionPrefix(request, E2E_DESCRIPTION_PREFIX);
    await page.goto(API_KEYS_URL);
    await expect(page.getByRole('heading', { name: 'API Keys', exact: true })).toBeVisible({ timeout: 10_000 });
  });

  test.afterEach(async ({ request }) => {
    await deleteApiKeysByDescriptionPrefix(request, E2E_DESCRIPTION_PREFIX);
  });

  test('loads seeded key and renders description, key value, and restrict sources', async ({ page, request }) => {
    const description = prefixedDescription('Seeded Key');
    const created = await createApiKey(request, {
      description,
      acl: ['search'],
      restrictSources: ['192.168.1.0/24'],
    });

    await page.reload();
    await expect(page.getByRole('heading', { name: 'API Keys', exact: true })).toBeVisible({ timeout: 10_000 });

    const seededCard = page.getByTestId('key-card').filter({ hasText: description });
    await expect(seededCard).toBeVisible({ timeout: 10_000 });
    await expect(seededCard.getByText(created.key)).toBeVisible({ timeout: 10_000 });
    await expect(seededCard.getByText('192.168.1.0/24')).toBeVisible({ timeout: 10_000 });
  });

  test('API keys page loads and shows heading and create button', async ({ page }) => {
    await expect(page.getByRole('heading', { name: 'API Keys', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Create Key', exact: true })).toBeVisible();
  });

  test('create key dialog shows all form sections', async ({ page }) => {
    const dialog = await openCreateKeyDialog(page);

    await expect(dialog.getByText('Description')).toBeVisible();
    await expect(dialog.getByText('Permissions').first()).toBeVisible();
    await expect(dialog.getByText('Search').first()).toBeVisible();
    await expect(dialog.getByText('Browse').first()).toBeVisible();
    await expect(dialog.getByText('Add Object').first()).toBeVisible();
    await expect(dialog.getByText('Delete Object').first()).toBeVisible();
    await expect(dialog.getByText('Delete Index').first()).toBeVisible();
    await expect(dialog.getByText('Settings').first()).toBeVisible();
    await expect(dialog.getByText('List Indexes').first()).toBeVisible();
    await expect(dialog.getByText('Analytics').first()).toBeVisible();

    await expect(dialog.getByText('Index Scope')).toBeVisible();
    await expect(dialog.getByText('Max Hits Per Query')).toBeVisible();
    await expect(dialog.getByText('Max Queries Per IP Per Hour')).toBeVisible();

    await expect(dialog.getByRole('button', { name: /cancel/i })).toBeVisible();
    await expect(dialog.getByRole('button', { name: /create key/i })).toBeVisible();

    await dialog.getByRole('button', { name: /cancel/i }).click();
  });

  test('toggling permissions updates selection badges', async ({ page }) => {
    const dialog = await openCreateKeyDialog(page);

    const permissionBadges = dialog.getByTestId('selected-permissions');
    await expect(permissionBadges.getByText('search').first()).toBeVisible();

    await dialog.getByTestId('acl-option-addObject').click();
    await expect(permissionBadges.getByText('addObject').first()).toBeVisible();

    await dialog.getByTestId('acl-option-search').click();
    await expect(permissionBadges.getByText('addObject').first()).toBeVisible();

    await dialog.getByRole('button', { name: /cancel/i }).click();
  });

  test('create a new API key increments visible key-card count by exactly one', async ({ page }) => {
    const keyCards = page.getByTestId('key-card');
    const beforeCreateCount = await keyCards.count();
    const dialog = await openCreateKeyDialog(page);

    await dialog.getByPlaceholder('e.g., Frontend search key').fill(prefixedDescription('Create List Key'));
    await dialog.getByRole('button', { name: /create key/i }).click();

    const createdCard = page.getByTestId('key-card').filter({ hasText: prefixedDescription('Create List Key') });
    await expect(createdCard).toBeVisible({ timeout: 10_000 });
    await expect(keyCards).toHaveCount(beforeCreateCount + 1, { timeout: 10_000 });
    await expect(createdCard.getByTestId('key-permissions').getByText('search')).toBeVisible({ timeout: 10_000 });
  });

  test('create then delete an API key', async ({ page }) => {
    const description = prefixedDescription('Delete Key');
    const keyCards = page.getByTestId('key-card');
    const beforeCreateCount = await keyCards.count();

    const dialog = await openCreateKeyDialog(page);
    await dialog.getByPlaceholder('e.g., Frontend search key').fill(description);
    await dialog.getByRole('button', { name: /create key/i }).click();

    const keyCard = page.getByTestId('key-card').filter({ hasText: description });
    await expect(keyCard).toBeVisible({ timeout: 10_000 });
    await expect(keyCards).toHaveCount(beforeCreateCount + 1, { timeout: 10_000 });

    const beforeDeleteCount = await keyCards.count();

    await keyCard.getByTestId('delete-key-btn').click();
    const confirmDialog = page.getByRole('dialog', { name: 'Delete API Key' });
    await expect(confirmDialog).toBeVisible({ timeout: 10_000 });
    await confirmDialog.getByRole('button', { name: 'Delete', exact: true }).click();

    await expect(page.getByTestId('key-card').filter({ hasText: description })).toHaveCount(0, { timeout: 10_000 });
    await expect(keyCards).toHaveCount(beforeDeleteCount - 1, { timeout: 10_000 });
  });

  test('creating a key through dialog preserves the exact selected permission set', async ({ page }) => {
    const description = prefixedDescription('Permissions Key');
    const expectedPermissions = ['search', 'addObject', 'analytics'];
    const dialog = await openCreateKeyDialog(page);
    await dialog.getByPlaceholder('e.g., Frontend search key').fill(description);
    await dialog.getByTestId('acl-option-addObject').click();
    await dialog.getByTestId('acl-option-analytics').click();
    await dialog.getByRole('button', { name: /create key/i }).click();

    const keyCard = page.getByTestId('key-card').filter({ hasText: description });
    await expect(keyCard).toBeVisible({ timeout: 10_000 });
    const permissions = keyCard.getByTestId('key-permissions');
    await expect(permissions.locator('div')).toHaveCount(expectedPermissions.length, { timeout: 10_000 });
    const renderedPermissions = (await permissions.locator('div').allInnerTexts())
      .map((permission) => permission.trim())
      .sort();
    expect(renderedPermissions).toEqual([...expectedPermissions].sort());
  });

  test('copy button is visible on key cards', async ({ page, request }) => {
    await createApiKey(request, {
      description: prefixedDescription('Copy Button Key'),
      acl: ['search'],
    });

    await page.reload();

    const keyCard = page.getByTestId('key-card').filter({ hasText: prefixedDescription('Copy Button Key') });
    await expect(keyCard).toBeVisible({ timeout: 10_000 });
    await expect(keyCard.getByRole('button', { name: /copy/i })).toBeVisible({ timeout: 10_000 });
  });

  test('clicking copy button shows Copied feedback and writes the expected key to clipboard', async ({ page, request, readClipboard }) => {
    await createApiKey(request, {
      description: prefixedDescription('Copy Feedback Key'),
      acl: ['search'],
    });

    await page.reload();

    const keyCard = page.getByTestId('key-card').filter({ hasText: prefixedDescription('Copy Feedback Key') });
    await expect(keyCard).toBeVisible({ timeout: 10_000 });
    const keyValue = (await keyCard.locator('code').first().innerText()).trim();

    await expect(keyCard.getByRole('button', { name: 'Copy', exact: true })).toBeVisible({ timeout: 10_000 });
    await keyCard.getByRole('button', { name: 'Copy', exact: true }).click();
    await expect(keyCard.getByRole('button', { name: /copied/i })).toBeVisible({ timeout: 10_000 });
    const clipboardValue = await readClipboard();
    expect(clipboardValue).toBe(keyValue);
  });

  test('key with no index scope shows All Indexes badge', async ({ page, request }) => {
    await createApiKey(request, {
      description: prefixedDescription('Global Scope Key'),
      acl: ['search'],
    });

    await page.reload();

    const keyCard = page.getByTestId('key-card').filter({ hasText: prefixedDescription('Global Scope Key') });
    await expect(keyCard).toBeVisible({ timeout: 10_000 });
    await expect(keyCard.getByText('All Indexes')).toBeVisible({ timeout: 10_000 });
  });

  test('index filter bar shows scoped filters and toggling preserves scoped plus global key visibility', async ({ page, request }) => {
    const isolatedIndex = `e2e-api-keys-filter-${Date.now()}`;
    const secondaryIndex = `e2e-api-keys-filter-other-${Date.now()}`;
    const scopedDescription = prefixedDescription('Filter Scoped Key');
    const globalDescription = prefixedDescription('Filter Global Key');
    const otherScopedDescription = prefixedDescription('Filter Other Scoped Key');

    await createIndex(request, isolatedIndex);
    await createIndex(request, secondaryIndex);

    try {
      await createApiKey(request, {
        description: scopedDescription,
        acl: ['search'],
        indexes: [isolatedIndex],
      });
      await createApiKey(request, {
        description: globalDescription,
        acl: ['search'],
      });
      await createApiKey(request, {
        description: otherScopedDescription,
        acl: ['search'],
        indexes: [secondaryIndex],
      });

      await page.reload();

      const filterBar = page.getByTestId('index-filter-bar');
      await expect(filterBar).toBeVisible({ timeout: 10_000 });

      const scopedFilterButton = page.getByTestId(`filter-index-${isolatedIndex}`);
      await expect(scopedFilterButton).toBeVisible({ timeout: 10_000 });
      await scopedFilterButton.click();

      await expect(page.getByTestId('key-card').filter({ hasText: scopedDescription })).toBeVisible({ timeout: 10_000 });
      await expect(page.getByTestId('key-card').filter({ hasText: globalDescription })).toBeVisible({ timeout: 10_000 });
      await expect(page.getByTestId('key-card').filter({ hasText: otherScopedDescription })).toHaveCount(0, { timeout: 10_000 });

      await page.getByTestId('filter-all').click();

      const e2eCards = page.getByTestId('key-card').filter({
        has: page.getByRole('heading', {
          name: /^E2E Filter (Scoped|Global|Other Scoped) Key$/,
        }),
      });
      await expect(e2eCards).toHaveCount(3, { timeout: 10_000 });
      await expect(page.getByTestId('key-card').filter({ hasText: otherScopedDescription })).toBeVisible({ timeout: 10_000 });
    } finally {
      await deleteApiKeysByDescriptionPrefix(request, E2E_DESCRIPTION_PREFIX);
      await deleteIndex(request, isolatedIndex);
      await deleteIndex(request, secondaryIndex);
    }
  });

  test('create key with restricted index scope shows specific index badge', async ({ page, request }) => {
    const indexUid = `e2e-api-keys-index-${Date.now()}`;
    await createIndex(request, indexUid);

    try {
      await page.reload();

      const dialog = await openCreateKeyDialog(page);
      await dialog.getByPlaceholder('e.g., Frontend search key').fill(prefixedDescription('Scoped Index Key'));
      await dialog.getByRole('button', { name: indexUid, exact: true }).click();
      await dialog.getByRole('button', { name: /create key/i }).click();

      const keyCard = page.getByTestId('key-card').filter({ hasText: prefixedDescription('Scoped Index Key') });
      await expect(keyCard).toBeVisible({ timeout: 10_000 });
      await expect(keyCard.getByText(indexUid)).toBeVisible({ timeout: 10_000 });
    } finally {
      await deleteIndex(request, indexUid);
    }
  });

  test('create key with restrict sources through the dialog shows source badges', async ({ page }) => {
    await expect(page.getByTestId('key-restrict-sources')).toHaveCount(0);

    const dialog = await openCreateKeyDialog(page);

    await dialog.getByPlaceholder('e.g., Frontend search key').fill(prefixedDescription('Restrict Sources Dialog'));
    await dialog.getByLabel(/Restrict Sources/i).fill('10.0.0.0/8, 192.168.0.0/16\n172.16.0.0/12');
    await dialog.getByRole('button', { name: /create key/i }).click();

    const keyCard = page.getByTestId('key-card').filter({ hasText: prefixedDescription('Restrict Sources Dialog') });
    await expect(keyCard).toBeVisible({ timeout: 10_000 });
    const restrictSourcesSection = keyCard.getByTestId('key-restrict-sources');
    await expect(restrictSourcesSection).toBeVisible({ timeout: 10_000 });

    const expectedSources = ['10.0.0.0/8', '192.168.0.0/16', '172.16.0.0/12'];
    const renderedSources = (await restrictSourcesSection.innerText())
      .match(/\d+\.\d+\.\d+\.\d+\/\d+/g)
      ?.sort() ?? [];
    expect(renderedSources).toEqual([...expectedSources].sort());
  });
});
