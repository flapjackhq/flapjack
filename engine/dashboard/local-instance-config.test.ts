import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  findTrackedBackendDataDir,
  getLocalInstanceConfig,
  parseLocalConfigFile,
  resolveBackendDataDir,
  resolveAdminKey,
} from './local-instance-config';

const tempDirs: string[] = [];

afterEach(() => {
  while (tempDirs.length > 0) {
    const tempDir = tempDirs.pop();
    if (tempDir) {
      fs.rmSync(tempDir, { recursive: true, force: true });
    }
  }
  vi.restoreAllMocks();
  vi.unstubAllEnvs();
});

function createTempDir() {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'fj-local-instance-'));
  tempDirs.push(tempDir);
  return tempDir;
}

function writeTrackedMetaFile(stateDir: string, fileName: string, dataDir: string) {
  fs.writeFileSync(path.join(stateDir, fileName), [
    'bind_addr=127.0.0.1:18893',
    `data_dir=${dataDir}`,
  ].join('\n'), { mode: 0o600 });
}

describe('parseLocalConfigFile', () => {
  it('parses plain KEY=value assignments', () => {
    const parsed = parseLocalConfigFile([
      'FJ_HOST=127.0.0.1',
      'FJ_BACKEND_PORT=18893',
      'FJ_DASHBOARD_PORT=15183',
      '',
    ].join('\n'));

    expect(parsed).toEqual({
      FJ_HOST: '127.0.0.1',
      FJ_BACKEND_PORT: '18893',
      FJ_DASHBOARD_PORT: '15183',
    });
  });

  it('parses export syntax and strips inline comments from unquoted values', () => {
    const parsed = parseLocalConfigFile([
      'export FJ_HOST=127.0.0.1',
      'export FJ_BACKEND_PORT=18893 # backend',
      'export FJ_DASHBOARD_PORT=15183    # dashboard',
      'export FJ_TEST_ADMIN_KEY=fj_dev_key # test key',
    ].join('\n'));

    expect(parsed).toEqual({
      FJ_HOST: '127.0.0.1',
      FJ_BACKEND_PORT: '18893',
      FJ_DASHBOARD_PORT: '15183',
      FJ_TEST_ADMIN_KEY: 'fj_dev_key',
    });
  });

  it('keeps # characters inside quoted values', () => {
    const parsed = parseLocalConfigFile([
      'FJ_TEST_ADMIN_KEY="key-with-#-inside"',
      "FJ_HOST='127.0.0.1#suffix'",
    ].join('\n'));

    expect(parsed).toEqual({
      FJ_TEST_ADMIN_KEY: 'key-with-#-inside',
      FJ_HOST: '127.0.0.1#suffix',
    });
  });

  it('strips inline comments that appear after quoted values', () => {
    const parsed = parseLocalConfigFile([
      'FJ_TEST_ADMIN_KEY="key-with-#-inside" # keep quoted value only',
      "FJ_HOST='127.0.0.1' # keep quoted host only",
    ].join('\n'));

    expect(parsed).toEqual({
      FJ_TEST_ADMIN_KEY: 'key-with-#-inside',
      FJ_HOST: '127.0.0.1',
    });
  });

  it('keeps escaped quotes inside quoted values when stripping trailing comments', () => {
    const parsed = parseLocalConfigFile([
      'FJ_TEST_ADMIN_KEY="a\\\"b" # keep escaped quote sequence',
    ].join('\n'));

    expect(parsed).toEqual({
      FJ_TEST_ADMIN_KEY: 'a\\\"b',
    });
  });

  it('keeps doubly escaped backslashes before embedded quotes in quoted values', () => {
    const parsed = parseLocalConfigFile([
      'FJ_TEST_ADMIN_KEY="a\\\\\\"b" # keep slash+quote sequence',
    ].join('\n'));

    expect(parsed).toEqual({
      FJ_TEST_ADMIN_KEY: 'a\\\\\\"b',
    });
  });

  it('keeps unquoted suffix text that follows a quoted segment before a trailing comment', () => {
    const parsed = parseLocalConfigFile([
      'FJ_TEST_ADMIN_KEY="abc"def # trailing comment',
      "FJ_HOST='127.0.0.1'sfx # single quoted suffix",
    ].join('\n'));

    expect(parsed).toEqual({
      FJ_TEST_ADMIN_KEY: 'abcdef',
      FJ_HOST: '127.0.0.1sfx',
    });
  });

  it('does not treat # immediately after a closing quote as a comment', () => {
    const parsed = parseLocalConfigFile([
      'FJ_TEST_ADMIN_KEY="abc"#inline-comment',
    ].join('\n'));

    expect(parsed).toEqual({
      FJ_TEST_ADMIN_KEY: 'abc#inline-comment',
    });
  });

  it('preserves trailing spaces that are inside quoted values', () => {
    const parsed = parseLocalConfigFile([
      'FJ_TEST_ADMIN_KEY="abc "',
      "FJ_HOST='127.0.0.1 ' # keep quoted trailing space",
    ].join('\n'));

    expect(parsed).toEqual({
      FJ_TEST_ADMIN_KEY: 'abc ',
      FJ_HOST: '127.0.0.1 ',
    });
  });

  it('keeps unquoted # characters that are part of the value', () => {
    const parsed = parseLocalConfigFile([
      'FJ_TEST_ADMIN_KEY=key-with-#-inside',
      'FLAPJACK_BACKEND_URL=http://127.0.0.1:7701/#dev',
    ].join('\n'));

    expect(parsed).toEqual({
      FJ_TEST_ADMIN_KEY: 'key-with-#-inside',
      FLAPJACK_BACKEND_URL: 'http://127.0.0.1:7701/#dev',
    });
  });

  it('ignores comments, blank lines, and malformed lines', () => {
    const parsed = parseLocalConfigFile([
      '# comment',
      '',
      'NOT_AN_ASSIGNMENT',
      'export',
      '=missing_key',
      'FJ_HOST=0.0.0.0',
    ].join('\n'));

    expect(parsed).toEqual({
      FJ_HOST: '0.0.0.0',
    });
  });
});

