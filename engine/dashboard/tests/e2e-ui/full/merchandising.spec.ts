import type { APIRequestContext, Locator, Page } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import { deleteIndex, getRules } from '../../fixtures/api-helpers';
import { readVisibleObjectId, responseMatchesIndexQuery } from '../result-helpers';
import { createIsolatedMerchandisingLifecycleScenario } from '../scenario-helpers';
import { cleanupRulesByDescriptionPrefix, isRecord } from '../rule-cleanup-helpers';

interface MerchRuleSnapshot {
  objectID: string;
  description: string;
  conditionPattern: string;
  conditionAnchoring: string;
  promoted: Array<{ objectID: string; position: number }>;
  hidden: string[];
}

interface MerchLifecycleResult {
  pinnedObjectID: string;
  hiddenObjectID: string;
  expectedPinnedPosition: number;
  expectedVisibleOrder: string[];
}

function createUniqueRuleDescriptionPrefix(label: string): string {
  const slug = label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
  return `e2e-merch-rule-${slug}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

function readMerchRuleSnapshot(value: unknown): MerchRuleSnapshot | null {
  if (!isRecord(value)) {
    return null;
  }

  const objectID = value.objectID;
  const description = value.description;
  if (typeof objectID !== 'string' || typeof description !== 'string') {
    return null;
  }

  const conditions = Array.isArray(value.conditions) ? value.conditions : [];
  const firstCondition = conditions.find((condition) => isRecord(condition));
  if (!firstCondition || typeof firstCondition.pattern !== 'string' || typeof firstCondition.anchoring !== 'string') {
    return null;
  }

  const consequence = isRecord(value.consequence) ? value.consequence : {};
  const promote = Array.isArray(consequence.promote) ? consequence.promote : [];
  const hide = Array.isArray(consequence.hide) ? consequence.hide : [];

  const promoted: Array<{ objectID: string; position: number }> = [];
  for (const candidate of promote) {
    if (!isRecord(candidate) || typeof candidate.objectID !== 'string' || typeof candidate.position !== 'number') {
      return null;
    }
    promoted.push({ objectID: candidate.objectID, position: candidate.position });
  }

  const hidden: string[] = [];
  for (const candidate of hide) {
    if (!isRecord(candidate) || typeof candidate.objectID !== 'string') {
      return null;
    }
    hidden.push(candidate.objectID);
  }

  return {
    objectID,
    description,
    conditionPattern: firstCondition.pattern,
    conditionAnchoring: firstCondition.anchoring,
    promoted,
    hidden,
  };
}

function getMerchCardByObjectID(cards: Locator, objectID: string): Locator {
  return cards.filter({ hasText: objectID }).first();
}

async function waitForMerchSearch(page: Page, indexName: string, query: string): Promise<void> {
  const searchInput = page.getByTestId('merch-search-input');
  const searchResponsePromise = page.waitForResponse(
    (response) => responseMatchesIndexQuery(response, indexName, query),
    { timeout: 15_000 },
  );

  await searchInput.fill(query);
  await page.getByRole('button', { name: /^search$/i }).click();
  await searchResponsePromise;
}

async function readVisibleObjectIDs(cards: Locator): Promise<string[]> {
  const count = await cards.count();
  const objectIDs: string[] = [];
  for (let index = 0; index < count; index += 1) {
    objectIDs.push(await readVisibleObjectId(cards.nth(index)));
  }
  return objectIDs;
}

async function applyLifecycleEdits(page: Page, baselineVisibleOrder: string[]): Promise<MerchLifecycleResult> {
  const cards = page.getByTestId('merch-card');
  const pinnedObjectID = baselineVisibleOrder[1];
  const hiddenObjectID = baselineVisibleOrder[2];

  await getMerchCardByObjectID(cards, pinnedObjectID).getByTitle('Pin to this position').click();
  await getMerchCardByObjectID(cards, hiddenObjectID).getByTitle('Hide from results').click();
  await getMerchCardByObjectID(cards, pinnedObjectID).getByTitle('Move down').click();

  const expectedPinnedPosition = 2;
  const expectedVisibleOrder = baselineVisibleOrder.filter((objectID) => objectID !== hiddenObjectID && objectID !== pinnedObjectID);
  expectedVisibleOrder.splice(Math.min(expectedPinnedPosition, expectedVisibleOrder.length), 0, pinnedObjectID);

  return {
    pinnedObjectID,
    hiddenObjectID,
    expectedPinnedPosition,
    expectedVisibleOrder,
  };
}

async function findRuleByDescriptionPrefix(
  request: APIRequestContext,
  indexName: string,
  descriptionPrefix: string,
): Promise<MerchRuleSnapshot | null> {
  const response = await getRules(request, indexName);
  if (!response.ok) {
    return null;
  }

  for (const candidate of response.items) {
    const rule = readMerchRuleSnapshot(candidate);
    if (rule && rule.description.startsWith(descriptionPrefix)) {
      return rule;
    }
  }

  return null;
}

async function requireRuleByDescriptionPrefix(
  request: APIRequestContext,
  indexName: string,
  descriptionPrefix: string,
): Promise<MerchRuleSnapshot> {
  const persistedRule = await findRuleByDescriptionPrefix(request, indexName, descriptionPrefix);
  if (!persistedRule) {
    throw new Error(`Expected a persisted rule with description prefix "${descriptionPrefix}".`);
  }
  return persistedRule;
}

test.describe('Merchandising Studio', () => {
  test('deterministic lifecycle preview supports pin/hide/move and reset restores baseline order', async ({ page, request }) => {
    const lifecycleFixture = await createIsolatedMerchandisingLifecycleScenario(request, 'preview-lifecycle');

    try {
      await page.goto(`/index/${lifecycleFixture.indexName}/merchandising`);
      await expect(page.getByRole('heading', { name: /merchandising studio/i })).toBeVisible({ timeout: 15_000 });

      await waitForMerchSearch(page, lifecycleFixture.indexName, lifecycleFixture.searchQuery);

      const cards = page.getByTestId('merch-card');
      await expect(cards.first()).toBeVisible({ timeout: 10_000 });

      const baselineVisibleOrder = await readVisibleObjectIDs(cards);
      expect([...baselineVisibleOrder].sort()).toEqual([...lifecycleFixture.expectedObjectIDs].sort());

      const lifecycleResult = await applyLifecycleEdits(page, baselineVisibleOrder);

      await expect.poll(() => readVisibleObjectIDs(cards), { timeout: 10_000 }).toEqual(lifecycleResult.expectedVisibleOrder);
      await expect(page.getByRole('heading', { name: /hidden results \(1\)/i })).toBeVisible({ timeout: 10_000 });
      await expect(page.getByText(lifecycleResult.hiddenObjectID).first()).toBeVisible({ timeout: 10_000 });

      await page.getByRole('button', { name: /^reset$/i }).click();

      await expect.poll(() => readVisibleObjectIDs(cards), { timeout: 10_000 }).toEqual(baselineVisibleOrder);
      await expect(page.getByRole('heading', { name: /hidden results/i })).toHaveCount(0);
      await expect(page.getByText(/results for/i).first()).toBeVisible();
      await expect(page.getByText(lifecycleFixture.searchQuery).first()).toBeVisible();
    } finally {
      await deleteIndex(request, lifecycleFixture.indexName);
    }
  });

  test('save as rule persists expected condition/promote/hide payload and the spec cleans it up', async ({ page, request }) => {
    const lifecycleFixture = await createIsolatedMerchandisingLifecycleScenario(request, 'save-rule-lifecycle');
    const descriptionPrefix = createUniqueRuleDescriptionPrefix('stage-4-save');

    try {
      await page.goto(`/index/${lifecycleFixture.indexName}/merchandising`);
      await expect(page.getByRole('heading', { name: /merchandising studio/i })).toBeVisible({ timeout: 15_000 });

      await waitForMerchSearch(page, lifecycleFixture.indexName, lifecycleFixture.searchQuery);

      const cards = page.getByTestId('merch-card');
      await expect(cards.first()).toBeVisible({ timeout: 10_000 });
      const baselineVisibleOrder = await readVisibleObjectIDs(cards);
      expect([...baselineVisibleOrder].sort()).toEqual([...lifecycleFixture.expectedObjectIDs].sort());

      const lifecycleResult = await applyLifecycleEdits(page, baselineVisibleOrder);

      const fullDescription = `${descriptionPrefix} deterministic merchandising rule`;
      await page.getByPlaceholder(`Merchandising: "${lifecycleFixture.searchQuery}"`).fill(fullDescription);

      const saveRuleResponse = page.waitForResponse(
        (response) =>
          response.request().method() === 'PUT' &&
          response.url().includes(`/indexes/${lifecycleFixture.indexName}/rules/`) &&
          [200, 202].includes(response.status()),
        { timeout: 15_000 },
      );
      await page.getByRole('button', { name: /save as rule/i }).click();
      await saveRuleResponse;

      await page.goto(`/index/${lifecycleFixture.indexName}/rules`);
      await expect(page.getByRole('heading', { name: /^rules$/i })).toBeVisible({ timeout: 15_000 });

      const savedRuleCard = page.getByTestId('rule-card').filter({ hasText: descriptionPrefix }).first();
      await expect(savedRuleCard).toBeVisible({ timeout: 10_000 });
      await expect(savedRuleCard).toContainText(lifecycleFixture.searchQuery);
      await expect(savedRuleCard).toContainText('1 pinned');
      await expect(savedRuleCard).toContainText('1 hidden');

      await expect.poll(async () => {
        return (await findRuleByDescriptionPrefix(request, lifecycleFixture.indexName, descriptionPrefix))?.objectID ?? '';
      }, { timeout: 15_000 }).not.toBe('');

      const persistedRule = await requireRuleByDescriptionPrefix(request, lifecycleFixture.indexName, descriptionPrefix);

      expect(persistedRule.description).toContain(descriptionPrefix);
      expect(persistedRule.conditionPattern).toBe(lifecycleFixture.searchQuery);
      expect(persistedRule.conditionAnchoring).toBe('is');
      expect(persistedRule.promoted).toEqual([
        { objectID: lifecycleResult.pinnedObjectID, position: lifecycleResult.expectedPinnedPosition },
      ]);
      expect(persistedRule.hidden).toEqual([lifecycleResult.hiddenObjectID]);
    } finally {
      await cleanupRulesByDescriptionPrefix(request, lifecycleFixture.indexName, descriptionPrefix);
      await deleteIndex(request, lifecycleFixture.indexName);
    }
  });
});
