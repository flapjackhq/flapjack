/**
 */
/**
 * API helper functions for E2E tests.
 *
 * These wrap raw request.* calls so spec files don't need to use
 * request.get/post/delete directly (which is banned by ESLint).
 * Fixture files are exempt from the ESLint spec-file rules.
 */
import { expect, type APIRequestContext, type TestInfo } from '@playwright/test';
import { API_BASE, API_HEADERS } from './local-instance';
import { buildApiPath, buildIndexPath, getSettings, searchIndex } from './index-api-helpers';
import type { ApiKey, ApiKeyCreateResponse } from '../../src/lib/types';
import {
  asExperimentRecord,
  readExperimentId as readSharedExperimentId,
  toAlgoliaCreateExperimentPayload as toSharedAlgoliaCreateExperimentPayload,
  type CreateExperimentPayload,
  type DashboardCreateExperimentPayload,
  type ConcludeExperimentPayload,
} from '../../src/lib/experiment-api-contract';
import {
  buildRecommendationBatchPayload,
  RECOMMENDATIONS_PREVIEW_PATH,
} from '../../src/lib/recommendation-contract';
import type {
  PersonalizationProfile,
  PersonalizationStrategy,
  RecommendationBatchRequest,
  RecommendationBatchResponse,
} from '../../src/lib/types';
export {
  addDocuments,
  buildIndexPath,
  createIndex,
  deleteDocument,
  deleteIndex,
  getSettings,
  searchIndex,
  updateSettings,
} from './index-api-helpers';

export interface HealthCapabilities {
  vectorSearch?: boolean;
  vectorSearchLocal?: boolean;
  [key: string]: unknown;
}

export interface HealthResponse {
  capabilities?: HealthCapabilities;
  [key: string]: unknown;
}

/** Fetch server health payload from GET /health. */
export async function getHealth(
  request: APIRequestContext,
): Promise<HealthResponse> {
  const res = await request.get(`${API_BASE}/health`, {
    headers: API_HEADERS,
  });
  if (!res.ok()) {
    throw new Error(`getHealth failed (${res.status()}): ${await res.text()}`);
  }
  return res.json() as Promise<HealthResponse>;
}

/** True when vector capabilities are available to dashboard features. */
export async function isVectorSearchEnabled(
  request: APIRequestContext,
): Promise<boolean> {
  const health = await getHealth(request);
  return health.capabilities?.vectorSearch === true;
}

/** Mark vector-enabled browser coverage as not applicable on compiled-out builds. */
export async function skipWhenVectorSearchDisabled(
  request: APIRequestContext,
  testInfo: TestInfo,
  reason: string,
): Promise<void> {
  if (!(await isVectorSearchEnabled(request))) {
    testInfo.skip(true, reason);
  }
}

function buildExperimentPath(experimentId: string, ...segments: string[]): string {
  return buildApiPath('/2/abtests', experimentId, ...segments);
}

function buildDictionaryPath(dictName: string, ...segments: string[]): string {
  return buildApiPath('/1/dictionaries', dictName, ...segments);
}

function buildSecuritySourcesPath(...segments: string[]): string {
  return buildApiPath('/1/security/sources', ...segments);
}

function buildKeysPath(...segments: string[]): string {
  return buildApiPath('/1/keys', ...segments);
}

const RULES_SEARCH_PAGE_SIZE = 1000;

function readListItems(body: unknown): unknown[] {
  if (Array.isArray(body)) {
    return body;
  }
  if (!body || typeof body !== 'object') {
    return [];
  }

  const { hits, items } = body as { hits?: unknown; items?: unknown };
  if (Array.isArray(hits)) {
    return hits;
  }
  if (Array.isArray(items)) {
    return items;
  }

  return [];
}

function readNbPages(body: unknown, pageSize: number): number {
  if (!body || typeof body !== 'object') {
    return 1;
  }

  const { nbPages, nbHits } = body as { nbPages?: unknown; nbHits?: unknown };
  if (typeof nbPages === 'number' && Number.isFinite(nbPages) && nbPages > 0) {
    return nbPages;
  }
  if (typeof nbHits === 'number' && Number.isFinite(nbHits) && nbHits >= 0) {
    return Math.max(1, Math.ceil(nbHits / pageSize));
  }

  return 1;
}

async function searchRulesPage(
  request: APIRequestContext,
  searchPath: string,
  page: number,
): Promise<{ ok: boolean; body: unknown }> {
  const response = await request.post(searchPath, {
    headers: API_HEADERS,
    data: { query: '', page, hitsPerPage: RULES_SEARCH_PAGE_SIZE },
  });

  return {
    ok: response.ok(),
    body: response.ok() ? await response.json() : null,
  };
}

export interface SecuritySourceEntryPayload {
  source: string;
  description: string;
}