describe('findTrackedBackendDataDir', () => {
  it('finds a tracked backend data dir from multi-instance metadata', () => {
    const stateDir = createTempDir();
    // normalizeTrackedDataDir calls realpathSync, so compare against the resolved path
    const dataDir = fs.realpathSync(createTempDir());
    writeTrackedMetaFile(stateDir, 'review.meta', dataDir);

    expect(findTrackedBackendDataDir('http://127.0.0.1:18893', stateDir)).toBe(dataDir);
  });

  it('treats localhost and loopback bind-address aliases as the same tracked instance', () => {
    const stateDir = createTempDir();
    const dataDir = fs.realpathSync(createTempDir());
    writeTrackedMetaFile(stateDir, 'loopback.meta', dataDir);

    expect(findTrackedBackendDataDir('http://localhost:18893', stateDir)).toBe(dataDir);
  });

  it('ignores tracked metadata files with unsafe write permissions', () => {
    const stateDir = createTempDir();
    const dataDir = fs.realpathSync(createTempDir());
    const metaPath = path.join(stateDir, 'insecure.meta');
    fs.writeFileSync(metaPath, [
      'bind_addr=127.0.0.1:18893',
      `data_dir=${dataDir}`,
    ].join('\n'), { mode: 0o600 });
    fs.chmodSync(metaPath, 0o666);

    expect(findTrackedBackendDataDir('http://127.0.0.1:18893', stateDir)).toBeNull();
  });
});

describe('resolveBackendDataDir', () => {
  it('prefers configured backend data dir values over tracked metadata', () => {
    const stateDir = createTempDir();
    const dataDir = fs.realpathSync(createTempDir());
    writeTrackedMetaFile(stateDir, 'review.meta', dataDir);

    // pickConfiguredValue returns the raw string without realpathSync — no symlink resolution
    const configuredDir = createTempDir();
    expect(resolveBackendDataDir(
      { FJ_DATA_DIR: configuredDir },
      'http://127.0.0.1:18893',
      stateDir,
    )).toBe(configuredDir);
  });

  it('falls back to tracked multi-instance metadata when no configured override exists', () => {
    const stateDir = createTempDir();
    const dataDir = fs.realpathSync(createTempDir());
    writeTrackedMetaFile(stateDir, 'staging.meta', dataDir);

    expect(resolveBackendDataDir({}, 'http://127.0.0.1:18893', stateDir)).toBe(dataDir);
  });

  it('resolves tracked metadata for localhost backend URLs without falling back to engine/data', () => {
    const stateDir = createTempDir();
    const dataDir = fs.realpathSync(createTempDir());
    writeTrackedMetaFile(stateDir, 'localhost.meta', dataDir);

    expect(resolveBackendDataDir({}, 'http://localhost:18893', stateDir)).toBe(dataDir);
  });

  it('falls back to the repo engine/data dir when no override or tracked instance exists', () => {
    const stateDir = createTempDir();

    expect(resolveBackendDataDir({}, 'http://127.0.0.1:18893', stateDir)).toMatch(/\/engine\/data$/);
  });
});

