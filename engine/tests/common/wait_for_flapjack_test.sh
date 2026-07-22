#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
HELPER_PATH="$REPO_ROOT/engine/tests/common/wait_for_flapjack.sh"
SMOKE_PATH="$REPO_ROOT/engine/tests/integration_smoke.sh"
CI_PATH="$REPO_ROOT/.github/workflows/ci.yml"

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

pick_free_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

assert_helper_file_contract() {
  if [ ! -f "$HELPER_PATH" ]; then
    fail 'wait_for_flapjack helper file exists' "$HELPER_PATH"
    return
  fi
  pass 'wait_for_flapjack helper file exists'

  if [ -x "$HELPER_PATH" ]; then
    pass 'wait_for_flapjack helper is executable'
  else
    fail 'wait_for_flapjack helper is executable'
  fi

  local first_line
  first_line="$(head -n 1 "$HELPER_PATH")"
  if [ "$first_line" = '#!/usr/bin/env bash' ]; then
    pass 'wait_for_flapjack helper has bash shebang'
  else
    fail 'wait_for_flapjack helper has bash shebang' "$first_line"
  fi

  if grep -q '^set -euo pipefail$' "$HELPER_PATH"; then
    pass 'wait_for_flapjack helper enables strict mode'
  else
    fail 'wait_for_flapjack helper enables strict mode'
  fi
}

assert_helper_runtime_success() {
  local work_dir server_log port server_pid
  work_dir="$(mktemp -d)"
  server_log="$work_dir/server.log"
  port="$(pick_free_port)"
  python3 - "$port" >"$server_log" 2>&1 <<'PY' &
import http.server
import socketserver
import sys

PORT = int(sys.argv[1])

class HealthHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"status":"ok"}')
            return
        self.send_response(404)
        self.end_headers()

    def log_message(self, format, *args):
        return

with socketserver.TCPServer(("127.0.0.1", PORT), HealthHandler) as server:
    server.serve_forever()
PY
  server_pid=$!

  if "$HELPER_PATH" --pid "$server_pid" --health-url "http://127.0.0.1:${port}/health" --log-path "$server_log" --retries 30 --interval-seconds 0.1 >/dev/null 2>&1; then
    pass 'wait_for_flapjack helper succeeds when health endpoint is reachable'
  else
    fail 'wait_for_flapjack helper succeeds when health endpoint is reachable'
  fi

  kill "$server_pid" 2>/dev/null || true
  rm -rf "$work_dir"
}

assert_helper_runtime_auto_port() {
  local work_dir server_log port server_pid
  work_dir="$(mktemp -d)"
  server_log="$work_dir/server.log"
  port="$(pick_free_port)"
  printf '  ->  Local:      http://127.0.0.1:%s\n' "$port" >"$server_log"
  python3 - "$port" >>"$server_log" 2>&1 <<'PY' &
import http.server
import socketserver
import sys

PORT = int(sys.argv[1])

class HealthHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")
            return
        self.send_response(404)
        self.end_headers()

    def log_message(self, format, *args):
        return

with socketserver.TCPServer(("127.0.0.1", PORT), HealthHandler) as server:
    server.serve_forever()
PY
  server_pid=$!

  if "$HELPER_PATH" --pid "$server_pid" --host 127.0.0.1 --port auto --log-path "$server_log" --retries 30 --interval-seconds 0.1 >/dev/null 2>&1; then
    pass 'wait_for_flapjack helper resolves auto-port from startup log'
  else
    fail 'wait_for_flapjack helper resolves auto-port from startup log'
  fi

  kill "$server_pid" 2>/dev/null || true
  rm -rf "$work_dir"
}

assert_helper_runtime_auto_port_non_loopback_banner() {
  local work_dir server_log port server_pid
  work_dir="$(mktemp -d)"
  server_log="$work_dir/server.log"
  port="$(pick_free_port)"
  printf '  ->  Local:      http://0.0.0.0:%s\n' "$port" >"$server_log"
  python3 - "$port" >>"$server_log" 2>&1 <<'PY' &
import http.server
import socketserver
import sys

PORT = int(sys.argv[1])

class HealthHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")
            return
        self.send_response(404)
        self.end_headers()

    def log_message(self, format, *args):
        return

with socketserver.TCPServer(("127.0.0.1", PORT), HealthHandler) as server:
    server.serve_forever()
PY
  server_pid=$!

  if "$HELPER_PATH" --pid "$server_pid" --host 127.0.0.1 --port auto --log-path "$server_log" --retries 30 --interval-seconds 0.1 >/dev/null 2>&1; then
    pass 'wait_for_flapjack helper resolves auto-port from non-loopback startup log'
  else
    fail 'wait_for_flapjack helper resolves auto-port from non-loopback startup log'
  fi

  kill "$server_pid" 2>/dev/null || true
  rm -rf "$work_dir"
}

