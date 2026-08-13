/**
 * Single fixture owner for cluster-peer browser coverage.
 *
 * These helpers wrap the product cluster endpoints so spec files never call
 * `request.*` directly (which the e2e ESLint config bans). Fixture files are
 * exempt from those spec rules. Every helper asserts response readiness before
 * returning and mutates membership only through the real HTTP API — never by
 * writing storage.
 *
 * Request/response ownership stays on the backend handlers
 * (`internal.rs::{add_cluster_peer, remove_cluster_peer, cluster_status}`); this
 * file only exercises them.
 */
import { expect, type APIRequestContext, type Page, type Response } from '@playwright/test';
import { test as authenticatedTest } from './auth.fixture';
import { API_BASE, API_HEADERS } from './local-instance';

export { expect };

const CLUSTER_STATUS_PATH = `${API_BASE}/internal/cluster/status`;
const CLUSTER_PEERS_PATH = `${API_BASE}/internal/cluster/peers`;

/** A safe https:// peer that the transport policy accepts. */
export const SEEDED_HTTPS_PEER = {
  node_id: 'e2e-seed-peer',
  addr: 'https://e2e-seed-peer.internal:7700',
} as const;

/** A second https:// peer added through the UI during the add-success spec. */
export const UI_ADDED_HTTPS_PEER = {
  node_id: 'e2e-ui-peer',
  addr: 'https://e2e-ui-peer.internal:7700',
} as const;

/** A cleartext peer the backend must refuse with its transport-policy message. */
export const CLEARTEXT_HTTP_PEER = {
  node_id: 'e2e-http-peer',
  addr: 'http://e2e-http-peer.internal:7700',
} as const;

export function expectedCleartextRefusalMessage(peer: { node_id: string; addr: string }): string {
  return `Refusing replication peer ${peer.node_id} at ${peer.addr}: authenticated analytics query `
    + 'fan-out forwards caller API keys and the peer origin is cleartext http://, which would '
    + 'send the peer credential in plaintext. Move the peer to https://, or set '
    + 'FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1 to keep the cleartext peer.';
}

export function waitForAddClusterPeerResponse(page: Page): Promise<Response> {
  return page.waitForResponse((response) => (
    response.request().method() === 'POST'
    && new URL(response.url()).pathname === '/internal/cluster/peers'
  ));
}

export interface ClusterPeerStatusRow {
  peer_id: string;
  addr: string;
  status: string;
  last_success_secs_ago: number | null;
}

export interface HaClusterStatus {
  node_id: string;
  replication_enabled: true;
  peers_total: number;
  peers_healthy: number;
  peers: ClusterPeerStatusRow[];
}

interface StandaloneClusterStatus {
  node_id: string;
  replication_enabled: false;
}

type ClusterStatus = HaClusterStatus | StandaloneClusterStatus;

export interface SeededClusterPeerState {
  status: HaClusterStatus;
  expectedPresentation: {
    statusLabel: 'Never Contacted';
    lastSuccessLabel: 'Never';
  };
}

export interface ClusterPeerOracle {
  confirmUiAddedPeerInClusterStatus: () => Promise<void>;
  confirmSeededPeerInClusterStatus: () => Promise<void>;
  confirmSeededPeerAbsentFromClusterStatus: () => Promise<void>;
  confirmCleartextPeerRefusal: (response: Response) => Promise<void>;
}

/** An Arrange-phase harness failure, distinct from a product behavior failure. */
export class SetupInfrastructureError extends Error {
  override readonly name = 'SetupInfrastructureError';
}

type ClusterPeerFixtures = {
  seededCluster: SeededClusterPeerState;
  clusterPeerOracle: ClusterPeerOracle;
};

/** Fetch and validate the current cluster status payload. */
export async function getClusterStatus(request: APIRequestContext): Promise<ClusterStatus> {
  const response = await request.get(CLUSTER_STATUS_PATH, { headers: API_HEADERS });
  expect(response.status(), 'GET /internal/cluster/status must return 200').toBe(200);
  const body = (await response.json()) as ClusterStatus;
  expect(typeof body.node_id, 'cluster status must carry a node_id').toBe('string');
  expect(typeof body.replication_enabled, 'cluster status must carry replication_enabled').toBe('boolean');
  return body;
}

