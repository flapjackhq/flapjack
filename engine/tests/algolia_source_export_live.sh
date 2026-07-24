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
SERVER_LAUNCH_PID=""
SERVER_SERVING_PID=""
SERVER_PORT=""
SERVER_RUNTIME_OVERRIDE=0
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
MIG5_IMPORT_BARRIER_DIR=""
MIG5_HA_DATA_DIR=""
MIG5_HA_SERVER_LOG=""

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
  MIG5_HA_DATA_DIR="$WORK_DIR/flapjack-ha-data"
  LOG_DIR="$WORK_DIR/logs"
  RECEIPT="$WORK_DIR/receipt.json"
  mkdir -p "$DATA_DIR" "$MIG5_HA_DATA_DIR" "$LOG_DIR"
  SERVER_LOG="$LOG_DIR/flapjack-server.log"
  MIG5_HA_SERVER_LOG="$LOG_DIR/flapjack-ha-server.log"

  local random_hex
  random_hex="$(od -An -N8 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  [ -n "$random_hex" ] || die "failed to generate run entropy"
  RUN_PREFIX="fj_stage4_${random_hex}"
  ADMIN_KEY="fj_live_$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  DRIFT_BARRIER_DIR="$WORK_DIR/drift-barrier"
  DRIFT_BARRIER_OBSERVED_FILE="$DRIFT_BARRIER_DIR/observed"
  DRIFT_BARRIER_RELEASE_FILE="$DRIFT_BARRIER_DIR/release"
  MIG5_IMPORT_BARRIER_DIR="$WORK_DIR/mig5-import-barrier"
  mkdir -p "$DRIFT_BARRIER_DIR" "$MIG5_IMPORT_BARRIER_DIR"
  export FLAPJACK_ALGOLIA_LIVE_TEST_DRIFT_SOURCE="${RUN_PREFIX}_drift"
  export FLAPJACK_ALGOLIA_LIVE_TEST_DRIFT_BARRIER_DIR="$DRIFT_BARRIER_DIR"
  export FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_SOURCE="${RUN_PREFIX}_mig5_source"
  export FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_BARRIER_DIR="$MIG5_IMPORT_BARRIER_DIR"

  : >"$WORK_DIR/created_indexes.txt"
  : >"$WORK_DIR/created_keys.txt"
  jq -n --arg prefix "$RUN_PREFIX" --arg head "$(git -C "$REPO_DIR" rev-parse HEAD 2>/dev/null || true)" \
    '{prefix:$prefix, head:$head, runtime_override:false, servers:[], created_indexes:[], created_key_fingerprints:[], checks:[]}' >"$RECEIPT"
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

