import { expect, test } from '@playwright/test';
import {
  ADMIN_KEY,
  expectAccessible,
  expectVisibleUiWithinViewport,
  readSecuritySources,
  replaceSecuritySources,
  type SecuritySource,
} from '../fixtures';

const baseline: SecuritySource = {
  source: '127.0.0.1/32',
  description: 'Browser fixture baseline',
};
const additional: SecuritySource = {
  source: '127.0.0.0/8',
  description:
    'LocalProxyRangeWithAnIntentionallyLongUnbrokenDescriptionForTheExact390PixelViewportProof0123456789',
};

test.describe('standalone engine Security Sources', () => {
  let ownsAllowlist = false;

  test.beforeEach(async ({ request }) => {
    expect(
      await readSecuritySources(request),
      'the canonical fresh-data engine must start with an empty allowlist'
    ).toEqual([]);
    ownsAllowlist = true;
    await replaceSecuritySources(request, [baseline]);
  });

  test.afterEach(async ({ request }) => {
    if (ownsAllowlist) await replaceSecuritySources(request, []);
    ownsAllowlist = false;
  });

  test('deep-links, adds, rejects malformed input, and deletes through the real engine', async ({
    page,
    request,
  }, testInfo) => {
    await page.goto('/dashboard/security-sources');
    expect(page.viewportSize()?.width).toBe(testInfo.project.name === 'mobile-390' ? 390 : 1280);
    await page.getByLabel('Admin API Key').fill(ADMIN_KEY);
    await page.getByRole('button', { name: 'Connect' }).click();
    await expect(page.getByRole('heading', { name: 'Security Sources' })).toBeVisible();
    await expect(page.getByRole('article', { name: baseline.source })).toContainText(
      baseline.description
    );
    await expect(page.getByText('1 entry')).toBeVisible();

    await page.reload();
    await expect(page.getByRole('heading', { name: 'Security Sources' })).toBeVisible();
    await expect(page.getByRole('main', { name: 'Console authentication' })).toHaveCount(0);

    const addTrigger = page.getByRole('button', { name: 'Add Source' });
    await addTrigger.click();
    let dialog = page.getByRole('dialog', { name: 'Add security source' });
    await expect(dialog.getByLabel('Source')).toBeFocused();
    await expectAccessible(page);
    await expectVisibleUiWithinViewport(page, [dialog, dialog.getByLabel('Source')]);

    await dialog.getByLabel('Source').fill('not-a-cidr');
    await dialog.getByLabel('Description').fill(additional.description);
    await dialog.getByRole('button', { name: 'Add source' }).click();
    await expect(dialog.getByRole('alert')).toHaveText('Could not add security source.');
    await expect(dialog.getByLabel('Source')).toHaveValue('not-a-cidr');
    await expect(dialog.getByLabel('Description')).toHaveValue(additional.description);
    expect(await readSecuritySources(request)).toEqual([baseline]);

    await dialog.getByLabel('Source').fill(additional.source);
    await dialog.getByRole('button', { name: 'Add source' }).click();
    await expect(page.getByRole('article', { name: additional.source })).toContainText(
      additional.description
    );
    await expect(page.getByText('2 entries')).toBeVisible();
    await expect(page.getByRole('status')).toHaveText('Security source added.');
    await expect(addTrigger).toBeFocused();
    expect(await readSecuritySources(request)).toEqual([baseline, additional]);
    await expectVisibleUiWithinViewport(page, [
      page.getByRole('article', { name: additional.source }),
      page.getByText(additional.description),
    ]);

    const addedRow = page.getByRole('article', { name: additional.source });
    await addedRow
      .getByRole('button', { name: `Delete security source ${additional.source}` })
      .click();
    await expect(addedRow).toHaveCount(0);
    await expect(page.getByText('1 entry')).toBeVisible();
    await expect(page.getByRole('status')).toHaveText('Security source deleted.');
    await expect(page.getByRole('region', { name: 'Security Sources screen' })).toBeFocused();
    expect(await readSecuritySources(request)).toEqual([baseline]);

    await expectAccessible(page);
    await expectVisibleUiWithinViewport(page, [
      page.getByRole('region', { name: 'Security Sources screen' }),
      page.getByRole('article', { name: baseline.source }),
    ]);
  });
});
