import { memo, type FormEvent, type ReactNode } from 'react';
import { Network, XCircle } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Skeleton } from '@/components/ui/skeleton';
import {
  useClusterStatus,
  type ClusterPeer,
  type ClusterPeerStatus,
  type HAClusterStatus,
} from '@/hooks/useClusterStatus';
import { useClusterPeerManagement } from '@/hooks/useClusterPeerManagement';

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

function ClusterErrorState({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  return (
    <Card data-testid="cluster-error-state">
      <CardContent className="pt-6">
        <div className="flex items-center justify-between gap-3">
          <div className="flex items-center gap-3 text-destructive">
            <XCircle className="h-5 w-5" />
            <div>
              <p className="font-medium">Failed to fetch cluster status</p>
              <p className="text-sm text-muted-foreground">{getClusterErrorMessage(error)}</p>
            </div>
          </div>
          <Button variant="outline" onClick={onRetry} data-testid="cluster-error-retry">
            Retry
          </Button>
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

/**
 * Inline add-peer form. Requires both fields nonblank but never rejects
 * `http://` client-side: the backend transport policy
 * (`NodeConfig::validate_credentialed_peer_transport`) is the single owner of
 * the cleartext rule, so the browser must be free to submit an http:// peer and
 * surface the server's refusal verbatim.
 */
function AddPeerPanel({
  nodeId,
  addr,
  onNodeIdChange,
  onAddrChange,
  onSubmit,
  onCancel,
  isAdding,
  isMutationBusy,
  errorMessage,
}: {
  nodeId: string;
  addr: string;
  onNodeIdChange: (value: string) => void;
  onAddrChange: (value: string) => void;
  onSubmit: () => void;
  onCancel: () => void;
  isAdding: boolean;
  isMutationBusy: boolean;
  errorMessage: string | null;
}) {
  const canSubmit = nodeId.trim().length > 0 && addr.trim().length > 0 && !isMutationBusy;

  const handleSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (canSubmit) {
      onSubmit();
    }
  };

  return (
    <Card data-testid="cluster-add-peer-panel">
      <CardHeader>
        <CardTitle className="text-base">Add peer</CardTitle>
      </CardHeader>
      <CardContent>
        <form className="space-y-4" onSubmit={handleSubmit}>
          <div className="space-y-2">
            <Label htmlFor="cluster-add-peer-node-id">Node ID</Label>
            <Input
              id="cluster-add-peer-node-id"
              data-testid="cluster-add-peer-node-id-input"
              value={nodeId}
              onChange={(event) => onNodeIdChange(event.target.value)}
              disabled={isMutationBusy}
              autoComplete="off"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="cluster-add-peer-addr">Peer URL</Label>
            <Input
              id="cluster-add-peer-addr"
              data-testid="cluster-add-peer-addr-input"
              value={addr}
              onChange={(event) => onAddrChange(event.target.value)}
              disabled={isMutationBusy}
              autoComplete="off"
              placeholder="https://peer-host:7700"
            />
            <p className="text-xs text-muted-foreground">
              Use an https:// origin so replication credentials stay encrypted in transit.
            </p>
          </div>
          {errorMessage ? (
            <p
              className="whitespace-pre-wrap break-words text-sm text-destructive"
              role="alert"
              data-testid="cluster-add-peer-error"
            >
              {errorMessage}
            </p>
          ) : null}
          <div className="flex justify-end gap-2">
            <Button
              type="button"
              variant="outline"
              onClick={onCancel}
              disabled={isAdding}
              data-testid="cluster-add-peer-cancel"
            >
              Cancel
            </Button>
            <Button type="submit" disabled={!canSubmit} data-testid="cluster-add-peer-submit">
              {isAdding ? 'Adding...' : 'Add peer'}
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  );
}

function PeerHealthTable({
  peers,
  onRemovePeer,
  pendingRemovalPeerId,
  isMutationBusy,
}: {
  peers: ClusterPeer[];
  onRemovePeer: (peer: ClusterPeer, trigger: HTMLButtonElement) => void;
  pendingRemovalPeerId: string | null;
  isMutationBusy: boolean;
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
                <th className="pb-2 pr-4 font-medium">Last Success</th>
                <th className="pb-2 font-medium">Actions</th>
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
                    <td className="py-2 pr-4 font-medium break-all">{peer.peer_id}</td>
                    <td className="py-2 pr-4 font-mono text-xs break-all">{peer.addr}</td>
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
                      className="py-2 pr-4 text-muted-foreground"
                      data-testid={`cluster-peer-last-success-${peer.peer_id}`}
                    >
                      {formatLastSuccess(peer.last_success_secs_ago)}
                    </td>
                    <td className="py-2">
                      <Button
                        variant="outline"
                        size="sm"
                        className="text-destructive hover:bg-destructive hover:text-destructive-foreground"
                        onClick={(event) => onRemovePeer(peer, event.currentTarget)}
                        disabled={isMutationBusy || pendingRemovalPeerId === peer.peer_id}
                        data-testid={`cluster-peer-remove-${peer.peer_id}`}
                      >
                        Remove
                      </Button>
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

/**
 * Named destructive confirmation for peer removal. The copy deliberately scopes
 * the consequence to this node's runtime membership; it does not claim to stop
 * or delete the remote node. Escape / outside-click / Cancel all close without a
 * request while the single DELETE is unresolved.
 */
function RemovePeerDialog({
  peer,
  onConfirm,
  onOpenChange,
  isPending,
  isMutationBusy,
  errorMessage,
}: {
  peer: ClusterPeer;
  onConfirm: () => void;
  onOpenChange: (open: boolean) => void;
  isPending: boolean;
  isMutationBusy: boolean;
  errorMessage: string | null;
}) {
  return (
    <Dialog open onOpenChange={(open) => { if (!open && !isPending) onOpenChange(false); }}>
      <DialogContent className="max-w-md" data-testid="cluster-remove-peer-dialog">
        <DialogHeader>
          <DialogTitle>{`Remove peer ${peer.peer_id}?`}</DialogTitle>
          <DialogDescription asChild>
            <span className="space-y-3 text-sm text-muted-foreground">
              <span className="block">
                Address: <span className="font-mono break-all text-foreground">{peer.addr}</span>
              </span>
              <span className="block">
                This removes the peer from this node&apos;s runtime membership. It does not stop or delete the remote node.
              </span>
            </span>
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-3 text-sm text-muted-foreground">
          {errorMessage ? (
            <p
              className="whitespace-pre-wrap break-words text-destructive"
              role="alert"
              data-testid="cluster-remove-peer-error"
            >
              {errorMessage}
            </p>
          ) : null}
        </div>
        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={isPending}
            data-testid="cluster-remove-peer-cancel"
          >
            Cancel
          </Button>
          <Button
            variant="destructive"
            onClick={onConfirm}
            disabled={isMutationBusy}
            data-testid="cluster-remove-peer-confirm"
          >
            {isPending ? 'Removing...' : 'Remove peer'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function ClusterPageHeader({ action }: { action?: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <div className="flex items-center gap-3">
        <Network className="h-6 w-6" />
        <h1 className="text-2xl font-bold">Cluster</h1>
      </div>
      {action}
    </div>
  );
}

function HAClusterState({ data, refetch }: {
  data: HAClusterStatus;
  refetch: () => unknown;
}) {
  const peerManagement = useClusterPeerManagement(refetch);

  return (
    <>
      <ClusterPageHeader
        action={(
          <Button
            onClick={peerManagement.openAddForm}
            disabled={peerManagement.isTopologyMutationBusy || peerManagement.addFormOpen}
            data-testid="cluster-add-peer-button"
          >
            Add peer
          </Button>
        )}
      />
      <div className="space-y-4" data-testid="cluster-ha-state">
        <ClusterSummaryCards
          nodeId={data.node_id}
          peersTotal={data.peers_total}
          peersHealthy={data.peers_healthy}
        />
        {peerManagement.addFormOpen ? (
          <AddPeerPanel
            nodeId={peerManagement.nodeIdInput}
            addr={peerManagement.addrInput}
            onNodeIdChange={peerManagement.setNodeIdInput}
            onAddrChange={peerManagement.setAddrInput}
            onSubmit={peerManagement.submitAddPeer}
            onCancel={peerManagement.closeAddForm}
            isAdding={peerManagement.isAddPending}
            isMutationBusy={peerManagement.isTopologyMutationBusy}
            errorMessage={peerManagement.addErrorMessage}
          />
        ) : null}
        {data.peers.length === 0 ? (
          <Card data-testid="cluster-ha-empty-state">
            <CardContent className="pt-6 text-muted-foreground">
              HA is enabled but no peers are configured yet. Use <span className="font-medium">Add peer</span> to join one.
            </CardContent>
          </Card>
        ) : (
          <PeerHealthTable
            peers={data.peers}
            onRemovePeer={peerManagement.openRemoveDialog}
            pendingRemovalPeerId={peerManagement.removingPeerId}
            isMutationBusy={peerManagement.isTopologyMutationBusy}
          />
        )}
        {peerManagement.peerPendingRemoval ? (
          <RemovePeerDialog
            peer={peerManagement.peerPendingRemoval}
            onConfirm={peerManagement.confirmRemovePeer}
            onOpenChange={peerManagement.closeRemoveDialog}
            isPending={peerManagement.isRemovePending}
            isMutationBusy={peerManagement.isTopologyMutationBusy}
            errorMessage={peerManagement.removeErrorMessage}
          />
        ) : null}
      </div>
    </>
  );
}

export const Cluster = memo(function Cluster() {
  const { data, isLoading, isError, error, refetch } = useClusterStatus();
  const hasCurrentStatus = !isLoading && !isError && Boolean(data);
  const isStandalone = Boolean(hasCurrentStatus && data && !data.replication_enabled);

  let content: ReactNode;

  if (isLoading) {
    content = (
      <div className="space-y-4" data-testid="cluster-loading-state">
        <Skeleton className="h-28" />
        <Skeleton className="h-28" />
      </div>
    );
  } else if (isError) {
    content = <ClusterErrorState error={error} onRetry={() => void refetch()} />;
  } else if (!data) {
    content = (
      <Card data-testid="cluster-empty-state">
        <CardContent className="pt-6 flex items-center justify-between gap-3 text-muted-foreground">
          <span>Cluster status response is empty.</span>
          <Button variant="outline" onClick={() => void refetch()} data-testid="cluster-empty-retry">
            Retry
          </Button>
        </CardContent>
      </Card>
    );
  } else if (!data.replication_enabled) {
    content = <StandaloneClusterState nodeId={data.node_id} />;
  } else {
    content = <HAClusterState data={data} refetch={refetch} />;
  }

  return (
    <div className="space-y-6" data-testid="cluster-page-shell">
      {data?.replication_enabled && hasCurrentStatus ? null : (
        <ClusterPageHeader
          action={isStandalone ? (
            <Button variant="outline" onClick={() => void refetch()} data-testid="cluster-refresh-status-button">
              Refresh status
            </Button>
          ) : undefined}
        />
      )}
      {content}
    </div>
  );
});
