import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { Cluster } from './Cluster';

vi.mock('@/hooks/useClusterStatus', async () => {
  const actual = await vi.importActual<typeof import('@/hooks/useClusterStatus')>('@/hooks/useClusterStatus');
  return {
    ...actual,
    useClusterStatus: vi.fn(),
  };
});

import { useClusterStatus } from '@/hooks/useClusterStatus';

describe('Cluster page', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('renders loading state container while cluster status is loading', () => {
    vi.mocked(useClusterStatus).mockReturnValue({
      data: undefined,
      isLoading: true,
      isError: false,
      error: null,
    } as unknown as ReturnType<typeof useClusterStatus>);

    render(<Cluster />);

    expect(screen.getByTestId('cluster-page-shell')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-loading-state')).toBeInTheDocument();
  });

  it('renders request error state container when cluster status query fails', () => {
    vi.mocked(useClusterStatus).mockReturnValue({
      data: undefined,
      isLoading: false,
      isError: true,
      error: new Error('cluster status unavailable'),
    } as unknown as ReturnType<typeof useClusterStatus>);

    render(<Cluster />);

    expect(screen.getByTestId('cluster-page-shell')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-error-state')).toBeInTheDocument();
    expect(screen.getByText('cluster status unavailable')).toBeInTheDocument();
  });

  it('renders a distinct empty response state container when the query returns no payload', () => {
    vi.mocked(useClusterStatus).mockReturnValue({
      data: undefined,
      isLoading: false,
      isError: false,
      error: null,
    } as unknown as ReturnType<typeof useClusterStatus>);

    render(<Cluster />);

    expect(screen.getByTestId('cluster-page-shell')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-empty-state')).toBeInTheDocument();
    expect(screen.queryByTestId('cluster-error-state')).not.toBeInTheDocument();
  });

  it('renders standalone state container when replication is disabled', () => {
    vi.mocked(useClusterStatus).mockReturnValue({
      data: {
        node_id: 'standalone-node',
        replication_enabled: false,
        peers: [],
      },
      isLoading: false,
      isError: false,
      error: null,
    } as unknown as ReturnType<typeof useClusterStatus>);

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
    vi.mocked(useClusterStatus).mockReturnValue({
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
      },
      isLoading: false,
      isError: false,
      error: null,
    } as unknown as ReturnType<typeof useClusterStatus>);

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
    vi.mocked(useClusterStatus).mockReturnValue({
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
      },
      isLoading: false,
      isError: false,
      error: null,
    } as unknown as ReturnType<typeof useClusterStatus>);

    render(<Cluster />);

    expect(screen.getByTestId('cluster-peer-last-success-node-sub-second')).toHaveTextContent('<1s ago');
    expect(screen.getByTestId('cluster-peer-last-success-node-seconds')).toHaveTextContent('59s ago');
    expect(screen.getByTestId('cluster-peer-last-success-node-minutes')).toHaveTextContent('1m ago');
    expect(screen.getByTestId('cluster-peer-last-success-node-hours')).toHaveTextContent('1h ago');
  });

  it('renders explicit HA empty state when replication is enabled with zero peers', () => {
    vi.mocked(useClusterStatus).mockReturnValue({
      data: {
        node_id: 'ha-node-empty',
        replication_enabled: true,
        peers_total: 0,
        peers_healthy: 0,
        peers: [],
      },
      isLoading: false,
      isError: false,
      error: null,
    } as unknown as ReturnType<typeof useClusterStatus>);

    render(<Cluster />);

    expect(screen.getByTestId('cluster-ha-state')).toBeInTheDocument();
    expect(screen.getByTestId('cluster-ha-empty-state')).toBeInTheDocument();
  });
});
