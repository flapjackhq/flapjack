/* @vitest-environment node */
import { afterEach, describe, expect, it, vi } from 'vitest'

import { readFileSync } from 'node:fs'
import fs from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import packageJson from './package.json'
import playwrightConfig from './playwright.config'

import {
  assertBackendReadiness,
  acquireStartupLease,
  backendContractEnv,
  ensureServer,
  forwardShutdownSignals,
  readBackendEnv,
  resolveWaitForPortFreeTarget,
  spawnBackendServer,
  startPlaywrightServers,
  waitForPortFree,
} from './scripts/playwright-webserver.mjs'

// Read the declared contract independently of the implementation. Restating the
// expected variables here instead would make these assertions a copy of the code they
// check: adding a requirement to the contract would leave them green while the backend
// silently stopped satisfying it, which is the exact drift the contract exists to stop.
const CONTRACT_PATH = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  'tests',
  'e2e_backend_contract.json',
)
const CONTRACT = JSON.parse(readFileSync(CONTRACT_PATH, 'utf8')) as {
  requirements: Array<{
    id: string;
    env?: Record<string, string>;
    env_absent?: string[];
    symptom_if_missing: string;
  }>
  npm_script_spec_scopes: {
    playwright_entry_points: string[];
  };
}

afterEach(() => {
  vi.unstubAllEnvs()
})

async function createLeaseFixture() {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'flapjack-playwright-lease-test-'))
  return {
    lockPath: path.join(directory, 'startup.lock'),
    cleanup: () => fs.rm(directory, { recursive: true, force: true }),
  }
}

function createFakeChild() {
  return {
    once: vi.fn(),
    on: vi.fn(),
    kill: vi.fn(),
    killed: false,
    stdout: null,
    stderr: null,
  }
}

function healthResponse(capabilities: Record<string, unknown>) {
  return {
    ok: true,
    status: 200,
    json: async () => ({ status: 'ok', capabilities }),
  }
}

function jsonResponse(status: number, body: Record<string, unknown>) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
    text: async () => JSON.stringify(body),
  }
}

describe('acquireStartupLease', () => {
  it('publishes owner metadata and leaves a live owner in control', async () => {
    const fixture = await createLeaseFixture()
    try {
      const release = await acquireStartupLease(fixture.lockPath, {
        pid: 41,
        hostname: 'test-host',
        token: 'live-owner',
        isProcessAlive: vi.fn().mockReturnValue(true),
      })

      expect(release).toBeTypeOf('function')
      await expect(fs.readFile(fixture.lockPath, 'utf8')).resolves.toBe(
        `${JSON.stringify({
          version: 1,
          pid: 41,
          hostname: 'test-host',
          token: 'live-owner',
        })}\n`,
      )

      await expect(acquireStartupLease(fixture.lockPath, {
        pid: 42,
        hostname: 'test-host',
        token: 'contender',
        isProcessAlive: vi.fn().mockImplementation((pid) => pid === 41),
      })).resolves.toBeNull()

      await release?.()
      await expect(fs.stat(fixture.lockPath)).rejects.toMatchObject({ code: 'ENOENT' })
    } finally {
      await fixture.cleanup()
    }
  })

  it('reclaims a lease whose same-host owner is dead', async () => {
    const fixture = await createLeaseFixture()
    try {
      await fs.writeFile(fixture.lockPath, `${JSON.stringify({
        version: 1,
        pid: 41,
        hostname: 'test-host',
        token: 'dead-owner',
      })}\n`)

      const release = await acquireStartupLease(fixture.lockPath, {
        pid: 42,
        hostname: 'test-host',
        token: 'replacement-owner',
        isProcessAlive: vi.fn().mockImplementation((pid) => pid === 42),
      })

      expect(release).toBeTypeOf('function')
      await expect(fs.readFile(fixture.lockPath, 'utf8')).resolves.toContain(
        '"token":"replacement-owner"',
      )
      await release?.()
    } finally {
      await fixture.cleanup()
    }
  })

  it('elects only one owner when contenders race to reclaim a dead lease', async () => {
    const fixture = await createLeaseFixture()
    try {
      await fs.writeFile(fixture.lockPath, `${JSON.stringify({
        version: 1,
        pid: 40,
        hostname: 'test-host',
        token: 'dead-owner',
      })}\n`)

      const contenders = await Promise.all([
        acquireStartupLease(fixture.lockPath, {
          pid: 41,
          hostname: 'test-host',
          token: 'contender-a',
          isProcessAlive: (pid) => pid !== 40,
        }),
        acquireStartupLease(fixture.lockPath, {
          pid: 42,
          hostname: 'test-host',
          token: 'contender-b',
          isProcessAlive: (pid) => pid !== 40,
        }),
      ])

      const owners = contenders.filter((release) => release !== null)
      expect(owners).toHaveLength(1)
      const metadata = JSON.parse(await fs.readFile(fixture.lockPath, 'utf8'))
      expect(['contender-a', 'contender-b']).toContain(metadata.token)

      await owners[0]?.()
    } finally {
      await fixture.cleanup()
    }
  })
})

