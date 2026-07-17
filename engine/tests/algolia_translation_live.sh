#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SECRET_HELPER="$SCRIPT_DIR/common/load_named_secrets.sh"

SECRET_FILE=""
WORK_DIR=""
LOG_DIR=""
FIXTURE_DIR=""
RAW_DIR=""
RECEIPT=""
KEY_LEDGER=""
RUN_PREFIX=""
PASS_COMPLETE=0
INTERRUPTED_EXIT_CODE=0
CLEANUP_FAILED=0
EVIDENCE_DIR=""
PRESERVE_FAILED=0

usage() {
  cat <<'EOF'
Usage:
  algolia_translation_live.sh --secret-file <path>
EOF
}

die() {
  printf 'ERROR: %s\n' "$1" >&2
  exit "${2:-1}"
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"
}

parse_args() {
  if [ "$#" -ne 2 ] || [ "${1:-}" != "--secret-file" ] || [ -z "${2:-}" ]; then
    usage >&2
    exit 2
  fi
  SECRET_FILE="$2"
}

load_credentials() {
  # shellcheck source=engine/tests/common/load_named_secrets.sh
  source "$SECRET_HELPER"
  local loader_output
  loader_output="$(mktemp)"
  if ! load_named_secrets "$SECRET_FILE" ALGOLIA_APP_ID ALGOLIA_ADMIN_KEY >"$loader_output" 2>&1; then
    rm -f "$loader_output"
    die "required Algolia credentials could not be loaded"
  fi
  rm -f "$loader_output"
}

fingerprint() {
  printf '%s' "$1" | python3 -c 'import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())'
}

url_encode() {
  python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$1"
}

init_run() {
  require_tool curl
  require_tool jq
  require_tool python3
  require_tool od
  require_tool tr

  WORK_DIR="$(mktemp -d)"
  LOG_DIR="$WORK_DIR/logs"
  FIXTURE_DIR="$WORK_DIR/fixtures"
  RAW_DIR="$WORK_DIR/raw"
  RECEIPT="$WORK_DIR/receipt.json"
  KEY_LEDGER="$WORK_DIR/cleanup_keys.private"
  mkdir -p "$LOG_DIR" "$FIXTURE_DIR" "$RAW_DIR"
  : >"$KEY_LEDGER"
  chmod 600 "$KEY_LEDGER"

  local random_hex
  random_hex="$(od -An -N8 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  [ -n "$random_hex" ] || die "failed to generate run entropy"
  RUN_PREFIX="fj_stage4_translation_${random_hex}"
  jq -n --arg prefix "$RUN_PREFIX" --arg head "$(git -C "$ENGINE_DIR/.." rev-parse HEAD 2>/dev/null || true)" \
    '{prefix:$prefix, head:$head, created_indexes:[], created_key_fingerprints:[], checks:[]}' >"$RECEIPT"
}

