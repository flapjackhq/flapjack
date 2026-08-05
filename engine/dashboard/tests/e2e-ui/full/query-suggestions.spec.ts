/**
 * E2E-UI Full Suite — Query Suggestions Page (Real Server)
 *
 * NON-MOCKED SIMULATED-HUMAN REAL-BROWSER TESTS.
 * Tests run against a REAL Flapjack server.
 *
 * Covers:
 * - Page loads with heading and Create Config button
 * - Seeded config renders in list (load-and-verify rule)
 * - Create config dialog shows required form fields
 * - Create config via dialog → appears in list with source info
 * - Config card shows source, status fields, and action buttons
 * - Rebuild button is enabled and triggers a build (toast visible)
 * - Delete config via confirm dialog → removed from list
 * - Cancel in create dialog closes without creating
 * - Sidebar nav link navigates to the page
 */
import { test, expect } from '../../fixtures/auth.fixture';
import type { Page } from '@playwright/test';

const QS_SOURCE = 'e2e-products';
const QS_PAGE_HEADING = { name: 'Query Suggestions', exact: true, level: 2 } as const;

async function waitForQuerySuggestionsPageReady(page: Page, timeout: number = 30_000) {
  await page.waitForURL('**/query-suggestions', { timeout });
  // Valid dual-state: configs list vs "No Query Suggestions configs" empty heading
  await expect(
    page
      .getByTestId('qs-configs-list')
      .or(page.getByRole('heading', { name: 'No Query Suggestions configs' })),
  ).toBeVisible({ timeout });
  // Keep the heading assertion as an explicit route-level sanity check.
  await expect(page.getByRole('heading', QS_PAGE_HEADING)).toBeVisible({ timeout });
}

// ── Shared UI helpers (pure UI interactions — no API calls) ──────────────────

async function goToQsPage(page: Page) {
  await page.goto('/query-suggestions', { waitUntil: 'domcontentloaded' });
  await waitForQuerySuggestionsPageReady(page);
}

async function goToOverviewPage(page: Page) {
  await page.goto('/overview');
  await expect(page.getByRole('heading', { name: 'Overview', exact: true })).toBeVisible({ timeout: 10000 });
}

async function ensureNoConfigsViaUi(page: Page) {
  while (await page.getByTestId('qs-config-card').count()) {
    const card = page.getByTestId('qs-config-card').first();
    const configName = (await card.getByTestId('qs-config-name').innerText()).trim();
    await deleteConfigViaUi(page, configName);
  }
}

async function assertConfigNameVisible(page: Page, configName: string) {
  await expect(
    page.getByTestId('qs-config-name').filter({ hasText: configName }),
  ).toBeVisible({ timeout: 10000 });
}

