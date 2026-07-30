#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
GENERATOR="$SCRIPT_DIR/generate_dataset.mjs"
IMPORT_SCRIPT="$SCRIPT_DIR/import_benchmark.sh"
SEARCH_SCRIPT="${FLAPJACK_SCALE_SEARCH_SCRIPT:-$SCRIPT_DIR/search_benchmark.sh}"
HELPERS="$SCRIPT_DIR/lib/loadtest_shell_helpers.sh"
CAPACITY_HELPER="$SCRIPT_DIR/lib/scale_capacity.mjs"
CAPACITY_OBSERVATION_HELPER="$SCRIPT_DIR/lib/scale_capacity_observation.mjs"
RUNG_VERDICT_HELPER="$SCRIPT_DIR/lib/scale_rung_verdict.mjs"
SCALE_BULK_BUILD_HELPER="$SCRIPT_DIR/lib/scale_bulk_build_probe.sh"
SEARCH_SAMPLES_PER_TYPE=30

# shellcheck source=lib/loadtest_shell_helpers.sh
source "$HELPERS"
# Reuse post_batch_file, run_batch_imports, and write_result_artifact without
# invoking import_benchmark.sh's reset-oriented main entrypoint.
# shellcheck source=import_benchmark.sh
source "$IMPORT_SCRIPT"
# shellcheck source=lib/scale_bulk_build_probe.sh
source "$SCALE_BULK_BUILD_HELPER"

SENTINELS_PER_RUNG=2
SERVER_PID=""
RUNNER_TMP_DIR=""
RESULTS_DIR=""
SERVER_DATA_DIR=""
RUN_SUCCEEDED=0
INTERRUPTED_EXIT_CODE=0
CLEANUP_COMPLETE=0
ACTIVE_RUNG=0
FAILURE_OUTCOME="FAILED"
export RESOURCE_MONITOR_PID=""

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: scale_ladder.sh --profile <compact|standard> --rungs <n,n,...> --data-dir <path> [options]

Options:
  --base-url <url>                 Server URL (default: http://127.0.0.1:7700)
  --batch-size <n>                 Import batch size (default: 1000)
  --results-dir <path>             Durable result directory
  --server-binary <path>           Release flapjack binary
  --stall-seconds <n>              Flat-count failure window (default: 60)
  --min-docs-per-second <n>        Per-tranche throughput floor (default: 1)
  --resume                         Continue from an exact saved checkpoint
  --stop-after-rung <n>            Exit green after checkpointing this rung
  --throughput-probe               Continue past latency failures for the dedicated 1M/4M/8M probe
  --bulk-build-throughput-probe    Rebuild each target through the atomic bulk-replace path
  --exercise-negative-controls     Delete a sentinel and probe a wrong index, then restore state

Capacity estimates can be overridden with SCALE_SOURCE_BYTES_PER_RECORD,
SCALE_INDEX_BYTES_PER_RECORD, SCALE_RSS_BYTES_PER_RECORD,
SCALE_DISK_RESERVE_BYTES, and SCALE_MEMORY_RESERVE_BYTES.
EOF
}

run_import_worker() {
  local dataset_dir="$1"
  local index_name="$2"
  local result_file="$3"
  local batch_files
  local batch_count=0
  local encoded_index

  require_loadtest_commands curl jq node
  load_shared_loadtest_config
  initialize_loadtest_auth_headers

  batch_files="$(
    cd "$SCRIPT_DIR"
    node --input-type=module -e '
import { listBatchFiles } from "./import_benchmark.mjs";
const files = await listBatchFiles(process.argv[1]);
if (files.length === 0) throw new Error(`no batch files found in ${process.argv[1]}`);
process.stdout.write(files.join("\n"));
' "$dataset_dir"
  )"
  while IFS= read -r _; do
    batch_count=$((batch_count + 1))
  done <<<"$batch_files"

  encoded_index="$(loadtest_encode_path_component "$index_name")"
  run_batch_imports "/1/indexes/${encoded_index}/batch" "$batch_files" "$batch_count"
  write_result_artifact \
    "$result_file" \
    "$BENCHMARK_SUCCESSFUL_DOCS" \
    "$batch_count" \
    "$BENCHMARK_ERROR_COUNT" \
    "$BENCHMARK_WALL_CLOCK_MS" \
    "$index_name" \
    "scale_ladder:${SCALE_PROFILE}" \
    "$BENCHMARK_LATENCIES_JSON"

  [[ "$BENCHMARK_ERROR_COUNT" -eq 0 ]] || {
    echo "FAIL: import worker recorded ${BENCHMARK_ERROR_COUNT} batch errors" >&2
    return 1
  }
}

