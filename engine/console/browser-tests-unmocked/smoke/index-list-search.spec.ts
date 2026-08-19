import { expect, test } from '@playwright/test';
import {
  ADMIN_KEY,
  captureNextResultOpenEvent,
  captureNextSearchExchange,
  LONG_RUST_TITLE,
  expectAccessible,
  expectVisibleUiWithinViewport,
  removeIndex,
  removeQueryEmbedders,
  seedIndex,
  type SeededIndex,
} from '../fixtures';

test.describe('standalone shared Index List and Basic Search', () => {
  let seeded: SeededIndex;

  test.beforeEach(async ({ request }, testInfo) => {
    seeded = await seedIndex(request, testInfo.project.name);
  });

  test.afterEach(async ({ request }) => {
    await removeIndex(request, seeded.name);
  });

  test('authenticates, browses, pages, and searches the real engine', async ({ page, request }, testInfo) => {
    let queryRequests = 0;
    let indexListRequests = 0;
    let eventRequests = 0;
    page.on('request', (networkRequest) => {
      if (
        networkRequest.method() === 'GET' &&
        networkRequest.url().endsWith('/1/indexes')
      ) {
        indexListRequests += 1;
      }
      if (
        networkRequest.method() === 'POST' &&
        networkRequest.url().endsWith(`/1/indexes/${seeded.name}/query`)
      ) {
        queryRequests += 1;
      }
      if (networkRequest.method() === 'POST' && networkRequest.url().endsWith('/1/events')) {
        eventRequests += 1;
      }
    });

    await page.goto('/dashboard/');
    await expect(page.getByRole('heading', { name: 'Flapjack Console' })).toBeVisible();
    expect(page.viewportSize()?.width).toBe(testInfo.project.name === 'mobile-390' ? 390 : 1280);
    await expectAccessible(page);
    await expectVisibleUiWithinViewport(page, [
      page.getByLabel('Admin API Key'),
      page.getByText('Admin API Key'),
    ]);

    await page.getByLabel('Admin API Key').fill('wrong-key');
    await page.getByRole('button', { name: 'Connect' }).click();
    await expect(page.getByRole('alert')).toHaveText('Could not authenticate.');
    await expectAccessible(page);
    await expectVisibleUiWithinViewport(page, [
      page.getByLabel('Admin API Key'),
      page.getByText('Admin API Key'),
    ]);

    await page.getByLabel('Admin API Key').fill(ADMIN_KEY);
    await page.getByRole('button', { name: 'Connect' }).click();

    const row = page.getByRole('row', {
      name: `${seeded.name} ${seeded.entries} ${seeded.dataSize} bytes`,
    });
    await expect(row).toBeVisible();
    indexListRequests = 0;
    await page.reload();
    await expect(row).toBeVisible();
    await expect(page.getByLabel('Admin API Key')).toHaveCount(0);
    expect(indexListRequests).toBe(2);
    await expectAccessible(page);
    await expectVisibleUiWithinViewport(page);

    const directRoute = `/dashboard/index/${encodeURIComponent(seeded.name)}`;
    await page.goto(directRoute);
    await expect(page.getByLabel('Admin API Key')).toHaveCount(0);
    await expect(page.getByRole('heading', { name: `Search ${seeded.name}` })).toBeVisible();
    const directQuery = page.getByRole('searchbox', { name: 'Query' });
    await expect(directQuery).toBeFocused();
    const directSemanticRatio = page.getByRole('slider', { name: 'Semantic ratio' });
    await expect(directSemanticRatio).toHaveValue('0.5');
    await directSemanticRatio.fill('0');
    await directQuery.fill('Rust');
    const directSearchExchangePromise = captureNextSearchExchange(page, seeded.name);
    await page.getByRole('button', { name: 'Search' }).click();
    const directSearchExchange = await directSearchExchangePromise;
    expect(directSearchExchange.responseOk).toBe(true);
    expect(directSearchExchange.requestBody).toEqual({
      query: 'Rust',
      page: 0,
      hitsPerPage: 20,
      analytics: false,
      mode: 'keywordSearch',
    });
    expect(directSearchExchange.userToken).toBeNull();
    await expect(page.getByText(/2 results in \d+ms/)).toBeVisible();
    const backToIndexes = page.getByRole('link', { name: 'Back to indexes' });
    await expect(backToIndexes).toHaveAttribute('href', '/dashboard/');
    await expectAccessible(page);
    await expectVisibleUiWithinViewport(page, [backToIndexes, directQuery]);
    await backToIndexes.click();
    await expect(page).toHaveURL(/\/dashboard\/$/);
    await expect(row).toBeVisible();
    queryRequests = 0;

    await row.getByRole('button', { name: `Search ${seeded.name}` }).click();
    const query = page.getByRole('searchbox', { name: 'Query' });
    await expect(query).toBeFocused();
    const trackAnalytics = page.getByRole('checkbox', { name: 'Track Analytics' });
    await expect(trackAnalytics).not.toBeChecked();
    const semanticRatio = page.getByRole('slider', { name: 'Semantic ratio' });
    await expect(semanticRatio).toHaveValue('0.5');
    await expect(page.getByText('Balanced', { exact: true })).toBeVisible();
    await expect(page.getByRole('combobox', { name: 'Query embedder' })).toHaveValue('remote');
    await expect(page.getByText('1 embedder configured; 1 can embed queries.')).toBeVisible();
    expect(queryRequests).toBe(0);

    await semanticRatio.fill('0');
    await expect(page.getByText('Keyword only', { exact: true })).toBeVisible();
    expect(queryRequests).toBe(0);
    const offSearchExchangePromise = captureNextSearchExchange(page, seeded.name);
    await page.getByRole('button', { name: 'Search' }).click();
    const offSearchExchange = await offSearchExchangePromise;
    expect(offSearchExchange.responseOk).toBe(true);
    expect(offSearchExchange.requestBody).toEqual({
      query: '',
      page: 0,
      hitsPerPage: 20,
      analytics: false,
      mode: 'keywordSearch',
    });
    expect(offSearchExchange.userToken).toBeNull();
    await expect(page.getByText(/21 results in \d+ms/)).toBeVisible();
    await expect(page.getByText('Page 1 of 2')).toBeVisible();
    await page.getByRole('button', { name: 'Next page' }).click();
    await expect(page.getByText('Page 2 of 2')).toBeVisible();
    await page.getByRole('button', { name: 'Previous page' }).click();
    await expect(page.getByText('Page 1 of 2')).toBeVisible();

    await query.fill('Rust');
    await page.getByRole('button', { name: 'Search' }).click();
    await expect(page.getByText(/2 results in \d+ms/)).toBeVisible();
    const rustLanguage = page.getByRole('article', { name: 'Rust Programming Language' });
    const rustAsync = page.getByRole('article', { name: LONG_RUST_TITLE });
    await expect(rustLanguage).toBeVisible();
    await expect(rustAsync).toBeVisible();
    await expect(rustLanguage.getByText(/"objectID": "rust-language"/)).toBeHidden();
    await expect(rustAsync.getByText(/"objectID": "rust-async"/)).toBeHidden();
    const openDetails = rustLanguage.getByText('Open details');
    await openDetails.focus();
    await page.keyboard.press('Enter');
    await expect(rustLanguage.getByText('Close details')).toBeFocused();
    await expect(rustLanguage.getByText(/"objectID": "rust-language"/)).toBeVisible();
    await expect(rustLanguage.getByText(/"title": "Rust Programming Language"/)).toBeVisible();
    await expect(rustAsync.getByText(/"objectID": "rust-async"/)).toBeHidden();
    expect(eventRequests).toBe(0);
    await page.keyboard.press('Enter');
    await expect(rustLanguage.getByText('Open details')).toBeFocused();

    await trackAnalytics.click();
    await expect(trackAnalytics).toBeChecked();
    await expect(
      page.getByText('Preview activity recording is on. Run a new search to record result opens.', {
        exact: true,
      })
    ).toBeVisible();
    const onSearchExchangePromise = captureNextSearchExchange(page, seeded.name);
    await page.getByRole('button', { name: 'Search' }).click();
    const onSearchExchange = await onSearchExchangePromise;
    expect(onSearchExchange.responseOk).toBe(true);
    expect(onSearchExchange.requestBody).toEqual({
      query: 'Rust',
      page: 0,
      hitsPerPage: 20,
      analytics: true,
      clickAnalytics: true,
      mode: 'keywordSearch',
    });
    const previewToken = onSearchExchange.userToken;
    expect(previewToken).toMatch(
      /^dashboard-[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
    );
    expect(onSearchExchange.responseQueryId).toMatch(/^[0-9a-f]{32}$/);

    const eventExchangePromise = captureNextResultOpenEvent(page);
    await rustLanguage.getByText('Open details').click();
    const eventExchange = await eventExchangePromise;
    expect(eventExchange.responseOk).toBe(true);
    expect(eventRequests).toBe(1);
    expect(eventExchange.requestBody).toEqual({
      events: [
        {
          eventType: 'click',
          eventName: 'Result Clicked',
          index: seeded.name,
          userToken: previewToken,
          queryID: onSearchExchange.responseQueryId,
          objectIDs: ['rust-language'],
          positions: [1],
          timestamp: expect.any(Number),
        },
      ],
    });
    await expect(page.getByText('Recorded result open.', { exact: true })).toBeVisible();
    await expect(page.getByText('Page 1 of 1')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Next page' })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Previous page' })).toHaveCount(0);
    expect(queryRequests).toBe(5);

    await trackAnalytics.click();
    await semanticRatio.fill('0.6');
    await expect(page.getByText('60% semantic')).toBeVisible();
    await removeQueryEmbedders(request, seeded.name);
    const fallbackExchangePromise = captureNextSearchExchange(page, seeded.name);
    await page.getByRole('button', { name: 'Search' }).click();
    const fallbackExchange = await fallbackExchangePromise;
    expect(fallbackExchange.responseOk).toBe(true);
    expect(fallbackExchange.requestBody).toEqual({
      query: 'Rust',
      page: 0,
      hitsPerPage: 20,
      analytics: false,
      mode: 'neuralSearch',
      hybrid: { semanticRatio: 0.6, embedder: 'remote' },
    });
    expect(fallbackExchange.userToken).toBeNull();
    expect(fallbackExchange.responseQueryId).toBeUndefined();
    await expect(page.getByText('Semantic search was unavailable; keyword results are shown.')).toBeVisible();
    await expect(page.getByText(/2 results in \d+ms/)).toBeVisible();
    await rustLanguage.getByText('Open details').click();
    await expect(rustLanguage.getByText(/"objectID": "rust-language"/)).toBeVisible();
    expect(eventRequests).toBe(1);
    expect(queryRequests).toBe(6);

    await expectAccessible(page);
    await expectVisibleUiWithinViewport(page, [
      page.getByText('Query', { exact: true }),
      page.getByText(/2 results in \d+ms/),
      rustLanguage,
      rustLanguage.getByText(/"title": "Rust Programming Language"/),
      rustAsync,
      page.getByText('Page 1 of 1'),
    ]);

    await page.getByRole('button', { name: 'Sign out' }).click();
    await expect(page.getByLabel('Admin API Key')).toBeVisible();
    await page.reload();
    await expect(page.getByLabel('Admin API Key')).toBeVisible();
  });
});
