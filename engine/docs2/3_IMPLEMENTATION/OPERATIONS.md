# Operations & Release Discipline

This document is the canonical operator-facing guide for release proof,
upgrade/rollback discipline, and day-2 runbooks in the open-source Flapjack
repo.

- For deployment shapes and example topologies, see [DEPLOYMENT.md](./DEPLOYMENT.md).
- For environment variables and defaults, see [OPS_CONFIGURATION.md](./OPS_CONFIGURATION.md).
- For security contracts (production auth floor, admin-only surfaces, request correlation), see [SECURITY_BASELINE.md](./SECURITY_BASELINE.md).
- For shipped/readiness status, see [../FEATURES.md](../FEATURES.md).

## Cross-references

- [DISASTER_RECOVERY.md](./DISASTER_RECOVERY.md)

## What is already verified

The repo already has executable proof surfaces for the core operational paths:

| Concern | Current proof surface |
|---|---|
| Single-node Linux/systemd deployment | `engine/examples/systemd/README.md` plus the 2026-03-26 live VPS verification recorded in `FEATURES.md` |
| Crash/restart durability for acknowledged writes | `cargo test -p flapjack-server --test crash_durability_test -- acknowledged_batch_write_remains_searchable_after_crash_restart` |
| Restart catch-up before a restarted replica serves traffic | `cargo test -p flapjack --test test_replication -- test_restart_catches_up_before_serving` |
| File-snapshot export/import DR contract under active writes | `engine/tests/test_snapshot_restore_under_load.rs` plus [`DISASTER_RECOVERY.md`](./DISASTER_RECOVERY.md) |
| Snapshot upload/list/restore/scheduled backup/auto-restore | `engine/examples/s3-snapshot/test_snapshots.sh` |
| Startup failure (env, auth, lock, blank key) | `cargo test -p flapjack-server --test env_mode_test` |
| Admin-key rotation and recovery | `cargo test -p flapjack-server --test admin_key_test` |
| Metrics auth contract | `cargo test -p flapjack-http router::tests::metrics_returns_200_with_admin_key_only -- --exact` |
| Release-to-release data-dir handoff | `engine/tests/upgrade_smoke.sh --old-bin <old> --new-bin <new>` |

The main remaining gap is not whether these flows exist. It is how much
long-duration and release-to-release evidence has been captured around them.

