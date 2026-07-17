#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRIVER="$SCRIPT_DIR/algolia_translation_live.sh"
WORK_DIR="$(mktemp -d)"
TESTS_RUN=0
TESTS_FAILED=0
TESTS_PASSED=0

cleanup() {
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_PASSED=$((TESTS_PASSED + 1))
  printf 'ok - %s\n' "$1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  printf 'not ok - %s\n%s\n' "$1" "${2:-}" >&2
}

run_driver() {
  local out="$1"
  shift || true
  set +e
  bash "$DRIVER" "$@" >"$out" 2>&1
  local rc=$?
  set -e
  printf '%s' "$rc"
}

write_stub_runtime() {
  local runtime="$1"
  mkdir -p "$runtime/bin" "$runtime/state"
  cat >"$runtime/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
state="${ALGOLIA_TRANSLATION_LIVE_STUB_STATE:?}"
printf '%s\n' "$*" >"$state/cargo_args.txt"
printf '%s\n' "${FLAPJACK_TRANSLATION_LIVE_FIXTURES:-}" >"$state/cargo_fixture_env.txt"
case "${ALGOLIA_TRANSLATION_LIVE_STUB_CARGO_MODE:-success}" in
  success) echo 'LIVE_TRANSLATION_PASS=7'; exit 0 ;;
  skipped) echo 'SKIPPED: FLAPJACK_TRANSLATION_LIVE_FIXTURES unset'; exit 0 ;;
  fail) echo 'cargo failed by stub' >&2; exit 101 ;;
esac
SH
  cat >"$runtime/bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
state="${ALGOLIA_TRANSLATION_LIVE_STUB_STATE:?}"
mkdir -p "$state"
method="GET"
body=""
url=""
key=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -X) method="$2"; shift 2 ;;
    -H)
      case "$2" in
        x-algolia-api-key:*) key="${2#x-algolia-api-key: }" ;;
      esac
      shift 2
      ;;
    --data) body="$2"; shift 2 ;;
    -sS|-w) [ "$1" = "-w" ] && shift; shift ;;
    http*) url="$1"; shift ;;
    *) shift ;;
  esac
done
python3 - "$state" "$method" "$url" "$key" "$body" <<'PY'
import json, pathlib, sys, urllib.parse
state = pathlib.Path(sys.argv[1])
method, url, key, body = sys.argv[2:]
parsed = urllib.parse.urlparse(url)
path = parsed.path
query = urllib.parse.parse_qs(parsed.query)

def lines(name):
    path = state / name
    return path.read_text().splitlines() if path.exists() else []

def append(name, value):
    with (state / name).open("a", encoding="utf-8") as f:
        f.write(value + "\n")

def append_unique(name, value):
    if value not in lines(name):
        append(name, value)

def respond(payload, code=200):
    print(json.dumps(payload, separators=(",", ":")))
    print(code)

def index_name():
    marker = "/1/indexes/"
    if marker not in path:
        return ""
    tail = path.split(marker, 1)[1].split("/", 1)[0]
    return urllib.parse.unquote(tail)

if method == "POST" and path == "/1/keys":
    key_value = f"stub-secret-key-{len(lines('created_keys.txt')) + 1}"
    append("created_keys.txt", key_value)
    respond({"key": key_value})
elif method == "DELETE" and path.startswith("/1/keys/"):
    append("deleted_keys.txt", urllib.parse.unquote(path.rsplit("/", 1)[-1]))
    respond({"deleted": True})
elif method == "GET" and path.startswith("/1/keys/"):
    queried = urllib.parse.unquote(path.rsplit("/", 1)[-1])
    respond({"message": "not found"}, 404 if queried in lines("deleted_keys.txt") else 200)
elif method == "GET" and path == "/1/indexes":
    if "ALGOLIA_TRANSLATION_LIVE_STUB_PAGE_ONE_LEFTOVER" in __import__("os").environ:
        page = int(query.get("page", ["0"])[0])
        leftovers = [name for name in lines("created_indexes.txt") if name.endswith("_replica")]
        items = leftovers if page == 1 else []
        respond({"items": [{"name": name} for name in items], "nbPages": 2})
        raise SystemExit
    deleted = set(lines("deleted_indexes.txt"))
    visible = [name for name in lines("created_indexes.txt") if name not in deleted]
    respond({"items": [{"name": name} for name in visible], "nbPages": 1})
