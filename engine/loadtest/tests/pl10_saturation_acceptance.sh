#!/usr/bin/env bash
set -euo pipefail

LOADTEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE_DIR="$(cd "$LOADTEST_DIR/.." && pwd)"
REPO_DIR="$(cd "$ENGINE_DIR/.." && pwd)"
LOADTEST_HELPERS="$LOADTEST_DIR/lib/loadtest_shell_helpers.sh"
SOAK_HELPERS="$LOADTEST_DIR/lib/loadtest_soak_helpers.sh"
SEED_SCRIPT="$LOADTEST_DIR/seed-loadtest-data.sh"
MIXED_SOAK_SCENARIO="scenarios/mixed-soak.js"
SERVER_BINARY="$ENGINE_DIR/target/release/flapjack"
RESULTS_BASE_DIR="$LOADTEST_DIR/results"

TARGET_SUCCESSFUL_WRITES=45000
TARGET_WRITE_VUS=200
TARGET_SOAK_DURATION="2m"
TUNED_WRITE_QUEUE_BATCH_SIZE=64
SCENARIO_TIMEOUT_SECONDS=420
MIN_TUNED_BASELINE_UPLIFT_RATIO=1.50
VERDICT_TUNABLE_VERIFIED="TUNABLE_VERIFIED"
VERDICT_TUNABLE_INSUFFICIENT="PARTIAL_TUNABLE_INSUFFICIENT"

RESULTS_DIR=""
RUNNER_TMP_DIR=""
SERVER_PID=""
FLAPJACK_BIND_ADDR=""
LAST_CASE_SUMMARY_PATH=""
BASELINE_SUMMARY_PATH=""
TUNED_SUMMARY_PATH=""
COMPARISON_UPLIFT_RATIO=""
COMPARISON_VERDICT=""

fail() {
  echo "FAIL: $1"
  exit 1
}

cleanup() {
  stop_loadtest_server "${SERVER_PID:-}" >/dev/null 2>&1 || true
}

trap cleanup EXIT

require_file() {
  local path="$1"
  [[ -f "$path" ]] || fail "missing file: $path"
}

choose_loopback_base_url() {
  local free_port
  free_port="$(
    python3 - <<'PY'
import socket
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.bind(("127.0.0.1", 0))
print(sock.getsockname()[1])
sock.close()
PY
  )"
  [[ -n "$free_port" ]] || fail "unable to allocate an ephemeral loopback port"
  printf 'http://127.0.0.1:%s' "$free_port"
}

parse_metric_row() {
  local metric_name="$1"
  local stdout_path="$2"
  local metric_line

  metric_line="$(grep -E "^[[:space:]]*${metric_name}\\.{2,}:[[:space:]]+[0-9]+\\.[0-9]+%[[:space:]]+[0-9]+[[:space:]]+out of[[:space:]]+[0-9]+" "$stdout_path" | tail -n 1)"
  [[ -n "$metric_line" ]] || fail "missing metric row for ${metric_name} in $stdout_path"

  sed -E 's/^.*:[[:space:]]*([0-9]+\.[0-9]+)%[[:space:]]+([0-9]+)[[:space:]]+out of[[:space:]]+([0-9]+).*/\1\t\2\t\3/' <<<"$metric_line"
}

parse_write_failure_row() {
  local stdout_path="$1"
  local write_row

  write_row="$(grep -E '^[[:space:]]*\{ type:write \}\.{2,}:[[:space:]]+[0-9]+\.[0-9]+%[[:space:]]+[0-9]+[[:space:]]+out of[[:space:]]+[0-9]+' "$stdout_path" | tail -n 1)"
  [[ -n "$write_row" ]] || fail "missing write failure row in $stdout_path"

  sed -E 's/^.*:[[:space:]]*([0-9]+\.[0-9]+)%[[:space:]]+([0-9]+)[[:space:]]+out of[[:space:]]+([0-9]+).*/\1\t\2\t\3/' <<<"$write_row"
}

