#!/usr/bin/env bash
#
# Stage 1 RED baseline for the replication peer-credential lane (SEC-G9).
#
# Proves against a RUNNING primary that the credential a replica is configured
# with today is not a replication credential at all: the same header pair that
# `PeerClient` sends (`x-algolia-application-id: flapjack-replication` +
# `x-algolia-api-key: <admin key>`) both pulls the oplog AND re-shapes cluster
# membership. Compromising any replica therefore yields administrative control
# of the primary.
#
# Reversible by construction: the only mutation is adding a bogus peer and
# removing it again, and the server runs against a `mktemp` data directory that
# is destroyed on success. `POST /internal/rotate-admin-key` is deliberately NOT
# exercised — it changes state other work on this host may depend on.
#
# Usage:
#   cd engine && timeout 900 bash tests/replication_peer_auth_red_baseline_capture.sh \
#     | tee tests/results/replication_peer_auth_red_baseline.log

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_FOR_FLAPJACK="$ENGINE_DIR/tests/common/wait_for_flapjack.sh"

ADMIN_KEY="secg9-baseline-admin-key"
# The exact application ID `flapjack-replication/src/peer.rs` attaches to every
# outbound peer request. Not a new header: this lane introduces none.
PEER_APP_ID="flapjack-replication"
RANDOM_KEY="secg9-baseline-not-a-real-key"
NODE_ID="secg9-baseline-primary"
# Non-loopback, documentation-range origin. `NodeConfig::normalize_peer_addr`
# rejects localhost/metadata destinations, so a peer origin must be routable-
# looking. Nothing here is ever contacted successfully.
ADVERTISE_ADDR="http://198.51.100.5:7700"
BASELINE_TENANT="secg9_baseline"
BOGUS_PEER_ID="secg9-bogus-peer"
BOGUS_PEER_ADDR="http://198.51.100.20:7700"

BIN=""
TMP_ROOT=""
SERVER_PID=""
BASE=""
CHECKS_RUN=0
CHECKS_FAILED=0

cleanup() {
  local script_exit_code=$?
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi

  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    if [ "$CHECKS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      printf 'INFO: preserved replication peer-auth baseline evidence at %s\n' "$TMP_ROOT" >&2
    else
      rm -rf "$TMP_ROOT"
    fi
  fi
}
trap cleanup EXIT

require_tools() {
  local missing=0 tool
  for tool in cargo curl mktemp python3 sed; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  if [ "$missing" -ne 0 ]; then
    printf 'INDETERMINATE: prerequisites missing\n'
    exit 1
  fi
  if [ ! -x "$WAIT_FOR_FLAPJACK" ]; then
    printf 'ERROR: wait helper is not executable: %s\n' "$WAIT_FOR_FLAPJACK" >&2
    printf 'INDETERMINATE: prerequisites missing\n'
    exit 1
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
    echo 'ERROR: cargo build -p flapjack-server failed' >&2
    exit 1
  fi

  BIN="$(target_dir)/debug/flapjack"
  if [ ! -x "$BIN" ]; then
    printf 'ERROR: expected current-checkout binary at %s\n' "$BIN" >&2
    exit 1
  fi
}

start_server() {
  local data_dir="$TMP_ROOT/data"
  local log_path="$data_dir/server.log"
  mkdir -p "$data_dir"

  # FLAPJACK_ADVERTISE_ADDR gives the node replication intent
  # (`NodeConfig::has_replication_intent`), so `state.replication_manager` is
  # Some and the runtime-membership endpoints are live. Without intent
  # `add_cluster_peer` answers 400 and the baseline would fail for a setup
  # reason rather than proving anything about authorization. Intent is supplied
  # this way rather than via FLAPJACK_PEERS because a seeded-but-unreachable
  # peer makes pre-serve catch-up abort startup.
  # Advertise intent with no peer credential: startup requires
  # FLAPJACK_REPLICATION_API_KEY unless this override is set.
  env \
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_ADVERTISE_ADDR="$ADVERTISE_ADDR" \
    FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS=1 \
    FLAPJACK_DATA_DIR="$data_dir" \
    FLAPJACK_DISABLE_DASHBOARD=1 \
    FLAPJACK_NODE_ID="$NODE_ID" \
    "$BIN" --auto-port >"$log_path" 2>&1 &
  SERVER_PID=$!

  "$WAIT_FOR_FLAPJACK" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$log_path" \
    --retries 80 \
    --interval-seconds 0.5

  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$log_path" | head -1)"
  if [ -z "$port" ]; then
    echo 'ERROR: server became healthy but no auto-port was found in startup log' >&2
    cat "$log_path" >&2 || true
    exit 1
  fi
  BASE="http://127.0.0.1:${port}"
}

# Issue one request and record status + body. Emits PASS/FAIL against an exact
# expected status so a silently changed refusal cannot read as success.
check() {
  local label="$1" expected_status="$2" method="$3" path="$4" api_key="$5" data="${6:-}"
  local body_path="$TMP_ROOT/body_${CHECKS_RUN}.json"
  local -a args=(-sS -X "$method" -H "x-algolia-application-id: $PEER_APP_ID")

  if [ -n "$api_key" ]; then
    args+=(-H "x-algolia-api-key: $api_key")
  fi
  if [ -n "$data" ]; then
    args+=(-H 'content-type: application/json' --data "$data")
  fi

  CHECKS_RUN=$((CHECKS_RUN + 1))
  local status
  status="$(curl "${args[@]}" -o "$body_path" -w '%{http_code}' "${BASE}${path}")"

  printf -- '--- %s\n' "$label"
  printf '    request : %s %s\n' "$method" "$path"
  printf '    headers : x-algolia-application-id: %s' "$PEER_APP_ID"
  if [ -n "$api_key" ]; then
    printf ', x-algolia-api-key: <%s>' "$(credential_label "$api_key")"
  else
    printf ', (no x-algolia-api-key header)'
  fi
  printf '\n'
  printf '    status  : %s (expected %s)\n' "$status" "$expected_status"
  printf '    body    : %s\n' "$(head -c 600 "$body_path")"

  if [ "$status" = "$expected_status" ]; then
    printf '    result  : PASS\n'
  else
    CHECKS_FAILED=$((CHECKS_FAILED + 1))
    printf '    result  : FAIL\n'
  fi
  printf '\n'
}

credential_label() {
  case "$1" in
    "$ADMIN_KEY") printf 'the admin key replicas are configured with today' ;;
    "$RANDOM_KEY") printf 'an unrelated random key' ;;
    *) printf 'redacted' ;;
  esac
}

