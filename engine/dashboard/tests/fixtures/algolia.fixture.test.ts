import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  assertAlgoliaCredentialsReady,
  AlgoliaCredentialSetupError,
  buildAlgoliaMigrationIndexName,
  classifyAlgoliaCredentialShape,
  hasAlgoliaCredentials,
  MissingAlgoliaCredentialsError,
  resolveAlgoliaCredentialMode,
  seedAlgoliaIndex,
} from './algolia.fixture';
import { API_BASE } from './local-instance';
import { PRODUCTS } from './test-data';

/**
 * `algoliasearch` and `fetch` are mocked so the readiness and seeding tests stay
 * unit tests: they must not require a live Flapjack backend or the real vendor.
 */
const algoliaClient = vi.hoisted(() => ({
  calls: [] as string[],
  failures: new Set<string>(),
  pendingTasks: new Set<number>(),
  nextTask: 1,
  indexName: '',
  metadataVisible: true,
  duplicateMetadata: false,
  record(name: string) {
    this.calls.push(name);
    if (this.failures.has(name)) {
      throw new Error(`${name} failed`);
    }
  },
  accept(name: string, indexName: string) {
    this.record(name);
    this.indexName = indexName;
    const taskID = this.nextTask++;
    this.pendingTasks.add(taskID);
    return { taskID };
  },
}));

vi.mock('algoliasearch', () => ({
  algoliasearch: () => ({
    setSettings: async ({ indexName }: { indexName: string }) => algoliaClient.accept('setSettings', indexName),
    saveSynonym: async ({ indexName }: { indexName: string }) => algoliaClient.accept('saveSynonym', indexName),
    saveRule: async ({ indexName }: { indexName: string }) => algoliaClient.accept('saveRule', indexName),
    saveObjects: async ({ indexName }: { indexName: string }) => [algoliaClient.accept('saveObjects', indexName)],
    waitForTask: async ({ taskID }: { taskID: number }) => {
      algoliaClient.record('waitForTask');
      if (!algoliaClient.pendingTasks.delete(taskID)) throw new Error('unknown task');
    },
    deleteIndex: async ({ indexName }: { indexName: string }) => algoliaClient.accept('deleteIndex', indexName),
    search: async () => {
      algoliaClient.record('search');
      return { results: [{ nbHits: Number.MAX_SAFE_INTEGER }] };
    },
  }),
}));

const SECRET_FILE_PATH = '/fixture/only/.env.secret';
const CLEAN_APP_ID = 'CLEANAPPID1';
const CLEAN_ADMIN_KEY = 'clean-admin-key-value';

function stubCleanCredentialEnv(): void {
  vi.stubEnv('FJ_SECRET_FILE', SECRET_FILE_PATH);
  vi.stubEnv('ALGOLIA_APP_ID', CLEAN_APP_ID);
  vi.stubEnv('ALGOLIA_ADMIN_KEY', CLEAN_ADMIN_KEY);
}

