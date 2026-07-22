#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_FOR_FLAPJACK="$ENGINE_DIR/tests/common/wait_for_flapjack.sh"

BIN=""
TMP_ROOT=""
SERVER_PID=""
BASE=""
LAST_LOG_PATH=""
TESTS_RUN=0
TESTS_FAILED=0
NO_AUTH_PUBLIC_BIND_WARNING='WARNING: FLAPJACK_NO_AUTH is enabled on a non-loopback or hostname bind address because FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND=1; this exposes unauthenticated Flapjack APIs publicly.'
NO_AUTH_PUBLIC_BIND_ERROR='ERROR: FLAPJACK_NO_AUTH cannot be used with non-loopback bind address 0.0.0.0:0 unless FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND=1 is set.'
NO_AUTH_HOSTNAME_BIND_ERROR='ERROR: FLAPJACK_NO_AUTH cannot be used with hostname bind address localhost:0 unless FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND=1 is set.'

usage() {
  cat <<'EOF'
Usage:
  dashboard_auth_hardening_acceptance.sh dashboard
  dashboard_auth_hardening_acceptance.sh no_auth
EOF
}

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  printf '  [PASS] %s\n' "$1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  printf '  [FAIL] %s\n' "$1" >&2
  if [ -n "${2:-}" ]; then
    printf '    %s\n' "$2" >&2
  fi
}

cleanup() {
  local script_exit_code=$?
  stop_server

  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    if [ "$TESTS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      printf 'INFO: preserved dashboard auth hardening evidence at %s\n' "$TMP_ROOT" >&2
    else
      rm -rf "$TMP_ROOT"
    fi
  fi
}
trap cleanup EXIT

require_tools() {
  local missing=0 tool
  for tool in cargo curl grep mktemp sed tr; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  if [ "$missing" -ne 0 ]; then
    exit 1
  fi
  if [ ! -x "$WAIT_FOR_FLAPJACK" ]; then
    printf 'ERROR: wait helper is not executable: %s\n' "$WAIT_FOR_FLAPJACK" >&2
    exit 1
  fi
}

target_dir() {
  if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    printf '%s\n' "$ENGINE_DIR/target"
  elif [ "${CARGO_TARGET_DIR#/}" != "$CARGO_TARGET_DIR" ]; then
    printf '%s\n' "$CARGO_TARGET_DIR"
  else
    printf '%s\n' "$ENGINE_DIR/$CARGO_TARGET_DIR"
  fi
}

build_current_checkout_binary() {
  local build_log="$TMP_ROOT/build.log"
  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$build_log" 2>&1); then
    tail -30 "$build_log" >&2 || true
    echo 'ERROR: cargo build -p flapjack-server failed' >&2
    exit 1
  fi

  BIN="$(target_dir)/debug/flapjack"
  if [ ! -x "$BIN" ]; then
    printf 'ERROR: expected current-checkout binary at %s\n' "$BIN" >&2
    exit 1
  fi
}

stop_server() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
  BASE=""
  LAST_LOG_PATH=""
}

start_server() {
  local mode="$1"
  local data_dir="$TMP_ROOT/${mode}_data"
  local log_path="$data_dir/server.log"
  mkdir -p "$data_dir"

  stop_server

  case "$mode" in
    locked)
      env \
        FLAPJACK_ADMIN_KEY="dashboard-auth-hardening-admin-key" \
        FLAPJACK_DATA_DIR="$data_dir" \
        FLAPJACK_DISABLE_DASHBOARD=1 \
        "$BIN" --auto-port >"$log_path" 2>&1 &
      ;;
    default)
      env -u FLAPJACK_DISABLE_DASHBOARD \
        FLAPJACK_ADMIN_KEY="dashboard-auth-hardening-admin-key" \
        FLAPJACK_DATA_DIR="$data_dir" \
        "$BIN" --auto-port >"$log_path" 2>&1 &
      ;;
    *)
      printf 'ERROR: unknown server mode: %s\n' "$mode" >&2
      exit 1
      ;;
  esac
  SERVER_PID=$!

  "$WAIT_FOR_FLAPJACK" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$log_path" \
    --retries 80 \
    --interval-seconds 0.5

  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$log_path" | head -1)"
  if [ -z "$port" ]; then
    echo 'ERROR: server became healthy but no auto-port was found in startup log' >&2
    cat "$log_path" >&2 || true
    exit 1
  fi
  BASE="http://127.0.0.1:${port}"
}

