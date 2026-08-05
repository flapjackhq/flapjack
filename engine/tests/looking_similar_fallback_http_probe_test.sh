#!/usr/bin/env bash

# False-positive self-test for looking_similar_fallback_http_probe.sh.
#
# Proves the production probe actually detects wrong wire ordering and wrong wire
# scoring, rather than passing because its assertions are vacuous or were never
# reached. Each case injects one wrong expectation, then requires the probe to
# fail at exactly the matching label while every other labelled assertion still
# runs and passes.
#
# This self-test deliberately makes no HTTP assertions of its own -- the wire
# contract has exactly one owner, the production probe. Only ordering and scoring
# are inverted: the unknown-seed, missing-index, and no-usable-terms cases pass on
# absent data, so inverting them could not distinguish a correct `200 + []` from a
# 4xx harness failure.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE="$SCRIPT_DIR/looking_similar_fallback_http_probe.sh"
HTTP_PROBE_LIB="$SCRIPT_DIR/common/http_probe_lib.sh"
EXPECTED_PROBE_CHECKS=8
SUMMARY_ENV_FIXTURE='{"results":[{"hits":[]}]}'

TMP_ROOT=""
TESTS_RUN=0
TESTS_FAILED=0

cleanup() {
  local script_exit_code=$?
  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    if [ "$TESTS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      printf 'INFO: preserved looking-similar probe self-test evidence at %s\n' "$TMP_ROOT" >&2
    else
      rm -rf "$TMP_ROOT"
    fi
  fi
}
trap cleanup EXIT

require_probe() {
  if [ ! -f "$PROBE" ]; then
    printf 'ERROR: production probe not found: %s\n' "$PROBE" >&2
    exit 1
  fi
}

record_check() {
  local label="$1" expected="$2" actual="$3"
  TESTS_RUN=$((TESTS_RUN + 1))
  if [ "$actual" = "$expected" ]; then
    printf '[PASS] %s expected=%s actual=%s\n' "$label" "$expected" "$actual"
  else
    TESTS_FAILED=$((TESTS_FAILED + 1))
    printf '[FAIL] %s expected=%s actual=%s\n' "$label" "$expected" "$actual"
  fi
}

# Reduces one inverted probe run to a comparable fact line. The check count keeps
# a build, start, or tool failure -- which also exits non-zero -- from being
# mistaken for a detected regression.
inversion_facts() {
  local output_path="$1" exit_code="$2"
  local exit_fact receipt_fact failed_labels check_count

  if [ "$exit_code" -ne 0 ]; then
    exit_fact="exit_nonzero"
  else
    exit_fact="exit_zero"
  fi

  if grep -Fqx 'CAPABILITY_VECTOR_SEARCH=false' "$output_path"; then
    receipt_fact="capability_receipt"
  else
    receipt_fact="capability_receipt_missing"
  fi

  failed_labels="$(grep -Eo '^\[FAIL\] [A-Z0-9_]+' "$output_path" \
    | awk '{print $2}' | sort -u | paste -sd, - || true)"
  if [ -z "$failed_labels" ]; then
    failed_labels="none"
  fi

  check_count="$(grep -Ec '^\[(PASS|FAIL)\] ' "$output_path" || true)"

  printf '%s|%s|failed=%s|checks=%s\n' \
    "$exit_fact" "$receipt_fact" "$failed_labels" "$check_count"
}

run_inversion() {
  local case_label="$1" inverted_variable="$2" inverted_value="$3" expected_failed_label="$4"
  local output_path="$TMP_ROOT/${case_label}.log" exit_code=0

  env "${inverted_variable}=${inverted_value}" bash "$PROBE" >"$output_path" 2>&1 || exit_code=$?

  record_check "$case_label" \
    "exit_nonzero|capability_receipt|failed=${expected_failed_label}|checks=${EXPECTED_PROBE_CHECKS}" \
    "$(inversion_facts "$output_path" "$exit_code")"
}

run_env_inheritance_inversion() {
  local output_path="$TMP_ROOT/env_inheritance.log" body_path="$TMP_ROOT/env_inheritance_body.json"
  local exit_code=0

  printf '%s\n' "$SUMMARY_ENV_FIXTURE" >"$body_path"
  env \
    FJ_PROBE_RECOMMEND_SUMMARY_ONLY=empty \
    FJ_PROBE_RECOMMEND_SUMMARY_BODY="$body_path" \
    FJ_PROBE_EXPECTED_TOP_OBJECT_ID=zero_overlap \
    bash "$PROBE" >"$output_path" 2>&1 || exit_code=$?

  record_check SELFTEST_SUMMARY_ENV_NO_BYPASS \
    "exit_nonzero|capability_receipt|failed=R1_ORDERING|checks=${EXPECTED_PROBE_CHECKS}" \
    "$(inversion_facts "$output_path" "$exit_code")"
}

run_summary_contract_case() {
  local case_label="$1" mode="$2" status="$3" body="$4" expected="$5"
  local body_path="$TMP_ROOT/${case_label}.json" output_path="$TMP_ROOT/${case_label}.out"
  local exit_code=0 actual

  printf '%s\n' "$body" >"$body_path"
  bash -c 'source "$1"; recommend_summary "$2" "$3" "$4"' \
    bash "$PROBE" "$mode" "$body_path" "$status" >"$output_path" 2>&1 || exit_code=$?

  if [ "$exit_code" -eq 0 ]; then
    actual="$(tr -d '\n' <"$output_path")"
  else
    actual="exit_${exit_code}:$(tail -1 "$output_path" 2>/dev/null || true)"
  fi

  record_check "$case_label" "$expected" "$actual"
}

run_seed_task_poll_case() {
  local output_path="$TMP_ROOT/settings_task_poll.out" calls_path="$TMP_ROOT/settings_task_poll.calls"
  local fixture_path="$TMP_ROOT/settings_task_poll_fixture.json"
  local exit_code=0 actual

  printf '%s\n' "$SUMMARY_ENV_FIXTURE" >"$fixture_path"
  FJ_PROBE_RECOMMEND_SUMMARY_ONLY=empty \
  FJ_PROBE_RECOMMEND_SUMMARY_BODY="$fixture_path" \
    bash -c '
      set -euo pipefail
      source "$1"
      TMP_ROOT="$2"
      CALLS_PATH="$3"
      BASE="http://127.0.0.1:1"
      INDEX_NAME="mock_index"
      curl_json() {
        local method="$1" url="$2" _body="$3" body_path="$4"
        if [ "$method" = "PUT" ] && [ "${url##*/}" = "settings" ]; then
          printf "{\"taskID\":\"settings-task\"}\n" >"$body_path"
        elif [ "$method" = "POST" ] && [ "${url##*/}" = "batch" ]; then
          printf "{\"taskID\":\"batch-task\"}\n" >"$body_path"
        else
          printf "unexpected request %s %s\n" "$method" "$url" >&2
          return 1
        fi
        printf "200\n"
      }
      wait_for_task() { printf "%s\n" "$1" >>"$CALLS_PATH"; }
      wait_for_fixtures_searchable() { :; }
      seed_fixtures
    ' bash "$PROBE" "$TMP_ROOT" "$calls_path" >"$output_path" 2>&1 || exit_code=$?

  if [ "$exit_code" -eq 0 ]; then
    actual="$(paste -sd, "$calls_path" 2>/dev/null || true)"
  else
    actual="exit_${exit_code}:$(tail -1 "$output_path" 2>/dev/null || true)"
  fi

  record_check SELFTEST_SETTINGS_TASK_POLLED "settings-task,batch-task" "$actual"
}

run_cleanup_preserves_early_failure_case() {
  local evidence_dir="$TMP_ROOT/early_failure_evidence"
  local output_path="$TMP_ROOT/early_failure_cleanup.out"
  local exit_code=0 actual

  mkdir -p "$evidence_dir"
  bash -c '
    set -euo pipefail
    source "$1"
    TMP_ROOT="$2"
    SERVER_PID=""
    TESTS_FAILED=0
    trap '\''script_exit_code=$?; if ! http_probe_cleanup "$script_exit_code" "cleanup regression"; then script_exit_code=1; fi; trap - EXIT; exit "$script_exit_code"'\'' EXIT
    exit 7
  ' bash "$HTTP_PROBE_LIB" "$evidence_dir" >"$output_path" 2>&1 || exit_code=$?

  actual="exit_${exit_code}|removed"
  if [ -d "$evidence_dir" ]; then
    actual="exit_${exit_code}|preserved"
  fi

  record_check SELFTEST_EARLY_FAILURE_EVIDENCE_PRESERVED "exit_7|preserved" "$actual"
}

run_cleanup_bounds_term_ignoring_server_case() {
  local evidence_dir="$TMP_ROOT/term_ignoring_cleanup_evidence"
  local output_path="$TMP_ROOT/term_ignoring_cleanup.out"
  local actual

  mkdir -p "$evidence_dir"
  actual="$(python3 - "$HTTP_PROBE_LIB" "$evidence_dir" <<'PY'
import os
import signal
import subprocess
import sys

probe_lib, evidence_dir = sys.argv[1:]
script = r'''
set -euo pipefail
source "$1"
TMP_ROOT="$2"
TESTS_FAILED=0
python3 -c 'import pathlib, signal, sys; signal.signal(signal.SIGTERM, signal.SIG_IGN); pathlib.Path(sys.argv[1]).write_text("ready"); signal.pause()' "$TMP_ROOT/ready" &
SERVER_PID=$!
for _attempt in $(seq 1 100); do
  if [ -f "$TMP_ROOT/ready" ]; then
    break
  fi
  sleep 0.01
done
if [ ! -f "$TMP_ROOT/ready" ]; then
  exit 8
fi
http_probe_cleanup 0 "bounded cleanup regression"
if kill -0 "$SERVER_PID" 2>/dev/null; then
  exit 9
fi
'''
environment = os.environ.copy()
environment["HTTP_PROBE_STOP_ATTEMPTS"] = "1"
environment["HTTP_PROBE_STOP_INTERVAL_SECONDS"] = "0.05"
process = subprocess.Popen(
    ["bash", "-c", script, "bash", probe_lib, evidence_dir],
    stdout=subprocess.PIPE,
    stderr=subprocess.STDOUT,
    text=True,
    env=environment,
    start_new_session=True,
)
try:
    output, _ = process.communicate(timeout=3)
except subprocess.TimeoutExpired:
    os.killpg(process.pid, signal.SIGKILL)
    output, _ = process.communicate()
    print("deadline_exceeded")
else:
    print(f"completed_{process.returncode}")
sys.stderr.write(output)
PY
  )"

  printf '%s\n' "$actual" >"$output_path"
  record_check SELFTEST_TERM_IGNORING_SERVER_CLEANUP_BOUNDED "completed_0" "$actual"
}

