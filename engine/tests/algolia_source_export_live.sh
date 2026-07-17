#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ENGINE_DIR="$REPO_DIR/engine"
WAIT_HELPER="$SCRIPT_DIR/common/wait_for_flapjack.sh"
SECRET_HELPER="$SCRIPT_DIR/common/load_named_secrets.sh"

SECRET_FILE=""
WORK_DIR=""
DATA_DIR=""
LOG_DIR=""
RECEIPT=""
SERVER_PID=""
SERVER_LOG=""
BASE_URL=""
ADMIN_KEY=""
BIN_PATH=""
RUN_PREFIX=""
PASS_COMPLETE=0
INTERRUPTED_EXIT_CODE=0
CLEANUP_FAILED=0
EVIDENCE_DIR=""
EVIDENCE_ANNOUNCED=0
DRIFT_BARRIER_DIR=""
DRIFT_BARRIER_OBSERVED_FILE=""
DRIFT_BARRIER_RELEASE_FILE=""

usage() {
  cat <<'EOF'
Usage:
  algolia_source_export_live.sh --secret-file <path>
EOF
}

die() {
  printf 'ERROR: %s\n' "$1" >&2
  exit "${2:-1}"
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"
}

json_quote() {
  python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$1"
}

fingerprint() {
  printf '%s' "$1" | python3 -c 'import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())'
}

parse_args() {
  if [ "$#" -ne 2 ] || [ "${1:-}" != "--secret-file" ] || [ -z "${2:-}" ]; then
    usage >&2
    exit 2
  fi
  SECRET_FILE="$2"
}

load_credentials() {
  # shellcheck source=engine/tests/common/load_named_secrets.sh
  source "$SECRET_HELPER"
  local loader_output
  loader_output="$(mktemp)"
  if ! load_named_secrets "$SECRET_FILE" ALGOLIA_APP_ID ALGOLIA_ADMIN_KEY >"$loader_output" 2>&1; then
    rm -f "$loader_output"
    die "required Algolia credentials could not be loaded"
  fi
  rm -f "$loader_output"
}

init_run() {
  require_tool curl
  require_tool jq
  require_tool python3
  require_tool od
  require_tool tr
  require_tool sed

  WORK_DIR="$(mktemp -d)"
  DATA_DIR="$WORK_DIR/flapjack-data"
  LOG_DIR="$WORK_DIR/logs"
  RECEIPT="$WORK_DIR/receipt.json"
  mkdir -p "$DATA_DIR" "$LOG_DIR"
  SERVER_LOG="$LOG_DIR/flapjack-server.log"

  local random_hex
  random_hex="$(od -An -N8 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  [ -n "$random_hex" ] || die "failed to generate run entropy"
  RUN_PREFIX="fj_stage4_${random_hex}"
  ADMIN_KEY="fj_live_$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  DRIFT_BARRIER_DIR="$WORK_DIR/drift-barrier"
  DRIFT_BARRIER_OBSERVED_FILE="$DRIFT_BARRIER_DIR/observed"
  DRIFT_BARRIER_RELEASE_FILE="$DRIFT_BARRIER_DIR/release"
  mkdir -p "$DRIFT_BARRIER_DIR"
  export FLAPJACK_ALGOLIA_LIVE_TEST_DRIFT_SOURCE="${RUN_PREFIX}_drift"
  export FLAPJACK_ALGOLIA_LIVE_TEST_DRIFT_BARRIER_DIR="$DRIFT_BARRIER_DIR"

  : >"$WORK_DIR/created_indexes.txt"
  : >"$WORK_DIR/created_keys.txt"
  jq -n --arg prefix "$RUN_PREFIX" --arg head "$(git -C "$REPO_DIR" rev-parse HEAD 2>/dev/null || true)" \
    '{prefix:$prefix, head:$head, created_indexes:[], created_key_fingerprints:[], checks:[]}' >"$RECEIPT"
}

