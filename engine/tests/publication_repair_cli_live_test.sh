#!/usr/bin/env bash
# Focused contract tests for publication_repair_cli_live.sh runner validation.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$ENGINE_DIR/.." && pwd)"
RUNNER="$SCRIPT_DIR/publication_repair_cli_live.sh"
TMP_ROOT=""

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    rm -rf "$TMP_ROOT"
  fi
}
trap cleanup EXIT

make_workspace() {
  TMP_ROOT="$(mktemp -d /tmp/flapjack_publication_runner_test.XXXXXX)"
  TMP_ROOT="$(cd "$TMP_ROOT" && pwd -P)"
  mkdir -p "$TMP_ROOT/bin" "$TMP_ROOT/artifacts"
}

reset_artifacts() {
  rm -rf "$TMP_ROOT/artifacts"
  mkdir -p "$TMP_ROOT/artifacts"
}

write_manifest() {
  local path="$1"
  local layout_count="${2:-1}"
  local disposition="${3:-absent-create}"
  python3 - "$path" "$layout_count" "$disposition" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
layout_count = int(sys.argv[2])
disposition = sys.argv[3]
if disposition == "quarantine":
    cli = {"status": "quarantined", "action": "quarantine", "exit_code": 2}
    residue = {
        "staging": "absent",
        "backup": "absent",
        "journal": "absent",
        "quarantine": "present",
    }
else:
    cli = {"status": "clean", "action": "none", "exit_code": 0}
    residue = {
        "staging": "absent",
        "backup": "absent",
        "journal": "absent",
        "quarantine": "absent",
    }
manifest = {
    "schema_version": 1,
    "layout_count": layout_count,
    "scenarios": [{
        "id": "case_clean",
        "kind": "base",
        "activation": "create",
        "tenant": "products",
        "transaction": "txn_001",
        "journal_phase": "absent",
        "boundary": {"identity": "create|sync_dir:.publication/products/txn_001/staging|1"},
        "policy_keys": [],
        "digests": {
            "old": "absent",
            "new": "absent",
            "target": "absent",
            "staging": "absent",
            "backup": "absent",
        },
        "sidecars": {
            "query_suggestions": {
                "old": "absent",
                "new": "absent",
                "target": "absent",
                "staging": "absent",
                "backup": "absent",
            },
            "analytics": {
                "old": "absent",
                "new": "absent",
                "target": "absent",
                "staging": "absent",
                "backup": "absent",
            },
        },
        "disposition": disposition,
        "cli": cli,
        "visible": {"target": "absent", "object": "absent", "search": "unavailable"},
        "residue": residue,
    }],
}
path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
PY
}

