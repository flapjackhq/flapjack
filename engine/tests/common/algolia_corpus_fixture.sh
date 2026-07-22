#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SECRET_HELPER="$SCRIPT_DIR/load_named_secrets.sh"
GENERATOR="$SCRIPT_DIR/generate_algolia_corpus.sh"

ALGOLIA_APP_ID=""
ALGOLIA_ADMIN_KEY=""
CORPUS_SIZE=""
SECRET_FILE=""
WORK_DIR=""
LEDGER_PATH=""
SOURCE_INDEX=""
TARGET_INDEX=""
OWNED_SOURCE=0
PREPARE_COMPLETE=0
SELFTEST_LEDGER_PATH=""
SELFTEST_TEMP_DIR=""

usage() {
  cat <<'EOF'
Usage:
  algolia_corpus_fixture.sh prepare --corpus-size <N> --secret-file <absolute-path> --work-dir <absolute-path>
  algolia_corpus_fixture.sh cleanup --ledger <absolute-path> --secret-file <absolute-path>
  algolia_corpus_fixture.sh source-count --index <name> --secret-file <absolute-path> --work-dir <absolute-path>
  algolia_corpus_fixture.sh selftest --corpus-size <N> --secret-file <absolute-path>
EOF
}

die() {
  printf 'ERROR: %s\n' "$1" >&2
  exit "${2:-1}"
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"
}

