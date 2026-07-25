#!/usr/bin/env bash
# HA auto-heal integration test: proves majority-loss refusal and legal eviction.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.yml"
COMPOSE_ARGS=(-f "$COMPOSE_FILE" -f "$SCRIPT_DIR/docker-compose.autoheal.yml")
. "$SCRIPT_DIR/_ha_lib.sh"

MAJORITY_REASON="2 failed peers constitute a majority of configured peers; local node may be isolated"
EVICT_REASON="sustained failure threshold reached and quorum remains"
WITH_DOCKER=false
if [ "${1:-}" = "--with-docker" ]; then WITH_DOCKER=true; fi

EVIDENCE_DIR="$SCRIPT_DIR/.evidence/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$EVIDENCE_DIR"
TEARDOWN_DONE=false

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

preflight() {
  command -v docker >/dev/null 2>&1 || fail "docker command is required"
  docker info >/dev/null 2>&1 || fail "docker daemon is not reachable"
  docker compose version >/dev/null 2>&1 || fail "docker compose support is required"
}

capture_status() {
  local label="$1"
  for service in node-a node-b node-c; do
    cluster_status_compose "$service" >"$EVIDENCE_DIR/${label}_${service}_status.json" 2>/dev/null || true
  done
}

capture_evidence() {
  local label="$1"
  mkdir -p "$EVIDENCE_DIR"
  capture_status "$label"
  compose ps >"$EVIDENCE_DIR/${label}_compose_ps.txt" 2>&1 || true
  compose logs --no-color >"$EVIDENCE_DIR/${label}_compose_logs.txt" 2>&1 || true
}

cleanup() {
  local status=$?
  if [ "$status" -eq 0 ]; then
    capture_evidence "pass"
  else
    capture_evidence "fail"
  fi
  if $WITH_DOCKER && ! $TEARDOWN_DONE; then
    compose down -v || true
    TEARDOWN_DONE=true
  fi
  echo "Evidence: $EVIDENCE_DIR"
  exit "$status"
}
trap cleanup EXIT

assert_exact_mesh() {
  for service in node-a node-b node-c; do
    local total
    total=$(peers_total_compose "$service")
    [ "$total" = "2" ] || fail "$service expected peers_total=2, got '$total'"
  done
}

wait_enabled_cluster_ready() {
  wait_healthy_compose "node-a" "node-a"
  wait_healthy_compose "node-b" "node-b"
  wait_healthy_compose "node-c" "node-c"
  wait_for_peer_mesh_ready 60 || fail "peer mesh did not converge to 3 nodes"
  assert_exact_mesh
}

assert_majority_refusal_status() {
  local status="$1"
  printf "%s\n" "$status" | MAJORITY_REASON="$MAJORITY_REASON" \
    python3 "$SCRIPT_DIR/ha_autoheal_assertions.py" majority_refusal
}

assert_eviction_status() {
  local status="$1"
  printf "%s\n" "$status" | EVICT_REASON="$EVICT_REASON" \
    python3 "$SCRIPT_DIR/ha_autoheal_assertions.py" eviction
}

assert_readmission_status() {
  local status="$1"
  printf "%s\n" "$status" | python3 "$SCRIPT_DIR/ha_autoheal_assertions.py" readmission
}

assert_disabled_status() {
  local status="$1"
  printf "%s\n" "$status" | python3 "$SCRIPT_DIR/ha_autoheal_assertions.py" disabled
}

poll_status_assertion() {
  local service="$1" label="$2" max_wait="$3" assertion="$4"
  local elapsed=0
  local last_error="$EVIDENCE_DIR/${label}_last_error.txt"
  : >"$last_error"

  while [ "$elapsed" -lt "$max_wait" ]; do
    local status
    status=$(cluster_status_compose "$service")
    printf "%s\n" "$status" >"$EVIDENCE_DIR/${label}_${service}_latest_status.json"
    if summary=$("$assertion" "$status" 2>"$last_error"); then
      printf "%s\n" "$summary" >"$EVIDENCE_DIR/${label}_summary.json"
      echo "$summary"
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done

  fail "$label did not reach expected status within ${max_wait}s; last assertion: $(cat "$last_error")"
}

