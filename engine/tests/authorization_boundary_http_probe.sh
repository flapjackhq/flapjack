#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_FOR_FLAPJACK="$ENGINE_DIR/tests/common/wait_for_flapjack.sh"

ADMIN_KEY="sec-g12-admin-key"
APP_ID="sec-g12-app"
INDEX_NAME="secg12_probe"
BIN=""
TMP_ROOT=""
SERVER_PID=""
BASE=""
TESTS_RUN=0
TESTS_FAILED=0

cleanup() {
  local script_exit_code=$?
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi

  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    if [ "$TESTS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      printf 'INFO: preserved authorization boundary probe evidence at %s\n' "$TMP_ROOT" >&2
    else
      rm -rf "$TMP_ROOT"
    fi
  fi
}
trap cleanup EXIT

require_tools() {
  local missing=0 tool
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

target_dir() {
  if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    printf '%s\n' "$ENGINE_DIR/target"
  elif [ "${CARGO_TARGET_DIR#/}" != "$CARGO_TARGET_DIR" ]; then
    printf '%s\n' "$CARGO_TARGET_DIR"
  else
    printf '%s\n' "$ENGINE_DIR/$CARGO_TARGET_DIR"
  fi
}

resolve_binary() {
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

start_server() {
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

curl_json() {
  local method="$1" url="$2" body="$3" api_key="$4" body_path="$5"
  curl -sS \
    -X "$method" \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $api_key" \
    -H "content-type: application/json" \
    -o "$body_path" \
    -w '%{http_code}' \
    --data "$body" \
    "$url"
}

curl_empty() {
  local method="$1" url="$2" api_key="$3" body_path="$4"
  curl -sS \
    -X "$method" \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $api_key" \
    -o "$body_path" \
    -w '%{http_code}' \
    "$url"
}

curl_head() {
  local url="$1" api_key="$2" headers_path="$3"
  curl -sS \
    --head \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $api_key" \
    -o "$headers_path" \
    -w '%{http_code}' \
    "$url"
}

json_field() {
  local path="$1" field="$2"
  python3 - "$path" "$field" <<'PY'
import json
import sys

path, field = sys.argv[1], sys.argv[2]
with open(path) as fh:
    data = json.load(fh)
value = data
for part in field.split("."):
    value = value[part]
print(value)
PY
}

require_status() {
  local description="$1" expected="$2" actual="$3" body_path="$4"
  if [ "$actual" != "$expected" ]; then
    printf 'ERROR: %s expected=%s actual=%s\n' "$description" "$expected" "$actual" >&2
    cat "$body_path" >&2 || true
    exit 1
  fi
}

create_search_key() {
  local body_path="$TMP_ROOT/create_key.json" status
  status="$(curl_json \
    POST \
    "$BASE/1/keys" \
    '{"acl":["search"],"description":"sec g12 probe search key"}' \
    "$ADMIN_KEY" \
    "$body_path")"
  require_status "create search-scoped API key" "200" "$status" "$body_path"
  json_field "$body_path" key
}

seed_index() {
  local body_path="$TMP_ROOT/create_index.json" status task_body task_status task_id
  status="$(curl_json \
    POST \
    "$BASE/1/indexes" \
    "{\"uid\":\"$INDEX_NAME\"}" \
    "$ADMIN_KEY" \
    "$body_path")"
  require_status "create probe index" "200" "$status" "$body_path"

  body_path="$TMP_ROOT/seed_object.json"
  status="$(curl_json \
    PUT \
    "$BASE/1/indexes/$INDEX_NAME/probe-1" \
    '{"objectID":"probe-1","name":"alpha"}' \
    "$ADMIN_KEY" \
    "$body_path")"
  require_status "seed probe index" "200" "$status" "$body_path"
  task_id="$(json_field "$body_path" taskID)"
  if [ -z "$task_id" ]; then
    echo 'ERROR: seed response did not include taskID' >&2
    cat "$body_path" >&2 || true
    exit 1
  fi

  task_body="$TMP_ROOT/task.json"
  for _i in $(seq 1 80); do
    task_status="$(curl_empty GET "$BASE/1/indexes/$INDEX_NAME/task/$task_id" "$ADMIN_KEY" "$task_body")"
    if [ "$task_status" = "200" ] && [ "$(json_field "$task_body" status)" = "published" ]; then
      return 0
    fi
    sleep 0.25
  done

  printf 'ERROR: seed task %s did not publish\n' "$task_id" >&2
  cat "$task_body" >&2 || true
  exit 1
}

record_check() {
  local description="$1" expected="$2" actual="$3"
  TESTS_RUN=$((TESTS_RUN + 1))
  if [ "$actual" = "$expected" ]; then
    printf '[PASS] %s expected=%s actual=%s\n' "$description" "$expected" "$actual"
  else
    TESTS_FAILED=$((TESTS_FAILED + 1))
    printf '[FAIL] %s expected=%s actual=%s\n' "$description" "$expected" "$actual"
  fi
}

record_status_and_body_check() {
  local description="$1" expected_status="$2" actual_status="$3" body_path="$4" expected_body="$5"
  local actual_body
  actual_body="$(cat "$body_path")"
  TESTS_RUN=$((TESTS_RUN + 1))
  if [ "$actual_status" = "$expected_status" ] && [ "$actual_body" = "$expected_body" ]; then
    printf '[PASS] %s expected=%s actual=%s\n' "$description" "$expected_status" "$actual_status"
  else
    TESTS_FAILED=$((TESTS_FAILED + 1))
    printf '[FAIL] %s expected=status=%s body=%s actual=status=%s body=%s\n' \
      "$description" \
      "$expected_status" \
      "$expected_body" \
      "$actual_status" \
      "$actual_body"
  fi
}

run_checks() {
  local search_key="$1" body_path status

  body_path="$TMP_ROOT/check_1_head_admin.headers"
  status="$(curl_head "$BASE/1/indexes" "$ADMIN_KEY" "$body_path")"
  record_check "1 HEAD collection route preserves read ACL for admin header credentials" "200" "$status"

  body_path="$TMP_ROOT/check_2_head_search.headers"
  status="$(curl_head "$BASE/1/indexes" "$search_key" "$body_path")"
  record_check "2 HEAD collection route still rejects search-only credentials" "403" "$status"

  body_path="$TMP_ROOT/check_3_admin_query_key.json"
  status="$(curl -sS \
    -H "x-algolia-application-id: $APP_ID" \
    -o "$body_path" \
    -w '%{http_code}' \
    "$BASE/1/keys?x-algolia-api-key=$ADMIN_KEY")"
  record_status_and_body_check \
    "3 key routes reject admin credentials in query string" \
    "403" \
    "$status" \
    "$body_path" \
    '{"message":"Invalid Application-ID or API key","status":403}'

  body_path="$TMP_ROOT/check_4_admin_header_key.json"
  status="$(curl_empty GET "$BASE/1/keys" "$ADMIN_KEY" "$body_path")"
  record_check "4 key routes accept admin credentials in header" "200" "$status"

  body_path="$TMP_ROOT/check_5_search_query_key.json"
  status="$(curl -sS \
    -X POST \
    -H "x-algolia-application-id: $APP_ID" \
    -H "content-type: application/json" \
    -o "$body_path" \
    -w '%{http_code}' \
    --data '{"query":"alpha"}' \
    "$BASE/1/indexes/$INDEX_NAME/query?x-algolia-api-key=$search_key")"
  record_check "5 search route accepts search credentials in query string" "200" "$status"

  body_path="$TMP_ROOT/check_6_put_index_status.json"
  status="$(curl_json \
    PUT \
    "$BASE/1/indexes/$INDEX_NAME" \
    '{"name":"beta"}' \
    "$ADMIN_KEY" \
    "$body_path")"
  record_check "6 unregistered PUT index method fails closed" "403" "$status"
}

main() {
  require_tools
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fj-auth-boundary.XXXXXX")"
  resolve_binary
  start_server

  local search_key
  search_key="$(create_search_key)"
  seed_index
  run_checks "$search_key"

  if [ "$TESTS_RUN" -ne 6 ]; then
    printf 'ERROR: expected 6 checks, ran %s\n' "$TESTS_RUN" >&2
    exit 1
  fi
  if [ "$TESTS_FAILED" -ne 0 ]; then
    exit 1
  fi
}

main "$@"