describe('ensureServer', () => {
  it('reuses an already healthy server without spawning', async () => {
    const spawnServer = vi.fn()
    const probeUrl = vi.fn().mockResolvedValue(true)
    const probePort = vi.fn()

    const result = await ensureServer({
      label: 'dashboard dev server',
      url: 'http://127.0.0.1:5177',
      host: '127.0.0.1',
      port: 5177,
      probeUrl,
      probePort,
      spawnServer,
      sleep: vi.fn(),
      timeoutMs: 100,
      pollIntervalMs: 1,
    })

    expect(result.mode).toBe('reuse')
    expect(spawnServer).not.toHaveBeenCalled()
    expect(probePort).not.toHaveBeenCalled()
  })

  it('rejects a healthy reused server when a fresh process is required', async () => {
    const spawnServer = vi.fn()

    await expect(ensureServer({
      label: 'backend server',
      url: 'http://127.0.0.1:7711/health',
      host: '127.0.0.1',
      port: 7711,
      requireFreshProcess: true,
      probeUrl: vi.fn().mockResolvedValue(true),
      probePort: vi.fn(),
      spawnServer,
      sleep: vi.fn(),
      timeoutMs: 100,
      pollIntervalMs: 1,
    })).rejects.toThrow(/requires a freshly spawned backend server/)

    expect(spawnServer).not.toHaveBeenCalled()
  })

  it('waits for an in-progress startup when the port is already bound', async () => {
    const spawnServer = vi.fn()
    const probeUrl = vi.fn()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true)
    const probePort = vi.fn().mockResolvedValue(true)
    const sleep = vi.fn().mockResolvedValue(undefined)

    const acquireStartupLease = vi.fn().mockResolvedValue(vi.fn())

    const result = await ensureServer({
      label: 'dashboard dev server',
      url: 'http://127.0.0.1:5177',
      host: '127.0.0.1',
      port: 5177,
      probeUrl,
      probePort,
      spawnServer,
      sleep,
      acquireStartupLease,
      timeoutMs: 100,
      pollIntervalMs: 1,
    })

    expect(result.mode).toBe('wait')
    expect(spawnServer).not.toHaveBeenCalled()
    expect(probePort).toHaveBeenCalledWith('127.0.0.1', 5177)
    expect(sleep).toHaveBeenCalled()
  })

  it('waits for another wrapper process that already owns startup', async () => {
    const spawnServer = vi.fn()
    const probeUrl = vi.fn()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true)
    const sleep = vi.fn().mockResolvedValue(undefined)
    const acquireStartupLease = vi.fn().mockResolvedValue(null)

    const result = await ensureServer({
      label: 'dashboard dev server',
      url: 'http://127.0.0.1:5177',
      host: '127.0.0.1',
      port: 5177,
      probeUrl,
      probePort: vi.fn().mockResolvedValue(false),
      spawnServer,
      sleep,
      acquireStartupLease,
      timeoutMs: 100,
      pollIntervalMs: 1,
    })

    expect(result.mode).toBe('wait')
    expect(acquireStartupLease).toHaveBeenCalledTimes(1)
    expect(spawnServer).not.toHaveBeenCalled()
    expect(sleep).toHaveBeenCalled()
  })

  it('spawns a new server when the port is free', async () => {
    const child = createFakeChild()
    const spawnServer = vi.fn().mockReturnValue(child)
    const probeUrl = vi.fn()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true)
    const probePort = vi.fn().mockResolvedValue(false)

    const acquireStartupLease = vi.fn().mockResolvedValue(vi.fn())

    const result = await ensureServer({
      label: 'dashboard dev server',
      url: 'http://127.0.0.1:5177',
      host: '127.0.0.1',
      port: 5177,
      probeUrl,
      probePort,
      spawnServer,
      sleep: vi.fn().mockResolvedValue(undefined),
      acquireStartupLease,
      timeoutMs: 100,
      pollIntervalMs: 1,
    })

    expect(result).toEqual({
      mode: 'spawn',
      label: 'dashboard dev server',
      child,
    })
    expect(spawnServer).toHaveBeenCalledTimes(1)
  })

  it('names the failing server in readiness-timeout errors', async () => {
    const child = createFakeChild()

    await expect(ensureServer({
      label: 'backend server',
      url: 'http://127.0.0.1:7700/health',
      host: '127.0.0.1',
      port: 7700,
      allowReuse: false,
      probeUrl: vi.fn().mockResolvedValue(false),
      probePort: vi.fn().mockResolvedValue(false),
      spawnServer: vi.fn().mockReturnValue(child),
      sleep: vi.fn().mockResolvedValue(undefined),
      timeoutMs: 5,
      pollIntervalMs: 1,
    })).rejects.toThrow('Timed out waiting for backend server at http://127.0.0.1:7700/health')

    expect(child.kill).toHaveBeenCalledWith('SIGTERM')
  })
})

