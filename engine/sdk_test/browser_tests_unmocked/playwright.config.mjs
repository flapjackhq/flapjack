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
    trace: 'retain-on-failure',
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
      VITE_FLAPJACK_SEARCH_KEY: process.env.REAL_CLIENT_SEARCH_KEY,
      VITE_REAL_CLIENT_INDEX_NAME: process.env.REAL_CLIENT_INDEX_NAME,
    },
  },
});
