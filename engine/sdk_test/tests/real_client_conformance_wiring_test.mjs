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

const here = dirname(fileURLToPath(import.meta.url));
const sdkDir = resolve(here, '..');
const engineDir = resolve(sdkDir, '..');
const rootDir = resolve(engineDir, '..');

// Identical expected lists let a server ignore an interaction while the browser
// test stays green. Keep every behavioral checkpoint observably different.
const resultSets = [FIRST_PAGE_NAMES, LAPTOP_NAMES, NOVA_NAMES, SECOND_PAGE_NAMES]
  .map((names) => JSON.stringify(names));
assert.equal(new Set(resultSets).size, resultSets.length, 'browser scenarios must have distinct results');

const requester = readFileSync(resolve(sdkDir, 'lib/flapjack_requester.js'), 'utf8');
assert.match(requester, /from 'algoliasearch\/lite'/, 'browser conformance must use the official lite client');
assert.match(requester, /createFlapjackLiteSearchClient/, 'the lite client must have an explicit shared factory');

const browserApp = readFileSync(resolve(sdkDir, 'browser_tests_unmocked/app/main.js'), 'utf8');
assert.match(
  browserApp,
  /createFlapjackLiteSearchClient\(configuration\)/,
  'the rendered browser application must instantiate the lite client',
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

for (const packageName of requiredPackages) {
  assert.ok(
    packageJson.devDependencies?.[packageName] || packageJson.dependencies?.[packageName],
    `real-client conformance must install the official runtime/tooling package ${packageName}`,
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
