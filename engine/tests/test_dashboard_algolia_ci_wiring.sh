#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_PASSED=$((TESTS_PASSED + 1))
  printf '  [PASS] %s\n' "$1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  printf '  [FAIL] %s\n' "$1"
}

section() {
  printf '\n\033[1m%s\033[0m\n' "$1"
}

run_node_checks() {
  REPO_DIR="$REPO_DIR" node <<'NODE'
const fs = require('fs');
const path = require('path');

const repoDir = process.env.REPO_DIR;
const dashboardDir = path.join(repoDir, 'engine/dashboard');
const packageJsonPath = process.env.DASHBOARD_PACKAGE_JSON_PATH ?? path.join(dashboardDir, 'package.json');
const playwrightConfigPath = process.env.DASHBOARD_PLAYWRIGHT_CONFIG_PATH ?? path.join(dashboardDir, 'playwright.config.ts');
const ciWorkflowPath = process.env.DASHBOARD_CI_WORKFLOW_PATH ?? path.join(repoDir, '.github/workflows/ci.yml');
const nightlyWorkflowPath = process.env.DASHBOARD_NIGHTLY_WORKFLOW_PATH ?? path.join(repoDir, '.github/workflows/nightly.yml');
const migrationSpec = 'tests/e2e-ui/full/migrate-algolia.spec.ts';
const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, 'utf8'));
const playwrightConfig = fs.readFileSync(playwrightConfigPath, 'utf8');

function check(condition, description) {
  console.log(`${condition ? 'pass' : 'fail'}\t${description}`);
}

