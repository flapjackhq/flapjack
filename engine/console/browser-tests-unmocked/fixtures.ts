import AxeBuilder from '@axe-core/playwright';
import type { APIRequestContext, AriaRole, Locator, Page } from '@playwright/test';
import { expect } from '@playwright/test';
import { requireOwnedTestBackend } from './local_backend';

export const ADMIN_KEY = process.env.FJ_CONSOLE_ADMIN_KEY ?? 'fj_devtestadminkey000000';
export const LONG_RUST_TITLE =
  'RustAsyncBookReferenceWithAnIntentionallyLongUnbrokenTokenForTheExact390PixelViewportProof0123456789';

const API_HEADERS = {
  'x-algolia-application-id': 'flapjack',
  'x-algolia-api-key': ADMIN_KEY,
  'Content-Type': 'application/json',
};

export type SeededIndex = { name: string; entries: number; dataSize: number };
export type SecuritySource = { source: string; description: string };

export type CapturedSearchExchange = {
  requestBody: Record<string, unknown>;
  userToken: string | null;
  responseOk: boolean;
  responseQueryId: unknown;
};

export async function captureNextSearchExchange(
  page: Page,
  indexName: string
): Promise<CapturedSearchExchange> {
  const response = await page.waitForResponse(
    (candidate) =>
      candidate.request().method() === 'POST' &&
      candidate.url().endsWith(`/1/indexes/${indexName}/query`)
  );
  const request = response.request();
  const responseBody = (await response.json()) as { queryID?: unknown };
  return {
    requestBody: request.postDataJSON() as Record<string, unknown>,
    userToken: await request.headerValue('x-algolia-usertoken'),
    responseOk: response.ok(),
    responseQueryId: responseBody.queryID,
  };
}

export async function captureNextResultOpenEvent(page: Page): Promise<{
  requestBody: Record<string, unknown>;
  responseOk: boolean;
}> {
  const response = await page.waitForResponse(
    (candidate) =>
      candidate.request().method() === 'POST' && candidate.url().endsWith('/1/events')
  );
  return {
    requestBody: response.request().postDataJSON() as Record<string, unknown>,
    responseOk: response.ok(),
  };
}

export async function seedIndex(request: APIRequestContext, projectName: string): Promise<SeededIndex> {
  requireOwnedTestBackend();
  const name = `p4a.console-${projectName}`;
  await request.delete(`/1/indexes/${name}`, { headers: API_HEADERS }).catch(() => undefined);

  const response = await request.post(`/1/indexes/${name}/batch`, {
    headers: API_HEADERS,
    data: {
      requests: [
        { action: 'addObject', body: { objectID: 'rust-language', title: 'Rust Programming Language' } },
        { action: 'addObject', body: { objectID: 'rust-async', title: LONG_RUST_TITLE } },
        ...Array.from({ length: 19 }, (_, index) => ({
          action: 'addObject',
          body: { objectID: `filler-${index + 1}`, title: `Reference manual ${index + 1}` },
        })),
      ],
    },
  });
  expect(response.ok(), 'fixture must seed the real engine').toBeTruthy();

  const settings = await request.put(`/1/indexes/${name}/settings`, {
    headers: API_HEADERS,
    data: {
      embedders: {
        remote: {
          source: 'rest',
          url: 'https://semantic-fixture.invalid/embed',
          request: { input: '{{text}}' },
          response: { embedding: '{{embedding}}' },
          dimensions: 3,
        },
      },
    },
  });
  expect(settings.ok(), 'fixture must configure one query-capable embedder').toBeTruthy();

  let exact: SeededIndex | undefined;
  await expect
    .poll(async () => {
      const list = await request.get('/1/indexes', { headers: API_HEADERS });
      if (!list.ok()) return undefined;
      const body = (await list.json()) as { items?: SeededIndex[] };
      exact = body.items?.find((item) => item.name === name);
      return exact?.entries;
    })
    .toBe(21);

  return exact!;
}

export async function removeIndex(request: APIRequestContext, name: string): Promise<void> {
  requireOwnedTestBackend();
  await request.delete(`/1/indexes/${name}`, { headers: API_HEADERS }).catch(() => undefined);
}

