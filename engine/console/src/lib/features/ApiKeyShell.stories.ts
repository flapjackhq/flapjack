import ApiKeyShell from './ApiKeyShell.svelte';

export const populatedApiKeyShellStory = {
  name: 'API key interaction shell — populated',
  component: ApiKeyShell,
  props: {
    state: {
      kind: 'ready' as const,
      keys: [
        {
          opaqueId: 'story-key',
          displayName: 'Search client',
          indexNames: ['products'],
          copyText: 'story-copy-value',
        },
      ],
    },
    filterOptions: ['products'],
    selectedFilter: '',
    copyText: async () => undefined,
  },
};
