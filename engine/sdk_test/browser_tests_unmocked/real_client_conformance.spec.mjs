import { expect, test } from '@playwright/test';
import {
  FIRST_PAGE_NAMES,
  LAPTOP_NAMES,
  NOVA_NAMES,
  SECOND_PAGE_NAMES,
} from './fixture_data.mjs';

const CLIENTS = Object.freeze(['vanilla', 'react', 'vue']);

async function expectExactHitNames(page, expectedNames) {
  const names = page.getByTestId('hit_name');
  await expect(names).toHaveCount(expectedNames.length);
  await expect.poll(() => names.allTextContents()).toEqual(expectedNames);
}

for (const clientName of CLIENTS) {
  test(`${clientName} official client renders query, facet, and pagination results`, async ({ page }) => {
    const pageErrors = [];
    page.on('pageerror', (error) => pageErrors.push(error.message));

    // Navigation is Arrange. Every operation under test after this point is performed
    // through visible controls, exactly as an application user would perform it.
    await page.goto(`/?client=${clientName}`);
    await expect(page.getByTestId('client_heading')).toHaveText(`${clientName} InstantSearch`);
    await expect(page.getByTestId('client_status')).toHaveText(`${clientName} client mounted`);
    await expectExactHitNames(page, FIRST_PAGE_NAMES);

    const searchInput = page.getByPlaceholder('Search products');
    await searchInput.fill('laptop');
    await expectExactHitNames(page, LAPTOP_NAMES);

    await searchInput.fill('');
    await expectExactHitNames(page, FIRST_PAGE_NAMES);

    const novaCheckbox = page.getByRole('checkbox', { name: /Nova/ });
    await novaCheckbox.check();
    await expectExactHitNames(page, NOVA_NAMES);

    await novaCheckbox.uncheck();
    await expectExactHitNames(page, FIRST_PAGE_NAMES);

    await page.getByRole('link', { name: 'Page 2', exact: true }).click();
    await expectExactHitNames(page, SECOND_PAGE_NAMES);
    expect(pageErrors).toEqual([]);
  });
}