assert_helper_rejects_non_200_health_status() {
  local work_dir server_log output_file port server_pid
  work_dir="$(mktemp -d)"
  server_log="$work_dir/server.log"
  output_file="$work_dir/output.txt"
  port="$(pick_free_port)"
  python3 - "$port" >"$server_log" 2>&1 <<'PY' &
import http.server
import socketserver
import sys

PORT = int(sys.argv[1])

class HealthHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self.send_response(204)
            self.end_headers()
            return
        self.send_response(404)
        self.end_headers()

    def log_message(self, format, *args):
        return

with socketserver.TCPServer(("127.0.0.1", PORT), HealthHandler) as server:
    server.serve_forever()
PY
  server_pid=$!

  if "$HELPER_PATH" --pid "$server_pid" --health-url "http://127.0.0.1:${port}/health" --log-path "$server_log" --retries 2 --interval-seconds 0.1 >"$output_file" 2>&1; then
    fail 'wait_for_flapjack helper rejects non-200 health status'
  else
    pass 'wait_for_flapjack helper rejects non-200 health status'
  fi

  kill "$server_pid" 2>/dev/null || true
  rm -rf "$work_dir"
}

assert_helper_runtime_process_exit() {
  local work_dir server_log output_file dead_pid
  work_dir="$(mktemp -d)"
  server_log="$work_dir/server.log"
  output_file="$work_dir/output.txt"
  printf 'process-exit-log\n' >"$server_log"
  sleep 0.1 &
  dead_pid=$!
  wait "$dead_pid"

  if "$HELPER_PATH" --pid "$dead_pid" --health-url "http://127.0.0.1:9/health" --log-path "$server_log" --retries 5 --interval-seconds 0.1 >"$output_file" 2>&1; then
    fail 'wait_for_flapjack helper fails fast when server process exits'
  else
    if grep -q 'exited before becoming ready' "$output_file"; then
      pass 'wait_for_flapjack helper fails fast when server process exits'
    else
      fail 'wait_for_flapjack helper reports process-exit failure message' "$(cat "$output_file")"
    fi
  fi

  rm -rf "$work_dir"
}

assert_helper_runtime_timeout_logs() {
  local work_dir server_log output_file sleeper_pid
  work_dir="$(mktemp -d)"
  server_log="$work_dir/server.log"
  output_file="$work_dir/output.txt"
  printf 'timeout-log-marker\n' >"$server_log"
  sleep 30 &
  sleeper_pid=$!

  if "$HELPER_PATH" --pid "$sleeper_pid" --health-url "http://127.0.0.1:9/health" --log-path "$server_log" --retries 2 --interval-seconds 0.1 >"$output_file" 2>&1; then
    fail 'wait_for_flapjack helper times out when health endpoint stays unavailable'
  else
    if grep -q 'did not become ready' "$output_file" && grep -q 'timeout-log-marker' "$output_file"; then
      pass 'wait_for_flapjack helper times out with server log output'
    else
      fail 'wait_for_flapjack helper timeout includes log output' "$(cat "$output_file")"
    fi
  fi

  kill "$sleeper_pid" 2>/dev/null || true
  rm -rf "$work_dir"
}

assert_smoke_and_ci_delegate_to_helper() {
  if grep -q 'engine/tests/common/wait_for_flapjack.sh' "$SMOKE_PATH"; then
    pass 'integration_smoke start_server delegates to shared helper'
  else
    fail 'integration_smoke start_server delegates to shared helper'
  fi

  local helper_count
  helper_count="$(grep -c 'engine/tests/common/wait_for_flapjack.sh' "$CI_PATH" || true)"
  if [ "$helper_count" -gt 0 ]; then
    pass 'CI uses shared helper readiness calls'
  else
    fail 'CI uses shared helper readiness calls' "found $helper_count"
  fi

  if grep -q 'sleep 3' "$CI_PATH"; then
    fail 'CI no longer uses fixed sleep 3 for server readiness'
  else
    pass 'CI no longer uses fixed sleep 3 for server readiness'
  fi

  if grep -q 'curl -sf http://localhost:7700/health ||' "$CI_PATH"; then
    fail 'CI no longer uses one-shot curl readiness check'
  else
    pass 'CI no longer uses one-shot curl readiness check'
  fi
}

main() {
  echo 'wait_for_flapjack shared readiness test'
  assert_helper_file_contract
  assert_helper_runtime_success
  assert_helper_runtime_auto_port
  assert_helper_runtime_auto_port_non_loopback_banner
  assert_helper_rejects_non_200_health_status
  assert_helper_runtime_process_exit
  assert_helper_runtime_timeout_logs
  assert_smoke_and_ci_delegate_to_helper

  printf '\nResults: %d/%d passed\n' "$TESTS_PASSED" "$TESTS_RUN"
  if [ "$TESTS_FAILED" -gt 0 ]; then
    printf '%d test(s) failed\n' "$TESTS_FAILED"
    return 1
  fi
  echo 'All tests passed'
}

main "$@"
