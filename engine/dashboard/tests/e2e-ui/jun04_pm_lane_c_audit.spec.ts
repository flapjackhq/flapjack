import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import type { APIRequestContext } from '@playwright/test';
import { test, expect } from '../fixtures/auth.fixture';
import {
  assertDashboardRouteCoverage,
  buildDashboardRouteAudit,
  EXCLUDED_DASHBOARD_ROUTES,
} from './route_audit_manifest';
import {
  MOVIES,
  MOVIES_DOCUMENT_ACTIONS_INDEX,
  MOVIES_INDEX,
  resolveLaneCBundleDir,
  seedMoviesDocumentActionsIndex,
  seedMoviesIndex,
  validateMovieCorpus,
} from '../fixtures/lane_c_movies';

type MockApiResponseBody = Record<string, unknown>;

const MOCK_BUNDLE_BASELINE_NAME = 'jun98_am_lane_c_baseline';
const MOCK_BUNDLE_TIMESTAMP = '20260605T000000Z';
const MOVIES_SEED_VERIFY_FILE = 'movies_seed_verify.json';

function buildMockApiResponse(body: MockApiResponseBody): {
  json: () => Promise<MockApiResponseBody>;
  ok: () => boolean;
  status: () => number;
  text: () => Promise<string>;
} {
  return {
    json: async () => body,
    ok: () => true,
    status: () => 200,
    text: async () => JSON.stringify(body),
  };
}

function buildMovieSeedRequest(initialHits: readonly Record<string, unknown>[]): {
  addedDocuments: () => readonly Record<string, unknown>[];
  deleteCount: () => number;
  request: APIRequestContext;
} {
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
  } as APIRequestContext;

  return {
    addedDocuments: () => addedDocuments,
    deleteCount: () => deleteCount,
    request,
  };
}

function createRepoLaneCBundleDir(baselineName: string, bundleName: string): string {
  const repoLiveStateRoot = path.resolve(process.cwd(), '../../docs/live-state');
  const bundleDir = path.join(repoLiveStateRoot, baselineName, bundleName);
  fs.mkdirSync(bundleDir, { recursive: true });
  return bundleDir;
}

async function withTemporaryLaneCBundleDir<T>(
  bundleDir: string,
  task: () => Promise<T>,
): Promise<T> {
  const originalBundleDir = process.env.LANE_C_BUNDLE_DIR;
  process.env.LANE_C_BUNDLE_DIR = bundleDir;

  try {
    return await task();
  } finally {
    if (originalBundleDir === undefined) {
      delete process.env.LANE_C_BUNDLE_DIR;
    } else {
      process.env.LANE_C_BUNDLE_DIR = originalBundleDir;
    }
  }
}

function removeRepoLaneCBundleDir(bundleDir: string): void {
  fs.rmSync(bundleDir, { recursive: true, force: true });
  try {
    fs.rmdirSync(path.dirname(bundleDir));
  } catch {
    // Other bundle-dir safety tests may still be using the shared fake baseline root.
  }
}