describe('assertBackendReadiness', () => {
  it('proves vector build, local outbound opt-in, and replication without leaving probe state', async () => {
    const fetchImpl = vi.fn()
      .mockResolvedValueOnce(healthResponse({ vectorSearch: true, vectorSearchLocal: true }))
      .mockResolvedValueOnce(jsonResponse(200, { uid: 'dashboard-e2e-readiness-test' }))
      .mockResolvedValueOnce(jsonResponse(200, { taskID: 1 }))
      .mockResolvedValueOnce(jsonResponse(200, { taskID: 2 }))
      .mockResolvedValueOnce(jsonResponse(200, {
        node_id: 'dashboard-e2e',
        replication_enabled: true,
      }))

    await expect(assertBackendReadiness('http://127.0.0.1:7700', {
      fetchImpl,
      adminKey: 'test-admin-key',
      readinessIndexName: 'dashboard-e2e-readiness-test',
    })).resolves.toEqual({ vectorSearch: true, vectorSearchLocal: true })

    expect(fetchImpl.mock.calls.map(([url]) => url)).toEqual([
      'http://127.0.0.1:7700/health',
      'http://127.0.0.1:7700/1/indexes',
      'http://127.0.0.1:7700/1/indexes/dashboard-e2e-readiness-test/settings',
      'http://127.0.0.1:7700/1/indexes/dashboard-e2e-readiness-test',
      'http://127.0.0.1:7700/internal/cluster/status',
    ])
    expect(fetchImpl.mock.calls[2][1]).toMatchObject({
      method: 'PUT',
      headers: {
        'x-algolia-application-id': 'flapjack',
        'x-algolia-api-key': 'test-admin-key',
        'Content-Type': 'application/json',
      },
    })
    expect(JSON.parse(fetchImpl.mock.calls[2][1].body)).toEqual({
      embedders: {
        dashboardE2eReadiness: {
          source: 'rest',
          url: 'http://127.0.0.1:9/embed',
          dimensions: 3,
          request: { input: '{{text}}' },
          response: { embedding: '{{embedding}}' },
        },
      },
    })
    expect(fetchImpl.mock.calls[4][1]).toEqual({
      method: 'GET',
      headers: {
        'x-algolia-application-id': 'flapjack',
        'x-algolia-api-key': 'test-admin-key',
        'Content-Type': 'application/json',
      },
    })
  })

  it('fails loudly on a text-only backend instead of letting specs mass-skip', async () => {
    // Without this gate a text-only backend produces a fully green run whose specs
    // were silently skipped by skipWhenVectorSearchDisabled — the exact failure this
    // startup contract exists to prevent. The message must name the rebuild command
    // so the operator is not left guessing which binary is wrong.
    const fetchImpl = vi.fn().mockResolvedValue(
      healthResponse({ vectorSearch: false, vectorSearchLocal: false }),
    )

    await expect(assertBackendReadiness('http://127.0.0.1:7700', { fetchImpl }))
      .rejects.toThrow(/vectorSearch=false[\s\S]*npm run update-server/)
  })

  it('allows the vector checks only for the explicit P20 negative control', async () => {
    const fetchImpl = vi.fn()
      .mockResolvedValueOnce(healthResponse({ vectorSearch: false, vectorSearchLocal: false }))
      .mockResolvedValueOnce(jsonResponse(200, {
        node_id: 'dashboard-e2e',
        replication_enabled: true,
      }))

    await expect(assertBackendReadiness('http://127.0.0.1:7700', {
      fetchImpl,
      allowTextOnlyNegativeControl: true,
    })).resolves.toEqual({ vectorSearch: false, vectorSearchLocal: false })
  })

  it('refuses a backend without the local outbound URL runtime opt-in and cleans up', async () => {
    const fetchImpl = vi.fn()
      .mockResolvedValueOnce(healthResponse({ vectorSearch: true, vectorSearchLocal: true }))
      .mockResolvedValueOnce(jsonResponse(200, { uid: 'dashboard-e2e-readiness-local' }))
      .mockResolvedValueOnce(jsonResponse(400, {
        message: 'rest embedder URL private or local destination `127.0.0.1` is not allowed',
      }))
      .mockResolvedValueOnce(jsonResponse(200, { taskID: 1 }))

    await expect(assertBackendReadiness('http://127.0.0.1:7700', {
      fetchImpl,
      readinessIndexName: 'dashboard-e2e-readiness-local',
    })).rejects.toThrow(
      /FLAPJACK_AI_ALLOW_LOCAL_URLS=1[\s\S]*restart the reused backend[\s\S]*stop it so Playwright can start a configured backend/,
    )
    expect(fetchImpl).toHaveBeenLastCalledWith(
      'http://127.0.0.1:7700/1/indexes/dashboard-e2e-readiness-local',
      expect.objectContaining({ method: 'DELETE' }),
    )
  })

  it('preserves the local-URL refusal when readiness cleanup fails afterward', async () => {
    const fetchImpl = vi.fn()
      .mockResolvedValueOnce(healthResponse({ vectorSearch: true, vectorSearchLocal: true }))
      .mockResolvedValueOnce(jsonResponse(200, { uid: 'dashboard-e2e-readiness-local' }))
      .mockResolvedValueOnce(jsonResponse(400, {
        message: 'rest embedder URL private or local destination `127.0.0.1` is not allowed',
      }))
      .mockResolvedValueOnce(jsonResponse(500, { message: 'cleanup failed' }))

    await expect(assertBackendReadiness('http://127.0.0.1:7700', {
      fetchImpl,
      readinessIndexName: 'dashboard-e2e-readiness-local',
    })).rejects.toThrow(
      /refused the dashboard local-URL embedder probe[\s\S]*Cleanup also failed: Backend readiness index cleanup failed \(500\): {"message":"cleanup failed"}/,
    )
  })

  it('refuses a standalone backend and names every replication startup variable', async () => {
    const fetchImpl = vi.fn()
      .mockResolvedValueOnce(healthResponse({ vectorSearch: true, vectorSearchLocal: true }))
      .mockResolvedValueOnce(jsonResponse(200, { uid: 'dashboard-e2e-readiness-replication' }))
      .mockResolvedValueOnce(jsonResponse(200, { taskID: 1 }))
      .mockResolvedValueOnce(jsonResponse(200, { taskID: 2 }))
      .mockResolvedValueOnce(jsonResponse(200, {
        node_id: 'standalone',
        replication_enabled: false,
      }))

    await expect(assertBackendReadiness('http://127.0.0.1:7700', {
      fetchImpl,
      readinessIndexName: 'dashboard-e2e-readiness-replication',
    })).rejects.toThrow(
      /replication_enabled=false[\s\S]*FLAPJACK_NODE_ID[\s\S]*FLAPJACK_ADVERTISE_ADDR[\s\S]*FLAPJACK_REPLICATION_API_KEY/,
    )
  })

  it('rejects when the health probe itself is not readable', async () => {
    const fetchImpl = vi.fn().mockResolvedValue({
      ok: false,
      status: 503,
      json: async () => ({}),
    })

    await expect(assertBackendReadiness('http://127.0.0.1:7700', { fetchImpl }))
      .rejects.toThrow('Backend health probe failed (503) at http://127.0.0.1:7700/health')
  })
})

