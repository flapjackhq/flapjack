import type { APIRequestContext } from '@playwright/test';
import { describe, expect, it, vi } from 'vitest';
import { API_BASE, API_HEADERS } from './local-instance';
import {
  assertIndexNotCreated,
  parseSourceProviderFixture,
  startMeilisearchSource,
  type SourceProviderFixtureControl,
} from './source-provider.fixture';

function indexListRequest(body: unknown, status = 200): APIRequestContext {
  return {
    get: vi.fn().mockResolvedValue({
      ok: () => status >= 200 && status < 300,
      status: () => status,
      text: () => Promise.resolve(JSON.stringify(body)),
      json: () => Promise.resolve(body),
    }),
  } as unknown as APIRequestContext;
}

describe('assertIndexNotCreated', () => {
  it('accepts a valid index list only when the dry-run target is absent', async () => {
    const request = indexListRequest({
      items: [
        { name: 'existing-products' },
        { uid: 'existing-orders' },
      ],
    });

    await expect(assertIndexNotCreated(request, 'dry-run-target')).resolves.toBeUndefined();
    expect(request.get).toHaveBeenCalledWith(`${API_BASE}/1/indexes`, {
      headers: API_HEADERS,
    });
  });

  it.each([
    ['name', { items: [{ name: 'dry-run-target' }] }],
    ['uid', { results: [{ uid: 'dry-run-target' }] }],
  ])('rejects a dry-run target present by %s-compatible identity', async (_identity, body) => {
    await expect(assertIndexNotCreated(
      indexListRequest(body),
      'dry-run-target',
    )).rejects.toThrow('Dry-run preview created target index "dry-run-target"');
  });

  it.each([
    ['missing items', {}],
    ['non-array items', { items: 'not-an-array' }],
  ])('fails closed for malformed list payload: %s', async (_case, body) => {
    await expect(assertIndexNotCreated(
      indexListRequest(body),
      'dry-run-target',
    )).rejects.toThrow('invalid index-list response');
  });

  it('fails closed when the index-list request is unsuccessful', async () => {
    await expect(assertIndexNotCreated(
      indexListRequest({ message: 'unavailable' }, 503),
      'dry-run-target',
    )).rejects.toThrow('index-list probe failed (503)');
  });
});

describe('parseSourceProviderFixture', () => {
  it.each([
    [
      'Meilisearch',
      JSON.stringify({
        port: 49101,
        apiKey: 'meili-test-key',
        sourceName: 'configured_pk',
        container: 'fj_source_migration_provider_parity_meili_123',
        fixtureDir: '/tmp/fj_source_provider_fixture_meilisearch_123',
        ownershipToken: 'meilisearch-token',
        seededDocumentCount: 2,
        seededIds: ['MEILI-001', 'MEILI-002'],
      }),
      'configured_pk',
      ['MEILI-001', 'MEILI-002'],
    ],
    [
      'Typesense',
      JSON.stringify({
        port: 49102,
        apiKey: 'typesense-test-key',
        sourceName: 'fj_ts_migration_products',
        container: 'fj_source_migration_provider_parity_typesense_456',
        fixtureDir: '/tmp/fj_source_provider_fixture_typesense_456',
        ownershipToken: 'typesense-token',
        seededDocumentCount: 2,
        seededIds: ['prod_1', 'prod_2'],
      }),
      'fj_ts_migration_products',
      ['prod_1', 'prod_2'],
    ],
  ])('returns the hand-known %s seed contract', (provider, payload, sourceName, seededIds) => {
    expect(parseSourceProviderFixture(payload, provider)).toEqual(expect.objectContaining({
      sourceName,
      seededDocumentCount: 2,
      seededIds,
    }));
  });

  it.each([
    ['', 'empty output'],
    ['not-json', 'malformed JSON'],
    [JSON.stringify({ port: 49101 }), 'missing fields'],
    [JSON.stringify({
      port: 49101,
      apiKey: 'key',
      sourceName: 'configured_pk',
      container: 'container',
      fixtureDir: '/tmp/fj_source_provider_fixture_meilisearch_123',
      seededDocumentCount: 0,
      seededIds: [],
    }), 'incorrect seeded count'],
    [JSON.stringify({
      port: 49101,
      apiKey: 'key',
      sourceName: 'configured_pk',
      container: 'fj_source_migration_provider_parity_meili_123',
      fixtureDir: '/tmp/fj_source_provider_fixture_meilisearch_123',
      seededDocumentCount: 2,
      seededIds: ['MEILI-001', 'MEILI-002'],
    }), 'missing ownership token'],
  ])('rejects %s rather than defaulting to ready', (payload) => {
    expect(() => parseSourceProviderFixture(payload, 'Meilisearch')).toThrow();
  });
});

describe('startMeilisearchSource', () => {
  it.each([
    ['', 'malformed JSON'],
    ['not-json', 'malformed JSON'],
    [JSON.stringify({ port: 49101 }), 'an invalid readiness contract'],
  ])('cleans the independently receipted fixture when readiness output is %s', async (
    readinessOutput,
    expectedError,
  ) => {
    const cleanupReceipt = {
      provider: 'meilisearch' as const,
      container: 'fj_source_migration_provider_parity_meili_123',
      fixtureDir: '/tmp/fj_source_provider_fixture_meilisearch_123',
      ownershipToken: 'meilisearch-token',
    };
    const fixtureControl = vi.fn<SourceProviderFixtureControl>()
      .mockResolvedValueOnce({
        stdout: readinessOutput,
        stderr: `SOURCE_PROVIDER_CLEANUP_RECEIPT=${JSON.stringify(cleanupReceipt)}\n`,
      })
      .mockResolvedValueOnce({ stdout: '{"removed":true}', stderr: '' });

    await expect(startMeilisearchSource(
      {} as APIRequestContext,
      fixtureControl,
    )).rejects.toThrow(`meilisearch fixture ctl returned ${expectedError}`);

    expect(fixtureControl).toHaveBeenNthCalledWith(1, 'up', 'meilisearch');
    expect(fixtureControl).toHaveBeenNthCalledWith(2, 'down', 'meilisearch', cleanupReceipt);
  });
});
