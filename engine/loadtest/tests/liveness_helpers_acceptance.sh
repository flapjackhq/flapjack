#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2016,SC2034,SC2329
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$(cd "$LOADTEST_DIR/.." && pwd)"
REPO_ROOT="$(cd "$ENGINE_DIR/.." && pwd)"

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

liveness_sampler_lifecycle_uses_caller_owned_pid() {
  local fixture_dir
  local sample_path
  local fixture_base_url="http://fixture.invalid"
  local fixture_index_name="live index"
  local watch_pid
  local sampler_pid=""
  local pid_var_name=""
  local current_sampler_pid=""
  local started_sampler_pid
  local sampler_child_pid=""
  local first_rows

  fixture_dir="$(mktemp -d)"
  sample_path="$fixture_dir/liveness_samples.tsv"
  sleep 30 &
  watch_pid=$!

  cleanup_lifecycle_fixture() {
    stop_liveness_sampler sampler_pid 2>/dev/null || true
    kill "$watch_pid" 2>/dev/null || true
    wait "$watch_pid" 2>/dev/null || true
    rm -rf "$fixture_dir"
  }
  trap cleanup_lifecycle_fixture RETURN

  sample_liveness_endpoint() {
    local endpoint="$1"
    local url="$2"

    case "${endpoint} ${url}" in
      "health ${fixture_base_url}/health")
        printf 'health\tok\t11\n'
        ;;
      "count ${fixture_base_url}/1/usage/documents_count/live%20index")
        printf 'count\tok\t22\n'
        ;;
      *)
        fail "unexpected liveness probe: endpoint=${endpoint} url=${url}"
        ;;
    esac
  }

  start_liveness_sampler \
    sampler_pid \
    "$fixture_base_url" \
    "$fixture_index_name" \
    "$sample_path" \
    "$watch_pid" \
    30
  [[ -n "$sampler_pid" ]] || fail "start_liveness_sampler must assign the caller PID variable"
  started_sampler_pid="$sampler_pid"
  jobs -pr | grep -Fxq "$started_sampler_pid" || \
    fail "sampler PID must be owned by the calling shell job table"

  for _ in $(seq 1 100); do
    if [[ "$(wc -l <"$sample_path" 2>/dev/null || printf '0')" -ge 2 ]]; then
      break
    fi
    sleep 0.02
  done
  first_rows="$(sed -n '1,2p' "$sample_path")"
  [[ "$first_rows" == $'health\tok\t11\ncount\tok\t22' ]] || \
    fail "sampler must write exact health/count rows to the requested file, got: ${first_rows}"

  for _ in $(seq 1 100); do
    sampler_child_pid="$(
      ps -eo pid=,ppid= |
        awk -v parent_pid="$started_sampler_pid" '$2 == parent_pid { print $1; exit }'
    )"
    [[ -n "$sampler_child_pid" ]] && break
    sleep 0.02
  done
  [[ -n "$sampler_child_pid" ]] || fail "sampler must have an active child before shutdown"
  kill -0 "$sampler_child_pid" 2>/dev/null || \
    fail "captured sampler child must be alive before shutdown"

  stop_liveness_sampler sampler_pid
  [[ -z "$sampler_pid" ]] || fail "stop_liveness_sampler must clear the caller PID variable"
  ! jobs -pr | grep -Fxq "$started_sampler_pid" || \
    fail "stop_liveness_sampler must reap the sampler job"
  ! kill -0 "$sampler_child_pid" 2>/dev/null || \
    fail "stop_liveness_sampler must terminate the sampler's active child"

  start_liveness_sampler \
    pid_var_name \
    "$fixture_base_url" \
    "$fixture_index_name" \
    "$sample_path" \
    "$watch_pid" \
    30
  [[ -n "$pid_var_name" ]] || \
    fail "start_liveness_sampler must assign caller variable named pid_var_name"
  started_sampler_pid="$pid_var_name"
  stop_liveness_sampler pid_var_name
  [[ -z "$pid_var_name" ]] || \
    fail "stop_liveness_sampler must clear caller variable named pid_var_name"
  ! jobs -pr | grep -Fxq "$started_sampler_pid" || \
    fail "stop_liveness_sampler must reap sampler job for pid_var_name"

  start_liveness_sampler \
    current_sampler_pid \
    "$fixture_base_url" \
    "$fixture_index_name" \
    "$sample_path" \
    "$watch_pid" \
    30
  [[ -n "$current_sampler_pid" ]] || \
    fail "start_liveness_sampler must assign caller variable named current_sampler_pid"
  started_sampler_pid="$current_sampler_pid"
  stop_liveness_sampler current_sampler_pid
  [[ -z "$current_sampler_pid" ]] || \
    fail "stop_liveness_sampler must clear caller variable named current_sampler_pid"
  ! jobs -pr | grep -Fxq "$started_sampler_pid" || \
    fail "stop_liveness_sampler must reap sampler job for current_sampler_pid"
}

