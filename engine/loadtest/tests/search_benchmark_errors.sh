#!/usr/bin/env bash
# Shell-level error-path tests for search_benchmark.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SEARCH_SCRIPT="$LOADTEST_DIR/search_benchmark.sh"
TEST_PASS=0
TEST_FAIL=0

assert_exit_nonzero() {
  local test_name="$1"
  shift
  local output
  if output=$("$@" 2>&1); then
    echo "FAIL: $test_name — expected non-zero exit, got 0"
    echo "  output: $output"
    TEST_FAIL=$((TEST_FAIL + 1))
    return
  fi
  echo "PASS: $test_name"
  TEST_PASS=$((TEST_PASS + 1))
}

assert_output_contains() {
  local test_name="$1"
  local expected="$2"
  shift 2
  local output
  output=$("$@" 2>&1) || true
  if [[ "$output" == *"$expected"* ]]; then
    echo "PASS: $test_name"
    TEST_PASS=$((TEST_PASS + 1))
  else
    echo "FAIL: $test_name — expected output to contain '$expected'"
    echo "  got: $output"
    TEST_FAIL=$((TEST_FAIL + 1))
  fi
}

assert_output_equals() {
  local test_name="$1"
  local expected="$2"
  shift 2
  local output
  if ! output=$("$@" 2>&1); then
    echo "FAIL: $test_name — command exited non-zero"
    echo "  output: $output"
    TEST_FAIL=$((TEST_FAIL + 1))
    return
  fi
  if [[ "$output" == "$expected" ]]; then
    echo "PASS: $test_name"
    TEST_PASS=$((TEST_PASS + 1))
  else
    echo "FAIL: $test_name — expected '$expected'"
    echo "  got: $output"
    TEST_FAIL=$((TEST_FAIL + 1))
  fi
}

make_sourceable_script() {
  local dest="$1"
  sed \
    -e "s|^SCRIPT_DIR=.*|SCRIPT_DIR=\"$LOADTEST_DIR\"|" \
    -e 's|^main "\$@"$|:|' \
    "$SEARCH_SCRIPT" > "$dest"
  chmod +x "$dest"
}

write_fixture_helpers() {
  local dest="$1"
  cat > "$dest" <<'HELPERS'
#!/usr/bin/env bash
set -euo pipefail

require_loadtest_commands() { :; }
load_shared_loadtest_config() {
  FLAPJACK_BASE_URL="http://fixture.local"
  FLAPJACK_BENCHMARK_INDEX="benchmark_100k"
  FLAPJACK_TASK_MAX_ATTEMPTS=1
  FLAPJACK_TASK_POLL_INTERVAL_SECONDS=0
}
initialize_loadtest_auth_headers() { LOADTEST_AUTH_HEADERS=(); }
loadtest_encode_path_component() { printf '%s' "$1"; }
loadtest_http_request() {
  local method="$1"
  local path="$2"

  if [[ "$method" == "GET" && "$path" == "/1/indexes" ]]; then
    if [[ -n "${SEARCH_FIXTURE_INDEXES_FILE:-}" ]]; then
      cat "$SEARCH_FIXTURE_INDEXES_FILE"
    else
      printf '%s' '{"items":[{"name":"benchmark_100k","entries":100000}]}'
    fi
    return 0
  fi

  printf '%s' '{"hits":[]}'
}
loadtest_get_index_doc_count() {
  local index_name="$1"
  local response
  response="$(loadtest_http_request GET "/1/indexes" "" "200")"
  jq -r --arg name "$index_name" \
    '(.items // []) | map(select(.name == $name)) | .[0].entries // 0' \
    <<<"$response"
}
loadtest_index_exists() {
  local index_name="$1"
  local response
  response="$(loadtest_http_request GET "/1/indexes" "" "200")"
  jq -e --arg name "$index_name" \
    '(.items // []) | any(.name == $name)' \
    <<<"$response" >/dev/null
}
HELPERS
  chmod +x "$dest"
}

