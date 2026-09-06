# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Removed

- **The Flapjack-specific client SDKs are retired; the supported clients are the official Algolia SDKs.** `sdks/` (eleven generated language clients and the WordPress plugin) is deleted with its CI jobs and the `--go` / `--sdk-host-safety` test flags. Sources remain in git history. Registry deprecation and removal actions and the public history tag `sdks-retired-2026-09` remain pending operator work.

## [1.0.16] - 2026-08-20

### Changed

- **Release publication now uses one exact Debbie-rendered public candidate.**
  Public CI validates candidate and main through one stable aggregate gate, while release
  dispatch remains main-only.

## [1.0.15] - 2026-08-20

### Added

- **Paid Beta v3 adds native Algolia-compatible search and Insights Starter endpoints.**
  Official pinned browser clients work directly against the engine while unsupported and
  privileged surfaces remain fail-closed.

## [1.0.14] - 2026-08-19

### Added

- **Algolia migrations can resume interrupted imports without restarting completed work.**
  Provider interruptions are deterministic, source indexes remain unchanged, and replica
  declarations retain explicit translation warnings where Flapjack uses virtual replicas.
- **Query Suggestions now exposes an honest durable build outcome.** Failed source reads preserve
  the prior destination, status distinguishes never-built, running, succeeded, and failed, and
  config deletion cannot race an active build or leak stale build metadata.
- **Paid-beta search controls now cover the migration-relevant relevance and merchandising set.**
  Advanced ranking, typo, highlighting, pagination, language, fallback, and rules behavior is
  persisted and exercised through exact search consequences.

### Fixed

- **Aggregated analytics return correct tenant-scoped counts, averages, top queries, and no-result
  data.** Multi-node analytics merge deterministically without averaging an already-averaged value.

## [1.0.13] - 2026-08-14

### Added

- **Paid Beta v1 deployments can opt into an exact customer data-plane profile.**
  `FLAPJACK_API_PROFILE=paid_beta_v1` keeps authenticated administration and replication
  behavior intact while limiting direct customer credentials to the literal Algolia batch-search
  route, a closed request schema, and one exactly scoped physical index. Unknown profiles,
  authentication-disabled startup, unpublished customer routes, unsupported parameters, and
  invalid or over-broad credentials fail closed. The default profile remains `full`.

## [1.0.12] - 2026-08-12

### Added

- **Typesense migration now requires an explicit source write-freeze attestation.** Preview and
  submit reject missing or false `sourceWriteFrozen` before any source request. The CLI and
  dashboard carry the same contract, the dashboard checkbox is default-unchecked, and successful
  preview/submit behavior is proven against a real pinned Typesense 30.2 source. Resume remains
  unsupported for Typesense.
- **The dashboard `Migrate` screen now reaches all three source providers.** Discovery,
  submit, and status polling drive the real `/1/migrations/{algolia,meilisearch,typesense}`
  routes from one provider descriptor, replacing the Algolia-only compat aliases. A source
  on a loopback or private address renders the outbound-SSRF refusal and names the
  `FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK` / `FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK` opt-in
  instead of a generic error.
- **The console offers a translation-report dry-run before any migration writes anything.**
  A `Preview` step renders the report — total entries, hard rejections, warnings and scope
  gaps, plus every individual entry — and **submit stays disabled while any hard rejection
  is present**. Preview and submit share one request builder, and editing any field that
  changes the request clears the stale report rather than leaving it on screen. This works
  for all three source providers. Neither Meilisearch's nor Typesense's own console offers
  a migration dry-run.

### Fixed

- **Official browser search clients now authenticate with the same public contract as direct
  requests.** The server accepts the Algolia Application ID from
  `x-algolia-application-id` or the requester's `x-algolia-agent` query parameter while keeping
  the search API key scoped and public. Recurring real-client tests exercise vanilla
  `algoliasearch`, React InstantSearch, and Vue InstantSearch against a live Flapjack server and
  require each client to return its own exact seeded record.

