#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2016,SC2030,SC2031,SC2034,SC2129,SC2329
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOAK_SCRIPT="$ROOT_DIR/soak_proof.sh"
LOADTEST_HELPERS="$ROOT_DIR/lib/loadtest_shell_helpers.sh"

fail() {
  echo "FAIL: $1"
  exit 1
}

[[ -f "$SOAK_SCRIPT" ]] || fail "missing $SOAK_SCRIPT"
[[ -f "$LOADTEST_HELPERS" ]] || fail "missing $LOADTEST_HELPERS"
grep -q 'scenario_requires_analytics_proof' "$SOAK_SCRIPT" || \
  fail "soak_proof.sh must gate analytics proof to analytics-bearing scenarios"
grep -q 'if scenario_requires_analytics_proof; then' "$SOAK_SCRIPT" || \
  fail "write-soak must not require analytics proof capture"
grep -Fq 'FLAPJACK_BIND_ADDR="$(derive_bind_addr_from_base_url "$FLAPJACK_BASE_URL")"' "$SOAK_SCRIPT" || \
  fail "soak_proof.sh must call the shared bind-address helper with the configured base URL"
grep -q 'assert_restart_preserved_value' "$SOAK_SCRIPT" || \
  fail "soak_proof.sh must define the restart preservation assertion it calls"
grep -q 'start_liveness_sampler' "$SOAK_SCRIPT" || \
  fail "write-soak must start a dedicated health/count liveness sampler"
grep -Fq 'liveness_distribution "$LIVENESS_SAMPLE_PATH" 250 5000' "$SOAK_SCRIPT" || \
  fail "write-soak must use the shared liveness_distribution verdict owner"
grep -Fq '/1/usage/documents_count/${encoded_index_name}' "$LOADTEST_HELPERS" || \
  fail "shared liveness helper must sample the live usage count endpoint"

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/soak_proof_write_acceptance.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

run_admission_sampler_fixture() {
  local data_dir="$WORK_DIR/admission_sampler/data"
  local admission_dir="$data_dir/loadtest_write/write_admission"
  local created_at_ms
  local observed

  mkdir -p "$admission_dir"
  created_at_ms="$(node -e 'process.stdout.write(String(Date.now() - 1200))')"
  cat >"$admission_dir/00000000000000000001.json" <<EOF
{"checksum":"fixture","record":{"sequence":1,"task_id":"1","numeric_id":1,"received_documents":1,"created_at_ms":${created_at_ms},"actions":[]}}
EOF

  observed="$(
    export FLAPJACK_SOAK_PROOF_SKIP_MAIN=1
    # shellcheck source=../soak_proof.sh
    source "$SOAK_SCRIPT"
    SERVER_DATA_DIR="$data_dir"
    FLAPJACK_WRITE_INDEX="loadtest_write"
    sample_write_admission_state
  )"

  [[ "$observed" =~ ^1,[0-9]+,ok$ ]] || \
    fail "expected envelope admission sample to parse, got: $observed"
}

run_admission_sampler_ignores_vanished_records_fixture() {
  local data_dir="$WORK_DIR/admission_sampler_enoent/data"
  local admission_dir="$data_dir/loadtest_write/write_admission"
  local observed

  mkdir -p "$admission_dir"
  ln -s "$admission_dir/removed-before-read.json" "$admission_dir/00000000000000000001.json"

  observed="$(
    export FLAPJACK_SOAK_PROOF_SKIP_MAIN=1
    # shellcheck source=../soak_proof.sh
    source "$SOAK_SCRIPT"
    SERVER_DATA_DIR="$data_dir"
    FLAPJACK_WRITE_INDEX="loadtest_write"
    sample_write_admission_state
  )"

  [[ "$observed" == "0,,empty" ]] || \
    fail "expected vanished admission record to be ignored as a drain race, got: $observed"
}

run_admission_sampler_fixture
run_admission_sampler_ignores_vanished_records_fixture

write_consistency_env() {
  local results_dir="$1"
  cat >"$results_dir/consistency.env" <<'EOF'
post_soak_read_doc_count=1000
post_soak_write_doc_count=6
post_soak_macbook_hits=12
post_soak_write_index_hits=6
post_restart_read_doc_count=1000
post_restart_write_doc_count=6
post_restart_macbook_hits=12
post_restart_write_index_hits=6
EOF
}