write_fake_binary() {
  local path="$1"
  local revision="$2"
  local mode="${3:-ok}"
  local default_server_mode="${4:-${FAKE_SERVER_MODE:-healthy}}"
  cat >"$path" <<SH
#!/usr/bin/env bash
set -euo pipefail

revision="\${FAKE_BUILD_REVISION:-$revision}"
mode="\${FAKE_BINARY_MODE:-$mode}"
server_mode="\${FAKE_SERVER_MODE:-$default_server_mode}"
event_log="\${FAKE_EVENT_LOG:-$TMP_ROOT/events.log}"
SH
  cat >>"$path" <<'SH'

log_event() {
  printf '%s\n' "$*" >>"$event_log"
}

if [ "$#" -eq 2 ] && [ "$1" = "build-info" ] && [ "$2" = "--json" ]; then
  python3 - "$revision" <<'PY'
import json
import sys
print(json.dumps({
    "schemaVersion": 1,
    "revision": sys.argv[1],
    "revisionKnown": True,
    "dirty": None,
    "dirtyKnown": False,
    "features": [],
    "capabilities": {"vectorSearch": False, "vectorSearchLocal": False},
}, separators=(",", ":")))
PY
  exit 0
fi

if [ "$mode" = "malformed" ]; then
  printf '{not-json\n'
  exit 0
fi

if [ "$#" -eq 4 ] && [ "$1" = "--data-dir" ] && [ "$3" = "--auto-port" ] && [ "$4" = "--no-auth" ]; then
  log_event "server_start|pid=$$|data_dir=$2|argv=$*|mode=$server_mode"
  if [ "$server_mode" = "exit-before-banner" ]; then
    printf 'fake server exiting before banner\n' >&2
    log_event "server_exit_before_banner|pid=$$"
    exit 17
  fi
  python3 - "$server_mode" "$event_log" <<'PY'
import http.server
import json
import os
import signal
import socketserver
import sys
import time

mode, event_log = sys.argv[1], sys.argv[2]

def log(message):
    with open(event_log, "a", encoding="utf-8") as handle:
        handle.write(message + "\n")

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        log(f"health_request|pid={os.getpid()}|path={self.path}")
        if mode == "banner-then-endpoint-hang":
            time.sleep(300)
            return
        if self.path != "/health":
            self.send_response(404)
            self.end_headers()
            return
        body = b'{"status":"ok"}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        log('health_response|body={"status":"ok"}')

    def log_message(self, fmt, *args):
        return

class Server(socketserver.TCPServer):
    allow_reuse_address = True

with Server(("127.0.0.1", 0), Handler) as server:
    port = server.server_address[1]
    def stop(signum, frame):
        if mode == "ignore-term":
            log(f"server_term_ignored|pid={os.getpid()}|signal={signum}")
            return
        log(f"server_term|pid={os.getpid()}|signal={signum}")
        raise SystemExit(0)
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    log(f"server_bound|pid={os.getpid()}|port={port}")
    if mode == "malformed-banner":
        print("Local:      http://127.0.0.1:notaport", flush=True)
    elif mode == "no-banner":
        pass
    elif mode == "api-docs-decoy-no-local":
        print(f"  ->  API Docs:   http://127.0.0.1:{port}", flush=True)
        log(f"server_api_docs_decoy|pid={os.getpid()}|port={port}")
    else:
        if mode == "delayed-banner":
            time.sleep(0.5)
        print(f"  ->  Local:      http://127.0.0.1:{port}", flush=True)
        log(f"server_banner|pid={os.getpid()}|port={port}")
    try:
        server.serve_forever()
    finally:
        log(f"server_exit|pid={os.getpid()}")
PY
  exit $?
fi

if [ "$#" -eq 6 ] && [ "$1" = "--data-dir" ] && [ "$3" = "repair-publication" ] && [ "$4" = "--tenant" ] && [ "$6" = "--json" ]; then
  log_event "repair_start|data_dir=$2|tenant=$5|argv=$*"
  if [ -f "$2/mutate_after_first" ]; then
    printf 'changed\n' >>"$2/idempotence_mutation"
  fi
  if [ "$mode" = "quarantine" ]; then
    mkdir -p "$2/.publication_quarantine/$5/txn_001"
    printf 'quarantined\n' >"$2/.publication_quarantine/$5/txn_001/journal.json"
    python3 - "$5" <<'PY'
import json
import sys
print(json.dumps({
    "tenant": sys.argv[1],
    "status": "quarantined",
    "action": "quarantine",
    "transaction_id": "txn_001",
    "phase": None,
    "evidence": ".publication/products/txn_001",
}, separators=(",", ":")))
PY
    log_event "repair_exit|data_dir=$2|tenant=$5|status=2"
    exit 2
  fi
  python3 - "$5" <<'PY'
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
  log_event "repair_exit|data_dir=$2|tenant=$5|status=0"
  exit 0
fi

printf 'unexpected fake binary argv: %s\n' "$*" >&2
log_event "unexpected_argv|argv=$*"
exit 99
SH
  chmod +x "$path"
}

