import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ComponentType } from 'react';
import { Cluster } from './Cluster';

// Cluster is wrapped in React.memo and takes no props, so plain rerender()
// bails out and never re-reads the mocked status. Forwarding a changing token
// prop (ignored by the component) forces a real re-render on the same instance,
// which is required to exercise the loading -> error -> recovery transition.
const TokenizedCluster = Cluster as unknown as ComponentType<{ token: number }>;

vi.mock('@/hooks/useClusterStatus', async () => {
  const actual = await vi.importActual<typeof import('@/hooks/useClusterStatus')>('@/hooks/useClusterStatus');
  return {
    ...actual,
    useClusterStatus: vi.fn(),
  };
});

vi.mock('@/hooks/useClusterPeerMutations', () => ({
  useAddClusterPeer: vi.fn(),
  useRemoveClusterPeer: vi.fn(),
}));

import { useClusterStatus } from '@/hooks/useClusterStatus';
import { useAddClusterPeer, useRemoveClusterPeer } from '@/hooks/useClusterPeerMutations';

type ClusterStatusReturn = ReturnType<typeof useClusterStatus>;
type MutationReturn = { mutate: ReturnType<typeof vi.fn>; isPending: boolean; error: Error | null; reset: ReturnType<typeof vi.fn> };

function mockStatus(overrides: Partial<ClusterStatusReturn>): ReturnType<typeof vi.fn> {
  const refetch = vi.fn();
  vi.mocked(useClusterStatus).mockReturnValue({
    data: undefined,
    isLoading: false,
    isError: false,
    error: null,
    refetch,
    ...overrides,
  } as unknown as ClusterStatusReturn);
  return refetch;
}

function mockMutation(overrides: Partial<MutationReturn> = {}): MutationReturn {
  return {
    mutate: vi.fn(),
    isPending: false,
    error: null,
    reset: vi.fn(),
    ...overrides,
  };
}

