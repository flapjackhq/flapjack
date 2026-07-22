#!/usr/bin/env bash
#
# search_pagination_live_http.sh — Prove the Stage 1/2 pagination known-answer
# contract at the served HTTP boundary, through the real flapjack-server binary.
#
# The in-process Rust KAT
# (flapjack-http/src/handlers/search/stage5_integration_tests/search_pagination_known_answer.rs)
# exercises the handler router directly. This driver replays the same five cases
# over real HTTP against a freshly started binary, seeded from the one shared
# fixture both consumers read:
#
#   engine/tests/fixtures/search_pagination_known_answer.json
#
# Expected response values below are hand-calculated constants mirroring that
# KAT. This script must not compute pagination itself.
#
# Usage:
#   bash engine/tests/search_pagination_live_http.sh
#
# Environment:
#   FLAPJACK_BIN  Optional path to a prebuilt flapjack binary (skips cargo build).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ENGINE_DIR="$REPO_DIR/engine"
FIXTURE="$SCRIPT_DIR/fixtures/search_pagination_known_answer.json"
WAIT_HELPER="$SCRIPT_DIR/common/wait_for_flapjack.sh"

# Hand-calculated from the 25-document fixture at hitsPerPage=10, mirroring the
# Rust KAT. Held here as named constants so no assertion repeats a bare literal.
readonly HITS_PER_PAGE=10
readonly FULL_NB_HITS=25
readonly FULL_NB_PAGES=3
readonly FULL_PAGE_0_LEN=10
readonly FULL_PAGE_2_LEN=5
readonly DISTINCT_NB_HITS=5
readonly DISTINCT_NB_PAGES=1
readonly DISTINCT_PAGE_0_LEN=5
readonly DISTINCT_PAGE_2_LEN=0
readonly SORTED_PAGE_0_FIRST_ID='id_zebra_900'
readonly SORTED_PAGE_0_LAST_ID='id_echo_050'
readonly SORTED_PAGE_2_FIRST_ID='id_romeo_130'
readonly SORTED_PAGE_2_LAST_ID='id_november_170'

SERVER_PID=""
TMP_DATA=""
BIN=""
BASE=""
PORT=""
TRANSCRIPT=""
CHECKS_RUN=0
CHECKS_FAILED=0

usage() {
  sed -n '3,21p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

log() {
  printf '%s\n' "$*" | tee -a "${TRANSCRIPT:-/dev/null}"
}

pass() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  log "  [PASS] $1"
}

fail() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  CHECKS_FAILED=$((CHECKS_FAILED + 1))
  log "  [FAIL] $1"
  if [ -n "${2:-}" ]; then
    log "         $2"
  fi
}

die() {
  log "ERROR: $1"
  exit 1
}

# Kill and wait only the exact server PID this script started. Preserve the temp
# data directory (server log, request/response JSON) whenever anything failed.
cleanup() {
  local script_exit_code=$?
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [ -n "$TMP_DATA" ] && [ -d "$TMP_DATA" ]; then
    if [ "$CHECKS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      printf 'INFO: preserved pagination verifier evidence at %s\n' "$TMP_DATA"
    else
      rm -rf "$TMP_DATA"
    fi
  fi
}
trap cleanup EXIT

require_tools() {
  local tool missing=0
  for tool in curl jq sed; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  [ "$missing" -eq 0 ] || exit 1
  [ -f "$FIXTURE" ] || die "missing shared fixture: $FIXTURE"
  [ -x "$WAIT_HELPER" ] || die "missing readiness helper: $WAIT_HELPER"
}

build_or_resolve_binary() {
  if [ -n "${FLAPJACK_BIN:-}" ]; then
    [ -x "$FLAPJACK_BIN" ] || die "FLAPJACK_BIN=$FLAPJACK_BIN is not executable"
    BIN="$FLAPJACK_BIN"
    log "Using pre-built binary: $BIN"
    return
  fi

  log 'Building flapjack-server release binary...'
  local build_log
  build_log="$(mktemp)"
  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server --release >"$build_log" 2>&1); then
    tail -20 "$build_log" >&2 || true
    rm -f "$build_log"
    die 'cargo build -p flapjack-server --release failed'
  fi
  rm -f "$build_log"

  BIN="$ENGINE_DIR/target/release/flapjack"
  [ -x "$BIN" ] || die "expected binary at $BIN"
}

