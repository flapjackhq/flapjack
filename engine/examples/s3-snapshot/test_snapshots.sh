#!/bin/bash
# S3 snapshot verification against MinIO.
#
# Prerequisites:
#   docker compose up -d --build   (from this directory)
#   Wait for flapjack health check to pass.
#
# Scope: single-node S3 snapshot round-trip, scheduled backups, auto-restore.
# NOT proven: failover, oplog replay, multi-node replication.
set -euo pipefail
cd "$(dirname "$0")"

API="${FLAPJACK_API:-http://localhost:7700}"
IDX="test_s3_snap"
# The compose example runs with FLAPJACK_NO_AUTH=1, so this harness should not
# send partial auth headers that can accidentally exercise auth parsing instead
# of the snapshot flow itself.
HDR=(-H "Content-Type: application/json")

pass() { echo "  PASS: $1"; }
fail() { echo "  FAIL: $1"; exit 1; }

wait_healthy() {
    local max_wait=${1:-60}
    local elapsed=0
    while [ $elapsed -lt $max_wait ]; do
        if curl -sf "$API/health" > /dev/null 2>&1; then
            return 0
        fi
        sleep 2
        elapsed=$((elapsed + 2))
    done
    fail "Flapjack not healthy after ${max_wait}s"
}

ensure_no_local_flapjack_conflict() {
    if ! command -v lsof >/dev/null 2>&1; then
        return 0
    fi

    if lsof -nP -iTCP:7700 -sTCP:LISTEN 2>/dev/null | grep -Fq '/engine/target/debug/flapjack'; then
        fail "localhost:7700 is owned by a local flapjack process; stop it before running the compose snapshot proof"
    fi
}

ensure_localhost_only_ports() {
    if ! docker compose config --format json | python3 -c '
import json, sys
config = json.load(sys.stdin)
ports = config["services"]["minio"]["ports"] + config["services"]["flapjack"]["ports"]
if len(ports) != 3 or any(port.get("host_ip") != "127.0.0.1" for port in ports):
    raise SystemExit("the unauthenticated example must publish exactly three localhost-only ports")
'; then
        fail "Compose example ports must bind only to 127.0.0.1"
    fi
}

wait_for_hits() {
    local query="$1"
    local expected_hits="$2"
    local max_wait="${3:-60}"
    local elapsed=0
    while [ "$elapsed" -lt "$max_wait" ]; do
        local hits
        hits=$(curl -s -X POST "$API/1/indexes/$IDX/query" "${HDR[@]}" \
          -d "{\"query\":\"$query\"}" \
          | python3 -c "import sys,json
try:
    print(json.load(sys.stdin).get('nbHits', -1))
except Exception:
    print(-1)")
        if [ "$hits" = "$expected_hits" ]; then
            echo "$hits"
            return 0
        fi
        sleep 2
        elapsed=$((elapsed + 2))
    done
    echo "$hits"
    return 1
}

echo "=== S3 Snapshot Verification ==="
echo "API: $API"
echo ""

ensure_no_local_flapjack_conflict
ensure_localhost_only_ports

# ── Wait for health ──────────────────────────────────────────────────────────
echo "--- Waiting for Flapjack health ---"
wait_healthy 60
pass "Flapjack is healthy"

# ── Setup: add documents ─────────────────────────────────────────────────────
echo "--- Setup: add documents ---"
curl -sf -X DELETE "$API/1/indexes/$IDX" "${HDR[@]}" > /dev/null 2>&1 || true
sleep 0.3

curl -sf -X POST "$API/1/indexes/$IDX/batch" "${HDR[@]}" \
  -d '{"requests":[
    {"action":"addObject","body":{"objectID":"1","name":"Gaming Laptop","price":1299}},
    {"action":"addObject","body":{"objectID":"2","name":"Wireless Mouse","price":49}},
    {"action":"addObject","body":{"objectID":"3","name":"Mechanical Keyboard","price":159}}
  ]}' > /dev/null
sleep 2

HITS=$(curl -sf -X POST "$API/1/indexes/$IDX/query" "${HDR[@]}" \
  -d '{"query":"laptop"}' | python3 -c "import sys,json; print(json.load(sys.stdin).get('nbHits',0))")
[ "$HITS" = "1" ] && pass "Search before snapshot: $HITS hit" || fail "Expected 1 hit, got $HITS"

# ── Test 1: Snapshot to S3 ───────────────────────────────────────────────────
echo "--- Test 1: Snapshot to S3 ---"
SNAP=$(curl -sf -X POST "$API/1/indexes/$IDX/snapshot" "${HDR[@]}")
STATUS=$(echo "$SNAP" | python3 -c "import sys,json; print(json.load(sys.stdin)['status'])")
KEY=$(echo "$SNAP" | python3 -c "import sys,json; print(json.load(sys.stdin)['key'])")
echo "  Key: $KEY"
[ "$STATUS" = "uploaded" ] && pass "Snapshot status: uploaded" || fail "Expected 'uploaded', got '$STATUS'"
[ -n "$KEY" ] && pass "Key present" || fail "No key returned"

# ── Test 2: List snapshots ───────────────────────────────────────────────────
echo "--- Test 2: List snapshots ---"
COUNT=$(curl -sf "$API/1/indexes/$IDX/snapshots" "${HDR[@]}" \
  | python3 -c "import sys,json; print(len(json.load(sys.stdin)['snapshots']))")
