import { createFlapjackClient, FLAPJACK_URL, FLAPJACK_ADMIN_KEY } from './lib/flapjack-client.js';
import {
  assert,
  createTestRunner,
  bindClientHelpers
} from './lib/test-helpers.js';

const client = createFlapjackClient();

const RUN_ID = Date.now();
const SELECTED_EVENT_USER_TOKEN = '3f25cf54-46f6-4f67-9ac8-87c4a34c86f1';
const MAIN_INDEX = `instantsearch_contract_${RUN_ID}`;
const REPLICA_ASC_INDEX = `${MAIN_INDEX}_price_asc`;
const REPLICA_DESC_INDEX = `${MAIN_INDEX}_price_desc`;
const AUTOCOMPLETE_INDEX = `${MAIN_INDEX}_autocomplete`;

const ALL_INDEXES = [MAIN_INDEX, REPLICA_ASC_INDEX, REPLICA_DESC_INDEX, AUTOCOMPLETE_INDEX];

const { test, runAllTests } = createTestRunner();
const { searchIndex, waitForSearch, waitForSettings, waitForMinHits, cleanupIndexes } =
  bindClientHelpers(client);

async function cleanup() {
  await cleanupIndexes(ALL_INDEXES);
}

function buildMainFixtures() {
  return [
    {
      objectID: 'p1',
      name: 'Wireless Mouse',
      brand: 'TechBrand-A',
      category: 'Electronics',
      subcategory: 'Accessories',
      description: 'Compact wireless mouse',
      price: 29.99,
      rating: 4.3,
      tags: ['wireless', 'mouse'],
      _geoloc: { lat: 40.7128, lng: -74.0060 },
      'category.lvl0': 'Electronics',
      'category.lvl1': 'Electronics > Accessories',
      testCase: 'sortby'
    },
    {
      objectID: 'p2',
      name: 'Wireless Keyboard',
      brand: 'TechBrand-A',
      category: 'Electronics',
      subcategory: 'Accessories',
      description: 'Slim wireless keyboard',
      price: 59.99,
      rating: 4.6,
      tags: ['wireless', 'keyboard'],
      _geoloc: { lat: 34.0522, lng: -118.2437 },
      'category.lvl0': 'Electronics',
      'category.lvl1': 'Electronics > Accessories',
      testCase: 'sortby'
    },
    {
      objectID: 'p3',
      name: 'Noise Cancelling Headphones',
      brand: 'AudioBrand-X',
      category: 'Electronics',
      subcategory: 'Audio',
      description: 'Over-ear noise cancelling headphones',
      price: 129.99,
      rating: 4.8,
      tags: ['headphones', 'audio'],
      _geoloc: { lat: 41.8781, lng: -87.6298 },
      'category.lvl0': 'Electronics',
      'category.lvl1': 'Electronics > Audio',
      testCase: 'sortby'
    },
    {
      objectID: 'p4',
      name: 'Smart Fitness Watch',
      brand: 'FitBrand-Y',
      category: 'Wearables',
      subcategory: 'Fitness',
      description: 'Fitness watch with heart-rate monitor',
      price: 179.99,
      rating: 4.2,
      tags: ['fitness', 'watch'],
      _geoloc: { lat: 37.7749, lng: -122.4194 },
      'category.lvl0': 'Wearables',
      'category.lvl1': 'Wearables > Fitness',
      testCase: 'sortby'
    },
    {
      objectID: 'p5',
      name: 'USB-C Hub',
      brand: 'TechBrand-B',
      category: 'Electronics',
      subcategory: 'Accessories',
      description: 'Multi-port USB-C hub',
      price: 45.5,
      rating: 4.1,
      tags: ['usb-c', 'hub'],
      _geoloc: { lat: 29.7604, lng: -95.3698 },
      'category.lvl0': 'Electronics',
      'category.lvl1': 'Electronics > Accessories',
      testCase: 'sortby'
    },
    {
      objectID: 'p6',
      name: 'Office Chair',
      brand: 'FurnitureBrand-U',
      category: 'Furniture',
      subcategory: 'Office',
      description: 'Ergonomic office chair',
      price: 239.0,
      rating: 4.5,
      tags: ['chair', 'office'],
      _geoloc: { lat: 39.9526, lng: -75.1652 },
      'category.lvl0': 'Furniture',
      'category.lvl1': 'Furniture > Office',
      testCase: 'sortby'
    },
    {
      objectID: 'p7',
      name: 'Desk Lamp',
      brand: 'LightingBrand-T',
      category: 'Furniture',
      subcategory: 'Lighting',
      description: 'Adjustable LED desk lamp',
      price: 39.99,
      rating: 4.0,
      tags: ['lamp', 'lighting'],
      _geoloc: { lat: 47.6062, lng: -122.3321 },
      'category.lvl0': 'Furniture',
      'category.lvl1': 'Furniture > Lighting',
      testCase: 'sortby'
    },
    {
      objectID: 'p8',
      name: 'Portable Bluetooth Speaker',
      brand: 'AudioBrand-X',
      category: 'Electronics',
      subcategory: 'Audio',
      description: 'Portable waterproof speaker',
      price: 79.99,
      rating: 4.4,
      tags: ['bluetooth', 'speaker'],
      _geoloc: { lat: 33.4484, lng: -112.0740 },
      'category.lvl0': 'Electronics',
      'category.lvl1': 'Electronics > Audio',
      testCase: 'sortby'
    },
    {
      objectID: 'p9',
      name: '4K Monitor',
      brand: 'DisplayBrand-Z',
      category: 'Electronics',
      subcategory: 'Displays',
      description: '27 inch 4K display',
      price: 349.99,
      rating: 4.7,
      tags: ['monitor', '4k'],
      _geoloc: { lat: 32.7767, lng: -96.7970 },
      'category.lvl0': 'Electronics',
      'category.lvl1': 'Electronics > Displays',
      testCase: 'sortby'
    },
    {
      objectID: 'p10',
      name: 'Smart Home Hub',
      brand: 'HomeBrand-V',
      category: 'Smart Home',
      subcategory: 'Hubs',
      description: 'Smart home control hub',
      price: 149.99,
      rating: 4.1,
      tags: ['smart-home', 'hub'],
      _geoloc: { lat: 42.3601, lng: -71.0589 },
      'category.lvl0': 'Smart Home',
      'category.lvl1': 'Smart Home > Hubs',
      testCase: 'sortby'
    },
    {
      objectID: 'p11',
      name: 'Wireless Earbuds',
      brand: 'AudioBrand-X',
      category: 'Electronics',
      subcategory: 'Audio',
      description: 'In-ear wireless earbuds',
      price: 99.99,
      rating: 4.6,
      tags: ['wireless', 'earbuds'],
      _geoloc: { lat: 30.2672, lng: -97.7431 },
      'category.lvl0': 'Electronics',
      'category.lvl1': 'Electronics > Audio',
      testCase: 'sortby'
    },
    {
      objectID: 'p12',
      name: 'Laptop Stand',
      brand: 'TechBrand-B',
      category: 'Electronics',
      subcategory: 'Accessories',
      description: 'Aluminum laptop stand',
      price: 49.99,
      rating: 4.2,
      tags: ['laptop', 'stand'],
      _geoloc: { lat: 39.7392, lng: -104.9903 },
      'category.lvl0': 'Electronics',
      'category.lvl1': 'Electronics > Accessories',
      testCase: 'sortby'
    }
  ];
}

