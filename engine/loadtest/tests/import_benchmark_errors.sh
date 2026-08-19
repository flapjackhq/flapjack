#!/usr/bin/env bash
# Shell-level error-path tests for import_benchmark.sh
# These tests verify that the runner fails correctly for bad inputs
# without needing a live server.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
IMPORT_SCRIPT="$LOADTEST_DIR/import_benchmark.sh"
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

# Create a patched copy of import_benchmark.sh with a custom DATA_DIR but
# keeping SCRIPT_DIR pointing to the real loadtest directory.
make_patched_script() {
  local data_dir_override="$1"
  local dest="$2"
  sed \
    -e "s|^SCRIPT_DIR=.*|SCRIPT_DIR=\"$LOADTEST_DIR\"|" \
    -e "s|^DATA_DIR=.*|DATA_DIR=\"$data_dir_override\"|" \
    "$IMPORT_SCRIPT" > "$dest"
  chmod +x "$dest"
}

make_sourceable_script() {
  local dest="$1"
  sed \
    -e "s|^SCRIPT_DIR=.*|SCRIPT_DIR=\"$LOADTEST_DIR\"|" \
    -e 's|^main "\$@"$|:|' \
    "$IMPORT_SCRIPT" > "$dest"
  chmod +x "$dest"
}

# TODO: Document write_runner_fixture_helpers.
write_runner_fixture_helpers() {
  local dest="$1"
  cat > "$dest" <<'EOF'
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
load_dashboard_seed_settings() { LOADTEST_SETTINGS_JSON='{}'; }
reset_loadtest_index() { :; }
apply_loadtest_index_settings() { :; }
wait_for_loadtest_task_published() { :; }
loadtest_encode_path_component() { printf '%s' "$1"; }
extract_loadtest_numeric_task_id() {
  local response_json="$1"
  jq -er '.taskID | select(type == "number")' <<<"$response_json"
}
loadtest_http_request() {
  local final_entries="${LOADTEST_FAKE_FINAL_ENTRIES:-0}"
  jq -cn --arg name "$FLAPJACK_BENCHMARK_INDEX" --argjson entries "$final_entries" \
    '{items: [{name: $name, entries: $entries}]}'
}
loadtest_get_index_doc_count() {
  local index_name="$1"
  local response
  response="$(loadtest_http_request GET "/1/indexes" "" "200")"
  jq -r --arg name "$index_name" \
    '(.items // []) | map(select(.name == $name)) | .[0].entries // 0' \
    <<<"$response"
}
EOF
  chmod +x "$dest"
}

write_two_request_batch_file() {
  local dest="$1"
  cat > "$dest" <<'JSON'
{
  "requests": [
    { "action": "addObject", "body": { "objectID": "1" } },
    { "action": "addObject", "body": { "objectID": "2" } }
  ]
}
JSON
}

# --- Test 1: Missing data directory ---
test_missing_data_dir() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local test_script="$tmp_dir/test_import.sh"

  make_patched_script "$tmp_dir/nonexistent_data" "$test_script"

  assert_exit_nonzero "rejects missing data directory" bash "$test_script"
  assert_output_contains "error message mentions data directory" "data directory not found" bash "$test_script"

  rm -rf "$tmp_dir"
}

# --- Test 2: Empty data directory (no batch files) ---
test_empty_data_dir() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local empty_data="$tmp_dir/data"
  mkdir -p "$empty_data"
  local test_script="$tmp_dir/test_import.sh"

  make_patched_script "$empty_data" "$test_script"

  assert_exit_nonzero "rejects empty data directory" bash "$test_script"
  assert_output_contains "error message mentions no batch files" "no batch files found" bash "$test_script"

  rm -rf "$tmp_dir"
}

