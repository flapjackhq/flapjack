import fs from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import process from 'node:process';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { fileURLToPath, pathToFileURL } from 'node:url';

/**
 * Single owner of "what environment makes a backend able to serve the e2e specs".
 *
 * The same file is read by engine/tests/test_dashboard_e2e_backend_contract.py, which
 * holds the CI workflows to it. Reading it here rather than restating the variables is
 * what keeps the two halves from drifting — and drift is precisely the defect this
 * contract was created for: on 2026-08-06 a nightly run and a prod CI run failed on two
 * different specs because two hands had configured two backends differently.
 */
const CONTRACT_PATH = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
  'tests',
  'e2e_backend_contract.json',
);
const ENV_ABSENT_PROPERTY = '__flapjackEnvAbsent';

/**
 * The union of the selected declared requirements' environment.
 *
 * `spawnBackendServer` starts ONE backend for whatever the developer runs, so it cannot
 * scope the environment to a spec selection the way a CI job can — it has to satisfy the
 * widest ordinary scope. Requirements that declare `env_absent` are intentionally
 * excluded from that default because they are negative preconditions for dedicated
 * split-lifecycle runs; those scripts select them explicitly through
 * PLAYWRIGHT_BACKEND_CONTRACT_IDS. A missing entry here surfaces as a spec-level failure
 * deep into a run (a `configureEmbedder` 400, or a cluster-status precondition throw),
 * never as a startup error, which is why the contract is data rather than a comment.
 */
function parseRequiredContractIds(rawValue) {
  if (!rawValue) {
    return null;
  }
  return rawValue.split(',').map((value) => value.trim()).filter(Boolean);
}

function selectBackendRequirements(requirements, requiredIds) {
  if (!requiredIds) {
    return requirements.filter((requirement) => !requirement.env_absent);
  }
  const byId = new Map(requirements.map((requirement) => [requirement.id, requirement]));
  return requiredIds.map((id) => {
    const requirement = byId.get(id);
    if (!requirement) {
      throw new Error(`Unknown dashboard e2e backend contract requirement: ${id}`);
    }
    return requirement;
  });
}

function buildBackendContractEnvironment(
  contractPath = CONTRACT_PATH,
  requiredIds = parseRequiredContractIds(process.env.PLAYWRIGHT_BACKEND_CONTRACT_IDS),
) {
  const contract = JSON.parse(readFileSync(contractPath, 'utf8'));
  const merged = {};
  const absent = new Set();
  for (const requirement of selectBackendRequirements(contract.requirements, requiredIds)) {
    for (const name of requirement.env_absent ?? []) {
      absent.add(name);
      delete merged[name];
    }
    Object.assign(merged, requirement.env ?? {});
  }
  Object.defineProperty(merged, ENV_ABSENT_PROPERTY, {
    value: [...absent],
    enumerable: false,
  });
  return merged;
}

export function backendContractEnv(
  contractPath = CONTRACT_PATH,
  requiredIds = parseRequiredContractIds(process.env.PLAYWRIGHT_BACKEND_CONTRACT_IDS),
) {
  return buildBackendContractEnvironment(contractPath, requiredIds);
}

function normalizeBackendContractEnvironment(contractEnvironment) {
  if (
    contractEnvironment
    && typeof contractEnvironment === 'object'
    && (
      Object.prototype.hasOwnProperty.call(contractEnvironment, 'env')
      || Object.prototype.hasOwnProperty.call(contractEnvironment, 'envAbsent')
    )
  ) {
    return {
      env: contractEnvironment.env ?? {},
      envAbsent: contractEnvironment.envAbsent ?? [],
    };
  }
  return {
    env: contractEnvironment ?? {},
    envAbsent: contractEnvironment?.[ENV_ABSENT_PROPERTY] ?? [],
  };
}

function applyBackendContractEnvironment(baseEnv, contractEnvironment) {
  const { env, envAbsent } = normalizeBackendContractEnvironment(contractEnvironment);
  const resolved = {
    ...baseEnv,
    ...env,
  };
  for (const name of envAbsent) {
    delete resolved[name];
  }
  return resolved;
}

