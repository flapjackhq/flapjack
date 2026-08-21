import { algoliasearch } from 'algoliasearch';
import { liteClient } from 'algoliasearch/lite';

function flapjackClientOptions(baseUrl, apiKey, authMode) {
  if (!baseUrl || !apiKey) {
    throw new Error('Flapjack client requires non-empty baseUrl and apiKey values');
  }

  const target = new URL(baseUrl);
  if ((target.pathname && target.pathname !== '/') || target.search || target.hash) {
    throw new Error(`Flapjack baseUrl must contain only an origin, got ${baseUrl}`);
  }
  const loopback = target.hostname === 'localhost'
    || target.hostname === '127.0.0.1'
    || target.hostname === '[::1]';
  if (!loopback && target.protocol !== 'https:') {
    throw new Error('Non-loopback Flapjack origins must use HTTPS');
  }

  return {
    authMode,
    hosts: [{
      url: target.host,
      protocol: target.protocol.slice(0, -1),
      accept: 'readWrite',
    }],
  };
}

/** Build the full client used by fixture administration and Node compatibility tests. */
export function createFlapjackSearchClient({ baseUrl, apiKey, applicationId = 'flapjack' }) {
  return algoliasearch(
    applicationId,
    apiKey,
    flapjackClientOptions(baseUrl, apiKey, 'WithinHeaders'),
  );
}

/** Build the official search-only client shipped in customer browser examples. */
export function createFlapjackLiteSearchClient({ baseUrl, apiKey, applicationId }) {
  if (!applicationId) {
    throw new Error('Flapjack lite client requires a non-empty applicationId');
  }
  // `hosts` is the official configuration seam. Transport remains inside the
  // upstream lite package, so browser wire-format changes make conformance red.
  return liteClient(
    applicationId,
    apiKey,
    flapjackClientOptions(baseUrl, apiKey, 'WithinQueryParameters'),
  );
}
