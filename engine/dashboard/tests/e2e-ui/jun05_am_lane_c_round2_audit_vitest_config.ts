import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    environment: 'node',
    include: [
      'tests/e2e-ui/jun05_am_lane_c_round2_audit_helpers.test.ts',
      'playwright.config.test.ts',
    ],
  },
});