write_samples() {
  local sample_path="$1"
  local max_age="$2"
  local sample_status="${3:-ok}"
  cat >"$sample_path" <<EOF
timestamp_utc,rss_kb,heap_bytes,pressure_level,admission_record_count,oldest_admission_age_ms,admission_sample_status
2026-07-22T00:00:00Z,1000,2000,0,2,1000,ok
2026-07-22T00:00:01Z,1200,2100,0,4,${max_age},${sample_status}
2026-07-22T00:00:02Z,1100,2050,0,0,,empty
EOF
}

write_invalid_samples() {
  local sample_path="$1"
  cat >"$sample_path" <<'EOF'
timestamp_utc,rss_kb,heap_bytes,pressure_level,admission_record_count,oldest_admission_age_ms,admission_sample_status
2026-07-22T00:00:00Z,1000,2000,0,2,,invalid:bad_record
EOF
}

write_liveness_samples() {
  local sample_path="$1"
  local mode="${2:-valid}"
  local sample_number

  : >"$sample_path"
  case "$mode" in
    valid)
      for sample_number in $(seq 1 100); do
        printf 'health\tok\t%s\ncount\tok\t%s\n' \
          "$((sample_number % 25 + 1))" "$((sample_number % 40 + 1))" >>"$sample_path"
      done
      ;;
    timeout)
      for _ in $(seq 1 100); do
        printf 'health\tok\t10\ncount\tok\t20\n' >>"$sample_path"
      done
      printf 'health\ttimeout\t5000\n' >>"$sample_path"
      ;;
    missing_count)
      for _ in $(seq 1 100); do
        printf 'health\tok\t10\n' >>"$sample_path"
      done
      ;;
    insufficient)
      for _ in $(seq 1 99); do
        printf 'health\tok\t10\ncount\tok\t20\n' >>"$sample_path"
      done
      ;;
    empty)
      ;;
    *)
      fail "unknown liveness fixture mode: $mode"
      ;;
  esac
}

write_k6_json() {
  local output_path="$1"
  local attempts="$2"
  local accepted="$3"
  local queue_full="$4"
  local unexpected_4xx="$5"
  local server_5xx="$6"
  local dropped="$7"
  local duration_values="$8"
  local raw_path="${output_path%.gz}"

  : >"$raw_path"
  for _ in $(seq 1 "$attempts"); do
    printf '%s\n' '{"type":"Point","metric":"http_reqs","data":{"value":1,"tags":{"type":"write"}}}' >>"$raw_path"
  done
  printf '{"type":"Point","metric":"write_http_accepted_200_count","data":{"value":%s,"tags":{"type":"write"}}}\n' "$accepted" >>"$raw_path"
  printf '{"type":"Point","metric":"write_http_queue_full_429_count","data":{"value":%s,"tags":{"type":"write"}}}\n' "$queue_full" >>"$raw_path"
  printf '{"type":"Point","metric":"write_http_unexpected_4xx_rate","data":{"value":%s,"tags":{"type":"write"}}}\n' "$unexpected_4xx" >>"$raw_path"
  printf '{"type":"Point","metric":"write_http_5xx_rate","data":{"value":%s,"tags":{"type":"write"}}}\n' "$server_5xx" >>"$raw_path"
  printf '{"type":"Point","metric":"dropped_iterations","data":{"value":%s,"tags":{"type":"write"}}}\n' "$dropped" >>"$raw_path"
  IFS=, read -ra durations <<<"$duration_values"
  for duration in "${durations[@]}"; do
    printf '{"type":"Point","metric":"http_req_duration","data":{"value":%s,"tags":{"type":"write","status":"200"}}}\n' "$duration" >>"$raw_path"
  done
  gzip -c "$raw_path" >"$output_path"
}