describe('resolveAdminKey', () => {
  it('allows the test default admin key for loopback backends', () => {
    expect(resolveAdminKey(undefined, 'http://127.0.0.1:7700')).toBe('fj_devtestadminkey000000');
    expect(resolveAdminKey(undefined, 'http://localhost:7700')).toBe('fj_devtestadminkey000000');
  });

  it('prefers the live loopback process admin key when env defaults are stale', () => {
    expect(resolveAdminKey(
      'fj_devtestadminkey000000',
      'http://127.0.0.1:7700',
      { loopbackProcessAdminKey: 'runtime-admin-key' },
    )).toBe('runtime-admin-key');
  });

  it('requires an explicit admin key for non-loopback backends', () => {
    expect(() => resolveAdminKey(undefined, 'https://staging.example.com')).toThrow(
      /FJ_TEST_ADMIN_KEY must be set/i,
    );
  });

  it('uses the configured admin key for non-loopback backends', () => {
    expect(resolveAdminKey('remote-admin-key', 'https://staging.example.com')).toBe(
      'remote-admin-key',
    );
  });
});

describe('getLocalInstanceConfig', () => {
  it('prefers flapjack.local.conf values over env overrides for shared routing keys', () => {
    vi.stubEnv('FJ_HOST', '0.0.0.0');
    vi.stubEnv('FJ_BACKEND_PORT', '19999');
    vi.stubEnv('FJ_DASHBOARD_PORT', '59999');
    vi.stubEnv('FJ_TEST_ADMIN_KEY', 'env-admin-key');
    vi.stubEnv('FLAPJACK_BACKEND_URL', 'http://127.0.0.1:19999');
    vi.spyOn(fs, 'existsSync').mockImplementation((target) => (
      String(target).endsWith('flapjack.local.conf')
    ));
    vi.spyOn(fs, 'readFileSync').mockImplementation((target) => {
      if (!String(target).endsWith('flapjack.local.conf')) {
        throw new Error(`Unexpected read target: ${String(target)}`);
      }

      return [
        'FJ_HOST=127.0.0.1',
        'FJ_BACKEND_PORT=18893',
        'FJ_DASHBOARD_PORT=15183',
        'FJ_TEST_ADMIN_KEY=file-admin-key',
        'FLAPJACK_BACKEND_URL=http://localhost:18893',
      ].join('\n');
    });

    const config = getLocalInstanceConfig();

    expect(config.host).toBe('127.0.0.1');
    expect(config.backendPort).toBe(18893);
    expect(config.dashboardPort).toBe(15183);
    expect(config.adminKey).toBe('file-admin-key');
    expect(config.backendBaseUrl).toBe('http://localhost:18893');
    expect(config.dashboardBaseUrl).toBe('http://127.0.0.1:15183');
    expect(config.loadedFromFile).toBe(true);
  });

  it('falls back to default ports when configured env ports are invalid', () => {
    vi.stubEnv('FJ_HOST', '127.0.0.1');
    vi.stubEnv('FJ_BACKEND_PORT', 'invalid-port');
    vi.stubEnv('FJ_DASHBOARD_PORT', '70000');
    vi.stubEnv('FLAPJACK_BACKEND_URL', 'not-a-valid-url');
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);

    const config = getLocalInstanceConfig();

    expect(config.backendPort).toBe(7700);
    expect(config.dashboardPort).toBe(5177);
    expect(config.backendBaseUrl).toBe('http://127.0.0.1:7700');
    expect(config.dashboardBaseUrl).toBe('http://127.0.0.1:5177');
  });

  it.each([undefined, 'not-a-valid-url'])(
    'keeps the local-config backend endpoint coherent when the URL override is %s',
    (backendUrl) => {
      vi.stubEnv('FLAPJACK_BACKEND_URL', backendUrl);
      vi.spyOn(fs, 'existsSync').mockImplementation((target) => (
        String(target).endsWith('flapjack.local.conf')
      ));
      vi.spyOn(fs, 'readFileSync').mockReturnValue([
        'FJ_HOST=localhost',
        'FJ_BACKEND_PORT=18893',
      ].join('\n'));

      const config = getLocalInstanceConfig();

      expect(config).toMatchObject({
        host: 'localhost',
        backendHost: 'localhost',
        backendPort: 18893,
        backendBaseUrl: 'http://localhost:18893',
        loadedFromFile: true,
      });
    },
  );

  it('keeps the backend port coherent with a loopback FLAPJACK_BACKEND_URL', () => {
    vi.stubEnv('FLAPJACK_BACKEND_URL', 'http://127.0.0.1:17707');
    vi.stubEnv('FJ_BACKEND_PORT', undefined);
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);

    const config = getLocalInstanceConfig();

    expect(config.loadedFromFile).toBe(false);
    expect(config.backendBaseUrl).toBe('http://127.0.0.1:17707');
    expect(config.backendPort).toBe(17707);
  });

  it('prefers an explicit FLAPJACK_BACKEND_URL port over an env FJ_BACKEND_PORT', () => {
    // backendBaseUrl already derives unconditionally from FLAPJACK_BACKEND_URL, so
    // backendPort must follow the same URL port or the port half of the endpoint splits:
    // readiness probes the URL port while the spawn binds FJ_BACKEND_PORT. When both are
    // supplied the URL's explicit port is the single source of truth.
    vi.stubEnv('FLAPJACK_BACKEND_URL', 'http://127.0.0.1:17707');
    vi.stubEnv('FJ_BACKEND_PORT', '18893');
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);

    const config = getLocalInstanceConfig();

    expect(config.backendBaseUrl).toBe('http://127.0.0.1:17707');
    expect(config.backendPort).toBe(17707);
  });

  it('prefers an explicit FLAPJACK_BACKEND_URL port over a flapjack.local.conf FJ_BACKEND_PORT', () => {
    // Same precedence as the env variant, but FJ_BACKEND_PORT comes from the per-clone
    // config file, which normally beats env. A file-declared backend port must still lose
    // to an explicit URL port so per-clone config-file isolation cannot silently repoint
    // the readiness probe away from the spawned backend.
    vi.stubEnv('FLAPJACK_BACKEND_URL', 'http://127.0.0.1:17707');
    vi.spyOn(fs, 'existsSync').mockImplementation((target) => (
      String(target).endsWith('flapjack.local.conf')
    ));
    vi.spyOn(fs, 'readFileSync').mockImplementation((target) => {
      if (!String(target).endsWith('flapjack.local.conf')) {
        throw new Error(`Unexpected read target: ${String(target)}`);
      }

      return 'FJ_BACKEND_PORT=18893';
    });

    const config = getLocalInstanceConfig();

    expect(config.loadedFromFile).toBe(true);
    expect(config.backendBaseUrl).toBe('http://127.0.0.1:17707');
    expect(config.backendPort).toBe(17707);
  });

  it.each([
    { backendUrl: 'http://staging.example.com', expectedPort: 80 },
    { backendUrl: 'https://staging.example.com', expectedPort: 443 },
  ])('derives backendPort $expectedPort from portless $backendUrl', ({
    backendUrl,
    expectedPort,
  }) => {
    // URL.port is empty for both accepted portless protocols. Pin both branches so the
    // resolver must use the protocol default, never 0 / NaN / a hardcoded HTTPS port.
    vi.stubEnv('FLAPJACK_BACKEND_URL', backendUrl);
    vi.stubEnv('FJ_TEST_ADMIN_KEY', 'remote-admin-key');
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);

    const config = getLocalInstanceConfig();

    expect(config.backendBaseUrl).toBe(backendUrl);
    expect(config.backendPort).toBe(expectedPort);
  });

  it('honors a non-loopback FLAPJACK_BACKEND_URL host as the backend target host', () => {
    // `host` remains the dashboard bind owner used by Vite and dashboardBaseUrl. The
    // backend target host needs its own resolved field so a remote backend URL cannot
    // repoint the dashboard bind address or be silently collapsed back to loopback.
    vi.stubEnv('FLAPJACK_BACKEND_URL', 'http://192.168.1.50:17707');
    vi.stubEnv('FJ_TEST_ADMIN_KEY', 'remote-admin-key');
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);

    const config = getLocalInstanceConfig();

    expect(config).toMatchObject({
      backendBaseUrl: 'http://192.168.1.50:17707',
      backendHost: '192.168.1.50',
      host: '127.0.0.1',
      dashboardBaseUrl: 'http://127.0.0.1:5177',
    });
  });

  it('preserves a bracketed IPv6 host for a backend bind derived from an explicit URL', () => {
    vi.stubEnv('FLAPJACK_BACKEND_URL', 'http://[::1]:17707');
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);

    const config = getLocalInstanceConfig();

    expect(config).toMatchObject({
      backendBaseUrl: 'http://[::1]:17707',
      backendHost: '[::1]',
      backendPort: 17707,
    });
  });

  it('reads a flapjack.local.conf FLAPJACK_BACKEND_URL like every other shared routing key', () => {
    // Every shared routing key (FJ_HOST, FJ_BACKEND_PORT, FJ_DASHBOARD_PORT,
    // FJ_TEST_ADMIN_KEY) is read file-first, env-second, and parseLocalConfigFile already
    // parses FLAPJACK_BACKEND_URL out of a config file. A per-clone file must therefore be
    // able to declare the backend URL too; otherwise the value is silently dropped for
    // this one key while every sibling honors the file.
    vi.stubEnv('FLAPJACK_BACKEND_URL', undefined);
    vi.spyOn(fs, 'existsSync').mockImplementation((target) => (
      String(target).endsWith('flapjack.local.conf')
    ));
    vi.spyOn(fs, 'readFileSync').mockImplementation((target) => {
      if (!String(target).endsWith('flapjack.local.conf')) {
        throw new Error(`Unexpected read target: ${String(target)}`);
      }

      return 'FLAPJACK_BACKEND_URL=http://127.0.0.1:17707';
    });

    const config = getLocalInstanceConfig();

    expect(config.loadedFromFile).toBe(true);
    expect(config.backendBaseUrl).toBe('http://127.0.0.1:17707');
    expect(config.backendPort).toBe(17707);
  });

  it('uses FLAPJACK_BACKEND_URL origin and configured admin key for remote backends', () => {
    vi.stubEnv('FLAPJACK_BACKEND_URL', 'https://staging.example.com:9443/internal/status?x=1');
    vi.stubEnv('FJ_TEST_ADMIN_KEY', 'remote-admin-key');
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);

    const config = getLocalInstanceConfig();

    expect(config.backendBaseUrl).toBe('https://staging.example.com:9443');
    expect(config.adminKey).toBe('remote-admin-key');
  });

  it('uses FLAPJACK_ADMIN_KEY when no test-only admin key override is configured', () => {
    vi.stubEnv('FLAPJACK_ADMIN_KEY', 'runtime-admin-key');
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);

    const config = getLocalInstanceConfig();

    expect(config.backendBaseUrl).toBe('http://127.0.0.1:7700');
    expect(config.adminKey).toBe('runtime-admin-key');
  });

  it('does not reuse FLAPJACK_ADMIN_KEY for remote backends without an explicit test key', () => {
    vi.stubEnv('FLAPJACK_BACKEND_URL', 'https://staging.example.com:9443/internal/status');
    vi.stubEnv('FLAPJACK_ADMIN_KEY', 'runtime-admin-key');
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);

    expect(() => getLocalInstanceConfig()).toThrow(/FJ_TEST_ADMIN_KEY must be set/i);
  });

  it('normalizes dashboard URL host when FJ_HOST uses an unspecified bind address', () => {
    vi.stubEnv('FJ_HOST', '0.0.0.0');
    vi.stubEnv('FJ_TEST_ADMIN_KEY', 'bind-admin-key');
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);

    const config = getLocalInstanceConfig();

    // Keep the bind host untouched for Vite/server startup, but expose a loopback URL
    // for browser clients that cannot navigate to 0.0.0.0.
    expect(config.host).toBe('0.0.0.0');
    expect(config.adminKey).toBe('bind-admin-key');
    expect(config.dashboardBaseUrl).toBe('http://127.0.0.1:5177');
  });

  it('formats IPv6 loopback hosts as valid URL origins', () => {
    vi.stubEnv('FJ_HOST', '::1');
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);

    const config = getLocalInstanceConfig();

    expect(config.backendBaseUrl).toBe('http://[::1]:7700');
    expect(config.dashboardBaseUrl).toBe('http://[::1]:5177');
  });
});