async function setupMainIndex() {
  await cleanup();

  await client.setSettings({
    indexName: MAIN_INDEX,
    indexSettings: {
      attributesForFaceting: [
        'brand',
        'category',
        'subcategory',
        'tags',
        'category.lvl0',
        'category.lvl1',
        'testCase',
        'price',
        'rating'
      ]
    }
  });

  const fixtures = buildMainFixtures();

  await client.saveObjects({
    indexName: MAIN_INDEX,
    objects: fixtures
  });

  await waitForMinHits(MAIN_INDEX, fixtures.length, 10000);
}

// 1) SearchBox + Hits

test('SearchBox+Hits contract fields', async () => {
  const result = await searchIndex(MAIN_INDEX, {
    query: 'wireless',
    hitsPerPage: 5,
    page: 0
  });

  assert(Array.isArray(result.hits), 'hits must be an array');
  assert(typeof result.nbHits === 'number', 'nbHits must be a number');
  assert(typeof result.page === 'number', 'page must be a number');
  assert(typeof result.nbPages === 'number', 'nbPages must be a number');
  assert(typeof result.hitsPerPage === 'number', 'hitsPerPage must be a number');
  assert(typeof result.processingTimeMS === 'number', 'processingTimeMS must be a number');

  if (result.hits.length > 0) {
    assert(typeof result.hits[0].objectID === 'string', 'first hit must contain objectID string');
  }
});