run_fixture() {
  local name="$1"
  local condition="$2"
  local attempts="$3"
  local accepted="$4"
  local queue_full="$5"
  local unexpected_4xx="$6"
  local server_5xx="$7"
  local dropped="$8"
  local max_age="$9"
  local drain_seconds="${10}"
  local drain_count="${11}"
  local control_summary="${12:-}"
  local sample_mode="${13:-valid}"
  local liveness_mode="${14:-valid}"
  local results_dir="$WORK_DIR/$name"

  mkdir -p "$results_dir"
  write_consistency_env "$results_dir"
  write_k6_json "$results_dir/write-soak.json.gz" "$attempts" "$accepted" "$queue_full" \
    "$unexpected_4xx" "$server_5xx" "$dropped" "100,200,300,400,500,600"
  if [[ "$sample_mode" == "invalid" ]]; then
    write_invalid_samples "$results_dir/memory_samples.csv"
  else
    write_samples "$results_dir/memory_samples.csv" "$max_age"
  fi
  write_liveness_samples "$results_dir/liveness_samples.tsv" "$liveness_mode"

  (
    export FLAPJACK_SOAK_PROOF_SKIP_MAIN=1
    # shellcheck source=../soak_proof.sh
    source "$SOAK_SCRIPT"
    RESULTS_DIR="$results_dir"
    SAMPLE_PATH="$results_dir/memory_samples.csv"
    LIVENESS_SAMPLE_PATH="$results_dir/liveness_samples.tsv"
    SUMMARY_PATH="$results_dir/summary.md"
    K6_JSON_PATH="$results_dir/write-soak.json.gz"
    K6_STDOUT_PATH="$results_dir/stdout.txt"
    SERVER_LOG_PATH="$results_dir/server.log"
    SERVER_BINARY="/tmp/flapjack"
    SCENARIO_NAME="write-soak"
    SCENARIO_EXIT_CODE=0
    FLAPJACK_SOAK_DURATION="10s"
    FLAPJACK_BASE_URL="http://127.0.0.1:7700"
    FLAPJACK_BIND_ADDR="127.0.0.1:7700"
    K6_API_ADDR="127.0.0.1:17700"
    RETENTION_GATE_PROBE_PARTITION_PATH="N/A"
    FLAPJACK_LOADTEST_WRITE_TARGET_RPS=1
    FLAPJACK_LOADTEST_WRITE_EXPECTED_ATTEMPTS=10
    FLAPJACK_LOADTEST_WRITE_CONDITION="$condition"
    if [[ "$condition" == "control" ]]; then
      FLAPJACK_LOADTEST_WRITE_ACCEPTED_FLOOR=5
    fi
    if [[ -n "$control_summary" ]]; then
      FLAPJACK_LOADTEST_WRITE_CONTROL_SUMMARY="$control_summary"
    fi
    WRITE_ADMISSION_DRAIN_DURATION_SECONDS="$drain_seconds"
    WRITE_ADMISSION_DRAIN_RECORD_COUNT="$drain_count"
    write_summary
  )
}

require_summary_value() {
  local summary_path="$1"
  local label="$2"
  local expected="$3"
  if ! grep -Fq -- "- ${label}: \`${expected}\`" "$summary_path"; then
    grep -F -- "- ${label}:" "$summary_path" || true
    fail "expected ${label}=${expected} in $summary_path"
  fi
}

require_summary_line() {
  local summary_path="$1"
  local expected="$2"
  grep -Fq -- "$expected" "$summary_path" || fail "expected line in $summary_path: $expected"
}

control_summary="$WORK_DIR/control/summary.md"
run_fixture "control" "control" 10 6 4 0 0 0 2500 12 0
require_summary_value "$control_summary" "write attempted requests" "10"
require_summary_value "$control_summary" "write accepted 200 count" "6"
require_summary_value "$control_summary" "write QueueFull 429 count" "4"
require_summary_value "$control_summary" "write dirty-error count" "0"
require_summary_value "$control_summary" "write accepted 200 p95 ms" "600"
require_summary_value "$control_summary" "write admission peak record count" "4"
require_summary_value "$control_summary" "write admission max oldest age ms" "2500"
require_summary_value "$control_summary" "write admission drain duration seconds" "12"
require_summary_line "$control_summary" '- write RSS KB diagnostics: start=`1000`, peak=`1200`, end=`1100`'
require_summary_line "$control_summary" '- write heap bytes diagnostics: start=`2000`, peak=`2100`, end=`2050`'
require_summary_value "$control_summary" "write overall verdict" "PASS"
[[ -s "$WORK_DIR/control/liveness_samples.tsv" ]] || \
  fail "write-soak must preserve a non-empty liveness TSV"
[[ "$(awk -F '\t' '$1 == "health" { count += 1 } END { print count + 0 }' "$WORK_DIR/control/liveness_samples.tsv")" == "100" ]] || \
  fail "health fixture must retain 100 determinate samples"
[[ "$(awk -F '\t' '$1 == "count" { count += 1 } END { print count + 0 }' "$WORK_DIR/control/liveness_samples.tsv")" == "100" ]] || \
  fail "count fixture must retain 100 determinate samples"
require_summary_line "$control_summary" '- write health liveness: `endpoint=health samples=100'
require_summary_line "$control_summary" '- write count liveness: `endpoint=count samples=100'
require_summary_value "$control_summary" "write liveness verdict" "PASS"

