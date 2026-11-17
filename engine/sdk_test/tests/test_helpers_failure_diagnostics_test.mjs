import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

test('real SDK failures retain safe diagnostics, denominator, cleanup and exit status', () => {
  const fixture = fileURLToPath(new URL('./fixtures/runner_failure.mjs', import.meta.url));
  const result = spawnSync(process.execPath, [fixture], {
    encoding: 'utf8', timeout: 10_000, maxBuffer: 1_000_000,
  });
  assert.equal(result.error, undefined, result.error?.message);
  assert.equal(result.signal, null);
  assert.equal(result.status, 1, result.stderr);
  const output = result.stdout;
  assert.match(output, /=== Results: 1 passed, 7 failed ===/);
  assert.match(output, /SLOW_ENDPOINT_SEEN=true/);
  assert.ok(output.indexOf('CLEANUP_DONE') > output.indexOf('=== Results:'));
  assert.match(output, /ordinary fixture failure/);
  assert.match(output, /unreadable fixture failure/);

  const prefix = '  Transport: ';
  const readDetails = (text) => text.split('\n').filter((line) => line.startsWith(prefix))
    .map((line) => JSON.parse(line.slice(prefix.length)));
  const detailsWithin = (name) => {
    const marker = `✗ ${name}\n`;
    const start = output.indexOf(marker);
    assert.ok(start >= 0, `missing failure section: ${name}`);
    const tail = output.slice(start + marker.length);
    const next = tail.search(/\n[✓✗] /);
    return readDetails(next < 0 ? tail : tail.slice(0, next));
  };
  for (const name of ['full-client HTTP failure', 'lite-client HTTP failure']) {
    assert.ok(detailsWithin(name).some((row) => row.method === 'POST'
      && row.path === '/1/indexes/*/queries' && row.status === 503 && row.isTimedOut === false),
    `missing real HTTP diagnostics for ${name}`);
  }
  assert.ok(detailsWithin('deadline failure').some((row) => row.status === 0
    && row.isTimedOut === true && Number.isFinite(row.connectTimeoutMs)
    && Number.isFinite(row.responseTimeoutMs)));
  const details = readDetails(output);
  assert.ok(details.length >= 3, 'missing real SDK transport failure diagnostics');
  assert.ok(details.some((row) => row.method === 'POST'
    && row.path === '/1/indexes/*/queries' && row.status === 503 && row.isTimedOut === false));
  assert.ok(details.some((row) => row.status === 0 && row.isTimedOut === true));
  assert.ok(details.some((row) => Number.isFinite(row.connectTimeoutMs)
    && Number.isFinite(row.responseTimeoutMs)));
  assert.ok(details.every((row) => row.path.length <= 256 && row.method.length <= 12));
  const bounded = output.split('✗ bounded transport metadata')[1].split('=== Results:')[0];
  assert.equal(bounded.split('\n').filter((line) => line.startsWith(prefix)).length, 3);
  for (const marker of [
    'header-key-marker', 'query-key-marker', 'request-body-marker',
    'response-content-marker', 'raw-url-marker', 'method-marker', 'getter-marker',
  ]) {
    assert.ok(!output.includes(marker), `diagnostics exposed ${marker}`);
  }
});
