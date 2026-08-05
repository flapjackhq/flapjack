#!/usr/bin/env bash
#
# Contract test for package/ghcr_publish_preflight.
#
# The preflight exists because release.yml creates the public git tag and the
# GitHub Release BEFORE any job touches GHCR (`release` -> `docker_prepare` ->
# `docker_build_*`). A GHCR credential that has expired or lost `write:packages`
# is therefore discovered only after the release is already public, producing a
# half-release: binaries published, container images missing. That is the
# v1.0.9 shape. The preflight proves the credential can push BEFORE the tag
# exists, so the run fails while it is still free to fail.
#
# This test drives the helper against a local fake registry, so it needs no
# network and no real credential. Every case asserts a DISTINCT exit code, so a
# helper that failed for the wrong reason cannot pass, and the success case
# asserts the fake registry actually received both requests, so a helper that
# short-circuits to `exit 0` cannot pass either.
#
# Usage:
#   bash engine/tests/test_ghcr_publish_preflight.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
PREFLIGHT="$REPO_DIR/engine/package/ghcr_publish_preflight"
IMAGE_REPOSITORY="flapjackhq/flapjack"

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
}

section() {
  printf '\n\033[1m%s\033[0m\n' "$1"
}

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/flapjack-ghcr-preflight.XXXXXX")"
SERVER_PID=""

cleanup() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    # Only ever the PID this test itself started.
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

# A fake registry that speaks just enough of the Docker Registry v2 auth flow
# for the preflight: the `/token` handshake and the blob-upload session POST
# that proves push capability. `mode` selects which failure the registry
# presents. Every request is appended to REQUEST_LOG so the success case can
# prove the helper really talked to it.
cat >"$WORK_DIR/fake_registry.py" <<'PY'
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

MODE = sys.argv[1]
PORT_FILE = sys.argv[2]
REQUEST_LOG = sys.argv[3]


def log(line):
    with open(REQUEST_LOG, "a", encoding="utf-8") as handle:
        handle.write(line + "\n")


class Handler(BaseHTTPRequestHandler):
    def _send(self, status, body=b"", headers=None):
        self.send_response(status)
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_GET(self):  # noqa: N802 - BaseHTTPRequestHandler API
        log("GET " + self.path.split("?")[0])
        if not self.path.startswith("/token"):
            self._send(404)
            return
        if MODE == "token_unauthorized":
            self._send(401, json.dumps({"errors": [{"code": "UNAUTHORIZED"}]}).encode())
            return
        if MODE == "token_absent":
            # 200 with no `token` field. GHCR answers this way for a credential
            # it cannot mint a scoped token for, so treating 200 as success
            # would be a false green.
            self._send(200, json.dumps({}).encode())
            return
        self._send(200, json.dumps({"token": "fake-bearer"}).encode())

    def do_POST(self):  # noqa: N802 - BaseHTTPRequestHandler API
        log("POST " + self.path)
        if MODE == "upload_denied":
            self._send(403, json.dumps({"errors": [{"code": "DENIED"}]}).encode())
            return
        self._send(
            202,
            headers={"Location": "/v2/flapjackhq/flapjack/blobs/uploads/fake-session"},
        )

    def log_message(self, *_args):
        return


server = HTTPServer(("127.0.0.1", 0), Handler)
with open(PORT_FILE, "w", encoding="utf-8") as handle:
    handle.write(str(server.server_port))
    handle.flush()
    os.fsync(handle.fileno())
server.serve_forever()
PY

start_fake_registry() {
  local mode="$1"
  local port_file="$WORK_DIR/port"
  REQUEST_LOG="$WORK_DIR/requests.log"
  rm -f "$port_file" "$REQUEST_LOG"
  python3 "$WORK_DIR/fake_registry.py" "$mode" "$port_file" "$REQUEST_LOG" &
  SERVER_PID=$!

  # Wait for the bind to publish its port rather than sleeping a guessed
  # interval; an unbound server would otherwise look like a network failure.
  local waited=0
  while [ ! -s "$port_file" ]; do
    if [ "$waited" -ge 100 ]; then
      printf 'fake registry did not start\n' >&2
      return 1
    fi
    sleep 0.05
    waited=$((waited + 1))
  done
  REGISTRY_URL="http://127.0.0.1:$(cat "$port_file")"
}

stop_fake_registry() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
}

# Runs the preflight and echoes its exit code. Credentials default to non-empty
# so that a case exercising a *missing* credential has to clear it explicitly.
run_preflight() {
  local registry="$1"
  local username="${2-probe-user}"
  local token="${3-probe-token}"
  GHCR_REGISTRY="$registry" \
  GHCR_USERNAME="$username" \
  GHCR_TOKEN="$token" \
    "$PREFLIGHT" "$IMAGE_REPOSITORY" >"$WORK_DIR/out.txt" 2>&1
  echo "$?"
}

