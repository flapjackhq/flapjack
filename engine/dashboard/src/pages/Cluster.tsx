import { memo, type ReactNode } from 'react';
import { Network, XCircle } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { useClusterStatus, type ClusterPeer, type ClusterPeerStatus } from '@/hooks/useClusterStatus';

type PeerStatusBadgeProps = {
  className: string;
  label: string;
  variant: 'default' | 'secondary' | 'destructive' | 'outline';
};

function getPeerStatusBadgeProps(status: ClusterPeerStatus): PeerStatusBadgeProps {
  switch (status) {
    case 'healthy':
      return {
        className: 'bg-green-100 text-green-800',
        label: 'Healthy',
        variant: 'secondary',
      };
    case 'stale':
      return {
        className: 'border-amber-300 text-amber-700',
        label: 'Stale',
        variant: 'outline',
      };
    case 'unhealthy':
      return {
        className: '',
        label: 'Unhealthy',
        variant: 'destructive',
      };
    case 'circuit_open':
      return {
        className: 'bg-orange-100 text-orange-800',
        label: 'Circuit Open',
        variant: 'secondary',
      };
    case 'never_contacted':
      return {
        className: 'border-slate-300 text-slate-600',
        label: 'Never Contacted',
        variant: 'outline',
      };
    default: {
      const exhaustiveStatus: never = status;
      return exhaustiveStatus;
    }
  }
}

function formatLastSuccess(lastSuccessSecsAgo: number | null): string {
  if (lastSuccessSecsAgo === null) {
    return 'Never';
  }

  if (lastSuccessSecsAgo < 1) {
    return '<1s ago';
  }

  if (lastSuccessSecsAgo < 60) {
    return `${Math.floor(lastSuccessSecsAgo)}s ago`;
  }

  if (lastSuccessSecsAgo < 3600) {
    return `${Math.floor(lastSuccessSecsAgo / 60)}m ago`;
  }

  return `${Math.floor(lastSuccessSecsAgo / 3600)}h ago`;
}

function getClusterErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message) {
    return error.message;
  }

  return 'Cluster status request failed.';
}