// 2) RefinementList

test('RefinementList contract facets shape', async () => {
  const result = await searchIndex(MAIN_INDEX, {
    query: '',
    facets: ['brand', 'category']
  });

  assert(result.facets && typeof result.facets === 'object', 'facets must be present as an object');
  assert(result.facets.brand && typeof result.facets.brand === 'object', 'facets.brand must be present');
  assert(result.facets.category && typeof result.facets.category === 'object', 'facets.category must be present');

  for (const count of Object.values(result.facets.brand)) {
    assert(typeof count === 'number', 'brand facet counts must be numbers');
  }
});

// 3) RangeSlider

test('RangeSlider contract facets_stats shape', async () => {
  const result = await searchIndex(MAIN_INDEX, {
    query: '',
    facets: ['price', 'rating']
  });

  assert(result.facets_stats && typeof result.facets_stats === 'object', 'facets_stats must be present');

  for (const facetName of ['price', 'rating']) {
    const stats = result.facets_stats[facetName];
    assert(stats && typeof stats === 'object', `facets_stats.${facetName} must be an object`);
    for (const statName of ['min', 'max', 'avg', 'sum']) {
      assert(typeof stats[statName] === 'number', `facets_stats.${facetName}.${statName} must be a number`);
    }
  }
});

// 4) Pagination

test('Pagination contract page metadata', async () => {
  const page0 = await searchIndex(MAIN_INDEX, { query: '', hitsPerPage: 4, page: 0 });
  const page1 = await searchIndex(MAIN_INDEX, { query: '', hitsPerPage: 4, page: 1 });

  assert(page0.page === 0, `expected page0.page=0, got ${page0.page}`);
  assert(page1.page === 1, `expected page1.page=1, got ${page1.page}`);
  assert(typeof page0.nbPages === 'number' && page0.nbPages >= 1, 'nbPages must be >= 1');
  assert(page0.hits.length <= page0.hitsPerPage, 'page0 hits length must not exceed hitsPerPage');
  assert(page1.hits.length <= page1.hitsPerPage, 'page1 hits length must not exceed hitsPerPage');
});

// 5) SortBy

