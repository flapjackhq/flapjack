import { Link } from 'react-router-dom';
import type { LucideIcon } from 'lucide-react';
import {
  Plus,
  Search,
  Users,
  AlertCircle,
  BarChart3,
  Trash2,
  Download,
  Upload,
  Eraser,
  Loader2,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
} from 'lucide-react';
import { AreaChart, Area, ResponsiveContainer, XAxis, YAxis, CartesianGrid, Tooltip } from 'recharts';
import type { Index } from '@/lib/types';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { InfoTooltip } from '@/components/ui/info-tooltip';
import { formatBytes, formatDate } from '@/lib/utils';

interface AnalyticsOverviewData {
  totalSearches?: number;
  uniqueUsers?: number;
  noResultRate?: number | null;
  dates?: Array<{ date: string; count: number }>;
  indices?: Array<{ index: string; searches: number }>;
}

interface OverviewHeaderProps {
  hasIndexes: boolean;
  importPending: boolean;
  exportPending: boolean;
  onUpload: () => void;
  onExportAll: () => void;
  onCreateIndex: () => void;
}

export function OverviewHeader({
  hasIndexes,
  importPending,
  exportPending,
  onUpload,
  onExportAll,
  onCreateIndex,
}: OverviewHeaderProps) {
  return (
    <div className="flex items-center justify-between">
      <h1 className="text-3xl font-bold">Overview</h1>
      <div className="flex gap-2">
        {hasIndexes && (
          <>
            <Button
              variant="outline"
              onClick={onUpload}
              disabled={importPending}
              data-testid="overview-upload-btn"
            >
              <Upload className="mr-2 h-4 w-4" /> Upload
            </Button>
            <Button
              variant="outline"
              onClick={onExportAll}
              disabled={exportPending}
              data-testid="overview-export-all-btn"
            >
              <Download className="mr-2 h-4 w-4" /> Export All
            </Button>
          </>
        )}
        <Button onClick={onCreateIndex}>
          <Plus className="mr-2 h-4 w-4" /> Create Index
        </Button>
      </div>
    </div>
  );
}

interface OverviewStatsCardsProps {
  indexesCount: number;
  totalDocs: number;
  totalSize: number;
  healthLoading: boolean;
  healthError: boolean;
  healthStatus?: string;
}