run_cleanup_rejects_injected_stop_config_case() {
  local marker_path="$TMP_ROOT/stop_config_injection_marker"
  local output_path="$TMP_ROOT/stop_config_injection.out"
  local actual

  actual="$(python3 - "$HTTP_PROBE_LIB" "$marker_path" <<'PY'
import os
import signal
import subprocess
import sys

probe_lib, marker_path = sys.argv[1:]
environment = os.environ.copy()
environment["HTTP_PROBE_STOP_ATTEMPTS"] = (
    f'attempt[$(touch "{marker_path}")]+1'
)
environment["HTTP_PROBE_STOP_INTERVAL_SECONDS"] = "0.01"
process = subprocess.Popen(
    [
        "bash",
        "-c",
        'set -u; source "$1"; SERVER_PID=$$; http_probe_wait_for_server_exit',
        "bash",
        probe_lib,
    ],
    stdout=subprocess.PIPE,
    stderr=subprocess.STDOUT,
    text=True,
    env=environment,
    start_new_session=True,
)
try:
    output, _ = process.communicate(timeout=1)
except subprocess.TimeoutExpired:
    os.killpg(process.pid, signal.SIGKILL)
    output, _ = process.communicate()
    completion = "deadline_exceeded"
else:
    completion = "completed_zero" if process.returncode == 0 else "completed_nonzero"
marker = "marker_present" if os.path.exists(marker_path) else "marker_absent"
print(f"{completion}|{marker}")
sys.stderr.write(output)
PY
  )"

  printf '%s\n' "$actual" >"$output_path"
  record_check SELFTEST_STOP_CONFIG_INJECTION_REJECTED \
    "completed_nonzero|marker_absent" "$actual"
}

