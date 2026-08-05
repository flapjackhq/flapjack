import type { APIRequestContext } from '@playwright/test';
import type { DashboardCreateExperimentPayload } from '../../src/lib/experiment-api-contract';
import {
  createExperiment,
  deleteExperiment,
  listExperiments,
  type ExperimentRecord,
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
