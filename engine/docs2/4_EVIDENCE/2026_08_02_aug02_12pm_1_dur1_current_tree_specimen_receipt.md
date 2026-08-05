# DUR-1 Current-Tree Disk-Exhaustion Specimen Receipt

Date: 2026-08-02
Lane: `aug02_12pm_1_dur1_current_tree_specimen_and_freshness_scope`
Scope: local evidence receipt only. This does not close the broader durable-publish-before-acknowledgement class.

## Freshness Scope Repair

The merge-base version of `engine/loadtest/tests/disk_exhaustion_acceptance.sh` used this broad exclude-style pathspec:

```bash
git diff --quiet "$summary_sha" HEAD -- ':(exclude)engine/loadtest/results' ':(exclude)engine/docs2' ':(exclude)chats'
```

Current owner: `engine/loadtest/tests/disk_exhaustion_acceptance.sh`.

Current `FRESHNESS_RELEVANT_PATHS` include list and rationale:

- `engine/Cargo.toml`: workspace graph, features, crate membership, and binary build inputs can change the release binary the probe exercises.
- `engine/Cargo.lock`: resolved dependency versions can change storage, HTTP, async, or build behavior even when source paths are unchanged.
- `engine/build.rs`: build-time generated metadata can change what the binary embeds or exposes.
- `engine/flapjack-http/build.rs`: HTTP crate build-time generation can change server metadata and generated API surface.
- `engine/rust-toolchain.toml`: pins the compiler channel used to build the binary under test.
- `engine/src`: owns core index, search, storage, tokenizer, vector, analytics, and recovery behavior that determine acknowledged IDs, recovered IDs, rejection behavior, and persisted data.
- `engine/flapjack-server/Cargo.toml`: owns the server binary manifest, features, and linked dependencies.
- `engine/flapjack-server/src`: owns the executable entrypoint and runtime wiring for the probe.
- `engine/flapjack-http/Cargo.toml`: owns HTTP crate features and dependencies used by the Algolia-compatible API.
- `engine/flapjack-http/src`: owns the server routing, handlers, middleware, and batch/query paths the probe drives.
- `engine/flapjack-replication/Cargo.toml`: linked workspace crate manifest; dependency or feature changes can alter the binary under test.
- `engine/flapjack-replication/src`: linked workspace crate source reached through `flapjack-http`/`flapjack`.
- `engine/flapjack-ssl/Cargo.toml`: linked workspace crate manifest; dependency or feature changes can alter the binary under test.
- `engine/flapjack-ssl/src`: linked workspace crate source reached through `flapjack-http`/`flapjack`.
- `engine/loadtest/lib/loadtest_shell_helpers.sh`: sourced loadtest helper controlling result directory creation and shared shell behavior.
- `engine/loadtest/lib/loadtest_soak_helpers.sh`: sourced loadtest helper controlling server startup, health waiting, and shared soak behavior.
- `engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh`: owns the disk-exhaustion harness, write pressure, recovery, and summary artifact creation.
- `engine/_dev/s/manual-tests/disk_exhaustion_volume_lifecycle.sh`: owns bounded APFS volume setup and lifecycle.
- `engine/loadtest/tests/disk_exhaustion_acceptance.sh`: owns acceptance and freshness semantics, so a change there invalidates prior acceptance evidence.

`engine/loadtest/tests/disk_exhaustion_acceptance_selftest.sh` owns only guard tests. A change to that selftest does not stale a specimen unless freshness semantics are deliberately routed through it.

## Mutation Proof

Stage 1 mutation proof is owned by `engine/loadtest/tests/disk_exhaustion_acceptance_selftest.sh` and routes through the acceptance owner instead of implementing a second freshness checker.

Latest pre-receipt rerun evidence from Stage 2 clean review:

```bash
bash engine/loadtest/tests/disk_exhaustion_acceptance_selftest.sh
exit=0
selftest_mutations_run=45
PASS: disk exhaustion acceptance self-test
```

Expected mutation verdicts:

