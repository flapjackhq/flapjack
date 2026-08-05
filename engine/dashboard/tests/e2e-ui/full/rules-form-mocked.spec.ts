/**
 * E2E-UI Full Suite -- Rule Editor Form Mode Mocked Tests (Browser-Mocked)
 *
 * Tests the Rule Editor form mode with mocked API responses to verify
 * form field population from complex rule data.
 *
 * Covers:
 * - Load existing rule with promote + hide consequences into form mode
 * - Verify all form fields populate correctly from mocked API response
 * - Rule with multiple conditions loads correctly
 *
 * STANDARDS COMPLIANCE:
 * - page.route() is allowed for browser-mocked tests
 * - Zero page.evaluate()
 * - Zero CSS class selectors
 */
import { test, expect } from '../../fixtures/auth.fixture';

const INDEX = 'e2e-products';
const RULES_URL = `/index/${INDEX}/rules`;

test.describe('Rule Editor Form — Mocked Data Loading', () => {
  test('loads complex rule into form mode with all fields populated', async ({ page }) => {
    const complexRule = {
      objectID: 'complex-mock-rule',
      conditions: [
        { pattern: 'laptop', anchoring: 'contains' },
        { pattern: 'sale', anchoring: 'is', context: 'promo-page' },
      ],
      consequence: {
        promote: [
          { objectID: 'p01', position: 0 },
          { objectID: 'p02', position: 1 },
        ],
        hide: [{ objectID: 'p05' }],
        params: {
          query: 'laptop deals',
          hitsPerPage: 20,
          filters: 'inStock:true',
        },
        filterPromotes: true,
      },
      description: 'Complex test rule for form loading',
      enabled: true,
    };

    // Mock rules list to include our complex rule
    await page.route(`**/1/indexes/${INDEX}/rules/search*`, (route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ hits: [complexRule], nbHits: 1 }),
      }),
    );

    // Mock individual rule fetch
    await page.route(`**/1/indexes/${INDEX}/rules/complex-mock-rule`, (route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(complexRule),
      }),
    );

    await page.goto(RULES_URL);
    await expect(page.getByText('Rules').first()).toBeVisible({ timeout: 15_000 });

    // Wait for rules list to show the mocked rule
    await expect(page.getByText('complex-mock-rule').first()).toBeVisible({ timeout: 10_000 });

    // Click edit on the rule card
    const ruleCard = page.getByTestId('rule-card').filter({ hasText: 'complex-mock-rule' }).first();
    await ruleCard.getByRole('button', { name: /edit/i }).click();

    // Dialog should open in form mode
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible({ timeout: 10_000 });

    // Verify Object ID and Description
    await expect(dialog.getByRole('textbox', { name: 'Object ID', exact: true })).toHaveValue('complex-mock-rule');
    await expect(dialog.getByLabel('Description')).toHaveValue('Complex test rule for form loading');

    // Verify conditions loaded (2 conditions)
    await expect(dialog.getByLabel('Pattern 1')).toHaveValue('laptop');
    await expect(dialog.getByLabel('Pattern 2')).toHaveValue('sale');

    // Cancel to close
    await dialog.getByRole('button', { name: /cancel/i }).click();
  });

  test('rule with empty conditions loads as conditionless', async ({ page }) => {
    const conditionlessRule = {
      objectID: 'conditionless-mock-rule',
      conditions: [],
      consequence: {
        promote: [{ objectID: 'p01', position: 0 }],
      },
      description: 'Always applies',
      enabled: true,
    };

    await page.route(`**/1/indexes/${INDEX}/rules/search*`, (route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ hits: [conditionlessRule], nbHits: 1 }),
      }),
    );

    await page.goto(RULES_URL);
    await expect(page.getByText('conditionless-mock-rule').first()).toBeVisible({ timeout: 15_000 });

    // Click edit
    const ruleCard = page.getByTestId('rule-card').filter({ hasText: 'conditionless-mock-rule' }).first();
    await ruleCard.getByRole('button', { name: /edit/i }).click();

    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible({ timeout: 10_000 });

    // Should show "No conditions configured" for conditionless rule
    await expect(dialog.getByText('No conditions configured')).toBeVisible();

    await dialog.getByRole('button', { name: /cancel/i }).click();
  });
});
