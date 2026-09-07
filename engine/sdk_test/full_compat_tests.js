/*
§24 SDK + Widget Coverage Matrix (ALGOLIA_PARITY_IMPLEMENTATION_CHECKLIST.md lines 948-976)

SDKs:
- algoliasearch (JS/TS): Covered by contract_tests.js + full_compat_tests.js + instantsearch_contract_tests.js
- Python: Official synchronous client proof in python_client_contract_test.sh; separate python_smoke_test.sh (protocol-level)
- PHP: Covered by php_smoke_test.sh (protocol-level)
- Ruby/Go/Java/Swift: GAP (planned Stage 2 protocol edge-case verification)

InstantSearch libs:
- InstantSearch.js 4.111, React InstantSearch 7.44, and Vue InstantSearch 4.29:
  rendered browser coverage lives in browser_tests_unmocked/real_client_conformance.spec.mjs.
- This file and instantsearch_contract_tests.js cover API contracts only; they are not
  evidence that a framework client rendered successfully.
- Angular/Android/iOS InstantSearch: no rendered-client proof in this repository.

Critical widgets:
- RangeSlider: Covered (instantsearch_contract_tests.js: "RangeSlider contract facets_stats shape")
- SortBy: Covered (instantsearch_contract_tests.js: "SortBy contract via replica switching")
- HierarchicalMenu: Covered (instantsearch_contract_tests.js: "HierarchicalMenu contract with lvl0/lvl1 facets")
- GeoSearch: Covered (instantsearch_contract_tests.js: "GeoSearch contract returns _geoloc hits")
- Autocomplete: Covered (instantsearch_contract_tests.js: "Autocomplete contract for multi-index search")
- Other widgets:
  SearchBox/Hits, RefinementList, Pagination, CurrentRefinements, Stats: Covered
  NumericMenu, ClearRefinements, HitsPerPage, PoweredBy: GAP (filled in this stage)
*/

import { createFlapjackClient, FLAPJACK_URL, FLAPJACK_ADMIN_KEY } from './lib/flapjack-client.js';
import {
  assert,
  createTestRunner,
  bindClientHelpers
} from './lib/test-helpers.js';

const client = createFlapjackClient();

const RUN_ID = Date.now();
const SELECTED_EVENT_USER_TOKEN = '3f25cf54-46f6-4f67-9ac8-87c4a34c86f1';
const TEST_INDEX = `compat_test_${RUN_ID}`;
const REPLICA_INDEX = `${TEST_INDEX}_price_asc`;
const BROWSE_INDEX = `${TEST_INDEX}_browse`;
const LIFECYCLE_INDEX = `${TEST_INDEX}_lifecycle`;
const SETTINGS_ROUNDTRIP_INDEX = `${TEST_INDEX}_settings_rt`;
const MULTI_PRIMARY_INDEX = `${TEST_INDEX}_multi_primary`;
const MULTI_SECONDARY_INDEX = `${TEST_INDEX}_multi_secondary`;

const ALL_INDEXES = [
  TEST_INDEX,
  REPLICA_INDEX,
  BROWSE_INDEX,
  LIFECYCLE_INDEX,
  SETTINGS_ROUNDTRIP_INDEX,
  MULTI_PRIMARY_INDEX,
  MULTI_SECONDARY_INDEX
];

const ADMIN_HEADERS = {
  'x-algolia-api-key': FLAPJACK_ADMIN_KEY,
  'x-algolia-application-id': 'flapjack'
};

const ADMIN_JSON_HEADERS = {
  ...ADMIN_HEADERS,
  'Content-Type': 'application/json'
};

const { test, runAllTests } = createTestRunner();
const { waitForSearch, waitForObject, waitForMinHits, cleanupIndexes } = bindClientHelpers(client);

function waitForIndexing(indexName, expectedCount, maxWaitMs) {
  return waitForMinHits(indexName, expectedCount, maxWaitMs);
}

async function setSettingsAndWait(indexName, indexSettings) {
  const setResponse = await client.setSettings({ indexName, indexSettings });
  assert(
    setResponse && typeof setResponse.taskID === 'number',
    `Expected numeric taskID from setSettings for ${indexName}, got ${JSON.stringify(setResponse)}`
  );
  await client.waitForTask({ indexName, taskID: setResponse.taskID });
}

