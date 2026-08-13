/* @vitest-environment node */
import { describe, expect, it, vi } from 'vitest';
import type { APIRequestContext } from '@playwright/test';

import {
  requireHaHarness,
  SetupInfrastructureError,
} from './cluster-peers';

describe('requireHaHarness', () => {
  it('classifies standalone mode as setup infrastructure while failing the run', async () => {
    const request = {
      get: vi.fn().mockResolvedValue({
        status: () => 200,
        json: async () => ({
          node_id: 'standalone',
          replication_enabled: false,
        }),
      }),
    } as unknown as APIRequestContext;

    await expect(requireHaHarness(request)).rejects.toMatchObject({
      name: 'SetupInfrastructureError',
      message: expect.stringContaining('standalone mode (node_id=standalone)'),
    });
    await expect(requireHaHarness(request)).rejects.toBeInstanceOf(SetupInfrastructureError);
  });

  it('rejects a malformed HA payload before later helpers dereference peers', async () => {
    const request = {
      get: vi.fn().mockResolvedValue({
        status: () => 200,
        json: async () => ({
          node_id: 'ha-node',
          replication_enabled: true,
        }),
      }),
    } as unknown as APIRequestContext;

    await expect(requireHaHarness(request)).rejects.toMatchObject({
      name: 'SetupInfrastructureError',
      message: expect.stringContaining('peers_total, peers_healthy, and peers[]'),
    });
  });
});
