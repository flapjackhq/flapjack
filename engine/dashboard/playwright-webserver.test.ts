/* @vitest-environment node */
import { describe, expect, it, vi } from 'vitest'

import {
  assertVectorSearchEnabled,
  ensureServer,
  forwardShutdownSignals,
  resolveWaitForPortFreeTarget,
  spawnBackendServer,
  startPlaywrightServers,
  waitForPortFree,
} from './scripts/playwright-webserver.mjs'

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

describe('assertVectorSearchEnabled', () => {
  it('returns the reported capabilities for a vector-enabled backend', async () => {
    const fetchImpl = vi.fn().mockResolvedValue(
      healthResponse({ vectorSearch: true, vectorSearchLocal: true }),
    )

    await expect(assertVectorSearchEnabled('http://127.0.0.1:7700', { fetchImpl }))
      .resolves.toEqual({ vectorSearch: true, vectorSearchLocal: true })
    expect(fetchImpl).toHaveBeenCalledWith('http://127.0.0.1:7700/health', { method: 'GET' })
  })

  it('fails loudly on a text-only backend instead of letting specs mass-skip', async () => {
    // Without this gate a text-only backend produces a fully green run whose specs
    // were silently skipped by skipWhenVectorSearchDisabled — the exact failure this
    // startup contract exists to prevent. The message must name the rebuild command
    // so the operator is not left guessing which binary is wrong.
    const fetchImpl = vi.fn().mockResolvedValue(
      healthResponse({ vectorSearch: false, vectorSearchLocal: false }),
    )

    await expect(assertVectorSearchEnabled('http://127.0.0.1:7700', { fetchImpl }))
      .rejects.toThrow(/vectorSearch=false[\s\S]*npm run update-server/)
  })

  it('allows a text-only backend only for the explicit P20 negative control', async () => {
    const fetchImpl = vi.fn().mockResolvedValue(
      healthResponse({ vectorSearch: false, vectorSearchLocal: false }),
    )

    await expect(assertVectorSearchEnabled('http://127.0.0.1:7700', {
      fetchImpl,
      allowTextOnlyNegativeControl: true,
    })).resolves.toEqual({ vectorSearch: false, vectorSearchLocal: false })
  })

  it('rejects when the health probe itself is not readable', async () => {
    const fetchImpl = vi.fn().mockResolvedValue({
      ok: false,
      status: 503,
      json: async () => ({}),
    })

    await expect(assertVectorSearchEnabled('http://127.0.0.1:7700', { fetchImpl }))
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
    const assertVectorSearchEnabledImpl = vi.fn().mockResolvedValue({ vectorSearch: true })

    const started = await startPlaywrightServers({
      backend,
      dashboard,
      readiness,
      ensureServer: ensureServerImpl,
      assertVectorSearchEnabled: assertVectorSearchEnabledImpl,
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
    expect(assertVectorSearchEnabledImpl).toHaveBeenCalledWith(
      'http://127.0.0.1:7711',
      { allowTextOnlyNegativeControl: false },
    )
    expect(started).toEqual([
      { label: 'backend server', child: backendChild },
      { label: 'dashboard dev server', child: dashboardChild },
    ])
  })

  it('aborts before starting the dashboard when the backend has no vector support', async () => {
    const backendChild = createFakeChild()
    const ensureServerImpl = vi.fn()
      .mockResolvedValueOnce({ mode: 'spawn', label: 'backend server', child: backendChild })
    const assertVectorSearchEnabledImpl = vi.fn()
      .mockRejectedValue(new Error('vectorSearch=false'))

    await expect(startPlaywrightServers({
      backend,
      dashboard,
      readiness,
      ensureServer: ensureServerImpl,
      assertVectorSearchEnabled: assertVectorSearchEnabledImpl,
    })).rejects.toThrow('vectorSearch=false')

    expect(ensureServerImpl).toHaveBeenCalledTimes(1)
    expect(backendChild.kill).toHaveBeenCalledWith('SIGTERM')
  })

  it('forwards the explicit P20 text-only control to the capability gate', async () => {
    const assertVectorSearchEnabledImpl = vi.fn().mockResolvedValue({ vectorSearch: false })
    const ensureServerImpl = vi.fn()
      .mockResolvedValueOnce({ mode: 'reuse', label: 'backend server' })
      .mockResolvedValueOnce({ mode: 'reuse', label: 'dashboard dev server' })

    await startPlaywrightServers({
      backend: { ...backend, allowTextOnlyNegativeControl: true },
      dashboard,
      readiness,
      ensureServer: ensureServerImpl,
      assertVectorSearchEnabled: assertVectorSearchEnabledImpl,
    })

    expect(assertVectorSearchEnabledImpl).toHaveBeenCalledWith(
      'http://127.0.0.1:7711',
      { allowTextOnlyNegativeControl: true },
    )
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
      assertVectorSearchEnabled: vi.fn().mockResolvedValue({ vectorSearch: true }),
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
      assertVectorSearchEnabled: vi.fn().mockResolvedValue({ vectorSearch: true }),
    })

    expect(started).toEqual([{ label: 'dashboard dev server', child: dashboardChild }])
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
