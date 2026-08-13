#!/usr/bin/env bash
#
# Release-profile contract for explicitly opted-in migration loopback sources.
#
# This is deliberately a thin consumer of the provider-parity probe, which owns
# source containers, seeding, request helpers, polling, search, and cleanup.
# Exit 1 is reserved for product-contract reds; build, Docker, and readiness
# failures exit 2 as indeterminate harness failures.

set -euo pipefail

CONTRACT_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=source_migration_provider_parity_http_probe.sh
source "$CONTRACT_SCRIPT_DIR/source_migration_provider_parity_http_probe.sh"

readonly RELEASE_LOOPBACK_EXPECTED_MEILI_MESSAGE="Meilisearch Cloud endpoint is not allowed"
readonly RELEASE_LOOPBACK_EXPECTED_TYPESENSE_MESSAGE="Typesense Cloud endpoint is not allowed"
readonly RELEASE_LOOPBACK_EXPECTED_ARM_COUNT=26
readonly DNS_CANARY_HOST="release-loopback-dns-canary.invalid"

CONTRACT_REDS=()
CONTRACT_PASSES=0
REQUEST_CANARY_PID=""
REQUEST_CANARY_BASE=""
DNS_CANARY_LOG=""
DNS_CANARY_LIB=""
DNS_CANARY_PROBE=""
DNS_CANARY_ENV=()
CLEANUP_FAILURE_OVERRIDES_EXIT=1

die_indeterminate() {
  printf 'MIGRATION_RELEASE_LOOPBACK_CONTRACT=INDETERMINATE reason=%s\n' "$1" >&2
  if [ -n "$LOG" ] && [ -f "$LOG" ]; then
    tail -80 "$LOG" >&2 || true
  fi
  exit 2
}

fail_red() {
  CHECKS_FAILED=$((CHECKS_FAILED + 1))
  printf 'MIGRATION_RELEASE_LOOPBACK_CONTRACT=RED %s\n' "$1" >&2
  exit 1
}

record_arm() {
  local label="$1"
  shift
  local rc
  set +e
  ( set -e; "$@" )
  rc=$?
  set -e
  case "$rc" in
    0)
      CONTRACT_PASSES=$((CONTRACT_PASSES + 1))
      printf 'CONTRACT_ARM=%s result=PASS\n' "$label"
      ;;
    1)
      CONTRACT_REDS+=("$label")
      printf 'CONTRACT_ARM=%s result=RED\n' "$label" >&2
      ;;
    *)
      die_indeterminate "contract_arm_indeterminate label=${label} rc=${rc}"
      ;;
  esac
}

assert_expected_contract_arm_count() {
  [ "$CONTRACT_PASSES" -eq "$RELEASE_LOOPBACK_EXPECTED_ARM_COUNT" ] \
    || die_indeterminate "contract_arm_count_mismatch expected=${RELEASE_LOOPBACK_EXPECTED_ARM_COUNT} actual=${CONTRACT_PASSES}"
}

build_release_binary() {
  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server --release >"$TMP/release_build.log" 2>&1); then
    tail -40 "$TMP/release_build.log" >&2 || true
    die_indeterminate 'flapjack_server_release_build_failed'
  fi
  BIN="$(target_dir)/release/flapjack"
  [ -x "$BIN" ] || die_indeterminate "release_binary_missing path=${BIN}"
  case "$BIN" in
    */release/flapjack) ;;
    *) die_indeterminate "release_binary_profile_mismatch path=${BIN}" ;;
  esac
  printf 'RELEASE_BINARY=%s\n' "$BIN"
}

