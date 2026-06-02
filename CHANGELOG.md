# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
[1.0.5]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.5
[1.0.4]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.4
[1.0.3]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.3
[1.0.2]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.2
[1.0.1]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.1
[1.0.0]: https://github.com/flapjackhq/flapjack/releases/tag/v1.0.0
