#!/usr/bin/env bash
#
# Served provider-parity probe for the shared source-migration lifecycle.
#
# The probe starts the real auth-enabled flapjack-server on an automatic
# loopback port, exercises submit/status/cancel/ACK for every public source
# provider, migrates real Meilisearch and Typesense documents, proves those
# documents through the served search API, self-tests route/tag mutation
# detection, then checks that the shared reader contract is provider-neutral.
#
# Exit codes:
#   0  PASS  — served lifecycle parity and the neutral shared seam both hold.
#   1  RED   — a value-level route/tag or neutral-seam invariant failed.
#   2  INDET — the real server or probe harness could not be established.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_HELPER="$SCRIPT_DIR/common/wait_for_flapjack.sh"
SOURCE_READER="$ENGINE_DIR/flapjack-http/src/handlers/migration/source_reader.rs"
ROUTER_SOURCE="$ENGINE_DIR/flapjack-http/src/router.rs"
TYPESENSE_FIXTURE="$ENGINE_DIR/tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json"

# shellcheck source=lib/source_provider_fixtures.sh
source "$SCRIPT_DIR/lib/source_provider_fixtures.sh"

readonly PROBE_ADMIN_KEY="source-migration-provider-parity-admin-key"
readonly PROBE_APPLICATION_ID="source-migration-provider-parity-app"
readonly UNKNOWN_JOB_ID="01890f8e-8b28-78e8-b542-8cfdcb2d4f24"

BIN=""
TMP=""
LOG=""
SERVER_PID=""
ALGOLIA_STUB_PID=""
ALGOLIA_STUB_BASE=""
MEILI_CONTAINER="fj_source_migration_provider_parity_meili_$$"
MEILI_PORT=""
TYPESENSE_CONTAINER="fj_source_migration_provider_parity_typesense_$$"
TYPESENSE_PORT=""
BASE=""
CHECKS_FAILED=0
CLEANUP_FAILED=0
CLEANUP_FAILURE_OVERRIDES_EXIT="${CLEANUP_FAILURE_OVERRIDES_EXIT:-0}"
EXTRA_OWNED_PIDS=()

die_indeterminate() {
  printf 'SOURCE_MIGRATION_HTTP_PROBE=INDETERMINATE reason=%s\n' "$1" >&2
  if [ -n "$LOG" ] && [ -f "$LOG" ]; then
    tail -80 "$LOG" >&2 || true
  fi
  exit 2
}

fail_red() {
  CHECKS_FAILED=$((CHECKS_FAILED + 1))
  printf 'SOURCE_MIGRATION_HTTP_PROBE=RED %s\n' "$1" >&2
  exit 1
}

mark_cleanup_failure() {
  CLEANUP_FAILED=1
  printf 'SOURCE_MIGRATION_HTTP_PROBE=INDETERMINATE cleanup=%s\n' "$1" >&2
}

pid_state() {
  local pid="$1"
  ps -p "$pid" -o stat= 2>/dev/null | awk 'NR == 1 {print $1}' || true
}

terminate_owned_pid() {
  local label="$1" pid="$2" attempt state wait_rc
  [ -n "$pid" ] || return 0
  if ! kill -0 "$pid" 2>/dev/null; then
    wait "$pid" 2>/dev/null || true
    return 0
  fi
  if ! kill "$pid" 2>/dev/null; then
    mark_cleanup_failure "${label}_term_signal_failed pid=${pid}"
    return 0
  fi
  for attempt in $(seq 1 40); do
    state="$(pid_state "$pid")"
    if [ -z "$state" ] || [[ "$state" == Z* ]]; then
      break
    fi
    sleep 0.1
  done
  state="$(pid_state "$pid")"
  if [ -n "$state" ] && [[ "$state" != Z* ]]; then
    if ! kill -KILL "$pid" 2>/dev/null; then
      mark_cleanup_failure "${label}_kill_signal_failed pid=${pid}"
      return 0
    fi
    for attempt in $(seq 1 40); do
      state="$(pid_state "$pid")"
      if [ -z "$state" ] || [[ "$state" == Z* ]]; then
        break
      fi
      sleep 0.1
    done
  fi
  state="$(pid_state "$pid")"
  if [ -n "$state" ] && [[ "$state" != Z* ]]; then
    mark_cleanup_failure "${label}_pid_still_running pid=${pid} state=${state}"
    return 0
  fi
  set +e
  wait "$pid" 2>/dev/null
  wait_rc=$?
  set -e
  if [ "$wait_rc" -eq 127 ]; then
    mark_cleanup_failure "${label}_wait_failed pid=${pid} rc=${wait_rc}"
  fi
  if kill -0 "$pid" 2>/dev/null; then
    mark_cleanup_failure "${label}_pid_residue pid=${pid}"
  fi
  return 0
}

cleanup_source_provider_container() {
  local provider="$1" container="$2" fixture_dir="$3"
  [ -n "$container" ] || return 0
  if ! source_provider_owned_container_exists "$provider" "$container"; then
    return 0
  fi
  if [ "$provider" = typesense ] && [ -n "$fixture_dir" ]; then
    repair_typesense_data_permissions "$container" "$fixture_dir" || return 0
    ensure_stopped_typesense_data_host_removable "$container" "$fixture_dir" || return 0
  fi
  remove_owned_container "$provider" "$container" || return 0
}

