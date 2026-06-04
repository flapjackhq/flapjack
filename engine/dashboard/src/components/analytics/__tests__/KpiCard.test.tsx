import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { Search } from 'lucide-react';
import { KpiCard, DeltaBadge } from '@/components/analytics/KpiCard';

const { responsiveContainerProps } = vi.hoisted(() => ({
  responsiveContainerProps: [] as Array<Record<string, unknown>>,
}));

// Recharts renders ResizeObserver-sensitive primitives in jsdom
vi.mock('lucide-react', async () => {
  const actual = await vi.importActual<typeof import('lucide-react')>('lucide-react');
  return {
    ...actual,
    ArrowDownRight: () => <span data-testid="arrow-down-right-icon" />,
    ArrowUpRight: () => <span data-testid="arrow-up-right-icon" />,
    Minus: () => <span data-testid="minus-icon" />,
    Search: () => <span data-testid="search-icon" />,
  };
});

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
  Area: ({ fill, dataKey }: { fill?: string; dataKey?: string }) => (
    <path data-testid="spark-area" data-fill={fill} data-key={dataKey} />
  ),
}));

describe('KpiCard', () => {
  it('renders title, formatted value, and icon', () => {
    render(
      <KpiCard
        title="Total Searches"
        value={1234}
        prevValue={1000}
        loading={false}
        icon={Search}
        format="number"
      />
    );

    expect(screen.getByText('Total Searches')).toBeInTheDocument();
    expect(screen.getByTestId('kpi-value')).toHaveTextContent('1,234');

    expect(screen.getByTestId('search-icon')).toBeInTheDocument();
  });

  it('renders loading skeleton when loading', () => {
    const { container } = render(
      <KpiCard
        title="Total Searches"
        value={1234}
        loading
        icon={Search}
        format="number"
      />
    );

    expect(container.querySelector('.animate-pulse')).toBeInTheDocument();
    expect(screen.queryByTestId('kpi-value')).not.toBeInTheDocument();
    expect(screen.queryByTestId('delta-badge')).not.toBeInTheDocument();
  });

  it('passes through the testId prop to the card root', () => {
    render(
      <KpiCard
        title="Total Searches"
        value={1234}
        loading={false}
        icon={Search}
        format="number"
        testId="kpi-searches"
      />
    );

    expect(screen.getByTestId('kpi-searches')).toBeInTheDocument();
  });

  it('builds a stable fallback test id from the title when testId is omitted', () => {
    render(
      <KpiCard
        title="Total   Searches"
        value={1234}
        loading={false}
        icon={Search}
        format="number"
      />
    );

    expect(screen.getByTestId('kpi-total-searches')).toBeInTheDocument();
  });

  it('normalizes sparkline gradient ids from the title slug', () => {
    responsiveContainerProps.length = 0;
    const { container } = render(
      <KpiCard
        title="Total   Searches"
        value={1234}
        loading={false}
        icon={Search}
        sparkData={[{ count: 1234 }]}
        format="number"
      />
    );

    expect(screen.getByTestId('sparkline')).toBeInTheDocument();
    expect(container.querySelector('#spark-total-searches')).toBeInTheDocument();
    expect(screen.getByTestId('spark-area')).toHaveAttribute('data-fill', 'url(#spark-total-searches)');
    expect(screen.getByTestId('spark-area')).toHaveAttribute('data-key', 'count');
  });

  it('gives sparkline charts a positive minimum width before ResizeObserver has measured layout', () => {
    responsiveContainerProps.length = 0;

    render(
      <KpiCard
        title="Total Searches"
        value={1234}
        loading={false}
        icon={Search}
        sparkData={[{ count: 1234 }]}
        format="number"
      />
    );

    expect(responsiveContainerProps).toContainEqual(
      expect.objectContaining({ minWidth: 1 }),
    );
  });
});

describe('DeltaBadge', () => {
  it('renders green up arrow when delta is positive', () => {
    render(<DeltaBadge current={150} previous={100} />);

    const badge = screen.getByTestId('delta-badge');
    expect(badge).toHaveClass('text-green-600');
    expect(badge).toHaveTextContent('50.0%');
    expect(screen.getByTestId('arrow-up-right-icon')).toBeInTheDocument();
  });

  it('renders red down arrow when delta is negative', () => {
    render(<DeltaBadge current={25} previous={100} />);

    const badge = screen.getByTestId('delta-badge');
    expect(badge).toHaveClass('text-red-500');
    expect(badge).toHaveTextContent('75.0%');
    expect(screen.getByTestId('arrow-down-right-icon')).toBeInTheDocument();
  });

  it('renders neutral dash when values are equal', () => {
    render(<DeltaBadge current={50} previous={50} />);

    const badge = screen.getByTestId('delta-badge');
    expect(badge).toHaveClass('text-muted-foreground');
    expect(badge).toHaveTextContent('0%');
    expect(screen.getByTestId('minus-icon')).toBeInTheDocument();
  });

  it('returns null when prevValue is missing', () => {
    const { container } = render(<DeltaBadge current={42} />);

    expect(container.firstChild).toBeNull();
  });
});
