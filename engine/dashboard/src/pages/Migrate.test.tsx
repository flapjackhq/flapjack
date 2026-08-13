import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import api from '@/lib/api';
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture';
import { Migrate } from './Migrate';
import type { AsyncMigrationStatusResponse, MigrationPreviewResponse } from './migrateHelpers';

const axiosMocks = vi.hoisted(() => ({
  post: vi.fn(),
}));

vi.mock('axios', () => ({
  default: {
    post: axiosMocks.post,
    isAxiosError: vi.fn(() => false),
  },
}));

vi.mock('@/lib/api', () => ({
  default: {
    get: vi.fn(),
  },
}));

function createMigrationStatus(
  overrides: Partial<AsyncMigrationStatusResponse> = {},
): AsyncMigrationStatusResponse {
  return {
    jobId: 'job-final-poll',
    phase: 'exporting',
    disposition: 'running',
    targetIndex: 'products-copy',
    createdAt: '2026-08-06T12:00:00Z',
    updatedAt: '2026-08-06T12:00:01Z',
    ...overrides,
  };
}

function createMigrationPreview(
  overrides: Partial<MigrationPreviewResponse> = {},
): MigrationPreviewResponse {
  return {
    report: {
      entries: [],
      summary: {
        totalEntries: 0,
        hardRejections: 0,
        warnings: 0,
        scopeGaps: 0,
      },
      reportDigest: null,
    },
    sourceCounts: {
      indexes: 1,
      records: 37,
    },
    ...overrides,
  };
}

