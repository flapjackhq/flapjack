import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { IndexListSearch, IndexSearch } from '../src/lib/features';
import type {
  ConsoleTransport,
  IndexSummary,
  SearchPage,
} from '../src/lib/transport/console_transport';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function fixture(
  listIndexes: ConsoleTransport['listIndexes'],
  searchIndex: ConsoleTransport['searchIndex'] = vi.fn(async () => {
    throw new Error('unexpected search');
  })
) {
  return { listIndexes, searchIndex } satisfies ConsoleTransport;
}

const INDEXES: IndexSummary[] = [
  { name: 'products', entries: 27, dataSize: 4096 },
  { name: 'articles', entries: 3, dataSize: 512 },
];

const FIRST_PAGE: SearchPage = {
  hits: [{ objectID: 'sku-27', title: 'Red shoes', note: '<img src=x alt=attack>' }],
  nbHits: 41,
  page: 0,
  nbPages: 2,
  hitsPerPage: 20,
  processingTimeMs: 7,
};

describe('shared Index List and basic Search', () => {
  it('mounts portable Search without loading indexes or searching implicitly', async () => {
    const transport = fixture(vi.fn(async () => INDEXES));
    render(IndexSearch, { props: { transport, indexName: 'products' } });

    expect(screen.getByRole('searchbox', { name: 'Query' })).toHaveFocus();
    expect(transport.listIndexes).not.toHaveBeenCalled();
    expect(transport.searchIndex).not.toHaveBeenCalled();
  });

  it('loads the exact index values once and never searches on mount', async () => {
    const pending = deferred<IndexSummary[]>();
    const transport = fixture(vi.fn(() => pending.promise));
    render(IndexListSearch, { props: { transport } });

    expect(screen.getByRole('status')).toHaveTextContent('Loading indexes');
    expect(transport.listIndexes).toHaveBeenCalledOnce();
    expect(transport.searchIndex).not.toHaveBeenCalled();

    pending.resolve(INDEXES);
    expect(await screen.findByRole('table', { name: 'Indexes' })).toBeInTheDocument();
    expect(screen.getByRole('row', { name: 'products 27 4096 bytes' })).toBeInTheDocument();
    expect(screen.getByRole('row', { name: 'articles 3 512 bytes' })).toBeInTheDocument();
    expect(transport.listIndexes).toHaveBeenCalledOnce();
    expect(transport.searchIndex).not.toHaveBeenCalled();
  });

  it('keeps empty and failed list states distinct and retries only on request', async () => {
    const user = userEvent.setup();
    const listIndexes = vi
      .fn<ConsoleTransport['listIndexes']>()
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce([]);
    render(IndexListSearch, { props: { transport: fixture(listIndexes) } });

    expect(await screen.findByRole('alert')).toHaveTextContent('Could not load indexes.');
    expect(listIndexes).toHaveBeenCalledOnce();
    await user.click(screen.getByRole('button', { name: 'Retry loading indexes' }));
    expect(await screen.findByText('No indexes yet.')).toBeInTheDocument();
    expect(listIndexes).toHaveBeenCalledTimes(2);
  });

  it('submits and pages portable Search with exact requests', async () => {
    const user = userEvent.setup();
    const listIndexes = vi.fn(async () => INDEXES);
    const searchIndex = vi
      .fn<ConsoleTransport['searchIndex']>()
      .mockResolvedValueOnce(FIRST_PAGE)
      .mockResolvedValueOnce({
        ...FIRST_PAGE,
        hits: [{ objectID: 'sku-41', title: 'Red boots' }],
        page: 1,
        processingTimeMs: 9,
      })
      .mockResolvedValueOnce(FIRST_PAGE)
      .mockResolvedValueOnce({
        ...FIRST_PAGE,
        hits: [{ objectID: 'sku-41', title: 'Red boots' }],
        page: 1,
        processingTimeMs: 9,
      })
      .mockResolvedValueOnce({ ...FIRST_PAGE, nbHits: 0, nbPages: 0, hits: [] });
    render(IndexSearch, {
      props: {
        transport: fixture(listIndexes, searchIndex),
        indexName: 'products',
      },
    });

    const query = screen.getByRole('searchbox', { name: 'Query' });
    expect(query).toHaveFocus();
    expect(searchIndex).not.toHaveBeenCalled();

    await user.type(query, 'red shoes');
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(searchIndex).toHaveBeenNthCalledWith(1, 'products', {
      query: 'red shoes',
      page: 0,
      hitsPerPage: 20,
    });
    expect(await screen.findByText('41 results in 7ms')).toBeInTheDocument();
    const firstResult = screen.getByRole('article', { name: 'Red shoes' });
    expect(within(firstResult).getByText('Open details')).toBeInTheDocument();
    expect(within(firstResult).getByText(/"objectID": "sku-27"/)).not.toBeVisible();
    await user.click(within(firstResult).getByText('Open details'));
    expect(within(firstResult).getByText('Close details')).toBeInTheDocument();
    expect(screen.getByText(/"objectID": "sku-27"/)).toBeVisible();
    expect(screen.getByText(/<img src=x alt=attack>/)).toBeVisible();
    expect(screen.queryByRole('img', { name: 'attack' })).not.toBeInTheDocument();
    expect(screen.getByText('Page 1 of 2')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Next page' }));
    expect(searchIndex).toHaveBeenNthCalledWith(2, 'products', {
      query: 'red shoes',
      page: 1,
      hitsPerPage: 20,
    });
    expect(await screen.findByRole('article', { name: 'Red boots' })).toBeInTheDocument();
    expect(screen.getByText(/"objectID": "sku-41"/)).not.toBeVisible();
    expect(screen.queryByText(/"objectID": "sku-27"/)).not.toBeInTheDocument();
    expect(screen.getByText('Page 2 of 2')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Previous page' }));
    expect(searchIndex).toHaveBeenNthCalledWith(3, 'products', {
      query: 'red shoes',
      page: 0,
      hitsPerPage: 20,
    });
    expect(await screen.findByRole('article', { name: 'Red shoes' })).toBeInTheDocument();
    expect(screen.getByText(/"objectID": "sku-27"/)).not.toBeVisible();

    await user.click(screen.getByRole('button', { name: 'Next page' }));
    expect(await screen.findByRole('article', { name: 'Red boots' })).toBeInTheDocument();
    await user.clear(query);
    await user.type(query, 'blue boots');
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(searchIndex).toHaveBeenNthCalledWith(5, 'products', {
      query: 'blue boots',
      page: 0,
      hitsPerPage: 20,
    });
    expect(await screen.findByText('No results.')).toBeInTheDocument();
    expect(searchIndex).toHaveBeenCalledTimes(5);
    expect(listIndexes).not.toHaveBeenCalled();
  });

  it('omits preview analytics unless capability and host copy are both present', async () => {
    const user = userEvent.setup();
    const copy = { toggleLabel: 'Track Analytics' };
    const recordResultOpen = vi.fn(async () => undefined);

    for (const props of [
      {},
      { searchAnalyticsCopy: copy },
      { searchAnalytics: { recordResultOpen } },
    ]) {
      const searchIndex = vi.fn<ConsoleTransport['searchIndex']>().mockResolvedValue(FIRST_PAGE);
      const transport = {
        ...fixture(vi.fn(async () => INDEXES), searchIndex),
        ...('searchAnalytics' in props ? { searchAnalytics: props.searchAnalytics } : {}),
      } as ConsoleTransport;
      render(IndexSearch, {
        props: {
          transport,
          indexName: 'products',
          ...('searchAnalyticsCopy' in props
            ? { searchAnalyticsCopy: props.searchAnalyticsCopy }
            : {}),
        },
      });

      expect(screen.queryByRole('checkbox', { name: 'Track Analytics' })).not.toBeInTheDocument();
      await user.click(screen.getByRole('button', { name: 'Search' }));
      expect(searchIndex).toHaveBeenCalledExactlyOnceWith('products', {
        query: '',
        page: 0,
        hitsPerPage: 20,
      });
      cleanup();
    }

    expect(recordResultOpen).not.toHaveBeenCalled();
  });

  it('loads semantic options without blocking keyword search and commits exact native controls', async () => {
    const user = userEvent.setup();
    const semantics = deferred<{
      configuredEmbedderCount: number;
      queryEmbedderNames: string[];
      mode: 'neuralSearch';
    }>();
    const load = vi.fn(() => semantics.promise);
    const searchIndex = vi
      .fn<ConsoleTransport['searchIndex']>()
      .mockResolvedValueOnce(FIRST_PAGE)
      .mockResolvedValueOnce(FIRST_PAGE)
      .mockResolvedValueOnce(FIRST_PAGE)
      .mockResolvedValueOnce({ ...FIRST_PAGE, semanticFallback: true })
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce({ ...FIRST_PAGE, page: 1 });
    const transport = {
      ...fixture(vi.fn(async () => INDEXES), searchIndex),
      searchSemantics: { load },
    } satisfies ConsoleTransport;
    render(IndexSearch, { props: { transport, indexName: 'products' } });

    expect(screen.getByRole('status')).toHaveTextContent('Loading semantic search options');
    expect(load).toHaveBeenCalledExactlyOnceWith('products');
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(searchIndex).toHaveBeenCalledExactlyOnceWith('products', {
      query: '',
      page: 0,
      hitsPerPage: 20,
      mode: 'keywordSearch',
    });

    semantics.resolve({
      configuredEmbedderCount: 4,
      queryEmbedderNames: ['cloud', 'remote'],
      mode: 'neuralSearch',
    });
    const ratio = await screen.findByRole('slider', { name: 'Semantic ratio' });
    const embedder = screen.getByRole('combobox', { name: 'Query embedder' });
    expect(ratio).toHaveValue('0.5');
    expect(embedder).toHaveValue('cloud');
    expect(screen.getByText('Balanced')).toBeVisible();
    expect(screen.getByText('4 embedders configured; 2 can embed queries.')).toBeVisible();
    expect(searchIndex).toHaveBeenCalledOnce();

    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(searchIndex).toHaveBeenNthCalledWith(2, 'products', {
      query: '',
      page: 0,
      hitsPerPage: 20,
      mode: 'neuralSearch',
      hybrid: { semanticRatio: 0.5, embedder: 'cloud' },
    });

    await fireEvent.input(ratio, { target: { value: '0' } });
    expect(screen.getByText('Keyword only')).toBeVisible();
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(searchIndex).toHaveBeenNthCalledWith(3, 'products', {
      query: '',
      page: 0,
      hitsPerPage: 20,
      mode: 'keywordSearch',
    });

    await fireEvent.input(ratio, { target: { value: '0.6' } });
    await user.selectOptions(embedder, 'remote');
    expect(screen.getByText('60% semantic')).toBeVisible();
    expect(searchIndex).toHaveBeenCalledTimes(3);
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(searchIndex).toHaveBeenNthCalledWith(4, 'products', {
      query: '',
      page: 0,
      hitsPerPage: 20,
      mode: 'neuralSearch',
      hybrid: { semanticRatio: 0.6, embedder: 'remote' },
    });
    expect(await screen.findByText('Semantic search was unavailable; keyword results are shown.')).toBeVisible();

    await fireEvent.input(ratio, { target: { value: '0' } });
    await user.selectOptions(embedder, 'cloud');
    await user.click(screen.getByRole('button', { name: 'Next page' }));
    expect(searchIndex).toHaveBeenNthCalledWith(5, 'products', {
      query: '',
      page: 1,
      hitsPerPage: 20,
      mode: 'neuralSearch',
      hybrid: { semanticRatio: 0.6, embedder: 'remote' },
    });
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not search this index.');
    await user.click(screen.getByRole('button', { name: 'Retry search' }));
    expect(searchIndex).toHaveBeenNthCalledWith(6, 'products', {
      query: '',
      page: 1,
      hitsPerPage: 20,
      mode: 'neuralSearch',
      hybrid: { semanticRatio: 0.6, embedder: 'remote' },
    });
  });

  it('keeps semantic configuration failure safe and retries without searching', async () => {
    const user = userEvent.setup();
    const load = vi
      .fn<NonNullable<ConsoleTransport['searchSemantics']>['load']>()
      .mockRejectedValueOnce(new Error('do-not-render-settings-error'))
      .mockResolvedValueOnce({
        configuredEmbedderCount: 1,
        queryEmbedderNames: [],
        mode: 'keywordSearch',
      });
    const searchIndex = vi.fn<ConsoleTransport['searchIndex']>().mockResolvedValue(FIRST_PAGE);
    const transport = {
      ...fixture(vi.fn(async () => INDEXES), searchIndex),
      searchSemantics: { load },
    } satisfies ConsoleTransport;
    render(IndexSearch, { props: { transport, indexName: 'products' } });

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Could not load semantic search options.'
    );
    expect(screen.queryByText('do-not-render-settings-error')).not.toBeInTheDocument();
    expect(searchIndex).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(searchIndex).toHaveBeenCalledExactlyOnceWith('products', {
      query: '',
      page: 0,
      hitsPerPage: 20,
      mode: 'keywordSearch',
    });
    await user.click(screen.getByRole('button', { name: 'Retry semantic options' }));
    expect(load).toHaveBeenCalledTimes(2);
    expect(searchIndex).toHaveBeenCalledOnce();
    expect(await screen.findByText('No query-capable embedders are configured.')).toBeVisible();
    expect(screen.queryByRole('slider', { name: 'Semantic ratio' })).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(searchIndex).toHaveBeenNthCalledWith(2, 'products', {
      query: '',
      page: 0,
      hitsPerPage: 20,
      mode: 'keywordSearch',
    });
  });

  it('records only analytics-enabled Open details with exact correlation and safe feedback', async () => {
    const user = userEvent.setup();
    const firstDelivery = deferred<void>();
    const recordResultOpen = vi
      .fn()
      .mockImplementationOnce(() => firstDelivery.promise)
      .mockRejectedValueOnce(new Error('do-not-render-host-error'));
    const searchIndex = vi
      .fn<ConsoleTransport['searchIndex']>()
      .mockResolvedValue({ ...FIRST_PAGE, queryId: 'a'.repeat(32) });
    const transport = {
      ...fixture(vi.fn(async () => INDEXES), searchIndex),
      searchAnalytics: { recordResultOpen },
    } as ConsoleTransport;

    render(IndexSearch, {
      props: {
        transport,
        indexName: 'products',
        searchAnalyticsCopy: {
          toggleLabel: 'Track Analytics',
          helpText: 'Records preview searches and explicit result opens.',
        },
      },
    });

    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(searchIndex).toHaveBeenNthCalledWith(1, 'products', {
      query: '',
      page: 0,
      hitsPerPage: 20,
      analytics: false,
    });
    const result = await screen.findByRole('article', { name: 'Red shoes' });
    await user.click(within(result).getByText('Open details'));
    expect(recordResultOpen).not.toHaveBeenCalled();
    await user.click(within(result).getByText('Close details'));

    await user.click(screen.getByRole('checkbox', { name: 'Track Analytics' }));
    expect(screen.getByText('Records preview searches and explicit result opens.')).toBeVisible();
    expect(screen.getByRole('status')).toHaveTextContent(
      'Preview activity recording is on. Run a new search to record result opens.'
    );
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(searchIndex).toHaveBeenNthCalledWith(2, 'products', {
      query: '',
      page: 0,
      hitsPerPage: 20,
      analytics: true,
      clickAnalytics: true,
    });

    await user.click(within(result).getByText('Open details'));
    expect(recordResultOpen).toHaveBeenCalledExactlyOnceWith({
      indexName: 'products',
      objectId: 'sku-27',
      position: 1,
      queryId: 'a'.repeat(32),
    });
    expect(screen.queryByText('Recorded result open.')).not.toBeInTheDocument();
    firstDelivery.resolve();
    expect(await screen.findByText('Recorded result open.')).toBeVisible();

    await user.click(within(result).getByText('Close details'));
    expect(recordResultOpen).toHaveBeenCalledOnce();
    await user.click(within(result).getByText('Open details'));
    expect(recordResultOpen).toHaveBeenCalledTimes(2);
    expect(await screen.findByText('Result open was not recorded.')).toBeVisible();
    expect(screen.queryByText('do-not-render-host-error')).not.toBeInTheDocument();
  });

  it('does not resurrect query correlation or delivery feedback across state generations', async () => {
    const user = userEvent.setup();
    const searchResponse = deferred<SearchPage>();
    const eventDelivery = deferred<void>();
    const replacementSearch = deferred<SearchPage>();
    const recordResultOpen = vi.fn(() => eventDelivery.promise);
    const searchIndex = vi
      .fn<ConsoleTransport['searchIndex']>()
      .mockImplementationOnce(() => searchResponse.promise)
      .mockResolvedValueOnce({
        ...FIRST_PAGE,
        hits: [...FIRST_PAGE.hits, { objectID: '   ', title: 'Missing object ID' }],
        queryId: 'b'.repeat(32),
      })
      .mockImplementationOnce(() => replacementSearch.promise);
    const transport = {
      ...fixture(vi.fn(async () => INDEXES), searchIndex),
      searchAnalytics: { recordResultOpen },
    } as ConsoleTransport;

    render(IndexSearch, {
      props: {
        transport,
        indexName: 'products',
        searchAnalyticsCopy: { toggleLabel: 'Track Analytics' },
      },
    });

    const toggle = screen.getByRole('checkbox', { name: 'Track Analytics' });
    await user.click(toggle);
    await user.click(screen.getByRole('button', { name: 'Search' }));
    await user.click(toggle);
    await user.click(toggle);
    searchResponse.resolve({ ...FIRST_PAGE, queryId: 'a'.repeat(32) });
    const result = await screen.findByRole('article', { name: 'Red shoes' });
    await user.click(within(result).getByText('Open details'));
    expect(recordResultOpen).not.toHaveBeenCalled();
    expect(screen.getByRole('status')).toHaveTextContent(
      'Not recorded: run a new search after enabling preview activity.'
    );
    await user.click(within(result).getByText('Close details'));

    await user.click(screen.getByRole('button', { name: 'Search' }));
    await user.click(within(result).getByText('Open details'));
    expect(recordResultOpen).toHaveBeenCalledOnce();
    const unidentifiedResult = screen.getByRole('article', { name: 'Missing object ID' });
    await user.click(within(unidentifiedResult).getByText('Open details'));
    expect(screen.getByRole('status')).toHaveTextContent(
      'Not recorded: the result does not include an object ID.'
    );
    eventDelivery.resolve();
    await eventDelivery.promise;
    await Promise.resolve();
    expect(screen.getByRole('status')).toHaveTextContent(
      'Not recorded: the result does not include an object ID.'
    );
    await user.click(within(result).getByText('Close details'));
    await user.click(screen.getByRole('button', { name: 'Search' }));
    await user.click(within(result).getByText('Open details'));
    expect(recordResultOpen).toHaveBeenCalledOnce();
    expect(screen.getAllByRole('status').some((status) => status.textContent?.includes('wait'))).toBe(
      true
    );
    replacementSearch.reject(new Error('offline'));
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not search this index.');
    await user.click(within(result).getByText('Close details'));
    await user.click(within(result).getByText('Open details'));
    expect(screen.getByRole('status')).toHaveTextContent(
      'Not recorded: the search response did not include a query ID.'
    );
    expect(screen.queryByText('Recorded result open.')).not.toBeInTheDocument();
    expect(screen.queryByText('Result open was not recorded.')).not.toBeInTheDocument();
  });

  it('suppresses uncorrelated or unidentified result opens and uses absolute positions', async () => {
    const user = userEvent.setup();
    const recordResultOpen = vi.fn(async () => undefined);
    const searchIndex = vi
      .fn<ConsoleTransport['searchIndex']>()
      .mockResolvedValueOnce({ ...FIRST_PAGE, queryId: undefined })
      .mockResolvedValueOnce({
        ...FIRST_PAGE,
        hits: [{ objectID: '   ', title: 'Missing object ID' }],
        queryId: 'a'.repeat(32),
      })
      .mockResolvedValueOnce({
        ...FIRST_PAGE,
        page: 1,
        hits: [{ objectID: 'page-two', title: 'Page two result' }],
        queryId: 'b'.repeat(32),
      });
    const transport = {
      ...fixture(vi.fn(async () => INDEXES), searchIndex),
      searchAnalytics: { recordResultOpen },
    } as ConsoleTransport;
    render(IndexSearch, {
      props: {
        transport,
        indexName: 'products',
        searchAnalyticsCopy: { toggleLabel: 'Track Analytics' },
      },
    });

    await user.click(screen.getByRole('checkbox', { name: 'Track Analytics' }));
    await user.click(screen.getByRole('button', { name: 'Search' }));
    let result = await screen.findByRole('article', { name: 'Red shoes' });
    await user.click(within(result).getByText('Open details'));
    expect(screen.getByRole('status')).toHaveTextContent(
      'Not recorded: the search response did not include a query ID.'
    );
    expect(recordResultOpen).not.toHaveBeenCalled();
    await user.click(within(result).getByText('Close details'));

    await user.click(screen.getByRole('button', { name: 'Search' }));
    result = await screen.findByRole('article', { name: 'Missing object ID' });
    await user.click(within(result).getByText('Open details'));
    expect(screen.getByRole('status')).toHaveTextContent(
      'Not recorded: the result does not include an object ID.'
    );
    expect(recordResultOpen).not.toHaveBeenCalled();

    await user.click(screen.getByRole('button', { name: 'Next page' }));
    result = await screen.findByRole('article', { name: 'Page two result' });
    await user.click(within(result).getByText('Open details'));
    expect(recordResultOpen).toHaveBeenCalledExactlyOnceWith({
      indexName: 'products',
      objectId: 'page-two',
      position: 21,
      queryId: 'b'.repeat(32),
    });
  });

  it('renders safe deterministic result titles and isolates native details disclosure', async () => {
    const user = userEvent.setup();
    const searchIndex = vi.fn<ConsoleTransport['searchIndex']>().mockResolvedValue({
      hits: [
        { objectID: 'title-id', title: '<img src=x alt=attack>', secret: 'title details' },
        { objectID: 'name-id', title: '   ', name: 'Named result', secret: 'name details' },
        { objectID: 'object-only', title: '', name: '\t', secret: 'object details' },
        { objectID: ' ', title: '\t', name: '', secret: 'fallback details' },
      ],
      nbHits: 24,
      page: 1,
      nbPages: 2,
      hitsPerPage: 20,
      processingTimeMs: 4,
    });
    render(IndexSearch, {
      props: {
        transport: fixture(vi.fn(async () => INDEXES), searchIndex),
        indexName: 'articles',
      },
    });

    await user.click(screen.getByRole('button', { name: 'Search' }));

    const titled = await screen.findByRole('article', { name: '<img src=x alt=attack>' });
    const named = screen.getByRole('article', { name: 'Named result' });
    const objectOnly = screen.getByRole('article', { name: 'object-only' });
    const fallback = screen.getByRole('article', { name: 'Result 24' });
    expect(screen.queryByRole('img', { name: 'attack' })).not.toBeInTheDocument();
    expect(screen.getByText(/"secret": "title details"/)).not.toBeVisible();
    expect(screen.getByText(/"secret": "fallback details"/)).not.toBeVisible();

    await user.click(within(titled).getByText('Open details'));
    expect(within(titled).getByText('Close details')).toBeInTheDocument();
    expect(within(titled).getByText(/"secret": "title details"/)).toBeVisible();
    expect(within(named).getByText('Open details')).toBeInTheDocument();
    expect(within(named).getByText(/"secret": "name details"/)).not.toBeVisible();
    expect(within(objectOnly).getByText('Open details')).toBeInTheDocument();

    await user.click(within(fallback).getByText('Open details'));
    expect(within(fallback).getByText('Close details')).toBeInTheDocument();
    expect(within(fallback).getByText(/"secret": "fallback details"/)).toBeVisible();
    await user.click(within(fallback).getByText('Close details'));
    expect(within(fallback).getByText('Open details')).toBeInTheDocument();
    expect(within(fallback).getByText(/"secret": "fallback details"/)).not.toBeVisible();
  });

  it('allows an explicit blank search and does not infer paging from a full result page', async () => {
    const user = userEvent.setup();
    const searchIndex = vi.fn<ConsoleTransport['searchIndex']>().mockResolvedValue({
      hits: Array.from({ length: 20 }, (_, index) => ({ objectID: `sku-${index}` })),
      nbHits: 20,
      page: 0,
      nbPages: 1,
      hitsPerPage: 20,
      processingTimeMs: 4,
    });
    render(IndexSearch, {
      props: {
        transport: fixture(vi.fn(async () => INDEXES), searchIndex),
        indexName: 'articles',
      },
    });

    expect(screen.queryByText('No results.')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Search' }));

    expect(searchIndex).toHaveBeenCalledExactlyOnceWith('articles', {
      query: '',
      page: 0,
      hitsPerPage: 20,
    });
    expect(await screen.findByText('20 results in 4ms')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Next page' })).not.toBeInTheDocument();
  });

  it('announces an unresolved search and prevents a duplicate request', async () => {
    const user = userEvent.setup();
    const pending = deferred<SearchPage>();
    const searchIndex = vi.fn<ConsoleTransport['searchIndex']>(() => pending.promise);
    render(IndexSearch, {
      props: {
        transport: fixture(vi.fn(async () => INDEXES), searchIndex),
        indexName: 'products',
      },
    });

    const submit = screen.getByRole('button', { name: 'Search' });
    await user.click(submit);
    expect(screen.getByRole('status')).toHaveTextContent('Searching...');
    expect(submit).toBeDisabled();
    await user.click(submit);
    expect(searchIndex).toHaveBeenCalledOnce();

    pending.resolve(FIRST_PAGE);
    expect(await screen.findByText('41 results in 7ms')).toBeInTheDocument();
    expect(submit).toBeEnabled();
  });

  it('retains prior results and committed intent when search fails, then retries exactly once', async () => {
    const user = userEvent.setup();
    const listIndexes = vi.fn(async () => INDEXES);
    const searchIndex = vi
      .fn<ConsoleTransport['searchIndex']>()
      .mockResolvedValueOnce(FIRST_PAGE)
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce({
        ...FIRST_PAGE,
        hits: [{ objectID: 'replacement', title: 'Replacement result', note: 'fresh details' }],
        nbHits: 1,
        nbPages: 1,
      });
    render(IndexSearch, {
      props: {
        transport: fixture(listIndexes, searchIndex),
        indexName: 'products',
      },
    });

    const query = screen.getByRole('searchbox', { name: 'Query' });
    await user.type(query, 'red');
    await user.keyboard('{Enter}');
    const retainedResult = await screen.findByRole('article', { name: 'Red shoes' });
    await user.click(within(retainedResult).getByText('Open details'));
    expect(within(retainedResult).getByText(/"objectID": "sku-27"/)).toBeVisible();
    expect(searchIndex).toHaveBeenNthCalledWith(1, 'products', {
      query: 'red',
      page: 0,
      hitsPerPage: 20,
    });

    await user.clear(query);
    await user.type(query, 'blue');
    await user.keyboard('{Enter}');
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not search this index.');
    expect(searchIndex).toHaveBeenNthCalledWith(2, 'products', {
      query: 'blue',
      page: 0,
      hitsPerPage: 20,
    });
    expect(within(retainedResult).getByText('Close details')).toBeInTheDocument();
    expect(within(retainedResult).getByText(/"objectID": "sku-27"/)).toBeVisible();

    await user.type(query, ' draft');
    await user.click(screen.getByRole('button', { name: 'Retry search' }));
    expect(searchIndex).toHaveBeenNthCalledWith(3, 'products', {
      query: 'blue',
      page: 0,
      hitsPerPage: 20,
    });
    const replacement = await screen.findByRole('article', { name: 'Replacement result' });
    expect(within(replacement).getByText('Open details')).toBeInTheDocument();
    expect(within(replacement).getByText(/"objectID": "replacement"/)).not.toBeVisible();
    expect(searchIndex).toHaveBeenCalledTimes(3);
    expect(listIndexes).not.toHaveBeenCalled();
  });

  it('renders and invokes the optional Back action only when supplied', async () => {
    const user = userEvent.setup();
    const onBackToIndexes = vi.fn();
    const transport = fixture(vi.fn(async () => INDEXES));
    const first = render(IndexSearch, {
      props: { transport, indexName: 'products', onBackToIndexes },
    });

    await user.click(screen.getByRole('button', { name: 'Back to indexes' }));
    expect(onBackToIndexes).toHaveBeenCalledOnce();
    await first.unmount();

    render(IndexSearch, { props: { transport, indexName: 'products' } });
    expect(screen.queryByRole('button', { name: 'Back to indexes' })).not.toBeInTheDocument();
  });

  it('keeps the loaded list on Back and gives another index fresh Search state', async () => {
    const user = userEvent.setup();
    const listIndexes = vi.fn(async () => INDEXES);
    const searchIndex = vi
      .fn<ConsoleTransport['searchIndex']>()
      .mockResolvedValueOnce(FIRST_PAGE)
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce({ ...FIRST_PAGE, hits: [], nbHits: 0, nbPages: 0 });
    render(IndexListSearch, { props: { transport: fixture(listIndexes, searchIndex) } });

    await user.click(await screen.findByRole('button', { name: 'Search products' }));
    const productsQuery = screen.getByRole('searchbox', { name: 'Query' });
    expect(productsQuery).toHaveFocus();
    expect(searchIndex).not.toHaveBeenCalled();
    await user.type(productsQuery, 'red');
    await user.keyboard('{Enter}');
    expect(await screen.findByRole('article', { name: 'Red shoes' })).toBeInTheDocument();
    await user.clear(productsQuery);
    await user.type(productsQuery, 'blue');
    await user.keyboard('{Enter}');
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not search this index.');

    await user.click(screen.getByRole('button', { name: 'Back to indexes' }));
    expect(await screen.findByRole('table', { name: 'Indexes' })).toBeInTheDocument();
    expect(listIndexes).toHaveBeenCalledOnce();
    await user.click(screen.getByRole('button', { name: 'Search articles' }));

    const articlesQuery = screen.getByRole('searchbox', { name: 'Query' });
    expect(articlesQuery).toHaveFocus();
    expect(articlesQuery).toHaveValue('');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(screen.queryByRole('article', { name: 'Red shoes' })).not.toBeInTheDocument();
    expect(searchIndex).toHaveBeenCalledTimes(2);

    await user.type(articlesQuery, 'fresh');
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(searchIndex).toHaveBeenNthCalledWith(3, 'articles', {
      query: 'fresh',
      page: 0,
      hitsPerPage: 20,
    });
    expect(await screen.findByText('No results.')).toBeInTheDocument();
    expect(listIndexes).toHaveBeenCalledOnce();
  });
});