test.describe('Lane C bundle dir safety', () => {
  test('keeps verification output inside the baseline root', () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'lane-c-bundle-root-'));
    const baselineRoot = path.join(tmpRoot, 'baseline');
    const bundleDir = path.join(baselineRoot, '20260604T191244Z');
    fs.mkdirSync(bundleDir, { recursive: true });

    expect(resolveLaneCBundleDir(bundleDir, baselineRoot)).toBe(fs.realpathSync.native(bundleDir));
  });

  test('rejects bundle dirs outside the baseline root', () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'lane-c-bundle-root-'));
    const baselineRoot = path.join(tmpRoot, 'baseline');
    const outsideDir = path.join(tmpRoot, 'outside');
    fs.mkdirSync(baselineRoot, { recursive: true });
    fs.mkdirSync(outsideDir, { recursive: true });

    expect(() => resolveLaneCBundleDir(outsideDir, baselineRoot)).toThrow(
      /LANE_C_BUNDLE_DIR must stay inside/,
    );
  });

  test('rejects bundle dirs that traverse a symlinked segment', () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'lane-c-bundle-root-'));
    const baselineRoot = path.join(tmpRoot, 'baseline');
    const outsideDir = path.join(tmpRoot, 'outside');
    const linkPath = path.join(baselineRoot, 'latest');
    fs.mkdirSync(baselineRoot, { recursive: true });
    fs.mkdirSync(outsideDir, { recursive: true });
    fs.symlinkSync(outsideDir, linkPath, 'dir');

    expect(() => resolveLaneCBundleDir(linkPath, baselineRoot)).toThrow(
      /must not traverse symlinks/,
    );
  });

  test('rejects bundle dirs whose baseline root is itself a symlink', () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'lane-c-bundle-root-'));
    const realBaselineRoot = path.join(tmpRoot, 'real-baseline');
    const symlinkedBaselineRoot = path.join(tmpRoot, 'baseline-link');
    const bundleDir = path.join(symlinkedBaselineRoot, '20260604T191244Z');
    fs.mkdirSync(path.join(realBaselineRoot, '20260604T191244Z'), { recursive: true });
    fs.symlinkSync(realBaselineRoot, symlinkedBaselineRoot, 'dir');

    expect(() => resolveLaneCBundleDir(bundleDir, symlinkedBaselineRoot)).toThrow(
      /baseline root must not be a symlink/,
    );
  });

  test('allows later Lane C live-state bundle roots without a per-spec fixture clone', () => {
    const repoLiveStateRoot = path.resolve(process.cwd(), '../../docs/live-state');
    const baselineRoot = path.join(repoLiveStateRoot, 'jun99_am_lane_c_baseline');
    const bundleDir = path.join(baselineRoot, '20260605T000000Z');

    try {
      fs.mkdirSync(bundleDir, { recursive: true });

      expect(resolveLaneCBundleDir(bundleDir)).toBe(fs.realpathSync.native(bundleDir));
    } finally {
      fs.rmSync(baselineRoot, { recursive: true, force: true });
    }
  });

  test('rejects unrelated live-state bundle roots when no explicit root is supplied', () => {
    const repoLiveStateRoot = path.resolve(process.cwd(), '../../docs/live-state');
    const baselineRoot = path.join(repoLiveStateRoot, 'jun99_am_other_lane_baseline');
    const bundleDir = path.join(baselineRoot, '20260605T000000Z');

    try {
      fs.mkdirSync(bundleDir, { recursive: true });

      expect(() => resolveLaneCBundleDir(bundleDir)).toThrow(/LANE_C_BUNDLE_DIR must stay inside/);
    } finally {
      fs.rmSync(baselineRoot, { recursive: true, force: true });
    }
  });

  test('reseeds the shared movie index when an existing in-set corpus has drifted', async () => {
    const driftedMovies = MOVIES.map((movie, index) => (
      index === 0 ? { ...movie, title: 'Drifted Movie Title' } : { ...movie }
    ));
    const mockBackend = buildMovieSeedRequest(driftedMovies);
    const activeBundleDir = createRepoLaneCBundleDir(
      MOCK_BUNDLE_BASELINE_NAME,
      `${MOCK_BUNDLE_TIMESTAMP}_active_${process.pid}`,
    );
    const isolatedBundleDir = createRepoLaneCBundleDir(
      MOCK_BUNDLE_BASELINE_NAME,
      `${MOCK_BUNDLE_TIMESTAMP}_isolated_${process.pid}`,
    );
    const activeVerificationPath = path.join(activeBundleDir, MOVIES_SEED_VERIFY_FILE);
    const isolatedVerificationPath = path.join(isolatedBundleDir, MOVIES_SEED_VERIFY_FILE);
    const activeVerificationContents = '{"source":"real-backend"}\n';
    fs.writeFileSync(activeVerificationPath, activeVerificationContents, 'utf8');

    try {
      const response = await withTemporaryLaneCBundleDir(activeBundleDir, () => (
        withTemporaryLaneCBundleDir(isolatedBundleDir, () => seedMoviesIndex(mockBackend.request))
      ));

      expect(mockBackend.deleteCount()).toBe(1);
      expect(mockBackend.addedDocuments()).toEqual([...MOVIES]);
      expect(response.hits).toEqual([...MOVIES]);
      expect(fs.readFileSync(activeVerificationPath, 'utf8')).toBe(activeVerificationContents);
      expect(fs.existsSync(isolatedVerificationPath)).toBe(true);
    } finally {
      removeRepoLaneCBundleDir(activeBundleDir);
      removeRepoLaneCBundleDir(isolatedBundleDir);
    }
  });

  test('document action seed reuses the shared movie index without overview-visible spillover', async () => {
    const mockBackend = buildMovieSeedRequest([...MOVIES]);
    const isolatedBundleDir = createRepoLaneCBundleDir(
      MOCK_BUNDLE_BASELINE_NAME,
      `${MOCK_BUNDLE_TIMESTAMP}_document_actions_${process.pid}`,
    );

    try {
      const response = await withTemporaryLaneCBundleDir(isolatedBundleDir, () => (
        seedMoviesDocumentActionsIndex(mockBackend.request)
      ));

      expect(MOVIES_DOCUMENT_ACTIONS_INDEX).toBe(MOVIES_INDEX);
      expect(mockBackend.deleteCount()).toBe(0);
      expect(mockBackend.addedDocuments()).toEqual([]);
      expect(response.hits).toEqual([...MOVIES]);
    } finally {
      removeRepoLaneCBundleDir(isolatedBundleDir);
    }
  });
});

test.describe('Lane C Stage 1 route audit baseline', () => {
  test.describe.configure({ mode: 'serial' });

  test.beforeAll(async ({ request }) => {
    await seedMoviesIndex(request);
  });

  test('movie corpus is exactly 50 unique stable documents', () => {
    expect(() => validateMovieCorpus(MOVIES)).not.toThrow();
    expect(MOVIES).toHaveLength(50);
  });

  test('route manifest exactly covers App user-facing routes', () => {
    const routes = buildDashboardRouteAudit(MOVIES_INDEX);

    assertDashboardRouteCoverage(routes);
    expect(EXCLUDED_DASHBOARD_ROUTES).toEqual([
      {
        appPath: '*',
        reason: 'fallback_shell',
        detail: 'App wildcard is a fallback-only not-found shell, not an authenticated dashboard surface.',
      },
      {
        appPath: '/experiments/:experimentId',
        reason: 'requires_runtime_experiment_fixture',
        detail: 'Detail coverage depends on runtime-created experiment IDs until a deterministic fixture is promoted.',
      },
    ]);
  });

  for (const route of buildDashboardRouteAudit(MOVIES_INDEX)) {
    test(`${route.path} reaches a stable ready state`, async ({ page }) => {
      await page.goto(route.path);
      await route.waitForReady(page);
    });
  }
});
