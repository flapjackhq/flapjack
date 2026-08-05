import type { APIRequestContext, APIResponse } from '@playwright/test';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { API_BASE, API_HEADERS } from './local-instance';
import {
  seedRouteAuditExperiment,
  STAGE_1_ROUTE_AUDIT_EXPERIMENT_NAME_PREFIX,
} from './experiment-seed';

const FROZEN_TIME = new Date('2026-08-03T12:00:00.000Z');

function expectedFixtureNamePattern(): RegExp {
  return new RegExp(
    `^${STAGE_1_ROUTE_AUDIT_EXPERIMENT_NAME_PREFIX}-${FROZEN_TIME.getTime()}-${process.pid}-\\d+$`,
  );
}

function apiResponse(
  ok: boolean,
  status: number,
  body: Record<string, unknown> = {},
): APIResponse {
  return {
    ok: () => ok,
    status: () => status,
    json: async () => body,
    text: async () => JSON.stringify(body),
  } as APIResponse;
}

afterEach(() => {
  vi.useRealTimers();
});

describe('route audit experiment seed fixture', () => {
  it('creates and verifies a collision-safe route audit experiment identity', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(FROZEN_TIME);
    const expectedNamePattern = expectedFixtureNamePattern();

    const get = vi.fn()
      .mockResolvedValueOnce(apiResponse(true, 200, { abtests: [] }))
      .mockImplementationOnce(async () => apiResponse(true, 200, {
        abTestID: 731,
        name: String(post.mock.calls[0]?.[1]?.data?.name),
        status: 'draft',
      }));
    const deleteRequest = vi.fn().mockResolvedValue(apiResponse(true, 200));
    const post = vi.fn().mockResolvedValue(apiResponse(true, 200, {
      abTestID: 731,
      index: 'stage_1_route_audit_products',
      taskID: 731,
    }));
    const request = { delete: deleteRequest, get, post } as unknown as APIRequestContext;

    const experiment = await seedRouteAuditExperiment(request);

    expect(experiment).toEqual({
      id: '731',
      name: expect.stringMatching(expectedNamePattern),
      indexName: 'stage_1_route_audit_products',
      status: 'draft',
      primaryMetricLabel: 'CTR',
    });

    const createdName = experiment.name;
    expect(post).toHaveBeenCalledWith(`${API_BASE}/2/abtests`, {
      headers: API_HEADERS,
      data: {
        name: createdName,
        variants: [
          {
            index: 'stage_1_route_audit_products',
            trafficPercentage: 50,
            description: 'Route audit control',
          },
          {
            index: 'stage_1_route_audit_products',
            trafficPercentage: 50,
            description: 'Route audit variant',
            customSearchParameters: { typoTolerance: false },
          },
        ],
        endAt: '2026-08-10T12:00:00.000Z',
        metrics: [{ name: 'clickThroughRate' }],
      },
    });
    expect(deleteRequest).not.toHaveBeenCalled();
    expect(get).toHaveBeenCalledTimes(2);
    expect(get).toHaveBeenNthCalledWith(1, `${API_BASE}/2/abtests`, {
      headers: API_HEADERS,
    });
    expect(get).toHaveBeenNthCalledWith(2, `${API_BASE}/2/abtests/731`, {
      headers: API_HEADERS,
    });
    expect(get.mock.invocationCallOrder[0]).toBeLessThan(
      post.mock.invocationCallOrder[0],
    );
    expect(post.mock.invocationCallOrder[0]).toBeLessThan(
      get.mock.invocationCallOrder[1],
    );
  });

  it('does not delete another live seeded route audit experiment during concurrent setup', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(FROZEN_TIME);

    const get = vi.fn()
      .mockResolvedValueOnce(apiResponse(true, 200, { abtests: [] }))
      .mockResolvedValueOnce(apiResponse(true, 200, { abtests: [] }))
      .mockImplementationOnce(async () => apiResponse(true, 200, {
        abTestID: 801,
        name: String(post.mock.calls[0]?.[1]?.data?.name),
        status: 'draft',
      }))
      .mockImplementationOnce(async () => apiResponse(true, 200, {
        abTestID: 802,
        name: String(post.mock.calls[1]?.[1]?.data?.name),
        status: 'draft',
      }));
    const post = vi.fn()
      .mockResolvedValueOnce(apiResponse(true, 200, {
        abTestID: 801,
        index: 'stage_1_route_audit_products',
        taskID: 801,
      }))
      .mockResolvedValueOnce(apiResponse(true, 200, {
        abTestID: 802,
        index: 'stage_1_route_audit_products',
        taskID: 802,
      }));
    const deleteRequest = vi.fn().mockResolvedValue(apiResponse(true, 200));
    const request = { delete: deleteRequest, get, post } as unknown as APIRequestContext;

    const seeded = await Promise.all([
      seedRouteAuditExperiment(request),
      seedRouteAuditExperiment(request),
    ]);

    const names = seeded.map((experiment) => experiment.name);
    expect(names).toEqual([
      expect.stringMatching(expectedFixtureNamePattern()),
      expect.stringMatching(expectedFixtureNamePattern()),
    ]);
    expect(new Set(names).size).toBe(2);
    expect(seeded.map((experiment) => experiment.id)).toEqual(['801', '802']);
    expect(deleteRequest).not.toHaveBeenCalled();
  });

  it('rejects when the runtime experiment id is not ready by id', async () => {
    const get = vi.fn()
      .mockResolvedValueOnce(apiResponse(true, 200, { abtests: [] }))
      .mockResolvedValueOnce(apiResponse(false, 404));
    const deleteRequest = vi.fn().mockResolvedValue(apiResponse(true, 200));
    const request = {
      delete: deleteRequest,
      get,
      post: vi.fn().mockResolvedValue(apiResponse(true, 200, {
        abTestID: 842,
        index: 'stage_1_route_audit_products',
        taskID: 842,
      })),
    } as unknown as APIRequestContext;

    await expect(seedRouteAuditExperiment(request)).rejects.toThrow(
      /Route audit experiment 842 is not ready with expected name "stage-1-route-audit-experiment-/,
    );
    expect(deleteRequest).toHaveBeenCalledWith(`${API_BASE}/2/abtests/842`, {
      headers: API_HEADERS,
    });
  });

  it('rejects when the by-id record carries the wrong name', async () => {
    const get = vi.fn()
      .mockResolvedValueOnce(apiResponse(true, 200, { abtests: [] }))
      .mockResolvedValueOnce(apiResponse(true, 200, {
        abTestID: 842,
        name: 'some-other-experiment',
        status: 'draft',
      }));
    const deleteRequest = vi.fn().mockResolvedValue(apiResponse(true, 200));
    const request = {
      delete: deleteRequest,
      get,
      post: vi.fn().mockResolvedValue(apiResponse(true, 200, {
        abTestID: 842,
        index: 'stage_1_route_audit_products',
        taskID: 842,
      })),
    } as unknown as APIRequestContext;

    await expect(seedRouteAuditExperiment(request)).rejects.toThrow(
      /Route audit experiment 842 is not ready with expected name "stage-1-route-audit-experiment-/,
    );
  });

  it('reclaims stale interrupted route audit fixtures without deleting live owners', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(FROZEN_TIME);

    const staleTimestamp = FROZEN_TIME.getTime() - 3_600_001;
    const liveTimestamp = FROZEN_TIME.getTime() - 59_999;
    const staleName = `${STAGE_1_ROUTE_AUDIT_EXPERIMENT_NAME_PREFIX}-${staleTimestamp}-777-1`;
    const liveName = `${STAGE_1_ROUTE_AUDIT_EXPERIMENT_NAME_PREFIX}-${liveTimestamp}-888-1`;
    const post = vi.fn().mockResolvedValue(apiResponse(true, 200, {
      abTestID: 901,
      index: 'stage_1_route_audit_products',
      taskID: 901,
    }));
    const deleteRequest = vi.fn().mockResolvedValue(apiResponse(true, 200));
    const request = {
      delete: deleteRequest,
      get: vi.fn()
        .mockResolvedValueOnce(apiResponse(true, 200, {
          abtests: [
            { abTestID: 701, name: staleName, status: 'draft' },
            { abTestID: 702, name: liveName, status: 'draft' },
            { abTestID: 703, name: 'unrelated-experiment', status: 'draft' },
          ],
        }))
        .mockImplementationOnce(async () => apiResponse(true, 200, {
          abTestID: 901,
          name: String(post.mock.calls[0]?.[1]?.data?.name),
          status: 'draft',
        })),
      post,
    } as unknown as APIRequestContext;

    const experiment = await seedRouteAuditExperiment(request);

    expect(experiment.id).toBe('901');
    expect(deleteRequest).toHaveBeenCalledTimes(1);
    expect(deleteRequest).toHaveBeenCalledWith(`${API_BASE}/2/abtests/701`, {
      headers: API_HEADERS,
    });
  });

  it('reclaims stale interrupted route audit fixtures beyond the first list page', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(FROZEN_TIME);

    const staleTimestamp = FROZEN_TIME.getTime() - 3_600_001;
    const liveTimestamp = FROZEN_TIME.getTime() - 59_999;
    const staleFixture = (id: number) => ({
      abTestID: id,
      name: `${STAGE_1_ROUTE_AUDIT_EXPERIMENT_NAME_PREFIX}-${staleTimestamp}-${id}-1`,
      status: 'draft',
    });
    const staleFirstPage = Array.from({ length: 10 }, (_, offset) => staleFixture(1_000 + offset));
    const staleSecondPage = Array.from({ length: 3 }, (_, offset) => staleFixture(2_000 + offset));
    const liveName = `${STAGE_1_ROUTE_AUDIT_EXPERIMENT_NAME_PREFIX}-${liveTimestamp}-3000-1`;
    const post = vi.fn().mockResolvedValue(apiResponse(true, 200, {
      abTestID: 950,
      index: 'stage_1_route_audit_products',
      taskID: 950,
    }));
    const deleteRequest = vi.fn().mockResolvedValue(apiResponse(true, 200));
    const get = vi.fn()
      .mockResolvedValueOnce(apiResponse(true, 200, {
        abtests: staleFirstPage,
        count: 10,
        total: 15,
      }))
      .mockResolvedValueOnce(apiResponse(true, 200, {
        abtests: [
          ...staleSecondPage,
          { abTestID: 3_001, name: liveName, status: 'draft' },
          { abTestID: 3_002, name: 'unrelated-experiment', status: 'draft' },
        ],
        count: 5,
        total: 15,
      }))
      .mockImplementationOnce(async () => apiResponse(true, 200, {
        abTestID: 950,
        name: String(post.mock.calls[0]?.[1]?.data?.name),
        status: 'draft',
      }));
    const request = {
      delete: deleteRequest,
      get,
      post,
    } as unknown as APIRequestContext;

    await seedRouteAuditExperiment(request);

    const deletedIds = deleteRequest.mock.calls.map((call) => String(call[0]));
    expect(deletedIds).toEqual([
      ...staleFirstPage,
      ...staleSecondPage,
    ].map((experiment) => `${API_BASE}/2/abtests/${experiment.abTestID}`));
    expect(deletedIds).not.toContain(`${API_BASE}/2/abtests/3001`);
    expect(deletedIds).not.toContain(`${API_BASE}/2/abtests/3002`);
    expect(get).toHaveBeenNthCalledWith(2, `${API_BASE}/2/abtests`, {
      headers: API_HEADERS,
      params: { offset: 10, limit: 10 },
    });
  });
});
