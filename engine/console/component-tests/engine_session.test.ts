import { describe, expect, it, vi } from 'vitest';
import { createEngineSessionProvider } from '../src/host/engine_session';
import type { ConsoleTransport } from '../src/lib/transport/console_transport';

const transport: ConsoleTransport = {
  listIndexes: async () => [],
  searchIndex: async () => {
    throw new Error('not used in P3a');
  },
};

describe('standalone engine session provider', () => {
  it('restores the existing HttpOnly session through the protected indexes route', async () => {
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(new Response(JSON.stringify({ items: [] }), { status: 200 }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    const provider = createEngineSessionProvider(fetcher, transport);

    const session = await provider.restore();
    expect(session?.transport).toBe(transport);
    expect(fetcher).toHaveBeenNthCalledWith(1, '/1/indexes', {
      method: 'GET',
      cache: 'no-store',
      headers: { 'x-algolia-application-id': 'flapjack' },
    });

    await session?.signOut();
    expect(fetcher).toHaveBeenNthCalledWith(2, '/1/dashboard/session', {
      method: 'DELETE',
      headers: { 'x-algolia-application-id': 'flapjack' },
    });
  });

  it('returns no session when the protected restore probe is refused', async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(new Response(null, { status: 403 }));
    const provider = createEngineSessionProvider(fetcher, transport);

    await expect(provider.restore()).resolves.toBeNull();
    expect(fetcher).toHaveBeenCalledExactlyOnceWith('/1/indexes', {
      method: 'GET',
      cache: 'no-store',
      headers: { 'x-algolia-application-id': 'flapjack' },
    });
    expect(fetcher.mock.calls.flatMap(([, init]) => [init?.method, init?.body])).not.toContain(
      'POST'
    );
    expect(JSON.stringify(fetcher.mock.calls)).not.toContain('apiKey');
  });

  it('creates and deletes the existing HttpOnly dashboard session contract', async () => {
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    const provider = createEngineSessionProvider(fetcher, transport);

    const session = await provider.signIn('admin-key');
    expect(session.transport).toBe(transport);
    expect(fetcher).toHaveBeenNthCalledWith(1, '/1/dashboard/session', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ apiKey: 'admin-key' }),
    });

    await session.signOut();
    expect(fetcher).toHaveBeenNthCalledWith(2, '/1/dashboard/session', {
      method: 'DELETE',
      headers: { 'x-algolia-application-id': 'flapjack' },
    });
  });

  it('rejects a failed sign-in without returning a session', async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(new Response(null, { status: 403 }));
    const provider = createEngineSessionProvider(fetcher, transport);

    await expect(provider.signIn('wrong-key')).rejects.toThrow('Authentication failed');
    expect(fetcher).toHaveBeenCalledOnce();
  });
});
