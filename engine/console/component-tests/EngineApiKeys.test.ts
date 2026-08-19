import '@testing-library/jest-dom/vitest';
import { fireEvent, render, screen, within } from '@testing-library/svelte';
import { beforeAll, describe, expect, it, vi } from 'vitest';
import EngineApiKeys from '../src/host/EngineApiKeys.svelte';
import type {
  ConsoleTransport,
  EngineApiKey,
  EngineApiKeysCapability,
} from '../src/lib/transport/console_transport';

const productsKey: EngineApiKey = {
  value: 'engine-secret-products',
  createdAt: Date.UTC(2026, 7, 17),
  acl: ['search', 'browse'],
  description: 'Products browser key',
  indexes: ['products'],
  maxHitsPerQuery: 50,
  maxQueriesPerIPPerHour: 500,
  queryParameters: 'typoTolerance=false',
  referers: ['https://example.test/*'],
  restrictSources: ['10.0.0.0/8'],
  validity: 3_600,
};

beforeAll(() => {
  if (!HTMLDialogElement.prototype.showModal) {
    HTMLDialogElement.prototype.showModal = function showModal() {
      this.open = true;
    };
  }
  if (!HTMLDialogElement.prototype.close) {
    HTMLDialogElement.prototype.close = function close() {
      this.open = false;
    };
  }
});

function fixture(initialKeys: EngineApiKey[] = [productsKey]) {
  let listedKeys = initialKeys;
  const apiKeys: EngineApiKeysCapability = {
    kind: 'engine',
    list: vi.fn(async () => listedKeys),
    create: vi.fn(async () => {
      listedKeys = [
        ...listedKeys,
        {
          ...productsKey,
          value: 'new-engine-secret',
          description: 'New browser key',
          acl: ['search', 'analytics'],
          indexes: ['articles'],
          restrictSources: ['192.168.0.0/16'],
          validity: 7_200,
          maxHitsPerQuery: 25,
          maxQueriesPerIPPerHour: 250,
          queryParameters: 'hitsPerPage=10',
          referers: ['https://app.example/*'],
        },
      ];
      return { key: 'new-engine-secret', createdAt: '2026-08-17T07:00:00Z' };
    }),
    remove: vi.fn(async (value: string) => {
      listedKeys = listedKeys.filter((key) => key.value !== value);
    }),
  };
  const transport: ConsoleTransport = {
    apiKeys,
    listIndexes: vi.fn(async () => [
      { name: 'articles', entries: 2, dataSize: 20 },
      { name: 'products', entries: 1, dataSize: 10 },
    ]),
    searchIndex: vi.fn(async () => {
      throw new Error('not used');
    }),
  };
  const copyText = vi.fn(async () => undefined);
  return { apiKeys, copyText, transport };
}