build_dns_canary() {
  local os_name
  command -v cc >/dev/null 2>&1 || die_indeterminate 'required_tool_missing tool=cc'
  DNS_CANARY_LOG="$TMP/dns_canary.log"
  : >"$DNS_CANARY_LOG"
  cat >"$TMP/dns_canary.c" <<'EOF_DNS_CANARY'
#define _GNU_SOURCE
#include <dlfcn.h>
#include <netdb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef int (*getaddrinfo_fn)(const char *, const char *, const struct addrinfo *, struct addrinfo **);

static void record_dns_canary(const char *node) {
  const char *log_path = getenv("FJ_DNS_CANARY_LOG");
  if (node != NULL && log_path != NULL && strstr(node, "release-loopback-dns-canary.invalid") != NULL) {
    FILE *log = fopen(log_path, "a");
    if (log != NULL) {
      fprintf(log, "%s\n", node);
      fclose(log);
    }
  }
}

#ifdef __APPLE__
extern int getaddrinfo(const char *, const char *, const struct addrinfo *, struct addrinfo **);

static int canary_getaddrinfo(const char *node, const char *service, const struct addrinfo *hints, struct addrinfo **res) {
  record_dns_canary(node);
  return getaddrinfo(node, service, hints, res);
}

struct interpose_t {
  const void *replacement;
  const void *replacee;
};

__attribute__((used)) static const struct interpose_t interposers[] __attribute__((section("__DATA,__interpose"))) = {
  { (const void *)canary_getaddrinfo, (const void *)getaddrinfo }
};
#else
int getaddrinfo(const char *node, const char *service, const struct addrinfo *hints, struct addrinfo **res) {
  static getaddrinfo_fn real_getaddrinfo = NULL;

  if (real_getaddrinfo == NULL) {
    real_getaddrinfo = (getaddrinfo_fn)dlsym(RTLD_NEXT, "getaddrinfo");
  }
  record_dns_canary(node);
  return real_getaddrinfo(node, service, hints, res);
}
#endif
EOF_DNS_CANARY
  cat >"$TMP/dns_canary_probe.c" <<'EOF_DNS_PROBE'
#include <netdb.h>

int main(void) {
  struct addrinfo *res = 0;
  int rc = getaddrinfo("release-loopback-dns-canary.invalid", "80", 0, &res);
  if (rc == 0 && res != 0) {
    freeaddrinfo(res);
  }
  return 0;
}
EOF_DNS_PROBE
  os_name="$(uname -s)"
  case "$os_name" in
    Darwin)
      DNS_CANARY_LIB="$TMP/dns_canary.dylib"
      cc -dynamiclib -o "$DNS_CANARY_LIB" "$TMP/dns_canary.c" \
        >"$TMP/dns_canary_build.log" 2>&1 \
        || { tail -40 "$TMP/dns_canary_build.log" >&2 || true; die_indeterminate 'dns_canary_build_failed'; }
      DNS_CANARY_ENV=(DYLD_INSERT_LIBRARIES="$DNS_CANARY_LIB")
      ;;
    Linux)
      DNS_CANARY_LIB="$TMP/dns_canary.so"
      cc -shared -fPIC -o "$DNS_CANARY_LIB" "$TMP/dns_canary.c" -ldl \
        >"$TMP/dns_canary_build.log" 2>&1 \
        || { tail -40 "$TMP/dns_canary_build.log" >&2 || true; die_indeterminate 'dns_canary_build_failed'; }
      DNS_CANARY_ENV=(LD_PRELOAD="$DNS_CANARY_LIB")
      ;;
    *)
      die_indeterminate "dns_canary_unsupported_os os=${os_name}"
      ;;
  esac
  DNS_CANARY_PROBE="$TMP/dns_canary_probe"
  cc -o "$DNS_CANARY_PROBE" "$TMP/dns_canary_probe.c" \
    >"$TMP/dns_canary_probe_build.log" 2>&1 \
    || { tail -40 "$TMP/dns_canary_probe_build.log" >&2 || true; die_indeterminate 'dns_canary_probe_build_failed'; }
  assert_dns_canary_self_test
}