- `ledger-prose`: passes because `ROADMAP.md` documentation-only changes are outside the durability outcome.
- `security-prose`: passes because `docs/security/DECISIONS.md` documentation-only changes are outside the durability outcome.
- `product-path`: fails with `summary.json source_sha must have no durability-relevant changes between recorded SHA and current HEAD: engine/src/index/mod.rs`.
- `harness-path`: fails with `summary.json source_sha must have no durability-relevant changes between recorded SHA and current HEAD: engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh`.
- `narrowed-entry`: fails when a narrowed include entry drops `engine/src/query/mod.rs` coverage.
- `manifest-removal`: fails when omitting `engine/flapjack-server/Cargo.toml` drops the manifest sentinel.
- `overbroad-entry`: fails when an include entry resolves under excluded `engine/loadtest/results`.

## Specimen Provenance

- Measurement SHA: `e93818a04f3c5dd5f2ce96ac2f6e13180bee34df`.
- Evidence commit: `9c83870945fede0091d05ca3454dd4542551068c`.
- Stage 2 clean review accepted all three explicit directories and discovery mode at current HEAD.
- The earlier `20260802T174630Z`, `20260802T174952Z`, and `20260802T175218Z`
  specimens are superseded. Their source SHA, `3978141d86d5a4b0266ca8581280c32dd7f75f09`,
  is not an ancestor of the retaken tree after the Stage 2 rebase and is not the
  basis of this receipt's current-tree claim.

Attempt and exit ledger:

- Superseded pre-rebase attempt at
  `3978141d86d5a4b0266ca8581280c32dd7f75f09`: all three probe runs exited `0`;
  all three explicit acceptance runs and discovery acceptance exited `0`. The
  later `513d84532` durability-relevant merge invalidated this attempt, which is
  why its specimens are retained only as superseded evidence.
- Accepted retake at `e93818a04f3c5dd5f2ce96ac2f6e13180bee34df`: all
  three probe runs exited `0`; all three explicit acceptance runs exited `0`;
  discovery acceptance exited `0` (`CLOSE_DUR1_EXIT=0`). No staleness-only retry
  was needed within the retake.

## Specimen Facts

### `engine/loadtest/results/20260803T003241Z-disk-exhaustion`

Summary fields:

```text
outcome=PASS acknowledged_count=28 recovered_count=28 rejection_status=500 script_exit_code=0 interrupted_exit_code=0 source_sha=e93818a04f3c5dd5f2ce96ac2f6e13180bee34df
```

Locality:

```text
run_index=1
measurement_head=e93818a04f3c5dd5f2ce96ac2f6e13180bee34df
captured_at_utc=2026-08-03T00:32:41Z
hostname=stuarts-MBP-3.lan
uname=Darwin stuarts-MBP-3.lan 25.3.0 Darwin Kernel Version 25.3.0: Wed Jan 28 20:54:38 PST 2026; root:xnu-12377.91.3~2/RELEASE_ARM64_T6050 arm64
temp_root=/var/folders/v6/b8qh29l57ql_p7hdw2qhpqkw0000gn/T/
uptime=20:32  up 4 days, 23:41, 8 users, load averages: 9.56 18.08 19.78
```

Bounded volume identity:

```text
image_path=/var/folders/v6/b8qh29l57ql_p7hdw2qhpqkw0000gn/T/fjdisk_99566_20260803T003241Z.mfKSVw/fjdisk_99566_20260803T003241Z.dmg
volume_label=fjdisk_99566_20260803T003241Z
attached_device=/dev/disk5s1
attachment_owner_device=/dev/disk4
mount_point=/Volumes/fjdisk_99566_20260803T003241Z
flapjack_data_dir=/Volumes/fjdisk_99566_20260803T003241Z/flapjack-data
df_device=/dev/disk5s1
```

### `engine/loadtest/results/20260803T003328Z-disk-exhaustion`

Summary fields:

```text
outcome=PASS acknowledged_count=28 recovered_count=28 rejection_status=500 script_exit_code=0 interrupted_exit_code=0 source_sha=e93818a04f3c5dd5f2ce96ac2f6e13180bee34df
```

Locality:

