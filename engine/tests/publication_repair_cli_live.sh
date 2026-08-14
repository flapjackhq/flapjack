#!/usr/bin/env bash
# Live contract for the shipped repair-publication CLI against generated crash layouts.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$ENGINE_DIR/.." && pwd)"

BINARY_ARG=""
MANIFEST_ARG=""
ARTIFACT_ARG=""
BINARY_PATH=""
MANIFEST_PATH=""
ARTIFACT_DIR=""
TIMEOUT_BIN=""
HELPER=""
HELPER_SOURCE="$SCRIPT_DIR/publication_repair_cli_live_helper.py"
CHILD_PID=""
CHILD_TARGET_MODE=""
FLAPJACK_ENV_ARGS=()
SERVER_BIND_ADDR=""
SERVER_STDOUT_PATH=""
SERVER_STDERR_PATH=""
LAST_CHILD_PID=""
LAST_SERVER_BIND_ADDR=""
LAST_SERVER_STDOUT_PATH=""
LAST_SERVER_STDERR_PATH=""
CURRENT_CASE_ID=""
FAILURE_PHASE="argument_validation"
FAILURE_REASON=""
EVIDENCE_READY=0

usage() {
  cat <<'EOF'
Usage:
  publication_repair_cli_live.sh --binary <absolute-path> --manifest <absolute-path> --artifact-dir <absolute-temp>

Options:
  --binary        Absolute path to the release flapjack executable to test.
  --manifest      Absolute path to publication_repair_cli_scenarios.json.
  --artifact-dir  Existing empty absolute artifact directory outside the checkout.
  --help          Show this help text.
EOF
}

die() {
  FAILURE_REASON="$*"
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

append_cleanup_trace() {
  [ "$EVIDENCE_READY" -eq 1 ] || return 0
  python3 "$HELPER" append-cleanup-trace \
    "$ARTIFACT_DIR/.runner/cleanup_trace.jsonl" "$@"
}

evidence_command() {
  local command="$1"
  local original_status="$2"
  local final_status="$3"
  local cleanup_outcome="$4"
  python3 "$HELPER" "$command" \
    "$ARTIFACT_DIR" "$MANIFEST_PATH" "$REPO_DIR" \
    "$original_status" "$final_status" "$FAILURE_PHASE" "$FAILURE_REASON" \
    "$LAST_CHILD_PID" "$LAST_SERVER_BIND_ADDR" \
    "$LAST_SERVER_STDOUT_PATH" "$LAST_SERVER_STDERR_PATH" \
    "$CURRENT_CASE_ID" "$cleanup_outcome"
}

cleanup() {
  local original_status=$?
  local final_status="$original_status"
  local cleanup_failed=0
  trap - EXIT
  if [ "$EVIDENCE_READY" -eq 1 ]; then
    append_cleanup_trace cleanup_started "${CHILD_PID:-}" "" "" "" "" started || cleanup_failed=1
    evidence_command snapshot-evidence "$original_status" "$final_status" in_progress || cleanup_failed=1
  fi
  stop_server || cleanup_failed=1
  if [ "$original_status" -eq 0 ] && [ "$cleanup_failed" -ne 0 ]; then
    final_status=1
    FAILURE_PHASE="cleanup"
  fi
  if [ "$EVIDENCE_READY" -eq 1 ] && [ -f "$HELPER" ]; then
    append_cleanup_trace cleanup_finished "" "" "" "" "" \
      "$(if [ "$cleanup_failed" -eq 0 ]; then printf succeeded; else printf failed; fi)" || cleanup_failed=1
    if ! evidence_command finalize-evidence "$original_status" "$final_status" \
      "$(if [ "$cleanup_failed" -eq 0 ]; then printf succeeded; else printf failed; fi)"; then
      cleanup_failed=1
      if [ "$original_status" -eq 0 ]; then
        final_status=1
      fi
    fi
  fi
  exit "$final_status"
}
trap cleanup EXIT

handle_signal() {
  local signal_name="$1"
  local signal_number="$2"
  FAILURE_PHASE="signal"
  FAILURE_REASON="runner received $signal_name"
  exit $((128 + signal_number))
}
trap 'handle_signal INT 2' INT
trap 'handle_signal TERM 15' TERM
trap 'handle_signal HUP 1' HUP

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --binary)
        [ -z "$BINARY_ARG" ] || die "--binary may only be provided once"
        [ "$#" -ge 2 ] || die "--binary requires a value"
        BINARY_ARG="$2"
        shift 2
        ;;
      --manifest)
        [ -z "$MANIFEST_ARG" ] || die "--manifest may only be provided once"
        [ "$#" -ge 2 ] || die "--manifest requires a value"
        MANIFEST_ARG="$2"
        shift 2
        ;;
      --artifact-dir)
        [ -z "$ARTIFACT_ARG" ] || die "--artifact-dir may only be provided once"
        [ "$#" -ge 2 ] || die "--artifact-dir requires a value"
        ARTIFACT_ARG="$2"
        shift 2
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        usage >&2
        die "unknown argument: $1"
        ;;
    esac
  done
  [ -n "$BINARY_ARG" ] || die "--binary is required"
  [ -n "$MANIFEST_ARG" ] || die "--manifest is required"
  [ -n "$ARTIFACT_ARG" ] || die "--artifact-dir is required"
}