write_fake_cargo() {
  local path="$1"
  local generated_mode="${2:-${FAKE_GENERATED_MODE:-ok}}"
  local mutate_on_repair="${3:-no}"
  mutate_on_repair="${FAKE_GENERATED_MUTATE_ON_REPAIR:-$mutate_on_repair}"
  cat >"$path" <<SH
#!/usr/bin/env bash
set -euo pipefail
generated_mode="$generated_mode"
mutate_on_repair="$mutate_on_repair"
SH
  cat >>"$path" <<'SH'

[ "${1:-}" = "test" ] || { echo "unexpected cargo argv: $*" >&2; exit 99; }
root="${PUBLICATION_REPAIR_CLI_ARTIFACT_DIR:?}"
manifest="${PUBLICATION_REPAIR_CLI_MANIFEST:?}"
mkdir -p "$root/case_clean"
python3 - "$root" "$manifest" "$generated_mode" "$mutate_on_repair" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
manifest_path = pathlib.Path(sys.argv[2])
mode = sys.argv[3]
mutate = sys.argv[4]
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
scenario = manifest["scenarios"][0]
layout = dict(scenario)
layout["scenario_id"] = layout.pop("id")
layout["boundaries"] = [layout.pop("boundary")["identity"]]
if mode == "bad-count":
    layouts = []
elif mode == "bad-digest":
    layout["digests"] = dict(layout["digests"])
    layout["digests"]["target"] = "sha256:" + ("0" * 64)
    layouts = [layout]
else:
    layouts = [layout]
case = root / "case_clean"
case.mkdir(exist_ok=True)
if mutate == "yes":
    (case / "mutate_after_first").write_text("1", encoding="utf-8")
(root / "generated_layouts.json").write_text(json.dumps(layouts), encoding="utf-8")
PY
SH
  chmod +x "$path"
}

run_runner() {
  local binary="$1"
  local manifest="$2"
  local artifact_dir="$3"
  shift 3
  PATH="$TMP_ROOT/bin:$PATH" "$RUNNER" \
    --binary "$binary" \
    --manifest "$manifest" \
    --artifact-dir "$artifact_dir" "$@"
}

expect_failure() {
  local expected="$1"
  shift
  local log="$TMP_ROOT/failure.log"
  if "$@" >"$log" 2>&1; then
    cat "$log" >&2
    die "expected failure containing: $expected"
  fi
  grep -F "$expected" "$log" >/dev/null || {
    cat "$log" >&2
    die "failure did not contain: $expected"
  }
}

assert_log_contains() {
  local path="$1"
  local expected="$2"
  grep -F "$expected" "$path" >/dev/null || {
    [ ! -f "$path" ] || cat "$path" >&2
    die "expected log to contain: $expected"
  }
}

assert_event_contains() {
  local expected="$1"
  assert_log_contains "$TMP_ROOT/events.log" "$expected"
}

assert_event_not_contains() {
  local unexpected="$1"
  if [ -f "$TMP_ROOT/events.log" ] && grep -F "$unexpected" "$TMP_ROOT/events.log" >/dev/null; then
    cat "$TMP_ROOT/events.log" >&2
    die "unexpected event present: $unexpected"
  fi
}

assert_event_count() {
  local pattern="$1"
  local expected="$2"
  local actual="0"
  if [ -f "$TMP_ROOT/events.log" ]; then
    actual="$( (grep -F "$pattern" "$TMP_ROOT/events.log" || true) | wc -l | tr -d ' ')"
  fi
  [ "$actual" = "$expected" ] || {
    [ ! -f "$TMP_ROOT/events.log" ] || cat "$TMP_ROOT/events.log" >&2
    die "event count for '$pattern' was $actual, expected $expected"
  }
}

