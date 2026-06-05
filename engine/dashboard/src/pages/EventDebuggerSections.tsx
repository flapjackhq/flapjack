import { useState, useCallback } from 'react';
import { Area, AreaChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import { Copy, Check, X } from 'lucide-react';
import { Card } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Label } from '@/components/ui/label';
import { Skeleton } from '@/components/ui/skeleton';
import type { DebugEvent } from '@/hooks/useDebugEvents';
import type { DebugEventRow, TimeRangePreset, VolumePoint } from './eventDebuggerUtils';
import { formatEventTooltipLabel, formatTimestamp } from './eventDebuggerUtils';

const EVENT_TOOLTIP_STYLE = {
  background: 'hsl(var(--card))',
  border: '1px solid hsl(var(--border))',
  borderRadius: '8px',
  fontSize: '12px',
} as const;

export function EventDebuggerHeader({ eventCount }: { eventCount?: number }) {
  return (
    <div className="flex items-center justify-between">
      <div className="flex items-center gap-2">
        <h2 className="text-xl font-semibold">Event Debugger</h2>
        {eventCount !== undefined && (
          <Badge variant="secondary" data-testid="event-count">
            {eventCount}
          </Badge>
        )}
      </div>
    </div>
  );
}

interface EventFiltersPanelProps {
  eventTypeFilter: string;
  indexFilter: string;
  onEventTypeFilterChange: (value: string) => void;
  onIndexFilterChange: (value: string) => void;
  onStatusFilterChange: (value: string) => void;
  onTimeRangeChange: (value: TimeRangePreset) => void;
  statusFilter: string;
  timeRange: TimeRangePreset;
}

export function EventFiltersPanel({
  eventTypeFilter,
  indexFilter,
  onEventTypeFilterChange,
  onIndexFilterChange,
  onStatusFilterChange,
  onTimeRangeChange,
  statusFilter,
  timeRange,
}: EventFiltersPanelProps) {
  return (
    <div className="flex items-end gap-4 flex-wrap">
      <div className="space-y-1">
        <Label htmlFor="filter-date-range">Date Range</Label>
        <select
          id="filter-date-range"
          value={timeRange}
          className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
          onChange={(e) => onTimeRangeChange(e.target.value as TimeRangePreset)}
        >
          <option value="15m">Last 15 minutes</option>
          <option value="1h">Last 1 hour</option>
          <option value="24h">Last 24 hours</option>
          <option value="7d">Last 7 days</option>
          <option value="all">All available</option>
        </select>
      </div>
      <div className="space-y-1">
        <Label htmlFor="filter-status">Status</Label>
        <select
          id="filter-status"
          value={statusFilter}
          className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
          onChange={(e) => onStatusFilterChange(e.target.value)}
        >
          <option value="">All</option>
          <option value="ok">OK</option>
          <option value="error">Failed</option>
        </select>
      </div>
      <div className="space-y-1">
        <Label htmlFor="filter-event-type">Event Type</Label>
        <select
          id="filter-event-type"
          value={eventTypeFilter}
          className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
          onChange={(e) => onEventTypeFilterChange(e.target.value)}
        >
          <option value="">All</option>
          <option value="click">click</option>
          <option value="conversion">conversion</option>
          <option value="view">view</option>
        </select>
      </div>
      <div className="space-y-1">
        <Label htmlFor="filter-index">Index</Label>
        <input
          id="filter-index"
          type="text"
          placeholder="Filter by index..."
          value={indexFilter}
          className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
          onChange={(e) => onIndexFilterChange(e.target.value)}
        />
      </div>
    </div>
  );
}

interface EventVolumePanelProps {
  events: DebugEvent[];
  errorCount: number;
  isError: boolean;
  isLoading: boolean;
  okCount: number;
  volumeData: VolumePoint[];
}

