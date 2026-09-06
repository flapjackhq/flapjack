import type { APIRequestContext } from '@playwright/test';
import type { DashboardCreateExperimentPayload } from '../../src/lib/experiment-api-contract';
import {
  createExperiment,
  deleteExperiment,
  flushAnalytics,
  getExperimentResults,
  listExperiments,
  sendEvents,
  type ExperimentRecord,
  type ExperimentResultsRecord,
  type InsightEvent,
} from './api-helpers';
import { API_BASE, API_HEADERS } from './local-instance';

export const STAGE_1_ROUTE_AUDIT_EXPERIMENT_NAME_PREFIX = 'stage-1-route-audit-experiment';
export const STAGE_1_ROUTE_AUDIT_EXPERIMENT_INDEX = 'stage_1_route_audit_products';
const STALE_ROUTE_AUDIT_EXPERIMENT_AGE_MS = 60 * 60 * 1000;

let routeAuditExperimentSequence = 0;

function nextRouteAuditExperimentName(): string {
  routeAuditExperimentSequence += 1;
  return [
    STAGE_1_ROUTE_AUDIT_EXPERIMENT_NAME_PREFIX,
    Date.now(),
    process.pid,
    routeAuditExperimentSequence,
  ].join('-');
}

function buildRouteAuditExperimentPayload(name: string): DashboardCreateExperimentPayload {
  return {
    name,
    indexName: STAGE_1_ROUTE_AUDIT_EXPERIMENT_INDEX,
    trafficSplit: 0.5,
    control: { name: 'Route audit control' },
    variant: {
      name: 'Route audit variant',
      queryOverrides: { typoTolerance: false },
    },
    primaryMetric: 'ctr',
    minimumDays: 7,
  };
}

function readRouteAuditExperimentCreatedAt(name: string): number | null {
  const prefix = `${STAGE_1_ROUTE_AUDIT_EXPERIMENT_NAME_PREFIX}-`;
  if (!name.startsWith(prefix)) {
    return null;
  }

  const [timestamp] = name.slice(prefix.length).split('-');
  const createdAt = Number(timestamp);
  return Number.isSafeInteger(createdAt) ? createdAt : null;
}

function isStaleRouteAuditExperiment(experiment: ExperimentRecord, now: number): boolean {
  const createdAt = readRouteAuditExperimentCreatedAt(experiment.name);
  return createdAt !== null && now - createdAt >= STALE_ROUTE_AUDIT_EXPERIMENT_AGE_MS;
}

async function cleanupStaleRouteAuditExperiments(request: APIRequestContext): Promise<void> {
  const now = Date.now();
  const experiments = await listExperiments(request);
  const staleExperiments = experiments.filter((experiment) => (
    isStaleRouteAuditExperiment(experiment, now)
  ));

  for (const experiment of staleExperiments) {
    await deleteExperiment(request, experiment.id);
  }
}

export interface SeededRouteAuditExperiment {
  id: string;
  name: string;
  indexName: typeof STAGE_1_ROUTE_AUDIT_EXPERIMENT_INDEX;
  status: 'draft';
  primaryMetricLabel: 'CTR';
}

function readinessError(experimentId: string, expectedName: string, cause: string): Error {
  return new Error(
    `Route audit experiment ${experimentId} is not ready with expected name `
      + `"${expectedName}" (${cause})`,
  );
}

async function assertExperimentReady(
  request: APIRequestContext,
  experimentId: string,
  expectedName: string,
): Promise<void> {
  const response = await request.get(
    `${API_BASE}/2/abtests/${encodeURIComponent(experimentId)}`,
    { headers: API_HEADERS },
  );
  if (!response.ok()) {
    throw readinessError(experimentId, expectedName, `HTTP ${response.status()}`);
  }

  const body = await response.json() as { name?: unknown };
  if (body.name !== expectedName) {
    throw readinessError(experimentId, expectedName, `got name "${String(body.name)}"`);
  }
}

export async function seedRouteAuditExperiment(
  request: APIRequestContext,
): Promise<SeededRouteAuditExperiment> {
  await cleanupStaleRouteAuditExperiments(request);

  const name = nextRouteAuditExperimentName();

  // createExperiment already throws when the response carries no id-like field,
  // so the runtime id here is guaranteed non-empty.
  const { id } = await createExperiment(request, buildRouteAuditExperimentPayload(name));

  try {
    await assertExperimentReady(request, id, name);
  } catch (error) {
    await deleteExperiment(request, id);
    throw error;
  }

  // Callers may navigate with this runtime id because the by-id read has already passed.
  return {
    id,
    name,
    indexName: STAGE_1_ROUTE_AUDIT_EXPERIMENT_INDEX,
    status: 'draft',
    primaryMetricLabel: 'CTR',
  };
}

type ExperimentBatchResult = {
  abTestVariantID?: unknown;
  hits?: Array<{ objectID?: unknown }>;
  queryID?: unknown;
};

const EXPERIMENT_BATCH_LIMIT = 50;
const EXPERIMENT_SEARCHES_PER_ARM = 200;
const EXPERIMENT_CONTROL_CLICKS = 190;