test('SortBy contract via replica switching', async () => {
  await client.setSettings({
    indexName: MAIN_INDEX,
    indexSettings: {
      replicas: [REPLICA_ASC_INDEX, REPLICA_DESC_INDEX]
    }
  });

  await client.setSettings({
    indexName: REPLICA_ASC_INDEX,
    indexSettings: {
      customRanking: ['asc(price)']
    }
  });

  await client.setSettings({
    indexName: REPLICA_DESC_INDEX,
    indexSettings: {
      customRanking: ['desc(price)']
    }
  });

  const ascSettingsReady = await waitForSettings(
    REPLICA_ASC_INDEX,
    (settings) => Array.isArray(settings.customRanking) && settings.customRanking.includes('asc(price)'),
    10000
  );
  assert(ascSettingsReady, 'replica asc customRanking setting did not propagate');

  const descSettingsReady = await waitForSettings(
    REPLICA_DESC_INDEX,
    (settings) => Array.isArray(settings.customRanking) && settings.customRanking.includes('desc(price)'),
    10000
  );
  assert(descSettingsReady, 'replica desc customRanking setting did not propagate');

  const sortQuery = `sortby-marker-${RUN_ID}`;
  const sortFixtures = [
    {
      objectID: `sortby-${RUN_ID}-low`,
      name: `${sortQuery} low`,
      brand: 'SortBrand',
      category: 'Electronics',
      subcategory: 'Accessories',
      price: 10,
      rating: 4.1,
      tags: ['sortby'],
      _geoloc: { lat: 40.0, lng: -73.0 },
      'category.lvl0': 'Electronics',
      'category.lvl1': 'Electronics > Accessories',
      testCase: 'sortby'
    },
    {
      objectID: `sortby-${RUN_ID}-mid`,
      name: `${sortQuery} mid`,
      brand: 'SortBrand',
      category: 'Electronics',
      subcategory: 'Accessories',
      price: 20,
      rating: 4.2,
      tags: ['sortby'],
      _geoloc: { lat: 41.0, lng: -74.0 },
      'category.lvl0': 'Electronics',
      'category.lvl1': 'Electronics > Accessories',
      testCase: 'sortby'
    },
    {
      objectID: `sortby-${RUN_ID}-high`,
      name: `${sortQuery} high`,
      brand: 'SortBrand',
      category: 'Electronics',
      subcategory: 'Accessories',
      price: 30,
      rating: 4.3,
      tags: ['sortby'],
      _geoloc: { lat: 42.0, lng: -75.0 },
      'category.lvl0': 'Electronics',
      'category.lvl1': 'Electronics > Accessories',
      testCase: 'sortby'
    }
  ];

  // Save a dedicated comparable cohort after replica settings are in place.
  await client.saveObjects({
    indexName: MAIN_INDEX,
    objects: sortFixtures
  });
  const sortFixturesReady = await waitForSearch(
    MAIN_INDEX,
    { query: sortQuery },
    (result) => result.nbHits >= sortFixtures.length,
    10000
  );
  assert(sortFixturesReady, 'sortby marker fixtures did not become searchable in primary index');

  const ascReady = await waitForSearch(
    REPLICA_ASC_INDEX,
    { query: sortQuery },
    (result) => result.nbHits >= sortFixtures.length,
    10000
  );
  assert(ascReady, 'replica asc did not become queryable');

  const descReady = await waitForSearch(
    REPLICA_DESC_INDEX,
    { query: sortQuery },
    (result) => result.nbHits >= sortFixtures.length,
    10000
  );
  assert(descReady, 'replica desc did not become queryable');

  const [asc, desc] = await Promise.all([
    searchIndex(REPLICA_ASC_INDEX, { query: sortQuery, hitsPerPage: sortFixtures.length }),
    searchIndex(REPLICA_DESC_INDEX, { query: sortQuery, hitsPerPage: sortFixtures.length })
  ]);

  assert(asc.hits.length > 1 && desc.hits.length > 1, 'replica searches must return enough hits for ordering checks');

  const ascPrices = asc.hits.map((h) => h.price);
  const descPrices = desc.hits.map((h) => h.price);

  for (let i = 1; i < ascPrices.length; i++) {
    assert(ascPrices[i - 1] <= ascPrices[i], `ascending replica not sorted by price: ${JSON.stringify(ascPrices)}`);
  }

  for (let i = 1; i < descPrices.length; i++) {
    assert(descPrices[i - 1] >= descPrices[i], `descending replica not sorted by price: ${JSON.stringify(descPrices)}`);
  }

  assert(asc.hits[0].objectID !== desc.hits[0].objectID, 'replica switching should change top-ranked result');
});

// 6) HierarchicalMenu

test('HierarchicalMenu contract with lvl0/lvl1 facets', async () => {
  const result = await searchIndex(MAIN_INDEX, {
    query: '',
    facets: ['category.lvl0', 'category.lvl1']
  });

  assert(result.facets && typeof result.facets === 'object', 'facets must be present');
  assert(result.facets['category.lvl0'], 'category.lvl0 facet must be present');
  assert(result.facets['category.lvl1'], 'category.lvl1 facet must be present');

  for (const [key, count] of Object.entries(result.facets['category.lvl1'])) {
    assert(typeof key === 'string' && key.includes(' > '), 'category.lvl1 keys must follow hierarchical format');
    assert(typeof count === 'number', 'category.lvl1 counts must be numbers');
  }
});

// 7) GeoSearch

test('GeoSearch contract returns _geoloc hits', async () => {
  const result = await searchIndex(MAIN_INDEX, {
    query: '',
    aroundLatLng: '40.7128,-74.0060',
    getRankingInfo: true,
    hitsPerPage: 5
  });

  assert(Array.isArray(result.hits), 'hits must be an array');
  assert(result.hits.length > 0, 'geosearch query should return at least one hit');

  const withGeo = result.hits.filter((hit) => hit._geoloc && typeof hit._geoloc === 'object');
  assert(withGeo.length > 0, 'at least one hit must include _geoloc');

  for (const hit of withGeo) {
    assert(typeof hit._geoloc.lat === 'number', '_geoloc.lat must be numeric');
    assert(typeof hit._geoloc.lng === 'number', '_geoloc.lng must be numeric');
  }

  // §24 GeoSearch widget requires _rankingInfo with geoDistance
  for (const hit of result.hits) {
    assert(hit._rankingInfo && typeof hit._rankingInfo === 'object', 'hit must include _rankingInfo when getRankingInfo=true');
    assert(typeof hit._rankingInfo.geoDistance === 'number', '_rankingInfo.geoDistance must be numeric');
  }

  // aroundLatLng must be echoed in params string
  assert(typeof result.params === 'string', 'response must contain params string');
  const echoedAroundLatLng = new URLSearchParams(result.params).get('aroundLatLng');
  assert(
    echoedAroundLatLng === '40.7128,-74.0060',
    `params must echo aroundLatLng exactly, got "${echoedAroundLatLng}"`
  );
});