parse_write_check_rate() {
  local stdout_path="$1"
  local check_name="$2"
  local rate_line
  local rate

  rate_line="$(awk -v check_name="$check_name" '
    $0 ~ check_name {
      if (getline > 0) {
        print $0
        exit
      }
    }
  ' "$stdout_path")"
  rate="$(sed -E 's/.*rate=([0-9]+\.[0-9]+)%.*/\1/' <<<"$rate_line")"

  [[ -n "$rate" ]] || fail "missing check rate for ${check_name} in $stdout_path"
  printf '%s' "$rate"
}

derive_successful_writes() {
  local total_writes="$1"
  local write_200_rate="$2"
  awk -v total="$total_writes" -v rate="$write_200_rate" 'BEGIN { printf "%.0f", (total * rate / 100.0) }'
}

compute_uplift_ratio() {
  local baseline_successful_writes="$1"
  local tuned_successful_writes="$2"
  awk -v baseline="$baseline_successful_writes" -v tuned="$tuned_successful_writes" 'BEGIN {
    if (baseline <= 0) {
      print "0.0000"
    } else {
      printf "%.4f", tuned / baseline
    }
  }'
}

assert_floats_near_equal() {
  local actual="$1"
  local expected="$2"
  local tolerance="$3"
  local error_message="$4"
  awk -v actual="$actual" -v expected="$expected" -v tol="$tolerance" 'BEGIN {
    diff = actual - expected
    if (diff < 0) diff = -diff
    exit !(diff <= tol)
  }' || fail "$error_message"
}

assert_comparison_verdict_passes() {
  [[ "$COMPARISON_VERDICT" == "$VERDICT_TUNABLE_VERIFIED" ]] || fail "tuned/baseline successful_writes uplift ratio ${COMPARISON_UPLIFT_RATIO} is below required ${MIN_TUNED_BASELINE_UPLIFT_RATIO} (${VERDICT_TUNABLE_INSUFFICIENT})"
}

run_with_timeout() {
  local timeout_seconds="$1"
  shift

  "$@" &
  local cmd_pid=$!
  local elapsed=0

  while kill -0 "$cmd_pid" >/dev/null 2>&1; do
    if (( elapsed >= timeout_seconds )); then
      kill "$cmd_pid" >/dev/null 2>&1 || true
      sleep 1
      kill -9 "$cmd_pid" >/dev/null 2>&1 || true
      wait "$cmd_pid" >/dev/null 2>&1 || true
      return 124
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done

  wait "$cmd_pid"
}

write_run_commands_and_head() {
  local output_path="$1"
  {
    echo "head_sha=$(git -C "$REPO_DIR" rev-parse HEAD)"
    echo "timestamp_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "baseline_run=FLAPJACK_LOADTEST_BASE_URL=${FLAPJACK_BASE_URL} FLAPJACK_LOADTEST_WRITE_VUS=${TARGET_WRITE_VUS} FLAPJACK_LOADTEST_SOAK_DURATION=${TARGET_SOAK_DURATION} k6 run ${MIXED_SOAK_SCENARIO}"
    echo "tuned_run=FLAPJACK_LOADTEST_BASE_URL=${FLAPJACK_BASE_URL} FLAPJACK_LOADTEST_WRITE_VUS=${TARGET_WRITE_VUS} FLAPJACK_LOADTEST_SOAK_DURATION=${TARGET_SOAK_DURATION} FLAPJACK_WRITE_QUEUE_BATCH_SIZE=${TUNED_WRITE_QUEUE_BATCH_SIZE} k6 run ${MIXED_SOAK_SCENARIO}"
  } >"$output_path"
}

run_mixed_soak_with_artifacts() {
  local json_path="$1"
  local stdout_path="$2"

  FLAPJACK_LOADTEST_BASE_URL="$FLAPJACK_BASE_URL" \
  FLAPJACK_LOADTEST_WRITE_VUS="$TARGET_WRITE_VUS" \
  FLAPJACK_LOADTEST_SOAK_DURATION="$TARGET_SOAK_DURATION" \
    run_loadtest_scenario_with_artifacts "$LOADTEST_DIR" "mixed-soak" "" "$json_path" "$stdout_path"
}

