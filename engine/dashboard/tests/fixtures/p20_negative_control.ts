import type { APIRequestContext, Route } from '@playwright/test';
import { waitForSearchableObjectIds } from './api-helpers';

interface BrowserHealthRouter {
  route(
    url: string,
    handler: (route: Pick<Route, 'fulfill'>) => Promise<void>,
  ): Promise<void>;
}

type Environment = Record<string, string | undefined>;
type SearchabilityWait = typeof waitForSearchableObjectIds;

interface TextOnlyTargetReadinessOptions {
  request: APIRequestContext;
  indexName: string;
  targetObjectId: string;
  waitForSearchability?: SearchabilityWait;
}

/**
 * The single P20 test the text-only negative control may be applied to. The control proves
 * the semantic returned-content assertion fails against a vector-disabled backend, so it must
 * never relax the capability gate for the sibling tests in the same describe.
 */
export const P20_TEXT_ONLY_CONTROL_TEST_TITLE =
  'set search mode to Neural Search and verify persistence';

export function isP20TextOnlyNegativeControl(
  environment: Environment = process.env,
): boolean {
  return environment.P20_TEXT_ONLY_NEGATIVE_CONTROL === '1';
}

// A vector-disabled backend reports `vectorSearch: false`, which hides the browse hybrid
// controls long before the returned-content assertion runs. The control replaces only that
// capability answer so the run fails on the assertion under proof.
export async function installTextOnlyNegativeControlCapability(
  page: BrowserHealthRouter,
): Promise<void> {
  await page.route('**/health', async (route) => {
    await route.fulfill({
      status: 200,
      json: {
        status: 'ok',
        capabilities: { vectorSearch: true, vectorSearchLocal: false },
      },
    });
  });
}

// A vector-disabled backend never answers a neural query, so the control proves the seeded
// target reached the index by keyword instead of waiting forever on neural readiness.
export async function waitForTextOnlyNegativeControlReadiness({
  request,
  indexName,
  targetObjectId,
  waitForSearchability = waitForSearchableObjectIds,
}: TextOnlyTargetReadinessOptions): Promise<void> {
  await waitForSearchability(request, indexName, '', [targetObjectId], {
    hitsPerPage: 10,
    mode: 'keywordSearch',
  });
}
