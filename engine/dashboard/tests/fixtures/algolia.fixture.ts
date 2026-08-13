import { algoliasearch } from 'algoliasearch';
import { randomUUID } from 'node:crypto';
import { PRODUCTS, SYNONYMS, RULES, SETTINGS } from './test-data';
import { API_HEADERS } from './local-instance';
import { buildApiPath, buildIndexPath, joinEncodedPath } from './index-api-helpers';
import { resolvePlaywrightSecretEnvPath } from '../global-setup';

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

/**
 * Closed set of credential-shape verdicts. `missing` means at least one value is
 * absent or empty, `blank` means a present value is entirely whitespace, and
 * `padded` means a present value carries leading or trailing whitespace. Only
 * `ok` describes a pair worth sending to a vendor.
 */
export type AlgoliaCredentialShape = 'ok' | 'missing' | 'blank' | 'padded';

export interface AlgoliaCredentialPair {
  appId: string | undefined;
  adminKey: string | undefined;
}

export interface AlgoliaCredentialSetupErrorInput {
  shape: AlgoliaCredentialShape;
  appIdLength: number;
  adminKeyLength: number;
  credentialSourcePath: string;
  readinessStatus?: number;
  readinessCause?: unknown;
}

const NO_SECRET_FILE_SOURCE = 'no secret file configured (FJ_NO_SECRET_FILE=1)';

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

export function buildAlgoliaMigrationIndexName(
  timestamp = Date.now(),
  pid = process.pid,
  nonce: string = randomUUID(),
): string {
  return `fj_e2e_migrate_${timestamp}_${pid}_${nonce}`;
}

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
 *
 * Truthiness only — it cannot tell a usable pair from a whitespace-padded or
 * blank one. Use `classifyAlgoliaCredentialShape` for the shape verdict and
 * `assertAlgoliaCredentialsReady` for the usable-against-the-vendor verdict.
 */
export function hasAlgoliaCredentials(): boolean {
  return !!(process.env.ALGOLIA_APP_ID && process.env.ALGOLIA_ADMIN_KEY);
}

/** Severity order used to report the worst verdict across the credential pair. */
const CREDENTIAL_SHAPE_SEVERITY: Record<AlgoliaCredentialShape, number> = {
  ok: 0,
  padded: 1,
  blank: 2,
  missing: 3,
};

function classifyCredentialValue(value: string | undefined): AlgoliaCredentialShape {
  if (!value) {
    return 'missing';
  }
  if (value.trim().length === 0) {
    return 'blank';
  }
  if (value !== value.trim()) {
    return 'padded';
  }
  return 'ok';
}

/**
 * Reports the worst shape verdict across the Algolia credential pair. Pure: it
 * reads nothing from the environment and makes no network call.
 */
export function classifyAlgoliaCredentialShape({
  appId,
  adminKey,
}: AlgoliaCredentialPair): AlgoliaCredentialShape {
  return [classifyCredentialValue(appId), classifyCredentialValue(adminKey)].reduce(
    (worst, verdict) => (
      CREDENTIAL_SHAPE_SEVERITY[verdict] > CREDENTIAL_SHAPE_SEVERITY[worst] ? verdict : worst
    ),
    'ok' as AlgoliaCredentialShape,
  );
}

/**
 * Raised when the Algolia credentials this suite was handed are not usable.
 *
 * This is a `setup-infra` fault, never a product defect and never a skip: the
 * migration path under test was never exercised, so neither a pass nor a
 * product-defect failure would be truthful. The message carries credential
 * lengths and the file the credentials came from — never a credential value.
 */
export class AlgoliaCredentialSetupError extends Error {
  readonly classification = 'setup-infra';
  readonly shape: AlgoliaCredentialShape;
  readonly readinessStatus?: number;
  readonly cause?: unknown;

  constructor({
    shape,
    appIdLength,
    adminKeyLength,
    credentialSourcePath,
    readinessStatus,
    readinessCause,
  }: AlgoliaCredentialSetupErrorInput) {
    const status = readinessStatus === undefined ? '' : `, readinessStatus=${readinessStatus}`;
    super(
      `[setup-infra] Algolia credentials are not usable (shape=${shape}, `
      + `appIdLength=${appIdLength}, adminKeyLength=${adminKeyLength}${status}, `
      + `credentialSource=${credentialSourcePath})`,
    );
    this.name = 'AlgoliaCredentialSetupError';
    this.shape = shape;
    this.readinessStatus = readinessStatus;
    this.cause = readinessCause;
  }
}