record_transcript_metadata() {
  local next
  next="$(mktemp)"
  jq --arg started_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg command "bash engine/tests/algolia_source_export_live.sh --secret-file <redacted>" \
    --arg stage2_product "31787aaf4d117a9183825d0a717081b1e3d779f3" \
    '.transcript_metadata = {started_at_utc:$started_at, command:$command, stage2_product_commit:$stage2_product}' \
    "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
  record_check "transcript_metadata" "pass" "recorded sanitized command, UTC timestamp, and Stage 2 product SHA"
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

flapjack_request_capture() {
  local method="$1" path="$2" body="${3:-}" body_out="$4" headers_out="$5"
  if [ -n "$body" ]; then
    curl -sS -D "$headers_out" -o "$body_out" -w '%{http_code}' -X "$method" "${BASE_URL}${path}" \
      -H "x-algolia-application-id: flapjack" \
      -H "x-algolia-api-key: ${ADMIN_KEY}" \
      -H 'content-type: application/json' \
      --data "$body"
  else
    curl -sS -D "$headers_out" -o "$body_out" -w '%{http_code}' -X "$method" "${BASE_URL}${path}" \
      -H "x-algolia-application-id: flapjack" \
      -H "x-algolia-api-key: ${ADMIN_KEY}" \
      -H 'content-type: application/json'
  fi
}

urlencode() {
  python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$1"
}

wait_flapjack_task() {
  local index="$1" task_id="$2" label="$3" encoded out response code payload
  encoded="$(urlencode "$index")"
  out="$LOG_DIR/${label}-task-${task_id}.json"
  for _ in $(seq 1 120); do
    response="$(flapjack_request GET "/1/indexes/${encoded}/task/${task_id}" "")"
    code="$(printf '%s\n' "$response" | http_code)"
    payload="$(printf '%s\n' "$response" | http_body)"
    printf '%s\n' "$payload" >"$out"
    if [ "$code" = "200" ] && [ "$(jq -r '.status // empty' "$out" 2>/dev/null)" = "published" ]; then
      return 0
    fi
    sleep 0.5
  done
  die "Flapjack task ${task_id} for ${index} did not publish"
}

flapjack_batch() {
  local index="$1" body="$2" label="$3" encoded response code payload task
  encoded="$(urlencode "$index")"
  response="$(flapjack_request POST "/1/indexes/${encoded}/batch" "$body")"
  code="$(printf '%s\n' "$response" | http_code)"
  payload="$(printf '%s\n' "$response" | http_body)"
  printf '%s\n' "$payload" >"$LOG_DIR/${label}.json"
  [ "$code" = "200" ] || die "${label} batch returned HTTP ${code}"
  task="$(jq -er '.taskID' "$LOG_DIR/${label}.json")" || die "${label} batch response was missing taskID"
  wait_flapjack_task "$index" "$task" "$label"
  printf '%s\n' "$task"
}

collect_flapjack_query_ids() {
  local index="$1" out="$2" page=0 page_size=100 encoded ids_file response code payload nb_pages
  encoded="$(urlencode "$index")"
  ids_file="$(mktemp)"
  : >"$ids_file"
  while :; do
    response="$(flapjack_request POST "/1/indexes/${encoded}/query" "$(jq -cn --argjson page "$page" --argjson hits "$page_size" '{query:"",page:$page,hitsPerPage:$hits}')")"
    code="$(printf '%s\n' "$response" | http_code)"
    payload="$(printf '%s\n' "$response" | http_body)"
    printf '%s\n' "$payload" >"$LOG_DIR/query-${index}-${page}.json"
    [ "$code" = "200" ] || die "query ${index} page ${page} returned HTTP ${code}"
    jq -r '.hits[]?.objectID' "$LOG_DIR/query-${index}-${page}.json" >>"$ids_file"
    nb_pages="$(jq -er '.nbPages' "$LOG_DIR/query-${index}-${page}.json")" || die "query ${index} page ${page} missing nbPages"
    if [ "$page" -eq 0 ]; then
      jq -er '.nbHits' "$LOG_DIR/query-${index}-${page}.json" >"$LOG_DIR/query-${index}-nbHits.txt" \
        || die "query ${index} missing nbHits"
    fi
    if [ $((page + 1)) -ge "$nb_pages" ]; then
      break
    fi
    page=$((page + 1))
  done
  python3 - "$ids_file" "$out" <<'PY'
import json, sys
values = [line.strip() for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
json.dump(sorted(values), open(sys.argv[2], "w", encoding="utf-8"), separators=(",", ":"))
PY
  rm -f "$ids_file"
}

build_or_resolve_binary() {
  if [ -n "${FLAPJACK_BIN:-}" ]; then
    [ -x "$FLAPJACK_BIN" ] || die "FLAPJACK_BIN is not executable"
    BIN_PATH="$FLAPJACK_BIN"
    SERVER_RUNTIME_OVERRIDE=1
    return
  fi
  BIN_PATH=""
  SERVER_RUNTIME_OVERRIDE=0
}

discover_serving_pid() {
  local launcher_pid="$1" runtime_override="$2"
  if [ "$runtime_override" -eq 1 ]; then
    printf '%s\n' "$launcher_pid"
    return 0
  fi
  python3 - "$launcher_pid" <<'PY'
import os, pathlib, subprocess, sys
launcher = int(sys.argv[1])
rows = subprocess.check_output(["ps", "-ax", "-o", "pid=", "-o", "ppid=", "-o", "comm="], text=True).splitlines()
children = {}
commands = {}
for row in rows:
    parts = row.strip().split(None, 2)
    if len(parts) < 3:
        continue
    pid, ppid, comm = int(parts[0]), int(parts[1]), parts[2]
    children.setdefault(ppid, []).append(pid)
    commands[pid] = comm
stack = [launcher]
seen = set()
candidates = []
while stack:
    pid = stack.pop()
    if pid in seen:
        continue
    seen.add(pid)
    comm = commands.get(pid, "")
    if pathlib.Path(comm).name == "flapjack":
        candidates.append(pid)
    stack.extend(children.get(pid, []))
if not candidates and pathlib.Path(commands.get(launcher, "")).name == "flapjack":
    candidates.append(launcher)
candidates = sorted(set(candidates))
if len(candidates) != 1:
    raise SystemExit(f"ambiguous serving flapjack process lineage for launcher {launcher}: {candidates}")
print(candidates[0])
PY
}

record_server_start() {
  local label="$1" data_dir="$2" log_path="$3"
  local next
  next="$(mktemp)"
  jq --arg label "$label" \
    --arg data_dir "$data_dir" \
    --arg log_path "$log_path" \
    --argjson launcher "$SERVER_LAUNCH_PID" \
    --argjson serving "$SERVER_SERVING_PID" \
    --argjson port "$SERVER_PORT" \
    --argjson runtime_override "$SERVER_RUNTIME_OVERRIDE" \
    '.runtime_override = $runtime_override
     | .servers += [{label:$label,data_dir:$data_dir,log_path:$log_path,launcher_pid:$launcher,serving_pid:$serving,port:$port,runtime_override:$runtime_override,launch:"cargo run -p flapjack-server --bin flapjack -- --data-dir <isolated-temp-dir> --auto-port"}]' \
    "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

start_server() {
  local label="${1:-standalone}" data_dir="${2:-$DATA_DIR}" log_path="${3:-$SERVER_LOG}"
  if [ "$SERVER_RUNTIME_OVERRIDE" -eq 1 ]; then
    ALGOLIA_LIVE_TEST_SERVER_LABEL="$label" \
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
      FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_SOURCE="${RUN_PREFIX}_mig5_source" \
      FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_BARRIER_DIR="$MIG5_IMPORT_BARRIER_DIR" \
      FLAPJACK_DATA_DIR="$data_dir" \
      "$BIN_PATH" --auto-port >"$log_path" 2>&1 &
  elif [ "$label" = "ha" ]; then
    (
      export FLAPJACK_NODE_ID="mig5-live-proof"
      export FLAPJACK_PEERS="migration-peer=http://10.0.0.2:7700"
      export FLAPJACK_STARTUP_CATCHUP_STRICT=0
      export FLAPJACK_STARTUP_CATCHUP_TIMEOUT_SECS=2
      export FLAPJACK_ADMIN_KEY="$ADMIN_KEY"
      export FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_SOURCE="${RUN_PREFIX}_mig5_source"
      export FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_BARRIER_DIR="$MIG5_IMPORT_BARRIER_DIR"
      cd "$ENGINE_DIR" && cargo run -p flapjack-server --bin flapjack -- --data-dir "$data_dir" --auto-port
    ) >"$log_path" 2>&1 &
  else
    (
      export FLAPJACK_ADMIN_KEY="$ADMIN_KEY"
      export FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_SOURCE="${RUN_PREFIX}_mig5_source"
      export FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_BARRIER_DIR="$MIG5_IMPORT_BARRIER_DIR"
      cd "$ENGINE_DIR" && cargo run -p flapjack-server --bin flapjack -- --data-dir "$data_dir" --auto-port
    ) >"$log_path" 2>&1 &
  fi
  SERVER_LAUNCH_PID=$!
  SERVER_PID="$SERVER_LAUNCH_PID"

  "$WAIT_HELPER" --pid "$SERVER_LAUNCH_PID" --host 127.0.0.1 --port auto --log-path "$log_path" --retries 600 --interval-seconds 0.5
  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$log_path" | head -1)"
  [ -n "$port" ] || die "server became ready but no auto-port was found"
  SERVER_PORT="$port"
  SERVER_SERVING_PID="$(discover_serving_pid "$SERVER_LAUNCH_PID" "$SERVER_RUNTIME_OVERRIDE")" \
    || die "could not discover exact serving flapjack PID"
  BASE_URL="http://127.0.0.1:${port}"
  record_server_start "$label" "$data_dir" "$log_path"
  record_check "local_server" "pass" "started ${label} launcher=${SERVER_LAUNCH_PID} serving=${SERVER_SERVING_PID} port=${SERVER_PORT}"
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
    {"objectID": "syn-2", "type": "synonym", "synonyms": ["tee", "tshirt", "t-shirt"]},
    {"objectID": "syn-3", "type": "synonym", "synonyms": ["flapjack", "flapjacks"]},
]
settings = {
    "searchableAttributes": ["title", "category"],
    "attributesForFaceting": ["category"],
    "unretrievableAttributes": ["secret_note"],
    "disableTypoToleranceOnAttributes": ["secret_note"],
    "paginationLimitedTo": 2000,
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
    "synonyms": {"count": len(synonyms), "ids": [s["objectID"] for s in synonyms], "hash": "b975f85b4db6cbb6e229071fb26c1b4378c7cda5e94d3fb915f8f9e67c815d70"},
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
  local mig5_source_index="${RUN_PREFIX}_mig5_source"
  local aux_a="${RUN_PREFIX}_aux_a"
  local aux_b="${RUN_PREFIX}_aux_b"
  local drift_index="${RUN_PREFIX}_drift"
  seed_index "$source_index" "$WORK_DIR/documents.json"
  seed_rules_synonyms "$source_index"
  seed_index "$mig5_source_index" "$WORK_DIR/documents.json"
  seed_rules_synonyms "$mig5_source_index"
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
successful = []
for path in job_dirs:
    manifest = json.load(open(os.path.join(path, "manifest.json")))
    if manifest["lifecycle"] in ("Accepted", "Deleted"):
        successful.append((path, manifest))
if len(successful) != 1:
    raise SystemExit(f"expected exactly one successful job, got {len(successful)}")
path, manifest = successful[0]

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

checks = [("documents", "DocumentPage"), ("rules", "RulesPage"), ("synonyms", "SynonymsPage")]
summary = {}
if manifest["lifecycle"] == "Accepted":
    resources = {"DocumentPage": [], "RulesPage": [], "SynonymsPage": []}
    for artifact in manifest["artifacts"]:
        if artifact["state"] != "Visible":
            raise SystemExit("manifest contains non-visible artifact after acceptance")
        if artifact["kind"] in resources:
            resources[artifact["kind"]].extend(json.load(open(os.path.join(path, artifact["final_path"]))))

    for name, kind in checks:
        ids = sorted(item["objectID"] for item in resources[kind])
        if ids != sorted(expected[name]["ids"]):
            raise SystemExit(f"{name} membership mismatch")
        if aggregate(resources[kind]) != expected[name]["hash"]:
            raise SystemExit(f"{name} hash mismatch")
        if manifest["resource_completions"][name]["hash"] != expected[name]["hash"]:
            raise SystemExit(f"{name} completion hash mismatch")
        summary[name] = {"count": len(ids), "hash": expected[name]["hash"], "lifecycle": "Accepted"}

    for sidecar_name, expected_name in [("completed_objects", "documents"), ("completed_rules", "rules"), ("completed_synonyms", "synonyms")]:
        if manifest[sidecar_name]["count"] != expected[expected_name]["count"]:
            raise SystemExit(f"{sidecar_name} count mismatch")
else:
    if manifest["artifacts"]:
        raise SystemExit("deleted successful job retained artifacts")
    for name, _kind in checks:
        completion = manifest["resource_completions"][name]
        if not completion.get("complete"):
            raise SystemExit(f"{name} completion missing complete=true")
        if completion["count"] != expected[name]["count"]:
            raise SystemExit(f"{name} completion count mismatch")
        if completion["hash"] != expected[name]["hash"]:
            raise SystemExit(f"{name} completion hash mismatch")
        summary[name] = {"count": completion["count"], "hash": expected[name]["hash"], "lifecycle": "Deleted"}
    for sidecar_name in ["completed_objects", "completed_rules", "completed_synonyms"]:
        if manifest[sidecar_name]["count"] != 0:
            raise SystemExit(f"{sidecar_name} should be empty after synchronous import cleanup")
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
  body="$(jq -n --arg app "$ALGOLIA_APP_ID" --arg key "$permitted" --arg src "$source_index" '{appId:$app,apiKey:$key,sourceIndex:$src}')"
  response="$(flapjack_request POST "/1/migrate-from-algolia" "$body")"
  code="$(printf '%s\n' "$response" | http_code)"
  payload="$(printf '%s\n' "$response" | http_body)"
  printf '%s\n' "$payload" >"$LOG_DIR/permitted-migration.json"
  [ "$code" = "200" ] || die "permitted migration returned HTTP ${code}"
  jq -e '.status == "complete" and .settings == true and .objects.imported == 1005 and .rules.imported == 3 and .synonyms.imported == 3 and .taskID == 0' "$LOG_DIR/permitted-migration.json" >/dev/null
  summary="$(inspect_spool)"

  flapjack_request GET "/1/indexes" >"$LOG_DIR/denied-before.raw"
  printf '%s\n' "$(cat "$LOG_DIR/denied-before.raw" | http_body)" >"$LOG_DIR/denied-before.json"
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

  flapjack_request GET "/1/indexes" >"$LOG_DIR/denied-after.raw"
  printf '%s\n' "$(cat "$LOG_DIR/denied-after.raw" | http_body)" >"$LOG_DIR/denied-after.json"
  assert_no_destination_index "$LOG_DIR/denied-before.json" "$LOG_DIR/denied-after.json"
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

job_count_snapshot() {
  local root="$1"
  if [ -d "$root/migration_exports/jobs" ]; then
    find "$root/migration_exports/jobs" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l | tr -d ' '
  else
    printf '0\n'
  fi
}

directory_manifest() {
  local root="$1" out="$2"
  if [ -d "$root/migration_exports/jobs" ]; then
    (cd "$root/migration_exports/jobs" && find . -type f -print | LC_ALL=C sort | xargs shasum -a 256 2>/dev/null || true) >"$out"
  else
    : >"$out"
  fi
}

wait_for_import_barrier() {
  local observed="$MIG5_IMPORT_BARRIER_DIR/observed"
  for _ in $(seq 1 240); do
    if [ -s "$observed" ]; then
      return 0
    fi
    sleep 0.1
  done
  die "MIG-5 import barrier was not observed"
}

release_import_barrier() {
  : >"$MIG5_IMPORT_BARRIER_DIR/release"
}

mig5_seed_target() {
  local target_index="$1" stale_body
  stale_body="$(jq -n --arg a "${RUN_PREFIX}_stale_0001" --arg b "${RUN_PREFIX}_stale_0002" \
    '{requests:[{action:"addObject",body:{objectID:$a,title:"stale one"}},{action:"addObject",body:{objectID:$b,title:"stale two"}}]}')"
  flapjack_batch "$target_index" "$stale_body" "mig5-stale-seed" >/dev/null
  jq -n --arg a "${RUN_PREFIX}_stale_0001" --arg b "${RUN_PREFIX}_stale_0002" '[$a,$b]' >"$LOG_DIR/mig5-stale-ids.json"
  record_check "mig5_stale_seed" "pass" "seeded 2 stale target IDs"
}

mig5_overlap_write() {
  local target_index="$1" object_id="$2" body body_file headers_file code task
  body="$(jq -n --arg id "$object_id" --arg title "overlap ${object_id}" \
    '{requests:[{action:"addObject",body:{objectID:$id,title:$title,mig5_overlap:true}}]}')"
  body_file="$LOG_DIR/mig5-overlap-response.json"
  headers_file="$LOG_DIR/mig5-overlap-response.headers"
  code="$(flapjack_request_capture POST "/1/indexes/$(urlencode "$target_index")/batch" "$body" "$body_file" "$headers_file")"
  printf '%s\n' "$code" >"$LOG_DIR/mig5-overlap-status.txt"
  case "$code" in
    200)
      task="$(jq -er '.taskID' "$body_file")" || die "overlap HTTP 200 response was missing taskID"
      printf '%s\n' "$task" >"$LOG_DIR/mig5-overlap-success-task-ids.txt"
      wait_flapjack_task "$target_index" "$task" "mig5-overlap"
      jq -n --arg id "$object_id" '[$id]' >"$LOG_DIR/mig5-overlap-published-ids.json"
      printf '200\n' >"$LOG_DIR/mig5-overlap-success-count.txt"
      ;;
    503)
      grep -Fiq 'Retry-After: 1' "$headers_file" || die "overlap 503 missing Retry-After: 1"
      jq -e '.message == "Index is temporarily unavailable" and .status == 503' "$body_file" >/dev/null \
        || die "overlap 503 body did not match IndexPaused JSON identity"
      : >"$LOG_DIR/mig5-overlap-success-task-ids.txt"
      jq -n '[]' >"$LOG_DIR/mig5-overlap-published-ids.json"
      printf '503\n' >"$LOG_DIR/mig5-overlap-refused-count.txt"
      ;;
    *)
      die "overlap write returned HTTP ${code}, expected 200 or retryable 503"
      ;;
  esac
}

run_mig5_overwrite_scenario() {
  local source_index="${RUN_PREFIX}_mig5_source" target_index="${RUN_PREFIX}_mig5_target"
  local permitted body curl_out curl_pid response code payload attempted completed success_count refused_count
  local expected_ids final_ids nb_hits expected_count
  permitted="$(cat "$WORK_DIR/permitted.key")"
  mig5_seed_target "$target_index"

  rm -f "$MIG5_IMPORT_BARRIER_DIR/observed" "$MIG5_IMPORT_BARRIER_DIR/release"
  body="$(jq -n --arg app "$ALGOLIA_APP_ID" --arg key "$permitted" --arg src "$source_index" --arg target "$target_index" \
    '{appId:$app,apiKey:$key,sourceIndex:$src,targetIndex:$target,overwrite:true}')"
  jq -n --arg source "$source_index" --arg target "$target_index" \
    '{endpoint:"POST /1/migrate-from-algolia",sourceIndex:$source,targetIndex:$target,overwrite:true,apiKey:"<redacted>"}' \
    >"$LOG_DIR/mig5-overwrite-request.sanitized.json"
  curl_out="$LOG_DIR/mig5-overwrite-migration.raw"
  (flapjack_request POST "/1/migrate-from-algolia" "$body" >"$curl_out") &
  curl_pid=$!
  wait_for_import_barrier
  kill -0 "$curl_pid" 2>/dev/null || die "MIG-5 overwrite migration was not in flight at overlap point"

  attempted=1
  mig5_overlap_write "$target_index" "${RUN_PREFIX}_overlap_0001"
  completed=1
  release_import_barrier
  wait "$curl_pid" || true
  response="$(cat "$curl_out")"
  code="$(printf '%s\n' "$response" | http_code)"
  payload="$(printf '%s\n' "$response" | http_body)"
  printf '%s\n' "$payload" >"$LOG_DIR/mig5-overwrite-migration.json"
  [ "$code" = "200" ] || die "MIG-5 overwrite migration returned HTTP ${code}, expected 200"
  jq -e '.status == "complete" and .settings == true and .objects.imported == 1005 and .rules.imported == 3 and .synonyms.imported == 3 and .taskID == 0' \
    "$LOG_DIR/mig5-overwrite-migration.json" >/dev/null || die "MIG-5 overwrite migration response did not report exact completion counts"

  jq -c '.documents.ids' "$WORK_DIR/expected.json" >"$LOG_DIR/mig5-replacement-ids.json"
  jq -s '.[0] + .[1] | sort' "$LOG_DIR/mig5-replacement-ids.json" "$LOG_DIR/mig5-overlap-published-ids.json" >"$LOG_DIR/mig5-expected-final-ids.json"
  collect_flapjack_query_ids "$target_index" "$LOG_DIR/mig5-final-ids.json"
  expected_ids="$(jq -c 'sort' "$LOG_DIR/mig5-expected-final-ids.json")"
  final_ids="$(jq -c 'sort' "$LOG_DIR/mig5-final-ids.json")"
  [ "$final_ids" = "$expected_ids" ] || die "MIG-5 final target mismatch: duplicate, missing, unexpected, or success-acknowledged-but-absent IDs"
  nb_hits="$(cat "$LOG_DIR/query-${target_index}-nbHits.txt")"
  expected_count="$(jq 'length' "$LOG_DIR/mig5-expected-final-ids.json")"
  [ "$nb_hits" = "$expected_count" ] || die "MIG-5 nbHits ${nb_hits} did not equal hand-calculated ${expected_count}; loose hit-count checks are forbidden"
  jq -e --slurpfile stale "$LOG_DIR/mig5-stale-ids.json" '. as $ids | all($stale[0][]; . as $id | ($ids | index($id) | not))' "$LOG_DIR/mig5-final-ids.json" >/dev/null \
    || die "MIG-5 stale seeded ID survived overwrite"

  success_count="$(jq 'length' "$LOG_DIR/mig5-overlap-published-ids.json")"
  refused_count="$(if [ -f "$LOG_DIR/mig5-overlap-refused-count.txt" ]; then echo 1; else echo 0; fi)"
  [ "$attempted" -gt 0 ] || die "MIG-5 overlap_attempted denominator was zero"
  [ "$completed" -gt 0 ] || die "MIG-5 overlap_completed denominator was zero"
  jq -n --argjson attempted "$attempted" --argjson completed "$completed" --argjson success "$success_count" --argjson refused "$refused_count" \
    '{overlap_attempted:$attempted,overlap_completed:$completed,overlap_http_200:$success,overlap_retryable_refused:$refused}' \
    >"$LOG_DIR/mig5-overlap-counters.json"
  record_check "mig5_overwrite_exact_membership" "pass" "nbHits=${nb_hits}; overlap_attempted=${attempted}; overlap_completed=${completed}; overlap_success=${success_count}; overlap_refused=${refused_count}"
}

run_mig5_async_and_ha_refusals() {
  local source_index="${RUN_PREFIX}_mig5_source" target_index="${RUN_PREFIX}_mig5_async_target"
  local permitted body code before_count after_count before_manifest after_manifest
  local body_file headers_file
  permitted="$(cat "$WORK_DIR/permitted.key")"
  before_count="$(job_count_snapshot "$DATA_DIR")"
  before_manifest="$LOG_DIR/mig5-async-jobs-before.txt"
  after_manifest="$LOG_DIR/mig5-async-jobs-after.txt"
  directory_manifest "$DATA_DIR" "$before_manifest"
  body="$(jq -n --arg app "$ALGOLIA_APP_ID" --arg key "$permitted" --arg src "$source_index" --arg target "$target_index" \
    '{appId:$app,apiKey:$key,sourceIndex:$src,targetIndex:$target,overwrite:true}')"
  body_file="$LOG_DIR/mig5-async-overwrite-refusal.json"
  headers_file="$LOG_DIR/mig5-async-overwrite-refusal.headers"
  code="$(flapjack_request_capture POST "/1/migrations/algolia" "$body" "$body_file" "$headers_file")"
  [ "$code" = "400" ] || die "async overwrite expected HTTP 400, got ${code}"
  jq -e '.message == "overwrite=true is not supported by Algolia migration import" and .status == 400' "$body_file" >/dev/null \
    || die "async overwrite refusal body drifted"
  after_count="$(job_count_snapshot "$DATA_DIR")"
  directory_manifest "$DATA_DIR" "$after_manifest"
  [ "$after_count" = "$before_count" ] || die "async overwrite refusal created a migration job artifact"
  cmp -s "$before_manifest" "$after_manifest" || die "async overwrite refusal changed migration job contents"
  record_check "mig5_async_overwrite_refusal" "pass" "HTTP 400 before source access; jobs ${before_count}->${after_count}"

  stop_server
  start_server "ha" "$MIG5_HA_DATA_DIR" "$MIG5_HA_SERVER_LOG"
  body_file="$LOG_DIR/mig5-ha-overwrite-refusal.json"
  headers_file="$LOG_DIR/mig5-ha-overwrite-refusal.headers"
  code="$(flapjack_request_capture POST "/1/migrate-from-algolia" "$body" "$body_file" "$headers_file")"
  [ "$code" = "400" ] || die "HA overwrite expected overwrite-specific HTTP 400, got ${code}"
  jq -e '.message == "overwrite=true is not supported by Algolia migration import" and .status == 400' "$body_file" >/dev/null \
    || die "HA overwrite refusal body drifted"

  body="$(jq -n --arg app "$ALGOLIA_APP_ID" --arg key "$permitted" --arg src "$source_index" --arg target "${RUN_PREFIX}_mig5_ha_create" \
    '{appId:$app,apiKey:$key,sourceIndex:$src,targetIndex:$target,overwrite:false}')"
  body_file="$LOG_DIR/mig5-ha-create-refusal.json"
  headers_file="$LOG_DIR/mig5-ha-create-refusal.headers"
  code="$(flapjack_request_capture POST "/1/migrate-from-algolia" "$body" "$body_file" "$headers_file")"
  [ "$code" = "503" ] || die "HA overwrite=false expected HTTP 503, got ${code}"
  jq -e '.code == "migration_ha_unsupported" and .message == "Algolia migration import is unavailable on HA clusters until MIG-7 supplies a costed convergence protocol."' "$body_file" >/dev/null \
    || die "HA migration_ha_unsupported body drifted"
  [ "$(job_count_snapshot "$MIG5_HA_DATA_DIR")" = "0" ] || die "HA refusal created a migration artifact"
  record_check "mig5_ha_refusals" "pass" "peer_count=1; overwrite true HTTP 400; overwrite false HTTP 503 migration_ha_unsupported"
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
  fi
  if [ -n "$LOG_DIR" ] && [ -d "$LOG_DIR" ]; then
    mkdir -p "$EVIDENCE_DIR/logs"
    cp -R "$LOG_DIR/." "$EVIDENCE_DIR/logs/" 2>/dev/null || true
  fi
  [ -n "$RECEIPT" ] && [ -f "$RECEIPT" ] && cp "$RECEIPT" "$EVIDENCE_DIR/receipt.json" 2>/dev/null || true
  if [ -n "$DATA_DIR" ] && [ -d "$DATA_DIR/migration_exports/jobs" ] \
    && [ ! -d "$EVIDENCE_DIR/migration_exports/jobs" ]; then
    mkdir -p "$EVIDENCE_DIR/migration_exports"
    cp -R "$DATA_DIR/migration_exports/jobs" "$EVIDENCE_DIR/migration_exports/jobs" 2>/dev/null || true
  fi
  if [ "$announce" -eq 1 ] && [ "$EVIDENCE_ANNOUNCED" -eq 0 ]; then
    printf 'INFO: preserved sanitized live evidence at %s\n' "$EVIDENCE_DIR" >&2
    EVIDENCE_ANNOUNCED=1
  fi
}

stop_server() {
  local label="${1:-server}" serving_status="not-started" launcher_status="not-started" cleanup_status="pass"
  [ -n "$SERVER_LAUNCH_PID" ] || return 0
  # unrelated PID cleanup is forbidden; this function only targets the recorded
  # serving and launcher PIDs owned by this lane.
  if [ -n "$SERVER_SERVING_PID" ] && [ "$SERVER_SERVING_PID" != "$SERVER_LAUNCH_PID" ] && kill -0 "$SERVER_SERVING_PID" 2>/dev/null; then
    kill "$SERVER_SERVING_PID" 2>/dev/null || true
    for _ in $(seq 1 50); do
      kill -0 "$SERVER_SERVING_PID" 2>/dev/null || break
      sleep 0.1
    done
    if kill -0 "$SERVER_SERVING_PID" 2>/dev/null; then
      serving_status="still-running"
      CLEANUP_FAILED=1
      cleanup_status="fail"
    else
      serving_status="gone"
    fi
  elif [ -n "$SERVER_SERVING_PID" ]; then
    serving_status="same-as-launcher-or-gone"
  fi
  if kill -0 "$SERVER_LAUNCH_PID" 2>/dev/null; then
    kill "$SERVER_LAUNCH_PID" 2>/dev/null || true
  fi
  if wait "$SERVER_LAUNCH_PID" 2>/dev/null; then
    launcher_status="0"
  else
    launcher_status="$?"
  fi
  if kill -0 "$SERVER_LAUNCH_PID" 2>/dev/null; then
    CLEANUP_FAILED=1
    cleanup_status="fail"
    launcher_status="still-running"
  fi
  printf 'label=%s launcher_pid=%s launcher_status=%s serving_pid=%s serving_status=%s\n' \
    "$label" "$SERVER_LAUNCH_PID" "$launcher_status" "${SERVER_SERVING_PID:-}" "$serving_status" \
    >>"$LOG_DIR/server-cleanup-status.txt"
  record_check "server_cleanup_${label}" "$cleanup_status" "launcher=${SERVER_LAUNCH_PID}; serving=${SERVER_SERVING_PID:-}; launcher_status=${launcher_status}; serving_status=${serving_status}"
  SERVER_LAUNCH_PID=""
  SERVER_SERVING_PID=""
  SERVER_PID=""
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
  stop_server "cleanup"
  # Refresh the staged copy with the final owned-PID exit statuses before
  # removing the temporary work directory.
  preserve_run_evidence 0
  if [ -n "$DATA_DIR" ] && [ -d "$DATA_DIR" ]; then
    rm -rf "$DATA_DIR"
  fi
  if [ -n "$MIG5_HA_DATA_DIR" ] && [ -d "$MIG5_HA_DATA_DIR" ]; then
    rm -rf "$MIG5_HA_DATA_DIR"
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
  record_transcript_metadata
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
  run_mig5_overwrite_scenario
  run_mig5_async_and_ha_refusals
  PASS_COMPLETE=1
  record_check "cleanup_precheck" "pass" "all live assertions completed"
  jq '{prefix, head, expected_observed: .checks}' "$RECEIPT"
}

main "$@"
