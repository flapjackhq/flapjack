import { type ReactNode } from 'react';
import {
  Area,
  AreaChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { AlertCircle, type LucideIcon } from 'lucide-react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { formatDateLong, formatDateShort } from '@/lib/analytics-utils';

const ANALYTICS_ERROR_MESSAGE = 'Unable to load analytics data. Try again.';

export function formatAnalyticsErrorMessage(...errors: unknown[]): string | null {
  const firstError = errors.find((error) => error != null);
  if (!firstError) return null;

  return ANALYTICS_ERROR_MESSAGE;
}

export function AreaTrendCard({
  testId,
  title,
  loading,
  data,
  chartHeight,
  gradientId,
  gradientColor,
  dataKey,
  strokeColor,
  yAxisWidth = 40,
  yAxisFormatter,
  tooltipValueFormatter,
  seriesLabel,
  emptyState,
}: {
  testId: string;
  title: string;
  loading: boolean;
  data?: any[];
  chartHeight: number;
  gradientId: string;
  gradientColor: string;
  dataKey: string;
  strokeColor: string;
  yAxisWidth?: number;
  yAxisFormatter?: (value: number) => string;
  tooltipValueFormatter: (value: number) => string;
  seriesLabel: string;
  emptyState: ReactNode;
}) {
  return (
    <Card data-testid={testId}>
      <CardHeader className="pb-2">
        <CardTitle className="text-base font-medium">{title}</CardTitle>
      </CardHeader>
      <CardContent>
        {loading ? (
          <Skeleton className="w-full" style={{ height: chartHeight }} />
        ) : data?.length ? (
          <ResponsiveContainer width="100%" height={chartHeight} minWidth={1}>
            <AreaChart data={data}>
              <defs>
                <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0%" stopColor={gradientColor} stopOpacity={0.2} />
                  <stop offset="100%" stopColor={gradientColor} stopOpacity={0} />
                </linearGradient>
              </defs>
              <CartesianGrid strokeDasharray="3 3" className="stroke-border" vertical={false} />
              <XAxis
                dataKey="date"
                className="text-xs"
                tickFormatter={(date: string) => formatDateShort(date)}
                tick={{ fill: 'hsl(var(--muted-foreground))' }}
              />
              <YAxis
                className="text-xs"
                width={yAxisWidth}
                tickFormatter={yAxisFormatter}
                tick={{ fill: 'hsl(var(--muted-foreground))' }}
              />
              <Tooltip
                contentStyle={{
                  background: 'hsl(var(--card))',
                  border: '1px solid hsl(var(--border))',
                  borderRadius: '8px',
                  fontSize: '13px',
                }}
                labelFormatter={(date: any) => formatDateLong(String(date))}
                formatter={(value: any) => [tooltipValueFormatter(Number(value)), seriesLabel]}
              />
              <Area
                type="monotone"
                dataKey={dataKey}
                stroke={strokeColor}
                strokeWidth={2}
                fill={`url(#${gradientId})`}
              />
            </AreaChart>
          </ResponsiveContainer>
        ) : (
          emptyState
        )}
      </CardContent>
    </Card>
  );
}

export function EmptyState({
  icon: Icon,
  title,
  description,
  positive,
}: {
  icon: LucideIcon;
  title: string;
  description: string;
  positive?: boolean;
}) {
  return (
    <div className="py-12 text-center" data-testid="empty-state">
      <Icon className={`h-12 w-12 mx-auto mb-4 ${positive ? 'text-green-500/60' : 'text-muted-foreground/30'}`} />
      <h3 className="text-base font-medium mb-1">{title}</h3>
      <p className="text-sm text-muted-foreground max-w-sm mx-auto">{description}</p>
    </div>
  );
}

export function ErrorState({ message }: { message: string }) {
  return (
    <div className="py-8 text-center" data-testid="error-state">
      <AlertCircle className="h-8 w-8 mx-auto mb-2 text-red-500/60" />
      <p className="text-sm text-red-600">Error: {message}</p>
    </div>
  );
}

export function TableSkeleton({ rows }: { rows: number }) {
  return (
    <div className="space-y-3" data-testid="table-skeleton">
      {Array.from({ length: rows }).map((_, index) => (
        <div key={index} className="flex items-center gap-4">
          <Skeleton className="h-4 w-6" />
          <Skeleton className="h-4 flex-1 max-w-48" />
          <Skeleton className="h-4 w-16 ml-auto" />
        </div>
      ))}
    </div>
  );
}