liveness_sampler_output=""
echo "RUN: liveness_sampler_lifecycle_uses_caller_owned_pid"
if ! liveness_sampler_output="$(
  liveness_sampler_lifecycle_uses_caller_owned_pid 2>&1
)"; then
  fail "liveness_sampler_lifecycle_uses_caller_owned_pid: $liveness_sampler_output"
fi

start_health_owner_http_fixture() {
  local fixture_port_path="$1"
  local fixture_pid_variable_name="$2"
  local fixture_process_pid

  node -e '
const fs = require("node:fs");
const http = require("node:http");
const portPath = process.argv[1];
const server = http.createServer((request, response) => {
  if (request.url !== "/health") {
    response.writeHead(404);
    response.end();
    return;
  }
  response.writeHead(200);
  response.end("ok");
});
server.listen(0, "127.0.0.1", () => {
  fs.writeFileSync(portPath, String(server.address().port));
});
' "$fixture_port_path" &
  fixture_process_pid=$!
  printf -v "$fixture_pid_variable_name" '%s' "$fixture_process_pid"

  for _ in $(seq 1 100); do
    [[ -s "$fixture_port_path" ]] && return 0
    kill -0 "$fixture_process_pid" 2>/dev/null || break
    sleep 0.02
  done
  fail "health ownership fixture did not publish its port"
}

assert_partial_health_owner_arguments_fail() {
  local fixture_base_url="$1"
  local launched_server_pid="$2"
  local server_log_path="$3"
  local fixture_bind_addr="$4"
  local partial_owner_case
  local partial_server_log_path
  local partial_bind_addr
  local output

  for partial_owner_case in log_only bind_only; do
    partial_server_log_path=""
    partial_bind_addr=""
    if [[ "$partial_owner_case" == "log_only" ]]; then
      partial_server_log_path="$server_log_path"
    else
      partial_bind_addr="$fixture_bind_addr"
    fi
    if output="$(
      wait_for_loadtest_health \
        "$fixture_base_url" \
        "$launched_server_pid" \
        2 \
        0.01 \
        "$partial_server_log_path" \
        "$partial_bind_addr" 2>&1
    )"; then
      fail "health ownership arguments must be supplied together: $partial_owner_case"
    fi
    [[ "$output" == *"needs server log path and bind address"* ]] || {
      fail "partial ownership arguments must name the paired contract: $partial_owner_case: $output"
    }
  done
}

assert_health_owner_contract() {
  local fixture_base_url="$1"
  local launched_server_pid="$2"
  local server_log_path="$3"
  local fixture_bind_addr="$4"
  local caller_label="$5"
  shift 5

  local matching_owner_output
  local output
  local resolved_owner_args=()
  local owner_arg

  for owner_arg in "$@"; do
    case "$owner_arg" in
      __fixture_server_log_path__)
        resolved_owner_args+=("$server_log_path")
        ;;
      __fixture_bind_addr__)
        resolved_owner_args+=("$fixture_bind_addr")
        ;;
      *)
        resolved_owner_args+=("$owner_arg")
        ;;
    esac
  done

  printf 'Local: http://%s0\n' "$fixture_bind_addr" >"$server_log_path"
  if output="$(
    wait_for_loadtest_health \
      "$fixture_base_url" \
      "$launched_server_pid" \
      "${resolved_owner_args[@]}" 2>&1
  )"; then
    fail "${caller_label}: health readiness must reject a foreign listener's HTTP 200"
  fi
  [[ "$output" == *"did not confirm ownership"* ]] || {
    fail "${caller_label}: foreign-listener failure must say did not confirm ownership: $output"
  }

  printf 'Local: http://%s\n' "$fixture_bind_addr" >"$server_log_path"
  if matching_owner_output="$(
    wait_for_loadtest_health \
      "$fixture_base_url" \
      "$launched_server_pid" \
      "${resolved_owner_args[@]}" 2>&1
  )"; then
    :
  else
    fail "${caller_label}: matching owned-log banner must accept the HTTP 200: $matching_owner_output"
  fi

  assert_partial_health_owner_arguments_fail \
    "$fixture_base_url" "$launched_server_pid" "$server_log_path" "$fixture_bind_addr"

  echo "PASS: foreign HTTP 200 rejected with did not confirm ownership"
  echo "PASS: matching Local: http://${fixture_bind_addr} banner accepted"
}

