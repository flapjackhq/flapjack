import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture';
import { Header } from './Header';
import { Layout } from './Layout';

vi.stubGlobal('__BACKEND_URL__', 'http://127.0.0.1:3000');

let mockTheme = { theme: 'light', toggleTheme: vi.fn() };
let mockAuth = { isAuthenticated: true };
let mockHealth = { data: { status: 'ok' }, isLoading: false };
let mockIndexingStatus = { isIndexing: false, totalPending: 0, activeTasks: [] };
let mockDevMode = { enabled: false, toggle: vi.fn() };

vi.mock('@/hooks/useTheme', () => ({
  useTheme: () => mockTheme,
}));

vi.mock('@/hooks/useAuth', () => ({
  useAuth: () => mockAuth,
}));

vi.mock('@/hooks/useHealth', () => ({
  useHealth: () => mockHealth,
}));

vi.mock('@/hooks/useIndexingStatus', () => ({
  useIndexingStatus: () => mockIndexingStatus,
}));

vi.mock('@/hooks/useDevMode', () => ({
  useDevMode: () => mockDevMode,
}));

vi.mock('./ConnectionDialog', () => ({
  ConnectionDialog: () => null,
}));

vi.mock('./Sidebar', () => ({
  Sidebar: () => <div data-testid="mock-sidebar" />,
}));

vi.mock('./ApiLogger', () => ({
  ApiLogger: () => <div data-testid="mock-api-logger" />,
}));

vi.mock('./DevModePanel', () => ({
  DevModePanel: () => <div data-testid="mock-dev-panel" />,
}));

describe('Header', () => {
  function renderHeader(onMenuToggle = vi.fn()) {
    render(
      <MemoryRouter future={TEST_ROUTER_FUTURE}>
        <Header onMenuToggle={onMenuToggle} />
      </MemoryRouter>,
    );

    return { onMenuToggle };
  }

  function renderLayout() {
    render(
      <MemoryRouter future={TEST_ROUTER_FUTURE} initialEntries={['/overview']}>
        <Routes>
          <Route element={<Layout />}>
            <Route path="/overview" element={<div data-testid="mock-route">Overview</div>} />
          </Route>
        </Routes>
      </MemoryRouter>,
    );
  }

  beforeEach(() => {
    mockTheme = { theme: 'light', toggleTheme: vi.fn() };
    mockAuth = { isAuthenticated: true };
    mockHealth = { data: { status: 'ok' }, isLoading: false };
    mockIndexingStatus = { isIndexing: false, totalPending: 0, activeTasks: [] };
    mockDevMode = { enabled: false, toggle: vi.fn() };
  });

  it('keeps the narrow-width task path reachable while deferring low-priority chrome', async () => {
    const user = userEvent.setup();
    const { onMenuToggle } = renderHeader();

    const header = screen.getByTestId('app-shell-header');
    expect(header).toHaveClass('min-w-0');

    const primaryRegion = screen.getByTestId('app-shell-header-primary');
    expect(primaryRegion).toHaveClass('min-w-0', 'flex-1');

    const menuButton = screen.getByRole('button', { name: 'Toggle navigation' });
    expect(menuButton).toBeVisible();
    await user.click(menuButton);
    expect(onMenuToggle).toHaveBeenCalledOnce();

    expect(screen.getByTestId('app-shell-connection-status')).toHaveTextContent('Connected');
    expect(screen.getByRole('button', { name: 'Indexing queue' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'Connection Settings' })).toBeVisible();

    const actionRegion = screen.getByTestId('app-shell-header-actions');
    expect(actionRegion).toHaveClass('shrink-0');
    expect(screen.getByRole('link', { name: 'API Docs' })).toHaveClass('hidden', 'md:block');
    expect(screen.getByRole('button', { name: 'Toggle dev mode' })).toHaveClass('hidden', 'md:inline-flex');
    expect(screen.getByRole('button', { name: 'Toggle theme' })).toHaveClass('hidden', 'md:inline-flex');
    expect(screen.getByText('Beta')).toHaveClass('hidden', 'sm:inline-flex');
  });

  it('shows Connecting while the health request is pending without hiding the task path', async () => {
    const user = userEvent.setup();
    mockHealth = { data: undefined, isLoading: true };
    const { onMenuToggle } = renderHeader();

    expect(screen.getByTestId('app-shell-connection-status')).toHaveTextContent('Connecting...');
    expect(screen.getByRole('button', { name: 'Indexing queue' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'Connection Settings' })).toBeVisible();

    await user.click(screen.getByRole('button', { name: 'Toggle navigation' }));
    expect(onMenuToggle).toHaveBeenCalledOnce();
  });

  it('surfaces the unauthenticated state without changing the narrow-width action contract', () => {
    mockAuth = { isAuthenticated: false };
    renderHeader();

    expect(screen.getByTestId('app-shell-connection-status')).toHaveTextContent('Not Authenticated');
    expect(screen.getByRole('button', { name: 'Indexing queue' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'Connection Settings' })).toBeVisible();
  });

  it('wraps the disconnected banner inside the app shell without dropping adjacent regions', () => {
    mockHealth = { data: { status: 'degraded' }, isLoading: false };
    renderLayout();

    const banner = screen.getByTestId('disconnected-banner');
    expect(banner).toHaveClass('min-w-0', 'flex-wrap');
    expect(banner).toHaveTextContent('Server disconnected');
    expect(banner.querySelector('span')).toHaveClass('min-w-0', 'break-words');
    expect(screen.getByTestId('app-shell-header')).toBeVisible();
    expect(screen.getByTestId('mock-route')).toBeVisible();
    expect(screen.getByTestId('mock-api-logger')).toBeVisible();
  });
});
