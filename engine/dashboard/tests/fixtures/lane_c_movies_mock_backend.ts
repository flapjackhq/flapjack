/**
 * In-memory stand-in for the search API that the Lane C movie seeder talks to.
 *
 * Extracted from `tests/e2e-ui/jun04_pm_lane_c_audit.spec.ts` so the seeder's
 * evidence-write contract can also be asserted from the Vitest unit suite without a
 * second copy of the mock drifting away from the original. Two consumers today:
 * that spec, and `tests/fixtures/lane_c_movies.test.ts`.
 */
import type { APIRequestContext } from '@playwright/test';

type MockApiResponseBody = Record<string, unknown>;

type MockApiResponse = {
  json: () => Promise<MockApiResponseBody>;
  ok: () => boolean;
  status: () => number;
  text: () => Promise<string>;
};

export type MovieSeedMockBackend = {
  addedDocuments: () => readonly Record<string, unknown>[];
  deleteCount: () => number;
  request: APIRequestContext;
};

function buildMockApiResponse(body: MockApiResponseBody): MockApiResponse {
  return {
    json: async () => body,
    ok: () => true,
    status: () => 200,
    text: async () => JSON.stringify(body),
  };
}

/**
 * Build a mock request context seeded with `initialHits`.
 *
 * Passing the full corpus exercises the seeder's reuse path (no delete, no batch);
 * passing a drifted or empty corpus exercises the reseed path.
 */
export function buildMovieSeedRequest(
  initialHits: readonly Record<string, unknown>[],
): MovieSeedMockBackend {
  let storedDocuments = [...initialHits];
  let addedDocuments: Record<string, unknown>[] = [];
  let deleteCount = 0;

  const request = {
    delete: async () => {
      deleteCount += 1;
      storedDocuments = [];
      return buildMockApiResponse({});
    },
    post: async (url: string, options?: { data?: unknown }) => {
      if (url.endsWith('/query')) {
        return buildMockApiResponse({ hits: storedDocuments, nbHits: storedDocuments.length });
      }

      if (url.endsWith('/batch')) {
        const data = options?.data as { requests?: Array<{ body?: Record<string, unknown> }> };
        addedDocuments = data.requests?.map((entry) => entry.body ?? {}) ?? [];
        storedDocuments = [...addedDocuments];
        return buildMockApiResponse({});
      }

      throw new Error(`Unexpected mock request URL: ${url}`);
    },
  } as unknown as APIRequestContext;

  return {
    addedDocuments: () => addedDocuments,
    deleteCount: () => deleteCount,
    request,
  };
}
