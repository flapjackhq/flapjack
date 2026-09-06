<script lang="ts">
  import { onMount } from 'svelte';
  import type { ConsoleTransport, IndexSummary } from '../transport/console_transport';
  import { Button } from '../ui';
  import IndexSearch from './IndexSearch.svelte';
  import type { SearchAnalyticsCopy } from './IndexSearch.svelte';

  let {
    transport,
    searchAnalyticsCopy,
  }: { transport: ConsoleTransport; searchAnalyticsCopy?: SearchAnalyticsCopy } = $props();
  let indexes = $state<IndexSummary[] | null>(null);
  let listLoading = $state(true);
  let listError = $state(false);
  let selectedIndex = $state<string | null>(null);

  async function loadIndexes(): Promise<void> {
    listLoading = true;
    listError = false;
    try {
      indexes = await transport.listIndexes();
    } catch {
      indexes = null;
      listError = true;
    } finally {
      listLoading = false;
    }
  }

  function selectIndex(name: string): void {
    selectedIndex = name;
  }

  function showIndexes(): void {
    selectedIndex = null;
  }

  onMount(() => {
    void loadIndexes();
  });
</script>

{#if selectedIndex === null}
  <section aria-labelledby="indexes_heading" class="feature_panel">
    <h2 id="indexes_heading">Indexes</h2>
    {#if listLoading}
      <p role="status" aria-live="polite">Loading indexes...</p>
    {:else if listError}
      <div class="state_message">
        <p role="alert">Could not load indexes.</p>
        <Button label="Retry loading indexes" onpress={loadIndexes} />
      </div>
    {:else if indexes?.length === 0}
      <p>No indexes yet.</p>
    {:else if indexes}
      <table aria-label="Indexes">
        <thead>
          <tr><th>Name</th><th>Entries</th><th>Data size</th></tr>
        </thead>
        <tbody>
          {#each indexes as index (index.name)}
            <tr>
              <th scope="row">
                <button aria-label={`Search ${index.name}`} onclick={() => selectIndex(index.name)}>
                  {index.name}
                </button>
              </th>
              <td>{index.entries ?? 'Unavailable'}</td>
              <td>{index.dataSize === null ? 'Unavailable' : `${index.dataSize} bytes`}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>
{:else}
  {#key selectedIndex}
    <IndexSearch
      transport={transport}
      indexName={selectedIndex}
      onBackToIndexes={showIndexes}
      {searchAnalyticsCopy}
    />
  {/key}
{/if}

<style>
  .feature_panel {
    padding: var(--console-space-lg);
  }

  table {
    width: 100%;
    border-collapse: collapse;
  }

  th,
  td {
    padding: var(--console-space-sm);
    border-block-end: var(--console-border-width) solid var(--console-border);
    text-align: start;
  }

  .state_message {
    display: flex;
    align-items: center;
    gap: var(--console-space-md);
  }

</style>
