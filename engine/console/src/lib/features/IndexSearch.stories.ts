import IndexSearch from './IndexSearch.svelte';
import type { ConsoleTransport } from '../transport/console_transport';

const storyTransport: ConsoleTransport = {
  searchSemantics: {
    async load() {
      return {
        configuredEmbedderCount: 2,
        queryEmbedderNames: ['default', 'remote'],
        mode: 'neuralSearch',
      };
    },
  },
  searchAnalytics: {
    async recordResultOpen() {},
  },
  async listIndexes() {
    return [];
  },
  async searchIndex() {
    return {
      hits: [{ objectID: 'story-result', title: 'Story result' }],
      nbHits: 1,
      page: 0,
      nbPages: 1,
      hitsPerPage: 20,
      processingTimeMs: 1,
      queryId: 'abcdef0123456789abcdef0123456789',
    };
  },
};

export const readyIndexSearchStory = {
  name: 'Portable Index Search — ready',
  component: IndexSearch,
  props: {
    transport: storyTransport,
    indexName: 'products',
    searchAnalyticsCopy: {
      toggleLabel: 'Track Analytics',
    },
  },
};
