import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  hasAlgoliaCredentials,
  MissingAlgoliaCredentialsError,
  resolveAlgoliaCredentialMode,
} from './algolia.fixture';

afterEach(() => {
  vi.unstubAllEnvs();
});

describe('resolveAlgoliaCredentialMode', () => {
  it('runs when Algolia credentials are present', () => {
    expect(resolveAlgoliaCredentialMode({ hasCredentials: true })).toBe('run');
  });

  it('fails closed when Algolia credentials are missing', () => {
    expect(resolveAlgoliaCredentialMode({ hasCredentials: false })).toBe('fail');
  });

  it('names both required CI credentials in the fail-closed error', () => {
    const error = new MissingAlgoliaCredentialsError();

    expect(error.name).toBe('MissingAlgoliaCredentialsError');
    expect(error.message).toContain('ALGOLIA_APP_ID');
    expect(error.message).toContain('ALGOLIA_ADMIN_KEY');
  });
});

describe('hasAlgoliaCredentials', () => {
  it.each([
    ['neither credential', undefined, undefined, false],
    ['ALGOLIA_APP_ID only', 'test-app-id', undefined, false],
    ['ALGOLIA_ADMIN_KEY only', undefined, 'test-admin-key', false],
    ['both credentials', 'test-app-id', 'test-admin-key', true],
  ])('returns %s availability', (_label, appId, adminKey, expected) => {
    vi.stubEnv('ALGOLIA_APP_ID', appId);
    vi.stubEnv('ALGOLIA_ADMIN_KEY', adminKey);

    expect(hasAlgoliaCredentials()).toBe(expected);
  });
});
