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

function stepHasAlgoliaEnv(step) {
  const joined = step.lines.join('\n');
  return joined.includes('ALGOLIA_APP_ID:') && joined.includes('ALGOLIA_ADMIN_KEY:');
}

function workflowSteps() {
  return [
    { workflow: 'ci.yml', path: ciWorkflowPath },
    { workflow: 'nightly.yml', path: nightlyWorkflowPath },
  ].flatMap((workflow) => stepBlocks(workflow.path).map((step) => ({ ...step, workflow: workflow.workflow })));
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