function renderMigrate() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  });

  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter future={TEST_ROUTER_FUTURE}>
        <Migrate />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe('Migrate', () => {
  beforeEach(() => {
    axiosMocks.post.mockReset();
    vi.mocked(api.get).mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('classifies terminal success returned by the 120th status poll', async () => {
    vi.useFakeTimers();
    axiosMocks.post
      .mockResolvedValueOnce({ data: createMigrationPreview() })
      .mockResolvedValueOnce({ data: createMigrationStatus({ phase: 'queued' }) });
    vi.mocked(api.get)
      .mockResolvedValueOnce({ data: createMigrationStatus({ phase: 'exporting' }) })
      .mockResolvedValueOnce({ data: createMigrationStatus({ phase: 'importing' }) });
    for (let pollNumber = 3; pollNumber < 120; pollNumber += 1) {
      vi.mocked(api.get).mockResolvedValueOnce({
        data: createMigrationStatus({ phase: 'importing' }),
      });
    }
    vi.mocked(api.get).mockResolvedValueOnce({
      data: createMigrationStatus({
        phase: 'activating',
        disposition: 'succeeded',
        terminalAt: '2026-08-06T12:01:30Z',
        settingsApplied: true,
        objectsImported: { imported: 37 },
        synonymsImported: { imported: 4 },
        rulesImported: { imported: 2 },
      }),
    });

    renderMigrate();

    fireEvent.change(screen.getByLabelText('Application ID'), {
      target: { value: 'algolia-app' },
    });
    fireEvent.change(screen.getByLabelText('Admin API Key'), {
      target: { value: 'algolia-key' },
    });
    fireEvent.change(screen.getByLabelText('Source Index (Algolia)'), {
      target: { value: 'products' },
    });
    fireEvent.change(screen.getByLabelText(/Target Index \(Flapjack\)/), {
      target: { value: 'products-copy' },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /preview migration/i }));
      await vi.runOnlyPendingTimersAsync();
    });
    const submitButton = screen.getByRole('button', { name: /^submit migration$/i });
    await act(async () => {
      fireEvent.click(submitButton);
    });

    for (let pollNumber = 0; pollNumber < 120; pollNumber += 1) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(750);
      });
    }

    await act(async () => {});

    expect(screen.getByText('Migration complete')).toBeInTheDocument();
    expect(screen.getByTestId('migrate-stat-documents')).toHaveTextContent('37');
    expect(screen.queryByTestId('migration-error-card')).not.toBeInTheDocument();
    expect(api.get).toHaveBeenCalledTimes(120);
  });

  it('keeps polling a running migration beyond the old 90-second boundary', async () => {
    vi.useFakeTimers();
    axiosMocks.post
      .mockResolvedValueOnce({ data: createMigrationPreview() })
      .mockResolvedValueOnce({ data: createMigrationStatus({ phase: 'queued' }) });
    for (let pollNumber = 1; pollNumber <= 120; pollNumber += 1) {
      vi.mocked(api.get).mockResolvedValueOnce({
        data: createMigrationStatus({ phase: 'importing' }),
      });
    }
    vi.mocked(api.get).mockResolvedValueOnce({
      data: createMigrationStatus({
        phase: 'activating',
        disposition: 'succeeded',
        terminalAt: '2026-08-06T12:01:31Z',
        objectsImported: { imported: 41 },
      }),
    });

    renderMigrate();

    fireEvent.change(screen.getByLabelText('Application ID'), {
      target: { value: 'algolia-app' },
    });
    fireEvent.change(screen.getByLabelText('Admin API Key'), {
      target: { value: 'algolia-key' },
    });
    fireEvent.change(screen.getByLabelText('Source Index (Algolia)'), {
      target: { value: 'products' },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /preview migration/i }));
      await vi.runOnlyPendingTimersAsync();
    });
    const submitButton = screen.getByRole('button', { name: /^submit migration$/i });
    await act(async () => {
      fireEvent.click(submitButton);
    });

    for (let pollNumber = 0; pollNumber < 121; pollNumber += 1) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(750);
      });
    }
    await act(async () => {});

    expect(screen.getByText('Migration complete')).toBeInTheDocument();
    expect(screen.getByTestId('migrate-stat-documents')).toHaveTextContent('41');
    expect(screen.queryByTestId('migration-error-card')).not.toBeInTheDocument();
    expect(api.get).toHaveBeenCalledTimes(121);
  });

  it('renders provider discovery metadata from the full response envelope', async () => {
    axiosMocks.post.mockResolvedValue({
      data: {
        indexes: [
          {
            name: 'products',
            documentCount: 37,
            defaultSortingField: 'popularity',
          },
        ],
        total: 91,
        offset: 20,
        limit: 10,
      },
    });

    renderMigrate();

    fireEvent.click(screen.getByRole('button', { name: 'Typesense' }));
    fireEvent.change(screen.getByLabelText('Node URL'), {
      target: { value: 'https://typesense.example' },
    });
    fireEvent.change(screen.getByLabelText('API Key'), {
      target: { value: 'typesense-key' },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Discover sources' }));
    });
    await act(async () => {});

    const sourceOption = await screen.findByTestId('migration-source-option-products');
    expect(sourceOption).toHaveTextContent('37 records');
    expect(sourceOption).toHaveTextContent('sort: popularity');
    expect(screen.getByTestId('migration-source-pagination')).toHaveTextContent(
      'Showing 21–30 of 91',
    );
  });

  it('requires an explicit Typesense write freeze without changing other providers', async () => {
    axiosMocks.post.mockResolvedValue({ data: createMigrationPreview() });
    renderMigrate();

    fireEvent.change(screen.getByLabelText('Application ID'), {
      target: { value: 'algolia-app' },
    });
    fireEvent.change(screen.getByLabelText('Admin API Key'), {
      target: { value: 'algolia-key' },
    });
    fireEvent.change(screen.getByLabelText('Source Index (Algolia)'), {
      target: { value: 'products' },
    });
    expect(screen.getByRole('button', { name: /preview migration/i })).toBeEnabled();
    expect(screen.queryByTestId('typesense-source-write-frozen')).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Meilisearch' }));
    fireEvent.change(screen.getByLabelText('Endpoint'), {
      target: { value: 'https://meilisearch.example' },
    });
    fireEvent.change(screen.getByLabelText('API Key'), {
      target: { value: 'meilisearch-key' },
    });
    fireEvent.change(screen.getByLabelText('Source index'), {
      target: { value: 'products' },
    });
    expect(screen.getByRole('button', { name: /preview migration/i })).toBeEnabled();
    expect(screen.queryByTestId('typesense-source-write-frozen')).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Typesense' }));
    fireEvent.change(screen.getByLabelText('Node URL'), {
      target: { value: 'https://typesense.example' },
    });
    fireEvent.change(screen.getByLabelText('API Key'), {
      target: { value: 'typesense-key' },
    });
    fireEvent.change(screen.getByLabelText('Source collection'), {
      target: { value: 'products' },
    });

    const attestation = screen.queryByTestId('typesense-source-write-frozen');
    if (!attestation) {
      console.error('WRITE_FREEZE_UI_RED=unchecked_control_did_not_block_preview_or_submit');
      expect(attestation, 'Typesense write-freeze checkbox must be rendered').not.toBeNull();
      return;
    }

    expect(attestation).toHaveAccessibleName(
      /I have paused writes to the selected Typesense collection for the complete migration/i,
    );
    expect(attestation).not.toBeChecked();
    expect(screen.getByRole('button', { name: /preview migration/i })).toBeDisabled();

    fireEvent.click(attestation);
    fireEvent.change(screen.getByLabelText('Source collection'), {
      target: { value: 'products_v2' },
    });
    expect(attestation).not.toBeChecked();
    fireEvent.click(attestation);
    fireEvent.change(screen.getByLabelText('API Key'), {
      target: { value: 'rotated-typesense-key' },
    });
    expect(attestation).not.toBeChecked();
    fireEvent.click(attestation);
    fireEvent.change(screen.getByLabelText('Node URL'), {
      target: { value: 'https://replacement.typesense.example' },
    });
    expect(attestation).not.toBeChecked();
    fireEvent.click(attestation);
    const previewButton = screen.getByRole('button', { name: /preview migration/i });
    expect(previewButton).toBeEnabled();
    await act(async () => {
      fireEvent.click(previewButton);
    });
    const submitButton = await screen.findByRole('button', { name: /^submit migration$/i });
    expect(submitButton).toBeEnabled();

    fireEvent.click(attestation);
    expect(screen.queryByRole('button', { name: /^submit migration$/i })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /preview migration/i })).toBeDisabled();
  });

  it('makes editing the source the primary action after preview refusal', async () => {
    axiosMocks.post.mockRejectedValueOnce(
      new Error('Meilisearch preview loopback endpoint is disabled'),
    );
    renderMigrate();

    fireEvent.click(screen.getByRole('button', { name: 'Meilisearch' }));
    fireEvent.change(screen.getByLabelText('Endpoint'), {
      target: { value: 'http://127.0.0.1:7700' },
    });
    fireEvent.change(screen.getByLabelText('API Key'), {
      target: { value: 'meilisearch-key' },
    });
    const sourceInput = screen.getByLabelText('Source index');
    fireEvent.change(sourceInput, { target: { value: 'products' } });

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /preview migration/i }));
    });

    const editSourceButton = await screen.findByRole('button', { name: 'Edit source' });
    fireEvent.click(editSourceButton);
    expect(sourceInput).toHaveFocus();
    expect(screen.getByTestId('migration-error-card')).toHaveTextContent(
      'Meilisearch preview loopback endpoint is disabled',
    );
  });
});