function ClusterErrorState({ error }: { error: unknown }) {
  return (
    <Card data-testid="cluster-error-state">
      <CardContent className="pt-6">
        <div className="flex items-center gap-3 text-destructive">
          <XCircle className="h-5 w-5" />
          <div>
            <p className="font-medium">Failed to fetch cluster status</p>
            <p className="text-sm text-muted-foreground">{getClusterErrorMessage(error)}</p>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

function StandaloneClusterState({ nodeId }: { nodeId: string }) {
  return (
    <Card data-testid="cluster-standalone-state">
      <CardHeader>
        <CardTitle className="text-base">Standalone mode</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <div>
          <p className="text-sm text-muted-foreground">Node ID</p>
          <p className="font-mono text-sm break-all" data-testid="cluster-node-id-value">{nodeId}</p>
        </div>
        <div>
          <p className="text-sm text-muted-foreground">Mode</p>
          <p className="text-sm font-medium" data-testid="cluster-replication-value">Standalone mode</p>
          <p className="mt-1 text-sm text-muted-foreground">
            Single-node operation is healthy and expected. Add peers only if you want multi-node HA replication.
          </p>
        </div>
      </CardContent>
    </Card>
  );
}

function ClusterSummaryCards({
  nodeId,
  peersTotal,
  peersHealthy,
}: {
  nodeId: string;
  peersTotal: number;
  peersHealthy: number;
}) {
  return (
    <div className="grid gap-4 sm:grid-cols-3">
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm font-medium text-muted-foreground">Node ID</CardTitle>
        </CardHeader>
        <CardContent>
          <p className="font-mono text-sm break-all" data-testid="cluster-node-id-value">{nodeId}</p>
        </CardContent>
      </Card>
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm font-medium text-muted-foreground">Peers Total</CardTitle>
        </CardHeader>
        <CardContent>
          <p className="text-2xl font-bold" data-testid="cluster-peers-total-value">{peersTotal}</p>
        </CardContent>
      </Card>
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-sm font-medium text-muted-foreground">Peers Healthy</CardTitle>
        </CardHeader>
        <CardContent>
          <p className="text-2xl font-bold" data-testid="cluster-peers-healthy-value">{peersHealthy}</p>
        </CardContent>
      </Card>
    </div>
  );
}

function PeerHealthTable({
  peers,
}: {
  peers: ClusterPeer[];
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">Peer Health</CardTitle>
      </CardHeader>
      <CardContent>
        <div className="overflow-x-auto">
          <table className="w-full text-sm" data-testid="cluster-peer-table">
            <thead>
              <tr className="border-b text-left text-muted-foreground">
                <th className="pb-2 pr-4 font-medium">Peer ID</th>
                <th className="pb-2 pr-4 font-medium">Address</th>
                <th className="pb-2 pr-4 font-medium">Status</th>
                <th className="pb-2 font-medium">Last Success</th>
              </tr>
            </thead>
            <tbody>
              {peers.map((peer) => {
                const badgeProps = getPeerStatusBadgeProps(peer.status);
                return (
                  <tr
                    key={peer.peer_id}
                    className="border-b last:border-0"
                    data-testid={`cluster-peer-row-${peer.peer_id}`}
                  >
                    <td className="py-2 pr-4 font-medium">{peer.peer_id}</td>
                    <td className="py-2 pr-4 font-mono text-xs">{peer.addr}</td>
                    <td className="py-2 pr-4">
                      <Badge
                        variant={badgeProps.variant}
                        className={badgeProps.className}
                        data-testid={`cluster-peer-status-${peer.peer_id}`}
                      >
                        {badgeProps.label}
                      </Badge>
                    </td>
                    <td
                      className="py-2 text-muted-foreground"
                      data-testid={`cluster-peer-last-success-${peer.peer_id}`}
                    >
                      {formatLastSuccess(peer.last_success_secs_ago)}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </CardContent>
    </Card>
  );
}

function HAClusterState({
  nodeId,
  peersTotal,
  peersHealthy,
  peers,
}: {
  nodeId: string;
  peersTotal: number;
  peersHealthy: number;
  peers: ClusterPeer[];
}) {
  return (
    <div className="space-y-4" data-testid="cluster-ha-state">
      <ClusterSummaryCards
        nodeId={nodeId}
        peersTotal={peersTotal}
        peersHealthy={peersHealthy}
      />
      {peers.length === 0 ? (
        <Card data-testid="cluster-ha-empty-state">
          <CardContent className="pt-6 text-muted-foreground">
            HA is enabled but no peer health rows are available yet.
          </CardContent>
        </Card>
      ) : (
        <PeerHealthTable peers={peers} />
      )}
    </div>
  );
}

export const Cluster = memo(function Cluster() {
  const { data, isLoading, isError, error } = useClusterStatus();
  let content: ReactNode;

  if (isLoading) {
    content = (
      <div className="space-y-4" data-testid="cluster-loading-state">
        <Skeleton className="h-28" />
        <Skeleton className="h-28" />
      </div>
    );
  } else if (isError) {
    content = <ClusterErrorState error={error} />;
  } else if (!data) {
    content = (
      <Card data-testid="cluster-empty-state">
        <CardContent className="pt-6 text-muted-foreground">
          Cluster status response is empty.
        </CardContent>
      </Card>
    );
  } else if (!data.replication_enabled) {
    content = <StandaloneClusterState nodeId={data.node_id} />;
  } else {
    content = (
      <HAClusterState
        nodeId={data.node_id}
        peersTotal={data.peers_total}
        peersHealthy={data.peers_healthy}
        peers={data.peers}
      />
    );
  }

  return (
    <div className="space-y-6" data-testid="cluster-page-shell">
      <div className="flex items-center gap-3">
        <Network className="h-6 w-6" />
        <h1 className="text-2xl font-bold">Cluster</h1>
      </div>
      {content}
    </div>
  );
});