cleanup() {
  local script_exit_code=$?
  local owned_pid_entry owned_pid_label owned_pid
  if { [ "$CHECKS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; } && [ -n "$TMP" ]; then
    [ -z "$MEILI_CONTAINER" ] || docker logs "$MEILI_CONTAINER" >"$TMP/meilisearch.log" 2>&1 || true
    [ -z "$TYPESENSE_CONTAINER" ] || docker logs "$TYPESENSE_CONTAINER" >"$TMP/typesense.log" 2>&1 || true
  fi
  terminate_owned_pid flapjack_server "$SERVER_PID"
  terminate_owned_pid algolia_stub "$ALGOLIA_STUB_PID"
  for owned_pid_entry in ${EXTRA_OWNED_PIDS[@]+"${EXTRA_OWNED_PIDS[@]}"}; do
    owned_pid_label="${owned_pid_entry%%:*}"
    owned_pid="${owned_pid_entry#*:}"
    terminate_owned_pid "$owned_pid_label" "$owned_pid"
  done
  cleanup_source_provider_container meilisearch "$MEILI_CONTAINER" "$TMP"
  cleanup_source_provider_container typesense "$TYPESENSE_CONTAINER" "$TMP"
  if [ -n "$TMP" ] && [ -d "$TMP" ]; then
    if [ "$CHECKS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ] || [ "$CLEANUP_FAILED" -ne 0 ]; then
      printf 'INFO: preserved source migration provider parity evidence at %s\n' "$TMP" >&2
    else
      if ! rm -rf "$TMP"; then
        mark_cleanup_failure "source_migration_fixture_dir_rm_failed dir=${TMP}"
      fi
      if [ -d "$TMP" ]; then
        mark_cleanup_failure "source_migration_fixture_dir_residue dir=${TMP}"
      fi
    fi
  fi
  if [ "$CLEANUP_FAILED" -ne 0 ] && { [ "$script_exit_code" -eq 0 ] || [ "$CLEANUP_FAILURE_OVERRIDES_EXIT" -eq 1 ]; }; then
    exit 2
  fi
  exit "$script_exit_code"
}
require_tools() {
  local tool missing=0
  for tool in awk cargo curl docker grep jq mktemp perl ps python3 sed tail; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  [ "$missing" -eq 0 ] || die_indeterminate 'required_tools_missing'
  [ -x "$WAIT_HELPER" ] || die_indeterminate 'wait_helper_not_executable'
  [ -f "$SOURCE_READER" ] || die_indeterminate 'source_reader_missing'
  [ -f "$ROUTER_SOURCE" ] || die_indeterminate 'router_source_missing'
  [ -f "$TYPESENSE_FIXTURE" ] || die_indeterminate 'typesense_fixture_missing'
}

start_algolia_stub() {
  cat >"$TMP/algolia_stub.py" <<'PY'
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/1/indexes?page=0&hitsPerPage=100":
            self.send_response(404); self.end_headers(); return
        if self.headers.get("x-algolia-application-id") != "ParityApp1" or self.headers.get("x-algolia-api-key") != "algolia-discovery-key":
            self.send_response(401); self.end_headers(); return
        body = json.dumps({"items":[{"name":"algolia_products","entries":7,"updatedAt":"2026-08-02T05:00:00Z"}],"page":0,"nbPages":1}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *_):
        pass

server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
PY
  python3 "$TMP/algolia_stub.py" >"$TMP/algolia_stub.port" 2>"$TMP/algolia_stub.log" &
  ALGOLIA_STUB_PID=$!
  local attempt port=""
  for attempt in $(seq 1 80); do
    kill -0 "$ALGOLIA_STUB_PID" 2>/dev/null || die_indeterminate 'algolia_stub_exited'
    port="$(sed -n '1p' "$TMP/algolia_stub.port")"
    [ -n "$port" ] && break
    sleep 0.1
  done
  [ -n "$port" ] || die_indeterminate 'algolia_stub_port_missing'
  ALGOLIA_STUB_BASE="http://127.0.0.1:${port}"
}

start_discovery_upstreams() {
  start_algolia_stub
  start_meilisearch
  start_typesense
}

configure_source_provider_owner_token() {
  local token_suffix
  [ -n "$TMP" ] || die_indeterminate 'source_provider_owner_token_tmp_missing'
  token_suffix="${TMP##*.}"
  [ "$token_suffix" != "$TMP" ] || token_suffix="${TMP##*/}"
  SOURCE_PROVIDER_OWNER_TOKEN="source_migration_provider_parity_$$_${token_suffix}"
  export SOURCE_PROVIDER_OWNER_TOKEN
}

target_dir() {
  if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    printf '%s\n' "$ENGINE_DIR/target"
  elif [ "${CARGO_TARGET_DIR#/}" != "$CARGO_TARGET_DIR" ]; then
    printf '%s\n' "$CARGO_TARGET_DIR"
  else
    printf '%s\n' "$ENGINE_DIR/$CARGO_TARGET_DIR"
  fi
}

build_or_resolve_binary() {
  if [ -n "${FLAPJACK_BIN:-}" ]; then
    [ -x "$FLAPJACK_BIN" ] || die_indeterminate 'configured_binary_not_executable'
    BIN="$FLAPJACK_BIN"
    return
  fi

  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$TMP/build.log" 2>&1); then
    tail -40 "$TMP/build.log" >&2 || true
    die_indeterminate 'flapjack_server_build_failed'
  fi
  BIN="$(target_dir)/debug/flapjack"
  [ -x "$BIN" ] || die_indeterminate 'built_binary_missing'
}

start_server() {
  local data_dir="$TMP/data"
  mkdir -p "$data_dir"
  LOG="$TMP/server.log"
  env \
    -u FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND \
    -u FLAPJACK_BIND_ADDR \
    -u FLAPJACK_NO_AUTH \
    -u FLAPJACK_PORT \
    FLAPJACK_ADMIN_KEY="$PROBE_ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$data_dir" \
    FLAPJACK_TEST_ALGOLIA_BASE_URL="$ALGOLIA_STUB_BASE" \
    FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=1 \
    FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1 \
    "$BIN" --auto-port >"$LOG" 2>&1 &
  SERVER_PID=$!

  if ! "$WAIT_HELPER" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$LOG" \
    --retries 80 \
    --interval-seconds 0.5; then
    die_indeterminate 'server_readiness_failed'
  fi

  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9][0-9]*\).*/\1/p' "$LOG" | head -1)"
  [ -n "$port" ] || die_indeterminate 'auto_port_missing_from_log'
  BASE="http://127.0.0.1:${port}"
}

served_request() {
  local label="$1" method="$2" path="$3" body="$4" expected_status="$5"
  local body_file="$TMP/${label}.json"
  local status curl_exit

  set +e
  status="$(curl -sS --connect-timeout 2 --max-time 30 \
    -o "$body_file" \
    -w '%{http_code}' \
    -X "$method" \
    -H 'Content-Type: application/json' \
    -H "x-algolia-application-id: ${PROBE_APPLICATION_ID}" \
    -H "x-algolia-api-key: ${PROBE_ADMIN_KEY}" \
    --data "$body" \
    "${BASE}${path}")"
  curl_exit=$?
  set -e

  [ "$curl_exit" -eq 0 ] \
    || die_indeterminate "served_transport label=${label} expected=0 actual=${curl_exit}"
  [ "$status" = "$expected_status" ] || fail_red \
    "served_status label=${label} expected=${expected_status} actual=${status} body=$(jq -c . "$body_file" 2>/dev/null || true)"
}

assert_served_error() {
  local label="$1" expected_code="$2" expected_message="$3"
  local body_file="$TMP/${label}.json" observed_code observed_message
  observed_code="$(jq -r '.code // empty' "$body_file" 2>/dev/null || true)"
  if [ -n "$expected_code" ] && [ "$observed_code" != "$expected_code" ]; then
    fail_red "served_code label=${label} expected=${expected_code} actual=${observed_code:-missing}"
  fi
  observed_message="$(jq -r '.message // empty' "$body_file" 2>/dev/null || true)"
  if [ -n "$expected_message" ] && [ "$observed_message" != "$expected_message" ]; then
    fail_red "served_provider_tag label=${label} expected=${expected_message} actual=${observed_message:-missing}"
  fi
}

served_discovery_request() {
  local label="$1" path="$2" body="$3" expected_status="$4"
  local body_file="$TMP/${label}.json" status curl_exit
  set +e
  status="$(curl -sS --connect-timeout 2 --max-time 20 \
    -o "$body_file" -w '%{http_code}' -X POST \
    -H 'Content-Type: application/json' \
    -H "x-algolia-application-id: ${PROBE_APPLICATION_ID}" \
    -H "x-algolia-api-key: ${PROBE_ADMIN_KEY}" \
    --data "$body" "${BASE}${path}")"
  curl_exit=$?
  set -e
  [ "$curl_exit" -eq 0 ] || die_indeterminate \
    "served_discovery_transport label=${label} expected=0 actual=${curl_exit}"
  [ "$status" = "$expected_status" ] || fail_red \
    "served_discovery_status label=${label} expected=${expected_status} actual=${status} body=$(jq -c . "$body_file" 2>/dev/null || true)"
}

served_preview_request() {
  local label="$1" path="$2" body="$3" expected_status="$4"
  local body_file="$TMP/${label}.json" status curl_exit
  : >"$body_file"
  set +e
  status="$(curl -sS --connect-timeout 2 --max-time 30 \
    -o "$body_file" -w '%{http_code}' -X POST \
    -H 'Content-Type: application/json' \
    -H "x-algolia-application-id: ${PROBE_APPLICATION_ID}" \
    -H "x-algolia-api-key: ${PROBE_ADMIN_KEY}" \
    --data "$body" "${BASE}${path}")"
  curl_exit=$?
  set -e
  [ "$curl_exit" -eq 0 ] || fail_red "served_preview_transport label=${label} expected=0 actual=${curl_exit}"
  [ "$status" = "$expected_status" ] || fail_red \
    "served_preview_status label=${label} expected=${expected_status} actual=${status} body=$(jq -c . "$body_file" 2>/dev/null || true)"
  printf '%s\n' "$status" >"$TMP/${label}_status.txt"
}

poll_served_migration() {
  local provider="$1" job_id="$2" label="$3" attempt status curl_exit disposition
  local body_file="$TMP/${label}_terminal.json"
  for attempt in $(seq 1 240); do
    : >"$body_file"
    set +e
    status="$(curl -sS --connect-timeout 2 --max-time 10 \
      -o "$body_file" -w '%{http_code}' \
      -H "x-algolia-application-id: ${PROBE_APPLICATION_ID}" \
      -H "x-algolia-api-key: ${PROBE_ADMIN_KEY}" \
      "${BASE}/1/migrations/${provider}/${job_id}")"
    curl_exit=$?
    set -e
    [ "$curl_exit" -eq 0 ] || die_indeterminate \
      "served_migration_poll_transport label=${label} expected=0 actual=${curl_exit}"
    [ "$status" = 200 ] || fail_red \
      "served_migration_poll_status label=${label} expected=200 actual=${status} body=$(jq -c . "$body_file" 2>/dev/null || true)"
    disposition="$(jq -er '.disposition' "$body_file" 2>/dev/null || true)"
    case "$disposition" in
      succeeded)
        # A successful terminal migration serializes phase `activating` (the
        # final phase in the pipeline, AsyncMigrationPhase's highest variant),
        # never `completed` — no such phase exists in the API enum. Terminality
        # is carried by disposition=`succeeded` plus a string `terminalAt`. This
        # matches the product contract asserted in async_status_tests.rs.
        jq -e --arg job_id "$job_id" '
          .jobId == $job_id and .phase == "activating" and
          .disposition == "succeeded" and (.terminalAt | type == "string")
        ' "$body_file" >/dev/null || fail_red \
          "served_migration_terminal_body_mismatch label=${label} body=$(jq -c . "$body_file" 2>/dev/null || true)"
        return 0
        ;;
      failed|cancelled)
        fail_red \
          "served_migration_terminal_failure label=${label} disposition=${disposition} body=$(jq -c . "$body_file" 2>/dev/null || true)"
        ;;
      running)
        sleep 0.25
        ;;
      *)
        fail_red \
          "served_migration_disposition_invalid label=${label} actual=${disposition:-missing} body=$(jq -c . "$body_file" 2>/dev/null || true)"
        ;;
    esac
  done
  fail_red "served_migration_terminal_timeout label=${label} attempts=${attempt}"
}

run_served_migration() {
  local provider="$1" label="$2" body="$3" job_id
  served_request "${label}_submit" POST "/1/migrations/${provider}" "$body" 202
  job_id="$(jq -er '.jobId' "$TMP/${label}_submit.json" 2>/dev/null || true)"
  [ -n "$job_id" ] || fail_red \
    "served_migration_job_id_missing label=${label} body=$(jq -c . "$TMP/${label}_submit.json" 2>/dev/null || true)"
  poll_served_migration "$provider" "$job_id" "$label"
  served_request "${label}_ack" POST \
    "/1/migrations/${provider}/${job_id}/acknowledge" '' 204
}

served_search() {
  local label="$1" index_name="$2" query="$3" body
  body="$(jq -cn --arg query "$query" '{query:$query,hitsPerPage:20}')"
  served_request "$label" POST "/1/indexes/${index_name}/query" "$body" 200
}

assert_meilisearch_discovery_body() {
  local body_file="$1" failure="$2"
  jq -n -e --slurpfile actual "$body_file" \
    --slurpfile source "$TMP/meili_expected_listing.json" '
      $source[0].results[0] as $expected |
      $actual[0].indexes == [{
        name:$expected.uid, primaryKey:$expected.primaryKey, entries:null,
        documentCount:2, createdAt:$expected.createdAt,
        updatedAt:$expected.updatedAt, defaultSortingField:null
      }] and $actual[0].total == 1 and $actual[0].offset == 0 and $actual[0].limit == 10
    ' >/dev/null || fail_red "$failure body=$(jq -c . "$body_file" 2>/dev/null || true)"
}