/** Shape returned by GET /1/keys (each entry in the keys array). */
export type ApiKeyFixtureRecord = ApiKey;

export interface CreateApiKeyRequestPayload {
  description?: string;
  acl: string[];
  indexes?: string[];
  restrictSources?: string[];
  maxHitsPerQuery?: number;
  maxQueriesPerIPPerHour?: number;
  expiresAt?: number;
}

/** Preview recommendations for one or more requests via POST /1/indexes/{wildcard}/recommendations. */
export async function getRecommendations(
  request: APIRequestContext,
  payload: RecommendationBatchRequest,
): Promise<RecommendationBatchResponse> {
  const body: RecommendationBatchRequest = buildRecommendationBatchPayload(payload.requests);
  const res = await request.post(`${API_BASE}${RECOMMENDATIONS_PREVIEW_PATH}`, {
    headers: API_HEADERS,
    data: body,
  });
  if (!res.ok()) {
    throw new Error(`getRecommendations failed (${res.status()}): ${await res.text()}`);
  }
  return res.json() as Promise<RecommendationBatchResponse>;
}

/** Resolve the first hit objectID for a live index query. */
export async function findFirstObjectIdByQuery(
  request: APIRequestContext,
  indexName: string,
  query: string,
): Promise<string> {
  const response = await searchIndex(request, indexName, query, { hitsPerPage: 1 });
  const firstHit = Array.isArray(response.hits) ? response.hits[0] : null;
  const objectID =
    firstHit && typeof firstHit === 'object' && typeof (firstHit as { objectID?: unknown }).objectID === 'string'
      ? (firstHit as { objectID: string }).objectID
      : null;

  if (!objectID) {
    throw new Error(`findFirstObjectIdByQuery found no objectID for "${query}" in index "${indexName}"`);
  }

  return objectID;
}

/** Get all rules for an index. */
export async function getRules(
  request: APIRequestContext,
  indexName: string,
): Promise<{ ok: boolean; items: unknown[] }> {
  const searchPath = buildIndexPath(indexName, 'rules', 'search');

  // Flapjack exposes list semantics on /rules/search.
  const firstPage = await searchRulesPage(request, searchPath, 0);
  if (firstPage.ok) {
    const firstBody = firstPage.body;
    const items = [...readListItems(firstBody)];
    const nbPages = readNbPages(firstBody, RULES_SEARCH_PAGE_SIZE);

    for (let page = 1; page < nbPages; page += 1) {
      const nextPage = await searchRulesPage(request, searchPath, page);
      if (!nextPage.ok) {
        return { ok: false, items: [] };
      }
      items.push(...readListItems(nextPage.body));
    }

    return { ok: true, items };
  }

  // Fallback for environments that still support GET /rules listing.
  const listRes = await request.get(
    buildIndexPath(indexName, 'rules'),
    { headers: API_HEADERS },
  );
  if (!listRes.ok()) return { ok: false, items: [] };
  const body = await listRes.json();
  return { ok: true, items: readListItems(body) };
}

/** Delete a specific rule by objectID. */
export async function deleteRule(
  request: APIRequestContext,
  indexName: string,
  ruleId: string,
): Promise<void> {
  const res = await request.delete(
    buildIndexPath(indexName, 'rules', ruleId),
    { headers: API_HEADERS },
  );
  if (!res.ok() && res.status() !== 404) {
    throw new Error(`deleteRule failed (${res.status()}): ${await res.text()}`);
  }
}

/** Create or upsert a rule (PUT). Throws on failure. */
export async function createRule(
  request: APIRequestContext,
  indexName: string,
  rule: { objectID: string } & Record<string, unknown>,
): Promise<void> {
  const res = await request.put(
    buildIndexPath(indexName, 'rules', rule.objectID),
    { headers: API_HEADERS, data: rule },
  );
  if (!res.ok()) {
    throw new Error(`createRule failed (${res.status()}): ${await res.text()}`);
  }
}

/** Create or upsert a synonym (PUT). Throws on failure. */
export async function createSynonym(
  request: APIRequestContext,
  indexName: string,
  synonym: { objectID: string } & Record<string, unknown>,
): Promise<void> {
  const res = await request.put(
    buildIndexPath(indexName, 'synonyms', synonym.objectID),
    { headers: API_HEADERS, data: synonym },
  );
  if (!res.ok()) {
    throw new Error(`createSynonym failed (${res.status()}): ${await res.text()}`);
  }
}

/** Delete a synonym by objectID. */
export async function deleteSynonym(
  request: APIRequestContext,
  indexName: string,
  synonymId: string,
): Promise<void> {
  const res = await request.delete(
    buildIndexPath(indexName, 'synonyms', synonymId),
    { headers: API_HEADERS },
  );
  if (!res.ok() && res.status() !== 404) {
    throw new Error(`deleteSynonym failed (${res.status()}): ${await res.text()}`);
  }
}

