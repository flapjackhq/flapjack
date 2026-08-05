import { create } from 'zustand';
import { persist } from 'zustand/middleware';

interface AuthStore {
  appId: string;
  isAuthenticated: boolean;
  login: (key: string) => Promise<boolean>;
  logout: () => Promise<void>;
  setAppId: (id: string) => void;
  setSessionAuthenticated: (authenticated: boolean) => void;
}

export const useAuth = create<AuthStore>()(
  persist(
    (set) => ({
      appId: 'flapjack',
      isAuthenticated: false,
      login: async (key: string) => {
        const response = await fetch('/1/dashboard/session', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ apiKey: key }),
        });
        const authenticated = response.ok;
        set({ isAuthenticated: authenticated });
        return authenticated;
      },
      logout: async () => {
        const appId = useAuth.getState().appId || 'flapjack';
        await fetch('/1/dashboard/session', {
          method: 'DELETE',
          headers: { 'x-algolia-application-id': appId },
        });
        set({ isAuthenticated: false });
      },
      setAppId: (id: string) => {
        set({ appId: id });
      },
      setSessionAuthenticated: (authenticated: boolean) => {
        set({ isAuthenticated: authenticated });
      },
    }),
    {
      name: 'flapjack-auth',
      version: 1,
      partialize: (state) => ({ appId: state.appId }),
      migrate: (persistedState) => {
        const state = persistedState as { appId?: unknown } | undefined;
        return {
          appId: typeof state?.appId === 'string' ? state.appId : 'flapjack',
        };
      },
    }
  )
);

// Header-key authentication remains a server capability because SDKs,
// InstantSearch clients, and HTTP probes depend on Algolia-compatible headers.
// Only the browser dashboard switches to its HttpOnly session cookie.
