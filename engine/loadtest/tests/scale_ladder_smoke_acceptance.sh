#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$(cd "$LOADTEST_DIR/.." && pwd)"
LADDER_SCRIPT="${FLAPJACK_SCALE_LADDER_SCRIPT:-$LOADTEST_DIR/scale_ladder.sh}"
SERVER_BINARY="${FLAPJACK_SCALE_SERVER_BINARY:-$ENGINE_DIR/target/release/flapjack}"

# The campaign calibration starts at 1M. Keep this 10K plumbing fixture above fixed process/index
# overhead; the dedicated negative control below overrides both values to one byte per record.
export SCALE_INDEX_BYTES_PER_RECORD="${SCALE_INDEX_BYTES_PER_RECORD:-100000}"
export SCALE_RSS_BYTES_PER_RECORD="${SCALE_RSS_BYTES_PER_RECORD:-100000}"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

[[ -x "$LADDER_SCRIPT" ]] || fail "scale ladder driver missing or not executable: $LADDER_SCRIPT"
[[ -x "$SERVER_BINARY" ]] || fail "release server binary missing: $SERVER_BINARY"

test_root="$(mktemp -d)"
data_dir="$test_root/server_data"
results_dir="$test_root/results"
port=$((18000 + ($$ % 20000)))
base_url="http://127.0.0.1:${port}"

SCALE_DISK_RESERVE_BYTES=1048576 \
SCALE_MEMORY_RESERVE_BYTES=1048576 \
timeout 900 bash "$LADDER_SCRIPT" \
  --profile compact \
  --rungs 10000,20000 \
  --throughput-probe \
  --stop-after-rung 10000 \
  --batch-size 1000 \
  --stall-seconds 10 \
  --base-url "$base_url" \
  --server-binary "$SERVER_BINARY" \
  --data-dir "$data_dir" \
  --results-dir "$results_dir"

jq -e '
  .version == 4 and
  .profile == "compact" and
  .purpose == "throughput_probe" and
  .batchSize == 1000 and
  .rungs == [10000, 20000] and
  .lastCompletedRung == 10000
' "$results_dir/checkpoint.json" >/dev/null || fail "planned pause did not write the exact checkpoint"

if SCALE_DISK_RESERVE_BYTES=1048576 \
  SCALE_MEMORY_RESERVE_BYTES=1048576 \
  timeout 60 bash "$LADDER_SCRIPT" \
    --profile compact \
    --rungs 10000,20000 \
    --throughput-probe \
    --resume \
    --batch-size 999 \
    --stall-seconds 10 \
    --base-url "$base_url" \
    --server-binary "$SERVER_BINARY" \
    --data-dir "$data_dir" \
    --results-dir "$results_dir"; then
  fail "mismatched batch-size checkpoint negative control unexpectedly passed"
fi
jq -e '.version == 4 and .batchSize == 1000 and .lastCompletedRung == 10000' \
  "$results_dir/checkpoint.json" >/dev/null || {
  fail "mismatched batch-size resume altered the last valid checkpoint"
}

SCALE_DISK_RESERVE_BYTES=1048576 \
SCALE_MEMORY_RESERVE_BYTES=1048576 \
timeout 900 bash "$LADDER_SCRIPT" \
  --profile compact \
  --rungs 10000,20000 \
  --throughput-probe \
  --resume \
  --batch-size 1000 \
  --stall-seconds 10 \
  --base-url "$base_url" \
  --server-binary "$SERVER_BINARY" \
  --data-dir "$data_dir" \
  --results-dir "$results_dir" \
  --exercise-negative-controls

metrics_paths=()
while IFS= read -r metrics_path; do
  metrics_paths+=("$metrics_path")
done < <(ls -1 "$results_dir"/rung_*/metrics.json)

[[ "${#metrics_paths[@]}" -eq 2 ]] || fail "expected two rung metrics files"

jq -e '.removed == true and .datasetBytes > 0' \
  "$results_dir/rung_10000/dataset_cleanup.json" \
  "$results_dir/rung_20000/dataset_cleanup.json" >/dev/null || {
  fail "successful rungs did not prove generated-dataset cleanup"
}
jq -e '.remainingGeneratedDatasetDirs == 0' "$results_dir/run_receipt.json" >/dev/null || {
  fail "successful run retained generated rung datasets"
}

