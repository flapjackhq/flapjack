# Flapjack - Roadmap

**Last updated:** 2026-07-31 — reconciliation against `origin/main` after the
`jul30_12am` local-truth/migration-resume batch and security Wave 1. Five
security lanes and the MIG-6 resume lane merged; this revision is the ledger
update those lanes deliberately deferred to a Wave-4 owner that was never
dispatched.

**MIG-6 is closed.** Interrupted-job resume — the product's single most-deferred
item, carried across `jul18_11am`, `jul25_pm_1`, and `jul29_12pm_3` — now ships
as an explicit admin-authenticated `POST /1/migrations/{provider}/{job_id}/resume`
route, proven by a full-process-restart exactly-once contract. Its budget-stop
revert rule was not invoked.

**Security Wave 1 is two-thirds landed.** `SEC-G1` (credential-parser panic),
`SEC-G7` (admin key in query string), and `SEC-G12` (fail-open route
authorization) are fixed and merged; `SEC-G6`, `SEC-G10`, and `SEC-G11` from
Wave 4 were pulled forward and also merged. `SEC-G3` (response headers / CSP /
dashboard `localStorage`) and `SEC-G8` (request timeout, concurrency limit,
panic containment) remain open and are the entire remaining W1 scope — their
lane was gated on `router.rs`, which the MIG-6 merge released.

**One new durability defect is open.** The disk-exhaustion lane measured, and
reproduced 5/5, a batch that is rejected to the client with HTTP `500` under
ENOSPC and then becomes visible in the index after restart. It is recorded
below as `DUR-1`. It is the only known correctness defect on `main`.

Prior revision (2026-07-30) follows verbatim.

**Last updated:** 2026-07-30 — Stage 6 of the atomic bulk build/replace lane
froze the node-local bulk-only writer buffer (20,000,000 bytes) and document
checkpoint interval (1,000 documents) at the behavior-preserving baseline,
against a local-locality gate measurement of 50,000- and 100,000-document
staged bulk builds, both passing with 3/3 crash proofs. The exact specimens
(throughput, peak RSS, peak build disk, settled disk, live segment count) are
recorded once, at their owner
`engine/src/index/mod.rs::BULK_BUILD_CONFIGURATION_MEASUREMENT`, and in
`engine/docs2/4_EVIDENCE/2026_07_29_jul26_8pm_7_stage6_bulk_constant_freeze.md`;
they are deliberately not restated here, so there is one place to update when
they are re-measured. The reference-locality (`i4i.4xlarge` NVMe) sweep that could
justify a larger bulk-only budget is a paid AWS scale run held out of this batch
by its no-AWS-provisioning posture and assigned to the named successor "paid
reference ladder" batch. The repaired-path local standard ladder remains a
historical RED outcome, but its
[`capacity_preflight.json`](engine/loadtest/results/20260729T151500Z-jul26_8pm_9-local-repaired-standard/rung_1000000/capacity_preflight.json)
was `GO`, with 4.66x disk and 5.42x memory headroom; the failed import is
therefore attributed to write backpressure rather than a capacity shortage.
The pause/retry and eventual bulk-replace success are recorded in the
[L1 retry receipt](engine/docs2/4_EVIDENCE/2026_07_29_jul29_7pm_1_migration_backpressure_retry_receipt.md),
while the
[L3 characterization](engine/docs2/4_EVIDENCE/2026_07_29_jul29_7pm_3_read_stall_characterization.md)
refutes a no-shutdown read stall. The single-machine scale/capacity row stays
OPEN for the paid reference-locality question, and no new Guaranteed or
throughput claim is published. Prior revision (2026-07-28) follows verbatim.