if [[ "${1:-}" == "--import-worker" ]]; then
  [[ $# -eq 4 ]] || fail "--import-worker requires dataset-dir, index-name, and result-file"
  run_import_worker "$2" "$3" "$4"
  exit $?
fi

if [[ "${1:-}" == "--bulk-build-worker" ]]; then
  [[ $# -eq 4 ]] || fail "--bulk-build-worker requires dataset-dir, index-name, and result-file"
  run_scale_bulk_build_worker "$2" "$3" "$4"
  exit $?
fi

evaluate_rung_liveness() {
  local rung_dir="$1"
  local sample_path="$2"
  local summary_path="${rung_dir}/liveness_summary.txt"
  local distribution_output

  mkdir -p "$rung_dir"
  # The helper owns the minimum-sample contract; its default fails empty and endpoint-missing samples closed.
  if distribution_output="$(liveness_distribution "$sample_path" 250 5000 2>&1)"; then
    printf '%s\n' "$distribution_output" > "$summary_path"
    printf 'LIVENESS_PASSED\n'
    return 0
  fi

  printf '%s\n' "$distribution_output" > "$summary_path"
  if ! grep -Eq '(^|[[:space:]])verdict=fail([[:space:]]|$)' "$summary_path"; then
    printf 'verdict=fail reason=liveness_distribution_failed\n' >> "$summary_path"
  fi
  printf 'LIVENESS_FAILED\n'
  return 1
}

portable_dir_size_bytes() {
  local directory="$1"
  local size_bytes
  size_bytes="$(du -sk "$directory" | awk 'NR == 1 { print $1 * 1024 }')" || return 1
  [[ "$size_bytes" =~ ^[0-9]+$ ]] || return 1
  (( size_bytes > 0 )) || return 1
  printf '%s\n' "$size_bytes"
}

self_test_portable_dir_size() {
  local fixture_dir="$RUNNER_TMP_DIR/du_size_self_test"
  mkdir -p "$fixture_dir"
  printf 'portable-size-probe\n' > "$fixture_dir/specimen.txt"
  portable_dir_size_bytes "$fixture_dir" >/dev/null || {
    fail "portable du -sk size probe did not return a positive integer"
  }
}

remove_completed_dataset() {
  local dataset_dir="$1"
  local receipt_path="$2"
  local dataset_bytes

  case "$dataset_dir" in
    "$RUNNER_TMP_DIR"/rung_*_dataset)
      ;;
    *)
      fail "refusing generated-dataset cleanup outside runner state: ${dataset_dir}"
      ;;
  esac
  [[ -d "$dataset_dir" ]] || fail "completed dataset directory is missing: ${dataset_dir}"
  dataset_bytes="$(portable_dir_size_bytes "$dataset_dir")" || {
    fail "completed dataset size is missing, zero, or non-numeric: ${dataset_dir}"
  }

  rm -rf "$dataset_dir"
  [[ ! -e "$dataset_dir" ]] || fail "completed dataset cleanup did not remove ${dataset_dir}"
  jq -n \
    --arg dataset_dir "$dataset_dir" \
    --argjson dataset_bytes "$dataset_bytes" \
    '{datasetDir: $dataset_dir, datasetBytes: $dataset_bytes, removed: true}' \
    > "$receipt_path"
}

disk_free_bytes() {
  local directory="$1"
  local available_bytes="${SCALE_DISK_FREE_BYTES_OVERRIDE:-}"

  if [[ -z "$available_bytes" ]]; then
    available_bytes="$(df -Pk "$directory" | awk 'NR == 2 { printf "%.0f", $4 * 1024 }')"
  fi
  [[ "$available_bytes" =~ ^[1-9][0-9]*$ ]] || {
    echo "FAIL: disk-free evidence is missing, zero, or non-numeric: ${available_bytes}" >&2
    return 1
  }
  printf '%s\n' "$available_bytes"
}

memory_capacity_bytes() {
  local capacity_bytes="${SCALE_MEMORY_CAPACITY_BYTES_OVERRIDE:-}"

  if [[ -z "$capacity_bytes" ]]; then
    capacity_bytes="$(node -e 'process.stdout.write(String(require("node:os").totalmem()))')"
  fi
  [[ "$capacity_bytes" =~ ^[1-9][0-9]*$ ]] || {
    echo "FAIL: memory-capacity evidence is missing, zero, or non-numeric: ${capacity_bytes}" >&2
    return 1
  }
  printf '%s\n' "$capacity_bytes"
}

capacity_source_bytes_per_record() {
  if [[ "$PROFILE" == "compact" ]]; then
    printf '%s\n' "${SCALE_SOURCE_BYTES_PER_RECORD:-512}"
  else
    printf '%s\n' "${SCALE_SOURCE_BYTES_PER_RECORD:-2048}"
  fi
}

capacity_index_bytes_per_record() {
  if [[ "$PROFILE" == "compact" ]]; then
    printf '%s\n' "${SCALE_INDEX_BYTES_PER_RECORD:-16384}"
  else
    printf '%s\n' "${SCALE_INDEX_BYTES_PER_RECORD:-32768}"
  fi
}

capacity_rss_bytes_per_record() {
  if [[ "$PROFILE" == "compact" ]]; then
    printf '%s\n' "${SCALE_RSS_BYTES_PER_RECORD:-4096}"
  else
    printf '%s\n' "${SCALE_RSS_BYTES_PER_RECORD:-8192}"
  fi
}

run_capacity_preflight() {
  local starting_count="$1"
  local target_count="$2"
  local receipt_path="$3"
  local source_bytes_per_record
  local index_bytes_per_record
  local rss_bytes_per_record
  local disk_reserve_bytes="${SCALE_DISK_RESERVE_BYTES:-53687091200}"
  local memory_reserve_bytes="${SCALE_MEMORY_RESERVE_BYTES:-17179869184}"
  local free_disk
  local memory_capacity
  local input_json
  local helper_exit_code=0
  local verdict

  source_bytes_per_record="$(capacity_source_bytes_per_record)"
  index_bytes_per_record="$(capacity_index_bytes_per_record)"
  rss_bytes_per_record="$(capacity_rss_bytes_per_record)"

  free_disk="$(disk_free_bytes "$SERVER_DATA_DIR")" || return 1
  memory_capacity="$(memory_capacity_bytes)" || return 1
  input_json="$(
    jq -cn \
      --arg profile "$PROFILE" \
      --argjson starting_count "$starting_count" \
      --argjson target_count "$target_count" \
      --argjson disk_free_bytes "$free_disk" \
      --argjson memory_capacity_bytes "$memory_capacity" \
      --argjson source_bytes_per_record "$source_bytes_per_record" \
      --argjson index_bytes_per_record "$index_bytes_per_record" \
      --argjson rss_bytes_per_record "$rss_bytes_per_record" \
      --argjson disk_reserve_bytes "$disk_reserve_bytes" \
      --argjson memory_reserve_bytes "$memory_reserve_bytes" \
      '{
        profile: $profile,
        startingCount: $starting_count,
        targetCount: $target_count,
        diskFreeBytes: $disk_free_bytes,
        memoryCapacityBytes: $memory_capacity_bytes,
        sourceBytesPerRecord: $source_bytes_per_record,
        indexBytesPerRecord: $index_bytes_per_record,
        rssBytesPerRecord: $rss_bytes_per_record,
        diskReserveBytes: $disk_reserve_bytes,
        memoryReserveBytes: $memory_reserve_bytes
      }'
  )" || return 1

  node "$CAPACITY_HELPER" --input-json "$input_json" > "$receipt_path" || helper_exit_code=$?
  verdict="$(jq -er '.verdict' "$receipt_path" 2>/dev/null)" || {
    echo "FAIL: capacity preflight receipt is missing or unparseable: ${receipt_path}" >&2
    return 1
  }
  if [[ "$helper_exit_code" -ne 0 || "$verdict" != "GO" ]]; then
    echo "FAIL: capacity preflight verdict for rung ${target_count}: ${verdict}" >&2
    return 1
  fi
}

