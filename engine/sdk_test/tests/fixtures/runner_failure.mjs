import { createServer } from 'node:http';
import { createTestRunner } from '../../lib/test-helpers.js';
import {
  createFlapjackSearchClient,
  createFlapjackLiteSearchClient,
} from '../../lib/flapjack_requester.js';

let slowEndpointSeen = false;
const server = createServer((request, response) => {
  let body = '';
  request.on('data', (chunk) => { body += chunk; });
  request.on('end', () => {
    if (JSON.parse(body).requests[0].indexName === 'slow') {
      slowEndpointSeen = true;
      return;
    }
    response.writeHead(503, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ message: 'response-content-marker' }));
  });
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const baseUrl = `http://127.0.0.1:${server.address().port}`;
const full = createFlapjackSearchClient({ baseUrl, apiKey: 'header-key-marker' });
const lite = createFlapjackLiteSearchClient({
  baseUrl, apiKey: 'query-key-marker', applicationId: 'fixture',
});
const { test, runAllTests } = createTestRunner();

test('successful fixture', async () => {});
test('full-client HTTP failure', async () => full.search({
  requests: [{ indexName: 'full', query: 'request-body-marker' }],
}));
test('lite-client HTTP failure', async () => lite.search({
  requests: [{ indexName: 'lite', query: 'request-body-marker' }],
}));
test('deadline failure', async () => full.search({
  requests: [{ indexName: 'slow', query: 'request-body-marker' }],
}, { timeouts: { connect: 250, read: 250 } }));
test('ordinary failure', async () => { throw new Error('ordinary fixture failure'); });
test('malformed transport metadata', async () => {
  const error = new Error('malformed fixture failure');
  error.stackTrace = [null, {
    request: { url: 'not-a-url/raw-url-marker', method: 'method-marker' },
    response: { status: 503, content: 'response-content-marker' },
  }, {
    request: { url: 'data:text/plain,raw-url-marker', method: 'GET' },
    response: { status: 503 },
  }];
  throw error;
});
test('unreadable transport metadata', async () => {
  const error = new Error('unreadable fixture failure');
  Object.defineProperty(error, 'stackTrace', {
    get() { throw new Error('getter-marker'); },
  });
  throw error;
});
test('bounded transport metadata', async () => {
  const error = new Error('bounded fixture failure');
  error.stackTrace = Array.from({ length: 20 }, () => ({
    request: {
      url: `${baseUrl}/${'x'.repeat(1000)}?secret=query-key-marker`,
      method: 'method-marker', connectTimeout: Infinity,
      headers: { authorization: 'header-key-marker' }, data: 'request-body-marker',
    },
    response: { status: 503, isTimedOut: false, content: 'response-content-marker' },
  }));
  throw error;
});

await runAllTests({
  banner: 'Controlled runner failures',
  cleanup: async () => {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
    console.log(`SLOW_ENDPOINT_SEEN=${slowEndpointSeen}`);
    console.log('CLEANUP_DONE');
  },
});