- **Typesense export reads the endpoint's real single-stream contract.** The source client no
  longer fabricates `page` / `per_page` windows for `/documents/export`; it consumes one bounded
  JSONL stream, including a final record without a trailing newline, while preserving the existing
  response-byte and item ceilings. A pinned live contract proves 137 exact IDs are captured once
  from one query-free export request.
- **Release binaries now honour the self-hosted-source loopback opt-ins they document.**
  The Meilisearch and Typesense loopback admission seams were `#[cfg(debug_assertions)]`-gated,
  so `FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=1` / `FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1`
  had no effect in a released binary — the console correctly told a self-hosting user to set
  a variable the shipped build ignored. Both providers now use the same production-first,
  explicit-loopback-fallback admission path in debug and release. **The fail-closed default
  is unchanged and is proven by absence of traffic:** with the opt-in absent, or set to any
  value other than `1`, the server makes zero requests to the source and performs zero DNS
  resolutions.
- Unknown fields in an Algolia migration request body are now rejected instead of silently
  ignored, and migration jobs poll to a terminal state rather than stopping at the first
  non-terminal status.
- Loadtest readiness helpers no longer accept a `200` from a listener they did not launch:
  every live caller now supplies the launched server's log and expected bind address.

## [1.0.11] - 2026-08-06

### Security

- **Fixed a panic in the API key parser on malformed input.** A secured API key whose
  decoded bytes did not fall on a UTF-8 character boundary would panic the request handler.
  Malformed keys are now rejected as `400`. Reachable by any unauthenticated caller and
  present in released binaries — operators should upgrade. Availability only: no key
  material, index data, or credentials were exposed.
- **Route authorization is now fail-closed.** A request path matching no ACL rule was
  previously allowed through to its handler; it is now denied. All 124 currently registered
  route/method pairs were swept before the flip, and none changed behavior.
- **Admin credentials are no longer accepted in the query string.** Admin-ACL routes read
  the key only from the `x-algolia-api-key` header, keeping admin keys out of server logs,
  shell history, and proxy access logs. Search-scoped keys keep query-string support, so
  browser and InstantSearch clients are unaffected.
- **Analytics no longer persist full client IP addresses.** Addresses are coarsened before
  the Parquet write — IPv4 to /24, IPv6 to /48. Existing Parquet files remain readable;
  historical data is not rewritten.
- **The Docker image runs as a non-root user.** Fixed `flapjack:flapjack` at UID/GID
  `10001:10001`, with image-time `/data` ownership. The numeric identity is pinned so
  base-image upgrades cannot silently shift ownership of persisted volumes. The container
  now exits non-zero with an actionable message when a pre-existing `/data` volume is not
  writable, instead of starting and failing later.
- **The bundled dashboard no longer ships the known Axios production vulnerability.** Its
  Axios dependency floor is now `^1.18.0`.
- The embedded dashboard, API, Swagger UI, and generated error responses now receive a
  strict default security-header set, including CSP frame protection; HSTS remains tied to
  the separate TLS-listener work.
- Request handling now has configurable execution timeout, queued global concurrency, and
  panic-to-JSON containment boundaries, so timeout and panic paths preserve the canonical
  JSON error shape.
- Security audit events now cover admin authentication and sensitive mutations across API
  keys, index deletion, settings changes, snapshot import and restore, S3 restore, and
  admin-key rotation. Events use bounded actor/action/target/outcome fields and route
  templates so credentials, headers, query strings, and raw payloads are not logged.
- S3 snapshot uploads now request server-side encryption by default with `AES256`, can opt
  into `aws:kms` through `FLAPJACK_S3_SSE`, and verify the returned encryption header
  instead of trusting the request.
- Portable snapshot bytes now support optional AES-256-GCM-SIV encryption through
  `FLAPJACK_SNAPSHOT_KEY_FILE`, with encrypted imports requiring the matching key before
  restore.
