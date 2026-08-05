# 2026-07-30 Disk Exhaustion Durability Receipt

## Stage 1 - denominator and rig design

Branch identity: `batman/jul30_12am_5_disk_exhaustion_durability` at `2c131f31f2d721b618df68b7c23a1596ebb77338` (`2c131f31f`).

Purpose: establish the PR-10 coverage denominator and the bounded full-disk rig contract before any destructive product probe is built.

### Stage contract and no-edit boundaries

Sources:

- Stage 1 contract: matt project `stages.md:1-31`.
- Complete lane preamble: `chats/icg/jul30_12am_5_disk_exhaustion_durability.md:1-190`.
- PR-10 row: `engine/docs2/FEATURES.md:427`.

Hard boundaries recorded from the stage and lane preamble:

- Do not edit `engine/flapjack-server/tests/**`, including `crash_durability_test.rs` and `support/mod.rs`; Wave-2 sibling `jul30_12am_4` owns that surface.
- Do not edit `engine/loadtest/lib/loadtest_shell_helpers.sh`, `engine/loadtest/scale_ladder.sh`, or `engine/loadtest/soak_proof.sh`; source or read existing helper seams only.
- Do not edit `engine/dashboard/**`.
- Do not edit the four ledger files: `ROADMAP.md`, `PROJECT_OVERVIEW.md`, `engine/docs2/FEATURES.md`, or `CHANGELOG.md`; Wave 4 narrows PR-10 from this receipt.
- Do not build the other PR-10 modes in this lane.
- `engine/src/error.rs` is read-only in Stage 1. It may be edited only in Stage 3 if the experiment proves a small narrow defect and the Wave-2 priority lane has merged; otherwise write a gap spec instead.

The rig must never fill host storage. It must use a small file-backed detachable volume, install cleanup before creating an image or process, detach and delete only its own run artifacts on every exit path, and exit non-zero rather than falling back to host storage when isolation fails. Evidence must be preserved before destructive teardown for every non-PASS, non-zero, or interrupted outcome, following the pattern in `engine/_dev/s/manual-tests/ha-soak-test.sh`.

### PR-10 denominator at HEAD

Evidence threshold:

- Covered: an automated test or retained acceptance owner exercises the named failure mode itself and asserts durable post-fault correctness.
- Partly covered: an automated test exercises an adjacent lower-level or topology behavior but not the named adversarial mode.
- Uncovered: no automated test or probe exercises the named fault.

| PR-10 mode ID | Named mode | HEAD status | Evidence and rationale |
|---|---|---|---|
| `kill.server.mid.write` | kill-server-mid-write | Covered | `engine/flapjack-server/tests/crash_durability_test.rs:205` defines `acknowledged_batch_write_remains_searchable_after_crash_restart`, waits for the task to publish, kills/restarts the server, and asserts the acknowledged document remains searchable. `engine/flapjack-server/tests/restart_during_writes_test.rs:15` defines `acknowledged_writes_remain_searchable_across_restart_during_active_traffic`, keeps active writes against a stable bind address, restarts during traffic, and asserts every acknowledged ID is searchable after restart. `engine/flapjack-server/tests/idempotency_restart_durability_test.rs:11` defines `restart_replays_cached_response_and_preserves_single_execution_state`, restarts after an accepted idempotent mutation, asserts the replay returns the exact cached response, and proves the mutation executed once. These are three pieces of evidence for one PR-10 mode because all three exercise process death/restart around acknowledged write durability; they are not three denominator entries. |
| `fill.disk.mid.write` | fill-disk-mid-write | Uncovered | Current validation command `grep -rln 'ENOSPC\|No space left\|disk_full' --include='*.rs' --include='*.sh' engine/ \| sort` returned no files at this branch point. No HEAD owner exhausts a real filesystem or bounded mounted image during active writes. |
| `partition.replica.from.primary` | partition-replica-from-primary | Partly covered | `engine/examples/ha-cluster/README.md:8-13` scopes HA evidence to nginx-routed single-node outage, peer oplog replication, restarted-node pre-serve catch-up, and analytics fan-out; it explicitly does not prove leader election, promotion, or load-balancer HA. `engine/examples/ha-cluster/test_ha_autoheal.sh:211-252` stops peers/nodes and verifies majority refusal, legal single-node eviction, readmission, and catch-up. `engine/_dev/s/manual-tests/ha-soak-test.sh:432-455` rotates node restarts during k6 traffic and samples convergence. `engine/loadtest/tests/ha_soak_acceptance.sh:107-153` and `engine/loadtest/tests/ha_soak_loadtest_durability_acceptance.sh:35-75` assert evidence-preservation contracts. This is network-isolation and restart/catch-up evidence, not a true replica partition from primary under an asserted split-brain/replica-primary contract. |
| `OOM.kill.and.restart` | OOM-kill-and-restart | Uncovered | `engine/loadtest/scenarios/memory-pressure.js` observes memory-pressure middleware behavior through `/health`, `/internal/status`, search, and write responses; it does not force an operating-system OOM kill or assert post-OOM restart recovery. The other scenarios are throughput/soak/spike/write-load scripts, not process OOM-kill probes. |

### FlapjackError full-disk prediction

Prediction only, not observed write-path behavior: absent a narrower interception, an ENOSPC `std::io::Error` becomes `FlapjackError::Io`, maps to HTTP 500, and uses the sanitized internal-error response body.

Trace:

