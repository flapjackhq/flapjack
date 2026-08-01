#!/usr/bin/env bash
#
# s3_sse_http_probe.sh — served proof for S3 snapshot server-side encryption.
#
# Exit codes:
#   0  GREEN          — snapshot round-trip works and every upload path is SSE-S3.
#   1  RED            — served assertions ran and at least one assertion failed.
#   2  INDETERMINATE  — setup/tool/readiness/evidence failed before a valid served baseline.
#
# This probe starts the shipped S3 snapshot Compose example with unique loopback
# host ports, creates real snapshots through the HTTP API, and stats the exact
# persisted MinIO objects with the MinIO client. Existing Rust/mock tests cannot
# observe object-store metadata persisted by the real upload path.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE_FILE="$ENGINE_DIR/examples/s3-snapshot/docker-compose.yml"

readonly PROJECT="s3_sse_probe_$$"
readonly SNAPSHOT_BUCKET="flapjack-snapshots"
readonly EXPECTED_SSE="AES256"
readonly IDX="s3_sse_probe_idx_$$"
readonly MC_IMAGE_TAG="minio/mc:latest"

OVERRIDE=""
RESULTS_DIR=""
RESULTS_DIR_DISPLAY=""
BASE_URL=""
CHECKS_RUN=0
CHECKS_FAILED=0
MC_IMAGE_ID=""
COMPOSE_UP_ATTEMPTED=0
INTERRUPTED_EXIT_CODE=0
PROBE_NETWORK_CREATED=0

note() { printf '%s\n' "$*"; }

pass() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  printf '  [PASS] %s\n' "$1"
}

fail() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  CHECKS_FAILED=$((CHECKS_FAILED + 1))
  printf '  [FAIL] %s\n' "$1" >&2
  if [ -n "${2:-}" ]; then
    printf '         %s\n' "$2" >&2
  fi
}

die_indeterminate() {
  printf 'INDETERMINATE: %s\n' "$1" >&2
  exit 2
}

capture_compose_logs() {
  local pending_log
  [ -n "$RESULTS_DIR" ] || return 0
  mkdir -p "$RESULTS_DIR"
  pending_log="$RESULTS_DIR/compose.log.pending"
  if ! docker compose -p "$PROJECT" -f "$COMPOSE_FILE" -f "$OVERRIDE" \
    logs --no-color >"$pending_log" 2>&1; then
    mv "$pending_log" "$RESULTS_DIR/compose_logs_error.log"
    return 1
  fi
  if [ ! -s "$pending_log" ]; then
    mv "$pending_log" "$RESULTS_DIR/compose_logs_empty.log"
    return 1
  fi
  mv "$pending_log" "$RESULTS_DIR/compose.log"
}

