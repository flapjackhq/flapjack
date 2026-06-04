import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { IndexLayout } from '@/components/layout/IndexLayout'
import { SearchBrowse } from './SearchBrowse'

const { displayPreferencesModalSpy } = vi.hoisted(() => ({
  displayPreferencesModalSpy: vi.fn(),
}))

vi.mock('@/hooks/useIndexes', () => ({
  useIndexes: vi.fn(),
}))

vi.mock('@/hooks/useSettings', () => ({
  useSettings: vi.fn(),
  useEmbedderNames: vi.fn(),
}))

vi.mock('@/hooks/useSystemStatus', () => ({
  useHealthDetail: vi.fn(),
}))

// Mock child components — SearchBrowse tests focus on the page shell
vi.mock('@/components/search/SearchBox', () => ({
  SearchBox: () => <div data-testid="search-box" />,
}))
vi.mock('@/components/search/ResultsPanel', () => ({
  ResultsPanel: () => <div data-testid="results-panel" />,
}))
vi.mock('@/components/search/FacetsPanel', () => ({
  FacetsPanel: () => <div data-testid="facets-panel" />,
}))
vi.mock('@/components/documents/AddDocumentsDialog', () => ({
  AddDocumentsDialog: ({ open }: { open: boolean }) =>
    open ? <div data-testid="add-documents-dialog" /> : null,
}))
vi.mock('@/components/search/DisplayPreferencesModal', () => ({
  DisplayPreferencesModal: (props: {
    open: boolean
    onOpenChange: (open: boolean) => void
    indexName: string
  }) => {
    displayPreferencesModalSpy(props)
    return props.open ? (
      <div data-testid="display-preferences-modal">
        <button type="button" onClick={() => props.onOpenChange(false)}>
          Close Display Preferences
        </button>
      </div>
    ) : null
  },
}))

import { useIndexes } from '@/hooks/useIndexes'
import { useSettings, useEmbedderNames } from '@/hooks/useSettings'
import { useHealthDetail } from '@/hooks/useSystemStatus'

const INDEX = { uid: 'products', entries: 1234, dataSize: 5_000_000 }

function makeWrapper(path: string) {
  return function wrapper({ children }: { children: React.ReactNode }) {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } })
    return (
      <QueryClientProvider client={qc}>
        <MemoryRouter future={TEST_ROUTER_FUTURE} initialEntries={[path]}>
          <Routes>
            <Route path="/index/:indexName" element={<IndexLayout />}>
              <Route index element={children} />
            </Route>
            <Route path="/no-index" element={children} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>
    )
  }
}

const withIndex = makeWrapper('/index/products')
const withoutIndex = makeWrapper('/no-index')