function assertHaClusterStatusShape(status: ClusterStatus): asserts status is HaClusterStatus {
  if (
    typeof status.peers_total !== 'number'
    || typeof status.peers_healthy !== 'number'
    || !Array.isArray(status.peers)
  ) {
    throw new SetupInfrastructureError(
      'Cluster peer specs require /internal/cluster/status to include peers_total, '
      + 'peers_healthy, and peers[] when replication_enabled=true.',
    );
  }

  for (const [index, peer] of status.peers.entries()) {
    if (
      !peer
      || typeof peer.peer_id !== 'string'
      || typeof peer.addr !== 'string'
      || typeof peer.status !== 'string'
      || (
        peer.last_success_secs_ago !== null
        && typeof peer.last_success_secs_ago !== 'number'
      )
    ) {
      throw new SetupInfrastructureError(
        `Cluster peer specs require /internal/cluster/status peers[${index}] to carry `
        + 'peer_id, addr, status, and nullable last_success_secs_ago.',
      );
    }
  }
}

/**
 * Fail setup with an explicit message when the target backend is not a
 * replication-enabled HA harness. A `replication manager is not configured`
 * add-peer failure downstream is NOT evidence that these specs caught missing
 * UI, so this preflight makes the missing harness a loud, unambiguous setup
 * failure instead.
 */
export async function requireHaHarness(request: APIRequestContext): Promise<HaClusterStatus> {
  const status = await getClusterStatus(request);
  if (!status.replication_enabled) {
    throw new SetupInfrastructureError(
      'Cluster peer specs require a replication-enabled HA backend, but the target reports '
      + `standalone mode (node_id=${status.node_id}). Start the backend with replication `
      + 'configured before running tests/e2e-ui/full/cluster_peers.spec.ts.',
    );
  }
  assertHaClusterStatusShape(status);
  return status;
}

async function confirmPeerInClusterStatus(
  request: APIRequestContext,
  peer: { node_id: string; addr: string },
): Promise<void> {
  const status = await requireHaHarness(request);
  const persistedPeer = status.peers.find((row) => row.peer_id === peer.node_id);
  expect(
    persistedPeer,
    `${peer.node_id} must appear in GET /internal/cluster/status`,
  ).toBeDefined();
  expect(
    persistedPeer?.addr,
    `${peer.node_id} must retain its address in GET /internal/cluster/status`,
  ).toBe(peer.addr);
}

async function confirmPeerAbsentFromClusterStatus(
  request: APIRequestContext,
  peer: { node_id: string },
): Promise<void> {
  const status = await requireHaHarness(request);
  expect(
    status.peers.some((row) => row.peer_id === peer.node_id),
    `${peer.node_id} must be absent from GET /internal/cluster/status`,
  ).toBe(false);
}

/** Independent backend oracle for the peer added through the browser. */
export async function confirmUiAddedPeerInClusterStatus(
  request: APIRequestContext,
): Promise<void> {
  await confirmPeerInClusterStatus(request, UI_ADDED_HTTPS_PEER);
}

/** Independent backend oracle for a cancelled removal. */
export async function confirmSeededPeerInClusterStatus(
  request: APIRequestContext,
): Promise<void> {
  await confirmPeerInClusterStatus(request, SEEDED_HTTPS_PEER);
}

/** Independent backend oracle for the confirmed browser removal. */
export async function confirmSeededPeerAbsentFromClusterStatus(
  request: APIRequestContext,
): Promise<void> {
  await confirmPeerAbsentFromClusterStatus(request, SEEDED_HTTPS_PEER);
}

/**
 * Confirm the browser submission reached the backend transport-policy owner.
 * The response envelope and unchanged membership distinguish a server refusal
 * from client-side validation that never called POST /internal/cluster/peers.
 */
export async function confirmCleartextPeerRefusal(
  request: APIRequestContext,
  response: Response,
  peersTotalBeforeAttempt: number,
): Promise<void> {
  const expectedMessage = expectedCleartextRefusalMessage(CLEARTEXT_HTTP_PEER);
  expect(response.status(), 'POST /internal/cluster/peers must return 400').toBe(400);
  expect(
    await response.json(),
    'POST /internal/cluster/peers must return the backend cleartext-refusal envelope',
  ).toEqual({ message: expectedMessage, status: 400 });

  const status = await requireHaHarness(request);
  expect(status.peers_total, 'a refused cleartext peer must not change membership').toBe(
    peersTotalBeforeAttempt,
  );
  expect(
    status.peers.some((row) => row.peer_id === CLEARTEXT_HTTP_PEER.node_id),
    'the refused cleartext peer must be absent from GET /internal/cluster/status',
  ).toBe(false);
}