async function apiRequest(path, { method = 'GET', body, expectOk = true } = {}) {
  const response = await fetch(`${FLAPJACK_URL}${path}`, {
    method,
    headers: body === undefined ? ADMIN_HEADERS : ADMIN_JSON_HEADERS,
    body: body === undefined ? undefined : JSON.stringify(body)
  });

  if (!expectOk && !response.ok) {
    return { response, data: null, text: await response.text() };
  }

  const raw = await response.text();
  let data = null;
  if (raw.length > 0) {
    try {
      data = JSON.parse(raw);
    } catch (e) {
      if (expectOk) {
        throw new Error(`Expected JSON response for ${method} ${path}, got: ${raw}`);
      }
    }
  }

  if (expectOk && !response.ok) {
    throw new Error(`${method} ${path} failed (${response.status}): ${raw}`);
  }

  return { response, data, text: raw };
}

async function cleanup() {
  await cleanupIndexes(ALL_INDEXES);
}

// Start with cleanup to ensure a clean state.
await cleanup();

test('faceted search with facets_stats', async () => {
  await setSettingsAndWait(TEST_INDEX, {
    attributesForFaceting: ['brand', 'price']
  });

  await client.saveObjects({
    indexName: TEST_INDEX,
    objects: [
      { objectID: 'facet_product_1', name: 'Laptop A', brand: 'ABC', price: 1000 },
      { objectID: 'facet_product_2', name: 'Laptop B', brand: 'XYZ', price: 1200 },
      { objectID: 'facet_product_3', name: 'Phone C', brand: 'ABC', price: 800 },
      { objectID: 'facet_product_4', name: 'Tablet D', brand: 'DEF', price: 600 }
    ]
  });

  await waitForIndexing(TEST_INDEX, 4);

  const result = await client.search({
    requests: [{
      indexName: TEST_INDEX,
      query: '',
      facets: ['brand', 'price']
    }]
  });

  const response = result.results[0];

  assert(response.facets && response.facets.brand, 'Missing facets.brand in response');
  assert(
    response.facets.brand.ABC === 2 && response.facets.brand.XYZ === 1 && response.facets.brand.DEF === 1,
    `Expected brand counts ABC:2 XYZ:1 DEF:1, got ${JSON.stringify(response.facets.brand)}`
  );

  assert(response.facets_stats && response.facets_stats.price, 'Missing facets_stats.price in response');

  const priceStats = response.facets_stats.price;
  assert(
    typeof priceStats.min === 'number' &&
      typeof priceStats.max === 'number' &&
      typeof priceStats.avg === 'number' &&
      typeof priceStats.sum === 'number',
    `Expected facets_stats.price values to be numbers, got ${JSON.stringify(priceStats)}`
  );

  assert(
    priceStats.min === 600 && priceStats.max === 1200,
    `Expected min:600 max:1200, got min:${priceStats.min} max:${priceStats.max}`
  );

  assert(priceStats.avg === 900, `Expected avg:900, got ${priceStats.avg}`);
  assert(priceStats.sum === 3600, `Expected sum:3600, got ${priceStats.sum}`);
});

test('rules - create and verify consequence', async () => {
  await client.saveObjects({
    indexName: TEST_INDEX,
    objects: [
      { objectID: 'rule_test_product', name: 'Promotional Product', description: 'Special offer' }
    ]
  });

  const readyObject = await waitForObject(
    TEST_INDEX,
    'rule_test_product',
    (obj) => obj.name === 'Promotional Product'
  );
  assert(readyObject, 'Timed out waiting for rule_test_product to index');

  await apiRequest(`/1/indexes/${TEST_INDEX}/rules/rule_promo_banner`, {
    method: 'PUT',
    body: {
      objectID: 'rule_promo_banner',
      condition: {
        pattern: 'promo',
        anchoring: 'is'
      },
      consequence: {
        userData: {
          banner: 'special sale'
        }
      }
    }
  });

  const { data: ruleData } = await apiRequest(`/1/indexes/${TEST_INDEX}/rules/search`, {
    method: 'POST',
    body: { query: 'rule_promo_banner' }
  });

  assert(ruleData.nbHits === 1, `Expected exactly 1 matching rule, got: ${ruleData.nbHits}`);
  assert(
    ruleData.hits[0]?.objectID === 'rule_promo_banner',
    `Expected rule_promo_banner in search hits, got: ${JSON.stringify(ruleData.hits)}`
  );

  const ruleApplied = await waitForSearch(
    TEST_INDEX,
    { query: 'promo' },
    (result) =>
      Array.isArray(result.userData) &&
      result.userData.some((entry) => entry && entry.banner === 'special sale'),
    5000
  );

  assert(ruleApplied, 'Expected userData.banner from rule consequence in search response');
});