require_tools() {
  local missing=0
  local tool
  for tool in bash cargo cp git python3 mktemp; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  if command -v gtimeout >/dev/null 2>&1; then
    TIMEOUT_BIN="$(command -v gtimeout)"
  elif command -v timeout >/dev/null 2>&1; then
    TIMEOUT_BIN="$(command -v timeout)"
  else
    printf 'ERROR: required tool not found: timeout or gtimeout\n' >&2
    missing=1
  fi
  [ "$missing" -eq 0 ] || exit 1
}

canonical_path() {
  python3 - "$1" <<'PY'
import pathlib
import sys
print(pathlib.Path(sys.argv[1]).resolve(strict=True))
PY
}

validate_paths() {
  [[ "$BINARY_ARG" = /* ]] || die "binary must be an absolute path"
  [[ "$MANIFEST_ARG" = /* ]] || die "manifest must be an absolute path"
  [[ "$ARTIFACT_ARG" = /* ]] || die "artifact directory must be an absolute path"

  BINARY_PATH="$(canonical_path "$BINARY_ARG")"
  MANIFEST_PATH="$(canonical_path "$MANIFEST_ARG")"
  ARTIFACT_DIR="$(canonical_path "$ARTIFACT_ARG")"

  [ -f "$BINARY_PATH" ] || die "binary must be a regular file: $BINARY_ARG"
  [ -x "$BINARY_PATH" ] || die "binary must be executable: $BINARY_ARG"
  [ -f "$MANIFEST_PATH" ] || die "manifest must be a regular file: $MANIFEST_ARG"
  [ -d "$ARTIFACT_DIR" ] || die "artifact directory must exist: $ARTIFACT_ARG"

  python3 - "$REPO_DIR" "$ARTIFACT_DIR" "$MANIFEST_PATH" <<'PY'
import json
import os
import pathlib
import re
import sys

repo = pathlib.Path(sys.argv[1]).resolve()
artifact = pathlib.Path(sys.argv[2]).resolve()
manifest_path = pathlib.Path(sys.argv[3])
home = pathlib.Path.home().resolve()
tmp = pathlib.Path("/tmp").resolve()
safe_path_component = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")

def fail(message):
    raise SystemExit(message)

if artifact in (pathlib.Path("/").resolve(), home, tmp):
    fail(f"artifact directory is not an allowed root: {artifact}")
if os.path.commonpath([repo, artifact]) == str(repo):
    fail(f"artifact directory must be outside the checkout: {artifact}")
if any(artifact.iterdir()):
    fail(f"artifact directory must be empty: {artifact}")

with manifest_path.open(encoding="utf-8") as handle:
    manifest = json.load(handle)
scenarios = manifest.get("scenarios")
if not isinstance(scenarios, list):
    fail("manifest scenarios must be a list")
for index, scenario in enumerate(scenarios):
    if not isinstance(scenario, dict):
        fail(f"manifest scenario {index} must be an object")
    path_fields = {
        "id": scenario.get("id"),
        "tenant": scenario.get("tenant") or "products",
        "transaction": scenario.get("transaction") or "txn_001",
    }
    for field, value in path_fields.items():
        if not isinstance(value, str) or not safe_path_component.fullmatch(value):
            fail(
                f"manifest scenario {index} {field} must be a non-empty safe path component"
            )
PY
}

validate_test_invoke_mode() {
  local mode="${PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST:-}"
  local harness_pid="${PUBLICATION_REPAIR_CLI_TEST_HARNESS_PID:-}"
  local harness_command="$(ps -o command= -p "$PPID" 2>/dev/null || true)"
  case "$mode" in
    ""|skip_first_repair) ;;
    *) die "unknown PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST: $mode" ;;
  esac
  if [ -n "$mode" ] && [ "${PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST:-0}" != "1" ]; then
    die "PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST requires PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST=1"
  fi
  if [ -n "$mode" ] && { [ "$harness_pid" != "$PPID" ] || ! [[ "$harness_pid" =~ ^[0-9]+$ ]] || [[ "$harness_command" != *publication_repair_cli_live_test.sh* ]]; }; then
    die "PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST is restricted to publication_repair_cli_live_test.sh"
  fi
}

collect_flapjack_env() {
  local name
  FLAPJACK_ENV_ARGS=()
  while IFS='=' read -r name _; do
    case "$name" in
      FLAPJACK_*) FLAPJACK_ENV_ARGS+=("-u" "$name") ;;
    esac
  done < <(env)
}

run_bounded() {
  local status=0
  local restore_errexit=0
  [[ "$-" == *e* ]] && restore_errexit=1
  set +e
  "$TIMEOUT_BIN" --kill-after=30s "$@" &
  CHILD_PID=$!
  CHILD_TARGET_MODE="direct"
  wait "$CHILD_PID"
  status=$?
  CHILD_PID=""
  CHILD_TARGET_MODE=""
  [ "$restore_errexit" -eq 0 ] || set -e
  return "$status"
}

# Copy the tracked implementation into the evidence directory so cleanup and
# failure snapshots keep the same transient-artifact contract as before.
write_helper() {
  [ -f "$HELPER_SOURCE" ] || die "publication repair helper is missing: $HELPER_SOURCE"
  mkdir -p "$ARTIFACT_DIR/.runner"
  HELPER="$ARTIFACT_DIR/.runner/publication_repair_cli_live_helper.py"
  cp "$HELPER_SOURCE" "$HELPER"
}

assert_build_info_json() {
  local path="$1"
  local revision="$2"
  python3 - "$path" "$revision" <<'PY'
import json
import sys

path, revision = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as handle:
    value = json.load(handle)

def fail(message):
    raise SystemExit(f"{path}: {message}: {value}")

if value.get("schemaVersion") != 1:
    fail("schemaVersion must be 1")
if value.get("revision") != revision:
    fail("revision must match reviewed HEAD")
if value.get("revisionKnown") is not True:
    fail("revisionKnown must be true")
dirty = value.get("dirty")
dirty_known = value.get("dirtyKnown")
if dirty is True:
    fail("dirty must not be true")
if dirty is None:
    if dirty_known is not False:
        fail("dirtyKnown must be false when dirty is null")
elif dirty is False:
    if dirty_known is not True:
        fail("dirtyKnown must be true when dirty is false")
else:
    fail("dirty must be a boolean or null")
features = value.get("features")
if not isinstance(features, list) or features != sorted(features):
    fail("features must be sorted")
capabilities = value.get("capabilities")
if set(capabilities or {}) != {"vectorSearch", "vectorSearchLocal"}:
    fail("capabilities must contain only vectorSearch and vectorSearchLocal")
if not all(isinstance(capabilities[key], bool) for key in capabilities):
    fail("capabilities values must be booleans")
serialized = json.dumps(value, sort_keys=True, separators=(",", ":"))
for forbidden in ("algolia_migration_v1", "algoliaMigrationV1"):
    if forbidden in serialized:
        fail(f"serialized payload must not contain {forbidden}")
PY
}

identity_gate() {
  local revision=""
  local status=""
  local build_info="$ARTIFACT_DIR/.runner/build_info.json"
  revision="$(git -C "$REPO_DIR" rev-parse HEAD)"
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || die "reviewed revision must be a 40-character lowercase SHA"
  if [ "${PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST:-0}" != "1" ]; then
    status="$(git -C "$REPO_DIR" status --short)"
    [ -z "$status" ] || die "checkout must be clean before live publication repair contract"
  fi
  collect_flapjack_env
  run_bounded 60s env ${FLAPJACK_ENV_ARGS[@]+"${FLAPJACK_ENV_ARGS[@]}"} "$BINARY_PATH" build-info --json >"$build_info"
  assert_build_info_json "$build_info" "$revision"
}

run_generator() {
  local generated_dir="$ARTIFACT_DIR/generated"
  mkdir -p "$generated_dir"
  (
    cd "$ENGINE_DIR"
    collect_flapjack_env
    run_bounded 600s env ${FLAPJACK_ENV_ARGS[@]+"${FLAPJACK_ENV_ARGS[@]}"} \
      PUBLICATION_REPAIR_CLI_MANIFEST="$MANIFEST_PATH" \
      PUBLICATION_REPAIR_CLI_ARTIFACT_DIR="$generated_dir" \
      cargo test -p flapjack --lib publication_repair_cli -- --ignored
  )
  [ -f "$generated_dir/generated_layouts.json" ] || die "generator did not write generated_layouts.json"
  python3 "$HELPER" validate-generated "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$generated_dir" \
    >"$ARTIFACT_DIR/.runner/scenario_ids.txt"
}

invoke_cli() {
  local case_root="$1"
  local target="$2"
  local stdout_path="$3"
  local stderr_path="$4"
  local mode="${PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST:-}"
  local receipt_path="${stderr_path}.invoke_receipt"
  local status=0
  local restore_errexit=0
  if [ "$mode" = "skip_first_repair" ] && [[ "$(basename "$stdout_path")" = *.first.stdout.json ]]; then
    python3 - "$target" >"$stdout_path" <<'PY'
import json
import sys
print(json.dumps({
    "tenant": sys.argv[1],
    "status": "clean",
    "action": "none",
    "transaction_id": None,
    "phase": None,
    "evidence": None,
}, separators=(",", ":")))
PY
    : >"$stderr_path"
    [ -f "$receipt_path" ] || die "repair-publication subprocess invocation missing for $(basename "$stdout_path")"
  fi
  collect_flapjack_env
  [[ "$-" == *e* ]] && restore_errexit=1
  set +e
  run_bounded 120s env ${FLAPJACK_ENV_ARGS[@]+"${FLAPJACK_ENV_ARGS[@]}"} \
    "$BINARY_PATH" --data-dir "$case_root" repair-publication --tenant "$target" --json \
    >"$stdout_path" 2>"$stderr_path"
  status=$?
  [ "$restore_errexit" -eq 0 ] || set -e
  printf 'repair-publication subprocess invoked\n' >"$receipt_path"
  return "$status"
}

server_output_path() {
  local ident="$1"
  local label="$2"
  printf '%s\n' "$ARTIFACT_DIR/.runner/${ident}.${label}.server"
}

wait_for_startup_bind_addr() {
  local ident="$1"
  local timeout_secs="$2"
  local deadline=$((SECONDS + timeout_secs))
  local bind_addr=""

  while :; do
    if bind_addr="$(python3 "$HELPER" startup-bind-addr "$SERVER_STDOUT_PATH" 2>/dev/null)"; then
      SERVER_BIND_ADDR="$bind_addr"
      return 0
    fi
    if [ -n "$CHILD_PID" ] && ! kill -0 "$CHILD_PID" 2>/dev/null; then
      local status=0
      set +e
      wait "$CHILD_PID"
      status=$?
      set -e
      CHILD_PID=""
      die "$ident server exited before startup banner (status $status); stdout: $SERVER_STDOUT_PATH stderr: $SERVER_STDERR_PATH"
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      local stdout_path="$SERVER_STDOUT_PATH"
      local stderr_path="$SERVER_STDERR_PATH"
      stop_server
      die "$ident timed out waiting for startup banner; stdout: $stdout_path stderr: $stderr_path"
    fi
    sleep 0.1
  done
}

wait_for_health() {
  local ident="$1"
  local bind_addr="$2"
  local timeout_secs="$3"
  local deadline=$((SECONDS + timeout_secs))
  local health_probe_stderr="$ARTIFACT_DIR/.runner/${ident}.health_probe.stderr"
  : >"$health_probe_stderr"

  while :; do
    if python3 "$HELPER" probe-health "$bind_addr" "$ARTIFACT_DIR/.runner/build_info.json" >/dev/null 2>"$health_probe_stderr"; then
      return 0
    fi
    if [ -n "$CHILD_PID" ] && ! kill -0 "$CHILD_PID" 2>/dev/null; then
      local status=0
      set +e
      wait "$CHILD_PID"
      status=$?
      set -e
      CHILD_PID=""
      die "$ident server exited before /health became ready (status $status); stdout: $SERVER_STDOUT_PATH stderr: $SERVER_STDERR_PATH"
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      local stdout_path="$SERVER_STDOUT_PATH"
      local stderr_path="$SERVER_STDERR_PATH"
      local health_error=""
      health_error="$(tail -n 1 "$health_probe_stderr" 2>/dev/null || true)"
      stop_server
      die "$ident /health probe timed out; last health error: $health_error; stdout: $stdout_path stderr: $stderr_path"
    fi
    sleep 0.05
  done
}

start_server() {
  local case_root="$1"
  local ident="$2"
  local label="$3"

  FAILURE_PHASE="server_startup"
  SERVER_STDOUT_PATH="$(server_output_path "$ident" "$label").stdout"
  SERVER_STDERR_PATH="$(server_output_path "$ident" "$label").stderr"
  : >"$SERVER_STDOUT_PATH"
  : >"$SERVER_STDERR_PATH"

  collect_flapjack_env
  python3 -c 'import os, sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])' \
    env ${FLAPJACK_ENV_ARGS[@]+"${FLAPJACK_ENV_ARGS[@]}"} \
    FLAPJACK_ANALYTICS_ENABLED=false \
    "$BINARY_PATH" --data-dir "$case_root" --auto-port --no-auth \
    >"$SERVER_STDOUT_PATH" 2>"$SERVER_STDERR_PATH" &
  CHILD_PID=$!
  CHILD_TARGET_MODE="group"
  LAST_CHILD_PID="$CHILD_PID"
  LAST_SERVER_STDOUT_PATH="$SERVER_STDOUT_PATH"
  LAST_SERVER_STDERR_PATH="$SERVER_STDERR_PATH"

  wait_for_startup_bind_addr "$ident" 5
  LAST_SERVER_BIND_ADDR="$SERVER_BIND_ADDR"
  FAILURE_PHASE="server_health"
  wait_for_health "$ident" "$SERVER_BIND_ADDR" 5
}

server_process_group_alive() {
  local pid="$1"
  if [ "$CHILD_TARGET_MODE" = "group" ]; then
    kill -0 -- "-$pid" 2>/dev/null || kill -0 "$pid" 2>/dev/null
  else
    kill -0 "$pid" 2>/dev/null
  fi
}

send_server_signal() {
  local signal="$1"
  local pid="$2"
  if [ "$CHILD_TARGET_MODE" = "group" ]; then
    if kill "-$signal" -- "-$pid" 2>/dev/null; then
      append_cleanup_trace signal_attempt "$pid" "$signal" group "-$pid" not_needed success
      return 0
    fi
    if kill "-$signal" "$pid" 2>/dev/null; then
      append_cleanup_trace signal_attempt "$pid" "$signal" direct "$pid" group_failed success
      return 0
    fi
    append_cleanup_trace signal_attempt "$pid" "$signal" direct "$pid" group_failed failed
    return 1
  fi
  if kill "-$signal" "$pid" 2>/dev/null; then
    append_cleanup_trace signal_attempt "$pid" "$signal" direct "$pid" not_applicable success
    return 0
  fi
  append_cleanup_trace signal_attempt "$pid" "$signal" direct "$pid" not_applicable failed
  return 1
}

wait_for_server_process_group_exit() {
  local pid="$1"
  local timeout_secs="$2"
  local deadline=$((SECONDS + timeout_secs))

  while server_process_group_alive "$pid"; do
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 0.1
  done
}

# Reap the recorded direct child without blocking past the bounded shutdown
# budget. A plain `wait "$pid"` blocks until the child exits, so a child that
# has not yet died (e.g. delayed KILL delivery or an uninterruptible state)
# would stall shutdown indefinitely before the final process-group check runs.
# Poll the child's liveness instead; once it is gone the child is a reapable
# zombie and `wait` returns its status immediately. If the child is still alive
# after the budget, give up so the caller fails closed rather than blocking.
reap_child_within_budget() {
  local pid="$1"
  local timeout_secs="$2"
  local deadline=$((SECONDS + timeout_secs))

  while kill -0 "$pid" 2>/dev/null; do
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 0.05
  done

  wait "$pid" >/dev/null 2>&1 || true
  return 0
}

clear_server_state() {
  append_cleanup_trace state_cleared "${CHILD_PID:-}" "" "${CHILD_TARGET_MODE:-}" "${CHILD_PID:-}" "" cleared || true
  CHILD_PID=""
  CHILD_TARGET_MODE=""
  SERVER_BIND_ADDR=""
  SERVER_STDOUT_PATH=""
  SERVER_STDERR_PATH=""
}

stop_server() {
  local pid="${CHILD_PID:-}"

  if [ -z "$pid" ]; then
    clear_server_state
    return 0
  fi

  if server_process_group_alive "$pid"; then
    send_server_signal TERM "$pid" || true
    if ! wait_for_server_process_group_exit "$pid" 1; then
      append_cleanup_trace wait_timeout "$pid" TERM "$CHILD_TARGET_MODE" "$pid" "" timed_out || true
      send_server_signal KILL "$pid" || true
      if ! reap_child_within_budget "$pid" 1; then
        append_cleanup_trace reap_result "$pid" KILL "$CHILD_TARGET_MODE" "$pid" "" timed_out || true
        printf 'ERROR: server child could not be reaped within the bounded shutdown budget after KILL (pid %s)\n' "$pid" >&2
        clear_server_state
        return 1
      fi
      append_cleanup_trace reap_result "$pid" KILL "$CHILD_TARGET_MODE" "$pid" "" reaped || true
      if ! wait_for_server_process_group_exit "$pid" 1; then
        append_cleanup_trace wait_timeout "$pid" KILL "$CHILD_TARGET_MODE" "$pid" "" timed_out || true
        printf 'ERROR: server process group remained alive after KILL (pid %s)\n' "$pid" >&2
        clear_server_state
        return 1
      fi
    else
      if reap_child_within_budget "$pid" 1; then
        append_cleanup_trace reap_result "$pid" TERM "$CHILD_TARGET_MODE" "$pid" "" reaped || true
      else
        append_cleanup_trace reap_result "$pid" TERM "$CHILD_TARGET_MODE" "$pid" "" timed_out || true
      fi
    fi
  fi

  clear_server_state
}

run_server_lifecycle() {
  local case_root="$1"
  local ident="$2"
  local label="$3"
  local receipt_path="$4"

  start_server "$case_root" "$ident" "$label"
  FAILURE_PHASE="http_projection"
  python3 "$HELPER" assert-http-projection "$MANIFEST_PATH" "$ARTIFACT_DIR/generated/generated_layouts.json" "$ident" "$SERVER_BIND_ADDR" "$ARTIFACT_DIR/.runner/build_info.json" "$receipt_path" || {
    local status=$?
    stop_server || true
    return "$status"
  }
  stop_server
}

assert_exit_code() {
  local actual="$1"
  local expected="$2"
  local ident="$3"
  [ "$actual" -eq "$expected" ] || die "$ident CLI exit code $actual does not match manifest $expected"
}

manifest_exit_code() {
  python3 - "$MANIFEST_PATH" "$1" <<'PY'
import json
import sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
for scenario in manifest["scenarios"]:
    if scenario["id"] == sys.argv[2]:
        print(scenario["cli"]["exit_code"])
        break
else:
    raise SystemExit(f"unknown scenario {sys.argv[2]}")
PY
}

manifest_disposition() {
  python3 - "$MANIFEST_PATH" "$1" <<'PY'
import json
import sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
for scenario in manifest["scenarios"]:
    if scenario["id"] == sys.argv[2]:
        print(scenario["disposition"])
        break
else:
    raise SystemExit(f"unknown scenario {sys.argv[2]}")
PY
}

run_case() {
  local ident="$1"
  CURRENT_CASE_ID="$ident"
  local generated_dir="$ARTIFACT_DIR/generated"
  local repair_root="$ARTIFACT_DIR/repair"
  local case_root="$repair_root/$ident"
  local target=""
  local expected_exit=""
  local first_status=""
  local second_status=""
  local first_stdout="$ARTIFACT_DIR/.runner/${ident}.first.stdout.json"
  local first_stderr="$ARTIFACT_DIR/.runner/${ident}.first.stderr"
  local second_stdout="$ARTIFACT_DIR/.runner/${ident}.second.stdout.json"
  local second_stderr="$ARTIFACT_DIR/.runner/${ident}.second.stderr"
  local first_managed="$ARTIFACT_DIR/.runner/${ident}.first.managed.json"
  local second_managed="$ARTIFACT_DIR/.runner/${ident}.second.managed.json"
  local first_projection="$ARTIFACT_DIR/.runner/${ident}.first.projection.json"
  local second_projection="$ARTIFACT_DIR/.runner/${ident}.second.projection.json"
  local disposition=""

  python3 "$HELPER" clone-case "$generated_dir" "$repair_root" "$ident"
  target="$(python3 "$HELPER" target "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$ident")"
  expected_exit="$(manifest_exit_code "$ident")"
  disposition="$(manifest_disposition "$ident")"

  set +e
  invoke_cli "$case_root" "$target" "$first_stdout" "$first_stderr"
  first_status=$?
  set -e
  assert_exit_code "$first_status" "$expected_exit" "$ident"
  FAILURE_PHASE="report_assertion"
  python3 "$HELPER" assert-report "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$ident" "$first_stdout" first
  FAILURE_PHASE="state_assertion"
  python3 "$HELPER" assert-state "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$generated_dir" "$case_root" "$ident" true
  run_server_lifecycle "$case_root" "$ident" first "$first_projection"
  python3 "$HELPER" managed-snapshot "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$case_root" "$ident" >"$first_managed"

  if [ "$disposition" = "commit" ] || [ "$disposition" = "rollback" ] || [ "$disposition" = "absent-create" ]; then
    set +e
    invoke_cli "$case_root" "$target" "$second_stdout" "$second_stderr"
    second_status=$?
    set -e
    assert_exit_code "$second_status" 0 "$ident second"
    python3 "$HELPER" assert-clean-report "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$ident" "$second_stdout"
    python3 "$HELPER" managed-snapshot "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$case_root" "$ident" >"$second_managed"
    python3 "$HELPER" assert-equal-managed-snapshot "$first_managed" "$second_managed" "$ident"
    run_server_lifecycle "$case_root" "$ident" second "$second_projection"
    python3 "$HELPER" assert-equal-projection-receipt "$first_projection" "$second_projection" "$ident"
    if [ "$(cat "$first_stdout")" = "$(cat "$second_stdout")" ]; then
      python3 "$HELPER" assert-equal-report-json "$first_stdout" "$second_stdout" "$ident"
    fi
  else
    [ "$expected_exit" -ne 0 ] || die "$ident unclassified disposition has zero exit"
  fi
}

run_contract() {
  local ident=""
  mkdir -p "$ARTIFACT_DIR/repair"
  while IFS= read -r ident; do
    [ -n "$ident" ] || continue
    run_case "$ident"
  done <"$ARTIFACT_DIR/.runner/scenario_ids.txt"
  printf 'PASS: publication repair CLI live contract passed\n'
}

main() {
  FAILURE_PHASE="argument_validation"
  parse_args "$@"
  require_tools
  validate_paths
  validate_test_invoke_mode
  write_helper
  EVIDENCE_READY=1
  FAILURE_PHASE="identity_gate"
  identity_gate
  FAILURE_PHASE="generator"
  run_generator
  FAILURE_PHASE="contract_case"
  run_contract
  FAILURE_PHASE="complete"
}

main "$@"
