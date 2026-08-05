# Chaos residual modes — acceptance receipt

Batch: `jul31_5pm_1_chaos_oom_kill_and_replica_partition` · Stage 4 · dated 2026-08-01

## Purpose

Record the standing acceptance contract that now holds the two residual `PR-10`
chaos modes — kernel OOM-kill durability and replica partition healing — so
coverage no longer depends on remembering two ad hoc scenario commands. This
receipt is anchored to real owner paths, not free prose.

## Owners

| Role | Path |
| --- | --- |
| OOM-kill scenario owner | `engine/examples/ha-cluster/test_oom_kill_durability.sh` |
| Partition-healing scenario owner | `engine/examples/ha-cluster/test_ha_partition.sh` |
| Standing acceptance contract | `engine/loadtest/tests/chaos_residual_modes_acceptance.sh` |
| Fail-capability self-test | `engine/loadtest/tests/chaos_residual_modes_acceptance_selftest.sh` |

The acceptance wrapper runs the two scenario owners exactly as they are and then
accepts or rejects their scenario-owned summary artifacts and oracle files under
`engine/loadtest/results/chaos-residual/`. It re-implements none of their
orchestration, cleanup, or HTTP assertions.

## Stage 1 gap baselines (gap evidence, not fail-capable red tests)

- `engine/loadtest/results/chaos-residual/oom_gap_baseline.log`
- `engine/loadtest/results/chaos-residual/partition_gap_baseline.log`

These retained Stage 1 transcripts prove the two modes were uncovered at their
audit HEAD. They execute no assertion and cannot fail for a defect; the
fail-capable red path is the self-test below.

## Acceptance contract

### OOM-kill durability (newly created `chaos_oom_durability_*` run at HEAD)

Required artifacts: `summary.json`, `summary_line.txt`, `script_exit.txt`,
`oom_killed_value.txt`, `acknowledged_object_ids.txt`,
`recovered_acknowledged_ids.txt`, `explicit_rejections.jsonl`,
`recovered_explicit_rejected_ids.txt`.

Assertions:
- `outcome=PASS`, `oom_killed`/`OOMKilled=true`, `oom_killed_value.txt=true`.
- `acknowledged_count` equals unique `acknowledged_object_ids.txt`, equals
  `recovered_acknowledged_count` equals unique `recovered_acknowledged_ids.txt`
  (every acknowledged write is recovered after the OS kill and restart).
- `explicit_rejected_attempted_count` is non-zero and equals
  `explicit_rejections.jsonl` line count.
- `recovered_explicit_rejected_count` is zero and equals unique
  `recovered_explicit_rejected_ids.txt` (no rejected batch replays into the
  index on restart).
- `script_exit.txt` agrees with `summary.json`; `summary_line.txt` reproduces
  the canonical `summary.json` record; `source_sha` equals current HEAD.

### Replica partition healing (newly created `ha_partition_*` run at HEAD)

Required artifacts: `summary.json`, `summary_line.txt`, `source_sha.txt`,
`acknowledged_union_ids.txt`, `acknowledged_union_expected.tsv`,
`no_ack_ids.txt`, and the exact post-heal raw query responses plus projections
`post_heal_acknowledged_union_node-{a,b,c}_query.json{,.tsv}`.

Assertions:
- `outcome=PASS`, `assertions_failed=0`, `assertions_passed>0`.
- `acknowledged_union_ids.txt` non-empty and exactly equal to the `objectID`
  column of `acknowledged_union_expected.tsv`; `no_ack_ids.txt` empty for the
  converged pass.
- `summary_line.txt` reproduces the canonical `RESULT` record and equals the
  `summary` field of `summary.json`.
- Each node's retained raw response and TSV projection agree, and its post-heal
  exact-set query converges to the acknowledged union with `nbHits` equal to
  the retained row count.
- `summary.source_sha` and `source_sha.txt` both equal current HEAD.

### Fail-closed on stale or incomplete evidence

The wrapper snapshots matching `chaos_oom_durability_*` / `ha_partition_*`
directories before each scenario owner runs, then accepts only the one directory
created by that invocation (prefix + retained `source_sha`, never latest-wins
alone). It exits non-zero if either scenario-owner process exits nonzero, the
fresh run's `source_sha` differs from `git -C <repo> rev-parse HEAD`, or its
required artifact set is incomplete. It never falls through to reuse an older
green specimen, and rejected fresh specimens are still named in the summary
line for durable triage. Missing host tooling or an unreachable Docker daemon
is a distinct readiness exit (code 3) with durable stderr output; it never exits
`0` and never masquerades as a product regression.

## Negative controls (fail-capability owners)

The self-test reuses each scenario's own negative control — no second sabotage
mechanism lives in the wrapper — one at a time, and asserts the wrapper goes red
naming the sabotaged scenario while leaving the healthy scenario clean:

- `FLAPJACK_OOM_NEGATIVE_EMPTY_RESTART=1` — OOM recovery restarts on an empty
  volume, so acknowledged writes are unrecoverable → wrapper flags `OOM:` only.
- `FLAPJACK_PARTITION_SKIP_HEAL=1` — the isolated replica is never reconnected,
  so the acknowledged union never converges → wrapper flags `PARTITION:` only.

## Durable evidence directory shape

All runs write under `engine/loadtest/results/chaos-residual/`:
- `chaos_oom_durability_<UTC-timestamp>_<slug>/` per OOM run.
- `ha_partition_<run-id>/` per partition run.
Each directory carries the scenario-owned `summary.json` + `summary_line.txt`
plus the oracle and query artifacts enumerated above. The two Stage 1 gap
baselines live directly under `chaos-residual/`.

## Measured timeout bounds

Wall times measured in real execution locality (macOS + Colima Docker,
`docker` layer cache warm after the first image build):

<!-- MEASUREMENTS -->
- Happy-path OOM scenario (`test_oom_kill_durability.sh`): 59.68 seconds;
  `OOMKilled=true`, all 14 acknowledged IDs recovered, and no explicitly
  rejected ID recovered.
- Happy-path partition scenario (`test_ha_partition.sh`): 180.83 seconds; all 22
  assertions passed and the five-ID acknowledged union converged on every node.
- Combined acceptance wrapper (`chaos_residual_modes_acceptance.sh`): 106.14
  seconds; both fresh current-HEAD specimens were accepted.
- Self-test wrapper (`chaos_residual_modes_acceptance_selftest.sh`): 184.74
  seconds; both scenario-owned negative controls drove the wrapper red for the
  expected scenario-specific reason and retained their failure artifacts.
- Chosen validation bounds and headroom: use `timeout 1800` for the combined
  acceptance wrapper and `timeout 3600` for the self-test in this locality. A
  prior cold-build run exceeded 600 seconds, so the 1800-second bound preserves
  measured cold-build headroom; the self-test runs two full wrappers and keeps
  the measured 3600-second bound. On Docker hosts whose automatic address pools
  are exhausted, the scenario owners accept opt-in, non-overlapping subnets via
  `FLAPJACK_OOM_NETWORK_SUBNET` and `FLAPJACK_HA_NETWORK_SUBNET`; defaults still
  use Docker IPAM.

## Remaining open mode

One `PR-10` mode is still open: the originally planned
`engine/scripts/chaos_test.sh` + `engine/tests/test_resilience_isolation.rs`
scaffold was never created. The `fill-disk-mid-write` mode is separately
product-gap-routed as `ROADMAP.md` row `DUR-1` and is not closed by this stage;
it is not blurred into this lane.
