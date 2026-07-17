#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ENGINE_DIR="$REPO_DIR/engine"
WAIT_HELPER="$SCRIPT_DIR/common/wait_for_flapjack.sh"
SECRET_HELPER="$SCRIPT_DIR/common/load_named_secrets.sh"

EXPECT_MODE=""
SECRET_FILE=""
SOURCE_INDEX=""
TARGET_INDEX=""
SOURCE_APP_ID=""
SOURCE_API_KEY=""

WORK_DIR=""
DATA_DIR=""
LOG_DIR=""
RECEIPT=""
SERVER_PID=""
SERVER_LOG=""
BASE_URL=""
ADMIN_KEY=""
BIN_PATH=""
RUN_PREFIX=""
PASS_COMPLETE=0
INTERRUPTED_EXIT_CODE=0
CLEANUP_FAILED=0
EVIDENCE_DIR=""
EVIDENCE_ANNOUNCED=0

usage() {
  cat <<'EOF'
Usage:
  migration_import_contract.sh --expect-mode unavailable
  migration_import_contract.sh --expect-mode importing --secret-file <absolute-path> --source-index <name> --target-index <name>
EOF
}

die() {
  printf 'ERROR: %s\n' "$1" >&2
  exit "${2:-1}"
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"
}

http_body() { sed '$d'; }
http_code() { tail -1; }

