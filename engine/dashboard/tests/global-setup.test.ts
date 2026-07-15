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

  it('uses the explicit FJ_SECRET_FILE path when provided', () => {
    expect(resolvePlaywrightSecretEnvPath(
      { FJ_SECRET_FILE: '/tmp/custom.env' },
      '/repo/engine/dashboard/tests',
    )).toBe('/tmp/custom.env');
  });

  it('falls back to engine/.secret/.env.secret relative to the tests directory', () => {
    expect(resolvePlaywrightSecretEnvPath({}, '/repo/engine/dashboard/tests')).toBe(
      join('/repo/engine/dashboard/tests', '..', '..', '.secret', '.env.secret'),
    );
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

  it('loads the explicit secret file path through dotenv', () => {
    loadPlaywrightSecretEnv({ FJ_SECRET_FILE: '/tmp/custom.env' }, '/repo/engine/dashboard/tests');

    expect(dotenvConfig).toHaveBeenCalledWith({ path: '/tmp/custom.env' });
  });

  it('keeps global setup as the Playwright secret-loading entry point', () => {
    vi.stubEnv('FJ_SECRET_FILE', '/tmp/global.env');

    globalSetup();

    expect(dotenvConfig).toHaveBeenCalledWith({ path: '/tmp/global.env' });
  });
});
