<script module lang="ts">
  export type SearchAnalyticsCopy = {
    toggleLabel: string;
    helpText?: string;
  };
</script>

<script lang="ts">
  import { onMount } from 'svelte';
  import type {
    ConsoleTransport,
    SearchPage,
    SearchRequest,
    SearchSemantics,
  } from '../transport/console_transport';
  import { Button } from '../ui';

  let {
    transport,
    indexName,
    onBackToIndexes,
    searchAnalyticsCopy,
  }: {
    transport: ConsoleTransport;
    indexName: string;
    onBackToIndexes?: () => void;
    searchAnalyticsCopy?: SearchAnalyticsCopy;
  } = $props();
  let queryInput = $state<HTMLInputElement>();
  let draftQuery = $state('');
  let committedRequest = $state<SearchRequest | null>(null);
  let result = $state<SearchPage | null>(null);
  let searchLoading = $state(false);
  let searchError = $state(false);
  let openDetails = $state<Set<number>>(new Set());
  let trackAnalyticsEnabled = $state(false);
  let analyticsQueryId = $state<string | null>(null);
  let analyticsSearchRequired = $state(false);
  let analyticsStatusMessage = $state('');
  let searchSemantics = $state<SearchSemantics | null>(null);
  let semanticsLoading = $state(false);
  let semanticsError = $state(false);
  let semanticRatio = $state(0.5);
  let selectedQueryEmbedder = $state('');
  let analyticsRevision = 0;
  let eventAttempt = 0;
  let semanticsRevision = 0;
  let mounted = false;
  const analyticsAvailable = $derived(
    transport.searchAnalytics !== undefined && searchAnalyticsCopy !== undefined
  );
  const semanticsAvailable = $derived(
    searchSemantics !== null && searchSemantics.queryEmbedderNames.length > 0
  );

  function nonEmptyString(value: unknown): string | null {
    return typeof value === 'string' && value.trim().length > 0 ? value : null;
  }

  function semanticRatioLabel(ratio: number): string {
    if (ratio === 0) return 'Keyword only';
    if (ratio === 1) return 'Semantic only';
    if (ratio === 0.5) return 'Balanced';
    return `${Math.round(ratio * 100)}% semantic`;
  }

  function resultPosition(index: number): number {
    if (!result) return index + 1;
    return result.page * result.hitsPerPage + index + 1;
  }

  function resultTitle(hit: Record<string, unknown>, position: number): string {
    return (
      nonEmptyString(hit.title) ??
      nonEmptyString(hit.name) ??
      nonEmptyString(hit.objectID) ??
      `Result ${position}`
    );
  }

  function setDetailsOpen(hit: Record<string, unknown>, position: number, open: boolean): void {
    const wasOpen = openDetails.has(position);
    const next = new Set(openDetails);
    if (open) next.add(position);
    else next.delete(position);
    openDetails = next;
    if (open && !wasOpen) {
      recordResultOpen(hit, position);
    }
  }

  function invalidateAnalyticsCorrelation(): number {
    analyticsQueryId = null;
    analyticsRevision += 1;
    eventAttempt += 1;
    analyticsStatusMessage = '';
    return analyticsRevision;
  }

  function effectiveRequest(request: SearchRequest): SearchRequest {
    if (!analyticsAvailable) return request;
    return trackAnalyticsEnabled
      ? { ...request, analytics: true, clickAnalytics: true }
      : { ...request, analytics: false };
  }

  function requestWithSemanticDraft(request: SearchRequest): SearchRequest {
    if (!transport.searchSemantics) return request;
    if (!semanticsAvailable || !searchSemantics) return { ...request, mode: 'keywordSearch' };
    if (semanticRatio <= 0) return { ...request, mode: 'keywordSearch' };
    return {
      ...request,
      mode: 'neuralSearch',
      hybrid: {
        semanticRatio,
        embedder: selectedQueryEmbedder,
      },
    };
  }

  async function loadSearchSemantics(): Promise<void> {
    const capability = transport.searchSemantics;
    if (!capability) {
      searchSemantics = null;
      semanticsLoading = false;
      semanticsError = false;
      return;
    }

    const revision = ++semanticsRevision;
    semanticsLoading = true;
    semanticsError = false;
    try {
      const next = await capability.load(indexName);
      if (!mounted || semanticsRevision !== revision) return;
      searchSemantics = next;
      semanticRatio = 0.5;
      selectedQueryEmbedder = next?.queryEmbedderNames[0] ?? '';
    } catch {
      if (!mounted || semanticsRevision !== revision) return;
      searchSemantics = null;
      semanticsError = true;
    } finally {
      if (mounted && semanticsRevision === revision) semanticsLoading = false;
    }
  }

  async function runSearch(request: SearchRequest): Promise<void> {
    if (searchLoading) return;
    committedRequest = request;
    searchLoading = true;
    searchError = false;
    const requestTracksAnalytics = analyticsAvailable && trackAnalyticsEnabled;
    const requestAnalyticsRevision = invalidateAnalyticsCorrelation();
    analyticsSearchRequired = false;
    try {
      const nextResult = await transport.searchIndex(indexName, effectiveRequest(request));
      result = nextResult;
      openDetails = new Set();
      if (
        requestTracksAnalytics &&
        trackAnalyticsEnabled &&
        analyticsRevision === requestAnalyticsRevision
      ) {
        analyticsQueryId = nextResult.queryId ?? null;
      }
    } catch {
      searchError = true;
    } finally {
      searchLoading = false;
    }
  }

  function submitSearch(): void {
    void runSearch(
      requestWithSemanticDraft({ query: draftQuery, page: 0, hitsPerPage: 20 })
    );
  }

  function moveToPage(page: number): void {
    if (!committedRequest) return;
    void runSearch({ ...committedRequest, page });
  }

  function moveByPage(delta: number): void {
    if (result) moveToPage(result.page + delta);
  }

  function retrySearch(): void {
    if (committedRequest) void runSearch(committedRequest);
  }

  function setTrackAnalyticsEnabled(nextEnabled: boolean): void {
    if (!analyticsAvailable || nextEnabled === trackAnalyticsEnabled) return;
    trackAnalyticsEnabled = nextEnabled;
    invalidateAnalyticsCorrelation();
    analyticsSearchRequired = nextEnabled;
    analyticsStatusMessage = nextEnabled
      ? 'Preview activity recording is on. Run a new search to record result opens.'
      : '';
  }

  function recordResultOpen(hit: Record<string, unknown>, position: number): void {
    const capability = transport.searchAnalytics;
    if (!analyticsAvailable || !capability || !trackAnalyticsEnabled) return;
    const attempt = ++eventAttempt;
    const revision = analyticsRevision;
    if (searchLoading) {
      analyticsStatusMessage = 'Not recorded: wait for the current search to finish.';
      return;
    }
    if (analyticsSearchRequired) {
      analyticsStatusMessage = 'Not recorded: run a new search after enabling preview activity.';
      return;
    }
    if (!analyticsQueryId) {
      analyticsStatusMessage = 'Not recorded: the search response did not include a query ID.';
      return;
    }
    const objectId = hit.objectID;
    if (typeof objectId !== 'string' || objectId.trim().length === 0) {
      analyticsStatusMessage = 'Not recorded: the result does not include an object ID.';
      return;
    }

    analyticsStatusMessage = '';
    void capability
      .recordResultOpen({ indexName, objectId, position, queryId: analyticsQueryId })
      .then(() => {
        if (mounted && eventAttempt === attempt && analyticsRevision === revision) {
          analyticsStatusMessage = 'Recorded result open.';
        }
      })
      .catch(() => {
        if (mounted && eventAttempt === attempt && analyticsRevision === revision) {
          analyticsStatusMessage = 'Result open was not recorded.';
        }
      });
  }

  onMount(() => {
    mounted = true;
    queryInput?.focus();
    void loadSearchSemantics();
    return () => {
      mounted = false;
      analyticsRevision += 1;
      eventAttempt += 1;
      semanticsRevision += 1;
    };
  });
