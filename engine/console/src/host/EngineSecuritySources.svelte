<script lang="ts">
  import { onMount, tick } from 'svelte';
  import type {
    ConsoleTransport,
    EngineSecuritySourcesCapability,
    SecuritySource,
  } from '../lib/transport/console_transport';

  type ViewState = 'loading' | 'error' | 'ready';

  let { transport }: { transport: ConsoleTransport } = $props();

  let viewState = $state<ViewState>('loading');
  let sources = $state<SecuritySource[]>([]);
  let announcement = $state('');
  let addOpen = $state(false);
  let addPending = $state(false);
  let addError = $state('');
  let sourceValue = $state('');
  let descriptionValue = $state('');
  let addDialog = $state<HTMLDialogElement | null>(null);
  let addSourceInput = $state<HTMLInputElement | null>(null);
  let addTrigger = $state<HTMLButtonElement | null>(null);
  let removingSource = $state('');
  let removalErrorSource = $state('');
  let removalTrigger = $state<HTMLButtonElement | null>(null);
  let screenFocusTarget = $state<HTMLElement | null>(null);

  function capability(): EngineSecuritySourcesCapability {
    if (!transport.securitySources || transport.securitySources.kind !== 'engine') {
      throw new Error('Engine security sources are unavailable');
    }
    return transport.securitySources;
  }

  async function load(): Promise<void> {
    viewState = 'loading';
    try {
      sources = await capability().list();
      viewState = 'ready';
    } catch {
      sources = [];
      viewState = 'error';
    }
  }

  function resetAddForm(): void {
    sourceValue = '';
    descriptionValue = '';
    addError = '';
  }

  async function openAdd(): Promise<void> {
    resetAddForm();
    addOpen = true;
    await tick();
    addDialog?.showModal();
    addSourceInput?.focus();
  }

  async function closeAdd(): Promise<void> {
    if (addPending) return;
    addDialog?.close();
    addOpen = false;
    resetAddForm();
    await tick();
    addTrigger?.focus();
  }

  async function appendSource(): Promise<void> {
    const source = sourceValue.trim();
    if (!source) {
      addError = 'Source is required.';
      return;
    }

    addError = '';
    addPending = true;
    try {
      await capability().append({ source, description: descriptionValue.trim() });
      sources = await capability().list();
      addDialog?.close();
      addOpen = false;
      resetAddForm();
      announcement = 'Security source added.';
      await tick();
      addTrigger?.focus();
    } catch {
      addError = 'Could not add security source.';
    } finally {
      addPending = false;
    }
  }

  async function removeSource(source: string, trigger: HTMLButtonElement): Promise<void> {
    if (removingSource) return;
    announcement = '';
    removalErrorSource = '';
    removingSource = source;
    removalTrigger = trigger;
    let removed = false;
    try {
      await capability().remove(source);
      sources = await capability().list();
      announcement = 'Security source deleted.';
      removed = true;
    } catch {
      removalErrorSource = source;
    } finally {
      removingSource = '';
      await tick();
      if (removed) {
        removalTrigger = null;
        screenFocusTarget?.focus();
      } else {
        removalTrigger?.focus();
      }
    }
  }

  onMount(() => {
    void load();
  });
</script>

