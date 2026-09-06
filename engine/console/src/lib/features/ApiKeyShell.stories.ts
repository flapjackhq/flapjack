import ApiKeyShell from './ApiKeyShell.svelte';

export const loadingApiKeyShellStory = {
  name: 'API key interaction shell — loading',
  component: ApiKeyShell,
  props: {
    state: { kind: 'loading' as const },
  },
};

export const errorApiKeyShellStory = {
  name: 'API key interaction shell — error',
  component: ApiKeyShell,
  props: {
    state: { kind: 'error' as const, message: 'API keys are temporarily unavailable.' },
    onRetry: () => undefined,
  },
};

export const emptyApiKeyShellStory = {
  name: 'API key interaction shell — empty',
  component: ApiKeyShell,
  props: {
    state: { kind: 'ready' as const, keys: [] },
    onCreate: () => undefined,
  },
};

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
    onCreate: () => undefined,
    copyText: async () => undefined,
    onRequestRemove: () => undefined,
  },
};
