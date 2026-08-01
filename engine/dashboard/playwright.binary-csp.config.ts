import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './tests/e2e-binary',
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: [['html', { open: 'never' }]],
  use: {
    ...devices['Desktop Chrome'],
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [
    {
      name: 'binary-csp-dashboard',
      testMatch: 'csp_binary_dashboard.spec.ts',
    },
  ],
});
