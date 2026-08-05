/**
 * @module Application header component with branding, connection health indicator, indexing queue panel, and global action buttons.
 */
import { useState, useRef, useEffect } from 'react';
import { Link } from 'react-router-dom';
import { Moon, Sun, Settings, Menu, Loader2, ListTodo, Bug } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Card } from '@/components/ui/card';
import { useTheme } from '@/hooks/useTheme';
import { useAuth } from '@/hooks/useAuth';
import { useHealth } from '@/hooks/useHealth';
import { useIndexingStatus } from '@/hooks/useIndexingStatus';
import { ConnectionDialog } from './ConnectionDialog';
import { useDevMode } from '@/hooks/useDevMode';

interface HeaderProps {
  onMenuToggle?: () => void;
}

/**
 * Top-level application header bar displaying the Flapjack logo, server connection status, indexing queue popover, and toolbar buttons for API docs, dev mode, theme toggle, and connection settings.
 * 
 * @param onMenuToggle - Callback fired when the mobile hamburger menu button is clicked.
 */
export function Header({ onMenuToggle }: HeaderProps) {
  const { theme, toggleTheme } = useTheme();
  const { isAuthenticated } = useAuth();
  const health = useHealth();
  const { isIndexing, totalPending, activeTasks } = useIndexingStatus();
  const devMode = useDevMode();
  const [showSettings, setShowSettings] = useState(false);
  const [showQueue, setShowQueue] = useState(false);
  const queueRef = useRef<HTMLDivElement>(null);

  // Close queue panel on outside click
  useEffect(() => {
    if (!showQueue) return;
    function handleClick(e: MouseEvent) {
      if (queueRef.current && !queueRef.current.contains(e.target as Node)) {
        setShowQueue(false);
      }
    }
    document.addEventListener('mousedown', handleClick);
    return () => document.removeEventListener('mousedown', handleClick);
  }, [showQueue]);

  // In dev, show the actual backend server from vite.config.ts.
  // In prod the dashboard is served by the Flapjack binary itself, so window.location.host is correct.
  const serverHost = import.meta.env.DEV
    ? new URL(__BACKEND_URL__).host
    : window.location.host;
  const isHealthy = health.data?.status === 'ok';
  const isChecking = health.isLoading;

  return (
    <header
      className="flex h-16 min-w-0 items-center justify-between gap-2 border-b border-border bg-background px-2 shadow-sm sm:px-4 md:px-6"
      data-testid="app-shell-header"
    >
      {/* The identity/status side yields space first; the task controls on the
          right remain reachable at the 390px acceptance boundary. */}
      <div className="flex min-w-0 flex-1 items-center gap-2 md:gap-4" data-testid="app-shell-header-primary">
        <Button
          variant="ghost"
          size="icon"
          className="shrink-0 md:hidden"
          onClick={onMenuToggle}
          aria-label="Toggle navigation"
        >
          <Menu className="h-5 w-5" />
        </Button>
        <Link to="/" className="shrink-0 text-xl font-bold transition-opacity hover:opacity-80">
          <span className="text-2xl">🥞</span>
          <span className="hidden sm:inline"> Flapjack</span>
        </Link>
        <span className="hidden rounded bg-orange-500 px-1.5 py-0.5 text-[10px] font-bold uppercase leading-none tracking-wider text-white sm:inline-flex">Beta</span>
        <span className="hidden text-sm text-muted-foreground md:inline">{serverHost}</span>
        {isChecking ? (
          <span className="shrink-0 rounded-full bg-gray-100 px-2 py-0.5 text-xs text-gray-600 dark:bg-gray-800 dark:text-gray-400" data-testid="app-shell-connection-status">
            Connecting...
          </span>
        ) : isHealthy && isAuthenticated ? (
          <span className="shrink-0 rounded-full bg-green-100 px-2 py-0.5 text-xs text-green-700 dark:bg-green-900 dark:text-green-300" data-testid="app-shell-connection-status">
            Connected
          </span>
        ) : !isAuthenticated ? (
          <span className="shrink-0 rounded-full bg-yellow-100 px-2 py-0.5 text-xs text-yellow-700 dark:bg-yellow-900 dark:text-yellow-300" data-testid="app-shell-connection-status">
            Not Authenticated
          </span>
        ) : (
          <span className="shrink-0 rounded-full bg-red-100 px-2 py-0.5 text-xs text-red-700 dark:bg-red-900 dark:text-red-300" data-testid="app-shell-connection-status">
            Disconnected
          </span>
        )}
      </div>
      {/* Queue state and connection settings are the narrow-width task path.
          Lower-priority desktop affordances opt in again at md. */}
      <div className="flex shrink-0 items-center gap-1 md:gap-2" data-testid="app-shell-header-actions">
        {/* Queue / task status button */}
        <div className="relative shrink-0" ref={queueRef}>
          <Button
            variant="ghost"
            size="icon"
            title="Indexing queue"
            onClick={() => setShowQueue((v) => !v)}
            className="relative"
          >
            {isIndexing ? (
              <Loader2 className="h-5 w-5 text-blue-600 dark:text-blue-400 animate-spin" />
            ) : (
              <ListTodo className="h-5 w-5 text-muted-foreground" />
            )}
            {isIndexing && (
              <span className="absolute -top-0.5 -right-0.5 h-3 w-3 rounded-full bg-blue-500 border-2 border-background" />
            )}
          </Button>

          {showQueue && (
            <Card className="absolute right-0 top-12 w-72 z-50 shadow-lg border p-0 overflow-hidden">
              <div className="px-3 py-2 border-b bg-muted/50">
                <h3 className="text-sm font-medium">Indexing Queue</h3>
              </div>
              <div className="p-3">
                {activeTasks.length === 0 ? (
                  <p className="text-sm text-muted-foreground text-center py-4">
                    No active tasks. Queue is idle.
                  </p>
                ) : (
                  <div className="space-y-2">
                    {activeTasks.map((task) => {
                      const elapsed = Math.round((Date.now() - task.startedAt) / 1000);
                      return (
                        <div
                          key={task.taskID}
                          className="flex items-center justify-between text-sm p-2 rounded-md bg-blue-50 dark:bg-blue-950/30 border border-blue-200 dark:border-blue-800"
                        >
                          <div className="min-w-0">
                            <p className="font-medium truncate">{task.indexName}</p>
                            <p className="text-xs text-muted-foreground">
                              {task.documentCount} doc{task.documentCount !== 1 ? 's' : ''} · {elapsed}s
                            </p>
                          </div>
                          <Loader2 className="h-4 w-4 text-blue-500 animate-spin shrink-0 ml-2" />
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
              <div className="px-3 py-2 border-t bg-muted/30 text-center">
                <span className="text-xs text-muted-foreground">
                  {totalPending > 0
                    ? `${totalPending} task${totalPending !== 1 ? 's' : ''} in progress`
                    : 'All clear'}
                </span>
              </div>
            </Card>
          )}
        </div>

        <a className="hidden md:block" href="/swagger-ui" target="_blank" rel="noopener noreferrer">
          <Button variant="ghost" size="sm">
            API Docs
          </Button>
        </a>
        <Button
          variant="ghost"
          size="icon"
          onClick={devMode.toggle}
          aria-label="Toggle dev mode"
          title={devMode.enabled ? 'Disable dev mode' : 'Enable dev mode'}
          className={`hidden md:inline-flex ${devMode.enabled ? 'text-orange-500' : ''}`}
        >
          <Bug className="h-5 w-5" />
        </Button>
        <Button
          variant="ghost"
          size="icon"
          onClick={toggleTheme}
          aria-label="Toggle theme"
          title={`Switch to ${theme === 'light' ? 'dark' : 'light'} mode`}
          className="hidden md:inline-flex"
        >
          {theme === 'light' ? <Moon className="h-5 w-5" /> : <Sun className="h-5 w-5" />}
        </Button>
        <Button
          variant="ghost"
          size="icon"
          aria-label="Connection Settings"
          title="Connection Settings"
          onClick={() => setShowSettings(true)}
          className="shrink-0"
        >
          <Settings className="h-5 w-5" />
        </Button>
      </div>
      <ConnectionDialog open={showSettings} onOpenChange={setShowSettings} />
    </header>
  );
}
