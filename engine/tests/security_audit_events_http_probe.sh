#!/usr/bin/env bash
#
# security_audit_events_http_probe.sh — RED served-boundary proof for SEC-G4
# security-audit event coverage through the real flapjack-server binary.
#
# Expected RED shape on the unfixed tree:
#   FAIL checks 1, 3, 4, 5, 6, and 7 because the canonical events do not exist.
#   PASS checks 2, 8, and 9 because they are absence-shaped until emitters exist.
#
# Exit codes:
#   0  GREEN  — all served audit checks passed.
#   1  RED    — one or more served audit assertions failed.
#   2  INDET  — setup, transport, status, or fixture precondition failed.
#
# Usage:
#   bash engine/tests/security_audit_events_http_probe.sh
#
# Environment:
#   FLAPJACK_BIN       Optional path to a prebuilt flapjack binary.
#   CARGO_TARGET_DIR  Optional Cargo target directory used to resolve debug/flapjack.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_HELPER="$SCRIPT_DIR/common/wait_for_flapjack.sh"

readonly ADMIN_KEY="sec-g4-admin-key-probe"
readonly APP_ID="sec-g4-app"
readonly EXPECTED_CHECKS=9
readonly SEARCH_QUERY_PROBE="sec_g4_query_string_probe"

BIN=""
TMP_ROOT=""
DATA_DIR=""
LOG=""
SERVER_PID=""
BASE=""
CHECKS_RUN=0
CHECKS_FAILED=0

pass() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  printf '  [PASS] %s\n' "$1"
}

fail() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  CHECKS_FAILED=$((CHECKS_FAILED + 1))
  printf '  [FAIL] %s\n' "$1" >&2
  if [ -n "${2:-}" ]; then
    printf '         %s\n' "$2" >&2
  fi
}

die_indeterminate() {
  printf 'INDETERMINATE: %s\n' "$1" >&2
  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    printf 'INFO: preserved security audit probe evidence at %s\n' "$TMP_ROOT" >&2
  fi
  exit 2
}