start_request_canary() {
  cat >"$TMP/request_canary.py" <<'PY'
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

count = 0

class Handler(BaseHTTPRequestHandler):
    def _write_json(self, payload, status=200):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _record(self):
        global count
        count += 1
        self._write_json({"canary": True, "count": count})

    def do_GET(self):
        global count
        if self.path == "/__count":
            self._write_json({"count": count})
        elif self.path == "/__reset":
            count = 0
            self._write_json({"count": count})
        else:
            self._record()

    def do_POST(self):
        global count
        if self.path == "/__reset":
            count = 0
            self._write_json({"count": count})
        else:
            self._record()

    def do_PUT(self):
        self._record()

    def do_PATCH(self):
        self._record()

    def do_DELETE(self):
        self._record()

    def log_message(self, *_):
        pass

server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
PY
  python3 "$TMP/request_canary.py" >"$TMP/request_canary.port" 2>"$TMP/request_canary.log" &
  REQUEST_CANARY_PID=$!
  local attempt port=""
  for attempt in $(seq 1 80); do
    kill -0 "$REQUEST_CANARY_PID" 2>/dev/null || die_indeterminate 'request_canary_exited'
    port="$(sed -n '1p' "$TMP/request_canary.port")"
    [ -n "$port" ] && break
    sleep 0.1
  done
  [ -n "$port" ] || die_indeterminate 'request_canary_port_missing'
  REQUEST_CANARY_BASE="http://127.0.0.1:${port}"
  EXTRA_OWNED_PIDS+=("request_canary:${REQUEST_CANARY_PID}")
  assert_request_canary_self_test
}

start_release_server() {
  local label="$1" meili_opt_in="$2" typesense_opt_in="$3"
  local algolia_base="${4:-$ALGOLIA_STUB_BASE}"
  local data_dir="$TMP/data_${label}" port
  local env_args=(
    -u FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND
    -u FLAPJACK_BIND_ADDR
    -u FLAPJACK_NO_AUTH
    -u FLAPJACK_PORT
    -u FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK
    -u FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK
    "FLAPJACK_ADMIN_KEY=$PROBE_ADMIN_KEY"
    "FLAPJACK_DATA_DIR=$data_dir"
    "FLAPJACK_TEST_ALGOLIA_BASE_URL=$algolia_base"
    "FJ_DNS_CANARY_LOG=$DNS_CANARY_LOG"
  )

  terminate_owned_pid flapjack_server "$SERVER_PID"
  SERVER_PID=""
  BASE=""
  mkdir -p "$data_dir"
  LOG="$TMP/server_${label}.log"
  [ -z "$meili_opt_in" ] || env_args+=("FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=$meili_opt_in")
  [ -z "$typesense_opt_in" ] || env_args+=("FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=$typesense_opt_in")
  env_args+=("${DNS_CANARY_ENV[@]}")
  env "${env_args[@]}" "$BIN" --auto-port >"$LOG" 2>&1 &
  SERVER_PID=$!

  if ! "$WAIT_HELPER" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$LOG" \
    --retries 80 \
    --interval-seconds 0.5; then
    die_indeterminate "release_server_readiness_failed label=${label}"
  fi
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9][0-9]*\).*/\1/p' "$LOG" | head -1)"
  [ -n "$port" ] || die_indeterminate "release_auto_port_missing label=${label}"
  BASE="http://127.0.0.1:${port}"
  printf 'RELEASE_SERVER=%s base=%s meili_opt_in=%s typesense_opt_in=%s\n' \
    "$label" "$BASE" "${meili_opt_in:-unset}" "${typesense_opt_in:-unset}"
}

write_request_canary_count() {
  local label="$1" dest="$2"
  local out="$TMP/${label}_request_canary_count.json"
  curl -sS --connect-timeout 1 --max-time 2 "$REQUEST_CANARY_BASE/__count" >"$out" \
    || die_indeterminate "request_canary_count_failed label=${label}"
  jq -er '.count | select(type == "number")' "$out" >"$dest" \
    || die_indeterminate "request_canary_count_invalid label=${label}"
}

reset_request_canary() {
  local label="$1"
  local out="$TMP/${label}_request_canary_reset.json"
  curl -sS --connect-timeout 1 --max-time 2 -X POST "$REQUEST_CANARY_BASE/__reset" >"$out" \
    || die_indeterminate "request_canary_reset_failed label=${label}"
  jq -e '.count == 0' "$out" >/dev/null \
    || die_indeterminate "request_canary_reset_invalid label=${label}"
}