// 8) CurrentRefinements

test('CurrentRefinements contract for filtered searches', async () => {
  const filtered = await searchIndex(MAIN_INDEX, {
    query: '',
    filters: 'category:Electronics AND subcategory:Accessories'
  });

  const unfiltered = await searchIndex(MAIN_INDEX, { query: '' });

  assert(typeof filtered.nbHits === 'number', 'filtered nbHits must be numeric');
  assert(typeof unfiltered.nbHits === 'number', 'unfiltered nbHits must be numeric');
  assert(filtered.nbHits <= unfiltered.nbHits, 'filtered results should not exceed unfiltered results');

  assert(typeof filtered.params === 'string', 'filtered response must contain params string');
  assert(filtered.params.includes('filters='), 'filtered params should include applied filters');

  for (const hit of filtered.hits) {
    assert(hit.category === 'Electronics', 'filtered hit category must match applied filter');
    assert(hit.subcategory === 'Accessories', 'filtered hit subcategory must match applied filter');
  }
});

test('NumericMenu contract - numeric filter ranges', async () => {
  const result = await searchIndex(MAIN_INDEX, {
    query: '',
    numericFilters: ['price>=10', 'price<=100']
  });

  assert(typeof result.nbHits === 'number', 'nbHits must be numeric');
  assert(result.nbHits > 0, 'numeric filter query should return at least one hit');
  assert(Array.isArray(result.hits), 'hits must be an array');
  assert(typeof result.params === 'string', 'params must be a string');
  assert(result.params.includes('numericFilters='), `params must include numericFilters, got ${result.params}`);

  for (const hit of result.hits) {
    assert(typeof hit.price === 'number', 'filtered hits must include numeric price');
    assert(hit.price >= 10 && hit.price <= 100, `hit.price out of expected range: ${hit.price}`);
  }
});

test('ClearRefinements contract - unfiltered after clear', async () => {
  const filtered = await searchIndex(MAIN_INDEX, {
    query: '',
    filters: 'category:Electronics'
  });
  const unfiltered = await searchIndex(MAIN_INDEX, { query: '' });

  assert(typeof filtered.nbHits === 'number', 'filtered nbHits must be numeric');
  assert(typeof unfiltered.nbHits === 'number', 'unfiltered nbHits must be numeric');
  assert(unfiltered.nbHits > filtered.nbHits, `expected unfiltered nbHits > filtered nbHits, got ${unfiltered.nbHits} <= ${filtered.nbHits}`);

  const filteredKeys = Object.keys(filtered).sort();
  const unfilteredKeys = Object.keys(unfiltered).sort();
  assert(
    JSON.stringify(filteredKeys) === JSON.stringify(unfilteredKeys),
    `filtered/unfiltered response keys differ: filtered=${JSON.stringify(filteredKeys)} unfiltered=${JSON.stringify(unfilteredKeys)}`
  );
});

test('HitsPerPage contract - dynamic page size', async () => {
  const smallPage = await searchIndex(MAIN_INDEX, {
    query: '',
    hitsPerPage: 2
  });
  const largePage = await searchIndex(MAIN_INDEX, {
    query: '',
    hitsPerPage: 10
  });

  assert(smallPage.hits.length <= 2, `hitsPerPage=2 should return <=2 hits, got ${smallPage.hits.length}`);
  assert(largePage.hits.length <= 10, `hitsPerPage=10 should return <=10 hits, got ${largePage.hits.length}`);
  assert(smallPage.hitsPerPage === 2, `response hitsPerPage should be 2, got ${smallPage.hitsPerPage}`);
  assert(largePage.hitsPerPage === 10, `response hitsPerPage should be 10, got ${largePage.hitsPerPage}`);
});