run_main_with_fixture() {
  local sourceable_script="$1"
  local helper_script="$2"
  local indexes_json_file="$3"
  local catalog_mode="$4"

  SEARCH_FIXTURE_INDEXES_FILE="$indexes_json_file" bash -lc '
    source "$1"
    LOADTEST_HELPERS="$2"
    RESULTS_BASE_DIR="$(dirname "$1")/results"
    fixture_mode="$3"

    if [[ "$fixture_mode" == "bad-json" ]]; then
      emit_query_catalog_json() {
        printf "%s" "not-json"
      }
    elif [[ "$fixture_mode" == "empty-catalog" ]]; then
      emit_query_catalog_json() {
        printf "%s" "{\"text\":[],\"typo\":[],\"multi_word\":[],\"facet\":[],\"filter\":[],\"geo\":[],\"highlight\":[]}"
      }
    fi

    run_single_search_request() {
      if [[ "$fixture_mode" == "request-fail" ]]; then
        return 1
      fi
      printf "17"
    }

    main
  ' bash "$sourceable_script" "$helper_script" "$catalog_mode"
}

run_actual_search_request_with_curl_fixture() {
  local sourceable_script="$1"
  local fixture_bin_dir="$2"
  local curl_metrics="$3"

  PATH="$fixture_bin_dir:$PATH" CURL_FIXTURE_METRICS="$curl_metrics" bash -c '
    source "$1"
    FLAPJACK_BASE_URL="http://fixture.local"
    LOADTEST_AUTH_HEADERS=()
    run_single_search_request "/1/indexes/fixture/query" "{\"query\":\"fixture\"}"
  ' bash "$sourceable_script"
}

test_search_latency_uses_curl_transport_time() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local sourceable_script="$tmp_dir/sourceable_search_benchmark.sh"
  local fixture_bin_dir="$tmp_dir/bin"

  make_sourceable_script "$sourceable_script"
  mkdir -p "$fixture_bin_dir"
  cat > "$fixture_bin_dir/curl" <<'CURL_FIXTURE'
#!/usr/bin/env bash
set -euo pipefail

response_file=""
while [[ $# -gt 0 ]]; do
  if [[ "$1" == "-o" ]]; then
    response_file="$2"
    shift 2
    continue
  fi
  shift
done

[[ -n "$response_file" ]]
printf '%s' '{"hits":[{"objectID":"fixture"}]}' > "$response_file"
printf '%b' "$CURL_FIXTURE_METRICS"
CURL_FIXTURE
  chmod +x "$fixture_bin_dir/curl"

  assert_output_equals \
    "per-request latency uses curl time_total without process startup overhead" \
    "12.345" \
    run_actual_search_request_with_curl_fixture \
    "$sourceable_script" "$fixture_bin_dir" '200\t0.012345'

  assert_exit_nonzero \
    "malformed curl time_total fails closed" \
    run_actual_search_request_with_curl_fixture \
    "$sourceable_script" "$fixture_bin_dir" '200\tnot-a-duration'

  assert_output_contains \
    "malformed curl time_total error is clear" \
    "invalid curl time_total" \
    run_actual_search_request_with_curl_fixture \
    "$sourceable_script" "$fixture_bin_dir" '200\tnot-a-duration'

  rm -rf "$tmp_dir"
}

test_missing_index_fails() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local sourceable_script="$tmp_dir/sourceable_search_benchmark.sh"
  local fixture_helpers="$tmp_dir/fixture_helpers.sh"
  local indexes_file="$tmp_dir/indexes.json"

  make_sourceable_script "$sourceable_script"
  write_fixture_helpers "$fixture_helpers"

  local missing_index_json='{"items":[{"name":"different_index","entries":100000}]}'
  printf '%s' "$missing_index_json" > "$indexes_file"

  assert_exit_nonzero \
    "fails when benchmark_100k index is missing" \
    run_main_with_fixture "$sourceable_script" "$fixture_helpers" "$indexes_file" "normal"

  assert_output_contains \
    "missing index error message is clear" \
    "index \"benchmark_100k\" was not found" \
    run_main_with_fixture "$sourceable_script" "$fixture_helpers" "$indexes_file" "normal"

  rm -rf "$tmp_dir"
}

test_zero_doc_index_fails() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local sourceable_script="$tmp_dir/sourceable_search_benchmark.sh"
  local fixture_helpers="$tmp_dir/fixture_helpers.sh"
  local indexes_file="$tmp_dir/indexes.json"

  make_sourceable_script "$sourceable_script"
  write_fixture_helpers "$fixture_helpers"

  local zero_doc_json='{"items":[{"name":"benchmark_100k","entries":0}]}'
  printf '%s' "$zero_doc_json" > "$indexes_file"

  assert_exit_nonzero \
    "fails when benchmark_100k has zero documents" \
    run_main_with_fixture "$sourceable_script" "$fixture_helpers" "$indexes_file" "normal"

  assert_output_contains \
    "zero-doc error message is clear" \
    "contains 0 documents" \
    run_main_with_fixture "$sourceable_script" "$fixture_helpers" "$indexes_file" "normal"

  rm -rf "$tmp_dir"
}