write_node_config() {
  local data_dir="$1" bind_addr="$2"
  cat >"$data_dir/node.json" <<EOF
{"node_id":"no-auth-acceptance","bind_addr":"${bind_addr}","peers":[]}
EOF
  test -s "$data_dir/node.json"
}

start_no_auth_server() {
  local mode="$1" bind_addr="$2" allow_public_bind="$3" node_json_bind="${4:-}"
  local data_dir="$TMP_ROOT/${mode}_data"
  local log_path="$data_dir/server.log"
  mkdir -p "$data_dir"

  stop_server
  if [ -n "$node_json_bind" ]; then
    write_node_config "$data_dir" "$node_json_bind" || {
      printf 'ERROR: failed to write node.json for %s\n' "$mode" >&2
      exit 1
    }
  fi

  local -a env_args=(
    env
    -u FLAPJACK_ADMIN_KEY
    -u FLAPJACK_ENV
    -u FLAPJACK_BIND_ADDR
    -u FLAPJACK_PORT
    -u FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND
    -u FLAPJACK_DISABLE_DASHBOARD
    -u FLAPJACK_NODE_ID
    -u FLAPJACK_PEERS
    FLAPJACK_DATA_DIR="$data_dir"
    FLAPJACK_BIND_ADDR="$bind_addr"
    FLAPJACK_NO_AUTH=1
  )
  if [ "$allow_public_bind" = "1" ]; then
    env_args+=(FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND=1)
  fi

  "${env_args[@]}" "$BIN" >"$log_path" 2>&1 &
  SERVER_PID=$!
  LAST_LOG_PATH="$log_path"
}

is_job_running() {
  local expected_pid="$1" running_pid
  for running_pid in $(jobs -pr); do
    if [ "$running_pid" = "$expected_pid" ]; then
      return 0
    fi
  done
  return 1
}

wait_for_exit_nonzero() {
  local description="$1" exit_code="" i
  for i in $(seq 1 80); do
    if ! is_job_running "$SERVER_PID"; then
      set +e
      wait "$SERVER_PID"
      exit_code=$?
      set -e
      if [ "$exit_code" -ne 0 ]; then
        pass "$description"
      else
        fail "$description" "expected non-zero exit, got ${exit_code}"
      fi
      SERVER_PID=""
      return 0
    fi
    sleep 0.1
  done
  fail "$description" "process ${SERVER_PID} did not exit"
  stop_server
  return 1
}

wait_for_no_auth_health() {
  local description="$1"
  if "$WAIT_FOR_FLAPJACK" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$LAST_LOG_PATH" \
    --retries 80 \
    --interval-seconds 0.5; then
    pass "$description"
  else
    fail "$description" "server did not reach exact-200 /health"
    return 1
  fi
}

assert_log_contains() {
  local description="$1" expected="$2"
  if grep -Fq "$expected" "$LAST_LOG_PATH"; then
    pass "$description"
  else
    fail "$description" "expected log line not found: ${expected}"
    return 1
  fi
}

assert_log_not_contains() {
  local description="$1" unexpected="$2"
  if grep -Fq "$unexpected" "$LAST_LOG_PATH"; then
    fail "$description" "unexpected log line found: ${unexpected}"
    return 1
  else
    pass "$description"
  fi
}

request_status() {
  local path="$1"
  local body_file="$TMP_ROOT/response_body.txt"
  local header_file="$TMP_ROOT/response_headers.txt"
  curl -sS -o "$body_file" -D "$header_file" -w '%{http_code}' "${BASE}${path}"
}