jq -e '
  .startingCount == 0 and
  .targetCount == 10000 and
  .finalCount == 10000 and
  .docsPerSecond > 0 and
  .batchSize == 1000 and
  .importLatency.count > 0 and
  .importLatencyWindows.first.count > 0 and
  .importLatencyWindows.middle.count > 0 and
  .importLatencyWindows.last.count > 0 and
  (.importLatencyWindows.lastToFirstP50Ratio | type == "number") and
  .indexBytes > 0 and
  .rssBytes > 0 and
  .sentinels == "PASS" and
  .capacityObservation.verdict == "PASS" and
  .capacityObservation.targetCount == 10000 and
  ([.queryTypes[] | .count] | all(. == 30)) and
  .overallSearch.count == 210
' "${metrics_paths[0]}" >/dev/null || fail "first rung metrics are not exact and non-zero"

jq -e '
  .startingCount == 10000 and
  .targetCount == 20000 and
  .finalCount == 20000 and
  .docsPerSecond > 0 and
  .indexBytes > 0 and
  .rssBytes > 0 and
  .sentinels == "PASS" and
  .capacityObservation.verdict == "PASS" and
  .runPurpose == "throughput_probe" and
  (.rungVerdict == "PASS" or .rungVerdict == "FAIL") and
  (.latency.verdict == "PASS" or .latency.verdict == "FAIL") and
  .negativeControls == "PASS" and
  (.queryTypes | ["text", "typo", "multi_word", "facet", "filter", "geo", "highlight"] - keys | length == 0) and
  ([.queryTypes.text, .queryTypes.typo, .queryTypes.multi_word, .queryTypes.facet, .queryTypes.filter, .queryTypes.geo, .queryTypes.highlight]
    | all(has("p50") and has("p95") and has("p99")))
' "${metrics_paths[1]}" >/dev/null || fail "final rung metrics/per-type latency block is incomplete"

grep -q $'name/prefix\t' "$results_dir/rung_20000/search_benchmark.stdout.txt" || {
  fail "final rung search report does not contain a distinct name/prefix line"
}
grep -q 'blended overall' "$results_dir/rung_20000/search_benchmark.stdout.txt" || {
  fail "final rung search report does not contain blended overall"
}

if SCALE_DISK_RESERVE_BYTES=1048576 \
  SCALE_MEMORY_RESERVE_BYTES=1048576 \
  timeout 60 bash "$LADDER_SCRIPT" \
    --profile standard \
    --rungs 10000,20000 \
    --resume \
    --base-url "http://127.0.0.1:$((port + 3))" \
    --server-binary "$SERVER_BINARY" \
    --data-dir "$data_dir" \
    --results-dir "$results_dir"; then
  fail "mismatched-profile checkpoint negative control unexpectedly passed"
fi
jq -e '.profile == "compact" and .lastCompletedRung == 20000' \
  "$results_dir/checkpoint.json" >/dev/null || {
  fail "mismatched resume altered the last valid checkpoint"
}

(
  # shellcheck source=../lib/loadtest_shell_helpers.sh
  source "$LOADTEST_DIR/lib/loadtest_shell_helpers.sh"
  export FLAPJACK_LOADTEST_BASE_URL="$base_url"
  export FLAPJACK_LOADTEST_BENCHMARK_INDEX="scale_ceiling_compact"
  load_shared_loadtest_config
  initialize_loadtest_auth_headers
  mutation_server_pid="$(
    start_loadtest_server "$SERVER_BINARY" "no-auth" "127.0.0.1:${port}" "$data_dir" \
      "$results_dir/server_count_mismatch.log"
  )"
  trap 'stop_loadtest_server "$mutation_server_pid"' EXIT
  wait_for_loadtest_health "$base_url" "$mutation_server_pid"
  encoded_index="$(loadtest_encode_path_component "scale_ceiling_compact")"
  encoded_object="$(loadtest_encode_path_component "bench-010001")"
  delete_response="$(
    loadtest_http_request DELETE "/1/indexes/${encoded_index}/${encoded_object}" "" "200"
  )"
  wait_for_loadtest_task_published "$(extract_loadtest_numeric_task_id "$delete_response")"
  mutated_count="$(index_doc_count "$base_url" "scale_ceiling_compact")"
  [[ "$mutated_count" -eq 19999 ]] || fail "count-mismatch setup expected 19999, got $mutated_count"
)
count_mismatch_output="$results_dir/count_mismatch_resume.stdout.txt"
if SCALE_DISK_RESERVE_BYTES=1048576 \
  SCALE_MEMORY_RESERVE_BYTES=1048576 \
  timeout 60 bash "$LADDER_SCRIPT" \
    --profile compact \
    --rungs 10000,20000 \
    --throughput-probe \
    --resume \
    --base-url "$base_url" \
    --server-binary "$SERVER_BINARY" \
    --data-dir "$data_dir" \
    --results-dir "$results_dir" \
    >"$count_mismatch_output" 2>&1; then
  fail "live-count mismatch resume negative control unexpectedly passed"
