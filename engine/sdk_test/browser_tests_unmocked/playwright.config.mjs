import { defineConfig } from '@playwright/test';
import { fileURLToPath } from 'node:url';

const testDir = fileURLToPath(new URL('.', import.meta.url));
const webPort = Number(process.env.REAL_CLIENT_WEB_PORT);
if (!Number.isInteger(webPort) || webPort < 1024 || webPort > 65535) {
  throw new Error(`REAL_CLIENT_WEB_PORT must be a non-privileged TCP port, got ${process.env.REAL_CLIENT_WEB_PORT}`);
}

const requiredEnvironment = ['FLAPJACK_URL', 'REAL_CLIENT_SEARCH_KEY', 'REAL_CLIENT_INDEX_NAME'];
for (const name of requiredEnvironment) {
  if (!process.env[name]) throw new Error(`Missing required ${name}`);
}

const baseURL = `http://127.0.0.1:${webPort}`;

export default defineConfig({
  testDir,
  testMatch: 'real_client_conformance.spec.mjs',
  globalTimeout: 120_000,
  timeout: 30_000,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['line']],
  use: {
    baseURL,
    browserName: 'chromium',
    headless: true,
    // Native query credentials are intentionally exercised. Persisting a browser
    // trace would copy them into a test artifact, so this conformance owner keeps
    // tracing disabled and makes failures rely on redacted assertions/reporting.
    trace: 'off',
    screenshot: 'only-on-failure',
  },
  webServer: {
    command: `npx vite --config browser_tests_unmocked/vite.config.mjs --host 127.0.0.1 --port ${webPort}`,
    cwd: fileURLToPath(new URL('..', import.meta.url)),
    url: baseURL,
    reuseExistingServer: false,
    timeout: 30_000,
    env: {
      VITE_FLAPJACK_URL: process.env.FLAPJACK_URL,
      VITE_FLAPJACK_APPLICATION_ID: 'flapjack',
      VITE_FLAPJACK_SEARCH_KEY: process.env.REAL_CLIENT_SEARCH_KEY,
      VITE_REAL_CLIENT_INDEX_NAME: process.env.REAL_CLIENT_INDEX_NAME,
      VITE_REAL_CLIENT_USER_TOKEN: '3f25cf54-46f6-4f67-9ac8-87c4a34c86f1',
    },
  },
});