assert_typesense_discovery_body() {
  local body_file="$1" failure="$2"
  jq -n -e --slurpfile actual "$body_file" \
    --slurpfile source "$TMP/typesense_expected_listing.json" '
      def expected_summary:
        {name,primaryKey:null,entries:null,documentCount:.num_documents,
         createdAt:.created_at,updatedAt:null,
         defaultSortingField:(.default_sorting_field // null)};
      ($source[0] | map(expected_summary)) as $expected |
      ($expected | map(.name)) == ["fj_ts_migration_products","fj_ts_migration_categories"] and
      $actual[0] == {indexes:$expected}
    ' >/dev/null || fail_red "$failure body=$(jq -c . "$body_file" 2>/dev/null || true)"
}

assert_meilisearch_landed_documents() {
  local body_file="$1" failure="$2"
  jq -e '
    .nbHits == 2 and (.hits | length) == 2 and
    ([.hits[] | {objectID,sku,title,price,stock}] | sort_by(.objectID)) == [
      {objectID:"MEILI-001",sku:"MEILI-001",title:"Espresso Tamper",price:24.5,stock:7},
      {objectID:"MEILI-002",sku:"MEILI-002",title:"Pour Over Kettle",price:39.75,stock:3}
    ]
  ' "$body_file" >/dev/null || fail_red "$failure body=$(jq -c . "$body_file" 2>/dev/null || true)"
}

assert_typesense_categories_landed_documents() {
  local body_file="$1" failure="$2"
  jq -e '.nbHits == 1 and (.hits | length) == 1 and
    (.hits[0] | {objectID,id,name,priority,active,labels}) ==
      {objectID:"cat_1",id:"cat_1",name:"Coffee",priority:1,active:true,labels:["coffee"]}' \
    "$body_file" >/dev/null || fail_red "$failure body=$(jq -c . "$body_file" 2>/dev/null || true)"
}

assert_typesense_products_landed_documents() {
  local body_file="$1" failure="$2"
  jq -e '
    .nbHits == 2 and (.hits | length) == 2 and
    ([.hits[] | {objectID,id,title,sku,price,inventory,available,tags,category_id}] | sort_by(.objectID)) == [
      {objectID:"prod_1",id:"prod_1",title:"Espresso",sku:"ESP-001",price:12.5,inventory:8,available:true,tags:["coffee"],category_id:"cat_1"},
      {objectID:"prod_2",id:"prod_2",title:"Latte",sku:"LAT-002",price:9.5,inventory:5,available:true,tags:["coffee","milk"],category_id:"cat_1"}
    ]
  ' "$body_file" >/dev/null || fail_red "$failure body=$(jq -c . "$body_file" 2>/dev/null || true)"
}

# Prove migrated provider documents are searchable in Flapjack with the exact
# values the source served. Every expected value below is a contract check, not
# a sample: MEILI-001 / MEILI-002 are the `sku` values of the seeded
# `configured_pk` index, and `cat_1` / `prod_1` / `prod_2` are the seeded
# Typesense `id` values. They appear as `objectID` because
# `meilisearch_source_reader.rs` pins the index's declared `primaryKey` field
# (here `sku`) to `objectID`, and `typesense_source_reader.rs` pins the
# document's `id` field to `objectID`. The seeded scalar, boolean, array, and
# reference fields are asserted byte-for-byte so a translation that lands
# documents but corrupts, rounds, or drops a field still fails red. Do not relax
# these to counts or field presence — that would turn the proof back into a
# smoke test.
probe_served_migrated_data() {
  run_served_migration meilisearch meilisearch_configured_pk \
    "{\"endpoint\":\"http://127.0.0.1:${MEILI_PORT}\",\"apiKey\":\"${MEILI_KEY}\",\"sourceIndex\":\"configured_pk\"}"
  run_served_migration typesense typesense_categories \
    "{\"node\":\"http://127.0.0.1:${TYPESENSE_PORT}\",\"apiKey\":\"${TYPESENSE_KEY}\",\"sourceIndex\":\"${TYPESENSE_CATEGORIES}\",\"sourceWriteFrozen\":true}"
  run_served_migration typesense typesense_products \
    "{\"node\":\"http://127.0.0.1:${TYPESENSE_PORT}\",\"apiKey\":\"${TYPESENSE_KEY}\",\"sourceIndex\":\"${TYPESENSE_PRODUCTS}\",\"sourceWriteFrozen\":true}"

  served_search meilisearch_all configured_pk ''
  assert_meilisearch_landed_documents "$TMP/meilisearch_all.json" \
    'meilisearch_all_documents_mismatch'
  served_search meilisearch_espresso configured_pk 'Espresso Tamper'
  jq -e '.nbHits == 1 and (.hits | length) == 1 and
    (.hits[0] | {objectID,sku,title,price,stock}) ==
      {objectID:"MEILI-001",sku:"MEILI-001",title:"Espresso Tamper",price:24.5,stock:7}' \
    "$TMP/meilisearch_espresso.json" >/dev/null || fail_red \
    "meilisearch_espresso_mismatch body=$(jq -c . "$TMP/meilisearch_espresso.json" 2>/dev/null || true)"
  served_search meilisearch_kettle configured_pk 'Pour Over Kettle'
  jq -e '.nbHits == 1 and (.hits | length) == 1 and
    (.hits[0] | {objectID,sku,title,price,stock}) ==
      {objectID:"MEILI-002",sku:"MEILI-002",title:"Pour Over Kettle",price:39.75,stock:3}' \
    "$TMP/meilisearch_kettle.json" >/dev/null || fail_red \
    "meilisearch_kettle_mismatch body=$(jq -c . "$TMP/meilisearch_kettle.json" 2>/dev/null || true)"

  served_search typesense_categories_all "$TYPESENSE_CATEGORIES" ''
  assert_typesense_categories_landed_documents "$TMP/typesense_categories_all.json" \
    'typesense_category_all_documents_mismatch'
  served_search typesense_category_coffee "$TYPESENSE_CATEGORIES" Coffee
  jq -e '.nbHits == 1 and (.hits | length) == 1 and
    (.hits[0] | {objectID,id,name,priority,active,labels}) ==
      {objectID:"cat_1",id:"cat_1",name:"Coffee",priority:1,active:true,labels:["coffee"]}' \
    "$TMP/typesense_category_coffee.json" >/dev/null || fail_red \
    "typesense_category_coffee_mismatch body=$(jq -c . "$TMP/typesense_category_coffee.json" 2>/dev/null || true)"

  served_search typesense_products_all "$TYPESENSE_PRODUCTS" ''
  assert_typesense_products_landed_documents "$TMP/typesense_products_all.json" \
    'typesense_product_all_documents_mismatch'
  served_search typesense_product_espresso "$TYPESENSE_PRODUCTS" Espresso
  jq -e '.nbHits == 1 and (.hits | length) == 1 and
    (.hits[0] | {objectID,id,title,sku,price,inventory,available,tags,category_id}) ==
      {objectID:"prod_1",id:"prod_1",title:"Espresso",sku:"ESP-001",price:12.5,inventory:8,available:true,tags:["coffee"],category_id:"cat_1"}' \
    "$TMP/typesense_product_espresso.json" >/dev/null || fail_red \
    "typesense_product_espresso_mismatch body=$(jq -c . "$TMP/typesense_product_espresso.json" 2>/dev/null || true)"
  served_search typesense_product_latte "$TYPESENSE_PRODUCTS" Latte
  jq -e '.nbHits == 1 and (.hits | length) == 1 and
    (.hits[0] | {objectID,id,title,sku,price,inventory,available,tags,category_id}) ==
      {objectID:"prod_2",id:"prod_2",title:"Latte",sku:"LAT-002",price:9.5,inventory:5,available:true,tags:["coffee","milk"],category_id:"cat_1"}' \
    "$TMP/typesense_product_latte.json" >/dev/null || fail_red \
    "typesense_product_latte_mismatch body=$(jq -c . "$TMP/typesense_product_latte.json" 2>/dev/null || true)"

  printf 'LANDED_DATA meilisearch=PASS configured_pk=2 identity=sku_to_objectID\n'
  printf 'LANDED_DATA typesense=PASS categories=1 products=2 identity=id_to_objectID\n'
}

probe_served_lifecycle() {
  served_request \
    'algolia_submit' POST '/1/migrations/algolia' \
    '{"appId":"","apiKey":"","sourceIndex":""}' 400
  assert_served_error algolia_submit '' 'appId, apiKey, and sourceIndex are required'
  served_request \
    'meilisearch_submit' POST '/1/migrations/meilisearch' \
    '{"endpoint":"","apiKey":"","sourceIndex":""}' 400
  assert_served_error meilisearch_submit '' 'endpoint, apiKey, and sourceIndex are required'
  served_request \
    'typesense_submit' POST '/1/migrations/typesense' \
    '{"node":"","apiKey":"","sourceIndex":""}' 400
  assert_served_error typesense_submit '' 'node, apiKey, and sourceIndex are required'

  local provider
  for provider in algolia meilisearch typesense; do
    served_request \
      "${provider}_status" GET "/1/migrations/${provider}/${UNKNOWN_JOB_ID}" '' 404
    assert_served_error "${provider}_status" migration_job_not_found ''
    served_request \
      "${provider}_cancel" POST "/1/migrations/${provider}/${UNKNOWN_JOB_ID}/cancel" '' 404
    assert_served_error "${provider}_cancel" migration_job_not_found ''
    served_request \
      "${provider}_ack" POST "/1/migrations/${provider}/${UNKNOWN_JOB_ID}/acknowledge" '' 404
    assert_served_error "${provider}_ack" migration_job_not_found ''
  done
}

probe_served_discovery() {
  served_discovery_request algolia_discovery '/1/migrations/algolia/list-indexes' \
    '{"appId":"ParityApp1","apiKey":"algolia-discovery-key"}' 200
  jq -e '. == {"indexes":[{"name":"algolia_products","primaryKey":null,"entries":7,"documentCount":null,"createdAt":null,"updatedAt":"2026-08-02T05:00:00Z","defaultSortingField":null}]}' \
    "$TMP/algolia_discovery.json" >/dev/null \
    || fail_red 'algolia_discovery_body_mismatch'

  served_discovery_request meilisearch_discovery \
    '/1/migrations/meilisearch/list-indexes?offset=0&limit=10' \
    "{\"endpoint\":\"http://127.0.0.1:${MEILI_PORT}\",\"apiKey\":\"${MEILI_KEY}\"}" 200
  assert_meilisearch_discovery_body "$TMP/meilisearch_discovery.json" \
    'meilisearch_discovery_body_mismatch'

  served_discovery_request typesense_discovery \
    '/1/migrations/typesense/list-indexes?offset=0&limit=2' \
    "{\"node\":\"http://127.0.0.1:${TYPESENSE_PORT}\",\"apiKey\":\"${TYPESENSE_KEY}\"}" 200
  assert_typesense_discovery_body "$TMP/typesense_discovery.json" \
    'typesense_discovery_body_mismatch'

  served_discovery_request meilisearch_localhost_refused \
    '/1/migrations/meilisearch/list-indexes' \
    "{\"endpoint\":\"http://localhost:${MEILI_PORT}\",\"apiKey\":\"${MEILI_KEY}\"}" 400
  jq -e '.message == "Meilisearch Cloud endpoint is not allowed"' \
    "$TMP/meilisearch_localhost_refused.json" >/dev/null \
    || fail_red 'meilisearch_localhost_refusal_mismatch'
  served_discovery_request typesense_localhost_refused \
    '/1/migrations/typesense/list-indexes' \
    "{\"node\":\"http://localhost:${TYPESENSE_PORT}\",\"apiKey\":\"${TYPESENSE_KEY}\"}" 400
  jq -e '.message == "Typesense Cloud endpoint is not allowed"' \
    "$TMP/typesense_localhost_refused.json" >/dev/null \
    || fail_red 'typesense_localhost_refusal_mismatch'

  local leaks
  leaks="$(grep -F -e "$PROBE_ADMIN_KEY" -e 'algolia-discovery-key' -e "$MEILI_KEY" -e "$TYPESENSE_KEY" \
    "$TMP"/*_discovery.json "$TMP"/*_localhost_refused.json 2>/dev/null || true)"
  [ -z "$leaks" ] || fail_red 'served_discovery_credential_leak'
  printf 'PASS: served discovery response and refusal bodies contain no source or admin credentials\n' \
    >"$TMP/credential_leak_scan.txt"
  printf 'DISCOVERY algolia=%s\n' "$(jq -c . "$TMP/algolia_discovery.json")"
  printf 'DISCOVERY meilisearch=%s\n' "$(jq -c . "$TMP/meilisearch_discovery.json")"
  printf 'DISCOVERY typesense=%s\n' "$(jq -c . "$TMP/typesense_discovery.json")"
  printf 'DISCOVERY localhost_refusal=PASS providers=meilisearch,typesense\n'
  cat "$TMP/credential_leak_scan.txt"
}

probe_served_typesense_preview() {
  served_preview_request typesense_preview '/1/migrations/typesense/preview' \
    "{\"node\":\"http://127.0.0.1:${TYPESENSE_PORT}\",\"apiKey\":\"${TYPESENSE_KEY}\",\"sourceIndex\":\"${TYPESENSE_PRODUCTS}\",\"targetIndex\":\"shop\",\"sourceWriteFrozen\":true}" 200
  jq -e '
    keys == ["report","sourceCounts"] and
    .sourceCounts == {"indexes":1,"records":2} and
    .report.entries == [
      {"severity":"ScopeGap","code":"ProductNotMigrated","resource":"Analytics","pageIndex":null,"itemIndex":null,"jsonPath":"$"},
      {"severity":"ScopeGap","code":"ProductNotMigrated","resource":"ApiKeys","pageIndex":null,"itemIndex":null,"jsonPath":"$"},
      {"severity":"ScopeGap","code":"ProductNotMigrated","resource":"Events","pageIndex":null,"itemIndex":null,"jsonPath":"$"},
      {"severity":"ScopeGap","code":"ProductNotMigrated","resource":"Experiments","pageIndex":null,"itemIndex":null,"jsonPath":"$"},
      {"severity":"ScopeGap","code":"ProductNotMigrated","resource":"Recommend","pageIndex":null,"itemIndex":null,"jsonPath":"$"},
      {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.curation_sets"},
      {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.default_sorting_field"},
      {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.fields[10]"},
      {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.fields[11]"},
      {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.symbols_to_index"},
      {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.synonym_sets"},
      {"severity":"Warning","code":"TypesenseSettingNotMigrated","resource":"Settings","pageIndex":null,"itemIndex":null,"jsonPath":"$.token_separators"}
    ] and
    .report.summary == {"totalEntries":12,"hardRejections":0,"warnings":7,"scopeGaps":5} and
    ([.report.entries[]
      | select(.code == "TypesenseSettingNotMigrated" and (.jsonPath | test("^\\$\\.fields\\[[0-9]+\\]$")))
      | .jsonPath] | sort) == ["$.fields[10]","$.fields[11]"]
  ' "$TMP/typesense_preview.json" >/dev/null || fail_red \
    "typesense_preview_body_mismatch body=$(jq -c . "$TMP/typesense_preview.json" 2>/dev/null || true)"

  local leaks
  leaks="$(grep -F -e "$PROBE_ADMIN_KEY" -e "$TYPESENSE_KEY" \
    "$TMP/typesense_preview.json" 2>/dev/null || true)"
  [ -z "$leaks" ] || fail_red 'served_typesense_preview_credential_leak'
  printf 'PASS: served Typesense preview response contains no source or admin credentials\n' \
    >>"$TMP/credential_leak_scan.txt"
  printf 'PREVIEW request={"node":"http://127.0.0.1:%s","apiKey":"[REDACTED_TYPESENSE_KEY]","sourceIndex":"%s","targetIndex":"shop","sourceWriteFrozen":true}\n' \
    "$TYPESENSE_PORT" "$TYPESENSE_PRODUCTS"
  printf 'PREVIEW status=%s\n' "$(cat "$TMP/typesense_preview_status.txt")"
  printf 'PREVIEW sourceCounts=%s\n' "$(jq -c .sourceCounts "$TMP/typesense_preview.json")"
  printf 'PREVIEW report_entries=%s\n' "$(jq -c .report.entries "$TMP/typesense_preview.json")"
  printf 'PREVIEW summary=%s\n' "$(jq -c .report.summary "$TMP/typesense_preview.json")"
  printf 'PREVIEW credential_leak_scan=PASS\n'
}

validate_route_tag_contract() {
  local openapi_file="$1" provider lifecycle route method expected_operation expected_schema observed
  for provider in algolia meilisearch typesense; do
    for lifecycle in submit status cancel ack discovery preview; do
      expected_schema=''
      case "$lifecycle" in
        submit)
          route="/1/migrations/${provider}"
          method='post'
          expected_operation="submit_${provider}_migration"
          case "$provider" in
            algolia) expected_schema='#/components/schemas/MigrateFromAlgoliaRequest' ;;
            meilisearch) expected_schema='#/components/schemas/MigrateFromMeilisearchRequest' ;;
            typesense) expected_schema='#/components/schemas/MigrateFromTypesenseRequest' ;;
          esac
          ;;
        status)
          route="/1/migrations/${provider}/{job_id}"
          method='get'
          expected_operation="get_${provider}_migration_status"
          ;;
        cancel)
          route="/1/migrations/${provider}/{job_id}/cancel"
          method='post'
          expected_operation="cancel_${provider}_migration"
          ;;
        ack)
          route="/1/migrations/${provider}/{job_id}/acknowledge"
          method='post'
          expected_operation="acknowledge_${provider}_migration"
          ;;
        discovery)
          route="/1/migrations/${provider}/list-indexes"
          method='post'
          expected_operation="list_${provider}_source_indexes_doc"
          case "$provider" in
            algolia) expected_schema='#/components/schemas/ListAlgoliaIndexesRequest' ;;
            meilisearch) expected_schema='#/components/schemas/ListMeilisearchIndexesRequest' ;;
            typesense) expected_schema='#/components/schemas/ListTypesenseIndexesRequest' ;;
          esac
          ;;
        preview)
          route="/1/migrations/${provider}/preview"
          method='post'
          expected_operation="preview_${provider}_migration"
          case "$provider" in
            algolia) expected_schema='#/components/schemas/MigrateFromAlgoliaRequest' ;;
            meilisearch) expected_schema='#/components/schemas/MigrateFromMeilisearchRequest' ;;
            typesense) expected_schema='#/components/schemas/MigrateFromTypesenseRequest' ;;
          esac
          ;;
      esac
      observed="$(jq -r --arg route "$route" --arg method "$method" '
        .paths[$route][$method]
        | [
            (.operationId // ""),
            ((.tags // []) | join(",")),
            (.requestBody.content["application/json"].schema["$ref"] // "")
          ]
        | @tsv
      ' "$openapi_file")"
      [ "$observed" = "${expected_operation}"$'\t''migration'$'\t'"${expected_schema}" ] || return 1
    done
  done
}

probe_route_tag_mutations() {
  local canonical="$TMP/live_openapi.json"
  local route_mutation="$TMP/route_mutation_openapi.json"
  local tag_mutation="$TMP/tag_mutation_openapi.json"
  local status_mutation="$TMP/status_mutation_openapi.json"
  local cancel_mutation="$TMP/cancel_mutation_openapi.json"
  local ack_mutation="$TMP/ack_mutation_openapi.json"
  local discovery_route_mutation="$TMP/discovery_route_mutation_openapi.json"
  local discovery_schema_mutation="$TMP/discovery_schema_mutation_openapi.json"
  local preview_route_mutation="$TMP/preview_route_mutation_openapi.json"
  local preview_schema_mutation="$TMP/preview_schema_mutation_openapi.json"
  if ! curl -sS -o "$canonical" "${BASE}/api-docs/openapi.json"; then
    die_indeterminate 'served_openapi_unreachable'
  fi

  validate_route_tag_contract "$canonical" || fail_red 'served_route_tag_contract_mismatch'
  jq 'del(.paths["/1/migrations/typesense"])' "$canonical" >"$route_mutation"
  if validate_route_tag_contract "$route_mutation"; then
    fail_red 'route_mutation_not_detected provider=typesense injected_route=missing'
  fi
  jq '.paths["/1/migrations/typesense"].post.operationId = "submit_meilisearch_migration"' \
    "$canonical" >"$tag_mutation"
  if validate_route_tag_contract "$tag_mutation"; then
    fail_red 'tag_mutation_not_detected provider=typesense injected_tag=meilisearch'
  fi
  jq '.paths["/1/migrations/meilisearch/{job_id}"].get.operationId = "get_algolia_migration_status"' \
    "$canonical" >"$status_mutation"
  if validate_route_tag_contract "$status_mutation"; then
    fail_red 'status_mutation_not_detected provider=meilisearch injected_operation=algolia'
  fi
  jq '.paths["/1/migrations/algolia/{job_id}/cancel"].post.tags = ["typesense"]' \
    "$canonical" >"$cancel_mutation"
  if validate_route_tag_contract "$cancel_mutation"; then
    fail_red 'cancel_tag_mutation_not_detected provider=algolia injected_tag=typesense'
  fi
  jq '.paths["/1/migrations/typesense/{job_id}/acknowledge"].post.operationId = "acknowledge_meilisearch_migration"' \
    "$canonical" >"$ack_mutation"
  if validate_route_tag_contract "$ack_mutation"; then
    fail_red 'ack_mutation_not_detected provider=typesense injected_operation=meilisearch'
  fi
  jq 'del(.paths["/1/migrations/typesense/list-indexes"])' \
    "$canonical" >"$discovery_route_mutation"
  if validate_route_tag_contract "$discovery_route_mutation"; then
    fail_red 'discovery_route_mutation_not_detected provider=typesense injected_route=missing'
  fi
  jq '.paths["/1/migrations/meilisearch/list-indexes"].post.requestBody.content["application/json"].schema["$ref"] = "#/components/schemas/ListTypesenseIndexesRequest"' \
    "$canonical" >"$discovery_schema_mutation"
  if validate_route_tag_contract "$discovery_schema_mutation"; then
    fail_red 'discovery_schema_mutation_not_detected provider=meilisearch injected_schema=typesense'
  fi
  jq 'del(.paths["/1/migrations/typesense/preview"])' \
    "$canonical" >"$preview_route_mutation"
  if validate_route_tag_contract "$preview_route_mutation"; then
    fail_red 'preview_route_mutation_not_detected provider=typesense injected_route=missing'
  fi
  jq '.paths["/1/migrations/typesense/preview"].post.requestBody.content["application/json"].schema["$ref"] = "#/components/schemas/MigrateFromAlgoliaRequest"' \
    "$canonical" >"$preview_schema_mutation"
  if validate_route_tag_contract "$preview_schema_mutation"; then
    fail_red 'preview_schema_mutation_not_detected provider=typesense injected_schema=algolia'
  fi
}

strip_rust_comments() {
  perl -0pe 's@/\*.*?\*/@@gs; s@//[^\n]*@@g' "$1"
}

# Resolves a Rust name through `type X = Y;` alias chains and `use path::Orig as
# Local;` import aliases to the name it ultimately stands for, so contract checks
# see the owning symbol rather than the local spelling. Prints the name unchanged
# when it is not aliased.
resolve_declared_alias_root() {
  local source_file="$1"
  local declared_name="$2"
  perl - "$source_file" "$declared_name" <<'EOF_RESOLVE_ALIAS'
my ($file, $target) = @ARGV;
open my $handle, '<', $file or do { print "$target\n"; exit 0 };
local $/;
my $source = <$handle>;
my %alias;
while ($source =~ /\btype\s+([A-Za-z_]\w*)\s*(?:<[^=;]*>)?\s*=\s*([^;]+);/gs) {
    $alias{$1} = $2 unless exists $alias{$1};
}
while ($source =~ /\buse\s+([^;]+);/gs) {
    my $statement = $1;
    while ($statement =~ /([A-Za-z_][\w:]*)\s+as\s+([A-Za-z_]\w*)/gs) {
        $alias{$2} = $1 unless exists $alias{$2};
    }
}
my %visited;
my $name = $target;
while (!$visited{$name}++) {
    my $right_hand_side = $alias{$name};
    last unless defined $right_hand_side;
    $right_hand_side =~ s/\s+//g;
    last unless $right_hand_side =~ /^(?:[A-Za-z_]\w*::)*([A-Za-z_]\w*)$/;
    $name = $1;
}
print "$name\n";
EOF_RESOLVE_ALIAS
}

router_registration_contract() {
  local source_file="$1"
  strip_rust_comments "$source_file" \
    | sed -n '/fn register_source_migration_routes/,/fn build_protected_routes/p'
}

validate_live_router_binding_contract() {
  local source_file="$1"
  local contract="$TMP/router_contract.$(basename "$source_file").rs"
  local stripped="$TMP/router_stripped.$(basename "$source_file").rs"
  local submit_handler
  local status_handler
  local cancel_handler
  local ack_handler
  strip_rust_comments "$source_file" >"$stripped"
  router_registration_contract "$source_file" >"$contract"
  [ -s "$contract" ] || die_indeterminate 'router_registration_contract_not_found'

  perl -0ne 'exit(/for\s+source_provider\s+in\s+AsyncMigrationSourceProvider::PUBLIC/s ? 0 : 1)' "$contract" || return 1
  perl -0ne 'exit(/format!\("\/1\/migrations\/\{provider\}"\)/s ? 0 : 1)' "$contract" || return 1
  submit_handler="$(perl -0ne 'print $1 if /&provider_path\s*,\s*post\s*\(\s*([a-zA-Z0-9_]+)\s*\)\s*\.layer\s*\(\s*Extension\s*\(\s*source_provider\s*\)\s*\)/s' "$contract")"
  status_handler="$(perl -0ne 'print $1 if /&job_path\s*,\s*get\s*\(\s*([a-zA-Z0-9_]+)\s*\)\s*\.layer\s*\(\s*Extension\s*\(\s*source_provider\s*\)\s*\)/s' "$contract")"
  cancel_handler="$(perl -0ne 'print $1 if /&format!\("\{job_path\}\/cancel"\)\s*,\s*post\s*\(\s*([a-zA-Z0-9_]+)\s*\)\s*\.layer\s*\(\s*Extension\s*\(\s*source_provider\s*\)\s*\)/s' "$contract")"
  ack_handler="$(perl -0ne 'print $1 if /&format!\("\{job_path\}\/acknowledge"\)\s*,\s*post\s*\(\s*([a-zA-Z0-9_]+)\s*\)\s*\.layer\s*\(\s*Extension\s*\(\s*source_provider\s*\)\s*\)/s' "$contract")"
  [ -n "$submit_handler" ] && [ -n "$status_handler" ] && [ -n "$cancel_handler" ] && [ -n "$ack_handler" ] || return 1

  # Lifecycle semantics belong to the handler the route ultimately calls, not to
  # the identifier spelled at the binding site: an import alias can spell a
  # cancel handler `submit_...` and still compile.
  submit_handler="$(resolve_declared_alias_root "$stripped" "$submit_handler")"
  status_handler="$(resolve_declared_alias_root "$stripped" "$status_handler")"
  cancel_handler="$(resolve_declared_alias_root "$stripped" "$cancel_handler")"
  ack_handler="$(resolve_declared_alias_root "$stripped" "$ack_handler")"

  case "$submit_handler" in *submit*) ;; *) return 1 ;; esac
  case "$status_handler" in *status*) ;; *) return 1 ;; esac
  case "$cancel_handler" in *cancel*) ;; *) return 1 ;; esac
  case "$ack_handler" in *acknowledge*|*ack*) ;; *) return 1 ;; esac
  [ "$submit_handler" != "$status_handler" ] || return 1
  [ "$submit_handler" != "$cancel_handler" ] || return 1
  [ "$submit_handler" != "$ack_handler" ] || return 1
  [ "$status_handler" != "$cancel_handler" ] || return 1
  [ "$status_handler" != "$ack_handler" ] || return 1
  [ "$cancel_handler" != "$ack_handler" ] || return 1
}