record_check() {
  local name="$1" status="$2" detail="${3:-}"
  local next
  next="$(mktemp)"
  jq --arg name "$name" --arg status "$status" --arg detail "$detail" \
    '.checks += [{name:$name,status:$status,detail:$detail}]' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

record_index() {
  printf '%s\n' "$1" >>"$WORK_DIR/created_indexes.txt"
  local next
  next="$(mktemp)"
  jq --arg name "$1" '.created_indexes += [$name]' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

record_key() {
  printf '%s\n' "$1" >>"$WORK_DIR/created_keys.txt"
  local fp next
  fp="$(fingerprint "$1")"
  next="$(mktemp)"
  jq --arg fp "$fp" '.created_key_fingerprints += [$fp]' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

algolia_base() {
  local mode="$1"
  if [ "$mode" = "read" ]; then
    printf 'https://%s-dsn.algolia.net' "$ALGOLIA_APP_ID"
  else
    printf 'https://%s.algolia.net' "$ALGOLIA_APP_ID"
  fi
}

http_body() { sed '$d'; }
http_code() { tail -1; }

algolia_request() {
  local mode="$1" method="$2" path="$3" key="$4" body="${5:-}"
  local base
  base="$(algolia_base "$mode")"
  if [ -n "$body" ]; then
    curl -sS -w '\n%{http_code}' -X "$method" "${base}${path}" \
      -H "x-algolia-application-id: ${ALGOLIA_APP_ID}" \
      -H "x-algolia-api-key: ${key}" \
      -H 'content-type: application/json' \
      --data "$body"
  else
    curl -sS -w '\n%{http_code}' -X "$method" "${base}${path}" \
      -H "x-algolia-application-id: ${ALGOLIA_APP_ID}" \
      -H "x-algolia-api-key: ${key}" \
      -H 'content-type: application/json'
  fi
}

expect_algolia_json() {
  local mode="$1" method="$2" path="$3" key="$4" body="${5:-}" out="$6"
  local response code payload
  response="$(algolia_request "$mode" "$method" "$path" "$key" "$body")"
  code="$(printf '%s\n' "$response" | http_code)"
  payload="$(printf '%s\n' "$response" | http_body)"
  if [ "$code" -lt 200 ] || [ "$code" -gt 299 ]; then
    printf '%s\n' "$payload" >"$LOG_DIR/last_algolia_error.json"
    die "Algolia request failed with HTTP ${code}"
  fi
  printf '%s\n' "$payload" >"$out"
}

flapjack_request() {
  local method="$1" path="$2" body="${3:-}"
  if [ -n "$body" ]; then
    curl -sS -w '\n%{http_code}' -X "$method" "${BASE_URL}${path}" \
      -H "x-algolia-application-id: flapjack" \
      -H "x-algolia-api-key: ${ADMIN_KEY}" \
      -H 'content-type: application/json' \
      --data "$body"
  else
    curl -sS -w '\n%{http_code}' -X "$method" "${BASE_URL}${path}" \
      -H "x-algolia-application-id: flapjack" \
      -H "x-algolia-api-key: ${ADMIN_KEY}" \
      -H 'content-type: application/json'
  fi
}

build_or_resolve_binary() {
  if [ -n "${FLAPJACK_BIN:-}" ]; then
    [ -x "$FLAPJACK_BIN" ] || die "FLAPJACK_BIN is not executable"
    BIN_PATH="$FLAPJACK_BIN"
    return
  fi
  if (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$LOG_DIR/build.log" 2>&1); then
    BIN_PATH="$ENGINE_DIR/target/debug/flapjack"
  else
    die "cargo build -p flapjack-server failed"
  fi
  [ -x "$BIN_PATH" ] || die "expected flapjack binary was not built"
}

start_server() {
  FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$DATA_DIR" \
    "$BIN_PATH" --auto-port >"$SERVER_LOG" 2>&1 &
  SERVER_PID=$!

  "$WAIT_HELPER" --pid "$SERVER_PID" --host 127.0.0.1 --port auto --log-path "$SERVER_LOG" --retries 80 --interval-seconds 0.5
  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$SERVER_LOG" | head -1)"
  [ -n "$port" ] || die "server became ready but no auto-port was found"
  BASE_URL="http://127.0.0.1:${port}"
  record_check "local_server" "pass" "started"
}

make_fixture_files() {
  python3 - "$WORK_DIR" "$RUN_PREFIX" <<'PY'
import json, os, sys
work, prefix = sys.argv[1], sys.argv[2]

docs = [
    {"objectID": f"doc-{i:04d}", "title": f"Stage 4 product {i:04d}", "category": "even" if i % 2 == 0 else "odd", "secret_note": f"hidden-{i:04d}"}
    for i in range(1005)
]
rules = [
    {"objectID": f"rule-{i}", "conditions": [{"pattern": f"product {i}", "anchoring": "contains"}], "consequence": {"params": {"filters": f"category:{'even' if i % 2 == 0 else 'odd'}"}}}
    for i in range(1, 4)
]
synonyms = [
    {"objectID": "syn-1", "type": "synonym", "synonyms": ["sneaker", "trainer"]},
    {"objectID": "syn-2", "type": "oneWaySynonym", "input": "tee", "synonyms": ["tshirt", "t-shirt"]},
    {"objectID": "syn-3", "type": "altCorrection1", "word": "flapjack", "corrections": ["flapjacks"]},
]
settings = {
    "searchableAttributes": ["title", "category"],
    "attributesForFaceting": ["category"],
    "unretrievableAttributes": ["secret_note"],
    "disableTypoToleranceOnAttributes": ["secret_note"],
}
drift_docs = [
    {"objectID": f"drift-{i:04d}", "title": f"Drift product {i:04d}", "secret_note": f"drift-hidden-{i:04d}"}
    for i in range(2500)
]
paths = {
    "documents": docs,
    "rules": rules,
    "synonyms": synonyms,
    "settings": settings,
    "drift_documents": drift_docs,
}
for name, value in paths.items():
    with open(os.path.join(work, f"{name}.json"), "w", encoding="utf-8") as f:
        json.dump(value, f, separators=(",", ":"), sort_keys=True)
summary = {
    "documents": {"count": len(docs), "ids": [d["objectID"] for d in docs], "hash": "63fcd6c4806d80d401a9201d3727c6dda6b248a626d21eae2897a4329d2d6319"},
    "rules": {"count": len(rules), "ids": [r["objectID"] for r in rules], "hash": "a8fa2917734a923921d5cba023feddbec402d7f17046c831c77b47c040eaf549"},
    "synonyms": {"count": len(synonyms), "ids": [s["objectID"] for s in synonyms], "hash": "1d7cae1d697267829cc629813422619dcf8e7e5150ba9d9672f5fc3e4beec754"},
    "drift_documents": {"count": len(drift_docs), "ids": [d["objectID"] for d in drift_docs]},
}
with open(os.path.join(work, "expected.json"), "w", encoding="utf-8") as f:
    json.dump(summary, f, indent=2, sort_keys=True)
PY
}

wait_task() {
  local index="$1"
  local task_id="$2"
  local out="$LOG_DIR/task-${index}-${task_id}.json"
  for _ in $(seq 1 120); do
    expect_algolia_json read GET "/1/indexes/$(python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$index")/task/${task_id}" "$ALGOLIA_ADMIN_KEY" "" "$out"
    if [ "$(jq -r '.status // empty' "$out")" = "published" ]; then
      return 0
    fi
    sleep 0.5
  done
  die "Algolia task did not publish"
}

http_success_code() {
  case "$1" in
    2*) return 0 ;;
    *) return 1 ;;
  esac
}

cleanup_wait_task() {
  local index="$1" task_id="$2" encoded response code payload
  encoded="$(python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$index")"
  for _ in $(seq 1 60); do
    response="$(algolia_request read GET "/1/indexes/${encoded}/task/${task_id}" "$ALGOLIA_ADMIN_KEY" "" 2>/dev/null || true)"
    code="$(printf '%s\n' "$response" | http_code)"
    payload="$(printf '%s\n' "$response" | http_body)"
    if http_success_code "$code" && [ "$(printf '%s\n' "$payload" | jq -r '.status // empty' 2>/dev/null)" = "published" ]; then
      return 0
    fi
    sleep 0.5
  done
  return 1
}

