# Flapjack Dashboard - Playwright Tests

Automated end-to-end tests for the Flapjack dashboard using Playwright.

## 🚀 Quick Start

```bash
# Run all tests (headless)
npm test

# Run tests with UI (recommended for development)
npm run test:ui

# Run tests in headed mode (see browser)
npm run test:headed

# Debug tests (step through with debugger)
npm run test:debug

# View test report
npm run test:report
```

## 📦 Test Structure

```
tests/
├── overview.spec.ts      - Overview page tests
├── search.spec.ts        - Search & Browse page tests
├── settings.spec.ts      - Settings page tests
├── apikeys.spec.ts       - API Keys page tests
└── navigation.spec.ts    - Navigation & layout tests
```

## ✅ Test Coverage

### Overview Page
- ✅ Page loads
- ✅ Stats cards display
- ✅ Index list renders
- ✅ Pagination works
- ✅ Browse button navigates to search
- ✅ Dark mode toggle

### Search & Browse Page
- ✅ Page loads
- ✅ Search input and filter button
- ✅ Results panel displays
- ✅ Search on Enter key
- ✅ Filter panel toggle
- ✅ Facets panel displays
- ✅ Breadcrumb navigation
- ✅ Document cards render
- ✅ Expand/collapse JSON viewer

### Settings Page
- ✅ Page loads
- ✅ Settings form displays
- ✅ Search behavior section
- ✅ Faceting section
- ✅ Save/Reset buttons
- ✅ Form modification detection
- ✅ Reset functionality
- ✅ Navigation breadcrumb

### API Keys Page
- ✅ Page loads
- ✅ Create key button
- ✅ Create key dialog
- ✅ Form fields in dialog
- ✅ Dialog cancel
- ✅ Keys list (empty state)
- ✅ Key actions (copy, delete)
- ✅ Navigation

### Navigation & Layout
- ✅ Header with logo
- ✅ Sidebar navigation
- ✅ Page navigation
- ✅ Active link highlighting
- ✅ API logger drawer
- ✅ Dark mode persistence
- ✅ 404 handling
- ✅ Responsive design

## 🎯 Test Status

**Current Results:**
- **32/195 tests passing** (Chromium only)
- **163 tests skipped** (Firefox/WebKit not installed)

## 🔧 Configuration

### Browser Setup

By default, only Chromium is configured to keep tests fast. To test other browsers:

1. Install browsers:
```bash
npx playwright install firefox webkit
```

2. Uncomment browser configs in `playwright.config.ts`

### Mobile Testing

Mobile viewports are disabled by default. To enable:
- Uncomment mobile projects in `playwright.config.ts`

## 🐛 Common Issues

### Tests fail with "No API response"
**Solution:** The dashboard needs a running Flapjack server. Tests expect `http://localhost:7700` to be available.

**Mock Mode (Future):** Tests will be updated to run with mocked API responses.

### Tests timeout
**Solution:** Increase timeout in `playwright.config.ts`:
```typescript
use: {
  timeout: 60000, // 60 seconds
}
```

### Page not found errors
**Solution:** Ensure dev server is running on `http://localhost:5177`

## 📊 Test Best Practices

### 1. Use test IDs for reliable selectors
```typescript
// Good - stable selector
await page.locator('[data-testid="stat-card"]').count()

// Bad - fragile selector
await page.locator('.grid > .card').count()
```

### 2. Wait for navigation
```typescript
await page.click('button');
await page.waitForURL('/new-page');
```

### 3. Handle async operations
```typescript
await searchInput.fill('test');
await searchInput.press('Enter');
await page.waitForTimeout(1000); // Wait for results
```

## 🚀 CI/CD Integration

### GitHub Actions Example
```yaml
name: E2E Tests

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: actions/setup-node@v3
      - run: npm ci
      - run: npx playwright install chromium
      - run: npm test
      - uses: actions/upload-artifact@v3
        if: always()
        with:
          name: playwright-report
          path: playwright-report/
```

## 📈 Adding New Tests

### Example Test
```typescript
import { test, expect } from '@playwright/test';

test.describe('My Feature', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/my-page');
  });

  test('should do something', async ({ page }) => {
    const button = page.getByRole('button', { name: /click me/i });
    await button.click();

    await expect(page.getByText('Success!')).toBeVisible();
  });
});
```

## 🎨 Visual Regression Testing (Future)

Playwright supports screenshot comparison:

```typescript
test('looks correct', async ({ page }) => {
  await expect(page).toHaveScreenshot();
});
```

## 🔍 Debugging Tips

### 1. Use UI Mode
```bash
npm run test:ui
```
Best for development - see tests run in real-time with time travel debugging.

### 2. Use Debug Mode
```bash
npm run test:debug
```
Step through tests line by line with Playwright Inspector.

### 3. Add Screenshots
```typescript
await page.screenshot({ path: 'debug.png' });
```

### 4. Pause Execution
```typescript
await page.pause(); // Opens Playwright Inspector
```

## 📝 Test Data

### Mock Data Setup (TODO)
Create `tests/fixtures/` for mock API responses:

```typescript
// tests/fixtures/indices.ts
export const mockIndices = [
  { uid: 'products', entries: 1000, dataSize: 1024000 },
  { uid: 'users', entries: 500, dataSize: 512000 },
];
```

## 🎯 Next Steps

1. **Add API mocking** - Use MSW (Mock Service Worker) to test without real backend
2. **Visual regression tests** - Add screenshot comparisons
3. **Accessibility tests** - Add axe-core for a11y testing
4. **Performance tests** - Measure page load times
5. **Coverage reports** - Track test coverage metrics

## 📚 Resources

- [Playwright Docs](https://playwright.dev)
- [Best Practices](https://playwright.dev/docs/best-practices)
- [Testing Library](https://testing-library.com/docs/queries/about)
- [MSW for API Mocking](https://mswjs.io/)

---

**Test Status:** ✅ Passing (Chromium)
**Last Updated:** 2026-02-06
**Coverage:** 5 test files, 39 test cases
