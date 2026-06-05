import { beforeEach, describe, expect, it, vi } from 'vitest'
import { act, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import {
  BrowserRouter,
  MemoryRouter,
  Route,
  Routes,
} from 'react-router-dom'
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { IndexLayout } from '@/components/layout/IndexLayout'
import api from '@/lib/api'
import { Recommendations } from './Recommendations'

const mockUseRecommendations = vi.hoisted(() => vi.fn())
const DEFAULT_RECOMMENDATIONS_PATH = '/index/products/recommendations'
const INDEX_ROUTE_PATH = '/index/:indexName'
const DEFAULT_ITEM_PREVIEW_RESULTS = [
  {
    hits: [{ objectID: 'sku-2', name: 'Phone', _score: 88 }],
    processingTimeMS: 4,
  },
]

vi.mock('@/hooks/useRecommendations', () => ({
  useRecommendations: mockUseRecommendations,
}))

vi.mock('@/hooks/useSettings', () => ({
  useSettings: vi.fn(),
}))

vi.mock('@/hooks/useSystemStatus', () => ({
  useHealthDetail: vi.fn(() => ({ data: undefined })),
}))

import { useSettings } from '@/hooks/useSettings'

type HookResult = {
  mutateAsync: ReturnType<typeof vi.fn>
  isPending: boolean
  error: Error | null
}

type ItemPreviewResults = typeof DEFAULT_ITEM_PREVIEW_RESULTS

function createTestQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  })
}

function renderRecommendationsRoutes(router: React.ReactNode) {
  return render(
    <QueryClientProvider client={createTestQueryClient()}>{router}</QueryClientProvider>,
  )
}

function buildRecommendationsRoutes() {
  return (
    <Routes>
      <Route path={INDEX_ROUTE_PATH} element={<IndexLayout />}>
        <Route path="recommendations" element={<Recommendations />} />
      </Route>
    </Routes>
  )
}

function renderRecommendationsInMemoryRouter(
  path = DEFAULT_RECOMMENDATIONS_PATH,
) {
  return renderRecommendationsRoutes(
    <MemoryRouter future={TEST_ROUTER_FUTURE} initialEntries={[path]}>
      {buildRecommendationsRoutes()}
    </MemoryRouter>,
  )
}

function renderRecommendationsInBrowserRouter(
  path = DEFAULT_RECOMMENDATIONS_PATH,
) {
  window.history.replaceState({}, '', path)

  return renderRecommendationsRoutes(
    <BrowserRouter future={TEST_ROUTER_FUTURE}>
      {buildRecommendationsRoutes()}
    </BrowserRouter>,
  )
}

async function navigateToRecommendations(path: string) {
  await act(async () => {
    window.history.pushState({}, '', path)
    window.dispatchEvent(new PopStateEvent('popstate'))
  })
}

function mockHook(resultOverrides: Partial<HookResult> = {}): HookResult {
  const hookResult: HookResult = {
    mutateAsync: vi.fn().mockResolvedValue([]),
    isPending: false,
    error: null,
    ...resultOverrides,
  }
  mockUseRecommendations.mockReturnValue(hookResult)
  return hookResult
}

function createDeferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error?: unknown) => void
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise
    reject = rejectPromise
  })

  return { promise, resolve, reject }
}

