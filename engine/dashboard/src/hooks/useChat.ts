/**
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import api from '@/lib/api';
import type { ChatRequest, ChatResponse } from '@/lib/types';

export interface ChatMessage {
  role: 'user' | 'assistant';
  content: string;
  sources?: Array<Record<string, unknown>>;
}

interface UseChatResult {
  messages: ChatMessage[];
  isLoading: boolean;
  error: Error | null;
  sendMessage: (query: string) => Promise<void>;
}

/**
 * TODO: Document useChat.
 */
export function useChat(indexName: string): UseChatResult {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const conversationIdRef = useRef<string | undefined>(undefined);
  const chatSessionRef = useRef(0);

  useEffect(() => {
    chatSessionRef.current += 1;
    conversationIdRef.current = undefined;
    setMessages([]);
    setIsLoading(false);
    setError(null);
  }, [indexName]);

  const sendMessage = useCallback(async (query: string) => {
    const chatSessionId = chatSessionRef.current;

    setError(null);
    setMessages((previousMessages) => [
      ...previousMessages,
      { role: 'user', content: query },
    ]);
    setIsLoading(true);

    try {
      const payload: ChatRequest = {
        query,
        conversationId: conversationIdRef.current,
      };
      const chatPath = `/1/indexes/${encodeURIComponent(indexName)}/chat`;
      const response = await api.post<ChatResponse>(
        chatPath,
        payload,
      );

      if (chatSessionRef.current !== chatSessionId) {
        return;
      }

      conversationIdRef.current = response.data.conversationId;
      setMessages((previousMessages) => [
        ...previousMessages,
        {
          role: 'assistant',
          content: response.data.answer,
          sources: response.data.sources,
        },
      ]);
    } catch (caughtError) {
      if (chatSessionRef.current !== chatSessionId) {
        return;
      }

      if (caughtError instanceof Error) {
        setError(caughtError);
      } else {
        setError(new Error('Failed to send chat message.'));
      }
    } finally {
      if (chatSessionRef.current === chatSessionId) {
        setIsLoading(false);
      }
    }
  }, [indexName]);

  return {
    messages,
    isLoading,
    error,
    sendMessage,
  };
}