const DEFAULT_TIMEOUT_MS = 120_000;
const DEFAULT_POLL_INTERVAL_MS = 250;
const PORT_PROBE_TIMEOUT_MS = 500;
const BACKEND_LABEL = 'backend server';
const DASHBOARD_LABEL = 'dashboard dev server';
/** Canonical dashboard-owned command that produces bin/flapjack-stable. */
const BACKEND_BUILD_COMMAND = 'npm run update-server';
/** Single owner of backend binary path, admin key, and secret-env loading. */
const BACKEND_START_SCRIPT = 'scripts/start-stable-server.sh';
const DEFAULT_DASHBOARD_ADMIN_KEY = 'fj_devtestadminkey000000';
/** Bind hosts a backend authenticated by the public dev key may be exposed on. */
const LOOPBACK_BIND_HOSTS = new Set(['127.0.0.1', 'localhost', '::1', '[::1]']);
const READINESS_EMBEDDER_NAME = 'dashboardE2eReadiness';
const READINESS_EMBEDDER_URL = 'http://127.0.0.1:9/embed';

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

function isAlreadyExists(error) {
  return error && typeof error === 'object' && 'code' in error && error.code === 'EEXIST';
}

function isProcessAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    // EPERM still proves a process owns the PID; only ESRCH proves that it is gone.
    return !(error && typeof error === 'object' && 'code' in error && error.code === 'ESRCH');
  }
}

async function readStartupLease(lockPath) {
  try {
    const metadata = JSON.parse(await fs.readFile(lockPath, 'utf8'));
    if (
      metadata?.version !== 1
      || !Number.isSafeInteger(metadata.pid)
      || metadata.pid <= 0
      || typeof metadata.hostname !== 'string'
      || !metadata.hostname
      || typeof metadata.token !== 'string'
      || !metadata.token
    ) {
      return null;
    }
    return metadata;
  } catch (error) {
    if (error && typeof error === 'object' && 'code' in error && error.code === 'ENOENT') {
      return undefined;
    }
    // A legacy empty lock or an interrupted write cannot prove its owner is dead.
    return null;
  }
}

function startupLeaseIsLive(metadata, hostname, checkProcessAlive) {
  // A shared temporary directory may expose another host's lease. Its PID namespace is
  // not ours, so inability to prove death must fail closed.
  return metadata.hostname !== hostname || checkProcessAlive(metadata.pid);
}

async function linkLease(candidatePath, targetPath) {
  try {
    await fs.link(candidatePath, targetPath);
    return true;
  } catch (error) {
    if (isAlreadyExists(error)) {
      return false;
    }
    throw error;
  }
}

async function unlinkOwnedLease(lockPath, token) {
  const current = await readStartupLease(lockPath);
  if (current?.token === token) {
    await fs.unlink(lockPath).catch((error) => {
      if (!(error && typeof error === 'object' && 'code' in error && error.code === 'ENOENT')) {
        throw error;
      }
    });
  }
}

/**
 * Atomically acquires the per-port startup lease.
 *
 * The old implementation left an empty `wx` file behind when its process died, which
 * gave successors no safe way to distinguish a live startup from a stale lock. The
 * published file is now a fully written hard link carrying same-host PID ownership.
 * A second hard-link guard serializes dead-owner reclamation, while the final link to
 * `lockPath` remains the single atomic election point for simultaneous contenders.
 */
