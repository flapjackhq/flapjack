import axios from 'axios';
import { useAuth } from '@/hooks/useAuth';

export type MigrationProviderId = 'algolia' | 'meilisearch' | 'typesense';

export interface MigrationProviderDescriptor {
  id: MigrationProviderId;
  routeSegment: MigrationProviderId;
  displayName: string;
  firstCredentialField: {
    label: string;
    requestKey: string;
    placeholder: string;
  };
  apiKeyLabel: string;
  sourceFieldLabel: string;
  connectionErrorMessage: string;
  terminalFailureMessage: string;
  loopbackOptInName?: string;
}

// Keep dashboard payload construction downstream of the backend's
// provider-specific migration request schemas.
export const MIGRATION_PROVIDER_DESCRIPTORS: readonly MigrationProviderDescriptor[] = [
  {
    id: 'algolia',
    routeSegment: 'algolia',
    displayName: 'Algolia',
    firstCredentialField: {
      label: 'Application ID',
      requestKey: 'appId',
      placeholder: 'YourAlgoliaAppId',
    },
    apiKeyLabel: 'Admin API Key',
    sourceFieldLabel: 'Source Index (Algolia)',
    connectionErrorMessage: 'Could not connect to Algolia. Check your App ID and API Key.',
    terminalFailureMessage:
      'Algolia upstream rejected the request or the migration failed before it completed.',
  },
  {
    id: 'meilisearch',
    routeSegment: 'meilisearch',
    displayName: 'Meilisearch',
    firstCredentialField: {
      label: 'Endpoint',
      requestKey: 'endpoint',
      placeholder: 'https://example.meilisearch.io',
    },
    apiKeyLabel: 'API Key',
    sourceFieldLabel: 'Source index',
    connectionErrorMessage: 'Could not connect to Meilisearch. Check your endpoint and API key.',
    terminalFailureMessage: 'Meilisearch migration failed before it completed.',
    loopbackOptInName: 'FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK',
  },
  {
    id: 'typesense',
    routeSegment: 'typesense',
    displayName: 'Typesense',
    firstCredentialField: {
      label: 'Node URL',
      requestKey: 'node',
      placeholder: 'https://example.typesense.net',
    },
    apiKeyLabel: 'API Key',
    sourceFieldLabel: 'Source collection',
    connectionErrorMessage: 'Could not connect to Typesense. Check your node URL and API key.',
    terminalFailureMessage: 'Typesense migration failed before it completed.',
    loopbackOptInName: 'FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK',
  },
] as const;

export interface SourceIndexSummary {
  name: string;
  entries?: number;
  documentCount?: number;
  defaultSortingField?: string;
  updatedAt?: string;
}

export interface ListSourceIndexesResponse {
  indexes: SourceIndexSummary[];
  total?: number;
  offset?: number;
  limit?: number;
}

interface MigrationCount {
  imported: number;
}

export interface AsyncMigrationStatusResponse {
  jobId: string;
  phase: string;
  disposition: 'running' | 'succeeded' | 'failed' | 'cancelled';
  targetIndex?: string;
  createdAt: string;
  updatedAt: string;
  terminalAt?: string;
  settingsApplied?: boolean;
  objectsImported?: MigrationCount;
  synonymsImported?: MigrationCount;
  rulesImported?: MigrationCount;
  warnings?: Array<{
    code: string;
    message: string;
    resource: string;
    jsonPath: string;
  }>;
}

export type AsyncMigrationViewState =
  | { kind: 'running'; status: AsyncMigrationStatusResponse }
  | {
    kind: 'success';
    status: AsyncMigrationStatusResponse;
    counts: {
      documents: number;
      settings: boolean;
      synonyms: number;
      rules: number;
    };
  }
  | { kind: 'error'; status: AsyncMigrationStatusResponse };

export interface MigrationResult {
  status: AsyncMigrationStatusResponse;
  counts: Extract<AsyncMigrationViewState, { kind: 'success' }>['counts'];
}

