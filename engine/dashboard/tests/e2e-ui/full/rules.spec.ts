import type { APIRequestContext, Locator } from '@playwright/test';
import { test, expect } from '../../fixtures/auth.fixture';
import { TEST_INDEX } from '../helpers';
import { createRule, getRules } from '../../fixtures/api-helpers';
import { cleanupRulesByPrefix, isRecord } from '../rule-cleanup-helpers';

const RULES_URL = `/index/${TEST_INDEX}/rules`;
const SEEDED_RULE_IDS = ['rule-pin-macbook', 'rule-hide-galaxy-tab'] as const;

function createUniqueRulePrefix(label: string): string {
  const slug = label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
  return `e2e-rules-${slug}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

function readObjectId(value: unknown): string | null {
  if (!isRecord(value)) {
    return null;
  }
  const objectID = value.objectID;
  return typeof objectID === 'string' ? objectID : null;
}

function readPromotedObjectId(rule: Record<string, unknown>): string {
  const consequence = rule.consequence;
  if (!isRecord(consequence)) {
    return '';
  }

  const promote = consequence.promote;
  if (!Array.isArray(promote) || promote.length === 0 || !isRecord(promote[0])) {
    return '';
  }

  const promotedObjectID = promote[0].objectID;
  return typeof promotedObjectID === 'string' ? promotedObjectID : '';
}

function readFirstValidityRange(rule: Record<string, unknown>): { from: number | null; until: number | null } {
  const validity = rule.validity;
  if (!Array.isArray(validity) || validity.length === 0 || !isRecord(validity[0])) {
    return { from: null, until: null };
  }

  const from = validity[0].from;
  const until = validity[0].until;
  return {
    from: typeof from === 'number' ? from : null,
    until: typeof until === 'number' ? until : null,
  };
}

function dateTimeLocalToUnix(value: string): number {
  return Math.floor(new Date(value).getTime() / 1000);
}

function getRuleCardByObjectId(rulesList: Locator, objectID: string): Locator {
  return rulesList.getByTestId('rule-card').filter({ hasText: objectID }).first();
}

async function readRuleIdsFromApi(request: APIRequestContext, indexName: string): Promise<string[]> {
  const response = await getRules(request, indexName);
  if (!response.ok) {
    return [];
  }

  return response.items
    .map((item) => readObjectId(item))
    .filter((objectID): objectID is string => Boolean(objectID));
}

async function readRuleFromApi(
  request: APIRequestContext,
  indexName: string,
  objectID: string,
): Promise<Record<string, unknown> | null> {
  const response = await getRules(request, indexName);
  if (!response.ok) {
    return null;
  }

  for (const item of response.items) {
    if (!isRecord(item) || item.objectID !== objectID) {
      continue;
    }
    return item;
  }

  return null;
}

test.describe('Rules', () => {
  test.describe.configure({ mode: 'serial' });
  test.use({ viewport: { width: 1280, height: 1500 } });

  test.beforeEach(async ({ page }) => {
    await page.goto(RULES_URL);
    await expect(page.getByText('Rules').first()).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('rules-list')).toBeVisible({ timeout: 10_000 });
  });

  test('list shows seeded rules', async ({ page }) => {
    const rulesList = page.getByTestId('rules-list');

    for (const seededRuleId of SEEDED_RULE_IDS) {
      await expect(getRuleCardByObjectId(rulesList, seededRuleId)).toBeVisible();
    }
  });

  test('form mode create/read/delete is deterministic and restores seeded baseline', async ({ page, request }) => {
    test.setTimeout(60_000);

    const rulePrefix = createUniqueRulePrefix('form-crud');
    const ruleObjectID = `${rulePrefix}-rule`;
    const ruleDescription = `Stage 3 form CRUD ${rulePrefix}`;
    const conditionPattern = `${rulePrefix}-pattern`;
    const promotedObjectID = `${rulePrefix}-promoted`;

    await cleanupRulesByPrefix(request, TEST_INDEX, rulePrefix);
    const baselineRuleIds = (await readRuleIdsFromApi(request, TEST_INDEX)).sort();

    expect(baselineRuleIds).toEqual(expect.arrayContaining([...SEEDED_RULE_IDS]));

    const rulesList = page.getByTestId('rules-list');

    try {
      await page.getByRole('button', { name: /add rule/i }).click();
      const createDialog = page.getByRole('dialog');
      await expect(createDialog).toBeVisible({ timeout: 10_000 });

      await createDialog.getByLabel('Object ID', { exact: true }).fill(ruleObjectID);
      await createDialog.getByLabel('Description', { exact: true }).fill(ruleDescription);
      await createDialog.getByLabel('Pattern 1', { exact: true }).fill(conditionPattern);
      await createDialog.getByLabel('Anchoring 1', { exact: true }).selectOption('is');

      const addPromotedItemButton = createDialog.getByRole('button', { name: /add promoted item/i });
      await addPromotedItemButton.scrollIntoViewIfNeeded();
      await addPromotedItemButton.click();
      await createDialog.getByLabel('Promote Object ID 1', { exact: true }).fill(promotedObjectID);
      await createDialog.getByLabel('Promote Position 1', { exact: true }).fill('0');

      await createDialog.getByRole('button', { name: /^create$/i }).click();
      await expect(createDialog).not.toBeVisible({ timeout: 10_000 });

      const createdRuleCard = getRuleCardByObjectId(rulesList, ruleObjectID);
      await expect(createdRuleCard).toBeVisible({ timeout: 10_000 });
      await expect(createdRuleCard).toContainText(ruleDescription);
      await expect(createdRuleCard).toContainText('pin 1 result');

      await expect
        .poll(async () => Boolean(await readRuleFromApi(request, TEST_INDEX, ruleObjectID)), { timeout: 10_000 })
        .toBe(true);

      await createdRuleCard.getByRole('button', { name: /edit/i }).click();
      const editDialog = page.getByRole('dialog');
      await expect(editDialog).toBeVisible({ timeout: 10_000 });

      await expect(editDialog.getByLabel('Object ID', { exact: true })).toHaveValue(ruleObjectID);
      await expect(editDialog.getByLabel('Description', { exact: true })).toHaveValue(ruleDescription);
      await expect(editDialog.getByLabel('Pattern 1', { exact: true })).toHaveValue(conditionPattern);
      await expect(editDialog.getByLabel('Promote Object ID 1', { exact: true })).toHaveValue(promotedObjectID);

      await editDialog.getByRole('button', { name: /cancel/i }).click();
      await expect(editDialog).not.toBeVisible({ timeout: 10_000 });

      const deleteTargetCard = getRuleCardByObjectId(rulesList, ruleObjectID);
      await deleteTargetCard.getByRole('button', { name: /delete/i }).click();

      const confirmDialog = page.getByRole('dialog');
      await expect(confirmDialog).toBeVisible({ timeout: 10_000 });
      await confirmDialog.getByRole('button', { name: 'Delete' }).click();
      await expect(confirmDialog).not.toBeVisible({ timeout: 10_000 });

      await expect(getRuleCardByObjectId(rulesList, ruleObjectID)).toHaveCount(0, { timeout: 10_000 });

      await expect
        .poll(async () => Boolean(await readRuleFromApi(request, TEST_INDEX, ruleObjectID)), { timeout: 10_000 })
        .toBe(false);

      await page.reload();
      await expect(page.getByText('Rules').first()).toBeVisible({ timeout: 15_000 });
      const restoredRuleIds = (await readRuleIdsFromApi(request, TEST_INDEX)).sort();
      expect(restoredRuleIds).toEqual(baselineRuleIds);

      for (const seededRuleId of SEEDED_RULE_IDS) {
        await expect(getRuleCardByObjectId(page.getByTestId('rules-list'), seededRuleId)).toBeVisible();
      }
    } finally {
      await cleanupRulesByPrefix(request, TEST_INDEX, rulePrefix);
    }
  });

  test('json-tab save persists form updates for consequence and validity', async ({ page, request }) => {
    test.setTimeout(60_000);

    const rulePrefix = createUniqueRulePrefix('json-update');
    const ruleObjectID = `${rulePrefix}-rule`;
    const initialDescription = `Stage 3 initial ${rulePrefix}`;
    const updatedDescription = `Stage 3 updated ${rulePrefix}`;
    const initialPromotedObjectID = `${rulePrefix}-promote-before`;
    const updatedPromotedObjectID = `${rulePrefix}-promote-after`;
    const initialValidityFromLocal = '2030-01-01T08:00';
    const initialValidityUntilLocal = '2030-01-02T08:00';
    const validityFromLocal = '2031-04-05T06:30';
    const validityUntilLocal = '2031-04-06T09:45';
    const initialValidityFrom = dateTimeLocalToUnix(initialValidityFromLocal);
    const initialValidityUntil = dateTimeLocalToUnix(initialValidityUntilLocal);
    const expectedValidityFrom = dateTimeLocalToUnix(validityFromLocal);
    const expectedValidityUntil = dateTimeLocalToUnix(validityUntilLocal);

    await cleanupRulesByPrefix(request, TEST_INDEX, rulePrefix);

    try {
      await createRule(request, TEST_INDEX, {
        objectID: ruleObjectID,
        conditions: [{ pattern: `${rulePrefix}-pattern`, anchoring: 'is' }],
        consequence: {
          promote: [{ objectID: initialPromotedObjectID, position: 0 }],
        },
        validity: [{ from: initialValidityFrom, until: initialValidityUntil }],
        description: initialDescription,
        enabled: true,
      });

      await expect
        .poll(async () => Boolean(await readRuleFromApi(request, TEST_INDEX, ruleObjectID)), { timeout: 10_000 })
        .toBe(true);

      await page.reload();
      await expect(page.getByText('Rules').first()).toBeVisible({ timeout: 15_000 });
      const rulesList = page.getByTestId('rules-list');
      const seededRuleCard = getRuleCardByObjectId(rulesList, ruleObjectID);
      await expect(seededRuleCard).toBeVisible({ timeout: 10_000 });

      await seededRuleCard.getByRole('button', { name: /edit/i }).click();
      const editDialog = page.getByRole('dialog');
      await expect(editDialog).toBeVisible({ timeout: 10_000 });

      await editDialog.getByLabel('Description', { exact: true }).fill(updatedDescription);
      await editDialog.getByLabel('Promote Object ID 1', { exact: true }).fill(updatedPromotedObjectID);

      await editDialog.getByLabel('Valid From 1', { exact: true }).fill(validityFromLocal);
      await editDialog.getByLabel('Valid Until 1', { exact: true }).fill(validityUntilLocal);

      await editDialog.getByRole('tab', { name: 'JSON' }).click();
      await expect(editDialog.getByText(updatedDescription, { exact: false })).toBeVisible({ timeout: 10_000 });
      await expect(editDialog.getByText(`\"${ruleObjectID}\"`)).toBeVisible({ timeout: 10_000 });

      await editDialog.getByRole('button', { name: /^save$/i }).click();
      await expect(editDialog).not.toBeVisible({ timeout: 10_000 });

      await page.reload();
      await expect(page.getByText('Rules').first()).toBeVisible({ timeout: 15_000 });

      const updatedRuleCard = getRuleCardByObjectId(page.getByTestId('rules-list'), ruleObjectID);
      await expect(updatedRuleCard).toBeVisible({ timeout: 10_000 });
      await expect(updatedRuleCard).toContainText(updatedDescription);

      await updatedRuleCard.getByRole('button', { name: /edit/i }).click();
      const reopenedDialog = page.getByRole('dialog');
      await expect(reopenedDialog).toBeVisible({ timeout: 10_000 });
      await expect(reopenedDialog.getByLabel('Description', { exact: true })).toHaveValue(updatedDescription);
      await expect(reopenedDialog.getByLabel('Promote Object ID 1', { exact: true })).toHaveValue(updatedPromotedObjectID);
      await expect(reopenedDialog.getByLabel('Valid From 1', { exact: true })).toHaveValue(validityFromLocal);
      await expect(reopenedDialog.getByLabel('Valid Until 1', { exact: true })).toHaveValue(validityUntilLocal);
      await reopenedDialog.getByRole('button', { name: /cancel/i }).click();

      await expect
        .poll(async () => {
          const persistedRule = await readRuleFromApi(request, TEST_INDEX, ruleObjectID);
          if (!persistedRule) {
            return null;
          }

          const description = persistedRule.description;
          const normalizedDescription = typeof description === 'string' ? description : '';
          const promotedObjectID = readPromotedObjectId(persistedRule);
          const validityRange = readFirstValidityRange(persistedRule);

          return JSON.stringify({
            description: normalizedDescription,
            promotedObjectID,
            validityFrom: validityRange.from,
            validityUntil: validityRange.until,
          });
        }, { timeout: 12_000 })
        .toBe(JSON.stringify({
          description: updatedDescription,
          promotedObjectID: updatedPromotedObjectID,
          validityFrom: expectedValidityFrom,
          validityUntil: expectedValidityUntil,
        }));
    } finally {
      await cleanupRulesByPrefix(request, TEST_INDEX, rulePrefix);
    }
  });
});