create_key() {
  local body="$1" out="$2"
  local raw_out
  raw_out="$(mktemp)"
  expect_algolia_json write POST "/1/keys" "$ALGOLIA_ADMIN_KEY" "$body" "$raw_out"
  local key
  key="$(jq -r '.key // empty' "$raw_out")"
  [ -n "$key" ] || die "Algolia key creation response was missing key"
  jq '.key = "<redacted>"' "$raw_out" >"$out"
  rm -f "$raw_out"
  record_key "$key"
  printf '%s\n' "$key"
}

seed_index() {
  local index="$1" documents_file="$2"
  record_index "$index"
  local encoded out task
  encoded="$(python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$index")"
  out="$LOG_DIR/${index}-settings.json"
  expect_algolia_json write PUT "/1/indexes/${encoded}/settings" "$ALGOLIA_ADMIN_KEY" "$(cat "$WORK_DIR/settings.json")" "$out"
  task="$(jq -r '.taskID // empty' "$out")"
  [ -n "$task" ] && wait_task "$index" "$task"

  python3 - "$documents_file" "$WORK_DIR/${index}-batch-" <<'PY'
import json, sys
docs = json.load(open(sys.argv[1]))
prefix = sys.argv[2]
for offset in range(0, len(docs), 1000):
    requests = [{"action": "addObject", "body": doc} for doc in docs[offset:offset+1000]]
    json.dump({"requests": requests}, open(f"{prefix}{offset//1000}.json", "w"), separators=(",", ":"))
PY
  for batch_file in "$WORK_DIR/${index}-batch-"*.json; do
    out="$LOG_DIR/${index}-batch-$(basename "$batch_file").json"
    expect_algolia_json write POST "/1/indexes/${encoded}/batch" "$ALGOLIA_ADMIN_KEY" "$(cat "$batch_file")" "$out"
    task="$(jq -r '.taskID // empty' "$out")"
    [ -n "$task" ] && wait_task "$index" "$task"
  done
}