assert_ipv6_loopback_base_url_contract() {
  local bind_addr

  bind_addr="$(derive_bind_addr_from_base_url 'http://[::1]:7700')" || {
    fail "IPv6 loopback base URL must be accepted"
  }
  [[ "$bind_addr" == "[::1]:7700" ]] || {
    fail "IPv6 loopback base URL must preserve a bindable bracketed address, got ${bind_addr}"
  }
}

with_health_owner_fixture() (
  local assertion_function="$1"
  shift
  local fixture_dir
  local fixture_port_path
  local server_log_path
  local fixture_pid
  local launched_server_pid
  local fixture_port
  local fixture_bind_addr
  local fixture_base_url
  local fixture_status_code

  fixture_dir="$(mktemp -d)"
  fixture_port_path="$fixture_dir/port"
  server_log_path="$fixture_dir/server.log"

  start_health_owner_http_fixture "$fixture_port_path" fixture_pid
  sleep 30 &
  launched_server_pid=$!

  cleanup_health_owner_fixture() {
    kill "$fixture_pid" "$launched_server_pid" 2>/dev/null || true
    wait "$fixture_pid" "$launched_server_pid" 2>/dev/null || true
    rm -rf "$fixture_dir"
  }
  trap cleanup_health_owner_fixture EXIT

  fixture_port="$(cat "$fixture_port_path")"
  [[ "$fixture_port" =~ ^[0-9]+$ ]] || {
    fail "health ownership fixture published a non-numeric port: $fixture_port"
  }
  [[ "$fixture_port" != "7700" ]] || {
    fail "health ownership fixture must use a private port, never the shared loadtest port 7700"
  }
  fixture_bind_addr="127.0.0.1:${fixture_port}"
  fixture_base_url="http://${fixture_bind_addr}"
  fixture_status_code="$(
    curl -sS -o /dev/null -w '%{http_code}' --max-time 1 "${fixture_base_url}/health"
  )" || fail "health ownership fixture was not reachable"
  [[ "$fixture_status_code" == "200" ]] || {
    fail "health ownership fixture expected HTTP 200, got $fixture_status_code"
  }
  kill -0 "$launched_server_pid" 2>/dev/null || fail "launched-server decoy PID must be live"
  [[ "$launched_server_pid" != "$fixture_pid" ]] || {
    fail "launched-server decoy PID must not be the foreign listener's own PID"
  }

  "$assertion_function" \
    "$fixture_base_url" \
    "$launched_server_pid" \
    "$server_log_path" \
    "$fixture_bind_addr" \
    "$@"
)

loadtest_health_requires_launched_server_log_owner() {
  with_health_owner_fixture \
    assert_health_owner_contract \
    "generic readiness owner contract" \
    2 \
    0.01 \
    __fixture_server_log_path__ \
    __fixture_bind_addr__
}

echo "RUN: loadtest_health_requires_launched_server_log_owner"
if ! owner_output="$(
  loadtest_health_requires_launched_server_log_owner 2>&1
)"; then
  fail "loadtest_health_requires_launched_server_log_owner: $owner_output"
fi
printf '%s\n' "$owner_output"

echo "RUN: assert_ipv6_loopback_base_url_contract"
assert_ipv6_loopback_base_url_contract

grep -Fq 'wait_for_loadtest_health "$BASE_URL" "$SERVER_PID" 300 0.1 "$server_log_path" "$bind_addr"' \
  "$LOADTEST_DIR/scale_ladder.sh" || {
  fail "scale_ladder.sh must pass server log path and bind address to the health owner guard"
}

