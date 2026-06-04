import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { Link, MemoryRouter, Route, Routes } from 'react-router-dom'
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture'
import { IndexLayout } from './IndexLayout'

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

function BrowseChild() {
  return (
    <section>
      <h2>Browse content</h2>
      <Link to="settings">Go to Settings</Link>
    </section>
  )
}

function SettingsChild() {
  return (
    <section>
      <h2>Settings content</h2>
      <Link to="../analytics">Go to Analytics</Link>
    </section>
  )
}

function AnalyticsChild() {
  return <h2>Analytics content</h2>
}

describe('IndexLayout', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(useSettings).mockReturnValue({
      data: { mode: 'neuralSearch' },
      isLoading: false,
    } as any)
  })

  it('renders IndexTabBar above the outlet and keeps it mounted while child routes change', async () => {
    const user = userEvent.setup()

    render(
      <MemoryRouter future={TEST_ROUTER_FUTURE} initialEntries={['/index/products']}>
        <Routes>
          <Route path="/index/:indexName" element={<IndexLayout />}>
            <Route index element={<BrowseChild />} />
            <Route path="settings" element={<SettingsChild />} />
            <Route path="analytics" element={<AnalyticsChild />} />
          </Route>
        </Routes>
      </MemoryRouter>,
    )

    const shellNode = screen.getByTestId('index-tab-bar')
    expect(shellNode).toBeInTheDocument()
    const browseHeading = screen.getByRole('heading', { name: 'Browse content' })
    expect(browseHeading).toBeInTheDocument()
    expect(shellNode.compareDocumentPosition(browseHeading) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0)

    await user.click(screen.getByRole('link', { name: 'Go to Settings' }))
    expect(screen.getByRole('heading', { name: 'Settings content' })).toBeInTheDocument()
    expect(screen.getByTestId('index-tab-bar')).toBe(shellNode)

    await user.click(screen.getByRole('link', { name: 'Go to Analytics' }))
    expect(screen.getByRole('heading', { name: 'Analytics content' })).toBeInTheDocument()
    expect(screen.getByTestId('index-tab-bar')).toBe(shellNode)
  })
})
