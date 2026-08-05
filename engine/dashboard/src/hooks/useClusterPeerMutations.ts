import { useMutation } from '@tanstack/react-query';
import api from '@/lib/api';
import { extractApiErrorMessage } from '@/lib/apiErrorMessage';

/**
 * Runtime peer membership mutations for the Cluster screen.
 *
 * These live beside the read-side owner `useClusterStatus` rather than inside
 * `Cluster.tsx` so the page stays presentation-only. Request/response ownership
 * remains on the backend handlers
 * (`internal.rs::{add_cluster_peer, remove_cluster_peer}`); the UI never
 * reproduces the transport/cleartext rule and renders the server `{message}`
 * verbatim on failure.
 */

/** Used only when the server and the transport both fail to supply any text. */
const PEER_REQUEST_FALLBACK_MESSAGE = 'Cluster peer request failed.';

export interface AddClusterPeerVariables {
  node_id: string;
  addr: string;
}

export interface AddClusterPeerResult {
  node_id: string;
  addr: string;
  peers_total: number;
}

export interface RemoveClusterPeerResult {
  node_id: string;
  peers_total: number;
}

export function useAddClusterPeer() {
  return useMutation<AddClusterPeerResult, Error, AddClusterPeerVariables>({
    mutationFn: async (variables) => {
      try {
        const response = await api.post<AddClusterPeerResult>(
          '/internal/cluster/peers',
          variables,
        );
        return response.data;
      } catch (error) {
        throw new Error(extractApiErrorMessage(error, PEER_REQUEST_FALLBACK_MESSAGE));
      }
    },
  });
}

export function useRemoveClusterPeer() {
  return useMutation<RemoveClusterPeerResult, Error, string>({
    mutationFn: async (nodeId) => {
      try {
        const response = await api.delete<RemoveClusterPeerResult>(
          `/internal/cluster/peers/${encodeURIComponent(nodeId)}`,
        );
        return response.data;
      } catch (error) {
        throw new Error(extractApiErrorMessage(error, PEER_REQUEST_FALLBACK_MESSAGE));
      }
    },
  });
}
