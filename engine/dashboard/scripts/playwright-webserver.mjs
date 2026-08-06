import fs from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import process from 'node:process';
import { spawn } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const DEFAULT_TIMEOUT_MS = 120_000;
const DEFAULT_POLL_INTERVAL_MS = 250;
const PORT_PROBE_TIMEOUT_MS = 500;
const BACKEND_LABEL = 'backend server';
const DASHBOARD_LABEL = 'dashboard dev server';
/** Canonical dashboard-owned command that produces bin/flapjack-stable. */
const BACKEND_BUILD_COMMAND = 'npm run update-server';
/** Single owner of backend binary path, admin key, and secret-env loading. */
const BACKEND_START_SCRIPT = 'scripts/start-stable-server.sh';

function parseInteger(value, fallback) {
  const parsed = Number.parseInt(value ?? '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

export function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export async function probeUrl(url, fetchImpl = fetch) {
  try {
    const response = await fetchImpl(url, { method: 'GET' });
    return response.status < 500;
  } catch {
    return false;
  }
}

export function probePort(host, port) {
  return new Promise((resolve) => {
    const socket = net.createConnection({ host, port });
    let settled = false;

    const finish = (value) => {
      if (settled) {
        return;
      }
      settled = true;
      socket.destroy();
      resolve(value);
    };

    socket.setTimeout(PORT_PROBE_TIMEOUT_MS);
    socket.once('connect', () => finish(true));
    socket.once('timeout', () => finish(false));
    socket.once('error', () => finish(false));
  });
}

function buildStartupLeasePath(port) {
  return path.join(os.tmpdir(), `flapjack-playwright-webserver-${port}.lock`);
}

export async function acquireStartupLease(lockPath) {
  try {
    const handle = await fs.open(lockPath, 'wx');
    let released = false;

    return async () => {
      if (released) {
        return;
      }
      released = true;
      await handle.close().catch(() => {});
      await fs.unlink(lockPath).catch(() => {});
    };
  } catch (error) {
    if (error && typeof error === 'object' && 'code' in error && error.code === 'EEXIST') {
      return null;
    }
    throw error;
  }
}

function readWebServerEnv() {
  return {
    url: process.env.PLAYWRIGHT_WEBSERVER_URL,
    host: process.env.PLAYWRIGHT_WEBSERVER_HOST,
    port: parseInteger(process.env.PLAYWRIGHT_WEBSERVER_PORT, 0),
    allowReuse: process.env.PLAYWRIGHT_WEBSERVER_REUSE !== '0',
    timeoutMs: parseInteger(process.env.PLAYWRIGHT_WEBSERVER_TIMEOUT_MS, DEFAULT_TIMEOUT_MS),
    pollIntervalMs: parseInteger(
      process.env.PLAYWRIGHT_WEBSERVER_POLL_INTERVAL_MS,
      DEFAULT_POLL_INTERVAL_MS,
    ),
  };
}

function readBackendEnv() {
  return {
    url: process.env.PLAYWRIGHT_BACKEND_URL,
    host: process.env.PLAYWRIGHT_BACKEND_HOST,
    port: parseInteger(process.env.PLAYWRIGHT_BACKEND_PORT, 0),
    dataDir: process.env.PLAYWRIGHT_BACKEND_DATA_DIR,
    // This exception is intentionally as narrow as the P20 fixture that consumes it.
    // Ordinary suites still fail startup against a text-only backend.
    allowTextOnlyNegativeControl: process.env.P20_TEXT_ONLY_NEGATIVE_CONTROL === '1',
  };
}

async function waitForUrlReady({
  label,
  url,
  timeoutMs,
  pollIntervalMs,
  probeUrl: probeUrlImpl,
  sleep,
}) {
  const deadline = Date.now() + timeoutMs;

  while (Date.now() <= deadline) {
    if (await probeUrlImpl(url)) {
      return;
    }
    await sleep(pollIntervalMs);
  }

  throw new Error(`Timed out waiting for ${label} at ${url}`);
}

/**
 * Reads GET /health and fails unless the backend was compiled with vector search.
 *
 * `skipWhenVectorSearchDisabled` in tests/fixtures/api-helpers.ts reads the same
 * field, so a text-only backend turns the chat, hybrid-search, vector-settings and
 * navigation specs into silent skips while the run still reports green. Asserting
 * the capability at startup converts that invisible coverage loss into a loud,
 * non-zero webserver failure.
 */
export async function assertVectorSearchEnabled(backendBaseUrl, {
  fetchImpl = fetch,
  allowTextOnlyNegativeControl = false,
} = {}) {
  const healthUrl = `${backendBaseUrl}/health`;
  const response = await fetchImpl(healthUrl, { method: 'GET' });

  if (!response.ok) {
    throw new Error(`Backend health probe failed (${response.status}) at ${healthUrl}`);
  }

  const body = await response.json();
  const capabilities = body?.capabilities ?? {};

  if (capabilities.vectorSearch !== true && !allowTextOnlyNegativeControl) {
    throw new Error(
      `Backend at ${backendBaseUrl} reports vectorSearch=${capabilities.vectorSearch}. `
      + 'Dashboard e2e-ui requires a vector-search-enabled build, otherwise the '
      + 'vector specs silently skip. Rebuild the stable backend binary with: '
      + `${BACKEND_BUILD_COMMAND}`,
    );
  }

  return capabilities;
}

/** Starts the backend through its existing owner, pinned to the Playwright target. */
export function spawnBackendServer({ host, port, dataDir }, spawnImpl = spawn) {
  return spawnImpl('bash', [BACKEND_START_SCRIPT], {
    stdio: 'inherit',
    env: {
      ...process.env,
      FLAPJACK_BIND_ADDR: `${host}:${port}`,
      FLAPJACK_AI_ALLOW_LOCAL_URLS: '1',
      ...(dataDir ? { FLAPJACK_DATA_DIR: dataDir } : {}),
    },
  });
}

/** Starts the Vite dashboard the browser tests navigate to. */
export function spawnDashboardServer(spawnImpl = spawn) {
  return spawnImpl('npm', ['run', 'dev'], {
    stdio: 'inherit',
    env: process.env,
  });
}

export function resolveWaitForPortFreeTarget({ url, host, port }) {
  if (url) {
    try {
      const parsed = new URL(url);
      const parsedPort = parseInteger(parsed.port, parsed.protocol === 'https:' ? 443 : 80);
      const parsedHost = parsed.hostname.replace(/^\[(.*)\]$/, '$1');

      if (parsedHost && parsedPort) {
        return {
          host: parsedHost,
          port: parsedPort,
        };
      }
    } catch {
      // Fall back to the explicit bind target when the URL is malformed.
    }
  }

  return { host, port };
}

export async function waitForPortFree(host, port, {
  probePort: probePortImpl = probePort,
  sleep = delay,
  timeoutMs = DEFAULT_TIMEOUT_MS,
  pollIntervalMs = DEFAULT_POLL_INTERVAL_MS,
} = {}) {
  const deadline = Date.now() + timeoutMs;

  while (Date.now() <= deadline) {
    if (!await probePortImpl(host, port)) {
      return;
    }
    await sleep(pollIntervalMs);
  }

  throw new Error(`Timed out waiting for ${host}:${port} to become free`);
}

function stopSpawnedServers(startedServers) {
  for (const { child } of startedServers) {
    if (child.killed) {
      continue;
    }
    try {
      child.kill('SIGTERM');
    } catch {
      // Preserve the startup error that triggered rollback.
    }
  }
}

/**
 * Brings one server up at `url`, reusing or waiting for an existing one when allowed.
 *
 * `label` names the server in every readiness error so a failed Playwright startup
 * says which of the two processes never became ready.
 */
export async function ensureServer({
  label,
  url,
  host,
  port,
  allowReuse = true,
  timeoutMs = DEFAULT_TIMEOUT_MS,
  pollIntervalMs = DEFAULT_POLL_INTERVAL_MS,
  probeUrl: probeUrlImpl = probeUrl,
  probePort: probePortImpl = probePort,
  sleep = delay,
  acquireStartupLease: acquireStartupLeaseImpl = () => acquireStartupLease(buildStartupLeasePath(port)),
  spawnServer = () => spawnDashboardServer(),
}) {
  const waitForReady = () =>
    waitForUrlReady({
      label,
      url,
      timeoutMs,
      pollIntervalMs,
      probeUrl: probeUrlImpl,
      sleep,
    });

  if (allowReuse && await probeUrlImpl(url)) {
    return { mode: 'reuse', label };
  }

  let releaseStartupLease = null;

  if (allowReuse) {
    releaseStartupLease = await acquireStartupLeaseImpl();
    if (!releaseStartupLease) {
      await waitForReady();
      return { mode: 'wait', label };
    }
  }

  const releaseLease = async () => {
    if (releaseStartupLease) {
      const release = releaseStartupLease;
      releaseStartupLease = null;
      await release();
    }
  };

  if (allowReuse && await probePortImpl(host, port)) {
    try {
      await waitForReady();
      return { mode: 'wait', label };
    } finally {
      await releaseLease();
    }
  }

  let child;
  try {
    child = spawnServer();
    const childExit = new Promise((_, reject) => {
      child.once('error', reject);
      child.once('exit', (code, signal) => {
        reject(
          new Error(
            `${label} exited before becoming ready (code=${code ?? 'null'}, signal=${signal ?? 'null'})`,
          ),
        );
      });
    });

    await Promise.race([
      waitForReady(),
      childExit,
    ]);

    await releaseLease();
    return { mode: 'spawn', label, child };
  } catch (error) {
    if (child) {
      stopSpawnedServers([{ child }]);
    }
    await releaseLease();
    throw error;
  }
}

/**
 * Starts the backend, proves it is vector-enabled, then starts the dashboard.
 *
 * Ordering is load-bearing: the capability assertion runs before the dashboard so a
 * text-only backend aborts startup instead of producing a green all-skipped run.
 */
export async function startPlaywrightServers({
  backend,
  dashboard,
  readiness = {},
  ensureServer: ensureServerImpl = ensureServer,
  assertVectorSearchEnabled: assertVectorSearchEnabledImpl = assertVectorSearchEnabled,
}) {
  const startedServers = [];
  try {
    const backendResult = await ensureServerImpl({
      ...readiness,
      label: BACKEND_LABEL,
      url: `${backend.url}/health`,
      host: backend.host,
      port: backend.port,
      // A backend already listening on the shared port is reused rather than fought
      // over — unlike the dashboard, it holds seeded index state worth keeping. The
      // capability assertion below is what proves a reused backend is the right one.
      allowReuse: true,
      spawnServer: () => spawnBackendServer(backend),
    });
    if (backendResult.child) {
      startedServers.push({ label: backendResult.label, child: backendResult.child });
    }

    await assertVectorSearchEnabledImpl(backend.url, {
      allowTextOnlyNegativeControl: backend.allowTextOnlyNegativeControl ?? false,
    });

    const dashboardResult = await ensureServerImpl({
      ...readiness,
      label: DASHBOARD_LABEL,
      url: dashboard.url,
      host: dashboard.host,
      port: dashboard.port,
      allowReuse: dashboard.allowReuse,
      spawnServer: () => spawnDashboardServer(),
    });
    if (dashboardResult.child) {
      startedServers.push({ label: dashboardResult.label, child: dashboardResult.child });
    }

    return startedServers;
  } catch (error) {
    stopSpawnedServers(startedServers);
    throw error;
  }
}

/** Tears down every process this wrapper spawned when Playwright stops it. */
export function forwardShutdownSignals(startedServers, target = process) {
  const forward = (signal) => {
    if (signal === 'SIGTERM') {
      stopSpawnedServers(startedServers);
      return;
    }
    for (const { child } of startedServers) {
      if (!child.killed) child.kill(signal);
    }
  };

  target.once('SIGINT', () => forward('SIGINT'));
  target.once('SIGTERM', () => forward('SIGTERM'));
}

/** Resolves on clean exit, rejects on the first child that dies with a failure code. */
function waitForFirstChildExit(startedServers) {
  return Promise.race(startedServers.map(({ label, child }) => new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => {
      if (signal || code === 0) {
        resolve();
        return;
      }
      reject(new Error(`${label} exited with code ${code}`));
    });
  })));
}

