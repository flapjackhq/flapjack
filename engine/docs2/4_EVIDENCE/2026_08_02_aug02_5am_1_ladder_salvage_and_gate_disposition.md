# August 2 5am Ladder Salvage And Gate Disposition

## Stage 4 Local Salvage Receipt

Verdict: `STOPPED` -- this lane salvaged existing proofs and historical
artifacts locally on `2026-08-02`; it did not run a fresh rung and did not
produce a new capacity result.

- Lane: `aug02_5am_1_ladder_salvage_and_ladder_gate_disposition`
- Locality of the preserved ladder evidence: `local-laptop (Darwin/APFS)`
- Historical failed specimen:
  `engine/loadtest/results/20260729T151500Z-jul26_8pm_9-local-repaired-standard/rung_1000000/bulk_replace_status.json`
- Parked worker disposition: `jul30_12am_6` worker `a58d2` is stopped and
  salvaged here; its stale body remains historical.
- Formatting owner:
  `engine/docs2/4_EVIDENCE/2026_07_26_jul26_8pm_9_repaired_path_local_proof_receipt.md`

### What Landed Here

| Change | Proof owner | Exact proof |
|---|---|---|
| Router-owned request-timeout exemption for bounded bulk-replace uploads | `engine/flapjack-http/src/router_inline_tests.rs` | `bulk_replace_upload_outlives_global_request_timeout_until_body_eof` |
| Scope control that normal request timeouts still apply elsewhere | `engine/flapjack-http/src/router_inline_tests.rs` | `global_request_timeout_still_bounds_other_routes` |
| Loadtest harness health ownership guard against foreign-listener false positives | `engine/loadtest/tests/liveness_helpers_acceptance.sh` | `loadtest_health_requires_launched_server_log_owner` |
| Stage 3 bulk-replace status count contract | `engine/flapjack-http/src/router_tests.rs` | `bulk_replace_status_count_matches_three_submitted_documents` |

The Stage 3 status-count owner contract is `N exactly, including the two
sentinels already counted in the submitted body`. The historical `1000001`
artifact is a failed specimen from the old ladder contract, not the current
owner contract.

### Re-Derived Historical Numbers

| Specimen | Re-derived value | Evidence |
|---|---:|---|
| Failed staging specimen export progress | `exportProgress.completed == total == 1000001` | `engine/loadtest/results/20260729T151500Z-jul26_8pm_9-local-repaired-standard/rung_1000000/bulk_replace_status.json` |
| Parked worker 50,000-record throughput, lower-load specimen | `docsPerSecond=755.070297`; `importWallClockMs=66219`; start one-minute load `12.22` | `artifacts/stage_01/20260801T123400Z_50k_results/rung_50000/metrics.json` under worker `a58d2`; load value preserved in that worker's Stage 1 checklist annotations |
| Parked worker 50,000-record throughput, higher-load specimen | `docsPerSecond=760.919190`; `importWallClockMs=65710`; start one-minute load `44.05` | `artifacts/stage_01/20260802T011747Z_50k_s126_results/rung_50000/metrics.json` and `proxy_run_receipt.md` under worker `a58d2` |
| Parked worker 1,000,000-record proxy | `finalCount=1000000`; `importWallClockMs=1243023`; `docsPerSecond=804.490343` | `artifacts/stage_02/20260801T205843Z_s107_1m_proxy_results/rung_1000000/metrics.json` under worker `a58d2` |

The 50,000-record spread is narrow: `755.070297` to `760.919190`
docs/second while the captured one-minute load differed materially. Its worth
of evidence is limited to this local range: it suggests engine work, not host
CPU alone, appears binding across those two local specimens.

### What This Does Not Claim

This is `local-laptop (Darwin/APFS)` evidence captured under fleet load. It is
`not a capacity claim`.

It is not a new guaranteed ceiling, not a reference-locality result, and not a
64M progress claim. The parked 1,000,000-record proxy is diagnostic because its
receipt classifies the run as load-contaminated and invalid for publication.

