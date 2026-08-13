// Shared Flapjack client config for all SDK tests.
// Loads FLAPJACK_ADMIN_KEY from .env.secret so tests and dev-server use the same key.

import * as dotenv from 'dotenv';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import { createFlapjackSearchClient } from './flapjack_requester.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);


dotenv.config({ path: join(__dirname, '..', '..', '.secret', '.env.secret') });

const FLAPJACK_URL = process.env.FLAPJACK_URL || 'http://localhost:7700';
const FLAPJACK_ADMIN_KEY = process.env.FLAPJACK_ADMIN_KEY || 'fj_test_admin_key_for_local_dev';

export function createFlapjackClient(opts = {}) {
  return createFlapjackSearchClient({
    baseUrl: FLAPJACK_URL,
    apiKey: FLAPJACK_ADMIN_KEY,
    ...opts,
  });
}

export { FLAPJACK_URL, FLAPJACK_ADMIN_KEY };
