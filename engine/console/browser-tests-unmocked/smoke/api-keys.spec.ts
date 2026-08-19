import { expect, test } from '@playwright/test';
import {
  ADMIN_KEY,
  expectAccessible,
  expectVisibleUiWithinViewport,
  removeApiKeysByDescription,
  removeIndex,
  seedIndex,
  type SeededIndex,
} from '../fixtures';

test.describe('standalone engine API Keys', () => {
  let description: string;
  let seeded: SeededIndex;

  test.beforeEach(async ({ request }, testInfo) => {
    description = `P4b ${testInfo.project.name} browser key`;
    await removeApiKeysByDescription(request, description);
    seeded = await seedIndex(request, `${testInfo.project.name}-keys`);
  });

  test.afterEach(async ({ request }) => {
    await removeApiKeysByDescription(request, description);
    await removeIndex(request, seeded.name);
  });

  test('deep-links, creates, filters, copies, and deletes through the real engine', async ({
    context,
    page,
  }, testInfo) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await page.goto('/dashboard/keys');
    expect(page.viewportSize()?.width).toBe(testInfo.project.name === 'mobile-390' ? 390 : 1280);
    await page.getByLabel('Admin API Key').fill(ADMIN_KEY);
    await page.getByRole('button', { name: 'Connect' }).click();
    await expect(page.getByRole('heading', { name: 'API Keys' })).toBeVisible();

    await page.reload();
    await expect(page.getByRole('heading', { name: 'API Keys' })).toBeVisible();
    await expect(page.getByRole('main', { name: 'Console authentication' })).toHaveCount(0);
    await page.getByRole('link', { name: 'Indexes' }).click();
    await expect(
      page.getByRole('row', {
        name: `${seeded.name} ${seeded.entries} ${seeded.dataSize} bytes`,
      })
    ).toBeVisible();
    await page.goBack();
    await expect(page.getByRole('heading', { name: 'API Keys' })).toBeVisible();

    const createButton = page.getByRole('button', { name: 'Create API Key' });
    await createButton.click();
    let createDialog = page.getByRole('dialog', { name: 'Create engine API key' });
    await expect(createDialog).toBeVisible();
    await expect(createDialog.getByLabel('Description')).toBeFocused();
    await expectAccessible(page);
    await expectVisibleUiWithinViewport(page, [
      createDialog,
      createDialog.getByText('Permissions'),
      createDialog.getByText('Index scope'),
    ]);
    await page.keyboard.press('Escape');
    await expect(createButton).toBeFocused();

    await createButton.click();
    createDialog = page.getByRole('dialog', { name: 'Create engine API key' });
    await createDialog.getByLabel('Description').fill(description);
    await createDialog.getByLabel('analytics').click();
    await createDialog.getByLabel(`Index ${seeded.name}`).click();
    await createDialog.getByLabel('Restrict sources').fill('192.168.0.0/16');
    await createDialog.getByLabel('Max hits per query').fill('25');
    await createDialog.getByRole('button', { name: 'Create key' }).click();

    const keyCard = page.getByRole('article', { name: description });
    await expect(keyCard).toBeVisible();
    await expect(keyCard.getByText('search, analytics')).toBeVisible();
    await expect(keyCard.getByText(seeded.name)).toBeVisible();
    await expect(keyCard.getByText('192.168.0.0/16')).toBeVisible();
    await expect(keyCard.getByText('25 hits/query')).toBeVisible();

    await keyCard.getByRole('button', { name: `Copy ${description}` }).click();
    await expect(keyCard.getByRole('status')).toHaveText('Copied');
    await page.getByLabel('Filter by index').selectOption(seeded.name);
    await expect(keyCard).toBeVisible();
    await expectAccessible(page);
    await expectVisibleUiWithinViewport(page, [
      page.getByText('Filter by index'),
      keyCard.getByText('Key value'),
      keyCard.getByText('Permissions'),
      keyCard.getByText('Index scope'),
    ]);

    const deleteButton = keyCard.getByRole('button', { name: `Delete ${description}` });
    await deleteButton.click();
    let deleteDialog = page.getByRole('dialog', { name: 'Delete engine API key' });
    await expectAccessible(page);
    await expectVisibleUiWithinViewport(page, [deleteDialog]);
    await deleteDialog.getByRole('button', { name: 'Cancel' }).click();
    await expect(deleteButton).toBeFocused();

    await deleteButton.click();
    deleteDialog = page.getByRole('dialog', { name: 'Delete engine API key' });
    await deleteDialog.getByRole('button', { name: 'Delete key' }).click();
    await expect(keyCard).toHaveCount(0);
    await expect(page.getByRole('region', { name: 'API Keys screen' })).toBeFocused();
  });
});