assert_body_contains() {
  local label="$1" needle="$2" body_index="$3"
  local body_path="$TMP_ROOT/body_${body_index}.json"
  CHECKS_RUN=$((CHECKS_RUN + 1))
  printf -- '--- %s\n' "$label"
  if grep -q "$needle" "$body_path"; then
    printf '    result  : PASS (found %s)\n\n' "$needle"
  else
    CHECKS_FAILED=$((CHECKS_FAILED + 1))
    printf '    result  : FAIL (missing %s in %s)\n\n' "$needle" "$(head -c 600 "$body_path")"
  fi
}

main() {
  require_tools
  TMP_ROOT="$(mktemp -d)"
  resolve_binary
  start_server

  printf 'SEC-G9 replication peer-credential RED baseline\n'
  printf 'captured against a running primary at %s\n' "$BASE"
  # Repo-relative: the worktree path is session-ephemeral and must not be baked
  # into committed evidence.
  printf 'binary: engine/%s\n' "${BIN#"$ENGINE_DIR"/}"
  printf 'node id: %s (replication intent via FLAPJACK_ADVERTISE_ADDR)\n' "$NODE_ID"
  printf 'credential under test: the admin key, exactly as PeerClient sends it today\n\n'

  printf '== Setup: one tenant with a real oplog ==\n'
  printf 'GET /internal/ops serves a per-tenant oplog, so an empty node would 404 for a\n'
  printf 'reason unrelated to authorization. Seed one document first.\n\n'
  check 'seed a tenant so an oplog exists' 201 POST "/1/indexes/$BASELINE_TENANT" \
    "$ADMIN_KEY" '{"objectID":"secg9-baseline-doc","title":"baseline"}'

  printf '== Direction 1: the replica credential works for replication ==\n\n'
  local ops_index=$CHECKS_RUN
  check 'replica pulls the oplog exactly as PeerClient does' 200 GET \
    "/internal/ops?tenant_id=$BASELINE_TENANT&since_seq=0" "$ADMIN_KEY"
  assert_body_contains 'oplog response carries the replication payload shape' '"ops"' "$ops_index"
  assert_body_contains 'oplog response is scoped to the seeded tenant' "$BASELINE_TENANT" "$ops_index"

  printf '== Direction 2: the SAME credential re-shapes cluster membership ==\n'
  printf 'This is the over-privilege. Nothing about replication needs this authority.\n\n'
  local add_index=$CHECKS_RUN
  check 'replica credential adds a bogus cluster peer' 200 POST '/internal/cluster/peers' \
    "$ADMIN_KEY" "{\"node_id\":\"$BOGUS_PEER_ID\",\"addr\":\"$BOGUS_PEER_ADDR\"}"
  assert_body_contains 'add-peer receipt names the bogus peer' "$BOGUS_PEER_ID" "$add_index"

  local status_index=$CHECKS_RUN
  check 'membership now contains the bogus peer' 200 GET '/internal/cluster/status' "$ADMIN_KEY"
  assert_body_contains 'cluster status shows the injected peer' "$BOGUS_PEER_ID" "$status_index"

  printf '== Reversal: leave the scratch primary clean ==\n\n'
  check 'remove the bogus cluster peer' 200 DELETE "/internal/cluster/peers/$BOGUS_PEER_ID" \
    "$ADMIN_KEY"

  local after_index=$CHECKS_RUN
  check 'membership after removal' 200 GET '/internal/cluster/status' "$ADMIN_KEY"
  CHECKS_RUN=$((CHECKS_RUN + 1))
  printf -- '--- bogus peer is gone from runtime membership\n'
  if grep -q "$BOGUS_PEER_ID" "$TMP_ROOT/body_${after_index}.json"; then
    CHECKS_FAILED=$((CHECKS_FAILED + 1))
    printf '    result  : FAIL (bogus peer still present after DELETE)\n\n'
  else
    printf '    result  : PASS\n\n'
  fi

  printf '== Controls: the boundary that already holds today ==\n\n'
  check 'an unrelated random key is refused on the replication route' 403 GET \
    "/internal/ops?tenant_id=$BASELINE_TENANT&since_seq=0" "$RANDOM_KEY"
  check 'a query-string-only credential is refused on the replication route' 403 GET \
    "/internal/ops?tenant_id=$BASELINE_TENANT&since_seq=0&x-algolia-api-key=$ADMIN_KEY" ''

  printf '== Summary ==\n'
  printf 'checks run: %s, failed: %s\n' "$CHECKS_RUN" "$CHECKS_FAILED"
  if [ "$CHECKS_FAILED" -gt 0 ]; then
    printf 'RESULT: FAIL\n'
    exit 1
  fi
  printf 'RESULT: PASS — over-privilege demonstrated and reversed\n'
  printf 'Refusals in this transcript are 403; this crate has no 401 auth path.\n'
}

main "$@"
