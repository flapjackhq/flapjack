#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  ./engine/tests/upgrade_smoke.sh --old-bin <path> --new-bin <path>

Runs a minimal upgrade smoke by:
1. starting the old binary on a temp data dir
2. seeding data and verifying search
3. stopping the old binary
4. starting the new binary on the same data dir
5. verifying /health, /health/ready, /dashboard, search, and writes
EOF
}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WAIT_HELPER="$SCRIPT_DIR/common/wait_for_flapjack.sh"
ADMIN_KEY=""
INDEX_NAME="upgrade_smoke"
QUERY_TOKEN="upgrade-smoke-token"

OLD_BIN=""
NEW_BIN=""
TMP_DIR=""
DATA_DIR=""
OLD_LOG=""
NEW_LOG=""
OLD_PID=""
NEW_PID=""
INTERRUPTED_EXIT_CODE=0

pass() {
  printf 'PASS: %s\n' "$1"
}

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  if [ -n "${OLD_LOG:-}" ] && [ -f "$OLD_LOG" ]; then
    printf '\n== old log ==\n' >&2
    cat "$OLD_LOG" >&2 || true
  fi
  if [ -n "${NEW_LOG:-}" ] && [ -f "$NEW_LOG" ]; then
    printf '\n== new log ==\n' >&2
    cat "$NEW_LOG" >&2 || true
  fi
  exit 1
}

# TODO: Document cleanup.
cleanup() {
  local script_exit_code=$?
  local effective_exit_code="$script_exit_code"
  if [ "$INTERRUPTED_EXIT_CODE" -ne 0 ]; then
    effective_exit_code="$INTERRUPTED_EXIT_CODE"
  fi
  if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
    kill "$OLD_PID" 2>/dev/null || true
    wait "$OLD_PID" 2>/dev/null || true
  fi
  if [ -n "$NEW_PID" ] && kill -0 "$NEW_PID" 2>/dev/null; then
    kill "$NEW_PID" 2>/dev/null || true
    wait "$NEW_PID" 2>/dev/null || true
  fi
  if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
    if [ "$effective_exit_code" -ne 0 ]; then
      local failure_snapshot="/tmp/flapjack_upgrade_smoke_failure_${$}_$(date +%s)"
      cp -R "$TMP_DIR" "$failure_snapshot"
      printf 'INFO: preserved upgrade smoke data at %s\n' "$failure_snapshot"
    else
      rm -rf "$TMP_DIR"
    fi
  fi
}

extract_port_from_log() {
  local log_path="$1"
  sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$log_path" | head -1
}

generate_admin_key() {
  local random_hex
  random_hex="$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  [ -n "$random_hex" ] || fail "failed to generate a random admin key from /dev/urandom"
  printf 'fj_upgrade_smoke_%s\n' "$random_hex"
}

# TODO: Document http_json.
http_json() {
  local method="$1"
  local url="$2"
  local body="${3:-}"
  local -a curl_args=(
    -fsS
    -X "$method"
    "$url"
    -H "content-type: application/json"
    -H "x-algolia-application-id: flapjack"
    -H "x-algolia-api-key: $ADMIN_KEY"
  )

  if [ -n "$body" ]; then
    curl_args+=(-d "$body")
  fi

  curl "${curl_args[@]}"
}

wait_for_task_published() {
  local base_url="$1"
  local task_id="$2"

  for _ in $(seq 1 120); do
    local response
    response="$(http_json GET "$base_url/1/indexes/$INDEX_NAME/task/$task_id")" || true
    if [ -n "$response" ] && [ "$(printf '%s' "$response" | jq -r '.status // empty')" = "published" ]; then
      return 0
    fi
    sleep 0.25
  done

  fail "task $task_id did not reach published state"
}

verify_search_hits() {
  local base_url="$1"
  local query="$2"
  local expected_hits="$3"
  local request_body
  local response
  local hits

  request_body="$(jq -cn --arg query "$query" '{query: $query}')"
  response="$(http_json POST "$base_url/1/indexes/$INDEX_NAME/query" "$request_body")"
  hits="$(printf '%s' "$response" | jq -r '.nbHits')"
  [ "$hits" = "$expected_hits" ] || fail "expected $expected_hits hits for query '$query', got $hits"
}

start_server() {
  local bin_path="$1"
  local log_path="$2"

  FLAPJACK_ENV=production \
  FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
  FLAPJACK_BIND_ADDR="127.0.0.1:0" \
  FLAPJACK_DATA_DIR="$DATA_DIR" \
  "$bin_path" >"$log_path" 2>&1 &

  printf '%s' "$!"
}

