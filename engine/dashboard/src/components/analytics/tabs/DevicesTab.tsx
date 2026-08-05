import { useMemo } from 'react';
import {
  Area,
  AreaChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import {
  Monitor,
  Search,
  Smartphone,
  Tablet,
} from 'lucide-react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import type { TabProps } from '@/lib/analytics-types';
import { formatDateShort } from '@/lib/analytics-utils';
import { CHART_CANVAS_TEST_ID, CHART_MARK_TEST_ID } from '@/lib/constants';
import { useDeviceBreakdown } from '@/hooks/useAnalytics';
import { EmptyState, TableSkeleton } from '@/components/analytics/AnalyticsShared';

const PLATFORM_META: Record<string, { label: string; icon: any; color: string }> = {
  desktop: { label: 'Desktop', icon: Monitor, color: 'hsl(var(--primary))' },
  mobile: { label: 'Mobile', icon: Smartphone, color: 'hsl(210, 80%, 55%)' },
  tablet: { label: 'Tablet', icon: Tablet, color: 'hsl(150, 60%, 45%)' },
  unknown: { label: 'Unknown', icon: Search, color: 'hsl(var(--muted-foreground))' },
};

function buildDeviceChartData(
  dailyData: any[],
): Array<{ date: string; desktop: number; mobile: number; tablet: number }> {
  const countsByDate: Record<string, Record<string, number>> = {};
  for (const dayRow of dailyData) {
    if (!countsByDate[dayRow.date]) countsByDate[dayRow.date] = {};
    countsByDate[dayRow.date][dayRow.platform] = dayRow.count;
  }

  return Object.entries(countsByDate)
    .sort(([leftDate], [rightDate]) => leftDate.localeCompare(rightDate))
    .map(([date, counts]) => ({
      date: formatDateShort(date),
      desktop: counts.desktop || 0,
      mobile: counts.mobile || 0,
      tablet: counts.tablet || 0,
    }));
}

export function DevicesTab({ index, range }: TabProps) {
  const { data, isLoading } = useDeviceBreakdown(index, range);
  const platforms: any[] = data?.platforms || [];
  const dailyData: any[] = data?.dates || [];
  const total = platforms.reduce((sum: number, platform: any) => sum + (platform.count || 0), 0);
  const chartData = useMemo(() => buildDeviceChartData(dailyData), [dailyData]);

  if (isLoading) {
    return (
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        {[1, 2, 3].map((indexPosition) => (
          <Card key={indexPosition}><CardContent className="py-6"><TableSkeleton rows={1} /></CardContent></Card>
        ))}
      </div>
    );
  }

  if (!platforms.length || total === 0) {
    return (
      <Card>
        <CardContent className="py-12">
          <EmptyState
            icon={Smartphone}
            title="No device data"
            description="Device breakdown requires analytics_tags with platform:desktop, platform:mobile, or platform:tablet. Create a demo index to get sample data, or add analyticsTags to your search requests."
          />
        </CardContent>
      </Card>
    );
  }

  return (
    <div className="space-y-4">
      <DevicePlatformGrid platforms={platforms} total={total} />
      <DeviceTrendChart chartData={chartData} />
    </div>
  );
}

function DevicePlatformGrid({ platforms, total }: { platforms: any[]; total: number }) {
  return (
    <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
      {platforms
        .filter((platform) => platform.platform !== 'unknown')
        .map((platform) => {
          const platformMeta = PLATFORM_META[platform.platform] || PLATFORM_META.unknown;
          const PlatformIcon = platformMeta.icon;
          const sharePercentage = total > 0 ? ((platform.count / total) * 100).toFixed(1) : '0';

          return (
            <Card key={platform.platform} data-testid={`device-${platform.platform}`}>
              <CardContent className="py-5">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-3">
                    <div className="p-2 rounded-lg bg-muted">
                      <PlatformIcon className="h-5 w-5" />
                    </div>
                    <div>
                      <div className="text-sm font-medium text-muted-foreground">{platformMeta.label}</div>
                      <div className="text-2xl font-bold tabular-nums" data-testid="device-count">
                        {(platform.count as number).toLocaleString()}
                      </div>
                    </div>
                  </div>
                  <div className="text-lg font-semibold text-muted-foreground" data-testid="device-pct">
                    {sharePercentage}%
                  </div>
                </div>
                <div className="mt-3 h-1.5 bg-muted rounded-full overflow-hidden">
                  <div
                    className="h-full rounded-full transition-all"
                    style={{ width: `${sharePercentage}%`, backgroundColor: platformMeta.color }}
                  />
                </div>
              </CardContent>
            </Card>
          );
        })}
    </div>
  );
}

function DeviceTrendChart({
  chartData,
}: {
  chartData: Array<{ date: string; desktop: number; mobile: number; tablet: number }>;
}) {
  if (chartData.length === 0) return null;

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm font-medium">Searches by Device Over Time</CardTitle>
      </CardHeader>
      <CardContent>
        <div className="h-64" data-testid="device-chart">
          <ResponsiveContainer width="100%" height="100%">
            <AreaChart data={chartData} data-testid={CHART_CANVAS_TEST_ID}>
              <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
              <XAxis dataKey="date" className="text-xs" tick={{ fontSize: 11 }} />
              <YAxis className="text-xs" tick={{ fontSize: 11 }} />
              <Tooltip
                contentStyle={{
                  backgroundColor: 'hsl(var(--popover))',
                  border: '1px solid hsl(var(--border))',
                  borderRadius: '6px',
                  fontSize: '12px',
                }}
              />
              <Area data-testid={CHART_MARK_TEST_ID} type="monotone" dataKey="desktop" stackId="1" stroke={PLATFORM_META.desktop.color} fill={PLATFORM_META.desktop.color} fillOpacity={0.6} />
              <Area data-testid={CHART_MARK_TEST_ID} type="monotone" dataKey="mobile" stackId="1" stroke={PLATFORM_META.mobile.color} fill={PLATFORM_META.mobile.color} fillOpacity={0.6} />
              <Area data-testid={CHART_MARK_TEST_ID} type="monotone" dataKey="tablet" stackId="1" stroke={PLATFORM_META.tablet.color} fill={PLATFORM_META.tablet.color} fillOpacity={0.6} />
            </AreaChart>
          </ResponsiveContainer>
        </div>
      </CardContent>
    </Card>
  );
}