export interface MigrationPreviewResponse {
  report: MigrationPreviewReport;
  sourceCounts: MigrationPreviewSourceCounts;
}

export interface MigrationPreviewReport {
  entries: MigrationPreviewReportEntry[];
  summary: MigrationPreviewReportSummary;
  reportDigest: string | null;
}

export interface MigrationPreviewReportSummary {
  totalEntries: number;
  hardRejections: number;
  warnings: number;
  scopeGaps: number;
}

export interface MigrationPreviewReportEntry {
  severity: 'ScopeGap' | 'Warning' | 'HardRejection';
  code: string;
  resource: string;
  pageIndex: number | null;
  itemIndex: number | null;
  jsonPath: string;
}

export interface MigrationPreviewSourceCounts {
  indexes: number;
  records: number;
}

interface BuildMigrationRequestBodyInput {
  provider: MigrationProviderDescriptor;
  firstCredentialValue: string;
  apiKey: string;
  sourceIndex: string;
  targetIndex: string;
  overwrite: boolean;
  sourceWriteFrozen?: boolean;
}

export function buildDashboardAuthHeaders(): Record<string, string> {
  const { appId } = useAuth.getState();
  return {
    'Content-Type': 'application/json',
    'x-algolia-application-id': appId || 'flapjack',
  };
}

export async function postSensitiveMigrationRequest<TResponse>(
  url: string,
  body: Record<string, unknown>,
): Promise<TResponse> {
  // The shared dashboard client persists request bodies into API Logs.
  // Send third-party Algolia credentials outside that logger so secrets never
  // land in sessionStorage or the Search Logs UI.
  const response = await axios.post<TResponse>(url, body, {
    headers: buildDashboardAuthHeaders(),
  });
  return response.data;
}

function buildProviderCredentialFields(
  provider: MigrationProviderDescriptor,
  firstCredentialValue: string,
  apiKey: string,
): Record<string, string> {
  return {
    [provider.firstCredentialField.requestKey]: firstCredentialValue,
    apiKey,
  };
}

export function buildDiscoveryRequestBody(
  provider: MigrationProviderDescriptor,
  firstCredentialValue: string,
  apiKey: string,
): Record<string, unknown> {
  return buildProviderCredentialFields(provider, firstCredentialValue, apiKey);
}

export function buildMigrationRequestBody({
  provider,
  firstCredentialValue,
  apiKey,
  sourceIndex,
  targetIndex,
  overwrite,
  sourceWriteFrozen,
}: BuildMigrationRequestBodyInput): Record<string, unknown> {
  const body: Record<string, unknown> = {
    ...buildProviderCredentialFields(provider, firstCredentialValue, apiKey),
    sourceIndex,
  };

  if (targetIndex) {
    body.targetIndex = targetIndex;
  }
  if (overwrite) {
    body.overwrite = true;
  }
  if (provider.id === 'typesense' && sourceWriteFrozen === true) {
    body.sourceWriteFrozen = true;
  }

  return body;
}

export function buildAsyncMigrationViewState(
  status: AsyncMigrationStatusResponse,
): AsyncMigrationViewState {
  if (status.disposition === 'succeeded' && status.terminalAt) {
    return {
      kind: 'success',
      status,
      counts: {
        documents: status.objectsImported?.imported ?? 0,
        settings: status.settingsApplied ?? false,
        synonyms: status.synonymsImported?.imported ?? 0,
        rules: status.rulesImported?.imported ?? 0,
      },
    };
  }

  if (status.disposition === 'failed' || status.disposition === 'cancelled') {
    return { kind: 'error', status };
  }

  return { kind: 'running', status };
}

export function resolveEffectiveTargetIndex(sourceIndex: string, targetIndex: string): string {
  return targetIndex || sourceIndex;
}