function waitForShutdownSignal() {
  return new Promise((resolve) => {
    let settled = false;
    const keepAlive = setInterval(() => {}, 1_000);

    const finish = () => {
      if (settled) {
        return;
      }
      settled = true;
      clearInterval(keepAlive);
      resolve();
    };

    process.once('SIGINT', finish);
    process.once('SIGTERM', finish);
  });
}

async function runWaitForPortFree() {
  const {
    url,
    host,
    port,
    timeoutMs,
    pollIntervalMs,
  } = readWebServerEnv();
  const probeTarget = resolveWaitForPortFreeTarget({ url, host, port });

  if (!probeTarget.host || !probeTarget.port) {
    throw new Error('PLAYWRIGHT_WEBSERVER_HOST and PLAYWRIGHT_WEBSERVER_PORT are required');
  }

  await waitForPortFree(probeTarget.host, probeTarget.port, {
    timeoutMs,
    pollIntervalMs,
  });
}

async function run() {
  if (process.argv.includes('--wait-port-free')) {
    await runWaitForPortFree();
    return;
  }

  const { timeoutMs, pollIntervalMs, ...dashboard } = readWebServerEnv();
  const readiness = { timeoutMs, pollIntervalMs };
  const backend = readBackendEnv();

  if (!dashboard.url || !dashboard.host || !dashboard.port) {
    throw new Error(
      'PLAYWRIGHT_WEBSERVER_URL, PLAYWRIGHT_WEBSERVER_HOST, and PLAYWRIGHT_WEBSERVER_PORT are required',
    );
  }

  if (!backend.url || !backend.host || !backend.port) {
    throw new Error(
      'PLAYWRIGHT_BACKEND_URL, PLAYWRIGHT_BACKEND_HOST, and PLAYWRIGHT_BACKEND_PORT are required',
    );
  }

  const startedServers = await startPlaywrightServers({ backend, dashboard, readiness });

  if (startedServers.length === 0) {
    await waitForShutdownSignal();
    return;
  }

  forwardShutdownSignals(startedServers);
  await waitForFirstChildExit(startedServers);
}

const invokedAsScript =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;

if (invokedAsScript) {
  run().catch((error) => {
    console.error(error instanceof Error ? error.message : error);
    process.exit(1);
  });
}
