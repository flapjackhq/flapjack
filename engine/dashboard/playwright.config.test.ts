/* @vitest-environment node */
import { afterEach, describe, expect, it, vi } from 'vitest'

type PlaywrightProject = {
  name?: string
  dependencies?: string[]
  teardown?: string
  testMatch?: string | string[]
  testIgnore?: string[]
}

const { mockInstance } = vi.hoisted(() => ({
  mockInstance: {
    host: '127.0.0.1',
    backendPort: 7711,
    dashboardPort: 5511,
    adminKey: 'test-admin-key',
    backendBaseUrl: 'http://127.0.0.1:7711',
    backendDataDir: '/tmp/flapjack/test-data',
    dashboardBaseUrl: 'http://127.0.0.1:5511',
    configPath: '/tmp/flapjack.local.conf',
    loadedFromFile: true,
  },
}))

async function loadPlaywrightConfig(
  ciValue?: string,
  workerOverride?: string,
  laneCBundleDir?: string,
) {
  vi.resetModules()
  vi.unstubAllEnvs()

  if (ciValue !== undefined) {
    vi.stubEnv('CI', ciValue)
  }
  if (workerOverride !== undefined) {
    vi.stubEnv('PLAYWRIGHT_E2E_WORKERS', workerOverride)
  }
  if (laneCBundleDir !== undefined) {
    vi.stubEnv('LANE_C_BUNDLE_DIR', laneCBundleDir)
  }

  vi.doMock('./local-instance-config', () => ({
    getLocalInstanceConfig: () => mockInstance,
  }))

  const module = await import('./playwright.config')
  vi.doUnmock('./local-instance-config')
  return module.default
}

function findProject(configProjects: unknown, name: string): PlaywrightProject | undefined {
  if (!Array.isArray(configProjects)) {
    return undefined
  }

  return configProjects.find((project): project is PlaywrightProject => (
    typeof project === 'object'
    && project !== null
    && 'name' in project
    && (project as { name?: string }).name === name
  ))
}

afterEach(() => {
  vi.unstubAllEnvs()
  vi.resetModules()
  vi.doUnmock('./local-instance-config')
})

describe('playwright.config startup contracts', () => {
  it('uses clone-local dashboard URL and expected lifecycle projects', async () => {
    // Force the local/non-CI branch explicitly so this contract test is stable
    // even when the Vitest process itself is running under CI=true.
    const config = await loadPlaywrightConfig('')

    expect(config.use?.baseURL).toBe(mockInstance.dashboardBaseUrl)
    expect(config.webServer).toMatchObject({
      command: 'node scripts/playwright-webserver.mjs',
      url: mockInstance.dashboardBaseUrl,
      timeout: 120_000,
      reuseExistingServer: false,
    })
    expect(config.webServer?.env).toMatchObject({
      PLAYWRIGHT_WEBSERVER_HOST: mockInstance.host,
      PLAYWRIGHT_WEBSERVER_PORT: String(mockInstance.dashboardPort),
      PLAYWRIGHT_WEBSERVER_URL: mockInstance.dashboardBaseUrl,
      PLAYWRIGHT_WEBSERVER_REUSE: '0',
    })

    const seedProject = findProject(config.projects, 'seed')
    const cleanupProject = findProject(config.projects, 'cleanup')
    const uiProject = findProject(config.projects, 'e2e-ui')
    const apiProject = findProject(config.projects, 'e2e-api')

    expect(seedProject?.testMatch).toBe('seed.setup.ts')
    expect(seedProject?.teardown).toBe('cleanup')
    expect(cleanupProject?.testMatch).toBe('cleanup.setup.ts')
    expect(uiProject?.dependencies).toEqual(['seed'])
    expect(uiProject?.testIgnore).toEqual([
      '*.setup.ts',
      '*.test.ts',
      'jun04_pm_lane_c_audit.spec.ts',
      'jun05_am_lane_c_round2_audit.spec.ts',
    ])
    expect(apiProject).toMatchObject({ testDir: './tests/e2e-api' })

    // Non-CI defaults: permissive parallelism, no retries, forbidOnly disabled
    expect(config.workers).toBe(3)
    expect(config.retries).toBe(0)
    expect(config.forbidOnly).toBe(false)
    expect(config.fullyParallel).toBe(true)
  })

  it('tightens retries/workers and disables webServer reuse in CI', async () => {
    const config = await loadPlaywrightConfig('1')

    expect(config.forbidOnly).toBe(true)
    expect(config.retries).toBe(2)
    expect(config.workers).toBe(1)
    expect(config.webServer).toMatchObject({
      reuseExistingServer: false,
    })
    expect(config.webServer?.env).toMatchObject({
      PLAYWRIGHT_WEBSERVER_REUSE: '0',
    })
  })

  it('honors PLAYWRIGHT_E2E_WORKERS override for local runs', async () => {
    const config = await loadPlaywrightConfig(undefined, '1')
    expect(config.workers).toBe(1)
  })

  it('pins the HTML reporter to open: never so a red run returns unattended', async () => {
    // Playwright resolves the reporter's `open` as
    //   PLAYWRIGHT_HTML_OPEN || options.open || 'on-failure'
    // (playwright/lib/reporters/html.js). With no explicit option, a red run whose
    // stdin is a TTY serves the report and blocks forever instead of exiting, which
    // is how `./s/test --dashboard-full` stopped returning. Pinning 'never' here is
    // what makes the canonical runner terminate; the env var still overrides it, so
    // a human keeps the old behaviour without a second Flapjack-owned flag.
    // The end-to-end behaviour is proven separately by a PTY-based probe under
    // _dev/testing/ (internal tooling, not part of the published dashboard).
    const config = await loadPlaywrightConfig('')

    expect(Array.isArray(config.reporter)).toBe(true)
    const reporters = config.reporter as Array<[string, Record<string, unknown>?]>
    const htmlReporter = reporters.find(([name]) => name === 'html')

    expect(htmlReporter).toBeDefined()
    expect(htmlReporter?.[1]?.open).toBe('never')
  })

  it('runs Lane C evidence-only specs only when the bundle directory is explicit', async () => {
    const defaultConfig = await loadPlaywrightConfig('')
    const defaultUiProject = findProject(defaultConfig.projects, 'e2e-ui')

    expect(defaultUiProject?.testIgnore).toContain('jun04_pm_lane_c_audit.spec.ts')
    expect(defaultUiProject?.testIgnore).toContain('jun05_am_lane_c_round2_audit.spec.ts')

    const laneCConfig = await loadPlaywrightConfig('', undefined, 'docs/live-state/jun05_am_lane_c_baseline/20260605T045543Z')
    const laneCUiProject = findProject(laneCConfig.projects, 'e2e-ui')

    expect(laneCUiProject?.testIgnore).toEqual(['*.setup.ts', '*.test.ts'])
  })
})
