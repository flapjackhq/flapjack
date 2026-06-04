import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test, expect } from '../fixtures/auth.fixture';
import {
  assertDashboardRouteCoverage,
  buildDashboardRouteAudit,
  EXCLUDED_DASHBOARD_ROUTES,
} from './route_audit_manifest';
import {
  MOVIES,
  MOVIES_INDEX,
  resolveLaneCBundleDir,
  seedMoviesIndex,
  validateMovieCorpus,
} from '../fixtures/lane_c_movies';

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
