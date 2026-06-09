import type { ReactNode } from 'react'
import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import { BrowserRouter, MemoryRouter } from 'react-router-dom'
import { TEST_ROUTER_FUTURE } from './test/routerFuture'
import App from './App'

interface MockWrapperProps {
  children?: ReactNode
}

function renderApp(initialEntry: string) {
  render(
    <MemoryRouter future={TEST_ROUTER_FUTURE} initialEntries={[initialEntry]}>
      <App />
    </MemoryRouter>
  )
}

vi.mock('./hooks/useTheme', () => ({
  useTheme: vi.fn(),
}))

vi.mock('./hooks/useSettings', () => ({
  useSettings: vi.fn(() => ({
    data: { mode: 'keywordSearch' },
    isLoading: false,
  })),
}))

vi.mock('./hooks/useSystemStatus', () => ({
  useHealthDetail: vi.fn(() => ({
    data: { capabilities: { vectorSearch: true, vectorSearchLocal: true } },
    isLoading: false,
  })),
}))

vi.mock('./components/layout/AuthGate', () => ({
  AuthGate: ({ children }: MockWrapperProps) => <>{children}</>,
}))

vi.mock('./components/ErrorBoundary', () => ({
  ErrorBoundary: ({ children }: MockWrapperProps) => <>{children}</>,
}))

vi.mock('./components/ui/toaster', () => ({
  Toaster: () => null,
}))

vi.mock('./components/layout/Layout', async () => {
  const { Outlet } = await import('react-router-dom')

  return {
    Layout: () => (
      <div data-testid="layout-shell">
        <Outlet />
      </div>
    ),
  }
})

vi.mock('./pages/Overview', () => ({
  Overview: () => <h1>Overview page</h1>,
}))

vi.mock('./pages/SearchBrowse', () => ({
  SearchBrowse: () => <div>Browse page</div>,
}))

vi.mock('./pages/Settings', () => ({
  Settings: () => <div>Settings page</div>,
}))

vi.mock('./pages/Cluster', async () => {
  await new Promise((resolve) => setTimeout(resolve, 0))

  return {
    Cluster: () => <div data-testid="cluster-page-contract">Cluster page contract</div>,
  }
})

function renderDashboardRoute(path: string) {
  window.history.replaceState({}, '', path)

  render(
    <BrowserRouter basename="/dashboard" future={TEST_ROUTER_FUTURE}>
      <App />
    </BrowserRouter>,
  )
}

describe('App', () => {
  it('renders /cluster through the lazy loading shell and shared layout shell', async () => {
    renderApp('/cluster')

    expect(screen.getByTestId('layout-shell')).toBeInTheDocument()
    expect(screen.getByText('Loading...')).toBeInTheDocument()
    expect(await screen.findByTestId('cluster-page-contract')).toBeInTheDocument()
    expect(screen.getByText('Cluster page contract')).toBeInTheDocument()
  })

  it('renders the shared index tab shell for nested per-index settings routes', async () => {
    renderApp('/index/products/settings')

    expect(screen.getByTestId('layout-shell')).toBeInTheDocument()
    expect(screen.getByTestId('index-tab-bar')).toBeInTheDocument()
    expect(await screen.findByText('Settings page')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Browse' })).toHaveAttribute('href', '/index/products')
  })

  it('renders the shared index tab shell for the default browse route', async () => {
    renderApp('/index/products')

    expect(screen.getByTestId('index-tab-bar')).toBeInTheDocument()
    expect(await screen.findByText('Browse page')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Browse' })).toHaveAttribute('aria-current', 'page')
  })

  it('renders the overview route when the production dashboard overview URL has a trailing slash', async () => {
    renderDashboardRoute('/dashboard/overview/')

    expect(await screen.findByRole('heading', { name: 'Overview page' })).toBeInTheDocument()
    expect(screen.queryByText('Page not found')).not.toBeInTheDocument()
  })

  it('renders nested index settings routes when the production dashboard URL has a trailing slash', async () => {
    renderDashboardRoute('/dashboard/index/products/settings/')

    expect(await screen.findByText('Settings page')).toBeInTheDocument()
    expect(screen.getByTestId('index-tab-bar')).toBeInTheDocument()
    expect(screen.queryByText('Page not found')).not.toBeInTheDocument()
  })
})
