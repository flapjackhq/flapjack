import { useQuery } from '@tanstack/react-query';
import api from '@/lib/api';

export type ClusterPeerStatus =
  | 'healthy'
  | 'stale'
  | 'unhealthy'
  | 'circuit_open'
  | 'never_contacted';

export interface ClusterPeer {
  peer_id: string;
  addr: string;
  status: ClusterPeerStatus;
  last_success_secs_ago: number | null;
}

export interface StandaloneClusterStatus {
  node_id: string;
  replication_enabled: false;
  peers: [];
}

export interface HAClusterStatus {
  node_id: string;
  replication_enabled: true;
  peers_total: number;
  peers_healthy: number;
  peers: ClusterPeer[];
}

export type ClusterStatusResponse = StandaloneClusterStatus | HAClusterStatus;

const CLUSTER_STATUS_QUERY_KEY = ['cluster-status'] as const;
const CLUSTER_PEER_STATUSES: ReadonlySet<ClusterPeerStatus> = new Set([
  'healthy',
  'stale',
  'unhealthy',
  'circuit_open',
  'never_contacted',
]);

class ClusterStatusContractError extends Error {
  constructor(message: string) {
    super(`Invalid cluster status response: ${message}`);
    this.name = 'ClusterStatusContractError';
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * TODO: Document parsePeer.
 */
function parsePeer(peer: unknown, index: number): ClusterPeer {
  if (!isRecord(peer)) {
    throw new ClusterStatusContractError(`peer ${index + 1} must be an object`);
  }

  if (typeof peer.peer_id !== 'string' || peer.peer_id.length === 0) {
    throw new ClusterStatusContractError(`peer ${index + 1} is missing a valid peer_id`);
  }

  if (typeof peer.addr !== 'string' || peer.addr.length === 0) {
    throw new ClusterStatusContractError(`peer ${peer.peer_id} is missing a valid addr`);
  }

  if (typeof peer.status !== 'string' || !CLUSTER_PEER_STATUSES.has(peer.status as ClusterPeerStatus)) {
    throw new ClusterStatusContractError(`peer ${peer.peer_id} has an unknown status`);
  }

  if (peer.last_success_secs_ago !== null && typeof peer.last_success_secs_ago !== 'number') {
    throw new ClusterStatusContractError(
      `peer ${peer.peer_id} has an invalid last_success_secs_ago value`,
    );
  }

  return {
    peer_id: peer.peer_id,
    addr: peer.addr,
    status: peer.status as ClusterPeerStatus,
    last_success_secs_ago: peer.last_success_secs_ago,
  };
}

/**
 * TODO: Document parseClusterStatusResponse.
 */
function parseClusterStatusResponse(data: unknown): ClusterStatusResponse {
  if (!isRecord(data)) {
    throw new ClusterStatusContractError('response body must be an object');
  }

  if (typeof data.node_id !== 'string' || data.node_id.length === 0) {
    throw new ClusterStatusContractError('node_id must be a non-empty string');
  }

  if (typeof data.replication_enabled !== 'boolean') {
    throw new ClusterStatusContractError('replication_enabled must be a boolean');
  }

  if (!Array.isArray(data.peers)) {
    throw new ClusterStatusContractError('peers must be an array');
  }

  if (!data.replication_enabled) {
    return {
      node_id: data.node_id,
      replication_enabled: false,
      peers: [],
    };
  }

  if (typeof data.peers_total !== 'number' || !Number.isInteger(data.peers_total) || data.peers_total < 0) {
    throw new ClusterStatusContractError('peers_total must be a non-negative integer');
  }

  if (
    typeof data.peers_healthy !== 'number'
    || !Number.isInteger(data.peers_healthy)
    || data.peers_healthy < 0
  ) {
    throw new ClusterStatusContractError('peers_healthy must be a non-negative integer');
  }

  return {
    node_id: data.node_id,
    replication_enabled: true,
    peers_total: data.peers_total,
    peers_healthy: data.peers_healthy,
    peers: data.peers.map((peer, index) => parsePeer(peer, index)),
  };
}

export function useClusterStatus() {
  return useQuery<ClusterStatusResponse>({
    queryKey: CLUSTER_STATUS_QUERY_KEY,
    queryFn: async () => {
      const response = await api.get<unknown>('/internal/cluster/status');
      return parseClusterStatusResponse(response.data);
    },
    refetchInterval: 5000,
    retry: 1,
  });
}
