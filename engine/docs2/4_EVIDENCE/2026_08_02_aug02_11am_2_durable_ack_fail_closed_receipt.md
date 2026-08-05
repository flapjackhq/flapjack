# 2026-08-02 aug02_11am_2 Durable Ack Fail-Closed Receipt

## Purpose

Record the current-HEAD evidence for the `DUR-1` oplog append I/O failure class
closed by the `aug02_11am_2_durable_ack_fail_closed` lane, and separate that
closed class from the disk_exhaustion specimen owned by the `jul31_2pm_0` lane.

## Scope

This lane proves the direct oplog append I/O failure before acknowledgement:
a task-tagged row can be flushed and synced during `OpLog::append_operations_with_task_id`
before `current_seq` advances. The named contract requires one honest outcome:

- Arm A: the client sees failure and no task row remains replayable after restart.
- Arm B: the client sees durable acknowledgement and the acknowledged documents
  are present after restart.

The forbidden outcome is client-visible failure plus replayable state on disk.
This lane observed Arm A.

Out of scope here:

- Re-running the disk_exhaustion probe or acceptance script.
- Editing `ROADMAP.md`, `PROJECT_OVERVIEW.md`, `engine/docs2/FEATURES.md`, or
  `CHANGELOG.md`.
- Claiming the whole `DUR-1` row is closed.

## Implementation Source

Final in-scope product-code review HEAD:
`b4e4f6e5ecd451956d600bcd648338a17cd18ed7`. The direct DUR-1 implementation
was first validated at `f6757341edfd87a8ff622a35030c80907cfee630`.
Subsequent posthoc work changed in-scope source: it added legacy temporary-file
filtering, raised a test-only merge-convergence ceiling, and then removed a
never-emitted legacy name plus narrowed/refactored fault-injection test support.
Those later deltas were validated at their own review heads as recorded below;
they were not receipt-only commits.

The injection point is
`engine/src/index/oplog.rs::OpLog::append_operations_with_task_id`, immediately
after a task-tagged line is written and before `current_seq.store(...)`. Under
`cfg(any(test, feature = "fault-injection"))`, the test-only
`FinalizationFaultPoint::DuringOplogAppendAfterPartialDurableWrite` flushes and
syncs that partial row before returning an injected I/O error. That is the right
surface because it models the EIO/ENOSPC shape where durable bytes exist that the
in-memory sequence counter has not accepted.

The compensation owner remains the existing write-queue path:
`compensate_failed_commit_batch` delegates to `compensate_uncommitted_tasks`,
which calls `OpLog::retract_tasks_from` before removing admission records. The
receipt does not add a second rollback owner.

## Contract Outcome

`oplog_append_io_failure_before_acknowledgement_is_fail_closed` is discoverable
and currently green. Its guard keeps `fault.was_triggered()` mandatory, requires
an exclusive client outcome, and delegates restart assertions to
`assert_partial_append_restart_outcome`.

Observed disposition: Arm A. The client-visible tasks fail, restart exposes only
the baseline document, `oplog.read_since(pre_batch_oplog_seq)` returns no
replayable task rows, and admission records are drained.

Fail capability was proved during Stage 1 before trusting the green result:
temporary inversion made the oplog call a non-matching fault point, so the append
could not fail and the test failed at the guard:

```text
test index::write_queue::tests::oplog_append_io_failure_before_acknowledgement_is_fail_closed ... FAILED
the guard must prove the mid-append fault fired before either contractual arm is accepted
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 2206 filtered out
INVERTED_EXIT=101
```

Restored injection then passed:

```text
RED_EXIT=0
test index::write_queue::tests::oplog_append_io_failure_before_acknowledgement_is_fail_closed ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2206 filtered out; finished in 0.35s
```

## Adjacent Flake Disposition

