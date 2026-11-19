import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { algoliasearch } from 'algoliasearch';
import { createFlapjackClient } from './lib/flapjack-client.js';

// This JSON is also consumed byte-for-byte by the cloud acceptance journey.
const fixturePath = new URL('./fixtures/algolia_migrant_recall.json', import.meta.url);
const fixture = JSON.parse(await readFile(fixturePath, 'utf8'));
const capture = process.argv.includes('--capture');
if (capture && (!process.env.ALGOLIA_APP_ID || !process.env.ALGOLIA_ADMIN_KEY)) {
  throw new Error('Capture requires the repo-owned Algolia credentials');
}
const client = capture
  ? algoliasearch(process.env.ALGOLIA_APP_ID, process.env.ALGOLIA_ADMIN_KEY)
  : createFlapjackClient();
const prefix = capture ? 'fj_e_' : 'migrant_recall_';
const names = Object.fromEntries(Object.keys(fixture.indexes).map((key) => [key, `${prefix}${randomUUID()}_${key}`]));
const ids = (result) => result.hits.map((hit) => hit.objectID);
const sorted = (values) => [...values].sort();

async function search(key, row) {
  return client.searchSingleIndex({ indexName: names[key], searchParams: { query: row.query, ...row.params } });
}

function select(row, result) {
  switch (row.kind) {
    case 'ids': return sorted(ids(result));
    case 'orderedIDs': return ids(result);
    case 'counts': return { nbHits: result.nbHits, hits: result.hits.length };
    case 'highlight': return result.hits.find((hit) => hit.objectID === row.objectID)?._highlightResult?.[row.attribute]?.value;
    case 'facets': return result.facets;
    case 'facetValues': return result.facetHits.map((hit) => hit.value);
    default: throw new Error(`Unknown assertion kind ${row.kind}`);
  }
}

async function main() {
  let failures = 0;
  const cleanupFailures = [];
  try {
    for (const [key, config] of Object.entries(fixture.indexes)) {
      if (Object.keys(config.settings).length) {
        const { taskID } = await client.setSettings({ indexName: names[key], indexSettings: config.settings });
        await client.waitForTask({ indexName: names[key], taskID });
      }
      const objects = fixture.objects.map((object) => config.reverseKeys
        ? Object.fromEntries(Object.entries(object).reverse()) : object);
      const batches = await client.saveObjects({ indexName: names[key], objects });
      for (const { taskID } of batches) await client.waitForTask({ indexName: names[key], taskID });
    }
    for (const row of fixture.assertions) {
      try {
        if (row.kind === 'equivalentKeyOrder') {
          for (const queryRow of fixture.assertions.filter((item) => item.index === 'A')) {
            const [a, b] = await Promise.all([search('A', queryRow), search('B', queryRow)]);
            assert.deepEqual(sorted(ids(a)), sorted(ids(b)), `key order changed hits for ${queryRow.query}`);
          assert.equal(a.nbHits, b.nbHits, `key order changed count for ${queryRow.query}`);
            if (queryRow.kind === 'orderedIDs') assert.deepEqual(ids(a), ids(b));
          }
        } else {
          const result = row.kind === 'facetValues'
            ? await client.searchForFacetValues({ indexName: names[row.index], facetName: row.facetName, searchForFacetValuesRequest: { facetQuery: row.facetQuery } })
            : await search(row.index, row);
          const actual = select(row, result);
          if (capture) {
            assert.notEqual(actual, undefined, `Missing oracle value for assertion ${row.id}`);
            try { assert.deepEqual(actual, row.kind === 'ids' ? sorted(row.expected) : row.expected); }
            catch { fixture.oracle.deviations.push({ assertion: row.id, spec: row.expected, algolia: actual }); }
            row.expected = actual;
          } else {
            assert.deepEqual(actual, row.kind === 'ids' ? sorted(row.expected) : row.expected);
          }
        }
        console.log(`PASS ${row.id}: ${row.query ?? row.kind}`);
      } catch (error) {
        failures++;
        console.log(`FAIL ${row.id}: ${row.query ?? row.kind}: ${error.message}`);
      }
    }
  } catch (error) {
    failures++;
    console.error(`Setup failed: ${error.message}`);
  } finally {
    for (const indexName of Object.values(names)) {
      try {
        const { taskID } = await client.deleteIndex({ indexName });
        await client.waitForTask({ indexName, taskID });
        const { items } = await client.listIndices();
        assert.equal(items.some((item) => item.name === indexName), false, 'Deleted index remains visible');
        console.log(`CLEAN ${indexName}`);
      } catch (error) {
        if (error.status !== 404) cleanupFailures.push(indexName);
      }
    }
  }
  if (cleanupFailures.length) throw new Error(`Cleanup failed for ${cleanupFailures.join(', ')}`);
  if (failures) throw new Error(`${failures} of 27 assertions failed`);
  if (capture) {
    fixture.oracle.capturedAt = new Date().toISOString();
    await writeFile(fixturePath, `${JSON.stringify(fixture, null, 2)}\n`);
    console.log(`Captured 27 assertions; deviations: ${JSON.stringify(fixture.oracle.deviations)}`);
  }

}

await main().catch((error) => {
  console.error(`Migrant recall failed: ${error.message}`);
  process.exitCode = 1;
});