function normalizeRelativePath(rawPath) {
  return rawPath.replace(/^\.\//, '').replace(/\/$/, '');
}

function yamlScalarValue(rawValue) {
  const trimmed = rawValue.trim();
  if (trimmed.length < 2 || trimmed[0] !== trimmed[trimmed.length - 1]) {
    return trimmed;
  }
  if (trimmed[0] === "'") {
    return trimmed.slice(1, -1).replace(/''/g, "'");
  }
  if (trimmed[0] === '"') {
    try {
      return JSON.parse(trimmed);
    } catch {
      return '';
    }
  }
  return trimmed;
}

function normalizeReportDirectory(rawDirectory) {
  if (!rawDirectory) {
    return '';
  }
  const directory = yamlScalarValue(rawDirectory).replace(/\\/g, '/');
  if (!directory) {
    return '';
  }
  const normalized = path.posix.normalize(directory);
  return normalized === '.' ? '' : normalized.replace(/\/$/, '');
}

function parseProjects(configSource) {
  const projects = [];
  const projectPattern = /\{\s*name:\s*['"]([^'"]+)['"][\s\S]*?testDir:\s*['"]([^'"]+)['"][\s\S]*?\}/g;
  for (const match of configSource.matchAll(projectPattern)) {
    const testMatch = match[0].match(/testMatch:\s*['"]([^'"]+)['"]/);
    projects.push({
      name: match[1],
      testDir: normalizeRelativePath(match[2]),
      testMatch: testMatch?.[1],
    });
  }
  return projects;
}

function projectOwnsSpec(project, specPath) {
  if (project.testMatch && path.basename(specPath) !== project.testMatch) {
    return false;
  }
  return specPath === project.testDir || specPath.startsWith(`${project.testDir}/`);
}

function specOwners(projects, specPath) {
  return projects.filter((project) => projectOwnsSpec(project, specPath));
}

function hasExactlyOneSpecOwner(projects, specPath) {
  return specOwners(projects, specPath).length === 1;
}

function projectNames(projects) {
  return projects.map((project) => project.name).join(', ') || 'none';
}

function listSpecFiles(relativeDir) {
  const absoluteDir = path.join(dashboardDir, relativeDir);
  if (!fs.existsSync(absoluteDir)) {
    return [];
  }
  return fs.readdirSync(absoluteDir, { withFileTypes: true }).flatMap((entry) => {
    const relativePath = `${relativeDir}/${entry.name}`;
    if (entry.isDirectory()) {
      return listSpecFiles(relativePath);
    }
    return entry.name.endsWith('.spec.ts') ? [relativePath] : [];
  });
}

function tokenizeCommand(command) {
  return command.match(/"[^"]+"|'[^']+'|\S+/g)?.map((token) => token.replace(/^['"]|['"]$/g, '')) ?? [];
}

function parsePlaywrightInvocation(command) {
  const tokens = tokenizeCommand(command);
  const projects = [];
  const specSelectors = [];
  let forwardedCliArgs = false;
  tokens.forEach((token, index) => {
    if (token.startsWith('--project=')) {
      projects.push(token.slice('--project='.length));
      return;
    }
    if (token === '--project' && tokens[index + 1]) {
      projects.push(tokens[index + 1]);
      return;
    }
    if (token.startsWith('tests/')) {
      specSelectors.push(normalizeRelativePath(token));
      return;
    }
    if (token === '$@') {
      forwardedCliArgs = true;
    }
  });
  return { projects, specSelectors, forwardedCliArgs };
}

function splitPlaywrightCommands(scriptCommand) {
  return scriptCommand
    .split(/\s+&&\s+/)
    .filter((command) => /\bplaywright\s+test\b/.test(command))
    .map(parsePlaywrightInvocation);
}

function invocationSelectsSpec(invocation, ownerProject, specPath) {
  if (!invocation.projects.includes(ownerProject.name)) {
    return false;
  }
  if (invocation.specSelectors.length === 0) {
    return true;
  }
  return invocation.specSelectors.some((selector) => {
    if (selector.endsWith('/')) {
      return specPath.startsWith(selector);
    }
    const statPath = path.join(dashboardDir, selector);
    if (fs.existsSync(statPath) && fs.statSync(statPath).isDirectory()) {
      return specPath.startsWith(`${selector}/`);
    }
    return specPath === selector;
  });
}

function invocationSelectsNonzeroProject(invocation, project) {
  if (!invocation.projects.includes(project.name)) {
    return false;
  }
  const projectSpecs = listSpecFiles(project.testDir);
  if (invocation.specSelectors.length === 0) {
    return projectSpecs.length > 0;
  }
  return projectSpecs.some((specPath) => invocation.specSelectors.some((selector) => {
    if (selector.endsWith('/')) {
      return specPath.startsWith(selector);
    }
    const statPath = path.join(dashboardDir, selector);
    if (fs.existsSync(statPath) && fs.statSync(statPath).isDirectory()) {
      return specPath.startsWith(`${selector}/`);
    }
    return specPath === selector;
  }));
}

function invocationForwardsCliArgs(invocation) {
  return invocation.forwardedCliArgs === true;
}

function scriptInvocations(scriptName) {
  const command = packageJson.scripts[scriptName];
  return command ? splitPlaywrightCommands(command) : [];
}

function stepBlocks(workflowPath) {
  const lines = fs.readFileSync(workflowPath, 'utf8').split('\n');
  return stepBlocksFromLines(lines);
}

function stepBlocksFromLines(lines) {
  const blocks = [];
  let current = null;
  for (const line of lines) {
    const stepStart = line.match(/^\s*-\s+name:\s*(.+?)\s*$/);
    if (stepStart) {
      if (current) {
        blocks.push(current);
      }
      current = { name: stepStart[1], lines: [line] };
    } else if (current) {
      if (/^\s*-\s+(name|uses):/.test(line)) {
        blocks.push(current);
        current = /^\s*-\s+name:/.test(line) ? { name: line.replace(/^\s*-\s+name:\s*/, '').trim(), lines: [line] } : null;
      } else {
        current.lines.push(line);
      }
    }
  }
  if (current) {
    blocks.push(current);
  }
  return blocks;
}

function leadingSpaces(line) {
  return line.match(/^ */)?.[0].length ?? 0;
}

function unindentBlock(lines) {
  const contentIndent = lines
    .filter((line) => line.trim() !== '')
    .reduce((lowest, line) => Math.min(lowest, leadingSpaces(line)), Infinity);
  const stripWidth = contentIndent === Infinity ? 0 : contentIndent;
  return lines.map((line) => line.slice(Math.min(stripWidth, leadingSpaces(line)))).join('\n').trim();
}

function runCommandFromStep(step) {
  const runLineIndex = step.lines.findIndex((line) => /^\s*run:\s*/.test(line));
  if (runLineIndex === -1) {
    return '';
  }

  const runLine = step.lines[runLineIndex];
  const runIndent = leadingSpaces(runLine);
  const runValue = runLine.replace(/^\s*run:\s*/, '').trim();
  if (!/^[|>][+-]?$/.test(runValue)) {
    return runValue;
  }

  const blockLines = [];
  for (const line of step.lines.slice(runLineIndex + 1)) {
    if (line.trim() !== '' && leadingSpaces(line) <= runIndent) {
      break;
    }
    blockLines.push(line);
  }
  return unindentBlock(blockLines);
}

function npmScriptsFromCommand(command) {
  return [...command.matchAll(/\bnpm\s+run\s+([^\s&]+)/g)].map((match) => match[1]);
}

function npmScriptFromCommand(command) {
  return npmScriptsFromCommand(command)[0];
}

function shellCommandSeparatorWidth(command, index) {
  if (command[index] === ';' || command[index] === '\n') {
    return 1;
  }
  const twoCharacterOperator = command.slice(index, index + 2);
  if (['&&', '||', '|&'].includes(twoCharacterOperator)) {
    return 2;
  }
  return command[index] === '&' || command[index] === '|' ? 1 : 0;
}

function commandSegments(command) {
  const segments = [];
  let segmentStart = 0;
  let quote = null;
  let escaped = false;
  const suspendedQuotes = [];
  let index = 0;

  while (index < command.length) {
    const character = command[index];
    if (escaped) {
      escaped = false;
    } else if (quote === "'") {
      if (character === quote) quote = null;
    } else if (character === '\\') {
      escaped = true;
    } else if (command.slice(index, index + 2) === '$(') {
      suspendedQuotes.push(quote);
      quote = null;
      index += 2;
      continue;
    } else if (quote !== null) {
      if (character === quote) quote = null;
    } else if (character === "'" || character === '"') {
      quote = character;
    } else if (suspendedQuotes.length > 0) {
      if (character === '(') suspendedQuotes.push(null);
      if (character === ')') quote = suspendedQuotes.pop();
    } else {
      const separatorWidth = shellCommandSeparatorWidth(command, index);
      if (separatorWidth > 0) {
        const segment = command.slice(segmentStart, index).trim();
        if (segment !== '') segments.push(segment);
        index += separatorWidth;
        segmentStart = index;
        continue;
      }
    }
    index += 1;
  }

  const finalSegment = command.slice(segmentStart).trim();
  if (finalSegment !== '') segments.push(finalSegment);
  return segments;
}

function commandSegmentRunsPlaywright(commandSegment) {
  if (splitPlaywrightCommands(commandSegment).length > 0) {
    return true;
  }
  return npmScriptsFromCommand(commandSegment)
    .some((scriptName) => scriptInvocations(scriptName).length > 0);
}

function exportedEnvironmentAssignment(tokens, variableName) {
  if (tokens[0] !== 'export') {
    return null;
  }
  for (const token of tokens.slice(1)) {
    const match = token.match(/^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/);
    if (!match) {
      break;
    }
    if (match[1] === variableName) {
      return { value: match[2] };
    }
  }
  return null;
}

function inlinePrefixEnvironmentAssignment(tokens, variableName) {
  for (const token of tokens) {
    if (token === 'env') {
      continue;
    }
    if (token === 'npm' || token === 'npx' || token === 'playwright' || token === 'sh') {
      return null;
    }
    const match = token.match(/^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/);
    if (!match) {
      return null;
    }
    if (match[1] === variableName) {
      return { value: match[2] };
    }
  }
  return null;
}

function playwrightEnvironmentAssignments(command, variableName) {
  // A shell `export VAR=value` (standalone, or chained with `&& runner`) sets VAR
  // in the step's shell for every later command, so it can silently redirect a
  // later Playwright run's HTML report just like an inline `VAR=value runner`
  // prefix. Only report-producing runner segments matter; setup commands such as
  // `npx playwright install` must not stop the scan before the real test command.
  const assignments = [];
  let exportedAssignment = null;
  for (const commandSegment of commandSegments(command)) {
    const tokens = tokenizeCommand(commandSegment);
    if (tokens.length === 0) {
      continue;
    }

    if (tokens[0] === 'export') {
      const assignment = exportedEnvironmentAssignment(tokens, variableName);
      if (assignment !== null) {
        exportedAssignment = assignment;
      }
      continue;
    }

    if (!commandSegmentRunsPlaywright(commandSegment)) {
      continue;
    }

    assignments.push(inlinePrefixEnvironmentAssignment(tokens, variableName) ?? exportedAssignment);
  }
  return assignments;
}

function stepHasAlgoliaEnv(step) {
  const joined = step.lines.join('\n');
  return joined.includes('ALGOLIA_APP_ID:') && joined.includes('ALGOLIA_ADMIN_KEY:');
}

function stepEnvironmentValue(step, variableName) {
  const pattern = new RegExp(`^\\s*${variableName}:\\s*(\\S+)\\s*$`);
  for (const line of step.lines) {
    const match = line.match(pattern);
    if (match) {
      return match[1];
    }
  }
  return '';
}

function stepIncludesPath(step, expectedPath) {
  return step.lines.some((line) => line.trim() === expectedPath);
}

function workflowSteps() {
  return [
    { workflow: 'ci.yml', path: ciWorkflowPath },
    { workflow: 'nightly.yml', path: nightlyWorkflowPath },
  ].flatMap((workflow) => stepBlocks(workflow.path).map((step) => ({ ...step, workflow: workflow.workflow })));
}

function workflowJobBlock(workflowPath, jobName) {
  const lines = fs.readFileSync(workflowPath, 'utf8').split('\n');
  const jobStart = lines.findIndex((line) => line === `  ${jobName}:`);
  if (jobStart === -1) {
    return '';
  }
  const nextJobOffset = lines.slice(jobStart + 1).findIndex((line) => /^  [a-zA-Z0-9_-]+:$/.test(line));
  const jobEnd = nextJobOffset === -1 ? lines.length : jobStart + 1 + nextJobOffset;
  return lines.slice(jobStart, jobEnd).join('\n');
}

function workflowJobSteps(workflowPath, jobName) {
  const jobBlock = workflowJobBlock(workflowPath, jobName);
  return jobBlock ? stepBlocksFromLines(jobBlock.split('\n')) : [];
}

function scopeEnvironmentValue(scopeBlock, envIndent, variableName) {
  const lines = scopeBlock.split('\n');
  const envPattern = new RegExp(`^ {${envIndent}}env:\\s*$`);
  const envStart = lines.findIndex((line) => envPattern.test(line));
  if (envStart === -1) {
    return '';
  }

  const pattern = new RegExp(`^ {${envIndent + 2}}${variableName}:\\s*(\\S+)\\s*$`);
  for (const line of lines.slice(envStart + 1)) {
    if (line.trim() !== '' && leadingSpaces(line) <= envIndent) {
      break;
    }
    const match = line.match(pattern);
    if (match) {
      return match[1];
    }
  }
  return '';
}

function stepPlaywrightInvocations(step) {
  const command = runCommandFromStep(step);
  const directInvocations = splitPlaywrightCommands(command);
  const scriptInvocationsFromCommand = npmScriptsFromCommand(command)
    .flatMap((scriptName) => scriptInvocations(scriptName));
  return [...directInvocations, ...scriptInvocationsFromCommand];
}

function stepRunsPlaywright(step) {
  return stepPlaywrightInvocations(step).length > 0;
}

function configuredHtmlReportDirectory(configSource) {
  const htmlReporter = configSource.match(/\[\s*(['"])html\1\s*,\s*\{([\s\S]*?)\}\s*\]/);
  if (!htmlReporter) {
    return '';
  }
  const outputFolder = htmlReporter[2].match(/\boutputFolder\s*:\s*(['"])(.*?)\1/);
  if (outputFolder) {
    return normalizeReportDirectory(outputFolder[2]);
  }
  if (/\boutputFolder\s*:/.test(htmlReporter[2])) {
    return '';
  }
  return 'playwright-report';
}

function stepHtmlReportDirectories(
  step,
  jobReportDirectory,
  workflowReportDirectory,
  configReportDirectory,
) {
  const command = runCommandFromStep(step);
  const inheritedReportDirectory = normalizeReportDirectory(
    stepEnvironmentValue(step, 'PLAYWRIGHT_HTML_OUTPUT_DIR'),
  )
    || jobReportDirectory
    || workflowReportDirectory
    || configReportDirectory;
  return playwrightEnvironmentAssignments(command, 'PLAYWRIGHT_HTML_OUTPUT_DIR')
    .map((commandAssignment) => (
      commandAssignment === null
        ? inheritedReportDirectory
        : normalizeReportDirectory(commandAssignment.value) || configReportDirectory
    ));
}

function stepSelectsMigrationSpec(step, ownerProjects, specPath) {
  const scriptNames = npmScriptsFromCommand(runCommandFromStep(step));
  if (scriptNames.length === 0) {
    return false;
  }
  return scriptNames.some((scriptName) => (
    scriptInvocations(scriptName).some((invocation) => (
      ownerProjects.some((ownerProject) => invocationSelectsSpec(invocation, ownerProject, specPath))
    ))
  ));
}

const projects = parseProjects(playwrightConfig);
const migrationOwners = specOwners(projects, migrationSpec);
const migrationOwner = migrationOwners[0];
const apiProject = projects.find((project) => listSpecFiles(project.testDir).some((specPath) => specPath.startsWith('tests/e2e-api/')));
const integrationInvocations = scriptInvocations('test:integration');
const integrationStepNames = workflowSteps().filter((step) => (
  step.name === 'Run integration tests'
  && npmScriptFromCommand(runCommandFromStep(step))
));
const migrationSelectingSteps = workflowSteps().filter((step) => stepSelectsMigrationSpec(step, migrationOwners, migrationSpec));
const dashboardBuildSteps = workflowJobSteps(ciWorkflowPath, 'dashboard-build');
const auditFixtureStepIndex = dashboardBuildSteps.findIndex((step) => (
  runCommandFromStep(step) === 'bash scripts/audit_gate_fixture_test.sh'
  && step.lines.some((line) => line.trim() === 'working-directory: engine/dashboard')
));
const liveAuditStepIndex = dashboardBuildSteps.findIndex((step) => (
  runCommandFromStep(step) === 'bash scripts/audit_gate.sh'
  && step.lines.some((line) => line.trim() === 'working-directory: engine/dashboard')
));
const dashboardFullE2eJob = workflowJobBlock(ciWorkflowPath, 'dashboard-full-e2e');
const nightlyPageStep = workflowSteps().find((step) => (
  step.workflow === 'nightly.yml' && step.name === 'Run page tests'
));
const nightlyDashboardJob = workflowJobBlock(nightlyWorkflowPath, 'dashboard-all');
const nightlyDashboardSteps = workflowJobSteps(nightlyWorkflowPath, 'dashboard-all');
const nightlyJobReportDirectory = normalizeReportDirectory(
  scopeEnvironmentValue(nightlyDashboardJob, 4, 'PLAYWRIGHT_HTML_OUTPUT_DIR'),
);
const nightlyWorkflowReportDirectory = normalizeReportDirectory(
  scopeEnvironmentValue(
    fs.readFileSync(nightlyWorkflowPath, 'utf8'),
    0,
    'PLAYWRIGHT_HTML_OUTPUT_DIR',
  ),
);
const configReportDirectory = configuredHtmlReportDirectory(playwrightConfig);
const nightlyPageStepIndex = nightlyDashboardSteps.findIndex((step) => step.name === 'Run page tests');
const laterNightlyPlaywrightSteps = nightlyPageStepIndex === -1
  ? []
  : nightlyDashboardSteps.slice(nightlyPageStepIndex + 1).filter(stepRunsPlaywright);
const nightlyReportUploadStep = workflowSteps().find((step) => (
  step.workflow === 'nightly.yml' && step.name === 'Upload test reports'
));
const nightlyPageReportDirectory = nightlyPageStep
  ? normalizeReportDirectory(stepEnvironmentValue(nightlyPageStep, 'PLAYWRIGHT_HTML_OUTPUT_DIR'))
  : '';

check(
  hasExactlyOneSpecOwner(projects, migrationSpec),
  `Playwright config has exactly one project owning tests/e2e-ui/full/migrate-algolia.spec.ts (observed: ${projectNames(migrationOwners)})`,
);
check(Boolean(apiProject), 'Playwright config has a nonzero API project denominator');
check(
  Boolean(apiProject) && integrationInvocations.some((invocation) => invocationSelectsNonzeroProject(invocation, apiProject)),
  'test:integration selects a nonzero API integration group',
);
check(
  migrationOwners.length === 1 && integrationInvocations.some((invocation) => invocationSelectsSpec(invocation, migrationOwner, migrationSpec)),
  'test:integration selects the project/spec owning tests/e2e-ui/full/migrate-algolia.spec.ts',
);
check(
  integrationInvocations.length > 0 && integrationInvocations.every(invocationForwardsCliArgs),
  'test:integration forwards npm CLI args to every Playwright leg',
);
check(
  integrationStepNames.length === 2 && integrationStepNames.every((step) => npmScriptFromCommand(runCommandFromStep(step)) === 'test:integration'),
  'credentialed Run integration tests steps in ci.yml and nightly.yml execute npm run test:integration',
);
check(
  integrationStepNames.length === 2 && integrationStepNames.every(stepHasAlgoliaEnv),
  'Run integration tests steps carry ALGOLIA_APP_ID and ALGOLIA_ADMIN_KEY',
);
check(
  migrationSelectingSteps.length > 0 && migrationSelectingSteps.every(stepHasAlgoliaEnv),
  'every workflow step selecting migrate-algolia.spec.ts carries ALGOLIA_APP_ID and ALGOLIA_ADMIN_KEY',
);
check(
  auditFixtureStepIndex !== -1
    && liveAuditStepIndex !== -1
    && auditFixtureStepIndex < liveAuditStepIndex,
  'dashboard-build runs the deterministic audit fixture suite before the live production audit',
);
check(
  nightlyPageReportDirectory !== '' && nightlyPageReportDirectory !== 'playwright-report',
  'nightly page tests write HTML evidence outside the later integration report directory',
);
check(
  nightlyPageStepIndex !== -1
    && laterNightlyPlaywrightSteps.length > 0
    && laterNightlyPlaywrightSteps.every((step) => {
      const reportDirectories = stepHtmlReportDirectories(
        step,
        nightlyJobReportDirectory,
        nightlyWorkflowReportDirectory,
        configReportDirectory,
      );
      return reportDirectories.length > 0 && reportDirectories.every((reportDirectory) => (
        reportDirectory !== '' && reportDirectory !== nightlyPageReportDirectory
      ));
    }),
  `later nightly Playwright steps do not write HTML evidence into ${nightlyPageReportDirectory || 'the page report directory'}`,
);
check(
  Boolean(nightlyReportUploadStep)
    && stepIncludesPath(nightlyReportUploadStep, `engine/dashboard/${nightlyPageReportDirectory}/`)
    && stepIncludesPath(nightlyReportUploadStep, 'engine/dashboard/playwright-report/'),
  'nightly dashboard artifact uploads both page and integration HTML reports',
);
check(
  dashboardFullE2eJob.includes('FLAPJACK_NODE_ID=dashboard-e2e')
    && dashboardFullE2eJob.includes('FLAPJACK_ADVERTISE_ADDR=https://dashboard-e2e.invalid:7700')
    && dashboardFullE2eJob.includes('FLAPJACK_REPLICATION_API_KEY=fj_devtestadminkey000000'),
  'dashboard-full-e2e starts a deterministic replication-enabled backend for cluster peer specs',
);
check(
  !hasExactlyOneSpecOwner([
    { name: 'synthetic-e2e-ui-a', testDir: 'tests/e2e-ui' },
    { name: 'synthetic-e2e-ui-b', testDir: 'tests/e2e-ui/full', testMatch: 'migrate-algolia.spec.ts' },
  ], migrationSpec),
  'contract self-test detects duplicate Playwright owners for tests/e2e-ui/full/migrate-algolia.spec.ts',
);
check(
  stepSelectsMigrationSpec({
    name: 'Synthetic block scalar migration step',
    lines: [
      '      - name: Synthetic block scalar migration step',
      '        working-directory: engine/dashboard',
      '        run: |',
      '          npm run test:integration',
    ],
  }, migrationOwners, migrationSpec),
  'contract self-test detects GitHub Actions block-scalar run steps that select migrate-algolia.spec.ts',
);
check(
  stepHtmlReportDirectories({
    name: 'Synthetic inline report environment step',
    lines: [
      '      - name: Synthetic inline report environment step',
      '        working-directory: engine/dashboard',
      '        run: PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages npm run test:integration',
    ],
  }, 'job-report', 'workflow-report', 'config-report').join(',') === 'playwright-report-pages',
  'contract self-test detects inline Playwright HTML report environment assignments',
);
check(
  stepHtmlReportDirectories({
    name: 'Synthetic exported report environment step',
    lines: [
      '      - name: Synthetic exported report environment step',
      '        working-directory: engine/dashboard',
      '        run: |',
      '          export PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages',
      '          npm run test:integration',
    ],
  }, 'job-report', 'workflow-report', 'config-report').join(',') === 'playwright-report-pages',
  'contract self-test detects exported Playwright HTML report environment assignments',
);
check(
  stepHtmlReportDirectories({
    name: 'Synthetic later inline report environment step',
    lines: [
      '      - name: Synthetic later inline report environment step',
      '        working-directory: engine/dashboard',
      '        run: |',
      '          npx playwright install --with-deps chromium',
      '          PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages npm run test:integration',
    ],
  }, 'job-report', 'workflow-report', 'config-report').join(',') === 'playwright-report-pages',
  'contract self-test keeps scanning until the Playwright test runner segment',
);
check(
  stepHtmlReportDirectories({
    name: 'Synthetic empty inline report environment step',
    lines: [
      '      - name: Synthetic empty inline report environment step',
      '        working-directory: engine/dashboard',
      '        run: |',
      '          export PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages',
      '          npx playwright install --with-deps chromium',
      '          PLAYWRIGHT_HTML_OUTPUT_DIR= npm run test:integration',
    ],
  }, 'job-report', 'workflow-report', 'config-report').join(',') === 'config-report',
  'contract self-test treats empty inline Playwright HTML report assignment as an override',
);
check(
  stepHtmlReportDirectories({
    name: 'Synthetic multiple Playwright runners step',
    lines: [
      '      - name: Synthetic multiple Playwright runners step',
      '        working-directory: engine/dashboard',
      '        run: |',
      '          export PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages',
      '          PLAYWRIGHT_HTML_OUTPUT_DIR= npm run test:integration',
      '          npm run test:integration',
    ],
  }, 'job-report', 'workflow-report', 'config-report').join(',')
    === 'config-report,playwright-report-pages',
  'contract self-test inspects every Playwright runner in a step',
);
check(
  stepHtmlReportDirectories({
    name: 'Synthetic same-line Playwright runners step',
    lines: [
      '      - name: Synthetic same-line Playwright runners step',
      '        working-directory: engine/dashboard',
      '        run: PLAYWRIGHT_HTML_OUTPUT_DIR= npm run test:integration ; PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages npm run test:integration',
    ],
  }, 'job-report', 'workflow-report', 'config-report').join(',')
    === 'config-report,playwright-report-pages',
  'contract self-test inspects semicolon-separated Playwright runners',
);
check(
  ['&&', '||', '&', '|', '|&'].every((operator) => (
    stepHtmlReportDirectories({
      name: `Synthetic ${operator} Playwright runners step`,
      lines: [
        `      - name: Synthetic ${operator} Playwright runners step`,
        '        working-directory: engine/dashboard',
        `        run: PLAYWRIGHT_HTML_OUTPUT_DIR= npm run test:integration ${operator} PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages npm run test:integration`,
      ],
    }, 'job-report', 'workflow-report', 'config-report').join(',')
      === 'config-report,playwright-report-pages'
  )),
  'contract self-test inspects shell-separated Playwright runners',
);

console.log(`note\tworkflow steps selecting migration spec: ${migrationSelectingSteps.map((step) => `${step.workflow}:${step.name}`).join(', ') || 'none'}`);
NODE
}

section "Dashboard Algolia CI wiring"
while IFS=$'\t' read -r status description; do
  case "$status" in
    pass)
      pass "$description"
      ;;
    fail)
      fail "$description"
      ;;
    note)
      printf '  [NOTE] %s\n' "$description"
      ;;
  esac
done < <(run_node_checks)

printf '\n\033[1mResults: %d/%d passed\033[0m\n' "$TESTS_PASSED" "$TESTS_RUN"
if [ "$TESTS_FAILED" -gt 0 ]; then
  printf '\033[0;31m%d test(s) failed\033[0m\n' "$TESTS_FAILED"
  exit 1
fi
printf '\033[0;32mAll tests passed\033[0m\n'
