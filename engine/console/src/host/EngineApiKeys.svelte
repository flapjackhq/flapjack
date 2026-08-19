<script lang="ts">
  import { onMount, tick } from 'svelte';
  import ApiKeyShell, {
    type ApiKeyShellItem,
    type ApiKeyShellState,
  } from '../lib/features/ApiKeyShell.svelte';
  import type {
    ConsoleTransport,
    CreateEngineApiKeyRequest,
    EngineApiKey,
  } from '../lib/transport/console_transport';

  type ViewState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready' };

  const ACL_OPTIONS = [
    'search',
    'browse',
    'addObject',
    'deleteObject',
    'deleteIndex',
    'settings',
    'listIndexes',
    'analytics',
  ];

  let {
    transport,
    copyText = async (value: string) => navigator.clipboard.writeText(value),
  }: {
    transport: ConsoleTransport;
    copyText?: (value: string) => Promise<void>;
  } = $props();

  let viewState = $state<ViewState>({ kind: 'loading' });
  let engineKeys = $state<EngineApiKey[]>([]);
  let filterOptions = $state<string[]>([]);
  let selectedFilter = $state('');
  let createOpen = $state(false);
  let createError = $state('');
  let description = $state('');
  let selectedAcl = $state<string[]>(['search']);
  let selectedIndexes = $state<string[]>([]);
  let restrictSources = $state('');
  let maxHitsPerQuery = $state('');
  let maxQueriesPerIPPerHour = $state('');
  let createPending = $state(false);
  let createDialog = $state<HTMLDialogElement | null>(null);
  let createDescriptionInput = $state<HTMLInputElement | null>(null);
  let createTrigger = $state<HTMLButtonElement | null>(null);
  let removalTarget = $state<EngineApiKey | null>(null);
  let removalTrigger = $state<HTMLButtonElement | null>(null);
  let removePending = $state(false);
  let removeError = $state('');
  let removalDialog = $state<HTMLDialogElement | null>(null);
  let removeCancelButton = $state<HTMLButtonElement | null>(null);
  let screenFocusTarget = $state<HTMLElement | null>(null);

  const shellKeys = $derived<ApiKeyShellItem[]>(
    engineKeys.map((key) => ({
      opaqueId: key.value,
      displayName: key.description || 'Untitled key',
      indexNames: key.indexes,
      copyText: key.value,
    }))
  );
  const shellState = $derived<ApiKeyShellState>(
    viewState.kind === 'ready' ? { kind: 'ready', keys: shellKeys } : viewState
  );

  function apiKeysCapability() {
    if (!transport.apiKeys || transport.apiKeys.kind !== 'engine') {
      throw new Error('Engine API keys are unavailable');
    }
    return transport.apiKeys;
  }

  function updateReadyState(keys: EngineApiKey[]): void {
    engineKeys = keys;
    viewState = { kind: 'ready' };
  }

  async function loadKeys(): Promise<void> {
    try {
      updateReadyState(await apiKeysCapability().list());
    } catch {
      engineKeys = [];
      viewState = { kind: 'error', message: 'Could not load API keys.' };
    }
  }

  async function load(): Promise<void> {
    viewState = { kind: 'loading' };
    try {
      const keysPromise = apiKeysCapability().list();
      const indexPromise = transport.listIndexes().catch(() => []);
      const [keys, indexes] = await Promise.all([keysPromise, indexPromise]);
      filterOptions = Array.from(
        new Set([...indexes.map((index) => index.name), ...keys.flatMap((key) => key.indexes)])
      ).sort((left, right) => left.localeCompare(right));
      updateReadyState(keys);
    } catch {
      engineKeys = [];
      viewState = { kind: 'error', message: 'Could not load API keys.' };
    }
  }

  function keyForShellItem(item: ApiKeyShellItem): EngineApiKey | undefined {
    return engineKeys.find((key) => key.value === item.opaqueId);
  }

  function toggleValue(values: string[], value: string): string[] {
    return values.includes(value)
      ? values.filter((current) => current !== value)
      : [...values, value];
  }

  function resetCreateForm(): void {
    createError = '';
    description = '';
    selectedAcl = ['search'];
    selectedIndexes = [];
    restrictSources = '';
    maxHitsPerQuery = '';
    maxQueriesPerIPPerHour = '';
  }

  async function openCreate(): Promise<void> {
    createTrigger =
      document.activeElement instanceof HTMLButtonElement ? document.activeElement : null;
    resetCreateForm();
    createOpen = true;
    await tick();
    createDialog?.showModal();
    createDescriptionInput?.focus();
  }

  async function closeCreate(): Promise<void> {
    if (createPending) return;
    const trigger = createTrigger;
    createDialog?.close();
    createOpen = false;
    createTrigger = null;
    resetCreateForm();
    await tick();
    trigger?.focus();
  }

  function stringList(value: string): string[] {
    return value
      .split(/[\n,]/)
      .map((entry) => entry.trim())
      .filter((entry) => entry.length > 0);
  }

  function optionalPositiveInteger(value: string | number, label: string): number | undefined {
    const normalized = String(value).trim();
    if (!normalized) return undefined;
    const parsed = Number(normalized);
    if (!Number.isInteger(parsed) || parsed < 1) {
      throw new Error(`${label} must be a positive integer.`);
    }
    return parsed;
  }

  async function createKey(): Promise<void> {
    if (selectedAcl.length === 0) {
      createError = 'Select at least one permission.';
      return;
    }

    createError = '';
    createPending = true;
    try {
      const normalizedRestrictSources = stringList(restrictSources);
      const normalizedMaxHits = optionalPositiveInteger(maxHitsPerQuery, 'Max hits per query');
      const normalizedMaxQueries = optionalPositiveInteger(
        maxQueriesPerIPPerHour,
        'Max queries per IP per hour'
      );
      const request: CreateEngineApiKeyRequest = {
        acl: selectedAcl,
        ...(description.trim() ? { description: description.trim() } : {}),
        ...(selectedIndexes.length > 0 ? { indexes: selectedIndexes } : {}),
        ...(normalizedRestrictSources.length > 0
          ? { restrictSources: normalizedRestrictSources }
          : {}),
        ...(normalizedMaxHits === undefined ? {} : { maxHitsPerQuery: normalizedMaxHits }),
        ...(normalizedMaxQueries === undefined
          ? {}
          : { maxQueriesPerIPPerHour: normalizedMaxQueries }),
      };
      await apiKeysCapability().create(request);
      const trigger = createTrigger;
      createDialog?.close();
      createOpen = false;
      createTrigger = null;
      resetCreateForm();
      await loadKeys();
      await tick();
      trigger?.focus();
    } catch (error) {
      createError = error instanceof Error ? error.message : 'Could not create API key.';
    } finally {
      createPending = false;
    }
  }

  async function requestRemoval(request: {
    opaqueId: string;
    trigger: HTMLButtonElement;
  }): Promise<void> {
    removalTarget = engineKeys.find((key) => key.value === request.opaqueId) ?? null;
    if (!removalTarget) return;
    removeError = '';
    removalTrigger = request.trigger;
    await tick();
    removalDialog?.showModal();
    removeCancelButton?.focus();
  }

  async function closeRemoval(): Promise<void> {
    if (removePending) return;
    const trigger = removalTrigger;
    removalDialog?.close();
    removalTarget = null;
    removalTrigger = null;
    removeError = '';
    await tick();
    trigger?.focus();
  }

  async function confirmRemoval(): Promise<void> {
    const target = removalTarget;
    if (!target || removePending) return;
    removeError = '';
    removePending = true;
    try {
      await apiKeysCapability().remove(target.value);
      removalDialog?.close();
      removalTarget = null;
      removalTrigger = null;
      await loadKeys();
      await tick();
      screenFocusTarget?.focus();
    } catch {
      removeError = 'Could not delete API key.';
    } finally {
      removePending = false;
    }
  }

  function formatValidity(seconds: number): string {
    if (seconds === 0) return 'No expiry';
    if (seconds === 3_600) return '1 hour';
    return `${seconds.toLocaleString()} seconds`;
  }

  function formatCreatedAt(epochMillis: number): string {
    return new Date(epochMillis).toISOString().slice(0, 10);
  }

  onMount(() => {
    void load();
  });