The earlier `compensation_failure_is_fail_closed` load-sensitive 25 ms race is
not left as an unfiled note. The root cause was the hardcoded
`Duration::from_millis(25)` success deadline passed to
`wait_for_write_durable_with_timeout_for_test` at
`engine/src/index/write_queue_tests.rs:986` — a wall-clock race under sweep load,
not a product defect (measured denominator at the Stage 2 HEAD: 1 red / 3 runs of
the exact module command; focused-alone 5/5 green at 1.5–2.1 s). The in-lane
repair changed `wait_for_persistent_compensation_durable_ack`
(`write_queue_tests.rs:979`) to poll up to 10 seconds while retaining each
contractual 25 ms waiter attempt. Stage 2 recorded the loaded module gate passing
three consecutive times with `88 passed; 0 failed; 3 ignored`.

The 10 s bound is not itself a wall-clock guess. The success path returns on the
first 25 ms poll that observes the durable-ack transition and never consults the
outer deadline; 10 s is an upper bound on a sweep-scheduling stall before the test
declares failure, not a tuned success latency. The fail-closed sibling
`assert_compensation_failure_is_fail_closed` still pins its contractual
`Err(WriteAckTimeout)` at 25 ms (`write_queue_tests.rs:892`), so the bounded-error
arm is unchanged.

Fail-capability (non-vacuity) of the repaired contract was proved live at
current HEAD by a temporary in-test sabotage, then reverted to leave the tree
clean:

- Baseline, unmodified: `test result: ok. 1 passed; 0 failed`, `BASELINE_EXIT=0`,
  1.40 s.
- Sabotage — arm the compensation fault permanently
  (`fail_compensation_attempts_for_test(tenant_id, 2)` → `100_000`) so the durable
  scenario is perturbed: the contract goes RED at `write_queue_tests.rs:951`,
  `SABOTAGE_EXIT=101`, `assertion left == right failed: the worker and bounded
  waiter must each reach the compensation seam, left: 99998, right: 0`. This also
  proves the give-up→durable-ack conversion fires after exactly two compensation
  attempts, which is what the `remaining == 0` assertion pins.
- Bound probe — shrink the outer deadline to 0 ms with the fault back at 2: the
  contract stays GREEN (`DEADLINE_SABOTAGE_EXIT=0`), confirming the durable ack is
  observed inside the first 25 ms poll and the 10 s bound is only a stall ceiling.

The durable-ack waiter itself retains a live red branch: any result other than a
timed-out `Err(WriteAckTimeout)` within the bound, or the bound elapsing, hits the
`result => panic!` arm at `write_queue_tests.rs:991`.

## Validation Evidence

The direct named contract, crash target, and initial full-library evidence below
were recorded at `f6757341edfd87a8ff622a35030c80907cfee630`. After the later
in-scope test and compatibility corrections, the full library passed at
`80c9cb978`, and the final product review at `b4e4f6e5e` passed the entire
`index::write_queue` module, the affected utility/metadata and publication
digest groups, clippy, and formatting. This stratified provenance avoids
claiming that all evidence came from one unchanged product HEAD.

```text
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2211 filtered out; finished in 5.07s
GREEN_EXIT=0
```

```text
test result: ok. 88 passed; 0 failed; 3 ignored; 0 measured; 2121 filtered out; finished in 291.46s
MODULE_EXIT=0
```

```text
test result: ok. 2204 passed; 0 failed; 8 ignored; 0 measured; 0 filtered out; finished in 199.85s
LIB_EXIT=0
```

Posthoc full-library gate at `80c9cb978`:

```text
test result: ok. 2205 passed; 0 failed; 8 ignored
```

Final in-scope review gates at `b4e4f6e5e`:

```text
index::write_queue: 88 passed; 0 failed; 3 ignored
index::utils + index::index_metadata: 13 passed; 0 failed
index::manager::publication::tests::canonical_tree_digest: 4 passed; 0 failed
```

```text
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 18.82s
CRASH_EXIT=0
```

```text
cargo clippy -p flapjack --all-targets finished dev profile in 1m 04s
CLIPPY_DONE
```

