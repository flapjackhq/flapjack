import type { APIRequestContext } from '@playwright/test';
import { API_BASE, API_HEADERS } from './local-instance';

export function joinEncodedPath(...segments: string[]): string {
  return segments.map((segment) => encodeURIComponent(segment)).join('/');
}

export function buildApiPath(basePath: string, ...segments: string[]): string {
  const suffix = segments.length > 0 ? `/${joinEncodedPath(...segments)}` : '';
  return `${API_BASE}${basePath}${suffix}`;
}

export function buildIndexPath(indexName: string, ...segments: string[]): string {
  return buildApiPath('/1/indexes', indexName, ...segments);
}

/** Delete an index. Ignores errors if index doesn't exist. */
export async function deleteIndex(
  request: APIRequestContext,
  indexName: string,
): Promise<void> {
  await request.delete(buildIndexPath(indexName), {
    headers: API_HEADERS,
  }).catch(() => {});
}

/** Create an index via POST /1/indexes. Throws on failure. */
export async function createIndex(
  request: APIRequestContext,
  indexName: string,
): Promise<void> {
  const response = await request.post(`${API_BASE}/1/indexes`, {
    headers: API_HEADERS,
    data: { uid: indexName },
  });

  if (!response.ok()) {
    throw new Error(`createIndex failed (${response.status()}): ${await response.text()}`);
  }
}

/** Add documents to an index via the batch API. */
export async function addDocuments(
  request: APIRequestContext,
  indexName: string,
  documents: Array<Record<string, unknown>>,
): Promise<void> {
  const response = await request.post(buildIndexPath(indexName, 'batch'), {
    headers: API_HEADERS,
    data: {
      requests: documents.map((doc) => ({ action: 'addObject', body: doc })),
    },
  });
  if (!response.ok()) {
    throw new Error(`addDocuments failed (${response.status()}): ${await response.text()}`);
  }
}

/** Delete a single document from an index by objectID. Ignores 404 (already gone). */
export async function deleteDocument(
  request: APIRequestContext,
  indexName: string,
  objectID: string,
): Promise<void> {
  const res = await request.delete(
    buildIndexPath(indexName, objectID),
    { headers: API_HEADERS },
  );
  if (!res.ok() && res.status() !== 404) {
    throw new Error(`deleteDocument failed (${res.status()}): ${await res.text()}`);
  }
}

export interface SearchIndexOptions {
  userToken?: string;
  analytics?: boolean;
  clickAnalytics?: boolean;
  responseFields?: string[];
  hitsPerPage?: number;
  page?: number;
  filters?: string;
  mode?: 'keywordSearch' | 'neuralSearch';
}

export type SearchIndexResponse = {
  nbHits?: number;
  hits?: unknown[];
  queryID?: string;
  abTestID?: string;
  abTestVariantID?: string;
  interleavedTeams?: Record<string, string>;
  [key: string]: unknown;
};

/** Search an index. Returns the parsed JSON response body. */
export async function searchIndex(
  request: APIRequestContext,
  indexName: string,
  query: string,
  options?: SearchIndexOptions,
): Promise<SearchIndexResponse> {
  const body: Record<string, unknown> = { query };
  if (typeof options?.userToken === 'string') body.userToken = options.userToken;
  if (typeof options?.analytics === 'boolean') body.analytics = options.analytics;
  if (typeof options?.clickAnalytics === 'boolean') body.clickAnalytics = options.clickAnalytics;
  if (Array.isArray(options?.responseFields)) body.responseFields = options.responseFields;
  if (typeof options?.hitsPerPage === 'number') body.hitsPerPage = options.hitsPerPage;
  if (typeof options?.page === 'number') body.page = options.page;
  if (typeof options?.filters === 'string') body.filters = options.filters;
  if (typeof options?.mode === 'string') body.mode = options.mode;

  const res = await request.post(
    buildIndexPath(indexName, 'query'),
    { headers: API_HEADERS, data: body },
  );
  if (!res.ok()) {
    throw new Error(`searchIndex failed (${res.status()}): ${await res.text()}`);
  }
  return res.json();
}

/** Get index settings. */
export async function getSettings(
  request: APIRequestContext,
  indexName: string,
): Promise<Record<string, unknown>> {
  const res = await request.get(
    buildIndexPath(indexName, 'settings'),
    { headers: API_HEADERS },
  );
  if (!res.ok()) {
    throw new Error(`getSettings failed (${res.status()}): ${await res.text()}`);
  }
  return res.json();
}

/** Update index settings via PUT. */
export async function updateSettings(
  request: APIRequestContext,
  indexName: string,
  settings: Record<string, unknown>,
): Promise<void> {
  const res = await request.put(
    buildIndexPath(indexName, 'settings'),
    { headers: API_HEADERS, data: settings },
  );
  if (!res.ok()) {
    throw new Error(`updateSettings failed (${res.status()}): ${await res.text()}`);
  }
}
