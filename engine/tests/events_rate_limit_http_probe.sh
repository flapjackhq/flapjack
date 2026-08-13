#!/usr/bin/env bash

# Running-system contract probe for bounded customer event ingress.
#
# The probe builds this checkout, starts one lane-owned listener, creates an
# event-capable key with an allowance of two requests per loopback IP, and proves
# the first excess request cannot reach either analytics side-effect store.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_FOR_FLAPJACK="$ENGINE_DIR/tests/common/wait_for_flapjack.sh"
HTTP_PROBE_LIB="$ENGINE_DIR/tests/common/http_probe_lib.sh"

# shellcheck source=common/http_probe_lib.sh
source "$HTTP_PROBE_LIB"

ADMIN_KEY="sec-events-2-live-admin-key"
APP_ID="sec-events-2-live-app"
INDEX_NAME="sec_events_2_live"
ACCEPTED_A="sec-events-2-live-accepted-a"
ACCEPTED_B="sec-events-2-live-accepted-b"
REJECTED="sec-events-2-live-rejected-first-excess"
EXPECTED_CHECKS=6

EXPECTED_FIRST_EXCESS_STATUS="${SEC_EVENTS_2_EXPECT_FIRST_EXCESS_STATUS:-429}"
EXPECTED_FIRST_EXCESS_BODY='{"message":"Too many requests per IP per hour","status":429}'
if [ -n "${SEC_EVENTS_2_EXPECT_FIRST_EXCESS_BODY+x}" ]; then
  EXPECTED_FIRST_EXCESS_BODY="$SEC_EVENTS_2_EXPECT_FIRST_EXCESS_BODY"
fi
EXPECTED_DEBUG_REJECTED="${SEC_EVENTS_2_EXPECT_DEBUG_REJECTED:-absent}"
EXPECTED_ANALYTICS_REJECTED="${SEC_EVENTS_2_EXPECT_ANALYTICS_REJECTED:-absent}"
EXPECTED_DEBUG_ACCEPTED="${SEC_EVENTS_2_EXPECT_DEBUG_ACCEPTED:-status=200,count=2,ids=$ACCEPTED_A,$ACCEPTED_B}"
EXPECTED_ANALYTICS_ACCEPTED="${SEC_EVENTS_2_EXPECT_ANALYTICS_ACCEPTED:-status=200,rows=2,hits=$ACCEPTED_A=1,$ACCEPTED_B=1}"

BIN=""
TMP_ROOT=""
SERVER_PID=""
BASE=""
TESTS_RUN=0
TESTS_FAILED=0

cleanup() {
  local script_exit_code=$?
  if ! http_probe_cleanup "$script_exit_code" "SEC-EVENTS-2 event rate limit"; then
    script_exit_code=2
  fi
  trap - EXIT
  exit "$script_exit_code"
}

setup_error() {
  printf 'ERROR: %s\n' "$1" >&2
  exit 2
}

require_tools() {
  local tool
  if ! http_probe_validate_stop_config; then
    exit 2
  fi
  for tool in cargo curl grep mktemp python3 sed; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      setup_error "required tool not found: $tool"
    fi
  done
  if [ ! -x "$WAIT_FOR_FLAPJACK" ]; then
    setup_error "wait helper is not executable: $WAIT_FOR_FLAPJACK"
  fi
}

# Run the shared resolver in a subshell so its build errors can be translated to
# setup exit 2. The inherited FLAPJACK_BIN is never read or trusted.
resolve_binary() {
  if ! (http_probe_resolve_default_binary); then
    setup_error "cargo build -p flapjack-server failed"
  fi
  BIN="$(http_probe_target_dir)/debug/flapjack"
  if [ ! -x "$BIN" ]; then
    setup_error "current-checkout binary is not executable: $BIN"
  fi
}

start_server() {
  local data_dir="$TMP_ROOT/data"
  local log_path="$data_dir/server.log"
  local port
  mkdir -p "$data_dir" "$TMP_ROOT/analytics"

  env \
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$data_dir" \
    FLAPJACK_DISABLE_DASHBOARD=1 \
    FLAPJACK_ANALYTICS_ENABLED=true \
    FLAPJACK_ANALYTICS_DIR="$TMP_ROOT/analytics" \
    "$BIN" --auto-port >"$log_path" 2>&1 &
  SERVER_PID=$!

  if ! "$WAIT_FOR_FLAPJACK" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$log_path" \
    --retries 80 \
    --interval-seconds 0.5; then
    setup_error "listener did not become ready; see $log_path"
  fi

  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$log_path" | head -1)"
  if [ -z "$port" ]; then
    setup_error "healthy listener did not report its auto-assigned port"
  fi
  BASE="http://127.0.0.1:${port}"
}

curl_json_as_key() {
  local method="$1" url="$2" key="$3" body="$4" body_path="$5"
  local status
  if ! status="$(curl -sS \
    -X "$method" \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $key" \
    -H "content-type: application/json" \
    -o "$body_path" \
    -w '%{http_code}' \
    --data "$body" \
    "$url")"; then
    setup_error "transport failed for $method $url"
  fi
  printf '%s\n' "$status"
}

