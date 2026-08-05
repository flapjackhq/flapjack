#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_FOR_FLAPJACK="$ENGINE_DIR/tests/common/wait_for_flapjack.sh"

ADMIN_KEY="stage4-admin-key"
PEER_KEY="stage4-peer-key"
APP_ID="stage4-probe-app"
PEER_APP_ID="flapjack-replication"
AUTH_TENANT="stage4_auth_tenant"
NO_AUTH_TENANT="stage4_no_auth_tenant"
BIN=""
TMP_ROOT=""
LOCAL_IP=""
NODE_A_PORT=""
NODE_B_PORT=""
NODE_A_BASE=""
NODE_B_BASE=""
NO_AUTH_BASE=""
TESTS_RUN=0
TESTS_FAILED=0
INTERRUPTED_EXIT_CODE=0
OUTCOME="INDETERMINATE"
STARTED_PID=""
NODE_PIDS=()

cleanup() {
  local script_exit_code=$? pid
  for pid in "${NODE_PIDS[@]}"; do
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done

  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    if [ "$TESTS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ] || \
      [ "$INTERRUPTED_EXIT_CODE" -ne 0 ] || [ "$OUTCOME" != "PASS" ]; then
      printf 'INFO: preserved replication peer auth probe evidence at %s\n' "$TMP_ROOT" >&2
    else
      rm -rf "$TMP_ROOT"
    fi
  fi
}
trap cleanup EXIT

record_interrupt() {
  local signal_name="$1" exit_code="$2"
  INTERRUPTED_EXIT_CODE="$exit_code"
  printf 'INDETERMINATE: interrupted by %s\n' "$signal_name" >&2
  exit "$exit_code"
}
trap 'record_interrupt INT 130' INT
trap 'record_interrupt TERM 143' TERM

indeterminate() {
  printf 'INDETERMINATE: %s\n' "$1" >&2
  exit 1
}

require_tools() {
  local missing=0 tool
  for tool in cargo curl grep mktemp python3 sed; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'INDETERMINATE: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  if [ "$missing" -ne 0 ]; then
    exit 1
  fi
  if [ ! -x "$WAIT_FOR_FLAPJACK" ]; then
    indeterminate "wait helper is not executable: $WAIT_FOR_FLAPJACK"
  fi
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

resolve_binary() {
  local build_log="$TMP_ROOT/build.log"
  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$build_log" 2>&1); then
    tail -30 "$build_log" >&2 || true
    indeterminate "cargo build -p flapjack-server failed"
  fi

  BIN="$(target_dir)/debug/flapjack"
  if [ ! -x "$BIN" ]; then
    indeterminate "expected current-checkout binary is not executable: $BIN"
  fi
}

select_non_loopback_ip() {
  local selected
  if ! selected="$(python3 - <<'PY'
import ipaddress
import socket

candidates = []
probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
try:
    probe.connect(("192.0.2.1", 9))
    candidates.append(probe.getsockname()[0])
except OSError:
    pass
finally:
    probe.close()

try:
    candidates.extend(
        info[4][0]
        for info in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET)
    )
except OSError:
    pass

for candidate in dict.fromkeys(candidates):
    address = ipaddress.ip_address(candidate)
    if address.is_loopback or address.is_link_local or address.is_unspecified:
        continue
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        listener.bind((candidate, 0))
    except OSError:
        listener.close()
        continue
    listener.close()
    print(candidate)
    break
else:
    raise SystemExit(1)
PY
)"; then
    indeterminate "no bindable non-loopback local IPv4 interface was found"
  fi
  if [ -z "$selected" ]; then
    indeterminate "non-loopback interface selection returned no address"
  fi
  LOCAL_IP="$selected"
}

allocate_cluster_ports() {
  local allocated
  if ! allocated="$(python3 - "$LOCAL_IP" <<'PY'
import socket
import sys

listeners = []
try:
    for _ in range(2):
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.bind((sys.argv[1], 0))
        listener.listen(1)
        listeners.append(listener)
    print(listeners[0].getsockname()[1], listeners[1].getsockname()[1])
finally:
    for listener in listeners:
        listener.close()
PY
)"; then
    indeterminate "could not allocate two free ports on $LOCAL_IP"
  fi
  read -r NODE_A_PORT NODE_B_PORT <<<"$allocated"
  if [ -z "$NODE_A_PORT" ] || [ -z "$NODE_B_PORT" ] || [ "$NODE_A_PORT" = "$NODE_B_PORT" ]; then
    indeterminate "free-port allocator returned invalid ports: $allocated"
  fi
  NODE_A_BASE="http://${LOCAL_IP}:${NODE_A_PORT}"
  NODE_B_BASE="http://${LOCAL_IP}:${NODE_B_PORT}"
}

