import { render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { MemoryRouter } from 'react-router-dom';
import { OverviewAnalyticsSection } from '@/pages/OverviewSections';
import { CHART_CANVAS_TEST_ID, CHART_MARK_TEST_ID } from '@/lib/constants';

vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children }: { children: any }) => <div>{children}</div>,
  // Forward the component's own props so tests assert the production chart
  // test ids rather than ids invented by this mock.
  AreaChart: ({ children, ...props }: { children: any } & Record<string, unknown>) => (
    <svg viewBox="0 0 100 100" {...props}>
      {children}
    </svg>
  ),
  Area: ({ dataKey, ...props }: { dataKey?: string } & Record<string, unknown>) => (
    <path data-key={dataKey} {...props} />
  ),
  CartesianGrid: () => null,
  Tooltip: () => null,
  XAxis: () => null,
  YAxis: () => null,
}));

const defaultProps = {
  overviewLoading: false,
  indexes: [],
  cleanupPending: false,
  cleanupSuccess: false,
  onOpenCleanup: vi.fn(),
};

function renderSection(overview: Parameters<typeof OverviewAnalyticsSection>[0]['overview']) {
  render(
    <MemoryRouter>
      <OverviewAnalyticsSection {...defaultProps} overview={overview} />
    </MemoryRouter>
  );
}

describe('OverviewAnalyticsSection chart', () => {
  it('tags the trend chart surface and its search-count mark', () => {
    renderSection({
      totalSearches: 120,
      uniqueUsers: 8,
      noResultRate: 0.05,
      dates: [
        { date: '2026-01-01', count: 40 },
        { date: '2026-01-02', count: 80 },
      ],
    });

    const chart = screen.getByTestId('overview-analytics-chart');
    expect(within(chart).getByTestId(CHART_CANVAS_TEST_ID)).toBeInTheDocument();
    expect(within(chart).getByTestId(CHART_MARK_TEST_ID)).toHaveAttribute('data-key', 'count');
  });

  it('renders no chart at all when the range has no dated searches', () => {
    renderSection({ totalSearches: 120, uniqueUsers: 8, noResultRate: 0, dates: [] });

    expect(screen.queryByTestId('overview-analytics-chart')).not.toBeInTheDocument();
    expect(screen.queryByTestId(CHART_CANVAS_TEST_ID)).not.toBeInTheDocument();
  });
});
