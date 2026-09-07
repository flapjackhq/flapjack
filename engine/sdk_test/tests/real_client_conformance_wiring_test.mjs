import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  FIRST_PAGE_NAMES,
  INDEX_SETTINGS,
  LAPTOP_NAMES,
  NOVA_NAMES,
  PRODUCTS,
  SECOND_PAGE_NAMES,
} from '../browser_tests_unmocked/fixture_data.mjs';
import { createFlapjackLiteSearchClient } from '../lib/flapjack_requester.js';

const here = dirname(fileURLToPath(import.meta.url));
const sdkDir = resolve(here, '..');
const engineDir = resolve(sdkDir, '..');
const rootDir = resolve(engineDir, '..');

function assertExactKeys(value, expectedKeys, label) {
  assert.deepEqual(Object.keys(value).sort(), [...expectedKeys].sort(), `${label} keys must match`);
}

function collectScalarValues(value) {
  if (['string', 'number', 'boolean'].includes(typeof value)) {
    return [value];
  }
  if (Array.isArray(value)) {
    return value.flatMap(collectScalarValues);
  }
  if (value && typeof value === 'object') {
    return Object.values(value).flatMap(collectScalarValues);
  }
  return [];
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

// Comments and string literals are not code, so digits inside them are never
// numeric literals. Blank them out (preserving offsets) before scanning numbers.
const COMMENT_OR_STRING_LITERAL = /\/\/[^\n]*|\/\*[\s\S]*?\*\/|(['"`])(?:\\.|(?!\1)[^\\])*\1/g;
// An identifier before `[` opens a member-expression index unless it is one of
// these keywords, after which `[` opens an array literal instead.
const KEYWORDS_PRECEDING_ARRAY_LITERAL = new Set([
  'await', 'case', 'delete', 'do', 'else', 'in', 'instanceof', 'new', 'of', 'return',
  'throw', 'typeof', 'void', 'yield',
]);

function blankCommentsAndStrings(source) {
  return source.replace(COMMENT_OR_STRING_LITERAL, (match) => ' '.repeat(match.length));
}

function numericLiteralPattern(fixtureValue) {
  return new RegExp(`(?<![\\w$.])${escapeRegExp(String(fixtureValue))}(?![\\w$.])`, 'g');
}

function isMemberExpressionIndex(code, literalStart, literalEnd) {
  const before = code.slice(0, literalStart).trimEnd();
  const after = code.slice(literalEnd).trimStart();
  if (!before.endsWith('[') || !after.startsWith(']')) {
    return false;
  }
  const indexedObject = before.slice(0, -1).trimEnd().match(/([\w$]+|[)\]])$/);
  return Boolean(indexedObject) && !KEYWORDS_PRECEDING_ARRAY_LITERAL.has(indexedObject[1]);
}

function containsNumericFixtureLiteral(source, fixtureValue) {
  const code = blankCommentsAndStrings(source);
  return [...code.matchAll(numericLiteralPattern(fixtureValue))]
    .some((match) => !isMemberExpressionIndex(code, match.index, match.index + match[0].length));
}

function containsFixtureLiteral(source, fixtureValue) {
  if (typeof fixtureValue === 'string') {
    const quotedFixtureValue = new RegExp(`(['"\`])${escapeRegExp(fixtureValue)}\\1`);
    return quotedFixtureValue.test(source);
  }
  if (typeof fixtureValue === 'number') {
    return containsNumericFixtureLiteral(source, fixtureValue);
  }
  return false;
}

const sharedFixturePath = resolve(sdkDir, 'fixtures/official_client_contract.json');
const sharedFixture = JSON.parse(readFileSync(sharedFixturePath, 'utf8'));

assertExactKeys(sharedFixture, ['products', 'settings', 'expected'], 'shared fixture');
assertExactKeys(
  sharedFixture.settings,
  ['searchableAttributes', 'attributesForFaceting', 'customRanking', 'paginationLimitedTo'],
  'shared fixture settings',
);
assertExactKeys(
  sharedFixture.expected,
  [
    'firstPageNames',
    'secondPageNames',
    'laptopNames',
    'novaNames',
    'laptopObjectIDs',
    'laptopNbHits',
    'brandFacetHits',
  ],
  'shared fixture expectations',
);
for (const [index, product] of sharedFixture.products.entries()) {
  assertExactKeys(product, ['objectID', 'name', 'brand', 'sortOrder'], `shared fixture product ${index}`);
}
for (const [index, facetHit] of sharedFixture.expected.brandFacetHits.entries()) {
  assertExactKeys(facetHit, ['value', 'count'], `shared fixture facet hit ${index}`);
}

const expectedAdapterExports = {
  PRODUCTS: sharedFixture.products,
  INDEX_SETTINGS: sharedFixture.settings,
  FIRST_PAGE_NAMES: sharedFixture.expected.firstPageNames,
  SECOND_PAGE_NAMES: sharedFixture.expected.secondPageNames,
  LAPTOP_NAMES: sharedFixture.expected.laptopNames,
  NOVA_NAMES: sharedFixture.expected.novaNames,
};
const actualAdapterExports = {
  PRODUCTS,
  INDEX_SETTINGS,
  FIRST_PAGE_NAMES,
  SECOND_PAGE_NAMES,
  LAPTOP_NAMES,
  NOVA_NAMES,
};
for (const [exportName, expectedValue] of Object.entries(expectedAdapterExports)) {
  const actualValue = actualAdapterExports[exportName];
  assert.deepEqual(actualValue, expectedValue, `${exportName} must map to the shared fixture`);
  assert.ok(Object.isFrozen(actualValue), `${exportName} must preserve its top-level freeze contract`);
}

const fixtureAdapter = readFileSync(
  resolve(sdkDir, 'browser_tests_unmocked/fixture_data.mjs'),
  'utf8',
);
assert.equal(
  fixtureAdapter.match(/\.\.\/fixtures\/official_client_contract\.json/g)?.length,
  1,
  'the browser adapter must load the shared fixture exactly once',
);
const sharedFixtureValues = collectScalarValues(sharedFixture);
const benignAdapterSyntax = 'const product1Name = fixture.products[1].name;';
assert.equal(
  containsFixtureLiteral(benignAdapterSyntax, 'name'),
  false,
  'generic schema values must not make benign adapter property access look like copied fixture data',
);
assert.equal(
  containsFixtureLiteral(benignAdapterSyntax, 1),
  false,
  'single-digit fixture values must not make benign adapter indexes look like copied fixture data',
);
assert.equal(
  containsFixtureLiteral("const id = 'record_1'; // step 1", 1),
  false,
  'digits inside strings and comments are not numeric literals',
);
assert.equal(
  containsFixtureLiteral('const copiedHitCount = 2;', 2),
  true,
  'a copied one-digit expected value must remain detectable',
);
assert.equal(
  containsFixtureLiteral('const copiedCounts = [2];', 2),
  true,
  'a copied one-digit value inside an array literal must remain detectable',
);
assert.equal(
  containsFixtureLiteral('const copiedCount = getCounts()[2];', 2),
  false,
  'a one-digit member-expression index after a call must remain allowed',
);
assert.equal(
  containsFixtureLiteral("const copied = ['fixture value'];", 'fixture value'),
  true,
  'quoted fixture strings must remain detectable',
);
assert.equal(
  containsFixtureLiteral('const copied = 100;', 100),
  true,
  'distinctive numeric fixture values must remain detectable',
);
for (const fixtureValue of sharedFixtureValues) {
  assert.equal(
    containsFixtureLiteral(fixtureAdapter, fixtureValue),
    false,
    `the browser adapter must not retain ${fixtureValue}`,
  );
}
assert.doesNotMatch(
  fixtureAdapter,
  /\?\?|\bcatch\b|\bfallback\b/i,
  'the browser adapter must not embed fallback fixture behavior',
);

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
  sdkContractJob(workflow),
  /name: SDK real-client conformance[^]*npm run test:real_clients/,
  'public CI must run the real-client suite rather than leaving it local-only',
);
assert.match(
  sdkContractJob(workflow),
  /name: Install SDK Playwright browser\n[^]*?timeout-minutes: 5\n[^]*?run: npx playwright install chromium\n[^]*?name: SDK real-client conformance/,
  'public CI must provision the browser in a bounded step before the real-client suite',
);