wait_for_event() {
  local pattern="$1"
  local timeout_secs="$2"
  local deadline=$((SECONDS + timeout_secs))
  while :; do
    if [ -f "$TMP_ROOT/events.log" ] && grep -F "$pattern" "$TMP_ROOT/events.log" >/dev/null; then
      return 0
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      [ ! -f "$TMP_ROOT/events.log" ] || cat "$TMP_ROOT/events.log" >&2
      die "timed out waiting for event: $pattern"
    fi
    sleep 0.05
  done
}

latest_event_pid() {
  local pattern="$1"
  python3 - "$TMP_ROOT/events.log" "$pattern" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
pattern = sys.argv[2]
events = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
for line in reversed(events):
    if pattern not in line:
        continue
    match = re.search(r"(?:^|\|)pid=([0-9]+)(?:\||$)", line)
    if match:
        print(match.group(1))
        raise SystemExit(0)
raise SystemExit(f"missing pid for event containing {pattern!r}")
PY
}

assert_pid_stopped() {
  local pid="$1"
  local label="$2"
  local deadline=$((SECONDS + 3))
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      kill -9 "$pid" 2>/dev/null || true
      die "$label process $pid was still running"
    fi
    sleep 0.05
  done
}

assert_no_health_before_banner() {
  python3 - "$TMP_ROOT/events.log" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
events = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
banner = next((idx for idx, line in enumerate(events) if line.startswith("server_banner|")), None)
request = next((idx for idx, line in enumerate(events) if line.startswith("health_request|")), None)
if banner is None:
    raise SystemExit("missing canonical server banner event")
if request is None:
    raise SystemExit("missing /health request event after banner")
if request < banner:
    raise SystemExit("/health request occurred before canonical server banner")
PY
}

assert_converged_lifecycle_trace() {
  python3 - "$TMP_ROOT/events.log" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
events = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
kinds = [line.split("|", 1)[0] for line in events]
expected = [
    "repair_start",
    "repair_exit",
    "server_start",
    "server_bound",
    "server_banner",
    "health_request",
    "health_response",
    "server_term",
    "server_exit",
    "repair_start",
    "repair_exit",
    "server_start",
    "server_bound",
    "server_banner",
    "health_request",
    "health_response",
    "server_term",
    "server_exit",
]
if kinds != expected:
    raise SystemExit(
        "converged lifecycle event trace mismatch; expected first repair completion before server start/banner/health/stop and second repair completion before a distinct second server lifecycle, got "
        + repr(kinds)
    )
server_starts = [line for line in events if line.startswith("server_start|")]
if len(server_starts) != 2 or server_starts[0] == server_starts[1]:
    raise SystemExit("expected two distinct server invocations")
for line in server_starts:
    if "--data-dir " not in line or " --auto-port --no-auth" not in line:
        raise SystemExit("server invocation did not use exact --data-dir <case_root> --auto-port --no-auth argv: " + line)
PY
}

assert_term_ignored_kill_trace() {
  python3 - "$TMP_ROOT/events.log" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
events = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
kinds = [line.split("|", 1)[0] for line in events]
expected = [
    "repair_start",
    "repair_exit",
    "server_start",
    "server_bound",
    "server_banner",
    "health_request",
    "health_response",
    "server_term_ignored",
]
if kinds != expected:
    raise SystemExit("TERM-ignored lifecycle trace mismatch: " + repr(kinds))
bound = next(line for line in events if line.startswith("server_bound|"))
ignored = next(line for line in events if line.startswith("server_term_ignored|"))
bound_pid = re.search(r"(?:^|\|)pid=([0-9]+)(?:\||$)", bound).group(1)
ignored_pid = re.search(r"(?:^|\|)pid=([0-9]+)(?:\||$)", ignored).group(1)
if bound_pid != ignored_pid:
    raise SystemExit(f"TERM was not delivered to the bound server process: bound={bound_pid} ignored={ignored_pid}")
PY
}

