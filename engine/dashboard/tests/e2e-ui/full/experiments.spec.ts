/**
 * Browser-unmocked Full/OSS experiment conformance.
 *
 * Arrange may use maintained API fixtures. Every claimed lifecycle action and
 * assertion is performed through visible React UI.
 */
import { test, expect } from '../../fixtures/auth.fixture';
import {
  deleteExperimentsByName,
  deleteExperimentsByPrefix,
  listExperiments,
} from '../../fixtures/api-helpers';
import { seedDeterministicExperimentTraffic } from '../../fixtures/experiment-seed';

const EXPERIMENT_INDEX = 'e2e-products';

async function createdExperimentId(
  request: Parameters<typeof listExperiments>[0],
  name: string,
): Promise<string> {
  const matches = (await listExperiments(request)).filter((experiment) => experiment.name === name);
  if (matches.length !== 1) {
    throw new Error(`expected exactly one experiment named "${name}", found ${matches.length}`);
  }
  return matches[0].id;
}

test.describe.configure({ mode: 'serial' });

test.beforeEach(async ({ request }) => {
  await deleteExperimentsByPrefix(request, 'e2e-exp-pbv4-');
});

test.describe('Experiments Full/OSS conformance', () => {
  test('visible create, inspect, stop, conclude, and delete lifecycle', async ({ page, request }) => {
    const experimentName = `e2e-exp-pbv4-lifecycle-${Date.now()}`;
    const conclusionReason = 'Deterministic Full dashboard lifecycle';

    try {
      await page.goto('/experiments');
      await expect(page.getByRole('heading', { name: 'Experiments', exact: true })).toBeVisible();

      await page.getByRole('button', { name: 'Create Experiment' }).click();
      const createDialog = page.getByTestId('create-experiment-dialog');
      await expect(createDialog).toBeVisible();
      await createDialog.getByLabel('Experiment name').fill(experimentName);
      await createDialog.getByLabel('Index').selectOption(EXPERIMENT_INDEX);
      await createDialog.getByRole('button', { name: 'Next' }).click();

      await createDialog.getByTestId('mode-a-option').check();
      await createDialog.getByLabel('Filters').fill('brand:Apple');
      await createDialog.getByRole('button', { name: 'Next' }).click();
      await expect(createDialog.getByTestId('user-token-warning')).toBeVisible();
      await createDialog.getByRole('button', { name: 'Next' }).click();

      await expect(createDialog.getByTestId('review-name')).toHaveText(experimentName);
      await expect(createDialog.getByTestId('review-index')).toHaveText(EXPERIMENT_INDEX);
      await expect(createDialog.getByTestId('review-mode')).toHaveText('Mode A');
      await createDialog.getByRole('button', { name: 'Launch' }).click();

      const row = page.getByRole('row').filter({ hasText: experimentName });
      await expect(row).toBeVisible();
      await expect(row).toContainText('running');
      await expect(row).toContainText('CTR');

      const experimentId = await createdExperimentId(request, experimentName);
      await seedDeterministicExperimentTraffic(request, experimentId, EXPERIMENT_INDEX);

      await row.getByRole('link', { name: experimentName }).click();
      await expect(page.getByTestId('experiment-detail-name')).toHaveText(experimentName);
      await expect(page.getByTestId('experiment-detail-status')).toContainText('running');

      const controlCard = page.getByTestId('metric-card-control');
      const variantCard = page.getByTestId('metric-card-variant');
      await expect(controlCard).toContainText('95.0%');
      await expect(controlCard).toContainText('190');
      await expect(controlCard).toContainText('200');
      await expect(variantCard).toContainText('100.0%');
      await expect(variantCard).toContainText('200');
      await expect(page.getByTestId('minimum-days-warning')).toBeVisible();
      await expect(page.getByTestId('bayesian-card')).toBeVisible();
      await expect(page.getByTestId('declare-winner-button')).toBeVisible();

      await page.getByTestId('experiment-detail-back-link').click();
      const runningRow = page.getByRole('row').filter({ hasText: experimentName });
      await runningRow.getByRole('button', { name: 'Stop' }).click();
      const stopDialog = page.getByRole('dialog');
      await expect(stopDialog).toBeVisible();
      await stopDialog.getByRole('button', { name: 'Stop' }).click();
      await expect(runningRow).toContainText('stopped');

      await runningRow.getByRole('link', { name: experimentName }).click();
      await expect(page.getByTestId('experiment-detail-status')).toContainText('stopped');
      await page.getByTestId('declare-winner-button').click();
      await expect(page.getByTestId('days-gate-confirmation')).toBeVisible();
      await page.getByRole('button', { name: 'Proceed Anyway' }).click();

      const winnerDialog = page.getByTestId('declare-winner-dialog');
      await expect(winnerDialog).toBeVisible();
      await winnerDialog.getByLabel('Variant').check();
      await winnerDialog.getByLabel('Reason').fill(conclusionReason);
      await expect(winnerDialog.getByLabel('Promote winner settings')).toBeVisible();
      await expect(winnerDialog.getByLabel('Promote winner settings')).not.toBeChecked();
      await winnerDialog.getByRole('button', { name: 'Confirm' }).click();

      await expect(page.getByTestId('experiment-detail-status')).toContainText('concluded');
      await expect(page.getByTestId('conclusion-card')).toContainText(conclusionReason);
      await expect(page.getByTestId('conclusion-card')).toContainText('Promoted to base index: No');

      await page.setViewportSize({ width: 390, height: 844 });
      await expect(controlCard).toBeVisible();
      await expect(variantCard).toBeVisible();
      await page.getByTestId('experiment-detail-back-link').click();
      await expect(page.getByRole('heading', { name: 'Experiments', exact: true })).toBeVisible();
      await expect(page.getByRole('button', { name: 'Create Experiment' })).toBeVisible();
      const concludedRow = page.getByRole('row').filter({ hasText: experimentName });
      await expect(concludedRow).toBeVisible();
      await expect(concludedRow.getByRole('button', { name: 'Delete' })).toBeVisible();

      await page.getByRole('button', { name: 'Create Experiment' }).click();
      const narrowCreateDialog = page.getByTestId('create-experiment-dialog');
      await expect(narrowCreateDialog).toBeVisible();
      await expect(narrowCreateDialog.getByLabel('Experiment name')).toBeVisible();
      await expect(narrowCreateDialog.getByRole('button', { name: 'Next' })).toBeVisible();
      await expect(narrowCreateDialog.getByRole('button', { name: 'Cancel' })).toBeVisible();
      await narrowCreateDialog.getByRole('button', { name: 'Cancel' }).click();
      await expect(narrowCreateDialog).toBeHidden();

      await concludedRow.getByRole('button', { name: 'Delete' }).click();
      const deleteDialog = page.getByRole('dialog');
      await expect(deleteDialog).toBeVisible();
      await deleteDialog.getByRole('button', { name: 'Delete' }).click();
      await expect(page.getByRole('row').filter({ hasText: experimentName })).toHaveCount(0);
    } finally {
      await deleteExperimentsByName(request, experimentName);
    }
  });
});
