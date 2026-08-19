/**
 */
import type { APIRequestContext } from '@playwright/test';
import { deleteRule, getRules } from '../fixtures/api-helpers';

export function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === 'object';
}

/**
 * TODO: Document cleanupRulesByPrefix.
 */
export async function cleanupRulesByPrefix(
  request: APIRequestContext,
  indexName: string,
  rulePrefix: string,
): Promise<void> {
  const response = await getRules(request, indexName).catch(() => null);
  if (!response?.ok) {
    return;
  }

  const matchingRuleIds = response.items
    .map((item) => (item as any)?.objectID)
    .filter((objectID): objectID is string => typeof objectID === 'string' && objectID.startsWith(rulePrefix));

  for (const ruleId of matchingRuleIds) {
    await deleteRule(request, indexName, ruleId).catch(() => {});
  }
}

/**
 * TODO: Document cleanupRulesByDescriptionPrefix.
 */
export async function cleanupRulesByDescriptionPrefix(
  request: APIRequestContext,
  indexName: string,
  descriptionPrefix: string,
): Promise<void> {
  const response = await getRules(request, indexName).catch(() => null);
  if (!response?.ok) {
    return;
  }

  const matchingRuleIDs = response.items
    .filter((item) => typeof (item as any)?.description === 'string')
    .filter((item) => ((item as any).description as string).startsWith(descriptionPrefix))
    .map((item) => (item as any)?.objectID)
    .filter((ruleID): ruleID is string => typeof ruleID === 'string');

  for (const ruleID of matchingRuleIDs) {
    await deleteRule(request, indexName, ruleID).catch(() => {});
  }
}
