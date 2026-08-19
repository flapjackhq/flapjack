/**
 */
import type { DictionaryEntry, DictionaryName } from '@/lib/types';

export const DICTIONARY_LABELS: Record<DictionaryName, string> = {
  stopwords: 'Stopwords',
  plurals: 'Plurals',
  compounds: 'Compounds',
};

export const DICTIONARY_EMPTY_STATES: Record<DictionaryName, string> = {
  stopwords: 'No stopword entries yet.',
  plurals: 'No plural entries yet.',
  compounds: 'No compound entries yet.',
};

export const LANGUAGE_OPTIONS = ['en', 'fr', 'de', 'es', 'it', 'pt', 'nl', 'sv'];

export const LANGUAGE_SELECT_CLASS =
  'h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm';
export const DICTIONARY_NAMES: DictionaryName[] = ['stopwords', 'plurals', 'compounds'];

export interface StopwordFormState {
  word: string;
  language: string;
  state: 'enabled' | 'disabled';
}

export interface PluralFormState {
  words: string;
  language: string;
}

export interface CompoundFormState {
  word: string;
  decomposition: string;
  language: string;
}

export function createStopwordFormState(): StopwordFormState {
  return { word: '', language: 'en', state: 'enabled' };
}

export function createPluralFormState(): PluralFormState {
  return { words: '', language: 'en' };
}

export function createCompoundFormState(): CompoundFormState {
  return { word: '', decomposition: '', language: 'en' };
}

export function isDictionaryName(value: string): value is DictionaryName {
  return DICTIONARY_NAMES.includes(value as DictionaryName);
}

export function getListTestId(dictName: DictionaryName): string {
  return `dictionaries-${dictName}-list`;
}

export function splitCommaSeparatedValues(input: string): string[] {
  return input
    .split(',')
    .map((value) => value.trim())
    .filter((value) => value.length > 0);
}

let dictionaryObjectIdSequence = 0;

/**
 * TODO: Document buildObjectIdSuffix.
 */
function buildObjectIdSuffix(): string {
  if (typeof globalThis.crypto?.randomUUID === 'function') {
    return globalThis.crypto.randomUUID();
  }

  dictionaryObjectIdSequence += 1;
  const sequenceSuffix = dictionaryObjectIdSequence.toString(36);

  if (typeof globalThis.crypto?.getRandomValues === 'function') {
    const randomBuffer = new Uint32Array(1);
    globalThis.crypto.getRandomValues(randomBuffer);
    return `${randomBuffer[0].toString(36)}-${sequenceSuffix}`;
  }

  const randomNumberSuffix = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER).toString(36);
  return `${randomNumberSuffix}-${sequenceSuffix}`;
}

export function buildObjectId(prefix: DictionaryName): string {
  return `${prefix}-${Date.now()}-${buildObjectIdSuffix()}`;
}

export function isStopwordEntry(entry: DictionaryEntry): entry is DictionaryEntry & { state: 'enabled' | 'disabled'; word: string } {
  return 'state' in entry && 'word' in entry;
}

export function isPluralEntry(entry: DictionaryEntry): entry is DictionaryEntry & { words: string[] } {
  return 'words' in entry;
}

export function buildEntryDescription(entry: DictionaryEntry): string {
  if (isStopwordEntry(entry)) {
    return entry.word;
  }

  if (isPluralEntry(entry)) {
    return entry.words.join(', ');
  }

  return `${entry.word} -> ${entry.decomposition.join(' + ')}`;
}

/**
 * TODO: Document buildDialogEntry.
 */
export function buildDialogEntry(
  dictName: DictionaryName,
  stopwordForm: StopwordFormState,
  pluralForm: PluralFormState,
  compoundForm: CompoundFormState,
): DictionaryEntry | null {
  if (dictName === 'stopwords') {
    const trimmedWord = stopwordForm.word.trim();
    if (!trimmedWord) {
      return null;
    }

    return {
      objectID: buildObjectId('stopwords'),
      word: trimmedWord,
      language: stopwordForm.language,
      state: stopwordForm.state,
    };
  }

  if (dictName === 'plurals') {
    const words = splitCommaSeparatedValues(pluralForm.words);
    if (words.length === 0) {
      return null;
    }

    return {
      objectID: buildObjectId('plurals'),
      words,
      language: pluralForm.language,
    };
  }

  const trimmedWord = compoundForm.word.trim();
  const decomposition = splitCommaSeparatedValues(compoundForm.decomposition);
  if (!trimmedWord || decomposition.length === 0) {
    return null;
  }

  return {
    objectID: buildObjectId('compounds'),
    word: trimmedWord,
    decomposition,
    language: compoundForm.language,
  };
}
