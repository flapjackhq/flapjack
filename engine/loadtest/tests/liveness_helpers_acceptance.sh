#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# shellcheck source=../lib/loadtest_shell_helpers.sh
source "$LOADTEST_DIR/lib/loadtest_shell_helpers.sh"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

LOADTEST_AUTH_HEADERS=()
COUNT_POLL_INTERVAL_SECONDS=0.05

index_doc_count() {
  printf '0\n'
}

stall_output=""
if stall_output="$(wait_for_count_or_stall "http://fixture.invalid" "fixed_index" 1 1 2>&1)"; then
  fail "fixed-count stub should trigger the anti-stall guard"
fi
[[ "$stall_output" == *"STALL"* ]] || fail "anti-stall failure must contain STALL: $stall_output"

sentinel_search_response() {
  printf '%s\n' '{"nbHits":0,"hits":[]}'
}

sentinel_output=""
if sentinel_output="$(assert_sentinels_top1 "http://fixture.invalid" "missing_sentinel_index" "10000" 2>&1)"; then
  fail "missing-sentinel stub should trigger the rank-1 correctness guard"
fi
[[ "$sentinel_output" == *"ranked #1"* ]] || {
  fail "missing-sentinel failure must explain the rank-1 contract: $sentinel_output"
}

echo "PASS: liveness helpers fail against fixed-count and missing-sentinel stubs"