write_node_configs() {
  mkdir -p "$TMP_ROOT/node_a" "$TMP_ROOT/node_b"
  printf '{"node_id":"node-a","bind_addr":"%s:%s","peers":[{"node_id":"node-b","addr":"%s"}]}\n' \
    "$LOCAL_IP" "$NODE_A_PORT" "$NODE_B_BASE" >"$TMP_ROOT/node_a/node.json"
  printf '{"node_id":"node-b","bind_addr":"%s:%s","peers":[{"node_id":"node-a","addr":"%s"}]}\n' \
    "$LOCAL_IP" "$NODE_B_PORT" "$NODE_A_BASE" >"$TMP_ROOT/node_b/node.json"
}

start_auth_node() {
  local node_id="$1" data_dir="$2" bind_addr="$3" log_path="$4"
  # This probe deliberately runs cleartext http:// peers with a peer key set,
  # which NodeConfig::load_or_default now refuses by default.
  env \
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_REPLICATION_API_KEY="$PEER_KEY" \
    FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1 \
    FLAPJACK_NODE_ID="$node_id" \
    FLAPJACK_DATA_DIR="$data_dir" \
    FLAPJACK_DISABLE_DASHBOARD=1 \
    FLAPJACK_STARTUP_CATCHUP_STRICT=0 \
    FLAPJACK_STARTUP_CATCHUP_TIMEOUT_SECS=2 \
    FLAPJACK_SYNC_INTERVAL_SECS=1 \
    NO_PROXY='*' no_proxy='*' \
    "$BIN" --bind-addr "$bind_addr" >"$log_path" 2>&1 &
  STARTED_PID=$!
  NODE_PIDS+=("$STARTED_PID")
}

wait_for_node() {
  local label="$1" pid="$2" base="$3" log_path="$4"
  if ! NO_PROXY='*' no_proxy='*' "$WAIT_FOR_FLAPJACK" \
    --pid "$pid" \
    --health-url "$base/health" \
    --log-path "$log_path" \
    --retries 80 \
    --interval-seconds 0.5; then
    TESTS_FAILED=$((TESTS_FAILED + 1))
    printf '[FAIL] %s expected=healthy actual=not-ready body_preview=see-log\n' "$label" >&2
    return 1
  fi
}

start_auth_cluster() {
  local node_a_pid node_b_pid
  start_auth_node "node-a" "$TMP_ROOT/node_a" "$LOCAL_IP:$NODE_A_PORT" "$TMP_ROOT/node_a/server.log"
  node_a_pid="$STARTED_PID"
  wait_for_node "node A readiness" "$node_a_pid" "$NODE_A_BASE" "$TMP_ROOT/node_a/server.log"

  start_auth_node "node-b" "$TMP_ROOT/node_b" "$LOCAL_IP:$NODE_B_PORT" "$TMP_ROOT/node_b/server.log"
  node_b_pid="$STARTED_PID"
  wait_for_node "node B readiness" "$node_b_pid" "$NODE_B_BASE" "$TMP_ROOT/node_b/server.log"
}

http_request() {
  local method="$1" url="$2" body_path="$3" api_key="$4" application_id="$5" payload="$6"
  local args=(--noproxy '*' -sS -X "$method" -o "$body_path" -w '%{http_code}')
  if [ -n "$application_id" ]; then
    args+=(-H "x-algolia-application-id: $application_id")
  fi
  if [ -n "$api_key" ]; then
    args+=(-H "x-algolia-api-key: $api_key")
  fi
  if [ "$payload" != "__NO_BODY__" ]; then
    args+=(-H 'content-type: application/json' --data "$payload")
  fi
  curl "${args[@]}" "$url"
}

json_field() {
  local path="$1" field="$2"
  python3 - "$path" "$field" <<'PY'
import json
import sys

with open(sys.argv[1]) as source:
    value = json.load(source)
for part in sys.argv[2].split("."):
    value = value[part]
print(value)
PY
}