assert_interrupted_cleanup_trace() {
  python3 - "$TMP_ROOT/events.log" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
events = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
kinds = [line.split("|", 1)[0] for line in events]
expected_prefix = [
    "repair_start",
    "repair_exit",
    "server_start",
    "server_bound",
    "server_banner",
    "health_request",
]
if kinds[:len(expected_prefix)] != expected_prefix:
    raise SystemExit("interrupted lifecycle prefix mismatch: " + repr(kinds))
terms = [line for line in events if line.startswith("server_term|")]
if len(terms) != 1:
    raise SystemExit("expected exactly one server_term during interrupted cleanup: " + repr(kinds))
bound = next(line for line in events if line.startswith("server_bound|"))
bound_pid = re.search(r"(?:^|\|)pid=([0-9]+)(?:\||$)", bound).group(1)
term_pid = re.search(r"(?:^|\|)pid=([0-9]+)(?:\||$)", terms[0]).group(1)
if bound_pid != term_pid:
    raise SystemExit(f"interrupted cleanup targeted the wrong process: bound={bound_pid} term={term_pid}")
PY
}

test_argument_and_absolute_path_validation() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "binary must be an absolute path" \
    run_runner "relative/flapjack" "$manifest" "$TMP_ROOT/artifacts"
  touch "$TMP_ROOT/artifacts/leftover"
  expect_failure "artifact directory must be empty" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_manifest_generated_count_and_digest_mismatch_failures() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo" bad-count
  expect_failure "generated layout count 0 does not match manifest layout_count 1" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"

  reset_artifacts
  write_fake_cargo "$TMP_ROOT/bin/cargo" bad-digest
  expect_failure "layout case_clean digests mismatch" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_malformed_cli_json_and_idempotence_mutation_failures() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" malformed
  write_fake_cargo "$TMP_ROOT/bin/cargo"
  expect_failure "case_clean first CLI stdout is not valid JSON" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"

  reset_artifacts
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" ok
  write_fake_cargo "$TMP_ROOT/bin/cargo" ok yes
  expect_failure "case_clean idempotence snapshot changed" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_manifest_declared_nonzero_exit_does_not_abort_runner() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest" 1 quarantine
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/nonzero.log" 2>&1 || {
      cat "$TMP_ROOT/nonzero.log" >&2
      die "manifest-declared nonzero CLI exit aborted the runner"
    }
  grep -F "PASS: publication repair CLI live contract passed" "$TMP_ROOT/nonzero.log" >/dev/null || {
    cat "$TMP_ROOT/nonzero.log" >&2
    die "nonzero CLI scenario did not complete the live contract"
  }
}

test_converged_case_runs_server_after_each_repair() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest" 1 absent-create
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/converged.log" 2>&1 || {
      cat "$TMP_ROOT/converged.log" >&2
      die "converged lifecycle runner failed before server assertions"
    }
  assert_converged_lifecycle_trace
}

test_manifest_declared_nonzero_exit_runs_one_server_then_stops() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest" 1 quarantine
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/nonzero_server.log" 2>&1 || {
      cat "$TMP_ROOT/nonzero_server.log" >&2
      die "manifest-declared nonzero CLI exit aborted the runner before server assertions"
    }
  assert_event_count "repair_exit|" 1
  assert_event_count "server_start|" 1
  assert_event_not_contains "second repair"
}

test_banner_gates_health_probe() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=delayed-banner write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/banner_gating.log" 2>&1 || {
      cat "$TMP_ROOT/banner_gating.log" >&2
      die "banner-gating runner failed before ordering assertions"
    }
  assert_no_health_before_banner
  assert_event_contains 'health_response|body={"status":"ok"}'
}

