#!/usr/bin/env bash
# HA cluster integration test: 3-node Flapjack + nginx load balancer.
#
# Tests:
#   1. All nodes healthy via LB
#   2. Search replication across all 3 nodes
#   3. Node failover: writes via LB still work when one node is down
#   3b. Startup catch-up: restarted node serves the document written while it was down
#   4. Analytics fan-out: 3 nodes contributing
#
# Usage:
#   docker compose up -d --build
#   ./test_ha.sh
#   docker compose down -v
#
# Or fully automated:
#   ./test_ha.sh --with-docker

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.yml"
COMPOSE_ARGS=(-f "$COMPOSE_FILE")
. "$SCRIPT_DIR/_ha_lib.sh"

LB="http://localhost:7800"          # nginx load balancer (host-exposed)
PASS=0
FAIL=0
STALE_WRITE_ASSERTIONS=0
PEERS_NODE_A=0
PEERS_NODE_B=0
PEERS_NODE_C=0
INTERRUPTED_EXIT_CODE=0
CLEANUP_COMPLETE=false

# ── helpers ──────────────────────────────────────────────────────────────────

green() { printf "\033[32m✓\033[0m %s\n" "$*"; }
red()   { printf "\033[31m✗\033[0m %s\n" "$*"; }

assert_eq() {
  local label="$1" expected="$2" actual="$3"
  if [ "$actual" = "$expected" ]; then
    green "$label"
    PASS=$((PASS + 1))
  else
    red "$label (expected '$expected', got '$actual')"
    FAIL=$((FAIL + 1))
  fi
}

assert_ge() {
  local label="$1" expected="$2" actual="$3"
  if [ "$actual" -ge "$expected" ] 2>/dev/null; then
    green "$label"
    PASS=$((PASS + 1))
  else
    red "$label (expected >= $expected, got '$actual')"
    FAIL=$((FAIL + 1))
  fi
}

assert_stale_eq() {
  STALE_WRITE_ASSERTIONS=$((STALE_WRITE_ASSERTIONS + 1))
  assert_eq "$@"
}

assert_stale_ge() {
  STALE_WRITE_ASSERTIONS=$((STALE_WRITE_ASSERTIONS + 1))
  assert_ge "$@"
}

query_payload() {
  python3 - "$1" <<'PY'
import json
import sys

print(json.dumps({"query": sys.argv[1], "hitsPerPage": 10}))
PY
}

parse_nb_hits() {
  py "d=json.load(sys.stdin); print(d.get('nbHits',0))"
}