record_check() {
  local name="$1" status="$2" detail="${3:-}" next
  next="$(mktemp)"
  jq --arg name "$name" --arg status "$status" --arg detail "$detail" \
    '.checks += [{name:$name,status:$status,detail:$detail}]' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

record_index() {
  local name="$1" next
  printf '%s\n' "$name" >>"$WORK_DIR/created_indexes.txt"
  next="$(mktemp)"
  jq --arg name "$name" '.created_indexes += [$name]' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

record_key() {
  local key="$1" fp next
  printf '%s\n' "$key" >>"$KEY_LEDGER"
  fp="$(fingerprint "$key")"
  next="$(mktemp)"
  jq --arg fp "$fp" '.created_key_fingerprints += [$fp]' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

algolia_base() {
  case "$1" in
    read) printf 'https://%s-dsn.algolia.net' "$ALGOLIA_APP_ID" ;;
    write) printf 'https://%s.algolia.net' "$ALGOLIA_APP_ID" ;;
  esac
}

http_body() { sed '$d'; }
http_code() { tail -1; }

algolia_request() {
  local mode="$1" method="$2" path="$3" key="$4" body="${5:-}" base
  base="$(algolia_base "$mode")"
  if [ -n "$body" ]; then
    curl -sS -w '\n%{http_code}' -X "$method" "${base}${path}" \
      -H "x-algolia-application-id: ${ALGOLIA_APP_ID}" \
      -H "x-algolia-api-key: ${key}" \
      -H 'content-type: application/json' \
      --data "$body"
  else
    curl -sS -w '\n%{http_code}' -X "$method" "${base}${path}" \
      -H "x-algolia-application-id: ${ALGOLIA_APP_ID}" \
      -H "x-algolia-api-key: ${key}" \
      -H 'content-type: application/json'
  fi
}

expect_algolia_json() {
  local mode="$1" method="$2" path="$3" key="$4" body="${5:-}" out="$6"
  local response code payload
  response="$(algolia_request "$mode" "$method" "$path" "$key" "$body")"
  code="$(printf '%s\n' "$response" | http_code)"
  payload="$(printf '%s\n' "$response" | http_body)"
  if [ "$code" -lt 200 ] || [ "$code" -gt 299 ]; then
    printf '%s\n' "$payload" >"$LOG_DIR/last_algolia_error.json"
    die "Algolia request failed with HTTP ${code}"
  fi
  printf '%s\n' "$payload" >"$out"
}

wait_task() {
  local index="$1"
  local task_id="$2"
  local out="$LOG_DIR/task-${index}-${task_id}.json"
  for _ in $(seq 1 120); do
    expect_algolia_json read GET "/1/indexes/$(url_encode "$index")/task/${task_id}" "$ALGOLIA_ADMIN_KEY" "" "$out"
    if [ "$(jq -r '.status // empty' "$out")" = "published" ]; then
      return 0
    fi
    sleep 0.5
  done
  die "Algolia task did not publish"
}

wait_recorded_task() {
  local index="$1" response_file="$2" task
  task="$(jq -r '.taskID // empty' "$response_file")"
  [ -z "$task" ] || wait_task "$index" "$task"
}

http_success_code() {
  case "$1" in
    2*) return 0 ;;
    *) return 1 ;;
  esac
}

cleanup_wait_task() {
  local index="$1" task_id="$2" encoded response code payload
  encoded="$(url_encode "$index")"
  for _ in $(seq 1 60); do
    response="$(algolia_request read GET "/1/indexes/${encoded}/task/${task_id}" "$ALGOLIA_ADMIN_KEY" "" 2>/dev/null || true)"
    code="$(printf '%s\n' "$response" | http_code)"
    payload="$(printf '%s\n' "$response" | http_body)"
    if http_success_code "$code" && [ "$(printf '%s\n' "$payload" | jq -r '.status // empty' 2>/dev/null)" = "published" ]; then
      return 0
    fi
    sleep 0.5
  done
  return 1
}

create_key() {
  local body="$1" out="$2" response code payload key
  response="$(algolia_request write POST "/1/keys" "$ALGOLIA_ADMIN_KEY" "$body")"
  code="$(printf '%s\n' "$response" | http_code)"
  payload="$(printf '%s\n' "$response" | http_body)"
  if [ "$code" -lt 200 ] || [ "$code" -gt 299 ]; then
    printf '%s\n' "$payload" >"$LOG_DIR/last_algolia_error.json"
    die "Algolia request failed with HTTP ${code}"
  fi
  key="$(printf '%s\n' "$payload" | jq -r '.key // empty')"
  [ -n "$key" ] || die "Algolia key creation response was missing key"
  record_key "$key"
  printf '%s\n' "$payload" | jq '.key = "<redacted>"' >"$out"
  printf '%s\n' "$key"
}

