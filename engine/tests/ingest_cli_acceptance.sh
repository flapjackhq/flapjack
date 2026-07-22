#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_FOR_FLAPJACK="$ENGINE_DIR/tests/common/wait_for_flapjack.sh"

APP_ID="flapjack"
INDEX_NAME="ingest_cli_acceptance"
ADMIN_KEY=""
BASE=""
BIN=""
TMP_DATA=""
SERVER_PID=""
BUILD_LOG=""

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_PASSED=$((TESTS_PASSED + 1))
  printf '  [PASS] %s\n' "$1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  printf '  [FAIL] %s\n' "$1"
  if [ -n "${2:-}" ]; then
    printf '    %s\n' "$2"
  fi
}

cleanup() {
  local script_exit_code=$?
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [ -n "$TMP_DATA" ] && [ -d "$TMP_DATA" ]; then
    if [ "$TESTS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      local snapshot="/tmp/flapjack_ingest_cli_acceptance_failure_${$}_$(date +%s)"
      cp -R "$TMP_DATA" "$snapshot"
      printf 'INFO: preserved ingest acceptance data at %s\n' "$snapshot"
    else
      rm -rf "$TMP_DATA"
    fi
  fi
  if [ -n "$BUILD_LOG" ] && [ -f "$BUILD_LOG" ]; then
    rm -f "$BUILD_LOG"
  fi
}
trap cleanup EXIT

require_tools() {
  local missing=0 tool
  for tool in cargo curl jq od sed tr; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  if [ "$missing" -ne 0 ]; then
    exit 1
  fi
}

generate_admin_key() {
  local random_hex
  random_hex="$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  if [ -z "$random_hex" ]; then
    echo 'ERROR: failed to generate admin key' >&2
    exit 1
  fi
  printf 'fj_ingest_acceptance_%s\n' "$random_hex"
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
  BUILD_LOG="$(mktemp)"
  if (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$BUILD_LOG" 2>&1); then
    tail -5 "$BUILD_LOG"
  else
    tail -30 "$BUILD_LOG" >&2 || true
    echo 'ERROR: cargo build -p flapjack-server failed' >&2
    exit 1
  fi
  BIN="$(target_dir)/debug/flapjack"
  if [ ! -x "$BIN" ]; then
    echo "ERROR: expected current-checkout binary at $BIN" >&2
    exit 1
  fi
}

start_server() {
  TMP_DATA="$(mktemp -d)"
  ADMIN_KEY="$(generate_admin_key)"

  if [ "${INGEST_ACCEPTANCE_SKIP_SERVER_START:-0}" = "1" ]; then
    BASE="${INGEST_ACCEPTANCE_EXTERNAL_BASE_URL:-http://127.0.0.1:9}"
    return
  fi

  FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$TMP_DATA" \
    "$BIN" --auto-port >"$TMP_DATA/server.log" 2>&1 &
  SERVER_PID=$!

  "$WAIT_FOR_FLAPJACK" --pid "$SERVER_PID" --host 127.0.0.1 --port auto --log-path "$TMP_DATA/server.log" --retries 60 --interval-seconds 0.5
  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$TMP_DATA/server.log" | head -1)"
  if [ -z "$port" ]; then
    echo 'ERROR: server became healthy but no auto-port was found in startup log' >&2
    cat "$TMP_DATA/server.log" >&2 || true
    exit 1
  fi
  BASE="http://127.0.0.1:${port}"
}

curl_json() {
  local method="$1" path="$2" body="${3:-}"
  curl -sS -X "$method" "${BASE}${path}" \
    -H "content-type: application/json" \
    -H "x-algolia-api-key: ${ADMIN_KEY}" \
    -H "x-algolia-application-id: ${APP_ID}" \
    --data "$body"
}

query_index() {
  local query="$1"
  curl_json POST "/1/indexes/${INDEX_NAME}/query" "$query"
}

wait_for_query() {
  local description="$1" jq_filter="$2" last_body=""
  for _i in $(seq 1 80); do
    last_body="$(query_index '{"query":"","hitsPerPage":100}')"
    if printf '%s\n' "$last_body" | jq -e "$jq_filter" >/dev/null 2>&1; then
      pass "$description"
      return 0
    fi
    sleep 0.25
  done
  fail "$description" "$last_body"
  return 1
}

write_source_files() {
  cat >"$TMP_DATA/env_source.ndjson" <<'EOF'
{"objectID":"doc_env","title":"Env Credential","rank":1}
{"objectID":"doc_delete","title":"Delete Me","rank":2}
EOF

  cat >"$TMP_DATA/file_source.ndjson" <<'EOF'
{"objectID":"doc_file","title":"File Credential","rank":3}
EOF

  if [ "${INGEST_ACCEPTANCE_DELETE_MODE:-normal}" = "skip" ]; then
    # Simulate a silently-unapplied delete: keep the same record count (so the
    # report-count assertion still passes) but re-upsert doc_delete instead of
    # deleting it, so ONLY the delete-effect query assertion can go red. This
    # isolates the delete oracle in the red-proof companion.
    cat >"$TMP_DATA/stdin_source.ndjson" <<'EOF'
{"objectID":"doc_stdin","title":"Stdin Source","rank":4}
{"objectID":"doc_delete","title":"Delete Me"}
EOF
  else
    cat >"$TMP_DATA/stdin_source.ndjson" <<'EOF'
{"objectID":"doc_stdin","title":"Stdin Source","rank":4}
{"objectID":"doc_delete","_action":"delete"}
EOF
  fi

  cat >"$TMP_DATA/replace_source.ndjson" <<'EOF'
{"objectID":"doc_replace","title":"Should Not Land"}
EOF

  printf '%s\n' "$ADMIN_KEY" >"$TMP_DATA/api_key.txt"
}

ingest_index_arg() {
  printf '%s%s\n' "$INDEX_NAME" "${INGEST_ACCEPTANCE_DEST_INDEX_SUFFIX:-}"
}

run_ingest_report() {
  local source="$1" report_path="$2"
  shift 2
  "$BIN" ingest \
    --endpoint "$BASE" \
    --index "$(ingest_index_arg)" \
    --source "$source" \
    --application-id "$APP_ID" \
    --batch-size 2 \
    --report-json \
    "$@" >"$report_path"
}

assert_report_counts() {
  local report_path="$1" attempted="$2" confirmed="$3" unknown="$4"
  if jq -e ".attempted == ${attempted} and .confirmed_committed == ${confirmed} and .outcome_unknown == ${unknown}" "$report_path" >/dev/null; then
    pass "JSON report counts for $(basename "$report_path")"
  else
    fail "JSON report counts for $(basename "$report_path")" "$(cat "$report_path")"
  fi
}

seed_replace_sentinel() {
  local body task_id
  body="$(curl_json POST "/1/indexes/${INDEX_NAME}/batch" '{"requests":[{"action":"addObject","body":{"objectID":"sentinel","title":"Keep Me"}}]}')"
  task_id="$(printf '%s\n' "$body" | jq -r '.taskID // empty')"
  if [ -z "$task_id" ]; then
    fail 'seed replace sentinel through batch endpoint' "$body"
    return 1
  fi
  wait_for_query 'replace sentinel is visible before refusal' '.hits | map(.objectID) | index("sentinel") != null'
}

run_acceptance() {
  require_tools
  build_current_checkout_binary
  start_server
  export FJ_INGEST_ACCEPTANCE_KEY="$ADMIN_KEY"
  write_source_files

  run_ingest_report "$TMP_DATA/env_source.ndjson" "$TMP_DATA/env_report.json" --api-key-env FJ_INGEST_ACCEPTANCE_KEY
  assert_report_counts "$TMP_DATA/env_report.json" 2 2 0

  run_ingest_report "$TMP_DATA/file_source.ndjson" "$TMP_DATA/file_report.json" --api-key-file "$TMP_DATA/api_key.txt"
  assert_report_counts "$TMP_DATA/file_report.json" 1 1 0

  FLAPJACK_DATA_DIR="$TMP_DATA" FJ_INGEST_ACCEPTANCE_KEY="$ADMIN_KEY" \
    "$BIN" ingest \
      --endpoint "$BASE" \
      --index "$(ingest_index_arg)" \
      --source - \
      --application-id "$APP_ID" \
      --batch-size 2 \
      --api-key-env FJ_INGEST_ACCEPTANCE_KEY \
      --report-json <"$TMP_DATA/stdin_source.ndjson" >"$TMP_DATA/stdin_report.json"
  assert_report_counts "$TMP_DATA/stdin_report.json" 2 2 0

  wait_for_query 'upserts are visible in the requested target index' \
    '.hits | map({key: .objectID, value: .title}) | from_entries | .doc_env == "Env Credential" and .doc_file == "File Credential" and .doc_stdin == "Stdin Source"'
  wait_for_query 'explicit delete action removes the requested objectID' \
    '.hits | map(.objectID) | index("doc_delete") == null'

  seed_replace_sentinel
  set +e
  "$BIN" ingest \
    --endpoint "$BASE" \
    --index "$INDEX_NAME" \
    --source "$TMP_DATA/replace_source.ndjson" \
    --application-id "$APP_ID" \
    --api-key-stdin \
    --mode replace \
    --report-json >"$TMP_DATA/replace_report.json" 2>"$TMP_DATA/replace_stderr.log" <<<"$ADMIN_KEY"
  local replace_exit=$?
  set -e
  if [ "$replace_exit" -ne 0 ] && jq -e '.failure_classification == "replace_not_supported" and .attempted == 0 and .confirmed_committed == 0 and .outcome_unknown == 0' "$TMP_DATA/replace_report.json" >/dev/null; then
    pass 'replace mode returns typed zero-mutation refusal'
  else
    fail 'replace mode returns typed zero-mutation refusal' "exit=${replace_exit} report=$(cat "$TMP_DATA/replace_report.json" 2>/dev/null || true) stderr=$(cat "$TMP_DATA/replace_stderr.log" 2>/dev/null || true)"
  fi
  wait_for_query 'replace refusal leaves sentinel unchanged and source absent' \
    '.hits | map({key: .objectID, value: .title}) | from_entries | .sentinel == "Keep Me" and (has("doc_replace") | not)'

  printf '\nResults: %d/%d passed\n' "$TESTS_PASSED" "$TESTS_RUN"
  [ "$TESTS_FAILED" -eq 0 ]
}

run_acceptance "$@"
