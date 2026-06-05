import { test, expect } from '../../fixtures/auth.fixture';
import { buildDashboardRouteAudit } from '../route_audit_manifest';

const clusterRoute = buildDashboardRouteAudit('cluster_standalone_copy').find((route) => route.path === '/cluster');

test.describe('Cluster standalone copy', () => {
  test('keeps disabled replication factual while reassuring single-node operators', async ({ page }) => {
    expect(clusterRoute, 'route manifest must own /cluster readiness').toBeDefined();

    await page.goto('/cluster');
    await clusterRoute?.waitForReady(page);

    const standaloneState = page.getByTestId('cluster-standalone-state');
    await expect(standaloneState).toBeVisible();
    await expect(standaloneState).toContainText('Standalone mode');
    await expect(page.getByTestId('cluster-node-id-value')).not.toBeEmpty();
    await expect(page.getByTestId('cluster-replication-value')).toHaveText('Standalone mode');
    await expect(standaloneState).toContainText(
      'Single-node operation is healthy and expected. Add peers only if you want multi-node HA replication.',
    );
  });
});