run_capacity_observation() {
  local target_count="$1"
  local index_bytes="$2"
  local rss_bytes="$3"
  local receipt_path="$4"
  local input_json

  input_json="$(
    jq -cn \
      --arg profile "$PROFILE" \
      --argjson target_count "$target_count" \
      --argjson index_bytes "$index_bytes" \
      --argjson rss_bytes "$rss_bytes" \
      --argjson index_allowance "$(capacity_index_bytes_per_record)" \
      --argjson rss_allowance "$(capacity_rss_bytes_per_record)" \
      '{
        profile: $profile,
        targetCount: $target_count,
        indexBytes: $index_bytes,
        rssBytes: $rss_bytes,
        indexBytesPerRecordAllowance: $index_allowance,
        rssBytesPerRecordAllowance: $rss_allowance
      }'
  )" || return 2

  node "$CAPACITY_OBSERVATION_HELPER" --input-json "$input_json" > "$receipt_path"
}

write_run_receipt() {
  local outcome="$1"
  local terminal_rung="${2:-0}"
  local terminal_metrics_path="${3:-}"
  local remaining_generated_dataset_dirs=0
  local remaining_dataset_dir

  for remaining_dataset_dir in "$RUNNER_TMP_DIR"/rung_*_dataset; do
    if [[ -d "$remaining_dataset_dir" ]]; then
      remaining_generated_dataset_dirs=$((remaining_generated_dataset_dirs + 1))
    fi
  done
  jq -n \
    --arg outcome "$outcome" \
    --arg runner_tmp_dir "$RUNNER_TMP_DIR" \
    --arg terminal_metrics_path "$terminal_metrics_path" \
    --argjson terminal_rung "$terminal_rung" \
    --argjson remaining_generated_dataset_dirs "$remaining_generated_dataset_dirs" \
    '{
      outcome: $outcome,
      runnerTmpDir: $runner_tmp_dir,
      remainingGeneratedDatasetDirs: $remaining_generated_dataset_dirs,
      terminalRung: (if $terminal_rung > 0 then $terminal_rung else null end),
      terminalMetricsPath: (if $terminal_metrics_path != "" then $terminal_metrics_path else null end)
    }' > "$RESULTS_DIR/run_receipt.json"
  case "$outcome" in
    FAILED|LIVENESS_FAILED|IMPORT_FAILED)
      ;;
    *)
      [[ "$remaining_generated_dataset_dirs" -eq 0 ]] || {
        fail "green ladder retained ${remaining_generated_dataset_dirs} generated dataset directories"
      }
      ;;
  esac
}

current_run_purpose() {
  if [[ "$WORKLOAD" == "bulk_build" ]]; then
    printf 'bulk_build_throughput_probe\n'
  elif [[ "$THROUGHPUT_PROBE" -eq 1 ]]; then
    printf 'throughput_probe\n'
  else
    printf 'reference_ladder\n'
  fi
}

write_checkpoint() {
  local completed_rung="$1"
  local metrics_path="$2"
  local checkpoint_tmp="${CHECKPOINT_PATH}.tmp.$$"
  local purpose
  purpose="$(current_run_purpose)"

  jq -n \
    --arg profile "$PROFILE" \
    --arg purpose "$purpose" \
    --arg workload "$WORKLOAD" \
    --arg data_dir "$SERVER_DATA_DIR" \
    --arg results_dir "$RESULTS_DIR" \
    --arg index_name "$index_name" \
    --arg metrics_path "$metrics_path" \
    --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson batch_size "$BATCH_SIZE" \
    --argjson rungs "$RUNGS_JSON" \
    --argjson last_completed_rung "$completed_rung" \
    '{
      version: 4,
      profile: $profile,
      workload: $workload,
      purpose: $purpose,
      rungs: $rungs,
      batchSize: $batch_size,
      dataDir: $data_dir,
      resultsDir: $results_dir,
      indexName: $index_name,
      lastCompletedRung: $last_completed_rung,
      metricsPath: $metrics_path,
      timestamp: $timestamp
    }' > "$checkpoint_tmp"
  jq -e '.version == 4 and .batchSize > 0 and .lastCompletedRung > 0' "$checkpoint_tmp" >/dev/null || {
    fail "new checkpoint is missing required fields"
  }
  mv "$checkpoint_tmp" "$CHECKPOINT_PATH"
}

validate_resume_checkpoint() {
  local checkpoint_metrics_path
  local last_completed_rung
  local purpose
  purpose="$(current_run_purpose)"

  [[ -s "$CHECKPOINT_PATH" ]] || fail "resume checkpoint is missing or empty: ${CHECKPOINT_PATH}"
  jq -e \
    --arg profile "$PROFILE" \
    --arg purpose "$purpose" \
    --arg workload "$WORKLOAD" \
    --arg data_dir "$SERVER_DATA_DIR" \
    --arg results_dir "$RESULTS_DIR" \
    --arg index_name "$index_name" \
    --argjson batch_size "$BATCH_SIZE" \
    --argjson rungs "$RUNGS_JSON" \
    '
      .version == 4 and
      .profile == $profile and
      (.workload // "import") == $workload and
      .purpose == $purpose and
      .rungs == $rungs and
      .batchSize == $batch_size and
      .dataDir == $data_dir and
      .resultsDir == $results_dir and
      .indexName == $index_name and
      (.lastCompletedRung as $last | ($rungs | index($last)) != null) and
      .metricsPath == (
        $results_dir + "/rung_" + (.lastCompletedRung | tostring) + "/metrics.json"
      )
    ' "$CHECKPOINT_PATH" >/dev/null || {
      fail "resume checkpoint does not exactly match profile, rungs, batch size, data dir, results dir, and index"
    }

  last_completed_rung="$(jq -er '.lastCompletedRung' "$CHECKPOINT_PATH")"
  checkpoint_metrics_path="$(jq -er '.metricsPath' "$CHECKPOINT_PATH")"
  [[ -s "$checkpoint_metrics_path" ]] || {
    fail "resume checkpoint metrics are missing or empty: ${checkpoint_metrics_path}"
  }
  jq -e \
    --arg profile "$PROFILE" \
    --arg purpose "$purpose" \
    --arg workload "$WORKLOAD" \
    --argjson expected_count "$last_completed_rung" \
    '
      .profile == $profile and
      (.workload // "import") == $workload and
      .runPurpose == $purpose and
      .targetCount == $expected_count and
      .finalCount == $expected_count and
      .sentinels == "PASS" and
      .capacityObservation.verdict == "PASS" and
      (
        $purpose == "throughput_probe" or
        .rungVerdict == "PASS"
      )
    ' "$checkpoint_metrics_path" >/dev/null || {
      fail "resume checkpoint metrics do not prove an exact green rung"
    }

  RESUME_START_COUNT="$last_completed_rung"
}