probe_live_router_binding_mutations() {
  local status_mutation="$TMP/router_status_mutation.rs"
  local cancel_mutation="$TMP/router_cancel_mutation.rs"
  local ack_mutation="$TMP/router_ack_mutation.rs"
  local same_handler_mutation="$TMP/router_same_handler_mutation.rs"
  local swapped_handler_mutation="$TMP/router_swapped_handler_mutation.rs"
  local import_alias_swap_mutation="$TMP/router_import_alias_swap_mutation.rs"
  local rename_control="$TMP/router_handler_rename_control.rs"

  validate_live_router_binding_contract "$ROUTER_SOURCE" || fail_red 'live_router_binding_contract_mismatch'

  perl -0pe 's/post\s*\(\s*submit_algolia_migration_http\s*\)/post(run_source_migration_http)/; s/get\s*\(\s*get_algolia_migration_status_http\s*\)/get(run_source_migration_http)/; s/post\s*\(\s*cancel_algolia_migration_http\s*\)/post(run_source_migration_http)/; s/post\s*\(\s*acknowledge_algolia_migration_http\s*\)/post(run_source_migration_http)/' \
    "$ROUTER_SOURCE" >"$same_handler_mutation"
  if validate_live_router_binding_contract "$same_handler_mutation"; then
    fail_red 'live_router_same_handler_mutation_not_detected injected_handler=run_source_migration_http'
  fi

  perl -0pe 's/post\s*\(\s*submit_algolia_migration_http\s*\)/post(cancel_source_migration_http)/; s/post\s*\(\s*cancel_algolia_migration_http\s*\)/post(submit_source_migration_http)/' \
    "$ROUTER_SOURCE" >"$swapped_handler_mutation"
  if validate_live_router_binding_contract "$swapped_handler_mutation"; then
    fail_red 'live_router_swapped_handler_mutation_not_detected swap=submit,cancel'
  fi

  perl -0pe 's/cancel_algolia_migration_http, cancel_bulk_replace_http,/cancel_algolia_migration_http as submit_slot_calls_cancel_handler, cancel_bulk_replace_http,/; s/submit_algolia_migration_http,\n/submit_algolia_migration_http as cancel_slot_calls_submit_handler,\n/; s/post\(submit_algolia_migration_http\)/post(submit_slot_calls_cancel_handler)/; s/post\(cancel_algolia_migration_http\)/post(cancel_slot_calls_submit_handler)/' \
    "$ROUTER_SOURCE" >"$import_alias_swap_mutation"
  grep -q 'as submit_slot_calls_cancel_handler' "$import_alias_swap_mutation" \
    && grep -q 'as cancel_slot_calls_submit_handler' "$import_alias_swap_mutation" \
    && grep -q 'post(submit_slot_calls_cancel_handler)' "$import_alias_swap_mutation" \
    && grep -q 'post(cancel_slot_calls_submit_handler)' "$import_alias_swap_mutation" \
    || die_indeterminate 'router_import_alias_swap_mutation_not_applied'
  if validate_live_router_binding_contract "$import_alias_swap_mutation"; then
    fail_red 'live_router_import_alias_swap_mutation_not_detected swap=submit,cancel'
  fi

  perl -0pe 's/submit_algolia_migration_http/submit_source_migration_http/g; s/get_algolia_migration_status_http/source_migration_status_http/g; s/cancel_algolia_migration_http/cancel_source_migration_http/g; s/acknowledge_algolia_migration_http/ack_source_migration_http/g' \
    "$ROUTER_SOURCE" >"$rename_control"
  validate_live_router_binding_contract "$rename_control" \
    || fail_red 'live_router_handler_rename_control_rejected action_semantics=preserved'

  perl -0pe 's/&job_path,\s*get\s*\(\s*([a-zA-Z0-9_]+)\s*\)/&job_path, post($1)/' \
    "$ROUTER_SOURCE" >"$status_mutation"
  if validate_live_router_binding_contract "$status_mutation"; then
    fail_red 'live_router_status_binding_mutation_not_detected injected_method=post'
  fi

  perl -0pe 's/(&format!\("\{job_path\}\/cancel"\),\s*post\s*\(\s*[a-zA-Z0-9_]+\s*\))\.layer\s*\(\s*Extension\s*\(\s*source_provider\s*\)\s*\)/$1/' \
    "$ROUTER_SOURCE" >"$cancel_mutation"
  if validate_live_router_binding_contract "$cancel_mutation"; then
    fail_red 'live_router_cancel_extension_mutation_not_detected missing_provider_extension=cancel'
  fi

  perl -0pe 's/(&format!\("\{job_path\}\/acknowledge"\),\s*post\s*\(\s*[a-zA-Z0-9_]+\s*\)\.layer\(Extension\()source_provider(\)\))/$1AsyncMigrationSourceProvider::Algolia$2/' \
    "$ROUTER_SOURCE" >"$ack_mutation"
  if validate_live_router_binding_contract "$ack_mutation"; then
    fail_red 'live_router_ack_extension_mutation_not_detected injected_provider=algolia'
  fi
}

