# Cluster Page

## cluster-1: Standalone node contract and card
1. Go to /cluster
2. See the "Cluster" heading
3. Confirm payload branch is standalone (`replication_enabled: false`, `peers: []`)
4. See the Standalone mode card (`cluster-standalone-state`)
5. See Node ID rendered from `node_id`
6. See Mode rendered as "Standalone mode"
7. See reassurance that single-node mode is healthy and expected for this instance

## cluster-2: HA failover visibility in peer table
1. Go to /cluster with HA payload (`replication_enabled: true`)
2. See summary cards for Node ID, Peers Total, and Peers Healthy
3. See the Peer Health table (`cluster-peer-table`)
4. Confirm status badges map exactly to payload values:
5. `healthy` -> "Healthy"
6. `stale` -> "Stale"
7. `unhealthy` -> "Unhealthy"
8. `circuit_open` -> "Circuit Open"
9. `never_contacted` -> "Never Contacted"

## cluster-3: HA recovery state reflects latest payload
1. Go to /cluster with HA payload where a peer previously unhealthy is now `healthy`
2. See `cluster-peer-status-<peer_id>` render "Healthy" after refresh
3. Confirm Peer Health table rows still render from `peers[]` without client-side summary remapping
4. Confirm `peers_total` and `peers_healthy` cards match backend values directly

## cluster-4: Refresh cadence and Last Success formatting
1. Go to /cluster with HA payload
2. Confirm `useClusterStatus` polling contract uses 5-second refresh (`refetchInterval: 5000`)
3. Wait approximately 5 seconds and confirm cluster query refreshes
4. Confirm `last_success_secs_ago === null` renders "Never"
5. Confirm numeric values render as relative time (`<1s ago`, `Ns ago`, `Nm ago`, `Nh ago`)

## cluster-5: Fetch-error state is operator-visible
1. Go to /cluster with `/internal/cluster/status` request failure
2. See the error card (`cluster-error-state`)
3. See "Failed to fetch cluster status"
4. See the surfaced request error message text

## cluster-6: HA enabled with zero peer rows
1. Go to /cluster with HA payload (`replication_enabled: true`) and `peers: []`
2. See the HA summary state (`cluster-ha-state`)
3. See the HA empty-state card (`cluster-ha-empty-state`)
4. See "HA is enabled but no peer health rows are available yet."
