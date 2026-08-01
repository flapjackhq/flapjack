#!/usr/bin/env bash

set -euo pipefail

SCRIPT_PATH="${BASH_SOURCE[0]}"
if [[ "$SCRIPT_PATH" == */* ]]; then
  SCRIPT_PARENT="${SCRIPT_PATH%/*}"
else
  SCRIPT_PARENT="."
fi
SCRIPT_DIR="$(cd "$SCRIPT_PARENT" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DASHBOARD_DIR="$ENGINE_DIR/dashboard"
WAIT_FOR_FLAPJACK="$ENGINE_DIR/tests/common/wait_for_flapjack.sh"
EXPECTED_CSP="default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"

BIN=""
TMP_ROOT=""
SERVER_PID=""
BASE=""
PLAYWRIGHT_STATUS=1
INDETERMINATE=0
ADMIN_KEY="csp-dashboard-browser-probe-admin-key"
REQUIRED_TOOLS=(awk cargo cat curl env grep head ls mkdir mktemp npm npx rm sed tail)

mark_indeterminate() {
  INDETERMINATE=1
  printf 'INDETERMINATE %s expected=%s actual=%s\n' "$1" "$2" "$3" >&2
}

required_tools() {
  local missing=0 tool
  for tool in "${REQUIRED_TOOLS[@]}"; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'INDETERMINATE required_tool expected=present actual=missing:%s\n' "$tool" >&2
      missing=1
    fi
  done
  if [ "$missing" -ne 0 ]; then
    INDETERMINATE=1
    exit 1
  fi
  if [ ! -x "$WAIT_FOR_FLAPJACK" ]; then
    mark_indeterminate wait_helper executable "$WAIT_FOR_FLAPJACK"
    exit 1
  fi
}

required_tools_self_test() {
  local expected_tool declared_tool found
  local expected_tools=(awk cargo cat curl env grep head ls mkdir mktemp npm npx rm sed tail)

  for expected_tool in "${expected_tools[@]}"; do
    found=0
    for declared_tool in "${REQUIRED_TOOLS[@]}"; do
      if [ "$declared_tool" = "$expected_tool" ]; then
        found=1
        break
      fi
    done
    if [ "$found" -eq 0 ]; then
      printf '[FAIL] required tool is not declared: %s\n' "$expected_tool" >&2
      return 1
    fi
  done

  printf '[PASS] all external tools are declared\n'
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

stop_server() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
  BASE=""
}

cleanup() {
  local script_exit_code=$?
  stop_server

  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    if [ "$INDETERMINATE" -ne 0 ] || [ "$script_exit_code" -ne 0 ]; then
      printf 'INFO: preserved CSP dashboard browser evidence at %s\n' "$TMP_ROOT" >&2
    else
      rm -rf "$TMP_ROOT"
    fi
  fi
}
trap cleanup EXIT

require_dashboard_assets() {
  local asset
  asset="$(ls "$DASHBOARD_DIR"/dist/assets/* 2>/dev/null | head -1 || true)"
  if [ -z "$asset" ] || [ ! -f "$asset" ]; then
    mark_indeterminate dashboard_assets "real file under dashboard/dist/assets" missing
    exit 1
  fi
}

build_dashboard_and_binary() {
  local npm_ci_log="$TMP_ROOT/npm_ci.log"
  local dashboard_build_log="$TMP_ROOT/dashboard_build.log"
  local cargo_build_log="$TMP_ROOT/cargo_build.log"

  if ! (cd "$DASHBOARD_DIR" && npm ci >"$npm_ci_log" 2>&1); then
    tail -40 "$npm_ci_log" >&2 || true
    mark_indeterminate npm_ci "npm ci succeeds" failed
    exit 1
  fi
  if ! (cd "$DASHBOARD_DIR" && npm run build >"$dashboard_build_log" 2>&1); then
    tail -40 "$dashboard_build_log" >&2 || true
    mark_indeterminate dashboard_build "npm run build succeeds" failed
    exit 1
  fi
  require_dashboard_assets

  if ! (cd "$ENGINE_DIR" && FLAPJACK_REQUIRE_DASHBOARD=1 cargo build -p flapjack-server >"$cargo_build_log" 2>&1); then
    tail -40 "$cargo_build_log" >&2 || true
    mark_indeterminate cargo_build "FLAPJACK_REQUIRE_DASHBOARD=1 cargo build -p flapjack-server succeeds" failed
    exit 1
  fi

  BIN="$(target_dir)/debug/flapjack"
  if [ ! -x "$BIN" ]; then
    mark_indeterminate binary "current checkout binary" "$BIN"
    exit 1
  fi
}

start_server() {
  local data_dir="$TMP_ROOT/data"
  local log_path="$TMP_ROOT/server.log"
  mkdir -p "$data_dir"

  env \
    -u FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND \
    -u FLAPJACK_BIND_ADDR \
    -u FLAPJACK_CONTENT_SECURITY_POLICY \
    -u FLAPJACK_DISABLE_DASHBOARD \
    -u FLAPJACK_ENV \
    -u FLAPJACK_NO_AUTH \
    -u FLAPJACK_NODE_ID \
    -u FLAPJACK_PEERS \
    -u FLAPJACK_PORT \
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$data_dir" \
    "$BIN" --auto-port >"$log_path" 2>&1 &
  SERVER_PID=$!

  if ! "$WAIT_FOR_FLAPJACK" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$log_path" \
    --retries 80 \
    --interval-seconds 0.5; then
    mark_indeterminate readiness "healthy loopback auto-port server" failed
    exit 1
  fi

  if [ ! -s "$log_path" ]; then
    mark_indeterminate server_log non_empty empty
    exit 1
  fi

  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9][0-9]*\).*/\1/p' "$log_path" | head -1)"
  if [ -z "$port" ]; then
    mark_indeterminate auto_port "Local: http://127.0.0.1:<port>" missing
    exit 1
  fi

  BASE="http://127.0.0.1:${port}"
  if [ -z "$BASE" ]; then
    mark_indeterminate base_url non_empty empty
    exit 1
  fi
  if [ -z "$ADMIN_KEY" ]; then
    mark_indeterminate admin_key non_empty empty
    exit 1
  fi
  printf 'INFO: browser probing %s\n' "$BASE"
}

wire_preflight() {
  local body="$TMP_ROOT/dashboard.html" headers="$TMP_ROOT/dashboard.headers"
  local http_code csp
  if ! http_code="$(curl -sS -D "$headers" -o "$body" -w '%{http_code}' "$BASE/dashboard/")"; then
    mark_indeterminate dashboard_curl "transport success" failed
    exit 1
  fi
  if [ "$http_code" != "200" ]; then
    mark_indeterminate dashboard_status 200 "$http_code"
    exit 1
  fi
  if grep -Fq 'Dashboard assets are unavailable' "$body"; then
    mark_indeterminate dashboard_assets "real embedded dashboard" "fallback dashboard"
    exit 1
  fi
  if ! grep -Fq '/assets/' "$body"; then
    mark_indeterminate dashboard_script_reference "/assets/ script reference" missing
    exit 1
  fi
  csp="$(awk -F':' 'tolower($1) == "content-security-policy" { value = substr($0, index($0, ":") + 1); sub(/^[[:space:]]+/, "", value); sub(/\r$/, "", value); print value; exit }' "$headers")"
  if [ "$csp" != "$EXPECTED_CSP" ]; then
    mark_indeterminate dashboard_csp "$EXPECTED_CSP" "${csp:-missing}"
    exit 1
  fi
}

resolve_chromium() {
  local install_log="$TMP_ROOT/playwright_install.log"
  if ! (cd "$DASHBOARD_DIR" && npx playwright install chromium >"$install_log" 2>&1); then
    tail -40 "$install_log" >&2 || true
    mark_indeterminate chromium "npx playwright install chromium succeeds" failed
    exit 1
  fi
}

run_playwright() {
  local playwright_log="$TMP_ROOT/playwright.log"
  set +e
  (
    cd "$DASHBOARD_DIR" &&
      FJ_BINARY_BASE_URL="$BASE" \
      FJ_BINARY_ADMIN_KEY="$ADMIN_KEY" \
      npx playwright test --config playwright.binary-csp.config.ts
  ) >"$playwright_log" 2>&1
  PLAYWRIGHT_STATUS=$?
  set -e
  cat "$playwright_log"
  return "$PLAYWRIGHT_STATUS"
}

main() {
  if [ "${1:-}" = "--self-test" ]; then
    required_tools_self_test
    return
  fi

  required_tools
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fj_csp_dashboard_browser.XXXXXX")"
  build_dashboard_and_binary
  resolve_chromium
  start_server
  wire_preflight
  if run_playwright; then
    PLAYWRIGHT_STATUS=0
  else
    PLAYWRIGHT_STATUS=$?
  fi
  stop_server

  if [ "$INDETERMINATE" -ne 0 ]; then
    exit 1
  fi
  exit "$PLAYWRIGHT_STATUS"
}

main "$@"