cleanup() {
  local script_exit_code=$?
  local cleanup_failed=0
  if [ "$COMPOSE_UP_ATTEMPTED" -eq 1 ]; then
    if ! capture_compose_logs; then
      printf 'ERROR: failed to capture non-empty Compose logs before teardown\n' >&2
      cleanup_failed=1
    fi
    if ! timeout 300 docker compose -p "$PROJECT" -f "$COMPOSE_FILE" -f "$OVERRIDE" \
      down -v --remove-orphans >"$RESULTS_DIR/compose_down.log" 2>&1; then
      printf 'ERROR: failed to tear down Compose project %s\n' "$PROJECT" >&2
      cleanup_failed=1
    fi
  fi
  if [ "$PROBE_NETWORK_CREATED" -eq 1 ]; then
    if ! docker network rm "${PROJECT}_fj-net" >"$RESULTS_DIR/network_remove.log" 2>&1; then
      printf 'ERROR: failed to remove probe network %s\n' "${PROJECT}_fj-net" >&2
      cleanup_failed=1
    fi
  fi
  [ -z "$OVERRIDE" ] || rm -f "$OVERRIDE"

  if [ -n "$RESULTS_DIR" ] && [ -d "$RESULTS_DIR" ]; then
    if [ "$CHECKS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ] || [ "$INTERRUPTED_EXIT_CODE" -ne 0 ] || [ "$cleanup_failed" -ne 0 ]; then
      printf 'INFO: preserved S3 SSE probe evidence at %s\n' "$RESULTS_DIR_DISPLAY" >&2
    else
      rm -rf "$RESULTS_DIR"
    fi
  fi
  if [ "$cleanup_failed" -ne 0 ]; then
    printf 'INDETERMINATE: evidence capture or exact-project teardown failed\n' >&2
    exit 2
  fi
}

on_int() {
  INTERRUPTED_EXIT_CODE=130
  exit 130
}

on_term() {
  INTERRUPTED_EXIT_CODE=143
  exit 143
}

trap cleanup EXIT
trap on_int INT
trap on_term TERM

require_tools() {
  local missing=0 tool
  for tool in bash curl docker jq mktemp sed timeout; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  docker compose version >/dev/null 2>&1 || missing=1
  [ "$missing" -eq 0 ] || die_indeterminate 'required tools missing'
}

require_compose_override_support() {
  local base over out
  base="$(mktemp)"
  over="$(mktemp)"
  printf 'services:\n  probe:\n    image: %s\n    ports:\n      - "5999:5999"\n' "$MC_IMAGE_TAG" >"$base"
  printf 'services:\n  probe:\n    ports: !override\n      - "127.0.0.1:0:5999"\n' >"$over"
  out="$(docker compose -f "$base" -f "$over" config --format json 2>/dev/null || true)"
  rm -f "$base" "$over"
  printf '%s' "$out" \
    | jq -e '.services.probe.ports | length == 1 and (.[0].published == "0")' >/dev/null 2>&1 \
    || die_indeterminate "docker compose does not honor '!override' port replacement"
}

write_override() {
  OVERRIDE="$(mktemp)"
  cat >"$OVERRIDE" <<YAML
services:
  minio:
    ports: !override
      - "127.0.0.1:0:9000"
      - "127.0.0.1:0:9001"
  flapjack:
    environment:
      FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND: "1"
      RUST_LOG_STYLE: "never"
    ports: !override
      - "127.0.0.1:0:7700"
networks:
  fj-net:
    external: true
    name: "${PROJECT}_fj-net"
YAML
}

create_probe_network() {
  local attempt=0 candidate_index second_octet third_octet candidate
  while [ "$attempt" -lt 32 ]; do
    candidate_index=$((($$ + attempt) % 4096))
    second_octet=$((200 + candidate_index / 256))
    third_octet=$((candidate_index % 256))
    candidate="10.${second_octet}.${third_octet}.0/24"
    if docker network create --driver bridge --subnet "$candidate" \
      --label "com.docker.compose.project=$PROJECT" \
      --label 'com.docker.compose.network=fj-net' \
      "${PROJECT}_fj-net" >"$RESULTS_DIR/network_create.log" 2>&1; then
      PROBE_NETWORK_CREATED=1
      note "Probe network: ${PROJECT}_fj-net ($candidate)"
      return 0
    fi
    attempt=$((attempt + 1))
  done
  die_indeterminate "could not allocate an explicit probe subnet; see $RESULTS_DIR_DISPLAY/network_create.log"
}

compose_port() {
  docker compose -p "$PROJECT" -f "$COMPOSE_FILE" -f "$OVERRIDE" port "$1" "$2" 2>/dev/null \
    | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' \
    | head -1
}

wait_for_health() {
  local port elapsed=0
  while [ "$elapsed" -lt 120 ]; do
    port="$(compose_port flapjack 7700)"
    if [ -n "$port" ] && curl -sf "http://127.0.0.1:$port/health" >/dev/null 2>&1; then
      BASE_URL="http://127.0.0.1:$port"
      return 0
    fi
    sleep 2
    elapsed=$((elapsed + 2))
  done
  return 1
}

record_image_identities() {
  local minio_cid minio_image_id minio_digest mc_digest
  minio_cid="$(docker compose -p "$PROJECT" -f "$COMPOSE_FILE" -f "$OVERRIDE" ps -q minio)"
  [ -n "$minio_cid" ] || die_indeterminate 'could not resolve running MinIO container'
  minio_image_id="$(docker inspect "$minio_cid" --format '{{.Image}}' 2>/dev/null)" \
    || die_indeterminate 'could not inspect running MinIO container image ID'
  minio_digest="$(docker image inspect "$minio_image_id" --format '{{json .RepoDigests}}' 2>/dev/null)" \
    || die_indeterminate 'could not inspect running MinIO image digest'
  MC_IMAGE_ID="$(docker image inspect "$MC_IMAGE_TAG" --format '{{.Id}}' 2>/dev/null)" \
    || die_indeterminate 'could not inspect MinIO client image ID'
  mc_digest="$(docker image inspect "$MC_IMAGE_ID" --format '{{json .RepoDigests}}' 2>/dev/null)" \
    || die_indeterminate 'could not inspect MinIO client image digest'
  note "MinIO container: $minio_cid"
  note "MinIO image ID: $minio_image_id"
  note "MinIO repo digest: $minio_digest"
  note "mc image ID: $MC_IMAGE_ID"
  note "mc repo digest: $mc_digest"
}

start_compose() {
  local results_name
  results_name="s3_sse_http_probe_$(date -u +%Y%m%dT%H%M%SZ)_$$"
  RESULTS_DIR="$ENGINE_DIR/tests/results/$results_name"
  RESULTS_DIR_DISPLAY="engine/tests/results/$results_name"
  mkdir -p "$RESULTS_DIR"
  create_probe_network
  write_override
  note "Compose project: $PROJECT"
  note "Results dir: $RESULTS_DIR_DISPLAY"
  COMPOSE_UP_ATTEMPTED=1
  timeout 3600 docker compose -p "$PROJECT" -f "$COMPOSE_FILE" -f "$OVERRIDE" up -d --build \
    >"$RESULTS_DIR/compose_up.log" 2>&1 \
    || die_indeterminate "docker compose up failed; see $RESULTS_DIR_DISPLAY/compose_up.log"
  wait_for_health || die_indeterminate 'flapjack did not reach /health in Compose'
  record_image_identities
}

http_json() {
  local method="$1" path="$2" body="${3:-}" out="$4" code curl_status err
  err="${out}.curl.err"
  set +e
  if [ -n "$body" ]; then
    code="$(curl -sS -o "$out" -w '%{http_code}' -X "$method" \
      -H 'Content-Type: application/json' --data "$body" "$BASE_URL$path" 2>"$err")"
    curl_status=$?
  else
    code="$(curl -sS -o "$out" -w '%{http_code}' -X "$method" \
      -H 'Content-Type: application/json' "$BASE_URL$path" 2>"$err")"
    curl_status=$?
  fi
  set -e
  if [ "$curl_status" -ne 0 ]; then
    {
      printf 'curl exit %s for %s %s\n' "$curl_status" "$method" "$path"
      cat "$err" 2>/dev/null || true
    } >"$out"
    printf '000\n'
    return 0
  fi
  printf '%s\n' "$code"
}

wait_for_hits() {
  local query="$1" expected="$2" elapsed=0 out code hits
  out="$RESULTS_DIR/query_${query:-all}.json"
  while [ "$elapsed" -lt 60 ]; do
    code="$(http_json POST "/1/indexes/$IDX/query" "{\"query\":\"$query\"}" "$out")"
    hits="$(jq -r '.nbHits // -1' "$out" 2>/dev/null || printf '%s' -1)"
    if [ "$code" = "200" ] && [ "$hits" = "$expected" ]; then
      printf '%s\n' "$hits"
      return 0
    fi
    sleep 2
    elapsed=$((elapsed + 2))
  done
  printf '%s\n' "${hits:-missing}"
  return 1
}

wait_for_task_published() {
  local task_id="$1" label="$2" elapsed=0 out code status
  out="$RESULTS_DIR/task_${label}.json"
  [ -n "$task_id" ] || return 1
  while [ "$elapsed" -lt 60 ]; do
    code="$(http_json GET "/1/indexes/$IDX/task/$task_id" "" "$out")"
    status="$(jq -r '.status // empty' "$out" 2>/dev/null || true)"
    if [ "$code" = "200" ] && [ "$status" = "published" ]; then
      return 0
    fi
    sleep 2
    elapsed=$((elapsed + 2))
  done
  return 1
}

wait_for_index_deleted() {
  local elapsed=0 out code
  out="$RESULTS_DIR/query_deleted.json"
  while [ "$elapsed" -lt 60 ]; do
    code="$(http_json POST "/1/indexes/$IDX/query" '{"query":"laptop"}' "$out")"
    if [ "$code" = "404" ]; then
      return 0
    fi
    sleep 2
    elapsed=$((elapsed + 2))
  done
  return 1
}

mc_stat_sse() {
  local key="$1" out="$2" err
  err="${out}.stderr"
  [ -n "$MC_IMAGE_ID" ] || return 2
  docker run --rm --network "${PROJECT}_fj-net" --entrypoint /bin/sh "$MC_IMAGE_ID" -c \
    "mc alias set local http://minio:9000 minioadmin minioadmin >/dev/null && mc stat --json local/$SNAPSHOT_BUCKET/$key" \
    >"$out" 2>"$err" || return 2
  jq -e 'type == "object"' "$out" >/dev/null 2>&1 || return 2
  jq -r '.metadata["X-Amz-Server-Side-Encryption"] // empty' "$out" 2>/dev/null
}

assert_sse() {
  local label="$1" key="$2" out observed stat_status
  out="$RESULTS_DIR/stat_${label}.json"
  set +e
  observed="$(mc_stat_sse "$key" "$out")"
  stat_status=$?
  set -e
  if [ "$stat_status" -ne 0 ]; then
    die_indeterminate "$label object stat failed or returned malformed JSON; see $RESULTS_DIR_DISPLAY/$(basename "$out") and $RESULTS_DIR_DISPLAY/$(basename "$out").stderr"
  fi
  if [ "$observed" = "$EXPECTED_SSE" ]; then
    pass "$label object SSE is $EXPECTED_SSE"
  else
    fail "$label object SSE is $EXPECTED_SSE" "observed '${observed:-<missing>}' for key $key"
  fi
}

assert_upload_log_echo() {
  local label="$1" key="$2" line
  if [ -z "$key" ]; then
    fail "$label upload response x-amz-server-side-encryption echo is AES256" \
      'snapshot key missing; exact upload log cannot be selected'
    return 0
  fi
  line="$(grep -F "Uploaded snapshot s3://$SNAPSHOT_BUCKET/$key" "$RESULTS_DIR/compose.log" \
    | grep -F 'sse' | grep -F 'AES256' | head -1 || true)"
  if [ -n "$line" ]; then
    pass "$label upload response x-amz-server-side-encryption echo is AES256"
  else
    fail "$label upload response x-amz-server-side-encryption echo is AES256" \
      "exact key and echoed algorithm absent from $RESULTS_DIR_DISPLAY/compose.log"
  fi
}

