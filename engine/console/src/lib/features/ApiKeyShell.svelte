<script module lang="ts">
  export type ApiKeyShellItem = {
    opaqueId: string;
    displayName: string;
    indexNames: string[];
    copyText: string;
  };

  export type ApiKeyShellState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; keys: ApiKeyShellItem[] };
</script>

<script lang="ts">
  import { onDestroy, type Snippet } from 'svelte';

  let {
    state: viewState,
    filterOptions = [],
    selectedFilter = '',
    createActionLabel = 'Create API Key',
    removeActionLabel = 'Remove',
    headingLevel = 2,
    interactive = true,
    onRetry,
    onCreate,
    onFilterChange,
    copyText,
    onRequestRemove,
    details,
  }: {
    state: ApiKeyShellState;
    filterOptions?: string[];
    selectedFilter?: string;
    createActionLabel?: string;
    removeActionLabel?: string;
    headingLevel?: 1 | 2;
    interactive?: boolean;
    onRetry?: () => void;
    onCreate?: () => void;
    onFilterChange?: (filter: string) => void;
    copyText?: (value: string) => Promise<void>;
    onRequestRemove?: (request: { opaqueId: string; trigger: HTMLButtonElement }) => void;
    details?: Snippet<[ApiKeyShellItem]>;
  } = $props();

  let copyFeedback = $state<{ opaqueId: string; kind: 'success' | 'error' } | null>(null);
  let copyFeedbackTimer: ReturnType<typeof setTimeout> | undefined;

  const readyKeys = $derived(viewState.kind === 'ready' ? viewState.keys : []);
  const visibleKeys = $derived(
    selectedFilter
      ? readyKeys.filter(
          (key) => key.indexNames.length === 0 || key.indexNames.includes(selectedFilter)
        )
      : readyKeys
  );

  function changeFilter(event: Event): void {
    if (!interactive) return;
    onFilterChange?.((event.currentTarget as HTMLSelectElement).value);
  }

  function createKey(): void {
    if (!interactive) return;
    onCreate?.();
  }

  async function copyKey(key: ApiKeyShellItem): Promise<void> {
    if (!interactive || !copyText) return;
    if (copyFeedbackTimer) clearTimeout(copyFeedbackTimer);
    try {
      await copyText(key.copyText);
      copyFeedback = { opaqueId: key.opaqueId, kind: 'success' };
      copyFeedbackTimer = setTimeout(() => {
        copyFeedback = null;
        copyFeedbackTimer = undefined;
      }, 2_000);
    } catch {
      copyFeedback = { opaqueId: key.opaqueId, kind: 'error' };
    }
  }

  function requestRemoval(key: ApiKeyShellItem, event: MouseEvent): void {
    if (!interactive) return;
    onRequestRemove?.({
      opaqueId: key.opaqueId,
      trigger: event.currentTarget as HTMLButtonElement,
    });
  }

  onDestroy(() => {
    if (copyFeedbackTimer) clearTimeout(copyFeedbackTimer);
  });
</script>

<section aria-labelledby="api_keys_heading" class="api_key_shell">
  <header>
    <div>
      {#if headingLevel === 1}
        <h1 id="api_keys_heading">API Keys</h1>
      {:else}
        <h2 id="api_keys_heading">API Keys</h2>
      {/if}
      <p>Review each key's access before sharing it.</p>
    </div>
    {#if onCreate}
      <button type="button" disabled={!interactive} onclick={createKey}>{createActionLabel}</button>
    {/if}
  </header>

  {#if viewState.kind === 'loading'}
    <p role="status" aria-live="polite">Loading API keys…</p>
  {:else if viewState.kind === 'error'}
    <div class="state_message">
      <p role="alert">{viewState.message}</p>
      {#if onRetry}<button type="button" disabled={!interactive} onclick={onRetry}>Retry</button>{/if}
    </div>
  {:else}
    {#if filterOptions.length > 0 && readyKeys.length > 0}
      <label class="filter_control">
        <span>Filter by index</span>
        <select value={selectedFilter} disabled={!interactive} onchange={changeFilter}>
          <option value="">All indexes</option>
          {#each filterOptions as option (option)}
            <option value={option}>{option}</option>
          {/each}
        </select>
      </label>
    {/if}

    {#if readyKeys.length === 0}
      <p>No API keys yet.</p>
    {:else if visibleKeys.length === 0}
      <p>No API keys match this filter.</p>
    {:else}
      <div class="key_list">
        {#each visibleKeys as key (key.opaqueId)}
          <article aria-label={key.displayName}>
            {#if headingLevel === 1}
              <h2>{key.displayName}</h2>
            {:else}
              <h3>{key.displayName}</h3>
            {/if}
            {#if details}{@render details(key)}{/if}
            <div class="key_actions">
              {#if copyText}
                <button
                  type="button"
                  disabled={!interactive}
                  aria-label={`Copy ${key.displayName}`}
                  onclick={() => void copyKey(key)}
                >Copy</button>
              {/if}
              {#if onRequestRemove}
                <button
                  type="button"
                  disabled={!interactive}
                  aria-label={`${removeActionLabel} ${key.displayName}`}
                  onclick={(event) => requestRemoval(key, event)}
                >{removeActionLabel}</button>
              {/if}
            </div>
            {#if copyFeedback?.opaqueId === key.opaqueId}
              {#if copyFeedback.kind === 'success'}
                <p role="status">Copied</p>
              {:else}
                <p role="alert">Could not copy</p>
              {/if}
            {/if}
          </article>
        {/each}
      </div>
    {/if}
  {/if}
</section>

<style>
  .api_key_shell {
    padding: var(--console-space-lg);
  }

  header,
  .state_message,
  .key_actions {
    display: flex;
    align-items: center;
    gap: var(--console-space-md);
  }

  header {
    justify-content: space-between;
    flex-wrap: wrap;
  }

  h1,
  h2,
  h3,
  header p {
    margin-block: 0;
  }

  header p {
    margin-block-start: var(--console-space-sm);
    color: var(--console-text-muted);
  }

  .filter_control {
    display: grid;
    gap: var(--console-space-sm);
    max-width: 24rem;
    margin-block: var(--console-space-lg);
  }

  select {
    border: var(--console-border-width) solid var(--console-border);
    border-radius: var(--console-radius);
    padding: var(--console-space-sm) var(--console-space-md);
    color: inherit;
    background: var(--console-surface-muted);
    font: inherit;
  }

  select:focus-visible {
    outline: calc(var(--console-border-width) * 2) solid var(--console-focus);
    outline-offset: calc(var(--console-border-width) * 2);
  }

  .key_list {
    display: grid;
    gap: var(--console-space-md);
    margin-block-start: var(--console-space-lg);
  }

  article {
    min-width: 0;
    padding: var(--console-space-md);
    border: var(--console-border-width) solid var(--console-border);
    border-radius: var(--console-radius);
    background: var(--console-surface-muted);
  }

  .key_actions {
    flex-wrap: wrap;
    margin-block-start: var(--console-space-md);
  }
</style>
