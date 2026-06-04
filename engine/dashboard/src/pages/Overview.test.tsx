import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes, useParams } from 'react-router-dom'
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { Overview } from './Overview'

const hookMutations = vi.hoisted(() => ({
  deleteMutate: vi.fn(),
  exportMutate: vi.fn(),
  importMutate: vi.fn(),
}))

const { responsiveContainerProps } = vi.hoisted(() => ({
  responsiveContainerProps: [] as Array<Record<string, unknown>>,
}))

// Mock API module
vi.mock('@/lib/api', () => ({
  default: {
    post: vi.fn(),
    get: vi.fn(),
    delete: vi.fn(),
  },
}))

// Mock hooks
vi.mock('@/hooks/useIndexes', () => ({
  useIndexes: vi.fn(),
  useDeleteIndex: () => ({ mutate: hookMutations.deleteMutate, isPending: false }),
  useCreateIndex: () => ({ mutate: vi.fn(), isPending: false, isSuccess: false, reset: vi.fn() }),
}))

vi.mock('@/hooks/useHealth', () => ({
  useHealth: () => ({ data: { status: 'ok' }, isLoading: false, error: null }),
}))

vi.mock('@/hooks/useAnalytics', () => ({
  useAnalyticsOverview: vi.fn(),
  defaultRange: () => ({ startDate: '2026-02-09', endDate: '2026-02-16' }),
}))

vi.mock('@/hooks/useSnapshots', () => ({
  useExportIndex: () => ({ mutate: hookMutations.exportMutate, isPending: false }),
  useImportIndex: () => ({ mutate: hookMutations.importMutate, isPending: false }),
}))

// Mock recharts to avoid rendering issues in jsdom
vi.mock('recharts', () => ({
  AreaChart: ({ children }: any) => <div data-testid="mock-chart">{children}</div>,
  Area: () => null,
  ResponsiveContainer: ({ children, ...props }: any) => {
    responsiveContainerProps.push(props)
    return <div>{children}</div>
  },
  XAxis: () => null,
  YAxis: () => null,
  CartesianGrid: () => null,
  Tooltip: () => null,
}))

import api from '@/lib/api'
import { useIndexes } from '@/hooks/useIndexes'
import { useAnalyticsOverview } from '@/hooks/useAnalytics'

type MockIndex = {
  uid: string
  entries?: number
  dataSize?: number
  updatedAt?: string
  numberOfPendingTasks?: number
}

function createWrapper() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0, staleTime: 0 },
      mutations: { retry: false },
    },
  })

  return ({ children }: { children: React.ReactNode }) => (
    <QueryClientProvider client={queryClient}>
      <MemoryRouter future={TEST_ROUTER_FUTURE}>{children}</MemoryRouter>
    </QueryClientProvider>
  )
}

function IndexRouteProbe() {
  const { indexName } = useParams<{ indexName: string }>()
  return <div data-testid={`index-route-${indexName}`}>Index route {indexName}</div>
}