write_case_summary_json() {
  local case_name="$1"
  local case_dir="$2"
  local stdout_path="$3"
  local k6_exit_code="$4"

  local write_failure_row
  local write_5xx_row
  local write_unexpected_4xx_row
  local write_200_rate
  local write_taskid_rate
  local write_objectids_rate
  local failure_pct
  local failed_writes
  local total_writes
  local write_http_5xx_rate
  local write_http_5xx_count
  local write_http_unexpected_4xx_rate
  local write_http_unexpected_4xx_count
  local successful_writes
  local saturation_target_pass=false
  local contract_pass=false
  local summary_path="$case_dir/summary.json"

  write_failure_row="$(parse_write_failure_row "$stdout_path")"
  write_5xx_row="$(parse_metric_row "write_http_5xx_rate" "$stdout_path")"
  write_unexpected_4xx_row="$(parse_metric_row "write_http_unexpected_4xx_rate" "$stdout_path")"
  write_200_rate="$(parse_write_check_rate "$stdout_path" 'checks\{check:write returns 200,type:write\}')"
  write_taskid_rate="$(parse_write_check_rate "$stdout_path" 'checks\{check:write returns numeric taskID,type:write\}')"
  write_objectids_rate="$(parse_write_check_rate "$stdout_path" 'checks\{check:write returns objectIDs array,type:write\}')"

  IFS=$'\t' read -r failure_pct failed_writes total_writes <<<"$write_failure_row"
  IFS=$'\t' read -r write_http_5xx_rate write_http_5xx_count _ <<<"$write_5xx_row"
  IFS=$'\t' read -r write_http_unexpected_4xx_rate write_http_unexpected_4xx_count _ <<<"$write_unexpected_4xx_row"

  successful_writes="$(derive_successful_writes "$total_writes" "$write_200_rate")"
  if (( successful_writes >= TARGET_SUCCESSFUL_WRITES )); then
    saturation_target_pass=true
  fi
  if [[ "$write_http_5xx_rate" == "0.00" && "$write_http_unexpected_4xx_rate" == "0.00" ]]; then
    contract_pass=true
  fi

  cat >"$summary_path" <<JSON
{
  "case_name": "${case_name}",
  "k6_exit_code": ${k6_exit_code},
  "write_vus": ${TARGET_WRITE_VUS},
  "soak_duration": "${TARGET_SOAK_DURATION}",
  "successful_writes": ${successful_writes},
  "target_successful_writes": ${TARGET_SUCCESSFUL_WRITES},
  "saturation_target_pass": ${saturation_target_pass},
  "failure_pct": ${failure_pct},
  "failed_writes": ${failed_writes},
  "total_writes": ${total_writes},
  "write_http_5xx_rate_pct": ${write_http_5xx_rate},
  "write_http_5xx_count": ${write_http_5xx_count},
  "write_http_unexpected_4xx_rate_pct": ${write_http_unexpected_4xx_rate},
  "write_http_unexpected_4xx_count": ${write_http_unexpected_4xx_count},
  "write_response_checks_success_rate_pct": {
    "write_returns_200": ${write_200_rate},
    "write_returns_numeric_taskid": ${write_taskid_rate},
    "write_returns_objectids_array": ${write_objectids_rate}
  },
  "contract_pass": ${contract_pass}
}
JSON

  LAST_CASE_SUMMARY_PATH="$summary_path"
}

start_case_server() {
  local case_name="$1"
  local batch_size_override="$2"
  local case_dir="$3"
  local case_data_dir="$RUNNER_TMP_DIR/${case_name}_server_data"
  local case_log_path="$case_dir/server.log"

  mkdir -p "$case_data_dir"
  if [[ -n "$batch_size_override" ]]; then
    SERVER_PID="$(start_loadtest_server "$SERVER_BINARY" "no-auth" "$FLAPJACK_BIND_ADDR" "$case_data_dir" "$case_log_path" "" "FLAPJACK_WRITE_QUEUE_BATCH_SIZE=$batch_size_override")"
  else
    SERVER_PID="$(start_loadtest_server "$SERVER_BINARY" "no-auth" "$FLAPJACK_BIND_ADDR" "$case_data_dir" "$case_log_path")"
  fi

  wait_for_loadtest_health "$FLAPJACK_BASE_URL" "$SERVER_PID" "300" "0.1"
}