/** Add dictionary entries via /1/dictionaries/:name/batch using addEntry actions. */
export async function batchDictionaryEntries(
  request: APIRequestContext,
  dictName: string,
  entries: Array<Record<string, unknown>>,
): Promise<void> {
  const res = await request.post(buildDictionaryPath(dictName, 'batch'), {
    headers: API_HEADERS,
    data: {
      requests: entries.map((entry) => ({ action: 'addEntry', body: entry })),
    },
  });
  if (!res.ok()) {
    throw new Error(`batchDictionaryEntries failed (${res.status()}): ${await res.text()}`);
  }
}

/** Delete a dictionary entry via /1/dictionaries/:name/batch using deleteEntry action. */
export async function deleteDictionaryEntry(
  request: APIRequestContext,
  dictName: string,
  objectID: string,
): Promise<void> {
  const res = await request.post(buildDictionaryPath(dictName, 'batch'), {
    headers: API_HEADERS,
    data: {
      requests: [
        {
          action: 'deleteEntry',
          body: { objectID },
        },
      ],
    },
  });
  if (!res.ok()) {
    throw new Error(`deleteDictionaryEntry failed (${res.status()}): ${await res.text()}`);
  }
}

/** Search dictionary entries via /1/dictionaries/:name/search. */
export async function searchDictionary(
  request: APIRequestContext,
  dictName: string,
  query: string,
): Promise<{ hits: unknown[]; nbHits: number }> {
  const res = await request.post(buildDictionaryPath(dictName, 'search'), {
    headers: API_HEADERS,
    data: { query },
  });
  if (!res.ok()) {
    throw new Error(`searchDictionary failed (${res.status()}): ${await res.text()}`);
  }
  return res.json();
}

/** Clear all dictionary entries for a dictionary. */
export async function clearDictionary(
  request: APIRequestContext,
  dictName: string,
): Promise<void> {
  const res = await request.post(buildDictionaryPath(dictName, 'batch'), {
    headers: API_HEADERS,
    data: {
      clearExistingDictionaryEntries: true,
      requests: [],
    },
  });
  if (!res.ok()) {
    throw new Error(`clearDictionary failed (${res.status()}): ${await res.text()}`);
  }
}

/** List security source allowlist entries via GET /1/security/sources. */
export async function getSecuritySources(
  request: APIRequestContext,
): Promise<SecuritySourceEntryPayload[]> {
  const res = await request.get(buildSecuritySourcesPath(), {
    headers: API_HEADERS,
  });
  if (!res.ok()) {
    throw new Error(`getSecuritySources failed (${res.status()}): ${await res.text()}`);
  }
  return res.json();
}

/** Append or update one security source entry via POST /1/security/sources/append. */
export async function appendSecuritySource(
  request: APIRequestContext,
  entry: SecuritySourceEntryPayload,
): Promise<void> {
  const res = await request.post(buildSecuritySourcesPath('append'), {
    headers: API_HEADERS,
    data: entry,
  });
  if (!res.ok()) {
    throw new Error(`appendSecuritySource failed (${res.status()}): ${await res.text()}`);
  }
}

/** Delete a security source entry via DELETE /1/security/sources/:source. */
export async function deleteSecuritySource(
  request: APIRequestContext,
  source: string,
): Promise<void> {
  const res = await request.delete(buildSecuritySourcesPath(source), {
    headers: API_HEADERS,
  });
  if (!res.ok()) {
    throw new Error(`deleteSecuritySource failed (${res.status()}): ${await res.text()}`);
  }
}

/** Replace the full security source allowlist via PUT /1/security/sources. */
export async function replaceSecuritySources(
  request: APIRequestContext,
  entries: SecuritySourceEntryPayload[],
): Promise<void> {
  const res = await request.put(buildSecuritySourcesPath(), {
    headers: API_HEADERS,
    data: entries,
  });
  if (!res.ok()) {
    throw new Error(`replaceSecuritySources failed (${res.status()}): ${await res.text()}`);
  }
}

/** Create a key via POST /1/keys. Returns `{ key, createdAt }` — the server's actual response. */
export async function createApiKey(
  request: APIRequestContext,
  params: CreateApiKeyRequestPayload,
): Promise<ApiKeyCreateResponse> {
  const res = await request.post(buildKeysPath(), {
    headers: API_HEADERS,
    data: params,
  });
  if (!res.ok()) {
    throw new Error(`createApiKey failed (${res.status()}): ${await res.text()}`);
  }
  return res.json();
}

/** List API keys via GET /1/keys and return the keys array. */
export async function listApiKeys(
  request: APIRequestContext,
): Promise<ApiKeyFixtureRecord[]> {
  const res = await request.get(buildKeysPath(), {
    headers: API_HEADERS,
  });
  if (!res.ok()) {
    throw new Error(`listApiKeys failed (${res.status()}): ${await res.text()}`);
  }
  const body = await res.json() as { keys?: ApiKeyFixtureRecord[] };
  return Array.isArray(body.keys) ? body.keys : [];
}