# --- Test 3: Validate taskID extraction rejects non-numeric ---
test_malformed_task_id() {
  # This tests the extract_loadtest_numeric_task_id helper from loadtest_shell_helpers.sh
  source "$LOADTEST_DIR/lib/loadtest_shell_helpers.sh"

  local malformed_responses=(
    '{"taskID": "abc"}'
    '{"taskID": null}'
    '{}'
    '{"taskID": true}'
  )

  local all_passed=true
  for response in "${malformed_responses[@]}"; do
    if extract_loadtest_numeric_task_id "$response" >/dev/null 2>&1; then
      echo "FAIL: extract_loadtest_numeric_task_id should reject: $response"
      all_passed=false
    fi
  done

  if [[ "$all_passed" == "true" ]]; then
    echo "PASS: extract_loadtest_numeric_task_id rejects non-numeric taskID"
    TEST_PASS=$((TEST_PASS + 1))
  else
    TEST_FAIL=$((TEST_FAIL + 1))
  fi

  # Valid numeric taskID should succeed
  local valid_result
  if valid_result="$(extract_loadtest_numeric_task_id '{"taskID": 42}')"; then
    if [[ "$valid_result" == "42" ]]; then
      echo "PASS: extract_loadtest_numeric_task_id accepts valid numeric taskID"
      TEST_PASS=$((TEST_PASS + 1))
    else
      echo "FAIL: extract_loadtest_numeric_task_id returned wrong value: $valid_result"
      TEST_FAIL=$((TEST_FAIL + 1))
    fi
  else
    echo "FAIL: extract_loadtest_numeric_task_id rejected valid taskID"
    TEST_FAIL=$((TEST_FAIL + 1))
  fi
}

# --- Test 4: Validate get_index_doc_count parsing (list_indices response) ---
test_doc_count_parsing() {
  local jq_expr='(.items // []) | map(select(.name == $name)) | .[0].entries // 0'

  # Index present in items array
  local valid_response='{"items": [{"name": "benchmark_100k", "entries": 100000, "dataSize": 42}]}'
  local count
  count="$(jq -r --arg name "benchmark_100k" "$jq_expr" <<< "$valid_response")"
  if [[ "$count" == "100000" ]]; then
    echo "PASS: doc count parsing extracts entries from list_indices items"
    TEST_PASS=$((TEST_PASS + 1))
  else
    echo "FAIL: doc count parsing — expected 100000, got $count"
    TEST_FAIL=$((TEST_FAIL + 1))
  fi

  # Index not present in items array
  local no_match='{"items": [{"name": "other_index", "entries": 5}]}'
  count="$(jq -r --arg name "benchmark_100k" "$jq_expr" <<< "$no_match")"
  if [[ "$count" == "0" ]]; then
    echo "PASS: doc count parsing defaults to 0 when index not found"
    TEST_PASS=$((TEST_PASS + 1))
  else
    echo "FAIL: doc count parsing — expected 0, got $count"
    TEST_FAIL=$((TEST_FAIL + 1))
  fi

  # Empty items array
  local empty_items='{"items": []}'
  count="$(jq -r --arg name "benchmark_100k" "$jq_expr" <<< "$empty_items")"
  if [[ "$count" == "0" ]]; then
    echo "PASS: doc count parsing defaults to 0 on empty items"
    TEST_PASS=$((TEST_PASS + 1))
  else
    echo "FAIL: doc count parsing — expected 0, got $count"
    TEST_FAIL=$((TEST_FAIL + 1))
  fi
}

# --- Test 5: Count batch operations in a payload file ---
test_batch_request_counting() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local sourceable_script="$tmp_dir/sourceable_import.sh"
  local batch_file="$tmp_dir/batch_001.json"

  make_sourceable_script "$sourceable_script"
  cat > "$batch_file" <<'JSON'
{
  "requests": [
    { "action": "addObject", "body": { "objectID": "1" } },
    { "action": "addObject", "body": { "objectID": "2" } },
    { "action": "addObject", "body": { "objectID": "3" } }
  ]
}
JSON

  local count
  count="$(
    bash -lc '
      source "$1"
      count_batch_requests "$2"
    ' bash "$sourceable_script" "$batch_file"
  )"

  if [[ "$count" == "3" ]]; then
    echo "PASS: count_batch_requests counts payload operations"
    TEST_PASS=$((TEST_PASS + 1))
  else
    echo "FAIL: count_batch_requests — expected 3, got $count"
    TEST_FAIL=$((TEST_FAIL + 1))
  fi

  rm -rf "$tmp_dir"
}