json_payload() {
  python3 - "$@" <<'PY'
import json
import sys

mode = sys.argv[1]
if mode == "batch_one":
    object_id = sys.argv[2]
    term = sys.argv[3]
    print(json.dumps({
        "requests": [
            {
                "action": "addObject",
                "body": {
                    "_id": object_id,
                    "objectID": object_id,
                    "title": f"Autoheal catchup {term}",
                    "term": term,
                },
            }
        ]
    }))
elif mode == "query":
    print(json.dumps({"query": sys.argv[2], "hitsPerPage": 10}))
else:
    raise SystemExit(f"unknown payload mode {mode!r}")
PY
}

post_json_compose() {
  local service="$1" path="$2" payload="$3"
  compose exec -T "$service" curl -sf -X POST "http://localhost:7700$path" \
    -H 'Content-Type: application/json' \
    -d "$payload"
}

query_index_compose() {
  local service="$1" index="$2" query="$3" payload
  payload=$(json_payload query "$query")
  post_json_compose "$service" "/1/indexes/$index/query" "$payload"
}

assert_exact_query_result() {
  local raw_json="$1" object_id="$2" label="$3"
  printf "%s\n" "$raw_json" | OBJECT_ID="$object_id" LABEL="$label" \
    python3 "$SCRIPT_DIR/ha_autoheal_assertions.py" exact_query
}

poll_exact_query_result() {
  local service="$1" label="$2" index="$3" query="$4" object_id="$5" max_wait="$6"
  local elapsed=0
  local last_error="$EVIDENCE_DIR/${label}_last_error.txt"
  : >"$last_error"

  while [ "$elapsed" -lt "$max_wait" ]; do
    local raw
    raw=$(query_index_compose "$service" "$index" "$query" 2>"$last_error") || raw="{}"
    printf "%s\n" "$raw" >"$EVIDENCE_DIR/${label}_${service}_query.json"
    if summary=$(assert_exact_query_result "$raw" "$object_id" "$label" 2>"$last_error"); then
      printf "%s\n" "$summary" >"$EVIDENCE_DIR/${label}_summary.json"
      echo "$summary"
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done

  fail "$label did not reach exact query result within ${max_wait}s; last assertion: $(cat "$last_error")"
}

write_catchup_document() {
  local service="$1" index="$2" object_id="$3" term="$4" label="$5" payload
  payload=$(json_payload batch_one "$object_id" "$term")
  post_json_compose "$service" "/1/indexes/$index/batch" "$payload" \
    >"$EVIDENCE_DIR/${label}_${service}_write_response.json"
}

preflight

if $WITH_DOCKER; then
  echo "=== Building and starting auto-heal-enabled 3-node HA cluster ==="
  compose up -d --build
fi

echo ""
echo "=== 1. Enabled cluster startup ==="
wait_enabled_cluster_ready

echo ""
echo "=== 2. Majority-loss refusal ==="
compose stop node-b node-c
MAJORITY_SUMMARY=$(poll_status_assertion "node-a" "majority_refusal" 90 assert_majority_refusal_status)
echo "  Majority refusal: $MAJORITY_SUMMARY"

echo ""
echo "=== 3. Reconverge after refusal ==="
compose start node-b node-c
wait_enabled_cluster_ready

echo ""
echo "=== 4. Legal single-node eviction ==="
compose stop node-c
EVICTION_A_SUMMARY=$(poll_status_assertion "node-a" "single_eviction_node_a" 90 assert_eviction_status)
EVICTION_B_SUMMARY=$(poll_status_assertion "node-b" "single_eviction_node_b" 90 assert_eviction_status)
echo "  Single-node eviction node-a: $EVICTION_A_SUMMARY"
echo "  Single-node eviction node-b: $EVICTION_B_SUMMARY"

