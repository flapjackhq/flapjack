import { expect, test } from '@playwright/test';

const DEFAULT_CONTENT_SECURITY_POLICY =
  "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";
const CSP_VIOLATION_PATTERN = /Content Security Policy|Refused to (execute|load|connect|apply)/;

function requiredEnv(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) {
    throw new Error(`${name} must be set`);
  }
  return value;
}

test('binary-served dashboard runs under the default content security policy', async ({
  page,
}) => {
  const baseUrl = requiredEnv('FJ_BINARY_BASE_URL');
  const adminKey = requiredEnv('FJ_BINARY_ADMIN_KEY');
  const collectedMessages: string[] = [];

  page.on('console', (message) => {
    collectedMessages.push(`console:${message.type()}:${message.text()}`);
  });
  page.on('pageerror', (error) => {
    collectedMessages.push(`pageerror:${error.message}`);
  });

  const response = await page.goto(`${baseUrl}/dashboard/`, {
    waitUntil: 'domcontentloaded',
  });

  expect(response, 'dashboard navigation response').not.toBeNull();
  expect(response?.headers()['content-security-policy']).toBe(
    DEFAULT_CONTENT_SECURITY_POLICY,
  );

  await expect(page.getByTestId('auth-gate')).toContainText('Welcome to Flapjack');
  await page.getByTestId('auth-key-input').fill(adminKey);
  const authenticatedIndexesResponse = page.waitForResponse(
    (authResponse) =>
      authResponse.url() === `${baseUrl}/1/indexes` && authResponse.status() === 200,
  );
  await page.getByTestId('auth-submit').click();
  await authenticatedIndexesResponse;
  await expect(page.getByTestId('auth-error')).toBeHidden();
  await expect(page.getByTestId('stat-card-indexes')).toContainText('Indexes');
  await expect(page.getByTestId('stat-card-status')).toContainText('Status');

  const cspViolations = collectedMessages.filter((message) =>
    CSP_VIOLATION_PATTERN.test(message),
  );
  console.log(`CSP_MESSAGE_DENOMINATOR=${collectedMessages.length}`);
  expect(cspViolations).toEqual([]);
});