test('rules - batch save and search', async () => {
  await apiRequest(`/1/indexes/${TEST_INDEX}/rules/batch`, {
    method: 'POST',
    body: [
      {
        objectID: 'rule_batch_1',
        condition: { pattern: 'test1', anchoring: 'is' },
        consequence: { userData: { tag: 'tag1' } }
      },
      {
        objectID: 'rule_batch_2',
        condition: { pattern: 'test2', anchoring: 'is' },
        consequence: { userData: { tag: 'tag2' } }
      }
    ]
  });

  const { data } = await apiRequest(`/1/indexes/${TEST_INDEX}/rules/search`, {
    method: 'POST',
    body: { query: 'rule_batch_' }
  });

  assert(data.nbHits === 2, `Expected nbHits to be 2 for rule_batch_ query, got ${data.nbHits}`);

  const ids = new Set((data.hits || []).map((hit) => hit.objectID));
  assert(ids.has('rule_batch_1') && ids.has('rule_batch_2'), `Expected both batch rules, got: ${JSON.stringify(data.hits)}`);
});

test('synonyms - create and verify match', async () => {
  await client.saveObjects({
    indexName: TEST_INDEX,
    objects: [
      { objectID: 'laptop_doc', name: 'This is a laptop computer' },
      { objectID: 'apple_laptop_doc', name: 'Premium apple laptop device' },
      { objectID: 'mac_only_doc', name: 'Top tier mac machine' }
    ]
  });

  const readyObject = await waitForObject(
    TEST_INDEX,
    'apple_laptop_doc',
    (obj) => obj.name === 'Premium apple laptop device'
  );
  assert(readyObject, 'Timed out waiting for synonym fixture documents to index');

  const taskResponse = await client.saveSynonyms({
    indexName: TEST_INDEX,
    synonymHit: [
      {
        objectID: 'multi_way_synonym',
        type: 'synonym',
        synonyms: ['laptop', 'notebook', 'computer']
      },
      {
        objectID: 'one_way_synonym',
        type: 'onewaysynonym',
        input: 'mac',
        synonyms: ['apple laptop']
      }
    ]
  });

  assert(taskResponse.taskID, `Expected taskID from saveSynonyms, got: ${JSON.stringify(taskResponse)}`);

  const notebookResult = await waitForSearch(
    TEST_INDEX,
    { query: 'notebook' },
    (result) => result.hits.some((hit) => hit.objectID === 'laptop_doc'),
    5000
  );
  assert(notebookResult, "Expected 'notebook' query to return laptop_doc via multi-way synonym");

  const macSearch = await client.search({
    requests: [{ indexName: TEST_INDEX, query: 'mac' }]
  });
  const macHits = macSearch.results[0].hits;
  assert(
    macHits.some((hit) => hit.objectID === 'apple_laptop_doc'),
    "Expected one-way synonym input 'mac' to match apple_laptop_doc"
  );

  const reverseSearch = await client.search({
    requests: [{ indexName: TEST_INDEX, query: 'apple laptop' }]
  });
  const reverseHitIds = new Set(reverseSearch.results[0].hits.map((hit) => hit.objectID));
  assert(
    !reverseHitIds.has('mac_only_doc'),
    "Expected one-way synonym to be directional; reverse query should not match mac_only_doc"
  );
});