function createDeferredPromise() {
  let resolve!: () => void;
  const promise = new Promise<void>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

async function expectPromisePending(promise: Promise<unknown>) {
  let settled = false;
  void promise.finally(() => { settled = true; });
  await Promise.resolve();
  expect(settled).toBe(false);
}

const HA_WITH_PEER: ClusterStatusReturn['data'] = {
  node_id: 'ha-node-a',
  replication_enabled: true,
  peers_total: 1,
  peers_healthy: 1,
  peers: [
    {
      peer_id: 'peer-b',
      addr: 'https://peer-b:7700',
      status: 'healthy',
      last_success_secs_ago: 4,
    },
  ],
} as ClusterStatusReturn['data'];

// A later five-second poll tick: same HA branch and same peer, but a new object
// identity and movement in exactly the fields a refresh actually changes.
const HA_WITH_PEER_NEXT_POLL: ClusterStatusReturn['data'] = {
  node_id: 'ha-node-a',
  replication_enabled: true,
  peers_total: 1,
  peers_healthy: 0,
  peers: [
    {
      peer_id: 'peer-b',
      addr: 'https://peer-b:7700',
      status: 'stale',
      last_success_secs_ago: 9,
    },
  ],
} as ClusterStatusReturn['data'];

describe('Cluster page', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(useAddClusterPeer).mockReturnValue(mockMutation() as unknown as ReturnType<typeof useAddClusterPeer>);
    vi.mocked(useRemoveClusterPeer).mockReturnValue(mockMutation() as unknown as ReturnType<typeof useRemoveClusterPeer>);
  });

  it('renders loading state container while cluster status is loading', () => {
    mockStatus({ isLoading: true });

    render(<Cluster />);

    expect(screen.getByTestId('cluster-page-shell')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-loading-state')).toBeInTheDocument();
  });

  it('renders request error state container when cluster status query fails', () => {
    mockStatus({ isError: true, error: new Error('cluster status unavailable') });

    render(<Cluster />);

    expect(screen.getByTestId('cluster-page-shell')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-error-state')).toBeInTheDocument();
    expect(screen.getByText('cluster status unavailable')).toBeInTheDocument();
  });

  it('renders a distinct empty response state container when the query returns no payload', () => {
    mockStatus({});

    render(<Cluster />);

    expect(screen.getByTestId('cluster-page-shell')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-empty-state')).toBeInTheDocument();
    expect(screen.queryByTestId('cluster-error-state')).not.toBeInTheDocument();
  });

  it('renders standalone state container when replication is disabled', () => {
    mockStatus({
      data: {
        node_id: 'standalone-node',
        replication_enabled: false,
        peers: [],
      } as ClusterStatusReturn['data'],
    });

    render(<Cluster />);

    expect(screen.getByTestId('cluster-page-shell')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-standalone-state')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-node-id-value')).toHaveTextContent('standalone-node');
    expect(screen.getByRole('heading', { name: 'Standalone mode' })).toBeInTheDocument();
    expect(screen.getByTestId('cluster-replication-value')).toHaveTextContent('Standalone mode');
    expect(
      screen.getByText('Single-node operation is healthy and expected. Add peers only if you want multi-node HA replication.')
    ).toBeInTheDocument();
  });

  it('renders HA summary cards from payload totals and peer rows from peers list', () => {
    mockStatus({
      data: {
        node_id: 'ha-node-a',
        replication_enabled: true,
        peers_total: 9,
        peers_healthy: 6,
        peers: [
          {
            peer_id: 'ha-node-b',
            addr: 'http://ha-node-b:7700',
            status: 'healthy',
            last_success_secs_ago: 4,
          },
          {
            peer_id: 'ha-node-c',
            addr: 'http://ha-node-c:7700',
            status: 'stale',
            last_success_secs_ago: 45,
          },
          {
            peer_id: 'ha-node-d',
            addr: 'http://ha-node-d:7700',
            status: 'never_contacted',
            last_success_secs_ago: null,
          },
          {
            peer_id: 'ha-node-e',
            addr: 'http://ha-node-e:7700',
            status: 'circuit_open',
            last_success_secs_ago: 120,
          },
          {
            peer_id: 'ha-node-f',
            addr: 'http://ha-node-f:7700',
            status: 'unhealthy',
            last_success_secs_ago: 7200,
          },
        ],
      } as ClusterStatusReturn['data'],
    });

    render(<Cluster />);

    expect(screen.getByTestId('cluster-ha-state')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Cluster' })).toBeInTheDocument();
    expect(screen.getByTestId('cluster-peer-table')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-peers-total-value')).toHaveTextContent('9');
    expect(screen.getByTestId('cluster-peers-healthy-value')).toHaveTextContent('6');
    expect(screen.getByTestId('cluster-peer-row-ha-node-b')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-peer-row-ha-node-c')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-peer-row-ha-node-d')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-peer-row-ha-node-e')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-peer-row-ha-node-f')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-peer-status-ha-node-b')).toHaveTextContent('Healthy');
    expect(screen.getByTestId('cluster-peer-status-ha-node-c')).toHaveTextContent('Stale');
    expect(screen.getByTestId('cluster-peer-status-ha-node-d')).toHaveTextContent('Never Contacted');
    expect(screen.getByTestId('cluster-peer-status-ha-node-e')).toHaveTextContent('Circuit Open');
    expect(screen.getByTestId('cluster-peer-status-ha-node-f')).toHaveTextContent('Unhealthy');
    expect(screen.getByTestId('cluster-peer-status-ha-node-b')).toHaveClass('bg-green-100', 'text-green-800');
    expect(screen.getByTestId('cluster-peer-status-ha-node-c')).toHaveClass('border-amber-300', 'text-amber-700');
    expect(screen.getByTestId('cluster-peer-status-ha-node-d')).toHaveClass('border-slate-300', 'text-slate-600');
    expect(screen.getByTestId('cluster-peer-status-ha-node-e')).toHaveClass('bg-orange-100', 'text-orange-800');
    expect(screen.getByTestId('cluster-peer-last-success-ha-node-b')).toHaveTextContent('4s ago');
    expect(screen.getByTestId('cluster-peer-last-success-ha-node-c')).toHaveTextContent('45s ago');
    expect(screen.getByTestId('cluster-peer-last-success-ha-node-d')).toHaveTextContent('Never');
    expect(screen.getByTestId('cluster-peer-last-success-ha-node-e')).toHaveTextContent('2m ago');
    expect(screen.getByTestId('cluster-peer-last-success-ha-node-f')).toHaveTextContent('2h ago');
  });

  it('formats last success values at sub-second, second, minute, and hour thresholds', () => {
    mockStatus({
      data: {
        node_id: 'ha-node-thresholds',
        replication_enabled: true,
        peers_total: 4,
        peers_healthy: 1,
        peers: [
          {
            peer_id: 'node-sub-second',
            addr: 'http://node-sub-second:7700',
            status: 'healthy',
            last_success_secs_ago: 0.2,
          },
          {
            peer_id: 'node-seconds',
            addr: 'http://node-seconds:7700',
            status: 'stale',
            last_success_secs_ago: 59,
          },
          {
            peer_id: 'node-minutes',
            addr: 'http://node-minutes:7700',
            status: 'circuit_open',
            last_success_secs_ago: 60,
          },
          {
            peer_id: 'node-hours',
            addr: 'http://node-hours:7700',
            status: 'unhealthy',
            last_success_secs_ago: 3600,
          },
        ],
      } as ClusterStatusReturn['data'],
    });

    render(<Cluster />);

    expect(screen.getByTestId('cluster-peer-last-success-node-sub-second')).toHaveTextContent('<1s ago');
    expect(screen.getByTestId('cluster-peer-last-success-node-seconds')).toHaveTextContent('59s ago');
    expect(screen.getByTestId('cluster-peer-last-success-node-minutes')).toHaveTextContent('1m ago');
    expect(screen.getByTestId('cluster-peer-last-success-node-hours')).toHaveTextContent('1h ago');
  });

  it('renders explicit HA empty state when replication is enabled with zero peers', () => {
    mockStatus({
      data: {
        node_id: 'ha-node-empty',
        replication_enabled: true,
        peers_total: 0,
        peers_healthy: 0,
        peers: [],
      } as ClusterStatusReturn['data'],
    });

    render(<Cluster />);

    expect(screen.getByTestId('cluster-ha-state')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-ha-empty-state')).toBeInTheDocument();
  });

  it('offers Retry that refetches cluster status from the error state', async () => {
    const refetch = mockStatus({ isError: true, error: new Error('cluster status unavailable') });
    const user = userEvent.setup();

    render(<Cluster />);

    await user.click(screen.getByTestId('cluster-error-retry'));
    expect(refetch).toHaveBeenCalledTimes(1);
  });

  it('offers only Retry when a refetch error retains prior HA data', () => {
    mockStatus({
      data: HA_WITH_PEER,
      isError: true,
      error: new Error('retained status is no longer current'),
    });
    const removeMutate = vi.fn();
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ mutate: removeMutate }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );

    render(<Cluster />);

    expect(screen.getByTestId('cluster-error-state')).toBeInTheDocument();
    expect(screen.getAllByRole('button')).toHaveLength(1);
    expect(screen.getByTestId('cluster-error-retry')).toBeEnabled();
    expect(screen.queryByTestId('cluster-add-peer-button')).not.toBeInTheDocument();
    expect(screen.queryByTestId('cluster-peer-table')).not.toBeInTheDocument();
    expect(screen.queryByTestId('cluster-remove-peer-dialog')).not.toBeInTheDocument();
    expect(removeMutate).not.toHaveBeenCalled();
  });

  it('shows Refresh status and no mutation controls in standalone mode', async () => {
    const refetch = mockStatus({
      data: {
        node_id: 'standalone-node',
        replication_enabled: false,
        peers: [],
      } as ClusterStatusReturn['data'],
    });
    const user = userEvent.setup();

    render(<Cluster />);

    expect(screen.queryByTestId('cluster-add-peer-button')).not.toBeInTheDocument();
    expect(screen.queryByTestId('cluster-add-peer-panel')).not.toBeInTheDocument();
    await user.click(screen.getByTestId('cluster-refresh-status-button'));
    expect(refetch).toHaveBeenCalledTimes(1);
  });

  it('makes Add peer the primary action on empty HA membership and opens the add form', async () => {
    mockStatus({
      data: {
        node_id: 'ha-node-empty',
        replication_enabled: true,
        peers_total: 0,
        peers_healthy: 0,
        peers: [],
      } as ClusterStatusReturn['data'],
    });
    const user = userEvent.setup();

    render(<Cluster />);

    expect(screen.queryByTestId('cluster-add-peer-panel')).not.toBeInTheDocument();
    await user.click(screen.getByTestId('cluster-add-peer-button'));

    const panel = screen.getByTestId('cluster-add-peer-panel');
    expect(within(panel).getByTestId('cluster-add-peer-node-id-input')).toBeInTheDocument();
    expect(within(panel).getByTestId('cluster-add-peer-addr-input')).toBeInTheDocument();
  });

  it('requires nonblank fields and submits trimmed node_id and addr through the shared mutation', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const mutate = vi.fn();
    vi.mocked(useAddClusterPeer).mockReturnValue(
      mockMutation({ mutate }) as unknown as ReturnType<typeof useAddClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);
    await user.click(screen.getByTestId('cluster-add-peer-button'));

    const submit = screen.getByTestId('cluster-add-peer-submit');
    expect(submit).toBeDisabled();

    await user.type(screen.getByTestId('cluster-add-peer-node-id-input'), '  peer-x  ');
    await user.type(screen.getByTestId('cluster-add-peer-addr-input'), '  https://peer-x:7700  ');
    expect(submit).toBeEnabled();

    await user.click(submit);
    expect(mutate).toHaveBeenCalledTimes(1);
    expect(mutate.mock.calls[0][0]).toEqual({ node_id: 'peer-x', addr: 'https://peer-x:7700' });
  });

  it('keeps add controls locked while an add request is pending', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const reset = vi.fn();
    vi.mocked(useAddClusterPeer).mockReturnValue(
      mockMutation({ isPending: true, reset }) as unknown as ReturnType<typeof useAddClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);

    expect(screen.getByTestId('cluster-add-peer-button')).toBeDisabled();
    await user.click(screen.getByTestId('cluster-add-peer-button'));
    expect(reset).not.toHaveBeenCalled();
    expect(screen.queryByTestId('cluster-add-peer-panel')).not.toBeInTheDocument();
  });

  it('blocks every remove entry point for the full add request lifetime', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const addMutate = vi.fn();
    const removeReset = vi.fn();
    vi.mocked(useAddClusterPeer).mockReturnValue(
      mockMutation({ mutate: addMutate }) as unknown as ReturnType<typeof useAddClusterPeer>,
    );
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ reset: removeReset }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);

    await user.click(screen.getByTestId('cluster-add-peer-button'));
    await user.type(screen.getByTestId('cluster-add-peer-node-id-input'), 'peer-x');
    await user.type(screen.getByTestId('cluster-add-peer-addr-input'), 'https://peer-x:7700');
    await user.click(screen.getByTestId('cluster-add-peer-submit'));

    const removeButton = screen.getByTestId('cluster-peer-remove-peer-b');
    expect(addMutate).toHaveBeenCalledTimes(1);
    expect(removeButton).toBeDisabled();
    expect(removeReset).not.toHaveBeenCalled();
    expect(screen.queryByTestId('cluster-remove-peer-dialog')).not.toBeInTheDocument();
  });

  it('keeps the add mutation unsettled until its status refresh completes', async () => {
    const refetch = mockStatus({ data: HA_WITH_PEER });
    const refresh = createDeferredPromise();
    refetch.mockReturnValue(refresh.promise);
    const addMutate = vi.fn();
    vi.mocked(useAddClusterPeer).mockReturnValue(
      mockMutation({ mutate: addMutate }) as unknown as ReturnType<typeof useAddClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);
    await user.click(screen.getByTestId('cluster-add-peer-button'));
    await user.type(screen.getByTestId('cluster-add-peer-node-id-input'), 'peer-x');
    await user.type(screen.getByTestId('cluster-add-peer-addr-input'), 'https://peer-x:7700');
    await user.click(screen.getByTestId('cluster-add-peer-submit'));

    const successResult = addMutate.mock.calls[0][1].onSuccess();
    await expectPromisePending(successResult);
    expect(refetch).toHaveBeenCalledTimes(1);

    refresh.resolve();
    await successResult;
  });

  it('allows http:// to reach the backend and renders the exact transport refusal message', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const cleartextMessage =
      'Refusing replication peer peer-x at http://peer-x:7700: authenticated analytics query '
      + 'fan-out forwards caller API keys and the peer origin is cleartext http://, which would '
      + 'send the peer credential in plaintext. Move the peer to https://, or set '
      + 'FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1 to keep the cleartext peer.';
    const mutate = vi.fn();
    vi.mocked(useAddClusterPeer).mockReturnValue(
      mockMutation({ mutate, error: new Error(cleartextMessage) }) as unknown as ReturnType<typeof useAddClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);
    await user.click(screen.getByTestId('cluster-add-peer-button'));

    await user.type(screen.getByTestId('cluster-add-peer-node-id-input'), 'peer-x');
    await user.type(screen.getByTestId('cluster-add-peer-addr-input'), 'http://peer-x:7700');
    await user.click(screen.getByTestId('cluster-add-peer-submit'));

    // http:// must not be blocked client-side: the mutation is still invoked.
    expect(mutate).toHaveBeenCalledTimes(1);
    expect(mutate.mock.calls[0][0]).toEqual({ node_id: 'peer-x', addr: 'http://peer-x:7700' });

    const errorRegion = screen.getByTestId('cluster-add-peer-error');
    expect(errorRegion).toHaveTextContent('FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1');
    expect(errorRegion).toHaveTextContent(cleartextMessage);
    // The submitted values are preserved for editing.
    expect(screen.getByTestId('cluster-add-peer-addr-input')).toHaveValue('http://peer-x:7700');
  });

  it('opens a named remove confirmation and cancels without any request', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const mutate = vi.fn();
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ mutate }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);
    const removeButton = screen.getByTestId('cluster-peer-remove-peer-b');
    await user.click(removeButton);

    const dialog = screen.getByTestId('cluster-remove-peer-dialog');
    expect(within(dialog).getByText('Remove peer peer-b?')).toBeInTheDocument();
    expect(dialog).toHaveTextContent('https://peer-b:7700');
    expect(dialog).toHaveTextContent(
      "This removes the peer from this node's runtime membership. It does not stop or delete the remote node.",
    );

    await user.click(screen.getByTestId('cluster-remove-peer-cancel'));
    expect(screen.queryByTestId('cluster-remove-peer-dialog')).not.toBeInTheDocument();
    expect(mutate).not.toHaveBeenCalled();
    await waitFor(() => expect(removeButton).toHaveFocus());
  });

  it('sends the DELETE by node_id when removal is confirmed', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const mutate = vi.fn();
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ mutate }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);
    await user.click(screen.getByTestId('cluster-peer-remove-peer-b'));
    await user.click(screen.getByTestId('cluster-remove-peer-confirm'));

    expect(mutate).toHaveBeenCalledTimes(1);
    expect(mutate.mock.calls[0][0]).toBe('peer-b');
  });

  it('disables remove actions and labels the destructive action while removal is in flight', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const mutate = vi.fn();
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ mutate }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);
    await user.click(screen.getByTestId('cluster-peer-remove-peer-b'));
    await user.click(screen.getByTestId('cluster-remove-peer-confirm'));

    const confirm = screen.getByTestId('cluster-remove-peer-confirm');
    expect(confirm).toBeDisabled();
    expect(confirm).toHaveTextContent('Removing...');
    expect(screen.getByTestId('cluster-remove-peer-cancel')).toBeDisabled();
    expect(screen.getByTestId('cluster-peer-remove-peer-b')).toBeDisabled();
  });

  it('blocks every add entry point for the full removal request lifetime', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const addReset = vi.fn();
    const removeMutate = vi.fn();
    vi.mocked(useAddClusterPeer).mockReturnValue(
      mockMutation({ reset: addReset }) as unknown as ReturnType<typeof useAddClusterPeer>,
    );
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ mutate: removeMutate }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);

    await user.click(screen.getByTestId('cluster-peer-remove-peer-b'));
    await user.click(screen.getByTestId('cluster-remove-peer-confirm'));

    const addButton = screen.getByTestId('cluster-add-peer-button');
    expect(removeMutate).toHaveBeenCalledTimes(1);
    expect(addButton).toBeDisabled();
    expect(addReset).not.toHaveBeenCalled();
    expect(screen.queryByTestId('cluster-add-peer-panel')).not.toBeInTheDocument();
  });

  it('keeps the remove mutation unsettled until its status refresh completes', async () => {
    const refetch = mockStatus({ data: HA_WITH_PEER });
    const refresh = createDeferredPromise();
    refetch.mockReturnValue(refresh.promise);
    const removeMutate = vi.fn();
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ mutate: removeMutate }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);
    await user.click(screen.getByTestId('cluster-peer-remove-peer-b'));
    await user.click(screen.getByTestId('cluster-remove-peer-confirm'));

    const successResult = removeMutate.mock.calls[0][1].onSuccess();
    await expectPromisePending(successResult);
    expect(refetch).toHaveBeenCalledTimes(1);

    refresh.resolve();
    await successResult;
  });

  it('reconciles cluster status before settling a failed removal', async () => {
    const refetch = mockStatus({ data: HA_WITH_PEER });
    const refresh = createDeferredPromise();
    refetch.mockReturnValue(refresh.promise);
    const removeMutate = vi.fn();
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ mutate: removeMutate }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);
    await user.click(screen.getByTestId('cluster-peer-remove-peer-b'));
    await user.click(screen.getByTestId('cluster-remove-peer-confirm'));

    const errorResult = removeMutate.mock.calls[0][1].onError(new Error("Peer 'peer-b' not found"));
    await expectPromisePending(errorResult);
    expect(refetch).toHaveBeenCalledTimes(1);

    refresh.resolve();
    await errorResult;
  });

  it('does not reopen a pre-error add form after cluster status recovers', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const user = userEvent.setup();
    const { rerender } = render(<TokenizedCluster token={0} />);

    await user.click(screen.getByTestId('cluster-add-peer-button'));
    expect(screen.getByTestId('cluster-add-peer-panel')).toBeInTheDocument();

    // Cluster status fails while the add form is open.
    mockStatus({ data: HA_WITH_PEER, isError: true, error: new Error('cluster status unavailable') });
    rerender(<TokenizedCluster token={1} />);
    expect(screen.getByTestId('cluster-error-state')).toBeInTheDocument();
    expect(screen.queryByTestId('cluster-add-peer-panel')).not.toBeInTheDocument();

    // Retry succeeds: status is current again. The stale add form must stay closed.
    mockStatus({ data: HA_WITH_PEER });
    rerender(<TokenizedCluster token={2} />);
    expect(screen.getByTestId('cluster-ha-state')).toBeInTheDocument();
    expect(screen.queryByTestId('cluster-add-peer-panel')).not.toBeInTheDocument();
    expect(screen.getByTestId('cluster-add-peer-button')).toBeEnabled();
  });

  it('does not reopen a pre-error remove dialog after cluster status recovers', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const removeMutate = vi.fn();
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ mutate: removeMutate }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );
    const user = userEvent.setup();
    const { rerender } = render(<TokenizedCluster token={0} />);

    await user.click(screen.getByTestId('cluster-peer-remove-peer-b'));
    expect(screen.getByTestId('cluster-remove-peer-dialog')).toBeInTheDocument();

    // Cluster status fails while the remove dialog is open.
    mockStatus({ data: HA_WITH_PEER, isError: true, error: new Error('cluster status unavailable') });
    rerender(<TokenizedCluster token={1} />);
    expect(screen.getByTestId('cluster-error-state')).toBeInTheDocument();
    expect(screen.queryByTestId('cluster-remove-peer-dialog')).not.toBeInTheDocument();

    // Retry succeeds: status is current again. The stale remove dialog must stay closed.
    mockStatus({ data: HA_WITH_PEER });
    rerender(<TokenizedCluster token={2} />);
    expect(screen.getByTestId('cluster-ha-state')).toBeInTheDocument();
    expect(screen.queryByTestId('cluster-remove-peer-dialog')).not.toBeInTheDocument();
    expect(removeMutate).not.toHaveBeenCalled();
  });

  // The sibling tests below prove mutation intent is DISCARDED at the HA/non-HA
  // unmount boundary. These two prove the complement the spec also requires:
  // an ordinary poll is not that boundary, so it must not close the form or
  // dialog, discard half-entered values, or clear a pending error.
  it('keeps an open add form, its typed values, and its error across a status poll', async () => {
    mockStatus({ data: HA_WITH_PEER });
    vi.mocked(useAddClusterPeer).mockReturnValue(
      mockMutation({ error: new Error("Peer 'peer-x' already exists") }) as unknown as ReturnType<typeof useAddClusterPeer>,
    );
    const user = userEvent.setup();
    const { rerender } = render(<TokenizedCluster token={0} />);

    await user.click(screen.getByTestId('cluster-add-peer-button'));
    await user.type(screen.getByTestId('cluster-add-peer-node-id-input'), 'peer-x');
    await user.type(screen.getByTestId('cluster-add-peer-addr-input'), 'https://peer-x:7700');
    expect(screen.getByTestId('cluster-add-peer-error')).toHaveTextContent("Peer 'peer-x' already exists");

    mockStatus({ data: HA_WITH_PEER_NEXT_POLL });
    rerender(<TokenizedCluster token={1} />);

    expect(screen.getByTestId('cluster-add-peer-panel')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-add-peer-node-id-input')).toHaveValue('peer-x');
    expect(screen.getByTestId('cluster-add-peer-addr-input')).toHaveValue('https://peer-x:7700');
    expect(screen.getByTestId('cluster-add-peer-error')).toHaveTextContent("Peer 'peer-x' already exists");
    // The refreshed status still reached the screen; the form was not frozen in place.
    expect(screen.getByTestId('cluster-peers-healthy-value')).toHaveTextContent('0');
    expect(screen.getByTestId('cluster-peer-status-peer-b')).toHaveTextContent('Stale');
  });

  it('keeps an open remove confirmation across a status poll', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const removeMutate = vi.fn();
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ mutate: removeMutate }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );
    const user = userEvent.setup();
    const { rerender } = render(<TokenizedCluster token={0} />);

    await user.click(screen.getByTestId('cluster-peer-remove-peer-b'));
    expect(screen.getByTestId('cluster-remove-peer-dialog')).toBeInTheDocument();

    mockStatus({ data: HA_WITH_PEER_NEXT_POLL });
    rerender(<TokenizedCluster token={1} />);

    const dialog = screen.getByTestId('cluster-remove-peer-dialog');
    expect(dialog).toHaveTextContent('Remove peer peer-b?');
    expect(dialog).toHaveTextContent('https://peer-b:7700');
    expect(screen.getByTestId('cluster-remove-peer-confirm')).toBeEnabled();
    expect(removeMutate).not.toHaveBeenCalled();
  });

  it('does not reopen an add form after HA changes to standalone and back', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const user = userEvent.setup();
    const { rerender } = render(<TokenizedCluster token={0} />);

    await user.click(screen.getByTestId('cluster-add-peer-button'));
    await user.type(screen.getByTestId('cluster-add-peer-node-id-input'), 'stale-peer');
    expect(screen.getByTestId('cluster-add-peer-panel')).toBeInTheDocument();

    mockStatus({
      data: {
        node_id: 'standalone-node',
        replication_enabled: false,
        peers: [],
      } as ClusterStatusReturn['data'],
    });
    rerender(<TokenizedCluster token={1} />);
    expect(screen.getByTestId('cluster-standalone-state')).toBeInTheDocument();

    mockStatus({ data: HA_WITH_PEER });
    rerender(<TokenizedCluster token={2} />);
    expect(screen.queryByTestId('cluster-add-peer-panel')).not.toBeInTheDocument();
    expect(screen.getByTestId('cluster-add-peer-button')).toBeEnabled();
  });

  it('does not reopen a remove dialog after HA changes to standalone and back', async () => {
    mockStatus({ data: HA_WITH_PEER });
    const removeMutate = vi.fn();
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ mutate: removeMutate }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );
    const user = userEvent.setup();
    const { rerender } = render(<TokenizedCluster token={0} />);

    await user.click(screen.getByTestId('cluster-peer-remove-peer-b'));
    expect(screen.getByTestId('cluster-remove-peer-dialog')).toBeInTheDocument();

    mockStatus({
      data: {
        node_id: 'standalone-node',
        replication_enabled: false,
        peers: [],
      } as ClusterStatusReturn['data'],
    });
    rerender(<TokenizedCluster token={1} />);
    expect(screen.getByTestId('cluster-standalone-state')).toBeInTheDocument();

    mockStatus({ data: HA_WITH_PEER });
    rerender(<TokenizedCluster token={2} />);
    expect(screen.queryByTestId('cluster-remove-peer-dialog')).not.toBeInTheDocument();
    expect(removeMutate).not.toHaveBeenCalled();
  });

  it('restores the remove confirmation with the exact server message on failure', async () => {
    mockStatus({ data: HA_WITH_PEER });
    vi.mocked(useRemoveClusterPeer).mockReturnValue(
      mockMutation({ error: new Error("Peer 'peer-b' not found") }) as unknown as ReturnType<typeof useRemoveClusterPeer>,
    );
    const user = userEvent.setup();

    render(<Cluster />);
    await user.click(screen.getByTestId('cluster-peer-remove-peer-b'));

    expect(screen.getByTestId('cluster-remove-peer-error')).toHaveTextContent("Peer 'peer-b' not found");
    expect(screen.getByTestId('cluster-remove-peer-confirm')).toBeEnabled();
    expect(screen.getByTestId('cluster-remove-peer-cancel')).toBeEnabled();
  });
});