create_sentinel_batch() {
  local output_path="$1"
  local rung="$2"
  jq -n --arg rung "$rung" '{
    requests: [
      {
        action: "addObject",
        body: {
          objectID: ("zzsentinel_" + $rung + "_0"),
          name: ("xyzzysentinel" + $rung + "0"),
          brand: "Sentinel",
          category: "Guard",
          donorType: "sentinel",
          region: "guard",
          tags: ["sentinel", "guard"]
        }
      },
      {
        action: "addObject",
        body: {
          objectID: ("zzsentinel_" + $rung + "_1"),
          name: ("xyzzysentinel" + $rung + "1"),
          brand: "Sentinel",
          category: "Guard",
          donorType: "sentinel",
          region: "guard",
          tags: ["sentinel", "guard"]
        }
      }
    ]
  }' > "$output_path"
}

server_rss_bytes() {
  local server_pid="$1"
  local rss_bytes
  rss_bytes="$(ps -o rss= -p "$server_pid" | awk 'NR == 1 { print $1 * 1024 }')" || return 1
  [[ "$rss_bytes" =~ ^[0-9]+$ ]] || return 1
  (( rss_bytes > 0 )) || return 1
  printf '%s\n' "$rss_bytes"
}

delete_object_and_wait() {
  local index_name="$1"
  local object_id="$2"
  local encoded_index
  local encoded_object
  local response

  encoded_index="$(loadtest_encode_path_component "$index_name")"
  encoded_object="$(loadtest_encode_path_component "$object_id")"
  response="$(loadtest_http_request DELETE "/1/indexes/${encoded_index}/${encoded_object}" "" "200")"
  wait_for_loadtest_task_published "$(extract_loadtest_numeric_task_id "$response")"
}

post_sentinel_batch_and_wait() {
  local index_name="$1"
  local sentinel_batch="$2"
  local encoded_index
  local response_file
  local status_code
  local response

  encoded_index="$(loadtest_encode_path_component "$index_name")"
  response_file="$(mktemp)"
  status_code="$(post_batch_file "/1/indexes/${encoded_index}/batch" "$sentinel_batch" "$response_file")"
  [[ "$status_code" == "200" ]] || fail "sentinel restore returned HTTP ${status_code}"
  response="$(<"$response_file")"
  rm -f "$response_file"
  wait_for_loadtest_task_published "$(extract_loadtest_numeric_task_id "$response")"
}

exercise_negative_controls() {
  local base_url="$1"
  local index_name="$2"
  local rung="$3"
  local target_count="$4"
  local sentinel_batch="$5"
  local wrong_index_output
  local sentinel_output
  local after_delete_count
  local restored_count

  if wrong_index_output="$(
    COUNT_POLL_INTERVAL_SECONDS=0.05 \
      wait_for_count_or_stall "$base_url" "${index_name}_missing_guard" 1 1 2>&1
  )"; then
    fail "wrong-index negative control unexpectedly passed"
  fi
  [[ "$wrong_index_output" == *"FAIL:"* || "$wrong_index_output" == *"STALL:"* ]] || {
    fail "wrong-index negative control failed without a fail-closed message"
  }

  delete_object_and_wait "$index_name" "zzsentinel_${rung}_0"
  after_delete_count="$(index_doc_count "$base_url" "$index_name")"
  [[ "$after_delete_count" -eq $((target_count - 1)) ]] || {
    fail "sentinel delete negative control expected $((target_count - 1)) docs, got ${after_delete_count}"
  }
  if sentinel_output="$(assert_sentinels_top1 "$base_url" "$index_name" "$rung" 2>&1)"; then
    fail "deleted-sentinel negative control unexpectedly passed"
  fi
  [[ "$sentinel_output" == *"ranked #1"* ]] || {
    fail "deleted-sentinel negative control failed for the wrong reason: ${sentinel_output}"
  }

  post_sentinel_batch_and_wait "$index_name" "$sentinel_batch"
  restored_count="$(index_doc_count "$base_url" "$index_name")"
  [[ "$restored_count" -eq "$target_count" ]] || {
    fail "sentinel restore expected ${target_count} docs, got ${restored_count}"
  }
  assert_sentinels_top1 "$base_url" "$index_name" "$rung"
}