### Stop Decision

The remaining rungs are not being chased in this lane. The parked ladder gate
still expects `N+1`, while the landed Stage 3 owner contract is `N exactly`.
Continuing the parked run would test the wrong receipt contract. Separately,
the projected 2,000,000-record run is `8.29-8.71` hours, which exceeds the
six-hour budget.

### Validation Disposition

| Command | Result | Evidence |
|---|---|---|
| `RECEIPT=engine/docs2/4_EVIDENCE/2026_08_02_aug02_5am_1_ladder_salvage_and_gate_disposition.md; test -s "$RECEIPT"; grep -Fq 'local-laptop (Darwin/APFS)' "$RECEIPT"; grep -Fq 'not a capacity claim' "$RECEIPT"; grep -Fq 'aug02_5am_1' "$RECEIPT"` | `PASS` | Receipt exists and contains the required locality, non-claim, and lane markers. |
| `grep -Fq 'dispatched+stopped(worker a58d2' chats/icg/jul30_12am_6_repaired_ladder_local_verdict.md` | `PASS` | Parked lane has exactly the required disposition stamp at the top. |
| `git diff --name-only origin/main...HEAD -- ROADMAP.md` | `PASS` | Printed nothing; this stage did not mutate `ROADMAP.md`. |
| `cd engine && timeout 3600 cargo test --workspace` | `FAIL` | One failure: `index::manager::write::mutation_fence::durable_acks_survive_replace_index_contents_mutation_fence`, assertion `left: 17`, `right: 24`; `2197 passed`, `1 failed`, `8 ignored`. |
| `cd /tmp/flapjack_stage4_origin_main_baseline_58346/engine && timeout 600 cargo test -p flapjack --lib -- index::manager::write::mutation_fence::durable_acks_survive_replace_index_contents_mutation_fence` | `PASS` | Detached `origin/main` focused baseline passed: `1 passed`, `0 failed`. |
| `cd engine && timeout 600 cargo test -p flapjack --lib -- index::manager::write::mutation_fence::durable_acks_survive_replace_index_contents_mutation_fence` | `PASS` | Current-tree focused rerun passed: `1 passed`, `0 failed`. |
| `cd engine && timeout 3600 cargo test --workspace` at committed `854b63023` | `FAIL` | The earlier mutation-fence failure did not recur, but `legacy_quickstart_routes_are_removed_even_with_valid_auth` failed: expected `403`, got `404`. |
| `cd /tmp/flapjack_stage4_origin_main_baseline_58346/engine && timeout 600 cargo test -p flapjack --test test_legacy_quickstart_routes -- legacy_quickstart_routes_are_removed_even_with_valid_auth` | `FAIL` | Detached `origin/main` reproduced the same `403` versus `404` failure, so this is a pre-existing baseline failure outside the receipt/stamp change. |
| `cd engine && timeout 600 cargo test -p flapjack --test test_legacy_quickstart_routes -- legacy_quickstart_routes_are_removed_even_with_valid_auth` | `FAIL` | Current-tree focused rerun reproduced the same pre-existing baseline failure. |

#### ROADMAP CORRECTION REQUIRED

This note belongs in this receipt and must not mutate `ROADMAP.md` in this
stage. FJ-6 needs exactly these three roadmap corrections in a later roadmap
owner pass:

| Row | Required correction |
|---|---|
| Bulk-replace request-timeout defect | Attribute the defect to the router timeout layer and the landed router-owned exemption proof. |
| Harness-ownership false-positive | Attribute the false positive to missing server-log ownership confirmation and the landed loadtest health owner guard. |
| `MIG-17` sibling reasoning | Record that the parked quiet-host lane behaved as abandoned without an explicit stop decision. |

No ladder run, release work, branch surgery, old July 30 receipt resurrection,
or `ROADMAP.md` mutation is part of this Stage 4 closeout.