<section bind:this={screenFocusTarget} aria-label="Security Sources screen" tabindex="-1">
  <header class="screen_header">
    <div>
      <h2>Security Sources</h2>
      <p>
        This engine-wide allowlist gates protected API requests. An empty list allows every source;
        adding the first entry restricts access immediately and can lock out this browser.
      </p>
    </div>
    <button bind:this={addTrigger} type="button" onclick={() => void openAdd()}>Add Source</button>
  </header>

  {#if announcement}<p role="status">{announcement}</p>{/if}

  <section aria-labelledby="security_sources_allowlist_heading" class="allowlist">
    <div class="allowlist_header">
      <h3 id="security_sources_allowlist_heading">Source Allowlist</h3>
      <span>{sources.length} {sources.length === 1 ? 'entry' : 'entries'}</span>
    </div>

    {#if viewState === 'loading'}
      <p role="status">Loading security sources…</p>
    {:else if viewState === 'error'}
      <div class="state_message">
        <p role="alert">Could not load security sources.</p>
        <button type="button" onclick={() => void load()}>Retry</button>
      </div>
    {:else if sources.length === 0}
      <p>No security sources configured yet.</p>
    {:else}
      <div class="source_list">
        {#each sources as entry (entry.source)}
          <article aria-label={entry.source} class="source_row">
            <div>
              <h4><code>{entry.source}</code></h4>
              <p>{entry.description || 'No description'}</p>
              {#if removalErrorSource === entry.source}
                <p role="alert">Could not delete security source.</p>
              {/if}
            </div>
            <button
              type="button"
              aria-label={`Delete security source ${entry.source}`}
              disabled={Boolean(removingSource)}
              onclick={(event) => void removeSource(entry.source, event.currentTarget)}
            >
              {removingSource === entry.source ? 'Deleting…' : 'Delete'}
            </button>
          </article>
        {/each}
      </div>
    {/if}
  </section>
</section>

{#if addOpen}
  <dialog
    bind:this={addDialog}
    aria-labelledby="add_security_source_heading"
    class="dialog_panel"
    oncancel={(event) => {
      event.preventDefault();
      void closeAdd();
    }}
  >
    <h2 id="add_security_source_heading">Add security source</h2>
    <p>
      The first entry immediately restricts this engine to the configured sources. Confirm that
      this browser's source remains allowed before continuing.
    </p>
    <form
      onsubmit={(event) => {
        event.preventDefault();
        void appendSource();
      }}
    >
      <label>
        Source
        <input
          bind:this={addSourceInput}
          bind:value={sourceValue}
          disabled={addPending}
          placeholder="192.168.1.0/24"
          oninput={() => {
            if (sourceValue.trim()) addError = '';
          }}
        />
      </label>
      <label>
        Description
        <textarea
          bind:value={descriptionValue}
          disabled={addPending}
          rows="3"
          placeholder="Office network"
        ></textarea>
      </label>
      {#if addError}<p role="alert">{addError}</p>{/if}
      <div class="dialog_actions">
        <button type="button" disabled={addPending} onclick={() => void closeAdd()}>Cancel</button>
        <button type="submit" disabled={addPending}>{addPending ? 'Adding…' : 'Add source'}</button>
      </div>
    </form>
  </dialog>
{/if}

<style>
  section[aria-label='Security Sources screen'] {
    min-width: 0;
    padding: var(--console-space-lg);
  }

  h2,
  h3,
  h4,
  p {
    margin-block-start: 0;
  }

  .screen_header,
  .allowlist_header,
  .source_row,
  .dialog_actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--console-space-md);
  }

  .screen_header {
    align-items: flex-start;
  }

  .screen_header p {
    max-width: 50rem;
    color: var(--console-text-muted);
  }

  .allowlist {
    margin-block-start: var(--console-space-lg);
    padding: var(--console-space-lg);
    border: var(--console-border-width) solid var(--console-border);
    border-radius: var(--console-radius);
    background: var(--console-surface-muted);
  }

  .source_list {
    display: grid;
    gap: var(--console-space-md);
  }

  .source_row {
    min-width: 0;
    align-items: flex-start;
    padding: var(--console-space-md);
    border: var(--console-border-width) solid var(--console-border);
    border-radius: var(--console-radius);
  }

  .source_row div,
  .source_row h4,
  .source_row p,
  code {
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .state_message {
    display: grid;
    justify-items: start;
    gap: var(--console-space-sm);
  }

  .dialog_panel {
    width: min(calc(100% - 2 * var(--console-space-md)), 36rem);
    max-height: calc(100vh - 2 * var(--console-space-md));
    margin: auto;
    overflow: auto;
    padding: var(--console-space-lg);
    border: var(--console-border-width) solid var(--console-border);
    border-radius: var(--console-radius);
    background: var(--console-surface-muted);
    box-shadow: var(--console-shadow);
  }

  .dialog_panel::backdrop {
    background: rgb(35 25 66 / 45%);
  }

  form,
  label {
    display: grid;
    gap: var(--console-space-sm);
  }

  form {
    gap: var(--console-space-md);
  }

  textarea {
    min-height: 5rem;
    resize: vertical;
  }

  .dialog_actions {
    justify-content: flex-end;
  }

  @media (max-width: 40rem) {
    .screen_header,
    .source_row {
      align-items: stretch;
      flex-direction: column;
    }

    .screen_header button,
    .source_row button {
      align-self: flex-start;
    }
  }
</style>
