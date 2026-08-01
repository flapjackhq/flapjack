#!/usr/bin/env bash

set -euo pipefail
export LC_ALL=C

SCRIPT_PATH="${BASH_SOURCE[0]}"
if [[ "$SCRIPT_PATH" == */* ]]; then
  SCRIPT_PARENT="${SCRIPT_PATH%/*}"
else
  SCRIPT_PARENT="."
fi
SCRIPT_DIR="$(cd "$SCRIPT_PARENT" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_FOR_FLAPJACK="$ENGINE_DIR/tests/common/wait_for_flapjack.sh"

ADMIN_KEY="resource-bounds-http-probe-admin-key"
EXPECTED_TIMEOUT_BODY='{"message":"Request timed out","status":408}'
EXPECTED_PANIC_BODY='{"message":"Internal server error","status":500}'
FAULT_SLEEP_MARKER="FLAPJACK_FAULT_SLEEP_STARTED"
FAULT_SLEEP_SECONDS=2
FAULT_SLEEP_MIN_SECONDS=0.5

BIN=""
TMP_ROOT=""
SERVER_PID=""
BASE=""
SERVER_LOG=""
CHECKS_RUN=0
CHECKS_FAILED=0
INDETERMINATE=0

target_dir() {
  if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    printf '%s\n' "$ENGINE_DIR/target"
  elif [ "${CARGO_TARGET_DIR#/}" != "$CARGO_TARGET_DIR" ]; then
    printf '%s\n' "$CARGO_TARGET_DIR"
  else
    printf '%s\n' "$ENGINE_DIR/$CARGO_TARGET_DIR"
  fi
}

mark_indeterminate() {
  INDETERMINATE=1
  printf 'INDETERMINATE %s expected=%s actual=%s\n' "$1" "$2" "$3" >&2
}

record_result() {
  local status="$1" surface="$2" check="$3" expected="$4" actual="$5"
  CHECKS_RUN=$((CHECKS_RUN + 1))
  if [ "$status" = "PASS" ]; then
    printf '[PASS] %s %s expected=%s actual=%s\n' "$surface" "$check" "$expected" "$actual"
  else
    CHECKS_FAILED=$((CHECKS_FAILED + 1))
    printf '[FAIL] %s %s expected=%s actual=%s\n' "$surface" "$check" "$expected" "$actual"
  fi
}

record_equals() {
  local surface="$1" check="$2" expected="$3" actual="$4"
  if [ "$actual" = "$expected" ]; then
    record_result PASS "$surface" "$check" "$expected" "$actual"
  else
    record_result FAIL "$surface" "$check" "$expected" "$actual"
  fi
}

file_contents() {
  local path="$1"
  if [ -f "$path" ]; then
    cat "$path"
  fi
}

stop_server() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
  BASE=""
  SERVER_LOG=""
}

cleanup() {
  local script_exit_code=$?
  stop_server
  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    if [ "$CHECKS_FAILED" -gt 0 ] || [ "$INDETERMINATE" -ne 0 ] || [ "$script_exit_code" -ne 0 ]; then
      printf 'INFO: preserved resource-bounds evidence at %s\n' "$TMP_ROOT" >&2
    else
      rm -rf "$TMP_ROOT"
    fi
  fi
}
trap cleanup EXIT

required_tools() {
  local missing=0 tool
  for tool in awk cargo cat curl env grep head mkdir mktemp rm sed seq sleep tail; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'INDETERMINATE required_tool expected=present actual=missing:%s\n' "$tool" >&2
      missing=1
    fi
  done
  if [ "$missing" -ne 0 ]; then
    INDETERMINATE=1
    exit 1
  fi
  if [ ! -x "$WAIT_FOR_FLAPJACK" ]; then
    mark_indeterminate wait_helper executable "$WAIT_FOR_FLAPJACK"
    exit 1
  fi
}

build_binary() {
  local label="$1"
  shift
  local build_log="$TMP_ROOT/build_${label}.log"
  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server "$@" >"$build_log" 2>&1); then
    tail -40 "$build_log" >&2 || true
    mark_indeterminate "cargo_build_${label}" success failed
    exit 1
  fi
  BIN="$(target_dir)/debug/flapjack"
  if [ ! -x "$BIN" ]; then
    mark_indeterminate "binary_${label}" executable "$BIN"
    exit 1
  fi
}

start_server() {
  local label="$1"
  shift
  local data_dir="$TMP_ROOT/${label}_data"
  local log_path="$TMP_ROOT/${label}_server.log"
  mkdir -p "$data_dir"

  env \
    -u FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND \
    -u FLAPJACK_BIND_ADDR \
    -u FLAPJACK_CONTENT_SECURITY_POLICY \
    -u FLAPJACK_DISABLE_DASHBOARD \
    -u FLAPJACK_ENV \
    -u FLAPJACK_LOG_FORMAT \
    -u FLAPJACK_NO_AUTH \
    -u FLAPJACK_NODE_ID \
    -u FLAPJACK_PEERS \
    -u FLAPJACK_PORT \
    "$@" \
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$data_dir" \
    "$BIN" --auto-port >"$log_path" 2>&1 &
  SERVER_PID=$!

  if ! "$WAIT_FOR_FLAPJACK" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$log_path" \
    --retries 80 \
    --interval-seconds 0.5; then
    mark_indeterminate "${label}_readiness" "healthy loopback auto-port server" failed
    exit 1
  fi
  if [ ! -s "$log_path" ]; then
    mark_indeterminate "${label}_server_log" non_empty empty
    exit 1
  fi

  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9][0-9]*\).*/\1/p' "$log_path" | head -1)"
  if [ -z "$port" ]; then
    mark_indeterminate "${label}_auto_port" "Local: http://127.0.0.1:<port>" missing
    exit 1
  fi
  BASE="http://127.0.0.1:${port}"
  SERVER_LOG="$log_path"
}

wait_for_log_marker() {
  local log_path="$1" marker="$2" retries=40
  for _i in $(seq 1 "$retries"); do
    if grep -Fq "$marker" "$log_path"; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

response_followed_fault_release() {
  local fault_started_clock="$1" response_clock="$2" sleep_seconds="$3"
  awk \
    -v fault_started_clock="$fault_started_clock" \
    -v response_clock="$response_clock" \
    -v sleep_seconds="$sleep_seconds" '
      function valid_clock(clock, fields, count) {
        count = split(clock, fields, ":")
        return count == 3 \
          && fields[1] ~ /^[0-9][0-9]$/ && fields[1] <= 23 \
          && fields[2] ~ /^[0-9][0-9]$/ && fields[2] <= 59 \
          && fields[3] ~ /^[0-9][0-9]\.[0-9]+$/ && fields[3] < 60
      }
      function clock_seconds(clock, fields) {
        split(clock, fields, ":")
        return fields[1] * 3600 + fields[2] * 60 + fields[3]
      }
      BEGIN {
        if (!valid_clock(fault_started_clock) || !valid_clock(response_clock)) exit 1
        fault_started = clock_seconds(fault_started_clock)
        fault_release = fault_started + sleep_seconds
        response = clock_seconds(response_clock)
        if (response < fault_started) response += 86400
        exit !(response >= fault_release)
      }
    '
}

record_health_release_order() {
  local log_path="$1" trace_path="$2" marker_timestamp fault_started_clock response_clock actual_timing
  marker_timestamp="$(awk -v marker="$FAULT_SLEEP_MARKER" 'index($0, marker) { print $1; exit }' "$log_path")"
  fault_started_clock="${marker_timestamp#*T}"
  fault_started_clock="${fault_started_clock%Z}"
  response_clock="$(awk '/<= Recv header/ { print $1; exit }' "$trace_path")"
  actual_timing="fault_started=${fault_started_clock:-missing}_health_responded=${response_clock:-missing}"

  if [ -n "$marker_timestamp" ] && [ -n "$response_clock" ] \
    && response_followed_fault_release "$fault_started_clock" "$response_clock" "$FAULT_SLEEP_SECONDS"; then
    record_result PASS concurrency slot_release_order "health_response_after_sleep_release" "$actual_timing"
  else
    record_result FAIL concurrency slot_release_order "health_response_after_sleep_release" "$actual_timing"
  fi
}

curl_capture() {
  local path="$1" body="$2" header="$3" meta="$4" exit_file="$5"
  local elapsed_file="${6:-}" write_out='%{http_code}'
  local trace_path="${elapsed_file:-$exit_file}.trace"
  if [ -n "$elapsed_file" ]; then
    write_out='%{http_code} %{time_total}'
  fi
  set +e
  local result
  result="$(TZ=UTC curl -sS -D "$header" -o "$body" -w "$write_out" \
    --trace-ascii "$trace_path" --trace-time \
    -H "x-algolia-application-id: resource-bounds-probe" \
    -H "x-algolia-api-key: ${ADMIN_KEY}" \
    "$BASE$path")"
  local curl_exit=$?
  set -e
  printf '%s\n' "${result%% *}" >"$meta"
  if [ -n "$elapsed_file" ]; then
    printf '%s\n' "${result#* }" >"$elapsed_file"
  fi
  printf '%s\n' "$curl_exit" >"$exit_file"
}

check_timeout_contract() {
  local log_path body header meta exit_file elapsed_file status elapsed
  start_server timeout FLAPJACK_REQUEST_TIMEOUT_SECS=1
  log_path="$SERVER_LOG"
  body="$TMP_ROOT/timeout.body"
  header="$TMP_ROOT/timeout.headers"
  meta="$TMP_ROOT/timeout.status"
  exit_file="$TMP_ROOT/timeout.exit"
  elapsed_file="$TMP_ROOT/timeout.elapsed"

  curl_capture "/internal/fault/sleep" "$body" "$header" "$meta" "$exit_file" "$elapsed_file"
  elapsed="$(file_contents "$elapsed_file")"

  status="$(<"$meta")"
  record_equals timeout curl_exit 0 "$(file_contents "$exit_file")"
  record_equals timeout http_status 408 "$status"
  record_equals timeout body "$EXPECTED_TIMEOUT_BODY" "$(file_contents "$body")"
  if awk -v elapsed="$elapsed" -v bound="$FAULT_SLEEP_MIN_SECONDS" 'BEGIN { exit !(elapsed > bound) }'; then
    record_result PASS timeout bounded_elapsed "greater_than_${FAULT_SLEEP_MIN_SECONDS}s" "${elapsed}s"
  else
    record_result FAIL timeout bounded_elapsed "greater_than_${FAULT_SLEEP_MIN_SECONDS}s" "${elapsed}s"
  fi
  if awk -v elapsed="$elapsed" -v bound="$FAULT_SLEEP_SECONDS" 'BEGIN { exit !(elapsed < bound) }'; then
    record_result PASS timeout bounded_elapsed "less_than_${FAULT_SLEEP_SECONDS}s" "${elapsed}s"
  else
    record_result FAIL timeout bounded_elapsed "less_than_${FAULT_SLEEP_SECONDS}s" "${elapsed}s"
  fi
  stop_server
}

check_concurrency_contract() {
  local log_path sleep_body sleep_headers sleep_meta sleep_exit
  local health_body health_headers health_meta health_exit health_elapsed_file health_elapsed
  start_server concurrency FLAPJACK_MAX_CONCURRENT_REQUESTS=1 FLAPJACK_REQUEST_TIMEOUT_SECS=5
  log_path="$SERVER_LOG"
  sleep_body="$TMP_ROOT/concurrency_sleep.body"
  sleep_headers="$TMP_ROOT/concurrency_sleep.headers"
  sleep_meta="$TMP_ROOT/concurrency_sleep.status"
  sleep_exit="$TMP_ROOT/concurrency_sleep.exit"
  health_body="$TMP_ROOT/concurrency_health.body"
  health_headers="$TMP_ROOT/concurrency_health.headers"
  health_meta="$TMP_ROOT/concurrency_health.status"
  health_exit="$TMP_ROOT/concurrency_health.exit"
  health_elapsed_file="$TMP_ROOT/concurrency_health.elapsed"

  curl_capture "/internal/fault/sleep" "$sleep_body" "$sleep_headers" "$sleep_meta" "$sleep_exit" &
  local sleep_pid=$!
  if ! wait_for_log_marker "$log_path" "$FAULT_SLEEP_MARKER"; then
    mark_indeterminate concurrency_sleep_marker "$FAULT_SLEEP_MARKER" missing
    wait "$sleep_pid" || true
    stop_server
    exit 1
  fi

  curl_capture "/health" "$health_body" "$health_headers" "$health_meta" "$health_exit" "$health_elapsed_file" &
  local health_pid=$!
  sleep 0.3
  if kill -0 "$health_pid" 2>/dev/null; then
    record_result PASS concurrency health_pending "pending while sleep owns slot" pending
  else
    record_result FAIL concurrency health_pending "pending while sleep owns slot" completed
  fi

  wait "$sleep_pid"
  wait "$health_pid"
  record_health_release_order "$log_path" "${health_elapsed_file}.trace"
  health_elapsed="$(file_contents "$health_elapsed_file")"
  if awk -v elapsed="$health_elapsed" -v bound="$FAULT_SLEEP_MIN_SECONDS" 'BEGIN { exit !(elapsed > bound) }'; then
    record_result PASS concurrency bounded_health_elapsed "greater_than_${FAULT_SLEEP_MIN_SECONDS}s" "${health_elapsed}s"
  else
    record_result FAIL concurrency bounded_health_elapsed "greater_than_${FAULT_SLEEP_MIN_SECONDS}s" "${health_elapsed}s"
  fi
  record_equals concurrency sleep_curl_exit 0 "$(file_contents "$sleep_exit")"
  record_equals concurrency sleep_http_status 200 "$(file_contents "$sleep_meta")"
  record_equals concurrency health_curl_exit 0 "$(file_contents "$health_exit")"
  record_equals concurrency health_http_status 200 "$(file_contents "$health_meta")"
  stop_server
}

check_panic_contract() {
  local body header meta exit_file
  start_server panic FLAPJACK_REQUEST_TIMEOUT_SECS=5 >/dev/null
  body="$TMP_ROOT/panic.body"
  header="$TMP_ROOT/panic.headers"
  meta="$TMP_ROOT/panic.status"
  exit_file="$TMP_ROOT/panic.exit"

  curl_capture "/internal/fault/panic" "$body" "$header" "$meta" "$exit_file"
  record_equals panic curl_exit 0 "$(file_contents "$exit_file")"
  record_equals panic http_status 500 "$(file_contents "$meta")"
  record_equals panic body "$EXPECTED_PANIC_BODY" "$(file_contents "$body")"

  curl_capture "/health" "$TMP_ROOT/panic_health.body" "$TMP_ROOT/panic_health.headers" "$TMP_ROOT/panic_health.status" "$TMP_ROOT/panic_health.exit"
  record_equals panic subsequent_health_status 200 "$(file_contents "$TMP_ROOT/panic_health.status")"
  stop_server
}

check_release_shape_contract() {
  local body header meta exit_file
  build_binary default
  start_server release_shape >/dev/null
  for route in sleep panic; do
    body="$TMP_ROOT/release_${route}.body"
    header="$TMP_ROOT/release_${route}.headers"
    meta="$TMP_ROOT/release_${route}.status"
    exit_file="$TMP_ROOT/release_${route}.exit"
    curl_capture "/internal/fault/${route}" "$body" "$header" "$meta" "$exit_file"
    record_equals release_shape "${route}_curl_exit" 0 "$(file_contents "$exit_file")"
    record_equals release_shape "${route}_http_status" 404 "$(file_contents "$meta")"
  done
  stop_server
}

main() {
  required_tools
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/flapjack-resource-bounds.XXXXXX")"

  build_binary fault --features flapjack-http/fault-injection
  check_timeout_contract
  check_concurrency_contract
  check_panic_contract
  check_release_shape_contract

  printf 'SUMMARY checks_run=%s checks_failed=%s indeterminate=%s\n' "$CHECKS_RUN" "$CHECKS_FAILED" "$INDETERMINATE"
  if [ "$INDETERMINATE" -ne 0 ] || [ "$CHECKS_FAILED" -ne 0 ]; then
    exit 1
  fi
}

main "$@"
