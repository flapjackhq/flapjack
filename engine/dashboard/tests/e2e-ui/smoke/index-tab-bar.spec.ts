/**
 * Smoke — Index Tab Bar (Structural Chrome Only)
 *
 * Verifies that the tab bar renders expected tabs and highlights the active
 * tab. Does NOT click through routes — navigation.spec.ts is the canonical
 * owner of tab route-click assertions.
 */
import { test, expect, TEST_INDEX, gotoIndexPage } from '../helpers';

test.describe('Index tab bar smoke', () => {
  test('renders all always-visible tabs on the seeded index', async ({ page }) => {
    await gotoIndexPage(page, TEST_INDEX);

    // All 7 always-visible tabs present via data-testid
    await expect(page.getByTestId('index-tab-browse')).toBeVisible();
    await expect(page.getByTestId('index-tab-settings')).toBeVisible();
    await expect(page.getByTestId('index-tab-analytics')).toBeVisible();
    await expect(page.getByTestId('index-tab-synonyms')).toBeVisible();
    await expect(page.getByTestId('index-tab-rules')).toBeVisible();
    await expect(page.getByTestId('index-tab-merchandising')).toBeVisible();
    await expect(page.getByTestId('index-tab-recommendations')).toBeVisible();
  });

  test('browse tab is active by default on index page', async ({ page }) => {
    await gotoIndexPage(page, TEST_INDEX);

    // Browse tab should have active/selected styling (aria-current or data-active)
    const browseTab = page.getByTestId('index-tab-browse');
    await expect(browseTab).toBeVisible();
    // The tab text should indicate browse is the current page
    await expect(browseTab).toHaveAttribute('aria-current', 'page');
  });

  test('chat tab is hidden when index is not in NeuralSearch mode', async ({ page }) => {
    await gotoIndexPage(page, TEST_INDEX);

    // The seeded TEST_INDEX is not in NeuralSearch mode, so Chat tab should not be visible
    await expect(page.getByTestId('index-tab-chat')).toBeHidden();
  });
});
