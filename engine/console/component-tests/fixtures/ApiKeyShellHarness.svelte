<script lang="ts">
  import ApiKeyShell, {
    type ApiKeyShellItem,
    type ApiKeyShellState,
  } from '../../src/lib/features/ApiKeyShell.svelte';

  let {
    state,
    filterOptions = [],
    selectedFilter = '',
    detailsById = {},
    createActionLabel = 'Create API Key',
    removeActionLabel = 'Remove',
    headingLevel = 2,
    interactive = true,
    onRetry,
    onCreate,
    onFilterChange,
    copyText,
    onRequestRemove,
  }: {
    state: ApiKeyShellState;
    filterOptions?: string[];
    selectedFilter?: string;
    detailsById?: Record<string, string>;
    createActionLabel?: string;
    removeActionLabel?: string;
    headingLevel?: 1 | 2;
    interactive?: boolean;
    onRetry?: () => void;
    onCreate?: () => void;
    onFilterChange?: (filter: string) => void;
    copyText?: (value: string) => Promise<void>;
    onRequestRemove?: (request: { opaqueId: string; trigger: HTMLButtonElement }) => void;
  } = $props();
</script>

<ApiKeyShell
  {state}
  {filterOptions}
  {selectedFilter}
  {createActionLabel}
  {removeActionLabel}
  {headingLevel}
  {interactive}
  {onRetry}
  {onCreate}
  {onFilterChange}
  {copyText}
  {onRequestRemove}
>
  {#snippet details(key: ApiKeyShellItem)}
    <p aria-label={`Details for ${key.displayName}`}>{detailsById[key.opaqueId] ?? ''}</p>
  {/snippet}
</ApiKeyShell>
