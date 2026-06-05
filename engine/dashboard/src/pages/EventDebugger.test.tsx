import { describe, it, expect, vi, beforeEach } from 'vitest'
import { act, render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { EventDebugger } from './EventDebugger'

const mockDebugEvents = vi.hoisted(() => vi.fn())

vi.mock('@/hooks/useDebugEvents', () => ({
  useDebugEvents: mockDebugEvents,
}))

function wrapper({ children }: { children: React.ReactNode }) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return (
    <QueryClientProvider client={qc}>
      <MemoryRouter future={TEST_ROUTER_FUTURE} initialEntries={['/events']}>
        <Routes>
          <Route path="/events" element={children} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const VALID_EVENT = {
  timestampMs: 1709251200000,
  index: 'products',
  eventType: 'view',
  eventSubtype: null,
  eventName: 'Viewed Product',
  userToken: 'user_abc',
  objectIds: ['obj1', 'obj2'],
  httpCode: 200,
  validationErrors: [],
}

const ERROR_EVENT = {
  timestampMs: 1709251201000,
  index: 'products',
  eventType: 'bogus',
  eventSubtype: null,
  eventName: 'Bad Event',
  userToken: 'user_xyz',
  objectIds: ['obj3'],
  httpCode: 422,
  validationErrors: ['Invalid eventType: bogus'],
}

describe('EventDebugger', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('shows empty state when no events exist', () => {
    mockDebugEvents.mockReturnValue({
      data: { events: [], count: 0 },
      isLoading: false,
      isError: false,
    })
    render(<EventDebugger />, { wrapper })
    expect(
      screen.getByText(/no events received yet/i)
    ).toBeInTheDocument()
  })

  it('shows loading state', () => {
    mockDebugEvents.mockReturnValue({
      data: undefined,
      isLoading: true,
      isError: false,
    })
    render(<EventDebugger />, { wrapper })
    // Should show some loading indicator, no event table
    expect(screen.queryByTestId('event-table')).not.toBeInTheDocument()
  })

  it('shows error state when backend unavailable', () => {
    mockDebugEvents.mockReturnValue({
      data: undefined,
      isLoading: false,
      isError: true,
    })
    render(<EventDebugger />, { wrapper })
    expect(screen.getByText(/unable to load events/i)).toBeInTheDocument()
  })

  it('renders event table with valid and invalid events', () => {
    mockDebugEvents.mockReturnValue({
      data: { events: [ERROR_EVENT, VALID_EVENT], count: 2 },
      isLoading: false,
      isError: false,
    })
    render(<EventDebugger />, { wrapper })

    const table = screen.getByTestId('event-table')
    expect(table).toBeInTheDocument()

    // Both events should be in the table
    const rows = within(table).getAllByTestId('event-row')
    expect(rows).toHaveLength(2)

    // Check status badges (scoped to table to avoid matching filter dropdown options)
    expect(within(table).getByText('OK')).toBeInTheDocument()
    expect(within(table).getByText('Error')).toBeInTheDocument()

    // Check event types displayed
    expect(within(table).getByText('view')).toBeInTheDocument()
    expect(within(table).getByText('bogus')).toBeInTheDocument()
  })

  it('shows event detail when clicking a row', async () => {
    const user = userEvent.setup()
    mockDebugEvents.mockReturnValue({
      data: { events: [VALID_EVENT], count: 1 },
      isLoading: false,
      isError: false,
    })
    render(<EventDebugger />, { wrapper })

    const row = screen.getByTestId('event-row')
    await user.click(row)

    // Detail panel should appear
    const detail = screen.getByTestId('event-detail')
    expect(detail).toBeInTheDocument()
    expect(within(detail).getByText('Viewed Product')).toBeInTheDocument()
    expect(within(detail).getByText('user_abc')).toBeInTheDocument()
  })

  it('clears a selected event when filters load a different result set', async () => {
    const user = userEvent.setup()
    mockDebugEvents.mockImplementation((filters) => ({
      data: {
        events: filters.status === 'error' ? [ERROR_EVENT] : [VALID_EVENT],
        count: 1,
      },
      isLoading: false,
      isError: false,
    }))

    render(<EventDebugger />, { wrapper })

    await user.click(screen.getByTestId('event-row'))
    expect(screen.getByTestId('event-detail')).toBeInTheDocument()

    await user.selectOptions(screen.getByLabelText('Status'), 'error')

    expect(screen.queryByTestId('event-detail')).not.toBeInTheDocument()
    expect(screen.getByTestId('event-table')).toHaveTextContent('Bad Event')
  })

  it('preserves duplicate-event selection by row across refreshes', async () => {
    const user = userEvent.setup()
    const initialDuplicateEvents = [{ ...VALID_EVENT }, { ...VALID_EVENT }]
    const refreshedDuplicateEvents = [{ ...VALID_EVENT }, { ...VALID_EVENT }]
    mockDebugEvents.mockImplementation((filters) => ({
      data: {
        events: filters.status === 'ok' ? refreshedDuplicateEvents : initialDuplicateEvents,
        count: initialDuplicateEvents.length,
      },
      isLoading: false,
      isError: false,
    }))

    render(<EventDebugger />, { wrapper })

    let rows = screen.getAllByTestId('event-row')
    await user.click(rows[1])

    expect(rows[0]).not.toHaveClass('bg-accent')
    expect(rows[1]).toHaveClass('bg-accent')

    await user.selectOptions(screen.getByLabelText('Status'), 'ok')

    rows = screen.getAllByTestId('event-row')
    expect(rows[0]).not.toHaveClass('bg-accent')
    expect(rows[1]).toHaveClass('bg-accent')
  })

  it('shows validation errors in detail panel for error events', async () => {
    const user = userEvent.setup()
    mockDebugEvents.mockReturnValue({
      data: { events: [ERROR_EVENT], count: 1 },
      isLoading: false,
      isError: false,
    })
    render(<EventDebugger />, { wrapper })

    await user.click(screen.getByTestId('event-row'))

    const detail = screen.getByTestId('event-detail')
    expect(within(detail).getByText('Invalid eventType: bogus')).toBeInTheDocument()
  })

  it('filters events by status', async () => {
    const user = userEvent.setup()
    mockDebugEvents.mockReturnValue({
      data: { events: [ERROR_EVENT, VALID_EVENT], count: 2 },
      isLoading: false,
      isError: false,
    })
    render(<EventDebugger />, { wrapper })

    const statusSelect = screen.getByLabelText('Status')
    const failedStatusOption = within(statusSelect).getByRole('option', { name: 'Failed' })
    expect(failedStatusOption).toHaveValue('error')

    await user.selectOptions(statusSelect, failedStatusOption)

    // Hook should have been called with status filter
    expect(mockDebugEvents).toHaveBeenCalledWith(
      expect.objectContaining({ status: 'error' }),
      false,
    )
  })

  it('filters events by event type', async () => {
    const user = userEvent.setup()
    mockDebugEvents.mockReturnValue({
      data: { events: [VALID_EVENT], count: 1 },
      isLoading: false,
      isError: false,
    })
    render(<EventDebugger />, { wrapper })

    await user.selectOptions(screen.getByLabelText('Event Type'), 'view')

    expect(mockDebugEvents).toHaveBeenCalledWith(
      expect.objectContaining({ eventType: 'view' }),
      false,
    )
  })

  it('filters events by index text input', async () => {
    const user = userEvent.setup()
    mockDebugEvents.mockReturnValue({
      data: { events: [VALID_EVENT], count: 1 },
      isLoading: false,
      isError: false,
    })
    render(<EventDebugger />, { wrapper })

    await user.type(screen.getByLabelText('Index'), 'products')

    expect(mockDebugEvents).toHaveBeenCalledWith(
      expect.objectContaining({ index: 'products' }),
      false,
    )
  })

  it('copies full payload from detail panel', async () => {
    const user = userEvent.setup()
    const writeText = vi.fn()
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText },
    })
    mockDebugEvents.mockReturnValue({
      data: { events: [VALID_EVENT], count: 1 },
      isLoading: false,
      isError: false,
    })
    render(<EventDebugger />, { wrapper })

    await user.click(screen.getByTestId('event-row'))
    await user.click(screen.getByRole('button', { name: 'Copy payload' }))

    expect(writeText).toHaveBeenCalledTimes(1)
    expect(writeText).toHaveBeenCalledWith(expect.stringContaining('"eventName": "Viewed Product"'))
  })

  it('shows event count in header', () => {
    mockDebugEvents.mockReturnValue({
      data: { events: [VALID_EVENT, ERROR_EVENT], count: 2 },
      isLoading: false,
      isError: false,
    })
    render(<EventDebugger />, { wrapper })
    expect(screen.getByTestId('event-count')).toHaveTextContent('2')
  })

  it('renders date range filter control', () => {
    mockDebugEvents.mockReturnValue({
      data: { events: [VALID_EVENT], count: 1 },
      isLoading: false,
      isError: false,
    })

    render(<EventDebugger />, { wrapper })
    expect(screen.getByLabelText('Date Range')).toBeInTheDocument()
  })

  it('passes date range window to the debug-events hook', async () => {
    const user = userEvent.setup()
    const nowSpy = vi.spyOn(Date, 'now').mockReturnValue(1_700_000_000_000)
    mockDebugEvents.mockReturnValue({
      data: { events: [VALID_EVENT], count: 1 },
      isLoading: false,
      isError: false,
    })

    render(<EventDebugger />, { wrapper })
    expect(mockDebugEvents).toHaveBeenCalledWith(
      expect.objectContaining({
        from: 1_700_000_000_000 - 24 * 60 * 60 * 1000,
        until: 1_700_000_000_000,
      }),
      false,
    )

    await user.selectOptions(screen.getByLabelText('Date Range'), 'all')
    expect(mockDebugEvents).toHaveBeenLastCalledWith(
      expect.objectContaining({
        from: undefined,
        until: undefined,
      }),
      5000,
    )
    nowSpy.mockRestore()
  })

  it('advances the active date range window while polling', () => {
    vi.useFakeTimers()
    vi.setSystemTime(1_700_000_000_000)
    mockDebugEvents.mockReturnValue({
      data: { events: [VALID_EVENT], count: 1 },
      isLoading: false,
      isError: false,
    })

    try {
      render(<EventDebugger />, { wrapper })

      expect(mockDebugEvents).toHaveBeenLastCalledWith(
        expect.objectContaining({
          from: 1_700_000_000_000 - 24 * 60 * 60 * 1000,
          until: 1_700_000_000_000,
        }),
        false,
      )

      act(() => {
        vi.advanceTimersByTime(5_000)
      })

      expect(mockDebugEvents).toHaveBeenLastCalledWith(
        expect.objectContaining({
          from: 1_700_000_005_000 - 24 * 60 * 60 * 1000,
          until: 1_700_000_005_000,
        }),
        false,
      )
    } finally {
      vi.useRealTimers()
    }
  })

  it('renders event volume chart when events are present', () => {
    mockDebugEvents.mockReturnValue({
      data: { events: [VALID_EVENT, ERROR_EVENT], count: 2 },
      isLoading: false,
      isError: false,
    })

    render(<EventDebugger />, { wrapper })
    expect(screen.getByTestId('event-volume-chart')).toBeInTheDocument()
  })
})