cleanup() {
  local script_exit_code=$?
  local receipt_runner_tmp_dir=""
  local -a backpressure_pause_sources=()
  local -a backpressure_pause_receipt_lines=()
  local -a liveness_evidence_receipt_lines=()
  local backpressure_pause_artifact_count=0
  local backpressure_pause_source_count=0
  local liveness_evidence_count=0
  local active_liveness_path active_liveness_relative active_liveness_i
  local pause_source pause_relative pause_parent pause_destination pause_decision pause_i
  if [[ "$CLEANUP_COMPLETE" -eq 1 ]]; then
    return
  fi
  CLEANUP_COMPLETE=1
  stop_scale_build_resource_monitor

  # Results and server logs already live outside RUNNER_TMP_DIR. SERVER_DATA_DIR
  # is caller-owned and is never deleted. On failure the generated tranche data
  # is retained at RUNNER_TMP_DIR before the server is stopped.
  if [[ "$RUN_SUCCEEDED" -ne 1 || "$script_exit_code" -ne 0 || "$INTERRUPTED_EXIT_CODE" -ne 0 ]]; then
    # jul26_8pm_9 lost this pause evidence, but full ladder data snapshots are too
    # large at the 1,000,000-record rung. Preserve only the decision artifacts.
    if [[ -n "$RESULTS_DIR" && -n "$SERVER_DATA_DIR" && -d "$SERVER_DATA_DIR" ]]; then
      while IFS= read -r -d '' pause_source; do
        backpressure_pause_sources+=("$pause_source")
      done < <(
        find "$SERVER_DATA_DIR" -type f -name 'write_backpressure_pause.json' -print0 ||
          echo "ERROR: failed to discover backpressure pause artifacts in ${SERVER_DATA_DIR}" >&2
      )
    fi
    backpressure_pause_source_count="${#backpressure_pause_sources[@]}"
    # Guard the iteration: under `set -u`, bash 3.2 (the macOS system bash) treats
    # "${arr[@]}" on an empty array as an unbound variable and aborts cleanup() before
    # failure_evidence.txt is written and the server is stopped — the exact evidence loss
    # this path exists to prevent. The zero-artifact case is common (e.g. capacity NO_GO
    # rejects a rung before any tenant writes a pause artifact).
    for ((pause_i = 0; pause_i < backpressure_pause_source_count; pause_i++)); do
      pause_source="${backpressure_pause_sources[pause_i]}"
      pause_relative="${pause_source#"$SERVER_DATA_DIR"/}"
      pause_parent="${pause_relative%/write_backpressure_pause.json}"
      pause_destination="$RESULTS_DIR/backpressure_pause_artifacts/$pause_relative"
      if ! mkdir -p "${pause_destination%/*}"; then
        echo "ERROR: failed to create directory for backpressure pause artifact ${pause_relative}" >&2
      elif ! cp "$pause_source" "$pause_destination"; then
        echo "ERROR: failed to preserve backpressure pause artifact ${pause_relative}" >&2
      else
        pause_decision="unknown"
        if ! pause_decision="$(jq -r '.decision // "unknown"' "$pause_destination" 2>/dev/null)"; then
          echo "ERROR: failed to read decision from preserved artifact ${pause_relative}" >&2
          pause_decision="unknown"
        fi
        backpressure_pause_artifact_count=$((backpressure_pause_artifact_count + 1))
        backpressure_pause_receipt_lines+=(
          "backpressure_pause_artifact=${pause_parent}:${pause_decision}:backpressure_pause_artifacts/${pause_relative}"
        )
      fi
    done
    if [[ -n "$RESULTS_DIR" && "$ACTIVE_RUNG" -gt 0 ]]; then
      for active_liveness_path in \
        "$RESULTS_DIR/rung_${ACTIVE_RUNG}/liveness_samples.tsv" \
        "$RESULTS_DIR/rung_${ACTIVE_RUNG}/liveness_summary.txt"; do
        if [[ -e "$active_liveness_path" ]]; then
          active_liveness_relative="${active_liveness_path#"$RESULTS_DIR"/}"
          liveness_evidence_count=$((liveness_evidence_count + 1))
          liveness_evidence_receipt_lines+=("liveness_evidence=${active_liveness_relative}")
        fi
      done
    fi
    if [[ -n "$RESULTS_DIR" ]]; then
      if ! {
        echo "outcome=FAIL"
        echo "failure_outcome=${FAILURE_OUTCOME}"
        echo "script_exit_code=${script_exit_code}"
        echo "interrupted_exit_code=${INTERRUPTED_EXIT_CODE}"
        echo "runner_state=${RUNNER_TMP_DIR}"
        echo "server_data=${SERVER_DATA_DIR}"
        echo "backpressure_pause_artifact_count=${backpressure_pause_artifact_count}"
        if [[ "$backpressure_pause_source_count" -eq 0 ]]; then
          echo "backpressure_pause_artifacts=none present"
        elif [[ "$backpressure_pause_artifact_count" -gt 0 ]]; then
          printf '%s\n' "${backpressure_pause_receipt_lines[@]}"
        fi
        echo "liveness_evidence_count=${liveness_evidence_count}"
        if [[ "$liveness_evidence_count" -eq 0 ]]; then
          echo "liveness_evidence=none present"
        else
          for ((active_liveness_i = 0; active_liveness_i < liveness_evidence_count; active_liveness_i++)); do
            printf '%s\n' "${liveness_evidence_receipt_lines[active_liveness_i]}"
          done
        fi
      } > "$RESULTS_DIR/failure_evidence.txt"; then
        echo "ERROR: failed to write failure evidence receipt in ${RESULTS_DIR}" >&2
      fi
    fi
    if [[ -n "$RESULTS_DIR" && -n "$RUNNER_TMP_DIR" ]]; then
      receipt_runner_tmp_dir="$(
        jq -er '.runnerTmpDir' "$RESULTS_DIR/run_receipt.json" 2>/dev/null || true
      )"
      if [[ "$receipt_runner_tmp_dir" != "$RUNNER_TMP_DIR" ]]; then
        write_run_receipt "$FAILURE_OUTCOME" "$ACTIVE_RUNG"
      fi
    fi
    if [[ -n "$RUNNER_TMP_DIR" ]]; then
      echo "INFO: preserving failure runner state at ${RUNNER_TMP_DIR}" >&2
    fi
  fi

  if [[ -n "$SERVER_PID" ]]; then
    stop_loadtest_server "$SERVER_PID"
  fi
  if [[ "$RUN_SUCCEEDED" -eq 1 && "$script_exit_code" -eq 0 && -n "$RUNNER_TMP_DIR" ]]; then
    rm -rf "$RUNNER_TMP_DIR"
  fi
}

if [[ "${FLAPJACK_SCALE_LADDER_SKIP_MAIN:-0}" != "1" ]]; then

trap cleanup EXIT
trap 'INTERRUPTED_EXIT_CODE=130; exit 130' INT
trap 'INTERRUPTED_EXIT_CODE=143; exit 143' TERM

PROFILE=""
RUNGS_CSV=""
BASE_URL="http://127.0.0.1:7700"
BATCH_SIZE=1000
STALL_SECONDS=60
MIN_DOCS_PER_SECOND=1
SERVER_BINARY="$ENGINE_DIR/target/release/flapjack"
EXERCISE_NEGATIVE_CONTROLS=0
THROUGHPUT_PROBE=0
WORKLOAD="import"
RESUME=0
RESUME_START_COUNT=0
STOP_AFTER_RUNG=""
RESULTS_DIR_EXPLICIT=0
IMPORT_TIMEOUT_SECONDS="${SCALE_IMPORT_TIMEOUT_SECONDS:-43200}"
GENERATE_TIMEOUT_SECONDS="${SCALE_GENERATE_TIMEOUT_SECONDS:-7200}"
SEARCH_TIMEOUT_SECONDS="${SCALE_SEARCH_TIMEOUT_SECONDS:-1800}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile)
      PROFILE="${2:-}"
      shift 2
      ;;
    --rungs)
      RUNGS_CSV="${2:-}"
      shift 2
      ;;
    --base-url)
      BASE_URL="${2:-}"
      shift 2
      ;;
    --batch-size)
      BATCH_SIZE="${2:-}"
      shift 2
      ;;
    --stall-seconds)
      STALL_SECONDS="${2:-}"
      shift 2
      ;;
    --min-docs-per-second)
      MIN_DOCS_PER_SECOND="${2:-}"
      shift 2
      ;;
    --server-binary)
      SERVER_BINARY="${2:-}"
      shift 2
      ;;
    --data-dir)
      SERVER_DATA_DIR="${2:-}"
      shift 2
      ;;
    --results-dir)
      RESULTS_DIR="${2:-}"
      RESULTS_DIR_EXPLICIT=1
      shift 2
      ;;
    --resume)
      RESUME=1
      shift
      ;;
    --stop-after-rung)
      STOP_AFTER_RUNG="${2:-}"
      shift 2
      ;;
    --throughput-probe)
      THROUGHPUT_PROBE=1
      shift
      ;;
    --bulk-build-throughput-probe)
      THROUGHPUT_PROBE=1
      WORKLOAD="bulk_build"
      shift
      ;;
    --exercise-negative-controls)
      EXERCISE_NEGATIVE_CONTROLS=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

