import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router-dom';
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture';
import { Sidebar } from './Sidebar';

vi.mock('@/hooks/useIndexes', () => ({
  useIndexes: vi.fn(),
}));

import { useIndexes } from '@/hooks/useIndexes';

const MANY_INDEXES = [
  { uid: 'idx-1' },
  { uid: 'idx-2' },
  { uid: 'idx-3' },
  { uid: 'idx-4' },
  { uid: 'idx-5' },
  { uid: 'idx-6' },
];

describe('Sidebar', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('renders grouped sections in design order and supports expanding index list', async () => {
    const user = userEvent.setup();
    vi.mocked(useIndexes).mockReturnValue({
      data: MANY_INDEXES,
      isLoading: false,
    } as unknown as ReturnType<typeof useIndexes>);

    render(
      <MemoryRouter initialEntries={['/overview']} future={TEST_ROUTER_FUTURE}>
        <Sidebar />
      </MemoryRouter>,
    );

    const sectionHeadings = screen.getAllByTestId(/sidebar-section-heading-/).map((heading) => heading.textContent);
    expect(sectionHeadings).toEqual([
      'Indexes',
      'Intelligence',
      'Developer',
      'System',
    ]);

    const indexesSection = screen.getByTestId('sidebar-section-indexes');
    const intelligenceSection = screen.getByTestId('sidebar-section-intelligence');
    const developerSection = screen.getByTestId('sidebar-section-developer');
    const systemSection = screen.getByTestId('sidebar-section-system');

    expect(screen.getByRole('navigation', { name: 'Indexes' })).toBeInTheDocument();
    expect(screen.getByRole('navigation', { name: 'Intelligence' })).toBeInTheDocument();
    expect(screen.getByRole('navigation', { name: 'Developer' })).toBeInTheDocument();
    expect(screen.getByRole('navigation', { name: 'System' })).toBeInTheDocument();

    expect(within(indexesSection).getByRole('link', { name: 'Overview' })).toHaveAttribute('href', '/overview');
    expect(within(intelligenceSection).getByRole('link', { name: 'Query Suggestions' })).toHaveAttribute('href', '/query-suggestions');
    expect(within(intelligenceSection).getByRole('link', { name: 'Experiments' })).toHaveAttribute('href', '/experiments');
    expect(within(intelligenceSection).getByRole('link', { name: 'Personalization' })).toHaveAttribute('href', '/personalization');
    expect(within(developerSection).getByRole('link', { name: 'API Keys' })).toHaveAttribute('href', '/keys');
    expect(within(developerSection).getByRole('link', { name: 'Security Sources' })).toHaveAttribute('href', '/security-sources');
    expect(within(developerSection).getByRole('link', { name: 'Dictionaries' })).toHaveAttribute('href', '/dictionaries');
    expect(within(developerSection).getByRole('link', { name: 'API Logs' })).toHaveAttribute('href', '/logs');
    expect(within(developerSection).getByRole('link', { name: 'Event Debugger' })).toHaveAttribute('href', '/events');
    expect(within(systemSection).getByRole('link', { name: 'Migrate' })).toHaveAttribute('href', '/migrate');
    expect(within(systemSection).getByRole('link', { name: 'Metrics' })).toHaveAttribute('href', '/metrics');
    expect(within(systemSection).getByRole('link', { name: 'System' })).toHaveAttribute('href', '/system');
    expect(within(systemSection).getByRole('link', { name: 'Cluster' })).toHaveAttribute('href', '/cluster');
    expect(screen.getAllByRole('link', { name: 'Cluster' })).toHaveLength(1);

    expect(screen.getByTestId('sidebar-indexes')).toBeInTheDocument();
    expect(screen.getByTestId('sidebar-indexes-header')).toBeInTheDocument();
    expect(screen.getByTestId('sidebar-index-idx-1')).toBeInTheDocument();
    expect(screen.getByTestId('sidebar-index-idx-5')).toBeInTheDocument();
    expect(screen.queryByTestId('sidebar-index-idx-6')).not.toBeInTheDocument();

    await user.click(screen.getByTestId('sidebar-show-all-indexes'));

    expect(screen.getByTestId('sidebar-index-idx-6')).toBeInTheDocument();
    expect(screen.getByText('Show less')).toBeInTheDocument();
  });

  it('keeps the dynamic index item active when browsing index routes', () => {
    vi.mocked(useIndexes).mockReturnValue({
      data: MANY_INDEXES,
      isLoading: false,
    } as unknown as ReturnType<typeof useIndexes>);

    render(
      <MemoryRouter initialEntries={['/index/idx-1']} future={TEST_ROUTER_FUTURE}>
        <Sidebar />
      </MemoryRouter>,
    );

    expect(screen.getByTestId('sidebar-index-idx-1')).toHaveClass('bg-primary/15');
    expect(screen.getByTestId('sidebar-index-idx-1')).toHaveClass('text-primary');
  });

  it('marks Overview active on the root route because it renders the overview page', () => {
    vi.mocked(useIndexes).mockReturnValue({
      data: MANY_INDEXES,
      isLoading: false,
    } as unknown as ReturnType<typeof useIndexes>);

    render(
      <MemoryRouter initialEntries={['/']} future={TEST_ROUTER_FUTURE}>
        <Sidebar />
      </MemoryRouter>,
    );

    expect(screen.getByRole('link', { name: 'Overview' })).toHaveClass('bg-primary/15');
    expect(screen.getByRole('link', { name: 'Overview' })).toHaveClass('text-primary');
  });

  it('does not mark prefix-colliding indexes active on nested routes', () => {
    vi.mocked(useIndexes).mockReturnValue({
      data: [{ uid: 'idx-1' }, { uid: 'idx-10' }],
      isLoading: false,
    } as unknown as ReturnType<typeof useIndexes>);

    render(
      <MemoryRouter initialEntries={['/index/idx-10/settings']} future={TEST_ROUTER_FUTURE}>
        <Sidebar />
      </MemoryRouter>,
    );

    expect(screen.getByTestId('sidebar-index-idx-10')).toHaveClass('bg-primary/15');
    expect(screen.getByTestId('sidebar-index-idx-1')).not.toHaveClass('bg-primary/15');
  });

  it('calls onClose when navigation changes route', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    vi.mocked(useIndexes).mockReturnValue({
      data: MANY_INDEXES,
      isLoading: false,
    } as unknown as ReturnType<typeof useIndexes>);

    render(
      <MemoryRouter initialEntries={['/overview']} future={TEST_ROUTER_FUTURE}>
        <Sidebar onClose={onClose} />
      </MemoryRouter>,
    );

    expect(onClose).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole('link', { name: 'API Keys' }));

    await waitFor(() => {
      expect(onClose).toHaveBeenCalledTimes(2);
    });
  });
});
