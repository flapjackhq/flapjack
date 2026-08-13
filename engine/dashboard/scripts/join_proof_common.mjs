import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const DASHBOARD_DIR = resolve(HERE, '..');
const ENGINE_DIR = resolve(DASHBOARD_DIR, '..');

export const DEFAULTS = {
  results: resolve(DASHBOARD_DIR, 'test-results/results.json'),
  manifest: resolve(DASHBOARD_DIR, 'tests/e2e-ui/join_proof_manifest.json'),
  features: resolve(ENGINE_DIR, 'docs2/FEATURES.md'),
};

export function die(scriptName, message) {
  process.stderr.write(`${scriptName}: ${message}\n`);
  process.exit(1);
}

export function parseArgs(argv, { defaults = DEFAULTS, booleanFlags = [], valueFlags = [], scriptName }) {
  const out = { ...defaults };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (booleanFlags.includes(flag)) {
      out[flag.slice(2)] = true;
    } else if (valueFlags.includes(flag)) {
      const value = argv[i + 1];
      if (!value) die(scriptName, `${flag} needs a value`);
      out[flag.slice(2)] = resolve(process.cwd(), value);
      i += 1;
    } else {
      die(scriptName, `unknown argument: ${flag}`);
    }
  }
  return out;
}

export function readJsonFile(path, label, scriptName) {
  if (!existsSync(path)) die(scriptName, `${label} not found: ${path}`);
  return JSON.parse(readFileSync(path, 'utf8'));
}

export function readJoinManifest(path, scriptName) {
  return readJsonFile(path, 'manifest', scriptName);
}