run_case() {
  local case_name="$1"
  local batch_size_override="$2"
  local case_dir="$RESULTS_DIR/$case_name"
  local stdout_path="$case_dir/mixed-soak.stdout.txt"
  local scenario_exit_code=0

  mkdir -p "$case_dir"
  start_case_server "$case_name" "$batch_size_override" "$case_dir"
  FLAPJACK_LOADTEST_BASE_URL="$FLAPJACK_BASE_URL" bash "$SEED_SCRIPT" >"$case_dir/seed.stdout.txt" 2>"$case_dir/seed.stderr.txt"

  if ! run_with_timeout "$SCENARIO_TIMEOUT_SECONDS" run_mixed_soak_with_artifacts "" "$stdout_path"; then
    scenario_exit_code=$?
  fi

  stop_loadtest_server "$SERVER_PID" >/dev/null 2>&1 || true
  SERVER_PID=""

  if [[ "$scenario_exit_code" -eq 124 ]]; then
    fail "${case_name} scenario timed out after ${SCENARIO_TIMEOUT_SECONDS}s"
  fi
  if [[ "$scenario_exit_code" -ne 0 && "$scenario_exit_code" -ne 99 ]]; then
    fail "${case_name} scenario failed with hard error (k6 exit code ${scenario_exit_code})"
  fi

  write_case_summary_json "$case_name" "$case_dir" "$stdout_path" "$scenario_exit_code"
}

assert_contract_preserved() {
  local summary_path="$1"
  local write_5xx
  local write_unexpected_4xx

  write_5xx="$(jq -r '.write_http_5xx_rate_pct' "$summary_path")"
  write_unexpected_4xx="$(jq -r '.write_http_unexpected_4xx_rate_pct' "$summary_path")"

  [[ "$write_5xx" == "0" || "$write_5xx" == "0.00" ]] || fail "contract breach: write_http_5xx_rate must be 0.00, got $write_5xx"
  [[ "$write_unexpected_4xx" == "0" || "$write_unexpected_4xx" == "0.00" ]] || fail "contract breach: write_http_unexpected_4xx_rate must be 0.00, got $write_unexpected_4xx"
}

compute_stage3_comparison() {
  local baseline_summary="$1"
  local tuned_summary="$2"
  local baseline_successful_writes
  local tuned_successful_writes

  baseline_successful_writes="$(jq -r '.successful_writes' "$baseline_summary")"
  tuned_successful_writes="$(jq -r '.successful_writes' "$tuned_summary")"

  assert_contract_preserved "$baseline_summary"
  assert_contract_preserved "$tuned_summary"

  COMPARISON_UPLIFT_RATIO="$(compute_uplift_ratio "$baseline_successful_writes" "$tuned_successful_writes")"
  COMPARISON_VERDICT="$VERDICT_TUNABLE_INSUFFICIENT"
  if awk -v ratio="$COMPARISON_UPLIFT_RATIO" -v min_ratio="$MIN_TUNED_BASELINE_UPLIFT_RATIO" 'BEGIN { exit !(ratio >= min_ratio) }'; then
    COMPARISON_VERDICT="$VERDICT_TUNABLE_VERIFIED"
  fi
}

evaluate_and_persist_stage3_comparison() {
  local baseline_summary="$1"
  local tuned_summary="$2"

  compute_stage3_comparison "$baseline_summary" "$tuned_summary"
  write_combined_summary "$baseline_summary" "$tuned_summary"
  write_combined_tsv "$baseline_summary" "$tuned_summary"
  assert_comparison_verdict_passes
}