test_bad_query_catalog_fails() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local sourceable_script="$tmp_dir/sourceable_search_benchmark.sh"
  local fixture_helpers="$tmp_dir/fixture_helpers.sh"
  local indexes_file="$tmp_dir/indexes.json"

  make_sourceable_script "$sourceable_script"
  write_fixture_helpers "$fixture_helpers"

  local valid_index_json='{"items":[{"name":"benchmark_100k","entries":100000}]}'
  printf '%s' "$valid_index_json" > "$indexes_file"

  assert_exit_nonzero \
    "fails when query catalog helper output is invalid" \
    run_main_with_fixture "$sourceable_script" "$fixture_helpers" "$indexes_file" "bad-json"

  assert_output_contains \
    "bad catalog error message is clear" \
    "query catalog helper returned unexpected output" \
    run_main_with_fixture "$sourceable_script" "$fixture_helpers" "$indexes_file" "bad-json"

  rm -rf "$tmp_dir"
}

test_empty_query_catalog_fails() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local sourceable_script="$tmp_dir/sourceable_search_benchmark.sh"
  local fixture_helpers="$tmp_dir/fixture_helpers.sh"
  local indexes_file="$tmp_dir/indexes.json"

  make_sourceable_script "$sourceable_script"
  write_fixture_helpers "$fixture_helpers"

  local valid_index_json='{"items":[{"name":"benchmark_100k","entries":100000}]}'
  printf '%s' "$valid_index_json" > "$indexes_file"

  assert_exit_nonzero \
    "fails when query catalog helper returns empty query arrays" \
    run_main_with_fixture "$sourceable_script" "$fixture_helpers" "$indexes_file" "empty-catalog"

  assert_output_contains \
    "empty catalog error message is clear" \
    "query catalog helper returned unexpected output" \
    run_main_with_fixture "$sourceable_script" "$fixture_helpers" "$indexes_file" "empty-catalog"

  rm -rf "$tmp_dir"
}

test_search_request_failure_fails() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local sourceable_script="$tmp_dir/sourceable_search_benchmark.sh"
  local fixture_helpers="$tmp_dir/fixture_helpers.sh"
  local indexes_file="$tmp_dir/indexes.json"

  make_sourceable_script "$sourceable_script"
  write_fixture_helpers "$fixture_helpers"

  local valid_index_json='{"items":[{"name":"benchmark_100k","entries":100000}]}'
  printf '%s' "$valid_index_json" > "$indexes_file"

  assert_exit_nonzero \
    "fails when a search request returns an error" \
    run_main_with_fixture "$sourceable_script" "$fixture_helpers" "$indexes_file" "request-fail"

  assert_output_contains \
    "request failure error message is clear" \
    "search request failed for query type" \
    run_main_with_fixture "$sourceable_script" "$fixture_helpers" "$indexes_file" "request-fail"

  rm -rf "$tmp_dir"
}

test_summary_surfaces_name_prefix_percentiles() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local sourceable_script="$tmp_dir/sourceable_search_benchmark.sh"
  local fixture_helpers="$tmp_dir/fixture_helpers.sh"
  local indexes_file="$tmp_dir/indexes.json"

  make_sourceable_script "$sourceable_script"
  write_fixture_helpers "$fixture_helpers"
  printf '%s' '{"items":[{"name":"benchmark_100k","entries":100000}]}' > "$indexes_file"

  assert_output_contains \
    "summary surfaces a distinct name/prefix percentile line" \
    $'name/prefix\t6\t17\t17\t17' \
    run_main_with_fixture "$sourceable_script" "$fixture_helpers" "$indexes_file" "normal"

  rm -rf "$tmp_dir"
}

echo "=== search_benchmark.sh error-path tests ==="
test_missing_index_fails
test_zero_doc_index_fails
test_bad_query_catalog_fails
test_empty_query_catalog_fails
test_search_request_failure_fails
test_summary_surfaces_name_prefix_percentiles
test_search_latency_uses_curl_transport_time

echo ""
echo "Results: $TEST_PASS passed, $TEST_FAIL failed"
if [[ $TEST_FAIL -gt 0 ]]; then
  exit 1
fi
echo "All error-path tests passed."
