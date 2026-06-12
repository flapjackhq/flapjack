#!/usr/bin/env bash
# readme_quickstart_smoke.sh - Cold-install README quickstart smoke.
#
# Installs Flapjack with the public installer into an isolated temp directory,
# starts the installed binary with first-boot admin-key generation, and checks
# the README quickstart's batch, task, and typo-tolerant search contract.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ENGINE_DIR="$REPO_DIR/engine"
README_PATH="$REPO_DIR/README.md"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0
SERVER_PID=""
TMP_PARENT=""
INSTALL_ROOT=""
TMP_DATA=""
BIN=""
BASE=""
ADMIN_KEY=""
LAST_HTTP=""
LAST_BODY=""

# Kept as self-test contract markers for the README-owned query block: matrxi.
README_QUERY_REQUIRED_MARKER='matrxi'

# shellcheck source=engine/tests/common/readme_curl_blocks.sh
# shellcheck disable=SC1091
. "$SCRIPT_DIR/common/readme_curl_blocks.sh"

if [ "${1:-}" = "--self-test" ]; then
  hardcoded_batch_pattern="batch""_payload="
  hardcoded_query_pattern="request_json POST '/1/indexes/movies/""query'"
  if grep -q "$hardcoded_batch_pattern" "$0"; then
    echo "FAIL: quickstart smoke must not hard-code the README batch payload" >&2
    exit 1
  fi
  if grep -q "$hardcoded_query_pattern" "$0"; then
    echo "FAIL: quickstart smoke must not hard-code the README typo query" >&2
    exit 1
  fi
  if ! grep -q "run_readme_curl '/1/indexes/movies/batch'" "$0"; then
    echo "FAIL: quickstart smoke must execute the README batch curl block" >&2
    exit 1
  fi
  if ! grep -q "run_readme_curl '/1/indexes/movies/query'" "$0"; then
    echo "FAIL: quickstart smoke must execute the README query curl block" >&2
    exit 1
  fi
  if ! grep -q "$README_QUERY_REQUIRED_MARKER" "$README_PATH"; then
    echo "FAIL: README quickstart query must keep the typo-tolerant matrxi contract" >&2
    exit 1
  fi
  helper_error="$({ README_PATH="$README_PATH" run_readme_curl '/1/indexes/movies/query'; } 2>&1 || true)"
  if ! printf '%s\n' "$helper_error" | grep -q "ERROR: BASE must be set before executing README curl blocks"; then
    echo "FAIL: shared README curl helper must reject missing BASE at its boundary" >&2
    exit 1
  fi
  helper_error="$({ README_PATH="$README_PATH" BASE='http://127.0.0.1:7700' run_readme_curl '/1/indexes/movies/query'; } 2>&1 || true)"
  if ! printf '%s\n' "$helper_error" | grep -q "ERROR: ADMIN_KEY must be set before executing README curl blocks"; then
    echo "FAIL: shared README curl helper must reject missing ADMIN_KEY at its boundary" >&2
    exit 1
  fi
  malicious_readme="$(mktemp)"
  cat >"$malicious_readme" <<'EOF'
curl -X POST http://localhost:7700/1/indexes/movies/query --config /tmp/steal-me
EOF
  helper_error="$({ README_PATH="$malicious_readme" BASE='http://127.0.0.1:7700' ADMIN_KEY='test-admin-key' run_readme_curl '/1/indexes/movies/query'; } 2>&1 || true)"
  rm -f "$malicious_readme"
  if ! printf '%s\n' "$helper_error" | grep -q "Unsupported curl option in README curl block: --config"; then
    echo "FAIL: shared README curl helper must reject dangerous curl options from README blocks" >&2
    exit 1
  fi
  echo "PASS: quickstart smoke executes README curl blocks"
  exit 0
fi

pass() {
  TESTS_PASSED=$((TESTS_PASSED + 1))
  TESTS_RUN=$((TESTS_RUN + 1))
  printf "  \033[0;32m✓\033[0m %s\n" "$1"
}