**Last updated:** 2026-07-28 — the admin-authenticated node-local
`POST /1/migrations/bulk-replace` job API now streams NDJSON through the durable
migration spool into the canonical fenced replacement owner. Its shared durable
status/cancel contract states `single_node_only`, and admission fails closed with
`503 migration_ha_unsupported` whenever replication peers are configured.
`flapjack ingest --mode replace` is a pure client of that API: it uses bounded
disk-backed normalization, streams the request body, polls durable job status,
and reports only the server-confirmed committed count.
Reconciliation against `origin/main` after the
async replacement and status-count lanes. Authenticated async
`overwrite=true` now ships through the existing fenced replacement/publication
owner, with create→replace, durable ACK replay, cancel/failure recovery, and
exact final-target known answers recorded in
[`engine/docs2/4_EVIDENCE/2026_07_25_jul25_am_4_async_migration_replace_publication_repair_receipt.md`](engine/docs2/4_EVIDENCE/2026_07_25_jul25_am_4_async_migration_replace_publication_repair_receipt.md).
Successful async status now carries the durable import outcome
(`settings_applied`, synonym/rule imported counts, and warnings) instead of
fabricating zeroes; resume remains the only MIG-6 deferral and HA import remains
refused under MIG-7. Track A's measured single-machine text record ceiling and its
limits are published in
[`engine/loadtest/BENCHMARKS.md#single-machine-text-record-ceiling-follow-up-july-26-2026`](engine/loadtest/BENCHMARKS.md#single-machine-text-record-ceiling-follow-up-july-26-2026):
1,000,000 compact and 1,000,000 standard records pass every frozen gate. That
benchmark section is the sole owner of the current curve and Guaranteed value;
the July 25 failures remain historical evidence, not an active profiling
premise. Fjcloud A3's real-Algolia consumer receipt is merged. Reconciliation against
`origin/main` after the jul24–25 ingestion/privacy/migration-reliability lanes: merged and
code-verified on `main`: **B1** ingestion adapter seam — hardened push/batch API (write-scope
enforcement, `413` size limits) plus the pull-connector framework decision record
`engine/docs2/3_IMPLEMENTATION/decisions/active/0011_b1_pull_connector_framework_design.md`
(ADR-0011), which is the prerequisite for any future connector lane (see ING-2 below); **F10E**
authenticated migration privacy-scrub transport (cross-repo dependency for fjcloud A5);
**migration ack-owner-identity** recovery; **empty-array customer-content preservation**; and
**crash-durability admission-record** hardening. F2 (HA-AUTOHEAL) and F3 (OPS-R5 → implemented)
were already reflected in the 2026-07-24 revision below. The former in-flight
async replacement lane is merged; the Fjcloud A3 continuation now owns the
real-Algolia consumer proof. Prior 2026-07-24 revision follows
verbatim below.

**Last updated:** 2026-07-24 — MIG-5b node-local synchronous `overwrite=true` is now live-proven against a real server (`jul24_am_4`, merge `94a15b1b0`; evidence `engine/docs2/evidence/2026_07_24_mig5b_node_local_overwrite/summary.md` — 1006 exact final IDs, async-overwrite → 400 and HA-overwrite → 400 refused, all 6 release gates exit 0), closing the node-local slice of MIG-5; Authenticated async overwrite (MIG-5) is shipped node-locally through the fenced replacement/publication owner; HA-converging overwrite (MIG-7) stays refused by design. That merge surfaced a staging-mirror CI regression now being repaired by the in-flight `jul24_pm_1` build-identity parity lane (`cli_build_info_matches_live_health_build_info`, staging run `30095619747`, reproduces on clean dev `main`); public-mirror Rust CI parity (`ef7b3190e`) and replication lint parity (`a1d8bcb4a`) already landed. The HA-AUTOHEAL local Docker decision contract is now proven by `engine/examples/ha-cluster/test_ha_autoheal.sh`, with the retained passing transcript under `engine/examples/ha-cluster/.evidence/20260724T215953Z/`. **2026-07-24** — OPS-R5 moved to `implemented/2026_06_05_history.md` after verifying the published consumer contract and all four route-mounted wire owners exist in engine code. Prior revision (2026-07-23) follows verbatim. HA-AUTOHEAL added as a shipped bounded default-off engine capability after the repaired Helm live proof covered disabled sustained-outage retention, enabled eviction, readmission/catch-up, and majority-loss refusal. Prior revision context follows verbatim. **2026-07-23** — MIG-9 moved to `implemented/2026_06_05_history.md` after the Stage 5 clean-SHA acceptance chain retained the real-server migration spool GC probe evidence and the final `flapjack-http` gate passed at the accepted HEAD. MIG-10 remains deferred and unimplemented, but no longer depends on a stale spool-module precondition. Prior revision context follows verbatim. **2026-07-22** — HA-BOOTSTRAP-JOIN moved to `implemented/2026_06_05_history.md` after focused bootstrap contracts and the live Stage 4 HA bootstrap probe passed at HEAD. **2026-07-21** — verification sweep against `origin/main` after the jul20_4pm OSS HA batch corrected MIG-6 cancel status and discharged the stale `flapjack-http` suite deferral. **2026-07-20** — reconciliation sweep against `origin/main` merge state. Stage 1 KATs proved all three **MIG-11** replica-fidelity gaps closed, so the row moved to `implemented/2026_06_05_history.md`. Stage 2 closed **MIG-8** with bounded completed-ID sidecar checkpoint writes and moved that row to implemented history. MIG-5 is shipped node-locally, while the remaining deferred-by-design MIG-7 and future-dated PL-2 rows remain open.
**Ledger policy:** `ROADMAP.md` is the only root open-work ledger. Mission,
scope, and strategic priority order live in [`PROJECT_OVERVIEW.md`](PROJECT_OVERVIEW.md).
Shipped capability status lives in [`engine/docs2/FEATURES.md`](engine/docs2/FEATURES.md),
release history lives in [`CHANGELOG.md`](CHANGELOG.md), and completed work
history is routed to `implemented/2026_06_05_history.md`.

**Last shipped release:** [v1.0.10](https://github.com/flapjackhq/flapjack/releases/tag/v1.0.10) (2026-06-09). Detailed release history lives in [`CHANGELOG.md`](CHANGELOG.md).

**ID prefixes:** `RF-*` = foundational refinement track; `PL-*` =
launch-hardening / operational-polish track; `MIG-*` = source-migration
capability track. IDs are stable identifiers, not priority rank.

## Active

| ID | Work Item | Current State | Evidence / Owner |
|----|-----------|---------------|------------------|
| ING-1 | OSS ingestion CLI and control-plane progression | In progress. The shipped beta is a client-side `flapjack ingest` slice for JSON arrays, NDJSON files, and stdin, with upsert plus explicit object-ID deletes; it is not the parked hosted scheduler/worker control plane. | Runtime owner: `engine/flapjack-server/src/ingest.rs`; executable contract: `engine/flapjack-server/tests/ingest_cli_test.rs`; public bounds: `engine/docs2/FEATURES.md#flapjack-ingest-beta-bounds`. |
| MIG-5 | Migration `overwrite=true` into an existing target | Shipped for node-local synchronous and authenticated async migration, including streamed NDJSON admission at `POST /1/migrations/bulk-replace`. All paths route through the canonical fenced replacement owner after validating the committed journal generation, durable target epoch `E_new`, and promoted target `committed_seq = W`; the bulk endpoint's receipt and status state `single_node_only`, and HA admission returns `503 migration_ha_unsupported`. Real-server synchronous evidence: [node-local overwrite summary](engine/docs2/evidence/2026_07_24_mig5b_node_local_overwrite/summary.md). Async known-answer evidence: [replacement/publication repair receipt](engine/docs2/4_EVIDENCE/2026_07_25_jul25_am_4_async_migration_replace_publication_repair_receipt.md). | Design reference: [ADR 0008](engine/docs2/3_IMPLEMENTATION/decisions/active/0008_mig5_overwrite_mutation_fence_design.md). Replacement owner: `engine/src/index/manager/lifecycle.rs`; publication owners: `engine/src/index/manager/publication.rs` and `engine/src/index/manager/publication/**`; mutation fence owners: `engine/src/index/manager/write.rs`, `engine/src/index/write_queue/`; migration routing owners: `engine/flapjack-http/src/handlers/migration/{bulk_replace,job_runner,spool}.rs`. |
| MIG-6 | Async migration job contract (status / cancel / resume) | **Shipped for Algolia (2026-07-31).** An interrupted pre-publication export can be claimed through the explicit admin-authenticated `POST /1/migrations/{provider}/{job_id}/resume` route using fresh request-only credentials; positive status exposes `resumable`, `operation`, and `resumeHandle`; interruption preserves the original absolute `expires_at` with no second GC owner; exact-ID and full-process-restart contracts prove exactly-once continuation. Meilisearch and Typesense resume remain unsupported. Row retained in Active only until a release carries it. | Owners: `engine/flapjack-http/src/handlers/migration/{spool.rs,spool_lifecycle.rs,spool_support.rs,export.rs,job_runner.rs,mod.rs,import.rs,algolia_client.rs}`, `engine/flapjack-http/src/{router.rs,openapi.rs}`; restart proof: `engine/flapjack-server/tests/crash_durability_test.rs::interrupted_async_migration_resumes_exactly_once_after_process_restart`; design: [ADR 0012](engine/docs2/3_IMPLEMENTATION/decisions/active/0012_mig6_resume_interrupt_state_design.md); evidence: `engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_4_mig6_resume_receipt.md`. |
| DUR-1 | Rejected write replays into the index after restart under disk exhaustion | **Open, measured, reproducible — the only known correctness defect on `main`.** Under ENOSPC a batch is rejected to the client with HTTP `500` (sanitized body, no path/device leakage, no panic) and then becomes visible in the index after restart: 76 acknowledged versus 80 recovered, extras `disk-020-00`..`disk-020-03`, reproduced 5/5 at source SHA `7aaa08f7fa300e7fa6cdac4ca0c440c2e2076a16`. Durable admission publishes the oplog entry before the acknowledgement decision, so a failed Tantivy commit leaves a published entry with no reconciliation. `error.rs` is explicitly **not** the owner and needs no change. Smallest unblocking change: add a fail-capable contract test inside the owning write-queue module that drives durable admission followed by a commit failure, *then* change `commit_batch` ordering to either roll the published oplog entry back below `committed_seq` or convert the response into an honest durable acknowledgement with a task ID and retry semantics. Do not weaken `disk_exhaustion_acceptance.sh` to make retained specimens pass. | Owners: `engine/src/index/write_queue/admission.rs::{stage_record,publish_record}`, `engine/src/index/write_queue/finalization.rs::commit_writer_with_panic_guard`, `engine/src/index/write_queue/mod.rs::commit_batch`; probe: `engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh`; acceptance owner: `engine/loadtest/tests/disk_exhaustion_acceptance.sh`; evidence: `engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_5_disk_exhaustion_receipt.md`. Closing condition: three sequential specimens green at a fixed HEAD with `summary.json.source_sha` equal to that HEAD. |
| JOIN-1 | Backend↔frontend joined proof for the dashboard | **Open.** The 2026-07-30 matrix audits 90 backend capability rows and finds 0 with a current passing joined proof and 0 partial; 63 have an operable route but no current joined proof. **Two of the three residuals are already closed; the row is much closer to runnable than the audit reads.** Invalid Algolia runtime inputs — resolved 2026-07-30 by the credential repoint. Playwright HTML reporter not returning — fixed at `53391b794` (2026-07-30 11:57), *after* the audit was measured, by pinning `open: 'never'` with a regression test in `engine/dashboard/playwright.config.test.ts`. **Only the inconclusive run-2 Vite/webserver startup failure remains open**, and it may not reproduce. The next action is simply to re-run `./s/test --dashboard-full` on a quiet host and see what it says — not a fix lane. The dashboard is frozen pending Svelte replacement, so this row is about *proof and honest reporting*, not new screens. | Matrix and per-row owners: `engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_3_dashboard_join_audit_receipt.md`; runner owner: `engine/_dev/s/test`; public claim owner: [`engine/docs2/FEATURES.md`](engine/docs2/FEATURES.md). |
| MIG-7 | HA-converging import | Refused by design, not planned for v1. [ADR 0009](engine/docs2/3_IMPLEMENTATION/decisions/active/0009_mig7_ha_converging_import_design.md) records the corrected baseline: `move_index` / `move_index_with_publication` preserves the source oplog and `committed_seq`; the HA gap is node-local publication plus no durable cross-node convergence epoch, exclusive promotion rule, or peer adoption receipt. Reopen only with the costed pull-snapshot adoption design in ADR 0009. | `engine/src/index/manager/publication.rs`; refusal owner: `engine/flapjack-http/src/handlers/migration/mod.rs::admit_migration_request` |
| RF-4 | Runbooks iteration | Open-ended operational follow-through. Continue refining runbooks from incident learnings. On 2026-07-21, the [Migration jobs](engine/docs2/3_IMPLEMENTATION/OPERATIONS.md#migration-jobs) operations runbook landed; no next RF-4 runbook candidate surfaced during the migration probe. | [`engine/docs2/3_IMPLEMENTATION/OPERATIONS.md`](engine/docs2/3_IMPLEMENTATION/OPERATIONS.md) |
| PL-10 | Write-path saturation under sustained load | Open for v1.1 architecture work. The v1.0.4 batch-size knob and v1.0.5 `uplift_ratio >= 1.50` acceptance gate are shipped and verified, but cross-node fanout remains constrained by the single-writer Tantivy mutex. The persistent admission/replay first slice has shipped inside the existing `write_queue` while preserving synchronous `wait_for_write_durable` ACK semantics and the distinct `QueueFull` 429 versus `TooManyConcurrentWrites` 503 contracts. The 2026-07-23 Stage 4 terminal disposition selected `withdraw`: after one evidence-justified group-commit retry validly falsified fixed-load parity again, new live writes no longer create persistent admission records while existing on-disk admission records still replay and reconcile, so remaining PL-10 work is the single-writer Tantivy bottleneck and cross-node fanout. | Stage 6 classification: `engine/docs/research/pl10_stage6_dual_scenario_classification.md`; v1.1 design: [ADR 0007](engine/docs2/3_IMPLEMENTATION/decisions/active/0007_pl10_v11_write_path_design.md); bounded-lag terminal evidence: [`engine/loadtest/BENCHMARKS.md#pl-10-stage-4-terminal-disposition-july-23-2026`](engine/loadtest/BENCHMARKS.md#pl-10-stage-4-terminal-disposition-july-23-2026); proof directories: `engine/loadtest/results/20260528T062547Z-pl10-stage6-dual-scenario/`, `engine/loadtest/results/20260601T202043Z-pl10-saturation-acceptance/`, `engine/loadtest/results/20260601T203717Z-pl10-saturation-acceptance/`, `engine/loadtest/results/20260601T204623Z-pl10-saturation-acceptance/`, `engine/loadtest/results/20260723T052406Z-write-soak/`, `engine/loadtest/results/20260723T052650Z-write-soak/`, `engine/loadtest/results/20260723T062314Z-write-soak/`, `engine/loadtest/results/20260723T062811Z-write-soak/` |
| HA-FLAKE | HA snapshot test flake remediation | Fix verified and leaky-pass sites closed; keep future HA regression signal protected. Not v1.0.x blocking. | Fix owner paths: `engine/flapjack-http/src/startup_catchup.rs`, `engine/flapjack-replication/src/manager.rs`, `engine/src/analytics/writer.rs`; regression contract: `engine/tests/test_snapshot_import_failure_contract.rs`; proof: `docs/reference/research/may31_eve_ha_snapshot_flake_verify_proof.md` |
| HA-AUTOHEAL | Dead-node auto-heal engine capability | Shipped as a bounded engine capability and disabled by default. **Local Docker decision contract proven (2026-07-24):** `engine/examples/ha-cluster/test_ha_autoheal.sh` proves enabled majority-loss refusal, legal single-node eviction, readmission with startup catch-up, and disabled refusal; the passing transcript is retained under `engine/examples/ha-cluster/.evidence/20260724T215953Z/`. With `FLAPJACK_AUTOHEAL_ENABLED=true`, sustained-unreachable single-node eviction is guarded by the three-observation threshold and the local quorum/partition refusal rule; returning healthy candidates are readmitted through the membership owner with startup catch-up before authoritative reads. It is not consensus, majority writes, arbitrary partition healing, or simultaneous majority-loss recovery; behavior details route to [Dead-node auto-heal](engine/docs2/3_IMPLEMENTATION/OPERATIONS.md#scenario-dead-node-auto-heal) and [High Availability](engine/docs2/FEATURES.md#high-availability). | Policy owner: `engine/flapjack-replication/src/autoheal.rs`; observe/act/readmission owners: `engine/flapjack-replication/src/manager.rs::{apply_autoheal_probe_pass,record_autoheal_eviction,readmit_healthy_autoheal_candidates}`; status owner: `engine/flapjack-http/src/handlers/internal.rs::cluster_status`; live proof: `deploy/helm/flapjack/test_live_cluster.sh`. |
| PL-8 | Nginx restart-window write-loss recovery residual routing | Core restart-window write-loss fix is closed; residual tracking remains here so HA docs keep one open-work owner. HA convergence posture is **bounded convergence** after L1 anti-entropy fix `066549d5`; remaining saturation routes to PL-10 and cross-node idempotency routes to ADR-0005 OQ4. | Canonical evidence owner: [`engine/loadtest/BENCHMARKS.md`](engine/loadtest/BENCHMARKS.md); narrative seam: `engine/docs2/3_IMPLEMENTATION/decisions/active/0004_ha_convergence_reversal.md` |
| SEC-W1 | Security wave 1 — request-handling robustness and browser hardening | **Partially shipped 2026-07-31.** Closed or moved to implemented: credential-parser robustness, admin-query credential refusal, fail-closed route authorization, global security headers including CSP, and request timeout / concurrency / panic containment. The remaining W1 work is narrowed to the dashboard admin key that still persists in `localStorage`. | Public probes: `engine/tests/credential_parser_http_probe.sh`, `engine/tests/authorization_boundary_http_probe.sh`, `engine/tests/security_headers_http_probe.sh`, `engine/tests/csp_dashboard_browser_probe.sh`, `engine/tests/resource_bounds_http_probe.sh`; remaining implementation owner: `engine/dashboard/src/hooks/useAuth.ts`. |
| SEC-W2 | Security wave 2 — optional in-binary TLS listener | Assessed 2026-07-30, not started. The server binds a plain TCP listener and has no server-side TLS; `engine/flapjack-ssl/` already acquires and renews ACME certificates that nothing terminates with. Ship `--ssl-cert-path` / `--ssl-key-path` (matching Meilisearch flag names so migrating operators need not relearn them) wired to those certificates. Plain HTTP remains the loopback development default and a reverse proxy remains the recommended production topology — the flag is an option, not a new blessed path. Decision record `SD-002` in `docs/security/DECISIONS.md`. | Owners: `engine/flapjack-http/src/server.rs`, `engine/flapjack-ssl/`; baseline claim to update on ship: [`engine/docs2/3_IMPLEMENTATION/SECURITY_BASELINE.md`](engine/docs2/3_IMPLEMENTATION/SECURITY_BASELINE.md) |
| SEC-W3 | Security wave 3 — security audit event coverage | Assessed 2026-07-30, not started. The engine emits two security event types today (auth failure, admin-key rotation). Broaden the vocabulary over API key lifecycle, index deletion, settings mutation, snapshot restore, and successful admin authentication, with actor/action/target/outcome fields and no key, header, or query payload content. Scheduled ahead of the cheaper wave-4 items because an audit trail proves nothing about the period before it starts recording — it is the one control that cannot be backfilled. Per `SD-006`, the engine emits events and does not own durable retention, so this does not by itself close the corresponding fjcloud control. | Owners: `engine/flapjack-http/src/auth/middleware.rs`, `engine/flapjack-http/src/handlers/internal.rs`; existing event precedent: `security_audit_auth_failure`, `security_audit_admin_action`; decisions: `docs/security/DECISIONS.md` |
| SEC-W4 | Security wave 4 — hardening and hygiene backlog | **Partially shipped 2026-07-31.** Closed or moved to implemented: analytics client-IP minimization before persistence, fixed non-root container runtime, and request-resource bounds that were pulled forward with the router middleware work. Supply-chain posture narrowed: the bundled dashboard now has a high-and-above production audit gate, while production moderate advisories remain open below that threshold. Remaining W4 work covers snapshot/S3 encryption, replication peer authentication, and the moderate-advisory supply-chain disposition. | Public probes: `engine/tests/test_analytics_ip_e2e.rs`, `engine/tests/test_docker_runtime_e2e.sh`, `engine/dashboard/scripts/audit_gate_fixture_test.sh`, `engine/tests/resource_bounds_http_probe.sh`; remaining implementation owners: `engine/src/index/snapshot.rs`, `engine/src/index/s3.rs`, `engine/flapjack-replication/src/peer.rs`, `engine/dashboard/scripts/audit_gate.sh`. |
| SEC-QXML | Remove quick-xml RUSTSEC ignores when rust-s3 updates | RUSTSEC-2026-0194/-0195 (quick-xml DoS, fix >=0.41) are temporarily ignored because rust-s3 0.37.2 (latest as of 2026-07-06) pins quick-xml ^0.38 — no upgrade path exists. When a rust-s3 release depends on quick-xml >=0.41: `cargo update rust-s3`, then delete the ignore entries from `engine/.cargo/audit.toml` and `engine/deny.toml` (kept in lockstep). The a06 nightly audit test enforces the rest. | Ignore owners: `engine/.cargo/audit.toml`, `engine/deny.toml`; gate: `engine/tests/test_security_audit.rs` (`a06_workspace_passes_cargo_audit_and_cargo_deny`); upstream watch: <https://crates.io/crates/rust-s3> |

## Up Next

Dispatch sequencing for the next batch. **This section owns ordering and
parallelism only** — every row's scope, evidence, and owners stay in `Active` or
`Planned` above and below, so the two cannot desync. Reference by ID here; do
not restate a row's content.

All five are **engine-backend** work. The OSS React dashboard is frozen pending
the fjcloud Svelte console unification, so no lane below touches
`engine/dashboard/`, and `JOIN-1` is deliberately excluded from this batch: its
remaining value is a re-run on a quiet host, not authored work.

**Wave A — four lanes, file-disjoint, dispatch together.** Verified disjoint
against each other and against the three lanes in flight on 2026-07-31
(`jul28_9pm_6` Typesense, `jul30_11pm_5` router middleware, `jul30_pm_3`
migration preview), none of which touch `engine/src/`:

| ID | Owns exclusively | Why now |
|----|------------------|---------|
| `DUR-1` | `engine/src/index/write_queue/{admission,finalization,mod}.rs` | The only known correctness defect. Red gate already exists at `engine/loadtest/tests/disk_exhaustion_acceptance.sh`; the probe reproduces 5/5. Author the fail-capable write-queue contract test **before** touching `commit_batch` ordering — Stage 3 of the disk lane correctly declined a speculative patch across three owners. |
| `SEC-W2` | `engine/flapjack-http/src/server.rs`, `engine/flapjack-ssl/**` | `flapjack-ssl` already acquires and renews ACME certificates that nothing terminates with. Ships `--ssl-cert-path` / `--ssl-key-path` using Meilisearch's flag names. Unblocks `SEC-G9`, which is otherwise permanently ordering-blocked. |
| `SEC-W3` | `engine/flapjack-http/src/auth/middleware.rs`, `engine/flapjack-http/src/handlers/internal.rs` | The one control that cannot be backfilled — an audit trail proves nothing about the period before it starts recording. Independent of every other row. |
| `SEC-G5` | `engine/src/index/snapshot.rs`, `engine/src/index/s3.rs` | Self-contained storage-layer work in a subtree no in-flight lane touches. |

**Wave B — gated, dispatches as its blocker merges.** Do not park these as
dependencies; they are capacity staggers.

- `SEC-G9` replication peer auth — after `SEC-W2` lands a TLS story to depend on.
- `MIG-12` provider-neutral source-migration core — after `jul28_9pm_6` and
  `jul30_pm_3` both merge, since all three edit
  `engine/flapjack-http/src/handlers/migration/`. `MIG-14` gates on this.
- `PR-10` residual chaos modes — OOM-kill-and-restart is uncovered, and
  replica-partition-from-primary has no asserted contract. Test-tree only, so it
  can run beside anything; sequenced second because neither is a correctness
  defect. See the `PR-10` row in [`engine/docs2/FEATURES.md`](engine/docs2/FEATURES.md).
- `jul30_12am_6` repaired-ladder 1M re-measure — **needs a quiet host.** It is a
  measurement lane; running it under fleet load produces a number that must be
  discarded. This is a validation-locality constraint, not a preference.

**Not in this batch, and why:** `PL-10` needs a new falsifiable question beyond
the verified 1M floor, not another run. `ADR-0005 OQ4` is a resolved v1.1 design
awaiting a cross-node execution decision. `SEC-QXML` is blocked on an upstream
`rust-s3` release. `DOCS-CLAIMS` and `PL-11` live on surfaces this repo does not
own.

## Planned

| ID | Work Item | Planned Direction | Evidence / Owner |
|----|-----------|-------------------|------------------|
| MIG-12 | Provider-neutral source-migration core | Pre-launch. Extract one genuine source-adapter contract while preserving the existing spool, job runner, status/cancel/ACK, and fenced publication owners. Keep recurring ingestion connectors separate. Preserve Algolia routes as compatibility aliases, use one provider-discriminated wire contract, and settle arbitrary self-hosted endpoint SSRF/TLS/redirect/DNS-rebinding policy before implementation. | M0 architecture owner: Fjcloud `chats/icg/jul26_am_11_source_migration_architecture_security.md`; current shared owners: `engine/flapjack-http/src/handlers/migration/{export,import,job_runner,spool}.rs`; outbound policy: `engine/src/security.rs`. |
| MIG-13 | Meilisearch source migration | Code-verified locally for the shared source-migration lifecycle at product SHA `c6c2973fdba95c640be606a5a1a205eaf7757ac0`; remote enablement remains default-off and later. | Receipt: `engine/docs2/4_EVIDENCE/2026_07_28_m2em_meilisearch_adapter_receipt.md`; M0 contract/KAT owner: `chats/icg/jul26_am_12_meilisearch_migration_contract_kat.md`. |
| MIG-14 | Typesense source migration | Code-verified locally for the shared source-migration lifecycle at product SHA `616acc0e42c8016cf7fbde75f5c1d6068a37e346`; typed collection schema, stable `id` mapping, exports, synonym/curation linkage, quiescence drift refusal, permissions, and endpoint policy are either carried or attributed through `TypesenseSettingNotMigrated` rather than guessed as Algolia-shaped behavior. Version bounds stay pinned by the M0B image digest and are not enforced at runtime, and automatic alias behavior is recorded as an implemented-but-uncertified reject. Remote enablement remains default-off and later. | Receipt: `engine/docs2/4_EVIDENCE/2026_07_28_m2et_typesense_adapter_receipt.md`; M0 contract/KAT owner: `chats/icg/jul26_am_13_typesense_migration_contract_kat.md`. |
| ING-2 | Next ingestion connector and compatibility catalog | Deferred until a named demand and concrete cost/auth/scale gate exists. PostgreSQL/Supabase, MySQL, object storage, crawler/ecommerce sources, and BigQuery/GA4 remain catalog entries, not shipped connectors; BigQuery is explicitly a named parity gap rather than a free-floating work item. | This row owns the future connector catalog; preserve the shipped CLI boundary in `engine/docs2/FEATURES.md#flapjack-ingest-beta-bounds` rather than expanding ING-1. |
| ADR-0005 OQ4 | Cross-node failover idempotency dedup | v1.1 design resolved. Node-local restart-durable idempotency remains at `${FLAPJACK_DATA_DIR}/_idempotency/cache.db`; cross-node single execution requires a quorum-durable reservation before mutation, a quorum-durable completed result before success acknowledgment, and fail-closed handling of pending or indeterminate claims. | [ADR 0010](engine/docs2/3_IMPLEMENTATION/decisions/active/0010_oq4_cross_node_idempotency_dedup_design.md) |
| PL-11 | Public mirror Laravel Scout CI cleanup | Remove the always-green `integration-laravel-scout` job from the public mirror CI once the Debbie flow reaches the mirror, so CI no longer advertises an unsourced integration stub. | Target: `.github/workflows/ci.yml`; owner: mirror/debbie flow; private-development context: `docs/reference/research/20260718_stage5_framework_integration_portfolio_disposition.md` |
| PL-2 | `cargo-nextest` migration re-evaluation | Re-evaluate around 2026-11-26 against accumulated hang-frequency data. If PL-1 plus test-hang discipline have not covered 95% of hangs, plan `.config/nextest.toml` per-test timeouts, a `.cargo/config.toml` alias, and CI workflow migration. | Existing CI-side cap owner: `engine/tests/ci_test_timeout_cap_acceptance.sh` |
| DOCS-CLAIMS | Stale docs-site pricing/organization/image claim audit | Audit and correct the public docs-site pricing, organization, and image claims, which live on the separate private `flapjackhq/flapjack-cloud` `docs-site/` surface (not this repo's Debbie sync surface). Entry gate before any edit or deploy: provenance recovery of the editable owner path, plus an automated claim inventory (rendered source → asserted value) that tests correctness, not page existence. No in-repo renderer, second roadmap, or docs-site mirror boundary here. | Research: `docs/reference/research/20260716_stage4_docs_site_claim_followup.md`; owner: private `flapjackhq/flapjack-cloud` `docs-site/` |
| MIG-10 | Deferred `max_items_per_resource` operator config override | Deferred and unimplemented. Reuse the existing `SpoolStore::new(data_root, limits)` constructor seam; document any eventual operator key only in `engine/docs2/3_IMPLEMENTATION/OPS_CONFIGURATION.md`, not `engine/docs/CONFIGURATION.md`; keep the default at `1_000_000`, and freeze limits into each manifest so later raises do not retroactively alter in-flight jobs. | Seam: `engine/flapjack-http/src/handlers/migration/spool.rs`; config-doc owner: `engine/docs2/3_IMPLEMENTATION/OPS_CONFIGURATION.md` |
| HA-MEMBERSHIP-UI | Dashboard peer add/remove UI for runtime HA membership | Deferred to the OSS React dashboard / managed-cloud console UI consolidation. Runtime membership shipped (jul20_4pm_2) as an API/CLI surface only — `POST /internal/cluster/peers` (`internal::add_cluster_peer`) and `DELETE /internal/cluster/peers/:node_id` (`internal::remove_cluster_peer`) — and no `engine/dashboard/` peer screen was added, so the consolidation does not have to migrate one. Add the UI on top of the existing endpoints only after that consolidation lands. | API surface owner: `engine/flapjack-http/src/handlers/internal.rs` (add_cluster_peer / remove_cluster_peer); routes: `engine/flapjack-http/src/router.rs`; membership seam: `engine/flapjack-replication/src/manager.rs` (`add_peer` / `remove_peer`) |
| HA-K8S-OPERATOR | Full Kubernetes operator (CRD/controller lifecycle) | Deferred to a separate decision. Engine-owned dead-peer eviction and returning-peer readmission now exist under `HA-AUTOHEAL`, and the official Helm chart remains the ecosystem-standard external-orchestration story. A CRD/controller-based operator would still need to justify declarative lifecycle orchestration, adoption, and recovery beyond the engine's local quorum guard; the engine binary intentionally does not self-provision, and this row does not settle operator demand. | Helm chart owner: `deploy/helm/flapjack/` (`Chart.yaml`, `templates/statefulset.yaml`, `values.yaml`); render-validation oracle: `deploy/helm/flapjack/test_render.sh`; live chart-adoption proof: `deploy/helm/flapjack/test_live_cluster.sh`; local engine auto-heal boundary: `HA-AUTOHEAL`. |

Detailed working checklists and proof-pack session notes may exist in the private
dev repo, but public routing docs should resolve entirely within the synced
public tree.
