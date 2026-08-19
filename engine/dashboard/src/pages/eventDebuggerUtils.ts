/**
 * @module Stub summary for eventDebuggerUtils.ts.
 */
import type { DebugEvent } from '@/hooks/useDebugEvents';

export type TimeRangePreset = '15m' | '1h' | '24h' | '7d' | 'all';

export interface DebugEventRow {
  event: DebugEvent;
  rowKey: string;
}

export interface VolumePoint {
  label: string;
  total: number;
  ok: number;
  error: number;
}

export function formatEventTooltipLabel(value: unknown, name: string | undefined): [string, string] {
  const seriesName = String(name);
  const label = seriesName === 'total' ? 'Total' : seriesName === 'ok' ? 'OK' : 'Error';
  return [String(value), label];
}

export function buildDebugEventIdentity(event: DebugEvent): string {
  return JSON.stringify([
    event.timestampMs,
    event.index,
    event.eventType,
    event.eventSubtype,
    event.eventName,
    event.userToken,
    event.httpCode,
    event.objectIds,
    event.validationErrors,
  ]);
}

export function buildDebugEventRows(events: DebugEvent[]): DebugEventRow[] {
  const duplicateCountsByIdentity = new Map<string, number>();

  return events.map((event) => {
    const baseIdentity = buildDebugEventIdentity(event);
    const duplicateOrdinal = duplicateCountsByIdentity.get(baseIdentity) ?? 0;
    duplicateCountsByIdentity.set(baseIdentity, duplicateOrdinal + 1);

    return {
      event,
      rowKey: `${baseIdentity}::${duplicateOrdinal}`,
    };
  });
}

export function formatTimestamp(ms: number): string {
  try {
    return new Date(ms).toISOString().replace('T', ' ').replace('Z', '').slice(0, 19);
  } catch {
    return String(ms);
  }
}

/**
 * TODO: Document resolveTimeRange.
 */
export function resolveTimeRange(range: TimeRangePreset, nowMs: number): { from?: number; until?: number } {
  if (range === 'all') {
    return {};
  }

  const minutesByPreset: Record<Exclude<TimeRangePreset, 'all'>, number> = {
    '15m': 15,
    '1h': 60,
    '24h': 24 * 60,
    '7d': 7 * 24 * 60,
  };
  const minutes = minutesByPreset[range];
  return {
    from: nowMs - minutes * 60_000,
    until: nowMs,
  };
}

/**
 * TODO: Document buildEventVolumeSeries.
 */
export function buildEventVolumeSeries(
  events: DebugEvent[],
  fromTimestampMs?: number,
  untilTimestampMs?: number,
): VolumePoint[] {
  if (events.length === 0) {
    return [];
  }

  let fromMs = fromTimestampMs ?? Math.min(...events.map((event) => event.timestampMs));
  let untilMs = untilTimestampMs ?? Math.max(...events.map((event) => event.timestampMs));
  if (untilMs < fromMs) {
    [fromMs, untilMs] = [untilMs, fromMs];
  }

  const bucketMs = resolveBucketSizeMs(untilMs - fromMs);
  const bucketCount = Math.max(1, Math.min(120, Math.floor((untilMs - fromMs) / bucketMs) + 1));
  const buckets = Array.from({ length: bucketCount }, (_, index) => {
    const bucketStart = fromMs + index * bucketMs;
    return {
      label: formatBucketLabel(bucketStart, bucketMs),
      total: 0,
      ok: 0,
      error: 0,
    };
  });

  for (const event of events) {
    if (event.timestampMs < fromMs || event.timestampMs > untilMs) {
      continue;
    }
    const bucketIdx = Math.min(
      buckets.length - 1,
      Math.floor((event.timestampMs - fromMs) / bucketMs),
    );
    buckets[bucketIdx].total += 1;
    if (event.httpCode === 200) {
      buckets[bucketIdx].ok += 1;
    } else {
      buckets[bucketIdx].error += 1;
    }
  }

  return buckets;
}

function resolveBucketSizeMs(spanMs: number): number {
  if (spanMs <= 60 * 60_000) {
    return 5 * 60_000;
  }
  if (spanMs <= 24 * 60 * 60_000) {
    return 60 * 60_000;
  }
  if (spanMs <= 7 * 24 * 60 * 60_000) {
    return 6 * 60 * 60_000;
  }
  return 24 * 60 * 60_000;
}

function formatBucketLabel(timestampMs: number, bucketMs: number): string {
  const date = new Date(timestampMs);
  if (bucketMs < 24 * 60 * 60_000) {
    return date.toISOString().slice(11, 16);
  }
  return date.toISOString().slice(5, 10);
}
