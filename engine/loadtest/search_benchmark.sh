#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_HELPERS="${LOADTEST_HELPERS:-$SCRIPT_DIR/lib/loadtest_shell_helpers.sh}"
RESULTS_BASE_DIR="${LOADTEST_RESULTS_BASE_DIR:-$SCRIPT_DIR/results}"
SEARCH_HELPER="$SCRIPT_DIR/search_benchmark.mjs"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

epoch_ms() {
  node -e 'process.stdout.write(String(Date.now()))'
}

emit_query_catalog_json() {
  local index_name="$1"
  node "$SEARCH_HELPER" --catalog --index-name "$index_name"
}

validate_query_catalog_json() {
  local catalog_json="$1"
  jq -e '
    def valid_requests:
      type == "array" and
      length > 0 and
      all(
        .[];
        type == "object" and
        (.query | type == "string") and
        (.params | type == "object")
      );

    type == "object" and
    length > 0 and
    all(
      to_entries[];
      (.key | type == "string") and
      (.value | valid_requests)
    )
  ' <<<"$catalog_json" >/dev/null
}

verify_benchmark_index_ready() {
  local index_name="$1"
  if ! loadtest_index_exists "$index_name"; then
    fail "index \"$index_name\" was not found"
  fi

  local doc_count
  doc_count="$(loadtest_get_index_doc_count "$index_name")"
  if [[ "$doc_count" == "0" ]]; then
    fail "index \"$index_name\" contains 0 documents"
  fi

  BENCHMARK_DOC_COUNT="$doc_count"
}

run_single_search_request() {
  local index_path="$1"
  local request_json="$2"
  local response_file
  local -a curl_args

  response_file="$(mktemp)"

  curl_args=(curl -sS -o "$response_file" -w $'%{http_code}\t%{time_total}' -X POST)
  if [[ ${#LOADTEST_AUTH_HEADERS[@]} -gt 0 ]]; then
    curl_args+=("${LOADTEST_AUTH_HEADERS[@]}")
  fi
  curl_args+=(-H "Content-Type: application/json" --data "$request_json")
  curl_args+=("${FLAPJACK_BASE_URL}${index_path}")

  local curl_metrics
  if ! curl_metrics="$("${curl_args[@]}")"; then
    rm -f "$response_file"
    echo "WARN: search request transport failed" >&2
    return 1
  fi

  local http_status
  local time_total_seconds
  local extra_metric
  IFS=$'\t' read -r http_status time_total_seconds extra_metric <<<"$curl_metrics"

  if [[ "$http_status" != "200" ]]; then
    local response_body
    response_body="$(cat "$response_file")"
    rm -f "$response_file"
    echo "WARN: search request returned HTTP $http_status: $response_body" >&2
    return 1
  fi

  if [[ -n "$extra_metric" || ! "$time_total_seconds" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
    rm -f "$response_file"
    echo "WARN: search request returned invalid curl time_total: $time_total_seconds" >&2
    return 1
  fi

  local latency_ms
  latency_ms="$(awk -v seconds="$time_total_seconds" 'BEGIN { printf "%.3f", seconds * 1000 }')"
  rm -f "$response_file"
  echo "$latency_ms"
}

run_query_benchmark() {
  local index_name="$1"
  local catalog_json="$2"
  local encoded_index
  encoded_index="$(loadtest_encode_path_component "$index_name")"
  local index_path="/1/indexes/${encoded_index}/query"

  PER_TYPE_LATENCIES_JSON="{}"
  local started_ms
  started_ms="$(epoch_ms)"

  local query_type
  while IFS= read -r query_type; do
    local requests_for_type=0
    while IFS= read -r request_entry; do
      local request_body
      request_body="$(jq -c '{query: .query} + (.params // {})' <<<"$request_entry")"
      local request_query
      request_query="$(jq -r '.query' <<<"$request_entry")"
      requests_for_type=$((requests_for_type + 1))

      local latency_ms
      latency_ms="$(run_single_search_request "$index_path" "$request_body")" || {
        if [[ -n "$request_query" ]]; then
          fail "search request failed for query type \"$query_type\" (query: $request_query)"
        fi
        fail "search request failed for query type \"$query_type\""
      }

      PER_TYPE_LATENCIES_JSON="$(
        jq -c --arg type "$query_type" --argjson latency "$latency_ms" \
          '.[$type] = ((.[$type] // []) + [$latency])' \
          <<<"$PER_TYPE_LATENCIES_JSON"
      )"
    done < <(jq -c --arg type "$query_type" '.[$type][]' <<<"$catalog_json")

    if [[ "$requests_for_type" -eq 0 ]]; then
      fail "query catalog for \"$query_type\" did not contain any requests"
    fi
  done < <(jq -r 'to_entries[].key' <<<"$catalog_json")

  local finished_ms
  finished_ms="$(epoch_ms)"
  BENCHMARK_WALL_CLOCK_MS=$((finished_ms - started_ms))
}

write_search_result_artifact() {
  local result_file="$1"
  local doc_count="$2"
  local wall_clock_ms="$3"
  local index_name="$4"
  local per_type_latencies_json="$5"

  node "$SEARCH_HELPER" \
    --artifact \
    --index-name "$index_name" \
    --doc-count "$doc_count" \
    --wall-clock-ms "$wall_clock_ms" \
    --per-type-latencies "$per_type_latencies_json" > "$result_file"
}

print_summary_table() {
  local result_file="$1"
  echo ""
  echo "=== Search Benchmark Results ==="
  jq -r '
    def display_type:
      if . == "text" then "name/prefix"
      elif . == "multi_word" then "multi-word"
      else .
      end;
    "Index: " + .indexName,
    "Doc count: " + (.docCount | tostring),
    "Wall clock: " + (.wallClockMs | tostring) + " ms",
    "",
    "Type\tCount\tP50\tP95\tP99",
    (.queryTypes | to_entries[] | "\(.key | display_type)\t\(.value.count)\t\(.value.p50)\t\(.value.p95)\t\(.value.p99)"),
    "blended overall\t\(.overall.count)\t\(.overall.p50)\t\(.overall.p95)\t\(.overall.p99)"
  ' < "$result_file"
  echo "================================"
}

main() {
  [[ -f "$LOADTEST_HELPERS" ]] || fail "missing $LOADTEST_HELPERS"
  [[ -f "$SEARCH_HELPER" ]] || fail "missing $SEARCH_HELPER"

  # shellcheck source=lib/loadtest_shell_helpers.sh
  source "$LOADTEST_HELPERS"

  require_loadtest_commands curl jq node
  load_shared_loadtest_config
  initialize_loadtest_auth_headers

  local index_name="$FLAPJACK_BENCHMARK_INDEX"
  verify_benchmark_index_ready "$index_name"
  echo "INFO: benchmark index '$index_name' has $BENCHMARK_DOC_COUNT documents"

  local catalog_json
  catalog_json="$(emit_query_catalog_json "$index_name")"
  if ! validate_query_catalog_json "$catalog_json"; then
    fail "query catalog helper returned unexpected output"
  fi

  run_query_benchmark "$index_name" "$catalog_json"

  local timestamp
  timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
  local result_dir="$RESULTS_BASE_DIR/$timestamp"
  mkdir -p "$result_dir"
  local result_file="$result_dir/search_benchmark.json"

  write_search_result_artifact "$result_file" "$BENCHMARK_DOC_COUNT" "$BENCHMARK_WALL_CLOCK_MS" \
    "$index_name" "$PER_TYPE_LATENCIES_JSON"

  print_summary_table "$result_file"
  echo "INFO: artifact written to $result_file"
}

main "$@"
