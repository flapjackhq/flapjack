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
  import Button from '../ui/Button.svelte';

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
      <Button
        label={createActionLabel}
        variant="primary"
        disabled={!interactive}
        onpress={createKey}
      />
    {/if}
  </header>

  {#if viewState.kind === 'loading'}
    <p class="state_message" role="status" aria-live="polite">Loading API keys…</p>
  {:else if viewState.kind === 'error'}
    <div class="state_message state_message_error">
      <p role="alert">{viewState.message}</p>
      {#if onRetry}
        <Button label="Retry" variant="secondary" disabled={!interactive} onpress={onRetry} />
      {/if}
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
      <div class="empty_state">
        <p class="empty_title">No API keys yet.</p>
        <p>Create a scoped key when you are ready to connect an application.</p>
      </div>
    {:else if visibleKeys.length === 0}
      <div class="empty_state">
        <p class="empty_title">No API keys match this filter.</p>
        <p>Choose another index or show all indexes.</p>
      </div>
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
                <Button
                  label="Copy"
                  variant="secondary"
                  disabled={!interactive}
                  ariaLabel={`Copy ${key.displayName}`}
                  onpress={() => void copyKey(key)}
                />
              {/if}
              {#if onRequestRemove}
                <Button
                  label={removeActionLabel}
                  variant="danger"
                  disabled={!interactive}
                  ariaLabel={`${removeActionLabel} ${key.displayName}`}
                  onpress={(event) => requestRemoval(key, event)}
                />
              {/if}
            </div>
            {#if copyFeedback?.opaqueId === key.opaqueId}
              {#if copyFeedback.kind === 'success'}
                <p class="action_feedback action_feedback_success" role="status">Copied</p>
              {:else}
                <p class="action_feedback action_feedback_error" role="alert">Could not copy</p>
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
    width: 100%;
    min-width: 0;
    padding: var(--console-space-lg);
    border: var(--console-border-width) solid var(--console-border);
    border-radius: var(--console-radius);
    color: var(--console-text);
    background: var(--console-surface);
    box-shadow: var(--console-shadow);
    font-family: var(--console-font);
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
  header p,
  .state_message p,
  .empty_state p,
  .action_feedback {
    margin-block: 0;
  }

  h1,
  h2,
  h3 {
    color: var(--console-text);
  }

  h1,
  header h2 {
    font-size: var(--console-heading-size);
  }

  article h2,
  article h3 {
    font-size: var(--console-subheading-size);
  }

  header p {
    margin-block-start: var(--console-space-sm);
    color: var(--console-text-muted);
  }

  .state_message,
  .empty_state {
    min-height: var(--console-content-min-height);
    margin-block-start: var(--console-space-lg);
    padding: var(--console-space-lg);
    border: var(--console-border-width) solid var(--console-border);
    border-radius: var(--console-radius);
    background: var(--console-surface-muted);
  }

  .state_message {
    justify-content: space-between;
    flex-wrap: wrap;
  }

  .state_message_error {
    border-color: var(--console-danger);
    background: var(--console-danger-surface);
  }

  .empty_state {
    display: grid;
    align-content: center;
    gap: var(--console-space-sm);
    border-style: dashed;
    text-align: center;
  }

  .empty_title {
    color: var(--console-text);
    font-size: var(--console-subheading-size);
    font-weight: var(--console-control-font-weight);
  }

  .empty_state p:not(.empty_title) {
    color: var(--console-text-muted);
  }

  .filter_control {
    display: grid;
    gap: var(--console-space-sm);
    max-width: 24rem;
    margin-block: var(--console-space-lg);
  }

  .filter_control span {
    color: var(--console-text);
    font-weight: var(--console-control-font-weight);
  }

  select {
    width: 100%;
    min-height: var(--console-control-min-height);
    border: var(--console-border-width) solid var(--console-border);
    border-radius: var(--console-radius);
    padding: var(--console-space-sm) var(--console-space-md);
    color: var(--console-text);
    background: var(--console-surface-muted);
    font: inherit;
  }

  select:focus-visible {
    outline: calc(var(--console-border-width) * 3) solid var(--console-focus);
    outline-offset: calc(var(--console-border-width) * 2);
  }

  select:disabled {
    opacity: 0.55;
    cursor: not-allowed;
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
    box-shadow: var(--console-shadow);
  }

  .key_actions {
    flex-wrap: wrap;
    margin-block-start: var(--console-space-md);
  }

  .action_feedback {
    display: inline-block;
    margin-block-start: var(--console-space-sm);
    padding: var(--console-space-sm) var(--console-space-md);
    border-radius: var(--console-radius);
    font-weight: var(--console-control-font-weight);
  }

  .action_feedback_success {
    color: var(--console-status);
    background: var(--console-status-surface);
  }

  .action_feedback_error {
    color: var(--console-danger);
    background: var(--console-danger-surface);
  }

  @media (max-width: 40rem) {
    .api_key_shell {
      padding: var(--console-space-md);
    }

    header,
    .state_message {
      align-items: stretch;
      flex-direction: column;
    }
  }
</style>
