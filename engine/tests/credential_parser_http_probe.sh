#!/usr/bin/env bash
#
# credential_parser_http_probe.sh — Observe the SEC-G1 secured-key parser defect
# at the SERVED pre-auth HTTP boundary, through the real flapjack-server binary.
#
# The in-process Rust regression
# (engine/tests/test_security_audit.rs::a07_secured_key_validation_survives_non_char_boundary_payload)
# guards flapjack_http::auth::validate_secured_key against decoded secured keys
# with malformed byte boundaries. This driver exercises the same parser through
# an unauthenticated client's real served pre-auth path: it starts an
# auth-enabled server and sends malformed `x-algolia-api-key` values to
# `/1/indexes/probe/query`.
#
# The served pre-auth contract is that EVERY malformed key is rejected with the
# canonical 403 body without crashing the parser:
#
#   {"message":"Invalid Application-ID or API key","status":403}
#
# Against an unfixed checkout the non-char-boundary key panics the connection
# task, so curl observes a dropped connection (curl_exit != 0) and the server
# log records a `panicked` line. Every other malformed key must also return the
# canonical 403 without leaking which parser guard rejected it.
#
# Exit codes:
#   0  GREEN  — every malformed key returned the canonical 403, no panic.
#   1  RED    — a served assertion failed (curl_exit / status / body / panicked).
#   2  INDET  — harness/setup could not establish a served baseline (prints
#              INDETERMINATE); NOT a valid red proof.
#
# Usage:
#   bash engine/tests/credential_parser_http_probe.sh
#
# Environment:
#   FLAPJACK_BIN  Optional path to a prebuilt flapjack binary (skips cargo build).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_HELPER="$SCRIPT_DIR/common/wait_for_flapjack.sh"

readonly PROBE_ADMIN_KEY="credential-parser-http-probe-admin-key"
readonly PROBE_APPLICATION_ID="credential-parser-http-probe-app"
readonly CANONICAL_403='{"message":"Invalid Application-ID or API key","status":403}'

BIN=""
TMP=""
LOG=""
SERVER_PID=""
BASE=""
CHECKS_RUN=0
CHECKS_FAILED=0

pass() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  printf '  [PASS] %s\n' "$1"
}

fail() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  CHECKS_FAILED=$((CHECKS_FAILED + 1))
  printf '  [FAIL] %s\n' "$1" >&2
  if [ -n "${2:-}" ]; then
    printf '         %s\n' "$2" >&2
  fi
}

# INDETERMINATE is reserved for harness/setup failures that never established a
# served baseline. A genuine RED must NOT print this token.
die_indeterminate() {
  printf 'INDETERMINATE: %s\n' "$1" >&2
  if [ -n "$LOG" ] && [ -f "$LOG" ]; then
    printf '---- server log ----\n' >&2
    cat "$LOG" >&2 || true
  fi
  exit 2
}