describe('spawnBackendServer', () => {
  it('starts the shared stable-server owner with the Playwright bind target and data dir', () => {
    const child = createFakeChild()
    const spawnImpl = vi.fn().mockReturnValue(child)

    const spawned = spawnBackendServer({
      host: '127.0.0.1',
      port: 7711,
      dataDir: '/tmp/flapjack/test-data',
    }, spawnImpl)

    expect(spawned).toBe(child)
    expect(spawnImpl).toHaveBeenCalledTimes(1)
    const [command, args, options] = spawnImpl.mock.calls[0] as [
      string,
      string[],
      { env: Record<string, string>; stdio: string },
    ]
    expect(command).toBe('bash')
    expect(args).toEqual(['scripts/start-stable-server.sh'])
    expect(options.env.FLAPJACK_BIND_ADDR).toBe('127.0.0.1:7711')
    expect(options.env.FLAPJACK_DATA_DIR).toBe('/tmp/flapjack/test-data')
  })

  it('passes the canonical resolved admin key to the spawned backend', () => {
    const spawnImpl = vi.fn().mockReturnValue(createFakeChild())

    spawnBackendServer({
      host: '127.0.0.1',
      port: 7711,
      dataDir: '/tmp/flapjack/test-data',
      adminKey: 'fj_file_custom',
    }, spawnImpl, {
      FLAPJACK_ADMIN_KEY: 'fj_contract_default',
    })

    const [, , options] = spawnImpl.mock.calls[0] as [
      string,
      string[],
      { env: Record<string, string | undefined> },
    ]
    expect(options.env.FLAPJACK_ADMIN_KEY).toBe('fj_file_custom')
  })

  it('leaves the data dir to the stable-server owner when Playwright does not pin one', () => {
    const spawnImpl = vi.fn().mockReturnValue(createFakeChild())

    spawnBackendServer({ host: '127.0.0.1', port: 7711 }, spawnImpl)

    const [, , options] = spawnImpl.mock.calls[0] as [
      string,
      string[],
      { env: Record<string, string | undefined> },
    ]
    expect(options.env.FLAPJACK_DATA_DIR).toBeUndefined()
  })

  // The locally-spawned backend serves whatever a developer runs, including
  // `npm run test:e2e-ui:full`, so it has to satisfy every declared requirement rather
  // than the subset a given CI job needs. Before the contract existed this function set
  // FLAPJACK_AI_ALLOW_LOCAL_URLS and nothing else, so a local full run failed
  // cluster_peers.spec.ts for the same reason the 2026-08-06 nightly did.
  it.each(CONTRACT.requirements.filter((requirement) => requirement.env))(
    'satisfies the declared "$id" backend requirement',
    ({ env, symptom_if_missing: symptom }) => {
      const spawnImpl = vi.fn().mockReturnValue(createFakeChild())

      spawnBackendServer({ host: '127.0.0.1', port: 7711 }, spawnImpl)

      const [, , options] = spawnImpl.mock.calls[0] as [
        string,
        string[],
        { env: Record<string, string | undefined> },
      ]
      for (const [name, value] of Object.entries(env)) {
        expect(options.env[name], `missing ${name} produces: ${symptom}`).toBe(value)
      }
    },
  )

  // The wrapper is now the owner of the spawned backend's credential: it pins
  // FLAPJACK_ADMIN_KEY (and the contract pins FLAPJACK_REPLICATION_API_KEY) before
  // start-stable-server.sh can consult engine/.secret/.env.secret. `fj_devtestadminkey000000`
  // is a public constant checked into this repo, so pinning it onto a listener that is not
  // loopback publishes an admin-authenticated backend to every host that can route to it.
  // local-instance-config.ts::resolveAdminKey already refuses that combination for the
  // TypeScript path; this is the same fail-closed rule on the plain-node path, which
  // readBackendEnv reaches whenever the wrapper is invoked standalone.
  it.each(['0.0.0.0', '::', '192.168.1.20'])(
    'refuses to bind %s while authenticating with the public dev admin key',
    (host) => {
      const spawnImpl = vi.fn().mockReturnValue(createFakeChild())

      expect(() => spawnBackendServer({
        host,
        port: 7711,
        adminKey: 'fj_devtestadminkey000000',
      }, spawnImpl)).toThrow(/public dashboard dev admin key/)
      expect(spawnImpl).not.toHaveBeenCalled()
    },
  )

  it('refuses a non-loopback bind that would fall through to the same public default', () => {
    // An omitted adminKey is not a safer case: start-stable-server.sh defaults to the
    // identical constant, so the listener ends up admin-authenticated by a public value.
    const spawnImpl = vi.fn().mockReturnValue(createFakeChild())
    const savedAdminKey = process.env.FLAPJACK_ADMIN_KEY
    delete process.env.FLAPJACK_ADMIN_KEY

    try {
      expect(() => spawnBackendServer({ host: '0.0.0.0', port: 7711 }, spawnImpl))
        .toThrow(/public dashboard dev admin key/)
      expect(spawnImpl).not.toHaveBeenCalled()
    } finally {
      if (savedAdminKey === undefined) {
        delete process.env.FLAPJACK_ADMIN_KEY
      } else {
        process.env.FLAPJACK_ADMIN_KEY = savedAdminKey
      }
    }
  })

  it('allows a non-loopback bind once a non-public admin key is supplied', () => {
    const spawnImpl = vi.fn().mockReturnValue(createFakeChild())

    spawnBackendServer({ host: '0.0.0.0', port: 7711, adminKey: 'fj_operator_supplied_key' }, spawnImpl)

    const [, , options] = spawnImpl.mock.calls[0] as [
      string,
      string[],
      { env: Record<string, string | undefined> },
    ]
    expect(options.env.FLAPJACK_BIND_ADDR).toBe('0.0.0.0:7711')
    expect(options.env.FLAPJACK_ADMIN_KEY).toBe('fj_operator_supplied_key')
  })

  it.each(['127.0.0.1', 'localhost', '::1', '[::1]'])(
    'still starts a loopback backend on %s with the public dev default',
    (host) => {
      const spawnImpl = vi.fn().mockReturnValue(createFakeChild())

      spawnBackendServer({ host, port: 7711, adminKey: 'fj_devtestadminkey000000' }, spawnImpl)

      expect(spawnImpl).toHaveBeenCalledTimes(1)
    },
  )

  it('lets the run\'s own bind target win over the contract', () => {
    // FLAPJACK_BIND_ADDR is per-run, not a capability, so a contract that ever named it
    // must not override the port Playwright is actually waiting on — that would hang
    // startup with no useful error.
    const spawnImpl = vi.fn().mockReturnValue(createFakeChild())

    spawnBackendServer({ host: '127.0.0.1', port: 17700 }, spawnImpl, {
      FLAPJACK_BIND_ADDR: '127.0.0.1:7700',
      FLAPJACK_NODE_ID: 'from-contract',
    })

    const [, , options] = spawnImpl.mock.calls[0] as [
      string,
      string[],
      { env: Record<string, string | undefined> },
    ]
    expect(options.env.FLAPJACK_BIND_ADDR).toBe('127.0.0.1:17700')
    expect(options.env.FLAPJACK_NODE_ID).toBe('from-contract')
  })

  it('removes contract-declared absent variables from the spawned backend environment', () => {
    vi.stubEnv('FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK', '1')
    const spawnImpl = vi.fn().mockReturnValue(createFakeChild())

    spawnBackendServer(
      { host: '127.0.0.1', port: 17700 },
      spawnImpl,
      backendContractEnv(CONTRACT_PATH, [
        'replication',
        'ai_local_outbound',
        'meilisearch_loopback_refusal',
      ]),
    )

    const [, , options] = spawnImpl.mock.calls[0] as [
      string,
      string[],
      { env: Record<string, string | undefined> },
    ]
    expect(options.env.FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK).toBeUndefined()
  })
})