test('browse with 1000+ docs - full cursor pagination', async () => {
  const documents = [];
  for (let i = 0; i < 1100; i++) {
    documents.push({
      objectID: `doc_${i}`,
      name: `Document ${i}`,
      content: `Content for document number ${i}`,
      position: i
    });
  }

  for (let i = 0; i < documents.length; i += 500) {
    await client.saveObjects({
      indexName: BROWSE_INDEX,
      objects: documents.slice(i, i + 500)
    });
  }

  await waitForIndexing(BROWSE_INDEX, 1100, 15000);

  const allHits = [];
  let cursor;
  let pageCount = 0;

  do {
    const browseParams = { hitsPerPage: 100 };
    if (cursor) {
      browseParams.cursor = cursor;
    }

    const pageResult = await client.browse({
      indexName: BROWSE_INDEX,
      browseParams
    });

    if (Array.isArray(pageResult.hits) && pageResult.hits.length > 0) {
      allHits.push(...pageResult.hits);
    }

    cursor = pageResult.cursor;
    pageCount++;
  } while (cursor);

  assert(allHits.length === 1100, `Expected 1100 hits from browse, got ${allHits.length}`);
  assert(pageCount > 1, `Expected multiple browse pages, got ${pageCount}`);
  assert(cursor === undefined || cursor === null, `Expected exhausted cursor, got ${JSON.stringify(cursor)}`);

  const ids = allHits.map((hit) => hit.objectID);
  const uniqueIds = new Set(ids);
  assert(uniqueIds.size === 1100, `Expected 1100 unique objectIDs, got ${uniqueIds.size}`);
});

test('multi-index search isolates per-request params and preserves request ordering', async () => {
  await Promise.all([
    setSettingsAndWait(MULTI_PRIMARY_INDEX, {
      attributesForFaceting: ['channel'],
      customRanking: ['asc(rank)']
    }),
    setSettingsAndWait(MULTI_SECONDARY_INDEX, {
      attributesForFaceting: ['channel'],
      customRanking: ['asc(rank)']
    })
  ]);

  await client.saveObjects({
    indexName: MULTI_PRIMARY_INDEX,
    objects: [
      { objectID: 'multi_primary_rank_2', name: 'isolated primary result', channel: 'primary', rank: 2 },
      { objectID: 'multi_primary_rank_1', name: 'isolated primary result', channel: 'primary', rank: 1 }
    ]
  });

  await client.saveObjects({
    indexName: MULTI_SECONDARY_INDEX,
    objects: [
      { objectID: 'multi_secondary_rank_2', name: 'isolated secondary result', channel: 'secondary', rank: 2 },
      { objectID: 'multi_secondary_rank_1', name: 'isolated secondary result', channel: 'secondary', rank: 1 }
    ]
  });

  await Promise.all([
    waitForIndexing(MULTI_PRIMARY_INDEX, 2, 5000),
    waitForIndexing(MULTI_SECONDARY_INDEX, 2, 5000)
  ]);

  const response = await client.search({
    requests: [
      { indexName: MULTI_PRIMARY_INDEX, query: 'isolated', filters: 'channel:primary' },
      { indexName: MULTI_SECONDARY_INDEX, query: 'isolated', filters: 'channel:secondary' },
      { indexName: MULTI_PRIMARY_INDEX, query: 'isolated', filters: 'channel:primary', hitsPerPage: 1 }
    ]
  });

  assert(response.results.length === 3, `Expected 3 results arrays, got ${response.results.length}`);

  const [primaryResult, secondaryResult, limitedPrimaryResult] = response.results;
  const primaryIds = primaryResult.hits.map((hit) => hit.objectID);
  const secondaryIds = secondaryResult.hits.map((hit) => hit.objectID);

  assert(primaryResult.nbHits === 2, `Expected 2 primary hits, got ${primaryResult.nbHits}`);
  assert(secondaryResult.nbHits === 2, `Expected 2 secondary hits, got ${secondaryResult.nbHits}`);
  assert(limitedPrimaryResult.hits.length === 1, `Expected hitsPerPage isolation (1 hit), got ${limitedPrimaryResult.hits.length}`);

  assert(primaryIds.every((id) => id.startsWith('multi_primary_')), `Primary request leaked cross-index hits: ${JSON.stringify(primaryIds)}`);
  assert(secondaryIds.every((id) => id.startsWith('multi_secondary_')), `Secondary request leaked cross-index hits: ${JSON.stringify(secondaryIds)}`);

  assert(primaryIds[0] === 'multi_primary_rank_1', `Expected primary rank ordering by asc(rank), got ${JSON.stringify(primaryIds)}`);
  assert(secondaryIds[0] === 'multi_secondary_rank_1', `Expected secondary rank ordering by asc(rank), got ${JSON.stringify(secondaryIds)}`);
  assert(limitedPrimaryResult.hits[0].objectID === primaryIds[0], 'Expected request ordering and per-request params to stay aligned');
});

