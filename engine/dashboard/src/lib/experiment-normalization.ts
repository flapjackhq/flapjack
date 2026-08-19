/**
 * @module Stub summary for experiment-normalization.ts.
 */
import type { Experiment } from '@/lib/types';
import { asExperimentRecord } from './experiment-api-contract';

type RawExperimentListResponse = {
  abtests?: unknown;
};

type RawRecord = Record<string, unknown>;

const DEFAULT_PRIMARY_METRIC = 'ctr';

function asNumber(value: unknown): number | undefined {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value;
  }

  if (typeof value === 'string' && value.trim().length > 0) {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : undefined;
  }

  return undefined;
}

function asTimestamp(value: unknown): number | undefined {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value;
  }

  if (typeof value === 'string' && value.trim().length > 0) {
    const parsed = Date.parse(value);
    return Number.isFinite(parsed) ? parsed : undefined;
  }

  return undefined;
}

function asString(value: unknown): string | undefined {
  if (typeof value === 'string' && value.length > 0) {
    return value;
  }

  if (typeof value === 'number' && Number.isFinite(value)) {
    return String(value);
  }

  return undefined;
}

function readStartedAt(record: RawRecord): number | undefined {
  return (
    asTimestamp(record.startedAt) ??
    asTimestamp(record.startAt) ??
    asTimestamp(record.startDate)
  );
}

function readEndedAt(record: RawRecord): number | undefined {
  return asTimestamp(record.endedAt) ?? asTimestamp(record.endAt);
}

/**
 * TODO: Document normalizePrimaryMetric.
 */
function normalizePrimaryMetric(metric: unknown): string {
  const rawMetric = asString(metric);
  switch (rawMetric) {
    case 'clickThroughRate':
    case 'ctr':
      return 'ctr';
    case 'conversionRate':
      return 'conversionRate';
    case 'revenue':
    case 'revenuePerSearch':
      return 'revenuePerSearch';
    case 'zeroResultRate':
      return 'zeroResultRate';
    case 'abandonmentRate':
      return 'abandonmentRate';
    default:
      return DEFAULT_PRIMARY_METRIC;
  }
}

function normalizeTrafficSplit(value: unknown): number {
  const parsed = asNumber(value);
  if (parsed === undefined) return 0.5;
  const scaled = parsed > 1 ? parsed / 100 : parsed;
  return Math.min(1, Math.max(0, scaled));
}

/**
 * TODO: Document normalizeStatus.
 */
function normalizeStatus(record: RawRecord): Experiment['status'] {
  const status = asString(record.status);
  if (
    status === 'draft' ||
    status === 'running' ||
    status === 'stopped' ||
    status === 'concluded' ||
    status === 'expired'
  ) {
    return status;
  }

  if (status === 'active') {
    return readStartedAt(record) === undefined ? 'draft' : 'running';
  }

  return 'draft';
}

/**
 * Maps the varied experiment payloads used by the backend and Algolia-style endpoints
 * into the dashboard's stable `Experiment` shape.
 */
export function normalizeExperimentRecord(rawExperiment: unknown): Experiment {
  const record = asExperimentRecord(rawExperiment);
  const variants = Array.isArray(record.variants) ? record.variants : [];
  const controlVariant = asExperimentRecord(variants[0]);
  const variantArm = asExperimentRecord(variants[1]);
  const control = asExperimentRecord(record.control);
  const variant = asExperimentRecord(record.variant);
  const responseMetric =
    Array.isArray(record.metrics) && record.metrics.length > 0
      ? asExperimentRecord(record.metrics[0]).name
      : undefined;

  const createdAt = asTimestamp(record.createdAt) ?? Date.now();
  const startedAt = readStartedAt(record) ?? null;
  const endedAt = readEndedAt(record) ?? null;
  const minimumDaysFromDates =
    endedAt && createdAt && endedAt > createdAt
      ? Math.ceil((endedAt - createdAt) / (1000 * 60 * 60 * 24))
      : 14;
  const minimumDays = asNumber(record.minimumDays) ?? minimumDaysFromDates;

  const variantTrafficPercentage = asNumber(variantArm.trafficPercentage);

  return {
    id: asString(record.id) ?? asString(record.abTestID) ?? '',
    name: asString(record.name) ?? '',
    indexName: asString(record.indexName) ?? asString(controlVariant.index) ?? '',
    status: normalizeStatus(record),
    trafficSplit:
      asNumber(record.trafficSplit) !== undefined
        ? normalizeTrafficSplit(record.trafficSplit)
        : normalizeTrafficSplit(variantTrafficPercentage),
    control: {
      name: asString(control.name) ?? asString(controlVariant.description) ?? 'control',
      queryOverrides: control.queryOverrides as Record<string, unknown> | undefined,
      indexName: asString(control.indexName),
    },
    variant: {
      name: asString(variant.name) ?? asString(variantArm.description) ?? 'variant',
      queryOverrides:
        (variant.queryOverrides as Record<string, unknown> | undefined) ??
        (variantArm.customSearchParameters as Record<string, unknown> | undefined),
      indexName: asString(variant.indexName) ?? asString(variantArm.index),
    },
    primaryMetric: normalizePrimaryMetric(record.primaryMetric ?? responseMetric),
    createdAt,
    startedAt,
    endedAt,
    minimumDays,
    winsorizationCap: asNumber(record.winsorizationCap) ?? null,
  };
}

