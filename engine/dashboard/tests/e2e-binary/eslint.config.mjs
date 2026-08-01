import playwright from 'eslint-plugin-playwright';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  {
    files: ['**/*.spec.ts'],
    languageOptions: {
      parser: tseslint.parser,
      parserOptions: {
        ecmaVersion: 'latest',
        sourceType: 'module',
      },
    },
    plugins: {
      playwright,
    },
    rules: {
      'playwright/no-eval': 'error',
      'playwright/no-element-handle': 'error',
      'playwright/no-force-option': 'error',
      'playwright/no-page-pause': 'error',
      'playwright/prefer-native-locators': 'error',
      'no-restricted-syntax': [
        'error',
        {
          selector: "CallExpression[callee.object.name='request'][callee.property.name='get']",
          message: 'API calls (request.get) are banned in spec files.',
        },
        {
          selector: "CallExpression[callee.object.name='request'][callee.property.name='post']",
          message: 'API calls (request.post) are banned in spec files.',
        },
        {
          selector: "CallExpression[callee.object.name='request'][callee.property.name='delete']",
          message: 'API calls (request.delete) are banned in spec files.',
        },
        {
          selector: "CallExpression[callee.object.name='request'][callee.property.name='put']",
          message: 'API calls (request.put) are banned in spec files.',
        },
        {
          selector: "CallExpression[callee.object.name='request'][callee.property.name='patch']",
          message: 'API calls (request.patch) are banned in spec files.',
        },
        {
          selector: "CallExpression[callee.property.name='waitForTimeout']",
          message: 'waitForTimeout is banned. Use Playwright auto-waiting or assertion timeouts instead.',
        },
        {
          selector: "CallExpression[callee.property.name='dispatchEvent']",
          message: 'dispatchEvent is banned. Use real user interactions.',
        },
        {
          selector: "CallExpression[callee.property.name='setExtraHTTPHeaders']",
          message: 'setExtraHTTPHeaders is banned in spec files.',
        },
        {
          selector: "CallExpression[callee.property.name='locator'] > Literal[value=/^\\./]",
          message: 'CSS class selectors are banned in spec files. Use data-testid or role/text locators.',
        },
        {
          selector: "CallExpression[callee.property.name='locator'] > Literal[value=/^\\/\\//]",
          message: 'XPath selectors are banned in spec files.',
        },
        {
          selector: "CallExpression[callee.property.name='locator'] > Literal[value=/^xpath=/]",
          message: 'XPath selectors are banned in spec files.',
        },
        {
          selector: "CallExpression[callee.property.name='locator'] > Literal[value=/\\[.*=/]",
          message: 'Attribute selectors are banned in spec files. Use data-testid or role/text locators.',
        },
      ],
    },
  },
);