fi
grep -q 'resume index count mismatch: checkpoint=20000, live=19999' "$count_mismatch_output" || {
  fail "live-count mismatch resume failed for the wrong reason"
}

preflight_root="$(mktemp -d)"
preflight_data_dir="$preflight_root/server_data"
preflight_results_dir="$preflight_root/results"
preflight_base_url="http://127.0.0.1:$((port + 2))"
if SCALE_DISK_FREE_BYTES_OVERRIDE=1 \
  SCALE_DISK_RESERVE_BYTES=1048576 \
  SCALE_MEMORY_RESERVE_BYTES=1048576 \
  timeout 60 bash "$LADDER_SCRIPT" \
    --profile compact \
    --rungs 10 \
    --batch-size 10 \
    --base-url "$preflight_base_url" \
    --server-binary "$SERVER_BINARY" \
    --data-dir "$preflight_data_dir" \
    --results-dir "$preflight_results_dir"; then
  fail "insufficient-disk preflight negative control unexpectedly passed"
fi
jq -e '.verdict == "NO_GO" and (.reasons | index("disk"))' \
  "$preflight_results_dir/rung_10/capacity_preflight.json" >/dev/null || {
  fail "insufficient-disk preflight did not preserve a fail-closed receipt"
}
[[ ! -s "$preflight_results_dir/server.log" ]] || {
  fail "capacity NO_GO started the server before rejecting the run"
}

failure_root="$(mktemp -d)"
failure_data_dir="$failure_root/server_data"
failure_results_dir="$failure_root/results"
failure_base_url="http://127.0.0.1:$((port + 1))"
mkdir -p "$failure_results_dir"
jq -n '{
  outcome: "PAUSED",
  runnerTmpDir: "/tmp/stale_prior_run",
  remainingGeneratedDatasetDirs: 0,
  terminalRung: null,
  terminalMetricsPath: null
}' > "$failure_results_dir/run_receipt.json"
if SCALE_DISK_RESERVE_BYTES=1048576 \
  SCALE_MEMORY_RESERVE_BYTES=1048576 \
  timeout 180 bash "$LADDER_SCRIPT" \
  --profile compact \
  --rungs 10 \
  --batch-size 10 \
  --stall-seconds 10 \
  --min-docs-per-second 999999999 \
  --base-url "$failure_base_url" \
  --server-binary "$SERVER_BINARY" \
  --data-dir "$failure_data_dir" \
  --results-dir "$failure_results_dir"; then
  fail "impossible throughput floor should force a non-PASS outcome"
fi

[[ -s "$failure_results_dir/failure_evidence.txt" ]] || {
  fail "non-PASS run did not preserve its failure evidence receipt"
}
[[ -s "$failure_results_dir/server.log" ]] || fail "non-PASS run did not preserve its server log"
[[ -d "$failure_data_dir" ]] || fail "non-PASS run removed caller-owned server data"
jq -e '
  .outcome == "FAILED" and
  .terminalRung == 10 and
  .remainingGeneratedDatasetDirs == 1 and
  .runnerTmpDir != "/tmp/stale_prior_run"
' "$failure_results_dir/run_receipt.json" >/dev/null || {
  fail "non-PASS run left a stale prior receipt instead of recording its active failure"
}
failure_runner_state="$(awk -F= '$1 == "runner_state" { print $2 }' "$failure_results_dir/failure_evidence.txt")"
[[ -n "$failure_runner_state" && -d "$failure_runner_state" ]] || {
  fail "non-PASS run did not preserve generated runner state before teardown"
}
[[ -s "$failure_runner_state/rung_10_dataset/batch_000.json" ]] || {
  fail "non-PASS run did not preserve the current rung input before teardown"
}

