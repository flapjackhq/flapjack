import { type FormEvent, type ReactNode, useCallback, useState } from 'react';
import { Link, useParams } from 'react-router-dom';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { useChat } from '@/hooks/useChat';
import { useSettings } from '@/hooks/useSettings';
import { useHealthDetail } from '@/hooks/useSystemStatus';
import type { ChatMessage } from '@/hooks/useChat';

const CHAT_LOADING_COPY =
  'Checking whether Chat is available for this index. Availability depends on NeuralSearch mode and embedder configuration.';

function buildSettingsPath(indexName: string): string {
  return `/index/${encodeURIComponent(indexName)}/settings`;
}

function formatSourceLabel(source: Record<string, unknown>): string {
  const objectId = source.objectID;
  if (typeof objectId === 'string' && objectId.trim().length > 0) {
    return objectId;
  }

  const content = source.content;
  if (typeof content === 'string' && content.trim().length > 0) {
    return content.trim();
  }

  return 'Source details unavailable';
}

function formatSourceContent(source: Record<string, unknown>): string {
  const content = source.content;
  return typeof content === 'string' ? content.trim() : '';
}

function getMessageKey(message: ChatMessage, index: number): string {
  return `${message.role}-${index}-${message.content}`;
}

function ChatPageShell({ children }: { children: ReactNode }) {
  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-bold">Chat</h1>
      {children}
    </div>
  );
}

function ChatAvailabilityCard({
  children,
  testId,
  title,
}: {
  children: ReactNode;
  testId: string;
  title: string;
}) {
  return (
    <Card data-testid={testId}>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">{children}</CardContent>
    </Card>
  );
}

function SettingsLink({ indexName }: { indexName: string }) {
  return (
    <Link
      className="inline-flex text-sm font-medium text-primary underline-offset-4 hover:underline"
      to={buildSettingsPath(indexName)}
    >
      Settings
    </Link>
  );
}

export function Chat() {
  const { indexName = '' } = useParams<{ indexName: string }>();
  const [draftMessage, setDraftMessage] = useState('');
  const { data: settings, isLoading: isSettingsLoading } = useSettings(indexName);
  const { data: health, isLoading: isHealthLoading } = useHealthDetail();
  const { messages, isLoading: isSending, error, sendMessage } = useChat(indexName);
  const hasEmbedders = Object.keys(settings?.embedders ?? {}).length > 0;

  const handleSubmit = useCallback(
    async (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      const trimmedMessage = draftMessage.trim();
      if (!trimmedMessage || isSending) {
        return;
      }

      setDraftMessage('');
      await sendMessage(trimmedMessage);
    },
    [draftMessage, isSending, sendMessage],
  );

  if (isSettingsLoading || isHealthLoading) {
    return (
      <ChatPageShell>
        <p className="text-sm text-muted-foreground">{CHAT_LOADING_COPY}</p>
      </ChatPageShell>
    );
  }

  if (!health) {
    return (
      <ChatPageShell>
        <ChatAvailabilityCard
          testId="chat-capability-unavailable"
          title="Chat availability is temporarily unavailable"
        >
          <p>Waiting for server capability data before enabling Chat.</p>
          <p className="text-sm text-muted-foreground">
            Retry once the health endpoint responds.
          </p>
        </ChatAvailabilityCard>
      </ChatPageShell>
    );
  }

  if (health.capabilities.vectorSearch === false) {
    return (
      <ChatPageShell>
        <ChatAvailabilityCard
          testId="chat-capability-disabled"
          title="Chat unavailable in this server build"
        >
          <p>Vector search is not compiled in for this server build.</p>
          <p className="text-sm text-muted-foreground">
            Use Docker, a macOS release, or a vector-enabled backend build to use Chat.
          </p>
        </ChatAvailabilityCard>
      </ChatPageShell>
    );
  }

  if (settings?.mode !== 'neuralSearch') {
    return (
      <ChatPageShell>
        <ChatAvailabilityCard
          testId="chat-requires-neural-search"
          title="Chat unavailable for current index mode"
        >
          <p>Chat requires NeuralSearch mode.</p>
          <p className="text-sm text-muted-foreground">
            Update this index configuration in settings, then return to chat.
          </p>
          <SettingsLink indexName={indexName} />
        </ChatAvailabilityCard>
      </ChatPageShell>
    );
  }

  if (!hasEmbedders) {
    return (
      <ChatPageShell>
        <ChatAvailabilityCard
          testId="chat-requires-embedder"
          title="Chat requires at least one configured embedder"
        >
          <p>Configure an embedder for this index before starting a chat.</p>
          <p className="text-sm text-muted-foreground">
            Chat relies on vector-enabled retrieval, so NeuralSearch mode alone is not enough.
          </p>
          <SettingsLink indexName={indexName} />
        </ChatAvailabilityCard>
      </ChatPageShell>
    );
  }

  return (
    <ChatPageShell>
      <Card>
        <CardContent className="space-y-4 pt-6">
          <div className="space-y-3" data-testid="chat-message-thread">
            {messages.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                Ask a question to start a retrieval-augmented conversation.
              </p>
            ) : (
              messages.map((message, messageIndex) => (
                <div
                  className="rounded-md border p-3"
                  data-testid={`chat-message-${message.role}`}
                  key={getMessageKey(message, messageIndex)}
                >
                  <p className="mb-1 text-xs uppercase tracking-wide text-muted-foreground">
                    {message.role === 'user' ? 'You' : 'Assistant'}
                  </p>
                  <p className="text-sm whitespace-pre-wrap">{message.content}</p>
                  {message.role === 'assistant' && message.sources?.length ? (
                    <div className="mt-3 space-y-1" data-testid="chat-sources">
                      <p className="text-xs font-medium text-muted-foreground">Sources</p>
                      <ul className="list-inside list-disc text-xs text-muted-foreground">
                        {message.sources.map((source, sourceIndex) => {
                          const sourceLabel = formatSourceLabel(source);
                          const sourceContent = formatSourceContent(source);
                          const shouldRenderSourceContent =
                            sourceContent.length > 0 && sourceContent !== sourceLabel;
                          return (
                            <li
                              data-testid="chat-source-item"
                              key={`source-${messageIndex}-${sourceIndex}`}
                            >
                              <span>{sourceLabel}</span>
                              {shouldRenderSourceContent ? (
                                <span>: {sourceContent}</span>
                              ) : null}
                            </li>
                          );
                        })}
                      </ul>
                    </div>
                  ) : null}
                </div>
              ))
            )}
          </div>

          {error ? (
            <p className="text-sm text-destructive" role="alert">
              {error.message}
            </p>
          ) : null}

          <form className="flex gap-2" onSubmit={handleSubmit}>
            <Input
              data-testid="chat-input"
              onChange={(event) => setDraftMessage(event.target.value)}
              placeholder="Ask a question about this index..."
              value={draftMessage}
            />
            <Button
              data-testid="chat-send-button"
              disabled={isSending || draftMessage.trim().length === 0}
              type="submit"
            >
              {isSending ? 'Sending...' : 'Send'}
            </Button>
          </form>
        </CardContent>
      </Card>
    </ChatPageShell>
  );
}