write_combined_summary() {
  local baseline_summary="$1"
  local tuned_summary="$2"
  local combined_path="$RESULTS_DIR/pl10_saturation_summary.json"

  jq -s \
    --arg results_dir "$RESULTS_DIR" \
    --arg scenario "$MIXED_SOAK_SCENARIO" \
    --arg soak_duration "$TARGET_SOAK_DURATION" \
    --argjson write_vus "$TARGET_WRITE_VUS" \
    --argjson target_successful_writes "$TARGET_SUCCESSFUL_WRITES" \
    --argjson tuned_batch_size "$TUNED_WRITE_QUEUE_BATCH_SIZE" \
    --argjson min_uplift_ratio "$MIN_TUNED_BASELINE_UPLIFT_RATIO" \
    --arg verdict "$COMPARISON_VERDICT" \
    --argjson uplift_ratio "$COMPARISON_UPLIFT_RATIO" \
    '{
      scenario: $scenario,
      write_vus: $write_vus,
      soak_duration: $soak_duration,
      target_successful_writes: $target_successful_writes,
      tuned_write_queue_batch_size: $tuned_batch_size,
      results_dir: $results_dir,
      baseline: .[0],
      tuned: .[1],
      comparison: {
        baseline_successful_writes: .[0].successful_writes,
        tuned_successful_writes: .[1].successful_writes,
        uplift_ratio: $uplift_ratio,
        min_uplift_ratio: $min_uplift_ratio,
        verdict: $verdict
      }
    }' "$baseline_summary" "$tuned_summary" >"$combined_path"
}

write_combined_tsv() {
  local baseline_summary="$1"
  local tuned_summary="$2"
  local tsv_path="$RESULTS_DIR/pl10_saturation_summary.tsv"

  {
    echo -e "case_name\tk6_exit_code\tsuccessful_writes\ttarget_successful_writes\tsaturation_target_pass\twrite_http_5xx_rate_pct\twrite_http_unexpected_4xx_rate_pct\twrite_returns_200_rate_pct"
    jq -r '[.case_name, .k6_exit_code, .successful_writes, .target_successful_writes, .saturation_target_pass, .write_http_5xx_rate_pct, .write_http_unexpected_4xx_rate_pct, .write_response_checks_success_rate_pct.write_returns_200] | @tsv' "$baseline_summary"
    jq -r '[.case_name, .k6_exit_code, .successful_writes, .target_successful_writes, .saturation_target_pass, .write_http_5xx_rate_pct, .write_http_unexpected_4xx_rate_pct, .write_response_checks_success_rate_pct.write_returns_200] | @tsv' "$tuned_summary"
  } >"$tsv_path"
}

assert_selftest_summary_matches_contract() {
  local summary_path="$1"
  local verdict
  local min_uplift_ratio
  local uplift_ratio
  local baseline_successful_writes
  local tuned_successful_writes
  local expected_uplift_ratio

  verdict="$(jq -r '.comparison.verdict' "$summary_path")"
  min_uplift_ratio="$(jq -r '.comparison.min_uplift_ratio' "$summary_path")"
  uplift_ratio="$(jq -r '.comparison.uplift_ratio' "$summary_path")"
  baseline_successful_writes="$(jq -r '.comparison.baseline_successful_writes' "$summary_path")"
  tuned_successful_writes="$(jq -r '.comparison.tuned_successful_writes' "$summary_path")"
  expected_uplift_ratio="$(compute_uplift_ratio "$baseline_successful_writes" "$tuned_successful_writes")"

  [[ "$verdict" == "$VERDICT_TUNABLE_VERIFIED" ]] || fail "selftest expected verdict ${VERDICT_TUNABLE_VERIFIED}, got ${verdict}"
  assert_floats_near_equal "$min_uplift_ratio" "$MIN_TUNED_BASELINE_UPLIFT_RATIO" "0" "selftest expected min_uplift_ratio ${MIN_TUNED_BASELINE_UPLIFT_RATIO}, got ${min_uplift_ratio}"
  assert_floats_near_equal "$uplift_ratio" "$expected_uplift_ratio" "0.0001" "selftest expected uplift_ratio ${expected_uplift_ratio}, got ${uplift_ratio}"
}

