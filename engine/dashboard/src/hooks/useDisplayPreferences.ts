/**
 */
import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { DisplayPreferences, FieldInfo } from '@/lib/types';

const DISPLAY_PREFERENCES_PERSIST_KEY = 'flapjack-display-prefs';

const TITLE_FIELD_CANDIDATES = ['name', 'title', 'label', 'headline', 'product_name'];
const IMAGE_FIELD_CANDIDATES = ['image', 'image_url', 'imageUrl', 'thumbnail', 'photo', 'picture', 'img'];
const TAG_FIELD_CANDIDATES = ['tags'];

interface DisplayPreferencesStore {
  preferencesByIndex: Record<string, DisplayPreferences>;
  getPreferences: (indexName: string) => DisplayPreferences | null;
  setPreferences: (indexName: string, preferences: DisplayPreferences) => void;
  clearPreferences: (indexName: string) => void;
}

function normalizeFieldName(fieldName: string): string {
  return fieldName.toLowerCase().replace(/[^a-z0-9]/g, '');
}

function buildNormalizedFieldNameMap(fields: FieldInfo[]): Map<string, string> {
  const fieldNameByNormalizedValue = new Map<string, string>();

  for (const field of fields) {
    const normalizedFieldName = normalizeFieldName(field.name);
    if (!fieldNameByNormalizedValue.has(normalizedFieldName)) {
      fieldNameByNormalizedValue.set(normalizedFieldName, field.name);
    }
  }

  return fieldNameByNormalizedValue;
}

function findFirstCandidate(
  fieldNameByNormalizedValue: Map<string, string>,
  candidates: string[]
): string | null {

  for (const candidate of candidates) {
    const normalizedCandidate = normalizeFieldName(candidate);
    const matchedFieldName = fieldNameByNormalizedValue.get(normalizedCandidate);
    if (matchedFieldName) {
      return matchedFieldName;
    }
  }

  return null;
}

const useDisplayPreferencesStore = create<DisplayPreferencesStore>()(
  persist(
    (set, get) => ({
      preferencesByIndex: {},
      getPreferences: (indexName: string) => {
        if (!indexName) {
          return null;
        }

        return get().preferencesByIndex[indexName] ?? null;
      },
      setPreferences: (indexName: string, preferences: DisplayPreferences) => {
        if (!indexName) {
          return;
        }

        set((state) => ({
          preferencesByIndex: {
            ...state.preferencesByIndex,
            [indexName]: preferences,
          },
        }));
      },
      clearPreferences: (indexName: string) => {
        if (!indexName) {
          return;
        }

        set((state) => {
          if (!state.preferencesByIndex[indexName]) {
            return state;
          }

          const { [indexName]: _removed, ...remainingPreferences } = state.preferencesByIndex;
          return { preferencesByIndex: remainingPreferences };
        });
      },
    }),
    {
      name: DISPLAY_PREFERENCES_PERSIST_KEY,
    }
  )
);

export function getPreferences(indexName: string): DisplayPreferences | null {
  return useDisplayPreferencesStore.getState().getPreferences(indexName);
}

export function setPreferences(indexName: string, preferences: DisplayPreferences): void {
  useDisplayPreferencesStore.getState().setPreferences(indexName, preferences);
}

export function clearPreferences(indexName: string): void {
  useDisplayPreferencesStore.getState().clearPreferences(indexName);
}

/**
 * TODO: Document autoDetectPreferences.
 */
export function autoDetectPreferences(fields: FieldInfo[]): DisplayPreferences | null {
  if (fields.length === 0) {
    return null;
  }

  const normalizedFieldNames = buildNormalizedFieldNameMap(fields);
  const titleAttribute = findFirstCandidate(normalizedFieldNames, TITLE_FIELD_CANDIDATES);
  const imageAttribute = findFirstCandidate(normalizedFieldNames, IMAGE_FIELD_CANDIDATES);
  const tagAttribute = findFirstCandidate(normalizedFieldNames, TAG_FIELD_CANDIDATES);

  if (!titleAttribute && !imageAttribute && !tagAttribute) {
    return null;
  }

  return {
    titleAttribute,
    subtitleAttribute: null,
    imageAttribute,
    tagAttributes: tagAttribute ? [tagAttribute] : [],
  };
}

export function useDisplayPreferences(indexName: string) {
  const preferences = useDisplayPreferencesStore((state) => state.getPreferences(indexName));

  return {
    preferences,
    getPreferences,
    setPreferences,
    clearPreferences,
  };
}