reset_dns_canary() {
  : >"$DNS_CANARY_LOG"
}

assert_request_canary_self_test() {
  local count count_file
  reset_request_canary request_canary_self_test
  curl -sS --connect-timeout 1 --max-time 2 "$REQUEST_CANARY_BASE/self-test" \
    >"$TMP/request_canary_self_test_hit.json" \
    || die_indeterminate 'request_canary_self_test_hit_failed'
  count_file="$TMP/request_canary_self_test_count.txt"
  write_request_canary_count request_canary_self_test "$count_file"
  count="$(cat "$count_file")"
  [ "$count" = 1 ] || die_indeterminate "request_canary_self_test_count_mismatch count=${count}"
  reset_request_canary request_canary_self_test_after_hit
  count_file="$TMP/request_canary_self_test_after_reset_count.txt"
  write_request_canary_count request_canary_self_test_after_reset "$count_file"
  count="$(cat "$count_file")"
  [ "$count" = 0 ] || die_indeterminate "request_canary_self_test_reset_mismatch count=${count}"
  printf 'REQUEST_CANARY_SELF_TEST=PASS\n'
}

assert_dns_canary_self_test() {
  reset_dns_canary
  env FJ_DNS_CANARY_LOG="$DNS_CANARY_LOG" "${DNS_CANARY_ENV[@]}" "$DNS_CANARY_PROBE" \
    >"$TMP/dns_canary_probe.out" 2>"$TMP/dns_canary_probe.err" \
    || die_indeterminate 'dns_canary_probe_failed'
  grep -Fxq "$DNS_CANARY_HOST" "$DNS_CANARY_LOG" \
    || die_indeterminate 'dns_canary_self_test_no_observed_resolution'
  reset_dns_canary
  printf 'DNS_CANARY_SELF_TEST=PASS host=%s\n' "$DNS_CANARY_HOST"
}

assert_server_dns_canary_positive_control() {
  local settings_body chat_body status curl_exit
  reset_dns_canary
  start_release_server dns_positive_control 1 1
  settings_body="$(jq -cn --arg base_url "http://${DNS_CANARY_HOST}:${REQUEST_CANARY_BASE##*:}" \
    '{mode:"neuralSearch",userData:{aiProvider:{baseUrl:$base_url,apiKey:"dns-positive-control"}}}')"
  set +e
  status="$(curl -sS --connect-timeout 2 --max-time 20 \
    -o "$TMP/server_dns_positive_control_settings.json" -w '%{http_code}' -X PUT \
    -H 'Content-Type: application/json' \
    -H "x-algolia-application-id: ${PROBE_APPLICATION_ID}" \
    -H "x-algolia-api-key: ${PROBE_ADMIN_KEY}" \
    --data "$settings_body" "${BASE}/1/indexes/dns_canary_control/settings")"
  curl_exit=$?
  set -e
  [ "$curl_exit" -eq 0 ] \
    || die_indeterminate "server_dns_positive_control_settings_transport actual=${curl_exit}"
  [ "$status" = 200 ] \
    || die_indeterminate "server_dns_positive_control_settings_status actual=${status} body=$(jq -c . "$TMP/server_dns_positive_control_settings.json" 2>/dev/null || true)"
  chat_body="$(jq -cn '{query:"dns positive control"}')"
  set +e
  status="$(curl -sS --connect-timeout 2 --max-time 20 \
    -o "$TMP/server_dns_positive_control_chat.json" -w '%{http_code}' -X POST \
    -H 'Content-Type: application/json' \
    -H "x-algolia-application-id: ${PROBE_APPLICATION_ID}" \
    -H "x-algolia-api-key: ${PROBE_ADMIN_KEY}" \
    --data "$chat_body" "${BASE}/1/indexes/dns_canary_control/chat")"
  curl_exit=$?
  set -e
  [ "$curl_exit" -eq 0 ] \
    || die_indeterminate "server_dns_positive_control_chat_transport actual=${curl_exit}"
  grep -Fxq "$DNS_CANARY_HOST" "$DNS_CANARY_LOG" \
    || die_indeterminate "server_dns_positive_control_no_observed_resolution status=${status} body=$(jq -c . "$TMP/server_dns_positive_control_chat.json" 2>/dev/null || true)"
  reset_dns_canary
  printf 'DNS_CANARY_SERVER_POSITIVE_CONTROL=PASS status=%s host=%s\n' "$status" "$DNS_CANARY_HOST"
}