body_preview() {
  python3 - "$1" <<'PY'
import sys

with open(sys.argv[1], errors="replace") as source:
    body = source.read(241).replace("\n", " ")
print(body[:240])
PY
}

fail_setup() {
  local label="$1" expected="$2" actual="$3" body_path="$4"
  TESTS_FAILED=$((TESTS_FAILED + 1))
  printf '[FAIL] %s expected=%s actual=%s body_preview=%s\n' \
    "$label" "$expected" "$actual" "$(body_preview "$body_path")" >&2
  return 1
}

wait_for_task() {
  local base="$1" tenant="$2" task_id="$3" api_key="$4" application_id="$5" prefix="$6"
  local attempt=0 body_path="$TMP_ROOT/${prefix}_task.json" status task_state
  while [ "$attempt" -lt 80 ]; do
    status="$(http_request GET "$base/1/indexes/$tenant/task/$task_id" "$body_path" "$api_key" "$application_id" __NO_BODY__)"
    task_state="$(json_field "$body_path" status 2>/dev/null || true)"
    if [ "$status" = "200" ] && [ "$task_state" = "published" ]; then
      return 0
    fi
    attempt=$((attempt + 1))
    sleep 0.25
  done
  fail_setup "$prefix task publication" "status=200 body.status=published" \
    "status=$status body.status=$task_state" "$body_path"
}

seed_object() {
  local base="$1" tenant="$2" object_id="$3" object_json="$4" api_key="$5" prefix="$6"
  local body_path="$TMP_ROOT/${prefix}_create.json" status task_id
  local application_id="$APP_ID"
  if [ -z "$api_key" ]; then
    application_id=""
  fi
  status="$(http_request POST "$base/1/indexes" "$body_path" "$api_key" "$application_id" \
    "{\"uid\":\"$tenant\"}")"
  if [ "$status" != "200" ]; then
    fail_setup "$prefix create tenant" "status=200" "status=$status" "$body_path"
  fi

  body_path="$TMP_ROOT/${prefix}_seed.json"
  status="$(http_request PUT "$base/1/indexes/$tenant/$object_id" "$body_path" "$api_key" \
    "$application_id" "$object_json")"
  if [ "$status" != "200" ]; then
    fail_setup "$prefix seed object" "status=200" "status=$status" "$body_path"
  fi
  task_id="$(json_field "$body_path" taskID 2>/dev/null || true)"
  if [ -z "$task_id" ]; then
    fail_setup "$prefix seed object" "numeric taskID" "missing taskID" "$body_path"
  fi
  wait_for_task "$base" "$tenant" "$task_id" "$api_key" "$application_id" "$prefix"
}

record_exact_body() {
  local label="$1" expected_status="$2" actual_status="$3" body_path="$4" expected_body="$5"
  local actual_body
  actual_body="$(cat "$body_path")"
  TESTS_RUN=$((TESTS_RUN + 1))
  if [ "$actual_status" = "$expected_status" ] && [ "$actual_body" = "$expected_body" ]; then
    printf '[PASS] %s expected=status=%s body=%s actual=status=%s body_preview=%s\n' \
      "$label" "$expected_status" "$expected_body" "$actual_status" "$(body_preview "$body_path")"
  else
    TESTS_FAILED=$((TESTS_FAILED + 1))
    printf '[FAIL] %s expected=status=%s body=%s actual=status=%s body_preview=%s\n' \
      "$label" "$expected_status" "$expected_body" "$actual_status" "$(body_preview "$body_path")"
  fi
}

replication_payload_matches() {
  local body_path="$1" tenant="$2" object_id="$3"
  python3 - "$body_path" "$tenant" "$object_id" <<'PY'
import json
import sys

with open(sys.argv[1]) as source:
    data = json.load(source)
matches = (
    isinstance(data, dict)
    and data.get("tenant_id") == sys.argv[2]
    and isinstance(data.get("ops"), list)
    and len(data["ops"]) > 0
    and sys.argv[3] in json.dumps(data["ops"], sort_keys=True)
)
raise SystemExit(0 if matches else 1)
PY
}

