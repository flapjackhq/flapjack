# 2026-07-25 jul25_am_4 Async Migration Replace Publication Repair Receipt

## Purpose

Record Stage 4 validation and merge-readiness evidence for the async create-then-replace migration publication repair. This receipt covers only the Flapjack engine contract and deterministic generated artifacts.

## Commits

- Verified implementation SHA: `bf6dbb13d542243b42a29e4f48e94aface9b67eb`
- Merge baseline refreshed from `origin/main`: `ab0105546d60a915d34861bc8c66055deea8ffa5`

## RED Specimen

- Specimen: `handlers::migration::import_contract_tests::async_import_create_then_overwrite_replaces_exact_target`
- Original Stage 1 semantic refusal: after creating the destination through one successful async job and acknowledging that terminal job through its authenticated owner path, the subsequent `overwrite=true` async replacement was refused before admission instead of replacing the acknowledged target.

## Known-Answer Contract

The passing specimen proves the initial async create terminalized successfully, the authenticated owner ACK was idempotent twice, then the replacement async job received a distinct durable UUID, terminalized successfully, returned a non-resumable wire status, and accepted two idempotent authenticated owner ACKs.

Initial target hits:

- `("doc-1", "Quartz adapter", "hardware", "initial")`
- `("doc-2", "Velvet compass", "navigation", "initial")`
- `("doc-3", "Cedar caliper", "hardware", "initial")`

Replacement target hits:

- `("doc-1", "Quartz adapter", "hardware", "replacement")`
- `("doc-2", "Velvet compass revised", "navigation", "replacement")`
- `("doc-4", "Ivory beacon", "navigation", "replacement")`

Observed assertions include absent `doc-3`, present `doc-4`, replacement revision for `doc-2`, exactly three final hits, no union with the initial target, terminal promoted success, no `resumeHandle`/`checkpointHandle`/`resume`/`resumable`/`operation` status fields, and unchanged terminal phases after each ACK.

## Stage 4 Verification

All commands below were run from `engine` at verified implementation SHA `bf6dbb13d542243b42a29e4f48e94aface9b67eb` unless noted.

- `git fetch origin main:refs/remotes/origin/main`: PASS.
- `git status --short`: PASS, clean tree before validation.
- `git rev-parse HEAD`: PASS, `bf6dbb13d542243b42a29e4f48e94aface9b67eb`.
- `git log --oneline origin/main..HEAD`: PASS, includes the Stage 1 RED pin, async replacement support, recovery, OpenAPI/artifact work, and shell meta-contract throughput repair commits through `bf6dbb13d`.
- `git diff --name-status origin/main...HEAD`: PASS, reviewed path inventory is recorded below.
- `timeout 600 cargo test -p flapjack-http --all-features --locked --lib -- handlers::migration::import_contract_tests::async_import_create_then_overwrite_replaces_exact_target --exact`: PASS, exactly 1 test executed and passed.
- `timeout 600 cargo test -p flapjack-http --all-features --locked -- handlers::migration::import_contract_tests`: PASS, 70 owning migration contract tests passed, retaining ordinary create, HA refusal, async replacement, cancel/ACK, privacy scrub, persisted semantic fallback, and migration publication coverage.
- `timeout 600 cargo test -p flapjack-http --all-features --locked -- handlers::migration::async_status_tests`: PASS, 14 async status tests passed, retaining foreign app/key hiding, cancel/ACK, terminal status, authenticated owner identity, durable phase, and runner-created isolation coverage.
- `timeout 600 cargo test -p flapjack-http --all-features --locked -- auth::tests::route_acl_tests`: PASS, 69 route authorization tests passed with the mounted `auth::tests::route_acl_tests` filter.
- `timeout 600 bash tests/migration_import_contract_test.sh`: PASS, scenario inventory `expected=157 observed=157 pass=157 fail=0 skip=0`; final output `Results: 157/157 passed (0 skipped)` and `All tests passed`; timed evidence `real 596.78`, `user 151.76`, `sys 228.31`.
- `cargo run -p flapjack-http --bin export-openapi`: PASS, regenerated `docs2/openapi.json`.
- `cargo run -p flapjack-http --bin export-openapi -- --output demo-dualclient/public/openapi.json`: PASS, regenerated `demo-dualclient/public/openapi.json`.
- `shasum -a 256 docs2/openapi.json demo-dualclient/public/openapi.json`: PASS after first generator run:
  - `2f3230eb49d2bc9fd9a73d4551bf9ff8dd20be282f1f8371e8c97b83bfa7679b  docs2/openapi.json`
  - `2f3230eb49d2bc9fd9a73d4551bf9ff8dd20be282f1f8371e8c97b83bfa7679b  demo-dualclient/public/openapi.json`
