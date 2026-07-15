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
export function resolvePlaywrightSecretEnvPath(
  env: NodeJS.ProcessEnv = process.env,
  testsDir = __dirname,
): string | undefined {
  if (env.FJ_NO_SECRET_FILE === '1') {
    return undefined;
  }
  if (env.FJ_SECRET_FILE) {
    return env.FJ_SECRET_FILE;
  }
  return join(testsDir, '..', '..', '.secret', '.env.secret');
}

export function loadPlaywrightSecretEnv(
  env: NodeJS.ProcessEnv = process.env,
  testsDir = __dirname,
): void {
  const secretEnvPath = resolvePlaywrightSecretEnvPath(env, testsDir);
  if (!secretEnvPath) {
    return;
  }
  dotenv.config({ path: secretEnvPath });
}

export default function globalSetup() {
  loadPlaywrightSecretEnv();
}
