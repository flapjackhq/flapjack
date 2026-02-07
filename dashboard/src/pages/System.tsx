import { memo } from 'react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Skeleton } from '@/components/ui/skeleton';
import { useHealthDetail, useInternalStatus } from '@/hooks/useSystemStatus';
import { useIndices } from '@/hooks/useIndices';
import {
  Activity,
  Server,
  Database,
  RefreshCw,
  CheckCircle,
  XCircle,
  Layers,
} from 'lucide-react';
import { formatBytes } from '@/lib/utils';

function HealthTab() {
  const { data, isLoading, isError, error } = useHealthDetail();

  if (isLoading) {
    return (
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {Array.from({ length: 5 }).map((_, i) => (
          <Card key={i}><CardContent className="pt-6"><Skeleton className="h-16" /></CardContent></Card>
        ))}
      </div>
    );
  }

  if (isError) {
    return (
      <Card>
        <CardContent className="pt-6">
          <div className="flex items-center gap-3 text-destructive">
            <XCircle className="h-5 w-5" />
            <div>
              <p className="font-medium">Failed to fetch health status</p>
              <p className="text-sm text-muted-foreground">{(error as Error)?.message}</p>
            </div>
          </div>
        </CardContent>
      </Card>
    );
  }

  const stats = [
    {
      label: 'Status',
      value: data?.status || 'unknown',
      icon: data?.status === 'ok' ? CheckCircle : XCircle,
      color: data?.status === 'ok' ? 'text-green-600 dark:text-green-400' : 'text-destructive',
    },
    {
      label: 'Active Writers',
      value: `${data?.active_writers ?? 0} / ${data?.max_concurrent_writers ?? 0}`,
      icon: Database,
      color: 'text-blue-600 dark:text-blue-400',
    },
    {
      label: 'Facet Cache',
      value: `${data?.facet_cache_entries ?? 0} / ${data?.facet_cache_cap ?? 0}`,
      icon: Layers,
      color: 'text-purple-600 dark:text-purple-400',
    },
  ];

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">Auto-refreshes every 5 seconds</p>
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {stats.map((stat) => (
          <Card key={stat.label}>
            <CardHeader className="flex flex-row items-center justify-between pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">
                {stat.label}
              </CardTitle>
              <stat.icon className={`h-5 w-5 ${stat.color}`} />
            </CardHeader>
            <CardContent>
              <p className="text-2xl font-bold">{stat.value}</p>
            </CardContent>
          </Card>
        ))}
      </div>
    </div>
  );
}

