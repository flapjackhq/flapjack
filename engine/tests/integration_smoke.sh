#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ENGINE_DIR="$REPO_DIR/engine"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

SERVER_PID=""
TMP_DATA=""
BUILD_LOG=""
BIN=""
BASE=""
PORT=""

APP_ID="flapjack"
SMOKE_INDEX="smoke_test"
CRUD_INDEX="smoke_crud_idx"
ADMIN_KEY=""
BATCH_TASK_ID=""
CREATED_API_KEY=""
LAST_HTTP=""
LAST_BODY=""

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_PASSED=$((TESTS_PASSED + 1))
  printf '  [PASS] %s\n' "$1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  printf '  [FAIL] %s\n' "$1"
  if [ -n "${2:-}" ]; then
    printf '    %s\n' "$2"
  fi
}

section() {
  printf '\n\033[1m%s\033[0m\n' "$1"
}

cleanup() {
  local script_exit_code=$?
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [ -n "$TMP_DATA" ] && [ -d "$TMP_DATA" ]; then
    if [ "$TESTS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      local failure_snapshot="/tmp/flapjack_integration_smoke_failure_${$}_$(date +%s)"
      cp -R "$TMP_DATA" "$failure_snapshot"
      printf 'INFO: preserved integration smoke data at %s\n' "$failure_snapshot"
    else
      rm -rf "$TMP_DATA"
    fi
  fi
  if [ -n "$BUILD_LOG" ] && [ -f "$BUILD_LOG" ]; then
    rm -f "$BUILD_LOG"
  fi
}
trap cleanup EXIT

http_code() {
  printf '%s\n' "$1" | tail -1
}

http_body() {
  printf '%s\n' "$1" | sed '$d'
}

extract_task_id() {
  printf '%s\n' "$1" | jq -r 'if (.taskID | type == "number") then (.taskID | tostring) else empty end' 2>/dev/null || true
}

curl_request() {
  local include_auth="$1"
  local method="$2"
  local path="$3"
  local body="${4:-}"
  local -a headers=('-H' 'content-type: application/json')

  if [ "$include_auth" = "yes" ]; then
    headers+=(
      '-H' "x-algolia-api-key: ${ADMIN_KEY}"
      '-H' "x-algolia-application-id: ${APP_ID}"
    )
  fi

  if [ -n "$body" ]; then
    curl -sS -w '\n%{http_code}' -X "$method" "${BASE}${path}" \
      "${headers[@]}" \
      --data "$body" 2>&1 || true
    return
  fi

  curl -sS -w '\n%{http_code}' -X "$method" "${BASE}${path}" \
    "${headers[@]}" 2>&1 || true
}

public_request() {
  curl_request "no" "$@"
}

api_request() {
  curl_request "yes" "$@"
}

# Exercise the real restricted-key middleware without changing the admin-key
# helper used by setup and cleanup. The created key remains process-local and
# is never printed, so failure output cannot disclose a live credential.
restricted_request() {
  local method="$1"
  local path="$2"
  local body="${3:-}"
  local -a headers=(
    '-H' 'content-type: application/json'
    '-H' "x-algolia-api-key: ${CREATED_API_KEY}"
    '-H' "x-algolia-application-id: ${APP_ID}"
  )

  if [ -n "$body" ]; then
    curl -sS -w '\n%{http_code}' -X "$method" "${BASE}${path}" \
      "${headers[@]}" --data "$body" 2>&1 || true
    return
  fi
  curl -sS -w '\n%{http_code}' -X "$method" "${BASE}${path}" \
    "${headers[@]}" 2>&1 || true
}

request_json() {
  local requester="$1"
  shift
  local response
  response="$("$requester" "$@")"
  LAST_HTTP="$(http_code "$response")"
  LAST_BODY="$(http_body "$response")"
}

json_matches() {
  local body="$1"
  local jq_filter="$2"
  printf '%s\n' "$body" | jq -e "$jq_filter" >/dev/null 2>&1
}

wait_for_task() {
  local task_id="$1"
  local task_status_body=""

  for _i in $(seq 1 20); do
    request_json api_request GET "/1/task/${task_id}"
    task_status_body="$LAST_BODY"

    if [ "$LAST_HTTP" = "200" ] && json_matches "$LAST_BODY" '.status == "published"' ; then
      pass "Task ${task_id} reached published status"
      return 0
    fi
    sleep 0.5
  done

  fail "Task ${task_id} did not reach published status within 10s" "$task_status_body"
  return 1
}

require_tools() {
  local missing=0
  local tool
  for tool in curl jq od sed tr; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  if [ "$missing" -ne 0 ]; then
    exit 1
  fi
}

# Generate a non-guessable admin key because the smoke server exposes privileged routes.
generate_admin_key() {
  local random_hex
  random_hex="$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  if [ -z "$random_hex" ]; then
    echo 'ERROR: failed to generate a random admin key from /dev/urandom' >&2
    exit 1
  fi
  printf 'fj_smoke_%s\n' "$random_hex"
}

build_or_resolve_binary() {
  if [ -n "${FLAPJACK_BIN:-}" ]; then
    if [ ! -x "$FLAPJACK_BIN" ]; then
      echo "ERROR: FLAPJACK_BIN=$FLAPJACK_BIN is not executable" >&2
      exit 1
    fi
    BIN="$FLAPJACK_BIN"
    printf 'Using pre-built binary: %s\n' "$BIN"
    return
  fi

  echo 'Building flapjack-server release binary...'
  BUILD_LOG="$(mktemp)"
  if (cd "$ENGINE_DIR" && cargo build -p flapjack-server --release >"$BUILD_LOG" 2>&1); then
    tail -5 "$BUILD_LOG"
  else
    tail -20 "$BUILD_LOG" >&2 || true
    echo 'ERROR: cargo build -p flapjack-server --release failed' >&2
    exit 1
  fi

  BIN="$ENGINE_DIR/target/release/flapjack"
  if [ ! -x "$BIN" ]; then
    echo "ERROR: expected binary at $BIN" >&2
    exit 1
  fi
}

start_server() {
  local wait_helper
  TMP_DATA="$(mktemp -d)"
  ADMIN_KEY="$(generate_admin_key)"
  wait_helper="$REPO_DIR/engine/tests/common/wait_for_flapjack.sh"

  FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$TMP_DATA" \
    "$BIN" --auto-port >"$TMP_DATA/server.log" 2>&1 &
  SERVER_PID=$!

  "$wait_helper" --pid "$SERVER_PID" --host 127.0.0.1 --port auto --log-path "$TMP_DATA/server.log" --retries 60 --interval-seconds 0.5
  PORT="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$TMP_DATA/server.log" | head -1)"
  if [ -z "$PORT" ]; then
    echo 'ERROR: server became healthy but no auto-port was found in startup log' >&2
    cat "$TMP_DATA/server.log" >&2 || true
    exit 1
  fi

  BASE="http://127.0.0.1:${PORT}"
  printf 'Server ready at %s (pid %s)\n' "$BASE" "$SERVER_PID"
}