curl_get_as_key() {
  local url="$1" key="$2" body_path="$3"
  local status
  if ! status="$(curl -sS \
    -H "x-algolia-application-id: $APP_ID" \
    -H "x-algolia-api-key: $key" \
    -o "$body_path" \
    -w '%{http_code}' \
    "$url")"; then
    setup_error "transport failed for GET $url"
  fi
  printf '%s\n' "$status"
}

canonical_json() {
  python3 - "$1" <<'PY'
import json
import sys

try:
    with open(sys.argv[1]) as handle:
        value = json.load(handle)
except Exception as error:  # any malformed setup response is indeterminate
    print(f"malformed:{type(error).__name__}")
    raise SystemExit(2)
print(json.dumps(value, separators=(",", ":"), sort_keys=True))
PY
}

require_setup_response() {
  local description="$1" expected_status="$2" actual_status="$3" body_path="$4"
  local expected_body="$5" actual_body
  if [ "$actual_status" != "$expected_status" ]; then
    setup_error "$description expected status $expected_status, got $actual_status"
  fi
  if ! actual_body="$(canonical_json "$body_path")"; then
    setup_error "$description returned malformed JSON"
  fi
  if [ "$actual_body" != "$expected_body" ]; then
    setup_error "$description expected body $expected_body, got $actual_body"
  fi
}

json_key() {
  python3 - "$1" <<'PY'
import json
import sys

try:
    with open(sys.argv[1]) as handle:
        key = json.load(handle)["key"]
    if not isinstance(key, str) or not key:
        raise ValueError("empty key")
except Exception as error:
    print(f"malformed:{type(error).__name__}", file=sys.stderr)
    raise SystemExit(2)
print(key)
PY
}

event_body() {
  local event_name="$1" user_token="$2" object_id="$3" timestamp_ms="$4"
  printf '{"events":[{"eventType":"click","eventName":"%s","index":"%s","userToken":"%s","objectIDs":["%s"],"positions":[1],"timestamp":%s}]}' \
    "$event_name" "$INDEX_NAME" "$user_token" "$object_id" "$timestamp_ms"
}

debug_summaries() {
  local body_path="$1" status="$2"
  python3 - "$body_path" "$status" "$REJECTED" <<'PY'
import json
import sys

path, status, rejected = sys.argv[1:]
try:
    with open(path) as handle:
        payload = json.load(handle)
    events = payload["events"]
    count = payload["count"]
    if not isinstance(events, list) or type(count) is not int or count != len(events):
        raise TypeError("invalid debug envelope")
    object_ids = []
    for event in events:
        values = event["objectIds"]
        if not isinstance(values, list) or not all(isinstance(value, str) for value in values):
            raise TypeError("invalid debug objectIds")
        object_ids.extend(values)
    accepted = f"status={status},count={count},ids={','.join(sorted(object_ids))}"
    rejected_fact = "present" if rejected in object_ids else "absent"
except Exception as error:
    accepted = f"status={status},malformed={type(error).__name__}"
    rejected_fact = "malformed"
print(f"{accepted}\t{rejected_fact}")
PY
}

analytics_summaries() {
  local body_path="$1" status="$2"
  python3 - "$body_path" "$status" "$REJECTED" <<'PY'
import json
import sys

path, status, rejected = sys.argv[1:]
try:
    with open(path) as handle:
        payload = json.load(handle)
    rows = payload["hits"]
    if not isinstance(rows, list):
        raise TypeError("invalid analytics hits")
    counts = {}
    for row in rows:
        object_ids = json.loads(row["hit"])
        count = row["count"]
        # Each request sends one object ID. Requiring the same row shape prevents
        # a combined row plus an empty row from satisfying only totals/counts.
        if (
            not isinstance(object_ids, list)
            or len(object_ids) != 1
            or not all(isinstance(value, str) for value in object_ids)
        ):
            raise TypeError("invalid analytics object IDs")
        if type(count) is not int:
            raise TypeError("invalid analytics count")
        for object_id in object_ids:
            counts[object_id] = counts.get(object_id, 0) + count
    joined = ",".join(f"{object_id}={counts[object_id]}" for object_id in sorted(counts))
    accepted = f"status={status},rows={len(rows)},hits={joined}"
    rejected_fact = "present" if rejected in counts else "absent"
except Exception as error:
    accepted = f"status={status},malformed={type(error).__name__}"
    rejected_fact = "malformed"
print(f"{accepted}\t{rejected_fact}")
PY
}

record_check() {
  http_probe_record_check "$@"
}