elif method == "DELETE" and path.startswith("/1/indexes/"):
    if "ALGOLIA_TRANSLATION_LIVE_STUB_PAGE_ONE_LEFTOVER" not in __import__("os").environ or not index_name().endswith("_replica"):
        append("deleted_indexes.txt", index_name())
    respond({"taskID": 9000})
elif method == "GET" and "/task/" in path:
    if path.endswith("/task/invalid-live-task"):
        respond({"message": "Invalid taskID", "status": 400}, 400)
    else:
        append("polled_tasks.txt", f"{index_name()}:{path.rsplit('/', 1)[-1]}")
        respond({"status": "published"})
elif method == "PUT" and path.endswith("/settings"):
    append_unique("created_indexes.txt", index_name())
    for replica in json.loads(body).get("replicas", []):
        append_unique("created_indexes.txt", replica)
        append_unique("replica_indexes.txt", replica)
    respond({"taskID": 1001})
elif method == "POST" and path.endswith("/batch") and not path.endswith(("/rules/batch", "/synonyms/batch")):
    append_unique("created_indexes.txt", index_name())
    respond({"taskID": 1002})
elif method == "POST" and path.endswith("/rules/batch"):
    rules = json.loads(body)
    for rule in rules:
        patterns = {condition.get("pattern") for condition in rule.get("conditions", [])}
        for facet_filter in rule.get("consequence", {}).get("params", {}).get("automaticFacetFilters", []):
            if f"{{facet:{facet_filter.get('facet')}}}" not in patterns:
                respond({"message": "automatic facet filter has no matching pattern placeholder"}, 400)
                raise SystemExit
    respond({"taskID": 1003})
elif method == "POST" and path.endswith("/synonyms/batch"):
    respond({"taskID": 1004})
elif path.endswith("/operation"):
    append("operation_methods.txt", method)
    if method != "POST":
        respond({"message": "operation endpoint requires POST", "method": method}, 405)
        raise SystemExit
    destination = json.loads(body)["destination"]
    if destination in lines("replica_indexes.txt"):
        respond({"message": "cannot copy to a primary/replica index"}, 403)
        raise SystemExit
    append_unique("created_indexes.txt", destination)
    respond({"taskID": 1005})
elif method == "GET" and path.endswith("/settings"):
    if key == "stub-secret-key-1":
        respond({"message": "invalid key acl"}, 403)
    else:
        respond({"searchableAttributes":["title","brand"],"attributesForFaceting":["brand"],"unretrievableAttributes":["secret_note"],"numericAttributesToIndex":["price"],"distinct":True,"allowCompressionOfIntegerArray":False,"minWordSizefor1Typo":4,"hitsPerPage":20,"version":2,"highlightPreTag":"<em>"})
elif method == "POST" and path.endswith("/browse"):
    respond({"hits":[{"objectID":"live-doc-1","title":"Live Trail Shoe","brand":"North","price":129,"secret_note":"redacted"},{"objectID":"live-doc-2","title":"Live City Shoe","brand":"South","price":89}],"nbHits":2})
elif method == "POST" and path.endswith("/rules/search"):
    respond({"hits":[{"objectID":"live-rule-1","conditions":[{"pattern":"{facet:brand}","anchoring":"is"}],"consequence":{"promote":[{"objectID":"live-doc-1","position":1}],"params":{"automaticFacetFilters":[{"facet":"brand","score":4}]}},"enabled":True,"_metadata":{"lastUpdate":123},"_highlightResult":{"conditions":[{"pattern":{"value":"{facet:brand}"}}]}}],"nbHits":1})
elif method == "POST" and path.endswith("/synonyms/search"):
    respond({"hits":[{"objectID":"live-syn-1","type":"synonym","synonyms":["sneaker","trainer"],"_highlightResult":{"synonyms":[{"value":"sneaker"},{"value":"trainer"}]}},{"objectID":"live-syn-2","type":"onewaysynonym","input":"tee","synonyms":["t-shirt"],"_highlightResult":{"input":{"value":"tee"}}}],"nbHits":2})
else:
    respond({"message":"unexpected stub request","method":method,"path":path}, 500)