assert_unreachable_request_canary_is_indeterminate() {
  local original_base="$REQUEST_CANARY_BASE" rc stderr_file
  REQUEST_CANARY_BASE='http://127.0.0.1:1'
  stderr_file="$TMP/request_canary_unreachable_negative_control.err"
  set +e
  (
    set -e
    record_arm request_canary_unreachable_negative_control write_request_canary_count \
      request_canary_unreachable_negative_control \
      "$TMP/request_canary_unreachable_negative_control_count.txt"
  ) >"$TMP/request_canary_unreachable_negative_control.out" 2>"$stderr_file"
  rc=$?
  set -e
  REQUEST_CANARY_BASE="$original_base"
  [ "$rc" -eq 2 ] \
    || die_indeterminate "request_canary_unreachable_negative_control_wrong_exit expected=2 actual=${rc}"
  grep -Fq 'contract_arm_indeterminate label=request_canary_unreachable_negative_control rc=2' "$stderr_file" \
    || die_indeterminate 'request_canary_unreachable_negative_control_missing_indeterminate_evidence'
  ! grep -Fq 'result=RED' "$stderr_file" \
    || die_indeterminate 'request_canary_unreachable_negative_control_reported_red'
  printf 'REQUEST_CANARY_UNREACHABLE_NEGATIVE_CONTROL=PASS rc=%s\n' "$rc"
}

assert_exact_refusal_without_dns() {
  local provider="$1" label="$2" endpoint="$3" expected_message="$4"
  reset_dns_canary
  assert_exact_refusal "$provider" "$label" "$endpoint" "$expected_message"
  [ ! -s "$DNS_CANARY_LOG" ] \
    || fail_red "opted_in_hostname_resolved provider=${provider} case=${label} log=$(tr '\n' ',' <"$DNS_CANARY_LOG")"
}

assert_exact_refusal() {
  local provider="$1" label="$2" endpoint="$3" expected_message="$4"
  local body field
  case "$provider" in
    meilisearch)
      field=endpoint
      body="$(jq -cn --arg endpoint "$endpoint" --arg key "$MEILI_KEY" \
        '{endpoint:$endpoint,apiKey:$key}')"
      ;;
    typesense)
      field=node
      body="$(jq -cn --arg node "$endpoint" --arg key "$TYPESENSE_KEY" \
        '{node:$node,apiKey:$key}')"
      ;;
    *) die_indeterminate "unknown_refusal_provider provider=${provider}" ;;
  esac
  served_discovery_request "$label" "/1/migrations/${provider}/list-indexes" "$body" 400
  jq -e --arg message "$expected_message" \
    '. == {message:$message,status:400}' "$TMP/${label}.json" >/dev/null \
    || fail_red "refusal_body_mismatch provider=${provider} case=${label} field=${field} body=$(jq -c . "$TMP/${label}.json" 2>/dev/null || true)"
}

assert_meilisearch_discovery() {
  served_discovery_request release_meilisearch_discovery \
    '/1/migrations/meilisearch/list-indexes?offset=0&limit=10' \
    "{\"endpoint\":\"http://127.0.0.1:${MEILI_PORT}\",\"apiKey\":\"${MEILI_KEY}\"}" 200
  assert_meilisearch_discovery_body "$TMP/release_meilisearch_discovery.json" \
    'positive_discovery_mismatch provider=meilisearch'
}

assert_typesense_discovery() {
  served_discovery_request release_typesense_discovery \
    '/1/migrations/typesense/list-indexes?offset=0&limit=2' \
    "{\"node\":\"http://127.0.0.1:${TYPESENSE_PORT}\",\"apiKey\":\"${TYPESENSE_KEY}\"}" 200
  assert_typesense_discovery_body "$TMP/release_typesense_discovery.json" \
    'positive_discovery_mismatch provider=typesense'
}