seed_rules_synonyms() {
  local index="$1" encoded out task
  encoded="$(python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$index")"
  out="$LOG_DIR/${index}-rules.json"
  expect_algolia_json write POST "/1/indexes/${encoded}/rules/batch?clearExistingRules=true" "$ALGOLIA_ADMIN_KEY" "$(cat "$WORK_DIR/rules.json")" "$out"
  task="$(jq -r '.taskID // empty' "$out")"
  [ -n "$task" ] && wait_task "$index" "$task"
  out="$LOG_DIR/${index}-synonyms.json"
  expect_algolia_json write POST "/1/indexes/${encoded}/synonyms/batch?replaceExistingSynonyms=true" "$ALGOLIA_ADMIN_KEY" "$(cat "$WORK_DIR/synonyms.json")" "$out"
  task="$(jq -r '.taskID // empty' "$out")"
  [ -n "$task" ] && wait_task "$index" "$task"
}

setup_vendor() {
  make_fixture_files
  local source_index="${RUN_PREFIX}_source"
  local aux_a="${RUN_PREFIX}_aux_a"
  local aux_b="${RUN_PREFIX}_aux_b"
  local drift_index="${RUN_PREFIX}_drift"
  seed_index "$source_index" "$WORK_DIR/documents.json"
  seed_rules_synonyms "$source_index"
  seed_index "$aux_a" "$WORK_DIR/documents.json"
  seed_index "$aux_b" "$WORK_DIR/documents.json"
  seed_index "$drift_index" "$WORK_DIR/drift_documents.json"

  local key_body key_out denied_body denied_out permitted denied
  key_body="$(jq -n --arg idx "${RUN_PREFIX}_*" --arg desc "${RUN_PREFIX} permitted source export" '{acl:["search","browse","settings","listIndexes","seeUnretrievableAttributes"], indexes:[$idx], description:$desc, validity:3600}')"
  denied_body="$(jq -n --arg idx "${RUN_PREFIX}_*" --arg desc "${RUN_PREFIX} denied source export" '{acl:["search","browse","settings","listIndexes"], indexes:[$idx], description:$desc, validity:3600}')"
  key_out="$LOG_DIR/permitted-key.json"
  denied_out="$LOG_DIR/denied-key.json"
  permitted="$(create_key "$key_body" "$key_out")"
  denied="$(create_key "$denied_body" "$denied_out")"
  printf '%s\n' "$permitted" >"$WORK_DIR/permitted.key"
  printf '%s\n' "$denied" >"$WORK_DIR/denied.key"
  record_check "vendor_setup" "pass" "created prefixed resources"
}

collect_paginated_ids() {
  local resource="$1" index="$2" out="$3" page_size="${4:-1}"
  local page=0 ids_file response code payload nb_pages path body encoded request_status
  ids_file="$(mktemp)"
  : >"$ids_file"
  encoded="$(python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$index")"
  while :; do
    if [ "$resource" = "indexes" ]; then
      path="/1/indexes?page=${page}&hitsPerPage=${page_size}"
      body=""
      response="$(algolia_request read GET "$path" "$ALGOLIA_ADMIN_KEY" "$body")" || {
        request_status=$?
        rm -f "$ids_file"
        return "$request_status"
      }
    else
      path="/1/indexes/${encoded}/${resource}/search"
      body="$(jq -cn --argjson page "$page" --argjson page_size "$page_size" '{query:"",hitsPerPage:$page_size,page:$page}')"
      response="$(algolia_request read POST "$path" "$ALGOLIA_ADMIN_KEY" "$body")" || {
        request_status=$?
        rm -f "$ids_file"
        return "$request_status"
      }
    fi
    code="$(printf '%s\n' "$response" | http_code)"
    payload="$(printf '%s\n' "$response" | http_body)"
    if ! http_success_code "$code"; then
      rm -f "$ids_file"
      return 1
    fi
    if ! nb_pages="$(printf '%s\n' "$payload" | jq -r --argjson page_size "$page_size" 'if (.nbPages? // null) != null then .nbPages else (((.nbHits // 0) + $page_size - 1) / $page_size | floor) end')"; then
      rm -f "$ids_file"
      return 1
    fi
    if [ "$resource" = "indexes" ]; then
      printf '%s\n' "$payload" | jq -r --arg prefix "$RUN_PREFIX" '.items[]?.name | select(startswith($prefix))' >>"$ids_file"
    else
      printf '%s\n' "$payload" | jq -r '.hits[]?.objectID' >>"$ids_file"
    fi
    if [ $((page + 1)) -ge "$nb_pages" ]; then
      break
    fi
    page=$((page + 1))
  done
  python3 - "$ids_file" "$out" <<'PY'