assert_selftest_fixture_discriminates_non_ratio_rules() {
  local summary_path="$1"
  local baseline_successful_writes
  local tuned_successful_writes
  local tuned_minus_baseline

  baseline_successful_writes="$(jq -r '.comparison.baseline_successful_writes' "$summary_path")"
  tuned_successful_writes="$(jq -r '.comparison.tuned_successful_writes' "$summary_path")"
  tuned_minus_baseline=$((tuned_successful_writes - baseline_successful_writes))

  (( tuned_successful_writes < 75000 )) || fail "selftest pass fixture must stay below absolute tuned-write gate guardrail (75000), got ${tuned_successful_writes}"
  (( tuned_minus_baseline < 25000 )) || fail "selftest pass fixture must stay below absolute delta gate guardrail (25000), got ${tuned_minus_baseline}"
}

assert_selftest_pass_fixture_persists_combined_tsv() {
  local tsv_path="$1"
  local expected_header
  local expected_baseline_row
  local expected_tuned_row
  local actual_header
  local actual_baseline_row
  local actual_tuned_row
  local line_count

  expected_header=$'case_name\tk6_exit_code\tsuccessful_writes\ttarget_successful_writes\tsaturation_target_pass\twrite_http_5xx_rate_pct\twrite_http_unexpected_4xx_rate_pct\twrite_returns_200_rate_pct'
  expected_baseline_row=$'baseline_unset\t\t20000\t\ttrue\t0\t0\t'
  expected_tuned_row=$'tuned_batch64\t\t30000\t\ttrue\t0\t0\t'

  [[ -f "$tsv_path" ]] || fail "selftest expected combined TSV artifact at ${tsv_path}"

  line_count="$(awk 'END { print NR }' "$tsv_path")"
  [[ "$line_count" == "3" ]] || fail "selftest expected combined TSV to contain exactly 3 lines, got ${line_count}"

  actual_header="$(sed -n '1p' "$tsv_path")"
  actual_baseline_row="$(sed -n '2p' "$tsv_path")"
  actual_tuned_row="$(sed -n '3p' "$tsv_path")"

  [[ "$actual_header" == "$expected_header" ]] || fail "selftest expected combined TSV header '${expected_header}', got '${actual_header}'"
  [[ "$actual_baseline_row" == "$expected_baseline_row" ]] || fail "selftest expected baseline TSV row '${expected_baseline_row}', got '${actual_baseline_row}'"
  [[ "$actual_tuned_row" == "$expected_tuned_row" ]] || fail "selftest expected tuned TSV row '${expected_tuned_row}', got '${actual_tuned_row}'"
}

assert_selftest_pass_fixture_rejects_malformed_tsv() {
  local tsv_assertion_path="$1"
  local malformed_tsv_path="$2"

  cp "$tsv_assertion_path" "$malformed_tsv_path"
  echo -e "case_name\tsuccessful_writes" >"$tsv_assertion_path"

  if (assert_selftest_pass_fixture_persists_combined_tsv "$tsv_assertion_path" >/dev/null 2>&1); then
    fail "selftest expected pass-fixture TSV assertion to reject malformed combined TSV output"
  fi

  mv "$malformed_tsv_path" "$tsv_assertion_path"
}