export function orderMigrationPreviewEntries(
  entries: readonly MigrationPreviewReportEntry[],
): MigrationPreviewReportEntry[] {
  return [...entries].sort((left, right) => {
    const leftHard = isHardMigrationPreviewEntry(left);
    const rightHard = isHardMigrationPreviewEntry(right);
    if (leftHard === rightHard) {
      return 0;
    }
    return leftHard ? -1 : 1;
  });
}

export function isHardMigrationPreviewEntry(entry: MigrationPreviewReportEntry): boolean {
  return entry.severity === 'HardRejection';
}

export function formatMigrationPreviewEntryMeta(
  entry: MigrationPreviewReportEntry,
): string | null {
  const parts = [];
  if (entry.pageIndex !== null) {
    parts.push(`page ${entry.pageIndex}`);
  }
  if (entry.itemIndex !== null) {
    parts.push(`item ${entry.itemIndex}`);
  }
  return parts.length > 0 ? parts.join(', ') : null;
}

export function getMigrationErrorMessage(
  error: unknown,
  provider: MigrationProviderDescriptor = MIGRATION_PROVIDER_DESCRIPTORS[0],
  sensitiveValues: readonly string[] = [],
): string {
  if (!error) {
    return 'Unknown error';
  }

  if (axios.isAxiosError<{ message?: string; code?: string }>(error)) {
    const status = error.response?.status;
    const message = error.response?.data?.message;
    const code = error.response?.data?.code;
    const normalizedCode = code?.trim();

    if (typeof message === 'string' && message.trim().length > 0) {
      const trimmedMessage = message.trim();
      const withLoopbackGuidance = addLoopbackGuidance(trimmedMessage, provider);
      if (normalizedCode) {
        return redactSensitiveMigrationDetails(
          `${withLoopbackGuidance} Code: ${normalizedCode}`,
          sensitiveValues,
        );
      }
      return redactSensitiveMigrationDetails(withLoopbackGuidance, sensitiveValues);
    }
    if (status === 409) {
      return 'Target index already exists. Enable "Overwrite if exists" to replace it.';
    }
    if (status === 502) {
      return provider.connectionErrorMessage;
    }
    if (status) {
      return `Server returned ${status}`;
    }
  }

  if (error instanceof Error) {
    return redactSensitiveMigrationDetails(error.message, sensitiveValues);
  }

  return redactSensitiveMigrationDetails(String(error), sensitiveValues);
}

export function getIndexListErrorMessage(
  error: unknown,
  provider: MigrationProviderDescriptor = MIGRATION_PROVIDER_DESCRIPTORS[0],
  sensitiveValues: readonly string[] = [],
): string {
  const message = getMigrationErrorMessage(error, provider, sensitiveValues);
  if (message.includes('403') || message.includes('Forbidden')) {
    return 'API key does not have permission to list indexes. Type the index name manually.';
  }
  return message;
}

export function getTerminalMigrationErrorMessage(
  status: AsyncMigrationStatusResponse,
  provider: MigrationProviderDescriptor,
): string {
  if (status.disposition === 'cancelled') {
    return 'Migration was cancelled before it completed.';
  }
  return provider.terminalFailureMessage;
}

function addLoopbackGuidance(
  message: string,
  provider: MigrationProviderDescriptor,
): string {
  if (
    !provider.loopbackOptInName
    || (
      !message.includes('Cloud endpoint is not allowed')
      && !message.includes('preview loopback endpoint is disabled')
    )
  ) {
    return message;
  }

  return `${message} Set ${provider.loopbackOptInName}=1 to allow local preview fixtures.`;
}

function redactSensitiveMigrationDetails(
  message: string,
  sensitiveValues: readonly string[],
): string {
  let redactedMessage = message.replace(/https?:\/\/[^\s"'<>),]+/gi, '[redacted URL]');
  for (const sensitiveValue of sensitiveValues) {
    const trimmedValue = sensitiveValue.trim();
    if (trimmedValue) {
      redactedMessage = redactedMessage.split(trimmedValue).join('[redacted]');
    }
  }
  return redactedMessage;
}