Canonical ownership reminder: deployment topology/catalog ownership lives in
[DEPLOYMENT.md](./DEPLOYMENT.md#deployment-surfaces-in-this-repo), and verified
example-path catalog ownership lives in
[OPS_CONFIGURATION.md](./OPS_CONFIGURATION.md#verified-example-paths).

## Release proof pack

Each release proof pack should record the same four things:

1. exact dev commit and exact staging/public commit
2. exact focused validations run
3. staged/public CI result for that commit
4. known acceptable caveats that still remain true

Keep the proof pack concise. Prefer links to the canonical docs and executable
proof surfaces over restating their contents.

## Upgrade smoke

Flapjack does not yet maintain a versioned historical compatibility matrix across
arbitrary old releases. The current bar is a repeatable upgrade smoke against a
known-good prior build on the same data directory.

### Minimum upgrade smoke

1. Stop the current service cleanly.
2. Capture the currently running binary version/commit and back up the data dir.
3. Stage the new binary without changing the data directory.
4. Start the new binary or restart the systemd service.
5. Verify `GET /health` and `GET /health/ready`.
6. Run one known-good search against pre-existing data.
7. Run one write path and wait for its task to publish.
8. Load `/dashboard` and confirm it renders.
9. Record the exact commands, commits, and any caveats.

For the documented Linux/systemd path, use the same deployment contract in
`engine/examples/systemd/README.md`: binary on disk, `EnvironmentFile`,
`systemctl enable --now`, then health/readiness verification.

The repo also ships a reusable focused harness:

```bash
engine/tests/upgrade_smoke.sh --old-bin <old-binary> --new-bin <new-binary>
```

That script exercises:

- pre-existing data dir still loads
- searches still work after upgrade
- writes still work after upgrade
- `/health` and `/health/ready` still succeed
- `/dashboard` still serves HTML on the upgraded binary

## Rollback guidance

Rollback currently means restoring the previous known-good binary and, when
needed, restoring operator-controlled data taken before the upgrade.

### What is safe to assume today

- Forward startup on an existing data directory is exercised by normal restart
  and upgrade-smoke workflows.
- Backward compatibility of an upgraded data directory with an older binary is
  **best-effort**, not a guaranteed contract across arbitrary historical builds.
- Operators should treat a pre-upgrade snapshot or filesystem backup as the
  rollback boundary.

### Recommended rollback procedure

1. Stop the upgraded process.
2. Restore the previous binary or package.
3. If no post-upgrade writes were accepted, restart on the same data dir and
   verify health/readiness.
4. If post-upgrade writes were accepted and rollback is required, restore the
   pre-upgrade snapshot or backup before restarting the older binary.
5. Re-run the same search + write + readiness checks used in upgrade smoke.

## Runbooks

### Scenario: Startup failure from data-dir or `.admin_key` permissions

**Symptom:** Startup exits with `ERROR: Failed to create data directory <path>: Permission denied`, or `❌ Error: Failed to save admin key: Permission denied`.
**Diagnosis:** The service user cannot read/write `FLAPJACK_DATA_DIR`, its parent directory, or `.admin_key`.
**Recovery:** Verify ownership/mode for the data dir and `.admin_key`, fix permissions, and restart.
**Test (where applicable):** `cargo test -p flapjack-server --test env_mode_test`

### Scenario: Startup failure from bind address or port conflict

**Symptom:** Startup exits before the banner with an OS bind error such as `Address already in use`.
**Diagnosis:** `FLAPJACK_BIND_ADDR` or `--bind-addr` points to a listener that is already occupied.
**Recovery:** Free the conflicting listener or choose a different bind address, then restart and confirm the resolved bind URL prints in startup output.
**Test (where applicable):** `cargo test -p flapjack-server --test env_mode_test`

### Scenario: Startup failure from missing or short production admin key

**Symptom:** Startup prints `ERROR: FLAPJACK_ADMIN_KEY is required in production mode.` or `ERROR: FLAPJACK_ADMIN_KEY must be at least 16 characters in production.`
**Diagnosis:** Production mode is active and `FLAPJACK_ADMIN_KEY` is absent or shorter than the required floor.
**Recovery:** Set `FLAPJACK_ADMIN_KEY` once in your secret store or environment file with length >= 16, then restart.
**Test (where applicable):** `cargo test -p flapjack-server --test env_mode_test`

### Scenario: Startup failure from `.process.lock` contention

**Symptom:** Startup prints `ERROR: Data directory already in use: <data_dir>/.process.lock. Use unique --data-dir per instance.`
**Diagnosis:** Multiple processes are attempting to use the same `--data-dir`.
**Recovery:** Ensure only one process owns that data dir (or assign unique dirs per instance), then restart after the conflicting process exits.
**Test (where applicable):** `cargo test -p flapjack-server --test env_mode_test`

### Scenario: Startup failure from blank or unreadable `.admin_key`

**Symptom:** Startup prints `❌ Error: .admin_key file <path> is empty` or `❌ Error: Failed to read .admin_key file <path>: <io error>` and includes `Run: flapjack --data-dir <path> reset-admin-key`.
**Diagnosis:** The local admin-key file exists but is empty or unreadable.
**Recovery:** Stop the server, run `flapjack --data-dir <path> reset-admin-key`, restart, and re-verify admin-authenticated endpoints.
**Test (where applicable):** `cargo test -p flapjack-server --test env_mode_test -- blank_admin_key_file_prints_explicit_reset_hint`

### Scenario: Readiness returns canonical 503 while liveness remains 200

**Symptom:** `/health` returns 200 while `/health/ready` returns `{"message":"Service unavailable","status":503}` with readiness probe failure logs.
**Diagnosis:** Tenant visibility/searchability checks failed for the current data dir.
**Recovery:** Verify data-dir readability and tenant-directory integrity, run a direct tenant query when visible tenants exist, and keep automation keyed to the canonical 503 body.
**Test (where applicable):** `cargo test -p flapjack-http handlers::readiness::tests::ready_returns_canonical_503_when_visible_tenant_discovery_fails -- --exact`

### Scenario: Readiness returns ready with no visible tenants

**Symptom:** `/health/ready` returns `{"ready":true}` and no visible tenant directories are present.
**Diagnosis:** The data dir is empty/new and no tenant needs search validation yet.
**Recovery:** Treat this as expected; escalate only if tenant directories exist but readiness remains 503.
**Test (where applicable):** `cargo test -p flapjack-http handlers::readiness::tests::ready_returns_ready_when_no_visible_tenants_exist -- --exact`

### Scenario: Disk-full or memory-pressure triage

**Symptom:** Elevated write-path `429` rates, rising request latency, or storage exhaustion near the data-dir/snapshot target.
**Diagnosis:** Resource pressure is constraining write throughput or service headroom.
**Recovery:** Check free disk space, inspect `/metrics` for latency/oplog progress, and verify configured memory bounds used by load tests before scaling or throttling traffic.
**Test (where applicable):** `engine/loadtest/soak_proof.sh`

### Scenario: Restore the latest snapshot for an index

**Symptom:** `POST /1/indexes/<index>/restore` with an empty body returns `{"status":"restored","key":"snapshots/<index>/...tar.gz","size_bytes":<n>}`.
**Diagnosis:** The service accepted a latest-snapshot restore request for the requested index.
**Recovery:** Use latest restore for standard rollback, then run a known query immediately to verify expected hits.
**Test (where applicable):** `engine/examples/s3-snapshot/test_snapshots.sh`

### Scenario: Restore by explicit snapshot key

**Symptom:** `POST /1/indexes/<index>/restore` with `{"key":"snapshots/<index>/<file>.tar.gz"}` returns a matching restored key.
**Diagnosis:** A specific backup point under the target index prefix was selected successfully.
**Recovery:** Use explicit keys when rolling back to a specific point and keep keys under the same index prefix.
**Test (where applicable):** `engine/examples/s3-snapshot/test_snapshots.sh`

### Scenario: Cross-index restore key is rejected

**Symptom:** `POST /1/indexes/<index>/restore` with a key outside `snapshots/<index>/` returns `400 {"message":"key must reference a snapshot for the requested index","status":400}`.
**Diagnosis:** The restore key does not belong to the requested index namespace.
**Recovery:** Select a key from `GET /1/indexes/<index>/snapshots` and retry restore.
**Test (where applicable):** `engine/examples/s3-snapshot/test_snapshots.sh`

### Scenario: Invalid snapshot import payload is sanitized

**Symptom:** `POST /1/indexes/<index>/import` with corrupt bytes returns `500 {"message":"Internal server error","status":500}` without tar/gzip parser internals.
**Diagnosis:** Import payload is corrupt and rejected by snapshot parsing layers.
**Recovery:** Re-export/re-download the artifact and retry; verify existing indexed data remains queryable after the failed import.
**Test (where applicable):** `cargo test -p flapjack --test test_snapshot_import_failure_contract`

### Scenario: Empty data dir triggers S3 auto-restore on startup

**Symptom:** Startup logs include `Empty data dir detected, attempting S3 auto-restore...` and successful restore logs include `S3 auto-restore: restored <tenant> from <key> (<bytes> bytes)`.
**Diagnosis:** Auto-restore bootstrap is running for an empty data dir with S3 snapshot configuration.
**Recovery:** Treat as expected bootstrap behavior and verify with a known query after startup completes.
**Test (where applicable):** `engine/examples/s3-snapshot/test_snapshots.sh`

### Scenario: Diagnose node-level replication status

**Symptom:** `curl -s -H "x-algolia-api-key: $FLAPJACK_ADMIN_KEY" https://<node>/internal/status` reports `node_id`, `replication_enabled`, `peer_count`, `storage_total_bytes`, and `tenant_count`. For localhost-only debugging without TLS termination, substitute `http://127.0.0.1:7700`.
**Diagnosis:** `replication_enabled=false` with `peer_count=0` indicates standalone mode, not peer failure.
**Recovery:** Confirm replication mode first; if replication is enabled and data is stale, continue with peer reachability and catch-up checks.
**Test (where applicable):** `cargo test -p flapjack-http handlers::internal::tests::status_includes_storage_total_and_tenant_count -- --exact`

### Scenario: Interpret peer health statuses during catch-up

**Symptom:** Peer status reports `never_contacted`, `healthy`, `stale`, `unhealthy`, or `circuit_open`.
**Diagnosis:** `never_contacted` and `circuit_open` indicate bootstrap risk before safe catch-up.
**Recovery:** Resolve network/auth/timeout issues before restarting replicas that must catch up.
**Test (where applicable):** `cargo test -p flapjack-http handlers::internal::tests::cluster_status_ha_returns_peer_list_with_correct_shape -- --exact`; `cargo test -p flapjack-replication manager::tests::test_peer_statuses_initially_never_contacted -- --exact`

### Scenario: Replica bootstrap refusal when peer is unreachable

**Symptom:** Restarted replica refuses to serve and logs peer fetch failures (`Failed to fetch tenants`, `Failed to fetch ops`, or circuit-breaker trips), including `pre-serve catch-up timed out after <n>s; refusing to serve stale data`.
**Diagnosis:** Pre-serve catch-up cannot complete because required peer routes are unreachable.
**Recovery:** Restore peer reachability (address/auth/network) first, then restart the replica.
**Test (where applicable):** `cargo test -p flapjack --test test_replication -- test_restart_refuses_to_serve_when_peer_is_unreachable`

### Scenario: Peer-failed amplification exceeds acceptance bound

**Symptom:** `engine/loadtest/tests/ha_peer_failed_amplification_acceptance.sh` reports `raw_peer_down_count` outside the absolute window `MIN_PEER_DOWN <= raw_peer_down_count <= MAX_PEER_DOWN` (calibrated `MAX_PEER_DOWN_LITERAL=94`) from the probe at `engine/_dev/s/manual-tests/ha-peer-failed-amp-probe.sh`.
**Diagnosis:** The induced peer-down window emitted either too few or too many `Failed to send request to node-b` events for the calibrated bound. The PL-12 v2 contract uses an absolute peer-down count, not a ratio against baseline — see `docs/research/pl12_stage1_baseline.md` for the calibration formula (`CV > 0.30` high-variance fallback, `ceil(max(max_observed * 2, 50))`) and `docs/research/pl12v2_stage2_tune_plan.md` for why `DEFAULT_FAILURE_THRESHOLD=3` is intentionally retained rather than retuned in response to amplification deviations.
**Recovery:** Re-run the probe against a stable emitter set with re-anchored windows; if deviation persists, recalibrate the bound from a fresh sample using the formula in `docs/research/pl12_stage1_baseline.md` before changing circuit-breaker defaults.
**Test (where applicable):** `bash engine/loadtest/tests/ha_peer_failed_amplification_acceptance.sh`

### Scenario: Rotate admin key online

**Symptom:** `POST /internal/rotate-admin-key` returns `{"key":"fj_admin_<32hex>","message":"Admin key rotated"}`, old key fails admin routes, and new key succeeds.
**Diagnosis:** Admin key rotation completed and old credentials are immediately invalidated.
**Recovery:** Store the new key immediately, update automation to the new key, and verify old-key rejection plus new-key success on `/metrics`.
**Test (where applicable):** `cargo test -p flapjack-server --test admin_key_test`

### Scenario: Recover after admin key is lost

**Symptom:** `flapjack --data-dir <path> reset-admin-key` prints a new `fj_admin_...` key.
**Diagnosis:** Local key recovery path is available for the configured data dir.
**Recovery:** Stop the service, run reset-admin-key, store the emitted key securely, restart, and re-verify admin-only routes.
**Test (where applicable):** `cargo test -p flapjack-server --test admin_key_test`

### Scenario: Admin key recovery fails because `keys.json` is missing

**Symptom:** `flapjack --data-dir <path> reset-admin-key` exits non-zero with `ERROR: No keys.json found. Start the server first to initialize.`
**Diagnosis:** Key store initialization has not happened yet for that data dir.
**Recovery:** Start the server once to initialize `keys.json`, then rerun `reset-admin-key`.
**Test (where applicable):** `cargo test -p flapjack-server --test admin_key_test`

### Scenario: Recover a partial GHCR release after Docker publish failure

**Symptom:** release automation published tag/release assets but Docker publish failed with `denied: permission_denied: write_package`, often when GHCR package metadata shows `repository:null`.
**Diagnosis:** In `.github/workflows/release.yml`, the `release` job can complete before Docker promotion jobs run; a package-authorization failure then leaves a partial release where `v<version>` exists but `ghcr.io/flapjackhq/flapjack:<version>` is missing.
**Recovery:** Keep the existing tag/release and re-dispatch the same workflow with `gh workflow run release.yml -f version=<v>`; do not recreate the git tag. Ensure GHCR auth uses `${{ secrets.GITHUB_TOKEN }}` (the workflow's docker/login owner) so Docker jobs can publish.
**Proof:** `docker run ghcr.io/flapjackhq/flapjack:<v> --version`
**Test (where applicable):** N/A (operator runbook sourced from `.github/workflows/release.yml` release + docker_prepare ownership)

## Runbook behavioral reference

- Security policy ownership for production admin-key floor and admin-only operational surfaces remains in [SECURITY_BASELINE.md](./SECURITY_BASELINE.md).
- Deployment/readiness probe contracts remain in [DEPLOYMENT.md](./DEPLOYMENT.md), and runtime configuration ownership remains in [OPS_CONFIGURATION.md](./OPS_CONFIGURATION.md).
- Under sustained rolling restarts with continuous writes, the nginx-routed example topology keeps availability while converging to a bounded per-node document-count spread; the residual reflects nginx restart-window write loss and is tracked as roadmap PL-8. Narrative seam is ADR [`0004`](decisions/active/0004_ha_convergence_reversal.md); canonical retained evidence is [`engine/loadtest/BENCHMARKS.md`](../../loadtest/BENCHMARKS.md).
- For PL-10 write-path saturation revalidation, operators should use [`engine/loadtest/BENCHMARKS.md`](../../loadtest/BENCHMARKS.md) as the numeric SSOT and [`engine/docs/research/pl10_stage6_dual_scenario_classification.md`](../../docs/research/pl10_stage6_dual_scenario_classification.md) for Stage 6 dual-scenario verdict details.
- Engine deploy-model fact mirror: flapjack reaches fjcloud via a `Packer AMI`, and `/version.dev_sha` is fjcloud control-plane SHA rather than engine version. Canonical owner remains [.scrai/rules.md](../../../.scrai/rules.md).

## Idempotency contract

Write paths admit an optional `X-Flapjack-Idempotency-Key` header that lets
clients retry safely without double-applying a write. Behavior is owned by
[`engine/flapjack-http/src/idempotency.rs`](../../flapjack-http/src/idempotency.rs)
and ADR [`0005`](decisions/active/0005_nginx_restart_window_write_recovery.md).

| Surface | Current contract |
|---|---|
| Header name | `X-Flapjack-Idempotency-Key` (case-insensitive per HTTP). |
| TTL | Default 300s; configurable via `FLAPJACK_IDEMPOTENCY_TTL_SECS` (min 1s). |
| Scope | Per application + index segment + idempotency key; node-local cache ownership. |
| Hit semantics | Cache hit replays the original `2xx` response and adds `X-Flapjack-Idempotency-Replayed: true`. |
| Restart | With `FLAPJACK_IDEMPOTENCY_PERSISTENT=true`, same-node restarts replay the cached response body and original `taskID`. |
| Add durability | `add_documents_durable` waits for `await_task_terminal`/`wait_for_write_durable` before acking success, so accepted add writes are durable-commit acknowledgements rather than queue-only acceptance. |
| Delete durability | PL-14 is done: delete handlers route through the durable delete path instead of unbounded `delete_documents_sync` calls. Behavioral ownership remains with `delete_documents_durable` in `engine/src/index/manager/write.rs`, delete callers in `objects/batch.rs`, `objects/mod.rs`, and `replicas.rs`, plus the non-terminal task eviction guard in `engine/src/index/manager/mod.rs`. |
| 429 / 503 | Transient errors include `Retry-After: 1`; clients SHOULD retry with the same key. |

### Known limitations (paid-beta posture)

- **Same key, different body**: the cache key is the header value only. Sending the
  same `X-Flapjack-Idempotency-Key` with a different body within the TTL window
  returns the original response and silently drops the new body. Clients SHOULD
  generate a fresh idempotency key per unique request. Stricter Stripe-style
  body-matching (return `409` on mismatch) is a known gap not yet captured in
  an ADR open question; track as a v1.0.x post-beta follow-up.
- **Cross-node durability**: replay persistence is node-local SQLite. This does
  not provide replication-aware cross-node replay guarantees; that remains
  explicitly deferred in ADR-0005.
- **Multi-index batch envelopes**: idempotency replay is per-request, not per
  operation. A batch envelope with one new + one previously-applied operation
  will replay the original batch response; it will not partially execute.
  Tracked as ADR-0005 Open Question 3 ("Multi-index batch semantics:
  idempotency per operation or per batch envelope?").

### Recommended client retry pattern

1. Generate a fresh idempotency key per logical write attempt (UUID v4 is fine).
2. Re-use that key only when retrying the *same* request after a transport
   error, 429, or 503.
3. Honor `Retry-After` (currently always `1s` on transient errors).
4. Treat `X-Flapjack-Idempotency-Replayed: true` as confirmation of a prior
   successful execution; do not re-emit the request.
5. Do not treat queue admission as success: accepted add writes acknowledge only
   after durable commit; on transient `429`/`503`, retry the same request with
   the same idempotency key.

Canonical runtime and proof commands are owned by
[OPS_CONFIGURATION.md](./OPS_CONFIGURATION.md#idempotency-restart-durability-proof).

## Observability contract

Operators can rely on the following today:

| Surface | Current contract |
|---|---|
| `/health` | Liveness probe semantics are owned by [DEPLOYMENT.md](./DEPLOYMENT.md#health-probes) |
| `/health/ready` | Readiness probe semantics are owned by [DEPLOYMENT.md](./DEPLOYMENT.md#health-probes) |
| `/metrics` | Admin-only surface contract is owned by [SECURITY_BASELINE.md](./SECURITY_BASELINE.md#admin-only-operational-surfaces) |
| `x-request-id` | Correlation/error-consistency contract is owned by [SECURITY_BASELINE.md](./SECURITY_BASELINE.md#request-correlation-and-error-consistency) |
| Structured JSON logs | Enabled by `FLAPJACK_LOG_FORMAT=json` |
| Request latency histograms | Exposed under `/metrics`; admin-surface policy remains in [SECURITY_BASELINE.md](./SECURITY_BASELINE.md#admin-only-operational-surfaces) |
| OpenTelemetry export | Available when the server is built with `--features otel` and `OTEL_EXPORTER_OTLP_ENDPOINT` is set |

Not yet shipped:

- richer release-proof packaging beyond checklist/proof-pack docs

## Documentation rule

When operational behavior changes, update the executable proof surface or this
document first, then update higher-level status docs. Avoid creating second
copies of runbooks in roadmap or status files.