/**
 * Accepts either a raw array or the Algolia `{ abtests: [...] }` envelope and returns
 * the dashboard's canonical experiment list.
 */
export function normalizeExperimentListResponse(data: unknown): Experiment[] {
  if (Array.isArray(data)) {
    return data.map(normalizeExperimentRecord);
  }

  const body = asExperimentRecord(data) as RawExperimentListResponse;
  if (!Array.isArray(body.abtests)) {
    return [];
  }
  return body.abtests.map(normalizeExperimentRecord);
}

export interface InterleavingResultsResponse {
  deltaAB: number;
  winsControl: number;
  winsVariant: number;
  ties: number;
  pValue: number;
  significant: boolean;
  totalQueries: number;
  dataQualityOk: boolean;
}

export interface GuardRailAlertResponse {
  metricName: string;
  controlValue: number;
  variantValue: number;
  dropPct: number;
}

export interface ExperimentConclusionResponse {
  winner: string | null;
  reason: string;
  controlMetric: number;
  variantMetric: number;
  confidence: number;
  significant: boolean;
  promoted: boolean;
}

export interface ArmResultsResponse {
  name: string;
  searches: number;
  users: number;
  clicks: number;
  conversions: number;
  revenue: number;
  ctr: number;
  conversionRate: number;
  revenuePerSearch: number;
  zeroResultRate: number;
  abandonmentRate: number;
  meanClickRank: number;
}

export interface SignificanceResponse {
  zScore: number;
  pValue: number;
  confidence: number;
  significant: boolean;
  relativeImprovement: number;
  winner: string | null;
}

/**
 * Canonical dashboard results model for a single experiment.
 */
export interface ExperimentResultsResponse {
  experimentID: string;
  name: string;
  status: string;
  indexName: string;
  startDate: string | null;
  endedAt: string | null;
  conclusion: ExperimentConclusionResponse | null;
  trafficSplit: number;
  primaryMetric: string;
  gate: {
    minimumNReached: boolean;
    minimumDaysReached: boolean;
    readyToRead: boolean;
    requiredSearchesPerArm: number;
    currentSearchesPerArm: number;
    progressPct: number;
    estimatedDaysRemaining: number | null;
  };
  control: ArmResultsResponse;
  variant: ArmResultsResponse;
  significance: SignificanceResponse | null;
  bayesian: { probVariantBetter: number } | null;
  sampleRatioMismatch: boolean;
  cupedApplied: boolean;
  guardRailAlerts: GuardRailAlertResponse[];
  outlierUsersExcluded: number;
  noStableIdQueries: number;
  recommendation: string | null;
  interleaving: InterleavingResultsResponse | null;
}

type ExperimentResultsApiResponse = ExperimentResultsResponse & {
  interleaving: (InterleavingResultsResponse & { deltaAb?: number }) | null;
};

/**
 * Normalizes wire-format variations in experiment results while preserving the
 * dashboard's public response contract.
 */
export function normalizeExperimentResultsResponse(
  response: ExperimentResultsApiResponse,
): ExperimentResultsResponse {
  const interleaving = response.interleaving;
  if (!interleaving) {
    return response;
  }

  return {
    ...response,
    interleaving: {
      ...interleaving,
      deltaAB:
        typeof interleaving.deltaAB === 'number'
          ? interleaving.deltaAB
          : interleaving.deltaAb ?? 0,
    },
  };
}
