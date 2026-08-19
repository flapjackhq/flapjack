import type {
  ConsoleTransport,
  CreatedEngineApiKey,
  EngineApiKey,
  IndexSummary,
  SecuritySource,
  SearchPage,
  SearchRequest,
  SearchSemantics,
} from '../lib/transport/console_transport';

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isNonNegativeInteger(value: unknown): value is number {
  return Number.isInteger(value) && (value as number) >= 0;
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === 'string');
}

function isTimestamp(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && Number.isFinite(Date.parse(value));
}

function isEpochMillis(value: unknown): value is number {
  return (
    Number.isSafeInteger(value) &&
    (value as number) >= 0 &&
    Number.isFinite(new Date(value as number).getTime())
  );
}

function parseEngineApiKey(value: unknown): EngineApiKey {
  if (
    !isRecord(value) ||
    typeof value.value !== 'string' ||
    value.value.length === 0 ||
    !isEpochMillis(value.createdAt) ||
    !isStringArray(value.acl) ||
    typeof value.description !== 'string' ||
    !isStringArray(value.indexes) ||
    !isNonNegativeInteger(value.maxHitsPerQuery) ||
    !isNonNegativeInteger(value.maxQueriesPerIPPerHour) ||
    typeof value.queryParameters !== 'string' ||
    !isStringArray(value.referers) ||
    (value.restrictSources !== undefined && !isStringArray(value.restrictSources)) ||
    !isNonNegativeInteger(value.validity)
  ) {
    throw new Error('Invalid API key response');
  }

  return {
    value: value.value,
    createdAt: value.createdAt,
    acl: value.acl,
    description: value.description,
    indexes: value.indexes,
    maxHitsPerQuery: value.maxHitsPerQuery,
    maxQueriesPerIPPerHour: value.maxQueriesPerIPPerHour,
    queryParameters: value.queryParameters,
    referers: value.referers,
    ...(value.restrictSources === undefined ? {} : { restrictSources: value.restrictSources }),
    validity: value.validity,
  };
}

function parseEngineApiKeyList(value: unknown): EngineApiKey[] {
  if (!isRecord(value) || !Array.isArray(value.keys)) {
    throw new Error('Invalid API key response');
  }
  return value.keys.map(parseEngineApiKey);
}

function parseCreatedEngineApiKey(value: unknown): CreatedEngineApiKey {
  if (
    !isRecord(value) ||
    typeof value.key !== 'string' ||
    value.key.length === 0 ||
    !isTimestamp(value.createdAt)
  ) {
    throw new Error('Invalid API key response');
  }
  return { key: value.key, createdAt: value.createdAt };
}

function parseDeletedEngineApiKey(value: unknown): void {
  if (!isRecord(value) || !isTimestamp(value.deletedAt)) {
    throw new Error('Invalid API key response');
  }
}

function parseSecuritySource(value: unknown): SecuritySource {
  if (
    !isRecord(value) ||
    typeof value.source !== 'string' ||
    value.source.trim().length === 0 ||
    typeof value.description !== 'string'
  ) {
    throw new Error('Invalid security source response');
  }
  return { source: value.source, description: value.description };
}

function parseSecuritySources(value: unknown): SecuritySource[] {
  if (!Array.isArray(value)) throw new Error('Invalid security source response');
  return value.map(parseSecuritySource);
}

function parseSecuritySourceMutation(value: unknown, timestamp: 'createdAt' | 'deletedAt'): void {
  if (!isRecord(value) || !isTimestamp(value[timestamp])) {
    throw new Error('Invalid security source response');
  }
}

function parseIndexList(value: unknown): IndexSummary[] {
  if (!isRecord(value) || !Array.isArray(value.items)) throw new Error('Invalid index response');

  return value.items.map((item) => {
    if (
      !isRecord(item) ||
      typeof item.name !== 'string' ||
      item.name.length === 0 ||
      !isNonNegativeInteger(item.entries) ||
      !isNonNegativeInteger(item.dataSize)
    ) {
      throw new Error('Invalid index response');
    }
    return { name: item.name, entries: item.entries, dataSize: item.dataSize };
  });
}

