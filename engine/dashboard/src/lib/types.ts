/**
 */
import type { RecommendationModelId } from './recommendation-contract';

/**
 * @module TypeScript type definitions for the dashboard's data model, covering indexes, search, documents, API keys, tasks, synonyms, rules, query suggestions, and experiments.
 */
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
/**
 * Parameters for executing a search query against an index.
 * 
 * @param query - The search query string.
 * @param filters - Filter expression to narrow results.
 * @param facets - Attributes to compute facet counts for.
 * @param facetFilters - Facet value filters (supports nested arrays for OR/AND logic).
 * @param numericFilters - Numeric comparison filters (e.g. `["price>10"]`).
 * @param page - Zero-based page number for pagination.
 * @param hitsPerPage - Number of hits to return per page.
 * @param attributesToRetrieve - Limits which attributes are included in each hit.
 * @param attributesToHighlight - Attributes to apply highlight markup to.
 * @param highlightPreTag - Opening tag inserted around highlighted matches.
 * @param highlightPostTag - Closing tag inserted around highlighted matches.
 * @param getRankingInfo - When true, includes ranking details in each hit.
 * @param aroundLatLng - Latitude/longitude string for geo search (e.g. `"48.8566,2.3522"`).
 * @param aroundRadius - Maximum geo search radius in meters, or `"all"` for unlimited.
 * @param sort - Attributes to sort results by (e.g. `["price:asc"]`).
 * @param distinct - De-duplicates results by the `attributeForDistinct` setting; a number sets the max duplicates kept.
 * @param analytics - Whether this query is counted in analytics.
 * @param clickAnalytics - Whether to attach a `queryID` for click analytics tracking.
 * @param analyticsTags - Tags to segment this query in analytics dashboards.
 * @param hybrid - Semantic/hybrid search parameters controlling the keyword-to-vector ratio.
 * @param mode - Switches between neural (vector) and keyword search modes.
 */
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
  analytics?: boolean;
  clickAnalytics?: boolean;
  analyticsTags?: string[];
  hybrid?: HybridSearchParams;
  mode?: IndexMode;
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
  queryID?: string;
  index?: string;
  exhaustiveNbHits?: boolean;
}

export interface FieldInfo {
  name: string;
  type: 'text' | 'number' | 'boolean';
}

export interface DisplayPreferences {
  titleAttribute: string | null;
  subtitleAttribute: string | null;
  imageAttribute: string | null;
  tagAttributes: string[];
}

// Chat / RAG types
export interface ChatRequest {
  query: string;
  model?: string;
  conversationHistory?: Array<Record<string, unknown>>;
  stream?: boolean;
  conversationId?: string;
}

export interface ChatResponse {
  answer: string;
  sources: Array<Record<string, unknown>>;
  conversationId: string;
  queryID: string;
}

// Document types
export interface Document {
  objectID: string;
  [key: string]: any;
}

// Vector search types
export type EmbedderSource = 'openAi' | 'rest' | 'userProvided' | 'fastEmbed';

export interface EmbedderConfig {
  source: EmbedderSource;
  model?: string;
  apiKey?: string;
  dimensions?: number;
  url?: string;
  request?: Record<string, unknown>;
  response?: Record<string, unknown>;
  headers?: Record<string, string>;
  documentTemplate?: string;
  documentTemplateMaxBytes?: number;
}

export type IndexMode = 'neuralSearch' | 'keywordSearch';

export interface SemanticSearchSettings {
  eventSources?: string[] | null;
}

export interface HybridSearchParams {
  semanticRatio?: number;
  embedder?: string;
}

// Settings types
/**
 * Configuration for an index's search behavior, ranking, and feature settings.
 * 
 * Controls which attributes are searchable, how results are ranked and highlighted,
 * typo tolerance thresholds, and optional vector/semantic search via embedders.
 */
export interface AiProviderSettings {
  baseUrl?: string;
  model?: string;
  apiKey?: string;
}

export interface UserData {
  aiProvider?: AiProviderSettings;
  [key: string]: unknown;
}

/**
 * TODO: Document IndexSettings.
 */
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
  embedders?: Record<string, EmbedderConfig>;
  mode?: IndexMode;
  semanticSearch?: SemanticSearchSettings;
  userData?: UserData;
}

// API Key types
export interface ApiKeyCreateResponse {
  key: string;
  createdAt: string;
}

export interface ApiKey {
  value: string;
  description?: string;
  acl: string[];
  indexes?: string[];
  restrictSources?: string[];
  expiresAt?: number;
  createdAt: number;
  updatedAt?: number;
  maxHitsPerQuery?: number;
  maxQueriesPerIPPerHour?: number;
  referers?: string[];
  queryParameters?: string;
  validity?: number;
}

// Security source types
export interface SecuritySourceEntry {
  source: string;
  description: string;
}