- `engine/src/error.rs:109-112`: `impl From<std::io::Error> for FlapjackError` converts every `std::io::Error` into `FlapjackError::Io(e.to_string())`.
- `engine/src/error.rs:206`: `FlapjackError::Io(_)` maps to `StatusCode::INTERNAL_SERVER_ERROR`.
- `engine/src/error.rs:331-334`: `io_error_is_500` asserts the Io variant returns HTTP 500.
- `engine/src/error.rs:707-718`: `internal_errors_dont_leak_details` asserts Io response bodies do not leak file paths.
- `engine/src/error.rs:798-804`: `api_message()` maps internal variants including `FlapjackError::Io(_)` to `Internal server error`.
- `engine/src/error.rs:827-849`: `IntoResponse` uses `status_code()` and the sanitized `api_message()` in the JSON body.

Single-line contract for Stage 2/3 validation: `FlapjackError::Io -> HTTP 500` with body fields `{ "message": "Internal server error", "status": 500 }` if ENOSPC reaches the generic `std::io::Error` conversion.

Open question: whether the active write path returns this generic Io response, a Tantivy-specific internal error, a panic, a timeout, or a narrower typed rejection when the bounded filesystem is actually full.

### Existing evidence-capture pattern to reuse

`engine/_dev/s/manual-tests/ha-soak-test.sh` is the cleanup and evidence-ordering reference:

- `engine/_dev/s/manual-tests/ha-soak-test.sh:38-43` tracks `K6_PID`, `INTERRUPTED_EXIT_CODE`, and `CLEANUP_COMPLETE`.
- `engine/_dev/s/manual-tests/ha-soak-test.sh:323-365` makes cleanup reentrant, stops only the tracked k6 child, copies node `/data` snapshots on non-converged outcomes, and skips destructive `docker compose down -v` to preserve evidence.
- `engine/_dev/s/manual-tests/ha-soak-test.sh:367-390` finalizes interrupted runs by sampling state, setting `CONVERGENCE_RESULT=interrupted`, writing the summary, and printing the artifact path.
- `engine/_dev/s/manual-tests/ha-soak-test.sh:473-476` installs EXIT/HUP/INT/TERM traps.

Acceptance style to follow:

- `engine/loadtest/tests/ha_soak_acceptance.sh:7-45` accumulates assertion failures instead of stopping at the first missing proof.
- `engine/loadtest/tests/ha_soak_acceptance.sh:107-153` asserts the HA soak cleanup, classification, snapshot, and interrupt-finalization contract with concrete text patterns.
- `engine/loadtest/tests/ha_soak_loadtest_durability_acceptance.sh:35-75` names exact required patterns for evidence preservation and cleanup split.

Reuse decision: keep disk-image lifecycle local to `engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh`; use existing loadtest helpers read-only only where their public seams fit generic operations. Do not copy generic server/build/liveness helpers and do not extract a new shared helper boundary in this lane.

### Initial bounded rig parameters

Stage 2 should start with these parameters:

| Parameter | Initial value | Reasoning and adjustment criteria |
|---|---:|---|
| Bounded image size | 32 MiB APFS image | The live preflight proved a 32 MiB APFS image can be created and mounted on this host. It leaves room for filesystem metadata and Tantivy initialization while being small enough to exhaust quickly. If Tantivy cannot initialize, increase to the next small bound, 64 MiB, and record the failed 32 MiB evidence. |
| Document payload size | 80 KiB payload string per document, plus `objectID` and searchable marker fields | `engine/flapjack-http/src/handlers/objects/mod.rs:35-39` makes the live default per-record limit `102_400` bytes through `max_record_bytes()`, unless `FLAPJACK_MAX_RECORD_BYTES` is explicitly overridden. The Stage 2 initial rig uses no override and keeps each deterministic payload strictly below that default at 81,920 bytes, leaving about 20 KiB for JSON field overhead before `check_record_size()` can reject the record for size instead of disk pressure. If JSON overhead unexpectedly trips `DocumentTooLarge`, reduce the payload to 64 KiB and record the rejection body. |
| Batch size | 4 documents per batch | About 320 KiB of payload per batch before index overhead, producing fast pressure while preserving exact per-object oracle recording. If write latency or request limits dominate before free space falls, reduce to 2; if exhaustion takes longer than the runtime bound, increase to 8 only after verifying body limits. |
| Maximum batches | 128 batches | About 512 documents and roughly 40 MiB of source payload pressure against a 32 MiB image, before Tantivy/index overhead, enough to reach full-disk pressure quickly while keeping every single record below the live default server limit. Stop earlier on the first captured non-2xx rejection. |
| Maximum write phase runtime | 60 seconds | A timeout is inconclusive, not a pass. If 60 seconds expires without a bounded-mount exhaustion signal and active non-2xx write rejection, fail the probe and preserve evidence. |

### Full-disk detection contract

Full disk is proven only by this conjunction:

1. The resolved `FLAPJACK_DATA_DIR` is verified before server startup to be on the newly attached image, using `df` and mount metadata for the exact mount point returned by `hdiutil attach`.
2. At rejection time, the bounded mount reports exhaustion or near-zero writable space. Stage 2 should record `df -k` for the mount and treat available space at or below 1024 KiB as near-zero unless the filesystem reports a hard no-space error first.
3. The active write receives and captures a non-2xx HTTP rejection with stable body fields.

Timeouts, slow writes, client disconnects without a response, and host-filesystem `df` output are inconclusive and must fail the probe. Unsupported or failed isolation must exit non-zero before starting the product.

### Lifecycle ordering for Stage 2 harness

Required ordering:

