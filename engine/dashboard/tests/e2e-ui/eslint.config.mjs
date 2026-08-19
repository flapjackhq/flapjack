/**
 * ESLint config for the browser test files under tests/e2e-ui/.
 *
 * Enforces the repository-owned browser testing standard at
 * `engine/dashboard/_dev/testing/TESTING.md`:
 * - No page.evaluate / page.$eval / page.$$eval / page.$()
 * - No raw CSS / XPath / attribute / tag locators outside the allow-list below
 * - No { force: true } on actions
 * - No page.pause() (debugging leftover)
 * - No API calls (request.*), waitForTimeout, dispatchEvent in spec files
 * - Everything else `playwright/flat/recommended` carries: no conditional
 *   expects, no skipped tests, web-first assertions, awaited assertions
 *
 * Fixture and setup files are exempt. Shared UI helpers are linted alongside spec files.
 *
 * `playwright/no-raw-locators` is the single owner of the locator bans, so this
 * config carries no duplicate custom locator regexes. Its `allowed` list is the
 * standard's verbatim row-scoping tag list — do not widen it.
 */
import playwright from 'eslint-plugin-playwright';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  {
    // Lint browser specs, focused helper tests, and shared UI helpers used by those specs.
    ...playwright.configs['flat/recommended'],
    files: ['**/*.spec.ts', '**/*.test.ts', '**/helpers.ts', '**/*_helpers.ts'],
    languageOptions: {
      parser: tseslint.parser,
      parserOptions: {
        ecmaVersion: 'latest',
        sourceType: 'module',
      },
    },
    rules: {
      ...playwright.configs['flat/recommended'].rules,

      // --- Layer 1: Playwright ESLint rules ---

      // Ban page.evaluate(), page.$eval(), page.$$eval()
      'playwright/no-eval': 'error',

      // Ban raw CSS class / XPath / attribute / tag locators.
      'playwright/no-raw-locators': ['error', {
        allowed: ['aside', 'tr', 'main', 'option'],
      }],

      // Prefer Playwright's semantic locator helpers over equivalent locator() queries
      'playwright/prefer-native-locators': 'error',

      // Ban deprecated page.$() / page.$$() handle API
      'playwright/no-element-handle': 'error',

      // Ban page.pause() (debugging leftover)
      'playwright/no-page-pause': 'error',

      // Ban { force: true } which bypasses actionability checks
      'playwright/no-force-option': 'error',

      // --- Layer 1: Custom banned patterns not owned by no-raw-locators ---
      'no-restricted-syntax': ['error',
        {
          selector: "MemberExpression[object.name='request']",
          message: 'API calls (request.*) are banned in spec files. Move to fixtures.ts.',
        },
        {
          selector: "MemberExpression[property.name='evaluate']",
          message: 'page.evaluate() is banned in spec files.',
        },
        {
          selector: "CallExpression[callee.property.name='waitForTimeout']",
          message: 'waitForTimeout is banned. Use Playwright auto-waiting or assertion timeouts instead.',
        },
        {
          selector: "CallExpression[callee.property.name='dispatchEvent']",
          message: 'dispatchEvent is banned. Use real user interactions (click, fill, etc.).',
        },
        {
          selector: "CallExpression[callee.property.name='setExtraHTTPHeaders']",
          message: 'setExtraHTTPHeaders is banned in spec files. Move to fixtures.ts.',
        },
      ],
    },
  },
);