test('replicas - create and search with different sort', async () => {
  const replicaQuery = `replica-marker-${RUN_ID}`;

  await setSettingsAndWait(TEST_INDEX, {
    replicas: [REPLICA_INDEX],
    customRanking: ['desc(price)']
  });

  const products = [
    { objectID: 'replica_prod_high', name: `High Price Product ${replicaQuery}`, price: 1000 },
    { objectID: 'replica_prod_low', name: `Low Price Product ${replicaQuery}`, price: 100 },
    { objectID: 'replica_prod_med', name: `Medium Price Product ${replicaQuery}`, price: 500 }
  ];

  await client.saveObjects({
    indexName: TEST_INDEX,
    objects: products
  });

  const primaryReady = await waitForSearch(
    TEST_INDEX,
    { query: replicaQuery },
    (result) => result.nbHits === 3,
    5000
  );
  assert(primaryReady, 'Timed out waiting for replica test documents in primary index');

  await setSettingsAndWait(REPLICA_INDEX, {
    customRanking: ['asc(price)']
  });

  const replicaReady = await waitForSearch(
    REPLICA_INDEX,
    { query: replicaQuery },
    (result) => result.nbHits === 3,
    10000
  );
  assert(replicaReady, 'Timed out waiting for replica test documents in replica index');

  const [primaryResults, replicaResults] = await Promise.all([
    client.search({
      requests: [{ indexName: TEST_INDEX, query: replicaQuery }]
    }),
    client.search({
      requests: [{ indexName: REPLICA_INDEX, query: replicaQuery }]
    })
  ]);

  const primaryHits = primaryResults.results[0].hits;
  const replicaHits = replicaResults.results[0].hits;

  assert(primaryHits.length === 3, `Expected 3 filtered hits from primary index, got ${primaryHits.length}`);
  assert(replicaHits.length === 3, `Expected 3 filtered hits from replica index, got ${replicaHits.length}`);

  const primaryPrices = primaryHits.map((hit) => hit.price);
  const replicaPrices = replicaHits.map((hit) => hit.price);
  assert(
    primaryPrices[0] >= primaryPrices[1] && primaryPrices[1] >= primaryPrices[2],
    `Primary results are not sorted by descending price: ${JSON.stringify(primaryPrices)}`
  );
  assert(
    replicaPrices[0] <= replicaPrices[1] && replicaPrices[1] <= replicaPrices[2],
    `Replica results are not sorted by ascending price: ${JSON.stringify(replicaPrices)}`
  );
  assert(
    primaryHits[0].objectID !== replicaHits[0].objectID,
    `Expected primary and replica to produce different top hit, got ${primaryHits[0].objectID}`
  );
});

test('insights - click, conversion, and view events accepted', async () => {
  const timestampMs = Date.now();

  const { data } = await apiRequest('/1/events', {
    method: 'POST',
    body: {
      events: [
        {
          eventType: 'click',
          eventName: 'product_clicked',
          index: TEST_INDEX,
          userToken: 'test_user_123',
          objectIDs: ['product123'],
          positions: [1],
          timestamp: timestampMs
        },
        {
          eventType: 'conversion',
          eventName: 'purchase_made',
          index: TEST_INDEX,
          userToken: 'test_user_123',
          objectIDs: ['product456'],
          timestamp: timestampMs + 1
        },
        {
          eventType: 'view',
          eventName: 'product_viewed',
          index: TEST_INDEX,
          userToken: 'test_user_123',
          objectIDs: ['product789'],
          timestamp: timestampMs + 2
        }
      ]
    }
  });

  assert(data.status === 200, `Expected insights status=200, got ${JSON.stringify(data)}`);
  assert(data.message === 'OK', `Expected insights message='OK', got ${JSON.stringify(data)}`);
});

