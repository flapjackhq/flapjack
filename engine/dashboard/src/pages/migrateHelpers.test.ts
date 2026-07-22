import React from 'react';
import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import axios from 'axios';
import { MigrationErrorCard } from './MigrateSections';
import {
  buildMigrationRequestBody,
  getIndexListErrorMessage,
  getMigrationErrorMessage,
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

  it('builds migration request bodies without optional fields unless needed', () => {
    expect(
      buildMigrationRequestBody({
        appId: 'app',
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
        appId: 'app',
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
  });

  it('resolves the effective target index from target or source', () => {
    expect(resolveEffectiveTargetIndex('products', '')).toBe('products');
    expect(resolveEffectiveTargetIndex('products', 'products-copy')).toBe('products-copy');
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

  it('uses connection guidance when a 502 response has no backend message', () => {
    vi.mocked(axios.isAxiosError).mockReturnValue(true as any);

    expect(
      getMigrationErrorMessage({
        response: { status: 502, data: {} },
      }),
    ).toBe('Could not connect to Algolia. Check your App ID and API Key.');
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
});