1. Create a unique run ID, image path, and volume label.
2. Install EXIT/HUP/INT/TERM cleanup before creating any image or process.
3. Refuse pre-existing or colliding image paths, volume labels, devices, or target mount paths.
4. Create the bounded image with explicit size.
5. Attach with `hdiutil attach`, capture the exact returned device and mount point, and verify that mount is backed by that image.
6. Resolve `FLAPJACK_DATA_DIR` under the mount and verify via `df`/mount metadata that it is on the bounded image before starting `flapjack-server`.
7. Start only the product process for this run and retain its exact PID.
8. Write batches, append every acknowledged `objectID` to an independently authored oracle, and stop on the first active non-2xx rejection or inconclusive timeout.
9. Preserve required evidence outside the image before detach for every non-PASS, non-zero, or interrupted outcome.
10. Stop only the exact server PID started by the harness.
11. Detach only the captured device.
12. Delete only the run's image and temporary parser files.
13. Verify the unique attachment and image are absent after cleanup.

### Stage 2 evidence schema

The harness must copy or write these fields to a durable evidence directory outside the image:

- `oracle_acknowledged_ids.txt`: one acknowledged `objectID` per line, written independently from recovery search.
- `rejection.json`: rejection status, headers needed for stable assertions, raw body, and parsed stable body fields.
- `bounded_mount_identity.txt`: image path, volume label, attached device, mount point, resolved `FLAPJACK_DATA_DIR`, and mount/df evidence before server start.
- `df_at_rejection.txt`: bounded-mount `df` and available KiB at rejection.
- `server.log`: server stderr/stdout and any panic/backtrace evidence.
- `server_exit.txt`: exact server PID, exit status if exited, and whether the harness stopped it.
- `recovered_ids.txt`: exact recovered ID set after freeing space and restarting against the same data directory.
- `post_restart_write.json`: status and stable body fields for one successful post-restart write.
- `script_exit.txt`: script exit code and `INTERRUPTED_EXIT_CODE`.
- `summary.json`: one machine-readable summary carrying at minimum `outcome`, `acknowledged_count`, `recovered_count`, `rejection_status`, `evidence_dir`, `script_exit_code`, and `interrupted_exit_code`.

Evidence must be copied outside the image before detach on every non-PASS, non-zero, or interrupted outcome.

### Acceptance consumer contract

`engine/loadtest/tests/disk_exhaustion_acceptance.sh` is the sole automated consumer of the disk-exhaustion evidence. Its first red state must name only missing probe evidence, must not invoke `flapjack-server`, and must not mount or detach volumes. Once evidence exists, it must assert:

- no panic or crash evidence in `server.log`/`server_exit.txt`;
- honest non-2xx rejection status and stable body fields from `rejection.json`;
- exact equality of `oracle_acknowledged_ids.txt` and `recovered_ids.txt`, not counts alone;
- successful post-restart write from `post_restart_write.json`;
- bounded-mount identity and `df_at_rejection.txt` prove the write data directory was on the attached image;
- `summary.json` carries consistent counts, outcome, rejection status, and durable evidence path.

### Darwin preflight result

First attempt:

- Command class: unique 32 MiB APFS `hdiutil create` / `attach` / identity / `detach` / delete.
- Result: create=0, attach=0, identity=1, detach=0, cleanup=0.
- Diagnosis: the initial parser looked only at `system-entities.0.mount-point`; APFS returned the mounted filesystem on a later plist entity. The image was detached and deleted. This was a parser defect in the preflight command, not a host-safety failure.

Corrected preflight:

- Run ID: `fjstage1v2_16758_20260730T130459Z`.
- Image: `/tmp/fjstage1v2_16758_20260730T130459Z.dmg`.
- Volume: `fjstage1v2_16758_20260730T130459Z`.
- Device: `/dev/disk5s1`.
- Mount: `/Volumes/fjstage1v2_16758_20260730T130459Z`.
- Results: create=0, attach=0, identity=0, detach=0, cleanup=0.

Stage 2 must parse the plist by scanning `system-entities` for the entity whose `mount-point` equals the unique volume mount and whose `dev-entry` is present.

### Stage 1 receipt repair gate

Live size-limit owner: `engine/flapjack-http/src/handlers/objects/mod.rs:35-39` defines `max_record_bytes()` as `FLAPJACK_MAX_RECORD_BYTES` parsed from the environment or `unwrap_or(102_400)`. This receipt's initial Stage 2 rig does not require a server-limit override; therefore the deterministic `80 KiB` payload is the canonical initial value and must remain strictly below `102,400` bytes. The bounded image remains `32 MiB`; `80 KiB * 4 documents * 128 batches` applies about `40 MiB` of source payload pressure before index overhead, so the corrected rig still exceeds the image size while avoiding a per-record size rejection.

Fail-capable receipt owner: `engine/loadtest/tests/disk_exhaustion_stage1_receipt_acceptance.sh`.

Red proof before this repair:

```text
payload_bytes=262144 must be below live_default=102400
FAIL: disk exhaustion Stage 1 receipt contract
 - receipt payload size must be strictly below live max_record_bytes default
 - receipt must contain the Stage 1 repair gate evidence section
 - receipt must state stage1-receipt-entry-gate: PASS
 - receipt must explicitly prove trap setup appears before image create/attach
 - receipt must record EXIT cleanup trap
 - receipt must record HUP cleanup trap
 - receipt must record INT cleanup trap
 - receipt must record TERM cleanup trap
 - receipt must record zero exits for create/attach/identity/detach/image-removal/zero-leak
```

Lifecycle-ordering proof: the corrected preflight used this ordering excerpt. The trap setup appears before image create/attach.