seed_fixture_files() {
  python3 - "$WORK_DIR" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
settings = {
    "searchableAttributes": ["title", "brand"],
    "attributesForFaceting": ["brand"],
    "unretrievableAttributes": ["secret_note"],
    "numericAttributesToIndex": ["price"],
    "distinct": True,
    "allowCompressionOfIntegerArray": False,
}
documents = [
    {"objectID": "live-doc-1", "title": "Live Trail Shoe", "brand": "North", "price": 129, "secret_note": "redacted"},
    {"objectID": "live-doc-2", "title": "Live City Shoe", "brand": "South", "price": 89},
]
rules = [
    {"objectID": "live-rule-1", "conditions": [{"pattern": "{facet:brand}", "anchoring": "is"}], "consequence": {"promote": [{"objectID": "live-doc-1", "position": 1}], "params": {"automaticFacetFilters": [{"facet": "brand", "score": 4}]}}, "enabled": True}
]
synonyms = [
    {"objectID": "live-syn-1", "type": "synonym", "synonyms": ["sneaker", "trainer"]},
    {"objectID": "live-syn-2", "type": "onewaysynonym", "input": "tee", "synonyms": ["t-shirt"]},
]
for name, value in {"settings": settings, "documents": documents, "rules": rules, "synonyms": synonyms}.items():
    (root / f"{name}.json").write_text(json.dumps(value, separators=(",", ":"), sort_keys=True), encoding="utf-8")
PY
}

seed_index() {
  local index="$1" encoded out
  record_index "$index"
  encoded="$(url_encode "$index")"
  out="$LOG_DIR/${index}-settings.json"
  expect_algolia_json write PUT "/1/indexes/${encoded}/settings" "$ALGOLIA_ADMIN_KEY" "$(cat "$WORK_DIR/settings.json")" "$out"
  wait_recorded_task "$index" "$out"
  out="$LOG_DIR/${index}-batch.json"
  expect_algolia_json write POST "/1/indexes/${encoded}/batch" "$ALGOLIA_ADMIN_KEY" "$(jq -c '{requests: [.[] | {action:"addObject", body:.}]}' "$WORK_DIR/documents.json")" "$out"
  wait_recorded_task "$index" "$out"
}

seed_empty_index() {
  local index="$1" encoded out
  record_index "$index"
  encoded="$(url_encode "$index")"
  out="$LOG_DIR/${index}-empty-settings.json"
  expect_algolia_json write PUT "/1/indexes/${encoded}/settings" "$ALGOLIA_ADMIN_KEY" "$(cat "$WORK_DIR/settings.json")" "$out"
  wait_recorded_task "$index" "$out"
}

seed_rules_synonyms() {
  local index="$1" encoded out
  encoded="$(url_encode "$index")"
  out="$LOG_DIR/${index}-rules.json"
  expect_algolia_json write POST "/1/indexes/${encoded}/rules/batch" "$ALGOLIA_ADMIN_KEY" "$(cat "$WORK_DIR/rules.json")" "$out"
  wait_recorded_task "$index" "$out"
  out="$LOG_DIR/${index}-synonyms.json"
  expect_algolia_json write POST "/1/indexes/${encoded}/synonyms/batch" "$ALGOLIA_ADMIN_KEY" "$(cat "$WORK_DIR/synonyms.json")" "$out"
  wait_recorded_task "$index" "$out"
}