test('PoweredBy contract - response attribution fields', async () => {
  const result = await searchIndex(MAIN_INDEX, { query: '' });

  assert(typeof result.processingTimeMS === 'number', 'processingTimeMS must be numeric');
  assert(typeof result.nbHits === 'number', 'nbHits must be numeric');
  assert(typeof result.query === 'string', 'query must be string');
});

// 9) Stats

test('Stats contract fields', async () => {
  const result = await searchIndex(MAIN_INDEX, { query: '' });

  for (const field of ['processingTimeMS', 'nbHits', 'nbPages']) {
    assert(typeof result[field] === 'number', `${field} must be numeric`);
  }

  assert(result.processingTimeMS >= 0, 'processingTimeMS must be non-negative');
  assert(typeof result.query === 'string', 'query must be present as a string');
});

// 10) Autocomplete

test('Autocomplete contract for multi-index search', async () => {
  await client.setSettings({
    indexName: AUTOCOMPLETE_INDEX,
    indexSettings: {
      searchableAttributes: ['suggestion']
    }
  });

  await client.saveObjects({
    indexName: AUTOCOMPLETE_INDEX,
    objects: [
      { objectID: 's1', suggestion: 'wireless mouse' },
      { objectID: 's2', suggestion: 'wireless keyboard' },
      { objectID: 's3', suggestion: 'wireless charger' }
    ]
  });

  await waitForMinHits(AUTOCOMPLETE_INDEX, 3, 10000);

  const response = await client.search({
    requests: [
      { indexName: MAIN_INDEX, query: 'wireless', hitsPerPage: 3 },
      { indexName: AUTOCOMPLETE_INDEX, query: 'wireless', hitsPerPage: 3 }
    ]
  });

  assert(Array.isArray(response.results), 'multi-index response must contain results array');
  assert(response.results.length === 2, `expected 2 result sets, got ${response.results.length}`);

  const expectedIndexes = [MAIN_INDEX, AUTOCOMPLETE_INDEX];
  for (const [i, result] of response.results.entries()) {
    assert(Array.isArray(result.hits), `results[${i}].hits must be an array`);
    assert(typeof result.nbHits === 'number', `results[${i}].nbHits must be numeric`);
    assert(typeof result.processingTimeMS === 'number', `results[${i}].processingTimeMS must be numeric`);
    assert(result.index === expectedIndexes[i], `results[${i}].index must echo back "${expectedIndexes[i]}", got "${result.index}"`);
  }
});

// 11) clickAnalytics

test('clickAnalytics returns queryID and accepts click event', async () => {
  // (a) Search with clickAnalytics: true
  const result = await searchIndex(MAIN_INDEX, {
    query: 'wireless',
    clickAnalytics: true,
    hitsPerPage: 3
  });

  // (b) queryID must be a 32-character hex string
  assert(typeof result.queryID === 'string', `queryID must be a string, got ${typeof result.queryID}`);
  assert(/^[0-9a-f]{32}$/.test(result.queryID), `queryID must be 32-char hex, got "${result.queryID}"`);

  // (c) Send click event via POST /1/events referencing the queryID
  const clickPayload = {
    events: [
      {
        eventType: 'click',
        eventName: 'Product Clicked',
        index: MAIN_INDEX,
        userToken: SELECTED_EVENT_USER_TOKEN,
        queryID: result.queryID,
        objectIDs: [result.hits[0].objectID],
        positions: [1]
      }
    ]
  };

  const eventsResponse = await fetch(`${FLAPJACK_URL}/1/events`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'X-Algolia-Application-Id': 'flapjack',
      'X-Algolia-API-Key': FLAPJACK_ADMIN_KEY
    },
    body: JSON.stringify(clickPayload)
  });

  assert(eventsResponse.ok, `POST /1/events failed with status ${eventsResponse.status}`);

  const eventsData = await eventsResponse.json();
  assert(eventsData.status === 200, `Expected events status=200, got ${JSON.stringify(eventsData)}`);
  assert(eventsData.message === 'OK', `Expected events message='OK', got ${JSON.stringify(eventsData)}`);
});

runAllTests({
  banner: 'InstantSearch Contract Tests',
  setup: setupMainIndex,
  cleanup
}).catch(async (e) => {
  console.error('Fatal error:', e);
  await cleanup();
  process.exit(1);
});