function IndicesTab() {
  const { data: indices, isLoading, isError } = useIndices();

  if (isLoading) {
    return (
      <div className="space-y-2">
        {Array.from({ length: 5 }).map((_, i) => (
          <Skeleton key={i} className="h-12 w-full" />
        ))}
      </div>
    );
  }

  if (isError || !indices) {
    return (
      <Card>
        <CardContent className="pt-6 text-center text-muted-foreground">
          Unable to load indices.
        </CardContent>
      </Card>
    );
  }

  const totalDocs = indices.reduce((sum, idx) => sum + (idx.entries ?? 0), 0);
  const totalSize = indices.reduce((sum, idx) => sum + (idx.dataSize ?? 0), 0);
  const pendingTasks = indices.reduce((sum, idx) => sum + (idx.numberOfPendingTasks ?? 0), 0);

  return (
    <div className="space-y-4">
      <div className="grid gap-4 sm:grid-cols-3">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">Total Indices</CardTitle>
          </CardHeader>
          <CardContent><p className="text-2xl font-bold">{indices.length}</p></CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">Total Documents</CardTitle>
          </CardHeader>
          <CardContent><p className="text-2xl font-bold">{totalDocs.toLocaleString()}</p></CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">Total Storage</CardTitle>
          </CardHeader>
          <CardContent><p className="text-2xl font-bold">{formatBytes(totalSize)}</p></CardContent>
        </Card>
      </div>

      {pendingTasks > 0 && (
        <Card>
          <CardContent className="pt-6">
            <div className="flex items-center gap-2 text-amber-600 dark:text-amber-400">
              <RefreshCw className="h-4 w-4 animate-spin" />
              <span className="text-sm font-medium">{pendingTasks} pending task(s) across indices</span>
            </div>
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Index Details</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b text-left text-muted-foreground">
                  <th className="pb-2 pr-4 font-medium">Name</th>
                  <th className="pb-2 pr-4 font-medium text-right">Documents</th>
                  <th className="pb-2 pr-4 font-medium text-right">Size</th>
                  <th className="pb-2 font-medium text-right">Pending</th>
                </tr>
              </thead>
              <tbody>
                {indices.map((idx) => (
                  <tr key={idx.uid} className="border-b last:border-0">
                    <td className="py-2 pr-4 font-medium">{idx.uid}</td>
                    <td className="py-2 pr-4 text-right">{(idx.entries ?? 0).toLocaleString()}</td>
                    <td className="py-2 pr-4 text-right">{formatBytes(idx.dataSize ?? 0)}</td>
                    <td className="py-2 text-right">
                      {(idx.numberOfPendingTasks ?? 0) > 0 ? (
                        <span className="text-amber-600 dark:text-amber-400">
                          {idx.numberOfPendingTasks}
                        </span>
                      ) : (
                        <span className="text-muted-foreground">0</span>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function ReplicationTab() {
  const { data, isLoading, isError, error } = useInternalStatus();

  if (isLoading) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-24" />
        <Skeleton className="h-24" />
      </div>
    );
  }

  if (isError) {
    return (
      <Card>
        <CardContent className="pt-6">
          <div className="flex items-center gap-3 text-muted-foreground">
            <Server className="h-5 w-5" />
            <div>
              <p className="font-medium">Replication status unavailable</p>
              <p className="text-sm">{(error as Error)?.message || 'Could not reach internal status endpoint.'}</p>
            </div>
          </div>
        </CardContent>
      </Card>
    );
  }

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">Auto-refreshes every 10 seconds</p>
      <div className="grid gap-4 sm:grid-cols-2">
        <Card>
          <CardHeader className="flex flex-row items-center justify-between pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">Node ID</CardTitle>
            <Server className="h-5 w-5 text-muted-foreground" />
          </CardHeader>
          <CardContent>
            <p className="text-sm font-mono break-all">{data?.node_id || 'N/A'}</p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="flex flex-row items-center justify-between pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">Replication</CardTitle>
            {data?.replication_enabled ? (
              <CheckCircle className="h-5 w-5 text-green-600 dark:text-green-400" />
            ) : (
              <XCircle className="h-5 w-5 text-muted-foreground" />
            )}
          </CardHeader>
          <CardContent>
            <p className="text-2xl font-bold">
              {data?.replication_enabled ? 'Enabled' : 'Disabled'}
            </p>
            {data?.replication_enabled && (
              <p className="text-sm text-muted-foreground mt-1">
                {data.peer_count} peer(s) connected
              </p>
            )}
          </CardContent>
        </Card>
      </div>

      {data?.ssl_renewal && (
        <Card>
          <CardHeader>
            <CardTitle className="text-base">SSL / TLS</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 text-sm">
            {data.ssl_renewal.certificate_expiry && (
              <p><span className="text-muted-foreground">Certificate expires:</span> {data.ssl_renewal.certificate_expiry}</p>
            )}
            {data.ssl_renewal.next_renewal && (
              <p><span className="text-muted-foreground">Next renewal:</span> {data.ssl_renewal.next_renewal}</p>
            )}
          </CardContent>
        </Card>
      )}
    </div>
  );
}

export const System = memo(function System() {
  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <Activity className="h-6 w-6" />
        <h1 className="text-2xl font-bold">System</h1>
      </div>

      <Tabs defaultValue="health">
        <TabsList>
          <TabsTrigger value="health">Health</TabsTrigger>
          <TabsTrigger value="indices">Indices</TabsTrigger>
          <TabsTrigger value="replication">Replication</TabsTrigger>
        </TabsList>

        <TabsContent value="health">
          <HealthTab />
        </TabsContent>

        <TabsContent value="indices">
          <IndicesTab />
        </TabsContent>

        <TabsContent value="replication">
          <ReplicationTab />
        </TabsContent>
      </Tabs>
    </div>
  );
});
