#!/usr/bin/env bash
set -euo pipefail

SCRIPT_SOURCE="${BASH_SOURCE[0]}"
if [[ "$SCRIPT_SOURCE" != */* ]]; then
  SCRIPT_SOURCE="./$SCRIPT_SOURCE"
fi
SCRIPT_DIR="${SCRIPT_SOURCE%/*}"
ENGINE_DIR="$SCRIPT_DIR/../.."
REPO_ROOT="$ENGINE_DIR/.."
RESULTS_BASE_DIR="$ENGINE_DIR/loadtest/results/chaos-residual"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.yml"
SUBNET_COMPOSE_FILE="$SCRIPT_DIR/docker_compose_explicit_subnet.yml"
REQUIRED_COMMANDS=(cat cmp date docker git grep jq mkdir python3 sed seq sleep sort tail tr wc)

RUN_ID=""
COMPOSE_PROJECT_NAME=""
COMPOSE_ARGS=()
RESULTS_DIR=""
INDEX_NAME=""
NETWORK_ID=""
PASS_COUNT=0
FAIL_COUNT=0
OUTCOME="INCONCLUSIVE"
SCRIPT_EXIT_CODE=0
INTERRUPTED_EXIT_CODE=0
CLEANUP_COMPLETE=false
SKIP_TEARDOWN=false

STARTER_TSV=""
NODE_A_TSV=""
NODE_C_TSV=""
UNION_TSV=""
ALL_DOCS_TSV=""
ACK_NODE_A_IDS=""
ACK_NODE_C_IDS=""
REJECTED_IDS=""
AMBIGUOUS_IDS=""
NO_ACK_IDS=""
SUMMARY_LINE_PATH=""
SUMMARY_JSON_PATH=""

green() { printf "\033[32mPASS\033[0m %s\n" "$*"; }
red() { printf "\033[31mFAIL\033[0m %s\n" "$*" >&2; }

record_pass() {
  PASS_COUNT=$((PASS_COUNT + 1))
  green "$1"
}

fail() {
  FAIL_COUNT=$((FAIL_COUNT + 1))
  OUTCOME="FAIL"
  red "$1"
  return 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

preflight() {
  for command_name in "${REQUIRED_COMMANDS[@]}"; do
    require_command "$command_name"
  done
  docker info >/dev/null 2>&1 || fail "docker daemon is not reachable"
  docker compose version >/dev/null 2>&1 || fail "docker compose support is required"
  [ -f "$COMPOSE_FILE" ] || fail "missing compose file: $COMPOSE_FILE"
  if [[ -n "${FLAPJACK_HA_NETWORK_SUBNET:-}" ]]; then
    [ -f "$SUBNET_COMPOSE_FILE" ] || fail "missing explicit-subnet compose file: $SUBNET_COMPOSE_FILE"
  fi
  [ -f "$SCRIPT_DIR/_ha_lib.sh" ] || fail "missing HA helper library"
}

configure_run_identity() {
  RUN_ID="${EPOCHSECONDS:-$SECONDS}_$$_${RANDOM}_${RANDOM}"
  COMPOSE_PROJECT_NAME="flapjack_ha_partition_${RUN_ID}"
  COMPOSE_ARGS=(-p "$COMPOSE_PROJECT_NAME" -f "$COMPOSE_FILE")
  if [[ -n "${FLAPJACK_HA_NETWORK_SUBNET:-}" ]]; then
    COMPOSE_ARGS+=(-f "$SUBNET_COMPOSE_FILE")
  fi

  RESULTS_DIR="$RESULTS_BASE_DIR/ha_partition_${RUN_ID}"
  INDEX_NAME="partition_${RUN_ID}"
  STARTER_TSV="$RESULTS_DIR/starter_expected.tsv"
  NODE_A_TSV="$RESULTS_DIR/node_a_acknowledged_expected.tsv"
  NODE_C_TSV="$RESULTS_DIR/node_c_acknowledged_expected.tsv"
  UNION_TSV="$RESULTS_DIR/acknowledged_union_expected.tsv"
  ALL_DOCS_TSV="$RESULTS_DIR/all_possible_docs.tsv"
  ACK_NODE_A_IDS="$RESULTS_DIR/acknowledged_node_a_ids.txt"
  ACK_NODE_C_IDS="$RESULTS_DIR/acknowledged_node_c_ids.txt"
  REJECTED_IDS="$RESULTS_DIR/rejected_ids.txt"
  AMBIGUOUS_IDS="$RESULTS_DIR/ambiguous_ids.txt"
  NO_ACK_IDS="$RESULTS_DIR/no_ack_ids.txt"
  SUMMARY_LINE_PATH="$RESULTS_DIR/summary_line.txt"
  SUMMARY_JSON_PATH="$RESULTS_DIR/summary.json"
}

source_ha_lib() {
  . "$SCRIPT_DIR/_ha_lib.sh"
}

initialize_results() {
  mkdir -p "$RESULTS_DIR"
  : >"$ACK_NODE_A_IDS"
  : >"$ACK_NODE_C_IDS"
  : >"$REJECTED_IDS"
  : >"$AMBIGUOUS_IDS"
  : >"$NO_ACK_IDS"
  git -C "$REPO_ROOT" rev-parse HEAD >"$RESULTS_DIR/source_sha.txt"
  printf '%s\n' "$COMPOSE_PROJECT_NAME" >"$RESULTS_DIR/compose_project_name.txt"
  printf '%s\n' "$INDEX_NAME" >"$RESULTS_DIR/index_name.txt"
}

doc_tsv() {
  local prefix="$1"
  shift
  python3 - "$RUN_ID" "$prefix" "$@" <<'PY'
import sys

run_id, prefix, *ids = sys.argv[1:]
for object_id in ids:
    print(f"{object_id}\t{prefix} body {object_id} {run_id}")
PY
}

write_expected_sets() {
  local starter_1="starter_${RUN_ID}_1"
  local starter_2="starter_${RUN_ID}_2"
  local node_a_1="node_a_${RUN_ID}_1"
  local node_a_2="node_a_${RUN_ID}_2"
  local node_c_1="node_c_${RUN_ID}_1"

  doc_tsv starter "$starter_1" "$starter_2" >"$STARTER_TSV"
  cat "$STARTER_TSV" >"$NODE_A_TSV"
  doc_tsv node_a "$node_a_1" "$node_a_2" >>"$NODE_A_TSV"
  doc_tsv node_c "$node_c_1" >"$NODE_C_TSV"
  cat "$NODE_A_TSV" "$NODE_C_TSV" | sort >"$ALL_DOCS_TSV"
  : >"$UNION_TSV"
}

materialize_acknowledged_union() {
  sort -u "$ACK_NODE_A_IDS" "$ACK_NODE_C_IDS" >"$RESULTS_DIR/acknowledged_union_ids.txt"
  python3 - "$RESULTS_DIR/acknowledged_union_ids.txt" "$ALL_DOCS_TSV" >"$UNION_TSV" <<'PY'
import sys

ids_path, docs_path = sys.argv[1:]
acknowledged = {
    line.strip()
    for line in open(ids_path, encoding="utf-8")
    if line.strip()
}
with open(docs_path, encoding="utf-8") as handle:
    for raw in handle:
        object_id, body = raw.rstrip("\n").split("\t", 1)
        if object_id in acknowledged:
            print(f"{object_id}\t{body}")
PY
}

payload_for_tsv() {
  local tsv_path="$1"
  python3 - "$RUN_ID" "$tsv_path" <<'PY'
import json
import sys

run_id, tsv_path = sys.argv[1:]
requests = []
with open(tsv_path, encoding="utf-8") as handle:
    for raw in handle:
        object_id, body = raw.rstrip("\n").split("\t", 1)
        requests.append({
            "action": "addObject",
            "body": {
                "_id": object_id,
                "objectID": object_id,
                "title": f"Partition document {object_id}",
                "body": body,
                "run_id": run_id,
            },
        })
print(json.dumps({"requests": requests}, separators=(",", ":")))
PY
}

query_payload() {
  python3 - "$1" <<'PY'
import json
import sys
print(json.dumps({"query": sys.argv[1], "hitsPerPage": 100}))
PY
}

post_json_compose() {
  local service="$1" path="$2" payload="$3" response_path="$4" status_path="$5"
  local output status
  if output="$(compose exec -T "$service" curl -sS -w '\n%{http_code}' \
    -X POST "http://localhost:7700$path" \
    -H 'Content-Type: application/json' \
    -d "$payload" 2>"${response_path}.stderr")"; then
    status="$(printf '%s\n' "$output" | tail -n 1)"
    printf '%s\n' "$output" | sed '$d' >"$response_path"
    printf '%s\n' "$status" >"$status_path"
    return 0
  fi
  printf '000\n' >"$status_path"
  printf '{}\n' >"$response_path"
  return 1
}

search_json_compose() {
  local service="$1" index="$2" query="$3" output_path="$4" payload status_path
  payload="$(query_payload "$query")"
  status_path="${output_path}.status"
  post_json_compose "$service" "/1/indexes/$index/query" "$payload" "$output_path" "$status_path" >/dev/null || true
  cat "$output_path"
}

parse_nb_hits() {
  jq -r '.nbHits // 0'
}

parse_first_hit_field() {
  local field="$1"
  python3 -c '
import json
import sys

field = sys.argv[1]
data = json.load(sys.stdin)
hits = data.get("hits") or []
if not hits:
    print("")
    raise SystemExit(0)
value = hits[0].get(field, "")
if field == "_id" and value == "":
    value = hits[0].get("objectID", "")
print(value if not isinstance(value, (dict, list)) else json.dumps(value, sort_keys=True))
' "$field" 2>/dev/null || echo ""
}

response_tsv() {
  local response_path="$1"
  jq -r '.hits[]? | [(.objectID // ._id // ""), (.body // "")] | @tsv' "$response_path" | sort
}

assert_response_exact_set() {
  local response_path="$1" expected_tsv="$2" expected_count="$3" label="$4"
  local actual_count actual_tsv
  actual_count="$(jq -r '.nbHits // -1' "$response_path")"
  actual_tsv="${response_path}.tsv"
  response_tsv "$response_path" >"$actual_tsv"
  if [ "$actual_count" != "$expected_count" ]; then
    printf 'expected nbHits=%s got %s\n' "$expected_count" "$actual_count" >&2
    return 1
  fi
  if ! cmp -s <(sort "$expected_tsv") "$actual_tsv"; then
    printf 'exact-set mismatch for %s\nexpected:\n' "$label" >&2
    sort "$expected_tsv" >&2
    printf 'actual:\n' >&2
    cat "$actual_tsv" >&2
    return 1
  fi
}

wait_for_exact_set_compose() {
  local service="$1" label="$2" expected_tsv="$3" max_wait="$4"
  local expected_count elapsed response_path
  expected_count="$(wc -l <"$expected_tsv" | tr -d ' ')"
  elapsed=0
  response_path="$RESULTS_DIR/${label}_${service}_query.json"
  while [ "$elapsed" -lt "$max_wait" ]; do
    search_json_compose "$service" "$INDEX_NAME" "" "$response_path" >/dev/null
    if assert_response_exact_set "$response_path" "$expected_tsv" "$expected_count" "$label" \
      2>"$RESULTS_DIR/${label}_${service}_last_error.txt"; then
      record_pass "$label exact set on $service (${expected_count} docs)"
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  cat "$RESULTS_DIR/${label}_${service}_last_error.txt" >&2
  fail "$label did not reach exact set on $service within ${max_wait}s"
}

assert_exact_set_now() {
  local service="$1" label="$2" expected_tsv="$3"
  local expected_count response_path
  expected_count="$(wc -l <"$expected_tsv" | tr -d ' ')"
  response_path="$RESULTS_DIR/${label}_${service}_query.json"
  search_json_compose "$service" "$INDEX_NAME" "" "$response_path" >/dev/null
  assert_response_exact_set "$response_path" "$expected_tsv" "$expected_count" "$label" \
    2>"$RESULTS_DIR/${label}_${service}_last_error.txt" \
    || fail "$label exact set assertion failed on $service"
  record_pass "$label exact set on $service (${expected_count} docs)"
}

requested_ids_from_payload() {
  local payload_path="$1" output_path="$2"
  jq -er '.requests | map(select(.action == "addObject") | .body.objectID) | .[]' \
    "$payload_path" | sort >"$output_path"
}

record_no_ack_ids() {
  local requested_ids_path="$1" target_path="$2"
  cat "$requested_ids_path" >>"$target_path"
  sort -u "$REJECTED_IDS" "$AMBIGUOUS_IDS" >"$NO_ACK_IDS"
}

validate_successful_batch_response() {
  local payload_path="$1" response_path="$2" requested_path="$3" object_ids_path="$4"
  requested_ids_from_payload "$payload_path" "$requested_path"
  jq -er '.taskID | numbers' "$response_path" >/dev/null
  jq -er '.objectIDs | arrays | .[]' "$response_path" | sort >"$object_ids_path"
  cmp -s "$requested_path" "$object_ids_path"
}

submit_batch_to_oracle() {
  local service="$1" label="$2" tsv_path="$3" ack_path="$4"
  local payload_path response_path status_path requested_path object_ids_path status payload
  payload_path="$RESULTS_DIR/${label}_payload.json"
  response_path="$RESULTS_DIR/${label}_${service}_write_response.json"
  status_path="$RESULTS_DIR/${label}_${service}_write_status.txt"
  requested_path="$RESULTS_DIR/${label}_requested_ids.txt"
  object_ids_path="$RESULTS_DIR/${label}_response_object_ids.txt"
  payload_for_tsv "$tsv_path" >"$payload_path"
  payload="$(cat "$payload_path")"
  if ! post_json_compose "$service" "/1/indexes/$INDEX_NAME/batch" "$payload" "$response_path" "$status_path"; then
    requested_ids_from_payload "$payload_path" "$requested_path"
    record_no_ack_ids "$requested_path" "$AMBIGUOUS_IDS"
    record_pass "$label transport failed and IDs were preserved as ambiguous"
    return 0
  fi
  status="$(cat "$status_path")"
  if [[ "$status" =~ ^2[0-9][0-9]$ ]] \
    && validate_successful_batch_response "$payload_path" "$response_path" "$requested_path" "$object_ids_path"; then
    cat "$object_ids_path" >>"$ack_path"
    record_pass "$label 2xx response matched requested object IDs"
    return 0
  fi
  requested_ids_from_payload "$payload_path" "$requested_path"
  if [[ "$status" =~ ^[45][0-9][0-9]$ ]]; then
    record_no_ack_ids "$requested_path" "$REJECTED_IDS"
    record_pass "$label rejected IDs were preserved separately"
    return 0
  fi
  record_no_ack_ids "$requested_path" "$AMBIGUOUS_IDS"
  record_pass "$label ambiguous IDs were preserved separately"
}

capture_status_snapshot() {
  local label="$1"
  for service in node-a node-b node-c; do
    cluster_status_compose "$service" >"$RESULTS_DIR/${label}_${service}_status.json" || return 1
  done
}

capture_topology_snapshot() {
  local label="$1"
  compose ps >"$RESULTS_DIR/${label}_compose_ps.txt" 2>&1 || return 1
  if [ -n "$NETWORK_ID" ]; then
    docker network inspect "$NETWORK_ID" >"$RESULTS_DIR/${label}_network_inspect.json" || return 1
  fi
  for service in node-a node-b node-c; do
    compose_container_id "$service" >"$RESULTS_DIR/${label}_${service}_container_id.txt" || return 1
    docker inspect "$(cat "$RESULTS_DIR/${label}_${service}_container_id.txt")" \
      >"$RESULTS_DIR/${label}_${service}_inspect.json" || return 1
  done
}

capture_required_evidence() {
  local label="$1"
  local failed=0
  if ! declare -F compose >/dev/null || ! command -v docker >/dev/null 2>&1; then
    printf 'cluster evidence unavailable before HA helpers and Docker preflight completed\n' \
      >"$RESULTS_DIR/${label}_cluster_evidence_unavailable.txt" || return 1
    return 0
  fi
  capture_status_snapshot "$label" || failed=1
  capture_topology_snapshot "$label" || failed=1
  compose logs --no-color >"$RESULTS_DIR/${label}_compose_logs.txt" 2>&1 || failed=1
  compose ps >"$RESULTS_DIR/${label}_compose_ps.txt" 2>&1 || failed=1
  return "$failed"
}

write_summary_artifacts() {
  local summary_line source_sha acknowledged_union_count no_ack_count
  [ -n "$RESULTS_DIR" ] || return 1
  if [ ! -d "$RESULTS_DIR" ]; then
    command -v mkdir >/dev/null 2>&1 || return 1
    mkdir -p "$RESULTS_DIR" || return 1
  fi
  source_sha="$(first_line_if_readable "$RESULTS_DIR/source_sha.txt")"
  acknowledged_union_count="$(line_count_if_readable "$UNION_TSV")"
  no_ack_count="$(line_count_if_readable "$NO_ACK_IDS")"
  summary_line="RESULT ${OUTCOME} assertions_passed=${PASS_COUNT} assertions_failed=${FAIL_COUNT} acknowledged_union_count=${acknowledged_union_count} no_ack_count=${no_ack_count}"
  printf '%s\n' "$summary_line" >"$SUMMARY_LINE_PATH"
  printf '{"outcome":"%s","source_sha":"%s","summary":"%s","note":"cluster status proves connectivity health for a known broken peer path; it does not prove per-index currency","assertions_passed":%s,"assertions_failed":%s,"script_exit_code":%s,"interrupted_exit_code":%s}\n' \
    "$OUTCOME" "$source_sha" "$summary_line" "$PASS_COUNT" "$FAIL_COUNT" \
    "$SCRIPT_EXIT_CODE" "$INTERRUPTED_EXIT_CODE" >"$SUMMARY_JSON_PATH"
}

first_line_if_readable() {
  local path="$1" line=""
  if [ -r "$path" ]; then
    IFS= read -r line <"$path" || true
  fi
  printf '%s' "$line"
}

line_count_if_readable() {
  local path="$1" count=0 line
  if [ -r "$path" ]; then
    while IFS= read -r line || [ -n "$line" ]; do
      count=$((count + 1))
    done <"$path"
  fi
  printf '%s' "$count"
}

cleanup() {
  local status=$?
  if $CLEANUP_COMPLETE; then
    exit "$status"
  fi
  CLEANUP_COMPLETE=true
  SCRIPT_EXIT_CODE="$status"
  if [ "$status" -eq 0 ] && [ "$FAIL_COUNT" -eq 0 ] && [ "$INTERRUPTED_EXIT_CODE" -eq 0 ]; then
    OUTCOME="PASS"
  else
    OUTCOME="FAIL"
  fi
  write_summary_artifacts || SKIP_TEARDOWN=true
  if [ "$OUTCOME" != "PASS" ] || [ "$SCRIPT_EXIT_CODE" -ne 0 ] || [ "$INTERRUPTED_EXIT_CODE" -ne 0 ]; then
    capture_required_evidence "failure_final" || SKIP_TEARDOWN=true
  else
    capture_required_evidence "pass_final" >/dev/null 2>&1 || true
  fi
  if ! $SKIP_TEARDOWN && declare -F compose >/dev/null; then
    compose down -v >/dev/null 2>&1 || true
  elif [ -n "$RESULTS_DIR" ] && [ -d "$RESULTS_DIR" ]; then
    printf 'teardown skipped because required evidence capture failed\n' >"$RESULTS_DIR/teardown_skipped.txt"
  fi
  printf 'Evidence: %s\n' "$RESULTS_DIR"
  if [ -r "$SUMMARY_LINE_PATH" ]; then
    first_line_if_readable "$SUMMARY_LINE_PATH"
    printf '\n'
  else
    printf 'RESULT %s assertions_passed=%s assertions_failed=%s acknowledged_union_count=0 no_ack_count=0\n' \
      "$OUTCOME" "$PASS_COUNT" "$FAIL_COUNT"
  fi
  exit "$status"
}

handle_interrupt() {
  INTERRUPTED_EXIT_CODE="$1"
  OUTCOME="FAIL"
  exit "$1"
}

assert_service_on_network() {
  local service="$1" label="$2"
  if compose_service_on_project_network "$service" "$NETWORK_ID"; then
    record_pass "$label"
    return 0
  fi
  fail "$label"
}

assert_service_not_on_network() {
  local service="$1" label="$2"
  if compose_service_on_project_network "$service" "$NETWORK_ID"; then
    fail "$label"
  fi
  record_pass "$label"
}

poll_peer_status_not_healthy() {
  local service="$1" peer_id="$2" label="$3" max_wait="$4"
  local elapsed=0 sample_path status_path
  sample_path="$RESULTS_DIR/${label}_samples.jsonl"
  : >"$sample_path"
  while [ "$elapsed" -lt "$max_wait" ]; do
    status_path="$RESULTS_DIR/${label}_${service}_latest_status.json"
    cluster_status_compose "$service" >"$status_path"
    jq -c --arg peer_id "$peer_id" \
      '.peers[]? | select(.peer_id == $peer_id) | {peer_id,status,last_success_secs_ago}' \
      "$status_path" >>"$sample_path"
    if jq -e --arg peer_id "$peer_id" \
      '.peers[]? | select(.peer_id == $peer_id) | .status != "healthy"' \
      "$status_path" >/dev/null; then
      record_pass "$label left healthy"
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  fail "$label stayed healthy for ${max_wait}s"
}

assert_no_ack_absent() {
  local service="$1" object_id response_path nb_hits
  if [ ! -s "$NO_ACK_IDS" ]; then
    record_pass "no rejected or ambiguous IDs require absence checks on $service"
    return 0
  fi
  while IFS= read -r object_id; do
    response_path="$RESULTS_DIR/no_ack_${service}_${object_id}_query.json"
    search_json_compose "$service" "$INDEX_NAME" "$object_id" "$response_path" >/dev/null
    nb_hits="$(cat "$response_path" | parse_nb_hits)"
    [ "$nb_hits" = "0" ] || fail "no-ack ID $object_id unexpectedly appeared on $service"
  done <"$NO_ACK_IDS"
  record_pass "all no-ack IDs absent on $service"
}

start_cluster() {
  compose up -d --build node-a node-b node-c
  wait_healthy_compose "node-a" "node-a" || fail "node-a did not become healthy"
  wait_healthy_compose "node-b" "node-b" || fail "node-b did not become healthy"
  wait_healthy_compose "node-c" "node-c" || fail "node-c did not become healthy"
  wait_for_peer_mesh_ready 60 || fail "peer mesh did not converge before partition"
  NETWORK_ID="$(compose_project_network_id node-a fj-net)"
  printf '%s\n' "$NETWORK_ID" >"$RESULTS_DIR/project_network_id.txt"
  capture_topology_snapshot "pre_partition"
  capture_status_snapshot "pre_partition"
}

disconnect_node_c() {
  {
    printf 'docker network disconnect %s %s\n' "$NETWORK_ID" "$(compose_container_id node-c)"
    disconnect_service_from_project_network node-c "$NETWORK_ID"
  } >"$RESULTS_DIR/disconnect_node_c_transcript.txt" 2>&1
  assert_service_not_on_network node-c "node-c disconnected from run-owned network"
  assert_service_on_network node-a "node-a remained attached during partition"
  assert_service_on_network node-b "node-b remained attached during partition"
  wait_healthy_compose "node-a" "node-a after node-c isolation" || fail "node-a health failed during node-c isolation"
  wait_healthy_compose "node-b" "node-b after node-c isolation" || fail "node-b health failed during node-c isolation"
  record_pass "node-a and node-b remained healthy during one-node isolation"
  capture_topology_snapshot "during_partition"
}

reconnect_node_c() {
  if [ "${FLAPJACK_PARTITION_SKIP_HEAL:-0}" = "1" ]; then
    printf 'FLAPJACK_PARTITION_SKIP_HEAL=1 skipped reconnect\n' \
      >"$RESULTS_DIR/reconnect_node_c_transcript.txt"
  else
    {
      printf 'docker network connect %s %s\n' "$NETWORK_ID" "$(compose_container_id node-c)"
      reconnect_service_to_project_network node-c "$NETWORK_ID"
    } >"$RESULTS_DIR/reconnect_node_c_transcript.txt" 2>&1
  fi
  assert_service_on_network node-c "node-c membership restored after heal"
}

main() {
  configure_run_identity

  trap cleanup EXIT
  trap 'handle_interrupt 130' INT
  trap 'handle_interrupt 143' TERM

  preflight
  source_ha_lib
  initialize_results
  write_expected_sets
  start_cluster

  submit_batch_to_oracle node-a starter "$STARTER_TSV" "$ACK_NODE_A_IDS"
  for service in node-a node-b node-c; do
    wait_for_exact_set_compose "$service" "starter_baseline" "$STARTER_TSV" 60
  done
  capture_status_snapshot "known_good"

  disconnect_node_c
  submit_batch_to_oracle node-a node_a_partition_write <(tail -n +3 "$NODE_A_TSV") "$ACK_NODE_A_IDS"
  wait_for_exact_set_compose node-a "node_a_partition_acknowledged" "$NODE_A_TSV" 60
  assert_exact_set_now node-c "isolated_node_c_behind_node_a_oracle" "$STARTER_TSV"
  poll_peer_status_not_healthy node-a node-c "node_a_peer_node_c_connectivity" 90

  submit_batch_to_oracle node-c node_c_isolated_write "$NODE_C_TSV" "$ACK_NODE_C_IDS"
  poll_peer_status_not_healthy node-c node-a "node_c_peer_node_a_connectivity" 90
  sort -u "$REJECTED_IDS" "$AMBIGUOUS_IDS" >"$NO_ACK_IDS"
  materialize_acknowledged_union
  capture_status_snapshot "during_partition_after_writes"

  reconnect_node_c
  wait_for_peer_mesh_ready 60 || fail "peer mesh did not converge after heal"
  for service in node-a node-b node-c; do
    wait_for_exact_set_compose "$service" "post_heal_acknowledged_union" "$UNION_TSV" 120
    assert_no_ack_absent "$service"
  done
  capture_status_snapshot "post_heal"
  capture_topology_snapshot "post_heal"

  record_pass "partition scenario completed with exact acknowledged-union convergence"
}

main "$@"
