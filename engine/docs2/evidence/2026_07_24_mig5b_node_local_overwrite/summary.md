# MIG-5b Node-Local Overwrite Live Proof

## Provenance

- UTC start: 2026-07-24T11:07:30Z
- UTC end: 2026-07-24T11:25:19Z
- Accepted proof HEAD: `4eac347291586441af96957e7f4e60c6dd06dca2`
- Stage 2 product commit: `31787aaf4d117a9183825d0a717081b1e3d779f3`
- Origin main observed during verification: `177f061600cdf6c61a9b6f5189c5a067d8e47f13`
- Live command: `bash engine/tests/algolia_source_export_live.sh --secret-file <redacted>`
- Runtime override: `0`
- Fixture prefix: `fj_stage4_04cd90161cbdeddf`

## Live Servers

- Standalone: `cargo run -p flapjack-server --bin flapjack -- --data-dir <isolated-temp-dir> --auto-port`; launcher PID `86141`; serving PID `86143`; port `60567`; cleanup recorded `launcher_status=0`, `serving_status=gone`.
- HA refusal server: `cargo run -p flapjack-server --bin flapjack -- --data-dir <isolated-temp-dir> --auto-port`; launcher PID `89057`; serving PID `89059`; port `60799`; peer count `1`; cleanup recorded `launcher_status=0`, `serving_status=gone`.

## MIG-5 Assertions

- Source fixture set: 1005 replacement IDs in `logs/mig5-replacement-ids.json`, exactly `doc-0000` through `doc-1004`.
- Stale target seed: 2 IDs in `logs/mig5-stale-ids.json`, `fj_stage4_04cd90161cbdeddf_stale_0001` and `fj_stage4_04cd90161cbdeddf_stale_0002`; both absent from the final target set.
- Overlap writes: attempted `1`, completed `1`, HTTP 200 `1`, retryable refused `0`; published overlap IDs are recorded in `logs/mig5-overlap-published-ids.json`.
- Final query proof: `nbHits=1006`; exact sorted target IDs in `logs/mig5-final-ids.json` matched `logs/mig5-expected-final-ids.json` with no duplicate, missing, unexpected, or success-acknowledged-but-absent IDs.
- Synchronous node-local `overwrite=true`: admitted and completed; response body retained in `logs/mig5-overwrite-migration.json`.
- Async `POST /1/migrations/algolia` with `overwrite:true`: exact HTTP 400 body in `logs/mig5-async-overwrite-refusal.json`; job directory count/content stayed `4 -> 4`.
- HA synchronous `overwrite:true`: exact overwrite-specific HTTP 400 body in `logs/mig5-ha-overwrite-refusal.json`.
- HA synchronous `overwrite:false`: exact HTTP 503 `migration_ha_unsupported` body in `logs/mig5-ha-create-refusal.json`.
- Oracle cleanup: only the recorded lane PIDs above were stopped and waited; `logs/server-cleanup-status.txt` records both owned process pairs gone.

## Command Evidence

- `bash engine/tests/algolia_source_export_live.sh --secret-file <redacted>`: exit `0`; validation-cache summary `PASS: real cargo-run live proof; exact 1006 IDs; overlap 1/1/1/0; async 400 jobs 4->4; HA 400/503`; sanitized receipt in `receipt.json`.
- `cd engine && timeout 600 cargo test -p flapjack --no-fail-fast -- --test-threads=1`: exit `0`; log `release_gates/01_flapjack_tests.log`.
- `cd engine && timeout 600 cargo test -p flapjack-http --no-fail-fast`: exit `0`; log `release_gates/02_flapjack_http_tests.log`.
- `cd engine && timeout 600 cargo test -p flapjack-http --no-fail-fast -- openapi_export_tests`: exit `0`; 8 matching tests passed; log `release_gates/03_openapi_export_tests.log`.
- `cd engine && cargo clippy -p flapjack -p flapjack-http --all-targets -- -D warnings`: exit `0`; log `release_gates/04_clippy.log`.
- `cd engine && cargo fmt --check`: exit `0`; log `release_gates/05_fmt_check.log`.
- `git diff --check`: exit `0`; log `release_gates/06_diff_check.log`.

## Evidence Files

- `receipt.json`: sanitized run receipt, server metadata, and assertion list.
- `logs/`: sanitized request and response bodies, headers, query pages, final ID sets, overlap counters, task-publish receipts, server logs, and cleanup status.
- `migration_exports/`: preserved sanitized migration export artifacts.
- `release_gates/`: sanitized release-gate outputs for the accepted proof HEAD.