assert_meilisearch_migration() {
  run_served_migration meilisearch release_meilisearch \
    "{\"endpoint\":\"http://127.0.0.1:${MEILI_PORT}\",\"apiKey\":\"${MEILI_KEY}\",\"sourceIndex\":\"configured_pk\"}"
  served_search release_meilisearch_search configured_pk ''
  assert_meilisearch_landed_documents "$TMP/release_meilisearch_search.json" \
    'positive_search_mismatch provider=meilisearch'
}

assert_typesense_migration() {
  run_served_migration typesense release_typesense_categories \
    "{\"node\":\"http://127.0.0.1:${TYPESENSE_PORT}\",\"apiKey\":\"${TYPESENSE_KEY}\",\"sourceIndex\":\"${TYPESENSE_CATEGORIES}\",\"sourceWriteFrozen\":true}"
  run_served_migration typesense release_typesense_products \
    "{\"node\":\"http://127.0.0.1:${TYPESENSE_PORT}\",\"apiKey\":\"${TYPESENSE_KEY}\",\"sourceIndex\":\"${TYPESENSE_PRODUCTS}\",\"sourceWriteFrozen\":true}"
  served_search release_typesense_categories_search "$TYPESENSE_CATEGORIES" ''
  assert_typesense_categories_landed_documents "$TMP/release_typesense_categories_search.json" \
    'positive_search_mismatch provider=typesense collection=categories'
  served_search release_typesense_products_search "$TYPESENSE_PRODUCTS" ''
  assert_typesense_products_landed_documents "$TMP/release_typesense_products_search.json" \
    'positive_search_mismatch provider=typesense collection=products'
}

run_refusal_matrix() {
  local provider="$1" port="$2" message="$3" field endpoint
  case "$provider" in
    meilisearch) field=endpoint ;;
    typesense) field=node ;;
  esac
  while IFS='|' read -r label endpoint; do
    if [ "$label" = unresolvable_hostname ]; then
      record_arm "${provider}_${label}" assert_exact_refusal_without_dns \
        "$provider" "${provider}_${label}" "$endpoint" "$message"
    else
      record_arm "${provider}_${label}" assert_exact_refusal \
        "$provider" "${provider}_${label}" "$endpoint" "$message"
    fi
  done <<EOF
localhost|http://localhost:${port}
userinfo|http://user:password@127.0.0.1:${port}
query|http://127.0.0.1:${port}?canary=1
fragment|http://127.0.0.1:${port}#canary
non_root_path|http://127.0.0.1:${port}/collections
private_non_loopback|http://10.0.0.1:${port}
unresolvable_hostname|http://${DNS_CANARY_HOST}:${port}
EOF
}

assert_no_canary_activity() {
  local provider="$1" label="$2" endpoint="$3" message="$4" count count_file
  reset_request_canary "$label"
  reset_dns_canary
  assert_exact_refusal "$provider" "$label" "$endpoint" "$message"
  count_file="$TMP/${label}_request_canary_count.txt"
  write_request_canary_count "$label" "$count_file"
  count="$(cat "$count_file")"
  [ "$count" = 0 ] \
    || fail_red "disabled_opt_in_reached_request_canary provider=${provider} case=${label} count=${count}"
  [ ! -s "$DNS_CANARY_LOG" ] \
    || fail_red "disabled_opt_in_resolved_hostname provider=${provider} case=${label} log=$(tr '\n' ',' <"$DNS_CANARY_LOG")"
}

