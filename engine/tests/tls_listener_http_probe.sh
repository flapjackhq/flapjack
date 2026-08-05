#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_HELPER="$SCRIPT_DIR/common/wait_for_flapjack.sh"
EVIDENCE_ROOT="$ENGINE_DIR/tests/results/tls_listener_evidence"

readonly ADMIN_KEY="stage3-tls-listener-admin-key"
readonly APP_ID="stage3-tls-listener-app"
readonly REQUIRED_TOOLS="awk bash cargo curl grep head mkdir mktemp openssl sed seq sleep tail tee"

BIN=""
EVIDENCE_DIR=""
TMP_ROOT=""
CERT_PATH=""
KEY_PATH=""
GENERATED_CERT_METADATA=""
GENERATED_CERT_FINGERPRINT=""
TLS_SERVER_PID=""
PLAINTEXT_SERVER_PID=""
STATUS_WRITTEN=0

write_status() {
  local outcome="$1" message="$2"
  STATUS_WRITTEN=1
  if [ -n "$EVIDENCE_DIR" ] && [ -d "$EVIDENCE_DIR" ]; then
    {
      printf 'outcome=%s\n' "$outcome"
      printf 'message=%s\n' "$message"
    } >"$EVIDENCE_DIR/status.txt"
  fi
  printf '%s: %s\n' "$outcome" "$message" >&2
}

die_fail() {
  write_status "FAIL" "$1"
  exit 1
}

die_indeterminate() {
  write_status "INDETERMINATE" "$1"
  exit 2
}

cleanup() {
  local script_exit_code=$?

  if [ "$script_exit_code" -ne 0 ] && [ "$STATUS_WRITTEN" -eq 0 ]; then
    write_status "INDETERMINATE" "script exited before recording final status"
  fi
  sanitize_evidence_paths

  if [ -n "$TLS_SERVER_PID" ] && kill -0 "$TLS_SERVER_PID" 2>/dev/null; then
    kill "$TLS_SERVER_PID" 2>/dev/null || true
    wait "$TLS_SERVER_PID" 2>/dev/null || true
  fi

  if [ -n "$PLAINTEXT_SERVER_PID" ] && kill -0 "$PLAINTEXT_SERVER_PID" 2>/dev/null; then
    kill "$PLAINTEXT_SERVER_PID" 2>/dev/null || true
    wait "$PLAINTEXT_SERVER_PID" 2>/dev/null || true
  fi

  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    rm -rf "$TMP_ROOT" 2>/dev/null || true
  fi

  if [ "$script_exit_code" -ne 0 ] && [ -n "$EVIDENCE_DIR" ]; then
    printf 'INFO: preserved TLS listener probe evidence at %s\n' "$EVIDENCE_DIR" >&2
  fi
}
trap cleanup EXIT INT TERM

create_evidence_dir() {
  if ! mkdir -p "$EVIDENCE_ROOT" 2>/dev/null; then
    printf 'INDETERMINATE: could not create TLS listener evidence root\n' >&2
    exit 2
  fi

  EVIDENCE_DIR="$(mktemp -d "$EVIDENCE_ROOT/run_XXXXXX" 2>/dev/null || true)"
  if [ -z "$EVIDENCE_DIR" ]; then
    EVIDENCE_DIR="$EVIDENCE_ROOT/run_$$"
    mkdir -p "$EVIDENCE_DIR" 2>/dev/null || {
      printf 'INDETERMINATE: could not create TLS listener evidence directory\n' >&2
      exit 2
    }
  fi
}

require_tools() {
  local missing="" tool
  for tool in $REQUIRED_TOOLS env mv rm; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      missing="${missing} ${tool}"
    fi
  done
  [ -z "$missing" ] || die_indeterminate "missing required tools:${missing}"
  [ -x "$WAIT_HELPER" ] || die_indeterminate "readiness helper is not executable"
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

build_or_resolve_binary() {
  local build_log="$EVIDENCE_DIR/cargo-build-full.log"

  if [ -n "${FLAPJACK_BIN:-}" ]; then
    [ -x "$FLAPJACK_BIN" ] || die_indeterminate "FLAPJACK_BIN is not executable"
    BIN="$FLAPJACK_BIN"
    printf 'FLAPJACK_BIN override used; cargo build skipped.\n' >"$build_log"
    return
  fi

  # Required display shape: cd engine && cargo build -p flapjack-server 2>&1 | tail -30
  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server 2>&1 | tee "$build_log" | tail -30); then
    die_indeterminate "cargo build -p flapjack-server failed"
  fi

  BIN="$(target_dir)/debug/flapjack"
  [ -x "$BIN" ] || die_indeterminate "debug flapjack binary is missing after build"
}

