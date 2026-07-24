# Operations Consumer Contract

This is the engine-owned R5 consumer contract for fjcloud operations surfaces.
It records the wire behavior that fjcloud may consume without copying screen
requirements into this repository.

Fjcloud screen-spec ownership remains in the fjcloud repo:

- `docs/screen_specs/system.md`
- `docs/screen_specs/cluster.md`
- `docs/screen_specs/snapshots.md`

## Runtime Owners

- `/health`: `engine/flapjack-http/src/handlers/health.rs::health`
- `/internal/status`: `engine/flapjack-http/src/handlers/internal.rs::replication_status`
- `/internal/cluster/status`: `engine/flapjack-http/src/handlers/internal.rs::cluster_status`
- `/internal/snapshots/capability`: `engine/flapjack-http/src/handlers/snapshot.rs::snapshot_capability`
- OpenAPI generation: `engine/flapjack-http/src/openapi.rs`
- Committed OpenAPI artifact: `engine/docs2/openapi.json`

## `/health`

The System consumer polls `/health` every 5 seconds.

Successful responses expose exactly these 14 top-level keys:

- `status`
- `version`
- `build`
- `uptime_secs`
- `capabilities`
- `active_writers`
- `max_concurrent_writers`
- `facet_cache_entries`
- `facet_cache_cap`
- `heap_allocated_mb`
- `system_limit_mb`
- `pressure_level`
- `allocator`
- `tenants_loaded`

Known bound: `build_profile` remains a fjcloud consumer/spec mismatch and is
not a top-level `/health` field. Public build metadata is nested under `build`,
which exposes only `schemaVersion`, `version`, `profile`, and `capabilities`.
The unauthenticated `/health` response intentionally omits revision, dirty
state, workspace digest, target triple, and feature list fingerprinting data.

## `/internal/status`

The Replication consumer polls `/internal/status` every 10 seconds.

Successful responses expose replication and storage telemetry owned by
`ReplicationStatusResponse`: `node_id`, `replication_enabled`, `peer_count`,
`ssl_renewal`, `storage_total_bytes`, `tenant_count`, and
`vector_memory_bytes`.

Standalone nodes have `replication_enabled: false`, `peer_count: 0`, and use
the `FLAPJACK_NODE_ID` value when configured. Without that env var, the
standalone fallback node ID is `"unknown"`.

## `/internal/cluster/status`

The Cluster consumer polls `/internal/cluster/status` every 5 seconds.

Successful responses are discriminated by `replication_enabled`:

- `replication_enabled: false`: standalone response. It has `node_id` and
  `peers`; it does not include `peers_total` or `peers_healthy`.
- `replication_enabled: true`: HA response. It has `node_id`,
  backend-owned `peers_total`, backend-owned `peers_healthy`, and `peers`.

Known bound: an empty standalone cluster returns `peers: []`.

## `/internal/snapshots/capability`

Successful responses describe the configured snapshot backend capability. The
current backend is `s3`, with these consumer states:

- `not_configured`: no S3 snapshot configuration was found.
- `configured_unverified`: S3 config is present; credentials, bucket existence,
  or reachability have not been checked.

The `bucket` field is always present. It is `null` when no bucket is configured
and contains the configured bucket name otherwise. It is not a reachability
proof.

## Consumer Error States

Request or parse failure is a consumer error state, not an alternate successful
branch. Consumers should surface failures separately from these successful wire
contracts instead of inferring additional success variants.
