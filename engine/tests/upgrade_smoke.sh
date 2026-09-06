#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  ./engine/tests/upgrade_smoke.sh \
    --old-bin <path> --old-manifest <path> --old-binary-sha256 <digest> \
    --new-bin <path> --new-manifest <path> \
    --rollback-mode <restore_pre_upgrade_backup|binary_reactivate_same_data>

Runs a minimal upgrade smoke by:
1. starting the old binary on a temp data dir
2. seeding data and verifying search
3. stopping the old binary
4. starting the new binary on the same data dir
5. verifying exact protected identity, health/readiness, dashboard, search, and writes
6. when declared, restarting the predecessor on the post-write data dir and rechecking data
EOF
}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WAIT_HELPER="$SCRIPT_DIR/common/wait_for_flapjack.sh"
ADMIN_KEY=""
INDEX_NAME="upgrade_smoke"
QUERY_TOKEN="upgrade-smoke-token"

OLD_BIN=""
OLD_MANIFEST=""
OLD_BINARY_SHA256=""
NEW_BIN=""
NEW_MANIFEST=""
ROLLBACK_MODE=""
TMP_DIR=""
DATA_DIR=""
OLD_LOG=""
NEW_LOG=""
OLD_PID=""
NEW_PID=""
INTERRUPTED_EXIT_CODE=0

pass() {
  printf 'PASS: %s\n' "$1"
}

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  if [ -n "${OLD_LOG:-}" ] && [ -f "$OLD_LOG" ]; then
    printf '\n== old log ==\n' >&2
    cat "$OLD_LOG" >&2 || true
  fi
  if [ -n "${NEW_LOG:-}" ] && [ -f "$NEW_LOG" ]; then
    printf '\n== new log ==\n' >&2
    cat "$NEW_LOG" >&2 || true
  fi
  exit 1
}

# TODO: Document cleanup.
cleanup() {
  local script_exit_code=$?
  local effective_exit_code="$script_exit_code"
  if [ "$INTERRUPTED_EXIT_CODE" -ne 0 ]; then
    effective_exit_code="$INTERRUPTED_EXIT_CODE"
  fi
  if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
    kill "$OLD_PID" 2>/dev/null || true
    wait "$OLD_PID" 2>/dev/null || true
  fi
  if [ -n "$NEW_PID" ] && kill -0 "$NEW_PID" 2>/dev/null; then
    kill "$NEW_PID" 2>/dev/null || true
    wait "$NEW_PID" 2>/dev/null || true
  fi
  if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
    if [ "$effective_exit_code" -ne 0 ]; then
      local failure_snapshot="/tmp/flapjack_upgrade_smoke_failure_${$}_$(date +%s)"
      cp -R "$TMP_DIR" "$failure_snapshot"
      printf 'INFO: preserved upgrade smoke data at %s\n' "$failure_snapshot"
    else
      rm -rf "$TMP_DIR"
    fi
  fi
}

extract_port_from_log() {
  local log_path="$1"
  sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$log_path" | head -1
}

generate_admin_key() {
  local random_hex
  random_hex="$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  [ -n "$random_hex" ] || fail "failed to generate a random admin key from /dev/urandom"
  printf 'fj_upgrade_smoke_%s\n' "$random_hex"
}

# TODO: Document http_json.
http_json() {
  local method="$1"
  local url="$2"
  local body="${3:-}"
  local -a curl_args=(
    -fsS
    -X "$method"
    "$url"
    -H "content-type: application/json"
    -H "x-algolia-application-id: flapjack"
    -H "x-algolia-api-key: $ADMIN_KEY"
  )

  if [ -n "$body" ]; then
    curl_args+=(-d "$body")
  fi

  curl "${curl_args[@]}"
}

wait_for_task_published() {
  local base_url="$1"
  local task_id="$2"

  for _ in $(seq 1 120); do
    local response
    response="$(http_json GET "$base_url/1/indexes/$INDEX_NAME/task/$task_id")" || true
    if [ -n "$response" ] && [ "$(printf '%s' "$response" | jq -r '.status // empty')" = "published" ]; then
      return 0
    fi
    sleep 0.25
  done

  fail "task $task_id did not reach published state"
}

