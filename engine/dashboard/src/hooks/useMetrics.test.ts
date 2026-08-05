import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

const { parsePrometheusTextMock } = vi.hoisted(() => ({
  parsePrometheusTextMock: vi.fn(),
}));

vi.mock('@/lib/prometheusParser', () => ({
  parsePrometheusText: parsePrometheusTextMock,
}));

const { authStateRef, useAuthMock } = vi.hoisted(() => {
  const authStateRef = {
    current: {
      appId: 'flapjack',
      isAuthenticated: false,
    },
  };
  const useAuthMock = Object.assign(
    vi.fn((selector?: (state: typeof authStateRef.current) => unknown) =>
      selector ? selector(authStateRef.current) : authStateRef.current
    ),
    {
      getState: vi.fn(() => authStateRef.current),
    }
  );

  return { authStateRef, useAuthMock };
});

vi.mock('@/hooks/useAuth', () => ({
  useAuth: useAuthMock,
}));

import {
  getPerIndexMetrics,
  getSystemMetric,
  usePrometheusMetrics,
} from './useMetrics';

function createWrapper() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
    },
  });

  const wrapper = ({ children }: { children: React.ReactNode }) =>
    React.createElement(QueryClientProvider, { client }, children);

  return { client, wrapper };
}

describe('usePrometheusMetrics', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal('__BACKEND_URL__', 'http://backend.test');
    authStateRef.current = { appId: 'flapjack', isAuthenticated: false };
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('uses the same-origin metrics proxy so authenticated requests carry the session cookie', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: vi.fn().mockResolvedValue('metrics-body'),
    });
    vi.stubGlobal('fetch', fetchMock);
    parsePrometheusTextMock.mockReturnValue([]);
    authStateRef.current = { appId: 'tenant-app', isAuthenticated: true };

    const { wrapper } = createWrapper();
    const { result } = renderHook(() => usePrometheusMetrics(), { wrapper });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(fetchMock).toHaveBeenCalledWith('/__flapjack_metrics', {
      credentials: 'include',
      headers: {
        'x-algolia-application-id': 'tenant-app',
      },
    });
  });

  it('falls back to the default app id without adding secret headers', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: vi.fn().mockResolvedValue('metrics-body'),
    });
    vi.stubGlobal('fetch', fetchMock);
    parsePrometheusTextMock.mockReturnValue([]);
    authStateRef.current = { appId: '', isAuthenticated: false };

    const { wrapper } = createWrapper();
    const { result } = renderHook(() => usePrometheusMetrics(), { wrapper });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(fetchMock).toHaveBeenCalledWith('/__flapjack_metrics', {
      credentials: 'include',
      headers: {
        'x-algolia-application-id': 'flapjack',
      },
    });
  });

  it('treats whitespace-only app ids as blank and falls back to flapjack', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: vi.fn().mockResolvedValue('metrics-body'),
    });
    vi.stubGlobal('fetch', fetchMock);
    parsePrometheusTextMock.mockReturnValue([]);
    authStateRef.current = { appId: '   ', isAuthenticated: true };

    const { wrapper } = createWrapper();
    const { result } = renderHook(() => usePrometheusMetrics(), { wrapper });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(fetchMock).toHaveBeenCalledWith('/__flapjack_metrics', {
      credentials: 'include',
      headers: {
        'x-algolia-application-id': 'flapjack',
      },
    });
  });

  it('refetches metrics when session authentication changes', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: vi.fn().mockResolvedValue('metrics-body'),
    });
    vi.stubGlobal('fetch', fetchMock);
    parsePrometheusTextMock.mockReturnValue([]);

    const { wrapper } = createWrapper();
    const { result, rerender } = renderHook(() => usePrometheusMetrics(), { wrapper });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    authStateRef.current = { appId: 'flapjack', isAuthenticated: true };
    rerender();

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
  });

  it('stores the exact non-secret session scope in the query cache key', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: vi.fn().mockResolvedValue('metrics-body'),
    });
    vi.stubGlobal('fetch', fetchMock);
    parsePrometheusTextMock.mockReturnValue([]);
    authStateRef.current = { appId: 'tenant-app', isAuthenticated: true };

    const { client, wrapper } = createWrapper();
    const { result } = renderHook(() => usePrometheusMetrics(), { wrapper });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    const metricsQuery = client
      .getQueryCache()
      .getAll()
      .find((query) => query.queryKey[0] === 'prometheus-metrics');
    expect(metricsQuery?.queryKey).toEqual([
      'prometheus-metrics',
      'tenant-app',
      'session:authenticated',
    ]);
  });

  it('uses the selector auth snapshot instead of imperative store reads', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      text: vi.fn().mockResolvedValue('metrics-body'),
    });
    vi.stubGlobal('fetch', fetchMock);
    parsePrometheusTextMock.mockReturnValue([]);
    authStateRef.current = { appId: 'selector-app', isAuthenticated: true };
    useAuthMock.getState.mockReturnValue({
      appId: 'imperative-app',
      isAuthenticated: false,
    });

    const { wrapper } = createWrapper();
    const { result } = renderHook(() => usePrometheusMetrics(), { wrapper });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(fetchMock).toHaveBeenCalledWith('/__flapjack_metrics', {
      credentials: 'include',
      headers: {
        'x-algolia-application-id': 'selector-app',
      },
    });
    expect(useAuthMock.getState).not.toHaveBeenCalled();
  });
});

describe('metrics helpers', () => {
  it('groups per-index metrics by short name', () => {
    const grouped = getPerIndexMetrics([
      {
        name: 'flapjack_documents_count',
        labels: { index: 'products' },
        value: 12,
      },
      {
        name: 'flapjack_search_requests_total',
        labels: { index: 'products' },
        value: 8,
      },
      {
        name: 'flapjack_documents_count',
        labels: { index: 'books' },
        value: 4,
      },
    ]);

    expect(grouped.get('products')).toEqual({
      documents_count: 12,
      search_requests_total: 8,
    });
    expect(grouped.get('books')).toEqual({
      documents_count: 4,
    });
  });

  it('returns only unlabeled system metrics', () => {
    const metrics = [
      {
        name: 'flapjack_tenants_loaded',
        labels: { index: 'products' },
        value: 1,
      },
      {
        name: 'flapjack_tenants_loaded',
        labels: {},
        value: 3,
      },
    ];

    expect(getSystemMetric(metrics, 'flapjack_tenants_loaded')).toBe(3);
    expect(getSystemMetric(metrics, 'flapjack_bytes_in_total')).toBeUndefined();
  });
});