async function runExperimentBatch(
  request: APIRequestContext,
  indexName: string,
  userTokens: string[],
  analytics: boolean,
): Promise<ExperimentBatchResult[]> {
  const response = await request.post(`${API_BASE}/1/indexes/*/queries`, {
    headers: API_HEADERS,
    data: {
      requests: userTokens.map((userToken) => ({
        indexName,
        query: 'apple',
        userToken,
        analytics,
        clickAnalytics: analytics,
        hitsPerPage: 1,
      })),
    },
  });
  if (!response.ok()) {
    throw new Error(`experiment batch failed (${response.status()}): ${await response.text()}`);
  }

  const body = await response.json() as { results?: ExperimentBatchResult[] };
  if (!Array.isArray(body.results) || body.results.length !== userTokens.length) {
    throw new Error('experiment batch returned an unexpected result count');
  }
  return body.results;
}

function readAssignedArm(result: ExperimentBatchResult): 'control' | 'variant' | null {
  return result.abTestVariantID === 'control' || result.abTestVariantID === 'variant'
    ? result.abTestVariantID
    : null;
}

function clickEventForResult(
  result: ExperimentBatchResult,
  indexName: string,
  userToken: string,
  arm: 'control' | 'variant',
  ordinal: number,
): InsightEvent {
  const queryID = typeof result.queryID === 'string' ? result.queryID : null;
  const objectID = Array.isArray(result.hits) && typeof result.hits[0]?.objectID === 'string'
    ? result.hits[0].objectID
    : null;
  if (!queryID || !objectID) {
    throw new Error(`tracked ${arm} search ${ordinal} lacked queryID or objectID`);
  }

  return {
    eventType: 'click',
    eventName: `deterministic-experiment-${arm}-${ordinal}`,
    index: indexName,
    userToken,
    objectIDs: [objectID],
    positions: [1],
    queryID,
  };
}

function assertKnownExperimentResults(results: ExperimentResultsRecord): void {
  const control = results.control as Record<string, unknown>;
  const variant = results.variant as Record<string, unknown>;
  if (
    control.searches !== EXPERIMENT_SEARCHES_PER_ARM
    || variant.searches !== EXPERIMENT_SEARCHES_PER_ARM
    || control.clicks !== EXPERIMENT_CONTROL_CLICKS
    || variant.clicks !== EXPERIMENT_SEARCHES_PER_ARM
    || control.ctr !== 0.95
    || variant.ctr !== 1
  ) {
    throw new Error(`unexpected deterministic experiment results: ${JSON.stringify({ control, variant })}`);
  }
}

function deterministicCandidateUserToken(index: number): string {
  return `00000000-0000-4000-8000-${(index + 1).toString(16).padStart(12, '0')}`;
}

/**
 * Seed one stable token per arm with eight bounded 50-query batches.
 *
 * A single analytics-disabled candidate batch resolves both assignments. The
 * selected tokens are then reused, proving stable-userToken behavior without
 * a sequential arm-discovery loop or NaN-based minimum-N shortcut.
 */
export async function seedDeterministicExperimentTraffic(
  request: APIRequestContext,
  experimentId: string,
  indexName: string,
): Promise<ExperimentResultsRecord> {
  const candidateTokens = Array.from(
    { length: EXPERIMENT_BATCH_LIMIT },
    (_, index) => deterministicCandidateUserToken(index),
  );
  const candidateResults = await runExperimentBatch(
    request,
    indexName,
    candidateTokens,
    false,
  );
  const tokenByArm = new Map<'control' | 'variant', string>();
  candidateResults.forEach((result, index) => {
    const arm = readAssignedArm(result);
    if (arm && !tokenByArm.has(arm)) {
      tokenByArm.set(arm, candidateTokens[index]);
    }
  });

  const controlToken = tokenByArm.get('control');
  const variantToken = tokenByArm.get('variant');
  if (!controlToken || !variantToken) {
    throw new Error('deterministic candidate batch did not contain both experiment arms');
  }

  const clickEvents: InsightEvent[] = [];
  for (const [arm, userToken] of [
    ['control', controlToken],
    ['variant', variantToken],
  ] as const) {
    for (let batch = 0; batch < EXPERIMENT_SEARCHES_PER_ARM / EXPERIMENT_BATCH_LIMIT; batch += 1) {
      const batchResults = await runExperimentBatch(
        request,
        indexName,
        Array(EXPERIMENT_BATCH_LIMIT).fill(userToken),
        true,
      );
      batchResults.forEach((result, index) => {
        const ordinal = batch * EXPERIMENT_BATCH_LIMIT + index;
        if (readAssignedArm(result) !== arm) {
          throw new Error(`${arm} stable userToken changed assignment at search ${ordinal}`);
        }
        if (arm === 'variant' || ordinal < EXPERIMENT_CONTROL_CLICKS) {
          clickEvents.push(clickEventForResult(result, indexName, userToken, arm, ordinal));
        }
      });
    }
  }

  await sendEvents(request, clickEvents);
  await flushAnalytics(request, indexName);
  const results = await getExperimentResults(request, experimentId);
  assertKnownExperimentResults(results);
  return results;
}