export interface SecuritySourceMutationResponse {
  updatedAt?: string;
  createdAt?: string;
  deletedAt?: string;
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

// Dictionary types
export type DictionaryName = 'stopwords' | 'plurals' | 'compounds';
export type DictionaryEntryType = 'custom' | 'standard';

interface DictionaryEntryBase {
  objectID: string;
  language: string;
  type?: DictionaryEntryType;
}

export interface StopwordEntry extends DictionaryEntryBase {
  word: string;
  state: 'enabled' | 'disabled';
}

export interface PluralEntry extends DictionaryEntryBase {
  words: string[];
}

export interface CompoundEntry extends DictionaryEntryBase {
  word: string;
  decomposition: string[];
}

export type DictionaryEntry = StopwordEntry | PluralEntry | CompoundEntry;

export interface DictionarySearchResponse {
  hits: DictionaryEntry[];
  nbHits: number;
  page: number;
  nbPages: number;
}

// Synonym types (tagged union matching Algolia API)
export type SynonymType = 'synonym' | 'onewaysynonym' | 'altcorrection1' | 'altcorrection2' | 'placeholder';

export type Synonym =
  | { type: 'synonym'; objectID: string; synonyms: string[] }
  | { type: 'onewaysynonym'; objectID: string; input: string; synonyms: string[] }
  | { type: 'altcorrection1'; objectID: string; word: string; corrections: string[] }
  | { type: 'altcorrection2'; objectID: string; word: string; corrections: string[] }
  | { type: 'placeholder'; objectID: string; placeholder: string; replacements: string[] };

export interface SynonymSearchResponse {
  hits: Synonym[];
  nbHits: number;
}

// Rule types (matching Algolia Rules API)
export interface Rule {
  objectID: string;
  conditions: RuleCondition[];
  consequence: RuleConsequence;
  description?: string;
  enabled?: boolean;
  validity?: TimeRange[];
}

export interface RuleCondition {
  pattern?: string;
  anchoring?: 'is' | 'startsWith' | 'endsWith' | 'contains';
  alternatives?: boolean;
  context?: string;
  filters?: string;
}

export type ConsequenceQuery =
  | string
  | {
      remove?: string[];
      edits?: Edit[];
    };

export interface Edit {
  type: 'remove' | 'replace';
  delete: string;
  insert?: string;
}

export interface AutomaticFacetFilter {
  facet: string;
  disjunctive?: boolean;
  score?: number;
  negative?: boolean;
}

export interface ConsequenceParams {
  query?: ConsequenceQuery;
  automaticFacetFilters?: Array<AutomaticFacetFilter | string>;
  automaticOptionalFacetFilters?: Array<AutomaticFacetFilter | string>;
  renderingContent?: Record<string, unknown>;
  filters?: string;
  facetFilters?: unknown;
  numericFilters?: unknown;
  optionalFilters?: unknown;
  tagFilters?: unknown;
  aroundLatLng?: string;
  aroundRadius?: number | 'all';
  hitsPerPage?: number;
  restrictSearchableAttributes?: string[];
}

export interface RuleConsequence {
  promote?: RulePromote[];
  hide?: RuleHide[];
  filterPromotes?: boolean;
  userData?: any;
  params?: ConsequenceParams;
}

export type RulePromote =
  | { objectID: string; position: number }
  | { objectIDs: string[]; position: number };

export interface RuleHide {
  objectID: string;
}

export interface TimeRange {
  from: number;
  until: number;
}

export interface RuleSearchResponse {
  hits: Rule[];
  nbHits: number;
  page: number;
  nbPages: number;
}

// Query Suggestions types
export interface QsSourceIndex {
  indexName: string;
  minHits?: number;
  minLetters?: number;
  facets?: Array<{ attribute: string; amount: number }>;
  generate?: string[][];
  analyticsTags?: string[];
  replicas?: boolean;
}

export interface QsConfig {
  indexName: string;
  sourceIndices: QsSourceIndex[];
  languages?: string[];
  exclude?: string[];
  allowSpecialCharacters?: boolean;
  enablePersonalization?: boolean;
}

export interface QsBuildStatus {
  indexName: string;
  isRunning: boolean;
  lastBuiltAt: string | null;
  lastSuccessfulBuiltAt: string | null;
}

export interface QsLogEntry {
  timestamp: string;
  level: string;
  message: string;
  contextLevel: number;
}

// Experiments types
export type ExperimentStatus = 'draft' | 'running' | 'stopped' | 'concluded' | 'expired';

export interface ExperimentArmConfig {
  name: string;
  queryOverrides?: Record<string, unknown>;
  indexName?: string;
}

export interface Experiment {
  id: string;
  name: string;
  indexName: string;
  status: ExperimentStatus;
  trafficSplit: number;
  control: ExperimentArmConfig;
  variant: ExperimentArmConfig;
  primaryMetric: string;
  createdAt: number;
  startedAt?: number | null;
  endedAt?: number | null;
  minimumDays: number;
  winsorizationCap?: number | null;
}

// Recommendations types
export type RecommendationModel = RecommendationModelId;

export interface RecommendationRequest {
  indexName: string;
  model: RecommendationModel;
  objectID?: string;
  threshold?: number;
  maxRecommendations?: number;
  facetName?: string;
  facetValue?: string;
  queryParameters?: Record<string, unknown>;
  fallbackParameters?: Record<string, unknown>;
}

export interface RecommendationBatchRequest {
  requests: RecommendationRequest[];
}

export interface RecommendationItemHit extends Record<string, unknown> {
  objectID: string;
  _score: number;
}

export interface RecommendationTrendingFacetHit {
  facetName: string;
  facetValue: string;
  _score: number;
}

export type RecommendationHit = RecommendationItemHit | RecommendationTrendingFacetHit;

export interface RecommendationResult {
  hits: RecommendationHit[];
  processingTimeMS: number;
}

export interface RecommendationBatchResponse {
  results: RecommendationResult[];
}

// Personalization types
export type PersonalizationEventType = 'click' | 'conversion' | 'view';

export interface EventScoring {
  eventName: string;
  eventType: PersonalizationEventType;
  score: number;
}

export interface FacetScoring {
  facetName: string;
  score: number;
}

export interface PersonalizationStrategy {
  eventsScoring: EventScoring[];
  facetsScoring: FacetScoring[];
  personalizationImpact: number;
}

export interface PersonalizationProfile {
  userToken: string;
  lastEventAt: string | null;
  scores: Record<string, Record<string, number>>;
}
