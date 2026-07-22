#!/usr/bin/env bash
#
# Prove persisted usage gauges survive a real flapjack engine restart. The
# /1/usage responses below come from the restarted local engine binary; no
# third-party service participates in fixture setup or assertions.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_FOR_FLAPJACK="$SCRIPT_DIR/common/wait_for_flapjack.sh"

readonly APP_ID="flapjack"
readonly INDEX_NAME="gauge_probe"

ADMIN_KEY=""
BASE=""
BIN=""
DATA_DIR=""
PORT=""
SERVER_PID=""
TMP_DATA=""
TODAY=""
YESTERDAY=""
YESTERDAY_EPOCH_MS=""
CHECKS_RUN=0
CHECKS_FAILED=0

log() {
  printf '%s\n' "$*"
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

stop_server() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
}

cleanup() {
  local script_exit_code=$?
  stop_server
  if [ -n "$TMP_DATA" ] && [ -d "$TMP_DATA" ]; then
    if [ "$CHECKS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      local failure_epoch failure_dir
      failure_epoch="$(jq -nr 'now | floor')"
      failure_dir="/tmp/flapjack_usage_gauge_persistence_failure_${$}_${failure_epoch}"
      mv "$TMP_DATA" "$failure_dir"
      printf 'INFO: preserved usage gauge persistence evidence at %s\n' "$failure_dir"
    else
      local pass_epoch pass_dir
      pass_epoch="$(jq -nr 'now | floor')"
      pass_dir="/tmp/flapjack_usage_gauge_persistence_pass_${$}_${pass_epoch}"
      mkdir -p "$pass_dir"
      cp "$TMP_DATA/http"/*.json "$pass_dir"/
      printf 'INFO: preserved passing usage gauge response evidence at %s\n' "$pass_dir"
      rm -rf "$TMP_DATA"
    fi
  fi
}
trap cleanup EXIT

require_tools() {
  local missing=0 tool
  for tool in cargo curl jq od sed tr; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  [ "$missing" -eq 0 ] || exit 1
  [ -x "$WAIT_FOR_FLAPJACK" ] || die "missing readiness helper: $WAIT_FOR_FLAPJACK"
}

generate_admin_key() {
  local random_hex
  random_hex="$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  [ -n "$random_hex" ] || die 'failed to generate disposable admin key'
  printf 'fj_usage_gauge_persistence_%s\n' "$random_hex"
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

build_server() {
  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$TMP_DATA/build.log" 2>&1); then
    tail -30 "$TMP_DATA/build.log" >&2 || true
    die 'cargo build -p flapjack-server failed'
  fi
  BIN="$(target_dir)/debug/flapjack"
  [ -x "$BIN" ] || die "expected current-checkout binary at $BIN"
}

start_server() {
  local server_log="$1"
  FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$DATA_DIR" \
    "$BIN" --auto-port >"$server_log" 2>&1 &
  SERVER_PID=$!

  "$WAIT_FOR_FLAPJACK" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$server_log" \
    --retries 60 \
    --interval-seconds 0.5

  PORT="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$server_log" | head -1)"
  [ -n "$PORT" ] || die "server became ready but no auto-port was found in $server_log"
  BASE="http://127.0.0.1:${PORT}"
}

http_json() {
  local out_file="$1" method="$2" path="$3" body="${4:-}"
  local curl_args=(
    --fail-with-body -sS -X "$method" "${BASE}${path}"
    -H 'content-type: application/json'
    -H "x-algolia-api-key: ${ADMIN_KEY}"
    -H "x-algolia-application-id: ${APP_ID}"
    -o "$out_file"
  )
  if [ -n "$body" ]; then
    curl_args+=(--data "$body")
  fi
  if ! curl "${curl_args[@]}"; then
    log "ERROR: ${method} ${path} failed: $(cat "$out_file" 2>/dev/null)"
    return 1
  fi
}

assert_jq() {
  local label="$1" response_file="$2" filter="$3"
  shift 3
  if jq -e "$@" "$filter" "$response_file" >/dev/null 2>&1; then
    pass "$label"
  else
    fail "$label" "filter <$filter> failed against $(jq -c . "$response_file" 2>/dev/null || cat "$response_file")"
  fi
}

seed_known_documents() {
  local batch_file="$TMP_DATA/http/batch.json"
  http_json "$batch_file" POST "/1/indexes/${INDEX_NAME}/batch" \
    '{"requests":[{"action":"addObject","body":{"objectID":"gauge-a","title":"Gauge A"}},{"action":"addObject","body":{"objectID":"gauge-b","title":"Gauge B"}}]}'
  assert_jq 'batch returns a numeric task ID' "$batch_file" '.taskID | type == "number"'
  [ "$CHECKS_FAILED" -eq 0 ] || return 1
}

wait_for_exact_documents() {
  local label="$1" response_file="$2" attempt
  for attempt in $(seq 1 80); do
    http_json "$response_file" POST "/1/indexes/${INDEX_NAME}/query" \
      '{"query":"","hitsPerPage":10}'
    if jq -e '.nbHits == 2 and ([.hits[].objectID] | sort) == ["gauge-a", "gauge-b"]' "$response_file" >/dev/null 2>&1; then
      pass "$label"
      return 0
    fi
    sleep 0.25
  done
  fail "$label" "$(jq -c '{nbHits,hits:(.hits | map(.objectID))}' "$response_file" 2>/dev/null || cat "$response_file")"
  return 1
}

write_usage_fixture() {
  if ! (cd "$ENGINE_DIR" && cargo run -p flapjack-http --bin usage_gauge_fixture -- \
    "$DATA_DIR" "$YESTERDAY" >"$TMP_DATA/fixture.log" 2>&1); then
    tail -30 "$TMP_DATA/fixture.log" >&2 || true
    die 'usage_gauge_fixture failed'
  fi
}

assert_usage_responses() {
  local before_ms after_ms
  before_ms="$(jq -nr 'now * 1000 | floor')"
  http_json "$TMP_DATA/http/documents_count_gauge_probe.json" GET \
    "/1/usage/documents_count/gauge_probe?startDate=${YESTERDAY}&endDate=${TODAY}"
  after_ms="$(jq -nr 'now * 1000 | floor')"
  assert_jq 'historical and live document gauges survive restart' \
    "$TMP_DATA/http/documents_count_gauge_probe.json" \
    '.documents_count | length == 2 and .[0] == {t: $midnight, v: 17} and .[1].v == 2 and .[1].t >= $before and .[1].t <= $after' \
    --argjson midnight "$YESTERDAY_EPOCH_MS" --argjson before "$before_ms" --argjson after "$after_ms"

  http_json "$TMP_DATA/http/storage_bytes_gauge_probe.json" GET \
    "/1/usage/storage_bytes/gauge_probe?startDate=${YESTERDAY}&endDate=${YESTERDAY}"
  assert_jq 'historical storage gauge survives restart' \
    "$TMP_DATA/http/storage_bytes_gauge_probe.json" \
    '.storage_bytes == [{t: $midnight, v: 123456}]' \
    --argjson midnight "$YESTERDAY_EPOCH_MS"

  http_json "$TMP_DATA/http/documents_count_explicit_zero.json" GET \
    "/1/usage/documents_count/explicit_zero?startDate=${YESTERDAY}&endDate=${YESTERDAY}"
  assert_jq 'explicit persisted zero remains a data point' \
    "$TMP_DATA/http/documents_count_explicit_zero.json" \
    '.documents_count == [{t: $midnight, v: 0}]' \
    --argjson midnight "$YESTERDAY_EPOCH_MS"

  http_json "$TMP_DATA/http/documents_count_legacy_missing.json" GET \
    "/1/usage/documents_count/legacy_missing?startDate=${YESTERDAY}&endDate=${YESTERDAY}"
  assert_jq 'legacy missing gauge does not fabricate zero' \
    "$TMP_DATA/http/documents_count_legacy_missing.json" \
    '.documents_count == []'
}

main() {
  local now_seconds
  require_tools
  TMP_DATA="$(mktemp -d)"
  DATA_DIR="$TMP_DATA/data"
  mkdir -p "$DATA_DIR" "$TMP_DATA/http"
  ADMIN_KEY="$(generate_admin_key)"

  build_server
  start_server "$TMP_DATA/server_before_restart.log"
  seed_known_documents
  wait_for_exact_documents 'exact seeded documents are queryable before restart' \
    "$TMP_DATA/http/query_before_restart.json"
  stop_server

  now_seconds="$(jq -nr 'now')"
  TODAY="$(jq -nr --argjson now "$now_seconds" '$now | strftime("%Y-%m-%d")')"
  YESTERDAY="$(jq -nr --argjson now "$now_seconds" '$now - 86400 | strftime("%Y-%m-%d")')"
  YESTERDAY_EPOCH_MS="$(jq -nr --arg date "$YESTERDAY" '$date + "T00:00:00Z" | fromdateiso8601 * 1000')"
  write_usage_fixture

  start_server "$TMP_DATA/server_after_restart.log"
  wait_for_exact_documents 'exact seeded documents are queryable after restart' \
    "$TMP_DATA/http/query_after_restart.json"
  assert_usage_responses

  log "Checks: ${CHECKS_RUN}; failures: ${CHECKS_FAILED}"
  [ "$CHECKS_FAILED" -eq 0 ]
}

main "$@"