# --- Test 6: Final doc-count mismatch warning helper ---
test_doc_count_mismatch_warning() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local sourceable_script="$tmp_dir/sourceable_import.sh"

  make_sourceable_script "$sourceable_script"

  local mismatch_output
  mismatch_output="$(
    bash -lc '
      source "$1"
      set +e
      output="$(warn_if_doc_count_mismatch 1000 995)"
      status=$?
      printf "%s\nSTATUS:%s\n" "$output" "$status"
    ' bash "$sourceable_script"
  )"

  if [[ "$mismatch_output" == *"WARN: document count mismatch — expected 1000, got 995"* && "$mismatch_output" == *"STATUS:1"* ]]; then
    echo "PASS: warn_if_doc_count_mismatch flags mismatched totals"
    TEST_PASS=$((TEST_PASS + 1))
  else
    echo "FAIL: warn_if_doc_count_mismatch did not flag mismatch"
    echo "  got: $mismatch_output"
    TEST_FAIL=$((TEST_FAIL + 1))
  fi

  local match_output
  match_output="$(
    bash -lc '
      source "$1"
      set +e
      output="$(warn_if_doc_count_mismatch 1000 1000)"
      status=$?
      printf "%sSTATUS:%s\n" "$output" "$status"
    ' bash "$sourceable_script"
  )"

  if [[ "$match_output" == "STATUS:0" ]]; then
    echo "PASS: warn_if_doc_count_mismatch stays quiet on matching totals"
    TEST_PASS=$((TEST_PASS + 1))
  else
    echo "FAIL: warn_if_doc_count_mismatch should be silent on matching totals"
    echo "  got: $match_output"
    TEST_FAIL=$((TEST_FAIL + 1))
  fi

  rm -rf "$tmp_dir"
}

# --- Test 7: Runner path handles malformed taskID response ---
test_runner_malformed_task_id_path() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local sourceable_script="$tmp_dir/sourceable_import.sh"
  local fixture_helpers="$tmp_dir/fixture_helpers.sh"
  local data_dir="$tmp_dir/data"
  local results_dir="$tmp_dir/results"
  mkdir -p "$data_dir" "$results_dir"

  make_sourceable_script "$sourceable_script"
  write_runner_fixture_helpers "$fixture_helpers"
  write_two_request_batch_file "$data_dir/batch_001.json"

  local output
  if ! output="$(
    bash -lc '
      source "$1"
      LOADTEST_HELPERS="$2"
      DATA_DIR="$3"
      RESULTS_BASE_DIR="$4"
      TASK_RESPONSE_JSON="$5"
      export LOADTEST_FAKE_FINAL_ENTRIES="$6"
      post_batch_file() {
        local body_file="$3"
        printf "%s" "$TASK_RESPONSE_JSON" > "$body_file"
        printf "200"
      }
      main
    ' bash "$sourceable_script" "$fixture_helpers" "$data_dir" "$results_dir" '{"taskID":"not-a-number"}' "0"
  )"; then
    echo "FAIL: runner should complete when batch taskID is malformed"
    TEST_FAIL=$((TEST_FAIL + 1))
    rm -rf "$tmp_dir"
    return
  fi

  if [[ "$output" != *"WARN: batch batch_001.json returned non-numeric or missing taskID — skipping"* ]]; then
    echo "FAIL: malformed taskID runner path warning missing"
    echo "  got: $output"
    TEST_FAIL=$((TEST_FAIL + 1))
    rm -rf "$tmp_dir"
    return
  fi

  local artifact_file
  artifact_file="$(awk '/INFO: artifact written to / { print $NF }' <<< "$output" | tail -n 1)"
  if [[ -z "$artifact_file" || ! -f "$artifact_file" ]]; then
    echo "FAIL: malformed taskID runner path did not write artifact"
    TEST_FAIL=$((TEST_FAIL + 1))
    rm -rf "$tmp_dir"
    return
  fi

  local error_count latency_count
  error_count="$(jq -r '.errorCount' "$artifact_file")"
  latency_count="$(jq -r '.latency.count' "$artifact_file")"
  if [[ "$error_count" == "1" && "$latency_count" == "0" ]]; then
    echo "PASS: runner malformed taskID path increments errors and records zero successful latency samples"
    TEST_PASS=$((TEST_PASS + 1))
  else
    echo "FAIL: malformed taskID artifact mismatch (errorCount=$error_count latency.count=$latency_count)"
    TEST_FAIL=$((TEST_FAIL + 1))
  fi

  rm -rf "$tmp_dir"
}

