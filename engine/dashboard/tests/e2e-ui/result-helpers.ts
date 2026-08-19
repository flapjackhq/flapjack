/**
 */
import { expect, type Locator, type Response } from '@playwright/test';

interface SearchRequestDetails {
  query?: string;
  hasFacets: boolean;
}

/**
 * TODO: Document readSearchRequestDetails.
 */
function readSearchRequestDetails(requestBody: string): SearchRequestDetails {
  try {
    const parsedBody = JSON.parse(requestBody) as Record<string, unknown>;
    const query = typeof parsedBody.q === 'string'
      ? parsedBody.q
      : typeof parsedBody.query === 'string'
      ? parsedBody.query
      : undefined;
    const facets = parsedBody.facets;
    const hasFacets = (Array.isArray(facets) && facets.length > 0)
      || (typeof facets === 'string' && facets.trim().length > 0);

    return { query, hasFacets };
  } catch {
    // Some requests may not be JSON-encoded in exactly the same shape.
  }

  return { query: undefined, hasFacets: false };
}

interface ResponseMatchOptions {
  requireFacets?: boolean;
}

/**
 * TODO: Document responseMatchesIndexQuery.
 */
export function responseMatchesIndexQuery(
  response: Response,
  indexName: string,
  query?: string,
  options: ResponseMatchOptions = {},
): boolean {
  if (response.request().method() !== 'POST') {
    return false;
  }

  const searchPath = `/indexes/${indexName}/search`;
  const queryPath = `/indexes/${indexName}/query`;
  if (!response.url().includes(searchPath) && !response.url().includes(queryPath)) {
    return false;
  }
  if (![200, 202].includes(response.status())) {
    return false;
  }
  if (!query) {
    return true;
  }

  const requestBody = response.request().postData();
  if (!requestBody) {
    return false;
  }

  const requestDetails = readSearchRequestDetails(requestBody);
  if (requestDetails.query !== query) {
    return false;
  }

  if (options.requireFacets && !requestDetails.hasFacets) {
    return false;
  }

  return true;
}

/**
 * TODO: Document extractObjectIdFromText.
 */
export function extractObjectIdFromText(cardText: string): string {
  const normalizedCardText = cardText.replace(/\s+/g, ' ');
  const objectIdLabelMatch = normalizedCardText.match(
    /\bobject\s*id\b\s*[:#]?\s*([a-z0-9._-]+)/i,
  );
  if (objectIdLabelMatch?.[1]) {
    return objectIdLabelMatch[1];
  }

  const idLabelMatch = normalizedCardText.match(
    /\bid\b(?:\s*[:#]\s*|\s+)([a-z0-9._-]+)/i,
  );
  if (idLabelMatch?.[1]) {
    return idLabelMatch[1];
  }

  return normalizedCardText.match(/\b(?:p\d+|prod-\d+)\b/i)?.[0] ?? '';
}

export async function readVisibleObjectId(card: Locator): Promise<string> {
  let objectId = '';
  await expect
    .poll(async () => {
      objectId = extractObjectIdFromText(await card.innerText());
      return objectId;
    }, { timeout: 10_000 })
    .not.toBe('');
  return objectId;
}