validate_neutral_shared_contract() {
  local source_file="$1"
  local source_contract="$TMP/source_reader_contract.$(basename "$source_file").txt"
  local trait_contract="$TMP/migration_source_reader_trait.$(basename "$source_file").txt"
  local future_contract="$TMP/source_future_alias.$(basename "$source_file").txt"
  local missing
  local neutral_type
  local forbidden_root
  local resolved_root
  strip_rust_comments "$source_file" >"$source_contract"
  sed -n '/pub(super) trait MigrationSourceReader/,/^}/p' "$source_contract" >"$trait_contract"
  sed -n '/type SourceFuture/,/;/p' "$source_contract" >"$future_contract"
  [ -s "$trait_contract" ] || die_indeterminate 'migration_source_reader_trait_not_found'
  [ -s "$future_contract" ] || die_indeterminate 'source_future_alias_not_found'

  missing=''
  # A neutral name is only neutral if it does not resolve — directly, through a
  # re-export, or through any chain of intermediate aliases — back to the
  # Algolia-shaped or raw-JSON type it is supposed to replace.
  while IFS='=' read -r neutral_type forbidden_root; do
    [ -n "$neutral_type" ] || continue
    resolved_root="$(resolve_declared_alias_root "$source_contract" "$neutral_type")"
    [ "$resolved_root" != "$forbidden_root" ] \
      || missing="${missing}${neutral_type} resolves to ${forbidden_root},"
  done <<'EOF_NEUTRAL_TYPE_ROOTS'
SourceExportError=AlgoliaClientError
SourceExportRecord=AlgoliaIndexRecord
SourceConfigurationArtifact=Value
EOF_NEUTRAL_TYPE_ROOTS
  perl -0ne 'exit(/type\s+SourceFuture\b.*?=.*?Result\s*<\s*T\s*,\s*SourceExportError\s*,?\s*>/s ? 0 : 1)' "$future_contract" \
    || missing="${missing}SourceExportError,"
  perl -0ne 'exit(/type\s+SourceDocumentPageConsumer\b.*?Vec\s*<\s*SourceExportRecord\s*>.*?SourceExportError/s ? 0 : 1)' "$source_contract" \
    || missing="${missing}SourceExportRecord,"
  perl -0ne 'exit(/type\s+SourceConfigurationConsumer\b.*?SourceConfigurationArtifact.*?SourceExportError/s ? 0 : 1)' "$source_contract" \
    || missing="${missing}SourceConfigurationArtifact,"
  grep -Eq 'fn[[:space:]]+source_provider[[:space:]]*\(&self\)[[:space:]]*->' "$trait_contract" \
    || missing="${missing}fn source_provider,"
  grep -Eq 'fn[[:space:]]+source_namespace[[:space:]]*\(&self\)[[:space:]]*->' "$trait_contract" \
    || missing="${missing}fn source_namespace,"
  grep -Eq 'fn[[:space:]]+observe_quiescent_source[[:space:]]*\(' "$trait_contract" \
    || missing="${missing}fn observe_quiescent_source,"
  grep -Eq 'fn[[:space:]]+read_configuration[[:space:]]*<' "$trait_contract" \
    || grep -Eq 'fn[[:space:]]+read_configuration[[:space:]]*\(' "$trait_contract" \
    || missing="${missing}fn read_configuration,"
  grep -Eq 'fn[[:space:]]+read_derived_configuration[[:space:]]*<' "$trait_contract" \
    || grep -Eq 'fn[[:space:]]+read_derived_configuration[[:space:]]*\(' "$trait_contract" \
    || missing="${missing}fn read_derived_configuration,"
  grep -Eq 'fn[[:space:]]+read_document_records[[:space:]]*<' "$trait_contract" \
    || grep -Eq 'fn[[:space:]]+read_document_records[[:space:]]*\(' "$trait_contract" \
    || missing="${missing}fn read_document_records,"
  grep -Eq 'fn[[:space:]]+wait_for_quiescent_source[[:space:]]*\(' "$trait_contract" \
    && missing="${missing}retained fn wait_for_quiescent_source,"
  grep -Eq 'fn[[:space:]]+read_configuration_artifacts[[:space:]]*[<(]' "$trait_contract" \
    && missing="${missing}retained fn read_configuration_artifacts,"
  grep -Eq 'fn[[:space:]]+app_id[[:space:]]*\(' "$trait_contract" \
    && missing="${missing}retained fn app_id,"
  grep -Eq 'fn[[:space:]]+read_rules[[:space:]]*[<(]' "$trait_contract" \
    && missing="${missing}retained fn read_rules,"
  grep -Eq 'fn[[:space:]]+read_synonyms[[:space:]]*[<(]' "$trait_contract" \
    && missing="${missing}retained fn read_synonyms,"
  missing="${missing%,}"
  [ -n "$missing" ] || return 0
  # The caller reports exactly what this predicate found, so the red text can
  # never drift from the invariant that actually failed.
  printf '%s\n' "$missing"
  return 1
}