/** The secret file the credentials were loaded from, for setup-fault triage. */
function resolveAlgoliaCredentialSourcePath(): string {
  return resolvePlaywrightSecretEnvPath()?.path ?? NO_SECRET_FILE_SOURCE;
}

/**
 * Asserts the ambient Algolia credentials are usable before any test relies on
 * them: first the pure shape verdict, then a live authentication through the
 * same backend route the Migrate screen calls (`POST /1/algolia-list-indexes`).
 *
 * Failures raise `AlgoliaCredentialSetupError`, never a skip.
 */
export async function assertAlgoliaCredentialsReady(): Promise<void> {
  const appId = process.env.ALGOLIA_APP_ID;
  const adminKey = process.env.ALGOLIA_ADMIN_KEY;
  const shape = classifyAlgoliaCredentialShape({ appId, adminKey });
  const buildSetupError = (readinessStatus?: number, readinessCause?: unknown) => (
    new AlgoliaCredentialSetupError({
      shape,
      appIdLength: appId?.length ?? 0,
      adminKeyLength: adminKey?.length ?? 0,
      credentialSourcePath: resolveAlgoliaCredentialSourcePath(),
      readinessStatus,
      readinessCause,
    })
  );

  if (shape !== 'ok') {
    throw buildSetupError();
  }

  let response: Response;
  try {
    response = await fetch(buildApiPath('/1/algolia-list-indexes'), {
      method: 'POST',
      headers: API_HEADERS,
      body: JSON.stringify({ appId, apiKey: adminKey }),
    });
  } catch (cause) {
    throw buildSetupError(undefined, cause);
  }

  if (response.status !== 200) {
    throw buildSetupError(response.status);
  }
}

/**
 * Seeds an Algolia index with known test data (products, settings, synonyms, rules).
 * Polls until all documents are searchable before returning.
 */
export async function seedAlgoliaIndex(): Promise<AlgoliaTestContext> {
  // A fixture making a network call is deliberate, not stray I/O — do not remove.
  // `hasAlgoliaCredentials` gates on truthiness, so a blank, whitespace-padded, or
  // stale-but-present pair passed the gate and only surfaced downstream as a vendor
  // 403 that is indistinguishable from an Algolia outage. That ambiguity is what
  // made P29 a misdiagnosis. Authenticating here, before any data is written,
  // separates "our credentials are wrong" from "the vendor is down" at the one
  // point where the answer is still unambiguous. It probes through
  // POST /1/algolia-list-indexes because that is the route the Migrate screen
  // itself calls; if that route ever stops distinguishing a credential fault from
  // a backend fault, replace it with a direct vendor call and say so here.
  await assertAlgoliaCredentialsReady();

  const appId = process.env.ALGOLIA_APP_ID!;
  const adminKey = process.env.ALGOLIA_ADMIN_KEY!;
  const indexName = buildAlgoliaMigrationIndexName();
  const targetIndexName = `${indexName}_target`;
  const invalidTargetIndexName = `${indexName}_invalid_target`;

  const client = algoliasearch(appId, adminKey);

  try {
    await client.setSettings({ indexName, indexSettings: SETTINGS });

    for (const syn of SYNONYMS) {
      await client.saveSynonym({
        indexName,
        objectID: syn.objectID,
        synonymHit: syn as SaveSynonymRequest['synonymHit'],
      });
    }

    for (const rule of RULES) {
      await client.saveRule({
        indexName,
        objectID: rule.objectID,
        rule: rule as SaveRuleRequest['rule'],
      });
    }

    await client.saveObjects({ indexName, objects: PRODUCTS });
    await pollAlgoliaReady(client, indexName, PRODUCTS.length);
  } catch (seedError) {
    try {
      await client.deleteIndex({ indexName });
    } catch (rollbackError) {
      throw new AggregateError(
        [seedError, rollbackError],
        `Algolia seed failed for "${indexName}" and rollback also failed`,
      );
    }
    throw seedError;
  }

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