create_tls_material() {
  TMP_ROOT="$(mktemp -d)"
  local cert_dir="$TMP_ROOT/tls"
  mkdir -p "$cert_dir"
  CERT_PATH="$cert_dir/server.crt"
  KEY_PATH="$cert_dir/server.key"
  local openssl_log="$EVIDENCE_DIR/openssl-generate-cert.log"

  if ! openssl req -x509 -newkey rsa:2048 -sha256 -days 1 -nodes -keyout "$KEY_PATH" -out "$CERT_PATH" -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" >"$openssl_log" 2>&1; then
    die_indeterminate "failed to generate self-signed TLS certificate"
  fi

  GENERATED_CERT_METADATA="$EVIDENCE_DIR/generated-certificate-metadata.txt"
  write_certificate_metadata "$CERT_PATH" "$GENERATED_CERT_METADATA" "issuer" \
    || die_indeterminate "failed to read generated certificate metadata"
  GENERATED_CERT_FINGERPRINT="$(
    openssl x509 -in "$CERT_PATH" -noout -fingerprint -sha256 \
      | sed -n 's/^.*Fingerprint=//p' \
      | head -1
  )"
  [ -n "$GENERATED_CERT_FINGERPRINT" ] || die_indeterminate "generated certificate fingerprint was empty"
  grep -q 'DNS:localhost' "$GENERATED_CERT_METADATA" || die_indeterminate "generated certificate lacks localhost SAN"
  grep -q 'IP Address:127.0.0.1' "$GENERATED_CERT_METADATA" || die_indeterminate "generated certificate lacks 127.0.0.1 SAN"
}

write_certificate_metadata() {
  local cert_path="$1" output_path="$2" issuer_mode="${3:-}"

  if [ "$issuer_mode" = "issuer" ]; then
    openssl x509 -in "$cert_path" -noout -subject -issuer -fingerprint -sha256 \
      >"$output_path" 2>&1 || return 1
  else
    openssl x509 -in "$cert_path" -noout -subject -fingerprint -sha256 \
      >"$output_path" 2>&1 || return 1
  fi

  openssl x509 -in "$cert_path" -noout -text 2>&1 \
    | sed -n '/Subject Alternative Name/,+1p' >>"$output_path" || return 1
}

start_tls_server() {
  local data_dir="$TMP_ROOT/tls-data"
  local log_path="$EVIDENCE_DIR/tls-server.log"
  mkdir -p "$data_dir"

  env \
    -u FLAPJACK_NO_AUTH \
    -u FLAPJACK_SSL_CERT_PATH \
    -u FLAPJACK_SSL_KEY_PATH \
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$data_dir" \
    FLAPJACK_DISABLE_DASHBOARD=1 \
    "$BIN" --auto-port --ssl-cert-path "$CERT_PATH" --ssl-key-path "$KEY_PATH" \
    >"$log_path" 2>&1 &
  TLS_SERVER_PID=$!
}

start_plaintext_server() {
  local data_dir="$TMP_ROOT/plaintext-data"
  local log_path="$EVIDENCE_DIR/plaintext-server.log"
  mkdir -p "$data_dir"

  env \
    -u FLAPJACK_NO_AUTH \
    -u FLAPJACK_SSL_CERT_PATH \
    -u FLAPJACK_SSL_KEY_PATH \
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$data_dir" \
    FLAPJACK_DISABLE_DASHBOARD=1 \
    "$BIN" --auto-port >"$log_path" 2>&1 &
  PLAINTEXT_SERVER_PID=$!
}

wait_for_startup_port() {
  local pid="$1" log_path="$2" scheme="$3" label="$4" port
  for _i in $(seq 1 80); do
    if ! kill -0 "$pid" 2>/dev/null; then
      sed -n '1,160p' "$log_path" >"$EVIDENCE_DIR/${label}-startup-failure.log" 2>/dev/null || true
      return 1
    fi

    port="$(sed -n -E "s/.*Local:[[:space:]]+${scheme}:\\/\\/127\\.0\\.0\\.1:([0-9]+).*/\\1/p" "$log_path" | head -1)"
    if [ -n "$port" ]; then
      printf '%s\n' "$port"
      return 0
    fi
    sleep 0.25
  done
  return 1
}

