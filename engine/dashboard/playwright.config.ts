import { defineConfig, devices } from '@playwright/test';
import { getLocalInstanceConfig } from './local-instance-config';

const instance = getLocalInstanceConfig();

function parseWorkersOverride(rawValue: string | undefined): number | undefined {
  if (!rawValue) {
    return undefined;
  }

  const parsed = Number(rawValue);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    return undefined;
  }

  return parsed;
}

const localWorkersOverride = parseWorkersOverride(process.env.PLAYWRIGHT_E2E_WORKERS);
const videoMode = process.env.FJ_PLAYWRIGHT_VIDEO === 'on' ? 'on' : undefined;

/**
 * Playwright configuration for Flapjack dashboard.
 *
 * Three test categories:
 * - e2e-ui: Real browser + real server, simulated-human interaction (no mocks)
 * - e2e-api: API-level tests against real server (no browser rendering)
 * - seed/cleanup: Setup/teardown projects for e2e-ui data seeding
 *
 * @see https://playwright.dev/docs/test-configuration
 */
export default defineConfig({
  testDir: './tests',
  globalSetup: './tests/global-setup.ts',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 1 : (localWorkersOverride ?? 3),
  reporter: 'html',

  use: {
    baseURL: instance.dashboardBaseUrl,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: videoMode,
  },

  projects: [
    // --- Setup: seed test data into real backend ---
    {
      name: 'seed',
      testDir: './tests/e2e-ui',
      testMatch: 'seed.setup.ts',
      teardown: 'cleanup',
    },
    // --- Teardown: delete test data ---
    {
      name: 'cleanup',
      testDir: './tests/e2e-ui',
      testMatch: 'cleanup.setup.ts',
    },
    // --- E2E-UI: real browser + real server, no mocks ---
    {
      name: 'e2e-ui',
      testDir: './tests/e2e-ui',
      testIgnore: ['*.setup.ts', '*.test.ts'],
      dependencies: ['seed'],
      use: { ...devices['Desktop Chrome'] },
    },
    // --- E2E-API: API-level tests against real server (no browser rendering) ---
    // For real-browser simulated-human tests, see the e2e-ui project above.
    {
      name: 'e2e-api',
      testDir: './tests/e2e-api',
      use: { ...devices['Desktop Chrome'] },
    },
  ],

  webServer: {
    command: 'node scripts/playwright-webserver.mjs',
    env: {
      ...process.env,
      PLAYWRIGHT_WEBSERVER_HOST: instance.host,
      PLAYWRIGHT_WEBSERVER_PORT: String(instance.dashboardPort),
      PLAYWRIGHT_WEBSERVER_URL: instance.dashboardBaseUrl,
      // Always spawn a fresh dashboard process for this run. Reuse mode depends on
      // a cross-process lease file and can block indefinitely if a stale lease is left behind.
      PLAYWRIGHT_WEBSERVER_REUSE: '0',
    },
    url: instance.dashboardBaseUrl,
    // Always start this workspace's Vite server so Playwright never reuses a foreign repo
    // that happens to already be listening on the shared local dashboard port.
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