```bash
cleanup_signal() {
  local code="$1"
  cleanup
  exit "$code"
}
cleanup() {
  if [[ -n "${device:-}" ]]; then
    hdiutil detach "$device" >/tmp/${run_id}_detach_cleanup.out 2>/tmp/${run_id}_detach_cleanup.err || true
  fi
  if [[ -e "$image_path" ]]; then
    rm "$image_path" || true
  fi
}
trap cleanup EXIT
trap 'cleanup_signal 129' HUP
trap 'cleanup_signal 130' INT
trap 'cleanup_signal 143' TERM

if [[ -e "$image_path" ]] || hdiutil info | grep -Fq "$volume_label"; then
  echo "COLLISION=1 run_id=$run_id image=$image_path volume=$volume_label"
  exit 2
fi

hdiutil create -size 32m -fs APFS -volname "$volume_label" "$image_path"
hdiutil attach -plist "$image_path"
```

Corrected preflight transcript:

```text
RUN_ID=fjstage1repair_45533_20260730T133623Z
IMAGE=/tmp/fjstage1repair_45533_20260730T133623Z.dmg
VOLUME=fjstage1repair_45533_20260730T133623Z
ATTACHED_DEVICE=/dev/disk5s1
MOUNT=/Volumes/fjstage1repair_45533_20260730T133623Z
create=0 attach=0 identity=0 detach=0 image-removal=0 zero-leak=0
```

stage1-receipt-entry-gate: PASS.

Stage 2 may now create its two scripts: `engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh` as the bounded destructive probe and `engine/loadtest/tests/disk_exhaustion_acceptance.sh` as the sole automated evidence consumer. The new Stage 1 receipt-contract check gates only this receipt's payload sizing and trap-before-image lifecycle proof; it does not invoke the product and does not mount or detach volumes.

## Stage 3 - measured result (routed product gap)

Stage 3 ran the bounded Darwin APFS harness `engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh` against the live `flapjack-server` binary with `FLAPJACK_DATA_DIR` resolved onto a 32 MiB attached image. The result is **not acceptance-green**. It is a reproducible product gap in write-queue durable admission / commit finalization, routed forward with the retained specimens as proof.

Path convention in this section: evidence directories and `summary.json.evidence_dir` are recorded repo-relative. The on-disk artifacts store an absolute worktree path in `evidence_dir`; that prefix is elided here as `<repo-root>/` because worktree-absolute paths are session-ephemeral and must not be written into committed docs. Every other field of every quoted summary line is verbatim.

### Retained completed full-disk specimens

Both retained specimens were produced at source SHA `7aaa08f7fa300e7fa6cdac4ca0c440c2e2076a16` (`7aaa08f7f`), asserted equal to the then-current HEAD by the harness itself via `summary.json.source_sha`.

| Run | Source SHA | Pre-run load averages | Evidence directory | Acceptance exit |
|---|---|---|---|---:|
| 1 | `7aaa08f7fa300e7fa6cdac4ca0c440c2e2076a16` | `13:03 up 1 day, 16:12, 7 users, load averages: 9.13 15.11 22.48` | `engine/loadtest/results/20260730T170347Z-disk-exhaustion` | `1` |
| 2 | `7aaa08f7fa300e7fa6cdac4ca0c440c2e2076a16` | `13:07 up 1 day, 16:16, 7 users, load averages: 23.89 24.22 25.15` | `engine/loadtest/results/20260730T170751Z-disk-exhaustion` | `1` |

Run 1 summary line (`engine/loadtest/results/20260730T170347Z-disk-exhaustion/summary_line.txt`):

```text
disk_exhaustion_summary outcome=FAIL_RECOVERY_MISMATCH acknowledged_count=76 recovered_count=80 rejection_status=500 evidence_dir=<repo-root>/engine/loadtest/results/20260730T170347Z-disk-exhaustion source_sha=7aaa08f7fa300e7fa6cdac4ca0c440c2e2076a16 script_exit_code=99 interrupted_exit_code=0
```

Run 2 summary line (`engine/loadtest/results/20260730T170751Z-disk-exhaustion/summary_line.txt`):

```text
disk_exhaustion_summary outcome=FAIL_RECOVERY_MISMATCH acknowledged_count=76 recovered_count=80 rejection_status=500 evidence_dir=<repo-root>/engine/loadtest/results/20260730T170751Z-disk-exhaustion source_sha=7aaa08f7fa300e7fa6cdac4ca0c440c2e2076a16 script_exit_code=99 interrupted_exit_code=0
```

Acceptance exit `1` is the routed-gap proof, not a harness defect. Stage 3 ran `DISK_EXHAUSTION_EVIDENCE_DIR=<dir> bash engine/loadtest/tests/disk_exhaustion_acceptance.sh` against each retained directory and both exited `1`. Stage 4 re-ran the explicit form against `engine/loadtest/results/20260730T170347Z-disk-exhaustion` at HEAD `ac6aae9303795e47cf46258c61e9ff0bbc8cf479` and reproduced exit `1` with the same five failing assertions; the full transcript is in the Stage 4 closeout section below.

Bounded-mount identity proves the write data directory was on the attached image and not on host storage. Run 1 `bounded_mount_identity.txt` recorded `attached_device=/dev/disk5s1`, `mount_point=/Volumes/fjdisk_32203_20260730T170347Z`, `flapjack_data_dir=/Volumes/fjdisk_32203_20260730T170347Z/flapjack-data`, and a pre-start `df` of `32728` 1024-blocks with `32068` available. Run 1 `df_at_rejection.txt` recorded `available_kib=756` at `98%` capacity on the same device; Run 2 recorded `available_kib=512` at `99%`. Both are at or below the receipt's near-zero threshold of 1024 KiB, and both runs also produced a hard `ENOSPC` from the filesystem, so the full-disk conjunction from the Stage 1 detection contract is satisfied by direct evidence rather than by the `df` threshold alone.

