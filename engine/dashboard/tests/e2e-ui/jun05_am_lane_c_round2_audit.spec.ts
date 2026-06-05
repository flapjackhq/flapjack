import fs from 'node:fs';
import path from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures/auth.fixture';
import { buildDashboardRouteAudit } from './route_audit_manifest';
import {
  MOVIES_INDEX,
  resolveLaneCBundleDir,
  seedMoviesIndex,
} from '../fixtures/lane_c_movies';
import {
  collectStableOverviewHeadings,
  selectEventStatusFinding,
  selectDocumentActionFinding,
} from './jun05_am_lane_c_round2_audit_helpers';

type BrowserIssueEvidence = {
  type: 'console.warn' | 'console.error' | 'pageerror';
  text: string;
};

type ButtonEvidence = {
  ariaLabel: string | null;
  text: string;
  title: string | null;
};

type RouteAuditEvidence = {
  browserIssues: BrowserIssueEvidence[];
  headings: string[];
  observations: Record<string, unknown>;
  path: string;
};

const AUDITED_PATHS = ['/cluster', '/events', `/index/${MOVIES_INDEX}`, '/overview'] as const;

function resolveEvidencePath(): string {
  const bundleDir = resolveLaneCBundleDir(process.env.LANE_C_BUNDLE_DIR);
  if (!bundleDir) {
    throw new Error('LANE_C_BUNDLE_DIR must point at the active Lane C bundle for round 2 audit evidence');
  }

  fs.mkdirSync(bundleDir, { recursive: true });
  return path.join(bundleDir, 'jun05_round2_route_evidence.json');
}

function attachBrowserIssueCapture(page: Page, issues: BrowserIssueEvidence[]): void {
  page.on('console', (message) => {
    if (message.type() === 'warning' || message.type() === 'error') {
      issues.push({
        type: message.type() === 'warning' ? 'console.warn' : 'console.error',
        text: message.text(),
      });
    }
  });
  page.on('pageerror', (error) => {
    issues.push({ type: 'pageerror', text: error.message });
  });
}

async function collectHeadings(page: Page): Promise<string[]> {
  const headings = await page.getByRole('heading').allTextContents();
  return headings.map((heading) => heading.trim()).filter(Boolean);
}

async function collectOverviewIndexHeadings(page: Page): Promise<string[]> {
  const headings = await page
    .getByTestId(/^overview-index-row-/)
    .getByRole('heading')
    .allTextContents();
  return headings.map((heading) => heading.trim()).filter(Boolean);
}

async function collectRouteHeadings(page: Page, routePath: string): Promise<string[]> {
  const headings = await collectHeadings(page);
  if (routePath !== '/overview') {
    return headings;
  }

  return collectStableOverviewHeadings(
    headings,
    await collectOverviewIndexHeadings(page),
    [MOVIES_INDEX],
  );
}

async function collectRouteObservations(page: Page, routePath: string): Promise<Record<string, unknown>> {
  if (routePath === '/cluster') {
    const standaloneState = page.getByTestId('cluster-standalone-state');
    await expect(standaloneState).toContainText('Standalone mode');
    await expect(standaloneState).toContainText('Mode');
    await expect(standaloneState).toContainText('Single-node operation is healthy and expected.');

    return {
      standaloneCopy: await standaloneState.innerText(),
      selectedFinding: 'Standalone copy presents single-node mode as a healthy default instead of an alarming disabled state.',
    };
  }

  if (routePath === '/events') {
    const statusFilter = page.getByLabel('Status');
    await expect(statusFilter).toBeVisible();
    const statusOptions = await statusFilter.evaluate((selectElement) => (
      Array.from((selectElement as HTMLSelectElement).options).map((option) => ({
        label: option.text,
        value: option.value,
      }))
    ));

    return {
      selectedFinding: selectEventStatusFinding(statusOptions),
      statusOptions,
    };
  }

  if (routePath === `/index/${MOVIES_INDEX}`) {
    const firstCard = page.getByTestId('document-card').first();
    await expect(firstCard).toContainText(/movie_\d{3}/);
    const buttons = await firstCard.getByRole('button').evaluateAll((buttonElements) => (
      buttonElements.map((buttonElement) => ({
        ariaLabel: buttonElement.getAttribute('aria-label'),
        text: buttonElement.textContent?.trim() ?? '',
        title: buttonElement.getAttribute('title'),
      }))
    )) satisfies ButtonEvidence[];

    return {
      firstResultText: await firstCard.innerText(),
      resultButtons: buttons,
      selectedFinding: selectDocumentActionFinding(buttons),
    };
  }

  if (routePath === '/overview') {
    await expect(page.getByTestId('stat-card-indexes')).toBeVisible();
    return {
      freshEyesRoute: 'Overview was the bounded fresh-eyes route; browserIssues carries the route decision evidence.',
    };
  }

  return {};
}

test.describe('Lane C round 2 focused audit', () => {
  test.describe.configure({ mode: 'serial' });

  test.beforeAll(async ({ request }) => {
    await seedMoviesIndex(request);
  });

  test('records focused route evidence for the round 2 ranked audit', async ({ page }) => {
    const auditRoutes = buildDashboardRouteAudit(MOVIES_INDEX);
    const evidence: RouteAuditEvidence[] = [];

    for (const routePath of AUDITED_PATHS) {
      const route = auditRoutes.find((candidate) => candidate.path === routePath);
      expect(route, `route manifest must own ${routePath}`).toBeDefined();

      const browserIssues: BrowserIssueEvidence[] = [];
      attachBrowserIssueCapture(page, browserIssues);

      await page.goto(routePath);
      await route?.waitForReady(page);

      evidence.push({
        browserIssues,
        headings: await collectRouteHeadings(page, routePath),
        observations: await collectRouteObservations(page, routePath),
        path: routePath,
      });

      page.removeAllListeners('console');
      page.removeAllListeners('pageerror');
    }

    fs.writeFileSync(resolveEvidencePath(), `${JSON.stringify(evidence, null, 2)}\n`, 'utf8');

    expect(evidence).toHaveLength(AUDITED_PATHS.length);
    expect(evidence.map((entry) => entry.path)).toEqual([...AUDITED_PATHS]);
    expect(evidence.every((entry) => entry.headings.length > 0)).toBe(true);
  });
});