export async function acquireStartupLease(lockPath, {
  pid = process.pid,
  hostname = os.hostname(),
  token = randomUUID(),
  isProcessAlive: checkProcessAlive = isProcessAlive,
} = {}) {
  const metadata = {
    version: 1,
    pid,
    hostname,
    token,
  };
  const candidatePath = `${lockPath}.${pid}.${token}.candidate`;
  const reclaimGuardPath = `${lockPath}.reclaim`;

  await fs.writeFile(candidatePath, `${JSON.stringify(metadata)}\n`, {
    flag: 'wx',
    mode: 0o600,
  });

  const release = async () => {
    await unlinkOwnedLease(lockPath, token);
  };

  try {
    for (let attempt = 0; attempt < 3; attempt += 1) {
      if (await linkLease(candidatePath, lockPath)) {
        return release;
      }

      const existing = await readStartupLease(lockPath);
      if (existing === undefined) {
        continue;
      }
      if (existing === null || startupLeaseIsLive(existing, hostname, checkProcessAlive)) {
        return null;
      }

      if (!await linkLease(candidatePath, reclaimGuardPath)) {
        return null;
      }

      try {
        // The owner may have released or another contender may have won between our
        // first read and guard acquisition. Re-read under the guard before unlinking.
        const guardedLease = await readStartupLease(lockPath);
        if (
          guardedLease?.token !== existing.token
          || startupLeaseIsLive(guardedLease, hostname, checkProcessAlive)
        ) {
          continue;
        }
        await fs.unlink(lockPath);

        // Keep the reclaim guard until this contender has either won the atomic link or
        // observed a winner. Callers that passed the guard check before it existed can
        // still contend safely at this link; exactly one candidate can become owner.
        if (await linkLease(candidatePath, lockPath)) {
          return release;
        }
      } finally {
        await unlinkOwnedLease(reclaimGuardPath, token);
      }
    }

    return null;
  } finally {
    await fs.unlink(candidatePath).catch(() => {});
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

export function readBackendEnv() {
  return {
    url: process.env.PLAYWRIGHT_BACKEND_URL,
    host: process.env.PLAYWRIGHT_BACKEND_HOST,
    port: parseInteger(process.env.PLAYWRIGHT_BACKEND_PORT, 0),
    dataDir: process.env.PLAYWRIGHT_BACKEND_DATA_DIR,
    // PLAYWRIGHT_BACKEND_ADMIN_KEY carries the key getLocalInstanceConfig() already
    // resolved — including one read from flapjack.local.conf or discovered from a reused
    // loopback backend's process line — which this plain-node script cannot re-derive
    // from the TypeScript resolver. It wins over the raw env fallbacks so a reused backend
    // with a custom key is authenticated as that key rather than the hardcoded default,
    // which would 403 the readiness probes before any spec runs. playwright.config.ts
    // forwards it ambiently (off-artifact); the raw chain covers standalone invocation.
    adminKey: process.env.PLAYWRIGHT_BACKEND_ADMIN_KEY
      ?? process.env.FJ_TEST_ADMIN_KEY
      ?? process.env.FLAPJACK_ADMIN_KEY
      ?? DEFAULT_DASHBOARD_ADMIN_KEY,
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

function dashboardApiHeaders(adminKey) {
  return {
    'x-algolia-application-id': 'flapjack',
    'x-algolia-api-key': adminKey,
    'Content-Type': 'application/json',
  };
}

async function responseBody(response) {
  try {
    return await response.text();
  } catch {
    return '<unreadable response body>';
  }
}

function withCleanupContext(primaryError, cleanupError) {
  const cleanupMessage = cleanupError instanceof Error ? cleanupError.message : String(cleanupError);
  if (primaryError instanceof Error) {
    primaryError.message = `${primaryError.message} Cleanup also failed: ${cleanupMessage}`;
    return primaryError;
  }
  return new Error(`${String(primaryError)} Cleanup also failed: ${cleanupMessage}`);
}

async function assertVectorSearchBuild(backendBaseUrl, fetchImpl, allowTextOnlyNegativeControl) {
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

async function probeLocalOutboundOptIn(backendBaseUrl, fetchImpl, adminKey, indexName) {
  const headers = dashboardApiHeaders(adminKey);
  const encodedIndexName = encodeURIComponent(indexName);
  const indexUrl = `${backendBaseUrl}/1/indexes/${encodedIndexName}`;
  const createResponse = await fetchImpl(`${backendBaseUrl}/1/indexes`, {
    method: 'POST',
    headers,
    body: JSON.stringify({ uid: indexName }),
  });
  if (!createResponse.ok) {
    throw new Error(
      `Backend readiness index creation failed (${createResponse.status}): `
      + await responseBody(createResponse),
    );
  }

  let probeFailure = null;
  try {
    const settingsResponse = await fetchImpl(`${indexUrl}/settings`, {
      method: 'PUT',
      headers,
      body: JSON.stringify({
        embedders: {
          [READINESS_EMBEDDER_NAME]: {
            source: 'rest',
            url: READINESS_EMBEDDER_URL,
            dimensions: 3,
            request: { input: '{{text}}' },
            response: { embedding: '{{embedding}}' },
          },
        },
      }),
    });
    if (!settingsResponse.ok) {
      const refusal = await responseBody(settingsResponse);
      if (refusal.includes('private or local destination')) {
        throw new Error(
          `Backend at ${backendBaseUrl} refused the dashboard local-URL embedder probe. `
          + 'Set FLAPJACK_AI_ALLOW_LOCAL_URLS=1 and restart the reused backend, or '
          + 'stop it so Playwright can start a configured backend.',
        );
      }
      throw new Error(
        `Backend local-URL readiness probe failed (${settingsResponse.status}): ${refusal}`,
      );
    }
  } catch (error) {
    probeFailure = error;
  } finally {
    let cleanupFailure = null;
    try {
      const cleanupResponse = await fetchImpl(indexUrl, { method: 'DELETE', headers });
      if (!cleanupResponse.ok) {
        cleanupFailure = new Error(
          `Backend readiness index cleanup failed (${cleanupResponse.status}): `
          + await responseBody(cleanupResponse),
        );
      }
    } catch (error) {
      cleanupFailure = error;
    }

    if (cleanupFailure) {
      if (probeFailure) {
        throw withCleanupContext(probeFailure, cleanupFailure);
      }
      throw cleanupFailure;
    }
  }

  if (probeFailure) {
    throw probeFailure;
  }
}

async function assertReplicationEnabled(backendBaseUrl, fetchImpl, adminKey) {
  const statusUrl = `${backendBaseUrl}/internal/cluster/status`;
  const response = await fetchImpl(statusUrl, {
    method: 'GET',
    headers: dashboardApiHeaders(adminKey),
  });
  if (!response.ok) {
    throw new Error(`Backend cluster-status probe failed (${response.status}) at ${statusUrl}`);
  }

  const status = await response.json();
  if (status?.replication_enabled !== true) {
    throw new Error(
      `Backend at ${backendBaseUrl} reports replication_enabled=${status?.replication_enabled}. `
      + 'Dashboard cluster-peer e2e requires replication. Set FLAPJACK_NODE_ID, '
      + 'FLAPJACK_ADVERTISE_ADDR, and FLAPJACK_REPLICATION_API_KEY before starting '
      + 'the backend.',
    );
  }
}

/** Proves every runtime capability required by the dashboard e2e backend contract. */
export async function assertBackendReadiness(backendBaseUrl, {
  fetchImpl = fetch,
  adminKey = DEFAULT_DASHBOARD_ADMIN_KEY,
  allowTextOnlyNegativeControl = false,
  readinessIndexName = `dashboard-e2e-readiness-${process.pid}-${Date.now()}`,
} = {}) {
  const capabilities = await assertVectorSearchBuild(
    backendBaseUrl,
    fetchImpl,
    allowTextOnlyNegativeControl,
  );
  if (!allowTextOnlyNegativeControl) {
    await probeLocalOutboundOptIn(backendBaseUrl, fetchImpl, adminKey, readinessIndexName);
  }
  await assertReplicationEnabled(backendBaseUrl, fetchImpl, adminKey);
  return capabilities;
}

/**
 * Refuses to publish a backend authenticated by a credential checked into this repo.
 *
 * This wrapper now pins the spawned backend's FLAPJACK_ADMIN_KEY (and the contract pins
 * FLAPJACK_REPLICATION_API_KEY) before scripts/start-stable-server.sh can consult
 * engine/.secret/.env.secret, so it owns the exposure decision that the shell script used
 * to. `DEFAULT_DASHBOARD_ADMIN_KEY` is a public constant, harmless on loopback and an
 * open admin API on anything routable. local-instance-config.ts::resolveAdminKey already
 * throws for a non-loopback backend without an explicit key; this is the same fail-closed
 * rule on the plain-node path, which readBackendEnv reaches on standalone invocation
 * where that resolver never runs.
 */
function assertBindTargetSafeForAdminKey(host, adminKey) {
  // An omitted key is not the safe case: start-stable-server.sh falls back to the
  // identical constant unless the ambient environment already carries a key.
  const effectiveKey = adminKey ?? process.env.FLAPJACK_ADMIN_KEY ?? DEFAULT_DASHBOARD_ADMIN_KEY;

  if (effectiveKey !== DEFAULT_DASHBOARD_ADMIN_KEY || LOOPBACK_BIND_HOSTS.has(host)) {
    return;
  }

  throw new Error(
    `Refusing to bind the e2e backend to ${host} while authenticating with the public `
    + 'dashboard dev admin key. That key is checked into this repository, so the backend '
    + 'would accept admin writes from anything that can route to the bind address. Bind to '
    + '127.0.0.1, or set FJ_TEST_ADMIN_KEY (or FLAPJACK_ADMIN_KEY) to a key that is not the '
    + 'shared default.',
  );
}

/**
 * Starts the backend through its existing owner, pinned to the Playwright target.
 *
 * The capability environment comes from the declared contract rather than from literals
 * here. Before that, this function set `FLAPJACK_AI_ALLOW_LOCAL_URLS` and nothing else,
 * so a local `npm run test:e2e-ui:full` failed `cluster_peers.spec.ts` for exactly the
 * reason the 2026-08-06 nightly did — the replication identity was never set.
 */
export function spawnBackendServer(
  { host, port, dataDir, adminKey },
  spawnImpl = spawn,
  contractEnv = backendContractEnv(),
) {
  assertBindTargetSafeForAdminKey(host, adminKey);

  return spawnImpl('bash', [BACKEND_START_SCRIPT], {
    stdio: 'inherit',
    env: {
      ...applyBackendContractEnvironment(process.env, contractEnv),
      ...(adminKey !== undefined ? { FLAPJACK_ADMIN_KEY: adminKey } : {}),
      // The bind target is the run's own, not the contract's, so it stays last.
      FLAPJACK_BIND_ADDR: `${host}:${port}`,
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
  requireFreshProcess = false,
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

  if (await probeUrlImpl(url)) {
    if (requireFreshProcess) {
      throw new Error(
        `${label} at ${url} is already running, but this backend contract requires `
        + 'a freshly spawned backend server. Stop the existing backend and retry.',
      );
    }
    if (allowReuse) {
      return { mode: 'reuse', label };
    }
  }

  if (requireFreshProcess && await probePortImpl(host, port)) {
    throw new Error(
      `${label} port ${host}:${port} is already in use, but this backend contract requires `
      + 'a freshly spawned backend server. Free the port and retry.',
    );
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
 * Starts the backend, proves every suite capability, then starts the dashboard.
 *
 * Ordering is load-bearing: each readiness check corresponds to a prior real red or
 * skip-hiding failure, and all must abort before the dashboard lets any spec start.
 */
export async function startPlaywrightServers({
  backend,
  dashboard,
  readiness = {},
  backendContractEnvironment = backendContractEnv(),
  ensureServer: ensureServerImpl = ensureServer,
  assertBackendReadiness: assertBackendReadinessImpl = assertBackendReadiness,
  spawnBackendServer: spawnBackendServerImpl = spawnBackendServer,
}) {
  assertBindTargetSafeForAdminKey(backend.host, backend.adminKey);

  const startedServers = [];
  const { envAbsent } = normalizeBackendContractEnvironment(backendContractEnvironment);
  const requireFreshBackend = envAbsent.length > 0;
  try {
    const backendResult = await ensureServerImpl({
      ...readiness,
      label: BACKEND_LABEL,
      url: `${backend.url}/health`,
      host: backend.host,
      port: backend.port,
      // Ordinary runs preserve seeded state by reusing a capable backend. A negative
      // process-start contract cannot be proved on an ambient process, so those runs
      // require an unoccupied port and a backend spawned with the selected contract.
      allowReuse: !requireFreshBackend,
      requireFreshProcess: requireFreshBackend,
      spawnServer: () => spawnBackendServerImpl(backend, spawn, backendContractEnvironment),
    });
    if (backendResult.child) {
      startedServers.push({ label: backendResult.label, child: backendResult.child });
    }

    await assertBackendReadinessImpl(backend.url, {
      allowTextOnlyNegativeControl: backend.allowTextOnlyNegativeControl ?? false,
      adminKey: backend.adminKey,
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