async function createConfigViaUi(page: Page, configName: string) {
  await page.getByRole('button', { name: /create config/i }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await dialog.getByLabel(/suggestions index name/i).fill(configName);
  await dialog.getByLabel(/source index name/i).fill(QS_SOURCE);
  await dialog.getByRole('button', { name: /create config/i }).click();
  await expect(async () => {
    await expect(dialog).not.toBeVisible({ timeout: 10_000 });
  }).toPass({ timeout: 20_000, intervals: [1_000, 2_000, 4_000] });
  await expect(page.getByText('Config created', { exact: true })).toBeVisible({ timeout: 5000 });
}

async function waitForConfigNotVisible(
  page: Page,
  configName: string,
  timeoutMs: number = 30_000,
) {
  const deadline = Date.now() + timeoutMs;
  const targetConfig = page.getByTestId('qs-config-name').filter({ hasText: configName });

  while (Date.now() < deadline) {
    if ((await targetConfig.count()) === 0) {
      return;
    }

    await page.reload();
    await goToQsPage(page);
  }

  await expect(targetConfig).toHaveCount(0);
}

async function deleteConfigViaUi(page: Page, configName: string) {
  page.once('dialog', async (dlg) => {
    if (dlg.type() === 'confirm') await dlg.accept();
  });
  const card = page.getByTestId('qs-config-card').filter({ hasText: configName });
  await card.getByRole('button', { name: /delete config/i }).click();
  await waitForConfigNotVisible(page, configName);
}

async function deleteConfigViaUiIfPresent(page: Page, configName: string) {
  try {
    await goToQsPage(page);
    const card = page.getByTestId('qs-config-card').filter({ hasText: configName });
    if ((await card.count()) === 0) {
      return;
    }
    await deleteConfigViaUi(page, configName);
  } catch {
    // Best-effort cleanup only. If the main assertion already failed, keep that failure visible.
  }
}

async function waitForConfigVisibleAfterNavigation(
  page: Page,
  configName: string,
  timeoutMs: number = 45_000,
) {
  const targetConfig = page
    .getByTestId('qs-configs-list')
    .getByTestId('qs-config-name')
    .filter({ hasText: configName });
  if ((await targetConfig.count()) > 0 && await targetConfig.first().isVisible()) {
    return;
  }

  // toPass retries thrown async assertion failures, including readiness waits and visibility checks.
  await expect(async () => {
    await page.reload({ waitUntil: 'domcontentloaded' });
    await waitForQuerySuggestionsPageReady(page, 10_000);
    await expect(
      page
        .getByTestId('qs-configs-list')
        .getByTestId('qs-config-name')
        .filter({ hasText: configName }),
    ).toBeVisible({ timeout: 10_000 });
  }).toPass({ timeout: timeoutMs, intervals: [2_000, 4_000, 6_000] });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

test.describe('Query Suggestions Page', () => {
  // This suite mutates shared Query Suggestions state (create/delete/all-config cleanup),
  // so tests must run serially to avoid cross-test interference under fullyParallel mode.
  test.describe.configure({ mode: 'serial' });

  // ── Load-and-verify (first spec per BROWSER_TESTING_STANDARDS_2.md) ─────────
  //
  // Seed a config via UI (creation isn't the core focus here), navigate away,
  // navigate back, and assert the config renders correctly in the list body.

  test('seeded config renders in the list after navigation', async ({ page }) => {
    // This flow intentionally waits for eventual consistency after route changes.
    // Marking slow avoids false timeouts in full-suite CI contention.
    test.slow();

    const configName = `qs-seed-${Date.now()}`;

    // ARRANGE: create config via UI (precondition for list-render test)
    await goToQsPage(page);
    await createConfigViaUi(page, configName);
    // Scope to card name element (configName also appears in the API log widget)
    await assertConfigNameVisible(page, configName);

    // Navigate away then back — forces a fresh data fetch
    await goToOverviewPage(page);
    await goToQsPage(page);

    // ASSERT: config appears in the list body
    await waitForConfigVisibleAfterNavigation(page, configName);
    await expect(page.getByTestId('qs-configs-list').getByTestId('qs-config-name').filter({ hasText: configName }))
      .toBeVisible();

    // CLEANUP
    await deleteConfigViaUi(page, configName);
  });

  test('config visibility poll survives delayed config response after navigation', async ({ page }) => {
    test.slow();

    const configName = `qs-delayed-nav-${Date.now()}`;
    const configListDelayMs = 1800;

    await goToQsPage(page);
    await createConfigViaUi(page, configName);
    await assertConfigNameVisible(page, configName);

    await page.route('**/1/configs', async (route) => {
      await new Promise<void>((resolve) => setTimeout(resolve, configListDelayMs));
      await route.continue();
    });

    await goToOverviewPage(page);

    // Use the shared readiness helper instead of bare goto+waitForURL to avoid
    // skeleton-phase races — the route interceptor delays configs by ~1800ms,
    // well within goToQsPage()'s 30s default heading-wait timeout.
    await goToQsPage(page);

    try {
      await waitForConfigVisibleAfterNavigation(page, configName, 8000);
      await expect(page.getByTestId('qs-configs-list').getByTestId('qs-config-name').filter({ hasText: configName }))
        .toBeVisible();
    } finally {
      await page.unroute('**/1/configs');
      await deleteConfigViaUiIfPresent(page, configName);
    }
  });

  // ── Page basics ──────────────────────────────────────────────────────────────

  test('page loads with heading and Create Config button', async ({ page }) => {
    await goToQsPage(page);
    await expect(page.getByRole('button', { name: /create config/i })).toBeVisible();
  });

  test('empty state shows Create Your First Config when no configs exist', async ({ page }) => {
    await goToQsPage(page);
    await ensureNoConfigsViaUi(page);
    await expect(page.getByRole('heading', { name: 'No Query Suggestions configs' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Create Your First Config' })).toBeVisible();
  });

  // ── Create config dialog ─────────────────────────────────────────────────────

  test('create config dialog shows all required form fields', async ({ page }) => {
    await goToQsPage(page);

    await page.getByRole('button', { name: /create config/i }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();
    await expect(
      dialog.getByRole('heading', { name: /create query suggestions/i })
    ).toBeVisible();

    await expect(dialog.getByLabel(/suggestions index name/i)).toBeVisible();
    await expect(dialog.getByLabel(/source index name/i)).toBeVisible();
    await expect(dialog.getByLabel(/minimum hits/i)).toBeVisible();
    await expect(dialog.getByLabel(/minimum letters/i)).toBeVisible();
    await expect(dialog.getByLabel(/exclude word/i)).toBeVisible();
    await expect(dialog.getByRole('button', { name: /create config/i })).toBeVisible();
    await expect(dialog.getByRole('button', { name: /cancel/i })).toBeVisible();

    await dialog.getByRole('button', { name: /cancel/i }).click();
    await expect(dialog).not.toBeVisible({ timeout: 5000 });
  });

  test('exclude word chips can be added and removed before submit', async ({ page }) => {
    const configName = `qs-exclude-${Date.now()}`;

    await goToQsPage(page);
    await page.getByRole('button', { name: /create config/i }).click();

    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();
    await dialog.getByLabel(/suggestions index name/i).fill(configName);
    await dialog.getByLabel(/source index name/i).fill(QS_SOURCE);

    const excludeInput = dialog.getByLabel(/exclude word/i);
    await excludeInput.fill('noise');
    await dialog.getByRole('button', { name: /^add$/i }).click();

    const excludeList = dialog.getByTestId('exclude-list');
    await expect(excludeList).toBeVisible();
    await expect(excludeList.getByText('noise')).toBeVisible();

    await excludeList.getByRole('button', { name: 'Remove noise from exclude list' }).click();
    await expect(dialog.getByTestId('exclude-list')).toHaveCount(0);

    await dialog.getByRole('button', { name: /create config/i }).click();
    await expect(dialog).not.toBeVisible({ timeout: 10000 });
    await assertConfigNameVisible(page, configName);

    await deleteConfigViaUi(page, configName);
  });

  test('cancel closes dialog without creating a config', async ({ page }) => {
    const uniqueName = `qs-cancelled-${Date.now()}`;

    await goToQsPage(page);

    await page.getByRole('button', { name: /create config/i }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();

    await dialog.getByLabel(/suggestions index name/i).fill(uniqueName);
    await dialog.getByRole('button', { name: /cancel/i }).click();
    await expect(dialog).not.toBeVisible({ timeout: 5000 });

    await expect(page.getByText(uniqueName)).toBeHidden();
  });

  // ── Create and verify card ───────────────────────────────────────────────────

  test('created config card shows source index, status, and action buttons', async ({ page }) => {
    const configName = `qs-card-${Date.now()}`;

    await goToQsPage(page);
    await createConfigViaUi(page, configName);

    const card = page.getByTestId('qs-config-card').filter({ hasText: configName });
    await expect(card).toBeVisible({ timeout: 10000 });

    await expect(card.getByText(QS_SOURCE)).toBeVisible();
    await expect(card.getByText(/last built/i)).toBeVisible();
    await expect(card.getByRole('button', { name: 'Rebuild suggestions index', exact: true })).toBeVisible();
    await expect(card.getByRole('button', { name: /delete config/i })).toBeVisible();

    // CLEANUP
    await deleteConfigViaUi(page, configName);
  });

  // ── Rebuild button ───────────────────────────────────────────────────────────

  test('rebuild button triggers a build and shows toast', async ({ page }) => {
    const configName = `qs-rebuild-${Date.now()}`;

    await goToQsPage(page);
    await createConfigViaUi(page, configName);

    const card = page.getByTestId('qs-config-card').filter({ hasText: configName });
    await expect(card).toBeVisible({ timeout: 10000 });

    // Wait until the initial auto-build finishes (button re-enables).
    // Use exact aria-label "Rebuild suggestions index" — the config name contains "qs-rbld"
    // which would otherwise let /rebuild/i also match the delete button's aria-label.
    const rebuildBtn = card.getByRole('button', { name: 'Rebuild suggestions index', exact: true });
    await expect(rebuildBtn).toBeEnabled({ timeout: 30000 });

    await rebuildBtn.click();

    // The toast renders 3 elements (title div, description div, aria-live span).
    // Match the toast title exactly to avoid strict mode violations.
    await expect(
      page.getByText('Build triggered', { exact: true })
    ).toBeVisible({ timeout: 5000 });

    // CLEANUP
    await deleteConfigViaUi(page, configName);
  });

  test('build logs can be expanded and collapsed after rebuild', async ({ page }) => {
    const configName = `qs-logs-${Date.now()}`;

    await goToQsPage(page);
    await createConfigViaUi(page, configName);

    let card = page.getByTestId('qs-config-card').filter({ hasText: configName });
    await expect(card).toBeVisible({ timeout: 10000 });

    const rebuildBtn = card.getByRole('button', { name: 'Rebuild suggestions index', exact: true });
    await expect(rebuildBtn).toBeEnabled({ timeout: 30000 });
    await rebuildBtn.click();
    await expect(page.getByText('Build triggered', { exact: true })).toBeVisible({ timeout: 5000 });

    // Wait for rebuild lifecycle to settle before refetching logs.
    await expect(rebuildBtn).toBeEnabled({ timeout: 30000 });
    await page.reload();
    await goToQsPage(page);

    card = page.getByTestId('qs-config-card').filter({ hasText: configName });
    await expect(card).toBeVisible({ timeout: 10000 });

    const buildLogsToggle = card.getByRole('button', { name: /build logs/i });
    await expect(buildLogsToggle).toBeVisible({ timeout: 30000 });
    await expect(buildLogsToggle).toHaveAttribute('aria-expanded', 'false');

    await buildLogsToggle.click();
    await expect(buildLogsToggle).toHaveAttribute('aria-expanded', 'true');

    const buildLogsPanel = card.getByTestId('build-logs');
    await expect(buildLogsPanel).toBeVisible();
    await expect(buildLogsPanel).toContainText(/\[(INFO|WARN|ERROR)\]/);

    await buildLogsToggle.click();
    await expect(buildLogsToggle).toHaveAttribute('aria-expanded', 'false');
    await expect(card.getByTestId('build-logs')).toHaveCount(0);

    await deleteConfigViaUi(page, configName);
  });

  // ── Delete config ────────────────────────────────────────────────────────────

  test('delete config removes it from the list', async ({ page }) => {
    const configName = `qs-delete-${Date.now()}`;

    await goToQsPage(page);
    await createConfigViaUi(page, configName);

    const card = page.getByTestId('qs-config-card').filter({ hasText: configName });
    await expect(card).toBeVisible({ timeout: 10000 });

    await deleteConfigViaUi(page, configName);

    await expect(page.getByTestId('qs-config-name').filter({ hasText: configName })).toBeHidden();
  });

  // ── Sidebar navigation ───────────────────────────────────────────────────────

  test('sidebar Query Suggestions link navigates to the page', async ({ page }) => {
    await goToOverviewPage(page);

    await page.getByRole('link', { name: /query suggestions/i }).click();

    await expect(
      page.getByRole('heading', QS_PAGE_HEADING)
    ).toBeVisible({ timeout: 10000 });
    await expect(page).toHaveURL(/query-suggestions/);
  });
});