import json, sys
values = [line.strip() for line in open(sys.argv[1]) if line.strip()]
json.dump(sorted(values), open(sys.argv[2], "w"))
PY
  rm -f "$ids_file"
}

collect_paginated_ids_or_die() {
  local label="$1" status
  shift
  collect_paginated_ids "$@" && return 0
  status=$?
  case "$status" in
    130 | 143) return "$status" ;;
    *) die "${label} pagination request failed" ;;
  esac
}

verify_vendor_pagination() {
  local source_index="${RUN_PREFIX}_source"
  local index_ids="$LOG_DIR/index-page-ids.json"
  local rule_ids="$LOG_DIR/rule-page-ids.json"
  local synonym_ids="$LOG_DIR/synonym-page-ids.json"
  for _ in $(seq 1 40); do
    collect_paginated_ids_or_die index indexes "" "$index_ids" 1
    collect_paginated_ids_or_die rule rules "$source_index" "$rule_ids" 1
    collect_paginated_ids_or_die synonym synonyms "$source_index" "$synonym_ids" 1
    if jq -e --arg a "${RUN_PREFIX}_source" --arg b "${RUN_PREFIX}_aux_a" --arg c "${RUN_PREFIX}_aux_b" \
      'index($a) and index($b) and index($c)' "$index_ids" >/dev/null \
      && jq -e 'sort == ["rule-1","rule-2","rule-3"]' "$rule_ids" >/dev/null \
      && jq -e 'sort == ["syn-1","syn-2","syn-3"]' "$synonym_ids" >/dev/null; then
      record_check "vendor_pagination" "pass" "page-size-one traversal matched fixtures"
      return 0
    fi
    sleep 0.5
  done
  die "vendor pagination did not expose the exact prefixed fixtures"
}

assert_no_destination_index() {
  local before="$1" after="$2"
  local before_names after_names
  before_names="$(jq -c '([.indexes[]?.name] + [.items[]?.name]) | sort' "$before")" \
    || die "Flapjack pre-export index response was malformed"
  after_names="$(jq -c '([.indexes[]?.name] + [.items[]?.name]) | sort' "$after")" \
    || die "Flapjack post-export index response was malformed"
  [ "$before_names" = "$after_names" ] \
    || die "Flapjack index membership changed during source-only export"
}

inspect_spool() {
  local expected="$WORK_DIR/expected.json"
  python3 - "$DATA_DIR" "$expected" <<'PY'
import hashlib, json, os, sys
data_dir, expected_path = sys.argv[1], sys.argv[2]
expected = json.load(open(expected_path))
jobs = os.path.join(data_dir, "migration_exports", "jobs")
job_dirs = [os.path.join(jobs, name) for name in os.listdir(jobs)]
accepted = []
for path in job_dirs:
    manifest = json.load(open(os.path.join(path, "manifest.json")))
    if manifest["lifecycle"] == "Accepted":
        accepted.append((path, manifest))
if len(accepted) != 1:
    raise SystemExit(f"expected exactly one accepted job, got {len(accepted)}")
path, manifest = accepted[0]

def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()

def item_hash(value):
    return hashlib.sha256(canonical(value)).hexdigest()

def aggregate(items):
    rows = [(item["objectID"], item_hash(item)) for item in items]
    h = hashlib.sha256()
    for oid, ih in sorted(rows):
        h.update(oid.encode()); h.update(b"\0"); h.update(ih.encode()); h.update(b"\n")
    return h.hexdigest()

resources = {"DocumentPage": [], "RulesPage": [], "SynonymsPage": []}
for artifact in manifest["artifacts"]:
    if artifact["state"] != "Visible":
        raise SystemExit("manifest contains non-visible artifact after acceptance")
    if artifact["kind"] in resources:
        resources[artifact["kind"]].extend(json.load(open(os.path.join(path, artifact["final_path"]))))

checks = [("documents", "DocumentPage"), ("rules", "RulesPage"), ("synonyms", "SynonymsPage")]
summary = {}
for name, kind in checks:
    ids = sorted(item["objectID"] for item in resources[kind])
    if ids != sorted(expected[name]["ids"]):
        raise SystemExit(f"{name} membership mismatch")
    if aggregate(resources[kind]) != expected[name]["hash"]:
        raise SystemExit(f"{name} hash mismatch")
    if manifest["resource_completions"][name]["hash"] != expected[name]["hash"]:
        raise SystemExit(f"{name} completion hash mismatch")
    summary[name] = {"count": len(ids), "hash": expected[name]["hash"]}

for sidecar_name, expected_name in [("completed_objects", "documents"), ("completed_rules", "rules"), ("completed_synonyms", "synonyms")]:
    if manifest[sidecar_name]["count"] != expected[expected_name]["count"]:
        raise SystemExit(f"{sidecar_name} count mismatch")
json.dump(summary, sys.stdout, sort_keys=True)
PY
}