record_replication_payload() {
  local label="$1" expected_status="$2" actual_status="$3" body_path="$4" tenant="$5" object_id="$6"
  TESTS_RUN=$((TESTS_RUN + 1))
  if [ "$actual_status" = "$expected_status" ] && \
    replication_payload_matches "$body_path" "$tenant" "$object_id"; then
    printf '[PASS] %s expected=status=%s tenant=%s ops-containing=%s actual=status=%s body_preview=%s\n' \
      "$label" "$expected_status" "$tenant" "$object_id" "$actual_status" "$(body_preview "$body_path")"
  else
    TESTS_FAILED=$((TESTS_FAILED + 1))
    printf '[FAIL] %s expected=status=%s tenant=%s ops-containing=%s actual=status=%s body_preview=%s\n' \
      "$label" "$expected_status" "$tenant" "$object_id" "$actual_status" "$(body_preview "$body_path")"
  fi
}

run_authorization_checks() {
  local body_path status
  local credential_error='{"message":"Invalid Application-ID or API key","status":403}'

  body_path="$TMP_ROOT/peer_ops.json"
  status="$(http_request GET "$NODE_A_BASE/internal/ops?tenant_id=$AUTH_TENANT&since_seq=0" \
    "$body_path" "$PEER_KEY" "$PEER_APP_ID" __NO_BODY__)"
  record_replication_payload "peer key reads replication ops" 200 "$status" "$body_path" \
    "$AUTH_TENANT" "boundary-object"

  body_path="$TMP_ROOT/peer_add.json"
  status="$(http_request POST "$NODE_A_BASE/internal/cluster/peers" "$body_path" "$PEER_KEY" \
    "$PEER_APP_ID" '{"node_id":"denied-node","addr":"http://192.0.2.10:7700"}')"
  record_exact_body "peer key cannot add cluster peer" 403 "$status" "$body_path" "$credential_error"

  body_path="$TMP_ROOT/peer_remove.json"
  status="$(http_request DELETE "$NODE_A_BASE/internal/cluster/peers/node-b" "$body_path" \
    "$PEER_KEY" "$PEER_APP_ID" __NO_BODY__)"
  record_exact_body "peer key cannot remove cluster peer" 403 "$status" "$body_path" "$credential_error"

  body_path="$TMP_ROOT/peer_rotate.json"
  status="$(http_request POST "$NODE_A_BASE/internal/rotate-admin-key" "$body_path" "$PEER_KEY" \
    "$PEER_APP_ID" '{}')"
  record_exact_body "peer key cannot rotate admin key" 403 "$status" "$body_path" "$credential_error"

  body_path="$TMP_ROOT/query_peer_key.json"
  status="$(http_request GET "$NODE_A_BASE/internal/ops?tenant_id=$AUTH_TENANT&since_seq=0&x-algolia-api-key=$PEER_KEY" \
    "$body_path" "" "$PEER_APP_ID" __NO_BODY__)"
  record_exact_body "peer route rejects query-string-only peer key" 403 "$status" "$body_path" "$credential_error"

  body_path="$TMP_ROOT/random_peer_key.json"
  status="$(http_request GET "$NODE_A_BASE/internal/ops?tenant_id=$AUTH_TENANT&since_seq=0" \
    "$body_path" "random-not-peer-key" "$PEER_APP_ID" __NO_BODY__)"
  record_exact_body "peer route rejects random header key" 403 "$status" "$body_path" "$credential_error"

  body_path="$TMP_ROOT/peer_public_route.json"
  status="$(http_request GET "$NODE_A_BASE/1/indexes" "$body_path" "$PEER_KEY" \
    "$PEER_APP_ID" __NO_BODY__)"
  record_exact_body "peer key cannot read public indexes route" 403 "$status" "$body_path" "$credential_error"

  body_path="$TMP_ROOT/admin_ops.json"
  status="$(http_request GET "$NODE_A_BASE/internal/ops?tenant_id=$AUTH_TENANT&since_seq=0" \
    "$body_path" "$ADMIN_KEY" "$PEER_APP_ID" __NO_BODY__)"
  record_replication_payload "admin key retains replication ops access" 200 "$status" "$body_path" \
    "$AUTH_TENANT" "boundary-object"
}

object_matches() {
  local body_path="$1" object_id="$2" expected_name="$3" expected_rank="$4"
  python3 - "$body_path" "$object_id" "$expected_name" "$expected_rank" <<'PY'
import json
import sys

with open(sys.argv[1]) as source:
    actual = json.load(source)
expected = {"objectID": sys.argv[2], "name": sys.argv[3], "rank": int(sys.argv[4])}
raise SystemExit(0 if actual == expected else 1)
PY
}