```text
run_index=2
measurement_head=e93818a04f3c5dd5f2ce96ac2f6e13180bee34df
captured_at_utc=2026-08-03T00:33:28Z
hostname=stuarts-MBP-3.lan
uname=Darwin stuarts-MBP-3.lan 25.3.0 Darwin Kernel Version 25.3.0: Wed Jan 28 20:54:38 PST 2026; root:xnu-12377.91.3~2/RELEASE_ARM64_T6050 arm64
temp_root=/var/folders/v6/b8qh29l57ql_p7hdw2qhpqkw0000gn/T/
uptime=20:33  up 4 days, 23:42, 8 users, load averages: 9.41 16.85 19.24
```

Bounded volume identity:

```text
image_path=/var/folders/v6/b8qh29l57ql_p7hdw2qhpqkw0000gn/T/fjdisk_15116_20260803T003328Z.kMF8KA/fjdisk_15116_20260803T003328Z.dmg
volume_label=fjdisk_15116_20260803T003328Z
attached_device=/dev/disk5s1
attachment_owner_device=/dev/disk4
mount_point=/Volumes/fjdisk_15116_20260803T003328Z
flapjack_data_dir=/Volumes/fjdisk_15116_20260803T003328Z/flapjack-data
df_device=/dev/disk5s1
```

### `engine/loadtest/results/20260803T003409Z-disk-exhaustion`

Summary fields:

```text
outcome=PASS acknowledged_count=28 recovered_count=28 rejection_status=500 script_exit_code=0 interrupted_exit_code=0 source_sha=e93818a04f3c5dd5f2ce96ac2f6e13180bee34df
```

Locality:

```text
run_index=3
measurement_head=e93818a04f3c5dd5f2ce96ac2f6e13180bee34df
captured_at_utc=2026-08-03T00:34:09Z
hostname=stuarts-MBP-3.lan
uname=Darwin stuarts-MBP-3.lan 25.3.0 Darwin Kernel Version 25.3.0: Wed Jan 28 20:54:38 PST 2026; root:xnu-12377.91.3~2/RELEASE_ARM64_T6050 arm64
temp_root=/var/folders/v6/b8qh29l57ql_p7hdw2qhpqkw0000gn/T/
uptime=20:34  up 4 days, 23:42, 8 users, load averages: 8.99 15.70 18.69
```

Bounded volume identity:

```text
image_path=/var/folders/v6/b8qh29l57ql_p7hdw2qhpqkw0000gn/T/fjdisk_29746_20260803T003409Z.dZfgqx/fjdisk_29746_20260803T003409Z.dmg
volume_label=fjdisk_29746_20260803T003409Z
attached_device=/dev/disk5s1
attachment_owner_device=/dev/disk4
mount_point=/Volumes/fjdisk_29746_20260803T003409Z
flapjack_data_dir=/Volumes/fjdisk_29746_20260803T003409Z/flapjack-data
df_device=/dev/disk5s1
```

All three specimens used private APFS `/dev/disk5s1` volumes mounted under `/Volumes/fjdisk_*` with whole-disk owner `/dev/disk4`.

## Live Red Condition

The falsifiable red condition is: after a specimen records
`source_sha=e93818a04f3c5dd5f2ce96ac2f6e13180bee34df`, any later change to
`engine/src/index/mod.rs` must stale that specimen.

Disposable detached-worktree proof:

```bash
git worktree add --detach "$wt" HEAD
printf '\n// dur1 scratch freshness mutation\n' >> "$wt/engine/src/index/mod.rs"
git -C "$wt" add engine/src/index/mod.rs
git -C "$wt" commit -qm "scratch freshness mutation"
(cd "$wt" && bash engine/loadtest/tests/disk_exhaustion_acceptance.sh)
RED_CONDITION_EXIT=1
FAIL: summary.json source_sha must have no durability-relevant changes between recorded SHA and current HEAD: engine/src/index/mod.rs
```

## Acceptance Boundary

This receipt proves only the bounded fill-disk rejected-batch replay instance represented by the three local specimens above. The broader durable-publish-before-acknowledgement class remains open until the append-I/O regression named in `ROADMAP.md` exists and passes.