cleanup() {
  local script_exit_code=$?
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi

  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    if [ "$CHECKS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      printf 'INFO: preserved security audit probe evidence at %s\n' "$TMP_ROOT" >&2
    else
      rm -rf "$TMP_ROOT"
    fi
  fi
}
trap cleanup EXIT

require_tools() {
  local missing=0 tool
  for tool in awk cargo curl env grep head jq mkdir mktemp python3 rm sed seq sleep; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  [ "$missing" -eq 0 ] || die_indeterminate 'required tools missing'
  [ -x "$WAIT_HELPER" ] || die_indeterminate "readiness helper not executable: $WAIT_HELPER"
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
  if [ -n "${FLAPJACK_BIN:-}" ]; then
    [ -x "$FLAPJACK_BIN" ] || die_indeterminate 'FLAPJACK_BIN is not executable'
    BIN="$FLAPJACK_BIN"
    return
  fi

  BIN="$(target_dir)/debug/flapjack"
  [ -x "$BIN" ] || die_indeterminate 'debug flapjack binary is missing; run cargo build first'
}

start_server() {
  DATA_DIR="$TMP_ROOT/data"
  mkdir -p "$DATA_DIR"
  LOG="$TMP_ROOT/server.log"

  env \
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$DATA_DIR" \
    FLAPJACK_DISABLE_DASHBOARD=1 \
    "$BIN" --auto-port >"$LOG" 2>&1 &
  SERVER_PID=$!

  if ! "$WAIT_HELPER" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$LOG" \
    --retries 80 \
    --interval-seconds 0.5; then
    die_indeterminate 'server did not reach exact-200 /health'
  fi

  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$LOG" | head -1)"
  [ -n "$port" ] || die_indeterminate 'server healthy but no auto-port was found'
  BASE="http://127.0.0.1:${port}"
  printf 'Server ready at %s (pid %s)\n' "$BASE" "$SERVER_PID"
}

curl_json() {
  local method="$1" path="$2" key="$3" body="$4" out="$5"
  curl -sS \
    -X "$method" \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $key" \
    -H "content-type: application/json" \
    -o "$out" \
    -w '%{http_code}' \
    --data "$body" \
    "$BASE$path"
}

curl_empty() {
  local method="$1" path="$2" key="$3" out="$4"
  curl -sS \
    -X "$method" \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $key" \
    -o "$out" \
    -w '%{http_code}' \
    "$BASE$path"
}

require_status() {
  local description="$1" expected="$2" actual="$3"
  [ "$actual" = "$expected" ] || die_indeterminate "$description returned unexpected HTTP status"
}

json_field() {
  local file="$1" field="$2"
  jq -er "$field" "$file"
}

log_count() {
  local pattern="$1"
  grep -cF "$pattern" "$LOG" || true
}

canonical_event_line_count() {
  local event="$1" action="$2" target="$3" outcome="$4" additional_field="${5:-}"
  awk \
    -v event="event=\"${event}\"" \
    -v actor='actor="admin_api_key"' \
    -v action="action=\"${action}\"" \
    -v target="target=\"${target}\"" \
    -v outcome="outcome=\"${outcome}\"" \
    -v additional_field="$additional_field" \
    'index($0, event) && index($0, actor) && index($0, action) && index($0, target) && index($0, outcome) && (additional_field == "" || index($0, additional_field)) { count++ } END { print count + 0 }' \
    "$LOG"
}

api_key_target() {
  local key_value="$1"
  [ -n "$key_value" ] || die_indeterminate 'key fingerprint specimen was empty'
  printf '%s' "$key_value" | python3 -c 'import hashlib, sys; print("api_key:" + hashlib.sha256(sys.stdin.buffer.read()).hexdigest()[:16])'
}

assert_log_absent_value() {
  local value="$1"
  [ -n "$value" ] || die_indeterminate 'non-leak specimen was empty'
  grep -qF "$value" "$LOG" && return 1
  return 0
}

seed_index() {
  local index="$1" status task_id task_body task_status
  local body="$TMP_ROOT/seed_${index}.json"
  status="$(curl_json POST "/1/indexes/${index}/batch" "$ADMIN_KEY" \
    '{"requests":[{"action":"addObject","body":{"objectID":"doc-1","name":"alpha"}}]}' \
    "$body")"
  require_status "seed index ${index}" "200" "$status"
  task_id="$(json_field "$body" '.taskID // .taskId')"
  [ -n "$task_id" ] || die_indeterminate "seed index ${index} did not return taskID"

  task_body="$TMP_ROOT/task_${index}.json"
  for _i in $(seq 1 80); do
    task_status="$(curl_empty GET "/1/indexes/${index}/task/${task_id}" "$ADMIN_KEY" "$task_body")"
    if [ "$task_status" = "200" ] && [ "$(json_field "$task_body" '.status' 2>/dev/null || true)" = "published" ]; then
      return 0
    fi
    sleep 0.25
  done
  die_indeterminate "seed index ${index} task did not publish"
}

create_search_key() {
  local body="$TMP_ROOT/search_key.json" status key
  status="$(curl_json POST "/1/keys" "$ADMIN_KEY" \
    '{"acl":["search"],"indexes":["secg4_search"],"description":"sec g4 search key"}' \
    "$body")"
  require_status 'create search key' "200" "$status"
  key="$(json_field "$body" '.key')"
  [ -n "$key" ] || die_indeterminate 'create search key returned empty key'
  printf '%s\n' "$key"
}

run_probe() {
  local status body key key_target line_count success_count failure_count before after delta snapshot_path

  printf 'SEC-G4 served RED expectations: checks 1,3,4,5,6,7 FAIL; checks 2,8,9 PASS on the unfixed tree.\n'

  body="$TMP_ROOT/check1_admin_auth.json"
  status="$(curl_empty GET "/1/keys" "$ADMIN_KEY" "$body")"
  require_status 'admin auth success setup' "200" "$status"
  line_count="$(canonical_event_line_count 'security_audit_auth_success' 'authenticate' 'route:/1/keys' 'success')"
  if [ "$line_count" -gt 0 ]; then
    pass "1 admin auth success event emitted"
  else
    fail "1 admin auth success event emitted" "matching safe event lines: 0"
  fi

  if assert_log_absent_value "$ADMIN_KEY"; then
    pass "2 admin key absent from server log"
  else
    fail "2 admin key absent from server log" "forbidden admin-key occurrences: $(log_count "$ADMIN_KEY")"
  fi

  body="$TMP_ROOT/check3_create_key.json"
  status="$(curl_json POST "/1/keys" "$ADMIN_KEY" \
    '{"acl":["search"],"indexes":["secg4_create"],"description":"sec g4 create key"}' \
    "$body")"
  require_status 'create key setup' "200" "$status"
  key="$(json_field "$body" '.key')"
  [ -n "$key" ] || die_indeterminate 'create key returned empty key'
  key_target="$(api_key_target "$key")"
  line_count="$(canonical_event_line_count 'security_audit_admin_action' 'create_key' "$key_target" 'success')"
  if [ "$line_count" -gt 0 ] && assert_log_absent_value "$key"; then
    pass "3 create-key event emitted and returned key absent"
  else
    fail "3 create-key event emitted and returned key absent" "matching canonical event lines: ${line_count}; forbidden returned-key occurrences: $(log_count "$key")"
  fi

  body="$TMP_ROOT/check4_delete_key.json"
  status="$(curl_empty DELETE "/1/keys/${key}" "$ADMIN_KEY" "$body")"
  require_status 'delete key setup' "200" "$status"
  body="$TMP_ROOT/check4_delete_key_failure.json"
  status="$(curl_empty DELETE "/1/keys/${key}" "$ADMIN_KEY" "$body")"
  require_status 'delete key failure setup' "404" "$status"
  success_count="$(canonical_event_line_count 'security_audit_admin_action' 'delete_key' "$key_target" 'success')"
  failure_count="$(canonical_event_line_count 'security_audit_admin_action' 'delete_key' "$key_target" 'failure' 'reason="key_not_found"')"
  if [ "$success_count" -gt 0 ] && [ "$failure_count" -gt 0 ] && assert_log_absent_value "$key"; then
    pass "4 delete-key success and failure events emitted and raw path key absent"
  else
    fail "4 delete-key success and failure events emitted and raw path key absent" "matching canonical success/failure lines: ${success_count}/${failure_count}; forbidden path-key occurrences: $(log_count "$key")"
  fi

  seed_index "secg4_delete_index"
  body="$TMP_ROOT/check5_delete_index.json"
  status="$(curl_empty DELETE "/1/indexes/secg4_delete_index" "$ADMIN_KEY" "$body")"
  require_status 'delete index setup' "200" "$status"
  body="$TMP_ROOT/check5_delete_index_failure.json"
  status="$(curl_empty DELETE "/1/indexes/secg4_delete_index_missing" "$ADMIN_KEY" "$body")"
  require_status 'delete index failure setup' "404" "$status"
  success_count="$(canonical_event_line_count 'security_audit_admin_action' 'delete_index' 'index:secg4_delete_index' 'success')"
  failure_count="$(canonical_event_line_count 'security_audit_admin_action' 'delete_index' 'index:secg4_delete_index_missing' 'failure')"
  if [ "$success_count" -gt 0 ] && [ "$failure_count" -gt 0 ]; then
    pass "5 delete-index success and failure targets recorded"
  else
    fail "5 delete-index success and failure targets recorded" "matching canonical success/failure lines: ${success_count}/${failure_count}"
  fi

  seed_index "secg4_settings"
  body="$TMP_ROOT/check6_settings.json"
  status="$(curl_json PUT "/1/indexes/secg4_settings/settings" "$ADMIN_KEY" \
    '{"userData":{"nested":{"probe":"sec_g4_settings_probe_token"}}}' \
    "$body")"
  case "$status" in
    2*) ;;
    *) die_indeterminate 'settings update returned non-2xx status' ;;
  esac
  line_count="$(canonical_event_line_count 'security_audit_admin_action' 'set_settings' 'index:secg4_settings:settings' 'success' 'changed_fields="userData"')"
  if [ "$line_count" -gt 0 ] && assert_log_absent_value 'sec_g4_settings_probe_token'; then
    pass "6 settings event names userData and omits probe token"
  else
    fail "6 settings event names userData and omits probe token" "matching canonical event lines: ${line_count}; forbidden probe-token occurrences: $(log_count 'sec_g4_settings_probe_token')"
  fi

  seed_index "secg4_snapshot"
  snapshot_path="$TMP_ROOT/secg4_snapshot.tar.gz"
  status="$(curl -sS \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $ADMIN_KEY" \
    -o "$snapshot_path" \
    -w '%{http_code}' \
    "$BASE/1/indexes/secg4_snapshot/export")"
  require_status 'snapshot export setup' "200" "$status"
  [ -s "$snapshot_path" ] || die_indeterminate 'snapshot export returned empty bytes'
  body="$TMP_ROOT/check7_import_success.json"
  status="$(curl -sS \
    -X POST \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $ADMIN_KEY" \
    -H "content-type: application/gzip" \
    -o "$body" \
    -w '%{http_code}' \
    --data-binary "@$snapshot_path" \
    "$BASE/1/indexes/secg4_snapshot/import")"
  require_status 'snapshot import success setup' "200" "$status"
  body="$TMP_ROOT/check7_import_failure.json"
  status="$(curl -sS \
    -X POST \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $ADMIN_KEY" \
    -H "content-type: application/gzip" \
    -o "$body" \
    -w '%{http_code}' \
    --data-binary 'not-a-valid-sec-g4-snapshot' \
    "$BASE/1/indexes/secg4_snapshot_invalid/import")"
  require_status 'snapshot import failure setup' "500" "$status"
  success_count="$(canonical_event_line_count 'security_audit_admin_action' 'import_snapshot' 'index:secg4_snapshot:snapshot' 'success')"
  failure_count="$(canonical_event_line_count 'security_audit_admin_action' 'import_snapshot' 'index:secg4_snapshot_invalid:snapshot' 'failure')"
  if [ "$success_count" -gt 0 ] && [ "$failure_count" -gt 0 ]; then
    pass "7 snapshot import success and failure events emitted"
  else
    fail "7 snapshot import success and failure events emitted" "matching canonical success/failure lines: ${success_count}/${failure_count}"
  fi

  seed_index "secg4_search"
  local search_key
  search_key="$(create_search_key)"
  before="$(log_count 'event="security_audit_auth_success"')"
  body="$TMP_ROOT/check8_search.json"
  status="$(curl_json POST "/1/indexes/secg4_search/query" "$search_key" '{"query":"alpha"}' "$body")"
  require_status 'search auth setup' "200" "$status"
  after="$(log_count 'event="security_audit_auth_success"')"
  delta=$((after - before))
  if [ "$delta" -eq 0 ]; then
    pass "8 search-only auth emits zero auth-success delta"
  else
    fail "8 search-only auth emits zero auth-success delta" "auth-success delta: ${delta}"
  fi

  body="$TMP_ROOT/check9_query_probe.json"
  status="$(curl_json POST "/1/indexes/secg4_search/query?probe=${SEARCH_QUERY_PROBE}" "$search_key" '{"query":"alpha"}' "$body")"
  require_status 'query probe setup' "200" "$status"
  if assert_log_absent_value 'x-algolia-api-key' && assert_log_absent_value "$SEARCH_QUERY_PROBE"; then
    pass "9 header name and distinctive query-string probe absent"
  else
    fail "9 header name and distinctive query-string probe absent" "forbidden occurrence counts are nonzero"
  fi

  if [ "$CHECKS_RUN" -ne "$EXPECTED_CHECKS" ]; then
    die_indeterminate "expected ${EXPECTED_CHECKS} checks but ran ${CHECKS_RUN}"
  fi
}

main() {
  require_tools
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fj-sec-g4-probe.XXXXXX")"
  resolve_binary
  start_server
  run_probe

  printf 'SEC-G4 served probe summary: checks=%s failed=%s\n' "$CHECKS_RUN" "$CHECKS_FAILED"
  if [ "$CHECKS_FAILED" -gt 0 ]; then
    exit 1
  fi
}

main "$@"