/** Delete an API key by value via DELETE /1/keys/:value. */
export async function deleteApiKey(
  request: APIRequestContext,
  keyValue: string,
): Promise<void> {
  const res = await request.delete(buildKeysPath(keyValue), {
    headers: API_HEADERS,
  });
  if (!res.ok()) {
    throw new Error(`deleteApiKey failed (${res.status()}): ${await res.text()}`);
  }
}

/** Delete all API keys whose description starts with the given prefix. */
export async function deleteApiKeysByDescriptionPrefix(
  request: APIRequestContext,
  prefix: string,
): Promise<void> {
  const keys = await listApiKeys(request);
  const keysToDelete = keys.filter((key) => (key.description ?? '').startsWith(prefix));
  await Promise.all(keysToDelete.map((key) => deleteApiKey(request, key.value)));
}

// ---------------------------------------------------------------------------
// Vector search helpers
// ---------------------------------------------------------------------------

/** Configure a single embedder via PUT settings (whole-map replacement). */
export async function configureEmbedder(
  request: APIRequestContext,
  indexName: string,
  embedderName: string,
  config: Record<string, unknown>,
): Promise<void> {
  const res = await request.put(
    buildIndexPath(indexName, 'settings'),
    { headers: API_HEADERS, data: { embedders: { [embedderName]: config } } },
  );
  if (!res.ok()) {
    throw new Error(`configureEmbedder failed (${res.status()}): ${await res.text()}`);
  }
}

/** Read the embedders map from index settings, or an empty object when absent. */
export function readEmbeddersFromSettings(
  settings: Record<string, unknown>,
): Record<string, unknown> {
  const embedders = settings.embedders;
  if (!embedders || typeof embedders !== 'object') {
    return {};
  }
  return embedders as Record<string, unknown>;
}

/** Add documents that include _vectors field via the batch API. */
export async function addDocumentsWithVectors(
  request: APIRequestContext,
  indexName: string,
  documents: Array<Record<string, unknown>>,
): Promise<void> {
  const res = await request.post(buildIndexPath(indexName, 'batch'), {
    headers: API_HEADERS,
    data: {
      requests: documents.map((doc) => ({ action: 'addObject', body: doc })),
    },
  });
  if (!res.ok()) {
    throw new Error(`addDocumentsWithVectors failed (${res.status()}): ${await res.text()}`);
  }
}

/** Clear all embedders by setting embedders to empty map. */
export async function clearEmbedders(
  request: APIRequestContext,
  indexName: string,
): Promise<void> {
  const res = await request.put(
    buildIndexPath(indexName, 'settings'),
    { headers: API_HEADERS, data: { embedders: {} } },
  );
  if (!res.ok()) {
    throw new Error(`clearEmbedders failed (${res.status()}): ${await res.text()}`);
  }
}

async function waitForEmbedders(
  request: APIRequestContext,
  indexName: string,
  assertEmbedders: (embedders: Record<string, unknown>) => void,
  timeoutMs: number = 15_000,
): Promise<void> {
  await expect(async () => {
    const settings = await getSettings(request, indexName);
    assertEmbedders(readEmbeddersFromSettings(settings));
  }).toPass({ timeout: timeoutMs });
}

/** Wait until a named embedder is persisted in index settings. */
export async function waitForEmbedder(
  request: APIRequestContext,
  indexName: string,
  embedderName: string,
  timeoutMs: number = 15_000,
): Promise<void> {
  await waitForEmbedders(
    request,
    indexName,
    (embedders) => {
      expect(embedders[embedderName]).toBeTruthy();
    },
    timeoutMs,
  );
}

/** Wait until index settings contain no embedders. */
export async function waitForNoEmbedders(
  request: APIRequestContext,
  indexName: string,
  timeoutMs: number = 15_000,
): Promise<void> {
  await waitForEmbedders(
    request,
    indexName,
    (embedders) => {
      expect(Object.keys(embedders)).toHaveLength(0);
    },
    timeoutMs,
  );
}

/** Wait until a named embedder is removed from index settings. */
export async function waitForEmbedderRemoval(
  request: APIRequestContext,
  indexName: string,
  embedderName: string,
  timeoutMs: number = 15_000,
): Promise<void> {
  await waitForEmbedders(
    request,
    indexName,
    (embedders) => {
      expect(embedders[embedderName]).toBeUndefined();
    },
    timeoutMs,
  );
}

// ---------------------------------------------------------------------------
// Personalization helpers
// ---------------------------------------------------------------------------