assert_selftest_below_threshold_rejected() {
  local baseline_summary="$1"
  local tuned_summary="$2"
  local summary_path="$3"
  local baseline_successful_writes
  local tuned_successful_writes
  local expected_uplift_ratio
  local comparison_output
  local persisted_baseline
  local persisted_tuned
  local persisted_uplift_ratio
  local persisted_verdict
  local persisted_min_uplift_ratio

  baseline_successful_writes="$(jq -r '.successful_writes' "$baseline_summary")"
  tuned_successful_writes="$(jq -r '.successful_writes' "$tuned_summary")"
  expected_uplift_ratio="$(compute_uplift_ratio "$baseline_successful_writes" "$tuned_successful_writes")"

  # Remove any prior summary at this path so the freshness assertions below
  # can only pass when evaluate_and_persist_stage3_comparison rewrote the
  # combined summary for THIS fixture. Without this, a stale file left by an
  # earlier passing or failing fixture would satisfy a plain `-f` check even
  # if the summary-write path were silently skipped on failure.
  rm -f "$summary_path"

  if comparison_output="$(evaluate_and_persist_stage3_comparison "$baseline_summary" "$tuned_summary" 2>&1)"; then
    fail "selftest expected below-threshold fixture to fail and keep verdict ${VERDICT_TUNABLE_INSUFFICIENT}"
  fi
  [[ -f "$summary_path" ]] || fail "selftest expected below-threshold summary artifact at ${summary_path}"

  persisted_baseline="$(jq -r '.comparison.baseline_successful_writes' "$summary_path")"
  persisted_tuned="$(jq -r '.comparison.tuned_successful_writes' "$summary_path")"
  persisted_uplift_ratio="$(jq -r '.comparison.uplift_ratio' "$summary_path")"
  persisted_verdict="$(jq -r '.comparison.verdict' "$summary_path")"
  persisted_min_uplift_ratio="$(jq -r '.comparison.min_uplift_ratio' "$summary_path")"

  [[ "$persisted_baseline" == "$baseline_successful_writes" ]] || fail "selftest expected persisted baseline_successful_writes ${baseline_successful_writes}, got ${persisted_baseline}"
  [[ "$persisted_tuned" == "$tuned_successful_writes" ]] || fail "selftest expected persisted tuned_successful_writes ${tuned_successful_writes}, got ${persisted_tuned}"
  assert_floats_near_equal "$persisted_uplift_ratio" "$expected_uplift_ratio" "0.0001" "selftest expected persisted uplift_ratio ${expected_uplift_ratio}, got ${persisted_uplift_ratio}"
  [[ "$persisted_verdict" == "$VERDICT_TUNABLE_INSUFFICIENT" ]] || fail "selftest expected persisted verdict ${VERDICT_TUNABLE_INSUFFICIENT}, got ${persisted_verdict}"
  assert_floats_near_equal "$persisted_min_uplift_ratio" "$MIN_TUNED_BASELINE_UPLIFT_RATIO" "0" "selftest expected persisted min_uplift_ratio ${MIN_TUNED_BASELINE_UPLIFT_RATIO}, got ${persisted_min_uplift_ratio}"

  grep -F "$expected_uplift_ratio" <<<"$comparison_output" >/dev/null || fail "selftest expected failure output to include uplift ratio ${expected_uplift_ratio}"
  grep -F "$MIN_TUNED_BASELINE_UPLIFT_RATIO" <<<"$comparison_output" >/dev/null || fail "selftest expected failure output to include minimum uplift ratio ${MIN_TUNED_BASELINE_UPLIFT_RATIO}"
  grep -F "$VERDICT_TUNABLE_INSUFFICIENT" <<<"$comparison_output" >/dev/null || fail "selftest expected failure output to include verdict ${VERDICT_TUNABLE_INSUFFICIENT}"
}

