/**
 */
import { expect, type APIRequestContext } from '@playwright/test';
import type { ChatRequest, ChatResponse } from '../../src/lib/types';
import { API_HEADERS } from './local-instance';
import { configureEmbedder, waitForEmbedder } from './api-helpers';
import {
  addDocuments,
  buildIndexPath,
  getSettings,
  searchIndex,
  updateSettings,
} from './index-api-helpers';

export interface SearchabilityExpectation {
  query: string;
  expectedMinimumHits: number;
}

export type ChatSearchMode = 'keywordSearch' | 'neuralSearch';

/**
 * TODO: Document hasStubAiProvider.
 */
function hasStubAiProvider(settings: Record<string, unknown>): boolean {
  if (settings.mode !== 'neuralSearch') {
    return false;
  }

  const userData = settings.userData;
  if (!userData || typeof userData !== 'object') {
    return false;
  }

  const aiProvider = (userData as Record<string, unknown>).aiProvider;
  if (!aiProvider || typeof aiProvider !== 'object') {
    return false;
  }

  const provider = aiProvider as Record<string, unknown>;
  return provider.baseUrl === 'stub' && typeof provider.apiKey === 'string' && provider.apiKey.length > 0;
}

/** Update mode and wait until a settings read reflects the requested value. */
export async function setChatSearchMode(
  request: APIRequestContext,
  indexName: string,
  mode: ChatSearchMode,
): Promise<void> {
  await updateSettings(request, indexName, { mode });

  await expect(async () => {
    const settings = await getSettings(request, indexName);
    expect(settings.mode).toBe(mode);
  }).toPass({ timeout: 15_000 });
}

/** Enable NeuralSearch mode with the stub AI provider for deterministic chat tests. */
export async function setChatStubProvider(
  request: APIRequestContext,
  indexName: string,
): Promise<void> {
  await updateSettings(request, indexName, {
    mode: 'neuralSearch',
    userData: {
      aiProvider: { baseUrl: 'stub', apiKey: 'stub-key' },
    },
  });

  await expect(async () => {
    const settings = await getSettings(request, indexName);
    expect(hasStubAiProvider(settings)).toBe(true);
  }).toPass({ timeout: 15_000 });
}

/** Configure the minimum persisted settings required for the chat UI to render. */
export async function setChatReadySettings(
  request: APIRequestContext,
  indexName: string,
): Promise<void> {
  await setChatStubProvider(request, indexName);
  await configureEmbedder(request, indexName, 'default', {
    source: 'userProvided',
    dimensions: 384,
  });
  await waitForEmbedder(request, indexName, 'default');
}

/** Add documents, then poll until the index reports the expected query hits. */
export async function addDocumentsAndWaitForSearchable(
  request: APIRequestContext,
  indexName: string,
  documents: Array<Record<string, unknown>>,
  searchExpectation: SearchabilityExpectation,
): Promise<void> {
  await addDocuments(request, indexName, documents);

  await expect(async () => {
    const body = await searchIndex(request, indexName, searchExpectation.query);
    expect(body.nbHits ?? 0).toBeGreaterThanOrEqual(searchExpectation.expectedMinimumHits);
  }).toPass({ timeout: 15_000 });
}

/** Send a chat query via POST /1/indexes/:name/chat. Returns parsed response. */
export async function postChat(
  request: APIRequestContext,
  indexName: string,
  query: ChatRequest['query'],
  conversationId?: string,
): Promise<ChatResponse> {
  const payload: ChatRequest = { query, ...(conversationId ? { conversationId } : {}) };
  const res = await request.post(buildIndexPath(indexName, 'chat'), {
    headers: API_HEADERS,
    data: payload,
  });
  if (!res.ok()) {
    throw new Error(`postChat failed (${res.status()}): ${await res.text()}`);
  }
  return res.json();
}