const DEFAULT_PROFILE_WAIT_TIMEOUT_MS = 90_000;
const DEFAULT_PROFILE_WAIT_INTERVAL_MS = 1_000;

/** Save personalization strategy via POST /1/strategies/personalization. */
export async function setPersonalizationStrategy(
  request: APIRequestContext,
  strategy: PersonalizationStrategy,
): Promise<void> {
  const res = await request.post(`${API_BASE}/1/strategies/personalization`, {
    headers: API_HEADERS,
    data: strategy,
  });
  if (!res.ok()) {
    throw new Error(`setPersonalizationStrategy failed (${res.status()}): ${await res.text()}`);
  }
}

/** Get personalization strategy via GET /1/strategies/personalization. */
export async function getPersonalizationStrategy(
  request: APIRequestContext,
): Promise<PersonalizationStrategy | null> {
  const res = await request.get(`${API_BASE}/1/strategies/personalization`, {
    headers: API_HEADERS,
  });

  if (res.status() === 404) {
    return null;
  }
  if (!res.ok()) {
    throw new Error(`getPersonalizationStrategy failed (${res.status()}): ${await res.text()}`);
  }

  return res.json();
}

/** Delete personalization strategy via DELETE /1/strategies/personalization. */
export async function deletePersonalizationStrategy(
  request: APIRequestContext,
): Promise<void> {
  const res = await request.delete(`${API_BASE}/1/strategies/personalization`, {
    headers: API_HEADERS,
  });

  if (!res.ok()) {
    throw new Error(`deletePersonalizationStrategy failed (${res.status()}): ${await res.text()}`);
  }
}