export function OverviewStatsCards({
  indexesCount,
  totalDocs,
  totalSize,
  healthLoading,
  healthError,
  healthStatus,
}: OverviewStatsCardsProps) {
  return (
    <div className="grid gap-4 grid-cols-2 md:grid-cols-4">
      <Card data-testid="stat-card-indexes">
        <CardHeader className="pb-2">
          <CardTitle className="text-sm font-medium text-muted-foreground flex items-center gap-1.5">
            Indexes
            <InfoTooltip content="Each index is an isolated data container with its own documents, settings, and search configuration." />
          </CardTitle>
        </CardHeader>
        <CardContent>
          <div className="text-2xl font-bold" data-testid="stat-value">{indexesCount}</div>
        </CardContent>
      </Card>
      <Card data-testid="stat-card-documents">
        <CardHeader className="pb-2">
          <CardTitle className="text-sm font-medium text-muted-foreground">
            Documents
          </CardTitle>
        </CardHeader>
        <CardContent>
          <div className="text-2xl font-bold" data-testid="stat-value">{totalDocs.toLocaleString()}</div>
        </CardContent>
      </Card>
      <Card data-testid="stat-card-storage">
        <CardHeader className="pb-2">
          <CardTitle className="text-sm font-medium text-muted-foreground">
            Storage
          </CardTitle>
        </CardHeader>
        <CardContent>
          <div className="text-2xl font-bold" data-testid="stat-value">{formatBytes(totalSize)}</div>
        </CardContent>
      </Card>
      <Card data-testid="stat-card-status">
        <CardHeader className="pb-2">
          <CardTitle className="text-sm font-medium text-muted-foreground flex items-center gap-1.5">
            Status
            <InfoTooltip content="Overall health of your Flapjack server. 'Healthy' means all systems are operational with no errors." />
          </CardTitle>
        </CardHeader>
        <CardContent>
          {healthLoading ? (
            <Skeleton className="h-8 w-28" />
          ) : healthError ? (
            <div className="text-2xl font-bold text-red-600">Disconnected</div>
          ) : healthStatus === 'ok' ? (
            <div className="text-2xl font-bold text-green-600">Healthy</div>
          ) : (
            <div className="text-2xl font-bold text-yellow-600">Unknown</div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

interface OverviewAnalyticsSectionProps {
  overview?: AnalyticsOverviewData;
  overviewLoading: boolean;
  indexes?: Index[];
  cleanupPending: boolean;
  cleanupSuccess: boolean;
  onOpenCleanup: () => void;
}

export function OverviewAnalyticsSection({
  overview,
  overviewLoading,
  indexes,
  cleanupPending,
  cleanupSuccess,
  onOpenCleanup,
}: OverviewAnalyticsSectionProps) {
  const totalSearches = overview?.totalSearches ?? 0;

  if (!overviewLoading && totalSearches <= 0) {
    return null;
  }

  return (
    <Card data-testid="overview-analytics">
      <CardHeader className="pb-2">
        <div className="flex items-center justify-between">
          <CardTitle className="text-base font-medium flex items-center gap-2">
            <BarChart3 className="h-4 w-4" />
            Search Analytics (Last 7 Days)
          </CardTitle>
          <div className="flex items-center gap-2">
            {cleanupSuccess && (
              <span className="text-xs text-green-600 flex items-center gap-1">
                <CheckCircle2 className="h-3 w-3" />
                Cleaned up
              </span>
            )}
            <Button
              variant="ghost"
              size="sm"
              onClick={onOpenCleanup}
              disabled={cleanupPending}
              title="Remove analytics data from deleted indexes"
              data-testid="overview-cleanup-btn"
            >
              {cleanupPending ? (
                <Loader2 className="h-4 w-4 mr-1.5 animate-spin" />
              ) : (
                <Eraser className="h-4 w-4 mr-1.5" />
              )}
              {cleanupPending ? 'Cleaning up...' : 'Cleanup'}
            </Button>
            {indexes?.[0] && (
              <Link to={`/index/${encodeURIComponent(indexes[0].uid)}/analytics`} className="text-xs text-primary hover:underline">View Details</Link>
            )}
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {overviewLoading ? (
          <div className="grid gap-4 grid-cols-2 md:grid-cols-4">
            {[1, 2, 3, 4].map((index) => <Skeleton key={index} className="h-16" />)}
          </div>
        ) : (
          <div className="space-y-4">
            <div className="grid gap-4 grid-cols-3">
              <OverviewKpi icon={Search} label="Total Searches" value={overview?.totalSearches?.toLocaleString() || '0'} />
              <OverviewKpi icon={Users} label="Unique Users" value={overview?.uniqueUsers?.toLocaleString() || '0'} />
              <OverviewKpi
                icon={AlertCircle}
                label="No-Result Rate"
                value={overview?.noResultRate != null ? `${(overview.noResultRate * 100).toFixed(1)}%` : '-'}
                warn={(overview?.noResultRate || 0) > 0.1}
              />
            </div>
            {overview?.dates && overview.dates.length > 0 && (
              <div className="h-32" data-testid="overview-analytics-chart">
                <ResponsiveContainer width="100%" height="100%" minWidth={1}>
                  <AreaChart data={overview.dates}>
                    <defs>
                      <linearGradient id="overviewGradient" x1="0" y1="0" x2="0" y2="1">
                        <stop offset="0%" stopColor="hsl(var(--primary))" stopOpacity={0.2} />
                        <stop offset="100%" stopColor="hsl(var(--primary))" stopOpacity={0} />
                      </linearGradient>
                    </defs>
                    <CartesianGrid strokeDasharray="3 3" className="stroke-border" vertical={false} />
                    <XAxis
                      dataKey="date"
                      className="text-xs"
                      tickFormatter={(dateText: string) => {
                        const date = new Date(`${dateText}T00:00:00`);
                        return date.toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
                      }}
                      tick={{ fill: 'hsl(var(--muted-foreground))' }}
                    />
                    <YAxis className="text-xs" tick={{ fill: 'hsl(var(--muted-foreground))' }} width={40} />
                    <Tooltip
                      contentStyle={{
                        background: 'hsl(var(--card))',
                        border: '1px solid hsl(var(--border))',
                        borderRadius: '8px',
                        fontSize: '13px',
                      }}
                      formatter={(value: number | undefined) => [Number(value ?? 0).toLocaleString(), 'Searches']}
                    />
                    <Area type="monotone" dataKey="count" stroke="hsl(var(--primary))" strokeWidth={2} fill="url(#overviewGradient)" />
                  </AreaChart>
                </ResponsiveContainer>
              </div>
            )}
            {overview?.indices && overview.indices.length > 1 && (
              <div className="text-xs text-muted-foreground">
                Across {overview.indices.length} indexes: {overview.indices.slice(0, 5).map((index) => `${index.index} (${index.searches})`).join(', ')}
                {overview.indices.length > 5 && ` and ${overview.indices.length - 5} more`}
              </div>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

interface OverviewIndexListSectionProps {
  indexes?: Index[];
  isLoading: boolean;
  error: unknown;
  paginatedIndexes: Index[];
  currentPage: number;
  totalPages: number;
  onPrevPage: () => void;
  onNextPage: () => void;
  onNavigateToIndex: (indexName: string) => void;
  onImport: (indexName: string) => void;
  onDeleteRequest: (indexName: string) => void;
  onExport: (indexName: string) => void;
  exportPending: boolean;
  importPending: boolean;
  deletePending: boolean;
}

export function OverviewIndexListSection({
  indexes,
  isLoading,
  error,
  paginatedIndexes,
  currentPage,
  totalPages,
  onPrevPage,
  onNextPage,
  onNavigateToIndex,
  onImport,
  onDeleteRequest,
  onExport,
  exportPending,
  importPending,
  deletePending,
}: OverviewIndexListSectionProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Indexes</CardTitle>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-2 py-2">
            {[1, 2, 3].map((index) => (
              <div key={index} className="flex items-center justify-between p-4 rounded-md border border-border">
                <div className="space-y-2 flex-1">
                  <Skeleton className="h-5 w-40" />
                  <Skeleton className="h-4 w-64" />
                </div>
                <div className="flex gap-2">
                  <Skeleton className="h-8 w-20 rounded-md" />
                  <Skeleton className="h-8 w-20 rounded-md" />
                </div>
              </div>
            ))}
          </div>
        ) : error ? (
          <div className="text-center py-8 text-red-600">
            Error loading indexes: {error instanceof Error ? error.message : 'Unknown error'}
          </div>
        ) : indexes && indexes.length > 0 ? (
          <div className="space-y-4">
            <div className="space-y-2">
              {paginatedIndexes.map((index) => (
                <OverviewIndexRow
                  key={index.uid}
                  index={index}
                  onNavigateToIndex={onNavigateToIndex}
                  onImport={onImport}
                  onDeleteRequest={onDeleteRequest}
                  onExport={onExport}
                  exportPending={exportPending}
                  importPending={importPending}
                  deletePending={deletePending}
                />
              ))}
            </div>

            {totalPages > 1 && (
              <div className="flex items-center justify-between pt-4 border-t">
                <div className="text-sm text-muted-foreground">
                  Showing {((currentPage - 1) * 10) + 1}-{Math.min(currentPage * 10, indexes.length)} of {indexes.length} indexes
                </div>
                <div className="flex gap-2">
                  <Button variant="outline" size="sm" onClick={onPrevPage} disabled={currentPage === 1}>
                    <ChevronLeft className="h-4 w-4" />
                    Previous
                  </Button>
                  <Button variant="outline" size="sm" onClick={onNextPage} disabled={currentPage === totalPages}>
                    Next
                    <ChevronRight className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            )}
          </div>
        ) : (
          <div className="text-center py-8 text-muted-foreground">
            No indexes yet. Create your first index to get started.
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function OverviewIndexRow({
  index,
  onNavigateToIndex,
  onImport,
  onDeleteRequest,
  onExport,
  exportPending,
  importPending,
  deletePending,
}: {
  index: Index;
  onNavigateToIndex: (indexName: string) => void;
  onImport: (indexName: string) => void;
  onDeleteRequest: (indexName: string) => void;
  onExport: (indexName: string) => void;
  exportPending: boolean;
  importPending: boolean;
  deletePending: boolean;
}) {
  const pending = index.numberOfPendingTasks ?? 0;
  const isHealthy = pending === 0;

  return (
    <div
      className="flex items-center justify-between p-4 rounded-md border border-border hover:bg-accent/50 transition-colors cursor-pointer"
      data-testid={`overview-index-row-${index.uid}`}
      onClick={() => onNavigateToIndex(index.uid)}
    >
      <div className="flex items-center gap-3">
        <span
          className={`inline-block h-2.5 w-2.5 rounded-full shrink-0 ${isHealthy ? 'bg-green-500' : 'bg-amber-500 animate-pulse'}`}
          title={isHealthy ? 'Healthy' : `${pending} pending task${pending !== 1 ? 's' : ''}`}
        />
        <div>
          <h3 className="font-medium">{index.uid}</h3>
          <p className="text-sm text-muted-foreground" data-testid={`overview-index-meta-${index.uid}`}>
            {index.entries?.toLocaleString() || 0} documents · {formatBytes(index.dataSize || 0)}
            {index.updatedAt && ` · Updated ${formatDate(index.updatedAt)}`}
            {pending > 0 && <span className="text-amber-600 dark:text-amber-400"> · {pending} pending</span>}
          </p>
        </div>
      </div>
      <div className="flex gap-2 items-center" onClick={(event) => event.stopPropagation()}>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => onExport(index.uid)}
          disabled={exportPending}
          title={`Export index "${index.uid}"`}
          data-testid={`overview-export-${index.uid}`}
        >
          <Download className="h-4 w-4" />
        </Button>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => onImport(index.uid)}
          disabled={importPending}
          title={`Import into index "${index.uid}"`}
          data-testid={`overview-import-${index.uid}`}
        >
          <Upload className="h-4 w-4" />
        </Button>
        <Link to={`/index/${encodeURIComponent(index.uid)}/settings`}>
          <Button variant="outline" size="sm">
            Settings
          </Button>
        </Link>
        <Button
          variant="ghost"
          size="sm"
          className="text-muted-foreground hover:text-destructive"
          onClick={() => onDeleteRequest(index.uid)}
          disabled={deletePending}
          title={`Delete index "${index.uid}"`}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}

function OverviewKpi({
  icon: Icon,
  label,
  value,
  warn,
}: {
  icon: LucideIcon;
  label: string;
  value: string;
  warn?: boolean;
}) {
  return (
    <div className="flex items-center gap-3 p-3 rounded-md bg-muted/30">
      <Icon className={`h-5 w-5 shrink-0 ${warn ? 'text-amber-500' : 'text-muted-foreground'}`} />
      <div>
        <div className={`text-lg font-bold tabular-nums ${warn ? 'text-amber-500' : ''}`}>{value}</div>
        <div className="text-xs text-muted-foreground">{label}</div>
      </div>
    </div>
  );
}