# ---------------------------------------------------------------------------
# PL-15 measured denominator — SINGLE OWNER. Do not restate these counts in any
# other scenario, script, or doc; reference this block instead.
#
# Sweep: every occurrence of the identifier `wait_for_loadtest_health` in shell
# sources across the repository (excluding generated/vendor directories and this
# test file, which is the observer rather than a caller). Reproduce with:
#   grep -rn --include='*.sh' --exclude-dir=target --exclude-dir=node_modules \
#     --exclude-dir=.git -I -F 'wait_for_loadtest_health' .
#
#   classification            | count | sites (path:line at the time of writing)
#   -------------------------|-------|------------------------------------------
#   helper definition        |   1   | engine/loadtest/lib/loadtest_shell_helpers.sh:121
#   helper failure message   |   1   | engine/loadtest/lib/loadtest_shell_helpers.sh:134
#   test stub (not a caller) |   2   | engine/_dev/s/manual-tests/disk_exhaustion_durability_selftest.sh:191,604
#   live caller, both args   |   4   | engine/loadtest/scale_ladder.sh:959
#                            |       | engine/loadtest/tests/scale_ladder_smoke_acceptance.sh:208
#                            |       | engine/loadtest/tests/pl10_saturation_acceptance.sh:273
#                            |       | engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh:179
#   live caller, neither arg |   0   | (none)
#   live caller, exactly one |   0   | (none — the one-arg refusal at
#                            |       |  loadtest_shell_helpers.sh:132-137 is owned by
#                            |       |  assert_partial_health_owner_arguments_fail above)
#   -------------------------|-------|------------------------------------------
#   live callers, total      |   4   |
#
# `assert_wait_for_loadtest_health_denominator` below pins that whole set, so a
# new shell call site anywhere in the repository fails this test instead of silently
# escaping the ownership contract.
# ---------------------------------------------------------------------------

assert_wait_for_loadtest_health_denominator() {
  local both_ownership_argument_caller_count
  local expected_sites
  local live_caller_count
  local neither_ownership_argument_caller_count
  local observed_sites

  # path:trimmed-source-text for each occurrence, sorted. Line numbers are
  # deliberately excluded so unrelated edits above a call site do not churn this
  # guard, while any added, removed, or reshaped call site does.
  expected_sites="$(
    cat <<'EXPECTED_SITES'
engine/_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh:wait_for_loadtest_health "$BASE_URL" "$SERVER_PID" 200 0.1 "$log_path" "$FLAPJACK_BIND_ADDR"
engine/_dev/s/manual-tests/disk_exhaustion_durability_selftest.sh:wait_for_loadtest_health() { return 0; }
engine/_dev/s/manual-tests/disk_exhaustion_durability_selftest.sh:wait_for_loadtest_health() { return 0; }
engine/loadtest/lib/loadtest_shell_helpers.sh:echo "FAIL: wait_for_loadtest_health owner check needs server log path and bind address"
engine/loadtest/lib/loadtest_shell_helpers.sh:wait_for_loadtest_health() {
engine/loadtest/scale_ladder.sh:wait_for_loadtest_health "$BASE_URL" "$SERVER_PID" 300 0.1 "$server_log_path" "$bind_addr"
engine/loadtest/tests/pl10_saturation_acceptance.sh:wait_for_loadtest_health "$FLAPJACK_BASE_URL" "$SERVER_PID" "300" "0.1" "$case_log_path" "$FLAPJACK_BIND_ADDR"
engine/loadtest/tests/scale_ladder_smoke_acceptance.sh:wait_for_loadtest_health "$base_url" "$mutation_server_pid" 300 0.1 "$mutation_server_log_path" "$mutation_bind_addr"
EXPECTED_SITES
  )"

  observed_sites="$(
    cd "$REPO_ROOT" &&
      grep -rn --exclude-dir=target --exclude-dir=node_modules --exclude-dir=.git \
        --include='*.sh' -I -F 'wait_for_loadtest_health' . |
      sed 's|^\./||' |
      grep -v '^engine/loadtest/tests/liveness_helpers_acceptance\.sh:' |
      awk -F: '{ path = $1; sub(/^[^:]*:[0-9]+:[[:space:]]*/, ""); printf "%s:%s\n", path, $0 }' |
      sort
  )"

  [[ "$observed_sites" == "$expected_sites" ]] || {
    fail "$(
      printf '%s\n%s\n%s\n%s\n' \
        "wait_for_loadtest_health denominator changed; update the PL-15 table and the per-caller scenarios together" \
        "--- expected ---" "$expected_sites" \
        "--- observed ---
$observed_sites"
    )"
  }

  live_caller_count="$(grep -c ':wait_for_loadtest_health "' <<<"$expected_sites")"
  both_ownership_argument_caller_count="$(
    grep ':wait_for_loadtest_health "' <<<"$expected_sites" |
      awk '{
        has_log = ($0 ~ /server_log_path|mutation_server_log_path|case_log_path|log_path/)
        has_bind = ($0 ~ /bind_addr|mutation_bind_addr|FLAPJACK_BIND_ADDR/)
        if (has_log && has_bind) {
          count += 1
        }
      } END { print count + 0 }'
  )"
  neither_ownership_argument_caller_count=$((
    live_caller_count - both_ownership_argument_caller_count
  ))

  [[ "$both_ownership_argument_caller_count" == "4" ]] || {
    fail "wait_for_loadtest_health denominator expected 4 both-args callers, got ${both_ownership_argument_caller_count}"
  }
  [[ "$neither_ownership_argument_caller_count" == "0" ]] || {
    fail "wait_for_loadtest_health denominator expected 0 neither-arg callers, got ${neither_ownership_argument_caller_count}"
  }

  echo "PASS: wait_for_loadtest_health denominator pinned at ${live_caller_count} live callers (${both_ownership_argument_caller_count} both-args, ${neither_ownership_argument_caller_count} neither-arg, 0 one-arg)"
}

