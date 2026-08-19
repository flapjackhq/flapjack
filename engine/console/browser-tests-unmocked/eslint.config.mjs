import playwright from 'eslint-plugin-playwright';
import tseslint from 'typescript-eslint';

export default tseslint.config({
  ...playwright.configs['flat/recommended'],
  files: ['**/*.spec.ts'],
  languageOptions: {
    parser: tseslint.parser,
    parserOptions: { ecmaVersion: 'latest', sourceType: 'module' },
  },
  rules: {
    ...playwright.configs['flat/recommended'].rules,
    'playwright/no-eval': 'error',
    'playwright/no-raw-locators': 'error',
    'playwright/prefer-native-locators': 'error',
    'playwright/no-element-handle': 'error',
    'playwright/no-page-pause': 'error',
    'playwright/no-force-option': 'error',
    'no-restricted-syntax': [
      'error',
      {
        selector: "MemberExpression[object.name='request']",
        message: 'API calls belong in fixtures.ts, not browser specs.',
      },
      {
        selector: "CallExpression[callee.property.name='waitForTimeout']",
        message: 'Use web-first assertions instead of fixed waits.',
      },
      {
        selector: "CallExpression[callee.property.name='dispatchEvent']",
        message: 'Use simulated-human actions.',
      },
    ],
  },
});