main() {
  local timestamp_ms analytics_date
  local key_status event_status_a event_status_b excess_status debug_status flush_status analytics_status
  local key debug_facts debug_accepted debug_rejected analytics_facts analytics_accepted analytics_rejected

  trap cleanup EXIT
  require_tools
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fj-sec-events-2-probe.XXXXXX")"
  resolve_binary
  start_server

  timestamp_ms="$(python3 - <<'PY'
import time
print(time.time_ns() // 1_000_000)
PY
)"
  analytics_date="$(python3 - "$timestamp_ms" <<'PY'
import datetime
import sys
print(datetime.datetime.fromtimestamp(int(sys.argv[1]) / 1000, datetime.timezone.utc).date())
PY
)"

  key_status="$(curl_json_as_key POST "$BASE/1/keys" "$ADMIN_KEY" \
    "{\"acl\":[\"search\"],\"indexes\":[\"$INDEX_NAME\"],\"maxQueriesPerIPPerHour\":2,\"description\":\"SEC-EVENTS-2 live bounded ingress key\"}" \
    "$TMP_ROOT/key.json")"
  if [ "$key_status" != "200" ]; then
    setup_error "event key creation expected status 200, got $key_status"
  fi
  if ! key="$(json_key "$TMP_ROOT/key.json")"; then
    setup_error "event key creation did not return a usable key"
  fi

  event_status_a="$(curl_json_as_key POST "$BASE/1/events" "$key" \
    "$(event_body "SEC Events 2 Live Accepted A" "sec_events_2_live_user_a" "$ACCEPTED_A" "$timestamp_ms")" \
    "$TMP_ROOT/accepted_a.json")"
  require_setup_response "first allowed event" 200 "$event_status_a" "$TMP_ROOT/accepted_a.json" \
    '{"message":"OK","status":200}'

  event_status_b="$(curl_json_as_key POST "$BASE/1/events" "$key" \
    "$(event_body "SEC Events 2 Live Accepted B" "sec_events_2_live_user_b" "$ACCEPTED_B" "$timestamp_ms")" \
    "$TMP_ROOT/accepted_b.json")"
  require_setup_response "second allowed event" 200 "$event_status_b" "$TMP_ROOT/accepted_b.json" \
    '{"message":"OK","status":200}'

  excess_status="$(curl_json_as_key POST "$BASE/1/events" "$key" \
    "$(event_body "SEC Events 2 Live Rejected First Excess" "sec_events_2_live_user_rejected" "$REJECTED" "$timestamp_ms")" \
    "$TMP_ROOT/first_excess.json")"

  debug_status="$(curl_get_as_key "$BASE/1/events/debug?index=$INDEX_NAME" "$ADMIN_KEY" \
    "$TMP_ROOT/debug.json")"

  flush_status="$(curl_json_as_key POST "$BASE/2/analytics/flush" "$ADMIN_KEY" '{}' \
    "$TMP_ROOT/flush.json")"
  require_setup_response "analytics flush" 200 "$flush_status" "$TMP_ROOT/flush.json" \
    '{"status":"ok"}'

  analytics_status="$(curl_get_as_key \
    "$BASE/2/hits?index=$INDEX_NAME&startDate=$analytics_date&endDate=$analytics_date&limit=10" \
    "$ADMIN_KEY" "$TMP_ROOT/analytics.json")"

  debug_facts="$(debug_summaries "$TMP_ROOT/debug.json" "$debug_status")"
  IFS=$'\t' read -r debug_accepted debug_rejected <<<"$debug_facts"
  analytics_facts="$(analytics_summaries "$TMP_ROOT/analytics.json" "$analytics_status")"
  IFS=$'\t' read -r analytics_accepted analytics_rejected <<<"$analytics_facts"

  printf 'SEC_EVENTS_2_DENOMINATOR key_status=%s allowed_statuses=%s,%s flush_status=%s\n' \
    "$key_status" "$event_status_a" "$event_status_b" "$flush_status"
  record_check FIRST_EXCESS_STATUS "$EXPECTED_FIRST_EXCESS_STATUS" "$excess_status"
  record_check FIRST_EXCESS_BODY "$EXPECTED_FIRST_EXCESS_BODY" \
    "$(canonical_json "$TMP_ROOT/first_excess.json" 2>/dev/null || printf 'malformed')"
  record_check DEBUG_REJECTED_ABSENT "$EXPECTED_DEBUG_REJECTED" "$debug_rejected"
  record_check ANALYTICS_REJECTED_ABSENT "$EXPECTED_ANALYTICS_REJECTED" "$analytics_rejected"
  record_check DEBUG_ACCEPTED_EXACT "$EXPECTED_DEBUG_ACCEPTED" "$debug_accepted"
  record_check ANALYTICS_ACCEPTED_EXACT "$EXPECTED_ANALYTICS_ACCEPTED" "$analytics_accepted"

  if [ "$TESTS_RUN" -ne "$EXPECTED_CHECKS" ]; then
    setup_error "expected $EXPECTED_CHECKS contract checks, ran $TESTS_RUN"
  fi
  if [ "$TESTS_FAILED" -ne 0 ]; then
    exit 1
  fi

  printf 'SEC_EVENTS_2_LIVE allowance=2 allowed=2 first_excess_status=429 debug_rejected=absent analytics_rejected=absent verdict=PASS\n'
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