export function EventVolumePanel({
  events,
  errorCount,
  isError,
  isLoading,
  okCount,
  volumeData,
}: EventVolumePanelProps) {
  if (isError || isLoading || events.length === 0) {
    return null;
  }

  return (
    <Card className="p-4 space-y-4" data-testid="event-volume-chart">
      <div className="flex items-center justify-between gap-3 flex-wrap">
        <h3 className="text-sm font-medium">Event Volume</h3>
        <div className="flex items-center gap-2 text-xs">
          <Badge variant="secondary">Total {events.length}</Badge>
          <Badge variant="secondary" className="bg-green-100 text-green-800">OK {okCount}</Badge>
          <Badge variant="destructive">Error {errorCount}</Badge>
        </div>
      </div>
      <ResponsiveContainer width="100%" height={220}>
        <AreaChart data={volumeData}>
          <defs>
            <linearGradient id="eventTotalGradient" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="hsl(var(--primary))" stopOpacity={0.25} />
              <stop offset="100%" stopColor="hsl(var(--primary))" stopOpacity={0} />
            </linearGradient>
          </defs>
          <CartesianGrid strokeDasharray="3 3" className="stroke-border" vertical={false} />
          <XAxis dataKey="label" tick={{ fontSize: 12, fill: 'hsl(var(--muted-foreground))' }} />
          <YAxis allowDecimals={false} width={36} tick={{ fontSize: 12, fill: 'hsl(var(--muted-foreground))' }} />
          <Tooltip contentStyle={EVENT_TOOLTIP_STYLE} formatter={formatEventTooltipLabel} />
          <Area type="monotone" dataKey="total" stroke="hsl(var(--primary))" fill="url(#eventTotalGradient)" strokeWidth={2} />
          <Area type="monotone" dataKey="error" stroke="#dc2626" fillOpacity={0} strokeWidth={1.5} />
        </AreaChart>
      </ResponsiveContainer>
    </Card>
  );
}

interface EventContentPanelProps {
  eventRows: DebugEventRow[];
  isError: boolean;
  isLoading: boolean;
  onSelectEventRow: (rowKey: string | null) => void;
  selectedEvent: DebugEvent | null;
  selectedEventRowKey: string | null;
}

export function EventContentPanel({
  eventRows,
  isError,
  isLoading,
  onSelectEventRow,
  selectedEvent,
  selectedEventRowKey,
}: EventContentPanelProps) {
  if (isError) {
    return (
      <Card className="p-8 text-center">
        <p className="text-sm text-destructive">Unable to load events. The debug endpoint may be unavailable.</p>
      </Card>
    );
  }

  if (isLoading) {
    return (
      <div className="space-y-2">
        {[1, 2, 3].map((i) => (
          <Card key={i} className="p-3">
            <Skeleton className="h-5 w-full" />
          </Card>
        ))}
      </div>
    );
  }

  if (eventRows.length === 0) {
    return (
      <Card className="p-8 text-center">
        <p className="text-sm text-muted-foreground">
          No events received yet — send events via the Insights API to see them here.
        </p>
      </Card>
    );
  }

  return (
    <div className="flex gap-4">
      <EventTable
        eventRows={eventRows}
        onSelectEventRow={onSelectEventRow}
        selectedEventRowKey={selectedEventRowKey}
      />
      {selectedEvent && <EventDetailPanel event={selectedEvent} onClose={() => onSelectEventRow(null)} />}
    </div>
  );
}