describe('backendContractEnv', () => {
  it('merges every declared requirement into one environment', () => {
    const merged = backendContractEnv()

    for (const requirement of CONTRACT.requirements) {
      for (const [name, value] of Object.entries(requirement.env ?? {})) {
        expect(merged[name]).toBe(value)
      }
    }
  })

  it('can run the Meilisearch refusal project without preview loopback capability env', () => {
    const merged = backendContractEnv(CONTRACT_PATH, [
      'replication',
      'ai_local_outbound',
      'meilisearch_loopback_refusal',
    ])

    expect(merged.FLAPJACK_NODE_ID).toBe('dashboard-e2e')
    expect(merged.FLAPJACK_AI_ALLOW_LOCAL_URLS).toBe('1')
    expect(merged).not.toHaveProperty('FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK')
    expect(merged).not.toHaveProperty('FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK')
  })

  it('selects the absent-env contract in the dedicated Meilisearch refusal script', () => {
    expect(packageJson.scripts['test:e2e-ui:migrate-meilisearch-refusal'])
      .toContain('meilisearch_loopback_refusal')
  })

  it('maps every declared Playwright entry point to configured projects', () => {
    const scripts: Record<string, string> = packageJson.scripts
    const configuredProjectNames = new Set(
      (playwrightConfig.projects ?? []).flatMap((project) => (
        typeof project.name === 'string' ? [project.name] : []
      )),
    )

    for (const scriptName of CONTRACT.npm_script_spec_scopes.playwright_entry_points) {
      const scriptBody = scripts[scriptName]
      expect(scriptBody, `${scriptName} must resolve to a package.json script`).toBeTypeOf('string')

      const referencedProjects = Array.from(
        scriptBody.matchAll(/(?:^|\s)--project(?:=|\s+)([^\s]+)/g),
        (match) => match[1],
      )
      expect(referencedProjects, `${scriptName} must select at least one Playwright project`)
        .not.toHaveLength(0)

      for (const projectName of referencedProjects) {
        expect(
          configuredProjectNames,
          `${scriptName} references missing Playwright project ${projectName}`,
        ).toContain(projectName)
      }
    }
  })

  it('fails loudly rather than returning an empty environment when the contract is unreadable', () => {
    // A missing contract must not degrade to "no requirements". That would hand back a
    // backend with none of the capabilities and turn a wiring error into N confusing
    // spec failures much later in the run.
    expect(() => backendContractEnv('/nonexistent/e2e_backend_contract.json')).toThrow()
  })
})

