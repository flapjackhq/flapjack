#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
LADDER_SCRIPT="${FLAPJACK_SCALE_LADDER_SCRIPT:-$LOADTEST_DIR/scale_ladder.sh}"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

assert_failure_evidence_basics() {
  local failure_evidence="$1"
  local context="$2"

  [[ -s "$failure_evidence" ]] || {
    fail "${context} did not preserve failure_evidence.txt"
  }
  grep -Fqx 'outcome=FAIL' "$failure_evidence" || {
    fail "${context} failure_evidence.txt is missing the outcome"
  }
  grep -Fqx 'failure_outcome=FAILED' "$failure_evidence" || {
    fail "${context} failure_evidence.txt is missing the failure outcome"
  }
  grep -Fqx 'script_exit_code=1' "$failure_evidence" || {
    fail "${context} failure_evidence.txt is missing the validation exit code"
  }
}

[[ -x "$LADDER_SCRIPT" ]] || fail "scale ladder driver missing or not executable: $LADDER_SCRIPT"
grep -Eq '^require_loadtest_commands .* stat( |$)' "$LADDER_SCRIPT" || {
  fail "scale ladder does not declare stat at its required-command boundary"
}

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/scale_ladder_argument_validation.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT

server_binary="$(command -v bash)"
data_dir="$work_dir/server_data"
results_dir="$work_dir/missing/results"
output_path="$work_dir/output.txt"

if bash "$LADDER_SCRIPT" \
  --profile standard \
  --data-dir "$data_dir" \
  --results-dir "$results_dir" \
  --server-binary /bin/true \
  >"$output_path" 2>&1; then
  fail "missing --rungs validation unexpectedly passed"
fi

grep -Fqx 'FAIL: --rungs is required' "$output_path" || {
  fail "missing --rungs validation failed for the wrong reason: $(cat "$output_path")"
}
if grep -Fq 'failure_evidence.txt: No such file or directory' "$output_path"; then
  fail "cleanup emitted a secondary missing failure_evidence.txt error"
fi

assert_failure_evidence_basics \
  "$results_dir/failure_evidence.txt" \
  "argument validation"

blocked_results_parent="$work_dir/blocked_results_parent"
blocked_results_dir="$blocked_results_parent/results"
blocked_results_output="$work_dir/blocked_results_output.txt"
: >"$blocked_results_parent"
if bash "$LADDER_SCRIPT" \
  --profile standard \
  --data-dir "$work_dir/blocked_server_data" \
  --results-dir "$blocked_results_dir" \
  --server-binary /bin/true \
  >"$blocked_results_output" 2>&1; then
  fail "unwritable failure-evidence directory unexpectedly passed"
fi

grep -Fqx 'FAIL: --rungs is required' "$blocked_results_output" || {
  fail "blocked results-directory probe failed for the wrong reason: $(cat "$blocked_results_output")"
}
grep -Fqx "ERROR: failed to create results directory for failure evidence in ${blocked_results_dir}" \
  "$blocked_results_output" || {
  fail "blocked results-directory probe did not report the evidence boundary error"
}

preflight_results_dir="$work_dir/preflight/results"
preflight_output_path="$work_dir/preflight_output.txt"
if SCALE_DISK_FREE_BYTES_OVERRIDE=1 SCALE_MEMORY_CAPACITY_BYTES_OVERRIDE=1 \
  bash "$LADDER_SCRIPT" \
    --profile standard \
    --rungs 100 \
    --data-dir "$work_dir/preflight_server_data" \
    --results-dir "$preflight_results_dir" \
    --server-binary "$server_binary" \
    >"$preflight_output_path" 2>&1; then
  fail "capacity preflight rejection unexpectedly passed"
fi

grep -Fqx 'FAIL: capacity preflight rejected rung 100 before server start' \
  "$preflight_output_path" || {
  fail "capacity preflight rejection failed for the wrong reason: $(cat "$preflight_output_path")"
}
if grep -Fq 'failure_evidence.txt: No such file or directory' "$preflight_output_path"; then
  fail "preflight cleanup emitted a secondary missing failure_evidence.txt error"
fi

assert_failure_evidence_basics \
  "$preflight_results_dir/failure_evidence.txt" \
  "capacity preflight rejection"

separate_disk_results_dir="$work_dir/separate_disk/results"
separate_disk_tmp_dir="$work_dir/separate_disk/dataset_tmp"
separate_disk_output_path="$work_dir/separate_disk_output.txt"
fake_bin_dir="$work_dir/fake_bin"
mkdir -p "$separate_disk_tmp_dir" "$fake_bin_dir"
cat >"$fake_bin_dir/df" <<'STUB'
#!/usr/bin/env bash
target="${@: -1}"
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
case "$target" in
  */dataset_tmp)
    printf 'overlay 1000 999 1 100%% /dataset\n'
    ;;
  *)
    printf 'overlay 1000000 1 999999 1%% /data\n'
    ;;
esac
STUB
chmod +x "$fake_bin_dir/df"
cat >"$fake_bin_dir/stat" <<'STUB'
#!/usr/bin/env bash
target="${@: -1}"
case "$target" in
  */dataset_tmp)
    [[ "${1:-}" == -f ]] || exit 1
    printf '222\n'
    ;;
  *)
    [[ "${1:-}" == -c ]] || exit 1
    printf '111\n'
    ;;
esac
STUB
chmod +x "$fake_bin_dir/stat"
if PATH="$fake_bin_dir:$PATH" \
  TMPDIR="$separate_disk_tmp_dir" \
  SCALE_DISK_RESERVE_BYTES=1024 \
  SCALE_MEMORY_RESERVE_BYTES=1024 \
  SCALE_SOURCE_BYTES_PER_RECORD=1024 \
  SCALE_INDEX_BYTES_PER_RECORD=1 \
  SCALE_RSS_BYTES_PER_RECORD=1 \
  SCALE_MEMORY_CAPACITY_BYTES_OVERRIDE=1000000 \
  bash "$LADDER_SCRIPT" \
    --profile standard \
    --rungs 100 \
    --data-dir "$work_dir/separate_disk/server_data" \
    --results-dir "$separate_disk_results_dir" \
    --server-binary "$server_binary" \
    >"$separate_disk_output_path" 2>&1; then
  fail "dataset-filesystem capacity preflight unexpectedly passed"
fi

grep -Fqx 'FAIL: capacity preflight rejected rung 100 before server start' \
  "$separate_disk_output_path" || {
  fail "dataset-filesystem preflight failed for the wrong reason: $(cat "$separate_disk_output_path")"
}
jq -e '
  .verdict == "NO_GO" and
  .diskFilesystemsShared == false and
  .dataDiskFreeBytes == 1023998976 and
  .datasetDiskFreeBytes == 1024 and
  .requiredDataDiskBytes == 1324 and
  .requiredDatasetDiskBytes == 103424 and
  .diskReasons == ["dataset"]
' "$separate_disk_results_dir/rung_100/capacity_preflight.json" >/dev/null || {
  fail "dataset-filesystem preflight did not preserve exact separate-disk evidence"
}
[[ ! -s "$separate_disk_results_dir/server.log" ]] || {
  fail "dataset-filesystem capacity NO_GO started the server before rejecting the run"
}

echo "PASS: scale ladder early failures write clean failure evidence"