assert_alias_bypass_does_not_satisfy_neutral_contract() {
  local alias_bypass="$TMP/source_reader_alias_bypass.rs"
  local qualified_alias_bypass="$TMP/source_reader_qualified_alias_bypass.rs"
  local reexport_bypass="$TMP/source_reader_reexport_bypass.rs"
  local indirect_alias_bypass="$TMP/source_reader_indirect_alias_bypass.rs"
  cat >"$alias_bypass" <<'EOF_ALIAS_BYPASS'
pub(super) type SourceExportError = AlgoliaClientError;
pub(super) type SourceExportRecord = AlgoliaIndexRecord;
pub(super) type SourceConfigurationArtifact = Value;

pub(super) type SourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SourceExportError>> + Send + 'a>>;

pub(super) trait MigrationSourceReader {
    fn source_provider(&self) -> &str;
    fn source_namespace(&self) -> Option<&str>;
    fn source_name(&self) -> &str;
    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, SourceExportRecord>;
    fn read_settings(&mut self) -> SourceFuture<'_, Value>;
    fn read_configuration_artifacts<'a>(
        &'a mut self,
        consume_artifact: &'a mut SourceConfigurationArtifactConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
    fn read_documents<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
}
EOF_ALIAS_BYPASS
  if validate_neutral_shared_contract "$alias_bypass" >/dev/null; then
    fail_red 'neutral_contract_alias_bypass_not_detected algolia_type_aliases=accepted'
  fi

  cat >"$qualified_alias_bypass" <<'EOF_QUALIFIED_ALIAS_BYPASS'
pub(super) type SourceExportError = crate::handlers::migration::AlgoliaClientError;
pub(super) type SourceExportRecord = crate::handlers::migration::AlgoliaIndexRecord;
pub(super) type SourceConfigurationArtifact = serde_json::Value;

pub(super) type SourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SourceExportError>> + Send + 'a>>;

pub(super) trait MigrationSourceReader {
    fn source_provider(&self) -> &str;
    fn source_namespace(&self) -> Option<&str>;
    fn source_name(&self) -> &str;
    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, SourceExportRecord>;
    fn read_settings(&mut self) -> SourceFuture<'_, Value>;
    fn read_configuration_artifacts<'a>(
        &'a mut self,
        consume_artifact: &'a mut SourceConfigurationArtifactConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
    fn read_documents<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
}
EOF_QUALIFIED_ALIAS_BYPASS
  if validate_neutral_shared_contract "$qualified_alias_bypass" >/dev/null; then
    fail_red 'neutral_contract_qualified_alias_bypass_not_detected algolia_type_aliases=accepted'
  fi

  cat >"$reexport_bypass" <<'EOF_REEXPORT_BYPASS'
pub(super) use crate::handlers::migration::AlgoliaClientError as SourceExportError;
pub(super) use crate::handlers::migration::AlgoliaIndexRecord as SourceExportRecord;
pub(super) use serde_json::Value as SourceConfigurationArtifact;

pub(super) type SourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SourceExportError>> + Send + 'a>>;

pub(super) trait MigrationSourceReader {
    fn source_provider(&self) -> &str;
    fn source_namespace(&self) -> Option<&str>;
    fn source_name(&self) -> &str;
    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, SourceExportRecord>;
    fn read_settings(&mut self) -> SourceFuture<'_, Value>;
    fn read_configuration_artifacts<'a>(
        &'a mut self,
        consume_artifact: &'a mut SourceConfigurationArtifactConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
    fn read_documents<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
}
EOF_REEXPORT_BYPASS
  if validate_neutral_shared_contract "$reexport_bypass" >/dev/null; then
    fail_red 'neutral_contract_reexport_bypass_not_detected algolia_reexports=accepted'
  fi

  cat >"$indirect_alias_bypass" <<'EOF_INDIRECT_ALIAS_BYPASS'
pub(super) type LegacyExportError = AlgoliaClientError;
pub(super) type LegacyExportRecord = crate::handlers::migration::AlgoliaIndexRecord;
pub(super) use serde_json::Value as LegacyArtifact;

pub(super) type SourceExportError = LegacyExportError;
pub(super) type SourceExportRecord = LegacyExportRecord;
pub(super) type SourceConfigurationArtifact = LegacyArtifact;

pub(super) type SourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SourceExportError>> + Send + 'a>>;

pub(super) trait MigrationSourceReader {
    fn source_provider(&self) -> &str;
    fn source_namespace(&self) -> Option<&str>;
    fn source_name(&self) -> &str;
    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, SourceExportRecord>;
    fn read_configuration_artifacts<'a>(
        &'a mut self,
        consume_artifact: &'a mut SourceConfigurationArtifactConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
}
EOF_INDIRECT_ALIAS_BYPASS
  if validate_neutral_shared_contract "$indirect_alias_bypass" >/dev/null; then
    fail_red 'neutral_contract_indirect_alias_bypass_not_detected algolia_alias_chain=accepted'
  fi
}

assert_comment_only_contract_tokens_do_not_satisfy_neutral_contract() {
  local comment_bypass="$TMP/source_reader_comment_bypass.rs"
  cat >"$comment_bypass" <<'EOF_COMMENT_BYPASS'
/// SourceExportError SourceExportRecord SourceConfigurationArtifact
/// fn observe_quiescent_source fn read_configuration fn read_derived_configuration fn read_document_records
pub(super) type SourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SourceExportError>> + Send + 'a>>;

pub(super) type SourceDocumentPageConsumer<'a> =
    dyn FnMut(Vec<Value>) -> Result<(), SourceExportError> + Send + 'a;

pub(super) type SourceConfigurationConsumer<'a> =
    dyn FnMut(Value) -> Result<(), SourceExportError> + Send + 'a;

pub(super) trait MigrationSourceReader {
    fn source_provider(&self) -> &str;
    fn source_namespace(&self) -> Option<&str>;
    fn source_name(&self) -> &str;
}
EOF_COMMENT_BYPASS
  if validate_neutral_shared_contract "$comment_bypass" >/dev/null; then
    fail_red 'neutral_contract_comment_bypass_not_detected comment_only_contract=accepted'
  fi
}

