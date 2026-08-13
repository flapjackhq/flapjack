import { createFlapjackLiteSearchClient } from '../../lib/flapjack_requester.js';

const clientName = new URLSearchParams(window.location.search).get('client');
const supportedClients = new Set(['vanilla', 'react', 'vue']);
const status = document.querySelector('[data-testid="client_status"]');
const heading = document.querySelector('[data-testid="client_heading"]');
const root = document.querySelector('#search_root');

if (!supportedClients.has(clientName)) {
  throw new Error(`Unsupported real-client adapter: ${clientName}`);
}

const configuration = {
  baseUrl: import.meta.env.VITE_FLAPJACK_URL,
  apiKey: import.meta.env.VITE_FLAPJACK_SEARCH_KEY,
  indexName: import.meta.env.VITE_REAL_CLIENT_INDEX_NAME,
};
if (Object.values(configuration).some((value) => !value)) {
  throw new Error('Real-client browser configuration is incomplete');
}

const searchClient = createFlapjackLiteSearchClient(configuration);
const adapter = await import(`./${clientName}_adapter.js`);
adapter.mountSearch({ root, searchClient, indexName: configuration.indexName });

heading.textContent = `${clientName} InstantSearch`;
status.textContent = `${clientName} client mounted`;