</script>

<section aria-labelledby="search_heading" class="feature_panel">
  <div class="feature_heading">
    <h2 id="search_heading">Search {indexName}</h2>
    {#if onBackToIndexes}
      <Button label="Back to indexes" onpress={onBackToIndexes} />
    {/if}
  </div>

  <form
    role="search"
    onsubmit={(event) => {
      event.preventDefault();
      submitSearch();
    }}
  >
    <label for="console_query">Query</label>
    <input id="console_query" type="search" bind:this={queryInput} bind:value={draftQuery} />
    <Button label="Search" type="submit" disabled={searchLoading} />
  </form>

  {#if semanticsLoading}
    <p role="status" aria-live="polite">Loading semantic search options…</p>
  {:else if semanticsError}
    <div class="state_message">
      <p role="alert">Could not load semantic search options.</p>
      <Button label="Retry semantic options" onpress={loadSearchSemantics} />
    </div>
  {:else if searchSemantics}
    {#if searchSemantics.queryEmbedderNames.length > 0}
      <fieldset class="semantic_controls">
        <legend>Search balance</legend>
        <p>
          {searchSemantics.configuredEmbedderCount}
          {searchSemantics.configuredEmbedderCount === 1 ? 'embedder' : 'embedders'} configured;
          {searchSemantics.queryEmbedderNames.length}
          can embed queries.
        </p>
        <label for="console_semantic_ratio">Semantic ratio</label>
        <input
          id="console_semantic_ratio"
          type="range"
          min="0"
          max="1"
          step="0.1"
          value={semanticRatio}
          oninput={(event) => {
            semanticRatio = (event.currentTarget as HTMLInputElement).valueAsNumber;
          }}
        />
        <output for="console_semantic_ratio">{semanticRatioLabel(semanticRatio)}</output>
        <label for="console_query_embedder">Query embedder</label>
        <select id="console_query_embedder" bind:value={selectedQueryEmbedder}>
          {#each searchSemantics.queryEmbedderNames as name}
            <option value={name}>{name}</option>
          {/each}
        </select>
      </fieldset>
    {:else if searchSemantics.configuredEmbedderCount > 0}
      <p>No query-capable embedders are configured.</p>
    {/if}
  {/if}

  {#if analyticsAvailable && searchAnalyticsCopy}
    <div class="analytics_control">
      <label for="console_track_analytics">
        <input
          id="console_track_analytics"
          type="checkbox"
          checked={trackAnalyticsEnabled}
          aria-describedby={searchAnalyticsCopy.helpText
            ? 'console_track_analytics_help'
            : undefined}
          onchange={(event) =>
            setTrackAnalyticsEnabled((event.currentTarget as HTMLInputElement).checked)}
        />
        {searchAnalyticsCopy.toggleLabel}
      </label>
      {#if searchAnalyticsCopy.helpText}
        <p id="console_track_analytics_help">{searchAnalyticsCopy.helpText}</p>
      {/if}
    </div>
  {/if}

  {#if analyticsStatusMessage}
    <p role="status" aria-live="polite">{analyticsStatusMessage}</p>
  {/if}

  {#if searchLoading}
    <p role="status" aria-live="polite">Searching...</p>
  {/if}
  {#if searchError}
    <div class="state_message">
      <p role="alert">Could not search this index.</p>
      <Button label="Retry search" onpress={retrySearch} disabled={searchLoading} />
    </div>
  {/if}

  {#if result?.semanticFallback}
    <p role="status" aria-live="polite">
      Semantic search was unavailable; keyword results are shown.
    </p>
  {/if}

  {#if result}
    <div aria-live="polite">
      <p>{result.nbHits} {result.nbHits === 1 ? 'result' : 'results'} in {result.processingTimeMs}ms</p>
      {#if result.hits.length === 0}
        <p>No results.</p>
      {:else}
        <ol aria-label="Search results">
          {#each result.hits as hit, index}
            {@const position = resultPosition(index)}
            {@const title = resultTitle(hit, position)}
            <li>
              <article aria-label={title}>
                <h3>{title}</h3>
                <details
                  open={openDetails.has(position)}
                  ontoggle={(event) =>
                    setDetailsOpen(hit, position, (event.currentTarget as HTMLDetailsElement).open)}
                >
                  <summary>{openDetails.has(position) ? 'Close details' : 'Open details'}</summary>
                  <pre>{JSON.stringify(hit, null, 2)}</pre>
                </details>
              </article>
            </li>
          {/each}
        </ol>
      {/if}
    </div>

    {#if result.nbPages > 0}
      <nav aria-label="Search result pages" class="pagination">
        {#if result.page > 0}
          <Button label="Previous page" onpress={() => moveByPage(-1)} disabled={searchLoading} />
        {/if}
        <span>Page {result.page + 1} of {result.nbPages}</span>
        {#if result.page + 1 < result.nbPages}
          <Button label="Next page" onpress={() => moveByPage(1)} disabled={searchLoading} />
        {/if}
      </nav>
    {/if}
  {/if}
</section>

<style>
  .feature_panel {
    padding: var(--console-space-lg);
  }

  .feature_heading,
  form,
  .pagination {
    display: flex;
    align-items: center;
    gap: var(--console-space-md);
  }

  .feature_heading {
    justify-content: space-between;
  }

  pre {
    overflow-wrap: anywhere;
    padding: var(--console-space-md);
    border: var(--console-border-width) solid var(--console-border);
    border-radius: var(--console-radius);
    background: var(--console-surface);
    white-space: pre-wrap;
  }

  ol {
    display: grid;
    gap: var(--console-space-md);
    padding: 0;
    list-style: none;
  }

  article {
    min-width: 0;
    padding: var(--console-space-md);
    border: var(--console-border-width) solid var(--console-border);
    border-radius: var(--console-radius);
    background: var(--console-surface);
  }

  article h3 {
    margin-block-start: 0;
    overflow-wrap: anywhere;
  }

  ol > li {
    min-width: 0;
  }

  summary {
    width: fit-content;
    padding-block: var(--console-space-sm);
    cursor: pointer;
  }

  summary:focus-visible {
    outline: calc(var(--console-border-width) * 2) solid var(--console-focus);
    outline-offset: calc(var(--console-border-width) * 2);
  }

  .state_message {
    display: flex;
    align-items: center;
    gap: var(--console-space-md);
  }

  .analytics_control {
    margin-block: var(--console-space-md);
  }

  .semantic_controls {
    display: grid;
    gap: var(--console-space-sm);
    min-width: 0;
    margin-block: var(--console-space-md);
  }

  .semantic_controls p,
  .semantic_controls output {
    margin: 0;
  }

  .semantic_controls input,
  .semantic_controls select {
    min-width: 0;
    max-width: 100%;
  }

  .analytics_control label {
    display: flex;
    align-items: center;
    gap: var(--console-space-sm);
    width: fit-content;
  }

  .analytics_control p {
    margin-block: var(--console-space-sm) 0;
  }

  .pagination {
    justify-content: space-between;
  }

  @media (max-width: 30rem) {
    form,
    .feature_heading {
      align-items: stretch;
      flex-direction: column;
    }
  }
</style>