seed_vendor() {
  seed_fixture_files
  local source_index="${RUN_PREFIX}_source"
  local empty_index="${RUN_PREFIX}_empty"
  local topology_index="${RUN_PREFIX}_topology"
  local replica_index="${RUN_PREFIX}_replica"
  seed_index "$source_index"
  seed_rules_synonyms "$source_index"
  seed_empty_index "$empty_index"
  record_index "$topology_index"
  record_index "$replica_index"
  expect_algolia_json write PUT "/1/indexes/$(url_encode "$topology_index")/settings" "$ALGOLIA_ADMIN_KEY" '{}' "$LOG_DIR/topology-initial-settings.json"
  wait_recorded_task "$topology_index" "$LOG_DIR/topology-initial-settings.json"
  expect_algolia_json write POST "/1/indexes/$(url_encode "$topology_index")/operation" "$ALGOLIA_ADMIN_KEY" "$(jq -n --arg destination "$replica_index" '{operation:"copy", destination:$destination}')" "$LOG_DIR/topology-copy.json"
  wait_recorded_task "$topology_index" "$LOG_DIR/topology-copy.json"
  expect_algolia_json write PUT "/1/indexes/$(url_encode "$topology_index")/settings" "$ALGOLIA_ADMIN_KEY" "$(jq -n --arg replica "$replica_index" '{replicas:[$replica]}')" "$LOG_DIR/topology-settings.json"
  wait_recorded_task "$topology_index" "$LOG_DIR/topology-settings.json"

  local restricted_body restricted_key response code payload
  restricted_body="$(jq -n --arg idx "${RUN_PREFIX}_*" --arg desc "${RUN_PREFIX} restricted translation fixture" '{acl:["search","browse"], indexes:[$idx], description:$desc, validity:3600}')"
  restricted_key="$(create_key "$restricted_body" "$LOG_DIR/restricted-key.json")"
  response="$(algolia_request read GET "/1/indexes/$(url_encode "$source_index")/settings" "$restricted_key" "" || true)"
  code="$(printf '%s\n' "$response" | http_code)"
  payload="$(printf '%s\n' "$response" | http_body)"
  printf '%s\n' "$payload" | jq '{status:"restricted-settings-denied", body:.}' >"$LOG_DIR/restricted-settings-denied.json"
  [ "$code" = "403" ] || die "restricted-key settings request returned HTTP ${code}, expected 403"

  response="$(algolia_request read GET "/1/indexes/$(url_encode "$source_index")/task/invalid-live-task" "$ALGOLIA_ADMIN_KEY" "" || true)"
  code="$(printf '%s\n' "$response" | http_code)"
  payload="$(printf '%s\n' "$response" | http_body)"
  printf '%s\n' "$payload" | jq '{status:"invalid-task-denied", body:.}' >"$LOG_DIR/invalid-task.json"
  [ "$code" = "400" ] || die "invalid task request returned HTTP ${code}, expected 400"
  record_check "vendor_setup" "pass" "created disposable source, empty, topology, and restricted-key fixtures"
}

fetch_translation_fixtures() {
  local source_index="${RUN_PREFIX}_source" encoded
  encoded="$(url_encode "$source_index")"
  expect_algolia_json read GET "/1/indexes/${encoded}/settings" "$ALGOLIA_ADMIN_KEY" "" "$RAW_DIR/settings.json"
  expect_algolia_json read POST "/1/indexes/${encoded}/browse" "$ALGOLIA_ADMIN_KEY" '{}' "$RAW_DIR/documents.json"
  expect_algolia_json read POST "/1/indexes/${encoded}/rules/search" "$ALGOLIA_ADMIN_KEY" '{"query":"","hitsPerPage":1000}' "$RAW_DIR/rules.json"
  expect_algolia_json read POST "/1/indexes/${encoded}/synonyms/search" "$ALGOLIA_ADMIN_KEY" '{"query":"","hitsPerPage":1000}' "$RAW_DIR/synonyms.json"
  cp "$RAW_DIR/settings.json" "$FIXTURE_DIR/settings.json"
  jq '[.hits]' "$RAW_DIR/documents.json" >"$FIXTURE_DIR/document_pages.json"
  jq '[.hits | map(del(._highlightResult, ._metadata))]' "$RAW_DIR/rules.json" >"$FIXTURE_DIR/rule_pages.json"
  jq '[.hits | map(del(._highlightResult, ._metadata))]' "$RAW_DIR/synonyms.json" >"$FIXTURE_DIR/synonym_pages.json"
  record_check "fixtures_fetched" "pass" "wrote SpoolTranslationInput fixture files"
}