parse_count() {
  py "d=json.load(sys.stdin); print(d.get('count',0))"
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

parse_op_field_for_object() {
  local object_id="$1" field="$2"
  python3 -c '
import json
import sys

object_id, field = sys.argv[1], sys.argv[2]
data = json.load(sys.stdin)
for op in data.get("ops") or []:
    payload = op.get("payload") or {}
    body = payload.get("body") or {}
    candidate = payload.get("objectID") or body.get("_id")
    if candidate == object_id:
        value = op
        for part in field.split("."):
            value = value.get(part) if isinstance(value, dict) else None
        print("" if value is None else value)
        break
' "$object_id" "$field" 2>/dev/null || echo ""
}

parse_oldest_retained_seq() {
  py "d=json.load(sys.stdin); print(d.get('oldest_retained_seq',''))"
}

wait_healthy() {
  local url="$1/health" name="$2"
  printf "  Waiting for %s..." "$name"
  for i in $(seq 1 45); do
    if curl -sf "$url" >/dev/null 2>&1; then
      echo " ready"
      return 0
    fi
    sleep 2
  done
  echo " TIMEOUT"
  return 1
}

# Local JSON POST routed through the compose network for node-only endpoints.
post_json_compose() {
  local service="$1" path="$2" payload="${3:-}"
  if [ -n "$payload" ]; then
    compose exec -T "$service" curl -sf -X POST "http://localhost:7700$path" \
      -H 'Content-Type: application/json' \
      -d "$payload"
  else
    compose exec -T "$service" curl -sf -X POST "http://localhost:7700$path"
  fi
}

post_json_compose_stream() {
  local service="$1" path="$2"
  compose exec -T "$service" curl -sf -X POST "http://localhost:7700$path" \
    -H 'Content-Type: application/json' \
    --data-binary @-
}

get_json_compose() {
  local service="$1" path="$2"
  compose exec -T "$service" curl -sf "http://localhost:7700$path"
}

# In-network search query via docker compose exec.
search_hits_compose() {
  local service="$1" index="$2" query="$3" payload
  payload=$(query_payload "$query")
  post_json_compose "$service" "/1/indexes/$index/query" "$payload" 2>/dev/null | parse_nb_hits || echo "0"
}

search_hits() {
  local node="$1" index="$2" query="$3" payload
  payload=$(query_payload "$query")
  curl -sf -X POST "$node/1/indexes/$index/query" \
    -H 'Content-Type: application/json' \
    -d "$payload" 2>/dev/null | parse_nb_hits || echo "0"
}

wait_for_hits_with() {
  local target="$1" index="$2" query="$3" min_hits="$4" max_wait="${5:-10}"
  local elapsed=0
  while [ "$elapsed" -lt "$max_wait" ]; do
    local h
    h=$("$WAIT_FOR_HITS_BACKEND" "$target" "$index" "$query")
    if [ "$h" -ge "$min_hits" ] 2>/dev/null; then
      echo "$h"; return 0
    fi
    sleep 1; elapsed=$((elapsed + 1))
  done
  echo "0"
}

wait_for_hits() {
  WAIT_FOR_HITS_BACKEND=search_hits wait_for_hits_with "$@"
}

wait_for_hits_compose() {
  WAIT_FOR_HITS_BACKEND=search_hits_compose wait_for_hits_with "$@"
}

wait_for_exact_hits_compose() {
  local service="$1" index="$2" query="$3" expected_hits="$4" max_wait="${5:-20}"
  local elapsed=0
  while [ "$elapsed" -lt "$max_wait" ]; do
    local h
    h=$(search_hits_compose "$service" "$index" "$query")
    if [ "$h" = "$expected_hits" ]; then
      echo "$h"
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  echo "$(search_hits_compose "$service" "$index" "$query")"
}

seed_analytics_compose() {
  local service="$1" index="$2" days="${3:-7}"
  local payload
  payload=$(python3 - "$index" "$days" <<'PY'
import json
import sys

print(json.dumps({"index": sys.argv[1], "days": int(sys.argv[2])}))
PY
)
  post_json_compose "$service" "/2/analytics/seed" "$payload" 2>/dev/null | py "d=json.load(sys.stdin); print(d.get('totalSearches',0))" || echo "0"
}

flush_analytics_compose() {
  post_json_compose "$1" "/2/analytics/flush" >/dev/null 2>&1
}

search_json_compose() {
  local service="$1" index="$2" query="$3" payload
  payload=$(query_payload "$query")
  post_json_compose "$service" "/1/indexes/$index/query" "$payload" 2>/dev/null || echo "{}"
}

wait_for_exact_body_compose() {
  local service="$1" index="$2" query="$3" expected_body="$4" max_wait="${5:-20}"
  local elapsed=0
  while [ "$elapsed" -lt "$max_wait" ]; do
    local response nb_hits hit_id body
    response=$(search_json_compose "$service" "$index" "$query")
    nb_hits=$(echo "$response" | parse_nb_hits)
    hit_id=$(echo "$response" | parse_first_hit_field "_id")
    body=$(echo "$response" | parse_first_hit_field "body")
    if [ "$nb_hits" = "1" ] && [ "$hit_id" = "doc-stale" ] && [ "$body" = "$expected_body" ]; then
      echo "1"
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  echo "0"
}

batch_payload_for_doc() {
  local object_id="$1" title="$2" body="$3"
  python3 - "$object_id" "$title" "$body" <<'PY'
import json
import sys

object_id, title, body = sys.argv[1], sys.argv[2], sys.argv[3]
print(json.dumps({
    "requests": [{
        "action": "addObject",
        "body": {"_id": object_id, "title": title, "body": body},
    }]
}))
PY
}

large_batch_payload_for_doc() {
  local object_id="$1" title="$2"
  python3 - "$object_id" "$title" <<'PY'
import json
import sys

object_id, title = sys.argv[1], sys.argv[2]
body = "x" * (65 * 1024)
print(json.dumps({
    "requests": [
        {
            "action": "addObject",
            "body": {
                "_id": f"{object_id}-{i}",
                "title": f"{title} {i}",
                "body": body,
            },
        }
        for i in range(170)
    ]
}))
PY
}

replicated_upsert_payload() {
  local index="$1" seq="$2" timestamp_ms="$3" node_id="$4" object_id="$5" title="$6" body="$7"
  python3 - "$index" "$seq" "$timestamp_ms" "$node_id" "$object_id" "$title" "$body" <<'PY'
import json
import sys

index, seq, timestamp_ms, node_id, object_id, title, body = sys.argv[1:]
print(json.dumps({
    "tenant_id": index,
    "ops": [{
        "seq": int(seq),
        "timestamp_ms": int(timestamp_ms),
        "node_id": node_id,
        "tenant_id": index,
        "op_type": "upsert",
        "payload": {
            "objectID": object_id,
            "body": {"_id": object_id, "title": title, "body": body},
        },
    }],
}))
PY
}

preserve_node_data_on_failure() {
  local script_exit_code="$1"
  if ! $WITH_DOCKER; then
    return
  fi
  if [ "$FAIL" -eq 0 ] && [ "$script_exit_code" -eq 0 ] && [ "$INTERRUPTED_EXIT_CODE" -eq 0 ]; then
    return
  fi

  local results_dir node_data_dir
  results_dir="$SCRIPT_DIR/results/$(date -u +%Y%m%dT%H%M%SZ)"
  node_data_dir="$results_dir/node_data"
  mkdir -p "$node_data_dir"

  # Durable evidence path: docker-compose.yml gives node-a/b/c no host
  # volumes and FLAPJACK_DATA_DIR=/data is container-local, so failed
  # runs must snapshot /data before destructive `compose down -v`.
  local node container
  local copy_failed=false
  for node in node-a node-b node-c; do
    container="$(compose ps -q "$node" 2>/dev/null || true)"
    if [ -z "$container" ]; then
      red "cannot preserve $node /data: container not found"
      copy_failed=true
    elif ! docker cp "$container:/data" "$node_data_dir/$node" >/dev/null 2>&1; then
      red "cannot preserve $node /data: docker cp failed"
      copy_failed=true
    fi
  done
  if $copy_failed; then
    echo "ERROR: node-data evidence is incomplete at $node_data_dir"
    return 1
  fi
  echo "INFO: node /data snapshots copied to $node_data_dir"
}

cleanup() {
  local script_exit_code=$?
  if [[ "$CLEANUP_COMPLETE" == "true" ]]; then
    return
  fi
  CLEANUP_COMPLETE=true

  if $WITH_DOCKER; then
    echo ""
    echo "=== Compose topology before teardown ==="
    docker compose -f "$COMPOSE_FILE" ps || true
    if ! preserve_node_data_on_failure "$script_exit_code"; then
      echo "ERROR: skipping destructive teardown because failure evidence was not preserved"
      return
    fi
    echo ""
    echo "=== Tearing down ==="
    compose down -v
  fi
}

# ── optional: docker lifecycle ────────────────────────────────────────────────

WITH_DOCKER=false
if [ "${1:-}" = "--with-docker" ]; then WITH_DOCKER=true; fi
trap cleanup EXIT
trap 'INTERRUPTED_EXIT_CODE=130; cleanup; exit 130' INT
trap 'INTERRUPTED_EXIT_CODE=143; cleanup; exit 143' TERM
if $WITH_DOCKER; then
  echo "=== Building and starting 3-node HA cluster ==="
  export FLAPJACK_HA_OPLOG_RETENTION=2
  export FLAPJACK_HA_MAX_RECORD_BYTES=13000000
  export FLAPJACK_HA_MAX_BODY_MB=16
  compose up -d --build
fi

# ── 1. Health checks ──────────────────────────────────────────────────────────

echo ""
echo "=== 1. Health checks ==="
wait_healthy "$LB" "load-balancer"
wait_healthy_compose "node-a" "node-a"
wait_healthy_compose "node-b" "node-b"
wait_healthy_compose "node-c" "node-c"

if ! wait_for_peer_mesh_ready 60; then
  red "peer mesh did not converge in 60s (node-a=$PEERS_NODE_A node-b=$PEERS_NODE_B node-c=$PEERS_NODE_C)"
  FAIL=$((FAIL + 1))
fi

LB_HEALTH=$(curl -sf "$LB/health" | py "print(json.load(sys.stdin).get('status',''))")
assert_eq "LB proxies health to a live node" "ok" "$LB_HEALTH"

assert_eq "node-a sees 2 peers" "2" "$PEERS_NODE_A"
assert_eq "node-b sees 2 peers" "2" "$PEERS_NODE_B"
assert_eq "node-c sees 2 peers" "2" "$PEERS_NODE_C"

# Cluster status on any node (via LB)
STATUS=$(curl -sf "$LB/internal/cluster/status" 2>/dev/null || echo "{}")
PEERS_TOTAL=$(echo "$STATUS" | py "print(json.load(sys.stdin).get('peers_total',0))")
assert_ge "cluster has >= 2 peers configured" 2 "$PEERS_TOTAL"

# ── 2. Search replication across all 3 nodes ──────────────────────────────────

echo ""
echo "=== 2. Search replication (write via LB, read from all nodes) ==="

INDEX="ha-test-$(date +%s)"
curl -sf -X POST "$LB/1/indexes/$INDEX/batch" \
  -H 'Content-Type: application/json' \
  -d '{
    "requests": [
      {"action":"addObject","body":{"_id":"1","title":"Cinnamon Rolls","flavor":"sweet"}},
      {"action":"addObject","body":{"_id":"2","title":"Blueberry Scones","flavor":"sweet"}},
      {"action":"addObject","body":{"_id":"3","title":"Earl Grey Tea","flavor":"bitter"}},
      {"action":"addObject","body":{"_id":"4","title":"Espresso Shots","flavor":"bitter"}},
      {"action":"addObject","body":{"_id":"5","title":"Vanilla Latte","flavor":"sweet"}}
    ]
  }' >/dev/null
