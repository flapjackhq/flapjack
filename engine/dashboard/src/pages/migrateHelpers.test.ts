import React from 'react';
import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import axios from 'axios';
import { MigrationErrorCard } from './MigrateSections';
import {
  MIGRATION_PROVIDER_DESCRIPTORS,
  buildAsyncMigrationViewState,
  buildDiscoveryRequestBody,
  buildMigrationRequestBody,
  getIndexListErrorMessage,
  getMigrationErrorMessage,
  getTerminalMigrationErrorMessage,
  formatMigrationPreviewEntryMeta,
  orderMigrationPreviewEntries,
  resolveEffectiveTargetIndex,
} from './migrateHelpers';

vi.mock('axios', () => ({
  default: {
    isAxiosError: vi.fn(),
  },
}));

describe('migrateHelpers', () => {
  const upstreamFailureMessage = 'Algolia rejected the supplied credentials.';
  const upstreamFailureCode = 'algolia_upstream_failure';

  it('exports one descriptor owner for every provider-varying migration string', () => {
    expect(MIGRATION_PROVIDER_DESCRIPTORS).toHaveLength(3);
    expect(MIGRATION_PROVIDER_DESCRIPTORS).toEqual([
      {
        id: 'algolia',
        routeSegment: 'algolia',
        displayName: 'Algolia',
        firstCredentialField: {
          label: 'Application ID',
          requestKey: 'appId',
          placeholder: 'YourAlgoliaAppId',
        },
        apiKeyLabel: 'Admin API Key',
        sourceFieldLabel: 'Source Index (Algolia)',
        connectionErrorMessage: 'Could not connect to Algolia. Check your App ID and API Key.',
        terminalFailureMessage:
          'Algolia upstream rejected the request or the migration failed before it completed.',
      },
      {
        id: 'meilisearch',
        routeSegment: 'meilisearch',
        displayName: 'Meilisearch',
        firstCredentialField: {
          label: 'Endpoint',
          requestKey: 'endpoint',
          placeholder: 'https://example.meilisearch.io',
        },
        apiKeyLabel: 'API Key',
        sourceFieldLabel: 'Source index',
        connectionErrorMessage: 'Could not connect to Meilisearch. Check your endpoint and API key.',
        terminalFailureMessage: 'Meilisearch migration failed before it completed.',
        loopbackOptInName: 'FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK',
      },
      {
        id: 'typesense',
        routeSegment: 'typesense',
        displayName: 'Typesense',
        firstCredentialField: {
          label: 'Node URL',
          requestKey: 'node',
          placeholder: 'https://example.typesense.net',
        },
        apiKeyLabel: 'API Key',
        sourceFieldLabel: 'Source collection',
        connectionErrorMessage: 'Could not connect to Typesense. Check your node URL and API key.',
        terminalFailureMessage: 'Typesense migration failed before it completed.',
        loopbackOptInName: 'FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK',
      },
    ]);

    for (const descriptor of MIGRATION_PROVIDER_DESCRIPTORS) {
      expect(descriptor).not.toHaveProperty('apiKey');
      expect(descriptor).not.toHaveProperty('sourceIndex');
      expect(descriptor).not.toHaveProperty('targetIndex');
      expect(descriptor).not.toHaveProperty('overwrite');
    }
  });

  it('builds descriptor-driven request bodies without optional fields unless needed', () => {
    expect(
      buildMigrationRequestBody({
        provider: MIGRATION_PROVIDER_DESCRIPTORS[0],
        firstCredentialValue: 'app',
        apiKey: 'key',
        sourceIndex: 'products',
        targetIndex: '',
        overwrite: false,
      }),
    ).toEqual({
      appId: 'app',
      apiKey: 'key',
      sourceIndex: 'products',
    });

    expect(
      buildMigrationRequestBody({
        provider: MIGRATION_PROVIDER_DESCRIPTORS[0],
        firstCredentialValue: 'app',
        apiKey: 'key',
        sourceIndex: 'products',
        targetIndex: 'products-copy',
        overwrite: true,
      }),
    ).toEqual({
      appId: 'app',
      apiKey: 'key',
      sourceIndex: 'products',
      targetIndex: 'products-copy',
      overwrite: true,
    });

    expect(
      buildMigrationRequestBody({
        provider: MIGRATION_PROVIDER_DESCRIPTORS[1],
        firstCredentialValue: 'https://meili.example',
        apiKey: 'key',
        sourceIndex: 'products',
        targetIndex: '',
        overwrite: false,
      }),
    ).toEqual({
      endpoint: 'https://meili.example',
      apiKey: 'key',
      sourceIndex: 'products',
    });

    expect(
      buildMigrationRequestBody({
        provider: MIGRATION_PROVIDER_DESCRIPTORS[2],
        firstCredentialValue: 'https://typesense.example',
        apiKey: 'key',
        sourceIndex: 'products',
        targetIndex: '',
        overwrite: false,
      }),
    ).toEqual({
      node: 'https://typesense.example',
      apiKey: 'key',
      sourceIndex: 'products',
    });
  });

  it('includes a write-freeze attestation only for checked Typesense requests', () => {
    const sharedRequest = {
      firstCredentialValue: 'credential',
      apiKey: 'key',
      sourceIndex: 'products',
      targetIndex: '',
      overwrite: false,
    };

    expect(buildMigrationRequestBody({
      ...sharedRequest,
      provider: MIGRATION_PROVIDER_DESCRIPTORS[2],
    })).not.toHaveProperty('sourceWriteFrozen');
    expect(buildMigrationRequestBody({
      ...sharedRequest,
      provider: MIGRATION_PROVIDER_DESCRIPTORS[2],
      sourceWriteFrozen: false,
    })).not.toHaveProperty('sourceWriteFrozen');
    expect(buildMigrationRequestBody({
      ...sharedRequest,
      provider: MIGRATION_PROVIDER_DESCRIPTORS[2],
      sourceWriteFrozen: true,
    })).toEqual({
      node: 'credential',
      apiKey: 'key',
      sourceIndex: 'products',
      sourceWriteFrozen: true,
    });

    for (const provider of MIGRATION_PROVIDER_DESCRIPTORS.slice(0, 2)) {
      expect(buildMigrationRequestBody({
        ...sharedRequest,
        provider,
        sourceWriteFrozen: true,
      })).not.toHaveProperty('sourceWriteFrozen');
    }
  });

  it('builds discovery credentials from the same provider descriptors as submit', () => {
    expect(
      MIGRATION_PROVIDER_DESCRIPTORS.map((provider) => (
        buildDiscoveryRequestBody(provider, 'credential-value', 'secret-key')
      )),
    ).toEqual([
      { appId: 'credential-value', apiKey: 'secret-key' },
      { endpoint: 'credential-value', apiKey: 'secret-key' },
      { node: 'credential-value', apiKey: 'secret-key' },
    ]);
  });

  it('classifies async migration status and extracts terminal import counts', () => {
    const running = buildAsyncMigrationViewState({
      jobId: 'job-1',
      phase: 'exporting',
      disposition: 'running',
      createdAt: '2026-08-06T12:00:00Z',
      updatedAt: '2026-08-06T12:00:01Z',
    });
    expect(running.kind).toBe('running');

    const succeeded = buildAsyncMigrationViewState({
      jobId: 'job-2',
      phase: 'activating',
      disposition: 'succeeded',
      targetIndex: 'products-copy',
      createdAt: '2026-08-06T12:00:00Z',
      updatedAt: '2026-08-06T12:00:05Z',
      terminalAt: '2026-08-06T12:00:05Z',
      settingsApplied: true,
      objectsImported: { imported: 37 },
      synonymsImported: { imported: 4 },
      rulesImported: { imported: 2 },
    });
    expect(succeeded).toEqual({
      kind: 'success',
      status: expect.objectContaining({
        jobId: 'job-2',
        disposition: 'succeeded',
      }),
      counts: {
        documents: 37,
        settings: true,
        synonyms: 4,
        rules: 2,
      },
    });

    expect(
      buildAsyncMigrationViewState({
        jobId: 'job-3',
        phase: 'exporting',
        disposition: 'failed',
        createdAt: '2026-08-06T12:00:00Z',
        updatedAt: '2026-08-06T12:00:03Z',
        terminalAt: '2026-08-06T12:00:03Z',
      }).kind,
    ).toBe('error');
    expect(
      buildAsyncMigrationViewState({
        jobId: 'job-4',
        phase: 'exporting',
        disposition: 'cancelled',
        createdAt: '2026-08-06T12:00:00Z',
        updatedAt: '2026-08-06T12:00:03Z',
        terminalAt: '2026-08-06T12:00:03Z',
      }).kind,
    ).toBe('error');
  });

  it('resolves the effective target index from target or source', () => {
    expect(resolveEffectiveTargetIndex('products', '')).toBe('products');
    expect(resolveEffectiveTargetIndex('products', 'products-copy')).toBe('products-copy');
  });

  it('prioritizes hard preview rejections without mutating backend entry order', () => {
    const entries = [
      {
        severity: 'Warning',
        code: 'warning',
        resource: 'Settings',
        pageIndex: null,
        itemIndex: null,
        jsonPath: '$.warning',
      },
      {
        severity: 'HardRejection',
        code: 'blocker',
        resource: 'Settings',
        pageIndex: 2,
        itemIndex: 4,
        jsonPath: '$.blocker',
      },
    ];

    expect(orderMigrationPreviewEntries(entries).map(({ code }) => code)).toEqual([
      'blocker',
      'warning',
    ]);
    expect(entries.map(({ code }) => code)).toEqual(['warning', 'blocker']);
  });

  it('formats only present preview page and item indexes as secondary metadata', () => {
    const baseEntry = {
      severity: 'Warning',
      code: 'warning',
      resource: 'Settings',
      jsonPath: '$.warning',
    };

    expect(formatMigrationPreviewEntryMeta({
      ...baseEntry,
      pageIndex: 2,
      itemIndex: 4,
    })).toBe('page 2, item 4');
    expect(formatMigrationPreviewEntryMeta({
      ...baseEntry,
      pageIndex: null,
      itemIndex: null,
    })).toBeNull();
  });

  it('formats conflict and native errors into user-facing migration messages', () => {
    vi.mocked(axios.isAxiosError).mockReturnValue(true as any);

    expect(
      getMigrationErrorMessage({
        response: { status: 409, data: {} },
      }),
    ).toBe('Target index already exists. Enable "Overwrite if exists" to replace it.');

    expect(
      getMigrationErrorMessage({
        response: { status: 500, data: { message: 'Boom' } },
      }),
    ).toBe('Boom');

    vi.mocked(axios.isAxiosError).mockReturnValue(false as any);
    expect(getMigrationErrorMessage(new Error('Plain error'))).toBe('Plain error');
  });

  it('passes through a coded nonempty backend error', () => {
    vi.mocked(axios.isAxiosError).mockReturnValue(true as any);

    expect(
      getMigrationErrorMessage({
        response: {
          status: 502,
          data: {
            message: upstreamFailureMessage,
            code: upstreamFailureCode,
          },
        },
      }),
    ).toBe(`${upstreamFailureMessage} Code: ${upstreamFailureCode}`);
  });

  it('redacts entered credentials and upstream URLs from boundary errors', () => {
    vi.mocked(axios.isAxiosError).mockReturnValue(true as any);

    expect(
      getMigrationErrorMessage(
        {
          response: {
            status: 502,
            data: {
              message:
                'Request to https://private.example:7700 failed with API key typesense-secret',
              code: 'upstream_unavailable',
            },
          },
        },
        MIGRATION_PROVIDER_DESCRIPTORS[2],
        ['https://private.example:7700', 'typesense-secret'],
      ),
    ).toBe(
      'Request to [redacted URL] failed with API key [redacted] Code: upstream_unavailable',
    );
  });

  it('uses connection guidance when a 502 response has no backend message', () => {
    vi.mocked(axios.isAxiosError).mockReturnValue(true as any);

    expect(
      getMigrationErrorMessage(
        {
          response: { status: 502, data: {} },
        },
        MIGRATION_PROVIDER_DESCRIPTORS[0],
      ),
    ).toBe('Could not connect to Algolia. Check your App ID and API Key.');

    expect(
      getMigrationErrorMessage(
        {
          response: { status: 502, data: {} },
        },
        MIGRATION_PROVIDER_DESCRIPTORS[1],
      ),
    ).toBe('Could not connect to Meilisearch. Check your endpoint and API key.');

    expect(
      getMigrationErrorMessage(
        {
          response: { status: 502, data: {} },
        },
        MIGRATION_PROVIDER_DESCRIPTORS[2],
      ),
    ).toBe('Could not connect to Typesense. Check your node URL and API key.');
  });

  it('adds provider-owned loopback opt-in guidance only for providers that need it', () => {
    vi.mocked(axios.isAxiosError).mockReturnValue(true as any);

    expect(
      getMigrationErrorMessage(
        {
          response: {
            status: 400,
            data: { message: 'Meilisearch Cloud endpoint is not allowed' },
          },
        },
        MIGRATION_PROVIDER_DESCRIPTORS[1],
      ),
    ).toBe(
      'Meilisearch Cloud endpoint is not allowed Set FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=1 to allow local preview fixtures.',
    );

    expect(
      getMigrationErrorMessage(
        {
          response: {
            status: 400,
            data: { message: 'Typesense Cloud endpoint is not allowed' },
          },
        },
        MIGRATION_PROVIDER_DESCRIPTORS[2],
      ),
    ).toContain('FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1');

    expect(
      getMigrationErrorMessage(
        {
          response: {
            status: 400,
            data: { message: 'Algolia appId is invalid' },
          },
        },
        MIGRATION_PROVIDER_DESCRIPTORS[0],
      ),
    ).not.toContain('FJ_ENABLE_');
  });

  it('renders the upstream failure message inside the migration error card', () => {
    vi.mocked(axios.isAxiosError).mockReturnValue(true as any);
    const errorMessage = getMigrationErrorMessage({
      response: {
        status: 502,
        data: {
          message: upstreamFailureMessage,
        },
      },
    });

    render(React.createElement(MigrationErrorCard, { errorMessage }));

    const errorCard = screen.getByTestId('migration-error-card');
    expect(errorCard).toHaveTextContent(upstreamFailureMessage);
  });

  it('maps forbidden index-list failures to the manual-entry guidance', () => {
    vi.mocked(axios.isAxiosError).mockReturnValue(true as any);
    expect(
      getIndexListErrorMessage({
        response: { status: 403, data: {} },
      }),
    ).toBe('API key does not have permission to list indexes. Type the index name manually.');
  });

  it('uses descriptor-owned context when an async migration fails without a backend reason', () => {
    const failedStatus = {
      jobId: 'job-failed',
      phase: 'exporting',
      disposition: 'failed' as const,
      createdAt: '2026-08-06T12:00:00Z',
      updatedAt: '2026-08-06T12:00:03Z',
      terminalAt: '2026-08-06T12:00:03Z',
    };

    expect(
      getTerminalMigrationErrorMessage(failedStatus, MIGRATION_PROVIDER_DESCRIPTORS[0]),
    ).toContain('Algolia upstream rejected the request');
    expect(
      getTerminalMigrationErrorMessage(failedStatus, MIGRATION_PROVIDER_DESCRIPTORS[1]),
    ).toBe('Meilisearch migration failed before it completed.');
    expect(
      getTerminalMigrationErrorMessage(failedStatus, MIGRATION_PROVIDER_DESCRIPTORS[2]),
    ).toBe('Typesense migration failed before it completed.');
  });
});