test('clickAnalytics end-to-end', async () => {
  const searchObjectId = 'search_hit_1';
  const clickEventName = `product_clicked_from_search_${RUN_ID}`;

  await client.saveObjects({
    indexName: TEST_INDEX,
    objects: [
      {
        objectID: searchObjectId,
        name: `Sample Product ${RUN_ID}`,
        category: 'Electronics',
        price: 299
      }
    ]
  });

  const readyObject = await waitForObject(
    TEST_INDEX,
    searchObjectId,
    (obj) => obj.name === `Sample Product ${RUN_ID}`
  );
  assert(readyObject, 'Timed out waiting for clickAnalytics fixture document');

  const searchResponse = await client.search({
    requests: [{
      indexName: TEST_INDEX,
      query: `${RUN_ID}`,
      clickAnalytics: true
    }]
  });

  const searchResult = searchResponse.results[0];
  assert(
    typeof searchResult.queryID === 'string' && searchResult.queryID.length > 0,
    `Expected non-empty queryID, got: ${JSON.stringify(searchResult.queryID)}`
  );

  const expectedQueryID = searchResult.queryID;

  const { data: clickResult } = await apiRequest('/1/events', {
    method: 'POST',
    body: {
      events: [{
        eventType: 'click',
        eventName: clickEventName,
        index: TEST_INDEX,
        userToken: SELECTED_EVENT_USER_TOKEN,
        objectIDs: [searchObjectId],
        positions: [1],
        queryID: expectedQueryID,
        timestamp: Date.now()
      }]
    }
  });

  assert(clickResult.status === 200, `Expected click event status=200, got ${JSON.stringify(clickResult)}`);
  assert(clickResult.message === 'OK', `Expected click event message='OK', got ${JSON.stringify(clickResult)}`);

  const { data: debugData } = await apiRequest(
    `/1/events/debug?index=${encodeURIComponent(TEST_INDEX)}&event_type=click&status=ok&limit=200`
  );

  assert(Array.isArray(debugData.events), `Expected debug response.events array, got ${JSON.stringify(debugData)}`);
  const foundClickEvent = debugData.events.some(
    (event) =>
      event.eventName === clickEventName &&
      event.eventType === 'click' &&
      event.index === TEST_INDEX &&
      Array.isArray(event.objectIds) &&
      event.objectIds.includes(searchObjectId)
  );

  assert(foundClickEvent, `Expected click event '${clickEventName}' in debug events`);
});

test('SDK full lifecycle (saveObjects -> waitTask -> search -> verify response)', async () => {
  const objects = [
    { objectID: 'lifecycle_1', name: 'Lifecycle Phone', category: 'electronics' },
    { objectID: 'lifecycle_2', name: 'Lifecycle Laptop', category: 'electronics' },
    { objectID: 'lifecycle_3', name: 'Lifecycle Case', category: 'accessories' }
  ];

  const saveResponse = await client.saveObjects({
    indexName: LIFECYCLE_INDEX,
    objects
  });

  const firstTask = Array.isArray(saveResponse) ? saveResponse[0] : saveResponse;
  assert(firstTask && typeof firstTask.taskID === 'number', `Expected taskID from saveObjects, got ${JSON.stringify(saveResponse)}`);

  await client.waitForTask({ indexName: LIFECYCLE_INDEX, taskID: firstTask.taskID });

  const searchResponse = await client.search({
    requests: [{
      indexName: LIFECYCLE_INDEX,
      query: 'Lifecycle'
    }]
  });

  const result = searchResponse.results[0];
  assert(Array.isArray(result.hits), 'Expected hits array in search response');
  assert(typeof result.nbHits === 'number', `Expected nbHits number, got ${JSON.stringify(result.nbHits)}`);
  assert(typeof result.page === 'number', `Expected page number, got ${JSON.stringify(result.page)}`);
  assert(typeof result.nbPages === 'number', `Expected nbPages number, got ${JSON.stringify(result.nbPages)}`);
  assert(typeof result.hitsPerPage === 'number', `Expected hitsPerPage number, got ${JSON.stringify(result.hitsPerPage)}`);
  assert(typeof result.processingTimeMS === 'number', `Expected processingTimeMS number, got ${JSON.stringify(result.processingTimeMS)}`);
  assert(typeof result.query === 'string', `Expected query string, got ${JSON.stringify(result.query)}`);
  assert(typeof result.params === 'string', `Expected params string, got ${JSON.stringify(result.params)}`);
  assert(typeof result.exhaustiveNbHits === 'boolean', `Expected exhaustiveNbHits boolean, got ${JSON.stringify(result.exhaustiveNbHits)}`);
  assert(result.hits.length >= 1, `Expected at least one hit, got ${result.hits.length}`);
  assert(typeof result.hits[0].objectID === 'string', `Expected hit.objectID string, got ${JSON.stringify(result.hits[0])}`);
  assert(result.hits[0]._highlightResult, `Expected hit._highlightResult, got ${JSON.stringify(result.hits[0])}`);
});

