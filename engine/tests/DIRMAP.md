<!-- [scrai:start] -->
## tests

| File | Summary |
| --- | --- |
| algolia_compat_bootstrap.sh | Stub summary for algolia_compat_bootstrap.sh. |
| build_identity_sync_contract.sh | Dev-only contract for Debbie cold-copy build identity materialization.

Usage:
  bash tests/build_identity_sync_contract.sh
  bash tests/build_identity_sync_contract.sh --shape-only. |
| ci_test_timeout_cap_acceptance.sh | Stub summary for ci_test_timeout_cap_acceptance.sh. |
| doc_sync_helpers.sh | Stub summary for doc_sync_helpers.sh. |
| integration_smoke.sh | Stub summary for integration_smoke.sh. |
| managed_fleet_ha_probe.sh | managed_fleet_ha_probe.sh — W0 decision-gate probe: prove every current
staging/prod migration-target Flapjack node, and every future provisioning
path, is standalone (peers=[]).

Emits a structured receipt to <artifact-dir>/receipt.json consumed by
verify_managed_fleet_ha_receipt.sh and the Wave-0.5 wrap gate.

THREE arms:
  1. |
| mutate_dashboard_algolia_ci_wiring_probe.py | Stub summary for engine/tests/mutate_dashboard_algolia_ci_wiring_probe.py. |
| no_green_by_absence.sh | no_green_by_absence.sh — local detector for guards that pass by missing inputs.

This is deliberately a negative gate: every reported path:line is a failure.
The active ratchet is owned by green_by_absence_allowlist.txt. |
| no_vendor_host_in_shipped_sdk.sh | no_vendor_host_in_shipped_sdk.sh — Source contract for the shipped SDK mirror.

Flapjack ships a self-hosted, registry-free search engine. |
| provision_algolia_sandbox_egress.sh | shellcheck disable=SC1091,SC2016. |
| publication_create_only_race.rs | Stub summary for engine/tests/publication_create_only_race.rs. |
| publication_repair_cli_live.sh | Live contract for the shipped repair-publication CLI against generated crash layouts. |
| readme_api_smoke.sh | readme_api_smoke.sh — Validate README curl examples against a local build.

Builds the flapjack binary (or uses $FLAPJACK_BIN), starts it on an
ephemeral port with auth enabled, and exercises the README's local API curl
examples. |
| readme_quickstart_smoke.sh | readme_quickstart_smoke.sh - Cold-install README quickstart smoke.

Installs Flapjack with the public installer into an isolated temp directory,
starts the installed binary with first-boot admin-key generation, and checks
the README quickstart's batch, task, and typo-tolerant search contract. |
| search_pagination_live_http.sh | search_pagination_live_http.sh — Prove the Stage 1/2 pagination known-answer
contract at the served HTTP boundary, through the real flapjack-server binary.

The in-process Rust KAT
(flapjack-http/src/handlers/search/stage5_integration_tests/search_pagination_known_answer.rs)
exercises the handler router directly. |
| upgrade_smoke.sh | Stub summary for upgrade_smoke.sh. |
| validate_algolia_sandbox_cleanup_ledger.py | Validate the Algolia measurement sandbox cleanup ledger contract. |
| validate_doc_links.sh | validate_doc_links.sh — Check that internal markdown links resolve to real files.

Scans README.md, ROADMAP.md, engine/README.md, and engine/docs2/FEATURES.md
for relative markdown links (excluding http/https/mailto/anchors) and verifies
each target exists on disk.

Usage:
  ./engine/tests/validate_doc_links.sh. |
| validate_sync_surface.sh | Stub summary for validate_sync_surface.sh. |
| verify_managed_fleet_ha_receipt.sh | verify_managed_fleet_ha_receipt.sh — Wave-0.5 wrap-gate verifier for the
managed-fleet HA standalone receipt emitted by managed_fleet_ha_probe.sh.

Exit codes:
  0  receipt ACCEPTED (gate green)
  1  receipt REJECTED  (a `REJECT: <reason>` line is printed to stderr)
  2  usage / system error

It REJECTS: a prose (non-numeric) value, a dirty/local checkout, a missing
environment, a filtered/zero target set, a nonzero peer on any reachable
target, any omitted/unpinned provisioning path, a stale receipt (SHA mismatch
vs the wrap gate's expected commits), a skip/fail on the inventory path, and a
receipt where EVERY target is corroboration_unavailable AND the provisioning
arm is absent. |

| Directory | Summary |
| --- | --- |
| common | This directory provides shared test utilities, including an Algolia vendor HTTP transport layer with request routing, URL encoding, and polling helpers for live test drivers. |
| fixtures | — |
| ha_contracts | The ha_contracts directory contains integration tests for high-availability and replication contracts in Flapjack, covering index ownership semantics, per-tenant sequencing guarantees, replica freshness validation, restart recovery, and split-brain precedence handling. |
<!-- [scrai:end] -->
