import { render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  API_LOG_REDACTED_VALUE,
  sanitizeApiLogEntry,
  sanitizePersistedApiLoggerStorageValue,
  useApiLogger,
  type ApiLogEntry,
} from '@/hooks/useApiLogger';
import { ApiLogger } from './ApiLogger';

vi.mock('@/hooks/useApiLogger', async () => {
  const actual = await vi.importActual<typeof import('@/hooks/useApiLogger')>('@/hooks/useApiLogger');
  return {
    ...actual,
    useApiLogger: vi.fn(),
  };
});

const LONG_REQUEST_URL = '/2/overview?startDate=2026-07-27&endDate=2026-08-03&segment=all-products';

const LAST_ENTRY: ApiLogEntry = {
  id: 'request-1',
  timestamp: 1_722_684_800_000,
  method: 'GET',
  url: LONG_REQUEST_URL,
  headers: {},
  duration: 125,
  status: 'success',
};

function mockLoggerState(isExpanded: boolean, entries: ApiLogEntry[] = [LAST_ENTRY]) {
  vi.mocked(useApiLogger).mockReturnValue({
    entries,
    maxEntries: 20,
    isExpanded,
    addEntry: vi.fn(),
    updateEntry: vi.fn(),
    clear: vi.fn(),
    toggleExpanded: vi.fn(),
    exportAsBash: vi.fn(),
    exportAsFile: vi.fn(),
  });
}

describe('ApiLogger', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('lets the collapsed request summary shrink without displacing Export or Clear', () => {
    mockLoggerState(false);

    render(<ApiLogger />);

    expect(screen.getByTestId('app-shell-api-logger')).toHaveClass('min-w-0', 'max-w-full');

    const primaryRegion = screen.getByTestId('api-logger-primary');
    expect(primaryRegion).toHaveClass('min-w-0', 'flex-1');

    const expectedSummary = `Last: GET ${LONG_REQUEST_URL} - 125ms`;
    const summary = screen.getByTestId('api-logger-summary');
    expect(summary).toHaveClass('min-w-0', 'flex-1', 'truncate');
    expect(summary).toHaveAttribute('title', expectedSummary);
    expect(summary).toHaveAttribute('aria-label', expectedSummary);

    const actions = screen.getByTestId('api-logger-actions');
    expect(actions).toHaveClass('shrink-0');
    expect(screen.getByRole('button', { name: 'Export' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'Clear' })).toBeVisible();
  });

  it('owns scrolling for expanded request content inside the logger panel', () => {
    mockLoggerState(true);

    render(<ApiLogger />);

    const entries = screen.getByTestId('api-logger-entries');
    expect(entries).toHaveClass('min-w-0', 'overflow-auto');
    expect(screen.getByText(LONG_REQUEST_URL, { exact: false })).toHaveClass('min-w-0', 'break-all');
  });

  it('disables Export and Clear when there are no entries', () => {
    mockLoggerState(false, []);

    render(<ApiLogger />);

    expect(screen.queryByTestId('api-logger-summary')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Export' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Clear' })).toBeDisabled();
  });

  it('redacts credentials before persisting or exporting log entries', () => {
    const sanitized = sanitizeApiLogEntry({
      ...LAST_ENTRY,
      url: '/1/keys/live-admin-key?token=live-token&view=full',
      headers: {
        authorization: 'Bearer live-token',
        'x-algolia-api-key': 'live-admin-key',
        'x-request-id': 'req_123',
      },
      body: {
        apiKey: 'source-admin-key',
        nested: {
          password: 'top-secret-password',
          keep: 'visible',
        },
      },
      response: {
        token: 'response-token',
        status: 'ok',
      },
    });

    expect(sanitized.url).toBe(`/1/keys/${API_LOG_REDACTED_VALUE}?token=${encodeURIComponent(API_LOG_REDACTED_VALUE)}&view=full`);
    expect(sanitized.headers).toEqual({
      authorization: API_LOG_REDACTED_VALUE,
      'x-algolia-api-key': API_LOG_REDACTED_VALUE,
      'x-request-id': 'req_123',
    });
    expect(sanitized.body).toEqual({
      apiKey: API_LOG_REDACTED_VALUE,
      nested: {
        password: API_LOG_REDACTED_VALUE,
        keep: 'visible',
      },
    });
    expect(sanitized.response).toEqual({
      token: API_LOG_REDACTED_VALUE,
      status: 'ok',
    });
  });

  it('scrubs legacy persisted entries loaded from session storage', () => {
    const persisted = sanitizePersistedApiLoggerStorageValue({
      state: {
        entries: [
          {
            ...LAST_ENTRY,
            url: '/1/keys/live-admin-key',
            headers: { cookie: 'session=plain-text-secret' },
            body: { credentials: 'plain-text-secret' },
          },
        ],
        isExpanded: false,
        maxEntries: 20,
      },
      version: 0,
    }) as {
      state: {
        entries: ApiLogEntry[];
      };
    };

    expect(persisted.state.entries[0]).toMatchObject({
      url: `/1/keys/${API_LOG_REDACTED_VALUE}`,
      headers: { cookie: API_LOG_REDACTED_VALUE },
      body: { credentials: API_LOG_REDACTED_VALUE },
    });
  });
});