echo "  Indexed 5 docs via LB"

# Wait for replication to all nodes (up to 10s)
sleep 2
echo "  Waiting for full replication (up to 10s)..."

HITS_A=$(wait_for_hits "$LB" "$INDEX" "cinnamon" 1 10)
assert_ge "LB: cinnamon reachable within 10s" 1 "$HITS_A"

LB_ALL=$(wait_for_hits "$LB" "$INDEX" "" 5 10)
assert_ge "LB: all 5 docs reachable" 5 "$LB_ALL"

NODE_A_ALL=$(wait_for_hits_compose "node-a" "$INDEX" "" 5 10)
assert_ge "node-a: all 5 docs replicated" 5 "$NODE_A_ALL"

NODE_B_ALL=$(wait_for_hits_compose "node-b" "$INDEX" "" 5 10)
assert_ge "node-b: all 5 docs replicated" 5 "$NODE_B_ALL"

NODE_C_ALL=$(wait_for_hits_compose "node-c" "$INDEX" "" 5 10)
assert_ge "node-c: all 5 docs replicated" 5 "$NODE_C_ALL"

# ── 3. Node failover: LB routes around downed node ────────────────────────────

echo ""
echo "=== 3. Node failover (stop node-c, LB continues serving) ==="

compose stop node-c 2>/dev/null || { echo "  (direct docker access not available, skipping failover test)"; }