[[ "$PROFILE" == "compact" || "$PROFILE" == "standard" ]] || {
  fail "--profile must be compact or standard"
}
[[ -n "$RUNGS_CSV" ]] || fail "--rungs is required"
[[ -n "$SERVER_DATA_DIR" ]] || fail "--data-dir is required"
[[ "$BATCH_SIZE" =~ ^[1-9][0-9]*$ ]] || fail "--batch-size must be a positive integer"
(( BATCH_SIZE <= 10000 )) || fail "--batch-size cannot exceed the server maximum of 10000"
[[ -x "$SERVER_BINARY" ]] || fail "missing executable server binary: $SERVER_BINARY"
if [[ "$RESUME" -eq 1 && "$RESULTS_DIR_EXPLICIT" -ne 1 ]]; then
  fail "--resume requires an explicit --results-dir"
fi
awk -v value="$MIN_DOCS_PER_SECOND" 'BEGIN { exit !(value > 0) }' || {
  fail "--min-docs-per-second must be positive"
}

IFS=',' read -r -a RUNGS <<<"$RUNGS_CSV"
[[ "${#RUNGS[@]}" -gt 0 ]] || fail "--rungs did not contain any values"
previous_rung=0
for rung in "${RUNGS[@]}"; do
  [[ "$rung" =~ ^[1-9][0-9]*$ ]] || fail "invalid rung: $rung"
  (( rung > previous_rung )) || fail "rungs must be strictly increasing"
  (( rung - previous_rung > SENTINELS_PER_RUNG )) || {
    fail "each rung must add more than ${SENTINELS_PER_RUNG} documents"
  }
  previous_rung="$rung"
done
if [[ -n "$STOP_AFTER_RUNG" ]]; then
  [[ "$STOP_AFTER_RUNG" =~ ^[1-9][0-9]*$ ]] || {
    fail "--stop-after-rung must be a positive integer"
  }
  stop_rung_found=0
  for rung in "${RUNGS[@]}"; do
    if [[ "$rung" -eq "$STOP_AFTER_RUNG" ]]; then
      stop_rung_found=1
    fi
  done
  [[ "$stop_rung_found" -eq 1 ]] || {
    fail "--stop-after-rung must name one of the configured rungs"
  }
fi

require_loadtest_commands curl jq node ps du awk timeout
mkdir -p "$SERVER_DATA_DIR"
SERVER_DATA_DIR="$(cd "$SERVER_DATA_DIR" && pwd)"
if [[ -z "$RESULTS_DIR" ]]; then
  RESULTS_DIR="$SCRIPT_DIR/results/scale_ladder_$(date -u +%Y%m%dT%H%M%SZ)"
fi
mkdir -p "$RESULTS_DIR"
RESULTS_DIR="$(cd "$RESULTS_DIR" && pwd)"

index_name="scale_ceiling_${PROFILE}"
if [[ "$WORKLOAD" == "bulk_build" ]]; then
  index_name="${index_name}_bulk_build"
fi
RUNGS_JSON="$(printf '%s\n' "${RUNGS[@]}" | jq -s 'map(tonumber)')"
CHECKPOINT_PATH="$RESULTS_DIR/checkpoint.json"
preflight_start_count=0
preflight_target=""

if [[ "$RESUME" -eq 1 ]]; then
  validate_resume_checkpoint
  if [[ "$WORKLOAD" == "bulk_build" ]]; then
    preflight_start_count=0
  else
    preflight_start_count="$RESUME_START_COUNT"
  fi
else
  RESUME_START_COUNT=0
fi
for rung in "${RUNGS[@]}"; do
  if (( rung > preflight_start_count )); then
    preflight_target="$rung"
    break
  fi
done
if [[ -n "$preflight_target" ]]; then
  preflight_rung_dir="$RESULTS_DIR/rung_${preflight_target}"
  mkdir -p "$preflight_rung_dir"
  if ! run_capacity_preflight \
    "$preflight_start_count" \
    "$preflight_target" \
    "$preflight_rung_dir/capacity_preflight.json"; then
    fail "capacity preflight rejected rung ${preflight_target} before server start"
  fi
fi

RUNNER_TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/flapjack_scale_ladder.XXXXXX")"
self_test_portable_dir_size

export FLAPJACK_LOADTEST_BASE_URL="$BASE_URL"
export FLAPJACK_LOADTEST_BENCHMARK_INDEX="$index_name"
export SCALE_PROFILE="$PROFILE"
load_shared_loadtest_config
initialize_loadtest_auth_headers
load_dashboard_seed_settings "$SCRIPT_DIR"

bind_addr="$(derive_bind_addr_from_base_url "$BASE_URL")"
server_log_path="$RESULTS_DIR/server.log"
if [[ "$RESUME" -eq 1 ]]; then
  server_log_path="$RESULTS_DIR/server_resume_$(date -u +%Y%m%dT%H%M%SZ).log"
fi
SERVER_PID="$(
  start_loadtest_server "$SERVER_BINARY" "no-auth" "$bind_addr" "$SERVER_DATA_DIR" \
    "$server_log_path"
)"
wait_for_loadtest_health "$BASE_URL" "$SERVER_PID"

if [[ "$RESUME" -eq 1 ]]; then
  initial_count="$(index_doc_count "$BASE_URL" "$index_name")"
  [[ "$initial_count" -eq "$RESUME_START_COUNT" ]] || {
    fail "resume index count mismatch: checkpoint=${RESUME_START_COUNT}, live=${initial_count}"
  }
  assert_sentinels_top1 "$BASE_URL" "$index_name" "$RESUME_START_COUNT"
else
  reset_loadtest_index "$index_name"
  apply_loadtest_index_settings "$index_name"
  if [[ "$WORKLOAD" == "bulk_build" ]]; then
    seed_scale_bulk_build_destination "$index_name"
  fi
  initial_count="$(index_doc_count "$BASE_URL" "$index_name")"
  expected_initial_count=0
  if [[ "$WORKLOAD" == "bulk_build" ]]; then
    expected_initial_count=1
  fi
  [[ "$initial_count" -eq "$expected_initial_count" ]] ||
    fail "fresh ladder index started with ${initial_count} docs, expected ${expected_initial_count}"