observe_drift_artifact_read() {
  local artifact="$1"
  python3 - "$artifact" <<'PY'
import sys

with open(sys.argv[1], "rb") as artifact:
    artifact.read()
PY
  if [ -n "${ALGOLIA_LIVE_TEST_BARRIER_OBSERVATION_FILE:-}" ]; then
    printf '%s\n' "$artifact" >"$ALGOLIA_LIVE_TEST_BARRIER_OBSERVATION_FILE"
  fi
}

release_drift_export_barrier() {
  [ -n "$DRIFT_BARRIER_RELEASE_FILE" ] || die "drift barrier release path was not initialized"
  : >"$DRIFT_BARRIER_RELEASE_FILE"
}

run_permitted_and_denied_migrations() {
  local source_index="${RUN_PREFIX}_source"
  local permitted denied before after body response code payload summary
  permitted="$(cat "$WORK_DIR/permitted.key")"
  denied="$(cat "$WORK_DIR/denied.key")"
  flapjack_request GET "/1/indexes" >"$WORK_DIR/flapjack-before.raw"
  printf '%s\n' "$(cat "$WORK_DIR/flapjack-before.raw" | http_body)" >"$WORK_DIR/flapjack-before.json"
  body="$(jq -n --arg app "$ALGOLIA_APP_ID" --arg key "$permitted" --arg src "$source_index" '{appId:$app,apiKey:$key,sourceIndex:$src}')"
  response="$(flapjack_request POST "/1/migrate-from-algolia" "$body")"
  code="$(printf '%s\n' "$response" | http_code)"
  payload="$(printf '%s\n' "$response" | http_body)"
  printf '%s\n' "$payload" >"$LOG_DIR/permitted-migration.json"
  [ "$code" = "200" ] || die "permitted migration returned HTTP ${code}"
  jq -e '.status == "complete" and .settings == true and .objects.imported == 1005 and .rules.imported == 3 and .synonyms.imported == 3 and .taskID == 0' "$LOG_DIR/permitted-migration.json" >/dev/null
  summary="$(inspect_spool)"

  body="$(jq -n --arg app "$ALGOLIA_APP_ID" --arg key "$denied" --arg src "$source_index" '{appId:$app,apiKey:$key,sourceIndex:$src}')"
  response="$(flapjack_request POST "/1/migrate-from-algolia" "$body" || true)"
  code="$(printf '%s\n' "$response" | http_code)"
  payload="$(printf '%s\n' "$response" | http_body)"
  printf '%s\n' "$payload" >"$LOG_DIR/denied-migration.json"
  [ "$code" = "400" ] || die "denied migration returned HTTP ${code}, expected 400"
  jq -e '.message == "Algolia key cannot export unretrievable attributes" and .status == 400' "$LOG_DIR/denied-migration.json" >/dev/null
  if grep -F "$denied" "$LOG_DIR/denied-migration.json" >/dev/null; then
    die "denied migration response leaked API key"
  fi

  flapjack_request GET "/1/indexes" >"$WORK_DIR/flapjack-after.raw"
  printf '%s\n' "$(cat "$WORK_DIR/flapjack-after.raw" | http_body)" >"$WORK_DIR/flapjack-after.json"
  assert_no_destination_index "$WORK_DIR/flapjack-before.json" "$WORK_DIR/flapjack-after.json"
  record_check "migration_acl_and_spool" "pass" "$summary"
}