Reproducibility beyond the two retained specimens: all five *complete* `*-disk-exhaustion` evidence directories present in `engine/loadtest/results/` at HEAD `ac6aae9303795e47cf46258c61e9ff0bbc8cf479` (`20260730T170347Z`, `20260730T170751Z`, `20260730T170946Z`, `20260730T171107Z`, `20260730T171233Z`) carry the identical summary shape `outcome=FAIL_RECOVERY_MISMATCH acknowledged_count=76 recovered_count=80 rejection_status=500 ... script_exit_code=99`. The gap is 5/5 reproducible on this host at that source SHA, not a two-sample coincidence.

### Interrupted cleanup proof

Produced at source SHA `4ec1516de0f9964906e762bf05b587e8bb7578a9` (`4ec1516de`, "Handle exact PID interrupts in disk harness"), after the Stage 3 harness signal-disposition fix.

- Pre-run load averages: **not recorded**. The Stage 3 handoffs captured a pre-run `uptime` line for the two completed specimens but not for this interrupted probe, and no load-average value for it exists in any retained artifact. It is not reconstructable after the fact and is deliberately left unstated here rather than back-filled. This is a small evidence-completeness gap in the interrupted-run record, not a correctness problem with the interrupt proof itself: `script_exit_code`/`interrupted_exit_code` and the leak checks are the load-bearing fields for that proof, and all are recorded.
- Evidence directory: `engine/loadtest/results/20260730T172514Z-disk-exhaustion`.
- Summary line (`summary_line.txt`):

```text
disk_exhaustion_summary outcome=INCONCLUSIVE acknowledged_count=0 recovered_count=0 rejection_status=0 evidence_dir=<repo-root>/engine/loadtest/results/20260730T172514Z-disk-exhaustion source_sha=4ec1516de0f9964906e762bf05b587e8bb7578a9 script_exit_code=130 interrupted_exit_code=130
```

- `script_exit.txt` records `script_exit_code=130` and `interrupted_exit_code=130`. Only `kill -INT <exact harness PID>` was used; no broad process pattern was issued.
- Durable artifacts written before teardown: `bounded_mount_identity.txt`, `df_at_rejection.txt`, `oracle_acknowledged_ids.txt`, `post_restart_write.json`, `recovered_ids.txt`, `rejection.json`, `script_exit.txt`, `server.log`, `summary.json`, `summary_line.txt`. This directory is deliberately *incomplete* against the acceptance artifact set — it has no `server_exit.txt`, because the interrupt landed before a product server was started. That is correct behavior, not a missing-evidence defect: the interrupted run has no server exit to report, and the acceptance consumer therefore skips this directory when resolving the latest complete specimen.
- `outcome=INCONCLUSIVE` with `acknowledged_count=0` is the honest classification. An interrupt is not a pass and is not a durability verdict.
- Leak check after the interrupted run and again at Stage 4 closeout: `hdiutil info | grep -c 'fjdisk\|flapjack-disk-exhaustion'` returned `0`. The run's `/tmp/fjdisk_61651_20260730T172514Z.dmg` image and `/Volumes/fjdisk_61651_20260730T172514Z` mount point are both absent.

### Measured answer for the four durability properties

Measured at source SHA `7aaa08f7fa300e7fa6cdac4ca0c440c2e2076a16` against both retained completed specimens.

| # | Property | Measured verdict | Direct evidence |
|---|---|---|---|
| 1 | No panic or unexplained process exit | **PASS** | `server_exit.txt` records `exit_status=0`, `stopped_by_harness=true`, `unexplained_exit=false` for both runs (Run 1 `pid=73363`, Run 2 `pid=28913`), and the recovery servers likewise (`pid=78655`, `pid=32890`). `grep -c -i panic` over `server.log` and `recovery_server.log` returns `0` in both directories. |
| 2 | Honest sanitized rejection | **PASS** | `rejection.json` records `http_status: 500` with raw body `{"message":"Internal server error","status":500}` in both runs. `rejection_headers.txt` confirms `HTTP/1.1 500 Internal Server Error` and `content-type: application/json`. No filesystem path, device name, or Tantivy internal leaks into the response body. |
| 3 | Exact acknowledged/recovered set equality | **FAIL (reproducible)** | 76 acknowledged, 80 recovered. `comm -13 <(sort -u oracle_acknowledged_ids.txt) <(sort -u recovered_ids.txt)` returns exactly `disk-020-00`, `disk-020-01`, `disk-020-02`, `disk-020-03` in **both** runs. The reverse direction `comm -23` is empty: nothing acknowledged was lost. The defect is *extra visible data*, not data loss. |
| 4 | Successful post-restart write | **FAIL (not proven)** | `post_restart_write.json` records the older non-PASS placeholder `{"http_status":0,"json":{}}` in both retained runs. See the fidelity note directly below — at specimen time this was not a measured failed write. |

Property 4 fidelity note (important, and an honest narrowing of the Stage 3 claim): at specimen time, `run_recovery_phase` in `engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh` short-circuited on `compare_recovered_to_oracle` failure and returned before `write_post_restart_document` was called. The `{"http_status":0,"json":{}}` body in both retained specimens is therefore the then-current non-PASS placeholder written by `write_missing_artifacts_for_nonpass`, not the result of an attempted-and-failed write. The decisive proof is that neither retained directory contains `post_restart_payload.json` or `post_restart_response.json`, which `write_post_restart_document` would have created unconditionally. The correct statement of property 4 for those specimens is **unproven**, and the acceptance script is right to fail it: an unproven durability property must not read as satisfied. Review hardening at current HEAD now keeps running the post-restart probe even when set equality fails and uses an explicit `attempted:false` placeholder only when no probe ran, so future evidence can distinguish "not attempted" from "attempted and failed."