describe('readBackendEnv', () => {
  const ADMIN_KEY_ENV_NAMES = [
    'PLAYWRIGHT_BACKEND_ADMIN_KEY',
    'FJ_TEST_ADMIN_KEY',
    'FLAPJACK_ADMIN_KEY',
  ] as const

  function withAdminKeyEnv(overrides: Record<string, string | undefined>, run: () => void) {
    const saved = new Map<string, string | undefined>()
    for (const name of ADMIN_KEY_ENV_NAMES) {
      saved.set(name, process.env[name])
      delete process.env[name]
    }
    try {
      for (const [name, value] of Object.entries(overrides)) {
        if (value === undefined) {
          delete process.env[name]
        } else {
          process.env[name] = value
        }
      }
      run()
    } finally {
      for (const [name, value] of saved) {
        if (value === undefined) {
          delete process.env[name]
        } else {
          process.env[name] = value
        }
      }
    }
  }

  it('reads the backend endpoint from PLAYWRIGHT_BACKEND environment variables', () => {
    vi.stubEnv('PLAYWRIGHT_BACKEND_URL', 'http://127.0.0.1:17707')
    vi.stubEnv('PLAYWRIGHT_BACKEND_HOST', '127.0.0.1')
    vi.stubEnv('PLAYWRIGHT_BACKEND_PORT', '17707')
    vi.stubEnv('PLAYWRIGHT_BACKEND_DATA_DIR', '/tmp/flapjack/17707-data')

    expect(readBackendEnv()).toMatchObject({
      url: 'http://127.0.0.1:17707',
      host: '127.0.0.1',
      port: 17707,
      dataDir: '/tmp/flapjack/17707-data',
    })
  })

  it('consumes the canonical resolved key from PLAYWRIGHT_BACKEND_ADMIN_KEY over any raw fallback', () => {
    // playwright.config.ts resolves the admin key through getLocalInstanceConfig — which
    // reads flapjack.local.conf and discovers a reused loopback backend's custom key from
    // its process line — and forwards the result as PLAYWRIGHT_BACKEND_ADMIN_KEY. When the
    // resolver selected a non-default key, readiness must authenticate with it, not with a
    // stale raw FLAPJACK_ADMIN_KEY or the hardcoded dev default.
    withAdminKeyEnv({
      PLAYWRIGHT_BACKEND_ADMIN_KEY: 'fj_resolvedfromprocessdiscovery',
      FLAPJACK_ADMIN_KEY: 'fj_devtestadminkey000000',
    }, () => {
      expect(readBackendEnv().adminKey).toBe('fj_resolvedfromprocessdiscovery')
    })
  })

  it('does not fall back to the hardcoded default once the resolver supplied a key', () => {
    withAdminKeyEnv({ PLAYWRIGHT_BACKEND_ADMIN_KEY: 'fj_customreusedbackendkey' }, () => {
      expect(readBackendEnv().adminKey).not.toBe('fj_devtestadminkey000000')
      expect(readBackendEnv().adminKey).toBe('fj_customreusedbackendkey')
    })
  })

  it('falls back to the dev default only when no resolver or raw key is present', () => {
    withAdminKeyEnv({}, () => {
      expect(readBackendEnv().adminKey).toBe('fj_devtestadminkey000000')
    })
  })
})

