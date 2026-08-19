import '@testing-library/jest-dom/vitest';
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { standaloneHostStory } from '../src/StandaloneHost.stories';
import type { AuthenticatedSession, SessionProvider } from '../src/host/session';
import type { ConsoleTransport } from '../src/lib/transport/console_transport';

function hostFixture(restoredSession: AuthenticatedSession | null = null) {
  const transport: ConsoleTransport = {
    listIndexes: vi.fn(async () => []),
    searchIndex: vi.fn(async () => {
      throw new Error('Search must not run in P3a');
    }),
  };
  const signOut = vi.fn(async () => undefined);
  const signIn = vi.fn(async () => ({ transport, signOut }));
  const restore = vi.fn(async () => restoredSession);
  const sessionProvider: SessionProvider = { restore, signIn };
  return { sessionProvider, restore, signIn, signOut, transport };
}

describe('standalone console host', () => {
  it('shows only the session check until restoration settles', async () => {
    let settleRestore: (session: AuthenticatedSession | null) => void = () => undefined;
    const restore = vi.fn(
      () =>
        new Promise<AuthenticatedSession | null>((resolve) => {
          settleRestore = resolve;
        })
    );
    const sessionProvider = {
      restore,
      signIn: vi.fn(async () => {
        throw new Error('not used');
      }),
    } satisfies SessionProvider;

    render(standaloneHostStory.component, { props: { sessionProvider } });

    expect(screen.getByRole('status')).toHaveTextContent('Checking session…');
    expect(screen.queryByLabelText('Admin API Key')).not.toBeInTheDocument();
    expect(screen.queryByRole('main', { name: 'Standalone console' })).not.toBeInTheDocument();
    expect(restore).toHaveBeenCalledOnce();

    settleRestore(null);
    expect(await screen.findByLabelText('Admin API Key')).toBeInTheDocument();
  });

  it('mounts the shared console when the existing session is restored', async () => {
    const fixture = hostFixture();
    fixture.restore.mockResolvedValueOnce({
      transport: fixture.transport,
      signOut: fixture.signOut,
    });

    render(standaloneHostStory.component, { props: { sessionProvider: fixture.sessionProvider } });

    expect(await screen.findByRole('main', { name: 'Standalone console' })).toBeInTheDocument();
    expect(screen.queryByLabelText('Admin API Key')).not.toBeInTheDocument();
    expect(fixture.restore).toHaveBeenCalledOnce();
    expect(fixture.transport.listIndexes).toHaveBeenCalledOnce();
    expect(fixture.transport.searchIndex).not.toHaveBeenCalled();
  });

  it('composes the exact standalone analytics copy only with the engine capability', async () => {
    const user = userEvent.setup();
    const fixture = hostFixture();
    fixture.transport.listIndexes = vi.fn(async () => [
      { name: 'products', entries: 27, dataSize: 4096 },
    ]);
    fixture.transport.searchAnalytics = { recordResultOpen: vi.fn(async () => undefined) };
    const loadSearchSemantics = vi.fn(async () => ({
      configuredEmbedderCount: 1,
      queryEmbedderNames: ['remote'],
      mode: 'keywordSearch' as const,
    }));
    fixture.transport.searchSemantics = { load: loadSearchSemantics };
    fixture.restore.mockResolvedValueOnce({
      transport: fixture.transport,
      signOut: fixture.signOut,
    });

    render(standaloneHostStory.component, { props: { sessionProvider: fixture.sessionProvider } });

    await user.click(await screen.findByRole('button', { name: 'Search products' }));
    expect(screen.getByRole('checkbox', { name: 'Track Analytics' })).not.toBeChecked();
    expect(screen.queryByText('Record preview activity in Analytics')).not.toBeInTheDocument();
    expect(await screen.findByRole('slider', { name: 'Semantic ratio' })).toHaveValue('0.5');
    expect(screen.getByText('Balanced')).toBeVisible();
    expect(screen.getByRole('combobox', { name: 'Query embedder' })).toHaveValue('remote');
    expect(loadSearchSemantics).toHaveBeenCalledExactlyOnceWith('products');
  });

  it('mounts a decoded dotted index directly without loading the index list', async () => {
    const user = userEvent.setup();
    const fixture = hostFixture();
    fixture.transport.searchIndex = vi.fn(async (indexName) => ({
      hits: [{ objectID: `${indexName}-result`, title: `Result for ${indexName}` }],
      nbHits: 1,
      page: 0,
      nbPages: 1,
      hitsPerPage: 20,
      processingTimeMs: 2,
    }));
    fixture.restore.mockResolvedValueOnce({
      transport: fixture.transport,
      signOut: fixture.signOut,
    });

    const view = render(standaloneHostStory.component, {
      props: { sessionProvider: fixture.sessionProvider, indexName: 'catalog.v2' },
    });

    expect(await screen.findByRole('heading', { name: 'Search catalog.v2' })).toBeInTheDocument();
    const query = screen.getByRole('searchbox', { name: 'Query' });
    expect(query).toHaveFocus();
    expect(screen.getByRole('link', { name: 'Indexes' })).toHaveAttribute('aria-current', 'page');
    expect(screen.getByRole('link', { name: 'Back to indexes' })).toHaveAttribute(
      'href',
      '/dashboard/'
    );
    expect(fixture.transport.listIndexes).not.toHaveBeenCalled();

    await user.type(query, 'catalog');
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(await screen.findByRole('article', { name: 'Result for catalog.v2' })).toBeVisible();
    expect(fixture.transport.searchIndex).toHaveBeenCalledExactlyOnceWith('catalog.v2', {
      query: 'catalog',
      page: 0,
      hitsPerPage: 20,
    });

    await view.rerender({ sessionProvider: fixture.sessionProvider, indexName: 'articles.v3' });
    expect(await screen.findByRole('heading', { name: 'Search articles.v3' })).toBeInTheDocument();
    const nextQuery = screen.getByRole('searchbox', { name: 'Query' });
    expect(nextQuery).toHaveFocus();
    expect(nextQuery).toHaveValue('');
    expect(screen.queryByRole('article', { name: 'Result for catalog.v2' })).not.toBeInTheDocument();

    await user.type(nextQuery, 'articles');
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(await screen.findByRole('article', { name: 'Result for articles.v3' })).toBeVisible();
    expect(fixture.transport.searchIndex).toHaveBeenNthCalledWith(2, 'articles.v3', {
      query: 'articles',
      page: 0,
      hitsPerPage: 20,
    });
    expect(fixture.transport.listIndexes).not.toHaveBeenCalled();
  });

  it('mounts the engine API Keys composition on the dedicated route screen', async () => {
    const fixture = hostFixture();
    const listKeys = vi.fn(async () => []);
    fixture.transport.apiKeys = {
      kind: 'engine',
      list: listKeys,
      create: vi.fn(async () => ({ key: 'unused', createdAt: '2026-08-17T00:00:00Z' })),
      remove: vi.fn(async () => undefined),
    };
    fixture.restore.mockResolvedValueOnce({
      transport: fixture.transport,
      signOut: fixture.signOut,
    });

    render(standaloneHostStory.component, {
      props: { sessionProvider: fixture.sessionProvider, screen: 'apiKeys' },
    });

    expect(await screen.findByRole('heading', { name: 'API Keys' })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'API Keys' })).toHaveAttribute('aria-current', 'page');
    expect(screen.getByRole('link', { name: 'Indexes' })).toHaveAttribute('href', '/dashboard/');
    expect(screen.getByRole('link', { name: 'API Keys' })).toHaveAttribute(
      'href',
      '/dashboard/keys'
    );
    expect(listKeys).toHaveBeenCalledOnce();
    expect(fixture.transport.listIndexes).toHaveBeenCalledOnce();
    expect(fixture.transport.searchIndex).not.toHaveBeenCalled();
  });

  it('mounts the engine-global Security Sources composition on its dedicated route screen', async () => {
    const fixture = hostFixture();
    const listSources = vi.fn(async () => []);
    fixture.transport.securitySources = {
      kind: 'engine',
      list: listSources,
      append: vi.fn(async () => undefined),
      remove: vi.fn(async () => undefined),
    };
    fixture.restore.mockResolvedValueOnce({
      transport: fixture.transport,
      signOut: fixture.signOut,
    });

    render(standaloneHostStory.component, {
      props: { sessionProvider: fixture.sessionProvider, screen: 'securitySources' },
    });

    expect(await screen.findByRole('heading', { name: 'Security Sources' })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Security Sources' })).toHaveAttribute(
      'aria-current',
      'page'
    );
    expect(screen.getByRole('link', { name: 'Indexes' })).toHaveAttribute('href', '/dashboard/');
    expect(screen.getByRole('link', { name: 'API Keys' })).toHaveAttribute(
      'href',
      '/dashboard/keys'
    );
    expect(screen.getByRole('link', { name: 'Security Sources' })).toHaveAttribute(
      'href',
      '/dashboard/security-sources'
    );
    expect(listSources).toHaveBeenCalledOnce();
    expect(fixture.transport.listIndexes).not.toHaveBeenCalled();
    expect(fixture.transport.searchIndex).not.toHaveBeenCalled();
  });

  it('shows authentication when the session check cannot reach the engine', async () => {
    const fixture = hostFixture();
    fixture.restore.mockRejectedValueOnce(new Error('offline'));

    render(standaloneHostStory.component, { props: { sessionProvider: fixture.sessionProvider } });

    expect(await screen.findByLabelText('Admin API Key')).toBeInTheDocument();
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
    expect(screen.queryByRole('main', { name: 'Standalone console' })).not.toBeInTheDocument();
    expect(fixture.restore).toHaveBeenCalledOnce();
    expect(fixture.signIn).not.toHaveBeenCalled();
    expect(fixture.transport.listIndexes).not.toHaveBeenCalled();
  });

  it('authenticates once, mounts the shared slice, and signs out without a search call', async () => {
    const user = userEvent.setup();
    const fixture = hostFixture();
    render(standaloneHostStory.component, { props: { sessionProvider: fixture.sessionProvider } });

    await user.type(await screen.findByLabelText('Admin API Key'), 'admin-key');
    await user.click(screen.getByRole('button', { name: 'Connect' }));

    const main = screen.getByRole('main', { name: 'Standalone console' });
    expect(main).toHaveAttribute('data-console-host', 'standalone');
    expect(main).toHaveAttribute('data-console-theme', 'flapjack');
    expect(fixture.signIn).toHaveBeenCalledExactlyOnceWith('admin-key');
    expect(screen.getByRole('heading', { name: 'Flapjack Console' })).toBeInTheDocument();
    expect(screen.getByRole('navigation', { name: 'Console' })).toBeInTheDocument();
    expect(screen.getByRole('region', { name: 'Console content' })).toHaveTextContent(
      'No indexes yet.'
    );
    expect(fixture.transport.listIndexes).toHaveBeenCalledOnce();
    expect(fixture.transport.searchIndex).not.toHaveBeenCalled();

    await user.click(screen.getByRole('button', { name: 'Sign out' }));
    expect(fixture.signOut).toHaveBeenCalledOnce();
    expect(screen.getByLabelText('Admin API Key')).toBeInTheDocument();
  });

  it('announces a rejected sign-in without mounting the shell', async () => {
    const user = userEvent.setup();
    const sessionProvider: SessionProvider = {
      restore: vi.fn(async () => null),
      signIn: vi.fn(async () => {
        throw new Error('rejected');
      }),
    };
    render(standaloneHostStory.component, { props: { sessionProvider } });

    await user.type(await screen.findByLabelText('Admin API Key'), 'wrong-key');
    await user.click(screen.getByRole('button', { name: 'Connect' }));

    expect(screen.getByRole('alert')).toHaveTextContent('Could not authenticate.');
    expect(screen.queryByRole('main', { name: 'Standalone console' })).not.toBeInTheDocument();
  });
});
