import type { ConsoleTransport } from '../lib/transport/console_transport';
import type { SessionProvider } from './session';

export function createEngineSessionProvider(
  fetcher: typeof fetch,
  transport: ConsoleTransport
): SessionProvider {
  function authenticatedSession() {
    return {
      transport,
      async signOut() {
        const response = await fetcher('/1/dashboard/session', {
          method: 'DELETE',
          headers: { 'x-algolia-application-id': 'flapjack' },
        });
        if (!response.ok) throw new Error('Sign out failed');
      },
    };
  }

  return {
    async restore() {
      const response = await fetcher('/1/indexes', {
        method: 'GET',
        cache: 'no-store',
        headers: { 'x-algolia-application-id': 'flapjack' },
      });
      return response.ok ? authenticatedSession() : null;
    },
    async signIn(apiKey) {
      const response = await fetcher('/1/dashboard/session', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ apiKey }),
      });
      if (!response.ok) throw new Error('Authentication failed');
      return authenticatedSession();
    },
  };
}