# --- Test 8: Runner path flags final count mismatch ---
test_runner_final_count_mismatch_path() {
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local sourceable_script="$tmp_dir/sourceable_import.sh"
  local fixture_helpers="$tmp_dir/fixture_helpers.sh"
  local data_dir="$tmp_dir/data"
  local results_dir="$tmp_dir/results"
  mkdir -p "$data_dir" "$results_dir"

  make_sourceable_script "$sourceable_script"
  write_runner_fixture_helpers "$fixture_helpers"
  write_two_request_batch_file "$data_dir/batch_001.json"

  local output
  if ! output="$(
    bash -lc '
      source "$1"
      LOADTEST_HELPERS="$2"
      DATA_DIR="$3"
      RESULTS_BASE_DIR="$4"
      TASK_RESPONSE_JSON="$5"
      export LOADTEST_FAKE_FINAL_ENTRIES="$6"
      post_batch_file() {
        local body_file="$3"
        printf "%s" "$TASK_RESPONSE_JSON" > "$body_file"
        printf "200"
      }
      main
    ' bash "$sourceable_script" "$fixture_helpers" "$data_dir" "$results_dir" '{"taskID":42}' "1"
  )"; then
    echo "FAIL: runner count-mismatch fixture should complete"
    TEST_FAIL=$((TEST_FAIL + 1))
    rm -rf "$tmp_dir"
    return
  fi

  if [[ "$output" == *"WARN: document count mismatch — expected 2, got 1"* ]]; then
    echo "PASS: runner final-count mismatch path emits warning"
    TEST_PASS=$((TEST_PASS + 1))
  else
    echo "FAIL: final-count mismatch warning missing"
    echo "  got: $output"
    TEST_FAIL=$((TEST_FAIL + 1))
    rm -rf "$tmp_dir"
    return
  fi

  local artifact_file total_docs error_count
  artifact_file="$(awk '/INFO: artifact written to / { print $NF }' <<< "$output" | tail -n 1)"
  if [[ -z "$artifact_file" || ! -f "$artifact_file" ]]; then
    echo "FAIL: count-mismatch runner path did not write artifact"
    TEST_FAIL=$((TEST_FAIL + 1))
    rm -rf "$tmp_dir"
    return
  fi

  total_docs="$(jq -r '.totalDocs' "$artifact_file")"
  error_count="$(jq -r '.errorCount' "$artifact_file")"
  if [[ "$total_docs" == "1" && "$error_count" == "0" ]]; then
    echo "PASS: runner count-mismatch artifact keeps final entries and zero batch errors"
    TEST_PASS=$((TEST_PASS + 1))
  else
    echo "FAIL: count-mismatch artifact mismatch (totalDocs=$total_docs errorCount=$error_count)"
    TEST_FAIL=$((TEST_FAIL + 1))
  fi

  rm -rf "$tmp_dir"
}

# --- Run tests ---
echo "=== import_benchmark.sh error-path tests ==="
test_missing_data_dir
test_empty_data_dir
test_malformed_task_id
test_doc_count_parsing
test_batch_request_counting
test_doc_count_mismatch_warning
test_runner_malformed_task_id_path
test_runner_final_count_mismatch_path

echo ""
echo "Results: $TEST_PASS passed, $TEST_FAIL failed"
if [[ $TEST_FAIL -gt 0 ]]; then
  exit 1
fi
echo "All error-path tests passed."
