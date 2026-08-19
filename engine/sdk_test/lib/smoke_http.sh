#!/bin/bash

# Shared HTTP/request helpers for SDK curl smoke tests.
# Callers must define FLAPJACK_URL, FLAPJACK_ADMIN_KEY, and APP_ID.
# Optional caller vars:
# - SMOKE_USER_AGENT: if set, sent as User-Agent header and asserted in request trace.

smoke_http_setup() {
  TMP_DIR="$(mktemp -d)"
  WORKING_DIR="$TMP_DIR/requests"
  mkdir -p "$WORKING_DIR"

  LAST_HTTP_STATUS=""
  LAST_RESPONSE_BODY_FILE=""
  LAST_RESPONSE_HEADERS_FILE=""
  LAST_REQUEST_TRACE_FILE=""
}

smoke_http_cleanup() {
  if [ -n "${TMP_DIR:-}" ] && [ -d "$TMP_DIR" ]; then
    rm -rf "$TMP_DIR"
  fi
}

assert_status_ok() {
  local status="$1"
  local context="$2"

  if [ "$status" -lt 200 ] || [ "$status" -ge 300 ]; then
    echo "FAIL: HTTP ${status} during ${context}"
    if [ -n "$LAST_RESPONSE_BODY_FILE" ] && [ -f "$LAST_RESPONSE_BODY_FILE" ]; then
      echo "Response: $(cat "$LAST_RESPONSE_BODY_FILE")"
    fi
    exit 1
  fi
}

assert_json_response_content_type() {
  local headers_file="$1"
  local context="$2"

  if ! grep -Eqi '^content-type:.*application/json' "$headers_file"; then
    echo "FAIL: Missing application/json content-type for ${context}"
    echo "Headers: $(cat "$headers_file")"
    exit 1
  fi
}

assert_request_header_present() {
  local trace_file="$1"
  local header="$2"
  local context="$3"

  if ! grep -qi "^> .*${header}:" "$trace_file"; then
    echo "FAIL: Missing request header ${header} during ${context}"
    echo "Trace: $(cat "$trace_file")"
    exit 1
  fi
}

# TODO: Document send_request.
send_request() {
  local method="$1"
  local path="$2"
  local body="${3:-}"
  local request_id="${4:-generic}"

  LAST_RESPONSE_BODY_FILE="${WORKING_DIR}/${request_id}.body"
  LAST_RESPONSE_HEADERS_FILE="${WORKING_DIR}/${request_id}.headers"
  LAST_REQUEST_TRACE_FILE="${WORKING_DIR}/${request_id}.trace"

  local args=(
    -sS
    -v
    -X "$method"
    -H "x-algolia-api-key: $FLAPJACK_ADMIN_KEY"
    -H "x-algolia-application-id: $APP_ID"
    -H "Content-Type: application/json"
    -D "$LAST_RESPONSE_HEADERS_FILE"
    -o "$LAST_RESPONSE_BODY_FILE"
    "$FLAPJACK_URL$path"
  )

  if [ -n "${SMOKE_USER_AGENT:-}" ]; then
    args+=(-H "User-Agent: $SMOKE_USER_AGENT")
  fi

  if [ -n "$body" ]; then
    args+=(-d "$body")
  fi

  if ! LAST_HTTP_STATUS=$(curl "${args[@]}" -w '%{http_code}' 2>"$LAST_REQUEST_TRACE_FILE"); then
    echo "FAIL: curl transport error during ${method} ${path}"
    if [ -f "$LAST_REQUEST_TRACE_FILE" ]; then
      echo "Trace: $(cat "$LAST_REQUEST_TRACE_FILE")"
    fi
    exit 1
  fi

  assert_status_ok "$LAST_HTTP_STATUS" "$method $path"
  assert_request_header_present "$LAST_REQUEST_TRACE_FILE" "x-algolia-api-key" "$method $path"
  assert_request_header_present "$LAST_REQUEST_TRACE_FILE" "x-algolia-application-id" "$method $path"
  assert_request_header_present "$LAST_REQUEST_TRACE_FILE" "Content-Type" "$method $path"
  if [ -n "${SMOKE_USER_AGENT:-}" ]; then
    assert_request_header_present "$LAST_REQUEST_TRACE_FILE" "User-Agent" "$method $path"
  fi
  assert_json_response_content_type "$LAST_RESPONSE_HEADERS_FILE" "$method $path"
}

extract_task_id() {
  local response="$1"
  echo "$response" | grep -Eo '"taskID"[[:space:]]*:[[:space:]]*[0-9]+' | grep -Eo '[0-9]+' | head -n1 || true
}

