// Shared test helpers for full_compat_tests.js and instantsearch_contract_tests.js.
// Provides test registration, assertion, polling, cleanup, and runner utilities.

export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function transportFailureDetails(error) {
  // SDK frames also contain credentials and payloads. Keep only bounded wire
  // metadata, and never let diagnostic formatting change the runner's verdict.
  try {
    if (!Array.isArray(error?.stackTrace)) return [];
    const deadline = (value) => typeof value === 'number'
      && Number.isFinite(value) && value >= 0 && value <= 3_600_000 ? value : undefined;
    return error.stackTrace.slice(-3).flatMap((frame) => {
      try {
        const { request, response } = frame;
        if (typeof request?.url !== 'string') return [];
        const url = new URL(request.url);
        if (url.protocol !== 'http:' && url.protocol !== 'https:') return [];
        return [JSON.stringify({
          method: typeof request.method === 'string' && /^[A-Z]{1,12}$/.test(request.method)
            ? request.method : 'unknown',
          path: url.pathname.slice(0, 256),
          status: Number.isInteger(response.status) && response.status >= 0 && response.status <= 599
            ? response.status : undefined,
          isTimedOut: typeof response.isTimedOut === 'boolean' ? response.isTimedOut : undefined,
          connectTimeoutMs: deadline(request.connectTimeout),
          responseTimeoutMs: deadline(request.responseTimeout),
        })];
      } catch {
        return [];
      }
    });
  } catch {
    return [];
  }
}

/**
 * Create a test registry and runner.
 *
 * Returns { test, runAllTests } where:
 *  - test(name, fn) registers an async test
 *  - runAllTests({ banner, setup, cleanup }) runs them sequentially
 */
export function createTestRunner() {
  const tests = [];

  function test(name, fn) {
    tests.push({ name, fn });
  }

  async function runAllTests({ banner = "Tests", setup, cleanup } = {}) {
    if (setup) await setup();

    console.log(`\n=== Running ${banner} (${tests.length}) ===\n`);

    let passed = 0;
    let failed = 0;

    for (const { name, fn } of tests) {
      try {
        await fn();
        console.log(`✓ ${name}`);
        passed++;
      } catch (e) {
        console.log(`✗ ${name}`);
        console.log(`  Error: ${e.message}`);
        for (const detail of transportFailureDetails(e)) {
          console.log(`  Transport: ${detail}`);
        }
        if (e.stack) {
          console.log(`  ${e.stack.split("\n")[1]}`);
        }
        failed++;
      }
    }

    console.log(`\n=== Results: ${passed} passed, ${failed} failed ===\n`);

    if (cleanup) await cleanup();
    process.exit(failed > 0 ? 1 : 0);
  }

  return { test, runAllTests };
}

/**
 * Build PASS/FAIL row printers and summary accounting used by smoke harnesses.
 */
export function createVerdictRecorder() {
  const rows = [];
  let firstFailureError = null;

  function printPass(name, detail) {
    console.log(`PASS ${name}: ${detail}`);
  }

  function printFail(name, detail) {
    console.log(`FAIL ${name}: ${detail}`);
  }

  function printSkip(name, reason) {
    console.log(`SKIP ${name}: ${reason}`);
  }

  async function runStep(name, operation) {
    try {
      const detail = await operation();
      printPass(name, detail ?? "ok");
      rows.push("pass");
      return true;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (firstFailureError === null) {
        firstFailureError = error instanceof Error ? error : new Error(message);
      }
      printFail(name, message);
      rows.push("fail");
      return false;
    }
  }

  function runSkip(name, reason) {
    const detail = reason ?? "policy skip";
    printSkip(name, detail);
    rows.push("skip");
    return true;
  }

  function summarize() {
    const passCount = rows.filter((row) => row === "pass").length;
    const failCount = rows.filter((row) => row === "fail").length;
    const skipCount = rows.filter((row) => row === "skip").length;
    console.log(`SUMMARY pass=${passCount} fail=${failCount} skip=${skipCount}`);
    return { passCount, failCount, skipCount, totalCount: rows.length };
  }

  function getFirstFailureError() {
    return firstFailureError;
  }

  return { runStep, runSkip, summarize, getFirstFailureError };
}

/**
 * Search a single index and return the first result object.
 */
