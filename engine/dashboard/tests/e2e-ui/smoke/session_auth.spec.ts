import { test, expect } from '@playwright/test';
import {
  captureDashboardSessionCookie,
  DASHBOARD_SESSION_COOKIE_NAME,
  readDocumentCookie,
  readLocalStorageSnapshot,
  replayCapturedSessionCookie,
  unauthenticatedProtectedRouteStatus,
} from '../../fixtures/auth.fixture';
import { TEST_ADMIN_KEY } from '../../fixtures/local-instance';

test.describe('dashboard cookie session', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.removeItem('flapjack-auth');
      localStorage.removeItem('flapjack-api-key');
      localStorage.removeItem('flapjack-app-id');
    });
  });

  test('keeps credentials HttpOnly across reload and revokes them through the UI', async ({
    page,
    context,
    request,
  }) => {
    expect(
      await unauthenticatedProtectedRouteStatus(request),
      'the backend precondition must enforce authentication on /1/indexes',
    ).toBe(403);

    await page.goto('/');
    await expect(page.getByTestId('auth-gate')).toBeVisible();
    await page.getByTestId('auth-key-input').fill(TEST_ADMIN_KEY);
    await page.getByTestId('auth-submit').click();
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({ timeout: 15_000 });

    const localStorageSnapshot = await readLocalStorageSnapshot(page);
    const serializedLocalStorage = JSON.stringify(localStorageSnapshot);
    expect(localStorageSnapshot).not.toHaveProperty('flapjack-api-key');
    expect(serializedLocalStorage).not.toContain(TEST_ADMIN_KEY);

    const sessionCookie = await captureDashboardSessionCookie(context);
    expect(sessionCookie, 'login must issue the named dashboard session cookie').toBeDefined();
    expect(sessionCookie?.httpOnly).toBe(true);
    expect(serializedLocalStorage).not.toContain(sessionCookie?.value ?? 'missing-session-token');

    const scriptVisibleCookies = await readDocumentCookie(page);
    expect(scriptVisibleCookies).not.toContain(DASHBOARD_SESSION_COOKIE_NAME);
    expect(scriptVisibleCookies).not.toContain(sessionCookie?.value ?? 'missing-session-token');

    await page.reload();
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('auth-gate')).toBeHidden();

    await page.getByTitle('Connection Settings').click();
    const connectionDialog = page.getByRole('dialog', { name: 'Connection Settings' });
    await expect(connectionDialog).toBeVisible();
    await connectionDialog.getByPlaceholder('Enter your admin API key').fill('');
    await connectionDialog.getByRole('button', { name: 'Save & Reconnect' }).click();
    await expect(page.getByTestId('auth-gate')).toBeVisible({ timeout: 15_000 });

    const replay = await replayCapturedSessionCookie(request, sessionCookie!);
    expect(replay).toEqual({
      status: 403,
      body: {
        message: 'Invalid Application-ID or API key',
        status: 403,
      },
    });
  });
});