report_summary() {
  printf '\n\033[1mResults: %d/%d passed\033[0m\n' "$TESTS_PASSED" "$TESTS_RUN"
  if [ "$TESTS_FAILED" -gt 0 ]; then
    printf '\033[0;31m%d test(s) failed\033[0m\n' "$TESTS_FAILED"
    return 1
  fi
  printf '\033[0;32mAll tests passed\033[0m\n'
  return 0
}

main() {
  echo 'Integration API Smoke Test'
  require_tools
  build_or_resolve_binary
  start_server

  section 'Data Setup'

  request_json api_request POST '/1/indexes' '{"uid":"smoke_test"}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.uid == "smoke_test"' ; then
    pass 'Create smoke_test index via POST /1/indexes'
  else
    fail 'Create smoke_test index via POST /1/indexes' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  local seed_payload
  seed_payload='{"requests":[{"action":"addObject","body":{"objectID":"doc-nyc-1","title":"Laptop Pro","category":"Electronics","brand":"Acme","price":1299,"_geoloc":{"lat":40.7128,"lng":-74.0060}}},{"action":"addObject","body":{"objectID":"doc-la-1","title":"Hiking Backpack","category":"Outdoors","brand":"TrailWorks","price":159,"_geoloc":{"lat":34.0522,"lng":-118.2437}}},{"action":"addObject","body":{"objectID":"doc-chi-1","title":"Wireless Mouse","category":"Electronics","brand":"Acme","price":49,"_geoloc":{"lat":41.8781,"lng":-87.6298}}},{"action":"addObject","body":{"objectID":"doc-mia-1","title":"Coffee Beans","category":"Grocery","brand":"RoastCo","price":18,"_geoloc":{"lat":25.7617,"lng":-80.1918}}},{"action":"addObject","body":{"objectID":"doc-sf-1","title":"Laptop Sleeve","category":"Electronics","brand":"Acme","price":39,"_geoloc":{"lat":37.7749,"lng":-122.4194}}},{"action":"addObject","body":{"objectID":"doc-chi-2","title":"Gaming Keyboard","category":"Electronics","brand":"InputLab","price":119,"_geoloc":{"lat":41.8810,"lng":-87.6230}}}]}'

  request_json api_request POST "/1/indexes/${SMOKE_INDEX}/batch" "$seed_payload"
  BATCH_TASK_ID="$(extract_task_id "$LAST_BODY")"
  if [ "$LAST_HTTP" = '200' ] && [ -n "$BATCH_TASK_ID" ]; then
    pass "Seed ${SMOKE_INDEX} index via batch import"
    wait_for_task "$BATCH_TASK_ID" || true
  else
    fail "Seed ${SMOKE_INDEX} index via batch import" "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  local setup_settings_task
  request_json api_request PUT "/1/indexes/${SMOKE_INDEX}/settings" '{"attributesForFaceting":["category","brand"],"searchableAttributes":["title","category","brand"]}'
  setup_settings_task="$(extract_task_id "$LAST_BODY")"
  if [ "$LAST_HTTP" = '200' ] && [ -n "$setup_settings_task" ]; then
    pass 'Data setup applies faceting/search settings prerequisites'
    wait_for_task "$setup_settings_task" || true
  else
    fail 'Data setup applies faceting/search settings prerequisites' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  section 'Health + Task Status'

  request_json public_request GET '/health'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.status' ; then
    pass 'GET /health returns status field'
  else
    fail 'GET /health returns status field' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request GET "/1/task/${BATCH_TASK_ID}"
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.status' ; then
    pass 'GET /1/task/:taskID returns status envelope'
  else
    fail 'GET /1/task/:taskID returns status envelope' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  section 'Index CRUD'

  request_json api_request POST '/1/indexes' '{"uid":"smoke_crud_idx"}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.uid == "smoke_crud_idx"' ; then
    pass 'POST /1/indexes creates smoke_crud_idx'
  else
    fail 'POST /1/indexes creates smoke_crud_idx' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request GET '/1/indexes'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.items | map(.name) | index("smoke_crud_idx") != null' ; then
    pass 'GET /1/indexes contains smoke_crud_idx'
  else
    fail 'GET /1/indexes contains smoke_crud_idx' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  local crud_delete_task
  request_json api_request DELETE "/1/indexes/${CRUD_INDEX}"
  crud_delete_task="$(extract_task_id "$LAST_BODY")"
  if [ "$LAST_HTTP" = '200' ] && [ -n "$crud_delete_task" ]; then
    pass 'DELETE /1/indexes/smoke_crud_idx returns task envelope'
    wait_for_task "$crud_delete_task" || true
  else
    fail 'DELETE /1/indexes/smoke_crud_idx returns task envelope' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  section 'Document CRUD'

  local doc_put_payload doc_put_task
  doc_put_payload='{"objectID":"smoke-doc-crud","title":"Smoke Doc","category":"Books","brand":"PageTurner","price":25,"_geoloc":{"lat":40.7306,"lng":-73.9352}}'
  request_json api_request PUT "/1/indexes/${SMOKE_INDEX}/smoke-doc-crud" "$doc_put_payload"
  doc_put_task="$(extract_task_id "$LAST_BODY")"
  if [ "$LAST_HTTP" = '200' ] && [ -n "$doc_put_task" ]; then
    pass 'PUT /1/indexes/smoke_test/:objectID upserts document'
    wait_for_task "$doc_put_task" || true
  else
    fail 'PUT /1/indexes/smoke_test/:objectID upserts document' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request GET "/1/indexes/${SMOKE_INDEX}/smoke-doc-crud"
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.objectID == "smoke-doc-crud" and .title == "Smoke Doc"' ; then
    pass 'GET /1/indexes/smoke_test/:objectID returns matching document'
  else
    fail 'GET /1/indexes/smoke_test/:objectID returns matching document' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  local doc_delete_task
  request_json api_request DELETE "/1/indexes/${SMOKE_INDEX}/smoke-doc-crud"
  doc_delete_task="$(extract_task_id "$LAST_BODY")"
  if [ "$LAST_HTTP" = '200' ] && [ -n "$doc_delete_task" ]; then
    pass 'DELETE /1/indexes/smoke_test/:objectID returns task envelope'
    wait_for_task "$doc_delete_task" || true
  else
    fail 'DELETE /1/indexes/smoke_test/:objectID returns task envelope' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  section 'Search Variants'

  request_json api_request POST "/1/indexes/${SMOKE_INDEX}/query" '{"query":"laptop"}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.hits | type == "array" and .[0].objectID != null' ; then
    pass 'POST /query basic search returns hits array'
  else
    fail 'POST /query basic search returns hits array' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request POST "/1/indexes/${SMOKE_INDEX}/query" '{"query":"lap"}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.nbHits >= 1' ; then
    pass 'Search prefix case returns at least one hit'
  else
    fail 'Search prefix case returns at least one hit' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request POST "/1/indexes/${SMOKE_INDEX}/query" '{"query":"laptpo"}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.nbHits >= 1' ; then
    pass 'Search typo-tolerant case returns at least one hit'
  else
    fail 'Search typo-tolerant case returns at least one hit' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request POST "/1/indexes/${SMOKE_INDEX}/query" '{"query":"","facetFilters":["category:Electronics"]}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.nbHits >= 1 and ([.hits[].category] | all(. == "Electronics"))' ; then
    pass 'Search facetFilters case narrows hits to Electronics'
  else
    fail 'Search facetFilters case narrows hits to Electronics' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request POST "/1/indexes/${SMOKE_INDEX}/query" '{"query":"","aroundLatLng":"40.7128, -74.0060","aroundRadius":200000}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.hits | map(.objectID) | index("doc-nyc-1") != null' ; then
    pass 'Search geo case returns NYC-seeded record'
  else
    fail 'Search geo case returns NYC-seeded record' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request POST "/1/indexes/${SMOKE_INDEX}/query" '{"query":"laptop","attributesToHighlight":["title"]}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.hits[0]._highlightResult.title.value' ; then
    pass 'Search highlight case includes _highlightResult'
  else
    fail 'Search highlight case includes _highlightResult' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  section 'Settings + Synonyms + Rules'

  local settings_put_task
  request_json api_request PUT "/1/indexes/${SMOKE_INDEX}/settings" '{"attributesForFaceting":["category","brand"],"searchableAttributes":["title","brand","category"]}'
  settings_put_task="$(extract_task_id "$LAST_BODY")"
  if [ "$LAST_HTTP" = '200' ] && [ -n "$settings_put_task" ]; then
    pass 'PUT /settings returns write task envelope'
    wait_for_task "$settings_put_task" || true
  else
    fail 'PUT /settings returns write task envelope' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request GET "/1/indexes/${SMOKE_INDEX}/settings"
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.attributesForFaceting and .searchableAttributes' ; then
    pass 'GET /settings returns faceting/search fields'
  else
    fail 'GET /settings returns faceting/search fields' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  local syn_put_task
  request_json api_request PUT "/1/indexes/${SMOKE_INDEX}/synonyms/syn-laptop" '{"objectID":"syn-laptop","type":"synonym","synonyms":["laptop","notebook"]}'
  syn_put_task="$(extract_task_id "$LAST_BODY")"
  if [ "$LAST_HTTP" = '200' ] && [ -n "$syn_put_task" ]; then
    pass 'PUT /synonyms/:id returns write task envelope'
    wait_for_task "$syn_put_task" || true
  else
    fail 'PUT /synonyms/:id returns write task envelope' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request POST "/1/indexes/${SMOKE_INDEX}/synonyms/search" '{"query":"notebook"}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.hits | type == "array" and . != null' ; then
    pass 'POST /synonyms/search returns hits envelope'
  else
    fail 'POST /synonyms/search returns hits envelope' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  local rule_put_task
  request_json api_request PUT "/1/indexes/${SMOKE_INDEX}/rules/rule-sale" '{"objectID":"rule-sale","conditions":[{"pattern":"laptop","context":"search"}],"consequence":{"params":{"query":"laptop"}},"enabled":true}'
  rule_put_task="$(extract_task_id "$LAST_BODY")"
  if [ "$LAST_HTTP" = '200' ] && [ -n "$rule_put_task" ]; then
    pass 'PUT /rules/:id returns write task envelope'
    wait_for_task "$rule_put_task" || true
  else
    fail 'PUT /rules/:id returns write task envelope' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request POST "/1/indexes/${SMOKE_INDEX}/rules/search" '{"query":"laptop"}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.hits | type == "array" and . != null' ; then
    pass 'POST /rules/search returns hits envelope'
  else
    fail 'POST /rules/search returns hits envelope' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  section 'API Keys + Analytics Events'

  request_json public_request POST "/1/indexes/${SMOKE_INDEX}/query" '{"query":"laptop"}'
  if [ "$LAST_HTTP" = '403' ] && json_matches "$LAST_BODY" '.status == 403' ; then
    pass 'Protected search rejects a request without an API key'
  else
    fail 'Protected search rejects a request without an API key' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  local boundary_index='smoke_restricted_boundary'
  request_json api_request POST '/1/indexes' "{\"uid\":\"${boundary_index}\"}"
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" ".uid == \"${boundary_index}\"" ; then
    pass 'Create cross-index boundary fixture'
  else
    fail 'Create cross-index boundary fixture' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request POST '/1/keys' '{"acl":["search","browse"],"indexes":["smoke_test"],"description":"Smoke restricted key"}'
  CREATED_API_KEY="$(printf '%s\n' "$LAST_BODY" | jq -r '.key // empty' 2>/dev/null || true)"
  if [ "$LAST_HTTP" = '200' ] && [ -n "$CREATED_API_KEY" ]; then
    pass 'POST /1/keys creates restricted key'
  else
    fail 'POST /1/keys creates restricted key' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json restricted_request POST "/1/indexes/${SMOKE_INDEX}/query" '{"query":"laptop"}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.nbHits >= 1' ; then
    pass 'Restricted search key can query its allowed index'
  else
    fail 'Restricted search key can query its allowed index' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json restricted_request POST "/1/indexes/${boundary_index}/query" '{"query":""}'
  if [ "$LAST_HTTP" = '403' ] && json_matches "$LAST_BODY" '.status == 403' ; then
    pass 'Restricted search key cannot cross its index boundary'
  else
    fail 'Restricted search key cannot cross its index boundary' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json restricted_request PUT "/1/indexes/${SMOKE_INDEX}/forbidden-write" '{"objectID":"forbidden-write","title":"must not persist"}'
  if [ "$LAST_HTTP" = '403' ] && json_matches "$LAST_BODY" '.status == 403' ; then
    pass 'Search-only key cannot mutate its allowed index'
  else
    fail 'Search-only key cannot mutate its allowed index' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request GET '/1/keys'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.keys | map(.description) | index("Smoke restricted key") != null' ; then
    pass 'GET /1/keys lists created key'
  else
    fail 'GET /1/keys lists created key' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request DELETE "/1/keys/${CREATED_API_KEY}"
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.deletedAt' ; then
    pass 'DELETE /1/keys/:key returns deletion envelope'
  else
    fail 'DELETE /1/keys/:key returns deletion envelope' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  local boundary_delete_task
  request_json api_request DELETE "/1/indexes/${boundary_index}"
  boundary_delete_task="$(extract_task_id "$LAST_BODY")"
  if [ "$LAST_HTTP" = '200' ] && [ -n "$boundary_delete_task" ]; then
    pass 'Delete cross-index boundary fixture'
    wait_for_task "$boundary_delete_task" || true
  else
    fail 'Delete cross-index boundary fixture' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request POST '/1/events' '{"events":[{"eventType":"view","eventName":"Smoke View","index":"smoke_test","userToken":"smoke_user_1","objectIDs":["doc-nyc-1"]}]}'
  if { [ "$LAST_HTTP" = '200' ] || [ "$LAST_HTTP" = '201' ]; } && json_matches "$LAST_BODY" '.status == 200 and .message == "OK"' ; then
    pass 'POST /1/events accepts analytics payload'
  else
    fail 'POST /1/events accepts analytics payload' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  section 'Dashboard + Multi-Index Search + Browse'

  request_json public_request GET '/dashboard/'
  if [ "$LAST_HTTP" = '200' ] \
    || [ "$LAST_HTTP" = '301' ] \
    || [ "$LAST_HTTP" = '302' ] \
    || [ "$LAST_HTTP" = '307' ] \
    || [ "$LAST_HTTP" = '308' ] \
    || { [ "$LAST_HTTP" = '404' ] && json_matches "$LAST_BODY" '.status == 404' ; }; then
    pass 'GET /dashboard/ returns HTML/redirect or 404 JSON envelope when assets are absent'
  else
    fail 'GET /dashboard/ returns HTML/redirect or 404 JSON envelope when assets are absent' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request POST '/1/indexes/*/queries' '{"requests":[{"indexName":"smoke_test","query":"laptop"},{"indexName":"smoke_test","query":"coffee"}]}'
  if [ "$LAST_HTTP" = '200' ] && json_matches "$LAST_BODY" '.results | type == "array" and length == 2' ; then
    pass 'POST /1/indexes/*/queries returns results array'
  else
    fail 'POST /1/indexes/*/queries returns results array' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  request_json api_request POST "/1/indexes/${SMOKE_INDEX}/browse" '{"hitsPerPage":2}'
  if [ "$LAST_HTTP" = '200' ] \
    && json_matches "$LAST_BODY" '.hits | type == "array" and . != null' \
    && json_matches "$LAST_BODY" '.nbHits and .hitsPerPage and .page and .nbPages and has("cursor")' ; then
    pass 'POST /1/indexes/smoke_test/browse returns browse envelope'
  else
    fail 'POST /1/indexes/smoke_test/browse returns browse envelope' "HTTP ${LAST_HTTP} — ${LAST_BODY}"
  fi

  report_summary
}

main "$@"
