// Index types
export interface Index {
  uid: string;
  name?: string;
  createdAt?: string;
  updatedAt?: string;
  primaryKey?: string;
  entries?: number;
  dataSize?: number;
  fileSize?: number;
  numberOfPendingTasks?: number;
}

// Search types
export interface SearchParams {
  query?: string;
  filters?: string;
  facets?: string[];
  facetFilters?: any[];
  numericFilters?: string[];
  page?: number;
  hitsPerPage?: number;
  attributesToRetrieve?: string[];
  attributesToHighlight?: string[];
  highlightPreTag?: string;
  highlightPostTag?: string;
  getRankingInfo?: boolean;
  aroundLatLng?: string;
  aroundRadius?: number | "all";
  sort?: string[];
  distinct?: boolean | number;
}

export interface SearchResponse<T = any> {
  hits: T[];
  nbHits: number;
  page: number;
  nbPages: number;
  hitsPerPage: number;
  processingTimeMS: number;
  facets?: Record<string, Record<string, number>>;
  query: string;
  index?: string;
  exhaustiveNbHits?: boolean;
}

// Document types
export interface Document {
  objectID: string;
  [key: string]: any;
}

// Settings types
export interface IndexSettings {
  searchableAttributes?: string[];
  attributesForFaceting?: string[];
  ranking?: string[];
  customRanking?: string[];
  attributesToRetrieve?: string[];
  unretrievableAttributes?: string[];
  attributesToHighlight?: string[];
  highlightPreTag?: string;
  highlightPostTag?: string;
  hitsPerPage?: number;
  removeStopWords?: boolean | string[];
  ignorePlurals?: boolean | string[];
  queryLanguages?: string[];
  queryType?: "prefixLast" | "prefixAll" | "prefixNone";
  minWordSizefor1Typo?: number;
  minWordSizefor2Typos?: number;
  distinct?: boolean | number;
  attributeForDistinct?: string;
}

// API Key types
export interface ApiKey {
  value: string;
  description?: string;
  acl: string[];
  indexes?: string[];
  expiresAt?: number;
  createdAt: number;
  updatedAt?: number;
  maxHitsPerQuery?: number;
  maxQueriesPerIPPerHour?: number;
  referers?: string[];
  queryParameters?: string;
  validity?: number;
}

// Task types
export interface Task {
  task_uid: number;
  status: "notPublished" | "published" | "error";
  type: string;
  indexUid?: string;
  received_documents?: number;
  indexed_documents?: number;
  rejected_documents?: any[];
  rejected_count?: number;
  error?: string;
  enqueuedAt?: string;
  startedAt?: string;
  finishedAt?: string;
  duration?: string;
}

// Health types
export interface HealthStatus {
  status: string;
  [key: string]: any;
}