assert_owned_caller_rejects_foreign_health_200() (
  local caller_label="$1"
  local caller_relative_path="$2"
  local caller_call_line="$3"
  local caller_max_attempts="$4"
  local caller_sleep_seconds="$5"
  shift 5

  local caller_path="$ENGINE_DIR/$caller_relative_path"
  local call_line_occurrences
  local owner_contract_output

  [[ -f "$caller_path" ]] || {
    fail "$caller_label: caller file is missing: $caller_relative_path"
  }
  # Whole-line (whitespace-trimmed) equality, not a substring match: Stage 2
  # appends ownership arguments to this same line, and a substring pin would
  # still match the lengthened line and silently keep reporting green.
  call_line_occurrences="$(
    awk -v expected="$caller_call_line" '
      {
        line = $0
        sub(/^[[:space:]]+/, "", line)
        sub(/[[:space:]]+$/, "", line)
      }
      line == expected { matched += 1 }
      END { print matched + 0 }
    ' "$caller_path"
  )"
  [[ "$call_line_occurrences" == "1" ]] || {
    fail "$caller_label: expected exactly 1 owned health call line in ${caller_relative_path}, found ${call_line_occurrences}; Stage 2 must wire ownership arguments into this caller"
  }

  # Reuse the one fixture/bootstrap and behavioral owner. The last two
  # placeholders are resolved inside the shared fixture so every caller scenario
  # checks the exact same foreign-listener rejection and matching-banner success.
  if owner_contract_output="$(
    with_health_owner_fixture \
      assert_health_owner_contract \
      "$caller_label" \
      "$caller_max_attempts" \
      "$caller_sleep_seconds" \
      __fixture_server_log_path__ \
      __fixture_bind_addr__ 2>&1
  )"; then
    printf '%s\n' "$owner_contract_output"
  else
    fail "$caller_label: foreign-listener owner contract failed: $owner_contract_output"
  fi

  echo "PASS: ${caller_label} (${caller_relative_path}) rejects a foreign HTTP 200 and accepts its matching owned log banner"
)

echo "RUN: assert_wait_for_loadtest_health_denominator"
assert_wait_for_loadtest_health_denominator

echo "RUN: owned_health_callers_reject_a_foreign_health_200"
assert_owned_caller_rejects_foreign_health_200 \
  "scale_ladder_smoke_acceptance" \
  "loadtest/tests/scale_ladder_smoke_acceptance.sh" \
  'wait_for_loadtest_health "$base_url" "$mutation_server_pid" 300 0.1 "$mutation_server_log_path" "$mutation_bind_addr"' \
  300 0.01
assert_owned_caller_rejects_foreign_health_200 \
  "pl10_saturation_acceptance" \
  "loadtest/tests/pl10_saturation_acceptance.sh" \
  'wait_for_loadtest_health "$FLAPJACK_BASE_URL" "$SERVER_PID" "300" "0.1" "$case_log_path" "$FLAPJACK_BIND_ADDR"' \
  300 0.01
assert_owned_caller_rejects_foreign_health_200 \
  "20260730_disk_exhaustion_durability" \
  "_dev/s/manual-tests/20260730_disk_exhaustion_durability.sh" \
  'wait_for_loadtest_health "$BASE_URL" "$SERVER_PID" 200 0.1 "$log_path" "$FLAPJACK_BIND_ADDR"' \
  200 0.01

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
