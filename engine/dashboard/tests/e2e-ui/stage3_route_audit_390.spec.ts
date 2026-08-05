import fs from 'node:fs';
import path from 'node:path';
import { test, expect } from '../fixtures/auth.fixture';
import { buildDashboardRouteAudit } from './route_audit_manifest';
import {
  seedRouteAuditExperiment,
  type SeededRouteAuditExperiment,
} from '../fixtures/experiment-seed';
import { deleteExperiment } from '../fixtures/api-helpers';
import { MOVIES_INDEX, seedMoviesIndex } from '../fixtures/lane_c_movies';
import {
  describeHorizontalOverflow,
  isHorizontallyUnusable,
  summarizeHorizontalOverflow,
} from '../fixtures/viewport_overflow';

type RouteUsabilityEvidence = {
  id: string;
  appPath: string;
  path: string;
  status: 'usable' | 'unusable';
  issue: string;
  /** Measured widths and offending selectors, so a red row names its own culprit. */
  detail: string;
  /** Raw oracle signals persisted so evidence consistency can be checked independently. */
  documentOverflow: boolean | null;
  culpritCount: number | null;
};

const VIEWPORT_WIDTH = 390;
const VIEWPORT_HEIGHT = 844;
const HORIZONTAL_OVERFLOW_ISSUE = 'horizontal overflow at 390px viewport';

function resolveAuditOutputPath(): string {
  return process.env.STAGE3_ROUTE_AUDIT_OUTPUT
    ?? path.resolve(process.cwd(), 'test-results', 'stage3_route_audit_390.json');
}

test.describe('Stage 3 390px route audit', () => {
  test.describe.configure({ mode: 'serial' });
  test.use({ viewport: { width: VIEWPORT_WIDTH, height: VIEWPORT_HEIGHT } });
  let routeAuditExperiment: SeededRouteAuditExperiment;

  test.beforeAll(async ({ request }) => {
    await seedMoviesIndex(request);
    routeAuditExperiment = await seedRouteAuditExperiment(request);
  });

  test.afterAll(async ({ request }) => {
    if (routeAuditExperiment) {
      await deleteExperiment(request, routeAuditExperiment.id);
    }
  });

  test('records route usability at 390px', async ({ page }) => {
    const evidence: RouteUsabilityEvidence[] = [];
    const routes = buildDashboardRouteAudit(MOVIES_INDEX, routeAuditExperiment);

    for (const route of routes) {
      try {
        await page.goto(route.path);
        await route.waitForReady(page);
        await expect(page.getByRole('main')).toBeVisible();
        const overflow = await describeHorizontalOverflow(page);
        const horizontalOverflow = isHorizontallyUnusable(overflow);

        evidence.push({
          id: route.id,
          appPath: route.appPath,
          path: route.path,
          status: horizontalOverflow ? 'unusable' : 'usable',
          issue: horizontalOverflow ? HORIZONTAL_OVERFLOW_ISSUE : '',
          detail: summarizeHorizontalOverflow(overflow),
          documentOverflow: overflow.documentOverflow,
          culpritCount: overflow.culpritCount,
        });
      } catch (error) {
        evidence.push({
          id: route.id,
          appPath: route.appPath,
          path: route.path,
          status: 'unusable',
          issue: error instanceof Error ? error.message : String(error),
          detail: '',
          documentOverflow: null,
          culpritCount: null,
        });
      }
    }

    const outputPath = resolveAuditOutputPath();
    fs.mkdirSync(path.dirname(outputPath), { recursive: true });
    fs.writeFileSync(outputPath, `${JSON.stringify(evidence, null, 2)}\n`, 'utf8');

    expect(evidence).toHaveLength(23);
    expect(evidence.map((entry) => entry.appPath)).toContain('/experiments/:experimentId');
    expect(evidence.filter((entry) => entry.status !== 'usable')).toEqual([]);
    // Pin the status against raw measurements, rather than against another derived summary.
    expect(
      evidence
        .filter((entry) => entry.status === 'usable')
        .every((entry) => entry.documentOverflow === false && entry.culpritCount === 0),
    ).toBe(true);
    expect(evidence.every((entry) => entry.path.length > 0)).toBe(true);
    expect(
      evidence.filter(
        (entry) => entry.issue !== '' && entry.issue !== HORIZONTAL_OVERFLOW_ISSUE,
      ),
    ).toEqual([]);
  });
});
