/**
 * @module End-to-end Playwright tests for the Experiments list and detail pages, verifying CRUD operations, status transitions, navigation, and metric card rendering against a real server.
 */
/**
 * Browser-unmocked Full Suite — Experiments Page (Real Server)
 *
 * Arrange uses fixture helpers for API seeding/cleanup.
 * Act + Assert uses only visible UI interactions.
 */
import { test, expect } from '../../fixtures/auth.fixture';
import {
  addDocuments,
  createExperiment,
  createIndex,
  deleteExperiment,
  deleteExperimentsByPrefix,
  deleteExperimentsByName,
  deleteIndex,
  flushAnalytics,
  getExperimentResults,
  sendEvents,
  searchIndex,
  startExperiment,
  stopExperiment,
  updateExperiment,
  waitForExperimentResults,
  waitForExperimentByName,
} from '../../fixtures/api-helpers';
import { PRODUCTS } from '../../fixtures/test-data';
import {
  seedRouteAuditExperiment,
  type SeededRouteAuditExperiment,
} from '../../fixtures/experiment-seed';

const EXPERIMENT_INDEX = 'e2e-products';
const EXPERIMENT_QUERY = 'apple';

type ExperimentSearchResponse = Awaited<ReturnType<typeof searchIndex>>;

function uniqueSuffix(): string {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

function makeVariantIndexName(prefix: string): string {
  return `${prefix}-${uniqueSuffix()}`;
}

function readSearchStringField(
  response: ExperimentSearchResponse,
  fieldName: string,
): string | null {
  const value = response[fieldName];
  return typeof value === 'string' && value.length > 0 ? value : null;
}

function readFirstHitObjectId(response: ExperimentSearchResponse): string | null {
  if (!Array.isArray(response.hits) || response.hits.length === 0) {
    return null;
  }
  const firstHit = response.hits[0];
  if (typeof firstHit !== 'object' || firstHit === null) {
    return null;
  }
  const objectId = (firstHit as { objectID?: unknown }).objectID;
  return typeof objectId === 'string' && objectId.length > 0 ? objectId : null;
}

function readInterleavedTeams(response: ExperimentSearchResponse): Record<string, string> {
  const teams = response.interleavedTeams;
  if (!teams || typeof teams !== 'object' || Array.isArray(teams)) {
    return {};
  }
  const parsed: Record<string, string> = {};
  for (const [objectId, team] of Object.entries(teams as Record<string, unknown>)) {
    if (typeof team === 'string' && (team === 'control' || team === 'variant')) {
      parsed[objectId] = team;
    }
  }
  return parsed;
}

async function searchWithExperimentTracking(
  request: Parameters<typeof searchIndex>[0],
  indexName: string,
  userToken: string,
  responseFields?: string[],
): Promise<ExperimentSearchResponse> {
  return searchIndex(request, indexName, EXPERIMENT_QUERY, {
    userToken,
    analytics: true,
    clickAnalytics: true,
    responseFields,
  });
}

async function collectUserTokensForArm(
  request: Parameters<typeof searchIndex>[0],
  indexName: string,
  targetArm: 'control' | 'variant',
  count: number,
): Promise<string[]> {
  const tokens: string[] = [];

  for (let i = 0; i < 400 && tokens.length < count; i += 1) {
    const candidate = `e2e-exp-arm-${targetArm}-${i}-${uniqueSuffix()}`;
    const response = await searchIndex(request, indexName, EXPERIMENT_QUERY, {
      userToken: candidate,
      analytics: false,
      clickAnalytics: false,
    });
    const arm = readSearchStringField(response, 'abTestVariantID');
    if (arm === targetArm) {
      tokens.push(candidate);
    }
  }

  if (tokens.length !== count) {
    throw new Error(`Unable to collect ${count} user tokens for arm "${targetArm}"`);
  }

  return tokens;
}

async function clickTrackedSearchHit(
  request: Parameters<typeof sendEvents>[0],
  indexName: string,
  userToken: string,
  eventPrefix: string,
  response: ExperimentSearchResponse,
): Promise<void> {
  const queryId = readSearchStringField(response, 'queryID');
  const objectId = readFirstHitObjectId(response);
  if (!queryId || !objectId) {
    return;
  }

  await sendEvents(request, [
    {
      eventType: 'click',
      eventName: `${eventPrefix}-${uniqueSuffix()}`,
      index: indexName,
      userToken,
      objectIDs: [objectId],
      positions: [1],
      queryID: queryId,
    },
  ]);
}

async function clickFirstSearchHit(
  request: Parameters<typeof sendEvents>[0],
  indexName: string,
  userToken: string,
  eventPrefix: string,
): Promise<void> {
  const response = await searchWithExperimentTracking(request, indexName, userToken);
  await clickTrackedSearchHit(request, indexName, userToken, eventPrefix, response);
}

/**
 * Seed traffic so that the experiment reaches soft-gate readiness
 * (minimumNReached = true, minimumDaysReached = false).
 *
 * Strategy: every user does exactly 1 search + 1 click, yielding 100%
 * per-user CTR. When the backend computes baseline_rate = 1.0, the power
 * analysis formula produces p2 = 1.05 > 1, causing NaN in the variance
 * term. Rust casts NaN to u64 as 0, so required_per_arm = 0 and
 * minimumNReached becomes trivially true for any search count > 0.
 */
async function seedSoftGateReadyTraffic(
  request: Parameters<typeof sendEvents>[0],
  experimentId: string,
  indexName: string,
): Promise<void> {
  const isSoftGateReady = (results: Awaited<ReturnType<typeof getExperimentResults>>) => (
    results.gate.minimumNReached
    && !results.gate.minimumDaysReached
    && results.control.searches > 0
    && results.variant.searches > 0
    && !!results.bayesian
  );

  // Generate 20 unique users, each doing 1 search + 1 click (100% CTR).
  // With 50/50 split, each arm gets ~10 users — enough for both arms.
  for (let i = 0; i < 20; i += 1) {
    const token = `e2e-exp-soft-${i}-${uniqueSuffix()}`;
    await clickFirstSearchHit(request, indexName, token, 'e2e-exp-soft');
  }

  await flushAnalytics(request, indexName);
  await waitForExperimentResults(
    request,
    experimentId,
    isSoftGateReady,
    20_000,
    500,
  );
}

async function seedSrmGuardRailTraffic(
  request: Parameters<typeof sendEvents>[0],
  experimentId: string,
  indexName: string,
): Promise<void> {
  const controlTokens = await collectUserTokensForArm(request, indexName, 'control', 12);
  const variantTokens = await collectUserTokensForArm(request, indexName, 'variant', 2);

  for (const token of controlTokens) {
    await clickFirstSearchHit(request, indexName, token, 'e2e-exp-srm-control');
  }
  for (const token of variantTokens) {
    await searchWithExperimentTracking(request, indexName, token);
  }

  await flushAnalytics(request, indexName);
  await waitForExperimentResults(
    request,
    experimentId,
    (results) => results.sampleRatioMismatch && results.guardRailAlerts.length > 0,
    45_000,
    500,
  );
}

async function seedInterleavingTraffic(
  request: Parameters<typeof sendEvents>[0],
  experimentId: string,
  indexName: string,
): Promise<void> {
  const interleavingToken = `e2e-exp-interleave-${uniqueSuffix()}`;

  for (let i = 0; i < 6; i += 1) {
    const response = await searchWithExperimentTracking(
      request,
      indexName,
      interleavingToken,
      ['hits', 'interleavedTeams', 'queryID'],
    );

    const queryId = readSearchStringField(response, 'queryID');
    const teams = readInterleavedTeams(response);
    if (!queryId) {
      continue;
    }

    const controlObjectId = Object.entries(teams).find(([, team]) => team === 'control')?.[0];
    const variantObjectId = Object.entries(teams).find(([, team]) => team === 'variant')?.[0];
    if (!controlObjectId || !variantObjectId) {
      continue;
    }

    await sendEvents(request, [
      {
        eventType: 'click',
        eventName: `e2e-exp-interleave-control-${uniqueSuffix()}`,
        index: indexName,
        userToken: interleavingToken,
        objectIDs: [controlObjectId],
        positions: [1],
        queryID: queryId,
        interleavingTeam: 'control',
      },
      {
        eventType: 'click',
        eventName: `e2e-exp-interleave-variant-${uniqueSuffix()}`,
        index: indexName,
        userToken: interleavingToken,
        objectIDs: [variantObjectId],
        positions: [1],
        queryID: queryId,
        interleavingTeam: 'variant',
      },
    ]);
  }

  await flushAnalytics(request, indexName);
  await waitForExperimentResults(
    request,
    experimentId,
    (results) => !!results.interleaving && results.interleaving.totalQueries > 0,
    30_000,
    500,
  );
}

/**
 * Builds a complete experiment creation payload with sensible defaults for E2E tests.
 * 
 * @param name - Unique experiment name, typically suffixed with `Date.now()` to avoid collisions.
 * @returns An experiment payload targeting the shared seeded products index with a 50/50 traffic split, a no-op control arm, a variant filtering to `brand:Apple`, CTR as the primary metric, and a 14-day minimum duration.
 */
function makeExperimentPayload(name: string) {
  return {
    name,
    indexName: EXPERIMENT_INDEX,
    trafficSplit: 0.5,
    control: { name: 'control' },
    variant: {
      name: 'variant',
      queryOverrides: {
        filters: 'brand:Apple',
      },
    },
    primaryMetric: 'ctr',
    minimumDays: 14,
  };
}

async function cleanupConcludedExperiment(
  request: Parameters<typeof getExperimentResults>[0],
  experimentId: string,
): Promise<void> {
  const latestResults = await getExperimentResults(request, experimentId);
  if (latestResults.status !== 'concluded') {
    await stopExperiment(request, experimentId);
  }
  await deleteExperiment(request, experimentId);
}

test.describe.configure({ mode: 'serial' });

test.beforeEach(async ({ request }) => {
  // Limit cleanup to this suite's own fixtures so browser-unmocked runs do not
  // delete unrelated experiments from the shared backend instance.
  await deleteExperimentsByPrefix(request, 'e2e-exp-');
});

test.describe('Experiments Page', () => {
  test('load-and-verify: seeded experiment renders in experiments table', async ({ page, request }) => {
    const experiment = await createExperiment(
      request,
      makeExperimentPayload(`e2e-exp-load-${Date.now()}`),
    );
    await startExperiment(request, experiment.id);

    try {
      await page.goto('/experiments');

      await expect(page.getByTestId('experiments-heading')).toBeVisible({ timeout: 10_000 });
      await expect(page.getByTestId('experiments-table')).toBeVisible();

      const row = page.getByTestId(`experiment-row-${experiment.id}`);
      await expect(row).toBeVisible({ timeout: 10_000 });
      await expect(row.getByText(experiment.name)).toBeVisible();
      await expect(row.getByText(EXPERIMENT_INDEX)).toBeVisible();
      await expect(row.getByText('running')).toBeVisible();
      await expect(row.getByText('CTR')).toBeVisible();
      await expect(page.getByTestId(`stop-experiment-${experiment.id}`)).toBeVisible();
    } finally {
      await deleteExperiment(request, experiment.id);
    }
  });

  test('running experiment can be stopped from the list UI', async ({ page, request }) => {
    const experiment = await createExperiment(
      request,
      makeExperimentPayload(`e2e-exp-stop-${Date.now()}`),
    );
    await startExperiment(request, experiment.id);

    try {
      await page.goto('/experiments');
      await expect(page.getByTestId('experiments-heading')).toBeVisible({ timeout: 10_000 });

      await page.getByTestId(`stop-experiment-${experiment.id}`).click();
      const dialog = page.getByRole('dialog');
      await expect(dialog).toBeVisible();
      await dialog.getByRole('button', { name: /^Stop$/i }).click();

      await expect(page.getByTestId(`experiment-status-${experiment.id}`)).toContainText('stopped', {
        timeout: 10_000,
      });
      await expect(page.getByTestId(`stop-experiment-${experiment.id}`)).toHaveCount(0);
      await expect(page.getByTestId(`delete-experiment-${experiment.id}`)).toBeVisible();
    } finally {
      await deleteExperiment(request, experiment.id);
    }
  });

  test('create dialog submit creates an experiment through the UI flow', async ({ page, request }) => {
    const experimentName = `e2e-exp-ui-create-${Date.now()}`;
    const variantIndex = makeVariantIndexName('e2e-exp-mode-b-variant');

    await createIndex(request, variantIndex);

    try {
      await page.goto('/experiments');
      await expect(page.getByTestId('experiments-heading')).toBeVisible({ timeout: 10_000 });

      await page.getByRole('button', { name: 'Create Experiment' }).click();
      await expect(page.getByTestId('create-experiment-dialog')).toBeVisible();

      await page.getByTestId('experiment-name-input').fill(experimentName);
      await page.getByTestId('experiment-index-select').selectOption(EXPERIMENT_INDEX);
      await page.getByRole('button', { name: 'Next' }).click();

      await page.getByTestId('mode-b-option').check();
      await page.getByTestId('variant-index-select').selectOption(variantIndex);
      await page.getByRole('button', { name: 'Next' }).click();
      await page.getByRole('button', { name: 'Next' }).click();

      await expect(page.getByTestId('review-name')).toHaveText(experimentName);
      await expect(page.getByTestId('review-index')).toHaveText(EXPERIMENT_INDEX);
      await expect(page.getByTestId('review-mode')).toHaveText('Mode B');
      await expect(page.getByTestId('review-variant-index')).toHaveText(variantIndex);
      await page.getByRole('button', { name: 'Launch' }).click();

      const created = await waitForExperimentByName(request, experimentName);
      const row = page.getByTestId(`experiment-row-${created.id}`);
      await expect(row).toBeVisible({ timeout: 10_000 });
      await expect(row.getByText(experimentName)).toBeVisible();
      await expect(row.getByText('running')).toBeVisible();
    } finally {
      await deleteExperimentsByName(request, experimentName);
      await deleteIndex(request, variantIndex);
    }
  });

  test('stopped experiment can be deleted from the list UI', async ({ page, request }) => {
    const experiment = await createExperiment(
      request,
      makeExperimentPayload(`e2e-exp-delete-${Date.now()}`),
    );
    await stopExperiment(request, experiment.id);

    try {
      await page.goto('/experiments');
      await expect(page.getByTestId('experiments-heading')).toBeVisible({ timeout: 10_000 });

      await expect(page.getByTestId(`stop-experiment-${experiment.id}`)).toHaveCount(0);
      await expect(page.getByTestId(`delete-experiment-${experiment.id}`)).toBeVisible();

      await page.getByTestId(`delete-experiment-${experiment.id}`).click();
      const dialog = page.getByRole('dialog');
      await expect(dialog).toBeVisible();
      await dialog.getByRole('button', { name: /^Delete$/i }).click();

      await expect(page.getByTestId(`experiment-row-${experiment.id}`)).toHaveCount(0, {
        timeout: 10_000,
      });
    } finally {
      await deleteExperiment(request, experiment.id);
    }
  });
});

test.describe('Experiment Detail Page', () => {
  test('clicking experiment name in list navigates to detail page', async ({ page, request }) => {
    const experiment = await createExperiment(
      request,
      makeExperimentPayload(`e2e-exp-detail-nav-${Date.now()}`),
    );
    await startExperiment(request, experiment.id);

    try {
      await page.goto('/experiments');
      await expect(page.getByTestId('experiments-heading')).toBeVisible({ timeout: 10_000 });

      // Click the experiment name link to navigate to detail
      const row = page.getByTestId(`experiment-row-${experiment.id}`);
      await row.getByRole('link', { name: experiment.name }).click();

      // Verify detail page loaded with correct name and status
      await expect(page.getByTestId('experiment-detail-name')).toHaveText(experiment.name, {
        timeout: 10_000,
      });
      await expect(page.getByTestId('experiment-detail-status')).toContainText('running');
    } finally {
      await deleteExperiment(request, experiment.id);
    }
  });

  test.describe('seeded draft detail', () => {
    let experiment: SeededRouteAuditExperiment;

    test.beforeAll(async ({ request }) => {
      experiment = await seedRouteAuditExperiment(request);
    });

    test.afterAll(async ({ request }) => {
      if (experiment) {
        await deleteExperiment(request, experiment.id);
      }
    });

    test('detail page shows experiment name, status, index, and metric', async ({ page }) => {
      await page.goto(`/experiments/${experiment.id}`);

      await expect(page.getByTestId('experiment-detail-name')).toHaveText(experiment.name, {
        timeout: 10_000,
      });
      await expect(page.getByTestId('experiment-detail-status')).toContainText(experiment.status);
      await expect(page.getByTestId('experiment-detail-index')).toHaveText(experiment.indexName);
      await expect(page.getByTestId('experiment-detail-primary-metric')).toHaveText(
        experiment.primaryMetricLabel,
      );
    });
  });

  test('detail page shows progress bar for running experiment collecting data', async ({ page, request }) => {
    const experiment = await createExperiment(
      request,
      makeExperimentPayload(`e2e-exp-detail-progress-${Date.now()}`),
    );
    await startExperiment(request, experiment.id);

    try {
      await page.goto(`/experiments/${experiment.id}`);

      await expect(page.getByTestId('experiment-detail-name')).toBeVisible({ timeout: 10_000 });
      // Progress bar should be visible for a fresh experiment with no data
      await expect(page.getByTestId('progress-bar')).toBeVisible({ timeout: 10_000 });
      await expect(page.getByText('Data collection progress')).toBeVisible();
    } finally {
      await deleteExperiment(request, experiment.id);
    }
  });

  test('detail page shows control and variant metric cards', async ({ page, request }) => {
    const experiment = await createExperiment(
      request,
      makeExperimentPayload(`e2e-exp-detail-metrics-${Date.now()}`),
    );

    try {
      await page.goto(`/experiments/${experiment.id}`);

      await expect(page.getByTestId('experiment-detail-name')).toBeVisible({ timeout: 10_000 });
      // Both arm metric cards should be visible
      await expect(page.getByTestId('metric-card-control')).toBeVisible({ timeout: 10_000 });
      await expect(page.getByTestId('metric-card-variant')).toBeVisible();

      // Verify metric labels are rendered in each card
      const controlCard = page.getByTestId('metric-card-control');
      await expect(controlCard.getByText('Control')).toBeVisible();
      await expect(controlCard.getByText('Searches')).toBeVisible();
      await expect(controlCard.getByText('Users')).toBeVisible();
      await expect(controlCard.getByText('Clicks')).toBeVisible();

      const variantCard = page.getByTestId('metric-card-variant');
      await expect(variantCard.getByText('Variant')).toBeVisible();
      await expect(variantCard.getByText('Searches')).toBeVisible();
    } finally {
      await deleteExperiment(request, experiment.id);
    }
  });

  test('back link navigates from detail to experiments list', async ({ page, request }) => {
    const experiment = await createExperiment(
      request,
      makeExperimentPayload(`e2e-exp-detail-back-${Date.now()}`),
    );

    try {
      await page.goto(`/experiments/${experiment.id}`);
      await expect(page.getByTestId('experiment-detail-name')).toBeVisible({ timeout: 10_000 });

      // Click the back link
      await page.getByTestId('experiment-detail-back-link').click();

      // Should be back on the list page
      await expect(page.getByTestId('experiments-heading')).toBeVisible({ timeout: 10_000 });
    } finally {
      await deleteExperiment(request, experiment.id);
    }
  });

  test('running experiment detail page renders from arrange-time creation', async ({ page, request }) => {
    const experimentName = `e2e-exp-detail-running-${Date.now()}`;
    const experiment = await createExperiment(
      request,
      makeExperimentPayload(experimentName),
    );
    await startExperiment(request, experiment.id);
    await seedSoftGateReadyTraffic(request, experiment.id, EXPERIMENT_INDEX);

    try {
      await page.goto(`/experiments/${experiment.id}`);

      await expect(page.getByTestId('experiment-detail-name')).toHaveText(experimentName, {
        timeout: 10_000,
      });
      await expect(page.getByTestId('experiment-detail-status')).toContainText('running');
      await expect(page.getByTestId('experiment-detail-index')).toHaveText(EXPERIMENT_INDEX);
      await expect(page.getByTestId('experiment-detail-primary-metric')).toHaveText('CTR');
      await expect(page.getByTestId('metric-card-control')).toBeVisible();
      await expect(page.getByTestId('metric-card-variant')).toBeVisible();
      await expect(page.getByTestId('minimum-days-warning')).toBeVisible();
      await expect(page.getByTestId('bayesian-card')).toBeVisible();
      await expect(page.getByTestId('declare-winner-button')).toBeVisible();
    } finally {
      await deleteExperiment(request, experiment.id);
    }
  });

  test('running experiment detail shows SRM and guard rail banners for skewed traffic', async ({ page, request }) => {
    test.setTimeout(60_000);
    const experiment = await createExperiment(
      request,
      makeExperimentPayload(`e2e-exp-detail-srm-${Date.now()}`),
    );
    await startExperiment(request, experiment.id);
    await seedSrmGuardRailTraffic(request, experiment.id, EXPERIMENT_INDEX);

    try {
      await page.goto(`/experiments/${experiment.id}`);
      await expect(page.getByTestId('experiment-detail-name')).toBeVisible({ timeout: 10_000 });
      await expect(page.getByTestId('srm-banner')).toBeVisible();
      await expect(page.getByTestId('guard-rail-banner')).toBeVisible();
    } finally {
      await deleteExperiment(request, experiment.id);
    }
  });

  test('running experiment detail shows interleaving card when interleaving metrics exist', async ({ page, request }) => {
    const experimentName = `e2e-exp-detail-interleave-${Date.now()}`;
    const variantIndex = makeVariantIndexName('e2e-exp-interleaving-variant');
    await createIndex(request, variantIndex);
    await addDocuments(request, variantIndex, PRODUCTS);
    await expect.poll(async () => (
      Number((await searchIndex(request, EXPERIMENT_INDEX, EXPERIMENT_QUERY, { hitsPerPage: 20 })).nbHits ?? 0)
    )).toBeGreaterThan(0);
    await expect.poll(async () => (
      Number((await searchIndex(request, variantIndex, EXPERIMENT_QUERY, { hitsPerPage: 20 })).nbHits ?? 0)
    )).toBeGreaterThan(0);

    const experiment = await createExperiment(request, {
      name: experimentName,
      indexName: EXPERIMENT_INDEX,
      trafficSplit: 0.5,
      control: { name: 'control' },
      variant: {
        name: 'variant',
        indexName: variantIndex,
      },
      primaryMetric: 'ctr',
      minimumDays: 14,
    });
    await updateExperiment(request, experiment.id, {
      name: experimentName,
      indexName: EXPERIMENT_INDEX,
      trafficSplit: 0.5,
      control: { name: 'control' },
      variant: {
        name: 'variant',
        indexName: variantIndex,
      },
      primaryMetric: 'ctr',
      minimumDays: 14,
      interleaving: true,
    });
    await startExperiment(request, experiment.id);
    await seedInterleavingTraffic(request, experiment.id, EXPERIMENT_INDEX);

    try {
      await page.goto(`/experiments/${experiment.id}`);
      await expect(page.getByTestId('experiment-detail-name')).toBeVisible({ timeout: 10_000 });
      await expect(page.getByTestId('interleaving-card')).toBeVisible();
    } finally {
      await deleteExperiment(request, experiment.id);
      await deleteIndex(request, variantIndex);
    }
  });

  test('stopped experiment detail shows stopped status and no declare winner', async ({ page, request }) => {
    const experiment = await createExperiment(
      request,
      makeExperimentPayload(`e2e-exp-detail-stopped-${Date.now()}`),
    );
    await stopExperiment(request, experiment.id);

    try {
      await page.goto(`/experiments/${experiment.id}`);

      await expect(page.getByTestId('experiment-detail-name')).toBeVisible({ timeout: 10_000 });
      await expect(page.getByTestId('experiment-detail-status')).toContainText('stopped');
      // Declare Winner button should not be present for stopped experiment without sufficient data
      await expect(page.getByTestId('declare-winner-button')).toHaveCount(0);
    } finally {
      await deleteExperiment(request, experiment.id);
    }
  });

  test('declare winner flow concludes a soft-gated experiment through browser dialog', async ({ page, request }) => {
    test.setTimeout(60_000);
    const experimentName = `e2e-exp-detail-declare-${Date.now()}`;
    const experiment = await createExperiment(
      request,
      makeExperimentPayload(experimentName),
    );
    await startExperiment(request, experiment.id);
    await seedSoftGateReadyTraffic(request, experiment.id, EXPERIMENT_INDEX);

    const conclusionReason = `Declare winner from browser flow ${uniqueSuffix()}`;

    try {
      await page.goto(`/experiments/${experiment.id}`);
      await expect(page.getByTestId('experiment-detail-name')).toHaveText(experimentName, {
        timeout: 10_000,
      });
      await expect(page.getByTestId('declare-winner-button')).toBeVisible();

      await page.getByTestId('declare-winner-button').click();
      await expect(page.getByTestId('days-gate-confirmation')).toBeVisible();
      await page.getByRole('button', { name: 'Proceed Anyway' }).click();

      const dialog = page.getByTestId('declare-winner-dialog');
      await expect(dialog).toBeVisible();
      await dialog.getByLabel('Variant').check();
      await dialog.getByLabel('Reason').fill(conclusionReason);
      await dialog.getByRole('button', { name: 'Confirm' }).click();

      await expect(page.getByTestId('experiment-detail-status')).toContainText('concluded', {
        timeout: 10_000,
      });
      await expect(page.getByTestId('conclusion-card')).toBeVisible();
      await expect(page.getByTestId('conclusion-card')).toContainText(conclusionReason);
      await expect(page.getByTestId('declare-winner-button')).toHaveCount(0);
    } finally {
      await cleanupConcludedExperiment(request, experiment.id);
    }
  });
});
