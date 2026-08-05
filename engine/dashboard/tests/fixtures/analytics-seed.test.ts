import type { APIRequestContext, APIResponse } from '@playwright/test';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  buildAnalyticsSeedPayload,
  clearAnalytics,
  DEFAULT_ANALYTICS_CONFIG,
  seedAnalytics,
  type AnalyticsSeedConfig,
} from './analytics-seed';

function apiResponse(
  ok: boolean,
  status: number,
  body: Record<string, unknown> = {},
): APIResponse {
  return {
    ok: () => ok,
    status: () => status,
    json: async () => body,
  } as APIResponse;
}

afterEach(() => {
  vi.useRealTimers();
});

describe('analytics seed fixture contract', () => {
  it('forwards the configured analytics volume and distributions to the seed endpoint', () => {
    const config: AnalyticsSeedConfig = {
      ...DEFAULT_ANALYTICS_CONFIG,
      indexName: 'analytics-contract',
      days: 14,
      documentCount: 3,
      searchCount: 120,
      noResultRate: 0.25,
      deviceDistribution: { desktop: 0.2, mobile: 0.7, tablet: 0.1 },
      countryDistribution: { US: 0.7, DE: 0.2, GB: 0.1 },
    };

    expect(buildAnalyticsSeedPayload(config)).toEqual({
      index: 'analytics-contract',
      days: 14,
      searchCount: 120,
      noResultRate: 0.25,
      deviceDistribution: { desktop: 0.2, mobile: 0.7, tablet: 0.1 },
      countryDistribution: { US: 0.7, DE: 0.2, GB: 0.1 },
    });
  });

  it('stops immediately when document seeding fails', async () => {
    const request = {
      delete: vi.fn().mockResolvedValue(apiResponse(true, 200)),
      post: vi.fn().mockResolvedValue(apiResponse(false, 503)),
    } as unknown as APIRequestContext;

    await expect(seedAnalytics(request)).rejects.toThrow(
      'Seeding documents for analytics-test failed with status 503',
    );
    expect(request.post).toHaveBeenCalledTimes(1);
  });

  it('rejects a seed response that does not honor the requested search count', async () => {
    vi.useFakeTimers();
    const post = vi.fn()
      .mockResolvedValueOnce(apiResponse(true, 200))
      .mockResolvedValueOnce(apiResponse(true, 200, { totalSearches: 19 }));
    const request = {
      delete: vi.fn().mockResolvedValue(apiResponse(true, 200)),
      post,
    } as unknown as APIRequestContext;
    const assertion = expect(seedAnalytics(request, {
      ...DEFAULT_ANALYTICS_CONFIG,
      searchCount: 20,
    })).rejects.toThrow('Analytics seed returned 19 searches; expected 20');

    await vi.advanceTimersByTimeAsync(2_000);
    await assertion;
    expect(post).toHaveBeenCalledTimes(2);
  });

  it('verifies the seeded search count for only the configured index', async () => {
    vi.useFakeTimers();
    const deleteRequest = vi.fn().mockResolvedValue(apiResponse(true, 200));
    const post = vi.fn()
      .mockResolvedValueOnce(apiResponse(true, 200))
      .mockResolvedValueOnce(apiResponse(true, 200, { totalSearches: 20 }));
    const get = vi.fn().mockResolvedValue(apiResponse(true, 200, { totalSearches: 37 }));
    const request = { delete: deleteRequest, get, post } as unknown as APIRequestContext;

    const assertion = expect(seedAnalytics(request, {
      ...DEFAULT_ANALYTICS_CONFIG,
      indexName: 'analytics-stage-owned',
      documentCount: 2,
      searchCount: 20,
    })).rejects.toThrow(
      'Analytics verification for analytics-stage-owned found 37 searches; expected 20',
    );

    await vi.advanceTimersByTimeAsync(2_000);
    await vi.advanceTimersByTimeAsync(3_000);
    await assertion;
    expect(get).toHaveBeenCalledWith(
      expect.stringMatching(/\/2\/overview$/),
      expect.objectContaining({
        params: expect.objectContaining({
          index: 'analytics-stage-owned',
          startDate: expect.stringMatching(/^\d{4}-\d{2}-\d{2}$/),
          endDate: expect.stringMatching(/^\d{4}-\d{2}-\d{2}$/),
        }),
      }),
    );
  });

  it('clears stale analytics for the configured index before adding new seed data', async () => {
    vi.useFakeTimers();
    const deleteRequest = vi.fn().mockResolvedValue(apiResponse(true, 200));
    const post = vi.fn()
      .mockResolvedValueOnce(apiResponse(true, 200))
      .mockResolvedValueOnce(apiResponse(true, 200, { totalSearches: 20 }));
    const get = vi.fn().mockResolvedValue(apiResponse(true, 200, { totalSearches: 20 }));
    const request = { delete: deleteRequest, get, post } as unknown as APIRequestContext;

    const result = seedAnalytics(request, {
      ...DEFAULT_ANALYTICS_CONFIG,
      indexName: 'analytics-stage-owned',
      documentCount: 2,
      searchCount: 20,
    });

    await vi.advanceTimersByTimeAsync(2_000);
    await vi.advanceTimersByTimeAsync(3_000);
    await result;
    expect(deleteRequest).toHaveBeenCalledWith(
      expect.stringMatching(/\/2\/analytics\/clear$/),
      expect.objectContaining({
        data: { index: 'analytics-stage-owned' },
        headers: expect.any(Object),
      }),
    );
    expect(deleteRequest.mock.invocationCallOrder[0]).toBeLessThan(
      post.mock.invocationCallOrder[0],
    );
  });

  it('sends the clear index in the JSON body and rejects a failed clear', async () => {
    const deleteRequest = vi.fn().mockResolvedValue(apiResponse(false, 500));
    const request = { delete: deleteRequest } as unknown as APIRequestContext;

    await expect(clearAnalytics(request, 'analytics-contract')).rejects.toThrow(
      'Clearing analytics for analytics-contract failed with status 500',
    );
    expect(deleteRequest).toHaveBeenCalledWith(
      expect.stringMatching(/\/2\/analytics\/clear$/),
      expect.objectContaining({ data: { index: 'analytics-contract' } }),
    );
  });
});
