/**
 * @module Stub summary for scenario-helpers.ts.
 */
/**
 */
import { expect, type APIRequestContext } from '@playwright/test';
import { addDocuments, createIndex, deleteIndex, searchIndex } from '../fixtures/api-helpers';

const MERCHANDISING_CATEGORY = 'Merchandising Fixtures';
const SEARCH_WAIT_TIMEOUT_MS = 15_000;

type SearchResponse = Awaited<ReturnType<typeof searchIndex>>;
type SearchHit = Record<string, unknown>;

interface MerchDocumentInput {
  objectID: string;
  name: string;
  brand: string;
  description?: string;
}

type MerchDocument = Record<string, unknown> & {
  objectID: string;
  name: string;
  brand: string;
  category: string;
  description?: string;
};

export interface IsolatedMerchandisingScenario {
  indexName: string;
  searchQuery: string;
}

export interface IsolatedMerchandisingComparisonScenario {
  indexName: string;
  firstQuery: string;
  secondQuery: string;
  firstObjectId: string;
  secondObjectId: string;
}

export interface IsolatedMerchandisingLifecycleScenario {
  indexName: string;
  searchQuery: string;
  expectedObjectIDs: string[];
}

function buildUniqueSuffix(): string {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

function slugifyIndexComponent(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 24);
}

function buildScenarioIndexName(prefix: string, scenarioName: string, suffix: string): string {
  return `${prefix}-${slugifyIndexComponent(scenarioName)}-${suffix}`;
}

function buildMerchDocument(document: MerchDocumentInput): MerchDocument {
  return {
    ...document,
    category: MERCHANDISING_CATEGORY,
  };
}

function getSearchHits(response: SearchResponse): SearchHit[] {
  return Array.isArray(response.hits)
    ? response.hits.filter((hit): hit is SearchHit => Boolean(hit) && typeof hit === 'object')
    : [];
}

function getHitObjectIDs(response: SearchResponse): string[] {
  return getSearchHits(response)
    .map((hit) => hit.objectID)
    .filter((objectID): objectID is string => typeof objectID === 'string');
}

async function resetScenarioIndex(
  request: APIRequestContext,
  indexName: string,
  documents: MerchDocument[],
): Promise<void> {
  await deleteIndex(request, indexName);
  await createIndex(request, indexName);
  await addDocuments(request, indexName, documents);
}

async function waitForIndexedQuery(
  request: APIRequestContext,
  indexName: string,
  query: string,
  assertResponse: (response: SearchResponse) => void,
): Promise<void> {
  await expect(async () => {
    const response = await searchIndex(request, indexName, query);
    assertResponse(response);
  }).toPass({ timeout: SEARCH_WAIT_TIMEOUT_MS });
}

/**
 * TODO: Document createIsolatedMerchandisingScenario.
 */
export async function createIsolatedMerchandisingScenario(
  request: APIRequestContext,
  scenarioName: string,
): Promise<IsolatedMerchandisingScenario> {
  const suffix = buildUniqueSuffix();
  const indexName = buildScenarioIndexName('e2e-merch', scenarioName, suffix);
  const searchQuery = `merch-fixture-${suffix}`;
  const brand = `MerchBrand-${suffix}`;

  await resetScenarioIndex(request, indexName, [
    buildMerchDocument({
      objectID: `merch-${suffix}`,
      name: `Merch Fixture ${searchQuery}`,
      brand,
    }),
  ]);

  await waitForIndexedQuery(request, indexName, searchQuery, (response) => {
    expect(response.nbHits ?? 0).toBeGreaterThan(0);
  });

  return { indexName, searchQuery };
}

/**
 * TODO: Document createIsolatedMerchandisingComparisonScenario.
 */
export async function createIsolatedMerchandisingComparisonScenario(
  request: APIRequestContext,
  scenarioName: string,
): Promise<IsolatedMerchandisingComparisonScenario> {
  const suffix = buildUniqueSuffix();
  const indexName = buildScenarioIndexName('e2e-merch-compare', scenarioName, suffix);
  const firstQuery = `tablet-fixture-${suffix}`;
  const secondQuery = `monitor-fixture-${suffix}`;
  const firstObjectId = `p${Date.now()}11`;
  const secondObjectId = `p${Date.now()}22`;
  const brand = `MerchBrand-${suffix}`;

  await resetScenarioIndex(request, indexName, [
    buildMerchDocument({
      objectID: firstObjectId,
      name: `Fixture ${firstQuery}`,
      brand,
      description: 'Deterministic tablet fixture for merchandising comparison',
    }),
    buildMerchDocument({
      objectID: secondObjectId,
      name: `Fixture ${secondQuery}`,
      brand,
      description: 'Deterministic monitor fixture for merchandising comparison',
    }),
  ]);

  await waitForIndexedQuery(request, indexName, firstQuery, (firstResponse) => {
    const firstHits = getSearchHits(firstResponse);
    expect(firstHits.length).toBeGreaterThan(0);
  });

  await waitForIndexedQuery(request, indexName, secondQuery, (secondResponse) => {
    const secondHits = getSearchHits(secondResponse);
    expect(secondHits.length).toBeGreaterThan(0);
  });

  return {
    indexName,
    firstQuery,
    secondQuery,
    firstObjectId,
    secondObjectId,
  };
}

/**
 * TODO: Document createIsolatedMerchandisingLifecycleScenario.
 */
export async function createIsolatedMerchandisingLifecycleScenario(
  request: APIRequestContext,
  scenarioName: string,
): Promise<IsolatedMerchandisingLifecycleScenario> {
  const suffix = buildUniqueSuffix();
  const indexName = buildScenarioIndexName('e2e-merch-life', scenarioName, suffix);
  const objectIdSeed = Date.now();
  const searchQuery = `merchlifecycle${objectIdSeed}`;
  const brand = `MerchBrand-${suffix}`;
  const expectedObjectIDs = [
    `p${objectIdSeed}01`,
    `p${objectIdSeed}02`,
    `p${objectIdSeed}03`,
    `p${objectIdSeed}04`,
  ];

  await resetScenarioIndex(request, indexName, [
    buildMerchDocument({
      objectID: expectedObjectIDs[0],
      name: `Lifecycle Alpha ${searchQuery}`,
      brand,
      description: `Deterministic merchandising fixture A for ${searchQuery}`,
    }),
    buildMerchDocument({
      objectID: expectedObjectIDs[1],
      name: `Lifecycle Beta ${searchQuery}`,
      brand,
      description: `Deterministic merchandising fixture B for ${searchQuery}`,
    }),
    buildMerchDocument({
      objectID: expectedObjectIDs[2],
      name: `Lifecycle Gamma ${searchQuery}`,
      brand,
      description: `Deterministic merchandising fixture C for ${searchQuery}`,
    }),
    buildMerchDocument({
      objectID: expectedObjectIDs[3],
      name: `Lifecycle Delta ${searchQuery}`,
      brand,
      description: `Deterministic merchandising fixture D for ${searchQuery}`,
    }),
  ]);

  await waitForIndexedQuery(request, indexName, searchQuery, (response) => {
    const hitObjectIDs = getHitObjectIDs(response);
    expect(hitObjectIDs).toEqual(expect.arrayContaining(expectedObjectIDs));
  });

  return {
    indexName,
    searchQuery,
    expectedObjectIDs,
  };
}