run_rust_live_suite() {
  local cargo_out="$LOG_DIR/cargo-live-translation.out" status sentinel_count sentinel_value
  set +e
  (
    cd "$ENGINE_DIR"
    FLAPJACK_TRANSLATION_LIVE_FIXTURES="$FIXTURE_DIR" cargo test -p flapjack-http -- handlers::migration::translation::tests::live_algolia_translation_fixtures --nocapture
  ) >"$cargo_out" 2>&1
  status=$?
  set -e
  cat "$cargo_out"
  [ "$status" = "0" ] || die "Rust live fixture suite failed"
  if grep -Fq 'SKIPPED:' "$cargo_out"; then
    die "Rust live fixture suite skipped during credentialed run"
  fi
  sentinel_count="$(grep -Ec '^LIVE_TRANSLATION_PASS=[1-9][0-9]*$' "$cargo_out" || true)"
  sentinel_value="$(sed -n 's/^LIVE_TRANSLATION_PASS=//p' "$cargo_out" | tail -1)"
  [ "$sentinel_count" = "1" ] || die "Rust live fixture suite did not emit exactly one nonzero pass sentinel"
  [ "${sentinel_value:-0}" -gt 0 ] || die "Rust live fixture pass sentinel was zero"
  record_check "rust_live_translation" "pass" "LIVE_TRANSLATION_PASS=${sentinel_value}"
}

cleanup_vendor() {
  [ -n "$WORK_DIR" ] || return 0
  local key index response code payload remaining task
  if [ -f "$KEY_LEDGER" ]; then
    while IFS= read -r key || [ -n "$key" ]; do
      [ -n "$key" ] || continue
      algolia_request write DELETE "/1/keys/$(url_encode "$key")" "$ALGOLIA_ADMIN_KEY" "" >/dev/null 2>&1 || true
    done <"$KEY_LEDGER"
  fi
  if [ -f "$WORK_DIR/created_indexes.txt" ]; then
    while IFS= read -r index || [ -n "$index" ]; do
      [ -n "$index" ] || continue
      response="$(algolia_request write DELETE "/1/indexes/$(url_encode "$index")" "$ALGOLIA_ADMIN_KEY" "" 2>/dev/null || true)"
      code="$(printf '%s\n' "$response" | http_code)"
      payload="$(printf '%s\n' "$response" | http_body)"
      if http_success_code "$code"; then
        task="$(printf '%s\n' "$payload" | jq -r '.taskID // empty' 2>/dev/null || true)"
        [ -z "$task" ] || cleanup_wait_task "$index" "$task" || true
      fi
    done <"$WORK_DIR/created_indexes.txt"
  fi
  if [ -n "${ALGOLIA_APP_ID:-}" ] && [ -n "${ALGOLIA_ADMIN_KEY:-}" ] && [ -n "$RUN_PREFIX" ]; then
    if remaining="$(remaining_recorded_indexes)"; then
      if [ -n "$remaining" ]; then
        CLEANUP_FAILED=1
        printf 'ERROR: remaining Algolia indexes for prefix %s:\n%s\n' "$RUN_PREFIX" "$remaining" >&2
      fi
    else
      CLEANUP_FAILED=1
      printf 'ERROR: unable to verify Algolia index cleanup for prefix %s\n' "$RUN_PREFIX" >&2
    fi
    if [ -f "$KEY_LEDGER" ]; then
      while IFS= read -r key || [ -n "$key" ]; do
        [ -n "$key" ] || continue
        response="$(algolia_request read GET "/1/keys/$(url_encode "$key")" "$ALGOLIA_ADMIN_KEY" "" 2>/dev/null || true)"
        code="$(printf '%s\n' "$response" | http_code)"
        if [ "$code" != "404" ]; then
          CLEANUP_FAILED=1
          printf 'ERROR: remaining Algolia restricted key fingerprint %s\n' "$(fingerprint "$key")" >&2
        fi
      done <"$KEY_LEDGER"
    fi
  fi
}

