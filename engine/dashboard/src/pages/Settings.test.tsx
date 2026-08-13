import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { IndexLayout } from '@/components/layout/IndexLayout'
import { Settings } from './Settings'

const hooksMocks = vi.hoisted(() => ({
  updateMutateAsync: vi.fn().mockResolvedValue({}),
  compactMutate: vi.fn(),
  settingsFormProps: vi.fn(),
}))

vi.mock('@/hooks/useSettings', () => ({
  useSettings: vi.fn(),
  useUpdateSettings: () => ({ mutateAsync: hooksMocks.updateMutateAsync, isPending: false }),
}))

vi.mock('@/hooks/useIndexes', () => ({
  useCompactIndex: () => ({ mutate: hooksMocks.compactMutate, isPending: false }),
}))

// Monaco editor is heavy — stub it out
vi.mock('@monaco-editor/react', () => ({
  default: ({ value }: { value: string }) => (
    <div data-testid="monaco-editor">{value}</div>
  ),
}))

vi.mock('@/components/settings/SettingsForm', () => ({
  SettingsForm: (props: {
    onChange: (updates: Record<string, unknown>) => void
    settings: unknown
    savedSettings: unknown
    indexName: string
  }) => (
    <div data-testid="settings-form">
      {hooksMocks.settingsFormProps(props) && null}
      <button onClick={() => props.onChange({ searchableAttributes: ['name'] })}>
        Change Setting
      </button>
      <button onClick={() => props.onChange({ queryType: 'prefixNone' })}>
        Change QueryType
      </button>
      <button onClick={() => props.onChange({ queryLanguages: [] })}>
        Clear QueryLanguages
      </button>
      <button onClick={() => props.onChange({ unretrievableAttributes: ['secret', 'internal'] })}>
        Change Unretrievable
      </button>
      <button onClick={() => props.onChange({ unretrievableAttributes: [] })}>
        Clear Unretrievable
      </button>
      <button onClick={() => props.onChange({ userData: { aiProvider: { baseUrl: 'https://api.openai.com/v1', model: 'gpt-4o', apiKey: 'sk-test' } } })}>
        Change AiProvider
      </button>
    </div>
  ),
}))

import { useSettings } from '@/hooks/useSettings'

const SAMPLE_SETTINGS = {
  searchableAttributes: ['name', 'description'],
  attributesForFaceting: ['brand'],
  customRanking: [],
  ranking: [],
}