# TODO: Document main.
main() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --old-bin)
        OLD_BIN="${2:-}"
        shift 2
        ;;
      --new-bin)
        NEW_BIN="${2:-}"
        shift 2
        ;;
      --help|-h)
        usage
        return 0
        ;;
      *)
        echo "ERROR: unknown argument: $1" >&2
        usage >&2
        return 1
        ;;
    esac
  done

  [ -n "$OLD_BIN" ] || { echo "ERROR: --old-bin is required" >&2; usage >&2; return 1; }
  [ -n "$NEW_BIN" ] || { echo "ERROR: --new-bin is required" >&2; usage >&2; return 1; }
  [ -x "$OLD_BIN" ] || fail "old binary is not executable: $OLD_BIN"
  [ -x "$NEW_BIN" ] || fail "new binary is not executable: $NEW_BIN"
  [ -x "$WAIT_HELPER" ] || fail "missing wait helper: $WAIT_HELPER"

  ADMIN_KEY="$(generate_admin_key)"
  TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/flapjack-upgrade-smoke.XXXXXX")"
  DATA_DIR="$TMP_DIR/data"
  OLD_LOG="$TMP_DIR/old.log"
  NEW_LOG="$TMP_DIR/new.log"
  mkdir -p "$DATA_DIR"

  OLD_PID="$(start_server "$OLD_BIN" "$OLD_LOG")"
  "$WAIT_HELPER" --pid "$OLD_PID" --port auto --log-path "$OLD_LOG" >/dev/null
  local old_port
  old_port="$(extract_port_from_log "$OLD_LOG")"
  [ -n "$old_port" ] || fail "could not detect old server port"
  local old_base="http://127.0.0.1:$old_port"

  local batch_response
  local task_id
  batch_response="$(http_json POST "$old_base/1/indexes/$INDEX_NAME/batch" '{
    "requests": [
      {
        "action": "addObject",
        "body": {
          "objectID": "old-doc-1",
          "title": "Upgrade smoke old doc",
          "token": "'"$QUERY_TOKEN"'"
        }
      },
      {
        "action": "addObject",
        "body": {
          "objectID": "old-doc-2",
          "title": "Upgrade smoke second doc",
          "token": "'"$QUERY_TOKEN"'"
        }
      }
    ]
  }')"
  task_id="$(printf '%s' "$batch_response" | jq -r '.taskID')"
  wait_for_task_published "$old_base" "$task_id"
  verify_search_hits "$old_base" "$QUERY_TOKEN" "2"
  pass "old binary seeded and searchable"

  kill "$OLD_PID" 2>/dev/null || true
  wait "$OLD_PID" 2>/dev/null || true
  OLD_PID=""

  NEW_PID="$(start_server "$NEW_BIN" "$NEW_LOG")"
  "$WAIT_HELPER" --pid "$NEW_PID" --port auto --log-path "$NEW_LOG" >/dev/null
  local new_port
  new_port="$(extract_port_from_log "$NEW_LOG")"
  [ -n "$new_port" ] || fail "could not detect new server port"
  local new_base="http://127.0.0.1:$new_port"

  curl -fsS "$new_base/health" >/dev/null || fail "new binary health check failed"
  curl -fsS "$new_base/health/ready" >/dev/null || fail "new binary readiness check failed"
  curl -fsS "$new_base/dashboard" >/dev/null || fail "new binary dashboard load failed"
  verify_search_hits "$new_base" "$QUERY_TOKEN" "2"
  pass "new binary preserved pre-upgrade search state"

  local upgrade_write_response
  local upgrade_task_id
  upgrade_write_response="$(http_json POST "$new_base/1/indexes/$INDEX_NAME/batch" '{
    "requests": [
      {
        "action": "addObject",
        "body": {
          "objectID": "new-doc-1",
          "title": "Upgrade smoke new doc",
          "token": "post-upgrade-token"
        }
      }
    ]
  }')"
  upgrade_task_id="$(printf '%s' "$upgrade_write_response" | jq -r '.taskID')"
  wait_for_task_published "$new_base" "$upgrade_task_id"
  verify_search_hits "$new_base" "post-upgrade-token" "1"
  pass "new binary accepted writes on the upgraded data dir"
}

trap cleanup EXIT
trap 'INTERRUPTED_EXIT_CODE=130; cleanup; exit 130' INT
trap 'INTERRUPTED_EXIT_CODE=143; cleanup; exit 143' TERM

main "$@"
