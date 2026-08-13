import { spawn } from 'node:child_process';
import net from 'node:net';
import { dirname, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import {
  createFixtureSearchKey,
  removeFixtureIndex,
  removeFixtureSearchKey,
  seedFixtureIndex,
} from './test_state.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const sdkDir = resolve(here, '..');
async function allocateLoopbackPort() {
  const server = net.createServer();
  await new Promise((resolveListen, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolveListen);
  });
  const address = server.address();
  if (!address || typeof address === 'string') {
    server.close();
    throw new Error('Unable to allocate a loopback port for the fixture app');
  }
  await new Promise((resolveClose, reject) => server.close((error) => error ? reject(error) : resolveClose()));
  return address.port;
}

const webPort = await allocateLoopbackPort();
const indexName = `real_client_${process.pid}_${Date.now()}`;
const playwrightCli = fileURLToPath(import.meta.resolve('@playwright/test/cli'));
process.env.REAL_CLIENT_INDEX_NAME = indexName;
let child;
let fixtureSeeded = false;
let searchKey;

function stopOwnedProcessGroup() {
  if (!Number.isInteger(child?.pid)) return;
  try {
    process.kill(-child.pid, 'SIGTERM');
  } catch (error) {
    if (error.code !== 'ESRCH') throw error;
  }
}

async function requireOwnedProcessGroupStopped() {
  if (!Number.isInteger(child?.pid)) return;
  for (let attempt = 0; attempt < 20; attempt += 1) {
    try {
      process.kill(-child.pid, 0);
    } catch (error) {
      if (error.code === 'ESRCH') return;
      throw error;
    }
    await new Promise((resolveWait) => setTimeout(resolveWait, 50));
  }
  throw new Error(`Browser process group ${child.pid} survived cleanup`);
}

process.once('SIGINT', () => stopOwnedProcessGroup());
process.once('SIGTERM', () => stopOwnedProcessGroup());

let exitCode;
let runError;
try {
  await seedFixtureIndex();
  fixtureSeeded = true;
  searchKey = await createFixtureSearchKey();
  const environment = {
    ...process.env,
    REAL_CLIENT_WEB_PORT: String(webPort),
    REAL_CLIENT_INDEX_NAME: indexName,
    REAL_CLIENT_SEARCH_KEY: searchKey,
  };

  child = spawn(process.execPath, [
    playwrightCli,
    'test',
    '--config',
    resolve(here, 'playwright.config.mjs'),
  ], {
    cwd: sdkDir,
    env: environment,
    stdio: 'inherit',
    // The dedicated group lets this wrapper clean only descendants it created.
    detached: true,
  });
  exitCode = await new Promise((resolveExit, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => {
      if (signal) reject(new Error(`Playwright exited via ${signal}`));
      else resolveExit(code ?? 1);
    });
  });
} catch (error) {
  runError = error;
} finally {
  // This runner owns every fixture and process, so failures cannot leave a usable
  // browser key, an index, Vite, or Chromium behind for a later run to mistake as proof.
  stopOwnedProcessGroup();
  try {
    await requireOwnedProcessGroupStopped();
  } finally {
    try {
      if (searchKey) await removeFixtureSearchKey(searchKey);
    } finally {
      // If seeding failed midway, remove any partial index but allow the legitimate
      // case where creation failed before an index existed.
      await removeFixtureIndex({ allowMissing: !fixtureSeeded });
    }
  }
}
if (runError) throw runError;
process.exitCode = exitCode ?? 1;
