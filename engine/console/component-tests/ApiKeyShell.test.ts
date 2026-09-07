import '@testing-library/jest-dom/vitest';
import { fireEvent, render, screen, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import ApiKeyShellHarness from './fixtures/ApiKeyShellHarness.svelte';
import * as apiKeyShellStories from '../src/lib/features/ApiKeyShell.stories';

const readyKeys = [
  {
    opaqueId: 'engine-secret-global',
    displayName: 'Global browser key',
    indexNames: [],
    copyText: 'engine-secret-global',
  },
  {
    opaqueId: 'engine-secret-products',
    displayName: 'Products browser key',
    indexNames: ['products'],
    copyText: 'engine-secret-products',
  },
  {
    opaqueId: 'engine-secret-articles',
    displayName: 'Articles browser key',
    indexNames: ['articles'],
    copyText: 'engine-secret-articles',
  },
];

afterEach(() => {
  vi.useRealTimers();
});

describe('shared API key interaction shell', () => {
  it('renders controlled loading and error states and retries exactly once', async () => {
    const onRetry = vi.fn();
    const { rerender } = render(ApiKeyShellHarness, {
      props: { state: { kind: 'loading' }, onRetry },
    });

    expect(screen.getByRole('status')).toHaveTextContent('Loading API keys…');
    expect(screen.queryByRole('button', { name: 'Retry' })).not.toBeInTheDocument();
    expect(screen.queryByText('No API keys yet.')).not.toBeInTheDocument();

    await rerender({ state: { kind: 'error', message: 'Keys are unavailable.' }, onRetry });
    expect(screen.getByRole('alert')).toHaveTextContent('Keys are unavailable.');
    const retryButton = screen.getByRole('button', { name: 'Retry' });
    expect(retryButton).toHaveAttribute('data-variant', 'secondary');
    await fireEvent.click(retryButton);
    expect(onRetry).toHaveBeenCalledOnce();
  });

  it('shows the controlled empty state and emits the create intent', async () => {
    const onCreate = vi.fn();
    render(ApiKeyShellHarness, {
      props: { state: { kind: 'ready', keys: [] }, onCreate },
    });

    expect(screen.getByRole('heading', { name: 'API Keys' })).toBeInTheDocument();
    expect(screen.getByText('No API keys yet.')).toBeInTheDocument();
    const createButton = screen.getByRole('button', { name: 'Create API Key' });
    expect(createButton).toHaveAttribute('data-variant', 'primary');
    await fireEvent.click(createButton);
    expect(onCreate).toHaveBeenCalledOnce();
  });

  it('uses the host heading level and fails closed while host interactions are not ready', async () => {
    const onCreate = vi.fn();
    const onFilterChange = vi.fn();
    const copyText = vi.fn<(value: string) => Promise<void>>().mockResolvedValue(undefined);
    const onRequestRemove = vi.fn();
    render(ApiKeyShellHarness, {
      props: {
        state: { kind: 'ready', keys: readyKeys.slice(0, 2) },
        filterOptions: ['products'],
        headingLevel: 1,
        interactive: false,
        onCreate,
        onFilterChange,
        copyText,
        onRequestRemove,
      },
    });

    expect(screen.getByRole('heading', { level: 1, name: 'API Keys' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { level: 2, name: 'Global browser key' })).toBeInTheDocument();
    const createButton = screen.getByRole('button', { name: 'Create API Key' });
    const filter = screen.getByLabelText('Filter by index');
    const globalCard = screen.getByRole('article', {
      name: 'Global browser key',
    });
    const copyButton = within(globalCard).getByRole('button', {
      name: 'Copy Global browser key',
    });
    const removeButton = within(globalCard).getByRole('button', {
      name: 'Remove Global browser key',
    });

    expect(createButton).toBeDisabled();
    expect(filter).toBeDisabled();
    expect(copyButton).toBeDisabled();
    expect(removeButton).toBeDisabled();
    await fireEvent.click(createButton);
    await fireEvent.change(filter, { target: { value: 'products' } });
    await fireEvent.click(copyButton);
    await fireEvent.click(removeButton);
    expect(onCreate).not.toHaveBeenCalled();
    expect(onFilterChange).not.toHaveBeenCalled();
    expect(copyText).not.toHaveBeenCalled();
    expect(onRequestRemove).not.toHaveBeenCalled();
  });

  it('filters by exact index while retaining unrestricted keys and renders host details as text', async () => {
    const onFilterChange = vi.fn();
    const detailsById = {
      'engine-secret-global': '<script>global details</script>',
      'engine-secret-products': 'ACL: search',
      'engine-secret-articles': 'ACL: browse',
    };
    const { rerender } = render(ApiKeyShellHarness, {
      props: {
        state: { kind: 'ready', keys: readyKeys },
        filterOptions: ['articles', 'products'],
        selectedFilter: '',
        detailsById,
        onFilterChange,
      },
    });

    expect(screen.getByRole('option', { name: 'All indexes' })).toHaveValue('');
    expect(screen.getAllByRole('article')).toHaveLength(3);
    expect(screen.getByLabelText('Details for Global browser key')).toHaveTextContent(
      '<script>global details</script>'
    );
    expect(document.querySelector('script')).toBeNull();

    await fireEvent.change(screen.getByLabelText('Filter by index'), {
      target: { value: 'products' },
    });
    expect(onFilterChange).toHaveBeenCalledExactlyOnceWith('products');

    await rerender({
      state: { kind: 'ready', keys: readyKeys },
      filterOptions: ['articles', 'products'],
      selectedFilter: 'products',
      detailsById,
      onFilterChange,
    });
    expect(screen.getAllByRole('article')).toHaveLength(2);
    expect(screen.getByText('Global browser key')).toBeInTheDocument();
    expect(screen.getByText('Products browser key')).toBeInTheDocument();
    expect(screen.queryByText('Articles browser key')).not.toBeInTheDocument();

    await rerender({
      state: { kind: 'ready', keys: readyKeys.slice(2) },
      filterOptions: ['products'],
      selectedFilter: 'products',
      detailsById,
      onFilterChange,
    });
    expect(screen.getByText('No API keys match this filter.')).toBeInTheDocument();
  });

  it('copies through the host callback, reports failure, clears feedback, and only requests removal', async () => {
    vi.useFakeTimers();
    const copyText = vi
      .fn<(value: string) => Promise<void>>()
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error('clipboard refused'));
    const onRequestRemove = vi.fn();
    render(ApiKeyShellHarness, {
      props: {
        state: { kind: 'ready', keys: readyKeys.slice(0, 2) },
        detailsById: {
          'engine-secret-global': 'Global details',
          'engine-secret-products': 'Products details',
        },
        copyText,
        onRequestRemove,
      },
    });

    const globalCard = screen.getByRole('article', { name: 'Global browser key' });
    const globalCopy = within(globalCard).getByRole('button', { name: 'Copy Global browser key' });
    await fireEvent.click(globalCopy);
    expect(copyText).toHaveBeenCalledExactlyOnceWith('engine-secret-global');
    expect(within(globalCard).getByRole('status')).toHaveTextContent('Copied');

    await vi.advanceTimersByTimeAsync(2_000);
    expect(within(globalCard).queryByRole('status')).not.toBeInTheDocument();

    const productsCard = screen.getByRole('article', { name: 'Products browser key' });
    await fireEvent.click(
      within(productsCard).getByRole('button', { name: 'Copy Products browser key' })
    );
    expect(copyText).toHaveBeenNthCalledWith(2, 'engine-secret-products');
    expect(within(productsCard).getByRole('alert')).toHaveTextContent('Could not copy');

    const removeButton = within(productsCard).getByRole('button', {
      name: 'Remove Products browser key',
    });
    expect(within(productsCard).getByRole('button', { name: 'Copy Products browser key' })).toHaveAttribute(
      'data-variant',
      'secondary'
    );
    expect(removeButton).toHaveAttribute('data-variant', 'danger');
    await fireEvent.click(removeButton);
    expect(onRequestRemove).toHaveBeenCalledOnce();
    expect(onRequestRemove.mock.calls[0]?.[0]).toEqual({
      opaqueId: 'engine-secret-products',
      trigger: removeButton,
    });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();

    for (const secret of readyKeys.map((key) => key.opaqueId)) {
      expect(document.querySelector(`[id*="${secret}"], [data-testid*="${secret}"]`)).toBeNull();
    }
  });

  it('registers every supported collection state as an executable story', () => {
    expect(Object.keys(apiKeyShellStories).sort()).toEqual([
      'emptyApiKeyShellStory',
      'errorApiKeyShellStory',
      'loadingApiKeyShellStory',
      'populatedApiKeyShellStory',
    ]);
  });
});