run_drift_refusal() {
  local drift_index="${RUN_PREFIX}_drift" permitted body curl_out curl_pid barrier_path drift_job_dir mutation_payload mutation_out response code payload
  permitted="$(cat "$WORK_DIR/permitted.key")"
  body="$(jq -n --arg app "$ALGOLIA_APP_ID" --arg key "$permitted" --arg src "$drift_index" '{appId:$app,apiKey:$key,sourceIndex:$src}')"
  curl_out="$LOG_DIR/drift-migration.raw"
  (flapjack_request POST "/1/migrate-from-algolia" "$body" >"$curl_out") &
  curl_pid=$!

  barrier_path=""
  drift_job_dir=""
  for _ in $(seq 1 240); do
    local barrier_job candidate artifact_name manifest_path
    if [ -s "$DRIFT_BARRIER_OBSERVED_FILE" ]; then
      barrier_job="$(cat "$DRIFT_BARRIER_OBSERVED_FILE")"
      [ "$(basename "$barrier_job")" = "$barrier_job" ] \
        || die "drift barrier returned an invalid job name"
      drift_job_dir="$DATA_DIR/migration_exports/jobs/$barrier_job"
    fi
    if [ -n "$drift_job_dir" ] && [ -d "$drift_job_dir" ]; then
      for candidate in "$drift_job_dir"/documents-*.bin; do
        [ -f "$candidate" ] || continue
        artifact_name="$(basename "$candidate")"
        manifest_path="$drift_job_dir/manifest.json"
        if [ -f "$manifest_path" ] && jq -e --arg artifact "$artifact_name" \
          '.artifacts[]? | select(.kind == "DocumentPage" and .final_path == $artifact and (.state == "Staged" or .state == "Visible"))' \
          "$manifest_path" >/dev/null 2>&1; then
          observe_drift_artifact_read "$candidate"
          barrier_path="$candidate"
          break
        fi
      done
    fi
    if [ -n "$barrier_path" ]; then
      break
    fi
    if ! kill -0 "$curl_pid" 2>/dev/null; then
      wait "$curl_pid" || true
      die "drift barrier was not reached before migration finished"
    fi
    sleep 0.1
  done
  [ -n "$barrier_path" ] || die "drift artifact barrier was not established"

  mutation_payload="$(jq -n --arg id "drift-2499" --arg title "mutated-${RUN_PREFIX}" '{requests:[{action:"partialUpdateObject", body:{objectID:$id,title:$title}}]}')"
  mutation_out="$LOG_DIR/drift-mutation.json"
  expect_algolia_json write POST "/1/indexes/${drift_index}/batch" "$ALGOLIA_ADMIN_KEY" "$mutation_payload" "$mutation_out"
  local task
  task="$(jq -r '.taskID // empty' "$mutation_out")"
  [ -n "$task" ] && wait_task "$drift_index" "$task"
  release_drift_export_barrier

  wait "$curl_pid" || true
  response="$(cat "$curl_out")"
  code="$(printf '%s\n' "$response" | http_code)"
  payload="$(printf '%s\n' "$response" | http_body)"
  printf '%s\n' "$payload" >"$LOG_DIR/drift-migration.json"
  [ "$code" = "502" ] || die "drift migration returned HTTP ${code}, expected 502"
  jq -e '.message == "Algolia source changed during export" and .status == 502' "$LOG_DIR/drift-migration.json" >/dev/null
  jq -e '.lifecycle == "Failed"' "$drift_job_dir/manifest.json" >/dev/null
  record_check "drift_refusal" "pass" "artifact barrier plus awaited mutation refused export"
}

cleanup_vendor() {
  [ -n "$WORK_DIR" ] || return 0
  local key index fp remaining response code payload task
  if [ -f "$WORK_DIR/created_keys.txt" ]; then
    while IFS= read -r key || [ -n "$key" ]; do
      [ -n "$key" ] || continue
      algolia_request write DELETE "/1/keys/$(python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$key")" "$ALGOLIA_ADMIN_KEY" "" >/dev/null 2>&1 || true
    done <"$WORK_DIR/created_keys.txt"
  fi
  if [ -f "$WORK_DIR/created_indexes.txt" ]; then
    while IFS= read -r index || [ -n "$index" ]; do
      [ -n "$index" ] || continue
      response="$(algolia_request write DELETE "/1/indexes/$(python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$index")" "$ALGOLIA_ADMIN_KEY" "" 2>/dev/null || true)"
      code="$(printf '%s\n' "$response" | http_code)"
      payload="$(printf '%s\n' "$response" | http_body)"
      if http_success_code "$code"; then
        task="$(printf '%s\n' "$payload" | jq -r '.taskID // empty' 2>/dev/null || true)"
        [ -z "$task" ] || cleanup_wait_task "$index" "$task" || true
      fi
    done <"$WORK_DIR/created_indexes.txt"
  fi

  if [ -n "${ALGOLIA_APP_ID:-}" ] && [ -n "${ALGOLIA_ADMIN_KEY:-}" ] && [ -n "$RUN_PREFIX" ]; then
    remaining="$(mktemp)"
    if collect_paginated_ids indexes "" "$remaining" 1000 2>/dev/null; then
      if jq -e 'length == 0' "$remaining" >/dev/null 2>&1; then
        :
      else
        CLEANUP_FAILED=1
        printf 'ERROR: remaining Algolia indexes for prefix %s:\n' "$RUN_PREFIX" >&2
        jq -r '.[]' "$remaining" >&2 || true
      fi
    else
      CLEANUP_FAILED=1
      printf 'ERROR: unable to verify Algolia index cleanup for prefix %s\n' "$RUN_PREFIX" >&2
    fi
    if [ -f "$WORK_DIR/created_keys.txt" ]; then
      while IFS= read -r key || [ -n "$key" ]; do
        [ -n "$key" ] || continue
        response="$(algolia_request read GET "/1/keys/$(python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$key")" "$ALGOLIA_ADMIN_KEY" "" 2>/dev/null || true)"
        code="$(printf '%s\n' "$response" | http_code)"
        if http_success_code "$code"; then
          CLEANUP_FAILED=1
          fp="$(fingerprint "$key")"
          printf 'ERROR: remaining Algolia restricted key fingerprint: %s\n' "$fp" >&2
        fi
      done <"$WORK_DIR/created_keys.txt"
    fi
  fi
}