export async function searchIndex(client, indexName, params = {}) {
  const response = await client.search({
    requests: [{ indexName, ...params }]
  });
  return response.results[0];
}

/**
 * Poll a search endpoint until `predicate(result)` returns truthy.
 * Returns the result on success, or null on timeout.
 */
export async function waitForSearch(client, indexName, params, predicate, maxWaitMs = 7000) {
  const start = Date.now();
  while (Date.now() - start < maxWaitMs) {
    try {
      const result = await searchIndex(client, indexName, params);
      if (predicate(result)) {
        return result;
      }
    } catch (e) {
      // Keep polling until ready.
    }
    await sleep(100);
  }
  return null;
}

/**
 * Poll getSettings until `predicate(settings)` returns truthy.
 * Returns the settings on success, or null on timeout.
 */
export async function waitForSettings(client, indexName, predicate, maxWaitMs = 7000) {
  const start = Date.now();
  while (Date.now() - start < maxWaitMs) {
    try {
      const settings = await client.getSettings({ indexName });
      if (predicate(settings)) {
        return settings;
      }
    } catch (e) {
      // Keep polling until settings become readable.
    }
    await sleep(100);
  }
  return null;
}

/**
 * Wait until an index reaches at least `minHits` total documents.
 * Throws on timeout.
 */
export async function waitForMinHits(client, indexName, minHits, maxWaitMs = 7000) {
  const ready = await waitForSearch(
    client,
    indexName,
    { query: "", hitsPerPage: 0 },
    (result) => typeof result.nbHits === "number" && result.nbHits >= minHits,
    maxWaitMs
  );
  if (!ready) {
    throw new Error(`Timed out waiting for ${indexName} to reach at least ${minHits} hits`);
  }
}

/**
 * Poll getObject until `predicate(object)` returns truthy.
 * Returns the object on success, or null on timeout.
 */
export async function waitForObject(client, indexName, objectID, predicate, maxWaitMs = 5000) {
  const start = Date.now();
  while (Date.now() - start < maxWaitMs) {
    try {
      const obj = await client.getObject({ indexName, objectID });
      if (predicate(obj)) {
        return obj;
      }
    } catch (e) {
      // Keep polling until object appears/updates.
    }
    await sleep(50);
  }
  return null;
}

/**
 * Poll listIndices until the named index no longer appears.
 * Returns true on success, false on timeout.
 */
export async function waitForIndexMissing(client, indexName, maxWaitMs = 5000) {
  const start = Date.now();
  while (Date.now() - start < maxWaitMs) {
    const list = await client.listIndices();
    if (Array.isArray(list.items) && !list.items.some((idx) => idx.name === indexName)) {
      return true;
    }
    await sleep(50);
  }
  return false;
}

/**
 * Best-effort index deletion (swallows 404).
 */
export async function deleteIndexIfPresent(client, indexName) {
  try {
    await client.deleteIndex({ indexName });
  } catch (e) {
    // Index may not exist, which is fine.
  }
}

/**
 * Delete a list of indexes and wait for each to disappear.
 */
export async function cleanupIndexes(client, indexNames) {
  for (const indexName of indexNames) {
    await deleteIndexIfPresent(client, indexName);
  }
  for (const indexName of indexNames) {
    await waitForIndexMissing(client, indexName);
  }
}

/**
 * Return client-bound wrappers to avoid repeating thin adapter functions
 * in every test file.
 */
export function bindClientHelpers(client) {
  return {
    searchIndex: (indexName, params = {}) => searchIndex(client, indexName, params),
    waitForSearch: (indexName, params, predicate, maxWaitMs) =>
      waitForSearch(client, indexName, params, predicate, maxWaitMs),
    waitForSettings: (indexName, predicate, maxWaitMs) =>
      waitForSettings(client, indexName, predicate, maxWaitMs),
    waitForMinHits: (indexName, minHits, maxWaitMs) =>
      waitForMinHits(client, indexName, minHits, maxWaitMs),
    waitForObject: (indexName, objectID, predicate, maxWaitMs) =>
      waitForObject(client, indexName, objectID, predicate, maxWaitMs),
    waitForIndexMissing: (indexName, maxWaitMs) =>
      waitForIndexMissing(client, indexName, maxWaitMs),
    cleanupIndexes: (indexNames) => cleanupIndexes(client, indexNames)
  };
}
