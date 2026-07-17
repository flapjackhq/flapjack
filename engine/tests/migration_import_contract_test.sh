#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORACLE="$SCRIPT_DIR/migration_import_contract.sh"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

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

WORK_DIR="$(mktemp -d)"
OWNED_PIDS=()

cleanup() {
  local pid
  for pid in "${OWNED_PIDS[@]:-}"; do
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

write_fake_runtime() {
  local runtime="$1"
  mkdir -p "$runtime/bin" "$runtime/state"

  cat >"$runtime/fake-flapjack" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
mkdir -p "$MIGRATION_IMPORT_CONTRACT_STUB_DIR"
printf '%s\n' "$0" >"$MIGRATION_IMPORT_CONTRACT_STUB_DIR/binary_ran"
printf '%s\n' "$$" >"$MIGRATION_IMPORT_CONTRACT_STUB_DIR/server_pid"
printf '%s\n' "$FLAPJACK_DATA_DIR" >"$MIGRATION_IMPORT_CONTRACT_STUB_DIR/data_dir"
mkdir -p "$FLAPJACK_DATA_DIR/migration_exports/jobs/fake-job"
printf 'artifact\n' >"$FLAPJACK_DATA_DIR/migration_exports/jobs/fake-job/artifact.txt"
printf 'Local: http://127.0.0.1:54321\n'
trap 'exit 0' TERM INT
while :; do sleep 1; done
SH
  chmod +x "$runtime/fake-flapjack"

  cat >"$runtime/bin/curl" <<'PY'
#!/usr/bin/env python3
import json
import os
import signal
import sys
import urllib.parse
from pathlib import Path

state = Path(os.environ["MIGRATION_IMPORT_CONTRACT_STUB_DIR"])
scenario = os.environ.get("MIGRATION_IMPORT_CONTRACT_SCENARIO", "unavailable_ok")
state.mkdir(parents=True, exist_ok=True)

def append(name, value):
    with (state / name).open("a", encoding="utf-8") as f:
        f.write(value + "\n")

def respond(payload, code=200):
    if isinstance(payload, str):
        sys.stdout.write(payload)
    else:
        sys.stdout.write(json.dumps(payload, separators=(",", ":")))
    sys.stdout.write("\n" + str(code))

def parse_args(argv):
    method = "GET"
    body = ""
    url = ""
    fail_health = False
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "-X":
            method = argv[i + 1]
            i += 2
        elif arg == "--data":
            body = argv[i + 1]
            i += 2
        elif arg in ("-H", "-w"):
            i += 2
        elif arg == "-sf":
            fail_health = True
            i += 1
        elif arg.startswith("http://") or arg.startswith("https://"):
            url = arg
            i += 1
        else:
            i += 1
    return method, body, url, fail_health

method, body, url, fail_health = parse_args(sys.argv[1:])
parsed = urllib.parse.urlparse(url)

if fail_health and parsed.path == "/health":
    sys.exit(0)

append("request_order.log", f"{method} {parsed.path}")
if body:
    append("request_bodies.log", body)
    try:
        payload = json.loads(body)
        if "targetIndex" in payload:
            (state / "target_index").write_text(payload["targetIndex"], encoding="utf-8")
    except json.JSONDecodeError:
        pass

if scenario in ("self_int", "self_term") and parsed.path == "/1/indexes":
    os.kill(os.getppid(), signal.SIGINT if scenario == "self_int" else signal.SIGTERM)
    sys.exit(130 if scenario == "self_int" else 143)

target = ""
target_file = state / "target_index"
if target_file.exists():
    target = target_file.read_text(encoding="utf-8").strip()

if parsed.path == "/1/migrate-from-algolia" and method == "POST":
    if scenario == "unavailable_ok":
        respond({"code": "migration_import_unavailable", "message": "import unavailable"}, 503)
    elif scenario == "unavailable_returns_2xx":
        respond({"status": "complete", "objects": {"imported": 7}}, 200)
    elif scenario == "unavailable_wrong_code":
        respond({"code": "wrong_code", "message": "not this"}, 503)
    elif scenario == "malformed_migration_json":
        respond("{not-json", 503)
    elif scenario == "importing_ok":
        respond({"status": "complete", "objects": {"imported": 7}}, 200)
    elif scenario == "importing_returns_503":
        respond({"code": "migration_import_unavailable"}, 503)
    elif scenario == "importing_wrong_count":
        respond({"status": "complete", "objects": {"imported": 8}}, 200)
    elif scenario in ("self_int", "self_term", "cleanup_failure"):
        respond({"code": "migration_import_unavailable", "message": "import unavailable"}, 503)
    else:
        respond({"message": f"unexpected scenario {scenario}"}, 500)
elif parsed.path == "/1/indexes" and method == "GET":
    if scenario == "malformed_indexes_json":
        respond("{not-json", 200)
    elif scenario == "unavailable_lists_target":
        respond({"items": [{"name": target, "entries": 0}]}, 200)
    elif scenario == "importing_ok":
        respond({"items": [{"name": target, "entries": 7}]}, 200)
    elif scenario == "importing_omits_target":
        respond({"items": []}, 200)
    elif scenario == "importing_duplicates_target":
        respond({"items": [{"name": target, "entries": 7}, {"name": target, "entries": 7}]}, 200)
    elif scenario == "importing_wrong_count":
        respond({"items": [{"name": target, "entries": 7}]}, 200)
    else:
        respond({"items": []}, 200)
else:
    respond({"message": "unexpected request", "method": method, "path": parsed.path}, 500)
PY
  chmod +x "$runtime/bin/curl"
}

secret_file_for() {
  local runtime="$1"
  mkdir -p "$runtime"
  printf 'ALGOLIA_APP_ID=APPID_CANARY\nALGOLIA_ADMIN_KEY=ADMIN_SECRET_CANARY\n' >"$runtime/secret.env"
  printf '%s\n' "$runtime/secret.env"
}

run_oracle_with_stub() {
  local scenario="$1" out="$2" runtime="$3"
  shift 3
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    FLAPJACK_BIN="$runtime/fake-flapjack" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="$scenario" \
    bash "$ORACLE" "$@" >"$out" 2>&1
  local rc=$?
  set -e
  if [ -f "$runtime/state/server_pid" ]; then
    OWNED_PIDS+=("$(cat "$runtime/state/server_pid")")
  fi
  printf '%s' "$rc"
}

extract_evidence_path() {
  sed -n 's/^INFO: preserved sanitized migration import evidence at //p' "$1" | tail -1
}

evidence_has_contract_files() {
  local evidence="$1"
  [ -d "$evidence" ] \
    && [ -f "$evidence/logs/flapjack-server.log" ] \
    && [ -f "$evidence/logs/migration-response.raw" ] \
    && [ -f "$evidence/logs/list-indices.raw" ] \
    && [ -f "$evidence/receipt.json" ] \
    && [ -d "$evidence/migration_exports/jobs" ]
}

assert_success_scenario() {
  local label="$1" scenario="$2" mode="$3" runtime out rc secret args data_dir
  runtime="$WORK_DIR/$scenario"
  out="$runtime.out"
  if [ "$mode" = "importing" ]; then
    secret="$(secret_file_for "$runtime")"
    args=(--expect-mode importing --secret-file "$secret" --source-index source_products --target-index target_products)
  else
    args=(--expect-mode unavailable)
  fi
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" "${args[@]}")"
  data_dir="$(cat "$runtime/state/data_dir" 2>/dev/null || true)"
  if [ "$rc" = "0" ] \
    && [ -f "$runtime/state/binary_ran" ] \
    && [ "$(cat "$runtime/state/binary_ran")" = "$runtime/fake-flapjack" ] \
    && cmp -s <(printf 'POST /1/migrate-from-algolia\nGET /1/indexes\n') "$runtime/state/request_order.log" \
    && [ -n "$data_dir" ] \
    && [ ! -e "$data_dir" ] \
    && [ -z "$(extract_evidence_path "$out")" ]; then
    pass "$label"
  else
    fail "$label" "rc=$rc output=$(cat "$out")"
  fi
}

assert_failure_scenario() {
  local label="$1" scenario="$2" mode="$3" runtime out rc secret args evidence data_dir server_pid
  runtime="$WORK_DIR/$scenario"
  out="$runtime.out"
  if [ "$mode" = "importing" ]; then
    secret="$(secret_file_for "$runtime")"
    args=(--expect-mode importing --secret-file "$secret" --source-index source_products --target-index target_products)
  else
    args=(--expect-mode unavailable)
  fi
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" "${args[@]}")"
  evidence="$(extract_evidence_path "$out")"
  data_dir="$(cat "$runtime/state/data_dir" 2>/dev/null || true)"
  server_pid="$(cat "$runtime/state/server_pid" 2>/dev/null || true)"
  if [ "$rc" != "0" ] \
    && evidence_has_contract_files "$evidence" \
    && [ -n "$data_dir" ] \
    && [ ! -e "$data_dir" ] \
    && { [ -z "$server_pid" ] || ! kill -0 "$server_pid" 2>/dev/null; } \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out" "$evidence"/* "$evidence"/logs/* 2>/dev/null; then
    rm -rf "$evidence"
    pass "$label"
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail "$label" "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_argument_contract() {
  local label="$1" expected_rc_relation="$2" runtime out rc
  shift 2
  runtime="$WORK_DIR/args-${label//[^A-Za-z0-9_]/_}"
  out="$runtime.out"
  rc="$(run_oracle_with_stub unavailable_ok "$out" "$runtime" "$@")"
  case "$expected_rc_relation" in
    zero)
      [ "$rc" = "0" ] && pass "$label" || fail "$label" "rc=$rc output=$(cat "$out")"
      ;;
    nonzero)
      if [ "$rc" != "0" ] && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
        pass "$label"
      else
        fail "$label" "rc=$rc output=$(cat "$out")"
      fi
      ;;
  esac
}

