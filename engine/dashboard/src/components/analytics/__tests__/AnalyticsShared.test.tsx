import { render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { Search } from 'lucide-react';
import {
  AreaTrendCard,
  EmptyState,
  ErrorState,
  formatAnalyticsErrorMessage,
  TableSkeleton,
} from '@/components/analytics/AnalyticsShared';

const { responsiveContainerProps } = vi.hoisted(() => ({
  responsiveContainerProps: [] as Array<Record<string, unknown>>,
}));

// Recharts renders ResizeObserver-sensitive primitives in jsdom
vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children, ...props }: { children: any } & Record<string, unknown>) => {
    responsiveContainerProps.push(props);
    return <div>{children}</div>;
  },
  AreaChart: ({ children }: { children: any }) => (
    <svg data-testid="area-chart" viewBox="0 0 100 100">
      {children}
    </svg>
  ),
  Area: () => null,
  CartesianGrid: () => null,
  Tooltip: () => null,
  XAxis: () => null,
  YAxis: () => null,
}));

describe('EmptyState', () => {
  it('renders icon, title, and description', () => {
    render(
      <EmptyState
        icon={Search}
        title="No Data"
        description="Nothing to show yet"
      />
    );

    const container = screen.getByTestId('empty-state');
    expect(container).toBeInTheDocument();
    expect(container).toHaveTextContent('No Data');
    expect(container).toHaveTextContent('Nothing to show yet');
    expect(container.querySelector('svg')).toBeInTheDocument();
  });

  it('uses positive styling when positive flag is set', () => {
    render(
      <EmptyState
        icon={Search}
        title="Great"
        description="All good"
        positive
      />
    );

    const icon = screen.getByTestId('empty-state').querySelector('svg');
    expect(icon).toBeInTheDocument();
    expect(icon).toHaveClass('text-green-500/60');
  });
});

describe('ErrorState', () => {
  it('renders the provided error message', () => {
    render(<ErrorState message="Failure" />);

    const container = screen.getByTestId('error-state');
    expect(container).toBeInTheDocument();
    expect(container).toHaveTextContent('Error: Failure');
    const icon = container.querySelector('svg');
    expect(icon).toBeInTheDocument();
    expect(icon).toHaveClass('text-red-500/60');
  });
});

describe('formatAnalyticsErrorMessage', () => {
  it('returns a generic analytics error message when errors are present', () => {
    expect(
      formatAnalyticsErrorMessage(null, new Error('Primary failed'), new Error('Secondary failed'))
    ).toBe('Unable to load analytics data. Try again.');
  });

  it('returns a generic fallback when the error is not an Error instance', () => {
    expect(formatAnalyticsErrorMessage('broken response')).toBe('Unable to load analytics data. Try again.');
  });

  it('returns null when no error is present', () => {
    expect(formatAnalyticsErrorMessage(undefined, null)).toBeNull();
  });
});

describe('TableSkeleton', () => {
  it('renders a loading row for each requested row', () => {
    render(<TableSkeleton rows={3} />);

    const container = screen.getByTestId('table-skeleton');
    const skeletonRows = container.querySelectorAll('.animate-pulse');

    expect(skeletonRows).toHaveLength(9);
  });
});

describe('AreaTrendCard', () => {
  beforeEach(() => {
    responsiveContainerProps.length = 0;
  });

  it('shows loading skeleton while loading', () => {
    render(
      <AreaTrendCard
        testId="trend"
        title="Trend"
        loading
        data={[]}
        chartHeight={160}
        gradientId="trend-gradient"
        gradientColor="#22c55e"
        dataKey="value"
        strokeColor="#22c55e"
        tooltipValueFormatter={(value) => String(value)}
        seriesLabel="Series"
        emptyState={<div>empty</div>}
      />
    );

    const card = screen.getByTestId('trend');
    expect(card).toBeInTheDocument();
    expect(card.querySelector('.animate-pulse')).toBeInTheDocument();
    expect(screen.queryByText('empty')).not.toBeInTheDocument();
  });

  it('shows empty state when not loading and no data exists', () => {
    render(
      <AreaTrendCard
        testId="trend-empty"
        title="Trend"
        loading={false}
        data={[]}
        chartHeight={160}
        gradientId="trend-gradient-empty"
        gradientColor="#22c55e"
        dataKey="value"
        strokeColor="#22c55e"
        tooltipValueFormatter={(value) => String(value)}
        seriesLabel="Series"
        emptyState={<div data-testid="empty-state-content">empty</div>}
      />
    );

    expect(screen.getByTestId('empty-state-content')).toBeInTheDocument();
    expect(screen.queryByTestId('area-chart')).not.toBeInTheDocument();
  });

  it('renders chart when data is present', () => {
    render(
      <AreaTrendCard
        testId="trend-data"
        title="Trend"
        loading={false}
        data={[
          { date: '2026-01-01', value: 1 },
          { date: '2026-01-02', value: 2 },
        ]}
        chartHeight={160}
        gradientId="trend-gradient-data"
        gradientColor="#22c55e"
        dataKey="value"
        strokeColor="#22c55e"
        tooltipValueFormatter={(value) => String(value)}
        seriesLabel="Series"
        emptyState={<div data-testid="empty-state-content">empty</div>}
      />
    );

    expect(screen.getByTestId('area-chart')).toBeInTheDocument();
    expect(screen.queryByTestId('empty-state-content')).not.toBeInTheDocument();
  });

  it('gives Recharts a positive minimum width before ResizeObserver has measured layout', () => {
    render(
      <AreaTrendCard
        testId="trend-data"
        title="Trend"
        loading={false}
        data={[
          { date: '2026-01-01', value: 1 },
          { date: '2026-01-02', value: 2 },
        ]}
        chartHeight={160}
        gradientId="trend-gradient-data"
        gradientColor="#22c55e"
        dataKey="value"
        strokeColor="#22c55e"
        tooltipValueFormatter={(value) => String(value)}
        seriesLabel="Series"
        emptyState={<div data-testid="empty-state-content">empty</div>}
      />
    );

    expect(responsiveContainerProps).toContainEqual(
      expect.objectContaining({ minWidth: 1 }),
    );
  });
});
