/**
 * @module Renders a collapsible API request log panel with expand/collapse, export, and clear functionality.
 */
import { useApiLogger } from '@/hooks/useApiLogger';
import { Button } from '@/components/ui/button';
import { ChevronDown, ChevronUp, Download, Trash2 } from 'lucide-react';
import { formatDuration } from '@/lib/utils';

/**
 * Collapsible bottom panel that displays a chronological log of API requests.
 * Shows the most recent request summary when collapsed; expands to a scrollable
 * list with status indicators, method, URL, and duration for each entry.
 * Provides controls to export the log as a file or clear all entries.
 */
export function ApiLogger() {
  const { entries, isExpanded, toggleExpanded, clear, exportAsFile } = useApiLogger();
  const lastEntry = entries[0];
  const lastRequestSummary = lastEntry
    ? `Last: ${lastEntry.method} ${lastEntry.url} - ${formatDuration(lastEntry.duration)}`
    : null;

  return (
    <div
      className={`w-full min-w-0 max-w-full border-t border-border bg-background transition-all duration-200 ${
        isExpanded ? 'h-[40vh]' : 'h-[50px]'
      }`}
      data-testid="app-shell-api-logger"
    >
      {/* Collapsed state */}
      <div className="flex h-[50px] min-w-0 items-center justify-between gap-2 border-b border-border px-2 sm:px-4">
        {/* Only the route-dependent summary yields width; the log toggle and
            action group stay operable when the URL is intrinsically long. */}
        <div className="flex min-w-0 flex-1 items-center gap-2 sm:gap-4" data-testid="api-logger-primary">
          <Button className="shrink-0 px-2 sm:px-3" variant="ghost" size="sm" onClick={toggleExpanded}>
            {isExpanded ? <ChevronDown className="h-4 w-4" /> : <ChevronUp className="h-4 w-4" />}
            <span className="ml-2">📋 API Log ({entries.length})</span>
          </Button>
          {lastRequestSummary && !isExpanded && (
            <span
              aria-label={lastRequestSummary}
              className="min-w-0 flex-1 truncate text-sm text-muted-foreground"
              data-testid="api-logger-summary"
              title={lastRequestSummary}
            >
              {lastRequestSummary}
            </span>
          )}
        </div>
        <div className="flex shrink-0 gap-0 sm:gap-2" data-testid="api-logger-actions">
          <Button className="px-2 sm:px-3" variant="ghost" size="sm" onClick={exportAsFile} disabled={entries.length === 0}>
            <Download className="h-4 w-4 mr-1" /> Export
          </Button>
          <Button className="px-2 sm:px-3" variant="ghost" size="sm" onClick={clear} disabled={entries.length === 0}>
            <Trash2 className="h-4 w-4 mr-1" /> Clear
          </Button>
        </div>
      </div>

      {/* Expanded state */}
      {isExpanded && (
        // Both axes belong to the panel: long methods and URLs must never turn
        // the root-level logger into document-width overflow.
        <div className="h-[calc(40vh-50px)] min-w-0 overflow-auto px-2 py-2 sm:px-4" data-testid="api-logger-entries">
          {entries.length === 0 ? (
            <p className="text-sm text-muted-foreground text-center py-8">
              No API requests yet. API calls will appear here.
            </p>
          ) : (
            <div className="space-y-2">
              {entries.map((entry) => (
                <div
                  key={entry.id}
                  className="p-3 rounded-md border border-border bg-card text-sm"
                >
                  <div className="flex min-w-0 items-start justify-between gap-2">
                    <span className="min-w-0 break-all font-medium">
                      <span className={entry.status === 'success' ? 'text-green-600' : entry.status === 'error' ? 'text-red-600' : 'text-yellow-600'}>
                        {entry.status === 'success' ? '✓' : entry.status === 'error' ? '✗' : '⏳'}
                      </span>
                      {' '}
                      {entry.method} {entry.url}
                    </span>
                    <span className="shrink-0 text-muted-foreground">{formatDuration(entry.duration)}</span>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