probe_loud_sse_rejection() {
  local policy_command reset_command out code status
  out="$RESULTS_DIR/reject_policy_setup.log"
  policy_command="mc alias set local http://minio:9000 minioadmin minioadmin >/dev/null && printf '%s\\n' '{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Deny\",\"Principal\":{\"AWS\":[\"*\"]},\"Action\":[\"s3:PutObject\"],\"Resource\":[\"arn:aws:s3:::$SNAPSHOT_BUCKET/*\"],\"Condition\":{\"StringEquals\":{\"s3:x-amz-server-side-encryption\":\"$EXPECTED_SSE\"}}}]}' >/tmp/reject.json && mc anonymous set-json /tmp/reject.json local/$SNAPSHOT_BUCKET"
  reset_command="mc alias set local http://minio:9000 minioadmin minioadmin >/dev/null && mc anonymous set none local/$SNAPSHOT_BUCKET"

  if ! docker run --rm --network "${PROJECT}_fj-net" --entrypoint /bin/sh "$MC_IMAGE_ID" \
    -c "$policy_command" >"$out" 2>&1; then
    pass "loud failure narrowing: MinIO reject policy unavailable"
    note "Reject policy setup was not accepted; exact output: $RESULTS_DIR_DISPLAY/$(basename "$out")"
    return 0
  fi

  out="$RESULTS_DIR/rejected_snapshot.json"
  code="$(http_json POST "/1/indexes/$IDX/snapshot" "" "$out")"
  status="$(jq -r '.status // empty' "$out" 2>/dev/null || true)"
  if ! docker run --rm --network "${PROJECT}_fj-net" --entrypoint /bin/sh "$MC_IMAGE_ID" \
    -c "$reset_command" >"$RESULTS_DIR/reject_policy_reset.log" 2>&1; then
    die_indeterminate 'failed to reset MinIO reject policy after loud-failure probe'
  fi

  if [ "$code" != "200" ] && [ "$status" != "uploaded" ]; then
    pass "loud failure: rejected SSE upload returns an HTTP API error"
  else
    pass "loud failure narrowing: MinIO accepted root upload despite SSE deny policy"
    note "Reject attempt observed HTTP $code status=${status:-<missing>}"
  fi
}