preserve_run_evidence() {
  local announce="${1:-1}"
  if [ -z "$EVIDENCE_DIR" ]; then
    EVIDENCE_DIR="/tmp/flapjack_algolia_source_export_live_evidence_${$}_$(date +%s)"
    mkdir -p "$EVIDENCE_DIR"
    chmod 700 "$EVIDENCE_DIR" 2>/dev/null || true
    [ -n "$LOG_DIR" ] && [ -d "$LOG_DIR" ] && cp -R "$LOG_DIR" "$EVIDENCE_DIR/logs" 2>/dev/null || true
    [ -n "$RECEIPT" ] && [ -f "$RECEIPT" ] && cp "$RECEIPT" "$EVIDENCE_DIR/receipt.json" 2>/dev/null || true
    if [ -n "$DATA_DIR" ] && [ -d "$DATA_DIR/migration_exports/jobs" ]; then
      mkdir -p "$EVIDENCE_DIR/migration_exports"
      cp -R "$DATA_DIR/migration_exports/jobs" "$EVIDENCE_DIR/migration_exports/jobs" 2>/dev/null || true
    fi
  fi
  if [ "$announce" -eq 1 ] && [ "$EVIDENCE_ANNOUNCED" -eq 0 ]; then
    printf 'INFO: preserved sanitized live evidence at %s\n' "$EVIDENCE_DIR" >&2
    EVIDENCE_ANNOUNCED=1
  fi
}

cleanup() {
  local script_exit_code=$?
  local effective_exit_code="$script_exit_code"
  [ "$INTERRUPTED_EXIT_CODE" -eq 0 ] || effective_exit_code="$INTERRUPTED_EXIT_CODE"
  trap - EXIT INT TERM
  set +e
  if [ "$PASS_COMPLETE" -ne 1 ] || [ "$effective_exit_code" -ne 0 ]; then
    preserve_run_evidence 1
  else
    # Stage private evidence before vendor cleanup so cleanup-only failures
    # cannot destroy their logs, receipt, or spool artifacts.
    preserve_run_evidence 1
  fi
  cleanup_vendor
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
  fi
  if [ -n "$DATA_DIR" ] && [ -d "$DATA_DIR" ]; then
    rm -rf "$DATA_DIR"
  fi
  if [ "$PASS_COMPLETE" -eq 1 ] && [ -n "$DATA_DIR" ] && [ -e "$DATA_DIR/migration_exports/jobs" ]; then
    CLEANUP_FAILED=1
    printf 'ERROR: local migration_exports/jobs path remains after cleanup\n' >&2
  fi
  if [ -n "$WORK_DIR" ] && [ -d "$WORK_DIR" ]; then
    rm -rf "$WORK_DIR"
  fi
  if [ "$CLEANUP_FAILED" -ne 0 ]; then
    preserve_run_evidence 1
    printf 'Retry with: bash engine/tests/algolia_source_export_live.sh --secret-file <secret-file-with-ALGOLIA_APP_ID-and-ALGOLIA_ADMIN_KEY>\n' >&2
    exit 1
  fi
  exit "$effective_exit_code"
}

main() {
  parse_args "$@"
  load_credentials
  init_run
  if [ -n "${ALGOLIA_LIVE_TEST_DRIVER_PID_FILE:-}" ]; then
    printf '%s\n' "${BASHPID:-$$}" >"$ALGOLIA_LIVE_TEST_DRIVER_PID_FILE"
  fi
  trap cleanup EXIT
  trap 'INTERRUPTED_EXIT_CODE=130; exit 130' INT
  trap 'INTERRUPTED_EXIT_CODE=143; exit 143' TERM
  build_or_resolve_binary
  start_server
  setup_vendor
  verify_vendor_pagination
  run_permitted_and_denied_migrations
  run_drift_refusal
  PASS_COMPLETE=1
  record_check "cleanup_precheck" "pass" "all live assertions completed"
  jq '{prefix, head, expected_observed: .checks}' "$RECEIPT"
}

main "$@"
