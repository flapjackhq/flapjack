/**
 * E2E-UI Full Suite — Navigation, Layout & Header (Real Server)
 *
 * CANONICAL OWNER of sidebar-link and index-tab route-click assertions.
 * Smoke specs verify structural chrome only; this spec verifies every
 * route defined in sidebar-nav.ts and IndexTabBar.tsx actually navigates.
 *
 * NO mocking. Tests verify real navigation between pages and UI state.
 *
 * Pre-requisites:
 *   - Flapjack server running on the repo-local configured backend port
 *   - `e2e-products` index seeded with 12 products (via seed.setup.ts)
 *   - Vite dev server on the repo-local configured dashboard port
 */
import {
  test,
  expect,
  TEST_INDEX,
  getSidebar,
  gotoIndexPage,
  gotoOverviewPage,
  waitForOverviewIndexRow,
  waitForSearchResultsOrEmptyState,
} from '../helpers';
import {
  createIndex,
  deleteIndex,
  addDocuments,
  isVectorSearchEnabled,
} from '../../fixtures/api-helpers';
import { setChatReadySettings } from '../../fixtures/chat-api-helpers';

test.describe('Navigation & Layout', () => {

  // =========================================================================
  // Sidebar Navigation — structural
  // =========================================================================

  test('sidebar shows all main navigation items', async ({ page }) => {
    await gotoOverviewPage(page);

    await expect(page.getByTestId('stat-card-indexes')).toBeVisible({ timeout: 10_000 });
    await waitForOverviewIndexRow(page, TEST_INDEX);

    const sidebar = getSidebar(page);

    // Indexes section
    await expect(sidebar.getByText('Overview').first()).toBeVisible();

    // Intelligence section
    await expect(sidebar.getByText('Query Suggestions').first()).toBeVisible();
    await expect(sidebar.getByText('Experiments').first()).toBeVisible();
    await expect(sidebar.getByText('Personalization').first()).toBeVisible();

    // Developer section
    await expect(sidebar.getByText('API Keys').first()).toBeVisible();
    await expect(sidebar.getByText('Security Sources').first()).toBeVisible();
    await expect(sidebar.getByText('Dictionaries').first()).toBeVisible();
    await expect(sidebar.getByText('API Logs').first()).toBeVisible();
    await expect(sidebar.getByText('Event Debugger').first()).toBeVisible();

    // System section
    await expect(sidebar.getByText('Migrate').first()).toBeVisible();
    await expect(sidebar.getByText('Metrics').first()).toBeVisible();
    await expect(sidebar.getByText('System').first()).toBeVisible();

    // Seeded data verification: index name renders in the page-body table row, not just sidebar.
    const indexRow = page.getByTestId(`overview-index-row-${TEST_INDEX}`);
    await expect(indexRow.getByText(TEST_INDEX)).toBeVisible({ timeout: 10_000 });
  });

  test('sidebar shows seeded index in indexes section', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await expect(sidebar.getByText(TEST_INDEX).first()).toBeVisible({ timeout: 10_000 });
  });

  // =========================================================================
  // Sidebar Navigation — route-click coverage (Indexes section)
  // =========================================================================

  test('clicking sidebar Overview navigates to overview page', async ({ page }) => {
    await gotoIndexPage(page, TEST_INDEX);

    const sidebar = getSidebar(page);
    await sidebar.getByText('Overview').first().click();
    await expect(page).toHaveURL(/\/overview/);
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({ timeout: 10_000 });
  });

  test('clicking sidebar index link navigates to search page', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByText(TEST_INDEX).first().click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}`));
    await waitForSearchResultsOrEmptyState(page);
  });

  // =========================================================================
  // Sidebar Navigation — route-click coverage (Intelligence section)
  // =========================================================================

  test('clicking sidebar Query Suggestions navigates correctly', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'Query Suggestions' }).click();
    await expect(page).toHaveURL(/\/query-suggestions/);
    await expect(page.getByRole('heading', { name: 'Query Suggestions', exact: true })).toBeVisible({ timeout: 10_000 });
  });

  test('clicking sidebar Experiments navigates correctly', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'Experiments' }).click();
    await expect(page).toHaveURL(/\/experiments/);
    await expect(page.getByTestId('experiments-heading')).toBeVisible({ timeout: 10_000 });
  });

  test('clicking sidebar Personalization navigates correctly', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'Personalization' }).click();
    await expect(page).toHaveURL(/\/personalization/);
    await expect(page.getByRole('heading', { name: 'Personalization', exact: true })).toBeVisible({ timeout: 10_000 });
  });

  // =========================================================================
  // Sidebar Navigation — route-click coverage (Developer section)
  // =========================================================================

  test('clicking sidebar API Keys navigates to keys page', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'API Keys' }).click();
    await expect(page).toHaveURL(/\/keys/);
    await expect(page.getByRole('heading', { name: 'API Keys', exact: true })).toBeVisible({ timeout: 10_000 });
  });

  test('clicking sidebar Security Sources navigates correctly', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'Security Sources' }).click();
    await expect(page).toHaveURL(/\/security-sources/);
    await expect(page.getByRole('heading', { name: 'Security Sources' })).toBeVisible({ timeout: 10_000 });
  });

  test('clicking sidebar Dictionaries navigates correctly', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'Dictionaries' }).click();
    await expect(page).toHaveURL(/\/dictionaries/);
    await expect(page.getByRole('heading', { name: 'Dictionaries' })).toBeVisible({ timeout: 10_000 });
  });

  test('clicking sidebar API Logs navigates to logs page', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'API Logs' }).click();
    await expect(page).toHaveURL(/\/logs/);
    await expect(page.getByRole('heading', { name: /api log/i })).toBeVisible({ timeout: 10_000 });
  });

  test('clicking sidebar Event Debugger navigates to events page', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'Event Debugger' }).click();
    await expect(page).toHaveURL(/\/events/);
    await expect(page.getByRole('heading', { name: 'Event Debugger' })).toBeVisible({ timeout: 10_000 });
  });

  // =========================================================================
  // Sidebar Navigation — route-click coverage (System section)
  // =========================================================================

  test('clicking sidebar Migrate navigates to migrate page', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'Migrate' }).click();
    await expect(page).toHaveURL(/\/migrate/);
    await expect(page.getByRole('heading', { name: /migrate/i })).toBeVisible({ timeout: 10_000 });
  });

  test('clicking sidebar Metrics navigates to metrics page', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'Metrics' }).click();
    await expect(page).toHaveURL(/\/metrics/);
    await expect(page.getByRole('heading', { name: /metrics/i })).toBeVisible({ timeout: 10_000 });
  });

  test('clicking sidebar System navigates to system page', async ({ page }) => {
    await gotoOverviewPage(page);

    const sidebar = getSidebar(page);
    await sidebar.getByRole('link', { name: 'System', exact: true }).click();
    await expect(page).toHaveURL(/\/system/);
    await expect(page.getByRole('heading', { name: /system/i })).toBeVisible({ timeout: 10_000 });
  });

  // =========================================================================
  // Header
  // =========================================================================

  test('header shows Flapjack logo and connection status', async ({ page }) => {
    await gotoOverviewPage(page);

    // Flapjack logo/brand text in header
    await expect(page.getByText('Flapjack').first()).toBeVisible();

    // Connection status badge — should show "Connected" since server is running and auth is seeded
    await expect(page.getByText('Connected')).toBeVisible({ timeout: 10_000 });
  });

  test('theme toggle switches between light and dark mode', async ({ page }) => {
    await gotoOverviewPage(page);

    // Click the theme toggle button
    const themeBtn = page.getByRole('button', { name: /toggle theme/i });
    await expect(themeBtn).toBeVisible();

    // Get initial theme state
    const documentRoot = page.getByRole('document');
    const htmlBefore = await documentRoot.getAttribute('class');

    // Click to toggle — wait for the class to actually change (Playwright retries automatically)
    await themeBtn.click();
    await expect(documentRoot).not.toHaveAttribute('class', htmlBefore ?? '');

    // Theme class should change
    const htmlAfter = await documentRoot.getAttribute('class');
    expect(htmlBefore).not.toBe(htmlAfter);

    // Toggle back — wait for class to return to original
    await themeBtn.click();
    await expect(documentRoot).toHaveAttribute('class', htmlBefore ?? '');
    const htmlRestored = await documentRoot.getAttribute('class');
    expect(htmlRestored).toBe(htmlBefore);
  });

  test('indexing queue button opens empty queue panel', async ({ page }) => {
    await gotoOverviewPage(page);

    // Click the queue button
    const queueBtn = page.getByRole('button', { name: /indexing queue/i });
    await expect(queueBtn).toBeVisible();
    await queueBtn.click();

    // Queue panel should appear and show idle state
    await expect(page.getByText('Indexing Queue')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText(/no active tasks|all clear/i).first()).toBeVisible();
  });

  // =========================================================================
  // Index Tab Bar — route-click coverage (seeded index)
  // =========================================================================

  test('index tab bar covers all always-visible tabs', async ({ page }) => {
    await gotoIndexPage(page, TEST_INDEX);

    // Settings
    await page.getByTestId('index-tab-settings').click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}/settings`));
    await expect(page.getByText('Searchable Attributes').first()).toBeVisible({ timeout: 10_000 });

    // Analytics
    await page.getByTestId('index-tab-analytics').click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}/analytics`));
    await expect(page.getByTestId('analytics-heading')).toBeVisible({ timeout: 15_000 });

    // Synonyms
    await page.getByTestId('index-tab-synonyms').click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}/synonyms`));
    await expect(page.getByText('Synonyms').first()).toBeVisible({ timeout: 10_000 });

    // Rules
    await page.getByTestId('index-tab-rules').click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}/rules`));
    await expect(page.getByRole('heading', { name: /rules/i })).toBeVisible({ timeout: 10_000 });

    // Merchandising
    await page.getByTestId('index-tab-merchandising').click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}/merchandising`));
    await expect(page.getByText('Merchandising Studio').first()).toBeVisible({ timeout: 15_000 });

    // Recommendations
    await page.getByTestId('index-tab-recommendations').click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}/recommendations`));
    await expect(page.getByRole('heading', { name: /recommendations/i })).toBeVisible({ timeout: 10_000 });

    // Browse (back to default) — clicking Browse tab returns to the index search page
    await page.getByTestId('index-tab-browse').click();
    await expect(page).toHaveURL(new RegExp(`/index/${TEST_INDEX}$`));
    await waitForSearchResultsOrEmptyState(page);
  });

  // =========================================================================
  // Index Tab Bar — Chat tab (requires NeuralSearch mode)
  // =========================================================================

  test('chat tab visible and navigable when NeuralSearch mode is enabled', async ({ page, request }) => {
    const chatIndex = `e2e-nav-chat-${Date.now()}`;
    const vectorSearchEnabled = await isVectorSearchEnabled(request);

    try {
      await deleteIndex(request, chatIndex);
      await createIndex(request, chatIndex);
      await addDocuments(request, chatIndex, [
        { objectID: 'chat-nav-1', name: 'Nav Chat Product', category: 'Test' },
      ]);
      await setChatReadySettings(request, chatIndex);

      await gotoIndexPage(page, chatIndex);

      if (!vectorSearchEnabled) {
        await expect(page.getByTestId('index-tab-chat')).not.toBeVisible();
        return;
      }

      // Chat tab should be visible with NeuralSearch mode
      await expect(page.getByTestId('index-tab-chat')).toBeVisible({ timeout: 10_000 });

      await page.getByTestId('index-tab-chat').click();
      await expect(page).toHaveURL(new RegExp(`/index/${chatIndex}/chat`));
      await expect(page.getByTestId('chat-input')).toBeVisible({ timeout: 10_000 });
    } finally {
      await deleteIndex(request, chatIndex);
    }
  });

  // =========================================================================
  // Cross-page Navigation (breadcrumb)
  // =========================================================================

  test('search page breadcrumb navigates back to overview', async ({ page }) => {
    await gotoIndexPage(page, TEST_INDEX);

    // Click the Overview breadcrumb link
    await page.getByRole('link', { name: /overview/i }).first().click();
    await expect(page).toHaveURL(/\/overview/);
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible({ timeout: 10_000 });
  });

  // =========================================================================
  // 404 / Not Found
  // =========================================================================

  test('navigating to unknown route shows page not found', async ({ page }) => {
    await page.goto('/nonexistent-page-12345');
    await expect(page.getByText('Page not found')).toBeVisible({ timeout: 10_000 });
  });
});
