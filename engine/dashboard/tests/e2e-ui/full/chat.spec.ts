import type { APIRequestContext, Page } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import type { ChatRequest } from '../../../src/lib/types';
import {
  createIndex,
  deleteIndex,
  skipWhenVectorSearchDisabled,
} from '../../fixtures/api-helpers';
import {
  setChatReadySettings,
  setChatSearchMode,
  addDocumentsAndWaitForSearchable,
} from '../../fixtures/chat-api-helpers';

const CHAT_INDEX = `e2e-chat-${Date.now()}`;
const CHAT_FIXTURE_QUERY = 'chatfixturechatrag';
const CHAT_DOCUMENTS = [
  {
    objectID: 'chat-doc-1',
    content: `Wireless headphones with noise cancellation ${CHAT_FIXTURE_QUERY}`,
  },
  {
    objectID: 'chat-doc-2',
    content: `Bluetooth speaker with 20-hour battery life ${CHAT_FIXTURE_QUERY}`,
  },
];
const CHAT_DOCUMENT_READINESS = {
  query: CHAT_FIXTURE_QUERY,
  expectedMinimumHits: CHAT_DOCUMENTS.length,
};
const FIRST_QUERY: ChatRequest['query'] = 'headphones';
const SECOND_QUERY: ChatRequest['query'] = 'speaker';
const EMPTY_INDEX_QUERY: ChatRequest['query'] = 'anything';
const VECTOR_ENABLED_MESSAGE = 'Chat/RAG e2e flows require a vector-search-enabled build';

async function prepareChatIndex(
  request: APIRequestContext,
  indexName: string = CHAT_INDEX,
) {
  await setChatReadySettings(request, indexName);
  await addDocumentsAndWaitForSearchable(
    request,
    indexName,
    CHAT_DOCUMENTS,
    CHAT_DOCUMENT_READINESS,
  );
}

function buildChatUrl(indexName: string = CHAT_INDEX): string {
  return `/index/${indexName}/chat`;
}

async function submitChatQuery(page: Page, query: ChatRequest['query']) {
  await page.getByTestId('chat-input').fill(query);
  await page.getByTestId('chat-send-button').click();
}

function getChatDocument(objectID: string) {
  const document = CHAT_DOCUMENTS.find((candidate) => candidate.objectID === objectID);
  if (!document) {
    throw new Error(`Missing CHAT_DOCUMENTS fixture for ${objectID}`);
  }
  return document;
}

async function expectAssistantMessageWithSource(
  thread: ReturnType<Page['getByTestId']>,
  messageIndex: number,
  query: ChatRequest['query'],
  objectID: string,
) {
  const document = getChatDocument(objectID);
  const assistantMessage = thread.getByTestId('chat-message-assistant').nth(messageIndex);
  await expect(assistantMessage).toContainText(`Based on your search for "${query}"`, {
    timeout: 15_000,
  });
  await expect(assistantMessage).toContainText(document.content);

  const sourceItem = assistantMessage.getByTestId('chat-source-item').filter({
    hasText: document.objectID,
  });
  await expect(sourceItem).toBeVisible();
  await expect(sourceItem).toContainText(document.content);
}