The Stage 1 `FlapjackError` prediction is confirmed on status and body, and corrected on variant. Stage 1 predicted `FlapjackError::Io -> HTTP 500` with `{"message":"Internal server error","status":500}`. The measured status and body match exactly. The measured *variant* is `FlapjackError::Tantivy`, not `FlapjackError::Io`: ENOSPC surfaces first inside Tantivy's writer, and `engine/src/error.rs:121-123` converts `tantivy::TantivyError` into `FlapjackError::Tantivy`. `engine/src/error.rs:207` maps that variant to `StatusCode::INTERNAL_SERVER_ERROR` and `engine/src/error.rs:799-804` maps it into the same sanitized `Internal server error` message, so the externally observable contract is identical to the prediction. Server-side log line (Run 1 `server.log:91`, Run 2 `server.log:91`):

```text
ERROR flapjack::index::write_queue::finalization: [WQ disk_exhaustion_durability] commit error: Failed to open file for write: 'IoError { io_error: Custom { kind: StorageFull, error: PathError { path: ".../flapjack-data/disk_exhaustion_durability/.tmphos6yf", err: Os { code: 28, kind: StorageFull, message: "No space left on device" } } }, filepath: "eeb1d2bd8f9e4c62bd1e0345dc362169.store" }'
```

Recovery-side log line proving the extras are an oplog replay of the rejected batch (Run 1 `recovery_server.log:28-29`, Run 2 `recovery_server.log:28-29`):

```text
INFO flapjack::index::manager::recovery: [RECOVERY disk_exhaustion_durability] replaying 4 ops from seq 77 (committed_seq=76)
INFO flapjack::index::manager::recovery: [RECOVERY disk_exhaustion_durability] recovered 4 document ops, new committed_seq=80
```

That is the whole defect in two lines: the batch was durably admitted to the oplog at seq 77-80, the Tantivy commit then failed with ENOSPC, the client was correctly told `500`, and recovery replayed seq 77-80 anyway and advanced `committed_seq` to `80`. The client was told the write failed; the index says it succeeded.

### Stage 3 validation transcript relevant to this receipt

- `bash -n engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh` — PASS.
- `bash -n engine/loadtest/tests/disk_exhaustion_acceptance.sh` — PASS.
- `DISK_EXHAUSTION_HARNESS_SELFTEST=1 bash engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh` — PASS (includes the fail-capable `run_exact_pid_interrupt_delivery_selftest` added in Stage 3, which was red before `4ec1516de`).
- `DISK_EXHAUSTION_ACCEPTANCE_SELFTEST=1 bash engine/loadtest/tests/disk_exhaustion_acceptance.sh` — PASS.
- `cd engine && cargo fmt --check` — PASS.
- `DISK_EXHAUSTION_EVIDENCE_DIR=engine/loadtest/results/20260730T170751Z-disk-exhaustion bash engine/loadtest/tests/disk_exhaustion_acceptance.sh` — exit `1`, expected routed-gap red.
- No Rust product files were changed in Stage 3. Stage 3's only repository edits were `engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh` and `engine/_dev/s/manual-tests/disk_exhaustion_durability_selftest.sh`, so the Rust `cargo check` / `cargo clippy` / `cargo test` conditional lane did not apply. `git status --porcelain --untracked-files=all` was empty at the end of Stage 3.

## Stage 4 - routed gap spec and narrowing text

### Routed product-gap specification

- **Failed property.** A batch rejected to the client with HTTP `500` under ENOSPC becomes visible in the index after restart. Recovered IDs strictly exceed the acknowledged oracle by exactly the rejected batch.
- **Reproduction.** Run `bash engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh` on Darwin with a 32 MiB APFS image. Reproduced 5/5 at source SHA `7aaa08f7fa300e7fa6cdac4ca0c440c2e2076a16`.
- **Current owners of the defective behavior:**
  - `engine/src/index/write_queue/admission.rs::{stage_record,publish_record}` — owns durable admission (oplog staging then publish) *before* the HTTP acknowledgement decision. `stage_record` is defined at `engine/src/index/write_queue/admission.rs:256` and `publish_record` at `:309`; both are called from the admission path at `:243` and `:246`.
  - `engine/src/index/write_queue/finalization.rs::commit_writer_with_panic_guard` (`engine/src/index/write_queue/finalization.rs:145`) — owns Tantivy commit error handling, and is the code that logged the ENOSPC commit error above.
  - `engine/src/index/write_queue/mod.rs::commit_batch` (`engine/src/index/write_queue/mod.rs:1660`) — owns the ordering from admission/oplog through commit, finalization, and task acknowledgement, and is therefore the seam where a failed commit must be reconciled against an already-published oplog entry.