run_selftest_stage3_comparison() {
  local selftest_dir
  local baseline_summary
  local tuned_summary
  local low_baseline_summary
  local low_tuned_summary
  local high_baseline_summary
  local high_tuned_summary

  selftest_dir="$(mktemp -d "${TMPDIR:-/tmp}/pl10-stage3-selftest.XXXXXX")"
  baseline_summary="$selftest_dir/baseline.json"
  tuned_summary="$selftest_dir/tuned.json"
  low_baseline_summary="$selftest_dir/low-baseline.json"
  low_tuned_summary="$selftest_dir/low-tuned.json"
  high_baseline_summary="$selftest_dir/high-baseline.json"
  high_tuned_summary="$selftest_dir/high-tuned.json"

  cat >"$baseline_summary" <<'JSON'
{
  "case_name": "baseline_unset",
  "successful_writes": 20000,
  "saturation_target_pass": true,
  "write_http_5xx_rate_pct": 0,
  "write_http_unexpected_4xx_rate_pct": 0
}
JSON
  cat >"$tuned_summary" <<'JSON'
{
  "case_name": "tuned_batch64",
  "successful_writes": 30000,
  "saturation_target_pass": true,
  "write_http_5xx_rate_pct": 0,
  "write_http_unexpected_4xx_rate_pct": 0
}
JSON
  cat >"$low_baseline_summary" <<'JSON'
{
  "case_name": "baseline_unset_low_case",
  "successful_writes": 50000,
  "saturation_target_pass": true,
  "write_http_5xx_rate_pct": 0,
  "write_http_unexpected_4xx_rate_pct": 0
}
JSON
  cat >"$low_tuned_summary" <<'JSON'
{
  "case_name": "tuned_batch64_low_case",
  "successful_writes": 70000,
  "saturation_target_pass": true,
  "write_http_5xx_rate_pct": 0,
  "write_http_unexpected_4xx_rate_pct": 0
}
JSON
  cat >"$high_baseline_summary" <<'JSON'
{
  "case_name": "baseline_unset_high_case",
  "successful_writes": 200000,
  "saturation_target_pass": true,
  "write_http_5xx_rate_pct": 0,
  "write_http_unexpected_4xx_rate_pct": 0
}
JSON
  cat >"$high_tuned_summary" <<'JSON'
{
  "case_name": "tuned_batch64_high_case",
  "successful_writes": 230000,
  "saturation_target_pass": true,
  "write_http_5xx_rate_pct": 0,
  "write_http_unexpected_4xx_rate_pct": 0
}
JSON

  RESULTS_DIR="$selftest_dir"
  evaluate_and_persist_stage3_comparison "$baseline_summary" "$tuned_summary"
  assert_selftest_summary_matches_contract "$selftest_dir/pl10_saturation_summary.json"
  assert_selftest_fixture_discriminates_non_ratio_rules "$selftest_dir/pl10_saturation_summary.json"
  assert_selftest_pass_fixture_persists_combined_tsv "$selftest_dir/pl10_saturation_summary.tsv"
  assert_selftest_pass_fixture_rejects_malformed_tsv "$selftest_dir/pl10_saturation_summary.tsv" "$selftest_dir/pl10_saturation_summary.tsv.bak"
  assert_selftest_below_threshold_rejected "$low_baseline_summary" "$low_tuned_summary" "$selftest_dir/pl10_saturation_summary.json"
  assert_selftest_below_threshold_rejected "$high_baseline_summary" "$high_tuned_summary" "$selftest_dir/pl10_saturation_summary.json"

  rm -rf "$selftest_dir"
}

main() {
  if [[ "${PL10_ACCEPTANCE_SELFTEST:-0}" == "1" ]]; then
    run_selftest_stage3_comparison
    echo "PASS: stage3 comparison selftest"
    return
  fi

  require_file "$LOADTEST_HELPERS"
  require_file "$SOAK_HELPERS"
  require_file "$SEED_SCRIPT"

  # shellcheck source=engine/loadtest/lib/loadtest_shell_helpers.sh
  source "$LOADTEST_HELPERS"
  # shellcheck source=engine/loadtest/lib/loadtest_soak_helpers.sh
  source "$SOAK_HELPERS"

  require_loadtest_commands cargo curl jq k6 node python3
  load_shared_loadtest_config
  initialize_loadtest_auth_headers

  FLAPJACK_BASE_URL="$(choose_loopback_base_url)"
  FLAPJACK_BIND_ADDR="$(derive_bind_addr_from_base_url "$FLAPJACK_BASE_URL")"
  (
    cd "$ENGINE_DIR"
    cargo build --release -p flapjack-server
  )

  RUNNER_TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/flapjack-pl10-saturation.XXXXXX")"
  RESULTS_DIR="$(create_loadtest_results_dir "$RESULTS_BASE_DIR" "pl10-saturation-acceptance")"
  write_run_commands_and_head "$RESULTS_DIR/run_commands_and_head.txt"

  run_case "baseline_unset" ""
  BASELINE_SUMMARY_PATH="$LAST_CASE_SUMMARY_PATH"
  run_case "tuned_batch64" "$TUNED_WRITE_QUEUE_BATCH_SIZE"
  TUNED_SUMMARY_PATH="$LAST_CASE_SUMMARY_PATH"

  evaluate_and_persist_stage3_comparison "$BASELINE_SUMMARY_PATH" "$TUNED_SUMMARY_PATH"

  echo "PASS: PL-10 saturation acceptance"
  echo "INFO: results written to $RESULTS_DIR"
}

main "$@"
