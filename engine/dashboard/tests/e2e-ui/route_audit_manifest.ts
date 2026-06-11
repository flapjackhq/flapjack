import type { Page } from '@playwright/test';
import { expect } from '@playwright/test';
import { waitForSearchResultsOrEmptyState } from './helpers';

export type DashboardRoute = {
  id: string;
  appPath: string;
  path: string;
  coverage: 'global route' | 'seeded child route' | 'global /index/:indexName route + seeded child index route';
  waitForReady: (page: Page) => Promise<void>;
};

export type ExcludedDashboardRoute = {
  appPath: string;
  reason: 'fallback_shell' | 'requires_runtime_experiment_fixture';
  detail: string;
};

export const APP_USER_FACING_ROUTE_PATTERNS = [
  '/',
  '/overview',
  '/index/:indexName',
  '/index/:indexName/settings',
  '/index/:indexName/analytics',
  '/index/:indexName/synonyms',
  '/index/:indexName/rules',
  '/index/:indexName/merchandising',
  '/index/:indexName/recommendations',
  '/index/:indexName/chat',
  '/keys',
  '/logs',
  '/migrate',
  '/metrics',
  '/cluster',
  '/system',
  '/query-suggestions',
  '/experiments',
  '/experiments/:experimentId',
  '/events',
  '/personalization',
  '/dictionaries',
  '/security-sources',
  '*',
] as const;

export const EXCLUDED_DASHBOARD_ROUTES: readonly ExcludedDashboardRoute[] = [
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
] as const;

export function buildDashboardRouteAudit(indexName: string): readonly DashboardRoute[] {
  const indexBasePath = `/index/${indexName}`;

  return [
    {
      id: 'root-overview',
      appPath: '/',
      path: '/',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'overview',
      appPath: '/overview',
      path: '/overview',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'index-search-browse',
      appPath: '/index/:indexName',
      path: indexBasePath,
      coverage: 'global /index/:indexName route + seeded child index route',
      waitForReady: async (page) => {
        await waitForSearchResultsOrEmptyState(page, { requireResults: true });
      },
    },
    {
      id: 'api-keys',
      appPath: '/keys',
      path: '/keys',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(
          page.getByTestId('keys-list').or(page.getByRole('heading', { name: 'No API keys', exact: true })),
        ).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'api-logs',
      appPath: '/logs',
      path: '/logs',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByRole('heading', { name: 'API Logs', exact: true })).toBeVisible({
          timeout: 15_000,
        });
      },
    },
    {
      id: 'migrate',
      appPath: '/migrate',
      path: '/migrate',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByRole('heading', { name: 'Migrate from Algolia', exact: true })).toBeVisible({
          timeout: 15_000,
        });
      },
    },
    {
      id: 'metrics',
      appPath: '/metrics',
      path: '/metrics',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByTestId('metrics-version')).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'cluster',
      appPath: '/cluster',
      path: '/cluster',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(
          page.getByTestId('cluster-standalone-state')
            .or(page.getByTestId('cluster-peer-table'))
            .or(page.getByTestId('cluster-error-state')),
        ).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'system',
      appPath: '/system',
      path: '/system',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByTestId('health-version')).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'query-suggestions',
      appPath: '/query-suggestions',
      path: '/query-suggestions',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByRole('heading', { name: 'Query Suggestions', exact: true })).toBeVisible({
          timeout: 15_000,
        });
      },
    },
    {
      id: 'experiments-list',
      appPath: '/experiments',
      path: '/experiments',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByTestId('experiments-heading')).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'events',
      appPath: '/events',
      path: '/events',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByRole('heading', { name: 'Event Debugger', exact: true })).toBeVisible({
          timeout: 15_000,
        });
      },
    },
    {
      id: 'personalization',
      appPath: '/personalization',
      path: '/personalization',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByRole('heading', { name: 'Personalization', exact: true })).toBeVisible({
          timeout: 15_000,
        });
      },
    },
    {
      id: 'dictionaries',
      appPath: '/dictionaries',
      path: '/dictionaries',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByTestId('add-dictionary-entry-btn')).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'security-sources',
      appPath: '/security-sources',
      path: '/security-sources',
      coverage: 'global route',
      waitForReady: async (page) => {
        await expect(page.getByRole('heading', { name: 'Source Allowlist', exact: true })).toBeVisible({
          timeout: 15_000,
        });
      },
    },
    {
      id: 'index-settings',
      appPath: '/index/:indexName/settings',
      path: `${indexBasePath}/settings`,
      coverage: 'seeded child route',
      waitForReady: async (page) => {
        await expect(page.getByText('Searchable Attributes', { exact: true })).toBeVisible({
          timeout: 15_000,
        });
      },
    },
    {
      id: 'index-analytics',
      appPath: '/index/:indexName/analytics',
      path: `${indexBasePath}/analytics`,
      coverage: 'seeded child route',
      waitForReady: async (page) => {
        await expect(page.getByTestId('analytics-heading')).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'index-synonyms',
      appPath: '/index/:indexName/synonyms',
      path: `${indexBasePath}/synonyms`,
      coverage: 'seeded child route',
      waitForReady: async (page) => {
        await expect(
          page.getByTestId('synonyms-list').or(page.getByRole('heading', { name: 'No synonyms', exact: true })),
        ).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'index-rules',
      appPath: '/index/:indexName/rules',
      path: `${indexBasePath}/rules`,
      coverage: 'seeded child route',
      waitForReady: async (page) => {
        await expect(
          page.getByTestId('rules-list').or(page.getByRole('heading', { name: 'No rules', exact: true })),
        ).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'index-merchandising',
      appPath: '/index/:indexName/merchandising',
      path: `${indexBasePath}/merchandising`,
      coverage: 'seeded child route',
      waitForReady: async (page) => {
        await expect(page.getByTestId('merch-search-input')).toBeVisible({ timeout: 15_000 });
      },
    },
    {
      id: 'index-recommendations',
      appPath: '/index/:indexName/recommendations',
      path: `${indexBasePath}/recommendations`,
      coverage: 'seeded child route',
      waitForReady: async (page) => {
        await expect(page.getByTestId('recommendations-model-select')).toBeVisible({
          timeout: 15_000,
        });
      },
    },
    {
      id: 'index-chat',
      appPath: '/index/:indexName/chat',
      path: `${indexBasePath}/chat`,
      coverage: 'seeded child route',
      waitForReady: async (page) => {
        await expect(
          page.getByTestId('chat-requires-neural-search').or(page.getByTestId('chat-capability-disabled')),
        ).toBeVisible({ timeout: 15_000 });
      },
    },
  ] as const;
}

export function assertDashboardRouteCoverage(routes: readonly DashboardRoute[]): void {
  const auditedAppPaths = new Set(routes.map((route) => route.appPath));
  const excludedAppPaths = new Set(EXCLUDED_DASHBOARD_ROUTES.map((route) => route.appPath));
  const coveredAppPaths = [...auditedAppPaths, ...excludedAppPaths].sort();
  const expectedAppPaths = [...APP_USER_FACING_ROUTE_PATTERNS].sort();

  expect(coveredAppPaths).toEqual(expectedAppPaths);
  expect([...auditedAppPaths].filter((path) => excludedAppPaths.has(path))).toEqual([]);
}
