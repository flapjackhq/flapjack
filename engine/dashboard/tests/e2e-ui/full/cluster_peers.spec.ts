import {
  expectedCleartextRefusalMessage,
  expect,
  test,
  SEEDED_HTTPS_PEER,
  UI_ADDED_HTTPS_PEER,
  CLEARTEXT_HTTP_PEER,
  waitForAddClusterPeerResponse,
} from '../../fixtures/cluster-peers';

test.describe('Cluster peer management', () => {
  // These specs mutate shared runtime membership; run them serially.
  test.describe.configure({ mode: 'serial' });

  test('renders seeded HA membership before any mutation', async ({ page, seededCluster }) => {
    await page.goto('/cluster');

    const row = page.getByTestId(`cluster-peer-row-${SEEDED_HTTPS_PEER.node_id}`);
    await expect(row).toBeVisible();
    await expect(row).toContainText(SEEDED_HTTPS_PEER.node_id);
    await expect(row).toContainText(SEEDED_HTTPS_PEER.addr);
    await expect(page.getByTestId(`cluster-peer-status-${SEEDED_HTTPS_PEER.node_id}`)).toHaveText(
      seededCluster.expectedPresentation.statusLabel,
    );
    await expect(page.getByTestId(`cluster-peer-last-success-${SEEDED_HTTPS_PEER.node_id}`)).toHaveText(
      seededCluster.expectedPresentation.lastSuccessLabel,
    );
    await expect(page.getByTestId('cluster-peers-total-value')).toHaveText(String(seededCluster.status.peers_total));
    await expect(page.getByTestId('cluster-peers-healthy-value')).toHaveText(String(seededCluster.status.peers_healthy));
  });

  test('adds an https:// peer from the UI and shows the new row', async ({
    page,
    seededCluster,
    clusterPeerOracle,
  }) => {
    await page.goto('/cluster');

    await page.getByTestId('cluster-add-peer-button').click();
    await page.getByTestId('cluster-add-peer-node-id-input').fill(UI_ADDED_HTTPS_PEER.node_id);
    await page.getByTestId('cluster-add-peer-addr-input').fill(UI_ADDED_HTTPS_PEER.addr);
    await page.getByTestId('cluster-add-peer-submit').click();

    await expect(page.getByTestId('cluster-add-peer-panel')).toBeHidden();
    const row = page.getByTestId(`cluster-peer-row-${UI_ADDED_HTTPS_PEER.node_id}`);
    await expect(row).toBeVisible();
    await expect(row).toContainText(UI_ADDED_HTTPS_PEER.addr);
    await expect(page.getByTestId('cluster-peers-total-value')).toHaveText(
      String(seededCluster.status.peers_total + 1),
    );
    await clusterPeerOracle.confirmUiAddedPeerInClusterStatus();
  });

  test('lets http:// reach the backend and renders the exact refusal message', async ({
    page,
    clusterPeerOracle,
  }) => {
    await page.goto('/cluster');

    await page.getByTestId('cluster-add-peer-button').click();
    await page.getByTestId('cluster-add-peer-node-id-input').fill(CLEARTEXT_HTTP_PEER.node_id);
    await page.getByTestId('cluster-add-peer-addr-input').fill(CLEARTEXT_HTTP_PEER.addr);
    const responsePromise = waitForAddClusterPeerResponse(page);
    await page.getByTestId('cluster-add-peer-submit').click();
    const response = await responsePromise;
    await clusterPeerOracle.confirmCleartextPeerRefusal(response);

    const errorRegion = page.getByTestId('cluster-add-peer-error');
    await expect(errorRegion).toHaveText(expectedCleartextRefusalMessage(CLEARTEXT_HTTP_PEER));
    // Membership is unchanged and the submitted values remain editable.
    await expect(page.getByTestId('cluster-add-peer-panel')).toBeVisible();
    await expect(page.getByTestId('cluster-add-peer-addr-input')).toHaveValue(CLEARTEXT_HTTP_PEER.addr);
    await expect(page.getByTestId(`cluster-peer-row-${CLEARTEXT_HTTP_PEER.node_id}`)).toBeHidden();
  });

  test('cancels removal with the Cancel button without dropping the seeded row', async ({
    page,
    clusterPeerOracle,
  }) => {
    await page.goto('/cluster');

    const removeButton = page.getByTestId(`cluster-peer-remove-${SEEDED_HTTPS_PEER.node_id}`);
    await removeButton.click();
    const dialog = page.getByTestId('cluster-remove-peer-dialog');
    await expect(dialog).toContainText(`Remove peer ${SEEDED_HTTPS_PEER.node_id}?`);

    await page.getByTestId('cluster-remove-peer-cancel').click();
    await expect(dialog).toBeHidden();
    await expect(page.getByTestId(`cluster-peer-row-${SEEDED_HTTPS_PEER.node_id}`)).toBeVisible();
    await expect(removeButton).toBeFocused();

    await clusterPeerOracle.confirmSeededPeerInClusterStatus();
  });

  test('cancels removal with Escape without dropping the seeded row', async ({
    page,
    clusterPeerOracle,
  }) => {
    await page.goto('/cluster');

    const removeButton = page.getByTestId(`cluster-peer-remove-${SEEDED_HTTPS_PEER.node_id}`);
    await removeButton.click();
    const dialog = page.getByTestId('cluster-remove-peer-dialog');
    await dialog.press('Escape');
    await expect(dialog).toBeHidden();
    await expect(page.getByTestId(`cluster-peer-row-${SEEDED_HTTPS_PEER.node_id}`)).toBeVisible();
    await expect(removeButton).toBeFocused();

    await clusterPeerOracle.confirmSeededPeerInClusterStatus();
  });

  test('cancels removal with an outside click without dropping the seeded row', async ({
    page,
    clusterPeerOracle,
  }) => {
    await page.goto('/cluster');

    const removeButton = page.getByTestId(`cluster-peer-remove-${SEEDED_HTTPS_PEER.node_id}`);
    await removeButton.click();
    const dialog = page.getByTestId('cluster-remove-peer-dialog');
    await page.mouse.click(1, 1);
    await expect(dialog).toBeHidden();
    await expect(page.getByTestId(`cluster-peer-row-${SEEDED_HTTPS_PEER.node_id}`)).toBeVisible();
    await expect(removeButton).toBeFocused();

    await clusterPeerOracle.confirmSeededPeerInClusterStatus();
  });

  test('confirms removal and drops the row', async ({ page, clusterPeerOracle }) => {
    await page.goto('/cluster');

    await page.getByTestId(`cluster-peer-remove-${SEEDED_HTTPS_PEER.node_id}`).click();
    await page.getByTestId('cluster-remove-peer-confirm').click();

    await expect(page.getByTestId('cluster-remove-peer-dialog')).toBeHidden();
    await expect(page.getByTestId(`cluster-peer-row-${SEEDED_HTTPS_PEER.node_id}`)).toBeHidden();
    await clusterPeerOracle.confirmSeededPeerAbsentFromClusterStatus();
  });
});
