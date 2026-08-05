/**
 * Auth flow E2E tests — real browser, real server.
 *
 * These tests verify the dashboard authentication gate:
 * - Shows auth screen when no API key is configured
 * - Rejects invalid keys with a clear error
 * - Accepts valid keys and loads the dashboard
 *
 * NOTE: This test does NOT use the auth fixture — it tests the
 * unauthenticated-to-authenticated flow from scratch.
 *
 * The dashboard now authenticates with an HttpOnly session cookie minted at
 * POST /1/dashboard/session, so the browser sends no key material of its own.
 * The dev server may run in open mode (unauthenticated /1/indexes returns 200,
 * and the session route is not even mounted), which would hide the auth gate
 * entirely. Both endpoints are therefore intercepted so this spec exercises the
 * auth *UI* flow deterministically; the server-side cookie contract is owned by
 * `smoke/session_auth.spec.ts` and the Rust transport contracts. All other
 * requests hit the real server.
 */
import { test, expect } from '@playwright/test';
import { API_HEADERS } from '../helpers';
import { readLocalStorageSnapshot } from '../../fixtures/auth.fixture';

// Use raw test (no auth fixture) — we're testing the auth gate itself
const ADMIN_KEY = API_HEADERS['x-algolia-api-key'];

const INVALID_CREDENTIALS_BODY = JSON.stringify({
  message: 'Invalid Application-ID or API key',
  status: 403,
});

test.describe('Auth Gate', () => {
  // Whether the simulated server currently holds a session for this page. Playwright
  // runs a worker's tests one at a time, and beforeEach resets it, so no test can
  // observe another's session.
  let sessionIsActive = false;

  test.beforeEach(async ({ page }) => {
    sessionIsActive = false;

    // Clear credential state a pre-session dashboard build may have left behind.
    await page.addInitScript(() => {
      localStorage.removeItem('flapjack-api-key');
      localStorage.removeItem('flapjack-app-id');
      localStorage.removeItem('flapjack-auth');
    });

    // Stand in for the session endpoint: the admin key mints a session, anything
    // else is rejected with the server's real invalid-credentials body.
    await page.route('**/1/dashboard/session', async (route) => {
      const pending = route.request();
      if (pending.method() === 'DELETE') {
        sessionIsActive = false;
        await route.fulfill({ status: 204, body: '' });
        return;
      }
      const submitted = pending.postDataJSON() as { apiKey?: string } | null;
      if (submitted?.apiKey === ADMIN_KEY) {
        sessionIsActive = true;
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ authenticated: true }),
        });
        return;
      }
      await route.fulfill({
        status: 403,
        contentType: 'application/json',
        body: INVALID_CREDENTIALS_BODY,
      });
    });

    // Stand in for cookie authentication on the endpoint AuthGate probes: with a
    // session the request reaches the real server carrying the admin key the cookie
    // resolves to server-side, without one it is rejected.
    await page.route('**/1/indexes', async (route) => {
      if (!sessionIsActive) {
        await route.fulfill({
          status: 403,
          contentType: 'application/json',
          body: INVALID_CREDENTIALS_BODY,
        });
        return;
      }
      await route.continue({
        headers: { ...route.request().headers(), 'x-algolia-api-key': ADMIN_KEY },
      });
    });
  });

  test('shows auth screen when no API key is configured', async ({ page }) => {
    await page.goto('/');

    // Should see the auth gate
    const authGate = page.getByTestId('auth-gate');
    await expect(authGate).toBeVisible();

    // Should show the Flapjack branding
    await expect(authGate.getByText('Welcome to Flapjack')).toBeVisible();

    // Should have an API key input
    const input = page.getByTestId('auth-key-input');
    await expect(input).toBeVisible();

    // Should have a connect button (disabled without input)
    const submitBtn = page.getByTestId('auth-submit');
    await expect(submitBtn).toBeVisible();
    await expect(submitBtn).toBeDisabled();

    // Should show help text about finding the key
    const helpText = page.getByTestId('auth-help');
    await expect(helpText).toBeVisible();
    await expect(helpText).toContainText(
      'flapjack --data-dir <data-dir> reset-admin-key'
    );
  });

  test('rejects invalid API key with error message', async ({ page }) => {
    await page.goto('/');

    const authGate = page.getByTestId('auth-gate');
    await expect(authGate).toBeVisible();

    // Type an invalid key
    const input = page.getByTestId('auth-key-input');
    await input.fill('wrong_key_12345');

    // Submit
    const submitBtn = page.getByTestId('auth-submit');
    await expect(submitBtn).toBeEnabled();
    await submitBtn.click();

    // Should show error
    const error = page.getByTestId('auth-error');
    await expect(error).toBeVisible({ timeout: 10_000 });
    await expect(error).toContainText('Invalid API key');

    // Should still be on the auth gate (not redirected)
    await expect(authGate).toBeVisible();
  });

  test('accepts valid API key and loads the dashboard', async ({ page }) => {
    await page.goto('/');

    const authGate = page.getByTestId('auth-gate');
    await expect(authGate).toBeVisible();

    // Type the correct admin key
    const input = page.getByTestId('auth-key-input');
    await input.fill(ADMIN_KEY);

    // Submit
    const submitBtn = page.getByTestId('auth-submit');
    await submitBtn.click();

    // Should show success state briefly
    const success = page.getByTestId('auth-success');
    await expect(success).toBeVisible({ timeout: 10_000 });

    // After reload, should see the dashboard (Overview page)
    // The page reloads after auth — wait for the Overview heading
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({ timeout: 15_000 });

    // Auth gate should no longer be visible
    await expect(authGate).toBeHidden();
  });

  test('connect button enables only when key input is non-empty', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('auth-gate')).toBeVisible();

    const input = page.getByTestId('auth-key-input');
    const submitBtn = page.getByTestId('auth-submit');

    // Initially disabled
    await expect(submitBtn).toBeDisabled();

    // Type something
    await input.fill('a');
    await expect(submitBtn).toBeEnabled();

    // Clear it
    await input.fill('');
    await expect(submitBtn).toBeDisabled();
  });

  test('stays signed in across page reloads without storing the key', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('auth-gate')).toBeVisible();

    // Authenticate
    await page.getByTestId('auth-key-input').fill(ADMIN_KEY);
    await page.getByTestId('auth-submit').click();

    // Wait for dashboard to load after auth
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({ timeout: 15_000 });

    // The session — not a stored credential — is what survives. Nothing the browser
    // persisted may contain the admin key, and no restoring init script is needed.
    const afterLogin = await readLocalStorageSnapshot(page);
    expect(Object.values(afterLogin).join('\n')).not.toContain(ADMIN_KEY);

    // Reload the page — should go straight to dashboard (no auth gate)
    await page.reload();
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('auth-gate')).toBeHidden();

    const afterReload = await readLocalStorageSnapshot(page);
    expect(Object.values(afterReload).join('\n')).not.toContain(ADMIN_KEY);
  });
});