start_server() {
  TMP_DATA="$(mktemp -d)"
  mkdir -p "$TMP_DATA/http"

  FLAPJACK_DATA_DIR="$TMP_DATA/data" \
    "$BIN" --auto-port --no-auth >"$TMP_DATA/server.log" 2>&1 &
  SERVER_PID=$!

  "$WAIT_HELPER" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$TMP_DATA/server.log"

  PORT="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$TMP_DATA/server.log" | head -1)"
  [ -n "$PORT" ] || die 'server became healthy but no auto-port was found in the startup log'
  BASE="http://127.0.0.1:${PORT}"
  log "Server ready at $BASE (pid $SERVER_PID)"
}

# POST a JSON body and save the response to $1. Any transport or HTTP error
# status exits the verifier nonzero via --fail-with-body.
http_json() {
  local out_file="$1" method="$2" path="$3" body="$4"
  if ! curl --fail-with-body -sS -X "$method" "${BASE}${path}" \
    -H 'content-type: application/json' \
    --data "$body" \
    -o "$out_file"; then
    log "ERROR: ${method} ${path} failed; response body:"
    cat "$out_file" >&2 || true
    return 1
  fi
}

# Block until an indexing/settings task reaches published status. A missing or
# non-numeric taskID is a hard failure, never a silent skip.
wait_for_task() {
  local task_file="$1" task_id
  task_id="$(jq -er 'if (.taskID | type == "number") then (.taskID | tostring) else error("no numeric taskID") end' "$task_file")" ||
    die "response is missing a numeric taskID: $(cat "$task_file")"

  local status_file="${TMP_DATA}/http/task_${task_id}.json"
  local attempt
  for attempt in $(seq 1 40); do
    if curl --fail-with-body -sS "${BASE}/1/task/${task_id}" -o "$status_file" &&
      jq -e '.status == "published"' "$status_file" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  die "task ${task_id} did not reach published status within 10s: $(cat "$status_file" 2>/dev/null)"
}

# Assert a jq expression holds against a saved response file.
assert_jq() {
  local label="$1" response_file="$2" filter="$3"
  if jq -e "$filter" "$response_file" >/dev/null 2>&1; then
    pass "$label"
  else
    fail "$label" "filter <$filter> failed against $(jq -c '{nbHits,nbPages,hits:(.hits|map(.objectID))}' "$response_file" 2>/dev/null || cat "$response_file")"
  fi
}

# Create a fresh index, seed it from the shared fixture, apply this case's
# complete settings, then query page 0 and page 2. Each case gets its own index
# so settings and indexed state never leak between cases.
#
# $1 case name, $2 settings JSON, $3 query parameters JSON (without "page").
run_case() {
  local case_name="$1" settings_json="$2" query_params="$3"
  local index="pagination_live_${case_name}"
  local http_dir="${TMP_DATA}/http/${case_name}"
  mkdir -p "$http_dir"

  log ""
  log "Case: ${case_name} (index ${index})"

  http_json "${http_dir}/create.json" POST '/1/indexes' "{\"uid\":\"${index}\"}"

  local seed_payload
  seed_payload="$(jq -c '{requests: [.[] | {action: "addObject", body: .}]}' "$FIXTURE")"
  printf '%s\n' "$seed_payload" >"${http_dir}/seed_request.json"
  http_json "${http_dir}/seed.json" POST "/1/indexes/${index}/batch" "$seed_payload"
  wait_for_task "${http_dir}/seed.json"

  printf '%s\n' "$settings_json" >"${http_dir}/settings_request.json"
  http_json "${http_dir}/settings.json" PUT "/1/indexes/${index}/settings" "$settings_json"
  wait_for_task "${http_dir}/settings.json"

  local page body
  for page in 0 2; do
    body="$(jq -cn --argjson params "$query_params" --argjson page "$page" \
      --argjson hits_per_page "$HITS_PER_PAGE" \
      '$params + {hitsPerPage: $hits_per_page, page: $page}')"
    printf '%s\n' "$body" >"${http_dir}/query_page_${page}_request.json"
    http_json "${http_dir}/page_${page}.json" POST "/1/indexes/${index}/query" "$body"
  done
}

