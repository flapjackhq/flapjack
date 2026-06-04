import { useMemo } from 'react';
import { Area, AreaChart, ResponsiveContainer } from 'recharts';
import { ArrowDownRight, ArrowUpRight, type LucideIcon, Minus } from 'lucide-react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';

interface DeltaBadgeProps {
  current: unknown;
  previous?: unknown;
  invertColor?: boolean;
}

interface KpiCardProps {
  title: string;
  value: unknown;
  prevValue?: unknown;
  loading: boolean;
  icon: LucideIcon;
  sparkData?: any[];
  sparkKey?: string;
  format?: 'number' | 'percent' | 'decimal';
  invertDelta?: boolean;
  emptyText?: string;
  tooltip?: string;
  testId?: string;
}

function buildKpiSlug(title: string) {
  return title.trim().toLowerCase().replace(/\s+/g, '-');
}

function buildKpiTitleId(title: string, prefix: 'kpi' | 'spark') {
  return `${prefix}-${buildKpiSlug(title)}`;
}

function formatKpiValue(value: unknown, format: NonNullable<KpiCardProps['format']>) {
  if (value == null) return null;
  if (format === 'percent') return `${(Number(value) * 100).toFixed(1)}%`;
  if (format === 'decimal') return Number(value).toFixed(1);
  return typeof value === 'number' ? value.toLocaleString() : String(value);
}

function KpiSparkline({
  data,
  dataKey,
  gradientId,
}: {
  data: any[];
  dataKey: string;
  gradientId: string;
}) {
  return (
    <div className="h-8 w-full" data-testid="sparkline">
      <ResponsiveContainer width="100%" height="100%" minWidth={1}>
        <AreaChart data={data}>
          <defs>
            <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="hsl(var(--primary))" stopOpacity={0.3} />
              <stop offset="100%" stopColor="hsl(var(--primary))" stopOpacity={0} />
            </linearGradient>
          </defs>
          <Area
            type="monotone"
            dataKey={dataKey}
            stroke="hsl(var(--primary))"
            strokeWidth={1.5}
            fill={`url(#${gradientId})`}
            dot={false}
            isAnimationActive={false}
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
}

export function KpiCard({
  title,
  value,
  prevValue,
  loading,
  icon: Icon,
  sparkData,
  sparkKey,
  format = 'number',
  invertDelta,
  emptyText,
  tooltip,
  testId,
}: KpiCardProps) {
  const formattedValue = useMemo(() => formatKpiValue(value, format), [value, format]);
  const sparkGradientId = buildKpiTitleId(title, 'spark');

  return (
    <Card className="relative group" data-testid={testId || buildKpiTitleId(title, 'kpi')}>
      {tooltip && (
        <div className="absolute top-2 right-2 opacity-0 group-hover:opacity-100 transition-opacity">
          <div className="relative">
            <div className="hidden group-hover:block absolute right-0 top-6 w-56 p-2 bg-popover border border-border rounded-md shadow-md text-xs text-muted-foreground z-10">
              {tooltip}
            </div>
          </div>
        </div>
      )}
      <CardHeader className="pb-1">
        <CardTitle className="text-xs font-medium text-muted-foreground flex items-center gap-1.5 uppercase tracking-wide">
          <Icon className="h-3.5 w-3.5" />
          {title}
        </CardTitle>
      </CardHeader>
      <CardContent className="pb-3">
        {loading ? (
          <Skeleton className="h-8 w-20" />
        ) : formattedValue != null ? (
          <div className="space-y-1">
            <div className="flex items-baseline gap-2">
              <span className="text-2xl font-bold tabular-nums" data-testid="kpi-value">
                {formattedValue}
              </span>
              <DeltaBadge current={value} previous={prevValue} invertColor={invertDelta} />
            </div>
            {sparkData?.length ? (
              <KpiSparkline
                data={sparkData}
                dataKey={sparkKey || 'count'}
                gradientId={sparkGradientId}
              />
            ) : null}
          </div>
        ) : (
          <div className="text-sm text-muted-foreground">{emptyText || 'No data'}</div>
        )}
      </CardContent>
    </Card>
  );
}

export function DeltaBadge({ current, previous, invertColor }: DeltaBadgeProps) {
  if (current == null || previous == null || previous === 0) return null;

  const delta = ((Number(current) - Number(previous)) / Math.abs(Number(previous))) * 100;
  if (!isFinite(delta)) return null;

  const isPositive = delta > 0;
  const isNeutral = Math.abs(delta) < 0.5;

  if (isNeutral) {
    return (
      <span className="inline-flex items-center gap-0.5 text-xs text-muted-foreground" data-testid="delta-badge">
        <Minus className="h-3 w-3" />
        0%
      </span>
    );
  }

  const isGood = invertColor ? !isPositive : isPositive;

  return (
    <span
      className={`inline-flex items-center gap-0.5 text-xs font-medium ${isGood ? 'text-green-600' : 'text-red-500'}`}
      data-testid="delta-badge"
    >
      {isPositive ? (
        <ArrowUpRight className="h-3 w-3" />
      ) : (
        <ArrowDownRight className="h-3 w-3" />
      )}
      {Math.abs(delta).toFixed(1)}%
    </span>
  );
}