http_success_code() {
  local code="$1"
  [[ "$code" =~ ^[0-9]+$ ]] && [ "$code" -ge 200 ] && [ "$code" -le 299 ]
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --expect-mode)
        EXPECT_MODE="${2:-}"
        shift 2
        ;;
      --secret-file)
        SECRET_FILE="${2:-}"
        shift 2
        ;;
      --source-index)
        SOURCE_INDEX="${2:-}"
        shift 2
        ;;
      --target-index)
        TARGET_INDEX="${2:-}"
        shift 2
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        usage >&2
        die "unknown argument: $1" 2
        ;;
    esac
  done

  case "$EXPECT_MODE" in
    unavailable)
      [ -z "$SECRET_FILE" ] || die "--secret-file is not allowed in unavailable mode" 2
      [ -z "$SOURCE_INDEX" ] || die "--source-index is not allowed in unavailable mode" 2
      [ -z "$TARGET_INDEX" ] || die "--target-index is not allowed in unavailable mode" 2
      ;;
    importing)
      [ -n "$SECRET_FILE" ] || die "--secret-file is required in importing mode" 2
      [ -n "$SOURCE_INDEX" ] || die "--source-index is required in importing mode" 2
      [ -n "$TARGET_INDEX" ] || die "--target-index is required in importing mode" 2
      case "$SECRET_FILE" in
        /*) ;;
        *) die "--secret-file must be an absolute path in importing mode" 2 ;;
      esac
      ;;
    "")
      usage >&2
      die "--expect-mode is required" 2
      ;;
    *)
      usage >&2
      die "--expect-mode must be unavailable or importing" 2
      ;;
  esac
}

load_credentials() {
  if [ "$EXPECT_MODE" = "unavailable" ]; then
    SOURCE_APP_ID="stub_app_id"
    SOURCE_API_KEY="stub_source_key"
    return
  fi

  # shellcheck source=engine/tests/common/load_named_secrets.sh
  source "$SECRET_HELPER"
  local loader_output
  loader_output="$(mktemp)"
  if ! load_named_secrets "$SECRET_FILE" ALGOLIA_APP_ID ALGOLIA_ADMIN_KEY >"$loader_output" 2>&1; then
    rm -f "$loader_output"
    die "required Algolia credentials could not be loaded"
  fi
  rm -f "$loader_output"
  SOURCE_APP_ID="$ALGOLIA_APP_ID"
  SOURCE_API_KEY="$ALGOLIA_ADMIN_KEY"
}

init_run() {
  require_tool curl
  require_tool jq
  require_tool od
  require_tool sed
  require_tool tr

  WORK_DIR="$(mktemp -d)"
  DATA_DIR="$WORK_DIR/flapjack-data"
  LOG_DIR="$WORK_DIR/logs"
  RECEIPT="$WORK_DIR/receipt.json"
  mkdir -p "$DATA_DIR" "$LOG_DIR"
  SERVER_LOG="$LOG_DIR/flapjack-server.log"
  : >"$LOG_DIR/migration-response.raw"
  : >"$LOG_DIR/list-indices.raw"

  local random_hex
  random_hex="$(od -An -N8 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  [ -n "$random_hex" ] || die "failed to generate run entropy"
  RUN_PREFIX="fj_migration_import_${random_hex}"
  ADMIN_KEY="fj_import_contract_$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"

  if [ "$EXPECT_MODE" = "unavailable" ]; then
    SOURCE_INDEX="${RUN_PREFIX}_source"
    TARGET_INDEX="${RUN_PREFIX}_target"
  fi

  jq -n \
    --arg mode "$EXPECT_MODE" \
    --arg source "$SOURCE_INDEX" \
    --arg target "$TARGET_INDEX" \
    --arg head "$(git -C "$REPO_DIR" rev-parse HEAD 2>/dev/null || true)" \
    '{mode:$mode, source_index:$source, target_index:$target, head:$head, checks:[]}' >"$RECEIPT"
}

record_check() {
  local name="$1" status="$2" detail="${3:-}"
  local next
  next="$(mktemp)"
  jq --arg name "$name" --arg status "$status" --arg detail "$detail" \
    '.checks += [{name:$name,status:$status,detail:$detail}]' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

build_or_resolve_binary() {
  if [ -n "${FLAPJACK_BIN:-}" ]; then
    [ -x "$FLAPJACK_BIN" ] || die "FLAPJACK_BIN is not executable"
    BIN_PATH="$FLAPJACK_BIN"
    return
  fi

  if (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$LOG_DIR/build.log" 2>&1); then
    BIN_PATH="$ENGINE_DIR/target/debug/flapjack"
  else
    die "cargo build -p flapjack-server failed"
  fi
  [ -x "$BIN_PATH" ] || die "expected flapjack binary was not built"
}

start_server() {
  FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$DATA_DIR" \
    "$BIN_PATH" --auto-port >"$SERVER_LOG" 2>&1 &
  SERVER_PID=$!

  "$WAIT_HELPER" --pid "$SERVER_PID" --host 127.0.0.1 --port auto --log-path "$SERVER_LOG" --retries 80 --interval-seconds 0.5
  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$SERVER_LOG" | head -1)"
  [ -n "$port" ] || die "server became ready but no auto-port was found"
  BASE_URL="http://127.0.0.1:${port}"
  record_check "local_server" "pass" "started"
}

flapjack_request() {
  local method="$1" path="$2" body="$3" out="$4" status body_file=""
  set +e
  if [ -n "$body" ]; then
    body_file="$(mktemp "$WORK_DIR/curl-body.XXXXXX")" || {
      set -e
      die "failed to create curl request body file"
    }
    chmod 600 "$body_file" 2>/dev/null || true
    printf '%s' "$body" >"$body_file" || {
      rm -f "$body_file"
      set -e
      die "failed to write curl request body file"
    }
  fi
  {
    printf 'silent\n'
    printf 'show-error\n'
    printf 'request = "%s"\n' "$method"
    printf 'url = "%s%s"\n' "$BASE_URL" "$path"
    printf 'header = "x-algolia-application-id: flapjack"\n'
    printf 'header = "x-algolia-api-key: %s"\n' "$ADMIN_KEY"
    printf 'header = "content-type: application/json"\n'
    if [ -n "$body_file" ]; then
      printf 'data-binary = "@%s"\n' "$body_file"
    fi
  } | curl -w '\n%{http_code}' --config - >"$out"
  status=$?
  [ -z "$body_file" ] || rm -f "$body_file"
  set -e
  return "$status"
}

migration_payload() {
  local app_json key_json source_json target_json
  app_json="$(printf '%s' "$SOURCE_APP_ID" | jq -Rs .)"
  key_json="$(printf '%s' "$SOURCE_API_KEY" | jq -Rs .)"
  source_json="$(printf '%s' "$SOURCE_INDEX" | jq -Rs .)"
  target_json="$(printf '%s' "$TARGET_INDEX" | jq -Rs .)"
  printf '{"appId":%s,"apiKey":%s,"sourceIndex":%s,"targetIndex":%s}\n' \
    "$app_json" "$key_json" "$source_json" "$target_json"
}

assert_unavailable() {
  local body code payload target_count
  body="$(migration_payload)"
  flapjack_request POST "/1/migrate-from-algolia" "$body" "$LOG_DIR/migration-response.raw" \
    || die "migration request transport failed"
  code="$(http_code <"$LOG_DIR/migration-response.raw")"
  payload="$(http_body <"$LOG_DIR/migration-response.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/migration-response.json"

  [ "$code" = "503" ] || die "unavailable mode expected HTTP 503, got ${code}"
  jq -e '.code == "migration_import_unavailable"' "$LOG_DIR/migration-response.json" >/dev/null \
    || die "unavailable mode response code was not migration_import_unavailable"
  record_check "migration_refusal" "pass" "503 migration_import_unavailable"

  flapjack_request GET "/1/indexes" "" "$LOG_DIR/list-indices.raw" \
    || die "list-indices request transport failed"
  code="$(http_code <"$LOG_DIR/list-indices.raw")"
  payload="$(http_body <"$LOG_DIR/list-indices.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/list-indices.json"
  http_success_code "$code" || die "GET /1/indexes returned HTTP ${code}"
  target_count="$(jq -er --arg target "$TARGET_INDEX" '[.items[]? | select(.name == $target)] | length' "$LOG_DIR/list-indices.json")" \
    || die "GET /1/indexes response was malformed"
  [ "$target_count" = "0" ] || die "unavailable mode created or exposed target index"
  record_check "target_absent" "pass" "target not listed"
}

assert_importing() {
  local body code payload imported matches
  body="$(migration_payload)"
  flapjack_request POST "/1/migrate-from-algolia" "$body" "$LOG_DIR/migration-response.raw" \
    || die "migration request transport failed"
  code="$(http_code <"$LOG_DIR/migration-response.raw")"
  payload="$(http_body <"$LOG_DIR/migration-response.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/migration-response.json"

  http_success_code "$code" || die "importing mode expected 2xx, got HTTP ${code}"
  imported="$(jq -er 'if (.objects.imported | type) == "number" and (.objects.imported | floor) == .objects.imported then .objects.imported else empty end' "$LOG_DIR/migration-response.json")" \
    || die "importing mode response was missing integer objects.imported"
  record_check "migration_import" "pass" "objects.imported=${imported}"

  flapjack_request GET "/1/indexes" "" "$LOG_DIR/list-indices.raw" \
    || die "list-indices request transport failed"
  code="$(http_code <"$LOG_DIR/list-indices.raw")"
  payload="$(http_body <"$LOG_DIR/list-indices.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/list-indices.json"
  http_success_code "$code" || die "GET /1/indexes returned HTTP ${code}"
  matches="$(jq -cer --arg target "$TARGET_INDEX" '[.items[]? | select(.name == $target)]' "$LOG_DIR/list-indices.json")" \
    || die "GET /1/indexes response was malformed"
  [ "$(printf '%s\n' "$matches" | jq 'length')" = "1" ] \
    || die "importing mode expected exactly one target index listing"
  [ "$(printf '%s\n' "$matches" | jq -r '.[0].entries')" = "$imported" ] \
    || die "importing mode target entries did not equal imported count"
  record_check "target_entries" "pass" "entries=${imported}"
}

preserve_run_evidence() {
  local announce="${1:-1}"
  if [ -z "$EVIDENCE_DIR" ]; then
    EVIDENCE_DIR="/tmp/flapjack_migration_import_contract_evidence_${$}_$(date +%s)"
    mkdir -p "$EVIDENCE_DIR"
    chmod 700 "$EVIDENCE_DIR" 2>/dev/null || true
    [ -n "$LOG_DIR" ] && [ -d "$LOG_DIR" ] && cp -R "$LOG_DIR" "$EVIDENCE_DIR/logs" 2>/dev/null || true
    [ -n "$RECEIPT" ] && [ -f "$RECEIPT" ] && cp "$RECEIPT" "$EVIDENCE_DIR/receipt.json" 2>/dev/null || true
    if [ -n "$DATA_DIR" ] && [ -d "$DATA_DIR/migration_exports/jobs" ]; then
      mkdir -p "$EVIDENCE_DIR/migration_exports"
      cp -R "$DATA_DIR/migration_exports/jobs" "$EVIDENCE_DIR/migration_exports/jobs" 2>/dev/null || true
    fi
  fi
  if [ "$announce" -eq 1 ] && [ "$EVIDENCE_ANNOUNCED" -eq 0 ]; then
    printf 'INFO: preserved sanitized migration import evidence at %s\n' "$EVIDENCE_DIR" >&2
    EVIDENCE_ANNOUNCED=1
  fi
}

cleanup() {
  local script_exit_code=$?
  local effective_exit_code="$script_exit_code"
  [ "$INTERRUPTED_EXIT_CODE" -eq 0 ] || effective_exit_code="$INTERRUPTED_EXIT_CODE"
  trap - EXIT INT TERM
  set +e

  if [ "${MIGRATION_IMPORT_CONTRACT_SIMULATE_CLEANUP_FAILURE:-0}" = "1" ]; then
    CLEANUP_FAILED=1
    printf 'ERROR: simulated cleanup failure\n' >&2
  fi

  if [ "$PASS_COMPLETE" -ne 1 ] || [ "$effective_exit_code" -ne 0 ] || [ "$CLEANUP_FAILED" -ne 0 ]; then
    preserve_run_evidence 1
  fi

  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi

  [ -z "$WORK_DIR" ] || rm -rf "$WORK_DIR" 2>/dev/null || CLEANUP_FAILED=1

  if [ "$CLEANUP_FAILED" -ne 0 ] && [ "$effective_exit_code" -eq 0 ]; then
    exit 1
  fi
  if [ "$effective_exit_code" -ne "$script_exit_code" ]; then
    exit "$effective_exit_code"
  fi
}

main() {
  parse_args "$@"
  load_credentials
  init_run
  trap cleanup EXIT
  trap 'INTERRUPTED_EXIT_CODE=130; exit 130' INT
  trap 'INTERRUPTED_EXIT_CODE=143; exit 143' TERM
  build_or_resolve_binary
  start_server

  if [ "$EXPECT_MODE" = "unavailable" ]; then
    assert_unavailable
  else
    assert_importing
  fi

  PASS_COMPLETE=1
  record_check "contract_complete" "pass" "$EXPECT_MODE"
  jq -c '{mode, source_index, target_index, checks}' "$RECEIPT"
}

main "$@"