sanitize_evidence_paths() {
  [ -n "$EVIDENCE_DIR" ] && [ -d "$EVIDENCE_DIR" ] || return 0
  command -v sed >/dev/null 2>&1 || return 0
  command -v mv >/dev/null 2>&1 || return 0

  local repo_root file tmp
  repo_root="$(cd "$ENGINE_DIR/.." && pwd)"
  for file in "$EVIDENCE_DIR"/*; do
    [ -f "$file" ] || continue
    tmp="${file}.sanitized"
    if [ -n "$TMP_ROOT" ]; then
      sed \
        -e "s|$ENGINE_DIR|<engine-dir>|g" \
        -e "s|$repo_root|<repo-root>|g" \
        -e "s|$TMP_ROOT|<tmp-root>|g" \
        "$file"
    else
      sed \
        -e "s|$ENGINE_DIR|<engine-dir>|g" \
        -e "s|$repo_root|<repo-root>|g" \
        "$file"
    fi \
      | awk '
          {
            sub(/\r$/, "")
            sub(/[[:space:]]+$/, "")
            lines[NR] = $0
          }
          END {
            n = NR
            while (n > 0 && lines[n] == "") {
              n--
            }
            for (i = 1; i <= n; i++) {
              print lines[i]
            }
          }
        ' >"$tmp" && mv "$tmp" "$file"
  done
}

curl_capture() {
  local output_prefix="$1"
  shift
  local status_file="${output_prefix}-status.txt"
  local body_file="${output_prefix}-body.json"
  local header_file="${output_prefix}-headers.txt"
  local error_file="${output_prefix}-curl-stderr.txt"
  local http_status curl_exit

  set +e
  http_status="$(curl "$@" -sS -D "$header_file" -o "$body_file" -w '%{http_code}' 2>"$error_file")"
  curl_exit=$?
  set -e
  printf '%s\n' "$http_status" >"$status_file"
  printf '%s\n' "$curl_exit"
}

wait_for_https_health() {
  local tls_port="$1"
  local output_prefix="$EVIDENCE_DIR/tls-health"
  local curl_exit http_status

  for _i in $(seq 1 80); do
    kill -0 "$TLS_SERVER_PID" 2>/dev/null || die_fail "TLS server exited before HTTPS readiness"
    curl_exit="$(curl_capture "$output_prefix" --cacert "$CERT_PATH" "https://127.0.0.1:${tls_port}/health")"
    http_status="$(tail -1 "${output_prefix}-status.txt")"
    if [ "$curl_exit" = "0" ] && [ "$http_status" = "200" ] && grep -q '"status":"ok"' "${output_prefix}-body.json"; then
      return 0
    fi
    sleep 0.25
  done

  die_fail "verified HTTPS /health did not return HTTP 200 with status ok"
}

assert_tls_peer_certificate_matches() {
  local tls_port="$1"
  local transcript="$EVIDENCE_DIR/tls-openssl-s-client.txt"
  local peer_cert="$EVIDENCE_DIR/tls-served-peer.pem"
  local peer_metadata="$EVIDENCE_DIR/tls-served-peer-metadata.txt"
  local peer_fingerprint

  set +e
  openssl s_client -connect "127.0.0.1:${tls_port}" -servername localhost -showcerts </dev/null \
    >"$transcript" 2>&1
  set -e

  awk '
    /-----BEGIN CERTIFICATE-----/ { capture = 1 }
    capture { print }
    /-----END CERTIFICATE-----/ { exit }
  ' "$transcript" >"$peer_cert"
  [ -s "$peer_cert" ] || die_fail "openssl s_client did not return a served certificate"

  write_certificate_metadata "$peer_cert" "$peer_metadata" \
    || die_fail "served certificate could not be parsed"
  peer_fingerprint="$(
    openssl x509 -in "$peer_cert" -noout -fingerprint -sha256 \
      | sed -n 's/^.*Fingerprint=//p' \
      | head -1
  )"

  [ "$peer_fingerprint" = "$GENERATED_CERT_FINGERPRINT" ] || die_fail "served certificate fingerprint did not match generated certificate"
  grep -Eq 'subject=.*CN ?= ?localhost' "$peer_metadata" || die_fail "served certificate subject did not match localhost"
  grep -q 'DNS:localhost' "$peer_metadata" || die_fail "served certificate lacks localhost SAN"
  grep -q 'IP Address:127.0.0.1' "$peer_metadata" || die_fail "served certificate lacks 127.0.0.1 SAN"
}

assert_https_api() {
  local tls_port="$1"
  local output_prefix="$EVIDENCE_DIR/tls-list-indexes"
  local curl_exit http_status

  curl_exit="$(curl_capture "$output_prefix" --cacert "$CERT_PATH" \
    -H "X-Algolia-Application-ID: $APP_ID" \
    -H "X-Algolia-API-Key: $ADMIN_KEY" \
    "https://127.0.0.1:${tls_port}/1/indexes")"
  http_status="$(tail -1 "${output_prefix}-status.txt")"

  [ "$curl_exit" = "0" ] || die_fail "HTTPS /1/indexes curl exited ${curl_exit}"
  [ "$http_status" = "200" ] || die_fail "HTTPS /1/indexes returned HTTP ${http_status}"
  grep -Eq '"items":[[]' "${output_prefix}-body.json" || die_fail "HTTPS /1/indexes did not return JSON items array"
}

assert_tls_rejects_plaintext() {
  local tls_port="$1"
  local output_prefix="$EVIDENCE_DIR/tls-plaintext-rejection"
  local curl_exit http_status

  curl_exit="$(curl_capture "$output_prefix" --max-time 2 "http://127.0.0.1:${tls_port}/health")"
  http_status="$(tail -1 "${output_prefix}-status.txt")"
  if [ "$curl_exit" = "0" ] && [ "$http_status" = "200" ]; then
    die_fail "TLS listener accepted plaintext HTTP /health with status 200"
  fi
}

stop_tls_server() {
  if [ -n "$TLS_SERVER_PID" ] && kill -0 "$TLS_SERVER_PID" 2>/dev/null; then
    kill "$TLS_SERVER_PID" 2>/dev/null || true
    wait "$TLS_SERVER_PID" 2>/dev/null || true
  fi
  TLS_SERVER_PID=""
}

wait_for_plaintext_readiness() {
  local log_path="$EVIDENCE_DIR/plaintext-server.log"
  if ! "$WAIT_HELPER" --pid "$PLAINTEXT_SERVER_PID" --host 127.0.0.1 --port auto --log-path "$log_path" --retries 80 --interval-seconds 0.25 \
    >"$EVIDENCE_DIR/plaintext-wait-helper.log" 2>&1; then
    die_fail "plaintext server did not become ready through wait_for_flapjack.sh"
  fi
}

assert_plaintext_rejects_tls() {
  local plaintext_port="$1"
  local transcript="$EVIDENCE_DIR/plaintext-openssl-s-client.txt"

  set +e
  openssl s_client -connect "127.0.0.1:${plaintext_port}" </dev/null >"$transcript" 2>&1
  local openssl_exit=$?
  set -e

  kill -0 "$PLAINTEXT_SERVER_PID" 2>/dev/null || die_fail "plaintext server exited after TLS probe"
  if [ "$openssl_exit" -eq 0 ] && grep -q 'BEGIN CERTIFICATE' "$transcript"; then
    die_fail "plaintext listener unexpectedly completed TLS handshake"
  fi
}

assert_plaintext_api() {
  local plaintext_port="$1"
  local health_prefix="$EVIDENCE_DIR/plaintext-health"
  local indexes_prefix="$EVIDENCE_DIR/plaintext-list-indexes"
  local curl_exit http_status

  curl_exit="$(curl_capture "$health_prefix" "http://127.0.0.1:${plaintext_port}/health")"
  http_status="$(tail -1 "${health_prefix}-status.txt")"
  [ "$curl_exit" = "0" ] || die_fail "plaintext /health curl exited ${curl_exit}"
  [ "$http_status" = "200" ] || die_fail "plaintext /health returned HTTP ${http_status}"
  grep -q '"status":"ok"' "${health_prefix}-body.json" || die_fail "plaintext /health did not return status ok"

  curl_exit="$(curl_capture "$indexes_prefix" \
    -H "X-Algolia-Application-ID: $APP_ID" \
    -H "X-Algolia-API-Key: $ADMIN_KEY" \
    "http://127.0.0.1:${plaintext_port}/1/indexes")"
  http_status="$(tail -1 "${indexes_prefix}-status.txt")"
  [ "$curl_exit" = "0" ] || die_fail "plaintext /1/indexes curl exited ${curl_exit}"
  [ "$http_status" = "200" ] || die_fail "plaintext /1/indexes returned HTTP ${http_status}"
  grep -Eq '"items":[[]' "${indexes_prefix}-body.json" || die_fail "plaintext /1/indexes did not return JSON items array"
}

main() {
  create_evidence_dir
  require_tools
  build_or_resolve_binary
  create_tls_material

  start_tls_server
  local tls_port
  if ! tls_port="$(wait_for_startup_port "$TLS_SERVER_PID" "$EVIDENCE_DIR/tls-server.log" "https" "TLS")"; then
    die_fail "TLS server did not print Local: https://127.0.0.1:<port>"
  fi
  wait_for_https_health "$tls_port"
  assert_tls_peer_certificate_matches "$tls_port"
  assert_https_api "$tls_port"
  assert_tls_rejects_plaintext "$tls_port"
  stop_tls_server

  start_plaintext_server
  wait_for_plaintext_readiness
  local plaintext_port
  if ! plaintext_port="$(wait_for_startup_port "$PLAINTEXT_SERVER_PID" "$EVIDENCE_DIR/plaintext-server.log" "http" "plaintext")"; then
    die_fail "plaintext server did not print Local: http://127.0.0.1:<port>"
  fi
  assert_plaintext_rejects_tls "$plaintext_port"
  assert_plaintext_api "$plaintext_port"

  write_status "PASS" "TLS listener served-boundary probe passed"
}

main "$@"
