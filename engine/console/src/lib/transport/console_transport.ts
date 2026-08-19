export type IndexSummary = {
  name: string;
  entries: number;
  dataSize: number;
};

export type SearchRequest = {
  query: string;
  page: number;
  hitsPerPage: number;
  analytics?: boolean;
  clickAnalytics?: boolean;
  mode?: 'keywordSearch' | 'neuralSearch';
  hybrid?: {
    semanticRatio: number;
    embedder: string;
  };
};

export type SearchPage = {
  hits: ReadonlyArray<Record<string, unknown>>;
  nbHits: number;
  page: number;
  nbPages: number;
  hitsPerPage: number;
  processingTimeMs: number;
  queryId?: string;
  semanticFallback?: boolean;
};

export type SearchSemantics = {
  configuredEmbedderCount: number;
  queryEmbedderNames: ReadonlyArray<string>;
  mode: 'keywordSearch' | 'neuralSearch';
};

export interface SearchSemanticsCapability {
  load(indexName: string): Promise<SearchSemantics | null>;
}

export type SearchResultOpenIntent = {
  indexName: string;
  objectId: string;
  position: number;
  queryId: string;
};

export interface SearchAnalyticsCapability {
  recordResultOpen(intent: SearchResultOpenIntent): Promise<void>;
}

export type EngineApiKey = {
  value: string;
  createdAt: number;
  acl: string[];
  description: string;
  indexes: string[];
  maxHitsPerQuery: number;
  maxQueriesPerIPPerHour: number;
  queryParameters: string;
  referers: string[];
  restrictSources?: string[];
  validity: number;
};

export type CreateEngineApiKeyRequest = {
  acl: string[];
  description?: string;
  indexes?: string[];
  maxHitsPerQuery?: number;
  maxQueriesPerIPPerHour?: number;
  restrictSources?: string[];
};

export type CreatedEngineApiKey = {
  key: string;
  createdAt: string;
};

export interface EngineApiKeysCapability {
  kind: 'engine';
  list(): Promise<EngineApiKey[]>;
  create(request: CreateEngineApiKeyRequest): Promise<CreatedEngineApiKey>;
  remove(value: string): Promise<void>;
}

export type SecuritySource = {
  source: string;
  description: string;
};

export interface EngineSecuritySourcesCapability {
  kind: 'engine';
  list(): Promise<SecuritySource[]>;
  append(entry: SecuritySource): Promise<void>;
  remove(source: string): Promise<void>;
}

export interface ConsoleTransport {
  apiKeys?: EngineApiKeysCapability;
  securitySources?: EngineSecuritySourcesCapability;
  searchAnalytics?: SearchAnalyticsCapability;
  searchSemantics?: SearchSemanticsCapability;
  listIndexes(): Promise<IndexSummary[]>;
  searchIndex(indexName: string, request: SearchRequest): Promise<SearchPage>;
}