remaining_recorded_indexes() {
  [ -f "$WORK_DIR/created_indexes.txt" ] || return 0
  local page=0 nb_pages=1 response code payload recorded
  recorded="$(jq -R -s 'split("\n") | map(select(length > 0))' "$WORK_DIR/created_indexes.txt")"
  while [ "$page" -lt "$nb_pages" ]; do
    response="$(algolia_request read GET "/1/indexes?page=${page}" "$ALGOLIA_ADMIN_KEY" "" 2>/dev/null || true)"
    code="$(printf '%s\n' "$response" | http_code)"
    payload="$(printf '%s\n' "$response" | http_body)"
    http_success_code "$code" || return 1
    nb_pages="$(printf '%s\n' "$payload" | jq -r '.nbPages // 1' 2>/dev/null)" || return 1
    printf '%s\n' "$payload" | jq -r --argjson recorded "$recorded" \
      '(.items // .indexes // [])[]?.name | select(. as $name | $recorded | index($name))' || return 1
    page=$((page + 1))
  done | LC_ALL=C sort -u
}

preserve_run_evidence() {
  [ -n "$WORK_DIR" ] || return 0
  [ -z "$EVIDENCE_DIR" ] || return 0
  local failed=0
  EVIDENCE_DIR="/tmp/flapjack_algolia_translation_live_evidence_${$}_$(date +%s)"
  mkdir -p "$EVIDENCE_DIR" || failed=1
  chmod 700 "$EVIDENCE_DIR" || failed=1
  cp "$RECEIPT" "$EVIDENCE_DIR/receipt.json" 2>/dev/null || failed=1
  if [ -d "$LOG_DIR" ]; then
    cp -R "$LOG_DIR" "$EVIDENCE_DIR/logs" || failed=1
  fi
  if [ -d "$FIXTURE_DIR" ]; then
    cp -R "$FIXTURE_DIR" "$EVIDENCE_DIR/fixtures" || failed=1
  fi
  if [ -d "$RAW_DIR" ]; then
    cp -R "$RAW_DIR" "$EVIDENCE_DIR/raw" || failed=1
  fi
  printf 'INFO: preserved sanitized live translation evidence at %s\n' "$EVIDENCE_DIR" >&2
  if [ "$failed" -ne 0 ]; then
    PRESERVE_FAILED=1
    printf 'ERROR: failed to preserve complete sanitized live translation evidence\n' >&2
    return 1
  fi
}

cleanup() {
  local exit_code=$?
  set +e
  preserve_run_evidence
  cleanup_vendor
  if [ "$PRESERVE_FAILED" -ne 0 ] || [ "$CLEANUP_FAILED" -ne 0 ]; then
    exit_code=1
  fi
  rm -rf "$WORK_DIR"
  if [ "$INTERRUPTED_EXIT_CODE" -ne 0 ]; then
    exit "$INTERRUPTED_EXIT_CODE"
  fi
  exit "$exit_code"
}

main() {
  parse_args "$@"
  trap cleanup EXIT
  trap 'INTERRUPTED_EXIT_CODE=130; exit 130' INT
  trap 'INTERRUPTED_EXIT_CODE=143; exit 143' TERM
  load_credentials
  init_run
  seed_vendor
  fetch_translation_fixtures
  run_rust_live_suite
  PASS_COMPLETE=1
  record_check "driver" "pass" "translation fixture driver completed"
  jq '{expected_observed:.checks, receipt:.}' "$RECEIPT"
  printf 'Retry with: bash engine/tests/algolia_translation_live.sh --secret-file <secret-file-with-ALGOLIA_APP_ID-and-ALGOLIA_ADMIN_KEY>\n' >&2
}

main "$@"
