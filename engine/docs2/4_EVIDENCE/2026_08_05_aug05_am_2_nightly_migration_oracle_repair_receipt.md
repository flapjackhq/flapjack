# Nightly migration oracle repair pre-merge receipt

Date: 2026-08-05

## Scope

This receipt records the pre-merge state for `aug05_am_2_nightly_migration_oracle_repair`.
It does not claim any public-mirror sync, tag, publish, or scheduled nightly success.

The repair stays inside the workflow/oracle gate plus its public-mirror transport contract:

- `.github/workflows/nightly.yml`
- `.debbie.toml`
- `engine/tests/migration_import_contract_test.sh`
- `engine/docs2/4_EVIDENCE/2026_08_05_stage2_nightly_migration_import_selector_specimen.log`

`engine/tests/migration_import_contract.sh` remains the single receipt emitter, and
`.debbie.toml` only whitelists the checked-in selector specimen plus this receipt for
public-mirror consumers.
No migration production code, release packaging, dashboard job, or `release.yml` file was changed.

## Failure Window Attribution

Stage 1 enumerated the public-mirror `nightly.yml` window from 2026-07-17 through
2026-08-05. Denominator: 20 total runs, 20 attributed, 0 evidence-loss.

Distinct failing-job buckets:

| Bucket | Count |
| --- | ---: |
| Migration import selector rejected a passing receipt | 14 |
| Dashboard success-card timeout | 6 |
| Dashboard invalid-credential message mismatch | 2 |
| Rust Clippy gate | 3 |

The bucket count is 25 because five nightly runs had two failing jobs.

## Selector Repair Evidence

The repaired selector parses log lines as JSON, selects the last object matching the current
receipt shape, and validates the `migration_import` check plus
`objects.imported=${SEEDED_OBJECT_COUNT}` in the same workflow-owned shell/JQ seam.

Numbered owner evidence at repaired HEAD:

- `.github/workflows/nightly.yml:325-344` owns `select_migration_import_receipt()`;
  `:330-337` parses every log line and applies the receipt checks.
- `engine/tests/migration_import_contract.sh:462-501` owns the receipt object and
  `:3112-3115` completes and prints the final JSON projection.
- `engine/tests/migration_import_contract_test.sh:2370-2468` owns the legacy-anchor,
  missing-selector, shape-drift, permuted-key, missing-check, and wrong-count controls;
  `:2581-2592` invokes them from the existing nightly-import contract owner.

The checked-in selector evidence includes:

- Legacy leading-`mode` anchor misses the captured passing receipt.
- Selector extraction rejects a workflow copy with the selector removed.
- Receipt-shape filter rejects drifted `counts` keys.
- Permuted-key receipt derived from the single captured specimen is accepted.
- Receipts missing `migration_import` or reporting a wrong imported count are rejected.
- The synced selector specimen and this receipt are both guarded against credential-shaped content.

Offline replay evidence from Stage 2: `permuted-keys exit=0`,
`missing-check exit=1`, and `wrong-count exit=1`.

The secrets-free replay was run from `engine/` with the exact workflow selector and the one
tracked specimen as its only receipt source:

```bash
bash -c '
set -uo pipefail
SPEC=docs2/4_EVIDENCE/2026_08_05_stage2_nightly_migration_import_selector_specimen.log
awk "/^          select_migration_import_receipt\(\) \{/,/^          \}/" ../.github/workflows/nightly.yml | sed "s/^          //" > /tmp/stage2_offline_selector.sh
RECEIPT="$(jq -Rcer "fromjson? | select(.mode == \"importing\" and .status == \"pass\")" "$SPEC" | tail -n 1)"
COUNT="$(printf "%s" "$RECEIPT" | jq -er ".checks[] | select(.name == \"migration_import\") | .detail | ltrimstr(\"objects.imported=\")")"
replay() {
  printf "noise line, not JSON\n" > /tmp/stage2_offline_replay.log
  printf "%s" "$RECEIPT" | jq -c "$2" >> /tmp/stage2_offline_replay.log
  ORACLE_LOG=/tmp/stage2_offline_replay.log SEEDED_OBJECT_COUNT="$COUNT" \
    bash -c "set -euo pipefail; . /tmp/stage2_offline_selector.sh; select_migration_import_receipt" >/dev/null 2>&1
  printf "%-28s exit=%s\n" "$1" "$?"
}
replay permuted-keys "to_entries | reverse | from_entries"
replay missing-check ".checks = [.checks[] | select(.name != \"migration_import\")]"
replay wrong-count "(.checks[] | select(.name == \"migration_import\") | .detail) |= (\"objects.imported=\" + ((ltrimstr(\"objects.imported=\") | tonumber) + 1 | tostring))"
'
```

```text
permuted-keys                exit=0
missing-check                exit=1
wrong-count                  exit=1
```

## Fresh Merged-Head Repair

After merging current `origin/main`, the supervisor run at `b66d32d78` found one remaining
test-owned race in `scale two-point preserves completed manifest after deleted snapshot`:
`seed_stale_sampled_manifest_snapshot()` could lose `manifest.0.json` between writing it and
aging it with `os.utime(...)`.

The repaired fixture now retries stale snapshot planting until the path exists and `utime`
has succeeded. The deleted-snapshot assertion remains unchanged, and no migration production
code was changed.

Red proof:

```text
EXIT=1
[FAIL] scale two-point preserves completed manifest after deleted snapshot
FileNotFoundError: [Errno 2] No such file or directory: .../sampler.json.candidates/manifest.0.json
expected=170 observed=170 pass=169 fail=1 skip=0
```

Green proof at repaired HEAD:

```text
cd engine && timeout 3000 bash tests/migration_import_contract_test.sh
EXIT=0
Scenario inventory: expected=170 observed=170 pass=170 fail=0 skip=0
Results: 170/170 passed (0 skipped)
```

Additional validation:

- `bash -n engine/tests/migration_import_contract_test.sh` exited 0.
- `actionlint .github/workflows/nightly.yml` exited 0.
- `git diff --check` exited 0.

## Scheduled Nightly Transfer

The two consecutive scheduled public-mirror nightly successes have not occurred yet and are
not claimed by this receipt. They cannot contain this repair until the branch lands and the
mirror is updated.

Per `chats/icg/aug05_am_0_nightly_signal_recovery_orchestration.md`, observing two consecutive
scheduled successes transfers to the next batch's Wave 1 as post-close follow-up work.

Both required scheduled run IDs remain explicitly open for that owner:

- First consecutive `event=schedule` success run ID: open — next batch Wave 1 must record it.
- Second consecutive `event=schedule` success run ID: open — next batch Wave 1 must record it.

A `workflow_dispatch` run is non-closing extra evidence and cannot fill either open run-ID slot.