function createRoutedWrapper() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0, staleTime: 0 },
      mutations: { retry: false },
    },
  })

  return ({ children }: { children: React.ReactNode }) => (
    <QueryClientProvider client={queryClient}>
      <MemoryRouter future={TEST_ROUTER_FUTURE} initialEntries={['/overview']}>
        <Routes>
          <Route path="/overview" element={children} />
          <Route path="/index/:indexName" element={<IndexRouteProbe />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function setOverviewState({
  indexes = [{ uid: 'products', entries: 100, dataSize: 1024 }],
  totalSearches = 50,
}: {
  indexes?: MockIndex[]
  totalSearches?: number
} = {}) {
  vi.mocked(useIndexes).mockReturnValue({
    data: indexes,
    isLoading: false,
    error: null,
  } as any)
  vi.mocked(useAnalyticsOverview).mockReturnValue({
    data: {
      totalSearches,
      uniqueUsers: totalSearches > 0 ? 10 : 0,
      noResultRate: totalSearches > 0 ? 0.05 : null,
      dates: totalSearches > 0
        ? [
            { date: '2026-02-09', count: Math.floor(totalSearches / 2) },
            { date: '2026-02-10', count: totalSearches },
          ]
        : [],
      indices: [],
    },
    isLoading: false,
  } as any)
}

describe('Overview Cleanup Button', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('does not emit console warnings during initial render', () => {
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      setOverviewState()

      render(<Overview />, { wrapper: createWrapper() })

      expect(warnSpy).not.toHaveBeenCalled()
    } finally {
      warnSpy.mockRestore()
    }
  })

  it('gives the overview analytics chart a positive minimum width before layout measurement', () => {
    responsiveContainerProps.length = 0
    setOverviewState()

    render(<Overview />, { wrapper: createWrapper() })

    expect(screen.getByTestId('overview-analytics-chart')).toBeInTheDocument()
    expect(responsiveContainerProps).toContainEqual(
      expect.objectContaining({ minWidth: 1 }),
    )
  })

  it('shows cleanup button when analytics card is visible', () => {
    setOverviewState()

    render(<Overview />, { wrapper: createWrapper() })

    expect(screen.getByTestId('overview-cleanup-btn')).toBeInTheDocument()
  })

  it('does not show cleanup button when analytics card is hidden', () => {
    setOverviewState({ totalSearches: 0 })

    render(<Overview />, { wrapper: createWrapper() })

    expect(screen.queryByTestId('overview-cleanup-btn')).not.toBeInTheDocument()
  })

  it('opens confirmation dialog when cleanup button is clicked', async () => {
    const user = userEvent.setup()
    setOverviewState()

    render(<Overview />, { wrapper: createWrapper() })

    await user.click(screen.getByTestId('overview-cleanup-btn'))

    expect(screen.getByText('Cleanup Analytics')).toBeInTheDocument()
    expect(
      screen.getByText(
        'This will remove analytics data for indexes that no longer exist. Analytics for your active indexes will not be affected.'
      )
    ).toBeInTheDocument()
  })

  it('calls POST /2/analytics/cleanup when confirmed', async () => {
    const user = userEvent.setup()
    setOverviewState()
    vi.mocked(api.post).mockResolvedValueOnce({
      data: { status: 'ok', removedIndices: ['old-index'], removedCount: 1 },
    })

    render(<Overview />, { wrapper: createWrapper() })

    // Open dialog
    await user.click(screen.getByTestId('overview-cleanup-btn'))

    // Confirm
    const confirmButton = screen.getByRole('button', { name: /cleanup/i })
    await user.click(confirmButton)

    await waitFor(() => {
      expect(api.post).toHaveBeenCalledWith('/2/analytics/cleanup')
    })
  })

  it('shows success message after cleanup', async () => {
    const user = userEvent.setup()
    setOverviewState()
    vi.mocked(api.post).mockResolvedValueOnce({
      data: { status: 'ok', removedIndices: [], removedCount: 0 },
    })

    render(<Overview />, { wrapper: createWrapper() })

    await user.click(screen.getByTestId('overview-cleanup-btn'))

    const confirmButton = screen.getByRole('button', { name: /cleanup/i })
    await user.click(confirmButton)

    await waitFor(() => {
      expect(screen.getByText('Cleaned up')).toBeInTheDocument()
    })
  })

  it('handles API error gracefully', async () => {
    const user = userEvent.setup()
    setOverviewState()
    vi.mocked(api.post).mockRejectedValueOnce(new Error('Server error'))

    render(<Overview />, { wrapper: createWrapper() })

    await user.click(screen.getByTestId('overview-cleanup-btn'))

    const confirmButton = screen.getByRole('button', { name: /cleanup/i })
    await user.click(confirmButton)

    // After error, the dialog should close and button should still be available
    await waitFor(() => {
      expect(screen.getByTestId('overview-cleanup-btn')).toBeInTheDocument()
    })
  })
})

describe('Overview index list contracts', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('renders stable row and metadata testids with index status details', () => {
    setOverviewState({
      indexes: [{
        uid: 'products',
        entries: 1234,
        dataSize: 2048,
        updatedAt: '2026-03-17T12:30:00Z',
        numberOfPendingTasks: 3,
      }],
    })

    render(<Overview />, { wrapper: createWrapper() })

    expect(screen.getByTestId('overview-index-row-products')).toBeInTheDocument()
    const meta = screen.getByTestId('overview-index-meta-products')
    expect(meta).toHaveTextContent(/1,234 documents/i)
    expect(meta).toHaveTextContent(/2 KB/i)
    expect(meta).toHaveTextContent(/updated/i)
    expect(meta).toHaveTextContent(/3 pending/i)
  })

  it('navigates to the index page when clicking the index row body', async () => {
    const user = userEvent.setup()
    setOverviewState()

    render(<Overview />, { wrapper: createRoutedWrapper() })

    await user.click(screen.getByTestId('overview-index-row-products'))

    expect(screen.getByTestId('index-route-products')).toBeInTheDocument()
  })

  it('invokes per-index export without triggering row navigation', async () => {
    const user = userEvent.setup()
    setOverviewState()

    render(<Overview />, { wrapper: createRoutedWrapper() })

    await user.click(screen.getByTestId('overview-export-products'))

    expect(hookMutations.exportMutate).toHaveBeenCalledWith('products')
    expect(screen.queryByTestId('index-route-products')).not.toBeInTheDocument()
  })

  it('paginates index rows and updates the visible page range', async () => {
    const user = userEvent.setup()
    const indexes = Array.from({ length: 11 }, (_, i) => ({
      uid: `products-${i + 1}`,
      entries: i + 1,
      dataSize: 1024 * (i + 1),
    }))
    setOverviewState({ indexes })

    render(<Overview />, { wrapper: createWrapper() })

    expect(screen.getByTestId('overview-index-row-products-1')).toBeInTheDocument()
    expect(screen.queryByTestId('overview-index-row-products-11')).not.toBeInTheDocument()
    expect(screen.getByText('Showing 1-10 of 11 indexes')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /next/i }))

    expect(screen.getByTestId('overview-index-row-products-11')).toBeInTheDocument()
    expect(screen.queryByTestId('overview-index-row-products-1')).not.toBeInTheDocument()
    expect(screen.getByText('Showing 11-11 of 11 indexes')).toBeInTheDocument()
  })
})
