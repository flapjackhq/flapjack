import { useRef, useState } from 'react';
import type { ClusterPeer } from '@/hooks/useClusterStatus';
import { useAddClusterPeer, useRemoveClusterPeer } from '@/hooks/useClusterPeerMutations';

export function useClusterPeerManagement(refetchStatus: () => unknown) {
  const addPeer = useAddClusterPeer();
  const removePeer = useRemoveClusterPeer();
  const [addFormOpen, setAddFormOpen] = useState(false);
  const [nodeIdInput, setNodeIdInput] = useState('');
  const [addrInput, setAddrInput] = useState('');
  const [peerPendingRemoval, setPeerPendingRemoval] = useState<ClusterPeer | null>(null);
  const [isAddingPeer, setIsAddingPeer] = useState(false);
  const [removingPeerId, setRemovingPeerId] = useState<string | null>(null);
  const removeTriggerRef = useRef<HTMLButtonElement | null>(null);

  const isAddPending = addPeer.isPending || isAddingPeer;
  const isRemovePending = removePeer.isPending || removingPeerId !== null;
  const isTopologyMutationBusy = isAddPending || isRemovePending;

  const openAddForm = () => {
    if (isTopologyMutationBusy || addFormOpen) return;
    addPeer.reset();
    setAddFormOpen(true);
  };

  const closeAddForm = () => {
    if (isAddPending) return;
    setAddFormOpen(false);
    setNodeIdInput('');
    setAddrInput('');
    addPeer.reset();
  };

  const submitAddPeer = () => {
    if (isTopologyMutationBusy) return;
    setIsAddingPeer(true);
    addPeer.mutate(
      { node_id: nodeIdInput.trim(), addr: addrInput.trim() },
      {
        onSuccess: async () => {
          setAddFormOpen(false);
          setNodeIdInput('');
          setAddrInput('');
          await refetchStatus();
        },
        onSettled: () => setIsAddingPeer(false),
      },
    );
  };

  const openRemoveDialog = (peer: ClusterPeer, trigger: HTMLButtonElement) => {
    if (isTopologyMutationBusy) return;
    removePeer.reset();
    removeTriggerRef.current = trigger;
    setPeerPendingRemoval(peer);
  };

  const closeRemoveDialog = () => {
    if (isRemovePending) return;
    const trigger = removeTriggerRef.current;
    setPeerPendingRemoval(null);
    removePeer.reset();
    requestAnimationFrame(() => trigger?.focus());
  };

  const confirmRemovePeer = () => {
    if (!peerPendingRemoval || isTopologyMutationBusy) return;
    setRemovingPeerId(peerPendingRemoval.peer_id);
    removePeer.mutate(peerPendingRemoval.peer_id, {
      onSuccess: async () => {
        removeTriggerRef.current = null;
        setPeerPendingRemoval(null);
        await refetchStatus();
      },
      onError: async () => {
        await refetchStatus();
      },
      onSettled: () => setRemovingPeerId(null),
    });
  };

  return {
    addFormOpen,
    nodeIdInput,
    setNodeIdInput,
    addrInput,
    setAddrInput,
    openAddForm,
    closeAddForm,
    submitAddPeer,
    addErrorMessage: addPeer.error?.message ?? null,
    isAddPending,
    peerPendingRemoval,
    removingPeerId,
    openRemoveDialog,
    closeRemoveDialog,
    confirmRemovePeer,
    removeErrorMessage: removePeer.error?.message ?? null,
    isRemovePending,
    isTopologyMutationBusy,
  };
}