assert_exit_code() {
  local expected="$1"
  local actual="$2"
  local description="$3"
  if [ "$actual" = "$expected" ]; then
    pass "$description"
  else
    fail "$description (expected exit $expected, got $actual)"
    sed 's/^/        /' "$WORK_DIR/out.txt" 2>/dev/null | head -5
  fi
}

section "Preflight helper exists and is runnable"
if [ -x "$PREFLIGHT" ]; then
  pass "package/ghcr_publish_preflight is executable"
else
  fail "package/ghcr_publish_preflight is executable"
  printf '\n\033[0;31mHelper missing — remaining cases cannot run\033[0m\n'
  printf '\nResults: %d/%d passed\n' "$TESTS_PASSED" "$TESTS_RUN"
  exit 1
fi

section "Fails closed on absent credentials"
# The repo rule is that a probe must not default to healthy on missing or
# indeterminate state. An empty GHCR_TOKEN is exactly the shape a deleted or
# renamed repository secret takes, and it must be a hard failure, not a skip.
code="$(run_preflight "https://ghcr.io" "probe-user" "")"
assert_exit_code 2 "$code" "empty GHCR_TOKEN fails closed with the credential exit code"

code="$(run_preflight "https://ghcr.io" "" "probe-token")"
assert_exit_code 2 "$code" "empty GHCR_USERNAME fails closed with the credential exit code"

section "Fails on a registry that will not mint a scoped token"
start_fake_registry "token_unauthorized" || exit 1
code="$(run_preflight "$REGISTRY_URL")"
assert_exit_code 3 "$code" "401 from the token endpoint is reported as a token failure"
stop_fake_registry

start_fake_registry "token_absent" || exit 1
code="$(run_preflight "$REGISTRY_URL")"
assert_exit_code 3 "$code" "200 with no token field is a failure, not a false green"
stop_fake_registry

section "Fails when the credential authenticates but cannot push"
# This is the case a plain `docker login` cannot catch: the credential is valid
# and can read, but has lost the write scope the publish jobs need.
start_fake_registry "upload_denied" || exit 1
code="$(run_preflight "$REGISTRY_URL")"
assert_exit_code 4 "$code" "403 on the blob-upload session is reported as a push-capability failure"
stop_fake_registry

section "Fails when the registry is unreachable"
# An indeterminate result must not read as healthy. Port 1 on loopback is
# reserved and refuses connections immediately.
# Asserted as the exact token-stage code, not merely non-zero: "non-zero" would
# also be satisfied by the helper dying for an unrelated reason.
code="$(run_preflight "http://127.0.0.1:1")"
assert_exit_code 3 "$code" "an unreachable registry fails at the token stage, not a pass"

section "Passes only when the registry grants a push session"
start_fake_registry "push_ok" || exit 1
code="$(run_preflight "$REGISTRY_URL")"
assert_exit_code 0 "$code" "a credential granted a 202 upload session passes"

# Guards against a helper that returns success without contacting the registry:
# both legs of the flow must appear in the request log.
if grep -q '^GET /token$' "$REQUEST_LOG" 2>/dev/null; then
  pass "the passing run really performed the token handshake"
else
  fail "the passing run really performed the token handshake"
fi
if grep -q "^POST /v2/$IMAGE_REPOSITORY/blobs/uploads/$" "$REQUEST_LOG" 2>/dev/null; then
  pass "the passing run really probed push against the requested image repository"
else
  fail "the passing run really probed push against the requested image repository"
fi
stop_fake_registry

section "Rejects malformed invocation"
# Usage errors must be exit 1 and must NOT be confusable with a credential or
# registry verdict, so the caller can tell "wired up wrong" from "token is dead".
# Both cases point at an unreachable registry so a helper that skipped argument
# validation would fail with 3, not 1, and be caught here.
code="$(GHCR_REGISTRY="http://127.0.0.1:1" GHCR_USERNAME="u" GHCR_TOKEN="t" "$PREFLIGHT" >/dev/null 2>&1; echo $?)"
assert_exit_code 1 "$code" "missing image repository argument is a usage error"

code="$(GHCR_REGISTRY="http://127.0.0.1:1" GHCR_USERNAME="u" GHCR_TOKEN="t" "$PREFLIGHT" one two >/dev/null 2>&1; echo $?)"
assert_exit_code 1 "$code" "extra arguments are a usage error"

printf '\n\033[1mResults: %d/%d passed\033[0m\n' "$TESTS_PASSED" "$TESTS_RUN"
if [ "$TESTS_FAILED" -gt 0 ]; then
  printf '\033[0;31m%d test(s) failed\033[0m\n' "$TESTS_FAILED"
  exit 1
fi
printf '\033[0;32mAll tests passed\033[0m\n'