extract_task_status() {
  local response="$1"
  echo "$response" | grep -Eo '"status"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -Eo 'published|notPublished' | head -n1 || true
}

# TODO: Document wait_for_task_published.
wait_for_task_published() {
  local task_id="$1"
  local task_path_template="$2"
  local max_attempts="${3:-30}"
  local sleep_seconds="${4:-1}"
  local request_prefix="${5:-wait-task}"
  local i

  for ((i = 1; i <= max_attempts; i++)); do
    local task_path
    local task_response
    local status
    printf -v task_path "$task_path_template" "$task_id"
    send_request GET "$task_path" "" "${request_prefix}-${i}"
    task_response=$(cat "$LAST_RESPONSE_BODY_FILE")
    status=$(extract_task_status "$task_response")

    if [ "$status" = "published" ]; then
      echo "OK: Task published"
      return 0
    fi

    if [ "$i" -eq "$max_attempts" ]; then
      echo "FAIL: Task not published after ${max_attempts} attempts"
      echo "Response: $task_response"
      exit 1
    fi

    sleep "$sleep_seconds"
  done
}

extract_nb_hits() {
  local response="$1"
  echo "$response" | grep -Eo '"nbHits"[[:space:]]*:[[:space:]]*[0-9]+' | grep -Eo '[0-9]+' | head -n1 || true
}

assert_min_nb_hits() {
  local response="$1"
  local min_hits="$2"
  local context="$3"
  local nb_hits

  nb_hits=$(extract_nb_hits "$response")
  if [ -z "$nb_hits" ] || [ "$nb_hits" -lt "$min_hits" ]; then
    echo "FAIL: Insufficient hits in ${context} (nbHits=${nb_hits:-missing}, expected >= ${min_hits})"
    echo "Response: $response"
    exit 1
  fi

  echo "$nb_hits"
}

assert_response_contains() {
  local response="$1"
  local expected_text="$2"
  local context="$3"

  if ! grep -Fq "$expected_text" <<<"$response"; then
    echo "FAIL: Expected text not found in ${context}: $expected_text"
    echo "Response: $response"
    exit 1
  fi
}

# TODO: Document run_standard_sdk_smoke_test.
run_standard_sdk_smoke_test() {
  local sdk_label="$1"
  local index_name="$2"
  local task_path_template="$3"

  local product_name="${sdk_label} Test Product"
  local secondary_name="Another ${sdk_label} Item"
  local search_query="$sdk_label"

  echo "=== ${sdk_label} SDK Protocol Smoke Test ==="
  echo "URL: $FLAPJACK_URL"
  echo "Index: $index_name"
  echo ""

  echo "Step 1: Adding documents..."
  send_request POST "/1/indexes/$index_name/batch" "{
  \"requests\": [
    {\"action\": \"addObject\", \"body\": {\"objectID\": \"doc1\", \"name\": \"$product_name\", \"price\": 99.99, \"category\": \"sdk\"}},
    {\"action\": \"addObject\", \"body\": {\"objectID\": \"doc2\", \"name\": \"$secondary_name\", \"price\": 49.99, \"category\": \"sdk\"}}
  ]
}" "add-documents"
  RESPONSE=$(cat "$LAST_RESPONSE_BODY_FILE")

  TASK_ID=$(extract_task_id "$RESPONSE")
  if [ -z "$TASK_ID" ]; then
    echo "FAIL: No taskID in batch response"
    echo "Response: $RESPONSE"
    exit 1
  fi
  echo "OK: Task ID = $TASK_ID"

  echo "Step 2: Waiting for task..."
  wait_for_task_published "$TASK_ID" "$task_path_template" 30 0.5 "wait-task"

  echo "Step 3: Searching..."
  send_request POST "/1/indexes/$index_name/query" "{\"query\": \"$search_query\", \"hitsPerPage\": 10}" "search"
  SEARCH_RESPONSE=$(cat "$LAST_RESPONSE_BODY_FILE")

  NB_HITS=$(assert_min_nb_hits "$SEARCH_RESPONSE" 1 "search response")
  echo "OK: Found $NB_HITS hits"

  assert_response_contains "$SEARCH_RESPONSE" "\"name\":\"$product_name\"" "search results"
  echo "OK: Document content verified"

  echo "Step 4: Cleaning up..."
  send_request DELETE "/1/indexes/$index_name" "" "delete-index"
  echo "OK: Index deleted"

  echo ""
  echo "=== ${sdk_label} SDK Protocol Smoke Test PASSED ==="
}