main() {
  require_tools
  TMP="$(mktemp -d "${TMPDIR:-/tmp}/fj_migration_release_loopback.XXXXXX")"
  configure_source_provider_owner_token
  build_dns_canary
  start_request_canary
  build_release_binary
  start_discovery_upstreams

  assert_server_dns_canary_positive_control
  assert_unreachable_request_canary_is_indeterminate

  start_release_server opted_in 1 1
  record_arm meilisearch_positive_discovery assert_meilisearch_discovery
  record_arm typesense_positive_discovery assert_typesense_discovery
  record_arm meilisearch_positive_submit_ack_search assert_meilisearch_migration
  record_arm typesense_positive_submit_ack_search assert_typesense_migration
  run_refusal_matrix meilisearch "$MEILI_PORT" "$RELEASE_LOOPBACK_EXPECTED_MEILI_MESSAGE"
  run_refusal_matrix typesense "$TYPESENSE_PORT" "$RELEASE_LOOPBACK_EXPECTED_TYPESENSE_MESSAGE"

  start_release_server opt_in_unset '' ''
  record_arm meilisearch_opt_in_unset_no_request assert_no_canary_activity \
    meilisearch meilisearch_opt_in_unset_no_request "${REQUEST_CANARY_BASE}" \
    "$RELEASE_LOOPBACK_EXPECTED_MEILI_MESSAGE"
  record_arm typesense_opt_in_unset_no_request assert_no_canary_activity \
    typesense typesense_opt_in_unset_no_request "${REQUEST_CANARY_BASE}" \
    "$RELEASE_LOOPBACK_EXPECTED_TYPESENSE_MESSAGE"
  record_arm meilisearch_opt_in_unset_no_dns assert_no_canary_activity \
    meilisearch meilisearch_opt_in_unset_no_dns "http://${DNS_CANARY_HOST}:${REQUEST_CANARY_BASE##*:}" \
    "$RELEASE_LOOPBACK_EXPECTED_MEILI_MESSAGE"
  record_arm typesense_opt_in_unset_no_dns assert_no_canary_activity \
    typesense typesense_opt_in_unset_no_dns "http://${DNS_CANARY_HOST}:${REQUEST_CANARY_BASE##*:}" \
    "$RELEASE_LOOPBACK_EXPECTED_TYPESENSE_MESSAGE"

  start_release_server opt_in_wrong_value true true
  record_arm meilisearch_opt_in_wrong_value_no_request assert_no_canary_activity \
    meilisearch meilisearch_opt_in_wrong_value_no_request "${REQUEST_CANARY_BASE}" \
    "$RELEASE_LOOPBACK_EXPECTED_MEILI_MESSAGE"
  record_arm typesense_opt_in_wrong_value_no_request assert_no_canary_activity \
    typesense typesense_opt_in_wrong_value_no_request "${REQUEST_CANARY_BASE}" \
    "$RELEASE_LOOPBACK_EXPECTED_TYPESENSE_MESSAGE"
  record_arm meilisearch_opt_in_wrong_value_no_dns assert_no_canary_activity \
    meilisearch meilisearch_opt_in_wrong_value_no_dns "http://${DNS_CANARY_HOST}:${REQUEST_CANARY_BASE##*:}" \
    "$RELEASE_LOOPBACK_EXPECTED_MEILI_MESSAGE"
  record_arm typesense_opt_in_wrong_value_no_dns assert_no_canary_activity \
    typesense typesense_opt_in_wrong_value_no_dns "http://${DNS_CANARY_HOST}:${REQUEST_CANARY_BASE##*:}" \
    "$RELEASE_LOOPBACK_EXPECTED_TYPESENSE_MESSAGE"

  if [ "${#CONTRACT_REDS[@]}" -ne 0 ]; then
    CHECKS_FAILED=${#CONTRACT_REDS[@]}
    printf 'MIGRATION_RELEASE_LOOPBACK_CONTRACT=RED profile=release passes=%s reds=%s arms=%s\n' \
      "$CONTRACT_PASSES" "${#CONTRACT_REDS[@]}" "$(IFS=,; printf '%s' "${CONTRACT_REDS[*]}")" >&2
    exit 1
  fi
  assert_expected_contract_arm_count
  printf 'MIGRATION_RELEASE_LOOPBACK_CONTRACT=PASS profile=release arms=%s providers=meilisearch,typesense\n' \
    "$CONTRACT_PASSES"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  trap cleanup EXIT
  main "$@"
fi