describe('startPlaywrightServers', () => {
  const backend = {
    url: 'http://127.0.0.1:7711',
    host: '127.0.0.1',
    port: 7711,
    dataDir: '/tmp/flapjack/test-data',
  }
  const dashboard = {
    url: 'http://127.0.0.1:5511',
    host: '127.0.0.1',
    port: 5511,
    allowReuse: false,
  }
  const readiness = { timeoutMs: 100, pollIntervalMs: 1 }

  it('waits for backend health and the dashboard URL, in that order', async () => {
    const backendChild = createFakeChild()
    const dashboardChild = createFakeChild()
    const ensureServerImpl = vi.fn()
      .mockResolvedValueOnce({ mode: 'spawn', label: 'backend server', child: backendChild })
      .mockResolvedValueOnce({ mode: 'spawn', label: 'dashboard dev server', child: dashboardChild })
    const assertBackendReadinessImpl = vi.fn().mockResolvedValue({ vectorSearch: true })

    const started = await startPlaywrightServers({
      backend,
      dashboard,
      readiness,
      ensureServer: ensureServerImpl,
      assertBackendReadiness: assertBackendReadinessImpl,
    })

    expect(ensureServerImpl).toHaveBeenCalledTimes(2)
    expect(ensureServerImpl.mock.calls[0][0]).toMatchObject({
      label: 'backend server',
      url: 'http://127.0.0.1:7711/health',
      host: '127.0.0.1',
      port: 7711,
      // A backend already listening on the shared port is reused rather than
      // fought over; the capability assertion below is what proves it is correct.
      allowReuse: true,
    })
    expect(ensureServerImpl.mock.calls[1][0]).toMatchObject({
      label: 'dashboard dev server',
      url: 'http://127.0.0.1:5511',
      allowReuse: false,
    })
    expect(assertBackendReadinessImpl).toHaveBeenCalledWith(
      'http://127.0.0.1:7711',
      { allowTextOnlyNegativeControl: false, adminKey: undefined },
    )
    expect(started).toEqual([
      { label: 'backend server', child: backendChild },
      { label: 'dashboard dev server', child: dashboardChild },
    ])
  })

  it('requires a fresh backend for a contract with an absent environment variable', async () => {
    const ensureServerImpl = vi.fn()
      .mockResolvedValueOnce({ mode: 'spawn', label: 'backend server', child: createFakeChild() })
      .mockResolvedValueOnce({ mode: 'spawn', label: 'dashboard dev server', child: createFakeChild() })
    const contractEnvironment = backendContractEnv(CONTRACT_PATH, [
      'replication',
      'ai_local_outbound',
      'meilisearch_loopback_refusal',
    ])

    await startPlaywrightServers({
      backend,
      dashboard,
      readiness,
      backendContractEnvironment: contractEnvironment,
      ensureServer: ensureServerImpl,
      // Must be `assertBackendReadiness`: injecting the old `assertVectorSearchEnabled`
      // name is silently ignored, so the real readiness probe runs and opens a live
      // socket to the backend port from a unit test.
      assertBackendReadiness: vi.fn().mockResolvedValue({ vectorSearch: true }),
    })

    expect(ensureServerImpl.mock.calls[0][0]).toMatchObject({
      label: 'backend server',
      requireFreshProcess: true,
    })
  })

  it('aborts before starting the dashboard when the backend has no vector support', async () => {
    const backendChild = createFakeChild()
    const ensureServerImpl = vi.fn()
      .mockResolvedValueOnce({ mode: 'spawn', label: 'backend server', child: backendChild })
    const assertBackendReadinessImpl = vi.fn()
      .mockRejectedValue(new Error('vectorSearch=false'))

    await expect(startPlaywrightServers({
      backend,
      dashboard,
      readiness,
      ensureServer: ensureServerImpl,
      assertBackendReadiness: assertBackendReadinessImpl,
    })).rejects.toThrow('vectorSearch=false')

    expect(ensureServerImpl).toHaveBeenCalledTimes(1)
    expect(backendChild.kill).toHaveBeenCalledWith('SIGTERM')
  })

  it('forwards the explicit P20 text-only control to the capability gate', async () => {
    const assertBackendReadinessImpl = vi.fn().mockResolvedValue({ vectorSearch: false })
    const ensureServerImpl = vi.fn()
      .mockResolvedValueOnce({ mode: 'reuse', label: 'backend server' })
      .mockResolvedValueOnce({ mode: 'reuse', label: 'dashboard dev server' })

    await startPlaywrightServers({
      backend: { ...backend, allowTextOnlyNegativeControl: true },
      dashboard,
      readiness,
      ensureServer: ensureServerImpl,
      assertBackendReadiness: assertBackendReadinessImpl,
    })

    expect(assertBackendReadinessImpl).toHaveBeenCalledWith(
      'http://127.0.0.1:7711',
      { allowTextOnlyNegativeControl: true, adminKey: undefined },
    )
  })

  it('rejects a reused non-loopback backend that still uses the public dev admin key', async () => {
    const ensureServerImpl = vi.fn()
    const assertBackendReadinessImpl = vi.fn()

    await expect(startPlaywrightServers({
      backend: {
        ...backend,
        url: 'http://192.168.1.20:7711',
        host: '192.168.1.20',
        adminKey: 'fj_devtestadminkey000000',
      },
      dashboard,
      readiness,
      ensureServer: ensureServerImpl,
      assertBackendReadiness: assertBackendReadinessImpl,
    })).rejects.toThrow(/public dashboard dev admin key/)

    expect(ensureServerImpl).not.toHaveBeenCalled()
    expect(assertBackendReadinessImpl).not.toHaveBeenCalled()
  })

  it('stops a spawned backend when dashboard startup fails', async () => {
    const backendChild = createFakeChild()
    const ensureServerImpl = vi.fn()
      .mockResolvedValueOnce({ mode: 'spawn', label: 'backend server', child: backendChild })
      .mockRejectedValueOnce(new Error('dashboard startup failed'))

    await expect(startPlaywrightServers({
      backend,
      dashboard,
      readiness,
      ensureServer: ensureServerImpl,
      assertBackendReadiness: vi.fn().mockResolvedValue({ vectorSearch: true }),
    })).rejects.toThrow('dashboard startup failed')

    expect(backendChild.kill).toHaveBeenCalledWith('SIGTERM')
  })

  it('reports only the processes it actually spawned', async () => {
    const dashboardChild = createFakeChild()
    const ensureServerImpl = vi.fn()
      .mockResolvedValueOnce({ mode: 'reuse', label: 'backend server' })
      .mockResolvedValueOnce({ mode: 'spawn', label: 'dashboard dev server', child: dashboardChild })

    const started = await startPlaywrightServers({
      backend,
      dashboard,
      readiness,
      ensureServer: ensureServerImpl,
      assertBackendReadiness: vi.fn().mockResolvedValue({ vectorSearch: true }),
    })

    expect(started).toEqual([{ label: 'dashboard dev server', child: dashboardChild }])
  })

  it('uses the same clean-start admin key for backend launch and readiness', async () => {
    const backendChild = createFakeChild()
    const dashboardChild = createFakeChild()
    const backendWithFileKey = { ...backend, adminKey: 'fj_file_custom' }
    const spawnBackendServerImpl = vi.fn().mockReturnValue(backendChild)
    const ensureServerImpl = vi.fn(async ({
      label,
      spawnServer,
    }: {
      label: string
      spawnServer: () => ReturnType<typeof createFakeChild>
    }) => {
      if (label === 'backend server') {
        return { mode: 'spawn', label, child: spawnServer() }
      }
      return { mode: 'spawn', label, child: dashboardChild }
    })
    const assertBackendReadinessImpl = vi.fn().mockResolvedValue({ vectorSearch: true })

    await startPlaywrightServers({
      backend: backendWithFileKey,
      dashboard,
      readiness,
      ensureServer: ensureServerImpl,
      assertBackendReadiness: assertBackendReadinessImpl,
      spawnBackendServer: spawnBackendServerImpl,
    })

    // The launcher forwards the resolved backend, the spawner, and the selected
    // contract environment. Asserting all three keeps the admin-key claim honest and
    // pins the contract env onto the same call, so a launch that drops the loopback
    // opt-ins can no longer pass this test.
    expect(spawnBackendServerImpl).toHaveBeenCalledWith(
      backendWithFileKey,
      expect.any(Function),
      expect.objectContaining({
        FLAPJACK_AI_ALLOW_LOCAL_URLS: '1',
        FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK: '1',
        FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK: '1',
      }),
    )
    expect(assertBackendReadinessImpl).toHaveBeenCalledWith(
      'http://127.0.0.1:7711',
      { allowTextOnlyNegativeControl: false, adminKey: 'fj_file_custom' },
    )
  })
})