```text
FMT_EXIT=0
```

## ROADMAP CORRECTION REQUIRED

Proposed replacement for the `DUR-1` row:

| DUR-1 | Durable admission under disk exhaustion | **NARROWED, not closed.** The direct oplog append I/O failure before acknowledgement is receipt-proved at `f6757341edfd87a8ff622a35030c80907cfee630` by `index::write_queue::tests::oplog_append_io_failure_before_acknowledgement_is_fail_closed`: a flushed task-tagged partial append before `current_seq` advances takes Arm A, meaning the client sees failure, restart retains only the baseline document, no post-floor task rows replay, and admission records drain. The previous `compensation_failure_is_fail_closed` 25 ms load race was repaired in-lane with a bounded poll and the `index::write_queue` module gate passed three consecutive loaded sweeps. The fresh current-HEAD disk_exhaustion specimen remains a separate `jul31_2pm_0` deliverable. This lane's product change invalidates any disk_exhaustion specimen taken before this merge, so that specimen must be re-taken after this merge before the broader row can close. | Owners: direct oplog append class in `engine/src/index/oplog.rs`, `engine/src/index/write_queue/finalization.rs`, `engine/src/index/write_queue/compensation.rs`, `engine/src/index/oplog_retraction.rs`, and `engine/src/index/write_queue_tests.rs`; disk_exhaustion specimen owner remains `engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh` and `engine/loadtest/tests/disk_exhaustion_acceptance.sh` under the `jul31_2pm_0` lane. |

Proposed new row for the atomic-write hardening surfaced by this lane's Stage 2
consolidation (recorded here so the owner survives this lane's merge; not fixed
by this verification stage — see the scope fence below):

| DUR-3 | Atomic-write durability consolidation across non-oplog tenant writers | **OPEN — filed 2026-08-02 by the `aug02_11am_2_durable_ack_fail_closed` lane.** Stage 2 gave `engine/src/index/utils.rs::atomic_write` / `atomic_write_with_before_rename` canonical ownership of collision-resistant `.tmp`-prefixed temp naming, file `sync_all`, atomic rename, and parent-directory fsync, and migrated `oplog.rs::write_committed_seq`, `index_metadata.rs::save`, and `backpressure.rs::write_decision_artifact` onto it. Two classes of writer were left unconverted and are recorded here with exact `file:line`: **(a) name-collision hazard** — `engine/src/dictionaries/persistence.rs:38::atomic_write` is a second, weaker function sharing the canonical name with no `sync_all`, no parent-dir fsync, and a non-unique `.{name}.tmp` temp whose `.tmp` *suffix* (not the `.tmp` *prefix* that `utils.rs::is_temporary_entry` and `publication/digest.rs` filter on) means a leaked temp is not excluded from a tenant tree walk; it predates this sprint (`493edd449`) and the shared name is the trap, since a later reader will assume the durable one. **(b) Five more hand-rolled temp-plus-rename writers** the shared owner could absorb: `engine/src/analytics/writer.rs:897` and `engine/src/analytics/writer.rs:1343` (the second closes the parquet writer and renames with no parent-dir fsync), `engine/src/analytics/manifest.rs:87` (`fs::write` + `fs::rename`, no fsync), and `engine/src/experiments/store.rs:156` (`persist_id_map`) and `engine/src/experiments/store.rs:193` (`atomic_write`), both `std::fs::write` + `std::fs::rename` with no fsync. None sits on the DUR-1 acknowledgement path, so they are durability-hardening tech debt, not a correctness regression. | Owner: `engine/src/index/utils.rs` (canonical `atomic_write` family) plus the six call sites above. Falsifiable exit: each converted writer routes through `utils.rs::atomic_write*`, the duplicate `persistence.rs::atomic_write` is either renamed or removed so the canonical name has one owner, and a test proves a leaked temp from each converted writer is excluded by `copy_dir_recursive` / `is_temporary_entry`. |
