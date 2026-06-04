import type { IndexSettings } from '../../lib/types'

export interface IndexTabDefinition {
  id: string
  label: string
  relativePath: string
  end?: boolean
  isVisible: (settings: IndexSettings | undefined, vectorSearchEnabled: boolean | undefined) => boolean
}

const ALWAYS_VISIBLE = () => true
const IS_CHAT_AVAILABLE = (
  settings: IndexSettings | undefined,
  vectorSearchEnabled: boolean | undefined,
) => settings?.mode === 'neuralSearch' && vectorSearchEnabled !== false

export const INDEX_TAB_DEFINITIONS: readonly IndexTabDefinition[] = [
  {
    id: 'browse',
    label: 'Browse',
    relativePath: '',
    end: true,
    isVisible: ALWAYS_VISIBLE,
  },
  {
    id: 'settings',
    label: 'Settings',
    relativePath: 'settings',
    isVisible: ALWAYS_VISIBLE,
  },
  {
    id: 'analytics',
    label: 'Analytics',
    relativePath: 'analytics',
    isVisible: ALWAYS_VISIBLE,
  },
  {
    id: 'synonyms',
    label: 'Synonyms',
    relativePath: 'synonyms',
    isVisible: ALWAYS_VISIBLE,
  },
  {
    id: 'rules',
    label: 'Rules',
    relativePath: 'rules',
    isVisible: ALWAYS_VISIBLE,
  },
  {
    id: 'merchandising',
    label: 'Merchandising',
    relativePath: 'merchandising',
    isVisible: ALWAYS_VISIBLE,
  },
  {
    id: 'recommendations',
    label: 'Recommendations',
    relativePath: 'recommendations',
    isVisible: ALWAYS_VISIBLE,
  },
  {
    id: 'chat',
    label: 'Chat',
    relativePath: 'chat',
    isVisible: IS_CHAT_AVAILABLE,
  },
]

export function getVisibleIndexTabs(
  settings: IndexSettings | undefined,
  vectorSearchEnabled: boolean | undefined,
): readonly IndexTabDefinition[] {
  return INDEX_TAB_DEFINITIONS.filter((tabDefinition) => tabDefinition.isVisible(settings, vectorSearchEnabled))
}

export function buildIndexTabHref(indexName: string, relativePath: string): string {
  const encodedIndexName = encodeURIComponent(indexName)
  if (!relativePath) {
    return `/index/${encodedIndexName}`
  }

  return `/index/${encodedIndexName}/${relativePath}`
}
