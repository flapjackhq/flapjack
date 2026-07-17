import axios from 'axios';
import { useAuth } from '@/hooks/useAuth';

export interface MigrationResult {
  status: string;
  settings: boolean;
  synonyms: { imported: number };
  rules: { imported: number };
  objects: { imported: number };
  taskID: number;
}

export interface AlgoliaIndexInfo {
  name: string;
  entries: number;
  updatedAt: string;
}

interface BuildMigrationRequestBodyInput {
  appId: string;
  apiKey: string;
  sourceIndex: string;
  targetIndex: string;
  overwrite: boolean;
}

export function buildDashboardAuthHeaders(): Record<string, string> {
  const { apiKey, appId } = useAuth.getState();
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    'x-algolia-application-id': appId || 'flapjack',
  };

  if (apiKey) {
    headers['x-algolia-api-key'] = apiKey;
  }

  return headers;
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

export function buildMigrationRequestBody({
  appId,
  apiKey,
  sourceIndex,
  targetIndex,
  overwrite,
}: BuildMigrationRequestBodyInput): Record<string, unknown> {
  const body: Record<string, unknown> = {
    appId,
    apiKey,
    sourceIndex,
  };

  if (targetIndex) {
    body.targetIndex = targetIndex;
  }
  if (overwrite) {
    body.overwrite = true;
  }

  return body;
}

export function resolveEffectiveTargetIndex(sourceIndex: string, targetIndex: string): string {
  return targetIndex || sourceIndex;
}

export function getMigrationErrorMessage(error: unknown): string {
  if (!error) {
    return 'Unknown error';
  }

  if (axios.isAxiosError<{ message?: string; code?: string }>(error)) {
    const status = error.response?.status;
    const message = error.response?.data?.message;
    const code = error.response?.data?.code;
    const normalizedCode = code?.trim();

    if (typeof message === 'string' && message.trim().length > 0) {
      if (normalizedCode) {
        return `${message} Code: ${normalizedCode}`;
      }
      return message;
    }
    if (status === 409) {
      return 'Target index already exists. Enable "Overwrite if exists" to replace it.';
    }
    if (status === 502) {
      return 'Could not connect to Algolia. Check your App ID and API Key.';
    }
    if (status) {
      return `Server returned ${status}`;
    }
  }

  if (error instanceof Error) {
    return error.message;
  }

  return String(error);
}

export function getIndexListErrorMessage(error: unknown): string {
  const message = getMigrationErrorMessage(error);
  if (message.includes('403') || message.includes('Forbidden')) {
    return 'API key does not have permission to list indexes. Type the index name manually.';
  }
  return message;
}