verify_search_hits() {
  local base_url="$1"
  local query="$2"
  local expected_hits="$3"
  local request_body
  local response
  local hits

  request_body="$(jq -cn --arg query "$query" '{query: $query}')"
  response="$(http_json POST "$base_url/1/indexes/$INDEX_NAME/query" "$request_body")"
  hits="$(printf '%s' "$response" | jq -r '.nbHits')"
  [ "$hits" = "$expected_hits" ] || fail "expected $expected_hits hits for query '$query', got $hits"
}

validate_binary_manifest() {
  local label="$1"
  local bin_path="$2"
  local manifest_path="$3"
  local require_binary_digest="$4"
  local canonical_build_path="$5"
  local expected_binary_digest="$6"

  "$bin_path" build-info --json >"$canonical_build_path" \
    || fail "$label binary did not expose build-info --json"
  python3 - \
    "$label" \
    "$bin_path" \
    "$manifest_path" \
    "$require_binary_digest" \
    "$canonical_build_path" \
    "$expected_binary_digest" <<'PY'
import hashlib
import json
import pathlib
import sys

label = sys.argv[1]
binary_path = pathlib.Path(sys.argv[2])
manifest_path = pathlib.Path(sys.argv[3])
require_binary_digest = sys.argv[4] == "1"
build_path = pathlib.Path(sys.argv[5])
expected_binary_digest = sys.argv[6]


def reject_duplicate_keys(pairs):
    value = {}
    for key, member in pairs:
        if key in value:
            raise ValueError(f"duplicate key: {key}")
        value[key] = member
    return value


def load(path):
    return json.loads(path.read_text(), object_pairs_hook=reject_duplicate_keys)


try:
    manifest = load(manifest_path)
    cli_build = load(build_path)
except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
    raise SystemExit(f"{label} identity input is invalid: {error}") from error

schema_version = manifest.get("schemaVersion")
expected_keys = {
    1: {"schemaVersion", "artifact", "build"},
    2: {"schemaVersion", "artifact", "build", "compatibility"},
}
if schema_version not in expected_keys or set(manifest) != expected_keys[schema_version]:
    raise SystemExit(f"{label} manifest schema or keys are unsupported")
if manifest["build"] != cli_build:
    raise SystemExit(f"{label} manifest build does not match executable build-info")

artifact = manifest["artifact"]
if not isinstance(artifact, dict):
    raise SystemExit(f"{label} manifest artifact must be an object")
if artifact.get("target") != cli_build.get("target"):
    raise SystemExit(f"{label} artifact target does not match executable target")
if artifact.get("profile") != "release" or cli_build.get("profile") != "release":
    raise SystemExit(f"{label} manifest and executable must use the release profile")

binary_digest = artifact.get("binarySha256")
actual_binary_digest = hashlib.sha256(binary_path.read_bytes()).hexdigest()
if expected_binary_digest and actual_binary_digest != expected_binary_digest:
    raise SystemExit(f"{label} executable does not match the declared binarySha256")
if require_binary_digest and binary_digest is None:
    raise SystemExit(f"{label} manifest must bind artifact.binarySha256")
if binary_digest is not None and binary_digest != actual_binary_digest:
    raise SystemExit(f"{label} artifact.binarySha256 does not match executable bytes")
PY
}

start_server() {
  local bin_path="$1"
  local log_path="$2"

  FLAPJACK_ENV=production \
  FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
  FLAPJACK_BIND_ADDR="127.0.0.1:0" \
  FLAPJACK_DATA_DIR="$DATA_DIR" \
  "$bin_path" >"$log_path" 2>&1 &

  printf '%s' "$!"
}

