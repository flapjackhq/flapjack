import {
  test as base,
  type APIRequestContext,
  type APIResponse,
  type BrowserContext,
  type Page,
} from '@playwright/test';
import { API_BASE, TEST_ADMIN_KEY } from './local-instance';

export const DASHBOARD_SESSION_COOKIE_NAME = 'flapjack_dashboard_session';

export type CapturedBrowserCookie = Awaited<ReturnType<BrowserContext['cookies']>>[number];

type DashboardFixtures = { readClipboard: () => Promise<string> };
type DashboardSessionCookie = Parameters<BrowserContext['addCookies']>[0][number];
type DashboardWorkerFixtures = { dashboardSessionCookie: DashboardSessionCookie };

/** Authenticated browser fixture backed by the dashboard's HttpOnly session. */
export const test = base.extend<DashboardFixtures, DashboardWorkerFixtures>({
  dashboardSessionCookie: [async ({ playwright }, use) => {
    const request = await playwright.request.newContext({ baseURL: API_BASE });
    try {
      await use(await exchangeDashboardSessionCookie(request));
    } finally {
      await request.dispose();
    }
  }, { scope: 'worker' }],

  page: async ({ page, context, dashboardSessionCookie }, use) => {
    await context.addCookies([dashboardSessionCookie]);
    await use(page);
  },

  readClipboard: async ({ context }, use) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await use(async () => {
      const page = context.pages()[0];
      return page.evaluate(() => navigator.clipboard.readText());
    });
  },
});

async function exchangeDashboardSessionCookie(
  request: APIRequestContext,
): Promise<DashboardSessionCookie> {
  const response = await request.post('/1/dashboard/session', {
    data: { apiKey: TEST_ADMIN_KEY },
  });
  if (!response.ok()) {
    throw new Error(`dashboard session exchange failed with ${response.status()}`);
  }
  return parseDashboardSessionCookie(response);
}

function parseDashboardSessionCookie(response: APIResponse): DashboardSessionCookie {
  const setCookie = response
    .headersArray()
    .find((header) => header.name.toLowerCase() === 'set-cookie')?.value;
  if (!setCookie) {
    throw new Error('dashboard session exchange did not return Set-Cookie');
  }
  const [nameValue, ...attributes] = setCookie.split(';').map((part) => part.trim());
  const [name, value] = nameValue.split('=', 2);
  if (name !== DASHBOARD_SESSION_COOKIE_NAME || !value) {
    throw new Error('dashboard session exchange returned an invalid cookie');
  }
  const normalizedAttributes = attributes.map((attribute) => attribute.toLowerCase());
  return {
    name,
    value,
    url: new URL(response.url()).origin,
    httpOnly: normalizedAttributes.includes('httponly'),
    secure: normalizedAttributes.includes('secure'),
    sameSite: 'Strict',
  };
}

export async function readLocalStorageSnapshot(page: Page): Promise<Record<string, string>> {
  return page.evaluate(() => Object.fromEntries(
    Array.from({ length: localStorage.length }, (_, index) => localStorage.key(index))
      .filter((key): key is string => key !== null)
      .map((key) => [key, localStorage.getItem(key) ?? '']),
  ));
}

export async function readDocumentCookie(page: Page): Promise<string> {
  return page.evaluate(() => document.cookie);
}

export async function captureDashboardSessionCookie(
  context: BrowserContext,
): Promise<CapturedBrowserCookie | undefined> {
  const cookies = await context.cookies();
  return cookies.find((cookie) => cookie.name === DASHBOARD_SESSION_COOKIE_NAME);
}

export async function unauthenticatedProtectedRouteStatus(
  request: APIRequestContext,
): Promise<number> {
  const response = await request.get('/1/indexes', {
    headers: { 'x-algolia-application-id': 'flapjack' },
  });
  return response.status();
}

export async function replayCapturedSessionCookie(
  request: APIRequestContext,
  cookie: CapturedBrowserCookie,
): Promise<{ status: number; body: unknown }> {
  const response = await request.get('/1/indexes', {
    headers: {
      cookie: `${cookie.name}=${cookie.value}`,
      'x-algolia-application-id': 'flapjack',
    },
  });
  return {
    status: response.status(),
    body: await response.json(),
  };
}

export { expect } from '@playwright/test';
