import { describe, expect, it, vi, beforeEach } from 'vitest';
import { join } from 'path';

const { dotenvConfig } = vi.hoisted(() => ({
  dotenvConfig: vi.fn(),
}));

vi.mock('dotenv', () => ({
  config: dotenvConfig,
}));

import globalSetup, {
  loadPlaywrightSecretEnv,
  resolvePlaywrightSecretEnvPath,
} from './global-setup';

describe('resolvePlaywrightSecretEnvPath', () => {
  it('returns undefined when secret-file loading is explicitly disabled', () => {
    expect(resolvePlaywrightSecretEnvPath({ FJ_NO_SECRET_FILE: '1' }, '/repo/engine/dashboard/tests')).toBeUndefined();
  });

  it('reports an explicit FJ_SECRET_FILE path as explicitly chosen', () => {
    expect(resolvePlaywrightSecretEnvPath(
      { FJ_SECRET_FILE: '/tmp/custom.env' },
      '/repo/engine/dashboard/tests',
    )).toEqual({ path: '/tmp/custom.env', explicit: true });
  });

  it('reports the engine/.secret/.env.secret fallback as not explicitly chosen', () => {
    expect(resolvePlaywrightSecretEnvPath({}, '/repo/engine/dashboard/tests')).toEqual({
      path: join('/repo/engine/dashboard/tests', '..', '..', '.secret', '.env.secret'),
      explicit: false,
    });
  });

  it('treats an empty FJ_SECRET_FILE as no explicit choice', () => {
    expect(resolvePlaywrightSecretEnvPath({ FJ_SECRET_FILE: '' }, '/repo/engine/dashboard/tests')).toEqual({
      path: join('/repo/engine/dashboard/tests', '..', '..', '.secret', '.env.secret'),
      explicit: false,
    });
  });
});

describe('loadPlaywrightSecretEnv', () => {
  beforeEach(() => {
    dotenvConfig.mockReset();
    vi.unstubAllEnvs();
  });

  it('does not call dotenv when FJ_NO_SECRET_FILE=1', () => {
    loadPlaywrightSecretEnv({ FJ_NO_SECRET_FILE: '1' }, '/repo/engine/dashboard/tests');

    expect(dotenvConfig).not.toHaveBeenCalled();
  });

  it('lets the explicit secret file replace stale ambient credential values', () => {
    loadPlaywrightSecretEnv({ FJ_SECRET_FILE: '/tmp/custom.env' }, '/repo/engine/dashboard/tests');

    expect(dotenvConfig).toHaveBeenCalledWith({
      path: '/tmp/custom.env',
      override: true,
    });
  });

  it('preserves ambient credential values when loading the fallback secret file', () => {
    loadPlaywrightSecretEnv({}, '/repo/engine/dashboard/tests');

    expect(dotenvConfig).toHaveBeenCalledWith({
      path: join('/repo/engine/dashboard/tests', '..', '..', '.secret', '.env.secret'),
    });
  });

  it('loads the fallback file without override when FJ_SECRET_FILE is set but empty', () => {
    loadPlaywrightSecretEnv({ FJ_SECRET_FILE: '' }, '/repo/engine/dashboard/tests');

    expect(dotenvConfig).toHaveBeenCalledWith({
      path: join('/repo/engine/dashboard/tests', '..', '..', '.secret', '.env.secret'),
    });
  });

  it('keeps global setup as the Playwright secret-loading entry point', () => {
    vi.stubEnv('FJ_SECRET_FILE', '/tmp/global.env');

    globalSetup();

    expect(dotenvConfig).toHaveBeenCalledWith({ path: '/tmp/global.env', override: true });
  });
});
