<script lang="ts">
  import { page } from '$app/state';
  import StandaloneHost from '../../../StandaloneHost.svelte';
  import { createEngineSessionProvider } from '../../../host/engine_session';
  import { createEngineTransport } from '../../../host/engine_transport';

  const sessionProvider = createEngineSessionProvider(
    globalThis.fetch,
    createEngineTransport(globalThis.fetch)
  );
</script>

<StandaloneHost {sessionProvider} indexName={page.params.indexName} />