/** Add one peer through the product API and assert the success contract. */
export async function addClusterPeer(
  request: APIRequestContext,
  peer: { node_id: string; addr: string },
): Promise<{ node_id: string; addr: string; peers_total: number }> {
  const response = await request.post(CLUSTER_PEERS_PATH, {
    headers: API_HEADERS,
    data: peer,
  });
  expect(response.status(), `POST /internal/cluster/peers must accept ${peer.node_id}`).toBe(200);
  const body = (await response.json()) as { node_id: string; addr: string; peers_total: number };
  expect(body.node_id, 'add-peer response echoes the node_id').toBe(peer.node_id);
  expect(typeof body.peers_total, 'add-peer response carries peers_total').toBe('number');
  return body;
}

/** Remove one peer, tolerating a 404 so cleanup is idempotent. */
export async function removeClusterPeer(
  request: APIRequestContext,
  nodeId: string,
): Promise<void> {
  const response = await request.delete(`${CLUSTER_PEERS_PATH}/${encodeURIComponent(nodeId)}`, {
    headers: API_HEADERS,
  });
  expect(
    [200, 404].includes(response.status()),
    `DELETE /internal/cluster/peers/${nodeId} must resolve to 200 or 404`,
  ).toBe(true);
}

/**
 * Seed one https:// peer into runtime membership and confirm it is visible in
 * status before the spec asserts against the rendered page.
 */
export async function seedClusterPeer(
  request: APIRequestContext,
  peer: { node_id: string; addr: string } = SEEDED_HTTPS_PEER,
): Promise<SeededClusterPeerState> {
  await requireHaHarness(request);
  await removeClusterPeer(request, peer.node_id);
  await addClusterPeer(request, peer);
  const status = await requireHaHarness(request);
  const seededPeer = status.peers.find((row) => row.peer_id === peer.node_id);
  expect(seededPeer, `seeded peer ${peer.node_id} must appear in cluster status`).toBeDefined();
  expect(seededPeer?.status, 'a newly seeded unreachable peer starts never_contacted').toBe('never_contacted');
  expect(seededPeer?.last_success_secs_ago, 'a never-contacted peer has no last success').toBeNull();

  // The fixture establishes one deterministic backend state, then supplies its
  // literal UX contract. It intentionally does not reproduce Cluster.tsx's
  // status mapping or elapsed-time formatting algorithms.
  return {
    status,
    expectedPresentation: {
      statusLabel: 'Never Contacted',
      lastSuccessLabel: 'Never',
    },
  };
}

/** Remove every peer this fixture may have introduced so specs stay isolated. */
export async function cleanupSeededPeers(request: APIRequestContext): Promise<void> {
  await requireHaHarness(request);
  for (const peer of [SEEDED_HTTPS_PEER, UI_ADDED_HTTPS_PEER, CLEARTEXT_HTTP_PEER]) {
    await removeClusterPeer(request, peer.node_id);
  }
}

/**
 * Authenticated browser fixture plus request-bound cluster setup and oracles.
 * Specs receive callbacks, never Playwright's raw request fixture, keeping all
 * direct backend access in this fixture owner.
 */
export const test = authenticatedTest.extend<ClusterPeerFixtures>({
  seededCluster: async ({ request }, use) => {
    const seededCluster = await seedClusterPeer(request, SEEDED_HTTPS_PEER);
    try {
      await use(seededCluster);
    } finally {
      await cleanupSeededPeers(request);
    }
  },

  clusterPeerOracle: async ({ request, seededCluster }, use) => {
    await use({
      confirmUiAddedPeerInClusterStatus: () => confirmUiAddedPeerInClusterStatus(request),
      confirmSeededPeerInClusterStatus: () => confirmSeededPeerInClusterStatus(request),
      confirmSeededPeerAbsentFromClusterStatus: () => confirmSeededPeerAbsentFromClusterStatus(request),
      confirmCleartextPeerRefusal: (response) => (
        confirmCleartextPeerRefusal(request, response, seededCluster.status.peers_total)
      ),
    });
  },
});