- Repeated both OpenAPI generator commands: PASS.
- Repeated `shasum -a 256 docs2/openapi.json demo-dualclient/public/openapi.json`: PASS, byte-for-byte identical hashes:
  - `2f3230eb49d2bc9fd9a73d4551bf9ff8dd20be282f1f8371e8c97b83bfa7679b  docs2/openapi.json`
  - `2f3230eb49d2bc9fd9a73d4551bf9ff8dd20be282f1f8371e8c97b83bfa7679b  demo-dualclient/public/openapi.json`
- `timeout 600 cargo test -p flapjack-http --all-features --locked -- openapi_export_tests`: PASS, 8 OpenAPI export tests passed and proved both committed artifacts equal `ApiDoc`.
- `cargo fmt --all --check`: PASS.
- `cargo clippy -p flapjack-http --all-targets -- -D warnings`: PASS.
- `git diff --check`: PASS.
- Path-specific `git diff origin/main...HEAD --` review of migration owner/test files and both OpenAPI artifacts: PASS.

## Post-Review Follow-Up

The Stage 4 shell meta-suite later received an in-scope timeout repair in the same changed test surfaces (`engine/tests/migration_import_contract.sh` and `engine/tests/migration_import_contract_test.sh`) after a posthoc review reproduced a late-suite `timeout 600` failure at `real 600.24`.

- Post-review rerun from `engine`: `timeout 600 bash tests/migration_import_contract_test.sh`: PASS, scenario inventory `expected=157 observed=157 pass=157 fail=0 skip=0`; final output `Results: 157/157 passed (0 skipped)` and `All tests passed`; timed evidence `real 420.73`, `user 131.63`, `sys 167.59`.
- The repair kept the production oracle default unchanged and only reduced the meta-suite's fake-server ready poll interval through test-owned environment wiring, closing the shell-suite timeout without changing the covered contract assertions.

## Diff Scope

Reviewed `origin/main...bf6dbb13d542243b42a29e4f48e94aface9b67eb` path inventory:

- `chats/icg/jul25_am_4_async_migration_replace_publication_repair.md`
- `engine/demo-dualclient/public/openapi.json`
- `engine/docs2/openapi.json`
- `engine/flapjack-http/src/background_tasks_tests.rs`
- `engine/flapjack-http/src/handlers/migration/async_status_tests.rs`
- `engine/flapjack-http/src/handlers/migration/import.rs`
- `engine/flapjack-http/src/handlers/migration/import_contract_recovery_tests.rs`
- `engine/flapjack-http/src/handlers/migration/import_contract_tests.rs`
- `engine/flapjack-http/src/handlers/migration/job_runner.rs`
- `engine/flapjack-http/src/handlers/migration/mod.rs`
- `engine/flapjack-http/src/handlers/migration/source_test_support.rs`
- `engine/flapjack-http/src/handlers/migration/spool.rs`
- `engine/flapjack-http/src/handlers/migration/spool_gc_probe_tests.rs`
- `engine/flapjack-http/src/handlers/migration/spool_tests.rs`
- `engine/flapjack-http/src/server_startup_tests.rs`
- `engine/tests/migration_import_contract.sh`
- `engine/tests/migration_import_contract_test.sh`

Production Rust changes remain confined to the existing migration owners:

- `engine/flapjack-http/src/handlers/migration/mod.rs`
- `engine/flapjack-http/src/handlers/migration/import.rs`
- `engine/flapjack-http/src/handlers/migration/job_runner.rs`
- `engine/flapjack-http/src/handlers/migration/spool.rs`

Adjacent changes are existing migration tests/support, startup/background-task tests, repository-owned shell oracle/meta-contract tests, the two generated OpenAPI JSON artifacts, and the existing orchestration/progress artifact.

## Owner Reuse

The implementation reuses the existing owners instead of introducing a second publication or artifact path:

- `admit_async_migration_payload`
- `MigrationJobRunner::{recover_async_jobs_before_serve,recover_async_job,recover_replacement_async_job,recover_cancel_requested_replacement_async_job}`
- `SpoolStore` and `AsyncMigrationPublicationSemantic`
- `import::activate_staged_publication`

## Deployment Status

No deployment, Debbie sync, release workflow, staging/prod mutation, Fjcloud edit, AMI update, HA import enablement, resume support, billing change, or privacy-scrub behavior change was performed.

## Downstream Handoff Contract

This receipt is not a substitute for downstream Fjcloud A3 proofs. Once this verified implementation SHA and this receipt commit are merged, Fjcloud A3 must use Fjcloud's canonical engine updater and independently run:

- `bash scripts/algolia_migration_parity_live_probe.sh --phases overwrite_rerun`
- `bash scripts/algolia_migration_parity_live_probe.sh --phases idempotency`
- `bash scripts/algolia_migration_parity_live_probe.sh --phases overwrite_rerun,idempotency`

Each real-Algolia consumer probe must reach terminal promoted success, durable ACK, exact known-answer parity, and `CLEANUP|...=0`.
