import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  FIRST_PAGE_NAMES,
  LAPTOP_NAMES,
  NOVA_NAMES,
  SECOND_PAGE_NAMES,
} from '../browser_tests_unmocked/fixture_data.mjs';
import { createFlapjackLiteSearchClient } from '../lib/flapjack_requester.js';

const here = dirname(fileURLToPath(import.meta.url));
const sdkDir = resolve(here, '..');
const engineDir = resolve(sdkDir, '..');
const rootDir = resolve(engineDir, '..');

// Identical expected lists let a server ignore an interaction while the browser
// test stays green. Keep every behavioral checkpoint observably different.
const resultSets = [FIRST_PAGE_NAMES, LAPTOP_NAMES, NOVA_NAMES, SECOND_PAGE_NAMES]
  .map((names) => JSON.stringify(names));
assert.equal(new Set(resultSets).size, resultSets.length, 'browser scenarios must have distinct results');

assert.throws(
  () => createFlapjackLiteSearchClient({
    baseUrl: 'http://customer.example',
    applicationId: 'flapjack',
    apiKey: 'redacted-fixture-key',
  }),
  /must use HTTPS/,
  'generated non-loopback customer configuration must reject plaintext origins',
);
assert.ok(
  createFlapjackLiteSearchClient({
    baseUrl: 'http://127.0.0.1:7700',
    applicationId: 'flapjack',
    apiKey: 'redacted-fixture-key',
  }),
  'loopback source conformance may use a plaintext ephemeral fixture origin',
);

const requester = readFileSync(resolve(sdkDir, 'lib/flapjack_requester.js'), 'utf8');
assert.match(requester, /from 'algoliasearch\/lite'/, 'browser conformance must use the official lite client');
assert.match(requester, /createFlapjackLiteSearchClient/, 'the lite client must have an explicit shared factory');
assert.doesNotMatch(requester, /requester\s*:/, 'PBV3 must not install a custom request wrapper');
assert.doesNotMatch(requester, /Authorization|authorization/, 'PBV3 must not add a bearer header');
assert.match(
  requester,
  /createFlapjackLiteSearchClient[^]*'WithinQueryParameters'/,
  'the pinned official lite client must use its native query-parameter credential mode',
);
assert.match(
  requester,
  /Non-loopback Flapjack origins must use HTTPS/,
  'generated non-loopback direct-engine configuration must require HTTPS',
);

const browserApp = readFileSync(resolve(sdkDir, 'browser_tests_unmocked/app/main.js'), 'utf8');
assert.match(
  browserApp,
  /createFlapjackLiteSearchClient\(configuration\)/,
  'the rendered browser application must instantiate the lite client',
);
assert.match(browserApp, /from 'search-insights'/, 'the browser KAT must use official search-insights');
assert.match(browserApp, /useCookie:\s*false/, 'the browser KAT must keep Insights cookies disabled');
assert.match(
  browserApp,
  /clickedObjectIDsAfterSearch/,
  'the complete browser journey must call the frozen after-search click method',
);
assert.doesNotMatch(browserApp, /VITE_FLAPJACK_ADMIN_KEY/, 'browser code must not read the admin key');

const playwrightConfig = readFileSync(
  resolve(sdkDir, 'browser_tests_unmocked/playwright.config.mjs'),
  'utf8',
);
assert.doesNotMatch(
  playwrightConfig,
  /VITE_FLAPJACK_ADMIN_KEY/,
  'the browser bundle must never receive the administrative key',
);
assert.match(
  playwrightConfig,
  /VITE_FLAPJACK_SEARCH_KEY/,
  'the browser bundle must receive only its fixture-scoped search key',
);
assert.match(
  playwrightConfig,
  /trace:\s*'off'/,
  'the query-credential KAT must not persist credentials in Playwright traces',
);

const packageJson = JSON.parse(readFileSync(resolve(sdkDir, 'package.json'), 'utf8'));
const requiredPackages = [
  '@playwright/test',
  'instantsearch.js',
  'react',
  'react-dom',
  'react-instantsearch',
  'vite',
  'vue',
  'vue-instantsearch',
];

const frozenPackages = {
  algoliasearch: '5.57.0',
  'instantsearch.js': '4.112.0',
  'react-instantsearch': '7.45.0',
  'search-insights': '2.17.3',
  'vue-instantsearch': '4.29.4',
};

for (const packageName of requiredPackages) {
  assert.ok(
    packageJson.devDependencies?.[packageName] || packageJson.dependencies?.[packageName],
    `real-client conformance must install the official runtime/tooling package ${packageName}`,
  );
}

const packageLock = JSON.parse(readFileSync(resolve(sdkDir, 'package-lock.json'), 'utf8'));
for (const [packageName, version] of Object.entries(frozenPackages)) {
  const manifestVersion = packageJson.dependencies?.[packageName]
    || packageJson.devDependencies?.[packageName];
  assert.equal(manifestVersion, version, `${packageName} must be exactly pinned`);
  assert.equal(
    packageLock.packages?.[`node_modules/${packageName}`]?.version,
    version,
    `${packageName} lock entry must match the frozen campaign version`,
  );
}

assert.equal(
  packageJson.scripts?.['test:real_clients'],
  'node browser_tests_unmocked/run_real_client_conformance.mjs',
  'package.json must expose the canonical real-client browser test command',
);

// Debbie remaps the private canonical owner into engine/s/test on public
// mirrors. Exercise whichever side of that single mapping exists here.
const devRunner = resolve(engineDir, '_dev/s/test');
const runnerPath = existsSync(devRunner) ? devRunner : resolve(engineDir, 's/test');
const runner = readFileSync(runnerPath, 'utf8');
assert.match(
  runner,
  /run_sdk_npm_test[^]*test:real_clients/,
  './s/test --sdk must execute the real-client browser suite against its managed Flapjack server',
);

const workflow = readFileSync(resolve(rootDir, '.github/workflows/ci.yml'), 'utf8');
assert.match(
  workflow,
  /name: SDK real-client conformance[^]*npm run test:real_clients/,
  'public CI must run the real-client suite rather than leaving it local-only',
);
assert.match(
  workflow,
  /name: Install SDK Playwright browser\n[^]*?timeout-minutes: 5\n[^]*?run: npx playwright install chromium\n[^]*?name: SDK real-client conformance/,
  'public CI must provision the browser in a bounded step before the real-client suite',
);

console.log('PASS real-client conformance dependency and recurring-gate wiring');