assert_retained_legacy_members_do_not_satisfy_neutral_contract() {
  local retained_legacy="$TMP/source_reader_retained_legacy.rs"
  cat >"$retained_legacy" <<'EOF_NEUTRAL_RETAINED'
pub(super) type SourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SourceExportError>> + Send + 'a>>;

pub(super) trait MigrationSourceReader {
    fn source_provider(&self) -> &str;
    fn source_namespace(&self) -> Option<&str>;
    fn source_name(&self) -> &str;
    fn app_id(&self) -> &str;
    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, SourceExportRecord>;
    fn read_configuration_artifacts<'a>(
        &'a mut self,
        consume_artifact: &'a mut SourceConfigurationArtifactConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
    fn read_rules<'a>(&'a mut self, consume_page: &'a mut PageConsumer<'a>) -> SourceFuture<'a, ()>;
    fn read_synonyms<'a>(&'a mut self, consume_page: &'a mut PageConsumer<'a>) -> SourceFuture<'a, ()>;
}
EOF_NEUTRAL_RETAINED
  if validate_neutral_shared_contract "$retained_legacy" >/dev/null; then
    fail_red 'neutral_contract_retained_legacy_members_not_detected retained=app_id,read_rules,read_synonyms'
  fi
}

assert_multiline_source_future_alias_satisfies_neutral_contract() {
  local multiline_neutral="$TMP/source_reader_multiline_neutral.rs"
  cat >"$multiline_neutral" <<'EOF_MULTILINE_NEUTRAL'
pub(super) type SourceFuture<'a, T> = Pin<
    Box<
        dyn Future<
                Output = Result<
                    T,
                    SourceExportError,
                >,
            > + Send
            + 'a,
    >,
>;

pub(super) type SourceDocumentPageConsumer<'a> =
    dyn FnMut(Vec<SourceExportRecord>) -> Result<(), SourceExportError> + Send + 'a;

pub(super) type SourceConfigurationConsumer<'a> =
    dyn FnMut(SourceConfigurationArtifact) -> Result<(), SourceExportError> + Send + 'a;

pub(super) trait MigrationSourceReader {
    fn source_provider(&self) -> &str;
    fn source_namespace(&self) -> Option<&str>;
    fn source_name(&self) -> &str;
    fn observe_quiescent_source(&mut self) -> SourceFuture<'_, SourceObservation>;
    fn read_configuration<'a>(
        &'a mut self,
        consume: &'a mut SourceConfigurationConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
    fn read_derived_configuration<'a>(
        &'a mut self,
        consume: &'a mut SourceConfigurationConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
    fn read_document_records<'a>(
        &'a mut self,
        consume_page: &'a mut SourceDocumentPageConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
}
EOF_MULTILINE_NEUTRAL
  validate_neutral_shared_contract "$multiline_neutral" >/dev/null \
    || fail_red 'neutral_contract_multiline_source_future_rejected rustfmt_alias=normal'
}

probe_neutral_shared_seam() {
  local missing
  assert_alias_bypass_does_not_satisfy_neutral_contract
  assert_comment_only_contract_tokens_do_not_satisfy_neutral_contract
  assert_retained_legacy_members_do_not_satisfy_neutral_contract
  assert_multiline_source_future_alias_satisfies_neutral_contract
  if ! missing="$(validate_neutral_shared_contract "$SOURCE_READER")"; then
    fail_red "missing_neutral_shared_seam invariant=provider_neutral_reader_contract missing_contract_members=${missing}"
  fi
}

write_parity_cleanup_fake_bins() {
  local fake_bin="$1"
  mkdir -p "$fake_bin"
  cat >"$fake_bin/docker" <<'FAKE_DOCKER'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${PARITY_CLEANUP_TEST_DOCKER_LOG:?}"
state_dir="${PARITY_CLEANUP_TEST_DOCKER_STATE:?}"

case "${1:-}" in
  ps)
    ps_count_file="$state_dir/ps_count"
    ps_count=0
    [ -f "$ps_count_file" ] && ps_count="$(cat "$ps_count_file")"
    ps_count=$((ps_count + 1))
    printf '%s\n' "$ps_count" >"$ps_count_file"
    if [ -n "${PARITY_CLEANUP_TEST_FAIL_PS_CALL:-}" ] && [ "$ps_count" -eq "${PARITY_CLEANUP_TEST_FAIL_PS_CALL}" ]; then
      printf 'fake docker: ps failed on call %s\n' "$ps_count" >&2
      exit 1
    fi
    name_filter=""
    include_stopped=0
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a) include_stopped=1; shift ;;
        --filter) name_filter="${2:-}"; shift 2 ;;
        *) shift ;;
      esac
    done
    name="${name_filter#name=^/}"
    name="${name%$}"
    if [ -n "$name" ] && [ -f "$state_dir/$name.provider" ] \
      && { [ "$include_stopped" -eq 1 ] || [ ! -f "$state_dir/$name.stopped" ]; }; then
      printf '%s\n' "$name"
    fi
    ;;
  inspect)
    format="${3:-}"
    name="${4:-}"
    case "$format" in
      *flapjack.source_provider_fixture.token*) cat "$state_dir/$name.token" 2>/dev/null || true ;;
      *flapjack.source_provider_fixture.provider*) cat "$state_dir/$name.provider" 2>/dev/null || true ;;
      *flapjack.source_provider_fixture*) [ -f "$state_dir/$name.provider" ] && printf '1\n' ;;
    esac
    ;;
  logs)
    exit 0
    ;;
  exec)
    name="${2:-}"
    shift 2
    printf 'docker exec %s %s\n' "$name" "$*" >>"${PARITY_CLEANUP_TEST_ORDER_LOG:?}"
    [ "${PARITY_CLEANUP_TEST_FAIL_EXEC_FOR:-}" != "$name" ] || exit 1
    ;;
  rm)
    [ "${2:-}" = "-f" ] || exit 97
    name="${3:-}"
    printf 'docker rm -f %s\n' "$name" >>"${PARITY_CLEANUP_TEST_ORDER_LOG:?}"
    rm -f -- "$state_dir/$name.provider" "$state_dir/$name.token" "$state_dir/$name.stopped"
    ;;
  *)
    exit 97
    ;;
esac
FAKE_DOCKER
  chmod +x "$fake_bin/docker"

  cat >"$fake_bin/rm" <<'FAKE_RM'
#!/usr/bin/env bash
set -euo pipefail
for arg in "$@"; do
  if [ -n "${PARITY_CLEANUP_TEST_RM_FAIL_DIR:-}" ] && [ "$arg" = "$PARITY_CLEANUP_TEST_RM_FAIL_DIR" ]; then
    exit 19
  fi
done
exec /bin/rm "$@"
FAKE_RM
  chmod +x "$fake_bin/rm"
}

register_parity_cleanup_fake_container() {
  local state_dir="$1" name="$2" provider="$3" token="$4"
  printf '%s\n' "$provider" >"$state_dir/$name.provider"
  printf '%s\n' "$token" >"$state_dir/$name.token"
}

run_parity_cleanup_contract_child() {
  local scenario="${SOURCE_MIGRATION_PROVIDER_PARITY_CLEANUP_CHILD:?}"
  TMP="${PARITY_CLEANUP_TEST_FIXTURE_DIR:?}"
  MEILI_CONTAINER=""
  SERVER_PID=""
  ALGOLIA_STUB_PID=""
  EXTRA_OWNED_PIDS=()
  SOURCE_PROVIDER_OWNER_TOKEN="${PARITY_CLEANUP_TEST_EXPECTED_TOKEN:-parity_cleanup_token}"
  export SOURCE_PROVIDER_OWNER_TOKEN
  case "$scenario" in
    unowned_typesense|repair_failure|running_state_ps_failure)
      TYPESENSE_CONTAINER="${PARITY_CLEANUP_TEST_TYPESENSE_CONTAINER:?}"
      mkdir -p "$TMP/$TYPESENSE_HOST_DATA_SUBDIR"
      touch "$TMP/$TYPESENSE_HOST_DATA_SUBDIR/marker"
      ;;
    rm_failure)
      TYPESENSE_CONTAINER=""
      mkdir -p "$TMP"
      ;;
    *)
      die_indeterminate "unknown_cleanup_contract_child scenario=${scenario}"
      ;;
  esac
  trap cleanup EXIT
  exit 0
}

assert_parity_cleanup_file_contains() {
  local file="$1" expected="$2"
  grep -F "$expected" "$file" >/dev/null || {
    printf 'expected %s to contain %s, saw:\n' "$file" "$expected" >&2
    cat "$file" >&2
    exit 1
  }
}

assert_parity_cleanup_file_excludes() {
  local file="$1" rejected="$2"
  ! grep -F "$rejected" "$file" >/dev/null || {
    printf 'did not expect %s in %s, saw:\n' "$rejected" "$file" >&2
    cat "$file" >&2
    exit 1
  }
}

run_parity_cleanup_contract_case() {
  local root="$1" scenario="$2" container token output status fixture_dir state_dir
  container="fj_source_migration_provider_parity_typesense_99101"
  token="parity_cleanup_expected_token"
  fixture_dir="$root/${scenario}_fixture"
  state_dir="$root/docker_state"
  mkdir -p "$fixture_dir" "$state_dir"
  : >"$root/docker.log"
  : >"$root/order.log"
  rm -f -- "$state_dir/ps_count"
  register_parity_cleanup_fake_container "$state_dir" "$container" typesense "$token"
  case "$scenario" in
    unowned_typesense)
      printf 'foreign_token\n' >"$state_dir/$container.token"
      ;;
    repair_failure)
      ;;
    running_state_ps_failure)
      ;;
    rm_failure)
      rm -f -- "$state_dir/$container.provider" "$state_dir/$container.token"
      ;;
  esac

  set +e
  output="$(env \
    PATH="$root/bin:$PATH" \
    PARITY_CLEANUP_TEST_DOCKER_LOG="$root/docker.log" \
    PARITY_CLEANUP_TEST_DOCKER_STATE="$state_dir" \
    PARITY_CLEANUP_TEST_EXPECTED_TOKEN="$token" \
    PARITY_CLEANUP_TEST_FIXTURE_DIR="$fixture_dir" \
    PARITY_CLEANUP_TEST_ORDER_LOG="$root/order.log" \
    PARITY_CLEANUP_TEST_TYPESENSE_CONTAINER="$container" \
    PARITY_CLEANUP_TEST_FAIL_EXEC_FOR="$([ "$scenario" = repair_failure ] && printf '%s' "$container")" \
    PARITY_CLEANUP_TEST_FAIL_PS_CALL="$([ "$scenario" = running_state_ps_failure ] && printf '2')" \
    PARITY_CLEANUP_TEST_RM_FAIL_DIR="$([ "$scenario" = rm_failure ] && printf '%s' "$fixture_dir")" \
    SOURCE_MIGRATION_PROVIDER_PARITY_CLEANUP_CHILD="$scenario" \
    bash "$0" 2>&1)"
  status=$?
  set -e
  printf '%s\n' "$output" >"$root/${scenario}.out"
  [ "$status" -eq 2 ] || {
    printf 'expected cleanup contract %s to exit 2, got %s\noutput=%s\n' "$scenario" "$status" "$output" >&2
    exit 1
  }
}