- **Explicitly not the owner.** `engine/src/error.rs::{From<std::io::Error>,status_code,api_message}` behaved exactly as the Stage 1 prediction required: HTTP `500` with the sanitized `Internal server error` body and no path/device leakage. The measured variant is `FlapjackError::Tantivy` rather than `FlapjackError::Io`, and that variant maps identically at `engine/src/error.rs:207` and `engine/src/error.rs:799-804`. `error.rs` needs no change for this gap, and changing it would not move any of the four properties.
- **Smallest unblocking change.** First add a fail-capable contract test inside the owning write-queue module that drives a durable admission followed by a Tantivy commit failure and asserts that a client-rejected operation cannot later recover as visible data. Only then change the `commit_batch` ordering so a failed commit either rolls the published oplog entry back below `committed_seq` or converts the response into an honest durable acknowledgement with a task ID and retry semantics. This needs care around Tantivy's ambiguous partial-commit state; a speculative patch across three owners without the contract test first is the wrong move and Stage 3 correctly declined it.
- **Disposition.** Routed, not closed. Do not mark the fill-disk product property done. Do not weaken `engine/loadtest/tests/disk_exhaustion_acceptance.sh` to make the retained specimens pass.
- **Closing condition.** `bash engine/loadtest/tests/disk_exhaustion_acceptance.sh` green for three sequential specimens captured at the fixed HEAD, with `summary.json.source_sha` equal to that HEAD.

### Proposed replacement text for the `PR-10` row in `engine/docs2/FEATURES.md`

**Not applied in this lane.** `engine/docs2/FEATURES.md` remains the single ledger owner for PR-10 and is untouched by this stage; Wave 4 owns the edit. The text below is a drop-in replacement for the single table row currently at `engine/docs2/FEATURES.md:427`. It replaces that row only and introduces no second ledger, no new table, and no roadmap entry.

```markdown
| PR-10 | Chaos / resilience testing | 🟡 Partially covered — 3 modes open (2026-07-30) | The originally planned `engine/scripts/chaos_test.sh` + `engine/tests/test_resilience_isolation.rs` were never created. Of the four named adversarial modes: **kill-server-mid-write is covered** by `crash_durability_test.rs`, `restart_during_writes_test.rs`, and `idempotency_restart_durability_test.rs`. **fill-disk-mid-write now has a bounded Darwin APFS probe and an automated acceptance contract** — `engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh` (bounded 32 MiB attached image, evidence-before-teardown, exact-PID interrupt handling) and `engine/loadtest/tests/disk_exhaustion_acceptance.sh` (sole automated evidence consumer) are the fill-disk evidence owner — but the mode remains **product-gap-routed, not closed**: retained specimens prove no panic and a sanitized HTTP 500 rejection, yet a rejected batch replays into the index on restart (76 acknowledged vs 80 recovered, extras `disk-020-00`..`disk-020-03`), and the post-restart write property is unproven. Closing it requires fixing the write-queue durable-admission / commit-finalization defect in `engine/src/index/write_queue/{admission.rs,finalization.rs,mod.rs}` and then three sequential final-HEAD specimens passing `disk_exhaustion_acceptance.sh`. **partition-replica-from-primary is partly covered** by HA autoheal/soak evidence (network isolation, restart, catch-up) but has no asserted replica-partition-from-primary contract. **OOM-kill-and-restart is uncovered**: `engine/loadtest/scenarios/memory-pressure.js` observes memory-pressure middleware, it does not force an OS OOM kill. Evidence receipt: [`engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_5_disk_exhaustion_receipt.md`](4_EVIDENCE/2026_07_30_jul30_12am_5_disk_exhaustion_receipt.md). |
```

Why this wording. The current row says the resilience surface "is in practice covered by PR-8's targeted integration tests" and lists all four adversarial modes as one undifferentiated post-launch item. That is now measurably wrong in two directions: kill-server-mid-write is genuinely covered and should not sit in the open pile, and fill-disk-mid-write is not merely untested — it has a probe, an acceptance contract, and a *measured, reproducible product defect*. The replacement narrows the row to the three still-uncovered or partly-covered modes, names the fill-disk evidence owner explicitly, and keeps fill-disk visibly red rather than letting "we built a probe" read as "the mode is covered."

### Self-check against the stage's no-weakening constraints

- The receipt does not claim acceptance-green closure. Every fill-disk statement in this document either reports a `FAIL`/routed verdict or reports the acceptance script exiting `1`. The only `PASS` claims are properties 1 and 2, the shell self-tests, `cargo fmt --check`, and the two HA acceptance owners — each with its command and output recorded.
- The acceptance contract is not weakened. `engine/loadtest/tests/disk_exhaustion_acceptance.sh` is unmodified in this stage; its exact-set, post-restart-write, outcome, `source_sha`, and `script_exit_code` assertions all still fail against the retained evidence, and this receipt records that failure as the expected result.
- No second ledger owner is introduced. `engine/docs2/FEATURES.md` keeps sole ownership of the PR-10 row; the block above is proposed replacement text held in an evidence receipt, explicitly marked not-applied. No `ROADMAP.md`, `PROJECT_OVERVIEW.md`, or `CHANGELOG.md` entry is created here.

## Stage 4 closeout verification transcript

Most closeout commands were run from the repository root at HEAD `ac6aae9303795e47cf46258c61e9ff0bbc8cf479`. The working tree at run time carried exactly one modification, this receipt file itself; no other repository file was dirty. The final `cargo fmt` and `cargo test -p flapjack --lib` rerun was completed at HEAD `3523dd689c535687a93b886de760c3fd37de04c5`, after the receipt-only Stage 4 evidence commit.