sleep 3  # nginx marks node-c as down

# Writes and reads should continue through node-a and node-b
curl -sf -X POST "$LB/1/indexes/$INDEX/batch" \
  -H 'Content-Type: application/json' \
  -d '{
    "requests": [
      {"action":"addObject","body":{"_id":"6","title":"Hazelnut Biscotti","flavor":"nutty"}}
    ]
  }' >/dev/null 2>&1 || true

FAILOVER_HIT=$(wait_for_hits "$LB" "$INDEX" "hazelnut" 1 8)
assert_ge "LB serves writes/reads with node-c down" 1 "$FAILOVER_HIT"

# Restart node-c and wait for it to become healthy inside the compose network.
# Node ports are not host-exposed; use docker compose exec for in-network probes.
compose start node-c 2>/dev/null || true
wait_healthy_compose "node-c" "node-c (restarted)"

# ── 3b. Startup catch-up: restarted node serves the missed document ────────

echo ""
echo "=== 3b. Startup catch-up (node-c returns doc written while it was down) ==="

# Startup catch-up and replication are asynchronous; allow extra time for replay.
CATCHUP_HIT=$(wait_for_hits_compose "node-c" "$INDEX" "hazelnut" 1 30)
assert_ge "node-c serves hazelnut doc after catch-up" 1 "$CATCHUP_HIT"