latency_root="$(mktemp -d)"
latency_data_dir="$latency_root/server_data"
latency_results_dir="$latency_root/results"
latency_stub="$latency_root/search_stub.sh"
cat >"$latency_stub" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
artifact_dir="$LOADTEST_RESULTS_BASE_DIR/stub"
mkdir -p "$artifact_dir"
jq -n --arg index "$FLAPJACK_LOADTEST_BENCHMARK_INDEX" '{
  indexName: $index,
  docCount: 10,
  wallClockMs: 1,
  queryTypes: {
    text: {count: 30, p50: 51, p95: 51, p99: 51},
    typo: {count: 30, p50: 1, p95: 1, p99: 1},
    multi_word: {count: 30, p50: 1, p95: 1, p99: 1},
    facet: {count: 30, p50: 1, p95: 1, p99: 1},
    filter: {count: 30, p50: 1, p95: 1, p99: 1},
    geo: {count: 30, p50: 1, p95: 1, p99: 1},
    highlight: {count: 30, p50: 1, p95: 1, p99: 1}
  },
  overall: {count: 210, p50: 1, p95: 100, p99: 100}
}' >"$artifact_dir/search_benchmark.json"
echo "name/prefix"
echo "blended overall"
STUB
chmod +x "$latency_stub"

SCALE_DISK_RESERVE_BYTES=1048576 \
SCALE_MEMORY_RESERVE_BYTES=1048576 \
SCALE_INDEX_BYTES_PER_RECORD=100000000 \
SCALE_RSS_BYTES_PER_RECORD=100000000 \
SCALE_MEMORY_CAPACITY_BYTES_OVERRIDE=2000000000 \
FLAPJACK_SCALE_SEARCH_SCRIPT="$latency_stub" \
timeout 180 bash "$LADDER_SCRIPT" \
  --profile compact \
  --rungs 10 \
  --batch-size 10 \
  --stall-seconds 10 \
  --base-url "http://127.0.0.1:$((port + 4))" \
  --server-binary "$SERVER_BINARY" \
  --data-dir "$latency_data_dir" \
  --results-dir "$latency_results_dir"

jq -e '
  .outcome == "CEILING_REACHED" and
  .terminalRung == 10 and
  (.terminalMetricsPath | endswith("/rung_10/metrics.json"))
' "$latency_results_dir/run_receipt.json" >/dev/null || {
  fail "latency breach did not produce a terminal run receipt"
}
jq -e '
  .rungVerdict == "FAIL" and
  .latency.verdict == "FAIL" and
  .latency.reasons == ["namePrefixP95"]
' "$latency_results_dir/rung_10/metrics.json" >/dev/null || {
  fail "latency breach did not preserve the exact failed gate"
}
[[ ! -e "$latency_results_dir/checkpoint.json" ]] || {
  fail "latency-failed first rung was incorrectly saved as a green checkpoint"
}

capacity_failure_root="$(mktemp -d)"
capacity_failure_data_dir="$capacity_failure_root/server_data"
capacity_failure_results_dir="$capacity_failure_root/results"
if SCALE_DISK_RESERVE_BYTES=1048576 \
  SCALE_MEMORY_RESERVE_BYTES=1048576 \
  SCALE_INDEX_BYTES_PER_RECORD=1 \
  SCALE_RSS_BYTES_PER_RECORD=1 \
  FLAPJACK_SCALE_SEARCH_SCRIPT="$latency_stub" \
  timeout 180 bash "$LADDER_SCRIPT" \
    --profile compact \
    --rungs 10 \
    --batch-size 10 \
    --stall-seconds 10 \
    --base-url "http://127.0.0.1:$((port + 5))" \
    --server-binary "$SERVER_BINARY" \
    --data-dir "$capacity_failure_data_dir" \
    --results-dir "$capacity_failure_results_dir"; then
  fail "forced post-rung capacity-calibration failure unexpectedly passed"
fi
jq -e '
  .outcome == "CAPACITY_CALIBRATION_FAILED" and
  .terminalRung == 10
' "$capacity_failure_results_dir/run_receipt.json" >/dev/null || {
  fail "forced capacity failure did not preserve its terminal run receipt"
}
jq -e '
  .rungVerdict == "FAIL" and
  .capacityObservation.verdict == "FAIL" and
  ((.capacityObservation.reasons | index("indexBytesPerRecord")) != null) and
  ((.capacityObservation.reasons | index("rssBytesPerRecord")) != null)
' "$capacity_failure_results_dir/rung_10/metrics.json" >/dev/null || {
  fail "forced capacity failure did not preserve the exact failed observations"
}
[[ ! -e "$capacity_failure_results_dir/checkpoint.json" ]] || {
  fail "capacity-failed rung was incorrectly saved as a green checkpoint"
}

echo "PASS: scale ladder grows 10k to 20k with exact counts, sentinels, metrics, and negative controls"