# Kill and wait only the exact server PID this script started. Preserve the temp
# directory (server log, request/response bodies) whenever anything failed so
# the red evidence survives teardown.
cleanup() {
  local script_exit_code=$?
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [ -n "$TMP" ] && [ -d "$TMP" ]; then
    if [ "$CHECKS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      printf 'INFO: preserved credential parser probe evidence at %s\n' "$TMP" >&2
    else
      rm -rf "$TMP"
    fi
  fi
}
trap cleanup EXIT

require_tools() {
  local tool missing=0
  for tool in curl jq sed grep base64 mktemp; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  [ "$missing" -eq 0 ] || die_indeterminate 'required tools missing'
  [ -x "$WAIT_HELPER" ] || die_indeterminate "readiness helper not executable: $WAIT_HELPER"
}

# Resolve engine/target honoring CARGO_TARGET_DIR, mirroring the sibling harness.
target_dir() {
  if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    printf '%s\n' "$ENGINE_DIR/target"
  elif [ "${CARGO_TARGET_DIR#/}" != "$CARGO_TARGET_DIR" ]; then
    printf '%s\n' "$CARGO_TARGET_DIR"
  else
    printf '%s\n' "$ENGINE_DIR/$CARGO_TARGET_DIR"
  fi
}

build_or_resolve_binary() {
  if [ -n "${FLAPJACK_BIN:-}" ]; then
    [ -x "$FLAPJACK_BIN" ] || die_indeterminate "FLAPJACK_BIN=$FLAPJACK_BIN is not executable"
    BIN="$FLAPJACK_BIN"
    printf 'Using pre-built binary: %s\n' "$BIN"
    return
  fi

  printf 'Building flapjack-server debug binary...\n'
  local build_log="$TMP/build.log"
  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$build_log" 2>&1); then
    tail -30 "$build_log" >&2 || true
    die_indeterminate 'cargo build -p flapjack-server failed'
  fi

  # flapjack-server/Cargo.toml names the binary `flapjack`, not `flapjack-server`.
  BIN="$(target_dir)/debug/flapjack"
  [ -x "$BIN" ] || die_indeterminate "expected debug binary at $BIN"
}

start_server() {
  LOG="$TMP/server.log"
  # Authentication ENABLED (admin key set, no --no-auth): the probe must observe
  # the real pre-auth credential path, not an auth-bypassed server.
  env \
    FLAPJACK_ADMIN_KEY="$PROBE_ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$TMP" \
    "$BIN" --auto-port >"$LOG" 2>&1 &
  SERVER_PID=$!

  if ! "$WAIT_HELPER" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$LOG" \
    --retries 80 \
    --interval-seconds 0.5; then
    die_indeterminate 'server did not reach exact-200 /health'
  fi

  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$LOG" | head -1)"
  [ -n "$port" ] || die_indeterminate 'server healthy but no auto-port in startup log'
  BASE="http://127.0.0.1:${port}"
  printf 'Server ready at %s (pid %s)\n' "$BASE" "$SERVER_PID"
}

# Build the base64 of: 63 ASCII bytes, then `é` (bytes 0xC3 0xA9 straddling
# offset 64), then trailing ASCII. This is the served analogue of the in-process
# reproducer; byte offset 64 is NOT a UTF-8 char boundary.
non_char_boundary_key() {
  { printf 'a%.0s' $(seq 63); printf '\303\251'; printf 'trailing_ascii_params'; } \
    | base64 | tr -d '\n'
}

ascii_hex_hmac_prefix() {
  printf 'a%.0s' $(seq 64)
}

# Base64 of a valid 64-byte ASCII-hex HMAC prefix followed by raw 0xFF. This
# passes the length and hex guards before the parameter suffix rejects as UTF-8.
invalid_utf8_key() {
  { ascii_hex_hmac_prefix; printf '\377'; } | base64 | tr -d '\n'
}

# Base64 of a valid ASCII-hex HMAC prefix with an empty parameter suffix. This
# reaches HMAC verification and is rejected because the digest does not match.
hmac_mismatch_key() {
  ascii_hex_hmac_prefix | base64 | tr -d '\n'
}

# Send one malformed key and assert the served pre-auth contract. Records the
# `curl_exit=` marker on a dropped connection and the `panicked` lines are
# checked separately after the loop.
probe_malformed_key() {
  local label="$1" key="$2"
  local body_file="$TMP/body_${label}.json"
  local status_file="$TMP/status_${label}.txt"
  local curl_exit http_status body_canonical

  set +e
  http_status="$(curl -sS \
    -o "$body_file" \
    -w '%{http_code}' \
    -X POST \
    -H 'Content-Type: application/json' \
    -H "x-algolia-application-id: ${PROBE_APPLICATION_ID}" \
    -H "x-algolia-api-key: ${key}" \
    --data '{"query":"probe"}' \
    "${BASE}/1/indexes/probe/query")"
  curl_exit=$?
  set -e
  printf '%s\n' "$http_status" >"$status_file"

  # Assert 1: transport succeeded. A parser panic drops the connection, so this
  # is the primary served RED signal for the non-char-boundary key.
  if [ "$curl_exit" -eq 0 ]; then
    pass "key=${label} curl_exit=0 (connection completed)"
  else
    fail "key=${label} curl_exit=${curl_exit} (connection dropped by served parser)" \
      "a dropped connection means the pre-auth credential parser crashed the request task"
    return
  fi

  # Assert 2: canonical 403 status.
  if [ "$http_status" = "403" ]; then
    pass "key=${label} HTTP status 403"
  else
    fail "key=${label} HTTP status ${http_status} (expected 403)" \
      "body: $(cat "$body_file" 2>/dev/null || true)"
  fi

  # Assert 3: canonical, non-leaky error body (key-sorted for a stable compare).
  if body_canonical="$(jq -S -c . "$body_file" 2>/dev/null)" \
    && [ "$body_canonical" = "$CANONICAL_403" ]; then
    pass "key=${label} canonical 403 body"
  else
    fail "key=${label} non-canonical error body" \
      "got: ${body_canonical:-<unparseable>} want: ${CANONICAL_403}"
  fi
}

run_probe() {
  local non_char_boundary invalid_utf8 hmac_mismatch
  non_char_boundary="$(non_char_boundary_key)"
  invalid_utf8="$(invalid_utf8_key)"
  hmac_mismatch="$(hmac_mismatch_key)"

  # Covered malformed inputs: the non-char-boundary payload, a non-base64
  # string, a short valid-base64 payload, invalid UTF-8 parameters, and an HMAC
  # mismatch after every decoding guard succeeds.
  probe_malformed_key 'non_char_boundary' "$non_char_boundary"
  probe_malformed_key 'not_base64'        'not_base64!!!'
  probe_malformed_key 'short_decoded'     'c2hvcnQ='
  probe_malformed_key 'invalid_utf8'      "$invalid_utf8"
  probe_malformed_key 'hmac_mismatch'      "$hmac_mismatch"

  # After the loop: the served parser must not have panicked at all.
  local panic_count
  panic_count="$(grep -c 'panicked' "$LOG" || true)"
  if [ "$panic_count" -eq 0 ]; then
    pass "server log shows no panic (panic_count=0)"
  else
    fail "server panicked while parsing a malformed credential (panic_count=${panic_count})"
    grep -n 'panicked' "$LOG" >&2 || true
  fi

  # The server process must survive every malformed request.
  local health_status
  set +e
  health_status="$(curl -sS -o /dev/null -w '%{http_code}' "${BASE}/health")"
  set -e
  if [ "$health_status" = "200" ]; then
    pass "GET /health still 200 after malformed credentials"
  else
    fail "GET /health returned ${health_status} (expected 200)"
  fi
}

main() {
  require_tools
  TMP="$(mktemp -d "${TMPDIR:-/tmp}/flapjack_credential_parser_probe.XXXXXX")"
  build_or_resolve_binary
  start_server
  run_probe

  if [ "$CHECKS_FAILED" -ne 0 ]; then
    printf 'credential_parser_http_probe: RED — %s/%s served assertions failed\n' \
      "$CHECKS_FAILED" "$CHECKS_RUN" >&2
    exit 1
  fi
  printf 'credential_parser_http_probe: GREEN — %s served assertions passed\n' "$CHECKS_RUN"
}

main "$@"
