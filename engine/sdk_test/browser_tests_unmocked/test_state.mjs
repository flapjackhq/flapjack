import { createFlapjackSearchClient } from '../lib/flapjack_requester.js';
import { INDEX_SETTINGS, PRODUCTS } from './fixture_data.mjs';

function requiredEnvironment(name) {
  const value = process.env[name];
  if (!value) {
    throw new Error(`Missing required ${name} for real-client conformance`);
  }
  return value;
}

export function testConfiguration() {
  return {
    baseUrl: requiredEnvironment('FLAPJACK_URL'),
    apiKey: requiredEnvironment('FLAPJACK_ADMIN_KEY'),
    indexName: requiredEnvironment('REAL_CLIENT_INDEX_NAME'),
  };
}

function clientForTest() {
  const { baseUrl, apiKey } = testConfiguration();
  return createFlapjackSearchClient({ baseUrl, apiKey });
}

async function indexExists(client, indexName) {
  const response = await client.listIndices();
  if (!Array.isArray(response.items)) {
    throw new Error(`Malformed listIndices response: ${JSON.stringify(response)}`);
  }
  return response.items.some((item) => item.name === indexName);
}

export async function removeFixtureIndex({ allowMissing }) {
  const client = clientForTest();
  const { indexName } = testConfiguration();
  const exists = await indexExists(client, indexName);
  if (!exists) {
    if (allowMissing) return;
    throw new Error(`Cleanup cannot prove removal because ${indexName} was already missing`);
  }

  await client.deleteIndex({ indexName });
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    if (!(await indexExists(client, indexName))) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`Cleanup did not remove fixture index ${indexName}`);
}

export async function seedFixtureIndex() {
  const client = clientForTest();
  const { indexName } = testConfiguration();
  await removeFixtureIndex({ allowMissing: true });

  const settingsTask = await client.setSettings({ indexName, indexSettings: INDEX_SETTINGS });
  if (!Number.isInteger(settingsTask.taskID)) {
    throw new Error(`setSettings returned no numeric taskID: ${JSON.stringify(settingsTask)}`);
  }
  await client.waitForTask({ indexName, taskID: settingsTask.taskID });

  const saveTasks = await client.saveObjects({ indexName, objects: PRODUCTS });
  const taskList = Array.isArray(saveTasks) ? saveTasks : [saveTasks];
  if (taskList.length === 0 || taskList.some((task) => !Number.isInteger(task.taskID))) {
    throw new Error(`saveObjects returned malformed tasks: ${JSON.stringify(saveTasks)}`);
  }
  for (const task of taskList) {
    await client.waitForTask({ indexName, taskID: task.taskID });
  }

  const result = await client.search({
    requests: [{ indexName, query: '', hitsPerPage: PRODUCTS.length }],
  });
  const hits = result.results?.[0]?.hits;
  if (!Array.isArray(hits) || hits.length !== PRODUCTS.length) {
    throw new Error(`Fixture readiness expected ${PRODUCTS.length} hits, got ${JSON.stringify(result)}`);
  }
}

export async function createFixtureSearchKey() {
  const client = clientForTest();
  const { indexName } = testConfiguration();
  const created = await client.addApiKey({
    acl: ['search', 'browse'],
    description: 'Temporary real-client browser fixture key',
    indexes: [indexName],
  });
  if (typeof created.key !== 'string' || created.key.length === 0) {
    throw new Error(`addApiKey returned no key: ${JSON.stringify(created)}`);
  }

  try {
    // Reading the key back proves both restrictions reached the canonical key store
    // before a browser is allowed to use the credential.
    const stored = await client.getApiKey({ key: created.key });
    if (JSON.stringify(stored.acl) !== JSON.stringify(['search', 'browse'])
      || JSON.stringify(stored.indexes) !== JSON.stringify([indexName])) {
      throw new Error(`Fixture key restrictions were not persisted: ${JSON.stringify(stored)}`);
    }
    return created.key;
  } catch (error) {
    // Creation succeeded, so this function still owns the key when readiness proof fails.
    await client.deleteApiKey({ key: created.key });
    throw error;
  }
}

export async function removeFixtureSearchKey(key) {
  const client = clientForTest();
  await client.deleteApiKey({ key });
  const remaining = await client.listApiKeys();
  if (!Array.isArray(remaining.keys) || remaining.keys.some((candidate) => candidate.value === key)) {
    throw new Error('Cleanup did not remove the temporary browser search key');
  }
}
