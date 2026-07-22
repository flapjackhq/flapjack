#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ACCEPTANCE="$SCRIPT_DIR/ingest_cli_acceptance.sh"

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

assert_nonzero() {
  local name="$1"
  shift
  local log
  log="$(mktemp)"
  set +e
  "$@" >"$log" 2>&1
  local status=$?
  set -e
  if [ "$status" -ne 0 ]; then
    pass "$name"
  else
    fail "$name" "expected non-zero exit, got 0; log: $(cat "$log")"
  fi
  rm -f "$log"
}

main() {
  assert_nonzero 'server-absent acceptance arm fails closed' \
    env INGEST_ACCEPTANCE_SKIP_SERVER_START=1 INGEST_ACCEPTANCE_EXTERNAL_BASE_URL=http://127.0.0.1:9 bash "$ACCEPTANCE"

  assert_nonzero 'wrong-index acceptance arm fails closed' \
    env INGEST_ACCEPTANCE_DEST_INDEX_SUFFIX=_wrong bash "$ACCEPTANCE"

  assert_nonzero 'silently-unapplied-delete acceptance arm fails closed' \
    env INGEST_ACCEPTANCE_DELETE_MODE=skip bash "$ACCEPTANCE"

  printf '\nResults: %d/%d passed\n' "$TESTS_PASSED" "$TESTS_RUN"
  [ "$TESTS_FAILED" -eq 0 ]
}

main "$@"