function parseSearchPage(value: unknown, semanticRequested = false): SearchPage {
  if (
    !isRecord(value) ||
    !Array.isArray(value.hits) ||
    !value.hits.every(isRecord) ||
    !isNonNegativeInteger(value.nbHits) ||
    !isNonNegativeInteger(value.page) ||
    !isNonNegativeInteger(value.nbPages) ||
    !isNonNegativeInteger(value.hitsPerPage) ||
    !isNonNegativeInteger(value.processingTimeMS)
  ) {
    throw new Error('Invalid search response');
  }
  if (
    value.queryID !== undefined &&
    (typeof value.queryID !== 'string' || !/^[0-9a-fA-F]{32}$/.test(value.queryID))
  ) {
    throw new Error('Invalid search response');
  }
  if (
    value.message !== undefined &&
    (typeof value.message !== 'string' || value.message.trim().length === 0)
  ) {
    throw new Error('Invalid search response');
  }

  return {
    hits: value.hits,
    nbHits: value.nbHits,
    page: value.page,
    nbPages: value.nbPages,
    hitsPerPage: value.hitsPerPage,
    processingTimeMs: value.processingTimeMS,
    ...(value.queryID === undefined ? {} : { queryId: value.queryID }),
    ...(semanticRequested && value.message !== undefined ? { semanticFallback: true } : {}),
  };
}

function parseSearchCapabilities(value: unknown): {
  vectorSearch: boolean;
  vectorSearchLocal: boolean;
} {
  if (
    !isRecord(value) ||
    value.status !== 'ok' ||
    !isRecord(value.capabilities) ||
    typeof value.capabilities.vectorSearch !== 'boolean' ||
    typeof value.capabilities.vectorSearchLocal !== 'boolean' ||
    (value.capabilities.vectorSearchLocal && !value.capabilities.vectorSearch)
  ) {
    throw new Error('Invalid search semantics response');
  }
  return {
    vectorSearch: value.capabilities.vectorSearch,
    vectorSearchLocal: value.capabilities.vectorSearchLocal,
  };
}

function parseSearchSemantics(value: unknown, vectorSearchLocal: boolean): SearchSemantics {
  if (!isRecord(value)) throw new Error('Invalid search semantics response');
  const mode = value.mode === undefined ? 'keywordSearch' : value.mode;
  if (mode !== 'keywordSearch' && mode !== 'neuralSearch') {
    throw new Error('Invalid search semantics response');
  }
  const embedders = value.embedders === undefined ? {} : value.embedders;
  if (!isRecord(embedders)) throw new Error('Invalid search semantics response');

  const queryEmbedderNames: string[] = [];
  for (const [name, config] of Object.entries(embedders)) {
    if (name.trim().length === 0 || !isRecord(config)) {
      throw new Error('Invalid search semantics response');
    }
    const source = config.source;
    if (
      typeof source !== 'string' ||
      !['openAi', 'rest', 'userProvided', 'fastEmbed'].includes(source)
    ) {
      throw new Error('Invalid search semantics response');
    }
    if (source === 'openAi' || source === 'rest' || (source === 'fastEmbed' && vectorSearchLocal)) {
      queryEmbedderNames.push(name);
    }
  }
  queryEmbedderNames.sort();
  return {
    configuredEmbedderCount: Object.keys(embedders).length,
    queryEmbedderNames,
    mode,
  };
}