PY
SH
  chmod +x "$runtime/bin/cargo" "$runtime/bin/curl"
}

make_secret_fixture() {
  printf 'ALGOLIA_APP_ID=APPID123\nALGOLIA_ADMIN_KEY=ADMIN_SECRET_CANARY\n' >"$1"
}

run_stubbed_driver() {
  local runtime="$1" mode="$2" out="$3"
  make_secret_fixture "$runtime/secret.env"
  set +e
  PATH="$runtime/bin:$PATH" \
    ALGOLIA_TRANSLATION_LIVE_STUB_STATE="$runtime/state" \
    ALGOLIA_TRANSLATION_LIVE_STUB_CARGO_MODE="$mode" \
    bash "$DRIVER" --secret-file "$runtime/secret.env" >"$out" 2>&1
  local rc=$?
  set -e
  printf '%s' "$rc"
}

write_jq_redaction_failure() {
  local runtime="$1" real_jq
  real_jq="$(command -v jq)"
  cat >"$runtime/bin/jq" <<SH
#!/usr/bin/env bash
set -euo pipefail
if [ "\${ALGOLIA_TRANSLATION_LIVE_STUB_JQ_REDACTION_FAIL:-}" = "1" ] && [ "\${1:-}" = '.key = "<redacted>"' ]; then
  exit 66
fi
if [ "\${ALGOLIA_TRANSLATION_LIVE_STUB_JQ_REDACTION_FAIL:-}" = "1" ] && [[ "\${1:-}" == *created_key_fingerprints* ]]; then
  exit 66
fi
if [ "\${ALGOLIA_TRANSLATION_LIVE_STUB_JQ_REDACTION_FAIL:-}" = "1" ] && [ -f "\${ALGOLIA_TRANSLATION_LIVE_STUB_STATE:?}/created_keys.txt" ]; then
  exit 66
fi
exec "$real_jq" "\$@"
SH
  chmod +x "$runtime/bin/jq"
}

write_cp_failure() {
  local runtime="$1" real_cp
  real_cp="$(command -v cp)"
  cat >"$runtime/bin/cp" <<SH
#!/usr/bin/env bash
set -euo pipefail
if [ "\${ALGOLIA_TRANSLATION_LIVE_STUB_CP_FAIL:-}" = "1" ]; then
  exit 77
fi
exec "$real_cp" "\$@"
SH
  chmod +x "$runtime/bin/cp"
}

evidence_path() {
  sed -n 's/^INFO: preserved sanitized live translation evidence at //p' "$1" | tail -1
}

same_lines() {
  cmp -s <(LC_ALL=C sort "$1") <(LC_ALL=C sort "$2")
}

task_poll_seen() {
  local runtime="$1" index="$2" task="$3"
  grep -Fxq "${index}:${task}" "$runtime/state/polled_tasks.txt"
}

topology_tasks_waited() {
  local runtime="$1" topology
  topology="$(sed -n '/_topology$/p' "$runtime/state/created_indexes.txt" | head -1)"
  [ -n "$topology" ] \
    && task_poll_seen "$runtime" "$topology" 1001 \
    && task_poll_seen "$runtime" "$topology" 1005
}

topology_operation_used_post() {
  local runtime="$1"
  grep -Fxq 'POST' "$runtime/state/operation_methods.txt"
}

delete_tasks_waited() {
  local runtime="$1" index
  [ -f "$runtime/state/polled_tasks.txt" ] || return 1
  while IFS= read -r index || [ -n "$index" ]; do
    [ -n "$index" ] || continue
    task_poll_seen "$runtime" "$index" 9000 || return 1
  done <"$runtime/state/deleted_indexes.txt"
}

assert_usage_requires_secret_file() {
  local out rc
  out="$WORK_DIR/usage.out"
  rc="$(run_driver "$out")"
  if [ "$rc" = "2" ] && grep -Fq 'Usage:' "$out"; then
    pass 'driver requires exactly --secret-file <path>'
  else
    fail 'driver requires exactly --secret-file <path>' "rc=$rc output=$(cat "$out")"
  fi
}

