import { expect, type Page } from '@playwright/test';

export interface MigrationPreviewOracle {
  summary: {
    totalEntries: number;
    hardRejections: number;
    warnings: number;
    scopeGaps: number;
  };
  entry: {
    severity: string;
    code: string;
    resource: string;
    jsonPath: string;
  };
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

export function migrationWriteActionName(provider: string, target: string): RegExp {
  const legacyName = `Migrate from ${escapeRegExp(provider)} "${escapeRegExp(target)}"`;
  return new RegExp(`^(?:Submit migration|${legacyName})$`, 'i');
}

export async function expectMigrationWriteUnavailable(
  page: Page,
  provider: string,
  target: string,
): Promise<void> {
  const writeAction = page.getByRole('button', {
    name: migrationWriteActionName(provider, target),
  });
  await expect.poll(async () => {
    const count = await writeAction.count();
    if (count === 0) {
      return true;
    }
    const disabledStates = await Promise.all(
      Array.from({ length: count }, (_, index) => writeAction.nth(index).isDisabled()),
    );
    return disabledStates.every(Boolean);
  }).toBe(true);
}

export async function expectMigrationPreviewReport(
  page: Page,
  oracle: MigrationPreviewOracle,
): Promise<void> {
  const dryRunAffordance = page.getByTestId('migration-preview-dry-run-affordance');
  await expect(dryRunAffordance).toContainText(/dry run/i);
  await expect(dryRunAffordance).toContainText('nothing has been written');
  await expect(page.getByTestId('migration-preview-summary-total-entries'))
    .toHaveText(String(oracle.summary.totalEntries));
  await expect(page.getByTestId('migration-preview-summary-hard-rejections'))
    .toHaveText(String(oracle.summary.hardRejections));
  await expect(page.getByTestId('migration-preview-summary-warnings'))
    .toHaveText(String(oracle.summary.warnings));
  await expect(page.getByTestId('migration-preview-summary-scope-gaps'))
    .toHaveText(String(oracle.summary.scopeGaps));

  const entry = page.getByTestId('migration-preview-entry').filter({
    has: page.getByTestId('migration-preview-entry-code').filter({
      hasText: oracle.entry.code,
    }),
  }).filter({
    has: page.getByTestId('migration-preview-entry-json-path').filter({
      hasText: oracle.entry.jsonPath,
    }),
  });
  await expect(entry).toHaveCount(1);
  await expect(entry.getByTestId('migration-preview-entry-severity'))
    .toHaveText(oracle.entry.severity);
  await expect(entry.getByTestId('migration-preview-entry-code'))
    .toHaveText(oracle.entry.code);
  await expect(entry.getByTestId('migration-preview-entry-resource'))
    .toHaveText(oracle.entry.resource);
  await expect(entry.getByTestId('migration-preview-entry-json-path'))
    .toHaveText(oracle.entry.jsonPath);
}