function sdkContractJob(source) {
  const job = source.match(/^  sdk-contract:\n([\s\S]*?)(?=^  [\w-]+:|$(?![\s\S]))/m);
  assert.ok(job, 'workflow must contain the existing sdk-contract job');
  return job[1];
}

function assertSdkPythonJob(source) {
  const job = sdkContractJob(source);
  const steps = job.split(/^      - /m).slice(1);
  const setup = steps.filter((step) => step.includes('uses: actions/setup-python@'));
  assert.equal(setup.length, 1, 'sdk-contract must set up Python once');
  assert.match(setup[0], /uses: actions\/setup-python@ece7cb06caefa5fff74198d8649806c4678c61a1 # v6\n/);
  assert.match(setup[0], /python-version: '3\.12'\n/);
  const contract = steps.filter((step) => step.includes('python_client_contract_test.sh'));
  assert.equal(contract.length, 1, 'sdk-contract must invoke official Python once');
  assert.match(contract[0], /^name: SDK official Python client contract\n/);
  assert.match(contract[0], /^        run: bash python_client_contract_test\.sh\s*$/m);
  assert.match(contract[0], /^          FLAPJACK_URL: http:\/\/localhost:7700\s*$/m);
  assert.doesNotMatch(contract[0], /FLAPJACK_ADMIN_KEY:/, 'inherit the existing job admin key');
  assert.match(job.split('    steps:')[0], /env:\n      FLAPJACK_ADMIN_KEY: \S+/);
  assert.doesNotMatch(job.split('    steps:')[0], /continue-on-error:/);
  const server = steps.findIndex((step) => step.startsWith('name: Start Flapjack server\n'));
  assert.ok(server >= 0 && server < steps.indexOf(contract[0]), 'reuse the existing running server');
  assert.match(steps[server], /FLAPJACK_ADMIN_KEY="\$FLAPJACK_ADMIN_KEY" \/tmp\/flapjack\/flapjack/);
  const install = steps.findIndex((step) => /^        run: npm ci\s*$/m.test(step));
  assert.ok(install >= 0, 'sdk-contract installs npm dependencies');
  const commands = ['test:runner-shell', 'test:python-client:unit'];
  const regressions = commands.map((command) => {
    const matches = steps.filter((step) => step.includes(`npm run ${command}`));
    assert.equal(matches.length, 1, `${command} must recur once in sdk-contract`);
    assert.match(matches[0], new RegExp(`^        run: npm run ${command}\\s*$`, 'm'));
    assert.ok(steps.indexOf(matches[0]) > install, `${command} follows npm installation`);
    return matches[0];
  });
  for (const step of [contract[0], ...regressions]) {
    assert.ok(steps.indexOf(step) > steps.indexOf(setup[0]), 'Python setup precedes its consumers');
    assert.match(step, /^        working-directory: engine\/sdk_test\s*$/m);
  }
  for (const step of [setup[0], contract[0], ...regressions]) {
    assert.doesNotMatch(step, /^        (?:if|continue-on-error|shell):/m, 'Python gates must run unconditionally and propagate failure');
  }
}

assertSdkPythonJob(workflow);
const pythonInvocation = '        run: bash python_client_contract_test.sh';
const jobWithPython = sdkContractJob(workflow);
const setupBlock = jobWithPython.match(/      - uses: actions\/setup-python@[^]*?python-version: '3\.12'\n/)[0];
const lateSetupJob = jobWithPython.replace(setupBlock, '').replace(
  '      - name: Run contract tests', `${setupBlock}      - name: Run contract tests`,
);
for (const [label, mutatedJob] of [
  ['missing invocation', jobWithPython.replace(pythonInvocation, '        run: echo omitted')],
  ['swallowed failure', jobWithPython.replace(pythonInvocation, `${pythonInvocation} || true`)],
  ['conditional skip', jobWithPython.replace(pythonInvocation, `        if: false\n${pythonInvocation}`)],
  ['continued failure', jobWithPython.replace(pythonInvocation, `        continue-on-error: true\n${pythonInvocation}`)],
  ['setup after invocation', lateSetupJob],
]) {
  assert.throws(() => assertSdkPythonJob(workflow.replace(jobWithPython, mutatedJob)), undefined, label);
}
assert.throws(
  () => assertSdkPythonJob(workflow.replace('  sdk-contract:', '  unrelated-job:') + '\n  sdk-contract:\n    steps: []\n'),
  undefined,
  'Python wiring in an unrelated job must not satisfy sdk-contract',
);
for (const [, scriptPath] of packageJson.scripts['test:runner-shell'].matchAll(/bash (tests\/[^ ]+\.sh)/g)) {
  assert.ok(existsSync(resolve(sdkDir, scriptPath)), `runner regression requires ${scriptPath}`);
}
assert.match(packageJson.scripts['test:runner-shell'], /(?:^| && )bash tests\/python_client_contract_bootstrap_test\.sh(?: && |$)/);
assert.equal(
  packageJson.scripts['test:python-client:unit'],
  '${FLAPJACK_SDK_PYTHON:-python3.12} tests/python_client_contract_unit_test.py',
  'the package script owns the Python unit command and interpreter override',
);
assert.equal(Object.values(packageJson.scripts).filter((script) => script.includes('tests/python_client_contract_unit_test.py')).length, 1);
for (const file of ['python_client_contract_test.sh', 'python_client_contract_test.py', 'requirements-python-client.txt', 'tests/python_client_contract_unit_test.py', 'tests/python_client_contract_bootstrap_test.sh']) {
  assert.ok(existsSync(resolve(sdkDir, file)), `Python gate requires ${file}`);
}

console.log('PASS real-client conformance dependency and recurring-gate wiring');