response_location() {
  local header_file="$TMP_ROOT/response_headers.txt"
  tr -d '\r' <"$header_file" | sed -n 's/^[Ll]ocation: //p' | head -1
}

assert_status() {
  local description="$1" path="$2" expected="$3"
  local actual
  actual="$(request_status "$path")"
  if [ "$actual" = "$expected" ]; then
    pass "$description"
  else
    fail "$description" "path=${path} expected=${expected} actual=${actual}"
    return 1
  fi
}

assert_redirect_location() {
  local description="$1" path="$2" expected_status="$3" expected_location="$4"
  local actual_status actual_location
  actual_status="$(request_status "$path")"
  actual_location="$(response_location)"
  if [ "$actual_status" = "$expected_status" ] && [ "$actual_location" = "$expected_location" ]; then
    pass "$description"
  else
    fail "$description" "path=${path} expected=${expected_status} ${expected_location} actual=${actual_status} ${actual_location}"
    return 1
  fi
}

run_dashboard_case() {
  build_current_checkout_binary

  start_server locked
  assert_status 'locked /dashboard is 404' '/dashboard' '404'
  assert_status 'locked /swagger-ui is 404' '/swagger-ui' '404'
  assert_status 'locked /swagger-ui/ is 404' '/swagger-ui/' '404'
  assert_status 'locked /api-docs/openapi.json is 404' '/api-docs/openapi.json' '404'
  assert_status 'locked /health remains 200' '/health' '200'
  stop_server

  start_server default
  assert_status 'default /dashboard is 200' '/dashboard' '200'
  assert_status 'default /swagger-ui/ is 200' '/swagger-ui/' '200'
  assert_status 'default /api-docs/openapi.json is 200' '/api-docs/openapi.json' '200'
  assert_redirect_location 'default /swagger-ui redirects to slash route' '/swagger-ui' '303' '/swagger-ui/'
  stop_server
}

run_no_auth_case() {
  build_current_checkout_binary

  start_no_auth_server public_reject "0.0.0.0:0" ""
  wait_for_exit_nonzero 'public no-auth bind without override exits non-zero'
  assert_log_contains 'public no-auth bind without override logs guard error' "$NO_AUTH_PUBLIC_BIND_ERROR"

  start_no_auth_server public_allowed "0.0.0.0:0" "1"
  wait_for_no_auth_health 'public no-auth bind with override reaches health'
  assert_log_contains 'public no-auth bind with override logs warning' "$NO_AUTH_PUBLIC_BIND_WARNING"
  stop_server

  start_no_auth_server loopback_allowed "127.0.0.1:0" ""
  wait_for_no_auth_health 'loopback no-auth bind without override reaches health'
  assert_log_not_contains 'loopback no-auth bind does not log public warning' "$NO_AUTH_PUBLIC_BIND_WARNING"
  stop_server

  start_no_auth_server hostname_reject "localhost:0" ""
  wait_for_exit_nonzero 'hostname no-auth bind without override exits non-zero'
  assert_log_contains 'hostname no-auth bind without override logs guard error' "$NO_AUTH_HOSTNAME_BIND_ERROR"

  start_no_auth_server node_json_public_reject "127.0.0.1:0" "" "0.0.0.0:0"
  wait_for_exit_nonzero 'node.json public bind overrides loopback env and exits non-zero'
  assert_log_contains 'node.json public bind override logs guard error' "$NO_AUTH_PUBLIC_BIND_ERROR"
}

main() {
  if [ "$#" -ne 1 ]; then
    usage >&2
    exit 1
  fi

  require_tools
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/flapjack_dashboard_auth_hardening.XXXXXX")"

  case "$1" in
    dashboard)
      run_dashboard_case
      ;;
    no_auth)
      run_no_auth_case
      ;;
    --help|-h)
      usage
      ;;
    *)
      usage >&2
      exit 1
      ;;
  esac

  if [ "$TESTS_FAILED" -ne 0 ]; then
    exit 1
  fi
  printf 'dashboard_auth_hardening_acceptance: %s assertions passed\n' "$TESTS_RUN"
}

main "$@"