function stubReadinessResponse(status: number): ReturnType<typeof vi.fn> {
  const fetchMock = vi.fn(async () => {
    algoliaClient.record('readiness');
    const entry = { name: algoliaClient.indexName, entries: PRODUCTS.length };
    const indexes = algoliaClient.metadataVisible && algoliaClient.indexName && !algoliaClient.pendingTasks.size
      ? (algoliaClient.duplicateMetadata ? [entry, entry] : [entry]) : [];
    return { status, ok: status === 200, json: async () => ({ indexes }) } as unknown as Response;
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

beforeEach(() => {
  algoliaClient.calls.length = 0;
  algoliaClient.failures.clear();
  algoliaClient.pendingTasks.clear();
  algoliaClient.nextTask = 1;
  algoliaClient.indexName = '';
  algoliaClient.metadataVisible = true;
  algoliaClient.duplicateMetadata = false;
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});

describe('resolveAlgoliaCredentialMode', () => {
  it('runs when Algolia credentials are present', () => {
    expect(resolveAlgoliaCredentialMode({ hasCredentials: true })).toBe('run');
  });

  it('fails closed when Algolia credentials are missing', () => {
    expect(resolveAlgoliaCredentialMode({ hasCredentials: false })).toBe('fail');
  });

  it('names both required CI credentials in the fail-closed error', () => {
    const error = new MissingAlgoliaCredentialsError();

    expect(error.name).toBe('MissingAlgoliaCredentialsError');
    expect(error.message).toContain('ALGOLIA_APP_ID');
    expect(error.message).toContain('ALGOLIA_ADMIN_KEY');
  });
});

describe('hasAlgoliaCredentials', () => {
  it.each([
    ['neither credential', undefined, undefined, false],
    ['ALGOLIA_APP_ID only', 'test-app-id', undefined, false],
    ['ALGOLIA_ADMIN_KEY only', undefined, 'test-admin-key', false],
    ['both credentials', 'test-app-id', 'test-admin-key', true],
  ])('returns %s availability', (_label, appId, adminKey, expected) => {
    vi.stubEnv('ALGOLIA_APP_ID', appId);
    vi.stubEnv('ALGOLIA_ADMIN_KEY', adminKey);

    expect(hasAlgoliaCredentials()).toBe(expected);
  });

  it.each([
    ['whitespace-only ALGOLIA_APP_ID', '   ', 'test-admin-key'],
    ['whitespace-padded ALGOLIA_ADMIN_KEY', 'test-app-id', ' test-admin-key '],
  ])('still reports %s as present — truthiness cannot see credential shape', (
    _label,
    appId,
    adminKey,
  ) => {
    vi.stubEnv('ALGOLIA_APP_ID', appId);
    vi.stubEnv('ALGOLIA_ADMIN_KEY', adminKey);

    expect(hasAlgoliaCredentials()).toBe(true);
  });
});

describe('classifyAlgoliaCredentialShape', () => {
  it.each([
    ['a clean pair', CLEAN_APP_ID, CLEAN_ADMIN_KEY, 'ok'],
    ['a genuinely missing pair', undefined, undefined, 'missing'],
    ['a missing ALGOLIA_APP_ID', undefined, CLEAN_ADMIN_KEY, 'missing'],
    ['an empty-string ALGOLIA_ADMIN_KEY', CLEAN_APP_ID, '', 'missing'],
    ['a whitespace-only ALGOLIA_APP_ID', '   ', CLEAN_ADMIN_KEY, 'blank'],
    ['a whitespace-only ALGOLIA_ADMIN_KEY', CLEAN_APP_ID, '\t\n', 'blank'],
    ['a whitespace-padded ALGOLIA_ADMIN_KEY', CLEAN_APP_ID, ` ${CLEAN_ADMIN_KEY} `, 'padded'],
    ['a whitespace-padded ALGOLIA_APP_ID', `${CLEAN_APP_ID}\n`, CLEAN_ADMIN_KEY, 'padded'],
  ])('classifies %s as %s', (_label, appId, adminKey, expected) => {
    expect(classifyAlgoliaCredentialShape({ appId, adminKey })).toBe(expected);
  });

  it('reports the most severe verdict when both credentials are malformed', () => {
    expect(classifyAlgoliaCredentialShape({ appId: undefined, adminKey: '  key  ' })).toBe('missing');
    expect(classifyAlgoliaCredentialShape({ appId: '   ', adminKey: '  key  ' })).toBe('blank');
  });
});

describe('AlgoliaCredentialSetupError', () => {
  it('classifies as setup-infra and names the shape verdict, lengths, and credential source', () => {
    const error = new AlgoliaCredentialSetupError({
      shape: 'padded',
      appIdLength: CLEAN_APP_ID.length,
      adminKeyLength: CLEAN_ADMIN_KEY.length + 2,
      credentialSourcePath: SECRET_FILE_PATH,
    });

    expect(error.name).toBe('AlgoliaCredentialSetupError');
    expect(error.classification).toBe('setup-infra');
    expect(error.message).toContain('setup-infra');
    expect(error.message).toContain('padded');
    expect(error.message).toContain(SECRET_FILE_PATH);
    expect(error.message).toContain(`appIdLength=${CLEAN_APP_ID.length}`);
    expect(error.message).toContain(`adminKeyLength=${CLEAN_ADMIN_KEY.length + 2}`);
  });

  it('never leaks a credential value into the message', () => {
    const error = new AlgoliaCredentialSetupError({
      shape: 'ok',
      appIdLength: CLEAN_APP_ID.length,
      adminKeyLength: CLEAN_ADMIN_KEY.length,
      credentialSourcePath: SECRET_FILE_PATH,
      readinessStatus: 403,
    });

    expect(error.message).not.toContain(CLEAN_APP_ID);
    expect(error.message).not.toContain(CLEAN_ADMIN_KEY);
    expect(error.message).toContain('readinessStatus=403');
  });
});

describe('assertAlgoliaCredentialsReady', () => {
  it('rejects a bad credential shape before making any network call', async () => {
    vi.stubEnv('FJ_SECRET_FILE', SECRET_FILE_PATH);
    vi.stubEnv('ALGOLIA_APP_ID', CLEAN_APP_ID);
    vi.stubEnv('ALGOLIA_ADMIN_KEY', ` ${CLEAN_ADMIN_KEY} `);
    const fetchMock = stubReadinessResponse(200);

    const error = await assertAlgoliaCredentialsReady().catch((thrown: unknown) => thrown);

    expect(error).toBeInstanceOf(AlgoliaCredentialSetupError);
    expect((error as AlgoliaCredentialSetupError).shape).toBe('padded');
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('raises setup-infra with the HTTP status when the readiness probe is rejected', async () => {
    stubCleanCredentialEnv();
    stubReadinessResponse(403);

    const error = await assertAlgoliaCredentialsReady().catch((thrown: unknown) => thrown);

    expect(error).toBeInstanceOf(AlgoliaCredentialSetupError);
    const setupError = error as AlgoliaCredentialSetupError;
    expect(setupError.classification).toBe('setup-infra');
    expect(setupError.shape).toBe('ok');
    expect(setupError.readinessStatus).toBe(403);
    expect(setupError.message).toContain('setup-infra');
    expect(setupError.message).toContain(SECRET_FILE_PATH);
    expect(setupError.message).toContain('readinessStatus=403');
    expect(setupError.message).not.toContain(CLEAN_ADMIN_KEY);
  });

  it('wraps readiness transport failures as setup-infra without leaking credentials', async () => {
    stubCleanCredentialEnv();
    const transportError = new TypeError('fetch failed');
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(transportError));

    const error = await assertAlgoliaCredentialsReady().catch((thrown: unknown) => thrown);

    expect(error).toBeInstanceOf(AlgoliaCredentialSetupError);
    const setupError = error as AlgoliaCredentialSetupError;
    expect(setupError.classification).toBe('setup-infra');
    expect(setupError.shape).toBe('ok');
    expect(setupError.message).toContain(SECRET_FILE_PATH);
    expect(setupError.message).toContain(`appIdLength=${CLEAN_APP_ID.length}`);
    expect(setupError.message).toContain(`adminKeyLength=${CLEAN_ADMIN_KEY.length}`);
    expect(setupError.message).not.toContain(CLEAN_APP_ID);
    expect(setupError.message).not.toContain(CLEAN_ADMIN_KEY);
    expect((setupError as Error & { cause?: unknown }).cause).toBe(transportError);
  });

  it('probes the same backend route the Migrate screen uses', async () => {
    stubCleanCredentialEnv();
    const fetchMock = stubReadinessResponse(200);

    await expect(assertAlgoliaCredentialsReady()).resolves.toBeUndefined();

    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe(`${API_BASE}/1/algolia-list-indexes`);
    expect(init.method).toBe('POST');
    expect(JSON.parse(String(init.body))).toEqual({
      appId: CLEAN_APP_ID,
      apiKey: CLEAN_ADMIN_KEY,
    });
  });

  it('names the missing-secret-file condition when no secret file is configured', async () => {
    vi.stubEnv('FJ_NO_SECRET_FILE', '1');
    vi.stubEnv('ALGOLIA_APP_ID', CLEAN_APP_ID);
    vi.stubEnv('ALGOLIA_ADMIN_KEY', CLEAN_ADMIN_KEY);
    stubReadinessResponse(502);

    await expect(assertAlgoliaCredentialsReady()).rejects.toThrowError(/no secret file configured/);
  });
});

describe('seedAlgoliaIndex', () => {
  it('finishes every accepted native task before returning a migration source', async () => {
    stubCleanCredentialEnv();
    stubReadinessResponse(200);
    await seedAlgoliaIndex();
    expect(algoliaClient.pendingTasks.size).toBe(0);
  });

  it('uses the operator-owned throwaway prefix when supplied', () => {
    vi.stubEnv('ALGOLIA_TEST_INDEX_PREFIX', 'fj_e_');
    expect(buildAlgoliaMigrationIndexName(1700000000000, 42, 'abc-123'))
      .toBe('fj_e_1700000000000_42_abc-123');
  });

  it('rejects a prefix outside the owned fixture namespace', () => {
    vi.stubEnv('ALGOLIA_TEST_INDEX_PREFIX', 'unowned_');
    expect(() => buildAlgoliaMigrationIndexName()).toThrowError(/owned fj_ namespace/);
  });

  it('propagates failed native completion and failed rollback completion', async () => {
    stubCleanCredentialEnv();
    stubReadinessResponse(200);
    algoliaClient.failures.add('waitForTask');
    await expect(seedAlgoliaIndex()).rejects.toMatchObject({
      name: 'AggregateError',
      errors: [
        expect.objectContaining({ message: 'waitForTask failed' }),
        expect.objectContaining({ message: 'waitForTask failed' }),
      ],
    });
  });

  it.each(['missing', 'duplicated'])('refuses %s selected metadata even when documents are searchable', async (mode) => {
    stubCleanCredentialEnv();
    stubReadinessResponse(200);
    algoliaClient.metadataVisible = mode !== 'missing';
    algoliaClient.duplicateMetadata = mode === 'duplicated';
    await expect(seedAlgoliaIndex()).rejects.toThrowError('Algolia fixture source metadata is not ready');
    expect(algoliaClient.calls).toContain('deleteIndex');
  });

  it('includes process and random discriminators in vendor-backed index names', () => {
    expect(buildAlgoliaMigrationIndexName(1_700_000_000_000, 42, 'abc-123')).toBe(
      'fj_e2e_migrate_1700000000000_42_abc-123',
    );
    expect(buildAlgoliaMigrationIndexName(1_700_000_000_000, 43, 'abc-123')).not.toBe(
      buildAlgoliaMigrationIndexName(1_700_000_000_000, 42, 'abc-123'),
    );
    expect(buildAlgoliaMigrationIndexName(1_700_000_000_000, 42, 'def-456')).not.toBe(
      buildAlgoliaMigrationIndexName(1_700_000_000_000, 42, 'abc-123'),
    );
  });

  it('asserts credential readiness before writing any data to Algolia', async () => {
    stubCleanCredentialEnv();
    stubReadinessResponse(403);

    await expect(seedAlgoliaIndex()).rejects.toBeInstanceOf(AlgoliaCredentialSetupError);
    expect(algoliaClient.calls).toEqual(['readiness']);
  });

  it('seeds only after the readiness probe succeeds', async () => {
    stubCleanCredentialEnv();
    stubReadinessResponse(200);

    const ctx = await seedAlgoliaIndex();

    expect(algoliaClient.calls[0]).toBe('readiness');
    expect(algoliaClient.calls).toContain('saveObjects');
    expect(algoliaClient.calls.indexOf('readiness')).toBeLessThan(
      algoliaClient.calls.indexOf('setSettings'),
    );
    expect(ctx.appId).toBe(CLEAN_APP_ID);
    expect(ctx.targetIndexName).toBe(`${ctx.indexName}_target`);
  });

  it('deletes a partially seeded index before propagating the seed failure', async () => {
    stubCleanCredentialEnv();
    stubReadinessResponse(200);
    algoliaClient.failures.add('saveObjects');

    await expect(seedAlgoliaIndex()).rejects.toThrowError('saveObjects failed');
    expect(algoliaClient.calls).toContain('deleteIndex');
    expect(algoliaClient.calls.indexOf('deleteIndex')).toBeGreaterThan(
      algoliaClient.calls.indexOf('saveObjects'),
    );
  });

  it('reports both the seed and rollback failures when partial-seed cleanup fails', async () => {
    stubCleanCredentialEnv();
    stubReadinessResponse(200);
    algoliaClient.failures.add('saveObjects');
    algoliaClient.failures.add('deleteIndex');

    await expect(seedAlgoliaIndex()).rejects.toMatchObject({
      name: 'AggregateError',
      message: expect.stringContaining('rollback also failed'),
      errors: [
        expect.objectContaining({ message: 'saveObjects failed' }),
        expect.objectContaining({ message: 'deleteIndex failed' }),
      ],
    });
  });
});