fail() {
  TESTS_FAILED=$((TESTS_FAILED + 1))
  TESTS_RUN=$((TESTS_RUN + 1))
  printf "  \033[0;31m✗\033[0m %s\n" "$1"
  if [ -n "${2:-}" ]; then
    printf "    %s\n" "$2"
  fi
}

section() {
  printf "\n\033[1m%s\033[0m\n" "$1"
}

cleanup() {
  local script_exit_code=$?
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi

  if [ -n "$TMP_PARENT" ] && [ -d "$TMP_PARENT" ]; then
    if [ "$TESTS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      local failure_snapshot
      failure_snapshot="/tmp/flapjack_readme_quickstart_smoke_failure_${$}_$(date +%s)"
      cp -R "$TMP_PARENT" "$failure_snapshot"
      printf 'INFO: preserved README quickstart smoke data at %s\n' "$failure_snapshot"
    else
      rm -rf "$TMP_PARENT"
    fi
  fi
}
trap cleanup EXIT

require_tools() {
  local missing=0
  local tool
  for tool in curl jq sed sh mktemp python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  if [ "$missing" -ne 0 ]; then
    exit 1
  fi
  if [ ! -f "$README_PATH" ]; then
    echo "ERROR: README not found at $README_PATH" >&2
    exit 1
  fi
}

http_code() {
  printf '%s\n' "$1" | tail -1
}

http_body() {
  printf '%s\n' "$1" | sed '$d'
}

extract_task_id() {
  printf '%s\n' "$1" | jq -r 'if (.taskID | type == "number") then (.taskID | tostring) else empty end' 2>/dev/null || true
}

json_matches() {
  local body="$1"
  local jq_filter="$2"
  printf '%s\n' "$body" | jq -e "$jq_filter" >/dev/null 2>&1
}

request_json() {
  local response
  response="$(curl -sS -w '\n%{http_code}' -X "$1" "${BASE}${2}" \
    -H "X-Algolia-API-Key: ${ADMIN_KEY}" \
    -H 'X-Algolia-Application-Id: flapjack' \
    -H 'Content-Type: application/json' 2>&1 || true)"
  LAST_HTTP="$(http_code "$response")"
  LAST_BODY="$(http_body "$response")"
}

create_temp_layout() {
  TMP_PARENT="$(mktemp -d)"
  INSTALL_ROOT="$TMP_PARENT/install"
  TMP_DATA="$TMP_PARENT/data"
  mkdir -p "$INSTALL_ROOT" "$TMP_DATA"
}

install_flapjack() {
  local install_url="${FLAPJACK_INSTALL_URL:-https://install.flapjack.foo}"
  local install_output

  if install_output="$({ curl -fsSL "$install_url" | NO_MODIFY_PATH=1 FLAPJACK_INSTALL="$INSTALL_ROOT" sh; } 2>&1)"; then
    :
  else
    fail "Install Flapjack from ${install_url}" "$install_output"
    return 1
  fi

  BIN="$INSTALL_ROOT/bin/flapjack"
  if [ -x "$BIN" ]; then
    pass "Installed binary exists at isolated FLAPJACK_INSTALL"
    return 0
  fi

  fail "Installed binary exists at isolated FLAPJACK_INSTALL" "$install_output"
  return 1
}

start_server() {
  local wait_helper="$ENGINE_DIR/tests/common/wait_for_flapjack.sh"
  local server_log="$TMP_DATA/server.log"
  local port

  env -u FLAPJACK_ADMIN_KEY FLAPJACK_DATA_DIR="$TMP_DATA" "$BIN" --auto-port >"$server_log" 2>&1 &
  SERVER_PID=$!

  "$wait_helper" --pid "$SERVER_PID" --host 127.0.0.1 --port auto --log-path "$server_log" --retries 60 --interval-seconds 0.5
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$server_log" | head -1)"
  if [ -z "$port" ]; then
    fail 'Extract auto port from first-boot server log' "$(cat "$server_log" 2>/dev/null || true)"
    return 1
  fi

  if [ ! -s "$TMP_DATA/.admin_key" ]; then
    fail 'Read first-boot generated admin key from data/.admin_key' "$(cat "$server_log" 2>/dev/null || true)"
    return 1
  fi

  BASE="http://127.0.0.1:${port}"
  ADMIN_KEY="$(cat "$TMP_DATA/.admin_key")"
  pass 'First boot generated admin key and auto-port server is ready'
}

wait_for_task() {
  local task_id="$1"
  local task_status_body=""
  local attempt

  for ((attempt = 1; attempt <= 100; attempt += 1)); do
    request_json GET "/1/tasks/${task_id}"
    task_status_body="$LAST_BODY"
    if [ "$LAST_HTTP" = "200" ] && json_matches "$LAST_BODY" '.status == "published"'; then
      pass "Task ${task_id} reached published status"
      return 0
    fi
    sleep 0.1
  done

  fail "Task ${task_id} did not reach published status within 10s" "$task_status_body"
  return 1
}

seed_readme_movies() {
  local batch_response
  local task_id

  if batch_response="$(run_readme_curl '/1/indexes/movies/batch' 2>&1)"; then
    LAST_HTTP="$(http_code "$batch_response")"
    LAST_BODY="$(http_body "$batch_response")"
  else
    LAST_HTTP=""
    LAST_BODY="$batch_response"
  fi

  task_id="$(extract_task_id "$LAST_BODY")"
  if [ "$LAST_HTTP" = "200" ] && [ -n "$task_id" ]; then
    pass 'README POST /1/indexes/movies/batch returns numeric taskID'
    wait_for_task "$task_id" || true
    return
  fi

  fail 'README POST /1/indexes/movies/batch returns numeric taskID' "HTTP ${LAST_HTTP} - ${LAST_BODY}"
}

assert_typo_query() {
  local query_response

  if query_response="$(run_readme_curl '/1/indexes/movies/query' 2>&1)"; then
    LAST_HTTP="$(http_code "$query_response")"
    LAST_BODY="$(http_body "$query_response")"
  else
    LAST_HTTP=""
    LAST_BODY="$query_response"
  fi

  if [ "$LAST_HTTP" != "200" ]; then
    fail 'README POST /1/indexes/movies/query returns HTTP 200' "HTTP ${LAST_HTTP} - ${LAST_BODY}"
    return
  fi
  pass 'README POST /1/indexes/movies/query returns HTTP 200'

  if json_matches "$LAST_BODY" '.hits[0].objectID == "1" and .hits[0].title == "The Matrix"'; then
    pass 'Typo-tolerant README query returns The Matrix as top hit'
    return
  fi

  fail 'Typo-tolerant README query returns The Matrix as top hit' "$LAST_BODY"
}

report_summary() {
  printf '\n\033[1mResults: %d/%d passed\033[0m\n' "$TESTS_PASSED" "$TESTS_RUN"
  if [ "$TESTS_SKIPPED" -gt 0 ]; then
    printf '\033[1;33m%d test(s) skipped\033[0m\n' "$TESTS_SKIPPED"
  fi
  if [ "$TESTS_FAILED" -gt 0 ]; then
    printf '\033[0;31m%d test(s) failed\033[0m\n' "$TESTS_FAILED"
    return 1
  fi
  printf '\033[0;32mAll tests passed\033[0m\n'
  return 0
}

main() {
  echo 'README Quickstart Cold-Install Smoke'
  require_tools
  create_temp_layout

  section 'Cold Install'
  install_flapjack || return 1

  section 'First Boot'
  start_server || return 1

  section 'README API Contract'
  seed_readme_movies
  assert_typo_query

  report_summary
}

main "$@"
