import { beforeEach, describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture'
import { IndexLayout } from '@/components/layout/IndexLayout'
import { useSettings } from '@/hooks/useSettings'

vi.mock('@/hooks/useSettings', () => ({
  useSettings: vi.fn(),
}))

vi.mock('@/hooks/useSystemStatus', () => ({
  useHealthDetail: vi.fn(() => ({
    data: { capabilities: { vectorSearch: true, vectorSearchLocal: true } },
    isLoading: false,
  })),
}))

import { useSettings } from '@/hooks/useSettings'

function renderIndexLayout(initialPath: string) {
  return render(
    <MemoryRouter future={TEST_ROUTER_FUTURE} initialEntries={[initialPath]}>
      <Routes>
        <Route path="/index/:indexName" element={<IndexLayout />}>
          <Route index element={<div>Browse page</div>} />
          <Route path="settings" element={<div>Settings page</div>} />
          <Route path="analytics" element={<div>Analytics page</div>} />
          <Route path="synonyms" element={<div>Synonyms page</div>} />
          <Route path="rules" element={<div>Rules page</div>} />
          <Route path="merchandising" element={<div>Merchandising page</div>} />
        </Route>
      </Routes>
    </MemoryRouter>
  )
}

describe('IndexLayout', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(useSettings).mockReturnValue({
      data: { mode: 'keywordSearch' },
      isLoading: false,
    } as any)
  })

  it('renders per-index tab links for all required pages', () => {
    renderIndexLayout('/index/products')

    expect(screen.getByRole('link', { name: 'Browse' })).toHaveAttribute('href', '/index/products')
    expect(screen.getByRole('link', { name: 'Settings' })).toHaveAttribute('href', '/index/products/settings')
    expect(screen.getByRole('link', { name: 'Analytics' })).toHaveAttribute('href', '/index/products/analytics')
    expect(screen.getByRole('link', { name: 'Synonyms' })).toHaveAttribute('href', '/index/products/synonyms')
    expect(screen.getByRole('link', { name: 'Rules' })).toHaveAttribute('href', '/index/products/rules')
    expect(screen.getByRole('link', { name: 'Merchandising' })).toHaveAttribute('href', '/index/products/merchandising')
  })

  it('highlights the active tab from current location', () => {
    renderIndexLayout('/index/products/analytics')
    expect(screen.getByRole('link', { name: 'Analytics' })).toHaveAttribute('aria-current', 'page')
    expect(screen.getByRole('link', { name: 'Browse' })).not.toHaveAttribute('aria-current', 'page')
  })

  it('navigates between tab routes', async () => {
    const user = userEvent.setup()
    renderIndexLayout('/index/products')

    await user.click(screen.getByRole('link', { name: 'Synonyms' }))
    expect(await screen.findByText('Synonyms page')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Synonyms' })).toHaveAttribute('aria-current', 'page')
  })

  it('shows the Chat tab when index mode is neuralSearch', () => {
    vi.mocked(useSettings).mockReturnValue({
      data: { mode: 'neuralSearch' },
      isLoading: false,
      error: null,
    } as any)

    renderIndexLayout('/index/products')

    expect(screen.getByRole('link', { name: 'Chat' })).toHaveAttribute('href', '/index/products/chat')
  })
})
