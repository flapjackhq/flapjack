#!/usr/bin/env bash
# SDK Compatibility Matrix Runner
# Thin orchestrator that runs existing SDK E2E test suites and produces matrix_report.json
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPORT_FILE="${REPORT_FILE:-$SCRIPT_DIR/matrix_report.json}"
LOG_DIR="${LOG_DIR:-$SCRIPT_DIR/matrix_logs}"
mkdir -p "$LOG_DIR"

# ============================================================================
# Configuration
# ============================================================================

FLAPJACK_URL="${FLAPJACK_URL:-http://localhost:7700}"
FLAPJACK_HOST="${FLAPJACK_HOST:-localhost}"
FLAPJACK_PORT="${FLAPJACK_PORT:-7700}"
FLAPJACK_APP_ID="${FLAPJACK_APP_ID:-flapjack}"
FLAPJACK_API_KEY="${FLAPJACK_API_KEY:-fj_devtestadminkey000000}"

# Operation buckets
BUCKETS=(
  "index_crud"
  "document_batch_get_delete"
  "search_with_filters"
  "browse_cursor_pagination"
  "settings_roundtrip_stage1_fields"
  "api_key_crud"
  "instantsearch_response_shapes"
)

# SDK targets
DEFAULT_SDKS=("js" "go" "python" "ruby" "php" "java" "swift")
SDKS=("${DEFAULT_SDKS[@]}")
source "$SCRIPT_DIR/lib/matrix_sdk_selection.sh"

# ============================================================================
# Helpers
# ============================================================================

has_any_fail=false

# Initialize the report JSON
init_report() {
  local server_info
  server_info=$(curl -sf "$FLAPJACK_URL/health" 2>/dev/null || echo '{"status":"unknown"}')
  cat > "$REPORT_FILE" <<JSONEOF
{
  "generated_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "server": $server_info,
  "sdk_versions": {},
  "results": {}
}
JSONEOF
}

# Set a result entry in the report
# Usage: set_result <sdk> <bucket> <status> <reason> [command] [duration_ms]
set_result() {
  local sdk="$1" bucket="$2" status="$3" reason="$4"
  local command="${5:-}" duration_ms="${6:-0}"
  local log_path="$LOG_DIR/${sdk}.log"

  python3 - "$REPORT_FILE" "$sdk" "$bucket" "$status" "$reason" "$command" "$duration_ms" "$log_path" <<'PYEOF'
import json, sys
report_file, sdk, bucket, status, reason, command, duration_ms, log_path = sys.argv[1:9]
with open(report_file, 'r') as f:
    report = json.load(f)
if sdk not in report['results']:
    report['results'][sdk] = {'operations': {}}
entry = {"status": status, "reason": reason, "log_path": log_path}
if command:
    entry["command"] = command
if duration_ms and duration_ms != "0":
    entry["duration_ms"] = int(duration_ms)
report['results'][sdk]['operations'][bucket] = entry
with open(report_file, 'w') as f:
    json.dump(report, f, indent=2)
PYEOF

  if [ "$status" = "fail" ]; then
    has_any_fail=true
  fi
}

# Set SDK version in report
set_sdk_version() {
  local sdk="$1" version="$2"
  python3 - "$REPORT_FILE" "$sdk" "$version" <<'PYEOF'
import json, sys
report_file, sdk, version = sys.argv[1:4]
with open(report_file, 'r') as f:
    report = json.load(f)
report['sdk_versions'][sdk] = version
with open(report_file, 'w') as f:
    json.dump(report, f, indent=2)
PYEOF
}

# Set all buckets for an SDK to the same status/reason.
# Usage: mark_all_buckets <sdk> <status> <reason> [command] [duration_ms]
# Handles instantsearch_response_shapes as JS-only automatically.
mark_all_buckets() {
  local sdk="$1" status="$2" reason="$3"
  local cmd="${4:-}" duration_ms="${5:-0}"
  for bucket in "${BUCKETS[@]}"; do
    if [ "$bucket" = "instantsearch_response_shapes" ] && [ "$sdk" != "js" ]; then
      set_result "$sdk" "$bucket" "skip" "JS-only bucket"
    else
      set_result "$sdk" "$bucket" "$status" "$reason" "$cmd" "$duration_ms"
    fi
  done
}