if ! wait_for_peer_mesh_ready 60; then
  red "peer mesh did not reconverge before analytics fan-out (node-a=$PEERS_NODE_A node-b=$PEERS_NODE_B node-c=$PEERS_NODE_C)"
  FAIL=$((FAIL + 1))
fi

# ── 3c. Stale replicated write after retained-oplog gap ─────────────────────

echo ""
echo "=== 3c. Stale replicated write rejected after node-a retained-oplog gap ==="

NEWER_BODY="newer-body-$(date +%s)"
OLDER_BODY="older-body-$(date +%s)"
NEWER_TITLE="Durable Newer $NEWER_BODY"
OLDER_TITLE="Durable Older $OLDER_BODY"
STALE_QUERY="$NEWER_TITLE"
post_json_compose "node-a" "/1/indexes/$INDEX/batch" "$(batch_payload_for_doc "doc-stale" "$NEWER_TITLE" "$NEWER_BODY")" >/dev/null

for service in node-a node-b node-c; do
  READY=$(wait_for_exact_body_compose "$service" "$INDEX" "$STALE_QUERY" "$NEWER_BODY" 30)
  assert_stale_eq "$service: doc-stale newer body replicated before stale injection" "1" "$READY"
done

OPS_BEFORE=$(get_json_compose "node-a" "/internal/ops?tenant_id=$INDEX&since_seq=0")
DOC_STALE_SEQ=$(echo "$OPS_BEFORE" | parse_op_field_for_object "doc-stale" "seq")
DOC_STALE_TS=$(echo "$OPS_BEFORE" | parse_op_field_for_object "doc-stale" "timestamp_ms")
assert_stale_ge "node-a: doc-stale originating seq recorded" 1 "${DOC_STALE_SEQ:-0}"
assert_stale_ge "node-a: doc-stale originating timestamp recorded" 1 "${DOC_STALE_TS:-0}"

for filler in 1 2 3; do
  large_batch_payload_for_doc "doc-stale-filler-$filler" "Stale Filler $filler" \
    | post_json_compose_stream "node-a" "/1/indexes/$INDEX/batch" >/dev/null
done

OPS_AFTER_TRUNCATE=$(get_json_compose "node-a" "/internal/ops?tenant_id=$INDEX&since_seq=0")
OLDEST_RETAINED_SEQ=$(echo "$OPS_AFTER_TRUNCATE" | parse_oldest_retained_seq)
assert_stale_ge "node-a: oldest_retained_seq proves original doc-stale gap" "$((DOC_STALE_SEQ + 1))" "${OLDEST_RETAINED_SEQ:-0}"
echo "  Stale-write gap proof: doc-stale seq=$DOC_STALE_SEQ oldest_retained_seq=${OLDEST_RETAINED_SEQ:-missing}"

EXPECTED_DOC_COUNT=$(search_hits_compose "node-a" "$INDEX" "")
EXPECTED_DOC_COUNT_NODE_A=$(wait_for_exact_hits_compose "node-a" "$INDEX" "" "$EXPECTED_DOC_COUNT" 10)
EXPECTED_DOC_COUNT_NODE_B=$(wait_for_exact_hits_compose "node-b" "$INDEX" "" "$EXPECTED_DOC_COUNT" 120)
EXPECTED_DOC_COUNT_NODE_C=$(wait_for_exact_hits_compose "node-c" "$INDEX" "" "$EXPECTED_DOC_COUNT" 120)
assert_stale_eq "node-a: document count converged before stale injection" "$EXPECTED_DOC_COUNT" "$EXPECTED_DOC_COUNT_NODE_A"
assert_stale_eq "node-b: document count converged before stale injection" "$EXPECTED_DOC_COUNT" "$EXPECTED_DOC_COUNT_NODE_B"
assert_stale_eq "node-c: document count converged before stale injection" "$EXPECTED_DOC_COUNT" "$EXPECTED_DOC_COUNT_NODE_C"