describe('standalone engine API Keys composition', () => {
  it('loads and labels the engine key domain without an implicit mutation', async () => {
    const { apiKeys, transport } = fixture();
    render(EngineApiKeys, { props: { transport } });

    const card = await screen.findByRole('article', { name: 'Products browser key' });
    expect(within(card).getByText('engine-secret-products')).toBeInTheDocument();
    expect(within(card).getByText('search, browse')).toBeInTheDocument();
    expect(within(card).getByText('products')).toBeInTheDocument();
    expect(within(card).getByText('10.0.0.0/8')).toBeInTheDocument();
    expect(within(card).getByText('1 hour')).toBeInTheDocument();
    expect(within(card).getByText('2026-08-17')).toHaveAttribute(
      'datetime',
      '2026-08-17T00:00:00.000Z'
    );
    expect(within(card).getByText('50 hits/query')).toBeInTheDocument();
    expect(within(card).getByText('500 queries/IP/hour')).toBeInTheDocument();
    expect(within(card).getByText('typoTolerance=false')).toBeInTheDocument();
    expect(within(card).getByText('https://example.test/*')).toBeInTheDocument();
    expect(apiKeys.list).toHaveBeenCalledOnce();
    expect(transport.listIndexes).toHaveBeenCalledOnce();
    expect(apiKeys.create).not.toHaveBeenCalled();
    expect(apiKeys.remove).not.toHaveBeenCalled();
  });

  it('creates the exact engine-domain request and refreshes only the key list', async () => {
    const { apiKeys, transport } = fixture();
    render(EngineApiKeys, { props: { transport } });
    await screen.findByRole('article', { name: 'Products browser key' });

    const createButton = screen.getByRole('button', { name: 'Create API Key' });
    createButton.focus();
    await fireEvent.click(createButton);
    let dialog = screen.getByRole('dialog', { name: 'Create engine API key' });
    expect(within(dialog).getByLabelText('Description')).toHaveFocus();
    await fireEvent(dialog, new Event('cancel', { cancelable: true }));
    expect(createButton).toHaveFocus();

    await fireEvent.click(createButton);
    dialog = screen.getByRole('dialog', { name: 'Create engine API key' });
    await fireEvent.input(within(dialog).getByLabelText('Description'), {
      target: { value: 'New browser key' },
    });
    await fireEvent.change(within(dialog).getByLabelText('analytics'), {
      target: { checked: true },
    });
    await fireEvent.click(within(dialog).getByLabelText('Index articles'));
    await fireEvent.input(within(dialog).getByLabelText('Restrict sources'), {
      target: { value: '192.168.0.0/16' },
    });
    await fireEvent.input(within(dialog).getByLabelText('Max hits per query'), {
      target: { value: '25' },
    });
    await fireEvent.input(within(dialog).getByLabelText('Max queries per IP per hour'), {
      target: { value: '250' },
    });
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Create key' }));

    await vi.waitFor(() => {
      expect(apiKeys.create).toHaveBeenCalledExactlyOnceWith({
        acl: ['search', 'analytics'],
        description: 'New browser key',
        indexes: ['articles'],
        restrictSources: ['192.168.0.0/16'],
        maxHitsPerQuery: 25,
        maxQueriesPerIPPerHour: 250,
      });
    });
    expect(await screen.findByRole('article', { name: 'New browser key' })).toBeInTheDocument();
    expect(createButton).toHaveFocus();
    expect(apiKeys.list).toHaveBeenCalledTimes(2);
    expect(transport.listIndexes).toHaveBeenCalledOnce();
  });

  it('copies the full engine value and keeps cancel/confirm removal state in the host', async () => {
    const { apiKeys, copyText, transport } = fixture();
    render(EngineApiKeys, { props: { transport, copyText } });
    const card = await screen.findByRole('article', { name: 'Products browser key' });

    await fireEvent.click(within(card).getByRole('button', { name: 'Copy Products browser key' }));
    expect(copyText).toHaveBeenCalledExactlyOnceWith('engine-secret-products');

    const deleteButton = within(card).getByRole('button', {
      name: 'Delete Products browser key',
    });
    await fireEvent.click(deleteButton);
    let dialog = screen.getByRole('dialog', { name: 'Delete engine API key' });
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(apiKeys.remove).not.toHaveBeenCalled();
    expect(deleteButton).toHaveFocus();

    await fireEvent.click(deleteButton);
    dialog = screen.getByRole('dialog', { name: 'Delete engine API key' });
    let rejectRemove: (error: Error) => void = () => undefined;
    vi.mocked(apiKeys.remove).mockImplementationOnce(
      () =>
        new Promise<void>((_resolve, reject) => {
          rejectRemove = reject;
        })
    );
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Delete key' }));
    expect(within(dialog).getByRole('button', { name: 'Deleting…' })).toBeDisabled();
    expect(within(dialog).getByRole('button', { name: 'Cancel' })).toBeDisabled();
    await fireEvent(dialog, new Event('cancel', { cancelable: true }));
    expect(screen.getByRole('dialog', { name: 'Delete engine API key' })).toBeInTheDocument();
    rejectRemove(new Error('offline'));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent('Could not delete API key.');
    expect(apiKeys.remove).toHaveBeenNthCalledWith(1, 'engine-secret-products');

    await fireEvent.click(within(dialog).getByRole('button', { name: 'Delete key' }));
    expect(apiKeys.remove).toHaveBeenNthCalledWith(2, 'engine-secret-products');
    expect(await screen.findByText('No API keys yet.')).toBeInTheDocument();
    expect(screen.getByRole('region', { name: 'API Keys screen' })).toHaveFocus();
    expect(screen.queryByRole('dialog', { name: 'Delete engine API key' })).not.toBeInTheDocument();
  });

  it('keeps invalid and rejected creation inside the host dialog', async () => {
    const { apiKeys, transport } = fixture();
    render(EngineApiKeys, { props: { transport } });
    await screen.findByRole('article', { name: 'Products browser key' });
    const createButton = screen.getByRole('button', { name: 'Create API Key' });
    await fireEvent.click(createButton);
    let dialog = screen.getByRole('dialog', { name: 'Create engine API key' });

    await fireEvent.click(within(dialog).getByLabelText('search'));
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Create key' }));
    expect(within(dialog).getByRole('alert')).toHaveTextContent('Select at least one permission.');
    expect(apiKeys.create).not.toHaveBeenCalled();

    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await fireEvent.click(createButton);
    dialog = screen.getByRole('dialog', { name: 'Create engine API key' });
    await fireEvent.input(within(dialog).getByLabelText('Max hits per query'), {
      target: { value: '0' },
    });
    await fireEvent.submit(
      within(dialog).getByRole('button', { name: 'Create key' }).closest('form')!
    );
    expect(within(dialog).getByRole('alert')).toHaveTextContent(
      'Max hits per query must be a positive integer.'
    );
    expect(apiKeys.create).not.toHaveBeenCalled();

    await fireEvent.input(within(dialog).getByLabelText('Max hits per query'), {
      target: { value: '1' },
    });
    vi.mocked(apiKeys.create).mockRejectedValueOnce(new Error('Could not create API key'));
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Create key' }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent('Could not create API key');
    expect(apiKeys.create).toHaveBeenCalledOnce();
    expect(screen.getByRole('dialog', { name: 'Create engine API key' })).toBeInTheDocument();
  });

  it('surfaces load failure and retries without manufacturing an empty list', async () => {
    const { apiKeys, transport } = fixture();
    vi.mocked(apiKeys.list)
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce([productsKey]);
    render(EngineApiKeys, { props: { transport } });

    expect(await screen.findByRole('alert')).toHaveTextContent('Could not load API keys.');
    expect(screen.queryByText('No API keys yet.')).not.toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(await screen.findByRole('article', { name: 'Products browser key' })).toBeInTheDocument();
    expect(apiKeys.list).toHaveBeenCalledTimes(2);
  });
});