function makeWrapper(indexName?: string) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  const path = indexName ? `/index/${indexName}/settings` : '/settings'

  return ({ children }: { children: React.ReactNode }) => (
    <QueryClientProvider client={qc}>
      <MemoryRouter future={TEST_ROUTER_FUTURE} initialEntries={[path]}>
        <Routes>
          <Route path="/index/:indexName" element={<IndexLayout />}>
            <Route path="settings" element={children} />
          </Route>
          <Route path="/settings" element={children} />
          <Route path="/overview" element={<div>Overview</div>} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

describe('Settings', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    hooksMocks.updateMutateAsync.mockResolvedValue({})
  })

  it('shows "No index selected" when no indexName param', () => {
    vi.mocked(useSettings).mockReturnValue({ data: undefined, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper(undefined) })
    expect(screen.getByText('No index selected')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: /go to overview/i })).toBeInTheDocument()
  })

  it('shows loading skeleton when settings are loading', () => {
    vi.mocked(useSettings).mockReturnValue({ data: undefined, isLoading: true } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })
    // SettingsForm not rendered yet during load
    expect(screen.queryByTestId('settings-form')).not.toBeInTheDocument()
  })

  it('renders settings form when loaded', () => {
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })
    expect(screen.getByTestId('settings-form')).toBeInTheDocument()
  })

  it('keeps breadcrumb and back-link in page header', () => {
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })

    expect(screen.getByRole('heading', { name: 'Settings' })).toBeInTheDocument()
    expect(screen.getByRole('link', { name: /products/i })).toHaveAttribute(
      'href',
      '/index/products'
    )
  })

  it('swaps form and JSON editor from the page-level JSON toggle', async () => {
    const user = userEvent.setup()
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })

    expect(screen.getByTestId('settings-form')).toBeInTheDocument()
    expect(screen.queryByTestId('monaco-editor')).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /json/i }))
    expect(await screen.findByTestId('monaco-editor')).toBeInTheDocument()
    expect(screen.queryByTestId('settings-form')).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /json/i }))
    expect(screen.getByTestId('settings-form')).toBeInTheDocument()
    expect(screen.queryByTestId('monaco-editor')).not.toBeInTheDocument()
  })

  it('shows page-owned Save and Reset buttons when form is dirty', async () => {
    const user = userEvent.setup()
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })

    await user.click(screen.getByRole('button', { name: /change setting/i }))

    expect(screen.getByRole('button', { name: /save changes/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /reset/i })).toBeInTheDocument()
  })

  it('keeps Save and Reset visible in header while JSON view is open', async () => {
    const user = userEvent.setup()
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })

    await user.click(screen.getByRole('button', { name: /change setting/i }))
    await user.click(screen.getByRole('button', { name: /json/i }))

    expect(screen.getByRole('button', { name: /save changes/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /reset/i })).toBeInTheDocument()
    expect(await screen.findByTestId('monaco-editor')).toBeInTheDocument()
  })

  it('save and reset are handled by the page state', async () => {
    const user = userEvent.setup()
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })

    await user.click(screen.getByRole('button', { name: /change setting/i }))
    await user.click(screen.getByRole('button', { name: /save changes/i }))

    expect(hooksMocks.updateMutateAsync).toHaveBeenCalledWith({
      searchableAttributes: ['name'],
    })
    await waitFor(() => {
      expect(screen.queryByRole('button', { name: /save changes/i })).not.toBeInTheDocument()
    })

    await user.click(screen.getByRole('button', { name: /change setting/i }))
    await user.click(screen.getByRole('button', { name: /reset/i }))

    await waitFor(() => {
      expect(screen.queryByRole('button', { name: /save changes/i })).not.toBeInTheDocument()
    })
  })

  it('saves a Stage 5 field through the page state merge into the mutation', async () => {
    const user = userEvent.setup()
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })

    await user.click(screen.getByRole('button', { name: /change querytype/i }))
    await user.click(screen.getByRole('button', { name: /save changes/i }))

    expect(hooksMocks.updateMutateAsync).toHaveBeenCalledWith({
      queryType: 'prefixNone',
    })
  })

  it('saves a cleared queryLanguages payload as an empty list', async () => {
    const user = userEvent.setup()
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })

    await user.click(screen.getByRole('button', { name: /clear querylanguages/i }))
    await user.click(screen.getByRole('button', { name: /save changes/i }))

    expect(hooksMocks.updateMutateAsync).toHaveBeenCalledWith({
      queryLanguages: [],
    })
  })

  it('saves unretrievableAttributes through the page state merge into the mutation', async () => {
    const user = userEvent.setup()
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })

    await user.click(screen.getByRole('button', { name: /change unretrievable/i }))
    await user.click(screen.getByRole('button', { name: /save changes/i }))

    expect(hooksMocks.updateMutateAsync).toHaveBeenCalledWith({
      unretrievableAttributes: ['secret', 'internal'],
    })
  })

  it('saves cleared unretrievableAttributes as an empty list', async () => {
    const user = userEvent.setup()
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })

    await user.click(screen.getByRole('button', { name: /clear unretrievable/i }))
    await user.click(screen.getByRole('button', { name: /save changes/i }))

    expect(hooksMocks.updateMutateAsync).toHaveBeenCalledWith({
      unretrievableAttributes: [],
    })
  })

  it('saves userData.aiProvider through the page state merge into the mutation', async () => {
    const user = userEvent.setup()
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })

    await user.click(screen.getByRole('button', { name: /change aiprovider/i }))
    await user.click(screen.getByRole('button', { name: /save changes/i }))

    expect(hooksMocks.updateMutateAsync).toHaveBeenCalledWith({
      userData: {
        aiProvider: { baseUrl: 'https://api.openai.com/v1', model: 'gpt-4o', apiKey: 'sk-test' },
      },
    })
  })

  it('passes merged settings and savedSettings separately into SettingsForm', async () => {
    const user = userEvent.setup()
    vi.mocked(useSettings).mockReturnValue({ data: SAMPLE_SETTINGS, isLoading: false } as any)
    render(<Settings />, { wrapper: makeWrapper('products') })

    await user.click(screen.getByRole('button', { name: /change setting/i }))

    const lastCall =
      hooksMocks.settingsFormProps.mock.calls[hooksMocks.settingsFormProps.mock.calls.length - 1][0]

    expect(lastCall.indexName).toBe('products')
    expect(lastCall.savedSettings).toEqual(SAMPLE_SETTINGS)
    expect(lastCall.settings).toEqual({
      ...SAMPLE_SETTINGS,
      searchableAttributes: ['name'],
    })
  })
})
