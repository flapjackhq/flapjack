import { create } from 'zustand';
import { createJSONStorage, persist } from 'zustand/middleware';

export const API_LOG_REDACTED_VALUE = '[REDACTED]';

export interface ApiLogEntry {
  id: string;
  timestamp: number;
  method: string;
  url: string;
  headers: Record<string, string>;
  body?: any;
  response?: any;
  duration: number;
  status: 'pending' | 'success' | 'error';
}

interface ApiLoggerStore {
  entries: ApiLogEntry[];
  maxEntries: number;
  isExpanded: boolean;
  addEntry: (entry: Omit<ApiLogEntry, 'id' | 'timestamp'>) => string;
  updateEntry: (id: string, updates: Partial<ApiLogEntry>) => void;
  clear: () => void;
  toggleExpanded: () => void;
  exportAsBash: () => string;
  exportAsFile: () => void;
}

function isSensitiveFieldName(fieldName: string): boolean {
  const normalized = fieldName.toLowerCase().replace(/[^a-z0-9]/g, '');
  return normalized === 'authorization'
    || normalized === 'cookie'
    || normalized === 'setcookie'
    || normalized.includes('secret')
    || normalized.endsWith('apikey')
    || normalized.endsWith('adminkey')
    || normalized.endsWith('accesskey')
    || normalized.endsWith('privatekey')
    || normalized.endsWith('credential')
    || normalized.endsWith('credentials')
    || normalized.endsWith('password')
    || normalized.endsWith('token');
}

function redactUrl(url: string): string {
  const scrubbedPath = url.replace(/(\/1\/keys\/)[^/?#]+/g, `$1${API_LOG_REDACTED_VALUE}`);

  try {
    const parsed = scrubbedPath.startsWith('http')
      ? new URL(scrubbedPath)
      : new URL(scrubbedPath, 'http://flapjack.local');
    for (const key of [...parsed.searchParams.keys()]) {
      if (isSensitiveFieldName(key)) {
        parsed.searchParams.set(key, API_LOG_REDACTED_VALUE);
      }
    }
    if (scrubbedPath.startsWith('http')) {
      return parsed.toString();
    }
    return `${parsed.pathname}${parsed.search}${parsed.hash}`;
  } catch {
    return scrubbedPath;
  }
}

function redactStructuredValue(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map((item) => redactStructuredValue(item));
  }
  if (!value || typeof value !== 'object') {
    return value;
  }

  return Object.fromEntries(
    Object.entries(value).map(([key, nestedValue]) => [
      key,
      isSensitiveFieldName(key) ? API_LOG_REDACTED_VALUE : redactStructuredValue(nestedValue),
    ]),
  );
}

function redactHeaders(headers: Record<string, string>): Record<string, string> {
  return Object.fromEntries(
    Object.entries(headers).map(([key, value]) => [
      key,
      isSensitiveFieldName(key) ? API_LOG_REDACTED_VALUE : value,
    ]),
  );
}

export function sanitizeApiLogEntry(entry: ApiLogEntry): ApiLogEntry {
  return {
    ...entry,
    url: redactUrl(entry.url),
    headers: redactHeaders(entry.headers),
    body: redactStructuredValue(entry.body),
    response: redactStructuredValue(entry.response),
  };
}

function sanitizeApiLogEntryPartial(updates: Partial<ApiLogEntry>): Partial<ApiLogEntry> {
  return {
    ...updates,
    ...(updates.url ? { url: redactUrl(updates.url) } : {}),
    ...(updates.headers ? { headers: redactHeaders(updates.headers) } : {}),
    ...(Object.prototype.hasOwnProperty.call(updates, 'body')
      ? { body: redactStructuredValue(updates.body) }
      : {}),
    ...(Object.prototype.hasOwnProperty.call(updates, 'response')
      ? { response: redactStructuredValue(updates.response) }
      : {}),
  };
}

export function sanitizePersistedApiLoggerStorageValue(value: unknown): unknown {
  if (!value || typeof value !== 'object' || !('state' in value)) {
    return value;
  }
  const persisted = value as {
    state?: Partial<ApiLoggerStore> & { entries?: ApiLogEntry[] };
  };
  if (!persisted.state || !Array.isArray(persisted.state.entries)) {
    return value;
  }
  return {
    ...persisted,
    state: {
      ...persisted.state,
      entries: persisted.state.entries.map(sanitizeApiLogEntry),
    },
  };
}

export const useApiLogger = create<ApiLoggerStore>()(
  persist(
    (set, get) => ({
      entries: [],
      maxEntries: 20,
      isExpanded: false,

      addEntry: (entry) => {
        const id = crypto.randomUUID();
        const timestamp = Date.now();
        const sanitizedEntry = sanitizeApiLogEntry({
          ...entry,
          id,
          timestamp,
        });
        set((state) => ({
          entries: [
            sanitizedEntry,
            ...state.entries.slice(0, state.maxEntries - 1),
          ],
        }));
        return id;
      },

      updateEntry: (id, updates) => {
        const sanitizedUpdates = sanitizeApiLogEntryPartial(updates);
        set((state) => ({
          entries: state.entries.map((e) =>
            e.id === id ? { ...e, ...sanitizedUpdates } : e
          ),
        }));
      },

      clear: () => set({ entries: [] }),

      toggleExpanded: () => set((state) => ({ isExpanded: !state.isExpanded })),

      exportAsBash: () => {
        const { entries } = get();
        const timestamp = new Date().toISOString();
        const header = `#!/bin/bash\n# Flapjack API Requests - ${timestamp}\n\n`;

        const commands = entries
          .slice()
          .reverse()
          .map((e, i) => {
            const headers = Object.entries(e.headers)
              .filter(([k]) => k !== 'x-request-id') // Exclude internal header
              .map(([k, v]) => `  -H "${k}: ${v}"`)
              .join(' \\\n');
            const body = e.body ? ` \\\n  -d '${JSON.stringify(e.body)}'` : '';
            const fullUrl = e.url.startsWith('http') ? e.url : `${__BACKEND_URL__}${e.url}`;
            return `# ${i + 1}. ${e.method} ${e.url}\ncurl -X ${e.method} ${fullUrl} \\\n${headers}${body}\n`;
          })
          .join('\n');

        return header + commands;
      },

      exportAsFile: () => {
        const bash = get().exportAsBash();
        const blob = new Blob([bash], { type: 'text/plain' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = `flapjack-api-log-${Date.now()}.sh`;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        URL.revokeObjectURL(url);
      },
    }),
    {
      name: 'flapjack-api-log',
      storage: createJSONStorage<ApiLoggerStore>(() => sessionStorage, {
        reviver: (key, value) => (
          key === '' ? sanitizePersistedApiLoggerStorageValue(value) : value
        ),
        replacer: (key, value) => (
          key === '' ? sanitizePersistedApiLoggerStorageValue(value) : value
        ),
      }),
    }
  )
);