require_absolute_path() {
  local label="$1" value="$2"
  [ -n "$value" ] || die "$label is required" 2
  case "$value" in
    /*) ;;
    *) die "$label must be an absolute path" 2 ;;
  esac
}

require_corpus_size() {
  [[ "$CORPUS_SIZE" =~ ^[1-9][0-9]*$ ]] || die "--corpus-size must be a positive integer" 2
}

load_algolia_credentials() {
  # shellcheck source=engine/tests/common/load_named_secrets.sh disable=SC1091
  source "$SECRET_HELPER"
  local loader_output
  loader_output="$(mktemp)"
  if ! load_named_secrets "$SECRET_FILE" ALGOLIA_APP_ID ALGOLIA_ADMIN_KEY >"$loader_output" 2>&1; then
    rm -f "$loader_output"
    die "required Algolia credentials could not be loaded"
  fi
  rm -f "$loader_output"
}

algolia_url() {
  local path="$1"
  printf 'https://%s.algolia.net%s' "$ALGOLIA_APP_ID" "$path"
}

algolia_request() {
  local method="$1" path="$2" body="$3" out="$4" body_file="" status
  if [[ "$body" == @* ]]; then
    body_file="${body#@}"
  elif [ -n "$body" ]; then
    body_file="$(mktemp)"
    chmod 600 "$body_file" 2>/dev/null || true
    printf '%s' "$body" >"$body_file"
  fi
  {
    printf 'silent\n'
    printf 'show-error\n'
    printf 'request = "%s"\n' "$method"
    printf 'url = "%s"\n' "$(algolia_url "$path")"
    printf 'header = "x-algolia-application-id: %s"\n' "$ALGOLIA_APP_ID"
    printf 'header = "x-algolia-api-key: %s"\n' "$ALGOLIA_ADMIN_KEY"
    printf 'header = "content-type: application/json"\n'
    [ -z "$body_file" ] || printf 'data-binary = "@%s"\n' "$body_file"
  } | curl -w '\n%{http_code}' --config - >"$out"
  status=$?
  [[ "$body" == @* ]] || [ -z "$body_file" ] || rm -f "$body_file"
  return "$status"
}

http_body() { sed '$d'; }
http_code() { tail -1; }

encoded_index_path() {
  local index_name="$1"
  printf '/1/indexes/%s' "$(jq -nr --arg value "$index_name" '$value | @uri')"
}

request_with_retry() {
  local method="$1" path="$2" body="$3" out="$4" attempt=1 code delay jitter
  while [ "$attempt" -le 5 ]; do
    algolia_request "$method" "$path" "$body" "$out" || return 1
    code="$(http_code <"$out")"
    [ "$code" != "429" ] && return 0
    delay=$((attempt * attempt))
    jitter=$((RANDOM % 3))
    sleep "$((delay + jitter))"
    attempt=$((attempt + 1))
  done
  return 0
}

require_success_response() {
  local out="$1" label="$2" code
  code="$(http_code <"$out")"
  [[ "$code" =~ ^2[0-9][0-9]$ ]] || die "$label returned HTTP ${code}"
}

wait_for_task() {
  local index_name="$1" task_id="$2" out="$3" path remaining=120 status
  [ -n "$task_id" ] || die "Algolia taskID was empty"
  path="$(encoded_index_path "$index_name")/task/$(jq -nr --arg value "$task_id" '$value | @uri')"
  while [ "$remaining" -gt 0 ]; do
    request_with_retry GET "$path" "" "$out"
    require_success_response "$out" "task ${task_id} status"
    status="$(http_body <"$out" | jq -er '.status // empty')" || die "task ${task_id} status response was malformed"
    [ "$status" = "published" ] && return 0
    [ "$status" = "notPublished" ] || die "task ${task_id} returned unexpected status: ${status}"
    sleep 1
    remaining=$((remaining - 1))
  done
  die "task ${task_id} did not publish before timeout"
}

wait_for_response_tasks() {
  local index_name="$1" out="$2" task_id task_ids
  task_ids="$(http_body <"$out" | jq -er '[.taskID? // empty, (.taskIDs? // [] | .[])] | map(select(. != null and . != "") | tostring) | .[]')" \
    || die "Algolia write response did not include taskID"
  while IFS= read -r task_id; do
    wait_for_task "$index_name" "$task_id" "$out"
  done < <(printf '%s\n' "$task_ids")
}

write_ledger() {
  local ledger="$1" source="$2" owned="$3" next
  next="$(mktemp)"
  jq -n --arg source "$source" --argjson owned "$owned" \
    '{algolia_sources:[{name:$source,owned:$owned}]}' >"$next"
  mv "$next" "$ledger"
}

cleanup_prepare_failure() {
  [ "$PREPARE_COMPLETE" -eq 0 ] || return 0
  [ "$OWNED_SOURCE" -eq 1 ] || return 0
  [ -n "$LEDGER_PATH" ] && [ -f "$LEDGER_PATH" ] || return 0
  cleanup_from_ledger "$LEDGER_PATH" || true
}

delete_index_and_prove_absent() {
  local index_name="$1" label="$2" out path code remaining=40
  out="$(mktemp)"
  path="$(encoded_index_path "$index_name")"
  request_with_retry DELETE "$path" "" "$out"
  require_success_response "$out" "${label} delete"
  while [ "$remaining" -gt 0 ]; do
    request_with_retry GET "$path" "" "$out"
    code="$(http_code <"$out")"
    [ "$code" = "404" ] && break
    [ "$code" = "200" ] || die "${label} absence proof returned HTTP ${code}"
    sleep 0.25
    remaining=$((remaining - 1))
  done
  [ "$remaining" -gt 0 ] || die "${label} left residue: $index_name"
  rm -f "$out"
}

prefix_preflight() {
  local out stale_index
  [ "$OWNED_SOURCE" -eq 1 ] || return 0
  case "$SOURCE_INDEX" in
    fj_scale_*) ;;
    *) die "scale fixture source index must use fj_scale_ prefix" ;;
  esac
  out="$WORK_DIR/algolia-prefix-preflight.raw"
  request_with_retry GET "/1/indexes" "" "$out"
  require_success_response "$out" "scale prefix preflight"
  while IFS= read -r stale_index; do
    [ -n "$stale_index" ] || continue
    delete_index_and_prove_absent "$stale_index" "scale prefix preflight"
  done < <(http_body <"$out" | jq -r --arg current "$SOURCE_INDEX" '
    .items[]? | .name? | strings | select(startswith("fj_scale_")) | select(. != $current)
  ')
}

seed_settings_synonyms_rules() {
  local out="$WORK_DIR/algolia-setup.raw" path manifest settings synonyms rules
  path="$(encoded_index_path "$SOURCE_INDEX")"
  manifest="$WORK_DIR/algolia-scale-manifest.json"
  "$GENERATOR" manifest --corpus-size "$CORPUS_SIZE" >"$manifest"

  settings="$(jq -ce '.source_configuration.settings' "$manifest")"
  request_with_retry PUT "${path}/settings" "$settings" "$out"
  require_success_response "$out" "settings setup"
  wait_for_response_tasks "$SOURCE_INDEX" "$out"

  synonyms="$(jq -ce '.source_configuration.synonyms' "$manifest")"
  request_with_retry POST "${path}/synonyms/batch" "$synonyms" "$out"
  require_success_response "$out" "synonym setup"
  wait_for_response_tasks "$SOURCE_INDEX" "$out"

  rules="$(jq -ce '.source_configuration.rules' "$manifest")"
  request_with_retry POST "${path}/rules/batch" "$rules" "$out"
  require_success_response "$out" "rule setup"
  wait_for_response_tasks "$SOURCE_INDEX" "$out"
}

seed_documents() {
  local docs_file batch_dir batch_file request_file out path
  docs_file="$WORK_DIR/algolia-scale-documents.ndjson"
  batch_dir="$WORK_DIR/algolia-scale-batches"
  mkdir -p "$batch_dir"
  "$GENERATOR" documents --corpus-size "$CORPUS_SIZE" >"$docs_file"
  split -l 1000 "$docs_file" "$batch_dir/batch_"
  path="$(encoded_index_path "$SOURCE_INDEX")"
  out="$WORK_DIR/algolia-seed.raw"
  for batch_file in "$batch_dir"/batch_*; do
    [ -f "$batch_file" ] || continue
    request_file="$(mktemp "$WORK_DIR/algolia-batch.XXXXXX")"
    jq -cs '{requests: map({action:"addObject", body:.})}' "$batch_file" >"$request_file"
    request_with_retry POST "${path}/batch" "@$request_file" "$out"
    require_success_response "$out" "document batch seed"
    wait_for_response_tasks "$SOURCE_INDEX" "$out"
    rm -f "$request_file"
  done
}

run_prepare() {
  require_absolute_path "--secret-file" "$SECRET_FILE"
  require_absolute_path "--work-dir" "$WORK_DIR"
  require_corpus_size
  mkdir -p "$WORK_DIR"
  load_algolia_credentials

  if [ -n "${FJ_SCALE_REUSE_FIXTURE:-}" ]; then
    SOURCE_INDEX="$FJ_SCALE_REUSE_FIXTURE"
    OWNED_SOURCE=0
  else
    SOURCE_INDEX="fj_scale_source_$(date +%Y%m%dT%H%M%S)_$$_$((RANDOM % 10000))"
    OWNED_SOURCE=1
  fi
  TARGET_INDEX="fj_scale_target_$(date +%Y%m%dT%H%M%S)_$$_$((RANDOM % 10000))"
  prefix_preflight
  LEDGER_PATH="$WORK_DIR/algolia-scale-ledger.json"
  write_ledger "$LEDGER_PATH" "$SOURCE_INDEX" "$([ "$OWNED_SOURCE" -eq 1 ] && printf true || printf false)"
  trap cleanup_prepare_failure EXIT

  if [ "$OWNED_SOURCE" -eq 1 ]; then
    seed_settings_synonyms_rules
    seed_documents
  fi

  PREPARE_COMPLETE=1
  trap - EXIT
  jq -n --arg source "$SOURCE_INDEX" --arg target "$TARGET_INDEX" --arg ledger "$LEDGER_PATH" \
    '{source_index:$source,target_index:$target,ledger_path:$ledger}'
}

cleanup_from_ledger() {
  local ledger="$1" source owned name
  jq -e '.algolia_sources | type == "array"' "$ledger" >/dev/null || die "ledger is malformed"
  while IFS=$'\t' read -r source owned; do
    [ "$owned" = "true" ] || continue
    case "$source" in
      fj_scale_*) ;;
      *) die "refusing to delete unprefixed Algolia source from ledger" ;;
    esac
    delete_index_and_prove_absent "$source" "source cleanup"
  done < <(jq -r '.algolia_sources[]? | [.name, .owned] | @tsv' "$ledger")

  name="$(jq -r '.algolia_sources[]? | select(.owned == true) | .name' "$ledger" | head -1)"
  [ -n "$name" ] || return 0
}

run_cleanup() {
  require_absolute_path "--secret-file" "$SECRET_FILE"
  require_absolute_path "--ledger" "$LEDGER_PATH"
  [ -f "$LEDGER_PATH" ] || die "ledger does not exist"
  load_algolia_credentials
  cleanup_from_ledger "$LEDGER_PATH"
}

run_source_count() {
  local out path count
  require_absolute_path "--secret-file" "$SECRET_FILE"
  require_absolute_path "--work-dir" "$WORK_DIR"
  [ -n "$SOURCE_INDEX" ] || die "--index is required" 2
  mkdir -p "$WORK_DIR"
  load_algolia_credentials
  out="$WORK_DIR/algolia-source-count.raw"
  path="$(encoded_index_path "$SOURCE_INDEX")/query"
  request_with_retry POST "$path" '{"query":"","hitsPerPage":0}' "$out"
  require_success_response "$out" "source count query"
  count="$(http_body <"$out" | jq -er '
    if (.nbHits | type) == "number" and .nbHits >= 0 and (.nbHits | floor) == .nbHits
    then .nbHits else empty end
  ')" || die "source count response was malformed"
  printf '%s\n' "$count"
}

verify_selftest_count() {
  local index="$1" out="$2" path count remaining=60
  path="$(encoded_index_path "$index")/query"
  while [ "$remaining" -gt 0 ]; do
    request_with_retry POST "$path" '{"query":"","hitsPerPage":0}' "$out"
    require_success_response "$out" "selftest count query"
    count="$(http_body <"$out" | jq -er '.nbHits')"
    [ "$count" = "$CORPUS_SIZE" ] && return 0
    sleep 1
    remaining=$((remaining - 1))
  done
  die "selftest expected nbHits == $CORPUS_SIZE, got $count"
}

cleanup_selftest_failure() {
  trap - EXIT
  [ -n "$SELFTEST_LEDGER_PATH" ] && [ -f "$SELFTEST_LEDGER_PATH" ] \
    && LEDGER_PATH="$SELFTEST_LEDGER_PATH" run_cleanup || true
  [ -z "$SELFTEST_TEMP_DIR" ] || rm -rf "$SELFTEST_TEMP_DIR"
}

run_selftest() {
  local temp_dir metadata ledger source out
  require_absolute_path "--secret-file" "$SECRET_FILE"
  require_corpus_size
  [ -z "${FJ_SCALE_REUSE_FIXTURE:-}" ] || die "FJ_SCALE_REUSE_FIXTURE is not allowed in selftest mode" 2
  temp_dir="$(mktemp -d)"
  metadata="$(CORPUS_SIZE="$CORPUS_SIZE" SECRET_FILE="$SECRET_FILE" WORK_DIR="$temp_dir" run_prepare)"
  ledger="$(printf '%s\n' "$metadata" | jq -r '.ledger_path')"
  source="$(printf '%s\n' "$metadata" | jq -r '.source_index')"
  SELFTEST_TEMP_DIR="$temp_dir"
  SELFTEST_LEDGER_PATH="$ledger"
  trap cleanup_selftest_failure EXIT
  out="$temp_dir/selftest-query.raw"
  load_algolia_credentials
  verify_selftest_count "$source" "$out"
  LEDGER_PATH="$ledger" run_cleanup
  trap - EXIT
  printf '%s\n' "$metadata" | jq -c '{status:"pass", source_index, ledger_path}'
  rm -rf "$temp_dir"
}

parse_prepare_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --corpus-size) CORPUS_SIZE="${2:-}"; shift 2 ;;
      --secret-file) SECRET_FILE="${2:-}"; shift 2 ;;
      --work-dir) WORK_DIR="${2:-}"; shift 2 ;;
      *) usage >&2; die "unknown argument: $1" 2 ;;
    esac
  done
}

parse_cleanup_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --ledger) LEDGER_PATH="${2:-}"; shift 2 ;;
      --secret-file) SECRET_FILE="${2:-}"; shift 2 ;;
      *) usage >&2; die "unknown argument: $1" 2 ;;
    esac
  done
}

parse_source_count_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --index) SOURCE_INDEX="${2:-}"; shift 2 ;;
      --secret-file) SECRET_FILE="${2:-}"; shift 2 ;;
      --work-dir) WORK_DIR="${2:-}"; shift 2 ;;
      *) usage >&2; die "unknown argument: $1" 2 ;;
    esac
  done
}

main() {
  local mode="${1:-}"
  [ "$#" -gt 0 ] && shift || true
  require_tool curl
  require_tool jq
  case "$mode" in
    prepare)
      parse_prepare_args "$@"
      run_prepare
      ;;
    cleanup)
      parse_cleanup_args "$@"
      run_cleanup
      ;;
    source-count)
      parse_source_count_args "$@"
      run_source_count
      ;;
    selftest)
      parse_prepare_args "$@"
      run_selftest
      ;;
    --help|-h)
      usage
      ;;
    *)
      usage >&2
      die "mode must be prepare, cleanup, source-count, or selftest" 2
      ;;
  esac
}

main "$@"