# Shared envelope assertions for the four cases that see all 25 documents.
assert_full_result_envelope() {
  local case_name="$1"
  local http_dir="${TMP_DATA}/http/${case_name}"

  assert_jq "${case_name}: page 0 nbHits == ${FULL_NB_HITS}" \
    "${http_dir}/page_0.json" ".nbHits == ${FULL_NB_HITS}"
  assert_jq "${case_name}: page 0 nbPages == ${FULL_NB_PAGES}" \
    "${http_dir}/page_0.json" ".nbPages == ${FULL_NB_PAGES}"
  assert_jq "${case_name}: page 0 hit count == ${FULL_PAGE_0_LEN}" \
    "${http_dir}/page_0.json" "(.hits | length) == ${FULL_PAGE_0_LEN}"
  assert_jq "${case_name}: page 2 nbHits == ${FULL_NB_HITS}" \
    "${http_dir}/page_2.json" ".nbHits == ${FULL_NB_HITS}"
  assert_jq "${case_name}: page 2 nbPages == ${FULL_NB_PAGES}" \
    "${http_dir}/page_2.json" ".nbPages == ${FULL_NB_PAGES}"
  assert_jq "${case_name}: page 2 hit count == ${FULL_PAGE_2_LEN}" \
    "${http_dir}/page_2.json" "(.hits | length) == ${FULL_PAGE_2_LEN}"
}

assert_simple_case() {
  local http_dir="${TMP_DATA}/http/simple"
  assert_full_result_envelope simple

  # The Stage 2 regression: an undersized candidate window let page 0 and page 2
  # return overlapping documents while the counts still looked correct.
  if jq -e -n \
    --slurpfile page_0 "${http_dir}/page_0.json" \
    --slurpfile page_2 "${http_dir}/page_2.json" \
    '(($page_0[0].hits | map(.objectID)) - ($page_2[0].hits | map(.objectID)) | length)
       == ($page_0[0].hits | length)' >/dev/null 2>&1; then
    pass 'simple: page 0 and page 2 objectID sets are disjoint'
  else
    fail 'simple: page 0 and page 2 objectID sets are disjoint' \
      "page_0=$(jq -c '.hits|map(.objectID)' "${http_dir}/page_0.json") page_2=$(jq -c '.hits|map(.objectID)' "${http_dir}/page_2.json")"
  fi
}

assert_distinct_case() {
  local http_dir="${TMP_DATA}/http/distinct"
  assert_jq "distinct: page 0 nbHits == ${DISTINCT_NB_HITS}" \
    "${http_dir}/page_0.json" ".nbHits == ${DISTINCT_NB_HITS}"
  assert_jq "distinct: page 0 nbPages == ${DISTINCT_NB_PAGES}" \
    "${http_dir}/page_0.json" ".nbPages == ${DISTINCT_NB_PAGES}"
  assert_jq "distinct: page 0 hit count == ${DISTINCT_PAGE_0_LEN}" \
    "${http_dir}/page_0.json" "(.hits | length) == ${DISTINCT_PAGE_0_LEN}"
  assert_jq "distinct: page 2 nbHits == ${DISTINCT_NB_HITS}" \
    "${http_dir}/page_2.json" ".nbHits == ${DISTINCT_NB_HITS}"
  assert_jq "distinct: page 2 nbPages == ${DISTINCT_NB_PAGES}" \
    "${http_dir}/page_2.json" ".nbPages == ${DISTINCT_NB_PAGES}"
  assert_jq "distinct: page 2 hit count == ${DISTINCT_PAGE_2_LEN}" \
    "${http_dir}/page_2.json" "(.hits | length) == ${DISTINCT_PAGE_2_LEN}"
}