describe('forwardShutdownSignals', () => {
  it('signals every spawned child so no backend survives the run', () => {
    const backendChild = createFakeChild()
    const dashboardChild = createFakeChild()
    const handlers = new Map<string, () => void>()
    const target = {
      once: vi.fn((signal: string, handler: () => void) => {
        handlers.set(signal, handler)
      }),
    }

    forwardShutdownSignals(
      [
        { label: 'backend server', child: backendChild },
        { label: 'dashboard dev server', child: dashboardChild },
      ],
      target,
    )

    handlers.get('SIGTERM')?.()

    expect(backendChild.kill).toHaveBeenCalledWith('SIGTERM')
    expect(dashboardChild.kill).toHaveBeenCalledWith('SIGTERM')
  })

  it('does not re-signal a child that already exited', () => {
    const child = createFakeChild()
    child.killed = true
    const handlers = new Map<string, () => void>()
    const target = {
      once: vi.fn((signal: string, handler: () => void) => {
        handlers.set(signal, handler)
      }),
    }

    forwardShutdownSignals([{ label: 'backend server', child }], target)
    handlers.get('SIGINT')?.()

    expect(child.kill).not.toHaveBeenCalled()
  })
})

describe('waitForPortFree', () => {
  it('returns immediately when the port is already free', async () => {
    const probePort = vi.fn().mockResolvedValue(false)
    const sleep = vi.fn()

    await waitForPortFree('127.0.0.1', 5177, {
      probePort,
      sleep,
      timeoutMs: 100,
      pollIntervalMs: 1,
    })

    expect(probePort).toHaveBeenCalledTimes(1)
    expect(probePort).toHaveBeenCalledWith('127.0.0.1', 5177)
    expect(sleep).not.toHaveBeenCalled()
  })

  it('polls until the port becomes free', async () => {
    const probePort = vi.fn()
      .mockResolvedValueOnce(true)
      .mockResolvedValueOnce(true)
      .mockResolvedValueOnce(false)
    const sleep = vi.fn().mockResolvedValue(undefined)

    await waitForPortFree('127.0.0.1', 5177, {
      probePort,
      sleep,
      timeoutMs: 100,
      pollIntervalMs: 1,
    })

    expect(probePort).toHaveBeenCalledTimes(3)
    expect(sleep).toHaveBeenCalledTimes(2)
  })

  it('throws after timeout when the port never becomes free', async () => {
    const probePort = vi.fn().mockResolvedValue(true)
    const sleep = vi.fn().mockResolvedValue(undefined)

    await expect(waitForPortFree('127.0.0.1', 5177, {
      probePort,
      sleep,
      timeoutMs: 10,
      pollIntervalMs: 1,
    })).rejects.toThrow('Timed out waiting for 127.0.0.1:5177 to become free')
  })
})

describe('resolveWaitForPortFreeTarget', () => {
  it('prefers the Playwright URL target over the raw bind host', () => {
    expect(resolveWaitForPortFreeTarget({
      url: 'http://127.0.0.1:5177',
      host: '0.0.0.0',
      port: 5177,
    })).toEqual({
      host: '127.0.0.1',
      port: 5177,
    })
  })

  it('normalizes bracketed IPv6 URL hosts for node socket probes', () => {
    expect(resolveWaitForPortFreeTarget({
      url: 'http://[::1]:5177',
      host: '0.0.0.0',
      port: 5177,
    })).toEqual({
      host: '::1',
      port: 5177,
    })
  })

  it('falls back to the explicit host and port when the URL is missing', () => {
    expect(resolveWaitForPortFreeTarget({
      url: undefined,
      host: '127.0.0.1',
      port: 5177,
    })).toEqual({
      host: '127.0.0.1',
      port: 5177,
    })
  })
})