describe('Recommendations page', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(useSettings).mockReturnValue({ data: undefined, isLoading: false } as any)
  })

  it('uses the route indexName and requires objectID for object-based models', async () => {
    const user = userEvent.setup()
    const hook = mockHook()

    renderRecommendationsInMemoryRouter()

    expect(screen.getByTestId('recommendations-index-name')).toHaveTextContent('products')
    expect(screen.getByTestId('recommendations-model-select')).toHaveValue('related-products')
    expect(screen.getByTestId('recommendations-object-input')).toBeRequired()
    expect(screen.getByTestId('get-recommendations-btn')).toBeDisabled()

    await user.type(screen.getByTestId('recommendations-object-input'), '  sku-123  ')
    await user.click(screen.getByTestId('get-recommendations-btn'))

    expect(hook.mutateAsync).toHaveBeenCalledWith({
      indexName: 'products',
      model: 'related-products',
      objectID: 'sku-123',
    })
  })

  it('does not issue live settings requests through the IndexLayout tab shell', async () => {
    const mockedUseSettings = vi.mocked(useSettings)
    const settingsGetSpy = vi.spyOn(api, 'get').mockResolvedValue({ data: {} } as any)

    try {
      renderRecommendationsInMemoryRouter()
      expect(mockedUseSettings).toHaveBeenCalledWith('products')
      expect(settingsGetSpy).not.toHaveBeenCalled()

      mockedUseSettings.mockClear()
      settingsGetSpy.mockClear()
      renderRecommendationsInBrowserRouter('/index/electronics/recommendations')
      expect(mockedUseSettings).toHaveBeenCalledWith('electronics')
      expect(settingsGetSpy).not.toHaveBeenCalled()
    } finally {
      settingsGetSpy.mockRestore()
    }
  })

  it('switches model-specific required inputs and submit enabled state', async () => {
    const user = userEvent.setup()
    mockHook()

    renderRecommendationsInMemoryRouter()

    const modelSelect = screen.getByTestId('recommendations-model-select')
    const submitButton = screen.getByTestId('get-recommendations-btn')

    await user.selectOptions(modelSelect, 'trending-items')
    expect(screen.queryByTestId('recommendations-object-input')).not.toBeInTheDocument()
    expect(screen.queryByTestId('recommendations-facet-input')).not.toBeInTheDocument()
    expect(submitButton).toBeEnabled()

    await user.selectOptions(modelSelect, 'trending-facets')
    expect(screen.getByTestId('recommendations-facet-input')).toBeRequired()
    expect(submitButton).toBeDisabled()

    await user.type(screen.getByTestId('recommendations-facet-input'), 'brand')
    expect(submitButton).toBeEnabled()
  })

  it('renders both item-based and trending-facets result shapes', async () => {
    const user = userEvent.setup()
    const hook = mockHook({
      mutateAsync: vi
        .fn()
        .mockResolvedValueOnce([
          {
            hits: [{ objectID: 'sku-2', name: 'Phone', _score: 88 }],
            processingTimeMS: 4,
          },
        ])
        .mockResolvedValueOnce([
          {
            hits: [{ facetName: 'brand', facetValue: 'Apple', _score: 77 }],
            processingTimeMS: 3,
          },
        ]),
    })

    renderRecommendationsInMemoryRouter()

    await user.type(screen.getByTestId('recommendations-object-input'), 'sku-1')
    await user.click(screen.getByTestId('get-recommendations-btn'))

    await waitFor(() => {
      expect(screen.getByTestId('recommendations-results')).toHaveTextContent('sku-2')
    })

    await user.selectOptions(screen.getByTestId('recommendations-model-select'), 'trending-facets')
    await user.type(screen.getByTestId('recommendations-facet-input'), 'brand')
    await user.click(screen.getByTestId('get-recommendations-btn'))

    await waitFor(() => {
      expect(screen.getByTestId('recommendations-results')).toHaveTextContent('brand')
      expect(screen.getByTestId('recommendations-results')).toHaveTextContent('Apple')
    })

    expect(hook.mutateAsync).toHaveBeenCalledTimes(2)
  })

  it('clears stale results when the selected model changes', async () => {
    const user = userEvent.setup()
    mockHook({
      mutateAsync: vi.fn().mockResolvedValue(DEFAULT_ITEM_PREVIEW_RESULTS),
    })

    renderRecommendationsInMemoryRouter()

    await user.type(screen.getByTestId('recommendations-object-input'), 'sku-1')
    await user.click(screen.getByTestId('get-recommendations-btn'))

    await waitFor(() => {
      expect(screen.getByTestId('recommendations-results')).toHaveTextContent('sku-2')
    })

    await user.selectOptions(screen.getByTestId('recommendations-model-select'), 'trending-facets')

    expect(screen.getByText('Submit a preview request to view recommendations.')).toBeInTheDocument()
    expect(screen.getByTestId('recommendations-results')).not.toHaveTextContent('sku-2')
  })

  it('clears preview state when the index route changes', async () => {
    const user = userEvent.setup()
    mockHook({
      mutateAsync: vi.fn().mockResolvedValue(DEFAULT_ITEM_PREVIEW_RESULTS),
    })

    renderRecommendationsInBrowserRouter()

    await user.type(screen.getByTestId('recommendations-object-input'), 'sku-1')
    await user.click(screen.getByTestId('get-recommendations-btn'))

    await waitFor(() => {
      expect(screen.getByTestId('recommendations-results')).toHaveTextContent('sku-2')
    })

    await navigateToRecommendations('/index/electronics/recommendations')

    expect(screen.getByTestId('recommendations-index-name')).toHaveTextContent('electronics')
    expect(screen.getByText('Submit a preview request to view recommendations.')).toBeInTheDocument()
    expect(screen.getByTestId('recommendations-results')).not.toHaveTextContent('sku-2')
  })

  it('clears submit errors when the selected model changes', async () => {
    const user = userEvent.setup()
    mockHook({
      mutateAsync: vi.fn().mockRejectedValue(new Error('preview failed')),
    })

    renderRecommendationsInMemoryRouter()

    await user.type(screen.getByTestId('recommendations-object-input'), 'sku-1')
    await user.click(screen.getByTestId('get-recommendations-btn'))

    await waitFor(() => {
      expect(screen.getByRole('alert')).toHaveTextContent('preview failed')
    })

    await user.selectOptions(screen.getByTestId('recommendations-model-select'), 'trending-facets')

    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('ignores stale results from a request that settles after the model changes', async () => {
    const user = userEvent.setup()
    const pendingPreview = createDeferred<ItemPreviewResults>()

    mockHook({
      mutateAsync: vi.fn().mockReturnValue(pendingPreview.promise),
    })

    renderRecommendationsInMemoryRouter()

    await user.type(screen.getByTestId('recommendations-object-input'), 'sku-1')
    await user.click(screen.getByTestId('get-recommendations-btn'))
    await user.selectOptions(screen.getByTestId('recommendations-model-select'), 'trending-facets')

    await act(async () => {
      pendingPreview.resolve(DEFAULT_ITEM_PREVIEW_RESULTS)
      await pendingPreview.promise
    })

    expect(screen.getByText('Submit a preview request to view recommendations.')).toBeInTheDocument()
    expect(screen.getByTestId('recommendations-results')).not.toHaveTextContent('sku-2')
  })

  it('ignores stale results from a request that settles after the index route changes', async () => {
    const user = userEvent.setup()
    const pendingPreview = createDeferred<ItemPreviewResults>()

    mockHook({
      mutateAsync: vi.fn().mockReturnValue(pendingPreview.promise),
    })

    renderRecommendationsInBrowserRouter()

    await user.type(screen.getByTestId('recommendations-object-input'), 'sku-1')
    await user.click(screen.getByTestId('get-recommendations-btn'))

    await navigateToRecommendations('/index/electronics/recommendations')

    await act(async () => {
      pendingPreview.resolve(DEFAULT_ITEM_PREVIEW_RESULTS)
      await pendingPreview.promise
    })

    expect(screen.getByTestId('recommendations-index-name')).toHaveTextContent('electronics')
    expect(screen.getByText('Submit a preview request to view recommendations.')).toBeInTheDocument()
    expect(screen.getByTestId('recommendations-results')).not.toHaveTextContent('sku-2')
  })

  it('ignores stale submit errors from a request that settles after the model changes', async () => {
    const user = userEvent.setup()
    const pendingPreview = createDeferred<never>()

    mockHook({
      mutateAsync: vi.fn().mockReturnValue(pendingPreview.promise),
    })

    renderRecommendationsInMemoryRouter()

    await user.type(screen.getByTestId('recommendations-object-input'), 'sku-1')
    await user.click(screen.getByTestId('get-recommendations-btn'))
    await user.selectOptions(screen.getByTestId('recommendations-model-select'), 'trending-facets')

    await act(async () => {
      pendingPreview.reject(new Error('preview failed'))
      try {
        await pendingPreview.promise
      } catch {}
    })

    expect(screen.getByText('Submit a preview request to view recommendations.')).toBeInTheDocument()
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('shows empty-state messaging when preview returns no hits', async () => {
    const user = userEvent.setup()
    mockHook({
      mutateAsync: vi.fn().mockResolvedValue([
        {
          hits: [],
          processingTimeMS: 2,
        },
      ]),
    })

    renderRecommendationsInMemoryRouter()

    await user.selectOptions(screen.getByTestId('recommendations-model-select'), 'trending-items')
    await user.click(screen.getByTestId('get-recommendations-btn'))

    await waitFor(() => {
      expect(screen.getByText('No recommendations found.')).toBeInTheDocument()
    })
  })
})