[ "$COUNT" -ge 1 ] && pass "Listed $COUNT snapshot(s)" || fail "Expected >=1, got $COUNT"

# ── Test 3: Delete index + restore latest ────────────────────────────────────
echo "--- Test 3: Restore from latest S3 snapshot ---"
curl -sf -X DELETE "$API/1/indexes/$IDX" "${HDR[@]}" > /dev/null
sleep 0.5

# Verify index is gone
DEL_STATUS=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$API/1/indexes/$IDX/query" \
  "${HDR[@]}" -d '{"query":"laptop"}')
[ "$DEL_STATUS" = "404" ] && pass "Index deleted (404)" || fail "Expected 404, got $DEL_STATUS"

RESTORE=$(curl -sf -X POST "$API/1/indexes/$IDX/restore" "${HDR[@]}")
RSTATUS=$(echo "$RESTORE" | python3 -c "import sys,json; print(json.load(sys.stdin)['status'])")
[ "$RSTATUS" = "restored" ] && pass "Restore status: restored" || fail "Expected 'restored', got '$RSTATUS'"
sleep 1

HITS=$(curl -sf -X POST "$API/1/indexes/$IDX/query" "${HDR[@]}" \
  -d '{"query":"laptop"}' | python3 -c "import sys,json; print(json.load(sys.stdin).get('nbHits',0))")
[ "$HITS" = "1" ] && pass "Search after restore: $HITS hit" || fail "Expected 1 hit after restore, got $HITS"

ALL=$(curl -sf -X POST "$API/1/indexes/$IDX/query" "${HDR[@]}" \
  -d '{"query":""}' | python3 -c "import sys,json; print(json.load(sys.stdin).get('nbHits',0))")
[ "$ALL" = "3" ] && pass "All docs restored: $ALL" || fail "Expected 3 docs, got $ALL"

# ── Test 4: Restore by explicit key ──────────────────────────────────────────
echo "--- Test 4: Restore by explicit key ---"
curl -sf -X DELETE "$API/1/indexes/$IDX" "${HDR[@]}" > /dev/null
sleep 0.5

RESTORE2=$(curl -sf -X POST "$API/1/indexes/$IDX/restore" "${HDR[@]}" \
  -d "{\"key\":\"$KEY\"}")
RSTATUS2=$(echo "$RESTORE2" | python3 -c "import sys,json; print(json.load(sys.stdin)['status'])")
[ "$RSTATUS2" = "restored" ] && pass "Restore by key: $RSTATUS2" || fail "Restore by key failed"
sleep 1

HITS=$(curl -sf -X POST "$API/1/indexes/$IDX/query" "${HDR[@]}" \
  -d '{"query":"laptop"}' | python3 -c "import sys,json; print(json.load(sys.stdin).get('nbHits',0))")
[ "$HITS" = "1" ] && pass "Search after key restore: $HITS hit" || fail "Expected 1 hit, got $HITS"

# ── Test 5: Scheduled backup (wait for FLAPJACK_SNAPSHOT_INTERVAL) ───────────
echo "--- Test 5: Scheduled backup ---"
echo "  Waiting ~45s for scheduled backup (interval=30s + buffer)..."
BEFORE=$(curl -sf "$API/1/indexes/$IDX/snapshots" "${HDR[@]}" \
  | python3 -c "import sys,json; print(len(json.load(sys.stdin)['snapshots']))")
sleep 45
AFTER=$(curl -sf "$API/1/indexes/$IDX/snapshots" "${HDR[@]}" \
  | python3 -c "import sys,json; print(len(json.load(sys.stdin)['snapshots']))")
[ "$AFTER" -gt "$BEFORE" ] && pass "Scheduled backup ran: $BEFORE -> $AFTER snapshots" \
  || fail "Snapshot count did not increase: $BEFORE -> $AFTER"

# ── Test 6: Auto-restore on empty data dir ───────────────────────────────────
echo "--- Test 6: Auto-restore on empty data dir ---"
# Wipe visible and hidden data while the container is running, then force-stop
# it so shutdown hooks cannot repopulate /data before the next startup check.
docker compose exec flapjack sh -c "rm -rf /data/* /data/.[!.]* /data/..?*"
docker compose kill -s KILL flapjack > /dev/null
RESTART_LOG_SINCE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
docker compose up -d flapjack > /dev/null
echo "  Waiting for Flapjack to restart and auto-restore..."
wait_healthy 60
sleep 2

RESTORE_LOGS=$(docker compose logs --since="$RESTART_LOG_SINCE" flapjack)
echo "$RESTORE_LOGS" | grep -q "Empty data dir detected, attempting S3 auto-restore" \
  && pass "Auto-restore startup path triggered" \
  || fail "Auto-restore startup path did not trigger"

HITS=$(wait_for_hits "laptop" "1" 60) \
  || fail "Auto-restore failed: expected 1 hit, got $HITS"
[ "$HITS" = "1" ] && pass "Auto-restore: search returns $HITS hit" || fail "Auto-restore failed: expected 1 hit, got $HITS"

# ── Cleanup ──────────────────────────────────────────────────────────────────
echo "--- Cleanup ---"
curl -sf -X DELETE "$API/1/indexes/$IDX" "${HDR[@]}" > /dev/null 2>&1 || true

echo ""
echo "=== ALL TESTS PASSED ==="
