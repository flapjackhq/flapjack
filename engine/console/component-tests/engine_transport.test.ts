import { describe, expect, it, vi } from 'vitest';
import { createEngineTransport } from '../src/host/engine_transport';

describe('engine console transport', () => {
  it('maps the exact engine-global security source list', async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json([
        { source: '127.0.0.1/32', description: 'Local console' },
        { source: '127.0.0.0/8', description: '' },
      ])
    );
    const transport = createEngineTransport(fetcher);

    await expect(transport.securitySources?.list()).resolves.toEqual([
      { source: '127.0.0.1/32', description: 'Local console' },
      { source: '127.0.0.0/8', description: '' },
    ]);
    expect(transport.securitySources?.kind).toBe('engine');
    expect(fetcher).toHaveBeenCalledExactlyOnceWith('/1/security/sources', {
      headers: { 'x-algolia-application-id': 'flapjack' },
    });
  });

  it('appends and encodes deletion through the existing engine security routes', async () => {
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        Response.json({ createdAt: '2026-08-17T11:00:00+00:00' })
      )
      .mockResolvedValueOnce(
        Response.json({ deletedAt: '2026-08-17T11:01:00+00:00' })
      );
    const transport = createEngineTransport(fetcher);
    const entry = { source: '127.0.0.0/8', description: 'Local proxy' };

    await expect(transport.securitySources?.append(entry)).resolves.toBeUndefined();
    await expect(transport.securitySources?.remove('127.0.0.1/32')).resolves.toBeUndefined();
    expect(fetcher).toHaveBeenNthCalledWith(1, '/1/security/sources/append', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'x-algolia-application-id': 'flapjack',
      },
      body: JSON.stringify(entry),
    });
    expect(fetcher).toHaveBeenNthCalledWith(2, '/1/security/sources/127.0.0.1%2F32', {
      method: 'DELETE',
      headers: { 'x-algolia-application-id': 'flapjack' },
    });
  });

  it.each([
    ['list envelope', { list: { source: '127.0.0.1/32', description: '' } }, 'list'],
    ['blank source', [{ source: ' ', description: '' }], 'list'],
    ['non-string description', [{ source: '127.0.0.1/32', description: null }], 'list'],
    ['append timestamp', { createdAt: 'not-a-date' }, 'append'],
    ['delete timestamp', { deletedAt: 123 }, 'remove'],
  ])('rejects malformed security source responses: %s', async (_case, body, operation) => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(Response.json(body));
    const capability = createEngineTransport(fetcher).securitySources!;
    const request =
      operation === 'list'
        ? capability.list()
        : operation === 'append'
          ? capability.append({ source: '127.0.0.1/32', description: '' })
          : capability.remove('127.0.0.1/32');

    await expect(request).rejects.toThrow('Invalid security source response');
  });

  it('keeps security source values out of transport failures', async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(new Response('private', { status: 503 }));
    const capability = createEngineTransport(fetcher).securitySources!;

    await expect(
      capability.append({ source: 'do-not-leak-source', description: 'do-not-leak-description' })
    ).rejects.toThrow('Could not add security source');
    await expect(capability.remove('do-not-leak-source')).rejects.toThrow(
      'Could not delete security source'
    );
    await expect(capability.list()).rejects.toThrow('Could not load security sources');
  });

  it('maps the exact engine API key list without normalizing its domain', async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        keys: [
          {
            value: 'engine-secret-value',
            createdAt: 1_723_817_600_000,
            acl: ['search', 'browse'],
            description: 'Browser search key',
            indexes: ['products'],
            maxHitsPerQuery: 50,
            maxQueriesPerIPPerHour: 500,
            queryParameters: 'typoTolerance=false',
            referers: ['https://example.test/*'],
            restrictSources: ['10.0.0.0/8'],
            validity: 3_600,
          },
        ],
      })
    );
    const transport = createEngineTransport(fetcher);

    await expect(transport.apiKeys?.list()).resolves.toEqual([
      {
        value: 'engine-secret-value',
        createdAt: 1_723_817_600_000,
        acl: ['search', 'browse'],
        description: 'Browser search key',
        indexes: ['products'],
        maxHitsPerQuery: 50,
        maxQueriesPerIPPerHour: 500,
        queryParameters: 'typoTolerance=false',
        referers: ['https://example.test/*'],
        restrictSources: ['10.0.0.0/8'],
        validity: 3_600,
      },
    ]);
    expect(transport.apiKeys?.kind).toBe('engine');
    expect(fetcher).toHaveBeenCalledExactlyOnceWith('/1/keys', {
      headers: { 'x-algolia-application-id': 'flapjack' },
    });
  });

  it('creates an engine API key with the exact camelCase request and returned secret', async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        key: 'new-engine-secret',
        createdAt: '2026-08-17T07:00:00+00:00',
      })
    );
    const transport = createEngineTransport(fetcher);
    const request = {
      acl: ['search'],
      description: 'Browser key',
      indexes: ['products'],
      restrictSources: ['10.0.0.0/8'],
      validity: 3_600,
      queryParameters: 'typoTolerance=false',
      maxHitsPerQuery: 50,
      maxQueriesPerIPPerHour: 500,
    };

    await expect(transport.apiKeys?.create(request)).resolves.toEqual({
      key: 'new-engine-secret',
      createdAt: '2026-08-17T07:00:00+00:00',
    });
    expect(fetcher).toHaveBeenCalledExactlyOnceWith('/1/keys', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'x-algolia-application-id': 'flapjack',
      },
      body: JSON.stringify(request),
    });
  });

  it('encodes the engine secret only in the delete path and keeps it out of failures', async () => {
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        Response.json({ deletedAt: '2026-08-17T07:01:00+00:00' })
      )
      .mockResolvedValueOnce(new Response('unavailable', { status: 503 }));
    const transport = createEngineTransport(fetcher);

    await expect(transport.apiKeys?.remove('engine+/secret')).resolves.toBeUndefined();
    expect(fetcher).toHaveBeenNthCalledWith(1, '/1/keys/engine%2B%2Fsecret', {
      method: 'DELETE',
      headers: { 'x-algolia-application-id': 'flapjack' },
    });
    const deletionFailure = transport.apiKeys?.remove('do-not-leak-this-secret');
    await expect(deletionFailure).rejects.toThrow('Could not delete API key');
    await expect(deletionFailure).rejects.not.toThrow('do-not-leak-this-secret');
  });

  it('rejects malformed engine key list and create responses', async () => {
    const malformedListFetcher = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        keys: [
          {
            value: 'secret',
            createdAt: 1,
            acl: 'search',
          },
        ],
      })
    );
    const malformedCreateFetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValue(Response.json({ key: '', createdAt: 'not-a-date' }));

    await expect(createEngineTransport(malformedListFetcher).apiKeys?.list()).rejects.toThrow(
      'Invalid API key response'
    );
    await expect(
      createEngineTransport(malformedCreateFetcher).apiKeys?.create({ acl: ['search'] })
    ).rejects.toThrow('Invalid API key response');
  });

  it('maps the exact engine index-list envelope without retries or invented fields', async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        items: [
          {
            name: 'products',
            entries: 27,
            dataSize: 4096,
            createdAt: '2026-08-16T12:00:00Z',
            pendingTask: false,
          },
        ],
        nbPages: 1,
      })
    );

    const indexes = await createEngineTransport(fetcher).listIndexes();

    expect(indexes).toEqual([{ name: 'products', entries: 27, dataSize: 4096 }]);
    expect(fetcher).toHaveBeenCalledExactlyOnceWith('/1/indexes', {
      headers: { 'x-algolia-application-id': 'flapjack' },
    });
  });

  it.each([
    [false, ['cloud', 'remote']],
    [true, ['cloud', 'local', 'remote']],
  ])(
    'loads only query-capable embedders when local vector search is %s',
    async (vectorSearchLocal, expectedNames) => {
      const fetcher = vi
        .fn<typeof fetch>()
        .mockResolvedValueOnce(
          Response.json({
            status: 'ok',
            capabilities: { vectorSearch: true, vectorSearchLocal },
          })
        )
        .mockResolvedValueOnce(
          Response.json({
            mode: 'neuralSearch',
            embedders: {
              supplied: { source: 'userProvided', dimensions: 3 },
              local: { source: 'fastEmbed' },
              remote: { source: 'rest' },
              cloud: { source: 'openAi' },
            },
          })
        );
      const transport = createEngineTransport(fetcher);

      await expect(transport.searchSemantics?.load('products/us')).resolves.toEqual({
        configuredEmbedderCount: 4,
        queryEmbedderNames: expectedNames,
        mode: 'neuralSearch',
      });
      expect(fetcher).toHaveBeenNthCalledWith(1, '/health', {
        method: 'GET',
        cache: 'no-store',
      });
      expect(fetcher).toHaveBeenNthCalledWith(2, '/1/indexes/products%2Fus/settings', {
        method: 'GET',
        cache: 'no-store',
        headers: { 'x-algolia-application-id': 'flapjack' },
      });
    }
  );

  it('skips settings when vector search is unavailable', async () => {
    const unavailableFetcher = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        status: 'ok',
        capabilities: { vectorSearch: false, vectorSearchLocal: false },
      })
    );

    await expect(
      createEngineTransport(unavailableFetcher).searchSemantics?.load('products')
    ).resolves.toBeNull();
    expect(unavailableFetcher).toHaveBeenCalledOnce();
  });

  it.each([
    ['non-boolean flags', { vectorSearch: 'yes', vectorSearchLocal: false }],
    ['local vector search without vector search', { vectorSearch: false, vectorSearchLocal: true }],
  ])('rejects malformed health capabilities: %s', async (_case, capabilities) => {
    const malformedHealthFetcher = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        status: 'ok',
        capabilities,
      })
    );

    await expect(
      createEngineTransport(malformedHealthFetcher).searchSemantics?.load('products')
    ).rejects.toThrow('Invalid search semantics response');
  });

  it.each([
    ['invalid mode', { mode: 'semanticSearch', embedders: { safe: { source: 'rest' } } }],
    ['unknown source', { mode: 'neuralSearch', embedders: { unsafe: { source: 'future' } } }],
    ['malformed config', { mode: 'neuralSearch', embedders: { unsafe: null } }],
    ['blank name', { mode: 'neuralSearch', embedders: { ' ': { source: 'rest' } } }],
  ])('rejects malformed semantic settings: %s', async (_case, settings) => {
    const malformedSettingsFetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        Response.json({
          status: 'ok',
          capabilities: { vectorSearch: true, vectorSearchLocal: false },
        })
      )
      .mockResolvedValueOnce(Response.json(settings));

    await expect(
      createEngineTransport(malformedSettingsFetcher).searchSemantics?.load('products')
    ).rejects.toThrow('Invalid search semantics response');
  });

  it('maps exact keyword and neural requests and normalizes only semantic fallback presence', async () => {
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        Response.json({
          hits: [],
          nbHits: 0,
          page: 0,
          nbPages: 0,
          hitsPerPage: 20,
          processingTimeMS: 2,
        })
      )
      .mockResolvedValueOnce(
        Response.json({
          hits: [{ objectID: 'rust-language' }],
          nbHits: 1,
          page: 0,
          nbPages: 1,
          hitsPerPage: 20,
          processingTimeMS: 4,
          message: 'do-not-expose-backend-fallback-reason',
        })
      );
    const transport = createEngineTransport(fetcher);

    const keyword = await transport.searchIndex('products', {
      query: 'rust',
      page: 0,
      hitsPerPage: 20,
      mode: 'keywordSearch',
    });
    const neural = await transport.searchIndex('products', {
      query: 'rust',
      page: 0,
      hitsPerPage: 20,
      mode: 'neuralSearch',
      hybrid: { semanticRatio: 0.6, embedder: 'remote' },
    });

    expect(keyword.semanticFallback).toBeUndefined();
    expect(neural.semanticFallback).toBe(true);
    expect(JSON.stringify(neural)).not.toContain('do-not-expose-backend-fallback-reason');
    expect(fetcher.mock.calls[0]?.[1]?.body).toBe(
      JSON.stringify({ query: 'rust', page: 0, hitsPerPage: 20, mode: 'keywordSearch' })
    );
    expect(fetcher.mock.calls[1]?.[1]?.body).toBe(
      JSON.stringify({
        query: 'rust',
        page: 0,
        hitsPerPage: 20,
        mode: 'neuralSearch',
        hybrid: { semanticRatio: 0.6, embedder: 'remote' },
      })
    );
  });

  it('maps an exact basic search request and response', async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        hits: [{ objectID: 'sku-27', title: 'Red shoes' }],
        nbHits: 41,
        page: 1,
        nbPages: 3,
        hitsPerPage: 20,
        processingTimeMS: 7,
        query: 'red shoes',
      })
    );

    const result = await createEngineTransport(fetcher).searchIndex('products/us', {
      query: 'red shoes',
      page: 1,
      hitsPerPage: 20,
      analytics: false,
    });

    expect(result).toEqual({
      hits: [{ objectID: 'sku-27', title: 'Red shoes' }],
      nbHits: 41,
      page: 1,
      nbPages: 3,
      hitsPerPage: 20,
      processingTimeMs: 7,
    });
    expect(fetcher).toHaveBeenCalledExactlyOnceWith('/1/indexes/products%2Fus/query', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'x-algolia-application-id': 'flapjack',
      },
      body: JSON.stringify({ query: 'red shoes', page: 1, hitsPerPage: 20, analytics: false }),
    });
  });

  it('maps analytics Off and On searches plus one correlated result-open event', async () => {
    globalThis.sessionStorage.clear();
    const queryId = 'abcdef0123456789abcdef0123456789';
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        Response.json({
          hits: [{ objectID: 'sku-27' }],
          nbHits: 1,
          page: 0,
          nbPages: 1,
          hitsPerPage: 20,
          processingTimeMS: 3,
        })
      )
      .mockResolvedValueOnce(
        Response.json({
          hits: [{ objectID: 'sku-27' }],
          nbHits: 1,
          page: 0,
          nbPages: 1,
          hitsPerPage: 20,
          processingTimeMS: 4,
          queryID: queryId,
        })
      )
      .mockResolvedValueOnce(Response.json({ status: 'ok' }));
    const transport = createEngineTransport(fetcher);

    await transport.searchIndex('products', {
      query: 'red',
      page: 0,
      hitsPerPage: 20,
      analytics: false,
    });
    const onResult = await transport.searchIndex('products', {
      query: 'red',
      page: 0,
      hitsPerPage: 20,
      analytics: true,
      clickAnalytics: true,
    });
    expect(onResult.queryId).toBe(queryId);

    expect(fetcher).toHaveBeenNthCalledWith(1, '/1/indexes/products/query', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'x-algolia-application-id': 'flapjack',
      },
      body: JSON.stringify({ query: 'red', page: 0, hitsPerPage: 20, analytics: false }),
    });
    const onHeaders = fetcher.mock.calls[1]?.[1]?.headers as Record<string, string>;
    expect(onHeaders).toEqual({
      'Content-Type': 'application/json',
      'x-algolia-application-id': 'flapjack',
      'x-algolia-usertoken': expect.stringMatching(
        /^dashboard-[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
      ),
    });
    expect(fetcher.mock.calls[1]?.[1]?.body).toBe(
      JSON.stringify({
        query: 'red',
        page: 0,
        hitsPerPage: 20,
        analytics: true,
        clickAnalytics: true,
      })
    );

    const now = vi.spyOn(Date, 'now').mockReturnValue(1_766_000_000_000);
    await transport.searchAnalytics?.recordResultOpen({
      indexName: 'products',
      objectId: 'sku-27',
      position: 21,
      queryId,
    });
    expect(fetcher).toHaveBeenNthCalledWith(3, '/1/events', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'x-algolia-application-id': 'flapjack',
      },
      body: JSON.stringify({
        events: [
          {
            eventType: 'click',
            eventName: 'Result Clicked',
            index: 'products',
            userToken: onHeaders['x-algolia-usertoken'],
            queryID: queryId,
            objectIDs: ['sku-27'],
            positions: [21],
            timestamp: 1_766_000_000_000,
          },
        ],
      }),
    });
    now.mockRestore();
  });

  it.each([
    ['', 'blank'],
    ['abc', 'short'],
    ['a'.repeat(33), 'long'],
    ['g'.repeat(32), 'non-hex'],
    [42, 'wrong type'],
  ])('rejects a %s queryID producer value', async (queryID) => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        hits: [],
        nbHits: 0,
        page: 0,
        nbPages: 0,
        hitsPerPage: 20,
        processingTimeMS: 1,
        queryID,
      })
    );

    await expect(
      createEngineTransport(fetcher).searchIndex('products', {
        query: '',
        page: 0,
        hitsPerPage: 20,
        analytics: true,
        clickAnalytics: true,
      })
    ).rejects.toThrow('Invalid search response');
  });

  it('does not normalize a queryId alias and keeps event failures generic', async () => {
    globalThis.sessionStorage.clear();
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        Response.json({
          hits: [],
          nbHits: 0,
          page: 0,
          nbPages: 0,
          hitsPerPage: 20,
          processingTimeMS: 1,
          queryId: 'a'.repeat(32),
        })
      )
      .mockResolvedValueOnce(new Response('do-not-render', { status: 503 }));
    const transport = createEngineTransport(fetcher);
    const result = await transport.searchIndex('products', {
      query: '',
      page: 0,
      hitsPerPage: 20,
      analytics: true,
      clickAnalytics: true,
    });
    expect(result.queryId).toBeUndefined();

    const delivery = transport.searchAnalytics?.recordResultOpen({
      indexName: 'products',
      objectId: 'sku-secret',
      position: 1,
      queryId: 'a'.repeat(32),
    });
    await expect(delivery).rejects.toThrow('Could not record result open');
    await expect(delivery).rejects.not.toThrow('sku-secret');
    await expect(delivery).rejects.not.toThrow('do-not-render');
  });

  it('rejects failed and malformed responses instead of manufacturing usable data', async () => {
    const failedFetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValue(new Response('unavailable', { status: 503 }));
    const malformedFetcher = vi.fn<typeof fetch>().mockResolvedValue(
      Response.json({
        hits: [],
        nbHits: '41',
        page: 0,
        nbPages: 3,
        hitsPerPage: 20,
        processingTimeMS: 7,
      })
    );

    await expect(createEngineTransport(failedFetcher).listIndexes()).rejects.toThrow(
      'Could not load indexes'
    );
    await expect(
      createEngineTransport(malformedFetcher).searchIndex('products', {
        query: '',
        page: 0,
        hitsPerPage: 20,
      })
    ).rejects.toThrow('Invalid search response');
    expect(failedFetcher).toHaveBeenCalledOnce();
    expect(malformedFetcher).toHaveBeenCalledOnce();
  });
});
