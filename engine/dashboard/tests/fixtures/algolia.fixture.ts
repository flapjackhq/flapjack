import { algoliasearch } from 'algoliasearch';
import { PRODUCTS, SYNONYMS, RULES, SETTINGS } from './test-data';
import { API_HEADERS } from './local-instance';
import { buildApiPath, buildIndexPath, joinEncodedPath } from './index-api-helpers';

export interface AlgoliaTestContext {
  appId: string;
  adminKey: string;
  indexName: string;
  targetIndexName: string;
  invalidTargetIndexName: string;
}

export interface MigrationCleanupReceipt {
  algoliaSource: string;
  flapjackTargets: Record<string, string>;
}

export type AlgoliaCredentialMode = 'run' | 'fail';

export interface AlgoliaCredentialModeInput {
  hasCredentials: boolean;
}

interface DeletionProbeResult {
  deleted: boolean;
  observation: string;
}

interface FlapjackTaskStatus {
  status?: string;
  error?: string;
}

interface FlapjackIndexListItem {
  name?: string;
  uid?: string;
}

type SearchClient = ReturnType<typeof algoliasearch>;
type SaveSynonymRequest = Parameters<SearchClient['saveSynonym']>[0];
type SaveRuleRequest = Parameters<SearchClient['saveRule']>[0];

const CLEANUP_POLL_INTERVAL_MS = 500;
const CLEANUP_TIMEOUT_MS = 20_000;

export class MissingAlgoliaCredentialsError extends Error {
  constructor() {
    super('Missing required Algolia credentials: ALGOLIA_APP_ID and ALGOLIA_ADMIN_KEY');
    this.name = 'MissingAlgoliaCredentialsError';
  }
}

export function resolveAlgoliaCredentialMode({
  hasCredentials,
}: AlgoliaCredentialModeInput): AlgoliaCredentialMode {
  if (hasCredentials) {
    return 'run';
  }
  // Input-spec-required: missing required Algolia credentials fail closed
  // everywhere; see chats/icg/jul16_3pm_8_green_by_absence_standard.md.
  return 'fail';
}

/**
 * Returns true if Algolia credentials are available in the environment.
 */
export function hasAlgoliaCredentials(): boolean {
  return !!(process.env.ALGOLIA_APP_ID && process.env.ALGOLIA_ADMIN_KEY);
}

/**
 * Seeds an Algolia index with known test data (products, settings, synonyms, rules).
 * Polls until all documents are searchable before returning.
 */
export async function seedAlgoliaIndex(): Promise<AlgoliaTestContext> {
  const appId = process.env.ALGOLIA_APP_ID!;
  const adminKey = process.env.ALGOLIA_ADMIN_KEY!;
  const indexName = `fj_e2e_migrate_${Date.now()}`;
  const targetIndexName = `${indexName}_target`;
  const invalidTargetIndexName = `${indexName}_invalid_target`;

  const client = algoliasearch(appId, adminKey);

  // Apply settings
  await client.setSettings({ indexName, indexSettings: SETTINGS });

  // Save synonyms
  for (const syn of SYNONYMS) {
    await client.saveSynonym({
      indexName,
      objectID: syn.objectID,
      synonymHit: syn as SaveSynonymRequest['synonymHit'],
    });
  }

  // Save rules
  for (const rule of RULES) {
    await client.saveRule({
      indexName,
      objectID: rule.objectID,
      rule: rule as SaveRuleRequest['rule'],
    });
  }

  // Save objects
  await client.saveObjects({ indexName, objects: PRODUCTS });

  // Poll until all documents are indexed and searchable
  await pollAlgoliaReady(client, indexName, PRODUCTS.length);

  return { appId, adminKey, indexName, targetIndexName, invalidTargetIndexName };
}

/**
 * Deletes the stage-owned Algolia and Flapjack indexes, then waits until both
 * backends confirm the index name is gone. Residue is a test failure.
 */
export async function cleanupMigrationIndexes(
  ctx: AlgoliaTestContext,
): Promise<MigrationCleanupReceipt> {
  const flapjackIndexNames = [ctx.indexName, ctx.targetIndexName, ctx.invalidTargetIndexName];
  await Promise.all([
    deleteAlgoliaIndex(ctx),
    ...flapjackIndexNames.map((indexName) => deleteFlapjackIndex(indexName)),
  ]);
  const [algoliaSource, ...flapjackObservations] = await Promise.all([
    waitForDeletion('Algolia', ctx.indexName, () => probeAlgoliaIndexDeleted(ctx)),
    ...flapjackIndexNames.map((indexName) => (
      waitForDeletion('Flapjack', indexName, () => probeFlapjackIndexDeleted(indexName))
    )),
  ]);

  return {
    algoliaSource,
    flapjackTargets: Object.fromEntries(
      flapjackIndexNames.map((indexName, index) => [indexName, flapjackObservations[index]]),
    ),
  };
}

/**
 * Deletes the Algolia test index. Cleanup verification happens separately so a
 * transient delete error is tolerated only when the index is already gone.
 */
async function deleteAlgoliaIndex(ctx: AlgoliaTestContext): Promise<void> {
  const client = algoliasearch(ctx.appId, ctx.adminKey);
  await client.deleteIndex({ indexName: ctx.indexName }).catch(() => {});
}

