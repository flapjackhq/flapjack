# Flapjack - Roadmap

**Last updated:** 2026-06-10
**Ledger policy:** `ROADMAP.md` is the only root open-work ledger. Mission,
scope, and strategic priority order live in [`PROJECT_OVERVIEW.md`](PROJECT_OVERVIEW.md).
Shipped capability status lives in [`engine/docs2/FEATURES.md`](engine/docs2/FEATURES.md),
release history lives in [`CHANGELOG.md`](CHANGELOG.md), and completed work
history is routed to `implemented/2026_06_05_history.md`.

**Last shipped release:** [v1.0.10](https://github.com/flapjackhq/flapjack/releases/tag/v1.0.10) (2026-06-09). Detailed release history lives in [`CHANGELOG.md`](CHANGELOG.md).

**ID prefixes:** `RF-*` = foundational refinement track; `PL-*` =
launch-hardening / operational-polish track. IDs are stable identifiers, not
priority rank.

## Active

| ID | Work Item | Current State | Evidence / Owner |
|----|-----------|---------------|------------------|
| RF-4 | Runbooks iteration | Open-ended operational follow-through. Continue refining runbooks from incident learnings. | [`engine/docs2/3_IMPLEMENTATION/OPERATIONS.md`](engine/docs2/3_IMPLEMENTATION/OPERATIONS.md) |
| PL-10 | Write-path saturation under sustained load | Open for v1.1 architecture work. The v1.0.4 batch-size knob and v1.0.5 `uplift_ratio >= 1.50` acceptance gate are shipped and verified, but cross-node fanout remains constrained by the single-writer Tantivy mutex. | Stage 6 classification: `engine/docs/research/pl10_stage6_dual_scenario_classification.md`; canonical benchmark owner: [`engine/loadtest/BENCHMARKS.md`](engine/loadtest/BENCHMARKS.md); proof directories: `engine/loadtest/results/20260528T062547Z-pl10-stage6-dual-scenario/`, `engine/loadtest/results/20260601T202043Z-pl10-saturation-acceptance/`, `engine/loadtest/results/20260601T203717Z-pl10-saturation-acceptance/`, `engine/loadtest/results/20260601T204623Z-pl10-saturation-acceptance/` |
| HA-FLAKE | HA snapshot test flake remediation | Fix verified and leaky-pass sites closed; keep future HA regression signal protected. Not v1.0.x blocking. | Fix owner paths: `engine/flapjack-http/src/startup_catchup.rs`, `engine/flapjack-replication/src/manager.rs`, `engine/src/analytics/writer.rs`; regression contract: `engine/tests/test_snapshot_import_failure_contract.rs`; proof: `docs/reference/research/may31_eve_ha_snapshot_flake_verify_proof.md` |
| PL-8 | Nginx restart-window write-loss recovery residual routing | Core restart-window write-loss fix is closed; residual tracking remains here so HA docs keep one open-work owner. HA convergence posture is **bounded convergence** after L1 anti-entropy fix `066549d5`; remaining saturation routes to PL-10 and cross-node idempotency routes to ADR-0005 OQ4. | Canonical evidence owner: [`engine/loadtest/BENCHMARKS.md`](engine/loadtest/BENCHMARKS.md); narrative seam: `engine/docs2/3_IMPLEMENTATION/decisions/active/0004_ha_convergence_reversal.md` |

## Planned

| ID | Work Item | Planned Direction | Evidence / Owner |
|----|-----------|-------------------|------------------|
| ADR-0005 OQ4 | Cross-node failover idempotency dedup | v1.1 design work. Node-local restart-durable idempotency is shipped at `${FLAPJACK_DATA_DIR}/_idempotency/cache.db`; cross-node dedup needs a durable coordination layer before a peer restart during the same idempotency-key window can be single-execution across nodes. | `engine/docs2/3_IMPLEMENTATION/decisions/active/0005_nginx_restart_window_write_recovery.md` |
| PL-2 | `cargo-nextest` migration re-evaluation | Re-evaluate around 2026-11-26 against accumulated hang-frequency data. If PL-1 plus test-hang discipline have not covered 95% of hangs, plan `.config/nextest.toml` per-test timeouts, a `.cargo/config.toml` alias, and CI workflow migration. | Existing CI-side cap owner: `engine/tests/ci_test_timeout_cap_acceptance.sh` |

Detailed working checklists and proof-pack session notes may exist in the private
dev repo, but public routing docs should resolve entirely within the synced
public tree.
