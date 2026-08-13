# Second post-merge union at final `main` — a recurrence experiment, not a re-measurement

**Source SHA:** `5487e725f0ca2f30bf1f34e8c8d4c0c78475e5d5` — **identical** to the first union's.
**Ended:** `2026-08-09T00:51:44Z` · **Exit:** `0` · **Binaries:** `86` · **6,069 passed, 0 failed.**
**Evidence:** `${MATT_HOME:-$HOME/.matt}/evidence/aug07_8pm_0_postmerge_union/{union.done,union.log,run_union.sh}`

## Why a second run at the same SHA is worth reading

It answers the one question the first run structurally could not: **is the single failure it found
deterministic or intermittent?** Two sessions independently launched the same union at the same SHA
on 2026-08-08 without either being able to see the other. That collision is a real coordination gap
(recorded as a handoff in `chats/icg/aug08_9pm_0_prod_parity_and_sdk_publication_orchestration.md`),
but its output is a controlled repetition, and it is read here as one.

| | Union 1 | Union 2 |
|---|---|---|
| Source SHA | `5487e725f` | `5487e725f` (same) |
| Ended | `2026-08-08T20:05:51Z` | `2026-08-09T00:51:44Z` |
| Exit | `101` | **`0`** |
| Binaries | 86 | 86 |
| Passed / failed | 6,068 / **1** | **6,069 / 0** |
| Failure | `non_json_failure_redacts_api_key_from_stderr` | none |

## Conditions, verified from the script rather than recalled

`run_union.sh` at `:12-14`, `:17`, `:28-29` — its own words are the reason "quarantined" is not
ambiguous here:

- **Quarantined detached worktree**, never the shared clone: `WT=/private/tmp/fj_postmerge_union_20260808`.
- **`CARGO_INCREMENTAL=0`**, set on the invocation.
- **Worktree-relative target dir**, deliberately: the script comments that an external
  `CARGO_TARGET_DIR` "makes it fail spuriously" — which is `TEST-HARNESS-1`, not a property of the
  union. Union 2 is therefore the **control arm** for that row, exactly as Union 1 was, and
  discharges nothing on it.

## What this discharges

- **`TEST-FLAKE-2` — CLOSED on the exit it named.** Its exit was reduced to one clause: *one broad
  gate run from a detached quarantined worktree at `CARGO_INCREMENTAL=0` exits `0`, with every
  failure attributed by name to a census member.* All four conditions hold, and the attribution
  clause is satisfied with nothing to attribute over a denominator of 6,069 tests — that is a real
  denominator, not a vacuous one. `bounded_aggregate_concurrency_across_simultaneous_requests` ran
  and passed in both unions.
- **`TEST-FLAKE-1` — NARROWED, not closed.** Its exit has two clauses joined by "plus": *a
  deterministic reproducer that is red before the fix and green after*, **and** *one broad gate that
  exits `0` rather than `124`*. The second is now met. The first is not: no reproducer exists,
  because `merge_owner_survives_consecutive_commits` passed in both unions and no fix was made.

## What this does NOT discharge, stated plainly

**A green that follows no repair is not proof of repair.** Nothing changed between the two runs —
same SHA, same tree, same command. The honest reading of Union 2 is *"this suite can pass"*, not
*"this suite is fixed"*. For `TEST-FLAKE-2` that is enough, because a single clean broad gate is the
exit its own author chose after deliberately narrowing the row. For anything else it is not, and in
particular it must not be read as evidence that the Union 1 failure was noise: **the opposite is
true.** A failure that appears in one run and not the next, at a byte-identical tree, is the
definition of a load-dependent race, and it is now filed as `TEST-SINK-1` with a named mechanism
rather than left as an unattributed flake.

## The Union 1 failure, re-read in light of Union 2

`non_json_failure_redacts_api_key_from_stderr` (`engine/flapjack-server/tests/ingest_cli_test.rs`)
failed Union 1 with `expected exit 3, got 5` and stderr
`failed to connect to sink: Connection refused (os error 61)`. Exit `5` is `EXIT_RETRY_EXHAUSTED`,
which `engine/flapjack-server/src/ingest.rs` returns only when **every** attempt failed *before
sending* — so the CLI never reached its own fake sink, the redaction assertions never ran, and the
test name describes something that was never measured. It is not a credential leak.

Union 2 proves the mechanism is timing-dependent, which is what `TEST-SINK-1` fixes: the fake sink
served exactly `responses.len()` connections and then dropped its listener, so any retry — and the
CLI retries up to `RETRY_ATTEMPT_LIMIT` — hit a closed port and reported the harness's own
bookkeeping as the product's failure.