/** Get personalization profile via GET /1/profiles/personalization/:userToken. */
export async function getPersonalizationProfile(
  request: APIRequestContext,
  userToken: string,
): Promise<PersonalizationProfile | null> {
  const res = await request.get(
    `${API_BASE}/1/profiles/personalization/${encodeURIComponent(userToken)}`,
    { headers: API_HEADERS },
  );

  if (res.status() === 404) {
    return null;
  }
  if (!res.ok()) {
    throw new Error(`getPersonalizationProfile failed (${res.status()}): ${await res.text()}`);
  }

  return res.json();
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Poll until the backend has flushed and can return a personalization profile. */
export async function waitForPersonalizationProfile(
  request: APIRequestContext,
  userToken: string,
  timeoutMs: number = DEFAULT_PROFILE_WAIT_TIMEOUT_MS,
  intervalMs: number = DEFAULT_PROFILE_WAIT_INTERVAL_MS,
): Promise<PersonalizationProfile> {
  const deadline = Date.now() + timeoutMs;

  while (Date.now() <= deadline) {
    // Profile computation reads analytics from persisted storage, so flush any
    // buffered events before polling for the derived profile.
    await flushAnalytics(request);
    const profile = await getPersonalizationProfile(request, userToken);
    if (profile && Object.keys(profile.scores).length > 0) {
      return profile;
    }
    await delay(intervalMs);
  }

  throw new Error(
    `waitForPersonalizationProfile timed out after ${timeoutMs}ms for userToken "${userToken}"`,
  );
}

// ---------------------------------------------------------------------------
// Experiments helpers
// ---------------------------------------------------------------------------

export type LegacyCreateExperimentPayload = DashboardCreateExperimentPayload;

export interface ExperimentRecord {
  id: string;
  name: string;
  status: string;
  [key: string]: unknown;
}

export interface ExperimentResultsArm {
  searches: number;
  [key: string]: unknown;
}

export interface ExperimentResultsGate {
  minimumNReached: boolean;
  minimumDaysReached: boolean;
  [key: string]: unknown;
}

export interface ExperimentResultsInterleaving {
  deltaAB: number;
  totalQueries: number;
  [key: string]: unknown;
}

export interface ExperimentResultsRecord {
  status: string;
  gate: ExperimentResultsGate;
  control: ExperimentResultsArm;
  variant: ExperimentResultsArm;
  bayesian: Record<string, unknown> | null;
  sampleRatioMismatch: boolean;
  guardRailAlerts: Record<string, unknown>[];
  interleaving: ExperimentResultsInterleaving | null;
  [key: string]: unknown;
}

type UpdateExperimentPayload = DashboardCreateExperimentPayload & {
  winsorizationCap?: number;
  interleaving?: boolean;
};

const DEFAULT_EXPERIMENT_WAIT_TIMEOUT_MS = 15_000;
const DEFAULT_EXPERIMENT_WAIT_INTERVAL_MS = 250;
const DEFAULT_EXPERIMENT_LIST_PAGE_SIZE = 10;

type RawExperimentRecord = Record<string, unknown>;
interface ExperimentListPage {
  experiments: ExperimentRecord[];
  count: number;
  total: number;
}

function asUnknownRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function readBoolean(value: unknown, fallback = false): boolean {
  return typeof value === 'boolean' ? value : fallback;
}

function readNumber(value: unknown, fallback = 0): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

function normalizeInterleaving(
  value: unknown,
): ExperimentResultsInterleaving | null {
  const record = asUnknownRecord(value);
  if (Object.keys(record).length === 0) {
    return null;
  }

  return {
    ...record,
    deltaAB:
      typeof record.deltaAB === 'number'
        ? record.deltaAB
        : readNumber(record.deltaAb),
    totalQueries: readNumber(record.totalQueries),
  };
}

function normalizeExperimentResults(
  value: unknown,
): ExperimentResultsRecord {
  const record = asUnknownRecord(value);
  const gate = asUnknownRecord(record.gate);
  const control = asUnknownRecord(record.control);
  const variant = asUnknownRecord(record.variant);
  const guardRailAlerts = Array.isArray(record.guardRailAlerts)
    ? record.guardRailAlerts
        .map((entry) => asUnknownRecord(entry))
        .filter((entry) => Object.keys(entry).length > 0)
    : [];

  return {
    ...record,
    status: typeof record.status === 'string' ? record.status : '',
    gate: {
      ...gate,
      minimumNReached: readBoolean(gate.minimumNReached),
      minimumDaysReached: readBoolean(gate.minimumDaysReached),
    },
    control: {
      ...control,
      searches: readNumber(control.searches),
    },
    variant: {
      ...variant,
      searches: readNumber(variant.searches),
    },
    bayesian: record.bayesian && typeof record.bayesian === 'object' && !Array.isArray(record.bayesian)
      ? record.bayesian as Record<string, unknown>
      : null,
    sampleRatioMismatch: readBoolean(record.sampleRatioMismatch),
    guardRailAlerts,
    interleaving: normalizeInterleaving(record.interleaving),
  };
}

function normalizeExperimentRecord(
  record: RawExperimentRecord,
  fallback: Partial<ExperimentRecord> = {},
): ExperimentRecord {
  return {
    ...record,
    id: readSharedExperimentId(record) || fallback.id || '',
    name: typeof record.name === 'string' ? record.name : (fallback.name ?? ''),
    status: typeof record.status === 'string' ? record.status : (fallback.status ?? ''),
  };
}

function readNonNegativeInteger(value: unknown): number | null {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function normalizeExperimentListPage(body: unknown): ExperimentListPage {
  const record = asUnknownRecord(body);
  const experiments = Array.isArray(record.abtests)
    ? record.abtests.map((entry) => normalizeExperimentRecord(asExperimentRecord(entry)))
    : [];

  return {
    experiments,
    count: readNonNegativeInteger(record.count) ?? experiments.length,
    total: readNonNegativeInteger(record.total) ?? experiments.length,
  };
}

async function fetchExperimentListPage(
  request: APIRequestContext,
  pagination?: { offset: number; limit: number },
): Promise<ExperimentListPage> {
  const res = await request.get(`${API_BASE}/2/abtests`, {
    headers: API_HEADERS,
    ...(pagination ? { params: pagination } : {}),
  });
  if (!res.ok()) {
    throw new Error(`listExperiments failed (${res.status()}): ${await res.text()}`);
  }
  return normalizeExperimentListPage(await res.json());
}

async function postExperimentLifecycleAction(
  request: APIRequestContext,
  experimentId: string,
  action: 'start' | 'stop' | 'conclude',
  errorLabel: string,
  payload?: ConcludeExperimentPayload,
): Promise<ExperimentRecord> {
  const res = await request.post(buildExperimentPath(experimentId, action), {
    headers: API_HEADERS,
    ...(payload ? { data: payload } : {}),
  });
  if (!res.ok()) {
    throw new Error(`${errorLabel} failed (${res.status()}): ${await res.text()}`);
  }
  return res.json();
}

async function deleteExperimentsMatching(
  request: APIRequestContext,
  shouldDelete: (experiment: ExperimentRecord) => boolean,
): Promise<void> {
  const experiments = await listExperiments(request);
  for (const experiment of experiments) {
    if (shouldDelete(experiment)) {
      await deleteExperiment(request, experiment.id);
    }
  }
}

/** List all experiments via GET /2/abtests. Returns the array of experiments. */
export async function listExperiments(
  request: APIRequestContext,
): Promise<ExperimentRecord[]> {
  const firstPage = await fetchExperimentListPage(request);
  const experiments = [...firstPage.experiments];
  const pageSize = firstPage.count > 0
    ? firstPage.count
    : DEFAULT_EXPERIMENT_LIST_PAGE_SIZE;

  let offset = firstPage.count;
  while (offset < firstPage.total) {
    const page = await fetchExperimentListPage(request, { offset, limit: pageSize });
    experiments.push(...page.experiments);
    if (page.count === 0) {
      break;
    }
    offset += page.count;
  }

  return experiments;
}

/** Find an experiment by name. Throws if not found. */
export async function getExperimentByName(
  request: APIRequestContext,
  name: string,
): Promise<ExperimentRecord> {
  const experiments = await listExperiments(request);
  const matches = experiments.filter((e) => e.name === name);
  if (matches.length === 0) {
    throw new Error(`No experiment found with name "${name}"`);
  }
  if (matches.length > 1) {
    const ids = matches.map((e) => e.id).join(', ');
    throw new Error(`Multiple experiments found with name "${name}": ${ids}`);
  }
  return matches[0];
}

/** Poll until an experiment with the given name appears in GET /2/abtests. */
export async function waitForExperimentByName(
  request: APIRequestContext,
  name: string,
  timeoutMs: number = DEFAULT_EXPERIMENT_WAIT_TIMEOUT_MS,
  intervalMs: number = DEFAULT_EXPERIMENT_WAIT_INTERVAL_MS,
): Promise<ExperimentRecord> {
  const deadline = Date.now() + timeoutMs;

  while (Date.now() <= deadline) {
    const experiments = await listExperiments(request);
    const matches = experiments.filter((experiment) => experiment.name === name);
    if (matches.length === 1) {
      return matches[0];
    }
    if (matches.length > 1) {
      const ids = matches.map((experiment) => experiment.id).join(', ');
      throw new Error(`Multiple experiments found with name "${name}": ${ids}`);
    }
    await delay(intervalMs);
  }

  throw new Error(`waitForExperimentByName timed out after ${timeoutMs}ms for name "${name}"`);
}

/** Create an experiment via POST /2/abtests. Throws on failure. */
export async function createExperiment(
  request: APIRequestContext,
  payload: CreateExperimentPayload,
): Promise<ExperimentRecord> {
  const requestPayload = toSharedAlgoliaCreateExperimentPayload(payload);
  const res = await request.post(`${API_BASE}/2/abtests`, {
    headers: API_HEADERS,
    data: requestPayload,
  });
  if (!res.ok()) {
    throw new Error(`createExperiment failed (${res.status()}): ${await res.text()}`);
  }
  const body = asExperimentRecord(await res.json());
  const normalized = normalizeExperimentRecord(body, {
    name: requestPayload.name,
    status: 'draft',
  });
  if (!normalized.id) {
    throw new Error(`createExperiment response missing id-like field: ${JSON.stringify(body)}`);
  }
  return normalized;
}

/** Update an experiment via PUT /2/abtests/:id. Throws on failure. */
export async function updateExperiment(
  request: APIRequestContext,
  experimentId: string,
  payload: UpdateExperimentPayload,
): Promise<ExperimentRecord> {
  const res = await request.put(buildExperimentPath(experimentId), {
    headers: API_HEADERS,
    data: payload,
  });
  if (!res.ok()) {
    throw new Error(`updateExperiment failed (${res.status()}): ${await res.text()}`);
  }

  const body = asExperimentRecord(await res.json());
  return normalizeExperimentRecord(body, {
    id: experimentId,
    name: payload.name,
  });
}

/** Get experiment results via GET /2/abtests/:id/results. Throws on failure. */
export async function getExperimentResults(
  request: APIRequestContext,
  experimentId: string,
): Promise<ExperimentResultsRecord> {
  const res = await request.get(buildExperimentPath(experimentId, 'results'), {
    headers: API_HEADERS,
  });
  if (!res.ok()) {
    throw new Error(`getExperimentResults failed (${res.status()}): ${await res.text()}`);
  }

  return normalizeExperimentResults(await res.json());
}

/** Poll for experiment results until the predicate returns true. */
export async function waitForExperimentResults(
  request: APIRequestContext,
  experimentId: string,
  predicate: (results: ExperimentResultsRecord) => boolean,
  timeoutMs: number = DEFAULT_EXPERIMENT_WAIT_TIMEOUT_MS,
  intervalMs: number = DEFAULT_EXPERIMENT_WAIT_INTERVAL_MS,
): Promise<ExperimentResultsRecord> {
  const deadline = Date.now() + timeoutMs;
  let lastResults: ExperimentResultsRecord | null = null;

  while (Date.now() <= deadline) {
    const results = await getExperimentResults(request, experimentId);
    lastResults = results;
    if (predicate(results)) {
      return results;
    }
    await delay(intervalMs);
  }

  throw new Error(
    `waitForExperimentResults timed out after ${timeoutMs}ms for experiment "${experimentId}"` +
      ` (last status: ${lastResults?.status ?? 'unknown'})`,
  );
}

/** Start an experiment via POST /2/abtests/:id/start. Throws on failure. */
export async function startExperiment(
  request: APIRequestContext,
  experimentId: string,
): Promise<ExperimentRecord> {
  return postExperimentLifecycleAction(request, experimentId, 'start', 'startExperiment');
}

/** Stop an experiment via POST /2/abtests/:id/stop. Throws on failure. */
export async function stopExperiment(
  request: APIRequestContext,
  experimentId: string,
): Promise<ExperimentRecord> {
  return postExperimentLifecycleAction(request, experimentId, 'stop', 'stopExperiment');
}

/** Conclude an experiment via POST /2/abtests/:id/conclude. Throws on failure. */
export async function concludeExperiment(
  request: APIRequestContext,
  experimentId: string,
  payload: ConcludeExperimentPayload,
): Promise<ExperimentRecord> {
  return postExperimentLifecycleAction(
    request,
    experimentId,
    'conclude',
    'concludeExperiment',
    payload,
  );
}

/** Delete an experiment, stopping first if it is running. */
export async function deleteExperiment(
  request: APIRequestContext,
  experimentId: string,
): Promise<void> {
  const url = buildExperimentPath(experimentId);
  const firstDelete = await request.delete(url, {
    headers: API_HEADERS,
  });

  if (firstDelete.ok() || firstDelete.status() === 404) {
    return;
  }

  if (firstDelete.status() === 409) {
    const stopRes = await request.post(`${url}/stop`, {
      headers: API_HEADERS,
    });
    if (!stopRes.ok() && stopRes.status() !== 409 && stopRes.status() !== 404) {
      throw new Error(`stopExperiment before delete failed (${stopRes.status()}): ${await stopRes.text()}`);
    }

    const retryDelete = await request.delete(url, {
      headers: API_HEADERS,
    });
    if (retryDelete.ok() || retryDelete.status() === 404) {
      return;
    }
    throw new Error(`deleteExperiment retry failed (${retryDelete.status()}): ${await retryDelete.text()}`);
  }

  throw new Error(`deleteExperiment failed (${firstDelete.status()}): ${await firstDelete.text()}`);
}

/** Delete all experiments whose name starts with the provided prefix. */
export async function deleteExperimentsByPrefix(
  request: APIRequestContext,
  prefix: string,
): Promise<void> {
  await deleteExperimentsMatching(
    request,
    (experiment) => typeof experiment.name === 'string' && experiment.name.startsWith(prefix),
  );
}

/** Delete all experiments with an exact name match. */
export async function deleteExperimentsByName(
  request: APIRequestContext,
  name: string,
): Promise<void> {
  await deleteExperimentsMatching(request, (experiment) => experiment.name === name);
}

// ---------------------------------------------------------------------------
// Insights / Events helpers
// ---------------------------------------------------------------------------

export interface InsightEvent {
  eventType: string;
  eventName: string;
  index: string;
  userToken: string;
  objectIDs: string[];
  timestamp?: number;
  positions?: number[];
  queryID?: string;
  eventSubtype?: string;
  interleavingTeam?: 'control' | 'variant';
}

/** Send insight events via POST /1/events. Throws on failure. */
export async function sendEvents(
  request: APIRequestContext,
  events: InsightEvent[],
): Promise<void> {
  const res = await request.post(`${API_BASE}/1/events`, {
    headers: API_HEADERS,
    data: { events },
  });
  if (!res.ok()) {
    throw new Error(`sendEvents failed (${res.status()}): ${await res.text()}`);
  }
}

/** Get debug events via GET /1/events/debug. Throws on failure. */
export async function getDebugEvents(
  request: APIRequestContext,
  params?: {
    index?: string;
    eventType?: string;
    status?: string;
    limit?: number;
    from?: number;
    until?: number;
  },
): Promise<{ events: unknown[]; count: number }> {
  const qs = new URLSearchParams();
  if (params?.index) qs.set('index', params.index);
  if (params?.eventType) qs.set('eventType', params.eventType);
  if (params?.status) qs.set('status', params.status);
  if (params?.limit) qs.set('limit', String(params.limit));
  if (params?.from !== undefined) qs.set('from', String(params.from));
  if (params?.until !== undefined) qs.set('until', String(params.until));
  const url = `${API_BASE}/1/events/debug${qs.toString() ? `?${qs}` : ''}`;
  const res = await request.get(url, { headers: API_HEADERS });
  if (!res.ok()) {
    throw new Error(`getDebugEvents failed (${res.status()}): ${await res.text()}`);
  }
  return res.json();
}

/** Flush buffered analytics data to disk via POST /2/analytics/flush. */
export async function flushAnalytics(
  request: APIRequestContext,
  index?: string,
): Promise<void> {
  const res = await request.post(`${API_BASE}/2/analytics/flush`, {
    headers: API_HEADERS,
    ...(index ? { params: { index } } : {}),
  });
  if (!res.ok()) {
    throw new Error(`flushAnalytics failed (${res.status()}): ${await res.text()}`);
  }
}