assert_parity_unowned_cleanup_contract() {
  local root="$1" container="fj_source_migration_provider_parity_typesense_99101"
  run_parity_cleanup_contract_case "$root" unowned_typesense
  assert_parity_cleanup_file_contains "$root/unowned_typesense.out" \
    "SOURCE_MIGRATION_HTTP_PROBE=INDETERMINATE cleanup=typesense_container_unowned_label name=$container"
  assert_parity_cleanup_file_excludes "$root/order.log" "docker exec $container"
  assert_parity_cleanup_file_excludes "$root/order.log" "docker rm -f $container"
  [ -f "$root/docker_state/$container.provider" ] || exit 1
  [ -f "$root/unowned_typesense_fixture/$TYPESENSE_HOST_DATA_SUBDIR/marker" ] || exit 1
}

assert_parity_repair_failure_contract() {
  local root="$1" container="fj_source_migration_provider_parity_typesense_99101"
  run_parity_cleanup_contract_case "$root" repair_failure
  assert_parity_cleanup_file_contains "$root/repair_failure.out" \
    "SOURCE_MIGRATION_HTTP_PROBE=INDETERMINATE cleanup=typesense_data_permission_repair_failed name=$container"
  assert_parity_cleanup_file_contains "$root/order.log" "docker exec $container"
  assert_parity_cleanup_file_excludes "$root/order.log" "docker rm -f $container"
  [ -f "$root/docker_state/$container.provider" ] || exit 1
  [ -f "$root/repair_failure_fixture/$TYPESENSE_HOST_DATA_SUBDIR/marker" ] || exit 1
}

assert_parity_running_state_ps_failure_contract() {
  local root="$1" container="fj_source_migration_provider_parity_typesense_99101"
  run_parity_cleanup_contract_case "$root" running_state_ps_failure
  assert_parity_cleanup_file_contains "$root/running_state_ps_failure.out" \
    "SOURCE_MIGRATION_HTTP_PROBE=INDETERMINATE cleanup=docker_ps_failed name=$container"
  assert_parity_cleanup_file_excludes "$root/order.log" "docker exec $container"
  assert_parity_cleanup_file_excludes "$root/order.log" "docker rm -f $container"
  [ -f "$root/docker_state/$container.provider" ] || exit 1
  [ -f "$root/running_state_ps_failure_fixture/$TYPESENSE_HOST_DATA_SUBDIR/marker" ] || exit 1
}

assert_parity_rm_failure_contract() {
  local root="$1"
  run_parity_cleanup_contract_case "$root" rm_failure
  assert_parity_cleanup_file_contains "$root/rm_failure.out" \
    "SOURCE_MIGRATION_HTTP_PROBE=INDETERMINATE cleanup=source_migration_fixture_dir_rm_failed dir=$root/rm_failure_fixture"
  assert_parity_cleanup_file_contains "$root/rm_failure.out" \
    "SOURCE_MIGRATION_HTTP_PROBE=INDETERMINATE cleanup=source_migration_fixture_dir_residue dir=$root/rm_failure_fixture"
  [ -d "$root/rm_failure_fixture" ] || exit 1
}

assert_typesense_host_removable_argument_contract() {
  local root="$1" direct_fixture_path
  direct_fixture_path="$root/direct_argument_fixture"
  mkdir -p "$direct_fixture_path/$TYPESENSE_HOST_DATA_SUBDIR"
  typesense_data_host_removable "$direct_fixture_path" || {
    printf 'expected Typesense host-removability helper to honor its direct argument\n' >&2
    exit 1
  }
}

run_parity_cleanup_contract_tests() {
  local root
  root="$(mktemp -d "${TMPDIR:-/tmp}/fj_source_migration_provider_parity_cleanup_test.XXXXXX")"
  write_parity_cleanup_fake_bins "$root/bin"
  assert_typesense_host_removable_argument_contract "$root"
  assert_parity_unowned_cleanup_contract "$root"
  assert_parity_repair_failure_contract "$root"
  assert_parity_running_state_ps_failure_contract "$root"
  assert_parity_rm_failure_contract "$root"
  rm -rf -- "$root"
  printf 'SOURCE_MIGRATION_PROVIDER_PARITY_CLEANUP_TEST=PASS cases=direct_argument,unowned_typesense,repair_failure,running_state_ps_failure,rm_failure\n'
}

run_parity_startup_token_contract_child() {
  require_tools() { :; }
  build_or_resolve_binary() { :; }
  start_discovery_upstreams() {
    env | grep -Fqx "SOURCE_PROVIDER_OWNER_TOKEN=${SOURCE_PROVIDER_OWNER_TOKEN:-}" \
      || fail_red 'source_provider_owner_token_not_exported_before_upstreams'
    [ -n "${SOURCE_PROVIDER_OWNER_TOKEN:-}" ] \
      || fail_red 'source_provider_owner_token_empty_before_upstreams'
    source_provider_docker_labels meilisearch
    printf '%s\n' "${SOURCE_PROVIDER_DOCKER_LABEL_ARGS[@]}" \
      >"${PARITY_STARTUP_TOKEN_TEST_ROOT:?}/meilisearch_labels.txt"
    source_provider_docker_labels typesense
    printf '%s\n' "${SOURCE_PROVIDER_DOCKER_LABEL_ARGS[@]}" \
      >"${PARITY_STARTUP_TOKEN_TEST_ROOT:?}/typesense_labels.txt"
  }
  start_server() { :; }
  probe_served_lifecycle() { :; }
  probe_served_discovery() { :; }
  probe_served_typesense_preview() { :; }
  probe_served_migrated_data() { :; }
  probe_route_tag_mutations() { :; }
  probe_live_router_binding_mutations() { :; }
  probe_neutral_shared_seam() { :; }
  main
}

assert_parity_startup_exports_owner_token_contract() {
  local root output status token
  root="$(mktemp -d "${TMPDIR:-/tmp}/fj_source_migration_provider_parity_startup_token_test.XXXXXX")"
  set +e
  output="$(PARITY_STARTUP_TOKEN_TEST_ROOT="$root" \
    SOURCE_MIGRATION_PROVIDER_PARITY_STARTUP_TOKEN_CHILD=1 \
    bash "$0" 2>&1)"
  status=$?
  set -e
  printf '%s\n' "$output" >"$root/startup_token.out"
  [ "$status" -eq 0 ] || {
    printf 'expected startup token contract to pass, got %s\noutput=%s\n' "$status" "$output" >&2
    exit 1
  }
  token="$(sed -n 's/^flapjack\.source_provider_fixture\.token=//p' "$root/typesense_labels.txt" | tail -1)"
  [ -n "$token" ] || {
    printf 'expected Typesense docker labels to include a source provider owner token, saw:\n' >&2
    cat "$root/typesense_labels.txt" >&2
    exit 1
  }
  grep -Fx "flapjack.source_provider_fixture.token=$token" "$root/meilisearch_labels.txt" >/dev/null || {
    printf 'expected Meilisearch docker labels to use the same owner token as Typesense\n' >&2
    cat "$root/meilisearch_labels.txt" "$root/typesense_labels.txt" >&2
    exit 1
  }
  rm -rf -- "$root"
}

assert_source_provider_owner_token_required_contract() {
  local status output
  set +e
  output="$(
    env -u SOURCE_PROVIDER_OWNER_TOKEN \
      bash -c '
        set -euo pipefail
        script_dir="$1"
        # shellcheck source=lib/source_provider_fixtures.sh
        source "$script_dir/lib/source_provider_fixtures.sh"
        set +e
        source_provider_docker_labels typesense 2>&1
        rc=$?
        set -e
        printf "\nstatus=%s\n" "$rc"
      ' bash "$SCRIPT_DIR"
  )"
  status=$?
  set -e
  [ "$status" -eq 0 ] || {
    printf 'expected owner-token requirement probe wrapper to exit 0, got %s\noutput=%s\n' "$status" "$output" >&2
    exit 1
  }
  printf '%s\n' "$output" | grep -F 'ERROR: SOURCE_PROVIDER_OWNER_TOKEN must be set before starting typesense fixtures' >/dev/null || {
    printf 'expected missing owner token error, saw:\n%s\n' "$output" >&2
    exit 1
  }
  printf '%s\n' "$output" | grep -F 'status=1' >/dev/null || {
    printf 'expected source_provider_docker_labels to fail without SOURCE_PROVIDER_OWNER_TOKEN, saw:\n%s\n' "$output" >&2
    exit 1
  }
}

run_parity_startup_contract_tests() {
  assert_source_provider_owner_token_required_contract
  assert_parity_startup_exports_owner_token_contract
  printf 'SOURCE_MIGRATION_PROVIDER_PARITY_STARTUP_TEST=PASS cases=owner_token_required,owner_token_exported_before_upstreams\n'
}

main() {
  require_tools
  TMP="$(mktemp -d "${TMPDIR:-/tmp}/fj_source_migration_provider_parity.XXXXXX")"
  configure_source_provider_owner_token
  build_or_resolve_binary
  start_discovery_upstreams
  start_server
  probe_served_lifecycle
  probe_served_discovery
  probe_served_typesense_preview
  probe_served_migrated_data
  probe_route_tag_mutations
  probe_live_router_binding_mutations
  probe_neutral_shared_seam
  printf 'SOURCE_MIGRATION_HTTP_PROBE=PASS providers=3 lifecycle=submit,status,cancel,ack,preview discovery=list-indexes landed_data=meilisearch,typesense\n'
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  if [ -n "${SOURCE_MIGRATION_PROVIDER_PARITY_CLEANUP_CHILD:-}" ]; then
    run_parity_cleanup_contract_child
  elif [ -n "${SOURCE_MIGRATION_PROVIDER_PARITY_STARTUP_TOKEN_CHILD:-}" ]; then
    run_parity_startup_token_contract_child
  elif [ "${SOURCE_MIGRATION_PROVIDER_PARITY_CLEANUP_TEST:-}" = "1" ]; then
    run_parity_cleanup_contract_tests
  elif [ "${SOURCE_MIGRATION_PROVIDER_PARITY_STARTUP_TEST:-}" = "1" ]; then
    run_parity_startup_contract_tests
  else
    trap cleanup EXIT
    main "$@"
  fi
fi