- **The binary can terminate TLS and hot-rotate ACME material without restarting.** Static
  PEM startup still fails closed when material is unreadable, malformed, incomplete, or
  mismatched. ACME issuance now persists the private key, publishes each fullchain/key pair
  as one owner-private fsynced generation, and updates subsequent rustls handshakes in the
  running listener without rebinding or changing the process. Malformed renewal material
  keeps the last valid certificate live. Plaintext HTTP-01 challenges remain reachable on
  the TLS listener while other plaintext API requests remain rejected. Reverse-proxy TLS
  remains supported.
- **Replication peers now authenticate with their own credential instead of the admin key.**
  Internal routes resolve to one peer-allowed-or-admin-only decision per method and path,
  so a configured peer credential can serve replication while being refused on
  `POST /internal/cluster/peers`, `DELETE /internal/cluster/peers/{node_id}`, and
  `POST /internal/rotate-admin-key`. The credential is read only from the request header and
  compared in constant time. Admin-key and unauthenticated behavior are unchanged, so a
  rolling upgrade needs no coordinated cutover. ~~Two limits remain: the peer credential is
  optional, so replication configured without one keeps the previous posture, and peer
  transport is still cleartext HTTP.~~ **Both limits are closed — see the next entry.**
- **A replication peer must now have a credential, and that credential is not sent in the
  clear by accident.** Two behavior changes, both fail-closed. (1) A node configured with
  replication peers but no `FLAPJACK_REPLICATION_API_KEY` now **refuses to start** instead of
  silently falling back to the previous posture. (2) A peer origin using cleartext `http://`
  is **refused** whenever a credential would travel over it — across static config, persisted
  config, bootstrap peers, and the runtime `POST /internal/cluster/peers` endpoint — with an
  actionable error naming the peer and the fix. Set
  `FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1` to keep a cleartext peer deliberately (for
  example a trusted private LAN); the refusal message names this escape hatch. Background
  analytics rollup fan-out, which was previously an unauthenticated peer client, now
  authenticates with the same credential. **This is a breaking configuration change for
  anyone running replication without a peer credential or over `http://`** — the rolling
  upgrade path is documented in `engine/docs2/3_IMPLEMENTATION/OPERATIONS.md`.
- **The S3 snapshot list path now fails loudly.** `list_snapshots` checks the ListObjectsV2
  HTTP status before parsing the response body, so a rejected list returns the status
  (`S3 list: HTTP 403`) instead of a misleading XML parse error. Upload and delete already
  did this; all three now have focused rejected-response regressions.
- **The dashboard no longer stores its admin key in browser-readable storage.** Login
  exchanges the key once for a server-owned `HttpOnly; SameSite=Strict` session cookie;
  logout revokes the session server-side, authenticated reloads survive, and legacy
  persisted key material is dropped during store migration. The durable session store
  persists only keyed fingerprints and per-session salted HMAC-SHA256 verifiers in a
  `0600` file — never the session token or admin key. Header-key authentication for SDKs
  and direct API clients is unchanged.

### Reliability

- **Durable admission stays fail-closed under disk exhaustion.** When the write path's disk fills,
  admission rejects the write with HTTP `500` and no rejected write replays into the index after a
  restart. Re-proved by three sequential real-filesystem fill-disk specimens at exact fetched
  `origin/main` SHA `3b11f8216f1d7dccc74262e1b63b3e1603152202`, each accepted with `outcome=PASS`,
  `acknowledged_count=28`, `recovered_count=28`, `rejection_status=500`, and a `source_sha` equal
  to that SHA. Receipt:
  the reviewed private dur1 respecimen and close receipt.

### Fixed (durability and correctness)

- **`looking-similar` recommendations no longer return an empty result solely because
  vector search is unavailable.** Published targets now use vector similarity when an
  embedder is configured and a content/term-similarity fallback otherwise, with no model
  download or new runtime dependency. Legitimate empty vector answers keep their original
  strategy instead of being silently replaced.
- **The dashboard is usable across all 23 authenticated routes at a 390px viewport.** The
  shared shell, header, and API logger now contain their content without document-level
  horizontal overflow; the route audit reports 23 tested and 23 usable, with a negative
  control proving the overflow oracle fails on the former layout.