fi

starting_count="$RESUME_START_COUNT"
for rung in "${RUNGS[@]}"; do
  if (( rung <= starting_count )); then
    continue
  fi
  ACTIVE_RUNG="$rung"
  rung_dir="$RESULTS_DIR/rung_${rung}"
  dataset_dir="$RUNNER_TMP_DIR/rung_${rung}_dataset"
  mkdir -p "$rung_dir" "$dataset_dir"
  evidence_starting_count="$starting_count"
  if [[ "$WORKLOAD" == "bulk_build" ]]; then
    evidence_starting_count=0
  fi
  tranche_size=$((rung - evidence_starting_count))
  generated_count=$((tranche_size - SENTINELS_PER_RUNG))
  capacity_receipt="$rung_dir/capacity_preflight.json"

  if [[ ! -s "$capacity_receipt" ]]; then
    run_capacity_preflight "$evidence_starting_count" "$rung" "$capacity_receipt" || {
      fail "capacity preflight rejected rung ${rung} before generation"
    }
  fi
  jq -e '.verdict == "GO"' "$capacity_receipt" >/dev/null || {
    fail "capacity preflight receipt for rung ${rung} is not GO"
  }
  timeout "$GENERATE_TIMEOUT_SECONDS" node "$GENERATOR" \
    --count "$generated_count" \
    --batch-size "$BATCH_SIZE" \
    --profile "$PROFILE" \
    --start-at "$evidence_starting_count" \
    --output-dir "$dataset_dir" \
    > "$rung_dir/generate.stdout.txt" 2>&1
  sentinel_batch="$dataset_dir/batch_000.json"
  create_sentinel_batch "$sentinel_batch" "$rung"

  import_artifact="$rung_dir/import_benchmark.json"
  liveness_sample_path="$rung_dir/liveness_samples.tsv"
  liveness_sampler_pid=""
  count_stall_status=0
  import_wait_status=0
  liveness_status=0
  worker_mode="--import-worker"
  if [[ "$WORKLOAD" == "bulk_build" ]]; then
    worker_mode="--bulk-build-worker"
  fi
  start_scale_build_resource_monitor "$SERVER_PID" "$SERVER_DATA_DIR" \
    "$rung_dir/build_resource_observation.json"
  start_liveness_sampler \
    liveness_sampler_pid \
    "$BASE_URL" \
    "$index_name" \
    "$liveness_sample_path" \
    "$SERVER_PID"
  timeout "$IMPORT_TIMEOUT_SECONDS" bash "$SCRIPT_DIR/scale_ladder.sh" \
    "$worker_mode" "$dataset_dir" "$index_name" "$import_artifact" \
    >"$rung_dir/import.stdout.txt" 2>&1 &
  import_pid=$!

  if [[ "$WORKLOAD" != "bulk_build" ]] &&
    ! wait_for_count_or_stall "$BASE_URL" "$index_name" "$rung" "$STALL_SECONDS"; then
    count_stall_status=1
    kill "$import_pid" 2>/dev/null || true
    wait "$import_pid" 2>/dev/null || true
  elif ! wait "$import_pid"; then
    import_wait_status=$?
  fi
  stop_liveness_sampler liveness_sampler_pid
  stop_scale_build_resource_monitor
  evaluate_rung_liveness "$rung_dir" "$liveness_sample_path" || liveness_status=$?
  if [[ "$count_stall_status" -ne 0 || "$liveness_status" -ne 0 ]]; then
    FAILURE_OUTCOME="LIVENESS_FAILED"
    fail "rung ${rung} liveness gate failed; see ${rung_dir}/liveness_summary.txt"
  fi
  if [[ "$import_wait_status" -ne 0 ]]; then
    FAILURE_OUTCOME="IMPORT_FAILED"
    fail "rung ${rung} import worker failed; see ${rung_dir}/import.stdout.txt"
  fi

  final_count="$(index_doc_count "$BASE_URL" "$index_name")"
  [[ "$final_count" -eq "$rung" ]] || {
    fail "rung ${rung} final count mismatch: expected ${rung}, got ${final_count}"
  }
  jq -e \
    --argjson expected_docs "$tranche_size" \
    '.
    | .errorCount == 0 and
      .totalDocs == $expected_docs and
      .wallClockMs > 0 and
      .latencyWindows != null and
      .latencyWindows.first.count > 0 and
      .latencyWindows.middle.count > 0 and
      .latencyWindows.last.count > 0 and
      (.latencyWindows.lastToFirstP50Ratio | type == "number")' \
    "$import_artifact" >/dev/null || {
      fail "rung ${rung} import artifact is missing exact non-zero evidence"
    }

  wall_clock_ms="$(jq -er '.wallClockMs | select(type == "number" and . > 0)' "$import_artifact")"
  docs_per_second="$(
    awk -v docs="$tranche_size" -v elapsed_ms="$wall_clock_ms" \
      'BEGIN { printf "%.6f", docs * 1000 / elapsed_ms }'
  )"
  awk -v actual="$docs_per_second" -v floor="$MIN_DOCS_PER_SECOND" \
    'BEGIN { exit !(actual > 0 && actual >= floor) }' || {
      fail "rung ${rung} throughput ${docs_per_second} docs/s is below floor ${MIN_DOCS_PER_SECOND}"
    }

  assert_sentinels_top1 "$BASE_URL" "$index_name" "$rung"

  search_results_dir="$rung_dir/search_results"
  mkdir -p "$search_results_dir"
  timeout "$SEARCH_TIMEOUT_SECONDS" env \
    FLAPJACK_LOADTEST_BASE_URL="$BASE_URL" \
    FLAPJACK_LOADTEST_BENCHMARK_INDEX="$index_name" \
    LOADTEST_RESULTS_BASE_DIR="$search_results_dir" \
    SEARCH_BENCHMARK_SAMPLES_PER_TYPE="$SEARCH_SAMPLES_PER_TYPE" \
    bash "$SEARCH_SCRIPT" \
    > "$rung_dir/search_benchmark.stdout.txt" 2>&1
  search_artifact=""
  for candidate in "$search_results_dir"/*/search_benchmark.json; do
    if [[ -f "$candidate" ]]; then
      search_artifact="$candidate"
    fi
  done
  [[ -n "$search_artifact" && -s "$search_artifact" ]] || {
    fail "rung ${rung} search artifact is missing or empty"
  }
  jq -e '
    .queryTypes
    | ["text", "typo", "facet", "filter", "geo"] - keys
    | length == 0
  ' "$search_artifact" >/dev/null || fail "rung ${rung} lacks required per-query-type latency"
  latency_verdict_path="$rung_dir/latency_verdict.json"
  node "$RUNG_VERDICT_HELPER" --search-artifact "$search_artifact" > "$latency_verdict_path" || {
    fail "rung ${rung} latency evidence is invalid"
  }
  latency_verdict="$(jq -er '.verdict | select(. == "PASS" or . == "FAIL")' "$latency_verdict_path")" || {
    fail "rung ${rung} latency verdict is missing or invalid"
  }
  run_purpose="$(current_run_purpose)"

  negative_controls="NOT_RUN"
  if [[ "$EXERCISE_NEGATIVE_CONTROLS" -eq 1 && "$rung" -eq "${RUNGS[${#RUNGS[@]} - 1]}" ]]; then
    exercise_negative_controls "$BASE_URL" "$index_name" "$rung" "$final_count" "$sentinel_batch"
    negative_controls="PASS"
  fi

  index_bytes="$(portable_dir_size_bytes "$SERVER_DATA_DIR")" || {
    fail "rung ${rung} index size is missing, zero, or non-numeric"
  }
  rss_bytes="$(server_rss_bytes "$SERVER_PID")" || {
    fail "rung ${rung} RSS is missing, zero, or non-numeric"
  }
  peak_rss_bytes="$(jq -er '.peakRssBytes' "$rung_dir/build_resource_observation.json")"
  peak_build_disk_bytes="$(
    jq -er '.peakBuildDiskBytes' "$rung_dir/build_resource_observation.json"
  )"
  (( rss_bytes > peak_rss_bytes )) && peak_rss_bytes="$rss_bytes"
  (( index_bytes > peak_build_disk_bytes )) && peak_build_disk_bytes="$index_bytes"
  live_segment_count="$(scale_live_segment_count "$SERVER_DATA_DIR" "$index_name")" || {
    fail "rung ${rung} live segment count is missing or zero"
  }
  capacity_observation_path="$rung_dir/capacity_observation.json"
  capacity_observation_exit=0
  run_capacity_observation "$rung" "$index_bytes" "$rss_bytes" "$capacity_observation_path" \
    || capacity_observation_exit=$?
  capacity_observation_verdict="$(
    jq -er '.verdict | select(. == "PASS" or . == "FAIL" or . == "INVALID")' \
      "$capacity_observation_path" 2>/dev/null
  )" || {
    fail "rung ${rung} capacity observation is missing or unparseable"
  }

  jq -n \
    --arg profile "$PROFILE" \
    --arg workload "$WORKLOAD" \
    --argjson starting_count "$evidence_starting_count" \
    --argjson target_count "$rung" \
    --argjson tranche_size "$tranche_size" \
    --argjson final_count "$final_count" \
    --argjson docs_per_second "$docs_per_second" \
    --argjson import_wall_clock_ms "$wall_clock_ms" \
    --argjson batch_size "$BATCH_SIZE" \
    --argjson index_bytes "$index_bytes" \
    --argjson rss_bytes "$rss_bytes" \
    --argjson peak_rss_bytes "$peak_rss_bytes" \
    --argjson peak_build_disk_bytes "$peak_build_disk_bytes" \
    --argjson live_segment_count "$live_segment_count" \
    --arg negative_controls "$negative_controls" \
    --arg run_purpose "$run_purpose" \
    --arg capacity_observation_verdict "$capacity_observation_verdict" \
    --slurpfile latency "$latency_verdict_path" \
    --slurpfile capacity_observation "$capacity_observation_path" \
    --slurpfile import "$import_artifact" \
    --slurpfile search "$search_artifact" \
    '{
      profile: $profile,
      workload: $workload,
      runPurpose: $run_purpose,
      startingCount: $starting_count,
      targetCount: $target_count,
      trancheSize: $tranche_size,
      finalCount: $final_count,
      docsPerSecond: $docs_per_second,
      importWallClockMs: $import_wall_clock_ms,
      batchSize: $batch_size,
      importLatency: $import[0].latency,
      importLatencyWindows: $import[0].latencyWindows,
      indexBytes: $index_bytes,
      rssBytes: $rss_bytes,
      peakRssBytes: $peak_rss_bytes,
      peakBuildDiskBytes: $peak_build_disk_bytes,
      settledDiskBytes: $index_bytes,
      liveSegmentCount: $live_segment_count,
      sentinels: "PASS",
      negativeControls: $negative_controls,
      rungVerdict: (
        if $capacity_observation_verdict == "PASS"
        then $latency[0].verdict
        else "FAIL"
        end
      ),
      capacityObservation: $capacity_observation[0],
      latency: $latency[0],
      queryTypes: $search[0].queryTypes,
      overallSearch: $search[0].overall
    }' > "$rung_dir/metrics.json"

  remove_completed_dataset "$dataset_dir" "$rung_dir/dataset_cleanup.json"
  starting_count="$final_count"
  if [[ "$capacity_observation_exit" -ne 0 || "$capacity_observation_verdict" != "PASS" ]]; then
    write_run_receipt "CAPACITY_CALIBRATION_FAILED" "$rung" "$rung_dir/metrics.json"
    fail "rung ${rung} exceeded or invalidated the frozen capacity calibration"
  fi
  if [[ "$latency_verdict" == "FAIL" && "$THROUGHPUT_PROBE" -ne 1 ]]; then
    write_run_receipt "CEILING_REACHED" "$rung" "$rung_dir/metrics.json"
    RUN_SUCCEEDED=1
    echo "STOP: rung ${rung} breached the frozen latency contract; evidence saved"
    exit 0
  fi
  if [[ "$latency_verdict" == "PASS" || "$THROUGHPUT_PROBE" -eq 1 ]]; then
    write_checkpoint "$rung" "$rung_dir/metrics.json"
  fi
  if [[ -n "$STOP_AFTER_RUNG" && "$rung" -eq "$STOP_AFTER_RUNG" ]]; then
    [[ "$latency_verdict" == "PASS" || "$THROUGHPUT_PROBE" -eq 1 ]] || {
      fail "--stop-after-rung cannot checkpoint failed latency evidence"
    }
    write_run_receipt "PAUSED"
    RUN_SUCCEEDED=1
    echo "PASS: scale ladder paused after checkpointing ${starting_count} documents"
    exit 0
  fi
done

write_run_receipt "COMPLETED"
RUN_SUCCEEDED=1
echo "PASS: scale ladder completed at ${starting_count} documents"

fi