test_exact_child_cleanup_preserves_sentinel() {
  make_workspace
  local sentinel_pid=""
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  sleep 300 &
  sentinel_pid=$!
  trap 'kill "$sentinel_pid" 2>/dev/null || true; cleanup' EXIT
  write_manifest "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/cleanup.log" 2>&1 || {
      cat "$TMP_ROOT/cleanup.log" >&2
      kill "$sentinel_pid" 2>/dev/null || true
      die "cleanup runner failed before exact-child assertions"
    }
  assert_event_count "server_term|" 2
  kill -0 "$sentinel_pid" 2>/dev/null || die "sentinel process was not preserved"
  kill "$sentinel_pid" 2>/dev/null || true
  wait "$sentinel_pid" 2>/dev/null || true
  trap cleanup EXIT
}

test_term_ignored_server_escalates_to_kill() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  local server_pid=""
  local started_at="$SECONDS"
  write_manifest "$manifest" 1 quarantine
  FAKE_SERVER_MODE=ignore-term write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/term_ignored.log" 2>&1 || {
      cat "$TMP_ROOT/term_ignored.log" >&2
      die "TERM-ignored server runner failed before escalation assertions"
    }
  server_pid="$(latest_event_pid "server_bound|")"
  assert_term_ignored_kill_trace
  assert_pid_stopped "$server_pid" "TERM-ignored server"
  [ $((SECONDS - started_at)) -le 9 ] || die "TERM-to-KILL escalation exceeded bounded shutdown"
}

test_post_kill_group_timeout_fails_closed() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  local bash_env="$TMP_ROOT/force_post_kill_group_alive.sh"
  local post_kill_marker="$TMP_ROOT/post_kill.signal"
  write_manifest "$manifest" 1 quarantine
  FAKE_SERVER_MODE=ignore-term write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo"
  cat >"$bash_env" <<'SH'
kill() {
  if [ "${1:-}" = "-KILL" ] && [ "${2:-}" = "--" ]; then
    builtin kill "$@"
    : >"$FORCE_POST_KILL_GROUP_ALIVE_MARKER"
    return 0
  fi
  if [ "${1:-}" = "-0" ] && [ "${2:-}" = "--" ] &&
    [ -f "$FORCE_POST_KILL_GROUP_ALIVE_MARKER" ]; then
    return 0
  fi
  builtin kill "$@"
}
SH

  BASH_ENV="$bash_env" FORCE_POST_KILL_GROUP_ALIVE_MARKER="$post_kill_marker" \
    expect_failure "server process group remained alive after KILL" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  [ -f "$post_kill_marker" ] || die "post-KILL process-group timeout seam did not observe KILL"
  assert_event_contains "server_term_ignored|"
}

test_post_kill_unreapable_child_fails_within_budget() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  local bash_env="$TMP_ROOT/force_child_unreapable.sh"
  local kill_marker="$TMP_ROOT/unreapable_child.signal"
  local started_at="$SECONDS"
  write_manifest "$manifest" 1 quarantine
  FAKE_SERVER_MODE=ignore-term write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo"
  # After the real KILL is delivered, force the direct-child liveness probe
  # (`kill -0 <pid>`, no group `--`) to keep reporting the child alive so the
  # recorded child can never be reaped. A blocking `wait` would stall shutdown
  # forever; the bounded reap must instead give up and fail closed on budget.
  cat >"$bash_env" <<'SH'
kill() {
  if [ "${1:-}" = "-KILL" ]; then
    builtin kill "$@"
    : >"$FORCE_UNREAPABLE_CHILD_MARKER"
    return 0
  fi
  if [ "${1:-}" = "-0" ] && [ "$#" -eq 2 ] && [ "${2#-}" = "$2" ] &&
    [ -f "$FORCE_UNREAPABLE_CHILD_MARKER" ]; then
    return 0
  fi
  builtin kill "$@"
}
SH

  BASH_ENV="$bash_env" FORCE_UNREAPABLE_CHILD_MARKER="$kill_marker" \
    expect_failure "server child could not be reaped within the bounded shutdown budget after KILL" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  [ -f "$kill_marker" ] || die "post-KILL unreapable-child seam did not observe KILL"
  [ $((SECONDS - started_at)) -le 9 ] || die "post-KILL unreapable-child shutdown exceeded bounded budget"
  assert_event_contains "server_term_ignored|"
}

