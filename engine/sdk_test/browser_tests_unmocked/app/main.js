import aa from 'search-insights';
import { createFlapjackLiteSearchClient } from '../../lib/flapjack_requester.js';

const pageParams = new URLSearchParams(window.location.search);
const clientName = pageParams.get('client');
const supportedClients = new Set(['vanilla', 'react', 'vue']);
const status = document.querySelector('[data-testid="client_status"]');
const heading = document.querySelector('[data-testid="client_heading"]');
const root = document.querySelector('#search_root');

if (!supportedClients.has(clientName)) {
  throw new Error(`Unsupported real-client adapter: ${clientName}`);
}

const configuration = {
  baseUrl: import.meta.env.VITE_FLAPJACK_URL,
  applicationId: import.meta.env.VITE_FLAPJACK_APPLICATION_ID,
  apiKey: import.meta.env.VITE_FLAPJACK_SEARCH_KEY,
  indexName: import.meta.env.VITE_REAL_CLIENT_INDEX_NAME,
  userToken: import.meta.env.VITE_REAL_CLIENT_USER_TOKEN,
};
if (Object.values(configuration).some((value) => !value)) {
  throw new Error('Real-client browser configuration is incomplete');
}

const searchClient = createFlapjackLiteSearchClient(configuration);
aa('init', {
  appId: configuration.applicationId,
  apiKey: configuration.apiKey,
  host: configuration.baseUrl,
  useCookie: false,
  userToken: configuration.userToken,
});
const trackClick = (hit) => aa('clickedObjectIDsAfterSearch', {
  eventName: 'PBV3 product clicked',
  index: configuration.indexName,
  queryID: hit.__queryID,
  objectIDs: [hit.objectID],
  positions: [hit.__position],
  userToken: configuration.userToken,
});
if (pageParams.get('probe') === 'insights-transport') {
  aa('clickedObjectIDsAfterSearch', {
    eventName: 'PBV3 Insights transport probe',
    index: configuration.indexName,
    queryID: '00000000000000000000000000000000',
    objectIDs: ['transport_probe'],
    positions: [1],
    userToken: configuration.userToken,
  });
  heading.textContent = 'search-insights transport probe';
  status.textContent = 'search-insights request dispatched';
} else {
  const adapter = await import(`./${clientName}_adapter.js`);
  adapter.mountSearch({
    root,
    searchClient,
    indexName: configuration.indexName,
    userToken: configuration.userToken,
    trackClick,
  });

  heading.textContent = `${clientName} InstantSearch`;
  status.textContent = `${clientName} client mounted`;
}