candidate_summary="$WORK_DIR/candidate/summary.md"
run_fixture "candidate" "candidate" 10 6 4 0 0 0 2500 12 0 "$control_summary"
require_summary_value "$candidate_summary" "write overall verdict" "CONFIRMED_BOUNDED_LAG"

if run_fixture "dropped" "candidate" 10 6 4 0 0 1 2500 12 0 "$control_summary"; then
  fail "dropped-iteration fixture must fail"
fi
require_summary_value "$WORK_DIR/dropped/summary.md" "write dropped-iterations verdict" "FAIL"

if run_fixture "age_boundary" "candidate" 10 6 4 0 0 0 30000 12 0 "$control_summary"; then
  fail "30,000ms admission-age boundary fixture must fail"
fi
require_summary_value "$WORK_DIR/age_boundary/summary.md" "write admission age verdict" "FAIL"

if run_fixture "dirty" "candidate" 10 5 4 1 0 0 2500 12 0 "$control_summary"; then
  fail "dirty-error fixture must fail"
fi
require_summary_value "$WORK_DIR/dirty/summary.md" "write dirty-error verdict" "FAIL"

if run_fixture "invalid_sample" "candidate" 10 6 4 0 0 0 2500 12 0 "$control_summary" "invalid"; then
  fail "invalid sample fixture must fail"
fi
require_summary_value "$WORK_DIR/invalid_sample/summary.md" "write admission sample verdict" "FAIL (bad_admission_sample)"

run_fixture "no_429" "candidate" 10 10 0 0 0 0 2500 12 0 "$control_summary"
require_summary_value "$WORK_DIR/no_429/summary.md" "write overall verdict" "INCONCLUSIVE_LOAD_NOT_SATURATING"

if run_fixture "regressed" "candidate" 10 5 5 0 0 0 2500 12 0 "$control_summary"; then
  fail "candidate regression fixture must fail"
fi
require_summary_value "$WORK_DIR/regressed/summary.md" "write candidate acceptance verdict" "FAIL (control=6)"

if run_fixture "liveness_timeout" "candidate" 10 6 4 0 0 0 2500 12 0 "$control_summary" "valid" "timeout"; then
  fail "liveness timeout fixture must fail the overall verdict"
fi
grep -Fq $'health\ttimeout\t5000' "$WORK_DIR/liveness_timeout/liveness_samples.tsv" || \
  fail "timed-out probes must remain explicit timeout rows in the TSV"
require_summary_value "$WORK_DIR/liveness_timeout/summary.md" "write liveness verdict" "FAIL"
require_summary_value "$WORK_DIR/liveness_timeout/summary.md" "write overall verdict" "FALSIFIED_UNBOUNDED_OR_REGRESSED"

if run_fixture "liveness_empty" "candidate" 10 6 4 0 0 0 2500 12 0 "$control_summary" "valid" "empty"; then
  fail "empty liveness TSV must fail the overall verdict"
fi
require_summary_value "$WORK_DIR/liveness_empty/summary.md" "write liveness verdict" "FAIL"

if run_fixture "liveness_missing_count" "candidate" 10 6 4 0 0 0 2500 12 0 "$control_summary" "valid" "missing_count"; then
  fail "missing count denominator must fail the overall verdict"
fi
require_summary_value "$WORK_DIR/liveness_missing_count/summary.md" "write liveness verdict" "FAIL"

if run_fixture "liveness_insufficient" "candidate" 10 6 4 0 0 0 2500 12 0 "$control_summary" "valid" "insufficient"; then
  fail "fewer than 100 samples per endpoint must fail the overall verdict"
fi
require_summary_value "$WORK_DIR/liveness_insufficient/summary.md" "write liveness verdict" "FAIL"

(
  # shellcheck source=../lib/loadtest_shell_helpers.sh
  source "$LOADTEST_HELPERS"
  LOADTEST_AUTH_HEADERS=()
  curl() {
    printf '200\t0.042'
  }
  [[ "$(sample_liveness_endpoint health http://127.0.0.1/health)" == $'health\tok\t42' ]] || \
    fail "successful probes must emit endpoint, ok status, and millisecond latency"
  curl() {
    printf '000\t5.000'
    return 28
  }
  [[ "$(sample_liveness_endpoint health http://127.0.0.1/health)" == $'health\ttimeout\t5000' ]] || \
    fail "timed-out probes must emit an explicit timeout row"
)

echo "PASS: soak proof write acceptance checks"