test('settings full round-trip (all fields preserved)', async () => {
  const beforeSettings = await client.getSettings({ indexName: SETTINGS_ROUNDTRIP_INDEX });
  assert(typeof beforeSettings === 'object' && beforeSettings !== null, `Expected initial getSettings object, got ${JSON.stringify(beforeSettings)}`);

  const settingsPayload = {
    searchableAttributes: ['name', 'description'],
    attributesForFaceting: ['category', 'brand'],
    customRanking: ['desc(popularity)', 'asc(price)'],
    attributesToRetrieve: ['name', 'price', 'category'],
    unretrievableAttributes: ['internalNotes'],
    paginationLimitedTo: 1200,
    attributeForDistinct: 'sku',
    distinct: true,
    removeStopWords: true,
    ignorePlurals: true,
    queryLanguages: ['en', 'fr'],
    numericAttributesForFiltering: ['price', 'rating'],
    allowCompressionOfIntegerArray: false,
    enableRules: true,
    renderingContent: {
      facetOrdering: {
        facets: { order: ['category', 'brand'] }
      }
    },
    userData: {
      owner: 'sdk-test',
      revision: 1
    }
  };

  const setResponse = await client.setSettings({
    indexName: SETTINGS_ROUNDTRIP_INDEX,
    indexSettings: settingsPayload
  });

  assert(
    setResponse && typeof setResponse.taskID === 'number',
    `Expected numeric taskID from setSettings, got ${JSON.stringify(setResponse)}`
  );

  await client.waitForTask({
    indexName: SETTINGS_ROUNDTRIP_INDEX,
    taskID: setResponse.taskID
  });

  const afterSettings = await client.getSettings({ indexName: SETTINGS_ROUNDTRIP_INDEX });

  for (const [field, expectedValue] of Object.entries(settingsPayload)) {
    const actualValue = afterSettings[field];
    assert(
      JSON.stringify(actualValue) === JSON.stringify(expectedValue),
      `Settings round-trip mismatch for ${field}: expected ${JSON.stringify(expectedValue)}, got ${JSON.stringify(actualValue)}`
    );
  }
});

test('error handling - SDK parses { message, status }', async () => {
  const nonExistentIndexName = `definitely-does-not-exist-${Date.now()}`;

  try {
    await client.search({
      requests: [{
        indexName: nonExistentIndexName,
        query: 'some query'
      }]
    });

    throw new Error('Expected error for search on non-existent index');
  } catch (error) {
    assert(typeof error.message === 'string' && error.message.length > 0, `Missing string error.message: ${JSON.stringify(error)}`);
    assert(typeof error.status === 'number', `Missing numeric error.status: ${JSON.stringify(error)}`);
    assert(error.status === 404, `Expected status 404 for non-existent index, got ${error.status}`);
  }
});

test('error handling - 404 for missing rule/synonym', async () => {
  const rule = await apiRequest(`/1/indexes/${TEST_INDEX}/rules/nonexistent_rule`, {
    expectOk: false
  });
  assert(rule.response.status === 404, `Expected 404 for missing rule, got ${rule.response.status}: ${rule.text}`);
  const ruleBody = JSON.parse(rule.text);
  assert(typeof ruleBody.message === 'string' && ruleBody.message.length > 0, `Expected missing-rule message, got ${rule.text}`);
  assert(ruleBody.status === 404, `Expected missing-rule status=404 body, got ${rule.text}`);

  const synonym = await apiRequest(`/1/indexes/${TEST_INDEX}/synonyms/nonexistent_synonym`, {
    expectOk: false
  });
  assert(synonym.response.status === 404, `Expected 404 for missing synonym, got ${synonym.response.status}: ${synonym.text}`);
  const synonymBody = JSON.parse(synonym.text);
  assert(typeof synonymBody.message === 'string' && synonymBody.message.length > 0, `Expected missing-synonym message, got ${synonym.text}`);
  assert(synonymBody.status === 404, `Expected missing-synonym status=404 body, got ${synonym.text}`);
});

runAllTests({ banner: 'Full Compatibility Tests', cleanup }).catch(async (e) => {
  console.error('Fatal error:', e);
  await cleanup();
  process.exit(1);
});
