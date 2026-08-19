import { defineConfig } from '@playwright/test';
import { requireOwnedTestBackend } from './local_backend';

const host = process.env.FJ_CONSOLE_HOST ?? '127.0.0.1';
const port = Number(process.env.FJ_CONSOLE_PORT ?? '5190');
const baseURL = `http://${host}:${port}`;
requireOwnedTestBackend();

export default defineConfig({
  testDir: '.',
  testMatch: 'smoke/**/*.spec.ts',
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: [['list'], ['html', { open: 'never' }]],
  outputDir: '../test-results/console-unmocked',
  use: {
    baseURL,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [
    { name: 'desktop', use: { browserName: 'chromium', viewport: { width: 1280, height: 720 } } },
    { name: 'mobile-390', use: { browserName: 'chromium', viewport: { width: 390, height: 844 } } },
  ],
  webServer: {
    command: `npm run dev -- --host ${host} --port ${port} --strictPort`,
    url: `${baseURL}/dashboard/`,
    reuseExistingServer: false,
    timeout: 30_000,
  },
});
