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

  it('passes only the webserver contract variables so proof artifacts hold no ambient secrets', async () => {
    // Playwright merges process.env into the spawned webserver itself
    // (playwright/lib/plugins/webServerPlugin.js), so the child still sees the full
    // environment without this config declaring it. What the config declares is also
    // what the JSON reporter serialises into test-results/results.json — the durable
    // JOIN-1 proof artifact — so spreading `...process.env` here wrote every ambient
    // credential (ALGOLIA_ADMIN_KEY, MAILSLURP_API_KEY, ...) into evidence files in
    // plaintext. Declare exactly the contract scripts/playwright-webserver.mjs reads.
    const config = await loadPlaywrightConfig('')

    expect(Object.keys(config.webServer?.env ?? {}).sort()).toEqual([
      'PLAYWRIGHT_BACKEND_DATA_DIR',
      'PLAYWRIGHT_BACKEND_HOST',
      'PLAYWRIGHT_BACKEND_PORT',
      'PLAYWRIGHT_BACKEND_URL',
      'PLAYWRIGHT_WEBSERVER_HOST',
      'PLAYWRIGHT_WEBSERVER_PORT',
      'PLAYWRIGHT_WEBSERVER_REUSE',
      'PLAYWRIGHT_WEBSERVER_URL',
    ])
    // PATH is present in every real process environment, so its absence proves the
    // ambient spread is gone rather than merely that these four keys are present.
    expect(config.webServer?.env).not.toHaveProperty('PATH')
  })

  it('declares the backend launch target so the webserver owns the vector-enabled backend', async () => {
    // Before this contract, `webServer` started only Vite and every e2e run silently
    // depended on an operator having pre-started a backend by hand. A text-only
    // backend then made the four vector specs skip through skipWhenVectorSearchDisabled
    // and the suite still reported green. The backend target has to travel with the
    // Playwright startup contract for those specs to be able to run at all.
    const config = await loadPlaywrightConfig('')

    expect(config.webServer?.env).toMatchObject({
      PLAYWRIGHT_BACKEND_HOST: mockInstance.host,
      PLAYWRIGHT_BACKEND_PORT: String(mockInstance.backendPort),
      PLAYWRIGHT_BACKEND_URL: mockInstance.backendBaseUrl,
      // Pinned to the same directory tests/fixtures/local-instance.ts resolves, so
      // filesystem-backed fixtures read the data dir the backend actually writes.
      PLAYWRIGHT_BACKEND_DATA_DIR: mockInstance.backendDataDir,
    })
  })

  it('redacts the JSON report after Playwright writes it', async () => {
    const config = await loadPlaywrightConfig('')
    const reporters = config.reporter as Array<[string, Record<string, unknown>?]>
    const jsonReporterIndex = reporters.findIndex(([name]) => name === 'json')
    const redactingReporterIndex = reporters.findIndex(
      ([name]) => name === './scripts/redact_playwright_evidence.mjs',
    )

    expect(jsonReporterIndex).toBeGreaterThanOrEqual(0)
    expect(redactingReporterIndex).toBeGreaterThan(jsonReporterIndex)
    expect(reporters[redactingReporterIndex]?.[1]).toEqual({
      inputFile: 'test-results/results.json',
    })
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

  it('emits machine-readable JSON results so the JOIN-1 report can be computed', async () => {
    // The HTML report is for humans and cannot be joined against the 90-row backend
    // capability matrix. Without a JSON artifact, answering "which of the 27 named
    // proof specs passed at this SHA?" costs a manual read of a 1,639-line receipt
    // against an HTML report — which is why JOIN-1 read 0 / 90 for three consecutive
    // lanes while the suite itself was near-green. scripts/join_proof_report.mjs
    // consumes exactly this path, so dropping the reporter or renaming the file
    // silently returns that row to being uncomputable. Assert both.
    const config = await loadPlaywrightConfig('')
    const reporters = config.reporter as Array<[string, Record<string, unknown>?]>
    const jsonReporter = reporters.find(([name]) => name === 'json')

    expect(jsonReporter).toBeDefined()
    expect(jsonReporter?.[1]?.outputFile).toBe('test-results/results.json')
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
