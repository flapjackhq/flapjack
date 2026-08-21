/* eslint-disable playwright/no-conditional-expect, playwright/no-conditional-in-test -- The shared KAT branches only on captured route and framework adapter; all three adapters run. */
import { expect, test } from '@playwright/test';
import {
  FIRST_PAGE_NAMES,
  LAPTOP_NAMES,
  NOVA_NAMES,
  SECOND_PAGE_NAMES,
} from './fixture_data.mjs';

const CLIENTS = Object.freeze(['vanilla', 'react', 'vue']);
const USER_TOKEN = '3f25cf54-46f6-4f67-9ac8-87c4a34c86f1';
const MANAGED_SEARCH_PARAMS = new Set([
  'analytics',
  'clickAnalytics',
  'facetFilters',
  'facets',
  'filters',
  'highlightPostTag',
  'highlightPreTag',
  'hitsPerPage',
  'maxValuesPerFacet',
  'page',
  'query',
  'userToken',
]);

async function expectExactHitNames(page, expectedNames) {
  const names = page.getByTestId('hit_name');
  await expect(names).toHaveCount(expectedNames.length);
  await expect.poll(() => names.allTextContents()).toEqual(expectedNames);
}

function credentialValues(url, name) {
  return [...url.searchParams.entries()]
    .filter(([candidate]) => candidate.toLowerCase() === name)
    .map(([, value]) => value);
}

function assertNativeQueryCredentials(networkRequest) {
  const url = new URL(networkRequest.url());
  const headers = networkRequest.headers();
  expect(headers['x-algolia-application-id']).toBeUndefined();
  expect(headers['x-algolia-api-key']).toBeUndefined();
  expect(headers.authorization).toBeUndefined();
  const applicationIds = credentialValues(url, 'x-algolia-application-id');
  const apiKeys = credentialValues(url, 'x-algolia-api-key');
  expect(applicationIds).toHaveLength(1);
  expect(apiKeys).toHaveLength(1);
  // Keep a failed assertion from printing credential material into a receipt.
  expect(applicationIds[0] === 'flapjack').toBe(true);
  expect(apiKeys[0] === process.env.REAL_CLIENT_SEARCH_KEY).toBe(true);
}

test('search-insights official client uses native query credentials', async ({ page }) => {
  const eventResponse = page.waitForResponse((response) => {
    return new URL(response.url()).pathname === '/1/events';
  });
  page.on('request', (networkRequest) => {
    if (new URL(networkRequest.url()).pathname !== '/1/events') return;
    assertNativeQueryCredentials(networkRequest);
    expect(networkRequest.postDataJSON().events[0]).toMatchObject({
      eventType: 'click',
      eventName: 'PBV3 Insights transport probe',
      index: process.env.REAL_CLIENT_INDEX_NAME,
      queryID: '00000000000000000000000000000000',
      objectIDs: ['transport_probe'],
      positions: [1],
      userToken: USER_TOKEN,
    });
  });

  await page.goto('/?client=vanilla&probe=insights-transport');
  expect((await eventResponse).status()).toBe(200);
  await expect(page.getByTestId('client_status')).toHaveText('search-insights request dispatched');
});

for (const clientName of CLIENTS) {
  test(`${clientName} official client uses the bounded managed-search transport`, async ({ page }) => {
    const pageErrors = [];
    const managedSearchRequests = [];
    const managedSearchResponseChecks = [];
    const insightsRequests = [];
    const analyticsDisabledRequests = [];
    const clickAnalyticsEnabledRequests = [];
    page.on('pageerror', (error) => pageErrors.push(error.message));
    page.on('request', (networkRequest) => {
      const url = new URL(networkRequest.url());
      if (url.pathname === '/1/indexes/*/queries') {
        assertNativeQueryCredentials(networkRequest);
        const body = networkRequest.postDataJSON();
        expect(Object.keys(body)).toEqual(['requests']);
        for (const query of body.requests) {
          const { indexName, ...params } = query;
          expect(indexName).toBeTruthy();
          const unauthorizedParams = Object.keys(params)
            .filter((name) => !MANAGED_SEARCH_PARAMS.has(name))
            .sort();
          expect(unauthorizedParams).toEqual([]);
          expect(params.highlightPreTag).toBe('__ais-highlight__');
          expect(params.highlightPostTag).toBe('__/ais-highlight__');
          expect(params.maxValuesPerFacet).toBe(10);
          if (Object.hasOwn(params, 'analytics')) {
            expect(params.analytics).toBe(false);
            expect(params.clickAnalytics).toBe(false);
            expect(params.hitsPerPage).toBe(0);
            expect(params.page).toBe(0);
            expect(params.facets).toBe('brand');
            analyticsDisabledRequests.push(params);
          } else {
            expect(params.clickAnalytics).toBe(true);
            clickAnalyticsEnabledRequests.push(params);
          }
          expect(params.userToken).toBe(USER_TOKEN);
        }
        managedSearchRequests.push(body);
      } else if (url.pathname === '/1/events') {
        assertNativeQueryCredentials(networkRequest);
        const body = networkRequest.postDataJSON();
        expect(body.events).toHaveLength(1);
        expect(body.events[0]).toMatchObject({
          eventType: 'click',
          eventName: 'PBV3 product clicked',
          index: process.env.REAL_CLIENT_INDEX_NAME,
          userToken: USER_TOKEN,
        });
        expect(body.events[0].objectIDs).toHaveLength(1);
        expect(body.events[0].positions).toHaveLength(1);
        expect(body.events[0].queryID).toMatch(/^[0-9a-f]{32}$/);
        insightsRequests.push(body);
      }
    });
    page.on('response', (response) => {
      const url = new URL(response.url());
      if (url.pathname === '/1/indexes/*/queries') {
        managedSearchResponseChecks.push(response.json().then((body) => {
          expect(response.status()).toBe(200);
          const requests = response.request().postDataJSON().requests;
          expect(body.results).toHaveLength(requests.length);
          for (const [index, result] of body.results.entries()) {
            if (requests[index].clickAnalytics) {
              expect(result.queryID).toMatch(/^[0-9a-f]{32}$/);
            } else {
              expect(result.queryID).toBeUndefined();
            }
          }
        }));
      }
    });

    await page.goto(`/?client=${clientName}`);
    await expect(page.getByTestId('client_heading')).toHaveText(`${clientName} InstantSearch`);
    await expect(page.getByTestId('client_status')).toHaveText(`${clientName} client mounted`);
    await expectExactHitNames(page, FIRST_PAGE_NAMES);

    // Vanilla owns the one complete backend journey. React and Vue are thin
    // binding/transport checks over the same official lite-client KAT.
    if (clientName === 'vanilla') {
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
      const eventResponse = page.waitForResponse((response) => {
        return new URL(response.url()).pathname === '/1/events';
      });
      await page.getByTestId('hit').first().click();
      expect((await eventResponse).status()).toBe(200);
      await expect.poll(() => insightsRequests.length).toBe(1);
    }

    await Promise.all(managedSearchResponseChecks);
    expect(managedSearchRequests.length).toBeGreaterThan(0);
    expect(clickAnalyticsEnabledRequests.length).toBeGreaterThan(0);
    if (clientName === 'vanilla') expect(insightsRequests).toHaveLength(1);
    if (clientName === 'vanilla') expect(analyticsDisabledRequests.length).toBeGreaterThan(0);
    expect(pageErrors).toEqual([]);
  });
}
