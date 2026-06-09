import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture'
import { IndexTabBar } from './IndexTabBar'

vi.mock('@/hooks/useSettings', () => ({
  useSettings: vi.fn(),
}))

vi.mock('@/hooks/useSystemStatus', () => ({
  useHealthDetail: vi.fn(),
}))

import { useSettings } from '@/hooks/useSettings'
import { useHealthDetail } from '@/hooks/useSystemStatus'

function renderTabBar(path: string) {
  return render(
    <MemoryRouter future={TEST_ROUTER_FUTURE} initialEntries={[path]}>
      <Routes>
        <Route path="/index/:indexName/*" element={<IndexTabBar indexName="products" />} />
      </Routes>
    </MemoryRouter>,
  )
}

describe('IndexTabBar', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(useHealthDetail).mockReturnValue({
      data: { capabilities: { vectorSearch: true, vectorSearchLocal: true } },
      isLoading: false,
    } as any)
  })

  it('renders design-doc tab order and hrefs when index mode is neuralSearch', () => {
    vi.mocked(useSettings).mockReturnValue({
      data: { mode: 'neuralSearch' },
      isLoading: false,
    } as any)

    renderTabBar('/index/products')

    const orderedTabs = screen.getAllByRole('link').map((link) => link.textContent)
    expect(orderedTabs).toEqual([
      'Browse',
      'Settings',
      'Analytics',
      'Synonyms',
      'Rules',
      'Merchandising',
      'Recommendations',
      'Chat',
    ])

    expect(screen.getByRole('link', { name: 'Browse' })).toHaveAttribute('href', '/index/products')
    expect(screen.getByRole('link', { name: 'Settings' })).toHaveAttribute('href', '/index/products/settings')
    expect(screen.getByRole('link', { name: 'Analytics' })).toHaveAttribute('href', '/index/products/analytics')
    expect(screen.getByRole('link', { name: 'Synonyms' })).toHaveAttribute('href', '/index/products/synonyms')
    expect(screen.getByRole('link', { name: 'Rules' })).toHaveAttribute('href', '/index/products/rules')
    expect(screen.getByRole('link', { name: 'Merchandising' })).toHaveAttribute('href', '/index/products/merchandising')
    expect(screen.getByRole('link', { name: 'Recommendations' })).toHaveAttribute('href', '/index/products/recommendations')
    expect(screen.getByRole('link', { name: 'Chat' })).toHaveAttribute('href', '/index/products/chat')
  })

  it('hides the Chat tab when settings are loading or keyword mode', () => {
    vi.mocked(useSettings).mockReturnValue({
      data: undefined,
      isLoading: true,
    } as any)

    renderTabBar('/index/products')
    expect(screen.queryByRole('link', { name: 'Chat' })).not.toBeInTheDocument()

    vi.mocked(useSettings).mockReturnValue({
      data: { mode: 'keywordSearch' },
      isLoading: false,
    } as any)

    renderTabBar('/index/products/settings')
    expect(screen.queryByRole('link', { name: 'Chat' })).not.toBeInTheDocument()
  })

  it('hides the Chat tab when vector search is not compiled into the backend', () => {
    vi.mocked(useSettings).mockReturnValue({
      data: { mode: 'neuralSearch' },
      isLoading: false,
    } as any)
    vi.mocked(useHealthDetail).mockReturnValue({
      data: { capabilities: { vectorSearch: false, vectorSearchLocal: false } },
      isLoading: false,
    } as any)

    renderTabBar('/index/products')

    expect(screen.queryByRole('link', { name: 'Chat' })).not.toBeInTheDocument()
  })

  it('uses end matching so Browse is not active on nested child routes', () => {
    vi.mocked(useSettings).mockReturnValue({
      data: { mode: 'neuralSearch' },
      isLoading: false,
    } as any)

    renderTabBar('/index/products/settings/advanced')

    expect(screen.getByRole('link', { name: 'Settings' })).toHaveAttribute('aria-current', 'page')
    expect(screen.getByRole('link', { name: 'Browse' })).not.toHaveAttribute('aria-current')
  })

  it('keeps the matching nested tab active when the route has a trailing slash', () => {
    vi.mocked(useSettings).mockReturnValue({
      data: { mode: 'neuralSearch' },
      isLoading: false,
    } as any)

    renderTabBar('/index/products/settings/')

    expect(screen.getByRole('link', { name: 'Settings' })).toHaveAttribute('aria-current', 'page')
    expect(screen.getByRole('link', { name: 'Browse' })).not.toHaveAttribute('aria-current')
  })

  it('keeps tabs horizontally scrollable without wrapping', () => {
    vi.mocked(useSettings).mockReturnValue({
      data: { mode: 'neuralSearch' },
      isLoading: false,
    } as any)

    renderTabBar('/index/products')

    const scrollContainer = screen.getByTestId('index-tab-bar-scroll')
    expect(scrollContainer).toHaveClass('overflow-x-auto')

    const tabList = screen.getByTestId('index-tab-bar-list')
    expect(tabList).toHaveClass('whitespace-nowrap')
  })
})
