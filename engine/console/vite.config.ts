import { sveltekit } from '@sveltejs/kit/vite';
import { svelteTesting } from '@testing-library/svelte/vite';
import { defineConfig } from 'vitest/config';

const backendTarget = process.env.FJ_CONSOLE_BACKEND_URL ?? 'http://127.0.0.1:7700';

export default defineConfig({
  plugins: [sveltekit(), svelteTesting()],
  server: {
    proxy: {
      '/1': {
        target: backendTarget,
        changeOrigin: true,
      },
      '/health': {
        target: backendTarget,
        changeOrigin: true,
      },
    },
  },
  test: {
    environment: 'jsdom',
    include: ['component-tests/**/*.test.ts'],
    maxWorkers: 2,
    testTimeout: 5_000,
    hookTimeout: 5_000,
  },
});