# TODO: Document main.
main() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --old-bin)
        OLD_BIN="${2:-}"
        shift 2
        ;;
      --old-manifest)
        OLD_MANIFEST="${2:-}"
        shift 2
        ;;
      --old-binary-sha256)
        OLD_BINARY_SHA256="${2:-}"
        shift 2
        ;;
      --new-bin)
        NEW_BIN="${2:-}"
        shift 2
        ;;
      --new-manifest)
        NEW_MANIFEST="${2:-}"
        shift 2
        ;;
      --rollback-mode)
        ROLLBACK_MODE="${2:-}"
        shift 2
        ;;
      --help|-h)
        usage
        return 0
        ;;
      *)
        echo "ERROR: unknown argument: $1" >&2
        usage >&2
        return 1
        ;;
    esac
  done

  [ -n "$OLD_BIN" ] || { echo "ERROR: --old-bin is required" >&2; usage >&2; return 1; }
  [ -n "$OLD_MANIFEST" ] || { echo "ERROR: --old-manifest is required" >&2; usage >&2; return 1; }
  [[ "$OLD_BINARY_SHA256" =~ ^[0-9a-f]{64}$ ]] \
    || { echo "ERROR: --old-binary-sha256 must be 64 lowercase hex characters" >&2; usage >&2; return 1; }
  [ -n "$NEW_BIN" ] || { echo "ERROR: --new-bin is required" >&2; usage >&2; return 1; }
  [ -n "$NEW_MANIFEST" ] || { echo "ERROR: --new-manifest is required" >&2; usage >&2; return 1; }
  case "$ROLLBACK_MODE" in
    restore_pre_upgrade_backup|binary_reactivate_same_data) ;;
    *) echo "ERROR: --rollback-mode must name a supported compatibility mode" >&2; usage >&2; return 1 ;;
  esac
  [ -x "$OLD_BIN" ] || fail "old binary is not executable: $OLD_BIN"
  [ -f "$OLD_MANIFEST" ] || fail "old manifest does not exist: $OLD_MANIFEST"
  [ -x "$NEW_BIN" ] || fail "new binary is not executable: $NEW_BIN"
  [ -f "$NEW_MANIFEST" ] || fail "new manifest does not exist: $NEW_MANIFEST"
  [ -x "$WAIT_HELPER" ] || fail "missing wait helper: $WAIT_HELPER"

  ADMIN_KEY="$(generate_admin_key)"
  TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/flapjack-upgrade-smoke.XXXXXX")"
  DATA_DIR="$TMP_DIR/data"
  OLD_LOG="$TMP_DIR/old.log"
  NEW_LOG="$TMP_DIR/new.log"
  mkdir -p "$DATA_DIR"

  local old_build_json="$TMP_DIR/old-build.json"
  local new_build_json="$TMP_DIR/new-build.json"
  validate_binary_manifest "old" "$OLD_BIN" "$OLD_MANIFEST" 0 "$old_build_json" "$OLD_BINARY_SHA256"
  validate_binary_manifest "new" "$NEW_BIN" "$NEW_MANIFEST" 1 "$new_build_json" ""

  OLD_PID="$(start_server "$OLD_BIN" "$OLD_LOG")"
  "$WAIT_HELPER" --pid "$OLD_PID" --port auto --log-path "$OLD_LOG" >/dev/null
  local old_port
  old_port="$(extract_port_from_log "$OLD_LOG")"
  [ -n "$old_port" ] || fail "could not detect old server port"
  local old_base="http://127.0.0.1:$old_port"

  local batch_response
  local task_id
  batch_response="$(http_json POST "$old_base/1/indexes/$INDEX_NAME/batch" '{
    "requests": [
      {
        "action": "addObject",
        "body": {
          "objectID": "old-doc-1",
          "title": "Upgrade smoke old doc",
          "token": "'"$QUERY_TOKEN"'"
        }
      },
      {
        "action": "addObject",
        "body": {
          "objectID": "old-doc-2",
          "title": "Upgrade smoke second doc",
          "token": "'"$QUERY_TOKEN"'"
        }
      }
    ]
  }')"
  task_id="$(printf '%s' "$batch_response" | jq -r '.taskID')"
  wait_for_task_published "$old_base" "$task_id"
  verify_search_hits "$old_base" "$QUERY_TOKEN" "2"
  pass "old binary seeded and searchable"

  kill "$OLD_PID" 2>/dev/null || true
  wait "$OLD_PID" 2>/dev/null || true
  OLD_PID=""

  NEW_PID="$(start_server "$NEW_BIN" "$NEW_LOG")"
  "$WAIT_HELPER" --pid "$NEW_PID" --port auto --log-path "$NEW_LOG" >/dev/null
  local new_port
  new_port="$(extract_port_from_log "$NEW_LOG")"
  [ -n "$new_port" ] || fail "could not detect new server port"
  local new_base="http://127.0.0.1:$new_port"

  curl -fsS "$new_base/health" >/dev/null || fail "new binary health check failed"
  curl -fsS "$new_base/health/ready" >/dev/null || fail "new binary readiness check failed"
  curl -fsS "$new_base/dashboard" >/dev/null || fail "new binary dashboard load failed"
  local live_build_json="$TMP_DIR/live-build.json"
  local expected_live_build_json="$TMP_DIR/expected-live-build.canonical.json"
  local actual_live_build_json="$TMP_DIR/actual-live-build.canonical.json"
  http_json GET "$new_base/internal/build-info" >"$live_build_json" \
    || fail "new binary protected build identity check failed"
  jq -S -c . "$new_build_json" >"$expected_live_build_json" \
    || fail "new binary CLI build identity was not valid JSON"
  jq -S -c . "$live_build_json" >"$actual_live_build_json" \
    || fail "new binary protected build identity was not valid JSON"
  cmp -s "$expected_live_build_json" "$actual_live_build_json" \
    || fail "new binary protected build identity does not match its executable and manifest"
  pass "new binary served the exact authenticated manifest identity"
  verify_search_hits "$new_base" "$QUERY_TOKEN" "2"
  pass "new binary preserved pre-upgrade search state"

  local upgrade_write_response
  local upgrade_task_id
  upgrade_write_response="$(http_json POST "$new_base/1/indexes/$INDEX_NAME/batch" '{
    "requests": [
      {
        "action": "addObject",
        "body": {
          "objectID": "new-doc-1",
          "title": "Upgrade smoke new doc",
          "token": "post-upgrade-token"
        }
      }
    ]
  }')"
  upgrade_task_id="$(printf '%s' "$upgrade_write_response" | jq -r '.taskID')"
  wait_for_task_published "$new_base" "$upgrade_task_id"
  verify_search_hits "$new_base" "post-upgrade-token" "1"
  pass "new binary accepted writes on the upgraded data dir"

  if [ "$ROLLBACK_MODE" = "binary_reactivate_same_data" ]; then
    kill "$NEW_PID" 2>/dev/null || true
    wait "$NEW_PID" 2>/dev/null || true
    NEW_PID=""

    OLD_LOG="$TMP_DIR/rollback-old.log"
    OLD_PID="$(start_server "$OLD_BIN" "$OLD_LOG")"
    "$WAIT_HELPER" --pid "$OLD_PID" --port auto --log-path "$OLD_LOG" >/dev/null
    local rollback_port
    rollback_port="$(extract_port_from_log "$OLD_LOG")"
    [ -n "$rollback_port" ] || fail "could not detect rollback predecessor port"
    local rollback_base="http://127.0.0.1:$rollback_port"

    curl -fsS "$rollback_base/health" >/dev/null \
      || fail "rollback predecessor health check failed"
    curl -fsS "$rollback_base/health/ready" >/dev/null \
      || fail "rollback predecessor readiness check failed"
    verify_search_hits "$rollback_base" "$QUERY_TOKEN" "2"
    verify_search_hits "$rollback_base" "post-upgrade-token" "1"
    pass "predecessor restarted on the post-write data dir and preserved both generations"
  fi
}

trap cleanup EXIT
trap 'INTERRUPTED_EXIT_CODE=130; cleanup; exit 130' INT
trap 'INTERRUPTED_EXIT_CODE=143; cleanup; exit 143' TERM

main "$@"
