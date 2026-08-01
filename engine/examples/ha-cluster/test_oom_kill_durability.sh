#!/usr/bin/env bash
set -euo pipefail
if [[ "${BASH_SOURCE[0]}" == "$0" && "${OOM_DURABILITY_SIGNALS_NORMALIZED:-0}" != "1" ]]; then
  exec python3 - "$BASH" "${BASH_SOURCE[0]}" "$@" <<'PY'
import os
import signal
import sys
bash_path, harness_path, *arguments = sys.argv[1:]
environment = os.environ.copy()
environment["OOM_DURABILITY_SIGNALS_NORMALIZED"] = "1"
signal.signal(signal.SIGINT, signal.SIG_DFL)
os.execve(bash_path, [bash_path, harness_path, *arguments], environment)
PY
fi
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPO_ROOT="$(cd "$ENGINE_DIR/.." && pwd)"
RESULTS_BASE_DIR="$ENGINE_DIR/loadtest/results/chaos-residual"
DOCKERFILE="$ENGINE_DIR/Dockerfile"
RUN_ID="" RUN_PREFIX=""
RESULTS_DIR="" SERVER_CONTAINER="" RECOVERY_CONTAINER=""
DATA_VOLUME="" NEGATIVE_DATA_VOLUME="" NETWORK_NAME=""
IMAGE_TAG="" BASE_URL="" RECOVERY_BASE_URL=""
INDEX_NAME="" ENCODED_INDEX=""
SCRIPT_EXIT_CODE="0" INTERRUPTED_EXIT_CODE="0"
FINALIZED="false" CLEANUP_COMPLETE="false"
OUTCOME="INCONCLUSIVE" OOM_KILLED="unknown"
ACKNOWLEDGED_COUNT="0" RECOVERED_ACKNOWLEDGED_COUNT="0"
EXPLICIT_REJECTED_COUNT="0" RECOVERED_REJECTED_COUNT="0" NO_ACK_COUNT="0"
HTTP_STATUS="000" HTTP_TRANSPORT_EXIT="0"
ACK_ORACLE_PATH="" EXPLICIT_REJECTION_PATH="" NO_ACK_PATH=""
RECOVERED_ACK_PATH="" RECOVERED_NO_ACK_PATH="" RECOVERED_REJECTED_PATH=""
SUMMARY_PATH="" SUMMARY_LINE_PATH="" SCRIPT_EXIT_PATH=""
BASE_URL_PATH="" OOM_VALUE_PATH="" INSPECT_PATH="" RECOVERY_INSPECT_PATH=""
SERVER_LOG_PATH="" RECOVERY_LOG_PATH="" QUEUE_PREFILL_PATH=""
TOTAL_DOCS="${FLAPJACK_OOM_TOTAL_DOCS:-12}"
BATCH_SIZE="${FLAPJACK_OOM_BATCH_SIZE:-4}"
MEMORY_LIMIT="${FLAPJACK_OOM_MEMORY_LIMIT:-256m}"
OOM_FILL_MIB="${FLAPJACK_OOM_FILL_MIB:-768}"
HOST_PORT="${FLAPJACK_OOM_HOST_PORT:-}"
NEGATIVE_EMPTY_RESTART="${FLAPJACK_OOM_NEGATIVE_EMPTY_RESTART:-0}"
fail() {
  echo "FAIL: $1" >&2
  OUTCOME="FAIL"
  return 1
}
require_command() {
  local command_name="$1"
  command -v "$command_name" >/dev/null 2>&1 || fail "required command not found: $command_name"
}
require_file() {
  local path="$1"
  [[ -f "$path" ]] || fail "missing file: $path"
}
choose_loopback_port() {
  python3 - <<'PY'
import socket
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.bind(("127.0.0.1", 0))
print(sock.getsockname()[1])
sock.close()
PY
}
url_encode_component() {
  python3 - "$1" <<'PY'
import sys
from urllib.parse import quote
print(quote(sys.argv[1], safe=""))
PY
}
initialize_run_identity() {
  local timestamp random_suffix
  timestamp="$(date -u +%Y%m%d_%H%M%S)"
  random_suffix="$(printf '%s' "$$-$RANDOM" | tr -cd 'A-Za-z0-9_')"
  RUN_ID="$(printf '%s_%s' "$timestamp" "$random_suffix" | tr '[:upper:]' '[:lower:]')"
  RUN_PREFIX="chaos_oom_durability_${RUN_ID}"
  SERVER_CONTAINER="${RUN_PREFIX}_server"
  RECOVERY_CONTAINER="${RUN_PREFIX}_recovery"
  DATA_VOLUME="${RUN_PREFIX}_data"
  NEGATIVE_DATA_VOLUME="${RUN_PREFIX}_empty_recovery_data"
  NETWORK_NAME="${RUN_PREFIX}_net"
  IMAGE_TAG="${RUN_PREFIX}_image"
  INDEX_NAME="oom_durability_${RUN_ID}"
  ENCODED_INDEX="$(url_encode_component "$INDEX_NAME")"
  RESULTS_DIR="$RESULTS_BASE_DIR/${RUN_PREFIX}"
}
initialize_results() {
  mkdir -p "$RESULTS_DIR"
  ACK_ORACLE_PATH="$RESULTS_DIR/acknowledged_object_ids.txt"
  EXPLICIT_REJECTION_PATH="$RESULTS_DIR/explicit_rejections.jsonl"
  NO_ACK_PATH="$RESULTS_DIR/no_ack_diagnostics.jsonl"
  RECOVERED_ACK_PATH="$RESULTS_DIR/recovered_acknowledged_ids.txt"
  RECOVERED_NO_ACK_PATH="$RESULTS_DIR/recovered_no_ack_ids.txt"
  RECOVERED_REJECTED_PATH="$RESULTS_DIR/recovered_explicit_rejected_ids.txt"
  SUMMARY_PATH="$RESULTS_DIR/summary.json"
  SUMMARY_LINE_PATH="$RESULTS_DIR/summary_line.txt"
  SCRIPT_EXIT_PATH="$RESULTS_DIR/script_exit.txt"
  BASE_URL_PATH="$RESULTS_DIR/http_base_urls.txt"
  OOM_VALUE_PATH="$RESULTS_DIR/oom_killed_value.txt"
  INSPECT_PATH="$RESULTS_DIR/server_container_inspect.json"
  RECOVERY_INSPECT_PATH="$RESULTS_DIR/recovery_container_inspect.json"
  SERVER_LOG_PATH="$RESULTS_DIR/server_container.log"
  RECOVERY_LOG_PATH="$RESULTS_DIR/recovery_container.log"
  QUEUE_PREFILL_PATH="$RESULTS_DIR/queue_full_precondition.txt"
  : >"$ACK_ORACLE_PATH"
  : >"$EXPLICIT_REJECTION_PATH"
  : >"$NO_ACK_PATH"
  : >"$RECOVERED_ACK_PATH"
  : >"$RECOVERED_NO_ACK_PATH"
  : >"$RECOVERED_REJECTED_PATH"
}
build_image() {
  if [[ -n "${FLAPJACK_OOM_IMAGE:-}" ]]; then
    IMAGE_TAG="$FLAPJACK_OOM_IMAGE"
    printf 'image_source=provided\nimage_tag=%s\n' "$IMAGE_TAG" >"$RESULTS_DIR/image_metadata.txt"
    return
  fi
  docker build -t "$IMAGE_TAG" -f "$DOCKERFILE" "$REPO_ROOT" >"$RESULTS_DIR/docker_build.log" 2>&1
  printf 'image_source=built\nimage_tag=%s\n' "$IMAGE_TAG" >"$RESULTS_DIR/image_metadata.txt"
}
create_run_docker_resources() {
  local subnet_args=()
  [[ -z "${FLAPJACK_OOM_NETWORK_SUBNET:-}" ]] || subnet_args=(--subnet "$FLAPJACK_OOM_NETWORK_SUBNET")
  docker network create "${subnet_args[@]}" "$NETWORK_NAME" >"$RESULTS_DIR/docker_network_create.txt"
  docker volume create "$DATA_VOLUME" >"$RESULTS_DIR/docker_volume_create.txt"
  if [[ "$NEGATIVE_EMPTY_RESTART" == "1" ]]; then
    docker volume create "$NEGATIVE_DATA_VOLUME" >"$RESULTS_DIR/docker_negative_volume_create.txt"
  fi
}
server_command() {
  cat <<'SH'
set -eu
rm -f /tmp/trigger_oom
flapjack --data-dir /data &
server_pid="$!"
while kill -0 "$server_pid" 2>/dev/null; do
  if [ -f /tmp/trigger_oom ]; then
    # This stage must prove a kernel cgroup OOM kill. A user-sent kill -9 would
    # only repeat the Rust crash tests and would not set Docker State.OOMKilled.
    exec sh -c "dd if=/dev/zero of=/dev/shm/fill bs=1M count=${OOM_FILL_MIB} 2>/tmp/oom_dd.log; head -c ${OOM_FILL_MIB}m /dev/zero | tail -c ${OOM_FILL_MIB}m >/dev/null"
  fi
  sleep 0.1
done
wait "$server_pid"
SH
}
start_server_container() {
  local container="$1"
  local volume="$2"
  local port="$3"
  local mode="$4"
  local command_text
  command_text="$(server_command)"
  docker run -d \
    --name "$container" \
    --network "$NETWORK_NAME" \
    --memory "$MEMORY_LIMIT" \
    --memory-swap "$MEMORY_LIMIT" \
    --shm-size "$OOM_FILL_MIB"'m' \
    -p "127.0.0.1:${port}:7700" \
    -v "${volume}:/data" \
    -e FLAPJACK_DATA_DIR=/data \
    -e FLAPJACK_BIND_ADDR=0.0.0.0:7700 \
    -e FLAPJACK_NO_AUTH=1 \
    -e FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND=1 \
    -e "OOM_FILL_MIB=$OOM_FILL_MIB" \
    -e FLAPJACK_WRITE_DURABLE_TIMEOUT_MS=20000 \
    -e FLAPJACK_WRITE_QUEUE_CHANNEL_CAPACITY=2 \
    -e "FLAPJACK_WRITE_QUEUE_START_DELAY_MS=$([[ "$mode" == "queue_full" ]] && printf '10000' || printf '0')" \
    --entrypoint /bin/sh \
    "$IMAGE_TAG" \
    -c "$command_text" >"$RESULTS_DIR/${container}_id.txt"
}
wait_for_health() {
  local base_url="$1"
  local container="$2"
  local attempt status
  for ((attempt = 1; attempt <= 120; attempt += 1)); do
    status="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 2 "$base_url/health" 2>/dev/null || true)"
    if [[ "$status" == "200" ]]; then
      return 0
    fi
    if [[ "$(docker inspect -f '{{.State.Running}}' "$container" 2>/dev/null || true)" != "true" ]]; then
      docker logs "$container" >"$RESULTS_DIR/${container}_early_exit.log" 2>&1 || true
      fail "$container exited before health returned 200"
      return 1
    fi
    sleep 1
  done
  fail "$container health check did not return 200"
}
http_request() {
  local method="$1"
  local path="$2"
  local body_path="$3"
  local expected_status_csv="$4"
  local response_path="$5"
  local headers_path="$6"
  local status status_match
  if status="$(
    curl -sS -D "$headers_path" -o "$response_path" -w '%{http_code}' --max-time 10 \
      -X "$method" -H 'content-type: application/json' \
      ${body_path:+--data-binary "@$body_path"} \
      "$BASE_URL$path"
  )"; then
    HTTP_TRANSPORT_EXIT="0"
  else
    HTTP_TRANSPORT_EXIT="$?"
  fi
  HTTP_STATUS="${status:-000}"
  if ((HTTP_TRANSPORT_EXIT != 0)); then
    fail "$method $path failed in transport with curl exit $HTTP_TRANSPORT_EXIT and HTTP $HTTP_STATUS"
    return "$HTTP_TRANSPORT_EXIT"
  fi
  status_match="false"
  IFS=',' read -ra statuses <<<"$expected_status_csv"
  for candidate in "${statuses[@]}"; do
    if [[ "$HTTP_STATUS" == "$candidate" ]]; then
      status_match="true"
    fi
  done
  if [[ "$status_match" != "true" ]]; then
    fail "$method $path returned HTTP $HTTP_STATUS, expected $expected_status_csv; body=$(cat "$response_path" 2>/dev/null || true)"
    return 1
  fi
}
write_batch_payload() {
  local start="$1"
  local count="$2"
  local output_path="$3"
  python3 - "$RUN_ID" "$start" "$count" >"$output_path" <<'PY'
import json
import sys
run_id, start, count = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
requests = []
for doc_index in range(start, start + count):
    tier = "alpha" if doc_index % 2 == 0 else "beta"
    requests.append({
        "action": "addObject",
        "body": {
            "objectID": f"oom-{doc_index:03d}",
            "title": f"OOM durability document {doc_index:03d}",
            "token": "oom-durability-proof",
            "tier": tier,
            "family": f"family-{doc_index % 3}",
            "marker": f"marker{doc_index:03d}",
            "run_id": run_id,
        },
    })
print(json.dumps({"requests": requests}, separators=(",", ":")))
PY
}
wait_for_task_published() {
  local task_id="$1"
  local task_path="$RESULTS_DIR/task_${task_id}.json"
  local attempt
  for ((attempt = 1; attempt <= 100; attempt += 1)); do
    if curl -sS --fail --max-time 2 "$BASE_URL/1/indexes/$ENCODED_INDEX/task/$task_id" >"$task_path"; then
      if jq -e '.status == "published" and .pendingTask == false' "$task_path" >/dev/null; then
        return 0
      fi
    fi
    sleep 0.1
  done
  fail "task $task_id did not publish"
}
record_acknowledged_response() {
  local request_path="$1"
  local response_path="$2"
  local task_id object_ids_path requested_object_ids_path
  task_id="$(jq -er '.taskID | numbers' "$response_path")"
  object_ids_path="${response_path%.json}_object_ids.txt"
  requested_object_ids_path="${response_path%.json}_requested_object_ids.txt"
  jq -er '.requests | map(select(.action == "addObject") | .body.objectID) | if length > 0 and all(.[]; type == "string" and length > 0) then .[] else error("invalid requested objectIDs") end' \
    "$request_path" >"$requested_object_ids_path"
  jq -er '.objectIDs | arrays | if length > 0 and all(.[]; type == "string" and length > 0) then .[] else error("invalid objectIDs") end' \
    "$response_path" >"$object_ids_path"
  cmp -s <(sort "$requested_object_ids_path") <(sort "$object_ids_path") || { fail "2xx response objectIDs do not exactly match requested objectIDs"; return 1; }
  wait_for_task_published "$task_id"
  cat "$object_ids_path" >>"$ACK_ORACLE_PATH"
  ACKNOWLEDGED_COUNT="$(unique_count "$ACK_ORACLE_PATH")"
}
drive_acknowledged_writes() {
  local batch_start document_count payload response headers
  for ((batch_start = 0; batch_start < TOTAL_DOCS; batch_start += BATCH_SIZE)); do
    document_count="$(batch_document_count "$batch_start")"
    payload="$RESULTS_DIR/write_batch_${batch_start}.json"
    response="$RESULTS_DIR/write_response_${batch_start}.json"
    headers="$RESULTS_DIR/write_headers_${batch_start}.txt"
    write_batch_payload "$batch_start" "$document_count" "$payload"
    submit_acknowledged_batch "write_batch_${batch_start}" "$payload" "$response" "$headers"
  done
}
submit_acknowledged_batch() {
  local label="$1"
  local payload="$2"
  local response="$3"
  local headers="$4"
  local request_exit
  if http_request POST "/1/indexes/$ENCODED_INDEX/batch" "$payload" "200" "$response" "$headers"; then
    classify_acknowledged_response "$label" "$HTTP_STATUS" "$payload" "$response"
    return $?
  else
    request_exit="$?"
  fi
  record_no_ack "$label" "$HTTP_STATUS" "$payload" "$response"
  return "$request_exit"
}
classify_acknowledged_response() {
  local label="$1" status="$2" payload="$3" response="$4" response_exit
  if record_acknowledged_response "$payload" "$response"; then
    return
  else
    response_exit="$?"
  fi
  record_no_ack "$label" "$status" "$payload" "$response"
  return "$response_exit"
}
batch_document_count() {
  local remaining="$((TOTAL_DOCS - $1))"
  if ((remaining < BATCH_SIZE)); then
    printf '%s\n' "$remaining"
  else
    printf '%s\n' "$BATCH_SIZE"
  fi
}
record_no_ack() {
  local label="$1"
  local status="$2"
  local request_path="$3"
  local response_path="$4"
  python3 - "$label" "$status" "$request_path" "$response_path" >>"$NO_ACK_PATH" <<'PY'
import json
import sys
label, status, request_path, response_path = sys.argv[1:5]
request = json.load(open(request_path, encoding="utf-8"))
object_ids = [item["body"]["objectID"] for item in request.get("requests", [])]
try:
    response = open(response_path, encoding="utf-8", errors="replace").read()
except FileNotFoundError:
    response = ""
print(json.dumps({"label": label, "http_status": int(status or 0), "objectIDs": object_ids, "response": response}, separators=(",", ":")))
PY
  NO_ACK_COUNT="$(no_ack_object_id_count)"
}
no_ack_object_id_count() {
  no_ack_object_ids | wc -l | tr -d ' '
}
no_ack_object_ids() {
  jq -r '.objectIDs[]' "$NO_ACK_PATH"
}
stop_container() {
  local container="$1"
  if docker inspect "$container" >/dev/null 2>&1; then
    docker stop -t 10 "$container" >/dev/null 2>&1 || true
    docker logs "$container" >"$RESULTS_DIR/${container}.log" 2>&1 || true
    docker inspect "$container" >"$RESULTS_DIR/${container}_stopped_inspect.json" 2>/dev/null || true
    docker rm "$container" >/dev/null 2>&1 || true
  fi
}
prepare_queue_full_server() {
  stop_container "$SERVER_CONTAINER"
  HOST_PORT="${HOST_PORT:-$(choose_loopback_port)}"
  BASE_URL="http://127.0.0.1:$HOST_PORT"
  start_server_container "$SERVER_CONTAINER" "$DATA_VOLUME" "$HOST_PORT" "queue_full"
  wait_for_health "$BASE_URL" "$SERVER_CONTAINER"
}
send_background_fill_request() {
  local id="$1"
  local payload="$RESULTS_DIR/queue_fill_${id}.json"
  local response="$RESULTS_DIR/queue_fill_${id}_response.json"
  local status_path="$RESULTS_DIR/queue_fill_${id}_status.txt"
  python3 - "$id" >"$payload" <<'PY'
import json
import sys
object_id = sys.argv[1]
print(json.dumps({"requests": [{"action": "addObject", "body": {"objectID": object_id, "token": "queue-fill"}}]}, separators=(",", ":")))
PY
  (
    curl -sS -o "$response" -w '%{http_code}' --max-time 30 \
      -H 'content-type: application/json' --data-binary "@$payload" \
      "$BASE_URL/1/indexes/$ENCODED_INDEX/batch" >"$status_path" || printf '000' >"$status_path"
  ) &
}
classify_queue_fill_result() {
  local id="$1"
  local payload="$RESULTS_DIR/queue_fill_${id}.json"
  local response="$RESULTS_DIR/queue_fill_${id}_response.json"
  local status_path="$RESULTS_DIR/queue_fill_${id}_status.txt"
  local status
  status="$(cat "$status_path" 2>/dev/null || printf '000')"
  if [[ "$status" =~ ^2[0-9][0-9]$ ]]; then
    classify_acknowledged_response "queue_fill_${id}" "$status" "$payload" "$response"
    return $?
  fi
  record_no_ack "queue_fill_${id}" "$status" "$payload" "$response"
}
container_admission_record_object_ids_present() {
  docker exec "$SERVER_CONTAINER" sh -c '
data_root="$1"
index_name="$2"
shift 2
admission_dir="$data_root/$index_name/write_admission"
[ -d "$admission_dir" ] || exit 1
for object_id in "$@"; do
  grep -R -F -q -- "$object_id" "$admission_dir" || exit 1
done
' sh /data "$INDEX_NAME" "$@" >/dev/null 2>&1
}
wait_for_queue_fill_precondition() {
  local attempt
  for ((attempt = 1; attempt <= 300; attempt += 1)); do
    if container_admission_record_object_ids_present "$@"; then
      {
        printf 'status=ready\n'
        printf 'attempt=%s\n' "$attempt"
        printf 'objectIDs=%s\n' "$*"
      } >"$QUEUE_PREFILL_PATH"
      return 0
    fi
    sleep 0.1
  done
  {
    printf 'status=timeout\n'
    printf 'objectIDs=%s\n' "$*"
  } >"$QUEUE_PREFILL_PATH"
  fail "queue-fill admission precondition did not become ready for object IDs: $*"
}
capture_explicit_rejection() {
  local object_id="$1"
  local status="$2"
  local request_path="$3"
  local response_path="$4"
  local headers_path="$5"
  python3 - "$object_id" "$status" "$request_path" "$response_path" "$headers_path" >>"$EXPLICIT_REJECTION_PATH" <<'PY'
import json
import sys
object_id, status, request_path, response_path, headers_path = sys.argv[1:6]
raw_body = open(response_path, encoding="utf-8", errors="replace").read()
headers = open(headers_path, encoding="utf-8", errors="replace").read()
try:
    parsed = json.loads(raw_body)
except json.JSONDecodeError:
    parsed = {}
print(json.dumps({
    "objectID": object_id,
    "http_status": int(status),
    "request": json.load(open(request_path, encoding="utf-8")),
    "headers": headers,
    "raw_body": raw_body,
    "json": parsed,
}, separators=(",", ":")))
PY
  EXPLICIT_REJECTED_COUNT="$(wc -l <"$EXPLICIT_REJECTION_PATH" | tr -d ' ')"
}
drive_explicit_rejection() {
  local sentinel_id="oom-explicit-rejected-sentinel"
  local payload="$RESULTS_DIR/explicit_rejection_request.json"
  local response="$RESULTS_DIR/explicit_rejection_response.json"
  local headers="$RESULTS_DIR/explicit_rejection_headers.txt"
  local status fill_pid_one fill_pid_two
  send_background_fill_request "oom-queue-fill-0"
  fill_pid_one="$!"
  send_background_fill_request "oom-queue-fill-1"
  fill_pid_two="$!"
  wait_for_queue_fill_precondition "oom-queue-fill-0" "oom-queue-fill-1"
  python3 - "$sentinel_id" >"$payload" <<'PY'
import json
import sys
object_id = sys.argv[1]
print(json.dumps({"requests": [{"action": "addObject", "body": {"objectID": object_id, "token": object_id}}]}, separators=(",", ":")))
PY
  status="$(
    curl -sS -D "$headers" -o "$response" -w '%{http_code}' --max-time 5 \
      -H 'content-type: application/json' --data-binary "@$payload" \
      "$BASE_URL/1/indexes/$ENCODED_INDEX/batch" || true
  )"
  if [[ "$status" == "429" ]] && jq -e '.status == 429 and .message == "Write queue full" and has("taskID") | not' "$response" >/dev/null; then
    capture_explicit_rejection "$sentinel_id" "$status" "$payload" "$response" "$headers"
  else
    record_no_ack "explicit_rejection_attempt" "${status:-0}" "$payload" "$response"
    fail "sentinel did not receive explicit 429 QueueFull rejection"
    return 1
  fi
  wait "$fill_pid_one" 2>/dev/null || true
  wait "$fill_pid_two" 2>/dev/null || true
  classify_queue_fill_result "oom-queue-fill-0"
  classify_queue_fill_result "oom-queue-fill-1"
}
trigger_kernel_oom() {
  # Keep the server and memory pressure in one memory-limited cgroup. The fill
  # size is intentionally several times the memory limit so Docker reports
  # State.OOMKilled=true quickly instead of relying on timing-sensitive load.
  docker exec "$SERVER_CONTAINER" sh -c 'touch /tmp/trigger_oom'
  for _ in $(seq 1 90); do
    if [[ "$(docker inspect -f '{{.State.Running}}' "$SERVER_CONTAINER" 2>/dev/null || true)" != "true" ]]; then
      break
    fi
    sleep 1
  done
  docker logs "$SERVER_CONTAINER" >"$SERVER_LOG_PATH" 2>&1 || true
  docker inspect "$SERVER_CONTAINER" >"$INSPECT_PATH"
  OOM_KILLED="$(docker inspect -f '{{.State.OOMKilled}}' "$SERVER_CONTAINER")"
  printf '%s\n' "$OOM_KILLED" >"$OOM_VALUE_PATH"
  [[ "$OOM_KILLED" == "true" ]] || fail "server container was not killed by the kernel OOM killer"
}
start_recovery_server() {
  local recovery_port recovery_volume
  recovery_port="$(choose_loopback_port)"
  RECOVERY_BASE_URL="http://127.0.0.1:$recovery_port"
  recovery_volume="$DATA_VOLUME"
  if [[ "$NEGATIVE_EMPTY_RESTART" == "1" ]]; then
    # The empty-volume negative control proves the acknowledged-ID assertion is
    # fail-capable: the restart phase must not pass if durable data is absent.
    recovery_volume="$NEGATIVE_DATA_VOLUME"
  fi
  # Reuse the same data volume for the real contract so the probe checks crash
  # durability, not whether a fresh instance can accept new writes.
  start_server_container "$RECOVERY_CONTAINER" "$recovery_volume" "$recovery_port" "normal"
  wait_for_health "$RECOVERY_BASE_URL" "$RECOVERY_CONTAINER"
  {
    echo "pre_oom_base_url=$BASE_URL"
    echo "restart_base_url=$RECOVERY_BASE_URL"
    echo "negative_empty_restart=$NEGATIVE_EMPTY_RESTART"
  } >"$BASE_URL_PATH"
}
query_recovered_ids() {
  local page="0"
  local hits_per_page="1000"
  local total_hits=""
  local response status recovered_all_path
  recovered_all_path="$RESULTS_DIR/recovered_all_ids.txt"
  : >"$recovered_all_path"
  : >"$RESULTS_DIR/recovery_all_pages.jsonl"
  : >"$RESULTS_DIR/recovery_all_query.json"
  while [[ -z "$total_hits" || $((page * hits_per_page)) -lt "$total_hits" ]]; do
    response="$RESULTS_DIR/recovery_all_query_page_${page}.json"
    status="$(curl -sS --max-time 10 -o "$response" -w '%{http_code}' \
      -H 'content-type: application/json' \
      --data-binary "$(jq -cn --argjson page "$page" --argjson hitsPerPage "$hits_per_page" '{query:"",page:$page,hitsPerPage:$hitsPerPage}')" \
      "$RECOVERY_BASE_URL/1/indexes/$ENCODED_INDEX/query" || true)"
    if [[ "$status" == "404" && "$NEGATIVE_EMPTY_RESTART" == "1" ]]; then
      printf '{"nbHits":0,"hits":[]}\n' >"$response"
    elif [[ "$status" != "200" ]]; then
      OUTCOME="FAIL_RECOVERY_QUERY"
      fail "recovery all query page $page returned HTTP ${status:-000}"
      return 1
    fi
    if [[ -z "$total_hits" ]]; then
      total_hits="$(jq -er '.nbHits | numbers' "$response")"
      jq -cn --argjson nbHits "$total_hits" '{nbHits:$nbHits,hits:[]}' >"$RESULTS_DIR/recovery_all_query.json"
    fi
    jq -er '.hits[]?.objectID' "$response" >>"$recovered_all_path" || true
    jq -c . "$response" >>"$RESULTS_DIR/recovery_all_pages.jsonl"
    page="$((page + 1))"
  done
  grep -Fxf <(sort -u "$ACK_ORACLE_PATH") <(sort -u "$recovered_all_path") >"$RECOVERED_ACK_PATH" || true
  grep -Fxf <(no_ack_object_ids | sort -u) <(sort -u "$recovered_all_path") >"$RECOVERED_NO_ACK_PATH" || true
  RECOVERED_ACKNOWLEDGED_COUNT="$(unique_count "$RECOVERED_ACK_PATH")"
}
query_rejected_ids() {
  local object_id response status
  : >"$RECOVERED_REJECTED_PATH"
  while IFS= read -r object_id; do
    [[ -n "$object_id" ]] || continue
    response="$RESULTS_DIR/rejected_search_${object_id}.json"
    status="$(curl -sS --max-time 10 -o "$response" -w '%{http_code}' \
      -H 'content-type: application/json' \
      --data-binary "$(jq -cn --arg query "$object_id" '{query:$query,hitsPerPage:10}')" \
      "$RECOVERY_BASE_URL/1/indexes/$ENCODED_INDEX/query" || true)"
    if [[ "$status" == "404" && "$NEGATIVE_EMPTY_RESTART" == "1" ]]; then
      printf '{"nbHits":0,"hits":[]}\n' >"$response"
    elif [[ "$status" != "200" ]]; then
      OUTCOME="FAIL_REJECTED_RECOVERY_QUERY"
      fail "rejected-id recovery query returned HTTP ${status:-000}"
      return 1
    fi
    if jq -e '.nbHits != 0' "$response" >/dev/null; then
      printf '%s\n' "$object_id" >>"$RECOVERED_REJECTED_PATH"
    fi
  done < <(jq -r '.objectID' "$EXPLICIT_REJECTION_PATH")
  RECOVERED_REJECTED_COUNT="$(unique_count "$RECOVERED_REJECTED_PATH")"
}
assert_recovery_contract() {
  local oracle_sorted recovered_sorted recovered_all_sorted permitted_recovered_sorted expected_alpha_hits alpha_hits sample_id specific_hits
  oracle_sorted="$RESULTS_DIR/acknowledged_sorted.txt"
  recovered_sorted="$RESULTS_DIR/recovered_acknowledged_sorted.txt"
  recovered_all_sorted="$RESULTS_DIR/recovered_all_sorted.txt"
  permitted_recovered_sorted="$RESULTS_DIR/permitted_recovered_sorted.txt"
  sort -u "$ACK_ORACLE_PATH" >"$oracle_sorted"
  sort -u "$RECOVERED_ACK_PATH" >"$recovered_sorted"
  compare_sorted_unique_files "$oracle_sorted" "$recovered_sorted" || {
    OUTCOME="FAIL_ACKNOWLEDGED_RECOVERY_MISMATCH"
    return 1
  }
  sort -u "$RESULTS_DIR/recovered_all_ids.txt" >"$recovered_all_sorted"
  sort -u "$ACK_ORACLE_PATH" "$RECOVERED_NO_ACK_PATH" >"$permitted_recovered_sorted"
  compare_sorted_unique_files "$permitted_recovered_sorted" "$recovered_all_sorted" || {
    OUTCOME="FAIL_UNCLASSIFIED_RECOVERY_MISMATCH"
    fail "post-restart object set contains IDs outside the acknowledged and recovered no-ack contracts"
    return 1
  }
  expected_alpha_hits="$(acknowledged_alpha_count)"
  alpha_hits="$(query_count "alpha" "$RESULTS_DIR/recovery_alpha_query.json")"
  [[ "$alpha_hits" == "$expected_alpha_hits" ]] || {
    OUTCOME="FAIL_TARGETED_ALPHA_MISMATCH"
    fail "post-restart alpha query nbHits=$alpha_hits, expected $expected_alpha_hits"
    return 1
  }
  sample_id="$(acknowledged_sample_id)"
  specific_hits="$(query_count "$sample_id" "$RESULTS_DIR/recovery_sample_id_query.json")"
  [[ "$specific_hits" == "1" ]] || {
    OUTCOME="FAIL_TARGETED_ID_MISSING"
    fail "post-restart $sample_id query did not recover exact acknowledged ID"
    return 1
  }
  [[ "$EXPLICIT_REJECTED_COUNT" != "0" ]] || {
    OUTCOME="FAIL_EMPTY_EXPLICIT_REJECTION_ORACLE"
    fail "explicit rejection oracle is empty"
    return 1
  }
  [[ "$RECOVERED_REJECTED_COUNT" == "0" ]] || {
    OUTCOME="FAIL_REJECTED_SENTINEL_RECOVERED"
    fail "explicitly rejected IDs were searchable after restart"
    return 1
  }
}
acknowledged_alpha_count() {
  awk '/^oom-[0-9]+$/ && !seen[$0]++ { suffix=substr($0, 5) + 0; if (suffix % 2 == 0) count++ } END { print count + 0 }' "$ACK_ORACLE_PATH"
}
acknowledged_sample_id() {
  awk '/^oom-[0-9]+$/ { sample=$0 } END { if (sample) print sample; else exit 1 }' "$ACK_ORACLE_PATH"
}
query_count() {
  local query="$1"
  local response="$2"
  curl -sS --fail --max-time 10 \
    -H 'content-type: application/json' \
    --data-binary "$(jq -cn --arg query "$query" '{query:$query,hitsPerPage:1000}')" \
    "$RECOVERY_BASE_URL/1/indexes/$ENCODED_INDEX/query" >"$response"
  jq -er '.nbHits | numbers' "$response"
}
compare_sorted_unique_files() {
  local expected_path="$1"
  local actual_path="$2"
  cmp -s "$expected_path" "$actual_path"
}
unique_count() {
  local path="$1"
  sort -u "$path" 2>/dev/null | sed '/^$/d' | wc -l | tr -d ' '
}
capture_missing_artifacts() {
  [[ -f "$SERVER_LOG_PATH" ]] || docker logs "$SERVER_CONTAINER" >"$SERVER_LOG_PATH" 2>&1 || true
  [[ -f "$INSPECT_PATH" ]] || docker inspect "$SERVER_CONTAINER" >"$INSPECT_PATH" 2>/dev/null || true
  [[ -f "$RECOVERY_LOG_PATH" ]] || docker logs "$RECOVERY_CONTAINER" >"$RECOVERY_LOG_PATH" 2>&1 || true
  [[ -f "$RECOVERY_INSPECT_PATH" ]] || docker inspect "$RECOVERY_CONTAINER" >"$RECOVERY_INSPECT_PATH" 2>/dev/null || true
  [[ -f "$OOM_VALUE_PATH" ]] || printf '%s\n' "$OOM_KILLED" >"$OOM_VALUE_PATH"
}
write_summary_artifacts() {
  local evidence_dir source_sha
  evidence_dir="${RESULTS_DIR#"$REPO_ROOT"/}"
  source_sha="$(git -C "$REPO_ROOT" rev-parse HEAD)"
  ACKNOWLEDGED_COUNT="$(unique_count "$ACK_ORACLE_PATH")"
  EXPLICIT_REJECTED_COUNT="$(wc -l <"$EXPLICIT_REJECTION_PATH" 2>/dev/null | tr -d ' ')"
  NO_ACK_COUNT="$(no_ack_object_id_count)"
  RECOVERED_ACKNOWLEDGED_COUNT="$(unique_count "$RECOVERED_ACK_PATH")"
  RECOVERED_REJECTED_COUNT="$(unique_count "$RECOVERED_REJECTED_PATH")"
  {
    echo "script_exit_code=$SCRIPT_EXIT_CODE"
    echo "interrupted_exit_code=$INTERRUPTED_EXIT_CODE"
  } >"$SCRIPT_EXIT_PATH"
  python3 - "$OUTCOME" "$OOM_KILLED" "$ACKNOWLEDGED_COUNT" "$RECOVERED_ACKNOWLEDGED_COUNT" \
    "$EXPLICIT_REJECTED_COUNT" "$RECOVERED_REJECTED_COUNT" "$NO_ACK_COUNT" "$evidence_dir" \
    "$source_sha" "$SCRIPT_EXIT_CODE" "$INTERRUPTED_EXIT_CODE" "$NEGATIVE_EMPTY_RESTART" >"$SUMMARY_PATH" <<'PY'
import json
import sys
(
    outcome,
    oom_killed,
    acknowledged,
    recovered_acknowledged,
    explicit_rejected,
    recovered_rejected,
    no_ack,
    evidence_dir,
    source_sha,
    script_exit,
    interrupted,
    negative_empty_restart,
) = sys.argv[1:13]
print(json.dumps({
    "outcome": outcome,
    "oom_killed": oom_killed,
    "acknowledged_count": int(acknowledged),
    "recovered_acknowledged_count": int(recovered_acknowledged),
    "explicit_rejected_attempted_count": int(explicit_rejected),
    "recovered_explicit_rejected_count": int(recovered_rejected),
    "no_ack_diagnostic_attempted_count": int(no_ack),
    "evidence_dir": evidence_dir,
    "source_sha": source_sha,
    "script_exit_code": int(script_exit),
    "interrupted_exit_code": int(interrupted),
    "negative_empty_restart": negative_empty_restart == "1",
}, separators=(",", ":")))
PY
  printf 'oom_durability_summary outcome=%s OOMKilled=%s acknowledged_count=%s recovered_acknowledged_count=%s explicit_rejected_attempted_count=%s recovered_explicit_rejected_count=%s no_ack_diagnostic_attempted_count=%s evidence_dir=%s source_sha=%s script_exit_code=%s interrupted_exit_code=%s negative_empty_restart=%s\n' \
    "$OUTCOME" "$OOM_KILLED" "$ACKNOWLEDGED_COUNT" "$RECOVERED_ACKNOWLEDGED_COUNT" \
    "$EXPLICIT_REJECTED_COUNT" "$RECOVERED_REJECTED_COUNT" "$NO_ACK_COUNT" \
    "$evidence_dir" "$source_sha" "$SCRIPT_EXIT_CODE" "$INTERRUPTED_EXIT_CODE" \
    "$NEGATIVE_EMPTY_RESTART" >"$SUMMARY_LINE_PATH"
  cat "$SUMMARY_LINE_PATH"
}
finalize_evidence() {
  if [[ "$FINALIZED" == "true" || -z "$RESULTS_DIR" ]]; then
    return
  fi
  FINALIZED="true"
  capture_missing_artifacts
  write_summary_artifacts
}
cleanup() {
  if [[ "$CLEANUP_COMPLETE" == "true" ]]; then
    return
  fi
  CLEANUP_COMPLETE="true"
  docker logs "$SERVER_CONTAINER" >"$SERVER_LOG_PATH" 2>&1 || true
  docker logs "$RECOVERY_CONTAINER" >"$RECOVERY_LOG_PATH" 2>&1 || true
  docker inspect "$RECOVERY_CONTAINER" >"$RECOVERY_INSPECT_PATH" 2>/dev/null || true
  docker rm -f "$SERVER_CONTAINER" "$RECOVERY_CONTAINER" >/dev/null 2>&1 || true
  docker volume rm "$DATA_VOLUME" "$NEGATIVE_DATA_VOLUME" >/dev/null 2>&1 || true
  docker network rm "$NETWORK_NAME" >/dev/null 2>&1 || true
  if [[ -z "${FLAPJACK_OOM_IMAGE:-}" && -n "$IMAGE_TAG" ]]; then
    docker image rm "$IMAGE_TAG" >/dev/null 2>&1 || true
  fi
}
handle_exit() {
  local exit_code="$?"
  if (( SCRIPT_EXIT_CODE == 0 && exit_code != 0 )); then
    SCRIPT_EXIT_CODE="$exit_code"
  fi
  finalize_evidence
  cleanup
  exit "$SCRIPT_EXIT_CODE"
}
handle_signal() {
  local signal_exit="$1"
  INTERRUPTED_EXIT_CODE="$signal_exit"
  SCRIPT_EXIT_CODE="$signal_exit"
  finalize_evidence
  cleanup
  exit "$signal_exit"
}
run_probe() {
  require_command docker
  require_command curl
  require_command jq
  require_command python3
  require_file "$DOCKERFILE"
  validate_workload_configuration
  initialize_run_identity
  initialize_results
  HOST_PORT="${HOST_PORT:-$(choose_loopback_port)}"
  BASE_URL="http://127.0.0.1:$HOST_PORT"
  build_image
  create_run_docker_resources
  start_server_container "$SERVER_CONTAINER" "$DATA_VOLUME" "$HOST_PORT" "normal"
  wait_for_health "$BASE_URL" "$SERVER_CONTAINER"
  printf 'pre_oom_base_url=%s\n' "$BASE_URL" >"$BASE_URL_PATH"
  drive_acknowledged_writes
  prepare_queue_full_server
  drive_explicit_rejection
  trigger_kernel_oom
  start_recovery_server
  query_recovered_ids
  query_rejected_ids
  assert_recovery_contract
  OUTCOME="PASS"
  finalize_evidence
}
validate_workload_configuration() {
  [[ "$TOTAL_DOCS" =~ ^[1-9][0-9]*$ ]] || fail "FLAPJACK_OOM_TOTAL_DOCS must be a positive integer"
  [[ "$BATCH_SIZE" =~ ^[1-9][0-9]*$ ]] || fail "FLAPJACK_OOM_BATCH_SIZE must be a positive integer"
  ((TOTAL_DOCS <= 1000)) || fail "FLAPJACK_OOM_TOTAL_DOCS must not exceed the 1000-hit recovery query limit"
  [[ "$OOM_FILL_MIB" =~ ^[1-9][0-9]*$ ]] || fail "FLAPJACK_OOM_FILL_MIB must be a positive integer"
}
run_self_test() {
  bash "$ENGINE_DIR/loadtest/tests/oom_kill_durability_self_test.sh"
}
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  trap handle_exit EXIT
  trap 'handle_signal 129' HUP
  trap 'handle_signal 130' INT
  trap 'handle_signal 143' TERM
  if [[ "${1:-}" == "--self-test" ]]; then
    trap - EXIT HUP INT TERM
    run_self_test
  else
    run_probe "$@"
  fi
fi