- **Runtime HA membership can be managed from the Cluster screen.** Operators can add a
  peer and remove one behind a node-scoped confirmation dialog. Served browser tests verify
  both mutations through the runtime cluster-status API and restore the fixture to zero
  peers after cleanup.
- **A write rejected to the client can no longer become visible after restart, including
  when the oplog append itself fails partway.** Previously the durable-acknowledgement
  guarantee was proven only for failures around the append; it is now proven for a failure
  *inside* `append_operations_with_task_id`, after a partial task-tagged row has been flushed
  and synced but before the sequence counter advances — the exact shape an `EIO`/`ENOSPC`
  takes. The contract admits exactly two honest outcomes: the client sees failure and nothing
  replays, or the client sees a durable acknowledgement and the documents are there. Rollback
  stays owned by the existing write-queue compensation path; no second rollback owner exists.
- **Analytics overview no longer aggregates across every index when one index is
  requested.** `GET /2/overview?index=<name>` returned figures blended across all indexes;
  it now aggregates only the requested index.
- **Bulk-replace uploads now have a route-specific, finite execution deadline.** The global
  request timeout could terminate a measured 1-million-record upload before durable spool
  EOF; the route now receives six times the configured timeout (1,800 seconds by default),
  while focused regressions prove slow clients remain bounded and other routes retain the
  global deadline.
- **Scale-ladder readiness no longer accepts an unrelated listener's HTTP 200.** Its health
  wait now requires the launched server's log to confirm the expected bind address before
  proceeding.
- **`flapjack migrate` auto-port startup failures now reap their child process.** Failed
  startup no longer leaves a listener behind to poison subsequent real-server CLI tests.

### Added

- Resume for interrupted Algolia migration jobs:
  `POST /1/migrations/{provider}/{job_id}/resume` claims a job whose pre-publication export
  was interrupted, using fresh request-only credentials that are never persisted, logged, or
  echoed. Positive status now carries `resumable`, `operation`, and `resumeHandle`. An
  interrupted job inherits its original `expires_at` retention deadline. Continuation is
  exactly-once across a full process restart. Meilisearch and Typesense resume are not
  supported.

- Provider-neutral source index discovery: `POST /1/migrations/{provider}/list-indexes` is
  mounted and published in the OpenAPI document for all three public providers (`algolia`,
  `meilisearch`, `typesense`), returning one shared response shape. Previously only the
  Algolia-shaped discovery path existed; the legacy Algolia route is preserved.
- Meilisearch and Typesense migrations now have served landed-data coverage through real,
  digest-pinned source containers and an authenticated Flapjack server, including exact
  source-ID-to-`objectID` projections and searchable field values. Meilisearch 1.50's default
  `attributeRank` rule now maps to Algolia's single `attribute` criterion, while its default
  `wordPosition` rule is omitted with a warning and unknown or custom rule shapes fail closed.
- Runtime HA membership management: `POST /internal/cluster/peers` adds a peer and
  `DELETE /internal/cluster/peers/{node_id}` removes one, both admin-gated, so cluster
  membership changes no longer require a restart.
- Dead-node auto-heal as a bounded, default-off engine capability
  (`FLAPJACK_AUTOHEAL_ENABLED=true`). The engine evicts at most one sustained-unreachable
  peer after a fixed three-observation threshold, only while the local quorum guard holds,
  readmits returning healthy peers through startup catch-up, and records every refusal,
  eviction, and readmission in `${FLAPJACK_DATA_DIR}/autoheal_decisions.jsonl`.
  `autoheal_enabled` and `autoheal_peers` are reported on admin-only
  `/internal/cluster/status`.
- Official Helm chart for multi-node deployment (`deploy/helm/flapjack/`) with a
  StatefulSet and automatic peer wiring.
- Published operations consumer contract for `/health`, `/internal/status`,
  `/internal/cluster/status`, and `/internal/snapshots/capability`
  (`engine/docs2/operations_consumer_contract.md`), so external operators have a stable,
  documented shape to build against.