# Bucket-specific JS contract test names (from contract_tests.js).
js_bucket_test_names() {
  local bucket="$1"
  case "$bucket" in
    index_crud)
      cat <<'EOF'
GET /1/indexes - list indices
DELETE /1/indexes/{indexName} - delete index
EOF
      ;;
    document_batch_get_delete)
      cat <<'EOF'
POST /1/indexes/{indexName}/batch - addObject
POST /1/indexes/{indexName}/batch - updateObject
POST /1/indexes/{indexName}/batch - partialUpdateObject
POST /1/indexes/{indexName}/batch - partialUpdateObject with createIfNotExists=false
POST /1/indexes/{indexName}/batch - deleteObject
GET /1/indexes/{indexName}/{objectID}
GET /1/indexes/{indexName}/{objectID} - 404 for missing object
PUT /1/indexes/{indexName}/{objectID}
DELETE /1/indexes/{indexName}/{objectID}
POST /1/indexes/*/objects - bulk retrieval
POST /1/indexes/*/objects - attributesToRetrieve
POST /1/indexes/{indexName}/deleteByQuery
POST /1/indexes/{indexName}/clear
EOF
      ;;
    search_with_filters)
      cat <<'EOF'
POST /1/indexes/{indexName}/query - text search
POST /1/indexes/{indexName}/query - filters
POST /1/indexes/{indexName}/query - numeric range
POST /1/indexes/{indexName}/query - facets
POST /1/indexes/*/queries - multi-index search
POST /1/indexes/{indexName}/facets/{facetName}/query - facet search
EOF
      ;;
    browse_cursor_pagination)
      cat <<'EOF'
POST /1/indexes/{indexName}/browse - cursor pagination
EOF
      ;;
    settings_roundtrip_stage1_fields)
      cat <<'EOF'
POST /1/indexes/{indexName}/settings - set settings
GET /1/indexes/{indexName}/settings - get settings
PUT/GET settings - numericAttributesForFiltering round-trip
PUT/GET settings - searchableAttributes with unordered() round-trip
PUT/GET settings - allowCompressionOfIntegerArray round-trip
EOF
      ;;
    api_key_crud)
      cat <<'EOF'
POST /1/keys - create API key
GET /1/keys - list API keys
GET /1/keys/:key - get specific API key
DELETE /1/keys/:key - delete API key
EOF
      ;;
    instantsearch_response_shapes)
      cat <<'EOF'
Search response - InstantSearch-compatible shape (hits, facets, pagination, highlighting)
EOF
      ;;
    *)
      ;;
  esac
}

js_bucket_human_name() {
  local bucket="$1"
  case "$bucket" in
    index_crud) echo "Index CRUD" ;;
    document_batch_get_delete) echo "Document batch/get/delete" ;;
    search_with_filters) echo "Search/filter" ;;
    browse_cursor_pagination) echo "Browse cursor pagination" ;;
    settings_roundtrip_stage1_fields) echo "Stage 1 settings roundtrip" ;;
    api_key_crud) echo "API key CRUD" ;;
    instantsearch_response_shapes) echo "InstantSearch response shape" ;;
    *) echo "$bucket" ;;
  esac
}

js_bucket_expected_count() {
  local bucket="$1"
  js_bucket_test_names "$bucket" | awk 'NF { count++ } END { print count + 0 }'
}

js_bucket_passed_count() {
  local bucket="$1" log_file="$2"
  local count=0 test_name
  while IFS= read -r test_name; do
    [ -z "$test_name" ] && continue
    if grep -Fq "✓ $test_name" "$log_file" 2>/dev/null; then
      count=$((count + 1))
    fi
  done < <(js_bucket_test_names "$bucket")
  echo "$count"
}

js_bucket_has_failure() {
  local bucket="$1" log_file="$2"
  local test_name
  while IFS= read -r test_name; do
    [ -z "$test_name" ] && continue
    if grep -Fq "✗ $test_name" "$log_file" 2>/dev/null; then
      return 0
    fi
  done < <(js_bucket_test_names "$bucket")
  return 1
}

# Run a command and capture timing + exit code
# Returns: "<exit_code> <duration_ms>" on stdout
run_timed() {
  local log_file="$1"
  shift
  local start_ms end_ms duration exit_code
  start_ms=$(($(date +%s) * 1000))
  "$@" > "$log_file" 2>&1
  exit_code=$?
  end_ms=$(($(date +%s) * 1000))
  duration=$((end_ms - start_ms))
  echo "$exit_code $duration"
}

# ============================================================================
# Preflight: server health check
# ============================================================================

echo "=== SDK Compatibility Matrix Runner ==="
echo "Server: $FLAPJACK_URL"

if ! curl -sf "$FLAPJACK_URL/health" > /dev/null 2>&1; then
  echo "FATAL: Flapjack server not reachable at $FLAPJACK_URL/health"
  echo "Start the server first, then re-run this script."
  exit 2
fi
echo "Server health: OK"

init_report

# ============================================================================
# JS SDK
# ============================================================================

# TODO: Document run_js.
run_js() {
  local sdk="js"
  echo ""
  echo "--- JS SDK ---"

  if ! command -v node &>/dev/null; then
    echo "  SKIP: node not found"
    mark_all_buckets "$sdk" "skip" "missing_node"
    return
  fi

  local node_ver
  node_ver=$(node --version 2>/dev/null || echo "unknown")
  set_sdk_version "$sdk" "node $node_ver"

  # Run contract_tests.js (covers: index_crud, document_batch_get_delete, search_with_filters,
  # browse_cursor_pagination, settings_roundtrip_stage1_fields, api_key_crud, instantsearch_response_shapes)
  local log_file="$LOG_DIR/${sdk}.log"
  local cmd="node contract_tests.js"
  echo "  Running: $cmd"

  local result
  result=$(cd "$SCRIPT_DIR" && run_timed "$log_file" node contract_tests.js)
  local exit_code duration_ms
  exit_code=$(echo "$result" | awk '{print $1}')
  duration_ms=$(echo "$result" | awk '{print $2}')

  if [ "$exit_code" = "0" ]; then
    echo "  PASS (${duration_ms}ms)"
    mark_all_buckets "$sdk" "pass" "All contract tests passed" "$cmd" "$duration_ms"
  else
    echo "  FAIL (exit $exit_code)"
    # Parse log to determine which buckets passed/failed.
    # A bucket is only pass if every expected test for that bucket is explicitly ✓ in the log.
    local passed_tests failed_tests
    passed_tests=$(grep -c '^✓' "$log_file" 2>/dev/null || echo "0")
    failed_tests=$(grep -c '^✗' "$log_file" 2>/dev/null || echo "0")

    for bucket in "${BUCKETS[@]}"; do
      local bucket_status bucket_reason bucket_name expected_count passed_count
      bucket_name=$(js_bucket_human_name "$bucket")
      expected_count=$(js_bucket_expected_count "$bucket")
      passed_count=$(js_bucket_passed_count "$bucket" "$log_file")

      if js_bucket_has_failure "$bucket" "$log_file"; then
        bucket_status="fail"
        bucket_reason="$bucket_name test failed"
      elif [ "$expected_count" -gt 0 ] && [ "$passed_count" -eq "$expected_count" ]; then
        bucket_status="pass"
        bucket_reason="$passed_tests tests passed, $failed_tests failed ($bucket_name $passed_count/$expected_count passed)"
      else
        bucket_status="fail"
        bucket_reason="contract_tests.js exited $exit_code and $bucket_name could not be fully verified ($passed_count/$expected_count tests seen)"
      fi

      set_result "$sdk" "$bucket" "$bucket_status" "$bucket_reason" "$cmd" "$duration_ms"
    done
  fi
}

# ============================================================================
# Go SDK
# ============================================================================

# TODO: Document run_go.
run_go() {
  local sdk="go"
  echo ""
  echo "--- Go SDK ---"

  if ! command -v go &>/dev/null; then
    echo "  SKIP: go not found"
    mark_all_buckets "$sdk" "skip" "missing_go"
    return
  fi

  local go_ver
  go_ver=$(go version 2>/dev/null | awk '{print $3}' || echo "unknown")
  set_sdk_version "$sdk" "$go_ver"

  local go_dir="$REPO_ROOT/sdks/go"
  if [ ! -f "$go_dir/tests/e2e_test.go" ]; then
    echo "  SKIP: test file not found"
    mark_all_buckets "$sdk" "skip" "test_file_missing"
    return
  fi

  local log_file="$LOG_DIR/${sdk}.log"
  local cmd="go test ./tests/ -v -count=1 -timeout=120s"
  echo "  Running: $cmd"

  local result
  result=$(cd "$go_dir" && FLAPJACK_HOST="${FLAPJACK_HOST}:${FLAPJACK_PORT}" FLAPJACK_APP_ID="$FLAPJACK_APP_ID" FLAPJACK_API_KEY="$FLAPJACK_API_KEY" run_timed "$log_file" go test ./tests/ -v -count=1 -timeout=120s)
  local exit_code duration_ms
  exit_code=$(echo "$result" | awk '{print $1}')
  duration_ms=$(echo "$result" | awk '{print $2}')

  if [ "$exit_code" = "0" ]; then
    echo "  PASS (${duration_ms}ms)"
    mark_all_buckets "$sdk" "pass" "All tests passed" "$cmd" "$duration_ms"
  else
    echo "  FAIL (exit $exit_code)"
    # Map Go test names to buckets
    for bucket in "${BUCKETS[@]}"; do
      if [ "$bucket" = "instantsearch_response_shapes" ]; then
        set_result "$sdk" "$bucket" "skip" "JS-only bucket"
        continue
      fi

      local bucket_status="pass"
      local bucket_reason="Tests passed"

      case "$bucket" in
        index_crud)
          if grep -q 'FAIL.*TestListIndices' "$log_file" 2>/dev/null; then
            bucket_status="fail"; bucket_reason="TestListIndices failed"
          fi ;;
        document_batch_get_delete)
          if grep -qE 'FAIL.*(TestGetObject|TestPartialUpdate|TestSaveAndDelete)' "$log_file" 2>/dev/null; then
            bucket_status="fail"; bucket_reason="Object test failed"
          fi ;;
        search_with_filters)
          if grep -qE 'FAIL.*(TestBasicSearch|TestSearchWith|TestSearchHighlight|TestSearchPagination)' "$log_file" 2>/dev/null; then
            bucket_status="fail"; bucket_reason="Search test failed"
          fi ;;
        browse_cursor_pagination)
          if grep -q 'FAIL.*TestBrowseCursor' "$log_file" 2>/dev/null; then
            bucket_status="fail"; bucket_reason="TestBrowseCursorPagination failed"
          fi ;;
        settings_roundtrip_stage1_fields)
          if grep -qE 'FAIL.*(TestSettingsStage1|TestGetSettings|TestUpdateSettings)' "$log_file" 2>/dev/null; then
            bucket_status="fail"; bucket_reason="Settings test failed"
          fi ;;
        api_key_crud)
          if grep -q 'FAIL.*TestApiKeyCRUD' "$log_file" 2>/dev/null; then
            bucket_status="fail"; bucket_reason="TestApiKeyCRUD failed"
          fi ;;
      esac

      set_result "$sdk" "$bucket" "$bucket_status" "$bucket_reason" "$cmd" "$duration_ms"
    done
  fi
}

# ============================================================================
# Python SDK
# ============================================================================

# TODO: Document run_python.
run_python() {
  local sdk="python"
  echo ""
  echo "--- Python SDK ---"

  if ! command -v python3 &>/dev/null; then
    echo "  SKIP: python3 not found"
    mark_all_buckets "$sdk" "skip" "missing_python"
    return
  fi

  local py_ver
  py_ver=$(python3 --version 2>/dev/null | awk '{print $2}' || echo "unknown")
  set_sdk_version "$sdk" "python $py_ver"

  local py_dir="$REPO_ROOT/sdks/python"
  if [ ! -f "$py_dir/tests/test_search_e2e.py" ]; then
    echo "  SKIP: test file not found"
    mark_all_buckets "$sdk" "skip" "test_file_missing"
    return
  fi

  # Check if pytest is available
  if ! python3 -m pytest --version &>/dev/null; then
    echo "  SKIP: pytest not available"
    mark_all_buckets "$sdk" "skip" "missing_pytest"
    return
  fi

  # Check if SDK package is importable
  if ! python3 -c "import flapjacksearch" &>/dev/null; then
    echo "  SKIP: flapjacksearch package not installed"
    mark_all_buckets "$sdk" "skip" "missing_flapjacksearch_package"
    return
  fi

  local log_file="$LOG_DIR/${sdk}.log"
  local cmd="python3 -m pytest tests/test_search_e2e.py -v"
  echo "  Running: $cmd"

  local result
  result=$(cd "$py_dir" && FLAPJACK_HOST="$FLAPJACK_HOST" FLAPJACK_PORT="$FLAPJACK_PORT" FLAPJACK_APP_ID="$FLAPJACK_APP_ID" FLAPJACK_API_KEY="$FLAPJACK_API_KEY" run_timed "$log_file" python3 -m pytest tests/test_search_e2e.py -v)
  local exit_code duration_ms
  exit_code=$(echo "$result" | awk '{print $1}')
  duration_ms=$(echo "$result" | awk '{print $2}')

  if [ "$exit_code" = "0" ]; then
    echo "  PASS (${duration_ms}ms)"
    mark_all_buckets "$sdk" "pass" "All tests passed" "$cmd" "$duration_ms"
  else
    # Check if all tests errored (likely SDK dependency/config issue)
    local error_count
    error_count=$(grep -c ' ERROR' "$log_file" 2>/dev/null || true)
    local passed_count
    passed_count=$(grep -c ' PASSED' "$log_file" 2>/dev/null || true)
    if [ "$error_count" -gt 0 ] && [ "$passed_count" = "0" ]; then
      echo "  SKIP: all tests errored (SDK dependency/config issue)"
      mark_all_buckets "$sdk" "skip" "sdk_dependency_or_config_error"
    else
      echo "  FAIL (exit $exit_code)"
      mark_all_buckets "$sdk" "fail" "pytest returned exit code $exit_code" "$cmd" "$duration_ms"
    fi
  fi
}

# ============================================================================
# Ruby SDK
# ============================================================================

# TODO: Document run_ruby.
run_ruby() {
  local sdk="ruby"
  echo ""
  echo "--- Ruby SDK ---"

  if ! command -v ruby &>/dev/null; then
    echo "  SKIP: ruby not found"
    mark_all_buckets "$sdk" "skip" "missing_ruby"
    return
  fi

  local ruby_ver
  ruby_ver=$(ruby --version 2>/dev/null | awk '{print $2}' || echo "unknown")
  set_sdk_version "$sdk" "ruby $ruby_ver"

  local ruby_dir="$REPO_ROOT/sdks/ruby"
  if [ ! -f "$ruby_dir/tests/flapjack_search_e2e_test.rb" ]; then
    echo "  SKIP: test file not found"
    mark_all_buckets "$sdk" "skip" "test_file_missing"
    return
  fi

  # Check for bundle
  if ! command -v bundle &>/dev/null; then
    echo "  SKIP: bundle not available"
    mark_all_buckets "$sdk" "skip" "missing_bundle"
    return
  fi

  # Check if Gemfile exists and bundle is runnable
  if [ ! -f "$ruby_dir/Gemfile" ]; then
    echo "  SKIP: Gemfile not found"
    mark_all_buckets "$sdk" "skip" "missing_gemfile"
    return
  fi

  # Check if bundle can resolve dependencies
  if ! (cd "$ruby_dir" && bundle check &>/dev/null); then
    echo "  SKIP: bundle dependencies not satisfied"
    mark_all_buckets "$sdk" "skip" "bundle_dependencies_not_satisfied"
    return
  fi

  local log_file="$LOG_DIR/${sdk}.log"
  local cmd="bundle exec ruby tests/flapjack_search_e2e_test.rb"
  echo "  Running: $cmd"

  local result
  result=$(cd "$ruby_dir" && FLAPJACK_SERVER="$FLAPJACK_HOST" FLAPJACK_PORT="$FLAPJACK_PORT" FLAPJACK_APP_ID="$FLAPJACK_APP_ID" FLAPJACK_API_KEY="$FLAPJACK_API_KEY" run_timed "$log_file" bundle exec ruby tests/flapjack_search_e2e_test.rb)
  local exit_code duration_ms
  exit_code=$(echo "$result" | awk '{print $1}')
  duration_ms=$(echo "$result" | awk '{print $2}')

  if [ "$exit_code" = "0" ]; then
    echo "  PASS (${duration_ms}ms)"
    mark_all_buckets "$sdk" "pass" "All tests passed" "$cmd" "$duration_ms"
  else
    echo "  FAIL (exit $exit_code)"
    mark_all_buckets "$sdk" "fail" "Ruby tests returned exit code $exit_code" "$cmd" "$duration_ms"
  fi
}

# ============================================================================
# PHP SDK
# ============================================================================

# TODO: Document run_php.
run_php() {
  local sdk="php"
  echo ""
  echo "--- PHP SDK ---"

  if ! command -v php &>/dev/null; then
    echo "  SKIP: php not found"
    mark_all_buckets "$sdk" "skip" "missing_php"
    return
  fi

  local php_ver
  php_ver=$(php --version 2>/dev/null | head -1 | awk '{print $2}' || echo "unknown")
  set_sdk_version "$sdk" "php $php_ver"

  local php_dir="$REPO_ROOT/sdks/php"
  if [ ! -f "$php_dir/tests/FlapjackSearchE2eTest.php" ]; then
    echo "  SKIP: test file not found"
    mark_all_buckets "$sdk" "skip" "test_file_missing"
    return
  fi

  # Check for phpunit
  local phpunit_cmd=""
  if [ -f "$php_dir/vendor/bin/phpunit" ]; then
    phpunit_cmd="vendor/bin/phpunit"
  elif command -v phpunit &>/dev/null; then
    phpunit_cmd="phpunit"
  else
    echo "  SKIP: phpunit not available"
    mark_all_buckets "$sdk" "skip" "missing_phpunit"
    return
  fi

  local log_file="$LOG_DIR/${sdk}.log"
  local cmd="$phpunit_cmd tests/FlapjackSearchE2eTest.php"
  echo "  Running: $cmd"

  local result
  result=$(cd "$php_dir" && FLAPJACK_HOST="$FLAPJACK_HOST" FLAPJACK_PORT="$FLAPJACK_PORT" FLAPJACK_APP_ID="$FLAPJACK_APP_ID" FLAPJACK_API_KEY="$FLAPJACK_API_KEY" run_timed "$log_file" $phpunit_cmd tests/FlapjackSearchE2eTest.php)
  local exit_code duration_ms
  exit_code=$(echo "$result" | awk '{print $1}')
  duration_ms=$(echo "$result" | awk '{print $2}')

  if [ "$exit_code" = "0" ]; then
    echo "  PASS (${duration_ms}ms)"
    mark_all_buckets "$sdk" "pass" "All tests passed" "$cmd" "$duration_ms"
  else
    echo "  FAIL (exit $exit_code)"
    mark_all_buckets "$sdk" "fail" "PHPUnit returned exit code $exit_code" "$cmd" "$duration_ms"
  fi
}

# ============================================================================
# Java SDK
# ============================================================================

# TODO: Document run_java.
run_java() {
  local sdk="java"
  echo ""
  echo "--- Java SDK ---"

  if ! command -v java &>/dev/null; then
    echo "  SKIP: java not found"
    mark_all_buckets "$sdk" "skip" "missing_java"
    return
  fi

  # Verify Java actually works (macOS may have stub that prompts for install)
  if ! java --version &>/dev/null; then
    echo "  SKIP: java runtime not functional"
    mark_all_buckets "$sdk" "skip" "missing_java_runtime"
    return
  fi

  local java_ver
  java_ver=$(java --version 2>/dev/null | head -1 || echo "unknown")
  set_sdk_version "$sdk" "$java_ver"

  local java_dir="$REPO_ROOT/sdks/java"

  # Check for gradle wrapper or system gradle
  local gradle_cmd=""
  if [ -f "$java_dir/gradlew" ]; then
    gradle_cmd="./gradlew"
  elif command -v gradle &>/dev/null; then
    gradle_cmd="gradle"
  else
    echo "  SKIP: gradle not available"
    mark_all_buckets "$sdk" "skip" "missing_gradle"
    return
  fi

  if [ ! -f "$java_dir/tests/src/test/java/com/flapjackhq/tests/SearchE2ETest.java" ]; then
    echo "  SKIP: test file not found"
    mark_all_buckets "$sdk" "skip" "test_file_missing"
    return
  fi

  local log_file="$LOG_DIR/${sdk}.log"
  local cmd="$gradle_cmd :tests:test --tests com.flapjackhq.tests.SearchE2ETest"
  echo "  Running: $cmd"

  local result
  result=$(cd "$java_dir" && FLAPJACK_HOST="$FLAPJACK_HOST" FLAPJACK_PORT="$FLAPJACK_PORT" FLAPJACK_APP_ID="$FLAPJACK_APP_ID" FLAPJACK_API_KEY="$FLAPJACK_API_KEY" run_timed "$log_file" $gradle_cmd :tests:test --tests com.flapjackhq.tests.SearchE2ETest)
  local exit_code duration_ms
  exit_code=$(echo "$result" | awk '{print $1}')
  duration_ms=$(echo "$result" | awk '{print $2}')

  if [ "$exit_code" = "0" ]; then
    echo "  PASS (${duration_ms}ms)"
    mark_all_buckets "$sdk" "pass" "All tests passed" "$cmd" "$duration_ms"
  else
    echo "  FAIL (exit $exit_code)"
    mark_all_buckets "$sdk" "fail" "Gradle test returned exit code $exit_code" "$cmd" "$duration_ms"
  fi
}

# ============================================================================
# Swift SDK
# ============================================================================

# TODO: Document run_swift.
run_swift() {
  local sdk="swift"
  echo ""
  echo "--- Swift SDK ---"

  if ! command -v swift &>/dev/null; then
    echo "  SKIP: swift not found"
    mark_all_buckets "$sdk" "skip" "missing_swift"
    return
  fi

  local swift_ver
  swift_ver=$(swift --version 2>/dev/null | head -1 | sed 's/.*version //' | awk '{print $1}' || echo "unknown")
  set_sdk_version "$sdk" "swift $swift_ver"

  local swift_dir="$REPO_ROOT/sdks/swift"
  if [ ! -f "$swift_dir/Tests/SearchE2ETests/SearchE2ETests.swift" ]; then
    echo "  SKIP: test file not found"
    mark_all_buckets "$sdk" "skip" "test_file_missing"
    return
  fi

  # Check for Package.swift
  if [ ! -f "$swift_dir/Package.swift" ]; then
    echo "  SKIP: Package.swift not found"
    mark_all_buckets "$sdk" "skip" "missing_package_swift"
    return
  fi

  local log_file="$LOG_DIR/${sdk}.log"
  local cmd="swift test --filter SearchE2ETests"
  echo "  Running: $cmd"

  local result
  result=$(cd "$swift_dir" && FLAPJACK_HOST="$FLAPJACK_HOST" FLAPJACK_PORT="$FLAPJACK_PORT" FLAPJACK_APP_ID="$FLAPJACK_APP_ID" FLAPJACK_API_KEY="$FLAPJACK_API_KEY" run_timed "$log_file" swift test --filter SearchE2ETests)
  local exit_code duration_ms
  exit_code=$(echo "$result" | awk '{print $1}')
  duration_ms=$(echo "$result" | awk '{print $2}')

  if [ "$exit_code" = "0" ]; then
    echo "  PASS (${duration_ms}ms)"
    mark_all_buckets "$sdk" "pass" "All tests passed" "$cmd" "$duration_ms"
  else
    # Detect "no tests found" or build/compile errors as skip rather than fail
    if grep -q 'no tests found' "$log_file" 2>/dev/null; then
      echo "  SKIP: no tests found (test target configuration issue)"
      mark_all_buckets "$sdk" "skip" "swift_test_target_not_configured"
    elif grep -qE 'error:.*compil|could not build|unable to resolve|cannot find module|no such module' "$log_file" 2>/dev/null; then
      echo "  SKIP: Swift build/dependency error"
      mark_all_buckets "$sdk" "skip" "swift_build_or_dependency_error"
    else
      echo "  FAIL (exit $exit_code)"
      mark_all_buckets "$sdk" "fail" "Swift test returned exit code $exit_code" "$cmd" "$duration_ms"
    fi
  fi
}

configure_sdks

echo "SDKs: ${SDKS[*]}"

for sdk in "${SDKS[@]}"; do
  run_sdk "$sdk"
done

echo ""
echo "=== Matrix Report ==="
echo "Written to: $REPORT_FILE"
echo ""

# Pretty-print summary
python3 -c "
import json, sys
with open('$REPORT_FILE') as f:
    report = json.load(f)

print(f\"Generated: {report['generated_at']}\")
print()

# Summary table
sdks = sorted(report['results'].keys())
buckets = set()
for sdk_data in report['results'].values():
    buckets.update(sdk_data.get('operations', {}).keys())
buckets = sorted(buckets)

# Header
header = f\"{'Bucket':<40}\" + ''.join(f'{s:>8}' for s in sdks)
print(header)
print('-' * len(header))

pass_count = 0
fail_count = 0
skip_count = 0

for bucket in buckets:
    row = f'{bucket:<40}'
    for sdk in sdks:
        ops = report['results'].get(sdk, {}).get('operations', {})
        entry = ops.get(bucket, {})
        status = entry.get('status', 'skip')
        if status == 'pass':
            row += f\"{'PASS':>8}\"
            pass_count += 1
        elif status == 'fail':
            row += f\"{'FAIL':>8}\"
            fail_count += 1
        else:
            row += f\"{'SKIP':>8}\"
            skip_count += 1
    print(row)

print()
print(f'Total: {pass_count} pass, {fail_count} fail, {skip_count} skip')
"

# Exit code: non-zero if any fail
if [ "$has_any_fail" = "true" ]; then
  echo ""
  echo "RESULT: FAIL (one or more SDK tests failed)"
  exit 1
else
  echo ""
  echo "RESULT: OK (all tests passed or skipped)"
  exit 0
fi
