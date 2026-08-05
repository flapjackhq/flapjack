/* @vitest-environment node */
import type { ConfigEnv } from 'vite';
import { describe, expect, it, vi } from 'vitest';

const { mockInstance } = vi.hoisted(() => ({
  mockInstance: {
    host: '127.0.0.1',
    backendPort: 7711,
    dashboardPort: 5511,
    adminKey: 'test-admin-key',
    backendBaseUrl: 'http://127.0.0.1:7711',
    backendDataDir: '/tmp/flapjack/test-data',
    dashboardBaseUrl: 'http://127.0.0.1:5511',
    configPath: '/tmp/flapjack.local.conf',
    loadedFromFile: true,
  },
}));

vi.mock('./local-instance-config', () => ({
  getLocalInstanceConfig: () => mockInstance,
}));

import viteConfig from './vite.config';

const resolveConfig = async (command: 'build' | 'serve') => {
  const env: ConfigEnv = {
    command,
    mode: 'test',
    isSsrBuild: false,
  };

  return typeof viteConfig === 'function' ? await viteConfig(env) : viteConfig;
};

describe('vite.config lifecycle contracts', () => {
  it('uses clone-local host/ports and proxy targets for serve mode', async () => {
    const config = await resolveConfig('serve');

    expect(config.base).toBe('/');
    expect(config.define?.__BACKEND_URL__).toBe(JSON.stringify(mockInstance.backendBaseUrl));
    expect(config.server?.host).toBe(mockInstance.host);
    expect(config.server?.strictPort).toBe(true);
    expect(config.server?.port).toBe(mockInstance.dashboardPort);
    expect(config.server?.proxy).toMatchObject({
      '/1': mockInstance.backendBaseUrl,
      '/2': mockInstance.backendBaseUrl,
      '/health': mockInstance.backendBaseUrl,
      '/internal': mockInstance.backendBaseUrl,
      '/api-docs': mockInstance.backendBaseUrl,
      '/swagger-ui': mockInstance.backendBaseUrl,
      '/__flapjack_metrics': {
        target: mockInstance.backendBaseUrl,
        rewrite: expect.any(Function),
      },
    });

    const metricsProxy = config.server?.proxy?.['/__flapjack_metrics'];
    expect(typeof metricsProxy).toBe('object');
    if (typeof metricsProxy === 'object') {
      expect(metricsProxy.rewrite?.('/__flapjack_metrics')).toBe('/metrics');
    }
  });

  it('keeps Playwright artifact directories out of Vite watch graph', async () => {
    const config = await resolveConfig('serve');
    const ignored = config.server?.watch?.ignored;

    expect(ignored).toEqual(expect.arrayContaining([
      '**/playwright-report/**',
      '**/test-results/**',
    ]));
  });

  it('serves dashboard assets from /dashboard/ in build mode', async () => {
    const config = await resolveConfig('build');
    expect(config.base).toBe('/dashboard/');
  });
});
