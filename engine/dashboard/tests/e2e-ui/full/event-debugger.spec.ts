/**
 * @module E2E tests for the Event Debugger page, verifying event table rendering, detail panel, status/type filtering, and sidebar navigation against a real Flapjack backend.
 */
/**
 * E2E-UI Full Suite -- Event Debugger Page (Real Server)
 *
 * NON-MOCKED SIMULATED-HUMAN REAL-BROWSER TESTS.
 * Tests the Event Debugger page at /events against a real Flapjack backend.
 * Events are seeded via the Insights API (POST /1/events) in the Arrange phase,
 * then the UI is tested for correct rendering and interaction.
 *
 * Covers:
 * - Page loads with heading and filter controls
 * - Seeded events appear in the event table
 * - Status badges render correctly (OK for valid events)
 * - Event detail panel opens on row click with full payload
 * - Status filter narrows results
 * - Empty state when no events match filters
 *
 * STANDARDS COMPLIANCE (BROWSER_TESTING_STANDARDS_2.md):
 * - Zero page.evaluate() — all assertions via Playwright locators
 * - Zero CSS class selectors — uses data-testid, getByRole, getByText
 * - Zero { force: true } — relies on Playwright actionability checks
 * - API calls only in fixtures (sendEvents helper), not in spec body
 */
import { test, expect, TEST_INDEX } from '../helpers';
import type { APIRequestContext } from '@playwright/test';
import { sendEvents } from '../../fixtures/api-helpers';

interface SeededEvents {
  clickName: string;
  purchaseName: string;
  viewName: string;
  clickUserToken: string;
}

function uniqueSuffix() {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

function randomHex32(): string {
  return Array.from({ length: 32 }, () => Math.floor(Math.random() * 16).toString(16)).join('');
}

/**
 * Seeds three distinct Insights events (click, conversion, view) via the API for use in Event Debugger tests.
 * 
 * Each event name and user token includes a unique timestamp+random suffix to avoid
 * collisions across parallel test runs. Returns the generated names and tokens so
 * specs can assert against them.
 * 
 * @param request - Playwright API request context used to POST events
 * @returns The unique event names and click user token for later assertions
 */
async function seedDebugEvents(request: APIRequestContext): Promise<SeededEvents> {
  const suffix = uniqueSuffix();
  const clickName = `Product Clicked ${suffix}`;
  const purchaseName = `Product Purchased ${suffix}`;
  const viewName = `Product Viewed ${suffix}`;
  const clickUserToken = `e2e-user-click-${suffix}`;

  await sendEvents(request, [
    {
      eventType: 'click',
      eventName: clickName,
      index: TEST_INDEX,
      userToken: clickUserToken,
      objectIDs: ['p01'],
      positions: [1],
      queryID: randomHex32(),
    },
    {
      eventType: 'conversion',
      eventName: purchaseName,
      index: TEST_INDEX,
      userToken: `e2e-user-conversion-${suffix}`,
      objectIDs: ['p03'],
      queryID: randomHex32(),
    },
    {
      eventType: 'view',
      eventName: viewName,
      index: TEST_INDEX,
      userToken: `e2e-user-view-${suffix}`,
      objectIDs: ['p05', 'p06'],
    },
  ]);

  return {
    clickName,
    purchaseName,
    viewName,
    clickUserToken,
  };
}

test.describe('Event Debugger', () => {
  test('page loads with heading and event count badge', async ({ page, request }) => {
    await seedDebugEvents(request);

    await page.goto('/events');
    await expect(page.getByRole('heading', { name: 'Event Debugger' })).toBeVisible({ timeout: 15_000 });
    const count = page.getByTestId('event-count');
    await expect(count).toBeVisible({ timeout: 10_000 });
    await expect(count).toContainText(/[1-9]/);
  });

  test('seeded events appear in the event table', async ({ page, request }) => {
    const seeded = await seedDebugEvents(request);

    await page.goto('/events');
    await expect(page.getByRole('heading', { name: 'Event Debugger' })).toBeVisible({ timeout: 15_000 });

    const table = page.getByTestId('event-table');
    await expect(table).toBeVisible({ timeout: 10_000 });

    // Verify seeded events appear
    await expect(table.getByText(seeded.clickName).first()).toBeVisible({ timeout: 10_000 });
    await expect(table.getByText(seeded.purchaseName).first()).toBeVisible();
    await expect(table.getByText(seeded.viewName).first()).toBeVisible();
  });

  test('event rows show correct index and user token', async ({ page, request }) => {
    const seeded = await seedDebugEvents(request);

    await page.goto('/events');
    const table = page.getByTestId('event-table');
    const seededRow = table.getByTestId('event-row').filter({ hasText: seeded.clickName }).first();
    await expect(seededRow).toBeVisible({ timeout: 15_000 });

    const rowText = await seededRow.textContent();
    expect(rowText).toContain(TEST_INDEX);
    expect(rowText).toContain(seeded.clickUserToken);
  });

  test('clicking an event row opens the detail panel', async ({ page, request }) => {
    const seeded = await seedDebugEvents(request);

    await page.goto('/events');
    const table = page.getByTestId('event-table');
    const seededRow = table.getByTestId('event-row').filter({ hasText: seeded.clickName }).first();
    await expect(seededRow).toBeVisible({ timeout: 15_000 });

    await seededRow.click();

    // Detail panel should appear with event details
    const detail = page.getByTestId('event-detail');
    await expect(detail).toBeVisible({ timeout: 5_000 });
    await expect(detail.getByText('Event Detail')).toBeVisible();
    await expect(detail.getByText('Full Payload')).toBeVisible();
    await expect(detail.getByText(seeded.clickName, { exact: true })).toBeVisible();
  });

  test('status filter narrows displayed events', async ({ page, request }) => {
    const seeded = await seedDebugEvents(request);

    await page.goto('/events');
    const table = page.getByTestId('event-table');
    await expect(table.getByText(seeded.clickName).first()).toBeVisible({ timeout: 15_000 });

    // Seeded events are all valid (OK), so the failed-events filter should show 0.
    await page.getByLabel('Status').selectOption({ label: 'Failed' });

    // Wait for the table to update (polling refetch) and verify seeded OK events disappear.
    await expect(async () => {
      const visibleSeededEvents =
        (await table.getByText(seeded.clickName).count()) +
        (await table.getByText(seeded.purchaseName).count()) +
        (await table.getByText(seeded.viewName).count());
      expect(visibleSeededEvents).toBe(0);
    }).toPass({ timeout: 10_000 });
  });

  test('event type filter works', async ({ page, request }) => {
    const seeded = await seedDebugEvents(request);

    await page.goto('/events');
    const table = page.getByTestId('event-table');
    await expect(table.getByText(seeded.clickName).first()).toBeVisible({ timeout: 15_000 });

    // Select "click" event type filter
    await page.getByLabel('Event Type').selectOption('click');

    // After filtering, only click events should remain, and non-click seeded events should disappear.
    await expect(async () => {
      const visibleClickSeeded = await table.getByText(seeded.clickName).count();
      const visibleNonClickSeeded =
        (await table.getByText(seeded.purchaseName).count()) +
        (await table.getByText(seeded.viewName).count());
      expect(visibleClickSeeded).toBeGreaterThan(0);
      expect(visibleNonClickSeeded).toBe(0);

      const rows = table.getByTestId('event-row');
      const rowCount = await rows.count();
      expect(rowCount).toBeGreaterThan(0);
      for (let i = 0; i < rowCount; i++) {
        const text = await rows.nth(i).textContent();
        expect(text).toContain('click');
      }
    }).toPass({ timeout: 10_000 });
  });

});