- Admin-authenticated atomic bulk-replace job API:
  `POST /1/migrations/bulk-replace?indexName=...` streams NDJSON through the durable
  migration spool and publishes one replacement generation atomically, with durable status
  and cooperative cancellation at `/1/migrations/bulk-replace/{jobID}`. It is node-local
  only; admission returns `503 migration_ha_unsupported` when replication peers are
  configured.
- `flapjack migrate` operator CLI for one-time migration from Algolia, Meilisearch, or
  Typesense: submit, poll durable status, cancel, and acknowledge from the command line.
  `--source-provider <algolia|meilisearch|typesense>` selects the route and defaults to
  `algolia`; Algolia takes `--app-id`, while Meilisearch and Typesense take
  `--source-endpoint`, sent as `endpoint` and `node` respectively. Source and destination
  credentials are accepted only through `--*-env`, `--*-file`, or `--*-stdin` — never as
  argv — so keys stay out of help text, shell history, and process listings; the original
  `--algolia-key-{env,file,stdin}` spellings remain as aliases of the provider-neutral
  `--source-key-*` flags. Distinct non-zero exit codes classify configuration, transport,
  and terminal-job failures.
- Cooperative cancellation for asynchronous migration jobs, with owner-identity enforcement
  and a `409` for cancel requests that arrive too late to take effect.
- Algolia replica topology is carried through migration: replicas named by the source
  primary are created as settings-only virtual sidecars whose sort order resolves at query
  time, with the remaining fidelity limits surfaced as explicit migration warnings.
- Daily usage snapshots now persist the two usage gauges, so the historical series survives
  restarts.
- The migration API now declares one closed `source_provider` union (`algolia`,
  `meilisearch`, `typesense`) with provider-parameterised routes at
  `/1/migrations/{provider}` and matching OpenAPI paths. Meilisearch and Typesense HTTP
  migration adapters now submit through the shared durable async lifecycle. Preview is
  supported for Algolia, Meilisearch, and Typesense; Typesense preview preserves its
  provider-specific settings translation report. Non-Algolia resume continues to fail
  closed with stable `source_provider_unsupported` errors.

### Changed

- Oversized ingestion requests now return HTTP `413 Payload Too Large` instead of `400 Bad Request` for both per-document (`DocumentTooLarge`) and per-batch (`BatchTooLarge`) size-limit rejections, letting ingestion clients distinguish "chunk and retry" from a malformed request.
- Authenticated asynchronous Algolia migration now supports fenced
  `overwrite=true` replacement through the same crash-safe publication owner as
  the synchronous path.
- Successful asynchronous migration status now carries durable settings,
  synonym, rule, and warning outcomes; running, failed, and cancelled jobs omit
  those outcomes instead of presenting fabricated zeroes.
- The repaired single-machine scale contract now proves 1,000,000 compact and
  1,000,000 standard records through every frozen correctness, liveness,
  locality, evidence, and text-search latency gate. The July 25 latency/count
  failures remain immutable historical evidence rather than the current
  Guaranteed result.
- Node-local bulk replacement now owns bulk-only writer-buffer and document
  checkpoint knobs, and the scale projector accepts bulk-build throughput probe
  evidence only under the existing reference-locality gate.
- The node-local bulk-build writer buffer (20,000,000 bytes) and document
  checkpoint interval (1,000 documents) are frozen at the behavior-preserving
  baseline with a recorded local-locality gate measurement. Raising the
  bulk-only budget requires a reference-locality (`i4i.4xlarge` NVMe) sweep,
  which is a paid AWS scale run deferred to the named successor "paid reference
  ladder" batch; no new capacity or throughput claim is published.

### Fixed

- Explicit empty arrays (`[]`) in customer document fields are now preserved on write instead of being dropped, keeping customer-supplied content intact.
- Staged bulk builds now await their write worker and Tantivy merge threads before
  validation, settled-metrics capture, or publication.
