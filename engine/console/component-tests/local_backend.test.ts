import { describe, expect, it } from 'vitest';
import { requireOwnedTestBackend } from '../browser-tests-unmocked/local_backend';

const OWNERSHIP_TOKEN = '00000000-0000-4000-8000-000000000000';

describe('unmocked browser backend safety', () => {
  it.each([
    'http://127.0.0.1:7700',
    'http://localhost:7700/path',
    'http://[::1]:7700',
  ])('accepts the owned loopback HTTP boundary: %s', (value) => {
    expect(requireOwnedTestBackend(value, OWNERSHIP_TOKEN)).toMatch(/^http:/);
  });

  it.each([
    'https://127.0.0.1:7700',
    'https://api.example.com',
    'http://192.0.2.10:7700',
    'not a URL',
  ])('refuses a backend that the fixture must not mutate: %s', (value) => {
    expect(() => requireOwnedTestBackend(value, OWNERSHIP_TOKEN)).toThrow(/loopback|valid/);
  });

  it('refuses direct invocation without a runner-owned test-instance token', () => {
    expect(() => requireOwnedTestBackend('http://127.0.0.1:7700', '')).toThrow(
      /test-instance token/
    );
  });
});