assert_sorted_case() {
  local http_dir="${TMP_DATA}/http/sorted"
  assert_full_result_envelope sorted

  assert_jq "sorted: page 0 first objectID == ${SORTED_PAGE_0_FIRST_ID}" \
    "${http_dir}/page_0.json" ".hits[0].objectID == \"${SORTED_PAGE_0_FIRST_ID}\""
  assert_jq "sorted: page 0 last objectID == ${SORTED_PAGE_0_LAST_ID}" \
    "${http_dir}/page_0.json" ".hits[-1].objectID == \"${SORTED_PAGE_0_LAST_ID}\""
  assert_jq "sorted: page 2 first objectID == ${SORTED_PAGE_2_FIRST_ID}" \
    "${http_dir}/page_2.json" ".hits[0].objectID == \"${SORTED_PAGE_2_FIRST_ID}\""
  assert_jq "sorted: page 2 last objectID == ${SORTED_PAGE_2_LAST_ID}" \
    "${http_dir}/page_2.json" ".hits[-1].objectID == \"${SORTED_PAGE_2_LAST_ID}\""
}

open_transcript() {
  local results_dir="$SCRIPT_DIR/results"
  mkdir -p "$results_dir"
  TRANSCRIPT="$results_dir/search_pagination_live_http.log"
  : >"$TRANSCRIPT"
  {
    printf 'command: bash engine/tests/search_pagination_live_http.sh\n'
    printf 'checkout_sha: %s\n' "$(cd "$REPO_DIR" && git rev-parse HEAD)"
    printf 'checkout_dirty: %s\n' "$(cd "$REPO_DIR" && { [ -n "$(git status --porcelain)" ] && echo yes || echo no; })"
    printf 'fixture: engine/tests/fixtures/search_pagination_known_answer.json\n'
    printf 'fixture_document_count: %s\n' "$(jq length "$FIXTURE")"
  } >>"$TRANSCRIPT"
}

# Append the request bodies and response JSON for every case to the transcript
# so Stage 4 can read the served evidence without re-running the binary.
append_http_evidence_to_transcript() {
  local case_dir file
  log ""
  log '--- HTTP evidence ---'
  for case_dir in "$TMP_DATA"/http/*/; do
    [ -d "$case_dir" ] || continue
    for file in "$case_dir"query_page_*_request.json "$case_dir"page_*.json; do
      [ -f "$file" ] || continue
      log "file: ${case_dir##*/http/}$(basename "$file")"
      jq -c '.' "$file" >>"$TRANSCRIPT"
    done
  done
}

main() {
  if [ "${1:-}" = '--help' ] || [ "${1:-}" = '-h' ]; then
    usage
    return 0
  fi

  open_transcript
  log 'Search Pagination Live HTTP Verifier'
  require_tools
  build_or_resolve_binary
  start_server
  log "server_address: $BASE"

  local searchable_title='"searchableAttributes":["title"]'

  run_case simple "{${searchable_title}}" '{"query":"pagination"}'
  run_case distinct "{${searchable_title},\"attributeForDistinct\":\"group\"}" '{"query":"pagination","distinct":true}'
  run_case faceted "{${searchable_title},\"attributesForFaceting\":[\"category\"]}" '{"query":"pagination","facets":["category"]}'
  run_case browse "{${searchable_title}}" '{"query":""}'
  run_case sorted "{${searchable_title}}" '{"query":"pagination","sort":["rank:asc"]}'

  log ""
  log '--- Assertions ---'
  assert_simple_case
  assert_distinct_case
  assert_full_result_envelope faceted
  assert_full_result_envelope browse
  assert_sorted_case

  append_http_evidence_to_transcript

  log ""
  log "Results: $((CHECKS_RUN - CHECKS_FAILED))/${CHECKS_RUN} checks passed"
  log "transcript: engine/tests/results/$(basename "$TRANSCRIPT")"
  if [ "$CHECKS_FAILED" -gt 0 ]; then
    log "${CHECKS_FAILED} check(s) failed"
    return 1
  fi
  log 'All pagination HTTP checks passed'
  return 0
}

main "$@"
