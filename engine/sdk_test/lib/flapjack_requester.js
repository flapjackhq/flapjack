import { algoliasearch } from 'algoliasearch';
import { liteClient } from 'algoliasearch/lite';

function flapjackClientOptions(baseUrl, apiKey) {
  if (!baseUrl || !apiKey) {
    throw new Error('Flapjack client requires non-empty baseUrl and apiKey values');
  }

  const target = new URL(baseUrl);
  if ((target.pathname && target.pathname !== '/') || target.search || target.hash) {
    throw new Error(`Flapjack baseUrl must contain only an origin, got ${baseUrl}`);
  }

  return {
    hosts: [{
      url: target.host,
      protocol: target.protocol.slice(0, -1),
      accept: 'readWrite',
    }],
  };
}

/** Build the full client used by fixture administration and Node compatibility tests. */
export function createFlapjackSearchClient({ baseUrl, apiKey }) {
  return algoliasearch('flapjack', apiKey, flapjackClientOptions(baseUrl, apiKey));
}

/** Build the official search-only client shipped in customer browser examples. */
export function createFlapjackLiteSearchClient({ baseUrl, apiKey }) {
  // `hosts` is the official configuration seam. Transport remains inside the
  // upstream lite package, so browser wire-format changes make conformance red.
  return liteClient('flapjack', apiKey, flapjackClientOptions(baseUrl, apiKey));
}
