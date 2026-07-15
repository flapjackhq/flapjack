import { describe, expect, it } from 'vitest';
import {
  MissingAlgoliaCredentialsError,
  resolveAlgoliaCredentialMode,
} from './algolia.fixture';

describe('resolveAlgoliaCredentialMode', () => {
  it('runs when Algolia credentials are present outside CI', () => {
    expect(resolveAlgoliaCredentialMode({ hasCredentials: true, isCI: false })).toBe('run');
  });

  it('runs when Algolia credentials are present in CI', () => {
    expect(resolveAlgoliaCredentialMode({ hasCredentials: true, isCI: true })).toBe('run');
  });

  it('skips local runs without Algolia credentials', () => {
    expect(resolveAlgoliaCredentialMode({ hasCredentials: false, isCI: false })).toBe('skip');
  });

  it('fails CI runs without Algolia credentials', () => {
    expect(resolveAlgoliaCredentialMode({ hasCredentials: false, isCI: true })).toBe('fail');
  });

  it('names both required CI credentials in the fail-closed error', () => {
    const error = new MissingAlgoliaCredentialsError();

    expect(error.name).toBe('MissingAlgoliaCredentialsError');
    expect(error.message).toContain('ALGOLIA_APP_ID');
    expect(error.message).toContain('ALGOLIA_ADMIN_KEY');
  });
});