test_interrupted_run_cleans_child_group_and_preserves_sentinel() {
  make_workspace
  local sentinel_pid=""
  local runner_pid=""
  local runner_status=0
  local watchdog_pid=""
  local timeout_marker="$TMP_ROOT/interrupted_runner_timeout"
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  local server_pid=""
  sleep 300 &
  sentinel_pid=$!
  trap 'kill "$sentinel_pid" 2>/dev/null || true; cleanup' EXIT
  write_manifest "$manifest"
  FAKE_SERVER_MODE=banner-then-endpoint-hang write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  PATH="$TMP_ROOT/bin:$PATH" python3 - "$RUNNER" "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/interrupted.log" 2>&1 <<'PY' &
import os
import signal
import sys

os.setsid()
signal.signal(signal.SIGINT, signal.SIG_DFL)
runner, binary, manifest, artifact_dir = sys.argv[1:]
os.execv(
    runner,
    [
        runner,
        "--binary", binary,
        "--manifest", manifest,
        "--artifact-dir", artifact_dir,
    ],
)
PY
  runner_pid=$!
  wait_for_event "health_request|" 5
  server_pid="$(latest_event_pid "server_bound|")"
  kill -INT -- "-$runner_pid"
  python3 - "$runner_pid" "$timeout_marker" <<'PY' &
import os
import pathlib
import signal
import sys
import time

runner_pid = int(sys.argv[1])
timeout_marker = pathlib.Path(sys.argv[2])
time.sleep(2)
try:
    os.kill(runner_pid, 0)
except ProcessLookupError:
    raise SystemExit(0)
timeout_marker.touch()
os.kill(runner_pid, signal.SIGTERM)
PY
  watchdog_pid=$!
  set +e
  wait "$runner_pid"
  runner_status=$?
  kill "$watchdog_pid" 2>/dev/null
  wait "$watchdog_pid" 2>/dev/null
  set -e
  [ ! -f "$timeout_marker" ] || die "interrupted runner did not exit promptly after SIGINT"
  [ "$runner_status" -eq 130 ] || die "interrupted runner exit status was $runner_status, expected SIGINT status 130"
  assert_interrupted_cleanup_trace
  assert_pid_stopped "$server_pid" "interrupted server"
  kill -0 "$sentinel_pid" 2>/dev/null || die "sentinel process was not preserved"
  kill "$sentinel_pid" 2>/dev/null || true
  wait "$sentinel_pid" 2>/dev/null || true
  trap cleanup EXIT
}

test_server_early_exit() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=exit-before-banner write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean server exited before startup banner (status 17)" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_log_contains "$TMP_ROOT/failure.log" \
    "stdout: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stdout stderr: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stderr"
  assert_event_contains "server_exit_before_banner|"
}

test_malformed_server_banner() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=malformed-banner write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean timed out waiting for startup banner" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_event_count "health_request|" 0
  assert_event_contains "server_term|"
}

test_decoy_api_docs_url_requires_local_banner() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=api-docs-decoy-no-local write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean timed out waiting for startup banner" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_event_contains "server_api_docs_decoy|"
  assert_event_count "health_request|" 0
  assert_event_contains "server_term|"
}

test_server_startup_timeout() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=no-banner write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean timed out waiting for startup banner" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_log_contains "$TMP_ROOT/failure.log" \
    "stdout: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stdout stderr: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stderr"
  assert_event_count "health_request|" 0
  assert_event_contains "server_term|"
}

test_server_endpoint_timeout() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=banner-then-endpoint-hang write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean /health probe timed out" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_log_contains "$TMP_ROOT/failure.log" \
    "stdout: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stdout stderr: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stderr"
  assert_no_health_before_banner
  assert_event_contains "server_term|"
}