echo ""
echo "=== 5. Enabled readmission and catch-up ==="
EVICTION_A_CONFIRM=$(poll_status_assertion "node-a" "single_eviction_confirm_node_a" 15 assert_eviction_status)
EVICTION_B_CONFIRM=$(poll_status_assertion "node-b" "single_eviction_confirm_node_b" 15 assert_eviction_status)
echo "  Eviction still active node-a: $EVICTION_A_CONFIRM"
echo "  Eviction still active node-b: $EVICTION_B_CONFIRM"

CATCHUP_INDEX="autoheal-catchup-$(date +%s)"
CATCHUP_OBJECT_ID="autoheal-catchup-42"
CATCHUP_TERM="quokka"
write_catchup_document "node-a" "$CATCHUP_INDEX" "$CATCHUP_OBJECT_ID" "$CATCHUP_TERM" "catchup_source"
SOURCE_QUERY_SUMMARY=$(poll_exact_query_result "node-a" "catchup_source" "$CATCHUP_INDEX" "$CATCHUP_TERM" "$CATCHUP_OBJECT_ID" 30)
echo "  Source survivor query before restart: $SOURCE_QUERY_SUMMARY"

compose start node-c
wait_healthy_compose "node-c" "node-c"
READMISSION_A_SUMMARY=$(poll_status_assertion "node-a" "readmission_node_a" 90 assert_readmission_status)
READMISSION_B_SUMMARY=$(poll_status_assertion "node-b" "readmission_node_b" 90 assert_readmission_status)
echo "  Readmission node-a: $READMISSION_A_SUMMARY"
echo "  Readmission node-b: $READMISSION_B_SUMMARY"

NODE_C_QUERY_SUMMARY=$(poll_exact_query_result "node-c" "catchup_node_c" "$CATCHUP_INDEX" "$CATCHUP_TERM" "$CATCHUP_OBJECT_ID" 60)
echo "  node-c query after readmission: $NODE_C_QUERY_SUMMARY"
capture_evidence "enabled_readmission"

echo ""
echo "=== 6. Disabled flag negative control ==="
compose down -v
TEARDOWN_DONE=true
COMPOSE_ARGS=(-f "$COMPOSE_FILE")
TEARDOWN_DONE=false
compose up -d --build
wait_enabled_cluster_ready

compose stop node-c
DISABLED_START_EPOCH=$(date +%s)
sleep 35
DISABLED_END_EPOCH=$(date +%s)
DISABLED_ELAPSED=$((DISABLED_END_EPOCH - DISABLED_START_EPOCH))
printf "%s\n" "$DISABLED_ELAPSED" >"$EVIDENCE_DIR/disabled_elapsed.txt"

DISABLED_A_SUMMARY=$(poll_status_assertion "node-a" "disabled_node_a" 45 assert_disabled_status)
DISABLED_B_SUMMARY=$(poll_status_assertion "node-b" "disabled_node_b" 45 assert_disabled_status)
echo "  Disabled node-a: $DISABLED_A_SUMMARY"
echo "  Disabled node-b: $DISABLED_B_SUMMARY"
capture_evidence "disabled_final"

echo ""
echo "=== Final verified contract summaries ==="
echo "  Stage 1 majority refusal: $MAJORITY_SUMMARY"
echo "  Stage 1 legal eviction node-a: $EVICTION_A_SUMMARY"
echo "  Stage 1 legal eviction node-b: $EVICTION_B_SUMMARY"
echo "  Stage 2 pre-restart source query: $SOURCE_QUERY_SUMMARY"
echo "  Stage 2 readmission node-a: $READMISSION_A_SUMMARY"
echo "  Stage 2 readmission node-b: $READMISSION_B_SUMMARY"
echo "  Stage 2 node-c catch-up query: $NODE_C_QUERY_SUMMARY"
echo "  Stage 2 disabled elapsed seconds: $DISABLED_ELAPSED"
echo "  Stage 2 disabled node-a: $DISABLED_A_SUMMARY"
echo "  Stage 2 disabled node-b: $DISABLED_B_SUMMARY"
echo ""
echo "✓ Auto-heal majority refusal, single-node eviction, readmission catch-up, and disabled-control assertions passed"