| Command | Result |
|---|---|
| `test -s engine/docs2/4_EVIDENCE/2026_07_30_jul30_12am_5_disk_exhaustion_receipt.md` | `RECEIPT_EXIT=0` |
| `bash engine/loadtest/tests/disk_exhaustion_acceptance.sh` | `ACCEPTANCE_EXIT=1` — expected routed-gap red |
| `bash engine/loadtest/tests/ha_soak_acceptance.sh` | `HA_SOAK_EXIT=0` — `PASS: PL-7 soak-evidence durability gate` |
| `bash engine/loadtest/tests/ha_soak_loadtest_durability_acceptance.sh` | `HA_DURABILITY_EXIT=0` — `PASS: loadtest durability owner acceptance` |
| `cd engine && cargo fmt --check; echo "FMT_EXIT=$?"` | `FMT_EXIT=0` |
| `cd engine && timeout 900 cargo test -p flapjack --lib --no-fail-fast > /tmp/jul30_12am_5_lib_exit.txt 2>&1; echo "LIB_EXIT=$?"; tail -5 /tmp/jul30_12am_5_lib_exit.txt` | `LIB_EXIT=0`; `test result: ok. 2131 passed; 0 failed; 8 ignored; 0 measured; 0 filtered out; finished in 150.91s` |
| `hdiutil info \| grep -c 'fjdisk\|flapjack-disk-exhaustion'` | `0` matching attachments |

Final validation-cache discipline: both final commands were checked before execution with `matt.validation_cache` imported from `/Users/stuart/repos/gridl/mike_dev/matt_root/matt/validation_cache.py`; both returned no reusable hit at clean-tree HEAD `3523dd689c535687a93b886de760c3fd37de04c5`. After the receipt transcript correction, both commands were checked again with the dirty receipt tree and again returned no reusable hit, then re-run. The same helper recorded the post-correction `FMT_EXIT=0` and `LIB_EXIT=0` under `matt_dir=/Users/stuart/.matt/projects/flapjack_dev-6d4946de/jul30_12am_5_disk_exhaustion_durability.md-a9f1a2f4`.

Exact `disk_exhaustion_acceptance.sh` failure output, recorded as expected gap evidence:

```text
FAIL: sorted unique recovered_ids.txt must exactly match oracle_acknowledged_ids.txt
FAIL: post_restart_write.json must record a 2xx write after restart
FAIL: summary.json outcome must be PASS
FAIL: summary.json source_sha must match current HEAD
FAIL: summary.json script_exit_code must be 0 for PASS evidence
ACCEPTANCE_EXIT=1
```

Two notes on that transcript, so it is not over- or under-read:

- With no `DISK_EXHAUSTION_EVIDENCE_DIR` override, `resolve_evidence_dir` picks the newest *complete* evidence directory by mtime, which at this HEAD is `engine/loadtest/results/20260730T171233Z-disk-exhaustion` — an earlier specimen from the same source SHA, not one of the two retained runs. The identical five failures reproduce when the retained specimens are named explicitly: `DISK_EXHAUSTION_EVIDENCE_DIR=engine/loadtest/results/20260730T170347Z-disk-exhaustion bash engine/loadtest/tests/disk_exhaustion_acceptance.sh` also exits `1` with the same five lines. The routed-gap conclusion does not depend on which complete specimen the resolver happens to select.
- `FAIL: summary.json source_sha must match current HEAD` is a *second, independent* reason for red, on top of the durability failures. Every retained specimen was captured at `7aaa08f7f` or `4ec1516de`, and HEAD has since advanced to `ac6aae930`. This is the acceptance contract correctly refusing to accept stale-HEAD evidence, and it is exactly why the closing condition requires three specimens re-captured at the fixed HEAD rather than a re-run of the acceptance script alone.

Both HA acceptance owners pass, so no external owner needs to be named for them and this stage does not expand beyond receipt and handoff work.

The pre-commit `git status --porcelain --untracked-files=all` for this receipt update showed only the owned receipt file. After committing the closeout receipt update, repository status was empty. The PR-10 replacement text above was deliberately **not** applied to `engine/docs2/FEATURES.md`.

## Open questions

Closed by Stage 3 measurement:

- ~~Which concrete write-path owner first observes ENOSPC during active batch writes?~~ **Answered**: `engine/src/index/write_queue/finalization.rs::commit_writer_with_panic_guard`, during the Tantivy commit, after the oplog entry has already been published by `admission.rs::publish_record`.
- ~~Does the server return the predicted sanitized HTTP 500, a different internal error, a narrower typed rejection, a timeout, or a panic?~~ **Answered**: the predicted sanitized HTTP `500` with body `{"message":"Internal server error","status":500}`, no panic, no timeout — arriving via the `FlapjackError::Tantivy` variant rather than `FlapjackError::Io`.
- ~~How much initial free space does Tantivy need on this host before the write phase can begin reliably?~~ **Answered**: a 32 MiB APFS image with `32068` KiB available at start is sufficient. Tantivy initialized and 19 batches (76 documents, ~6 MiB of payload) were acknowledged before the 20th batch hit ENOSPC at `756`-`512` KiB remaining.

Still open, and owned by the routed gap rather than by this receipt:

- Can a published oplog entry be safely retracted after a failed Tantivy commit, or must the failure instead be converted into a durable acknowledgement plus a retry? Tantivy's partial-commit state after ENOSPC is not characterized here, and that characterization is the first real design input to the fix.
- Does the same acknowledge/replay divergence occur for ENOSPC arriving at oplog append time rather than at commit time? The retained specimens only exercised the commit-time path; the oplog-append-time path is untested.
- Is property 4 (successful post-restart write on a freed volume) actually satisfiable at HEAD? It is currently unproven rather than failed, because the harness short-circuits before attempting it. A specimen that reaches the post-restart write requires the set-equality property to pass first, so property 4 cannot be measured until the routed gap is fixed. Whether to also give the harness an independent post-restart-write probe that does not depend on set equality is an open harness-design question for the fix lane.