- Write-admission backpressure no longer pauses a tenant whose live segment count
  settles into the staged-bulk range. The selected settled segment band now spans
  both measured regimes (online and staged bulk), so a large bulk build that settles
  above the online shape is no longer treated as an unhealthy segment ceiling.
- Async replacement now survives cancel/failure boundaries and idempotent owner
  ACK replay without leaving the replaced target in an indeterminate
  publication state.
- S3 snapshot deletion and retention failures now propagate their HTTP status instead of
  being ignored; `snapshot_to_s3` returns a sanitized `500` when retention cleanup fails.

## [1.0.10] - 2026-06-09

### Fixed

- Dashboard `/dashboard/` now serves the embedded dashboard HTML instead of returning 404.

## [1.0.9] - 2026-06-05

### Fixed

- Dashboard `/cluster` now presents standalone replication status as a healthy single-node state with explicit reassurance copy for operators who are not running HA peers.
- Dashboard `/events` now labels the failed-event status filter as `Failed`, reducing confusion with runtime page errors while preserving the underlying `error` status value.
- Dashboard `/index/:indexName` result-row delete actions now expose document-specific accessible names such as `delete document <objectID>`, restoring screen-reader context for icon-only controls.

## [1.0.8] - 2026-06-04

<!-- Promoted from [Unreleased] by Stage 3 CUT_V108 release drain. -->

### Fixed

- Documentation: refreshed install pin examples from stale `v0.2.0`/`v0.1.0` to current `v1.0.7` in `engine/install.sh` (lines 7 and 169) and propagated the repo-wide grep discipline; see Lane B Stage 3 propagation decisions bundle.

### Added

- README: added 7-question evaluator FAQ section covering Algolia API compatibility, multi-tenancy, write throughput, InstantSearch.js support, licensing, HMAC-scoped keys, and the migration endpoint; refreshed Known limitations phrasing for public readability.

## [1.0.7] - 2026-06-03

### Fixed

- Public install docs now describe the current installer contract: no argument resolves the latest release, while `sh -s -- <version>` pins an explicit release. The stale `v0.2.0` example was replaced with a current `v1.0.7` pin before release-cut validation.

## [1.0.6] - 2026-06-03

### Fixed

- Nightly SDK e2e jobs now wait for full readiness (`/health/ready`) in addition to liveness (`/health`) before exercising the server, eliminating the C# `InitializeAsync` flake whose root cause was that `engine/tests/common/wait_for_flapjack.sh` polls liveness only and the gap to readiness could exceed the C# SDK's 2s `ConnectTimeout`. A readiness curl is now appended after every `wait_for_flapjack.sh` invocation in `.github/workflows/nightly.yml` (dashboard-all, sdk-php-all, sdk-python-all, sdk-go-e2e, sdk-ruby-all, sdk-java-all, sdk-csharp-all), and the C# SDK's default `ConnectTimeout` was raised from `2000ms` to `5000ms` to provide a defense-in-depth margin for cold-start TCP accept jitter.
- Concurrent admin-key rotations are now serialized with a per-store mutex, preventing a race where two simultaneous `/1/keys/rotate` requests could leave the on-disk `.admin_key` file and in-memory key value inconsistent. The file-first/memory-second write ordering is preserved; the mutex only gates entry to `rotate_admin_key()`.
- Snapshot export and import filesystem walks now run on a blocking thread via `tokio::task::spawn_blocking` (`engine/flapjack-http/src/handlers/snapshot.rs`), preventing tokio worker starvation under concurrent multi-tenant write load that produced an intermittent restore document-count mismatch in the `snapshot_export_under_load` CI gate.

## [1.0.5] - 2026-06-01

### Fixed