/**
 * Deletes a Flapjack index via the REST API. Cleanup verification happens
 * after the delete task publishes so the final deletion probe observes the
 * backend's steady state instead of an accepted-but-not-yet-applied mutation.
 */
async function deleteFlapjackIndex(indexName: string): Promise<void> {
  const response = await fetch(buildIndexPath(indexName), {
    method: 'DELETE',
    headers: API_HEADERS,
  });

  if (response.status === 404) {
    return;
  }
  if (!response.ok) {
    throw new Error(`Flapjack deleteIndex failed (${response.status})`);
  }

  const body = await response.json() as Record<string, unknown>;
  if (typeof body.taskID === 'number') {
    await waitForFlapjackTaskPublished(body.taskID);
  }
}

/**
 * Polls Algolia until the expected number of documents are searchable.
 */
async function pollAlgoliaReady(
  client: SearchClient,
  indexName: string,
  expectedCount: number,
  maxWaitMs = 20_000,
): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < maxWaitMs) {
    try {
      const result = await client.search({
        requests: [{ indexName, query: '' }],
      });
      const first = result.results[0];
      if ('nbHits' in first && typeof first.nbHits === 'number' && first.nbHits >= expectedCount) return;
    } catch {
      // Index may not exist yet — keep polling
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(
    `Algolia indexing timeout: expected ${expectedCount} docs in "${indexName}" after ${maxWaitMs}ms`,
  );
}

function buildAlgoliaIndexUrl(appId: string, indexName: string, ...segments: string[]): string {
  return `https://${appId}.algolia.net/${joinEncodedPath('1', 'indexes', indexName, ...segments)}`;
}

function formatUnknownError(error: unknown): string {
  if (error instanceof Error) {
    return `${error.name}: ${error.message}`;
  }
  return String(error);
}

async function waitForDeletion(
  owner: string,
  indexName: string,
  probe: () => Promise<DeletionProbeResult>,
  maxWaitMs = CLEANUP_TIMEOUT_MS,
): Promise<string> {
  const start = Date.now();
  let lastObservation = 'no deletion confirmation observed';

  while (Date.now() - start < maxWaitMs) {
    try {
      const result = await probe();
      if (result.deleted) {
        return result.observation;
      }
      lastObservation = result.observation;
    } catch (error) {
      lastObservation = `probe failed: ${formatUnknownError(error)}`;
    }

    await new Promise((resolve) => setTimeout(resolve, CLEANUP_POLL_INTERVAL_MS));
  }

  throw new Error(
    `${owner} cleanup left stage-owned index "${indexName}" behind after ${maxWaitMs}ms (${lastObservation})`,
  );
}

async function probeAlgoliaIndexDeleted(ctx: AlgoliaTestContext): Promise<DeletionProbeResult> {
  const response = await fetch(buildAlgoliaIndexUrl(ctx.appId, ctx.indexName, 'settings'), {
    headers: {
      'x-algolia-application-id': ctx.appId,
      'x-algolia-api-key': ctx.adminKey,
    },
  });

  return {
    deleted: response.status === 404,
    observation: `GET settings returned ${response.status}`,
  };
}

async function probeFlapjackIndexDeleted(indexName: string): Promise<DeletionProbeResult> {
  const response = await fetch(buildApiPath('/1/indexes'), {
    headers: API_HEADERS,
  });
  if (!response.ok) {
    return {
      deleted: false,
      observation: `GET /1/indexes returned ${response.status}`,
    };
  }

  const body = await response.json() as {
    items?: FlapjackIndexListItem[];
    results?: FlapjackIndexListItem[];
  };
  const items = Array.isArray(body.items)
    ? body.items
    : (Array.isArray(body.results) ? body.results : []);
  const stillPresent = items.some((item) => item.name === indexName || item.uid === indexName);
  return {
    deleted: !stillPresent,
    observation: stillPresent
      ? `GET /1/indexes still lists ${indexName}`
      : `GET /1/indexes no longer lists ${indexName}`,
  };
}

async function waitForFlapjackTaskPublished(
  taskID: number,
  maxWaitMs = CLEANUP_TIMEOUT_MS,
): Promise<void> {
  const start = Date.now();
  let lastStatus = 'not yet observed';

  while (Date.now() - start < maxWaitMs) {
    const response = await fetch(buildApiPath('/1/tasks', String(taskID)), {
      headers: API_HEADERS,
    });

    if (!response.ok) {
      lastStatus = `GET task returned ${response.status}`;
    } else {
      const task = await response.json() as FlapjackTaskStatus;
      if (task.status === 'published') {
        return;
      }
      if (task.status === 'error') {
        throw new Error(`Flapjack delete task ${taskID} failed: ${task.error ?? 'unknown error'}`);
      }
      lastStatus = `task status=${task.status ?? 'unknown'}`;
    }

    await new Promise((resolve) => setTimeout(resolve, CLEANUP_POLL_INTERVAL_MS));
  }

  throw new Error(
    `Flapjack delete task ${taskID} did not publish after ${maxWaitMs}ms (${lastStatus})`,
  );
}