run_test_function() {
  local name="$1"
  case "$name" in
    argument_and_absolute_path_validation) test_argument_and_absolute_path_validation ;;
    manifest_generated_count_and_digest_mismatch_failures) test_manifest_generated_count_and_digest_mismatch_failures ;;
    malformed_cli_json_and_idempotence_mutation_failures) test_malformed_cli_json_and_idempotence_mutation_failures ;;
    manifest_declared_nonzero_exit_does_not_abort_runner) test_manifest_declared_nonzero_exit_does_not_abort_runner ;;
    converged_case_runs_server_after_each_repair) test_converged_case_runs_server_after_each_repair ;;
    manifest_declared_nonzero_exit_runs_one_server_then_stops) test_manifest_declared_nonzero_exit_runs_one_server_then_stops ;;
    banner_gates_health_probe) test_banner_gates_health_probe ;;
    exact_child_cleanup_preserves_sentinel) test_exact_child_cleanup_preserves_sentinel ;;
    term_ignored_server_escalates_to_kill) test_term_ignored_server_escalates_to_kill ;;
    post_kill_group_timeout_fails_closed) test_post_kill_group_timeout_fails_closed ;;
    post_kill_unreapable_child_fails_within_budget) test_post_kill_unreapable_child_fails_within_budget ;;
    interrupted_run_cleans_child_group_and_preserves_sentinel) test_interrupted_run_cleans_child_group_and_preserves_sentinel ;;
    server_early_exit) test_server_early_exit ;;
    malformed_server_banner) test_malformed_server_banner ;;
    decoy_api_docs_url_requires_local_banner) test_decoy_api_docs_url_requires_local_banner ;;
    server_startup_timeout) test_server_startup_timeout ;;
    server_endpoint_timeout) test_server_endpoint_timeout ;;
    *) die "unknown test selector: $name" ;;
  esac
}

main() {
  if [ "$#" -gt 0 ]; then
    local selected
    for selected in "$@"; do
      run_test_function "$selected"
      cleanup
      TMP_ROOT=""
    done
    printf 'PASS: selected publication repair CLI live runner focused tests passed\n'
    return
  fi

  run_test_function argument_and_absolute_path_validation
  cleanup
  TMP_ROOT=""
  run_test_function manifest_generated_count_and_digest_mismatch_failures
  cleanup
  TMP_ROOT=""
  run_test_function malformed_cli_json_and_idempotence_mutation_failures
  cleanup
  TMP_ROOT=""
  run_test_function manifest_declared_nonzero_exit_does_not_abort_runner
  cleanup
  TMP_ROOT=""
  run_test_function converged_case_runs_server_after_each_repair
  cleanup
  TMP_ROOT=""
  run_test_function manifest_declared_nonzero_exit_runs_one_server_then_stops
  cleanup
  TMP_ROOT=""
  run_test_function banner_gates_health_probe
  cleanup
  TMP_ROOT=""
  run_test_function exact_child_cleanup_preserves_sentinel
  cleanup
  TMP_ROOT=""
  run_test_function term_ignored_server_escalates_to_kill
  cleanup
  TMP_ROOT=""
  run_test_function post_kill_group_timeout_fails_closed
  cleanup
  TMP_ROOT=""
  run_test_function post_kill_unreapable_child_fails_within_budget
  cleanup
  TMP_ROOT=""
  run_test_function interrupted_run_cleans_child_group_and_preserves_sentinel
  cleanup
  TMP_ROOT=""
  run_test_function server_early_exit
  cleanup
  TMP_ROOT=""
  run_test_function malformed_server_banner
  cleanup
  TMP_ROOT=""
  run_test_function decoy_api_docs_url_requires_local_banner
  cleanup
  TMP_ROOT=""
  run_test_function server_startup_timeout
  cleanup
  TMP_ROOT=""
  run_test_function server_endpoint_timeout
  printf 'PASS: publication repair CLI live runner focused tests passed\n'
}

main "$@"
