<script lang="ts">
  import { onMount } from 'svelte';
  import { base } from '$app/paths';
  import './lib/design/tokens.css';
  import './app.css';
  import type { AuthenticatedSession, SessionProvider } from './host/session';
  import EngineApiKeys from './host/EngineApiKeys.svelte';
  import EngineSecuritySources from './host/EngineSecuritySources.svelte';
  import { IndexListSearch, IndexSearch } from './lib/features';
  import { Button } from './lib/ui';

  let {
    sessionProvider,
    screen = 'indexes',
    indexName,
  }: {
    sessionProvider: SessionProvider;
    screen?: 'indexes' | 'apiKeys' | 'securitySources';
    indexName?: string;
  } = $props();
  let apiKey = $state('');
  let error = $state('');
  let session = $state<AuthenticatedSession | null>(null);
  let restoringSession = $state(true);

  onMount(() => {
    void restoreSession();
  });

  async function restoreSession(): Promise<void> {
    try {
      session = await sessionProvider.restore();
    } catch {
      session = null;
    } finally {
      restoringSession = false;
    }
  }

  async function signIn(): Promise<void> {
    error = '';
    try {
      session = await sessionProvider.signIn(apiKey);
      apiKey = '';
    } catch {
      error = 'Could not authenticate.';
    }
  }

  async function signOut(): Promise<void> {
    if (!session) return;
    try {
      await session.signOut();
      session = null;
    } catch {
      error = 'Could not sign out.';
    }
  }
</script>

<svelte:head>
  <title>Flapjack Console</title>
</svelte:head>

{#if restoringSession}
  <main
    aria-label="Console session check"
    class="console_auth"
    data-console-host="standalone"
    data-console-theme="flapjack"
  >
    <p role="status">Checking session…</p>
  </main>
{:else if session}
  <main
    aria-label="Standalone console"
    class="console_host"
    data-console-host="standalone"
    data-console-theme="flapjack"
  >
    <header class="console_header">
      <h1>Flapjack Console</h1>
      <nav aria-label="Console">
        <a href={`${base}/`} aria-current={screen === 'indexes' ? 'page' : undefined}>Indexes</a>
        <a href={`${base}/keys`} aria-current={screen === 'apiKeys' ? 'page' : undefined}>API Keys</a>
        <a
          href={`${base}/security-sources`}
          aria-current={screen === 'securitySources' ? 'page' : undefined}
        >Security Sources</a>
        <Button label="Sign out" onpress={signOut} />
      </nav>
    </header>
    <section aria-label="Console content" class="console_content">
      {#if screen === 'apiKeys'}
        <EngineApiKeys transport={session.transport} />
      {:else if screen === 'securitySources'}
        <EngineSecuritySources transport={session.transport} />
      {:else if indexName}
        <a class="direct_search_back" href={`${base}/`}>Back to indexes</a>
        {#key indexName}
          <IndexSearch
            transport={session.transport}
            {indexName}
            searchAnalyticsCopy={{ toggleLabel: 'Track Analytics' }}
          />
        {/key}
      {:else}
        <IndexListSearch
          transport={session.transport}
          searchAnalyticsCopy={{ toggleLabel: 'Track Analytics' }}
        />
      {/if}
    </section>
  </main>
{:else}
  <main
    aria-label="Console authentication"
    class="console_auth"
    data-console-host="standalone"
    data-console-theme="flapjack"
  >
    <form
      onsubmit={(event) => {
        event.preventDefault();
        void signIn();
      }}
    >
      <h1>Flapjack Console</h1>
      <label for="admin_api_key">Admin API Key</label>
      <input id="admin_api_key" name="apiKey" type="password" bind:value={apiKey} required />
      <Button label="Connect" type="submit" />
      {#if error}<p role="alert">{error}</p>{/if}
    </form>
  </main>
{/if}

<style>
  .direct_search_back {
    display: inline-block;
    margin: var(--console-space-lg) var(--console-space-lg) 0;
  }
</style>
