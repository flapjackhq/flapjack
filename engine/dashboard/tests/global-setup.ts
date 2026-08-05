import * as dotenv from 'dotenv';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

/**
 * Playwright global setup — loads environment variables from the configured
 * secret env file so credentialed tests can access ALGOLIA_APP_ID and
 * ALGOLIA_ADMIN_KEY.
 */
export interface PlaywrightSecretEnvSelection {
  /** Absolute path of the secret env file to load. */
  path: string;
  /**
   * True when the operator named this file through FJ_SECRET_FILE. An explicitly
   * named file is authoritative and replaces ambient credential values; the repo
   * fallback yields to them.
   */
  explicit: boolean;
}

export function resolvePlaywrightSecretEnvPath(
  env: NodeJS.ProcessEnv = process.env,
  testsDir = __dirname,
): PlaywrightSecretEnvSelection | undefined {
  if (env.FJ_NO_SECRET_FILE === '1') {
    return undefined;
  }
  if (env.FJ_SECRET_FILE) {
    return { path: env.FJ_SECRET_FILE, explicit: true };
  }
  return { path: join(testsDir, '..', '..', '.secret', '.env.secret'), explicit: false };
}

export function loadPlaywrightSecretEnv(
  env: NodeJS.ProcessEnv = process.env,
  testsDir = __dirname,
): void {
  const selection = resolvePlaywrightSecretEnvPath(env, testsDir);
  if (!selection) {
    return;
  }
  dotenv.config({
    path: selection.path,
    ...(selection.explicit ? { override: true } : {}),
  });
}

export default function globalSetup() {
  loadPlaywrightSecretEnv();
}