export function createEngineTransport(fetcher: typeof fetch): ConsoleTransport {
  const applicationHeaders = { 'x-algolia-application-id': 'flapjack' };
  const analyticsTokenStorageKey = 'fj-dashboard-user-token';
  let activeAnalyticsToken: string | null = null;

  function searchAnalyticsToken(): string {
    const existing = globalThis.sessionStorage?.getItem(analyticsTokenStorageKey);
    if (existing) return existing;
    const token = `dashboard-${globalThis.crypto.randomUUID()}`;
    globalThis.sessionStorage?.setItem(analyticsTokenStorageKey, token);
    return token;
  }

  return {
    securitySources: {
      kind: 'engine',
      async list() {
        const response = await fetcher('/1/security/sources', { headers: applicationHeaders });
        if (!response.ok) throw new Error('Could not load security sources');
        return parseSecuritySources(await response.json());
      },
      async append(entry) {
        const response = await fetcher('/1/security/sources/append', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            ...applicationHeaders,
          },
          body: JSON.stringify(entry),
        });
        if (!response.ok) throw new Error('Could not add security source');
        parseSecuritySourceMutation(await response.json(), 'createdAt');
      },
      async remove(source) {
        const response = await fetcher(`/1/security/sources/${encodeURIComponent(source)}`, {
          method: 'DELETE',
          headers: applicationHeaders,
        });
        if (!response.ok) throw new Error('Could not delete security source');
        parseSecuritySourceMutation(await response.json(), 'deletedAt');
      },
    },
    searchSemantics: {
      async load(indexName) {
        const healthResponse = await fetcher('/health', {
          method: 'GET',
          cache: 'no-store',
        });
        if (!healthResponse.ok) throw new Error('Could not load semantic search options');
        const capabilities = parseSearchCapabilities(await healthResponse.json());
        if (!capabilities.vectorSearch) return null;

        const settingsResponse = await fetcher(
          `/1/indexes/${encodeURIComponent(indexName)}/settings`,
          {
            method: 'GET',
            cache: 'no-store',
            headers: applicationHeaders,
          }
        );
        if (!settingsResponse.ok) throw new Error('Could not load semantic search options');
        return parseSearchSemantics(
          await settingsResponse.json(),
          capabilities.vectorSearchLocal
        );
      },
    },
    searchAnalytics: {
      async recordResultOpen({ indexName, objectId, position, queryId }) {
        if (!activeAnalyticsToken) throw new Error('Could not record result open');
        const response = await fetcher('/1/events', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            ...applicationHeaders,
          },
          body: JSON.stringify({
            events: [
              {
                eventType: 'click',
                eventName: 'Result Clicked',
                index: indexName,
                userToken: activeAnalyticsToken,
                queryID: queryId,
                objectIDs: [objectId],
                positions: [position],
                timestamp: Date.now(),
              },
            ],
          }),
        });
        if (!response.ok) throw new Error('Could not record result open');
      },
    },
    apiKeys: {
      kind: 'engine',
      async list() {
        const response = await fetcher('/1/keys', { headers: applicationHeaders });
        if (!response.ok) throw new Error('Could not load API keys');
        return parseEngineApiKeyList(await response.json());
      },
      async create(request) {
        const response = await fetcher('/1/keys', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            ...applicationHeaders,
          },
          body: JSON.stringify(request),
        });
        if (!response.ok) throw new Error('Could not create API key');
        return parseCreatedEngineApiKey(await response.json());
      },
      async remove(value) {
        const response = await fetcher(`/1/keys/${encodeURIComponent(value)}`, {
          method: 'DELETE',
          headers: applicationHeaders,
        });
        if (!response.ok) throw new Error('Could not delete API key');
        parseDeletedEngineApiKey(await response.json());
      },
    },
    async listIndexes() {
      const response = await fetcher('/1/indexes', { headers: applicationHeaders });
      if (!response.ok) throw new Error('Could not load indexes');
      return parseIndexList(await response.json());
    },

    async searchIndex(indexName: string, request: SearchRequest) {
      const searchHeaders: Record<string, string> = {
        'Content-Type': 'application/json',
        ...applicationHeaders,
      };
      if (request.analytics === true) {
        activeAnalyticsToken = searchAnalyticsToken();
        searchHeaders['x-algolia-usertoken'] = activeAnalyticsToken;
      } else {
        activeAnalyticsToken = null;
      }
      const response = await fetcher(`/1/indexes/${encodeURIComponent(indexName)}/query`, {
        method: 'POST',
        headers: searchHeaders,
        body: JSON.stringify(request),
      });
      if (!response.ok) throw new Error('Could not search index');
      return parseSearchPage(
        await response.json(),
        request.mode === 'neuralSearch' && (request.hybrid?.semanticRatio ?? 0) > 0
      );
    },
  };
}