</script>

<section bind:this={screenFocusTarget} aria-label="API Keys screen" tabindex="-1">
  <ApiKeyShell
    state={shellState}
    {filterOptions}
    {selectedFilter}
    onFilterChange={(filter) => (selectedFilter = filter)}
    onRetry={load}
    onCreate={openCreate}
    {copyText}
    removeActionLabel="Delete"
    onRequestRemove={(request) => void requestRemoval(request)}
  >
    {#snippet details(item: ApiKeyShellItem)}
      {@const key = keyForShellItem(item)}
      {#if key}
        <dl class="key_details">
          <div><dt>Key value</dt><dd><code>{key.value}</code></dd></div>
          <div><dt>Permissions</dt><dd>{key.acl.join(', ')}</dd></div>
          <div><dt>Index scope</dt><dd>{key.indexes.length ? key.indexes.join(', ') : 'All indexes'}</dd></div>
          {#if key.restrictSources?.length}
            <div><dt>Restrict sources</dt><dd>{key.restrictSources.join(', ')}</dd></div>
          {/if}
          <div><dt>Validity</dt><dd>{formatValidity(key.validity)}</dd></div>
          <div>
            <dt>Created</dt>
            <dd><time datetime={new Date(key.createdAt).toISOString()}>{formatCreatedAt(key.createdAt)}</time></dd>
          </div>
          {#if key.maxHitsPerQuery > 0}
            <div><dt>Query limit</dt><dd>{key.maxHitsPerQuery.toLocaleString()} hits/query</dd></div>
          {/if}
          {#if key.maxQueriesPerIPPerHour > 0}
            <div><dt>Rate limit</dt><dd>{key.maxQueriesPerIPPerHour.toLocaleString()} queries/IP/hour</dd></div>
          {/if}
          {#if key.queryParameters}
            <div><dt>Query parameters</dt><dd>{key.queryParameters}</dd></div>
          {/if}
          {#if key.referers.length}
            <div><dt>Referers</dt><dd>{key.referers.join(', ')}</dd></div>
          {/if}
        </dl>
      {/if}
    {/snippet}
  </ApiKeyShell>
</section>

{#if createOpen}
  <dialog
    bind:this={createDialog}
    aria-labelledby="create_engine_key_heading"
    class="dialog_panel"
    oncancel={(event) => {
      event.preventDefault();
      void closeCreate();
    }}
  >
      <h2 id="create_engine_key_heading">Create engine API key</h2>
      <form
        onsubmit={(event) => {
          event.preventDefault();
          void createKey();
        }}
      >
        <label>Description <input bind:this={createDescriptionInput} bind:value={description} /></label>
        <fieldset>
          <legend>Permissions</legend>
          {#each ACL_OPTIONS as acl (acl)}
            <label>
              <input
                type="checkbox"
                checked={selectedAcl.includes(acl)}
                onchange={() => (selectedAcl = toggleValue(selectedAcl, acl))}
              />
              {acl}
            </label>
          {/each}
        </fieldset>
        <fieldset>
          <legend>Index scope</legend>
          {#each filterOptions as indexName (indexName)}
            <label>
              <input
                type="checkbox"
                aria-label={`Index ${indexName}`}
                checked={selectedIndexes.includes(indexName)}
                onchange={() => (selectedIndexes = toggleValue(selectedIndexes, indexName))}
              />
              {indexName}
            </label>
          {/each}
        </fieldset>
        <label>Restrict sources <textarea bind:value={restrictSources}></textarea></label>
        <label>Max hits per query <input type="number" min="1" bind:value={maxHitsPerQuery} /></label>
        <label>
          Max queries per IP per hour
          <input type="number" min="1" bind:value={maxQueriesPerIPPerHour} />
        </label>
        {#if createError}<p role="alert">{createError}</p>{/if}
        <div class="dialog_actions">
          <button type="button" disabled={createPending} onclick={() => void closeCreate()}>Cancel</button>
          <button type="submit" disabled={createPending}>
            {createPending ? 'Creating…' : 'Create key'}
          </button>
        </div>
      </form>
  </dialog>
{/if}

{#if removalTarget}
  <dialog
    bind:this={removalDialog}
    aria-labelledby="delete_engine_key_heading"
    class="dialog_panel"
    oncancel={(event) => {
      event.preventDefault();
      void closeRemoval();
    }}
  >
      <h2 id="delete_engine_key_heading">Delete engine API key</h2>
      <p>Delete {removalTarget.description || 'Untitled key'}? This action cannot be undone.</p>
      {#if removeError}<p role="alert">{removeError}</p>{/if}
      <div class="dialog_actions">
        <button
          bind:this={removeCancelButton}
          type="button"
          disabled={removePending}
          onclick={() => void closeRemoval()}
        >Cancel</button>
        <button type="button" disabled={removePending} onclick={() => void confirmRemoval()}>
          {removePending ? 'Deleting…' : 'Delete key'}
        </button>
      </div>
  </dialog>
{/if}

<style>
  .key_details {
    display: grid;
    gap: var(--console-space-sm);
    margin-block: var(--console-space-md) 0;
  }

  .key_details div {
    display: grid;
    grid-template-columns: minmax(8rem, 12rem) minmax(0, 1fr);
    gap: var(--console-space-sm);
  }

  dt {
    color: var(--console-text-muted);
  }

  dd {
    min-width: 0;
    margin: 0;
    overflow-wrap: anywhere;
  }

  .dialog_panel {
    width: min(calc(100% - 2 * var(--console-space-md)), 40rem);
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
  fieldset,
  label {
    display: grid;
    gap: var(--console-space-sm);
  }

  form {
    gap: var(--console-space-md);
  }

  fieldset label {
    display: flex;
    align-items: center;
  }

  textarea {
    min-height: 5rem;
    resize: vertical;
  }

  .dialog_actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--console-space-md);
  }

  @media (max-width: 30rem) {
    .key_details div {
      grid-template-columns: 1fr;
    }
  }
</style>
