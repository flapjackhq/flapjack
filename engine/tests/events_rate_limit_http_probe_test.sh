#!/usr/bin/env bash

# Fail-capability self-test for events_rate_limit_http_probe.sh.
#
# Each run inverts exactly one expected wire/store fact. A valid detection must
# reach the positive server/key/allowance/flush denominator, run all six checks,
# fail only the named label, and exit 1. Exit 2 is indeterminate setup evidence.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE="$SCRIPT_DIR/events_rate_limit_http_probe.sh"
EXPECTED_CHECKS=6

TMP_ROOT=""
INVERTED=0
DETECTED=0
UNKNOWN=0

cleanup() {
  local script_exit_code=$?
  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    if [ "$UNKNOWN" -ne 0 ] || [ "$script_exit_code" -ne 0 ]; then
      printf 'INFO: preserved SEC-EVENTS-2 probe self-test evidence at %s\n' "$TMP_ROOT" >&2
    else
      rm -rf "$TMP_ROOT"
    fi
  fi
}
trap cleanup EXIT

require_probe() {
  if [ ! -x "$PROBE" ]; then
    printf 'ERROR: production probe is not executable: %s\n' "$PROBE" >&2
    exit 2
  fi
  for tool in awk bash grep mktemp paste sort; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required self-test tool not found: %s\n' "$tool" >&2
      exit 2
    fi
  done
}

inversion_facts() {
  local output_path="$1" exit_code="$2"
  local denominator failed_labels check_count pass_count

  if grep -Eq '^SEC_EVENTS_2_DENOMINATOR key_status=200 allowed_statuses=200,200 flush_status=200$' "$output_path"; then
    denominator="yes"
  else
    denominator="no"
  fi
  failed_labels="$(grep -Eo '^\[FAIL\] [A-Z0-9_]+' "$output_path" \
    | awk '{print $2}' | LC_ALL=C sort -u | paste -sd, - || true)"
  if [ -z "$failed_labels" ]; then
    failed_labels="none"
  fi
  check_count="$(grep -Ec '^\[(PASS|FAIL)\] ' "$output_path" || true)"
  pass_count="$(grep -Ec '^\[PASS\] ' "$output_path" || true)"

  printf 'exit=%s|denominator=%s|failed=%s|checks=%s|passes=%s\n' \
    "$exit_code" "$denominator" "$failed_labels" "$check_count" "$pass_count"
}

run_inversion() {
  local case_label="$1" inverted_variable="$2" inverted_value="$3" expected_failed_label="$4"
  local output_path="$TMP_ROOT/${case_label}.log" exit_code=0 expected actual

  INVERTED=$((INVERTED + 1))
  env \
    -u FLAPJACK_BIN \
    -u SEC_EVENTS_2_EXPECT_FIRST_EXCESS_STATUS \
    -u SEC_EVENTS_2_EXPECT_FIRST_EXCESS_BODY \
    -u SEC_EVENTS_2_EXPECT_DEBUG_REJECTED \
    -u SEC_EVENTS_2_EXPECT_ANALYTICS_REJECTED \
    -u SEC_EVENTS_2_EXPECT_DEBUG_ACCEPTED \
    -u SEC_EVENTS_2_EXPECT_ANALYTICS_ACCEPTED \
    "${inverted_variable}=${inverted_value}" \
    bash "$PROBE" >"$output_path" 2>&1 || exit_code=$?

  expected="exit=1|denominator=yes|failed=${expected_failed_label}|checks=${EXPECTED_CHECKS}|passes=5"
  actual="$(inversion_facts "$output_path" "$exit_code")"
  if [ "$actual" = "$expected" ]; then
    DETECTED=$((DETECTED + 1))
    printf '[PASS] %s expected=%s actual=%s\n' "$case_label" "$expected" "$actual"
  else
    UNKNOWN=$((UNKNOWN + 1))
    printf '[FAIL] %s expected=%s actual=%s\n' "$case_label" "$expected" "$actual"
  fi
}

main() {
  require_probe
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fj-sec-events-2-probe-selftest.XXXXXX")"

  run_inversion SELFTEST_WRONG_FIRST_EXCESS_STATUS \
    SEC_EVENTS_2_EXPECT_FIRST_EXCESS_STATUS 200 FIRST_EXCESS_STATUS
  run_inversion SELFTEST_WRONG_FIRST_EXCESS_BODY \
    SEC_EVENTS_2_EXPECT_FIRST_EXCESS_BODY \
    '{"message":"wrong rate-limit body","status":429}' FIRST_EXCESS_BODY
  run_inversion SELFTEST_WRONG_DEBUG_REJECTED_ABSENCE \
    SEC_EVENTS_2_EXPECT_DEBUG_REJECTED present DEBUG_REJECTED_ABSENT
  run_inversion SELFTEST_WRONG_ANALYTICS_REJECTED_ABSENCE \
    SEC_EVENTS_2_EXPECT_ANALYTICS_REJECTED present ANALYTICS_REJECTED_ABSENT
  run_inversion SELFTEST_WRONG_DEBUG_ACCEPTED_EXACTNESS \
    SEC_EVENTS_2_EXPECT_DEBUG_ACCEPTED \
    'status=200,count=1,ids=sec-events-2-live-accepted-a' DEBUG_ACCEPTED_EXACT
  run_inversion SELFTEST_WRONG_ANALYTICS_ACCEPTED_EXACTNESS \
    SEC_EVENTS_2_EXPECT_ANALYTICS_ACCEPTED \
    'status=200,rows=2,hits=sec-events-2-live-accepted-a=2,sec-events-2-live-accepted-b=1' \
    ANALYTICS_ACCEPTED_EXACT

  if [ "$INVERTED" -ne 6 ]; then
    printf 'ERROR: expected 6 inversions, ran %s\n' "$INVERTED" >&2
    exit 2
  fi
  printf 'SEC_EVENTS_2_PROBE_SELFTEST inverted=%s detected=%s unknown=%s verdict=%s\n' \
    "$INVERTED" "$DETECTED" "$UNKNOWN" \
    "$([ "$DETECTED" -eq 6 ] && [ "$UNKNOWN" -eq 0 ] && printf PASS || printf FAIL)"
  if [ "$DETECTED" -ne 6 ] || [ "$UNKNOWN" -ne 0 ]; then
    exit 1
  fi
}

main "$@"