prove_replication_converges() {
  local body_path="$TMP_ROOT/replicated_object.json" status="000" attempt=0
  seed_object "$NODE_A_BASE" "$AUTH_TENANT" "replicated-object" \
    '{"objectID":"replicated-object","name":"replicated beta","rank":7}' "$ADMIN_KEY" "replication"

  while [ "$attempt" -lt 80 ]; do
    status="$(http_request GET "$NODE_B_BASE/1/indexes/$AUTH_TENANT/replicated-object" \
      "$body_path" "$ADMIN_KEY" "$APP_ID" __NO_BODY__)"
    if [ "$status" = "200" ] && object_matches "$body_path" "replicated-object" "replicated beta" 7; then
      break
    fi
    attempt=$((attempt + 1))
    sleep 0.25
  done

  TESTS_RUN=$((TESTS_RUN + 1))
  if [ "$status" = "200" ] && object_matches "$body_path" "replicated-object" "replicated beta" 7; then
    printf '[PASS] replication converges on node B expected=status=200 exact-object=replicated-object actual=status=%s body_preview=%s\n' \
      "$status" "$(body_preview "$body_path")"
  else
    TESTS_FAILED=$((TESTS_FAILED + 1))
    printf '[FAIL] replication converges on node B expected=status=200 exact-object=replicated-object actual=status=%s body_preview=%s\n' \
      "$status" "$(body_preview "$body_path")"
  fi
}

allocate_loopback_port() {
  python3 - <<'PY'
import socket

listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
listener.bind(("127.0.0.1", 0))
print(listener.getsockname()[1])
listener.close()
PY
}

start_no_auth_server() {
  local port log_path="$TMP_ROOT/no_auth/server.log"
  mkdir -p "$TMP_ROOT/no_auth"
  if ! port="$(allocate_loopback_port)" || [ -z "$port" ]; then
    indeterminate "could not allocate a free loopback port for the no-auth server"
  fi
  NO_AUTH_BASE="http://127.0.0.1:$port"
  env \
    FLAPJACK_NO_AUTH=1 \
    FLAPJACK_DATA_DIR="$TMP_ROOT/no_auth" \
    FLAPJACK_DISABLE_DASHBOARD=1 \
    NO_PROXY='*' no_proxy='*' \
    "$BIN" --bind-addr "127.0.0.1:$port" >"$log_path" 2>&1 &
  STARTED_PID=$!
  NODE_PIDS+=("$STARTED_PID")
  wait_for_node "no-auth server readiness" "$STARTED_PID" "$NO_AUTH_BASE" "$log_path"
}

prove_no_auth_compatibility() {
  local body_path="$TMP_ROOT/no_auth_ops.json" status
  start_no_auth_server
  seed_object "$NO_AUTH_BASE" "$NO_AUTH_TENANT" "no-auth-object" \
    '{"objectID":"no-auth-object","name":"no auth gamma","rank":11}' "" "no_auth"
  status="$(http_request GET "$NO_AUTH_BASE/internal/ops?tenant_id=$NO_AUTH_TENANT&since_seq=0" \
    "$body_path" "" "" __NO_BODY__)"
  record_replication_payload "no-auth mode exposes replication ops" 200 "$status" "$body_path" \
    "$NO_AUTH_TENANT" "no-auth-object"
}

main() {
  require_tools
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fj-replication-peer-auth.XXXXXX")"
  resolve_binary
  select_non_loopback_ip
  allocate_cluster_ports
  write_node_configs
  start_auth_cluster

  seed_object "$NODE_A_BASE" "$AUTH_TENANT" "boundary-object" \
    '{"objectID":"boundary-object","name":"boundary alpha","rank":3}' "$ADMIN_KEY" "boundary"
  run_authorization_checks
  prove_replication_converges
  prove_no_auth_compatibility

  if [ "$TESTS_RUN" -ne 10 ]; then
    printf 'ERROR: expected 10 checks, ran %s\n' "$TESTS_RUN" >&2
    exit 1
  fi
  if [ "$TESTS_FAILED" -ne 0 ]; then
    exit 1
  fi
  OUTCOME="PASS"
  printf 'PASS: replication peer authorization HTTP probe completed (%s checks)\n' "$TESTS_RUN"
}

main "$@"
