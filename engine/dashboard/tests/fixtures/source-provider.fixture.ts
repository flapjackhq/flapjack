import type { APIRequestContext } from '@playwright/test';
import { execFile } from 'node:child_process';
import path from 'node:path';
import { promisify } from 'node:util';
import { API_HEADERS } from './local-instance';
import { buildApiPath, deleteIndex } from './index-api-helpers';

const execFileAsync = promisify(execFile);
const fixtureControlPath = path.resolve(process.cwd(), '../tests/source_provider_fixture_ctl.sh');

type SourceProvider = 'meilisearch' | 'typesense';
type SourceProviderFixtureAction = 'up' | 'down';

export interface SourceProviderCleanupReceipt {
  provider: SourceProvider;
  container: string;
  fixtureDir: string;
  ownershipToken: string;
}

interface SourceProviderFixtureControlOutput {
  stdout: string;
  stderr: string;
}

export type SourceProviderFixtureControl = (
  action: SourceProviderFixtureAction,
  provider: SourceProvider,
  cleanupReceipt?: SourceProviderCleanupReceipt,
) => Promise<SourceProviderFixtureControlOutput>;

export interface SourceProviderContext {
  provider: SourceProvider;
  port: number;
  apiKey: string;
  sourceName: string;
  container: string;
  fixtureDir: string;
  ownershipToken: string;
  seededDocumentCount: 2;
  seededIds: string[];
  endpoint: string;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export function parseSourceProviderFixture(
  output: string,
  providerLabel: string,
): Omit<SourceProviderContext, 'provider' | 'endpoint'> {
  let payload: unknown;
  try {
    payload = JSON.parse(output);
  } catch {
    throw new Error(`${providerLabel} fixture ctl returned malformed JSON`);
  }

  if (!isRecord(payload)) {
    throw new Error(`${providerLabel} fixture ctl did not return an object`);
  }

  const {
    port,
    apiKey,
    sourceName,
    container,
    fixtureDir,
    ownershipToken,
    seededDocumentCount,
    seededIds,
  } = payload;
  const hasValidStrings = [apiKey, sourceName, container, fixtureDir, ownershipToken]
    .every((value) => typeof value === 'string' && value.length > 0);
  const hasValidSeedIds = Array.isArray(seededIds)
    && seededIds.length === 2
    && seededIds.every((value) => typeof value === 'string' && value.length > 0);
  if (!Number.isInteger(port) || Number(port) < 1 || Number(port) > 65_535
      || seededDocumentCount !== 2 || !hasValidStrings || !hasValidSeedIds) {
    throw new Error(`${providerLabel} fixture ctl returned an invalid readiness contract`);
  }

  return {
    port: Number(port),
    apiKey: String(apiKey),
    sourceName: String(sourceName),
    container: String(container),
    fixtureDir: String(fixtureDir),
    ownershipToken: String(ownershipToken),
    seededDocumentCount: 2,
    seededIds: seededIds as string[],
  };
}

const CLEANUP_RECEIPT_PREFIX = 'SOURCE_PROVIDER_CLEANUP_RECEIPT=';

function parseSourceProviderCleanupReceipt(
  output: string,
  expectedProvider: SourceProvider,
): SourceProviderCleanupReceipt {
  const receiptLine = output.split(/\r?\n/)
    .find((line) => line.startsWith(CLEANUP_RECEIPT_PREFIX));
  if (!receiptLine) {
    throw new Error(`${expectedProvider} fixture ctl did not return a cleanup receipt`);
  }

  let payload: unknown;
  try {
    payload = JSON.parse(receiptLine.slice(CLEANUP_RECEIPT_PREFIX.length));
  } catch {
    throw new Error(`${expectedProvider} fixture ctl returned a malformed cleanup receipt`);
  }
  if (!isRecord(payload)) {
    throw new Error(`${expectedProvider} fixture ctl returned an invalid cleanup receipt`);
  }

  const { provider, container, fixtureDir, ownershipToken } = payload;
  const hasValidStrings = [container, fixtureDir, ownershipToken]
    .every((value) => typeof value === 'string' && value.length > 0);
  if (provider !== expectedProvider || !hasValidStrings) {
    throw new Error(`${expectedProvider} fixture ctl returned an invalid cleanup receipt`);
  }

  return {
    provider: expectedProvider,
    container: String(container),
    fixtureDir: String(fixtureDir),
    ownershipToken: String(ownershipToken),
  };
}

const runFixtureControl: SourceProviderFixtureControl = async (
  action,
  provider,
  cleanupReceipt,
) => {
  const env = cleanupReceipt
    ? {
      ...process.env,
      SOURCE_PROVIDER_CONTAINER: cleanupReceipt.container,
      SOURCE_PROVIDER_FIXTURE_DIR: cleanupReceipt.fixtureDir,
      SOURCE_PROVIDER_OWNER_TOKEN: cleanupReceipt.ownershipToken,
    }
    : process.env;
  const { stdout, stderr } = await execFileAsync(
    'bash',
    [fixtureControlPath, action, provider],
    { env },
  );
  return { stdout, stderr };
};

async function assertSourceReady(
  request: APIRequestContext,
  context: SourceProviderContext,
): Promise<void> {
  const headers: Record<string, string> = context.provider === 'meilisearch'
    ? { Authorization: `Bearer ${context.apiKey}` }
    : { 'X-TYPESENSE-API-KEY': context.apiKey };
  const path = context.provider === 'meilisearch'
    ? `/indexes/${context.sourceName}/stats`
    : `/collections/${context.sourceName}`;
  const response = await request.get(`${context.endpoint}${path}`, { headers });
  if (!response.ok()) {
    throw new Error(`${context.provider} fixture readiness returned HTTP ${response.status()}`);
  }
  const body = await response.json() as Record<string, unknown>;
  const observedCount = context.provider === 'meilisearch'
    ? body.numberOfDocuments
    : body.num_documents;
  if (observedCount !== context.seededDocumentCount) {
    throw new Error(
      `${context.provider} fixture has ${String(observedCount)} documents; expected ${context.seededDocumentCount}`,
    );
  }
}

async function startSourceProvider(
  request: APIRequestContext,
  provider: SourceProvider,
  fixtureControl: SourceProviderFixtureControl = runFixtureControl,
): Promise<SourceProviderContext> {
  const { stdout, stderr } = await fixtureControl('up', provider);
  const cleanupReceipt = parseSourceProviderCleanupReceipt(stderr, provider);
  try {
    const parsed = parseSourceProviderFixture(stdout, provider);
    const context: SourceProviderContext = {
      provider,
      ...parsed,
      endpoint: `http://127.0.0.1:${parsed.port}`,
    };
    await assertSourceReady(request, context);
    return context;
  } catch (error) {
    await fixtureControl('down', provider, cleanupReceipt);
    throw error;
  }
}

export async function startMeilisearchSource(
  request: APIRequestContext,
  fixtureControl: SourceProviderFixtureControl = runFixtureControl,
): Promise<SourceProviderContext> {
  return startSourceProvider(request, 'meilisearch', fixtureControl);
}

export async function startTypesenseSource(
  request: APIRequestContext,
  fixtureControl: SourceProviderFixtureControl = runFixtureControl,
): Promise<SourceProviderContext> {
  return startSourceProvider(request, 'typesense', fixtureControl);
}

export async function cleanupSourceContainer(context: SourceProviderContext): Promise<void> {
  await runFixtureControl('down', context.provider, {
    provider: context.provider,
    container: context.container,
    fixtureDir: context.fixtureDir,
    ownershipToken: context.ownershipToken,
  });
}

export async function cleanupMigratedIndexes(
  request: APIRequestContext,
  indexNames: string[],
): Promise<void> {
  await Promise.all(indexNames.map((indexName) => deleteIndex(request, indexName)));
}

export async function assertIndexNotCreated(
  request: APIRequestContext,
  indexName: string,
): Promise<void> {
  const response = await request.get(buildApiPath('/1/indexes'), {
    headers: API_HEADERS,
  });
  if (!response.ok()) {
    throw new Error(`index-list probe failed (${response.status()}) while checking dry-run state`);
  }

  const body = await response.json() as unknown;
  if (!isRecord(body)) {
    throw new Error('invalid index-list response while checking dry-run state');
  }
  const items = Array.isArray(body.items)
    ? body.items
    : (Array.isArray(body.results) ? body.results : undefined);
  if (!items || !items.every(isRecord)) {
    throw new Error('invalid index-list response while checking dry-run state');
  }

  const targetExists = items.some((item) => item.name === indexName || item.uid === indexName);
  if (targetExists) {
    throw new Error(`Dry-run preview created target index "${indexName}"`);
  }
}