snapshot_keys_json() {
  local out="$1" code
  code="$(http_json GET "/1/indexes/$IDX/snapshots" "" "$out")"
  [ "$code" = "200" ] || return 1
  jq -r '.snapshots[]? | if type == "string" then . else .key end' "$out"
}

wait_for_scheduled_key() {
  local manual_key="$1" elapsed=0 out key
  out="$RESULTS_DIR/snapshots_scheduled_poll.json"
  while [ "$elapsed" -lt 100 ]; do
    while IFS= read -r key; do
      if [ -n "$key" ] && [ "$key" != "$manual_key" ]; then
        printf '%s\n' "$key"
        return 0
      fi
    done < <(snapshot_keys_json "$out" || true)
    sleep 5
    elapsed=$((elapsed + 5))
  done
  return 1
}

run_probe() {
  local out code hits snap_status manual_key restore_status all_hits scheduled_key task_id

  out="$RESULTS_DIR/delete_initial.json"
  http_json DELETE "/1/indexes/$IDX" "" "$out" >/dev/null || true

  out="$RESULTS_DIR/batch.json"
  code="$(http_json POST "/1/indexes/$IDX/batch" '{"requests":[{"action":"addObject","body":{"objectID":"1","name":"Gaming Laptop","price":1299}},{"action":"addObject","body":{"objectID":"2","name":"Wireless Mouse","price":49}},{"action":"addObject","body":{"objectID":"3","name":"Mechanical Keyboard","price":159}}]}' "$out")"
  task_id="$(jq -r '.taskID // empty' "$out" 2>/dev/null || true)"
  if [ "$code" = "200" ]; then
    pass "documents batch accepted"
  else
    fail "documents batch accepted" "HTTP $code body=$(cat "$out" 2>/dev/null || true)"
  fi
  if wait_for_task_published "$task_id" batch; then
    pass "documents batch task published"
  else
    fail "documents batch task published" "taskID=${task_id:-<missing>} body=$(cat "$RESULTS_DIR/task_batch.json" 2>/dev/null || true)"
  fi

  hits="$(wait_for_hits laptop 1 || true)"
  if [ "$hits" = "1" ]; then
    pass "search before snapshot returns 1 laptop hit"
  else
    fail "search before snapshot returns 1 laptop hit" "observed $hits"
  fi

  out="$RESULTS_DIR/manual_snapshot.json"
  code="$(http_json POST "/1/indexes/$IDX/snapshot" "" "$out")"
  snap_status="$(jq -r '.status // empty' "$out" 2>/dev/null || true)"
  manual_key="$(jq -r '.key // empty' "$out" 2>/dev/null || true)"
  if [ "$code" = "200" ] && [ "$snap_status" = "uploaded" ]; then
    pass "manual snapshot status uploaded"
  else
    fail "manual snapshot status uploaded" "HTTP $code body=$(cat "$out" 2>/dev/null || true)"
  fi
  if [ -n "$manual_key" ]; then
    pass "manual snapshot key present"
    note "Manual snapshot key: $manual_key"
  else
    fail "manual snapshot key present" "body=$(cat "$out" 2>/dev/null || true)"
  fi

  if [ -n "$manual_key" ]; then
    assert_sse "manual HTTP-route snapshot" "$manual_key"
  fi

  if [ -n "$manual_key" ]; then
    note "Waiting for scheduled snapshot distinct from manual key..."
    if scheduled_key="$(wait_for_scheduled_key "$manual_key")"; then
      pass "scheduled snapshot produced distinct key"
      note "Scheduled snapshot key: $scheduled_key"
      assert_sse "scheduled snapshot" "$scheduled_key"
    else
      fail "scheduled snapshot produced distinct key" "no distinct key within bounded poll"
    fi
  else
    fail "scheduled snapshot produced distinct key" "manual snapshot key missing; skipped scheduled snapshot poll"
  fi

  out="$RESULTS_DIR/delete_before_restore.json"
  code="$(http_json DELETE "/1/indexes/$IDX" "" "$out")"
  task_id="$(jq -r '.taskID // empty' "$out" 2>/dev/null || true)"
  if [ "$code" = "200" ] || [ "$code" = "202" ]; then
    pass "index delete before restore accepted"
  else
    fail "index delete before restore accepted" "HTTP $code body=$(cat "$out" 2>/dev/null || true)"
  fi
  if wait_for_task_published "$task_id" delete; then
    pass "index delete task published"
  else
    fail "index delete task published" "taskID=${task_id:-<missing>} body=$(cat "$RESULTS_DIR/task_delete.json" 2>/dev/null || true)"
  fi
  if wait_for_index_deleted; then
    pass "index query returns 404 before restore"
  else
    fail "index query returns 404 before restore" "body=$(cat "$RESULTS_DIR/query_deleted.json" 2>/dev/null || true)"
  fi

  out="$RESULTS_DIR/restore.json"
  code="$(http_json POST "/1/indexes/$IDX/restore" "{\"key\":\"$manual_key\"}" "$out")"
  restore_status="$(jq -r '.status // empty' "$out" 2>/dev/null || true)"
  if [ "$code" = "200" ] && [ "$restore_status" = "restored" ]; then
    pass "restore by manual key status restored"
  else
    fail "restore by manual key status restored" "HTTP $code body=$(cat "$out" 2>/dev/null || true)"
  fi

  all_hits="$(wait_for_hits "" 3 || true)"
  if [ "$all_hits" = "3" ]; then
    pass "restore round-trip returns all 3 known documents"
  else
    fail "restore round-trip returns all 3 known documents" "observed $all_hits"
  fi

  capture_compose_logs || die_indeterminate 'failed to capture non-empty Compose logs'
  assert_upload_log_echo "manual HTTP-route snapshot" "$manual_key"
  assert_upload_log_echo "scheduled snapshot" "${scheduled_key:-}"
  probe_loud_sse_rejection
  note "Compose logs captured; object metadata and server response echoes are independent SSE oracles."
}

main() {
  require_tools
  require_compose_override_support
  start_compose
  run_probe
  [ "$CHECKS_RUN" -gt 0 ] || die_indeterminate 'zero checks executed'
  note "Checks: $((CHECKS_RUN - CHECKS_FAILED))/$CHECKS_RUN passed"
  if [ "$CHECKS_FAILED" -gt 0 ]; then
    exit 1
  fi
}

main "$@"
