import { useState } from 'react';
import { Outlet } from 'react-router-dom';
import { Header } from './Header';
import { Sidebar } from './Sidebar';
import { ApiLogger } from './ApiLogger';
import { DevModePanel } from './DevModePanel';
import { useHealth } from '@/hooks/useHealth';
import { AlertTriangle } from 'lucide-react';

export function Layout() {
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const health = useHealth();
  const isDisconnected = health.isError || (health.data?.status !== 'ok' && !health.isLoading);

  return (
    <div className="flex h-screen w-full min-w-0 flex-col" data-testid="app-shell">
      <Header onMenuToggle={() => setSidebarOpen((o) => !o)} />
      {isDisconnected && (
        // The backend host can be one unbroken token, so the banner must own
        // wrapping instead of giving the document an intrinsic minimum width.
        <div className="flex min-w-0 flex-wrap items-center justify-center gap-2 bg-red-600 px-4 py-2 text-center text-sm font-medium text-white" data-testid="disconnected-banner">
          <AlertTriangle className="h-4 w-4 shrink-0" />
          <span className="min-w-0 break-words">Server disconnected — check that Flapjack is running on {import.meta.env.DEV ? new URL(__BACKEND_URL__).host : window.location.host}</span>
        </div>
      )}
      <DevModePanel />
      {/* Flex children default to min-width:auto; these boundaries let the
          existing main scroller contain wide route content locally. */}
      <div className="flex min-w-0 flex-1 overflow-hidden">
        <Sidebar open={sidebarOpen} onClose={() => setSidebarOpen(false)} />
        <main className="min-w-0 flex-1 overflow-auto bg-muted/30 p-6" data-testid="app-shell-main">
          <Outlet />
        </main>
      </div>
      <ApiLogger />
    </div>
  );
}
