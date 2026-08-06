import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { TEST_ROUTER_FUTURE } from '@/test/routerFuture';
import { Chat } from './Chat';

vi.mock('@/hooks/useSettings', () => ({
  useSettings: vi.fn(),
}));

vi.mock('@/hooks/useChat', () => ({
  useChat: vi.fn(),
}));

vi.mock('@/hooks/useSystemStatus', () => ({
  useHealthDetail: vi.fn(),
}));

import { useSettings } from '@/hooks/useSettings';
import { useChat } from '@/hooks/useChat';
import { useHealthDetail } from '@/hooks/useSystemStatus';

function mockSettings(
  mode?: 'keywordSearch' | 'neuralSearch',
  isLoading = false,
  embedders?: Record<string, unknown>,
) {
  vi.mocked(useSettings).mockReturnValue({
    data: mode ? { mode, embedders } : undefined,
    isLoading,
  } as any);
}

function mockChatState(overrides: Partial<ReturnType<typeof useChat>> = {}) {
  vi.mocked(useChat).mockReturnValue({
    messages: [],
    isLoading: false,
    error: null,
    sendMessage: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  });
}

function mockHealth(vectorSearch = true, isLoading = false) {
  vi.mocked(useHealthDetail).mockReturnValue({
    data: {
      capabilities: {
        vectorSearch,
        vectorSearchLocal: vectorSearch,
      },
    },
    isLoading,
  } as any);
}

function renderChat() {
  return render(
    <MemoryRouter future={TEST_ROUTER_FUTURE} initialEntries={['/index/products/chat']}>
      <Routes>
        <Route path="/index/:indexName/chat" element={<Chat />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe('Chat', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockChatState();
    mockHealth(true, false);
  });

  it('shows a loading-safe summary while settings are unresolved', () => {
    mockSettings(undefined, true);

    renderChat();

    expect(screen.getByText(/checking whether chat is available/i)).toBeInTheDocument();
    expect(screen.queryByTestId('chat-input')).not.toBeInTheDocument();
  });

  it('shows compiled-out callout when vector search capability is disabled', () => {
    mockSettings('neuralSearch');
    mockHealth(false);

    renderChat();

    expect(screen.getByTestId('chat-capability-disabled')).toBeInTheDocument();
    expect(screen.getByTestId('chat-capability-disabled')).toHaveTextContent(/not compiled in/i);
    expect(screen.queryByTestId('chat-input')).not.toBeInTheDocument();
    expect(screen.queryByTestId('chat-requires-neural-search')).not.toBeInTheDocument();
  });

  it('stays non-ready when health capability data is unavailable', () => {
    mockSettings('neuralSearch');
    vi.mocked(useHealthDetail).mockReturnValue({
      data: undefined,
      isLoading: false,
    } as any);

    renderChat();

    expect(screen.getByTestId('chat-capability-unavailable')).toBeInTheDocument();
    expect(screen.getByTestId('chat-capability-unavailable')).toHaveTextContent(/waiting for server capability data/i);
    expect(screen.queryByTestId('chat-input')).not.toBeInTheDocument();
  });

  it('shows the NeuralSearch requirement card when mode is not neuralSearch', () => {
    mockSettings('keywordSearch');

    renderChat();

    expect(screen.getByTestId('chat-requires-neural-search')).toBeInTheDocument();
    expect(screen.getByText('Chat requires NeuralSearch mode.')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /settings/i })).toHaveAttribute(
      'href',
      '/index/products/settings',
    );
  });

  it('shows the embedder requirement card when NeuralSearch mode has no embedders', () => {
    mockSettings('neuralSearch');

    renderChat();

    expect(screen.getByTestId('chat-requires-embedder')).toBeInTheDocument();
    expect(screen.getByText(/configure an embedder for this index/i)).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /settings/i })).toHaveAttribute(
      'href',
      '/index/products/settings',
    );
    expect(screen.queryByTestId('chat-input')).not.toBeInTheDocument();
  });

  it('renders the chat interface when NeuralSearch mode is enabled', () => {
    mockSettings('neuralSearch', false, {
      default: { source: 'userProvided', dimensions: 384 },
    });

    renderChat();

    expect(screen.getByTestId('chat-input')).toBeInTheDocument();
    expect(screen.getByTestId('chat-send-button')).toBeInTheDocument();
    expect(screen.getByTestId('chat-message-thread')).toBeInTheDocument();
  });

  it('renders user and assistant messages in the thread', () => {
    mockSettings('neuralSearch', false, {
      default: { source: 'userProvided', dimensions: 384 },
    });
    mockChatState({
      messages: [
        { role: 'user', content: 'headphones' },
        {
          role: 'assistant',
          content: 'Based on your search for "headphones"',
          sources: [{ objectID: 'doc-1', content: 'Wireless headphones' }],
        },
      ],
    });

    renderChat();

    const thread = screen.getByTestId('chat-message-thread');
    expect(thread).toHaveTextContent('headphones');
    expect(thread).toHaveTextContent('Based on your search for "headphones"');
    expect(screen.getByTestId('chat-sources')).toHaveTextContent('doc-1');
  });

  it('does not render arbitrary source object JSON when citation labels are missing', () => {
    mockSettings('neuralSearch', false, {
      default: { source: 'userProvided', dimensions: 384 },
    });
    mockChatState({
      messages: [
        {
          role: 'assistant',
          content: 'Answer with opaque source metadata',
          sources: [{ internalToken: 'secret-token', nested: { apiKey: 'should-not-render' } }],
        },
      ],
    });

    renderChat();

    const sources = screen.getByTestId('chat-sources');
    expect(sources).toHaveTextContent('Source details unavailable');
    expect(sources).not.toHaveTextContent('secret-token');
    expect(sources).not.toHaveTextContent('should-not-render');
  });

  it('renders a content-only source once through the no-objectID label fallback', () => {
    mockSettings('neuralSearch', false, {
      default: { source: 'userProvided', dimensions: 384 },
    });
    const sourceContent = 'Wireless headphones with noise cancellation';
    mockChatState({
      messages: [
        {
          role: 'assistant',
          content: 'Answer with content-only source',
          sources: [{ content: sourceContent }],
        },
      ],
    });

    renderChat();

    const sourceItem = screen.getByTestId('chat-source-item');
    expect(sourceItem).toHaveTextContent(sourceContent);
    expect(sourceItem.textContent?.match(new RegExp(sourceContent, 'g'))).toHaveLength(1);
    expect(sourceItem).not.toHaveTextContent(`${sourceContent}: ${sourceContent}`);
  });
});