export async function removeQueryEmbedders(
  request: APIRequestContext,
  name: string
): Promise<void> {
  requireOwnedTestBackend();
  const response = await request.put(`/1/indexes/${name}/settings`, {
    headers: API_HEADERS,
    data: { embedders: {} },
  });
  expect(response.ok(), 'fixture must remove the exact test embedder').toBeTruthy();

  const readback = await request.get(`/1/indexes/${name}/settings`, { headers: API_HEADERS });
  expect(readback.ok(), 'fixture must read back cleared settings').toBeTruthy();
  const body = (await readback.json()) as Record<string, unknown>;
  expect(body, 'fixture must prove no embedder can receive the fallback query').not.toHaveProperty(
    'embedders'
  );
}

export async function readSecuritySources(
  request: APIRequestContext
): Promise<SecuritySource[]> {
  requireOwnedTestBackend();
  const response = await request.get('/1/security/sources', { headers: API_HEADERS });
  expect(response.ok(), 'fixture must read the engine-global security allowlist').toBeTruthy();
  const body: unknown = await response.json();
  expect(Array.isArray(body), 'fixture must receive the security-source array').toBeTruthy();
  for (const entry of body as unknown[]) {
    expect(
      typeof entry === 'object' &&
        entry !== null &&
        typeof (entry as Record<string, unknown>).source === 'string' &&
        typeof (entry as Record<string, unknown>).description === 'string',
      'fixture must receive exact security-source records'
    ).toBeTruthy();
  }
  return body as SecuritySource[];
}

export async function replaceSecuritySources(
  request: APIRequestContext,
  sources: SecuritySource[]
): Promise<void> {
  requireOwnedTestBackend();
  const response = await request.put('/1/security/sources', {
    headers: API_HEADERS,
    data: sources,
  });
  expect(response.ok(), 'fixture must replace only the owned engine allowlist').toBeTruthy();
  const body = (await response.json()) as { updatedAt?: unknown };
  expect(
    typeof body.updatedAt === 'string' && Number.isFinite(Date.parse(body.updatedAt)),
    'fixture must receive the allowlist update timestamp'
  ).toBeTruthy();
  expect(await readSecuritySources(request)).toEqual(sources);
}

const VISIBLE_UI_ROLES: AriaRole[] = [
  'alert',
  'article',
  'button',
  'cell',
  'checkbox',
  'columnheader',
  'combobox',
  'dialog',
  'heading',
  'link',
  'list',
  'listitem',
  'main',
  'navigation',
  'region',
  'row',
  'search',
  'searchbox',
  'slider',
  'spinbutton',
  'status',
  'table',
  'textbox',
];

export async function removeApiKeysByDescription(
  request: APIRequestContext,
  description: string
): Promise<void> {
  requireOwnedTestBackend();
  const response = await request.get('/1/keys', { headers: API_HEADERS });
  expect(response.ok(), 'fixture must list API keys before cleanup').toBeTruthy();
  const body = (await response.json()) as {
    keys?: Array<{ value?: unknown; description?: unknown }>;
  };
  expect(Array.isArray(body.keys), 'fixture must receive the API key collection').toBeTruthy();
  for (const key of body.keys!) {
    if (key.description === description && typeof key.value === 'string') {
      const deleted = await request.delete(`/1/keys/${encodeURIComponent(key.value)}`, {
        headers: API_HEADERS,
      });
      expect(deleted.ok(), 'fixture must delete the exact test API key').toBeTruthy();
    }
  }
  const verified = await request.get('/1/keys', { headers: API_HEADERS });
  expect(verified.ok(), 'fixture must verify API key cleanup').toBeTruthy();
  const verifiedBody = (await verified.json()) as {
    keys?: Array<{ description?: unknown }>;
  };
  expect(
    Array.isArray(verifiedBody.keys),
    'fixture must receive the API key collection after cleanup'
  ).toBeTruthy();
  expect(
    verifiedBody.keys!.some((key) => key.description === description),
    'fixture must leave no API key with the exact test description'
  ).toBeFalsy();
}

export async function expectVisibleUiWithinViewport(
  page: Page,
  additionalLocators: Locator[] = []
): Promise<void> {
  const viewport = page.viewportSize();
  expect(viewport, 'browser project must declare a viewport').not.toBeNull();
  const roleLocators: Locator[] = [];
  for (const role of VISIBLE_UI_ROLES) {
    for (const locator of await page.getByRole(role).all()) {
      if (await locator.isVisible()) roleLocators.push(locator);
    }
  }
  for (const locator of [...roleLocators, ...additionalLocators]) {
    const box = await locator.boundingBox();
    expect(box, 'visible control must have a rendered box').not.toBeNull();
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(viewport!.width);
  }
}

export async function expectAccessible(page: Page): Promise<void> {
  const result = await new AxeBuilder({ page }).analyze();
  expect(result.violations).toEqual([]);
}
