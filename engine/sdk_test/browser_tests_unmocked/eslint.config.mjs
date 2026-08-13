import playwright from 'eslint-plugin-playwright';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  {
    ...playwright.configs['flat/recommended'],
    files: ['**/*.spec.mjs'],
    rules: {
      ...playwright.configs['flat/recommended'].rules,
      'playwright/no-eval': 'error',
      'playwright/no-raw-locators': ['error', { allowed: ['aside', 'tr', 'main', 'option'] }],
      'playwright/prefer-native-locators': 'error',
      'playwright/no-element-handle': 'error',
      'playwright/no-page-pause': 'error',
      'playwright/no-force-option': 'error',
      'no-restricted-syntax': ['error',
        {
          selector: "MemberExpression[object.name='request']",
          message: 'API calls are banned in browser spec files. Move setup to the fixture runner.',
        },
        {
          selector: "MemberExpression[property.name='evaluate']",
          message: 'page.evaluate() is banned in browser spec files.',
        },
        {
          selector: "CallExpression[callee.property.name='waitForTimeout']",
          message: 'Arbitrary waits are banned. Use Playwright auto-waiting.',
        },
        {
          selector: "CallExpression[callee.property.name='dispatchEvent']",
          message: 'Synthetic events are banned. Use visible browser controls.',
        },
        {
          selector: "CallExpression[callee.property.name='setExtraHTTPHeaders']",
          message: 'Invisible header manipulation is banned in browser spec files.',
        },
      ],
    },
  },
  {
    files: ['app/**/*.js'],
    languageOptions: {
      globals: {
        console: 'readonly',
        document: 'readonly',
        fetch: 'readonly',
        URL: 'readonly',
        URLSearchParams: 'readonly',
        window: 'readonly',
      },
    },
    rules: {
      'no-undef': 'error',
      'no-unused-vars': ['error', { argsIgnorePattern: '^_' }],
    },
  },
);
