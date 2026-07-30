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

liveness_distribution_rejects_one_fast_probe_and_one_five_second_stall() (
  local fixture_dir
  local fast_output
  fixture_dir="$(mktemp -d)"
  trap 'rm -rf "$fixture_dir"' EXIT

  printf 'health\tok\t10\ncount\tok\t20\nhealth\tok\t30\ncount\tok\t40\n' \
    >"$fixture_dir/fast.tsv"
  fast_output="$(liveness_distribution "$fixture_dir/fast.tsv" 250 5000)" || {
    fail "all-fast liveness distribution must pass: $fast_output"
  }
  [[ "$fast_output" == *"endpoint=health samples=2 p99_ms=30 max_ms=30 verdict=pass"* ]] || {
    fail "health evidence must report the exact fast distribution: $fast_output"
  }
  [[ "$fast_output" == *"endpoint=count samples=2 p99_ms=40 max_ms=40 verdict=pass"* ]] || {
    fail "count evidence must report the exact fast distribution: $fast_output"
  }
  if liveness_distribution "$fixture_dir/missing.tsv" 250 5000 >/dev/null 2>&1; then
    fail "an unreadable sample file must fail closed"
  fi
  if liveness_distribution "$fixture_dir/fast.tsv" invalid 5000 >/dev/null 2>&1; then
    fail "a nonnumeric p99 limit must fail closed"
  fi
  if liveness_distribution "$fixture_dir/fast.tsv" 250 0 >/dev/null 2>&1; then
    fail "a nonpositive stall limit must fail closed"
  fi

  printf 'health\tok\t10\ncount\tok\t20\nhealth\ttimeout\t5000\n' \
    >"$fixture_dir/stall.tsv"
  if liveness_distribution "$fixture_dir/stall.tsv" 250 5000 >/dev/null 2>&1; then
    fail "one five-second stall must fail the liveness distribution"
  fi

  printf 'health\tok\t5000\ncount\tok\t20\n' >"$fixture_dir/ok_at_stall_limit.tsv"
  printf 'health\tok\t251\ncount\tok\t20\n' >"$fixture_dir/p99_over_limit.tsv"
  printf 'health\ttimeout\t20\ncount\tok\t20\n' >"$fixture_dir/timeout_without_stall.tsv"
  : >"$fixture_dir/empty.tsv"
  printf 'health\tok\t10\n' >"$fixture_dir/missing_endpoint.tsv"
  printf 'search\tok\t10\ncount\tok\t20\n' >"$fixture_dir/unknown_endpoint.tsv"
  printf 'health\tok\t10\ncount\tunknown\t20\n' >"$fixture_dir/unknown_status.tsv"
  printf 'health\tok\t10\textra\ncount\tok\t20\n' >"$fixture_dir/malformed.tsv"
  printf 'health\tok\tfast\ncount\tok\t20\n' >"$fixture_dir/nonnumeric.tsv"

  local invalid_fixture
  for invalid_fixture in \
    ok_at_stall_limit \
    p99_over_limit \
    timeout_without_stall \
    empty \
    missing_endpoint \
    unknown_endpoint \
    unknown_status \
    malformed \
    nonnumeric; do
    if liveness_distribution "$fixture_dir/${invalid_fixture}.tsv" 250 5000 \
      >/dev/null 2>&1; then
      fail "invalid liveness distribution unexpectedly passed: $invalid_fixture"
    fi
  done

  printf '%s\n' "$fast_output"
)

liveness_case_failed=0
liveness_case_output=""
echo "RUN: liveness_distribution_rejects_one_fast_probe_and_one_five_second_stall"
if ! liveness_case_output="$(
  liveness_distribution_rejects_one_fast_probe_and_one_five_second_stall 2>&1
)"; then
  liveness_case_failed=1
fi

fixed_count() {
  printf '0\n'
}

stall_output=""
original_index_doc_count="$(declare -f index_doc_count)"
eval "$(declare -f fixed_count | sed '1s/fixed_count/index_doc_count/')"
if stall_output="$(wait_for_count_or_stall "http://fixture.invalid" "fixed_index" 1 1 2>&1)"; then
  fail "fixed-count stub should trigger the anti-stall guard"
fi
[[ "$stall_output" == *"STALL"* ]] || fail "anti-stall failure must contain STALL: $stall_output"
eval "$original_index_doc_count"

fixture_port=$((20000 + ($$ % 20000)))
fixture_base_url="http://127.0.0.1:${fixture_port}"
fixture_ready="$(mktemp)"
node -e '
const http = require("node:http");
const fs = require("node:fs");
const port = Number(process.argv[1]);
const ready = process.argv[2];
const server = http.createServer((request, response) => {
  if (request.url.startsWith("/1/indexes/")) {
    // The July 25 count path was a full search. Keep it blocked long enough
    // that any accidental dependency exceeds the fixture request timeout.
    setTimeout(() => {
      response.writeHead(200, {"content-type": "application/json"});
      response.end(JSON.stringify({nbHits: 999}));
    }, 5000);
    return;
  }
  const encodedIndex = request.url.split("/").pop();
  const index = decodeURIComponent(encodedIndex);
  const fixtures = {
    live: {documents_count: [{t: 1, v: 41}, {t: 2, v: 42}]},
    empty: {documents_count: []},
    missing: {},
    decimal: {documents_count: [{t: 1, v: 42.5}]},
  };
  const body = fixtures[index] ?? {documents_count: []};
  response.writeHead(200, {"content-type": "application/json"});
  response.end(JSON.stringify(body));
});
server.listen(port, "127.0.0.1", () => fs.writeFileSync(ready, "ready"));
' "$fixture_port" "$fixture_ready" &
fixture_pid=$!
cleanup_fixture() {
  kill "$fixture_pid" 2>/dev/null || true
  wait "$fixture_pid" 2>/dev/null || true
  rm -f "$fixture_ready"
}
trap cleanup_fixture EXIT

for _ in $(seq 1 100); do
  [[ -s "$fixture_ready" ]] && break
  sleep 0.02
done
[[ -s "$fixture_ready" ]] || fail "count HTTP fixture did not become ready"

COUNT_REQUEST_TIMEOUT_SECONDS=1
live_count="$(index_doc_count "$fixture_base_url" "live")" || {
  fail "usage-backed count must remain live while full search is blocked"
}
[[ "$live_count" == "42" ]] || fail "usage count expected latest value 42, got $live_count"

for invalid_index in empty missing decimal; do
  if index_doc_count "$fixture_base_url" "$invalid_index" >/dev/null 2>&1; then
    fail "invalid usage count fixture unexpectedly passed: $invalid_index"
  fi
done

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

if ((liveness_case_failed)); then
  fail "liveness_distribution_rejects_one_fast_probe_and_one_five_second_stall: $liveness_case_output"
fi

printf '%s\n' "$liveness_case_output"
echo "PASS: liveness uses the live usage gauge and fails against malformed, fixed-count, and missing-sentinel controls"
