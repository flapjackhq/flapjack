import type { APIRequestContext, APIResponse } from '@playwright/test';
import { describe, expect, it, vi } from 'vitest';
import { isVectorSearchEnabled } from './api-helpers';

function healthRequest(body: Record<string, unknown>): APIRequestContext {
  const response = {
    json: vi.fn().mockResolvedValue(body),
    ok: () => true,
  } as unknown as APIResponse;

  return {
    get: vi.fn().mockResolvedValue(response),
  } as unknown as APIRequestContext;
}

describe('vector capability fixture', () => {
  it.each([
    [{ capabilities: { vectorSearch: true } }, true],
    [{ capabilities: { vectorSearch: false } }, false],
    [{ capabilities: {} }, false],
    [{}, false],
  ])('reads vector availability from an explicit enabled capability', async (body, expected) => {
    await expect(isVectorSearchEnabled(healthRequest(body))).resolves.toBe(expected);
  });
});