main() {
  require_probe
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fj-looking-similar-probe-selftest.XXXXXX")"

  run_cleanup_bounds_term_ignoring_server_case
  run_cleanup_rejects_injected_stop_config_case
  run_cleanup_preserves_early_failure_case
  run_summary_contract_case SELFTEST_MISSING_SCORE_MARKER score 200 \
    '{"results":[{"hits":[{"objectID":"a"},{"objectID":"b"}]}]}' \
    '200|malformed:MissingScore'
  run_summary_contract_case SELFTEST_STRING_SCORE_MARKER score 200 \
    '{"results":[{"hits":[{"objectID":"a","_score":100},{"objectID":"b","_score":"99"}]}]}' \
    '200|malformed:InvalidScoreType'
  run_summary_contract_case SELFTEST_BOOLEAN_SCORE_MARKER score 200 \
    '{"results":[{"hits":[{"objectID":"a","_score":100},{"objectID":"b","_score":true}]}]}' \
    '200|malformed:InvalidScoreType'
  run_summary_contract_case SELFTEST_R1_TAIL_EXCLUSION order 200 \
    '{"results":[{"hits":[{"objectID":"strict_winner","_score":100},{"objectID":"five_terms","_score":47},{"objectID":"three_terms","_score":15},{"objectID":"two_terms","_score":7},{"objectID":"one_term","_score":0},{"objectID":"zero_overlap","_score":0}]}]}' \
    '200|strict_winner,five_terms,three_terms,two_terms,one_term|min_hits_ok|no_seed|has_zero_overlap'

  run_seed_task_poll_case
  run_env_inheritance_inversion
  run_inversion SELFTEST_WRONG_TOP_OBJECT_ID \
    FJ_PROBE_EXPECTED_TOP_OBJECT_ID zero_overlap R1_ORDERING
  run_inversion SELFTEST_WRONG_TOP_SCORE \
    FJ_PROBE_EXPECTED_TOP_SCORE 99 R2_SCORE

  if [ "$TESTS_RUN" -ne 11 ]; then
    printf 'ERROR: expected 11 self-test cases, ran %s\n' "$TESTS_RUN" >&2
    exit 1
  fi
  if [ "$TESTS_FAILED" -ne 0 ]; then
    exit 1
  fi
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