assert_secret_failures_are_sanitized() {
  local out rc secret
  out="$WORK_DIR/missing.out"
  secret="$WORK_DIR/secret-path.env"
  rc="$(run_driver "$out" --secret-file "$secret")"
  if [ "$rc" != "0" ] && grep -Fq 'required Algolia credentials could not be loaded' "$out" && ! grep -Fq "$secret" "$out"; then
    pass 'secret loading failures are path-sanitized'
  else
    fail 'secret loading failures are path-sanitized' "rc=$rc output=$(cat "$out")"
  fi
}

assert_static_contract() {
  if grep -Fq 'source "$SECRET_HELPER"' "$DRIVER" \
    && grep -Fq 'load_named_secrets "$SECRET_FILE" ALGOLIA_APP_ID ALGOLIA_ADMIN_KEY' "$DRIVER" \
    && grep -Fq 'FLAPJACK_TRANSLATION_LIVE_FIXTURES="$FIXTURE_DIR" cargo test -p flapjack-http -- handlers::migration::translation::tests::live_algolia_translation_fixtures --nocapture' "$DRIVER" \
    && grep -Fq 'Retry with: bash engine/tests/algolia_translation_live.sh --secret-file <secret-file-with-ALGOLIA_APP_ID-and-ALGOLIA_ADMIN_KEY>' "$DRIVER" \
    && grep -Fq 'chmod 600 "$KEY_LEDGER"' "$DRIVER" \
    && grep -Fq 'created_indexes' "$DRIVER" \
    && grep -Fq 'created_key_fingerprints' "$DRIVER" \
    && ! grep -Eq 'parallel_development|gridl-dev|\\.secret' "$DRIVER"; then
    pass 'driver has static secret, cargo, retry, ledger, and path-safety contracts'
  else
    fail 'driver has static secret, cargo, retry, ledger, and path-safety contracts'
  fi
}

assert_stubbed_success() {
  local runtime out rc evidence
  runtime="$WORK_DIR/success"
  out="$WORK_DIR/success.out"
  write_stub_runtime "$runtime"
  rc="$(run_stubbed_driver "$runtime" success "$out")"
  evidence="$(evidence_path "$out")"
  if [ "$rc" = "0" ] \
    && [ -d "$evidence/fixtures" ] \
    && [ -f "$evidence/receipt.json" ] \
    && [ "$(cat "$runtime/state/cargo_fixture_env.txt")" != "" ] \
    && grep -Fq 'handlers::migration::translation::tests::live_algolia_translation_fixtures --nocapture' "$runtime/state/cargo_args.txt" \
    && same_lines "$runtime/state/created_indexes.txt" "$runtime/state/deleted_indexes.txt" \
    && same_lines "$runtime/state/created_keys.txt" "$runtime/state/deleted_keys.txt" \
    && topology_tasks_waited "$runtime" \
    && topology_operation_used_post "$runtime" \
    && delete_tasks_waited "$runtime" \
    && ! grep -R -F -e '_highlightResult' -e '_metadata' "$evidence/fixtures" >/dev/null 2>&1 \
    && ! grep -R -F -e 'ADMIN_SECRET_CANARY' -e 'stub-secret-key-' -e "$runtime/secret.env" "$evidence" "$out" >/dev/null 2>&1; then
    rm -rf "$evidence"
    pass 'stubbed success creates fixtures, uses POST topology copy, runs cargo hook, preserves sanitized evidence, and cleans owned resources'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'stubbed success creates fixtures, uses POST topology copy, runs cargo hook, preserves sanitized evidence, and cleans owned resources' "rc=$rc output=$(cat "$out")"
  fi
}

assert_live_result_enforcement() {
  local runtime out rc evidence
  runtime="$WORK_DIR/skipped"
  out="$WORK_DIR/skipped.out"
  write_stub_runtime "$runtime"
  rc="$(run_stubbed_driver "$runtime" skipped "$out")"
  evidence="$(evidence_path "$out")"
  if [ "$rc" != "0" ] && grep -Fq 'Rust live fixture suite skipped during credentialed run' "$out" && [ -d "$evidence" ]; then
    rm -rf "$evidence"
    pass 'driver fails credentialed run when Rust suite skips'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'driver fails credentialed run when Rust suite skips' "rc=$rc output=$(cat "$out")"
  fi
}