function EventTable({
  eventRows,
  onSelectEventRow,
  selectedEventRowKey,
}: {
  eventRows: DebugEventRow[];
  onSelectEventRow: (rowKey: string | null) => void;
  selectedEventRowKey: string | null;
}) {
  return (
    <div className="flex-1 overflow-auto" data-testid="event-table">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b text-left text-muted-foreground">
            <th className="p-2">Time</th>
            <th className="p-2">Index</th>
            <th className="p-2">Type</th>
            <th className="p-2">Name</th>
            <th className="p-2">User Token</th>
            <th className="p-2">Status</th>
          </tr>
        </thead>
        <tbody>
          {eventRows.map(({ event, rowKey }) => (
            <EventRow
              event={event}
              key={rowKey}
              isSelected={selectedEventRowKey === rowKey}
              onSelectEventRow={onSelectEventRow}
              rowKey={rowKey}
            />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function EventRow({
  event,
  isSelected,
  onSelectEventRow,
  rowKey,
}: {
  event: DebugEvent;
  isSelected: boolean;
  onSelectEventRow: (rowKey: string) => void;
  rowKey: string;
}) {
  return (
    <tr
      data-testid="event-row"
      className={`border-b cursor-pointer hover:bg-accent/50 transition-colors ${isSelected ? 'bg-accent' : ''}`}
      onClick={() => onSelectEventRow(rowKey)}
    >
      <td className="p-2 font-mono text-xs">{formatTimestamp(event.timestampMs)}</td>
      <td className="p-2">{event.index}</td>
      <td className="p-2">
        {event.eventType}
        {event.eventSubtype && <span className="text-muted-foreground ml-1">({event.eventSubtype})</span>}
      </td>
      <td className="p-2 truncate max-w-[200px]">{event.eventName}</td>
      <td className="p-2 font-mono text-xs truncate max-w-[120px]">{event.userToken}</td>
      <td className="p-2"><EventStatusBadge httpCode={event.httpCode} /></td>
    </tr>
  );
}

function EventStatusBadge({
  httpCode,
  includeCode,
}: {
  httpCode: number;
  includeCode?: boolean;
}) {
  if (httpCode === 200) {
    return (
      <Badge variant="secondary" className="bg-green-100 text-green-800">
        {includeCode ? '200 OK' : 'OK'}
      </Badge>
    );
  }

  return <Badge variant="destructive">{includeCode ? httpCode : 'Error'}</Badge>;
}

function EventDetailPanel({
  event,
  onClose,
}: {
  event: DebugEvent;
  onClose: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const payload = JSON.stringify(event, null, 2);

  const handleCopy = useCallback(async () => {
    try {
      if (!navigator.clipboard?.writeText) return;
      await navigator.clipboard.writeText(payload);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      setCopied(false);
    }
  }, [payload]);

  return (
    <Card className="w-80 p-4 space-y-3 shrink-0" data-testid="event-detail">
      <div className="flex items-center justify-between">
        <p className="font-medium text-sm">Event Detail</p>
        <div className="flex items-center gap-1">
          <Button variant="ghost" size="sm" aria-label="Copy payload" onClick={handleCopy}>
            {copied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
          </Button>
          <Button variant="ghost" size="sm" aria-label="Close detail panel" onClick={onClose}>
            <X className="h-3 w-3" />
          </Button>
        </div>
      </div>

      <div className="space-y-2 text-sm">
        <div>
          <span className="text-muted-foreground">Event Name: </span>
          <span>{event.eventName}</span>
        </div>
        <div>
          <span className="text-muted-foreground">Type: </span>
          <span>{event.eventType}</span>
          {event.eventSubtype && <span> ({event.eventSubtype})</span>}
        </div>
        <div>
          <span className="text-muted-foreground">Index: </span>
          <span>{event.index}</span>
        </div>
        <div>
          <span className="text-muted-foreground">User Token: </span>
          <span>{event.userToken}</span>
        </div>
        <div>
          <span className="text-muted-foreground">Object IDs: </span>
          <span className="font-mono text-xs">{event.objectIds.join(', ')}</span>
        </div>
        <div>
          <span className="text-muted-foreground">Status: </span>
          <EventStatusBadge httpCode={event.httpCode} includeCode />
        </div>
        <div>
          <span className="text-muted-foreground">Timestamp: </span>
          <span className="font-mono text-xs">{formatTimestamp(event.timestampMs)}</span>
        </div>

        {event.validationErrors.length > 0 && (
          <div className="rounded-md border border-destructive/50 bg-destructive/10 p-2">
            <p className="text-xs font-medium text-destructive mb-1">Validation Errors</p>
            <ul className="space-y-1">
              {event.validationErrors.map((err, index) => (
                <li key={index} className="text-xs text-destructive">
                  {err}
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>

      <div className="mt-2">
        <p className="text-xs text-muted-foreground mb-1">Full Payload</p>
        <pre className="text-xs bg-muted p-2 rounded overflow-auto max-h-60 font-mono">
          {payload}
        </pre>
      </div>
    </Card>
  );
}