compose restart node-a >/dev/null
wait_healthy_compose "node-a" "node-a (restarted before stale injection)"
if ! wait_for_peer_mesh_ready 60; then
  red "peer mesh did not reconverge before stale injection (node-a=$PEERS_NODE_A node-b=$PEERS_NODE_B node-c=$PEERS_NODE_C)"
  FAIL=$((FAIL + 1))
fi

OLDER_TS=$((DOC_STALE_TS > 0 ? DOC_STALE_TS - 1 : 0))
replicated_upsert_payload "$INDEX" "$((DOC_STALE_SEQ + 1000))" "$OLDER_TS" "node-stale" "doc-stale" "$OLDER_TITLE" "$OLDER_BODY" \
  | post_json_compose_stream "node-a" "/internal/replicate" >/dev/null

for service in node-a node-b node-c; do
  case "$service" in
    node-a) EXPECTED_DOC_COUNT="$EXPECTED_DOC_COUNT_NODE_A" ;;
    node-b) EXPECTED_DOC_COUNT="$EXPECTED_DOC_COUNT_NODE_B" ;;
    node-c) EXPECTED_DOC_COUNT="$EXPECTED_DOC_COUNT_NODE_C" ;;
  esac
  RESPONSE=$(search_json_compose "$service" "$INDEX" "$STALE_QUERY")
  NB_HITS=$(echo "$RESPONSE" | parse_nb_hits)
  HIT_ID=$(echo "$RESPONSE" | parse_first_hit_field "_id")
  HIT_BODY=$(echo "$RESPONSE" | parse_first_hit_field "body")
  DOC_COUNT=$(search_hits_compose "$service" "$INDEX" "")
  assert_stale_eq "$service: stale-write nbHits remains exact" "1" "$NB_HITS"
  assert_stale_eq "$service: stale-write returned _id is doc-stale" "doc-stale" "$HIT_ID"
  assert_stale_eq "$service: stale-write retained newer body" "$NEWER_BODY" "$HIT_BODY"
  assert_stale_eq "$service: stale-write document count unchanged" "$EXPECTED_DOC_COUNT" "$DOC_COUNT"
done
echo "  stale-write assertions: $STALE_WRITE_ASSERTIONS"

# ── 4. Analytics fan-out across 3 nodes ────────────────────────────────────

echo ""
echo "=== 4. Analytics fan-out (3 nodes) ==="

# Seed and flush analytics on each node directly. These endpoints are local-only,
# so going through the load balancer would make node coverage nondeterministic.
TOTAL_SEEDED=0
for service in node-a node-b node-c; do
  SEEDED=$(seed_analytics_compose "$service" "$INDEX" 7)
  TOTAL_SEEDED=$((TOTAL_SEEDED + SEEDED))
  flush_analytics_compose "$service"
done
sleep 1

# Fan-out should merge all three node-local analytics stores.
FANOUT=$(curl -sf "$LB/2/searches/count?index=$INDEX" 2>/dev/null || echo "{}")
SEARCH_COUNT=$(echo "$FANOUT" | parse_count)
NODES_TOTAL=$(echo "$FANOUT" | py "print(json.load(sys.stdin).get('cluster',{}).get('nodes_total',0))")
NODES_RESP=$(echo "$FANOUT" | py "print(json.load(sys.stdin).get('cluster',{}).get('nodes_responding',0))")
assert_ge "fan-out: merged search count includes all seeded nodes" "$TOTAL_SEEDED" "$SEARCH_COUNT"
assert_eq "fan-out: nodes_total=3" "3" "$NODES_TOTAL"
assert_ge "fan-out: all nodes responding" 3 "$NODES_RESP"

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo "════════════════════════════════════════════"
TOTAL=$((PASS + FAIL))
if [ "$FAIL" -eq 0 ]; then
  printf "\033[32m✓ All %d assertions passed\033[0m\n" "$TOTAL"
else
  printf "\033[31m✗ %d/%d assertions failed\033[0m\n" "$FAIL" "$TOTAL"
fi
echo "stale-write assertions: $STALE_WRITE_ASSERTIONS"
echo "════════════════════════════════════════════"

[ "$FAIL" -eq 0 ]