assert_cleanup_verifies_later_index_pages() {
  local runtime out rc
  runtime="$WORK_DIR/page-leftover"
  out="$WORK_DIR/page-leftover.out"
  write_stub_runtime "$runtime"
  make_secret_fixture "$runtime/secret.env"
  set +e
  PATH="$runtime/bin:$PATH" \
    ALGOLIA_TRANSLATION_LIVE_STUB_STATE="$runtime/state" \
    ALGOLIA_TRANSLATION_LIVE_STUB_PAGE_ONE_LEFTOVER=1 \
    bash "$DRIVER" --secret-file "$runtime/secret.env" >"$out" 2>&1
  rc=$?
  set -e
  if [ "$rc" != "0" ] && grep -Fq 'remaining Algolia indexes' "$out"; then
    pass 'cleanup checks every Algolia index-list page for receipt-owned leftovers'
  else
    fail 'cleanup checks every Algolia index-list page for receipt-owned leftovers' "rc=$rc output=$(cat "$out")"
  fi
}

assert_raw_key_response_is_not_left_untracked() {
  local runtime out rc
  runtime="$WORK_DIR/raw-key"
  out="$WORK_DIR/raw-key.out"
  mkdir -p "$runtime/tmp"
  write_stub_runtime "$runtime"
  write_jq_redaction_failure "$runtime"
  make_secret_fixture "$runtime/secret.env"
  set +e
  TMPDIR="$runtime/tmp" \
    PATH="$runtime/bin:$PATH" \
    ALGOLIA_TRANSLATION_LIVE_STUB_STATE="$runtime/state" \
    ALGOLIA_TRANSLATION_LIVE_STUB_JQ_REDACTION_FAIL=1 \
    bash "$DRIVER" --secret-file "$runtime/secret.env" >"$out" 2>&1
  rc=$?
  set -e
  if [ "$rc" != "0" ] && ! grep -R -F 'stub-secret-key-' "$runtime/tmp" >/dev/null 2>&1; then
    pass 'raw key response is never left in an untracked temporary file'
  else
    fail 'raw key response is never left in an untracked temporary file' "rc=$rc output=$(cat "$out") raw=$(grep -R -F 'stub-secret-key-' "$runtime/tmp" 2>/dev/null || true)"
  fi
}

assert_preservation_failure_still_runs_vendor_cleanup() {
  local runtime out rc
  runtime="$WORK_DIR/preserve-fail"
  out="$WORK_DIR/preserve-fail.out"
  write_stub_runtime "$runtime"
  write_cp_failure "$runtime"
  make_secret_fixture "$runtime/secret.env"
  set +e
  PATH="$runtime/bin:$PATH" \
    ALGOLIA_TRANSLATION_LIVE_STUB_STATE="$runtime/state" \
    ALGOLIA_TRANSLATION_LIVE_STUB_CP_FAIL=1 \
    bash "$DRIVER" --secret-file "$runtime/secret.env" >"$out" 2>&1
  rc=$?
  set -e
  if [ "$rc" != "0" ] \
    && [ -f "$runtime/state/deleted_indexes.txt" ] \
    && [ -f "$runtime/state/deleted_keys.txt" ] \
    && same_lines "$runtime/state/created_indexes.txt" "$runtime/state/deleted_indexes.txt" \
    && same_lines "$runtime/state/created_keys.txt" "$runtime/state/deleted_keys.txt"; then
    pass 'evidence preservation failure returns RED after receipt-scoped vendor cleanup'
  else
    fail 'evidence preservation failure returns RED after receipt-scoped vendor cleanup' "rc=$rc output=$(cat "$out")"
  fi
}

main() {
  echo 'algolia_translation_live closed driver contract test'
  [ -f "$DRIVER" ] && pass 'live translation driver exists' || fail 'live translation driver exists'
  assert_usage_requires_secret_file
  assert_secret_failures_are_sanitized
  assert_static_contract
  assert_stubbed_success
  assert_live_result_enforcement
  assert_cleanup_verifies_later_index_pages
  assert_raw_key_response_is_not_left_untracked
  assert_preservation_failure_still_runs_vendor_cleanup

  printf '\nResults: %d/%d passed\n' "$TESTS_PASSED" "$TESTS_RUN"
  if [ "$TESTS_FAILED" -gt 0 ]; then
    return 1
  fi
}

main "$@"