test.describe('Chat / RAG', () => {
  test.describe.configure({ mode: 'serial' });

  test.beforeAll(async ({ request }) => {
    await deleteIndex(request, CHAT_INDEX);
    await createIndex(request, CHAT_INDEX);
  });

  test.afterAll(async ({ request }) => {
    await deleteIndex(request, CHAT_INDEX);
  });

  test('shows capability-disabled callout when vector search is compiled out', async ({ page, request }) => {
    await setChatSearchMode(request, CHAT_INDEX, 'neuralSearch');

    await page.route('**/health', async (route) => {
      await route.fulfill({
        status: 200,
        json: {
          status: 'ok',
          capabilities: { vectorSearch: false, vectorSearchLocal: false },
        },
      });
    });

    await page.goto(buildChatUrl());

    await expect(page.getByTestId('chat-capability-disabled')).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.getByTestId('chat-capability-disabled')).toContainText(
      'not compiled in',
    );
    await expect(page.getByTestId('chat-input')).toBeHidden();
    await expect(page.getByTestId('chat-requires-neural-search')).toBeHidden();
  });

  test.describe('vector-enabled chat flows', () => {
    test.beforeEach(async ({ request }, testInfo) => {
      await skipWhenVectorSearchDisabled(request, testInfo, VECTOR_ENABLED_MESSAGE);
    });

    test('shows setup prompt when not in NeuralSearch mode', async ({ page, request }) => {
      // Arrange: ensure the isolated chat index is NOT in neuralSearch mode.
      await setChatSearchMode(request, CHAT_INDEX, 'keywordSearch');

      // Act: navigate directly (tab hidden outside neuralSearch, but route is accessible)
      await page.goto(buildChatUrl());

      // Assert: mode-gating callout is visible with settings navigation
      const requiresNeuralSearch = page.getByTestId('chat-requires-neural-search');
      await expect(requiresNeuralSearch).toBeVisible({ timeout: 15_000 });
      await expect(requiresNeuralSearch.getByText('Chat requires NeuralSearch mode.')).toBeVisible();
      await expect(requiresNeuralSearch.getByRole('link', { name: 'Settings' })).toBeVisible();
    });

    test('shows embedder setup prompt when NeuralSearch mode has no embedders', async ({ page, request }) => {
      await setChatSearchMode(request, CHAT_INDEX, 'neuralSearch');

      await page.goto(buildChatUrl());

      const requiresEmbedder = page.getByTestId('chat-requires-embedder');
      await expect(requiresEmbedder).toBeVisible({ timeout: 15_000 });
      await expect(requiresEmbedder).toContainText(
        'Configure an embedder for this index before starting a chat.',
      );
      await expect(requiresEmbedder.getByRole('link', { name: 'Settings' })).toBeVisible();
      await expect(page.getByTestId('chat-input')).toBeHidden();
    });

    test('shows chat interface when NeuralSearch is enabled with documents', async ({ page, request }) => {
      // Arrange: enable stub provider + seed chat-specific documents in an isolated index.
      await prepareChatIndex(request);

      // Act
      await page.goto(buildChatUrl());

      await expect(page.getByTestId('chat-input')).toBeVisible({ timeout: 15_000 });

      // Assert: chat UI elements visible, placeholder shell gone
      await expect(page.getByTestId('chat-send-button')).toBeVisible();
      await expect(page.getByTestId('placeholder-page-chat')).toBeHidden();
    });

    test('sends query, displays answer with sources, and supports multi-turn', async ({ page, request }) => {
      // Arrange: ensure NeuralSearch + docs are in place on the isolated chat index.
      await prepareChatIndex(request);
      await page.goto(buildChatUrl());

      await expect(page.getByTestId('chat-input')).toBeVisible({ timeout: 15_000 });

      // Act: send first query
      await submitChatQuery(page, FIRST_QUERY);

      // Assert: user message visible in thread
      const thread = page.getByTestId('chat-message-thread');
      await expect(
        thread.getByText(new RegExp(`^${FIRST_QUERY}$`)),
      ).toBeVisible({ timeout: 15_000 });

      await expectAssistantMessageWithSource(thread, 0, FIRST_QUERY, 'chat-doc-1');

      // Act: send second message in same conversation (multi-turn)
      await submitChatQuery(page, SECOND_QUERY);

      // Assert: both exchanges visible in thread
      await expect(thread.getByText(new RegExp(`^${FIRST_QUERY}$`))).toBeVisible();
      await expect(
        thread.getByText(new RegExp(`^${SECOND_QUERY}$`)),
      ).toBeVisible({ timeout: 15_000 });

      await expect(thread.getByTestId('chat-message-assistant')).toHaveCount(2, {
        timeout: 15_000,
      });
      await expectAssistantMessageWithSource(thread, 1, SECOND_QUERY, 'chat-doc-2');
    });

    test('shows no-results message on an empty index', async ({ page, request }) => {
      // Arrange: create a dedicated empty index with stub provider, no documents
      const emptyIndex = `e2e-chat-empty-${Date.now()}`;
      const emptyChatUrl = `/index/${emptyIndex}/chat`;

      try {
        await deleteIndex(request, emptyIndex);
        await createIndex(request, emptyIndex);
        await setChatReadySettings(request, emptyIndex);

        // Act: navigate and send query against empty index
        await page.goto(emptyChatUrl);
        await expect(page.getByTestId('chat-input')).toBeVisible({ timeout: 15_000 });
        await submitChatQuery(page, EMPTY_INDEX_QUERY);

        // Assert: stub returns "No relevant results found for: anything"
        await expect(
          page.getByTestId('chat-message-thread').getByText('No relevant results found'),
        ).toBeVisible({ timeout: 15_000 });
      } finally {
        await deleteIndex(request, emptyIndex);
      }
    });
  });
});
