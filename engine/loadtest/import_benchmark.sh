#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_HELPERS="$SCRIPT_DIR/lib/loadtest_shell_helpers.sh"
RESULTS_BASE_DIR="$SCRIPT_DIR/results"
DATA_DIR="$SCRIPT_DIR/data"

# --- helpers ---

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

# Returns epoch milliseconds using node for sub-second precision.
epoch_ms() {
  node -e 'process.stdout.write(String(Date.now()))'
}

# POST a single batch file. Writes status code to stdout, body to the file at $3.
# Uses raw curl so the caller can inspect the status code for error counting.
post_batch_file() {
  local index_path="$1"
  local batch_file="$2"
  local body_file="$3"
  local -a curl_args

  curl_args=(curl -sS -o "$body_file" -w '%{http_code}' -X POST)
  if [[ ${#LOADTEST_AUTH_HEADERS[@]} -gt 0 ]]; then
    curl_args+=("${LOADTEST_AUTH_HEADERS[@]}")
  fi
  curl_args+=(-H "Content-Type: application/json" --data-binary "@${batch_file}")
  curl_args+=("${FLAPJACK_BASE_URL}${index_path}")

  "${curl_args[@]}"
}

# Build and write the result artifact JSON using the JS helper.
write_result_artifact() {
  local result_file="$1"
  local total_docs="$2"
  local batch_count="$3"
  local error_count="$4"
  local wall_clock_ms="$5"
  local index_name="$6"
  local settings_source="$7"
  local latencies_json="$8"

  (
    cd "$SCRIPT_DIR"
    node --input-type=module -e '
import { buildResultArtifact } from "./import_benchmark.mjs";
const artifact = buildResultArtifact({
  totalDocs: Number(process.argv[1]),
  batchCount: Number(process.argv[2]),
  errorCount: Number(process.argv[3]),
  latenciesMs: JSON.parse(process.argv[5]),
  wallClockMs: Number(process.argv[4]),
  indexName: process.argv[6],
  settingsSource: process.argv[7],
});
process.stdout.write(JSON.stringify(artifact, null, 2) + "\n");
' "$total_docs" "$batch_count" "$error_count" "$wall_clock_ms" "$latencies_json" "$index_name" "$settings_source"
  ) > "$result_file"
}

# Count the number of batch operations in a Stage 1 payload file.
count_batch_requests() {
  local batch_file="$1"
  jq -er '(.requests // []) | length' "$batch_file"
}

# Emit a warning when the imported document count differs from expectation.
warn_if_doc_count_mismatch() {
  local expected_docs="$1"
  local actual_docs="$2"

  if [[ "$actual_docs" == "$expected_docs" ]]; then
    return 0
  fi

  echo "WARN: document count mismatch — expected $expected_docs, got $actual_docs"
  return 1
}

# Print a summary line to stdout from the result artifact JSON.
print_summary() {
  local result_file="$1"
  echo ""
  echo "=== Import Benchmark Results ==="
  jq -r '
    "Index:          " + .indexName,
    "Settings:       " + .settingsSource,
    "Total docs:     " + (.totalDocs | tostring),
    "Batches:        " + (.batchCount | tostring),
    "Errors:         " + (.errorCount | tostring),
    "Wall clock:     " + (.wallClockMs | tostring) + " ms",
    "Latency avg:    " + (.latency.avg | tostring) + " ms",
    "Latency p50:    " + (.latency.p50 | tostring) + " ms",
    "Latency p95:    " + (.latency.p95 | tostring) + " ms",
    "Latency p99:    " + (.latency.p99 | tostring) + " ms",
    "Latency min:    " + (.latency.min | tostring) + " ms",
    "Latency max:    " + (.latency.max | tostring) + " ms",
    (if .latencyWindows == null then
      "Latency windows: unavailable (no successful batches)"
    else
      "First/middle/last p50: " +
      ([.latencyWindows.first.p50, .latencyWindows.middle.p50, .latencyWindows.last.p50]
        | map(tostring) | join(" / ")) +
      " ms; last/first=" + (.latencyWindows.lastToFirstP50Ratio | tostring)
    end)
  ' < "$result_file"
  echo "================================"
}

# --- orchestration ---

# Discover and count batch files in DATA_DIR.
# Sets globals: BENCHMARK_BATCH_FILES (newline-separated paths), BENCHMARK_BATCH_COUNT.
discover_benchmark_batches() {
  [[ -d "$DATA_DIR" ]] || fail "data directory not found: $DATA_DIR"

  BENCHMARK_BATCH_FILES="$(
    cd "$SCRIPT_DIR"
    node --input-type=module -e '
import { listBatchFiles } from "./import_benchmark.mjs";
const files = await listBatchFiles(process.argv[1]);
if (files.length === 0) {
  console.error("FAIL: no batch files found in " + process.argv[1]);
  process.exit(1);
}
process.stdout.write(files.join("\n"));
' "$DATA_DIR"
  )" || fail "batch file discovery failed"

  BENCHMARK_BATCH_COUNT=0
  while IFS= read -r _; do
    BENCHMARK_BATCH_COUNT=$((BENCHMARK_BATCH_COUNT + 1))
  done <<< "$BENCHMARK_BATCH_FILES"
  echo "INFO: found $BENCHMARK_BATCH_COUNT batch file(s) in $DATA_DIR"
}

# Import all batch files and collect latency metrics.
# Sets globals: BENCHMARK_ERROR_COUNT, BENCHMARK_SUCCESSFUL_DOCS,
#               BENCHMARK_LATENCIES_JSON, BENCHMARK_WALL_CLOCK_MS.
run_batch_imports() {
  local batch_path="$1"
  local batch_files="$2"
  local batch_count="$3"

  BENCHMARK_ERROR_COUNT=0
  BENCHMARK_SUCCESSFUL_DOCS=0
  BENCHMARK_LATENCIES_JSON="["
  local first_latency=true
  local overall_start_ms
  overall_start_ms="$(epoch_ms)"
  local body_tmp
  body_tmp="$(mktemp)"

  local batch_num=0
  while IFS= read -r batch_file; do
    batch_num=$((batch_num + 1))
    local file_basename
    file_basename="$(basename "$batch_file")"

    local batch_start_ms
    batch_start_ms="$(epoch_ms)"

    local http_status
    http_status="$(post_batch_file "$batch_path" "$batch_file" "$body_tmp")"

    if [[ "$http_status" != "200" ]]; then
      echo "WARN: batch $file_basename returned HTTP $http_status — skipping"
      BENCHMARK_ERROR_COUNT=$((BENCHMARK_ERROR_COUNT + 1))
      continue
    fi

    # Extract and validate taskID
    local task_response
    local task_id
    task_response="$(<"$body_tmp")"
    task_id="$(extract_loadtest_numeric_task_id "$task_response" 2>/dev/null)" || {
      echo "WARN: batch $file_basename returned non-numeric or missing taskID — skipping"
      BENCHMARK_ERROR_COUNT=$((BENCHMARK_ERROR_COUNT + 1))
      continue
    }

    # Wait for task to settle
    wait_for_loadtest_task_published "$task_id"

    local batch_end_ms
    batch_end_ms="$(epoch_ms)"
    local latency_ms=$((batch_end_ms - batch_start_ms))
    local batch_doc_count
    batch_doc_count="$(count_batch_requests "$batch_file")"
    BENCHMARK_SUCCESSFUL_DOCS=$((BENCHMARK_SUCCESSFUL_DOCS + batch_doc_count))

    if [[ "$first_latency" == "true" ]]; then
      BENCHMARK_LATENCIES_JSON="${BENCHMARK_LATENCIES_JSON}${latency_ms}"
      first_latency=false
    else
      BENCHMARK_LATENCIES_JSON="${BENCHMARK_LATENCIES_JSON},${latency_ms}"
    fi

    echo "INFO: batch $batch_num/$batch_count ($file_basename) — ${latency_ms}ms (task $task_id)"
  done <<< "$batch_files"

  BENCHMARK_LATENCIES_JSON="${BENCHMARK_LATENCIES_JSON}]"

  local overall_end_ms
  overall_end_ms="$(epoch_ms)"
  BENCHMARK_WALL_CLOCK_MS=$((overall_end_ms - overall_start_ms))

  rm -f "$body_tmp"
}

# --- main ---

main() {
  [[ -f "$LOADTEST_HELPERS" ]] || fail "missing $LOADTEST_HELPERS"

  # shellcheck source=lib/loadtest_shell_helpers.sh
  source "$LOADTEST_HELPERS"

  require_loadtest_commands curl jq node
  load_shared_loadtest_config
  initialize_loadtest_auth_headers
  load_dashboard_seed_settings "$SCRIPT_DIR"

  local index_name="$FLAPJACK_BENCHMARK_INDEX"
  local settings_source="engine/loadtest/product-seed-data.mjs::seedSettings"

  discover_benchmark_batches

  echo "INFO: resetting index '$index_name'"
  reset_loadtest_index "$index_name"
  echo "INFO: applying settings from $settings_source"
  apply_loadtest_index_settings "$index_name"

  local encoded_index
  encoded_index="$(loadtest_encode_path_component "$index_name")"
  local batch_path="/1/indexes/${encoded_index}/batch"

  run_batch_imports "$batch_path" "$BENCHMARK_BATCH_FILES" "$BENCHMARK_BATCH_COUNT"

  # Verify final document count
  local final_doc_count
  final_doc_count="$(loadtest_get_index_doc_count "$index_name")"
  echo "INFO: final document count in '$index_name': $final_doc_count"
  warn_if_doc_count_mismatch "$BENCHMARK_SUCCESSFUL_DOCS" "$final_doc_count" || true

  # Write result artifact
  local timestamp
  timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
  local result_dir="$RESULTS_BASE_DIR/$timestamp"
  mkdir -p "$result_dir"
  local result_file="$result_dir/import_benchmark.json"

  write_result_artifact "$result_file" "$final_doc_count" "$BENCHMARK_BATCH_COUNT" "$BENCHMARK_ERROR_COUNT" \
    "$BENCHMARK_WALL_CLOCK_MS" "$index_name" "$settings_source" "$BENCHMARK_LATENCIES_JSON"

  print_summary "$result_file"
  echo "INFO: artifact written to $result_file"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