- Test-environment shim now injects `AuthenticatedAppId` into the `test_stage4_sdk_smoke` Router via the canonical `apply_test_app_id_layer` helper, restoring the `ab_lifecycle_smoke` and `insights_to_analytics_full_lifecycle_smoke` regression gates that v1.0.4's idempotency rewrite (scoping by `AuthenticatedAppId`) left red. The production auth middleware always supplies the extension at runtime, so the v1.0.4 binary's customer-facing behavior was correct; this release re-greens the integration-test signal that protects against future regressions in the auth-scoped idempotency path.
- HA contracts `c3_replica_freshness` and `c4_restart_recovery` are CI-stabilized: the mirror-CI staging+prod failures observed on every push since the v1.0.4 sync were CI-environment-specific (resource-constrained runners, parallel test execution) rather than HA-mode behavior regressions. The tests now hold under both developer and constrained-CI environments, restoring the per-tenant catch-up and restart-recovery gates that protect HA deployments from real future regressions.
- PL-10 sustained-write saturation acceptance harness (`engine/loadtest/tests/pl10_saturation_acceptance.sh`) gate semantics were redesigned so the `FLAPJACK_WRITE_QUEUE_BATCH_SIZE` operator-tunable batching knob is legitimately distinguished from the baseline configuration. The prior gate could not fire under the shipped load profile because both configurations passed the absolute saturation threshold; the redesigned gate now produces a deterministic `TUNABLE_VERIFIED` outcome, restoring the harness as a real correctness mechanism for operators tuning batch size.
- HA test-hygiene sweep closed the LEAKY_PASS sites surfaced by the v1.0.4 snapshot-flake verification — dangling tokio tasks, `TempDir` lifecycle gaps, and adjacent test-helper leaks in `test_replication` and `make_test_app_state_wires_manager_dictionary_and_defaults` — preventing test-suite hygiene debt from masking real HA regressions in future releases.
- `flapjack-http` snapshot-install path-traversal test now uses the single-call `.expect_err("error tuple expected")` form (replacing the chained `.err().expect()`) to satisfy the staging linux x86_64 `clippy::err_expect` lint under `-D warnings`, re-greening the nightly clippy lane that had been red at the v1.0.4 tag.

## [1.0.4] - 2026-05-31

### Fixed

- Dashboard `DocumentCard` collapsed previews now apply deterministic remainder-field ordering before the six-field cutoff, preventing key fields such as `brand` from intermittently dropping behind lower-priority fields.
- `flapjack-server` now accepts the standard `--version` flag (e.g. `docker run ghcr.io/flapjackhq/flapjack:<next> --version` prints the version). The flag was missing from the clap derive metadata; landed on `main` after the v1.0.3 tag was cut, so it ships in 1.0.4.
- The Docker entrypoint now treats flag-only invocations (e.g. `--version`) as `flapjack` arguments instead of attempting to exec them as a binary. Same release window as above — landed on `main` after the v1.0.3 tag was cut.
- Startup catch-up snapshot installs now expose stable `sub_step` tags for failing restore branches and harden the staging/activation rename path against transient filesystem races during snapshot remediation.
- Node-local idempotency durability is now persisted at `${FLAPJACK_DATA_DIR}/_idempotency/cache.db`, with restart replay preserving single-execution semantics for repeated idempotency keys.
- Write-queue batching is tuned via `FLAPJACK_WRITE_QUEUE_BATCH_SIZE` with a default commit threshold of `32`, so commit batching follows the canonical runtime seam instead of per-operation flush behavior.

## [1.0.3] - 2026-05-30

### Changed (BREAKING)

- The `vector-search-local` feature (local embedding via `fastembed` → `ort` → `tokenizers` → `hf-hub`) is no longer enabled by default for the `flapjack`, `flapjack-http`, and `flapjack-server` crates. Consumers that need local embedding must build with `--features vector-search-local` (or `--features vector-search` for usearch-only without local model inference). This brings the baseline `cargo build` / `cargo test` graph under the runner disk budget that previously exhausted CI; the heavy embedding chain remains exercised by the explicit `--features vector-search` CI jobs.

### Fixed