describe('SearchBrowse', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    sessionStorage.clear()
    displayPreferencesModalSpy.mockClear()
    vi.mocked(useHealthDetail).mockReturnValue({
      data: {
        capabilities: {
          vectorSearch: true,
          vectorSearchLocal: true,
        },
      },
      isLoading: false,
    } as any)
    vi.mocked(useSettings).mockReturnValue({ data: undefined, isLoading: false } as any)
    vi.mocked(useEmbedderNames).mockReturnValue({ embedderNames: [], isLoading: false } as any)
  })

  it('does not emit console errors during add-documents flow', async () => {
    const user = userEvent.setup()
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {})
    try {
      vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)

      render(<SearchBrowse />, { wrapper: withIndex })
      await user.click(screen.getByRole('button', { name: /add documents/i }))

      await waitFor(() => {
        expect(screen.getByTestId('add-documents-dialog')).toBeInTheDocument()
      })
      expect(errorSpy).not.toHaveBeenCalled()
    } finally {
      errorSpy.mockRestore()
    }
  })

  it('shows no-index state when indexName is not in route', () => {
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    render(<SearchBrowse />, { wrapper: withoutIndex })
    expect(screen.getByText('No index selected')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: /go to overview/i })).toBeInTheDocument()
  })

  it('renders the index name in the header', () => {
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    render(<SearchBrowse />, { wrapper: withIndex })
    expect(screen.getByRole('heading', { name: 'products' })).toBeInTheDocument()
  })

  it('shows index stats (doc count and size) in the header', () => {
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    render(<SearchBrowse />, { wrapper: withIndex })
    expect(screen.getByText(/1,234 docs/)).toBeInTheDocument()
  })

  it('renders the SearchBox, ResultsPanel, and FacetsPanel', () => {
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    render(<SearchBrowse />, { wrapper: withIndex })
    expect(screen.getByTestId('search-box')).toBeInTheDocument()
    expect(screen.getByTestId('results-panel')).toBeInTheDocument()
    expect(screen.getByTestId('facets-panel')).toBeInTheDocument()
  })

  it('keeps vector controls hidden until health capability data resolves', () => {
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    vi.mocked(useSettings).mockReturnValue({
      data: {
        mode: 'neuralSearch',
        embedders: {
          default: { source: 'userProvided', dimensions: 384 },
        },
      },
      isLoading: false,
    } as any)
    vi.mocked(useEmbedderNames).mockReturnValue({
      embedderNames: ['default'],
      isLoading: false,
    } as any)
    vi.mocked(useHealthDetail).mockReturnValue({
      data: undefined,
      isLoading: false,
    } as any)

    render(<SearchBrowse />, { wrapper: withIndex })

    expect(screen.queryByTestId('vector-status-badge')).not.toBeInTheDocument()
    expect(screen.queryByTestId('vector-status-badge-disabled')).not.toBeInTheDocument()
    expect(screen.queryByTestId('hybrid-controls')).not.toBeInTheDocument()
  })

  it('does not foreground vector-unavailable status on the primary keyword browse route', () => {
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    vi.mocked(useHealthDetail).mockReturnValue({
      data: {
        capabilities: {
          vectorSearch: false,
          vectorSearchLocal: false,
        },
      },
      isLoading: false,
    } as any)

    render(<SearchBrowse />, { wrapper: withIndex })

    expect(screen.getByTestId('search-box')).toBeInTheDocument()
    expect(screen.queryByTestId('vector-status-badge-disabled')).not.toBeInTheDocument()
  })

  it('opens the Add Documents dialog when the button is clicked', async () => {
    const user = userEvent.setup()
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    render(<SearchBrowse />, { wrapper: withIndex })

    expect(screen.queryByTestId('add-documents-dialog')).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: /add documents/i }))
    expect(screen.getByTestId('add-documents-dialog')).toBeInTheDocument()
  })

  it('shows the Track Analytics toggle', () => {
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    render(<SearchBrowse />, { wrapper: withIndex })
    expect(screen.getByLabelText(/track analytics/i)).toBeInTheDocument()
  })

  it('does not persist an analytics user token until tracking is enabled', async () => {
    const user = userEvent.setup()
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)

    render(<SearchBrowse />, { wrapper: withIndex })

    expect(sessionStorage.getItem('fj-dashboard-user-token')).toBeNull()

    await user.click(screen.getByLabelText(/track analytics/i))

    expect(sessionStorage.getItem('fj-dashboard-user-token')).toMatch(/^dashboard-/)
  })

  it('keeps browse controls but delegates cross-page navigation to the shared index tab bar', () => {
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    render(<SearchBrowse />, { wrapper: withIndex })

    expect(screen.getByLabelText(/track analytics/i)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /add documents/i })).toBeInTheDocument()
    expect(screen.getByText(/1,234 docs/)).toBeInTheDocument()

    expect(screen.getAllByRole('link', { name: /settings/i })).toHaveLength(1)
    expect(screen.getAllByRole('link', { name: /analytics/i })).toHaveLength(1)
    expect(screen.getAllByRole('link', { name: /synonyms/i })).toHaveLength(1)
    expect(screen.getAllByRole('link', { name: /merchandising/i })).toHaveLength(1)
  })

  it('renders display preferences trigger in action bar alongside existing browse controls', () => {
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    render(<SearchBrowse />, { wrapper: withIndex })

    expect(screen.getByLabelText(/track analytics/i)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /add documents/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /display preferences/i })).toBeInTheDocument()
  })

  it('keeps display preferences modal closed initially, opens on click, and closes on onOpenChange(false)', async () => {
    const user = userEvent.setup()
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    render(<SearchBrowse />, { wrapper: withIndex })

    expect(screen.queryByTestId('display-preferences-modal')).not.toBeInTheDocument()
    expect(displayPreferencesModalSpy).toHaveBeenCalled()
    expect(displayPreferencesModalSpy.mock.lastCall?.[0]?.open).toBe(false)

    await user.click(screen.getByRole('button', { name: /display preferences/i }))
    expect(screen.getByTestId('display-preferences-modal')).toBeInTheDocument()
    expect(displayPreferencesModalSpy.mock.lastCall?.[0]?.open).toBe(true)

    await user.click(screen.getByRole('button', { name: /close display preferences/i }))
    expect(screen.queryByTestId('display-preferences-modal')).not.toBeInTheDocument()
    expect(displayPreferencesModalSpy.mock.lastCall?.[0]?.open).toBe(false)
  })

  it('passes only open, onOpenChange, and current route indexName to DisplayPreferencesModal', () => {
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    render(<SearchBrowse />, { wrapper: withIndex })

    const modalProps = displayPreferencesModalSpy.mock.lastCall?.[0]
    expect(modalProps).toBeDefined()
    expect(modalProps.indexName).toBe('products')
    expect(Object.keys(modalProps).sort()).toEqual(['indexName', 'onOpenChange', 'open'])
    expect(typeof modalProps.onOpenChange).toBe('function')
  })

  it('keeps no-index route behavior and does not render display preferences action or modal entry point', () => {
    vi.mocked(useIndexes).mockReturnValue({ data: [INDEX], isLoading: false } as any)
    render(<SearchBrowse />, { wrapper: withoutIndex })

    expect(screen.getByText('No index selected')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /display preferences/i })).not.toBeInTheDocument()
    expect(screen.queryByTestId('display-preferences-modal')).not.toBeInTheDocument()
    expect(displayPreferencesModalSpy).not.toHaveBeenCalled()
  })
})
