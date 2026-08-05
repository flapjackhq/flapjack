#!/usr/bin/env bash

# Shared shell helpers for HTTP probes that build and own a short-lived
# flapjack-server process. The sourcing script owns probe-specific assertions
# and must define ENGINE_DIR, WAIT_FOR_FLAPJACK, APP_ID, ADMIN_KEY, TMP_ROOT,
# SERVER_PID, BASE, TESTS_RUN, and TESTS_FAILED.

: "${HTTP_PROBE_STOP_ATTEMPTS:=40}"
: "${HTTP_PROBE_STOP_INTERVAL_SECONDS:=0.25}"

http_probe_validate_stop_config() {
  if ! [[ "$HTTP_PROBE_STOP_ATTEMPTS" =~ ^[1-9][0-9]{0,3}$ ]] \
    || [ "$HTTP_PROBE_STOP_ATTEMPTS" -gt 1200 ]; then
    printf 'ERROR: HTTP_PROBE_STOP_ATTEMPTS must be an integer from 1 to 1200, got %s\n' \
      "$HTTP_PROBE_STOP_ATTEMPTS" >&2
    return 1
  fi
  if ! [[ "$HTTP_PROBE_STOP_INTERVAL_SECONDS" =~ ^(10|[0-9](\.[0-9]{1,3})?)$ ]]; then
    printf 'ERROR: HTTP_PROBE_STOP_INTERVAL_SECONDS must be a number from 0 to 10 with at most three decimal places, got %s\n' \
      "$HTTP_PROBE_STOP_INTERVAL_SECONDS" >&2
    return 1
  fi
}

http_probe_wait_for_server_exit() {
  local attempt
  if ! http_probe_validate_stop_config; then
    return 1
  fi
  for ((attempt = 0; attempt < HTTP_PROBE_STOP_ATTEMPTS; attempt++)); do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      wait "$SERVER_PID" 2>/dev/null || true
      return 0
    fi
    sleep "$HTTP_PROBE_STOP_INTERVAL_SECONDS"
  done

  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    wait "$SERVER_PID" 2>/dev/null || true
    return 0
  fi
  return 1
}

http_probe_stop_server() {
  if [ -z "$SERVER_PID" ]; then
    return 0
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    wait "$SERVER_PID" 2>/dev/null || true
    return 0
  fi

  kill "$SERVER_PID" 2>/dev/null || true
  if http_probe_wait_for_server_exit; then
    return 0
  fi

  kill -KILL "$SERVER_PID" 2>/dev/null || true
  if http_probe_wait_for_server_exit; then
    return 0
  fi

  printf 'ERROR: server process %s did not stop during probe cleanup\n' "$SERVER_PID" >&2
  return 1
}

http_probe_cleanup() {
  local script_exit_code="$1"
  local probe_name="$2"
  local cleanup_failed=0
  if ! http_probe_stop_server; then
    cleanup_failed=1
  fi

  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    if [ "$TESTS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ] || [ "$cleanup_failed" -ne 0 ]; then
      printf 'INFO: preserved %s probe evidence at %s\n' "$probe_name" "$TMP_ROOT" >&2
    else
      rm -rf "$TMP_ROOT"
    fi
  fi

  return "$cleanup_failed"
}

http_probe_require_tools() {
  local missing=0 tool
  if ! http_probe_validate_stop_config; then
    exit 1
  fi
  for tool in cargo curl grep mktemp python3 sed; do
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

http_probe_target_dir() {
  if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    printf '%s\n' "$ENGINE_DIR/target"
  elif [ "${CARGO_TARGET_DIR#/}" != "$CARGO_TARGET_DIR" ]; then
    printf '%s\n' "$CARGO_TARGET_DIR"
  else
    printf '%s\n' "$ENGINE_DIR/$CARGO_TARGET_DIR"
  fi
}

http_probe_resolve_default_binary() {
  local build_log="$TMP_ROOT/build.log"
  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$build_log" 2>&1); then
    tail -30 "$build_log" >&2 || true
    echo 'ERROR: cargo build -p flapjack-server failed' >&2
    exit 1
  fi

  BIN="$(http_probe_target_dir)/debug/flapjack"
  if [ ! -x "$BIN" ]; then
    printf 'ERROR: expected current-checkout binary at %s\n' "$BIN" >&2
    exit 1
  fi
}

http_probe_start_server() {
  local data_dir="$TMP_ROOT/data"
  local log_path="$data_dir/server.log"
  mkdir -p "$data_dir"

  env \
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$data_dir" \
    FLAPJACK_DISABLE_DASHBOARD=1 \
    "$BIN" --auto-port >"$log_path" 2>&1 &
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

http_probe_curl_json() {
  local method="$1" url="$2" body="$3" body_path="$4"
  curl -sS \
    -X "$method" \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $ADMIN_KEY" \
    -H "content-type: application/json" \
    -o "$body_path" \
    -w '%{http_code}' \
    --data "$body" \
    "$url"
}

http_probe_curl_get() {
  local url="$1" body_path="$2"
  curl -sS \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $ADMIN_KEY" \
    -o "$body_path" \
    -w '%{http_code}' \
    "$url"
}

http_probe_record_check() {
  local label="$1" expected="$2" actual="$3"
  TESTS_RUN=$((TESTS_RUN + 1))
  if [ "$actual" = "$expected" ]; then
    printf '[PASS] %s expected=%s actual=%s\n' "$label" "$expected" "$actual"
  else
    TESTS_FAILED=$((TESTS_FAILED + 1))
    printf '[FAIL] %s expected=%s actual=%s\n' "$label" "$expected" "$actual"
  fi
}

http_probe_require_status() {
  local description="$1" expected="$2" actual="$3" body_path="$4"
  if [ "$actual" != "$expected" ]; then
    printf 'ERROR: %s expected=%s actual=%s\n' "$description" "$expected" "$actual" >&2
    cat "$body_path" >&2 || true
    exit 1
  fi
}