- HTTP delete endpoints now return a bounded retriable `503` on accepting-node restart instead of hanging the request indefinitely (completes the PL-13 ack-on-durable contract for the delete path). The new `delete_documents_durable` seam mirrors the add-path's bounded-durable semantics; delete callers in `flapjack-http`'s `objects/batch.rs`, `objects/mod.rs`, and `replicas.rs` are routed through it.
- Task eviction (`evict_old_tasks` in the index manager) now skips non-terminal (`Enqueued`/`Processing`) tasks. Previously, an in-flight write under ≥1000 tasks/tenant overload could be evicted before the durable-ack poll observed its terminal status, producing a spurious `TaskNotFound`/`5xx` for a write that may still have committed. Eviction now waits for the task to reach a terminal state before reclaiming.

## [1.0.2] - 2026-05-29

### Fixed

- HTTP batch-write endpoints now return 200 only after Tantivy commit durability (ack-on-durable). Previously, writes were acknowledged upon queue enqueue, meaning a write-queue task crash between enqueue and commit silently lost acknowledged documents. Implements ADR-0005 Option C with bounded durable-ack polling (default 30s, configurable via `FLAPJACK_WRITE_DURABLE_TIMEOUT_MS`). Queue-full returns 429, commit-failure and ack-timeout return 503, all with `Retry-After: 1`. Measured 120.7x single-doc write throughput reduction vs fire-and-forget baseline — the accepted durability-over-throughput tradeoff; batching via realistic-batch patterns (1,483 docs/sec) remains within typical Algolia-migrator requirements.

### Changed

- Stage 6 sustained-load revalidation documented two overload scenarios at `921.538969/s` and `831.652577/s`; each preserved contract health (`write_http_5xx_rate=0.00%`, `write_http_unexpected_4xx_rate=0.00%`) while saturation remained visible (`85.04%` and `98.21%` write-failure rates).
- Rolling-restart HA behavior improved to a steady-state `0.88%` per-node spread while maintaining availability, narrowing the prior convergence boundary.
- Known residual limits: sustained-write saturation under overload (PL-10), cross-node idempotency-cache durability (ADR-0005 OQ2), and replication-boundary convergence (ADR-0004). Restart-window write loss is resolved by the ack-on-durable fix above.

## [1.0.1] - 2026-05-23

### Changed

- Release publishing now builds and publishes Linux amd64 and arm64 Docker candidate images on separate per-architecture paths before stable tag promotion.
- Stable `ghcr.io/griddlehq/flapjack:<version>` and `:latest` Docker tags are now promoted only from a candidate manifest that passed required architecture checks.

## [1.0.0] - 2026-03-28

### Added

- Full-text search with typo tolerance.
- Faceting and filtered search support.
- Geo search capabilities.
- Vector search support.
- Multi-index federated search support.
- Click analytics collection and query support.
- Query suggestions generation support.
- Synonyms and query rules support.
- Personalization API and profile-aware search support.
- Recommendations API support.
- A/B testing (experiments) support.
- AI search and chat-style RAG endpoint support.
- API keys with ACLs, restrict-sources enforcement, and per-key rate limiting.
- Per-tenant dictionaries (stop words, plurals, compounds).
- S3 snapshot backup and restore support.
- Algolia API-compatible HTTP endpoints.
- OpenAPI specification export for API contract verification.
- Feature-gated OpenTelemetry tracing export support.
- Dashboard UI for operations and search workflows.
- Replication support for peer-to-peer index synchronization.
- TLS and ACME support for secure deployments.
- Docker deployment plus install-script and systemd bare-metal paths.

### Changed

- API behavior and payloads were aligned with Algolia-compatible client expectations across key search and index routes.
- Deployment and operations guidance were expanded to support consistent setup across local, container, and hosted environments.

### Fixed

- Stabilized core indexing and query execution paths for production usage.
- Hardened transport and replication flows to reduce operational failure modes during distributed operation.

[Unreleased]: https://github.com/flapjackhq/flapjack/commits/main
[1.0.10]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.10
[1.0.9]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.9
[1.0.8]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.8
[1.0.7]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.7
[1.0.6]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.6
[1.0.5]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.5
[1.0.4]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.4
[1.0.3]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.3
[1.0.2]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.2
[1.0.1]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.1
[1.0.0]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.0