assert_signal_scenario() {
  local label="$1" scenario="$2" expected_rc="$3" runtime out rc evidence data_dir server_pid
  runtime="$WORK_DIR/$scenario"
  out="$runtime.out"
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" --expect-mode unavailable)"
  evidence="$(extract_evidence_path "$out")"
  data_dir="$(cat "$runtime/state/data_dir" 2>/dev/null || true)"
  server_pid="$(cat "$runtime/state/server_pid" 2>/dev/null || true)"
  if [ "$rc" = "$expected_rc" ] \
    && evidence_has_contract_files "$evidence" \
    && [ -n "$data_dir" ] \
    && [ ! -e "$data_dir" ] \
    && { [ -z "$server_pid" ] || ! kill -0 "$server_pid" 2>/dev/null; }; then
    rm -rf "$evidence"
    pass "$label"
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail "$label" "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_cleanup_failure_scenario() {
  local runtime out rc evidence data_dir
  runtime="$WORK_DIR/cleanup_failure"
  out="$runtime.out"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    FLAPJACK_BIN="$runtime/fake-flapjack" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="cleanup_failure" \
    MIGRATION_IMPORT_CONTRACT_SIMULATE_CLEANUP_FAILURE=1 \
    bash "$ORACLE" --expect-mode unavailable >"$out" 2>&1
  rc=$?
  set -e
  [ ! -f "$runtime/state/server_pid" ] || OWNED_PIDS+=("$(cat "$runtime/state/server_pid")")
  evidence="$(extract_evidence_path "$out")"
  data_dir="$(cat "$runtime/state/data_dir" 2>/dev/null || true)"
  if [ "$rc" != "0" ] \
    && evidence_has_contract_files "$evidence" \
    && [ -n "$data_dir" ] \
    && [ ! -e "$data_dir" ]; then
    rm -rf "$evidence"
    pass 'simulated cleanup failure preserves evidence and exits nonzero'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'simulated cleanup failure preserves evidence and exits nonzero' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_static_contract() {
  if [ -f "$ORACLE" ]; then
    pass 'oracle file exists'
  else
    fail 'oracle file exists' "$ORACLE"
    return
  fi
  [ -x "$ORACLE" ] && pass 'oracle is executable' || fail 'oracle is executable'
  grep -Fq 'set -euo pipefail' "$ORACLE" && pass 'oracle enables strict mode' || fail 'oracle enables strict mode'
  grep -Fq 'load_named_secrets "$SECRET_FILE" ALGOLIA_APP_ID ALGOLIA_ADMIN_KEY' "$ORACLE" \
    && pass 'oracle loads only required Algolia secrets in importing mode' \
    || fail 'oracle loads only required Algolia secrets in importing mode'
  grep -Fq '/1/indexes"' "$ORACLE" \
    && ! grep -Fq '/1/indexes/${TARGET_INDEX}' "$ORACLE" \
    && pass 'oracle uses list-indices metadata surface, not single-index search route' \
    || fail 'oracle uses list-indices metadata surface, not single-index search route'
}

main() {
  echo 'migration_import_contract oracle meta-test'
  assert_static_contract

  assert_success_scenario 'unavailable positive control passes' unavailable_ok unavailable
  assert_success_scenario 'importing positive control passes' importing_ok importing

  assert_failure_scenario 'unavailable returning 2xx fails closed' unavailable_returns_2xx unavailable
  assert_failure_scenario 'unavailable wrong 503 code fails closed' unavailable_wrong_code unavailable
  assert_failure_scenario 'unavailable listed target fails closed' unavailable_lists_target unavailable
  assert_failure_scenario 'importing returning 503 fails closed' importing_returns_503 importing
  assert_failure_scenario 'importing omitted target fails closed' importing_omits_target importing
  assert_failure_scenario 'importing duplicated target fails closed' importing_duplicates_target importing
  assert_failure_scenario 'importing count mismatch fails closed' importing_wrong_count importing
  assert_failure_scenario 'malformed migration response fails closed' malformed_migration_json unavailable
  assert_failure_scenario 'malformed list-indices response fails closed' malformed_indexes_json unavailable

  assert_argument_contract 'missing expect-mode fails' nonzero
  assert_argument_contract 'unknown expect-mode fails' nonzero --expect-mode future
  assert_argument_contract 'unavailable refuses secret-file' nonzero --expect-mode unavailable --secret-file "$WORK_DIR/secret.env"
  assert_argument_contract 'unavailable refuses source-index' nonzero --expect-mode unavailable --source-index source
  assert_argument_contract 'unavailable refuses target-index' nonzero --expect-mode unavailable --target-index target
  assert_argument_contract 'importing requires secret-file' nonzero --expect-mode importing --source-index source --target-index target
  assert_argument_contract 'importing requires source-index' nonzero --expect-mode importing --secret-file "$WORK_DIR/secret.env" --target-index target
  assert_argument_contract 'importing requires target-index' nonzero --expect-mode importing --secret-file "$WORK_DIR/secret.env" --source-index source
  assert_argument_contract 'importing requires absolute secret-file path' nonzero --expect-mode importing --secret-file relative.env --source-index source --target-index target
  assert_argument_contract 'missing importing secret file is sanitized' nonzero --expect-mode importing --secret-file "$WORK_DIR/missing.env" --source-index source --target-index target

  assert_signal_scenario 'INT preserves evidence, stops server, and returns 130' self_int 130
  assert_signal_scenario 'TERM preserves evidence, stops server, and returns 143' self_term 143
  assert_cleanup_failure_scenario

  printf '\nResults: %d/%d passed\n' "$TESTS_PASSED" "$TESTS_RUN"
  if [ "$TESTS_FAILED" -gt 0 ]; then
    printf '%d test(s) failed\n' "$TESTS_FAILED"
    return 1
  fi
  echo 'All tests passed'
}

main "$@"
